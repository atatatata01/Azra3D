# AZRA UI/UX Redesign Plan — "Clean Base, Expose Slowly"

**Status:** planning · **Date:** 2026-07-13 · **Branch:** `fix/ui` (never merge to master until approved — owner rule)
**Supersedes framing of:** `UI_INTEGRATION_PLAN.md` (Phases 0–10 stay valid as the *restyle* backlog; this
doc adds the **demolition + consolidation spine** that must come first). Read both together.

> **The core problem (owner's words):** "everything is dev-look, placement of info is messy, panel
> structure is bad, scaling, duplication, change of information in panel is wrong, legacy commands no
> longer apply." This is **not a styling problem** — the tokens (`ui_theme.rs`) are already good. It is
> an **information-architecture + accumulated-cruft problem.** You cannot "expose slowly" on top of a
> mess. So we **demolish + unify first**, build a deliberately **minimal correct base**, then
> **re-expose vetted info one panel at a time.**

---

## 0. The two rules that make this tractable

1. **One-home rule.** Every piece of information renders in exactly ONE surface:
   - tool *parameters* → **Tool Options** (top-of-viewport strip)
   - object *state* (transform, material, visibility) → **Properties**
   - scene *structure* → **Outliner**
   - reusable *content* (materials, HDRIs) → **Library**
   - global *commands* → **one command registry** (menus/pie/shelf are just *views* of it)
2. **Strangler + progressive disclosure.** Nothing new is added until its single home is decided.
   The base ships showing the *minimum* that is correct; every extra panel/control must earn its way
   back in by passing the one-home test. Default state is *hidden*, not *shown-but-broken*.

---

## 1. The root findings this plan fixes (from the 2026-07-13 audit)

| # | Finding | Evidence | Fix track |
|---|---|---|---|
| R1 | **TWO coexisting dock systems** | `panel.rs` (`UiLayout`/`PanelId`/`DockZone`, `ui_layout.json`) **and** `workspace.rs` (`WorkspaceManager`/`WorkspacePanel`/`PanelDock`, `workspaces.json`) | Track B |
| R2 | **Commands scattered, not registered** | header menus (`menus.rs`) + palette registry (`win_command.rs::dispatch`) + pie (`pie_menu.rs`) + RMB (`draw_context_menu`) + shelf (`ui_shelf.rs`) + rail (`ToolButtonId`) all define commands independently | Track C |
| R3 | **Panel-visibility state triplicated** | `menus.rs` View (160–162), `win_layout.rs` (39–41), `win_command.rs` (196–198) all flip `show_*`; `panel.rs::draw_panel_list` flips a *different* set | Track B/C |
| R4 | **Add-primitive in ≥3 places** | header **Add** (`menus.rs` 129), RMB **Add Primitive** (`menus.rs` 893), rail/shelf tiles (`ui_shelf.rs` 24) | Track C |
| R5 | **Transform/dims in two homes** | floating Tool Settings W/D/H (`tool_settings.rs` 45) vs Properties transform (`properties.rs`) | Track D |
| R6 | **Shading + post-FX duplicated** | 7 inline shading toggles + per-mode SSR/Bloom/Fog popup (`viewport_header.rs` 183–343) **and** the separate Render ▾ menu (357–360) | Track D |
| R7 | **Stale dev toggles in user bars** | `force_volume_only`, `force_analytical_debug` (`win_tweaks.rs` 591), BVH (`viewport_header.rs` 357), DBG:Bricks cycler (363) — all reference disabled/parked systems per CLAUDE.md | Track A |
| R8 | **11 dead panel registrations** | `PanelId` declares 13; dock `match` only draws Outliner + Properties (`outliner_draw.rs` HOSTABLE, 15) | Track A |
| R9 | **Everything hard-coded px, not DPI-aware** | only `outliner_height_fraction` is fractional; no `pixels_per_point()` sizing anywhere | Track E |
| R10 | **Structural ops fired from Properties** | `properties.rs` 114–122 issues `add_primitive`/`slice_duplicate` from the object-state surface | Track D |

---

## 2. The decision that blocks everything — resolve FIRST

**D1 — Accent color: DECIDED → violet `#7b61ff`** (owner, 2026-07-13, from a running-app screenshot —
reverses an earlier amber lean). This is what `ui_theme.rs` already ships (`ACCENT #7b61ff`,
`SELECTED_ROW #262340`), so **no token flip needed** — keep violet. On-accent text is **white**. Keep
the semantic hues fixed: green=SDF, blue=mesh, yellow=lights, XYZ axis, red=destructive. Full palette is
documented in `UI_UX_DESIGN_RULES.md`.

Also lock the leftovers from the integration plan that this redesign depends on: **D2** (one constrained
dock — see Track B), **D4** (which command paradigms survive — see Track C). D3/D5/D6 can wait.

---

## 3. The plan — six tracks, sequenced

Each track is **shippable and mergeable on its own** (behavior-neutral where possible), on `fix/ui`,
one commit per slice, `cargo check -p app` after every slice. Tracks A→E are the *base*; Track F is the
existing restyle backlog.

### Track A — Demolition (delete the dead, cheapest + highest signal-to-noise)
*Goal: stop lying to the user. Every visible control must do something.*
- **A1** Strip stale render toggles from user-facing surfaces: `force_volume_only`, `force_analytical_debug`,
  `bvh_enabled`, DBG:Bricks cycler. Move any still-useful dev switch into ONE **Developer** window
  (`win_tweaks.rs`), never in the context bar. (Do NOT delete the underlying flags — just un-expose them.)
- **A2** Remove the 11 dead `PanelId` registrations (or mark them explicitly `Planned` and hide from
  `+ Panel`), so the panel list only shows panels that actually draw.
- **A3** Audit each RMB/menu command for "does this still apply?" — build a **demolition list** (below)
  and delete or fix each. Kill the empty-stub commands (`menus.rs::apply_tab_pie_tool` `{}`).
- **Done when:** every control in every visible bar/menu maps to a live action. No inert toggles.

### Track B — Unify the panel system (the structural keystone)
*Goal: ONE dock model, ONE persistence file. This is the single biggest fix for "panel structure is bad."*
- **B1** Choose the survivor: **`panel.rs` (`UiLayout`/`PanelId`)** is the more complete model
  (reorder/collapse/close/re-add). Make `WorkspaceManager` either (a) delegate its panel layout to
  `UiLayout`, or (b) be demoted to *only* workspace presets (which panels + widths per activity), with
  the actual docking owned by `UiLayout`. Recommendation: **(b)** — workspaces become named presets
  *over* the single dock, not a second dock.
- **B2** One serialization: migrate `workspaces.json` panel-layout fields into `ui_layout.json` (or make
  workspaces store a `UiLayout` snapshot). Additive `#[serde(default)]` only — never break old files.
- **B3** Generic **panel-host**: Outliner / Properties / (future) are interchangeable tenants of the one
  dock (extends current `draw_content_dock`). No per-panel special-casing.
- **Done when:** there is exactly one dock system, one visibility state, one layout file; reorder/
  collapse/close/resize/reset all work; layout survives restart.

### Track C — One command registry (dedup the verbs)
*Goal: every command defined once; menus, pie, shelf, RMB are VIEWS that dispatch into it.*
- **C1** Promote `win_command.rs`'s registry to the single source of truth (id, label, icon, context,
  enabled-predicate, handler). One entry per command.
