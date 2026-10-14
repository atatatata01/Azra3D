# WORK_ORDER.md — The Ordered Fix List

**Status:** ACTIVE ordering authority. Supersedes ad-hoc prioritization.
**Baseline:** master @ `e6d3f9e` (core_math 476 / render 13 / graphics 41 / app 175 — all green).
**Date:** 2026-07-15
**Ranking authority:** the CLAUDE.md Priority Ladder. Nothing else.

---

## 0. How this list is ordered

The ladder is the only ranking authority:

1. Crash / data corruption
2. Source-of-truth violations
3. Undo / selection / save-load regressions
4. Workflow breakage
5. Polish / perf / UI cleanup — only if explicitly asked
6. Opportunistic refactor — never, unless assigned

**Rung first, then cost, then dependency.** Within a rung, cheap-and-unblocked beats expensive-and-blocked.
Frequency is a tiebreak *inside* a rung — it never promotes across rungs. That rule is what keeps item 16
(the highest-frequency bug in the app) below rung-1 items that fire rarely.

**Two design docs are authoritative for their decisions and were not re-derived:**

- `docs/planning/poly/GRS_PARITY_DECISIONS.md` — the S0–S4 chain. Order is load-bearing; adversarially
  defended and re-defended during this pass (see §4).
- `docs/planning/input/INPUT_ARCHITECTURE_PLAN.md` — the 3-axis keymap. Refuted by all 3 verifiers on
  fixable defects B1/B2/B3, which gate its S1.

**They are authoritative for their *decisions*, not for their *code claims*.** Two of their code claims are
false and are corrected below (GRS §11.5 → item 8; GRS §12 pivot count → item 20). The same lens is now
turned on CLAUDE.md itself, which produced item 1 — the highest-ranked item on this list.

### What changed after adversarial challenge

Two challenge rounds attacked this list. Both landed. Every claim below was re-verified against code by
reading the file, not by trusting the report.

| Change | Verdict | Proof |
|---|---|---|
| **NEW item 1** — CLAUDE.md:315 is itself a corruption footgun | **ACCEPTED** | `scene_graph.rs:426` — `SdfObject` is index **11**, after `VoxelBody` (10). CLAUDE.md:315 says VoxelBody "must stay last". Verified. |
| **NEW item 2** — 1309 uncommitted lines at risk | **ACCEPTED** | `git diff --shortstat` in `.claude/worktrees/ui-polish` = `10 files changed, 1309 insertions(+), 257 deletions(-)`; no matching stash. Verified. |
| **Item 5 mechanism was wrong** — it is redo-stack annihilation, not "eats the previous action" | **ACCEPTED** | `types_transform.rs:76` — `push()` ends `self.redo_stack.clear()` unconditionally. Verified. |
| **Item 5's prescribed fix was broken** — "pop a no-op" cannot work | **ACCEPTED** | By the time you pop, `:76` already cleared redo. Only refuse-before-push is viable. Verified. |
| **Item 5 → #1** | **REJECTED — see below** | It is rung 3. Items 1/3/4 are rung 1 and cheaper. |
| **Item 7 was #1 on a false severity** | **ACCEPTED** | `keybindings.rs:511` — `if *current != dflt` means only non-default binds serialize. Verified. |
| **Item 6 must outrank item 7** | **ACCEPTED** | Same rung once 7 is relabeled; 6 destroys authored artwork. |
| **Item 15 (S0) needs a dual-branch acceptance criterion** | **ACCEPTED** | `transform.rs:163-166` free_mode branch carries the same two defects independently. Verified. |
| **Items 32/33 need a target decision first** | **ACCEPTED** | A second outliner already exists at `feat/ui-polish:ui2/mod.rs:414` with its own row abstraction at `:711`. Verified. |
| **The GRS S0→S3 chain should be reordered** | **REFUTED by the challenger itself** | Confirmed and *strengthened* — see §4. |

**On rejecting "item 5 → #1":** the challenger's *substance* is accepted in full — the mechanism is
corrected, the broken half of the fix is struck, and a new acceptance test is added. Only the *rank* is
rejected, on the ladder: losing the redo stack is rung 3 ("undo/redo regression"), not rung 1. Items 1 and
3 are rung 1, and items 1/2/4 together cost roughly half a day. Item 5 is the first real engineering item
on the list and sits at #5 behind them. This is a decision, not a compromise.

**The generalizable lesson from round 1:** this list verified call *sites* rigorously but not the *shared
primitives* they call into. Both top-of-list errors lived one hop below the cited line — inside `push()`
and inside `save()`. **Follow the mutation one level deeper than the call site.**

**The generalizable lesson from round 2:** all sweeps read master, and all sweeps treated CLAUDE.md as the
authority rather than as a claim. Both blind spots produced a real item.

---

## TIER 0 — DO NOW

### 1. CLAUDE.md:315 is a data-corruption footgun — the rule arms the class it exists to prevent

**Rung 1 (armed, latent — not live) · Size XS (~2 lines) · blocked_by: nothing**

CLAUDE.md:315 states verbatim: *"`NodePayload::VoxelBody` must stay last."* **This is false.**

Verified variant order in `core_math/src/scene_graph.rs`: Root 0 (`:280`), Group 1 (`:283`), SdfBody 2
(`:287`), ImportedMesh 3 (`:348`), Light 4 (`:355`), Camera 5 (`:358`), PolygonalBody 6 (`:368`), NurbsBody
7 (`:379`), Material 8 (`:387`), Empty 9 (`:394`), VoxelBody 10 (`:405`), **SdfObject 11 (`:426`)**.

The code's own comment at `:425` says *"Appended at index 11 (after VoxelBody)"*, and `:400-404` correctly
documents the tail as *"VoxelBody = index 10, SdfObject = index 11"*. **CLAUDE.md contradicts the code it
governs.**

Why this is rung 1 and not doc-drift: the CLAUDE.md preamble makes these instructions binding — *"These
instructions OVERRIDE any default behavior and you MUST follow them exactly as written."* A dev who appends
a variant and obeys the rule **literally** places it after VoxelBody but **before** SdfObject, shifting
SdfObject 11→12 and corrupting every existing `.fl4` containing an SdfObject — a shipped feature
(`ba6be84` "SdfObject foundation").

Second stale fact, same file: CLAUDE.md:66 says *"Save format v6"*; `app/src/project.rs:630` writes
`version: 7`.

**Honest severity note:** this is **latent, not live**. Nothing is corrupt today. It is ranked first
because it is ~2 lines and it is the only item on this list that can silently arm a rung-1 bug in someone
else's future task. Item 14 does **not** cover it — that triages MASTER_WORK_LIST and the bug tracker, not
the rules file.

**Fix:** `:315` → "append AFTER `SdfObject` (index 11) only"; `:66` → v7.

---

### 2. Commit the 1309 uncommitted lines in the `ui-polish` worktree

**Risk retirement (not a ladder item) · Size XS · blocked_by: nothing**

`.claude/worktrees/ui-polish` holds work that **exists on no branch, in no stash, and in no commit.**

Verified: `git diff --shortstat` = `10 files changed, 1309 insertions(+), 257 deletions(-)` — `ui2/mod.rs`
+1236, `outliner_actions.rs` +190, plus `app_new`/`app_update`/`body_library`/`outliner_panel`/
`prim_actions`/`properties_sdf_shape`/`ui_panels`/`ui_theme`. `git stash list` shows 4 stashes, **none of
them this** (newest is 2026-07-02 on `feat/sculpt-tool`). `git status --short` shows zero untracked files —
it is all tracked-file modification, so a single `git checkout .`, `git clean`, or `git worktree prune`
destroys it silently.

Committing **on the branch** does not violate the owner's "do NOT merge the mockup UI work to master until
approved" rule (memory `project_ui_redesign_stays_on_fix_ui`) — committing on `feat/ui-polish` is the
opposite of merging.

**This was invisible to every sweep because all of them read master.**

---

### 3. `dissolve_edge` — guard the same-face slit (poly audit #11)

**Rung 1 · Size S · blocked_by: nothing**

`dissolve_edge` guards `he.twin == NO_TWIN` and `face_a == NO_FACE || face_b == NO_FACE` but **never checks
`face_a == face_b`**. Dissolving a slit edge splices the ring into itself and destroys the face.
User-reachable via the X-menu.

The real prize: `apply_in_place` is *deliberately* still gated on the weak `validate()` instead of
`is_manifold()` **because** #11 and #8 are buggy. This bug is what holds the strong install gate open —
and that gate is what stops silent corruption from installing as a class. Closing this + item 41 lets the
gate be tightened.

**Evidence:** VERIFIED — `core_math/src/half_edge.rs:2049-2055` (guards read in full);
`grep -c "face_a == face_b"` over the file = **0**. Doc: `POLY_POLISH_PLAN.md` §4 Phase 2b (which ranks it
rung 4 — **under-claimed**, promoted here).

---

### 4. Restore the 4 parked bevel-inverse proptest seeds — resolve an UNKNOWN

**Rung 1 (potentially) · Size S to determine; unknown to fix · blocked_by: nothing**

`bevel_apply_inverse_apply_is_identity` is green today — and green carries **zero information**, because
the 4 seeds that made it fail were moved out of proptest's auto-load directory into a docs `.txt`.

Verified byte-for-byte: the parked file holds 4 `cc` seeds with `edge_pick`/`width` params (this test); the
live auto-loaded file holds **1 unrelated seed** with `face_pick`/`d` params (a *different* test). The gate
is defanged. The parked file's own line 11 names the cause — *"same pre-existing mesh_ops OpDelta-inverse
bug; cube single-edge bevel"*.

This is POLY_FOUNDATION Phase 1's **HARD MERGE GATE** — *"nothing in phases 2+ merges until the
apply→inverse→apply proptest is green"* — and phases 2 and 3 have already merged past it while it was
defanged.

One-line fixture change → either the bug is fixed (delete the parked file + the BEVEL.md §8 note) or you
have live undo/geometry corruption. **Honest status: UNKNOWN.** That is exactly why it is cheap and ranked
here — it reshapes the rest of the list.

**Evidence:** parked = `docs/planning/poly/KNOWN_bevel_inverse_regression_seeds.txt`; live =
`core_engine/core_math/proptest-regressions/mesh_ops/test_spine.txt`. Test:
`core_math/src/mesh_ops/test_spine.rs:252`, wired at `mesh_ops/mod.rs:21`, not `#[ignore]`d. Docs:
`BEVEL.md:534-537`, `POLY_FOUNDATION_PLAN.md:301`.

---

### 5. The no-op `transaction()` class — a refused op silently **destroys the entire redo stack**

**Rung 3 · Size M · blocked_by: nothing**

**The mechanism is NOT what was originally filed.** `undo()` pops exactly one entry
(`app/src/undo.rs:86`), so a no-op entry restores an identical state — one dead Ctrl+Z, stack stays
consistent. That alone would demote this item.

The real harm is worse and was entirely unfiled. Verified `app_types/src/types_transform.rs:73-77`:

```rust
pub fn push(&mut self, e: UndoEntry) {
    self.undo_stack.push_back(e);
    if self.undo_stack.len() > self.max_entries { self.undo_stack.pop_front(); }
    self.redo_stack.clear();          // ← unconditional
}
```

