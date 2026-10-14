# UNWIRED ACTIONS — v1 engine features with no ui2 (v2 shell) surface

**Purpose (owner contract):** every feature/action that EXISTS in the engine and is reachable
in the v1 shell (menus, shortcuts, legacy panels, popups) but that a user running the v2 shell
(`AZRA_UI=v2`, `Run_UIv2.bat`) **cannot reach through v2 chrome**. The owner works through this
list to place features into the new UI gradually. Nothing on this list may be silently dropped.

Generated 2026-07-24 on `feat/ui-polish` (after master merge `abebd3b`). Method: diff of
- v1 surfaces: `menus.rs` (menu bar / add popup / viewport ctx menu / Q menu / merge menu /
  shape-swap), `ACTION_DEFS` (`app_types/src/keybindings.rs`), `viewport_header.rs`,
  `tool_settings.rs:draw_left_tool_strip`, `outliner_draw.rs:draw_right_panel` (v1 properties
  stack), `win_*` windows, `ui_panels.rs`, AI panels, pie menus
- v2 surfaces: `ui2/menus.rs`, `ui2/tools/catalog.rs` + `activate.rs`, `ui2/outliner/*`
  (quick-bar + context menu), `properties_panel.rs` (Gate-5 pages), `ui2/shell/*`
  (app bar / ctxbar / shading / status), `ui2/draw/pies.rs`, `menu_shared.rs` fillers,
  and the legacy windows still drawn unconditionally in `app_update.rs:230-258`.

**Reading the columns**
- *Access in v2 today*: `chord` = keyboard still works (handle_keyboard runs in both shells);
  `palette` = findable in the Command Palette (v2 app-bar Search / Ctrl+Shift+P);
  `none` = truly unreachable in v2; `v1-only` = only the v1 shell can reach it.
- **DEAD-KEY warning:** several chords open popups whose *draw call is v1-gated*
  (`app_update.rs:630-640`) — the key fires, state is set, nothing appears. Those rows say
  **chord=dead**. They are worse than unwired; treat as P1 when placing.
- `[ ]` = not yet placed in v2. Check it off when the ui2 surface ships.

Legend for suggested placement: `menu` (app-bar), `palette` (tools panel), `ctxbar`,
`props` (Properties page), `pie`, `outliner`, `viewport-ctx` (a future v2 right-click menu).

---

## 1. File

Mostly wired: New/Open/Save/Save-As, default-scene pair, Import (OBJ→SDF, glTF, JSON),
Export (OBJ window, glTF, JSON), Recent Files, Quit — all in the v2 File menu via
`menu_shared.rs` fillers. Remaining gaps:

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [ ] | Export shortcut opens OBJ window | `win_export.rs:draw_export_window` (ACT_EXPORT) | chord `Ctrl+E` (works; window drawn in both) | already fine — verify menu label shows chord |

*(Nothing else unwired in File.)*

## 2. Edit / Selection / History

Wired in v2: Undo/Redo/Delete/Duplicate/Select-All/Deselect/Shortcuts… (Edit menu),
Macro submenu + Macro Manager, Experimental…, Select menu (poly grow/shrink/invert/linked/
edge loop/ring), Command Palette (Search button + `Ctrl+Shift+P`).

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [x] | Copy / Cut / Paste prims | ACT_COPY/CUT/PASTE → `input_keyboard.rs` | **v2 Edit menu** (copy_selection/cut_selection/paste) + chords | ✅ Edit menu rows |
| [ ] | Hide Selected / Unhide All (object level) | ACT_HIDE / ACT_UNHIDE_ALL | chord `H` / `Alt+H` only | Edit or Item menu — NOT surfaced: dispatch is an inline block in `input_keyboard.rs:329-380` with no shared fn; would require reimplementing (No-Hacks). Needs an extracted `hide_selected_objects()` first. |
| [x] | Group / Object (kitbash group) | `ops::edit::group_layer::run` (ACT_LAYER) | **v2 Object menu** (group_selected_bodies_or_prims, ACT_LAYER hint) + chord + outliner ctx | ✅ Object menu |
| [ ] | Create Layer (use Group) | ACT_CREATE_LAYER | folded into Group row (Object menu) | ✅ same as Group |
| [ ] | Rename (F2 outside outliner) | ACT_RENAME | chord `F2`; outliner ctx has Rename | ok — verify F2 works over viewport in v2 |
| [ ] | Undo History list (undo/redo buttons + depth display) | `properties_viewport.rs:draw_props_viewport` (bottom) | none (v1 right panel only) | status bar or History panel tab (`PanelKind::History` is an empty stub) |
| [ ] | Q "Operations" context menu | `menus.rs:draw_q_menu` (ACT_Q_MENU) | **chord `Q` = dead** (draw is v1-gated, `app_update.rs:635`) | v2 viewport-ctx or rebuilt Q pie — its ops now live in Object/Mesh menus, but the Q pie itself is a separate task |
| [ ] | Toggle Visible-Only Selection | ACT_SELECT_VISIBLE_ONLY → viewport header toggle | chord `Alt+Shift+V`; v1 header button | ctxbar (next to Snap) |

