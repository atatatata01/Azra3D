# Frame Stall Plan — 2026-08-05 (Grok work order)

**Owner report:** *"I have a lot of stall, large ms number, what is happening."*

Three **independent** problems, diagnosed from the owner's console log. Do them in the order
below — Gate 0 first, because the biggest number is currently unmeasured and we must not guess.

Read `CLAUDE.md` before starting. This document does not repeat its rules; it assumes them.

## Execution status (2026-08-05)

| Gate | Status | Notes |
|------|--------|--------|
| **0** Split `unaccounted` | **DONE** | `cpu_section_ms[8..10]` = poll / setup / toast_menu; `[STALL]` line prints them |
| **1** Lazy smart-snap occlusion | **DONE** | Best-first score, then ≤8 occlusion probes; test `smart_snap_visible_only_refuses_occluded_point` |
| **2** Library cache + virtualise | **FIXED again** | Item grid was virtualized; **folder tree was not** — with ~2k asset dirs under open categories that alone cost 380–900ms. Tree now flattens + `show_rows`. |
| **3** Owner re-measure | **PARTIAL** | Gate 0/1 confirmed in owner log (`setup=284` = one-shot WGSL, `input=0`). Library still hot until this tree fix — re-measure needed. |

---

## The evidence (owner's log, verbatim)

```
[STALL] frame=388ms  | layout=383.0 input=0.1   ... | unaccounted=0.1    | tool=Poly Bevel
[STALL] frame=280ms  | layout=0.4   input=277.8 ... | unaccounted=0.1    | tool=Poly Bevel
[STALL] frame=331ms  | layout=0.4   input=328.1 ... | unaccounted=0.1    | tool=Poly Bevel
[STALL] frame=954ms  | layout=1.2   input=0.0   ... | unaccounted=939.5  | tool=Idle
[STALL] frame=1455ms | layout=0.5   input=0.0   ... | unaccounted=1443.0 | tool=Idle
[Overlays] 95.0ms = chrome 0.1 + scene 0.1 + library 94.9 (thumbs 0.0) + viewport 0.0
[Overlays] 12.1ms = ... library 12.0 ...
[BEVEL] edges=2 width=0.010 segments=3 ... -> verts 78->90 faces 62->68 manifold=true
```

The `[BEVEL]` lines matter: the mesh being edited is **78 vertices**, and `mesh=0.0` in every
stall line. The bevel kernel is **not** the cost. Do not go looking there.

---

## Gate 0 — Split `unaccounted` (DO THIS FIRST)

**Problem.** `unaccounted=1443ms` on an **Idle** frame is the largest number in the log and it is
a black box. `app_update.rs:1095` documents it as *"poll (line 121) + pre-panel setup +
toasts/menus/process_commands"* — three suspects, no timers.

**Task.** Add timers for those three regions and print them in the `[STALL]` line.

- `core_engine/app/src/app_update.rs` — the `[STALL]` print is at ~line 1099; the accounting sum
  `acc` is at ~line 1098. `self.ui.cpu_section_ms` is the existing timer array (slot 7 is already
  in use for `mode_auto_ms`, so extend the array rather than reusing a slot).
- Time: (a) `poll_background_tasks`, (b) pre-panel setup, (c) toasts / menus / `process_commands`.
- Print them, and keep `unaccounted` in the line so we can see whether it actually closes.

**Acceptance.** On a frame where `wall_ms > 120`, the printed sub-timers account for **≥95%** of
what used to be `unaccounted`. Hand the owner a build and one log line showing a stalling Idle
frame fully attributed. **Do not attempt a fix for the idle stall until this identifies it** —
that is the whole point of this gate.

---

## Gate 1 — Smart snap is quadratic (`input` 277–328 ms)

**Root cause, already located.** `find_smart_snap` runs every frame the pointer moves
(`core_engine/app/src/smart_snap.rs:21`). For **each** candidate it calls
`snap_candidate_occluded` (`smart_snap.rs:263`), which calls
`raycast_scene_meshes` (`core_engine/app/src/camera.rs:133`) — and that walks **every**
`renderable_meshes()` entry and **every quad** in each. There is a second occlusion call nested
inside the edge-segment loop. Cost ≈ `candidates × scene triangles`, **per frame**.

`smart_snap_visible_only` defaults to `true` (`app_types/src/types_viewport.rs:185`), so every
user pays it. This landed in the 2026-08-05 merge round.

