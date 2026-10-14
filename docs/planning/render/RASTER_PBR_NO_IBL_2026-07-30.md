# Metalness / roughness only read correctly in Path Trace — raster has no IBL

**Date:** 2026-07-30
**Owner:** *"I still need to go to path tracing preview before metalness and roughness work in PBR."*
**Status:** root-caused by reading the shader. No code changed.

---

## The symptom is real, and it is not the material system

The maps are fine. The bake is fine. The G-buffer is fine. **The raster lighting path never evaluates
image-based lighting**, and metal + smooth roughness are *almost entirely* IBL.

A metal surface has **no diffuse term** (`diffuse_weight = 1.0 - metal * 0.7`) and its specular is a
mirror of the environment. With no environment to reflect, a metal reads as a dark, flat, slightly
shiny grey — regardless of how correct the metalness map is. Path Trace looks right because it samples
the environment properly.

---

## Where it breaks, exactly

`graphics/src/deferred_entry.wgsl` has **two** shading branches:

| branch | starts | what it lights | IBL? |
|---|---|---|---|
| **raster** (`lit_is_raster`) — poly bodies, baked DC meshes | `:543` | ends at `:673` | ❌ **none** |
| **SDF / analytical** | ~`:722` | ends after `:1055` | ✅ split-sum at `:968` |

The raster branch computes its final colour at `:673`:

```wgsl
var lit_color = lit_albedo * (lit_diff + lit_ambient * lit_ao_only * lit_ssao) + lit_spec;
```

`lit_diff` and `lit_spec` are **analytic punctual lights only** (the `for` loop at `:640-658`). There
is no environment term in that expression, and the branch returns before reaching the split-sum block.

The SDF branch, by contrast, runs the full thing at `:966-990`:

```wgsl
// Diffuse = SH9 irradiance; specular = GGX-prefiltered chain × BRDF LUT.
if (config.env_width > 0u) { … }
```

**So: the same material renders with IBL on an SDF body and without IBL on a poly body.** The owner's
object in the screenshot is `Poly.box` — the raster branch.

---

## What is NOT the problem (checked, so nobody re-investigates)

* **The maps install correctly.** `node_graph_bake.rs:5017-5025` sets `roughness_map.strength` /
  `metallic_map.strength` for the material-node path (and `:4602-4603` for the prim path).
* **The G-buffer samples them.** `render/src/gbuffer.wgsl:684-695` samples both slots and *replaces*
  the scalar by map strength — with a correct comment explaining why replace (not multiply) is the
  right semantic for value maps.
* **The BRDF math is correct in both branches.** Both compute GGX × Schlick with
  `f0 = mix(vec3(0.04), albedo, metal)` — `:655` (raster) and `:944`/`:970` (SDF). The comment at
  `:936` even says *"roughness/metal must read in PBR without HDRI"*, which is the intent; the raster
  branch just does not deliver it.
* **The G-buffer override is correctly gated.** `:553` and `:565` overwrite `lit_metal` from the
  analytic material only when `!lit_is_raster`, so poly geometry keeps its baked values.

Everything upstream is right. The gap is one missing term in one branch.

---

## The fix

**Port the split-sum IBL block from the SDF branch (`:966-990`) into the raster branch, before
`:673`.** Everything it needs already exists and is already bound:

* SH9 irradiance for the diffuse term
* the GGX-prefiltered environment chain (mips 1..6, roughness = mip/6)
* the BRDF LUT (`env_ibl.rs::generate_brdf_lut`, referenced at `:257`)

This is a **port, not new machinery** — which is the strongest argument for doing it: the risk is
mis-wiring, not designing.

### Two details that matter

1. **Ambient scaling.** The SDF branch does
   `let ambient = select(ambient_flat, ambient_flat * 0.12, config.env_width > 0u)` (`:962`) — flat
   ambient is cut to 12 % when an environment exists, because IBL replaces it. The raster branch must
   do the same, or adding IBL will **double-count** and wash everything out. This is exactly the
   ambient double-count class fixed earlier this session.
2. **SSAO already applies to the raster ambient** (`:672`), and the SDF branch applies occlusion to
   its IBL diffuse too. Keep occlusion on the ambient/IBL terms only — never on the direct lights
   (the existing comment at `:670` states this rule).

### Gate

A metal sphere (`metallic = 1.0`, `roughness = 0.2`) lit **only** by an environment (no punctual
lights) must render **non-black** on a poly body. Today it is near-black — that single assertion
captures the whole bug.

Then: the same material on an SDF body and a poly body should render within a small tolerance of each
other. That is the real invariant — **representation must not change shading** — and it is the one
worth keeping permanently.

---

## Why this is worth prioritising

It makes the **whole material system look broken** without being broken. Every metal preset — Worn
Gunmetal, Old Gold, the entire library Grok is building — is being judged in the viewport through a
lighting path that cannot show metalness. That is a bad feedback loop for authoring: the owner tunes
values against a preview that structurally cannot represent them, then has to switch to Path Trace to
see the truth.

It also undercuts the material work in progress. Presets authored against a no-IBL preview will be
mis-tuned when IBL arrives.
