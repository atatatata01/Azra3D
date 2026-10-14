# Dead Modifiers Plan — 2026-08-05 (Grok work order)

**Owner report:** *"The Weighted Normal modifier does nothing"* + *"Triangulate does nothing too."*

An audit of **every** modifier type (test `diag_modifier_audit`, commit `59c6e74`) found **three
dead** and one invisible-by-construction. Only one had been reported.

Read `CLAUDE.md` before starting. This document assumes its rules rather than repeating them.

---

## The evidence

`cargo test -p app diag_modifier_audit -- --nocapture` — each modifier applied **alone** to a
cube, reporting what changed in the mesh and what changed in the **baked** output:

```
BASE cube: verts=8 faces=6 | baked verts=24 tris=12
  Subdivide: mesh 8->26v  6->24f CHANGED | baked 24->96v  12->48tri CHANGED
     Mirror: mesh 8->16v  6->12f CHANGED | baked 24->48v  12->24tri CHANGED
   Solidify: mesh 8->16v  6->12f CHANGED | baked 24->48v  12->24tri CHANGED
     Smooth: mesh 8->8v   6->6f     same | baked 24->24v  12->12tri CHANGED
Triangulate: mesh 8->8v   6->12f CHANGED | baked 24->36v  12->12tri CHANGED
   Decimate: mesh 8->8v   6->6f     same | baked 24->24v  12->12tri    SAME   <-- DEAD
      Array: mesh 8->24v  6->18f CHANGED | baked 24->72v  12->36tri CHANGED
      Bevel: mesh 8->24v  6->26f CHANGED | baked 24->96v  12->44tri CHANGED
 WeightedNormal: mesh same             | baked                        SAME   <-- DEAD
   SmoothNormal: mesh same             | baked                        SAME   <-- DEAD
  Duplicate: mesh 8->16v  6->12f CHANGED | baked 24->48v  12->24tri CHANGED
ArrayCircular: mesh 8->48v 6->36f CHANGED | baked 24->144v 12->72tri CHANGED
```

**The root cause of the two normal modifiers is already identified — do not re-derive it.**
`recompute_normals_weighted_ex` and `recompute_normals_smooth` write `HVertex.normal`, one normal
**per vertex**. Every poly display path bakes through `raster_data_ngon_with_normals`
(`core_math/src/poly_mesh.rs:366`), which computes normals from face geometry **per corner** and
never reads that field. Two representations that never meet.

---

## Gate 0 — Weighted Normal / Smooth Normal must reach the screen

**Task.** Produce **per-corner** normals and let the bake consume them.

1. **New kernel producer** in `core_math/src/half_edge/normals.rs`, alongside (not replacing)
   `recompute_normals_weighted_ex`:
   ```rust
   pub fn weighted_corner_normals(
       mesh: &HalfEdgeMesh, keep_sharp_deg: f32, mode: u8, weight: u32, threshold: f32,
   ) -> Vec<Vec3>   // indexed by HalfEdgeId (one per face corner)
   ```
   Mirror the existing weighting **exactly** — mode 0 = corner angle, 1 = face area, 2 = area ×
   angle; weight exponent `weight/50` with the `powf(1.0)` short-circuit preserved (it exists so
   weight == 50 stays bit-exact); `threshold` blends every face within the relative tolerance of
   the max instead of picking a strict first-max; `keep_sharp_deg` excludes faces past the angle.
   The per-vertex version stays for any caller that needs it; extract shared weighting rather
   than copy-pasting it (CLAUDE.md reuse rule).
   Do the same for the smooth variant.

2. **`NormalConfig`** (`poly_mesh.rs:138`) gains an authored-corner-normal channel, e.g.
   `custom_corner: Option<&[Vec3]>`. When present, the emitter uses it instead of calling
   `auto_smooth_corner_normal`.

3. **Bake side** — `poly_normal_state` (`app/src/poly/normals.rs:37`) is the ONE place the
   sidecar becomes a bake input; make it the one place this happens too. If the body's stack has
   an **enabled** `WeightedNormal` / `SmoothNormal`, compute the corner normals from the
   **display** mesh with that modifier's parameters and put them in the config.

**Decisions you must make explicitly and write down in the code:**
- **Which wins if several normal modifiers are enabled?** Recommend: the LAST enabled one, so
  stack order reads naturally. State it in a comment.
