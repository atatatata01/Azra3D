# AZRA UI/UX Design Rules â€” Canonical Design System

**Status:** living reference Â· **Date:** 2026-07-13 Â· **Branch:** `feat/ui-polish`
**Source of truth in code:** `core_engine/app/src/ui_theme.rs` (tokens) â€” this doc documents intent;
`ui_theme.rs` is authoritative for exact values. **No panel may hard-code a color, size, or font** â€”
reference the tokens. Visual target: `azra_app_mockup` artifact + the mockups under `docs/UI/`.

> Recreated from the redesign learnings. Pairs with `UI_REDESIGN_PLAN.md` (the *how/when*) â€” this is the
> *what it must look and behave like*. Keep both green.

---

## 1. Accent â€” DECIDED: **violet `#7b61ff`** (2026-07-13, owner)

The app's active/selection/focus/brand color is **violet**, matching the current live app (owner
confirmed from a running-app screenshot). This reverses the earlier amber lean. One constant drives it:
`ui_theme::ACCENT`.

| Token | Hex | Use |
|---|---|---|
| `ACCENT` | `#7b61ff` | active Â· selection Â· focus Â· brand |
| `ACCENT_HOVER` | `#8b6fff` | hover on accent |
| `ACCENT_LIGHT` | `#a191ff` | active tab text, small accent glyphs |
| `SELECTED_ROW` | `#262340` | selected list-row fill (violet-tinted, opaque) |
| on-accent text | `#ffffff` | text/icons on an accent fill (violet â†’ **white**, not dark) |

**Rule:** hue carries *meaning*, surface carries *interaction*. Hover/press move up/down the graphite
ladder (no hue); only the single active/selected thing gets the accent. Selection = tinted row
(`SELECTED_ROW`) + a solid accent **left bar**, never a full-saturation flood.

## 2. Surface ladder (dark-only â€” deliberate, it's a DCC)

Background stack, darkâ†’light (`ui_theme.rs`):

`SURFACE_FRAME #0e0e0e` â†’ `APPBAR #141414` â†’ `BAR #161616` â†’ `TOOLCOL #181818` â†’
`PANEL #1c1c1c` â†’ `CARD #202024` â†’ `HEADER #232323` â†’ `FIELD #26262b` â†’ `CONTROL #2c2c2c`;
wells `WELL #101010`; seams `SEAM #0a0a0a`. Borders: `BORDER #2c2c30`, `BORDER_SUBTLE #262626`,
`BORDER_HOVER #3b3b40`.

**Rule:** each nesting level steps ONE rung. Seams are 1px `SEAM`. Inputs/wells go *darker* than their
container; raised/hover goes *lighter*. Never two non-adjacent rungs touching.

## 3. Semantic hues â€” meaning, never decoration

`SDF #39d98a` (green) Â· `MESH #3e9bff` (blue) Â· `LIGHT #ffd54a` (yellow) Â· `CAMERA #ff5555`
(intense red â€” cameras get their own hot hue so they pop from the tree, distinct from the muted
`DESTRUCTIVE #e08a80` and axis `X #e05252`) Â· axes `X #e05252 / Y #7dc242 / Z #4a8fe0` Â·
`WARNING #ffb020` Â· `DESTRUCTIVE #e08a80`.

**Boolean-op indicator (SDF prims only):** each SDF primitive row shows a small boolean-op badge
before its type icon â€” **green `unite` = Union (add)**, **red `subtract` = Subtract (cut)**, **amber
`intersect` = Intersect** (Phosphor glyphs). Click cycles the op; tooltip names it. Non-SDF rows have no
badge. This surfaces the boolean chain at a glance in the tree. **Leaf SDF prims show the boolean badge INSTEAD of a
type icon** (no redundant cube). **The SDF *body* (the top analytical unit) uses a unique
icon â€” Phosphor `circles-three` (green)** â€” distinct from the mesh `cube`. **There is NO "sub-body"
container** â€” a sub-body would read as a group and confuse the flat-CSG mental model. Instead, **prims
nest inside prims** (drag a prim onto another prim to reparent/nest it); a nested prim is still a prim
(badge only) shown by indentation + branch lines, never a group icon. So: SDF body = `circles-three`
icon; every SDF prim (nested or not) = boolean badge only, no type icon. Poly/other types have no badge.
Drag-to-nest highlights the drop target with an inset accent ring.

