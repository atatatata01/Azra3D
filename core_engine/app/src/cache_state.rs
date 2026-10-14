// cache_state.rs — GPU/mesh cache state extracted from FourthLayerApp.
// Contains volume caches, serialized prim/light buffers, DC mesh jobs,
// and related helpers.

use core_math::sdf_volume::SdfVolume;
use core_math::scene_graph::NodeId;
use render::mesh_cache::MeshCache;
use std::collections::HashMap;

use crate::{PendingDcJob, warming};
use crate::body_volume_cache;

/// GPU upload caches, mesh cache, volume caches, and DC extraction jobs.
pub(crate) struct CacheState {
    // CPU SDF volume (analytical → voxel grid)
    pub(crate) cpu_volume: SdfVolume,
    // Rasterized SDF body mesh cache (background Dual Contouring meshing)
    pub(crate) mesh_cache: MeshCache,
    // Per-body SDF volume cache (Phase 11.1)
    pub(crate) body_vol_cache: body_volume_cache::BodyVolumeCache,
    /// Phase D3: cluster-DAG background bake (dense poly meshes → virtual-geometry
    /// hierarchy; D4 consumes). Same job/debounce/staleness shape as body_vol_cache.
    pub(crate) cluster_bake: crate::cluster_bake::ClusterBakeState,
    pub(crate) use_body_vol_cache: bool,
    /// True when the serialized prim buffer needs re-upload to the GPU.
    pub(crate) prim_buf_stale: bool,
    /// Cached serialized light GPU data.
    pub(crate) cached_light_gpu_data: Vec<f32>,
    /// True when light properties changed — triggers re-serialization + upload.
    pub(crate) light_gpu_data_stale: bool,
    /// P3.1: last frame light pack (shade list + link entries + vol bits). Rebuilt with light buffer.
    pub(crate) cached_frame_lights: Option<crate::light_pack::FrameLightGpu>,
    /// P3.1 Phase C: sticky shade-list state (reduces top-K popping while orbiting).
    pub(crate) shade_hysteresis: crate::light_pack::ShadeHysteresis,
    /// Cached serialized prim buffer bytes from last upload.
    pub(crate) cached_prim_bytes: Vec<u8>,
    /// Cached visible-prim count from last serialize_from_graph call.
    pub(crate) cached_prim_nv: u32,
    /// Which body's prims changed (for partial re-serialization during drag).
    pub(crate) prim_buf_dirty_body: Option<NodeId>,
    /// Per-body byte ranges from the last full serialize.
    pub(crate) cached_body_byte_ranges: Vec<(NodeId, usize, usize, usize)>,
    /// Cached result of all-SDF-bodies-have-GPU-mesh check.
    pub(crate) cached_all_meshes_ready: Option<bool>,
    /// Hash guard for rebake_no_undo — prevents infinite SCENE dirty loops.
    pub(crate) rebake_hash: u64,
    // Volume cache helpers
    /// Dense 0-based object IDs for silhouette / gbuf_motion.r (C.5). Rebuilt with draw stream.
    pub(crate) body_index_map: HashMap<NodeId, usize>,
    pub(crate) suppress_body_vol_dirty: bool,
    pub(crate) smooth_dc_normals: bool,
    /// Deferred atlas rebuild: set by properties panel, processed in poll_background_tasks.
    /// Never create/replace atlas texture during egui panel draw (frame ordering rule).
    /// Deferred material-map image loads (Frame Ordering Contract): a material editor row that
    /// loads/applies an albedo map decodes the RGBA8 pixels on the UI thread, then pushes the
    /// upload request here. `poll_background_tasks` drains it and writes into the material texture
    /// array (the only place that creates/grows that GPU texture). Each entry:
    /// `(material_id, decoded RGBA8 pixels, width, height)`. The slot the load lands in is written
    /// back to the material's `albedo_map.tex_id` from poll.
    pub(crate) material_map_loads: Vec<crate::material_textures::PendingMapLoad>,
    /// Demo-scene harness: `(seed body, sibling bodies)` awaiting texture-slot
    /// propagation. The seed's `PendingMapLoad`s resolve a frame later; poll then
    /// copies the assigned `TexMap`s onto the siblings — ONE upload per map with
    /// thousands of materials pointing at the same array slot.
    pub(crate) demo_tex_groups: Vec<(NodeId, Vec<NodeId>)>,
    /// Worst observed `poll_primary_bake` readback cost this session, in ms — the FrameScheduler's
    /// cost hint for the `bake_readback` ticket.
    ///
    /// Learned rather than hardcoded: the hint was a flat `1.0` while a 128³ readback costs
    /// ~668 ms, so the scheduler's budget gate could never fire and a single readback stalled the
    /// frame for ~40 frames' worth of budget. Cost scales with grid resolution, so a constant
    /// would either lie (too small) or starve cheap readbacks (too large).
    pub(crate) bake_readback_worst_ms: f32,
    /// Consecutive frames `poll_primary_bake` has been skipped by the frame-budget gate.
    ///
    /// Exists so the gate can DEFER but never STARVE: the learned `bake_readback_worst_ms` can
    /// legitimately exceed the whole 12 ms frame budget, which made `can_run` return false forever
    /// and left the pending queue permanently undrained (surface stops updating, UI stays
    /// responsive). Reset to 0 on every actual run.
    pub(crate) bake_readback_deferred_frames: u32,
    /// Per-body prefix cache for the display mesh-modifier fold (`fold_display_mesh_cached`).
    ///
    /// `RefCell` because both fold entry points (`fold_body_display_after_dc`,
    /// `fold_body_display_for_export`) take `&self` — the cache is pure MEMOIZATION of a
    /// deterministic function, not scene state, so interior mutability is honest here rather than
    /// a way around ownership.
    ///
    /// Keyed by body so switching bodies does not thrash one shared cache. Entries are dropped
    /// when the body's base mesh changes (see `mesh_mod_base_key`) — the cached prefixes were
    /// folded from the OLD base and would otherwise be silently stale.
    pub(crate) mesh_mod_prefix_cache: std::cell::RefCell<
        std::collections::HashMap<
            core_math::scene_graph::NodeId,
            (u64, core_math::display_mesh_ops::DisplayMeshPrefixCache),
        >,
    >,
    /// Background node-graph bake results (preset / Apply). UI never blocks on 1024² eval.
    pub(crate) graph_bake_tx: std::sync::mpsc::Sender<crate::node_graph_bake::GraphBakeResult>,
    pub(crate) graph_bake_rx: std::sync::mpsc::Receiver<crate::node_graph_bake::GraphBakeResult>,
    /// GPU material bakes submitted but not yet read back.
    ///
    /// The GPU path used to call `bake_material_maps_gpu_blocking` inline from
    /// `spawn_or_park_graph_bake`, which runs on the FRAME THREAD (`&mut self`) — a
    /// `device.poll(Maintain::Wait)` per bake, i.e. exactly what `gpu_material_bake.rs`'s own module
    /// header forbids and what `gpu_curvature.rs` documents as a real owner-reported freeze. Found by
    /// the 2026-07-30 pillar review. The submit now returns immediately and lands here; the flight is
    /// drained non-blockingly in `poll_graph_bakes` and delivered through `graph_bake_tx`, so the GPU
    /// path reaches the exact same consumer the CPU worker path already used.
    pub(crate) graph_bake_gpu_inflight: Vec<crate::node_graph_bake::InflightGpuBake>,
    pub(crate) graph_bake_seq: u64,
    /// Latest requested bake seq per owner key `(kind, id)` — stale results are dropped.
    pub(crate) graph_bake_pending: HashMap<(u8, u32), u64>,
    /// Live node-editor preview: owner (Material mid or Prim uid) that needs / has a live bake.
    /// Coalesce pattern — not debounce-until-idle — so dragging knobs updates the viewport
    /// every completed bake, not only after the user stops.
    pub(crate) graph_live_owner: Option<crate::node_graph_bake::GraphBakeOwner>,
    /// True when knobs changed while a live bake was already running → spawn another on complete.
    pub(crate) graph_live_dirty: bool,
    /// True while a Live-quality bake is the latest pending (toolbar status).
    pub(crate) graph_live_in_flight: bool,
    /// ADJUST_SLIDER_LAG F2: channels dirty during the current live scrub coalesce window.
    /// `None` = bake all graph-driven channels. Accumulates via OR across multi-channel scrubs;
    /// cleared when a Final bake is enqueued (settle / Apply).
    pub(crate) graph_live_channel_filter: Option<crate::node_graph_bake::BakeChannelMask>,
    /// O4: last successful bake maps per owner (install-res buffers). Used as the composite
    /// base when a subsequent bake has a partial dirty rect at the same resolution.
    pub(crate) graph_bake_map_cache:
        HashMap<(u8, u32), crate::node_graph_bake::BakedNodeMaps>,
    /// O4: pending dirty region for the next bake of this owner. Taken (and cleared) at enqueue.
    /// Default when missing = Full. Generator/noise edits should set Full; paint/mask may set Rect.
    pub(crate) graph_bake_dirty:
        HashMap<(u8, u32), crate::node_graph_bake::BakeDirty>,
    /// GPU CURVATURE (`GPU_CURVATURE_KERNEL_PLAN.md` P1): bakes whose graph uses the SDF-field
    /// nodes are parked here by `enqueue_*` instead of spawning their worker immediately.
    /// `poll_graph_curvature` (poll phase — the ONLY legal window for a GPU dispatch, Frame
    /// Ordering Contract) runs the `curvature_eval` kernel, attaches the resulting grid to the
    /// job's `SdfFieldSources`, and spawns the worker. Enqueue is reachable from panel draws, so
    /// the dispatch cannot happen there; the cost is one frame of latency on a bake that takes
    /// tens of ms, and the repaint keep-alive already covers a pending bake.
    /// Every job here is drained the very next poll — the queue is never a backlog.
    pub(crate) graph_curv_pending: Vec<crate::node_graph_bake::PendingCurvBake>,
    /// GPU CURVATURE, stage 2: jobs whose `curvature_eval` has been SUBMITTED and whose readback
    /// has not settled yet. The frame thread never waits on these — `poll_graph_curvature` peeks
    /// each flight's `MapState` with a non-blocking `Maintain::Poll` and finishes at most ONE per
    /// frame (Frame Budget Rule), exactly like `poll_gpu_classify_readbacks`. A flight whose
    /// `seq` is no longer the pending seq for its owner is DISCARDED as superseded.
    pub(crate) graph_curv_inflight: Vec<crate::node_graph_bake::InFlightCurvBake>,
    /// O5: Convert SDF→Poly request queued from UI (no GPU work until poll).
    pub(crate) convert_sdf_pending: Option<crate::convert::PendingConvertSdf>,
    /// O5: In-flight async display/grid readback for convert (no `poll(Wait)` on the frame thread).
    pub(crate) convert_sdf_inflight: Option<crate::convert::InFlightConvertSdf>,
    /// Set on scene LOAD → poll resets the material texture array so the new scene never samples the
    /// PREVIOUS scene's albedo-map slots (review #2 cross-scene bleed). The reset recreates a GPU
    /// texture, so it must run in poll (Frame Ordering Contract). Full map persistence is a separate
    /// save-format change (the naive serde field would corrupt every existing .fl4 — field trap).
    pub(crate) material_array_reset_pending: bool,
    /// Decoded albedo-map pixels to re-upload after the load-time array reset (#2), in slot order.
    /// Set by `apply_to_app` from the FL4MATTX tail; consumed once in poll (reset → upload in order).
    pub(crate) material_maps_restore: Option<Vec<Vec<u8>>>,
    /// Deferred atlas COMPACT: set on a per-body resolution change. The shelf allocator's free()
    /// is append-only (leaves holes), so a resolution change frees the old cell as dead space and
    /// appends a new one — the atlas starves and other bodies' slots get evicted/orphaned → the
    /// stretched/garbage cross-body corruption. Processed in poll: reset the allocator + clear slots
    /// + rebake all bodies so every body repacks into a fresh, hole-free atlas. CPU-only (no texture
    /// realloc), so cheap; the rebake is the cost, paid only on a deliberate resolution change.
    /// Background DC mesh extraction jobs (Rayon). Polled each frame.
    pub(crate) pending_dc_jobs: Vec<PendingDcJob>,
    pub(crate) warming: warming::WarmingState,
    /// Material preview swatch cache (moved from FourthLayerApp — Phase 1).
    pub(crate) mat_preview_cache: crate::mat_preview::MaterialPreviewCache,
    /// Map-slot thumbnail cache (material editor rows + outliner shader-tree children).
    /// Keyed (slot, data-decode), invalidated by the texture array's generation counter.
    pub(crate) map_thumb_cache: crate::material_textures::MapThumbCache,
    /// UV-layout underlay preview (G5). Hit = no pixel alloc; miss = 256² downsample.
    pub(crate) uv_underlay_cache: crate::mesh_map_bake::UvUnderlayCache,
    /// Displace modifier height-map UI thumbs. Key = (prim_flat_idx, mod_idx);
    /// value = (content signature, handle) so reloads rebuild the texture.
    pub(crate) displace_hm_thumbs:
        HashMap<(usize, usize), (u64, eframe::egui::TextureHandle)>,
    /// CR-70 staggered post-load bake — bakes 2 bodies/frame to avoid TDR (moved from FourthLayerApp — Phase 1).
    pub(crate) post_load_bodies: Vec<NodeId>,
    pub(crate) post_load_warmup: u32,
    /// Phase G: mesh SDF bodies on the GPU (QEF Dual Contouring) instead of the background CPU DC.
    /// DEFAULT ON (validated live: fluid, async readback closes the CPU-DC generation race + the 66ms
    /// blocking stall). `dispatch_grid_dc_async` GPU-meshes the body grid into mesh_cache via the proven
    /// raster draw. Toggle OFF in Developer Tweaks to fall back to the CPU DC path.
    pub(crate) gpu_sdf_mesh: bool,
    /// Lazily-built GPU QEF-DC mesher (pipelines compiled once on first use).
    pub(crate) sdf_mesher: Option<crate::sdf_gpu_mesh::SdfMeshGpu>,
    /// G2 — the RESIDENT path: mesh the body into a persistent GpuCtx.sdf_body_mesh slot and draw it via
    /// draw_indexed_indirect (no per-frame mesh_cache buffer alloc/upload — replaces the 2.7-6ms
    /// `gpu_mesh_to_cache` `dc_dispatch` with a cheap resident dispatch). **DEFAULT ON** (user directive
    /// 2026-06-24). The bake readback still runs, so the G3 guard's CPU-grid fallback + all CPU consumers
    /// (save/export/duplicate) stay available — device-loss-safe. Toggle OFF in Developer Tweaks → falls
    /// back to the `gpu_sdf_mesh` readback path (mesh_cache draw). Slots pruned on SCENE (no leak).
    pub(crate) gpu_sdf_mesh_resident: bool,
    /// G3 (sub-mode of `gpu_sdf_mesh_resident`): feed the QEF-DC `field` input DIRECTLY from the
    /// GPU-resident density buffer (`ResidentVolume.buffer`) instead of uploading the CPU grid into
    /// `slot.field` — removing the per-resident-bake `slot.field` host→GPU upload. **DEFAULT ON** (user
    /// directive 2026-06-24). Guarded by content-identity (body_nid + resolution + bake_bounds) + the
    /// realloc flag; falls back to the CPU grid upload (always correct) when the guard fails. Bit-parity
    /// proven (`sdf_dc_resident_field_parity`). The readback's `to_vec` still runs (this is NOT yet the
    /// lazy-readback step 2) — it feeds the fallback + CPU consumers, so this stays device-loss-safe.
    pub(crate) gpu_sdf_mesh_resident_field: bool,
    /// Phase 3B (sparse): mesh the DC surface from a SPARSE brick atlas (`classify_grid_to_sparse` →
    /// brick index/pool, memory ∝ surface) instead of the dense res³ field. The dispatch/decode is proven
    /// bit-exact (`sdf_dc_sparse_brick_decode_parity`). Default ON (set below, per owner — the earlier
    /// white-cube issues were traced to stale-atlas/runaway and are fixed); toggle in Developer Tweaks.
    /// Wires into `sdf_populate_resident` (needs the CPU grid → only the full-grid populate path).
    pub(crate) gpu_sdf_mesh_sparse: bool,
    /// Step 2: lazy readback — skip the CPU `to_vec` on PARTIAL field-direct bakes (the surface is meshed
    /// from rv.buffer). Big win at 256³ (~16ms). Default ON now that path A (bake the dirty slab in place
    /// into rv.buffer) removed the stale-seed that caused the earlier "second surface" ghost during drag;
    /// body_grids self-heals on the settle re-tighten bake.
    pub(crate) gpu_sdf_mesh_lazy_readback: bool,
    /// Path-A for FULL bakes: bake an escape/settle FULL bake DIRECTLY into rv.buffer (GPU2GPU) and SKIP
    /// the CPU staging readback — exactly like the partial in-place path that makes small moves ghost-free.
    /// A large/fast move escapes the grid → a full bake; through the slow readback the mesh would show the
    /// old position for the readback frame(s) → the "drag-ghost". In-place → fresh mesh → no ghost (and it
    /// kills the high-res full-bake hitch). Default ON — user-confirmed (see the init note below).
    pub(crate) gpu_full_bake_path_a: bool,
    // S2.6 (2026-07-14): `sparse_bake_debug` REMOVED — the SparseBricks (mode-2) pipeline is now
    // STRUCTURAL for primary FULL bakes at ≥512 in Object Mode, keyed solely on
    // `gridbacked_only` (see `BodyVolumeCache::sparse_bake_eligible`). The toggle only ever
    // existed to A/B the dense path while 3A landed, and being runtime-only it was review
    // finding R1 (an OFF default silently sent every launch's first 512 bake down the dense
    // path: ~553ms + 512MB readback). Do NOT reintroduce a gate here: it cannot be persisted
    // (RenderSettings lives in the bincode ProjectFile = the .fl4 field-trap), so any such flag
    // is a launch-time regression waiting to happen.
    /// In-flight async GPU-mesh readbacks (one per body cid, coalesced). Polled each frame; when a job's
    /// staging maps, its mesh lands in mesh_cache. Async = NO blocking poll(Wait) stall during edits.
    pub(crate) pending_sdf_mesh: Vec<(core_math::scene_graph::MeshCacheId, crate::sdf_gpu_mesh::PendingSdfReadback)>,
    /// POLY P3 — the runtime per-body `CustomData` SoA sidecar (uv/seam/sharp/crease/vcolor),
    /// keyed by the body's `NodeId` (`POLY_FOUNDATION_PLAN.md` §4.1/§4.2 — CustomData is RUNTIME
    /// app/cache state, NEVER a graph node, NEVER serialized into the bincode blob). The
    /// Object-Mode bake reads it via `PolyMesh::to_raster_data_with_cd`; an absent entry = empty
    /// sidecar = byte-identical to the old `to_raster_data`. Persistence rides the in-blob
    /// `#[serde(default)] Vec<PolyLayerRecord>` field on `ProjectFile` (the `voxel_grids`
    /// precedent), reattached by `node_id` on load — never from this map directly.
    pub(crate) poly_custom_data: HashMap<NodeId, core_math::poly_mesh::CustomData>,
    /// Queued Bake Mesh Maps request, drained in `poll_mesh_map_bakes` (Frame Ordering Contract:
    /// `upload_map` creates GPU resources and must not run in a menu callback).
    pub(crate) pending_mesh_map_bake: Option<crate::mesh_map_bake::PendingMeshMapBake>,
    /// In-flight mesh map bake. `Some` = a worker is computing; the poll drains and uploads.
    pub(crate) mesh_map_rx:
        Option<std::sync::mpsc::Receiver<crate::mesh_map_bake::MeshMapResult>>,
    /// Image-panel PNG export: (suggested file name, encoded PNG bytes). Drained in poll.
    pub(crate) pending_baked_export: Option<(String, Vec<u8>)>,
    /// P1b — last applied post-DC mesh-mod display stamp per body:
    /// `hash(base_key, mesh_stack_hash)` where base_key includes field content + bake resolution.
    /// When it matches, `sync_body_mesh_mod_display` skips re-materialize.
    pub(crate) mesh_mod_display_stamp: HashMap<NodeId, u64>,
    /// P1b — raw (pre-mesh-mod) display mesh cache for fold-only rebuilds on slider scrub:
    /// (verts, indices, base_key). base_key = hash(field_content_id, effective_resolution).
    /// Avoids blocking GPU readback every mesh-mod tweak; invalidates on res change.
    pub(crate) mesh_mod_base: HashMap<NodeId, (Vec<[f32; 8]>, Vec<u32>, u64)>,
    /// Triangle-PT acceleration cache (Phase 2): per-body BLAS keyed by
    /// MeshCacheId + mesh_cache `content_stamp`. Fed by `update_pt_accel`
    /// (poll phase) only while PathTrace shading is active.
    pub(crate) pt_accel: render::pt_accel::PtAccelCache,
    /// Hash of the last-assembled PT instance list (cid, model bits, material).
    /// Assembly + SSBO upload re-run only when this or a BLAS changes.
    pub(crate) pt_last_scene_hash: u64,
    /// Instance count of the last-assembled PT scene — the render-mode gate
    /// (`resolve_render_mode`) requires > 0 to dispatch the triangle PT.
    pub(crate) pt_instance_count: u32,
    /// PT sparse-SDF snapshots (consolidation fix): a brick-resident (>=512) body has NO CPU
    /// mesh - its display mesh lives in GPU slots - so gather_pt_instances found 0 instances and
    /// PT silently fell back to raster ("looks like PBR"). update_pt_accel now snapshots such
    /// bodies: a TEMPORARY monolithic level-0 mesh dispatch from the body's atlas (never touches
    /// the display slot) + async readback, landed into mesh_cache via update_editable_mesh.
    /// Keyed on **(source_hash, bake_resolution)** — content-only hash is blind to Grid res
    /// changes, so a pure resolution bump must re-snapshot (else PT mesh stays forever at the
    /// old tessellation). PT mode only.
    pub(crate) pt_sdf_snap_hash: HashMap<NodeId, (u64, u32)>,
    // (PT_SNAP_SETTLE lives just below CacheState — module-level const.)
    /// SETTLE-GATE state: last OBSERVED (not yet snapshotted) (hash, res) per body + when first
    /// seen. A snapshot dispatches only once the pair holds still for `PT_SNAP_SETTLE` — a
    /// mid-gesture hash is stale before its readback lands, so dispatching it is pure waste
    /// (the 2026-07-16 dispatch→DROPPED 222ms/poll loop).
    pub(crate) pt_sdf_snap_seen: HashMap<NodeId, (u64, u32, std::time::Instant)>,
    /// In-flight snapshot readbacks: (body, cid, source_hash, bake_res at dispatch, pending).
    pub(crate) pt_sdf_snap_pending: Vec<(
        NodeId,
        core_math::scene_graph::MeshCacheId,
        u64,
        u32,
        crate::sdf_gpu_mesh::PendingSdfReadback,
    )>,
    /// O2b — automatic per-body octree-LOD level selection (`poll_sdf_lod_policy`): distant
    /// brick-resident bodies re-mesh at level 1/2 from their EXISTING atlas (no re-bake). Default
    /// OFF for the first ship (Developer Tweaks checkbox) so the owner can A/B it; flipping
    /// default-on is a one-line follow-up after owner verification.
    pub(crate) sdf_lod_auto: bool,
    /// PLAN_FOR_GROK_VIEWPORT_PERF P3: automatic poly display LOD (object mode). Default ON.
    /// Edit-mode active bodies always stay full detail.
    pub(crate) poly_lod_auto: bool,
    /// B.4 / C.1: last display key `(content_sig, stack_key)` written into each MeshCacheId.
    /// Lets Shift+D siblings seed their display cache without COW+reupload (share preserved).
    pub(crate) poly_mesh_gpu_key: HashMap<core_math::scene_graph::MeshCacheId, (u64, u64)>,
    /// Per-body rate-limit stamp for the LOD policy: the Instant of the body's last level switch
    /// (at most one switch per body per `LOD_MIN_SWITCH_INTERVAL`). The LEVEL itself is not
    /// duplicated here — `slot.lod_level` (the actual mesh content) is the current-level truth,
    /// so a normal bake/populate resetting the slot to L0 automatically re-enters the policy.
    pub(crate) sdf_lod_last_switch: HashMap<NodeId, std::time::Instant>,
    /// TLAS-refit fast path (P3.2): ordered unique *traceable* `cache_id`s from
    /// the last FULL assemble (`PtAccelCache::layout_ids`). When unchanged, instance
    /// count/order may vary while geometry SSBOs stay valid; only TLAS/instances/
    /// materials/meta re-upload.
    pub(crate) pt_last_instance_ids: Vec<core_math::scene_graph::MeshCacheId>,
    /// Counts uniform of the last FULL assemble — refit refreshes TLAS/instance/
    /// material counts while keeping geometry counts unchanged.
    pub(crate) pt_last_meta: crate::pt_scene::PtMetaGpu,
    /// Baked mesh-AO cache (AO_BAKE_PLAN.md): per-body per-vertex ambient occlusion
    /// for the RASTER deferred path, keyed by MeshCacheId + mesh_cache `content_stamp`
    /// (the same identity the PT BLAS uses → invalidates exactly when the mesh does).
    /// Fired by the explicit "Rebake AO" panel button; drained + uploaded in
    /// `poll_ao_bake` (poll phase, Frame Ordering Contract). NOT serialized (transient
    /// like pt_accel). The PT pass never binds it — PT does AO live in-shader.
    pub(crate) ao_bake: render::ao_bake::AoBakeCache,
    /// Per-body AO bake parameters (radius + quality preset), set from the panel and
    /// consumed by the next `request`. Radius default ≈ body-diagonal × 0.5 is filled
    /// at request time when this has no entry for the body.
    pub(crate) ao_bake_params: HashMap<core_math::scene_graph::MeshCacheId, render::ao_bake::AoBakeParams>,
    /// Bodies whose AO the user asked to (re)bake this session — drained by
    /// `poll_ao_bake` which kicks the on-demand readback + queues the bake. One-shot
    /// per press (D7: explicit rebake, never per-frame).
    pub(crate) ao_bake_requests: Vec<core_math::scene_graph::MeshCacheId>,
}