- **C2** Rewrite header menus / RMB context / shelf / pie to **dispatch registry ids**, not inline
  `if ui.button().clicked()` bodies. Add-primitive (R4) collapses to one registry entry surfaced in N views.
- **C3** One panel-visibility action set (R3) lives in the registry; the three ad-hoc toggle sites call it.
- **Done when:** grep shows each command handler body exactly once; adding a command = one registry entry.

### Track D — Information architecture (fix "wrong info in panel")
*Goal: apply the one-home rule surface by surface.*
- **D1** **Tool Options strip** (new home for tool *parameters*): fold the floating Tool Settings
  (`tool_settings.rs` W/D/H) + poly tool props into a top-of-viewport strip showing ONLY the active
  tool's params. Properties stops showing create-dims. (Fixes R5.)
- **D2** **Context bar** owns mode + transform-orientation + shading, once each. Consolidate the 7
  inline shading toggles and the Render ▾ menu into one shading control + one render-settings home; kill
  the duplicate SSR/Bloom/Fog (R6). Move proportional/X-Ray out of View ▾ into tool-options/edit context.
- **D3** **Properties** = object state only. Remove structural ops (`add_primitive`/`slice_duplicate`)
  from `properties.rs` (R10) — those belong to Outliner/Add command.
- **D4** **Outliner** = structure only (already close). **Library** = content, on-demand drawer (hotkey
  B) instead of a permanent bottom band.
