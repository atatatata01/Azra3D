# AZRA Mesh-Terrain Class Plan  
## SDF + Voxel + Remesh + Chunked DC (performant, visual, full 3D carve)

**Status:** Phase 0–2b **landed**. Phase 3: near/far + T1 + T3 **landed**. Phase 5 export **started** (terrain tile glTF multi-mesh + height PNG). Open: L6 tile store (separate save-format project).  
**Date:** 2026-08-08  
**ADR:** `docs/planning/TERRAIN_MESH_TERRAIN_ADR.md`  
**Code:** `core_engine/app/src/terrain.rs`, Shift+A → Terrain Island / Patch  
**Product target:** Unreal **Mesh Terrain**–class capability: full 3D carve (caves, overhangs, tunnels), not height-only paint.  
**References:**  
- Losasso & Hoppe geometry clipmaps (mesh LOD *ideas* only)  
- Mike Turitzin dynamic SDF engine (SDF under the hood → mesh extract; terrain special-case)  
- UE Mesh Terrain (3D mesh ops + stream/LOD) — the *capability bar*, not a specific version claim (the "5.8" in an earlier draft is unverified; what matters is the feature list in §12.7)  
- Existing AZRA: `BodyVolumeCache`, sparse bricks, chunked DC, `VoxelBody`, poly **Voxel Remesh** (`region_mesh_to_sdf_band`), draft bake, displace height maps  
- `docs/planning/sdf/TERRAIN_IMAGE_DISPLACEMENT.md` (T1–T3 GPU displace parity), `docs/planning/render/AZRA_MASTER_ROADMAP.md` §5 (terrain assessed)  

> **Read §12 before implementing anything here.** §12 is a code-grounded review (2026-08-08) that
> found three of this plan's pillars mutually exclusive in the current code, plus one conflict with
> a ratified roadmap decision. §§0–11 are the intent; §12 is what the engine actually does today.

---

## 1. Goal

Ship **one terrain track** that is:

| Requirement | Meaning |
|-------------|---------|
| **Full carve** | Dig tunnels, booleans, overhangs — like voxel/SDF, not vertical-only height |
| **Performant** | Interactive edit locally; stable FPS at open-world scale via streaming + budgets |
| **Visually nice** | Sharp features (DC), smooth shading, material detail near camera |
| **AZRA-native** | Reuse SDF, voxel sculpt, remesh, chunked DC — **not** a greenfield engine |

**Non-goals (v1):**

- Literal Hoppe nested-ring clipmap implementation  
- Continent as one dense 512³/2048³ SDF body  
- New modeling pillar that bypasses scene graph / transaction rules  
- Replacing poly/NURBS/SDF tools — terrain **composes** them  

---

## 2. Architecture (layers)

```
┌──────────────────────────────────────────────────────────────┐
│ L4  PRESENTATION                                             │
│     Raster mesh (chunked DC / remesh output)                 │
│     Optional material displacement (fine skin)               │
│     Stream: only resident chunks drawn                       │
└──────────────────────────────────────────────────────────────┘
                              ▲ extract / remesh
┌──────────────────────────────────────────────────────────────┐
│ L3  MESH SERVICES (existing)                                 │
│     Chunked Dual Contouring  (SDF volume → tris)             │
│     Poly Voxel Remesh        (mesh → band SDF → Surface Nets)│
│     Surface Nets (voxel body path)                           │
└──────────────────────────────────────────────────────────────┘
                              ▲ bake / edit
┌──────────────────────────────────────────────────────────────┐
│ L2  VOLUME TILES (stream unit)                               │
│     Sparse SDF bricks OR VoxelBody chunks in world tiles     │
│     Dirty-region rebake only                                 │
│     Draft res → promote (already in body_vol_cache)          │
└──────────────────────────────────────────────────────────────┘
                              ▲ authored by
┌──────────────────────────────────────────────────────────────┐
│ L1  AUTHORING (full carve)                                   │
│     SDF prims + booleans + image displace (local rocks)      │
│     Voxel sculpt brushes (dig / clay / paint)                │
│     Optional height seed → baked into tiles (2.5D start)     │
└──────────────────────────────────────────────────────────────┘
```

