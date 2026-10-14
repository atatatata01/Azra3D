//! Phase 3A slice 1 — `SdfBrickAtlasGpu`: PERSISTENT, generation-counted GPU storage for the
//! sparse SDF brick atlas (INDEX tags + POOL arena) read by the sparse DC mesher
//! (`sdf_mesh.wgsl` bindings 6/7, switched on by `brick_res` packed in `DcParams.gext.w`).
//!
//! Replaces the per-bake transient index/pool buffers (the "FIRST CUT" in
//! `app::sdf_gpu_mesh::SdfMeshGpu::mesh_into_slot_sparse`) with ONE GpuCtx-owned pair: a sparse
//! re-bake re-writes (`write_buffer`) instead of re-allocating a multi-hundred-MB pool per frame.
//!
//! Data shape matches `core_math::chunked_dc::SparseSdfBricks` (raw slices are passed in so this
//! type stays a dumb GPU mirror):
//! - `index`: `dims_bricks.x·y·z` u32 tags — `0xFFFF_FFFF` = EXTERIOR (decode → +far),
//!   `0xFFFF_FFFE` = INTERIOR (decode → −far), anything else = dense-packed pool handle.
//! - `pool`: handle-indexed arena of 8³ = 512-f32 bricks (2048 B each).
//!
//! Capacity follows the GROW-ONLY watermark pattern (`SdfBodyMeshSlot.res`/`capacity_res`,
//! `VoxelAtlasGpu.pool_cap`): content smaller than capacity re-uses the buffers. Buffer tails
//! beyond the logical extent are never read — the shader indexes the index by brick coordinate
//! `< dims_bricks` and the pool by handle `< allocated bricks`.
//!
//! FRAME ORDERING CONTRACT: `new()` and `ensure_capacity()` create GPU buffers — call ONLY from
//! `poll_background_tasks()` (never panels / input handlers / draw). The single production caller
//! is the sparse pre-pass in `app_poll::sdf_populate_resident`.
//!
//! This is a CLEAN new type for the SDF mesh path — it does NOT revive the parked `brick_pool.rs`
//! refinement layer; only the index/pool data-shape convention is shared.

use core_math::scene_graph::NodeId;

/// f32 count of one 8³ SDF brick (`classify_grid_to_sparse` is called with `brick_dim = 8`).
pub const BRICK_FLOATS: u32 = 512;
/// Voxels per brick per axis (`BRICK_FLOATS = BRICK_FLOATS_DIM³`). Mirrors the app-side
/// `SPARSE_BRICK_DIM`; used to map a handle-brick bbox to the DC cell window.
pub const BRICK_FLOATS_DIM: u32 = 8;

/// Grow quantum in bricks / index entries (4096 bricks × 2048 B = 8 MiB of pool). Growth lands on
/// coarse steps so a slowly-expanding surface doesn't realloc (and generation-bump) every bake.
const GROW_QUANTUM: u32 = 4096;

/// Persistent GPU brick atlas for the sparse SDF mesh path. Owned by `GpuCtx`
/// (`gpu.sdf_brick_atlas`), created lazily on the first sparse mesh dispatch.
pub struct SdfBrickAtlasGpu {
    /// STORAGE | COPY_DST — `dims_bricks.x·y·z` u32 tags (binding 6 of the DC mesher).
    pub index_buf: wgpu::Buffer,
    /// STORAGE | COPY_DST — `pool_capacity_bricks × 512` f32 brick arena (binding 7).
    pub pool_buf: wgpu::Buffer,
    /// Capacity of `index_buf` in u32 entries (bricks). Grow-only watermark.
    pub index_capacity: u32,
    /// Capacity of `pool_buf` in bricks (each `BRICK_FLOATS` f32 = 2048 B). Grow-only watermark.
    pub pool_capacity_bricks: u32,
    /// Bricks per axis of the CURRENT content (from `SparseSdfBricks.dims_bricks`).
    /// `dims_bricks[0]` is the `brick_res` packed into `DcParams.gext.w`.
    pub dims_bricks: [u32; 3],
    /// Voxel resolution of the CURRENT content — carried EXPLICITLY, NEVER derived from a buffer
    /// length (the kill-the-cbrt rule).
    pub res: u32,
    /// Handled (pool-backed) brick count of the CURRENT content — the classifier's `allocated`.
    /// Content metadata like `res`/`dims_bricks` (stamped by `upload`/`upload_index`), used by the
    /// mesh-dispatch log; the shader only ever reads bricks through the index tags.
    pub allocated_bricks: u32,
    /// Body identity of the CURRENT content. Mirrors `gpu_volume::ResidentVolume.body_nid`
    /// (`NodeId` is a `u64` newtype). `None` until the first `upload`.
    pub body_nid: Option<NodeId>,
    /// Bumped on ANY buffer (re)creation (`ensure_capacity` grow). Consumers that cache anything
    /// referencing `index_buf`/`pool_buf` MUST compare against their cached generation and rebuild
    /// on mismatch — generation counters over timing tricks (CLAUDE.md, GPU Resource Lifetime;
    /// same pattern as `VolumeAtlas.texture_generation` / `AtlasSlot.generation`).
    pub generation: u64,
    /// INCLUSIVE brick-coordinate bounding box of the CURRENT content's HANDLE (pool-backed)
    /// bricks — i.e. the surface shell + its apron. The iso=0 surface lives ENTIRELY inside
    /// these bricks (interior bricks are deep-solid past the classifier margin, exterior bricks
    /// empty — neither straddles), so DC only needs to run over this box, not the whole grid. On
    /// an escape-grown grid a small body occupies a fraction of the volume, so this turns the
    /// per-drag-frame full 511³ DC sweep (the owner's "laggy on edit") into a surface-sized
    /// dispatch. `min > max` (the `[u32::MAX;3] / [0;3]` empty sentinel) means NO surface — the
    /// mesher skips the dispatch (empty body). Stamped by `upload` / `upload_index`.
    pub surface_brick_min: [u32; 3],
    pub surface_brick_max: [u32; 3],
    /// Tiled DC (C1): per-surface-tile occupancy of the CURRENT content (see `SdfTileStat`).
    /// Stamped by `upload`/`upload_index` from the same CPU index as the surface bbox.
    pub tile_stats: Vec<SdfTileStat>,
}

