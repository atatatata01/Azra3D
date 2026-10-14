# PHASE 0 — Poly Honesty / Integrity (verified plan)

**Branch:** `feat/poly-phase0`  
**Worktree:** `.claude/worktrees/poly-phase0`  
**Base:** `master` @ `7563c27` (clean; no dirty work from main tree)  
**Date:** 2026-07-19  
**Scope:** trust / undo / keybind correctness only — **no new tools, no GRS migration, no bevel M_ADJ**

---

## 0. Code audit vs the original Phase 0 sketch

The review's Phase 0 was re-checked against **live master**. Several WORK_ORDER claims are **stale**. Do not implement stale items.

| Original item | Live status (master @ 7563c27) | Action |
|---|---|---|
| 0.1 Bevel inverse proptest / parked seeds | Test exists: `mesh_ops/test_spine.rs::bevel_apply_inverse_apply_is_identity` (96 cases). Seeds parked in `docs/planning/poly/KNOWN_bevel_inverse_regression_seeds.txt`. BEVEL.md notes test GREEN on later base — **must re-run**. | **DIAGNOSTIC first**, then restore seeds only if RED |
| 0.2 Gate modifier dropdown to live types | **STALE as written.** LIVE: Mirror, Subdivide, Solidify, **Array, Triangulate, Smooth**. DEAD: **Decimate, Bevel** only. Cards already show "⚠ Not yet" for dead types, but **Add still offers them**. | Remove Decimate/Bevel from **Add** only; keep existing stack cards with warning |
| 0.3 Kill dead "Shift: Fine" hints | **STILL LIVE** on Transform + Gizmo rows (`viewport_hints.rs:51,55`). Poly-transform row already fixed (`:127-129`). No `fine_mode` in `transform.rs` / `gizmo.rs`. | Drop "Fine" from those two rows; keep Ctrl=Snap if real |
| 0.3 Dead X-Ray button | **STILL LIVE.** `viewport_overlays.rs:354-356` writes `viewport.xray_mode`. **Zero readers.** Live concept: `sel.visible_only` (header + render panel). Dead fields: `xray_mode`, `poly_wire_xray`. | Re-key overlay button onto `!sel.visible_only` |
| 0.4 F double-fires (Fill + Focus) | **STILL LIVE.** `ACT_FOCUS` + `ACT_POLY_FILL` both default `F`. F-pie opens on `f` with **no poly Vertex guard** (`input_keyboard.rs:915`). Fill runs in Vertex via `key_dispatch.rs:96-98`. | Gate F-pie when poly Vertex fill can fire; fill wins |
| 0.4 Ctrl+I phantom instance | **LIKELY FIXED.** Instance path uses `!poly_sub_mode` (`input_keyboard.rs:1539`), where `poly_sub_mode = Polygonal && poly_work_mode != Object`. Invert uses `ACT_POLY_INVERT_SEL`. | **VERIFY only**; fix only if still broken |
| 0.5 Inset no-op undo | **STILL LIVE.** `poly_apply_inset_settled` always `transaction()` then returns `!inner.is_empty()` (`inset.rs:63-78`). Circle already dry-runs first (`circle.rs`). | Match circle: dry-run → transaction only on success |
| GRS S1 / paint select / bevel M_ADJ | Out of Phase 0 | Phase 1+ |

---

## 1. Goals & non-goals

### Goals
1. UI never **offers** a capability that cannot fire (modifiers, hints, X-Ray).
2. Daily keys in poly edit do not double-fire conflicting actions.
3. Refused inset does not push a no-op undo entry.
4. Bevel inverse gate status is **known** (green with restored seeds, or documented RED with root cause).

### Non-goals (explicit)
- Do **not** implement Decimate/Bevel modifiers.
- Do **not** implement fine/precision mode.
- Do **not** start GRS S0–S4, extrude-region, inset-region, bevel M_ADJ.
- Do **not** edit `HalfEdgeMesh` serialized layout.
- Do **not** batch unrelated refactors.

---

## 2. Task slices (one commit class each; build after each)

### P0.1 — Bevel inverse proptest re-verify (diagnostic)

**Files:** `core_math/src/mesh_ops/test_spine.rs`, `docs/planning/poly/KNOWN_bevel_inverse_regression_seeds.txt`

**Steps:**
1. Run: `cargo test -p core_math bevel_apply_inverse_apply_is_identity -- --nocapture`
2. If GREEN on random 96: try loading parked seeds into proptest corpus (or a focused unit test that replays the 4 `cc` seeds).
3. Decision tree:
   - GREEN with seeds restored → delete/archive parked file note; mark gate closed.
   - RED → document failing seed + do **not** expand Phase 0 into a bevel rewrite. File as Phase 3 prerequisite.
