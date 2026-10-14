# SSGI Plan — screen-space one-bounce indirect (Grok work order)

**Date:** 2026-08-06 · **Branch:** `master` (release trunk, repo ROOT) · **Status:** not started

**Owner ask:** *"If you want a fake GI step next, the usual fit for this stack would be SSGI or a
simple probe/SH cache — neither is in the product today."*

Read `CLAUDE.md` before starting. This document does not repeat its rules; it assumes them. Where
this document and `CLAUDE.md` disagree, **STOP and report the conflict** (§1 R1) — do not pick one.

---

## Execution status

> ## ⏸ PARKED 2026-08-06 — owner-directed
>
> **Owner:** *"QUALITY IS GARBAGE, WILL NEED TO EXPOSE ALL SETTING TO SEND QUALITY TO THE OTHER
> LEVEL, I AM BUILDING A PROGRESSIVE PBR RENDER ENGINE ON AN OTHER SESSION, SO FOR NOW PARK THE
> WORK."*
>
> Gates 0–3 and VG-A are **done and green**; Gates 4–7 and VG-B–E are **not started**. Nothing here
> is abandoned or half-applied — the feature ships **OFF** by default, is byte-identical to
> pre-feature when disabled (Gate 1, proven), and §4.10 carries an 8-step source removal list if it
> ever needs backing out entirely.
>
> **Why parked, in the owner's terms:** VG-B showed the technique working but the *image* is not at
> the bar. That is not a bug in this feature so much as a statement about where the whole raster
> viewport sits, and it interacts directly with the **progressive PBR renderer being built in a
> separate session**. Deciding SSGI's denoise/temporal strategy (Gate 4) before that architecture
> lands would be designing against a moving target: the single biggest lever on SSGI quality is
> whether a progressive accumulator exists, and that is exactly what the other session is building.
>
> **Resume conditions.** Take this off the shelf when (a) the progressive/accumulation architecture
> is settled, so Gate 4 can target it rather than guess, and (b) the quality plan below has an
> owner-approved direction. Gate 4's first task changes materially depending on (a): with an
> accumulator, §4.3's mandated deterministic sampling **inverts** — the correct move becomes varying
> the sequence per frame and letting accumulation converge it, instead of freezing it to avoid
> boiling.
>
> **State to resume from:** `graphics/src/ssgi{.rs,_entry.wgsl,_gpu_test.rs}`, the composite in
> `post_process.wgsl`, the dispatch in `viewport_render.rs`, and the Developer Tweaks panel
> (Enable / Kill / force-constant / skip-blur / ray override / Intensity / Radius / HDRI Occlusion).
> 8 SSGI tests green; graphics 83, render 95, app builds.

**Revision 2 (2026-08-06)** — this plan was adversarially reviewed against the code by five
independent passes and **repaired**. The energy formulation (§4.2) was wrong in six ways and the
gate list could not fail; both were rebuilt. Owner-approved **visual gates** were added, because the
executing agent cannot see the application and the failure modes that matter most here are visual.
Read §5.0 before starting.

| Gate | Status | Notes |
|------|--------|-------|
| **0** Harness + **CPU oracle** + baseline | **✅ DONE 2026-08-06** | `graphics/src/ssgi_gpu_test.rs`, 2 tests, CPU-only so it runs without an adapter. Baseline below. |
| **0b** File remaining design-time defects | **partly DONE** | ortho depth convention **FIXED 2026-08-06** (§4.4); 3 entries left |
| **1** Neutral plumbing (+ `ensure_zero`, settings field) | **✅ COMPLETE** (numeric + VG-A) | Shader + `build_ssgi_wgsl()` + BGL + `AO_UPSAMPLE` region · `graphics/src/ssgi.rs` · `SsgiParams` · settings + `SsgiDebug` · `GpuCtx` fields · resize-block creation · dispatch (mode-gated, `tw!` inside the guard, slot 13 ex-`PASS_BAKE`) · `post_flags` bit 128 mirroring the dispatch gate · tonemap binding 62 + `ssgi_delta()` + the load-bearing negative clamp · Dev Tweaks section. Gate `ssgi_gate1_composite_is_neutral_and_hits_only_receivers`: fresh texture reads zero, zero-constant → exactly zero everywhere, 0.25-constant → **6462 receivers exact / 12738 non-receivers bit-zero**, deterministic across runs. |
| 🔍 **VG-A** it changes nothing | **✅ APPROVED 2026-08-06** | Owner: no change at zero delta; constant delta lifted geometry + the shadow on the floor MESH. The greyed shadow was a *correct* receiver result, not a leak — a flat additive delta lifts a dark shadow (~0.05→0.30) far more visibly than a lit surface (0.72→0.97). The built-in grid is structurally immune: deferred stores it with alpha 0 (`deferred_entry.wgsl:385`) and the composite is gated on `is_scene`. |
| **2** The gather (rays, hits, radiance) | **✅ PASSED** | Real march shipped: cosine hemisphere about the BENT normal, deterministic `ign` rotation, world-uniform stepping, adaptive thickness, backface reject, binary refinement, edge fade. Directional red bleed with distance falloff in all 4 configs. |
| **3** Energy — **CPU-oracle cross-check** | **⚠ PARTIAL — do not close** | **Orthographic: GPU matches the oracle to 2e-4 in both env configs** — f16 noise, i.e. the energy formula is verified end to end (bent normal, `.y` metalness, opacity, raw env radiance, `min(S, applied)`, shared ray set). **Perspective: near/mid probes exact to 4 decimals; the FAR probe diverges** (worst 0.114). Diagnosed as screen-space *resolvability*, not arithmetic: that probe's single contributing ray crosses the wall near the floor junction, where the pixel resolves to the other surface. Ruled out: step count (96), stride scheme, march reach, ray-path visibility. Modelling resolvability in the oracle was tried and REVERTED — doing it faithfully turns the oracle into a march, which destroys its independence. **Sabotages RUN: 2 of 4 left the gate GREEN.** (a) `.x` for `.y` → RED 0.365 ✓; (c) prefiltered mip for raw radiance → RED 0.227 ✓; **(b) geometric-for-bent normal → GREEN** (harness AO texture has a zero bent normal, so the two are identical here); **(d) dropping `min(S, applied)` → GREEN** (the clamp never binds in this scene — hits point along a dim horizon). Closing Gate 3 requires a real encoded bent normal in the AO texture + the same decode in the oracle, and an env with a bright horizon band so the clamp binds. |
| 🔍 **VG-B** the light bounces, and looks like light | ⬜ awaiting owner | blocks Gate 4 |
| **4** Denoise + upsample (vs full-res reference) | not started | |
| 🔍 **VG-C** no halos, no leaks, no boiling | ⬜ awaiting owner | blocks Gate 5 |
| **5** Perf, interaction, timestamps, resize/split/ortho | not started | |
| 🔍 **VG-D** it survives being used | ⬜ awaiting owner | blocks Gate 6 |
| **6** Settings + UI (+ save/load round-trip) | not started | |
| **7** Re-measure and report | not started | |
| 🔍 **VG-E** ship it or not | ⬜ awaiting owner | final; sets the default intensity |

**Every gate also ships a Developer Tweaks toggle** (§4.10, binding via R13) — one per stage, so
each can be isolated while debugging, plus a master **Kill SSGI** switch. §4.10 also carries the
source-level removal list, so the feature can be backed out cleanly rather than disentangled.

### Reported deviation — no background pipeline compile (2026-08-06)

§3.7 lists a background-compile arm (`PipelineReady` variant + `app_update.rs` thread + a
`poll_sdf_compile` install arm with the `is_none()` race guard) alongside the lazy `ensure_*`
fallback. **Only the lazy path was implemented.** Reporting it per R1 rather than leaving it to be
discovered.

Rationale: SSGI ships default-OFF, so the compile happens on the user's explicit first enable
rather than costing every user startup time for a pass most never turn on, and the warmup counters
stay untouched. Cost: the first frame after enabling compiles three pipelines **synchronously on
the frame thread, mid-encode** — a one-time hitch, and the Gate 1 review notes it can land inside
the `draft_compile` TDR-protection window. The frames before it lands are covered by
`ensure_zero()`.

**Gate 5 decides:** measure the first-enable hitch. If it is objectionable, add the background arm
then — the site list in §3.7 is the checklist. Do not add it earlier just to satisfy the list.

### Gate 0 baseline — published 2026-08-06

Floor probes at 0.10 / 0.70 / 2.20 world units from the red wall. `base` is the deferred-lit floor
radiance the delta will be added to; `oracleΔ` is the CPU oracle's §4.2 result. 320×240.

| Config | probe0 (near) | probe1 | probe2 (far) |
|---|---|---|---|
| persp / HDRI | `+0.163, −0.252, −0.447` | `+0.080, −0.128, −0.228` | `+0.040, −0.064, −0.114` |
| ortho / HDRI | `+0.120, −0.192, −0.342` | `+0.123, −0.188, −0.333` | `+0.040, −0.064, −0.114` |
| persp / no-HDRI | `+0.360, +0.024, +0.024` | `+0.180, +0.012, +0.012` | `+0.090, +0.006, +0.006` |
| ortho / no-HDRI | `+0.270, +0.018, +0.018` | `+0.270, +0.018, +0.018` | `+0.090, +0.006, +0.006` |

`base` is `0.7202` in every cell. Two things to read from this, both of which confirm the design
rather than merely "running":

- **With an HDRI the delta is strongly signed** — red gained, green and blue *removed*. That is the
  energy formulation working: the wall blocks blue-ish sky and replaces it with red bounce, so the
  subtraction has to be negative in the channels the sky owned. A formula that only ever adds light
  would show all-positive columns here.
- **Without an HDRI the delta is purely additive** — exactly the deliberate residual §4.2 documents
  (`L_env ≡ 0` when `env_width == 0`, while deferred still applied a full-strength flat ambient).
  It is recorded here so nobody later "discovers" it as a bug.

Adjacent probes may tie (ortho probe0/probe1 above): with `N_RAYS = 8` the hit count is quantized,
so the falloff assertion requires non-increasing neighbours plus a decisive drop across the full
range, not a strict drop between neighbours. A strict-neighbour assert would be camera-dependent.

---

## 0. Decision — SSGI, and why not the other two

The owner named two candidates. There is a third already written into
`docs/planning/render/RENDERER_ROADMAP.md:36-40` ("The AZRA power move — SDF-traced GI"). All
three were evaluated against what AZRA actually is: **a DCC modeling app whose geometry changes
several times per second, whose content is mostly poly and voxel rather than analytic SDF, and
which has no motion vectors.**

| | Screen-space GI | Probe / SH cache | SDF cone-traced GI |
|---|---|---|---|
| **Shades the surface actually displayed** | **yes** — the G-buffer *is* the displayed surface | yes | **NO — the killer. Owner-verified.** See below |
| Works for poly + voxel + imported meshes | **yes** — reads the G-buffer, which every representation writes | yes | **no** |
| Survives constant geometry edits | **yes** — nothing is cached | **no** — stale on every extrude | yes (rebakes with the field) |
| Needs motion vectors / TAA | **no** (see §4.3) | no | no |
| Adds a second render technique over shared pixels | **no** — one compute pass on the existing G-buffer | **yes** — probe capture is a second scene rendering | no |
| Save-format impact | none (runtime-only settings) | probe placement + bake would want persistence | none |
| Reuses shipped machinery | **heavily** — SSR march, GTAO bent normals, SSAO half-res + bilateral, `dispatch_blend` | little | the `SDF_REFLECT` sphere trace |

**Why not a probe / SH cache.** A probe grid is *stale by construction* in a modeling app. Every
extrude, every boolean, every sculpt stroke invalidates the region it touches; the user is editing
continuously, so the cache would spend its life rebuilding or lying. It also needs a capture pass —
rendering the scene from probe positions — which is a second technique producing lighting for
pixels the main pass also produces, and it needs authoring/placement UX that does not exist. Note
what the one shipped SH *is*: `env_ibl.rs::project_sh9` produces **27 floats from an HDRI image**,
rebuilt when the HDRI loads. That is cheap precisely because it is not a scene capture. A spatial
probe cache is a different animal and does not inherit its economics.

