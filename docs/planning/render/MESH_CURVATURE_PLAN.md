# Mesh-Based Curvature — Plan

## 2026-10-12 — Polygon shader-node GPU cache integration

- Owner approved implementation in the existing `feat/ui-polish` version, selective curvature dependency ports from the main checkout, and parallel helper work. No branch merge, save-format change or replacement of existing shader fixes.
- Feature preflight: no new scene entity; authored controls remain in the material node graph and existing undo transactions; new cache work must be bounded and scheduled; raster/triangle display remains the single display technique.
- Selectively imported `core_math::mesh_curvature::RingMesh` and its geometry tests. Ported the allocation-reusing `HalfEdgeMesh::face_vertices_into` accessor; the allocating accessor uses the same traversal. Removed the unrelated AO-baker test from the dependency port rather than importing unrelated BVH changes.
- Added deterministic edge topology and a signed-dihedral CPU oracle. GPU production measurements must match positive right-angle cube edges, zero flat triangle seams, negative inward cavity edges, and reject non-manifold topology with a diagnostic.
- Acceptance: flat cube face centers must remain flat; all twelve real edges must respond. Interpolating equal corner values across a coarse cube is not acceptable. Ordinary repeating material coordinates also cannot encode object curvature: on a two-unit cube, edge coordinates and face-center coordinates alias with tiling 1.
- Caches must be object/geometry-specific even when objects share a material. Geometry generations invalidate measurements; graph contrast/strength/mask edits reuse geometry resources. UV seams, post-modifier display geometry, camera independence, undo and save/load require regression coverage.
- Implemented: topology from the post-modifier polygon display mesh feeds a GPU edge BVH. The object-specific surface atlas stores nearest real edge distance and signed dihedral angle. Connected-surface neighborhoods cross coplanar subdivision boundaries; internal triangulation diagonals do not become edges. Runtime atlas UVs are carried by both raster and triangle path tracing; authored geometry and UVs are untouched.
- Live edits use shared background-compiled parameterized pipelines. Poll work is scheduled and bounded: 64 discovery contexts, two graph preparations, 32 attempts, eight graph dispatches and one geometry build per frame. Surface residency is capped at 128 MiB and actual device buffer limits. Material values, array generations and source geometry stamps participate in freshness/recovery; publication/removal invalidates cached draws and converged PT.
- Supported scope: single-material polygon objects, procedural color/scalar graph outputs. Multiple face materials, retained authored-UV maps, warped Curvature Position, scene-dependent unsupported nodes and normal/height outputs are rejected rather than replaced with fake curvature. Atlas geometry is limited to 4096 triangles with at least eight texels per triangle tile, subject to array resolution and device limits. Existing SDF curvature bake handling remains separate.
- Validation: nine geometry tests; 20 material-GPU tests including cache reuse, shared-material object isolation, subdivided-edge distance and actual raster pixels; two height/normal regressions; 76 graphics tests; 96 render tests; and a source-mesh persistence roundtrip pass. The real raster test verifies light cube boundaries, dark centers/diagonals, intermediate gray decoding and visible default radius 0.02 at 256 resolution. A 32-update warm headless run measured 0.051 ms median CPU submission, 0.157 ms p95 and 0.088 ms completed-batch average per update; this is not a native viewport FPS claim.
- Full core-math run reported 950 passing, 11 ignored, and three failures outside curvature (`edge_select::directed_loop_crosses_pole_straight`, two slit-refusal tests in `mesh_ops`). Those unrelated failures were left untouched. Native viewport testing remains owner-operated.

**Owner 2026-07-28:** *"do you use sdf data or dc mesh curvature — we should use dc mesh
curvature"*, after repeated reports that edges read wrong and the map never matches the
reference (smooth midtone gradients across whole surfaces).

**The owner is right.** This documents what was verified, why the current approach cannot produce
that look, and what a correct implementation requires.

---

## What is wrong today (verified in code, not inferred)

The vertex rung takes the DC mesh's **vertex positions** and then evaluates **`eval_scene_at`** —
the analytical SDF — at those points (`gpu_curvature.rs` `SEED_POSITIONS`, kernel in
`volume_bake.wgsl`). It computes a **Laplacian of the field**, not curvature of the surface.

Near a boolean seam or a blended edge, the field's Laplacian is dominated by *how the SDF blends
there*, not by the actual surface shape. That is precisely the reported symptom: "shapes are bad
at edges in the middle".

### The trap: the mesh normal is ALSO field-derived

`sdf_mesh.wgsl:399-401` writes each vertex as `[pos.xyz, nrm.xyz, 0, 0]`, and `nrm` comes from
`gradient_normal(p, e)` — *"Outward unit normal from the signed-distance gradient"* (`:182-184`).

