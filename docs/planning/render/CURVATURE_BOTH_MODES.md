# Curvature needs BOTH modes — planar bake AND per-vertex

**Date:** 2026-07-29
**Owner report:** "the curvature generator is baking on UV even if set to Vertex" → then: **"we need
both modes."**
**Status:** Phase 1 correct. Phase 2 (AABB-face splat) measured broken (22.8 % vs planar 81.8 %).
**Phase 3 landed:** volume-stratified surface seeds + planar hole-fill; gate requires coverage
≥ 0.8× planar (not difference-only). UI: disabled Texture when no UVs.

**Secondary finding — why toggling the control looks inert on the owner's body.** In
`layer_ui.rs:1115` the **"Texture" option is only rendered when `gen.has_uvs`**, and `:1161` force-sets
the rung to `VertexCache` every frame when `!has_uvs`. On an SDF/DC body (no UVs) the combo therefore
offers **only "Vertex"**, and the doc is already pinned there. So switching the control changes nothing
— correct behaviour given the no-UV rule, but indistinguishable from a dead control. Once Phase 3 makes
Vertex actually render, this resolves itself; until then the combo should say *why* it is locked (a
disabled "Texture" entry with a "needs UVs" hover beats an absent one).

---

## 1. The report is accurate. Here is the exact line

`lower.rs:1034-1037`, added by P2 (`527dba8`):

```rust
// P2 (HANDOFF / ADR Decision 1): VertexCache no longer skips the bake chain.
// The G-buffer inline composite is deleted — curvature MUST land in the map
// via SdfCurvature/Cavity like Auto. Direction was always texture-only.
// (Panel may still label the rung "Vertex"; lowering always wires the bake.)
```

That last line is the bug as the owner experiences it: **the panel still says "Vertex" and lowering
ignores it.** The rung is read (`lower.rs:1030`) and then not acted on.

And the bake domain really is planar/UV-like: `bake_node_graph_maps_res` walks a flat `res × res`
grid, and the curvature kernel runs in `SEED_PLANAR` mode — rays fired from a plane across the body
AABB, not one sample per mesh vertex.

So a control that says "Vertex" produces a UV-projected result. That is a lying control, which is
the class of defect this project has been fixing all session.

---

## 2. Why P2 did it — and where the reasoning was too broad

P2 was implementing ADR-001 Decision 1, and Decision 1 is correct: **the bake must be the single
compositor**, because PT samples the baked texture array and anything outside the bake is invisible
in PT.

The step too far was concluding that *therefore per-vertex evaluation must be deleted*. Those are
different things:

* **Who COMPOSITES layers** → must be one owner (the bake). Decision 1. Still right.
* **How a generator SAMPLES the surface** → planar projection vs per-vertex. Orthogonal.

Per-vertex curvature is not a second compositor. It is a different **sampling domain** feeding the
same compositor. Deleting it removed a capability the ADR never asked to remove.

---

## 3. The good news: the machinery is intact

Only the *consumer* was deleted, not the implementation:

| Piece | Status |
|---|---|
| `SEED_POSITIONS` mode (per-vertex seeding) | **present** — 5 references in `gpu_curvature.rs` |
| `encode_curvature_vertex_dispatch` | **present** |
| `poll_vertex_curvature` | **present** — 8 references in `node_graph_bake.rs` |
| G-buffer `vertex_curv` carry (`@location(14)`, `curv_vertex[idx]`) | **present** |
| The inline composite that consumed it | **deleted** (correctly) |

`gbuffer.wgsl:678` even says it out loud: *"vertex_curv may still be bound for tooling, but they must
not re-composite over the bake."*

So restoring both modes is **wiring, not rebuilding**.

---

## 4. Why both modes genuinely matter (not just parity)

They fail in opposite places, which is exactly why a single mode cannot serve:

**Planar bake wins when:**
* the result must exist as a texture — export, PT, any baked deliverable
* you want to paint/adjust the curvature map afterwards
* the surface is UV-mapped or box-projectable without severe stretch

**Per-vertex wins when:**
* the geometry has **no usable UVs** — SDF/DC meshes and voxel output do not have them, which is
  most of what this app produces