/// GATE 3 (DC consolidation) — THE explicit DC-mesh route. Replaces the implicit
/// `gpu_sdf_mesh` × `gpu_sdf_mesh_resident` (× body brick-residency) truth table that was
/// re-derived ad hoc at every dispatch/draw/pick site. The bools above REMAIN as the
/// Developer-Tweaks / test-harness override storage, but they are read by exactly ONE
/// decision site: [`CacheState::dc_mesh_route`]. Everything else consumes the route.
///
/// NOT routes (documented orthogonal policies, deliberately kept as bools):
/// - `gpu_sdf_mesh_resident_field` (G3): a per-frame-GUARDED field-SOURCE choice inside
///   `DenseResident` (`use_resident_field` in sdf_populate_resident falls back to the CPU-grid
///   upload whenever buf/res/bounds provenance fails). A route enum cannot express a per-frame
///   guard — forcing it in would either lie or move the guard.
/// - `gpu_sdf_mesh_lazy_readback`: a READBACK policy (skip the CPU `to_vec` on partial bakes),
///   not a mesher choice — the mesh route is unchanged by it.
/// - `gpu_full_bake_path_a`: a BAKE-write policy (full bake lands in rv.buffer in place).
/// - `gpu_sdf_mesh_sparse`: the legacy dense-grid >512 sparse PRE-PASS inside the resident
///   populate (S2.6 made ≥512 structurally sparse at bake; this only matters for oversize
///   dense grids reaching the populate).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DcMeshRoute {
    /// S2.4/3B production ≥512 path: the body is brick-resident (SparseBricks bake) and its
    /// per-body `SdfBrickAtlasGpu` still holds THIS bake's content — mesh tiled from the
    /// sparse atlas into the persistent resident slot.
    SparseTiledResident,
    /// G2/G3 production dense path: mesh into the persistent GPU-resident slot
    /// (field-direct vs CPU-grid-upload is the per-frame G3 guard, see above).
    DenseResident,
    /// Legacy G1 (Developer-Tweaks-only fallback): GPU QEF-DC + full readback into
    /// mesh_cache. Kept as an explicit variant — it is the documented "surface vanishes /
    /// selection breaks" escape hatch, not a production route.
    GpuReadbackMeshCache,
    /// Background CPU chunked DC (rayon job) — the parity/golden ORACLE and the
    /// GPU-mesher-off fallback. Reachable via Developer Tweaks (all GPU-mesh boxes off).
    CpuChunkedOracle,
}

