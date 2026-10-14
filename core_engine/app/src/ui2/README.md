# UI v2 module layout

**Primary rule: one unit = one file** (panel, tool, op, chrome primitive).  
Why: later **procedural / geometry nodes** will call the same entrypoints as menus, tools, and shortcuts.  
**Secondary:** ~500-line hygiene; prefer ≤300 for a single op.

**Shared chrome** (resize/zone/float) lives only in `chrome/`.

**Mandatory agent rules:** [`INTEGRATION_RULES.md`](INTEGRATION_RULES.md) — read before any ui2 PR.

```text
ui2/
  INTEGRATION_RULES.md ★ size limits, SoT, shortcuts, chrome, PR checklist
  mod.rs              orchestration + re-exports only
  types.rs            PanelKind, DockSide, PanelGroup, Act, Ui2State, OlCol
  chrome/             ★ REUSABLE primitives (import these — do not reimplement)
    collapse.rs         pane body/head collapse
    zone.rs             zone hide + preferred size + SLOP
    seam.rs             edge resize + restore strip (L/R/B/T)
    scroll.rs           panel scrollbar style
    frame.rs            pane paint, header, icon button
    float_resize.rs     8-handle floating resize
  draw/               ui2_draw · pies · placement · viewport_split (all ≤500)
  shell/              app_bar · ctxbar · shading · status (all ≤500)
  menus.rs            File…Help menu fillers (label ACT_*) — next split if grows
  dock.rs             side docks + floating (chrome seams + float handles)
  pane.rs             tab strip · fit heights · pane body
  tools/              catalog · palette · activate (dispatch only → ops)
  outliner/           types · build · dnd · input · actions · menu · mod (paint)

app/src/ops/          ★ scene ops — one file per unit (node-ready)
  sdf/                sphere, box_shape, cylinder, … draw_polyline
  poly/               invert, grow, select_*, mesh_create, …
  instance/ create    edit/ duplicate, frame, export, group
  bool_op/            set_union, set_subtract, cycle, …
```

## Reuse rule

| Need | Call |
|------|------|
| Resize dock / library edge | `chrome::draw_resize_seam` |
| Zone hide restore strip | `chrome::draw_restore_strip` |
| Pane collapse state | `chrome::PaneCollapse` |
| Scrollable body | `chrome::apply_panel_scroll_style` |
| Floating 8 handles | `chrome::draw_handles` |
| Pane surface paint | `chrome::paint_pane` |
| Expand/collapse tree | `ol_is_expanded` / `ol_set_expanded` / `ol_ensure_expanded` |

**Visual bible:** `docs/UI/azra_app_mockup.html`

**Branch / worktree:** `feat/ui-polish` only.

## Health gates (before plugging more)

Do **not** grow these until the matching health PR lands:

| Gate | Rule |
|------|------|
| **G1 Outliner** | New context/selection verbs go in `outliner/{menu,actions,input}.rs` — not bloating `mod.rs` past ~800. |
| **G2 Shell menus** | Menu items only in `menus.rs` with `label(ACT_*)`. |
| **G3 Collapse SoT** | Only via `ol_set_expanded` / `ol_ensure_expanded` (dual-writes `expanded_bodies`). |
| **G4 Chrome** | Resize / zone-hide / float handles only via `chrome/`. |
| **G5 Shortcuts** | New shortcut = `ACT_*` + `default_bind` + dispatch + `label()`. |
| **G6 Placement** | Creates use `ui2_world_under_cursor` (or shared place helper). |
| **G7 Tools** | Not-ready cmds show `soon`, never a fake key. |

## Status (2026-07-22 health pass)

- [x] INTEGRATION_RULES.md for agents
- [x] P0 shortcuts (Save As ≠ Symmetrize, Space honest, ACT_RENAME, outliner focus)
- [x] Dock/float → chrome seams + `draw_handles`
- [x] Outliner module split
- [x] Expand SoT helpers + find_selected
- [x] Menus extracted; poly/select labels from keybindings
- [x] Geometry create uses cursor placement
- [x] Tools: hide keys on not-ready rows
- [x] Mode pie true rebind (`ACT_MODE_PIE` drives raw strip)
- [x] InputSystem keyboard → KeyBindings ACT_* (scroll stays InputSystem)
- [x] Shared menu fillers (`menu_shared.rs` for File/Edit core)
- [x] God-file splits: `tools/` · `shell/` · `draw/` (each file ≤500)
- [ ] Next size targets if needed: menus.rs · pane.rs · dock.rs · outliner/mod.rs · types.rs
