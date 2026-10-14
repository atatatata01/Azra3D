# Independent Review — Sculpt Performance & Feel

**Type:** Review + roadmap only — **no code changes**  
**Lens:** Fresh eye on the other agent’s sculpt work (`SCULPT_FEEL_*`, worktree `sculpt-feel-perf` / branch `fix/sculpt-feel-perf`) + current mainline hot paths  
**Date:** 2026-07-20  
**Goal:** One coherent plan to make sculpting **faster** and **feel** closer to Blender / ZBrush / 3D-Coat, without band-aids or red-line violations  

---

## 0. Executive verdict

| Dimension | Score (1–10) | One-line |
|-----------|-------------:|----------|
| **Methodology (feel oracle)** | **8.5** | Blender-as-behavior-oracle (not vertex parity) is correct and rare — keep it. |
| **Delivered feel wins** | **7** | F1a falloff + Clay-1 converge + Symmetrize are real; incomplete stack. |
| **Delivered perf wins (Move)** | **7.5** | Async Move bake + density-warp lineage is the right class of fix; still heavy at high res. |
| **System coherence** | **5** | Feel track and perf track are **two plans that don’t fully meet**. Worktree has large unmerged delta. |
| **Production “ZBrush-ready”** | **4** | Math/brushes deep; lag walls, incomplete undo, missing feel levers, dual display modes still define the experience. |
| **Ship readiness of open work** | **3–5** | `sculpt-feel-perf` worktree ~**+5k lines**, log ends mid-spec with **Result/Kept unfilled** — not owner-verified. |

**Bottom line:** The other agent did **high-quality, disciplined** work on *how* to tune feel and on *Move latency*. What is missing is a **unified, priority-ordered program** that treats **frame latency, visual feedback, and brush response** as one product surface — and that **finishes open correctness** (Move undo after grow) before more feel knobs.

---

## 1. What the other agent got right (preserve)

### 1.1 Methodology (Session 0–1) — **keep as law**

- Voxel sculpt ≠ Blender mesh ops → **no vertex parity** against Blender.
- Oracle forms that *are* valid:
  1. Falloff LUTs (1D)  
  2. Radial displacement fields  
  3. Behavioral scalars (spacing, accumulate)  
- CPU/GPU **byte-identical** stamp math + golden parity — non-negotiable (already in CLAUDE red lines).
- One phase + log entry + tests green — good process.

### 1.2 Concrete feel landings that matter

| Session | Landing | Why it counts |
|---------|---------|----------------|
| F1a | Default falloff = Blender smoothstep (`3e²−2e³`) mixed to hard `e⁸` | Fixes “every brush felt linear/sharp” — foundational |
| Clay-1 | ClayBuildup **converging plane fill** (not unbounded add) | Stops spike dwell — clay becomes packable |
| Symmetrize | One-shot mirror + sculpt-undo | Workflow completeness, not “feel math,” still high value |

### 1.3 Concrete perf landings that matter

| Theme | What landed (log + commits on `fix/sculpt-feel-perf`) | Why it counts |
|-------|------------------------------------------------------|----------------|
| Move freeze | Async worker for Move voxelize (Session 6) | Removes multi-second UI-thread freeze |
| Move cost model | Density warp / sphere-sweep cells / multi-warp polish (commit series) | Correct problem class: **latency walls**, not “more FPS tips” |
| Diagnostics | `[STALL]` / `[MOVEBAKE]` instrumentation | Evidence-driven; keep until verified then strip |

### 1.4 Architecture framing (`MESH_VOXEL_SDF_PLAN.md`) — **mostly correct**

Important reframes that should guide all future work:

1. Per-stamp sparse GPU path is **already near-minimal** when atlas-authoritative (no remesh every dab at high res).  
2. “Millions of polys” is largely an **Assembly viewport** problem, not the sculpt-room stamp loop.  
3. Sculpt pain is mostly **latency walls** (Move, remesh drain, save readback, rebuild).  
4. Dual-Room Surface (mesh-authoritative detail beyond voxel) is **hard** and correctly scored secondary.

