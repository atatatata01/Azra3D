# Material system → Substance-class authoring: state + plan

**Date:** 2026-07-29
**Goal (owner, corrected):** author **complex procedural textures that bake later** — everything
Substance Designer / Painter and Photoshop give you: generators, full layer blending, masks,
multi-material looks. **"Live" means live preview *feeling*** (scrub a noise scale, see it in the
viewport) — **NOT** that the graph must evaluate per-frame in a game engine.

That distinction was mis-read earlier in the project and it shaped the architecture toward
game-engine budgets. This plan corrects for it.

---

## 1. What Grok has built (uncommitted, 1269 insertions across 8 files)

Reviewed in the working tree. Substantial and mostly in the right direction:

| Piece | Assessment |
|---|---|
| **Dual-plane 16-bit** (`upload_map_16`, hi bytes at `slot`, lo at `slot+1`, `reconstruct_u16_channel`) | **Good call.** Avoids adding a second `R16` texture array, which would have meant threading a new binding through 26 WGSL references and 7 BGL sites — the "adding a binding requires 6+ BGL edits or bakes fail silently" hazard. |
| **G-buffer → `Rgba16Float`** | Correct and necessary; 8-bit albedo was a hard banding floor. |
| **`bake_res` in the panel** (128/256/512/1024) | Right shape — resolution becomes a *parameter*. Ceiling is still 1024; see §3. |
| **`ChannelEvalMode` (Live / Bake), user-chosen not inferred** | Good. Matches the corrected goal: Live is a preview tier, not a different feature set. |
| **EXR / HDR / float-TIFF height decode** | Real precision win for displacement. |

### Two regressions to resolve before it lands

1. **`new_generator_layer` reintroduces the Normal-blend clobber.** The default became
   `if vertex { Normal } else { Multiply }`. `gbuffer.wgsl` still has **no `blend == 0` arm** —
   verified — so blend 0 falls through to `blended = top`, which *replaces everything beneath*.
   That is exactly the owner-reported "Mult noise over curvature does nothing" (root cause in
   `84bb182`, fixed in `f6e13ae`). Test `new_generator_layer_defaults_to_multiply_not_normal`
   fails.
   **Fix either way, but pick deliberately:** keep Multiply as the default, *or* add a real
   `blend == 0u` "over" arm so Normal means over instead of replace. Not both by accident.

2. **GPU/CPU bake parity broken:** `gpu_bake_matches_cpu_on_procedural_graph` reports albedo
   mean |Δ| = **35.5/255** against a tolerance of 2. That test's own message is right — a *mean*
   shift is a real port bug (wrong constant / fade curve / encoder), not fp noise. Most likely the
   sRGB encode path the dual-plane work touched.

Everything else in `-p render` (63/63) and the rest of `-p app` material/curvature (117/119) is
green.

---

## 2. The architectural problem, stated once

Every material bug this session shares one cause: **four representations of a layer stack with no
single owner of blend semantics.**

```
panel doc (MaterialPanelDoc)  →  node graph  →  baked flat maps  →  G-buffer composite
```

Each hand-off re-implements or drops semantics. Evidence, all diagnosed today:

* Mult noise over curvature → discarded by the G-buffer's `blended = top`
* `Normal` blend silently means *replace*, not *over*
* Image mask → renders as a content layer
* 8/16-bit was authoring-only (now being fixed)
* Per-layer alpha **impossible** — the bake flattens, so "absent" and "black" are the same byte

Five blend modes exist (Normal / Multiply / Add / Overlay / Screen) implemented in **four places**.
Photoshop has ~27. Adding Soft Light today means four edits and four chances to drop it — which is
precisely how `blended = top` shipped.

---

## 3. Gaps to Substance-class, measured

| Ceiling today | Needed | Note |
|---|---|---|
| `MAP_RES = 1024`, hardcoded at **59 sites** | 2K / 4K bake | Grok's `bake_res` is the working res; `MAP_RES` is still the upsample target |
| 8-bit array (dual-plane 16 in progress) | 16-bit throughout | Grok's approach is sound |
| **5** blend modes × 4 implementations | ~27, declared **once** | The single highest-leverage change |
| Layers flattened at bake | Per-layer alpha | Blocks masking-removes vs blend-to-black |
| One material per body | Multi-material + masks | Owner asked explicitly; **separate decision** |
| Generators: Curvature, Cavity, Direction | + AO, Thickness, Position | AO is declared with **no backend** (eval reads a synthetic demo field) |

