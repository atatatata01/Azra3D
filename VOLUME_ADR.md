# Volume Representation ADR — Finalized

**Date:** 2026-03-29
**Status:** Finalized (code-backed analysis, no runtime instrumentation yet)
**Author:** Automated code audit

---

## Active Representations

### 1. SdfVolume (cpu_volume)

| Attribute | Value |
|-----------|-------|
| **What it stores** | Dense flat `Vec<f32>` grid of SDF distance values. `resolution[0]^3` voxels. Fields: `data`, `resolution: [u32;3]`, `grid_bounds: f32`, `dirty_region: Option<DirtyRegion>`. |
| **Owner** | `CacheState::cpu_volume` (single global instance) |
| **CPU cost** | `bake_from_primitives()`: O(N_prims x res^3) triple-nested loop. `apply_brush_advanced()`: localized to dirty AABB via `SculptBrush::apply_stamp()`. `get_distance()`: trilinear interpolation (8 samples). |
| **GPU cost** | Zero compute. Upload-only via `SdfGpuVolume::upload()` (dirty Z-slab) or `upload_full()`. |
| **Memory** | 128^3 x 4 bytes = 8 MB RAM. Single instance. |
| **Role** | (1) Sculpt mode backend: `SculptBackend` trait impl enables brush tools to read/write voxels directly. (2) Resolution/bounds source: `trigger_body_vol_bake()` reads `cpu_volume.resolution[0]` and `cpu_volume.grid_bounds` to parameterize per-body bakes. (3) Legacy composite sink: when `use_body_vol_cache=false`, `rebuild_composite` updates `cpu_volume.grid_bounds` and `cpu_volume.resolution`. |

**Files:** `core_math/src/sdf_volume.rs` (369 lines)

### 2. BodyVolumeCache (per-body cached grids)

| Attribute | Value |
|-----------|-------|
| **What it stores** | `HashMap<NodeId, BodyCacheEntry>` where each entry holds a `Vec<f32>` grid with per-body AABB bounds (`grid_min`/`grid_max`), `source_hash`, `bake_resolution`, and optional `shared_source` for duplicated bodies. Plus: `dirty_regions` (body-local AABBs), progressive `cascades`, `ram_cache`, generation counters, and draft-resolution tracking. |
| **Owner** | `CacheState::body_vol_cache` (single instance, manages all bodies) |
| **CPU cost** | `bake_body_grid()` (bake_utils.rs): O(N_body_prims x res^3). `bake_body_grid_region()`: O(N_body_prims x dirty_voxels). `rebuild_composite()`: O(N_bodies x res^3) trilinear resampling (667ms measured at 10 bodies x 128^3 -- but skipped in default cache-only mode). |
| **GPU cost** | GPU bake path: one compute dispatch per body via `PrimaryBakeRequest` or `GpuJobDispatcher`. Upload via `ResidentVolume::upload_full()` or `upload_subregion()`. |
| **Memory** | Per body active grid: res^3 x 4 bytes. At 128^3: 8 MB per body. 10 bodies = 80 MB. RAM cache tier adds up to 2 GB budget (`ram_cache_budget_bytes`) for demoted grids. |
| **Role** | Core bake system. Single-body edits rebake only the affected body. Dirty region tracking (`BakeDirtyRegion`) enables partial bakes. Progressive cascade: sync 64^3 then async 128^3 then 256^3. RAM cache survives undo/redo (validated by hash+resolution+bounds+epoch). |

**Files:** `app/src/body_volume_cache.rs` (2,693 lines), `app/src/bake_utils.rs` (403 lines) = **3,096 total**

### 3. SdfGpuVolume (global GPU volume)