**Why not SDF cone-traced GI (roadmap item 5) — DEAD END, owner-verified 2026-08-06. Do not
re-derive this.**

> **Owner, 2026-08-06:** *"I try all the SDF advance stuff, this do not work because we have DC
> mesh so information of SDF are not precise to the mesh."*

This is empirical, not theoretical — it has been attempted. The mechanism: **what the viewport
displays is the dual-contoured mesh, not the field's zero isosurface.** DC extracts a vertex per
occupied cell by minimizing a QEF against the sampled Hermite data, so the meshed surface deviates
from the analytic isosurface by up to roughly a cell diagonal, and sharp features are approximated
rather than reproduced. Every SDF-field-based shading technique therefore evaluates a surface that
is **not the surface being rasterized**: rays self-intersect or pass through, contacts land at the
wrong depth, occlusion is offset from the geometry the user sees. Increasing bake resolution
narrows the gap but never closes it, and it is not a tuning problem — the two surfaces are
different objects.

A second, independent disqualifier stands even where the field *is* accurate: the baked field
describes **only analytic SDF prims**. The engine already refuses to use it for this reason —
`deferred_entry.wgsl` declines to port the `SDF_REFLECT` sphere-trace probe to the Lit branch:

> *"Deliberately NOT ported: the SDF_REFLECT sphere-trace probe. It marches `scene_sdf`, which
> describes only analytic SDF prims — a poly body is not in that field."*
> — `core_engine/graphics/src/deferred_entry.wgsl:696-698`

**Consequence — do not propose, prototype, or "improve" any of these:** SDF cone-traced GI, SDF
distance-field soft shadows for display, SDF-field AO, or any new consumer of `scene_sdf` /
`scene_analytical` for shading raster pixels. `RENDERER_ROADMAP.md:36-40` still lists "SDF-traced
GI" as "The AZRA power move"; **that item is superseded by this finding** and should be struck when
the roadmap is next edited.

**Existing suspect, flagged not fixed:** the surviving `SDF_REFLECT` probe
(`deferred_entry.wgsl:1041-1086`, gated to `metal > 0.5 && rough < 0.25 && quality >= 2`) is an
instance of exactly this class — it sphere-traces `scene_sdf` to shade pixels rasterized from the
DC mesh. It is **out of scope here**; record it in `COMPLETE_BUG_TRACKER.md` (Gate 0b) rather than
touching it.

**Why this makes SSGI the right choice, not merely the surviving one.** A screen-space pass reads
the G-buffer, and the G-buffer is written by the raster pass **from the DC mesh itself**. The
geometry SSGI bounces light off is, by construction, the exact geometry on screen — there is no
second representation to disagree with. The class of error the owner hit cannot occur here.

**What SSGI is honestly worth — state this up front, do not oversell it.** It bounces light only
off surfaces that are **on screen and in front**. Off-screen geometry, surfaces facing away, and
anything hidden behind another surface contribute nothing. Contribution fades at screen edges.
Thin geometry produces false hits without a thickness test. These are not bugs to be fixed later;
they are the technique. The design consequence is §4.2: **where SSGI is not confident it must fall
back exactly to today's shading**, so the failure mode is "looks like it does now", never a dark
band, never a hard edge at the screen border.

**Forward compatibility.** `RENDERER_ROADMAP.md:22-25` puts TAA next, which brings motion vectors
and a history buffer. This design deliberately needs neither, so it does not block on TAA and does
not have to be retrofitted around it. When TAA lands, temporal accumulation makes the same pass
quieter for free (more effective rays per pixel). Do not pre-build for that here.

---

## 1. Standing rules (every gate)

Distilled from `CLAUDE.md`, `FEATURE_RULES.md`, and `RENDERER_ROADMAP.md:42-46` ("Discipline").
Binding.

**R1 — STOP on rule conflict.** Before starting, check this document against every `CLAUDE.md`
rule. If it contradicts one, STOP and report: which rule, which part of this document, and which
you believe is wrong. Never silently override a rule, even one written by the owner. (There is a
known conflict waiting for you in Gate 6 — §5 Gate 6. Do not improvise around it.)

**R2 — No bandaids, production-DCC bar.** Every fix passes the 5-question test (can this class of
bug recur? does it hide a problem? does the next dev need tribal knowledge? does it scale? does it
compose?). Never downgrade an `assert`, never add a timing/frame-skip hack, never special-case one
caller, never clamp to hide a NaN, never `catch_unwind` internal logic. If you cannot name the root
cause, write a diagnostic, not a fix.

**R3 — Default OFF, look-neutral.** SSGI ships disabled (`ssgi_enabled: false`, the `ssr_enabled`
precedent). With it off, the frame must be **byte-identical** to today. Nothing changes look
silently. This is testable and it is tested in Gate 1.

**R4 — Readback-proven, not reasoning-proven.** The shadow-saga / SSAO standard: render a real
G-buffer headlessly, run the real pass, read the numbers back, assert on them. The template is
`core_engine/render/src/ssao.rs:334-1012` (`try_device`, `cube_and_floor`, `TestCamera::project_px`,
`ao_at`, the determinism assert at `:822-825`). Copy that shape. "I looked at it and it seemed
brighter" is not a gate.

**R5 — Gate on the property, not on "it changed."** (`AZRA_MASTER_ROADMAP.md:641-656` rule 2 —
three features have shipped whose gate passed *because* the output was broken.) Every gate below
names a physical property with a number.

**R6 — Publish a number per step.** A gate without its measured value is not done. State the scene,
the resolution, and warm-vs-cold. Do not report a speedup measured against a cold first frame.

**R7 — Sabotage every gate.** After a gate passes, force the wrong path (zero the delta, disable
the fallback subtraction, feed a black HDR) and confirm the gate **fails**. Then restore. A gate
that cannot fail is not measuring anything.

**R8 — Runtime-only settings.** New render settings go on `app_types::RenderSettings`
(`core_engine/app_types/src/types_render.rs`), which is **not** serialized — no `Serialize` derive,
never written to `.fl4`. Do not add anything to `project::RenderSettings`
(`app/src/project/mod.rs:434`), do not touch the bincode blob, do not add a trailing magic section.
This is the save-format red line.

**R9 — Build after every change**, even five lines: `cd core_engine && cargo build -p app`. Fix
breaks immediately; never pile changes on a broken tree. `cargo test -p graphics` after any shader
change (Naga parse/validate). `cargo test -p render` after any change to the render crate.

**R10 — One WGSL edit per file per batch.** Shader edits invalidate pipeline cache entries and cost
25–43 s of recompile. Batch all changes to one shader into one edit.

**R11 — G-Buffer Contract untouched.** This feature adds **no** G-buffer slot and changes **no**
channel packing. If you find yourself wanting one, stop and report — that is a different plan with
a lockstep obligation across `graphics::gbuffer_format`, the raymarch-lite writer, the sculpt
raymarch writer and the deferred reader.

**R12 — Stage files explicitly. Never `git add -A`.** Other sessions are live in this repo (§7).

**R13 — Every gate ships its dev toggle, in the same commit as the gate.** §4.10 maps each toggle
to the gate that needs it. **Each gate's Acceptance implicitly includes one more item: its toggle
exists, is wired end to end, defaults to production behaviour, and you demonstrated it doing the
thing it is for.** A gate whose toggle is "to be added later" is not done — deferred debug
instrumentation never gets written, and the one moment it is worth having is the moment something
looks wrong. The toggles are also the feature's off-switch and its removal seam (§4.10), so they
are not a nice-to-have layered on top; they are how this feature stays reversible.

**R14 — A visual gate is a hard stop.** The `🔍 VG-x` blocks in §5 are owner approvals, not
checklists you tick. Produce the captures exactly as §5.0 specifies, hand them over, and **stop**.
Do not begin the next gate until the owner writes APPROVE. On REJECT, their note becomes the next
task. You cannot see the application; a green numeric gate is not evidence that the viewport looks
right, and for this feature the most likely failure is "technically correct, looks wrong."

---

## 2. FEATURE_RULES — the 4 questions, pre-answered for this feature

1. **Does it add a new scene object type?** **No.** No `NodePayload` variant, no `NodeId`, no
   selection surface, no graph mutation.
2. **Does it mutate scene state?** **No.** View-only. Every setting sets `DirtyFlags::RENDER` and
   nothing else — no `transaction()`, no undo entry. This is correct and consistent: render
   settings are exempt from the "every slider must have undo" rule, which is scoped to SDF
   properties via `PropsContext`. `UndoPayload` has no render-settings variant and must not gain
   one.
3. **Does it add frame-thread work?** One GPU compute dispatch plus one composite. **No CPU poll
   loop, no async drain**, therefore no `MAX_*_PER_FRAME` cap is required. GPU texture creation
   goes in the viewport resize block co-located with the G-buffer it reads — the `SsaoResources`
   precedent (`viewport_render.rs:774`), **not** a panel callback and **not** a lazy path.
4. **Does it add a render path or mode?** **YES — this is the binding one.** It must respect
   `InteractionEpisode`: zero pipeline switches and zero resolution switches during interaction.
   The policy decision is fixed in §4.5 and is not the agent's to change: **one pipeline, one
   resolution, always** — quality is never traded against camera motion, because indirect light
   appearing and disappearing as the user orbits is a lighting pop, which is the exact class of
   defect `InteractionRenderPolicy` exists to prevent.

---

## 3. Verified fact map — do not re-derive any of this

Established by direct code reading on 2026-08-06 against the working tree (which has uncommitted
edits — see §7). Line numbers are from that tree; re-confirm a line before editing it, but do not
re-investigate the *facts*.

### 3.1 The G-buffer (7 slots, all full viewport resolution)

Constants: `core_engine/graphics/src/lib.rs:150-171` (`gbuffer_format`), channel table `:126-149`.

| Slot | Read binding | Format | Contents |
|---|---|---|---|
| normal_depth | **12** | Rgba16Float | `xyz` = **world** normal (raw, no encoding), `w` = linear distance |
| material | **13** | Rgba16Float | `x` rough, `y` metallic, `z` shadow×AO, `w` opacity+em_lum |
| albedo_flags | **14** | Rgba16Float | `rgb` = linear albedo, `a` = **flags** (pixel class) |
| surface_id | 31 | R32Uint | prim UID — **never cleared, stale over raster pixels** |
| "motion" | 33 | R32Float | misnamed: carries `body_index`, **not** a velocity |
| curvature | 35 | Rgba16Float | **stale over raster pixels** |
| emission | 37 | Rgba16Float | **stale over raster pixels** |

SSGI needs **12, 14** and the lit HDR. It must not read 33, 35 or 37 (§6 T3).

### 3.2 Depth and normals — the two traps

- The de-facto screen-space depth is **`gbuf_normal.w`**, a *linear world-space distance from the
  camera position* for raster pixels (`render/src/gbuffer.wgsl:625`) but the *ray parameter t* for
  raymarch-lite pixels (`raymarch_lite.wgsl:244`). **`0.0` is the background sentinel** — every
  consumer tests `depth > 0.0`.
- The **hardware depth buffer** (`Depth32Float`) exists and *is* sampleable from compute (the HZB
  seed pass does it, `graphics/src/hzb_build.wgsl:30`), but is bound to **no** post-process layout.
  A **Hi-Z max-pyramid** also exists (`render/src/hzb.rs`) — currently consumed only by Phase-D
  instance culling. Do not pull either into SSGI in the first version; note them as later
  acceleration options.
- Normals are **flipped toward the camera** for two-sided surfaces
  (`render/src/gbuffer.wgsl:631-633`). A hemisphere built on that normal is camera-facing, which is
  what screen-space gathering wants — but do not also flip it yourself.
- **Orthographic disagreement:** the raster writes true Euclidean distance even in ortho, while
  `raymarch_lite` and the SSR metric treat ortho depth as forward-z. Pick one convention (§4.4) and
  state it.

### 3.3 Frame order (the compute/post encoder, `app/src/viewport_render.rs`)

Raster + shadows + HZB run in their own encoder and **submit first**. Then, inside
`if render_mode != RenderMode::PathTrace {` (`:2371`):

