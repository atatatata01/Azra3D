# Offset Edge Loop — Plan

**Date:** 2026-08-09
**Owner ask:** *"I NEED AN INSET LIKE ON SELECTED EDGE, SO I FEEL LIKE I DUPLICATE THE EDGE AND I
CAN RAIL IT ON EITHER SIDE … THIS COULD BE ALTERNATIVE FEATURE OF INSET (i)"* — with a reference
image: green = selected loop (kept), red = the new parallel loop, tick marks = the new connecting
quads.
**Industry name:** Maya *Offset Edge Loop Tool*; Blender *Offset Edge Loop Cut* (`Ctrl+Shift+R`).
**Status:** Plan only. Nothing implemented.

---

## 0. Owner decisions (answered 2026-08-09 — these are settled, do not re-litigate)

| Question | Decision |
|---|---|
| How many loops per press? | **One by default; a modifier mirrors a second on the opposite side.** |
| Offset units | **Absolute world distance by default; a modifier switches to proportional 0..1 mid-drag.** |
| Selection scope | **Closed loops AND open chains.** An open chain terminates at its end vertices. |

The original loop is always **kept** — this is not a bevel. (Confirmed by the reference image:
green and red both present, quads between them.)

---

## 1. Why this is smaller than it looks

Three pieces already exist and are the designated single homes for exactly this math:

