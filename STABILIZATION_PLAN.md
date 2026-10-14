# STABILIZATION PLAN — read at the START of every session

**Purpose:** stop the whack-a-mole. We were fixing one crash at a time, each fix exposing the
next gap in the same in-flight area (P0.2-full Object Mode). This file is the single source of
truth for what we are doing, in what order, and how we know we are done. If work drifts off this
plan, that is a **deviation** and must be flagged (see Rules).

**Owner:** Dimitri. **Status date:** 2026-06-20.

---

## 1. Why we were going in circles (the honest diagnosis)

- A large architectural change — **P0.2-full** (SDF + voxel + poly all rendered as rasterized
  triangles in one Object Mode, SDF via baked→DC-mesh) — was committed, then its gaps were
  discovered **reactively**, one user-triggered crash at a time.
- Every gap lived in the **same path** (the SDF DC-mesh→raster pipeline), so each fix uncovered the
  next: invisible duplicate → gray faces → delete-freeze → duplicate crash → still-invisible
  duplicate → GPU wedge.
- **The dev loop itself was broken:** a single Rust panic aborts via Rayon, which causes a GPU
  **device loss** that *wedges the NVIDIA driver and leaves an unkillable process* — turning every
  bug into a reboot. That is why it felt like "working for nothing."

**The antidote:** (a) make the dev loop survivable, (b) audit the whole P0.2 path ONCE and fix all
gaps before handing back to test, (c) work in gated phases with checkpoints and a regression
checklist so a fix can never silently create the next bug.

---

## 2. North Star (the destination — does not change without explicit decision)

- **Object Mode = triangles only.** SDF (baked to mesh) + voxel (meshed) + poly (native), one
  G-buffer. (Display Invariant, `VIEWPORT_MODEL.md`.)
- **Per-object Edit Mode** uses that object's single technique (SDF→volume raymarch, voxel→raster/atlas, poly→raster).
- The app is **production-DCC stable**: create / edit / duplicate / delete / undo / save / reload an
  SDF, voxel, and poly object, in any order, **without a crash, an invisible object, or a wedged GPU.**

If a task does not move us toward this, it does not belong in the current gate.

---

## 3. Rules of engagement (the discipline)

1. **One change at a time.** Edit → `cargo build -p app` → run the regression checklist (§6) →
   commit a checkpoint → next. No piling changes onto an unverified state.
2. **Deviation protocol.** Before working on anything **not in the current gate**, I (Claude) STOP
   and say: *"This is a deviation from GATE N because X. Options: (a) add to plan, (b) defer, (c)
   switch focus."* You decide. No silent scope creep.
3. **Proactive, not reactive.** Before declaring a gate done, I run an audit/test pass to find the
   *remaining* gaps in that area — so you test a more-complete thing, not the next landmine.
4. **Checkpoints.** Confirmed fixes get committed immediately, so we can always return to a
   known-good state. "Stabilize" means: known-good commit + one change live at a time.
5. **No new features below the line.** SdfObject, bevel v1, polish — frozen until GATE 2 is signed off.
6. **Measure, don't guess.** If root cause isn't certain, add a diagnostic first (we have the
   `[DUPDIAG]` pattern). No speculative fixes.
7. **Every fix updates `COMPLETE_BUG_TRACKER.md`** and this plan's status table.

---

## 4. Current state (as of 2026-06-20)

- **Committed baseline:** `ba6be84` (SdfObject foundation, dormant) on top of `dc2c6bd` (P0.2-full).
- **Uncommitted, build-verified fixes in the tree** (need a checkpoint commit):
  - Bug A — duplicate SDF now bakes (atlas instancing-skip gated to SDF Edit Mode). `app_poll.rs`.
  - DUPCRASH — no more crash on repeat duplicate (DC bakes forced cubic + cubic-invariant guard).
    `bake_dispatch.rs`, `app_poll.rs`.
  - Bug B — gray faces fixed (geometric normal repair on CSG creases) + 2 unit tests. `chunked_dc.rs`.
  - Bug C — delete-last-object no longer freezes (Object Mode always Hybrid). `viewport_render.rs`.
  - `[DUPDIAG]` temporary render-decision trace in `raster_render.rs` (REMOVE after GATE 1).
  - Bevel scaffold (`bevel_op.rs`, `BEVEL_PLAN.md`) — separate track, parked.
- **Open right now:** duplicate **bakes + meshes but is still not visible** (render/position, not bake).
- **Dev-loop blocker:** panic → device loss → wedged GPU / unkillable process / truncated pipeline cache.

---

## 5. The gated backlog (do in order; sign off each gate before the next)

### GATE 0 — Make the dev loop survivable + checkpoint  ← **START HERE**
Goal: a crash can't cost us a reboot, and we have a known-good commit to fall back to.
- [ ] **S0.1 Commit the 4 confirmed fixes** (A, B, C, DUPCRASH) as one checkpoint (exclude `[DUPDIAG]`).
- [ ] **S0.2 Crash-survivability audit.** Why does a panic wedge the GPU + leave an unkillable
      process + truncate `pipeline_cache.bin`? Identify the minimum change so a panic fails CLEAN
      (releases the device, flushes/skips the cache write, exits). *This is the highest-leverage
      item — it makes every later gate cheap to test.*
