# PT Transmission / Refraction Lobe — Implementation Plan ("M" build: real glass)

Status: PLAN (read-only investigation, no code changed). Author: investigation pass 2026-07-26.
Target shader: `core_engine/graphics/src/pathtrace_tri_entry.wgsl` (the LIVE triangle path tracer;
`pathtrace_entry.wgsl` = analytical PT = DEAD per `lib.rs`, do NOT touch it).
Goal: add a dielectric transmission/refraction lobe so `ShaderType::Glass` materials actually refract
(rays bend, Fresnel reflect/transmit split, TIR), driven by the existing `ior` field. Beer-Lambert
absorption and dispersion build on THIS — they are the next features, not this one.

---

## 0. Current state (verified file:line)

**Material struct** `PtMaterial` (`:107`–`:127`), 19×vec4 / 304 B, MUST match
`render/src/mesh_gpu.rs:193,251` upload stride:
- `albedo.w` = **opacity** (`:108`) — currently STORED-ONLY, never read in the BSDF.
- `roughness_metallic.w` = **shader_type** (`:109`). `ShaderType::Glass = 2` (`pbr_material.rs:24`).
- `adv_params.y` = **ior** (`:124`). Default 1.5 (fallback ctor `:416`; also glass preset `pbr_material.rs:396`).
- `adv_glass.xyz` = **glass_tint** (`:126`) — STORED-ONLY today.

**BSDF sampler** `pt_sample_bsdf(n, v, albedo, rough, metal, seed, out_pdf, out_f_cos) -> vec3` (`:1960`–`:1997`):
- Returns the sampled direction `l`; writes `out_pdf` (scalar) and `out_f_cos` (= f·cos, vec3).
- Lobe select: `spec_prob = clamp(metal + (1.0-rough)*0.35, 0.05, 0.95)` (`:1970`). `rand < spec_prob`
  → GGX half-vector reflect (`:1972`–`:1990`, below-horizon falls back to cosine); else cosine diffuse (`:1992`).
- `out_pdf = pt_pdf_bsdf(...)` (`:1994`, mirror of the same mix), `out_f_cos = pt_eval_f_cos(...)` (`:1995`).
- **No `refract()` anywhere in the file. `ior` never read in any BSDF/eval. `opacity` never read.**
  So transmission is a genuine NEW lobe with NO existing hooks.
- `pt_sample_bsdf` is also called from the GI-cache path: `pt_env_nee:1498`, `pt_gi_trace_along:1665`,
  `pt_sample_gi_candidate:1718`. **These extra call sites must NOT crash on the new signature** — see §4.

**Bounce loop** `pass_pathtrace_tri` (`:1999`–`:2334`), main integration surface:
- `for bounce in 0..=config.pt_max_bounces` (`:2104`).
- Hit → `hit_p` (`:2129`), `n = pt_hit_normal(hit, ray_d)` (`:2130`), `mat = pt_hit_material(hit)` (`:2131`).
- `albedo/rough/metal` resolved + mapped (`:2167`–`:2231`); emission (`:2234`–`:2247`).
- **Lean vs deep split (`:2262`):** `use_gi_cache = pt_max_bounces <= 2u`. When lean, at bounce 0 it calls
  `pt_restir_gi` and **`break`s (`:2288`)** — the path NEVER continues past the first surface.
  Deep path (`pt_max_bounces >= 3`) is the only one that runs the continuation loop.
- NEE: ReSTIR DI at bounce 0 (`:2265`), `pt_nee` deeper (`:2268`), env NEE (`:2273`).
- Russian roulette (`:2306`–`:2319`): `rr_start = 2` (1 when settled); `p_survive = max channel of throughput`;
  `throughput /= p_survive`.
- **Continuation (`:2321`–`:2333`):** `l_next = pt_sample_bsdf(...)`; guard
  `if pdf_b < 1e-6 || max(f_cos) < 1e-8 { break; }` (`:2326`); `throughput *= f_cos / pdf_b` (`:2329`);
  `ray_d = l_next` (`:2330`); **`ray_o = hit_p + n * PT_EPS * 2.0` (`:2331`)** — offset ALONG `n` (outward).