---

## 2. Critical gaps (fresh-eye adversarial)

### 2.1 Completeness — feel program stopped mid-stack

| Planned | Status | Impact if unfinished |
|---------|--------|----------------------|
| F1b full Kernel presets live | Dead enum / placeholders (Session 1 finding) | UI lies; only F1a bridge hardness |
| F2 Grab field oracle | Not done on main | Move density path evolved without field-oracle gate |
| F3 Spacing 10% (Blender) | Worktree mentions 0.10; main default historically **0.25** | Gaps on flicks, pile-up on slow — **#1 industry feel lever** |
| Clay-2 area plane | Spec only | Clay still follows local wobble |
| Clay-3 square tip | Spec only | Clay Strips aesthetic incomplete |
| Pressure ramps / stabilizer polish | Partial / defaults only | Tablet users underserved |

**Risk:** shipping F1a + Clay-1 alone looks “almost Blender” in marketing and still feels wrong on Draw spacing + Clay evenness + Move.

### 2.2 Completeness — Move undo is a **correctness P0**, not polish

Session 7 log (main) is explicit:

- Every outward Move can **grow** the grid → `voxel_undo_reset()` → **`push_undo=false`** → Ctrl+Z empty.  
- Spec for `VoxelGridUndoStack` + truncate-on-evict is **written but not closed** (Result/Kept empty).  
- Worktree has large undo/types delta — **must be treated as unfinished until adversarial review + owner A/B**.

**Fresh-eye ranking:** **Fix Move undo before more brush recipes.** Broken undo destroys trust faster than soft falloff helps.

### 2.3 Dual sources of truth still fight “feel”

| Mode | Mid-stroke truth | Visual feedback | Failure mode |
|------|------------------|-----------------|--------------|
| Sparse atlas strokes | GPU atlas | Raymarch and/or chunk mesh | CPU grid stale; remesh lag |
| Dense mesh-grab Move | Display mesh + deferred density | Shader grab preview | Bake lag; undo identity break on grow |
| Colour / mask previews | Zero-remesh vertex write | Can desync if gates wrong | “Paint never shows” class (already fixed once) |

**Feel implication:** User reads **latency of visual commitment** as “heavy brush,” even when stamp GPU is cheap.

### 2.4 Performance model still has walls (even after async Move)

| Wall | Symptom | Severity |
|------|---------|----------|
| **Move density bake** (worker) | Still O(dirty volume); high-res multi-instance costly | P0–P1 at ≥256³ dense |
| **Chunk remesh drain** (`FRAME_BUDGET_MS ≈ 14`) | Pop-in / lag after stroke | P1 |
| **Full remesh / resident rebuild** | Leave-edit, grow, parity, save | P0 freeze risk |
| **Save / leave sync land** | Block on pending Move + atlas materialize | P1 |
| **Display policy docs stale** | Plan says ≥256 raymarch; older docs >512 | Confusion → wrong perf work |

**Honest reframe (agree with MESH_VOXEL plan):** don’t optimize “triangle count in sculpt room” as primary. Optimize **time-to-visible-solid** and **time-to-undoable**.

### 2.5 Process risks in the other agent’s workstream

| Risk | Evidence | Mitigation |
|------|----------|------------|
| **Large unmerged worktree** | `fix/sculpt-feel-perf` ~+5k lines across 23 files | Gate merge; phase PRs; adversarial review |
| **Log / code drift** | Session 7 ends as design draft mid-file | Require closed LOG entry before merge |
| **Perf commits without feel gates** | Move density warp evolved fast | Re-attach F2 field oracle for Grab |
| **Instrumentation left on** | `[MOVEBAKE]` / `[STALL]` | Strip after owner confirm |
| **Scope creep into app god-crate** | `input_sculpt` + `ui_panels` huge | Keep feel in core_math/graphics; app only wiring |

### 2.6 What industry “feel” actually needs (checklist the agent partially covers)

