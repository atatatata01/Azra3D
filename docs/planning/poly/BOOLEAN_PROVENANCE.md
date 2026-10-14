# Boolean provenance layer — plan (2026-08-04, owner-approved: "rule 2, no downside to build it")

One capability, three payoffs:
1. **Bevel Smart Select rule 2** — exact "this edge came from the cutter cylinder, ignore it".
2. **Knife live cut** (`KNIFE_LIVE_CUT_AND_PIPE.md`) — needs to know which result geometry
   belongs to the live cutter.
3. **Stops the silent data loss**: UVs, seams, creases and face groups are currently
   DISCARDED on every boolean install (`app/src/poly/boolean.rs:549-551,:562` — the
   `BOOL-STALECD` comment; also `poly_drag_tools/extrude_drag.rs:1397-1399`,
   `poly/mesh_sync.rs:656`). `mesh_boolean_manifold.rs:14` already records this as an open gap.

## Why the data does not exist today

The boolean DOES know operand identity while it runs, and throws it away at both ends:

- Thread-local PRESERVE sets — `mesh_boolean/rebuild.rs:145-231`: `PRESERVE_VERTS`,
  `PRESERVE_A` / `PRESERVE_B` (already **per-operand**), `PRESERVE_EDGES`, quantized at
  `PRESERVE_Q = 1e-5`. Registered by `set_preserve_from(a, b)` (`:179`); consumed by
  `is_input_vertex` (`:234`), `same_operand_vertices` (`:221`), `edge_is_preserved` (`:245`).
- They are **cleared on every exit** — `PreserveGuard` / `clear_preserve`
  (`mesh_boolean/mod.rs:298-306`, armed at `:164`) — and are thread-locals with no output
  channel. Nothing escapes the call.
- `HFace` carries only `half_edge, normal, material_id` (`half_edge/types.rs:141-146`).
  **Adding a field there is forbidden** — the serialized shape of `HalfEdgeMesh` is the one
  hard red line in CLAUDE.md (it would corrupt every existing `.fl4`).

## Where the provenance must live

Per `POLY_FOUNDATION_PLAN.md` (HEX-BL): **all new poly state is runtime state on the
non-serialized `PolyMesh` wrapper** (`core_math/src/poly_mesh.rs`), never on `HalfEdgeMesh`.
Authored persistence, if it is ever needed, rides an in-blob `#[serde(default)]` field on
`ProjectFile` — never a second trailing magic after `FL4ORDER`, never `#[serde(skip)]` on
`PolygonalBody.mesh`.

The existing sidecar `CustomData` (`poly_mesh.rs:646-667`: uv/seam/sharp/crease/vcolor/
vcrease/groups/group_meta) is keyed by `FaceId` / `HalfEdgeId` — the right shape, and the
thing currently being wiped. Provenance belongs beside it.

## Design

**`mesh_boolean` returns provenance as an OUTPUT, positionally keyed.**
Result indices are new, so the tag cannot be an index remap. Key by QUANTIZED POSITION, the
same mechanism PRESERVE already uses (`PRESERVE_Q = 1e-5`), which is proven against this
exact geometry.

```
pub struct BoolProvenance {
    /// Per result FACE: which operand it came from.
    pub face_origin: Vec<FaceOrigin>,   // A | B | Mixed(new cut surface)
}
pub enum FaceOrigin { OperandA, OperandB, Cut }
```

A result face is classified by testing its centroid/vertices against the per-operand
PRESERVE sets that already exist (`PRESERVE_A` / `PRESERVE_B`) — no new geometry analysis.
Faces matching neither are `Cut` (created by the intersection).

**Plumbing.** `mesh_boolean` gains a sibling that also returns `BoolProvenance` (keep the
existing signature working — many callers). `PreserveGuard` must hand the sets to the
collector BEFORE clearing.

**Install.** The app maps `BoolProvenance` onto the installed mesh and stores it in the
`PolyMesh` sidecar instead of clearing it. The `BOOL-STALECD` clears exist because stale
`FaceId` groups scrambled group shading — so the fix is to REMAP the sidecar through the
provenance, not to keep clearing it.

## Stages

**P1** — `BoolProvenance` + collector inside `mesh_boolean`, returned from a new entry point.
core_math tests: subtract a cylinder from a cube, assert the barrel faces are `OperandB`,
the flat top/bottom are `OperandA`, and the rim ring is `Cut`.
**P2** — Carry it through the app's boolean install into the `PolyMesh` sidecar; keep the old
clear as the fallback when provenance is unavailable (fail closed, never stale tags).
**P3** — Bevel **Smart Select** consumes it: rule 2 = ignore edges whose both faces are
`OperandB` (the cutter's own tessellation) while KEEPING the rim (`Cut` vs `OperandA` edges).
Rule 3 = exact coplanar, reusing the dihedral predicate in
`mesh_boolean/rebuild.rs:1221-1229` (extract to a shared `HalfEdgeMesh` accessor — do not
write a third copy). One `Smart Select` toggle; excluded edges highlighted; manual
force-include / force-exclude per edge (all owner-ratified).
**P4** — Remap UV/seam/crease/groups through the provenance instead of dropping them; delete
the `BOOL-STALECD` clears once the remap is proven.
**P5** — Live cut (`KNIFE_LIVE_CUT_AND_PIPE.md` B1-B4) uses the same tags.

## Invariants

- Never touch `HalfEdgeMesh`'s serialized shape. Provenance is sidecar/runtime only.
- Provenance is ADVISORY: every consumer must behave correctly when it is absent (older
  saves, BSP fallback path, a boolean that refused). Fail closed — bevel everything rather
  than silently skipping edges on missing data.
- `result_installable` gates are unchanged; provenance never relaxes correctness checks.
- `cargo test -p core_math` + `cargo test -p app` green per stage.
