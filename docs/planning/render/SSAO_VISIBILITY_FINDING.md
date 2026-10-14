# Viewport SSAO — why the sliders appear to do nothing

**Owner 2026-07-28:** *"when I play with settings for AO I don't see anything"* (viewport AO, not
the material AO generator — that one is genuinely unbuilt, see `AO_BAKE_PLAN.md` Phase B/C).

**Verdict: the SSAO is computing and IS applied. It is bounded to a ~6% effect by where it is
applied, so no slider setting can make it visible.** This is a design consequence, not a bug, and
it needs an owner decision rather than a silent "fix".

---

## Verified end to end (not inferred)

| Link | State |
|---|---|
| `ssao_enabled` default | **true** (`types_render.rs:334`) |
| `ssao_strength` reaches the pass | yes — `viewport_render.rs:2405` |
| SSAO computed | yes — `ssao_factor()` (`deferred_entry.wgsl:156`) |
| Applied to flat ambient | yes — `:957` `scene_ambient * final_ao * ssao_f` |
| Applied to IBL diffuse | yes — `:978` `multi_bounce_ao(ssao_f, albedo)` |

Every link works. Nothing is disconnected.

---

## Why it is invisible anyway

The shading is
```
base_color = albedo * (total_diffuse + ambient) + total_spec
```
and **SSAO only multiplies `ambient`** — it never touches `total_diffuse` (direct light) or
`total_spec`. On top of that, with an environment map loaded the flat ambient is scaled by a
**hardcoded 0.12** (`:958`).

Measured effect of going from *no occlusion* to *fully occluded*:

| direct diffuse | pixel change at full occlusion |
|---|---|
| 0.2 | 15% |
| 0.6 | **6%** |
| 1.0 | 3.5% |

A typical lit surface sits near the middle. **Full occlusion — the strongest the effect can ever
be — moves the pixel about 6%.** Moving a slider inside that range is imperceptible, which is
exactly the report.

---

## Options (owner decision — do not pick one silently)

1. **Expose the `0.12` ambient scale** as a setting. Smallest change, and it directly controls how
   much room AO has to work in. Risk: raising it changes the look of every existing scene.
2. **Apply AO to more than ambient.** Physically, AO should attenuate indirect light only — but
   many DCCs deliberately let it bite into the direct term for artistic control (an "AO intensity"
   that is knowingly non-physical). This is the option that makes the slider feel like it does
   something.
3. **Accept it and remove the misleading control.** If AO is meant to be a subtle IBL refinement,
   a slider that visibly does nothing is worse than no slider — it reads as broken software.

**Recommendation: 1 first** (cheap, reversible, and it may be enough), then 2 if the owner wants
the strong stylised AO the reference DCCs give.

**Not recommended:** quietly boosting AO strength internally. That would make one scene look right
and every other scene look wrong, and would hide the real constraint.
