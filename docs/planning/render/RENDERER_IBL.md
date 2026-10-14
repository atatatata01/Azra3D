# RENDERER_IBL.md — Image-Based Lighting + Reflections (2026-07-18)

Owner context: *"[Azra] Default HDR loaded: 360x180"* — the entire environment was a thumbnail,
and the ambientCG metal/roughness materials (MATERIALS_UX.md) had essentially nothing to reflect.
This slice ships industry-standard **split-sum IBL** on the existing env plumbing, the
**rfd modal-dialog crash-class fix**, and the **reflections evaluation**.

---

## 1. Inventory (what existed before — the map)

| Piece | Where | Verdict |
|---|---|---|
| Env storage | ONE storage buffer `env_buffer` (binding 21), equirect RGB f32 + 7 CPU **Gaussian** mips | KEPT layout, semantics upgraded |
| Env sampling | `sdf_lighting.wgsl` `#region ENV` (`sample_env_at_level/_mip/sample_env`) — shared by deferred (full+lite), fog, PT-tri (region-extracted) | KEPT — the one integration point |
| Deferred IBL | `deferred_entry.wgsl` PBR branch: diffuse = blurriest mip; specular = mip(roughness) × ad-hoc `fresnel × (1−r²)` | UPGRADED to split-sum |
| Background | deferred samples env for depth-0 pixels (already worked); lite pass writes gradient into G-buffer, deferred overrides with env | KEPT (rotation now applies) |
| Default HDR | `app_update.rs` loaded `Factory_Pumps_Env.hdr` — the 360×180 **preview** file, synchronously on the frame thread | FIXED (full-res preferred, worker decode) |
| Loads | `load_hdr_file` (modal rfd in-frame), library click, viewport drop, stress tests — all sync decode + CPU Gaussian mips on the frame thread | ALL routed through the worker |
| SSAO | half-res factor (binding 38), multiplies ambient/diffuse-IBL only (17e contract) | UNCHANGED — `kd` still includes `ssao_f` |
| SSR | REAL and WIRED: `pass_ssr` in post_process.wgsl, `ssr_pipeline`/`ssr_bg`, dispatched when `ssr_enabled && !interaction`, composited via post_flags bit 2 | Depth-metric bug FIXED (§6) |
| Deferred-FULL | `SDF_REFLECT` region (scene_sdf sphere-trace reflections) — unreachable: render mode is always `AnalyticalLite` → df_lite strips it | Evaluated §6, not revived |

## 2. What shipped (split-sum IBL)

**Buffer layout** (binding 21, unchanged addressing + a tail):
`[L0: w*h*3][L1]…[L6][SH9: 27 floats]`
- **Mips 1..6 = GGX-prefiltered on the GPU** (`graphics/src/env_prefilter.wgsl`, roughness = mip/6,
  Hammersley + GGX importance sampling, NoL-weighted — the split-sum FIRST term). Dispatched from
  poll, **one mip per frame** (Frame Budget), lowest roughness first. Worker-computed **box mips
  upload immediately as placeholders** so nothing renders black while the 6 dispatches run.
- **SH9 irradiance** (diffuse term): projected on the load worker (`env_ibl.rs::project_sh9`),
  cosine-convolved and normalized so a uniform env returns itself (same scale as the old
  blurriest-mip lookup — no relighting jump). Evaluated in `sdf_lighting.wgsl::env_sh_irradiance`.
  Chosen over a 32² irradiance cubemap because it rides the EXISTING buffer/binding (zero BGL churn).
- **BRDF LUT** (split-sum SECOND term): 128², 256 GGX samples/texel, generated once on a startup
  worker thread (`generate_brdf_lut`), uploaded in poll to `brdf_lut_buffer` (**deferred binding
  39**, the one new binding). Until the upload lands, `GpuUniforms.env_lut_res == 0` and the shader
  answers with the analytic Karis approximation — never a wrong/black window. *(Deviation from the
  prompt's "512² RG16F texture, GPU compute": a storage buffer needs no sampler/texture plumbing
  and the CPU generator is unit-testable headlessly; 128² is visually identical for this smooth
  integrand — the LUT is still "generated once at startup, cached".)*