So the obvious shortcut — "use the mesh's own normals" — **reproduces the same field-derived
answer** and would change nothing. Any real fix must derive curvature from **neighbouring vertex
POSITIONS via the index buffer**, which is the only genuinely geometric data present.

---

## What is available (all verified)

| Piece | State |
|---|---|
| Mesh is **indexed** | yes — `draw_indexed_indirect`, so vertices are shared and adjacency is derivable |
| `slot.verts` | GPU-resident, 8 f32/vertex (`pos`, `nrm`, `uv`) |
| `slot.idx` | GPU-resident **alongside** verts (`resident.rs:251`) |
| index count | GPU-written into the `DrawIndexedIndirect` args — **no readback needed** |
| dispatch path | `encode_curvature_vertex_dispatch` already binds `verts`; adding `idx` is one more binding |
| output carry | already shipped — storage buffer read by `@builtin(vertex_index)` |

**This is why the work is tractable:** the data and the delivery path both exist. What is missing
is the algorithm.

---

## Algorithm

Standard discrete mean curvature, the same family Substance/ZBrush use:

1. **Per-triangle:** read the 3 indices, compute the geometric normal from the *positions*
   (`normalize(cross(b-a, c-a))`) — NOT the stored field normal.
2. **Accumulate per-vertex:** for each vertex, sum the signed angle between its adjacent triangle
   normals, area-weighted. Convex regions accumulate positive, concave negative.
3. **Normalise** by accumulated area so the result is scale-invariant.

Two GPU passes over `idx`, both indexed-atomic accumulations into a per-vertex buffer — the same
shape as the existing dispatch, so it inherits the `sparse_quiet` gate and the 2M cap already in
place (`8a27c1f`).

**Radius** becomes a genuine smoothing width: after accumulation, N smoothing iterations over the
adjacency spread curvature across neighbours. That is what makes a gently-domed shell read as a
midtone instead of "not an edge" — the property the reference image has and the current
point-sampled field lacks.

---

## Why this produces the reference look and the current path cannot

- **Field Laplacian** answers *"how fast is the distance field bending here?"* — dominated by
  blends and boolean seams, effectively binary on hard geometry.
- **Discrete mesh curvature** answers *"how much do adjacent faces disagree?"* — exactly the
  quantity the reference visualises, and naturally continuous across smooth surfaces.

---

## Phases

- **M1 — per-triangle geometric normals** into a per-vertex accumulation buffer; verify on a
  sphere (uniform positive curvature) and a box (zero on faces, high on the 12 edges only).
- **M2 — angle + area weighting**, signed convex/concave. Gate: a box's flat faces read ~0 and its
  edges read high, with a **real spread** (the non-vacuity check that caught the earlier gift-wrap
  bug).
- **M3 — Radius as smoothing iterations** over adjacency. Gate: increasing Radius visibly widens
  the falloff instead of only changing where it saturates.
- **M4 — replace the field path for the Vertex rung.** Keep the field path for the TEXTURE rung
  until it too has a mesh source; they must not silently disagree.

---

## Risks

- **f32 atomics do NOT exist in WGSL — RESOLVED, decides the design.** Verified: every atomic in
  this codebase is `atomic<u32>` (`sdf_mesh.wgsl:58`, `voxel_mesh.wgsl:34`,
  `pathtrace_tri_entry.wgsl:145`), and WGSL provides no float atomic. So per-triangle results
  CANNOT be atomically summed into a per-vertex f32 buffer.
  **Chosen approach: gather, not scatter.** One thread per VERTEX walks its own incident triangles
  and sums locally — no atomics at all, and deterministic (float addition is order-dependent, so a
  scatter would also make the result vary run to run, which the existing determinism test would
  catch). This requires a vertex→triangle adjacency table, built once per mesh generation:
  a counting pass then a prefix-sum + fill, both plain `u32` atomics.
- **Vertex sharing — RESOLVED, and it holds globally.** `cell_vert[li]` stores ONE vertex per DC
  cell and quads reference it by index, so adjacent faces genuinely share vertices. Critically,
  the sparse path keys storage on the BRICK that owns the cell (`cell_vert[H*512 + local]`), not
  on the meshing tile — so a vertex has a single slot no matter which tile meshes it. **No seam
  duplication**, therefore no grid artifact at tile boundaries.
- **The CPU oracle** (`sdf_curvature_masks`) stays field-based. Once the vertex rung is mesh-based
  the two legitimately differ, so `gpu_curvature_matches_cpu_sdf_curvature_masks` must be
  re-scoped to the texture rung rather than "fixed" — a mesh-vs-field mismatch there is correct,
  not a regression.