/// Tiled DC (C1): bricks per tile axis — a tile is 4³ bricks = 32³ cells, the §R2-ratified
/// granularity (per-brick bookkeeping exceeds the win; per-body is the monolith being replaced).
pub const SDF_TILE_BRICKS: u32 = 4;

/// Per-SURFACE-tile occupancy of the CURRENT atlas content (Tiled DC C1). One entry per tile of
/// the tile grid (`ceil(dims_bricks / 4)` per axis) that contains ≥1 HANDLE brick, sorted by tile
/// linear index (deterministic packing). Stamped by `upload`/`upload_index` alongside the surface
/// bbox; consumed by the tiled mesher to build per-tile windows + output caps, and by C2 to map a
/// dirty window to the tiles it overlaps.
#[derive(Clone, Copy, Debug)]
pub struct SdfTileStat {
    /// Tile coordinate in the tile grid (= handle brick coord >> 2).
    pub tile: [u32; 3],
    /// Handle (pool-backed) bricks inside this tile (1..=64).
    pub bricks: u32,
    /// Handle bricks whose cells can produce vertices in THIS tile's vertex region: own bricks
    /// plus the −1-cell halo layer's bricks (bricks at `coord % 4 == 3` of the lower neighbor
    /// tile). Drives the per-tile vertex cap (`halo_bricks × VERTS_PER_BRICK_HEADROOM`) — the own
    /// count alone under-sizes edge tiles whose halo cells cross into neighbor bricks.
    pub halo_bricks: u32,
    /// Inclusive HANDLE-brick bbox inside this tile, in GLOBAL brick coordinates — the tile's DC
    /// window is these bricks' cell range (a sparse tile dispatches less than its full 32³ cells).
    pub bmin: [u32; 3],
    pub bmax: [u32; 3],
}