| Attribute | Value |
|-----------|-------|
| **What it stores** | Flat `wgpu::Buffer` (f32, STORAGE + COPY_DST) and a 3D `wgpu::Texture` (R16Float, TEXTURE_BINDING + COPY_DST) with hardware trilinear sampler. Clamped to 256^3 if `size_bytes > device.limits().max_buffer_size`. |
| **Owner** | `GpuCtx::gpu_volume` (single instance on render device) |
| **CPU cost** | Upload only. `upload_full()`: f32->f16 conversion + two queue writes. `upload()`: dirty Z-slab variant for sculpt mode. |
| **GPU cost** | Read-only binding at slot 2 in raymarch/deferred bind groups. Used by `scene_voxel()` in shader when `render_mode==1` (sculpt) or `render_mode==2` (SVO hybrid). |
| **Memory** | Buffer: res^3 x 4 bytes = 8 MB at 128^3. Texture: res^3 x 2 bytes = 4 MB at 128^3. **Total: 12 MB.** |
| **Role** | Two active uses: (1) Sculpt mode: brush feedback uploaded via `gpu_volume.upload(&queue, &cpu_volume, &dirty)` in viewport_render.rs. (2) SVO hybrid path (`render_mode==2`): receives composite grid via `upload_full()`. Always bound in bind groups even when not read. |

**Files:** `graphics/src/gpu_volume.rs` lines 1-163

### 4. ResidentVolume + VolumePool (per-body GPU volume)

| Attribute | Value |
|-----------|-------|
| **What it stores** | Per-body: persistent GPU 3D texture (R16Float) + flat storage buffer (f32). Tracks `body_nid`, `resolution`, `bounds_min`/`bounds_max`, `buffer_bytes`. Pool: `Vec<Option<VolumePoolSlot>>` with LRU eviction + freelist (max 4 recycled volumes). |
| **Owner** | `BodyVolumeCache::volume_pool` (pool bookkeeping) + slots hold `ResidentVolume` instances. Active body tracked by `resident_body_nid` / `resident_slot`. |
| **CPU cost** | `upload_full()`: f32->f16 conversion + two queue writes. `upload_subregion()`: row-by-row f16 extraction + tight-pitch texture write. Pool operations: O(max_slots) for LRU eviction. `needs_realloc()`: 6 float comparisons. |
| **GPU cost** | Read-only binding for volume preview / deferred sampling. Buffer also serves as GPU bake output target. |
| **Memory** | Per slot at 128^3: buffer=8 MB + texture=4 MB = **12 MB**. Pool default: 8 slots = **96 MB GPU**. Scaling: 4 GB VRAM -> 4 slots (48 MB), 8 GB -> 8 slots (96 MB), 24 GB -> 16 slots (192 MB). Freelist: up to 4 evicted volumes retained for reuse. |
| **Role** | Active body's cached volume for GPU rendering. Sub-region upload for dirty bake results. Pool provides O(1) body switching when target is already resident (LRU eviction when full). This is the mainline GPU volume representation when `use_body_vol_cache=true`. |

**Files:** `graphics/src/gpu_volume.rs` lines 165-717 (ResidentVolume + VolumePool + VolumePoolSlot + tests) = **554 lines**

### 5. Composite Grid

| Attribute | Value |
|-----------|-------|
| **What it stores** | Single dense `Vec<f32>` grid combining all body grids via per-body boolean ops (union, subtract, intersect). Symmetric around origin with `composite_half_extent`. |
| **Owner** | `BodyVolumeCache::composite_grid` |
| **CPU cost** | `rebuild_composite()`: O(N_bodies x res^3) with trilinear resampling per body. Measured: 667ms at scale (code comment). |
| **GPU cost** | Zero compute. Uploaded to `SdfGpuVolume` via `upload_full()`. |
| **Memory** | res^3 x 4 bytes = 8 MB at 128^3. |
| **Role** | Used ONLY by SVO hybrid path (`render_mode==2`) for cross-body shadow/AO via `scene_voxel()`. **Effectively dead code in mainline.** Guarded by: `composite_needed = !self.cache.use_body_vol_cache && self.cache.svo_cache_enabled && self.cache.svo_cache_ready`. When `use_body_vol_cache=true`, this is always skipped. |