- **How does this compose with Mark Sharp and the Auto Smooth angle?** Recommend: authored
  corner normals REPLACE the angle-based fan, but Mark Sharp still splits (it is authored
  intent, and Blender's Weighted Normal has a "Keep Sharp" option for exactly this). Whatever
  you choose, say so where a reader will find it.

**Acceptance.**
1. `diag_modifier_audit` shows `WeightedNormal` and `SmoothNormal` flipping from `SAME` to
   `CHANGED`. Paste the new table.
2. A test that **changing** `weighted_mode` / `weighted_weight` / `weighted_threshold` /
   `weighted_keep_sharp_deg` changes the baked normals — not merely that the modifier does
   something. There is already `every_weighted_normal_field_changes_the_stack_hash`; that guards
   the CACHE KEY, not the output, and it passed the whole time this was broken.
3. **A body with no normal modifier bakes byte-identically to today.** Assert on the full vertex
   buffer. This is the regression that matters — every existing scene must be untouched.
4. `cargo test -p app` and `cargo test -p core_math` green.

---

## Gate 1 — Decimate is an empty arm

`PolyModifierType::Decimate => {}` in `apply_one_poly_modifier` (`app/src/poly/mesh_sync.rs`).
Not "not visible" — **not implemented**, while sitting in the Add-Modifier menu looking real.

**Owner decision required — ask before building.** Two acceptable outcomes, and doing nothing is
not one of them:
- **(a) Implement it.** Planar/limited dissolve driven by a ratio or angle parameter. Note
  `half_edge` already has `limited_dissolve(angle, only_faces, protect)` — that is the natural
  kernel, and it takes a protect set so tagged edges survive (see the 2026-08-05 cleanup work).
- **(b) Disable it in the UI** until implemented — greyed out with "not implemented yet" — so it
  cannot silently lie. Keep the enum variant: it is bincode-tagged and removing it shifts every
  later tag.

**Do NOT** leave a menu entry that runs and does nothing. That is the specific defect being fixed.

---

## Gate 2 — Triangulate is invisible, and that is a UX bug

Triangulate is **not** dead: it splits 6 quads into 12 triangles. But the display emitter already
triangulates n-gons in order to render them, so the shaded image is the same 12 triangles either
way. It shows in topology, edit mode and export — never in the viewport. The user has no way to
tell it worked.

**Task.** Make every modifier's effect legible: show a small **before → after** stat on the
modifier card (verts / faces, or tris). One line per card, from data the stack evaluation already
has. This fixes Triangulate's legibility *and* would have made all three dead modifiers obvious
the day they broke.

**Acceptance.** Adding Triangulate to a cube visibly reports `6 → 12 faces` in its card.

---

## Gate 3 — Make this class impossible to repeat

The real failure is that **nothing checked whether a modifier reaches the screen**.

**Task.** Make `diag_modifier_audit` exhaustive at COMPILE TIME: drive it from a `match` over
`PolyModifierType` that has no `_` arm, so adding a variant fails to build until it is added to
the audit. Then assert the invariant — every modifier either changes the mesh or changes the
bake — with an explicit, commented allow-list for any deliberate exception.

**Acceptance.** Adding a new `PolyModifierType` variant without touching the audit fails to
compile. A modifier that does nothing fails the test with a message naming it.

---

## Constraints and traps

- **`PolyModifierType` is bincode-tagged by position.** APPEND new variants at the END only.
  Never insert, reorder, or remove — it corrupts every existing `.fl4`.
- **Every new `PolyModifier` field must be hashed in `hash_modifier_stack_into`**
  (`app/src/poly/mesh_sync.rs`). The stack hash is half the display-cache key; an unhashed field
  makes the modifier look frozen — the slider moves and nothing happens. **This has shipped
  twice.**
- **Bit-exactness for untouched scenes.** Any change to the normal path must leave a body with no
  normal modifier and no authored normals baking byte-identically. Test it, do not assume it.
- **Build after every change:** `cargo build -p app`. Do not pile changes on a broken tree.
- **Frame Budget Rule:** per-corner normals are O(corners) per bake, which is fine — but do not
  recompute them per frame if the display mesh has not changed. The display cache key is the
  right place.

### Concurrency warning — other sessions are live in this repo

- `poly_knife.rs`, `mesh_boolean/*`, `ops_dissolve.rs`, `extrude_drag.rs`, `project/tests.rs` all
  have uncommitted work from another session. Suite failures in `mesh_ops::tests`
  (`apply_in_place_refusal_leaves_mesh_untouched`, `clean_mesh_still_refuses_slit_creating_dissolve`)
  and in `poly_knife::tests::s_straight_slice_collapses_densify_pins_to_clean_segments` are
  **theirs** — confirm before claiming a regression, and do not "fix" them.
- Stage files **explicitly**. Never `git add -A` — that mistake committed another session's
  half-finished file on 2026-08-05 and broke HEAD's test build.

### Scope

Gates 0–3 only. Do **not** touch the bevel kernel, the knife, the boolean, or the ui2 idle
gating. **Do not start the Smooth-by-Angle modifier** — that is a separate, already-scoped task
owned elsewhere.