/// Slice D (2026-07-16) — FUSED single-pass index content scan: `surface_brick_bbox` +
/// `tile_occupancy` in ONE walk over the index, with incremental coordinates (no per-entry
/// div/mod). This is the PRODUCTION scan: the two reference functions below cost ~55-70ms
/// combined at 1024³ (2M entries, three full passes, opt-level 0 dev profile — the owner's
/// measured "54ms sparse atlas upload" was ~87% these scans, only ~1ms the 8.4MB write_buffer);
/// the fused pass does the same work in one pass at a fraction of the cost.
///
/// EQUIVALENCE (gate: `fused_scan_matches_reference_oracles`):
/// - bbox: identical min/max over handle bricks.
/// - `bricks`/`bmin`/`bmax`: identical (pass-1 logic verbatim).
/// - `halo_bricks`: the reference pass 2 adds a boundary brick's contribution only to LIVE
///   neighbor tiles — but liveness there is FINAL liveness (pass 1 has completed), and dead
///   tiles are dropped at collect anyway, so accumulating halo contributions UNCONDITIONALLY
///   produces identical values on every live tile. That removes the second full pass.
/// PARALLELISM: the scan is embarrassingly parallel over TILE-Z LAYERS (4 brick planes each) —
/// the only cross-layer coupling is a boundary brick's z-halo contribution into the NEXT layer,
/// which each layer absorbs by pre-scanning the ONE brick plane below it (read-only). Layers
/// merge in tz order, so the output is deterministic and identical to the sequential walk.
pub fn index_content_scan(
    index: &[u32], dims_bricks: [u32; 3],
) -> ([u32; 3], [u32; 3], Vec<SdfTileStat>) {
    use rayon::prelude::*;
    debug_assert_eq!(SDF_TILE_BRICKS, 4, "tile shift math below assumes SDF_TILE_BRICKS == 4");
    let (dx, dy, dz) = (dims_bricks[0] as usize, dims_bricks[1] as usize, dims_bricks[2] as usize);
    let mut min = [u32::MAX; 3];
    let mut max = [0u32; 3];
    let (tx, ty, tzc) = (dx.div_ceil(4), dy.div_ceil(4), dz.div_ceil(4));
    if tx * ty * tzc == 0 || index.is_empty() {
        return (min, max, Vec::new());
    }
    debug_assert_eq!(index.len(), dx * dy * dz);
    let txty = tx * ty;
    let plane = dx * dy;

    // Per-layer scan: (layer bbox min, max, layer-local tile stats [txty]).
    let layers: Vec<([u32; 3], [u32; 3], Vec<SdfTileStat>)> = (0..tzc).into_par_iter().map(|tz| {
        let mut lmin = [u32::MAX; 3];
        let mut lmax = [0u32; 3];
        let mut stats: Vec<SdfTileStat> = vec![SdfTileStat {
            tile: [0; 3], bricks: 0, halo_bricks: 0, bmin: [u32::MAX; 3], bmax: [0; 3],
        }; txty];
        // ── Boundary pre-pass: the brick plane BELOW this layer (bz = 4·tz − 1, always a
        // `%4==3` plane) feeds this layer's tiles through the z-halo. Read-only; weights
        // per the EQUIVALENCE note below (z is a hi axis for every brick of this plane).
        if tz > 0 {
            let bz = tz * 4 - 1;
            let row0 = bz * plane;
            let mut i = row0;
            for by in 0..dy {
                let ty_base = (by >> 2) * tx;
                let y_hi = (by & 3) == 3 && by + 1 < dy;
                for bx in 0..dx {
                    let tag = index[i];
                    i += 1;
                    if tag >= 0xFFFF_FFFEu32 { continue; }
                    let x_hi = (bx & 3) == 3 && bx + 1 < dx;
                    let w = 1u32 << ((!x_hi as u32) + (!y_hi as u32)); // z IS hi here
                    let tl = ty_base + (bx >> 2);
                    stats[tl].halo_bricks += w;                              // S = {z}
                    if x_hi { stats[tl + 1].halo_bricks += w; }              // S = {x,z}
                    if y_hi { stats[tl + tx].halo_bricks += w; }             // S = {y,z}
                    if x_hi && y_hi { stats[tl + 1 + tx].halo_bricks += w; } // S = {x,y,z}
                }
            }
        }
        // ── Main pass: this layer's own brick planes. z-halo combos are OMITTED here — they
        // belong to the NEXT layer, whose boundary pre-pass emits them (z_hi still enters the
        // WEIGHT: the reference iterates the duplicate `[c, c]` entry of every non-hi axis, so
        // each contribution is counted 2^(non-hi axes) times — bug-compatible on purpose;
        // `halo_bricks` only drives conservative caps, and the C1/C2 headroom constants +
        // envelope gates were calibrated against these exact values. Do NOT "fix" the weight
        // without recalibrating TILE_VERTS_PER_BRICK_HEADROOM and the envelope gate.
        // Gate: `fused_scan_matches_reference_oracles`.)
        for bz in tz * 4..(tz * 4 + 4).min(dz) {
            let z_hi = (bz & 3) == 3 && bz + 1 < dz;
            let mut i = bz * plane;
            for by in 0..dy {
                let ty_base = (by >> 2) * tx;
                let y_hi = (by & 3) == 3 && by + 1 < dy;
                for bx in 0..dx {
                    let tag = index[i];
                    i += 1;
                    if tag >= 0xFFFF_FFFEu32 {
                        continue; // EXTERIOR / INTERIOR sentinel — not a handle brick
                    }
                    let b = [bx as u32, by as u32, bz as u32];
                    for a in 0..3 {
                        lmin[a] = lmin[a].min(b[a]);
                        lmax[a] = lmax[a].max(b[a]);
                    }
                    let tl = ty_base + (bx >> 2);
                    {
                        let s = &mut stats[tl];
                        s.bricks += 1;
                        s.halo_bricks += 1;
                        for a in 0..3 {
                            s.bmin[a] = s.bmin[a].min(b[a]);
                            s.bmax[a] = s.bmax[a].max(b[a]);
                        }
                    }
                    let x_hi = (bx & 3) == 3 && bx + 1 < dx;
                    let w = 1u32 << ((!x_hi as u32) + (!y_hi as u32) + (!z_hi as u32));
                    if x_hi { stats[tl + 1].halo_bricks += w; }              // S = {x}
                    if y_hi { stats[tl + tx].halo_bricks += w; }             // S = {y}
                    if x_hi && y_hi { stats[tl + 1 + tx].halo_bricks += w; } // S = {x,y}
                }
            }
        }
        (lmin, lmax, stats)
    }).collect();

    // ── Merge (tz order = brick-linear order → deterministic, identical to the sequential walk).
    let mut out = Vec::new();
    for (tz, (lmin, lmax, stats)) in layers.into_iter().enumerate() {
        for a in 0..3 {
            min[a] = min[a].min(lmin[a]);
            max[a] = max[a].max(lmax[a]);
        }
        for (idx, mut s) in stats.into_iter().enumerate() {
            if s.bricks == 0 { continue; }
            s.tile = [(idx % tx) as u32, (idx / tx) as u32, tz as u32];
            out.push(s);
        }
    }
    (min, max, out)
}

