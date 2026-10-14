# INPUT_ARCHITECTURE_PLAN.md

## AZRA Contextual Remappable Keymap — Final Architecture

**Status:** Authoritative. The architecture decision for AZRA's input/keymap subsystem. Supersedes the CLAUDE.md "Input System Discipline" rule, which is **stale and must be rewritten as part of this work** (see §0.4 — Rule Conflict).
**Scope:** The keymap subsystem — `app_types/src/keybindings.rs` (`KeyBindings` / `ACTION_DEFS`), `app/src/input_system.rs` (`InputSystem`), the `handle_keyboard` dispatch ladder (`app/src/input_keyboard.rs`), the scroll chain (`app/src/input_viewport.rs`), live-drag modifier qualifiers, and the four keymap consumers (hint row, toasts, menus/tooltips, prefs UI). **NOT** in scope: the `.fl4` save format, `HalfEdgeMesh`, `NodePayload`, the ui2 dock work on `fix/ui`.

---

## 0. THE DECISION (read first)

### 0.1 The owner's requirement, verbatim

> "at the end shurcut need to be remapeble, with mouse context (if i press f in ouliner those not the same
> thing as pressing f in viewport, tool context (pressing f with the bevel tool active change some paramenter
> those no affect the ouliner etc) , or selection state contex ( if i press f in object mode that those also
> something different)"

### 0.2 The three-axis decode

| Axis | Name | Means | Owner's example |
|---|---|---|---|
| **A** | **SURFACE** | which panel/region the input belongs to | "f in outliner ≠ f in viewport" |
| **B** | **TOOL** | which modal tool is running | "f with the bevel tool active changes a parameter … does not affect the outliner" |
| **C** | **MODE** | work mode + selection state (Object vs Edit; Vertex/Edge/Face; SDF vs Poly vs Voxel) | "f in object mode … also something different" |

**All three must compose** (one binding may constrain any subset), and **all of it must be remappable** — including in-tool modal keys and held-modifier drag qualifiers.

### 0.3 The decision

**Chosen: ONE typed registry in `app_types`, keyed by a partial predicate `When`, with derived layering, structural modal capture, and a per-action `Guard` migration firewall.**

Six load-bearing choices, each traceable to evidence in §1/§2:

1. **MERGE, don't extend.** There are two registries today and neither can satisfy the spec: `KeyBindings` (96 actions, persisted, real editor) is **context-blind by type signature** — `matches(&self, i: &egui::InputState)`, `keybindings.rs:56`, no context parameter exists. `InputSystem` (19 actions, has an `InputContext` enum) has **zero persistence** (`grep save|load|serde|json input_system.rs` = 0) and is **invisible to the editor** (`win_settings.rs:131` iterates `ACTION_DEFS` only). `KeyBindings` survives; `InputSystem` contributes its `InputContext` *idea* and dies.
2. **Context lives on the ACTION, not on the binding.** Keeps the on-disk shape at `action_id → one KeyBind` — no composite key, no array-valued entries, no editor row explosion, no big-bang rewrite of ~80 `kb.pressed(...)` call sites. Per-tool remapping is achieved by **splitting the action id**, which every candidate design required anyway.
3. **The predicate is plain data — masks, never closures.** This is what makes "can these two bindings both be live?" *decidable*, and it is the single thing Blender structurally cannot compute (§1.5).
4. **Priority is DERIVED from the predicate, never declared and never statement order.** A declared layer that disagrees with the declared predicate is a second source of truth inside one struct.
5. **Modal capture is checked BEFORE the scan, not after.** A running modal tool owns the keyboard structurally. This is the one mechanism that makes the owner's example #2 true without tribal knowledge (§3.4 — and see §9.7 for the trap two candidate designs fell into here).
6. **`Guard::Legacy` / `Guard::Declared` is the migration firewall.** The un-migrated remainder stays **bit-identical to today**, and half-done is *typed* rather than a mystery (§8).

**The load-bearing principle (the one rule that governs everything below):**

> **The vocabulary is STATIC (typed, exhaustive, compiler-checked); the binding is DATA (remappable, persisted, diffed). A tool reads a semantic action id — never a key. There is no API that turns a string into a key glyph.**

### 0.4 ⚠️ RULE CONFLICT — reported per CLAUDE.md "Rule Conflict Check"

CLAUDE.md, *Input System Discipline*:

> "Every NEW keyboard/scroll/mouse action MUST be registered in `InputSystem` (`input_system.rs`). … Register the action, bind a default key+context, dispatch via `resolve()`. … Old actions migrate gradually — when touching an old keybind for any reason, migrate it to `InputSystem` then. Do not batch-migrate."

The rule has **two clauses with opposite verdicts**, and conflating them is what makes this look unresolvable.

**Clause 1 — "register in `InputSystem`" — STALE. Must be rewritten in the same commit that retires it.**
`InputSystem` has no persistence and is invisible to the rebind editor. **Obeying this rule produces actions that cannot be remapped — the exact inverse of the owner's spec.** Migrating an action *into* it strips remappability and orphans the user's `keybinds.json` entry. The empirical proof that the rule is already unfollowable: **11 of its 19 registrations are dead** — every prior migration stalled at "registered, but dispatch stayed hardcoded", because the target had nowhere to put the context the dispatch needed. Three comments promise *"honors a later user remap"* (`input_system.rs:305-311`, `:324-328`, `:343`); **no such remap can ever occur** — nothing writes `InputSystem.bindings` after startup. The rule was written when the file was greenfield: *"Start small — add Mode/Tool contexts when first needed"* (`input_system.rs:18`). **The owner's requirement IS the "when first needed."** Leaving the rule pointing at the dead registry is itself the No-Bandaids #3 failure ("next dev needs tribal knowledge → the fix is wrong").

**Clause 2 — "do NOT batch-migrate" — VALID, and honored.** The insight that dissolves the apparent conflict: **building the registry is not migrating the actions.** An action carrying `Guard::Legacy(when)` is advisory — `resolve()` skips it entirely (`let Guard::Declared(w) = def.guard else { continue };`) and its dispatch stays exactly where it is today. So all 96 actions import mechanically, the registry is provably a mirror, and **each action flips to `Declared` one at a time, on touch.** That is the rule's letter and its spirit.

**This needs owner ratification before S1 lands** (§10, D1).

---

## 1. THE REFERENCE MODEL — what Blender actually does

Blender's keymap is a 20-year-proven solution to *this exact requirement*. All data below is **dumped from live Blender 4.5.1 LTS or read from the v4.5.1 C++ source**, not from memory. Anything from memory is labelled inline as such.

**Provenance gotcha, worth recording:** in `blender -b` (background), `wm.keyconfigs` reports 104 keymaps but they are **lazily populated — only 30 items total**, so `bpy.ops.preferences.keyconfig_export(all=True)` is useless there. The authoritative dump comes from the preset data module `scripts/presets/keyconfig/keymap_data/blender_default.py` → `generate_keymaps(Params(...))`, which yields **247 keymaps / 2981 items**.

### 1.1 Blender has FIVE context axes, not three

| # | Blender mechanism | Owner's axis | Encoded as |
|---|---|---|---|
| 1 | `km.space_type` × `km.region_type` | **A. SURFACE** | Static fields on the keymap |
| 2 | `keymap->poll(C)` | **C. MODE** | A **C predicate** — *not* data |
| 3 | Modal keymaps on `win->modalhandlers` | **B. TOOL (running modal)** | A separate handler list, runs first |
| 4 | Tool-system dynamic keymap | **B. TOOL (sticky/active)** | Keymap resolved *by name* at event time |
| 5 | `keymap->owner_id` + workspace check | *(none)* | `wm_keymap.cc:471-478` |

**Axis B splits in two, and that split is real.** Blender uses two unrelated mechanisms: a dynamic keymap handler in the region list (`area.cc:1929-1938`) for the *sticky* tool, versus `win->modalhandlers` (`wm_event_system.cc:4203-4204`) for the *running* modal. AZRA already straddles both by hand — `tool.current.is_idle() && poly_tool == PolyTool::Idle` (`input_keyboard.rs:1116-1117`).

**Surface distribution** (from the dump):
```
space_type:  VIEW_3D:95  EMPTY:84  IMAGE_EDITOR:19  SEQUENCE_EDITOR:16  NODE_EDITOR:11 …
region_type: WINDOW:245  PREVIEW:1  TEMPORARY:1
```
`EMPTY:84` is the tell: **every mode keymap** (`Object Mode`, `Mesh`, `Sculpt`, `Pose`) is `space_type='EMPTY'`. Surface and Mode are **orthogonal fields on one struct**. A keymap declares whichever it needs — exactly the partial-predicate shape we adopt in §3.1.

### 1.2 MODE is a poll function — and that is Blender's most expensive mistake

```c
static bool object_mode_poll(bContext *C) {
  Object *ob = CTX_data_active_object(C);
  return (!ob || ob->mode == OB_MODE_OBJECT);
}
// object_ops.cc:323-327, wired at :338-339:
keymap = WM_keymap_ensure(keyconf, "Object Mode", SPACE_EMPTY, RGN_TYPE_WINDOW);
keymap->poll = object_mode_poll;
```

**Consequence: ALL mode keymaps are in the handler list simultaneously, always.** `view3d_main_region_init` (`space_view3d.cc:332-450`) adds ~28 keymaps unconditionally every time; mutual exclusion is *entirely* the polls' job. The code says so at `:359`: `/* pose is not modal, operator poll checks for this */`.

Because `keymap->poll` is an opaque `bool(*)(bContext*)`, Blender **cannot ask "which keymaps are live?"** without *running* every poll — so it needs a **second, hand-maintained** hierarchy (`keymap_hierarchy.py:10-17`) just to draw its own Preferences UI. **Two sources of truth that can drift.** This is the flaw we refuse in §3.1.

### 1.3 Resolution — NOT "first match wins"

Handler-list order, verbatim from `wm_event_do_handlers`:
```c
/* First we do priority handlers, modal + some limited key-maps. */
action |= wm_handlers_do(C, event, &win->modalhandlers);      // :4203-4204
```
then each gated by `if ((action & WM_HANDLER_BREAK) == 0)`: region (`:4260`) → area (`:4274`) → window (`:4289`).

Within a list, priority is **list position** — `WM_event_add_keymap_handler` uses `BLI_addtail` (`:4736`), so registration order *is* evaluation order. Modal handlers use `BLI_addhead` (`:2562`), so the newest modal is on top.

**The tool keymap outranks every mode keymap** — `ed_default_handlers` (which adds it, `area.cc:1929`) runs *before* `region->type->init` (`area.cc:2119-2125`). Confirmed by the intent comment at `space_view3d.cc:444`: `/* own keymap, last so modes can override it */`.

**THE RULE — and the distinction is load-bearing:**
```c
wmOperatorStatus retval = OPERATOR_PASS_THROUGH;   // wm_event_system.cc:1591
...
if (WM_operator_poll(C, ot)) { ... }               // :1599  — poll fails ⇒ retval stays PASS_THROUGH
...
if (retval & OPERATOR_PASS_THROUGH) return WM_HANDLER_CONTINUE;   // :2745-2757
```
> **The winner is the first item, in handler-list then item order, whose event matches AND whose keymap poll passes AND whose operator poll passes.** A matched binding whose poll fails **does not consume** — the scan continues.

This is what lets `Alt+LMB CLICK` be legitimately claimed by both `3D View:view3d.select` and `Mesh:mesh.loop_select`. **Implement "first match wins" and every shared chord becomes a defect.**

### 1.4 Modal keymaps — the propvalue indirection (the decisive datum)

Probed against live RNA:

| Modal map | `keymap_items` | `modal_event_values` |
|---|---:|---:|
| `Bevel Modal Map` | **0** | **17** |
| `Transform Modal Map` | **0** | **37** |
| `Knife Tool Modal Map` | **0** | **19** |

> **The semantic vocabulary is owned by the operator (compiled C, non-remappable). The key→semantic binding is a separate data layer (remappable). They are so separate that one is empty while the other is full.**

The operator's `modal()` reads `event->val == BEV_MODAL_VALUE_OFFSET`. **It never sees a key.** A remap therefore *cannot* desync a tool from its own logic.