* the body is dense or organic, where planar projection smears across folds and the seam is visible
* you want curvature to follow the *actual surface* rather than a projection of it

The owner's own earlier report is the evidence: *"shapes are bad at edges in the middle"* — that is
planar projection failing across a boolean seam, and it is precisely what a per-vertex domain avoids.

---

## 5. Proposed design

### The rung stops being a compositor choice and becomes a SAMPLING DOMAIN choice

`GeneratorRung` currently reads as "who evaluates this". It should read as "how is the surface
sampled", with the bake owning composition in both cases:

| Rung | Sampling | Composited by | Works in PT |
|---|---|---|---|
| **Texture** (planar) | `SEED_PLANAR` over the AABB → `res × res` map | bake | yes |
| **Vertex** | `SEED_POSITIONS`, one sample per DC vertex | bake | yes — see §6 |
| **Auto** | pick Vertex when the body has no usable UVs, else Texture | bake | yes |

`Auto` is worth keeping and is arguably what most users should sit on: the existing
`normalize_generator_layers` already forces Vertex when `!has_uvs` (`lower.rs:697`), so the
"no UVs → vertex" rule is written and tested. It just needs to survive lowering.

### The one real engineering question

**A per-vertex result must still reach the baked map**, or Decision 1 is violated and PT loses it.
Two options:

**(a) Rasterise vertex curvature into the map.** After the vertex dispatch, splat per-vertex scalars
into the `res × res` domain using the body's UV or box projection, then composite as usual.
*Cost:* needs a scatter pass; quality limited by the same projection at the *storage* step — but the
MEASUREMENT is per-vertex, which is where the seam artifacts come from. This is the honest fix.

**(b) Keep vertex as a display-only preview** and bake planar for export.
*Cost:* rejected — it reintroduces "looks different in PT", the exact problem Decision 1 exists to
prevent.

**Recommend (a).** The measurement domain is what fixes the seam artifacts; the storage domain only
limits resolution, which is a knob (`bake_res`) rather than a correctness bug.

---

## 6. Verification this needs

* **The rung must be observable.** A test asserting that a Vertex-rung layer lowers to a
  `SEED_POSITIONS` dispatch and a Texture-rung layer lowers to `SEED_PLANAR`. Today the rung is read
  and dropped, and nothing caught it — the owner did.
* **Pixel proof, both modes.** Per the ADR's standard: a body with a boolean seam should look
  *different* between Vertex and Texture rungs. If the soak shows identical pixels, the rung is
  still being ignored.
* **PT parity for the Vertex rung specifically.** That is the constraint that killed the old vertex
  path; option (a) has to demonstrate it rather than assert it.

---

## 7. ADR amendment needed

ADR-001 Decision 1 should be sharpened to say what it actually meant:

> **The bake is the single COMPOSITOR.** This constrains *who combines layers*. It does **not**
> constrain how a generator samples the surface — planar and per-vertex sampling are both valid
> domains feeding the same compositor, and both must land in the baked map so PT sees them.

Without that clarification, the next reader draws the same conclusion P2 did.

---

## 8. Implementation log

### Phase 1 (landed) — stop the lying control

* `lower.rs` content + mask emitters re-tag VertexCache Curvature/Cavity with trailing `v`.
* Both rungs still wire SdfCurvature/Cavity into the bake chain (no dual compositor).
* Gates: `p7_rung_switch_determinism_lowering_identical` (tags differ; both wired),
  `owner_curvature_vertex_rung_on_base_color_produces_keys` (bake keys + vertex keys).
* `poll_vertex_curvature` can discover Vertex layers again via `vertex_curv_keys_for_graph`.

### Phase 2 (landed) — option (a) splat

* `build_vertex_splat_curv_grid`: dense AABB-face surface samples → κ → normal-weighted box
  splat into the planar bake domain.
* `prepare_vertex_splat_caches` runs at the start of `bake_node_graph_maps_res` for Vertex keys.
* `sdf_curvature_masks_ex(..., vertex_mode)` reads `vertex_curv_caches` when the node tag ends
  in `v`.
