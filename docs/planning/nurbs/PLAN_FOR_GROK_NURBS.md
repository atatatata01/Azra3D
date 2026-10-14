# Plan for Grok — NURBS modeling, Plasticity-style

**Date:** 2026-07-30 · **Worktree:** `.claude/worktrees/nurbs-modeling` (`feat/nurbs-modeling`)

---

# ⛔ THE RULES — read before EVERY task. No exceptions.

### Interaction (owner has corrected this THREE times — grab, then G/R/S, then bevel)

**R1. Muscle memory is a product requirement, not a nicety.** The user must never learn a second set
of habits for NURBS. *"I understand this is not the same code, but muscle memory is important."*

**R2. Before writing ANY tool interaction, grep the poly equivalent and COPY ITS STRUCT.** New math is
allowed. New feel is not. Report what you found, as the reuse rule requires. → §4b

**R3. G / R / S set `gizmo_mode` + `gizmo_armed = true`. Nothing else.** They arm a *visible gizmo*.
`*_start_transform()` is what the gizmo delegates into on handle-grab — **never** what a key launches.
Calling it from a key is what the owner calls "the Blender version." → §4b, commit `93d3b8f`

**R4. Never create an `ACT_NURBS_*` for something a GLOBAL action already does.** Poly has no
`ACT_POLY_GRAB` — it uses global G/R/S. `ACT_NURBS_GRAB_CV` was the bug: **delete it, don't replace
it.** New pillar actions are justified only with no global counterpart (knot insert, weight = fine).

**R5. G/R/S must work at BOTH levels** — whole solid *and* face/edge/vertex — through the same path.
Six cells, one code path. → §4b

**R6. One operation, one code path.** A keyboard shortcut and its menu/command entry must call the
*same* function. `AppCommand::NurbsGrabCv` doing something different from the G key is hard-banned.

**R7. Drag tools capture `start_mouse` and start the value at ZERO**, then regenerate from the
**pre-op state every frame** — never from last frame's output, or the op compounds on itself and the
drag stops being reversible. Scroll = segments · number = exact value · C = clamp. → §4b

**R8. Fail VISIBLY.** A missing precondition gets a toast, never a silent no-op. A tool that does
nothing when you press its key reads as broken.

### Geometry & data

**R9. NO HISTORY. Faces remember instead.** `NurbsFace.origin: FaceOrigin` keeps a fillet editable and
makes it follow its neighbours. `origin` is authored state and MUST serialize. → §2b

**R10. Object-level scale writes the NODE TRANSFORM, never the control points.** Baking scale into CVs
makes `FaceOrigin`'s radius a lie, and non-uniform scale turns a fillet into an ellipse. → §4b

**R11. Every NURBS op ships an invariant test.** Analytical ground truth is exact and dependency-free
— use it first. **No test → not done.** Sabotage-check each. → §0

**R11b. For anything involving TOPOLOGY, check against the OCCT oracle** — face count, face types,
trim loops. Invariants verify surfaces you already decided to build; they are blind to a *missing*
one. Both fillet failures were decomposition errors that no invariant could catch. Oracle ≠
dependency: generate offline, commit the JSON, exactly like the Blender poly refs. → §0-pre

**R12. Stable IDs, never Vec indices.** A fillet inserts faces; indices go stale immediately.

**R13. Append-only on serialized enums.** `NurbsSolid` goes AFTER `SdfObject`. Never insert or
reorder. **Do not block on `master`** — it is dormant. → §2c

### Process

**R14. `cargo build -p app` after every change; `cargo test -p core_math` after any `nurbs.rs` change.**

**R15. One item per commit, stating what was *verified*** — not what was written.

**R16. Log every dead end in §7.** With no oracle, the dead-end log is the primary defence.

---

## EXECUTION STATUS (Grok, 2026-08-01 STATE RESET)

**Owner in-app:** fillet is **NOT done at tool level**. Kernel stays; **integration layer is the work**.
`cargo test -p core_math` alone never marks a NURBS tool milestone done — need **AZRA_NURBSDIFF** harness shots.

| Phase | Kernel | Tool / app-path | Notes |
|---|---|---|---|
| N0 cube normals | **done** | **done** | outward normals green |
| N0 invariant harness | **done** | n/a | `core_math/tests/nurbs_invariants.rs` — kernel only |
| N0-pre OCCT oracle | **done** | n/a | FreeCAD JSON refs + Tier1/Tier2 tests |
| N1 NurbsSolid | **done** (cube) | **partial** | cube = one `NurbsSolid`; cylinder/sphere/torus still legacy `NurbsBody` |
| N2 modeless pick | n/a | **partial** | face-index pick exists; **must re-key on `NurbsFaceId`** (indices go stale after fillet) |
| N3 G/R/S solid | n/a | **check R3/R4/R6** | global G/R/S path — verify, do not reintroduce pillar grab action |
| N4 fillet kernel | **landed** (many tiers) | **NOT done** | see `FILLET_EDGE_CASES.md` kernel-vs-tool columns |
| N4 fillet tool path | n/a | **broken in owner test** | sequential junction gap; selection float; no anytime edit/delete parity |
| **P0 AZRA_NURBSDIFF** | n/a | **done** | harness + ref shots under `nurbsdiff_refs/` |
| **P1 solid primitives** | wall/caps + rim | **done** | cylinder/sphere/torus → `NurbsSolid`; rim shot 04 |
| **P2 stable-id selection** | `NurbsFaceId` | **done** | selection re-keyed; prune after regenerate |
| **P3 junction sequential** | batch 0.5 ✅ | **done** | un-shrink re-batch; sequential=OK |
| **P4 edit/delete anytime** | remove/adjust one | **done** | properties panel on Fillet face |
| **Save-format** | **RESOLVED** | n/a | `NurbsSolid` tag 12 append-only; master dormant (R13) |