**Real dump — `Bevel Modal Map`, 22 items** (this is the owner's example, shipped):
```
CANCEL <- ESC / RIGHTMOUSE          CONFIRM <- RET / NUMPAD_ENTER / LEFTMOUSE
VALUE_OFFSET <- A    VALUE_PROFILE <- P    VALUE_SEGMENTS <- S
SEGMENTS_UP <- WHEELUPMOUSE / NUMPAD_PLUS   SEGMENTS_DOWN <- WHEELDOWNMOUSE / NUMPAD_MINUS
OFFSET_MODE_CHANGE <- M     CLAMP_OVERLAP_TOGGLE <- C     AFFECT_CHANGE <- V
HARDEN_NORMALS_TOGGLE <- H  MARK_SEAM_TOGGLE <- U         MARK_SHARP_TOGGLE <- K
OUTER_MITER_CHANGE <- O     INNER_MITER_CHANGE <- I       PROFILE_TYPE_CHANGE <- Z
VERTEX_MESH_CHANGE <- N
```
Note **`C → CLAMP_OVERLAP_TOGGLE`** — AZRA already ships exactly this (`ACTION_BEVEL_TOGGLE_CLAMP`), and it is precisely where AZRA's model provably broke (§2.4).

**Proof that modal maps fully shadow the global keymap:** `Knife Tool Modal Map` must **re-declare `Ctrl+Z → UNDO`** as its own propvalue — because the global `ed.undo` is *unreachable* while it runs. Also in that map: `Z → Z_AXIS`, `C → CUT_THROUGH_TOGGLE`, `A → ANGLE_SNAP_TOGGLE`, `S → SHOW_DISTANCE_ANGLE_TOGGLE` — six collisions with the global/Mesh keymaps, zero conflict, because the modal list runs first and BREAKs.

### 1.5 The owner's F example is literally Blender's shipped behavior

**Bare `F`** (all 2981 items scanned):

| Keymap | Operator | Axis proven |
|---|---|---|
| **`Object Mode`** | *(unbound)* | — |
| **`Mesh`** (Edit) | `mesh.edge_face_add` | **MODE** |
| **`Outliner`** | *(unbound)* | **SURFACE** |
| `Sculpt` | `wm.radial_control(brush.size)` | **MODE** |
| `Node Editor` | `node.join_named` | **SURFACE** |
| `Clip Editor` | `clip.view_all(fit_view=True)` | **SURFACE** |

**`Ctrl+F` — a clean 6-way SURFACE collision, zero modifiers distinguishing them:**
```
Outliner        -> outliner.start_filter      <-- the owner's "F in outliner"
Mesh            -> wm.call_menu(VIEW3D_MT_edit_mesh_faces)   <-- the owner's "F in viewport"
Property Editor -> buttons.start_filter    File Browser -> file.start_filter
Node Editor     -> node.find_node          User Interface -> ui.list_start_filter
```

**F inside modal maps** (the owner's "F with the tool active changes a parameter"): `Primitive Tool Modal Map: F → CHANGE_RADIUS`, `Transform Modal Map: F → NODE_FRAME`, `Sculpt Expand Modal: F → INVERT`.

### 1.6 The hint bar is GENERATED from the keymap — this settles §7

`space_statusbar.py:8-15` is `layout.template_input_status()`. The bevel call site (`editmesh_bevel.cc`, `edbm_bevel_update_status_text`):
```c
WorkspaceStatus status(C);
status.opmodal(IFACE_("Confirm"),       op->type, BEV_MODAL_CONFIRM);
status.opmodal(IFACE_("Profile Shape"), op->type, BEV_MODAL_VALUE_PROFILE,
               opdata->value_mode == PROFILE_VALUE);   // 4th arg = highlight-if-active
```
> **`opmodal(label, operator_type, propvalue)` — the operator supplies a LABEL and a SEMANTIC ID, and never names a key.** The glyph is looked up from the live modal keymap by propvalue at draw time. Remap C→Q and the hint bar says Q with **zero code change**.

`ED_workspace_status_text(C, str)` — the hand-built-string legacy path — is the *old* way `WorkspaceStatus` replaced (tracked as #56599). **Do not build the legacy one.**

### 1.7 What Blender gets WRONG — the graveyard we refuse

| # | Blender flaw | Evidence | AZRA's structural answer |
|---|---|---|---|
| 1 | **Priority = the order of ~28 C statements**, defended only by comments | `space_view3d.cc:338` *"important to be before Pose keymap"*; `:444` *"last so modes can override it"* | Priority **derived** from the predicate in one pure function (§3.3). Reordering is not expressible. |
| 2 | **Zero conflict detection** | grep across `scripts/modules/`, `space_userpref.py`, `presets/keyconfig/`: the only "conflict" hits are **human comments** (`blender_default.py:76,433,4222,5221,6769`). Cost: **98 permanently-shadowed items**, **113 multi-claimed 3D-View signatures** | Decidable, because axes are data (§6.4) |
| 3 | **MODE is an opaque poll** ⇒ needs `keymap_hierarchy.py`, a **second hand-maintained tree**, to draw its own prefs UI | `keymap_hierarchy.py:10-17` | Masks. One table drives dispatch, editor, and hints. |
| 4 | **`any`-modifier is a blunt shadow** | **272 of 286** uses are inside modal maps; only **14** in ~2700 non-modal items — Blender's own data condemns it | Gate `any` to modal contexts only |
| 5 | **Dual disable/remove mechanism** | `kmi.active` (`rna_keymap_ui.py:139`) vs `keyitem_remove`; a disabled item still occupies its slot | ONE mechanism: `unbind` |
| 6 | **String-named keymaps** — a typo silently creates a new EMPTY keymap | `WM_keymap_ensure(conf, "Bevel Modal Map", ...)`; caught only by a runtime `CLOG_WARN` with a hardcoded allowlist, `wm_keymap.cc:480-496` | Typed IDs — a compile error. Per CLAUDE.md *"Stable IDs, Not Indices"*. |

### 1.8 What Blender gets RIGHT and we copy verbatim

- **The diff-based user layer.** Verbatim rationale, `wm_keymap.cc:594-602`: *"Rather than saving the entire keymap for user preferences, we only save a diff so that changes in the defaults get synced."* Applied as `runtime = copy(default) + addon(addhead) + patch(user)` (`wm_keymap_patch_update`, `:743-812`).
- **Poll pass-through** (§1.3).
- **Modal propvalue indirection** (§1.4).
- **Generated hint bar** (§1.6).
- **A dead binding is inert, not fatal.** `WM_operatortype_find(kmi_idname, false)` → `nullptr` → the branch is skipped → `retval` stays `PASS_THROUGH` → the next matching item gets a chance (`wm_event_system.cc:2683`). **Never `unwrap()` an action lookup.**

---

## 2. WHERE AZRA IS TODAY

### 2.1 Two registries, mutually blind, and the mandated one is the dead one

| | `KeyBindings` (`app_types/src/keybindings.rs`) | `InputSystem` (`app/src/input_system.rs`) |
|---|---|---|
| Actions | **96** (`ACTION_DEFS:209-319`) | **19** (`:251-376`) — **11 dead** |
| User-rebindable | **YES** (`win_settings.rs:84-180`) | **NO** — editor never reads it |
| Persisted | **YES** — `~/.azra/keybinds.json` v2 | **NO** — `grep save\|load\|serde\|json` = **0** |
| SURFACE axis | **absent** | present (`InputContext`, `:20-31`) |
| TOOL axis | free-text prose only | **absent** |
| MODE axis | free-text prose only | **absent** |
| Dispatch | ~80 `kb.pressed(...)` sites | 8 live via `binding_for()`; **`resolve()` has ONE call site app-wide** |

They already collide on **`"clip_curve_toggle"`**, registered in **both** (`keybindings.rs:188` and `input_system.rs:276`) and dispatched only from the former. Remap Clip Curve in the editor and `InputSystem`'s copy stays at `C` **forever**. That is a live *"Two code paths for the same operation — they MUST share one path"* violation. Worse: **that dead entry is the sole reason `bevel_toggle_clamp` needs its 6-line raw-`push` bandaid** (`input_system.rs:306-320`) — a dead entry in a dead registry forcing a bandaid in a live one.

**`resolve()` is not a dispatcher and cannot become one.** `bind_default()` silently drops on `(event, mods, context)` collision **without comparing action_id** (`:161-164`). Three actions therefore sit on `X+NONE+Viewport` — `sculpt_brush_size_up` (`:283`), `poly_boolean_pie` (`:330`), `sdf_boolean_pie` (`:348`). Since `resolve()` is first-match-wins over a `Vec`, **it can only ever return `sculpt_brush_size_up`; both boolean pies are permanently unreachable through it.** All 7 real consumers had to invert to `binding_for()` (action → key → hand-rolled compare), copy-pasted verbatim at `:45, :61, :580, :598, :1078, :1104, :1193`.

### 2.2 Raw vs registered — the sizing

| Category | Count | InputSystem-dispatched | Rebindable today |
|---|---:|---:|---:|
| `KeyBindings` `ACT_*` | 96 | 0 | **96** |
| `InputSystem` registered | 19 (18 unique) | 8 live / 11 dead | **0** |
| Raw literals — input handlers | 39 | 0 | 0 |
| Scroll-chain branches | 15 | 1 | 0 |
| Live-drag qualifiers | ~12 | 0 | 0 |
| **TOTAL (viewport/global)** | **~169** | **8 (4.7 %)** | **104 (61.5 %)** |
| *Dialog-local Esc/Enter (separate category, out of scope)* | *+29* | *0* | *0* |

**Axis coverage across all ~169:**
- **A / SURFACE: 3 sites (1.8 %)** — `input_keyboard.rs:1089`, `:1118`, `:1205`, all `== InputContext::Viewport`, all pies. And the detector itself is guesswork: `active_input_context()` derives Outliner/Properties/Library from **hardcoded screen fractions** (`mouse.x > screen.width()*0.75 && mouse.y < screen.height()*0.5`, `input_system.rs:232-243`). Only `viewport_rect` is a real rect — **it is the only panel rect stored anywhere in the codebase** (`types_viewport.rs:153`).
- **B / TOOL: ~162 ad-hoc `tool.current.is_*` guards.** Zero declared.
- **C / MODE: ~100 ad-hoc guards** across four unsynchronized fields. Zero declared.

**The three axes already exist — as ~265 hand-written boolean guards spread across two ~1,400-line functions, plus 16 free-text strings that lie to the conflict detector.** This work is not "add context." It is **"extract the context that is already there, and give it a type."**

### 2.3 The conflict map: **ACCIDENTAL, not structural** — verdict and proof

**36 of 96 actions (37.5 %) share a chord with another action**; 15 chords collide in the *shipped defaults*: `E`×3, `A`×3, `B`×3, `C`×3, `G`, `S`, `I`, `F`, `H`, `Q`, `Delete`, `Ctrl+I`, `Ctrl+X`, `Ctrl+D`, `Ctrl+Shift+S`, `Alt+M`. They work today **only because the `else if` ladder disambiguates them**. Collision is the norm, not the exception.

**At least 10 are reachable right now.** The destructive one:

> **C-1 — `Ctrl+Shift+S` fires Save-As AND silently symmetrizes the mesh.** `ACT_SAVE_AS => ctrl_shift(S)` (`keybindings.rs:354`) dispatched at `input_keyboard.rs:377`; `ACT_SYM_SYMMETRIZE => ctrl_shift(S)` (`:367`) dispatched at `:720`. Both `context: None`. Both run the same frame. **This is priority-ladder rung 1/3 and arguably warrants a standalone hotfix ahead of this entire plan** (§10, D6).

Three independent proofs that disambiguation is accidental, each sufficient:

1. **The guard exists on one site and was forgotten on its twin.** `input_keyboard.rs:288` computes `poly_sub_mode` and guards H with `&& !poly_sub_mode`, commented *"(skip when poly sub-mode handler already covers it)"*. The same guard was **forgotten on `Ctrl+I`** (`:1342`) → **C-5**: in Poly Vertex mode, `Ctrl+I` inverts the selection **and creates an instance**. Root cause: `poly_switch_work_mode()` (`poly_selection.rs:1089-1129`) **never sets `tool.work_mode`**, so `tool.work_mode == Object` is TRUE while `poly_work_mode == Vertex`, and both guards pass. If disambiguation were structural, no site could forget.
2. **The API admits it drops bindings, and the code works around its own API three times.** `input_system.rs:306-320`, `:324-336`, `:343-354` each bypass `bind_default()` with a raw `bindings.push(...)` plus a paragraph of apology.
3. **Two of nine poly handlers have an `is_idle()` gate; seven do not** (`poly/key_dispatch.rs:106`, `:128` have it). The seven are bugs waiting for a modal to be held open — e.g. **C-9**: `is_poly_slide()` has **no arm** in the `input_keyboard.rs:400-645` ladder, so during a live edge-slide the `else` branch runs and pressing `E` starts an extrude on top of the slide.

**Every "disambiguator" in this codebase is one of: (a) the order of an if/else chain, (b) a hand-written state guard replicated per site, or (c) two states that happen to be exclusive today. Not one is enforced by construction.**

**And there is no consumption model at all.** Verified: zero uses of `consume_key`/`consume_shortcut` in any input path. The only mechanism is the bespoke Tab `raw_input_hook` strip (`app_update.rs:21-33`) — one key, hand-built, non-generalising. Zones 2 and 3 of `handle_keyboard` both read the same immutable `ctx.input()` snapshot and both fire in the same frame. **This is the single biggest regression risk in the port** (§8, R1).

### 2.4 Per-tool remapping is structurally IMPOSSIBLE today — the minimal proof

`ACTION_BEVEL_TOGGLE_CLAMP` (`input_system.rs:51`) is consumed by **two different tools**:
```rust
// input_keyboard.rs:581  — inside  if self.tool.current.is_poly_bevel()
// input_keyboard.rs:599  — inside  else if self.tool.current.is_poly_loop_cut_slide()
.binding_for(crate::input_system::ACTION_BEVEL_TOGGLE_CLAMP, InputContext::Viewport)
```
**One action id, two tools.** Rebinding "bevel clamp" *necessarily* rebinds loop-cut clamp. The owner's per-tool requirement cannot be expressed until this id splits. And a **third** clamp site bypasses the binding entirely — `input_viewport.rs:1352` is a raw `key_pressed(egui::Key::C)` — so remapping clamp today fixes 2 of 3 sites and silently leaves the slide clamp on `C`.

### 2.5 The owner's F example is a shipping bug — twice over

**(a) No surface check.** `input_keyboard.rs:828` is `if f && !self.ui.f_pie.open && !self.tool.current.is_transform()`. Gated on tool state only. The outliner has **no F handler at all**. ⇒ **Pressing F over the outliner opens the viewport F pie.** Exactly as reported.

**(b) Remapping Focus makes the F pie permanently unreachable.** Verified verbatim:
```rust
// input_keyboard.rs:827 — the HOLD is hardcoded:
let f_down = ctx.input(|i| i.key_down(egui::Key::F) && !i.modifiers.command && !i.modifiers.alt);
// input_keyboard.rs:828 — but the OPEN is remappable (f = kb.pressed(ACT_FOCUS)):
if f && !self.ui.f_pie.open && !self.tool.current.is_transform() {
```
Rebind Focus → V: the pie opens on V, `f_down` is instantly false, `release()` fires the same frame (`:839`) → `PieResult::QuickTap` → `frame_selected()`. **Hold-to-open degrades to instant-tap and the radial menu becomes unreachable.** *The very remappability the owner is asking for is what breaks it* — caused by exactly one duplicated piece of key knowledge. Same latent class at `:782-783` (backtick view pie, in *neither* registry, so not remappable at all).

**(c) F double-fires in Poly Vertex mode.** `ACT_POLY_FILL => simple(F)` (`keybindings.rs:408`) dispatched at `poly/key_dispatch.rs:97`, **and** the F pie at `:828`. Press F on boundary verts → a face is created **and** the pie opens.

### 2.6 The free-text pseudo-axis — already the right vocabulary, with no type and no teeth

`ActionDef.context: Option<&'static str>` (`keybindings.rs:206`). **16 distinct strings, 52 of 96 actions carry one.** Sample: `"Polygonal edit mode"`×11, `"Object mode"`×5, `"Poly Vertex mode"`×4, `"Active tool"`×4, `"During creation"`×3, `"Poly Vertex/Edge mode"`×1, `"Footprint phase"`×1; **44 are `None`**.

**This is the owner's B and C axes, already enumerated, in free text.** It is also decorative — the dispatcher never reads it — and it **corrupts conflict detection**:
```rust
// keybindings.rs:485
if def.context.is_some() && excl_ctx.is_some() && def.context != excl_ctx { continue; }
```
String equality on prose ⇒ **false positives** (`context: None` conflicts with everything — 9 of the 15 default collisions) **and false negatives** (`"Poly Face mode"` vs `"Polygonal edit mode"` are both live in Face mode but compare unequal → a real collision is silently suppressed). Note the strings contain **disjunctions** (`"Poly Vertex/Edge mode"`) and **containment** (`"Polygonal edit mode"` ⊃ `"Poly Face mode"`) — **one enum can express neither.** That settles the mask decision in §3.1.

Also: `find_conflicts` is called **only** from the rebind UI (`input_keyboard.rs:79`). **It has never once examined the shipped defaults**, and conflicts are advisory — the bind is applied regardless (`:79-82`).

### 2.7 `PolyKeyFlags` — the partial-migration status

**Verdict: NOT a context system, NOT the intended pattern. It is a borrow-checker workaround. Do not generalize it.**

- **Definition:** `app_types/src/types_poly.rs:445-471` — a flat 25-field `struct PolyKeyFlags { extrude: bool, inset: bool, … }`. Sibling `NurbsKeyFlags` (`:473-479`).
- **Why it exists — the comments say it outright:** *"Evaluate all tool keybinds upfront to avoid borrow conflicts"* (`input_keyboard.rs:110`) and *"Pre-evaluate poly/NURBS contextual keybinds to avoid borrow conflicts"* (`:136`). `kb` is `&self.keybindings`; calling `self.poly_*()` needs `&mut self`. **The struct exists to drop the borrow, nothing more.**
- **Set from `KeyBindings`, not `InputSystem`** (`:137-163`) — and two fields bypass even that: `confirm_bevel: i.key_pressed(egui::Key::Enter)` (`:160`), `cancel_bevel: i.key_pressed(egui::Key::Escape)` (`:161`).
- **Context enforcement is ad-hoc per field** (`poly/key_dispatch.rs:9-135`): one function-level MODE guard (`:10`), **11 different hand-written `poly_work_mode` comparisons**, a TOOL guard on only 2 of 25 fields, and **zero SURFACE guards**. `kf.delete_face` fans out on mode *inside* the handler (`:86-93`) — one action id, three meanings.
- **"Phase 3 contextual keybinds"** (`:8`) refers to the *KeyBindings* prose rollout, **not** an `InputSystem` migration.

**So it is a partial migration of the MODE axis only, hand-rolled, with no surface axis and near-zero tool axis.** It is *evidence the axes are needed* — not a template. It has **no marker distinguishing "migrated" from "forgotten"**, which is precisely the fossil property §8 exists to prevent.

### 2.8 The drifted mirrors — eleven, three of them pure fiction

**`viewport_hints.rs` is ON MASTER.** Verified: `git merge-base --is-ancestor 58ccee1 HEAD` → **ancestor**; the file is tracked. *(This corrects the recon, which reported it as living in a sibling worktree. The "two agents editing the same hint row" collision risk that every candidate design and two judges flagged is **stale — it does not exist**. The hint-row slice rewrites one file on master with no coordination.)*

It is the best exhibit in this document. Line 2 calls itself *"THE single source of truth for the contextual viewport hint row."* Line 17 then declares:

> `// INVARIANT: every key listed here is verified against the real dispatch site. A wrong hint is worse`
> `// than none. When you add/rebind a key for a modal tool, update the matching arm here in the same task.`

**That is CLAUDE.md's No-Bandaids test #3 written down as a comment** — *"Will the next dev need tribal knowledge to avoid breaking it? If yes, the fix is wrong — enforce correctness structurally."* It improves the *shape* (pairs, not prose) and it fixed two real drifts, but it is a hand-written mirror of 26 arms and **it still ships two fictions**:

| Mirror | Claim | Reality |
|---|---|---|
| `viewport_hints.rs:154` | `("[/]", "Brush size")` | **`grep OpenBracket\|CloseBracket` across `core_engine`: ZERO hits.** Real binding is **Z/X** — and `input_system.rs:282-283` **already declares the truth** (`sculpt_brush_size_down/up`). The declaration exists; the hint row states the opposite. |
| `viewport_hints.rs:157` | `("X", "Cycle bool")` | Behavior **deleted at 777a56b**. `input_system.rs:58-59`: the pie *"replaces the old blind Union→Subtract→Intersect cycle."* |
| `ui_panels.rs:501` | `"F3: Command palette"` | **`grep Key::F3`: ZERO hits.** It's `Ctrl+Shift+P`. |
| `menus.rs:472`/`:480` vs `:861` | "Duplicate Ctrl+D" vs "Duplicate Shift+D" | **The same file contradicts itself.** `:861` is right; `Ctrl+D` is `ACT_POLY_SUBDIVIDE`. |
| `workspace.rs:455` | `.on_hover_text("Ctrl+Y")` for Redo | `ACT_REDO => ctrl_shift(Z)`. **Wrong at DEFAULTS — no remap needed.** |
| `win_settings.rs:19-29` | `"X/Y/Z=Lock axis"` | Two-thirds false — X/Y are raw literals (`input_keyboard.rs:95`), not bindable |
| `poly_drag_tools.rs:899` | `"Bevel: drag=width, SHIFT+drag=shape, scroll=segments, C=clamp, LMB/Enter=confirm"` | Duplicates the bevel hint arm verbatim in different prose |
| `viewport_overlays.rs:642` | `vec![("Duplicate", kb.label(ACT_DUPLICATE)), ("Box Select", "Shift+Drag".into())]` | **Half declaration, half literal in ONE expression** — because `KeyBind` cannot express a drag |

Plus ~22 hardcoded toasts across 19 files, and `menus.rs:702, 703, 717, 861, 880, 885`.

**The cure already exists and is simply not universal:** `menus.rs` uses `kb.label(ACT_*)` **correctly 15 times** (`:15, :24, :33-34, :95-101, :106, :151-155`). That is the drift-proof precedent §7 generalizes.

### 2.9 The event vocabulary gap — the product cost is already being paid

`InputEvent` is `Key | ScrollUp | ScrollDown` (`input_system.rs:40-44`). No mouse buttons (the doc comment concedes *"and (future) mouse buttons"*), no press/release, no click, no double-tap, no drag. `KeyBind` cannot even express scroll. Blender's `wm_eventmatch` matches type × **val** × direction × 5 modifiers × **key_modifier** (`wm_event_system.cc:2319-2400`).

The cost is not theoretical — it is a **shipped compromise**, `keybindings.rs:396`:
```rust
ACT_POLY_EDGE_SLIDE => KeyBind::shift(E), // Blender edge slide is GG (double-tap); Shift+E here
```
**Blender parity was abandoned because the data model cannot hold a double-tap.**

---

## 3. THE CHOSEN ARCHITECTURE

### 3.1 The context model — one predicate, plain data

Lives in **`app_types/src/keymap/`**. Verified enabling facts: `app → app_types` is the existing one-way dependency (`app/Cargo.toml:13`); `app_types/Cargo.toml` depends on core_math/graphics/eframe/egui/glam/**serde**/**serde_json**/indexmap/**dirs** and has **no `app` dep**; `ActiveTool`/`ModelingContext`/`WorkMode`/`PolyWorkMode` already live here (`types_tool.rs:41,19`; `types_poly.rs:12,140,24`). **Zero new deps, zero dependency inversion.**

```rust
// ── app_types/src/keymap/ctx.rs ──────────────────────────────────────

/// Stable serialized identity. Never an index. (CLAUDE.md "Stable IDs, Not Indices")
pub type ActionId = &'static str;   // keeps the existing ACT_* consts and the on-disk key

// ── Axis A: SURFACE ──────────────────────────────────────────────────
/// PUSHED by the panel that drew it — never guessed from screen fractions.
/// Replaces active_input_context() (input_system.rs:221-247).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] #[repr(u8)]
pub enum Surface { Unknown = 0, Viewport, Outliner, Properties, Library, ShaderGraph }

// ── Axis B: TOOL — data-only discriminant of ActiveTool's 26 payload-carrying variants
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] #[repr(u8)]
pub enum ToolId {
    Idle = 0, Transform, Gizmo, Create, SmartMirror, PivotEdit, SelectionBox,
    FacePushPull, Bevel, BoolEdgeBevel, FalloffAdjust, BlendKDrag,
    PolyExtrude, PolyInset, PolyBoxSelect, PolyCircleSelect, PolyTransform,
    PolyBevel, PolyLoopCutSlide, PolyVertSlide, PolyEdgeSlide, PolyShrinkFatten,
    NurbsCvMove, ClipCurveDraw, CurvePointTransform, CurveBevelTransform,
    Pie(PieId),                       // pies ARE modal tools — see §3.4
}

// ── Axis C: MODE — THREE sub-fields, NOT one flattened enum ──────────
// Justified by SHIPPED data, not taste: §2.6's strings already contain DISJUNCTIONS
// ("Poly Vertex/Edge mode") and CONTAINMENT ("Polygonal edit mode" ⊃ "Poly Face mode").
// One enum expresses neither.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] #[repr(u8)]
pub enum Pillar  { None = 0, Sdf, Polygonal, Voxel, Nurbs, Curve, Light, Camera, ImportedMesh }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] #[repr(u8)]
pub enum WorkKind { Object = 0, Edit, Sculpt }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] #[repr(u8)]
pub enum ElemMode { NA = 0, Vertex, Edge, Face }
```

Masks are hand-rolled (no `bitflags` dep) via one macro; `ANY` = all bits:

```rust
mask!(SurfaceMask, Surface, u16);  mask!(ToolMask, ToolId, u32);   // 26 variants fit
mask!(PillarMask, Pillar, u16);    mask!(WorkMask, WorkKind, u8);  mask!(ElemMask, ElemMode, u8);
// each gets: ANY, only(e), or(), has(e), intersects(o), is_any()

/// THE composite predicate. All three axes compose because they are independent
/// fields of ONE struct, matched conjunctively.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct When {
    pub surface: SurfaceMask,
    pub tool:    ToolMask,
    pub pillar:  PillarMask,
    pub work:    WorkMask,
    pub elem:    ElemMask,
}

impl When {
    /// == today's InputContext::Global, and == the 44 actions with `context: None`.
    /// `None` now means ANY (correct) instead of "conflicts with everything" (today's bug).
    pub const ANY: Self = Self { surface: SurfaceMask::ANY, tool: ToolMask::ANY,
                                 pillar: PillarMask::ANY, work: WorkMask::ANY, elem: ElemMask::ANY };

    /// Conjunctive. THIS IS THE COMPOSITION — the owner's requirement in one expression.
    #[inline]
    pub fn accepts(&self, c: &Ctx) -> bool {
        self.surface.has(c.surface) && self.tool.has(c.tool)
            && self.pillar.has(c.pillar) && self.work.has(c.work) && self.elem.has(c.elem)
    }

    /// Can two predicates be satisfied simultaneously? PURE ARITHMETIC — decidable
    /// because every axis is data. This is what Blender structurally cannot compute
    /// (§1.7 flaw 3) and what `keybindings.rs:485` gets wrong with free-text `!=`.
    pub fn overlaps(&self, o: &Self) -> bool {
        self.surface.intersects(o.surface) && self.tool.intersects(o.tool)
            && self.pillar.intersects(o.pillar) && self.work.intersects(o.work)
            && self.elem.intersects(o.elem)
    }
}
```

**The live snapshot** — the ONE place the four unsynchronized mode fields are read:

```rust
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub surface:    Surface,
    pub tool:       ToolId,
    pub tool_modal: Option<ToolId>,   // Some iff a modal drag owns the keyboard
    pub pillar:     Pillar,
    pub work:       WorkKind,
    pub elem:       ElemMode,
    pub mods:       Mods,             // live, for qualifier level-sampling (§5)
    pub text_focus: bool,
}
```

`build_ctx(&AzraApp) -> Ctx` is the single reader of `tool.current` / `modeling_context` / `tool.work_mode` / `poly_work_mode` / `sculpt.voxel`. Today those are re-derived at ~265 hand-written sites and **they lie to each other** (§2.3, C-5). Building `Ctx` in one function makes that lie fixable in one place. It carries a `debug_assert!(coherent(...))` — per CLAUDE.md *"Downgrading assert→log is ALWAYS a bandaid"*, if it fires we fix `poly_switch_work_mode()`, we never soften the assert.

**The `ActiveTool → ToolId` bridge is exhaustive — no `_` arm, ever:**
```rust
impl ToolId {
    pub fn of(t: &ActiveTool) -> Self {
        match t {                                   // NO wildcard. A 27th variant = COMPILE ERROR.
            ActiveTool::Idle           => Self::Idle,
            ActiveTool::PolyBevel(_)   => Self::PolyBevel,
            ActiveTool::PolyVertSlide(_) => Self::PolyVertSlide,
            /* ...all 26... */
        }
    }
}
```
This is the **structural fix for C-9 and C-8**: today `is_poly_slide()` has no arm in the ladder and `is_gizmo() != is_transform()`, so global keys leak into live drags. Here, forgetting to classify a new tool cannot compile.

### 3.2 The event vocabulary — widened ONCE, now

Retrofitting `val` later re-touches every binding and forces a **second** format migration. §2.9 shows the cost is already being paid.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Input { Key(KeyId), Mouse(MouseBtn), Wheel(WheelDir) }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Val { Press, Release, Hold, Click, DoubleClick, DoubleTap, Drag }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EventSig { pub input: Input, pub val: Val, pub mods: Mods }

/// Level-sampled, NOT edge-triggered. The home for drag modifiers (§5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct QualSig { pub mods: Mods, pub key: Option<KeyId> }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Trigger { Event(EventSig), Qual(QualSig) }
```

