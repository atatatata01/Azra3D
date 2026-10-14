# PHASE 2 — GRS D6 world-space poly transform + Elongate reject

**Branch:** `feat/poly-phase0`  
**Date:** 2026-07-19  
**Authority:** `GRS_PARITY_DECISIONS.md` D6 / D7 · WORK_ORDER #16 / #17

## Audit

| Item | Live status before this phase |
|------|-------------------------------|
| Symmetrize / cut / mirror pie | Already wired (`symmetry.rs`, Alt+X) |
| Full GRS S0–S4 unify onto gizmo | Large; deferred |
| **D6 world math on body-local verts** | **ACTIVE BUG** — center + deltas in local while camera is world |
| **D7 Elongate on poly** | Silent Scale alias |

`poly_selection_center` already applied `world_transform`; only `poly_start_transform` /
`poly_update_transform` did not.

## Fix

1. **Center** = WORLD median of selected verts (`world_xform.transform_point3`).
2. **Grab / Rotate / Scale** evaluate in WORLD around that center, then write **LOCAL** via inverse xform.
3. Proportional falloff distances measured in WORLD (same radius units as gizmo).
4. **Elongate** refused at `poly_start_transform` with toast (SDF-only).

Identity body transform remains numerically equivalent (world = local).

## Non-goals (later)

- Full S3 migration onto `ActiveTool::Gizmo` (HUD re-key risk)
- Rotate atan2 ring (already partially in `compute_transform` for SDF; poly still uses dx angle)
- Mirror symmetry plane world vs local audit

## Verify

```
cargo build -p app
cargo test -p core_math --lib
```

Manual: place a poly body at non-zero Pos, enter Vertex edit, G-drag along X — verts should follow the mouse the same as at origin.
