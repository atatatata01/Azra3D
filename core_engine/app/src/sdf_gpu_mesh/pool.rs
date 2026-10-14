//! Tiled-DC stage B-1 / Phase 4 — the shared slotted mesh pool for all SPARSE (brick-resident)
//! SDF bodies, now a SET of buffer GROUPS (Phase-4 multi-buffer).
//!
//! Mirrors the proven voxel-sculpt precedent (`VoxelMeshGpu` pool in `voxel_gpu_mesh.rs`): big
//! vertex/index buffers + an indirect-args buffer + ONE per-slot instance buffer, with each body's
//! mesh living in a SUB-REGION. The DC mesher writes a body's slot region by binding it as a
//! STORAGE SUB-RANGE (`BufferBinding { offset, size }`, 256B-aligned), so the shader's slot-local
//! writes and `arrayLength` overflow guards work UNCHANGED. The rasteriser draws every sparse
//! body with ONE `multi_draw_indexed_indirect` per body over its args block, rebinding the
//! vertex/index buffers only when the GROUP changes (bodies are sorted by group in the draw —
//! typically 1-3 rebinds per pass).
//!
//! PHASE 4 (multi-buffer, 2026-07-17): capacity was ONE 1 GiB-self-capped buffer pair — ONE
//! realistic 2048 body (or two 1024s) exceeded it and fell out of the pooled tiled path exactly
//! where it matters most (owner logs: "tiled packing exceeds the dense ceiling — monolithic
//! fallback" / "request exceeds the pool cap — per-body slot fallback"). The pool is now a
//! `Vec<SdfPoolGroup>`: each group owns its OWN verts/idx/indirect/tile_info/tile_counters
//! quintet (≤ `SDF_POOL_MAX_BUFFER_BYTES` per buffer — the reliable-allocation lesson stands),
//! its own bump/args/free-list state, and its OWN generation counter. A body's slot lives
//! ENTIRELY in one group (a tiled body's tiles never span groups — keeps the C2 entry/index
//! space self-contained); assignment is first-group-with-room, then per-GROUP rebuild (repacking
//! only that group — never multiplying the ~50ms rebuild across groups), then a NEW group up to
//! `SDF_POOL_TOTAL_BUDGET_BYTES`, then the existing per-body fallback. The voxel pool deferred
//! exactly this follow-up (voxel_gpu_mesh.rs:1201) — this is the reference implementation.
//!
//! DELTA vs the voxel pool (deliberate): voxel chunks are uniform 16³ → FIXED slot strides; SDF
//! bodies are wildly variable and GROW (R3b overflow-grow), so slots here are VARIABLE-SIZED
//! regions with a per-group bump allocator. A slot grow that no longer fits appends at its
//! group's bump tail (the old region becomes a hole); when the tail is exhausted the GROUP
//! rebuilds — new buffers sized ×1.5 of the repacked DEMAND (never of the group's own previous
//! size: that self-reference was the S0.3 ratchet, see `rebuild_group`), capped at
//! min(max_buffer_size, `max_group_bytes`), every surviving slot's region GPU-copied old→new,
//! indirect args reseeded (index_count preserved via GPU copy), that group's `generation` bumped
//! (generation counters over timing tricks). REBUILD/GROW ONLY IN POLL (Frame Ordering
//! Contract). Searched for existing implementations first (REUSE rule): the only in-repo pools
//! are `VoxelMeshGpu` (fixed-stride — cannot express variable per-body regions) and `VolumePool`
//! (3D-texture volumes) — neither fits, hence this module.

use eframe::wgpu;
use std::collections::{HashMap, VecDeque};

use core_math::scene_graph::NodeId;

/// Phase 4b (per-body multi-group tiling) — slots are keyed by (body, SEGMENT). A body whose
/// tiled packing fits one group has exactly segment 0 (the pre-4b state, byte-identical
/// behavior). A body whose packing EXCEEDS one group buffer is partitioned into contiguous
/// SEGMENTS of its tile table — each segment is an ordinary slot (region + args block + tile
/// table) living wholly in ONE group, and different segments may live in DIFFERENT groups.
/// Each tile still lives wholly in one group (its segment's), so the C2 entry/index space stays
/// self-contained per segment. Segment indices are DENSE from 0 (no gaps); segment 0 owns the
/// body's shared instance index.
pub(crate) type SlotKey = (NodeId, u32);

/// Bytes per `DrawIndexedIndirect` entry (5×u32) — same value as the voxel pool's stride.
pub(crate) const SDF_POOL_INDIRECT_STRIDE: u64 = 20;
/// Hard per-buffer allocation cap — mirror of `voxel_gpu_mesh::POOL_MAX_BUFFER_BYTES` (a device may
/// report a huge `max_buffer_size` yet fail a single multi-GB allocation; 1 GiB keeps it reliable).
/// Phase 4: this is now PER GROUP BUFFER — total pool capacity is bounded by
/// `SDF_POOL_TOTAL_BUDGET_BYTES` across groups instead.
pub(crate) const SDF_POOL_MAX_BUFFER_BYTES: u64 = 1 << 30;
/// Phase 4 — total region-buffer VRAM budget across ALL groups (Σ verts_bytes + idx_bytes). A
/// request that cannot fit any existing group NOR a new group under this budget falls back to
/// the per-body slot buffers exactly as the single-buffer pool did. Phase 4b raised this from
/// 4 GiB to 6 GiB: the owner-log realistic 2048 body packs 77.3M per-tile vert CAPS = 2.48 GB
/// verts + 1.86 GB idx = 4.33 GB of regions — just over the old 4 GiB, which would have refused
/// the very case per-body multi-group tiling exists for. 6 GiB hosts that body plus a settled
/// 1024 neighbor (~0.5 GB); 2048 is opt-in on 16-24 GB GPUs (the picker hover), where a 6 GiB
/// mesh-pool share is proportionate. Exhaustion still degrades exactly as before (loud fallback).
pub(crate) const SDF_POOL_TOTAL_BUDGET_BYTES: u64 = 6 << 30;
/// Phase 4c — VRAM cap on RETIRED region buffers held for recycling. A full rebuild/new-group
/// replaces (verts, idx) buffers; instead of dropping the old ones, the pool keeps them so the
/// NEXT rebuild reuses them, skipping both `create_buffer` (the DX12/Vulkan committed-resource
/// stall that balloons under production VRAM pressure — the owner-log 330ms `pool=` phase) AND
/// wgpu's lazy zero-init (measured ~0.5 s/GiB of async GPU fill that stalls rendering). 2 GiB ≈
/// one dense-2048 verts (~1 GiB) + idx (~0.8 GiB) pair, enough for a gesture's just-retired pair
/// to feed its next grow; larger and the free-list would itself become the VRAM pressure it
/// avoids. Fully drained at settle-trim (quiet = release, `trim_slot`).
pub(crate) const SDF_POOL_RECYCLE_MAX_BYTES: u64 = 2 << 30;
/// Bytes per instance entry in the shared instance buffer (one `render::mesh_gpu::InstanceData`).
pub(crate) const SDF_POOL_INSTANCE_STRIDE: u64 = std::mem::size_of::<render::mesh_gpu::InstanceData>() as u64;

/// Bytes per `TileInfo` entry in the shared tile-info buffer (4 × vec4<u32>, C1 tiled DC).
pub(crate) const SDF_POOL_TILE_INFO_STRIDE: u64 = 64;
/// Bytes per per-tile atomic counter pair ([vert_count, index_count]).
pub(crate) const SDF_POOL_TILE_COUNTER_STRIDE: u64 = 8;

/// 256B-align (storage-buffer sub-range offsets require %256==0; vertex stride 32 and index stride
/// 4 both divide 256, so `base_vertex = off/32` and `first_index = off/4` stay exact integers).
fn align256(bytes: u64) -> u64 { ((bytes.max(4) + 255) / 256) * 256 }

/// Phase 4c demand-size: cap the IDX region's ×1.5 headroom at 0.75× the sized VERTS region.
/// Every pool slot has `idx_cap == 6 × vert_cap` (DC quads = 6 idx/vert — every caller: tile protos
/// `vcap*6`, `grow_slot` `want_v*6`, the sparse monolithic `sv*6`), so idx BYTES = 6×4 / 32 = 0.75×
/// vert bytes ALWAYS. When a dense body's verts pin the per-buffer ceiling (`safe_max`), the raw
/// ×1.5 idx headroom lands on that same ceiling too, allocating ~1 GiB of idx where ~0.75 GiB is the
/// true bound — pure fresh-alloc + zero-init cost (measured ≈269 MB/segment waste at 2048³). Capping
/// at 0.75× the verts region grants idx the SAME proportional headroom verts got and never dips
/// below `need_i` (the align-rounded demand floor). No effect below the ceiling — small bodies keep
/// the full ×1.5 (there `nv*3/4 ≥ need_i*3/2`), so only the pinned dense case tightens.
fn clamp_idx_headroom(nv: u64, ni: u64, need_i: u64) -> u64 { ni.min(nv * 3 / 4).max(need_i) }

