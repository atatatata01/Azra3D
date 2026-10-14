# AZRA UI Integration Plan — "Rebuilding the DCC Layout"

**Status:** planning (no code yet) · **Author:** UI window · **Date:** 2026-07-13
**Design source:** `Rebuilding DCC application layout-handoff.zip` →
`AZRA Model Workspace.dc.html` (full-app mockup) + `AZRA UX System.dc.html` (design system + rationale).

> The mockups are a **visual + interaction spec**, not portable code. AZRA's UI is
> **egui (immediate-mode Rust)** — every region is re-painted from Rust each frame. "Integration"
> means re-implementing the spec in egui, panel by panel. The good news: **most regions already
> exist** and this is largely restyle + reorganize, not greenfield.

---

## 0. What already exists (the starting point)

AZRA is NOT starting from zero. Present today:

| Concern | Where it lives now | Reuse verdict |
|---|---|---|
| Workspace system | `app_types/types_workspace.rs` — `WorkspaceManager` (save/load `~/.azra/workspaces.json`), `Workspace{panels}`, `WorkspacePanel{id,name,buttons,dock,width,collapsed}`, `PanelDock::{Left,Right,Float}` | **Extend** — foundation is here, model needs enriching |
| Panel drawing + edit mode | `app/src/workspace.rs` `draw_workspace_panels()` (dock controls, float, resize) | **Extend** |
| Toolbar (data-driven) | `render_tool_button()`, `ToolButtonId`, `tool_button_meta()` | **Restyle** |
| Context bar / viewport header | `viewport_header.rs` | **Restyle + consolidate** |
| Outliner | `outliner_panel.rs`, `outliner_draw_body.rs`, `outliner_draw.rs` | **Restyle** (see `outliner_interactive_prototype.html`) |
| Properties | `properties_*.rs` | **Restyle** |
| Material/Library | `mat_preview.rs`, library panel | **Restyle → drawer** |
| Pie menus | `pie_menu.rs` | **Keep** — decide coexistence w/ header menus + shelf |
| Input/hotkeys | `input_system.rs` (register actions) | **Extend** for shelf/hotkey editor |

**Gap to the mockup:** the current dock is per-panel `Left/Right/Float` holding *toolbar buttons*.
The mockup wants a **constrained single-dock of *content* panels** (Outliner/Properties/History/…),
with grip-reorder, collapse, close, `+ Panel` re-add, 3 width presets, per-workspace layout presets,
activity-named workspaces, a quick shelf, and a systematic token palette.

---

## 1. Principles to adopt verbatim (from the UX doc — they're good)

1. **One-home rule.** A datum renders in exactly one place: tool *parameters* → Tool Options strip;
   object *state* → Properties; scene *structure* → Outliner; reusable *content* → Library.
2. **Hierarchy = distance from viewport.** Primary (rail, context bar) touches the viewport;
   secondary (dock) is one gutter away; advanced hides in flyouts / collapsed sections / the drawer.
3. **Transience floats, never reflows.** Anything that can vanish (adjust card, drawer, flyouts,
   toasts) floats *over* the viewport. Docked things never surprise-move.
4. **Constrained docking is the feature.** One dock column, ordered panels, three width presets —
   "every layout a user can produce is a good layout." (No Blender infinite-split foot-guns.)
5. **Surface carries interaction, color carries meaning.** Hover/press only move up/down the graphite
   ladder; hue is reserved for meaning (active, SDF, mesh, lights, axis, destructive).

---

## 2. Decisions to lock BEFORE coding (this is the real gate)

These are product decisions, not coding ones. Locking them prevents building conflicting systems.

