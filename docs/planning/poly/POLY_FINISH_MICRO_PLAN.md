# POLY FINISH — Micro Plan (Context-Limited Sessions)

**Status:** ACTIVE execution plan for remaining poly modeling follow-up  
**Date:** 2026-07-19  
**Authority chain:** `POLY_FOUNDATION_PLAN.md` (architecture) → `POLY_POLISH_PLAN.md` (oracle method) → **this file** (ordered micro-sessions)  
**Companion docs:** `BEVEL.md` (bevel only), `PHASE0_HONESTY_PLAN.md` (trust leftovers), `PHASE3_STATUS.md` (GRS notes)  
**Baseline branch tip at plan write:** `master` (post spine merge); re-read live code before each session  

---

## 0. Why this plan exists

The remaining poly work is **large** but **highly partitionable**. Context windows kill multi-tool sessions:

- Agents re-derive Blender math already documented in polish/bevel logs  
- Half-edge + app + keybinds + undo in one turn overflows and ships half-broken  
- “Just finish poly” produces silent wrong geometry  

**This plan forces one shippable unit per session**, with a hard file budget, a single done gate, and zero “while I’m here” refactors.

---

## 1. Session contract (non-negotiable)

### 1.1 One session = one micro-ID

| Rule | Detail |
|------|--------|
| **One micro-ID only** | e.g. `C1`, not “C1 + C2 if time” |
| **One commit** (or one PR) | Message: `poly(C1): <title>` |
| **File budget** | Prefer ≤ **6 production files** + tests/docs. Hard cap **10** unless ID is marked L |
| **No kernel+app+keymap redesign** | Wire existing paths; don’t invent parallel undo |
| **No half_edge serialized shape change** | Never add fields to `HalfEdgeMesh` / HVertex / HFace / HalfEdge layout |
| **Blender oracle when geometry changes** | Kernel topology/geometry IDs: oracle JSON + golden test. App-only wire: dry-run refuse + manual scenario |
| **Regression minimum** | `cargo test -p core_math` (or targeted filter if suite too slow) + `cargo check -p app` |
| **End of session** | Tick this plan `[x]`, one line in `POLY_POLISH_PLAN.md` §7, stop |

### 1.2 Context pack (paste into agent prompt — keep short)

```
You implement ONE micro-ID from docs/planning/poly/POLY_FINISH_MICRO_PLAN.md: <ID>.
Read ONLY: this plan’s § for that ID + POLY_POLISH_PLAN §2–§3 (oracle method) + listed files.
Do NOT read BEVEL.md unless ID starts with B.
Do NOT implement any other ID. Do NOT refactor. Do NOT “improve while here”.
Done = acceptance tests in the ID section all green + one commit.
```

### 1.3 Session size classes

| Class | Est. wall time | Context risk | Typical work |
|-------|----------------|--------------|--------------|
| **XS** | 30–90 min | Low | UI remove dead item, keybind gate, wire existing kernel |
| **S** | 2–4 h | Low–med | App tool + dry-run refuse + thin kernel |
| **M** | 1 day | Med | Oracle + kernel op + app wire |
| **L** | 1–2 days | High — split if possible | Multi-face region topology (extrude/inset region) |

If an L session exceeds ~half a context window without a green test: **stop, commit partial only if tests pass**, open a follow-up micro-ID. Never leave half-shipped topology.

### 1.4 Definition of Done (DoD) template

Every ID uses this checklist:

1. [ ] Kernel (if any): watertight/manifold as required + oracle or geometric gates  
2. [ ] App: one entry path (key/menu/cmd), **refuse-before-transaction** on no-op/fail  
3. [ ] Undo: success → one entry; refuse → zero entries  
4. [ ] `cargo test -p core_math` (relevant filters) green  
5. [ ] `cargo check -p app` green  
6. [ ] This plan ID marked `[x]` + §7 one-liner in polish plan  

---

## 2. Dependency graph (load-bearing)