**Key rule (Mike-style):**  
**Source of truth = volume (SDF and/or voxel density).**  
**What the player sees = remeshed / DC triangle mesh.**  
Clipmap paper applies only as *ideas* for L4 (viewer-centered detail budget), not as the data model.

---

## 3. What we reuse (do not rebuild)

| System | Role in terrain |
|--------|-----------------|
| **SdfBody + prims + body_op** | Macro landforms, CSG carve, displace rocks |
| **BodyVolumeCache + dirty regions + draft_resolution** | Interactive bake, progressive quality |
| **Sparse bricks / BrickGrid + chunked DC** | Surface-only storage + incremental mesh |
| **VoxelBody + sculpt brushes + Surface Nets** | Clay dig, high-frequency carve, paint — **ONE live island at a time** (§12.4) |
| **Poly Voxel Remesh** (`region_mesh_to_sdf_band` + Surface Nets) | Clean uniform mesh after heavy edit; “Mesh Terrain remesh” feel — **dense-tile path only** (§12.2) |
| **DisplaceHeightMap** | Local / tiled height detail on SDF prims — **GPU parity missing today, and its absence disables the GPU bake, sparse bricks and per-body LOD for that tile** (§12.5) |
| **Material displacement (gbuffer)** | Visual fine detail without rebaking volume |
| **block_cid / two-track** | Keep pure height demos off cluster DAG until terrain path is ready |

---

## 4. Core design decisions

### D1 — Tile the world, never one volume
- World split into **terrain tiles** (e.g. 64–128 m XY, height band Z).  
- Each tile owns sparse volume + chunked mesh.  
- Camera **resident window** (e.g. 3×3 or 5×5 tiles); unload mesh/volume outside.  
- Edit dirties **one tile** (or few) → partial bake + partial DC only.

### D2 — Full carve via two complementary volume modes
| Mode | When | Tool |
|------|------|------|
| **SDF tile** | Large forms, CSG, smooth boolean landscape | SdfBody scoped to tile / multi-tile island |
| **Voxel tile** | Free dig like clay/Minecraft carve, high detail holes | VoxelBody / sculpt in tile bounds |
| **Bridge** | After voxel dig, optional **Remesh** for clean manifold mesh | Poly Voxel Remesh on extracted mesh **or** voxel → SDF band → Surface Nets |

User-facing: one “Terrain sculpt” tool mode that routes dig to voxel or SDF subtract by brush setting — implementation stays two backends.

### D3 — Remesh is the “Mesh Terrain remesh” button
Unreal Mesh Terrain uses remesh modifiers for quality. AZRA already has:

- **Voxel Remesh** poly modifier (mesh → narrow-band SDF → Surface Nets) — live path recently ~135× faster.  
- Use it for:  
  - Clean export mesh  
  - Uniform triangle density for materials/LOD  
  - After big boolean scars  

**Not** every frame — on settle / user “Rebuild terrain mesh” / async after stroke end.

### D4 — Visual quality stack (cheap → expensive)
1. **Base mesh:** chunked DC or Surface Nets from resident tiles.  
2. **Shading:** smooth/weighted normals (existing poly/SDF normal path).  
3. **Skin:** material displacement / triplanar (existing gbuffer path) for rock grain.  
4. **Near camera only:** higher bake res / denser DC band (draft → full already exists).  
5. **Far:** coarser tile res or simplified mesh LOD (drop fine DC chunks).

### D5 — No new NodePayload unless forced
Prefer:

- **TerrainWorld** as app/runtime manager (not necessarily graph payload).  
- Tiles as existing **SdfBody** and/or **VoxelBody** nodes under a Group.  
- Metadata: tile coord, stream state, quality level on side-channel / node flags.

Only if streaming identity cannot be expressed: append **one** payload variant **after** `SdfObject` (bincode append-only rule). Defer until Phase 2 proves need.