| Lever | Industry default | AZRA position | Priority |
|-------|------------------|---------------|----------|
| Falloff presets | Full family | F1a only | High (F1b) |
| Spacing | ~10% radius | Often 25% / partial | **Critical** |
| Clay plane | Area-averaged + converge | Converge only | High (Clay-2) |
| Cursor preview | Live brush sphere + falloff | Incomplete | High for trust |
| Lazy / stabilize | Screen-space | Present | Medium tune |
| Tablet pressure | Strength + radius ramps | Partial | High for tablet owners |
| Symmetry | Live + one-shot | Both exist | Medium polish |
| Undo | Instant per stroke | **Broken on grow Move** | **P0** |
| Resolution | Stable identity | Grow thrash undoes history | P0 system |
| High-res display | Raymarch live | Mesh drain still hurts | P1 |

---

## 3. Principles for the unified plan

1. **Correctness before feel cosmetics** — undo, no device-loss remesh, no silent grow-reset.  
2. **Latency of feedback > peak throughput** — async, budgets, raymarch during stroke.  
3. **One oracle discipline** — every feel change has a ref JSON or an explicit “behavioral-only” label.  
4. **CPU/GPU parity never waived** for stamp math.  
5. **Display Invariant** — no fused analytical + raster.  
6. **No magic constants** — derive or expose (agent rule — keep).  
7. **Mainline or nothing** — unfinished worktree work is not “done.”  
8. **Separate Assembly “millions of polys”** from sculpt-room work (MESH_VOXEL plan).  

---

## 4. Unified roadmap (ordered)

### Track A — Trust & correctness (block feel marketing)

| ID | Item | Why | Depends | Effort | Gate |
|----|------|-----|---------|--------|------|
| **A0** | Inventory main vs `fix/sculpt-feel-perf`; write merge map | Avoid double-work / lost fixes | — | S | Diff list + owner priority |
| **A1** | **Move grow undo: GridSnapshot + truncate-on-evict** (close Session 7) | Ctrl+Z empty destroys trust | A0 | M | Grow Move → Ctrl+Z restores; no cross-stack corruption |
| **A2** | Pending-grab undo (if deferred Move still has window with no entry) | Instant undo before bake lands | A1 | S–M | Ctrl+Z during pending matches visual |
| **A3** | Cancel/land invariants under save/leave/clear (re-verify Session 6 adversarial list) | Regression class | A1 | S | Checklist green |
| **A4** | Strip `[MOVEBAKE]`/`[STALL]` after A1–A3 owner OK | Log noise | A3 | S | Clean console |

### Track B — Feel core (Blender-comparable response)

| ID | Item | Why | Depends | Effort | Gate |
|----|------|-----|---------|--------|------|
| **B0** | Confirm F1a + Clay-1 are on **mainline** with parity green | Foundation | — | S | `cargo test -p core_math/graphics` + A/B dab |
| **B1** | **Stroke spacing default → ~0.10r** (+ optional UI) | #1 industry lever; gaps/pile-up | B0 | S | `spacing_ref` + fast-flick no gaps |
| **B2** | **F1b Kernel presets live** (Smoothstep/Smoother/Root/Sphere/Linear/Constant) CPU+GPU+UI | Kill dead Kernel enum | B0 | M | LUT tests vs `falloff_presets_ref.json` |
| **B3** | Clay-2 area plane (avg pos/normal under area radius) | Even clay across bumps | B0 | M | Visual evenness + CPU/GPU parity |
| **B4** | F2 Grab displacement field oracle vs density Move | Prevent drift from Blender Grab feel | A1, B0 | M | `grab_field_ref` epsilon |
| **B5** | Cursor preview (radius + falloff hint) | Immediate “weight” feedback | B1 | S–M | Visible under brush |
| **B6** | Pressure curves (strength/radius) polish | Tablet feel | B1 | S | Manual tablet A/B |
| **B7** | LazyMouse defaults + optional Stabilize polish | Continuity | B1 | S | Fast flick continuous |
| **B8** | Clay-3 tip_roundness (optional) | Clay Strips look | B3 | S | Visual |

