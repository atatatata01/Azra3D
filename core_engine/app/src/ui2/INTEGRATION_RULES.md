# UI2 Integration Rules — Mandatory for every agent / PR

**Audience:** any human or agent adding code under `app/src/ui2/` or touching ui2-adjacent surfaces (menus, outliner, keybindings, dock, tools, ops).

**Goal:** release-worthy polish **and** a call graph that can become a **procedural node graph** later (each function / panel / tool / op = one callable unit).

If a change would violate a rule below, **stop and refactor first**. Do not dump “just one more feature” into a grab-bag file.

---

## 0. PRIMARY RULE — one unit = one file (node-ready)

**The 500-line cap is OK hygiene. The more important rule is isolation by capability.**

At some point we want **procedural / geometry node modeling**: each user-facing operation becomes a node. That only works if code is already split so a node can call **one clear entrypoint** without dragging a 2k-line module.

| Unit | Own file (or directory leaf) | Callable surface |
|------|------------------------------|------------------|
| **Panel** (Outliner, Properties, Library, History, …) | `panels/<name>.rs` or `outliner/`, `library_panel.rs` | `draw_*` / `ui2_*` entry only |
| **Tool** (Box, Extrude, Knife, Brush, …) | `tools/ops/<name>.rs` or `tools/activate` arms → extract | `fn activate_…` / `fn apply_…` |
| **Menu command** (Save, Instance, Invert, …) | Prefer `ops/` or `menu_shared` fillers calling **ops**, not mega-match | `fn do_…(&mut App)` |
| **Chrome primitive** (seam, float handle) | `chrome/<name>.rs` | pure UI helpers |
| **Orchestrator** only | `mod.rs` / `draw/mod.rs` / `shell/mod.rs` | wires units; **no** business logic |

### Naming for future nodes

- Prefer **verb-noun** free functions or small modules: `ops::instance::create`, `ops::poly::invert_selection`, `ops::sdf::add_box`.
- UI (button, tool row, menu, shortcut, **future node**) only **calls** that function — never reimplements.
- One public entry per unit; private helpers stay in the **same** file (or `ops/foo/helpers.rs` if large).
- **Do not** grow a giant `match name { "Box" => …, "Sphere" => … }` forever — each arm should become `ops::sdf::box::activate` and the match only dispatches.

### When adding anything new

1. **New tool / op / panel** → **new file** first, then wire one call site.
2. **Editing an existing mega-match** → extract the arm you touch into its own file in the same PR.
3. **Orchestrators** stay thin: list of calls, no scene math.

```
// GOOD — node can call this later
// app/src/ops/sdf/add_box.rs
pub fn add_box(app: &mut FourthLayerApp, at: Vec3) { … }

// UI only:
ops::sdf::add_box(self, at);

// BAD — trapped inside tools activate match, hard to node-ify
match name { "Box" => { /* 40 lines of logic */ } }
```

---

## 0b. Size limits (secondary — hygiene)

| Path | Max lines | Notes |
|------|----------:|-------|
| Any leaf unit file | **500** | Prefer ≤300 for a single op. |
| Orchestrators (`mod.rs`, `ui2_draw`) | **200** | Wire only. |
| `ui2/chrome/*` | **300** | Primitives only. |
| `app_types/keybindings.rs` | grows OK | ACT_* registry. |

**God-file ban:** Do not recreate `mod.rs.god_backup`. If a file exceeds 500 **or** mixes unrelated units (two panels, three tools), **split by unit** in the same PR — even if each piece is short.

---

## 1. Module ownership (where code goes)

```
ui2/
  mod.rs           re-exports only — no logic
  types.rs         Ui2State, PanelKind, Act, dock geometry types
  chrome/          ★ resize / zone-hide / float handles / scroll / frame
  draw/            ui2_draw entry + pies / placement / viewport_split
  shell/           app_bar · ctxbar · shading · status
  menus.rs         File…Help fillers → call ops, don’t own scene logic
  dock.rs          L/R reserve + Act glue (uses chrome)
  pane.rs          tab strip + pane body *dispatch* to panel modules
  tools/           catalog · palette · activate (activate only *dispatches*)
  tools/ops/       ★ preferred: one file per tool/op (node-ready)
  outliner/        tree UI only — see §3
ops/               ★ preferred long-term for non-UI scene operations
  (shared by menu, shortcut, tool, and future geometry nodes)
```

| Need | Call / put |
|------|------------|
| Dock / library edge resize | `chrome::draw_resize_seam` + `ZoneState` |
| Zone restore strip | `chrome::draw_restore_strip` |
| Float 8-handle resize | `chrome::draw_handles` |
| Panel scrollbar style | `chrome::apply_panel_scroll_style` |
| Pane surface / header paint | `chrome::paint_pane` / `paint_header` |
| Menu / tool / shortcut / **node** doing the same thing | **one** `ops::…` function |
| Outliner row paint | `outliner/*` — not dock/shell |
| Scene mutations | `transaction` / `AppCommand` via ops — never mid-paint without collect→apply |

---

## 2. Single source of truth (SoT)