---

## 5. Phased delivery

### Phase 0 — Product lock (½–1 day)
**Deliverable:** ADR in this folder (short). **Rewritten 2026-08-08 from the §12 review** — the six
locks below are all decidable on paper, and every one of them is currently unspecified in a way that
would be discovered mid-Phase-2 at the worst possible cost.

- [x] **L0 — Target.** Mesh Terrain–class **full 3D carve**, not height-only. *(unchanged)*
- [x] **L1 — Cluster DAG: ON for settled terrain tiles** (policy allowlist). No second LOD scheme.
      §D4 amended in ADR — far quality = cluster LOD + coarser bake res / cold-tile mesh, not a
      parallel clipmap. See `TERRAIN_MESH_TERRAIN_ADR.md`.
- [x] **L2 — Tile lattice invariant.** World tile coords drive origin + cell size; ≥1 cell overlap;
      straddling prims evaluated by **both** tiles (AABB overlap). Helpers: `tile_origin_world` /
      `world_to_tile` in `app/src/terrain.rs`. **Does not** yield a manifold multi-tile mesh —
      see ADR **Seams honesty** (overlap fixes field gaps; Phase 3 res rings can still mismatch
      silhouette at joints).
- [x] **L3 — Dense vs sparse.** Phase 1 = **dense 128³ only**. Near stays dense for remesh; far /
      sparse = Phase 2 budgeted transition.
- [x] **L4 — Height seeds.** **Out of Phase 1.** Pure SDF base only. T1 stays Phase 3.
- [x] **L5 — Voxel scope.** **Single active voxel island** (recommended). SDF subtract for multi-tile.
- [x] **L6 — Tile persistence.** Monolithic `.fl4` for Phase 1–2; memory criterion = VRAM + mesh RAM.
- [x] **L7 — Budgets.** `TILE_METERS=64`, `DENSE_TILE_RES=128`, `MAX_RESIDENT_TILES=25` in `terrain.rs`.
- [x] **L8 — Editor vs game export.** Phase 1 = editor authoring only; export = Phase 5.

**Gate:** Locked in `TERRAIN_MESH_TERRAIN_ADR.md` (2026-08-08).

---

### Phase 1 — Local “Mesh Terrain” island (1–2 weeks)
**Goal:** One tile, full carve, looks good, interactive — **no streaming yet**.

| Work | Uses |
|------|------|
| Terrain island = SdfBody (base) + optional VoxelBody overlay for dig | Existing nodes |
| Dig brush: SDF subtract **or** voxel sculpt in bounds | SDF + voxel tools |
| Mesh always from **chunked DC** (SDF) / Surface Nets (voxel) | Existing extract |
| On stroke end: optional **Voxel Remesh** of poly export / preview mesh | Remesh modifier path |
| Materials + displacement skin | Existing PBR |
| Perf HUD: bake ms, DC ms, tri count, dirty chunk count | Scoreboard style |

**Entry gates (from §12):**

- **No height seed unless T1 landed** (L4). A height-displaced base plane silently takes the whole
  tile onto the CPU full-stack bake. Phase 1 base = pure SDF prims, or T1 first.
- **Tile runs DENSE ≤256** (L3) — that is the only path with a CPU mesh for the remesh bridge. 128³
  is the recommended working size (8.4 MB/tile vs 67 MB at 256³).
- **One voxel island** (L5), not "a VoxelBody per tile".

**Success criteria:**

- Carve a tunnel through a hill in <100 ms local bake at draft res.  
- Settle to full res without multi-second UI freeze (use draft → promote).  
- Remesh of one 128³ tile completes in ≲100 ms on a background thread (measured baseline: ~80 ms,
  §12.9) and never blocks LMB.
- Mesh manifold enough for shade/export after remesh — **stated as a `validate()`/`classify()` pass,
  not a subjective read**.

**Out of scope:** multi-km world, clipmaps, multiplayer.

---

