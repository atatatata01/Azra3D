# Bake cost strategy — the owner is right, and the biggest win is not where I proposed

**Date:** 2026-07-29
**Owner:** *"A per-vertex result must still land in the baked map — this sounds expensive. Just the
vertex curvature is slow; if we bake it, it will be even slower. Can we also optimize the baking
process by separating the image into squares and only rebaking what changed? For a 4K texture maybe
the live change just bakes 256 resolution or even less."*

Diagnosis + design. No code changed.

---

## 1. The owner's cost objection is correct — at high resolution

Measured against the real vertex dispatch width (`vert_cap = 2,048,384`, ~172 MB transient):

| Planar bake res | Samples | Relative to vertex work |
|---|---|---|
| 256² | 65,536 | **0.03×** |
| 512² | 262,144 | 0.13× |
| 1024² | 1,048,576 | 0.51× |
| 2048² | 4,194,304 | **2.05×** |
| 4096² | 16,777,216 | **8.19×** |

So "vertex, then also bake it" is:

* **essentially free at 256²** (3 % on top),
* roughly break-even at 1024²,
* and **8× the vertex cost at 4K** — which is exactly the objection.

My `CURVATURE_BOTH_MODES.md` §5 recommendation ("rasterise vertex curvature into the map") was
written without this number. It is still the right *architecture* — PT can only see baked data — but
presenting it as cheap was wrong. It is cheap only at preview resolutions, which is precisely why the
owner's second idea matters.

---

## 2. The bigger finding: vertex curvature is slow for a fixable reason

`vert_cap` is pool **CAPACITY**, not the real vertex count. A DC box at 128³ has on the order of
15k–50k real vertices:

| Real vertices | Wasted work |
|---|---|
| 15,000 | **137× over-dispatch** |
| 50,000 | 41× |
| 200,000 | 10× |

**The dominant cost of "vertex curvature is slow" is over-dispatch, not per-vertex maths.** Grok's
soak already flagged this ("dispatch size is vert_cap 2,048,384, not live mesh verts — worth product
follow-up"), and it is the cheapest large win available: it helps the vertex mode *and* any
bake-from-vertex path, without inventing new machinery.

**Why it was left:** the true vertex count is GPU-only (`resident.rs` states "NO mesh readback" as a
contract), so it cannot simply be read back. The fix is an **indirect dispatch** — the GPU writes the
count and the dispatch reads it — not a readback. That keeps the no-stall contract intact.

**Recommendation: do this FIRST.** A 10–137× reduction on the mode the owner already uses beats any
tiling scheme, and it makes the bake-from-vertex path affordable rather than requiring it to be
avoided.

---

## 3. The tiling idea: correct, and the precedent already exists

> *"separating the image in squares and we only rebake what changed"*

This is right, and the codebase already does exactly this **one layer down**: the SDF *volume* bake
has `dirty_regions` per body, and partial bakes update only the dirty sub-region
(`bake_dispatch.rs:332/356/406/526`).

The **material map** bake has no equivalent — verified: no tile/region/partial concept anywhere in
`node_graph_bake.rs`'s map path. Every edit re-bakes the full `res × res` frame.

So this is not a new idea to evaluate; it is an existing, proven pattern that the material bake never
received. That materially lowers the risk.

### What makes it non-trivial (worth knowing up front)

* **Procedural noise is not spatially local in the way a paint stroke is.** Changing a noise *scale*
  changes every pixel; changing a *layer's mask* may change only part. So the dirty-rect must be
  derived from *what the user edited*, not assumed. A conservative rule (any generator param change ⇒
  full frame; mask/paint edits ⇒ rect) captures most of the value and never produces a wrong image.
* **Tile seams.** Anything with a spatial filter across pixels — blur, the normal→height integration
  — needs an overlap margin, or tile boundaries become visible. The height integrator in particular
  relaxes across neighbours and would band at seams.
* **Correctness gate.** A tiled bake must produce bit-identical output to a full bake for the same
  input. That is a cheap and decisive test, and it is the one that stops silent corruption.

---

## 4. The live/final resolution split: already partly built

> *"for a 4K texture maybe the live change just bakes 256 resolution or even less"*

This exists in the shape the owner describes, and Grok extended it:

* `BAKE_INTERACTIVE_RES = 256` — live bakes already cap at 256² while interacting
  (`node_graph_bake.rs:170`), except for image-texture graphs.
* Grok's `bake_res` panel control (128 / 256 / 512 / 1024) makes the working resolution explicit.

**What is missing is the top end.** `MAP_RES = 1024` is hardcoded at **59 sites**, so 2K/4K final
bakes are not reachable yet — that is P4 in the feature-complete plan. The live/final *split* is
already the right architecture; only the ceiling needs raising.

---

## 5. Recommended order (revised by the numbers)

1. **Indirect dispatch for vertex curvature** — 10–137× on work the owner does today. Biggest win,
   no new subsystem, and it makes everything downstream cheaper.
2. **Raise `MAP_RES` from a constant to a parameter** (P4) — needed for 4K at all, and it is what
   makes tiling *worth* doing.
3. **Tile the material bake with dirty rects** — follow the SDF volume bake's existing pattern.
   Gate on bit-identical output versus a full bake.
4. **Then** decide how vertex curvature reaches the map. After (1) and (3) the cost objection may
   have dissolved: a per-vertex measurement rasterised into a *dirty tile* at *live* resolution is a
   fraction of today's full-frame 1024² bake.

The ordering matters: doing (4) first is what looked expensive. Doing (1) and (3) first is what makes
(4) affordable — and (1) alone is likely to be the difference the owner actually feels.