## 3. Add (creation)

Wired in v2: Geometry menu (6 SDF prims, Stair, Spiral Stair, Polyline, Draw Polyline),
Shift+A add popup (drawn in v2 — SDF prims, poly meshes, NURBS, voxel objects, lights),
`Ctrl+Shift+A` SDF-in-sub-body popup, tools palette Create rows, outliner + Add
(SDF Body / lights / Camera), Item menu lights.

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [x] | **Spine / Pipe** (3D path tube) | `ops::sdf::spine::run` | **v2 Add → Curve → Spine / Pipe** + Shift+A popup | ✅ Add menu |
| [x] | Add **Material** | `material_assign.rs:186:create_material_interactive` | **v2 Add → Material** + Materials → Add Material | ✅ Add + Materials menus |
| [x] | Add Voxel Sphere / Voxel Box (buttons) | `input_sculpt.rs:354:new_voxel_sphere` / `new_voxel_box` | **v2 Add → Voxel Sculpt** + Shift+A popup | ✅ Add menu (palette still tracked separately) |
| [x] | NURBS create (Plane/Cyl/Sphere/Torus/Revolve/Loft buttons) | `creation.rs:471:create_nurbs_surface` | **v2 Add → NURBS Surface** (all 6) + Shift+A popup | ✅ Add menu (palette Create still tracked separately) |
| [x] | Add Camera from Add-surface (menu parity) | `add_camera` (ui2 outliner has it) | **v2 Add → Camera** + outliner | ✅ Add menu |
| [ ] | Empty-space viewport "Add Primitive" ctx submenu | `menus.rs:draw_context_menu` (988) | **none** (ctx menu v1-gated); the primitives themselves are in the v2 Add menu | v2 viewport-ctx (the right-click surface itself is a separate task) |

## 4. SDF tools & prim operations

