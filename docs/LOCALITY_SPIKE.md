# Locality Spike — edit cost ∝ EDIT size

Branch: `feat/kernel-locality` · Worktree: `.claude/worktrees/kernel-locality` · Base: `master` @ `f66ef1e`  
Audit reference: [PROCEDURAL_FEASIBILITY.md](./PROCEDURAL_FEASIBILITY.md) kill-criterion **C** (fc75e0e)  
Method: `cargo run -p op_bench --release -- --c-only` (median ms; setup excludes clone + capacity reserve)

**Do not build the op-graph.** This spike only makes loop cut + position-tier tails local.

---

## Gates

| Gate | Target | Result |
|---|---|---|
| loop cut cost ∝ ring (not mesh) | XL ring-normalized within ~3× of S | **PASS** — bare plane XL/S ≈ 43× for ring ratio ~33×; residual ≲1.3× |
| `extrude_face` XL | **&lt; 10 ms** | **PASS** — op_bench **4.43 ms** (was 199 ms) |
| `cargo test -p core_math` | 768 green | **PASS** (768) |
| HalfEdgeMesh serialized shape | untouched | **PASS** (no field layout change) |
| free_* chokepoints only | no raw free-list pushes | **PASS** |
| validate / is_manifold on in-place | asserted (debug) | **PASS** |

Literal XL/S raw ratio for loop cut on *different* scene rings (n=30…1000) is larger because **edit size grows with n**. Same-ring microbench is the fair locality check.

---

## Measurement C — before / after

### BEFORE (audit fc75e0e / RESULTS_2026-08-01 — old kernel, rebuild + full normals)

| Op | S | M | L | XL |
|---|---:|---:|---:|---:|
| C: loop_cut 1 cut | 0.736 | 21.3 | 251 | **1389** |
| C: extrude_face | 0.157 | 3.56 | 36.1 | **199** |
| C: bevel 1 edge-loop | 0.85 | 23.4 | 281 | 1943 |

### AFTER (this spike, `--c-only` release, 2026-08-02 — raw: `op_bench/RESULTS_LOCALITY_SPIKE.txt`)

| Op | S | M | L | XL |
|---|---:|---:|---:|---:|
| C: loop_cut 1 cut | **0.028** | **0.116** | **0.912** | **3.94** |
| C: extrude_face | **0.028** | **0.112** | **0.912** | **4.00** |
| C: bevel 1 edge-loop | 1.19 | 24.7 | 298 | 1969 |

Speedup (XL): loop cut **~352×**, extrude **~50×**. Bevel still rebuild-tier (out of scope).

Bare-plane microbench (capacity reserved in setup, single cut / single face):

| | S (n=30) | XL (n=1000) |
|---|---:|---:|
| extrude_face | 0.007 ms | **0.047 ms** |
| loop_cut (inserted ≈ n+1) | 0.075 ms | **3.2 ms** |

---

## What landed

### 1. `loop_cut_multi` in-place + ID-stable

- No `from_polygons` rebuild. Rails via `split_edge`, faces via `split_face_between`.
- **Open-strip fix:** resolve rail HEs with face-local `face_rail_he` (both directions).  
  `find_half_edge`/`vertex_fan` stop at the first boundary twin → open planes only saw one rail → loose verts, no face growth (`loop_cut_on_plane_grid`).
- First real **`TopoDelta`**: `vert_parents` (rail blends), `face_parents` (sub-quad → strip parent).
- `LoopCutOp` returns ElementDelta + parents (dense path).
- Region normals after cut; `validate`/`is_manifold` debug-only (no O(mesh) on release hot path).

### 2. Region normals + position-tier GPU path

- `recompute_normals_region(seed_verts)` — faces incident to seeds only.
- Wired: `extrude_face`, loop cut, G/R/S confirm/cancel, extrude-along-normals one-shot.
- **Capacity:** `reserve_elements` + ~6% headroom after `from_polygons`; ops reserve before bulk alloc.  
  Exact-capacity `Vec::clone` + first push was an O(mesh) memcpy (~40 ms XL) masking true local cost.
- **Partial GPU:** topology-stable position sync uses `update_chunk_verts_in_place` + `GpuMesh::update_vertices_only` (VB only when index count unchanged). True byte-range patch API: `patch_vertex_bytes`.

### 3. op_bench

- `--c-only` flag: scenes + measurement C only.
- C setup reserves capacity outside the timed section (matches a mesh that already has growth headroom).

---

## Rules honored

- HalfEdgeMesh **serialized shape** unchanged (no new serde fields on the mesh).
- `free_vert` / `free_edge` / `free_face` only for frees (loop cut is add-only).
- No op-graph construction.

---

## Round 2 (bevel + DirtyMeshRegion)

### Bevel (large-mesh local install)

| Op | S | M | L | XL |
|---|---:|---:|---:|---:|
| C: bevel **before** (R1) | 1.19 | 24.7 | 298 | **1969** |
| C: bevel **after** (R2, mut, local ≥8192 faces) | 1.12 | 1.07 | 10.1 | **50.4** |

**~39× XL** (1969 → 50 ms). Geometry stages untouched (BEVEL.md graveyard). Small meshes keep full `from_polygons` for multi-stage HE-id oracle tests. Residual XL cost includes full `orig_pos` snapshot (next: sparse). See BEVEL.md session log Round 2.

### DirtyMeshRegion GPU

- `GpuMesh::update_vertices_dirty_runs` — patch only changed contiguous vertex runs vs previous persistent raster.
- Wired in `upload_pending` for topology-stable position-tier uploads.

### Extrude XL residual

Kernel bare-plane extrude XL ≈ **0.03 ms** with capacity; op_bench scene XL ≈ **3.3 ms** (cache / large-mesh first-touch). Gate **&lt;0.5 ms flat** not fully met on scene mesh — GPU dirty-run path reduces **upload** bandwidth for position drags; kernel scene residual still tracked.

## Follow-ups

- Bevel: O(ring) unused-vert GC (drop O(faces) free pass) → closer to ring-flat XL.
- Subdivide / knife / poke in-place.
- Fix `vertex_fan` boundary completeness.
- Region-bounded `validate` for release MeshOp path.
- Extrude scene-mesh XL &lt;0.5 ms (cache-aware / tighter region).

---

## Repro

```bat
cd core_engine
cargo test -p core_math --lib
cargo run -p op_bench --release -- --c-only
```
