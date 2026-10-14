# AZRA UI Port Plan — HTML Mockup → egui (UI-only, gated)

**Status:** planning · **Date:** 2026-07-17 · **Branch:** `feat/ui-polish` (never merge to master until approved — owner rule)
**Source of truth:** [`docs/UI/azra_app_mockup.html`](azra_app_mockup.html) (committed copy of the approved mockup, 6558 lines) + its live artifact `claude.ai/code/artifact/1c35c96b-…`. The mockup is authoritative for **both look and behaviour**.
**Pairs with:** `UI_REDESIGN_PLAN.md` (the *how/when* tracks A–F, problems R1–R10) and `UI_UX_DESIGN_RULES.md` (the *what it must look like*). This doc is the **concrete mockup→egui execution** of that vision. `ui_theme.rs` stays the code authority for token *values*.

---

## 0. Ground rules for this port

1. **UI-only. Do NOT wire to scene/tool/engine state yet.** Every panel reads from a single stub, `UiMockState` (a plain struct of placeholder data + an "active tool" selector). The real wiring — replacing the stub with scene-graph / tool / selection queries — is the **explicit next phase**, gated separately, after the UI is proven. Each panel's read is a one-function seam so the swap is mechanical.
2. **The mockup is the verification oracle, not just the spec.** For each gate, the egui result is compared side-by-side with the mockup, and the mockup's *measured* values (extractable from the browser: computed px, hex, spacing) are the acceptance numbers. Because the running `.exe` cannot be visually inspected from here (exe locked; see `feedback_verify_app_path_not_kernel`), each gate closes on **(a)** `cargo build -p app` green, **(b)** any structural self-checks possible, **(c)** owner side-by-side visual sign-off against the mockup. Keep the mockup open next to the app.
3. **No hard-coded colours/sizes/fonts.** Everything references `ui_theme::*` (CLAUDE.md + design rules). Adding a colour means adding a token first.
4. **Front-load Easy value, isolate the one Hard thing.** The feasibility pass found **9 of 13 interactions are native/idiom/Easy** in egui. The only genuinely Hard features are *drag-between-zones docking* (#5) and *viewport splitting* (#6). Those are quarantined to the **last** gate so every other panel ships first and the layout risk never blocks the content.
5. **Consolidate, don't add a fourth system.** The app already has THREE overlapping panel layers (see §2). No gate adds a new one; the port picks ONE and retires the others (this is redesign-plan R1/Track B).

---

## 1. What already exists on `feat/ui-polish` (the head start)

The inventory workflow surveyed the tree. Key findings that shrink the work:

| Area | State on `feat/ui-polish` | Consequence |
|---|---|---|
| **Theme tokens** | `ui_theme.rs` exists: full surface ladder, accents, radii, font sizes, `accent_tint()`. | Gate 0 is *reconcile + verify*, not *build a theme system*. |
| **Token application** | `ui_theme::apply_style(&mut style)` called every frame (`app_update.rs:68`); `install_fonts(ctx)` called in `CreationContext` (`app_new.rs:27`) — correctly before frame 1. | Fonts + style are already wired. The dreaded "set_fonts is next-frame" landmine is already handled. |
| **Accent** | `ACCENT #7b61ff`, `ACCENT_HOVER`, `ACCENT_LIGHT`, `ACCENT_DIM`, `SELECTED_ROW #262340` — **all match the mockup exactly.** | Accent parity is free. |
| **Surfaces** | Diverged: code is darker than the owner-iterated mockup (see §3, Gate 0). | Gate 0 nudges ~6 token values. |
| **Panels** | Stock `SidePanel`/`TopBottomPanel`/`CentralPanel` + `workspace.rs` live data-driven dock (Left/Right/Float, edit mode, add-popup, tabs, persistence) + `panel.rs` inert registry. No dock crate. | Build on `workspace.rs`'s dock; retire `panel.rs`'s dead registry. |
| **Pie menu** | A working custom-painted pie already exists (Symmetry pie, commit `07e8418`). | Gate 6 extends a proven pattern, not a blank page. |
| **UI v2 clean shell** | `ui2/mod.rs` (~2800 lines) exists, gated by env `AZRA_UI=v2` → `ui2_draw()`: app bar, status bar, its own left/right dock (`self.ui2.dock`, DockSide/collapsed/height), floating, all already token-driven (`ui_theme::*`). Runs alongside legacy v1. | **This is the port surface for Gates 1+** — build the mockup into `ui2`, opt-in, without disturbing legacy v1 (the strangler). Split `mod.rs` (>500 lines, de-god rule) as panels land. |
| **DragValue** | Native egui — *is* the scrub field. | Gate 1 number field is a one-liner. |
| **Inline rename** | `outliner_actions::check_double_click(ui,id)` mandated helper exists. | Reuse for every rename (CLAUDE.md rule). |

**egui feasibility verdicts** (full table in the workflow result): scrub=`DragValue` (native), collapsible=`CollapsingHeader` (native), data-driven form=immediate-mode-native, reorder=`egui::dnd` (native), popovers/context-menus/hover-targets/rename/toasts=Easy. **Rotated rail tabs**=custom `TextShape` with `.angle` (Moderate). **Pie**=custom painter (Moderate, proven). **Dock drag-between-zones + viewport split**=`egui_tiles` or defer (Hard).

---

## 2. The one decision that shapes everything — dock strategy

The mockup's dock does five things: L/R/bottom panels · float · two collapse modes · **drag panels between zones** · plus **viewport splitting**. egui gives the first three at Easy effort natively. The last two are the Hard ones.

**Recommendation — staged, not up-front:**
- **Gates 3–7 use the hand-rolled system** (`SidePanel`/`TopBottomPanel`/`Area`/`Window`, consolidated onto `workspace.rs`). This covers L/R/bottom/float/collapse/add-panel — **the entire visible layout of the screenshot** — at Easy effort, no new dependency.
- **Gate 8 (last, and a go/no-go of its own) decides drag-between-zones + viewport-split**: adopt `egui_tiles` (one serde-able tree serving *both*, but it *replaces* `workspace.rs`'s `PanelDock` — a real sub-project) **or** hand-roll the corner-split gesture on the existing panels. This is deferred because everything else delivers full value without it, and it's the only place a wrong turn is expensive.

> **UPDATE (2026-07-17, Gate 3):** moot — `ui2` ALREADY hand-rolled grip-drag reorder / re-dock between zones / detach-to-float. No `egui_tiles`, no migration. **Gate 8 therefore shrinks to viewport-split only** (the corner-dot tiling of the central view), which is genuinely new. The dock question is answered by the code.

---

## 3. The gates

Each gate is independently shippable, ends **green + owner-signed against the mockup**, and touches only UI + `UiMockState`. Ordered by dependency and by Easy-first.

### Gate 0 — Theme parity (the substrate) — ✅ DONE (code), pending owner visual sign-off (2026-07-17)
**Shipped:** `ui_theme.rs` surfaces/seam/borders reconciled to the mockup (7 values: FRAME/APPBAR/CARD/FIELD/WELL/SEAM/BORDER/BORDER_HOVER); 4 pillar hues added (NURBS/CYAN/GEO/ROSE); `apply_style` `extreme_bg_color` → `SURFACE_FIELD` (inputs = mockup #282829, not the deep well); stale font comment fixed. Throwaway **Token Board** window (`ui_token_board.rs`, default-open on launch, closable, v1+v2) shows every token's *code hex → mockup hex* with ✓/≠ + a native-widget row. `cargo check -p app` green; a programmatic cross-check confirms **37/37 tokens == mockup**. **Owner action:** launch, confirm every Token-Board row is ✓ and the native widgets read like the mockup, then flip `show_token_board` default → false (or delete the throwaway) before Gate 1.
**Goal:** every surface, accent, hue, radius, and font in the app matches the mockup's final palette, so all later gates just reference tokens.
**Build:**
- Reconcile the diverged surface values in `ui_theme.rs` to the mockup (owner-approved, "compress the 3 dark grays"):
  | token | code now | → mockup |
  |---|---|---|
  | `SURFACE_FRAME` | `#0e0e0e` | `#161616` |
  | `SEAM` | `#0a0a0a` | `#141414` |
  | `SURFACE_WELL` | `#101010` | `#171717` |
  | `SURFACE_CARD` | `#202024` | `#222223` |
  | `SURFACE_FIELD` | `#26262b` | `#282829` |
  | `BORDER` | `#2c2c30` | `#2e2e2f` |
  Accents (`ACCENT`, `ACCENT_HOVER`, `ACCENT_LIGHT`, `ACCENT_DIM`, `SELECTED_ROW`) already match — leave them.
- Add the **pillar/semantic hues** as named tokens (from the mockup `:root`): `SDF #39d98a`, `MESH #3e9bff`, `LIGHT #ffd54a`, `SCULPT #ff8a3d`, `NURBS #a191ff`, `CYAN #4ecdc4`, `ROSE #ff6bb3`, `GEO #7486a3`, plus axis X/Y/Z and destructive.
- Verify `apply_style()` covers all widget states the mockup uses (panel/card/field/hover/active fills, selected-row violet + accent left-bar, border/border-hover, `RADIUS_*`).
**Reuse:** `ui_theme.rs` (already applied). **Defer:** nothing.
**Acceptance (must work):** a throwaway "token board" `Window` renders a swatch of every surface/accent/hue with its hex label; each matches the mockup hex. A row of native `DragValue`, `CollapsingHeader`, `Button`, `Checkbox`, `ComboBox` visually matches the mockup's equivalents. `cargo build -p app` green.
**Oracle:** mockup `:root` custom-property block (measured hex).

### Gate 1 — The field kit (reusable widget vocabulary) — ✅ DONE (code), pending owner visual sign-off (2026-07-17)
**Shipped:** `ui_fields.rs` (permanent, ~230 lines, tokens-only) — `num`/`int` (styled `DragValue`: FIELD fill, SEAM border, r5, mono numerals, accent border on drag/edit), `bool_row`, `enum_seg` (segmented ≤3, selected = SELECTED_ROW + ACCENT), `vec3_row` (XYZ cells with axis-colour dots), `axis_triad`, `subhead`, `section` (`CollapsingState` header: caret + caps label + right badge). Every metric measured from the mockup (label 96px, row 22/23, caps 10px +letter-spacing, dot 4px). A throwaway `demo()` (bound to egui temp memory) renders one of each and is shown under a "FIELD KIT (Gate 1)" separator in the Token Board. `cargo check -p app` = 0 errors. Scrub + collapse behaviours are native egui (guaranteed). **Owner action:** confirm the demo rows read like the mockup's Tool-Parameters rows (field colour, label alignment, section caret, XYZ dots, segmented selected-state).
**Goal:** egui equivalents of the mockup's field renderer, so Tool Parameters / Properties / Modifiers all compose from one set.
**Build a `ui_fields` module** with: `num`/`int` (styled `DragValue`, scrub + click-to-type + Shift-fine/Ctrl-coarse via `.speed`), `bool_row` (checkbox + right-aligned label), `enum_row` (segmented ≤3 buttons / `ComboBox` otherwise), `vec3_row` (XYZ with axis-colour dots), `axis_triad` (X/Y/Z toggles), `section` (custom-header `CollapsingState` with caret + caps label + right badge), `subhead`, `unit` suffix. Each takes `&mut` into a demo struct — **stub-bound, not scene-bound.**
**Reuse:** `DragValue`, `CollapsingState::show_header`, `check_double_click`. **Defer:** real data binding.
**Acceptance:** a demo panel shows one of each field type, pixel-close to the mockup's Tool-Parameters rows; DragValue scrubs, Shift=fine, click=type; a section collapses/expands with caret. Build green.
**Oracle:** mockup `.tprow` / `.xyz` / `.sect` CSS + the scrub behaviour we verified (drag +50px on step-0.01 → +0.10).

### Gate 2 — Tool Parameters panel (data-driven) — ✅ DONE (code), pending owner visual sign-off (2026-07-17)
**Shipped:** `tool_params.rs` (permanent) — the full per-tool catalog ported to Rust data (`Field`/`Group`/`ToolParams`, real defaults/ranges cited to Rust `file:line` in comments), a pillar-disambiguated `tool_params(tool, pillar)` (poly primitives shadow SDF names only in the poly pillar), and `draw(ui, tool, pillar)` rendering with the Gate-1 kit (enum ≤3 → segmented, >3 → combo; "Aucun paramètre" empty state). Snapping/Symmetry deliberately absent (global). Values ride egui temp memory (UI-only). A stub tool+pillar selector in the Token Board drives it. `cargo check -p app` = 0 errors. **Structural cross-check: 24/24 tools render the exact sections+fields the mockup does** (Box/Cylinder-sdf-vs-poly/Cube/Bevel/Loop Cut/Merge/Move/Clay/Circle Select/Union/Elongate + empty Paint/Bridge/Deselect All). **Owner action:** in the Token Board's "TOOL PARAMETERS" section, switch tools/pillars and confirm they read like the mockup panel (field look, categories, scrub).
**Goal:** the crown-jewel panel — per-tool parameters, rendered from the catalog, matching the mockup exactly.
**Build:**
- Port the **catalog as Rust data** (the tables we built: SDF primitives, booleans, poly primitives `poly/*`, poly tools, selection, transform, sculpt, Merge-by-Distance). Same field vocabulary, same categories, same real defaults/ranges (already cited to Rust `file:line` in the mockup — carry the citations as comments).
- Render with the Gate-1 kit. Pillar-disambiguated lookup (`poly/Cylinder` shadows bare `Cylinder`). Collapsible categories. The "Aucun paramètre" empty state.
- **Snapping and symmetry are NOT here** (owner corrections — they're global/ctxbar state).
- Stub: a debug **active-tool dropdown** drives the panel (stands in for real tool state).
**Reuse:** Gate 1 kit. **Defer:** real active-tool/selection signal (next phase feeds `derive_modeling_context()` + active tool).
**Acceptance:** selecting Box / Sphere / SDF-Cylinder vs poly-Cylinder / Bevel / Move / a sculpt brush shows exactly the mockup's categories + fields; poly Cube shows Size+Position+Scale; SDF Box shows Dimensions·Corners·Boolean·Transform (no Snapping); Merge-by-Distance shows Threshold. Scrub works. Build green.
**Oracle:** the Tool-Parameters catalog in the mockup (drive it in-browser per tool, compare field lists).

### Gate 3 — Panel shells + default one-dock layout — ✅ DONE (code), pending owner visual sign-off (2026-07-17)
**Reframed on contact with reality:** `ui2/mod.rs` ALREADY ships the whole dock — `PanelGroup` tab groups, `DockSide::{Left,Right,Float}`, collapse/resize/float, tab strips, **and grip-drag reorder / re-dock between zones / detach-to-float** — with the default layout the screenshot wants (Tools[SDF/Poly/Sculpt/NURBS tabs] + Tool Parameters LEFT, Outliner + Properties RIGHT). So the layout skeleton + the "hard" drag-between-zones were already built. **What Gate 3 added:** the ToolParams pane now renders the REAL Gate-2 panel (`tool_params::draw`, wired in both dock draw-sites via `tool_params_pane`), driven by a UI-only stub active tool/pillar in egui temp memory (`ACTIVE_TOOL_ID`/`ACTIVE_PILLAR_ID`, default Box/sdf — the Gate-4 Tools palette will write them). Outliner/Tools already had real content; Properties/Library stay placeholder until their gates. `cargo check -p app` = 0 errors. **Deferred:** Library placement — ui2's dock has no Bottom zone and the mockup pins Library over the viewport bottom; Gate 7 (which builds the Library) decides bottom-zone vs float. **Owner action:** launch with `AZRA_UI=v2`; confirm the left column's Tool Parameters tab shows the real data-driven panel matching the mockup.
**Goal:** the layout skeleton of the screenshot, with real chrome, stub content.
**Build:**
- Consolidate onto ONE dock (`workspace.rs`'s). Retire `panel.rs`'s inert registry (redesign R1/R8).
- **Default layout = the owner screenshot:** left column = Tools (top) + **Tool Parameters (bottom, the real Gate-2 panel)**; right column = Outliner + Properties; bottom = Library as an overlay pinned over the viewport.
- Panel chrome: header + six-dot grip, **two collapse modes** (window vs chrome-header), resize, **float** to `egui::Window`, **add-panel menu** (grip → Ajouter un panneau), zone hide.
- Outliner/Properties/Library are **shells with placeholder rows** this gate (real content in Gates 4/5/7). Tools shell too (content in Gate 4).
**Reuse:** `workspace.rs` dock, `SidePanel`/`TopBottomPanel`/`Area`/`Window`. **Defer (explicit):** drag-between-zones → Gate 8.
**Acceptance:** app opens to the screenshot layout; each panel collapses (both modes), resizes, floats, and re-docks via grip menu; add-panel menu lists all panels; Library floats over the viewport without pushing the side columns. Build green.
**Oracle:** mockup ZONES/P registry + the dock behaviours (collapse/float/resize/add-panel), and the resize-line-hides-with-Library fix.

### Gate 4 — Tools panel content (rail + rotated tabs + list)
**Goal:** the Tools panel interior.
**Build:** the vertical rail with **rotated category tabs** (custom `TextShape.angle` — the Moderate item), the tool list, command folders, tool flyout variants, the **"+" new-tab with inline rename** (`check_double_click` + swapped `TextEdit`). Clicking a tool sets the `UiMockState` active tool → drives Gate 2 (UI-only).
**Reuse:** rotated-galley paint, `check_double_click`, Gate-1 rows. **Defer:** real tool activation.
**Acceptance:** rail shows vertical category tabs matching the mockup; categories switch; tool list renders; clicking a tool updates Tool Parameters; "+" creates a tab you can name inline; Escape leaves no nameless ghost. Build green.
**Oracle:** mockup `tp-pane` (rail, `.tp-rtab` rotation, flyout, new-tab rename).

### Gate 5 — Properties panel + modifier stack
**Goal:** the Properties interior and the modifier stack we built.
**Build:** the prop-rail tabs (Object / Modifiers / SDF / Material); the **modifier stack** — object level (`PolyModifier`) + primitive level (`Modifier`), the **3 fixed passes** with headers, **drag-reorder within a pass** (`egui::dnd`, refusing cross-pass drops), add (real modifier-type menu) / remove / toggle / collapse rows, the "INACTIF" marker for the 5 no-op poly modifiers. Stub data.
**Reuse:** Gate-1 kit, `egui::dnd`. **Defer:** real modifier data.
**Acceptance:** both levels render; passes group correctly; dnd reorders within a pass and refuses to cross one; add-menu lists the real types; toggle/collapse/remove work. Build green.
**Oracle:** mockup modifier-stack section (pass grouping, the cross-pass-refusal we verified).

### Gate 6 — Overlays: pie + add menu + ctxbar
**Goal:** the floating command surfaces.
**Build:**
- **Pie (Tab):** hold-to-select, hub cycles mode, connector lines + hub direction wedge, **Ctrl-customize** (drag-reorder, add via command picker with search + pipette, size S/M/L, slot ±, persist). Shift+~ panels pie. Extend the repo's Symmetry pie.
- **Add menu (Shift+A).**
- **Ctxbar** (top strip): centred pills (orientation / pivot / snap — pillar removed), each a **settings popover**; snap targets **per-representation** (mesh vs SDF vs NURBS).
**Reuse:** existing Symmetry pie, `context_menu`/`Popup`, painter primitives. **Defer:** commands firing real ops.
**Acceptance:** Tab opens the pie at the cursor matching the mockup; hold-aim-release commits; Ctrl enters customize (reorder/add/size/save); ctxbar pills open popovers; snap targets change with the (stubbed) selection kind; Shift+A add menu. Build green.
**Oracle:** mockup pie (hold model, connector/wedge geometry, customize), ctxbar popovers, per-representation snap.

### Gate 7 — Library panel
**Goal:** the Library interior.
**Build:** folder tree (groups-in-groups), thumbnail grid (round-bottom cards), the **filter funnel** (same `.searchbox`+funnel idiom as the outliner), bookmarks/quick-access, bottom info bar. Stub content from a placeholder asset list.
**Reuse:** Gate-1 rows, funnel pattern. **Defer:** real asset library.
**Acceptance:** matches the mockup; funnel filters (categories derived from folders + favourites toggle); tree navigates; info bar at the bottom. Build green.
**Oracle:** mockup `pane-lib`.

### Gate 8 — Viewport split + dock drag-between-zones (the Hard gate, go/no-go)
**Goal:** the two topology features — quarantined last.
**Precondition:** the §2 owner decision (egui_tiles vs hand-roll vs skip drag-between-zones).
**Build:**
- **Viewport splitting:** the 4 invisible→orange corner dots, drag-to-split (dominant-axis rule), seam resize, **join-on-close**, corner context menu (**detach → floating *resizable* view**, close). ("Change to panel" is parked — see the mockup note: it collides with dock ownership; needs a third registry placement, an owner call.)
- **Panel drag-between-zones** (if kept): via `egui_tiles` or hand-rolled drop-targets.
**Reuse:** `allocate_rect`+`Sense` hover targets (Gate-feasibility #11), `egui_tiles` *iff* adopted. **Defer:** "change view to panel" (parked).
**Acceptance:** corner drag splits along the dominant axis; seams resize; closing a pane returns its space to the sibling and collapses the wrapper; detach makes a floating resizable view (8 grips) with a drag bar + close; (if kept) a panel drags from one zone to another. Build green.
**Oracle:** mockup viewport-split section (dominant-axis, join-on-close, detach→resizable).

---

## 4. Cross-cutting

- **`UiMockState`** — the single stub. Holds: active tool (name + pillar), a few fake outliner rows, fake modifier stacks, fake library assets, ctxbar state, pie customization (persisted). Every panel reads from it via one function. The **next phase** replaces each read with a real scene/tool query — that seam is the whole "connect to main code" step the owner deferred.
- **De-god discipline (CLAUDE.md):** new UI lives in focused modules (`ui_fields.rs`, `tool_params.rs`, `modifier_panel.rs`, `pie.rs`, `library_panel.rs`, `ctxbar.rs`, `viewport_split.rs`), never `pub(crate) fn` in `main.rs`, drawn from the `app_update.rs:210-231` panel block / the `:331-337` overlay block / the `:179-205` window block as appropriate. **Frame Ordering Contract:** any GPU resource mutation stays in `poll_background_tasks`, never in a panel draw.
- **Verification reality:** the running exe can't be seen from here. Every gate closes on build-green + owner side-by-side against the committed mockup. Where an interaction is drivable, self-check it; otherwise the mockup's measured numbers are the acceptance spec and the owner is the eyes.

## 5. Suggested order & rough shape
`Gate 0 → 1 → 2` (theme + kit + Tool Parameters — the highest-value, all-Easy core) → `3` (layout skeleton) → `4 · 5 · 6 · 7` (panel interiors + overlays, largely parallel-able) → `8` (topology, go/no-go). One gate per session/prompt, each behind its own acceptance.

## 6. Open decisions for the owner
1. **Dock (before Gate 8, not now):** drag-between-zones a hard requirement → `egui_tiles` migration; else grip-menu zone choice suffices and Gate 8 = viewport-split only.
2. **"Change view to panel"** stays parked (dock-ownership collision) unless you want the third-placement work scheduled.
3. **Tools panel default-on** (owner screenshot) vs off — confirmed on in the mockup; carry that default.