```
WAVE 0  Trust / dead UI          ── independent, do anytime ─────────────┐
WAVE 1  Region defaults          ── C1 extrude_region before C2 inset_region
WAVE 2  Merge family             ── after Wave 1 preferred (shares weld patterns)
WAVE 3  Wire-existing + small    ── after Wave 1 (symmetrize free; after region for Alt+E)
WAVE 4  Bridge loops             ── independent of Wave 2; after Wave 1 nice-to-have
WAVE 5  Medium tools             ── parallelizable once Wave 1–2 green
WAVE 6  LoopTools family         ── after Circle handle (optional) + shared loop helpers
WAVE 7  Bevel edge cases         ── BEVEL.md only; never block Wave 1
WAVE 8  Architecture stretch     ── CustomData / GRS S3 / parametric DAG — LAST
```

**Hard deps:**

| Must complete first | Before |
|---------------------|--------|
| C1 `extrude_region` | C3 Extrude Along Normals (Alt+E) |
| C1 (optional but preferred) | E2 interactive extrude drag honesty |
| D1 Merge at center | D2 Merge by distance (shares weld/self-loop) |
| Circle kernel (DONE) | F1 radius handle |
| F1 or Circle scaffolding | F2–F6 LoopTools ops |

**No dep (safe parallel tracks):** Wave 0 honesty · Wave 7 bevel · S1 Symmetrize wire · S3 Shade smooth  

---

## 3. Waves & micro-IDs

Legend: `[ ]` open · `[~]` partial · `[x]` done  

---

### WAVE 0 — Trust & dead affordances (XS, unblocked)

Goal: UI never offers a lie; keys don’t double-fire. **No geometry.**

| ID | Title | Size | Files (expected) | Acceptance |
|----|-------|------|------------------|------------|
| **T1** | Remove Decimate/Bevel from modifier **Add** only | XS | `properties_poly.rs` | Add list only live types; existing dead stack cards may keep “Not yet” |
| **T2** | Kill dead “Shift: Fine” on Transform/Gizmo hints | XS | `viewport_hints.rs` | String gone; Ctrl=Snap kept if real |
| **T3** | Re-key dead X-Ray button → `sel.visible_only` | XS | `viewport_overlays.rs` (+ header if needed) | Button toggles live visibility filter; zero writes to dead `xray_mode` |
| **T4** | F double-fire: Fill wins in poly Vertex | XS | `input_keyboard.rs` and/or F-pie gate | Vertex mode: F fills only; Object poly: F-pie/focus as designed |
| **T5** | Verify Ctrl+I no phantom instance in poly sub-mode | XS | read `input_keyboard.rs` | Manual: Vertex mode Ctrl+I = invert only. Code change only if broken |
| **T6** | Inset refuse-before-tx (if still missing) | XS | `poly/inset.rs` | Dry-run empty → no undo entry (match circle) |

**Session prompt:** Wave 0 IDs only. Do **not** open `half_edge.rs`.

---

### WAVE 1 — Region defaults (highest user value)

Goal: multi-face select behaves like Blender **E** / **I**.

| ID | Title | Size | Kernel | App | Oracle | Acceptance |
|----|-------|------|--------|-----|--------|------------|
| **C1** | `extrude_region(faces)` multi-face weld | **L** | `half_edge.rs` or mesh_ops | `poly/extrude.rs` | Extend `extrude_region_ref.py` / use existing region refs | 2+ adjacent faces: one region, shared interior edges not duplicated; manifold; Blender v-for-v on cube 2-face case; single-face path unchanged |
| **C1b** | App E default routes to region when \|faces\|>1 | S | — | `extrude.rs`, key_dispatch | — | Multi-face E uses C1; 1 face uses existing; zero-offset refuse no undo |
| **C2** | `inset_region(faces)` + Individual flag | **L** | `half_edge.rs` | `poly/inset.rs` | `inset_region_ref.py` | Region default: interior shared edges not inset; Individual=true matches per-face; even offset preserved |
| **C2b** | App I default = region; toggle Individual | S | — | inset + adjust panel | — | I multi-face = region; toggle + redo panel; refuse-before-tx |
| **C3** | Extrude Along Normals (Alt+E) | S–M | thin wrapper on C1 | extrude + keybind | optional | Caps move on smoothed vertex normals; after C1 |
| **E2** | Interactive extrude drag multi-face honesty | S | — | extrude drag path | — | Drag multi-face does not per-face split if C1 exists |