/// REFERENCE ORACLE (kept for the `fused_scan_matches_reference_oracles` gate — production uses
/// `index_content_scan`). Per-tile handle-brick occupancy (see `SdfTileStat`). Two linear passes over `index`:
/// pass 1 accumulates each live tile's own count + bbox; pass 2 adds the halo contributions
/// (a brick at `coord % 4 == 3` on an axis also feeds the +1 neighbor tile's vertex halo) to
/// tiles that are LIVE (a halo contribution to a surface-less tile is meaningless — that tile
/// has no window). ~1-2ms at 1024³ (2M entries), only on index change — beside the existing
/// `surface_brick_bbox` scan.
pub fn tile_occupancy(index: &[u32], dims_bricks: [u32; 3]) -> Vec<SdfTileStat> {
    let t = SDF_TILE_BRICKS;
    let (dx, dy, dz) = (dims_bricks[0] as usize, dims_bricks[1] as usize, dims_bricks[2] as usize);
    let (tx, ty, tz) = (dx.div_ceil(t as usize), dy.div_ceil(t as usize), dz.div_ceil(t as usize));
    let n_tiles = tx * ty * tz;
    if n_tiles == 0 { return Vec::new(); }
    // Dense scratch (≤32³ = 32768 tiles at 1024³) — u32::MAX bricks marks "not live".
    let mut stats: Vec<SdfTileStat> = vec![SdfTileStat {
        tile: [0; 3], bricks: 0, halo_bricks: 0, bmin: [u32::MAX; 3], bmax: [0; 3],
    }; n_tiles];
    let tlin = |bx: usize, by: usize, bz: usize| -> usize {
        (bx / t as usize) + (by / t as usize) * tx + (bz / t as usize) * tx * ty
    };
    // Pass 1: own bricks + bbox.
    for (bi, &tag) in index.iter().enumerate() {
        if tag >= 0xFFFF_FFFEu32 { continue; }
        let bx = bi % dx;
        let by = (bi / dx) % dy;
        let bz = bi / (dx * dy);
        let s = &mut stats[tlin(bx, by, bz)];
        s.bricks += 1;
        s.halo_bricks += 1;
        let b = [bx as u32, by as u32, bz as u32];
        for a in 0..3 {
            s.bmin[a] = s.bmin[a].min(b[a]);
            s.bmax[a] = s.bmax[a].max(b[a]);
        }
    }
    // Pass 2: halo contributions to LIVE neighbor tiles (+1 on each axis where coord % 4 == 3).
    for (bi, &tag) in index.iter().enumerate() {
        if tag >= 0xFFFF_FFFEu32 { continue; }
        let bx = bi % dx;
        let by = (bi / dx) % dy;
        let bz = bi / (dx * dy);
        let step = |c: usize, dim: usize| -> [usize; 2] {
            if c % t as usize == t as usize - 1 && c + 1 < dim { [c, c + 1] } else { [c, c] }
        };
        for nx in step(bx, dx) { for ny in step(by, dy) { for nz in step(bz, dz) {
            if (nx, ny, nz) == (bx, by, bz) { continue; }
            let tl = tlin(nx, ny, nz);
            if tl == tlin(bx, by, bz) { continue; }
            if stats[tl].bricks > 0 {
                stats[tl].halo_bricks += 1;
            }
        }}}
    }
    // Collect live tiles in linear order (deterministic).
    let mut out = Vec::new();
    for (i, mut s) in stats.into_iter().enumerate() {
        if s.bricks == 0 { continue; }
        s.tile = [(i % tx) as u32, ((i / tx) % ty) as u32, (i / (tx * ty)) as u32];
        out.push(s);
    }
    out
}

/// REFERENCE ORACLE (kept for the `fused_scan_matches_reference_oracles` gate — production uses
/// `index_content_scan`). Inclusive brick bbox of the pool-backed (handle) bricks in `index`. `min=[u32::MAX;3]` /
/// `max=[0;3]` when there are none (empty surface). One linear pass — cheap vs the DC sweep it
/// windows (~0.3ms at 512³, only on index change).
pub fn surface_brick_bbox(index: &[u32], dims_bricks: [u32; 3]) -> ([u32; 3], [u32; 3]) {
    let (dx, dy) = (dims_bricks[0] as usize, dims_bricks[1] as usize);
    let mut min = [u32::MAX; 3];
    let mut max = [0u32; 3];
    for (bi, &tag) in index.iter().enumerate() {
        if tag < 0xFFFF_FFFEu32 {
            // handle brick
            let bx = (bi % dx) as u32;
            let by = ((bi / dx) % dy) as u32;
            let bz = (bi / (dx * dy)) as u32;
            min[0] = min[0].min(bx); min[1] = min[1].min(by); min[2] = min[2].min(bz);
            max[0] = max[0].max(bx); max[1] = max[1].max(by); max[2] = max[2].max(bz);
        }
    }
    (min, max)
}

