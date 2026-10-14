//! Resident mesher: the production, no-readback, indirect-draw path.
//!
//! `SdfMeshGpu` — persistent cached pipelines + all dispatch entry points (new, mesh_readback,
//! dispatch_async, dispatch_resident, mesh_into_slot, mesh_into_slot_from_resident (G3),
//! mesh_into_slot_sparse + dispatch_resident_sparse (Phase 3B)). `SdfBodyMeshSlot` — the per-body
//! GPU buffer slot + its surface-cap allocation. Both re-exported by the parent `mod` so
//! `crate::sdf_gpu_mesh::SdfMeshGpu` / `SdfBodyMeshSlot` (main.rs, cache_state.rs, gpu_ctx.rs,
//! app_poll.rs) keep resolving. Depends on the DC plumbing + readback via `super::*`.

use eframe::wgpu;
use glam::Vec3;

use super::*;

/// C2 — what changed since the LAST tiled mesh of this body, as passed by the caller. `None`
/// (no dirty info) always means a FULL tiled re-mesh — incremental only ever narrows a dispatch
/// when the caller can prove the delta.
pub(crate) enum SdfTileDirty {
    /// The bake's changed-VOXEL window `[min, max)` in the `res` grid (the
    /// `voxel_bake_min/max` window the unified `dirty_mesh_region` accumulates). An empty window
    /// (min ≥ max on any axis) means "nothing changed" — zero dispatches.
    Voxels([u32; 3], [u32; 3]),
    /// Explicit packed tile keys (`x | y<<16 | z<<32`) — the overflow re-mesh path re-dispatches
    /// exactly the tiles whose cap floors were raised.
    Tiles(Vec<u64>),
}

/// C2 — provenance stamp of the last TILED mesh dispatch: the slot's tile table is only valid
/// for incremental reuse while ALL of these still match (any mismatch ⇒ full re-mesh; the
/// conservative default). `atlas_generation` covers atlas buffer recreation (grow) — content
/// relocations the tile table has no visibility into.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct SdfTiledStamp {
    pub(crate) res: u32,
    pub(crate) grid_min: Vec3,
    pub(crate) grid_max: Vec3,
    pub(crate) atlas_generation: u64,
    pub(crate) dims_bricks: [u32; 3],
    /// Phase 4b — the SEGMENT count the tile tables were partitioned into (per-body multi-group
    /// tiling). The tile→segment mapping is part of the table's identity: an incremental reuse
    /// against a table whose segmentation changed would route counters/args to wrong entries.
    pub(crate) segments: u32,
}

/// Pack a tile coordinate into the stable u64 key (floors, dirty maps, diff sets).
pub(crate) fn tile_key(t: [u32; 3]) -> u64 {
    t[0] as u64 | (t[1] as u64) << 16 | (t[2] as u64) << 32
}

/// A tile's quad anchor window in CELLS (inclusive), from its handle-brick bbox — ONE definition
/// shared by the full tiled build and the C2 incremental diff (they must agree bit-for-bit).
fn tile_window(st: &graphics::sdf_brick_atlas::SdfTileStat, bd: u32, cells: u32) -> ([u32; 3], [u32; 3]) {
    let mut amin = [0u32; 3];
    let mut amax = [0u32; 3];
    for a in 0..3 {
        amax[a] = ((st.bmax[a] + 1) * bd - 1).min(cells.saturating_sub(1));
        amin[a] = (st.bmin[a] * bd).min(amax[a]);
    }
    (amin, amax)
}

/// G1 resident mesher — persistent pipelines, dispatches into caller-owned persistent buffers and
/// writes the index count into a `DrawIndexedIndirect` args buffer via a GPU copy (NO readback).
/// The owning subsystem (GpuCtx, G1 integration) holds one of these + per-body buffer slots.
pub(crate) struct SdfMeshGpu {
    bgl: wgpu::BindGroupLayout,
    pipe_vertex: wgpu::ComputePipeline,
    pipe_quads: wgpu::ComputePipeline,
    /// C1 tiled DC — the 9-binding layout + the three tiled pipelines (vertex/quads/args-fill)
    /// from the SAME sdf_mesh.wgsl module. The 8-binding monolithic set above is untouched.
    tiled_bgl: wgpu::BindGroupLayout,
    pipe_vertex_tiled: wgpu::ComputePipeline,
    pipe_quads_tiled: wgpu::ComputePipeline,
    pipe_tile_args: wgpu::ComputePipeline,
    /// 1-elem dummies for the sparse-atlas bindings (6/7) on the dense DC path.
    dummy_index: wgpu::Buffer,
    dummy_pool: wgpu::Buffer,
    /// R3b — vertex headroom per allocated surface brick for the sparse output sizing. Production
    /// value = `VERTS_PER_BRICK_HEADROOM` (measured); tests shrink it to force the overflow-grow
    /// path deterministically.
    pub(crate) verts_per_brick_headroom: u64,
    /// Per-TILE twin of the above (C2 follow-up): tiles size at the tighter
    /// `TILE_VERTS_PER_BRICK_HEADROOM` (28 — measured ~20-22.6 + margin) because a per-tile
    /// overflow costs only a single-tile grow+re-mesh under C2, while 48 packed realistic 1024
    /// scenes past the pool ceiling (the C1 envelope finding → monolithic fallback). Tests
    /// shrink it to force the per-tile overflow path deterministically.
    pub(crate) tile_verts_per_brick_headroom: u64,
    /// R2 prereq diagnostic — GPU timestamps for the sparse DC dispatch. The CPU-side
    /// `[DcResidentSparse]` split cannot see GPU duration (submits are async), which is why the
    /// owner's logs showing no >2ms lines were NOT evidence the R2 term is dead. 4 queries
    /// (vertex begin/end, quads begin/end) + a 32-byte resolve buffer; both pairs are written and
    /// resolved inside the SAME encoder every sparse dispatch, so an unwritten-query resolve
    /// (device loss on NVIDIA) is impossible by construction. None when the adapter lacks
    /// `TIMESTAMP_QUERY` — the dispatch is then untimed, never a fallback timing trick.
    dc_ts: Option<(wgpu::QuerySet, wgpu::Buffer)>,
}