`Val::Hold` is **required**, not luxury — it is the fix for the F-pie bug (§2.5b). `Val::DoubleTap` stops paying the `GG`-edge-slide tax (§2.9).

### 3.3 The declaration shape — `Guard` on the ACTION

The prose `context` field is **REPLACED, not supplemented** — it is a live defect that lies to `find_conflicts` (§2.6), so porting it forward would carry the lie forward.

```rust
// ── app_types/src/keymap/decl.rs ────────────────────────────────────
pub enum Guard {
    /// Context enforced by hand-written ifs at the dispatch site. `pressed()` is the correct
    /// verb. The When is ADVISORY: it feeds find_conflicts + the editor; resolve() IGNORES it.
    /// THIS IS THE MIGRATION LEDGER — `rg 'Guard::Declared' | wc -l` is the progress bar.
    Legacy(When),
    /// Context enforced by resolve(). `pressed_in()` is the correct verb; `pressed()`
    /// debug_asserts. The When is authoritative and CANNOT drift from dispatch.
    Declared(When),
}
impl Guard { pub fn when(&self) -> When { match self { Guard::Legacy(w) | Guard::Declared(w) => *w } } }

pub struct ActionDef {
    pub id:       ActionId,        // UNCHANGED — serialized in keybinds.json, never rename blind (§6.5)
    pub name:     &'static str,    // UNCHANGED — the ONLY human string. Never a glyph.
    pub category: &'static str,    // UNCHANGED
    pub guard:    Guard,           // REPLACES `context: Option<&'static str>`
    pub hint:     Hint,            // NEW — drives the generated hint row (§7)
    pub avail:    Avail,           // NEW — the narrow escape hatch, §3.5
}
pub enum Hint  { None, Row(u8) }   // Row(order): ~115 actions can match a ctx; a row shows 4-7
pub enum Avail { Always, Poll(PollId) }
```

**Why context on the ACTION and not the BINDING** — the highest-leverage decision in this plan. Once ids split per tool (which every candidate design required anyway), context-on-action and context-on-binding are *the same expressive power*, and context-on-action is dramatically cheaper: the disk key stays `action_id → one KeyBind` (no composite key, no array-valued entries, no v3 shape break beyond the key encoding), `KeyBindings.map: HashMap<&'static str, KeyBind>` survives, and all ~80 `kb.pressed(id, i)` sites keep compiling. `bevel.clamp_toggle` and `loopcut.clamp_toggle` become **two actions, one bind each, one editor row each, both defaulting to `C`, with zero conflict by mask arithmetic** (`MODAL_BEVEL & MODAL_LOOPCUT == 0`).