4. No production code change unless a one-line inverse bug is obvious and gated.

**Done-when:** status written in this file §6 + COMPLETE_BUG_TRACKER one-liner.

---

### P0.2 — Stop offering dead modifiers in Add

**Files:** `app/src/properties_poly.rs` (Add combo only)

**Live map (`mesh_sync.rs::apply_poly_modifiers`):**

| Type | Kernel/app | Add menu |
|------|------------|----------|
| Subdivide | live | keep |
| Mirror | live | keep |
| Solidify | live | keep |
| Smooth | live | keep |
| Triangulate | live | keep |
| Array | live | keep |
| Decimate | no-op arm | **remove from Add** |
| Bevel | no-op arm | **remove from Add** |

**Steps:**
1. Restrict `types` array in Add combo to the six live types.
2. Keep Decimate/Bevel **card** arms with disabled sliders + "Not yet" for loads that already have them (save-compat; honest about dead stack entries).
3. Optional micro-copy under Add: "Decimate / Bevel modifiers not available yet".
4. Exhaustive match already present in apply path — do not reintroduce `_ => {}`.

**Done-when:** cannot add Decimate/Bevel from UI; existing .fl4 with those types still load and show warning.

---

### P0.3a — Dead "Shift: Fine" hints

**Files:** `app/src/viewport_hints.rs`

**Steps:**
1. Transform row (`is_transform`): remove `("Shift", "Fine")`. Prefer documenting real Shift behavior if any (plane on poly is separate row). If Shift does nothing on SDF transform, omit.
2. Gizmo row (`is_gizmo`): remove `("Shift", "Fine")`. Keep Ctrl=Snap only if `gizmo.rs` still reads Ctrl for snap (verify once).
3. Comment that Fine is not implemented — do not re-add until a real `fine_factor` exists.

**Done-when:** grepping `viewport_hints` for `"Fine"` returns 0.

---

### P0.3b — X-Ray button honesty

**Files:** `app/src/viewport_overlays.rs`  
**Optional cleanup (same task if tiny):** note on dead fields; do **not** remove serialized fields mid-flight without save audit.

**Steps:**
1. Re-key overlay "X-Ray" toggle to `!self.sel.visible_only` (same as header `viewport_header.rs:315-320` and `properties_render.rs:265-269`).
2. Hover text: match render panel wording (show all edges / select through).
3. Confirm `overlay_lines::poly_edit_wire_xray` keeps reading `!sel.visible_only`.
4. Leave `viewport.xray_mode` / `poly_wire_xray` unused for now (or mark with comment); deleting fields is save-format-adjacent — **out of scope** unless zero serialization risk proven.

**Done-when:** overlay X-Ray toggles the same flag as header X-Ray; both affect poly wire visibility.

---

### P0.4a — F key: Fill vs Focus pie

**Files:** `app/src/input_keyboard.rs` (F-pie open site ~915), possibly `key_dispatch.rs`

**Desired behavior (Blender-like, poly Vertex):**
- In poly **Vertex** edit: bare **F** = Fill Face only (no F-pie / no frame).
- In poly Edge/Face/Object, and non-poly: F pie / quick-tap focus as today.

**Steps:**
1. When opening F-pie (`if f && !self.ui.f_pie.open ...`), also require **not** (`modeling_context == Polygonal && poly_work_mode == Vertex`).
2. Confirm `key_dispatch` still runs fill only in Vertex.
3. Edge case: empty vertex selection → fill no-ops; still do **not** open pie (user can use menu Focus). Alternate: empty sel opens pie — prefer **no pie** for consistency with "F is Fill in Vertex".
4. Do **not** rebind defaults in this slice (rebind UI can still change keys later).

**Done-when:** Vertex mode F never opens F-pie; Edge mode F still opens pie; Fill still works with selection.

---

### P0.4b — Ctrl+I verify (no code if green)

**Files:** read-only unless broken: `input_keyboard.rs:278-279,1539`, `keybindings.rs` ACT_INSTANCE / ACT_POLY_INVERT_SEL

**Verify:**
1. Poly Vertex/Edge/Face: Ctrl+I → invert selection only; no `create_instance`.
2. Poly Object (or non-poly): Ctrl+I → instance (if selection valid).
3. If broken: root cause is likely `poly_sub_mode` false while user thinks they are in edit — check Tab entry path sets `poly_work_mode` correctly. Fix the gate, not a third keybind path.