- **Deferred PBR** (`deferred_entry.wgsl`, full + lite variants):
  `ambient_spec = prefiltered(reflect(V,N), r·6) · (F0·LUT.x + LUT.y)`,
  `ambient_diff = albedo · SH9(N) · (1−metal) · AO · SSAO · intensity`. Emission unaffected.
  The old `fresnel × (1−r²)` weighting is gone (over-bright at grazing, energy leak on rough).
- **Rotation + intensity**: `env_rotation` (radians) + `env_lut_res` added to `GpuUniforms`
  (592 → 608 bytes; Rust `types.rs` + `sdf_shared.wgsl` copy — the raymarch-lite/post-process
  copies are prefixes and stay valid). Rotation is applied in ONE place (`env_rotate` inside
  `sample_env_at_level` + `env_sh_irradiance`) so IBL, background, PT-tri and fog all turn
  together. UI: **Intensity** (renamed Brightness) + **Rotation (0–360°)** sliders in the render
  panel's Light & Env section. Runtime-only (save-format red line).
- **Loads**: `.hdr` (existing `parse_hdr`) + **`.exr`** (image crate, `exr` feature added) +
  LDR fallback (sRGB→linear). Decode + box mips + SH9 all run on a **worker thread**
  (`request_env_load`); install (buffer create/upload, bind-group invalidation) happens ONLY in
  `poll_env_ibl()` (Frame Ordering). Stale results are dropped by `env_load_gen` (bumped on
  New/Load project). Width capped at 8192 (user-input guard). Default startup load now prefers
  `Factory_Pumps_2k.hdr` / `Factory_Pumps.hdr` over the `_Env` thumbnail (last resort).

**New files:** `app/src/env_ibl.rs` (CPU core + worker + poll install + prefilter dispatch),
`graphics/src/env_prefilter.wgsl`, `graphics/src/env_prefilter_gpu_test.rs`,
`app/src/file_dialog.rs` (rider).

## 3. Rider — rfd modal-dialog crash class (FIXED)

