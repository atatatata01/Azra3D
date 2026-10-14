# Mesh-Map Baker — Substance-Class Plan (v2, 2026-08-13)

**Worktree:** `feat/mesh-map-baker` at `.claude/worktrees/mesh-map-baker` (branched from
`master@37e521d`). Do not implement this on the dirty repo-root working tree.

**Goal (owner):** "baking as strong as Substance Painter."
**v2:** external review (Grok) audited v1 against the tree; every load-bearing correction below was
independently re-verified in-tree before adoption. v1's three dangerous errors — D1 persistence,
D4 "re-bake never store", G2 "replace the sample basis" — are fixed here. **Do not implement from
v1.**

**Session status (2026-08-14):** G0–G6 complete. G7: 16-bit + BentNormal + mesh cage +
UDIM + **GPU rays** (worker wgpu any-hit, CPU fallback, `FL4GRAY`). Still parked:
per-map res (D8).

**Standing discipline:** every gate lands with a headless verification and a negative test before
the next starts (`SYNCUPLOADDELETED`, 2026-08-13, is the reason). Gate bar for tests: **no NEW
failures vs the counts measured at gate start** (do not hardcode today's counts into the gate).

---

## 0. Inventory (corrected, verified 2026-08-13)

Paths are `core_engine/app/src/…`.

**Right and kept:**
- `mesh_map_bake.rs`: six map kinds (Curvature/AO/Thickness/Position/ObjectNormal-mislabelled-
  WorldNormal/MaterialId), stable persisted bits 1<<0..5, `ALL` cheapest-first, worker bake via
  `poll_mesh_map_bakes` (menu only queues `PendingMeshMapBake`), AO/Thickness rays vs `TriBlas`.
- Tool Params already has: size presets 512/1024/2048/4096 (clamp 64–4096), per-map enable bits,
  AO/Thickness ray count 8–512, curvature radius, last-bake ms + stale badge keyed on the display
  cache key. **G1 extends this model — no second dialog.**
- ui2 already has `PanelKind::Image` and `PanelKind::UvLayout`, both stubbed ("content coming").
  G4/G5 FILL these; they do not add new panel kinds.
- `bvh.rs` already contains a **nearest-hit traverser** as private test helper `blas_hit` (:1411).
  G2 promotes it (fixed stack, `t_max`, same `ray_triangle`); it does NOT write a third traverser.

**Wrong in v1, corrected:**
- **"ONE shared rasterisation; every map is a function of it" is FALSE today.** Position/Normal/
  AO/Thickness/ID read `UvGeoBuffer`; **Curvature does its own second raster**
  (`src.build_grid`, mesh_map_bake.rs:786) with its own dilation and normal-map source. Maps can
  disagree at seams by construction, and any projection built only into `UvGeoBuffer` would skip
  the one map generators actually consume. → **G0 unifies curvature onto the shared buffer first.**
- **Dilation attribution:** `Writer::dilate` + `UV_DILATE_FRACTION` belong to the live curvature
  writer, not `UvGeoBuffer`. AO/Position/Normal/ID have NO island padding today. → G6 extracts and
  applies it to the shared buffer, all maps.
- **"High-poly" wording:** today's bake is self-mesh on the **post-modifier display mesh** — so
  subdiv/bevel on the SAME body already lands on the unwrap. What is missing is projecting from
  **separate high-poly scene bodies**. The stale module-header comment claiming the modifier stack
  IS the Substance HP→LP story must be rewritten in G2 or the next implementer finishes the wrong
  product.
- "WorldNormal" is **object-space** (`n*0.5+0.5`). Rename to ObjectNormal (label may stay "Normal
  (object)") — with high-poly sources in other nodes the misnomer becomes a correctness trap.
- All maps are RGBA8; Position/Normal will band. `upload_map_16` exists → 16-bit/EXR parked in G7,
  explicitly.
- `show_mesh_maps_panel` is write-only (menu sets, nobody reads). Delete it in G1; do not add
  another toggle.

## 0b. Persistence corrections (the two v1 landmines)

**D1 (settings). The in-blob `#[serde(default)]` pattern CORRUPTS old files.** `project/io.rs`
documents it: bincode is positional, `serde(default)` cannot rescue unwritten bytes ("failed to
fill whole buffer"); poly params/remesh/modifier flags all ship as **self-delimiting tail
sections** (`FL4PPRM`, `FL4PRMSH`, …, append-newest-LAST so older builds stop cleanly).
→ `BakeSettings` is runtime state, persisted as a **new `FL4BAKE` tail**: older AZRA loses only
bake settings. Serialized shape: `enabled_maps: u32` + `Vec<(bit, res)>` — never the enum as a
serde key, never ordinals. `BakeSource` append-only if serialized.
*(Note: the CLAUDE.md POLY_FOUNDATION line still prescribing in-blob-serde(default)/never-tail is
STALE against the tree and flagged to the owner per the Rule Conflict Check.)*

**D4 (pixels). v1's "re-bake on load, never store pixels" fights what already ships.** Mesh maps
already ride `FL4MATTX` as anonymous texture slots (pixels save and load) — but `ui.mesh_maps` is
NOT saved, so on load the registry is gone, `baked_mesh_map()` → None, and generators go
Live/black while the pixels sit resident. And `project_load` deliberately refuses to spawn N
rebakes (freeze risk), so "re-bake on load" is a non-starter.
→ **Persist registry METADATA in the `FL4BAKE` tail** (name, body, kind, res, slot,
settings/display-key hash) and **reconnect to the already-persisted FL4MATTX pixels on load**.
Re-bake happens only when the stale-badge key mismatches AND the user asks. Export PNG from
`slot_rgba` stays.

## 1. Architecture decisions (v2)

- **D1** — settings via `FL4BAKE` tail (above).
- **D2** — projection = nearest-hit ray from the low-poly texel within `max_front`/`max_rear`,
  via the **promoted** `blas_nearest_hit`. **All geometry in ONE space** (world, or low-local with
  the high mesh pre-transformed): the display mesh is body-local and the high-poly is another
  node — v1 ignored transforms entirely.
- **D3** — cage v1 numeric (front/rear distances); **mesh cage landed G7** (optional scene
  bodies, per-texel +n hit is `t_max`; miss falls back to Max Front). GPU/UDIM/per-map still parked.
- **D4** — pixels persist via existing FL4MATTX; registry metadata via FL4BAKE; reconnect on load
  (above).
- **D5** — MikkTSpace-compatible tangent basis in `core_math`, from UV + low-poly smoothed
  normals; relight oracle.
- **D6** — CPU rays first; GPU rays landed G7 (opt-in `gpu_rays`, worker wgpu, CPU fallback).
- **D7 (new)** — **`UvGeoBuffer` gains fields; nothing replaces them.** Keep: low position / low
  normal / coverage / face; add: **low tangent+bitangent** (G3 needs the LOW frame — overwriting
  the normal with the high hit, as v1 said, deletes exactly what tangent-space bake expresses the
  high normal in), and: high position / high normal / high face / miss flag. Self-bake: high ==
  low. Curvature after projection samples the HIGH surface or stays explicitly low-geo — decided
  and documented in G2, not left implicit.
- **D8 (new)** — **texture residency:** `MaterialTextures` has ONE shared `layer_res`; a per-map
  4096 override would resample every material map. v1 of settings is **global res only** (works
  today); per-map overrides arrive only if/when G4 gives mesh maps their own texture array. G5's
  "already resident" claim holds only under this rule.

## 2. Gates (v2 order)

### G0 — Unify + split *(new; prerequisite for everything)*
- Curvature bake consumes `UvGeoBuffer` (one raster; delete the `build_grid` second raster from
  the bake path; keep auto-normalise).
- Split `mesh_map_bake.rs` (~964 lines, will blow past the 500-line rule in G2) into
  `mesh_map_bake/{mod,raster,project,maps,ui}.rs`.
- **Verify:** existing soak `step_meshmap_bake` still green; curvature/ID/AO texel agreement test
  at a UV seam (the disagreement G0 exists to kill); map bytes before/after unification differ
  only where the second raster disagreed with the shared one.

### G1 — Settings model + FL4BAKE
- `BakeSettings { enabled_maps, res_global, supersample, padding_px, source: BakeSource
  {SelfMesh | HighPoly{nids, match_by_name}}, max_front, max_rear, ao_rays, curv_radius }` —
  absorbing the EXISTING Tool Params knobs (rays, radius, presets; add 256 preset). Extend the
  existing UI; no second dialog; delete `show_mesh_maps_panel`.
- Persist as FL4BAKE tail + golden-fixture test (old file loads, settings default).
- `max_front/max_rear/match_by_name/padding_px/supersample` may land UI-visible but unused until
  G2/G6 — labelled as such.
- **Verify:** extend soak `step_meshmap_bake` (both bake buttons read settings; slot dimensions
  assert res; round-trip through save/load fixture).

### G2 — High→low projection
- Promote `blas_hit` → `pub blas_nearest_hit` + brute-force oracle (mirror the any-hit test).
- `MeshMapJob` snapshots high-poly triangles (+ per-name groups) + matrices; projection pass after
  the UV raster fills the D7 high-fields; per-texel miss flag + miss count in the bake log
  ("N texels missed — rear distance too small?").
- Match-by-name `_low`/`_high` suffix groups, per-group BLAS.
- Rewrite the stale module-header comment (see §0).
- **Verify:** dense beveled-cube high + 6-quad low fixture — Position/Normal read the HIGH surface
  (analytic assert); miss count 0 at sane distances, non-zero at rear=0.001 (negative test);
  transform test with the high body offset/rotated (the v1 hole).

### G3 — Tangent-space Normal bake
- MikkT-compatible basis in `core_math` (+ unit tests); `MeshMapKind::TangentNormal`, **bit 1<<6
  append-only** (`enabled = u32::MAX` already turns new bits on — verified semantics).
- Encode high normal in the LOW TBN (D7 fields).
- **Verify:** flat-low/displaced-high analytic case; relight oracle within tolerance.

### G4 — Registry + Image panel *(the "accessible" ask)*
- `BakedImage` registry (D4): auto names (`Cube.Curvature.1024`), renamable; metadata → FL4BAKE;
  reconnect to FL4MATTX pixels on load (miss ⇒ stale badge, never silent Live fallback).
- **Fill `PanelKind::Image`**: list (name/kind/res/age/body), thumbnail, view large, re-bake,
  delete, Export PNG (`slot_rgba`).
- Generators/masks pick maps **by name**; every consumer (curvature generator, panel, tool-params
  list, soak — the current direct `ui.mesh_maps` readers) goes through the registry/`baked_mesh_map`.
  The two-consumer trap is ALREADY live in-tree; G4 closes it.
- **Verify:** registry unit tests; save/load reconnect fixture (pixels present + metadata present
  ⇒ generator stays Baked after load — the exact hole D4-v1 would have shipped); export hash test.

### G5 — UV viewport texture underlay + dropdown
- On the **viewport UV split** (`poly/uv.rs`) — the ui2 `UvLayout` pane stays stubbed unless it
  replaces the split (owner call, not this plan's).
- There is NO texture underlay today (no checker either): draw the underlay from `slot_rgba` (CPU
  thumbnail path exists) or a GPU textured rect; **no per-frame `upload_map`**.
- Dropdown top-left: Selected layer's texture / BakedImage by name / material slot by name /
  checker.
- **Verify:** state assert per source; no allocation in the draw loop (single-res rule D8).

### G6 — Quality
- Extract `Writer::dilate` → shared-buffer dilation for ALL maps, `padding_px` from G1
  (seam-bleed regression test: checker across an island edge, no fringe after 2-mip downsample).
- True supersampling (raster at res×SS + box filter) — the existing 4×4 in `rasterize_uv_geo` is
  coverage-AA, not SS; keep it, add SS on top.
- AO/Thickness: blue-noise ray sets + variance-vs-raycount test (the knob itself already exists).

### G7 — 16-bit + BentNormal + mesh cage + UDIM + GPU rays landed; rest parked
Landed: dual-plane 16-bit; `BentNormal`; mesh cage; UDIM; GPU rays (`FL4GRAY` — same
TriBlas any-hit + R2 hemisphere on a worker wgpu device; CPU fallback). Still parked:
per-map resolution overrides (needs the D8 texture-array decision).

## 3. Order

**G0 → G1 → G2 → G3** spine; **G4 → G5** visibility (may interleave after G1 — but G4 before its
D4 amendment would save orphan slots and lose the index on reload, so D4 lands WITH G4); **G6**
hardens. Leave-as-is list from review: CPU rays first, numeric cage, append-only bits, worker in
poll, stale badge, loud BakeNotes, SelfMesh default (the owner's modifier-stack path IS the
default high-rez story; scene high-poly is additive).