- **GATE 0 DONE when:** fixes are committed; a deliberate panic exits without wedging the GPU or
  needing a reboot; the app reliably relaunches.

### GATE 1 — SDF duplicate is fully correct
Goal: Shift+D / Ctrl+D / menu / outliner / paste an SDF → visible, correctly placed, survives
undo/redo + save/reload.
- [ ] **S1.1** Diagnose the still-invisible duplicate via `[DUPDIAG]` (gate-skip vs overlapping-at-0.3
      vs off-screen). Fix the identified cause.
- [ ] **S1.2** Duplicate offset behaviour (the "slightly positioned copy"): decide on-top vs a clear
      separation; make it intentional/configurable.
- [ ] **S1.3** Verify undo/redo of a duplicate, and save→reload of a scene with a duplicate.
- [ ] **S1.4** Remove the `[DUPDIAG]` trace.
- **GATE 1 DONE when:** the regression checklist (§6) passes for duplicate across all entry points.

### GATE 2 — P0.2 Object-Mode DC-mesh path is solid (the proactive audit)
Goal: enumerate and close ALL remaining gaps in the SDF→bake→DC→raster path in one pass, so there
are no more "next landmines."
- [ ] **S2.1** Audit pass (one multi-agent sweep): materials, normals, winding, invalidation on
      every mutation (add/delete/reparent/group/dissolve/undo/redo/load), asymmetric bodies,
      save/reload, mixed SDF+voxel+poly scene, isolate/ghost, selection/picking.
- [ ] **S2.2** Fix the confirmed gaps from S2.1 in a deliberate order, one change + checkpoint each.
- [ ] **S2.3** (Perf follow-up, optional) per-axis DC accessor to restore asymmetric voxel savings
      that the cubic-forcing gave up.
- **GATE 2 DONE when:** the full create/edit/duplicate/delete/undo/save/reload matrix passes for
  SDF + voxel + poly with no crash, no invisible object, no gray surface.

### GATE 3 — Resume features (only after GATE 2 sign-off)
- SdfObject as a COMPLETE feature (creation + outliner + selection + delete + materials, all at once).
- Bevel v1 (its own `BEVEL_PLAN.md` track + gates).
- Sculpt-entry lag, polish.

---

## 6. Regression checklist (run after EVERY fix — this is what stops "fix one, break one")

`cargo build -p app` ✅ · `cargo test -p core_math` ✅ · then in-app:
1. Create an SDF box → visible, shaded (not gray).
2. Edit it (move a prim, bevel, subtract) → updates, stays shaded.
3. Duplicate it → copy is **visible** and **clearly placed**; duplicate again → **no crash**.
4. Delete objects one by one incl. the **last** → viewport clears each time (no frozen ghost).
5. Undo/redo the above → consistent.
6. Save → reload → scene matches.
7. Create a voxel + a poly object alongside the SDF → all three render together (P0.2 invariant).
8. No `[DUPDIAG]`/spam in console; no device loss.

A fix is **not done** until its relevant rows pass.

---

## 7. Decision & deviation log (append-only)

- 2026-06-20 — Created this plan after a reactive-debugging spiral. Decision: stop reactive fixes,
  adopt gates + deviation protocol. Confirmed fixes A/B/C/DUPCRASH pending checkpoint commit.
- 2026-06-20 — GPU wedged by panic-induced device loss (unkillable process, truncated pipeline
  cache). Elevated "crash survivability" to GATE 0 / S0.2.
- 2026-06-28 — Reactive marathon (~12 commits, `5b7c87a..5617bde`) closed the SDF Object-Mode render/bake
  cluster: white-box ghost (bounds-provenance + `gpu_texture_upload_done` active-body guard), the select
  freezes (`resident_buffer_valid`), added-body/512³ black (sparse gated `>256`, sparse PARKED), Solid-mode
  black (clay floor + dropped cast-shadow), escape balloon (bounded 2×), subtract-crease black (GPU normal
  repair). User-confirmed: no white box, no freeze, escape bounded. Detail in COMPLETE_BUG_TRACKER `2026-06-28`
  + memory `project_sdf_object_mode_render_invariants`.
- 2026-06-28 — **GATE 2 / S2.1 proactive audit RUN** (3 read-only agents): the P0.2 path is largely SOLID —
  Structural Mutation Checklist (14 ops), save→reload, duplicate, click-to-pick, SDF shadows, and the mixed
  SDF+voxel+poly single-G-buffer Display Invariant all verified correct; NO P0 gaps. Remaining (S2.2, fix-order
  = owner's call): [P1] asymmetric/cubic-forcing (= S2.3), [P1] multi-prim → single material (architectural),
  [P2] slot-realloc generation counter, [P2] empty-body mesh_cache cleanup.