**Configurable info columns:** the outliner supports user-added, resizable columns (Maya-style) â€”
Modifiers Â· Animation Â· Textures Â· UV Sets Â· Manifold (extensible). **Right-click the `Name` header** to
toggle columns. Columns are **fixed narrow width (~32px)** â€” just big enough for a dot or small number,
no resize (the info is tiny). The header must reserve the tree's scrollbar width (measure
`offsetWidth - clientWidth`, pad the header right) so header icons stay aligned with row cells at any
panel width. **Name is the priority: when the panel narrows, info columns drop rightâ†’left** (keeping
Name â‰¥ ~96px) rather than squeezing the Name â€” recompute the fitting-column count on resize/dock-drag. Each cell shows a compact value: a small
colored **dot** (anim = amber, non-manifold = red) or a **number** (texture/UV/modifier count). If the
row has no value â†’ empty; if the column is **not applicable** to that type (e.g. Manifold on a camera)
â†’ empty. In egui: dynamic `grid`/columns with a header context menu and draggable separators; store the
active-column set + widths in the panel/workspace layout.

**Context menu is type-contextual:** the row right-click menu leads with a type section, then common
actions (Rename Â· Duplicate Â· Lock Â· Hide Â· Delete). Sections: **SDF prim** â†’ Boolean (Union/Subtract/
Intersect) Â· Change Shape Â· Add Modifier; **SDF body** â†’ Save as Part Â· Convert to Mesh Â· Add Modifier;
**Light** â†’ Light Type (Directional/Point/Spot/Area); **Mesh** â†’ Convert to SDF Â· Add Modifier; **Camera**
â†’ Look Through Â· Set Active. Multi-choice entries are hover-flyout submenus (â€º). In egui: build the menu
from the item's category, use nested `ui.menu_button()` for the submenus.

**Eye interactions:** click = toggle visibility; **Alt-click = isolate** (hide all but this + its
hierarchy; Alt-click again exits and restores the prior state via snapshot); **right-click = eye menu**
(Isolate Â· Show all Â· Hide all Â· Invert visibility Â· Show/Hide hierarchy). In egui: `modifiers.alt` on
the eye response, and `.context_menu()` on the eye widget.

**Hidden state:** a hidden object dims its name+icon (~0.42 opacity) AND turns its visibility eye
**red `#f0645c`** (eye-slash glyph) so hidden rows are scannable at a glance. Red here = "off/hidden",
distinct from camera red and semantic-destructive.

**Scrollbars:** visible + grabbable (not thin/auto-hidden), ~13px, `WELL` track + `CONTROL` rounded
thumb (7px radius), `BORDER_HOVER` on hover, `ACCENT_DIM` on active-drag. Vertical on the right,
horizontal at the bottom. In egui this is `ScrollArea` with `bar_width` from tokens.

**Rule:** these are reserved for their meaning. A green pill ALWAYS means SDF; never use a semantic hue
as decoration or as a second accent. Icons are tinted by type (SDF body = green hex, mesh = blue,
light = yellow, camera = neutral). **Row text:** a top-level body uses normal `TEXT`; its SDF
child/prim rows use `SDF` green. (e.g. "SDF Body" = white, "Hull_Base" = green.)

## 4. Text ladder

`TEXT #e6e6e6` (primary) Â· `TEXT_SECONDARY #c8c8c8` Â· `TEXT_LABEL #a0a0a0` (field labels) Â·
`TEXT_DIM #909090` Â· `TEXT_SECTION #8a8a8a` (uppercase headers) Â· `TEXT_FAINT #646464` (metadata/disabled).