Wired in v2: palette Shapes/Boolean(set-op)/Transform-arm/Slice, boolean pies (`X`),
resolution pie (`Ctrl+R`), Tab mode pie, D pivot pie + pivot-center dropdown, F isolate pie,
outliner ctx (Boolean op, Change Shape, Add Modifier, Group into sub-body, Save as Part,
Convert to Mesh, Enter Scope, Dissolve), Item menu (Isolate, Poly→SDF convert/link),
SDF props page (Resolution / Visibility / Adaptive), Tool Params pane (shape params,
bevel, boolean of active prim — `tool_params.rs`).

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| ~ | Viewport RMB context menu on prim (quick sliders, boolean, mirror, change-shape, reset-xform, hide/lock, instance, group, delete) | `menus.rs:590:draw_context_menu` | **partial in v2 menu bar**: boolean/reset-xform/instance/group/delete/save-to-library now in **Object menu**; quick-sliders + change-shape + per-prim mirror/hide/lock still only in the v1-gated draw | remaining bits → v2 viewport-ctx (separate task) |
| [ ] | Shape-swap pie (Shift+RMB on prim) | `menus.rs:507:draw_shape_swap_pie` | **none** (v1-gated) | v2 pie or viewport-ctx submenu |
| [ ] | Per-prim Mirror X/Y/Z + Flip Mirror | ctx menu → `symmetry.rs:87:flip_body_mirror` | none (outliner ctx lacks mirror) | outliner ctx + props SDF page |
| [x] | Reset Transform (pos/rot/scale/all) + Reset pie | ctx menu → `reset_position` etc., `pie_menu::reset_pie_def` | **v2 Object → Reset Transform submenu** (Position/Rotation/Scale/All) + props Object page | ✅ Object menu (the *pie* opener stays a separate task) |
| [x] | Create Instance | `ops::instance::create::run` (ACT_INSTANCE) | **v2 Object menu** (ACT_INSTANCE hint) + chord + palette | ✅ Object menu |
| [x] | Instance Into All Bodies | `instance_actions.rs:180:instance_into_all_bodies` | **v2 Object menu** + palette | ✅ Object menu |
| [x] | Make Unique / Break Instance Link | `make_instance_unique` / `make_selected_instances_unique` | **v2 Object menu** (make_selected_instances_unique) + palette | ✅ Object menu |
| [x] | Stack → SDF (merge down) | `stack_sdf.rs:100:stack_to_sdf` | **v2 Object menu → Stack → SDF (merge down)** | ✅ Object menu |
| [x] | Save Body to Library… (from viewport) | `body_library.rs:48:open_library_save_for_selection` | **v2 Object menu → Save Body to Library** + outliner ctx | ✅ Object menu |
| [ ] | SDF shape full properties (per-shape params, corner/rim/shell of CurveExtrude, HalfSpace, per-edge bevel "Advanced", chamfer angles) | `properties_sdf_shape.rs` ("🔷 Shape", "Advanced (per-edge)") | partial via Tool Params pane; the rest none | props SDF page — port shape section |
| [ ] | Boolean panel (op + blend K + Protect Walls) | `properties_sdf_shape.rs` ("⚙ Boolean", Protect Walls at :1059) | boolean set via pies/outliner; blend-K/Protect-Walls none | props SDF page |
| [ ] | Multi-prim Blend K slider | `properties.rs:29` (multi_blend block) | none | props SDF page (multi-selection state) |
| [ ] | Prim Modifiers stack (Twist/Taper/Shear/Displace/Smooth/Spherize/Shell/Emboss…) — edit/reorder/remove | `properties_sdf_shape.rs` ("🪞 Modifiers"), `ui_panels.rs:ADD_MODIFIER_TYPES` | ADD via outliner ctx "Add Modifier"; **no edit/remove UI in v2** | props Modifiers page (replace stub) |
| [ ] | Field Modifiers (volume) + Mesh Modifiers (post-DC) body stacks | `properties_sdf.rs` ("Field Modifiers (volume)", "Mesh Modifiers (post-DC)") | none | props Modifiers page |
| [ ] | Body Mirror (body-level symmetry flags) | `properties_sdf.rs` ("Body Mirror") | Alt+X symmetry pie chord only | props SDF page + ctxbar sym toggle |
| [ ] | Body properties (origin recalc, bake res per body, body info) | `properties_sdf.rs:669:draw_props_body` | partial (props SDF page has resolution); rest none | props SDF page |
| [ ] | SDF Transform panel (numeric, elongation) | `properties_sdf_transform.rs` ("📐 Transform") | props Object page covers TRS; elongation none (chord `E`) | props Object page row |
| [ ] | Clip Curve tool | `clip_curve_tool.rs` (ACT_CLIP_CURVE) | chord `C` (SDF ctx); catalog `tw("Clip Curve")` tracked | palette Modify (already-tracked not-ready) |
| [ ] | Smart Mirror | ACT_SMART_MIRROR → symmetry pie | chord `Alt+X`; catalog `tw` tracked | palette Symmetry |
| [ ] | Bool Cut (surface draw cutter) | `bool_cut.rs` (v1 left strip `ToolBoolCut`) | **none** | SDF palette Boolean row |
| [ ] | Extract Intersection tool settings | `extract_intersection.rs:draw_extract_intersection_settings` | none (v1 tool-properties panel) | Tool Params pane when tool active |
| [ ] | Linear Array tool settings | `array_dup.rs` / `draw_linear_array_tool_settings` | none (v1 tool-properties panel) | Tool Params pane; catalog `cw("Radial Array")` tracked |
| [ ] | Create-tool settings (footprint create opts) | `create_tool.rs:draw_create_tool_settings` | none in v2 (creation itself works) | Tool Params pane |
| [ ] | SDF Sculpt Brush + Sculpt Layers (legacy SDF sculpt) | `properties_sdf_shape.rs` ("🎨 SDF Sculpt Brush", "🖼 Sculpt Layers") | none — **check LEGACY_SCULPT_REMOVAL_PLAN before placing; may be scheduled for deletion** | probably do NOT place; confirm with owner |
| — | ~~French Curve cutter (45/75/90 convex/concave, rim)~~ | removed; only `types_workspace.rs:51:ShapeFrenchCurve` JSON slot remains | — | **DEAD — do not place (owner)** |
| — | ~~Soft-cut tool set~~ (from commit 56c6206; Protect-Walls slider is the only survivor, tracked above) | removed | — | **DEAD — do not place (owner)** |

