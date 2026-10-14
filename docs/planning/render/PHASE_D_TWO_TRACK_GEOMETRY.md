# Phase D — TWO geometry tracks (owner decision 2026-08-05)

**Decision: implement BOTH. Cluster DAG for general meshes, geometry clipmaps for
heightfield terrain.** Not either/or — they solve different problems and the
terrain case is what has generated every D4 artifact so far.

| | Cluster DAG (Nanite-class, D3/D4) | Geometry clipmap (Losasso & Hoppe 2004) |
|---|---|---|
| Input | ANY mesh — sculpts, imports, poly bodies | heightfield / 2.5D terrain only |
| Structure | meshlet DAG, baked, disk-cached | implicit nested regular grids, no bake |
| LOD | per-cluster screen-space error cut | concentric rings, 2× per level, camera-centred |
| Cracks | hand-off invariants (hard — the 2026-08-05 hole saga) | **impossible by construction** (fixed transition strips) |
| Memory | full DAG resident (~100 MB/body until D7 streaming) | constant, inherently streamable |
| Status | D3 done, D4 partial (holes open), OFF by default | not started |

## Why both
- The DAG earns its complexity on arbitrary geometry — that is the modelling
  product (sculpt a 3M-tri body, keep editing it). Terrain is not that.
- Terrain is a grid. A clipmap gets the same concentric-detail result with no bake,
  no DAG, no seam rule, no streaming subsystem — and the crack class that cost a
  full session simply cannot occur.
- Practical consequence: the demo landscape's terrain (the artifact generator)
  moves OFF the cluster path, which also unblocks honest D4 measurement on the
  geometry the DAG is actually for.

## Owner note (2026-08-05) — demo landscape ≠ D4 test after WP7

With Terrain `block_cid` / name-blocked and `CLUSTER_TRI_THRESHOLD = 50_000`:

- Demo landscape may have **tens of thousands** of poly bodies (props, trees, rocks)
  but **none** qualify for bake (typical props are tens–thousands of tris).
- Terrain was the only body over threshold and is **intentionally** off the path.
- Therefore `AZRA_CLUSTERS=1` on the demo landscape is a valid **WP7** check
  (terrain stays MESH, rock-steady) but is **not** a cluster implementation test:
  zero bakes, zero `[ClusterGpu] resident`, zero cut counters.

**To exercise D4:** `AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1` (one unique CC body,
~3.1M tris, ~90k clusters) — same binary, direct before/after vs dense baseline.
`8x9` for multi-body T2. A “looks fine” verdict on the demo landscape says nothing
about cut/holes/LOD.

## Sequencing (proposed)
1. **Clipmap track (new, parallel)** — heightfield source (the demo terrain's
   `terrain_height` is already a pure fn; real terrain later from a heightmap
   texture), N nested rings + transition strips, camera-centred toroidal update,
   vertex-texture displacement. Gate: fly the canyon at 4K with zero cracks and
   zero pop, constant memory.
2. **D4 hole hunt continues on NON-terrain dense meshes** (dense-stress bodies,
   sculpts) where the DAG belongs. Leads already recorded in
   `PHASE_D4_CLUSTER_RASTER_PLAN.md` + memory.
3. Keep D1+D2 as the production path throughout (they are the shipped win).

## Reading
- Losasso & Hoppe, *Geometry Clipmaps: Terrain Rendering Using Nested Regular
  Grids*, SIGGRAPH 2004 — https://hhoppe.com/geomclipmap.pdf
  **READ IN FULL 2026-08-05.** The implementation plan derived from it, with
  per-claim section references, is `PHASE_D_GEOMETRY_CLIPMAP_PLAN.md`. Note it
  opens with a RULE CONFLICT: `AZRA_MASTER_ROADMAP.md` §5 bans a new terrain LOD
  scheme on the premise that cluster LOD is the terrain LOD — owner call needed
  before any clipmap code lands.
- Aaltonen, *GPU-Driven Rendering Pipelines*, SIGGRAPH 2015 — the cluster/indirect
  model D1/D4 already follow: https://advances.realtimerendering.com/s2015/aaltonenhaar_siggraph2015_combined_final_footer_220dpi.pdf
