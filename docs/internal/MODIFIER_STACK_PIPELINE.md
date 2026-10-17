# Modifier-Stack Pipeline — PolyCube → Voxel → Remesh (feasibility + MVP + deferred perf)

Owner's pipeline (2026-06-28). **Base is a POLYCUBE (a polygonal mesh), NOT an SDF** — this is the
**voxel-remesh** paradigm (poly in → voxelize → clean uniform mesh out, à la 3D-Coat), where the voxel cache
is the workhorse and the poly cube is the *input*. Full feasibility analysis lives in the agent report
(task a5f95c14); this doc is the corrected framing + the MVP + the saved perf task.

```
PolyCube (half-edge mesh)
      │
      ▼
Modifier Stack { Bevel, Array, Mirror, Boolean, Bend, Lattice }   (ordered, non-destructive)
      │
      ▼
GPU Voxel Cache        ← VOXELIZE the result (mesh → signed-distance voxel field)
      │
 Dirty Brick Update    ← only re-voxelize/re-mesh the bricks that changed
      │
      ▼
Surface Extraction     ← remesh (Surface Nets / Dual Contouring)
      │
      ▼
Render Mesh            ← resident pool + (multi_)draw_indexed_indirect
```

## Feasibility verdict (corrected for the poly base)
~70% of the BACK HALF already ships. The poly base shifts the front half onto AZRA's poly + voxel-sculpt
systems rather than the SDF analytic path.

| Stage | AZRA today | Status |
|---|---|---|
| PolyCube | `HalfEdgeMesh` box (`creation.rs`); half_edge.rs is **FROZEN** → app-layer use only | HAVE |
| Poly modifier stack | `PolygonalBody.modifiers: Vec<PolyModifier>` — **the ordered non-destructive stack data shape already exists** (Subdivide + Mirror wired; Array/Bevel/Solidify/Smooth/Decimate declared-but-no-op) | HAVE (shape) / STUB (most ops) |
| **VOXELIZE (mesh → voxel field)** | **THE KEY GAP.** AZRA has SDF→voxel (`fill_from_sdf`) but no mesh→voxel (signed distance from a half-edge mesh into the grid). This is the bridge to build. | **NEW (the bridge)** |
| GPU Voxel Cache | voxel sculpt `VoxelGrid` + sparse atlas `VoxelAtlasGpu` (GPU-authority) | HAVE |
| Dirty Brick Update | sculpt sparse-brick dirty + SDF dirty-region partial bake | HAVE |
| Voxel Boolean (subtract) | `fill_from_sdf` additive/subtractive into the density field | HAVE |
| Remesh (Surface Extraction) | GPU Surface Nets (`voxel_mesh.wgsl`) / DC; Ctrl+R remesh | HAVE (LIVE) |
| Smooth geo | Smooth brush / density Laplacian | HAVE |
| Render Mesh | resident pool + `multi_draw_indexed_indirect` | HAVE (LIVE) |

**Build it as a SIBLING of the brush stack** (`BRUSH_SYSTEM_ARCHITECTURE.md`): the same "ordered slots →
CPU fold → flat POD → one bounded GPU loop" discipline, applied to GEOMETRY modifiers. The poly modifier
stack (`Vec<PolyModifier>`) is the data-shape precedent; the voxel sculpt system is the backend.

**Display Invariant: COMPATIBLE** — the output is a triangle mesh (remesh) rastered in Object Mode. The
voxel field is an intermediate, never a second render path. Use the voxel-sculpt backend → it lives under the
sculpt GPU-authority rules (atlas = truth during an edit), NOT the SDF rules.

---

## ★ MVP — prove the pipeline (do this FIRST, not the whole thing)
Owner directive: *"prove the pipeline, not build everything at once."*

**MVP chain: PolyCube → Boolean-Subtract a cube → Remesh → Smooth → render.**
1. Create a poly cube.
2. **Voxelize** it into the voxel cache (the bridge — the one new primitive the MVP needs).
3. **Boolean subtract** a second (voxelized) cube → carve.
4. **Remesh** (Surface Nets/DC) → clean uniform mesh.
5. **Smooth** the geo (density Laplacian) → render.

Most of 3–5 reuse the voxel sculpt system; step 2 (mesh→voxel) is the new bridge. This proves the
poly→voxel→remesh loop end to end with the smallest new code.

**Prerequisite / blocker:** the owner reported **"create cube, duplicate, press X to subtract → both cubes
disappear; changing resolution brings the surface back."** That's a CACHE/RETIGHTEN bug on the SDF boolean
subtract path (log: `[Retighten] settle → already tight, skipped redundant re-bake` ×5 — it SKIPPED a needed
re-bake after an aabb-escape, leaving a stale/empty field; only a full re-tighten/res-change restored it).
The boolean subtract is the MVP's core op, so **fix this first.** Likely root: the retighten "already tight,
skipped redundant" path mis-decides after a subtract that changed the occupied region.

---