## 5. Poly tools

Wired in v2: palette Create/Vertex/Edge/Face/Select/Duplicate ready rows (`activate.rs`),
Geometry menu poly section (mode-dependent), Select menu, Tab/mode pie, poly boolean pie,
outliner Convert-to-Editable-Poly, Item menu Poly→SDF. Solid chord coverage via ACTION_DEFS.

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| ~ | Poly Q menu (per-mode op list incl. To Sphere, Circle, Poke, Solidify, Extrude-along-normals, Delete-only-faces) | `menus.rs:381:draw_q_menu` | **chord `Q` = dead** in v2; core ops (Extrude/Inset/Bevel/Loop Cut/Knife/Bridge/Solidify/Subdivide/Merge/Dissolve/Delete elements/Fill/Flip+Recalc normals/Grow+Shrink/Hide) now in **v2 Mesh menu**. Not-yet-surfaced: To Sphere, Circle, Poke, Extrude-along-normals, Delete-only-faces | remaining ops → Mesh menu or v2 viewport-ctx |
| [ ] | Poly edit RMB ctx menu (+ Selection Shape radio Box/Circle/Lasso for region select) | `menus.rs:686` (poly branch of draw_context_menu) | **none** — Selection Shape switch unreachable in v2 (catalog rows `tw` tracked) | v2 viewport-ctx |
| [ ] | Merge menu At Center / At Last / By Distance | `menus.rs:349:draw_merge_menu` (ACT_POLY_MERGE_MENU, `poly/key_dispatch.rs:78`) | **chord `Alt+M` = dead** (draw v1-gated); `M` merge-at-center works; palette Merge/Merge-by-Distance work | rebuild as v2 popup or pie |
| [x] | Knife | `poly_knife` (ACT_POLY_KNIFE) | **v2 Mesh menu → Knife** (AppCommand::PolyKnife) + chord | ✅ Mesh menu (palette Edge still tracked) |
| [ ] | Vertex / Edge slide | ACT_POLY_VERT_SLIDE / ACT_POLY_EDGE_SLIDE | chords `Shift+V` / `Shift+E`; catalog `tw` tracked | palette rows |
| [ ] | Shrink/Fatten (Alt+S), Proportional edit (O / Shift+O) | ACT_POLY_SHRINK_FATTEN, ACT_POLY_PROPORTIONAL | chords only | ctxbar toggle (proportional) + palette |
| [ ] | Rip / Rip Fill | ACT_POLY_RIP / ACT_POLY_RIP_FILL | chords `V` / `Alt+V`; catalog `tw("Rip")` tracked | palette Vertex |
| [ ] | To Sphere, Circle (round region/loop), Poke, Spin | `poly/tosphere.rs:57`, `poly/circle.rs:135`, `poly_poke_faces`, ACT_POLY_SPIN | chords (`Alt+C`, `Ctrl+Alt+R`); Q-menu dead; catalog Poke/Spin `tw` tracked | palette Face + viewport-ctx |
| [ ] | LoopTools Relax / Flatten / Space | ACT_POLY_LOOP_RELAX/FLATTEN/SPACE | chords `Ctrl+Alt+L`, `Ctrl+Shift+F`, `Ctrl+Alt+S` only | palette Edge cmds |
| [ ] | Grid Fill | ACT_POLY_GRID_FILL | chord `Ctrl+Alt+F` only | palette Face cmds |
| [ ] | Limited Dissolve | ACT_POLY_LIMITED_DISSOLVE | chord `Ctrl+Shift+X` only | palette cmds |
| [ ] | Separate Selection | ACT_POLY_SEPARATE | chord `P`; catalog `cw("Separate")` tracked | palette Create cmds |
| [ ] | Bisect / Bisect Clear Inner | ACT_POLY_BISECT / _CLEAR | chords `Ctrl+Shift+B` / `Ctrl+Shift+Alt+B` only | palette Edge cmds |
| [ ] | Select Similar | ACT_POLY_SELECT_SIMILAR | chord `Shift+G` only | Select menu |
| [ ] | Symmetrize / Delete Half / Radial symmetry | ACT_SYM_SYMMETRIZE (`Ctrl+Shift+K`), ACT_SYM_DELETE_HALF, ACT_SYM_RADIAL_CYCLE + Alt+X pie | chords + pie; catalog `cw` tracked | palette Duplicate cmds |
| [ ] | Poly modifiers stack UI (Mirror/Array/Solidify/Subdiv — view/edit/remove) | `properties_poly.rs` ("🔧 Modifiers"; NOTE `mesh_sync.rs:82` silently drops 5 of 8 modifier types — pre-existing bug) | none | props Modifiers page (replace stub) |
| [ ] | Poly material assign + Mesh Info + Bevel Preview | `properties_poly.rs` | none | props Material page + status bar info |
| [ ] | Poly adjust-last-op panels (create/inset/circle/tosphere) | `draw_poly_create_adjust` etc. (app_update, drawn in both) | wired (floating windows appear in v2) — verify styling | keep; restyle later |
| [ ] | Bevel clamp-overlap toggle during bevel | ACT_BEVEL_TOGGLE_CLAMP | chord `C` during bevel | status-bar hint chip |