**Owner-decided (2026-07-30).** Four design answers settled — do not re-litigate (§1).

---

## 0-pre. ⭐ THERE *IS* AN ORACLE — OCCT (owner, 2026-07-30)

**This supersedes the "no oracle" framing below.** §0's invariants stay — they are still the fast,
dependency-free first line — but they are **not sufficient**, and two failed fillet attempts proved it.

### Oracle ≠ dependency

**Blender was never a dependency of AZRA.** Headless scripts (`docs/planning/poly/*_ref.py`) produced
reference vertex data offline, the data was committed as JSON, and tests compared against it. Nothing
about Blender shipped.

**Do exactly that with Open Cascade (OCCT).** A headless harness (pythonOCC, or a small C++ shim)
generates ground truth offline → commit the JSON → AZRA's tests compare. **No FFI, no runtime
dependency, no licensing exposure, no build-time cost.**

### Why this is the missing piece, specifically

The §0 invariants verify a surface **once you have decided which surfaces to build**. They say nothing
about **what the correct decomposition is** — and that is exactly where the fillet has failed twice:

| Question the invariants cannot answer | Symptom when guessed wrong |
|---|---|
| How many faces does a filleted cube have? | — |
| Where do the trim loops sit in (u,v)? | chord-vs-arc dark wedge (screenshot 2) |
| What is the corner when only **2 of 3** edges are filleted? | open notch (screenshot 1) |
| Unequal radii / non-orthogonal edges? | unknown |

A missing corner patch violates **no** invariant, because there is no surface to evaluate. That blind
spot is why "no test → not done" (R11) was necessary but insufficient.

### How to use it

1. Headless OCCT script: make a box, fillet edge(s) at radius *r*, dump the resulting B-rep —
   **face count, face types, per-face trim loops, and sampled points per face**.
2. Commit the dump as JSON next to the tests, exactly like the poly `*_ref.json` files.
3. AZRA's test asserts **topology first** (face count + types), *then* geometry (sampled deviation).
   **Topology mismatch is the more valuable failure** — it tells you the decomposition is wrong before
   you waste time polishing surfaces that should not exist.
4. Generate cases across radii **including near-max**, partial-edge selections (1, 2, 3 edges at a
   vertex), and unequal radii.

### What this does NOT decide

**Whether AZRA ever links OCCT as a kernel is a separate, later question.** Prior evaluation
(`reference_truck_cad_kernel_eval`) found the pure-Rust `truck` kernel architecture-fit GREEN but
booleans/STEP weak, and recommended keeping an "OCCT-via-FFI escape hatch." That decision belongs with
**booleans + STEP** (§6), not with fillets. Use OCCT as an oracle now; decide the dependency when
booleans force it.

---

## 0. Analytical invariants — still required, still first

The poly pillar was built against a headless-Blender oracle: run the op, diff vertex positions, done.
**For NURBS that method is unavailable — and the replacement is stronger, not weaker.**

**NURBS geometry has exact closed-form ground truth.** A fillet is not "whatever Blender produced"; it
is a surface every point of which lies at *exactly* radius `r` from a known axis. That is a machine-
checkable invariant with no external dependency, and it is a *better* gate than an oracle diff because
it states the property rather than a sample of one implementation's output.

**Every NURBS task in this plan ships with at least one invariant test from this table:**

| Invariant | Assertion | Catches |
|---|---|---|
| **Partition of unity** | `Σ Nᵢ,ₚ(u) == 1.0 ± 1e-6` for 100 random `u` at every degree used | knot-vector and basis bugs — the single best cheap self-check |
| **Planarity** | every `surface.point(u,v)` of a flat face is coplanar to 1e-6 | wrong corner order, non-planar quads |
| **Outward normal** | `normal(0.5,0.5) · (centre → face_centre) > 0` for all faces of a closed solid | the 4 inverted cube faces (§2, live bug) |
| **Exact circle** | a revolved/filleted arc satisfies `|P(u) − axis| == r ± 1e-6` | wrong rational weights — a "circle" that is really a parabola |
| **G1 tangency** | at a fillet seam, `fillet_normal · face_normal ≥ 1 − 1e-6` | the whole point of a fillet; a G0 fillet reads as a crease |
| **Refinement is a no-op** | knot insertion / degree elevation must not move the surface: sample 100 `(u,v)` before and after, compare to 1e-6 | the classic NURBS bug class |
| **Interpolation at corners** | `point(0,0) == cp[0][0]` (clamped knots) | knot clamping errors |

**Rule: a NURBS op with no invariant test is not done.** These are cheap — most are five lines — and
they are the only thing standing between us and silently wrong geometry, because unlike a poly mesh
you cannot eyeball a wrong NURBS surface. It looks plausible right up until you fillet it.

**Sabotage-check every one**: flip a sign, confirm the test FAILS, restore. A test that passes on
broken input is worse than no test — three AZRA features have now shipped whose gate passed *because*
the output was broken.

---

## 1. Settled design (owner, 2026-07-30)

