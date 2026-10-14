# Viewport Shading / Render-Mode Bar — Plan

**Status:** READ-ONLY investigation + plan. No code changed. Branch: `feat/ui-polish` (ui-polish worktree — UI gate passed).

**Owner spec recap:** 5 mode buttons (Wire / Solid / Material / PBR / Path Trace), left-click = activate, right-click = that mode's settings dropdown; a trailing "more options" ⌄ arrow → global render/view settings; every dropdown is a REAL detachable/dockable ui2 panel (new `PanelKind` per mode). Map all 7 engine `ShadingMode` variants losslessly onto the 5 buttons.

---

## Where the bar lives (architectural gotcha — decide first)

The current 4-icon strip is `ui2_shading_strip` in `core_engine/app/src/ui2/shell/shading.rs`, invoked from the **context bar** (`ui2/shell/ctxbar.rs:68`), right-aligned inside a `right_to_left` layout, next to the `View` pill. The context bar is `H_CONTEXT = 36px` (`ui_theme.rs:107`); the app bar above it is `H_APPBAR = 44px`.

**Recommendation: keep the bar in the ctxbar (where it is today), NOT the appbar, and NOT a viewport overlay.**

- The mockup already puts shading top-right in the ctxbar (`#shading margin-left:auto`); ctxbar.rs comments cite it. Moving it to the appbar breaks mockup parity and collides with workspace tabs.
- A viewport *overlay* (floating over the scene rect) is wrong: it would fight the Gate-8 viewport split, the corner-drag gesture, and gizmo/camera input capture, and it can't right-align cleanly against the dock edge. The ctxbar is chrome — no input-capture conflict.
- 36px is enough for 5 compact 28×24 cells + a ⌄ button. The strip already renders inside `right_to_left`, so it grows leftward without disturbing the centre pills.

