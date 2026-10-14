# AZRA — Feature Development Rules (Post-Q2 Architecture)

**Purpose:** The architecture is stable. These rules keep it that way during feature work.

---

## The 6 Invariants (break any of these = revert the PR)

1. **Every mutation through transaction()**
   No `scene.scene_graph.get_mut()` outside a `transaction()` or active episode.
   No direct `primitives[idx]` writes. No new `save_undo()` patterns.
   Test: `grep -rn "save_undo" app/src/` returns zero.

2. **Every scene object in the graph**
   No new `Vec<T>` for scene entities. New object types get a `NodePayload` variant.
   Selection by `NodeId` or `PrimUid`, never by flat index.

3. **Every async drain capped**
   Any new `try_recv()` or poll loop MUST have a `MAX_*_PER_FRAME` constant.
   Register with `FrameScheduler` using appropriate `WorkClass`.

4. **No pipeline/resolution switches during interaction**
   New render paths must respect `InteractionEpisode`. If adding a new pipeline
   mode, add it to `PipelineLatch`.

5. **No new god-file code**
   main.rs: only App struct + new() + eframe::App impl.
   New features go in dedicated files. New scene mutations go through
   scene_actions.rs or prim_actions.rs.

6. **No hacks — real path only**
   Never substitute a wrong stand-in for missing wiring (sphere for CurveExtrude,
   fake bounds for a mesh, dummy tool math). If bake/CPU already has the real
   eval, port it and bind the buffers. Fail closed or implement properly — no
   false silhouettes, no “good enough” wrong shape. See CLAUDE.md § No Hacks.

---

## Feature Prompt Template

Every feature prompt to Claude Code MUST include:

```
RULES:
- Read CLAUDE.md first. All rules apply.
- WORKTREE GATE: if this is UI/UX/ui2/theme/dock/shell work → you MUST be on
  branch feat/ui-polish at .claude/worktrees/ui-polish. If on master, STOP.
  See docs/WORKTREES.md. Never commit UI redesign to master.
- No hacks: real SDF/tool/render path only — no sphere/box stand-ins for missing shapes.
- All mutations through transaction() or begin_episode()/commit_episode().
- No direct primitives[] writes. No save_undo().
- New scene objects get NodePayload variant + NodeId selection.
- Any async poll loop must have MAX_*_PER_FRAME cap.
- Register frame-thread work with FrameScheduler.
- Do not add pub(crate) fn to main.rs.
- Build after every change: cd core_engine && cargo build -p app
- Update COMPLETE_BUG_TRACKER.md if fixing tracked bugs.
```

---

## Worktree Map

| Domain | Branch | Worktree | Launcher |
|--------|--------|----------|----------|
| **Everything by default** (incl. UI v2 / theme / docks) | `master` | repo root | `Dev_Azra.bat`, `Run_Fast.bat` |
| UI v2 shell (deprecated name, tracks master) | `feat/ui-polish` | `.claude/worktrees/ui-polish` | `Run_UIv2.bat` / `Dev_UI.bat` |
| Parametric poly primitives | `feat/poly-parametric-prims` | `.claude/worktrees/poly-params` | — |
| Viewport perf (Phase D) | `feat/viewport-perf` | `.claude/worktrees/viewport-perf` | — |

**The old "UI work on master is a process bug" gate was RETIRED 2026-08-08** — `feat/ui-polish` is the same commit as `master`, so the gate only forked history. Full table: `docs/WORKTREES.md`. Canonical rule: CLAUDE.md § *Branch Roles*.

---

## Before Starting Any Feature

Ask these 4 questions:

1. **Does it add a new scene object type?**
   → Add NodePayload variant. Add graph accessors on SceneState.
   → Add to save format (version bump if schema changes).

2. **Does it mutate scene state?**
   → Wrap in transaction() (atomic) or begin_episode()/commit_episode() (drag).
   → Verify undo works: do thing → undo → thing undone.

3. **Does it add frame-thread work?**
   → Register with FrameScheduler. Assign WorkClass.
   → Add drain cap if it polls async results.

4. **Does it add a render path or mode?**
   → Add to PipelineLatch in InteractionEpisode.
   → Verify zero path switches during interaction.

If the answer to all 4 is "no," it's a pure UI or pure math change — lowest risk.
**If pure UI:** do it only on `feat/ui-polish` (worktree). Never on master.

---

## Regression Checklist (run after every feature)

```bash
# Architecture invariants
grep -rn "save_undo\b" app/src/ | grep -v "fn save_undo" | grep -v "//"
# Expected: ZERO

grep -rn "\.primitives\[.*\] =" app/src/ | grep -v "//" | grep -v "undo.rs"
# Expected: ZERO

# Build + tests
cd core_engine && cargo build -p app
cd core_engine && cargo test -p core_math
cd core_engine && cargo test -p graphics

# Manual: create 5 prims → transform → boolean → undo x3 → save → load → undo
```

---

## Feature Priority Queue (Post-Architecture)

These are the features that were PARKED during Q2, now unblocked:

### Tier 1 — Build on the new architecture directly
| Feature | Uses | Complexity |
|---------|------|------------|
| Volume Preview wiring | Existing pipeline, additive | Small (prompt ready) |
| Nested SdfBodies | Scene graph ownership, transaction() | Large — killer differentiator |
| Snapping / anchors | FieldEvaluator | Medium |
| Export pipeline fixes | MaterialId, scene graph | Medium |

### Tier 2 — Independent work
| Feature | Notes |
|---------|-------|
| R1/R2/R3 BVH mask removal | Shader-only, no architecture risk |
| Bevel polish | core_math, no architecture risk |
| Mirror polish | Uses transaction(), low risk |

### Tier 3 — Larger scope, defer
| Feature | Why Defer |
|---------|-----------|
| AI backend integration | Needs material pipeline stable first |
| SDF + sculpt volume blend | Depends on nested bodies + volume ADR |
| Sparse Brick Pool | 23 bugs, needs volume path stable |
| OpenColorIO / MaterialX | Ecosystem, not core |
| Plugin API | Needs stable public API surface |

---

## One Feature At A Time

The rule that killed bugs during architecture work applies to features too:
**One feature per prompt. One prompt per agent. Diagnostic before fix.**

Do NOT:
- "While I'm in this file, also add X" — scope creep reintroduces chaos
- Start a new feature before the current one passes the regression checklist
- Skip the 4 questions above for "small" changes — small changes compound

The architecture survived 23 agents across 4 prompts because each agent
had ONE task, strict file ownership, and a gate to pass. Features get
the same discipline.