| # | Decision | Consequence |
|---|---|---|
| 1 | **One node holds N faces** — a `NurbsSolid`, mirroring `PolygonalBody` | Outliner shows **one** "NURBS.Cube". Faces/edges/vertices are sub-elements. |
| 2 | **Modeless selection, with 1/2/3 as an optional filter** | Hover highlights whatever is under the cursor; 1/2/3 lock the filter only when picking is ambiguous. No Tab required. |
| 3 | **Highlight: "select face → highlight face, select edge → highlight edge. No weird NURBS wireframe. Clean simple."** | Element-scoped highlight only. CV grids, hulls and isocurves are **off by default**. |
| 4 | **Ctrl+B = real rolling-ball fillet, G1 tangent** | Genuine kernel work. Sequenced last, after the foundation it depends on. |
| 5 | **Direct modeling with editable face attribution — NOT a Fusion-style history tree.** Owner, 2026-07-30: *"Plasticity doesn't have a history like Fusion 360, but everything can be changed — bevel, rotate face keeps bevel."* | **Changes N1.** Faces carry a `FaceOrigin` tag and are re-solved locally. No replay, no timeline. See §2b. |
| 6 | **`master` is dormant** — owner: *"the old master is no longer used too much, maybe refer to it for features that are lost but not active work"* | Save-format lockstep for `NurbsSolid` is **relaxed** — do not block on landing the variant on `master`. See §2c for why this is safe and what residual risk remains. |

**Philosophy:** a NURBS modeler that *feels like poly modeling*. Same shortcuts, same gizmo, same
action centre, same undo. The NURBS-ness is an implementation detail the user should never have to
think about — they select a face and press G.

---

## 2. Live bug — fix before anything else

**Four of the six cube faces have inverted normals.** Verified against
`nurbs.rs:217` (`control_points[row_u][col_v]`) and `nurbs.rs:271` (`normal() = du.cross(dv)`),
so for `new_quad(p00,p01,p10,p11)`: `dU = p10−p00`, `dV = p01−p00`, `n = dU×dV`.

| Face | n = dU×dV | Should be | |
|---|---|---|---|
| Top | −Y | +Y | ❌ |
| Bottom | +Y | −Y | ❌ |
| Right | +X | +X | ✅ |
| Left | −X | −X | ✅ |
| Front | −Z | +Z | ❌ |
| Back | +Z | −Z | ❌ |

Left/Right point outward, the other four point inward — so they disagree with **each other**. This is
wrong under either global convention; it is not a sign choice.

**Fix:** `new_quad`'s 2nd and 3rd args are `p01` and `p10`; swapping them exchanges dU/dV and negates
the cross product. Swap them on the four bad faces:

| Face | p00 | p01 | p10 | p11 |
|---|---|---|---|---|
| **Top** | `(-hx, hy,-hz)` | `( hx, hy,-hz)` | `(-hx, hy, hz)` | `( hx, hy, hz)` |
| **Bottom** | `(-hx,-hy, hz)` | `( hx,-hy, hz)` | `(-hx,-hy,-hz)` | `( hx,-hy,-hz)` |
| **Front** | `(-hx,-hy, hz)` | `(-hx, hy, hz)` | `( hx,-hy, hz)` | `( hx, hy, hz)` |
| **Back** | `( hx,-hy,-hz)` | `( hx, hy,-hz)` | `(-hx,-hy,-hz)` | `(-hx, hy,-hz)` |

Each verified: Top→+Y, Bottom→−Y, Front→+Z, Back→−Z, and `p11 == p10 + (p01−p00)` holds for all four,
so the quads stay planar. **Gate: the outward-normal invariant on all six faces.** Nothing would have
caught this — `new_box_faces` currently has exactly two references in the tree, its definition and its
one caller.

---

## 2b. Editable direct modeling — how Plasticity actually works

> Owner, 2026-07-30, correcting an earlier draft of this plan: *"Are you sure Plasticity has history?
> What I remember is that it doesn't have a history like Fusion 360, but everything can be changed —
> bevel, rotate face keep bevel."*

**The owner is right, and this plan previously said the wrong thing.** Plasticity is a **direct
modeler**. There is no feature tree, no timeline to scrub, no replay-from-step-N. What it has is
**faces that remember what they are**: a fillet face knows it is a fillet of radius *r* between two
specific neighbours, so it stays editable and re-solves when those neighbours move.

**This is a materially different — and much safer — architecture than history replay.**

```rust
NurbsSolid {
    name: String,
    faces: Vec<NurbsFace>,      // the CURRENT B-rep — this is the source of truth
    mesh_cache_id: Option<MeshCacheId>,
    material: PbrMaterial,
}

struct NurbsFace {
    id: NurbsFaceId,
    surface: Arc<NurbsSurfaceData>,
    origin: FaceOrigin,         // ← this is what makes it editable
    material: PbrMaterial,
}

enum FaceOrigin {
    Base,                                                  // came from the primitive
    Fillet  { radius: f32, between: [NurbsFaceId; 2] },    // re-solvable
    Chamfer { distance: f32, between: [NurbsFaceId; 2] },
    // extend as tools land — Extrude, Revolve, …
}
```

**Editing is a local re-solve, not a replay:**

* *Change a fillet radius* → find the face whose `origin` is `Fillet`, re-solve it against its two
  `between` faces **as they are right now**, retrim. One face rebuilt.