**Files:** `app/src/body_volume_cache.rs` lines 1319-1418 (~100 lines)

### 6. ChunkedDC (Dual Contouring mesh extraction)

| Attribute | Value |
|-----------|-------|
| **What it stores** | `HashMap<(u32,u32,u32), ChunkMesh>` per body -- per-chunk vertex/normal/quad data. 32^3 cells per chunk. Supports `FlatGridAccessor` (dense) and `BrickGridAccessor` (sparse) via `VolumeAccessor` trait. |
| **Owner** | `BodyVolumeCache::body_chunked_meshes: HashMap<NodeId, ChunkedMesh>` |
| **CPU cost** | Full extraction at 128^3: 64 chunks (4x4x4 of 32^3). Incremental: only re-extracts chunks overlapping dirty region via `dirty_chunk_coords()`. Per chunk: sign-change detection, QEF solve for sharp features, quad generation. Dispatched to rayon background thread (`dispatch_grid_dc_async`). |
| **GPU cost** | Zero compute. Output mesh uploaded to `mesh_cache` as vertex/index buffers. |
| **Memory** | Proportional to surface area. Typical body: 5K-50K vertices x (pos+normal+color = ~40 bytes) = 200KB-2MB per body. |
| **Role** | Converts SDF grid to renderable triangle mesh for the raster pipeline. Incremental extraction avoids re-meshing clean chunks. Normals via tetrahedron method (4 trilinear samples). Smooth normals toggle (`smooth_dc_normals`). |

**Files:** `core_math/src/chunked_dc.rs` (916 lines)

### 7. RAM Cache (LRU grid cache)

| Attribute | Value |
|-----------|-------|
| **What it stores** | `HashMap<NodeId, RamCacheEntry>` -- demoted body grids with `source_hash`, bounds, resolution, and `invalidation_epoch`. |
| **Owner** | `BodyVolumeCache::ram_cache` |
| **CPU cost** | Lookup: O(1) HashMap. No bake cost on hit. Validation: hash + resolution + bounds + epoch match. |
| **GPU cost** | Zero. |
| **Memory** | Budget: 2 GB default (`ram_cache_budget_bytes`). Tracks `ram_cache_used_bytes`. Feature-flagged by `ram_cache_enabled`. |
| **Role** | Survives `invalidate_for_undo()`. When undo demotes active grids, matching grids can be restored from RAM cache without rebaking. Eliminates the most expensive undo cost: full rebake of every body. |

**Files:** `app/src/body_volume_cache.rs` (RamCacheEntry struct + usage in trigger_bake, ~80 lines)

### 8. Brick Pool (PARKED)

| Attribute | Value |
|-----------|-------|
| **What it stores** | Fixed-size brick allocator: single large GPU SSBO holding 8^3 f32 bricks. Per-body `BodyBrickVolume` with indirection tables mapping brick grid coords to pool slots. CPU-side mirror (`cpu_data` + `cpu_slot_map`). |
| **Owner** | `BodyVolumeCache::brick_pool` (shared pool) + `body_brick_volumes` (per-body indirection) |
| **CPU cost** | Classification: scan brick corners + center to detect surface proximity. Copy: 512 f32 per active brick. |
| **GPU cost** | SSBO + indirection buffer. Brick fill shader (Phase 4, unvalidated). |
| **Memory** | Pool budget: 128 MB default (65K bricks). Active bricks proportional to surface area. Typical body: ~200-500 active bricks of 4K total (128^3 / 8^3) = 400KB-1MB CPU+GPU per body. Indirection: 4K x 4 bytes = 16KB per body. |
| **Role** | Sparse refinement layer. Only allocates bricks near surface. **PARKED with 23 tracked bugs** (`BRICK_CACHE_BUG_AUDIT.md`). Feature flags `use_brick_pool` and `use_brick_bake` both default to `false`. |