- **D1 — Accent color: amber or violet?** ⚠️ The handoff contradicts itself: the written palette says
  **amber `#F0A629`** = active/selection (matches today's app), but the mockup *renders* everything in
  **violet `#7b61ff`** as brand/active. "Happy with the mockup look" implies violet. Pick one — it
  recolors every active/selected/focused state and the brand mark. *Lean:* violet as brand + active
  (matches the look you like); keep green=SDF, blue=mesh, yellow=lights, XYZ=axis, red=destructive.
- **D2 — Docking model.** Adopt the constrained single-dock (Default 360 / Focus slide-away / Split
  560)? This *replaces* today's free `Left/Right/Float` per-panel model with something simpler and
  foolproof. (Recommended — it's exactly your "easy to reorganize, can't break it" goal.)
- **D3 — Panel scope.** Promote Outliner / Properties / History (and later SDF Graph, Channels, etc.)
  into **one generic panel-host component** ("every panel is the same component — no special cases").
- **D4 — Command paradigms.** The mockup adds Blender-style **header menus** (View/Select/Add/Object)
  and a **floating command shelf**, alongside AZRA's existing **pie menus** and **tool rail**. Decide
  which coexist and who owns what, or you get 3–4 overlapping ways to run the same command.
- **D5 — Workspace taxonomy.** Rename/reorg current representation-named workspaces (SDF/Sculpt/Poly/
  NURBS) into **activity-named** ones (Model / Sculpt / SDF / UV / Shade / Animate / Render). Which
  ship in v1 vs. stub-for-later?
- **D6 — Library.** On-demand **drawer over the viewport (hotkey B)** + optionally pinnable as a dock
  panel — replacing the permanent bottom band.

---

## 3. Phased plan

Each phase is **shippable on its own** and, where possible, **behavior-neutral** (visual only) so it
merges cleanly against in-flight pillar work. Order matters: tokens first (everything references them),
then the panel system (your top priority: move / scale / reorganize), then chrome panel-by-panel.

### Phase 0 — Design tokens (foundation) — *small, do first*
- Build `ui_theme` module: surface ladder (6 grays `#0B0C0D`→`#2D3033`), semantic accents (per D1),
  type scale (11 caps / 12.5 UI / 13 titles / 12 mono), radii (4 control / 8 panel), row height 26,
  motion constants (120/80/0/130/180 ms). Embed **Geist + Geist Mono** in the egui font atlas.
- Every later phase reads from this — no hard-coded colors.
- **Done:** one `egui::Style` + token constants; a scratch "swatch" view matches the UX doc §7.

### Phase 1 — Panel & dock system — *the priority (move / scale / reorganize)*
- Enrich `WorkspacePanel`/`PanelDock` into the constrained single-dock model (per D2/D3):
  ordered content panels in a right dock column; **grip `⠿` drag-to-reorder with live preview**;
  **chevron collapse**; **`✕` close** → returns via **`+ Panel`** menu; **3 width presets**
  (Default / Focus-slide-away / Split) with drag-edge fine width + double-click-edge to cycle.
- Generic **panel-host component** so Outliner/Properties/etc. are interchangeable tenants.
- Persist panel set + order + collapse + width in the workspace definition (extend `workspaces.json`
  via `#[serde(default)]` so old files still load — no save-format break).
- **Done:** you can reorder, collapse, close, re-add, and resize panels; layout survives restart;
  "Reset workspace" restores defaults.

### Phase 2 — App bar + workspace tabs
- App bar: brand mark, menu cluster, centered **workspace tabs** (activity-named per D5) with
  **`+` fork**, **override dot** when a workspace is modified, and Save / Revert / Reset in its menu.
- Preferences opens as a **window (⌘,)**, never a workspace (can't trap the layout).
- **Done:** switch/fork/reset workspaces; modified state shows a dot; presets remembered per workspace.

### Phase 3 — Context bar (consolidate the scattered mode controls)
- One bar owned by the viewport: **Mode (Object/Edit/SDF)** at the left; element-select (vert/edge/
  face) when in Edit; transform **orientation · pivot · snap · proportional**; **shading** (wire/solid/
  material/rendered) + overlays at the right. Engine badge (Vulkan) moves to the **status line**.
- Folds today's `viewport_header.rs` + parts of the pie/keyboard state into one home (fixes UX audit D).
- **Done:** every mode/transform/shading control has exactly one place; wires to real state.

### Phase 4 — Tool rail (verbs only) + Tool Options strip
- Rail = **4 labeled groups (Select · Transform · Create · Modify)**, one active tool (accent), corner-
  triangle **flyouts** for variants; **rail never scrolls**. Create group swaps to SDF primitives in
  SDF mode (same slots/hotkeys).
- **Tool Options strip** (top of viewport) shows **only the active tool's parameters**; **Tool
  Parameters** panel (Modo-home, bottom of rail) for persistent numeric fields (XYZ inputs, etc.).
- **Done:** rail holds verbs only (no "add light/camera"); parameters have one home (fixes audit A/C).

### Phase 5 — Outliner restyle — *(you already prototyped this)*
- Apply `docs/UI/outliner_interactive_prototype.html`: typed icons, branch/elbow tree lines, zebra,
  active/selected/hidden/locked states, hover **lock + gear** columns, context menu. Display *filters*
  (not 5 sub-editors — Mesh Ops → Properties modifier stack; Shading/Images → dockable panels) per
  audit E. Selection = amber 3px left-bar + tint (not a flood).
- **Done:** outliner matches the prototype; behavior unchanged; merges as a discrete commit.

### Phase 6 — Properties restyle
- Section headers, one-home object state, modifier stack (absorbs "Mesh Ops"), mono-aligned numeric
  fields, XYZ input triples per the mockup's Tool-Parameters form styling.
- **Done:** Properties is the single home for object state; visually matches tokens.

### Phase 7 — Library drawer (per D6)
- On-demand drawer sliding over the viewport (**hotkey B**): Materials / HDRIs / Textures / MatCaps
  tabs, folders, grid, "drag onto object or double-click to assign." Pinnable as a resident dock panel.
- **Done:** library no longer eats permanent vertical space (fixes audit F).

### Phase 8 — Quick shelf + status bar (the customization loop)
- Bottom bar = **personal command shelf** (per workspace) + **status** (scene stats, engine badge).
- Customization loop everywhere: **change in place → override dot → Save / Reset**. Pencil edit mode
  (✕ remove, `+` add from searchable palette, drag reorder). **"Pin to Shelf"** from any right-click.
- Optional floating **contextual shelf** in the viewport (bottom-center) — decide vs. pie menus (D4).
- **Done:** users build their own shelf; it persists per workspace; one-click reset.

### Phase 9 — New workspaces & panels (as subsystems come online)
- SDF Graph panel (SDF workspace), UV Layout (UV), Node Editor + Material Props (Shade), Channels/
  Curves + timeline (Animate), Render Settings/Passes (Render). Each is a tenant of the Phase-1 panel
  host + a workspace preset. Ship as each area matures — not blocking.

### Phase 10 — Polish pass
- Interaction states (9 states, §8), motion timings, focus rings, semantic-color audit (no decorative
  hue), hotkey editor (single global, inline conflicts, tooltips show bindings).

---

## 4. Sequencing, dependencies & merge discipline

```
Phase 0 (tokens) ──▶ everything
Phase 1 (panels) ──▶ Phases 5,6,7,9 (they dock into it)   ◀── your priority
Phase 2 (app bar/workspaces) ─▶ Phase 8 (shelf is per-workspace)
Phases 3,4 (context bar / rail) — independent, any time after tokens
Phases 5,6 (outliner / properties) — after Phase 1
Phase 7 (library) — after Phase 1
Phase 8 (shelf) — after Phase 2
Phase 9 — as subsystems land
Phase 10 — last, continuous
```

- **Behavior-neutral restyles merge cleanly** against poly/sculpt/cache/assembly pillars because they're
  localized to UI draw code. **Coordinate the higher-churn phases (1, 3, 4) with the integrator** — they
  touch files pillars also touch; land them in low-churn windows.
- **Never break `workspaces.json`**: all new fields ride `#[serde(default)]` (same rule as `.fl4`).
- **One phase per prompt** per the repo's feature discipline; each ends with a build + a manual test.

---

## 5. Effort & risk

- **Magnitude:** weeks of focused UI work, but decomposes into **~12–15 self-contained commits**
  (1 tokens + panel system + ~6 panel restyles + shelf/workspaces + polish). The restyles are the safe,
  tractable bulk; the new interaction paradigms (shelf, docking presets, workspaces) carry the cost/risk.
- **Top risks:** (1) the **accent decision (D1)** blocks a clean token pass — resolve first; (2) **paradigm
  overlap (D4)** — shelf vs. rail vs. pie menus vs. header menus doing the same job; (3) **blast radius**
  vs. active pillars — mitigated by token-first + per-panel behavior-neutral commits; (4) docking-model
  migration touching saved layouts — mitigated by additive serde.

## 6. Immediate next step

Lock **D1–D6** (a short decision review — mostly D1 accent and D4 paradigms). Then start **Phase 0
(tokens)** and **Phase 1 (panel system)** — the panel system is your stated priority (move / scale /
reorganize), and once it exists every subsequent panel restyle just docks into it.