The dropdowns themselves are the *only* thing that must be able to leave the bar — and they leave as **dock panels**, not overlays (that's the whole point of the new `PanelKind`s). Right-clicking a mode button opens a lightweight **anchored popup** (like today's `context_menu`) for quick edits; that popup carries a **"Open as panel ⧉"** button (and the ⌄ arrow's menu likewise) that spawns/reveals the real dockable `PanelKind`. Two tiers: fast popup for a slider tweak, full panel when you want it docked.

---

## DELIVERABLE 1 — MODE MAPPING (5 buttons ← 7 ShadingMode variants)

Engine enum (`app_types/src/types_transform.rs:135`):
`ShadingMode { Pbr, MatCap, Lit, Wireframe, SolidHdr, Raytrace, PathTrace }` — 7 variants.

**Dispatch evidence — the authoritative view-mode selector** (`viewport_render.rs:1019`):
```rust
let vm = match self.render.shading_mode {
    ShadingMode::Pbr=>0, ShadingMode::MatCap=>1, ShadingMode::Wireframe=>2,
    ShadingMode::SolidHdr=>3, ShadingMode::Raytrace=>4, ShadingMode::PathTrace=>5,
    ShadingMode::Lit=>6 };
```
`vm` is packed into GpuUniforms and read by the deferred shader to pick the shading branch. Additional dispatch forks keyed on the mode:
- `viewport_render.rs:2421` — `use_lite_deferred` forced for `MatCap | SolidHdr` (lite deferred path).
- `viewport_render.rs:2376` — SSAO active only for `Pbr | Lit`.
- `viewport_render.rs:1039, 175, 1025, 1029, 1032, 2258, 2784, …` — the entire `PathTrace` arm (progressive accum, PT-only fog/SSR suppression, TLAS, denoise).
- `viewport_render.rs:36-48` — `#[cfg(feature="cuda")]` HW-RT branch: fires for `use_cuda` OR auto `PathTrace + hw_rt_available()`. **`Raytrace` (`vm==4`) has NO live WGSL branch** — see D3.

The v1 grouping in `shading.rs:31-40` already collapses the 7 onto 4 buttons; the owner's 5-button split separates Material (Pbr) from PBR — but in the engine **Material and PBR are the same `ShadingMode::Pbr` branch**. The distinction the owner wants is *authoring intent* (untextured lookdev vs full PBR), which today is one mode. Resolution below: the "Material" button = `Pbr` with a **matcap-style / neutral-material lean** (drives MatCap as its secondary), and "PBR" = `Pbr` full. See notes.

| # | Button | Icon (phosphor) | LEFT-CLICK activates | RIGHT-CLICK dropdown hosts (secondary modes + settings) | Engine evidence |
|---|--------|-----------------|----------------------|----------------------------------------------------------|-----------------|
| 1 | **Wire** | `BOUNDING_BOX` | `ShadingMode::Wireframe` (`vm=2`) | Wire settings: `wire_overlay_thickness/opacity/color/backface`, `wire_overlay_enabled` (wire-on-shaded). No secondary mode. | `vm=2`; `gu.dc_wire_px` @ `viewport_render.rs:1376` |
| 2 | **Solid** | `CUBE` | `ShadingMode::SolidHdr` (`vm=3`) | Solid settings: `exposure` (HDR). **Secondary modes: MatCap (`vm=1`) + Lit (`vm=6`)** as selectable sub-items (both are "flat form shading" siblings of Solid — MatCap settings + Lit exposure live here). | `vm=3`; lite-deferred @ `:2421` groups `MatCap\|SolidHdr`; v1 groups `SolidHdr\|Lit\|MatCap` under one active state (`shading.rs:33`) |
| 3 | **Material** | `CIRCLE_HALF` | `ShadingMode::Pbr` (`vm=0`) | Material/lookdev settings: exposure, environment IBL (brightness/contrast/saturation/blur/rotation). **Secondary: MatCap** available here too as the "sculpt-clay lookdev" alt. | `vm=0`; env sliders `viewport_render.rs` env block |
| 4 | **PBR** | `SUN` (or `SPHERE`) | `ShadingMode::Pbr` (`vm=0`) — same engine mode, **full post-FX enabled** | PBR settings: tonemapper, bloom, SSR, SSAO/GTAO, sun shadows, RT shadows, fog, FXAA, SDF edge AA. | `vm=0`; SSAO `Pbr\|Lit` @ `:2376` |
| 5 | **Path Trace** | `SPARKLE` (`✨`) | `ShadingMode::PathTrace` (`vm=5`) | PT settings (huge): quality presets, bounces, spp, DOF, denoise, live-hybrid A–F, atmosphere, clamp, stop-mode. **Secondary: Raytrace (`vm=4`)** listed here (CUDA-only; disabled/greyed without the cuda feature — see D3c). | `vm=5`; PT arm throughout; `Raytrace` `vm=4` CUDA-gated `:36` |

**Lossless mapping check** — all 7 covered exactly once as a primary or secondary:
- Wireframe → **Wire** (primary).
- SolidHdr → **Solid** (primary). MatCap → **Solid** secondary (also surfaced on Material). Lit → **Solid** secondary.
- Pbr → **Material** primary AND **PBR** primary (same engine mode; two buttons differ only in which post-FX defaults/settings-page they present — this is the one owner-requested split that has no engine counterpart; documented, not a hack: both write `shading_mode = Pbr`, the buttons differ by the settings panel they open and by a `render_quality`/post-FX preset they apply).
- Raytrace → **Path Trace** secondary (CUDA-only).
- PathTrace → **Path Trace** primary.

> **Owner call needed (Material vs PBR):** since both are engine `Pbr`, options are (a) two buttons that both set `Pbr` but apply different post-FX presets + open different panels (recommended, matches the ask), or (b) collapse to 4 buttons (Wire/Solid/Material/PT) and drop the redundant PBR button. Flagging per the Rule-Conflict-Check habit — the spec says 5 buttons, so (a) is the default unless owner prefers 4.

---

## DELIVERABLE 2 — SETTINGS INVENTORY

Source of truth: `app_types/src/types_render.rs` `RenderSettings` (the struct), `app/src/properties_render.rs` (`draw_props_render` — the v1 Render panel), `app/src/viewport_header.rs` (v1 per-mode right-click dropdowns), `app/src/win_tweaks.rs` (Developer Tweaks). "Consumer" verified by grep; file:line cited.

Legend for **Applies to**: W=Wire, S=Solid, Ma=Material, P=PBR, PT=PathTrace, RT=Raytrace(CUDA), G=Global/all.

### Group → **Wire** dropdown (`PanelKind::WireSettings`)
| Field | Controls | Applies | Consumer (grep) |
|---|---|---|---|
| `wire_overlay_thickness` | screen-px wire width (0=off) | W, G-overlay | `gu.dc_wire_px` `viewport_render.rs:1376`; deferred shader dilation |
| `wire_overlay_opacity` | wire alpha | W, G | overlay build (`overlay_lines_fp`), `draw_viewport_overlays` |
| `wire_overlay_color` | wire RGB | W, G | overlay line color |
| `wire_overlay_backface` | draw back-facing edges | W, G | overlay cull |
| `wire_overlay_enabled` | wire-on-shaded (Shift+Z) | G-overlay | `gu.dc_wire_px` gate `:1377`; alive |

### Group → **Solid** dropdown (`PanelKind::SolidSettings`) — incl. MatCap + Lit secondaries
| Field | Controls | Applies | Consumer |
|---|---|---|---|
| `exposure` | HDR exposure | S, Ma, P, (Lit) | packed to GpuUniforms; post_process tonemap. Read across deferred. |
| `matcap_bg_color` | MatCap bg | MatCap | deferred vm==1 branch (sdf_lighting.wgsl matcap) |
| `matcap_bg_gradient` / `matcap_bg_gradient_top` | MatCap gradient bg | MatCap | same |
| `matcap_rotation` | MatCap env rotate | MatCap | same |
| `matcap_edge_enabled` / `matcap_edge_color` / `matcap_edge_thickness` / `matcap_edge_threshold` | MatCap edge lines | MatCap | deferred matcap edge |

### Group → **Material** dropdown (`PanelKind::MaterialSettings`)
| Field | Controls | Applies | Consumer |
|---|---|---|---|
| `env_brightness` | IBL + bg intensity | Ma, P | GpuUniforms env; deferred IBL |
| `env_contrast` | env contrast | Ma, P | env prefilter/IBL |
| `env_saturation` | env saturation | Ma, P | env |
| `env_blur` | env blur | Ma, P | env prefilter |
| `env_rotation_deg` | rotate IBL+bg | Ma, P | env sampling (runtime-only) |

### Group → **PBR** dropdown (`PanelKind::PbrSettings`)
| Field | Controls | Applies | Consumer |
|---|---|---|---|
| `tonemapper` | HDR→LDR curve (0 ACES…5 None) | P, all raster | `pass_tonemap viewport_tonemap`; post_process.wgsl |
| `bloom_enabled/threshold/intensity` | bloom | P, all | `bloom_flags` `viewport_render.rs:1028`; bloom pass |
| `ssr_enabled` | screen-space reflections | P | `bloom_flags` bit4 `:1029` (suppressed in PT) |
| `ssr_max_steps` | SSR march budget | P | SSR pass (runtime-only) |
| `ssr_intensity` | SSR blend | P | SSR pass |
| `ssao_enabled` | ambient occlusion | P, (Lit) | `ssao_active` `:2375` (`Pbr\|Lit` only) |
| `use_gtao` | GTAO vs SSAO tier | P, Lit | ssao dispatch `render/src/ssao.rs` |
| `ssao_strength` / `ssao_radius` | AO tuning | P, Lit | ssao blur pass |
| `shadow_map_resolution` | CSM cascade res | P, S, Ma (raster) | `ShadowMapSystem`; recreate in poll |
| `shadow_pcf_softness` | penumbra | raster | gbuffer.wgsl PCF |
| `shadow_blur_radius` | shadow/AO bilateral blur | raster | `gu.shadow_blur_radius` `:1372`, `:2341` — ALIVE |
| `rt_shadows` | RT point/spot shadows | P (non-PT) | `rt_active` `:2258`; `gu.rt_shadows_on` `:1344` |
| `rt_shadow_softness` / `rt_shadow_ray_samples` / `rt_shadow_frame` | RT soft shadow | P | rt_shadow pipeline |
| `fog_enabled` + `fog_density/height_falloff/scatter/phase_g/absorption/noise_scale/noise_strength/max_distance/shadow_steps/temporal_blend/frame_index` | height fog | P, S, Ma (non-PT) + PT (own path) | `fog_composite` `:1025`; `pass_fog`; PT in-scatter |
| `fxaa_enabled` | post AA | G | post_process fxaa |
| `sdf_edge_aa_enabled` | silhouette AA | G | post_process |

### Group → **Path Trace** dropdown (`PanelKind::PtSettings`) — the biggest page
`pt_max_bounces, pt_samples_per_frame, pt_stop_mode, pt_noise_threshold, pt_max_accum_frames, pt_adaptive_spp, pt_clamp, pt_fast_mode, pt_dof_aperture, pt_dof_falloff, pt_focus_radius, pt_denoise, denoise_strength, denoise_radius, pt_denoise_end_on_hdr, pt_show_spp, pt_transparent_bg, pt_temporal_reproject, pt_live_hybrid_edit, pt_live_draft_mesh, pt_live_draft_settle_ms, pt_soft_hybrid_resume, pt_soft_hybrid_frames, pt_draft_soft_reset, pt_draft_soft_frames` — all read by the PT arm / `pathtrace_tri_entry.wgsl` (see `properties_render.rs:1112-1192` + `draw_pt_settings_window` `:1405`). **Reuse `draw_pt_settings_window` body wholesale** — it is already a self-contained PT settings surface. `rt_bounces/rt_shadow_samples/rt_env_samples/rt_caustics` (Raytrace secondary) → CUDA-only, see D3.

### Group → **"more" ⌄ / Global** dropdown (`PanelKind::RenderSettings`)
| Field | Controls | Applies | Consumer |
|---|---|---|---|
| `render_quality` | 0 Draft…4 Ultra | G | `:2463`, `:2484` — half-res / vol gates |
| `resolution_scale` / `upsample_enabled` | render scale + CAS upscale | G | resolution + `Upscale (CAS)` post |
| `march_resolution` (dev) | lite-march scale | G | `march_scale` `:192` |
| `ortho` | ortho/persp | G/view | `:1067`; camera proj |
| `fov_deg` | FOV | G/view | `:1399` fov_scale |
| `interaction_half_res` / `interaction_shadows` | interaction degrade | G | interaction latch |
| Per-body bake: `voxel_cache_res` / Grid picker / Per-Body Cache / Adaptive / Detail-LO | SDF bake resolution | (SDF, not shading) | `apply_body_grid_resolution` etc. — **stays in Properties→SDF, not the shading bar** |
| Env: `Load HDRI…` / clear | environment | Ma, P | `request_env_file_dialog` |
| Multi-GPU: `worker_gpu_enabled` | worker GPU enable | G | `draw_multi_gpu_settings` |

---

## DELIVERABLE 3 — DEAD / REDUNDANT / CUDA-GATED LIST

Method: grep each field across the whole tree; a field is DEAD if the only hits are its `RenderSettings` def, `Default`, project save/load serialization, and the v1 slider — with **no shader/uniform/CPU read** that affects output.

### (a) CERTAIN-DEAD — no consumer that affects rendering

| Field(s) | Evidence | Confidence |
|---|---|---|
| `dof_enabled`, `dof_focus_dist`, `dof_aperture`, `dof_max_blur` (the **viewport** DOF block) | `grep render\.dof_enabled\|render\.dof_focus_dist\|render\.dof_aperture\|render\.dof_max_blur` → **0 reads**. All `dof_*` hits belong to a DIFFERENT struct `self.compositor.effects[].dof_*` (`properties_compositor.rs:144-154`, `ui_update.rs:37`). `post_process.wgsl:1091` hardcodes `let focus = config.sel_rim_base; // TODO: wire dof_focus_dist … currently 0.0 — viewport DOF disabled`. The v1 UI itself renders a **disabled** checkbox + "⚠ Viewport DOF not wired yet" (`properties_render.rs:1199-1208`). `build_errors.txt` shows these fields mid-removal. | **certain-dead** — do NOT add controls; PT DOF (`pt_dof_*`) is the real path. |
| `volume_tricubic`, `volume_early_out`, `volume_cone_step` | `grep … *.wgsl` → **0 files**. These were read only by the deleted VP volume-raymarch shader (RenderMode::VolumePreview, removed 2026-07-13). No GPU consumer remains. | **certain-dead** (VP shader deleted). |
| `volume_ao_radius`, `volume_ao_step_scale`, `volume_ao_samples`, `volume_ao_bias`, `volume_ao_color`, `volume_ao_invert`, `volume_ao_opacity` | Of the whole `volume_ao_*` block, **only** `volume_ao_intensity` is still packed (`viewport_render.rs:1496`, onto `scene_aabb_min[3]`, gated by `volume_ao_enabled && volume_ao_opacity>0.001`). `opacity` is used only as a boolean gate; `radius/step_scale/samples/bias/color/invert` are packed **nowhere** — their sliders write inert state. | **probably-redundant** — `volume_ao_enabled`/`volume_ao_intensity`/`volume_ao_in_orbit` alive; the other 6 sliders are dead. Owner call whether to keep the AO intensity control at all (it's a vestige of the deleted VP path). |
| `foveated_enabled`, `foveated_inner/outer/min_steps/threshold_mult/max_pixel_scale`, `lod_dist_enabled`, `lod_dist_threshold_0/1/2`, `lod_min_steps`, `lod_debug_colors`, `march_adaptive_enabled`, `march_lod_scale`, `march_relaxation`, `march_bisection_steps` | Packed into GpuUniforms (`viewport_render.rs:1320, 1358-1371`) but the WGSL only **declares** them as struct fields (`sdf_shared.wgsl:107-121`, `post_process.wgsl:100-114`) — **grep for read expressions in any shader body = none**. They were consumed exclusively by the analytical SDF march display, DELETED 2026-07-14. The uniform slots must stay for layout, but the settings are inert (SDF now renders from baked DC triangles). | **certain-dead as user settings** (uniform slots stay for ABI; do not build controls). |

### (b) PROBABLY-REDUNDANT — superseded / duplicated

| Field(s) | Evidence | Confidence |
|---|---|---|
| `exposure` appears in FOUR v1 places | v1 exposes `exposure` in the Render panel HDR section (`properties_render.rs:1006`) AND in the PBR, Solid, and Lit right-click dropdowns (`viewport_header.rs:138,248,271`). Same field, 4 widgets. New design: one control per settings panel is fine, but note it's ONE field — don't imply per-mode exposure. | **redundant widgets, one field** — consolidate. |
| `denoise` controls duplicated | `pt_denoise/denoise_strength/denoise_radius/pt_denoise_end_on_hdr` rendered in BOTH `draw_props_render` PT section (`:1149`) and `draw_pt_settings_window` (`:1754`). Same fields. | **redundant widgets** — the new `PtSettings` panel replaces both. |
| `bg_color_top`, `bg_color_bottom` | ALIVE consumer (`app_update.rs:542-543` → `bg_top_packed`/`bg_bottom_packed` GpuRenderConfig; gradient bg). BUT the v1 UI comment says "Background color removed — always uses HDR environment" (`properties_render.rs:1270`, `viewport_header.rs:155`) — **no UI writes them anymore**, they sit at the `0.003` default. Field is live but user-uncontrollable. | **needs-owner-call** — re-expose a bg-color control in Global, or leave HDRI-only. Not dead; not currently editable. |
| `fog_frame_index`, `rt_shadow_frame`, `pt_frame_seed`, `pt_accumulated_frames`, all `pt_last_cam_*`, `pt_lighting_sig`, `pt_was_*`, `pt_pending_*`, `pt_warm_notified`, `interaction_idle_frames`, `interaction_active_frames`, `last_frustum_culled/total` | Runtime bookkeeping, not user settings. | **not-settings** — never put in a panel (listed so the next dev doesn't try). |

### (c) FEATURE-GATED — CUDA build only

| Field(s) | Evidence | Confidence |
|---|---|---|
| `use_cuda`, `cuda_backend` | Read only under `#[cfg(feature="cuda")]` at `viewport_render.rs:42` and `cuda_render_dispatch.rs`. No effect without `--features cuda`. | **cuda-only** — hide unless cuda. |
| `ShadingMode::Raytrace` (`vm=4`) + `rt_bounces`, `rt_shadow_samples`, `rt_env_samples`, `rt_caustics` | `Raytrace` has NO live WGSL branch. `rt_*` read only by `graphics/types.rs` GpuUniforms packing (`:458-469`) and `cuda_render_dispatch.rs:77-81` (the CUDA path). Without cuda, selecting Raytrace falls through to the raster/deferred path (no analytical RT exists — deleted 2026-07-14). | **cuda-only** — Raytrace secondary + its 4 settings are inert without cuda; grey/disable them, or hide behind the cuda cfg. Distinct from `rt_shadows`/`rt_shadow_*` which are the LIVE WGSL RT-shadow path (not cuda). |

**Headline dead items (name them):** the 4 viewport-DOF fields (`dof_enabled/focus_dist/aperture/max_blur`); the 3 volume-march toggles (`volume_tricubic/early_out/cone_step`); the 6 orphaned `volume_ao_*` sub-sliders; the whole foveated/LOD/adaptive-march cluster (~13 fields) left behind by the deleted analytical march. Plus CUDA-only: `Raytrace` mode + `rt_bounces/rt_shadow_samples/rt_env_samples/rt_caustics` and `use_cuda/cuda_backend`.

---

## DELIVERABLE 4 — BUILD PLAN

### Step 0 — Decide Material-vs-PBR (owner) · risk: none · size: XS
Resolve the D1 note. If 5 buttons: both write `shading_mode=Pbr`, differ by settings-panel + a post-FX preset (`render_quality` / bloom/ssr defaults). If 4: drop PBR, keep Material=Pbr-full.

### Step 1 — New `PanelKind` variants · risk: LOW · size: S
`ui2/types.rs`:
- Add to enum (`PanelKind`, ~:9): `WireSettings, SolidSettings, MaterialSettings, PbrSettings, PtSettings, RenderSettings`.
- Extend `label()` (~:15): "Wire", "Solid", "Material", "PBR", "Path Trace", "Render".
- **Change `all()` array length** `[PanelKind; 13]` → `[PanelKind; 19]` and add the 6 (append at END; order only affects the Add-Panel menu, not persistence).
- `tool_pillar()` returns `None` for all 6 (they're not tool-mode windows).

Compile-forcing exhaustive matches (they will error until updated — this is the safety net):
- `ui2/persist.rs::panel_kind_id` (~:27) — add 6 **stable string ids**: `"wire_settings","solid_settings","material_settings","pbr_settings","pt_settings","render_settings"`. `panel_kind_from_id` derives from `all()` — no change once `all()` includes them. **These strings are the on-disk contract — never rename.**
- `ui2/pane.rs::fill_pane_body` (~:660) `match g.active_kind()` — add 6 arms → new draw fns (Step 3).

### Step 2 — The 5-button bar + ⌄ arrow · risk: MED · size: M
Rewrite `ui2/shell/shading.rs::ui2_shading_strip`:
- Replace the 4-tuple array with 5 buttons; each button knows its **primary** `ShadingMode` and its **settings `PanelKind`**.
- LEFT-CLICK: set `self.render.shading_mode = primary`, mirror the existing dither/`pt_accumulated_frames=0` side-effects (copy from `viewport_header.rs:107-114`), `mark_dirty(DirtyFlags::RENDER)`.
- `on` (active-highlight) logic: reuse the current grouping so a button reads "active" when its primary OR any secondary is current (Solid lights for MatCap/Lit; PT lights for Raytrace) — copy `shading.rs:31-40`.
- RIGHT-CLICK: `resp.context_menu(...)` — a compact popup with the mode's top ~3 settings inline PLUS an **"Open panel ⧉"** button that calls `ui2_reveal_settings_panel(kind)` (Step 5). (Keep the fast-popup so a quick thickness/exposure tweak doesn't require docking a panel.)
- Trailing **⌄ arrow** cell after button 5: opens a popup → "Render Settings", "View (ortho/persp/FOV/split)", "Open Render panel ⧉" → `ui2_reveal_settings_panel(PanelKind::RenderSettings)`.
- Icon set: `BOUNDING_BOX / CUBE / CIRCLE_HALF / SUN / SPARKLE` + `CARET_DOWN` for the arrow. Keep 28×24 cells; the strip already renders in `right_to_left`.

### Step 3 — Six draw fns (REUSE v1 widgets — do NOT re-author sliders) · risk: LOW-MED · size: M
Add `impl FourthLayerApp` fns, one per panel, each taking `&mut egui::Ui`. **Reuse existing v1 code by extraction, not copy:**
- `ui2_draw_wire_settings` ← the wire block already in `shading.rs:73-136` (lift into a fn both the popup and the panel call).
- `ui2_draw_solid_settings` (Solid exposure + MatCap group + Lit exposure) ← extract from `viewport_header.rs:157-211,244-272`.
- `ui2_draw_material_settings` (env IBL) ← extract the env sliders from `properties_render.rs:1304-1310` / `viewport_header.rs:140-144`.
- `ui2_draw_pbr_settings` ← extract the tonemapper/bloom/SSR/SSAO/shadows/fog/AA block from `properties_render.rs:985-1098` into a reusable fn.
- `ui2_draw_pt_settings` ← **wrap `draw_pt_settings_window`'s inner closure body** (`properties_render.rs:1405-1789`); factor the closure body into `ui2_draw_pt_settings(&mut self, ui)` and have the old `egui::Window` call it too (single source, kills the D3b denoise duplication).
- `ui2_draw_render_settings` (Global ⌄) ← extract quality/resolution/ortho/fov/interaction/env-load/multi-gpu from `properties_render.rs` (`render_quality` `:156`, HDRI `:1292`, multi-gpu `:1314`).

**Do NOT expose** the D3 dead fields in any of these (no viewport-DOF, no volume-march toggles, no foveated/LOD). CUDA-only (`Raytrace` + `rt_*`, `use_cuda`) gate behind `#[cfg(feature="cuda")]` or grey with a tooltip.

Where a widget currently mutates `self.render.*` directly and calls `self.mark_dirty(DirtyFlags::RENDER)`, keep exactly that — all these are **view-only** settings (RENDER dirty is correct; see Step 6).

### Step 4 — `fill_pane_body` dispatch · risk: LOW · size: XS
`ui2/pane.rs:660` add:
```rust
Some(PanelKind::WireSettings)     => app.ui2_draw_wire_settings(ui),
Some(PanelKind::SolidSettings)    => app.ui2_draw_solid_settings(ui),
Some(PanelKind::MaterialSettings) => app.ui2_draw_material_settings(ui),
Some(PanelKind::PbrSettings)      => app.ui2_draw_pbr_settings(ui),
Some(PanelKind::PtSettings)       => app.ui2_draw_pt_settings(ui),
Some(PanelKind::RenderSettings)   => app.ui2_draw_render_settings(ui),
```
Wrap each in a `ScrollArea::vertical` like `ui2_tool_params_pane` (`pane.rs:679`) since PBR/PT pages are tall.

### Step 5 — "Open as panel" helper (dock/float/reveal) · risk: LOW · size: S
Add `fn ui2_reveal_settings_panel(&mut self, kind: PanelKind)`:
- If a group already contains `kind`, un-hide its zone + set it active (reuse `ui2_set_panel_visible(&[kind], DockSide::Right, true)` — `dock.rs:447`).
- Else create a group. Default = **float** near the bar (so it doesn't shove the dock), OR dock-right. Recommend: `self.ui2.dock.push(self.ui2.mk(vec![kind], DockSide::Float))` with `pos` under the bar. The user can then drag it into the dock — the existing grip-drag / `ui2_drop_target` / `ui2_move_to_slot` machinery (`dock.rs:399-425,667-704`) already handles docking, tabbing, floating, and the tab-peel `⋮` — **zero new dock code needed**. All four systems (types/dock/persist/pane) already treat any `PanelKind` uniformly.

### Step 6 — Mutation correctness · risk: LOW · size: XS (audit)
- **Every field in these panels is view-only → `mark_dirty(DirtyFlags::RENDER)`** (per DirtyFlags Discipline: camera/shading/post are RENDER, never SCENE). The v1 code already does this everywhere being reused — preserve it verbatim.
- **The ONE exception that needs a bake, and it is NOT in the shading bar:** `voxel_cache_res` / Grid resolution / Per-Body Cache / Detail-LO. These call `apply_body_grid_resolution` → `trigger_body_vol_bake()`. **Keep them in Properties → SDF page, NOT in the shading dropdowns** (they are geometry/bake settings, not shading). If the Global ⌄ links to them, link to the existing Properties path — do not duplicate the bake trigger.
- `shading_mode` change itself is RENDER-dirty (no rebake) — confirmed by v1 (`viewport_header.rs:115`).
- PT settings that reset accumulation set `pt_accumulated_frames=0; pt_converged=false; mark_dirty(RENDER)` — preserve (already in the reused code).

### Step 7 — Persistence check · risk: LOW · size: XS
The 6 new `PanelKind`s serialize automatically once Step 1's `panel_kind_id`/`from_id` cover them (`persist.rs` snapshot/apply iterate `dock` generically). No schema bump: `BLOB_VERSION` stays 1; unknown ids already skip individually (fail-soft). A layout saved before this feature loads fine (no new required fields). **Verify:** float a PtSettings panel, restart, it reappears floating.

### Build / test
- `cargo build -p app` after Steps 1-2, again after 3-5 (CLAUDE Build Discipline).
- Manual scenario: click each of the 5 buttons → correct `vm` in viewport; right-click each → popup opens + "Open panel" docks a real panel; drag it into the right dock, tab it onto Properties, float it back, restart → layout persists; toggle a PBR slider → viewport updates with no rebake stutter.

### Risk summary
| Step | Size | Risk | Why |
|---|---|---|---|
| 0 Material/PBR decision | XS | none | owner input |
| 1 PanelKind enum | S | LOW | exhaustive matches force completeness |
| 2 5-button bar | M | MED | input routing, active-state grouping, layout in 36px |
| 3 six draw fns | M | LOW-MED | pure extraction; risk = accidentally re-authoring instead of reusing |
| 4 pane dispatch | XS | LOW | 6 match arms |
| 5 reveal/dock helper | S | LOW | reuses existing dock machinery entirely |
| 6 mutation audit | XS | LOW | keep RENDER-dirty; keep bake settings out |
| 7 persist | XS | LOW | automatic once ids added |

**Net:** medium feature, low architectural risk. The dock/float/persist plumbing is fully generic over `PanelKind` — new settings panels are first-class the moment the enum + 3 exhaustive matches + 1 dispatch arm are updated. The real work is the button bar's input UX and disciplined REUSE of the v1 render widgets (not re-authoring them), while omitting the ~26 dead/cuda-only fields from D3.