**Files:** `graphics/src/brick_pool.rs` (606 lines), classification in `body_volume_cache.rs` (~250 lines)

---

## Render Mode -> Volume Path Mapping

### render_mode assignment (viewport_render.rs:579)

```rust
let rm = if self.work_mode == WorkMode::Sculpt { 1u32 }
    else if self.cache.svo_cache_enabled && self.cache.svo_cache_ready && !self.cache.use_body_vol_cache { 2u32 }
    else { 0u32 };
```

### scene_sdf dispatch (sdf_scene.wgsl:344-348)

```wgsl
fn scene_sdf(p: vec3<f32>) -> f32 {
    if (config.render_mode == 0u) { return scene_analytical(p); }
    if (config.render_mode == 2u) { return scene_svo(p); }
    return scene_voxel(p); // render_mode == 1u: Sculpt mode
}
```

### Full mapping

| Render Mode | Condition | Volume Source | Upload Path | Status |
|---|---|---|---|---|
| **rm=0 Analytical** (default) | `!Sculpt && (use_body_vol_cache \|\| !svo_cache_enabled \|\| !svo_cache_ready)` | Direct SDF evaluation via `scene_analytical()`. Per-body DC mesh cache for raster. | Prim buffer upload. DC mesh -> `mesh_cache`. `ResidentVolume` for volume preview. | **MAINLINE** |
| **rm=1 Sculpt** | `WorkMode::Sculpt` | `SdfVolume` -> `SdfGpuVolume` -> `scene_voxel()` | `gpu_volume.upload()` with dirty Z-slab on each brush stroke (viewport_render.rs:233) | **Active for sculpt only** |
| **rm=2 SVO Hybrid** | `svo_cache_enabled && svo_cache_ready && !use_body_vol_cache` | Composite grid -> `SdfGpuVolume` -> `scene_svo()` (voxel + analytical blend) | `rebuild_composite()` -> `gpu_volume.upload_full()` | **Dead in practice.** Requires `use_body_vol_cache=false` which is the default, but `svo_cache_enabled` defaults to `false` too. Only reachable via stress tests or manual toggle. |
| **Hybrid selective** (during gizmo drag) | `use_body_vol_cache && gizmo active` | Active body: analytical SDF march. Inactive bodies: raster from DC mesh. | Active body excluded from raster G-buffer pass. Prim buffer for analytical. | **Active sub-mode of rm=0** |
| **Volume preview** | `use_body_vol_cache && active body has grid` | `ResidentVolume` 3D texture in pool slot | `update_resident_volume()` -> `upload_full()` or `upload_subregion()` | **Active** |

### Activation state of `use_body_vol_cache`

- **Default:** `false` (cache_state.rs:62)
- **Set to `true`:** User clicks "Per-Body Cache" toggle in properties_render.rs, or stress tests
- **Set to `false`:** New project (project_new.rs:74), project load (project_load.rs:310), user clicks "Analytical" toggle
- **Guards:** `poll_body_vol_cache()` returns early when false (app_poll.rs:395). `update_resident_volume()` returns early when false (app_poll.rs:675). Composite rebuild only runs when false.
- **Impact of transition to true-by-default:** The per-body cache path is fully wired. The comment at viewport_render.rs:1485-1488 notes auto-enable is deferred until `active_body_nid` tracking is reliable.

---

## Bake Dispatch Pipeline

