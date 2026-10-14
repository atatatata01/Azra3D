# AZRA_NURBSDIFF reference shots

**Gate rule (owner 2026-08-01):** no NURBS tool milestone is **done** without a harness shot
here. `cargo test -p core_math` alone never counts.

## Run

```powershell
cd core_engine
$env:AZRA_NURBSDIFF = "$PWD\..\docs\planning\nurbs\nurbsdiff_refs"
cargo run -p app
```

## P0 baseline (committed)

| Shot | Script step | What it proves |
|---|---|---|
| `01_vertical_r02.png` | cube → fillet FreeCAD edge 6 (vertical) r=0.2 | create solid + single-edge fillet app path renders |
| `02_vertical_then_top_r02.png` | then fillet FreeCAD edge 5 (top adjacent) r=0.2 | sequential junction path (owner repro) |

**P3:** sequential top re-batches via `remove_all_planar_edge_fillets` + multi-edge batch.
`02` is distinct from `01` (two fillets, faces=8).

## P1 solid primitives (committed)

| Shot | Script step |
|---|---|
| `03_cylinder_solid.png` | `NurbsSolid` cylinder (wall + 2 caps) |
| `04_cylinder_rim_r015.png` | top rim toroid r=0.15 (kernel reachable from solid) |
| `05_sphere_solid.png` | closed sphere as `NurbsSolid` |
| `06_torus_solid.png` | closed torus as `NurbsSolid` |

## Plasticity-class (committed)

| Shot | Script step |
|---|---|
| `07_top_loop_r02.png` | 4 top edges batch r=0.2 (FreeCAD top_loop; continuous rounded top) |

## How to get Plasticity-like result in UI

1. Select **≥2 edges of the same face** (e.g. two top edges) → Ctrl+B expands to that face's full boundary loop.
2. Or multi-select the full top loop, then Ctrl+B.
3. Two edges that meet at a corner now free-end-notch the third face (no gap/gouge).
