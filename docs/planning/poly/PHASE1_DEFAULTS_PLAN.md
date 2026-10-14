# PHASE 1 — Poly Blender-default completeness

**Branch:** `feat/poly-phase0`  
**Date:** 2026-07-19

## Audit result (live master code)

Most of the original Phase 1 list was **already shipped** with Blender oracles:

| Item | Status | Evidence |
|------|--------|----------|
| Extrude region multi-face | ✅ Done | `extrude_region`, app drag + one-shot; tests green |
| Inset region + Individual | ✅ Done | `inset_region`, G/I modal; tests green |
| Merge at center / last / by distance | ✅ Done | `merge_vertices_to`, self-loop collapse |
| DissolveVertex | ✅ Done | oracle tests green |
| **Inset depth on interactive drag** | ❌ Was missing | confirm hardcoded `depth: 0.0` |
| **Bridge edge loops** | ❌ Was 1/2-vert only | real N-edge loop bridge missing |

## Implemented this phase

### P1.2 — Inset depth on drag
- `PolyInsetDrag`: `depth_normal`, `accumulated_depth`
- Bare drag = thickness; **Alt+drag** = depth (Ctrl stays snap)
- Confirm seeds Adjust panel with real depth
- Mode switch G/I re-applies thickness+depth
- Hints + tool strip updated

### P1.4 — Bridge Edge Loops
- Kernel: `HalfEdgeMesh::bridge_loops` + `bridge_edge_selection`
- Aligns second loop (rotate + reverse) by min endpoint distance
- Winding aware of existing boundary HEs (twin correctly when present)
- App: Edge mode with two closed equal-length loops → real bridge; legacy 1/2-vert/2-edge kept
- Oracle: two 8-vert open circles → nv=16/nf=8 (Blender 4.5)

## Gates
- `cargo test -p core_math --lib bridge_` green
- `cargo test -p core_math --lib` full suite
- `cargo build -p app` green