---

## 4. Plan, ordered by leverage

**P0 — land what exists, cleanly.** Resolve the two regressions in §1, commit Grok's work. 1269
uncommitted lines in a two-agent tree is the largest current risk.

**P1 — one compositor owns blend semantics.** A blend mode becomes a row in a table, evaluated in
ONE place; the other three sites consume its result. This is what stops the bug *class*, and it is
a precondition for both alpha and the remaining ~22 modes. Requires an ADR first: it changes how
existing Normal-blend materials render.

**P2 — per-layer alpha through the bake.** Composite with real over-operators, carrying alpha per
layer. Unblocks masking-removes vs blend-to-black. Depends on P1.

**P3 — resolution/bit-depth as parameters, not constants.** Live preview stays small; final bake
goes 2K/4K 16-bit. Same compositor, different tier — that is what makes scrubbing feel live *and*
the final production-grade.

**P4 — generator completeness.** AO (real backend), Thickness, Position. Additive, no architectural
risk.

**P5 — multi-material.** Layer above the compositor. Own ADR; do not conflate with P1.

### Performance stance (corrected)

Bake cost is **not** a frame-budget problem — it is a *background* problem. The rules that follow:

* Live preview: capped resolution, capped octaves. Feel > fidelity. Already true.
* Final bake: **no caps**. Off the frame thread, progress reported. `df44281` (frame-budget cost
  hint) is the pattern — the fix for expensive work is scheduling it honestly, not shrinking it.
* Never make a *visual* compromise for frame budget in the final bake. That was the original
  mis-read.

---

## 5. Coordination

Grok owns `soak.rs` and the material panel work in flight. This document reviews rather than edits
that work — the two regressions above are for whoever lands it, not silent fixes on top.

---

## 6. Panel expressiveness — the limit the owner hit (2026-07-29)

> "I only have access to one mask — imagine I want to drive the curvature radius with noise, or
> mask the curvature mask with noise. Our current UI does not let us do so."

**Both limits are in the PANEL DATA MODEL, not the engine.** Verified:

```rust
pub struct Layer {
    pub content: LayerContent,
    pub mask: Option<LayerContent>,   // ONE mask; LayerContent is a LEAF
}
LayerContent::Generator { radius: f32, sharpness: f32, ... }   // plain scalars
```

* `mask: Option<LayerContent>` → exactly one mask, and because `LayerContent` is a leaf, **a mask
  cannot itself carry a mask**. No mask-of-a-mask, no stacked masks, no blend between masks.
* Generator params are `f32`, so **nothing can drive them**.

**But the node graph already supports both.** `SdfCurvature::inputs()` declares:

```
Position (vec3) · Epsilon (= the radius) · GrowShrink · Band · Contrast · Strength
```

Those are real **input sockets** — connectable. The panel simply writes `value_f32` constants into
them (`lower.rs:1122` writes radius into socket 1, Soft/Hard into socket 4). So "drive the curvature
radius with noise" is *already expressible in the graph* and merely unreachable from the panel.

Gated by `curvature_node_exposes_drivable_sockets_the_panel_cannot_reach` so the next reader treats
this as **panel-limited, not engine-limited**.

### What this changes about P1

P1 is no longer just "one owner of blend semantics" — it is **the panel's model must become as
expressive as the graph it lowers to.** Concretely:

1. **Masks become a stack, not an `Option`.** `mask: Vec<MaskLayer>` where a `MaskLayer` has its own
   content, blend mode, opacity — and recursively its own mask. That is what makes "mask the
   curvature mask with noise" expressible.
2. **Scalar params become drivable.** A generator parameter should be `Value::Const(f32)` or
   `Value::Driven(LayerContent)`, lowering to a socket **connection** instead of a `value_f32`. The
   sockets already exist, so this is a lowering change plus UI, not new shader work.
