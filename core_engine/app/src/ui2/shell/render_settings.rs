//! Shared viewport shading / render-settings widget bodies.
//!
//! ONE source of truth for every render-settings slider. The v1 Render panel
//! (`properties_render.rs`), the v1 per-mode dropdowns (`viewport_header.rs`),
//! the v1 floating PT window, AND the ui2 shading-bar settings panels
//! (`WireSettings` … `RenderSettingsGlobal`) all call these — no slider code is
//! authored twice (No-Duplication rule).
//!
//! Every field here is view-only → `mark_dirty(DirtyFlags::RENDER)` (never SCENE;
//! DirtyFlags Discipline). PT settings that invalidate accumulation also reset
//! `pt_accumulated_frames`/`pt_converged`, exactly as the v1 code did.
//!
//! Dead / orphaned fields are deliberately NOT surfaced here — see
//! `docs/UI/VIEWPORT_SHADING_BAR_PLAN.md` D3 + the owner override in the build
//! prompt: no foveated/LOD/adaptive-march, no viewport-DOF (`dof_*`), no
//! `cone_step`. Live-but-vestigial `volume_*` / `pt_dof_*` / `rt_shadow_*` ARE
//! kept.

use eframe::egui;
use crate::*;
use crate::ui_menus::DefaultMenu;