- **Done when:** the one-home table (§0) holds for every datum; no control appears in two homes.

### Track E — Scaling pass (DPI + fraction-aware)
*Goal: the app looks right at 100/125/150/200% and any window size.*
- **E1** Replace hard-coded panel px with token constants + `ctx.pixels_per_point()`-aware sizing.
  Panel widths as clamped fractions of the frame, not fixed 240/720.
- **E2** Make font-scale drive layout: row heights, rail width, icon sizes derive from the type scale so
  bumping UI font doesn't clip. Audit every `set_width`/`fixed_pos` from the audit §6 list.
- **Done when:** resizing the window and changing OS scale / UI font never clips or overlaps.

### Track F — Restyle & new panels (the existing UI_INTEGRATION_PLAN Phases 5–10)
Only after A–E: outliner restyle (Phase 5, prototype exists), properties restyle (6), library drawer
(7), shelf+status (8), new workspace panels (9), polish (10). These are the *reward* — safe, localized,
per-panel commits once the base is clean.

---

## 4. "Expose slowly" — the progressive-disclosure sequence

The base ships **minimal on purpose**. Re-introduce surfaces in this order, each gated on its one-home
decision, each a discrete commit the owner can veto:

1. **App bar** (brand, menus, workspace tabs) — chrome only.
2. **Viewport + context bar** (mode, shading — one home) — the thing you actually look at.
3. **Tool rail** (verbs only) + **Tool Options strip** (active-tool params).
4. **Outliner** (structure).
5. **Properties** (object state).
6. **Status bar** (stats, engine badge).
7. **Library drawer** (on-demand).
8. **Shelf / pie** (customization) — last, only if it doesn't overlap the rail/menus (D4).

Anything not on this list starts **hidden behind the Developer window**, not in the main UI.

## 5. Method (given the constraint that I can't see the running app)

- **Owner is the eyes.** Loop = I make a scoped change → `cargo check -p app` → owner runs the app and
  screenshots → I adjust. Never batch big blind UI changes.
- **Mockup-first alignment.** Before coding a surface, I render its **target as a widget in chat**
  (as done for the outliner) so we agree on the look *before* egui code is written.
- **One surface per prompt.** Per repo feature discipline; each ends with a build + a manual test note.
- **Behavior-neutral first.** Structural (Track B/C) and demolition (A) changes must not alter what
  works; visual restyle (F) is separated so regressions are easy to bisect.

## 6. Demolition list (living — append as found)
*Controls confirmed dead/duplicate to remove or re-home. Populate during Track A.*
- [ ] Context-bar `force_volume_only` / analytical / BVH / DBG:Bricks toggles → Developer window (R7)
- [ ] 11 non-drawing `PanelId` registrations → hide or mark Planned (R8)
- [ ] `menus.rs::apply_tab_pie_tool` empty stub
- [ ] Duplicate SSR/Bloom/Fog toggles (context-bar popup vs Render ▾) (R6)
- [ ] Add-primitive collapse to one registry entry (R4)
- [ ] Triplicated panel-visibility toggles → one registry action (R3)
- [ ] Structural ops in `properties.rs` 114–122 (R10)

## 7. Sequencing summary

```
D1 accent (decide)  ──▶ unblocks all visual sign-off
Track A demolition  ──▶ cheapest, do first (stop lying to the user)
Track B one dock    ──▶ structural keystone (unblocks D/F re-homing)
Track C one registry──▶ dedup verbs (parallel-ish with B)
Track D info-arch   ──▶ after B (needs the single dock to re-home into)
Track E scaling     ──▶ after layouts stabilize (B/D)
Track F restyle     ──▶ last; the existing Phase 5–10 backlog
```

**First concrete step:** lock D1 (accent), then Track A1 (strip the 4 stale dev toggles) — a small,
safe, high-signal win that immediately makes the app feel less "dev-look," and proves the loop.
