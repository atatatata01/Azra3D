# Move brush — Level-set / density warp (authoritative)

Owner direction (2026-07): voxel Move is **localized level-set advection** (SDF/density warp),
not mesh-grab. Mesh-grab may return later as optional “Surface / free pull” mode.

## Core operator

```
u(x) = Δ · w(x)
φ₁(x) = φ₀(x − u(x))     // backward / semi-Lagrangian
```

Always from **stroke-start snapshot** + **total drag** (never cumulative frame→frame warp).

## References (study, don’t integrate)

| Source | Use |
|--------|-----|
| OpenVDB LevelSetAdvection / Tracker / FastSweeping | Algorithm: dual buffer, narrow band, redistance |
| Volume Sculpting Using the Level-Set Method | Local velocity window, falloff, evolution |
| Godot Voxel Tools | Dirty regions, brush architecture (MIT, check rev) |
| bevy-sculpter | Rust sandbox for warp + redistance |
| Blender Grab | Behavior only (no GPL code) |

## Quality rules

1. **Snapshot at stroke start** → recompute every frame from original + total Δ.
2. **Dirty region** = capsule/union of sphere(c₀,R) ∪ sphere(c₀+Δ,R), expanded by band + trilinear + DC halo.
3. **Large drag** (`|Δ| > 0.2–0.3 R`): subdivide steps (`max step = 0.2 R`) and/or more inverse-map iters (3–5).
4. **Falloff**: smoothstep, soft rim ≥ 20% of radius.
5. **While dragging**: 0–1 light relax optional. **On release**: full local redistance/relax over dirty AABB.
6. Later: surface-point falloff, connected component, geodesic, mask, symmetry.

## Implementation order (gated)

1. Dense trilinear inverse warp + stroke snapshot — **elastic_grab baseline**
2. Spherical smooth falloff + min 20% border
3. Capsule dirty AABB + multi-step / multi-iter for large Δ
4. Local redistance on release
5. Surface-point falloff
6. Dirty-brick / GPU dual-buffer
7. Connected-component isolation
8. Mask + symmetry
9. Surface mesh-grab as optional Free Move

## AZRA field note

Sculpt stores **density ∈ [0,1], iso 0.5**, not a pure narrow-band SDF. Warp samples density
trilinearly; “redistance” is local Laplacian relax that preserves iso (same as Pinch post-relax).
True Eikonal / OpenVDB sweeping is a later upgrade once density warp feels right.