```
mark_prim_dirty() / mark_body_dirty()
    |
    v
dirty_bodies set + dirty_regions (optional partial)
    |
    v  [debounce 40ms, bypass for partial regions]
trigger_bake()
    |
    +-- RAM cache hit? -> restore grid, skip bake
    |
    +-- No existing grid? -> Level 4 cascade: sync 64^3 + async 128^3
    |
    +-- GPU worker available? -> GpuJob dispatch via GpuJobDispatcher
    |
    +-- Primary GPU pipeline ready? -> PrimaryBakeRequest (deferred to main loop)
    |
    +-- Fallback: rayon CPU bake (bake_body_grid / bake_body_grid_region)
    |
    v
poll() receives BodyBakeResult
    |
    +-- generation check (discard stale)
    +-- insert into body_grids
    +-- set resident_upload_needed (if active body)
    +-- set composite_dirty
    |
    v
dispatch_grid_dc_async() -> rayon DC extraction -> mesh_cache update
update_resident_volume() -> GPU texture upload (full or sub-region)
classify_bricks_for_body() -> brick pool (if use_brick_pool, currently disabled)
advance_cascade() -> dispatch next resolution level if cascade active
```

---

## Decision Criteria Analysis

### 1. Latency during single-body edit (target: <5ms CPU, <3ms GPU)

| Path | CPU Cost | GPU Cost | Meets Target? |
|---|---|---|---|
| **Partial bake (dirty region ~20^3)** | ~0.5ms (8K voxels x N_prims) | ~0.3ms sub-region upload | **YES** |
| **Full CPU bake 128^3** | ~15-30ms (2M voxels x N_prims, rayon) | ~2ms full upload (8MB buffer + 4MB texture) | NO (CPU) |
| **GPU bake 128^3 (primary device)** | <1ms dispatch overhead | ~1-3ms compute | **YES** |
| **Progressive cascade 64^3 sync** | ~2-4ms | ~0.5ms upload | **YES** |
| **DC extraction (incremental, ~4 dirty chunks)** | ~2-5ms on rayon thread (async) | ~0.5ms mesh upload | **YES** |
| **DC extraction (full, 64 chunks)** | ~20-40ms on rayon thread (async) | ~1ms mesh upload | NO (CPU), but async so non-blocking |
| **Composite rebuild 128^3** | ~667ms (measured) | ~2ms upload | **NO** (skipped in mainline) |

**Assessment:** The partial bake + sub-region upload path meets the target. Full bakes are mitigated by progressive cascade (instant 64^3, async 128^3). The 667ms composite rebuild is the only real failure -- but it is correctly guarded out when `use_body_vol_cache=true`.

### 2. Memory at scale (10 bodies x 128^3)

| Component | Per-Body | 10 Bodies | Notes |
|---|---|---|---|
| Body grid (CPU) | 8 MB | 80 MB | Active in `body_grids` HashMap |
| RAM cache (CPU) | 8 MB | 80 MB | If all 10 demoted after undo |
| ResidentVolume buffer (GPU) | 8 MB | 80 MB | If all 10 in pool (needs 10+ slots) |
| ResidentVolume texture (GPU) | 4 MB | 40 MB | R16Float 3D texture |
| DC mesh (CPU+GPU) | ~1 MB | ~10 MB | Surface-proportional |
| Brick pool (GPU) | N/A | 128 MB budget | Shared SSBO, if enabled |
| Composite grid (CPU) | N/A | 8 MB | Single instance (unused in mainline) |
| SdfGpuVolume (GPU) | N/A | 12 MB | Single instance (always allocated) |
| **Total CPU (mainline)** | | **~90 MB** | body_grids + DC mesh |
| **Total GPU (mainline)** | | **~142 MB** | pool slots + DC mesh + SdfGpuVolume |

At 256^3: memory 8x for grids. 640 MB CPU body_grids, 960 MB GPU pool. Workable on 24 GB GPUs but tight on 8 GB.

### 3. DC extraction quality

| Resolution | Voxel Size (1m body) | Feature Detail | Mesh Quality |
|---|---|---|---|
| **64^3** | ~15.6mm | Coarse | Acceptable for instant preview (cascade level 0) |
| **128^3** | ~7.8mm | Good | Default production quality. QEF sharp features + tetrahedron normals. |
| **256^3** | ~3.9mm | High | Needed for fine detail. 8x memory and bake cost. |
| **Brick-refined** | ~1mm (effective 1024^3) | Very high | Best quality-to-memory ratio, but 23 bugs. |