### Phase 2 — Multi-tile + stream (2–3 weeks)
**Goal:** Open-world *feel* with fixed memory.

| Work | Detail |
|------|--------|
| Tile grid + resident window | Hide non-resident; **drop dense `body_grids`** (~8.4 MB/128³) — not atlas |
| Async tile bake/DC | Frame budget: N dirty chunks / frame (existing body_vol path) |
| Placeholder LODs | **Landed** — `terrain_placeholder_mesh` + `ensure_terrain_placeholder_display` until DC |
| Seam policy | **Landed (L2 field stitch):** lattice apron + dual prim membership + dirty fan-out. **Non-manifold across tiles** (one mesh per body). Phase 3 res rings may leave joint silhouette mismatch — not a field gap |
| Save/load | Monolithic `.fl4` + `terrain_tiles` side-channel (L6 store = separate project) |
| Stream expand | Create tiles **only in a transaction** (never mid-frame outside undo) |
| Perf HUD + Rebuild + async settle remesh | **Landed** — HUD ui2; Rebuild sync; full-res bake → rayon remesh after DC stamp (never draft/LMB) |

**Cold-tile memory (correct wording):** Phase 1–2 tiles are dense ≤256. There is no brick
atlas to release. The leak class is `BodyVolumeCache.body_grids` retention after hide.
API: `release_cold_tile_dense_grid` (drop, do not `ram_cache` park).

**Success criteria:**

- Walk/fly across ≥16 tiles without VRAM climb unbounded. **Scope honestly (L6):** until a tile store
  exists this bounds VRAM + mesh RAM only — authored volume data still scales with world size.
- Dig only rebuilds touched tile(s).  
- No full-world rebake ever.
- **Fly-speed criterion (new, §12.9):** at target traverse speed, tiles become resident faster than
  the camera consumes them. The per-frame caps are hard: bakes 4/frame, bake readbacks 1/frame, DC
  results 2/frame, mesh landings 2/frame, LOD switches 2/frame + 1 s per body. A 25-tile refill is
  ≥13 frames of DC alone before any LOD settles. Measure tiles-ready/s, not just frame spikes.

---

### Phase 3 — Visual polish (1–2 weeks)
| Work | Detail |
|------|--------|
| GPU image displace parity | **Landed** — bake (hot[92]/b8) + live march (full[292]/b15); CPU force dropped for single-slot image. |
| Near/far quality | **Landed** — Chebyshev res rings in stream residency (`DENSE` / `RING1=64` / `RING2=32`); user Grid lock respected |
| Normal quality | Smooth Normal / Weighted on terrain poly path if needed |
| Foliage/scatter hooks | Optional later; not blocking |

---

### Phase 4 — Performance hardening (ongoing)
| Budget | Target (adjust with measurement) |
|--------|-----------------------------------|
| Stroke bake (draft, one tile) | < 50–100 ms wall, < 16 ms frame spike preferred |
| DC dirty chunks / frame | Cap; defer rest to background |
| Resident mesh tris | Cap (e.g. 1–3 M visible) via coarser far tiles |
| Remesh on settle | Background; never block LMB drag |

Harness: extend `bench_remesh_stages` + tile bake microbench; no silent “feels ok.”

---

### Phase 5 — Export / game (optional)
- Height pyramid export for classic engines (sample top surface). **Landed:** selected tile → 16-bit PNG (`Export Terrain Height PNG`, `sample_terrain_height_grid`).  
- Mesh tile export (glTF/Nanite-style chunks) for UE Mesh Terrain–like pipelines. **Landed:** multi-mesh `.glb` of visible tiles (`Export Terrain Tiles (glTF)`, world-space, DC display mesh).  
- Keep SDF/voxel source in `.fl4` for re-edit.

---

## 6. Tooling UX (user-facing)

Minimal first:

1. **Create Terrain Island** — bounds + base shape (plane / noise / height import).  
2. **Sculpt** — add/sub/smooth (routes SDF or voxel by mode).  
3. **Boolean stamp** — rock/building SDF cut or union.  
4. **Rebuild mesh** — remesh / promote full res.  
5. **Stream debug** — show resident tiles, dirty chunks.