/// C1 tiled DC — one 32³-cell tile's sub-allocation inside its body's pool region. Offsets are
/// REGION-RELATIVE (in elements, no alignment needed — the tiled shader binds the WHOLE group
/// buffers and addresses absolutely via TileInfo), so a region relocation/rebuild keeps the table
/// valid: the pool just re-derives the absolute bases from the new `vert_off`/`idx_off`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SdfTileEntry {
    /// Tile coordinate in the tile grid (cells >> 5 == bricks >> 2). Stable across re-meshes
    /// while the grid bounds hold — the key for per-tile overflow floors and the C2 dirty map.
    pub(crate) tile: [u32; 3],
    /// Quad anchor window in CELLS (inclusive) — the tile's handle-brick bbox clamped to the grid.
    pub(crate) amin: [u32; 3],
    pub(crate) amax: [u32; 3],
    /// Region-relative vertex offset (VERTICES) and capacity.
    pub(crate) rel_vert: u64,
    pub(crate) vert_cap: u64,
    /// Region-relative index offset (u32s) and capacity.
    pub(crate) rel_idx: u64,
    pub(crate) idx_cap: u64,
    /// C2 — occupancy stamp copied from the tile's `SdfTileStat` at the dispatch that (re)meshed
    /// it: own handle-brick count + halo-augmented count. The incremental tile-set check compares
    /// these (plus the window) against the CURRENT `atlas.tile_stats` — any mismatch OUTSIDE the
    /// dirty window is a contract violation that forces a full re-mesh.
    pub(crate) bricks: u32,
    pub(crate) halo_bricks: u32,
    /// C2 — the tile DIED inside a dirty window (occupancy dropped to 0) during an incremental
    /// re-mesh: its args entry is CPU-zeroed (never drawn) and its region becomes a hole, but the
    /// entry keeps its position so every OTHER tile's entry index / counters / args stay put.
    /// Purged by the next FULL tiled re-mesh (which rebuilds the table from stats).
    pub(crate) dead: bool,
}

/// One body's region inside its buffer group.
pub(crate) struct SdfPoolSlot {
    /// Index of the owning `SdfPoolGroup` in `SdfMeshPool.groups`. A slot (and a tiled body's
    /// whole tile table / args block / counter block) lives ENTIRELY in one group — tiles never
    /// span groups (Phase 4 invariant; keeps the C2 entry space self-contained).
    pub(crate) group: usize,
    /// Byte offset of the vertex region in the group's `verts` (%256==0). `base_vertex = vert_off / 32`.
    pub(crate) vert_off: u64,
    /// Vertex capacity of the region in VERTICES (8 f32 each). The bound sub-range size is
    /// `align256(vert_cap*32)` — the shader's `arrayLength` guard clamps against it.
    pub(crate) vert_cap: u64,
    /// Byte offset of the index region in the group's `idx` (%256==0). `first_index = idx_off / 4`.
    pub(crate) idx_off: u64,
    /// Index capacity in u32 INDICES.
    pub(crate) idx_cap: u64,
    /// Stable INSTANCE index: this body's `InstanceData` lives at
    /// `index * SDF_POOL_INSTANCE_STRIDE` of the pool-shared `instances` (= the args'
    /// `first_instance` when the device supports `INDIRECT_FIRST_INSTANCE`). Instance indices are
    /// POOL-GLOBAL (one instance buffer for all groups) — allocated bump+free-list.
    pub(crate) index: u32,
    /// First `DrawIndexedIndirect` entry of this body's args BLOCK in ITS GROUP's entry space. A
    /// monolithic body owns a block of 1 (seeded by `seed_indirect`); a TILED body owns
    /// `args_cap ≥ tiles.len()` contiguous entries (index_count GPU-written by dc_tile_args).
    /// Entry `e` also owns the group's `tile_info[e]` and `tile_counters[e*2..e*2+2]` — the three
    /// buffers share the entry index space, so a block never moves piecemeal.
    pub(crate) args_base: u32,
    /// Block capacity in entries (grow-only per body; live tiles ≤ args_cap; the stale tail past
    /// `tiles.len()` is never drawn — the per-body multi-draw covers exactly the live count).
    pub(crate) args_cap: u32,
    /// C1 tile table: empty = monolithic (single args entry). Rebuilt by every tiled dispatch;
    /// preserved (offsets re-derived) across pool rebuilds/relocations.
    pub(crate) tiles: Vec<SdfTileEntry>,
}

impl SdfPoolSlot {
    pub(crate) fn indirect_offset(&self) -> u64 { self.args_base as u64 * SDF_POOL_INDIRECT_STRIDE }
    pub(crate) fn vert_region_bytes(&self) -> u64 { align256(self.vert_cap * 32) }
    pub(crate) fn idx_region_bytes(&self) -> u64 { align256(self.idx_cap * 4) }
    /// Gate 0 (bytes/body baseline) — total physical VRAM this slot's region pair occupies
    /// (256-aligned vertex + index spans). Summed over a body's segments by
    /// [`SdfMeshPool::body_vram_bytes`] — the `SdfPoolSlot::region_bytes()` the DC-consolidation plan
    /// called for (it did not exist; the pieces did).
    pub(crate) fn region_bytes(&self) -> u64 { self.vert_region_bytes() + self.idx_region_bytes() }
    /// DrawIndexedIndirect entries the body draws this frame: its live tiles, or the single
    /// monolithic entry. (0 live tiles on a tiled slot = empty surface → nothing drawn.)
    pub(crate) fn draw_entries(&self) -> u32 {
        if self.tiles.is_empty() { 1 } else { self.tiles.len() as u32 }
    }
    /// Absolute base of tile `t` in the group buffers (VERTICES / u32s).
    pub(crate) fn tile_abs(&self, t: &SdfTileEntry) -> (u64, u64) {
        (self.vert_off / 32 + t.rel_vert, self.idx_off / 4 + t.rel_idx)
    }
    /// C2 — region-relative packing high-water marks (max rel+cap over ALL entries, dead
    /// included — a dead entry's region is a hole, never re-aliased before the next full
    /// repack). Incremental tile grows/births append here.
    pub(crate) fn tile_watermarks(&self) -> (u64, u64) {
        self.tiles.iter().fold((0, 0), |(v, i), t| {
            (v.max(t.rel_vert + t.vert_cap), i.max(t.rel_idx + t.idx_cap))
        })
    }
}

/// Build one `TileInfoGpu` from a CPU tile entry. `entry` = the tile's REAL entry index in its
/// GROUP's counter/args space (travels in tmeta.z — the C2 compaction contract; a full dispatch
/// passes `args_base + position`).
fn tile_info_gpu(s: &SdfPoolSlot, t: &SdfTileEntry, entry: u32, first_instance: u32) -> super::TileInfoGpu {
    let (vb, ib) = s.tile_abs(t);
    super::TileInfoGpu {
        amin: [t.amin[0], t.amin[1], t.amin[2], vb as u32],
        amax: [t.amax[0], t.amax[1], t.amax[2], t.vert_cap as u32],
        vmn: [t.amin[0].saturating_sub(1), t.amin[1].saturating_sub(1),
              t.amin[2].saturating_sub(1), ib as u32],
        tmeta: [t.idx_cap as u32, first_instance, entry, 0],
    }
}

/// Phase 4 — ONE buffer group: its own region pair + entry-indexed triple + allocator state.
/// Every buffer here has this group as its ONE owner (GPU Resource Ownership); recreation of any
/// of them bumps `generation` (and the pool-global one) — consumers caching a bind group or
/// offset over group buffers compare against it (generation counters over timing tricks).
pub(crate) struct SdfPoolGroup {
    pub(crate) verts: wgpu::Buffer,     // STORAGE|VERTEX|COPY_SRC|COPY_DST — 8 f32/vert regions
    pub(crate) idx: wgpu::Buffer,       // STORAGE|INDEX|COPY_SRC|COPY_DST (Uint32) regions
    /// INDIRECT|STORAGE|COPY_DST|COPY_SRC — `args_cap_total` × DrawIndexedIndirect entries.
    /// STORAGE: the C1 `dc_tile_args` pass writes tiled entries directly (binding 4 as u32s).
    pub(crate) indirect: wgpu::Buffer,
    /// C1 tiled DC — per-ENTRY metadata, parallel to `indirect` (entry `e` owns tile_info[e] +
    /// tile_counters[e*2..+2]): the TileInfo array (binding 8) and the per-tile atomic counter
    /// pairs (binding 5 on the tiled path).
    pub(crate) tile_info: wgpu::Buffer,
    pub(crate) tile_counters: wgpu::Buffer,
    verts_bytes: u64,
    idx_bytes: u64,
    /// Bump pointers: next free byte at the tail of each region buffer. A relocated slot's old
    /// region becomes a hole reclaimed by this group's next rebuild.
    bump_verts: u64,
    bump_idx: u64,
    /// Entry capacity of `indirect`/`tile_info`/`tile_counters` (args blocks share this space).
    args_cap_total: u32,
    args_next: u32,
    /// Freed args blocks (base, cap) — best-fit reused before bumping `args_next`.
    args_free: Vec<(u32, u32)>,
    /// Bumped on EVERY buffer recreate of THIS group (rebuild / args grow) AND on every slot
    /// relocation within it. A rebuild of one group NEVER bumps another group's generation —
    /// per-group invalidation is the Phase-4 point (never multiply the rebuild cost).
    pub(crate) generation: u64,
}

impl SdfPoolGroup {
    fn region_bytes(&self) -> u64 { self.verts_bytes + self.idx_bytes }
    /// Total group VRAM (regions + entry-indexed meta) for diagnostics.
    fn vram_bytes(&self) -> u64 {
        self.region_bytes()
            + self.args_cap_total as u64
                * (SDF_POOL_INDIRECT_STRIDE + SDF_POOL_TILE_INFO_STRIDE + SDF_POOL_TILE_COUNTER_STRIDE)
    }
}

/// Phase 4c — the bounded free-list of RETIRED region buffers (see `SDF_POOL_RECYCLE_MAX_BYTES`).
/// Verts and idx buffers carry different `BufferUsages`, so a buffer only ever serves its own kind
/// (`is_verts`). A reused buffer is bit-for-bit whatever it last held — the mesher rewrites the
/// requester's region and `rebuild_group` GPU-copies survivors, so only the never-drawn GAPS keep
/// stale bytes, exactly as a fresh buffer's uninitialized holes are never drawn (the pool's
/// hole-safety invariant). Reuse still swaps the `wgpu::Buffer` identity → the group generation
/// bumps → consumers rebuild bind groups, so recycling is transparent to the generation-counter
/// machinery (contents changed ⇒ gen bumped, correctly).
#[derive(Default)]
struct RetiredBuffers {
    /// (buffer, physical size bytes, is_verts). FIFO-ish; small (a handful) so linear scans are fine.
    bufs: VecDeque<(wgpu::Buffer, u64, bool)>,
    held_bytes: u64,
}

