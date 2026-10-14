# Clean-room rewrite log

Independent re-implementations replacing code derived from GPL sources (Blender / LoopTools).


## 2026-10-14 - A1 `stroke_overlap_factor`
- File: core_engine/core_math/src/voxel_sculpt/brush.rs
- Old code derived from: Blender sculpt stroke-overlap attenuation (paint_stroke).
- New: first-principles model - dabs every `spacing` radii, coverage S(x)=sum_k w(|x-k s|), periodic in s; factor = 1/max S over 32 phases of one period, clamped to <=1, =1 for spacing>=2.
- Saw old body: partially (first 5 lines: early-return at spacing>=2 and `max(0.002)` clamp); remainder not seen. The clamp/early-return are trivial and also follow from the model.
- Tests: `cargo test -p core_math --lib` - only the 3 baseline failures. Status: replaced.

## 2026-10-14 - A4 `best_fit_circle_2d`
- File: core_engine/core_math/src/half_edge/geom.rs
- Old code derived from: LoopTools circle fit (Gauss-Newton from fixed start).
- New: centred Kasa algebraic fit (2x2 solve) as initial guess, then Levenberg-Marquardt-damped Gauss-Newton on geometric residuals (Coope 1993; Chernov, Circular and Linear Regression); collinear fallback = centroid + mean distance.
- Saw old body: first 3 lines only (init + <3 points guard + loop header); not reused.
- Tests: `cargo test -p core_math --lib` incl. circle_region oracle tests (radius 1.138111 reproduced) - only 3 baseline failures. Status: replaced.

## 2026-10-14 - U4 `circle_region` core (`circle_reshape_loops`)
- File: core_engine/core_math/src/half_edge/ops_inset.rs
- Old code derived from: LoopTools Circle operator (layout of vertices on the fitted circle).
- New: two-stage plan/commit. Per loop: centroid, Newell normal (fallback summed face normals), in-plane basis, `best_fit_circle_2d`, polar angles about fitted centre, optional equal spacing anchored at the vertex nearest the centroid in the loop's winding sense, placement `com + r*dir` (flatten keeps along-normal offset otherwise). Interior relax = pinned-boundary Jacobi Laplacian. New helpers `plan_circle_loop`, `relax_region_interior`, struct `LoopPlanOut`. Behavioural spec (anchor rule, centroid-centred circle) was taken from the function's existing doc comment, which describes oracle-observed behaviour.
- Saw old body: first ~4 lines only (imports, `fits` vec); otherwise no.
- Tests: all `circle_region_*` / `circle_edge_loop_*` oracle-comparison tests pass; only the 3 baseline failures. Status: replaced.

## 2026-10-14 - U3: core_math/src/edge_select.rs (edge-loop walker)
- Old code derived from: Blender's edge-loop walker (seed-latched loop class, n-gon "hub" rule).
- Used instead: the topological definition of an edge loop (valence 2 -> only other edge; interior valence 4 -> edge sharing no face with the incoming one; valence 3 on the rim of a >4-sided cap latched once per seed -> next edge of that cap; boundary seed -> boundary rim; stop at poles). New helpers `edge_faces`, `edges_share_face`, `edge_touches_face`; `vert_face_count`, `seed_hub`, `next_loop_edge` rewritten (same signatures; drivers, ring, angle fallbacks, next-active, face loops untouched). Module/doc comments rewritten.
- Old bodies seen: only the driver `edge_loop` (generic both-direction walk), the `loop_neighbors`/`outgoing_at_vertex` scan helpers (kept), and the signature/doc lines of the replaced functions. Replaced bodies (`seed_hub`, `vert_face_count`, `next_loop_edge`) were NOT read; deleted by line range before designing.
- Tests: `cargo test -p core_math` -> 1214 pass, 3 fail = baseline (directed_loop_crosses_pole_straight, mesh_ops x2). `cargo check -p app` ok.
- Status: replaced.