## TASK 3 — "AS FAST AS POSSIBLE" performance architecture (SAVED — tackle later, after the MVP)
Deferred per owner. The full design (from the feasibility agent) to implement once the MVP proves the loop:

- **(a) Lazy stack eval** — per-stage hash chain `h_i = hash(h_{i-1}, modifier_i)`; editing modifier k only
  re-evals stages ≥ k. 80/20: cache the ONE field immediately upstream of the most-edited modifier, not all
  N. *(Today `compute_body_source_hash` hashes the whole body → whole-body invalidation; this is the gap.)*
- **(b) Dirty-brick incremental** — local modifiers (bevel/boolean/move) already only re-voxelize the touched
  sub-region (LIVE). **Global deformers (Bend, Lattice, Array count-change) break locality** → keep them LAST
  in the stack; apply **Bend as a warp-at-mesh-sample** (bake the un-bent field once, bend the read coords at
  DC time) so the upstream field stays cached.
- **(c) GPU surface extraction, no CPU readback** — already LIVE (field-direct DC → indirect draw).
- **(d) Modifier eval domains** — Boolean/Bevel/Mirror cheap + local; Array = domain-repeat; Bend/Lattice =
  point-warp, non-Lipschitz (needs the over-relaxation guard). Voxelize-the-mesh is the poly-base entry cost.
- **(e) Memory wall** — dense `res³×4B`: 256³ ≈ 96 MB/body (fine, LIVE); 512³ ≈ 512 MB (borderline); >512³ or
  many bodies **REQUIRES the parked sparse-bake Phase 3A** (`SPARSE_BRICK_CLIPMAP_PLAN.md`, ~94% saving,
  ~5–20 dirty bricks/edit — the perfect substrate). High-res is gated on reviving 3A (parked, 23 bugs).
- **(f) Generation counters + Frame Ordering Contract** — the folded modifier POD rides the bake-params UBO;
  created only in `poll_background_tasks`; generation-counted.

**Top 3 hard problems:** (1) Bend/Lattice global deformers break dirty-brick locality (mitigate: last-in-stack
+ warp-at-mesh); (2) the dense-field memory wall caps high-res until Phase 3A; (3) ordered non-destructive
stack + per-stage cache vs today's flat list + whole-body hash. None are GPU-hard; they're bounded CPU-data
work — but they ARE the new architecture.

---

## Two modifier PHASES — field-level then mesh-level (the Blender poly-stack ask)
Owner: *"add modifiers that affect the rasterized MESH — smooth geometry, smooth normals, decimate, the same
modifiers you'd find in Blender's poly-modeling stack."* This is a clean fit: the pipeline runs **two ordered
modifier stacks** around surface extraction.

```
base → [FIELD modifiers: Bevel, Array, Mirror, Boolean, Bend, Lattice]   (change the SDF/voxel field)
     → voxelize → dirty-brick → Surface Extraction → MESH
     → [MESH modifiers: Smooth, Smooth Normals, Decimate, Subdivide, ...]  (change the extracted triangles)
     → Render
```
This mirrors Blender's own split (generate/deform stack vs finalize). The dirty-brick incremental covers the
FIELD phase; the MESH phase re-runs its passes on the (incrementally) re-extracted mesh.

**The mesh modifiers, by cost:**
- **Smooth geometry** (Laplacian / Taubin) — EASY. A GPU compute pass over the extracted vertex buffer (move
  each vert toward its neighbours' average, N iters, λ weight). No CPU readback — the mesh is already on GPU.
  *(Or smooth the FIELD upstream — often cheaper for the voxel pipeline; offer both.)*
- **Smooth normals** (shading smooth) — EASY. Recompute vertex normals as area/angle-weighted face-normal
  averages; one GPU pass. (The DC mesher emits normals already; this re-derives a smoothed set.)
- **Decimate** — THE HARD ONE. Mesh simplification (quadric-error-metric / edge collapse). The voxel remesh
  emits UNIFORM-density triangles, so decimate = *adaptive reduction* (fewer tris on flat regions). Since
  half_edge.rs is FROZEN, this runs on the EXTRACTED DC/Surface-Nets mesh (app-layer CPU simplification or a
  GPU decimation), never the half-edge kernel. The only genuinely heavy mesh modifier.
- **Blender parity (later):** Subdivide (loop), Triangulate, Weld/merge-by-distance, Solidify (shell),
  Shrinkwrap, Displace (height map), Wireframe — each a mesh-buffer transform.

**Architecture:** the SAME ordered-stack pattern again — a `Vec<MeshModifier>` folded into a sequence of GPU
compute passes over the extracted mesh buffers (vertex/index), generation-counted, no readback. So AZRA now
has **THREE phases of one pattern on a single object: field modifiers + mesh modifiers + material layers.**

**Caveat:** half_edge.rs FROZEN → mesh modifiers operate on the DC/Surface-Nets OUTPUT mesh (GPU buffers /
app-layer), not the poly kernel. **MVP-cheap:** Smooth geo + Smooth normals are ~one compute pass each and can
ride into the MVP; **Decimate is a Phase-2 item** (needs a from-scratch simplifier).