## 4b. Icons â€” `egui_phosphor` (Phosphor Regular)

The app has **no icon font today**; the redesign adopts **Phosphor (Regular weight)** via the
**`egui_phosphor`** crate â€” the same set embedded in the target mockup, so mockup â‡„ egui icons are the
identical glyphs (true 1:1). Icons are tinted by meaning (type color for tree rows, accent for active,
`TEXT_DIM`/`TEXT_FAINT` for idle chrome), never multi-colored. Reference glyphs by their Phosphor name
(e.g. `cube`, `eye`, `lock-simple`, `gear-six`, `caret-down`, `arrows-out-cardinal`,
`dots-six-vertical`). Add `egui_phosphor` to `app/Cargo.toml` and merge its font into the egui atlas
alongside Geist in `install_fonts`.

## 5. Typography

- **UI sans = Geist** (Medium working weight; SemiBold for titles via `azra_semibold` family),
  **numerics/keycaps = Geist Mono** â€” embedded in the font atlas (`install_fonts`), not system-dependent.
- Type scale (px): `FONT_CAPS 10` (uppercase section labels, +letter-spacing) Â· `FONT_UI 12.5`
  (body/buttons/tool names) Â· `FONT_TITLE 12.5` (panel titles) Â· `FONT_MONO 11` (values/keycaps).
- **Rule:** every number the user reads (transform fields, stats, keycaps) is **mono**. Section headers
  are uppercase caps. Never below 10px.

## 6. Metrics

Radii: `RADIUS_SEAM 3` Â· `RADIUS_CONTROL 5` (buttons/inputs) Â· `RADIUS_CARD/PANEL 8`.
Heights: `ROW_TOOL 28` Â· `ROW_HEIGHT 24` (list/property rows) Â· `H_APPBAR 44` Â· `H_CONTEXT 36` Â·
`H_PANEL_HEADER 30` Â· `H_STATUS 24`. Motion (ms): surface 120 Â· press 80 Â· focus 0 Â· popover 120 Â·
toast 180.

## 7. Information architecture â€” the one-home rule (non-negotiable)

Every datum renders in **exactly one** surface:

| Datum | Home | Never in |
|---|---|---|
| tool *parameters* (active tool's inputs) | **Tool Options** strip (top of viewport) + Tool Parameters (rail bottom) | Properties |
| object *state* (transform, material, visibility flags) | **Properties** | Tool Options, context bar |
| scene *structure* (hierarchy, parenting) | **Outliner** | Properties |
| reusable *content* (materials, HDRIs, textures) | **Library** (on-demand drawer, hotkey B) | permanent band |
| global *commands* | **one command registry** â€” menus/pie/shelf/RMB are just *views* | inline `if button` bodies |
| mode / orientation / shading | **context bar**, once each | View menu + popups + Render menu |

**Rule:** if a control appears in two homes, one is wrong â€” delete or re-home it. Structural ops
(add/duplicate) never fire from Properties.

## 7b. Naming / vocabulary (so we communicate precisely)

Nested largest â†’ smallest. Use these exact terms in prompts and code:

- **Dock** â€” a docked region (right / left / bottom) that holds panel groups.
- **Panel group** â€” a slot in the dock. Shows one or more panels as **tabs** (the top **tab strip**).
  It owns the **grip** `â£¿` (move/dock) and `+` (add a panel). *This chrome belongs to the dock, NOT to
  any panel.* Switching tabs swaps which panel is shown in the slot (VS-Code-/Blender-editor style).
- **Panel** â€” a tenant: Outliner, Image, Sculpt Layer, Paint Layer, Properties, â€¦ Each is a
  quick-access view you switch to via the group's tabs.
- **Panel toolbar** â€” a panel's OWN controls (e.g. the Outliner's `+ Add` / Filter). Belongs to the panel.
- **Column header** â€” e.g. the Outliner's eye / `Name` row. Belongs to the panel.
- **Row** â€” one tree item inside the Outliner.

