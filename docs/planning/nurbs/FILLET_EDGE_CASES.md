# NURBS fillet — edge-case matrix, derived from OCCT source

**Sources (read 2026-07-30, `Open-Cascade-SAS/OCCT` master):**
* `src/ModelingAlgorithms/TKFillet/ChFiDS/ChFiDS_State.hxx` — corner/end-condition states
* `src/ModelingAlgorithms/TKFillet/ChFiDS/ChFiDS_ErrorStatus.hxx` — runtime failure taxonomy
* `src/ModelingAlgorithms/TKFillet/ChFi3d/ChFi3d_Builder.hxx` — algorithm dispatch
* `src/ModelingAlgorithms/TKFillet/BRepFilletAPI/BRepFilletAPI_MakeFillet.hxx` — API surface

> **Correction to the previous version of this file.** It was written from general CAD domain
> knowledge and **missed an entire dimension** — the *concavity configuration* at a corner
> (`ChFiDS_AllSame` / `OnSame` / `OnDiff`). It also mis-ranked tangent-chain propagation as an
> advanced feature when OCCT makes it the **default behaviour of `Add()`**. Both corrected below.

---

## 1. What OCCT's structure tells us

### 1a. Corner handling is N separate algorithms, not one with parameters

`ChFi3d_Builder` dispatches to **distinct methods per vertex valence**:

| OCCT method | Configuration |
|---|---|
| `PerformOneCorner` | 1 filleted edge at the vertex |
| `PerformTwoCorner` **and** `PerformTwoCornerbyInter` | 2 edges — **two different strategies** |
| `PerformThreeCorner` | 3 edges |
| `PerformMoreThreeCorner(Index, nbcourb)` | > 3 edges |
| `PerformSingularCorner` | degenerate/singular vertex |
| `PerformFilletOnVertex` | filleting a **vertex** itself, not an edge |
| `PerformIntersectionAtEnd`, `IntersectMoreCorner` | resolving where fillet ends meet |
| `ExtentOneCorner` / `ExtentTwoCorner` / `ExtentThreeCorner` | extend the spine *before* building the corner (setback) |

**Consequence for AZRA:** 1-, 2-, 3- and N-edge corners are **not variations of one formula**. Our
current bug (0.5) is exactly this — a single predicate (`count == 3`) trying to cover cases OCCT
gives separate algorithms. Two strategies for the 2-corner case also implies the direct one fails
in some configurations and needs an intersection-based fallback.

### 1b. ⭐ Concavity configuration — the dimension we missed entirely

`ChFiDS_State`, verbatim:

| State | Source comment |
|---|---|
| `ChFiDS_AllSame` | "the three concavities are on the same side of the Shape" |
| `ChFiDS_OnSame` | "the edge of the fillet has a concave side different than one of the two other edges and identical to the third edge" |
| `ChFiDS_OnDiff` | "the edge of the fillet has a concave side different than the two other edges" |
| `ChFiDS_BreakPoint` | corner with 3 edges and 3 faces — base case for the three above |
| `ChFiDS_FreeBoundary` | (no comment) — spine ends at an open boundary |
| `ChFiDS_Closed` | (no comment) — closed loop |
| `ChFiDS_Tangent` | (no comment) — tangent continuation |

**A cube corner is only `AllSame`.** `OnSame` and `OnDiff` arise the moment convex and concave edges
meet at one vertex — an L-bracket, a boss on a plate, any real part. **Our spherical-octant corner
is the `AllSame` case only**, and will be silently wrong for the other two.

### 1c. Analytic vs general dispatch is real

`PerformSetOfKPart` (known/analytic special cases) vs `PerformSetOfKGen` (general) — OCCT splits the
surface solve exactly the way the `FilletSolve` enum below proposes. That design is validated.

### 1d. Failure is granular and partial results are first-class

`ChFiDS_ErrorStatus`: `Ok` · `Error` · **`WalkingFailure`** (rolling-ball march failed) ·
**`StartsolFailure`** (no valid starting solution) · **`TwistedSurface`** (result self-intersects).

And the API exposes: `HasResult()`, `BadShape()` (the *partial* result), `NbFaultyContours()` /
`FaultyContour(I)`, **`NbFaultyVertices()` / `FaultyVertex(IV)`**, `StripeStatus(IC)`, `Simulate(IC)`.

