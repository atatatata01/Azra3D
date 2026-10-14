# MATERIALS_UX.md — Materials UX Overhaul (owner ask 2026-07-17)

Owner, verbatim: *"I need to be able to create material — right now our material tab is a mess,
lots of testing old tech. I need to be able to create a material that will live in the OUTLINER;
in Properties I can assign it to an SDF object or mesh. Material is simple — a place where I can
plug basic maps — will develop it later."*

Backend was already correct (graph `NodePayload::Material` nodes, `MaterialId` refs,
`resolve_prim_material`, `material_textures` array + `material_map_loads` poll upload, save v7).
This work is UX + cleanup on top of it. No save-format change of any kind.

---

## 1. What shipped

### Outliner — Materials section (`outliner_draw_material.rs`, new)
- Dedicated collapsible **"🎨 Materials (N)"** section (below Lights), listing every
  `NodePayload::Material` node sorted by stable `MaterialId` (HashMap order is random — sorted).
- Row = sphere preview swatch (`mat_preview_cache`) + name. Select = `SceneItemId::Node(nid)` via
  `select_single_unified` / `toggle` / `range` (unified-selection writers only).
- **Rename** via `check_double_click` + `draw_rename_widget` (the outliner rename rule — never
  `resp.double_clicked()`), committed in `transaction("Rename material")`.
- Context menu: Rename / Duplicate / Delete. Hover shows "Used by N prim(s), F face(s)".
- **"+ New Material"** at the section bottom + a **"🎨 Material"** item in the outliner **+** menu.
  Both route through `create_material_interactive()` (transaction, select, inline rename starts).
- The generic tree walk now **skips Material nodes** (like Lights) — the old generic "MAT" rows had
  a silently no-op rename and an unguarded delete.

### Delete-in-use guard (the friendlier option: reassign-to-default, undoable)
- Chosen over hard-blocking: deleting an in-use material **reverts every user to the default
  material in the SAME transaction** (prims → `MaterialId::NONE`, poly faces → `0`) and the toast
  says exactly what happened: *"Deleted 'X' — N prim(s) and F face(s) reverted to default (Ctrl+Z
  restores)"*. One Ctrl+Z restores the material AND all assignments (transaction = full graph
  snapshot).
- Structurally enforced: `delete_node_no_undo()` (the ONE deletion core — Del key, outliner menu,
  `delete_selected`) clears references for Material payloads before removal. Not cosmetic:
  `add_material` allocates `max+1`, so a dangling id could be REUSED by the next material and
  silently re-skin old users.

### Properties — assignment
- **SDF prim** (`properties_sdf_material.rs`): the Material combo is now always visible at the top:
  `Embedded (local)` / every graph material / **"➕ New Material"** (creates a shared material
  **seeded from the prim's current embedded look** — nothing visibly changes on creation — and
  assigns it; one transaction). A **✏ jump button** selects the material node so its editor shows.
- **SDF body** (`properties_sdf.rs` `draw_props_body`): same combo at body level — assigns
  `material_id` to **every prim of the body** in one transaction (`assign_material_to_body_prims`).
  Shows "Mixed (per-prim)" when prims disagree.
- **Poly body** (`properties_poly.rs`): same combo — object-level assignment writes ALL faces'
  `HFace.material_id` (the already-serialized Ctrl+M mechanism; `assign_material_to_poly_body`).
  Face-mode per-face assignment stays on Ctrl+M (unchanged).
- **When linked, the embedded editor is hidden** (prim + poly): the resolver ignores the embedded
  material then; the old UI showed dead sliders. A banner + the jump button replace it. Prim-level
  features that stay live regardless (shader graph, DNA, texture-atlas id) remain visible.

### Poly assignment now actually DISPLAYS (bug fix)
- New canonical resolver `SceneGraph::resolve_poly_body_material(mesh, embedded)` (sibling of
  `resolve_prim_material`): first face's `material_id` → graph material; `0`/dangling → embedded.
- Wired into **raster** (`raster_gbuffer.rs` PolygonalBody arm) and **PT** (`pt_scene.rs`).
  Before this, Ctrl+M face assignment had **zero visual effect** — both paths read only the
  embedded body material.