**C1 split rule (if context blows):**

1. Session C1-kernel: pure function + unit tests only (no app)  
2. Session C1b: wire app  

**Do not start C2 until C1 kernel tests green.**

---

### WAVE 2 — Merge family

| ID | Title | Size | Notes |
|----|-------|------|-------|
| **D1** | Merge At Center (selected verts) | M | Single target centroid once; collapse self-loops + degenerate faces; deterministic; oracle `merge_ref.py` At Center |
| **D2** | Merge At First / At Last | S | Same path as D1, different target pick |
| **D3** | Merge by Distance (spatial hash) | M | Threshold UI; oracle By Distance |
| **D4** | DissolveVertex (fan → one face) | M | Completes polish 2b REMAINING; Blender dissolve verts |
| **D5** | Merge/Collapse edges & faces to point | M | Can follow D1 patterns; separate ID |

**Order:** D1 → D2 → D3; D4 parallel after D1; D5 last in wave.

---

### WAVE 3 — Wire-existing & small tools (cheap wins)

| ID | Title | Size | Notes |
|----|-------|------|-------|
| **S1** | Wire Symmetrize (kernel exists) | S | App op + menu + key; kernel already tested — **do not rewrite kernel** |
| **S2** | Shrink/Fatten (Alt+S) | S | Per-vertex normal translate; no topology; modal + confirm refuse no-op |
| **S3** | Shade Smooth / Flat per-face + autosmooth angle | S | Partial today; finish face flag + UI |
| **S4** | Circle interactive radius handle | S | Kernel returns radial dirs; viewport handle + drag; no re-fit each frame |
| **S5** | “Delete Only Faces” menu entry | XS | Kernel already correct; menu/key only |

---

### WAVE 4 — Bridge edge loops

| ID | Title | Size | Notes |
|----|-------|------|-------|
| **R1** | Kernel `bridge_loops(loop_a, loop_b)` | M | Face strip; oracle two N-gons; keep legacy 1/2-vert “make quad” separate |
| **R2** | App Bridge uses R1 when two loops selected | S | Dry-run refuse; undo honesty already started — preserve |

---

### WAVE 5 — Medium tools (parallel after Wave 1–2)

Each ID = own session. Order inside wave is preference, not hard dep.

| ID | Title | Size | Notes |
|----|-------|------|-------|
| **M1** | Separate (P) → new PolygonalBody | M | Island/face split; scene graph + undo |
| **M2** | Bisect (plane + fill/clear) | M | Reuse knife plane infra |
| **M3** | Limited Dissolve (angle threshold) | M | Expose boolean’s internal planar merge carefully |
| **M4** | Standalone Edge Slide | M | Not only loop-cut-time |
| **M5** | Standalone Vertex Slide | M | |
| **M6** | Rip (V) | M | |
| **M7** | Rip Fill (Alt+V) | M | After M6 |
| **M8** | Select Similar (Shift+G) | M | normal/area/material/valence modes — can split M8a/b |
| **M9** | Snap-to-geometry (V/E/F) for edit G/R/S | M | |
| **M10** | Grid Fill | L | Split kernel/app if needed |
| **M11** | Spin | M | |
| **M12** | Screw | M | After M11 optional |

**Parallelism:** M1, M2, M3, M8 can run in different worktrees **if** they don’t all edit `key_dispatch.rs` / `half_edge.rs` same day — prefer **one agent owns half_edge**.