**This is the strongest design lesson in the whole source read.** OCCT does not fail all-or-nothing:
it builds what it can, hands back the partial shape, and names **which vertex** failed. AZRA should
copy this — it satisfies "fail visibly" (R8) far better than a single toast.

### 1e. Tangent-chain propagation is the DEFAULT, not a feature

`Add(const TopoDS_Edge& E)` — *"Adds a fillet contour in the builder (**builds a contour of tangent
edges**)."* Every `Add` overload says the same. **Selecting one edge fillets the whole tangent chain**
in OCCT. Our previous ranking of this as Tier 2 was wrong; users coming from any CAD tool expect it.

### 1f. Radius is a law, not a scalar

`Add(R1, R2, E)` (linear law) · `Add(Law_Function, E)` · `Add(Array1<gp_Pnt2d> UandR, E)`
(interpolated) · `SetRadius(R, IC, TopoDS_Vertex)` (**per-vertex**).

`ChFi3d_FilletShape` = `Rational` (default, exact NURBS circle) · `QuasiAngular` · `Polynomial`.
`SetContinuity(GeomAbs_Shape, tol)` — C0/C1/C2 internal continuity.

**Design implication:** model radius as `enum Radius { Const(f32), Linear{r0,r1}, Law(...) }` from the
start. Retrofitting a scalar into a law later touches every call site.

---

## 2. The matrix

Status columns (owner STATE RESET 2026-08-01 — **kernel ≠ tool**):

| Symbol | Meaning |
|---|---|
| ✅ kernel | `core_math` path green (unit / FreeCAD oracle). **Not** app-path done. |
| ⚠️ kernel | kernel partial / residual |
| ❌ tool | broken or missing in the app integration layer |
| ⬜ | not started at either layer |
| 🟢 tool | app-path verified with **AZRA_NURBSDIFF** harness shot |

> **Rule (owner):** no NURBS tool milestone is **done** without a harness shot.
> `cargo test -p core_math` alone never counts again — that is how "done" shipped broken.

### Tier 0 — live bugs, nothing else matters until these close

| # | Case | OCCT ref | Kernel | Tool / app-path |
|---|---|---|---|---|
| 0.1 | Two planar orthogonal faces, one edge, no corner | — | ✅ kernel | ❌ tool — floating selection / sequential gaps |
| 0.2 | Radius clamp / refuse when too large | `StartsolFailure` | ✅ kernel | ⚠️ tool (toast exists; no harness) |
| 0.3 | 3 edges, all filleted, equal r, orthogonal (`AllSame`) | `PerformThreeCorner` | ✅ kernel octant + G1 gate | ❌ tool |
| 0.4 | Side-face notch where a cylinder ends | `PerformIntersectionAtEnd` | ⚠️ kernel Arc notch n_sel==1 | ❌ tool |
| 0.5 | **2 of 3 edges filleted** | `PerformTwoCorner` | ✅ kernel (0 spheres; Tier2 hole ~0.008) | 🟢 tool — sequential un-shrink+rebatch (`02_vertical_then_top_r02.png`) |

**Honest map (2026-08-01 STATE RESET):**
* **Kernel landed** for many Tier 0–3 rows below. Owner in-app test: **fillet is NOT done at tool level.**
* Integration work: `AZRA_NURBSDIFF` harness, solid primitives as `NurbsSolid`, stable-id selection, sequential junction, edit/delete anytime.
* FreeCAD **Tier 2 Hausdorff** wired (`*_pts.json`) — kernel gate only.
* `apply_edges_fillet_batch_outcome` → `FilletOutcome` exists; app still thin-wraps.
* Tangent-chain: **`expand_tangent_edge_chain`** (G1-collinear only; cube corners stay put) — kernel.

### Tier 1 — a usable fillet on real solids

