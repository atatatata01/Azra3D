# PROMPT FOR GROK — Trimmed NURBS surfaces (the Plasticity model)

**Owner-chosen 2026-07-30.** Read `PLAN_FOR_GROK_NURBS.md` §RULES first — R1–R16 all still apply.
This document supersedes the "Option A — split the side face" suggestion. **Do not split faces.**

---

## Why this, and why it is smaller than it sounds

The fillet gap you are chasing is **not a coding slip — it is a representation limit.** Cutting a
rounded corner out of a square makes a **five-sided** region. A tensor-product NURBS patch has a
rectangular parameter domain, so it is always four-sided. **No control-point placement can ever
represent it.** `update_adjacent_faces_at_endpoint` relocating a CP to `cylinder_end_arc_mid` makes
the boundary a straight chamfer against a circular arc — the lens-shaped gap between them is the dark
wedge in the screenshots, and it exists at *every* radius. Small radii only hide it.

**Do NOT clamp the radius to make it invisible.** That is a documented visual lie (No Hacks rule).

### Scope reduction — read this before estimating

Trimming is needed in **far fewer places** than "a trimmed CAD kernel" implies:

| Surface | Needs trim? | Why |
|---|---|---|
| Cylinder (fillet along an edge) | **No** | a rectangular patch already |
| Spherical octant (corner) | **No** | a rectangular patch already |
| The two faces **adjacent** to a filleted edge | **No** | the shrink translates one edge — still a rectangle |
| The faces **perpendicular** to a filleted edge | **YES** | these get the quarter-circle corner notch |

So T3 changes one code path — the side-face notch — and deletes
`update_adjacent_faces_at_endpoint`. Everything already exact stays exact.

---

## The data model

A trim loop lives in the surface's **(u,v) parameter space**, not in 3D. That is what makes it robust:
the surface stays a clean tensor-product patch and the boundary is a separate, exact 2D curve.

```rust
/// Parameter-space boundary of a face. Outer loop CCW, holes CW (standard convention).
pub struct TrimLoop {
    pub curves: Vec<TrimCurve2d>,   // head-to-tail, closed
    pub is_outer: bool,
}

/// Exact 2D curve in (u,v). Line and Arc cover 100% of the fillet cases.
pub enum TrimCurve2d {
    Line { a: Vec2, b: Vec2 },
    /// Rational quadratic arc — EXACT. Weights (1, cos(Δ/2), 1).
    Arc  { centre: Vec2, radius: f32, t0: f32, t1: f32 },
    /// Escape hatch for later (loft/boolean). Do NOT use for fillets.
    Nurbs2d { cps: Vec<Vec3>, knots: Vec<f32>, degree: u8 },
}
```

`NurbsFace` gains `trims: Vec<TrimLoop>`. **Empty `trims` = untrimmed = the full patch.** That keeps
every existing face valid with zero migration, and it is the default.

**Serialization:** `trims` is authored state and must persist. It rides the same `NurbsSolid` payload
(bincode tag 12, appended after `SdfObject` — R13). Adding a field to `NurbsFace` is safe **only
because `NurbsSolid` is brand new and no `.fl4` contains one yet.** Land it before any save ships with
solids in it, or it becomes a format break.

---

## Phases — each with a gate that must FAIL before the fix

### T1 — Data model + serialization

Add `TrimLoop` / `TrimCurve2d` / `NurbsFace.trims`. Nothing consumes them yet.

**Gate:** a face with a hand-built square trim loop round-trips through save→load byte-identically.
Sabotage: perturb one `Vec2`, confirm the test fails.

### T2 — Tessellation against trims ★ the hard part, do it alone

The display mesh must cover **only** the trimmed region.

* Flatten each `TrimCurve2d` into a (u,v) polyline at a tolerance derived from **3D** chord error, not
  parameter error — a fixed parameter step gives visibly wrong density on a stretched patch.
* Constrained triangulation of the parameter domain against the loops (outer keeps, inner removes),
  then evaluate each 2D vertex through `surface.point(u,v)` to 3D.
* Untrimmed faces (`trims.is_empty()`) must keep the **existing** fast path untouched — no regression
  for the 99% case.

**Gates (all three):**
1. **Area** — tessellated 3D area of a trimmed unit square with a known corner arc removed matches the
   analytic value within 1 %.
2. **No leakage** — no triangle centroid falls outside the trim region.
3. **Untrimmed unchanged** — a face with empty `trims` produces a byte-identical mesh to today.

Sabotage each: invert a loop's winding, confirm gate 1 or 2 fails.

### T3 — Fillet emits trims instead of moving control points

Now the payoff. In `apply_edges_fillet_batch`:

* **Delete `update_adjacent_faces_at_endpoint` and `relocate_cps_at`** for this path. Do not keep them
  "just in case" — a second path that produces a chamfer is the bug returning.
* For each perpendicular side face at a filleted edge endpoint, **add an `Arc` trim** in that face's
  (u,v) space with the exact quarter-circle. The arc is exact; no approximation enters anywhere.
* Keep the existing shrink for the two adjacent faces — translating one edge keeps them rectangular,
  so they need no trim.
* Keep the cylinder and octant exactly as they are. They are already correct.

**Gates — these two are the whole point, and both fail on today's code:**

1. **Watertight seam** — for every shared boundary between two faces, sample N=64 points along it on
   *both* surfaces; assert max deviation < 1e-4. **This is the test that catches the chord-vs-arc gap.**
2. **Closure** — no trimmed cylinder end may be left with a free boundary; every one must be adjacent
   to a patch or a face. **This catches the missing corner patch** (the earlier screenshot).

Run both at r = 0.05, 0.2, and near `max_fillet_radius`. **The bug scales with radius — a
small-radius-only test proves nothing**, which is exactly how it survived this long.

### T4 — Picking and selection respect trims

Clicking inside a trimmed-away region must **not** hit that face.

**Gate:** ray through the notch region returns no hit on the side face. Sabotage: ignore trims in the
picker, confirm it fails. *(Selection breaking is a P0 regression — Feature Dependency Audit.)*

---

## What NOT to do

* **Do not split faces into sub-patches.** That was the alternative design; the owner chose trimming.
  Mixing both leaves two representations of the same notch.
* **Do not approximate the arc with more control points.** It converges, never closes, and the gap
  returns at larger radii.
* **Do not clamp the fillet radius** to hide the seam.
* **Do not trim the cylinder or octant patches.** They are rectangular and already exact.
* **Do not build boolean-on-trims yet.** Booleans are §6 and come later; T1–T4 must not assume them,
  but `Nurbs2d` exists in the enum so they will not need a format change.

---

## Definition of done

1. `cargo build -p app`; `cargo test -p core_math` green.
2. All T1–T4 gates green **and each one sabotage-checked** — revert the fix, confirm FAIL, restore.
3. A cube with all 12 edges filleted at three radii shows **no gap, no dark wedge, no crease**, and
   passes watertight-seam + closure.
4. `PLAN_FOR_GROK_NURBS.md` execution-status table updated; every dead end appended to its §7.
5. One commit per phase, each stating what was **verified** — not what was written (R15).