So a refused op pushes an empty entry and **annihilates the entire redo stack** — silently, immediately
after an operation that visibly did nothing. It also evicts the oldest real entry at the 50-entry cap
(`UndoHistory::new(50)`, `app/src/app_new.rs:196`).

`transaction()` (`undo.rs:362-366`) snapshots → `f(self)` → `self.history.push(entry)` **unconditionally**.
It has no notion of "the closure mutated nothing". Any op with a legitimate refuse path leaves an empty
entry. **3 confirmed sites against 156 `transaction()` call sites — 3 is a floor, not a ceiling; the audit
sweep is part of the task.**

**THE PRESCRIBED FIX IS CORRECTED — the "pop a no-op" variant CANNOT WORK.** By the time you pop, `:76`
has already cleared redo, and popping the entry back cannot restore it. **Only refuse-before-push is
viable.** Both correct patterns already ship in-repo, and both are of that family: dry-run-on-scratch
(`poly/circle.rs:53-60`) and refuse-before-record (`undo.rs:403`) — the list's own evidence contradicted
its own alternative.

CLAUDE.md's No-Bandaids test #1 forces the architectural fix (a `try_transaction` that only pushes when the
closure reports work) over per-site patches.

**ACCEPTANCE TEST (new):** *the redo stack survives a refused op.*

**Evidence:** VERIFIED by reading both files — `app/src/undo.rs:362-366`, `:86`;
`app_types/src/types_transform.rs:73-77`. Refusing callers: `poly/inset.rs:58-70`; `poly/fill.rs:23`,
`:37-38`, `:73-79`; `poly/bridge.rs:16`, `:133-135`. Refuse is reachable: `half_edge.rs:1506`/`:1510`.
Filed `COMPLETE_BUG_TRACKER.md:55-67` (with the wrong mechanism — correct it).

---

### 6. Voxel paint colour is silently destroyed by save/load

**Rung 3 · Size M · blocked_by: nothing**

`VoxelGrid.colors: Vec<u32>` is user-authored via the "Paint Color" checkbox + brush colour picker.
`VoxelGridRecord` carries only `{res, bounds, iso, payload}` — **verified, no `colors` field**. Paint →
save → reload → every painted voxel reverts. The field's own doc comment admits it: *"Not serialised
(save/load drops paint colour until the persistence side-channel is extended)."*

This destroys **authored artwork** irreversibly. It ranks above item 7 (same rung; item 7 destroys a
re-bindable preference).

**CONSTRAINT — do not get this wrong:** do **NOT** add a field to `VoxelGridRecord`. It is
bincode-serialized and that corrupts every existing `.fl4`; `#[serde(default)]` is inert under bincode. The
fix rides a NEW parallel record in the `FL4ORDER` side-channel, following the `VoxelNodeRecord` precedent.

**Evidence:** `core_math/src/voxel_sculpt.rs:1452`, `:1454-1455` (the admission), `:1484-1490`
(VoxelGridRecord — verified). Entry: `app/src/ui_panels.rs:624`, `:637-639`. Precedent:
`app/src/project.rs:93-96`. Trap: memory `project_bincode_save_format_field_trap`.

---

### 7. Keymap key round-trip — remapped keys silently evaporate on restart

**Rung 3 (relabeled from rung 1) · Size S · blocked_by: record owner decision D2 in the INPUT plan first**

`to_json()` serializes a *display glyph*; `key_display()` ends in `_ => "?"`; `key_from_str()` ends in
`_ => return None`; `load()` silently skips the None.

**THE ORIGINAL SEVERITY DOES NOT SURVIVE READING `save()`.** Verified `keybindings.rs:511` —
`if *current != dflt` means **only non-default binds are serialized.** The two shipped defaults this item
originally led with (`ACT_POLY_GROW_SEL = ctrl(Plus)`, `ACT_POLY_SHRINK_SEL = ctrl(Minus)`, `:436-437`)
therefore **never reach `to_json`**. There is zero data loss from them. The "Ctrl+?" in the Shortcut Editor
is real but **purely cosmetic**.

Real loss **is** reachable: `app/src/input_keyboard.rs:77` captures the raw `egui::Event::Key`, so a user
can bind any key → set → save → `"key":"?"` → `key_from_str` returns None at `:40` → `load` skips at
`:528` → silent revert. But the loss is **one preference, re-bindable in five seconds.** That is not rung 1
"data corruption" in a DCC. It is rung 3 at most.

**This item keeps its early slot on honest grounds only: size S, and it is the hard blocker for item 30
(INPUT S1–S4). Never on severity.** Rung labels drive behavior, and this was the exact over-claim pattern
that produced the earlier "Ctrl+Shift+S is rung 1 destructive" error.

**Fix:** split serialization from display — an exhaustive `key_id`/`key_from_id` with **no `_` arm** (a new
`egui::Key` becomes a compile error) + `every_key_round_trips` + `every_default_binding_round_trips` tests.
The existing test is inert (`let _ = bind.label();`).

**Blocker detail:** owner decision D2 = **LOGICAL** key mapping (printed letter, not physical position) is
already chosen but **not recorded in `INPUT_ARCHITECTURE_PLAN.md` §10**. The answer freezes into the v3
disk format this item ships. 2-minute doc edit — a checklist item, not a real blocker.

**Evidence:** `core_engine/app_types/src/keybindings.rs:24`, `:40`, `:71`, `:436-437`, `:511`, `:528`.
Inert test: `app/src/types/mod.rs:143-149`. Doc: INPUT plan §6.3, §8 S0. **Citation drift:** the plan cites
`app/src/types/` — real path is `app_types/` (cross-crate); and `:429-430` — real lines `:436-437`.

---

### 8. Poly Transform + Material sliders: TYPED values have no undo (drags do)

**Rung 3 · Size S · blocked_by: nothing**

**Scope is much narrower than three sweeps AND `GRS_PARITY_DECISIONS.md` §11.5 claimed.** §11.5 says
verbatim *"with only `mark_dirty(RENDER)` and no transaction"* — **false as written.** Verified by reading
the file: `begin_episode("Transform Poly")` **is** called on `drag_started()` at `:27`/`:32`/`:37`, and
`auto_commit_episodes` commits on pointer-up (`undo.rs:474-481`). A *drag* is correctly one undo entry, and
an episode transaction is explicitly permitted by CLAUDE.md's Transaction Model.

**The real hole:** an egui `Slider`'s numeric text field can be edited by **keyboard**, which fires
`changed()` without `drag_started()` — no episode opens, and the write at `:152-157` lands with no undo
entry at all. Same shape on the Material combo at `:158-163`.

**Fix:** `transaction_snapshot_only("Transform Poly")` on the changed-without-episode path — the pattern
already used 120 lines below in the same function (`:281`).

Doing this before GRS S1 is practical: S1's falsifiable gate (REPRO A/B/C) requires setting Pos/Rot/Scl
through these exact sliders. Make the measuring instrument trustworthy first.

**GRS §11.5 should be corrected.**

**Evidence:** VERIFIED — `app/src/properties_poly.rs:25-39`, `:152-157`, `:158-163`, `:281`.

---

## TIER 1 — HOTFIX THE INPUT COLLISIONS

**Rung 4, all S/M, owner hits these every session.** Precedent: `da2b46b` (Ctrl+Shift+S) shipped
yesterday, and the INPUT plan's own ruling — *"Yes — standalone hotfix now. The priority ladder beats
architecture."*

**These are independent of INPUT S1 and of B1/B2/B3.** They are copy-paste/dispatch defects, not
keymap-architecture questions. Item 10's independence is stronger than it looks: its root cause is a
mode-field desync in `poly_selection.rs`, which no keymap slice touches. Item 22's is a false *string*.
**Do not let them wait for the architecture.**

### 9. `F` double-fires in Poly Vertex mode — fills a face AND yanks the camera

**Rung 4 · Size S · blocked_by: nothing**

`ACT_FOCUS => simple(F)` and `ACT_POLY_FILL => simple(F)` are both shipped defaults, both dispatched, and
there is **no key-consumption model anywhere** (`grep consume_key|consume_shortcut` = 0). The F pie is
gated only on `!f_pie.open && !is_transform()` — no surface check, no mode check. Select boundary verts,
press F: face created **and** pie opens; release = QuickTap = `frame_selected` = camera jumps.

**COMPOUNDING DEFECT (found by challenge; the reason this is not merely cosmetic):** `poly_fill_face()`
routes to `poly/fill.rs:23`'s `transaction("Fill Face")`, which can refuse **inside** the closure at
`:37-38` on a degenerate plane. So one F press on a bad vertex selection = fill refuses **+ the entire redo
stack is destroyed** (item 5, `types_transform.rs:76`) **+ the camera yanks.** Items 9 and 5 stack into a
single keypress.

**Evidence:** VERIFIED — `app_types/src/keybindings.rs:334`, `:415`; dispatch `app/src/poly/key_dispatch.rs:97`
(gated on `poly_work_mode == Vertex`) vs `app/src/input_keyboard.rs:828` (ungated). INPUT plan §2.5c (C-4).

### 10. `Ctrl+I` in poly edit inverts the selection AND creates a phantom instance

**Rung 4 · Size S · blocked_by: nothing**

Root cause VERIFIED: `poly_switch_work_mode()` sets `self.poly_work_mode` but **never** sets
`self.tool.work_mode`, so `tool.work_mode == Object` is true while `poly_work_mode == Vertex` — both guards
pass. The H twin at `input_keyboard.rs:288-289` carries the `&& !poly_sub_mode` guard that was simply
forgotten here. (The real fix — collapsing the four unsynchronized mode fields — blows the refactor budget;
INPUT plan §9.10.)

**Evidence:** VERIFIED — `app_types/src/keybindings.rs:348`, `:435`; `app/src/input_keyboard.rs:249-251`,
`:1362`, `:288-289`; root cause `app/src/poly_selection.rs:1089-1128`.

### 11. Unify the copy-pasted "is a pie open?" guards — two are LIVE bugs

**Rung 4 · Size M · blocked_by: nothing**

**11 copy-pasted sites** (6 guard chains + 5 "close other pies" blocks) against 7 pies; no two agree.
**Live bug A:** the G/R/S arm-gizmo guard omits `d_pie`, and D is hold-to-open → hold D + press G arms the
gizmo behind the open pie. **Live bug B:** **not one** close-block closes `bool_pie` or `sdf_bool_pie` →
hold X then press Z = two pies on screen. Fix as a class: one `any_pie_open()` + one
`close_all_pies_except()`, so pie #8 cannot miss a site.

**Evidence:** guard chains `app/src/input_keyboard.rs:933`, `:938`, `:950` (**missing d_pie = bug A**),
`:972`, `:1139-1140`, `:1226-1227` (the only COMPLETE one). Close-blocks `:743-745`, `:786-788`, `:830-832`,
`:857-859`, `:1051-1052`. Hold-to-open proof: `:1063`, `:1156`.
⚠️ A source report aimed this at `:1226` claiming it was incomplete — **BACKWARDS**; `:1226` is the complete
one. Don't look there.

### 12. `find_conflicts` is unsound — it is why all 15 collisions shipped

**Rung 4 · Size S (reduced form) · blocked_by: nothing**

It caught neither Ctrl+Shift+S nor Ctrl+I. The detector compares free-text `context` *prose* with `!=`,
giving both false positives and false negatives. Until it is sound, **every new bind must be hand-grepped.**
Replace string-eq with mask-overlap; the 15 real default collisions surface as **advisory** with zero
dispatch risk. Full soundness wants INPUT S1's `When`/mask types, but the string-eq lie can be replaced
independently.

