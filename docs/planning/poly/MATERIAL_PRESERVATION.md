# Per-face material preservation across mesh-rebuilding ops

**Status:** class fixed at the source 2026-08-07. This doc is the rule + the caller audit.
**Owner reports that produced it:** "Dissolving edge (Ctrl+X) removes material" and
"Symmetrize (Alt+X): I lose my material that is assigned; it should symmetrize material by default."

---

## 1. The defect class

`HFace.material_id` is the ONLY place a poly face's material assignment lives. Two kernel
constructors allocate faces, and both used to hardcode `material_id: 0`:

| Allocator | File | Used by |
|---|---|---|
| `from_polygons` / `from_quads` | `core_math/src/half_edge/builders.rs` | whole-mesh polygon-soup rebuilds (bevel, poke, subdivide, boolean, knife) |
| `create_face` / `create_face_no_normals` | `core_math/src/half_edge/euler.rs` | incremental face installs (dissolve region merge, mirror rebuild, bridge, grid fill) |

So **any op that deletes faces and re-creates replacement geometry silently repaints the whole
affected region with the default material.** Every instance looks like a different bug to the
user; they are one bug. Before this fix it had been patched THREE separate times at three call
sites (knife, boolean, bevel) without the shared cause being closed.

`create_face` was the half that had not been found — and it is the one the owner's Ctrl+X hits.

## 2. The rule (follow this, do not re-derive)

> **Never allocate a replacement face without deciding its material.**
>
> * Rebuilding from a polygon soup → `from_polygons_with_materials(positions, faces, materials)`
>   / `from_quads_with_materials(...)`, with `materials` **index-parallel to the INPUT face
>   list**.
> * Installing one replacement face → `create_face_with_material(ring, mat)` /
>   `create_face_no_normals_with_material(ring, mat)`.
> * A face DIVIDED from a parent inherits the parent's id. A face MERGED from several takes an
>   example face's id (we use the lowest live `FaceId` — deterministic; a `HashSet` region is
>   not). A face MIRRORED from a source carries the source's id.

**The index-drift trap.** `from_polygons` SKIPS rings shorter than 3, so output `FaceId`s drift
from input indices as soon as one degenerate ring is present. A post-pass that walks output faces
and stamps `materials[i]` therefore paints faces with a NEIGHBOUR's material — worse than the
default it replaces. The `*_with_materials` constructors stamp at ALLOCATION time, so they cannot
drift. Any skip in a caller's own loop (e.g. `mirrored_across` dropping on-plane faces) must drop
the material entry with the face. Guarded by
`half_edge::tests_materials::short_rings_do_not_drift_materials`.

**Additive only.** The plain `from_polygons` / `from_quads` / `create_face` entry points still
exist and delegate with an empty material slice, so they are bit-identical to before. No
signature changed, and the `HalfEdgeMesh` serialized shape is untouched (`.fl4` red line).

## 3. `from_polygons` / `from_quads` caller audit

Production call sites only (test fixtures build brand-new meshes and have nothing to preserve).

| Call site | Loses materials? | Status |
|---|---|---|
| `half_edge/builders.rs` `new_box` / `new_plane` / `new_uv_sphere` / `new_cylinder` / knife pipe | No — brand-new primitive | SAFE by construction |
| `half_edge/builders.rs` `faces_as_new_mesh` (Separate by Selection) | **Yes** | **FIXED** — `from_polygons_with_materials` |
| `half_edge/ops_misc.rs` `poke_faces` | **Yes** | **FIXED** — fan tris inherit the poked face |
| `half_edge/ops_misc.rs` `subdivide_faces` | **Yes** | **FIXED** — child quads + copied neighbours |
| `half_edge/ops_misc.rs` `subdivide_catmull_clark` (`from_quads`) | **Yes** | **FIXED** — `from_quads_with_materials` |
| `half_edge/ops_misc.rs` `bevel_edges_ex` | **Yes** | **NOT FIXED, deliberate** — DEAD: superseded by `core_math/src/bevel_op/`, referenced only from `half_edge/tests.rs`. Fix only if revived. |
| `core_math/src/bevel_op/mod.rs` (×2) | No | already fixed — `apply_face_materials` post-pass |
| `core_math/src/knife.rs` | No | already fixed — `poly_mat` threaded through the split helper (the reference pattern) |
| `app/src/poly/boolean.rs` `merge_meshes` / `merge_meshes_n` | No | already fixed — ring materials snapshotted and restored |
| `app/src/poly/boolean.rs` (kernel result) | No | already fixed — `app/src/poly/bool_material.rs` nearest-surface plan + redo panel |
| `app/src/poly_knife.rs` cutter, `app/src/poly_drag_tools/extrude_drag.rs` cutter | No — throwaway boolean cutter, never displayed | SAFE |
| `app/src/dc_baseline.rs` | No — DC-baked SDF mesh, no authored materials | SAFE |
| `app/src/stress_test/demo_scene.rs`, `smart_snap.rs`, `poly_selection/mod.rs` fixtures | No — synthetic | SAFE |

## 4. `create_face` audit (the half that was missing)