### Track C — Performance (sculpt room latency)

| ID | Item | Why | Depends | Effort | Gate |
|----|------|-----|---------|--------|------|
| **C0** | Single source of truth for display mode thresholds (256 vs 512 docs) | Stop wrong optimisations | — | S | Code + docs match |
| **C1** | **Stroke-time: guarantee no full-object remesh on dab path** (assert/budget) | Protect atlas-authoritative path | C0 | S | Profiling stamp path < budget |
| **C2** | Post-stroke remesh: budgeted drain + optional “finish remesh” pulse | Less pop-in | C1 | M | No multi-second hitch after stroke |
| **C3** | Move bake: measure residual cost after density-warp; target p95 land | Worker still expensive | A1 | M | Land p95 under agreed ms @256³ |
| **C4** | Async save / leave materialize (map_async precedents) | Kill save freeze @ high res | C0 | M | Save never freezes UI seconds |
| **C5** | Resident rebuild throughput (batch, frustum-first) | Leave-edit / grow | C2 | M | Rebuild not multi-second common cases |
| **C6** | Half-res raymarch during interaction (if raymarch path live) | Interactive high res | C0 | M | Orbit/stroke FPS stable ≥256³ |
| **C7** | Defer sparse forward-scatter Move (research) | Real sparse grab | — | L | Separate design spike |

### Track D — Workflow / product (not pure feel math)

| ID | Item | Why | Depends | Effort | Gate |
|----|------|-----|---------|--------|------|
| **D1** | Expose brush UI completely (picker, hardness, spacing, kernel) | Math without UI is dead | B1–B2 | M | All levers reachable |
| **D2** | Symmetrize paint colours (v1 shape-only gap) | Completeness | A1 | S | Paint mirrors too |
| **D3** | Sculpt → SDF action (mesh_to_sdf exists) | Room merge | C3 stable | M | One-shot body create |
| **D4** | SDF stamp into sculpt (boolean modifiers) | Differentiator | D3 | L | Documented workflow |
| **D5** | Dual-Room Surface Phase 0 only (mode enum + no-op plumbing) | Unblock future detail | Serialization proof | M | No behavior change |
| **D6** | Assembly viewport (millions of polys) | True Thread 3 | Separate plan | XL | Not sculpt-room scope |

---

## 5. Recommended execution order (next 6–8 weeks if sequential)

```
Week 1:     A0 merge map + A1 Move grow undo (P0 trust)
Week 1–2:   A2–A4 + B0 verify F1a/Clay-1 on main
Week 2:     B1 spacing + B5 cursor preview (feel leap users notice immediately)
Week 2–3:   B2 F1b presets + D1 UI exposure
Week 3:     C0–C2 remesh/display honesty + budgets
Week 3–4:   B3 Clay-2 + C3 Move bake p95
Week 4–5:   C4–C5 save/rebuild latency
Week 5+:    B4 Grab oracle, B6–B8 polish, D3 sculpt→SDF
Parallel:   D6 Assembly = separate team/track
Never block: C7 sparse Move research
```

**Parallelization:** Feel (B) and Perf remesh (C) can parallelize **after A1**. Do not parallelize more Move density rewrites with open undo.

---

## 6. Definition of done (product)

Sculpt is “good enough to market” when **all** of the following hold:

1. **Undo:** Any Move (including grow) is one Ctrl+Z step, before and after bake land.  
2. **Stroke:** Continuous dabs at default spacing; no visible gaps on fast flicks; no multi-second hitch mid-Draw.  
3. **Clay:** Dwell converges to a plane; build across bumps looks even (Clay-2).  
4. **Falloff:** Default matches Blender Smooth; presets selectable and CPU/GPU parity green.  
5. **High res:** ≥256³ sculpt stays interactive (raymarch or budgeted mesh); save does not freeze.  
6. **Parity:** Golden CPU/GPU + new sculpt_ref tests green on CI.  
7. **No device loss** from sculpt remesh/bind paths (related cell_vert work is separate but adjacent).  