**Evidence:** `app_types/src/keybindings.rs:486-496`, decisive line `:492`. INPUT plan §8 S2, §2.6.

### 13. Move Alt+C (Circle) from the dead InputSystem registry to ACTION_DEFS

**Rung 4 · Size S · blocked_by: nothing**

Fresh drift created yesterday by the Circle merge — fix it while it is **one entry**, not a pattern. Circle
registered Alt+C in InputSystem: 22 registrations, **zero persistence**, never read by the rebind editor.
So Alt+C is not rebindable, not saved to `keybinds.json`, invisible in the Shortcut Editor. This is exactly
what the just-rewritten CLAUDE.md Input rule forbids — the rule was rewritten at `e6d3f9e` *because* it
pointed at the wrong registry. INPUT S4 kills the second registry wholesale; this one entry should not wait.

**Evidence:** `app/src/input_system.rs:69`, `:369-370`. ACTION_DEFS = 97 vs InputSystem = 22.

### 14. Status pass on MASTER_WORK_LIST.md + triage pass on COMPLETE_BUG_TRACKER.md

**Rung 4 (it *is* the priority-source repair) · Size M · blocked_by: nothing**

CLAUDE.md says *"Priorities come from MASTER_WORK_LIST.md and COMPLETE_BUG_TRACKER.md"* — **and neither is
currently trustworthy.** ~half of the tracker's 223 open markers (116 📋 + 107 ⏸️) reference subsystems that
no longer exist (`graphics/src/glsl/`, `raymarch_entry.wgsl`, `compute.wgsl`, `brick_pool.rs` — all
verified deleted). Every `CR-*` line number is impossible (`main.rs` is 562 lines; the block cites
`:570-794`, `:1137`, `:1237`). The work list still shows P0.2 and P0.5 as open when both shipped. The
tracker went stale **within one day** (an item filed 2026-07-15 was already done). The work list itself
asked for this pass at `MASTER_WORK_LIST.md:232` and it never happened.

Cheap, and it is what makes every ranking below this line defensible. Full already-fixed and dead-entry
inventories are in §6.

**Note:** this does **not** cover CLAUDE.md — that is item 1.

---

## TIER 2 — THE GRS CHAIN

**Rung 4. The order is load-bearing. Do not reorder — see §4.**

### 15. GRS S0 — fix `compute_transform` rotate: uniform RH+ sign + view-awareness (SDF only)

**Rung 4 · Size S · blocked_by: nothing — and it MUST precede S3**

Head of the defended chain, and cheap. SDF is the broken pillar two ways:

(a) it negates only Z (`transform.rs:171` X → RH+, `:176` Y → RH+, `:177` Z → `-angle` = RH−) — exactly one
of three axes flipped is the signature of a symptom-patch, not a handedness fix; the Blender oracle negates
all three identically.

(b) `perpendicular_axis()` takes `.abs()`, so SDF free-rotate is structurally **view-blind** — orbit 180°
and the object turns opposite the mouse — while poly signs by `cam_fwd.z.signum()`.

**Fix:** drop the `-` on Z; adopt poly's view-aware `signum()`. Owner-visible: SDF R,Z now spins the other
way.

**⚠️ NEW ACCEPTANCE CRITERION (from challenge — this item would otherwise silently under-fix):** **both
`AxisConstraint::None` sub-branches must be sign-corrected in the same commit.** Verified
`transform.rs:163-166` — the `free_mode` branch is
`Vec3::new(angle * fwd.x.abs(), angle * fwd.y.abs(), -angle * fwd.z.abs())`, carrying **the same two
defects independently**: `.abs()` on all three components AND the lone Z negation. This matters because
**item 19 (S4) is the slice that wires free_mode into the math (D3)**. If S0 fixes only `:168-172`, S4
inherits a view-blind, Z-negated free-rotate at the exact moment it makes free_mode load-bearing — four
slices downstream, where the regression will read as S4's fault. Size unchanged (still S).

**Behavior-NEUTRAL for poly — now PROVEN, not asserted.** Verified: `grep "compute_transform("` across
`core_engine` returns 9 call sites in `gizmo.rs` plus **exactly one** outside it —
`poly_drag_tools.rs:633` — and that call passes `kind: TransformKind::Grab` (`:625`), so it can never enter
the Rotate branch S0 edits.

**Evidence:** `app/src/transform.rs:163-166`, `:168-172`, `:171`, `:177`; `app_types/src/types_gizmo.rs:90-99`.
Doc: GRS §3, §6 S0.

### 16. GRS S1 — D6: poly runs world math on body-local verts (+ mirror co-fix, + reuse `poly_selection_center`)

**Rung 4 · Size M · blocked_by: item 15 (S0)**

**The highest-frequency bug in the app.** `poly_update_transform` feeds a LOCAL center as `original_pos`
against the WORLD camera and adds a WORLD delta onto LOCAL verts, with a hand-rolled centroid from raw
local verts. Every other poly subsystem (render, picking, gizmo placement, poly_join) applies
`world_transform` correctly — only this one doesn't, so the user picks the right vertex, sees the gizmo in
the right place, and the drag misbehaves.

**CRITICAL FREQUENCY FINDING:** `creation.rs:342-345` writes `node.local_position = a.pos` at placement, so
*every poly body created away from the origin already has a non-identity transform* — REPRO A fires on
essentially every poly body.

Falsifiable today: Pos Y=5 → G overshoots 1.5× (Y=10 → 3×); Rot Y=90 → G+X moves along world −Z; Scl=2 →
2× cursor.

**Fix:** world math end-to-end, map back with M⁻¹ per vert (copy `gizmo.rs:1282-1296` / `:1639-1649`);
delete the hand-rolled centroid for `poly_selection_center()` (CLAUDE.md REUSE RULE).

**Mirror is NOT separable** — it reflects a LOCAL delta across a WORLD normal, so fixing D6 alone ships a
new bug. Same commit.

**REGRESSION CHECK:** uniform scale is CORRECT today under any body transform — do not regress it.

**Rung note, honestly:** this is rung 4, not higher. It corrupts no source of truth; undo, selection, and
save-load all work. Its weight is **frequency**, not severity — and frequency does not promote across rungs.

**Evidence:** `app/src/poly_drag_tools.rs:517-530` (centroid, duplicates `selection.rs:138-151`), `:626`,
`:663`, `:710`/`:723` (mirror); `app/src/creation.rs:342-345`; `app/src/types_mirror.rs:143`. Doc: GRS §4,
§6 S1, §9.

### 17. GRS S2 — reject Elongate on poly explicitly (toast + no-op)