impl CacheState {
    /// GATE 3 — THE one DC-route decision site. `brick_resident_atlas_ok` is body state
    /// (brick_resident_gen present AND the per-body atlas content matches), passed by the
    /// dispatch site; pass `false` when asking about the dense/base route only.
    ///
    /// Derivation is HIERARCHICAL, matching the Developer-Tweaks tree: `gpu_sdf_mesh` is the
    /// master gate (OFF → CPU oracle, period), `gpu_sdf_mesh_resident` selects the resident
    /// family, body brick-residency selects sparse-vs-dense within it. (This intentionally
    /// collapses the one accidental matrix cell — `gpu_sdf_mesh=false && resident=true` —
    /// which previously still ran the resident mesher while the drag-suppress/readback-defer
    /// gates treated the app as CPU-DC. That cell contradicted the UI tree and its own gates.)
    pub(crate) fn dc_mesh_route(&self, brick_resident_atlas_ok: bool) -> DcMeshRoute {
        if !self.gpu_sdf_mesh {
            DcMeshRoute::CpuChunkedOracle
        } else if self.gpu_sdf_mesh_resident {
            if brick_resident_atlas_ok {
                DcMeshRoute::SparseTiledResident
            } else {
                DcMeshRoute::DenseResident
            }
        } else {
            DcMeshRoute::GpuReadbackMeshCache
        }
    }