**The two verbs share ONE binding table** — this is the crux. They are *not* two sources of truth for the key; they read the same `map` and differ **only** in whether the predicate is enforced:

```rust
impl KeyBindings {
    /// LEGACY. Context-blind. Today's exact semantics. ~80 call sites keep compiling.
    pub fn pressed(&self, id: ActionId, i: &egui::InputState) -> bool {
        #[cfg(debug_assertions)]
        if let Some(d) = ACTION_DEFS.iter().find(|d| d.id == id) {
            debug_assert!(!matches!(d.guard, Guard::Declared(_)),
                "{id} is Declared — use pressed_in(). A straggler call site was missed.");
        }
        self.get(id).matches(i)          // unchanged body
    }
    /// MIGRATED. Enforces the declared When.
    pub fn pressed_in(&self, id: ActionId, i: &egui::InputState, c: &Ctx) -> bool {
        let Some(d) = ACTION_DEFS.iter().find(|d| d.id == id) else { return false };
        d.guard.when().accepts(c) && self.get(id).matches(i)
    }
}
```
In **release**, `pressed()` on a `Declared` action degrades to today's context-blind behavior — never to broken. A missed straggler is a dev-time assert, not a user-facing regression. That is why the `debug_assert` is a real structural guard and not a downgraded one.

### 3.4 Modal capture — `Capture::Exclusive`, checked BEFORE the scan

**This is the single most important correction in this plan, and it repairs a flaw that was present in more than one candidate design.**

The tempting shape is: scan everything, then if nothing matched and a modal is live, swallow. **That is wrong.** An action declaring `when: When::ANY` — and §3.6 makes all 44 `context: None` actions exactly that — has `When::ANY.accepts(ctx) == true` **during a bevel drag**, because `ToolMask::ANY` contains the `PolyBevel` bit. So `Fire(save)` returns *before* any post-scan swallow check runs. A post-scan swallow only catches events that matched **nothing**.

The only escape from that is to require all ~96 actions to individually declare a narrow `tool` mask — which is tribal knowledge, i.e. CLAUDE.md No-Bandaids #3 ("if the next dev needs tribal knowledge, the fix is wrong"). **Capture must gate the scan, not follow it.**

```rust
#[derive(Clone, Copy, PartialEq, Eq)] pub enum Capture { Exclusive, Additive }

impl ToolId {
    /// EXHAUSTIVE — no `_` arm. A new tool must declare its capture or it will not compile.
    /// Mirrors Blender: a modal operator's modal() sees EVERY event and returns RUNNING_MODAL
    /// (= WM_HANDLER_BREAK), so it swallows unmatched keys (§1.4).
    pub const fn capture(self) -> Capture {
        match self {
            Self::PolyBevel | Self::PolyInset | Self::PolyExtrude | Self::PolyLoopCutSlide
            | Self::PolyShrinkFatten | Self::PolyVertSlide | Self::PolyEdgeSlide
            | Self::Transform | Self::Create | Self::ClipCurveDraw | Self::SmartMirror
            | Self::Pie(_)                                    => Capture::Exclusive,
            Self::Idle | Self::Gizmo | Self::SelectionBox
            | Self::PolyBoxSelect | Self::PolyCircleSelect /* … */ => Capture::Additive,
        }
    }
}
```

**Two properties this buys, both structural:**

1. **The owner's example #2 is true by construction** — F with bevel active cannot reach the outliner, without any action declaring anything about bevel.
2. **It removes the `tool: IDLE` discipline entirely**, which is what makes the derived layering in §3.3′ sound (see below). Actions can honestly declare `when.tool = ANY` and still not fire during a modal.

It also **deletes the 17-site `!self.ui.z_pie.open && !self.ui.tab_pie.open && !self.ui.view_pie.open && !self.ui.f_pie.open && !self.ui.d_pie.open` chain** across `input_keyboard.rs` / `tool_mode.rs` / `viewport_overlays.rs` — a hand-rolled reimplementation of Blender's one line. Every new pie today means editing all 17 sites; here a pie is `ToolId::Pie(PieId)` with `Capture::Exclusive` and the chain has nowhere to live. Direct discharge of *"Two code paths for the same operation — they MUST share one path."*

**Safety rail, non-negotiable:** every `Capture::Exclusive` tool MUST bind Cancel — enforced by a test over all tools, **not by review**. Per CLAUDE.md's Feature Dependency Audit, *"Selection must ALWAYS work. Breaking click-to-select is a P0 regression"*; a modal that swallows Escape strands the user.

### 3.3′ Layer is DERIVED, never declared

A declared layer that disagrees with the declared `When` is a second source of truth *inside one struct*. Derive it:

```rust
#[derive(PartialEq, Eq, PartialOrd, Ord)] pub enum Layer { Tool, Mode, Surface, Global }

pub fn layer_of(w: &When) -> Layer {
    if !w.tool.is_any()                                  { Layer::Tool }
    else if !w.pillar.is_any() || !w.work.is_any() || !w.elem.is_any() { Layer::Mode }
    else if !w.surface.is_any()                          { Layer::Surface }
    else                                                 { Layer::Global }
}
```
This is Blender's region-handler order expressed as **a pure function of the declaration** instead of the order of ~28 C statements (§1.7 flaw 1). Reordering it is a diff to one function with a failing test, not a silent keymap change. **Note there is no `Modal` layer** — modal is handled by `Capture` gating the scan (§3.4), which is precisely what keeps this derivation from collapsing.

### 3.5 The resolution algorithm — numbered

```rust
pub enum Outcome { Fire(ActionId), Swallowed, Unhandled }

/// Total specificity order. Lower wins. DERIVED, never declared.
type Spec = (Layer, u32, u32, u32);   // (layer, tool_width, mode_width, surface_width)

impl KeyBindings {
    pub fn resolve(&self, sig: &EventSig, c: &Ctx, app: &AzraApp) -> Outcome {
        // 1. Text fields win over everything. (Today's ONLY gate — input_keyboard.rs:18.)
        if c.text_focus && !sig.is_text_safe() { return Outcome::Unhandled; }

        // 2. MODAL CAPTURE — gates the scan, does NOT follow it. (§3.4)
        //    Eligible = tool-SPECIFIC bindings naming this modal. `when.tool.is_any()` is
        //    what distinguishes "tool-agnostic" from "the modal's own map" — pure data,
        //    no second source of truth.
        let exclusive = c.tool_modal.filter(|t| t.capture() == Capture::Exclusive);

        let mut best: Option<(ActionId, Spec)> = None;
        for def in ACTION_DEFS {
            // 3. LEGACY NEVER RESOLVES. The migration firewall. (§8)
            let Guard::Declared(w) = def.guard else { continue };

            // 4. Capture filter.
            if let Some(t) = exclusive {
                if w.tool.is_any() || !w.tool.has(t) { continue; }   // not this modal's map
            }

            // 5. Event match (exact mods — unchanged from KeyBind::matches, keybindings.rs:56).
            if self.get(def.id).sig() != *sig { continue; }

            // 6. Context admits — the 3-axis conjunction. (§3.1)
            if !w.accepts(c) { continue; }

            // 7. Availability poll — Blender's third cascade level (§1.3). RARE.
            //    A failed poll does NOT consume; we continue scanning. This is
            //    OPERATOR_PASS_THROUGH (wm_event_system.cc:2745-2757), and it is what lets
            //    a chord be legitimately shared.
            if !available(def.avail, app) { continue; }

            // 8. Most-specific wins. Ties are a BUG, caught at CI (§6.4), not a coin-flip.
            let s = (layer_of(&w), w.tool.width(), w.mode_width(), w.surface.width());
            match best {
                None => best = Some((def.id, s)),
                Some((_, bs)) if s < bs => best = Some((def.id, s)),
                Some((prev, bs)) if s == bs => debug_assert!(false,
                    "ambiguous chord {sig:?} in {c:?}: {prev} vs {}", def.id),
                _ => {}
            }
        }

        // 9. Fire, or swallow if a modal owns the keyboard.
        match (best, exclusive) {
            (Some((id, _)), _) => Outcome::Fire(id),
            (None, Some(_))    => Outcome::Swallowed,   // BREAK. Nothing below is reachable.
            (None, None)       => Outcome::Unhandled,
        }
    }
}
```

**Consumption:** `Fire` and `Swallowed` both consume; only `Unhandled` reaches egui. **At most one action fires per event** — which alone kills all ten verified shipping double-fires, including the destructive C-1.

**The `available` escape hatch is deliberately quarantined** — and its layering is what makes this compile. `app_types` **cannot name `AzraApp`** (verified: no `app` dep). So:
```rust
// app_types: data only.
pub enum PollId { HasVoxel, HasSelection, CurveEditing }
// app/src/keymap_polls.rs — the ONLY closure-shaped thing in the system.
pub fn available(a: Avail, app: &AzraApp) -> bool {
    match a {
        Avail::Always => true,
        Avail::Poll(PollId::HasVoxel)     => app.sculpt.voxel.is_some(),
        Avail::Poll(PollId::HasSelection) => !app.sel.is_empty(),
        Avail::Poll(PollId::CurveEditing) => app.curve_edit_prim_index().is_some(),
    }
}
```
Polls are kept **out of `When`** on purpose: an opaque poll is not statically decidable, and Blender's decision to make MODE an opaque poll is exactly why it needs a second hand-maintained tree (§1.7 flaw 3). **Discipline, enforced at review:** every `Poll` arm carries a mandatory comment justifying why no axis fits; **any arm that reads `tool.current`, `modeling_context`, `poly_work_mode`, or a surface rect is a bug — that's an axis, declare it.** If it exceeds ~10 arms, an axis is missing.

### 3.6 Worked example 1 — BEVEL (where the old model provably broke)

```rust
// ── app_types/src/keybindings.rs ────────────────────────────────────
// Modal semantics ARE actions, namespaced by tool. This is Blender's propvalue idea
// (§1.4) WITHOUT a parallel system: the tool reads a semantic id, never a key, and the
// SAME table / editor / hint row / persistence serves it.
pub const ACT_BEVEL_CLAMP:   &str = "bevel.clamp_toggle";     // was ACTION_BEVEL_TOGGLE_CLAMP
pub const ACT_BEVEL_CONFIRM: &str = "bevel.confirm";          // was raw Key::Enter, input_keyboard.rs:160
pub const ACT_BEVEL_CANCEL:  &str = "bevel.cancel";           // was raw Key::Escape,  :161
pub const ACT_BEVEL_SEG_UP:  &str = "bevel.segments_up";
pub const ACT_LOOPCUT_CLAMP: &str = "loopcut.clamp_toggle";   // THE SPLIT — independently rebindable
pub const ACT_LOOPCUT_PAIRED:&str = "loopcut.paired_loops";   // was HARDCODED Key::S, :609

const WHEN_BEVEL: When = When {
    tool: ToolMask::only(ToolId::PolyBevel),
    ..When::ANY      // surface/pillar/work/elem deliberately UNCONSTRAINED — see §4.2
};
const WHEN_LOOPCUT: When = When { tool: ToolMask::only(ToolId::PolyLoopCutSlide), ..When::ANY };

ActionDef { id: ACT_BEVEL_CLAMP,   name: "Clamp Overlap", category: "Bevel",
            guard: Guard::Declared(WHEN_BEVEL),   hint: Hint::Row(4), avail: Avail::Always },
ActionDef { id: ACT_BEVEL_CONFIRM, name: "Confirm",       category: "Bevel",
            guard: Guard::Declared(WHEN_BEVEL),   hint: Hint::Row(9), avail: Avail::Always },
ActionDef { id: ACT_LOOPCUT_CLAMP, name: "Clamp Overlap", category: "Loop Cut",
            guard: Guard::Declared(WHEN_LOOPCUT), hint: Hint::Row(4), avail: Avail::Always },

// Defaults — both C. Blender's Bevel Modal Map binds exactly this (§1.4).
ACT_BEVEL_CLAMP   => KeyBind::simple(C),
ACT_LOOPCUT_CLAMP => KeyBind::simple(C),
```
`WHEN_BEVEL.overlaps(&WHEN_LOOPCUT)` is **false** (`only(PolyBevel) & only(PolyLoopCutSlide) == 0`), so both defaulting to `C` is **zero conflict, by arithmetic**. The owner's per-tool requirement, satisfied. This also deletes the 6-line raw-`push` bandaid at `input_system.rs:306-320` whose apology comment was literally the API reporting a missing tool axis.