**Normal convention** `pt_hit_normal` (`:383`–`:394`): interpolated + instance-transformed, then
**force-faced to camera: `if dot(nw, dir) > 0.0 { nw = -nw; }` (`:392`).** So `n` ALWAYS opposes `ray_d`
(front-face geometric normal). The shader has **NO medium/ior stack, NO inside-glass flag** — it is
**single-surface today**. The backface flip DESTROYS the entering/exiting distinction the loop would
otherwise carry, so the refraction code must re-derive it from the RAW geometric normal (see §2).

**Fresnel today:** only `pt_fresnel_schlick(cos, f0)` (`:551`) — the metal/plastic F0 form. There is
**no dielectric Fresnel** (the scalar reflectance-ratio form). Must be added (§3).

**Denoiser/reproject:** `pt_reproject.wgsl` keys on the **primary hit** normal+distance
(`cur_normal_dist`, `:24`, `lighting_to_gbuf`), and `pt_denoise.wgsl`. Glass's refracted radiance is
stored at the PRIMARY glass surface, guided by the glass surface normal/depth — acceptable, but see §6.

---

## 1. BSDF sampler — exact integration points

The transmission lobe is a THIRD branch that must be reachable when the surface is glass. Two options:

- **(Chosen) A dedicated `pt_sample_dielectric()` called from the bounce loop BEFORE the generic
  `pt_sample_bsdf`, gated on the glass trigger.** Keeps `pt_sample_bsdf`'s signature untouched (so the
  4 GI/NEE call sites `:1498,:1665,:1718,:2325` do not change and glass simply doesn't feed the GI
  cache — correct, glass is a specular transport that the diffuse GI cache should not sample).
- Rejected: overloading `pt_sample_bsdf` with an ior/opacity param — forces touching all 4 call sites,
  and the diffuse-oriented GI reservoir eval would try to MIS a delta transmission lobe (pdf spikes).

New function (smooth v1, delta lobe — see §3 for math):
```
// Returns sampled dir; writes weight (already = throughput multiplier, NO extra /pdf needed
// because reflect/transmit are chosen by Fresnel probability so f/pdf collapses to the tint).
// out_inside toggles the caller's medium bit. n_geo = RAW geometric normal (NOT camera-faced).
fn pt_sample_dielectric(n_geo, v, ior, tint, entering, seed, out_dir, out_inside) -> vec3<f32>
```

**Loop change at `:2321`–`:2331`.** Insert, before the generic sampler:
```
let is_glass = shader_type == 2u || (shader_type == 4u && mat.albedo.w < 0.995 && ior > 1.01);
if (is_glass) {
    var out_dir; var new_inside;
    let w = pt_sample_dielectric(n_geo, view, ior, glass_tint, !inside, &seed, &out_dir, &new_inside);
    throughput *= w;                          // w already includes Fresnel split + tint; NO /pdf
    ray_d = out_dir;
    // Offset on the SIDE the ray leaves toward (transmit = -n_geo hemisphere, reflect = +):
    ray_o = hit_p + sign(dot(out_dir, n_geo)) * n_geo_raw * PT_EPS * 2.0;
    inside = new_inside;
    last_pdf_b = 0.0;                          // delta lobe → env MIS must treat as pure specular
    glass_bounce += 1u;                        // separate budget, see §4
    continue;                                  // skip the diffuse RR/NEE tail for this bounce
}
```
`shader_type = u32(mat.roughness_metallic.w + 0.5)`, `ior = mat.adv_params.y`,
`glass_tint = mat.adv_glass.xyz` are read from `mat` already in scope at `:2131`.

**Where the delta lobe skips machinery:** NEE (`:2265`/`:2268`) and env NEE (`:2273`) must be **skipped
for glass hits** — a delta BSDF has zero measure toward an explicitly sampled light, so importance-
sampling a light contributes nothing and would double-add noise. Gate those three blocks with `!is_glass`.

---

## 2. Geometry / normal convention — entering vs exiting

Today `n` is camera-faced (`:392`) → cannot tell entering from exiting. Fix:
- Compute the **raw geometric/shading normal `n_geo_raw`** (interpolated, instance-transformed, NOT flipped)
  once at the hit. `entering = dot(ray_d, n_geo_raw) < 0.0` (ray hits the outward face → entering).