* Gate: splat grid max-delta vs planar multi-seed > 0.02 on carved-box repro.

### Phase 3 — NO LONGER OPTIONAL. Phase 2 does not work, and I measured why.

**Owner 2026-07-29: "the curvature set in Vertex still does not work."** They are right. Phase 2 landed
and is wired correctly end-to-end, but the grid it produces is mostly empty.

#### Measurement (owner's own `owner_repro_body`, res=64, curv channel)

| Path | Texels with a non-zero value |
|---|---|
| **Planar** (`sdf_curvature_masks`, the working mode) | **81.8 %** (3352/4096) |
| **Vertex** (`build_vertex_splat_curv_grid` → reader with `vertex_mode=true`) | **22.8 %** (934/4096) |

**77 % of the Vertex map is exactly 0.0.** On a curvature mask zero means "no curvature", so over
three-quarters of the surface renders as *unmasked* — which is exactly what "does not work" looks like.

The reader is innocent: it returns 22.8 % non-zero from a grid that is 22.8 % non-zero, and the value
range is healthy (`[0.0, 0.985]`). The defect is entirely in the **writer**.

#### Root cause — and it is NOT seed density

`build_vertex_splat_curv_grid` seeds from **six outward AABB faces** (`face_seed`, one ray per lattice
cell, `s = (res/2).clamp(24, 96)`). My first hypothesis was that `s` was simply too low. I tested it by
making `s` overridable and sweeping it:

| seeds per face axis `s` | coverage |
|---|---|
| 24 | 17.0 % |
| 64 | 26.1 % |
| 192 | 32.3 % |
| 384 | 34.9 % |

**16× the seeds buys 17 % → 35 %, and it is clearly asymptoting near ~35 %** — less than half of
planar's 81.8 %. So density is not the bug and no amount of it will fix this. Throwing seeds at it just
makes a wrong answer slower (and this is the cost path the owner already objected to in
`BAKE_COST_STRATEGY.md`).

The real cause is **structural**: a ray fired inward from an AABB face can only ever find surface that
is *visible from outside the box*. Every concave pocket, boolean seam interior, overhang, and
self-occluded region receives **no seed at all**, so its texels are never written. That is precisely
the geometry curvature masks exist to shade — and precisely the geometry the owner is testing on
(a carved/booleaned body).

Note the irony: §4 of this document argues per-vertex wins *because* planar projection "smears across
folds". Phase 2's implementation samples via **six planar projections**, so it inherited the very
limitation it was meant to remove, and then made it worse by only keeping what those projections could
see.

#### What Phase 3 must do (now the only viable route)

Use the **real mesh vertices**, which is what the rung has always claimed and what the machinery
already supports:

* `SEED_POSITIONS` mode exists in `gpu_curvature.rs`, `encode_curvature_vertex_dispatch` exists, and
  `poll_vertex_curvature` exists — all listed as **present** in §3 of this doc.
* DC vertices lie *on* the surface, including inside concavities, so coverage becomes a function of
  mesh density rather than of AABB visibility. No occlusion blind spot by construction.
* Grok's O1 indirect dispatch (`4d663eb`) just made this cheap — the dispatch now runs over the live
  vertex count instead of `vert_cap`, which was the 10–137× over-dispatch that made the vertex path
  expensive in the first place.

So the pieces are in place; Phase 2 substituted an AABB-face approximation for them and that
approximation has a hard ceiling.

#### The gate that let this ship — worth fixing, it is the reusable lesson

`p2_vertex_splat_grid_differs_from_planar_cpu_on_carved_box` asserts the vertex grid **differs** from
the planar grid by more than 0.02. A grid that is 77 % zeros differs from planar enormously, so the
gate passes *because* the output is broken. It tests "not identical", which is satisfied by garbage.

**Any replacement gate must assert coverage, not difference:**

```
vertex_coverage >= 0.8 * planar_coverage      // fills the surface planar fills
```

plus the existing "differs from planar" check (to prove the sampling domain really changed). Difference
alone is a null hypothesis, not a spec — the same trap as the earlier sabotage-passing tests logged in
this project.