#[allow(dead_code)] // wired into GpuCtx + the live draw in G1-integration / G2.
impl SdfMeshGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        Self::new_with_cache(device, None)
    }

    /// The 562ms first-select stall (owner log 5, 2026-07-16): this constructor compiles
    /// `sdf_mesh.wgsl` into TWO shader modules + FIVE compute pipelines synchronously. It used to
    /// run LAZILY inside `poll_primary_bake` → `sdf_populate_resident` on the first bake accept
    /// (and with `cache: None`, so no pipeline cache — cold every session). Production now warms
    /// it on the background WGSL compile thread (`init_gpu_ctx` → `PipelineReady::SdfMesher`) with
    /// the shared pipeline cache; the lazy in-poll path is a fallback only (`ensure_sdf_mesher`).
    pub(crate) fn new_with_cache(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>) -> Self {
        let _t = std::time::Instant::now();
        let (bgl, pipe_vertex, pipe_quads) = build_dc_pipelines(device, cache);
        let (tiled_bgl, pipe_vertex_tiled, pipe_quads_tiled, pipe_tile_args) =
            build_dc_tiled_pipelines(device, cache);
        // One-shot per session — mirrors the [TIMING] pipeline-compile convention so a run that
        // still hits the lazy path attributes the stall by name.
        eprintln!("[TIMING] SDF DC mesher pipelines compiled (2 modules, 5 pipelines): {:?}", _t.elapsed());
        let dummy_index = dummy_storage(device, "sdf_dc_dummy_index");
        let dummy_pool = dummy_storage(device, "sdf_dc_dummy_pool");
        let dc_ts = device.features().contains(wgpu::Features::TIMESTAMP_QUERY).then(|| {
            let qs = device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("sdf_dc_sparse_ts"), ty: wgpu::QueryType::Timestamp, count: 4,
            });
            let resolve = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_dc_sparse_ts_resolve"), size: 32,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            (qs, resolve)
        });
        Self { bgl, pipe_vertex, pipe_quads,
               tiled_bgl, pipe_vertex_tiled, pipe_quads_tiled, pipe_tile_args,
               dummy_index, dummy_pool,
               verts_per_brick_headroom: VERTS_PER_BRICK_HEADROOM,
               tile_verts_per_brick_headroom: TILE_VERTS_PER_BRICK_HEADROOM, dc_ts }
    }

    /// GPU-mesh `grid` (dense `res³` signed distance, world box [grid_min, grid_max]) and read back
    /// `(verts, indices)` using the cached pipelines. Blocking (one submit + poll Wait). The live
    /// mesh_cache path uses this until the resident no-readback path (G3/G4) lands.
    pub(crate) fn mesh_readback(
        &self, device: &wgpu::Device, queue: &wgpu::Queue,
        grid: &[f32], res: u32, grid_min: Vec3, grid_max: Vec3,
    ) -> (Vec<[f32; 8]>, Vec<u32>) {
        dispatch_readback(device, queue, &self.bgl, &self.pipe_vertex, &self.pipe_quads, grid, res, grid_min, grid_max)
    }

    /// ASYNC: dispatch the QEF-DC passes + kick a non-blocking readback, returning immediately. The
    /// caller stores the `PendingSdfReadback` and reads it (`ready()`/`take()`) on a later frame. This
    /// is the live mesh_cache path — it removes the blocking `poll(Wait)` stall (the per-frame freeze)
    /// while the GPU is saturated by per-frame bakes during a drag.
    pub(crate) fn dispatch_async(
        &self, device: &wgpu::Device, queue: &wgpu::Queue,
        grid: &[f32], res: u32, grid_min: Vec3, grid_max: Vec3,
    ) -> PendingSdfReadback {
        dispatch_to_staging(device, queue, &self.bgl, &self.pipe_vertex, &self.pipe_quads, grid, res, grid_min, grid_max)
    }

    /// Mesh `field` (resident density) into the persistent `verts`/`idx` buffers, then copy the GPU
    /// index count into `indirect`'s `DrawIndexedIndirect.index_count` (first u32) — no readback. The
    /// caller seeds `indirect` once as [_, 1, 0, 0, 0] (instance_count=1, first_index/base_vertex/
    /// first_instance = 0); only `index_count` changes per dispatch. `counters` is zeroed here.
    /// `params` must already hold `dc_params(res, bounds)`. Appends to the caller's command stream is
    /// avoided — this owns one encoder + submit so the bake→mesh write→read barrier is explicit.
    pub(crate) fn dispatch_resident(
        &self, device: &wgpu::Device, queue: &wgpu::Queue,
        field: &wgpu::Buffer, params: &wgpu::Buffer, cell_vert: &wgpu::Buffer,
        verts: &wgpu::Buffer, idx: &wgpu::Buffer, counters: &wgpu::Buffer, indirect: &wgpu::Buffer,
        res: u32,
    ) {
        // [DcResident] split timing — pinpoints whether the ~2.7ms "dc_dispatch" is bind-group creation,
        // command encode, or queue.submit (GPU-queue backpressure). Logged only when total >2ms so it
        // catches the expensive drag-frame dispatches without spamming.
        let _t0 = std::time::Instant::now();
        queue.write_buffer(counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));
        let bg = dc_bind_group(device, &self.bgl, field, params, cell_vert, verts.as_entire_binding(), idx.as_entire_binding(), counters, &self.dummy_index, &self.dummy_pool);
        let _t_bg = _t0.elapsed().as_secs_f64() * 1000.0;
        let _t1 = std::time::Instant::now();
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sdf_dc_resident") });
        encode_dc_passes(&mut enc, &self.pipe_vertex, &self.pipe_quads, &bg, res - 1);
        // GPU index_count (counters[1] @ byte 4) → indirect args index_count (byte 0). No CPU readback;
        // the shader's atomicMin clamp guarantees this never exceeds the index buffer length (no OOB draw).
        enc.copy_buffer_to_buffer(counters, 4, indirect, 0, 4);
        let _t_enc = _t1.elapsed().as_secs_f64() * 1000.0;
        let _t2 = std::time::Instant::now();
        queue.submit(std::iter::once(enc.finish()));
        let _t_submit = _t2.elapsed().as_secs_f64() * 1000.0;
        let _total = _t_bg + _t_enc + _t_submit;
        if _total > 2.0 {
            eprintln!("[DcResident] res={res} total={:.2}ms (bind_group={:.2} encode={:.2} submit={:.2})",
                _total, _t_bg, _t_enc, _t_submit);
        }
    }

    /// G1: mesh `grid` into a body's RESIDENT slot — uploads the density + params, dispatches the two
    /// QEF-DC passes, and writes the GPU index count into the slot's `DrawIndexedIndirect` args. NO mesh
    /// readback: `slot.verts`/`slot.idx` stay GPU-resident for G2 to draw via `draw_indexed_indirect`.
    /// The bind group is built fresh over the slot's OWN buffers — nothing external is referenced, so a
    /// `ResidentVolume` realloc can never dangle it (the device-loss class the generation counter would
    /// otherwise guard). G3 swaps `slot.field` for the persistent resident density (no grid upload).
    #[allow(dead_code)] // wired into the poll-phase populate + indirect draw in G2.
    /// Dense mesh into slot. Returns `false` if res is illegal for dense DC (must use sparse)
    /// or capacity cannot be bound — never submits an unbindable `sdf_dc_bg`.
    pub(crate) fn mesh_into_slot(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut SdfBodyMeshSlot,
        grid: &[f32], grid_min: Vec3, grid_max: Vec3,
    ) -> bool {
        let res = slot.res;
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        // I1: dense DC only ≤ max_gridbacked (512). Dense@1024 → 4.28GB cell_vert → device loss.
        let Some(cv) = dc_cell_vert_cap_dense(res, max_bind) else {
            eprintln!(
                "[DcBind] REJECT dense mesh_into_slot res={res}³ — exceeds GridBacked max or bind limit"
            );
            return false;
        };
        slot.sanitize_cell_vert_if_poisoned(device);
        slot.ensure_field(device, res); // lazy dense field (P0-R3a) — poll-phase caller only
        // R3c: cell_vert from DISPATCH res (not capacity_res) — see CELL_VERT_BIND_LIMIT plan.
        let (dv, di) = dc_output_caps_dense(max_bind, res);
        if !slot.ensure_output_caps(device, dv, di, cv) {
            eprintln!("[DcBind] REJECT dense mesh_into_slot res={res}³ — ensure_output_caps failed");
            return false;
        }
        slot.pooled = false; // dense mesh → per-body buffers; the pool entry self-skips per frame
        slot.tiled = false;
        slot.last_tiled_stamp = None; // C2: a non-tiled dispatch invalidates the tile table provenance
        let field = slot.field.as_ref().expect("ensure_field just allocated");
        let grid_bytes = (grid.len() as u64) * 4;
        if grid_bytes > field.size() {
            // 128³ (8 MB) into a 4 B placeholder = device-loss (PathTrace / dense DC).
            eprintln!(
                "[DcBind] REJECT mesh_into_slot field write: grid {}B vs field {}B (res={res})",
                grid_bytes,
                field.size()
            );
            return false;
        }
        if grid.len() as u32 != res * res * res {
            eprintln!(
                "[DcBind] REJECT mesh_into_slot: grid len {} != res³ {}",
                grid.len(),
                res * res * res
            );
            return false;
        }
        queue.write_buffer(field, 0, bytemuck::cast_slice(grid));
        queue.write_buffer(&slot.params, 0, bytemuck::bytes_of(&dc_params(res, grid_min, grid_max)));
        self.dispatch_resident(
            device, queue, field, &slot.params, &slot.cell_vert,
            &slot.verts, &slot.idx, &slot.counters, &slot.indirect, res);
        slot.note_mesh_content();
        true
    }

    /// G3: mesh DIRECTLY from an EXTERNAL GPU-resident density buffer (e.g. `ResidentVolume.buffer`) —
    /// NO `queue.write_buffer(&slot.field, …)`, so the per-frame res³ f32 host→GPU upload disappears (the
    /// ~2.5ms `to_vec` + nan-scan readback the live drag log shows). `dispatch_resident` builds the bind
    /// group fresh over `resident_field` each call, so there is no cached BG to dangle — BUT the caller is
    /// responsible for the two hazards fresh-binding does NOT cover (adversarially confirmed in the G3
    /// de-risk pass): (1) `resident_field` must already hold THIS bake's density at dispatch time — issue
    /// this in the SAME submit as, and AFTER, the bake→resident `copy_buffer_to_buffer`; (2) guard against
    /// a `resident_field` realloc between bake and dispatch via a `ResidentVolume.generation` match.
    /// `resident_field` MUST carry `STORAGE` usage and be res³ f32 in `x + y*res + z*res²` order (identical
    /// to `slot.field` — proven bit-parity-equivalent by `sdf_dc_resident_field_parity`).
    #[allow(dead_code)] // wired into sdf_populate_resident behind the gpu_sdf_mesh_resident_field sub-flag.
    /// Dense mesh from resident field. Returns `false` if illegal for dense or bind-unsafe.
    pub(crate) fn mesh_into_slot_from_resident(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut SdfBodyMeshSlot,
        resident_field: &wgpu::Buffer, grid_min: Vec3, grid_max: Vec3,
    ) -> bool {
        let res = slot.res;
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        let Some(cv) = dc_cell_vert_cap_dense(res, max_bind) else {
            eprintln!(
                "[DcBind] REJECT dense mesh_into_slot_from_resident res={res}³ — exceeds GridBacked max or bind limit"
            );
            return false;
        };
        slot.sanitize_cell_vert_if_poisoned(device);
        let (dv, di) = dc_output_caps_dense(max_bind, res);
        if !slot.ensure_output_caps(device, dv, di, cv) {
            eprintln!("[DcBind] REJECT dense field-direct res={res}³ — ensure_output_caps failed");
            return false;
        }
        slot.pooled = false; // dense mesh → per-body buffers; the pool entry self-skips per frame
        slot.tiled = false;
        slot.last_tiled_stamp = None; // C2: a non-tiled dispatch invalidates the tile table provenance
        // params still written per dispatch — they carry res/bounds (the staleness vector §3 guards).
        queue.write_buffer(&slot.params, 0, bytemuck::bytes_of(&dc_params(res, grid_min, grid_max)));
        self.dispatch_resident(
            device, queue, resident_field, &slot.params, &slot.cell_vert,
            &slot.verts, &slot.idx, &slot.counters, &slot.indirect, res);
        slot.note_mesh_content();
        true
    }

    /// Phase 3A slice 1 (sparse): mesh the body from the PERSISTENT sparse brick atlas
    /// (`SdfBrickAtlasGpu`, owned by GpuCtx) instead of a dense res³ field. The caller — the sparse
    /// pre-pass in `app_poll::sdf_populate_resident`, POLL context — has already classified the
    /// dense grid (`classify_grid_to_sparse`, proven bit-exact) and `ensure_capacity`+`upload`ed
    /// index/pool into `atlas`; this only writes the sparse params (`brick_res` in `gext.w`
    /// switches `fld()` to the 8³ decode) and dispatches the production sparse DC. The bind group
    /// is built FRESH over the atlas buffers each dispatch (nothing is cached here), so an atlas
    /// grow (buffer recreate + `generation` bump) can never dangle a BG in this path — the
    /// generation field guards consumers that DO cache. The dense `field` binding is unused by the
    /// shader on this path but the BGL still requires a buffer there — `slot.field` is passed
    /// (not read).
    pub(crate) fn mesh_into_slot_sparse(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut SdfBodyMeshSlot,
        atlas: &graphics::SdfBrickAtlasGpu, grid_min: Vec3, grid_max: Vec3,
        body: core_math::scene_graph::NodeId, mut pool: Option<&mut SdfMeshPool>,
    ) {
        let res = slot.res;
        // The atlas carries `res` explicitly (never cbrt-derived); it must hold THIS slot's content.
        assert_eq!(atlas.res, res, "SdfBrickAtlasGpu content res != slot res — upload before dispatch");

        // R3b: SURFACE-PROPORTIONAL output sizing — the vertex/index caps come from the allocated
        // surface-brick count (measured headroom, see VERTS_PER_BRICK_HEADROOM), not the dense
        // cells³ worst case. Grow-only: a slot that already meshed dense keeps its larger buffers.
        let (sv, si) = dc_output_caps_sparse(atlas.allocated_bricks as u64, self.verts_per_brick_headroom);
        // Stage B-1: SPARSE bodies mesh into a REGION of the shared `SdfMeshPool` (one
        // multi-draw for all sparse bodies) instead of per-body verts/idx. Grow-only per slot:
        // never request below the region's current caps (the R3b oscillation discipline). The
        // caps are clamped to the dense ceiling exactly like `ensure_output_caps` does. On
        // `Err` (the request can't fit the pool's 1 GiB/binding cap) fall back to the proven
        // per-body buffers — `slot.pooled=false` routes the draw back to `draw_sdf_resident_slot`.
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        // capacity_res may be 1024 (sparse-era slot) — dense ceiling math only legal <=512.
        let (dense_v, dense_i) = {
            let d = dc_output_caps_dense(
                max_bind,
                slot.capacity_res.min(crate::gpu_jobs::max_gridbacked_resolution()),
            );
            if d.0 == 0 {
                dc_output_caps_dense(max_bind, crate::gpu_jobs::max_gridbacked_resolution())
            } else {
                d
            }
        };
        // SETTLE-TIME TRIM (slice b): one-shot flag set by the poll caller when the body is quiet.
        // Shrinks the grow-only caps back toward the CURRENT surface need (hysteresis: fire under
        // ½, land at 1.25×) so a transient mid-gesture brick peak stops pinning GBs of VRAM. The
        // trimmed region/buffers are rewritten by THIS dispatch — nothing stale survives.
        let trim = std::mem::take(&mut slot.may_trim);
        let mut pooled = false;
        if let Some(p) = pool.as_deref_mut() {
            if trim {
                let tv = (sv.min(dense_v) * 5 / 4).max(64);
                if p.trim_slot(device, queue, body, tv, (tv * 6).min(dense_i)) {
                    slot.generation += 1; // region relocated — counters over timing
                }
            }
            let (cur_v, cur_i) = p.slot(body).map(|s| (s.vert_cap, s.idx_cap)).unwrap_or((0, 0));
            // CHURN HEADROOM (2026-07-16 owner log): with ONE slot the bump tail is always
            // exhausted, so EVERY growth REBUILDS the pool — 5 rebuilds × 45-70ms of fresh
            // ~1GB allocations in a single 1024 gesture (the [SLOWPOLL primary] spikes). The
            // gesture grew the surface 2.3× (203k → 463k bricks), so sizing to the CURRENT need
            // guarantees a rebuild every few frames. While the body is CHURNING (not quiet —
            // the same escalation signal that drives `may_trim`), request 2.5× so one rebuild
            // covers a whole gesture; the settle trim above reclaims it the moment things go
            // quiet. Anchored to NEED and clamped by the pool's own per-buffer ceiling (a
            // request above it would fall back to per-body slots — losing the multi-draw), so
            // it can neither compound nor overflow. Quiet bodies keep today's tight sizing.
            let (mut want_v, mut want_i) = (sv.min(dense_v), si.min(dense_i));
            if !trim {
                // Phase 4: clamp to the pool's PER-GROUP buffer cap (a body's region must fit
                // one group buffer — tiles/regions never span groups).
                let pool_max = device.limits().max_buffer_size
                    .min(device.limits().max_storage_buffer_binding_size as u64)
                    .min(p.max_group_bytes);
                want_v = (want_v.saturating_mul(5) / 2).min(dense_v).min(pool_max / 32);
                want_i = (want_i.saturating_mul(5) / 2).min(dense_i).min(pool_max / 4);
            }
            let want_v = want_v.max(cur_v);
            let want_i = want_i.max(cur_i);
            match p.ensure_slot(device, queue, body, want_v, want_i) {
                Ok(relocated) => {
                    // Phase 4b: a body falling back from the TILED path must shed its stale tile
                    // table + higher segments before the monolithic entry draws (else the
                    // per-body multi-draw still covers the stale tile entries).
                    p.make_monolithic(queue, body);
                    let ps = p.slot(body).expect("ensure_slot just succeeded");
                    // Track the region caps on the slot so the R3b overflow predicate
                    // (`output_overflow`, judged in poll_sdf_mesh_output_overflow) stays exact.
                    slot.vert_cap = ps.vert_cap;
                    slot.idx_cap = ps.idx_cap;
                    if relocated { slot.generation += 1; } // offsets moved — counters over timing
                    pooled = true;
                }
                Err(()) => {
                    eprintln!("[SdfPool] body={body:?} request (v={sv} i={si}) exceeds the pool cap — per-body slot fallback");
                }
            }
        }
        slot.pooled = pooled;
        slot.lod_level = 0; // every monolithic dispatch is full detail (O2 LOD resets)
        slot.tiled = false; // monolithic sparse — one region, one args entry
        slot.last_tiled_stamp = None; // C2: tile table provenance void after a monolithic dispatch
        // R3c (Tiled-DC stage A): cell_vert is BRICK-LOCAL on the sparse path — the shader writes
        // cell 'c' of handle H at cell_vert[H*512 + local], so the EXACT capacity is
        // allocated_bricks × 512 (surface-proportional; ≈2KB/brick vs cells³-dense ~534MB at 512).
        // No overflow is possible: every handle the shader can read from THIS dispatch's atlas is
        // < allocated_bricks by construction, and the bind group is built fresh over that same
        // atlas in this call (identity holds like the atlas.res assert above).
        // I2/I3: sparse cell_vert = allocated_bricks×512, must fit one storage binding.
        let Some(cv_need) = dc_cell_vert_cap_sparse(atlas.allocated_bricks as u64, max_bind) else {
            eprintln!(
                "[DcBind] REJECT sparse mesh res={}³ bricks={} — cell_vert would exceed bind limit ({}MB)",
                res, atlas.allocated_bricks, max_bind >> 20
            );
            slot.envelope_over_budget = true;
            return;
        };
        slot.sanitize_cell_vert_if_poisoned(device);
        // Pooled: verts/idx live in the pool region — size only cell_vert here (want_v/want_i = 0
        // is a no-op against the grow-only watermark). Unpooled: today's exact per-body sizing.
        let (own_v, own_i) = if pooled { (0, 0) } else { (sv, si) };
        if trim {
            // cell_vert lands at EXACTLY cv_need (the sparse cap is exact by design); unpooled
            // own buffers land at 1.25× need. Pooled slots pass 0 (their verts/idx live in the
            // pool region, trimmed above).
            let (tw_v, tw_i) = if pooled { (0, 0) }
                               else { ((own_v * 5 / 4).max(64), (own_i * 5 / 4).max(64)) };
            slot.trim_output_caps(device, tw_v, tw_i, cv_need);
        }
        if !slot.ensure_output_caps(device, own_v, own_i, cv_need) {
            eprintln!(
                "[DcBind] REJECT sparse mesh res={}³ — ensure_output_caps failed for cell_vert",
                res
            );
            slot.envelope_over_budget = true;
            return;
        }
        assert!(slot.cell_vert_cap >= cv_need && slot.cell_vert.size() >= cv_need * 4,
            "sparse cell_vert cap ({}) < allocated_bricks×512 ({cv_need}) — brick-local addressing would read OOB",
            slot.cell_vert_cap);
        slot.last_sparse_bounds = Some((grid_min, grid_max));
        // Output targets: the pool region sub-ranges (256B-aligned offset+size — the voxel-pool
        // pattern; the shader's arrayLength guards clamp against the bound sub-range) or the
        // slot's own buffers. Indirect target = the slot's entry in the shared args buffer
        // (multi-draw) or the per-body args (per-body draw). Shared refs only from here on.
        let pool_ref = pool.as_deref();
        let (verts_bind, idx_bind, ind_buf, ind_off): (wgpu::BindingResource, wgpu::BindingResource, &wgpu::Buffer, u64) =
            match (pooled, pool_ref) {
                (true, Some(p)) => {
                    let ps = p.slot(body).expect("pooled slot exists");
                    // Phase 4: the slot's region + args live in ITS buffer GROUP.
                    let grp = p.group(ps.group);
                    (
                        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &grp.verts, offset: ps.vert_off,
                            size: Some(std::num::NonZeroU64::new(ps.vert_region_bytes()).unwrap()),
                        }),
                        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &grp.idx, offset: ps.idx_off,
                            size: Some(std::num::NonZeroU64::new(ps.idx_region_bytes()).unwrap()),
                        }),
                        &grp.indirect, ps.indirect_offset(),
                    )
                }
                _ => (slot.verts.as_entire_binding(), slot.idx.as_entire_binding(), &slot.indirect, 0),
            };

        // WINDOWED DC (the "laggy on edit" fix): the iso surface lives entirely in the atlas's
        // HANDLE bricks (surface + apron), so re-mesh only their cell bbox instead of sweeping the
        // full 511³ grid every drag frame. On an escape-grown grid a small body is a fraction of
        // the volume → a surface-sized dispatch. Output is BIT-IDENTICAL to the full-grid sweep
        // (cells outside the box hold no surface); proven by sdf_dc_sparse_windowed_matches_full.
        let (cmin, cmax) = match surface_cell_window(atlas, res) {
            Some(w) => w,
            None => {
                // Empty surface (no handle bricks) — nothing to mesh. Zero the counter + the indirect
                // index_count so the resident draw renders no triangles (never a stale mesh).
                queue.write_buffer(&slot.counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));
                queue.write_buffer(ind_buf, ind_off, bytemuck::cast_slice(&[0u32]));
                slot.note_mesh_content();
                return;
            }
        };
        let (p, vertex_ext, quad_ext) = dc_params_sparse_windowed(
            res, grid_min, grid_max, atlas.dims_bricks[0], cmin, cmax);
        queue.write_buffer(&slot.params, 0, bytemuck::bytes_of(&p));
        // binding 0 (dense field) is never read on the sparse path (fld() with brick_res>0 reads
        // the atlas) — bind the mesher's 4-byte dummy instead of forcing the slot to carry a
        // dead 512MiB dense buffer (P0-R3a lazy field).
        // R3b overflow detection: copy the counters into a tiny staging buffer inside the SAME
        // submit (8 bytes, per-dispatch — NOT per-frame) and map it async; the poll phase reads it
        // and grows + re-meshes on overflow. At most one readback in flight per slot.
        let staging = (slot.pending_counters.is_none()).then(|| device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_slot_counters_staging"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
        self.dispatch_resident_sparse(
            device, queue, &self.dummy_index, &atlas.index_buf, &atlas.pool_buf, &slot.params, &slot.cell_vert,
            verts_bind, idx_bind, &slot.counters, ind_buf, ind_off, vertex_ext, quad_ext, staging.as_ref());
        if let Some(staging) = staging {
            slot.kick_counter_readback(staging);
        }
        slot.note_mesh_content();
    }

    /// C1 TILED sparse mesh: the production sparse entry. Subdivides the body's surface into
    /// 32³-cell tiles (from `atlas.tile_stats`), gives each tile its OWN sub-region of the body's
    /// pool region + its own `DrawIndexedIndirect` entry, and dispatches ALL tiles in ONE submit:
    /// per batch of ≤`SDF_TILE_BATCH_MAX` tiles, ONE dc_vertex_tiled pass + ONE dc_quads_tiled
    /// pass (per-tile disjoint scratch windows → no races inside a pass; pass boundaries order
    /// the batches), then ONE dc_tile_args pass publishes every tile's args from its counters.
    /// The body then draws as N tiles via ONE `multi_draw_indexed_indirect` over its args block.
    ///
    /// TILE BOUNDARY STRATEGY (option (a), halo duplication): each tile's vertex pass covers its
    /// anchor window +1-cell halo, so its quads reference ONLY own-tile vertices — tiles are
    /// SELF-CONTAINED index spaces (boundary vertices duplicated across neighbors). This is what
    /// makes C2's "re-mesh only dirty tiles" sound: rewriting one tile's region can never dangle
    /// a neighbor's indices. The alternative (cross-tile vertex sharing via the brick-local
    /// cell_vert) breaks under per-tile `base_vertex` AND under partial re-mesh — rejected.
    ///
    /// Rendered geometry is IDENTICAL to the monolithic dispatch: tile anchor windows PARTITION
    /// the surface window (Σ per-tile index_count == monolithic index_count, gate-asserted);
    /// vertex parity is bit-exact-class (same solver, same inputs). Total vertex COUNT is larger
    /// by the duplicated halo layer — expected and correct.
    ///
    /// Returns false when the pool cannot host the request (per-buffer cap) — the caller falls
    /// back to the monolithic entry (which lands on the proven unpooled per-body slot).
    /// POLL PHASE ONLY (Frame Ordering Contract).
    pub(crate) fn mesh_into_slot_sparse_tiled(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut SdfBodyMeshSlot,
        atlas: &graphics::SdfBrickAtlasGpu, grid_min: Vec3, grid_max: Vec3,
        body: core_math::scene_graph::NodeId, pool: &mut SdfMeshPool,
        dirty: Option<&SdfTileDirty>,
    ) -> bool {
        let res = slot.res;
        assert_eq!(atlas.res, res, "SdfBrickAtlasGpu content res != slot res — upload before dispatch");
        let cells = res - 1;
        let bd = graphics::sdf_brick_atlas::BRICK_FLOATS_DIM; // 8 voxels/brick per axis
        let brick_res = atlas.dims_bricks[0];
        let stats = &atlas.tile_stats;

        // ── C2: INCREMENTAL arm — when the caller proved a dirty window AND every staleness
        // guard holds, re-dispatch only the overlapping tiles. Any doubt (Err) falls through to
        // the full tiled re-mesh below; a contract violation (occupancy changed OUTSIDE the
        // window) additionally prints — never silent wrongness. A pending trim forces full
        // (trim repacks regions + resets floors — incompatible with keeping clean tiles' regions).
        if let Some(d) = dirty {
            if !slot.may_trim {
                match self.mesh_tiled_incremental(device, queue, slot, atlas, grid_min, grid_max, body, pool, d) {
                    Ok(()) => {
                        slot.note_mesh_content();
                        return true;
                    }
                    Err(Some(violation)) => eprintln!(
                        "[DcResidentSparse] body={body:?} incremental CONTRACT VIOLATION ({violation}) — full tiled re-mesh"),
                    Err(None) => {} // stale/incapable — the conservative full path below
                }
            }
        }

        // Empty surface: nothing to mesh. Zero every segment's args block so the (now stale)
        // entries self-skip; the slot stays pooled and draws nothing (matches the monolithic
        // empty arm).
        if stats.is_empty() {
            let nsegs = pool.segment_count(body);
            for seg in 0..nsegs {
                if let Some(s) = pool.slot_seg(body, seg) {
                    queue.write_buffer(&pool.group(s.group).indirect, s.indirect_offset(),
                        &vec![0u8; (s.args_cap as u64 * SDF_POOL_INDIRECT_STRIDE) as usize]);
                }
                pool.set_tiles(body, seg, Vec::new());
            }
            if nsegs > 0 {
                slot.pooled = true;
                slot.tiled = true;
            }
            queue.write_buffer(&slot.counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));
            slot.last_tiled_stamp = None; // no tile table — nothing to reuse incrementally
            slot.last_dispatched_tiles = (0, 0);
            slot.note_mesh_content();
            return true;
        }

        // SETTLE-TIME TRIM (slice b, tile-adapted): floors are transient-peak artifacts — a quiet
        // window resets them so per-tile caps re-derive from the CURRENT surface truth, and the
        // body's region shrinks back via trim_slot's hysteresis deadband below.
        let trim = std::mem::take(&mut slot.may_trim);
        if trim { slot.tile_cap_floor.clear(); }

        // ── Per-tile windows + caps (CPU tile protos; segment-relative offsets assigned by the
        // partition below) ───────────────────────────────────────────────────────────────────────
        let _t_full0 = std::time::Instant::now(); // slice D split: full-path CPU attribution
        let headroom = self.tile_verts_per_brick_headroom;
        let key = tile_key;
        let mut protos: Vec<crate::sdf_gpu_mesh::SdfTileEntry> = Vec::with_capacity(stats.len());
        let (mut total_v, mut total_i) = (0u64, 0u64);
        for st in stats {
            let (amin, amax) = tile_window(st, bd, cells);
            let floor = slot.tile_cap_floor.get(&key(st.tile)).copied().unwrap_or(0) as u64;
            let vcap = (st.halo_bricks as u64 * headroom).max(floor).clamp(32, SDF_TILE_MAX_VERTS);
            protos.push(crate::sdf_gpu_mesh::SdfTileEntry {
                tile: st.tile, amin, amax,
                rel_vert: 0, vert_cap: vcap, rel_idx: 0, idx_cap: vcap * 6,
                bricks: st.bricks, halo_bricks: st.halo_bricks, dead: false,
            });
            total_v += vcap;
            total_i += vcap * 6;
        }
        let n_total = protos.len() as u32;
        let _table_ms = _t_full0.elapsed().as_secs_f64() * 1000.0;
        let _t_pool = std::time::Instant::now();

        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        let (dense_v, dense_i) = dc_output_caps_dense(max_bind, slot.capacity_res);
        let pool_max = device.limits().max_buffer_size.min(max_bind).min(pool.max_group_bytes);

        // ── Phase 4b routing: SINGLE segment (packing ≤ dense ceiling — the pre-4b policies
        // verbatim: churn headroom, settle trim) vs MULTI-SEGMENT (packing exceeds the dense
        // ceiling → the tile table is partitioned into contiguous segments, each an ordinary pool
        // slot in ONE group; segments of one body may live in DIFFERENT groups). The old
        // "exceeds the dense ceiling — monolithic fallback" is GONE: the dense ceiling is a
        // PER-SLOT binding bound, not a surface bound — multi-segment tiling is the path that
        // keeps a >33.5M-vert surface complete AND incremental. Deadband (the owner-log-8
        // hysteresis lesson, now at the single↔multi boundary): once multi, stay multi until the
        // packing drops below 75% of the ceiling — a gesture oscillating across it must not
        // repartition (full re-mesh) every landing.
        let was_multi = pool.segment_count(body) > 1;
        // Single-segment is feasible only when the packing fits BOTH per-slot walls: the dense
        // ceiling (per-slot binding-derived vert bound) AND one group buffer (a slot's region
        // lives wholly in one group). Either exceeded ⇒ multi-segment.
        let over_single = total_v > dense_v || total_i > dense_i
            || total_v.saturating_mul(32) > pool_max || total_i.saturating_mul(4) > pool_max;
        let near_single_limit = total_v > dense_v.saturating_mul(3) / 4
            || total_i > dense_i.saturating_mul(3) / 4
            || total_v.saturating_mul(32) > pool_max.saturating_mul(3) / 4
            || total_i.saturating_mul(4) > pool_max.saturating_mul(3) / 4;
        let go_multi = over_single || (was_multi && near_single_limit);

        let segs: Vec<Vec<crate::sdf_gpu_mesh::SdfTileEntry>> = if !go_multi {
            // Single segment: pack sequentially (identical to the pre-4b layout).
            let mut cur = protos;
            let (mut cv, mut ci) = (0u64, 0u64);
            for t in cur.iter_mut() {
                t.rel_vert = cv;
                t.rel_idx = ci;
                cv += t.vert_cap;
                ci += t.idx_cap;
            }
            vec![cur]
        } else {
            // Partition into contiguous segments filled to ≤80% of the per-group buffer caps —
            // the remaining ≥20% is the C2 incremental grow/birth slack (mirrors the churn-slack
            // idea at segment scale). Each tile lives wholly in one segment (= one group).
            let seg_v_fill = (pool_max / 32).saturating_mul(4) / 5;
            let seg_i_fill = (pool_max / 4).saturating_mul(4) / 5;
            let mut segs = Vec::new();
            let mut cur: Vec<crate::sdf_gpu_mesh::SdfTileEntry> = Vec::new();
            let (mut cv, mut ci) = (0u64, 0u64);
            for mut t in protos {
                if !cur.is_empty() && (cv + t.vert_cap > seg_v_fill || ci + t.idx_cap > seg_i_fill) {
                    segs.push(std::mem::take(&mut cur));
                    cv = 0;
                    ci = 0;
                }
                t.rel_vert = cv;
                t.rel_idx = ci;
                cv += t.vert_cap;
                ci += t.idx_cap;
                cur.push(t);
            }
            if !cur.is_empty() { segs.push(cur); }
            segs
        };
        let nsegs = segs.len() as u32;

        // ── BUDGET envelope (replaces the dense-ceiling envelope): the multi-segment packing
        // must fit the pool's TOTAL region budget (1.25× slack per segment, 32B/vert + 24B/vert
        // idx). Over budget ⇒ LOUD monolithic fallback — and because a monolithic slot clamps at
        // the dense ceiling, a real >ceiling surface WILL truncate there: say so (the self-heal
        // toast precedent lives at the poll layer, which reads `envelope_over_budget`). Same
        // 75%-deadband hysteresis as before, now against the budget.
        if nsegs > 1 {
            let want_bytes: u64 = segs.iter().map(|s| {
                let v: u64 = s.iter().map(|t| t.vert_cap).sum();
                (v * 5 / 4) * (32 + 24)
            }).sum();
            let over = want_bytes > pool.total_budget_bytes;
            let cooling = slot.envelope_fallback
                && want_bytes > pool.total_budget_bytes.saturating_mul(3) / 4;
            if over || cooling {
                if over && !slot.envelope_fallback {
                    eprintln!("[SdfPool] body={body:?} tiled packing (v={total_v}, ~{}MB) exceeds the \
                               {}MB pool budget — monolithic fallback (hysteresis armed){}",
                        want_bytes >> 20, pool.total_budget_bytes >> 20,
                        if total_v > dense_v { "; SURFACE EXCEEDS THE DENSE CEILING — the monolithic \
                             mesh WILL truncate (lower the body resolution)" } else { "" });
                    slot.envelope_over_budget = total_v > dense_v;
                }
                slot.envelope_fallback = true;
                pool.free_segments_from(queue, body, 1);
                return false;
            }
        }
        slot.envelope_fallback = false;
        slot.envelope_over_budget = false;

        // ── Regions + args blocks per segment (grow-only per slot; churn headroom + settle trim
        // on the single-segment path exactly as before; multi-segment sizes tightly at 1.25× —
        // the 20% partition slack is the churn reserve) ─────────────────────────────────────────
        if trim && nsegs == 1 {
            let tv = (total_v.min(dense_v)).max(64);
            if pool.trim_slot(device, queue, body, tv, (tv * 6).min(dense_i)) {
                slot.generation += 1; // region relocated — counters over timing
            }
        }
        for (si, seg_tiles) in segs.iter().enumerate() {
            let sv: u64 = seg_tiles.iter().map(|t| t.vert_cap).sum();
            let sidx = sv * 6;
            let (mut want_v, mut want_i) = if nsegs == 1 {
                let (cur_v, cur_i) = pool.slot(body).map(|s| (s.vert_cap, s.idx_cap)).unwrap_or((0, 0));
                let (mut wv, mut wi) = (sv.min(dense_v), sidx.min(dense_i));
                if !trim {
                    wv = (wv.saturating_mul(5) / 2).min(dense_v).min(pool_max / 32);
                    wi = (wi.saturating_mul(5) / 2).min(dense_i).min(pool_max / 4);
                }
                (wv.max(cur_v), wi.max(cur_i))
            } else {
                ((sv * 5 / 4).min(pool_max / 32), (sidx * 5 / 4).min(pool_max / 4))
            };
            // The region must hold the FULL segment packing even when a multiplier clamped.
            want_v = want_v.max(sv.min(pool_max / 32));
            want_i = want_i.max(sidx.min(pool_max / 4));
            match pool.ensure_slot_seg(device, queue, body, si as u32, want_v, want_i) {
                Ok(relocated) => { if relocated { slot.generation += 1; } }
                Err(()) => {
                    eprintln!("[SdfPool] body={body:?} tiled segment {si}/{nsegs} (v={want_v} i={want_i}) \
                               exceeds the pool cap — monolithic fallback{}",
                        if total_v > dense_v { " (SURFACE EXCEEDS THE DENSE CEILING — the monolithic \
                             mesh WILL truncate; lower the body resolution)" } else { "" });
                    slot.envelope_over_budget = total_v > dense_v;
                    slot.envelope_fallback = true;
                    pool.free_segments_from(queue, body, 1);
                    return false;
                }
            }
            if pool.ensure_tile_block(device, queue, body, si as u32, seg_tiles.len() as u32) {
                slot.generation += 1; // args entries moved
            }
        }
        if let Some(ps) = pool.slot(body) {
            slot.vert_cap = ps.vert_cap;
            slot.idx_cap = ps.idx_cap;
        }
        // Retire stale tail segments from a previous, larger partitioning (dense from 0 stays).
        if pool.segment_count(body) > nsegs {
            pool.free_segments_from(queue, body, nsegs.max(1));
        }
        // Phase 4c: this body meshed QUIET (settle) — release the pool's recycle reserve so a
        // gesture's churn-time retired buffers never linger on a now-stable body (the single-segment
        // trim path drains inside `trim_slot`; a multi-segment body doesn't trim, so drain here).
        if trim { pool.drain_recycle(); }
        let _pool_ms = _t_pool.elapsed().as_secs_f64() * 1000.0;
        let _t_infos = std::time::Instant::now();
        let seg_lens: Vec<u32> = segs.iter().map(|s| s.len() as u32).collect();
        for (si, seg_tiles) in segs.into_iter().enumerate() {
            pool.set_tiles(body, si as u32, seg_tiles);
            pool.write_tile_infos(queue, body, si as u32);
        }
        let _infos_ms = _t_infos.elapsed().as_secs_f64() * 1000.0;
        slot.pooled = true;
        slot.tiled = true;
        slot.lod_level = 0; // tiled path is always full detail (O2 LOD is monolithic-only)
        slot.last_sparse_bounds = Some((grid_min, grid_max));
        // C2 — stamp the provenance this tile table is valid for; a later incremental dispatch
        // reuses it only while res/bounds/atlas generation/brick dims/segmentation ALL match.
        slot.last_tiled_stamp = Some(SdfTiledStamp {
            res, grid_min, grid_max,
            atlas_generation: atlas.generation, dims_bricks: atlas.dims_bricks,
            segments: nsegs,
        });
        slot.last_dispatched_tiles = (n_total, n_total);

        // ── Dispatch every segment (each = the proven batched dispatch against ITS group's
        // buffers; ONE submit per segment). Scratch/params sized once for the largest segment;
        // per-segment params overwrites are queue-ordered against the interleaved submits. ──────
        let max_batch = seg_lens.iter().map(|&n| SDF_TILE_BATCH_MAX.min(n)).max().unwrap_or(1);
        let max_params = seg_lens.iter()
            .map(|&n| n.div_ceil(SDF_TILE_BATCH_MAX.min(n).max(1)) + 1).max().unwrap_or(1);
        pool.ensure_tile_scratch(device, max_batch);
        pool.ensure_tile_params(device, max_params);
        // ONE combined counters staging for the overflow poll (all segments, table order).
        let staging = (slot.pending_tile_counters.is_none()).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_tile_counters_staging"), size: (n_total as u64 * 8).max(8),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false })
        });
        let mut snapshot: Vec<(u64, u32, u32)> = Vec::with_capacity(n_total as usize);
        let mut stage_off = 0u64;
        for si in 0..nsegs {
            let s = pool.slot_seg(body, si).expect("segment just installed");
            snapshot.extend(s.tiles.iter().map(|t| (key(t.tile), t.vert_cap as u32, t.idx_cap as u32)));
            let n_seg = seg_lens[si as usize];
            self.encode_tiled_segment_full(
                device, queue, pool, body, si, atlas, res, grid_min, grid_max, brick_res,
                si == 0, staging.as_ref().map(|st| (st, stage_off)));
            stage_off += n_seg as u64 * 8;
        }
        if let Some(st) = staging {
            slot.kick_tile_counter_readback(st, snapshot);
        }
        let _total = _t_full0.elapsed().as_secs_f64() * 1000.0;
        if _total > 2.0 {
            // Slice D split: table = CPU tile-table build; pool = trim/ensure_slot/ensure_tile_block
            // (region grow/relocate — the rebuild-balloon site); infos = TileInfo uploads.
            eprintln!("[DcResidentSparse] tiled tiles={n_total} segs={nsegs} cpu_total={_total:.2}ms | table={_table_ms:.2} pool={_pool_ms:.2} infos={_infos_ms:.2}");
        }
        slot.note_mesh_content();
        true
    }

    /// Phase 4b — encode + submit ONE segment's full tiled dispatch (clear counters + N batches ×
    /// (vertex, quads) + args-fill) against the segment's GROUP buffers. Extracted verbatim from
    /// the single-segment path so single- and multi-segment bodies share ONE dispatch
    /// implementation. `with_ts` gates the GPU timestamp writes+resolve (segment 0 only — one
    /// query set); `counters_staging` = (buffer, byte offset) for this segment's counter block in
    /// the caller's combined overflow staging. Caller ensured scratch/params capacity.
    #[allow(clippy::too_many_arguments)]
    fn encode_tiled_segment_full(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, pool: &SdfMeshPool,
        body: core_math::scene_graph::NodeId, seg: u32, atlas: &graphics::SdfBrickAtlasGpu,
        res: u32, grid_min: Vec3, grid_max: Vec3, brick_res: u32,
        with_ts: bool, counters_staging: Option<(&wgpu::Buffer, u64)>,
    ) {
        let s = pool.slot_seg(body, seg).expect("segment exists");
        let n = s.tiles.len() as u32;
        if n == 0 { return; }
        let args_base = s.args_base;
        let grp = pool.group(s.group);
        let batch = SDF_TILE_BATCH_MAX.min(n);
        let nb = n.div_ceil(batch);
        // Per-batch params entries + the args-fill entry — all written before this ONE submit
        // (queue.write_buffer is queue-ordered against the previous segment's submit).
        for b in 0..nb {
            let e0 = args_base + b * batch;
            let e1 = args_base + ((b + 1) * batch).min(n);
            let p = dc_params_tiled(res, grid_min, grid_max, brick_res, e0, e1);
            queue.write_buffer(&pool.tile_params, b as u64 * 256, bytemuck::bytes_of(&p));
        }
        let pfill = dc_params_tiled(res, grid_min, grid_max, brick_res, args_base, args_base + n);
        queue.write_buffer(&pool.tile_params, nb as u64 * 256, bytemuck::bytes_of(&pfill));

        let scratch = pool.tile_scratch.as_ref().expect("caller ensured");
        let params_size = std::num::NonZeroU64::new(std::mem::size_of::<DcParamsGpu>() as u64).unwrap();
        let mk_bg = |params_entry: u32, out_idx: &wgpu::Buffer| -> wgpu::BindGroup {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sdf_dc_tiled_bg"), layout: &self.tiled_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.dummy_pool.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &pool.tile_params, offset: params_entry as u64 * 256, size: Some(params_size) }) },
                    wgpu::BindGroupEntry { binding: 2, resource: scratch.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: grp.verts.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: out_idx.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: grp.tile_counters.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: atlas.index_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 7, resource: atlas.pool_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 8, resource: grp.tile_info.as_entire_binding() },
                ],
            })
        };

        let _t0 = std::time::Instant::now();
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sdf_dc_tiled") });
        // Zero this segment's counter block (per-tile atomic pairs).
        enc.clear_buffer(&grp.tile_counters, args_base as u64 * 8, Some(n as u64 * 8));
        let pass_ts = |base: u32| (with_ts).then(|| self.dc_ts.as_ref()).flatten()
            .map(|(qs, _)| wgpu::ComputePassTimestampWrites {
                query_set: qs,
                beginning_of_pass_write_index: Some(base),
                end_of_pass_write_index: Some(base + 1),
            });
        for b in 0..nb {
            let bg = mk_bg(b, &grp.idx);
            let nb_tiles = ((b + 1) * batch).min(n) - b * batch;
            {
                // GPU Timestamp Query Safety: queries 0/1 on the FIRST vertex pass, 2/3 on the
                // LAST quads pass — with nb ≥ 1 all four are always written before the resolve
                // below (same encoder), so an unwritten-query resolve is impossible.
                let ts = if b == 0 { pass_ts(0) } else { None };
                let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("sdf_dc_vertex_tiled"), timestamp_writes: ts });
                p.set_pipeline(&self.pipe_vertex_tiled);
                p.set_bind_group(0, &bg, &[]);
                p.dispatch_workgroups(9 * 9 * 9, nb_tiles, 1);
            }
            {
                let ts = if b == nb - 1 { pass_ts(2) } else { None };
                let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("sdf_dc_quads_tiled"), timestamp_writes: ts });
                p.set_pipeline(&self.pipe_quads_tiled);
                p.set_bind_group(0, &bg, &[]);
                p.dispatch_workgroups(8 * 8 * 8, nb_tiles, 1);
            }
        }
        {
            // Args-fill: publish every tile's DrawIndexedIndirect entry from its counters —
            // binding 4 is the pool's INDIRECT buffer for this pass (u32 view, entry stride 5).
            let bg = mk_bg(nb, &grp.indirect);
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sdf_dc_tile_args"), timestamp_writes: None });
            p.set_pipeline(&self.pipe_tile_args);
            p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(n.div_ceil(64), 1, 1);
        }
        // Overflow readback: this segment's counter block into the caller's combined staging.
        if let Some((st, off)) = counters_staging {
            enc.copy_buffer_to_buffer(&grp.tile_counters, args_base as u64 * 8, st, off, n as u64 * 8);
        }
        // Timestamp resolve + fire-and-forget print (the dispatch_resident_sparse pattern —
        // resolved in this SAME encoder, after the passes that write them). Segment 0 only.
        let ts_staging = (with_ts).then(|| self.dc_ts.as_ref()).flatten().map(|(qs, resolve)| {
            enc.resolve_query_set(qs, 0..4, resolve, 0);
            let st = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_dc_tiled_ts_staging"), size: 32,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false });
            enc.copy_buffer_to_buffer(resolve, 0, &st, 0, 32);
            st
        });
        let _encode_ms = _t0.elapsed().as_secs_f64() * 1000.0;
        let _t_submit = std::time::Instant::now();
        queue.submit(std::iter::once(enc.finish()));
        let _submit_ms = _t_submit.elapsed().as_secs_f64() * 1000.0;
        if let Some(st) = ts_staging {
            let period_ns = queue.get_timestamp_period() as f64;
            let st = std::sync::Arc::new(st);
            let cb = st.clone();
            let (n_tiles, batches) = (n, nb);
            st.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                if r.is_err() { return; }
                let (vert_ms, quads_ms) = {
                    let data = cb.slice(..).get_mapped_range();
                    let t: &[u64] = bytemuck::cast_slice(&data);
                    let ms = |a: u64, b: u64| if b >= a && a > 0 { (b - a) as f64 * period_ns / 1.0e6 } else { 0.0 };
                    (ms(t[0], t[1]), ms(t[2], t[3]))
                };
                cb.unmap();
                if vert_ms + quads_ms > 1.0 {
                    eprintln!("[DcResidentSparse] GPU tiled vert(b0)={vert_ms:.2}ms quads(bN)={quads_ms:.2}ms tiles={n_tiles}/{n_tiles} batches={batches}");
                }
            });
        }
        if _encode_ms + _submit_ms > 2.0 {
            eprintln!("[DcResidentSparse] tiled seg={seg} tiles={n} batches={nb} encode={_encode_ms:.2} submit={_submit_ms:.2}");
        }
    }

    /// C2 — INCREMENTAL tiled re-mesh: re-dispatch ONLY the tiles overlapping the dirty window
    /// (dilated to the tile's full field support: 1-cell vertex halo + trilinear/gradient reach),
    /// keeping every clean tile's region/args/TileInfo/counters byte-untouched. The result is
    /// BIT-IDENTICAL to a full tiled re-mesh of the same atlas (gate:
    /// `sdf_tiled_incremental_matches_full`) because tiles are self-contained index spaces (C1
    /// halo duplication) — rewriting a dirty tile's region can never dangle a neighbor.
    ///
    /// Dispatch mechanism (option (a) of the C2 brief): the dirty tiles' TileInfos are COMPACTED
    /// into `pool.tile_info_stage` (bound at binding 8 instead of the shared table) and the params
    /// range over `[0, n_dirty)`; each staged info's `tmeta.z` carries the tile's REAL entry
    /// index, which the shader uses for counters and the args slot — so a non-contiguous dirty
    /// set dispatches as one contiguous scratch range with zero disturbance to the entry space.
    ///
    /// Returns `Ok(())` when the incremental dispatch (possibly zero tiles) fully handled the
    /// mesh; `Err(None)` = fall back to the full tiled re-mesh (stale stamp / capacity);
    /// `Err(Some(reason))` = CONTRACT VIOLATION (occupancy changed outside the window) — full
    /// re-mesh + a diagnostic, never silent wrongness.
    #[allow(clippy::too_many_arguments)]
    fn mesh_tiled_incremental(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut SdfBodyMeshSlot,
        atlas: &graphics::SdfBrickAtlasGpu, grid_min: Vec3, grid_max: Vec3,
        body: core_math::scene_graph::NodeId, pool: &mut SdfMeshPool,
        dirty: &SdfTileDirty,
    ) -> Result<(), Option<String>> {
        let res = slot.res;
        let cells = res - 1;
        let bd = graphics::sdf_brick_atlas::BRICK_FLOATS_DIM;
        let brick_res = atlas.dims_bricks[0];

        // ── Staleness guards: ANY mismatch ⇒ full re-mesh (conservative by construction). ──────
        if !(slot.tiled && slot.pooled) { return Err(None); }
        let nsegs = pool.segment_count(body);
        if nsegs == 0 { return Err(None); }
        let stamp = SdfTiledStamp {
            res, grid_min, grid_max,
            atlas_generation: atlas.generation, dims_bricks: atlas.dims_bricks,
            segments: nsegs,
        };
        if slot.last_tiled_stamp != Some(stamp) { return Err(None); }
        if atlas.tile_stats.is_empty() { return Err(None); } // surface vanished — empty arm
        let total_live: usize = (0..nsegs)
            .map(|s| pool.slot_seg(body, s).map_or(0, |x| x.tiles.len())).sum();
        if total_live == 0 { return Err(None); }

        // ── Dirty predicate at TILE granularity. A tile's output depends on voxels
        // [32T − 3, 32T + 33] per axis (anchor window 32 cells + 1-cell vertex halo below; a
        // cell's QEF/normal reads reach ±2 voxels past its corners via trilinear + gradient
        // sampling) — overlap that support with the changed-voxel window [wmin, wmax). ─────────
        let explicit: Option<std::collections::HashSet<u64>> = match dirty {
            SdfTileDirty::Tiles(ks) => Some(ks.iter().copied().collect()),
            SdfTileDirty::Voxels(..) => None,
        };
        let (wmin, wmax) = match dirty {
            SdfTileDirty::Voxels(a, b) => (*a, *b),
            SdfTileDirty::Tiles(_) => ([0; 3], [0; 3]),
        };
        if matches!(dirty, SdfTileDirty::Voxels(..)) && (0..3).any(|a| wmin[a] >= wmax[a]) {
            // Empty window — nothing changed. Zero dispatches, mesh bit-untouched.
            slot.last_dispatched_tiles = (0, total_live as u32);
            return Ok(());
        }
        let tile_dirty = |t: [u32; 3]| -> bool {
            match &explicit {
                Some(set) => set.contains(&tile_key(t)),
                None => (0..3).all(|a| {
                    let lo = t[a] as i64 * 32 - 3;
                    let hi = t[a] as i64 * 32 + 33;
                    (wmin[a] as i64) <= hi && (wmax[a] as i64) - 1 >= lo
                }),
            }
        };

        // ── PLAN phase (no mutation until every check passes — a mid-way bail must leave the
        // slot exactly as it was, so the full path can take over cleanly). Phase 4b: the plan
        // runs across ALL segments — a tile's segment is part of its identity (its entry space),
        // so remesh/death land in their own segment; births go to any segment with slack. ───────
        struct Remesh {
            seg: u32,
            pos: usize,
            amin: [u32; 3],
            amax: [u32; 3],
            bricks: u32,
            halo: u32,
            need: u64,
            grow: bool,
        }
        let headroom = self.tile_verts_per_brick_headroom;
        let mut stat_by_key: std::collections::HashMap<u64, &graphics::sdf_brick_atlas::SdfTileStat> =
            atlas.tile_stats.iter().map(|st| (tile_key(st.tile), st)).collect();
        let mut remesh: Vec<Remesh> = Vec::new();
        let mut deaths: Vec<(u32, usize)> = Vec::new();
        // Per-segment plan state: (args_base, n_before, region_vcap, region_icap, args_cap,
        // watermarks, group).
        struct SegState {
            args_base: u32,
            n_before: usize,
            vcap: u64,
            icap: u64,
            args_cap: u32,
            wm: (u64, u64),
            group: usize,
        }
        let seg_state: Vec<SegState> = (0..nsegs).map(|si| {
            let s = pool.slot_seg(body, si).expect("checked above");
            SegState { args_base: s.args_base, n_before: s.tiles.len(), vcap: s.vert_cap,
                       icap: s.idx_cap, args_cap: s.args_cap, wm: s.tile_watermarks(),
                       group: s.group }
        }).collect();
        for si in 0..nsegs {
            let s = pool.slot_seg(body, si).expect("checked above");
            for (i, e) in s.tiles.iter().enumerate() {
                let k = tile_key(e.tile);
                let in_dirty = tile_dirty(e.tile);
                match stat_by_key.remove(&k) {
                    Some(st) => {
                        let (amin, amax) = tile_window(st, bd, cells);
                        let occ_changed = e.dead || st.bricks != e.bricks
                            || st.halo_bricks != e.halo_bricks || amin != e.amin || amax != e.amax;
                        if occ_changed && !in_dirty {
                            return Err(Some(format!(
                                "tile {:?} occupancy changed outside the dirty window", e.tile)));
                        }
                        if in_dirty {
                            let floor = slot.tile_cap_floor.get(&k).copied().unwrap_or(0) as u64;
                            let need = (st.halo_bricks as u64 * headroom)
                                .max(floor).clamp(32, SDF_TILE_MAX_VERTS);
                            remesh.push(Remesh {
                                seg: si, pos: i, amin, amax, bricks: st.bricks,
                                halo: st.halo_bricks, need, grow: need > e.vert_cap,
                            });
                        }
                    }
                    None => {
                        if e.dead { continue; } // already tombstoned — stays dead
                        if !in_dirty {
                            return Err(Some(format!(
                                "tile {:?} died outside the dirty window", e.tile)));
                        }
                        deaths.push((si, i));
                    }
                }
            }
        }
        // Leftover stats = NEW tiles. Deterministic order (HashMap iteration is random — CLAUDE.md).
        let mut births: Vec<graphics::sdf_brick_atlas::SdfTileStat> =
            stat_by_key.into_values().copied().collect();
        births.sort_unstable_by_key(|st| (st.tile[2], st.tile[1], st.tile[0]));
        for st in &births {
            if !tile_dirty(st.tile) {
                return Err(Some(format!(
                    "tile {:?} appeared outside the dirty window", st.tile)));
            }
        }
        // Capacity feasibility: grows/births tail-append into their segment's region slack (the
        // churn headroom the sizing left); births also need spare args entries. Insufficient
        // slack in the required segment ⇒ full re-mesh (which repartitions + grows through the
        // proven machinery). Grows are charged to the tile's OWN segment; births to the LAST
        // segment with room (last-first — the partition fills front-to-back, so slack pools at
        // the tail).
        let mut wms: Vec<(u64, u64)> = seg_state.iter().map(|s| s.wm).collect();
        let mut counts: Vec<usize> = seg_state.iter().map(|s| s.n_before).collect();
        for r in remesh.iter().filter(|r| r.grow) {
            let si = r.seg as usize;
            if wms[si].0 + r.need > seg_state[si].vcap
                || wms[si].1 + r.need * 6 > seg_state[si].icap { return Err(None); }
            wms[si].0 += r.need;
            wms[si].1 += r.need * 6;
        }
        let mut birth_plan: Vec<(u32, u64)> = Vec::with_capacity(births.len()); // (seg, need)
        for st in &births {
            let floor = slot.tile_cap_floor.get(&tile_key(st.tile)).copied().unwrap_or(0) as u64;
            let need = (st.halo_bricks as u64 * headroom).max(floor).clamp(32, SDF_TILE_MAX_VERTS);
            let mut placed = None;
            for si in (0..nsegs as usize).rev() {
                if counts[si] + 1 <= seg_state[si].args_cap as usize
                    && wms[si].0 + need <= seg_state[si].vcap
                    && wms[si].1 + need * 6 <= seg_state[si].icap {
                    placed = Some(si);
                    break;
                }
            }
            let Some(si) = placed else { return Err(None); };
            counts[si] += 1;
            wms[si].0 += need;
            wms[si].1 += need * 6;
            birth_plan.push((si as u32, need));
        }

        if remesh.is_empty() && deaths.is_empty() && births.is_empty() {
            // Dirty window overlaps no live tiles and no tile was born/died: zero dispatches.
            slot.last_dispatched_tiles = (0, total_live as u32);
            return Ok(());
        }

        // ── APPLY: mutate the CPU tile tables (every check passed — no bail from here on). ─────
        let mut dirty_by_seg: Vec<Vec<usize>> = vec![Vec::new(); nsegs as usize];
        {
            let mut wms: Vec<(u64, u64)> = seg_state.iter().map(|s| s.wm).collect();
            for r in &remesh {
                let tiles = pool.tiles_mut(body, r.seg).expect("checked above");
                let e = &mut tiles[r.pos];
                e.amin = r.amin;
                e.amax = r.amax;
                e.bricks = r.bricks;
                e.halo_bricks = r.halo;
                e.dead = false;
                if r.grow {
                    // Tail-append: the old sub-region becomes a hole (repacked by the next full
                    // re-mesh); neighbors' regions are untouched by construction.
                    let si = r.seg as usize;
                    e.rel_vert = wms[si].0;
                    e.vert_cap = r.need;
                    e.rel_idx = wms[si].1;
                    e.idx_cap = r.need * 6;
                    wms[si].0 += r.need;
                    wms[si].1 += r.need * 6;
                }
                dirty_by_seg[r.seg as usize].push(r.pos);
            }
            for (st, (bseg, need)) in births.iter().zip(&birth_plan) {
                let (amin, amax) = tile_window(st, bd, cells);
                let si = *bseg as usize;
                let tiles = pool.tiles_mut(body, *bseg).expect("checked above");
                tiles.push(crate::sdf_gpu_mesh::SdfTileEntry {
                    tile: st.tile, amin, amax,
                    rel_vert: wms[si].0, vert_cap: *need,
                    rel_idx: wms[si].1, idx_cap: need * 6,
                    bricks: st.bricks, halo_bricks: st.halo_bricks, dead: false,
                });
                wms[si].0 += *need;
                wms[si].1 += need * 6;
                dirty_by_seg[si].push(tiles.len() - 1);
            }
            for &(si, d) in &deaths {
                pool.tiles_mut(body, si).expect("checked above")[d].dead = true;
            }
        }
        // Dead tiles: zero the whole args entry (index_count AND instance_count — nothing can
        // draw it, even via a stale offset). Their counters/regions become inert holes.
        for &(si, d) in &deaths {
            let st = &seg_state[si as usize];
            queue.write_buffer(&pool.group(st.group).indirect,
                (st.args_base as u64 + d as u64) * SDF_POOL_INDIRECT_STRIDE,
                bytemuck::cast_slice(&[0u32; 5]));
        }
        // Keep the SHARED TileInfo tables in sync with the CPU truth for the dirty tiles (the
        // dispatch itself reads the compacted stage, but rebuild()/full re-dispatch read this).
        for (si, dp) in dirty_by_seg.iter().enumerate() {
            for &p in dp {
                pool.write_tile_info_at(queue, body, si as u32, p);
            }
        }

        let nd: u32 = dirty_by_seg.iter().map(|d| d.len() as u32).sum();
        let n_total: u32 = (0..nsegs)
            .map(|s| pool.slot_seg(body, s).map_or(0, |x| x.tiles.len() as u32)).sum();
        slot.last_dispatched_tiles = (nd, n_total);
        if nd == 0 {
            return Ok(()); // deaths only — args zeroed above, nothing to dispatch
        }

        // ── Compacted dispatch per segment with dirty tiles: stage that segment's dirty
        // TileInfos, zero their counters, run the three tiled passes over the contiguous scratch
        // range [0, nd_seg) against the segment's GROUP buffers. ONE combined counters staging
        // (snapshot order = segment order). ─────────────────────────────────────────────────────
        let staging = (slot.pending_tile_counters.is_none()).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_tile_counters_staging"), size: (nd as u64 * 8).max(8),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false })
        });
        let mut snapshot: Vec<(u64, u32, u32)> = Vec::with_capacity(nd as usize);
        let mut stage_off = 0u64;
        let mut first_dispatch = true;
        let _t0 = std::time::Instant::now();
        for si in 0..nsegs {
            let dp = std::mem::take(&mut dirty_by_seg[si as usize]);
            if dp.is_empty() { continue; }
            {
                let s = pool.slot_seg(body, si).expect("checked above");
                snapshot.extend(dp.iter().map(|&p| {
                    let t = &s.tiles[p];
                    (tile_key(t.tile), t.vert_cap as u32, t.idx_cap as u32)
                }));
            }
            pool.stage_dirty_tile_infos(device, queue, body, si, &dp);
            let nd_seg = dp.len() as u32;
            let batch = SDF_TILE_BATCH_MAX.min(nd_seg);
            pool.ensure_tile_scratch(device, batch);
            pool.ensure_tile_params(device, nd_seg.div_ceil(batch) + 1);
            self.encode_tiled_segment_incremental(
                device, queue, pool, body, si, &dp, atlas, res, grid_min, grid_max, brick_res,
                first_dispatch, nd, n_total,
                staging.as_ref().map(|st| (st, stage_off)));
            stage_off += dp.len() as u64 * 8;
            first_dispatch = false;
        }
        if let Some(st) = staging {
            slot.kick_tile_counter_readback(st, snapshot);
        }
        let _total = _t0.elapsed().as_secs_f64() * 1000.0;
        if _total > 2.0 {
            eprintln!("[DcResidentSparse] tiled INC tiles={nd}/{n_total} cpu_total={_total:.2}ms");
        }
        Ok(())
    }

    /// Phase 4b — encode + submit ONE segment's INCREMENTAL tiled dispatch: the segment's dirty
    /// TileInfos are already compacted into `pool.tile_info_stage` (bound at binding 8); params
    /// range over [0, nd_seg); each staged info's `tmeta.z` routes counters/args to the live
    /// entry space of the segment's GROUP. `with_ts` gates the timestamp writes+resolve (first
    /// dispatched segment only — one query set). `counters_staging` = (buffer, byte offset) for
    /// this segment's dirty-counter block in the caller's combined overflow staging.
    #[allow(clippy::too_many_arguments)]
    fn encode_tiled_segment_incremental(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, pool: &SdfMeshPool,
        body: core_math::scene_graph::NodeId, seg: u32, dirty_pos: &[usize],
        atlas: &graphics::SdfBrickAtlasGpu, res: u32, grid_min: Vec3, grid_max: Vec3,
        brick_res: u32, with_ts: bool, nd_all: u32, n_total: u32,
        counters_staging: Option<(&wgpu::Buffer, u64)>,
    ) {
        let s = pool.slot_seg(body, seg).expect("segment exists");
        let (args_base, group) = (s.args_base, s.group);
        let nd = dirty_pos.len() as u32;
        let batch = SDF_TILE_BATCH_MAX.min(nd);
        let nb = nd.div_ceil(batch);
        for b in 0..nb {
            let e0 = b * batch;
            let e1 = ((b + 1) * batch).min(nd);
            let p = dc_params_tiled(res, grid_min, grid_max, brick_res, e0, e1);
            queue.write_buffer(&pool.tile_params, b as u64 * 256, bytemuck::bytes_of(&p));
        }
        let pfill = dc_params_tiled(res, grid_min, grid_max, brick_res, 0, nd);
        queue.write_buffer(&pool.tile_params, nb as u64 * 256, bytemuck::bytes_of(&pfill));

        let stage = pool.tile_info_stage.as_ref().expect("staged by caller");
        let scratch = pool.tile_scratch.as_ref().expect("ensured by caller");
        let grp = pool.group(group);
        let params_size = std::num::NonZeroU64::new(std::mem::size_of::<DcParamsGpu>() as u64).unwrap();
        let mk_bg = |params_entry: u32, out_idx: &wgpu::Buffer| -> wgpu::BindGroup {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sdf_dc_tiled_inc_bg"), layout: &self.tiled_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.dummy_pool.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &pool.tile_params, offset: params_entry as u64 * 256, size: Some(params_size) }) },
                    wgpu::BindGroupEntry { binding: 2, resource: scratch.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: grp.verts.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: out_idx.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: grp.tile_counters.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: atlas.index_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 7, resource: atlas.pool_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 8, resource: stage.as_entire_binding() },
                ],
            })
        };

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sdf_dc_tiled_incremental") });
        // Zero ONLY the dirty tiles' counter pairs (coalesced into contiguous entry runs).
        {
            let mut run_start: Option<(usize, usize)> = None; // (first pos, last pos)
            let mut flush = |enc: &mut wgpu::CommandEncoder, run: Option<(usize, usize)>| {
                if let Some((a, b)) = run {
                    enc.clear_buffer(&grp.tile_counters,
                        (args_base as u64 + a as u64) * 8, Some((b - a + 1) as u64 * 8));
                }
            };
            for &p in dirty_pos {
                run_start = match run_start {
                    Some((a, b)) if p == b + 1 => Some((a, p)),
                    Some(run) => { flush(&mut enc, Some(run)); Some((p, p)) }
                    None => Some((p, p)),
                };
            }
            flush(&mut enc, run_start);
        }
        let pass_ts = |base: u32| (with_ts).then(|| self.dc_ts.as_ref()).flatten()
            .map(|(qs, _)| wgpu::ComputePassTimestampWrites {
                query_set: qs,
                beginning_of_pass_write_index: Some(base),
                end_of_pass_write_index: Some(base + 1),
            });
        for b in 0..nb {
            let bg = mk_bg(b, &grp.idx);
            let nb_tiles = ((b + 1) * batch).min(nd) - b * batch;
            {
                // GPU Timestamp Query Safety: with nb ≥ 1 all four queries are written before the
                // resolve below in this SAME encoder (the C1 pattern).
                let ts = if b == 0 { pass_ts(0) } else { None };
                let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("sdf_dc_vertex_tiled_inc"), timestamp_writes: ts });
                p.set_pipeline(&self.pipe_vertex_tiled);
                p.set_bind_group(0, &bg, &[]);
                p.dispatch_workgroups(9 * 9 * 9, nb_tiles, 1);
            }
            {
                let ts = if b == nb - 1 { pass_ts(2) } else { None };
                let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("sdf_dc_quads_tiled_inc"), timestamp_writes: ts });
                p.set_pipeline(&self.pipe_quads_tiled);
                p.set_bind_group(0, &bg, &[]);
                p.dispatch_workgroups(8 * 8 * 8, nb_tiles, 1);
            }
        }
        {
            let bg = mk_bg(nb, &grp.indirect);
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sdf_dc_tile_args_inc"), timestamp_writes: None });
            p.set_pipeline(&self.pipe_tile_args);
            p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(nd.div_ceil(64), 1, 1);
        }
        // Overflow readback: ONLY this segment's dirty tiles' counters (into the caller's
        // combined staging, snapshot order).
        if let Some((st, off)) = counters_staging {
            for (i, &p) in dirty_pos.iter().enumerate() {
                enc.copy_buffer_to_buffer(&grp.tile_counters,
                    (args_base as u64 + p as u64) * 8, st, off + i as u64 * 8, 8);
            }
        }
        let ts_staging = (with_ts).then(|| self.dc_ts.as_ref()).flatten().map(|(qs, resolve)| {
            enc.resolve_query_set(qs, 0..4, resolve, 0);
            let st = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_dc_tiled_ts_staging"), size: 32,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false });
            enc.copy_buffer_to_buffer(resolve, 0, &st, 0, 32);
            st
        });
        queue.submit(std::iter::once(enc.finish()));
        if let Some(st) = ts_staging {
            let period_ns = queue.get_timestamp_period() as f64;
            let st = std::sync::Arc::new(st);
            let cb = st.clone();
            st.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                if r.is_err() { return; }
                let (vert_ms, quads_ms) = {
                    let data = cb.slice(..).get_mapped_range();
                    let t: &[u64] = bytemuck::cast_slice(&data);
                    let ms = |a: u64, b: u64| if b >= a && a > 0 { (b - a) as f64 * period_ns / 1.0e6 } else { 0.0 };
                    (ms(t[0], t[1]), ms(t[2], t[3]))
                };
                cb.unmap();
                // THE deliverable metric: the incremental batches — always printed (they are the
                // small dispatches the >1ms gate would hide).
                eprintln!("[DcResidentSparse] GPU tiled INC vert={vert_ms:.3}ms quads={quads_ms:.3}ms tiles={nd_all}/{n_total}");
            });
        }
    }

    /// O2 LOD: mesh a body at octree `level` (cells span 1<<level voxels) from the SAME brick
    /// atlas — no re-classify, no re-bake, and no cracks (the whole body sits at one level).
    /// Level 0 = the monolithic full mesh verbatim. UNPOOLED by design: LOD bodies are the
    /// few/distant ones, drawn per-body via `draw_sdf_resident_slot`; the active body keeps the
    /// tiled incremental path at level 0. Output caps scale ÷4 per level (surface area in cells);
    /// overflow is handled by the existing counter readback + grow machinery.
    /// CALL ONLY FROM poll-phase code (Frame Ordering Contract).
    ///
    /// O2b: WINDOWED to the atlas's surface-brick bbox exactly like `mesh_into_slot_sparse` —
    /// the O2a full-grid sweep dispatched (res−1)³ threads (26ms GPU at 1024); the surface window
    /// is a small fraction of that. `window` = explicit LEVEL-0-cell window override (the
    /// windowed-vs-full parity gate passes `lod_full_window(res)`); `None` = production (derive
    /// from `surface_brick_{min,max}`, empty surface → zeroed counters + indirect, no dispatch).
    pub(crate) fn mesh_into_slot_sparse_lod(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut SdfBodyMeshSlot,
        atlas: &graphics::SdfBrickAtlasGpu, grid_min: Vec3, grid_max: Vec3, level: u32,
        window: Option<([u32; 3], [u32; 3])>,
    ) {
        let res = slot.res;
        assert_eq!(atlas.res, res, "SdfBrickAtlasGpu content res != slot res — upload before dispatch");
        // Surface-proportional caps at this level: cells shrink 4× per level in surface count.
        let (sv0, si0) = dc_output_caps_sparse(atlas.allocated_bricks as u64, self.tile_verts_per_brick_headroom);
        let shrink = 1u64 << (2 * level.min(8));
        let (sv, si) = ((sv0 / shrink).max(256), (si0 / shrink).max(1536));
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        let (dense_v, dense_i) = {
            let d = dc_output_caps_dense(max_bind, slot.capacity_res.min(crate::gpu_jobs::max_gridbacked_resolution()));
            if d.0 == 0 {
                dc_output_caps_dense(max_bind, crate::gpu_jobs::max_gridbacked_resolution())
            } else {
                d
            }
        };
        // cell_vert stays brick-local (allocated×512 — the L0 sizing; LOD reuses the same slots).
        let Some(cv_need) = dc_cell_vert_cap_sparse(atlas.allocated_bricks as u64, max_bind) else {
            eprintln!(
                "[DcBind] REJECT sparse LOD mesh res={}³ level={} bricks={} — cell_vert over bind limit",
                res, level, atlas.allocated_bricks
            );
            return;
        };
        slot.sanitize_cell_vert_if_poisoned(device);
        if !slot.ensure_output_caps(device, sv.min(dense_v), si.min(dense_i), cv_need) {
            eprintln!("[DcBind] REJECT sparse LOD ensure_output_caps res={}³", res);
            return;
        }
        slot.pooled = false; // per-body draw; the pool entry self-skips per frame
        slot.tiled = false;
        slot.last_tiled_stamp = None;
        slot.last_sparse_bounds = Some((grid_min, grid_max));
        slot.lod_level = level;
        // Window in LEVEL-0 cells: surface-brick bbox (the mesh_into_slot_sparse window) unless
        // the caller passed an explicit one. Empty surface = nothing to mesh (same contract as
        // the L0 monolithic path): zero the counters + indirect index_count and return.
        let (cmin, cmax) = match window.or_else(|| surface_cell_window(atlas, res)) {
            Some(w) => w,
            None => {
                queue.write_buffer(&slot.counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));
                queue.write_buffer(&slot.indirect, 0, bytemuck::cast_slice(&[0u32]));
                slot.note_mesh_content();
                return;
            }
        };
        let (p, vertex_ext, quad_ext) =
            dc_params_sparse_lod(res, grid_min, grid_max, atlas.dims_bricks[0], level, cmin, cmax);
        queue.write_buffer(&slot.params, 0, bytemuck::bytes_of(&p));
        let staging = (slot.pending_counters.is_none()).then(|| device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_slot_counters_staging"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
        self.dispatch_resident_sparse(
            device, queue, &self.dummy_index, &atlas.index_buf, &atlas.pool_buf, &slot.params, &slot.cell_vert,
            slot.verts.as_entire_binding(), slot.idx.as_entire_binding(), &slot.counters, &slot.indirect, 0,
            vertex_ext, quad_ext, staging.as_ref());
        if let Some(staging) = staging {
            slot.kick_counter_readback(staging);
        }
        slot.note_mesh_content();
    }

    /// Sparse twin of `dispatch_resident`: the DC mesher reads the field from the brick atlas
    /// (`brick_index` tags + `brick_pool` arena, bindings 6/7) instead of the dense res³ buffer. `params`
    /// MUST carry `brick_res` in `gext.w` (use `dc_params_sparse`). Bind group is built fresh over the
    /// passed buffers, so nothing external dangles (no device-loss class).
    fn dispatch_resident_sparse(
        &self, device: &wgpu::Device, queue: &wgpu::Queue,
        field_unused: &wgpu::Buffer, brick_index: &wgpu::Buffer, brick_pool: &wgpu::Buffer,
        params: &wgpu::Buffer, cell_vert: &wgpu::Buffer,
        verts: wgpu::BindingResource, idx: wgpu::BindingResource, counters: &wgpu::Buffer,
        indirect: &wgpu::Buffer, indirect_off: u64,
        vertex_ext: [u32; 3], quad_ext: [u32; 3], counters_staging: Option<&wgpu::Buffer>,
    ) {
        // [DcResidentSparse] split timing (P0-5) — the sparse twin of [DcResident] above, same >2ms
        // gate. Also prints the windowed extents: `ext` growing toward the full grid is review finding
        // R2 (surface bbox ≈ whole volume ⇒ windowing degenerates) — the datum that decides whether
        // 3B-S3 (tiled incremental DC) is the real term or the window suffices.
        let _t0 = std::time::Instant::now();
        queue.write_buffer(counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));
        let bg = dc_bind_group(device, &self.bgl, field_unused, params, cell_vert, verts, idx, counters, brick_index, brick_pool);
        let _t_bg = _t0.elapsed().as_secs_f64() * 1000.0;
        let _t1 = std::time::Instant::now();
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sdf_dc_resident_sparse") });
        encode_dc_passes_timed(&mut enc, &self.pipe_vertex, &self.pipe_quads, &bg, vertex_ext, quad_ext,
            self.dc_ts.as_ref().map(|(qs, _)| qs));
        enc.copy_buffer_to_buffer(counters, 4, indirect, indirect_off, 4);
        // R3b: 8-byte counters snapshot for the overflow poll (same submit — no extra latency).
        if let Some(staging) = counters_staging {
            enc.copy_buffer_to_buffer(counters, 0, staging, 0, 8);
        }
        // R2 diagnostic: resolve the 4 timestamps written by encode_dc_passes_timed above — SAME
        // encoder, so they are provably written before this resolve (never the unwritten-query
        // device-loss class). Copy into a fresh 32-byte staging (the counters-staging precedent:
        // tiny per-dispatch buffer) and print fire-and-forget from the map_async callback — no poll
        // site to wire; the app's per-frame device.poll (or a test's poll(Wait)) delivers it 1-2
        // frames late, which is fine for a diagnostic.
        let ts_staging = self.dc_ts.as_ref().map(|(qs, resolve)| {
            enc.resolve_query_set(qs, 0..4, resolve, 0);
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_dc_sparse_ts_staging"), size: 32,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            enc.copy_buffer_to_buffer(resolve, 0, &staging, 0, 32);
            staging
        });
        let _t_enc = _t1.elapsed().as_secs_f64() * 1000.0;
        let _t2 = std::time::Instant::now();
        queue.submit(std::iter::once(enc.finish()));
        let _t_submit = _t2.elapsed().as_secs_f64() * 1000.0;
        if let Some(staging) = ts_staging {
            let period_ns = queue.get_timestamp_period() as f64;
            let staging = std::sync::Arc::new(staging);
            let cb_buf = staging.clone();
            staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                if r.is_err() { return; }
                let (vert_ms, quads_ms) = {
                    let data = cb_buf.slice(..).get_mapped_range();
                    let t: &[u64] = bytemuck::cast_slice(&data);
                    let ms = |a: u64, b: u64| if b >= a && a > 0 { (b - a) as f64 * period_ns / 1.0e6 } else { 0.0 };
                    (ms(t[0], t[1]), ms(t[2], t[3]))
                };
                cb_buf.unmap();
                // >1ms gate: silent for small-window dispatches, always fires if the R2 term
                // (est. 15-40ms full-surface re-mesh) is real.
                if vert_ms + quads_ms > 1.0 {
                    eprintln!("[DcResidentSparse] GPU vert={vert_ms:.2}ms quads={quads_ms:.2}ms ext={}x{}x{}",
                        vertex_ext[0], vertex_ext[1], vertex_ext[2]);
                }
            });
        }
        let _total = _t_bg + _t_enc + _t_submit;
        if _total > 2.0 {
            eprintln!("[DcResidentSparse] ext={}x{}x{} total={:.2}ms (bind_group={:.2} encode={:.2} submit={:.2})",
                vertex_ext[0], vertex_ext[1], vertex_ext[2], _total, _t_bg, _t_enc, _t_submit);
        }
    }
}