* *Rotate a base face* → any `Fillet` face listing it in `between` is stale → re-solve just those.
  That is exactly the owner's *"rotate face keeps bevel."*
* Nothing replays. Nothing before the edit is recomputed. There is no step ordering to preserve.

### Why this removes the biggest risk in the plan

The earlier draft warned that persistent topological naming — re-finding "edge E" after an earlier
step changed — is an unsolved problem that every parametric kernel fumbles. **Direct modeling with
face attribution largely sidesteps it.** A `Fillet` face references faces *that exist right now, in
the same solid*. There is no replay across which a reference can rot. If a `between` face is deleted,
the fillet is orphaned — a local, detectable, recoverable condition, not a corrupted history.

That deletes the "ask the owner what happens when a reference breaks" question from §6, and it means
the in-progress N1 shape (`faces: Vec<NurbsFace>`) was **closer to right than the previous draft
claimed**. What it needs is the `origin` tag per face, not a feature list.

### Still required

* **Stable `NurbsFaceId`** — a fillet references neighbours by ID. Vec indices go stale the moment a
  fillet inserts a face. Allocate from a monotonic counter on the solid; never reuse a freed ID.
* **`origin` is authored state and must serialize** with the face. A face that forgets it is a fillet
  is a face the user can no longer edit — which is the whole feature.
* **Orphan handling: fail visibly.** If a fillet's `between` face is gone, mark that face orphaned and
  keep its last good geometry. Never silently re-solve against a different neighbour.
* **Re-solve must be idempotent** — re-solving an unchanged fillet must produce a bit-identical
  surface. Gate it: solve twice, compare to 1e-6. Cheap, and it catches accumulating drift, which is
  the failure mode that makes direct modelers slowly corrupt a shape across many edits.

## 2c. Save format — `master` is dormant, so the lockstep is relaxed

> Owner, 2026-07-30: *"the old master is no longer used too much, maybe refer to it for features that
> are lost but not active work."*

**Do not block N1 on landing `NurbsSolid` on `master`.** The relaxation is safe, and specifically
*because of which direction the change goes*:

* **Appending a variant is backward-compatible for reading.** Old `.fl4` files never contain tag 12,
  so every existing save still loads. The lockstep rule's real teeth are against **inserting or
  reordering** variants — that shifts every later tag and corrupts *every* existing file. This change
  does not do that.
* The only incompatibility is **forward**: a new save containing a `NurbsSolid` cannot be opened by a
  build lacking the variant. With `master` dormant, no user hits that.

**Residual risk, recorded so nobody is surprised later:** if `master` is ever revived for active work,
it must take the `NurbsSolid` variant **before** it writes any save, or the two branches will produce
mutually unreadable files. This is an owner-directed relaxation of a CLAUDE.md non-negotiable — it is
written down here rather than silently dropped, per the Rule Conflict Check.

---

## 3. Phases

### N0 — Fix the cube, add the invariant harness *(half a day)*

1. The four corner swaps above.
2. `core_math/tests/nurbs_invariants.rs` with: partition of unity, planarity, outward normal,
   corner interpolation. Run against every constructor that exists today.
3. Sabotage-check each.

**Exit:** all six cube faces outward; `cargo test -p core_math` green.

### N1 — `NurbsSolid`: one node, N faces ★ the keystone

Everything else depends on this. **Do it before any tool work** — every tool written against the
6-node model has to be rewritten.

> ### ✅ N1 shape CONFIRMED — small addition only (owner, 2026-07-30)
> **NO HISTORY. NO feature list. NO timeline.** Owner: *"so no history, I don't need it."*
> `faces: Vec<NurbsFace>` — the current B-rep — **is** the source of truth, and the in-progress N1
> is on the right track.
>
> **The one addition: each `NurbsFace` carries a `FaceOrigin` tag** (`Base` / `Fillet{radius,
> between}` / `Chamfer{…}`). That tag is what makes a bevel editable and makes it follow when you
> move the face under it. See **§2b** for the exact shape — it is ~10 lines, not a redesign.
>
> **Save-format:** per §2c, do **not** block on `master` — append the variant and keep going.
>
> *(An earlier draft of this plan wrongly called for a Fusion-style feature list. It was wrong;
> §2b is authoritative. Do not reintroduce replay.)*

**⚠ Save-format rules — non-negotiable, read before typing:**
* `NodePayload`'s tail is `… Empty, VoxelBody, SdfObject`. **`SdfObject` is LAST — append after it.**
  bincode tags variants positionally; inserting anywhere else corrupts **every** existing `.fl4`.
* **Do NOT** change `NurbsBody`'s shape. Keep it — it is the single-patch type and it is already on
  disk. `NurbsSolid` is additive.
* **`#[serde(default)]` is inert for bincode** (no field names in the stream) — adding a field to an
  existing struct still shifts every following byte. New state goes in the **new** type only.
* **Save-format lockstep (CLAUDE.md):** appending a variant means a file containing a `NurbsSolid`
  cannot be read by a build that lacks it. The variant must land **identically on `master` and
  `feat/ui-polish` in the same session**. Flag this to the owner before committing — it is the one
  step in this plan that reaches outside this worktree.

**Stable IDs:** `NurbsFaceId`, `NurbsEdgeId`, `NurbsVertexId` newtypes. Vec indices go stale the
moment a fillet inserts a face. Selection stores IDs, never indices — same rule as `PrimUid`.