RT shadows `:2429` → temporal history copy `:2481` → **SSAO** `:2532` → **deferred** `:2566` →
bevel preview `:2578` → **SSR** `:2587` → **fog** `:2614` → *(test blend slot `:2639`)* → block
closes `:2684` → prim outline + its blend `:2690-2783` → bloom `:2790` → **tonemap** `:2797` →
FXAA `:2829` → overlay lines/faces → submit `:2953`.

**`skip_deferred_pp` does not exist** — it was removed 2026-07-16; only a comment remains at
`:2785`. The CLAUDE.md sentence describing blend as running "inside the `!skip_deferred_pp` guard"
describes a flag that is gone; the live guard is the plain mode check at `:2371`.

### 3.4 Where the lit result lives

`gpu.hdr_texture` — `Rgba16Float`, full viewport, usage
`TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC | COPY_DST`, created **only** at
`viewport_render.rs:693` inside the resize block guarded at `:689`, stored `:1084`. Written by
deferred (binding 0 of `deferred_bg`), read by SSR (binding 5), bloom, tonemap.

**SSR is the pattern to copy:** it runs *after* deferred, reads the fully-lit HDR, writes an
**energy-conserving delta** into its own texture, and that delta is added in `pass_tonemap`
(`post_process.wgsl:1224-1229`). It subtracts the IBL term it replaces so it cannot double-count —
`post_process.wgsl:441-444`. SSGI is the same shape with a cosine-hemisphere distribution instead
of a mirror direction.

### 3.5 Ambient today — what SSGI is displacing

Three terms, none of them GI (`deferred_entry.wgsl`):
1. **Flat scalar ambient** — `scene_ambient` from the key light's Ambient slider, `:924`, applied
   `:1010` as `scene_ambient * final_ao * ssao_f`, and **scaled by a hardcoded `0.12` when an HDRI
   is loaded** (`:1011`).
2. **A hardcoded procedural fill light** — `:827`, `:1005`.
3. **Split-sum IBL** when an HDRI is loaded — `env_sh_irradiance(bent_normal)` for diffuse
   (`:1026-1033`) and prefiltered mips + BRDF LUT for specular (`:1038-1094`). **This is the only
   physically-derived indirect in the product.**

With no HDRI there is **no sky/environment ambient at all** — just (1) and (2).

**The precedent trap that must not repeat:** `docs/planning/render/SSAO_VISIBILITY_FINDING.md`
proved SSAO is correctly wired end-to-end yet moves a typical pixel only **~6 %**, because it
multiplies `ambient` only and ambient is cut to 12 % under an HDRI. A GI feature that lands in the
same place would be equally invisible. §4.2 avoids it by compositing *radiance*, not by multiplying
`ambient` — and Gate 2 measures the actual pixel delta rather than asserting the wiring exists.

### 3.6 Existing reusable machinery

| Need | Existing | Location |
|---|---|---|
| Screen-space DDA march, thickness, backface reject, refinement, confidence | `pass_ssr` | `graphics/src/post_process.wgsl:258-445` |
| Short world-space march projected per step | `contact_shadow` | `deferred_entry.wgsl:14-51` |
| Camera ray + world→pixel projection (ortho-aware) | `ssao_camera_ray`, `ssao_project` | `render/src/ssao.wgsl:74-117` |
| Cosine hemisphere sampling | `cosine_hemisphere` | `sdf_material.wgsl:61-75` — **but it is inside `#region RNG_TRACE`, which both deferred builds strip** |
| Golden-angle world-space hemisphere taps + tangent frame | `ssao_main` | `ssao.wgsl:140-162` |
| Deterministic per-pixel noise | `ign` (interleaved gradient) | `ssao.wgsl:66-68` |
| Half-res resource pair + params buffer + lazy bind groups | `SsaoResources` | `render/src/ssao.rs:166-260` |
| Depth-aware bilateral upsample of a half-res signal | `ssao_factor` — **copy this contract** | `deferred_entry.wgsl:156-184` |
| Normal-gated upsample weight `pow(ndot, 8)` — the one thing `ssao_factor` lacks | `pt_upscale` — **copy this term only, not the shader** | `graphics/src/pt_upscale.wgsl:118` |
| Additive composite of two rendered results | `dispatch_blend` — **evaluated and rejected**, §4.7 | `app/src/blend_dispatch.rs:127,155` |
| Headless GPU readback test harness | `ssr_gpu_test.rs` (same crate, same class) | `graphics/src/ssr_gpu_test.rs:341-405` |
| CPU projection replica for addressing probe pixels | `TestCamera::project_px`, `try_device` | `render/src/ssao.rs:363-386, 437-448` |

**Blue noise does not exist** in this repo (zero matches), nor Halton/Sobol. Nor does TAA. Both
existing screen-space passes therefore use **deterministic, non-jittered** per-pixel noise on
purpose — jitter without a temporal resolve boils. SSGI must do the same (§4.3).

### 3.7 Sites that must know about a new post-process pass

The known "6+ BGL sites or it fails silently" trap. Failure mode is *not* a validation error: the
pipeline is invalid, the dispatch site is an `if let Some(..)`, and **the pass silently never
runs**.

- Bind group layout → `ssgi_bgl_entries()` in `graphics/src/bgl.rs`, the single-source pattern. Do
  **not** hand-copy entries the way `fog` (4 copies) and `fxaa` (2 copies) do — that is the trap
  that makes a pass silently never dispatch. (SSAO keeps its BGL private in `render`; SSGI cannot
  follow it — see §4.9, the crate is forced.)
- Pipeline field on `PipelineRegistry` (`app/src/gpu_ctx.rs:248-357`), a `PipelineReady` variant at
  the **end** of the enum (`gpu_ctx.rs:243`), a background compile arm
  (`app_update.rs:1635-1641` is the cleanest exemplar — SSAO), an install arm with the
  `is_none()` race guard (`app_poll_pipeline.rs:183-193`), an `ensure_*` lazy fallback
  (`gpu_init/pipelines_lazy.rs:567-573`) plus its re-export in `gpu_init/mod.rs`, and the warmup
  counters (`gpu_init/pipelines_bg.rs:793,811,827` and the duplicate list at `app_update.rs:980`).
- Resource creation + bind group in the **one** resize block, `viewport_render.rs:689-880`.
- Dispatch site inside the non-PathTrace block.
- Settings field + default (`app_types/src/types_render.rs`) and UI (§5 Gate 6).

---

## 4. Architecture — decided. Not the executing agent's to change.

Every choice below was verified against the code on 2026-08-06. Where a choice looks arbitrary, the
alternative was checked and rejected for the stated reason. If you believe one is wrong, STOP and
report (R1) — do not substitute.

### 4.1 Shape and placement

**One half-resolution compute pass plus two half-resolution blur passes, dispatched immediately
after SSR, inside the non-PathTrace block.** It reads the deferred-lit HDR and the G-buffer, and
writes a **radiance delta** to its own half-res texture. The delta is upsampled and added inside
`pass_tonemap`, beside the SSR add.

```
… deferred (:2566) → bevel (:2578) → SSR (:2587) → [ SSGI gather + blur_h + blur_v ] → fog (:2614) …
                                                                                   … → tonemap: hdr += ssr; hdr += ssgi
```

**Premise correction — "after fog" is not a schedulable slot.** The fog pass never writes the HDR
texture; it writes `fog_texture` (`fog_entry.wgsl:8`, binding 17) and is composited inside
`pass_tonemap` (`post_process.wgsl:1246-1249`). At every point in the frame the HDR is fog-free.
SSGI therefore runs *before* fog exactly as SSR does, which is also physically right: indirect light
should be fogged and bloomed with the rest of the scene, not pasted over it
(`post_process.wgsl:1222-1223` states this intent for SSR).

**What the HDR contains when SSGI reads it:** deferred direct + IBL. It does **not** yet contain
SSR (added at tonemap) and does not contain fog. So SSGI gathers one bounce off directly-lit
surfaces — correct, and no feedback loop is possible.

### 4.2 The energy formulation — the load-bearing part

SSGI must not add light on top of the indirect light deferred already applied. Use SSR's
energy-conserving delta pattern (`post_process.wgsl:187-202, 441-444`), applied **per ray**.

Draw `N` cosine-weighted directions `dᵢ` about the shading normal. With pdf = cosθ/π and a
Lambertian BRDF albedo/π, the estimator collapses to a plain mean:

```
outgoing_diffuse ≈ albedo · (1/N) Σ Lᵢ
```

For each ray, march (§4.4). On a hit, `L_hit` = the HDR radiance at the hit pixel. On a miss, the
ray escapes to the environment — and that is **exactly what deferred already applied** through
`env_sh_irradiance`. So subtract per ray and the misses cancel identically.

**This is the normative code. Type it as written.** An earlier draft of this section was wrong in
six independent ways; every correction below is load-bearing and carries the review finding that
produced it.

```wgsl
// ── receiver gate: view mode AND flag band AND raster class ──────────────────
// c = 2h : the full-res pixel this half-res texel represents (SSAO convention, §4.6)
if (!(config.view_mode == 0u || config.view_mode == 6u)) { store zero; return; }   // E9
let f     = textureLoad(gbuf_albedo_in,  c, 0).a;
let depth = textureLoad(gbuf_normal_in,  c, 0).w;
let is_geo = depth > 0.0 && ((f > 0.76 && f < 0.83) || f > 0.93);
let raster_snap = textureLoad(raster_depth_in, c, 0).w;      // binding 29 — REQUIRED, §4.9
let is_raster   = raster_snap > 0.0 && abs(raster_snap - depth) < 0.05;
if (!is_geo || !is_raster) { textureStore(dst, h, vec4(0.0, 0.0, 0.0, depth)); return; }

// ── the terms deferred actually used ────────────────────────────────────────
let n       = textureLoad(gbuf_normal_in, c, 0).xyz;
let n_s     = ao_bent_normal(c, depth, n);                   // E4: deferred uses the BENT normal
let albedo  = textureLoad(gbuf_albedo_in,   c, 0).rgb;
let metal   = textureLoad(gbuf_material_in, c, 0).y;         // E13: .y is metallic, .x is roughness
let op      = min(textureLoad(gbuf_material_in, c, 0).w, 1.0); // E7: .w packs opacity+emissive
let ao_d    = 1.0;                                           // E5: is_raster ⇒ deferred forced final_ao = 1
let ssao_f  = ssao_factor(c, depth);
let mb      = multi_bounce_ao(ssao_f, albedo);
let has_env = config.env_width > 0u;                         // E6

// ── N cosine-weighted directions about n_s, rotated by ign(h). Never by frame index (§4.3) ──
var L_sum = vec3(0.0);
var S_sum = vec3(0.0);
for (var i = 0u; i < N; i++) {
    let d = cosine_dir(n_s, i, ign(h));
    if (march_hit(...)) {                                    // §4.4
        L_sum += L_hit;
        if (has_env) { S_sum += env_radiance(d); }           // E2: RAW radiance, see below
    }
}

// Never remove more energy than deferred added. Structural, not a tolerance — env_sh_irradiance
// reconstructs only l<=2, while a raw-radiance estimator carries every band, so a small bright
// sun in the HDRI makes individual samples legitimately exceed the applied term.
// NOTE the rejected-pixel store above writes vec4(0.0) — guide depth ZERO, not the
// real depth. Both consumers treat `a > 1e-4` as "this tap carries radiance", so a
// rejected pixel holding a valid depth reads as a legitimate ZERO sample and the
// blur + upsample average it into neighbouring receivers: a dark seam ~8 px wide
// along every geometry/floor-grid boundary. `ssao.wgsl:132` writes 0 for exactly
// this reason. (Found by the Gate 1 review; the first draft of this section had it
// wrong, and the constant-delta Gate 1 test cannot see it.)
let applied = ao_d * mb * env_sh_irradiance(n_s);
let S       = min(S_sum / f32(N), applied);
let delta   = op * ssgi_intensity * albedo * (1.0 - metal)
            * (L_sum / f32(N) - config.ssgi_env_occlusion * ao_d * mb * S);   // user control, below
textureStore(dst, h, vec4(delta, depth));                    // a = in-band guide depth (§4.6)
```