## 6. Sculpt (voxel)

v2 has: Sculpt workspace tab + palette (arms WorkMode only; every brush except Draw is
`tw` not-ready — already-tracked), brush size chords, remesh chord.

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [ ] | **Voxel sculpt panel** (brush type/radius/strength, falloff, symmetry, resolution, remesh button — the whole sculpt cockpit) | `ui_panels.rs:463:draw_voxel_sculpt_panel` | **none** (draw v1-gated `app_update.rs:639`); only chords `Z`/`X` size, `Ctrl+R` remesh, Enter voxelize | Tool Params pane when pillar=sculpt (real brush binding) |
| [ ] | Brush selection (Draw/Erase/Clay/Smooth/Grab/… real `BrushType`) | `voxel_sculpt.rs` + `input_sculpt.rs` (see memory: tool_activation_map) | palette rows exist but `tw` not-ready (already-tracked) | wire palette rows → BrushType |
| [ ] | Convert to Clay (mesh → voxel sculpt) | v1 left strip `ConvertToClay` (`tool_settings.rs:draw_left_tool_strip`) | none | Sculpt palette cmd |
| [ ] | Remesh / resolution dialog | ACT_SCULPT_REMESH (`Ctrl+R` in sculpt) | chord only | Sculpt palette cmd + props |
| [ ] | Voxelize pending moves | ACT_VOXEL_VOXELIZE_NOW (Enter) | chord only | status chip while pending |
| [ ] | Mask ops (invert/clear/fill/hide) | catalog Mask rows all `cw` | already-tracked not-ready | palette |

## 7. NURBS

