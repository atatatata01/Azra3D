# ADR-TERRAIN-001: Mesh Terrain locks (Phase 0)

**Status:** ACCEPTED (2026-08-08) — locked for implementation of
`TERRAIN_SDF_VOXEL_REMESH_PLAN.md` Phase 1+.  
**Date:** 2026-08-08  
**Deciders:** owner (execute-plan directive) + §12 code review recommendations  
**Context docs:** `TERRAIN_SDF_VOXEL_REMESH_PLAN.md` (§§1–12),  
`docs/planning/render/AZRA_MASTER_ROADMAP.md` §5,  
`docs/planning/sdf/TERRAIN_IMAGE_DISPLACEMENT.md`

---

## Context

The Mesh Terrain plan wants full 3D carve (tunnels, overhangs) via tiled SDF/voxel volumes
and remeshed DC meshes. §12 of the plan showed that three assumed pillars (chunked DC,
sparse bricks, Voxel Remesh of DC output) are **mutually exclusive** on a single path, and
that §D4 (“coarser far tiles”) conflicts with the ratified “cluster LOD *is* the terrain
LOD” decision. Phase 0 must lock product/engineering choices before code.

---

## Decisions (L0–L8)

| Lock | Decision | Rationale |
|------|----------|-----------|
| **L0 — Target** | Full 3D carve (Mesh Terrain–class), **not** height-only. | Plan goal; competitive wedge as editor. |
| **L1 — Cluster DAG** | **ON for settled terrain tiles** (policy allowlist only). No second LOD scheme. Far quality = cluster LOD + coarser **bake res**, not a parallel clipmap/ring LOD. | Roadmap-ratified; DC mesh already lands as CPU verts in `mesh_cache`. Amend plan §D4: drop “simplified mesh LOD” as a separate system. |
| **L2 — Tile lattice** | Lattice **origin + cell size from world tile coords** `(tile_i * TILE_M, tile_j * TILE_M)`, never per-body padded AABB. Tiles **overlap ≥1 cell** (apron). Prim membership = **AABB overlap both tiles** (evaluate in both). | **Shared lattice + overlap** makes adjacent *fields* agree in the apron (no air gap / no independent pad AABB). It does **not** make multi-tile display **manifold** or crack-free as one mesh — see **Seams honesty** below. |
| **L3 — Dense vs sparse** | Phase 1: **dense only, res = 128³**. Near (resident) tiles stay dense for remesh/export. Far / Phase 2: sparse or clustered **draw-only**; near↔far transition is a **budgeted Phase 2 work item**, not free. | Remesh needs dense CPU mesh; 128³ = 8.4 MB vs 67 MB at 256³ (§12.9). |
| **L4 — Height seeds** | Phase 1 base was pure SDF (gate). **T1 landed** → single-slot image displace is GPU-ok; height-seeded bases allowed on the fast path when they stay single-mod. | Was: `has_image_displace` → CPU full-stack. T1 dropped that force. |
| **L5 — Voxel scope** | **One live voxel island forever** for Phase 1–2. SDF subtract covers multi-tile carve. “N live VoxelObjects” is a separate project. | Single `VoxelObject` authority (§12.4). |
| **L6 — Persistence** | Phase 1–2: **monolithic `.fl4`**. Memory criteria = VRAM + resident mesh RAM. Per-tile disk store = separate save-format project (append-only / side-channel rules). | No second trailing magic after `FL4ORDER` without a real design. |
| **L7 — Budgets (numbers)** | See constants below (from §12.9). | Measured, not placeholders. |
| **L8 — Export** | Phase 1 = **editor authoring only**. Game export (height pyramid / mesh tiles) = Phase 5 optional. | §10 A/B split. |

### Budget numbers (L7)

| Constant | Value | Source |
|----------|-------|--------|
| `TILE_METERS` | **64 m** (XY) | Plan D1 |
| `DENSE_TILE_RES` | **128** | §12.9 (only sane dense size) |
| Cell size | `TILE_METERS / DENSE_TILE_RES` ≈ **0.5 m** | Derived |
| Max resident tiles (Phase 2 target) | **25** (5×5) | Plan D1 |
| Resident CPU grids at 128³ | ≈ **210 MB** (25 × 8.4 MB) | §12.9 |
| Remesh one 128³ tile | ≈ **80 ms** background only | §12.9 |
| Per-frame caps (existing, hard) | bake 4 · bake RB 1 · DC 2 · mesh land 2 · LOD switch 2 | Code |