All 5 map-load sites + the HDRI load opened `rfd::FileDialog` **modally inside the egui frame**
(Windows re-entrancy → BorrowMutError class; flagged in MATERIALS_UX.md §E4.3 as the prime
suspect for the owner's unexplained crash). Structural fix: ONE shared `FileDialogService`
(`file_dialog.rs`) — the picker AND the image decode run on a worker thread; outcomes are drained
in poll into the standard queues (`material_map_loads` / `request_env_load` / AI service).
One dialog at a time (`busy` latch + toast). Converted sites:
`material_editor.rs` (Prim + BodyMaterial albedo), `properties_material_node.rs` (graph albedo +
N/R/M data maps), `properties_sdf_material.rs` (Analyze Image), `properties_render.rs` (HDRI).
Out of scope (not map/env paths): project save/load, export, macro dialogs — same class, listed
for a follow-up sweep.

## 4. Tests / proofs

- `env_ibl` unit tests (app --bins): **SH9 uniform-env round-trip** (energy: irradiance == env for
  every normal), **top-light directionality**, **BRDF LUT analytic corners** (smooth head-on
  A≈1/B≈0; A,B ≥ 0; A+B ≤ 1 everywhere; grazing fresnel bias grows), **chain layout constancy**
  (constant env stays constant through all 7 levels; non-POT dims; SH DC value).
- `env_prefilter_gpu_test` (graphics, real device, production BGL): constant env passes through
  the GGX prefilter unchanged (importance weights normalized), **level 0 and the SH tail are
  untouched by a mip dispatch** (addressing proof), roughness-1 keeps top/bottom ordering.
  Skips without an adapter.
- `validate_env_prefilter_shader` Naga parse+validate; all existing Naga variants green.
- Gates: `cargo build -p app` clean · graphics **45** · render **33** · app --bins single-threaded
  **236** (232 + 4 env_ibl) · core_math untouched. Goldens/pick untouched.
- **Owner-verified only:** the look (metals reflecting with roughness-correct blur), slider feel,
  dialog behavior — I cannot run the app.

## 5. Perf notes

- Steady-state deferred cost ≈ unchanged: SH9 eval (27 MADs) replaces a trilinear buffer sample;
  the LUT fetch replaces the old fresnel math; specular chain sampling is the same `sample_env_mip`.
- HDRI load: decode/mips/SH on the worker (frame thread does one buffer upload); GGX prefilter =
  6 GPU dispatches, one per frame (4K env mip1 ≈ 2M texels × 32 samples — a few ms of GPU, once).
  No >50 ms frames on load.
- BRDF LUT: one worker thread at startup (~128²×256 ≈ 4M iters), uploaded once (128 KB).

## 6. Reflections evaluation (Part 2 — the report)

**(a) Deferred-FULL / SDF_REFLECT** — the one-bounce `scene_sdf` sphere-trace inside
`deferred_entry.wgsl`:
- **Why it never runs:** `resolve_render_mode` returns `AnalyticalLite` whenever
  `inputs.unified_raster` (always, post-P0.2) → `use_lite_deferred` is always true →
  `deferred_lite_pipeline`, whose build strips `SDF_REFLECT` (`DF_LITE_EXCLUDE`). Interaction
  and MatCap/SolidHdr force lite anyway.
- **What enabling it would cost:** compile deferred-FULL (25–43 s cold, lazy path exists) + per
  pixel up to 48 `scene_sdf` steps + central-difference normals + `eval_material` at hit — Naga
  fully inlines `scene_sdf`, which is why the region was quarantined. At 2048×1152 with real prim
  counts this is the TDR-class cost the lite split exists to avoid.
- **The disqualifier (not perf):** `scene_sdf` sees ONLY analytical SDF prims. Poly bodies, voxel
  sculpts, and even the SDF's own DC-baked mesh detail don't exist in it — reflections would show
  a *different scene* than the raster viewport renders (Display-Invariant-adjacent mismatch).
  **Verdict: not the right investment. Do not revive.**
- **(b) SSR** — real machinery, wired end-to-end (`pass_ssr` → `ssr_texture` → post composite,
  toggle in 3 UIs, OFF by default): world-space march, 32 steps × 0.4 units, gated to
  metallic > 0.15 && roughness ≤ 0.6, fresnel+edge/distance/roughness fades. It reflects
  whatever is on screen — all body types. Found + FIXED one real defect: it compared marched
  forward-Z against the G-buffer's ray-distance (metrics diverge off-center → edge artifacts).
- **Shipped this slice:** the split-sum IBL **is** the reflection story (roughness-correct
  env reflections on everything), plus the SSR depth-metric fix so the existing toggle is
  correct when used.
- **Options for the next slice** (effort estimates):
  1. **SSR quality pass** (~1 day): binary-search refinement after hit, screen-space (DDA)
     stepping instead of fixed world steps, roughness→mip blur of the reflection fetch, IBL
     fallback INSIDE the SSR mask (currently mask=0 → plain IBL, which now looks right anyway).
     Cheap, benefits all body types. **Recommended next.** → **BUILT 2026-07-18d, see §7.**
  2. **SSR default-ON at quality ≥ high** (hours): after (1); needs an interaction latch review
     (already skipped during interaction).
  3. **Planar/probe reflections** (~1 week): real mirrors for floors; new render pass — only if
     the owner asks for mirror-floor shots.
  4. **PT ground truth** (exists): the triangle path tracer already gives true reflections for
     look-dev stills; no work needed.

## 7. SSR quality pass (2026-07-18d — §6 option 1, built)

Goal: make the existing toggle worth turning ON (stays OFF by default — owner will A/B).
All in `pass_ssr` (post_process.wgsl) unless noted.

**March — screen-space DDA** (replaces 32 × 0.4-unit world steps):
- Segment = projections of [surface + ε, surface + R·60u clipped to the near plane]; steps are
  UNIFORM IN PIXELS (≥2px stride, budget `ssr_max_steps` 8..128), world point per step via
  perspective-correct interpolation (`mix(P/z)/mix(1/z)`; ortho = lerp). Kills the
  over/undershoot banding of fixed world steps. Deterministic per-pixel hash jitter
  (NO temporal jitter — readback-proven byte-identical across dispatches).
- **Binary refinement**: 4 bisection iterations on the crossing, bounded to the LAST stride
  (a rejected/passed-behind surface is never re-found). Crisp contacts.
- **Adaptive thickness**: `ssr_thickness (0.35) + 1.5×(per-step depth advance) + 1% depth`.
- **Validity**: backface rejection `dot(sample_n, R) < 0` (structurally rejects self-hits too),
  near-plane clip, toward-camera fade `smoothstep(-0.95,-0.55, dot(R, view_ray))`.

**Roughness-aware**:
- Resolve = 4-tap cone blur around the refined hit, radius ∝ `roughness × 0.35 × marched pixel
  distance` (clamped 20px), background-taps excluded. *(Chose jittered cone taps over an HDR mip
  chain: the hdr texture has no mips and adding a per-frame mip build for one consumer fails the
  Frame Budget sniff test; 4 taps at ≤0.2ms total is the pragmatic call.)*
- Confidence fades 1→0 over roughness 0.45→0.7; above 0.7 the pixel early-outs. IBL takes over
  smoothly — the standard blend.
- The old hard `metallic < 0.15` gate is GONE: the march gate is now envelope LUMINANCE
  (`max(F0·A+B) ≥ 0.005`), so glossy dielectric floors reflect too (the owner test scene).

**Composite correctness — energy-conserving delta** (the load-bearing change):
- pass_ssr outputs `rgb = envelope × conf × (traced − IBL_specular)`, `a = conf`, where
  `envelope` = the EXACT split-sum second term deferred applied (ENV_BRDF region extracted from
  deferred_entry.wgsl — LUT binding 39 + Karis fallback, not a re-derivation) and
  `IBL_specular` = the EXACT deferred term (`sample_env_mip(R, r·6) × brightness`, ENV region
  extracted from sdf_lighting.wgsl, env binding 21). pass_tonemap ADDS the delta: at conf 1 the
  IBL specular is fully swapped for the traced reflection, at 0 nothing changes, in between it
  cross-fades — never double-adds energy. Per-pixel IBL fallback inside the mask = the subtraction
  scaling with conf (closes §6's "IBL fallback INSIDE the SSR mask" item).
- Confidence is opacity-scaled (deferred alpha-blends its specular by `mat_opacity` — the swap
  must scale identically or transparent pixels over-subtract).
- Composite MOVED BEFORE bloom + fog: reflections now bloom and get fogged like everything else.
- SSAO untouched by SSR in both directions (specular ≠ ambient — the 17e contract): deferred's
  `ssao_f` multiplies only diffuse ambient; the SSR delta never touches it.
- Rider: post_flags bit 2 now excluded in PathTrace mode — it used to composite a STALE ssr
  texture over the PT accumulation (the pass never dispatches in PT).

**Found live — post_process GpuUniforms copy truncation (PPUNIFORMLAYOUT)**: the module's local
struct copy skipped the LOD (512–575) + atlas (576–591) blocks, so appended fields read the
wrong bytes (ssr_intensity read foveated_enabled == 0 → silent all-zero SSR, no validation
error). Copy completed to the full layout; `validate_post_process_shader` now asserts the
Naga-reflected WGSL struct span == `size_of::<GpuUniforms>()` — the whole "stale prefix copy"
bug class fails the suite now.

**Plumbing**: GpuUniforms 608 → 624 (`ssr_max_steps`/`ssr_intensity`/`ssr_thickness` + pad; all
other WGSL copies remain valid prefixes — no sdf_* recompile). ssr_bgl += albedo (14), env (21),
BRDF LUT (39); ssr_bg rebuilt via the texture block (rebuild gate now also keys on
`ssr_bg.is_none()`), invalidated at all three env/LUT buffer recreation sites in env_ibl.rs.
Controls (render panel, existing pattern): **SSR Quality** (16–96 steps) + **SSR Intensity**
(0–1). Runtime-only — save-format red line. Profiler: existing PASS_SSR slot unchanged.

**Proof** (`graphics/src/ssr_gpu_test.rs` — production shader module, bind group mirrors the
production ssr_bgl entry-for-entry): red cube on a mirror floor, G-buffer synthesized with the
shader's own camera math, CPU mirror-trace oracle. Results: **93.5%** of expected-reflection
floor pixels get conf > 0 with red-dominant color; **0** false-confidence pixels of 30,834
sky-mirroring floor pixels; background exactly vec4(0); delta non-negative with env off;
**byte-identical** across dispatches. Before the pass, the same probe scored **0%** (the layout
bug) — and the old shader would fail the oracle shape anyway (metallic gate + full-pixel mix).
Perf: **0.18 ms/dispatch wall-clock at 1920×1080 / 48 steps on the RTX 4090** (upper bound —
includes submit overhead; in-app PASS_SSR timestamp is authoritative). Full-res fits the ≤1.5 ms
budget with ~8× headroom → half-res + bilateral upsample NOT needed.

**Owner's eyes needed (honestly)**: the LOOK — contact-reflection crispness on real scenes,
cone-blur feel vs Blender/Modo, fade behavior while orbiting (SSR still pauses during
interaction — §6 option 2's latch review is still open), and whether default-ON at quality ≥
high is wanted after A/B. Manual test: reflective floor (metal 1 / rough 0.05–0.3) + object on
it, PBR mode, toggle SSR — crisp contact reflections that blur as floor roughness rises, fade
gracefully at screen edges, and hand off to IBL above roughness 0.7; fog ON now fogs the
reflections; Intensity slider cross-fades SSR↔IBL.

## 8. Flagged follow-ups (not built, per instructions)

- **Viewport Tonemapper selector — BUILT (2026-07-18e).** The HDR→LDR chain map (inventory):
  deferred (lite/full) writes linear HDR + `alpha = is_scene` → `pass_ssr` (delta texture) →
  **`pass_tonemap`** (post_process.wgsl, the sole LDR writer): debug passthrough → viewport DOF
  (bit 5) → **SSR delta add** (bit 2, kept BEFORE bloom/fog — §7 ordering preserved) → **bloom**
  composite (bit 1) → **fog** composite (bit 3) → compositor effects (comp[0], off by default) →
  scene pixels: [compositor white balance] → **exposure** (`config.exposure` ×
  2^comp_exposure) → **tonemap curve** → [compositor grading — the only contrast/saturation in
  this chain, post-tonemap LDR, comp-only] → `linear_to_srgb` → mask preview / body outline /
  dither → LDR → **FXAA** (LDR-space) → upsample → egui. Background pixels with alpha=0 (user
  gradient, MatCap) bypass exposure/curve; the HDRI env background has alpha=1 and IS tonemapped
  (bright sun rolls off — the point). `env_contrast`/`env_saturation` act on ENV SAMPLING
  (sdf_lighting), not in this chain; nothing was moved.
  - **Selector**: Render panel → "Tonemapper" — `0 ACES (Classic)` (Narkowicz fitted — the
    exact curve the path hardcoded before, still the DEFAULT: scenes keep their look),
    `1 AgX` (canonical inset→log2→sigmoid→outset, was already in the file for the compositor),
    `2 Filmic (Hable)` (NEW `hable_tonemap`: Uncharted 2 shoulder, exposure bias 2, white point
    11.2, LINEAR output — deliberately not the compositor's gamma-baked Hejl `filmic_tonemap`),
    `3 Reinhard`, `4 PBR Neutral` (Khronos), `5 None/Linear` (raw clamp, debugging).
    All curves take exposed linear HDR, return linear [0,1]; gamma stays after the curve.
  - **Plumbing**: `GpuUniforms.tonemapper` repurposes `_pad_ssr0` (size stays 624 — the
    PPUNIFORMLAYOUT span guard is untouched and green; no sdf_* file touched, no BGL churn).
    `RenderSettings.tonemapper` is runtime-only (save-format red line, ssr_max_steps precedent).
    Compositor path (comp[92]) unchanged and takes precedence when the compositor is ON.
  - **Proof** (`graphics/src/tonemap_gpu_test.rs` — production module through a bind group
    mirroring `tonemap_bgl` entry-for-entry, gradient HDR 2^-8..2^8 gray + saturated red ramps):
    every curve monotonic + [0,1] + deterministic; shoulder at L=1.5→4: ACES 0.944→0.987,
    AgX 0.927→0.970, Hable 0.811→0.961, all < 1 and climbing while **Linear clips flat at 1.0**
    (numeric no-flat-white-patches proof); **AgX max hue shift 0.11°** over 151 red-ramp pixels
    (bound 30°); Classic vs CPU ACES-fitted reference max err 5.4e-4 (f16) — default look
    unchanged. Perf: pure ALU swap inside the existing pass (same uniform switch class as the
    compositor's), no new textures/passes.
  - **Owner decision (one line)**: flipping the default to AgX =
    `app_types/src/types_render.rs` → `RenderSettings::default()` → `tonemapper: 0,` → `1`
    (ACES fitted = `2` is not there — it IS Classic; AgX=1, Hable=2). Recommendation below §10.
  - **Known divergences**: optional CUDA `tonemap.cu` hardcodes ACES (= the default; the
    env_rotation precedent). Compositor "Filmic" (Hejl) double-gammas — pre-existing, flagged.

- **Per-project HDRI persistence — BUILT (2026-07-18d).** The `FL4ENVIR` self-delimiting FL4 tail
  section (sibling of `FL4ORDER`/`FL4MATTX`/`FL4MATX2`; written LAST so older readers stop cleanly)
  carries `EnvRecord { path: String, brightness, rotation_deg }` — path (never pixels), version
  stays 7, file byte-identical when no HDRI is loaded. *Deviation from the shape sketched above:*
  contrast/saturation/blur are NOT in the record — they already persist as in-blob v3
  `RenderSettings` fields; duplicating them would create two sources of truth. `brightness` IS
  duplicated (same value as the blob field at save time — the owner's authored trio travels as one
  unit); `rotation_deg` exists ONLY here (a blob field would be the bincode field trap).
  - Save: `ProjectFile::from_app` records `env_source_path` (set on env install, cleared with the
    env), relative-ized against the project dir when the HDRI is under it (`env_path_for_save`),
    absolute otherwise.
  - Load: `load_scene_from_path` → `restore_env_from_record` AFTER `apply_to_app` (which bumps
    `env_load_gen` + queues the clear, so the re-kicked `request_env_load` worker is the sole
    valid load — the existing generation guard drops anything stale). Missing file → toast naming
    it + default (cleared) env; the load never fails. Rotation resets to 0 on load/new when no
    record (it is authored env state now).
  - Gates: `env_save_tests` (round-trip, no-env-no-magic byte-compat, tail-section coexistence +
    FL4ENVIR-last, path relativize/resolve) + `dc_gate0_committed_ref_fl4_loads_unchanged`
    (committed pre-change ref .fl4 through the new loader). App --bins: 240 green.
- **Dialog sweep**: remaining rfd sites outside map/env paths (project/save/export/macro) still
  open in-frame — same helper is ready for them.
- **CUDA PT** (feature-gated, optional): builds its own uniforms; env rotation does not apply
  there (kernel reads its own struct). Cosmetic divergence, noted.
- **`env` role in the content library** still points at `*_Env.hdr` preview files for some packs;
  selection logic untouched (content_library.rs is a merge-risk zone — ONE minimal edit made:
  the load mechanism). If packs carry full-res HDRIs under another role, prefer it in a
  library-owned change.

## 9. Owner manual test

1. Launch → log shows `Default HDR queued: …Factory_Pumps…` (full-res if present) and the
   background shows the HDRI; a fraction of a second later glossy mips sharpen (prefilter).
2. Render panel → Light & Env → **Load HDRI…** → pick a 2–4K `.hdr` or `.exr` → UI stays live
   (no stall), toast on install → background shows it.
3. A metal material (ambientCG, metalness 1, roughness map): reflections show the environment,
   blur increasing with roughness — rough=0 mirror-sharp, rough=1 diffuse-ish.
4. **Rotation** slider: background AND reflections turn together. **Intensity** scales both.
5. SSAO toggle still grounds contacts (darkens creases in the diffuse ambient, not the sun).
6. Open several map Load… dialogs in a row (albedo/normal/roughness) + the HDRI dialog — no
   crash, no frame hitch while the dialog is open; a second Load while one is open just toasts.
7. New Project → env clears to the gradient; loading a project keeps working (no stale env).
8. PT mode: background/lighting rotates with the same slider (WGSL PT shares the sampler).

## 10. Session log

- **2026-07-18f** — GTAO + bent normals (§11): horizon-based AO with a bent
  normal packed into the spare b,a of the existing binding-38 half-res texture
  (zero new G-buffer target / deferred binding / BGL churn). Deferred diffuse IBL
  now samples `env_sh_irradiance(bent_normal)` — the flat-cube fix — plus Jimenez
  multi-bounce on the diffuse ambient and roughness/N·V specular occlusion on the
  specular IBL. SSAO kept as a cheaper tier (shares the module, resources, and
  output layout; writes b,a=0 → deferred falls back to the geometric normal).
  Panel: AO checkbox + GTAO/SSAO quality radio (runtime-only `use_gtao`, default
  GTAO). Gates: app build clean, graphics 50 (df-lite + df-full both compile),
  render 35 (+2 GTAO GPU readback/timing), app --bins 240. Readback (cube-over-
  floor, real raster G-buffer): open floor AO 0.990 / bent·Y 0.941; contact AO
  0.469 / bent·toward_cube −0.658 < geo 0.000 (bends away — load-bearing); two
  cube faces |Δbent| 0.660, base AO 0.541 < open 0.996; deterministic. GTAO
  0.162 ms @1080p half-res (SSAO 0.071 ms) — well under the ≤2 ms budget.
- **2026-07-18d** — SSR quality pass (§7): screen-space DDA + 4-iter binary refinement +
  roughness cone blur/fade + smooth edge/backface/toward-camera rejection + energy-conserving
  delta composite (extracted ENV/ENV_BRDF regions, exact IBL-specular swap by confidence,
  moved before bloom/fog) + PT stale-composite guard + Quality/Intensity controls (runtime-only).
  Found+fixed the post_process GpuUniforms truncated-copy class (layout span guard added).
  Gates: app build clean, graphics 48 (2 new GPU readback tests, 93.5% oracle / 0 false-positive /
  deterministic), render 33, app --bins 236. 0.18 ms @1080p/48 steps on the 4090 (budget 1.5 ms).
- **2026-07-18d** — per-project HDRI persistence BUILT (§7 first bullet): `FL4ENVIR` tail section
  (path relative-to-project + brightness + rotation, never pixels), `env_source_path` runtime
  tracking, load re-kick through `request_env_load` (generation-guarded), missing-file toast +
  default env. Rider: library open-scan moved to a worker (`library_scan.rs`) — the sync scan +
  full-res matcap decode + GPU upload ran in `draw_content_library` (Frame Ordering violation /
  the filed open-stall); GPU upload now in `poll_library_scan` only. Gates: app build clean,
  app --bins 240 (236 + 4 env), committed ref .fl4 gate green, core_math/graphics untouched.
- **2026-07-18c** — this slice: split-sum IBL (GPU GGX prefilter chain + SH9 irradiance + BRDF
  LUT binding 39 + rotation/intensity), worker-thread HDRI loads (.hdr/.exr), rfd modal-dialog
  crash-class fix (6 sites via file_dialog.rs), SSR depth-metric fix, reflections evaluation
  (deferred-FULL: do not revive; SSR: fixed + quality-pass recommended). Gates: app build clean,
  graphics 45, render 33, app --bins 236, env_prefilter GPU probe green.

## 11. GTAO — Ground Truth Ambient Occlusion + bent normals (2026-07-18f)

**The problem (owner-observed):** a white cube under a bright HDRI looked FLAT.
The diffuse IBL (`env_sh_irradiance(normal)`) floods every face with the same
SH9 irradiance, and the only occlusion was SSAO — a scalar contact-darkening
applied uniformly. There was NO *directional* occlusion: a face never darkened
by how much environment it could actually SEE. GTAO + bent normals is the fix.

**What GTAO computes:** for each pixel, sweep N azimuthal slices; in each slice
plane (spanned by the view vector V and an in-plane tangent T) march the depth
buffer to both horizons, integrate the cosine-weighted visibility arc against
the PROJECTED geometric normal (Jimenez 2016). Two outputs: the **AO factor**
and the **bent normal** — the average unoccluded direction, weighted per slice.

**The load-bearing integration:** deferred diffuse IBL samples the SH9
irradiance with the **bent normal** instead of the geometric normal
(`deferred_entry.wgsl` — `env_sh_irradiance(ao_bent_normal(...))`). Faces near
occluders sample the sky they can actually see → real directional self-shadowing
from the environment. Numerically proven: two cube faces with different exposure
have bent normals differing by |Δ|=0.660; a contact bent normal bends away from
the occluder (`bent·toward_cube −0.658` vs geometric `0.000`).

**Where the bent normal lives (zero new plumbing):** the existing SSAO output is
a half-res `rgba16float` texture at deferred binding 38 (`r`=AO, `g`=depth, `b,a`
previously unused). GTAO packs an **octahedral-encoded bent normal into b,a**.
No new G-buffer target, no new deferred binding, no BGL churn, no VRAM doubling.
`b=a=0` is the "no bent normal" sentinel: SSAO/off/background/non-raster pixels
leave it 0, and deferred falls back to the geometric normal (identical lighting
to the pre-GTAO path). The measure-zero collision (a bent normal of exactly world
+Z encodes to (0,0)) is harmless — it coincides with open pixels where bent ≈ geo.

**Also shipped in deferred (both from the same AO factor):**
- **Multi-bounce diffuse occlusion** (`multi_bounce_ao`): the Jimenez cubic fit
  remaps the scalar visibility per-channel, tinted by albedo — recovers the
  inter-reflection a single-bounce AO crushes. Reduces to identity when open.
  Feeds the diffuse-IBL ambient slot the SSAO used (17e contract intact: ambient/
  IBL ONLY, never direct sun, never Solid, never SDF-march pixels).
- **Specular occlusion** (`specular_occlusion`, Lagarde/Frostbite
  `pow(NdV+ao, exp2(-16r-1)) - 1 + ao`): multiplies the specular IBL so metals
  don't over-brighten at grazing angles in occluded areas.

**SSAO subsumed, not deleted.** GTAO *is* a better AO, but SSAO is kept as a
cheaper quality tier — it shares the module, the resources, the params buffer,
and the 3 ping-pong bind groups; only the 3 compute pipelines differ
(`AoMode::{Ssao,Gtao}` selects them in `render::ssao::dispatch`). One owner, one
binding, no double-apply. Panel: the "Ambient Occlusion" checkbox + a GTAO/SSAO
quality radio (`RenderSettings.use_gtao`, runtime-only, default GTAO). OFF skips
all dispatches and white-fills the factor texture once (the 17d behavior).

**Design decisions (made + documented):**
- Half-res + depth-aware bilateral upsample (the SSAO precedent), extended to
  denoise the bent normal (b,a) with the same bilateral weights. Single-frame
  stable — interleaved-gradient-noise slice rotation + step jitter, deterministic,
  NO temporal filter (TAA is a separate future slice; readback-proven byte-
  identical across dispatches).
- World-radius adaptive (screen footprint scales with distance via world-space
  tap projection — no magic pixel radius). Horizon falloff over one radius
  (`thickness`) is the range check that kills halos and softens thin occluders.
- Resources created in the established viewport resize block alongside
  `gbuf_normal` (Frame Ordering; co-located so no cached bind group can dangle —
  the SSAO precedent, no separate generation counter needed). `tw!()` only on the
  dispatching main pass (timestamp safety). GTAO defaults: 3 slices × 6 steps.

**Proof** (`render/src/ssao.rs` — `gtao_readback_cube_over_floor`, real raster
G-buffer via `RasterPipeline` + the production `dispatch()`, cube-over-floor,
512², zero-light): (a) open floor AO **0.990** (≥0.9, no acne), bent·Y **0.941**
(≈ geometric normal); (b) floor-cube contact AO **0.469** (≤0.75), bent·toward_cube
**−0.658** < geometric **0.000** (bent AWAY from the cube); (c) two cube faces
|Δbent| **0.660**, floor-occluded base AO **0.541** < open face **0.996**;
(d) two runs byte-identical. Timing (`gtao_gpu_timing_1080p`): **0.162 ms** for
the 3-pass chain at 1920×1080 half-res (SSAO tier 0.071 ms) — under the ≤2 ms
budget. Deferred WGSL validates in both df-lite and df-full Naga variants.

**Files:** `render/src/ssao.wgsl` (GTAO passes + oct encode + `SsaoParams`
GTAO fields), `render/src/ssao.rs` (`AoMode`, GTAO pipelines, `dispatch(mode)`,
GTAO readback + timing tests), `graphics/src/deferred_entry.wgsl` (bent-normal
diffuse IBL + multi-bounce + specular occlusion + decode helpers),
`app_types/src/types_render.rs` (`use_gtao`), `app/src/properties_render.rs`
(quality radio), `app/src/viewport_render.rs` (GTAO params + mode dispatch).

**Owner's eyes needed:** the LOOK — a white cube + floor under a bright HDRI now
self-shades (faces turned from the environment's bright side darken), contact
corners occlude, the flat floor stays clean, no shimmer while orbiting. Toggle
GTAO→SSAO for the A/B (SSAO = the old flatter look; the diffuse IBL reverts to
the geometric normal). Owner may retune AO Strength / Radius and the slice/step
defaults for feel.