### 4. Implementation complexity

| Component | Lines of Code | Known Bug Count | Stability |
|---|---|---|---|
| SdfVolume | 369 | 0 | Stable, simple |
| BodyVolumeCache + bake_utils | 2,693 + 403 = 3,096 | ~5 (dirty region edge cases) | Complex but functional |
| SdfGpuVolume | 163 | 0 | Stable |
| ResidentVolume + VolumePool | 554 | 0 | Stable, has tests |
| ChunkedDC | 916 | 0 | Stable, well-tested |
| Composite rebuild | ~100 (within BVC) | 1 (667ms CPU stall) | Dead code in mainline |
| Brick Pool + BodyBrickVolume | 606 + ~250 = ~856 | **23 tracked** | PARKED |
| RAM Cache | ~80 (within BVC) | 0 | Simple |
| **Total** | **~5,764** | **~29** | |

---

## Decision

### Mainline: Per-body cache (BodyVolumeCache + ResidentVolume + ChunkedDC)

The per-body cache path is the only volume system that:
- Supports partial bakes (dirty regions -> sub-region re-evaluation)
- Supports progressive cascade (instant 64^3 -> async 128^3 -> 256^3)
- Supports multi-body scenes without O(N_bodies x res^3) recomposite
- Supports incremental DC mesh extraction (only dirty chunks)
- Has an LRU volume pool for GPU-side multi-body switching
- Has a RAM cache tier for undo/redo without rebake

**Action:** Make `use_body_vol_cache = true` the default. The remaining blocker (noted at viewport_render.rs:1485) is reliable `active_body_nid` tracking. Once that is resolved, flip the default.

### Cut: Composite grid + SVO hybrid path (rm=2)

The composite grid is dead code in practice:
- Only reachable when `!use_body_vol_cache && svo_cache_enabled && svo_cache_ready`
- The 667ms rebuild makes it unusable at scale
- No user workflow depends on it (the per-body raster path handles all rendering)
- `scene_svo()` in the shader is only called when `render_mode==2`

**Action:** Remove `rebuild_composite()`, `composite_grid`, `composite_dirty`, `composite_half_extent` from `BodyVolumeCache`. Remove the `render_mode==2` branch from `scene_sdf()` and `scene_sdf_culled()` in sdf_scene.wgsl. Remove `scene_svo()` and `scene_voxel()` if sculpt mode is migrated to ResidentVolume (see Defer below).

### Cut: `svo_cache_enabled`, `svo_cache_ready`, `svo_bake_rx/gen/hash/rebake_hash` fields

These in `CacheState` serve only the legacy SVO bake path:
- `svo_cache_enabled` defaults false
- `svo_bake_rx` is the old single-body bake channel (replaced by per-body `bake_rx`)
- `svo_bake_hash`/`rebake_hash` are stale duplicate-detection for the old path

**Action:** Remove after composite grid removal. The `svo_status` field can remain for diagnostics.

### Defer: SdfVolume + SdfGpuVolume

SdfVolume is still required for sculpt mode (`apply_brush_advanced`, `raycast`, `get_distance`, `snapshot_region`, `restore_delta`). SdfGpuVolume provides the GPU side for sculpt brush feedback (dirty Z-slab upload at viewport_render.rs:233).

The sculpt system would need significant rework to use per-body ResidentVolume instead (body-local coordinate transforms, multi-body sculpt, prim-level vs voxel-level distinction). This is not justified until sculpt mode gets a dedicated development cycle.

**Action:** Keep both alive but document that SdfGpuVolume binding slot 2 is wasted memory for non-sculpt workflows. Consider lazy allocation (only create when sculpt mode is entered). Also note that `trigger_body_vol_bake()` reads resolution/bounds from `cpu_volume` -- this coupling should be broken by storing resolution/bounds directly on `BodyVolumeCache`.

