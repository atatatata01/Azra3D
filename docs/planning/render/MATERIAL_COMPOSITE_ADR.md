# ADR-001: Panel ↔ node-graph ownership, and who composites layers

**Status:** ACCEPTED (2026-07-29) — Decision 1 = B (bake is the compositor);
Decision 2 = C (per-parameter ownership), confirmed by owner.
Remaining open item: how PT handles normal maps (see §Open questions).
**Date:** 2026-07-29
**Deciders:** owner
**Context docs:** `MATERIAL_FEATURE_COMPLETE_PLAN.md`, `MATERIAL_SUBSTANCE_PARITY_PLAN.md`

---

## Context

### The goal (owner's words)

* Author **complex procedural textures that bake later** — Substance-class generators, full layer
  blending, masks, multi-material looks.
* **"Live" = live preview *feeling*** (scrub a noise scale, see it move). NOT per-frame evaluation
  in a game engine.
* **Shader tree and node system LINKED** — panel for basic users, nodes for advanced work, on the
  same material.
* **Everything must work in the path tracer.**

### What forces a decision now

The owner hit two walls that are not bugs — they are the data model:

1. `Layer.mask: Option<LayerContent>` → **one** mask, and `LayerContent` is a leaf, so **a mask
   cannot carry a mask**. "Mask the curvature mask with noise" is structurally impossible.
2. Generator params are plain `f32`, so **nothing can drive them**. "Drive the curvature radius with
   noise" is impossible from the panel.

**Both are panel-side.** The node graph already exposes what is needed: `SdfCurvature::inputs()`
declares `Position · Epsilon (the radius) · GrowShrink · Band · Contrast · Strength` as real,
connectable input sockets. The panel just writes `value_f32` constants into them
(`lower.rs:1122`). Gated by
`curvature_node_exposes_drivable_sockets_the_panel_cannot_reach`.

So the capability exists and the panel cannot reach it. Widening the panel means deciding **who owns
what** between panel and graph — which is this ADR.

### Two facts that constrain the answer

**Regen is destructive.** `regen_channel_subgraph` deletes every panel-tagged node for the channel
and rebuilds it (`lower.rs:854-862`). Deterministic — same doc, same graph — but a user's hand-edit
to a panel-tagged node is lost on the next panel edit.

**Identity is a parsed string.** `mp:<kind>:<role>:<layer>`, via `parse_panel_tag`. That cannot
survive reorder, insert, or nesting: two mask layers at the same depth collide.

### One fact that simplifies it

**PT samples the material texture array**, and the GPU bake writes into that array via
`NodeGraph::generate_wgsl()`. So **anything that goes through the bake works in PT for free.** What
does *not* work in PT is what is evaluated live outside it — the vertex-rung curvature carry and the
G-buffer's inline layer composite — because PT never runs the G-buffer fragment path.

"Everything must work in PT" is therefore not a porting project. It is an argument about **who
composites**.

---

## Decision 1 — Who composites layers?

### Options

**A. Keep the G-buffer's inline composite (status quo).** Layers composite in `fs_main`, and the
bake composites separately for the map path.
*Cost:* two implementations that already disagree. This is the direct cause of the `blended = top`
clobber, of Normal silently meaning *replace*, and of PT never matching Hybrid on a layered
material. Per-layer alpha stays impossible.

**B. The BAKE is the single compositor. The G-buffer and PT both sample its result.**
*Cost:* the G-buffer composite is deleted; live preview must go through a (cheaper) bake rather than
inline shader math. Existing Normal-blend materials change appearance.
*Benefit:* PT parity is structural, not maintained. One place owns blend modes. Per-layer alpha
becomes possible because the compositor can carry alpha; a flattened map cannot.

### Recommendation: **B**

The owner's "everything must work in PT" requirement makes A permanently expensive — every feature
would need doing twice, and the two would keep drifting (they already have). B makes PT parity a
property of the architecture instead of a maintenance task.

**Non-negotiable rule that follows:** *do not grow a second Live compositor.* Live may run at lower
resolution / fewer octaves, but it must be the SAME compositor. A separate Live path is exactly how
the inline G-buffer composite came to disagree with the bake.

Note `ChannelEvalMode` (Live / Bake) already exists as an explicit user control — **keep it**. It
selects a quality tier, not a different implementation.

---

## Decision 2 — Panel/graph ownership granularity

### Options

**A. Panel-authoritative, graph read-only.** The graph is a *view*; never hand-edited.
*Rejected by the goal* — it deletes the "advanced work" half.