/// The WINDOWED-DC anchor window in LEVEL-0 cells (voxel coordinates), derived from the atlas's
/// surface-brick bbox: the iso surface lives entirely in the HANDLE bricks (surface + apron), so
/// meshing only their cell bbox is bit-identical to the full-grid sweep (gates:
/// `sdf_dc_sparse_windowed_matches_full` L0, `sdf_lod_windowed_matches_full_grid` at level).
/// `None` = the atlas has no handle bricks (empty surface — nothing to mesh). Shared by the
/// monolithic L0 entry and the O2 LOD entry (one definition of the window math).
pub(crate) fn surface_cell_window(
    atlas: &graphics::SdfBrickAtlasGpu, res: u32,
) -> Option<([u32; 3], [u32; 3])> {
    let (bmin, bmax) = (atlas.surface_brick_min, atlas.surface_brick_max);
    if (0..3).any(|a| bmin[a] > bmax[a]) { return None; }
    let bd = graphics::sdf_brick_atlas::BRICK_FLOATS_DIM; // 8 voxels/brick per axis
    let cells = res - 1;
    let cmin = [bmin[0] * bd, bmin[1] * bd, bmin[2] * bd];
    let cmax = [
        ((bmax[0] + 1) * bd - 1).min(cells - 1),
        ((bmax[1] + 1) * bd - 1).min(cells - 1),
        ((bmax[2] + 1) * bd - 1).min(cells - 1),
    ];
    Some((cmin, cmax))
}