**`ssgi_env_occlusion` — "HDRI Occlusion", a user-facing advanced HDR setting (owner-requested
2026-08-06).** It scales how much environment light SSGI is allowed to **remove** in directions it
proved are blocked. This is the term that makes the delta *signed*: measured at Gate 0, a floor
probe beside a red wall under a blue-ish sky gives `+0.163, −0.252, −0.447` — red gained, green and
blue removed, because the wall occludes sky and replaces it with red bounce.

- **1.0 (default)** — energy-consistent. The sky the geometry occludes is subtracted, so SSGI
  replaces rather than adds. This is the physically defensible setting and what ships.
- **0.0** — additive only. Bounce light is added and the environment is never removed: softer,
  brighter, lower contrast. Knowingly non-physical, and legitimate as an artistic control — the
  precedent is option 2 in `SSAO_VISIBILITY_FINDING.md`, where the owner is already asked to choose
  between physical correctness and a control that visibly does something.

At `0.0` the delta must land on **exactly** the numbers the no-HDRI path produces, since that path
has `L_env = 0` for the same reason. That equality is what justifies a single scalar instead of a
second code path, so it is asserted rather than assumed —
`ssgi_gate0_hdri_occlusion_zero_matches_no_hdri` checks it to 1e-5 in both projections, and also
that raising the control can only ever remove light, never add it, and that at 1.0 the subtraction
is not inert.

It belongs with the environment controls, not buried under SSGI, because it does nothing without an
HDRI loaded (`env_width == 0` ⇒ `S = 0` ⇒ the scalar multiplies zero).

**`env_radiance(d)` must be RAW radiance, not a prefiltered mip.** `sample_env_at_level` mips 1..6
are GGX-prefiltered at roughness = mip/6 (`sdf_lighting.wgsl:210-211`), so mip 6 is already a
near-hemispherical convolution at 1/64 resolution. Cosine-averaging that double-convolves it into
a direction-independent constant — which is precisely the "GI that looks like flat ambient"
No-Hacks violation §4.9 rejects for the fallback. Use `sample_env_raw(d) * config.env_brightness`
(`sdf_lighting.wgsl:276`); `env_sh_irradiance` folds brightness in itself, `sample_env_*` does not.
Do **not** substitute `sample_env` — it additionally applies contrast/saturation, which
`env_sh_irradiance` does not.

**Units are verified, there is no hidden π.** `env_ibl.rs:308-312` scales the SH bands by
`Â_l/π` (`A0=1, A1=2/3, A2=1/4`), so `env_sh_irradiance(n) = (1/π)∫L cosθ dω` = the cosine-weighted
average radiance, and deferred applies it with no `1/π` (`deferred_entry.wgsl:1033`). The
Monte-Carlo estimator matches: `(albedo/π)·L·cosθ / (cosθ/π) = albedo·L`.

**What this buys — and what it does not:**

1. **Zero hits ⇒ delta ≡ 0 exactly.** True, but it proves nothing on its own: it is equally true of
   any `Σ_over_hits(f)` for any wrong `f`. **Gate 3 therefore does not test it** — see §5 Gate 3,
   which was rebuilt around a CPU oracle for this reason.
2. **No double-count on hit directions**, because each hit's environment contribution is removed at
   the weight deferred applied it.
3. **It bypasses the 6 % ceiling** (§3.5 / T1) without touching the `0.12` ambient scale, because
   the delta is composited as radiance into the HDR rather than multiplied into `ambient`.

**Deliberate residuals — state these in the shader header, do not "fix" them:**

- **Misses stay AO-darkened.** Deferred applied `ao·mb·E_sh` over the whole hemisphere; the delta
  only corrects hit directions, so sky that SSGI proved is visible keeps its AO attenuation. The
  residual is `−albedo(1−metal)(1−ao·mb)·(1/N)Σ_miss L_env`. Adding a miss term would destroy the
  zero-hit identity, which is worth more. This is a conservative choice, not "no double-count".
- **With no HDRI, `L_env ≡ 0`, so the delta is purely additive.** Deferred still applied a flat
  ambient at *full* strength in that case (no `0.12` cut) plus the hardcoded procedural fill, and
  neither is occlusion-aware. So without an HDRI, SSGI genuinely does brighten. That is accepted
  for v1; §8 forbids touching the ambient scale to compensate.
- **`mb` is exact only at the top-left pixel of each 2×2 quad**, since deferred evaluates
  `ssao_factor` per full-res pixel and SSGI works at half res.
- **HDR is radiance toward the camera**, reused as radiance toward the shading point: specular
  highlights bleed as diffuse. One bounce only. Nothing off-screen contributes.
- **Receivers are raster pixels only in v1.** Sculpt-atlas raymarch pixels are geometry-flagged but
  not raster, and deferred gives them a real `final_ao` rather than 1.0; including them would
  over-subtract by `(1−final_ao)·mb·L_env`. Widening the receiver set means binding 29 *and*
  reproducing `final_ao`, which is a separate task.

### 4.3 Sampling — deterministic, no temporal anything