**Task.** Make occlusion **lazy**, not exhaustive. The candidates are already scored by screen
distance and priority. Choose the best candidate first, then test occlusion only on that one;
if it is occluded, fall to the next best and test again. Stop at the first visible winner.

Worst case that still degenerates (everything occluded) must be bounded — cap the number of
occlusion probes per frame with a named constant and a comment, per CLAUDE.md's Frame Budget
Rule ("never add unbounded per-frame work").

**Do NOT:**
- Do not "fix" this by defaulting `smart_snap_visible_only` to `false`. That changes behaviour to
  dodge a cost. The owner rejected exactly that move today on the UI panels.
- Do not add a frame-skip / every-Nth-frame throttle. Snapping must be correct on the frame the
  user clicks.
- Do not cache occlusion results across frames keyed on the mouse position — the camera and the
  mesh both move.

**Acceptance.**
1. `input` phase **< 5 ms** during a bevel drag and a gizmo drag in the owner's scene. Paste the
   before/after `[STALL]` lines.
2. A test proving smart snap still **refuses** an occluded point — i.e. a candidate behind a
   surface must not win. Without this, "fast" and "broken" are indistinguishable.
3. `cargo test -p app` green.

---

## Gate 2 — Library panel costs 10–95 ms every frame

**Problem.** `[Overlays] ... library 94.9` and `library 12.0` on ordinary frames. The owner's
library holds **2278 assets** (visible in their screenshot). The panel rebuilds its list every
frame.

**Task.** Two changes, both standard:
1. **Cache the built list**, keyed on what actually changes it (filter string, active category,
   asset count / scan generation). Rebuild only on a key change.
2. **Virtualise the rows** — build widgets only for rows inside the visible scroll viewport.
   `egui::ScrollArea::show_rows` exists for this.

**Do NOT** reduce the cost by not drawing the panel, or by drawing a cached *picture* of it. That
was tried on 2026-08-05 (`ui2/idle.rs`) and produced three regressions in one afternoon; it is
now opt-in behind `AZRA_UI_IDLE=1` and **must stay off by default**. See commit `8fc7936`.

**Acceptance.** `library` under **1 ms** steady-state with 2278 assets loaded, with scrolling,
filtering and category switching all still correct. Paste the `[Overlays]` line before and after.

---

## Gate 3 — Re-measure and report

Run the owner's workflow: load a scene with the library populated, select a poly body, drag a
bevel, orbit, sit idle. Paste:
- the `[STALL]` lines (should be rare, and attributed when they appear),
- the `[Overlays]` line steady-state,
- `cargo test -p app` and `cargo test -p core_math` counts.

State honestly which of the three problems is fixed and which is not. A partial result reported
accurately is worth more than a claim.

---

## Constraints and traps

- **Build after every change:** `cargo build -p app`. Do not pile changes on a broken tree.
- **Frame Ordering Contract:** no GPU resource creation (`create_texture`, `create_buffer`,
  atlas resize) inside egui panel draws or input handlers. Defer to `poll_background_tasks`.
- **Frame Budget Rule:** any new per-frame loop needs an explicit cap constant.
- **No bandaids.** If a fix hides the problem rather than removing it, it is the wrong fix. Do not
  downgrade an assert, do not add a timing delay to dodge a race, do not special-case a caller.
- **Measurement discipline.** Cold vs warm cache matters; measure the same scene the same way
  before and after, and say which. Do not report a speedup measured against a cold first frame.

### Concurrency warning — the repo has other sessions live right now

- `core_engine/app/src/project/diag_edge_loop_22.rs` **does not compile** (it reads the private
  `free_verts` / `free_edges` fields of `HalfEdgeMesh`). It blocks `cargo test -p app` entirely.
  Either coordinate with whoever added it or fix the access — but do **not** delete their file.
- `poly_knife.rs`, `mesh_boolean/*`, `ops_dissolve.rs`, `extrude_drag.rs` and `project/tests.rs`
  have uncommitted work from another session. Two suite failures
  (`s_straight_slice_collapses_densify_pins_to_clean_segments`, `intrude_owner_fixtures_pinned`)
  are theirs, not yours — confirm before claiming a regression, and do not "fix" them.
- Stage files **explicitly**. Never `git add -A`. That mistake committed another session's
  half-finished file earlier today and broke HEAD's test build.

### Scope

Gates 0–2 only. Do **not**:
- re-enable the ui2 idle gating,
- touch the bevel kernel, the knife, or the boolean,
- start the Smooth-by-Angle modifier (that is a separate, already-scoped task).