/// G1 (Phase G): a per-body GPU-resident SDF mesh slot — owns the full buffer set the QEF-DC mesher
/// writes. `verts`/`idx` are a drawable vertex+index pool; `indirect` holds the `DrawIndexedIndirect`
/// args (index_count GPU-written, instance_count seeded to 1); `field`/`params`/`cell_vert`/`counters`
/// are scratch. ALL buffers are slot-owned, so the dispatch bind group references nothing external →
/// no dangling on a `ResidentVolume` realloc (no device-loss class). Allocated ONLY in
/// `poll_background_tasks` (Frame Ordering Contract). G2 draws `verts`/`idx` via the indirect args.
#[allow(dead_code)] // fields read by the poll-phase populate + indirect draw in G2.
pub(crate) struct SdfBodyMeshSlot {
    /// Dense res³ signed distance — LAZY (P0-R3a): 512MiB at 512³ and read ONLY by the dense
    /// CPU-grid fallback path (`mesh_into_slot`). The production paths never touch it —
    /// field-direct meshes from `rv.buffer`, sparse meshes from the brick atlas (binding 0 gets
    /// the 4-byte dummy) — so a brick-resident 512 body used to carry half a gigabyte of dead
    /// weight (the review's VRAM/crash suspect). Allocated on first dense use via
    /// `ensure_field` (poll-phase callers only — Frame Ordering Contract holds).
    pub(crate) field: Option<wgpu::Buffer>,
    pub(crate) params: wgpu::Buffer,    // DcParams uniform
    /// Scratch (per-cell vertex index). R3c dual-sized like the shader's dual addressing: dense
    /// dispatches size it cells³ (region-local addressing); sparse dispatches size it
    /// allocated_bricks×512 (brick-local addressing) — surface-proportional, the last dense mesh
    /// buffer that blocked 1024³. 4-byte placeholder until the first dispatch sizes it.
    pub(crate) cell_vert: wgpu::Buffer,
    pub(crate) verts: wgpu::Buffer,     // STORAGE|VERTEX|COPY_SRC — drawn in G2
    pub(crate) idx: wgpu::Buffer,       // STORAGE|INDEX|COPY_SRC — drawn in G2
    pub(crate) counters: wgpu::Buffer,  // atomic [vert_count, index_count]
    pub(crate) indirect: wgpu::Buffer,  // INDIRECT|COPY_DST|COPY_SRC — DrawIndexedIndirect args (5×u32)
    /// R3b — LOGICAL output capacities in ELEMENT counts (vertices / u32 indices) the buffers were
    /// last sized for. Grow-only within a slot's lifetime (never shrink mid-session — the 512↔256
    /// oscillation precedent). 0 until the first dispatch sizes them (`ensure_output_caps`).
    pub(crate) vert_cap: u64,
    pub(crate) idx_cap: u64,
    /// R3c — logical `cell_vert` capacity in u32 ELEMENTS (cells for dense, handles×512 for
    /// sparse). Same grow-only + generation pattern as vert_cap/idx_cap. NOT clamped to the dense
    /// ceiling: the sparse brick-local addressing NEEDS allocated×512 exactly (clamping would
    /// corrupt the mapping), and its byte size equals the atlas pool's — if the pool fits the
    /// binding limit, so does cell_vert.
    pub(crate) cell_vert_cap: u64,
    /// R3b — bumped whenever `verts`/`idx` are RECREATED (output grow). Draw + picking bind the
    /// slot buffers fresh every pass today, but any future consumer that caches a bind group over
    /// them MUST compare against this (generation counters over timing tricks — CLAUDE.md).
    pub(crate) generation: u64,
    /// Bumped on EVERY successful mesh rewrite into this slot (including same-buffer re-dispatch
    /// after add-prim / boolean edits). Buffer `generation` alone does NOT change when content is
    /// rewritten into existing verts/idx capacity — vertex-curvature and similar consumers MUST
    /// compare against `content_gen` or they keep sampling stale per-vertex data until a manual
    /// layer eye-toggle forces a rebuild.
    pub(crate) content_gen: u64,
    /// R3b overflow detection: pending async readback of `counters` after a sparse dispatch —
    /// (staging buffer, map_async result channel, vert_cap at dispatch, idx_cap at dispatch).
    pending_counters: Option<(wgpu::Buffer, std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>, u64, u64)>,
    /// R3b — grid bounds of the last sparse mesh dispatch, so an overflow-grow can re-dispatch
    /// the SAME mesh from the still-resident atlas without waiting for the next bake.
    pub(crate) last_sparse_bounds: Option<(Vec3, Vec3)>,
    /// LOGICAL resolution of the CURRENT mesh — the res every DC dispatch uses (params, workgroup
    /// extents, field indexing). May be ≤ `capacity_res`: the resolution cascade / Grid picker
    /// oscillation (64→512, 512↔256) re-dispatches a smaller res into the same oversized buffers
    /// instead of a multi-GB free+realloc per switch. Safe because every dispatch zeroes `counters`,
    /// rewrites `indirect.index_count`, and the vertex pass rewrites EVERY logical cell of
    /// `cell_vert` — buffer tails beyond the logical extent are never read.
    pub(crate) res: u32,
    /// The resolution the buffers were SIZED for (grow-only watermark). A bake at res ≤ capacity_res
    /// reuses the slot; only a GROW reallocates.
    pub(crate) capacity_res: u32,
    /// Stage B-1: true iff the CURRENT mesh content lives in the shared `SdfMeshPool` region for
    /// this body (set by a pooled sparse dispatch, cleared by every dense dispatch). Routes the
    /// draw: pooled bodies are drawn by the ONE pool multi-draw; unpooled by
    /// `draw_sdf_resident_slot`. Per-frame visibility zeroing in the pool makes a stale pool
    /// entry (body went dense) self-skip without special cases.
    pub(crate) pooled: bool,
    /// C1 — true iff the CURRENT mesh was dispatched TILED (per-tile regions + args block in the
    /// pool). Routes the overflow poll (tile counters vs body counters) and the draw diagnostics.
    /// Cleared by every monolithic/dense dispatch.
    pub(crate) tiled: bool,
    /// C1 — per-tile OVERFLOW floors (packed tile coord → vertex cap floor): raised by the tile
    /// counters readback when a tile's measured need beat `halo_bricks × headroom`, so the next
    /// re-mesh sizes that tile from truth. Keyed on the tile COORD (stable while grid bounds
    /// hold); cleared by the settle trim (floors are transient-peak artifacts by definition).
    pub(crate) tile_cap_floor: std::collections::HashMap<u64, u32>,
    /// C1 — pending per-tile counters readback: (staging, map result, snapshot of the dispatch's
    /// per-tile (packed coord, vert cap, idx cap) in block order). At most one in flight (the
    /// R3b discipline — overflow is sticky).
    pending_tile_counters: Option<(
        wgpu::Buffer,
        std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
        Vec<(u64, u32, u32)>,
    )>,
    /// C2 — provenance of the last TILED dispatch (res/bounds/atlas gen/brick dims). `Some` and
    /// matching ⇒ the pool tile table may be reused incrementally; `None` (or any mismatch) ⇒
    /// full re-mesh. Cleared by every monolithic/dense dispatch.
    pub(crate) last_tiled_stamp: Option<SdfTiledStamp>,
    /// C2 — (dispatched, total live) tiles of the last tiled mesh call. Full re-mesh reports
    /// (n, n); an incremental dispatch reports the dirty subset — the gate asserts it actually
    /// skipped clean tiles, and the `[DcResidentSparse]` print shows `tiles=14/2700`.
    pub(crate) last_dispatched_tiles: (u32, u32),
    /// SETTLE-TIME TRIM one-shot (slice b): set by the poll caller when the body is QUIET (no
    /// escalation within the cooldown window — `BodyVolumeCache::sparse_quiet`), consumed by the
    /// next `mesh_into_slot_sparse`. When set AND the current surface need is under HALF the
    /// grow-only caps, the pool region / own buffers / cell_vert shrink to ~1.25× need — the
    /// hysteresis deadband that reclaims transient-peak VRAM (owner log: verts 1073MB + idx 846MB
    /// + cell_vert ~1GB pinned by ONE gesture's 490k-brick transient) without ever ping-ponging
    /// mid-gesture. Timing lives at the CALLER — this layer stays deterministic and testable.
    pub(crate) may_trim: bool,
    /// O2 LOD: the octree level the CURRENT mesh content was extracted at (0 = full detail —
    /// every pre-O2 path). Set by `mesh_into_slot_sparse_lod`; cleared (0) by every level-0
    /// dispatch. Consumers (the O2b policy, debug HUD) read it; nothing branches on it in the
    /// draw — a LOD mesh is ordinary triangles in the ordinary slot buffers.
    pub(crate) lod_level: u32,
    /// Envelope-fallback hysteresis (owner log 8): true after a tiled packing burst the dense
    /// ceiling — stay monolithic until packing drops below 75% of it, so a gesture oscillating
    /// across the boundary can't flip-flop tiled↔monolithic (each flip = a ~1GB pool re-pack,
    /// the last remaining ~51ms mesh-dispatch spike).
    pub(crate) envelope_fallback: bool,
    /// Phase 4b — LOUD-degradation latch: the tiled packing exceeded the pool budget AND the
    /// real surface exceeds the dense ceiling, so the monolithic fallback mesh IS truncated.
    /// The poll layer reads this once per arming to raise the user-facing toast (the self-heal
    /// precedent) — never silent truncation. Cleared whenever the body re-enters the tiled path.
    pub(crate) envelope_over_budget: bool,
}

