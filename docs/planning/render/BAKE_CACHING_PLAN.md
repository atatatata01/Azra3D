# Bake Caching — Plan (composite-only edits)

**Status:** proposed, not started.
**Owner exchange 2026-07-28:** worth planning — *"or we need to be feature complete"*.

---

## Sequencing recommendation (read this first)

The owner raised the real question: **optimize now, or finish the features first?**

**Recommendation: finish the features. Do this plan after.** Reasons, in order of weight:

1. **The urgent pain is already fixed.** `c83391a` caps Live bakes at 128² while dragging — 64×
   fewer pixels, so a preview frame is ~20 ms (release) instead of seconds. The remaining win is
   real but no longer blocking anyone's work.
2. **This plan's design depends on features that are not built yet.** The cache key must cover
   every input that can change a generator's output. M2 (projections + triplanar blend), M3 (the
   adjustment curve) and the new generators (Direction shipped; AO / SDF-target pending) each add
   inputs. Designing the key now means redesigning it three times, and a cache with an incomplete
   key is not slow — it is **wrong**, silently serving stale pixels.
3. **The GPU bake (P1) may make it moot.** If `GPU_MATERIAL_BAKE_PLAN` P2/P3 land, a full bake
   becomes milliseconds and per-generator caching stops being worth its complexity for procedural
   graphs. Building both is likely wasted work.

**Do it sooner only if** a composite-only edit is still visibly slow after `c83391a` in real use —
that is the trigger, and it is a measurement, not a guess.

---

## The observation (verified, not assumed)

Changing **opacity** or **blend mode** re-evaluates every generator in the stack, even though
neither can change a generator's output.

Evidence in the lowering (`material_panel/lower.rs`):
- opacity is written into ONE `MixRgb` factor (`:1224`, `s.value_f32 = layer.opacity`), and the
  emitter's own comment says *"ONE MixRgb node carries BOTH the blend mode and the opacity"*
  (`:1208`);
- a masked layer multiplies that factor by the mask (`:1227`) — still compositing only.

So both are **tail-of-the-graph** parameters. Yet `mp_commit_doc_edit` regenerates the channel
subgraph and schedules a bake that re-runs the whole per-pixel node evaluation — including FBM
noise and the ~500-eval curvature path — to produce pixels whose generator inputs are byte-identical
to the previous frame's.

---

## What a fix looks like

**Cache generator OUTPUT per (node, parameters, resolution)**, keyed on everything that can change
the result, and reuse it when only composite parameters changed.

The precedent already exists in this codebase and should be copied rather than reinvented:
`CurvGrid` + `CurvKey` (`gpu_curvature.rs`) do exactly this for curvature — one grid per distinct
knob-set, bit-exact key comparison, and a fail-soft fallback to full evaluation when the key does
not match. `curvature_keys_for_graph` even caps the number of distinct keys per bake
(`MAX_CURV_KEYS_PER_BAKE`) to bound work — a pattern this plan needs too.

### The cache key is the whole problem

It must include EVERY input that can change a generator's output:

| Input | Why it must be in the key |
|---|---|
| node type + all knob sockets | obvious |
| resolution | a 128² preview must not satisfy a 1024² final |
| the SDF field snapshot | curvature/cavity/PrimSdfMask read geometry — **moving a prim must invalidate** |
| placement (tiling / offset / projection) | changes the sample position per pixel |
| upstream wired inputs | a wired Position/knob makes the node a function of another node |
| axis (Direction), and any future generator params | added by each new source |

**A missing key component does not make the cache slow — it makes it WRONG**, serving stale
pixels that look plausible. That is the failure mode this codebase has already shipped twice
(`ImageTexture` white placeholder, synthetic-field curvature), and it is why the key cannot be
designed until the input set stops moving.

### Scope boundary

Only **composite** parameters may skip re-evaluation: opacity, blend mode, layer visibility,
layer reorder. Everything else (any generator knob, placement, adjust, mask content, geometry)
invalidates. When in doubt, invalidate — a needless re-bake costs time; a wrong cache costs trust.

---

## Phases (when it is time)

- **C1 — Prove the win.** Instrument a composite-only edit end to end and record the actual cost
  after `c83391a`. If a 128² preview edit is already ~20 ms, the remaining headroom may not justify
  the complexity. **Do not build on the assumption; measure first.**
- **C2 — Key + cache for ONE source** (Noise is the cheapest to validate). Golden test: same key →
  byte-identical output to an uncached bake.
- **C3 — Invalidation tests before extending.** Explicit tests that moving a prim, changing a
  radius, changing resolution, and changing placement each MISS the cache. These are the tests that
  make the cache trustworthy; write them before adding more sources.
- **C4 — Extend to remaining sources**, one at a time, each with its own invalidation test.

---

## Rules carried in

- **Fail-soft:** any doubt about key validity → full evaluation. Never serve a maybe-stale pixel.
- **Bounded:** cap cache entries (the `MAX_CURV_KEYS_PER_BAKE` precedent) — unbounded caches are a
  memory leak with extra steps.
- **The CPU bake stays the oracle** for the GPU path (`GPU_MATERIAL_BAKE_PLAN` P4). A cache that
  changes CPU output breaks that contract; the golden test in C2 is what protects it.