#[inline]
fn round_up_quantum(n: u32) -> u32 {
    n.div_ceil(GROW_QUANTUM).max(1) * GROW_QUANTUM
}

/// 2048-SHIP item 3 (2026-07-17) — the hard brick ceiling ONE pool buffer can hold and still be
/// bound as a single entire storage binding (bake binding 2 / mesher binding 7). At 2048³ the
/// measured gesture churn (1024's 2.3× mid-drag surface growth, scaled) crosses
/// `max_storage_buffer_binding_size` — every sizing decision (the dispatch-side self-heal AND
/// the ×1.25 grow pad below) must share THIS one ceiling, or the pad can cross the limit that
/// the exact `allocated` check just passed (device-loss class: validation error → invalid
/// encoder cascade). Pure — unit-tested without a device.
pub fn pool_ceiling_bricks(max_storage_binding: u64) -> u32 {
    (max_storage_binding / (BRICK_FLOATS as u64 * 4)).min(u32::MAX as u64) as u32
}

/// Grow-pad policy with the binding-limit clamp: `need × 1.25` rounded up to the grow quantum,
/// clamped so `capacity × elem_bytes` never exceeds the binding limit (`ceiling` in ELEMENTS),
/// and never below `needed` (callers gate `needed ≤ ceiling` BEFORE growing — the dispatch
/// self-heal drops resolution rather than requesting an unbindable buffer). Refusing the PAD is
/// the graceful degradation: the atlas still fits the actual content; only the churn headroom
/// shrinks as the ceiling nears. Pure — unit-tested at the boundary.
pub fn padded_capacity_clamped(needed: u32, ceiling: u32) -> u32 {
    debug_assert!(needed <= ceiling,
        "atlas capacity request ({needed}) exceeds the binding-limit ceiling ({ceiling}) — \
         the dispatch-side self-heal (resolution drop) must refuse this BEFORE the grow");
    round_up_quantum(needed.saturating_add(needed / 4)).min(ceiling).max(needed)
}