**Topology:** faces alone are not a solid. A fillet needs "which two faces share this edge." Add a
lightweight adjacency record (edge → (faceA, faceB), vertex → [edges]) built at construction and
rebuilt after topology ops. This is *runtime* state — keep it off the serialized type, mirroring the
`PolyMesh` wrapper precedent.

**Exit:** outliner shows one "NURBS.Cube"; a cube saves and reloads; `cargo test -p core_math` green.

### N2 — Modeless selection + clean highlight

Answers 2 and 3. This is what makes it *feel* right, and it is mostly deleting things.

1. **Hover pick** — ray vs tessellated faces for the face hit; then test whether the hit is within a
   screen-space threshold of an edge (→ edge) or a corner (→ vertex). Priority **vertex > edge > face**,
   so the small target wins. `1`/`2`/`3` lock the filter; no Tab.
2. **Element-scoped highlight.** Selecting a face must highlight **that face only** — a per-face index
   range in the draw, not a whole-node outline. This is the current bug: with one surface per node,
   "highlight the selection" can only mean "highlight everything."
3. **Kill the wireframe clutter.** CV grid, hull lines and isocurves **off by default**. Show CVs only
   when a vertex is actually selected. Owner: *"no weird looking NURBS wireframe, clean simple."*
4. Reuse `select_single_unified` / `toggle_select_unified` for the solid; `NurbsSelection` for
   sub-elements. **Do not write a second selection system** — one `UnifiedSelection` is a hard rule.

**Exit:** click a cube face → that one face highlights. Click an edge → that edge. Nothing else drawn.

### N3 — G / R / S on sub-elements

The payoff of N1+N2, and cheap because the machinery exists.

* Selected faces/edges/vertices move with **the AZRA gizmo** — `start_gizmo_drag`, action centre,
  axis constraints (X/Y/Z), the lot. **Route through the existing gizmo path**, never a parallel one
  (two code paths for one operation is a hard-banned pattern).
* Moving a **face** translates its control points; the shared edges must follow, or the solid opens.
  **This is the first real topology test** — assert the solid stays closed afterwards.
* One `begin_episode()` / `commit_episode()` per drag = one undo entry.

**Exit:** select a face, press G, drag — the cube deforms and stays closed. One Ctrl+Z restores it.

### N4 — Ctrl+B fillet, planar-to-planar first

**The math is exact for the cube case, so gate it hard.** Two planes meeting at dihedral θ:

* rolling ball centre at `r / sin(θ/2)` from the edge; tangent points at `r / tan(θ/2)` along each face
  (θ = 90° → tangent at exactly `r`, centre at `r√2`)
* the fillet is a **quarter-cylinder**: exactly representable as a rational quadratic in cross-section
  (weights `1, cos(Δ/2), 1` — the standard exact-arc form), degree 1 along the edge
* both neighbour faces shrink by the setback

**Gates (all three):** every fillet point exactly `r` from the axis (1e-6) · **G1 at both seams**
(`n_fillet · n_face ≥ 1 − 1e-6`) · the solid is still closed.

**Ship this before curved-face fillets and before corners.** It proves selection → gizmo → topology →
undo end to end on the case where ground truth is exact.

### N5 — The corner problem *(the known-hard step — expect it to resist)*

**Canonical matrix + OCCT source map: [`FILLET_EDGE_CASES.md`](./FILLET_EDGE_CASES.md)** (rewritten
2026-07-30 from `ChFiDS_State` / `ChFi3d_Builder` / `BRepFilletAPI_MakeFillet` — not domain folklore).

Three fillets at a cube vertex need a corner patch. **This is the NURBS analogue of the poly bevel's
multi-edge miter (~22 failed approaches)** — treat it with the same respect.

#### Architecture rules (from OCCT source — non-negotiable)

1. **Corners are N algorithms, not one `count` predicate.** OCCT: `PerformOneCorner` /
   `PerformTwoCorner` (+ `byInter` fallback) / `PerformThreeCorner` / `PerformMoreThreeCorner` /
   `PerformSingularCorner`. Our live bug class is exactly "one formula with ifs."
2. **Concavity is a first-class dimension.** `ChFiDS_AllSame` | `OnSame` | `OnDiff`. A **cube is only
   `AllSame`**. Spherical octant = that leaf only. `OnSame`/`OnDiff` appear on L-brackets and
   boss-on-plate — silently wrong if we always emit an octant.
3. **Partial results are first-class.** OCCT: `HasResult()` / `BadShape()` / `FaultyVertex(IV)`. Target
   shape: `FilletOutcome { shape, faulty_vertices }` — not one toast for the whole op.
4. **Tangent-chain propagation is Tier 1 default** (`Add(edge)` builds a tangent contour). Not "advanced."
5. **Radius is a law from day one** (`Const` / `Linear` / `Law`) — four `Add` overloads + per-vertex
   radius in OCCT. Retrofitting a scalar later touches every call site.
6. **Analytic vs general solve is real** (`PerformSetOfKPart` vs `KGen`) → `FilletSolve::{Analytic, NumericRollingBall}`.
7. **Unhandled `(CornerKind, CornerState)` → `Unsupported` refusal**, never fall through to the planar solver.

#### N5 status (2026-07-31)

**Shipped (cube / `AllSame` leaf only — do not claim general corners):**
* `make_spherical_octant` — rational biquadratic; gate `|P−C|==r`
* `fillet_symmetric_cube_vertex` + batch path: 2- or 3-selected edges at a solid triad → sphere +
  trimmed cylinders; `snap_triad_corners_to_sphere` (anti-clover wing gap)