| # | Case | OCCT ref | Kernel | Tool |
|---|---|---|---|---|
| 1.1 | 1 edge at a vertex, others sharp | `PerformOneCorner` | ⬜ | ⬜ |
| 1.2 | **`OnSame` concavity corner** | `ChFiDS_OnSame` | ✅ kernel circular loft / toroid | ❌ tool (L-bracket not a solid primitive UI path) |
| 1.3 | **`OnDiff` concavity corner** | `ChFiDS_OnDiff` | ✅ kernel edge-relative classify | ❌ tool |
| 1.4 | Concave edge (interior > 180°) | concavity sign | ✅ kernel | ❌ tool |
| 1.5 | Non-orthogonal dihedral (acute/obtuse) | — | ✅ kernel rational arc | ❌ tool |
| 1.6 | **Tangent-chain propagation** | `Add()` default | ✅ kernel G1-collinear | ⚠️ tool (wired into Ctrl+B start) |
| 1.7 | Edge shorter than 2×r (setbacks overlap) | `Extent*Corner` | ✅ kernel | ❌ tool |
| 1.8 | Radius at exact max — face shrinks to zero | `StartsolFailure` | ✅ kernel | ❌ tool |
| 1.9 | Fillet consumes a whole face → delete it | — | ⬜ | ⬜ |
| 1.10 | Unequal radii at one vertex | `SetRadius(R,IC,Vertex)` | ✅ kernel per-edge const | ❌ tool |
| 1.11 | Non-orthogonal 3-edge corner | `PerformThreeCorner` | ⚠️ kernel octant assumes ⊥ | ⬜ |

### Tier 2 — beyond a box

| # | Case | OCCT ref | Kernel | Tool |
|---|---|---|---|---|
| 2.1 | Closed edge loop (cylinder rim → torus) | `ChFiDS_Closed` | ✅ kernel `fillet_cylinder_plane_rim` | 🟢 tool — cylinder `NurbsSolid` + rim path (`04_cylinder_rim_r015.png`) |
| 2.2 | Spine ends at an open boundary | `ChFiDS_FreeBoundary` | ⬜ | ⬜ |
| 2.3 | Planar + cylindrical face | `PerformSetOfKPart` | ✅ kernel rim path; general → `CurvedFace` refuse | ❌ tool |
| 2.4 | Cylinder + cylinder, parallel axes | `PerformSetOfKPart` | ⬜ | ⬜ |
| 2.5 | Cylinder + cylinder, skew axes | `PerformSetOfKGen` | ⬜ | ⬜ |
| 2.6 | Free-form + free-form (numeric rolling ball) | `PerformSetOfKGen` | ⬜ | ⬜ |
| 2.7 | **> 3 edges at a vertex** | `PerformMoreThreeCorner` | ⬜ | ⬜ |
| 2.8 | Two fillets overlap → detect/refuse | `TwistedSurface` | ⬜ | ⬜ |
| 2.9 | Fillet across a surface seam | — | ⬜ | ⬜ |

### Tier 3 — defer (kernel arms)

| # | Case | OCCT ref | Kernel | Tool |
|---|---|---|---|---|
| 3.1 | Variable radius, linear law | `Add(R1,R2,E)` | ✅ kernel slanted setback | ⚠️ tool live `V` cycle + Adjust panel (last-op only) |
| 3.2 | Variable radius, arbitrary law / interpolated | `Add(Law_Function)` / `Add(UandR)` | ✅ kernel multi-station Law + bent rail | ⚠️ tool live `N` stations; not anytime |
| 3.3 | **Vertex fillet** (fillet a vertex, not an edge) | `PerformFilletOnVertex` | ⬜ | ⬜ |
| 3.4 | Singular/degenerate corner | `PerformSingularCorner` | ⬜ | ⬜ |
| 3.5 | Chamfer mode | `ChFi3d_ChBuilder` | ⬜ | ⬜ |
| 3.6 | Alternate circle representations | `QuasiAngular` / `Polynomial` | ⬜ | ⬜ |
| 3.7 | C2 internal continuity | `SetContinuity` | ⬜ | ⬜ |

### Robustness guards — every tier

| # | Guard | OCCT analogue | Kernel |
|---|---|---|---|
| R.1 | Edge length ≈ 0 | `DegenerateEdge` | ⬜ |
| R.2 | Dihedral ≈ 180° (tangent faces) | `StartsolFailure` | ⬜ |
| R.3 | Dihedral ≈ 0° (folded) | `StartsolFailure` | ⬜ |
| R.4 | `r <= 0` / non-finite | — | ⬜ |
| R.5 | Coincident vertices within tolerance | — | ⬜ |
| R.6 | Result self-intersects | **`TwistedSurface`** | ⬜ |
| R.7 | Rolling-ball march fails to converge | **`WalkingFailure`** | ⬜ |
| R.8 | Result not closed (free boundary on a trimmed end) | — | ⬜ |
| R.9 | Non-monotone `Radius::Law` (interior r peak/valley) | — | ✅ kernel bent rail |