- Maintain a **single `var inside: bool = false;`** in the per-sample scope (init before the loop).
  This is the minimal medium tracker: v1 assumes a single glass medium (air outside, one ior inside),
  which is correct for non-overlapping convex/closed glass. Nested/overlapping glass needs a stack — OUT OF SCOPE v1.
- `eta = select(ior/1.0, 1.0/ior, entering)` i.e. `entering ? air_ior/glass_ior : glass_ior/air_ior`,
  with `air_ior = 1.0`. Orient the refraction normal to oppose the ray: `nf = entering ? n_geo_raw : -n_geo_raw`.

**Recommendation: v1 = SOLID with a single-medium `inside` bool** (not thin-surface). It is *barely*
more code than thin and gives correct entry+exit refraction on closed meshes (a sphere, a cube),
which is the honest demo of "real glass." Thin-surface (single interface, ray exits same side) looks
wrong on solids and would have to be redone. The `inside` bool is also exactly the state Beer-Lambert
absorption (next feature) needs — so building it now is the seam, not throwaway.

**Blocker to flag:** the **lean GI-cache path (`pt_max_bounces <= 2`) `break`s at bounce 0 (`:2288`)**
and never runs the continuation loop, so **glass cannot refract in interactive/lean mode.** v1 must
force glass paths onto the deep continuation loop even when `use_gi_cache` is true (e.g. detect glass at
bounce 0 and skip the `pt_restir_gi ... break`, or require `pt_max_bounces >= 3` for scenes with glass).
OWNER DECISION needed (§ owner list). Simplest honest v1: **if the primary hit is glass, do NOT take the
lean break — fall through to the continuation loop** (bounded by `glass_bounce` budget, §4).

---

## 3. Dielectric math to add