fn create_index_buf(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sdf_brick_atlas_index"),
        size: (capacity as u64 * 4).max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_pool_buf(device: &wgpu::Device, capacity_bricks: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sdf_brick_atlas_pool"),
        size: (capacity_bricks as u64 * BRICK_FLOATS as u64 * 4).max(4),
        // COPY_SRC: pool GROWTH preserves existing brick content via a GPU→GPU copy into the
        // replacement buffer (S2.5c incremental allocation grows the pool MID-DRAG, with only a
        // windowed partial bake after — without the copy every previously-baked brick would
        // decode as garbage).
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

impl SdfBrickAtlasGpu {
    /// VRAM footprint of the CAPACITY (watermark) buffers — what this atlas actually holds on
    /// the device, independent of current content. Drives the multi-tenancy LRU budget
    /// (`GpuCtx.sdf_brick_atlases` — P2-B5).
    pub fn vram_bytes(&self) -> u64 {
        self.index_capacity as u64 * 4 + self.pool_capacity_bricks as u64 * BRICK_FLOATS as u64 * 4
    }

    /// Allocate the atlas with initial capacities (rounded up to the grow quantum). Content fields
    /// (`dims_bricks`/`res`/`body_nid`) stay empty until the first `upload`.
    /// POLL CONTEXT ONLY (Frame Ordering Contract — see module docs).
    pub fn new(device: &wgpu::Device, index_capacity: u32, pool_capacity_bricks: u32) -> Self {
        // Quantum rounding must not cross the binding limit either (a request within one grow
        // quantum of the ceiling) — same clamp policy as `ensure_capacity`'s pad.
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        let index_ceiling = (max_bind / 4).min(u32::MAX as u64) as u32; // u32 elements
        let index_capacity = round_up_quantum(index_capacity).min(index_ceiling.max(index_capacity));
        let pool_capacity_bricks = round_up_quantum(pool_capacity_bricks)
            .min(pool_ceiling_bricks(max_bind).max(pool_capacity_bricks));
        Self {
            index_buf: create_index_buf(device, index_capacity),
            pool_buf: create_pool_buf(device, pool_capacity_bricks),
            index_capacity,
            pool_capacity_bricks,
            dims_bricks: [0; 3],
            res: 0,
            allocated_bricks: 0,
            body_nid: None,
            generation: 0,
            surface_brick_min: [u32::MAX; 3],
            surface_brick_max: [0; 3],
            tile_stats: Vec::new(),
        }
    }

    /// GROW-ONLY capacity watermark: if either buffer is too small for the requested content, it
    /// is recreated at `need × 1.25` rounded up to the grow quantum and `generation` is bumped
    /// (once, even if both grow) so any consumer caching a bind group over the OLD buffer detects
    /// the swap and rebuilds. Sufficient capacity is a strict no-op — never shrinks, never bumps.
    ///
    /// POOL GROWTH PRESERVES CONTENT: the old pool's bricks are GPU→GPU copied into the
    /// replacement buffer (one encoder + submit) BEFORE the swap. S2.5c's incremental allocation
    /// grows the pool mid-drag and follows with only a WINDOWED partial bake — without this copy,
    /// every brick outside the window would decode as garbage. The index buffer is NOT copied on
    /// growth: every path that grows it re-uploads the full index immediately after
    /// (`upload`/`upload_index`), so preservation there would be dead work.
    /// POLL CONTEXT ONLY (Frame Ordering Contract).
    pub fn ensure_capacity(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue,
        index_needed: u32, pool_bricks_needed: u32,
    ) {
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        let mut grew = false;
        if index_needed > self.index_capacity {
            let index_ceiling = (max_bind / 4).min(u32::MAX as u64) as u32; // u32 elements
            self.index_capacity = padded_capacity_clamped(index_needed, index_ceiling);
            self.index_buf = create_index_buf(device, self.index_capacity);
            grew = true;
        }
        if pool_bricks_needed > self.pool_capacity_bricks {
            let old_pool_bytes = self.pool_capacity_bricks as u64 * BRICK_FLOATS as u64 * 4;
            // 2048-SHIP item 3: the ×1.25 pad + quantum rounding are CLAMPED at the binding
            // ceiling — at 723k bricks (realistic 2048) the pad alone is +353MB and the old
            // unclamped grow could hand the driver an unbindable pool right after the exact
            // `allocated` check passed. Needing MORE than the ceiling is refused upstream
            // (dispatch self-heal drops the resolution); here only the headroom degrades.
            self.pool_capacity_bricks = padded_capacity_clamped(
                pool_bricks_needed, pool_ceiling_bricks(max_bind));
            let new_pool = create_pool_buf(device, self.pool_capacity_bricks);
            if self.allocated_bricks > 0 {
                // Preserve every previously-baked brick (the whole old capacity — cheap GPU DMA,
                // growth is quantum-coarse and rare).
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("sdf_brick_atlas_pool_grow_copy"),
                });
                enc.copy_buffer_to_buffer(&self.pool_buf, 0, &new_pool, 0, old_pool_bytes.max(4));
                queue.submit(std::iter::once(enc.finish()));
            }
            self.pool_buf = new_pool;
            grew = true;
        }
        if grew {
            self.generation += 1;
        }
    }

    /// Write one classified body's content (`SparseSdfBricks.index`/`.pool` as raw slices) into the
    /// persistent buffers and stamp the content identity (`dims_bricks`, explicit `res`, owning
    /// body). Capacities must already fit — call `ensure_capacity` first; asserted, never silently
    /// truncated (a truncated pool would mesh garbage).
    pub fn upload(
        &mut self, queue: &wgpu::Queue,
        index: &[u32], pool: &[f32], dims_bricks: [u32; 3], res: u32, body_nid: NodeId,
    ) {
        assert_eq!(
            index.len() as u64,
            dims_bricks[0] as u64 * dims_bricks[1] as u64 * dims_bricks[2] as u64,
            "brick index length must equal dims_bricks product"
        );
        assert_eq!(pool.len() % BRICK_FLOATS as usize, 0, "pool must be whole 512-f32 bricks");
        let pool_bricks = (pool.len() / BRICK_FLOATS as usize) as u32;
        assert!(
            index.len() as u32 <= self.index_capacity && pool_bricks <= self.pool_capacity_bricks,
            "SdfBrickAtlasGpu::upload without ensure_capacity (index {}/{}, pool {}/{})",
            index.len(), self.index_capacity, pool_bricks, self.pool_capacity_bricks
        );
        queue.write_buffer(&self.index_buf, 0, bytemuck::cast_slice(index));
        if !pool.is_empty() {
            queue.write_buffer(&self.pool_buf, 0, bytemuck::cast_slice(pool));
        }
        let _ = self.stamp_index_content(index, pool_bricks, dims_bricks, res, body_nid);
    }

    /// S2.4 (SparseBricks bake dispatch): write ONLY the classifier index + stamp the content
    /// identity (`dims_bricks`, explicit `res`, `allocated`, owning body). The POOL content is NOT
    /// uploaded — the mode-2 GPU bake (`volume_bake.wgsl`, `output_mode == 2u`) writes it in place
    /// via binding 2, using exactly this index (binding 5) to route voxels to handles. Capacities
    /// must already fit — call `ensure_capacity(index.len(), allocated)` first; asserted like
    /// `upload` (a truncated index would route bake writes out of the meshed brick set).
    ///
    /// Returns the per-phase CPU split so the caller's [SPIKE] line can attribute a slow upload
    /// (write_buffer vs the two CPU index scans) — slice D instrumentation, 2026-07-16.
    pub fn upload_index(
        &mut self, queue: &wgpu::Queue,
        index: &[u32], allocated: u32, dims_bricks: [u32; 3], res: u32, body_nid: NodeId,
    ) -> UploadIndexSplitMs {
        assert_eq!(
            index.len() as u64,
            dims_bricks[0] as u64 * dims_bricks[1] as u64 * dims_bricks[2] as u64,
            "brick index length must equal dims_bricks product"
        );
        assert!(
            index.len() as u32 <= self.index_capacity && allocated <= self.pool_capacity_bricks,
            "SdfBrickAtlasGpu::upload_index without ensure_capacity (index {}/{}, pool {}/{})",
            index.len(), self.index_capacity, allocated, self.pool_capacity_bricks
        );
        let t = std::time::Instant::now();
        queue.write_buffer(&self.index_buf, 0, bytemuck::cast_slice(index));
        let write_ms = t.elapsed().as_secs_f64() * 1000.0;
        let scans_ms = self.stamp_index_content(index, allocated, dims_bricks, res, body_nid);
        UploadIndexSplitMs { write_ms, ..scans_ms }
    }

    /// D1 (GPU-autonomous index): stamp content identity + recompute the CPU-derived index
    /// products (`surface_brick_bbox`, `tile_stats`) WITHOUT touching `index_buf` — the caller
    /// already placed the identical bytes on-device (GPU→GPU copy from the classify finalize
    /// output). Shared by `upload_index` (which prepends the write) so both paths produce the
    /// exact same CPU-side state.
    pub fn stamp_index_content(
        &mut self,
        index: &[u32], allocated: u32, dims_bricks: [u32; 3], res: u32, body_nid: NodeId,
    ) -> UploadIndexSplitMs {
        self.dims_bricks = dims_bricks;
        self.res = res;
        self.allocated_bricks = allocated;
        self.body_nid = Some(body_nid);
        // Slice D: ONE fused pass replaces the separate bbox + tile scans (see
        // `index_content_scan` — ~55-70ms → single-pass at 1024³ in the dev profile).
        let t = std::time::Instant::now();
        let (bmin, bmax, stats) = index_content_scan(index, dims_bricks);
        let tiles_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.surface_brick_min = bmin;
        self.surface_brick_max = bmax;
        self.tile_stats = stats;
        UploadIndexSplitMs { write_ms: 0.0, bbox_ms: 0.0, tiles_ms }
    }
}