---

## 7. Adversarial “do not do” list

| Temptation | Why not |
|------------|---------|
| Vertex-parity tests vs Blender meshes | Impossible / lying oracle |
| More brushes before spacing + undo | Cosmetics on broken trust |
| Merge entire `sculpt-feel-perf` as one PR | Unreviewable; undo half-finished |
| Sparse forward-scatter Move as “quick perf” | Research-grade races |
| Dual-Room Surface as P0 | Serialization + parity gaps |
| Magic hardness constants per scene | Band-aid rule |
| Optimize Assembly instancing inside sculpt room | Wrong problem |

---

## 8. Merge strategy for the other agent’s worktree

Treat `fix/sculpt-feel-perf` as a **candidate stack**, not truth:

1. **List commits by theme:** Move density/async · Clay · UI · undo · brush library.  
2. **Rebase onto current main** (cell_vert, PT, etc. may conflict).  
3. **Land only closed LOG items** with tests + owner A/B.  
4. **Park** incomplete Session 7 code until A1 design is fully implemented and reviewed.  
5. Prefer **small PRs** matching Track A → B → C order above.

---

## 9. Metrics (how to know we improved)

| Metric | Capture | Target (illustrative) |
|--------|---------|------------------------|
| Input phase ms mid-Draw stroke | existing `time_block` / frame profiler | < 4 ms p95 @256³ |
| Move bake worker wall ms | `[MOVEBAKE]` worker line | < 200 ms p95 @256³ dense single-instance |
| Time to undoable after Move release | stopwatch | < 50 ms (pending entry) |
| Dabs per radius travel | automated | ≈ 10 at default spacing |
| Falloff LUT max error vs Blender | unit test | < 1e-4 (F1a already) |
| Clay dwell height after N frames | simple height sample | asymptote to plane, no unbounded growth |

---

## 10. Relationship to other critical plans

| Plan | Interaction |
|------|-------------|
| `CELL_VERT_BIND_LIMIT_CRITICAL_PLAN` | Mesh bind crashes during remesh — **orthogonal P0**; fix independently |
| `MESH_VOXEL_SDF_PLAN` | Parent architecture; this review **narrows** next 6 weeks |
| `SCULPT_FEEL_PLAN` | Keep oracle phases; **reorder** under Track A/B |
| `RENDER_ARCHITECTURE` Assembly | Owns millions-of-polys; do not conflate |

---

## 11. Final recommendation (one paragraph)

The other agent’s **feel methodology and first landings (smoothstep default, clay converge, symmetrize) are solid**, and the **Move async/density-warp performance work is the right attack on latency walls**. The program is **not yet a production sculpt tool** because **Move undo on grow is still broken in the record**, **spacing/kernel/area-clay are incomplete**, and a **large worktree sits unmerged** without closed verification. The next phase of work should be a **trust-first merge** (undo + mainline F1a/Clay-1), then **spacing + cursor + F1b** for immediate feel, then **remesh/display/save latency**, then Clay-2 and Grab oracle — with Assembly millions-of-polys kept on a **separate track**.

---

## 12. Owner decision checklist (answer before coding)

- [ ] Prioritize **A1 Move undo** over any new brush math? (**Recommended: yes**)  
- [ ] Merge strategy: cherry-pick from `fix/sculpt-feel-perf` vs re-implement A1 on main?  
- [ ] Default spacing: adopt **0.10** globally or per-brush?  
- [ ] F1b full presets now or after spacing? (**Recommended: spacing first, then F1b**)  
- [ ] Clay-2 before or after remesh perf C2? (**Either; Clay-2 is feel-only, C2 is latency**)  
- [ ] Is Dual-Room Surface in scope this quarter? (**Recommended: Phase 0 plumbing only or defer**)  

---

*End of independent review. No code was modified for this document.*