**(a) Dielectric Fresnel (exact, not Schlick-for-metals).** Add:
```
fn pt_fresnel_dielectric(cos_i: f32, eta: f32) -> f32 {
    // eta = ior_from / ior_to. cos_i = |dot(-ray_d, nf)| >= 0.
    let s2t = eta*eta * (1.0 - cos_i*cos_i);   // sin^2(theta_t)
    if (s2t >= 1.0) { return 1.0; }            // TIR → full reflect
    let cos_t = sqrt(1.0 - s2t);
    let rs = (eta*cos_i - cos_t) / (eta*cos_i + cos_t);
    let rp = (cos_i - eta*cos_t) / (cos_i + eta*cos_t);
    return 0.5 * (rs*rs + rp*rp);
}
```
(Schlick's `f0 = ((1-ior)/(1+ior))^2` is an acceptable cheaper fallback but gets TIR wrong at grazing
exit angles — use the exact form; it's a handful of ALU.)

**(b) Refract with TIR guard.** WGSL has builtin `refract(I, N, eta)` which returns `vec3(0.0)` on TIR
(its internal `k < 0`). Rely on the Fresnel `s2t >= 1.0` test as the authoritative TIR signal and, on
TIR, **reflect** (`reflect(ray_d, nf)`) instead — do not trust a possibly-zero `refract()` result.

**(c) Sampling (smooth v1, delta lobe):**
```
let cos_i = abs(dot(ray_d, nf));            // ray_d points INTO surface
let F = pt_fresnel_dielectric(cos_i, eta); // reflectance
if (pt_rand(seed) < F) {                    // reflect branch
    out_dir = reflect(ray_d, nf);
    out_inside = inside;                    // stays on same side
    weight = tint_reflect;                  // usually vec3(1.0); glass_tint applies to TRANSMIT only
} else {                                    // transmit branch
    out_dir = refract(ray_d, nf, eta);      // guaranteed non-TIR here (F<1)
    out_inside = !inside;
    // Radiance transmission: multiply by eta^2 ONLY if you compress by radiance convention.
    weight = tint_transmit;                 // see eta^2 note below
}
```
Because reflect vs transmit is chosen with probability `F` / `1-F`, the `F` (or `1-F`) and the branch
pdf **cancel**: `throughput *= (F * 1) / F = 1` for reflect and `((1-F)) / (1-F) = 1` for transmit
(delta lobes, pdf is the discrete branch probability). So `weight` is just the optional tint —
**do NOT additionally divide by F.** This is the single most common firefly source if done wrong.

**(d) The eta² radiance-compression factor.** When a ray crosses into a denser medium, radiance is
compressed by `eta_t^2/eta_i^2 = 1/eta^2`. For a **bidirectional/importance-sampled path tracer from the
camera, the classic result is: multiply transmitted throughput by `eta^2`** (with `eta = ior_from/ior_to`
as defined here) to keep the estimator unbiased. **v1 recommendation: OMIT the eta² factor** and document
it. Reason: with a symmetric closed solid, the enter (`*eta_in^2`) and exit (`*eta_out^2 = *1/eta_in^2`)
factors CANCEL to 1.0, so a closed glass object is unaffected, and omitting it avoids a subtle brightness
bug on the far more common closed-mesh case. Add it (guarded, behind the same `inside` state) only if a
single-interface/thin case is ever shipped. **Flag as OWNER DECISION** because it is the textbook "glass
looks too dark/too bright" knob.

**(e) Rough dielectric (frosted glass):** GGX-sample a microfacet half-vector `h` (reuse the `:1974`–`:1985`
half-vector code), then Fresnel+refract/reflect ACROSS `h` instead of `nf`, and weight by the Smith G /
visibility term (the `F`/pdf cancellation no longer holds — a proper `f*cos/pdf` weight is needed).
**v1 recommendation: SMOOTH ONLY** (`rough` ignored for glass, treated as a delta lobe). It is
deterministic, unit-testable (Snell's law is exact), and covers "clear glass," the actual unlock.
Rough glass is a fast-follow once smooth is verified. Gate: `if rough < ROUGH_SMOOTH_EPS treat as smooth`,
and for v1 just always-smooth for glass.

---

## 4. Ray continuation & bounce budget

- Refracted/reflected ray spawns on the correct side via `sign(dot(out_dir, n_geo_raw))` offset (§1) —
  transmit exits behind the surface (`-n_geo_raw`), reflect stays in front (`+n_geo_raw`). Using the
  camera-faced `n` for the offset (as the current `:2331` does) is WRONG for a transmitted ray (it would
  offset back onto the entry side → immediate self re-hit / acne). This is a required change.
- `max_bounces` = `config.pt_max_bounces` (`:2104`). **Glass eats bounces fast** (a solid cube = enter+exit
  = 2 bounces minimum, plus internal reflections). Add a **separate `glass_bounce` budget**, e.g.
  `const MAX_GLASS_BOUNCE = 8u`, incremented only on glass hits and NOT counted against the diffuse
  `pt_max_bounces`. When `glass_bounce >= MAX_GLASS_BOUNCE`, terminate (return env/black) to avoid a ray
  trapped in TIR forever. **OWNER DECISION: default MAX_GLASS_BOUNCE (recommend 8) and whether it's a
  separate budget (recommend yes).**
- **Russian roulette (`:2308`):** glass throughput is ~1.0 (tint aside), so RR rarely kills it — fine, but
  ensure the glass `continue` path (§1) still passes through RR OR is explicitly exempt for the first few
  glass bounces (recommend: exempt glass bounces from RR until `glass_bounce >= 3`, else clear glass
  goes noisy from premature termination).
- The glass branch uses `continue` (§1) so it skips the diffuse NEE/GI tail cleanly. Verify the loop's
  trailing `throughput < 1e-4 break` (`:2333`) is AFTER the glass branch or duplicated inside it.

---

## 5. Wiring — the honest trigger

Match the raster G-buffer exactly (`render/src/gbuffer.wgsl:684`):
`glass_on = shader_type == 2u || (shader_type == 4u && opacity < 0.995 && ior > 1.01)`.
- **Primary trigger: `shader_type == Glass (2)`.** This is the honest, explicit signal (user picked the
  Glass shader). `ior` (`adv_params.y`) drives the bend; `opacity` (`albedo.w`) is NOT the gate for pure
  Glass (a Glass material with opacity 1.0 is still glass — opacity gates only the Advanced=4 path, per raster).
- `glass_tint` (`adv_glass.xyz`) = **surface tint applied to the TRANSMITTED throughput** in v1 (a constant
  multiply, thickness-independent). This is the Beer-Lambert seam: absorption replaces the constant tint
  with `exp(-absorption * path_length_inside)`, where `path_length_inside` is available because the `inside`
  bool + hit distance let the loop accumulate the segment length between an enter and the matching exit.
  **Document: v1 tint = `throughput *= glass_tint` on transmit; absorption slots in by swapping that one
  line for the Beer-Lambert term keyed on the tracked interior segment length.** No struct change needed —
  `glass_tint` already carries the color; absorption will reinterpret it as an extinction coefficient.

---

## 6. Failure modes (black glass / fireflies) + mitigations

| # | Failure | Cause | Mitigation |
|---|---------|-------|-----------|
| 1 | Fireflies on glass | Dividing throughput by F/(1-F) on top of probabilistic branch selection | §3(c): branch prob and pdf CANCEL — weight is tint only, NO extra `/pdf`. |
| 2 | Glass too dark/bright | eta² factor applied on one interface but not the matching one | §3(d): OMIT eta² in v1 (cancels on closed solids); if added, must be symmetric. |
| 3 | Self-intersection acne | Transmitted ray offset with the wrong-side normal | §4: offset by `sign(dot(out_dir, n_geo_raw)) * n_geo_raw * PT_EPS*2`, NOT the camera-faced `n`. |
| 4 | NaN / black at grazing angles | TIR sqrt of negative; trusting `refract()`'s zero return | §3(a,b): `s2t >= 1.0 → reflect` BEFORE calling refract; refract only on the F<1 branch. |
| 5 | Energy gain (scene brightens) | Fresnel > 1 or reflect+transmit both added (double path) | Single branch per bounce (choose one); exact dielectric Fresnel ∈ [0,1]. |
| 6 | Ray trapped in TIR forever | Internal reflections never escape a solid | §4: `MAX_GLASS_BOUNCE` cap → terminate to env/black. |
| 7 | Glass invisible in interactive mode | Lean GI-cache path breaks at bounce 0 (`:2288`) | §2 blocker: glass primary hit must fall through to the deep continuation loop. |
| 8 | Denoiser smears / lags glass | `pt_reproject.wgsl` guides on the PRIMARY surface normal+depth (`:24`), which for glass is the glass shell, not the refracted content behind it | Acceptable v1 (primary shell IS stable under orbit). Note: refracted content ghosts slightly during fast camera moves; do NOT special-case reproject in v1 — accept, revisit with a "specular hit distance" guide later. Grep confirms no glass-specific reproject rejection exists today. |
| 9 | GI cache samples the delta lobe → pdf spikes | `pt_sample_bsdf` (used by `pt_restir_gi`) tries to MIS glass | §1: glass uses a SEPARATE `pt_sample_dielectric`, never enters the GI reservoir; NEE/env-NEE gated `!is_glass`. |
| 10 | Stride mismatch / wrong material read | Any struct edit | NO struct change needed (all fields exist). If ever changed, update `mesh_gpu.rs:193/251` in lockstep. |

---

## 7. Test strategy (path tracers are hard to unit-test)

- **T1 — Snell's-law bend (feasible headless, unit).** In a Rust test mirroring the WGSL math (port
  `pt_fresnel_dielectric` + refract to a `#[cfg(test)]` fn in `graphics` or `core_math`), assert the
  refracted direction for a flat interface at known incidence matches `n1 sinθ1 = n2 sinθ2` within eps.
  Also assert TIR fires at exactly `θ_c = asin(1/ior)` for ior 1.5 (≈41.8°). This is the most valuable,
  fully deterministic, headless-feasible check. **Do this first.**
- **T2 — Flat-slab radiance offset.** A known-IOR flat glass slab over a checker floor: verify the seen
  point is laterally displaced by the analytic slab-shift `t·sinθ(1 - cosθ/√(n²-sin²θ))`. Needs a GPU
  dispatch + readback (there IS a GPU-test harness pattern in `graphics/src/*_gpu_test.rs`, e.g.
  `ssr_gpu_test.rs`, `vol_light_gpu_test.rs`) — **feasible headless via that harness**, moderate effort.
- **T3 — White-furnace energy conservation.** Uniform env = 1.0, a glass sphere, no absorption/tint:
  the sphere must integrate to ~1.0 (neither brighten nor darken) — catches eta² and Fresnel-double-add
  bugs (#2, #5). GPU dispatch + average readback; **feasible headless** via the same harness, but noisy
  (needs many spp — average a large accum). Tolerance ~±3%.
- **T4 — Visual smoke (NOT headless — I cannot see the running app).** Owner/JAKE launches, drops a glass
  sphere + cube, confirms refraction + TIR rim + tint. Gate before merge.
- Naga parity: **`cargo test -p graphics`** after the shader edit (all parse tests must pass — the shader
  is concatenated/validated). Recompile cost applies (single-file entry, no `sdf_*` fan-out, so cheaper).

---

## 8. The exact change list (surgical)

1. Add `fn pt_fresnel_dielectric(cos_i, eta)` near `pt_fresnel_schlick` (`:551`).
2. Add `fn pt_sample_dielectric(n_geo, v, ior, tint, entering, seed, out_dir, out_inside) -> vec3` near
   `pt_sample_bsdf` (`:1960`) — smooth delta lobe, Fresnel branch, TIR→reflect, tint on transmit.
3. In `pass_pathtrace_tri`: add `var inside = false;` and `var glass_bounce = 0u;` before the loop (`:2104`).
4. Compute `n_geo_raw` (un-faced geometric normal) at the hit alongside `n` (`:2130`).
5. Read `shader_type`, `ior`, `glass_tint` from `mat` (in scope at `:2131`); compute `is_glass`.
6. Gate NEE (`:2265`/`:2268`), env NEE (`:2273`), and the lean `pt_restir_gi ... break` (`:2280`–`:2288`)
   with `!is_glass` so glass falls through to continuation.
7. Insert the glass branch (§1) just before the generic `pt_sample_bsdf` call (`:2325`), using `continue`,
   with the corrected side-aware ray offset and `glass_bounce` budget + cap.
8. No struct / stride / CPU-upload change.

---

## Architecture summary (5–6 sentences)

The live triangle path tracer (`pathtrace_tri_entry.wgsl`) currently has only two reflection lobes
(GGX + cosine diffuse) selected by `spec_prob`, never reads `ior` or `opacity` in shading, and force-faces
every normal to the camera — so it is single-surface and cannot refract. Real glass adds a THIRD, delta
transmission lobe implemented as a dedicated `pt_sample_dielectric` that reads the raw geometric normal to
distinguish entering vs exiting, tracks a single `inside` medium bool (solid, single-medium v1), and picks
reflect-vs-transmit by exact dielectric Fresnel with TIR→reflect. Because the branch is chosen by Fresnel
probability, throughput needs no `/pdf` and no eta² factor on a closed solid — only the transmit-side
`glass_tint` multiply, which is the exact seam where Beer-Lambert absorption later swaps in. The lobe is
gated by `shader_type == Glass` (matching the raster G-buffer trigger), bypasses the diffuse NEE/GI-cache
machinery, spawns the next ray on the correct side, and runs on a separate `glass_bounce` budget so glass
doesn't starve the diffuse depth. v1 ships SMOOTH glass on the deep continuation path; rough (frosted)
glass and the interactive lean-mode fall-through are the immediate fast-follows.

Plan path: `docs/planning/PT_TRANSMISSION_PLAN.md`

## Owner-decision list
1. **v1 scope: smooth-only (recommended) vs rough-capable glass.** Smooth is deterministic + testable and
   is the real unlock; rough is a fast-follow.
2. **Solid single-medium `inside` bool (recommended) vs thin single-surface.** Solid is barely more code,
   correct on closed meshes, and is exactly the state absorption needs (no rework).
3. **Separate `glass_bounce` budget (recommended, default MAX_GLASS_BOUNCE = 8) vs counting glass against
   `pt_max_bounces`.** Separate budget prevents black solids under low bounce counts.
4. **eta² transmission factor: OMIT in v1 (recommended, cancels on closed solids) vs include.** This is the
   textbook glass-brightness knob; include only if a thin/single-interface case ships.
5. **Interactive lean mode (`pt_max_bounces <= 2`): let glass fall through to the deep loop (recommended)
   vs require ≥3 bounces when glass is present.** Without a decision here glass is invisible in the
   interactive viewport.

## Single biggest risk
**The lean GI-cache path `break`s at bounce 0 (`:2288`), so glass silently does nothing in the default
interactive viewport** — the feature can look "not working" even when the math is correct. The plan forces
glass primary hits onto the deep continuation loop (decision #5); this MUST be resolved or the demo fails.