- Verified payload support (owner ask said VERIFY, don't invent): `PolygonalBody` = embedded
  `PbrMaterial` + serialized per-face `material_id` (→ linked assignment shipped);
  `VoxelBody` / `NurbsBody` = ONE embedded `PbrMaterial`, `ImportedMesh` = `Vec<PbrMaterial>`
  (first used) — **no reference field exists** on any of them, so they keep their embedded editors
  (voxel: sculpt-panel material; nurbs: shared widget). No per-face materials invented; no payload
  fields added (bincode field-trap).

### Material editor (`properties_material_node.rs`, new)
Selecting a material node shows: preview + "Used by N prim(s), F face(s)", **Name**,
the shared `draw_pbr_material` widget (albedo tint / roughness / metalness + **albedo map** row —
load file / AI maps / clear / strength / tiling / projection, via `MapTarget::Graph` and the
existing `material_map_loads` poll upload — Frame Ordering respected), **Emission** color +
intensity (fields `PbrMaterial` already has), and three **disabled placeholder slots**
(Normal / Roughness / Metallic — "coming soon") so the layout is future-shaped **without touching
`PbrMaterial` or the save format**.
Undo: widget mutates a LOCAL snapshot; `begin_episode` (drags) / `transaction_snapshot_only`
(one-shots) fire BEFORE the graph write-back, so the captured state is genuinely pre-mutation.
Edits set `prim_buf_stale` + `RENDER` (materials never move geometry — no bake).

---

## 2. Legacy audit (consumers-grepped verdicts)

| Control / system | Where | Consumers found | Verdict |
|---|---|---|---|
| `PanelId::MaterialEditor` "Material Editor" panel | `panel.rs` | **Zero draw sites** — registry-only phantom checkbox in Panel List | **DELETE** (dropped from default layout + hidden in Panel List; enum variant kept for serde compat with old `ui_layout.json`) |
| Second "🔗 Node Editor..." button | `properties_sdf_material.rs` | Same window as the "Edit/Add Shader Graph" row 300 lines above | **DELETE** (duplicate entry point) |
| Generic outliner "MAT" rows | `outliner_draw_poly.rs` walk | Rename `_ => {}` no-op; delete without ref clearing | **REPLACE** with the dedicated Materials section (walk now skips Material payloads) |
| "Shared Material" combo (prim) | `properties_sdf_material.rs` | Live (`material_id` → resolver) | **MIGRATE** — promoted to always-visible, + New Material, + jump button |
| Embedded PBR editor shown while linked | prim + poly tabs | Resolver ignores `p.material` when `material_id` set → dead sliders | **HIDE when linked** (banner + jump instead) |
| Shader Node Graph row + presets | `properties_sdf_material.rs` | LIVE: `rebuild_shader_pipelines` → `build_deferred_wgsl(uber)` (dirty.rs:226) | **KEEP** (one entry point) |
| Material DNA section | same | LIVE: `eval_material_dna` in `deferred_entry.wgsl` + `pathtrace_entry.wgsl`; serialized per prim | **KEEP** (prim-level; visible even when linked) |
| ShaderType / Quality combos, Advanced (SSS/Glass/Clearcoat/Aniso/Sheen), SDF Procedural Layers (edge wear / cavity dirt) | same | LIVE: `graphics/serialize.rs` prim buffer + `sdf_material.wgsl`/PT | **KEEP** (embedded-material fields — shown only in embedded mode) |
| Texture atlas control (`texture_id`) | same | LIVE: serialize.rs [83] + `prim_texture_id` in `sdf_lighting.wgsl` | **KEEP** (conditional, prim-level) |
| AI material query / Analyze Image | same | LIVE: `ai_service` TextToProfile → embedded material | **KEEP**, hidden when linked (it writes the ignored embedded material) |
| Ctrl+M assign popup | `material_assign.rs` | LIVE: the face-mode assignment path | **KEEP** (object-mode inner loop now shared with the properties combo via `apply_material_all_faces_no_undo`) |
| Agent-bar quick "Materials:" preset row | `ui_panels.rs` | Writes embedded prim materials (resolver fallback) | **KEEP** |
| Content library material apply | `content_library.rs` | Writes embedded prim materials | **KEEP** |
| `MaterialLibrary` type | `app_types/types_library.rs` | v4/v5 file deserialization only (`project_load.rs` migration) | **KEEP** (serialization-compat only, per CLAUDE.md) |
| sRGB↔linear helper triplication | `properties_sdf_material.rs`, `material_editor.rs`, `ui_panels.rs` | — | **FLAGGED for extraction** (not this task's scope; material_editor's pair made `pub(crate)` and reused by the new editor instead of a 4th copy) |

## 3. Bugs fixed en route
1. **Ctrl+M poly assignment had no visual effect** — raster + PT ignored `HFace.material_id`
   (read embedded only). Fixed via `resolve_poly_body_material` in both paths.
2. **Outliner "MAT" rows: rename silently no-op'd, delete left dangling `material_id` refs**
   (reusable ids → future re-skin hazard). Fixed by the dedicated section + ref clearing in the
   shared deletion core.
3. **Phantom "Material Editor" panel** in Panel List (no draw site ever existed).

## 4. Test coverage vs owner-verified
**Test-covered (all green):**
- `core_math` `scene_graph::tests` (5 new): create/rename/delete via graph API; id-reuse contract;
  `material_use_counts` + `clear_material_references` (the delete guard's core);
  `resolve_poly_body_material` (reference wins / fallback / dangling); snapshot-restore semantics
  (the exact mechanism `transaction()` undo uses) restores materials + assignments exactly.
- `app` `dc_gate0_materials_assignment_roundtrip` (new): created material with authored values →
  assigned to SDF prims + poly faces → `save_binary`/`load_binary` → values, assignments, resolver
  results, and use-counts all survive. Existing Gate-0 golden/round-trip/pick tests untouched and
  green (`cargo test -p app --bins -- --test-threads=1`: 227 passed; `cargo test -p core_math`:
  563 passed). No shader files touched (graphics parity gate not applicable).

**Owner-verified only (UI can't be headless-tested; no test constructs an `AzraApp`):**
outliner section drawing/row interactions, combo dropdowns, jump-to-material navigation,
rename double-click feel, toasts, viewport appearance of assigned materials, map load file dialog.

## 5. Owner manual test script
1. Outliner **+** → **🎨 Material** (or Materials section → "+ New Material") → a "Material.001"
   row appears selected with rename active → type "Steel", Enter.
2. Double-click the row → rename works again (single click selects, no accidental rename).
3. With the material selected, Properties shows the editor → set albedo to red, Roughness 0.2 →
   preview swatch updates. Click "Load albedo map…" → pick a PNG → map row appears (strength/tiling).
4. Select an SDF prim → Material combo → "Steel" → prim turns red in the PBR viewport.
   Click **✏** → jumps to the material's editor. Combo → "➕ New Material" on another prim →
   new material inherits that prim's look, nothing visibly changes.
5. Select a poly body → Properties → Material → "Steel" → the mesh turns red (raster AND
   path-traced modes). The embedded editor is hidden with the "shared material" banner.
6. Select an SDF **body** (no prim) → Material combo assigns to all its prims at once.
7. Ctrl+S save → File → reload → all assignments and the albedo map still there.
8. Outliner → right-click "Steel" → Delete → toast: "Deleted 'Steel' — N prim(s) and F face(s)
   reverted to default (Ctrl+Z restores)" → objects go back to their embedded look.
9. Ctrl+Z at every step above: rename, edits, assignment, deletion — each is one undo step
   (slider drags = one step per drag).

## 6. File list (all uncommitted, this worktree)
- `core_engine/core_math/src/scene_graph.rs` — `material_node_id`, `material_use_counts`,
  `clear_material_references`, `resolve_poly_body_material` (+ `remove_material` reuses the
  extracted lookup); 5 new tests.
- `core_engine/app/src/outliner_draw_material.rs` — **new** Materials section.
- `core_engine/app/src/properties_material_node.rs` — **new** material-node editor +
  `handle_graph_albedo_map_outcome`.
- `core_engine/app/src/material_assign.rs` — `create_material_interactive`,
  `create_shared_material_from_prim`, `duplicate_material_node`, `delete_material_node`,
  `assign_material_to_poly_body`, `assign_material_to_body_prims`,
  `apply_material_all_faces_no_undo` (extracted, shared with Ctrl+M).
- `core_engine/app/src/outliner_draw.rs` — section call, Material skip in generic walk, + menu item.
- `core_engine/app/src/outliner_panel.rs` — deferred `mat_create`/`mat_duplicate`/`mat_delete`.
- `core_engine/app/src/scene_actions.rs` — Material ref clearing in `delete_node_no_undo`;
  `duplicate_subtree` C4b: fresh `MaterialId` on duplicated Material nodes (Shift+D on a material
  row would otherwise produce TWO nodes with the SAME id → ambiguous `material_by_id`).
- `core_engine/app/src/properties.rs` — material-node dispatch + deferred combo actions.
- `core_engine/app/src/properties_sdf.rs` — PropsContext fields + SDF-body material combo.
- `core_engine/app/src/properties_sdf_material.rs` — combo promoted, linked-mode hiding,
  duplicate Node Editor button removed.
- `core_engine/app/src/properties_poly.rs` — poly Material combo + linked-mode hiding.
- `core_engine/app/src/raster_gbuffer.rs`, `pt_scene.rs` — poly material resolution.
- `core_engine/app/src/panel.rs` — MaterialEditor phantom removed from default + Panel List.
- `core_engine/app/src/material_editor.rs` — color helpers `pub(crate)` (reuse, no 4th copy).
- `core_engine/app/src/dc_baseline.rs` — Gate-0 materials round-trip test.
- `core_engine/app/src/main.rs` — 2 module registrations.

---

# EVOLUTION 2 (2026-07-17b) — Explicit materials, multi-map + box projection, library drag-drop

Owner, verbatim: *"the material should not be directly inside the outliner but should be an item
that user adds (Add Material); from there user can add different maps (choose projection type —
start with box, will add others later); if this works properly I would be able to click-drag
material from my library that I just downloaded to texture SDF."*

## E2.1 Part 1 — Materials are EXPLICITLY-ADDED items
- **No auto "Default Material" node anymore** (removed from `project_new.rs` and the
  `project_load.rs` zero-materials fallback). Zero materials is a fully valid state: resolvers
  fall back to embedded materials (`MaterialId::NONE` semantics unchanged), the delete guard
  reverts to NONE, combos offer "➕ New Material".
  *NOTE: the CLAUDE.md line "Default scene creates a 'Default Material' node" is now STALE —
  superseded by this owner refinement. Old .fl4 files that contain the node keep it (it's in
  their blob); only the auto-creation is gone.*
- **Outliner Materials section renders ONLY when ≥1 material exists** (`outliner_draw_material.rs`
  early-return). Creation entry points: outliner **+** menu (existing) + **main menu Add →
  🎨 Material** (new, `menus.rs`), both via `create_material_interactive()`.

## E2.2 Part 2 — Multi-map materials with BOX projection
**Struct extension (field-trap-safe):** `PbrMaterial` gains `normal_map` / `roughness_map` /
`metallic_map` as **`#[serde(skip)]` `TexMap`s** — ZERO wire bytes, the bincode blob stays
byte-identical (guarded by `dc_gate0_committed_ref_fl4_loads_unchanged`, which loads the
committed pre-change ref binary). `MapKind` (Albedo/Normal/Roughness/Metallic, stable
`wire_tag`s) + `MapProjection` (Box=0 live, Uv=1; Planar/Sphere = announced extension points)
live in `core_math/pbr_material.rs`; `PbrMaterial::map/map_mut(kind)` is the one channel mapping.
**Consequence (by design):** multi-map channels persist for GRAPH materials only — embedded
prim/body materials can't carry them across a save (they're the owner's "Add Material" feature).

**Persistence — FL4MATX2 side-channel:** third self-delimiting tail section (sibling of
`FL4ORDER`/`FL4MATTX`): `Vec<MaterialChannelRecord { material_id, kind, tex_id, strength,
uv_scale, projection }>`, written only when non-empty, reattached by `MaterialId` on load
(`apply_material_channels`). The map **pixels needed nothing new** — FL4MATTX already persists
EVERY used texture-array slot regardless of which channel references it. An older AZRA hitting
the unknown magic stops reading the tail (loses only the new channels, never the blob). Gates:
`material_channels_roundtrip_and_coexist_with_other_sections` (all three magics in one file),
`no_material_channels_has_no_magic_and_loads_empty` (byte-compat), extended
`dc_gate0_materials_assignment_roundtrip` (authored channels round-trip; also asserts the
channels are ABSENT from the blob pre-reattach — the field-trap guard).

**Linear data in an sRGB array (documented honest trade):** the array stays ONE
`Rgba8UnormSrgb` texture (a second linear array would grow every consuming BGL/bind group for
one bit of semantics). Data maps are **pre-encoded to sRGB on the CPU**
(`decode_file_to_data_map`: byte' = encode_srgb(byte/255)), so hardware decode returns the raw
value exactly, up to 8-bit re-quantization (error ≲ 1.2/255 near white, finer near black —
on par with plain 8-bit storage).

**Shading (`render/src/gbuffer.wgsl` + `RasterMaterial` lockstep, 13×vec4 = 208 B):**
- **Box projection is BODY-LOCAL** (owner decision): triplanar samples at the pre-instance
  vertex position with the local normal (new `local_position`/`local_normal` varyings) — SDF DC
  meshes, poly bodies and voxel chunks are all body-local by construction, so textures STICK
  when a body moves/rotates (zero rebake, instance matrix only). The old albedo triplanar was
  world-space; it moved to local with this change.
- **Normal maps: tangentless "whiteout" triplanar blend** (Ben Golus) — chosen because no AZRA
  mesh carries a tangent basis; swizzles re-derived for this file's `p.yz` X-plane convention
  (differs from the textbook `p.zy`) so color and normal features land on the same texels.
  Perturbed LOCAL normal → world via the instance normal matrix (3 new flat varyings — exact
  under non-uniform scale). Shadowing keeps the GEOMETRIC normal (anti-acne offset must track
  the true slope). UV-space normal mapping is unsupported (needs tangents) — normal maps are
  always triplanar.
- **Roughness/metallic maps REPLACE the scalar** by strength (`mix(scalar, map.r, strength)`) —
  ambientCG value maps hold actual values; the albedo keeps its tint-multiply convention.
- **PT: out of scope** — `pt_scene` resolves the material but samples no maps (scalar values
  render). Same for the embedded-prim graphics serializer (`graphics/types.rs` albedo-map
  fields untouched).

**Headless GPU proof (`render/src/tests_triplanar.rs`):** the `// ---- Material Map Sampling
----` section of gbuffer.wgsl is extracted VERBATIM (markers are load-bearing) into a compute
probe on a real device: box-projection axis invariance (+X face ignores x — the "sticks to
local space" property), checker texel lookup, flat-normal-map identity, and per-face whiteout
tilt orientation (pins the derived swizzles) — all asserted from readback. The fs wiring of
`local_position` into those functions is the one part only owner-verifiable visually.
`gbuffer_wgsl_parses_and_validates` guards the WGSL statically.

**Editor (`properties_material_node.rs`):** the three "coming soon" slots are LIVE — Load…
(file picker → `decode_file_to_data_map` → `material_map_loads`, Frame Ordering respected),
Clear + per-map strength (undoable via the standard snapshot write-back), plus a per-material
**Projection combo** (Box live, UV, Planar/Sphere disabled "soon") and **Tiling (all maps)**
that write all four channels uniformly. Like albedo, the async tex_id write-back itself is not
a separate undo step.

## E2.3 Part 3 — Library drag-drop (`material_import.rs`, new)
Drag an `assets/PBR/<Name>/` folder material (already scanned by `content_library.rs` with
role-tagged files) onto:
- **a viewport object** — hovered SDF prim (CPU raycast) → its body via
  `find_body_containing_prim`; else the async GPU colour-ID pick (`pick_node_at`, the same one
  click-select uses) dispatched at the drop point, with `handle_pending_pick` routing the
  result into the import via `pending_material_drop` instead of selecting;
- **an outliner body row** — `material_row_drop_check` hook in the SDF-body header + generic
  node row (highlights valid targets, consumes the payload on release so the viewport handler
  can't double-fire).

Drop = **ONE transaction**: create the graph material (named after the folder; Box projection +
1.0 world-unit tiling = TexMap defaults), assign (SDF body → every prim's `material_id` +
`sync_flat_cache`; poly body → every face — the same inner helpers the combos use). Ctrl+Z
removes material + assignment together. Voxel/NURBS/imported-mesh targets toast (no
`material_id` reference field exists — verified in §1). **Decode is a worker thread** (ambientCG
maps are 1–4K); results stream through `material_import_rx` → `poll_material_imports` (budgeted
`MAX_IMPORT_MAPS_PER_FRAME = 4`, Frame Budget rule) → the existing `material_map_loads` upload →
`apply_map_slot(kind)`. Click-to-apply (non-drag) keeps the legacy embedded-prim path.

`content_library.rs` touched MINIMALLY (owner has uncommitted local edits — merge risk): ONE
insertion in `handle_library_drag_drop`'s release branch (~7 lines: route folder-material drops
to `drop_library_material_on_viewport`, then `return`). Everything else lives in
`material_import.rs`.

## E2.4 Files touched (evolution 2, uncommitted)
- `core_math/src/pbr_material.rs` — MapKind, MapProjection, 3 skip-field TexMaps, map/map_mut.
- `core_math/src/primitives.rs` — re-exports.
- `render/src/mesh_gpu.rs` — RasterMaterial +3 vec4 (lockstep), `pack_tex_map`.
- `render/src/gbuffer.wgsl` — MaterialData +3 vec4; local varyings + normal-matrix varyings;
  body-local triplanar; whiteout normal blend; scalar map sampling; section markers.
- `render/src/tests_triplanar.rs` — **new** GPU probe + parse test; `render/src/lib.rs` — mod.
- `app/src/material_textures.rs` — PendingMapLoad.kind, `decode_file_to_data_map` (+LUT).
- `app/src/material_editor.rs` — `apply_map_slot(kind)`, existing loads tagged Albedo.
- `app/src/properties_material_node.rs` — live map slots, projection combo, tiling, data-map load.
- `app/src/project.rs` — MaterialChannelRecord, FL4MATX2 magic, save/load tail arms, 2 gate tests.
- `app/src/project_save.rs` — `collect_material_channels` (+ from_app wiring).
- `app/src/project_load.rs` — `apply_material_channels` (+ apply_to_app wiring); default-material
  fallback removed.
- `app/src/project_new.rs` — default-material creation removed.
- `app/src/outliner_draw_material.rs` — hide section when empty.
- `app/src/menus.rs` — Add → Material.
- `app/src/material_import.rs` — **new** (import transaction, worker decode, drop targeting).
- `app/src/content_library.rs` — the ONE routing insertion (see E2.3).
- `app/src/outliner_draw_body.rs`, `outliner_draw_poly.rs` — row drop hook (1 call each).
- `app/src/app_poll.rs` — `poll_material_imports` call + kind pass-through.
- `app/src/app_poll_misc.rs` — pending-material-drop interception in `handle_pending_pick`.
- `app/src/main.rs` — module + 2 fields; `app/src/app_new.rs` — field init.
- `app/src/dc_baseline.rs` — `project_with` channels, round-trip extension, committed-ref gate.

## E2.5 Gates (all green)
`cargo build -p app` clean · `cargo test -p app --bins -- --test-threads=1` **230 passed**
(227 + FL4MATX2 coexist/compat gates + committed-ref backward-compat; materials round-trip
extended in place) · `cargo test -p core_math` 565 · `cargo test -p render` 33 (incl. the new
GPU probe, run on real hardware) · `cargo test -p graphics` all Naga variants green (no
graphics WGSL touched). Goldens + pick untouched.

## E2.6 Owner manual test script (evolution 2)
1. New scene → outliner has NO Materials section. Add → 🎨 Material (main menu or outliner +)
   → section appears with the new material selected, rename active.
2. In its editor: Load albedo / Normal / Roughness maps from files → object assigned via the
   combo shows them (box-projected). Projection combo shows "Box (triplanar)"; Tiling scales
   all maps together; per-map strength sliders work; Ctrl+Z steps back each edit.
3. Assign to an SDF body → **move/rotate the body → the texture sticks** (body-local box
   projection — the core visual check the harness can't see).
4. Open the Library → Materials → drag e.g. "Bricks" onto an SDF body in the viewport →
   toast "Applied 'Bricks' — loading 3 map(s)…" → fully textured (color + relief + roughness)
   within a second. Drag another onto an outliner body row (row highlights) → same.
5. Ctrl+Z after a drop → material AND assignment gone in one step.
6. Ctrl+S → reload → maps, projection, tiling, assignments all back (FL4MATTX + FL4MATX2).
7. Load a pre-today .fl4 → loads clean, no Materials section beyond what the file carries.
8. Path-traced mode: assigned materials show scalar values only (maps are raster-path; PT
   sampling is out of scope — expected).

---

# EVOLUTION 3 (2026-07-18) — P1 COPY_SRC fix, prim material inheritance, outliner shader-tree rows, stall verdict

## E3.1 P1 — `material_tex_array` missing COPY_SRC (uncaptured validation error, crash class)
Owner log: `[GPU] UNCAPTURED ERROR: Validation { CommandEncoder::copy_texture_to_texture ...
Texture "material_tex_array" usages COPY_DST|TEXTURE_BINDING do not contain required COPY_SRC }`.
- **Root cause:** `upload_map`'s GROW path (E2 addition) copies the old array into the new bigger
  one with the old texture as `copy_texture_to_texture` SOURCE — which requires `COPY_SRC`.
  `create_texture` requested only `TEXTURE_BINDING | COPY_DST`. First exercised at startup by the
  FL4MATTX/FL4MATX2 multi-map restore: >3 restored maps → grow past the initial 4-layer capacity.
- **Fix at creation:** the ONE descriptor is now an inspectable fn
  (`MaterialTextureArray::texture_descriptor`) including `COPY_SRC`; `create_texture` consumes it.
- **Copy audit:** the grow-copy is the ONLY copy involving this array (repo-wide grep); both
  `upload_map` call sites are inside `poll_background_tasks` (`app_poll.rs:52` restore /
  `app_poll.rs:66` load drain) — **Frame Ordering was already respected**; the defect was purely
  the missing usage flag. `reset()` recreates with the same descriptor.
- **Regression test:** `material_textures::tests::descriptor_usages_support_grow_copy` (device-free
  — asserts COPY_SRC + COPY_DST + TEXTURE_BINDING on the creation descriptor across layer counts).
- Comment debt fixed: two comments claimed "the GPU texture is not COPY_SRC" as the reason
  `slot_pixels` exists — save STILL reads the CPU pixels (no readback added); reworded.

## E3.2 Owner ask 1 — a new prim inherits the body's material
*"When I add a new cylinder inside the body it changes the material to default."*
- **Graph-level definition** (`SceneGraph::body_prevailing_material`, scene_graph.rs): the
  prevailing material = the `material_id` of the **LAST prim in the body** — chosen over a
  majority vote for predictability: stable under assign-to-body (writes all prims uniformly) and
  chains as prims are added. Empty body / non-body / unassigned last prim → `NONE` (exactly the
  old behavior — embedded default).
- **Applied at fresh-construction sites only** (`inherited_material_for_create()` in creation.rs):
  `add_primitive_with_op` (Add menu / Shift+A / pie / context menu / polyline / dev tools /
  properties), create tool `instant_create_at_origin` / `click_create_at_point` / `start_create`
  (footprint drag), and the **BoolCut cutter** (cut surfaces render with the cutter's material —
  a default-gray cutter would paint gray gouges into a textured body). Deliberately **NOT** in
  `add_prim_to_graph`: paste / duplicate / instance / mirror / agent prims carry authored ids
  that must not be rewritten. Billboard image-drop and the lab mesh→SDF prim keep NONE (they
  display their own textures). Scope note: inheritance covers the `material_id` REFERENCE (the
  owner's "Add Material" workflow); embedded-material-only bodies (all NONE) keep the old
  default-embedded look for new prims, per the ask.
- **Test:** `create_prim_inherits_body_prevailing_material` (core_math scene_graph tests) —
  NONE cases, inheritance, resolver identity (old + new prim resolve to the SAME graph material),
  chaining, mixed-body last-prim rule.

## E3.3 Owner ask 2 — expandable materials in the outliner (Modo shader-tree style)
- **Parent row** (`outliner_draw_material.rs`): sphere-preview row unchanged, plus a caret
  (▶/▼, disabled-but-space-reserving when no maps are loaded, so names stay aligned). Expansion
  state = `OutlinerState.expanded_materials` (same uniform semantics as `expanded_bodies`).
- **Child rows:** one per LOADED channel — 🖼 icon + `"<map name> — <channel>"` with the owner's
  Modo channel naming (Albedo shows as **"Diffuse Color"**; editor keeps "Albedo"). Map names
  come from a new **runtime slot-name registry** (`MaterialTextureArray.slot_names`, lockstep
  with `slot_pixels`): every load path now feeds `PendingMapLoad.name` (file stem via
  `map_display_name`; import worker sends the ambientCG file stem through its channel; AI maps =
  "AI Albedo"). **Names are runtime-only** — FL4MATTX stores pixels, and extending FL4MATX2's
  record shape would corrupt yesterday's saves — so slots restored from a save fall back to the
  material's own name ("Steel — Roughness"). Persisting names = future work (would need a new
  self-delimiting tail section).
- **Child interactions (implemented):** click = select the parent material AND focus the editor
  on that channel — `OutlinerState.focus_map_channel` drives an accent-frame highlight + one-shot
  scroll in `properties_material_node.rs` for ~2.5 s (albedo focus = banner above the shared PBR
  widget, which owns the albedo row). Context menu = **Clear Map** → deferred
  `OutlinerDeferred.mat_clear_map` → `clear_material_map_channel()` (one transaction, same marks
  as the editor write-back, toast + Ctrl+Z). **No child drag, no reordering** — that is future
  shader-tree work, noted here on purpose. Children are not selection items (no `SceneItemId`;
  range-select skips them) and are hidden while filtering.

## E3.4 Stall investigation — VERDICT: reported, not fixed here
Owner log: `[STALL] frame=662ms layout=616.6` repeating ([STALL] printed by `app_update.rs:431`
when wall > 500 ms; `layout` = the egui panel-build section, s[5]).
- **The prompt's prime suspect is NOT in committed code:** library thumbnails ALREADY decode on a
  background thread with a 128 px disk cache and per-frame budgets (content_library.rs:11-80
  "Background Thumbnail Loader (Phase 2+3: async decode + disk cache)"; enqueue ≤8/frame :291,
  poll ≤6/frame :323, placeholder until ready). The prescribed fix shipped earlier.
- **What IS confirmed in the committed draw path:** `draw_content_library` calls
  `scan_content_library()` synchronously in the layout phase on EVERY library open
  (content_library.rs:269; `scanned` reset on ✕-close :605 → reopen rescans): nested
  `fs::read_dir` walks over assets/PBR / HDRI / matcaps, plus a **full-resolution
  `image::open` of the first matcap** (:243, one-shot while `matcap_data` is empty) and a GPU
  `upload_matcap_to_gpu()` from inside the draw (a latent Frame Ordering violation, also in the
  matcap click path). A 4K PNG decode is exactly ~600 ms-class; the read_dir walk repeats per
  open. This explains an open-time stall but not a continuously repeating one while idle.
- **Why not fixed here:** content_library.rs is a merge-risk zone (owner has uncommitted local
  edits — mandated minimal footprint; this task touched it ZERO lines), the remaining sync
  decode is one-shot per run, and a repeated-while-idle stall most likely lives in the owner's
  uncommitted library edits which this worktree cannot see. **Recommended follow-up task:** move
  `scan_content_library` + the matcap preload decode to the existing thumb worker (or a sibling),
  apply the matcap GPU upload in `poll_background_tasks` (fixes the Frame Ordering violation in
  the same stroke), and have the owner re-log with their local edits present.
- **[Retighten] settle → full re-tighten spam at idle 128³:** the KNOWN settle-loop class. The
  2026-07-16 guards (`note_cpu_grid_stale` SETTLE-LOOP GUARD + entry-less/brick-resident refusals,
  `body_volume_cache/mod.rs:833-859`) close the no-copy-completion-on-empty-entry feeders, and the
  comment there names the remaining signature exactly: "[RetightenArm] by=no_copy_completion while
  IDLE ... the exact feeder of a repeating '[Retighten] settle → full re-tighten' sequence". The
  owner's log shows that class resurfacing at dense 128³ — i.e. a bake completing after the settle
  heal still re-arms `cpu_grid_stale` for a body WITH an entry. REPORTED for a separate task
  (per instruction); breadcrumbs: check which `note_cpu_grid_stale` caller fires between settle
  heals (`check_idle_retighten` queue decisions log in `bake_dispatch.rs:252-278` — the `why`
  tags say loose/clipped/cpu_stale).

## E3.5 Files touched (evolution 3, uncommitted, this worktree)
- `core_engine/app/src/material_textures.rs` — COPY_SRC descriptor fn + regression test;
  slot-name registry; `map_display_name`; comment fixes.
- `core_engine/core_math/src/scene_graph.rs` — `body_prevailing_material` + graph test.
- `core_engine/app/src/creation.rs` — `inherited_material_for_create` + wiring in
  `add_primitive_with_op`.
- `core_engine/app/src/create_tool.rs` — inheritance at instant/click/drag create (3 sites).
- `core_engine/app/src/bool_cut.rs` — cutter inherits the body material.
- `core_engine/app/src/outliner_draw_material.rs` — caret + map-channel child rows.
- `core_engine/app/src/outliner_panel.rs` — `mat_clear_map` deferred action + processing.
- `core_engine/app/src/material_assign.rs` — `clear_material_map_channel`.
- `core_engine/app/src/properties_material_node.rs` — channel focus highlight/scroll; load-site
  names.
- `core_engine/app/src/material_editor.rs`, `material_import.rs` (worker channel carries the
  name), `app_poll.rs` (upload_map call sites), `main.rs` (rx type doc/signature) — name
  threading.
- `core_engine/app_types/src/types_selection.rs` — `expanded_materials`, `focus_map_channel`.
- `content_library.rs` — **ZERO lines touched** (owner's uncommitted edits; stall reported, not
  fixed).

## E3.6 Gates + owner manual test (evolution 3)
Gates: `cargo build -p app` clean · `cargo test -p core_math` 566 (+`create_prim_inherits_…`) ·
`cargo test -p app --bins -- --test-threads=1` green (230 + `descriptor_usages_support_grow_copy`;
goldens / pick / materials round-trip untouched). No shader or graphics WGSL touched.
**Owner-verified only (UI):** caret + child rows' look/feel, child click → editor focus highlight
+ scroll, Clear Map from the child context menu, "new cylinder blends into the textured body"
visual, absence of the startup GPU validation error in the log, library open feel.
Manual script:
1. Load the project whose startup log showed the validation error (≥4 maps across materials) →
   no `[GPU] UNCAPTURED ERROR` lines; textures render.
2. Textured body (e.g. dropped "Bricks") → Shift+A cylinder inside it → the cylinder appears
   with the SAME material instantly; Ctrl+Z removes it. BoolCut into the body → the cut faces
   show the body texture, not gray.
3. Outliner → Materials → caret on the textured material → children like
   "Bricks090_1K-JPG_Color — Diffuse Color". Click the Roughness child → Properties highlights +
   scrolls to the Roughness slot. Right-click a child → Clear Map → relief/roughness disappears;
   Ctrl+Z restores. After save+reload, children read "Bricks — Roughness" (names are
   runtime-only — expected).
4. Library open: first open may still hitch (reported scan/matcap stall — separate task);
   scrolling stays smooth (thumbs stream in async).

---

# EVOLUTION 4 (2026-07-18b) — Map thumbnails, per-map projection, crash audit

## E4.1 Owner ask 1 — "I need to see ALL images here too" (thumbnails everywhere)
- **New `MapThumbCache`** (`material_textures.rs`, mirrors `MaterialPreviewCache`): egui-texture
  cache keyed `(slot, data-decode)`, invalidated by the texture array's **generation counter**
  (covers both grow and the scene-load reset — a stale thumb can never show another scene's
  image). Lives in `CacheState.map_thumb_cache`; `AzraApp::map_slot_thumb()` is the one accessor.
- **Build cost is O(THUMB_RES²), never O(MAP_RES²)** (the stall lesson): a 64² thumb is built by
  STRIDE box-sampling the retained `slot_pixels` (2×2 taps per output pixel ≈ 16k reads — µs on a
  cache miss, one HashMap hit after). No worker needed because the source pixels are already
  decoded in RAM (`slot_pixels` persists for save) — only file DECODE was ever the stall.
- **Data maps decode back for display**: normal/roughness/metallic slots store bytes pre-ENCODED
  to sRGB (E2 trade); thumbnails run the inverse LUT so they look like the source image, not a
  brightened version. Unit-tested (`slot_thumb_stride_sample_and_data_decode` — LUT round-trip).
- **Editor** (`properties_material_node.rs` + the shared `draw_pbr_material`): every loaded slot
  (albedo + N/R/M) shows a **48px clickable thumbnail** (click = replace via the existing load
  flow); text fallback when a slot has no backing pixels (`slot_rgba` gate — see E4.3). Unset
  slots keep the Load… button.
- **Outliner shader-tree children**: the 🖼 icon is now a **14px thumbnail** (same cache — one
  build serves both), generic icon fallback.

## E4.2 Owner ask 2 — per-map projection (default triplanar)
- **Discovery: the stack below the UI was ALREADY per-map.** `TexMap.projection` is per-channel
  (albedo's rides the blob; N/R/M ride FL4MATX2 — each record has always carried its own
  `projection` field); `RasterMaterial` packs each map's projection in its own vec4 `.w` lane;
  `gbuffer.wgsl` reads each channel's own code. Only the UI wrote all four uniformly. So:
  **zero shader / RasterMaterial / save-format changes; NO FL4MATX3 needed.** Yesterday's
  FL4MATX2 files load per-channel intact (uniform codes → identical look), pinned by the
  strengthened `dc_gate0_materials_assignment_roundtrip` (now authors per-channel-DISTINCT
  projections and asserts channel-exact reattach).
- **UI**: per-slot **Projection combo** on albedo (shared widget, replaces the old
  "UV projection" checkbox — same semantics, consistent control) and on Roughness/Metallic rows
  (`draw_projection_combo` — the ONE combo impl). **Normal shows a static "Box (triplanar)"
  note** — the shader ignores its projection code (tangentless normal mapping; dead combo would
  lie). The old per-material combo became an **"All maps:" set-all convenience** showing "Mixed"
  on disagreement. Box (triplanar) stays the default (`TexMap::default`).
- **UV honesty gating** (chosen option: DISABLE with explanation, not silent fallback): the UV
  option is enabled only where UVs exist — graph materials: only when the material has polygon
  FACE users (`material_use_counts.face_refs > 0`); embedded SDF prim editor: never; embedded
  poly body editor: always. Hover text explains why. A material already saved with UV keeps it
  and can always switch back to Box.
- **Tiling stays per-material** (owner didn't ask; per-map tiling = open question for the owner —
  the plumbing would be identical to projection: `uv_scale` is already per-record on disk).
- **PT unchanged**: still scalar-only (no map sampling) — noted, out of scope.

## E4.3 Crash audit (owner "app crash", no log — E2/E3 surfaces)
Audited: editor draw/write-back, outliner child rows + deferred actions, Clear Map, map load
paths (file/AI/import worker), `poll_material_imports`, `material_map_loads` drain +
`upload_map` grow, scene-load reset/restore, drag-drop targeting (`pending_material_drop`),
FL4MATX2/FL4MATTX load. **Verdict: no panic reachable by a normal click sequence was found in
the committed paths** — every editor/outliner path pattern-matches or `get()`s before use;
deferred actions re-validate their `NodeId`s inside transactions; drops on deleted bodies toast.
Findings + dispositions:
1. **FIXED (real, silent data corruption): FL4MATTX restore slot-shift** (`project_load.rs`).
   The restore `filter_map`-dropped failed PNG decodes, shifting EVERY later upload one slot
   left — all subsequent materials silently re-skinned with the wrong images (tex_ids index by
   position). Also a corrupt record with a huge `slot` could have driven an unbounded alloc.
   Fix: slot-indexed placement, white placeholder preserves alignment on any decode failure,
   slots clamped to 512 with a log line (user-generated input hardening).
2. **Pre-existing build break at base (fixed to unblock gates):** commit d926541 changed
   `poly_knife_confirm(commit_hover: bool)` but missed its two callers (`input_keyboard.rs:684`
   Enter path → `true`, `input_viewport.rs:1849` RMB path → `false` per the fn's own doc).
   The worktree base did not compile — this, not a materials path, may be the owner's "crash"
   if they pulled a mid-refactor state.
3. **Not-a-hazard verdicts** (checked, left alone): `upload_map`'s `debug_assert_eq!` lockstep
   (slot_pixels/slot_names/used_layers mutate only together, poll-serial); editor snapshot
   write-back vs same-frame outliner Clear (both are single user clicks — cannot co-occur in one
   frame; poll-phase tex_id write-backs land BEFORE panels clone their snapshot); grow-path
   re-entrancy (poll-serial, generation counters invalidate consumers same-frame);
   `pending_material_drop` staleness (already cleared when no pick is in flight);
   dangling `tex_id` after reset (shader samples a zeroed layer; UI now gates via `slot_rgba`).
4. **Known non-crash liability (pre-existing, unchanged):** `rfd::FileDialog::pick_file()` runs
   modally on the UI thread mid-frame in every map-load path (albedo since E1). On some Windows
   setups modal dialogs inside the egui frame are a re-entrancy crash class (BorrowMutError).
   If the owner's crash log (when it recurs) shows a borrow panic in eframe around a file
   dialog, THIS is the suspect — fix would be deferring the dialog outside the draw (flag +
   poll), a separate task touching 5 call sites.

## E4.4 Files touched (evolution 4, uncommitted, this worktree)
- `core_engine/app/src/material_textures.rs` — `slot_rgba` gate, inverse sRGB LUT,
  `build_slot_thumb` (stride sampler), `MapThumbCache`, thumb unit test.
- `core_engine/app/src/cache_state.rs` — `map_thumb_cache` field + init.
- `core_engine/app/src/material_editor.rs` — `draw_pbr_material(.., albedo_thumb, uv_allowed)`,
  albedo thumbnail button, `draw_projection_combo` (shared), `AzraApp::map_slot_thumb`.
- `core_engine/app/src/properties_material_node.rs` — slot thumbnails + replace-by-click,
  per-slot projection combos, "All maps" set-all with Mixed state, UV gating; header comment
  updated (stale "disabled slots").
- `core_engine/app/src/properties_sdf_material.rs` / `properties_poly.rs` — widget call sites
  (thumb + uv_allowed: SDF=false, poly=true).
- `core_engine/app/src/outliner_draw_material.rs` — child rows carry the slot; tiny thumbnails.
- `core_engine/app/src/project_load.rs` — restore slot-shift fix (E4.3 #1).
- `core_engine/app/src/dc_baseline.rs` — round-trip authors per-channel-distinct projections.
- `core_engine/app/src/input_keyboard.rs`, `input_viewport.rs` — base-breakage repair (E4.3 #2).
- `content_library.rs` — **ZERO lines touched** (owner's uncommitted edits preserved).

## E4.5 Gates
`cargo build -p app` clean · `cargo test -p app --bins -- --test-threads=1` **232 passed**
(231 + `slot_thumb_stride_sample_and_data_decode`; goldens / pick / FL4MATX2 coexist+compat /
materials round-trip all green, round-trip strengthened in place). No WGSL, no `RasterMaterial`,
no `core_math` types touched → render/graphics/core_math gates not applicable (nothing in their
domains changed).

## E4.6 Owner manual test (evolution 4)
1. Select a multi-map material → every loaded slot shows its actual image (48px); the albedo
   row too. Click a thumbnail → file picker replaces that map. Outliner → expand the material →
   child rows show tiny thumbnails of the same images.
2. On a material assigned to a POLY body: set only the Roughness map's projection to UV → the
   roughness pattern follows the mesh UVs while albedo stays box-projected (only that channel
   changes in the viewport). On an SDF-only material the UV option is greyed with an explanation.
3. "All maps: Box" resets every channel; with mixed projections the combo reads "Mixed".
4. Load yesterday's save (FL4MATX2) → maps, per-channel projections, tiling all intact.
5. Ctrl+Z steps back each projection/strength/clear edit individually.
6. Knife still confirms on Enter (commits the hover point) and on RMB (does not).

---

## 7. Session log
- **2026-07-18c** — (IBL slice, full doc: RENDERER_IBL.md) §E4.3 finding #4 is RESOLVED: the
  rfd modal-in-frame crash class is fixed structurally — all 5 map-load sites + the HDRI load
  now go through the deferred `file_dialog.rs` service (picker + decode on a worker, results
  consumed in poll). The ambientCG metal/roughness materials now have a real environment to
  reflect (split-sum IBL: GGX-prefiltered specular + SH9 diffuse + BRDF LUT).
- **2026-07-18b** — Evolution 4 (this doc §E4): map thumbnails everywhere (MapThumbCache,
  generation-keyed, O(thumb) stride sampling of the retained slot pixels; editor 48px
  click-to-replace + outliner 14px children), per-map projection surfaced in the UI (the
  GPU/persistence stack was already per-channel — zero shader/format change; per-slot combos +
  "All maps" set-all + honest UV gating; normal maps stay triplanar-only by design), crash
  audit: FL4MATTX restore slot-shift FIXED (silent wrong-image corruption + unbounded-alloc
  hardening), base build break (poly_knife_confirm callers) repaired, no reachable panic found
  in committed E2/E3 paths; rfd-modal-in-frame flagged as the likely suspect if the owner's
  crash recurs with a borrow panic. Gates: app 232 green, build clean.
- **2026-07-18** — Evolution 3 (this doc §E3): P1 `material_tex_array` COPY_SRC fix (grow-copy
  source usage, descriptor unit-tested), new prims inherit the body's prevailing material
  (`body_prevailing_material` last-prim rule + all fresh-create sites incl. the BoolCut cutter),
  Modo-shader-tree expandable material rows in the outliner (map-channel children w/ runtime
  slot names, click→editor channel focus, Clear Map context action), stall verdict REPORTED
  (thumbnails already async; sync scan+matcap decode on library open + the resurfaced idle
  Retighten settle loop → separate tasks). Gates: core_math 566, app 231, build clean.
- **2026-07-17b** — Evolution 2 (this doc §E2): explicit Add-Material outliner (no auto default
  node — CLAUDE.md line now stale, flagged), multi-map materials (normal/roughness/metallic as
  serde-skip TexMaps + FL4MATX2 tail; pixels ride existing FL4MATTX), body-local BOX projection +
  whiteout triplanar normal mapping in gbuffer.wgsl (GPU-probe-tested), live editor map slots +
  projection combo, ambientCG library drag-drop → one-transaction graph-material import with
  worker-thread decode. Gates: app 230 / core_math 565 / render 33 / graphics green.
- **2026-07-17** — Materials UX overhaul (this doc): outliner Materials section
  (create/rename/duplicate/delete with reassign-to-default guard), always-visible assignment
  combos (prim / SDF body / poly body) with "+ New Material" + jump-to-editor, material-node
  Properties editor with albedo map + future-shaped disabled slots, poly face-material display
  fix (raster + PT), phantom Material Editor panel removed, dup Node Editor button removed.
  Gates: `cargo build -p app` clean; app --bins 227 green (single-threaded); core_math 563 green;
  Gate-0 goldens untouched. Materials never move geometry — no bake paths touched.