* Side free ends: UV Arc trims (no CP relocate)
* Oracle Tier 1 face counts on unit box; FreeCAD harness present
* **Not shipped:** `CornerState` dispatch, `OnSame`/`OnDiff`, dual TwoCorner strategies, tangent chains,
  `FilletOutcome`, `Radius` law through app, non-90° / unequal-r corners

#### Trimmed surfaces (PROMPT_TRIMMED_SURFACES, 2026-07-31)

| Phase | Status | Verified |
|---|---|---|
| T1 data model | **done** | `TrimLoop`/`TrimCurve2d` on `NurbsFace.trims`; bincode round-trip test |
| T2 tessellation | **done** | grid+holes; untrimmed fast path; area gate |
| T3 fillet emits trims | **done** | side-face Arc holes; selection-only edges |
| T4 picking | **done** | face hover samples skip UV outside trims |

#### Multi-edge situations (cube only, 2026-07-31)

| Situation | Behaviour |
|---|---|
| 1 edge | Cylinder + side Arc trims at free ends (`PerformOneCorner`-class) |
| 2 top edges | Selection-only (no auto vertical) + sphere + triad snap |
| 3 concurrent **selected** edges | Sphere + trimmed cylinders (`AllSame` octant) |
| 4 verticals | 4 cylinders + free-end notches |
| `OnSame` / `OnDiff` | **Refuse or wrong if we ship octant** — need L-bracket / boss refs first |
| Cylinder body / curved | **Not yet** — toast: planar 90° only |

**Dead-end:** auto-complete missing triad edge from 2/3 selection — beveled unselected verticals. Removed
(selection-only). Distinct from OCCT tangent-chain (along a smooth edge contour, not "complete the corner").

#### OCCT oracle harness (2026-07-31 → 16-case batch)

| Piece | Status |
|---|---|
| `FILLET_EDGE_CASES.md` | Source-backed matrix + Rust shape |
| `OCCT_ORACLE_HARNESS.md` | Spec; **16 cases** need cube + **L-bracket** + **boss-on-plate** |
| `occt_fillet_ref.py` | FreeCADCmd → `refs/*.json` |
| `refs/expected_tier1.json` | Box matrix committed; expand when L/boss refs land |
| AZRA Tier 1 tests | `oracle_tier1_*` on box |
| FreeCAD mesh refs | Generate with `freecadcmd` — Tier 2 Hausdorff |
| **Never** regen refs to silence AZRA | Harness + refs/README |

**Box Tier 1 (committed):** single 6/1/0 · two-at-vertex **6/2/1** · three 6/3/1 · all-edges 6/12/8.

**Next oracle work:** generate `OnSame`/`OnDiff` solids deliberately — a cube **cannot** produce them.

#### N5 dead-end log

| # | Approach | Why it failed | Do not retry |
|---|---|---|---|
| 1 | Sequential `apply_fillet` per edge then insert corner | After edge 1 shrinks faces, edge 2/3 neighbour match by endpoints fails → `DegenerateEdge` | Mutate-then-match |
| 2 | Collect `face_idxs` only from tagged sides of `face_edges` | User/test may tag only one face per solid edge → only 2 faces discovered, third missing | Require both sides in input |
| 3 | Full-length cylinders + sphere overlapping ends | Geometric self-intersection / double surface at join | Always trim cylinders by `r` at corner end |
| 4 | Trim if *any* other selected edge touches end, but sphere only if `count == 3` | 2-edge vertex: both cylinders retract by `r`, no patch → open notch | **Trim and patch share one predicate** (`is_patch_vertex` only) |
| 5 | `update_adjacent_faces_at_endpoint` CP move to arc mid | Straight chamfer vs circular arc → dark lens gap at every r | **Parameter-space Arc trim only** (owner chose) |
| 6 | `snap_triad_corners_to_sphere` on 2-edge corners | Pulls outer vertical CPs → pyramid / beveled vertical | Never CP-snap outer vertical |
| 8 | Sphere when `count == 2` at triad (top-loop clover) | FreeCAD/OCCT top-loop = **4 cyl, 0 spheres**; two-edge = **0 spheres**. Spheres only for **3** selected edges | **Sphere iff count==3** (OCCT). Top-loop is cylinders only |
| 9 | Free-end Arc notch at every endpoint (incl. count==2 shared) | Over-cuts sides; not OCCT | **Free-end only when n_sel==1**; shared multi-edge ends use setback only |
| 7 | Draw all 4 UV edges of Fillet/Corner wires | Sphere isoparms read as flower/clover wire clutter | Fillet = u rails only; Corner = 3 boundary arcs (skip pole) |

### N6 — Beyond the cube

Only after N0–N5 land: extrude a face, revolve, loft, shell/thicken, boolean. Each one gets its
invariant test before it is called done.

---

## 4. Shortcut map — identical to poly, deliberately

| Key | Action | Notes |
|---|---|---|
| `G` / `R` / `S` | move / rotate / scale selection | AZRA gizmo, action centre, X/Y/Z constraints |
| `Ctrl+B` | fillet | radius by drag; scroll = segments, matching poly bevel |
| `1` / `2` / `3` | filter vertex / edge / face | *optional* — modeless by default |
| `Esc` / RMB | cancel + exit tool | RMB does **not** confirm (knife is the only exception) |
| tool switch | **confirms** current work, then switches | already the app-wide convention |