impl RetiredBuffers {
    /// Best-fit reuse: the smallest retired buffer of the right kind in `[need, need*2]` — big
    /// enough to host the region, not so oversized we pin a huge buffer for a small need (a trim to
    /// a small region then allocates fresh and this drains the bigs at settle). `None` ⇒ create.
    fn take(&mut self, need: u64, is_verts: bool) -> Option<(wgpu::Buffer, u64)> {
        let hi = need.saturating_mul(2);
        let mut best: Option<usize> = None;
        for (i, (_, sz, iv)) in self.bufs.iter().enumerate() {
            if *iv == is_verts && *sz >= need && *sz <= hi
                && best.map_or(true, |b| self.bufs[b].1 > *sz) { best = Some(i); }
        }
        best.map(|i| { let (b, sz, _) = self.bufs.remove(i).expect("scanned index"); self.held_bytes -= sz; (b, sz) })
    }

    /// Retire a replaced buffer for later reuse; evict the OLDEST held buffers (dropping frees
    /// their VRAM) until the free-list is back under the byte cap. A buffer bigger than the whole
    /// cap is never held (dropped immediately — it would evict everything then sit alone).
    fn retire(&mut self, buf: wgpu::Buffer, size: u64, is_verts: bool) {
        if size > SDF_POOL_RECYCLE_MAX_BYTES { return; }
        self.bufs.push_back((buf, size, is_verts));
        self.held_bytes += size;
        while self.held_bytes > SDF_POOL_RECYCLE_MAX_BYTES {
            match self.bufs.pop_front() { Some((_, s, _)) => self.held_bytes -= s, None => break }
        }
    }

    /// Release every held buffer (settle-trim: the gesture ended, reclaim the VRAM).
    fn drain(&mut self) { self.bufs.clear(); self.held_bytes = 0; }
    fn len(&self) -> usize { self.bufs.len() }
}

/// The shared sparse-SDF mesh pool. Owned by `GpuCtx.sdf_mesh_pool`; created lazily in
/// `poll_background_tasks` (Frame Ordering Contract) on the first sparse mesh dispatch.
pub(crate) struct SdfMeshPool {
    /// Phase 4 — the buffer groups. Never shrinks (group indices are stored on slots); an emptied
    /// group's buffers shrink to demand on its next rebuild. Group 0 exists from `new`.
    groups: Vec<SdfPoolGroup>,
    pub(crate) instances: wgpu::Buffer, // VERTEX|COPY_DST — inst_cap × InstanceData (per-frame written)
    /// C1 — the batch cell_vert scratch (binding 2 on the tiled path): `tile_scratch_tiles ×
    /// SDF_TILE_CV_STRIDE` u32 slots, reused across batches (pass boundaries order the reuse).
    /// SHARED across groups: it is transient per-dispatch scratch, never addressed by entry index.
    pub(crate) tile_scratch: Option<wgpu::Buffer>,
    tile_scratch_tiles: u32,
    /// C2 — the COMPACTED TileInfo staging buffer for incremental dispatches (binding 8 of a
    /// dirty-tile-only dispatch; the group's shared `tile_info` stays untouched for partials).
    pub(crate) tile_info_stage: Option<wgpu::Buffer>,
    tile_info_stage_cap: u32,
    /// C1 — per-batch DcParams entries (256B stride; uniform sub-range bindings). One entry per
    /// batch of a tiled dispatch + one for the args-fill pass, all written before the ONE submit.
    pub(crate) tile_params: wgpu::Buffer,
    tile_params_cap: u32,
    /// Instance capacity (bodies) of `instances`.
    slots_cap: u32,
    slots: HashMap<SlotKey, SdfPoolSlot>,
    free_indices: Vec<u32>,
    next_index: u32,
    /// C1 — the per-pass draw list: bodies marked drawn by THIS pass's prep (`mark_drawn`),
    /// cleared by `clear_frame_visibility`. The draw issues one `multi_draw_indexed_indirect`
    /// per listed body over its args block — a hidden/dense-switched body is simply never
    /// listed (replaces per-entry instance_count zeroing, which cannot scale to per-tile
    /// entries: N tiles would need N strided 4-byte writes per pass per frame).
    frame_drawn: Vec<NodeId>,
    /// POOL-GLOBAL generation: bumped whenever ANY group's generation bumps or a pool-shared
    /// buffer (instances / scratch / stage / params) is recreated. Coarse consumers compare this;
    /// per-group consumers compare `SdfPoolGroup.generation`.
    pub(crate) generation: u64,
    /// True when the device has both `MULTI_DRAW_INDIRECT` and `INDIRECT_FIRST_INSTANCE`: args
    /// carry `first_instance = slot index` and each drawn body is ONE
    /// `multi_draw_indexed_indirect` over its args block. False → args keep first_instance=0 and
    /// the raster fallback loops per-entry `draw_indexed_indirect` with a per-draw instance
    /// binding (same geometry, same buffers).
    pub(crate) multi_draw_ready: bool,
    /// Per-group-buffer size cap (production = `SDF_POOL_MAX_BUFFER_BYTES`). pub(crate) so tests
    /// shrink it to force multi-group placement deterministically (the headroom-shrink pattern).
    pub(crate) max_group_bytes: u64,
    /// Total region budget across groups (production = `SDF_POOL_TOTAL_BUDGET_BYTES`). pub(crate)
    /// so tests shrink it to force the per-body fallback deterministically.
    pub(crate) total_budget_bytes: u64,
    /// Phase 4c — retired region buffers kept for reuse across rebuilds (skip create + zero-init).
    retired: RetiredBuffers,
    /// Lifetime count of region buffers served from the recycle free-list (instrumentation / test).
    recycle_hits: u32,
}

impl SdfMeshPool {
    /// CALL ONLY FROM `poll_background_tasks` (Frame Ordering Contract). Starts small (group 0 at
    /// 1 MiB per region buffer, 16 slot metas); grows on demand. wgpu zero-initializes buffers, so
    /// every unseeded indirect entry reads instance_count=0 → a multi-draw no-op (the voxel-pool
    /// "empty slots self-skip" invariant).
    pub(crate) fn new(device: &wgpu::Device, multi_draw_ready: bool) -> Self {
        const INIT_REGION: u64 = 1 << 20;
        const INIT_SLOTS: u32 = 16;
        const INIT_PARAMS: u32 = 8;
        let group0 = Self::make_group(device, INIT_REGION, INIT_REGION);
        let instances = Self::make_instance_buffer(device, INIT_SLOTS);
        let tile_params = Self::make_params_buffer(device, INIT_PARAMS);
        Self {
            groups: vec![group0],
            instances,
            tile_scratch: None, tile_scratch_tiles: 0,
            tile_info_stage: None, tile_info_stage_cap: 0,
            tile_params, tile_params_cap: INIT_PARAMS,
            slots_cap: INIT_SLOTS,
            slots: HashMap::new(), free_indices: Vec::new(), next_index: 0,
            frame_drawn: Vec::new(),
            generation: 0, multi_draw_ready,
            max_group_bytes: SDF_POOL_MAX_BUFFER_BYTES,
            total_budget_bytes: SDF_POOL_TOTAL_BUDGET_BYTES,
            retired: RetiredBuffers::default(),
            recycle_hits: 0,
        }
    }

    /// Lifetime count of region buffers served from the recycle free-list (diagnostics / tests).
    pub(crate) fn recycle_hits(&self) -> u32 { self.recycle_hits }
    /// Release every retired region buffer (settle/quiet — reclaim the recycle reserve VRAM). The
    /// mesh layer calls this when a body meshes QUIET (`may_trim`), so the churn-time reserve never
    /// lingers on a stable body. POLL PHASE ONLY (drops GPU buffers — wgpu defers the actual free
    /// past any pending submit that still references them).
    pub(crate) fn drain_recycle(&mut self) { self.retired.drain(); }
    /// Retired-buffer count (diagnostics / tests).
    pub(crate) fn retired_len(&self) -> usize { self.retired.len() }

    /// Group 0 from `new` (1 MiB regions, no recycle candidates yet). Groups born later go through
    /// `assemble_group` with recycle-acquired region buffers (`take_region_buffers`).
    fn make_group(device: &wgpu::Device, vbytes: u64, ibytes: u64) -> SdfPoolGroup {
        let (verts, idx) = Self::make_region_buffers(device, vbytes, ibytes);
        Self::assemble_group(device, verts, idx, vbytes.max(256), ibytes.max(256))
    }

    /// Wrap already-acquired region buffers (fresh OR recycled) + a fresh args triple into a group.
    /// `vbytes`/`ibytes` are the buffers' PHYSICAL sizes (a recycled buffer may exceed the request —
    /// the extra tail is free bump room, so the group records the real size).
    fn assemble_group(device: &wgpu::Device, verts: wgpu::Buffer, idx: wgpu::Buffer, vbytes: u64, ibytes: u64) -> SdfPoolGroup {
        const INIT_ARGS: u32 = 64;
        let (indirect, tile_info, tile_counters) = Self::make_args_buffers(device, INIT_ARGS);
        SdfPoolGroup {
            verts, idx, indirect, tile_info, tile_counters,
            verts_bytes: vbytes.max(256), idx_bytes: ibytes.max(256),
            bump_verts: 0, bump_idx: 0,
            args_cap_total: INIT_ARGS, args_next: 0, args_free: Vec::new(),
            generation: 0,
        }
    }