| Call site | Loses materials? | Status |
|---|---|---|
| `ops_dissolve.rs` `dissolve_one_region` | **Yes — the Ctrl+X bug** | **FIXED** — merged n-gon takes the lowest live `FaceId`'s material |
| `ops_misc.rs` `mirrored_single_axis` | **Yes — the Alt+X bug** | **FIXED** — mirrored face carries its source's material |
| `ops_misc.rs` `mirrored_across` (used by `symmetrize`) | **Yes — the Alt+X bug** | **FIXED** — same, with the on-plane skip dropping the material entry too |
| `ops_dissolve.rs` `dissolve_edge` | No — keeps `face_a`, material rides along | SAFE (regression-tested anyway) |
| `euler.rs` `split_face_between` (used by `bisect_plane`) | No — already copies `mat` to the child | SAFE |
| `ops_misc.rs` `bridge_loops` / `bridge_edge_selection` / `bridge_faces` / `bridge_unequal_loops` | n/a — BRAND-NEW geometry, no parent | **OPEN parity item, not this class**: Blender gives new faces the active/adjacent face's material; we give 0. |
| `ops_misc.rs` `grid_fill*`, `rip_fill_edge*`, `solidify` inner shell | n/a — brand-new geometry | same OPEN parity item |

## 5. Coverage

* `core_math/src/half_edge/tests_materials.rs` — 17 kernel tests: both new constructors, the
  index-drift guard, `dissolve_edge` / `dissolve_edges_region` / `dissolve_faces_region`,
  `mirrored` / `mirrored_across` / `symmetrize`, `poke_faces`, `subdivide_faces`,
  `subdivide_catmull_clark`, `faces_as_new_mesh`.
* `app/src/poly/diag_material_class.rs` — 4 APP-PATH tests: headless `AzraApp`, a body with
  DISTINCT per-face materials, driven through the real `poly_dissolve()` (Ctrl+X) and
  `poly_symmetrize_op()` (Alt+X) entries, materials read back off the INSTALLED scene-graph mesh.
  A green kernel test does not prove the user's tool works (`feedback_verify_app_path_not_kernel`).

Every assertion is on EXACT ids. "Not zero" would pass for a fix that floods one surviving
material over the whole body — just as destructive, and invisible to a weaker check.

---

## 6. Boolean (X pie) — materials + the cutter-material policy (2026-08-07)

Owner: *"Doing a boolean, it should pick up the material. Boolean needs their own tool
properties, so after a boolean, if the user wants to toggle 'no material' I could — even
though this operation is destructive."*

### What was actually broken (measured, not assumed)

| Path | Before | After |
|---|---|---|
| Default **Manifold** kernel | Materials already restored by `mesh_boolean_manifold::transfer_materials_from_operands` (nearest input face). No target/cutter distinction, no way to revisit. | App-layer plan supersedes it, with the split + the policy. |
| Legacy **BSP** kernel (`AZRA_BSP_BOOLEAN=1`) | **24/24 result faces at material 0 — total loss** (`from_polygons` stamps 0). | Same plan fixes it — the app layer no longer depends on which kernel ran. |
| `poly::boolean::mesh_from_faces` (edit-mode island cutter extraction) | **Rebuilt through `from_polygons` → the cutter island lost every material BEFORE the boolean ran.** | Fixed in place with the `merge_mesh_list` snapshot/restore carry. |

### The rule
`app/src/poly/bool_material.rs::plan_result_materials` gives each result face the material of
the input face whose SURFACE it lies on. Operand membership comes from the existing
`BoolProvenance` (`FaceOrigin::OperandA/OperandB/Cut`); `Cut` and missing provenance fall back
to "whichever operand's surface is nearer". Which face inside that operand is a nearest
point-to-triangle lookup (`point_triangle_dist_sq`), done in the app layer —
`core_math/src/mesh_boolean/` was not touched.

### Post-hoc adjustability, honestly
The boolean is destructive; the kernel is **never re-run**. Each result face stores BOTH
candidates (`FaceMatPlan { own, inherit, from_cutter }`), so changing the policy is a pure
re-stamp of `face.material_id` on the installed mesh — reversible, geometry untouched.
Policy (`BoolCutterMaterial`, sticky in `ui.bool_cutter_material`):

* **Cutter material** (default) — the carved surface keeps the cutter's id (Blender parity).
* **Target material** — the carved surface reads as the target; the result is one material.
* **No material** — the carved surface falls back to the default; the target's own faces are
  untouched.

UI: `Last · Boolean <op>` in the Tool Parameters dock (`tool_params.rs`, after the To Sphere
arm) plus the legacy floating window, exactly the `PolyToSphereAdjust` lifecycle. The panel
**fails closed** — if any target body's face count no longer covers its plan (a later boolean,
bevel or cleanup rebuilt the mesh), `poly_boolean_adjust_active()` returns false and the panel
drops rather than stamping materials onto strangers.

### Coverage
* `app/src/poly/bool_material.rs` — kernel-level: cube(3) − cylinder(7), all three policies.
* `app/src/poly/diag_bool_material.rs` — **4 APP-PATH tests**: headless `AzraApp`, two poly
  bodies with distinct materials, driven through the real `poly_boolean_execute(Difference)`
  X-pie entry, materials read off the INSTALLED scene-graph mesh; plus a test that flips the
  policy after the op and flips it back, asserting exact reversibility and an unchanged face
  count (`feedback_verify_app_path_not_kernel`).