v2 has: NURBS palette (ALL rows `tw`/`cw` not-ready — already-tracked), Tab pie NURBS modes,
Shift+A NURBS creation.

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [ ] | Insert Knot U/V, CV weight ±, Grab CVs | ACT_NURBS_* (`nurbs edit ops`) | chords `U`/`V`/`W`/`Shift+W`/`G`; palette rows not-ready (tracked) | wire palette Edit CV rows |
| [ ] | Loft / Revolve tools | v1 left strip `NurbsLoft`/`NurbsRevolve` → `create_nurbs_surface("loft"/"revolve")` | Shift+A only | NURBS palette Surface rows |
| [ ] | NURBS properties (Surface Info, CV Weight, Transform, Material) | `properties_nurbs.rs` | none | props pages |

## 8. Materials & Nodes

**Materials are effectively unreachable in the v2 shell** — biggest single gap.

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [x] | Create Material | `material_assign.rs:186:create_material_interactive` | **v2 Materials → Add Material** + Add → Material | ✅ Materials + Add menus |
| [x] | Assign material popup (Ctrl+M quick-assign with search/create) | `material_assign.rs:12:draw_mat_assign_popup` | **v2 Materials → Assign to Selection** (submenu lists materials → `assign_material_to_selection`, incl. Remove/embedded). The `Ctrl+M` search/create *popup* remains v1-gated, but the assign action itself is now reachable | ✅ Materials menu (search/create popup itself = separate v2-popup task) |
| [ ] | Material node editor (PBR params, maps, preview) | `properties_material_node.rs:draw_props_material_node` | none (v1 right panel when material node selected) | props Material page (replace stub) |
| [ ] | Per-prim embedded material editor + presets + DNA (procedural layers, age, rules) | `properties_sdf_material.rs` ("🎨 Material", "SDF Procedural Layers") | none | props Material page |
| [ ] | Shared-material-from-prim / jump-to-material | `properties.rs` PropsContext (`new_shared_mat_from_prim`, `jump_to_material`) | none | props Material page |
| [x] | Shader node editor window (custom shading graph) | `win_node_editor.rs:13:draw_node_editor_window` | **v2 Materials → Shader / Node Editor** + Window menu (sets `show_node_editor`; window draws in both shells) | ✅ Materials + Window menus (NodeEditor panel *tab* still a stub) |
| [ ] | Node-graph bake | `node_graph_bake.rs` (from node editor) | none (depends on above) | inside node editor |
| [ ] | Texture Generator window | `win_texture.rs:13:draw_texture_generator` | **wired** (Texture menu) | — |
| [ ] | Material preview cache/thumbnails in pickers | `mat_preview.rs` | follows the popups above | with mat UI |

## 9. Render / PT / Viewport display

Wired in v2: shading strip (4 modes + DC-wire RMB settings), Render menu (shading modes,
wire-on-shaded, PT Settings window), Z shading pie, View pie, split view (ctxbar View pill),
camera views/focus/reset (View menu + chords), ortho toggle chord.

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [ ] | Render section (viewport render mode Hybrid/…, quality presets Draft→Ultra, DoF, Interaction Quality, Volume Shading, per-mode Lit/MatCap/PBR/Raytrace settings) | `properties_render.rs:149:draw_props_render` | none (v1 right-panel bottom) | new props Render page or Render workspace right dock |
| [ ] | Light & Env section (env IBL, exposure/EV, background gradient, env samples) | `properties_render.rs:1276` ("💡 Light & Env") | none | props Render page |
| [ ] | Light properties (type params, cone, direction, ray-traced shadow, volumetric scattering, light linking) | `properties_light.rs:draw_props_light` | outliner ctx can change light TYPE only; rest none | props page when light selected |
| [ ] | Viewport header extras: X-Ray, Soft-select toggle, snap bar duplicates, grid/shadow-catcher toggles, DBG modes (Bricks/March/Steps), proportional-edit chip, hidden-count, REC chip | `viewport_header.rs:7:draw_viewport_header` | none (header suppressed in v2); snap subset lives in ctxbar Snap popover | ctxbar right cluster + status bar |
| [x] | Grid settings window | `win_grid_workplane.rs:draw_grid_settings` | **v2 View → Grid Settings** (`viewport.show_grid_settings`; window draws in both) + palette | ✅ View menu |
| [x] | Workplane settings window | `win_grid_workplane.rs:draw_workplane_settings` | **v2 View → Work Plane Settings** (`viewport.wp_show_settings`) + chord `Home` places WP | ✅ View menu |
| [ ] | Compositor (post-FX graph) | `properties_compositor.rs` ("🎬 Compositor") | none | props Render page / Shade workspace |
| [ ] | Profiler section (+ Perf HUD toggle) | `properties_profiler.rs` ("📊 Profiler") | none (Tweaks panel has some) | Windows+Dev / status bar fps click |
| [ ] | PT Settings window | `properties_render.rs:1405:draw_pt_settings_window` | **wired** (Render menu) | — |
| [ ] | Camera object properties (focal, DoF target…) | (camera node — v1 props) | none | props page when camera selected |

