# nurbs_fillet split — good-practice bug sweep findings

Collected while relocating `core_math/src/nurbs_fillet.rs` (6,542 lines) into
`nurbs_fillet/{mod,solve,cap,oracle,tests}.rs` (branch `chore/split-nurbs-fillet`).
**Observations only — nothing was fixed here** (owner directive: sweep, don't touch).

## 1. `occt_oracle` feature is referenced but never declared
`oracle.rs` gates `load_occt_tier2_points` with `#[cfg(any(test, feature = "occt_oracle"))]`,
but `core_math/Cargo.toml` has no `[features]` section — the feature can never be turned on,
and cargo emits `unexpected cfg condition value: occt_oracle` warnings (pre-existing on master).
Either declare the feature (and move serde_json to an optional dep behind it) or drop the
`feature = ...` arm and keep it test-only. Do NOT un-gate: serde_json is a dev-dependency
(un-gating broke the build before, c8cf303).

## 2. Tolerance magic numbers without ratchet notes
Raw literals (`1e-3`..`1e-6`, `0.999`, `0.9995`, `0.9999`) appear ~65 times in `solve.rs`,
~11 in `cap.rs`, ~3 in `oracle.rs`, mostly inline in planarity checks (`surface_is_planar`),
cylinder fitting (`try_fit_cylinder`), tangent-chain expansion (`expand_tangent_edge_chain_tol`),
orthogonality gates (triad detection), and seam asserts. Almost none carry a comment saying
why that value or what oracle case ratcheted it. `0.999`-style cosine gates in particular
are duplicated across classification, chain expansion, and triad detection with slightly
different values — a future unification target (named consts with provenance comments).

## 3. Near-duplicate const / linear / law solve rails
Three parallel families that share ~70% of their structure and diverge only in how the
radius varies along the edge:
- `fillet_planar_edge_trimmed_ex` / `fillet_planar_edge_linear` / `fillet_planar_edge_law`
  (each rebuilds rails, setbacks, and the rational cross-section independently);
- `shrink_face_edge` / `shrink_face_edge_tapered` / `shrink_face_edge_law` (cap.rs);
- batch entry points `apply_edges_fillet_batch_radius` / `_edge_radii` / `_linear` / `_law`
  each re-implementing edge dedupe + corner classification glue.
A bug fixed in one rail (e.g. setback capacity clamp) does not automatically reach the
other two. Candidate for a single law-driven rail with `Radius::Const`/`Linear` as
degenerate cases — but that is a behavior-affecting refactor, not for this split.

## 4. Unwraps on geometry-derived indices
- `solve.rs:332` (`apply_cylinder_rim_fillet_to_solid`): `cap_fi.unwrap()` — safe today only
  because the preceding `if cap_fi.is_none()` hard-codes the face layout of
  `new_cylinder_nurbs_faces` (`Some(if top { 1 } else { 2 })`); a solid with a different
  face order silently picks the wrong cap face rather than erroring.
- `cap.rs:493-494` (`fillet_symmetric_cube_vertex` / octant path):
  `face_idxs.iter().position(...).unwrap()` twice — panics if the triad pair list references
  a face not in `face_idxs`. Upstream triad construction currently guarantees it, but the
  invariant is implicit (no assert/comment).

## 5. Misc
- `assert_fillet_radius` / `assert_sphere_radius` / `assert_g1_seams` / `assert_tier1_face_counts`
  are `pub` panicking fns compiled into the production lib (not test-gated), under a
  "Invariant helpers (for tests)" banner. Harmless but odd surface for app code to reach.
- `FREECAD_UNIT_BOX_TOP_LOOP` / `FREECAD_UNIT_BOX_CORNER_111` hard-code face indices of the
  unit-box constructor; any reordering of `new_box_nurbs_faces` output silently invalidates
  the oracle mapping (no cross-check against `cube_solid_edge_catalog`).
- Baseline drift note: the split gate expected 789 passed / 8 ignored; current master
  (38e94ed) is 793 / 8 (tests added by c82d74a/b494480). Split preserved the count exactly
  (73 `#[test]`s in nurbs_fillet before and after; public symbol parity diff clean).