Match Mesh Terrain intent without cloning Unreal’s full modifier UI in v1.

---

## 7. Risk register

| Risk | Mitigation |
|------|------------|
| One big volume “for simplicity” | Hard rule: tile always |
| Seam / multi-tile surface | **L2 + apron** stops *field* gaps (shared lattice, dual membership). Display remains **non-manifold multi-mesh**. Phase 3 near/far **res rings** mean neighbours can bake at different cell sizes → joint silhouette mismatch is expected, not fixed by L2 alone. Do not claim “L2 prevents cracks” without ADR Seams honesty |
| Remesh every stroke freezes UI | Settle-only + band remesh (already fast path); background thread; skip the half-edge rebuild (26% of cost) for display-only tiles |
| Voxel + SDF double truth | Clear ownership: dig mode picks one; bake order documented |
| **Volume + remeshed poly double truth** (new) | Remesh output is a DERIVED cache, never authored. The moment a user edits the remeshed poly, terrain has two owners and §2's "volume = truth" is dead |
| ~~Cluster LOD on dense terrain~~ **Deferring clusters conflicts with the roadmap** | See L1 / §12.1 — resolve the conflict, don't route around it |
| **Far tiles can't coarsen** (new) | LOD re-mesh requires atlas residency, but atlases are LRU-evicted under a VRAM budget — far tiles are exactly the eviction victims. Needs an explicit **cold-tile state**: LOD baked in at eviction, atlas released, mesh retained (§12.3) |
| **Height seed silently kills the fast path** (new) | L4 gate — `has_image_displace()` forces the CPU full-stack bake (§12.5) |
| New payload breaks .fl4 | Prefer Group of existing bodies; append-only if new type |
| **Tile paging assumed but absent** (new) | L6 — `.fl4` is one blob; "tile set on disk" is a save-format project (§12.8) |

---

## 8. Relationship to older docs

| Doc | Relationship |
|-----|----------------|
| `docs/planning/sdf/TERRAIN_IMAGE_DISPLACEMENT.md` | **Still valid** for local rock stamps on SDF; T1 GPU displace is not merely "high value" — it is a **gate** for any height-seeded tile (§12.5) |
| `docs/planning/render/AZRA_MASTER_ROADMAP.md` §5 | **Conflicts with this plan's §D4** — "the cluster LOD *is* the terrain LOD… no new LOD scheme". Resolve via L1 (§12.1) |
| WP7 clipmap vs cluster | Height demos stay clipmap-eligible; **this plan** is the **3D carve** track |
| Hoppe geomclipmaps | Inspiration for **budgets / stream**, not required algorithm for v1 |
| Mike Turitzin | Closest external analogue: SDF → mesh; terrain special-case efficiency |
| UE Mesh Terrain | Product bar: full carve + remesh + stream/LOD |

---

## 9. Suggested first implementation ticket (when coding starts)

**P1.0 — Terrain Island MVP** — **partial (2026-08-08)**

1. [x] Spawn Group `TerrainIsland.NNN` with one SdfBody `TerrainTile_0_0` base hill (**pure SDF**, no height).  
2. [x] Dense bake target 128³ via `body_vol_cache.set_body_resolution` (existing dirty + chunked DC path).  
3. [x] New prim centres on terrain tiles **clamped** to tile AABB (create + drag via `write_prim_transform_ex`).  
4. [x] Bake-finish **toast** (bake ms + optional tris) + **Perf HUD** line (tiles/vis, bake ms, DC ms, tris, dirty b/r).  
5. [x] Tile constants + lattice helpers in `app/src/terrain.rs`.  
6. [x] Tunnel acceptance: `tunnel_carve_through_hill_air_inside_solid_bulk` (BodyField).  
7. [x] **Perf HUD** (bake ms, DC ms, tri count, dirty body/region) — Phase 2b landed (`refresh_terrain_perf_gauges` + viewport overlay).  
8. [x] **Rebuild Terrain Mesh** remesh of display (quads **or** triangle soup) + dense bake dirty.  
9. [x] **Voxel Remesh on settle** — full-res bake queues; stamp-gate after DC; rayon worker (~80 ms@128³ off frame); gen/stamp discard; Rebuild still sync.

