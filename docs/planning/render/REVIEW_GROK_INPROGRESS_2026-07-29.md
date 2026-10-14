# Review: Grok's in-progress work + owner's colour/light report

**Date:** 2026-07-29
**Scope:** review only, no code changed.

---

## 1. Correction to my own earlier review — the "two regressions" were NOT regressions

In `d060903` I reported two failing tests in Grok's uncommitted work and called them regressions.
**That was wrong, and the correction matters because it was about to send someone chasing bugs that
do not exist.**

Re-checked properly:

| Test | Full parallel run | Alone | `--test-threads=1` |
|---|---|---|---|
| `gpu_bake_matches_cpu_on_procedural_graph` | FAILED | **ok** | **ok** |
| `mult_image_stack_bakes_at_map_res_for_upload` | FAILED | **ok** | **ok** |
| `mixrgb_multiply_is_base_times_layer_not_replace` | FAILED | **ok** | **ok** |

Single-threaded across the whole material/curvature/bake set: **193 passed, 0 failed.**

So these are a **parallel-execution race on shared state**, not broken behaviour. The GPU-bake
parity test in particular almost certainly contends over a shared GPU/device or texture-array
resource with the other bake tests.

**Two consequences:**

* The P0 item "fix the GPU/CPU parity regression" in `MATERIAL_FEATURE_COMPLETE_PLAN.md` is
  **withdrawn** — there is nothing to fix there.
* The **real** issue is that these tests are not parallel-safe. That is worth fixing on its own
  merits: a test that fails depending on scheduling teaches people to ignore red, which is far more
  expensive than the race itself.

The one item that **does** stand from that review: `new_generator_layer`'s
`if vertex { Normal }` still reintroduces the clobber, because `gbuffer.wgsl` still has no
`blend == 0u` arm (re-verified). That is a real behaviour question, not a test artifact.

### State of the tree

* `cargo build -p app` — **green**
* 65 files uncommitted, now including `body_volume_cache/bake_dispatch.rs` and `mod.rs`
* Standing risk unchanged: this much unlanded work in a shared tree is the largest single hazard

---

## 2. Owner's report: colours disagree between swatch, cube, and preview

> "check the color they feel broken, look at my choice the saturation of the cube, the fill preview
> they are all different color"

**Not yet root-caused — but the shape of it is diagnostic.** The picker reads
**RGB (224, 95, 95)**. Converted to linear that is **(0.745, 0.114, 0.114)**.

Three surfaces disagree, which means **more than one colour-space conversion is inconsistent**, not
a single missing decode:

* if a picker value is treated as **linear** and then sRGB-**encoded** on bake → cube renders
  lighter and desaturated versus the swatch
* if it is **double-decoded** → cube renders darker
* the owner reports *all three different*, so at least two of {picker swatch, bake path, preview
  path} disagree with each other

This is the same class as the already-fixed *"baked material too dark"* bug
(`encode_srgb_color_rgba`'s doc comment: "storing `v*255` raw made the hardware decode darken every
baked color (0.5 → 0.21)"). That precedent is why this needs measuring rather than guessing: the fix
last time was one specific conversion, and the wrong guess produces a plausible-looking but still
wrong result.

**Recommended next step (measurement, not a fix):** dump the actual RGB at all three points for one
known input — picker value, baked map byte, final rendered pixel — and compare. Grok's dual-plane
work touches `encode_*_rgba16_planes`, so a run under the current tree is the right sample.

---

## 3. Owner's report: sun ambient should default to zero — CONFIRMED

> "by default we have ambient color inside the sun light, but ambient should be zero by default since
> we use HDR, otherwise we have 2 ambient lights"

**Verified in `scene_graph.rs`:**

```rust
LightType::Directional => { ... ambient: 0.15, ... }   // :170
LightType::Point       => { ... ambient: 0.0,  ... }   // :205
LightType::Spot        => { ... ambient: 0.0,  ... }   // :216
```

The sun is the **only** light type with non-zero default ambient. Point and spot are already 0.0. So
this is an inconsistency, not a considered choice — and with an HDRI supplying environment light it
is double-counting exactly as the owner says.

**Caveat before changing it:** a scene with an HDRI *and* no ambient may currently rely on that 0.15
to avoid fully black shadow sides. Changing the default is correct, but it will visibly darken
existing scenes — so it wants a deliberate decision (and possibly a migration note), not a silent
flip.

---

## 4. Owner's report: hiding the sun does not remove its light — CONFIRMED, and the cause is visible

> "when i click on the eye on the sun the lighting does not disappear and it should — the eye should
> remove all the effect from the object in the scene"

**`light_pack.rs` contains no visibility filter at all.** Greping it for `node_visible` /
`is_visible` / `.visible` returns nothing outside `visible_count` (which is a *count of packed
lights*, unrelated to the outliner eye). Its own doc says "Input snapshot for one author light
(visible only)" — but nothing enforces that.

So the eye toggles a scene-graph flag that the light packer never consults. The light keeps being
packed and keeps lighting the scene.

This is the same **class** as several bugs already fixed this session: a control that appears to do
something, changes state, and is never read by the consumer. It is also the easiest of the three to
fix confidently — the fix is a filter at the pack site, and it is testable (pack a hidden light,
assert it is absent).

---

## 5. Summary of what is actionable

| Item | Status | Note |
|---|---|---|
| Grok's "2 regressions" | **withdrawn** | parallel-test race; 193/193 single-threaded |
| Tests not parallel-safe | **new finding** | worth fixing; scheduling-dependent red trains people to ignore red |
| `new_generator_layer` Normal default | **stands** | still no `blend == 0u` arm |
| Colour mismatch (3 surfaces) | **needs measurement** | dump picker / baked byte / rendered pixel for one input |
| Sun `ambient: 0.15` default | **confirmed** | only light type that is non-zero; will darken existing scenes |
| Eye does not unlight | **confirmed + cause found** | `light_pack.rs` has no visibility filter |

Nothing in this document was fixed — review only, per instruction.