#[allow(dead_code)] // constructed in the poll-phase ensure (G2); exercised now by the parity test.
impl SdfBodyMeshSlot {
    /// Allocate all buffers for a `res³` grid (sized by `dc_caps`). CALL ONLY FROM
    /// `poll_background_tasks` (Frame Ordering Contract — buffer creation is banned in panels/input/draw).
    pub(crate) fn new(device: &wgpu::Device, res: u32) -> Self {
        use wgpu::BufferUsages as U;
        let mk = |bytes: u64, usage: U, label: &str| device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label), size: bytes.max(4), usage, mapped_at_creation: false });
        let params = mk(std::mem::size_of::<DcParamsGpu>() as u64, U::UNIFORM | U::COPY_DST, "sdf_slot_params");
        // R3c: cell_vert starts as a placeholder like verts/idx — every dispatch entry sizes it
        // via ensure_output_caps (dense: cells³ region-local; sparse: allocated_bricks×512
        // brick-local). A brick-resident 512 body no longer carries the ~534MB dense map.
        let cell_vert = mk(4, U::STORAGE, "sdf_slot_cell_vert");
        // R3b: the OUTPUT buffers (verts/idx) start as 4-byte placeholders. Every dispatch entry
        // sizes them via `ensure_output_caps` BEFORE dispatching (all entries run in poll — Frame
        // Ordering holds): dense arms use the volume-proportional `dc_output_caps_dense` (byte-for-
        // byte today's sizing), the sparse arm uses the surface-proportional `dc_output_caps_sparse`
        // from the atlas's allocated brick count — a brick-resident 512 body no longer carries the
        // 2×1GiB dense worst case. Overflow is counter-detected and grows the buffers (never OOB —
        // the shader's arrayLength/atomicMin guards clamp writes meanwhile).
        let verts = mk(4, U::STORAGE | U::VERTEX | U::COPY_SRC, "sdf_slot_verts");
        let idx = mk(4, U::STORAGE | U::INDEX | U::COPY_SRC, "sdf_slot_idx");
        let counters = mk(8, U::STORAGE | U::COPY_DST | U::COPY_SRC, "sdf_slot_counters");
        // Seed the DrawIndexedIndirect args once via a mapped create (no queue needed):
        // [index_count=0, instance_count=1, first_index=0, base_vertex=0, first_instance=0].
        // dispatch_resident overwrites index_count each dispatch; the other four are static.
        let indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_slot_indirect"), size: 20,
            usage: U::INDIRECT | U::COPY_DST | U::COPY_SRC, mapped_at_creation: true });
        indirect.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(&[0u32, 1, 0, 0, 0]));
        indirect.unmap();
        Self { field: None, params, cell_vert, verts, idx, counters, indirect, res, capacity_res: res,
               vert_cap: 0, idx_cap: 0, cell_vert_cap: 0, generation: 0, content_gen: 0,
               pending_counters: None, last_sparse_bounds: None, pooled: false, may_trim: false,
               envelope_fallback: false, envelope_over_budget: false, lod_level: 0,
               tiled: false, tile_cap_floor: std::collections::HashMap::new(),
               pending_tile_counters: None,
               last_tiled_stamp: None, last_dispatched_tiles: (0, 0) }
    }

    /// Mark that mesh *content* was rewritten (positions/topology). Call after every successful
    /// `mesh_into_slot*` dispatch — even when buffer identity / `generation` did not change.
    #[inline]
    pub(crate) fn note_mesh_content(&mut self) {
        self.content_gen = self.content_gen.wrapping_add(1);
    }

    /// SETTLE-TIME TRIM of the slot-OWNED buffers (slice b twin of the pool's `trim_slot`).
    /// `want_v`/`want_i` apply only to unpooled slots (a pooled slot's vert_cap mirrors its POOL
    /// region — trimmed by `trim_slot`, not here; pass 0 to skip). `want_cv` always applies
    /// (cell_vert is slot-owned on every path). Shrink fires only under HALF the current cap and
    /// lands at exactly the wanted size (the caller passes ~1.25× need) — same hysteresis
    /// deadband as the pool trim, so a stable surface never churns. Content is scratch
    /// (rewritten by the dispatch that follows in the same call). Generation-bumped.
    /// CALL ONLY FROM poll-phase code (Frame Ordering Contract).
    pub(crate) fn trim_output_caps(&mut self, device: &wgpu::Device, want_v: u64, want_i: u64, want_cv: u64) {
        use wgpu::BufferUsages as U;
        let align = |bytes: u64| ((bytes.max(4) + 255) / 256) * 256;
        let mut trimmed = false;
        if want_v > 0 && self.vert_cap >= want_v.saturating_mul(2) {
            let before = self.vert_cap;
            self.vert_cap = want_v;
            self.idx_cap = want_i.max(1);
            self.verts = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_slot_verts"), size: align(self.vert_cap * 8 * 4),
                usage: U::STORAGE | U::VERTEX | U::COPY_SRC, mapped_at_creation: false });
            self.idx = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_slot_idx"), size: align(self.idx_cap * 4),
                usage: U::STORAGE | U::INDEX | U::COPY_SRC, mapped_at_creation: false });
            eprintln!("[SdfSlotTrim] verts cap {} → {} ({}MB freed at settle)",
                before, want_v, ((before - want_v) * 32) >> 20);
            trimmed = true;
        }
        if want_cv > 0 && self.cell_vert_cap >= want_cv.saturating_mul(2) {
            let before = self.cell_vert_cap;
            self.cell_vert_cap = want_cv;
            self.cell_vert = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_slot_cell_vert"), size: align(self.cell_vert_cap * 4),
                usage: U::STORAGE, mapped_at_creation: false });
            eprintln!("[SdfSlotTrim] cell_vert cap {} → {} ({}MB freed at settle)",
                before, want_cv, ((before - want_cv) * 4) >> 20);
            trimmed = true;
        }
        if trimmed { self.generation += 1; } // counters over timing — consumers re-acquire
    }

    /// True if the current `cell_vert` buffer cannot be bound as one storage buffer
    /// (poison from a past dense@1024 grow — sticky BufferRangeTooLarge).
    #[inline]
    pub(crate) fn cell_vert_is_poisoned(&self, max_storage_binding: u64) -> bool {
        self.cell_vert.size() > 4 && !dc_bytes_bindable(self.cell_vert.size(), max_storage_binding)
    }

    /// If `cell_vert` is larger than the storage bind limit, destroy it and reset to a
    /// placeholder so the next `ensure_output_caps` can allocate a legal size. Without this,
    /// `as_entire_binding()` keeps failing forever after a single poison grow.
    pub(crate) fn sanitize_cell_vert_if_poisoned(&mut self, device: &wgpu::Device) {
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        if !self.cell_vert_is_poisoned(max_bind) {
            return;
        }
        eprintln!(
            "[DcBind] POISON heal: cell_vert was {}MB > bind limit {}MB — recreating placeholder",
            self.cell_vert.size() >> 20,
            max_bind >> 20
        );
        use wgpu::BufferUsages as U;
        self.cell_vert = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sdf_slot_cell_vert"),
            size: 4,
            usage: U::STORAGE,
            mapped_at_creation: false,
        });
        self.cell_vert_cap = 0;
        self.generation += 1;
    }

    /// R3b — size the OUTPUT buffers (verts/idx) for at least `vert_cap` vertices / `idx_cap` u32
    /// indices. GROW-ONLY watermark under the storage-binding ceiling. Returns `false` if the
    /// `cell_vert` request cannot be bound (never creates an unbindable buffer — device-loss class).
    /// CALL ONLY FROM poll-phase code (Frame Ordering Contract).
    pub(crate) fn ensure_output_caps(
        &mut self,
        device: &wgpu::Device,
        vert_cap: u64,
        idx_cap: u64,
        cell_vert_cap: u64,
    ) -> bool {
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        self.sanitize_cell_vert_if_poisoned(device);
        let (dense_v, dense_i) = dc_output_caps_dense(max_bind, self.capacity_res.min(crate::gpu_jobs::max_gridbacked_resolution()));
        // When capacity_res is 1024 (sparse-era slot), dense ceiling uses 512 math for verts/idx.
        let (dense_v, dense_i) = if dense_v == 0 && dense_i == 0 {
            dc_output_caps_dense(max_bind, crate::gpu_jobs::max_gridbacked_resolution())
        } else {
            (dense_v, dense_i)
        };
        let want_v = vert_cap.min(dense_v);
        let want_i = idx_cap.min(dense_i);
        // cell_vert: exact need for sparse brick-local / dense dispatch — but MUST bind.
        let want_cv = cell_vert_cap;
        if want_cv > 0 && !dc_bytes_bindable(want_cv * 4, max_bind) {
            eprintln!(
                "[DcBind] REJECT ensure_output_caps cell_vert={}MB > bind limit {}MB (capacity_res={})",
                (want_cv * 4) >> 20,
                max_bind >> 20,
                self.capacity_res
            );
            return false;
        }
        // OWN-BUFFER TRUTH (2026-07-16, the "Auto SDF LOD makes a sparse body VANISH" P1): a slot
        // whose current mesh is POOLED mirrors its POOL region caps in `vert_cap`/`idx_cap`
        // while OWN `verts`/`idx` may still be placeholders — size from buffer truth.
        let have_v = self.verts.size() / (8 * 4); // 8 f32 per vertex
        let have_i = self.idx.size() / 4;
        let have_cv = self.cell_vert.size() / 4;
        use wgpu::BufferUsages as U;
        let align = |bytes: u64| dc_align_buffer_bytes(bytes);
        let mut recreated = false;
        if want_v > have_v || want_i > have_i {
            // Grow-only vs the OWN buffers (never shrink mid-session — trim owns shrinking).
            self.vert_cap = have_v.max(want_v);
            self.idx_cap = have_i.max(want_i);
            let v_bytes = align(self.vert_cap * 8 * 4);
            let i_bytes = align(self.idx_cap * 4);
            if !dc_bytes_bindable(v_bytes, max_bind) || !dc_bytes_bindable(i_bytes, max_bind) {
                eprintln!(
                    "[DcBind] REJECT verts/idx grow v={}MB i={}MB over bind limit",
                    v_bytes >> 20,
                    i_bytes >> 20
                );
                return false;
            }
            self.verts = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_slot_verts"), size: v_bytes,
                usage: U::STORAGE | U::VERTEX | U::COPY_SRC, mapped_at_creation: false });
            self.idx = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_slot_idx"), size: i_bytes,
                usage: U::STORAGE | U::INDEX | U::COPY_SRC, mapped_at_creation: false });
            recreated = true;
        } else if vert_cap > 0 || idx_cap > 0 {
            // Own-output request satisfied by the existing OWN buffers — re-anchor the logical
            // caps (they may still carry POOL region numbers from a pooled era). Pool callers
            // pass 0/0 and keep their region caps untouched.
            self.vert_cap = have_v;
            self.idx_cap = have_i;
        }
        if want_cv > have_cv {
            // Fail closed at creation — never allocate an unbindable cell_vert (device-loss class).
            let cv_bytes = align(want_cv * 4);
            if !dc_bytes_bindable(cv_bytes, max_bind) {
                eprintln!(
                    "[DcBind] REJECT cell_vert create {}MB > bind limit {}MB",
                    cv_bytes >> 20,
                    max_bind >> 20
                );
                return false;
            }
            self.cell_vert_cap = want_cv;
            self.cell_vert = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_slot_cell_vert"), size: cv_bytes,
                usage: U::STORAGE, mapped_at_creation: false });
            recreated = true;
        } else if want_cv > 0 {
            // Existing buffer covers need — but it must still be bindable (poison edge).
            if self.cell_vert_is_poisoned(max_bind) {
                self.sanitize_cell_vert_if_poisoned(device);
                return self.ensure_output_caps(device, vert_cap, idx_cap, cell_vert_cap);
            }
            self.cell_vert_cap = self.cell_vert_cap.max(want_cv);
        }
        if recreated { self.generation += 1; }
        true
    }

    /// R3b — grow the output caps ×1.5 (or to the exact `need_verts` if larger — the counter
    /// readback gives the TRUE unclamped vertex count) after a detected overflow. Generation-bumped
    /// via `ensure_output_caps`. Returns false when already at the dense ceiling (nothing to grow —
    /// the mesh keeps the dense-era truncation behavior, never OOB).
    pub(crate) fn grow_output_caps(&mut self, device: &wgpu::Device, need_verts: u64) -> bool {
        let before = (self.vert_cap, self.idx_cap);
        // ×1.5 from the current cap, but never below 1.25× the TRUE count — an exact-fit grow
        // would re-overflow on the very next surface-growing edit (re-mesh churn).
        let want_v = (need_verts * 5 / 4).max(self.vert_cap * 3 / 2).max(64);
        if !self.ensure_output_caps(device, want_v, want_v * 6, self.cell_vert_cap) {
            return false;
        }
        (self.vert_cap, self.idx_cap) != before
    }

    /// R3b — the overflow predicate over the read-back counters. `vert_count` (counters[0]) is
    /// UNCLAMPED (writes past the cap are dropped but the atomicAdd keeps counting) → strict `>`.
    /// `index_count` (counters[1]) is atomicMin-clamped to the buffer length → a full buffer
    /// (`>=`) is indistinguishable from overflow; treating exact-fit as overflow costs one
    /// harmless ×1.5 grow and then never again.
    pub(crate) fn output_overflow(&self, vert_count: u32, index_count: u32) -> bool {
        vert_count as u64 > self.vert_cap || index_count as u64 >= self.idx_cap
    }

    /// R3b — kick the async counters readback after a sparse dispatch (called by
    /// `mesh_into_slot_sparse`; the copy is already encoded in the dispatch submit). At most one
    /// in flight: overflow is sticky (an undersized cap overflows every dispatch), so skipping a
    /// re-issue while one is pending loses nothing.
    fn kick_counter_readback(&mut self, staging: wgpu::Buffer) {
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.pending_counters = Some((staging, rx, self.vert_cap, self.idx_cap));
    }

    /// R3b — non-blocking poll of the pending counters readback. Returns `Some([vert_count,
    /// index_count])` once the map completes IF the counts overflowed the caps that were live at
    /// dispatch time (stale results against already-grown caps are discarded); consumes the
    /// pending state either way. Call from `poll_background_tasks` (and tests, after a Wait poll).
    pub(crate) fn poll_counter_overflow(&mut self) -> Option<[u32; 2]> {
        use std::sync::mpsc::TryRecvError;
        match self.pending_counters.as_ref()?.1.try_recv() {
            Err(TryRecvError::Empty) => return None, // still in flight
            Ok(Ok(())) => {}
            // Map failed or sender dropped — discard this readback (the next sparse dispatch
            // re-issues one; overflow is sticky, nothing is lost).
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => { self.pending_counters = None; return None; }
        }
        let (staging, _rx, cap_v, cap_i) = self.pending_counters.take().unwrap();
        let counts: [u32; 2] = {
            let mapped = staging.slice(..).get_mapped_range();
            let c: &[u32] = bytemuck::cast_slice(&mapped);
            [c[0], c[1]]
        };
        // Overflow judged against the caps AT DISPATCH; only actionable if those caps are still
        // current (a grow that already happened supersedes this result).
        let overflowed = counts[0] as u64 > cap_v || counts[1] as u64 >= cap_i;
        (overflowed && cap_v == self.vert_cap && cap_i == self.idx_cap).then_some(counts)
    }

    /// C1 — kick the async PER-TILE counters readback after a tiled dispatch (the copy is already
    /// encoded in the dispatch submit). At most one in flight (the R3b discipline — overflow is
    /// sticky, an undersized tile overflows every dispatch).
    fn kick_tile_counter_readback(&mut self, staging: wgpu::Buffer, snapshot: Vec<(u64, u32, u32)>) {
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.pending_tile_counters = Some((staging, rx, snapshot));
    }

    /// C1 — non-blocking poll of the pending per-tile counters readback. Returns the overflowed
    /// tiles as `(packed tile coord, true vertex need)` once the map completes (empty result =
    /// no overflow); consumes the pending state either way. The vert counter is UNCLAMPED
    /// (strict `>`); the index counter is atomicMin-clamped to the tile cap (`>=` treated as
    /// overflow — one harmless grow, the R3b rationale). Call from `poll_background_tasks`.
    pub(crate) fn poll_tile_overflow(&mut self) -> Option<Vec<(u64, u32)>> {
        use std::sync::mpsc::TryRecvError;
        match self.pending_tile_counters.as_ref()?.1.try_recv() {
            Err(TryRecvError::Empty) => return None, // still in flight
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                self.pending_tile_counters = None;
                return None;
            }
        }
        let (staging, _rx, snapshot) = self.pending_tile_counters.take().unwrap();
        let counts: Vec<u32> = {
            let mapped = staging.slice(..).get_mapped_range();
            bytemuck::cast_slice::<u8, u32>(&mapped).to_vec()
        };
        let mut over = Vec::new();
        for (i, &(k, vcap, icap)) in snapshot.iter().enumerate() {
            let (vc, ic) = (counts.get(i * 2).copied().unwrap_or(0),
                            counts.get(i * 2 + 1).copied().unwrap_or(0));
            if vc > vcap || ic >= icap {
                over.push((k, vc.max(ic / 6)));
            }
        }
        Some(over)
    }

    /// C1 — raise the overflow floors from `poll_tile_overflow`'s result (×1.25 the true need,
    /// clamped to the per-tile hard bound). Returns true when any floor actually rose — the
    /// caller re-meshes; a no-op result (all floors already at the bound) keeps the truncation.
    pub(crate) fn raise_tile_floors(&mut self, over: &[(u64, u32)]) -> bool {
        let mut raised = false;
        for &(k, need) in over {
            let want = ((need as u64 * 5 / 4).clamp(32, SDF_TILE_MAX_VERTS)) as u32;
            let e = self.tile_cap_floor.entry(k).or_insert(0);
            if want > *e {
                *e = want;
                raised = true;
            }
        }
        raised
    }

    /// Allocate the dense field buffer on first DENSE use (`capacity_res`-sized so the grow-only
    /// watermark semantics match the other buffers). CALL ONLY FROM poll-phase code (Frame
    /// Ordering Contract) — today's sole caller is `mesh_into_slot`, which runs in
    /// `sdf_populate_resident` (poll).
    pub(crate) fn ensure_field(&mut self, device: &wgpu::Device, res: u32) -> &wgpu::Buffer {
        // R3c fix (2026-07-13): sized from the DISPATCH res (grow-only watermark), never from
        // `capacity_res` — a 1024-capacity (sparse-era) slot would allocate a 4.29GB dense field
        // that exceeds max_storage_buffer_binding_size and invalidates every dense DC bind group
        // (the "surface breaks and stops updating" cascade). Dense dispatch res is structurally
        // ≤ max_gridbacked_resolution (512), so the res-sized field is always bindable — the
        // assert below enforces that invariant loudly at the creation site.
        let r = res as u64;
        let need = (r * r * r * 4).max(4);
        assert!(
            need <= device.limits().max_storage_buffer_binding_size as u64,
            "dense DC field at {res}³ ({}MB) exceeds the storage-binding limit — a >{}³ mesh \
             must route the sparse (brick atlas) path, never the dense field",
            need >> 20, crate::gpu_jobs::max_gridbacked_resolution(),
        );
        let too_small = self.field.as_ref().map_or(true, |f| f.size() < need);
        if too_small {
            self.field = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sdf_slot_field"),
                size: need,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.generation += 1; // buffer recreated — generation counters over timing tricks
        }
        self.field.as_ref().unwrap()
    }
}