**UI:** Shift+A → SDF → **Terrain Island** / **Terrain Patch 3×3** / **Terrain Stream Toggle** /
**Demo Tunnel Carve** / **Rebuild Terrain Mesh**.

**Phase 2 scaffold notes:**
- Stream (poll) **only** toggles visibility + drops dense `body_grids` + `SdfMeshPool::free_slot`  
  (+ optional CPU `mesh_cache` free). **Never** `transaction()` / never creates nodes from poll.  
- Growth: **Patch 3×3/5×5** or **Expand Terrain Here** (explicit, undoable transaction).  
- Hysteresis: `STREAM_CENTER_COOLDOWN_MS` + `STREAM_SWITCH_DEADBAND_M` on residency centre.  
- **Identity:** NodeId registry + in-blob `ProjectFile.terrain_tiles` side-channel (not outliner names).  
  Clamp / bake toast use `is_terrain_tile_body` (registry → origin+BaseHill). Display names are labels only.  
- `tile_aabb_body_local()` / `tile_aabb_world(tx,tz)` — body-local is origin-centred; world uses lattice.  
- Gates: `poll_sync_never_expands_*`, `expand_here_*`, `may_switch_center_*`, `origin_lattice_identity_survives_rename`,
  `registry_contains_body_not_name`, `tile_aabb_world_uses_tx_tz`, `create_patch_undo_*`.

---

## 10. Success definition (whole feature)

Split into two products (§12.7) — otherwise Phase 5's "export (optional)" quietly carries the whole
competitive claim.

**A. Editor: full-carve terrain authoring — the competitive wedge, achievable on this stack**

- **Carve:** Tunnel through a mountain; walk through in viewport.  
- **Perf:** Local dig stays interactive; world size does not scale bake cost.  
- **Look:** Sharp cliff edges (DC), clean rebuild via remesh, material detail near camera.  
- **Stack:** 100% uses SDF + voxel + remesh + chunked DC — no parallel secret mesher.

**B. Runtime: open-world terrain — gated on work this plan does NOT contain**

- Continuous LOD at horizon distance (Phase D clusters; the sparse path tops out at L2 = ~16×).
- **Material layer blending / virtual texturing** — arguably a bigger visual gap than mesh LOD. A
  carved cliff with one material reads as clay, not terrain.
- **Collision / physics** — table stakes for the roadmap's game-runtime track; no story here.
- Foliage / scatter (explicitly "optional later" — fine for v1, half of what reads as terrain).

Do not claim B on the strength of A.

---

## 11. One-line summary

**AZRA Mesh Terrain = tiled SDF/voxel volumes (full 3D carve) → chunked DC / Surface Nets mesh → optional Voxel Remesh polish → stream resident tiles — performant by locality, pretty by DC + materials, carved like voxels.**

---

## 12. Implementation review — code-grounded (2026-08-08)

Every claim below was checked against the code, not inferred. §§1–11 describe intent; this section
describes what the engine does today. **Verdict: the layering is right and the reuse map is honest,
but three of the plan's pillars — "chunked DC", "sparse bricks", and "Voxel Remesh the DC output" —
are mutually exclusive in the current code, and one Phase-0 decision contradicts a ratified roadmap
decision.**

### 12.1 Rule conflict — cluster DAG (CLAUDE.md "Rule Conflict Check")