## 10. AI

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [x] | AI Beauty (path-tracer overlay) window | `ai_beauty.rs:1023` (+ viewport overlay draws in both) | **v2 Render → AI Beauty** (`ai.show_beauty_window`; window draws in both shells) | ✅ Render menu |
| [x] | Code Review (Claude) window | `win_ai.rs:105:draw_code_review` | **v2 Help → Code Review (Claude)** (`ai.show_code_review`) | ✅ Help menu |
| [ ] | Agent command bar (floating NL command input) | `ui_panels.rs:269:draw_agent_bar` | **none** (v1 panel-gated) | app-bar Search unified with palette, or status bar |
| [ ] | AI Texture (Stable Diffusion) window | `win_texture.rs:157:draw_ai_texture_window` | **wired** (Texture menu) | — |
| [ ] | Agent Settings window | `win_ai.rs:draw_agent_settings` | **wired** (Texture menu) | — |

## 11. Windows / Dev / Layout

Wired in v2: Developer Tweaks (View menu + app-bar Tweaks btn), Layout Settings, Panel List,
Reset Default Layout, Body Library (View menu), Stress Test (Help), Rebuild/Restart +
cache test scenes (Help menu), About, Shortcut editor (Edit → Shortcuts… + `End`),
Keybind quick-window (same row), Library band (View menu + grip menu float panels).

| ✓ | Feature | Engine entry point | Access in v2 today | Suggested v2 placement |
|---|---------|--------------------|--------------------|------------------------|
| [ ] | v1 Content Library browser (`Ctrl+Space`) | `content_library.rs:141:draw_content_library` (ACT_CONTENT_LIBRARY toggles `library.open` — **v1 flag; chord is a no-op in v2**) | v2 has its own library band (View menu) | re-bind `Ctrl+Space` to `ui2.library.open`; retire v1 browser |
| [ ] | Edit UI Layout mode (ZBrush-style panel edit) | v1 View menu → `panel_layout.ui_edit_mode` | none (v1 concept) | decide: retire or port to ui2 dock edit |
| [ ] | Workspace manager (custom toolbars, `~/.azra/workspaces.json`) | `workspace.rs` + `types_workspace.rs` | none (v1 left strip system) | superseded by ui2 palette — owner decision to retire |
| [ ] | Panel List window | `draw_panel_list` | **wired** (View menu) | — |
| [ ] | UV / Shade / Animate / Render workspace tabs | `ui2/shell/app_bar.rs` tabs | tabs exist, content is chrome-only | fill per workspace plan |
| [ ] | Panel tabs Image / History / UV Layout / Node Editor / Channels | `ui2/pane.rs:group_body` — "content coming" | placeholder panes | fill gradually |

---

## Honesty list — wired-looking v2 surfaces that are stubs or broken

These LOOK live in v2 but are not (do not tick a row above "done" by pointing at these):

1. **Properties → Modifiers page** — `properties_panel.rs:page_modifiers` renders a hardcoded
   mockup stack (Mirror/Array/Bevel-INACTIF/…); toggles write egui temp memory, never the scene.