**Rung 4 · Size S · blocked_by: item 16 (doc's fixed slice order; the fix itself is independent)**

Wrong TODAY, before any migration. `TransformKind::Scale | TransformKind::Elongate` alias to the same
branch, so Elongate silently behaves as Scale. Post-S3 it becomes silent geometry nonsense:
`compute_transform`'s Elongate branch returns `original_scale` unchanged (ONE → verts freeze) **and** a
non-zero `elongate_pos_offset` folded into `result.position` (→ verts translate). Loud rejection removes a
migration-created corruption path in advance.

**Evidence:** `app/src/poly_drag_tools.rs:672`; `app/src/transform.rs:208-248`; `app/src/gizmo.rs:1216`,
`:1426`. Doc: GRS §6 S2 (D7, "promoted to BLOCKING").

### 18. GRS S3 — THE UNIFY: poly rotate+scale onto `compute_transform`; delete `ActiveTool::PolyTransform`

**Rung 4 · Size L (multi-session) · blocked_by: items 15, 16, 17**

The dangerous slice, and the answer to the owner's ruling *"the g,r,s do not have same behavior as the sdf
g,r,s THEY SHOULD BE THE SAME"* — make it ONE code path.

Move rotate+scale into `update_gizmo_drag`'s poly branch via the identity-originals trick already proven at
`poly_drag_tools.rs:626`; the poly branch **early-returns** (like the light branch) carrying
`poly_sync_mesh_cache` + `mark_dirty(RENDER)` — it must NOT fall through to the unconditional
`prim_buf_stale` (meaningless for poly), and `finish_gizmo_drag` must early-return before `rebake_no_undo`.

**MUST CARRY, SAME COMMIT:**

**(a) The cancel vert-restore port.** `cancel_gizmo_drag` has no poly branch, so a poly `ActiveTool::Gizmo`
with all three target vecs empty hits the fallthrough that rolls back the episode **while the verts stay
moved** = graph and undo stack disagree = silent geometry corruption. Port the restore (3 vecs +
`recompute_normals`, verts restored BEFORE `cancel_episode`).
**This must NOT be fixed before S3** — poly is never `ActiveTool::Gizmo` today, so a poly branch added early
is **unreachable by construction**, and you would hand-build the exact fossil the migration exists to
delete. A stall there leaves a permanent artifact that *reads as intentional*. CLAUDE.md's "never leave a
functionality gap for even one commit" is satisfied by same-commit, not before-commit.

**(b) The poly HUD re-key.** Poly HAS a HUD; `ActiveTool::Gizmo` has ZERO. Regression risk, not
enhancement.

**(c) Delete `ActiveTool::PolyTransform` + its accessors** (cross-crate, in `app_types`) so the compiler
enumerates all 24 sites / 10 files. **The `_ => None` accessor is exactly what fossilized the LAST run of
this migration with a fully green build** (~150 unreachable lines still compiling at `gizmo.rs:619-771`).
**A red build here is a SAFE stall; a green one is the failure mode.**

**Evidence:** `app_types/src/types_tool.rs:57`, `:179`, `:279-283`; `app/src/gizmo.rs:1601-1655` (no poly
branch; `:1654` fallthrough), `:619-771` (the fossil), `:1568`, `:1598`; restore source
`app/src/poly_drag_tools.rs:742-756` (**ORDER MATTERS**). Doc: GRS §6 S3, §7, §9.

### 19. GRS S4 — lift the Alt+LMB gate in poly edit (owner-requested) + wire snap/free_mode honestly

**Rung 4 · Size M · blocked_by: item 18 (S3)**

Last, because wiring `free_mode` is the one change that alters math on path B only. Owner asked: *"alt click
in viewport it move with the workplane directio free mode no need to use gizmo"* — VERIFIED, this is a
**GATE LIFT, not a feature**: `input_viewport.rs:1258-1276` already routes poly to free-move (sets
`ts.free_mode = true`) when the gizmo is armed and you click empty space; the Alt+LMB branch at `:1282` is
an `else if` below it carrying `&& !in_sub_edit`.

**MANDATORY per the decision record:** lift the gate AND align both free-move entries on `free_mode = true`
in the SAME slice — the disagreement is cosmetic today (draw only) but becomes **two different rotations
for the same gesture** once D3 wires free_mode into the math.

Also plumb `snap_active`/`snap_grid` with `snap_pos: None` for sub-element.

**ANNOUNCE:** off-axis views will feel different (free_mode gives a 3-axis camera-blended euler vs today's
snapped single axis).

**Evidence:** `app/src/input_viewport.rs:1282` (verified verbatim), `:1258-1276`;
`app/src/poly_drag_tools.rs:630` (`snap_active: false, free_mode: false` hardcoded — the 25%-migration
fossil); `app/src/draw_gizmo.rs:276`, `:443`. Doc: GRS §2 GATE, §6 S4.

---

## TIER 3 — AFTER S3

### 20. FILE + fix the SDF pivot defect — the gizmo is DRAWN at the pivot, the math ignores it

**Rung 4 · Size M · blocked_by: nothing structurally — but sequence after item 18 (S3)**

A shipped, owner-visible, reproducible bug that **appears nowhere in the tracker.** GRS §7 explicitly
ordered it filed and it never was. It only survives because it is written into a design doc; if that doc is
not read, the bug is invisible. **Filing it is part of the item.**

REPRO (today, no migration needed): select an SDF body → `D` → "Origin" → `pivot.mode = WorldOrigin` inside
`transaction("Set Pivot")` → `selection_median()` returns `Vec3::ZERO` → **the gizmo visibly jumps to world
(0,0,0)** → press `R` → **the body spins about its own origin, not the drawn gizmo.**

The feature is fully wired — PivotMode ships, is user-reachable, is undoable, is resolved correctly, is
drawn, and is captured into every `GizmoInteraction.median_start_pos` — and then `median_pos` is applied
**only `if multi_body`**, so the single-node case (the normal case) always ignores it; the prim branch
hardcodes the SdfBody origin, in **two verbatim copies**.

Do NOT fold this into S0–S4 (CLAUDE.md "Fix ONE bug per task"; GRS §7 is explicit). **It edits
`gizmo.rs:1475`/`:1498-1505`, exactly what S3 rewrites — sequence after S3 or accept a merge conflict.**

**Evidence:** VERIFIED — `grep -c pivot` in `app/src/transform.rs` = **0**, in `app/src/poly_drag_tools.rs`
= **0**; the only `pivot` hit in `gizmo.rs` is a COMMENT at `:1474`; `:1460`, `:1475` (gate), `:1498-1505`
(hardcoded body origin), `:1096-1136` (second copy). Correct resolver: `app/src/selection.rs:61-83`. Drawn:
`app/src/draw_gizmo.rs:288`, `:450`. Pie: `app/src/pivot.rs:184-185`, `:259`. Doc: GRS §7. **Absent from
COMPLETE_BUG_TRACKER.md.**
**Citation correction:** GRS §12 claims *"`node.pivot`/`.pivot.mode`: 0 times in gizmo.rs"* — actual 1 hit,
but it is a comment, so the substance holds.

---

## TIER 4 — THE UI LIES

**Rung 4, all S.** Same class throughout: **the UI advertises a capability that cannot fire.** Could be
batched as one "dead UI audit" pass if the owner prefers. Order within tier = by how often the owner meets
it.

### 21. Poly modifier stack silently drops 5 of its 8 modifiers

**Rung 4 · Size S (gate) / L (implement) · blocked_by: nothing**

User adds "Smooth", gets a modifier card with live sliders, tweaks it, **nothing happens** — no toast, no
warning — and the dead entry then persists through save/load (`PolyModifier` is `Serialize`). Same stub
class as the Solidify no-op fixed 2026-07-07 by wiring this exact match.

**MECHANISM — two sweeps got this wrong in opposite directions.** Verified:
`grep "pub fn (bevel_edges|smooth|triangulate|decimate|array)\w*\("` over `half_edge.rs` returns **only**
`bevel_edges:3004` and `bevel_edges_ex:3050` — and *those are the deprecated wrong-algorithm pair slated for
deletion* (item 39); the live bevel is `bevel_op::bevel_edges_offset`, which needs an edge-set a modifier
stack cannot supply. So: **Smooth/Triangulate/Decimate/Array = no kernel at all; Bevel = no *usable*
kernel.**

**The honest minimal fix is "stop offering them", not "restore the dispatch":** gate the dropdown to
implemented types via an **exhaustive match (not `_`)**, so the next added variant cannot silently regress.
Implementing all five is L and a separate decision.

**Evidence:** VERIFIED by reading both files — `app/src/poly/mesh_sync.rs:68-84` (`_ => {}` at `:82`, only
Mirror/Subdivide/Solidify arms); `app/src/properties_poly.rs:179-188` (all 8 offered), sliders `:247-254`;
`core_math/src/scene_graph.rs:197-206` (8 variants). **In no plan doc; deserves a tracker entry.**

### 22. Hint row lies — "Shift: Fine" is a third dead key, and 5 modal tools fall through

**Rung 4 · Size S · blocked_by: nothing. Soft link: do (a) BEFORE item 18 (S3).**

The hint module declares its own invariant at `viewport_hints.rs:16-18` — *"every key listed here is
verified against the real dispatch site. A wrong hint is worse than none"* — and violates it three ways.

**(a)** The Transform and Gizmo rows both advertise `("Shift","Fine")`; **no fine/precision mode exists
anywhere** (`grep -in "fine"` over `transform.rs` + `gizmo.rs` = 0; no `fine_mode`/`precision_mode`/
`fine_factor` in `app/src` or `app_types/src`). Same defect class fixed twice yesterday (`8a1c0a1`,
`965059f`), and the module's own comment at `:124-126` documents removing this exact hint from the
PolyTransform arm — the two rows above were left.

**(b)** Five modal tools have a live `ActiveTool` variant + accessor but **no arm**, so while they run the
row shows idle work-mode keys the modal has reassigned.

**(c)** Two other sites paint their own hint strings and **disagree** with it.

**Soft link:** (a) touches the same rows S3 must re-key onto the gizmo variant. Doing it first is safe and
makes S3's re-key cheaper. **Doing them in the same task is not** (refactor budget).

**Evidence:** (a) `app/src/viewport_hints.rs:51`, `:55`; precedent `:124-126`. ⚠️ `:127` is **already
fixed** — don't re-audit it. (b) `app_types/src/types_tool.rs:187`, `:188`, `:163`, `:185`, `:174` — none
appear in `viewport_hints.rs`. (c) `app/src/draw_create.rs:68-69`; `app/src/draw_poly.rs:478` (Knife,
duplicates `viewport_hints.rs:144-147`). ⚠️ A source report cited `draw_poly.rs:582` — **that site does not
exist.**

### 23. Dead X-Ray toggle button + dead `poly_wire_xray` field

**Rung 4 · Size S · blocked_by: nothing**

The viewport bar has an "X-Ray" toggle that **writes `viewport.xray_mode` and marks RENDER dirty — and
nothing reads it.** A user-facing control that does nothing. The LIVE x-ray flag is `sel.visible_only`,
which already has two working UIs — so the user faces three X-Ray-looking controls, one of which is a lie.
Fix both together: re-key the button onto `sel.visible_only` (the code already asserts ONE X-Ray concept for
display AND picking) and delete both dead fields.

**Evidence:** dead writer `app/src/viewport_overlays.rs:342-345`; declared `app_types/src/types_viewport.rs:129`,
defaulted `:181`; **zero readers** — `app/src/overlay_lines.rs:67` mentions it only in a comment explaining
it is deliberately NOT used. Live flag: `overlay_lines.rs:72`, `poly_selection.rs:324`/`:399`; working UIs
`properties_render.rs:265-268` + header toggle; action `input_system.rs:387`. Dead field:
`app_types/src/types_render.rs:96`, `:217`.

### 24. Viewport "Depth of Field" panel is entirely dead UI — 4 controls wired to nothing

**Rung 4 · Size S · blocked_by: nothing**

An "Enable DOF" checkbox + Focus Distance / Aperture / Max Blur sliders, hover-texted *"Viewport
depth-of-field (Lit/PBR modes)"*. **None of the four fields is ever read** — they do not appear in the
`RenderConfig` literal (which carries only the unrelated path-tracer `pt_dof_aperture`). Meanwhile the
post-process DOF pass reads `config.sel_rim_base` as its focus distance, and `sel_rim_base` is hardcoded
`0.0` at both writers — so the shader's own TODO is accurate: viewport DOF is disabled. Either wire the
uniforms or delete the section.

**Evidence:** `app/src/properties_render.rs:417-432`; `app/src/viewport_render.rs:723` (RenderConfig omits
every `dof_*`); `graphics/src/post_process.wgsl:847` (`let focus = config.sel_rim_base;` + TODO — **the only
real user-visible TODO in the entire workspace**, 1 of 8 markers); `sel_rim_base = 0.0` at
`viewport_render.rs:761` and `cuda_render_dispatch.rs:103`.

### 25. "Paint Color" checkbox silently no-ops on sparse (≥256³) sculpts

**Rung 4 · Size S (disable + explain) / L (carry colour through sparse/atlas/GPU-mesher) · blocked_by: nothing**

Paint is dense-only **by construction**, and the code says so — `cpu_paint_path()` is
`!grid.density.is_sparse() && (...)`, with the comment *"Paint is dense-only, so this is always false on a
sparse (>256³) grid."* The checkbox is **not gated on resolution** (no `add_enabled`, no `is_sparse` check
anywhere near it). On a ≥256³ sculpt the user ticks Paint Color, picks a colour, strokes — the GPU brush
writes density only, nothing paints, no toast, no disabled control.

**Pairs naturally with item 6** — both are "paint colour is a second-class citizen", and they share the
FL4ORDER/sparse surface.

**Evidence:** `app/src/voxel_sculpt_types.rs:460-464`; `app/src/ui_panels.rs:623-634` (no
`is_sparse`/`add_enabled` anywhere in `:600-660`).

### 26. Inset DEPTH is unreachable from the interactive `I` drag

**Rung 4 · Size S · blocked_by: nothing**

**Milder than remembered** — depth IS user-reachable and functional, just not live. The kernel takes it
(`inset_face_depth(f, thickness, depth)`), `apply_inset` plumbs it, and the "Adjust Inset" redo panel
exposes a working Depth slider. But `PolyInsetDrag` has no depth field, the confirm path hardcodes
`depth: 0.0`, and the tool strip shows only "Amount" / "Drag = inset, Ctrl = snap". Blender binds Ctrl
during the I drag to depth — Ctrl is spent on snap here, so pick a free modifier.

**Different bug from item 5's inset entry, same file — keep them separate.**

**Evidence:** `core_math/src/half_edge.rs:1774`; `app/src/poly/inset.rs:105`;
`app/src/poly_drag_tools.rs:364` (`depth: 0.0`); `app_types/src/types_poly.rs:222-244`;
`app/src/tool_settings.rs:232-237`.

### 27. DIAGNOSTIC: PathTrace mode appears to never write the presented LDR texture

**Rung 4 · Size S · blocked_by: nothing (needs 60 seconds of owner hands-on)**

The code chain is airtight — PT writes `hdr_texture`, sets `skip_deferred_pp = true`, which skips the
tonemap; the tonemap is the **sole** hdr→ldr converter; `ldr` is the texture registered as the egui viewport
image. So a PT frame presents a stale/frozen previous frame. Pre-existing, not caused by anything merged
yesterday.

**Run as a DIAGNOSTIC, not a fix:** PT's reachability is narrow (the `unified_raster` arm outranks it) and
per `feedback_verify_app_path_not_kernel` the owner is the only oracle for what PT actually displays.

**Evidence:** `app/src/viewport_render.rs:1114`, `:1399` wrapping `:1409`, `:596-598`, `:490-491`; gate
`viewport_render_mode.rs:64` vs `:71-72`.