**The call site.** Was 18 lines of copy-pasted `binding_for` + manual mod-compare (`input_keyboard.rs:576-593`, duplicated verbatim at `:598-607`):
```rust
Outcome::Fire(ACT_BEVEL_CLAMP)   => self.poly_bevel_toggle_clamp(),
Outcome::Fire(ACT_BEVEL_CONFIRM) => self.poly_confirm_bevel(),
Outcome::Fire(ACT_BEVEL_CANCEL)  => self.poly_cancel_bevel(),
Outcome::Fire(ACT_BEVEL_SEG_UP)  => self.poly_bevel_segments(1),
```
The 7 copy-pasted `binding_for` blocks (`:45, :61, :580, :598, :1078, :1104, :1193`) collapse into `resolve()`. **That collapse IS the "No Duplication" discharge.**

### 3.7 Worked example 2 — INSET (the sharper test)

Today, `input_keyboard.rs:629-634`:
```rust
let (g, i_key) = ctx.input(|i| (
    i.key_pressed(egui::Key::G) && !i.modifiers.any(),
    i.key_pressed(egui::Key::I) && !i.modifiers.any(),
));
if g { self.poly_inset_set_mode(false); } else if i_key { self.poly_inset_set_mode(true); }
```
Two raw literals, unregistered, unremappable, invisible to the hint row and the editor. And `poly/inset.rs:~110` hardcodes the *prose* `"Off = Region (Blender's I default) … On = Individual (I,I)"` in a hover text that goes stale on any rebind.

```rust
pub const ACT_INSET_REGION:     &str = "inset.region_toggle";      // was raw Key::G, :630
pub const ACT_INSET_INDIVIDUAL: &str = "inset.individual_toggle";  // was raw Key::I, :631
const WHEN_INSET: When = When { tool: ToolMask::only(ToolId::PolyInset), ..When::ANY };

ActionDef { id: ACT_INSET_REGION,     name: "Region",     category: "Inset",
            guard: Guard::Declared(WHEN_INSET), hint: Hint::Row(2), avail: Avail::Always },
ActionDef { id: ACT_INSET_INDIVIDUAL, name: "Individual", category: "Inset",
            guard: Guard::Declared(WHEN_INSET), hint: Hint::Row(2), avail: Avail::Always },
ACT_INSET_REGION     => KeyBind::simple(G),
ACT_INSET_INDIVIDUAL => KeyBind::simple(I),
```