3. **Blend modes declared once** (already stated in §2/§4) — needed because a mask stack multiplies
   the number of places a mode is applied.

This is the difference between a fixed layer panel and a Substance-class one: in Substance every
parameter is an input. The engine here is closer to that than the panel admits.

**Save-format note:** `Layer.mask` is serialized. Widening `Option<LayerContent>` →
`Vec<MaskLayer>` is a wire change on a bincode struct — it must ride the in-blob
`#[serde(default)]` side-channel pattern, and it must NOT be an in-place field retype (see
`docs/planning/sdf/REF_FL4_BACKCOMPAT_INVESTIGATION.md`: appending to a nested struct also breaks
the wire).

---

## 7. Panel ↔ node graph: the coupling question (owner 2026-07-29)

> "My goal was always to have the shader tree and node system linked — one for basic users, the
> other for advanced work."

That is the right product shape, and it is the constraint that decides §6's design. **Today the
link is one-directional and lossy**, which is why "does Photoshop/Substance behaviour even work
with the current node system" is the correct question to ask before building more panel features.

### How it works now

* The panel LOWERS to nodes. Identity is a **string label**: `mp:<kind>:<role>:<layer>`, parsed by
  `parse_panel_tag`.
* `regen_channel_subgraph` **deletes every panel-tagged node for the channel and rebuilds it**
  (`lower.rs:854-862`, "regen is idempotent"). Deterministic — same doc, same graph.
* There is already an escape hatch: `ChannelState::GraphDriven { node }` — "the channel is owned by
  a wire into the master material socket; the panel shows a badge, never a lying slider."

### The actual problem

Determinism and round-tripping are **in tension**:

* Regen is destructive, so a user who hand-edits a panel-tagged node loses it on the next panel
  edit. The panel is authoritative or the graph is — not both, for the same channel.
* Identity lives in a **parsed string**. Any structural change (a mask STACK, a driven parameter)
  needs a richer key than `kind:role:layer`, or two mask layers at the same depth collide.
* `GraphDriven` solves it by **surrender**: hand the whole channel to the graph and stop showing
  panel controls. That is honest, but it is all-or-nothing per channel — it cannot express "panel
  layers, with this one parameter driven by a node I wired myself", which is exactly the Substance
  workflow the owner wants.

### The decision to make (this is the ADR)

Three viable models, and they are genuinely different products:

1. **Panel-authoritative, graph read-only** — the graph is a *view* of the panel, never edited by
   hand. Simple, keeps determinism, and kills the "advanced work" half of the goal. **Rejected by
   the stated goal.**
2. **Per-channel ownership (today, extended)** — a channel is either panel-owned or graph-owned
   (`GraphDriven`). Cheap, already partly built, but the granularity is wrong: you cannot mix within
   a channel, so driving one generator parameter with a node means giving up the whole layer stack
   for that channel.
3. **Per-parameter ownership** — every panel control is `Const(v)` **or** `Driven(node_output)`, and
   regen preserves anything marked driven instead of deleting it. The panel becomes a *structured
   editor over the graph* rather than a generator of it. This is Substance's model, and it is what
   makes §6's two asks (mask stacks, noise-driven radius) expressible without abandoning the panel.

**(3) is the one that matches the goal**, and it has consequences worth stating up front:

* Regen must become **merge, not replace**: preserve driven sockets and user-added nodes,
  regenerate only what the panel still owns. That is the real work — bigger than the UI.
* Identity must stop being a string. A stable per-layer/per-parameter id (like `prim_uid` for
  prims — "stable IDs, not indices" is already a CLAUDE.md rule) survives reorder, insert, and
  nesting. `kind:role:layer` does not.
* `GraphDriven` stays valid as the coarse escape hatch — it just stops being the ONLY way to reach
  the graph.

### Sequencing consequence

**§6 should not be built before this is decided.** A mask stack and drivable params both need the
identity scheme and the merge semantics; building them on `kind:role:layer` + destructive regen
would mean re-doing them. So the ADR covers §6 and §7 together, and P1 becomes:

> **P1 = identity + merge semantics + one blend-mode owner**, then mask stacks and drivable
> parameters land on top of it.
