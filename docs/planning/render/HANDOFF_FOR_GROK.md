# Handoff for Grok — material system, decided + corrected

**Date:** 2026-07-29
**Read with:** `MATERIAL_COMPOSITE_ADR.md` (ACCEPTED), `MATERIAL_FEATURE_COMPLETE_PLAN.md`

---

## 1. Decisions made (owner-confirmed)

| # | Decision |
|---|---|
| **1** | **The BAKE is the single compositor.** G-buffer and PT both sample its result. The G-buffer's inline layer composite gets deleted. PT parity becomes structural. |
| **2** | **Per-PARAMETER ownership.** Every panel control is `Const(v)` \| `Driven(node_output)`. The panel becomes a structured editor over the graph, not a generator of it. |
| **3** | **PT gets real normal perturbation** — option (b). Bake-to-height stays as a separate authoring feature, not a substitute. |

**Non-negotiable rule from Decision 1:** *do not grow a second Live compositor.* Live may run at
lower resolution / fewer octaves but must be the SAME compositor. `ChannelEvalMode` (Live/Bake) stays
as a user-facing **quality tier**, not a second implementation.

---

## 2. CORRECTION — "Normal should mean over" was WRONG. Do not implement it.

I attempted this and reverted it. **The tests caught a real design error in my reasoning**, and the
correction matters because the ADR previously recommended it.

What I tried, in `gbuffer.wgsl`'s vertex-curvature composite:

```wgsl
} else {
    blended = mix(base, top, mag);   // "Normal = over"
}
```

Two `-p render` GPU tests failed immediately, including
`vertex_curvature_signed_channel_reaches_albedo_pixel` with *"left pixel barely moved off center
baseline 129"*.

**Why it is wrong:** `mag` is the curvature **VALUE** (0 = fully concave, 1 = fully convex), not a
coverage. Using it as over-coverage means **concave curvature becomes fully transparent** — it
vanishes instead of painting dark. Verified numerically: at signed curvature −1.0, `mag = 0`, so
`mix(base, top, 0)` returns the base unchanged.

**The correct semantics, which the shader ALREADY implements:**

```wgsl
top     = mix(black, white, mag);        // curvature value AS a colour
blended = top;                           // Normal: this colour replaces the base
color   = mix(base, blended, opacity);   // coverage applied HERE, from layer opacity
```

Coverage is the layer **opacity** (and its mask) — and the last line already applies it. So
`blended = top` is right, and my change double-applied coverage.

### So what IS the real bug?

**The layer beneath is never present in `base` at all.** `base` is the material albedo, not "the
composited result of the layers below". The G-buffer composites exactly ONE vertex-curvature layer
against the material colour — there is no layer stack in that shader to be discarded.

That is Decision 1's actual justification, and it is stronger than the version in the ADR: the
G-buffer is not *mis-blending* a stack, it **has no stack**. Making the bake the single compositor is
what gives the layers below somewhere to exist. Changing the blend arm could never have fixed it.

**Action for whoever implements P2:** do not touch `blended = top`. Delete the inline composite
entirely and sample the baked result. Also revisit the `new_generator_layer` default
(`if vertex { Normal }`) only *after* that — with a real compositor, Normal vs Multiply becomes a
genuine authoring choice rather than a workaround for a missing stack.

---

## 3. Implementation order

### P1 — foundation (nothing else is safe first)

1. **Stable IDs replace `mp:<kind>:<role>:<layer>`.** The parsed string cannot survive reorder,
   insert, or nesting — two mask layers at the same depth collide. Use the `prim_uid` pattern
   ("stable IDs, not indices" is already a CLAUDE.md rule).
2. **Regen becomes MERGE, not replace.** `regen_channel_subgraph` (`lower.rs:854-862`) currently
   deletes every panel-tagged node and rebuilds. It must preserve user-added nodes and any socket
   marked `Driven`, regenerating only panel-owned structure.
3. **`Const|Driven` params.** `Driven` lowers to a socket **connection**. The sockets already exist —
   `SdfCurvature::inputs()` declares `Position · Epsilon (the radius) · GrowShrink · Band · Contrast
   · Strength` — so this is lowering + UI, **not shader work**.

**Exit test:** hand-edit a node, then edit the panel; the hand edit survives.

### P2 — one compositor

1. Delete the G-buffer's inline vertex-curvature composite; sample the baked result.
2. Extend `MixBlend` to the full Photoshop set (~27). Honest scope: `MixBlend` (`ops.rs:292`) is
   already ONE definition evaluated in TWO places — CPU bake (`node_graph_bake.rs:2159`) and WGSL
   codegen (`graph.rs:1118`). One enum, two evaluators. Bounded and mechanical.
3. Per-layer alpha through the compositor with real over-operators.

**Exit test:** PT and Hybrid pixel-comparable on a multi-layer material.

### P3 — the owner's two blocked cases

1. Mask **stacks**: `mask: Option<LayerContent>` → recursive, each with its own blend/opacity.
2. Drivable params exposed in the UI (model landed in P1).

**Exit test:** noise drives curvature radius (visible); a mask has its own mask (visible).

### P4 — PT normal perturbation (Decision 3)

PT's shading path never reads `normal_map`/`bump_map` — gated by
`pathtracer_samples_albedo_roughness_metallic_but_not_normal_or_bump` in `graphics/src/lib.rs`.
**That test will fail when you wire it; update it deliberately.** Hybrid's
`sample_mat_triplanar_normal` (whiteout blend, no tangent basis) is the technique to port.

---

## 4. Hard constraints

* **Save format.** `Layer` is bincode-serialized. Widening `mask` → recursive stack and `radius: f32`
  → `Const|Driven` are **wire changes**: ride the in-blob `#[serde(default)]` side-channel, never an
  in-place retype. Appending to a **nested** struct also breaks the wire — reproduction in
  `docs/planning/sdf/REF_FL4_BACKCOMPAT_INVESTIGATION.md`.
* **Shader edits:** `cargo test -p graphics` and `-p render` must stay green (`shadow.wgsl` has a
  drift guard against `gbuffer.wgsl`).
* **Feature parity:** any material feature must land in CPU eval AND WGSL codegen together — they do
  not propagate automatically (CLAUDE.md).

---

## 5. Two things about the current tree

1. **65 files uncommitted**, now including `body_volume_cache/bake_dispatch.rs`. Build is green.
   Please commit before P1 — that much unlanded work in a shared tree is the standing risk, and P1
   touches the same files.
2. **The three "failing" material tests are NOT regressions.** I reported them as such in `d060903`
   and was wrong. `gpu_bake_matches_cpu_on_procedural_graph`,
   `mult_image_stack_bakes_at_map_res_for_upload`, and `mixrgb_multiply_is_base_times_layer_not_replace`
   all pass **alone** and pass with `--test-threads=1` (193 passed / 0 failed). They are a
   **parallel-execution race**, most likely GPU device/texture-array contention between bake tests.
   Worth fixing as its own item: red that depends on scheduling teaches people to ignore red.

---

## 6. Verification standard

Gate every step on the **soak harness showing a real pixel delta**. This is not ceremony — this
project has twice shipped a feature that passed every unit test and moved zero pixels (a curvature
cap that silently skipped every dispatch; a blend default that discarded the layer beneath). And as
§2 shows, GPU tests also catch *wrong fixes*, not just missing ones.

Also fixed this session, for context: the outliner eye now removes a light's contribution (it wrote
`SceneNode.visible` while the packer read `LightData.visible`), and the sun's `ambient` default is
now 0.0 to match Point/Spot — existing scenes will look darker on the shadow side, which is correct
since ambient belongs to the environment.