### Defer: Brick Pool

Per CLAUDE.md "Brick Pool Status": PARKED. Prerequisites before investment:
1. Base DC mesh cache threaded (done -- rayon `dispatch_grid_dc_async`)
2. Wave 2 audit fixes landed (in progress)
3. Explicit user request to resume

The 23 tracked bugs span: allocation leaks (BUG-01), missing deallocation on body delete, GPU upload validation, indirection buffer sizing, and DC extraction integration. Estimated 2-3 developer-weeks to fix all.

**Action:** Do not invest. If 256^3+ resolution becomes a user requirement, evaluate fresh sparse grid implementation (OpenVDB-style) vs fixing existing brick pool.

---

## Cleanup Task List

### Phase 1: Remove composite grid (safe, zero user impact)

| File | Section | Grep Pattern | Action |
|---|---|---|---|
| `app/src/body_volume_cache.rs` | `composite_grid`, `composite_dirty`, `composite_half_extent` fields | `composite_grid\|composite_dirty\|composite_half_extent` | Remove fields + `rebuild_composite()` + helper `sample_body_grid()` + `combine_sdf()` |
| `app/src/app_poll.rs` | Lines 444-465 | `composite_needed\|rebuild_composite` | Remove entire `composite_needed` block |
| `graphics/src/sdf_scene.wgsl` | `scene_svo()`, `scene_voxel()` | `fn scene_svo\|fn scene_voxel\|fn read_voxel` | Remove functions. Remove `render_mode == 2u` branch from `scene_sdf` and `scene_sdf_culled`. **Keep `scene_voxel` if sculpt mode still needs `render_mode==1`.** |
| `app/src/cache_state.rs` | `svo_cache_enabled`, `svo_cache_ready`, `svo_bake_rx`, `svo_bake_gen`, `svo_bake_hash`, `rebake_hash`, `svo_cache_res` | `svo_cache_enabled\|svo_cache_ready\|svo_bake_rx\|svo_bake_gen\|svo_bake_hash\|rebake_hash` | Remove fields + initialization. Keep `svo_status` for diagnostics. |
| `app/src/viewport_render.rs` | rm=2 branch at line 579 | `svo_cache_enabled&&self.cache.svo_cache_ready&&!self.cache.use_body_vol_cache` | Simplify to: `let rm = if self.work_mode == WorkMode::Sculpt { 1u32 } else { 0u32 };` |

### Phase 2: Decouple resolution/bounds from cpu_volume

| File | Section | Grep Pattern | Action |
|---|---|---|---|
| `app/src/dirty.rs` | `trigger_body_vol_bake` lines 192-193 | `cpu_volume.resolution\|cpu_volume.grid_bounds` | Read from `body_vol_cache.resolution` and `body_vol_cache.bounds` directly (both fields already exist) |

### Phase 3: Default `use_body_vol_cache = true`

| File | Section | Grep Pattern | Action |
|---|---|---|---|
| `app/src/cache_state.rs` | Line 62 | `use_body_vol_cache: false` | Change to `true` after `active_body_nid` tracking is validated |
| `app/src/project_new.rs` | Line 74 | `use_body_vol_cache = false` | Change to `true` |
| `app/src/project_load.rs` | Line 310 | `use_body_vol_cache = false` | Change to `true` |

### Phase 4: Lazy SdfGpuVolume allocation (optional, saves 12 MB GPU)

| File | Section | Grep Pattern | Action |
|---|---|---|---|
| `graphics/src/gpu_volume.rs` | `SdfGpuVolume::new()` | N/A | Wrap in `Option<SdfGpuVolume>`, allocate only on sculpt mode entry |
| `app/src/viewport_render.rs` | All `gpu.gpu_volume` references | `gpu_volume\.` | Guard with `if let Some(ref gv) = gpu.gpu_volume` |

---

## Questions Resolved

### Q1: Is the composite grid needed if per-body rendering is mainline?