/// Per-phase CPU cost of `upload_index` (slice D attribution): the 8.4MB-at-1024 `write_buffer`
/// vs the fused index content scan (`tiles_ms`; `bbox_ms` is 0 now that the scans are fused —
/// kept so the [SPIKE] line format stays comparable across owner logs).
#[derive(Clone, Copy, Debug, Default)]
pub struct UploadIndexSplitMs {
    pub write_ms: f64,
    pub bbox_ms: f64,
    pub tiles_ms: f64,
}

#[cfg(test)]
mod fused_scan_tests {
    use super::*;

    /// Deterministic pseudo-random tag pattern (no rand dep): mixes the linear index through a
    /// splitmix-style hash; handle bricks are re-tagged dense-packed afterwards (tag VALUES are
    /// irrelevant to both scans — only `>= 0xFFFF_FFFE` vs handle matters, but keep it shaped
    /// like a real classifier index anyway).
    fn synth_index(dims: [u32; 3], live_permille: u64, salt: u64) -> Vec<u32> {
        let n = (dims[0] * dims[1] * dims[2]) as usize;
        let mut index = vec![0u32; n];
        let mut handle = 0u32;
        for (i, tag) in index.iter_mut().enumerate() {
            let mut x = i as u64 ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            x ^= x >> 30; x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x ^= x >> 27; x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^= x >> 31;
            *tag = if x % 1000 < live_permille {
                handle += 1;
                handle - 1
            } else if x % 2 == 0 {
                0xFFFF_FFFFu32 // EXTERIOR
            } else {
                0xFFFF_FFFEu32 // INTERIOR
            };
        }
        index
    }

    /// SLICE D GATE — the fused single-pass `index_content_scan` produces EXACTLY the reference
    /// oracles' output (`surface_brick_bbox` + `tile_occupancy`) on dims that exercise all the
    /// edge classes: non-multiple-of-4 axes (partial edge tiles), sparse/dense occupancy, empty.
    #[test]
    fn fused_scan_matches_reference_oracles() {
        for (dims, permille, salt) in [
            ([16u32, 16, 16], 120u64, 1u64),
            ([13, 9, 7], 300, 2),   // non-multiple-of-4 per axis — partial edge tiles
            ([4, 4, 4], 1000, 3),   // fully live
            ([8, 8, 8], 0, 4),      // fully sentinel — empty surface
            ([32, 20, 12], 50, 5),  // sparse
            ([5, 1, 1], 500, 6),    // degenerate thin axis
            ([40, 24, 36], 200, 7), // many tile-z layers — exercises the parallel boundary pre-pass
        ] {
            let index = synth_index(dims, permille, salt);
            let (fmin, fmax, fused) = index_content_scan(&index, dims);
            let (rmin, rmax) = surface_brick_bbox(&index, dims);
            let reference = tile_occupancy(&index, dims);
            assert_eq!((fmin, fmax), (rmin, rmax), "bbox diverged (dims {dims:?})");
            assert_eq!(fused.len(), reference.len(), "live tile count diverged (dims {dims:?})");
            for (f, r) in fused.iter().zip(reference.iter()) {
                assert_eq!(f.tile, r.tile, "tile order diverged (dims {dims:?})");
                assert_eq!(
                    (f.bricks, f.halo_bricks, f.bmin, f.bmax),
                    (r.bricks, r.halo_bricks, r.bmin, r.bmax),
                    "tile {:?} stats diverged (dims {dims:?})", f.tile
                );
            }
        }
    }