2. **Properties → Material page** — `properties_panel.rs:page_material` — fake
   Roughness/Metalness sliders in temp memory; "No maps assigned" is hardcoded.
3. **Ctxbar Global / Bounding-Center pills** — `ui2/shell/ctxbar.rs:74-112` — popovers show
   "Current: …" info text only; you cannot actually switch transform space or pivot mode there
   (space switch is chord `T`; pivot via D pie).
4. **Ctxbar Snap "AFFECTS Move/Rotate/Scale" + poly "Edge perpendicular"/"Volume" checkboxes**
   — write `ui2.snap_*` fields; verify consumers before trusting (`snap_affect_*` are v2-only
   state, the transform code path reads the viewport smart-snap flags).
5. **App-bar Share button** — toasts "Share — coming soon".
6. **Workspace tabs UV / Shade / Animate / Render** — selection chrome only (`app_bar.rs:204`
   comment: "others stay chrome for now").
7. **Tools palette `tw()`/`cw()` rows** — dimmed by design (ready:false) — already tracked in
   catalog; the honest ones. `ui2_tool_command` falls back to a "not implemented" toast.
8. **v2 bottom tool-options bar** — `ui2/draw/viewport_split.rs:ui2_viewport_scene_chrome` —
   XYZ values are **read-only labels**, not editable fields (mockup parity pending).
9. **Dead chords in v2** (state set, surface never drawn — `app_update.rs:630-640` gate):
   `Q` (Q menu), `Alt+M` (merge menu), `Ctrl+M` (material assign), viewport RMB context menu,
   `Shift+RMB` shape-swap pie, `Ctrl+Space` (v1 content library flag).
10. **PanelKind Image/History/UvLayout/NodeEditor/Channels tabs** — "content coming" placeholder.
11. **Outliner funnel `.active` accent** — noted in code as unreachable until the
    filter-settings popover exists (`ui2/outliner/mod.rs:854`).

---

## How to keep this current

Re-run the diff **after every master merge into `feat/ui-polish`**:

1. `git log --oneline <last-audit>..HEAD -- core_engine/app/src/menus.rs
   core_engine/app/src/ui2 core_engine/app_types/src/keybindings.rs` — new v1 menu items,
   new ACT_* actions, new ui2 surfaces.
2. Check the v2 gate blocks in `app_update.rs` (`if ui_v2 { … } else { … }` around lines
   176/263/564/630) — anything added to the *else* branch is v1-only and belongs here.
3. Check `ui2/tools/catalog.rs` for `tw`/`cw` → `t`/`c` promotions (tick rows here) and for
   new not-ready rows (add as already-tracked).
4. Grep new `show_*` window flags: every new `self.ui.show_X = true` / `self.ai.show_X = true`
   needs a v2 opener or a row here.
5. Update the checkboxes; move newly-stubbed v2 surfaces into the Honesty list, and delete
   Honesty entries once they bind to real state.

Last audit: 2026-07-24 (`feat/ui-polish` @ post-merge of master `57c1b97`).

**Menu-bar reorg (2026-07-24, `feat/ui-polish`):** v2 app-bar menus rebuilt into
File · Edit · Add · Select · Object · Mesh · Materials · Render · View · Window · Help
(`ui2/menus.rs`, order in `ui2/shell/app_bar.rs`). Each item dispatches the SAME fn as
the v1 menu / keybinding action (ops::*, add_light, apply_sdf_bool_pie,
assign_material_to_selection, copy/cut/paste, etc.). A `menu_act()` helper adds a
right-aligned weak keyboard-chord hint (egui `Button::shortcut_text`, chord from
ACTION_DEFS). Rows above marked `[x]` / `~` were (fully / partly) surfaced by this pass.
Deliberately NOT surfaced: object-level Hide/Unhide (no shared fn — inline in
`input_keyboard.rs`, reachable by `H`/`Alt+H`); Select-Similar (dead keybind, no
dispatch fn); French Curve + soft-cut tools (owner: DEAD).