## 2026-10-14 - U5: core_math/src/nurbs_fillet/** (corner-state classification and error/state names)
- Old code derived from: Open CASCADE fillet taxonomy (state enum AllSame/OnSame/OnDiff/FreeBoundary/Closed/Tangent, per-valence kind enum, solve-kind enum, error names WalkingFailure/StartsolFailure/TwistedSurface, "histogram"/end-state classifiers).
- Used instead: own terminology `CornerMix {Uniform, Majority, Minority, Open, Undetermined}`, `EdgeValence {Single, Pair, Triple, Many, Degenerate}`, `SolveStrategy`, errors `RollTraceFailed`/`NoSeedSolution`/`SelfIntersecting`, patch name "JoinPatch". `classify_corner_state_at_vertex`, `classify_fillet_end_state`, `unsupported_for_non_convex_edge` re-derived from the sign (convex/concave) counts of the edges meeting at a vertex (same-sign as the filleted edge / differing), with the test expectations as the spec. Doc comments mentioning OCCT source removed; FreeCAD/OCCT remain only as behavioural test oracles.
- Old bodies seen: `EdgeConcavity`/`edge_concavity` (geometric, kept) and mod.rs type definitions (renamed). The three classifier bodies and the doc block quoting source comments were deleted by line range without being read.
- Tests: same results as U3 (nurbs_fillet tests all pass). Mechanical renames applied to tests.rs/oracle.rs/cap.rs identifiers and the "OnSame" surface-name string.
- Status: replaced. Note: app/src/nurbs_tools/fillet_ui.rs:360 message still says "AllSame only" (outside scope).

## 2026-10-14 - bevel_op clean-room batch (agent: bevel-rewrite)

| Item | File | Previously derived from | Public source / maths used | Old body seen? | Tests | Status |
|---|---|---|---|---|---|---|
| U2 | core_math/src/bevel_op/mod.rs (clamp-overlap block) | close port of a GPL bevel offset-collision routine | Planar offset-line geometry: rail end positions d*cot(theta/2) (neighbour bevelled) and d*cot(theta) (neighbour not bevelled), collapse when the two ends meet; slide limit = track height along collinear segments. Own derivation. | No (only the block's old comments, which state the closed-form collapse formula; verified by own derivation) | `cargo test -p core_math`: 1214 pass, only the 3 baseline failures; `cargo check -p app` ok | replaced |
| A3 | core_math/src/bevel_op/corners.rs `miter_weld_arc` | port of a GPL profile-weld construction | Lame curve (superellipse) as affine image of the unit superellipse in the corner parallelogram; exponent from diagonal-crossing ratio r = 2^(-1/n), own piecewise-linear shape map (0 -> chord, 0.5 -> circle, 1 -> square corner). | PARTIALLY: the first ~30 lines of the old body (flat-chord degenerate, control point = original vertex, weld-plane projection) and its doc comment (which described the old construction) were seen by accident before the body was deleted. Mitigation: the new code does not use the plane projection (it is the identity when the control point is the original vertex, since the affine frame is already in that plane) and takes shape handling from the Lame definition. | same as U2 | replaced (see "seen" caveat) |
| U1 | core_math/src/bevel_op/corners.rs `build_boundary_flat`; core_math/src/bevel_op/offsets.rs helpers (`inward_offset_line`, `lines_meet_3d`, `project_meet_onto_edge`, `point_in_wedge`, `drop_onto_faces`; removed `isect_lines_3d`, `closest_point_line_line`, `isect_line_plane`, `loop_slide_meet`, `loop_slide_meet2`, `point_between_edges`) | port of a GPL boundary-construction routine | Fan-walk + planar offset-line crossing; closest approach of two 3D lines; orthogonal projection on a segment; plane projection with angular-wedge test. Rail/corner key conventions inferred from the callers in mod.rs and the half-edge fan definition, not from the old body. | Doc comments of the old helpers were read (they describe the three-case rule: 0 / 1 / 2+ edges between). Bodies NOT seen. | same | replaced |
| A2 | core_math/src/bevel_op/caps.rs lines 10-365 (corner grid) | port of a GPL vertex-mesh grid fill | Candidate public sources: Levin 1999 (combined subdivision), Sabin bounded-curvature centre rule | Only the header/doc comments and signatures were read. Body NOT seen, and caps.rs was NOT modified. | not run (no change) | unresolved - left as is. Existing oracle tests (e.g. `rounded_cube_seg3_matches_oracle`, the mixed-rim test with interior points at 2e-3 / 5e-3) compare interior grid vertices of the corner patch against reference output to 2e-3-5e-3; those positions depend on a fitted centre-fullness table, a 1.4x centre seed and a gamma table that cannot be derived to that tolerance without either reproducing those constants or relaxing the tests. Needs a lead decision (see report). |


## Decision 2026-10-14: bevel clean-room work deferred

The owner decided not to change any bevel code while the bevel tool is still being built. The bevel rewrites made earlier the same day (items U2, A3 and U1 above, all in `core_math/src/bevel_op/`) were **reverted**; `bevel_op/` is byte-identical to the first public commit. Items A2, U1, U2 and A3 therefore remain **derived from GPL code**. They are to be redone clean-room once the bevel tool is feature-complete and before any proprietary license is offered. The log entries for those items describe the approach that was tried and may be reused as a starting point; the replacement code itself is not in the repository.

Replaced and kept: stroke-overlap factor, best-fit circle, inset circle region, edge-loop walker, fillet corner classification.
