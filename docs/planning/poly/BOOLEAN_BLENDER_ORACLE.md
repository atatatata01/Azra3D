# Poly Boolean — Blender Exact is Source of Truth

Same discipline as bevel: **Blender Exact solver** is ground truth for poly boolean edge cases.
Years of edge-case fixes live there; we lock start→end topology against it, not against “looks ok”.

## How to regenerate the oracle

```bat
"C:\Program Files\Blender Foundation\Blender 5.0\blender.exe" ^
  --factory-startup --background ^
  --python docs\planning\poly\bool_edge_case_matrix.py ^
  -- docs\planning\poly\bool_edge_case_matrix.json
```

Always use `--factory-startup` (addons break background context).

## Blender 5.0.1 Exact matrix (2026-07-22)

| Case | nv | ne | nf | tris | nonman | euler | face sizes |
|------|----|----|----|------|--------|-------|------------|
| cube_minus_corner | 14 | 21 | 9 | **0** | 0 | 2 | 4×6, 6×3 |
| cube_minus_rotcube | 10 | 15 | 7 | **0** | 0 | 2 | 4×5, 5×2 |
| cube_minus_cyl16 | 40 | 64 | 24 | **0** | 0 | 0 | 4×20, 7, 17, 12×2 |
| coplanar_union | 12 | 20 | 10 | **0** | 0 | 2 | 4×10 |
| seq cut1 | 14 | 21 | 9 | **0** | 0 | 2 | 4×6, 6×3 |
| seq cut2 | 15 | 23 | 10 | **0** | 0 | 2 | 4×6, 5×2, 6×2 |
| intersect_overlap | 8 | 12 | 6 | **0** | 0 | 2 | 4×6 |
| union_overlap | 20 | 30 | 12 | **0** | 0 | 2 | 4×6, 6×6 |
| cube_minus_half | 8 | 12 | 6 | **0** | 0 | 2 | 4×6 |
| torus_minus_cube | 122 | 232 | 112 | 6 | 0 | 2 | (curved residual) |
| cube_minus_ico | 60 | 133 | 75 | 57 | 0 | 2 | (curved residual) |

**Hard rule for flat cases:** `tris == 0`, `non_manifold == 0`, closed, correct Euler.
Residual triangles on flats = the “random edges / need only one edge” class.

## Rust gates (`core_math::mesh_boolean` tests)

- `blender_exact_edge_case_matrix_flat` — locks flat cases to Blender n-gon topology
- `blender_exact_cube_minus_cyl16` — 0 tris, χ=0, nf=24 (Blender); nv budget until full collinear dissolve
- `blender_exact_seq_double_subtract_invariants` — cut1 exact 14/9; cut2 ≤ Blender 15/10
- Existing: `difference_dissolves_to_ngons`, `difference_rotated_cutter_matches_blender`

## Known intentional / open gaps

1. **coplanar_union** — Blender keeps a mid-belt (12v/10f). We fully dissolve to one box (8v/6f). Cleaner “one edge per side”; still solid. Locked to *our* 8/6.
2. **cube_minus_cyl16 nv** — Blender Exact **40v**. After finite plane-support splits (spanning hits only near cutter body) we land **44v** (was **60v**). Hard locks: **0 tris**, nf=24, χ=0, nv∈[40,44]. Remaining ≤4 pins = mid-edge residues on one side wall.
3. **Curved cutters** (ico, dense torus) — Blender itself leaves tris; compare manifold + euler, not exact face layout.

### Fix applied (2026-07-22)
- Finite spanning-hit filter in `split_targeted` / `split_polygon_finite`
- Post-boolean polish: limited dissolve, collinear valence-2, per-ring collinear resimplify
- Blender Exact matrix tests in `mesh_boolean.rs` (`blender_exact_*`)

## Policy (owner)

> Blender is source of truth for polymodeling. They have years of edge cases resolved.
> Bevel was fixed by comparing point start→end across all edge-case scenarios.
> Boolean follows the same matrix discipline.