    fn make_verts_buffer(device: &wgpu::Device, bytes: u64) -> wgpu::Buffer {
        use wgpu::BufferUsages as U;
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_pool_verts"), size: bytes.max(256),
            usage: U::STORAGE | U::VERTEX | U::COPY_SRC | U::COPY_DST, mapped_at_creation: false })
    }
    fn make_idx_buffer(device: &wgpu::Device, bytes: u64) -> wgpu::Buffer {
        use wgpu::BufferUsages as U;
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_pool_idx"), size: bytes.max(256),
            usage: U::STORAGE | U::INDEX | U::COPY_SRC | U::COPY_DST, mapped_at_creation: false })
    }
    fn make_region_buffers(device: &wgpu::Device, vbytes: u64, ibytes: u64) -> (wgpu::Buffer, wgpu::Buffer) {
        (Self::make_verts_buffer(device, vbytes), Self::make_idx_buffer(device, ibytes))
    }

    /// Phase 4c — acquire a region-buffer pair for a rebuild/new-group, REUSING a retired buffer of
    /// sufficient size when one exists (skips `create_buffer` AND wgpu zero-init). Returns the
    /// buffers, their PHYSICAL sizes (≥ requested when recycled), and how many of the two were
    /// recycled (instrumentation). POLL PHASE ONLY (Frame Ordering Contract — allocation).
    fn take_region_buffers(&mut self, device: &wgpu::Device, req_v: u64, req_i: u64)
        -> (wgpu::Buffer, wgpu::Buffer, u64, u64, u32)
    {
        let (rv, ri) = (req_v.max(256), req_i.max(256));
        let mut recycled = 0u32;
        let (verts, av) = match self.retired.take(rv, true) {
            Some((b, sz)) => { recycled += 1; (b, sz) }
            None => (Self::make_verts_buffer(device, rv), rv),
        };
        let (idx, ai) = match self.retired.take(ri, false) {
            Some((b, sz)) => { recycled += 1; (b, sz) }
            None => (Self::make_idx_buffer(device, ri), ri),
        };
        self.recycle_hits += recycled;
        (verts, idx, av, ai, recycled)
    }

    fn make_instance_buffer(device: &wgpu::Device, slots: u32) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_pool_instances"), size: slots as u64 * SDF_POOL_INSTANCE_STRIDE,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    fn make_args_buffers(device: &wgpu::Device, entries: u32) -> (wgpu::Buffer, wgpu::Buffer, wgpu::Buffer) {
        use wgpu::BufferUsages as U;
        let indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_pool_indirect"), size: entries as u64 * SDF_POOL_INDIRECT_STRIDE,
            usage: U::INDIRECT | U::STORAGE | U::COPY_DST | U::COPY_SRC, mapped_at_creation: false });
        let tile_info = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_pool_tile_info"), size: entries as u64 * SDF_POOL_TILE_INFO_STRIDE,
            usage: U::STORAGE | U::COPY_DST | U::COPY_SRC, mapped_at_creation: false });
        let tile_counters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_pool_tile_counters"), size: entries as u64 * SDF_POOL_TILE_COUNTER_STRIDE,
            usage: U::STORAGE | U::COPY_DST | U::COPY_SRC, mapped_at_creation: false });
        (indirect, tile_info, tile_counters)
    }

    fn make_params_buffer(device: &wgpu::Device, entries: u32) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_pool_tile_params"), size: entries as u64 * 256,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    /// Bump group `gi`'s generation AND the pool-global one (per-group consumers compare the
    /// group's; coarse consumers the pool's — both must see every group event).
    fn bump_group_gen(&mut self, gi: usize) {
        self.groups[gi].generation += 1;
        self.generation += 1;
    }

    pub(crate) fn slot(&self, nid: NodeId) -> Option<&SdfPoolSlot> { self.slots.get(&(nid, 0)) }
    /// Phase 4b — segment `seg` of body `nid` (segment 0 == `slot(nid)`).
    pub(crate) fn slot_seg(&self, nid: NodeId, seg: u32) -> Option<&SdfPoolSlot> {
        self.slots.get(&(nid, seg))
    }
    /// Phase 4b — number of contiguous segments of body `nid` (0 = not pooled). Segment indices
    /// are dense from 0 by construction (`free_segments_from` keeps them gapless).
    pub(crate) fn segment_count(&self, nid: NodeId) -> u32 {
        let mut n = 0;
        while self.slots.contains_key(&(nid, n)) { n += 1; }
        n
    }
    /// Upper bound of live INSTANCE indices (bodies ever pooled this session).
    pub(crate) fn slot_count_hi(&self) -> u32 { self.next_index }
    /// The bodies marked drawn by this pass's prep (the C1 per-body multi-draw list).
    pub(crate) fn frame_drawn(&self) -> &[NodeId] { &self.frame_drawn }
    /// Phase 4 — the buffer group of body `nid`'s segment 0.
    pub(crate) fn group_of(&self, nid: NodeId) -> Option<&SdfPoolGroup> {
        self.slots.get(&(nid, 0)).map(|s| &self.groups[s.group])
    }
    /// Phase 4 — group by index (for draw loops that already resolved `slot.group`).
    pub(crate) fn group(&self, gi: usize) -> &SdfPoolGroup { &self.groups[gi] }
    pub(crate) fn group_count(&self) -> usize { self.groups.len() }
    /// Diagnostics — group `gi`'s remaining bump tail in (vert bytes, idx bytes). Tests use it to
    /// consume the tail exactly; the log line uses it for occupancy.
    pub(crate) fn group_tail_bytes(&self, gi: usize) -> (u64, u64) {
        let g = &self.groups[gi];
        (g.verts_bytes - g.bump_verts, g.idx_bytes - g.bump_idx)
    }
    /// Physical (verts, idx) region-buffer sizes of group `gi` — for the demand-size gate.
    pub(crate) fn group_region_bytes(&self, gi: usize) -> (u64, u64) {
        let g = &self.groups[gi];
        (g.verts_bytes, g.idx_bytes)
    }
    /// Gate 0 (bytes/body baseline) — per-body resident-mesh VRAM: Σ region bytes over the body's
    /// pool SEGMENTS (0 when the body is not pooled). A multi-segment/multi-group body sums across
    /// all of its segments, so the number is the whole body's mesh footprint.
    pub(crate) fn body_vram_bytes(&self, nid: NodeId) -> u64 {
        (0..self.segment_count(nid))
            .filter_map(|seg| self.slot_seg(nid, seg))
            .map(|s| s.region_bytes())
            .sum()
    }
    /// Gate 0 (pool fragmentation baseline) — fraction of the bump-allocated region space that is
    /// HOLES: space past relocated/freed slots, reclaimed only on the next group rebuild. 0.0 =
    /// perfectly packed. `Σ live-slot region bytes` vs `Σ per-group bump watermark`.
    pub(crate) fn hole_ratio(&self) -> f32 {
        let bumped: u64 = self.groups.iter().map(|g| g.bump_verts + g.bump_idx).sum();
        if bumped == 0 { return 0.0; }
        let live: u64 = self.slots.values().map(|s| s.region_bytes()).sum();
        1.0 - (live.min(bumped) as f32 / bumped as f32)
    }
    /// Total pool VRAM (all groups + shared meta buffers) for diagnostics.
    pub(crate) fn vram_bytes(&self) -> u64 {
        self.groups.iter().map(|g| g.vram_bytes()).sum::<u64>()
            + self.slots_cap as u64 * SDF_POOL_INSTANCE_STRIDE
            + self.tile_scratch_tiles as u64 * super::SDF_TILE_CV_STRIDE * 4
            + self.tile_info_stage_cap as u64 * SDF_POOL_TILE_INFO_STRIDE
            + self.tile_params_cap as u64 * 256
    }
    /// Σ region-buffer bytes across groups (the quantity `total_budget_bytes` governs).
    fn total_region_bytes(&self) -> u64 {
        self.groups.iter().map(|g| g.region_bytes()).sum()
    }
    /// Per-group-buffer hard cap for this device.
    fn safe_max(&self, device: &wgpu::Device) -> u64 {
        device.limits().max_buffer_size
            .min(device.limits().max_storage_buffer_binding_size as u64)
            .min(self.max_group_bytes)
    }

    /// Seed one slot's `DrawIndexedIndirect` entry in its group: [index_count=0 (GPU-written by
    /// the DC counters copy), instance_count=0 (CPU visibility switch — the draw prep sets 1 per
    /// drawn frame), first_index, base_vertex, first_instance]. Monolithic (single-entry) slots
    /// only — tiled blocks are published by the `dc_tile_args` GPU pass.
    fn seed_indirect(&self, queue: &wgpu::Queue, s: &SdfPoolSlot) {
        let first_instance = if self.multi_draw_ready { s.index } else { 0 };
        let args = [0u32, 0, (s.idx_off / 4) as u32, (s.vert_off / 32) as u32, first_instance];
        queue.write_buffer(&self.groups[s.group].indirect, s.indirect_offset(), bytemuck::cast_slice(&args));
    }

    // ── C1 tiled-DC entry-space management (per group) ──────────────────────────────────────────

    /// Grow group `gi`'s ENTRY-indexed buffers (`indirect` + `tile_info` + `tile_counters`) to
    /// hold `want` entries, GPU-copying existing content (surviving blocks keep their GPU-written
    /// index_counts/counters in place — entry indices are stable across this grow). Group
    /// generation bumped. POLL PHASE ONLY (Frame Ordering Contract).
    fn ensure_args_capacity(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, gi: usize, want: u32) {
        if want <= self.groups[gi].args_cap_total { return; }
        let old_cap = self.groups[gi].args_cap_total;
        let new_cap = want.max(old_cap * 2);
        let (indirect, tile_info, tile_counters) = Self::make_args_buffers(device, new_cap);
        let g = &self.groups[gi];
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sdf_pool_args_grow") });
        enc.copy_buffer_to_buffer(&g.indirect, 0, &indirect, 0, old_cap as u64 * SDF_POOL_INDIRECT_STRIDE);
        enc.copy_buffer_to_buffer(&g.tile_info, 0, &tile_info, 0, old_cap as u64 * SDF_POOL_TILE_INFO_STRIDE);
        enc.copy_buffer_to_buffer(&g.tile_counters, 0, &tile_counters, 0, old_cap as u64 * SDF_POOL_TILE_COUNTER_STRIDE);
        queue.submit(std::iter::once(enc.finish()));
        let g = &mut self.groups[gi];
        g.indirect = indirect;
        g.tile_info = tile_info;
        g.tile_counters = tile_counters;
        g.args_cap_total = new_cap;
        self.bump_group_gen(gi);
    }

    /// Allocate an args block of `cap` entries in group `gi`: best-fit from that group's free
    /// list, else tail bump.
    fn alloc_args_block(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, gi: usize, cap: u32) -> u32 {
        // Best-fit search over freed blocks (bodies are dozens — linear is fine).
        let g = &mut self.groups[gi];
        let mut best: Option<usize> = None;
        for (i, &(_, c)) in g.args_free.iter().enumerate() {
            if c >= cap && best.map_or(true, |b| g.args_free[b].1 > c) { best = Some(i); }
        }
        if let Some(i) = best {
            let (base, c) = g.args_free.swap_remove(i);
            // Return the unused tail to the free list (avoid leaking capacity).
            if c > cap { g.args_free.push((base + cap, c - cap)); }
            return base;
        }
        let base = g.args_next;
        self.ensure_args_capacity(device, queue, gi, base + cap);
        self.groups[gi].args_next = base + cap;
        base
    }

    /// Ensure body `nid`'s args block holds ≥ `want` entries (grow-only, ×1.25 headroom so a
    /// mildly fluctuating tile count doesn't churn blocks). A moved block gets fresh zeroed
    /// entries (the caller's tiled dispatch rewrites TileInfo + args + counters immediately).
    /// Returns true when the block moved. POLL PHASE ONLY.
    pub(crate) fn ensure_tile_block(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue, nid: NodeId, seg: u32, want: u32,
    ) -> bool {
        let key = (nid, seg);
        let Some(s) = self.slots.get(&key) else { return false; };
        if s.args_cap >= want { return false; }
        let (gi, old_base, old_cap) = (s.group, s.args_base, s.args_cap);
        let new_cap = (want + want / 4).max(4);
        let new_base = self.alloc_args_block(device, queue, gi, new_cap);
        // Zero the old block's entries (index_count=instance_count=0 → never drawn if referenced
        // by a stale offset) and recycle it.
        queue.write_buffer(&self.groups[gi].indirect, old_base as u64 * SDF_POOL_INDIRECT_STRIDE,
            &vec![0u8; (old_cap as u64 * SDF_POOL_INDIRECT_STRIDE) as usize]);
        self.groups[gi].args_free.push((old_base, old_cap));
        let s = self.slots.get_mut(&key).expect("checked above");
        s.args_base = new_base;
        s.args_cap = new_cap;
        self.bump_group_gen(gi); // entry indices moved — counters/args offsets change
        true
    }

    /// Install segment `seg` of body `nid`'s tile table (the caller computed windows/caps/offsets).
    pub(crate) fn set_tiles(&mut self, nid: NodeId, seg: u32, tiles: Vec<SdfTileEntry>) {
        if let Some(s) = self.slots.get_mut(&(nid, seg)) {
            debug_assert!(tiles.len() as u32 <= s.args_cap, "tile table exceeds the args block");
            s.tiles = tiles;
        }
    }

    /// Ensure the shared batch scratch holds `tiles` × SDF_TILE_CV_STRIDE u32 slots (grow-only).
    /// POLL PHASE ONLY.
    pub(crate) fn ensure_tile_scratch(&mut self, device: &wgpu::Device, tiles: u32) -> &wgpu::Buffer {
        if tiles > self.tile_scratch_tiles || self.tile_scratch.is_none() {
            self.tile_scratch = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_pool_tile_scratch"),
                size: (tiles as u64 * super::SDF_TILE_CV_STRIDE * 4).max(4),
                usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false }));
            self.tile_scratch_tiles = tiles;
            self.generation += 1;
        }
        self.tile_scratch.as_ref().expect("just ensured")
    }

    /// Ensure the per-batch params buffer holds `entries` 256B slots (grow-only). POLL PHASE ONLY.
    pub(crate) fn ensure_tile_params(&mut self, device: &wgpu::Device, entries: u32) -> &wgpu::Buffer {
        if entries > self.tile_params_cap {
            self.tile_params = Self::make_params_buffer(device, entries.max(self.tile_params_cap * 2));
            self.tile_params_cap = entries.max(self.tile_params_cap * 2);
            self.generation += 1;
        }
        &self.tile_params
    }

    /// (Re)write body `nid`'s TileInfo block (in its group) from its CPU tile table (absolute
    /// bases derived from the CURRENT region offsets — called by every tiled dispatch and by
    /// `rebuild_group` for surviving tiled bodies whose regions moved).
    pub(crate) fn write_tile_infos(&self, queue: &wgpu::Queue, nid: NodeId, seg: u32) {
        let Some(s) = self.slots.get(&(nid, seg)) else { return; };
        if s.tiles.is_empty() { return; }
        let first_instance = if self.multi_draw_ready { s.index } else { 0 };
        let infos: Vec<super::TileInfoGpu> = s.tiles.iter().enumerate().map(|(i, t)| {
            tile_info_gpu(s, t, s.args_base + i as u32, first_instance)
        }).collect();
        queue.write_buffer(&self.groups[s.group].tile_info,
            s.args_base as u64 * SDF_POOL_TILE_INFO_STRIDE, bytemuck::cast_slice(&infos));
    }

    /// C2 — rewrite ONE tile's TileInfo entry in its group's buffer (an incremental re-mesh
    /// updates only the dirty tiles' windows/caps/bases; a whole-block rewrite would be wasted
    /// bandwidth at 5k tiles). `pos` = position in the body's tile table.
    pub(crate) fn write_tile_info_at(&self, queue: &wgpu::Queue, nid: NodeId, seg: u32, pos: usize) {
        let Some(s) = self.slots.get(&(nid, seg)) else { return; };
        let Some(t) = s.tiles.get(pos) else { return; };
        let first_instance = if self.multi_draw_ready { s.index } else { 0 };
        let entry = s.args_base + pos as u32;
        let info = tile_info_gpu(s, t, entry, first_instance);
        queue.write_buffer(&self.groups[s.group].tile_info,
            entry as u64 * SDF_POOL_TILE_INFO_STRIDE, bytemuck::bytes_of(&info));
    }

    /// C2 — mutable access to a body's tile table for the incremental re-mesh (window/cap/dead
    /// updates + births at the tail). Region/args allocation still goes through the pool's own
    /// methods; this only edits the CPU-side table.
    pub(crate) fn tiles_mut(&mut self, nid: NodeId, seg: u32) -> Option<&mut Vec<SdfTileEntry>> {
        self.slots.get_mut(&(nid, seg)).map(|s| &mut s.tiles)
    }

    /// C2 — write the COMPACTED TileInfo array for an incremental dispatch: the dirty tiles (by
    /// position in `nid`'s table) packed contiguously into `tile_info_stage[0..n)`, each with
    /// `tmeta.z` = its REAL entry index (`args_base + pos`) so counters/args land home. POLL
    /// PHASE ONLY (may create/grow the stage buffer).
    pub(crate) fn stage_dirty_tile_infos(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue, nid: NodeId, seg: u32, positions: &[usize],
    ) {
        let Some(s) = self.slots.get(&(nid, seg)) else { return; };
        let first_instance = if self.multi_draw_ready { s.index } else { 0 };
        let infos: Vec<super::TileInfoGpu> = positions.iter().map(|&p| {
            tile_info_gpu(s, &s.tiles[p], s.args_base + p as u32, first_instance)
        }).collect();
        if infos.is_empty() { return; }
        let bytes: Vec<u8> = bytemuck::cast_slice(&infos).to_vec();
        let stage = self.ensure_tile_info_stage(device, positions.len() as u32);
        queue.write_buffer(stage, 0, &bytes);
    }

    /// C2 — ensure the COMPACTED scratch TileInfo buffer holds `entries` (grow-only). An
    /// incremental dispatch binds THIS buffer at binding 8 and ranges its params over
    /// [0, n_dirty); each scratch entry's tmeta.z routes counters/args to the live entry space,
    /// so the group's tile_info/counters/indirect buffers are never grown or disturbed for a
    /// partial dispatch. POLL PHASE ONLY (Frame Ordering Contract).
    pub(crate) fn ensure_tile_info_stage(&mut self, device: &wgpu::Device, entries: u32) -> &wgpu::Buffer {
        if entries > self.tile_info_stage_cap || self.tile_info_stage.is_none() {
            let cap = entries.max(self.tile_info_stage_cap * 2).max(64);
            self.tile_info_stage = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_pool_tile_info_stage"),
                size: cap as u64 * SDF_POOL_TILE_INFO_STRIDE,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false }));
            self.tile_info_stage_cap = cap;
            self.generation += 1;
        }
        self.tile_info_stage.as_ref().expect("just ensured")
    }

    // ── Phase 4 placement machinery ─────────────────────────────────────────────────────────────

    /// Packed footprint group `gi` would need with body `nid` at (`want_verts`, `want_idx`) —
    /// every OTHER member at its current caps, the requester counted once at the wanted caps
    /// (whether it currently lives in `gi`, another group, or nowhere).
    fn group_need(&self, gi: usize, key: SlotKey, want_verts: u64, want_idx: u64) -> (u64, u64) {
        let (mut nv, mut ni) = (align256(want_verts * 32), align256(want_idx * 4));
        for (n, s) in &self.slots {
            if s.group != gi || *n == key { continue; }
            nv += align256(s.vert_cap * 32);
            ni += align256(s.idx_cap * 4);
        }
        (nv, ni)
    }

    /// Rebuild sizing for group `gi` at packed need (`need_v`, `need_i`): ×1.5 headroom of DEMAND
    /// (never of the group's own previous size — the S0.3 ratchet lesson), capped per-buffer at
    /// `safe_max`, then clamped so the TOTAL across groups stays under `total_budget_bytes`
    /// (headroom dropped before giving up). `None` = this group cannot host the packing.
    fn rebuild_sizes(&self, gi: usize, need_v: u64, need_i: u64, safe_max: u64) -> Option<(u64, u64)> {
        if need_v > safe_max || need_i > safe_max { return None; }
        let others: u64 = self.groups.iter().enumerate()
            .filter(|(i, _)| *i != gi)
            .map(|(_, g)| g.region_bytes()).sum();
        let mut nv = (need_v * 3 / 2).min(safe_max).max(need_v);
        let mut ni = clamp_idx_headroom(nv, (need_i * 3 / 2).min(safe_max).max(need_i), need_i);
        if others + nv + ni > self.total_budget_bytes {
            nv = need_v;
            ni = need_i;
            if others + nv + ni > self.total_budget_bytes { return None; }
        }
        Some((nv, ni))
    }

    /// Ensure `nid`'s args block (and tile table) lives in group `gi`; returns
    /// (args_base, args_cap, tiles) with the tiles TAKEN out of the slot (the caller re-installs
    /// them on the new/updated slot). A cross-group MOVE zeroes + frees the old group's block
    /// (nothing stale can draw) and allocates a fresh one in `gi` sized to hold the tile table
    /// (`ensure_tile_block` grows it later if the tile count rises).
    fn adopt_args(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, gi: usize, key: SlotKey)
        -> (u32, u32, Vec<SdfTileEntry>)
    {
        match self.slots.get(&key).map(|s| s.group) {
            Some(g) if g == gi => {
                let s = self.slots.get_mut(&key).expect("just matched");
                (s.args_base, s.args_cap, std::mem::take(&mut s.tiles))
            }
            Some(old_gi) => {
                let (old_base, old_cap, tiles) = {
                    let s = self.slots.get_mut(&key).expect("just matched");
                    (s.args_base, s.args_cap, std::mem::take(&mut s.tiles))
                };
                queue.write_buffer(&self.groups[old_gi].indirect,
                    old_base as u64 * SDF_POOL_INDIRECT_STRIDE,
                    &vec![0u8; (old_cap as u64 * SDF_POOL_INDIRECT_STRIDE) as usize]);
                self.groups[old_gi].args_free.push((old_base, old_cap));
                self.bump_group_gen(old_gi); // the body left — its entries in old_gi are void
                let cap = (tiles.len() as u32).max(1);
                let base = self.alloc_args_block(device, queue, gi, cap);
                (base, cap, tiles)
            }
            None => (self.alloc_args_block(device, queue, gi, 1), 1, Vec::new()),
        }
    }

    /// Place body `nid` at group `gi`'s bump tail (caller verified the tail room). Handles
    /// new/grown/moved slots uniformly via `adopt_args`.
    fn place_slot_bump(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue, gi: usize,
        key: SlotKey, want_verts: u64, want_idx: u64, index: u32,
    ) {
        let relocated = self.slots.contains_key(&key);
        let (args_base, args_cap, tiles) = self.adopt_args(device, queue, gi, key);
        let g = &mut self.groups[gi];
        let s = SdfPoolSlot {
            group: gi,
            vert_off: g.bump_verts, vert_cap: want_verts,
            idx_off: g.bump_idx, idx_cap: want_idx, index,
            args_base, args_cap, tiles,
        };
        g.bump_verts += s.vert_region_bytes();
        g.bump_idx += s.idx_region_bytes();
        if s.tiles.is_empty() { self.seed_indirect(queue, &s); }
        self.slots.insert(key, s);
        if relocated { self.bump_group_gen(gi); } // offsets moved — invalidate cached consumers
    }

    /// Ensure body `nid` owns a pool region of at least `want_verts` vertices / `want_idx` u32
    /// indices. GROW-ONLY per slot (the R3b `ensure_output_caps` discipline): sufficient capacity
    /// is a strict no-op (`Ok(false)`); a grow RELOCATES the slot (bump-append in its group, a
    /// per-GROUP rebuild, a MOVE to another group with room, or a NEW group under the total
    /// budget) and returns `Ok(true)` — the caller MUST re-dispatch the mesh into the new region
    /// (its old content is not carried; the mesher rewrite follows immediately in
    /// `mesh_into_slot_sparse*`). Other slots' content IS preserved across a group rebuild (GPU
    /// region copies + index_count copy), and OTHER groups are never touched by it. `Err(())` =
    /// the request cannot fit under the per-buffer cap / total budget — the caller falls back to
    /// the legacy per-body slot buffers. CALL ONLY FROM poll-phase code (Frame Ordering Contract).
    pub(crate) fn ensure_slot(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue,
        nid: NodeId, want_verts: u64, want_idx: u64,
    ) -> Result<bool, ()> {
        self.ensure_slot_seg(device, queue, nid, 0, want_verts, want_idx)
    }

    /// Phase 4b — segment-addressed `ensure_slot`. Segment 0 must exist before any higher
    /// segment (it owns the body's shared instance index — all segments draw with the SAME
    /// `first_instance`, so one `InstanceData` write covers the whole body).
    pub(crate) fn ensure_slot_seg(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue,
        nid: NodeId, seg: u32, want_verts: u64, want_idx: u64,
    ) -> Result<bool, ()> {
        let key: SlotKey = (nid, seg);
        if let Some(s) = self.slots.get(&key) {
            if s.vert_cap >= want_verts && s.idx_cap >= want_idx { return Ok(false); }
        }
        let vreg = align256(want_verts * 32);
        let ireg = align256(want_idx * 4);
        let safe_max = self.safe_max(device);
        // One body's region must fit ONE group buffer (tiles never span groups) — larger requests
        // are per-body-fallback territory regardless of how many groups the budget allows.
        if vreg > safe_max || ireg > safe_max { return Err(()); }

        // Stable INSTANCE index: reuse the existing one on a grow; a NEW higher segment shares
        // segment 0's index (one instance per BODY); a new body allocates free-list/bump.
        let index = match self.slots.get(&key) {
            Some(s) => s.index,
            None if seg > 0 => self.slots.get(&(nid, 0))
                .expect("segment 0 must exist before higher segments").index,
            None => match self.free_indices.pop() {
                Some(i) => i,
                None => { let i = self.next_index; self.next_index += 1; i }
            },
        };
        if index >= self.slots_cap {
            self.ensure_meta_capacity(device, (index + 1).max(self.slots_cap * 2));
        }
        let existed = self.slots.contains_key(&key);
        let cur_group = self.slots.get(&key).map(|s| s.group);

        // Candidate order: the body's current group first (grow in place — no cross-group churn),
        // then the other groups in index order (first group with room).
        let mut order: Vec<usize> = Vec::with_capacity(self.groups.len());
        if let Some(g) = cur_group { order.push(g); }
        order.extend((0..self.groups.len()).filter(|gi| Some(*gi) != cur_group));

        // Pass 1 — bump-append into the first group with tail room (a grown slot's old region
        // becomes a hole until that group's next rebuild repacks — bounded: bodies are dozens
        // and grows are rare).
        for &gi in &order {
            let g = &self.groups[gi];
            if g.bump_verts + vreg <= g.verts_bytes && g.bump_idx + ireg <= g.idx_bytes {
                self.place_slot_bump(device, queue, gi, key, want_verts, want_idx, index);
                return Ok(true);
            }
        }

        // Pass 2 — per-GROUP rebuild: the first group whose repacked footprint (with the
        // requester at the wanted caps) fits the per-buffer cap + total budget. Feasibility is
        // checked BEFORE any mutation, so the rebuild itself cannot fail and nothing can leak.
        for &gi in &order {
            let (need_v, need_i) = self.group_need(gi, key, want_verts, want_idx);
            let Some((nv, ni)) = self.rebuild_sizes(gi, need_v, need_i, safe_max) else { continue; };
            let (args_base, args_cap, tiles) = self.adopt_args(device, queue, gi, key);
            // Normalize the requester into `gi` so rebuild_group sees one uniform member set.
            if let Some(s) = self.slots.get_mut(&key) {
                s.group = gi;
                s.args_base = args_base;
                s.args_cap = args_cap;
                s.tiles = tiles;
            }
            self.rebuild_group(device, queue, gi, key, want_verts, want_idx, index,
                               (args_base, args_cap), need_v, need_i, nv, ni);
            return Ok(true);
        }

        // Pass 3 — NEW group under the total budget (Phase 4: the multi-buffer point). Sized like
        // a rebuild (×1.5 of the request, headroom dropped before giving up; idx headroom clamped
        // to 0.75× the verts region — the idx=6×vert invariant, `clamp_idx_headroom`).
        let total = self.total_region_bytes();
        let mut nv = (vreg * 3 / 2).min(safe_max).max(vreg);
        let mut ni = clamp_idx_headroom(nv, (ireg * 3 / 2).min(safe_max).max(ireg), ireg);
        if total + nv + ni > self.total_budget_bytes {
            nv = vreg;
            ni = ireg;
            if total + nv + ni > self.total_budget_bytes {
                // A NEW body's just-allocated instance index must not leak on the fallback
                // (a new HIGHER segment shares seg 0's index — nothing to return).
                if !existed && seg == 0 { self.free_indices.push(index); }
                eprintln!("[SdfPool] body={nid:?} request (v={want_verts} i={want_idx}) exceeds the \
                           {}MB total pool budget ({} groups, {}MB allocated) — per-body fallback",
                    self.total_budget_bytes >> 20, self.groups.len(), total >> 20);
                return Err(());
            }
        }
        // Phase 4c: a new group's region buffers RECYCLE a retired pair when one fits (a group that
        // was rebuilt/trimmed away just now can feed this birth) — skips create + zero-init.
        let _t_create = std::time::Instant::now();
        let (verts, idx, av, ai, recycled) = self.take_region_buffers(device, nv, ni);
        let create_ms = _t_create.elapsed().as_secs_f64() * 1000.0;
        self.groups.push(Self::assemble_group(device, verts, idx, av, ai));
        let gi = self.groups.len() - 1;
        self.generation += 1; // a new group appeared — coarse consumers re-evaluate
        eprintln!("[SdfPool] group {gi} created (verts {:.1}MB + idx {:.1}MB) — {} groups, total {:.1}MB / budget {:.0}MB \
                   | acquire={:.2}ms(recycled {}/2)",
            av as f64 / 1.0e6, ai as f64 / 1.0e6, self.groups.len(),
            self.total_region_bytes() as f64 / 1.0e6, self.total_budget_bytes as f64 / 1.0e6,
            create_ms, recycled);
        self.place_slot_bump(device, queue, gi, key, want_verts, want_idx, index);
        Ok(true)
    }

    /// REBUILD group `gi` ONLY: repack every slot of that group contiguously into NEW buffers,
    /// GPU-copy every OTHER member's region old→new (content preserved — no remesh storm), reseed
    /// that group's indirect args with the new offsets and GPU-copy each surviving slot's
    /// index_count so its mesh keeps drawing. The requesting slot gets its (possibly grown)
    /// region but NO content copy — its caller re-dispatches the mesh immediately. Other GROUPS
    /// are untouched (their buffers, contents, and generations do not change — the Phase-4
    /// isolation property; never multiply the ~50ms rebuild across groups). Group generation
    /// bumped once. Feasibility (`rebuild_sizes`) is the CALLER's job — this cannot fail.
    ///
    /// Headroom is a function of DEMAND, never of the group's own previous size. The previous
    /// form anchored the multiplier to `verts_bytes`, making it a recurrence on itself:
    /// `buffer(n+1) = max(need, 1.5 × buffer(n))` — measured 2.74× over-allocation ratchet on a
    /// real 1024³ drag (S0.3). Anchored to need, `verts_bytes ≤ 1.5 × Σ live regions` ALWAYS.
    #[allow(clippy::too_many_arguments)]
    fn rebuild_group(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue, gi: usize,
        key: SlotKey, want_verts: u64, want_idx: u64, index: u32,
        req_args: (u32, u32), need_v: u64, need_i: u64, new_vbytes: u64, new_ibytes: u64,
    ) {
        // New packed layout: every slot of THIS group (with the requester at its new caps).
        let mut order: Vec<SlotKey> = self.slots.iter()
            .filter(|(_, s)| s.group == gi).map(|(n, _)| *n).collect();
        if !self.slots.contains_key(&key) { order.push(key); }
        order.sort_unstable_by_key(|k| (k.0.0, k.1)); // deterministic packing (body, segment)
        let mut new_layout: Vec<(SlotKey, SdfPoolSlot)> = Vec::with_capacity(order.len());
        let (mut bv, mut bi) = (0u64, 0u64);
        for n in &order {
            let (vc, ic, idx_slot) = if *n == key {
                (want_verts, want_idx, self.slots.get(n).map_or(index, |s| s.index))
            } else {
                let s = &self.slots[n];
                (s.vert_cap, s.idx_cap, s.index)
            };
            // Args block + tile table are ENTRY-space state — untouched by a region repack (the
            // relative tile offsets stay valid; absolute bases are re-derived below).
            let (ab, ac, tiles) = match self.slots.get_mut(n) {
                Some(s) => (s.args_base, s.args_cap, std::mem::take(&mut s.tiles)),
                // Brand-new requester: its already-allocated block (NEVER a placeholder — a 0
                // base would alias body 0's entry and the reseed would clobber it).
                None => (req_args.0, req_args.1, Vec::new()),
            };
            let s = SdfPoolSlot {
                group: gi,
                vert_off: bv, vert_cap: vc, idx_off: bi, idx_cap: ic, index: idx_slot,
                args_base: ab, args_cap: ac, tiles,
            };
            bv += s.vert_region_bytes();
            bi += s.idx_region_bytes();
            new_layout.push((*n, s));
        }
        debug_assert!(bv <= new_vbytes && bi <= new_ibytes, "rebuild_sizes under-provisioned");
        // Phase 4c: acquire the new region buffers — RECYCLING a retired pair when one fits (skips
        // create + zero-init). `new_vb`/`new_ib` are the PHYSICAL sizes (≥ requested if recycled).
        let _t_create = std::time::Instant::now();
        let (new_verts, new_idx, new_vb, new_ib, recycled) =
            self.take_region_buffers(device, new_vbytes, new_ibytes);
        let create_ms = _t_create.elapsed().as_secs_f64() * 1000.0;

        // GPU-copy every surviving slot's region into the new buffers. Old→new is a cross-buffer
        // copy (wgpu forbids same-buffer copies — the reason a rebuild always allocates new
        // buffers, even for a pure repack). The indirect + instance META buffers are KEPT (slot
        // indices are stable; each entry's GPU-written index_count survives in place).
        let g = &self.groups[gi];
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sdf_pool_rebuild") });
        for (n, ns) in &new_layout {
            if *n == key { continue; } // requester re-dispatches; no stale content carried
            let os = &self.slots[n];
            enc.copy_buffer_to_buffer(&g.verts, os.vert_off, &new_verts, ns.vert_off,
                os.vert_region_bytes().min(ns.vert_region_bytes()));
            enc.copy_buffer_to_buffer(&g.idx, os.idx_off, &new_idx, ns.idx_off,
                os.idx_region_bytes().min(ns.idx_region_bytes()));
        }
        let _t_submit = std::time::Instant::now();
        queue.submit(std::iter::once(enc.finish()));
        let submit_ms = _t_submit.elapsed().as_secs_f64() * 1000.0;

        // Swap in the new buffers and RETIRE the old ones for reuse (their pending COPY_SRC reads
        // in the just-submitted encoder keep them alive; a later rebuild reuses them queue-ordered
        // AFTER this copy). `verts_bytes`/`idx_bytes` = the PHYSICAL sizes (recycled ⇒ may exceed
        // the request — the extra tail is free bump room).
        let (old_verts, old_idx, old_vb, old_ib) = {
            let g = &mut self.groups[gi];
            let ov = std::mem::replace(&mut g.verts, new_verts);
            let oi = std::mem::replace(&mut g.idx, new_idx);
            let (ovb, oib) = (g.verts_bytes, g.idx_bytes);
            g.verts_bytes = new_vb;
            g.idx_bytes = new_ib;
            g.bump_verts = need_v;
            g.bump_idx = need_i;
            (ov, oi, ovb, oib)
        };
        self.retired.retire(old_verts, old_vb, true);
        self.retired.retire(old_idx, old_ib, false);
        // Members of OTHER groups are untouched; replace only this group's slots.
        for (n, s) in new_layout {
            self.slots.insert(n, s);
        }
        self.bump_group_gen(gi);
        // Reseed the STATIC args of every member slot (all offsets in this group moved).
        // MONOLITHIC survivors keep their GPU index_count (byte 0 untouched — only bytes 4..20
        // rewritten); the requester gets a full reseed (index_count=0 — its re-dispatch writes
        // the real count right after).
        // TILED survivors: rewrite each tile entry's bytes 8..20 (first_index / base_vertex /
        // first_instance — the region moved) while PRESERVING index_count AND instance_count
        // (both GPU-owned truth), and refresh the TileInfo block (absolute bases moved). A tiled
        // REQUESTER is skipped entirely — its re-dispatch rewrites TileInfo + args + counters.
        let mono: Vec<(SlotKey, u64, [u32; 4])> = self.slots.iter()
            .filter(|(_, s)| s.group == gi && s.tiles.is_empty())
            .map(|(n, s)| {
                let first_instance = if self.multi_draw_ready { s.index } else { 0 };
                (*n, s.indirect_offset(),
                 [0u32, (s.idx_off / 4) as u32, (s.vert_off / 32) as u32, first_instance])
            }).collect();
        for (n, off, tail) in mono {
            if n == key {
                queue.write_buffer(&self.groups[gi].indirect, off, bytemuck::cast_slice(&[0u32]));
            }
            // bytes 4..20: instance_count(0) / first_index / base_vertex / first_instance.
            queue.write_buffer(&self.groups[gi].indirect, off + 4, bytemuck::cast_slice(&tail));
        }
        let tiled: Vec<SlotKey> = self.slots.iter()
            .filter(|(n, s)| s.group == gi && !s.tiles.is_empty() && **n != key)
            .map(|(n, _)| *n).collect();
        for n in tiled {
            self.write_tile_infos(queue, n.0, n.1);
            let s = &self.slots[&n];
            let first_instance = if self.multi_draw_ready { s.index } else { 0 };
            for (i, t) in s.tiles.iter().enumerate() {
                let (vb, ib) = s.tile_abs(t);
                let tail = [ib as u32, vb as u32, first_instance];
                queue.write_buffer(&self.groups[gi].indirect,
                    (s.args_base as u64 + i as u64) * SDF_POOL_INDIRECT_STRIDE + 8,
                    bytemuck::cast_slice(&tail));
            }
        }
        let n_slots = self.slots.values().filter(|s| s.group == gi).count();
        // Slice-D split of the rebuild's frame-thread cost: buffer acquisition (create OR recycle)
        // vs the region-copy submit. `recycled=N/2` names how many of the (verts, idx) pair reused a
        // retired buffer (skipping create + zero-init); 2/2 ⇒ this rebuild allocated nothing.
        eprintln!("[SdfPool] rebuild g{gi}: {} slots, verts {:.1}MB idx {:.1}MB (g_gen={} groups={}) \
                   | acquire={:.2}ms(recycled {}/2) copy_submit={:.2}ms held={}",
            n_slots, new_vb as f64 / 1.0e6, new_ib as f64 / 1.0e6,
            self.groups[gi].generation, self.groups.len(),
            create_ms, recycled, submit_ms, self.retired.len());
    }

    /// Grow the INSTANCE buffer to hold `want_slots` bodies (instances are rewritten per drawn
    /// frame — no copy needed). Pool generation bumped (the buffer identity changed). The
    /// per-group args-entry buffers grow independently via `ensure_args_capacity`.
    fn ensure_meta_capacity(&mut self, device: &wgpu::Device, want_slots: u32) {
        if want_slots <= self.slots_cap { return; }
        self.instances = Self::make_instance_buffer(device, want_slots);
        self.slots_cap = want_slots;
        self.generation += 1;
    }

    /// R3b twin of `SdfBodyMeshSlot::grow_output_caps` for a POOLED slot: ×1.5 from the current
    /// region caps, never below 1.25× the TRUE counter-read vertex count, clamped by
    /// `dense_ceiling` (vertex-count form — sparse must never exceed the dense worst case).
    /// Returns false when already at the ceiling / cannot fit (mesh keeps the safe truncation).
    pub(crate) fn grow_slot(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue,
        nid: NodeId, need_verts: u64, dense_ceiling_verts: u64, dense_ceiling_idx: u64,
    ) -> bool {
        let Some(s) = self.slots.get(&(nid, 0)) else { return false; };
        let before = (s.vert_cap, s.idx_cap);
        let want_v = (need_verts * 5 / 4).max(s.vert_cap * 3 / 2).max(64).min(dense_ceiling_verts);
        let want_i = (want_v * 6).min(dense_ceiling_idx);
        if want_v <= before.0 && want_i <= before.1 { return false; }
        match self.ensure_slot(device, queue, nid, want_v.max(before.0), want_i.max(before.1)) {
            Ok(_) => {
                let s = &self.slots[&(nid, 0)];
                (s.vert_cap, s.idx_cap) != before
            }
            Err(()) => false,
        }
    }

    /// SETTLE-TIME TRIM (slice b, 2026-07-16 live attribution): the inverse of `grow_slot`, run
    /// only in a QUIET window (no recent escalation — the caller decides via `slot.may_trim`).
    /// Transient mid-gesture brick peaks push the grow-only region caps to ~2× the settled need
    /// and PIN that VRAM forever. When the current need is under HALF the region caps, rebuild
    /// the body's GROUP with this slot at exactly `want` — the hysteresis deadband (shrink at <½,
    /// land at the caller's 1.25×need) means a later shrink needs a further −50% and a regrow a
    /// +60% rise, so a stable surface cannot ping-pong. The requester's region content is NOT
    /// carried (exactly like a grow relocate) — the caller re-dispatches the mesh immediately.
    /// Other slots' content IS preserved (rebuild GPU-copies); OTHER GROUPS are untouched.
    /// Returns true when a trim actually happened (caller updates slot caps + generation).
    pub(crate) fn trim_slot(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue,
        nid: NodeId, want_verts: u64, want_idx: u64,
    ) -> bool {
        let key: SlotKey = (nid, 0);
        let Some(s) = self.slots.get(&key) else { return false; };
        if want_verts == 0 || s.vert_cap < want_verts.saturating_mul(2) { return false; }
        let want_idx = want_idx.max(1);
        let (gi, index, before_v) = (s.group, s.index, s.vert_cap);
        let req_args = (s.args_base, s.args_cap);
        let safe_max = self.safe_max(device);
        let (need_v, need_i) = self.group_need(gi, key, want_verts, want_idx);
        // Shrinking below the current footprint cannot really fail; keep the old region if the
        // sizing somehow doesn't fit (paranoia, not a path).
        let Some((nv, ni)) = self.rebuild_sizes(gi, need_v, need_i, safe_max) else { return false; };
        self.rebuild_group(device, queue, gi, key, want_verts, want_idx, index,
                           req_args, need_v, need_i, nv, ni);
        // Settle = the gesture ended: release the recycle free-list (the just-retired big buffers
        // from this trim + any lingering from the gesture's grows). `may_trim` fires only when the
        // body is sparse-quiet, so no imminent rebuild needs them; holding GiB-class buffers past
        // settle is exactly the VRAM pressure the trim exists to relieve.
        self.retired.drain();
        eprintln!("[SdfPool] trim body={nid:?} (g{gi}): vert region {} → {} ({}MB freed at settle)",
            before_v, want_verts, ((before_v - want_verts) * 32) >> 20);
        true
    }

    /// Free the slot of a deleted body: zero its args BLOCK in its group (instance_count=0 AND
    /// index_count=0 on every entry — nothing can draw it even via a stale offset) and recycle
    /// its instance index + args block. Its region becomes a hole reclaimed by that group's next
    /// rebuild. Mirrors the voxel pool's free-list.
    pub(crate) fn free_slot(&mut self, queue: &wgpu::Queue, nid: NodeId) {
        let freed_any = self.slots.contains_key(&(nid, 0));
        let mut seg = 0u32;
        while let Some(s) = self.slots.remove(&(nid, seg)) {
            queue.write_buffer(&self.groups[s.group].indirect, s.indirect_offset(),
                &vec![0u8; (s.args_cap as u64 * SDF_POOL_INDIRECT_STRIDE) as usize]);
            if seg == 0 { self.free_indices.push(s.index); } // instance index is body-shared
            self.groups[s.group].args_free.push((s.args_base, s.args_cap));
            seg += 1;
        }
        if freed_any { self.frame_drawn.retain(|n| *n != nid); }
    }

    /// Phase 4b — convert body `nid` to a MONOLITHIC pooled slot: retire every higher segment
    /// and, if segment 0 still carries a TILED table, drop it, zero its whole args block (stale
    /// tile entries must never draw) and reseed the single monolithic entry. Called by the
    /// monolithic dispatch arm when a body falls back from the tiled path — without this, the
    /// per-body multi-draw would still cover `tiles.len()` stale entries.
    pub(crate) fn make_monolithic(&mut self, queue: &wgpu::Queue, nid: NodeId) {
        if self.slots.contains_key(&(nid, 1)) {
            self.free_segments_from(queue, nid, 1);
        }
        let had_tiles = self.slots.get(&(nid, 0)).map_or(false, |s| !s.tiles.is_empty());
        if had_tiles {
            let s = self.slots.get_mut(&(nid, 0)).expect("checked above");
            s.tiles = Vec::new();
            let (gi, off, cap) = (s.group, s.indirect_offset(), s.args_cap);
            queue.write_buffer(&self.groups[gi].indirect, off,
                &vec![0u8; (cap as u64 * SDF_POOL_INDIRECT_STRIDE) as usize]);
            let s = &self.slots[&(nid, 0)];
            self.seed_indirect(queue, s);
        }
    }

    /// Phase 4b — free body `nid`'s segments `from_seg..` (a re-dispatch whose new partitioning
    /// needs FEWER segments must retire the stale tail: zero each args block so nothing can draw,
    /// recycle the block; regions become holes reclaimed by their groups' next rebuilds).
    /// Segment indices stay dense from 0. Instance index untouched (owned by segment 0).
    pub(crate) fn free_segments_from(&mut self, queue: &wgpu::Queue, nid: NodeId, from_seg: u32) {
        debug_assert!(from_seg > 0, "use free_slot to free the whole body");
        let mut seg = from_seg.max(1);
        while let Some(s) = self.slots.remove(&(nid, seg)) {
            queue.write_buffer(&self.groups[s.group].indirect, s.indirect_offset(),
                &vec![0u8; (s.args_cap as u64 * SDF_POOL_INDIRECT_STRIDE) as usize]);
            self.groups[s.group].args_free.push((s.args_base, s.args_cap));
            seg += 1;
        }
    }

    /// SCENE-prune companion (dirty.rs): free every slot whose body no longer passes `keep`.
    pub(crate) fn retain_bodies(&mut self, queue: &wgpu::Queue, mut keep: impl FnMut(&NodeId) -> bool) {
        let mut dead: Vec<NodeId> = self.slots.keys()
            .filter(|k| !keep(&k.0)).map(|k| k.0).collect();
        dead.sort_unstable_by_key(|n| n.0);
        dead.dedup();
        for n in dead { self.free_slot(queue, n); }
    }

    // ── per-frame draw prep (render phase — write_buffer only, NO resource creation) ────────────

    /// Reset the per-pass draw list + zero the instance_count of every MONOLITHIC slot's entry
    /// (in its group) — called once at the start of each pass's draw prep, then `mark_drawn`
    /// re-enables exactly the bodies that pass this frame's filters (visibility / isolation /
    /// top-level / renderable-content). A body hidden this frame, or one whose current mesh went
    /// dense (`slot.pooled == false`), is never listed → its args block is never multi-drawn.
    /// Tiled entries keep instance_count=1 (GPU-written by dc_tile_args) — their visibility IS
    /// the draw list (per-entry zeroing cannot scale to thousands of tiles per frame).
    pub(crate) fn clear_frame_visibility(&mut self, queue: &wgpu::Queue) {
        self.frame_drawn.clear();
        for s in self.slots.values().filter(|s| s.tiles.is_empty()) {
            queue.write_buffer(&self.groups[s.group].indirect, s.indirect_offset() + 4,
                bytemuck::cast_slice(&[0u32]));
        }
    }

    /// Mark body `nid` drawn this pass: write its InstanceData into the shared instance buffer at
    /// its slot index, set the (monolithic) entry's instance_count=1, and list it for the
    /// per-body multi-draw. The G-buffer pass writes render instances; the picking pass writes
    /// picking instances (node id in material_idx) — same slots, same geometry, LOCKSTEP by
    /// construction (P0 selection rule).
    pub(crate) fn mark_drawn(&mut self, queue: &wgpu::Queue, nid: NodeId, inst: &render::mesh_gpu::InstanceData) {
        let Some(s) = self.slots.get(&(nid, 0)) else { return; };
        queue.write_buffer(&self.instances, s.index as u64 * SDF_POOL_INSTANCE_STRIDE, bytemuck::bytes_of(inst));
        if s.tiles.is_empty() {
            queue.write_buffer(&self.groups[s.group].indirect, s.indirect_offset() + 4,
                bytemuck::cast_slice(&[1u32]));
        }
        if !self.frame_drawn.contains(&nid) { self.frame_drawn.push(nid); }
    }
}