This plan said *"cluster DAG **off** for terrain tiles until measured"* and *"keep block_cid /
separate stream path"*. `docs/planning/render/AZRA_MASTER_ROADMAP.md:594-603` says the opposite and is
owner-ratified: *"terrain **wants** Phase D rather than a separate terrain system: the cluster LOD
**is** the terrain LOD… No new pillar, **no new LOD scheme**."*
`docs/planning/sdf/TERRAIN_IMAGE_DISPLACEMENT.md:52-60` repeats it. §D4's "coarser far tiles /
simplified mesh LOD" **is** the second LOD scheme the roadmap forbids.

Reconciliation is cheaper than it looks: `app/src/app_poll_gpu.rs:111-141` builds the cluster
candidate set from `DRAW_KIND_MESH`/`DRAW_KIND_CLUSTERED` draws and `PolygonalBody` cids —
*"SDF/voxel kinds are still excluded by allowlist"*. The DC mesh already lands in `mesh_cache` as
real CPU verts/indices (`app_poll_gpu.rs:486-509`). Admitting a **settled** terrain tile is a policy
change, not a new system. → **Lock L1.**

### 12.2 The dense/sparse fork the plan never makes

There is no single path with all the properties §§2–4 assume. There are two, and they are disjoint:

| | Dense tile ≤256³ | Sparse tile ≥512³ |
|---|---|---|
| Gate | `gridbacked_only` (`body_volume_cache/mod.rs:548-554`) | `sparse_bake_eligible` = Object Mode && res ≥ 512 (`:887`) |
| CPU grid | yes — **67 MB at 256³**, 8.4 MB at 128³ | none; `grid` is EMPTY by design (`:112-131`) |
| CPU mesh for the Remesh bridge | yes (`body_chunked_meshes`, or async readback ≤2/frame) | GPU pool only |
| Per-body LOD | **none** | L0/L1/L2, requires atlas residency |
| Per-body brick atlas | n/a | yes, LRU under `sdf_atlas_budget_bytes` |

**D3 (remesh) only exists on the dense path. D4 (coarse far tiles) only exists on the sparse path.**
The likely answer — near tiles dense ≤256 (editable + remeshable), far tiles sparse or clustered
(drawn only) — needs the near↔far transition as real, budgeted work. → **Lock L3.**

### 12.3 LOD depth is 2 levels, and eviction cancels it

`app/src/sdf_gpu_mesh/lod_policy.rs:22-42`: `LOD_MAX_LEVEL = 2`, splits at 0.5 px / 0.25 px, ≤1 switch
per body per second (`:30`), ≤2 switches per frame (`app_poll_gpu.rs:1117`). Two levels ≈ 16× quad
reduction — a *quality* LOD, not an open-world horizon LOD.

Worse: the LOD re-mesh requires `sdf_brick_atlases[nid].body_nid == nid`
(`app_poll_gpu.rs:1148-1151`), and atlases are LRU-evicted under a VRAM budget (~12 resident 512³
bodies, `gpu_ctx.rs:898-910`). **Far tiles are exactly the eviction victims, so the tiles that most
need coarsening are the ones that cannot coarsen** — their level freezes, and their next edit
escalates to a full re-classify. Needs an explicit **cold-tile state**: LOD baked in at eviction
time, atlas released, mesh retained.

### 12.4 Voxel tiles are not multi-resident — D2's voxel mode does not scale past one island

There is exactly ONE live `VoxelObject` (`app/src/sculpt_state.rs:25`); activating another persists +
frees the current one (`app/src/input_sculpt/ui_state.rs:63-69`). Therefore:

- A dig stroke **cannot cross a tile boundary** — the neighbour is not live.
- Inactive voxel tiles are a static mesh + a CPU grid in the node payload; they cannot receive brush
  writes.
- Anything >512³ displays via atlas raymarch, and the **Display Invariant** forbids multiple
  raymarched objects sharing pixels — so multi-tile voxel terrain is capped at meshed resolutions by
  a non-negotiable rule, not a preference.

D2 is honest for Phase 1 (one voxel island) and wrong for Phase 2. → **Lock L5.**

### 12.5 Height/image displace is a Phase-1 prerequisite, not Phase-3 polish