    /// Is a RESIDENT-slot route active (G2 family: dense or sparse-tiled)? The single
    /// predicate the draw gather / pick / PT-snapshot / LOD-policy gates key on — MUST stay
    /// lockstep with `dc_mesh_route` so a body is drawn/picked from the same source it is
    /// meshed into.
    pub(crate) fn dc_route_is_resident(&self) -> bool {
        matches!(
            self.dc_mesh_route(false),
            DcMeshRoute::DenseResident | DcMeshRoute::SparseTiledResident
        )
    }
}

/// How long a sparse body's source hash must hold still before a PT snapshot dispatches.
/// Matches the escalation settle scale (300ms): shorter re-enters the mid-gesture waste the
/// gate exists to kill; longer just delays the PT image after the user lets go.
pub(crate) const PT_SNAP_SETTLE: std::time::Duration = std::time::Duration::from_millis(300);

impl CacheState {
    pub(crate) fn new(cpu_volume: SdfVolume) -> Self {
        let (graph_bake_tx, graph_bake_rx) = std::sync::mpsc::channel();
        Self {
            cpu_volume,
            mesh_cache: MeshCache::new(),
            body_vol_cache: body_volume_cache::BodyVolumeCache::new(128, 2.0),
            cluster_bake: crate::cluster_bake::ClusterBakeState::default(),
            demo_tex_groups: Vec::new(),
            use_body_vol_cache: true,
            prim_buf_stale: true,
            cached_light_gpu_data: Vec::new(),
            light_gpu_data_stale: true,
            cached_frame_lights: None,
            shade_hysteresis: crate::light_pack::ShadeHysteresis::default(),
            cached_prim_bytes: Vec::new(),
            cached_prim_nv: 0,
            prim_buf_dirty_body: None,
            cached_body_byte_ranges: Vec::new(),
            cached_all_meshes_ready: None,
            rebake_hash: 0,
            body_index_map: HashMap::new(),
            suppress_body_vol_dirty: false,
            material_map_loads: Vec::new(),
            // 0 -> `.max(1.0)` at the ticket, so the FIRST readback always runs (nothing learned yet).
            bake_readback_worst_ms: 0.0,
            bake_readback_deferred_frames: 0,
            mesh_mod_prefix_cache: Default::default(),
            graph_bake_tx,
            graph_bake_rx,
            graph_bake_gpu_inflight: Vec::new(),
            graph_bake_seq: 0,
            graph_bake_pending: HashMap::new(),
            graph_live_owner: None,
            graph_live_dirty: false,
            graph_bake_map_cache: HashMap::new(),
            graph_bake_dirty: HashMap::new(),
            graph_live_in_flight: false,
            graph_live_channel_filter: None,
            graph_curv_pending: Vec::new(),
            graph_curv_inflight: Vec::new(),
            convert_sdf_pending: None,
            convert_sdf_inflight: None,
            material_array_reset_pending: false,
            material_maps_restore: None,
            smooth_dc_normals: true,
            pending_dc_jobs: Vec::new(),
            warming: warming::WarmingState::new(),
            mat_preview_cache: crate::mat_preview::MaterialPreviewCache::new(),
            map_thumb_cache: crate::material_textures::MapThumbCache::new(),
            uv_underlay_cache: crate::mesh_map_bake::UvUnderlayCache::new(),
            displace_hm_thumbs: HashMap::new(),
            pt_accel: render::pt_accel::PtAccelCache::new(),
            pt_last_scene_hash: 0,
            pt_instance_count: 0,
            pt_sdf_snap_hash: HashMap::new(),
            pt_sdf_snap_seen: HashMap::new(),
            pt_sdf_snap_pending: Vec::new(),
            pt_last_instance_ids: Vec::new(),
            pt_last_meta: bytemuck::Zeroable::zeroed(),
            ao_bake: render::ao_bake::AoBakeCache::new(),
            ao_bake_params: HashMap::new(),
            ao_bake_requests: Vec::new(),
            post_load_bodies: Vec::new(),
            post_load_warmup: 0,
            gpu_sdf_mesh: true, // Phase G default ON (CPU DC remains the Developer-Tweaks-off fallback)
            sdf_mesher: None,
            pending_sdf_mesh: Vec::new(),
            // "Before step 2" — the dense resident + field-direct state the owner confirmed ran
            // smooth at 128³. G2 (resident slot + indirect draw) + G3 (DC field-direct from rv.buffer)
            // ON; lazy readback OFF. The comb/trailing came from the LAZY partial-bake stale-seed
            // (needs lazy to manifest); with lazy OFF the full readback keeps body_grids fresh, so the
            // seed is correct and the resident path is clean. Seed-fix + draft experiments reverted.
            gpu_sdf_mesh_resident: true,       // G2 — resident GPU mesh + indirect draw
            gpu_sdf_mesh_resident_field: true, // G3 — DC field-direct from rv.buffer (the accumulated field)
            gpu_sdf_mesh_sparse: true,         // Phase 3B — sparse brick mesh (default ON per owner; earlier
                                               // white-cube issues traced to stale-atlas/runaway, now fixed)
            // Phase 1: ON now that path A (bake the dirty slab in place into rv.buffer, app_poll.rs ~1110)
            // removed the stale-CPU-grid seed that caused the comb/trailing. The partial bake accumulates
            // in rv.buffer; the field-direct DC meshes it; this skips the ~48ms CPU to_vec readback during
            // drag → 256³ editing is fast. body_grids self-heals on the settle full re-tighten bake.
            gpu_sdf_mesh_lazy_readback: true,
            gpu_full_bake_path_a: true,  // default ON — user-confirmed: kills the drag-ghost + the full-bake
                                         // hitch; bounds sourced synchronously so a bevel/escape can't smear
            // (S2.6: sparse_bake_debug removed — sparse ≥512 is structural; see the field note above)
            poly_custom_data: HashMap::new(), // POLY P3 — per-body runtime CustomData sidecar (empty until authored)
            pending_mesh_map_bake: None,
            mesh_map_rx: None,
            pending_baked_export: None,
            mesh_mod_display_stamp: HashMap::new(),
            mesh_mod_base: HashMap::new(),
            sdf_lod_auto: false, // O2b — default OFF for the first ship (owner A/B via Developer Tweaks)
            poly_lod_auto: true, // P3 poly display LOD — object mode; edit stays full
            poly_mesh_gpu_key: HashMap::new(),
            sdf_lod_last_switch: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod dc_route_tests {
    use super::*;

    fn state_with(mesh: bool, resident: bool) -> CacheState {
        let mut s = CacheState::new(SdfVolume::new_empty(8, 2.0));
        s.gpu_sdf_mesh = mesh;
        s.gpu_sdf_mesh_resident = resident;
        s
    }

    /// GATE 3 derivation table — every (gpu_sdf_mesh, resident, brick_resident_atlas_ok)
    /// combination maps to exactly the documented route. This IS the flag matrix, now
    /// explicit and pinned.
    #[test]
    fn dc_route_derivation_table() {
        use DcMeshRoute::*;
        let table: &[(bool, bool, bool, DcMeshRoute)] = &[
            // (gpu_sdf_mesh, resident, brick_ok) → route
            (true,  true,  true,  SparseTiledResident),
            (true,  true,  false, DenseResident),
            (true,  false, true,  GpuReadbackMeshCache), // brick body, resident OFF → dispatch skips (no CPU grid); base route is readback
            (true,  false, false, GpuReadbackMeshCache),
            (false, true,  true,  CpuChunkedOracle), // the collapsed accidental cell (see dc_mesh_route doc)
            (false, true,  false, CpuChunkedOracle),
            (false, false, true,  CpuChunkedOracle),
            (false, false, false, CpuChunkedOracle),
        ];
        for &(m, r, b, expect) in table {
            let s = state_with(m, r);
            assert_eq!(
                s.dc_mesh_route(b), expect,
                "route mismatch for gpu_sdf_mesh={m} resident={r} brick_ok={b}"
            );
        }
    }

    /// Default (production) config routes DenseResident / SparseTiledResident and the
    /// resident-family predicate (draw/pick/PT/LOD gate) agrees with the route.
    #[test]
    fn dc_route_default_config_is_resident() {
        let s = CacheState::new(SdfVolume::new_empty(8, 2.0));
        assert_eq!(s.dc_mesh_route(false), DcMeshRoute::DenseResident);
        assert_eq!(s.dc_mesh_route(true), DcMeshRoute::SparseTiledResident);
        assert!(s.dc_route_is_resident());
        // The oracle override (all GPU-mesh boxes off) reaches the parity oracle.
        assert!(!state_with(false, false).dc_route_is_resident());
        // Resident-family predicate is lockstep with the route family for every combo.
        for &(m, r) in &[(true, true), (true, false), (false, true), (false, false)] {
            let s = state_with(m, r);
            let fam = matches!(
                s.dc_mesh_route(false),
                DcMeshRoute::DenseResident | DcMeshRoute::SparseTiledResident
            );
            assert_eq!(s.dc_route_is_resident(), fam);
        }
    }
}