---

## 3. Rust structure

**Types landed in `core_math::nurbs_fillet` (2026-07-31).** Dispatch is **not** fully rewired yet —
batch still runs the `AllSame` octant leaf with `f32` radius; `Radius::Const` / `Unsupported` /
`FilletOutcome` are the shape to grow into.

```rust
enum CornerState { AllSame, OnSame, OnDiff, FreeBoundary, Closed, Tangent }
enum CornerKind { One, Two, Three, MoreThan(u8), Singular }
enum FilletSolve { Analytic, NumericRollingBall }
enum Radius { Const(f32), Linear { r0: f32, r1: f32 }, Law(Vec<(f32, f32)>) }
enum FilletError {
    WalkingFailure, StartsolFailure, TwistedSurface,
    DegenerateEdge, RadiusTooLarge { max: f32 },
    Unsupported { kind: CornerKind, state: CornerState },
    // + NotPlanar / NotOrthogonal / BadEdgeIndex
}
struct FilletOutcome {
    faces: Vec<NurbsFace>,  // partial if faulty_vertices non-empty
    faulty_vertices: Vec<(Vec3, FilletError)>,
}
```

**Rules that fall out of the source:**
* **Never let an unhandled `(CornerKind, CornerState)` fall through to the planar solver.** Return
  `Unsupported` — a wrong surface is worse than a refusal (No Hacks).
* **Return partial results with named faulty vertices.** OCCT proves this is the right UX: fillet
  what you can, tell the user exactly which corner failed.
* **`concave` is a sign flag inside the solver, never a second code path.**
* **Corner blends are their own module.** The spherical octant is the `AllSame`-orthogonal-equal-radius
  case — one leaf, not the general form.

---

## 4. Priority (STATE RESET 2026-08-01 — integration, not more kernel leaves)

Kernel rows above that said "landed" are **kernel landed** only. Tool priorities in order (one commit each):

0. **P0 AZRA_NURBSDIFF** — scripted app-path harness (create cube → fillet vertical r=0.2 → shot → adjacent top → shot). Modeled on `AZRA_VISDIFF`. No tool milestone without a shot.
1. **P1 Solid primitives** — cylinder/sphere/torus as `NurbsSolid` (not legacy `NurbsBody`). Cylinder = wall + two circular-trim caps. Rim fillet reachable from UI.
2. **P2 Stable-id selection** — `NurbsSelection` on `NurbsFaceId`; remap after regenerate; overlay always from current face list.
3. **P3 Junction sequential** — repro owner screenshot (vertical then top, both orders, both-in-one-batch); fix gap/gouge vs FreeCAD two-edges ref.
4. **P4 Plasticity edit/delete anytime** — select Fillet face → properties radius + Adjust + Remove (un-shrink planar supports); undoable at any later time.

Prior kernel leaves (kept for history, not current tool gates):
* Core fillet + FaceOrigin Linear/Law re-solve — **kernel landed**.
* OnSame 2-edge circular loft — **kernel landed**.
* Live drag Const/Linear/Law + last-op Adjust panel — **partial tool** (not anytime / not harness-gated).

### FaceOrigin variable-radius (landed)

| Arm | When written | Re-solve |
|-----|--------------|----------|
| `Fillet { radius, between }` | Const batch | `Radius::Const` |
| `FilletLinear { r0, r1, between }` | Linear batch (append-only enum arm) | `Radius::Linear` via `resolve_fillet_op_from_origins` |
| `FilletLaw { samples, between }` | Law batch (append-only) | `Radius::Law` |

**Bincode rule:** never insert arms above `FilletLinear` / `FilletLaw` — they are last for layout stability.

**Oracle (FreeCAD 1.1.3 on this machine):**
* `occt_sample_tier2.py` → lightweight `refs/*_pts.json` (commit these).
* Linear: `single_edge_linear_r005_r015_pts.json` from `makeFillet(0.05, 0.15, [edge5])`.
* `occt_fillet_ref.py` → full meshes (gitignored if huge).
* Tier1 matrix: `expected_tier1.json` (sphere only for 3-edge).