### 28. DECISION: should `ViewportRenderMode::FullSdf` keep existing?

**Rung 5 · Size S · blocked_by: owner decision**

A decision, not a repair — 5 minutes of owner input closes it. FullSdf gets no GPU wireframe because it has
no raster pass and therefore no depth buffer; **the gate is correct, not a bug**. But FullSdf is still
user-reachable via a viewport toggle, and under the Display Invariant (SDF renders from its baked DC mesh
via the raster pass in EVERY mode) it looks vestigial. Either delete the mode + its toggle, or accept it
loses the wireframe.

**Do NOT "fix" it by feeding the wireframe an SDF depth source — that is the forbidden raymarch/raster
fusion class.**

**Evidence:** gate `app/src/viewport_render.rs:1465`, rationale `:1456-1458`; still reachable
`app/src/viewport_overlays.rs:352-354`; other refs `viewport_header.rs:89`, `viewport_render_mode_auto.rs:35`,
`cuda_render_dispatch.rs:44`.

---

## TIER 5 — THE BIG TRACKS

### 28b. Poly viewport performance — P0 + P1a (validated plan, ready to implement)

**Rung 4/5 · Size M · blocked_by: nothing**

Plan: `docs/planning/poly/POLY_VIEWPORT_PERF_PLAN.md` — **validated 2026-07-30**, verdict APPROVE WITH
CHANGES, C1–C14 all confirmed against live code. Status: READY.

**The measured case** (`cargo test -p core_math --test circle_perf --release -- --nocapture`, already in
tree): `apply_poly_modifiers` at subdiv=3 on a 900-face body = **108.1 ms/frame**; the
`content_signature` check that avoids it = **0.028 ms**. ~3800×.

**First PR = P0 + P1a:**
- **P0** — extend the existing `circle_perf.rs` harness + add `time_block!` timers on the poly sync /
  overlay / selection / pick paths. (Half done already; §12.3 of the plan holds the baseline.)
- **P1a** — kill the **uncached per-frame** `apply_poly_modifiers` call sites. Headline offender:
  `app/src/draw_selection.rs:409` (`draw_node_selection_outlines`), an egui painter with **no
  fingerprint, no cache, no settle gate**, invoked from `viewport_overlays.rs:500`/`:1265`. Any selected
  poly body with a modifier stack pays the full stack **every frame, including pure camera orbit**.
  Fix = cache keyed exactly like `poly_display_cache`. Hours of work, no product decision needed.

**Also filed by the validation** (plan §12.2): the dual tessellation producer
(`raster_mesh_prep.rs:117` `ensure_poly_meshes` bypasses modifiers *and* the display cache — a "two code
paths for one operation" violation); pick geometry ≠ solid geometry (`poly_select_gpu.rs:176` base+fan vs
display+earclip — **correctness**, not just perf); and poly wire having no local edge cache while SDF has
one 40 lines away (`overlay_lines.rs:349` vs `:351`).

Only **P2b (instanced Array/Mirror)** is owner-blocked — 3 product questions, incl. what the poly→SDF
link (`lab_mesh_sdf.rs:390`) should receive. Everything else is unblocked.

### 29. Answer the still-open owner decisions

**Rung 4 · Size S · blocked_by: nothing**

Cheap, and several gate work below.

**INPUT_ARCHITECTURE_PLAN §10 — already answered:** D1 (rule rewrite, `e6d3f9e`), D2 (LOGICAL — **must be
written into the doc before item 7 ships**), D6 (Ctrl+Shift+S, `da2b46b`).

**Still open:**
- **D3** — accept the Soft-conflict residual + allowlist.
- **D4** — does "F in outliner" require inventing an outliner action set? (gates item 34)
- **D5** — Shift = duplicate AND rotation-snap in one gesture: split or keep. Per
  `feedback_ui_mockup_first_workflow`, prototype in the HTML Artifact before any egui work.
- **D7** — config root `~/.azra` vs `%LOCALAPPDATA%/Azra`.
- **D8** — fallback-tool concept so click-select survives an Exclusive modal (gates INPUT S9).
- **D9** — accept Mode as a derived view over 4 contradictory fields.
- **D10** — make the `Guard::Legacy` count a tracked number.

**NEW — the outliner target question (gates items 32/33, added by challenge):** does the per-mode outliner
target legacy `outliner_draw.rs` or the ui2 outliner? **A second outliner already exists** at
`feat/ui-polish:core_engine/app/src/ui2/mod.rs:414` (`fn ui2_outliner`), dispatched at `:208`/`:318`, with
its **own** flattened row abstraction at `:711` — i.e. the exact concept item 32 proposes to invent as
`OutlinerRow`/`RowProvider`. Item 32's "0 hits" evidence is true **on master only**, which is why every
sweep missed it. The merge axis is clean (`git diff --stat master feat/ui-polish -- outliner_draw.rs
viewport_header.rs` = empty; ui2 is purely additive behind `AZRA_UI=v2`, which does not exist on master at
all). So this is not a blocker — it is a **decision**, and getting it wrong wastes an L-sized slice. See
also memory `project_outliner_target_mockup`.

### 30. INPUT S1–S4 — keymap types → declare-don't-enforce → SURFACE axis → kill the second registry

**Rung 4 · Size L · blocked_by: item 7 (S0) + the B1/B2/B3 verifier defects**

The architecture, after the hotfixes have already taken the pain out. S1: types only, zero callers,
trivially green — the exhaustive `ToolId::of()` forces all 26 `ActiveTool` variants to be classified. S2:
swap `ActionDef.context` prose for guard/hint/avail, all `Guard::Legacy` — inert by construction, conflict
detection becomes sound. S3: panels publish surface rects; delete `active_input_context()`'s hardcoded
screen fractions. S4: the resolver + delete InputSystem.

**S4's "rewrite the CLAUDE.md rule" sub-task is ALREADY DONE — do not redo it.**

⚠️ **B1/B2/B3 are NOT in INPUT_ARCHITECTURE_PLAN.md.** Two independent sweeps searched and could not locate
them on disk; they live only in the out-of-band verifier reports. **Locate them before starting S1** — an S1
started without them is starting against a refuted plan.

**Evidence:** INPUT plan §8 S1–S4 (lines 1119-1153). Motivating defects verified: `resolve()` has ONE call
site app-wide; `bind_default` silently drops on `(event,mods,context)` collision without comparing
`action_id`, so 3 actions sit on X+NONE+Viewport and 2 boolean pies are permanently unreachable through it
(§2.1). **Citation drift:** §2.1 says ACTION_DEFS = 96; actual **97**. InputSystem is now **22**, not 19
(the Circle merge added the 22nd → item 13).

### 31. POLY_FOUNDATION Phase 3 — populate the CustomData sidecar + the FL4 `PolyLayerRecord`

**Rung 4 · Size L · blocked_by: item 4**

**Half-scaffolded, which is the trap.** The FL4 in-blob record type and its round-trip test suite exist, but
the field is hardcoded to `vec![]` on save and never read back, and CustomData is still an empty
`LayerStore`. Work = fill the sidecar with live typed columns (Copy/Constant interp only), wire per-corner
`uv` into `to_raster_data`'s hardcoded `[0,0]` slot and `sharp` into split-vs-averaged normals, and actually
populate/consume `poly_layers`.

**Blocked by item 4:** do not build more on top of a merge gate whose status is unknown.

**Evidence:** shipped: `core_math/src/poly_mesh.rs` (LayerStore `:203`),
`mesh_ops/{local_ops,local_rebuild_ops,subdivide,propagate}.rs`, inverse tiers `mesh_ops/mod.rs:110`/`:113`.
NOT shipped: `app/src/project.rs:48`, `:127` define `poly_layers`/`PolyLayerRecord`, `:665` writes
`poly_layers: vec![]`, no other writer/reader outside tests `:949-980`. `poly_mesh.rs:16` says verbatim
*"Phase P3 fills the CustomData SoA sidecar"*. Doc: `POLY_FOUNDATION_PLAN.md:309`.

### 32. MASTER_WORK_LIST P0.4 — per-mode outliner (RowProvider). NEVER STARTED.

**Rung 4 · Size L · blocked_by: the outliner target decision (item 29)**

**This is a P0 in the priority-source doc with literally zero code on master, and it is ranked 32nd.** That
is a judgment call about the word "fix", not a fact — the owner should overrule me if it is wrong.

The outliner is mode-blind: it always shows the SDF prim tree regardless of mode. Designed architecture:
derive `OutlinerMode` from existing `work_mode`/`modeling_context` → extract `OutlinerRow` + a generic
drawer (route the current SDF path through it, pixel-identical) → `ObjectProvider` → decouple
select-vs-enter-edit → Sculpt/Poly providers + a pinned "Scene Globals" in edit modes.

⚠️ **`blocked_by` CORRECTED (was "nothing").** A second outliner with its own row abstraction already exists
on `feat/ui-polish` — see item 29. Building a **third** row abstraction into legacy `outliner_draw.rs`
while a second sits unmerged is CLAUDE.md's "Two code paths for the same operation" violation, merely
deferred.

The work list says fix outliner audit bugs O3/O4/O6 first — **re-validate those, they are March-2026
vintage and may be stale** (item 14 covers this).

**Evidence:** `MASTER_WORK_LIST.md:77` (P0.4), `:70`. VERIFIED —
`grep 'RowProvider|OutlinerMode|ObjectProvider'` across `core_engine` = **0 hits** *on master*;
`grep 'work_mode|WorkMode'` in `app/src/outliner_draw.rs` = **0 hits** (the drawer structurally cannot see
the mode).

### 33. MASTER_WORK_LIST P0.3 — Mode breadcrumb + Layout demote. NEVER STARTED.

**Rung 4 · Size M · blocked_by: the outliner target decision (item 29)**