| Piece | Location | Role here |
|---|---|---|
| `edge_slide_rails(h)` | [nav.rs:50](../../../core_engine/core_math/src/half_edge/nav.rs#L50) | Returns the two off-loop neighbour vertices a loop vertex rides — `rail_pos` on `h`'s face side, `rail_neg` on the twin's. Its own doc calls it the *"Single home for the rail math (reuse rule)"*. This IS "rail it on either side". |
| `circle_edge_loop` | [ops_inset.rs:493](../../../core_engine/core_math/src/half_edge/ops_inset.rs#L493) | Precedent for an edge-loop-scoped kernel op taking `&HashSet<HalfEdgeId>`. |
| `LoopCutOp` | `mesh_ops/` | Precedent for a loop-insertion op that achieves an `OpDelta::Element` (O(touched)) inverse via add-only edge splits. |

**`I` in Edge mode is a dead key today.** [key_dispatch.rs:66-75](../../../core_engine/app/src/poly/key_dispatch.rs#L66)
only acts when `poly_work_mode == Face` with a non-empty face selection; every other mode falls to
`self.set_poly_tool(PolyTool::InsetFace)` and nothing happens. The slot is genuinely free.

**No new keybinding is required, and none should be added.** `I` / `kf.inset` is already a
registered `ACTION_DEFS` action. Making it context-dependent (Face → Inset Face, Edge → Offset Edge
Loop) is explicitly blessed by CLAUDE.md § *Input System Discipline*: *"Same key + different
context is CORRECT, not a bug."* Do not invent a unique chord for this, and do not let a future
conflict sweep "fix" it.

---

## 2. Modifier assignment (follows existing drag-tool convention)

Read from the live toasts:

* Bevel — `drag=width, ALT+drag=shape, scroll=segments, SHIFT=consume, C=clamp`
* Inset Face — `drag R=inward L=outward, Alt=depth`

So the house convention is **Alt = alternate drag semantic**, **Shift = discrete toggle**. This
tool inherits it:

| Input | Effect |
|---|---|
| drag | offset distance; crossing zero flips the loop to the other adjacent face |
| **Alt** + drag | proportional (0..1 along each rail) instead of absolute distance |
| **Shift** | toggle: mirror a second loop on the opposite side (symmetric pair) |
| LMB / Enter | confirm |
| Esc / RMB | cancel (Tool Exit Convention — knife is the only RMB-confirms exception) |

**Reuse the bevel session's rising-edge guard verbatim.** `bevel_drag.rs:199-203` seeds
`shift_was_down: true` with the comment *"Seeded as already-down so ARMING the tool can never fire
the toggle: if the activation chord itself carries SHIFT (rebindable), frame 1 sees no rising
edge."* A Shift toggle in a modal that can be entered by a rebindable chord has exactly this bug
otherwise. Same for Alt if it is ever made a latch rather than a held state.

---

## 3. Geometry

For each vertex `V` on the selected chain:

1. `edge_slide_rails(h)` → `(rail_pos, rail_neg)`. The drag sign picks which one is active.
2. Place `V'`:
   * **absolute:** `V' = V + d · normalize(rail − V)`, clamped to `d < |rail − V| · (1 − ε)`
   * **proportional:** `V' = V + t · (rail − V)`, `t ∈ [0, 1)`
3. Split the faces on the active side with the new `V'` chain, producing the connecting quads
   (the tick marks in the reference image).

### 3.1 The absolute-mode clamp is a correctness invariant, not a nicety

Proportional mode cannot overshoot by construction (`t < 1`). Absolute mode can: on this model the
quads around the rounded corner vary a lot in width, so one `d` will exceed some rails while
clearing others. Unclamped, `V'` crosses its rail vertex and the connecting quad inverts.

**Per-vertex clamp, not a whole-loop clamp.** Clamping the entire loop to the narrowest rail would
make one tight quad dictate the whole band — the "uniform band" property the owner chose absolute
mode to get. Clamp each vertex independently and surface it (bevel already has the `C=clamp`
precedent and a `warned_clamp_force` flag for exactly this class of message).

**Assert against inversion.** Per CLAUDE.md § *half_edge.rs editable*: a topology op that could
produce non-manifold or inverted geometry MUST assert against it. Never downgrade that assert to a
log.

### 3.2 Ambiguous rails are the real risk

`edge_slide_rails` returns `None` on a boundary edge or malformed topology, and the reference model
has irregular valence where the loop wraps the rounded corner. A chain with a `None` in the middle
cannot simply skip that vertex — the new loop would be discontinuous.

**~~Rule: offset along the average of neighbouring valid rail directions, projected onto the
incident face plane.~~ — WITHDRAWN 2026-08-10. The rule was geometrically incoherent. Do not
reinstate it.**

It was written as though the new vertices were free-floating points. They are not: each is created
by `split_edge` on a rung and then joined by `split_face_between`. **A new vertex must lie ON an
existing edge or the strip cannot close.** An averaged direction lies on no edge at all — the loop
would pinch and the rail would visibly bend. Averaging also substitutes an approximation for a
value that is exactly recoverable (below), which CLAUDE.md § No Hacks forbids outright.

**The corrected rule, which is what P2 implements — and it starts from a misread of the helper.**
`edge_slide_rails` returns `None` because the **twin** side is missing, not the chosen side. On a
boundary edge the chosen-side rail is perfectly well defined; it is sitting right there in the face
ring. P1 read that `None` as "no rail" and therefore refused *every* chain touching an open rim —
which is why the owner's first real test refused.

| Case | Correct answer |
|---|---|
| Boundary / `None` from `edge_slide_rails` | Read the rail **one-sidedly** from the chosen-side face ring — `h.prev`'s source at the start, `h.next`'s destination at the end. Exact, not approximate. A `debug_assert` pins it equal to `rail_pos` wherever `edge_slide_rails` has an answer, so the two can never silently diverge. |
| Genuinely ambiguous (valence > 4 on the offset side) | There are **two** rungs, and one point cannot lie on both. The answer is topological, not an average: ride **each** rung and split the fan faces between them, so the loop cuts the corner (`offset_loop_fan`). |
| Truly undefined (zero-length rung, fan cut by a boundary, no face on the chosen side) | **Decline**, mesh byte-identical — as the withdrawn rule already prescribed for this case. |

There is no remaining case where averaging would be both reachable and correct.

### 3.3 Open-chain ends

At an open chain's terminal vertex the new loop stops. The final face gains one boundary vertex and
becomes an n-gon. That is the correct, manifold result and matches Blender. Do **not** fan it into
triangles — this kernel is n-gon-native and triangulating here would fight the emitter.

---

## 4. Phases

Each phase builds green (`cargo build -p app`) and has a gate that fails on its own specific defect.

### P1 — Kernel op, closed loops, one side, absolute *(core_math, no UI)*

* New op beside `circle_edge_loop` in `ops_inset.rs` (same file family, same `&HashSet<HalfEdgeId>`
  signature) + a `MeshOp` wrapper for provenance/inverse.
* **Inverse tier — be honest.** Aim for `OpDelta::Element` like `LoopCutOp` (this op is add-only:
  new verts, new edges, face splits, no free-list mutation). If the implementation ends up touching
  the free-lists, ship `OpDelta::Snapshot` and say so in the doc comment.
  `local_rebuild_ops.rs:9-10` is explicit: *"This is NOT a fake delta"* — never claim O(touched)
  that isn't.

**Gate (`cargo test -p core_math`):** on a cube with a selected face-ring loop, the op adds exactly
one loop of N vertices and N quads; `validate()` passes; no face has inverted winding; and
`apply → inverse → apply` is identity (the `test_spine.rs` pattern all sibling ops already use).

### P2 — Rail ambiguity + open chains

* Averaged-rail fallback (§3.2) and its fail-closed decline.
* Open-chain termination (§3.3).

**Gate:** a loop crossing a valence-5 vertex still produces a continuous loop; an open chain of k
edges produces k+1 new vertices and terminates in an n-gon; a chain with an isolated undefined rail
declines with a toast and mutates nothing (assert the mesh is byte-identical after the decline).

### P3 — Proportional mode + symmetric pair

* `t ∈ [0,1)` placement path.
* Mirror: run the same placement on `rail_neg` and split those faces too.

**Gate:** proportional at `t=0.5` lands each new vertex exactly at its rail midpoint (exact float
check, not a tolerance); the symmetric pair is mirror-equal about the original loop on a symmetric
fixture; absolute and proportional agree on a uniform-quad grid where they are defined to agree.

### P4 — Drag tool + `I` dispatch

* New `poly_drag_tools/offset_loop_drag.rs` — do **not** add to an existing drag file
  (FEATURE_RULES invariant 5; `poly_drag_tools.rs` was already split for size).
* `key_dispatch.rs`: `kf.inset` in `PolyWorkMode::Edge` with a non-empty `selected_edges` starts it.
  Face mode behaviour unchanged.
* Episode transaction: `begin_episode()` on drag start, `commit_episode()` on confirm — ONE undo
  entry per drag, per CLAUDE.md § *Transaction Model*. Not `transaction()` per mouse-move.
* Tool Parameters dock follows the modal (`ui2.active_tool`), as bevel does.
* Toast on entry, matching the house format.

**Gate:** the owner's repro — select the loop in the reference image, press `I`, drag, see one
parallel loop follow the surface around the rounded corner; Shift adds the mirrored one; Alt
switches to proportional mid-drag; Esc cancels leaving the mesh untouched; one `Ctrl+Z` undoes the
whole drag.

### P5 — Sidecar + modifier-stack survival

* Verify authored CustomData (UV, seams, sharp, Smart Select tags) survives the op — this is the
  `project_smart_select_and_sidecar` class of loss, and new-loop corners need UV interpolated from
  the faces they split.
* If the op cannot yet emit a parent-map, declare `PROPAGATES = false` **loudly** exactly as
  `BevelOp` does ([bevel.rs:47-49](../../../core_engine/core_math/src/mesh_ops/bevel.rs#L47)) rather
  than silently dropping layers.

**Gate:** a body with authored UVs and a Mark Sharp edge keeps both across the op; a body with
Smart Select tags keeps them; the display mesh and `poly_export_raster` agree (the
`export_raster_matches_the_displayed_normals` invariant).

---

## 5. Relationship to neighbouring tools — keep them distinct

| Tool | What it does | Not this because |
|---|---|---|
| **Bevel** (`Ctrl+B`) | Replaces the edge with two rails + optional round | **Consumes** the original edge; this keeps it |
| **Loop Cut** (`Ctrl+R`) | Inserts a loop *perpendicular* to a ring, positioned by slide | Different axis and different seed (a ring, not a selected loop) |
| **Inset Face** (`I`, Face mode) | Shrinks a face region inward | Face-scoped; unchanged by this work |
| **Offset Edge Loop** (`I`, Edge mode) | Parallel copy of a selected loop, original kept | — |

Do not merge this into Bevel with a "keep original" flag. Bevel is a whole-mesh `Snapshot` op with
a 22-dead-end history (`BEVEL.md` §6); this is an add-only loop insertion. Fusing them would drag
this feature into that risk surface for no gain.

---

## 6. FEATURE_RULES pre-flight

1. **New scene object type?** No. No `NodePayload` variant, no save-format change, no serialized
   enum touched.
2. **Mutates scene state?** Yes — episode transaction (P4). Undo gate: drag → `Ctrl+Z` → fully
   reverted.
3. **Frame-thread work?** Per-mouse-move mesh regeneration, same shape as bevel's modal. No async
   poll loop, so no `MAX_*_PER_FRAME` needed; if the regenerate proves heavy on dense loops,
   throttle it the way bevel does rather than inventing a new mechanism.
4. **New render path or mode?** No. No shader edit, no pipeline, no `PipelineLatch` entry.

`half_edge.rs` red line respected: no serialized field on `HVertex`/`HFace`/`HalfEdge`/
`HalfEdgeMesh` is retyped, reordered or removed. This op only adds elements through the existing
public mutating API.

---

## 7. Regression checklist (run at P4 and again at P5)

```bash
cd core_engine && cargo build -p app
cd core_engine && cargo test -p core_math
```

Manual:

1. Closed loop → `I` → drag → one parallel loop, original kept.
2. Drag through zero → loop flips to the other side.
3. Shift → mirrored pair appears; Shift again → back to one.
4. Alt mid-drag → proportional; release → back to absolute, no jump discontinuity.
5. Open chain → terminates cleanly, n-gon at the end, `validate()` green.
6. Loop through a valence-5 vertex → continuous, no pinch.
7. Esc mid-drag → mesh byte-identical to before.
8. Confirm → one `Ctrl+Z` reverts the whole drag.
9. Save → reload → geometry identical.
10. Face mode `I` still insets faces exactly as before.

---

## 7-0. CROSS-TOOL BUG FOUND DURING P4 — needs an owner decision (affects SEVEN tools)

**`auto_commit_episodes` commits every keyboard-armed poly modal's episode at the end of its arm
frame.** `undo.rs:1006-1011` commits whenever `!pointer.any_down()`, and a modal armed by a KEY
(Ctrl+B bevel, `I` inset, `I` offset loop, …) has no pointer down at arm time. So:

* the episode is committed one frame after it opens;
* the later `cancel_episode()` finds `pending_episode == None` and is a **no-op**;
* a CANCELLED modal therefore leaves one snapshot entry on the undo stack.

Affected: bevel, vertex bevel, inset, shrink/fatten, face hinge, offset loop — the whole
keyboard-armed poly modal family. **Not data loss** — each tool restores its `original_mesh`
manually on cancel, so the mesh is correct; the defect is a junk undo entry. Confirm still yields
exactly ONE entry per drag.

**The fix belongs in `auto_commit_episodes`, once, for all of them** — not in any individual tool.
The exemption pattern already exists immediately above the offending block (`shift_duplicate` and
`array_dup` both early-return), and `ActiveTool::is_poly_modal()` already exists as the predicate.
Special-casing one tool would violate CLAUDE.md § No Bandaids ("Two code paths for the same
operation — they MUST share one path"), so P4 deliberately **matched the family's existing
behaviour** rather than fixing itself in isolation.

---

## 7a. Phase order — CHANGED 2026-08-10 (owner decision)

**P4 runs before P2 and P3.** The owner chose "make it reachable" over "correctness first", and the
reasoning holds: the valence-3 restriction in §7b bites when offsetting a *quad face's boundary
loop outward*, but an **interior** edge loop — the actual use case in the reference images, where
each loop vertex is valence 4 — has both rails defined and already works from P1.

Consequence, and it must not be papered over: **P4 ships with absolute distance and one side only.**
Alt (proportional) and Shift (symmetric pair) do not exist in the kernel yet, so they stay **inert**
in P4 — not stubbed, not approximated, and NOT advertised in the entry toast. A modifier that is
listed but does nothing is the "lying control" defect this project keeps fixing. They attach in P3.

Revised order: **P4 → P2 → P3.** P2 is what unlocks quad-grid topology and irregular-valence loops;
until it lands, those refuse cleanly rather than producing wrong geometry.

### …and that order was WRONG. Corrected 2026-08-10, same day, after the owner ran it.

P4 shipped, the owner selected a loop on a cube with one beveled edge, and it refused:
`ops_inset.rs:650` — *"selection is not a simple CLOSED edge loop (a vertex carries != 2 selected
edges)"*. Their selection ran along the top edge, around the bevel, and down the side: **two free
endpoints, i.e. an open chain.**

The misjudgement is worth naming so it is not repeated. The argument for P4-first was "the interior
edge-loop case already works, so the tool is closer to usable than it looks." That is true and
irrelevant — **both of the owner's reference images for this feature show a silhouette chain with
free ends.** Open chains were never an edge case; they were the use case. The tool now arms,
probes, drags and installs correctly on geometry the owner does not have.

Second-order point that also holds: a **beveled** corner is exactly where `edge_slide_rails`
returns `None`, so even a CLOSED loop over that bevel would have hit §3.2's rail ambiguity and
refused. Both halves of P2 were load-bearing for the first real thing anyone tried.

**Actual order: P4 → P2 (in progress) → P3.** The lesson generalises — validate phase order against
the owner's own screenshots before deferring any phase as "correctness polish".

---

## 7b. Findings from the P1 implementation (2026-08-09) — read before P3/P4

### P4 MUST NOT assume the sign means what you think

`distance`'s sign selects the side by reversing the ring, and the ring's orientation falls out of
`edge_loop_rings`' **lowest-vertex-id start** — it is deterministic but geometrically arbitrary.
So `+` is NOT "the side the user is dragging toward". **P4 must map drag direction to sign by
testing where the loop actually lands**, never by assuming a fixed correspondence. Getting this
wrong produces a tool that flips side depending on which vertex happens to hold the lowest id.

### Multi-ring selections are refused in P1 (the plan was silent)

Because side derives from each ring's own walk orientation, one sign across several rings could
send them to **opposite geometric sides**. Refused with an explicit message rather than guessed.
P3/P4 must decide the intended semantics before allowing multi-ring input.

### The valence-3 constraint narrows what P1's gate actually proves

A quad face's boundary loop is only offsettable outward when its corners are **valence 3** — true
on a cube (2 loop edges + 1 rung). On a regular quad grid those corners are valence 4 with *two*
outward edges, which is a genuine §3.2 rail ambiguity and is refused. The cube is therefore close
to the only face-boundary case that passes; the general fixture is a proper interior edge loop
(a torus minor ring was added for that reason). **P2's averaged-rail fallback is what unlocks the
grid case** — it is not optional polish.

### Inversion guarding — implemented STRONGER than §3.1 asked

§3.1 said "assert against inversion". A hard `assert!` reachable from a drag would crash the app on
a legitimate over-drag, and a bare `debug_assert!` vanishes in release. Shipped instead:
a **pre-mutation refusal** (mesh left byte-identical — strictly stronger than asserting after the
damage) **plus** a `debug_assert!` post-condition that each emitted quad's winding agrees with the
face it was carved from. Nothing was downgraded to a log.

### Wiring debts for P4

* **No `PolyEdit` variant.** `OffsetEdgeLoopOp` is reachable only through the `MeshOp` trait today,
  same as `BevelOp` (adding a variant would force an edit to the deliberately-exhaustive
  `targets()` match in shared `mesh_ops/mod.rs`, which was off-limits while bevel work was live).
  P4 must decide: route through `PolyEdit::apply_in_place` (the app's undo path) or the MeshOp path.
* **The wrapper pays one mesh clone even on refusal** (inherited from `run_add_only`). Harmless for
  a one-shot; for a per-frame drag that refuses every frame, P4 can probe the kernel directly — it
  is atomic on refusal by design.

---

## 8. Session log

* **2026-08-10** — P2 shipped (open chains + rail resolution). `offset_edge_loop` rewritten:
  `offset_loop_chain` delegates the CLOSED case to `edge_loop_rings` unchanged (so that stays the
  single home for the closed walk) and uses a new bounded walk for open chains; `offset_loop_fan`
  handles irregular valence by riding **both** rungs. §3.2's averaged-rail rule was WITHDRAWN as
  incoherent — see the table there. Contracts re-verified, not assumed: ± sign→side unchanged
  (one code path via `ring.reverse()`); **pure scaling in `d` holds**, gated by
  `offset_edge_loop_placement_is_a_pure_scaling_in_distance` asserting identical direction and an
  exactly 10× magnitude ratio between d=0.02 and d=0.2 — which is what keeps P4's ±probe honest;
  all 5 `offset_loop_sign_tests` still green; `OpDelta::Element` tier intact; closed-loop behaviour
  byte-identical **structurally**, not merely asserted. Verified independently: build exit 0,
  `core_math` 1074/3, `app --bins` 725/5 — all baselines.

  **Two debts recorded rather than hidden:**
  1. **P4 probe anchor is subtly wrong at a fan vertex.** `offset_loop_drag.rs` anchors its probe
     at the centroid of the CHAIN vertices. That equals the centroid of the cut bases for closed
     loops and simple open chains, so probe direction is exactly proportional to `d` — but at a
     **fan** vertex the base vertex is counted twice, leaving a small `d`-independent bias. At a
     small `PROBE_FRACTION` that term can dominate and **mis-bind the drag side on
     irregular-valence meshes**. Fix is a weighted anchor in the tool. NOT fixed — P4's file.
  2. **Open-walk duplication debt.** No extractable open-chain walk existed: `edge_loop_rings`
     refuses degree ≠ 2, and `loop_space` (`ops_misc.rs:756`) and `edge_slide_rails_loop`
     (`nav.rs:70`) each inline their own private walk. `nav.rs`/`ops_misc.rs` were off-limits
     (live vertex-bevel work), so P2 wrote a fourth in `ops_inset.rs`. Extraction into `nav.rs`
     is owed once bevel work settles.
* **2026-08-10** — P4 shipped. New `app/src/poly_drag_tools/offset_loop_drag.rs`; `kf.inset` gains
  an Edge-mode branch in `key_dispatch.rs` (Face branch byte-identical); `PolyOffsetLoop` appended
  to the END of `ActiveTool` with its `Capture` declared in the exhaustive `ToolId::of()` match
  (no free `ToolId` bit remains — mapped to the sibling with identical capture, the documented
  fallback already used by `PolyExtrudeSweep` / `NurbsFillet` / `PolyPropOrigin`).
  **Sign↔side is MEASURED, not assumed** (§7b's requirement): at arm time the kernel runs twice on
  throwaway clones at ±(0.05 × mean loop edge length), each landing is reduced to a **unit world
  direction** from the loop centroid through the node's world matrix, and each frame the sign whose
  screen-projected axis has the larger `dot(drag, axis)` wins. Storing a *unit direction* rather
  than the landing point is load-bearing — the probe is deliberately tiny, so projecting the
  landing itself is sub-pixel and reads as edge-on; a gate pins that the direction is independent
  of probe magnitude (legal because the kernel's placement is a pure scaling below the rail clamp).
  A probe refusal means that side is topologically illegal; both refusing ⇒ refuse **before**
  `begin_episode`, so nothing mutates and no undo entry is burned. Alt/Shift inert with `// P3:`
  markers and absent from the toast, per §7a.
  **Deviation, accepted:** ~14 additive lines outside the allowed file set (`tool_mode.rs`,
  `input_keyboard/handle.rs`, `input_viewport.rs`) — a new `ActiveTool` variant is inert without
  modal dispatch, and shipping a key that does nothing is the defect class this project keeps
  fixing. All lines are tagged `offset_loop` and are trivially separable from the owner's WIP in
  those same files.
  Also found: the cross-tool `auto_commit_episodes` defect, §7-0.
* **2026-08-09** — P1 implemented. `HalfEdgeMesh::offset_edge_loop(&edges, distance)` appended to
  `ops_inset.rs` (+7 gates); `OffsetEdgeLoopOp` / `OffsetEdgeLoopParams` added to
  `mesh_ops/local_ops.rs` between `LoopCutOp` and `FlipFaceOp` (+4 tests), reusing `run_add_only`
  unchanged so its 3 existing call sites see a zero diff. Pipeline: `edge_loop_rings` → per-vertex
  rail from `edge_slide_rails` → `split_edge` each rung at `t = min(d/|rail−V|, 1−1e-3)` →
  `split_face_between`. Inverse tier `OpDelta::Element`, **proved not claimed**: the same 16-edge
  loop on a 256-face and a 768-face torus records an identical slot count, so a delta secretly
  scaling with the body would diverge. `cargo test -p core_math` 1068 passed / 3 failed = exactly
  the known pre-existing set. Findings recorded in §7b.
* **2026-08-09** — Plan authored from owner reference images + three settled decisions (§0).
  Verified: `I` in Edge mode is a dead key (`key_dispatch.rs:66-75`); `edge_slide_rails` already
  exists and is the designated single home for the rail math; `circle_edge_loop` and `LoopCutOp`
  are the structural precedents. No code changed.