---

### WAVE 6 — LoopTools family

Shared scaffolding already in Circle (`region_boundary_loops`, `newell_normal`, `best_fit_circle_2d`).

| ID | Title | Size | Oracle |
|----|-------|------|--------|
| **F1** | = S4 Circle handle (if not done in Wave 3) | S | — |
| **F2** | Relax | M | LoopTools headless |
| **F3** | Flatten | M | |
| **F4** | Space | M | |
| **F5** | Curve | M | |
| **F6** | GStretch | M–L | |

One op per session; copy Circle’s app pattern (cmd + redo panel + refuse-before-tx).

---

### WAVE 7 — Bevel edge cases only

**Do not block Waves 1–6.** Read `BEVEL.md` §8–§9 only.

| ID | Title | Size |
|----|-------|------|
| **B1** | Bevel-on-beveled k=1 deg>3 partial corner (safe flat → better) | L |
| **B2** | Mixed corners two consecutive unbeveled fan edges | L |
| **B3** | Bevel inverse parked seeds re-verify | XS diagnostic |

---

### WAVE 8 — Architecture stretch (explicitly last)

Only after Waves 1–5 feel “daily Blender-complete” for hard-surface.

| ID | Title | Size | Notes |
|----|-------|------|-------|
| **A1** | CustomData Stage A (Copy/Constant on extrude/inset) | L | Foundation §4.2; per-op PROPAGATES |
| **A2** | CustomData Stage B one op (e.g. UV after loop cut) | L | One op only |
| **A3** | GRS S3 delete PolyTransform single path | L | HUD risk; PHASE3_STATUS defers |
| **A4** | Parametric op-history DAG projection | L | Runtime only; never retype `Vec<PolyModifier>` |
| **A5** | Gen-tagged handles first consumer | L | Wrapper only |

---

## 4. Recommended calendar (context-safe)

Assume ~1 focused session/day, L = 2 days:

| Week | Sessions | Focus |
|------|----------|--------|
| **W1** | T1–T6, S1, S5 | Trust + free wires |
| **W2** | C1, C1b | Extrude region (the big one) |
| **W3** | C2, C2b, E2 | Inset region + drag honesty |
| **W4** | D1–D4 | Merge + DissolveVertex |
| **W5** | R1–R2, S2, S3, S4 | Bridge + thicken/small |
| **W6** | C3, M1, M2, M3 | Alt+E + Separate/Bisect/Limited Dissolve |
| **W7+** | M4–M12, F2–F6 | Medium + LoopTools |
| **Later** | B1–B2, Wave 8 | Bevel edge + architecture |

Adjust if owner prioritizes **Separate** or **Merge** over LoopTools.

---

## 5. Worktree / merge protocol (collision control)

| Rule | Detail |
|------|--------|
| **Prefer one poly worktree** | e.g. `feat/poly-finish` branched from current master |
| **Never two sessions edit `half_edge.rs` same day** | Serialize Wave 1/2/4 kernel IDs |
| **App-only IDs** (T*, S1, S5, C1b, C2b) | Can parallel with a kernel session on another worktree |
| **Integrate often** | FF/merge to master after each green wave, not at the end |
| **No force-push** | Linear history preferred |

---

## 6. Per-session template (copy-paste)

```markdown
## Session: <ID> — <title>
**Class:** XS|S|M|L
**Branch:** feat/poly-finish
**Read:** POLY_FINISH_MICRO_PLAN.md § <ID>, POLY_POLISH_PLAN §2-3, files: <list>

### Steps
1. Reproduce / oracle (if geometry)
2. Implement minimum change
3. Tests
4. App wire + refuse-before-tx
5. cargo test -p core_math <filter>
6. cargo check -p app
7. Commit: poly(<ID>): ...
8. Tick plan + polish §7

### Out of scope
Everything not in this ID.
```

---