**Register every new binding in `ACTION_DEFS`** (`app_types/src/keybindings.rs`) so it persists and
appears in the rebind editor. Do **not** add raw `key_pressed` blocks. Check the chord is free first —
`Ctrl+B` in a NURBS context is fine alongside poly bevel (same key, different context is *correct*).

---

## 4b. 🛑 AZRA TOOL DESIGN CONTRACT — read before touching any tool

> Owner, 2026-07-30: *"Grok needs to make sure it looks at AZRA tool design when I work on the NURBS,
> because it just gave me a grab tool completely different from AZRA behavior."*

**This exact mistake was already made and corrected once today, in the poly pillar** (commit
`93d3b8f`). Its comment is the rule, in the owner's words:

> *"The G after the loop cut did not bring the proper drag tool (it brings the Blender version, this
> is not the one from AZRA)."*

### The canonical path — memorise this

```rust
// AZRA G/R/S: ARM A VISIBLE GIZMO. This is the ONLY thing a bare G/R/S may do.
self.tool.gizmo_mode  = GizmoMode::Translate;   // or Rotate / Scale
self.tool.gizmo_armed = true;
```

The user then drags a **visible gizmo handle**. `start_gizmo_drag()` (`gizmo.rs:1678`) fires, and it
delegates INTO `poly_start_transform()` / `nurbs_start_transform()` (`gizmo.rs:1746`).

**`*_start_transform` is what the gizmo calls on handle-grab. It is NOT what a bare G launches.**
Calling it directly from a key is precisely what produces "the Blender version."

### What is wrong today, specifically

| # | Problem | Evidence |
|---|---|---|
| 1 | **NURBS invented its own grab action.** Poly has **no** `ACT_POLY_GRAB` — it uses AZRA's *global* G/R/S. NURBS added `ACT_NURBS_GRAB_CV => KeyBind::simple(G)`, shadowing it. | `keybindings.rs:308`, `:663` |
| 2 | **No R or S at all.** `ACT_NURBS_GRAB_CV` is the only NURBS transform action — rotate and scale have no NURBS path. Owner asked for *"G R S, select face, move it with G R S, same gizmo."* | `keybindings.rs:304-308` — five NURBS actions, one transform |
| 3 | **Two code paths for one operation** — hard-banned by CLAUDE.md. The keyboard path arms the gizmo correctly; the command path sets a tool mode instead. Pressing G and invoking the same command from a menu do **different things**. | `nurbs_tools.rs:1068-1073` (arms gizmo) vs `app_commands.rs:165` (`nurbs_tool = NurbsTool::MoveCv`) |
| 4 | **Silently does nothing** in Object mode or with no sub-selection — no gizmo, no toast, no feedback. | the `has_sub_selection()` guard at `nurbs_tools.rs:1069-1071` |

### The fix

**Delete `ACT_NURBS_GRAB_CV` and `AppCommand::NurbsGrabCv`. Do not replace them.** Extend the existing
global G/R/S branch in `input_keyboard.rs` — the one added by `93d3b8f` — so its condition also covers
a NURBS sub-selection. One branch, one behaviour, all three keys, both pillars. That is the
"two paths must share one path" rule applied literally.

### G / R / S must work at BOTH levels — this is the whole point

> Owner, 2026-07-30: *"Rotation works in Plasticity, and scale can still scale the whole object. The
> goal is to make sure we have the same behavior / muscle memory for most of the actions."*

**Muscle memory is the product requirement.** The user should never have to remember "in NURBS, that
key does something else." Same key, same gizmo, same feel — the only thing that changes is what is
selected.

| Selection | G | R | S |
|---|---|---|---|
| **The solid** (Object level) | move the node | rotate the node | **scale the node** — see the trap below |
| **Face / edge / vertex** (sub-element) | move those CVs | rotate them about the action centre | scale them about the action centre |

All six cells are the **same global G/R/S → `gizmo_mode` + `gizmo_armed`** path. There is no NURBS
branch, no NURBS-only key, and no separate modal. What differs is only which selection the gizmo
operates on — exactly as it already differs between an SDF body and a poly face.

### ⚠ Object-level scale: scale the TRANSFORM, never the control points

Baking scale into the CVs breaks `FaceOrigin` (§2b) — and silently:

* **Uniform scale** of a fillet is still a valid fillet, but its recorded `radius` is now wrong by the
  scale factor. The face says "radius 2" while the geometry is radius 4.
* **Non-uniform scale** is worse: a rolling-ball fillet's circular cross-section becomes an **ellipse**.
  It is no longer a fillet at all, so `FaceOrigin::Fillet` becomes a lie and the next re-solve
  silently replaces the user's shape with a circular one.

**So object-level scale writes the node's scale, never the CVs.** This is the same rule the SDF pillar
already enforces — *"moving/rotating/scaling a body NEVER modifies prim positions; it only changes
`body.origin` / `body_rotation` / `body_scale`."* Apply it identically here: geometry stays in local
space, fillets stay honest, zero re-solve, and it composes with the gizmo for free.

*(If "apply scale" — baking the transform into geometry — is ever wanted, it is a separate explicit
command, and for non-uniform scale it must either refuse or downgrade the affected `FaceOrigin` to
`Base` with a toast. Never silently keep a `Fillet` tag that no longer describes the surface.)*