| Concern | Canonical store | Forbidden |
|---------|-----------------|-----------|
| Tree expand/collapse | **One** set on `OutlinerState` (see helpers `ol_is_expanded` / `ol_set_expanded`) | Second set on `Ui2State`; dual polarity hacks without helpers |
| Outliner filter | Prefer `OutlinerState` or document ui2-only `ol_filter` — one active path | Two filters both live |
| Selection order (Shift-range) | Explicit row slice from current build | Silent read of stale `outliner.visual_order` under v2 |
| Keybinds | `app_types::keybindings` `ACT_*` + `KeyBindings` | Parallel `key_pressed(Key::…)` for rebindable actions |
| Tool active | `Ui2State` (prefer typed pillar, not free `String` long-term) | Third copy in random egui memory without a single writer |
| Dock width / hide | `Ui2State` widths + hidden **or** `ZoneState` fields — synced only via chrome helpers | Hand-rolled edge hit loops next to chrome |

**Dual-write rule:** If v1 and v2 must stay in sync temporarily, hide it behind **one helper**. Call sites never touch both stores.

---

## 3. Outliner rules

1. **Collect → apply:** gather clicks/menus during paint; apply after (existing pattern). No `transaction` inside row paint closures that also borrow UI mutably without deferral.
2. **No growth of a monolith:** new columns / context verbs / selection modes go into the matching submodule (`menu`, `row`, `input`, `actions`), never a 2k-line `mod.rs`.
3. **Keyboard focus:** panel keys (rename, arrows, Space expand) only when pointer-over / explicit focus (`ol_pointer_over` or successor). Never steal viewport global keys without a gate.
4. **Rename:** `ACT_RENAME` only — no raw `Key::F2`.
5. **Visibility / lock:** one apply path for icon **and** context menu (already documented on eye/lock).

---

## 4. Shortcut system (SSO) — non-negotiable

### Adding a shortcut

1. Add `pub const ACT_…` in `app_types/src/keybindings.rs`
2. Add `ActionDef` in `ACTION_DEFS` (name + category + **context** if not global)
3. Add `default_bind` arm
4. Dispatch **exactly once** (usually `input_keyboard.rs` or panel-local with focus gate)
5. Every menu/tool label uses `self.keybindings.label(ACT_…)` — **no** `"\tCtrl+S"` literals for rebindable actions
6. Run / keep green: `no_duplicate_global_default_chords` test

### Forbidden

- Advertising a key the code does not honor (Mode Pie rebind, “Command Bar” on Space, etc.)
- Showing a shortcut on a button that only `toast("coming soon")`
- Global default chord shared by two `context: None` actions (Save As vs Symmetrize was a P0)
- Outliner / panel keys without focus gating when they collide with global actions

### Space semantics

- `ACT_CMD_BAR` / `ACT_CANCEL_DESELECT` = **Cancel / Deselect** (drop tool, clear sub-sel, deselect). Not the agent bar.
- Over outliner: Space = expand/collapse (global cancel deferred via `ol_pointer_over`).

### Tab / Mode pie

Mode Pie is **rebindable** via `ACT_MODE_PIE`. `raw_input_hook` strips that key (default Tab) so egui focus nav never steals it. Hold state uses `input_system.mode_pie_held`.

---

## 5. Placement & create paths

All user-facing **Add / Create** (Geometry menu, tools palette, Shift+A, outliner Add) must use the **same placement helper** (cursor / workplane / active node).  

**Ban:** `Vec3::ZERO` as the default create position for interactive “Add Box” style actions (origin-only ops must be explicit and labeled).

---

## 6. Chrome adoption

```text
✅ library_panel uses chrome seams
✅ dock L/R uses chrome seams / restore
✅ float uses chrome::draw_handles
❌ new ui2_drive_* copy of edge resize
❌ pane-local resize_handles
```

Constants: use `chrome::{SEAM_HIT, ZONE_SLOP, ZONE_STRIP, HEADER_H}` — do not invent `DOCK_ZONE_SLOP = 60` next door.

---

## 7. Menus (v1 + v2)

- **One filler** per command group (File open/save, Select all, create primitives).
- v2 shell and v1 `menus.rs` either call the same filler or v1 is frozen/deprecated for that item.
- Viewport context menu stays in a dedicated module — **do not paste** `draw_context_menu` into shell or outliner.

---

## 8. PR checklist (copy into PR body)

```
[ ] New panel / tool / op lives in its OWN file (or extracted if you touched a mega-match)
[ ] UI / menu / shortcut only CALLS that unit — no duplicated scene logic
[ ] No file over 500 lines without a unit-based split in this PR
[ ] No new dual source of truth (expand, filter, keybinds, resize)
[ ] New shortcuts: ACT_* + default + dispatch + label()
[ ] cargo test -p app_types --lib   (keybind clash tests)
[ ] cargo check -p app
[ ] powershell -File scripts/check_ops_gate.ps1   (or scripts/check_ops_gate.sh)
[ ] Creates use shared placement (no silent origin spawn)
[ ] Chrome used for resize/zone/float (no parallel path)
[ ] Named so a future node could wrap the same fn (verb-noun, clear module path)
```

---

## 9. Priority when unsure

1. **Correctness / SoT** over feature speed  
2. **Split file** before adding lines  
3. **Honest UI** (hide key or mark soon) over fake shortcut chrome  
4. **chrome/** reuse over local clever hit-testing  

---

## 10. Related docs

- `ui2/README.md` — layout map + health gates  
- Visual bible: `docs/UI/azra_app_mockup.html`  
- Branch: `feat/ui-polish` / worktree ui-polish only for this work  

**Last updated:** 2026-07-22 — multi-agent health review + P0 shortcut fixes.