**Why inset is the sharper test than bevel:** it binds **G**, and `ACT_GRAB` is globally bound to **G**; it binds **I**, and `ACT_BOOL_INT` is globally bound to **I**. During an inset drag: `PolyInset.capture() == Exclusive` ⇒ step 4 filters `ACT_GRAB` out (`when.tool.is_any()` ⇒ not this modal's map) **before** any event match. `ACT_GRAB` is unreachable, and `ACT_INSET_REGION` fires alone. Today this works **only** because the `else if` ladder happens to reach the inset arm (`:622`) before the terminal `else` (`:645`) — *correctness by statement position*. Same outcome, now declared and testable. Reordering is no longer even expressible.

---

## 4. THE OWNER'S THREE EXAMPLES, TRACED

### 4.1 "F in outliner ≠ F in viewport" — the SURFACE axis

```rust
ActionDef { id: ACT_FOCUS, ..,
            guard: Guard::Declared(When { surface: SurfaceMask::only(Surface::Viewport), ..When::ANY }) },
ActionDef { id: ACT_OUTLINER_FRAME, ..,
            guard: Guard::Declared(When { surface: SurfaceMask::only(Surface::Outliner), ..When::ANY }) },
```
**Hovering the outliner, press F.** `Ctx.surface = Outliner` (pushed by the outliner's own draw, §8/S2):
1. Step 2 — no modal ⇒ `exclusive = None`.
2. `ACT_FOCUS`: step 5 event matches; step 6 `accepts` → `SurfaceMask::only(Viewport).has(Outliner)` = **false** ⇒ `continue`.
3. `ACT_OUTLINER_FRAME`: matches, `accepts` = true, `available` = true ⇒ `best`.
4. `Fire(ACT_OUTLINER_FRAME)`. **Exactly one action.**

In the viewport, the reverse. *Today:* `input_keyboard.rs:828` has **no surface check** and the outliner has no F handler ⇒ F over the outliner opens the viewport pie. Blender parity: `Ctrl+F` → `Outliner:outliner.start_filter` vs `Mesh:wm.call_menu`, zero modifiers distinguishing them (§1.5).

**⚠️ Prerequisite the plan must not hide:** *"F in outliner"* presumes **outliner actions exist to bind to.** They do not — the outliner has zero keyboard actions today (only ctrl/shift drag-copy modifiers and Esc/Enter rename, `outliner_actions.rs:30-40`). **The axis is worthless until there are actions on the far side of it.** Inventing an outliner action set (frame / rename / isolate / expand-all / delete) is an owner scoping call → §10, D4.

### 4.2 "F with the bevel tool active … does not affect the outliner" — the TOOL axis

`Ctx.tool = PolyBevel`, `Ctx.tool_modal = Some(PolyBevel)`.

**Case A — F IS bound in the bevel map** (say `bevel.harden_normals`, Blender's `H → HARDEN_NORMALS_TOGGLE`):
1. Step 2 — `PolyBevel.capture() == Exclusive` ⇒ `exclusive = Some(PolyBevel)`.
2. Step 4 — only bindings with `!when.tool.is_any() && when.tool.has(PolyBevel)` survive. `ACT_FOCUS` (`tool: ANY`) is filtered **before its event is even compared**. So is `ACT_OUTLINER_FRAME`. So is `save`, and every other `When::ANY` action.
3. `bevel.harden_normals` matches ⇒ `Fire`. **F tweaks the bevel parameter. Nothing else can run.**

**Case B — F is NOT bound in the bevel map:**
1–2. Same filter; nothing survives ⇒ `best = None`.
3. Step 9: `(None, Some(_))` ⇒ **`Outcome::Swallowed`**. F reaches *nothing* — not the outliner, not the viewport pie, not `save`.

**Why this is structural and not discipline:** the swallow does not depend on `ACT_FOCUS` declaring anything about bevel. It falls out of `PolyBevel.capture()`, which is one exhaustive `const fn` a new tool cannot forget to fill in. Blender parity: `Primitive Tool Modal Map: F → CHANGE_RADIUS` is this scenario verbatim, and `Knife Tool Modal Map` must **re-declare `Ctrl+Z → UNDO`** precisely because the global `ed.undo` is unreachable while it runs — proof-by-necessity that modal maps fully shadow (§1.4).

**Note `WHEN_BEVEL` leaves `surface` as `ANY` — deliberately** (§3.6). A modal that has captured the keyboard owns it regardless of where the mouse wanders. Constraining `surface: Viewport` there would mean **C silently stops toggling clamp mid-drag** the moment the cursor crosses the properties panel — a regression this design would *introduce*. The alternative (a stateful "latch the surface at modal start") is new lifecycle state in a codebase where `auto_commit_episodes()` exists *because episodes leak*; a modal exiting by a path that doesn't clear the latch would strand every subsequent keypress against a stale surface, silently. **Decision: modal-layer bindings declare `surface: ANY`. The latch does not exist.** Pointer-capture semantics are recovered for free because `Capture::Exclusive` already owns the keyboard.

### 4.3 "F in object mode … something different" — the MODE axis

```rust
ActionDef { id: ACT_FOCUS,     .., guard: Guard::Declared(
    When { surface: SurfaceMask::only(Surface::Viewport), ..When::ANY }) },        // Layer::Surface
ActionDef { id: ACT_POLY_FILL, .., guard: Guard::Declared(
    When { surface: SurfaceMask::only(Surface::Viewport),
           pillar:  PillarMask::only(Pillar::Polygonal),
           work:    WorkMask::only(WorkKind::Edit),
           elem:    ElemMask::only(ElemMode::Vertex).or(ElemMask::only(ElemMode::Edge)),
           ..When::ANY }) },                                                        // Layer::Mode
```
**In Poly Vertex Edit mode, viewport, press F.** Both match on event and both `accepts`:
- `layer_of(ACT_POLY_FILL.when)` → `pillar` not ANY ⇒ **`Layer::Mode`**
- `layer_of(ACT_FOCUS.when)` → tool/pillar/work/elem all ANY, surface not ANY ⇒ **`Layer::Surface`**
- `Mode < Surface` ⇒ **`Fire(ACT_POLY_FILL)`. Alone.**

**In Object mode:** `ACT_POLY_FILL.accepts` fails on `work` (`only(Edit).has(Object)` = false) ⇒ `continue` ⇒ **`Fire(ACT_FOCUS)`**.

*Today both fire* — verified C-4 (§2.5c). Blender parity: bare F is `mesh.edge_face_add` in Mesh, `wm.radial_control(brush.size)` in Sculpt, and **unbound in Object Mode** (§1.5).

Note `elem` uses `or(...)` — that is the shipped `"Poly Vertex/Edge mode"` disjunction (§2.6) expressed as data, which one flat enum could not hold.

---

## 5. MODAL & LIVE-DRAG MODIFIERS — the second primitive

### 5.1 Why they need their own primitive

`resolve(sig, ctx) -> Outcome` is **edge-shaped**. It structurally cannot express `is_snap_active` (`smart_snap.rs:10-12`) — a *continuous predicate sampled every frame mid-drag*:
```rust
pub fn is_snap_active(&self, ctrl: bool) -> bool { self.viewport.smart_snap_on || ctrl }
```
`ctrl` is **hardcoded**, consumed at **10 sites** (`gizmo.rs:1038/1074/1431`, `poly_drag_tools.rs:114/303/421`, `face_ops.rs:104`, `nurbs_tools.rs:331`, `viewport_overlays.rs:400`), and **remappable by no means today**. Same for `shift` → duplicate (`gizmo.rs:1175, 1391`), `shift` → bevel profile (`input_keyboard.rs:591`), `ctrl` → inset/extrude precision (`:617, :624, :640`). All threaded as bare `bool` params.

**These are `Trigger::Qual` — level-sampled, declared in the SAME registry**, so they remain remappable, conflict-checked, and hint-generatable:
```rust
pub const QUAL_SNAP:          &str = "qual.snap";                // was hardcoded ctrl
pub const QUAL_GIZMO_DUP:     &str = "gizmo.q.duplicate";        // was gizmo.rs:1175's bare `shift`
pub const QUAL_BEVEL_PROFILE: &str = "bevel.q.profile";          // was input_keyboard.rs:591
pub const QUAL_INSET_PRECISE: &str = "inset.q.precision";        // was input_keyboard.rs:624

/// O(1) precomputed lookup — NO scan. Safe on the hot drag path.
pub fn qual_active(&self, id: ActionId, c: &Ctx) -> bool {
    let Some(b) = self.qual_index.get(id) else { return false };
    b.when.accepts(c) && b.trigger.qual_matches(c.mods)
}
```

### 5.2 How `Shift = duplicate` is firewalled out of a bevel drag — DECIDED

Both are `Trigger::Qual(SHIFT)`:

| Qualifier | `when` | Live during a bevel drag? |
|---|---|---|
| `QUAL_GIZMO_DUP` | `{ tool: only(Gizmo), ..ANY }` | `only(Gizmo).has(PolyBevel)` = **false** |
| `QUAL_BEVEL_PROFILE` | `{ tool: only(PolyBevel), ..ANY }` | **true** |

`poly_update_bevel_drag(mpos, shift)` keeps its exact signature — **only the source of the bool changes**, from `ctx.input(|i| i.modifiers.shift)` to `km.qual_active(QUAL_BEVEL_PROFILE, &ctx)`. Minimal diff.

**Bonus — this design SURFACES verified bug C-7, which nothing detects today.** `shift_duplicate` at gizmo-drag start (`gizmo.rs:1175, 1391`) and `snap_rotation_active: shift` read every frame during the move (`gizmo.rs:1396, 1444`) are **BOTH live in one gesture**. Declared as two qualifiers, both on SHIFT, both `when.tool = only(Gizmo)`, overlapping ⇒ the conflict checker reports it. Today it is invisible. *(Which of the two Shift meanings is intended is a UX decision, not a code one → §10, D5.)*

### 5.3 LATCHED vs LIVE — **DECIDED: level-sampled (live). Not latched.**

This is a deliberate, argued deviation from the oracle.

**The oracle is unambiguous and says latch.** `Knife Tool Modal Map` binds `LEFT_SHIFT PRESS → SNAP_MIDPOINTS_ON` / `LEFT_SHIFT RELEASE → SNAP_MIDPOINTS_OFF` (×2 for right shift, ×4 for ctrl → `IGNORE_SNAP_ON/OFF`).

**We reject it anyway. Three reasons, in order of weight:**

1. **An edge-triggered latch has a real bug class: a RELEASE lost to focus change, alt-tab, or a swallowing modal strands the latch ON, with no self-correction.** Level-sampling is **idempotent and cannot strand**: if Shift is not down this frame, the answer is false, no matter what happened to the RELEASE.
2. **Blender itself pays for the latch.** `View3D Walk Modal` needs an explicit `LOCAL_DOWN_STOP`; Knife needs *four separate OFF items* purely to paper over it. Every one is a row someone can forget.
3. **It matches AZRA's existing shape exactly.** `is_snap_active(ctrl) = smart_snap_on || ctrl` is *already* level-sampled at 10 call sites, so migration is a **source-swap, not a restructure**.

The declaration still lives in the same registry, so it stays remappable and hint-generatable — which is the part Blender gets right and we keep.

**Accepted cost, stated plainly:** we cannot express "tap Shift to toggle snap on for the rest of the drag." If that is ever wanted, it is a `Val::Press`-triggered **action** that flips tool state — not a qualifier. The two primitives stay clean.

**Residual risk (the most likely place a slice silently breaks feel):** `is_snap_active`'s 10 call sites each need their snap declared in the owning tool's map, or the qualifier goes dead under `Capture::Exclusive`. This will not be caught by a fixture — only by the owner in the running app. It is why S5 (qualifiers, snap-first) is a standalone slice with an owner smoke-test.

---

## 6. REMAPPING

### 6.1 Location — `~/.azra/keybinds.json`, unchanged

**For the record:** this is `dirs::home_dir()/.azra` (`keybindings.rs:491-496`), **not** `%LOCALAPPDATA%/Azra/` as CLAUDE.md documents. Both roots are live: `~/.azra/` holds `keybinds.json` + `workspaces.json`; `%LOCALAPPDATA%/Azra/` holds projects/library/gpu-cache.

**The keymap already lives in `~/.azra/`. Do NOT move it under this task.** Moving it is itself a data-loss event needing an old-path read, and the priority ladder says fix corruption first, not tidy paths. Flagged for a separate decision → §10, D7.

**JSON, not bincode.** The keymap is a config sidecar and stays one. **The bincode "append variants at the END only" rule does NOT bind here**, and nothing in this design goes near `.fl4`, `HalfEdgeMesh`, or `NodePayload`. **No save-format red line is engaged by any part of this work.**

### 6.2 Format — v2 → v3, a DIFF over defaults

Blender's rationale, verbatim (`wm_keymap.cc:594-602`): *"Rather than saving the entire keymap for user preferences, we only save a diff so that changes in the defaults get synced."* **AZRA's `save()` already does diff-only** (`keybindings.rs:501-507`) — that instinct was right; it just lacks a version read and a key encoding.

Because context lives on the **action** (§3.3), the shape barely changes:
```json
{ "version": 3,
  "bindings":   { "focus": {"key":"key.V","val":"press","shift":false,"ctrl":false,"alt":false} },
  "qualifiers": { "qual.snap": {"shift":false,"ctrl":true,"alt":false} },
  "quarantine": { "some_retired_id": {"key":"key.P","val":"press","shift":false,"ctrl":false,"alt":false} } }
```

### 6.3 ⚠️ Fix the LIVE data-loss trap FIRST — before anything else

```rust
// keybindings.rs:24  key_display()   →  _ => "?"
// keybindings.rs:71  to_json()       →  json!({ "key": key_display(self.key), ... })
// keybindings.rs:40  key_from_str()  →  _ => return None
// keybindings.rs:521 load()          →  silently skipped
```
**This is not hypothetical.** `ACT_POLY_GROW_SEL => KeyBind::ctrl(Plus)` and `ACT_POLY_SHRINK_SEL => KeyBind::ctrl(Minus)` are **shipped defaults** (`:429-430`) on keys `key_display` cannot render — **the Shortcut Editor renders "Ctrl+?" today**, and any user rebind to Plus/Minus/F9/numpad/PageUp **evaporates on restart with no error**.

**The structural fix: split the two concerns that are currently fused into one function.**
```rust
/// SERIALIZATION ONLY. EXHAUSTIVE — NO `_` arm. A new egui::Key on a version bump
/// becomes a COMPILE ERROR, not a silent "?". This is the whole fix.
pub const fn key_id(k: egui::Key) -> &'static str { match k { egui::Key::A => "key.A", /* ...all... */ } }
pub fn key_from_id(s: &str) -> Option<egui::Key> { /* inverse, table-driven */ }
/// DISPLAY ONLY. May be lossy; NEVER reaches disk.
pub fn key_display(k: egui::Key) -> &'static str { /* unchanged */ }
```
> **Never serialize a display glyph. Serialize a stable id; derive the glyph for display only.**

This matters double once the keymap feeds every hint row — a `"?"` would propagate everywhere.

**The existing test cannot catch it** — `app/src/types/mod.rs:143-149`, `default_bindings_exist_for_all_actions`, does `let _ = bind.label();` and **discards the result**. It proves only "no panic." Replace with:
```rust
#[test] fn every_key_round_trips() {
    for k in ALL_EGUI_KEYS { assert_eq!(key_from_id(key_id(k)), Some(k)); }
}
#[test] fn every_default_binding_round_trips() {
    for def in ACTION_DEFS {
        let b = default_bind(def.id);
        assert_eq!(KeyBind::from_json(&b.to_json()), Some(b.clone()), "{} does not round-trip", def.id);
    }
}
```

### 6.4 Conflict detection — real collision vs legitimate context split

**Two checkers on two different axes. Both ship. They are complementary, not redundant.**

**(a) Static — predicate algebra, replaces `keybindings.rs:485`'s free-text `!=`:**
```rust
pub enum Severity { Hard, Soft }

pub fn conflicts(&self) -> Vec<Conflict> {
    let mut out = Vec::new();
    for (i, a) in ACTION_DEFS.iter().enumerate() {
        for b in &ACTION_DEFS[i + 1..] {
            if self.get(a.id) != self.get(b.id)              { continue; }   // different chord
            if !a.guard.when().overlaps(&b.guard.when())     { continue; }   // DECIDABLE — pure data

            // ── THE RULE THAT DISTINGUISHES A COLLISION FROM A CONTEXT SPLIT ──
            // Cross-LAYER shadowing is CORRECT, not a bug: Knife's modal map re-declares
            // Ctrl+Z → UNDO over the global ed.undo BY DESIGN (§1.4). Flagging that would
            // train the user to mute the detector — which is exactly how Blender ended up
            // shipping 98 shadowed items with zero warnings.
            if layer_of(&a.guard.when()) != layer_of(&b.guard.when()) { continue; }

            let both_declared = matches!(a.guard, Guard::Declared(_))
                             && matches!(b.guard, Guard::Declared(_));
            let both_always   = matches!(a.avail, Avail::Always) && matches!(b.avail, Avail::Always);
            out.push(Conflict { a: a.id, b: b.id,
                sev: if both_declared && both_always { Severity::Hard } else { Severity::Soft } });
        }
    }
    out
}
```
- **Hard** = same chord, same derived layer, overlapping `when`, both `Declared`, both `Avail::Always` ⇒ **provably unreachable**. `debug_assert!` at startup + blocking in the editor. This is the structural replacement for `bind_default()`'s silent drop.
- **Soft** = anything touching a `Legacy` guard (its real gate is hand-written; the declaration is a *claim*) or an `Avail::Poll` (undecidable). **Report, never block.** Blender's *permissiveness* is right — 113 multi-claimed 3D-View signatures mostly work via operator poll; only its *silence* is wrong.

**(b) Dynamic — brute-force reachability, which catches what (a) structurally cannot:**
```rust
#[test] fn no_declared_action_is_ambiguous() {
    // 6 surfaces × 27 tools × 9 pillars × 3 works × 4 elems, minus unreachable combos.
    // × ~76 distinct default chords. Runs in milliseconds.
    for c in all_reachable_ctxs() {
        for chord in all_default_chords() {
            assert!(winners_at_min_spec(chord, &c).len() <= 1,
                    "{chord:?} ambiguous in {c:?}: {:?}", winners_at_min_spec(chord, &c));
        }
    }
}
#[test] fn no_declared_overlaps_any_legacy() { /* the S-slice gate — see §8 R1 */ }
#[test] fn shipped_defaults_have_zero_hard_conflicts() { /* … */ }
#[test] fn every_exclusive_tool_binds_cancel() { /* §3.4 P0 rail */ }
```
(a) tests the **algebra**; (b) tests **reachability**. A `Poll`-bearing tie is invisible to (a) but surfaces in (b) as a concrete `(chord, ctx) → 2 winners` failure rather than a muted warning.

**⚠️ The honest residual — the weakest point in this design, named rather than buried.** Two `Declared` bindings at the same derived layer, on the same chord, with overlapping masks, where *either* carries an `Avail::Poll`: they might be genuinely disjoint at runtime (the poll separates them) or a real bug. **Undecidable.** So the tie is allowed, downgraded to `Soft`… and then declaration order silently decides the winner. That is the very flaw we condemn in Blender (§1.7 flaw 1), reproduced in the one place we claim to have deleted it. We cannot simply drop `Avail::Poll` — pass-through is load-bearing (§1.3), and removing it would make every legitimately-shared chord a `Hard` error.

**The mitigation, and it is partial by construction:** the `Soft` list is a **checked-in allowlist file**, and CI fails if `conflicts()` produces a `Soft` entry not in it. That converts "a warning nobody reads" into "a diff a reviewer must approve." A new `Soft` tie becomes a review event. It does not make the tie decidable; it makes it **visible and deliberate**. → §10, D3.

### 6.5 Action renames and splits — where the current design silently destroys data

`set()` (`keybindings.rs:471-477`) only inserts if the id is in `ACTION_DEFS`; `load()` calls `set()` per entry (`:519-524`). ⇒ **Renaming or removing an action silently discards the user's bind — no warning, no log, no toast, no recovery.** This work *requires* renames, so this is not optional.

```rust
/// APPEND-ONLY. An id that ever shipped must NEVER be reused for a different action.
const ACTION_RENAMES: &[(&str, &str)] = &[
    ("clip_curve_toggle_dup", "clip_curve_toggle"),   // InputSystem's dup dies; ACT_CLIP_CURVE survives
];

/// THE SPLIT TABLE. One v2 id was consumed by TWO tools (input_keyboard.rs:581 AND :599).
/// A v2 remap MUST fan out to BOTH v3 ids, or the user silently loses half their customization.
const ACTION_SPLITS: &[(&str, &[&str])] = &[
    ("bevel_toggle_clamp", &["bevel.clamp_toggle", "loopcut.clamp_toggle"]),
];
```
**Unknown at load ⇒ QUARANTINE, never drop.** Blender's precedent: a binding whose operator no longer exists is **inert, not fatal** (`wm_event_system.cc:2683`). The quarantine block preserves the raw JSON and is written back on every save, so a later rename entry can recover it.

```rust
pub fn load() -> Self {
    let mut kb = Self::defaults();
    let Ok(txt) = fs::read_to_string(path) else { return kb };
    let Ok(root): Result<serde_json::Value, _> = serde_json::from_str(&txt)
        else { return Self::quarantine_whole_file(txt) };   // NEVER unwrap_or_else(defaults)
    let ver = root.get("version").and_then(|v| v.as_u64()).unwrap_or(2);   // ← FINALLY READ
    match ver {
        3       => parse_v3(&root, &mut kb),
        0..=2   => migrate_v2_to_v3(&root, &mut kb),      // near-identity + RENAMES + SPLITS
        v       => return Self::quarantine_newer(v, txt), // downgrade: keep the file INTACT
    }
    kb
}
```
**`"version": 2` is written today (`:508`) and NEVER READ** (`:513-527`). There is no migration branch and no place to put one; the `2` implies a v1 migrated by nothing.

**A bare `unwrap_or_else(|_| Self::defaults())` anywhere in this recreates the exact `workspaces.json` catastrophe** (`types_workspace.rs:4-30`, fixed at **e9fa024**): one unknown `ToolButtonId` silently discarded **every** workspace, then made it permanent on the next `save()` — verified against the owner's real 9509-byte, 9-workspace file. **Keep every bit of `load()`'s existing tolerance** (Value-first, per-entry `Option`, membership-checked `set()`); it independently arrived at the shape `workspaces.json` had to be *retrofitted* with.

**Gate test, mirroring the e9fa024 template** (`types_workspace.rs:325+`, `retiring_a_tool_button_drops_the_button_not_the_users_workspaces`):
```rust
#[test] fn retiring_an_action_quarantines_its_bind_and_keeps_the_others() { /* … */ }
#[test] fn v2_remap_of_a_split_action_lands_on_both_halves() { /* bevel_toggle_clamp */ }
```

### 6.6 Profiles

`profile_modo()` (`:529-536`) is a 4-action stub. In v3 a profile is a named default *layer* under the user diff (`runtime = base ++ profile ++ user_diff`) — Blender's exact shape, free structurally. **Authoring real Maya/Modo/ZBrush maps is explicitly NOT in scope.**

---

## 7. ONE SOURCE OF TRUTH

### 7.1 The mechanism

**One declaration (`ACTION_DEFS`). One binding store (`KeyBindings.map`). Five readers. None may hold a key string.**

Enforced by **type**, at the boundary that matters: nothing outside `keymap` can construct a hint, because a hint is only ever produced from a *typed action id* resolved against the *live* map.

```rust
impl KeyBindings {
    pub fn resolve(&self, sig: &EventSig, c: &Ctx, app: &AzraApp) -> Outcome;  // dispatch
    pub fn qual_active(&self, id: ActionId, c: &Ctx) -> bool;                  // drag modifiers
    pub fn label(&self, id: ActionId) -> String;                               // menus/tooltips
    pub fn hints_for(&self, c: &Ctx, app: &AzraApp) -> Vec<HintRow>;           // the hint row
    pub fn editor_rows(&self) -> Vec<EditorRow>;                               // prefs UI
}
pub struct HintRow { pub glyph: String, pub name: &'static str, pub active: bool }
impl HintRow {
    /// The ONLY constructor. It takes an ActionId — there is NO `&str` glyph overload.
    /// `("[/]", "Brush size")` becomes UNWRITABLE: there is no type to hand in.
    pub fn of(km: &KeyBindings, id: ActionId) -> Option<Self>;
}
```

### 7.2 `hints_for` — where the design pays off

```rust
pub fn hints_for(&self, c: &Ctx, app: &AzraApp) -> Vec<HintRow> {
    let mut v: Vec<(u8, HintRow)> = ACTION_DEFS.iter()
        // ONLY Declared. A Legacy `when` is a CLAIM, not enforced — showing it could lie,
        // and "a wrong hint is worse than none" is the one thing viewport_hints.rs got exactly right.
        .filter_map(|d| match (&d.guard, &d.hint) {
            (Guard::Declared(w), Hint::Row(o)) if w.accepts(c) => Some((*o, d)),
            _ => None })
        .filter(|(_, d)| available(d.avail, app))     // the SAME poll dispatch uses
        .map(|(o, d)| (o, HintRow { glyph: self.label(d.id),        // DERIVED, never stored
                                    name:  d.name,
                                    active: self.qual_active(d.id, c) }))  // Blender's 4th opmodal arg
        .collect();
    v.sort_by_key(|(o, _)| *o);
    v.into_iter().map(|(_, h)| h).collect()
}
```

**The proof of one-source-of-truth: `hints_for` filters with the SAME `when.accepts()` and the SAME `available()` that `resolve()` uses.** Not an equivalent copy — the same code. **A key therefore cannot disagree with its own hint, because a disagreement would require the two to evaluate different predicates, and there is only one predicate.**

This is Blender's `status.opmodal(label, ot, propvalue)` (§1.6) reached without a parallel propvalue system — because our modal semantics **are** actions.

**The transition property** (this is why the migration unit is a TOOL, §8): the generated row starts **small and truthful** and grows as tools migrate. `viewport_hints()` becomes `generated(c) ++ legacy_arm(tool)`, and **a tool's legacy arm is DELETED in the same slice that flips its actions to `Declared`.** The two sources coexist safely because they are **partitioned by `Guard`, and the partition lives in the data** — not in a comment, not in a convention.

### 7.3 What this deletes

| Consumer | Today | After |
|---|---|---|
| Hint row | `viewport_hints.rs` — **on master** (58ccee1), 26 hand-written arms, header declares a manual-sync INVARIANT, still ships `[/]` and `X: Cycle bool` | `km.hints_for(c, app)` |
| Hint row (legacy) | `ui_panels.rs:425-503` — 14 tool arms + 12 idle rows of prose | deleted |
| Toasts (~22, 19 files) | `poly_drag_tools.rs:899` duplicates the bevel arm verbatim in different prose | `hints_for(c).join(" · ")` |
| Menus | 15 correct `kb.label()` **and** literals at `menus.rs:702, 703, 717, 861, 880, 885` | all `label()` |
| Overlay | `viewport_overlays.rs:642` — half declaration, half literal in one expression | both from the registry |
| Tooltips | `workspace.rs:455` `.on_hover_text("Ctrl+Y")` for Redo — **wrong at defaults** | `label()` |
| Settings cheat sheet | `win_settings.rs:19-29` — `"X/Y/Z=Lock axis"`, two-thirds false | deleted |
| Prefs editor | iterates `ACTION_DEFS` only ⇒ 19 InputSystem actions invisible & unremappable | one registry ⇒ all rows |

**Drift becomes unrepresentable, not merely fixed.** After this change **there is no string literal to type** — a glyph can only come from `self.label(id)`. That is the only cure, because **drift is a property of hand-writing strings, not of these particular authors.** `viewport_hints.rs` is the proof: it was written *specifically* to be the single source of truth, by an author who understood the problem well enough to document the invariant in the header — and it still ships two keys that exist nowhere in the codebase.

**⚠️ The honest limit of the claim.** `HintRow::of` means a literal cannot enter the **hint row**. Nothing in Rust stops a dev writing `ui.label("C: Clamp")` somewhere else on screen. The real enforcement outside that boundary is **deletion discipline** — remove `viewport_hints.rs`'s arms, `ui_panels.rs:425-503`, and the 22 toasts so there is *no arm left to edit and no precedent to copy*. That is a strong mitigation, but it is discipline, not the type system, and this plan does not claim otherwise.

### 7.4 Two `&'static str` blockers, both real, both cheap

- **`PieItem.shortcut: &'static str`** (`pie_menu.rs:81`, 47 sites) cannot hold a remapped label (`label()` returns `String`) → `Cow<'static, str>`. **But note the trap:** those values are `"1".."8"` — **pie-local SLOT digits** (`PieMenuState.key_selection`, drawn at `:533-537`), *not* keymap glyphs. **One field name, two unrelated concepts.** Only the *opening* key is a keymap binding. Slot positions are owner-LOCKED (`pie_menu.rs:1123-1131`: Union TOP / Subtract RIGHT / Intersect BOTTOM "FOREVER", muscle memory). **Split the field; do not "fix" the digits.**
- **`ActionEntry`'s `&'static str` fields** die with `InputSystem`.

---

## 8. THE GATED MIGRATION

**Each slice: one commit, `cargo build -p app` green, independently revertable, no functionality gap.**

### S0 — `key_id` round-trip. **SHIPS ALONE, FIRST, before any architecture.**
Exhaustive `key_id`/`key_from_id` (no `_` arm), `every_key_round_trips`, `every_default_binding_round_trips`.
*Why first:* it is **live user-data corruption today** (§6.3 — "Ctrl+?" ships; Plus/Minus remaps evaporate), it sits at **priority-ladder rung 1**, and it has **zero dependency on which architecture wins.** Every later slice serializes chords through it.

### S1 — types only, zero callers.
`app_types/src/keymap/`: `Surface`, `ToolId` (+ exhaustive `of()` and `capture()`), masks, `When`, `layer_of`, `Trigger`, `EventSig`, `QualSig`, `Ctx`. Unit tests only (overlap arithmetic, spec total-ordering, `When::ANY` accepts everything). Trivially green, **zero behavior change**.
*Why here:* everything needs the vocabulary and nothing can break. The exhaustive `ToolId::of()` forces all 26 `ActiveTool` variants to be classified — structurally fixing C-9's missing ladder arm.

### S2 — declare, don't enforce. **The only "wide" commit, and it is inert by construction.**
`ActionDef.context: Option<&'static str>` → `guard: Guard`, `hint: Hint`, `avail: Avail`. All 96 get **`Guard::Legacy(when)`**; the 52 prose strings translate to real `When` (only 16 distinct strings — a small, reviewable mapping); the 44 `None` become `When::ANY`. `find_conflicts` switches from string-eq to mask-overlap.
**Behavior: unchanged** — nothing reads `guard` for dispatch yet.
**Value shipped immediately:** conflict detection becomes *sound* and will correctly surface the 15 real default collisions (advisory only, zero dispatch risk).
*If a reviewer objects to the width:* this is a **mechanical field replacement with provably zero behavior change**, not a migration in the rule's sense. It can be split per-category (File, Transform, Poly…) at the cost of ~6 extra commits.

### S3 — the SURFACE axis becomes real. **⚠️ Read the frame-order note.**
Panels publish rects into `ui.surface_rects: Vec<(Surface, Rect)>` (cleared at frame start, pushed during draw); `surface_at(mouse)` = topmost containing rect. Delete `active_input_context()`'s screen fractions.

> **⚠️ CORRECTION TO EVERY PRIOR ANALYSIS — verified against the live code.** It has been claimed that panels draw *before* `handle_keyboard`, so a pushed surface is available "by construction." **That is false.** `draw_panel_list` (`app_update.rs:204`) is **`panel.rs:136` — a hidden panel-*visibility* settings window** (`if !self.ui.show_panel_list { return; }`), not the panels. The real order is:
> ```
> app_update.rs:207   self.handle_keyboard(ctx, tab_consumed);
> app_update.rs:211   draw_viewport_header
> app_update.rs:216   draw_left_tool_strip
> app_update.rs:222   draw_right_panel   → outliner_draw.rs:12   ← THE OUTLINER
> app_update.rs:230   draw_content_library
> app_update.rs:238   self.viewport.viewport_rect = rect;        ← THE VIEWPORT
> ```
> **Every panel draws AFTER the keyboard.**

**Decision: publish rects; `handle_keyboard` consumes the PREVIOUS frame's map. Document the staleness explicitly; do not reorder the frame.**

*Why not move `handle_keyboard` after the panels?* Both options carry a one-frame lag; the question is **which** thing lags. Moving the keyboard makes the surface fresh but delays **every keyboard action's visual effect by one frame** across 96 actions — a felt regression. Keeping the order makes the *surface rect* one frame stale, which is benign: panels do not move between frames except on resize/layout change, and the pointer does not teleport. **The status quo already proves it:** `active_input_context()` reads `viewport_rect`, which is set at `:238` — *after* `handle_keyboard` at `:207` — so surface detection is **already** one frame stale today, and works.

**Accepted, bounded cost, stated so nobody rediscovers it as a mystery:** on frame 1, and for exactly one frame after a dock resize or layout change, `Ctx.surface` is `Surface::Unknown` or stale. `Surface::Unknown` matches only `surface: ANY` bindings. **One keypress during the exact frame of a dock resize may resolve to the previous surface.** This is documented in the module header, not in tribal memory. Per CLAUDE.md's Frame Ordering Contract, ordering is non-negotiable and **explicit** — making the lag explicit is the compliant move; shipping it *unnoticed*, as three prior analyses would have, is not.

### S4 — resolver + kill the second registry + rewrite the CLAUDE.md rule.
Land `resolve()`, `pressed_in()`, `build_ctx()` (the ONE reader of the four mode fields — the fix for C-5), `qual_active()`, `available()` in `app/src/keymap_polls.rs`. Land `no_declared_action_is_ambiguous` (vacuously green: zero `Declared` actions).
Fold `InputSystem`'s **8 live** actions into `ACTION_DEFS` as `Legacy` (**they become remappable — a user-visible win, no capability gap**). Delete the **11 dead** ones (`hierarchy_*`×4, `copy`/`cut`/`paste`, `sculpt_brush_size_*`×2 — whose scroll binds have no consumer *and actively shadow `resolve()` in every mode* — the `clip_curve_toggle` dup, `select_visible_only_toggle`). Delete `is_default` (write-only, never read), `actions_in_category()`/`categories()` (zero callers), and the 4 false comments (`input_keyboard.rs:24`; `input_system.rs:306, 324-325, 343`) — **porting them forward carries the lie forward.** Delete `input_system.rs`'s registry.
**Rewrite the CLAUDE.md rule** to name `ACTION_DEFS`.
*Bounded at ~19 actions. This is deleting a duplicate system, not migrating 96 — and `resolve()` has exactly ONE real call site today (`input_viewport.rs:238`), so the signature change is cheap NOW and never cheaper.*

### S5 — qualifiers; first consumer = snap.
`qual_active()` + `QUAL_SNAP`. `smart_snap.rs:10-12`'s hardcoded `ctrl` → `qual_active(QUAL_SNAP, &c)`. **Ten call sites, ONE function body changes.** Cheapest possible proof of the qualifier registry. **Owner smoke-test required** (§5.3 residual risk).

### S6 — FIRST TOOL: bevel. *(Read `docs/planning/poly/BEVEL.md` first — CLAUDE.md rule.)*
Split `ACTION_BEVEL_TOGGLE_CLAMP` → `bevel.clamp_toggle` + `loopcut.clamp_toggle` (+ `ACTION_SPLITS` entry). Add `bevel.confirm`/`bevel.cancel` (were raw Enter/Escape, `:160-161`), `QUAL_BEVEL_PROFILE` (was raw `shift`, `:591`). **Fix `input_viewport.rs:1352`'s raw `key_pressed(Key::C)`** — the third clamp site that bypasses the binding the other two honor. Migrate the hardcoded `Key::S` (`:609`) — touching it, so in-scope **by the rule**. Delete both copy-pasted `binding_for` blocks, the bevel hint arm, and the duplicated toast at `poly_drag_tools.rs:899`. `Capture::Exclusive`.
**On completion: append the new modal-map shape to `BEVEL.md` §6/§11 and update the `project_bevel_corner_reference` memory** — CLAUDE.md rule; the task is not done until the KB, the bug tracker, and the memory are updated.
Owner tests: the bevel drag, and a C→Q remap.

### S7 — inset. Second modal proves the pattern generalizes; **inset is the sharper test** (its G collides with global `ACT_GRAB`, its I with `ACT_BOOL_INT` — §3.7).

### S8 — the F pie. The owner's literal example, end-to-end.
`ACT_FOCUS` → `Declared(surface: Viewport)`; `ACT_POLY_FILL` → `Declared(pillar: Polygonal, work: Edit, elem: Vertex|Edge)`. **Fix `input_keyboard.rs:827`'s hardcoded `egui::Key::F`** via `Val::Hold` — the pie stops degrading to QuickTap under remap. **Fixing `f_down` IS touching that keybind → this migration is in-scope BY THE RULE, not scope creep.**
**Owner-verifiable in three gestures:** F over the outliner → nothing (was: viewport pie); F in Poly Vertex → fill only (was: fill AND pie); F in Object → pie.
*Gated on §10 D4 (do outliner actions exist to bind to?).*
*Why after S6/S7:* F touches all three axes and the global layer; bevel touched one.

### S9 — pies → `ToolId::Pie(PieId)`, `Capture::Exclusive`.
Deletes the **17** `!z_pie.open` guard chains and the 5 manual "close other pies" lines. **Largest single duplication win; also the biggest blast radius — deliberately AFTER two modals have proven the model.**

### S10 — hint row reads `hints_for()`.
Delete `viewport_hints.rs`'s arms, `ui_panels.rs:425-503`, the 22 toasts, `win_settings.rs:19-29`, `menus.rs`' 6 literals, `workspace.rs:455`. **No coordination needed — `viewport_hints.rs` is on master (verified §2.8); the sibling-worktree collision reported by recon is stale.**

### S11+ — on touch, forever. Remaining actions migrate when touched, per the rule. No deadline.

**Also in-scope by rule, not opportunism:** `COMPLETE_BUG_TRACKER.md` gets the 10 reachable collisions + the `find_conflicts` string-equality defect. **C-1 (Ctrl+Shift+S = Save-As + silent Symmetrize) is destructive and arguably warrants a standalone hotfix ahead of this entire plan** → §10, D6.

### 8.1 What does a stall at 40 % look like?

**A LEDGER, not a fossil.** Three mechanical properties, none of which is discipline:

1. **The un-migrated remainder is BIT-IDENTICAL to today, not degraded.** `Guard::Legacy` never enters `resolve()` — step 3 is a `continue`, in the data. All 15 pre-existing default collisions keep being disambiguated by the same `else if` ladder, unchanged. **There is no window where a new matcher and the old ladder fight over the same action.**
2. **The state is self-describing.** `rg 'Guard::Declared' | wc -l` is the progress bar. `Guard::Legacy` is a **typed TODO that carries its own spec** — the `When` someone already wrote down. Contrast the graveyard: `PolyKeyFlags` (§2.7) has *no marker* distinguishing "migrated" from "forgotten"; poly-transform stalled at 25 % is invisible in the type system. **Here, half-done is typed.**
3. **Every slice is gated by a check that runs in milliseconds** — `no_declared_action_is_ambiguous` + `no_declared_overlaps_any_legacy`. "Is this half-state correct?" is a **CI answer, not a code-review opinion.**

**Concretely at 40 % (≈40/96 `Declared`):** sound conflict detection on all 96; a real surface axis; ONE registry; a truthful (if sparse) generated hint row plus shrinking legacy arms; ~4 tools fully declared with per-tool remappable modal keys; the F pie fixed; `Ctrl+?` fixed; C-1 fixed. **Worse than today in exactly zero ways.**

**And here is the honest counter-argument, because it is the real risk:** *that is exactly why it might never finish.* There is no forcing function. 40/96 is a comfortable, defensible, shippable state, and the pressure to migrate action 41 is **lower** than today's, because today's pain is at least visible. **A ledger nobody is required to close is a monument.** The mitigation is not architectural, it is managerial: **the `Guard::Legacy` count belongs in `MASTER_WORK_LIST.md` as a tracked number with a target**, not left to drift. Recorded here so the trade is made with open eyes.

### 8.2 Risks

| # | Risk | Mitigation | Residual |
|---|---|---|---|
| **R1** | **`Declared`↔`Legacy` double-fire** — flip `ACT_POLY_FILL` while `ACT_FOCUS` still fires from `:828` and C-4 is unchanged | `no_declared_overlaps_any_legacy` **CI test** (not a checklist step) forces overlapping actions into the same slice. **This is why the migration unit is a TOOL, not an action.** | Build failure, not a shipped bug |
| **R2** | **`Avail::Poll` metastasizes** ⇒ overlap undecidable ⇒ we rebuild Blender's opaque poll with extra steps | ONE function; mandatory per-arm justification comment; **any arm reading an axis is a bug**; cap ~10 arms | Review discipline |
| **R3** | **S2's 52 prose→`When` translations are judgment calls.** Wrong ones are **inert while `Legacy`** but go live at migration, long after review | Re-verified against the actual dispatch guard at migration time (that IS the slice's work); UI labels `Legacy` as "unverified" | Accepted — the alternative (all `ANY`) discards 16 strings of already-discovered vocabulary |
| **R4** | **ui2 sequencing.** `fix/ui` is owner-locked; if it lands its own panel registry we built S3 twice | S3 defines `Surface` + `publish_surface_rect()` as a ~15-line contract ui2 *implements*. **Check `fix/ui` before S3.** | Cheap to rebuild |
| **R5** | **`Capture::Exclusive` strands the user** (eats Escape / click-select) — violates the **P0** "Selection must ALWAYS work" | `every_exclusive_tool_binds_cancel` test over all tools — enforced, not reviewed | Whether AZRA also needs Blender's fallback-tool concept (`wm_event_system.cc:4743-4752`) is **unresolved** → §10 D8 |
| **R6** | **Qualifier goes dead under capture** — `is_snap_active`'s 10 sites need snap declared in each owning tool's map | S5 is standalone with an owner smoke-test | **The most likely place a slice silently breaks *feel* rather than compilation.** Not catchable by any fixture. |
| **R7** | **Verification honesty.** Per `feedback_verify_app_path_not_kernel`: `resolve()` is a pure function so its tests are genuinely meaningful — but **green CI does not mean the app feels right** | Every slice touching a live key needs an owner smoke-test in the running app | I cannot see the running app; the exe is often locked (`cargo check -p app`) |
| **R8** | **`Surface` staleness** (§8/S3) | Documented in the module header; bounded to frame 1 + the frame of a layout change | Accepted, explicitly |

---

## 9. REJECTED ALTERNATIVES — the graveyard

**9.1 Grow `InputSystem` (what CLAUDE.md literally says).** REJECTED — **it inverts the spec.** Zero persistence; invisible to the editor; **11 of 19 registrations already dead**. Migrating an action *into* it **removes** remappability and orphans the user's `keybinds.json` entry — migration-as-specified is a regression, action by action. Its `resolve()` cannot even reach both boolean pies. Rebuilding persistence + UI + re-registering 96 actions with new ids would break every user's file.

**9.2 Keep both registries; add context to each.** REJECTED — perpetuates a **live** two-sources-of-truth: `"clip_curve_toggle"` is registered in **both**, dispatched from one, permanently divergent on any remap. Two context-aware registries is that squared. And it leaves the dead-entry-forces-a-bandaid absurdity (§2.1) standing.

**9.3 Context on the BINDING (`{event, mods, when} → action`).** REJECTED, and this was the closest call. It is the natural-looking design and it is the expensive one: composite storage key ⇒ `keybinds.json` goes from `action_id → one bind` to `action_id → [binds]` ⇒ a real v3 shape break ⇒ N editor rows per action (several hundred) ⇒ breaks `HashMap<&'static str, KeyBind>` and therefore all ~80 `kb.pressed` sites **at once** = DOA under the no-batch rule. **Context-on-the-action gets the same expressive power** once ids split per tool — which every candidate required anyway — at a fraction of the diff. **The highest-leverage decision in this plan.**

**9.4 Blender's opaque poll closures (`Box<dyn Fn(&Ctx) -> bool>`) for the axes.** REJECTED — **this is Blender flaw #3 and the cost is measured.** It makes reachability undecidable, killing conflict detection, **and** it forces a second hand-maintained tree to render the prefs UI (`keymap_hierarchy.py`) — two sources that drift. Closures survive only as `Avail::Poll`, at Blender's *operator*-poll level, where "unknown reachability" is honest and merely downgrades a conflict to `Soft`.

**9.5 A declared `layer: Layer` field on the binding.** REJECTED — a declared layer that disagrees with the declared `When` is a second source of truth *inside one struct*. `layer_of(when)` derives it, so disagreement is **unrepresentable**. (Blender's flaw here is real — `space_view3d.cc`'s ~28 hand-ordered statements — but the fix is *derivation*, not *declaration*.)

**9.6 Blender's registration/statement-order priority.** REJECTED — flaw #1, the worst one. Priority defended only by comments; reorder two lines and shortcuts silently change; no test detects it. Textbook No-Bandaids #3.

**9.7 ⚠️ Post-scan swallow ("scan everything, then swallow if nothing matched").** REJECTED, **and recorded loudly because it is the trap two independent candidate designs fell into and no reviewer fully caught.** It does not work: `When::ANY.accepts(ctx)` is **true** during a modal (`ToolMask::ANY` contains every tool bit), so a `When::ANY` action **fires before the swallow check ever runs**. The only rescue is requiring all ~96 actions to individually declare `tool: IDLE` — tribal knowledge, i.e. the exact failure the design claims to prevent. **Capture must gate the scan (§3.4), not follow it.** The corollary is load-bearing: because capture gates the scan, no action needs a `tool: IDLE` declaration, which is precisely what keeps `layer_of` (§3.3′) from collapsing into a single layer.

**9.8 Specificity-only, no layer (popcount of non-`ANY` axes).** REJECTED as the *sole* rule — it is **unstable under widening**: relaxing one binding's axis to `ANY` silently reorders *unrelated* pairs, so a local edit has non-local effects. `layer_of` provides the stable coarse ordering; popcount only breaks ties **within** a layer.

**9.9 Flatten MODE to one enum.** REJECTED — **the shipped data refutes it.** The prose contexts already contain **disjunctions** (`"Poly Vertex/Edge mode"`) and **containment** (`"Polygonal edit mode"` ⊃ `"Poly Face mode"`). One enum expresses neither — which is *exactly* why `find_conflicts`' string `!=` has both false positives and false negatives. Three masked sub-fields: disjunction is `or`, containment is `ANY`.

**9.10 Collapse `WorkMode`/`PolyWorkMode`/`ModelingContext`/`PolyTool` into one enum in this task.** REJECTED **on budget** — the true fix for C-5, and the honest thing to want, but `WorkMode::Sculpt` gates the entire voxel subsystem; the blast radius blows the refactor budget. `build_ctx()` + `debug_assert!(coherent(...))` gets dispatch correctness now; **the collapse is a tracked follow-on.** Flagged as a deliberate interim → §10 D9.

**9.11 A per-tool `type Qual` associated type (compile-error leak prevention).** REJECTED — genuinely stronger than our runtime `when.tool` check (a bevel drag *cannot name* `GizmoQual::Duplicate`), but it requires a `ModalToolKeymap` trait with associated types per tool, which pulls `AzraApp` into the trait (`on_action(&mut self, app: &mut AzraApp, …)`) — and **`app_types` cannot name `AzraApp`** (verified: no `app` dep in `app_types/Cargo.toml`). Putting the registry in `app` instead makes it `pub(crate)` in the god-crate — the exact defect that made `InputSystem` unremappable. **The layering forbids it.** Our `Capture` + `when.tool` gives the same runtime guarantee with correct layering.

**9.12 A `#[derive(Action)]` proc-macro crate.** REJECTED for now — ergonomic, not load-bearing. Adds a workspace crate and compile weight for boilerplate that the `ActionDef` table already handles. Revisit only if the boilerplate becomes demonstrably painful.

**9.13 Press/release latches for qualifiers (the Blender-faithful choice).** REJECTED — see §5.3. A lost RELEASE strands the latch with no self-correction; Blender pays for this with four separate OFF items in `Knife Tool Modal Map` alone.

**9.14 Scatter key handling into panels to fix axis A.** REJECTED — trades one problem for N. `handle_keyboard` being ONE global handler is **not** the bug; feeding it a *wrong surface* is. `pointer_over_ui` is **already computed** at `input_keyboard.rs:12` and thrown away (used only to surrender egui focus, never to gate an action) — the wiring is one line from existing.

**9.15 Move `handle_keyboard` after the panel draws.** REJECTED — see §8/S3. It makes the surface fresh at the cost of delaying **every** keyboard action's visual effect by one frame across 96 actions. A stale rect is benign; a stale *response* is felt.

**9.16 bincode for the keymap.** REJECTED — JSON sidecar, per the `project_bincode_save_format_field_trap` memory. The keymap is config, not scene data. **Nothing here goes near `.fl4`, `HalfEdgeMesh`, or `NodePayload`.**

**9.17 Fold the 29 dialog-local Esc/Enter literals in.** REJECTED as out of scope — widget-local dismissal is idiomatic egui and a different category from global shortcuts. **But the exemption MUST be written INTO the rewritten CLAUDE.md rule**, or the rule reads as violated 29 times and the next dev cannot tell intent from debt. (`shader_nodes.rs` is a frozen file regardless.)

**9.18 Author real Maya/Modo/ZBrush profiles.** REJECTED as out of scope — the *mechanism* is free (§6.6); the *content* is a separate task.

---

## 10. OPEN DECISIONS FOR THE OWNER

Decisions that change what gets built and that the code cannot answer. **D1 gates everything.**

| # | Decision | Why it can't be answered from the code | Recommendation |
|---|---|---|---|
| **D1** | **Ratify the CLAUDE.md rule rewrite (§0.4).** The rule mandates registration in the system that *cannot be remapped*. Rewriting it is required, not optional. | It is a rule; only the owner may change it. Per the Rule Conflict Check we flag rather than override. | **Rewrite it in S4**, naming `ACTION_DEFS`, keeping "migrate gradually", adding the §9.17 dialog exemption. |
| **D2** | **Keyboard layout: physical or logical?** `input_keyboard.rs:219` uses `key_pressed(Key::Num1)` (position-ish) while `gizmo.rs:999` uses `Event::Text` (layout-aware) — **two input models for digits, live, today.** | winit/egui expose both. **The answer freezes into the v3 disk format**; getting it wrong costs a second migration. The owner is in Quebec, likely on a CSA/French layout — this may matter to him personally far more than abstractly. | **Ask directly, before S0 ships `key_id`.** Blender uses physical (a long-standing complaint); we may not want parity here. |
| **D3** | **Accept the `Soft`-conflict residual (§6.4)?** A `Poll`-bearing intra-layer tie is undecidable; declaration order silently decides. Mitigation = a checked-in `Soft` allowlist so a new tie is a review event. | Undecidable by construction once `Avail::Poll` exists, and pass-through is load-bearing. | **Accept, with the allowlist.** Revisit if the `Soft` list exceeds ~10. |
| **D4** | **Does "F in outliner" require inventing an outliner action set?** The outliner has **zero** keyboard actions today. **The axis is worthless until actions exist on the far side of it.** | Product scope, not a code fact. | **Decide before S8.** Minimum viable: frame / rename / isolate / expand-all / delete. |
| **D5** | **`Shift` during a gizmo drag means duplicate (at press) AND rotation-snap (during move)** — C-7, both live in one gesture today. Which is intended, or do they split onto different modifiers? | A UX decision, not a code one. Users may have learned both. | Per the mockup-first memory: **prototype in the HTML Artifact before any egui work.** |
| **D6** | **Hotfix C-1 ahead of this plan?** `Ctrl+Shift+S` = Save-As **and silently symmetrizes the mesh** (`input_keyboard.rs:377` + `:720`). Destructive; ladder rung 1/3. | Sequencing call: architecture vs. a shipping destructive bug. | **Yes — standalone hotfix now.** The priority ladder beats architecture. |
| **D7** | **Config root: `~/.azra/` or `%LOCALAPPDATA%/Azra/`?** Both live; CLAUDE.md documents the latter; the keymap is in the former. | Consolidating is itself a data-migration task with its own loss risk. | **Out of scope here.** Keep `~/.azra/`; decide separately. |
| **D8** | **Does AZRA need Blender's fallback-tool concept** (`wm_event_system.cc:4743-4752`) so click-select survives an Exclusive modal? | Interacts with the **P0** "Selection must ALWAYS work". `every_exclusive_tool_binds_cancel` covers *escape*, not *select*. | **Resolve before S9** (pies → Exclusive). May be simpler than Blender's `TOOLREF_FLAG_FALLBACK_KEYMAP`. |
| **D9** | **Accept `Mode` as a derived view over four contradictory fields (§9.10)?** The real fix is collapsing them; the blast radius blows the refactor budget. | Deliberate interim needing sign-off. | **Accept for this plan**; track the collapse as a follow-on in `MASTER_WORK_LIST.md`. |
| **D10** | **Should the `Guard::Legacy` count be a tracked number with a target (§8.1)?** Without it, 40 % is a comfortable resting place. | Managerial, not architectural. | **Yes** — put it in `MASTER_WORK_LIST.md`. |

---

## APPENDIX — RED LINES

- **Never serialize a display glyph.** Serialize a stable key id; derive the glyph for display only. (`key_display`'s `_ => "?"` is live corruption — §6.3.)
- **Never `unwrap_or_else(|_| defaults())` on the keymap file.** That is the `workspaces.json` catastrophe verbatim (e9fa024). Unknown ⇒ **quarantine**, never drop.
- **Never rename or split an action id without an `ACTION_RENAMES` / `ACTION_SPLITS` entry.** `set()` drops unknown ids silently (`keybindings.rs:471-477`).
- **Never let a `Legacy` guard reach `resolve()` or `hints_for()`.** The `continue` at step 3 is the migration firewall.
- **Never add a post-scan swallow.** Capture gates the scan (§3.4, §9.7).
- **Never put `AzraApp` in `app_types`.** Verified: no `app` dep. Polls live in `app/src/keymap_polls.rs` behind `PollId`.
- **Never downgrade `debug_assert!(coherent(...))` in `build_ctx()` to a log.** If it fires, fix `poly_switch_work_mode()`.
- **Nothing in this work touches `.fl4`, `HalfEdgeMesh`, `NodePayload`, or bincode.** The keymap is a JSON sidecar and stays one.