`core_math/src/primitives.rs:980-984`: `has_image_displace()` → `needs_cpu_full_stack_bake()` → true
→ CPU `BodyField` bake. That means **no GPU bake, no sparse bricks, and therefore no per-body LOD**
for any tile carrying a height seed. §9's ticket says *"one SdfBody base hill (or height-displaced
plane)"* — that parenthesis takes the whole tile off the fast path. T1 (9 BGL sites,
`TERRAIN_IMAGE_DISPLACEMENT.md:64-73`) moves before Phase 1, or Phase 1 bans height seeds. →
**Lock L4.**

### 12.6 Seams: the risk was named, the invariant was not

Each tile is an independent body with its own padded, escape-grown, settle-retightened bounds. Two
adjacent tiles therefore sample **unrelated lattices**, and DC vertices on the shared face will not
coincide — cracks are certain, not occasional. The invariant to lock: *tile lattice origin and cell
size derive from world tile coordinates, never from body bounds; tiles overlap by ≥1 cell; a prim
straddling a boundary is evaluated by **both** tiles.* That last clause conflicts with the current
per-body bake (which evaluates only that body's own prims) and is a real decision. → **Lock L2.**

### 12.7 "Can it compete with Unreal mesh terrain?"

**As an authoring tool — plausibly yes, and better on one axis.** True SDF/voxel carve with booleans,
tunnels and overhangs, plus a remesh button, is more expressive than heightfield-plus-modifiers.
That is a genuine wedge and this stack can carry it.

**As a runtime — not with this plan.** Missing entirely: continuous LOD at horizon distance (§12.3),
material layer blending / virtual texturing, collision/physics, foliage/scatter. See §10's A/B split.

### 12.8 "Streaming" in Phase 2 is really "async baking"

There is no per-tile disk path. `.fl4` is one bincode blob: the whole `scene_graph` clone plus EVERY
voxel grid (`app/src/project_save.rs:124-131`, `app/src/project/mod.rs:31`). Phase 2's *"tile set on
disk; not one giant blob"* is a save-format project under CLAUDE.md's rules (in-blob
`#[serde(default)]`; never a second trailing magic after `FL4ORDER`). Until it exists, the resident
window bounds VRAM and mesh RAM only. → **Lock L6.**

### 12.9 Measured numbers — use these, not placeholders

`core_math/tests/bench_remesh_stages.rs:8-10` (release, this machine, res=128):
density **5.4 ms** · Surface Nets smooth **42.4 ms** · chunked parallel mesher **4.6 ms** ·
`HalfEdgeMesh::from_mesh` **21.0 ms** · `to_raster_data` **10.7 ms**.

- Full remesh of one 128³ tile ≈ **80 ms**; at 256³ ≈ 8× ≈ **0.6 s**. Background-only, per tile.
- The half-edge rebuild is **26%** of that and is only needed if the result becomes an editable
  `PolygonalBody`. Display/cluster-only tiles should skip it.
- Per-frame caps (all hard): bakes **4** (`app_poll_bake.rs:38`), bake readbacks **1**
  (`app_poll_bake_poll.rs:94`), DC results **2** (`app_poll_gpu.rs:1420`), mesh landings **2**
  (`app_poll_gpu.rs:490`), LOD switches **2** + 1 s/body. A 5×5 refill (25 tiles) is ≥13 frames of DC
  alone.
- Memory per resident dense tile: **67 MB at 256³** (5×5 window = 1.7 GB of CPU grids) vs **8.4 MB at
  128³** (210 MB). 128³ is the only sane dense tile size → for a 64 m tile that fixes voxel size at
  ~0.5 m, which is what actually decides whether the intended look is reachable.

### 12.10 Open items this review did NOT resolve

- Cost of the near↔far dense↔sparse transition (L3) — unmeasured.
- Whether a *settled* terrain tile is stable enough for a cluster DAG to be worth building before the
  next edit invalidates it (L1) — needs a re-bake-rate measurement during real authoring.
- `VoxelGrid` memory scaling per inactive tile (dense vs sparse density storage) — not measured here.