impl FourthLayerApp {
    // ── WIRE ────────────────────────────────────────────────────────────────
    /// DC-mesh wireframe overlay: thickness / opacity / color / backfaces +
    /// the wire-on-shaded (Shift+Z) toggle. Drives the GPU wire pass.
    pub(crate) fn render_wire_settings_body(&mut self, ui: &mut egui::Ui) {
        if ui
            .add(
                egui::Slider::new(&mut self.render.wire_overlay_thickness, 0.0..=20.0)
                    .text("Thickness")
                    .clamping(egui::SliderClamping::Never),
            )
            .on_hover_text("0 = hidden. Screen-space width in pixels.")
            .changed()
        {
            self.render.wire_overlay_thickness = self.render.wire_overlay_thickness.max(0.0);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .add(egui::Slider::new(&mut self.render.wire_overlay_opacity, 0.0..=1.0).text("Opacity"))
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        ui.horizontal(|ui| {
            ui.label("Color");
            let mut c32 = egui::Color32::from_rgb(
                (self.render.wire_overlay_color[0] * 255.0) as u8,
                (self.render.wire_overlay_color[1] * 255.0) as u8,
                (self.render.wire_overlay_color[2] * 255.0) as u8,
            );
            if ui.color_edit_button_srgba(&mut c32).changed() {
                self.render.wire_overlay_color = [
                    c32.r() as f32 / 255.0,
                    c32.g() as f32 / 255.0,
                    c32.b() as f32 / 255.0,
                ];
                self.mark_dirty(DirtyFlags::RENDER);
            }
        });
        if ui
            .checkbox(&mut self.render.wire_overlay_backface, "Show backfaces")
.default_menu("render.wire_overlay_backface", serde_json::json!(self.render.wire_overlay_backface))
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        ui.separator();
        if ui
            .checkbox(
                &mut self.render.wire_overlay_enabled,
                format!(
                    "Wire on shaded ({})",
                    self.keybindings.label(ACT_WIRE_OVERLAY)
                ),
            )
.default_menu("render.wire_overlay_enabled", serde_json::json!(self.render.wire_overlay_enabled))
            .on_hover_text("Draw mesh edges over any shading mode without switching to DC Wire.")
            .changed()
        {
            self.overlay_lines_fp = u64::MAX;
            if self.render.wire_overlay_enabled {
                self.wire_local_cache.clear();
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    // ── SOLID (+ MatCap + Lit secondaries) ──────────────────────────────────
    /// Solid HDR exposure + the MatCap sub-group + Lit exposure. Solid, MatCap
    /// and Lit are the three "flat form-shading" siblings (Solid button hosts
    /// all three).
    pub(crate) fn render_solid_settings_body(&mut self, ui: &mut egui::Ui) {
        if self.env_hdr_width > 0 {
            if ui
                .add(
                    egui::Slider::new(&mut self.render.exposure, 0.01..=5.0)
                        .text("Exposure")
                        .clamping(egui::SliderClamping::Never),
                )
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            ui.separator();
        }
        // MatCap sub-group (secondary mode reachable from the Solid button; the
        // shading-bar's own MatCap button uses the same body directly).
        ui.label(egui::RichText::new("MatCap").strong().size(11.0));
        self.render_matcap_settings_body(ui);
    }

    // ── MATCAP ──────────────────────────────────────────────────────────────
    /// The MatCap material-colour overlay control (toggle + amount). ONE definition,
    /// called by the v2 MatCap popover/panel AND by both v1 MatCap blocks (the
    /// properties Render page and the viewport-header shading popup) — those two are
    /// verbatim copies of the v2 body, and the option existing in one UI but not the
    /// other is exactly the "WHERE IS MY MATCAP BG" failure (owner 2026-08-06).
    pub(crate) fn matcap_material_tint_ui(&mut self, ui: &mut egui::Ui) {
        if ui
            .checkbox(&mut self.render.matcap_material_tint, "Material Color")
.default_menu("render.matcap_material_tint", serde_json::json!(self.render.matcap_material_tint))
            .on_hover_text(
                "Overlay each surface's own material base colour on the matcap\n\
                 (matcap × base colour — albedo map, noise and curvature layers included).\n\
                 Bodies with no material use the default warm gray.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.matcap_material_tint
            && ui
                .add(
                    egui::Slider::new(&mut self.render.matcap_material_tint_strength, 0.0..=1.0)
                        .text("Amount"),
                )
                .on_hover_text("0 = pure matcap, 1 = full material colour.")
                .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    /// Unlit baked-sphere MatCap: background (solid/gradient), rotation, and the
    /// optional edge-line pass. Owns its own shading-bar button (owner 2026-07-26).
    /// Shared by the Solid page (as a sub-group) and the MatCap popover/panel.
    pub(crate) fn render_matcap_settings_body(&mut self, ui: &mut egui::Ui) {
        // Flat viewport background (default = UI panel dark gray). Gradient is opt-in.
        {
            let mut col32 = egui::Color32::from_rgb(
                (self.render.matcap_bg_color[0] * 255.0) as u8,
                (self.render.matcap_bg_color[1] * 255.0) as u8,
                (self.render.matcap_bg_color[2] * 255.0) as u8,
            );
            ui.horizontal(|ui| {
                ui.label(if self.render.matcap_bg_gradient {
                    "BG Bottom"
                } else {
                    "BG Color"
                });
                let swatch = ui.color_edit_button_srgba(&mut col32)
                    .default_menu("render.matcap_bg_color", serde_json::json!(self.render.matcap_bg_color));
                if swatch.changed() {
                    self.render.matcap_bg_color = [
                        col32.r() as f32 / 255.0,
                        col32.g() as f32 / 255.0,
                        col32.b() as f32 / 255.0,
                    ];
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if !self.render.matcap_bg_gradient
                    && ui
                        .small_button("UI gray")
                        .on_hover_text("Reset to panel dark gray (0x1c1c1c)")
                        .clicked()
                {
                    // Match ui_theme::SURFACE_PANEL
                    self.render.matcap_bg_color = [0.1098, 0.1098, 0.1098];
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
        }
        if ui
            .checkbox(&mut self.render.matcap_bg_gradient, "Gradient BG")
.default_menu("render.matcap_bg_gradient", serde_json::json!(self.render.matcap_bg_gradient))
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.matcap_bg_gradient {
            let mut top32 = egui::Color32::from_rgb(
                (self.render.matcap_bg_gradient_top[0] * 255.0) as u8,
                (self.render.matcap_bg_gradient_top[1] * 255.0) as u8,
                (self.render.matcap_bg_gradient_top[2] * 255.0) as u8,
            );
            ui.horizontal(|ui| {
                ui.label("BG Top");
                if ui.color_edit_button_srgba(&mut top32).changed() {
                    self.render.matcap_bg_gradient_top = [
                        top32.r() as f32 / 255.0,
                        top32.g() as f32 / 255.0,
                        top32.b() as f32 / 255.0,
                    ];
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
        }
        if ui
            .add(egui::Slider::new(&mut self.render.matcap_rotation, -180.0..=180.0).text("Rotation"))
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        self.matcap_material_tint_ui(ui);
        if ui
            .checkbox(&mut self.render.matcap_edge_enabled, "Edge Lines")
.default_menu("render.matcap_edge_enabled", serde_json::json!(self.render.matcap_edge_enabled))
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.matcap_edge_enabled {
            {
                let mut ec32 = egui::Color32::from_rgb(
                    (self.render.matcap_edge_color[0] * 255.0) as u8,
                    (self.render.matcap_edge_color[1] * 255.0) as u8,
                    (self.render.matcap_edge_color[2] * 255.0) as u8,
                );
                ui.horizontal(|ui| {
                    ui.label("Edge Color");
                    if ui.color_edit_button_srgba(&mut ec32).changed() {
                        self.render.matcap_edge_color = [
                            ec32.r() as f32 / 255.0,
                            ec32.g() as f32 / 255.0,
                            ec32.b() as f32 / 255.0,
                        ];
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                });
            }
            if ui
                .add(
                    egui::Slider::new(&mut self.render.matcap_edge_thickness, 1.0..=3.0)
                        .text("Thickness")
                        .step_by(1.0),
                )
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .add(
                    egui::Slider::new(&mut self.render.matcap_edge_threshold, 0.05..=1.0)
                        .text("Threshold"),
                )
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
    }

    // ── MATERIAL / ENV-IBL ──────────────────────────────────────────────────
    /// Environment / IBL lookdev: enable mute, Load HDRI, intensity, rotation,
    /// contrast, saturation, **background blur** + **reflection blur**. Shared by
    /// the HDR shading-bar button, Material, PBR, and the EnvSettings dock panel.
    /// Exposure is NOT here — it belongs to the tonemap chain.
    pub(crate) fn render_env_ibl_body(&mut self, ui: &mut egui::Ui) {
        let mut env_on = self.render.env_enabled;
        let enable_resp = ui
            .checkbox(&mut env_on, "HDR enabled")
.default_menu("render.env_enabled", serde_json::json!(self.render.env_enabled))
            .on_hover_text(
                "Mute the environment without unloading it.\n\
                 Same as left-clicking the HDR button on the shading bar.",
            );
        if enable_resp.changed() {
            self.render.env_enabled = env_on;
            self.mark_dirty(DirtyFlags::RENDER);
        }

        ui.horizontal(|ui| {
            // Deferred dialog (file_dialog.rs — rfd modal-in-frame crash-class fix):
            // picker + decode run on a worker; install lands in poll.
            if ui
                .button("Load HDRI…")
                .on_hover_text(
                    "Load an .hdr/.exr equirect environment.\nDrives the background AND \
                     image-based lighting (metals reflect it).",
                )
                .clicked()
            {
                self.request_env_file_dialog();
            }
            if self.env_hdr_width > 0 {
                ui.label(format!("{}×{}", self.env_hdr_width, self.env_hdr_height));
                if ui.small_button("Clear").clicked() {
                    // GPU buffer swap happens in poll (Frame Ordering Contract).
                    self.env_load_gen = self.env_load_gen.wrapping_add(1);
                    self.env_clear_pending = true;
                }
            }
        });
        if self.env_hdr_width > 0 {
            let muted = !self.render.env_enabled;
            if muted {
                ui.weak("HDR muted — left-click the bar button or tick “HDR enabled” to restore.");
            }
            ui.add_enabled_ui(!muted, |ui| {
                // Camera visibility of the HDRI IMAGE only — NOT a mute. Unticking gives
                // the flat viewport background colour while IBL fill + metal reflections
                // keep reading the map (Cycles' World → Ray Visibility → Camera).
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.render.env_bg_visible, "Show HDRI in background")
.default_menu("render.env_bg_visible", serde_json::json!(self.render.env_bg_visible))
                        .on_hover_text(
                            "Untick to show a flat background colour instead of the HDRI image.\n\
                             Lighting is UNCHANGED: diffuse fill, metal reflections, SSR and \
                             path-traced bounces still use the HDRI — dark flat plate, real metal.\n\
                             To drop the lighting too, untick “HDR enabled” instead.",
                        )
                        .changed()
                    {
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    // The plate colour lives next to its toggle, not in a distant Background
                    // section — it only has meaning while the HDRI is hidden.
                    ui.add_enabled_ui(!self.render.env_bg_visible, |ui| {
                        if ui
                            .color_edit_button_rgb(&mut self.render.env_bg_color)
                            .on_hover_text("Flat background colour shown in place of the HDRI")
                            .changed()
                        {
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    });
                });
                if ui
                    .add(egui::Slider::new(&mut self.render.env_brightness, 0.0..=3.0).text("Intensity"))
                    .on_hover_text(
                        "HDR / IBL intensity — scales sky + diffuse fill + specular reflections.\n\
                         Does not scale the Sun/key light (set that light’s Intensity separately).\n\
                         On metals (Metalness ≈ 1) you mostly see reflection change — dielectrics also get env fill lighting.",
                    )
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .add(
                        egui::Slider::new(&mut self.render.env_rotation_deg, 0.0..=360.0)
                            .text("Rotation")
                            .suffix("°"),
                    )
                    .on_hover_text("Rotate the environment around the vertical axis — background and reflections turn together")
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .add(egui::Slider::new(&mut self.render.env_contrast, 0.0..=3.0).text("Contrast"))
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .add(egui::Slider::new(&mut self.render.env_saturation, 0.0..=3.0).text("Saturation"))
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // Dual blur: visible sky vs IBL specular reflections are independent.
                if ui
                    .add(
                        egui::Slider::new(&mut self.render.env_blur, 0.0..=1.0)
                            .text("Background blur"),
                    )
                    .on_hover_text(
                        "Blur the visible sky / background only.\n\
                         Does not soften metal reflections — use Reflection blur for that.",
                    )
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .add(
                        egui::Slider::new(&mut self.render.env_reflection_blur, 0.0..=1.0)
                            .text("Reflection blur"),
                    )
                    .on_hover_text(
                        "Extra soft-focus on IBL specular reflections only.\n\
                         0 = roughness-driven sharp reflections (real chrome).\n\
                         Raise to soften reflections without touching the sky.",
                    )
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
        } else {
            ui.weak("Load an HDRI to unlock IBL controls.");
        }
    }

    // ── PBR POST-FX ─────────────────────────────────────────────────────────
    /// Tonemapper + bloom + SSR + SSAO/GTAO + sun shadows + RT shadows + fog +
    /// AA. The full deferred post chain. `env` controls live in the Material page.
    pub(crate) fn render_pbr_postfx_body(&mut self, ui: &mut egui::Ui) {
        // Viewport tonemap curve — applies after exposure + bloom/fog composite,
        // before gamma (pass_tonemap `viewport_tonemap`). Runtime-only.
        const TM_NAMES: [&str; 6] = [
            "ACES (Classic)",
            "AgX",
            "Filmic (Hable)",
            "Reinhard",
            "PBR Neutral",
            "None (Linear)",
        ];
        let tm_idx = (self.render.tonemapper as usize).min(TM_NAMES.len() - 1);
        let tm_resp = egui::ComboBox::from_label("Tonemapper")
            .selected_text(TM_NAMES[tm_idx])
            .show_ui(ui, |ui| {
                let mut changed = false;
                for (i, name) in TM_NAMES.iter().enumerate() {
                    changed |= ui
                        .selectable_value(&mut self.render.tonemapper, i as u32, *name)
                        .changed();
                }
                changed
            });
        if tm_resp.inner.unwrap_or(false) {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        tm_resp.response.on_hover_text(
            "HDR\u{2192}LDR display curve. ACES (Classic) = the existing default look. AgX keeps \
             hue under bright saturated light; Filmic = Uncharted 2 shoulder; None = raw clamp for debugging.",
        );
        ui.separator();
        if ui.checkbox(&mut self.render.bloom_enabled, "Bloom")
.default_menu("render.bloom_enabled", serde_json::json!(self.render.bloom_enabled)).changed() {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.bloom_enabled {
            if ui
                .add(egui::Slider::new(&mut self.render.bloom_threshold, 0.0..=3.0).text("Threshold"))
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .add(egui::Slider::new(&mut self.render.bloom_intensity, 0.0..=2.0).text("Intensity"))
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if ui
            .checkbox(&mut self.render.ssr_enabled, "SSR")
.default_menu("render.ssr_enabled", serde_json::json!(self.render.ssr_enabled))
            .on_hover_text(
                "Screen-space reflections: crisp traced contact reflections that blur with \
                 roughness and hand off to IBL at screen edges / high roughness. Paused during \
                 camera interaction.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.ssr_enabled {
            if ui
                .add(egui::Slider::new(&mut self.render.ssr_max_steps, 16..=96).text("SSR Quality"))
                .on_hover_text("March step budget (screen-space DDA steps). Higher = longer/denser reflections, more GPU.")
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .add(egui::Slider::new(&mut self.render.ssr_intensity, 0.0..=1.0).text("SSR Intensity"))
                .on_hover_text("0 = pure IBL specular, 1 = full traced-reflection swap by hit confidence.")
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if ui
            .checkbox(&mut self.render.ssao_enabled, "Ambient Occlusion")
.default_menu("render.ssao_enabled", serde_json::json!(self.render.ssao_enabled))
            .on_hover_text(
                "Contact darkening in creases and at object bases. Ambient/sky occlusion only — \
                 never dims direct light. PBR/Lit modes; Solid stays pure form shading.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.ssao_enabled {
            ui.horizontal(|ui| {
                ui.label("Quality:");
                if ui
                    .selectable_value(&mut self.render.use_gtao, true, "GTAO")
                    .on_hover_text("Ground Truth AO: horizon-based, directional. Adds bent normals so IBL diffuse self-shades. Slightly pricier.")
                    .clicked()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .selectable_value(&mut self.render.use_gtao, false, "SSAO")
                    .on_hover_text("Hemisphere AO: cheaper, contact darkening only, no bent normal (flatter).")
                    .clicked()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
            if ui
                .add(egui::Slider::new(&mut self.render.ssao_strength, 0.0..=1.0).text("AO Strength"))
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .add(egui::Slider::new(&mut self.render.ssao_radius, 0.1..=2.0).text("AO Radius"))
                .on_hover_text("Hemisphere radius in world units")
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        // Sun Shadows — cascaded-shadow-map quality (raster path). Panel writes
        // settings only; texture recreation happens in poll (Frame Ordering).
        ui.separator();
        ui.label(egui::RichText::new("Sun Shadows").strong());
        let shadow_res_before = self.render.shadow_map_resolution;
        egui::ComboBox::from_label("Shadow Map Resolution")
            .selected_text(format!("{0}\u{00b2}", self.render.shadow_map_resolution))
            .show_ui(ui, |ui| {
                // 8192 = "ultra" quality (owner 2026-08-06). Heavy VRAM — hover warns.
                for r in [1024u32, 2048, 4096, 8192] {
                    let label = if r >= 8192 {
                        format!("{r}\u{00b2}  (ultra)")
                    } else if r >= 4096 {
                        format!("{r}\u{00b2}  (high)")
                    } else {
                        format!("{r}\u{00b2}")
                    };
                    ui.selectable_value(&mut self.render.shadow_map_resolution, r, label);
                }
            })
            .response
            .on_hover_text(
                "Per-cascade shadow map size — higher = crisper contact edges.\n\
                 VRAM (3 cascades, D32):\n\
                 1024\u{00b2} = 12 MB · 2048\u{00b2} = 48 MB · 4096\u{00b2} = 192 MB · 8192\u{00b2} = 768 MB.\n\
                 8K is for close-up lookdev; prefer 2K/4K for general work.\n\
                 Applied next frame (poll recreates textures).",
            );
        if self.render.shadow_map_resolution != shadow_res_before {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .add(egui::Slider::new(&mut self.render.shadow_pcf_softness, 0.0..=3.0).text("Shadow Softness"))
            .on_hover_text(
                "Penumbra width in shadow texels (PCF radius).\n\
                 0 = hard 1-tap shadow (sharpest possible for the map resolution).\n\
                 0.5 = classic tight 2\u{00d7}2 · higher = softer penumbra.\n\
                 Anti-acne bias tracks this footprint.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        // ── Shadow bias tuning ────────────────────────────────────────────────────
        // Live sliders instead of hand-picked constants (owner 2026-08-06, after three
        // successive constants each shipped a visible artifact). All three ride unused
        // padding lanes of ShadowUniforms — no layout change. Defaults are neutral.
        //
        // The ordering below is the intended tuning ORDER, and the hovers say so: drop
        // Normal Offset until the boundary sits right, then clear the acne that exposes
        // with Depth/Slope Bias (which do not move the boundary).
        egui::CollapsingHeader::new("Shadow Bias (tuning)")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Tune in order: Normal Offset first (it POSITIONS the shadow), \
                         then Depth/Slope Bias to clear acne without moving it.",
                    )
                    .small()
                    .weak(),
                );
                let mut changed = false;
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.render.shadow_normal_offset, 0.0..=8.0)
                            .text("Normal Offset (texels)"),
                    )
                    .on_hover_text(
                        "Offsets the shadow lookup along the surface normal, in texels \
                         (added to Shadow Softness).\n\n\
                         This MOVES THE SHADOW BOUNDARY along the surface — it is the \
                         cause of a shadow that looks detached or in the wrong place. \
                         Lower it until the boundary sits correctly; expect acne to \
                         appear, then clear that with the two sliders below.\n\n\
                         Default 1.0. 0 = no positional offset at all.",
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.render.shadow_depth_bias, 0.0..=8.0)
                            .text("Depth Bias (texels)")
                            .fixed_decimals(2)
                            .step_by(0.05),
                    )
                    .on_hover_text(
                        "Constant bias on the compared DEPTH, in TEXELS of the cascade in \
                         use — the same unit as Normal Offset, so it means the same thing \
                         at any map resolution or scene scale.\n\n\
                         Fights acne WITHOUT moving the shadow boundary, so this is the \
                         knob to prefer. Too much detaches contact shadows \
                         (peter-panning).\n\n\
                         Default 0. Useful range is typically 1\u{2013}3 texels.",
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.render.shadow_slope_bias, 0.0..=8.0)
                            .text("Slope Bias (texels)")
                            .fixed_decimals(2)
                            .step_by(0.05),
                    )
                    .on_hover_text(
                        "Depth bias in TEXELS, scaled by tan(angle between surface and \
                         light).\n\n\
                         Targets ONLY grazing angles, where one shadow texel spans a \
                         large depth range and the comparison flips across texels — the \
                         comb-tooth terminator. Faces pointing at the light are \
                         unaffected, so this costs no contact tightness.\n\n\
                         Default 0.",
                    )
                    .changed();
                if ui
                    .button("Reset bias")
                    .on_hover_text("Back to 1.0 / 0 / 0 — the behaviour before these sliders existed.")
                    .clicked()
                {
                    self.render.shadow_normal_offset = 1.0;
                    self.render.shadow_depth_bias = 0.0;
                    self.render.shadow_slope_bias = 0.0;
                    changed = true;
                }
                if changed {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
        // Ray-traced projected shadows for point/spot lights (LIVE WGSL RT-shadow
        // path — NOT the cuda Raytrace mode). Master toggle: OFF by default (viewport perf).
        if ui
            .checkbox(&mut self.render.rt_shadows, "Ray-Traced Shadows (point/spot)")
.default_menu("render.rt_shadows", serde_json::json!(self.render.rt_shadows))
            .on_hover_text(
                "Point & spot lights cast ray-traced projected shadows against ALL geometry \
                 (poly, voxel, imported, NURBS — not just SDF), using the path-tracer BVH. Sun \
                 stays on cascaded shadow maps unless the light sets Source Radius > 0.\n\n\
                 OFF by default — keeps the trace accel resident while ON (viewport cost).",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.rt_shadows {
            ui.indent("rt_shadow_soft", |ui| {
                if ui
                    .add(egui::Slider::new(&mut self.render.rt_shadow_softness, 0.0..=1.0).text("Softness"))
                    .on_hover_text(
                        "Angular radius of the light as seen from the surface.\n\
                         0 = hard contact shadow · higher = wider penumbra.",
                    )
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                let mut s = self.render.rt_shadow_ray_samples as i32;
                if ui
                    .add(egui::Slider::new(&mut s, 1..=16).text("Samples"))
                    .on_hover_text(
                        "Shadow rays over the light disc. 1 = hard.\n\
                         Higher = smoother penumbra (less noise).\n\
                         For clean edges, also enable Full-res Soft below.",
                    )
                    .changed()
                {
                    self.render.rt_shadow_ray_samples = s as u32;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // Quality vs perf: half-res soft is ~4× cheaper but stairs the penumbra.
                // Full-res is opt-in so Samples actually improves the edge.
                if ui
                    .checkbox(
                        &mut self.render.rt_shadow_full_res,
                        "Full-res Soft (quality)",
                    )
.default_menu("render.rt_shadow_full_res", serde_json::json!(self.render.rt_shadow_full_res))
                    .on_hover_text(
                        "OFF (default): soft shadows trace at half resolution (~4× cheaper) — \
                         penumbra can look stepped; Samples mainly fights noise.\n\
                         ON: one disc sample set per pixel — Samples cleanly improves edge quality. \
                         Higher viewport cost.",
                    )
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
        }
        // Fog (deferred height fog; PT has its own copy in the PT page).
        ui.separator();
        if ui
            .checkbox(&mut self.render.fog_enabled, "Fog")
.default_menu("render.fog_enabled", serde_json::json!(self.render.fog_enabled))
            .on_hover_text(
                "Height fog. Orbit = analytic Lite (no pop). Draft = Lite; Low–Med = half-res \
                 march; High/Ultra = full-res.\nHaze alone is cheap. God Rays + Shadow Steps + Noise cost extra.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.fog_enabled {
            // Unbounded sliders: type any value; soft range remaps to include it.
            ui.horizontal(|ui| {
                ui.label("Color");
                if ui.color_edit_button_rgb(&mut self.render.fog_color).changed() {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                ui.weak("haze / ambient tint");
            });
            if crate::ui_fields::unbound_slider(ui, "Density", &mut self.render.fog_density, 0.0, 0.2)
                .on_hover_text("Optical density. Type any number — slider range expands.")
                .changed()
            {
                self.render.fog_density = self.render.fog_density.max(0.0);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(
                ui,
                "Height Falloff",
                &mut self.render.fog_height_falloff,
                0.0,
                2.0,
            )
            .changed()
            {
                self.render.fog_height_falloff = self.render.fog_height_falloff.max(0.0);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(ui, "God Rays", &mut self.render.fog_scatter, 0.0, 2.0)
                .on_hover_text("Sun scatter in the haze. Soft tint is free. Full shafts need Shadow Steps > 0.")
                .changed()
            {
                self.render.fog_scatter = self.render.fog_scatter.max(0.0);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(
                ui,
                "Phase Asymmetry",
                &mut self.render.fog_phase_g,
                0.0,
                0.95,
            )
            .on_hover_text("Mie phase g. 0 = isotropic, high = forward scatter. Values above 0.95 allowed.")
            .changed()
            {
                self.render.fog_phase_g = self.render.fog_phase_g.clamp(-0.99, 0.99);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(
                ui,
                "Absorption",
                &mut self.render.fog_absorption,
                0.0,
                2.0,
            )
            .changed()
            {
                self.render.fog_absorption = self.render.fog_absorption.max(0.0);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(
                ui,
                "Noise Strength",
                &mut self.render.fog_noise_strength,
                0.0,
                1.0,
            )
            .on_hover_text("0 = free (default). >0 enables 3D FBM — expensive.")
            .changed()
            {
                self.render.fog_noise_strength = self.render.fog_noise_strength.max(0.0);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if self.render.fog_noise_strength > 0.01
                && crate::ui_fields::unbound_slider(
                    ui,
                    "Noise Scale",
                    &mut self.render.fog_noise_scale,
                    0.5,
                    10.0,
                )
                .changed()
            {
                self.render.fog_noise_scale = self.render.fog_noise_scale.max(0.01);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(
                ui,
                "Max Distance",
                &mut self.render.fog_max_distance,
                5.0,
                100.0,
            )
            .changed()
            {
                self.render.fog_max_distance = self.render.fog_max_distance.max(0.1);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            let mut shadow_i = self.render.fog_shadow_steps as i32;
            if ui
                .add(egui::Slider::new(&mut shadow_i, 0..=32).clamping(egui::SliderClamping::Never).text("Shadow Steps"))
                .on_hover_text("0 = free (default). Enables god-ray occlusion with God Rays > 0. Type any integer ≥ 0.")
                .changed()
            {
                self.render.fog_shadow_steps = shadow_i.max(0) as f32;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(
                ui,
                "Temporal Blend",
                &mut self.render.fog_temporal_blend,
                0.0,
                0.98,
            )
            .changed()
            {
                self.render.fog_temporal_blend = self.render.fog_temporal_blend.clamp(0.0, 0.999);
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        // Anti-aliasing — always available regardless of shading mode.
        ui.separator();
        if ui
            .checkbox(&mut self.render.sdf_edge_aa_enabled, "SDF Edge AA")
.default_menu("render.sdf_edge_aa_enabled", serde_json::json!(self.render.sdf_edge_aa_enabled))
            .on_hover_text("Silhouette anti-aliasing for SDF surfaces")
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .checkbox(&mut self.render.fxaa_enabled, "FXAA")
.default_menu("render.fxaa_enabled", serde_json::json!(self.render.fxaa_enabled))
            .on_hover_text("Fast approximate anti-aliasing (post-process)")
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .checkbox(&mut self.render.wire_aa_enabled, "Wireframe AA")
.default_menu("render.wire_aa_enabled", serde_json::json!(self.render.wire_aa_enabled))
            .on_hover_text(
                "Smooth anti-aliased overlay wireframes (edit/object wire, modifier wires). \
                 Disable to save a little GPU work on low-end machines.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .checkbox(&mut self.render.physical_light_falloff, "Physical Light Falloff")
.default_menu("render.physical_light_falloff", serde_json::json!(self.render.physical_light_falloff))
            .on_hover_text(
                "Point/spot lights attenuate with true inverse-square distance (hot \
                 near-field like a real bulb). Off = the legacy soft artistic falloff.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .checkbox(&mut self.render.pbr_progressive_enabled, "Progressive Refine")
.default_menu("render.pbr_progressive_enabled", serde_json::json!(self.render.pbr_progressive_enabled))
            .on_hover_text(
                "PBR/Lit only: while the camera is idle, keep rendering jittered frames \
                 and average them — edges and shading converge toward supersampled \
                 quality over time (like the path tracer's progressive mode, on raster \
                 tech). Zero cost while interacting; leave off on low-end machines to \
                 also skip the idle GPU work (and free refine-only VRAM — Phase E1).",
            )
            .changed()
        {
            self.render.pbr_refine_frames = 0;
            self.render.pbr_refine_sig = 0;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.pbr_progressive_enabled {
            // E2: Idle Quality Low/High + resolution_scale escape hatch in the same group.
            ui.horizontal(|ui| {
                ui.label("Idle Quality");
                let high = self.render.idle_quality_high;
                if ui
                    .selectable_label(!high, "Low")
                    .on_hover_text(
                        "Cheaper refine: no SSGI/SSR idle doubles. Prefer on low-end GPUs \
                         so long idle sessions stay interactive.",
                    )
                    .clicked()
                {
                    self.render.idle_quality_high = false;
                    self.render.pbr_refine_frames = 0;
                    self.render.pbr_refine_sig = 0;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .selectable_label(high, "High")
                    .on_hover_text(
                        "Full idle boosts: 2× SSGI rays, 2× SSR steps while refining \
                         (current default).",
                    )
                    .clicked()
                {
                    self.render.idle_quality_high = true;
                    self.render.pbr_refine_frames = 0;
                    self.render.pbr_refine_sig = 0;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
            if ui
                .add(
                    egui::Slider::new(&mut self.render.resolution_scale, 0.25..=1.0)
                        .text("Resolution scale")
                        .suffix("×"),
                )
                .on_hover_text(
                    "Global render resolution scale (escape hatch for low-end). \
                     1.0 = full viewport; 0.5 ≈ quarter pixels.",
                )
                .changed()
            {
                self.render.pbr_refine_frames = 0;
                self.render.pbr_refine_sig = 0;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            // Live accumulator status: frame count + WHY it last restarted — the
            // terminal-free version of AZRA_REFINE_LOG (owner "GI seems to loop
            // restart" 2026-08-07). A healthy idle session climbs steadily with
            // "settings" restarts only at the two shadow-ladder steps.
            {
                // First-run wait is EXPLAINED, not silent (owner: shadows "came after
                // a minute — shader building in bg"): refinement deliberately holds
                // until the ray pipelines/TLAS are resident so no shadow-map frame
                // can contaminate the average. Cold compiles take up to ~1 min once;
                // the disk pipeline cache makes later launches near-instant.
                let rt_waiting = self.render.pbr_refine_rt_sun
                    && self.gpu.as_ref().map_or(true, |g| {
                        g.pipelines.rt_shadow_pipeline.is_none()
                            || g.buffers.pt_tlas_buffer.is_none()
                            || g.buffers.pt_blas_buffer.is_none()
                            || g.buffers.pt_tri_buffer.is_none()
                            || g.buffers.pt_vert_buffer.is_none()
                    });
                // Outside PBR/Lit the wait-pump is off (the Matcap 20 Hz spin fix) —
                // the TLAS never becomes resident there, so "preparing…" would read
                // as stuck forever. Say what's actually happening instead.
                let mode_ok = matches!(
                    self.render.shading_mode,
                    ShadingMode::Pbr | ShadingMode::Lit
                );
                if !mode_ok {
                    ui.weak("refine: inactive in this shading mode (PBR / Lit only)");
                } else if rt_waiting {
                    ui.weak(
                        "refine: preparing ray pipelines… (first run compiles shaders; \
                         shadows/GI/reflections start when ready)",
                    );
                } else {
                    let reason = match self.render.pbr_refine_last_reset {
                        1 => "camera",
                        2 => "scene content",
                        3 => "settings",
                        4 => "interaction",
                        _ => "—",
                    };
                    // WIDTH-STABLE (owner "the refinement number makes the UI
                    // glitch"): the live count changed the label's width every
                    // frame, toggling the line between 1 and 2 wrapped rows —
                    // the whole panel reflowed at 60 Hz. Display ≥1000 as a
                    // rounded "1.5k" (changes every ~100 frames) and keep the
                    // line short enough to never wrap; the restart reason —
                    // per-frame-stable but long — lives on hover.
                    let n = self.render.pbr_refine_frames;
                    let n_txt = if n >= 1000 {
                        format!("{:.1}k", n as f32 / 1000.0)
                    } else {
                        format!("{n}")
                    };
                    ui.weak(format!("refine: {n_txt} frames"))
                        .on_hover_text(format!(
                            "Accumulated idle frames — higher = cleaner shadows, GI \
                             and reflections. Restarts whenever the image must change.\n\
                             Last restart: {reason}."
                        ));
                }
            }
            // Per-tech opt-out (owner 2026-08-07: "let the user toggle the tech he
            // wants — easily opt out of GI or RT reflection").
            if ui
                .checkbox(&mut self.render.pbr_refine_reflections, "Reflections (idle)")
.default_menu("render.pbr_refine_reflections", serde_json::json!(self.render.pbr_refine_reflections))
                .on_hover_text(
                    "Full traced screen-space reflections while idle (converged by the \
                     accumulator) — chrome picks up the actual scene, not just the HDRI. \
                     The auto probe covers what's off-screen.",
                )
                .changed()
            {
                self.render.pbr_refine_frames = 0;
                self.render.pbr_refine_sig = 0;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .checkbox(&mut self.render.pbr_refine_gi, "Global Illumination (idle)")
.default_menu("render.pbr_refine_gi", serde_json::json!(self.render.pbr_refine_gi))
                .on_hover_text(
                    "One-bounce indirect light (color bleed, filled corners), converged \
                     while idle. Off = no GI cost; reflections and shadows keep refining.",
                )
                .changed()
            {
                self.render.pbr_refine_frames = 0;
                self.render.pbr_refine_sig = 0;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if self.render.pbr_refine_gi {
                // GI strength/reach, surfaced HERE (owner "no second-bounce feel"
                // 2026-08-07 — the standalone SSGI panel was invisible). Same fields
                // as that panel — one state, two views.
                if ui
                    .add(egui::Slider::new(&mut self.render.ssgi_intensity, 0.0..=3.0)
                        .text("GI Intensity"))
                    .on_hover_text("1.0 = physically balanced bounce; higher = artistic boost.")
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .add(egui::Slider::new(&mut self.render.ssgi_radius, 0.5..=10.0)
                        .text("GI Radius"))
                    .on_hover_text(
                        "How far bounce light travels, in world units — must be \
                         room-scale for a lit floor patch to reach the walls.",
                    )
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
            // Phase 2c: ray-traced sun while idle — the automated thin-wall leak fix.
            if ui
                .checkbox(&mut self.render.pbr_refine_rt_sun, "Leak-Proof Sun Shadows")
.default_menu("render.pbr_refine_rt_sun", serde_json::json!(self.render.pbr_refine_rt_sun))
                .on_hover_text(
                    "While idle, sun shadows are ray-traced against the real geometry — \
                     no shadow-map bias, so light never leaks through thin walls. The \
                     automated alternative to thickening walls. Interaction keeps fast \
                     shadow maps.",
                )
                .changed()
            {
                self.render.pbr_refine_frames = 0;
                self.render.pbr_refine_sig = 0;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            // Phase 3: one scene-centered cubemap, captured through the real pipeline
            // while idle, consumed by SSR where the screen-space march can't see.
            if ui
                .checkbox(&mut self.render.auto_reflection_probe, "Auto Reflection Probe")
.default_menu("render.auto_reflection_probe", serde_json::json!(self.render.auto_reflection_probe))
                .on_hover_text(
                    "Automatically captures the scene into a reflection cubemap while \
                     idle — objects reflect their actual surroundings (room, floor, \
                     props) instead of only the environment. Nothing to place by hand; \
                     works with or without SSR (SSR adds sharper on-screen detail).",
                )
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            // Probe face resolution. This is the SHARPNESS CEILING of every reflection
            // the probe supplies, and it is deliberately NOT affected by "Reflection
            // blur" (that slider is an HDRI mip bias). At the original hardcoded 128 the
            // owner read reflections as permanently blurry with blur at 0 — hence both
            // the control and the hover text saying which knob owns what.
            if self.render.auto_reflection_probe {
                ui.horizontal(|ui| {
                    ui.label("Probe Res");
                    for v in crate::probe_bake::PROBE_FACE_SIZES {
                        let vram = match v {
                            128 => "1 MB",
                            256 => "4 MB",
                            _ => "17 MB",
                        };
                        if ui
                            .selectable_label(self.render.probe_face_size == v, format!("{v}"))
                            .on_hover_text(format!(
                                "{v}\u{00b2} per cube face \u{2014} {vram} VRAM.\n\n\
                                 Sets how SHARP probe reflections can be. \"Reflection \
                                 blur\" does not affect this path, so if reflections look \
                                 soft with that at 0, this is the control you want.\n\n\
                                 Rebakes over the next 6 idle frames."
                            ))
                            .clicked()
                        {
                            self.render.probe_face_size = v;
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    }
                });
                // Live bake status (owner "still no reflections" 2026-08-07): the
                // console [PROBE] line, surfaced in the panel so "did it bake?" never
                // needs a terminal.
                if let Some(gpu) = self.gpu.as_ref() {
                    let status = if gpu.probe_baked {
                        format!(
                            "probe: baked ✓  center ({:.1}, {:.1}, {:.1})",
                            gpu.probe_center[0], gpu.probe_center[1], gpu.probe_center[2]
                        )
                    } else if gpu.probe_rejected {
                        format!(
                            "probe: off — centre ({:.1}, {:.1}, {:.1}) is inside geometry \
                             (free {:.0}%), using the HDRI",
                            gpu.probe_center[0], gpu.probe_center[1], gpu.probe_center[2],
                            gpu.probe_free_fraction * 100.0
                        )
                    } else if gpu.probe_next_face < 6 {
                        format!("probe: baking face {}/6…", gpu.probe_next_face + 1)
                    } else if !self.render.pbr_refine_active {
                        "probe: waiting for idle (let the camera rest)".into()
                    } else {
                        "probe: waiting for first bake…".into()
                    };
                    ui.weak(status);
                }
                // Placement debug view (owner 2026-08-07): the capture point + the
                // parallax proxy box, X-ray cyan wire.
                if ui
                    .checkbox(&mut self.viewport.show_probe_gizmo, "Show Probes")
.default_menu("viewport.show_probe_gizmo", serde_json::json!(self.viewport.show_probe_gizmo))
                    .on_hover_text(
                        "Visualize BOTH probe types in the viewport:\n\
                         • proxy box + capture point (cyan, X-ray) — reflections \
                         are parallax-corrected against this box\n\
                         • CHROME ball at the center — the raw captured reflection \
                         cube (what glossy surfaces receive)\n\
                         • MATTE ball above it — the light probe's SH lighting \
                         (what the ambient receives)\n\
                         Gray balls = not baked yet (let the camera rest).",
                    )
                    .changed()
                {
                    self.overlay_lines_fp = u64::MAX; // rebuild the overlay line batch
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // Phase 3b: the same auto-placed bake doubles as a LIGHT probe —
                // SH9 irradiance feeding the diffuse ambient (real bounce light).
                if ui
                    .checkbox(&mut self.render.pbr_refine_light_probe, "Auto Light Probe")
.default_menu("render.pbr_refine_light_probe", serde_json::json!(self.render.pbr_refine_light_probe))
                    .on_hover_text(
                        "The auto probe also acts as a LIGHT probe: real bounce \
                         lighting (sunlit floors tint the walls, colored rooms tint \
                         props) replaces the flat gray ambient. With an HDRI loaded \
                         it corrects the sky ambient for walls that block it. Same \
                         automatic placement — nothing to author.",
                    )
                    .changed()
                {
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if self.render.pbr_refine_light_probe {
                    ui.horizontal(|ui| {
                        ui.label("Bounce Strength");
                        if ui
                            .add(egui::Slider::new(&mut self.render.light_probe_strength, 0.0..=1.0))
                            .on_hover_text(
                                "How much the probe's captured lighting drives the \
                                 ambient. 1 = full probe lighting; 0 = classic flat \
                                 ambient / HDRI-only.",
                            )
                            .changed()
                        {
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    });
                }
            }
            // Phase 2b: sun angular half-radius while refining — each idle frame samples
            // one hard CSM shadow from a jittered direction, so the average converges to
            // the physical area-light penumbra (accurate contact hardening).
            if ui
                .add(
                    egui::Slider::new(&mut self.render.pbr_refine_sun_angle_deg, 0.0..=30.0)
                        .text("Sun Softness (idle)")
                        .suffix("°"),
                )
                .on_hover_text(
                    "Sun size for refined shadows: penumbra comes from real geometry — \
                     wide far from contact, sharp at contact. 0 = perfectly hard, \
                     real sun ≈ 0.5°, overcast/studio softness 10–30°.",
                )
                .changed()
            {
                self.render.pbr_refine_frames = 0;
                self.render.pbr_refine_sig = 0;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            // Phase 2 / E2: ceiling for the idle shadow-resolution ladder (8192 tier
            // dropped — 4096 + cone-jitter is enough once Leak-Proof RT sun owns refine).
            ui.horizontal(|ui| {
                ui.label("Idle shadow cap");
                for v in [2048u32, 4096] {
                    if ui
                        .selectable_label(
                            self.render.pbr_refine_shadow_res_cap == v,
                            format!("{v}"),
                        )
                        .on_hover_text("How high the idle refinement may raise shadow maps")
                        .clicked()
                    {
                        self.render.pbr_refine_shadow_res_cap = v.min(4096);
                        self.render.pbr_refine_frames = 0;
                        self.render.pbr_refine_sig = 0;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
            });
            // Cap is a CEILING on the idle ladder, not the interactive shadow size.
            // Ladder tops at base<<2; mirror app_poll so the panel never implies a
            // resolution the engine will not allocate.
            let base = self.render.shadow_map_resolution;
            let ladder_top = (base << 2).min(self.render.pbr_refine_shadow_res_cap.max(base));
            ui.label(
                egui::RichText::new(format!(
                    "→ actual: {base}\u{00b2} moving · {ladder_top}\u{00b2} idle",
                ))
                .small()
                .weak(),
            )
            .on_hover_text(
                "The cap only limits the idle ladder, which stops at 4\u{00d7} the base.\n\
                 To raise the SHARPNESS of the shadow itself, raise \"Shadow Map \
                 Resolution\" above \u{2014} the cap cannot do it.",
            );
        }
    }

    // ── PATH TRACE ──────────────────────────────────────────────────────────
    /// The full Path-Trace settings surface — status strip, quality presets,
    /// progressive refine, live-SDF-feedback, atmosphere, DOF, denoise, display.
    /// This is the factored body of the former floating PT window; the v1 window
    /// and the ui2 PtSettings panel both call it (single source, no dup).
    pub(crate) fn render_pt_settings_body(&mut self, ui: &mut egui::Ui) {
        // Progressive status.
        let total_spp = self
            .render
            .pt_accumulated_frames
            .saturating_mul(self.render.pt_samples_per_frame.max(1));
        let mode_lbl = match self.render.pt_stop_mode {
            0 => "Infinite",
            1 => "Until noise",
            _ => "Max frames",
        };
        ui.horizontal(|ui| {
            if self.render.pt_converged {
                ui.colored_label(egui::Color32::from_rgb(100, 220, 140), "Stopped");
            } else {
                ui.colored_label(egui::Color32::from_rgb(180, 140, 255), "Refining\u{2026}");
            }
            ui.label(format!(
                "{mode_lbl}  ·  ~{total_spp} spp  ·  {} frames",
                self.render.pt_accumulated_frames
            ));
            if ui.small_button("Reset").clicked() {
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if self.render.pt_converged
                && ui
                    .small_button("Keep going")
                    .on_hover_text("Resume progressive refine (switch to Infinite if needed)")
                    .clicked()
            {
                self.render.pt_converged = false;
                if self.render.pt_stop_mode != 0 {
                    self.render.pt_max_accum_frames = self
                        .render
                        .pt_max_accum_frames
                        .saturating_mul(2)
                        .max(4096)
                        .min(131_072);
                    self.render.pt_noise_threshold =
                        (self.render.pt_noise_threshold * 0.5).max(0.002);
                }
                self.mark_dirty(DirtyFlags::RENDER);
            }
        });
        ui.label(
            egui::RichText::new("Camera move → reset. Camera still → keep shooting light frames.")
                .small()
                .color(egui::Color32::from_gray(140)),
        );
        ui.separator();

        self.draw_pt_quality_presets(ui);
        ui.separator();

        ui.label(egui::RichText::new("Progressive refine").strong().size(11.0));
        let noisy_frac = if let Some(gpu) = self.gpu.as_ref() {
            f32::from_bits(
                gpu.buffers
                    .pt_noisy_fraction_bits
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        } else {
            1.0
        };
        if self.render.pt_stop_mode == 1 {
            ui.label(
                egui::RichText::new(format!(
                    "Noisy pixels: {:.1}%  (stop ≤ {:.1}%)",
                    noisy_frac * 100.0,
                    self.render.pt_noise_threshold * 100.0
                ))
                .small()
                .color(if self.render.pt_converged {
                    egui::Color32::from_rgb(100, 220, 140)
                } else {
                    egui::Color32::from_gray(160)
                }),
            );
            if ui
                .add(
                    egui::Slider::new(&mut self.render.pt_noise_threshold, 0.002..=0.25)
                        .text("Noise Threshold")
                        .logarithmic(true),
                )
                .on_hover_text("Lower = cleaner before stop. Only used in Noise threshold mode.")
                .changed()
            {
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if (self.render.pt_stop_mode == 1 || self.render.pt_stop_mode == 2)
            && ui
                .add(
                    egui::Slider::new(&mut self.render.pt_max_accum_frames, 64..=131_072)
                        .text(if self.render.pt_stop_mode == 1 {
                            "Safety Max Frames"
                        } else {
                            "Max Frames"
                        })
                        .logarithmic(true),
                )
                .on_hover_text("Mode Max frames: hard stop. Mode Noise: safety ceiling only.")
                .changed()
        {
            self.render.pt_converged = false;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .add(egui::Slider::new(&mut self.render.pt_max_bounces, 1..=32).text("Max Bounces"))
            .changed()
        {
            self.render.pt_accumulated_frames = 0;
            self.render.pt_converged = false;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.draw_pt_samples_per_frame_control(ui) {
            self.render.pt_accumulated_frames = 0;
            self.render.pt_converged = false;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .checkbox(&mut self.render.pt_adaptive_spp, "1 spp while orbiting")
.default_menu("render.pt_adaptive_spp", serde_json::json!(self.render.pt_adaptive_spp))
            .on_hover_text(
                "While the camera moves: always 1 sample/frame (never freezes). When still: use \
                 Samples/Frame and keep progressive refine running.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .add(
                egui::Slider::new(&mut self.render.pt_clamp, 1.0..=100.0)
                    .text("Firefly Clamp")
                    .logarithmic(true),
            )
            .changed()
        {
            self.render.pt_accumulated_frames = 0;
            self.render.pt_converged = false;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        ui.separator();

        // Live SDF feedback while PathTrace is the shading mode.
        ui.label(egui::RichText::new("Live SDF Feedback").strong().size(11.0));
        ui.label(
            egui::RichText::new(
                "While reshaping SDF bodies under Path Trace — turn either/both off for classic settle-only PT.",
            )
            .small()
            .weak(),
        );
        if ui
            .checkbox(&mut self.render.pt_live_hybrid_edit, "A · Hybrid while editing")
.default_menu("render.pt_live_hybrid_edit", serde_json::json!(self.render.pt_live_hybrid_edit))
            .on_hover_text(
                "ON: during an SDF edit, show Hybrid/raster (live bake mesh) instead of frozen PT. \
                 On settle, Path Trace resumes.\nOFF: stay in Path Trace (shape updates only after mesh settles).",
            )
            .changed()
        {
            self.render.pt_was_live_hybrid = false;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .checkbox(&mut self.render.pt_live_draft_mesh, "B · Draft mesh into PT")
.default_menu("render.pt_live_draft_mesh", serde_json::json!(self.render.pt_live_draft_mesh))
            .on_hover_text(
                "ON: active SDF body pushes mesh into the path tracer sooner (shorter settle).\n\
                 OFF: full quiet + 300 ms settle before PT mesh update.",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.pt_live_draft_mesh {
            let mut ms = self.render.pt_live_draft_settle_ms as f32;
            if ui
                .add(
                    egui::Slider::new(&mut ms, 16.0..=300.0)
                        .text("Draft settle (ms)")
                        .step_by(1.0),
                )
                .on_hover_text("How long the active body's field hash must hold still before a PT mesh snapshot.")
                .changed()
            {
                self.render.pt_live_draft_settle_ms = ms as u32;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if ui
            .checkbox(&mut self.render.pt_soft_hybrid_resume, "C · Soft Hybrid → PT resume")
.default_menu("render.pt_soft_hybrid_resume", serde_json::json!(self.render.pt_soft_hybrid_resume))
            .on_hover_text(
                "ON: when Hybrid feedback ends, seed PT accumulation from the last Hybrid image (no black flash).\n\
                 OFF: hard restart (frames = 0) on Hybrid exit.",
            )
            .changed()
        {
            self.render.pt_pending_soft_hybrid_seed = false;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.pt_soft_hybrid_resume {
            let mut f = self.render.pt_soft_hybrid_frames as f32;
            if ui
                .add(
                    egui::Slider::new(&mut f, 1.0..=16.0)
                        .text("Hybrid seed frames")
                        .step_by(1.0),
                )
                .on_hover_text("Progressive weight after Hybrid exit. Higher holds Hybrid color longer before PT takes over.")
                .changed()
            {
                self.render.pt_soft_hybrid_frames = f as u32;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if ui
            .checkbox(&mut self.render.pt_draft_soft_reset, "D · Soft reset on draft mesh")
.default_menu("render.pt_draft_soft_reset", serde_json::json!(self.render.pt_draft_soft_reset))
            .on_hover_text(
                "ON: mesh/TLAS updates keep a short progressive history; variance-aware blend collapses ghosts.\n\
                 OFF: frames = 0 on every scene mesh change (classic).",
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.pt_draft_soft_reset {
            let mut f = self.render.pt_draft_soft_frames as f32;
            if ui
                .add(
                    egui::Slider::new(&mut f, 1.0..=8.0)
                        .text("Draft soft frames")
                        .step_by(1.0),
                )
                .on_hover_text("History weight kept after draft mesh / TLAS upload. Lower = snappier, less ghost risk.")
                .changed()
            {
                self.render.pt_draft_soft_frames = f as u32;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if self.render.shading_mode == ShadingMode::PathTrace {
            let hybrid_now = self.pt_live_hybrid_feedback_active();
            let edit = self.pt_sdf_edit_episode_active();
            let status = if hybrid_now {
                "Status: Hybrid feedback (editing)"
            } else if edit && self.render.pt_live_draft_mesh {
                "Status: Path Trace + draft mesh"
            } else if edit {
                "Status: Path Trace (edit settle)"
            } else {
                "Status: Path Trace"
            };
            ui.label(
                egui::RichText::new(status).small().color(if hybrid_now {
                    egui::Color32::from_rgb(120, 200, 255)
                } else {
                    egui::Color32::from_gray(160)
                }),
            );
        }
        ui.separator();

        // Atmosphere (shares the Fog controls with the deferred path).
        ui.label(egui::RichText::new("Atmosphere / Fog").strong().size(11.0));
        if ui
            .checkbox(&mut self.render.fog_enabled, "Volumetric Fog")
.default_menu("render.fog_enabled", serde_json::json!(self.render.fog_enabled))
            .on_hover_text("Homogeneous fog with height falloff along PT rays (Beer's law). Same density as viewport Fog.")
            .changed()
        {
            self.render.pt_accumulated_frames = 0;
            self.render.pt_converged = false;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.fog_enabled {
            ui.horizontal(|ui| {
                ui.label("Color");
                if ui.color_edit_button_rgb(&mut self.render.fog_color).changed() {
                    self.render.pt_accumulated_frames = 0;
                    self.render.pt_converged = false;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
            if crate::ui_fields::unbound_slider(ui, "Density", &mut self.render.fog_density, 0.0, 0.2)
                .changed()
            {
                self.render.fog_density = self.render.fog_density.max(0.0);
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(
                ui,
                "Height Falloff",
                &mut self.render.fog_height_falloff,
                0.0,
                2.0,
            )
            .changed()
            {
                self.render.fog_height_falloff = self.render.fog_height_falloff.max(0.0);
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if crate::ui_fields::unbound_slider(ui, "Scatter", &mut self.render.fog_scatter, 0.0, 2.0)
                .changed()
            {
                self.render.fog_scatter = self.render.fog_scatter.max(0.0);
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        ui.separator();

        // Depth of Field (LIVE PT DOF — pt_dof_* is serialized + read).
        ui.label(egui::RichText::new("Depth of Field").strong().size(11.0));
        if ui.add(egui::Slider::new(&mut self.render.pt_dof_aperture, 0.0..=1.0).text("Aperture")).changed() {
            self.render.pt_accumulated_frames = 0;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.pt_dof_aperture > 0.0 {
            if ui.add(egui::Slider::new(&mut self.render.pt_dof_falloff, 0.0..=1.0).text("Falloff")).changed() {
                self.render.pt_accumulated_frames = 0;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui.add(egui::Slider::new(&mut self.render.pt_focus_radius, 0.05..=5.0).text("Focus Distance")).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        ui.separator();

        // Denoise.
        ui.label(egui::RichText::new("Denoise").strong().size(11.0));
        if ui.checkbox(&mut self.render.pt_denoise, "Enable Denoise")
.default_menu("render.pt_denoise", serde_json::json!(self.render.pt_denoise)).changed() {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.render.pt_denoise {
            if ui.add(egui::Slider::new(&mut self.render.denoise_strength, 0.0..=1.0).text("Strength").step_by(0.01)).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            let mut r = self.render.denoise_radius as f32;
            if ui.add(egui::Slider::new(&mut r, 1.0..=8.0).text("Radius").step_by(1.0)).changed() {
                self.render.denoise_radius = r as u32;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .checkbox(&mut self.render.pt_denoise_end_on_hdr, "Ends on HDR (no blit)")
.default_menu("render.pt_denoise_end_on_hdr", serde_json::json!(self.render.pt_denoise_end_on_hdr))
                .on_hover_text(
                    "ON: even à-trous step counts invert pass0 so the final filter writes HDR directly.\n\
                     OFF: classic ping-pong + final blit.",
                )
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        ui.separator();

        // Display.
        ui.label(egui::RichText::new("Display").strong().size(11.0));
        if ui.checkbox(&mut self.render.pt_show_spp, "Show SPP Counter")
.default_menu("render.pt_show_spp", serde_json::json!(self.render.pt_show_spp)).changed() {}
        if ui.checkbox(&mut self.render.pt_transparent_bg, "Transparent Background")
.default_menu("render.pt_transparent_bg", serde_json::json!(self.render.pt_transparent_bg)).changed() {
            self.render.pt_accumulated_frames = 0;
            self.mark_dirty(DirtyFlags::RENDER);
        }

        // CUDA Raytrace mode settings (only meaningful in a cuda build).
        #[cfg(feature = "cuda")]
        {
            ui.separator();
            ui.label(egui::RichText::new("CUDA Raytrace (secondary)").strong().size(11.0));
            if ui.add(egui::Slider::new(&mut self.render.rt_bounces, 1..=8).text("Bounces")).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui.add(egui::Slider::new(&mut self.render.rt_shadow_samples, 1..=16).text("Shadow Samples")).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui.add(egui::Slider::new(&mut self.render.rt_env_samples, 1..=16).text("Env Samples")).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui.checkbox(&mut self.render.rt_caustics, "Caustics")
.default_menu("render.rt_caustics", serde_json::json!(self.render.rt_caustics)).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        #[cfg(not(feature = "cuda"))]
        {
            ui.separator();
            ui.label(
                egui::RichText::new("CUDA Raytrace: build with --features cuda")
                    .small()
                    .weak(),
            );
        }
    }

    // ── GLOBAL ⌄ — CAMERA + VIEWPORT OVERLAYS ───────────────────────────────
    /// What can sit ON TOP of the shaded image: wire, selection, gizmo, grid,
    /// poly verts, camera projection + lens FOV.
    ///
    /// Intentionally NOT here (owner 2026-07-27): Front/Top/Right/Focus view
    /// presets, analytical quality ladder (unused after DC mesh path), Per-Body
    /// Cache, interaction half-res, multi-GPU. Those live elsewhere or nowhere.
    pub(crate) fn render_global_settings_body(&mut self, ui: &mut egui::Ui) {
        // ── CAMERA ──────────────────────────────────────────────────────────
        ui.label(
            egui::RichText::new("CAMERA")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let persp_on = !self.render.ortho;
            if ui.selectable_label(persp_on, "Perspective").clicked() && !persp_on {
                self.set_user_projection_ortho(false);
            }
            if ui.selectable_label(!persp_on, "Orthographic").clicked() && persp_on {
                self.set_user_projection_ortho(true);
            }
        });
        if !self.render.ortho {
            if ui
                .add(
                    egui::Slider::new(&mut self.render.fov_deg, 10.0..=170.0)
                        .text("Lens FOV")
                        .suffix("°"),
                )
                .on_hover_text("Perspective field of view (camera lens angle)")
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        } else {
            ui.weak("FOV is perspective-only");
        }

        ui.separator();

        // ── OVERLAYS (master + display layers) ───────────────────────────────
        ui.label(
            egui::RichText::new("OVERLAYS")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_space(2.0);
        if ui
            .checkbox(&mut self.viewport.show_overlays, "Show overlays")
                .default_menu("viewport.show_overlays", serde_json::json!(self.viewport.show_overlays))
            .on_hover_text("Master toggle for all viewport overlays (wire, selection, gizmos…)")
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        ui.add_enabled_ui(self.viewport.show_overlays, |ui| {
            if ui.checkbox(&mut self.viewport.grid_visible, "Grid")
                .default_menu("viewport.grid_visible", serde_json::json!(self.viewport.grid_visible)).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .checkbox(&mut self.viewport.show_workplane, "Work plane")
                .default_menu("viewport.show_workplane", serde_json::json!(self.viewport.show_workplane))
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .checkbox(&mut self.viewport.show_light_gizmos, "Light gizmos")
                .default_menu("viewport.show_light_gizmos", serde_json::json!(self.viewport.show_light_gizmos))
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .checkbox(&mut self.viewport.show_poly_overlays, "Poly overlays")
                .default_menu("viewport.show_poly_overlays", serde_json::json!(self.viewport.show_poly_overlays))
                .on_hover_text("Edit-mode wires, verts, faces")
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui.checkbox(&mut self.viewport.show_gizmo, "Transform gizmo")
                .default_menu("viewport.show_gizmo", serde_json::json!(self.viewport.show_gizmo)).changed() {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if self.viewport.show_gizmo {
                ui.indent("gizmo_center", |ui| {
                    if ui
                        .checkbox(
                            &mut self.viewport.show_reset_handle,
                            "Center point (reset handle)",
                        )
                .default_menu("viewport.show_reset_handle", serde_json::json!(self.viewport.show_reset_handle))
                        .on_hover_text(
                            "Gizmo center handle — click to reset rotate/scale pivot",
                        )
                        .changed()
                    {
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                });
            }
        });

        ui.separator();

        // ── WIRE (object-mode mesh overlay) ──────────────────────────────────
        ui.label(
            egui::RichText::new("WIREFRAME")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_space(2.0);
        {
            let mut wire = self.render.wire_overlay_enabled;
            if ui
                .checkbox(
                    &mut wire,
                    format!(
                        "Object wire ({})",
                        self.keybindings.label(ACT_WIRE_OVERLAY)
                    ),
                )
.default_menu("render.wire_overlay_enabled", serde_json::json!(self.render.wire_overlay_enabled))
                .on_hover_text(
                    "Draw mesh edges over any shading mode (poly + SDF DC topology).",
                )
                .changed()
            {
                self.render.wire_overlay_enabled = wire;
                self.overlay_lines_fp = u64::MAX;
                if wire {
                    self.wire_local_cache.clear();
                }
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if self.render.wire_overlay_enabled {
            ui.indent("obj_wire", |ui| {
                if ui
                    .add(
                        egui::Slider::new(&mut self.render.wire_overlay_thickness, 0.0..=20.0)
                            .text("Thickness")
                            .clamping(egui::SliderClamping::Never),
                    )
                    .changed()
                {
                    self.render.wire_overlay_thickness =
                        self.render.wire_overlay_thickness.max(0.0);
                    self.overlay_lines_fp = u64::MAX;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui
                    .add(
                        egui::Slider::new(&mut self.render.wire_overlay_opacity, 0.0..=1.0)
                            .text("Opacity"),
                    )
                    .changed()
                {
                    self.overlay_lines_fp = u64::MAX;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                ui.horizontal(|ui| {
                    ui.label("Color");
                    let mut c32 = egui::Color32::from_rgb(
                        (self.render.wire_overlay_color[0] * 255.0) as u8,
                        (self.render.wire_overlay_color[1] * 255.0) as u8,
                        (self.render.wire_overlay_color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut c32).changed() {
                        self.render.wire_overlay_color = [
                            c32.r() as f32 / 255.0,
                            c32.g() as f32 / 255.0,
                            c32.b() as f32 / 255.0,
                        ];
                        self.overlay_lines_fp = u64::MAX;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                });
                if ui
                    .checkbox(&mut self.render.wire_overlay_backface, "Show backfaces")
.default_menu("render.wire_overlay_backface", serde_json::json!(self.render.wire_overlay_backface))
                    .changed()
                {
                    self.overlay_lines_fp = u64::MAX;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            });
        }

        ui.separator();

        // ── POLY EDIT (wire + verts) ─────────────────────────────────────────
        ui.label(
            egui::RichText::new("POLY EDIT")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_space(2.0);
        if ui
            .add(
                egui::Slider::new(&mut self.render.poly_wire_thickness, 0.5..=5.0)
                    .text("Edge size"),
            )
            .changed()
        {
            self.overlay_lines_fp = u64::MAX;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .add(
                egui::Slider::new(&mut self.render.poly_wire_opacity, 0.05..=1.0)
                    .text("Edge opacity"),
            )
            .changed()
        {
            self.overlay_lines_fp = u64::MAX;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .add(
                egui::Slider::new(&mut self.viewport.poly_sel_vert_size, 1.0..=12.0)
                    .text("Vertex size"),
            )
            .on_hover_text("Selected / edit-mode vertex point radius (px)")
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if ui
            .add(
                egui::Slider::new(&mut self.viewport.poly_sel_edge_width, 0.5..=8.0)
                    .text("Selected edge size"),
            )
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        {
            let mut xray = !self.sel.visible_only;
            if ui
                .checkbox(&mut xray, "X-Ray (all edges + select through)")
                .on_hover_text(
                    "On: every edge + pick through the surface. Off: front-facing wire + visible-only pick.",
                )
                .changed()
            {
                self.sel.visible_only = !xray;
                self.overlay_lines_fp = u64::MAX;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }

        ui.separator();

        // ── MESH EDGE MARKS (UV seam / sharp / weight) ───────────────────────
        ui.label(
            egui::RichText::new("MESH EDGE MARKS")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_space(2.0);
        ui.add_enabled_ui(self.viewport.show_overlays, |ui| {
            if ui
                .checkbox(&mut self.viewport.show_uv_seams, "UV seams")
.default_menu("viewport.show_uv_seams", serde_json::json!(self.viewport.show_uv_seams))
                .on_hover_text("Draw authored UV seam edges in red on the active poly mesh.")
                .changed()
            {
                self.overlay_lines_fp = u64::MAX;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .checkbox(&mut self.viewport.show_sharp_edges, "Sharp edges")
.default_menu("viewport.show_sharp_edges", serde_json::json!(self.viewport.show_sharp_edges))
                .on_hover_text("Draw Mark Sharp edges in cyan (Ctrl+Shift+E in Edge mode).")
                .changed()
            {
                self.overlay_lines_fp = u64::MAX;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .checkbox(&mut self.viewport.show_edge_weights, "Edge weights")
.default_menu("viewport.show_edge_weights", serde_json::json!(self.viewport.show_edge_weights))
                .on_hover_text(
                    "Draw per-edge weights / creases in magenta (strength = brightness). \
                     Poly menu → Set Edge Weight.",
                )
                .changed()
            {
                self.overlay_lines_fp = u64::MAX;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        });

        ui.separator();

        // ── SELECTION OUTLINE ────────────────────────────────────────────────
        ui.label(
            egui::RichText::new("SELECTION")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_space(2.0);
        if ui
            .checkbox(&mut self.viewport.show_sel_rim, "Object outline")
.default_menu("viewport.show_sel_rim", serde_json::json!(self.viewport.show_sel_rim))
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.viewport.show_sel_rim {
            if ui
                .add(
                    egui::Slider::new(&mut self.viewport.sel_outline_width, 0.5..=8.0)
                        .text("Outline width")
                        .suffix(" px"),
                )
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if ui
            .checkbox(
                &mut self.viewport.show_selection_ui,
                "Hover rings (selection UI)",
            )
.default_menu("viewport.show_selection_ui", serde_json::json!(self.viewport.show_selection_ui))
            .changed()
        {
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    // ── ui2 panel wrappers (the 6 PanelKind draw fns) ───────────────────────
    // Each = a section title + the shared body. The shading strip's RMB popover
    // and the docked pane both call these, so a docked panel and a quick popover
    // are pixel-identical (single draw path).

    fn ui2_settings_heading(ui: &mut egui::Ui, title: &str) {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(title)
                .strong()
                .size(12.5)
                .color(ui_theme::TEXT),
        );
        ui.separator();
    }

    pub(crate) fn ui2_draw_wire_settings(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "DC Wire");
        self.render_wire_settings_body(ui);
    }

    pub(crate) fn ui2_draw_solid_settings(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "Solid · MatCap · Lit");
        self.render_solid_settings_body(ui);
    }

    pub(crate) fn ui2_draw_material_settings(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "Material · Lookdev");
        // Exposure + tonemapper live in the tonemap chain (env body is env-only).
        if self.env_hdr_width > 0 {
            if ui
                .add(
                    egui::Slider::new(&mut self.render.exposure, 0.01..=5.0)
                        .text("Exposure")
                        .clamping(egui::SliderClamping::Never),
                )
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            ui.separator();
        }
        self.render_env_ibl_body(ui);
    }

    pub(crate) fn ui2_draw_pbr_settings(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "PBR · Full Post-FX");
        // Material == env IBL + everything the PBR post chain adds.
        egui::CollapsingHeader::new("Environment / IBL")
            .default_open(true)
            .show(ui, |ui| self.render_env_ibl_body(ui));
        ui.separator();
        self.render_pbr_postfx_body(ui);
    }

    pub(crate) fn ui2_draw_pt_settings(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "Path Trace");
        self.render_pt_settings_body(ui);
    }

    pub(crate) fn ui2_draw_matcap_settings(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "MatCap");
        self.render_matcap_settings_body(ui);
    }

    pub(crate) fn ui2_draw_env_settings(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "HDR / Environment");
        self.render_env_ibl_body(ui);
    }

    pub(crate) fn ui2_draw_render_settings_global(&mut self, ui: &mut egui::Ui) {
        Self::ui2_settings_heading(ui, "Overlays · Camera");
        self.render_global_settings_body(ui);
    }
}