**Rule:** "the Outliner" means *only* the panel content (toolbar â†’ column header â†’ tree). The tab strip
above it is the **panel group**, shared chrome. Never conflate the two.

## 8. Panels & docking

- **ONE dock system.** (`panel.rs`/`UiLayout` is the survivor; workspaces are *presets over* the one
  dock, not a second dock.) One persistence file, additive `#[serde(default)]` only.
- Panels are interchangeable tenants of a generic host: grip `â£¿` reorder Â· chevron collapse Â· `âœ•` close
  â†’ re-add via `+ Panel`. A panel with sub-editors uses **tabs** in its header (Outliner / Image /
  Sculpt Layer / Paint Layer â€¦), not stacked panels.
- **Tear-off (detach) panels.** Dragging a panel's grip `â£¿` **out of the dock** and releasing over the
  viewport **detaches it into a floating standalone window** (position it at the drop point; a dashed
  accent ghost previews the action during the drag). Drag a floating panel's grip **back over the dock**
  to re-dock; the grip menu also offers Float / Dock. In egui: a floating `egui::Window` when detached,
  re-parented into the dock on drop over the dock rect â€” with an **insertion line** showing which slot
  (between which docked panels) it will land in, so the user chooses placement among 3+ panels. Grip
  drags must **pointer-capture** so a release always registers (no dropped drops). A floating panel is **resizable from every edge
  and corner** (N/S/E/W + diagonals), and **all its right-click menus keep working** â€” menu layers must
  sit above the floating window (z-order menus > floating panel).
- **Split to add a panel.** **Shift + drag/click a splitter** (the resize handle between two docked
  panels) inserts a **new empty panel slot** at that position, showing a **panel-type picker** (Image /
  UV Layout / Node Editor / History / Channels / â€¦). The dock is N-panel: a dynamic splitter sits
  between each consecutive pair, each pane resizable (per-pane height basis). In egui: N resizable rows
  with a shift-modifier on the separator that inserts a new panel-host row.
- **Transience floats, never reflows.** Anything that can vanish (Tool Options, Library drawer, toasts,
  flyouts) floats *over* the viewport. Docked things never surprise-move.
- **Constrained docking is the feature.** Ordered column + width presets â€” "every layout the user can
  produce is a good layout." No infinite-split foot-guns.

## 9. Scaling (DPI + font-driven)

- **No hard-coded panel px.** Widths/heights are clamped fractions of the frame; sizing is
  `ctx.pixels_per_point()`-aware. Row height, rail width, icon size derive from the type scale so a
  larger UI font never clips.
- **Rule:** the app must look correct at 100/125/150/200% OS scale and any window size. Audit every
  `set_width`/`fixed_pos`/literal px against this.

## 10. Do / Don't

**Do**
- Reference tokens for every color/size/font.
- Selection = `SELECTED_ROW` + accent left-bar.
- Tint icons by type; keep row text in the text ladder.
- Hover lifts one surface rung; active = accent fill (white text).
- Mono for all numerics; caps for section headers.

**Don't**
- Hard-code a hex, px, or font in a panel.
- Use a semantic hue (green/blue/yellow) as decoration or a second accent.
- Flood a selected row with full-saturation accent.
- Show the same datum in two homes.
- Leave an inert/dead control visible (dev toggles â†’ Developer window only).
- Rely on frame-timing for layout; use fractions/tokens.

---

## 11. Method (visual iteration)

The app can't be screenshotted by the assistant (exe locked). So: **mockup-first.** Before coding a
surface, render its target (chat widget or the `azra_app_mockup` artifact), get owner sign-off, THEN
write egui. Loop: change â†’ `cargo check -p app` â†’ owner runs + screenshots â†’ adjust. One surface per
prompt. All UI work stays on `feat/ui-polish` (never merge to master unpromised).