## 7. Tracking checklist (tick here)

### Wave 0
- [x] T1 Add modifiers live-only  
- [x] T2 Dead Fine hints  
- [x] T3 X-Ray → visible_only  
- [x] T4 F double-fire  
- [x] T5 Ctrl+I verify  
- [x] T6 Inset refuse-before-tx  

### Wave 1
- [x] C1 extrude_region kernel  
- [x] C1b extrude_region app  
- [x] C2 inset_region kernel  
- [x] C2b inset_region app  
- [x] C3 Extrude Along Normals  
- [ ] E2 extrude drag multi-face  

### Wave 2
- [x] D1 Merge At Center  
- [x] D2 Merge First/Last  
- [x] D3 Merge by Distance  
- [x] D4 DissolveVertex  
- [ ] D5 Collapse to point  

### Wave 3
- [x] S1 Symmetrize wire  
- [x] S2 Shrink/Fatten  
- [ ] S3 Shade smooth/flat  
- [x] S4 Circle radius handle  
- [x] S5 Delete Only Faces entry  

### Wave 4
- [x] R1 bridge_loops kernel  
- [x] R2 Bridge app  

### Wave 5
- [x] M1 Separate  
- [x] M2 Bisect  
- [x] M3 Limited Dissolve  
- [x] M4 Edge Slide  
- [x] M5 Vertex Slide  
- [x] M6 Rip  

- [x] M7 Rip Fill  

- [x] M8 Select Similar  
- [x] M9 Snap geometry  

- [x] M10 Grid Fill  

- [x] M11 Spin  

- [ ] M12 Screw  

### Wave 6
- [x] F2 Relax  
- [x] F3 Flatten  
- [x] F4 Space  
- [ ] F5 Curve  
- [ ] F6 GStretch  

### Wave 7
- [ ] B3 Bevel inverse diagnostic  
- [ ] B1 Bevel k=1 high-degree  
- [ ] B2 Bevel mixed unbeveled fan  

### Wave 8
- [ ] A1 CustomData Stage A  
- [ ] A2 CustomData one-op Stage B  
- [ ] A3 GRS S3  
- [ ] A4 Op-history DAG  
- [ ] A5 Gen handles  

---

## 8. Explicit non-goals (until Wave 8)

- Porting full Blender BMesh  
- Rewriting half-edge to SoA  
- f64 CAD kernel  
- Geometry Nodes dual system  
- Batching two micro-IDs “to save sessions”  
- Silent attribute drop without PROPAGATES flag  

---

## 9. Success metrics

| Milestone | When |
|-----------|------|
| **M0 Trust** | Wave 0 all `[x]` |
| **M1 Daily region** | C1+C1b+C2+C2b green — multi-face E/I Blender-like |
| **M2 Merge complete** | D1–D4 green |
| **M3 Edit suite** | Waves 3–4 green (symmetrize, bridge, Alt+S) |
| **M4 Parity mid** | Wave 5 ≥ half done  
| **M5 LoopTools** | Wave 6 green |
| **M6 Stretch** | Wave 8 optional |

**Owner “poly is finished for hard-surface daily work”** ≈ **M1 + M2 + S1 + R1/R2**.  
Everything after is breadth, not blocking.

---

## 10. First three sessions (start here)

1. **T1 + T2 + T5** (same day only if XS and no half_edge) — or **T1 alone** if strict  
2. **S1 Symmetrize wire** — confidence win, kernel ready  
3. **C1 extrude_region kernel** — begin the real gap  

After C1 green: **C1b → C2 → D1**.

---

## 11. Maintenance

When an ID completes:

1. Tick §7 above  
2. Append one line to `POLY_POLISH_PLAN.md` §7  
3. If scope discovered larger: add `C1c` etc. — **do not expand the same ID mid-session**  
4. If plan drifts from code: fix **this file**, not tribal memory  

---

*End of POLY_FINISH_MICRO_PLAN.md*