### Ctrl+B — copy the poly bevel interaction EXACTLY

> Owner, 2026-07-30: *"Just now I worked on bevel, but I cannot choose the width by just dragging like
> in AZRA. I understand this is not the same code, but muscle memory is important."*

**"Not the same code" is not a reason for "not the same feel."** The NURBS fillet solves different
math, but the *interaction* is a copy. Mirror `PolyBevelDrag` field for field
(`app_types/src/types_poly.rs:306`, started at `poly_drag_tools.rs:1463`):

```rust
// The AZRA bevel interaction, in one struct. NurbsFilletDrag mirrors this.
PolyBevelDrag {
    start_mouse: egui::Pos2,   // captured at Ctrl+B — width is drag distance FROM here
    width: 0.0,                // starts at ZERO and grows with the drag
    segments: 3,
    shape: 0.5,                // 0 = flat chamfer · 0.5 = circular fillet · 1 = convex
    scroll_accum: f32,         // wheel accumulator for segments
    clamp: true,               // Clamp Overlap ON; C toggles
    original_mesh: Arc<…>,     // the PRE-op state — every frame regenerates from this, never
                               // from last frame's result (else the op compounds on itself)
}
```

**The controls, all of which must work:**

| Input | Effect | Poly reference |
|---|---|---|
| **Drag** | sets width from `start_mouse` distance — **this is the one that is missing** | drag loop → `poly_bevel_regenerate()` |
| **Scroll wheel** | segments up/down | `poly_bevel_scroll_segments()` |
| **Type a number** | exact width | `poly_bevel_set_width()` |
| **C** | toggle clamp-overlap | `poly_bevel_toggle_clamp()` |
| **Enter / LMB** | confirm | |
| **Esc / RMB** | cancel, restore `original_mesh` | |
| **switch tool** | confirms first, then switches | |

**Two structural details that are easy to miss and both matter:**

1. **Regenerate from `original_mesh` every frame, never from the previous frame's output.** Otherwise
   dragging compounds the fillet on itself and the width becomes non-linear and unreturnable. For
   NURBS this means: keep the pre-fillet faces, and re-solve from them on every drag frame.
2. **Ship the Adjust-Last-Operation panel.** Poly keeps a redo panel alive *after* confirm
   (`PolyBevelAdjust` / `PolyBevelHistory`, `poly/bevel.rs:47`) so width/segments/shape stay editable
   without redoing the op. For NURBS this falls out of `FaceOrigin` almost free (§2b) — the fillet
   face already stores its radius, so the panel edits that and re-solves.

### Parity checklist — every NURBS tool must have ALL of these

Copy the poly behaviour; do not re-derive it. If poly does it, NURBS does it the same way.

- [ ] G / R / S arm the gizmo — never launch a modal directly
- [ ] The **action centre** is honoured (median / individual / cursor), not a hardcoded pivot
- [ ] X / Y / Z axis constraint, **Shift+axis** = plane constraint
- [ ] Numeric input during the drag, Backspace to correct
- [ ] **Enter or LMB confirms; Esc or RMB cancels** (RMB confirms only for Knife)
- [ ] **Switching tools CONFIRMS** the live operation, then switches
- [ ] **ONE undo entry per drag** — `begin_episode()` on start, `commit_episode()` on release
- [ ] Registered in `ACTION_DEFS` with a context guard, so it persists and is rebindable
- [ ] Fails **visibly** — a toast when a precondition is missing, never a silent no-op

**Before adding ANY new NURBS action, grep for the poly equivalent first.** If poly solves it with a
global action, NURBS uses that same global action. A new `ACT_NURBS_*` is only justified when the
operation genuinely has no poly counterpart (knot insert, weight adjust — those are correct).

---

## 5. Standing rules

1. `cargo build -p app` after every change; `cargo test -p core_math` after any `nurbs.rs` change.
2. **Every NURBS op ships an invariant test from §0.** No test → not done.
3. **Sabotage-check every gate** — revert the fix, confirm FAIL, restore.
4. Stable IDs, never Vec indices — a fillet inserts faces and invalidates every index.
5. All mutations through `transaction()` / `begin_episode()`. Every slider gets undo.
6. **Search before writing** — `duplicate_subtree`, `cam_ray`, `select_single_unified`,
   `start_gizmo_drag` all exist. Extend, don't duplicate.
7. **Never change a serialized shape.** Append-only on `NodePayload`; new state on new types.
8. One item per commit, stating what was *verified*.

---

## 6. Open questions — ask the owner, do not guess

* **Fillet radius UI:** drag-to-set like poly bevel, or numeric entry, or both?
* ~~**Non-uniform solids: procedural history or destructive?**~~ **ANSWERED 2026-07-30 — neither.**
  No history (owner: *"so no history, I don't need it"*), and the fillet is **not** destructive
  either. **Faces remember what they are** via `FaceOrigin` (§2b), so a fillet stays editable and
  follows its neighbours when they move. That is the whole mechanism — do not reopen this.
* **Booleans:** NURBS-native (hard, needs surface-surface intersection) or tessellate-and-boolean
  (easy, loses the NURBS)? Not needed before N6, but it shapes N1's topology record.

---

## 7. Dead-end log

*(Append every failed approach here, with the reason. Same discipline as `BEVEL.md` §6 — this file is
the only defence against re-deriving a dead end when there is no oracle to check against.)*

| date | approach | why it failed |
|---|---|---|
| | | |