### Amended plan §D4

Visual quality stack for terrain:

1. Base mesh: chunked DC / Surface Nets from **dense resident** tiles.  
2. Settled tiles may enter **cluster DAG** (L1).  
3. Material displacement / triplanar for fine skin.  
4. Near: promote draft → full 128.  
5. Far: **cluster LOD + unload dense grid** (cold-tile: mesh retained, atlas released) — **not** a second mesher LOD scheme.
   Phase 3 also: **Chebyshev bake-res rings** (centre 128³ / ring1 64³ / ring2 32³) — coarser far
   **volume** sampling, not a second mesher.

### Seams honesty (carry this; supersedes “L2 prevents cracks”)

What L2 actually delivers:

1. **Same lattice** on both sides of a boundary (world tile origin + fixed cell size).  
2. **≥1 cell apron overlap** + dual prim membership so the SDF *field* is continuous across the
   shared band (no undersized BaseHill air gap; no independent escape-pad lattices).  
3. **Stitch feel** in the viewport = two solid fields that agree in the apron, each extracted to its
   own display mesh.

What L2 does **not** deliver:

- **One manifold mesh** spanning tiles. Each tile is a separate body + separate DC/remesh product.
  Across tile boundaries the surface is **non-manifold by construction** (two meshes meet; verts are
  not welded into one half-edge).  
- **Pixel-perfect crack immunity** when **Phase 3 near/far res rings** give neighbours different
  bake resolutions (e.g. 128³ vs 64³). Overlap still keeps the fields solid, but sample grids differ,
  so silhouette / crease mismatch at the joint is expected and is **not** a regression of L2.  
- **Export as one solid** — Phase 5 glTF exports **one mesh per tile**; consumers that need a single
  manifold must weld/merge offline.

Do not write “L2 prevents seam cracks” without this qualification. Prefer: **“L2 + apron overlap
prevents field gaps; display seams remain non-manifold multi-mesh.”**

---

## Consequences

- Phase 1 ticket **P1.0 Terrain Island MVP** may start: Group + SdfBody hill, dense 128, no height seed, no streaming.  
- Phase 2 must not claim “tile set on disk” until L6 store exists.  
- Height-seeded bases are **allowed** after T1 (single-slot image Displace on GPU); multi-mod stacks still force CPU.  
- Voxel dig on terrain is one active island; multi-tile dig is SDF-only.  
- Do not promise a single manifold terrain mesh until a future weld/merge product exists.

---

## Implementation map

| Phase | Entry | Status |
|-------|--------|--------|
| 1 | Island / tunnel / clamp / bake toast + HUD | **landed** (see plan §9) |
| 2 | Stream = residency only (poll); grow = Patch / Expand; hysteresis; free dense + GPU mesh; placeholder LOD | **scaffold+** |
| 2b | Full perf HUD; async settle remesh; L2 seam bake; placeholder | **landed** (tile disk store = L6 separate) |
| 3 | Near/far + T1 + T3 **landed** | **done** |
| 5 | Terrain glTF multi-mesh + height PNG export | **landed** (L6 disk store still open) |
| 5 | Export (L8) | open |

### Phase 1 gaps (do not mark landed until closed or explicitly deferred)

| Item | Plan asked | Today |
|------|------------|--------|
| Perf HUD | bake ms, DC ms, tri count, dirty chunk count | bake ms + optional tris as **toast only** → **defer to 2b** |
| Voxel Remesh on stroke end | optional remesh polish | **not implemented** → **defer to 2b** |
| Tunnel / carve | full carve interactive | demo tunnel + BodyField gate **done** |
| AABB clamp | island-constrained carve | create + drag clamp **done** |

### Memory note (dense, not atlas)

Terrain Phase 1–2 tiles are **dense 128³ (L3)**. They do **not** use the sparse brick atlas
(≥512). Cold-tile memory is `BodyVolumeCache.body_grids` (~8.4 MB/tile). Release path:
`release_cold_tile_dense_grid` — **drop** the grid (do not park in `ram_cache`).

Constants live in `app/src/terrain.rs` (`TILE_METERS`, `DENSE_TILE_RES`, …) so Phase 2 streaming shares one source of truth.