**Answer: No.** The composite grid exists solely for `scene_voxel()` / `scene_svo()` in the shader (render_mode 1 and 2). With per-body raster rendering as the default (render_mode 0), no shader reads the composite during normal operation. Cross-body effects (shadows, AO) use the analytical SDF path which evaluates all prims on-the-fly. The composite rebuild is already guarded by `!self.cache.use_body_vol_cache` and adds 667ms CPU stall when triggered.

**Evidence:** `app/src/app_poll.rs:449` -- `composite_needed = !self.cache.use_body_vol_cache && ...`. When `use_body_vol_cache=true`, `composite_needed` is always false.

### Q2: Can ResidentVolume serve multi-body scenes?

**Answer: Yes, with current pool architecture.** VolumePool manages up to 16 slots (24 GB VRAM). For rendering, each body's DC mesh is independent -- the raster pipeline draws them as separate mesh instances. ResidentVolume is needed only for: volume preview of the active body, GPU bake output target, and potential future deferred shading. Bodies not in the pool render from DC mesh cache (no volume texture needed). The bottleneck at 256^3 is pool capacity: 96 MB per slot, so 16 slots = 1.5 GB.

**Evidence:** `graphics/src/gpu_volume.rs:579-582` -- `max_slots_for_vram()` scales slots with VRAM. `app/src/app_poll.rs:486-562` -- `dispatch_grid_dc_async()` works for all bodies regardless of pool residency.

### Q3: Is `SdfVolume` (cpu_volume) still used for anything other than sculpt mode?

**Answer: Yes, but only as a parameter source.** Two non-sculpt uses:
1. `trigger_body_vol_bake()` reads `cpu_volume.resolution[0]` and `cpu_volume.grid_bounds` to parameterize per-body bakes (dirty.rs:192-193).
2. After composite rebuild, `cpu_volume.grid_bounds` and `cpu_volume.resolution` are updated to match the composite (app_poll.rs:462-463) -- but this path is dead when `use_body_vol_cache=true`.

Use (1) is an accidental coupling: `BodyVolumeCache` has its own `resolution` and `bounds` fields. Breaking this coupling is trivial (Phase 2 cleanup).

### Q4: Is composite grid rebuild still active?

**Answer: Only when `use_body_vol_cache=false` (the current default).** The guard at app_poll.rs:449 prevents the rebuild in the per-body cache path. However, since `use_body_vol_cache` currently defaults to `false`, the composite path is technically reachable if `svo_cache_enabled` is toggled on. In practice, `svo_cache_enabled` also defaults to `false`, so the composite never rebuilds unless both flags are manually toggled.

### Q5: Can per-body cached rendering serve ALL current use cases?

**Answer: All except sculpt mode.** The per-body cache + DC mesh + raster pipeline handles: creation, editing, gizmo transform, undo/redo, multi-body scenes, body switching, project save/load. Sculpt mode requires direct voxel read/write which is architecturally different from SDF prim-based baking.

---

## Line Count Summary

| File | Lines | Role |
|---|---|---|
| `app/src/body_volume_cache.rs` | 2,693 | Per-body cache, bake dispatch, pool management, brick classification |
| `core_math/src/chunked_dc.rs` | 916 | DC mesh extraction |
| `graphics/src/gpu_volume.rs` | 717 | SdfGpuVolume + ResidentVolume + VolumePool |
| `graphics/src/brick_pool.rs` | 606 | Brick pool allocator + BodyBrickVolume (PARKED) |
| `app/src/bake_utils.rs` | 403 | Grid bake, bounds, hashing |
| `core_math/src/sdf_volume.rs` | 369 | CPU SDF volume (sculpt backend) |
| **Total** | **5,704** | |

---

*Finalized 2026-03-29. Runtime instrumentation (P50/P99 bake latency, GPU upload timing, DC wall-clock per chunk, memory high-water mark) is recommended to confirm the cost estimates above.*