**Done-when:** verify checklist checked off in §6.

---

### P0.5 — Inset refuse-before-transaction

**Files:** `app/src/poly/inset.rs`  
**Pattern:** `circle.rs` dry-run (already correct)

**Steps:**
1. Extract / reuse `apply_inset` on a scratch clone (or pure function return) before `transaction`.
2. If empty / refuse: toast if useful, return false, **no** `history.push`.
3. On success: `transaction` then apply + selection + sync (current body).
4. Confirm Adjust Inset redo panel path still one undo entry (settled path).
5. Unit test if cheap: refuse path does not increase undo stack length (app test or document manual).

**Done-when:** failed inset does not eat Ctrl+Z; success still one undo.

---

## 3. Order of execution

```
P0.1 diagnostic (no/low code)
  → P0.2 modifiers Add gate
  → P0.3a Fine hints
  → P0.3b X-Ray re-key
  → P0.4a F-pie gate
  → P0.4b Ctrl+I verify
  → P0.5 inset undo
  → full gates
```

Rationale: pure UI honesty first (fast, low risk), then keybind, then undo (touches mutation path).

---

## 4. Gates (Definition of Done)

After all slices:

```text
cd core_engine
cargo build -p app
cargo test -p core_math
cargo test -p app --bins   # or at least poly/inset-related if suite is huge
```

Manual smoke (owner or agent checklist):
1. Poly body → Add Modifier: no Decimate/Bevel.
2. G/R/S or gizmo drag: hints show no "Fine".
3. Overlay X-Ray toggles same as header; poly wires respond.
4. Vertex mode: F fills, does not open Focus pie; Edge mode: F opens pie.
5. Vertex mode: Ctrl+I inverts only.
6. Inset with empty/invalid selection: no undo entry; valid inset: one undo.

Update: `COMPLETE_BUG_TRACKER.md` (Phase 0 items), this file §6 session log.

---

## 5. Risks

| Risk | Mitigation |
|------|------------|
| F-pie gate surprises users who used F=focus in Vertex | Document; Focus still in pie from Edge/Object + menu |
| Existing .fl4 with Decimate/Bevel | Keep card + warning; no silent strip on load |
| `xray_mode` left dead | OK for Phase 0; field deletion later with save audit |
| Bevel inverse RED | Do not expand Phase 0; record only |
| Dirty main tree on `fix/poly-corruption-family` | Worktree is clean master — do not copy dirty files |

---

## 6. Session log

- **2026-07-19** — Worktree created: `.claude/worktrees/poly-phase0` on `feat/poly-phase0` @ `7563c27`. Plan written after code audit. Implementation starting.
- **2026-07-19 (impl)** —
  - **P0.1** `bevel_apply_inverse_apply_is_identity` **GREEN** (`cargo test -p core_math --lib bevel_apply_inverse` → 1 passed). Parked seeds in `KNOWN_bevel_inverse_regression_seeds.txt` are **not** auto-loaded by proptest; leave as optional follow-up (not RED today). Gate status: **closed for random 96**.
  - **P0.2** Add Modifier dropdown: Decimate/Bevel removed; six live types only; weak note under list. Dead cards kept for save-compat.
  - **P0.3a** Removed `Shift: Fine` from Transform + Gizmo hint rows (only comment remains).
  - **P0.3b** Overlay X-Ray re-keyed to `!sel.visible_only` (same as header). Dead `viewport.xray_mode` no longer written.
  - **P0.4a** F-pie suppressed when `Polygonal && Vertex` so Fill owns bare F.
  - **P0.4b** Ctrl+I: code already gates `instance_key && !poly_sub_mode` — **VERIFY OK by inspection**; no code change.
  - **P0.5** `poly_apply_inset_settled` dry-runs on clone before `transaction()` (circle pattern).
  - **Build:** `cargo build -p app` Finished ok.

---

## 7. File touch map (expected)

| Slice | Paths |
|-------|--------|
| P0.1 | docs only unless test harness for seeds |
| P0.2 | `core_engine/app/src/properties_poly.rs` |
| P0.3a | `core_engine/app/src/viewport_hints.rs` |
| P0.3b | `core_engine/app/src/viewport_overlays.rs` |
| P0.4a | `core_engine/app/src/input_keyboard.rs` |
| P0.4b | verify only |
| P0.5 | `core_engine/app/src/poly/inset.rs` |
| Close | `docs/planning/poly/PHASE0_HONESTY_PLAN.md` §6, tracker |