There is no TAA, no motion vectors, and **no blue noise anywhere in this repo** (§3.6). Both shipped
screen-space passes are deliberately frame-stable: `ssr_hash` (`post_process.wgsl:213-218`, *"no
temporal jitter — SSR output must be deterministic frame-to-frame"*) and `ign`
(`ssao.wgsl:64-68`, *"Stable across frames by construction"*).

**SSGI must be deterministic per pixel.** Rotate the cosine-hemisphere tap set by `ign(pixel)`;
never by frame index. Denoise spatially (§4.6). A frame-jittered sequence without a temporal
resolve reads as boiling and there is nothing in the engine to resolve it against.

Reuse the golden-angle cosine tap construction at `ssao.wgsl:140-162` — it is already a cosine
hemisphere in world space with a tangent frame. Note `cosine_hemisphere` in `sdf_material.wgsl:61`
is **not** reachable: it lives in `#region RNG_TRACE`, which both deferred builds strip (T8).

### 4.4 Depth convention — and a shipped bug you must not inherit silently

Reconstruct world position as **`origin + dir · depth` from `camera_ray()`** — the ray-parameter
convention (call it **A**). This matches `pass_ssr` (`post_process.wgsl:277-284`), SSAO
(`ssao.wgsl:136-137`), and the deferred pass itself (`deferred_entry.wgsl:537, 766`). Compare
against sampled depth with `ssr_metric` (`post_process.wgsl:242-246`). Reuse `ssr_project`
(`:222-237`) and `ssr_point_at` (`:251-255`) verbatim — the latter is perspective-correct segment
interpolation that exists nowhere else and must not be re-derived.

**The defect this plan found — ✅ FIXED 2026-08-06, before Gate 0.** The raster G-buffer wrote
Euclidean camera distance with **no orthographic branch**, while every consumer reconstructs a ray
parameter. Perspective was identical either way, so it survived unnoticed; in orthographic the two
diverge by `‖rel‖ − dot(rel, forward)` — several world units at the screen edge — which meant
**SSR reflected nothing in ortho views** (its `delta > 0.0` hit test could never fire), SSAO
compared a Euclidean distance against an ortho depth, and deferred shading positions were wrong for
every raster pixel in ortho.

The producer now branches in `gbuf_depth()` (`render/src/gbuffer.wgsl`), `ssao.wgsl`'s occluder
metric matches, and the convention is stated once in the `gbuffer_format` contract block
(`graphics/src/lib.rs`). Regression test:
`gbuffer_depth_is_ray_parameter_in_both_projections` (`render/src/ssao.rs`), sabotage-verified.
Full write-up in `COMPLETE_BUG_TRACKER.md` (2026-08-06).

**What this means for SSGI: convention A is now simply correct in both projections.** Reconstruct
with `camera_ray()`, compare with `ssr_metric()`, and ortho works. Do not invent a third convention.

**Still open, and SSGI must not trip over it:** `pt_reproject.wgsl`, `pt_restir_reproject.wgsl` and
`rt_shadow_temporal.wgsl` still disable themselves in ortho, because their `world_to_prev_screen` /
`world_to_prev_lighting` helpers remain perspective-only (they divide by forward-z with `fov_scale`,
no ortho branch). That is a *projection* gap, not a depth gap. SSGI has no temporal component, so it
is unaffected — but do not "fix" those guards as part of this work.

Terminology: `cam_pos.w` is the ortho **half-height** (`render/src/camera.rs:83`), despite being
named `ortho_hw`/documented as half-width in four shaders. The code is right (`ortho_hw * aspect`
for x); only the comments lie. Do not "fix" it by inserting an aspect factor.

Terminology: `cam_pos.w` is the ortho **half-height** (`render/src/camera.rs:83`), despite being
named `ortho_hw`/documented as half-width in four shaders. The code is right (`ortho_hw * aspect`
for x); only the comments lie. Do not "fix" it by inserting an aspect factor.

### 4.5 Interaction policy — fixed, per FEATURE_RULES Q4

**One pipeline, one resolution, always on when enabled — including during camera drag.** Indirect
light that appears and disappears as the user orbits is a lighting pop, the exact defect
`InteractionRenderPolicy` exists to prevent. This follows SSAO's precedent and its recorded
rationale (`viewport_render.rs:2492-2498`: *"orbit stability is an owner requirement"*), not SSR's
(`ssr_enabled && !interaction_active`).

The cost is therefore paid every frame, which makes §4.6's budget a hard constraint rather than an
aspiration. If the measured cost cannot meet it, **reduce the fixed ray count** — do not add a
dynamic quality ladder keyed on interaction.

### 4.6 Resolution, denoise, upsample

**Half resolution**, `((w+1)/2, (h+1)/2)`, the SSAO convention: half texel `h` maps to full pixel
`2h` (`ssao.wgsl:123-126`), so the gather's ray origin is an exact G-buffer pixel. Do **not** use
`pt_upscale`'s box convention (`(gid+0.5)·0.5−0.5`) — it would shift the GI half a pixel.

Output format `Rgba16Float`, packed **`vec4(delta_rgb, guide_depth)`**. Carrying the guide depth
in-band is what makes the upsample robust; it is the SSAO pattern (`ssao.wgsl:185`) and it removes
`pt_upscale`'s separate failure mode of re-deriving a guide coordinate by integer rounding.

**Denoise at half res, before upsampling** — this is the load-bearing lesson from the PT path,
which denoises at half res (`viewport_render.rs:2112-2122`) and only then upscales (`:2125`). Two
separable bilateral passes, copied from `ssao_blur` (`ssao.wgsl:190-212`): σ_spatial 2.5 texels,
σ_depth tied to the world radius, background texels (`guide ≤ 1e-4`) skipped, ping-pong A→B→A on
the `SsaoResources` shape (`render/src/ssao.rs:166-260`).

**Upsample inline at the composite point — no separate upsample pass and no full-res texture.**
Write `fn ssgi_delta(coord, depth, normal) -> vec3<f32>` in `post_process.wgsl`, modelled exactly on
`ssao_factor` (`deferred_entry.wgsl:156-184`): 4 taps at `vec2<f32>(coord) * 0.5`, weight =
bilinear × `exp(-rel·rel·400.0)` (relative, squared — short-tailed) × `pow(max(ndot,0), 8.0)`
(the normal gate from `pt_upscale.wgsl:118`, which `ssao_factor` lacks and colour bleed needs),
taps with `guide ≤ 1e-4` skipped, and:

```wgsl
if (wsum < 1e-4) { return vec3(0.0); }   // additive identity — never invent light on uncertainty
```

That `vec3(0.0)` is the exact analogue of `ssao_factor`'s `return 1.0`. `pt_upscale` instead falls
back to the nearest sample (`pt_upscale.wgsl:132`) — committing to a value it just decided it could
not trust. Do not copy that.

**Do not reuse `pt_upscale.wgsl` as-is.** It is a destructive full-frame overwrite with no
compositing path, its background branch is an *ungated* bilinear (guaranteed chromatic fringe on
every silhouette for a delta), and its sharpen clamps to `[0, 64]` — which on a signed delta removes
the dark lobe of the unsharp ring and keeps the bright one, a systematic energy gain at every
gradient edge. **No sharpen on the delta.**

**Do not use a 2×2 splat** (the fog / rt_shadow pattern, `fog_entry.wgsl:292-306`). Both splat the
top-left texel with zero depth awareness — a hard 2 px chromatic leak at every silhouette — and
produce piecewise-constant output, which is precisely the "zipper" artifact on the smooth gradients
that are all a GI delta consists of.

**Budget: ≤ 1.5 ms at 1920×1080**, the established ceiling for a screen-space effect here
(`RENDERER_IBL.md:204, 312`). Reference points measured in this repo: GTAO's 3-pass half-res chain
**0.162 ms**, SSAO tier **0.071 ms**, SSR full-res 48 steps **0.18 ms**. Expect a multiple of GTAO,
since each tap fetches HDR colour and marches rather than point-sampling.

### 4.7 Composite — inline add in `pass_tonemap`, and the rule conflict that entails

```wgsl
// post_process.wgsl, immediately after the SSR add at :1224-1229
if (!skip_post && (config.post_flags & 128u) != 0u && is_scene) {
    hdr += ssgi_delta(coord, depth, normal);
    hdr = max(hdr, vec3(0.0));   // the delta is SIGNED by construction — SSR carries this at :1228
}
```

**Bit 128 is the next free `post_flags` bit** (1 ACES, 2 bloom, 4 SSR, 8 fog, 16 dither, 32 DOF,
64 pt_transparent_bg).

**The bit must mirror the dispatch condition exactly, including the PathTrace guard**, the way
SSR's does at `viewport_render.rs:1109`. The tonemap chain runs for **every** render mode
(`viewport_render.rs:2797`) while the SSGI dispatch lives inside the non-PathTrace block — without
the guard, PathTrace composites a stale SSGI texture from the last raster frame:

```rust
| (if self.render.ssgi_enabled && self.render.shading_mode != ShadingMode::PathTrace
     && matches!(self.render.shading_mode, ShadingMode::Pbr | ShadingMode::Lit) { 128u32 } else { 0 })
```

**Stale-composite rule — SSGI needs an `ensure_zero()`.** The `post_flags` bit is assembled ~1480
lines before the dispatch is known to happen, and neither mirrors
`bind_group.is_some() && pipeline.is_some()`. Gate 5 states outright that this pass **will** have
frames where that guard fails (lazy pipeline compile, freshly allocated texture). On those frames
the tonemap would add a half-res texture nothing wrote this frame. SSAO solved exactly this with
`is_white` + `ensure_white()` (`render/src/ssao.rs:178-183`, called at `viewport_render.rs:2534`);
SSGI needs the additive analogue — a one-shot zero fill on the frames it does not dispatch. This is
gated in Gate 1.

**This conflicts with a CLAUDE.md rule and the conflict must be reported (R1), not hidden.** The
Blend Dispatch rule says *"Any **new** feature blending two rendered results uses `dispatch_blend()`
with the `composite_blend` pipeline. Don't inline blending into existing shaders. Existing inline
blending stays."* SSR's `hdr += ssr` is grandfathered; this would not be.

The recommendation is nonetheless the inline add, for four verified reasons — **report them, then
proceed unless the owner says otherwise:**

1. **`dispatch_blend` cannot do the job.** It is strictly 1:1 full-res: bounds come from
   `textureDimensions(tex_base)` and one unscaled `coord` indexes both textures
   (`composite_blend.wgsl:31-36`), there is no sampler in the BGL, and `BlendParams` has no scale,
   rect, or source-dimension field. A half-res delta would land in the top-left quadrant. It would
   need a separate full-res upsample pass and a full-res texture that §4.6 otherwise avoids.
2. **`BlendMode::Additive` is alpha-gated.** `result = base.rgb + over.rgb * over.a`
   (`composite_blend.wgsl:55`), with `over.a = over_raw.a · tint.a · opacity`. A delta written as
   `vec4(rgb, 0.0)` — the natural encoding, and the one §4.6 needs so alpha can carry the guide
   depth — composites to **exactly zero**. It would silently do nothing. (`BlendParams::default()`
   also sets `opacity: 0.5`, which would halve it.)
3. **Cost.** It forces a full-res `hdr → blend_temp` copy plus a full-res blend dispatch every
   frame: ≈ 83 MB of bandwidth at 1080p rgba16f, roughly 0.12–0.4 ms depending on the card, versus
   one extra texture read in a pass that already reads the HDR. Around 4× the traffic for the same
   result, against a 1.5 ms budget.
4. **Correctness for free.** The tonemap site is already gated by `is_scene` (`hdr.a > 0.5`,
   `post_process.wgsl:1206`), which excludes alpha-0 floor/grid/wire passthrough pixels — the exact
   class §4.9 must not touch. And placing it beside the SSR add means GI blooms and fogs with the
   scene, per the documented intent at `post_process.wgsl:1222-1223`.

Do **not** reuse `gpu.overlay_texture` for anything. It is a shared scratch that `prim_outline`
overwrites unconditionally every dispatch (`prim_outline_entry.wgsl:224-225`), and calling
`ensure_overlay_texture` with half-res dimensions would destroy and reallocate both scratch
textures twice per frame.

### 4.8 Receiver classification — flags, never alpha

```wgsl
let f     = textureLoad(gbuf_albedo_in, coord, 0).a;
let depth = textureLoad(gbuf_normal_in, coord, 0).w;
let is_lit_geo = depth > 0.0 && ((f > 0.76 && f < 0.83) || f > 0.93);
```

Accepts the 0.78 geometry band and the 1.0 selected band; rejects background (0.0), floor and
workplane (0.5), ghost (0.61), VFX-unlit (0.68/0.88) and DC wire (0.74). Non-receivers write
`vec4(0, 0, 0, 0)` — neutrality comes from **zero RGB**, never from alpha.

**Never gate on `hdr.a > 0.5` inside the SSGI pass.** Background with an HDRI loaded is written with
alpha 1.0 (`deferred_entry.wgsl:364`), so an `is_scene`-shaped test would inject GI into the sky.
Alpha and pixel class are independent axes: in Wireframe mode fully-lit PBR geometry is stored with
alpha 0.0 (`deferred_entry.wgsl:1143`).

Two residual cases, to be verified and documented rather than engineered around in v1: selection-rim
pixels (`deferred_entry.wgsl:1125-1139`) pass the `f > 0.93` test and will receive a small additive
tint on the rim colour; and DC-wire *dilation* repaints geometry-flagged pixels as wire with alpha 0
(`:396-422`), which the `is_scene` composite gate already excludes.

### 4.9 Where the code lives

**The `graphics` crate, not `render`.** This is a correction to the obvious guess: `extract_region`
is a **private** fn in `graphics/src/shader_build.rs:419`, `SDF_LIGHTING_WGSL` is `pub(crate)`, and
`render` does not depend on `graphics` (the dependency runs the other way). SSGI needs the ENV
region, so it must be built inside `graphics`.

- `graphics/src/ssgi_entry.wgsl` — the pass. Entry `pass_ssgi` plus the two blur entries.
- `graphics/src/shader_build.rs` — `pub fn build_ssgi_wgsl()`, mirroring `build_rt_shadow_wgsl`
  (`:298`) and `build_pathtrace_tri_wgsl` (`:274`): `SDF_HEADER_WGSL` with `{{FLOATS_PER_PRIM}}`
  substituted, then an inline `@group(0) @binding(21) var<storage, read> env_data: array<f32>;`
  (required because `sdf_lighting.wgsl:63` declares it *outside* every region), then
  `extract_region(SDF_LIGHTING_WGSL, "ENV")`, then the entry.
- `graphics/src/bgl.rs` — `pub fn ssgi_bgl_entries()`, the single-source pattern. Modelled on
  `ssr_bgl` (`app_new.rs:82`): `sto(0)` delta out (this **is** the header's `output_texture`),
  `uni(1)` GpuUniforms, `tex(5)` lit HDR, `tex(12) tex(13) tex(14)` G-buffer,
  **`tex(29)` raster_depth — required by §4.2's `is_raster` gate**, `buf_ro(21)` env_data,
  `tex(38)` the AO factor for `ssao_factor`/`mb`.

**Binding 0 is already declared by the header** as
`@group(0) @binding(0) var output_texture: texture_storage_2d<rgba16float, write>`
(`sdf_shared.wgsl:188`) — **reuse it as the delta target; do not declare your own**, or you get a
duplicate-declaration error. The bindings the header declares that you genuinely do not use are
**2/3/4** (`voxel_grid`, `prim_data`, `tex_data`); those are dead-code-eliminated and need no BGL
entries. (`rt_shadow_bgl_entries` at `bgl.rs:185-191` binds 0 and 1 and omits 2/3/4 — that is the
precedent, and it does *not* say binding 0 is unused.)

Use `SDF_HEADER_WGSL` verbatim — **never hand-write a truncated `GpuUniforms`**. `env_sh_irradiance`
reads fields up to `env_rotation` at byte 592, and a subset copy that mis-sizes any earlier field
makes every later field silently read the wrong bytes. That exact bug shipped once already
(`shader_build.rs:997-1001`: the 2026-07-18 SSR copy skipped the LOD/atlas blocks, so `ssr_intensity`
read `foveated_enabled == 0` and SSR output was all-zero).

**`multi_bounce_ao` and `ssao_factor` are NOT reachable by the recipe above — this needs one extra
step.** Both live in `deferred_entry.wgsl` deliberately **outside every `#region`** (`:57-58` and
`:191` say so explicitly, so the `df_lite` strip keeps them), and the only extractable region in
that file is `ENV_BRDF`. Extracting `"ENV"` from `sdf_lighting.wgsl` yields `sample_env_*` and
`env_sh_irradiance` and nothing else, so `build_ssgi_wgsl()` as first drafted produces two
undefined-function errors.

**Fix, in this order:** wrap `deferred_entry.wgsl:149-256` (`ssao_factor`, `oct_decode_bent`,
`ao_bent_normal`, `multi_bounce_ao`, `specular_occlusion`) in a new
`// #region AO_UPSAMPLE` … `// #endregion AO_UPSAMPLE` pair, then
`extract_region(DEFERRED_ENTRY_WGSL, "AO_UPSAMPLE")` in `build_ssgi_wgsl()`. This mirrors exactly
what `process_post_process_wgsl` already does with `ENV_BRDF` (`shader_build.rs:19`). Adding the
region tags is a no-op for every existing build **only if** no `*_EXCLUDE` list names it — do not
add `AO_UPSAMPLE` to any exclude list, and re-run the full `cargo test -p graphics` suite, because
a region-tag slip in `deferred_entry.wgsl` breaks fog and prim-outline, not SSGI. Hand-duplicating
the two functions instead is forbidden — it is the drift this plan's own §4.9 argues against, and
nothing would catch it six months from now.

**Rejected alternative:** passing a CPU-computed fallback irradiance instead of extracting ENV. It
would collapse a 9-coefficient directional function to a constant, so every escaped ray would return
the same colour regardless of direction — "GI that looks like flat ambient", a No-Hacks violation.
It also breaks the delta contract of §4.2, which requires subtracting *exactly* what deferred
applied. Extraction costs two lines of Rust and one BGL entry and cannot drift.

---

### 4.10 Dev toggles — one per gate — and how to remove the feature

**Owner requirement (2026-08-06):** every gate ships a toggle in the Developer Tweaks panel, so each
stage can be isolated while debugging and the whole feature can be switched off or backed out
without archaeology.

This is not decoration. A three-dispatch screen-space effect that composites into the HDR has no
natural seam to probe: when the result looks wrong, "is it the gather, the energy term, the blur, or
the upsample?" is otherwise unanswerable without recompiling. Each toggle below exists to answer
exactly one of those questions, and each is tied to the gate that needs it.

#### The toggles

| Toggle (Developer Tweaks label) | Gate | What it isolates | Default |
|---|---|---|---|
| **Kill SSGI (master)** | all | Hard-disables the dispatch **and** clears the `post_flags` bit, regardless of the user setting. The instant answer to "is SSGI causing this?" | off |
| **Force constant delta** + value slider | 1 | Gather writes a compile-independent constant. Proves the composite path end-to-end with no GI math in play — this is Gate 1's mechanism, kept live afterwards. | off |
| **Show delta only** | 1,2,3 | Tonemap outputs the delta *instead of* adding it. Makes an invisible or blown-out delta immediately visible, and is the fastest read on ray count / noise. | off |
| **Disable env subtraction** | 3 | Drops the `− ao·mb·S` term, leaving the raw gather. Toggling it is a live double-count check: the difference between the two images *is* the term deferred already applied. | off |
| **Skip blur passes** | 4 | Dispatch the gather only. Shows the raw per-pixel noise the denoiser is hiding. | off |
| **Nearest upsample** | 4 | Bypass the depth+normal bilateral gate. Deliberately exposes the leaks and halos the gate suppresses, so you can confirm the gate is the thing suppressing them. | off |
| **Full-res gather (dev)** | 4 | Dispatches at full resolution. This **is** Gate 4's reference render; keeping it as a toggle means the upsample-error number can be re-measured any time. | off |
| **Ray count override** | 2,5 | `0` = use the user setting. Perf/quality sweeps without touching the UI. | 0 |

#### Rules these must obey

1. **Every default is the production value.** A build with every toggle at its default is the
   shipping build. A toggle may only ever make the pass *more* inspectable — never silently change
   what ships. (This is why they are all `false`/`0`, and why "Kill SSGI" is off rather than a
   double negative.)
2. **Runtime-only, never serialized.** Same save-format red line as R8 — these appear in no
   `ProjectFile`, no `.fl4`, no side-channel.
3. **One struct, not eight fields on `AzraApp`.** Declare `SsgiDebug` in
   `app_types/src/types_render.rs` next to `RenderSettings` and hold **one** field on the app. The
   `test_blend_overlay` precedent (a bare `pub(crate) bool` at `main.rs:757`, defaulted at
   `app_new.rs:329`, drawn at `win_tweaks.rs:792`) is the shape to follow for the *panel wiring*,
   but do not add eight more loose fields to `main.rs` — that is the god-file growth
   `GOD_FILES_SPLIT_PLAN.md` exists to stop.
4. **Shader-side toggles ride one `debug_flags: u32` bitfield** in the SSGI params uniform. Bit
   tests are free; do not compile pipeline variants, and do not branch on a toggle in a way that
   changes the pipeline — that would violate FEATURE_RULES Q4 (no pipeline switches).
5. **"Full-res gather" changes a GPU resource size.** It must set a deferred flag consumed in
   `poll_background_tasks`, never reallocate from the panel callback — Frame Ordering Contract. The
   `cache.atlas_rebuild_xy` pattern is the precedent.
6. **Panel placement:** one collapsing section titled `SSGI` in `win_tweaks.rs`, next to the
   existing render smoke tests, each checkbox with an `.on_hover_text` saying what it isolates.
   Follow the `test_blend_overlay` hover style — state the mechanism, not just the name.
7. **The toggle lands in the same commit as its gate.** See R13.

#### Removing the feature

Two levels, both deliberate:

**Switch it off at runtime** — "Kill SSGI (master)" in Developer Tweaks, or `ssgi_enabled = false`
in the render settings (which is already the default). Both leave the pass undispatched and the
`post_flags` bit clear, so the frame is bit-identical to a build without the feature. That identity
is Gate 1.3, so it is tested rather than assumed.

**Back it out of the source** — the feature is deliberately built so this is a delete, not a
disentangling. Revert exactly these, in this order (the inverse of §3.7):

1. The composite: the `hdr += ssgi_delta(...)` block and `fn ssgi_delta` in `post_process.wgsl`, and
   the `tex(N)` entry it needs in `tonemap_bgl` (`app_new.rs:96`) plus that bind group's entry in
   the `viewport_render.rs:689` block.
2. The `post_flags` bit 128 assembly at `viewport_render.rs:1108-1112`.
3. The dispatch block and `ensure_zero()` call in `viewport_render.rs`.
4. `PASS_SSGI` back to `PASS_BAKE` in `gpu_timestamps.rs`, and the two HUD label tables.
5. Resource creation in the `viewport_render.rs:689` block; the `PipelineRegistry` field, the
   `PipelineReady` variant, the background-compile arm, the `poll_sdf_compile` arm, the
   `ensure_ssgi_pipeline` fallback and its `gpu_init/mod.rs` re-export; the warmup counters.
6. `graphics/src/ssgi_entry.wgsl`, `build_ssgi_wgsl()`, `ssgi_bgl_entries()`, and the
   `validate_ssgi_shader` test.
7. The `RenderSettings` fields, the `SsgiDebug` struct, the UI block in `render_settings.rs`, and
   the Developer Tweaks section.
8. **Leave `#region AO_UPSAMPLE` in `deferred_entry.wgsl` alone.** It is inert region tagging that
   changes no build, and removing it risks the fog / prim-outline strip. Note it in the removal
   commit instead.

Everything on that list is additive; nothing in this plan modifies existing shading math. If a
future change to SSGI requires editing `deferred_entry.wgsl`'s ambient or IBL terms, that is a
different feature and this removal list stops being valid — say so at the time.

---

## 5. Gates

Each gate is independently buildable and independently verifiable. Do them in order. Publish the
number named in each Acceptance block (R6) and sabotage each gate before moving on (R7).

**This gate list was rebuilt on 2026-08-06 after an adversarial review found that the previous
version could not fail.** Its two headline defects are worth stating, because the same mistakes are
easy to reintroduce: Gate 3 asserted "delta ≡ 0 in a scene with no hits", which is true of *any*
formula of the form `Σ_over_hits(f)` including a completely wrong `f` — and its sabotage step
(remove a term from an empty sum) provably could not fail. Gate 4 asked that a wall behind a lit red
object gain **no** red, which is the feature working, so a correct implementation failed and a
delta-killing one passed. Both are fixed below. If you find yourself weakening a gate to make it
pass, you have found a bug, not a bad gate.

### 5.0 Two kinds of gate

**Numeric gates** are headless, run in CI, and must be falsifiable — a wrong implementation has to
make them red. They are the primary defence.

**VISUAL GATES (marked `VG-x`) are owner approvals.** They exist because the failure mode this
feature is most likely to hit is not "a number is wrong" but "it is technically correct and looks
bad" — halos, boiling, colour bleeding through walls, a lighting pop when you orbit. No headless
assert catches those, and the executing agent cannot see the application. **A visual gate is not
advisory: work stops at it until the owner writes APPROVE.** If the owner writes REJECT, the note
they give is the next task; do not proceed to the following gate.

**How to produce visual-gate images.** Full-resolution captures of the viewport, one pair per
comparison, identical camera and scene, only the named setting changed between them. Do **not** use
the `AZRA_VISDIFF` rig or the 512-px screenshot thumbnail path for these — that path centre-crops
to square and resizes with a Triangle filter (`viewport_render.rs:440-449`), which destroys exactly
the low-amplitude gradients under review. Name files `VG-<x>_<label>_<off|on>.png`, put them in
`docs/planning/render/ssgi_captures/`, and state the scene, camera, HDRI, and every setting that
differs. Present A/B pairs, never a single image.

---

### Gate 0 — Test harness, CPU oracle, and baseline, before any GI code

**Context.** Gate 0 carries more weight than it looks. The review's verdict on the previous plan was
that its most likely failure was "the agent completes every gate, publishes numbers that all look
right, and ships something wrong or invisible" — because every gate was a zero-hit test. The fix is
a **CPU oracle**: the same synthesized inputs evaluated on the CPU, cross-checked against the GPU.
That single mechanism can fail on a `.x`/`.y` channel swap, a missing `env_width == 0` guard, a
wrong mip level, an SH-vs-radiance normalisation mismatch, the bent-normal divergence and the depth
convention — none of which any other gate can catch. The precedent is in the repo:
`graphics/src/rt_shadow_gpu_test.rs` cross-checks its WGSL against `trace_scene_cpu`.

**Task.** In the `graphics` crate, modelled on `graphics/src/ssr_gpu_test.rs` (same crate, same
class of pass, and its minimal ENV-consuming BGL at `:341-405` is the working example):

1. **Synthesize** the G-buffer and lit HDR by writing textures directly — do not rasterize. The
   gate must test the SSGI kernel, not the raster path. Scene: a white floor plane meeting a
   saturated red vertical wall at a right angle, filling known pixel regions, with a known lit HDR
   (floor white, wall red).
2. Build a **real environment buffer** — the full 7-level mip chain plus the 27 SH9 floats, in the
   layout `env_ibl.rs:483` produces. Gate 3 needs an HDRI and `ssr_gpu_test.rs`'s 16-byte stub is
   not enough; building this here avoids re-opening Gate 0 later.
3. Provide named world-space probes and a CPU projection replica so probes address pixels — reuse
   `TestCamera::project_px` and `try_device` from `render/src/ssao.rs:363-386, 437-448`.
4. Write the **CPU oracle**: a plain-Rust evaluation of §4.2's formula at each probe, reading the
   same synthesized inputs.
5. Parameterise the harness over **four configurations**: {HDRI, no HDRI} × {perspective,
   orthographic}. Ortho is not optional — §4.4 is this plan's longest technical section and the
   previous gate list tested it zero times while filing the identical omission in `ssr_gpu_test.rs`
   as a defect.

**Acceptance.**
1. The harness runs in all four configurations and dumps floor pixel values at three probes:
   adjacent to the wall, one wall-height away, and far from the wall. Paste the twelve numbers.
   This is the baseline.
2. The CPU oracle runs and prints its own value at each probe. With no GPU pass implemented yet the
   two do not agree — that is expected; Gate 3 is where they must.
3. Skips cleanly with a message when no GPU adapter is present.
4. `cargo test -p graphics` green.

### Gate 0b — File the remaining defects found during design (no fixes)

**Task.** Item 1 is **already done** — do not redo it. Add the rest to `COMPLETE_BUG_TRACKER.md`:
1. ~~Orthographic depth-convention divergence~~ — **✅ FIXED 2026-08-06**, before this plan was
   executed. See §4.4 and the tracker entry.
2. **`SDF_REFLECT` shades DC-meshed pixels from the analytic field** (§0) —
   `deferred_entry.wgsl:1041-1086`. Owner-verified class: field is not precise to the mesh.
3. **The three temporal ortho guards are now blocked on projection, not depth** (§4.4) —
   `world_to_prev_screen` / `world_to_prev_lighting` need ortho branches before PT and RT-shadow
   temporal reprojection can run in orthographic views.
4. **`properties_profiler.rs:61-65` is missing `PASS_RT_SHADOW = 15`** — 15 `gpu_names` entries and
   a `0..15` loop, so RT shadow is invisible in the Profiler panel and excluded from its GPU total,
   while `viewport_overlays.rs:100` does list it.

**Acceptance.** Four tracker entries with file:line and repro. No code changed.

### Gate 1 — Neutral plumbing: the pass exists and provably changes nothing

**Context.** Prove the composite is neutral *before* any GI math exists, so that when Gate 2 changes
pixels you know why. Note the dependency fix: the `ssgi_enabled` **settings field** (not the UI
widget) is introduced here, because Gates 1 and 5 both need to toggle it and Gate 6 is too late.

**Task.** Create the shader, `build_ssgi_wgsl()` (including the `AO_UPSAMPLE` region work in §4.9),
the BGL, the pipeline (background compile arm + `is_none()`-guarded install + `ensure_ssgi_pipeline`
lazy fallback + warmup counters — site list in §3.7), the half-res resources in the
`viewport_render.rs:689` resize block, the dispatch **gated on shading mode** the way SSAO is
(`viewport_render.rs:2499-2500`), the `post_flags` bit 128 with its PathTrace guard (§4.7), the
`ensure_zero()` stale-composite guard (§4.7), and the `ssgi_delta()` upsample + add in
`pass_tonemap`. Have the gather write a **compile-time constant** delta, and classify receivers per
§4.2's receiver gate.

**Acceptance.**
1. Constant = `vec3(0)`: in the Gate 0 headless harness, `ssgi_delta()` returns **exactly**
   `vec3(0.0)` at every probe, and the composited HDR is bit-equal to SSGI-off. Assert on the HDR
   readback, not on a screenshot — `AZRA_VISDIFF` cannot A/B a render setting and downsamples to
   512 px with a Triangle filter, so a uniform +0.3 % error would pass it.
2. Constant = `vec3(0.25)`: exactly the receiver pixels change. Background, floor grid, workplane,
   ghost, VFX, DC-wire and non-raster geometry are **bit-unchanged**. Assert per class.
3. `ssgi_enabled: false` (the default) leaves the pass undispatched and the frame bit-identical to
   a build without the feature.
4. **Stale-composite:** force one frame where the bit is set but the pipeline is absent (simulate
   the lazy-compile frame). The composite must add zero, not stale content.
5. `cargo test -p graphics` green, including `validate_ssgi_shader`, which asserts the extractions
   landed — `src.contains("fn env_sh_irradiance(")`, `"fn sample_env_raw("`,
   `"fn multi_bounce_ao("`, `"fn ssao_factor("`, `"var<storage, read> env_data"` — and then
   **parses and validates** with Naga. Parse-only is insufficient: it accepts a call with the wrong
   argument count (`shader_build.rs:554-559`).
6. Sabotage: break the receiver predicate so floor pixels are accepted; confirm (2) fails.

> #### 🔍 VG-A — "it changes nothing" (owner approval)
> **Why:** the whole default-off, look-neutral promise (R3) rests on this, and no headless assert
> can prove the viewport still *looks* right.
> **Setup:** the owner's own working scene, any camera. Capture with `ssgi_enabled` off, then on
> with the constant delta forced to zero.
> **Owner judges:** the two images are indistinguishable — no brightness shift, no tint, no change
> at the floor grid, the background, the selection outline, or on a ghosted object.
> **REJECT if:** anything at all differs.
> **Owner verdict:** ⬜ APPROVE ⬜ REJECT — note: ______________________

### Gate 2 — The gather

**Context.** Reuse, do not re-derive: `ssr_project`, `ssr_metric`, `ssr_point_at`, the DDA setup
with `du = max(1/max_steps, 2.0/screen_len_px)`, the coarse loop with adaptive thickness and
backface rejection, the binary refinement, and the edge fade — all with line ranges in §3.6/§4.4.
Ray generation from `ssao.wgsl:140-162`, **built about the bent normal `n_s`**, not the geometric
normal (§4.2 E4). §6 T10 lists fourteen failure modes these already solve; rediscovering any is a
Gate 2 failure.

**Task.** N cosine-weighted rays per half-res pixel, deterministic `ign` rotation, screen-space
march per ray, `L_hit` = HDR at the hit pixel, misses contribute nothing. Emit
`vec4(delta_rgb, guide_depth)`. Start at N = 8; tune only after Gate 5 measures.

**Acceptance.**
1. **Colour bleed exists and is directional:** floor probe adjacent to the red wall has a red
   channel measurably above its own green/blue, and above the same probe with SSGI off. Publish the
   three probe triples, in all four Gate 0 configurations.
2. **Monotonic falloff:** adjacent > one-wall-height > far. This is the property gate; "the floor
   got redder" alone would also pass with a global tint bug.
3. **Determinism:** two consecutive runs byte-identical (`ssao.rs:822-825` pattern).
4. **No self-illumination:** a single flat lit plane alone on screen produces delta ≈ 0 everywhere
   — proving the origin bias and backface rejection work.
5. Sabotage: remove the backface test; confirm (4) fails.

### Gate 3 — Energy: the CPU-oracle cross-check

**Context.** This replaces the previous Gate 3 entirely. The old one asserted "a lone object in
empty space renders within 1e-3 of SSGI-off" — vacuously true for any formula, with a sabotage step
that removed a term from an empty sum. **A gate that cannot fail is not measuring anything (R7).**
The property that actually matters is that the GPU computes the formula §4.2 specifies, on a scene
**with hits**, in every configuration.

**Task.** Implement §4.2 exactly: bent normal, `.y` metalness, opacity, `env_radiance` = raw
radiance × brightness, the `min(S, applied)` clamp, the `has_env` guard, and the `is_raster`
receiver gate.

**Acceptance.**
1. **GPU ≈ CPU oracle to 1e-3 at every probe, in all four configurations** (HDRI / no-HDRI ×
   perspective / orthographic). Publish the table: 3 probes × 4 configs × {GPU, CPU}.
2. **Zero-hit identity still holds** — lone object, HDRI loaded, within 1e-3 of SSGI-off. Keep it,
   but understand it proves nothing on its own; (1) is the real gate.
3. **No negative HDR reaches tonemap:** assert the composited HDR has no channel < 0 anywhere.
4. **Env toggle:** with an HDRI resident, untick Environment (`env_width` → 0 while `env_buffer`
   still holds the mip chain). The delta must not change by more than the no-HDRI oracle predicts —
   this is the E6 trap, where an unguarded `sample_env_mip` reads real HDRI floats at meaningless
   indices.
5. Sabotage — **four separate runs, each must turn (1) red:** (a) read `.x` instead of `.y` for
   metalness; (b) build the hemisphere about the geometric normal instead of the bent normal;
   (c) use `sample_env_mip(d, 6)` instead of raw radiance; (d) drop the `min(S, applied)` clamp.
   Each of these was a real defect in the first draft of §4.2. If any sabotage leaves the gate
   green, the oracle is not testing what it claims.

> #### 🔍 VG-B — "the light bounces, and it looks like light" (owner approval)
> **Why:** this is the first time the feature does anything. Numbers can confirm red arrives on the
> floor; only the owner can say whether it reads as bounced light or as a stain.
> **Setup:** build a simple interior corner — a white floor, one saturated red wall, one saturated
> blue wall, a neutral object in the middle, a single key light. Capture off/on. Then a second
> pair with the walls changed to a mid grey, to show the effect is colour-driven, not a generic
> darkening or brightening.
> **Owner judges:** coloured light gathers on the floor near each wall and fades with distance;
> the neutral object picks up colour on the sides facing the walls; the grey-wall version shows a
> much weaker, uncoloured effect; nothing glows, nothing is blotchy, no colour appears on surfaces
> that face away from the coloured walls.
> **REJECT if:** the whole frame brightens uniformly (that is a brightness bug wearing GI's coat),
> colour appears on the far side of a wall, or the effect is invisible at default intensity.
> **Owner verdict:** ⬜ APPROVE ⬜ REJECT — note: ______________________

### Gate 4 — Denoise and upsample

**Context.** The previous Gate 4.1 was backwards: it demanded that a wall behind a lit red object
gain **no** red within 2 px of the silhouette. That is the feature. An implementation that zeroes
the delta near any depth discontinuity would have passed — and it produces exactly the dark halo §0
says must never happen. The property actually wanted is **upsample error**, which cannot be
measured against zero. It has to be measured against a **full-resolution reference** of the same
delta.

**Task.** The two half-res bilateral blur passes and the inline `ssgi_delta()` upsample with the
depth **and** normal gate, `vec3(0)` on total rejection, no sharpen (§4.6). Add a test-only path
that dispatches the gather at **full resolution** to produce the reference.

**Acceptance.**
1. **Upsample error vs the full-res reference:** mean and max |Δ| over the frame, and separately
   over a 4-px band along silhouettes. Publish both. The silhouette band is where half-res costs
   you; the number is the budget, not zero.
2. **No leak across a convex corner:** a two-plane corner must not bleed colour across the crease —
   this is what `pow(ndot, 8)` is for. Measure against the full-res reference, not against zero.
3. **Blockiness:** max difference between adjacent full-res pixels in a flat-lit region, before and
   after upsample.
4. Sabotage: remove the normal gate; confirm (2) fails. Remove the `wsum < 1e-4` guard; confirm
   (1) or (3) fails.

> #### 🔍 VG-C — "no halos, no leaks, no boiling" (owner approval)
> **Why:** every artifact class this feature is prone to is a spatial or temporal pattern the eye
> catches instantly and a per-probe number does not.
> **Setup:** three captures — (i) an object with a thin protruding part against a distant wall,
> static, off/on; (ii) the same scene at half and full viewport width, to expose upsample scaling;
> (iii) a **slow orbit**, captured as a short screen recording or 5 sequential stills.
> **Owner judges:** no dark or bright rim tracing any silhouette; no colour bleeding through or
> around thin geometry; no visible 2×2 blockiness or diagonal zipper on smooth gradients; and
> during the orbit **no shimmering, crawling or boiling** in the indirect light.
> **REJECT if:** any halo, any leak past a silhouette, or any temporal instability. Boiling in
> particular is fatal — there is no TAA to hide it and the sampling must be deterministic (§4.3).
> **Owner verdict:** ⬜ APPROVE ⬜ REJECT — note: ______________________

### Gate 5 — Perf, interaction, timestamps, and the paths no gate covered

**Task.** Time the pass and wire it into the profiler.

**Use timestamp slot 13.** `PASS_BAKE = 13` is **dead** — a workspace-wide grep returns only its own
definition, it carries `#[allow(dead_code)]`, and the bake systems own private query sets. Rename it
`PASS_SSGI`. This avoids raising `MAX_QUERIES` and the silent-zero trap. Update **both** HUD label
tables — `viewport_overlays.rs:42` and `:94-101`, and `properties_profiler.rs:61-65` (already
missing slot 15; fix that too, per Gate 0b#4).

**Time the whole pass, not a third of it — and note the constraint found in Gate 1.** §4.1 is three
dispatches (gather + 2 blurs), and one `tw!()` brackets one compute pass. **They cannot be merged
into a single timed pass:** wgpu's usage scope spans the entire pass, and the gather's
storage-write target is the blurs' sampled source, so merging reproduces the exclusive-usage
conflict the Gate 1 test caught. So the HUD number is the **gather alone, a lower bound**. Gate 5
must either publish it while stating plainly that the blurs and the inline upsample are excluded
(and estimate them), or spend two more timestamp slots. Do not compare the gather number to the
full-chain budget and call it passing.

**The device-loss rule, non-negotiable:** place `tw!(PASS_SSGI)` **inside** the
`if let (Some(bg), Some(pl))` that immediately precedes `begin_compute_pass`, as
`viewport_render.rs:2423-2432` does for RT shadow. Marking a slot whose pass then does not dispatch
resolves unwritten queries — `vkCmdCopyQueryPoolResults` with the WAIT bit on a query that was never
reset — which hangs the GPU.

**Acceptance.**
1. Measured GPU ms at 1920×1080 with N = 8, warm, stating the GPU and what is included. **≤ 1.5 ms.**
   Reference: GTAO's 3-pass half-res chain 0.162 ms, SSR 0.18 ms.
2. If over budget, reduce N as a fixed constant and re-measure. **Do not** add an interaction-keyed
   quality ladder (§4.5).
3. The Profiler panel and the `[STALL]` scoreboard both show a non-zero SSGI time.
4. **Resize:** drag the viewport edge continuously for several seconds with SSGI on. No crash, no
   validation error. Odd widths included (`(w+1)/2`).
5. **Split viewport:** `dispatch_gpu_render` runs twice per frame for split panes
   (`app_update.rs:701, :708`) and a third time after a dock repaint (`:902`). Both panes share one
   half-res texture. Confirm no corruption, and state which pane's number the HUD reports.
6. **Small viewport:** shrink a pane to near-minimum. `(1+1)/2 = 1` half-res dimensions must not
   crash or read past the last texel.
7. **Shading modes:** confirm the dispatch is skipped entirely in Solid, MatCap, Wireframe and
   PathTrace — not merely that the composite is skipped. Burning the budget for a discarded result
   is a defect.
8. **Hygiene greps:** `UndoPayload` gained no variant, no `transaction()` was opened, and
   `ssgi_*` appears in **no** file under `app/src/project/` (the save-format red line, R8).
9. Sabotage: **do not** deliberately hang the GPU. Instead, in the headless harness, assert that
   `active_passes` has bit 13 set **only** on frames where the dispatch is encoded — a unit-level
   check of the same invariant, with no TDR risk to the owner's machine.

> #### 🔍 VG-D — "it survives being used" (owner approval)
> **Why:** the interaction policy (§4.5) is a deliberate, contestable choice — SSGI stays on during
> camera motion because a lighting pop is worse than a cost. Only the owner can judge that trade.
> **Setup:** the owner's real working scene. Orbit, pan, zoom, drag a gizmo, enter and leave edit
> mode, toggle SSGI on and off a few times, switch shading modes and back.
> **Owner judges:** the lighting does not pop, flash, or change brightness when motion starts or
> stops; the viewport still feels responsive; toggling the feature is instant and reversible;
> nothing flickers when switching modes.
> **REJECT if:** indirect light appears or disappears with camera motion, framerate is
> unacceptable, or any mode switch flashes.
> **Owner verdict:** ⬜ APPROVE ⬜ REJECT — note: ______________________

### Gate 6 — Settings and UI, and a rule conflict to report

**Context — read all of this before touching a file.** The settings widget must go in
`render_pbr_postfx_body` (`core_engine/app/src/ui2/shell/render_settings.rs:343`), the **single**
source of truth for every render slider; `properties_render.rs:999` and `ui2/shell/shading.rs:453`
both delegate to it, and `properties_render.rs` has no AO/post-FX controls of its own. There is
**no** non-duplicating location outside `ui2/`. That collides with the CLAUDE.md hard ban
*"Never create or edit `ui2/` on master."*

**Task.** Report the conflict per R1, with this evidence, then **proceed on master**:
- `master` and `feat/ui-polish` were the same commit with an empty diff when this was checked.
- The 2026-08-02 amendment (`CLAUDE.md:19-23`) says *"Historical text below predates the
  consolidation"*, and the Hard bans list sits below it, inside that disclaimer's scope.
- Six `ui2` commits were authored on master's first-parent line 2026-08-02 → 2026-08-06, including
  merge `023a7dd` labelled *"owner-directed"*.
- `ui2/shell/render_settings.rs` was modified in the working tree by owner-directed shadow-tier work
  dated the same day.
- It is engine settings plumbing filed under `ui2/shell/`, not theme, dock or shell chrome.

Then add, following the SSAO block (`:418-459`) verbatim as the template: `ssgi_intensity`,
`ssgi_radius` (world units), and `ssgi_rays` if Gate 5 justifies exposing it. (`ssgi_enabled`
already exists from Gate 1.)

**Plus one control that does NOT go in the SSGI block:** `ssgi_env_occlusion` — labelled
**"HDRI Occlusion"**, `0.0..=1.0`, **default 1.0** — belongs with the **environment / HDR**
controls (near `env_brightness` / `env_rotation`), because it does nothing without an HDRI loaded.
See §4.2 for what it does and why it is defensible below 1.0. Hover text must say plainly that
values under 1.0 are a deliberate artistic choice that stops SSGI from removing occluded sky, so
nobody later reads a non-1.0 default as a bug. Gate 3's oracle already covers it. Fields go on `app_types::RenderSettings` with the same "runtime-only,
save-format red line" doc comment as `use_gtao` (`types_render.rs:52-55`). Pattern: `.changed()` →
`self.mark_dirty(DirtyFlags::RENDER)`. No `transaction()`, no undo.

**Also report, do not fix:** `CLAUDE.md:55-60`'s first two hard bans forbid what the owner has been
doing daily; and `docs/WORKTREES.md:34` asserts the exact inverse of its own `:99`.

**Acceptance.**
1. One toggle + sliders, appearing in all delegating panels from the single edit.
2. Default off ⇒ frame bit-identical to pre-feature (re-run Gate 1.1).
3. Save a `.fl4` with SSGI on, reload: the setting is **not** persisted (runtime-only) and nothing
   in the file changed. This is the R8 gate — one line, and it catches an agent that "helpfully"
   adds the field to `project::RenderSettings`.
4. The conflict report, in the task write-up, with the evidence above.
5. Rebased cleanly onto any in-flight edits in the same files; their hunks intact.

### Gate 7 — Re-measure, and the shipping decision

**Task.** Load a real scene. Run the owner's workflow. Report:
- measured SSGI ms and total frame ms, with and without, warm, same scene and camera;
- the Gate 2/3/4 numbers re-run at the end;
- `cargo test -p graphics`, `-p render`, `-p app` counts;
- **one honest paragraph on what SSGI does and does not do in this build**, including the §0
  limitations (no off-screen light, screen-edge fade) and the §4.2 deliberate residuals, plus any
  gate that came out partial.

Update `RENDERER_ROADMAP.md` (add SSGI to shipped; strike item 5 per §0),
`COMPLETE_BUG_TRACKER.md`, and this document's status table.

A partial result reported accurately is worth more than a claim.

> #### 🔍 VG-E — "ship it or not" (owner approval, final)
> **Why:** default-off is the safe choice (R3), but the owner decides whether this is good enough
> to be discoverable, and whether the default intensity is right.
> **Setup:** three of the owner's own scenes, chosen by them — ideally one interior, one object on
> a floor, one with an HDRI doing most of the lighting. Off/on pairs for each, at the default
> intensity, plus one capture at whatever intensity the owner considers correct.
> **Owner judges:** does this make the viewport look more like the path-traced reference? Is the
> default intensity right, too strong, or too weak? Is it worth its measured cost?
> **Outcome to record:** APPROVE-as-default-off / APPROVE-and-change-default / REJECT-with-notes,
> plus the chosen default `ssgi_intensity`.
> **Owner verdict:** ⬜ APPROVE ⬜ REJECT — note: ______________________

---

## 6. Traps — specific, found in this codebase, not generic advice

**T1 — The 6 % ceiling.** See §3.5. Measure the delivered pixel change, not the wiring.

**T2 — `gbuf_normal.w` is two different quantities** (world distance for raster pixels, ray `t`
for raymarch-lite pixels) and the ortho conventions disagree between the raster writer and the SSR
metric. Choose one, write it down in the shader header, and make the test cover ortho.

**T3 — Emission, curvature and surface_id are STALE over raster pixels.** `gbuf_surface_id` is a
storage texture with no `RENDER_ATTACHMENT`/`COPY_DST` usage, so it is never cleared and retains
values from an earlier SDF frame (`render/src/gbuffer.wgsl:601-612`; deferred compensates at
`deferred_entry.wgsl:788`). **Do not source radiance from `gbuf_emission`.** The lit HDR already
contains emissive surfaces correctly; that is one more reason the radiance source is the HDR.

**T4 — Deterministic sampling only.** No blue noise, no TAA (§3.6). Per-frame jitter will boil.
Use a fixed per-pixel sequence and denoise spatially.

**T5 — GPU timestamps: the risk is not where it looks.** The cap is *not* the problem —
`MAX_QUERIES = 32` is an arbitrary local constant (wgpu's actual ceiling is 8192) and both buffers
derive their size from it, and **slot 13 (`PASS_BAKE`) is dead and free** anyway (§5 Gate 5). Two
real hazards:
- **Device loss / TDR** if `tw!()` is evaluated on a frame where the pass does not dispatch. The
  mark is eager, at construction; the slot's queries are then never reset but *are* resolved with
  the WAIT bit, and the GPU waits forever. Mitigation is placement, not care: `tw!()` goes **inside**
  the `if let (Some(bg), Some(pl))` immediately before `begin_compute_pass`. No assert enforces
  this — the invariant is held together by comments.
- **Silent zero.** `prof_gpu_pass_times: [f32; 16]` (`types/types_ui.rs:434`) is a hand-written
  literal, and `viewport_render.rs:476` copies `.min()` of the two lengths. Grow `MAX_PASSES`
  without growing that literal and the new pass reads 0.00 ms forever, with no error — you would
  then tune a pass whose cost you cannot see. Using the dead slot 13 sidesteps this entirely.

**T10 — Fourteen failure modes are already solved in `pass_ssr`. Rediscovering one is a defect.**
Self-intersection needs *three* guards (biased origin along both normal and ray, the backface test
as a structural self-reject, and the `u_lo` advance so refinement cannot bisect back onto the origin
surface) — and a constant bias is not enough at range, which is why SSAO scales it with distance
(`ssao.wgsl:176`). Thickness must be adaptive (`base + step's own depth advance × 1.5 + 1 % of
depth`) or hits flicker with stride. Screen edges need a smooth confidence fade, not a hard `break`.
The `depth == 0.0` background sentinel is checked in five distinct places for five distinct reasons;
missing one reads "distance 0" as a surface at the eye and occludes everything. Rays pointing back
at the camera must be rejected (`cam_fade`). `du = max(1/steps, 2/screen_len_px)` prevents the
degenerate stride that both `contact_shadow` and GTAO exhibit by stepping in world units. And
`opacity = min(gbuf_m.w, 1.0)` — reading `.w` raw gives > 1 on emissive pixels, because that channel
packs opacity + emissive luminance.

**T11 — `BlendMode::Additive` is alpha-gated.** Not used by this design (§4.7), but recorded because
it is a live landmine: `base.rgb + over.rgb * over.a`. Any future pass that writes a delta as
`vec4(rgb, 0.0)` and composites it through `dispatch_blend` will silently produce nothing.
`BlendParams::default()` also sets `opacity: 0.5`.

**T12 — Base your headless test on `ssr_gpu_test.rs`, not `ssao.rs`.** Same crate (`graphics`), same
class of pass, and its minimal ENV-consuming BGL at `:341-405` is the working example. Borrow only
`TestCamera::project_px` and `try_device` from `render/src/ssao.rs`.

**T6 — The alpha contract.** Floor/grid pixels are written with **alpha 0 = tonemap passthrough,
this color is final**; background alpha depends on whether an HDRI is loaded. Adding radiance to
those pixels is wrong twice over (they are unlit by design, and the add breaks the passthrough
contract). Reject them by flag band, and prove the rejection in Gate 1.

**T7 — Shared blend scratch.** `overlay_texture` / `blend_temp_texture` are shared with the
prim-outline blend that runs a few lines later, and `ensure_overlay_texture` resizes them from
inside the dispatch path. Do not assume they are yours.

**T8 — `#region` scoping.** Any symbol referenced inside a region tag must be *defined* inside the
same region, or stripping breaks the build for some variants only. `cosine_hemisphere` lives in
`RNG_TRACE`, which **both** deferred builds strip — you cannot simply call it from deferred.

**T9 — The floor is never lit.** raymarch-lite writes the grid with flat pre-baked color and
deferred returns early on it. It is neither a valid GI receiver nor a physically meaningful
emitter. Decide explicitly and document what SSGI does when a ray hits floor pixels.

---

## 7. Concurrency — other sessions are live in this repo

`git status` at plan time shows **uncommitted work you must not disturb or commit**:

- `app_types/src/types_render.rs`, `ui2/shell/render_settings.rs`, `render/src/shadows.rs`,
  `render/src/gbuffer.wgsl` — an in-flight **shadow "ultra" tier** (1024/2048/4096/8192) plus a
  hard-shadow single-tap floor at softness 0. **These are the same two files an SSGI setting must
  touch.** Rebase your edits onto whatever is there; do not revert their hunks.
- `app/src/project/mod.rs` — MatCap background serde defaults changed, which alters how **pre-v6
  saves** deserialize. Do not extend that change.
- `app/src/poly_knife.rs`, `app/src/library_panel.rs`, `app/src/app_update.rs`,
  `app/src/menus.rs` — unrelated in-flight work (knife fix, library perf gates, default HDRI,
  context-menu search).

**Stage files explicitly. Never `git add -A`** — that mistake committed another session's
half-finished file on 2026-08-05 and broke HEAD's test build.

---

## 8. Scope guard

**In scope:** one screen-space indirect-diffuse pass, its composite, its settings, its tests, its
documentation.

**Out of scope — do not start any of these, even if they look adjacent:**
- TAA, motion vectors, or any G-buffer slot change (R11).
- Screen-space *specular* GI / SSR changes. SSR exists and is not this task.
- SDF cone tracing (§0) — a later, separate plan.
- Probe capture of any kind.
- Touching the `0.12` ambient scale or the AO application point. That is an open **owner decision**
  in `SSAO_VISIBILITY_FINDING.md` with three named options; picking one silently is forbidden.
- Reflection probes, auto-exposure, colour grading (`RENDERER_ROADMAP.md` Tier 1/2 items).
- Any refactor of `viewport_render.rs`. It is a god function under a separate split plan.