The second unstarted P0, same caveat as item 32. Primary navigation should be a mode breadcrumb ("Object ›
Sculpt · Sphere.001") with mode driving the default panel layout, and the workspace tabs demoted to an
optional Layout menu.

**Shares the `OutlinerMode` derivation with item 32 — sequence them together or duplicate the work.**

**Evidence:** `MASTER_WORK_LIST.md:76`; `VIEWPORT_MODEL.md` is the canonical spec. VERIFIED —
`app/src/viewport_header.rs:11-13` still draws `---- Workspace tabs ----` as the first header element;
`grep 'breadcrumb'` across `core_engine` returns only crash-diagnostic files — nothing navigational.

### 34. `F` over the outliner opens the viewport F pie

**Rung 4 · Size M · blocked_by: owner decision D4; INPUT S1 (itself blocked on B1/B2/B3)**

The owner's literal example, but **blocked on a decision, not on code.** `input_keyboard.rs:828` has no
surface check and the outliner has no F handler.

Two compounding defects: **(a)** the surface axis is checked at only 3 sites app-wide and the detector is
guesswork — `active_input_context()` derives Outliner/Properties/Library from **hardcoded screen fractions**
(`mouse.x > width*0.75 && mouse.y < height*0.5`), because `viewport_rect` is the only real panel rect stored
anywhere. **(b)** `:827` hardcodes `key_down(egui::Key::F)` for the HOLD while `:828` uses the remappable
binding for the OPEN — so **rebinding Focus to V makes the pie permanently unreachable** (`f_down` instantly
false → QuickTap every time).

**PREREQUISITE the plan must not hide:** the outliner has **zero** keyboard actions today. The axis is
worthless until actions exist on the far side of it.

**Evidence:** `app/src/input_keyboard.rs:827-828`; `app/src/input_system.rs:232-243`;
`app/src/outliner_actions.rs:30-40`. INPUT plan §2.5a/b, §4.1, §10 D4.

### 35. BEVEL §9 item 4 — rounded N-way / edge-loop k=2 (Blender M_ADJ adjacency grid)

**Rung 4 · Size L · blocked_by: nothing (independent of the GRS and input tracks)**

The **only** remaining item in BEVEL.md §9 (items 1–3 all shipped 2026-07-02). Replace the shared-corner
`n_seg→1` flat clamp (3+ beveled edges, or a degree-3 k=2 corner) with the adjacency grid: seed a seg-2 cage
(BoundVert corners + arc midpoints + one fullness center), Catmull-Clark subdivide with
`sabin_gamma(count)`, resample. M_ADJ is **only** for count≥3 — the 2-edge case is the M_NONE weld and is
already done. Keep the §8 guard as the safety net.

**CLAUDE.md mandates:** read BEVEL.md §6 first; on completion append to §6/§11 + the bug tracker + the
`project_bevel_corner_reference` memory.

**Evidence:** BEVEL.md §9 item 4. App guard warns on **THREE+** edges at one vertex, not two:
`app/src/poly_drag_tools.rs:860-863` (comment verbatim: *"What is still unimplemented is the N-WAY miter:
THREE+ selected edges meeting at one"*), `:873`.

---

## BACKLOG — owner-deferred, or rung 5–6

**CLAUDE.md rung 6 is "never, unless it's the assigned task."** Items 38–43 are listed so they are **filed**,
not so they get picked up. See the rule-conflict report in §7.

### 36. BEVEL-NOTCH — per-edge box bevel combines tri-planar bevels with a hard `max()`

**Rung 4 · Size L · owner-deferred — do not start without an explicit go.** Directly violates the CLAUDE.md
Per-Edge Bevel Corner Rule; reproduces on a single plain box. HARD — a corner-sphere-via-`max()` attempt was
already tried and reverted 2026-03-22. **Evidence:** `graphics/src/sdf_prim_eval.wgsl ~:418-422`; tracker
`:981`. ⚠️ **The tracker's port list is stale** — it names GLSL `sdf_functions.glsl`, deleted 2026-07-14.
Real port surface = `sdf_prim_eval.wgsl` + `volume_bake.wgsl` + `sdf_mesh.wgsl` + CPU
`bevel_op.rs`/`bake_utils.rs`.

### 37. SUBBODY-EVAL-ORDER — grouping a prim changes the boolean result

**Rung 4 · Size L · owner-deferred.** `all_primitives_ordered()` walks depth-first, so grouping moves a
prim's position in the boolean chain. Owner's invariant: *"drag-to-create-sub-body should NOT change the
stack order."* **Evidence:** tracker `:988`, `core_math/scene_graph.rs:897`. **Re-verify against the
now-fixed recursive CPU evaluator (`field_eval.rs:144-165`) before designing** — the interleave may be
cheaper than the tracker assumes.

### 38. Cache the edit wireframe on a mesh revision counter

**Rung 5 · Size M · do not pick up opportunistically.** CONFIRMED: no revision/version/generation counter
exists on `HalfEdgeMesh` or `PolygonalBody`, so `build_overlay_lines()` rebuilds the whole segment list every
frame and re-uploads every instance every frame. Correctness is fine; pure perf. **Evidence:**
`app/src/overlay_lines.rs:36`, called at `viewport_render.rs:256`, uploaded at `:1473`;
`grep 'revision|mesh_version|topology_gen'` = 0 hits. **Red line:** the counter belongs on the
non-serialized `PolyMesh` wrapper or the app layer — **never** a new bincode field on `HalfEdgeMesh`.

### 39. Delete the dead `bevel_edges` / `bevel_edges_ex` kernel pair

**Rung 6 · Size S · blocked_by: item 4** (don't delete the old path while the new path's inverse gate is of
unknown status). A named deliverable of POLY_FOUNDATION Phase 1 that is being declared done: the app caller
was retired (live path = `bevel_op::bevel_edges_offset`), but the wrong-algorithm kernel and its zero-caller
wrapper still stand. **Evidence:** `core_math/src/half_edge.rs:3004`, `:3050` — verified present; zero
non-test callers; live path `app/src/poly_drag_tools.rs:968-970`. **Interacts with item 21** —
Bevel-as-modifier has no live kernel to bind to.

### 40. Delete `core_math/src/sculpt_brush.rs` — 355 lines of dead code the compiler cannot flag

**Rung 6 · Size S.** `pub mod sculpt_brush;` makes it public API of a lib crate, so rustc never warns — but
zero references from `app/` or `graphics/`. The live smooth is `voxel_sculpt.rs` `apply_stamp_plan`
`Operator::Relax`. **This has already cost a session** (a dev fixing "smooth is weak" edited this file and
saw no effect). CLAUDE.md's own bar — *"Will the next dev need tribal knowledge to avoid breaking it? If
yes, enforce correctness structurally"* — makes deletion the structural fix. **Evidence:**
`core_math/src/lib.rs:22`; grep across `app/src`+`graphics/src` returns only unrelated `sculpt_brush_size_*`
action-id strings (`input_system.rs:285-290`).

### 41. Collapse `PolyEdit::MergeVertices` onto the fixed merge path (audit #8 survives there)

**Rung 6 · Size S.** Merge #8 (self-loop) and #13 (nondeterministic center) were fixed in
`merge_vertices_to` (calls `collapse_self_loops()`, repairs the survivor's disk pointer). The legacy 2-arg
`merge_vertices` never got that treatment and is still the target of `PolyEdit::MergeVertices`/
`MergeVerticesOp`. **App-unreachable today** (it only emits `MergeVerticesAt`) — so this is two paths for one
op with the bug live on one, waiting for the next caller. **Evidence:** fixed
`core_math/src/half_edge.rs:1196-1215`; unfixed `:1169-1187`; still routed `mesh_ops/mod.rs:384`,
`mesh_ops/local_rebuild_ops.rs:140`; app emits only `MergeVerticesAt` at `app/src/poly/merge.rs:30`/`:54`/
`:99`. **Pairs with item 3 — together they should let `apply_in_place` be tightened from `validate()` to
`is_manifold()`.**

### 42. Delete `debug_validate_mesh` — dead, and logs where CLAUDE.md requires an assert

**Rung 6 · Size S.** Zero callers, kept alive by `#[allow(dead_code)] // FUTURE`. Its body downgrades a
`validate()` failure to `eprintln!`, which CLAUDE.md forbids verbatim. Not a live violation only because
nothing calls it — a loaded footgun. Delete it, or make it `debug_assert!` and call it. The capability is
not missing (`validate()` gates the boolean install at `poly/boolean.rs:149-150` alongside `is_manifold()`).
**Evidence:** `app/src/poly/mesh_sync.rs:90-96` (read in full, verified).

### 43. Fold the three copies of Newell's method into one

**Rung 6 · Size S · RIDE-ALONG ONLY, never a standalone task.** Canonical:
`core_math/half_edge.rs:114 newell_normal(pts: &[Vec3])`. Copy 1: `core_math/bevel_op.rs newell_idx`
(indices + positions slice; used `:167`, `:1162`, `:1272`). Copy 2:
`app/src/poly/fill.rs:89 best_fit_plane_normal` (points + a collinear-triple fallback; used `:36`, `:61`).
Signatures genuinely differ — not a mechanical swap. Already marked `TODO(dedup)` in-code
(`half_edge.rs:110`); filed at tracker `:68-71`. **Do it only inside a task that already touches
`bevel_op.rs` or `poly/fill.rs`** — and note `bevel_op.rs` carries a mandatory BEVEL.md read-gate plus a
mandatory KB-append on completion, so it is cheap-looking, not cheap.

---

## 4. Dependency graph

### HARD dependencies (verified, not assumed)

**1. Record D2 = LOGICAL in INPUT plan §10 → item 7 (key_id)**
D2's answer freezes into the v3 on-disk format item 7 ships. The owner has already chosen; the doc still
lists D2 as open. Cost: a 2-minute doc edit. A checklist item, not a real blocker.

**2. Item 7 (key_id / INPUT S0) → item 30 (INPUT S1–S4)**
Every later slice serializes chords through `key_id`. The plan states S0 has zero dependency on which
architecture wins, specifically so it can ship first and alone. **This — plus size S — is the honest
justification for item 7's early slot. Not severity.**

**3. B1/B2/B3 verifier defects → item 30 (INPUT S1)**
All 3 verifiers REFUTED the plan on these, and they were ruled fixable-before-S1. ⚠️ Not on disk — locate
the out-of-band verifier reports before starting S1.

**4. Item 15 (S0) → 16 (S1) → 17 (S2) → 18 (S3) → 19 (S4) — the GRS chain**

*Attacked directly during this pass and CONFIRMED — the challenger tried to break it and could not.*

**Why S0 before S3 is hard (the load-bearing one):** unify-first ships a commit where **poly is strictly
worse** (CLAUDE.md: "never leave a functionality gap for even one commit") AND **destroys poly's view-aware
rotate — the only reference the owner can A/B the new sign against.** Once poly is migrated that reference
is gone forever, and per `feedback_verify_app_path_not_kernel` the owner is the ONLY oracle for rotate feel.
Losing it is unrecoverable; deferring S3 by one small slice costs nothing.

**Honest characterization (adopted from the challenge):** this dependency is **process-natured, not
compile-order.** S3-before-S0 would compile, and would merely make poly inherit SDF's wrong sign, fixed
later by S0 with identical total work. Its real force is the A/B-reference argument. **But since S0 is size
S and provably neutral for poly (verified: one non-gizmo caller, passing `Grab`), reordering has zero upside
and a real downside.** The order stands.

**Why S1 before S3:** S3 moves poly onto `compute_transform` via the identity-originals trick, which assumes
the math already runs in world. Migrating a local-space bug onto a world-space evaluator produces a
*different* wrong answer with no owner-legible signal.

S2 and S4's positions are the doc's fixed order; the fixes themselves are independent of their neighbours.

**5. INTRA-SLICE (not separable — same commit or you ship a new bug)**
- **S0 carries BOTH `AxisConstraint::None` sub-branches** (`:163-166` free_mode AND `:168-172`
  perpendicular_axis). Miss the first and S4 inherits the bug four slices downstream. *(New — from
  challenge.)*
- **S1 carries the mirror co-fix.** The mirror plane is WORLD while the verts are LOCAL. Fix D6 alone and
  symmetry silently skews on every rotated body. A correctness constraint, not a sequencing choice.
- **S3 carries the cancel vert-restore.** It **must NOT be fixed before S3** — poly is never
  `ActiveTool::Gizmo` today, so a poly branch added early is unreachable by construction and becomes the
  exact fossil the migration exists to delete.
- **S3 carries the `ActiveTool::PolyTransform` deletion.** The `_ => None` accessor is what fossilized the
  last run with a fully green build. A red build here is a SAFE stall.
- **S3 carries the poly HUD re-key.**
- **S4 carries the `!in_sub_edit` lift AND aligning both free-move entries on `free_mode = true`.**

**6. Item 4 (bevel-inverse seed status) → item 31 (Phase 3) and item 39 (delete `bevel_edges_ex`)**
POLY_FOUNDATION Phase 1 made the proptest a **HARD MERGE GATE**, and phases 2 and 3 have already merged past
it while it was defanged. Building Phase 3 on top, or deleting the old bevel path, while the new path's
inverse status is UNKNOWN, compounds an unmeasured risk. Item 4 costs one line and a test run.

**7. Owner decision D4 → item 34 (F over the outliner)**
The outliner has **zero** keyboard actions today. The axis is worthless until actions exist on the far side
of it — you would ship dispatch machinery guarding an empty set. D4 is a product decision.

**8. The outliner target decision (item 29) → items 32/33** *(New — from challenge.)*
A second outliner already exists on `feat/ui-polish`. Not a merge blocker (the axis is clean), but building
a third row abstraction while a second sits unmerged wastes an L slice and defers a two-paths violation.

**9. Item 3 (dissolve_edge slit) + item 41 (MergeVertices collapse) → tightening `apply_in_place` from
`validate()` to `is_manifold()`**
POLY_POLISH's own 2b note says `apply_in_place` is *deliberately* still gated on the weak `validate()`
**because** #11 and #8 are buggy. These two bugs hold the strong install gate open — and the gate is what
stops silent corruption from installing as a class.

### SOFT dependencies (sequence to save work — not correctness)

- **Item 18 (S3) → item 20 (SDF pivot).** Item 20 edits `gizmo.rs:1475`/`:1498-1505` — exactly the region
  S3 rewrites. Sequence after, or accept a merge conflict. Do NOT fold it into S0–S4.
- **Item 22a ("Shift: Fine") BEFORE item 18 (S3).** Same hint rows S3 must re-key. First is safe and makes
  S3 cheaper; **same task is not** (refactor budget).
- **Item 8 (poly slider undo) BEFORE item 16 (S1).** S1's falsifiable gate requires those exact sliders.
  Make the measuring instrument trustworthy first.
- **Item 32 + item 33 share the `OutlinerMode` derivation.** Sequence together.
- **Item 14 (triage) BEFORE items 32/33.** The work list says fix O3/O4/O6 first; those are March-2026
  vintage and may be stale.
- **Item 6 + item 25** are the same theme and share the FL4ORDER/sparse surface. Natural pair.
- **Items 21–26** are one class and could be one batched "dead UI audit" pass.
- **Item 43 (Newell)** rides inside any task already touching `bevel_op.rs` or `poly/fill.rs`.
- **Item 39** interacts with **item 21** — Bevel-as-modifier has no live kernel to bind to.

### NOT dependencies (traps that look like ordering constraints)

- **The GRS cancel gap is not a standalone item and must not be pulled forward.** It is filed inside item 18
  as an acceptance criterion, nowhere else — filing it separately invites someone to "fix" it into a fossil.
- **PathTrace-gets-no-wireframe does not block item 27.** The gate is deliberate and correct
  (`viewport_render.rs:1459-1463`). Do NOT "restore" it.
- **Items 9, 10, 11, 12, 13 are independent of B1/B2/B3 and S1.** Copy-paste/dispatch defects, not
  keymap-architecture questions. Item 10's root cause is a mode-field desync no keymap slice touches; item
  22's is a false string. Precedent: `da2b46b`. **Do not let them wait.**
- **Item 20 and items 9/10 are fully independent of the GRS chain** — interleave freely.
- **The other worktrees are not rotting.** Verified `git rev-list --count master..<branch>`:
  `feat/sparse-3a` = **0**, `feat/poly-foundation` = **0** (both fully merged), `gpu-resample-wip` = **1**
  (intentionally parked). Only `feat/ui-polish` = **29** is genuinely ahead, and it is owner-gated.

---

## 5. Memory corrections

- **`project_sdf_tools_worktree` is STALE — drop or rewrite it.** It claims UNCOMMITTED work in a worktree
  that **no longer exists**. Verified: `git branch -a --list '*sdf*'` = empty; `git reflog --all` shows
  `6375b9e … merge(sdf): intersect-cull fix + Slice/HalfSpace + Emboss/Deboss (feat/sdf-tools)`. **The work
  LANDED** — HalfSpace/Emboss/Deboss appear across 24 files on master including `sdf_prim_eval.wgsl`,
  `volume_bake.wgsl`, `core_math/src/sdf_shapes.rs`, `core_math/src/modifiers.rs`. As written it would send
  a dev hunting for work already in the product. Its "known graphics test fail = FLOATS_PER_PRIM 288vs292"
  line is also stale (§6.C).
- **`project_bevel_corner_reference` is STALE on the multi-edge miter** — see §6.C.

---

## 6. Dropped — do not re-derive these

*Silent removal is how a stale tracker happens. Everything below was proposed by at least one sweep and cut,
with proof.*

### A. VERIFIED FIXED — marked open in COMPLETE_BUG_TRACKER.md, fixed in code

- **CR-01** (no primary GPU device-loss handler) → FIXED: `app/src/app_update.rs:456` `on_uncaptured_error`
  + `:462` `set_device_lost_callback`.
- **CR-04** (compositor undo illusory, "all 76 controls call `save_undo()`") → STALE BY CONSTRUCTION:
  `save_undo()` was DELETED per CLAUDE.md; the only surviving hit is a comment at `undo.rs:231`.
- **CR-06** (unchecked `self.primitives[idx]`) → FIXED: `draw_gizmo.rs:61`, `:166`, `:486-487`,
  `camera.rs:15` all bounds-check.
- **CR-07** (prim name field: no undo, no pctx, never mirrored) → FIXED:
  `properties_sdf_shape.rs:34-36`.
- **CR-08** (sRGB/linear broken for all material pickers) → FIXED: `properties_sdf_material.rs:14-36`.
- **CR-09** (42 click controls have no undo) → FIXED: `properties.rs:88` has the `!pctx.drag_started` →
  `transaction_snapshot_only()` branch.
- **CR-21** (3 of 5 `map_async` callbacks silently hang) → FIXED: `gpu_ctx.rs:13-32` tri-state `MapResult`
  + `app_poll_bake_poll.rs:76-84`.
- **CR-27** (toolbar `handle_tool_button` bypasses `switch_work_mode`) → FIXED: `workspace.rs:261-262`.
- **CR-30** (3 `pub(crate) fn` in main.rs) → mostly fixed; 1 remains. Not worth an item.
- **H5–H10 "6 flat-cache mutation violations"** (tracker `:2478`, filed under *"STILL OPEN — HIGH
  PRIORITY"*) → **STALE. The most important drop on this list**, because rung 2 would rocket it to the top
  of any ladder-sorted list. Every mutable write in all three named files was grepped:
  `input_viewport.rs:861` writes `self.scene.primitives[prim_idx].operation.set_blend_k(new_k);` then
  **immediately** `self.mirror_prim_to_graph(prim_idx);` — same shape at `:915`, `:955`, `:1041`;
  `menus.rs:1080-1081` does `begin_episode` + `mirror_prim_to_graph`; `draw_viewport_overlays.rs` has
  **zero** mutable writes. **There is no live source-of-truth violation at these sites.** Mark FIXED.
- **Nested sub-body boolean tree** (tracker `:308`, a stale 📋 sitting *inside* a section whose own header
  reads "✅ FIXED — 2026-07-09") → FIXED. `field_eval.rs:144-165` has the recursive `FieldChild` nesting and
  cites this exact tracker entry. **Not a work item — an OWNER VERIFY request:** enter the sub-body scope,
  flip a cube to Subtract, confirm the viewport volume updates live.

### B. VERIFIED SHIPPED — plan docs still list these as REMAINING/unchecked

- POLY_POLISH §4 **Phase 2a "REMAINING: extrude_region(faces)"** → shipped 2026-07-07
  (`half_edge.rs:921`, `extrude_region_volume` `:1065`).
- POLY_POLISH §4 **Phase 2e "REMAINING: inset_region(faces) + an Individual toggle"** → shipped 2026-07-15
  (`half_edge.rs:1500`; `app/src/poly/inset.rs:27`; Individual toggle = `individual: bool` at `inset.rs:59`).
  ⚠️ **The tracker entry (`:95`, dated 2026-07-15 — filed the same day it was already done) went stale within
  ONE DAY. Treat even yesterday's tracker entries as claims, not facts.**
- POLY_POLISH §4 **Phase 2b "REMAINING: DissolveVertex"** → shipped (`half_edge.rs:2221`). Only the #11 slit
  half is open → item 3.
- POLY_POLISH §5 **"[ ] Symmetrize — kernel exists, just unwired (free win)"** → SHIPPED and wired
  (`symmetry.rs:94`, dispatched `input_keyboard.rs:720`). **The Ctrl+Shift+S hotfix at `da2b46b` was
  literally about this key firing.** Actively misleading — tick it.
- POLY_POLISH audit **#15** ("the app insets each selected face INDIVIDUALLY, but Blender's I default is
  REGION") → shipped as `3dfbc0e`.
- **"drag→region wiring"** → SHIPPED: `poly_drag_tools.rs:41` calls `extrude_region`, `:238` calls
  `inset_region`.
- INPUT §10 **D1** → done at `e6d3f9e`. **D6** → done at `da2b46b`. Therefore INPUT §8 **S4's "rewrite the
  CLAUDE.md rule" sub-task is ALREADY DONE.**

### C. FIXED — contradicts a memory or a plan doc that is now wrong

- **"boolean coplanar #2"** → FIXED, do not re-list. Boolean XL landed 2026-07-08: a robust winding-number
  classifier replaced the csg.js BSP as mainline (`mesh_boolean.rs:1154-1160`), and `polygons_to_mesh` now
  guards on `is_manifold` and returns `None` rather than emitting unvalidated geometry (`:935`).
  ⚠️ **POLY_POLISH §4 Phase 2c is self-contradictory** — it says *"#2 outcome: does NOT reproduce … the
  concrete fix landed"*, then lists *"REMAINING: (b) … stop returning `build(&faces)` unvalidated (:733)"*.
  That REMAINING text is a leftover of the "Original text:" block preserved directly below it, **not live
  work. Delete that bullet** (item 14).
- **"merge self-loop #8"** → FIXED on every path the app can reach: `merge_vertices_to` calls
  `collapse_self_loops()` (`half_edge.rs:1209`) and the app emits only `MergeVerticesAt`. Merge-by-Distance
  also shipped (`poly/merge.rs:65`). #8 survives **only** on the app-unreachable `PolyEdit::MergeVertices`
  path → item 41 at rung 6, NOT a live bug.
- **"multi-edge miter (2 edges at a degree>3 vertex; STEP3 uses one rail)"** → **STALE, and the
  `project_bevel_corner_reference` memory is stale the same way — correct it.** BEVEL.md §9 items 1–3 all
  shipped 2026-07-02: flat N-way, k=1 partial corner at degree>3, AND the rounded 2-edge M_NONE weld miter.
  Code agrees: the app guard warns on **THREE+** edges at one vertex, not two
  (`poly_drag_tools.rs:860-863`, guard `:873`). Only §9 item 4 remains → item 35.
- **graphics `FLOATS_PER_PRIM` 288 vs 292 test failure** → CONFIRMED STALE / WORKTREE-ONLY.
  `graphics/src/glsl/` **does not exist** (deleted 2026-07-14). No such test on master;
  `cargo test -p graphics` green (41 passed) is correct. Residual doc drift, one-line fix if anyone is in
  the file: `FLOATS_PER_PRIM = 292` (`core_math/src/lib.rs:31`) but `graphics/src/serialize.rs:958` still
  says *"The monolithic 288-float prim buffer"*.

### D. DEAD TRACKER ENTRIES — the subsystem no longer exists. Delete them (item 14)

Verified deleted at master: `graphics/src/glsl/`, `raymarch_entry.wgsl`, `compute.wgsl`,
`app/src/brick_pool.rs`.

- **CR-25, P11** (raymarch_bvh_full.comp) — GLSL BVH path, DELETED 2026-07-14.
- **S16** (raymarch_entry.wgsl) — DELETED.
- **S25–S29, G4, G12** (compute.wgsl) — DELETED, and it was DEAD code per CLAUDE.md, never compiled into any
  pipeline.
- **BUG-05…BUG-23, RR-01…RR-03** (brick pool) — PARKED per CLAUDE.md; file gone.
- ⚠️ **"⏸️ DEFERRED (shaders frozen)"** (G4, G11, G12, K65–K67) — the *rationale* is stale: shaders are NOT
  frozen; only `shader_nodes.rs` and `compute.wgsl` are. They are rung 6 regardless, so it changes nothing
  operationally — but **do not cite "frozen" as a reason to skip them.**

### E. NOT A DEFECT — the premise is false, or the behavior is deliberate

- **"PathTrace gets no GPU wireframe" as a regression** → the gate is CORRECT and deliberate; the reasoning
  at `viewport_render.rs:1459-1463` is sound (without a fresh tonemap underneath, alpha-blended lines
  composite onto their own previous output and accumulate to full opacity). **Do NOT "restore" it.** The real
  finding is the pre-existing defect *underneath* → item 27.
- **"Shift+X plane-exclude is broken in poly rotate/scale"** → REJECTED, premise false.
  `compute_transform` **aliases** Plane\* → axis for both Rotate (`transform.rs:175-177`) and Scale
  (`:191-193`); only Grab implements real plane math. SDF has no plane-exclude for R/S either, so **no
  migration can "fix" what does not exist in either pillar.** Tell the owner: "Shift+X in R/S = axis, not
  plane, in BOTH pillars" — file as new UI-track work if he wants the feature.
- **"i can see through the mesh"** → CLOSED (`1b63894`/`1a912f5`), structurally: a real GPU depth-tested line
  pass (`render/src/line_overlay.{rs,wgsl}`, LessEqual + depth_write=false + slope-scaled bias) against the
  SAME depth buffer the raster pass wrote, with **both** CPU heuristics DELETED. Headless GPU tests on a real
  device assert occlusion. ⚠️ **NUANCE the owner may re-report:** the commit deliberately leaves
  selection/hover/vertex-debug overlays as flat always-on-top egui, **matching Blender**. If "see through the
  mesh" comes back, it will be those overlays and it is **intended** — confirm before treating it as a
  regression.
- **"inset rotate face it not working properly"** → CLOSED (`3ada9ec`). Root cause: `face_vertices()`
  returns the ring rotated by one (it pushes `he.vertex` = the DESTINATION), so the drag zipped
  `inset_offset_dirs()` in source order against it and every vertex got its **neighbour's** displacement.
  Fixed by pairing on vertex IDENTITY. ⚠️ **RESIDUAL RISK:** this was interpreted as the inset TWIST. If the
  owner meant "rotating a face doesn't work," that symptom is **item 16** (S1/D6 REPRO B). Treat a re-report
  of face rotation as D6, not as a failed inset fix.
- **The 9 `#[ignore]`d tests** → the premise "an ignored test is a bug with a lid on it" does NOT hold. All 9
  are benign harnesses: core_math (6) `bevel_op.rs:3473` reference-dump (BEVEL_DUMP env),
  `chunked_dc.rs:2810`/`:2839`/`:3165`/`:3669` perf timing, `voxel_sculpt.rs:3483` perf probe; app (3)
  `project.rs:684`/`:737` env-driven diagnostics, `sdf_gpu_mesh/tests_bake.rs:713` needs a real GPU. **The
  only one worth a second look is `tests_bake.rs:713`** — a CPU/GPU bake PARITY test that never runs in CI,
  so a real bake divergence could hide behind it. Not filed (test-infra question) — but flag it if bake
  correctness is ever suspect. Separate doc drift: `mesh_ops/test_spine.rs:8` describes *"`#[ignore]` where
  the live PolyLayerRecord field does not exist yet"*, but that file contains **zero** `#[ignore]`.
- **The unwrap/expect sweep** → NO FINDING, reported honestly rather than padded. Raw counts are high (app
  404, core_math 176, graphics 26, render 2) but dominated by tests. Targeted greps on the dangerous idioms
  (`parse().unwrap`, `last()`/`first()`/`max_by().unwrap`, `lock().unwrap` — only 2) plus a hand-check of
  every plausible user-reachable candidate found **all guarded or unreachable**: `clip_curve_tool.rs:277`
  inside `!is_empty()`; `knife.rs:141` behind `len() < 2 { return }`; `bevel_op.rs:2102-2114` behind
  `is_empty()` early returns; `gpu_init.rs:624` takes `port: u16` so the parse cannot fail;
  `outliner_drop_slots.rs:438` is inside `#[test]`. No reachable panic constructed.
- **The assert audit** → CLEAN. Zero commented-out asserts, zero asserts downgraded to log/eprintln in live
  code. The 5 `// assert…` grep hits are all prose explaining why an assert IS or ISN'T there
  (`bake_dispatch.rs:2384`, `resident.rs:550`, `propagate.rs:145`) — exactly the "delete the assert with a
  comment" outcome CLAUDE.md permits. The single latent case is `debug_validate_mesh` → item 42.
- **`sdf_prim_eval.wgsl:153` BoolBlend unwired seam** → correctly inert (BoolBlend was only ever evaluated by
  the DELETED GLSL BVH path). **Do not "fix" it.**

### F. RE-FRAMED, not dropped — the old framing would have mis-aimed the fix

- **"properties_poly.rs writes a transform with NO transaction()"** (claimed by 3 sweeps AND by GRS §11.5)
  → **FALSE AS WRITTEN.** → item 8 with the real, much narrower gap. **GRS §11.5 should be corrected.**
- **"mesh_sync.rs:82 drops 5 of 8 modifiers because the dispatch discards working kernels"** → the DROP is
  confirmed; the **MECHANISM is misremembered in both directions.** → item 21. **The honest fix is "stop
  offering them", not "restore the dispatch".**
- **"The GRS cancel gap is a shipped rung-1 corruption bug"** → OVER-CLAIMED. It is **migration-CREATED and
  unreachable today** (poly runs as `ActiveTool::PolyTransform`, whose own cancel DOES restore verts at
  `poly_drag_tools.rs:742-756`). **Not filed standalone** — folded into item 18 as an acceptance criterion.
- **"The dissolve_edge slit (#11) is rung 4"** (POLY_POLISH's own ranking) → UNDER-claimed. **Promoted to
  rung 1** → item 3.
- **"`bevel_apply_inverse_apply_is_identity` is RED"** → REFUTED, and the real finding is worse. It
  **passes**, and it is neither `#[ignore]`d nor cfg'd out — but only because the 4 failing seeds live
  outside proptest's auto-load directory. **Honest status: UNKNOWN, gate defanged.** → item 4.
- **"The inset no-op undo is a bug in inset.rs"** → UNDER-claimed as an instance. It is a **class**, and its
  real harm is redo-stack annihilation. **3 is a floor, not a ceiling.** → item 5.
- **"Item 5 eats the user's PREVIOUS action"** *(new, from challenge)* → **FALSE.** `undo()` pops exactly one
  entry. The real harm is `push()` clearing redo unconditionally. → item 5, mechanism corrected.
- **"Item 7 is rung 1 data loss from the shipped Plus/Minus defaults"** *(new, from challenge)* → **FALSE.**
  `save()` only serializes non-default binds, so those defaults never reach `to_json`. → item 7, relabeled
  rung 3.

### G. CITATION DRIFT — so nobody chases ghosts

- INPUT §6.3 / §2.1 cite `keybindings.rs` as if under `app/src/types/` — real path is
  **`core_engine/app_types/src/keybindings.rs`** (cross-crate; matters for item 7's blast radius).
- INPUT §6.3 cites the Plus/Minus defaults at `:429-430`; actual **`:436-437`**. §2.1 says ACTION_DEFS = 96;
  actual **97**. InputSystem is now **22**, not 19.
- GRS §12 claims *"`node.pivot`/`.pivot.mode`: 0 times in gizmo.rs"* — actual **1 hit**, but it is a
  **comment** at `gizmo.rs:1474`, so the substance holds. `pivot` really is 0 in `transform.rs` and
  `poly_drag_tools.rs`.
- GRS §8 says `docs/planning/input/` *"does not exist yet (verified)"* — **it exists now** (committed at
  `282e349`).
- A gap report aimed the pie-guard finding at `input_keyboard.rs:1226` claiming it was missing a pie —
  **BACKWARDS**: `:1222-1227` is the only COMPLETE guard in the file.
- A gap report cited `draw_poly.rs:582` as a hint site — **it does not exist**; the real second source is
  `:478` (Knife).
- `viewport_hints.rs:127` is **already fixed.** The bug is the two rows ABOVE at `:51`/`:55`.
- **⚠️ Do not trust ANY `CR-*` line number.** `main.rs` is **562 lines**; the CR block cites `:570-794`,
  `:1137`, `:1237` — impossible. The de-god refactor moved everything. **Re-grep by symbol, never by line.**

### H. NOT LISTED — unresolved; worth 10 minutes of owner hands-on, not more grepping

- **MASTER_WORK_LIST P0.6** (materials on the voxel raymarch) — could not resolve.
- **MASTER_WORK_LIST P2.1** ("biggest quick win — you can't pick a brush") — `brush_type` appears in
  `workspace.rs`, suggesting a picker exists and P2.1 is at least partly stale.
- **`app/src/main.rs:313`'s blanket `#[allow(dead_code)]` on the types module** — the one dead-code allow
  that could be masking real rot at scale. Not filed (unverified, rung 6), but it is where to look first if
  dead-code cleanup is ever assigned. Context: 76 dead-code allows total (app 67, app_gpu 5, core_math 2,
  app_types 1, graphics 1), nearly all with an honest reason and genuine in-flight scaffolding. Only items 40
  and 42 are dead with no live plan behind them.

---

## 7. Rule-conflict report

CLAUDE.md requires reporting conflicts rather than silently resolving them.

- **Items 38–43 are rung 5–6**, which CLAUDE.md says is *"only if explicitly asked"* / *"never, unless it's
  the assigned task."* They are listed as **filed reminders, not work items.** Item 43 in particular is
  already `TODO(dedup)` in-code and filed at tracker `:68-71` — it rides along, never standalone.
- **Item 1 flags CLAUDE.md contradicting itself against the code it governs** (`:315` vs
  `scene_graph.rs:426`; `:66` vs `project.rs:630`). Per the Rule Conflict Check, reporting rather than
  silently fixing: **the rule is wrong, not the code.** The code is correct and documents itself correctly at
  `scene_graph.rs:400-404`/`:425`.

No other item conflicts with a CLAUDE.md rule.