**B. Per-CHANNEL ownership (today, extended).** A channel is either panel-owned or graph-owned
(`ChannelState::GraphDriven { node }` already implements this: "the channel is owned by a wire into
the master material socket — the panel shows a badge, never a lying slider").
*Cost:* wrong granularity. To drive ONE generator parameter with a node you surrender the whole
layer stack for that channel. Cannot express "panel layers, with this one parameter driven".
*Benefit:* cheap, already partly built, keeps regen destructive and simple.

**C. Per-PARAMETER ownership.** Every panel control is `Const(v)` **or** `Driven(node_output)`. The
panel becomes a *structured editor over the graph* rather than a generator of it.
*Cost:* regen must become **merge, not replace** — preserve driven sockets and user-added nodes,
regenerate only what the panel still owns. Identity must become a stable id. This is the real work,
and it is larger than the UI.
*Benefit:* the Substance model. Both of the owner's blocked cases become expressible without giving
up the panel. Sockets already exist, so it is lowering + UI, not shader work.

### Recommendation: **C**

It is the only option that satisfies "one for basic users, the other for advanced work" on the same
material. B is cheaper but structurally cannot express the owner's stated workflow.

**`GraphDriven` stays valid** as the coarse escape hatch — it just stops being the only way in.

---

## Consequences

### Required before any feature work

1. **Stable IDs replace the parsed string.** Every layer and every parameter gets a stable id — the
   `prim_uid` pattern; *"stable IDs, not indices"* is already a CLAUDE.md rule. Survives reorder,
   insert, nesting.
2. **Regen becomes merge.** Preserve user-added nodes and any socket marked `Driven`; regenerate only
   panel-owned structure.
3. **Blend modes evaluated in one place.** Note the honest starting point: `MixBlend` (`ops.rs:292`)
   is already a single *definition*, evaluated in **two** places — CPU bake
   (`node_graph_bake.rs:2159`) and WGSL codegen (`graph.rs:1118`) — with the panel mapping onto it.
   So the full Photoshop set (~27) is one enum plus two evaluators: bounded and mechanical. The
   genuine duplication to delete is the G-buffer's inline composite.

### Save format (hard constraint)

`Layer` is bincode-serialized. Widening `mask: Option<LayerContent>` → a recursive stack, and
`radius: f32` → `Const|Driven`, are **wire changes**. They must ride the in-blob
`#[serde(default)]` side-channel. **Never** an in-place retype, and note that appending to a *nested*
struct also breaks the wire — see `docs/planning/sdf/REF_FL4_BACKCOMPAT_INVESTIGATION.md`, where that
exact trap is documented with a reproduction.

### What breaks

* **Existing Normal-blend materials change appearance** (Normal starts meaning *over* instead of
  *replace*, or the default moves to Multiply). This is the intended fix, but it is visible and
  should be called out in release notes, not discovered.
* Live preview goes through the compositor rather than inline shader math. Must be measured, not
  assumed: the target is that scrubbing still feels immediate.

### What we deliberately do NOT do

* **Multi-material** — sits above the compositor. Own ADR. Do not conflate.
* **True linear `R16` texture array** — dual-plane-in-sRGB is a workaround with a real limitation:
  on a flat colour, 8-bit and 16-bit can land on the same byte, so 16-bit helps *gradients*, not
  everything. Whether to pay for a real array (26 WGSL references, 7 BGL sites) is a separate,
  explicit choice — not part of this ADR.

---

## Verification (how we know it worked)

Every step gated on the **soak harness showing a real pixel delta**. This is not ceremony: this
project has twice shipped a feature that passed all unit tests and moved zero pixels (a curvature
cap that silently skipped every dispatch; a blend default that discarded the layer beneath).

Specific exits:

* **Decision 1:** PT and Hybrid produce pixel-comparable output for a multi-layer material.
* **Decision 2:** hand-edit a node, then edit the panel — the hand edit survives. Test-gated.
* **Features:** noise drives curvature radius (visible); a mask has its own mask (visible).

---

## Owner answers (2026-07-29)

### 1. Decision 2 = C (per-parameter) — CONFIRMED

Proceed on per-parameter ownership. Implementation order is unchanged: stable IDs → merge regen →
`Const|Driven` params, then mask stacks on top.

### 2. "What is perturbation?" — normal mapping, explained

**Normal perturbation** is what a normal map physically does. The map does not change geometry; it
stores, per texel, a *fake surface direction*. At shading time the renderer replaces the triangle's
real normal with that faked one, so lighting behaves as if the surface were bumpy while the mesh
stays flat. "Perturb" = nudge the normal away from the true geometric one.

* **Hybrid/G-buffer does this today** (`sample_mat_triplanar_normal`, the "whiteout" tangentless
  blend) — which is why normal maps look right there.
* **PT does not.** Its shading path never reads `normal_map` or `bump_map` (gated by
  `pathtracer_samples_albedo_roughness_metallic_but_not_normal_or_bump`). It samples albedo /
  roughness / metallic and uses the true geometric normal, so a normal map is simply absent.

The three options, restated concretely:

| Option | What happens | Cost |
|---|---|---|
| **(a) bake normal → height, drive displacement** | Real geometry is displaced, so PT, Hybrid, shadows, and silhouettes all see it | Conversion already landed (`35f5d09`); needs the displacement path finished. Changes silhouette — which is *more* correct, but it is geometry, not shading |
| **(b) add normal perturbation to PT** | PT reads the normal map and fakes the surface direction, exactly as Hybrid does | The correct renderer fix; needs a tangent basis or the same whiteout trick, in PT's shading path |
| **(c) accept the gap** | Normal maps stay Hybrid-only | Free, but the owner's "everything must work in PT" rules it out |

**Recommendation: (b), with (a) available as an authoring choice.** (b) is what "normal maps work in
PT" actually means, and Hybrid already proves the technique works without a tangent basis. (a) is
genuinely useful too — but it is a different feature (real displaced geometry), not a substitute, and
presenting it as one would be the kind of stand-in CLAUDE.md's No-Hacks rule forbids.

### 3. "Why will existing materials look different?" — I overstated this

**Measured rather than asserted, and the honest answer is: for most materials, they will not.**

```
Normal today        : blended = top                  (REPLACE)
Normal as "over"    : blended = mix(base, top, mag)   (OVER)
```

* **A single layer over the material base:** where the generator is fully on (`mag = 1`),
  `mix(base, top, 1.0) == top` — **identical**. They differ only in the falloff region where
  `mag < 1`, and there "over" is what a user expects: a partial mask should blend, not fully replace.
* **A layer with anything beneath it:** replace *discards* the lower layer; over lets it through by
  `(1 - mag)`. This is the owner's reported bug, so the change here is **the fix**, not a regression.

So the accurate statement is: **materials whose appearance depends on Normal-blend silently
discarding a lower layer will change** — and those are exactly the ones currently rendering wrongly.
Single-layer materials are unaffected at full strength and *improve* at the edges.

I had written "existing Normal-blend materials change appearance" as a blanket warning. That was
over-cautious to the point of being misleading, and it made a correct fix sound risky.

**Consequence for the plan:** release notes should say "layer stacks using Normal blend now composite
instead of replacing", not "materials may look different". No migration is needed.

---

## CORRECTION 2026-07-29 — "Normal should mean over" is WITHDRAWN

I attempted this change and reverted it. Two `-p render` GPU tests failed immediately, and they were
right.

`mag` is the curvature **VALUE** (0 = fully concave, 1 = fully convex), **not a coverage**. Using it
as over-coverage makes concave curvature fully transparent — it vanishes instead of painting dark.
Verified numerically: at signed curvature −1.0, `mag = 0`, so `mix(base, top, 0)` returns the base
unchanged. `vertex_curvature_signed_channel_reaches_albedo_pixel` caught exactly that.

The shader already had it right:

```wgsl
top     = mix(black, white, mag);        // curvature value AS a colour
blended = top;                           // Normal: that colour replaces the base
color   = mix(base, blended, opacity);   // coverage applied HERE, from layer opacity
```

Coverage is the layer **opacity** and its mask, and the final line already applies it. My change
double-applied coverage.

**This strengthens Decision 1 rather than weakening it.** The real defect is not that Normal
mis-blends a stack — it is that **the G-buffer has no stack at all**. `base` is the material albedo,
not "the composited layers below", and the G-buffer composites exactly ONE vertex-curvature layer
against it. Making the bake the single compositor is what gives the lower layers somewhere to exist.
No change to the blend arm could ever have fixed it.

**Consequences for the plan:**

* P2 must **delete** the inline composite, not repair its Normal arm. Leave `blended = top` alone.
* The `new_generator_layer` default (`if vertex { Normal }`) should be revisited only AFTER P2 — with
  a real compositor, Normal vs Multiply becomes a genuine authoring choice instead of a workaround
  for a missing stack.
* §"What breaks" is further reduced: with the inline composite deleted rather than re-blended, there
  is no Normal-semantics change to warn about at all.
