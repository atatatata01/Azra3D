# Agent instructions — UI polish / UI2

When working on **ui2** (`AZRA_UI=v2`, path `app/src/ui2/`) or scene ops called from UI:

1. **Read first:** [`app/src/ui2/INTEGRATION_RULES.md`](app/src/ui2/INTEGRATION_RULES.md)
2. **Layout map:** [`app/src/ui2/README.md`](app/src/ui2/README.md)

## Primary design rule (more important than line count)

**One function / panel / tool / op = one file (or one clear leaf module).**

Why: later we will expose ops as **procedural / geometry nodes**. A node must call a single entrypoint (`ops::sdf::add_box`, `ops::poly::invert_selection`, …) without importing a god file.

| Do | Don’t |
|----|--------|
| New tool → new `tools/ops/<name>.rs` or `ops/<domain>/<name>.rs` | Add 40 lines into a mega-`match` in activate |
| New panel → own module; pane only dispatches | Paste panel body into `pane.rs` |
| Menu + shortcut + tool + future node → same `ops::` fn | Copy-paste scene logic into three UIs |
| Orchestrator files only wire calls | Put scene math in `ui2_draw` / shell |

**500 lines** is secondary hygiene (prefer ≤300 per leaf). Splitting by *unit* wins even if files are small.

## Other hard rules

3. **Shortcuts:** `ACT_*` in `app_types/src/keybindings.rs` + `keybindings.label()` in UI  
4. **Chrome:** resize / zone hide / float handles only via `ui2::chrome`  
5. **Expand tree:** `ol_is_expanded` / `ol_set_expanded` / `ol_ensure_expanded` only  
6. **Menus:** fillers call ops; do not re-grow monofiles  
7. **No** `mod.rs.god_backup` or parallel dock edge code  

PR checklist: bottom of INTEGRATION_RULES.md.

Branch / worktree: `feat/ui-polish` / ui-polish only for this surface.