    /// 2048-SHIP item 3 GATE — refuse-grow degrades gracefully at the binding limit: the ×1.25
    /// pad + quantum rounding are clamped so the pool buffer stays bindable as ONE entire
    /// storage binding; the actual content ALWAYS still fits (never a validation error, never
    /// an under-`needed` capacity). Boundary numbers = the spike's 4090-class limit (2.15GB /
    /// 2048B per brick = 1 048 576-brick ceiling) and the measured realistic-2048 surface.
    #[test]
    fn grow_pad_clamps_at_binding_ceiling() {
        let max_bind: u64 = 2_147_483_648; // 2 GiB — the measured device class
        let ceiling = pool_ceiling_bricks(max_bind);
        assert_eq!(ceiling as u64 * BRICK_FLOATS as u64 * 4, max_bind, "exact-fit ceiling");

        // Far from the ceiling: pad applies in full (grow-quantum rounded, >= 1.25x need).
        let need = 723_000u32; // realistic-2048 measured surface
        let cap = padded_capacity_clamped(need, ceiling);
        assert!(cap >= need + need / 4 && cap <= ceiling, "full pad far from the ceiling");
        assert_eq!(cap % GROW_QUANTUM, 0, "quantum-aligned when unclamped");

        // Near the ceiling: the pad would cross the limit -> clamped to the ceiling exactly,
        // content still fits (refuse-grow = only the churn headroom degrades).
        let need = ceiling - GROW_QUANTUM / 2;
        let cap = padded_capacity_clamped(need, ceiling);
        assert_eq!(cap, ceiling, "pad refused at the ceiling");
        assert!(cap >= need, "content always fits");
        assert!(cap as u64 * BRICK_FLOATS as u64 * 4 <= max_bind, "always bindable");

        // AT the ceiling: capacity == need == ceiling, still bindable, no panic.
        assert_eq!(padded_capacity_clamped(ceiling, ceiling), ceiling);

        // Degenerate small requests keep the quantum floor.
        assert_eq!(padded_capacity_clamped(1, ceiling), GROW_QUANTUM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2048-SHIP item 3 GATE — refuse-grow degrades gracefully at the binding limit: the ×1.25
    /// pad + quantum rounding are clamped so the pool buffer stays bindable as ONE entire
    /// storage binding; the actual content ALWAYS still fits (never a validation error, never
    /// an under-`needed` capacity). Boundary numbers = the spike's 4090-class limit (2.15GB /
    /// 2048B per brick = 1 048 576-brick ceiling) and the measured realistic-2048 surface.
    #[test]
    fn grow_pad_clamps_at_binding_ceiling() {
        let max_bind: u64 = 2_147_483_648; // 2 GiB — the measured device class
        let ceiling = pool_ceiling_bricks(max_bind);
        assert_eq!(ceiling as u64 * BRICK_FLOATS as u64 * 4, max_bind, "exact-fit ceiling");

        // Far from the ceiling: pad applies in full (grow-quantum rounded, >= 1.25x need).
        let need = 723_000u32; // realistic-2048 measured surface
        let cap = padded_capacity_clamped(need, ceiling);
        assert!(cap >= need + need / 4 && cap <= ceiling, "full pad far from the ceiling");
        assert_eq!(cap % GROW_QUANTUM, 0, "quantum-aligned when unclamped");

        // Near the ceiling: the pad would cross the limit -> clamped to the ceiling exactly,
        // content still fits (refuse-grow = only the churn headroom degrades).
        let need = ceiling - GROW_QUANTUM / 2;
        let cap = padded_capacity_clamped(need, ceiling);
        assert_eq!(cap, ceiling, "pad refused at the ceiling");
        assert!(cap >= need, "content always fits");
        assert!(cap as u64 * BRICK_FLOATS as u64 * 4 <= max_bind, "always bindable");

        // AT the ceiling: capacity == need == ceiling, still bindable, no panic.
        assert_eq!(padded_capacity_clamped(ceiling, ceiling), ceiling);

        // Degenerate small requests keep the quantum floor.
        assert_eq!(padded_capacity_clamped(1, ceiling), GROW_QUANTUM);
    }
}
