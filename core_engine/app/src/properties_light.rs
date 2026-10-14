//! Light properties sub-panel — Unreal-style lighting controls.
//! Viewport also exposes zone / softness / range handles on selected lights.

use eframe::egui;
use glam::Vec3;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_props_light(&mut self, ui: &mut egui::Ui) {
        let last_light = match self.sel.usel.last_clicked { Some(SceneItemId::Light(lid)) => Some(lid), _ => None };
        // A light selected in the OUTLINER arrives as a graph node, not a `SceneItemId::Light`
        // (verified by instrumentation: clicking Sun.001 logs `active=Some(Node(NodeId(1)))`).
        // Lights live in the scene graph as `NodePayload::Light`, so resolve that route too —
        // otherwise this function finds no light and silently draws nothing, which is exactly
        // what the owner saw. Both routes end at the same `LightData.id`, so everything below is
        // unchanged.
        let node_light = self
            .sel
            .usel
            .active_node()
            .and_then(|nid| self.scene.scene_graph.get(nid))
            .and_then(|n| match &n.payload {
                core_math::scene_graph::NodePayload::Light(d) => Some(d.id),
                _ => None,
            });
        if let Some(id) = self.sel.usel.active_light().or(node_light).or(last_light) {
            let Some(mut light) = self.scene.light_by_id(id).cloned() else { return; };
            let lt = light.light_type;
            let type_icon = match lt { LightType::Directional => "\u{2600}", LightType::Point => "\u{1F4A1}", LightType::Spot => "\u{1F526}" };
            let type_name = match lt { LightType::Directional => "Directional", LightType::Point => "Point", LightType::Spot => "Spot" };

            // ── Header ──
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(255, 200, 60),
                    format!("{} {}", type_icon, light.name));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut vis = light.visible;
                    if ui.checkbox(&mut vis, "Visible").changed() {
                        self.begin_episode("Toggle light visibility");
                        self.write_light(id, |l| l.visible = vis);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                });
            });
            ui.separator();

            // Name
            ui.horizontal(|ui| {
                ui.label("Name");
                let r = ui.text_edit_singleline(&mut light.name);
                if r.gained_focus() { self.begin_episode("Edit light name"); }
                if r.changed() {
                    let name = light.name.clone();
                    self.write_light(id, |l| l.name = name);
                    self.mark_dirty(DirtyFlags::LIGHT);
                }
            });

            // Type switcher (Unreal: change light class without re-create)
            ui.horizontal(|ui| {
                ui.label("Type");
                let mut new_lt = lt;
                egui::ComboBox::from_id_salt("light_type_switch")
                    .selected_text(format!("{} {}", type_icon, type_name))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut new_lt, LightType::Directional, "\u{2600} Directional");
                        ui.selectable_value(&mut new_lt, LightType::Point, "\u{1F4A1} Point");
                        ui.selectable_value(&mut new_lt, LightType::Spot, "\u{1F526} Spot");
                    });
                if new_lt != lt {
                    self.transaction_snapshot_only("Change light type");
                    self.write_light(id, |l| {
                        l.light_type = new_lt;
                        // Sensible migration
                        match new_lt {
                            LightType::Directional => {
                                if l.position.length() > 0.1 {
                                    // Keep aim-ish from position if coming from point
                                }
                                l.ambient = l.ambient.max(0.05);
                            }
                            LightType::Point => {
                                if l.position.length() < 1e-4 {
                                    l.position = Vec3::new(0.0, 2.0, 0.0);
                                }
                                if l.range < 0.5 { l.range = 10.0; }
                                l.ambient = 0.0;
                            }
                            LightType::Spot => {
                                if l.position.length() < 1e-4 {
                                    l.position = Vec3::new(0.0, 3.0, 0.0);
                                }
                                if l.elevation.abs() < 1.0 { l.elevation = -45.0; }
                                if l.spot_outer_deg < 2.0 {
                                    l.spot_inner_deg = 25.0;
                                    l.spot_outer_deg = 35.0;
                                }
                                if l.range < 0.5 { l.range = 15.0; }
                                l.ambient = 0.0;
                            }
                        }
                    });
                    self.mark_dirty(DirtyFlags::LIGHT);
                    // Refresh local copy
                    if let Some(l) = self.scene.light_by_id(id).cloned() { light = l; }
                }
            });
            ui.add_space(4.0);

            // ── Transform ──
            if matches!(light.light_type, LightType::Directional | LightType::Spot) {
                egui::CollapsingHeader::new(egui::RichText::new("Direction").strong())
                    .default_open(true).show(ui, |ui| {
                    let r = ui.add(egui::Slider::new(&mut light.azimuth, -180.0..=180.0)
                        .text("Azimuth \u{00B0}").suffix("\u{00B0}"));
                    if r.drag_started() { self.begin_episode("Edit light azimuth"); }
                    if r.changed() { let v = light.azimuth; self.write_light(id, |l| l.azimuth = v); self.mark_dirty(DirtyFlags::LIGHT); }
                    r.on_hover_text("Horizontal aim. Viewport: R / rotate gizmo.");
                    let r = ui.add(egui::Slider::new(&mut light.elevation, -90.0..=90.0)
                        .text("Elevation \u{00B0}").suffix("\u{00B0}"));
                    if r.drag_started() { self.begin_episode("Edit light elevation"); }
                    if r.changed() { let v = light.elevation; self.write_light(id, |l| l.elevation = v); self.mark_dirty(DirtyFlags::LIGHT); }
                    r.on_hover_text("Vertical aim (+90 = up).");
                });
                ui.add_space(2.0);
            }
            if matches!(light.light_type, LightType::Point | LightType::Spot) {
                egui::CollapsingHeader::new(egui::RichText::new("Transform").strong())
                    .default_open(true).show(ui, |ui| {
                    ui.weak("Viewport: move gizmo / G grab");
                    let rx = ui.add(egui::DragValue::new(&mut light.position.x).speed(0.05).prefix("X "));
                    let ry = ui.add(egui::DragValue::new(&mut light.position.y).speed(0.05).prefix("Y "));
                    let rz = ui.add(egui::DragValue::new(&mut light.position.z).speed(0.05).prefix("Z "));
                    if rx.drag_started() || ry.drag_started() || rz.drag_started() {
                        self.begin_episode("Edit light position");
                    }
                    if rx.changed() || ry.changed() || rz.changed() {
                        let pos = light.position;
                        self.write_light(id, |l| l.position = pos);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                    let r = ui.add(egui::Slider::new(&mut light.range, 0.5..=200.0).text("Attenuation Radius")
                        .clamping(egui::SliderClamping::Never).logarithmic(true));
                    if r.drag_started() { self.begin_episode("Edit light range"); }
                    if r.changed() { let v = light.range; self.write_light(id, |l| l.range = v); self.mark_dirty(DirtyFlags::LIGHT); }
                    r.on_hover_text("How far surface lighting reaches. Drag the amber knob in the viewport.");
                });
                ui.add_space(2.0);
            }
            if light.light_type == LightType::Spot {
                egui::CollapsingHeader::new(egui::RichText::new("Cone").strong())
                    .default_open(true).show(ui, |ui| {
                    let r = ui.add(egui::Slider::new(&mut light.spot_inner_deg, 1.0..=89.0).text("Inner Cone")
                        .suffix("\u{00B0}").clamping(egui::SliderClamping::Never));
                    if r.drag_started() { self.begin_episode("Edit spot inner angle"); }
                    if r.changed() { let v = light.spot_inner_deg; self.write_light(id, |l| l.spot_inner_deg = v); self.mark_dirty(DirtyFlags::LIGHT); }
                    let r = ui.add(egui::Slider::new(&mut light.spot_outer_deg, 2.0..=90.0).text("Outer Cone")
                        .suffix("\u{00B0}").clamping(egui::SliderClamping::Never));
                    if r.drag_started() { self.begin_episode("Edit spot outer angle"); }
                    if r.changed() {
                        let v = light.spot_outer_deg.max(light.spot_inner_deg + 0.5);
                        light.spot_outer_deg = v;
                        self.write_light(id, |l| l.spot_outer_deg = v);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                });
                ui.add_space(2.0);
            }

            // ── Light (intensity / color) ──
            egui::CollapsingHeader::new(egui::RichText::new("Light").strong())
                .default_open(true).show(ui, |ui| {
                let r = ui.add(egui::Slider::new(&mut light.intensity, 0.0..=100.0).text("Intensity")
                    .clamping(egui::SliderClamping::Never).logarithmic(true));
                if r.drag_started() { self.begin_episode("Edit light intensity"); }
                if r.changed() { let v = light.intensity; self.write_light(id, |l| l.intensity = v); self.mark_dirty(DirtyFlags::LIGHT); }
                r.on_hover_text("Candela-ish brightness. Log scale for fine control at low values.");

                if light.light_type == LightType::Directional {
                    let r = ui.add(
                        egui::Slider::new(&mut light.ambient, 0.0..=2.0)
                            .text("Ambient")
                            .clamping(egui::SliderClamping::Never),
                    );
                    if r.drag_started() {
                        self.begin_episode("Edit light ambient");
                    }
                    if r.changed() {
                        let v = light.ambient;
                        self.write_light(id, |l| l.ambient = v);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                    r.on_hover_text(
                        "Flat ambient strength (shadow-side fill). Multiplies Ambient Color.\n\
                         When HDR is loaded and From HDR is on, tint comes from the environment.",
                    );

                    ui.horizontal(|ui| {
                        ui.label("Ambient Color");
                        let ac = &light.ambient_color;
                        let mut col = [
                            (ac[0].clamp(0.0, 1.0) * 255.0) as u8,
                            (ac[1].clamp(0.0, 1.0) * 255.0) as u8,
                            (ac[2].clamp(0.0, 1.0) * 255.0) as u8,
                        ];
                        let r = ui.color_edit_button_srgb(&mut col);
                        if r.drag_started() && !self.ui.undo_drag_light {
                            self.begin_episode("Edit ambient color");
                            self.ui.undo_drag_light = true;
                        }
                        if r.changed() {
                            let c = [
                                col[0] as f32 / 255.0,
                                col[1] as f32 / 255.0,
                                col[2] as f32 / 255.0,
                            ];
                            light.ambient_color = c;
                            self.write_light(id, |l| l.ambient_color = c);
                            self.mark_dirty(DirtyFlags::LIGHT);
                        }
                        r.on_hover_text(
                            "Tint for the flat ambient term (linear sRGB picker).\n\
                             Disabled look when From HDR is on — HDR supplies the tint.",
                        );
                    });

                    let mut from_hdr = light.ambient_from_hdr;
                    let hdr_avail = self.render.env_enabled && self.env_hdr_width > 0;
                    let resp = ui
                        .add_enabled(
                            hdr_avail || from_hdr,
                            egui::Checkbox::new(&mut from_hdr, "Ambient from HDR"),
                        )
                        .on_hover_text(
                            if hdr_avail {
                                "Use the loaded HDRI (SH sky average × env brightness) as ambient tint.\n\
                                 Ambient strength still applies. IBL diffuse continues separately."
                            } else {
                                "Load an HDR environment (Light & Env) to drive ambient tint from the sky."
                            },
                        );
                    if resp.changed() {
                        self.begin_episode("Toggle ambient from HDR");
                        self.write_light(id, |l| l.ambient_from_hdr = from_hdr);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                    if from_hdr && !hdr_avail {
                        ui.weak("No HDR loaded — using Ambient Color until env is set.");
                    }
                }

                // (The old 2..64 "Source Radius" slider was REMOVED 2026-08-07: it fed
                // GPU slot 14, whose only consumer — the analytical soft_shadow — is
                // deleted. Dragging it did nothing, and its name collided with the REAL
                // Source Radius in the Ray-Traced Shadow group below. The serialized
                // `shadow_softness` field stays for save compat; UI is gone.)

                // Color: the picker edits sRGB bytes; LightData.color is LINEAR.
                // Round-trip through egui::Rgba so both directions decode/encode
                // properly (the old raw /255 skipped the sRGB decode — mid-tones
                // shaded ~2× too bright relative to the swatch).
                let lc0 = light.color;
                let mut col32 = egui::Color32::from(egui::Rgba::from_rgb(lc0[0], lc0[1], lc0[2]));
                ui.horizontal(|ui| {
                    ui.label("Color");
                    let r = ui.color_edit_button_srgba(&mut col32);
                    if r.drag_started() && !self.ui.undo_drag_light {
                        self.begin_episode("Edit light color");
                        self.ui.undo_drag_light = true;
                    }
                    if r.changed() {
                        let lin = egui::Rgba::from(col32);
                        let c = [lin.r(), lin.g(), lin.b()];
                        self.write_light(id, |l| l.color = c);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                });
            });

            // ── Volumetric (god rays / bulb glow) ──
            let vol_open = light.volumetric.enabled;
            egui::CollapsingHeader::new(egui::RichText::new("Volumetric Scattering").strong()
                .color(egui::Color32::from_rgb(160, 130, 255)))
                .default_open(vol_open)
                .show(ui, |ui| {
                    let mut en = light.volumetric.enabled;
                    if ui.checkbox(&mut en, "Enabled").on_hover_text(
                        "Participating medium around this light.\n\
                         Point = glow sphere centered on the lamp.\n\
                         Spot = light cone shafts.\n\
                         Directional = atmospheric god rays.\n\
                         Viewport: drag violet Zone / Softness knobs."
                    ).changed() {
                        self.begin_episode("Toggle volumetric light");
                        self.write_light(id, |l| l.volumetric.enabled = en);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                    ui.add_enabled_ui(en, |ui| {
                        let r = ui.add(egui::Slider::new(&mut light.volumetric.density, 0.0..=4.0)
                            .text("Density").clamping(egui::SliderClamping::Never));
                        if r.drag_started() { self.begin_episode("Edit volumetric density"); }
                        if r.changed() {
                            let v = light.volumetric.density;
                            self.write_light(id, |l| l.volumetric.density = v);
                            self.mark_dirty(DirtyFlags::LIGHT);
                        }
                        r.on_hover_text("Scattering strength of the light's medium.");

                        let r = ui.add(egui::Slider::new(&mut light.volumetric.softness, 0.0..=1.0)
                            .text("Softness"));
                        if r.drag_started() { self.begin_episode("Edit volumetric softness"); }
                        if r.changed() {
                            let v = light.volumetric.softness;
                            self.write_light(id, |l| l.volumetric.softness = v);
                            self.mark_dirty(DirtyFlags::LIGHT);
                        }
                        r.on_hover_text("Edge feather 0..1. Drag the inner violet knob in the viewport.");

                        let mut zone = light.volumetric.zone;
                        let eff = if zone > 0.0 { zone } else { light.range };
                        let r = ui.add(egui::Slider::new(&mut zone, 0.0..=100.0)
                            .text("Zone (reach)").clamping(egui::SliderClamping::Never));
                        if r.drag_started() { self.begin_episode("Edit volumetric zone"); }
                        if r.changed() {
                            self.write_light(id, |l| l.volumetric.zone = zone);
                            self.mark_dirty(DirtyFlags::LIGHT);
                        }
                        r.on_hover_text(format!(
                            "Volume radius / shaft length. 0 = use Attenuation Radius ({:.1}).\n\
                             Drag the outer violet knob in the viewport.",
                            light.range
                        ));
                        if light.volumetric.zone <= 0.0 {
                            ui.weak(format!("Using range = {:.2} as zone", eff));
                        }
                    });
                });

            // ── Ray-Traced Shadow ──
            // DIRECTIONAL (the sun) is included as of 2026-07-28 (RT_SHADOWS_DESIGN's deferred
            // "Sun RT area shadows"). It was excluded here while `rt_shadow_entry.wgsl` skipped
            // directional lights outright; that skip is gone, so the sun now offers the same
            // per-light opt-in — soft, all-geometry, self-shadowing sun.
            {
                let is_sun = light.light_type == LightType::Directional;
                egui::CollapsingHeader::new(egui::RichText::new("Ray-Traced Shadow").strong()
                    .color(egui::Color32::from_rgb(130, 200, 255)))
                    .default_open(light.rt_shadow.enabled && light.rt_shadow.radius > 0.0 && !is_sun)
                    .show(ui, |ui| {
                        // OPT-IN FOR THE SUN. `LightRtShadow::default()` is `enabled: true` (it
                        // predates this feature and means "if global RT is on, point/spot lights
                        // participate"), so reading it raw would silently switch EVERY existing
                        // scene's sun to RT the moment global RT is enabled — the opposite of the
                        // opt-in the owner asked for, and an unannounced perf cliff (a sun ray is
                        // distance-uncapped and fires on every lit pixel). The sun therefore
                        // requires a radius the user actually set: `radius > 0` IS the opt-in.
                        // Point/spot behaviour is bit-identical to before.
                        let effective_en = light.rt_shadow.enabled
                            && (!is_sun || light.rt_shadow.radius > 0.0);
                        let mut en = light.rt_shadow.enabled;
                        if ui.checkbox(&mut en, "Enable (all-geometry BVH)")
                            .on_hover_text("Requires global Render \u{2192} Ray-Traced Shadows ON.")
                            .changed()
                        {
                            self.begin_episode("Toggle ray-traced shadow");
                            self.write_light(id, |l| l.rt_shadow.enabled = en);
                            self.mark_dirty(DirtyFlags::LIGHT);
                        }
                        if is_sun {
                            if effective_en {
                                ui.weak("Sun: ray-traced soft shadows ACTIVE (replaces shadow maps).");
                            } else {
                                ui.weak("Sun: set Source Radius above 0 to switch from shadow maps \
                                         to ray-traced soft shadows.");
                            }
                            ui.weak("Costliest RT light — rays are distance-uncapped and fire on \
                                     every lit pixel.");
                        }
                        ui.add_enabled_ui(en, |ui| {
                            let range = if is_sun { 0.0..=1.0 } else { 0.0..=2.0 };
                            let r = ui.add(egui::Slider::new(&mut light.rt_shadow.radius, range)
                                .text("Source Radius"));
                            if r.drag_started() { self.begin_episode("Edit RT shadow radius"); }
                            if r.changed() {
                                let v = light.rt_shadow.radius;
                                self.write_light(id, |l| l.rt_shadow.radius = v);
                                self.mark_dirty(DirtyFlags::LIGHT);
                            }
                            if is_sun {
                                ui.weak("Angular size. 0 = inherit global softness.");
                            } else {
                                // Unified 2026-08-07 ("fix the light"): ONE radius, WORLD
                                // units, drives highlight size + RT penumbra + PT sampling.
                                ui.weak("Bulb radius in WORLD units — sets the specular \
                                         highlight size AND the shadow penumbra (and the \
                                         path tracer samples the same bulb). 0 = pinpoint.");
                            }
                        });
                    });
            }

            if !ui.ctx().dragged_id().is_some() { self.ui.undo_drag_light = false; }

            // ── Light Linking ──
            egui::CollapsingHeader::new(egui::RichText::new("Light Linking").strong()
                .color(egui::Color32::from_rgb(180, 200, 255)))
                .default_open(false).show(ui, |ui| {
                let mode_idx = match light.link_mode {
                    LightLinkMode::Default => 0usize,
                    LightLinkMode::Include => 1,
                    LightLinkMode::Exclude => 2,
                };
                let modes = ["Default (All)", "Include Only", "Exclude"];
                let mut sel_mode = mode_idx;
                egui::ComboBox::from_label("Link Mode").selected_text(modes[sel_mode])
                    .show_ui(ui, |ui| {
                        for (i, label) in modes.iter().enumerate() {
                            ui.selectable_value(&mut sel_mode, i, *label);
                        }
                    });
                if sel_mode != mode_idx {
                    self.transaction_snapshot_only("Change light link mode");
                    let new_mode = match sel_mode {
                        1 => LightLinkMode::Include,
                        2 => LightLinkMode::Exclude,
                        _ => LightLinkMode::Default,
                    };
                    light.link_mode = new_mode.clone();
                    self.write_light(id, |l| l.link_mode = new_mode);
                    self.mark_dirty(DirtyFlags::LIGHT);
                }

                if light.link_mode != LightLinkMode::Default {
                    ui.label(match light.link_mode {
                        LightLinkMode::Include => "Affects ONLY these prims:",
                        LightLinkMode::Exclude => "Affects everything EXCEPT:",
                        _ => "",
                    });
                    let mut link_change: Option<(u32, bool)> = None;
                    for (pi, prim) in self.scene.primitives.iter().enumerate() {
                        let uid = prim.prim_uid;
                        let mut included = light.link_targets.contains(&uid);
                        if ui.checkbox(&mut included, format!("[{}] {}", pi, &prim.name)).changed() {
                            link_change = Some((uid, included));
                        }
                    }
                    if let Some((uid, included)) = link_change {
                        self.transaction_snapshot_only("Edit light link target");
                        if included {
                            if !light.link_targets.contains(&uid) { light.link_targets.push(uid); }
                        } else {
                            light.link_targets.retain(|&t| t != uid);
                        }
                        let targets = light.link_targets.clone();
                        self.write_light(id, |l| l.link_targets = targets);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                }

                ui.horizontal(|ui| {
                    ui.label("Group");
                    let r = ui.text_edit_singleline(&mut light.light_group);
                    if r.gained_focus() { self.begin_episode("Edit light group"); }
                    if r.changed() {
                        let g = light.light_group.clone();
                        self.write_light(id, |l| l.light_group = g);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                });
            });
        }
    }
}

impl AzraApp {
    /// Quick light color popup (C with a lights-only selection, owner 2026-08-07):
    /// a floating color square anchored at the press position. Scroll over the
    /// VIEWPORT adjusts intensity (input_viewport wheel arm — camera zoom is
    /// suppressed while open); a click outside the popup, Esc, or C again closes
    /// it. All edits ride ONE undo episode (opened in handle_keyboard, committed
    /// on close).
    pub(crate) fn draw_light_color_popup(&mut self, ctx: &egui::Context) {
        let Some((lid, anchor)) = self.ui.light_color_popup else { return };
        let Some(light) = self.scene.light_by_id(lid).cloned() else {
            self.ui.light_color_popup = None;
            return;
        };
        let mut col32 = egui::Color32::from(egui::Rgba::from_rgb(
            light.color[0],
            light.color[1],
            light.color[2],
        ));
        let area = egui::Area::new(egui::Id::new("light_color_popup"))
            .fixed_pos(anchor + egui::vec2(14.0, 14.0))
            .order(egui::Order::Foreground);
        let inner = area.show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(210.0);
                ui.label(egui::RichText::new(light.name.clone()).strong());
                if egui::color_picker::color_picker_color32(
                    ui,
                    &mut col32,
                    egui::color_picker::Alpha::Opaque,
                ) {
                    // sRGB picker → LINEAR LightData.color (same round-trip as the panel).
                    let lin = egui::Rgba::from(col32);
                    let c = [lin.r(), lin.g(), lin.b()];
                    self.write_light(lid, |l| l.color = c);
                    self.mark_dirty(DirtyFlags::LIGHT);
                }
                ui.weak(format!(
                    "Intensity {:.2} — scroll wheel over the viewport",
                    light.intensity
                ));
            });
        });
        // Close on any press OUTSIDE the popup (a viewport click included) or Esc.
        let clicked_out =
            ctx.input(|i| i.pointer.any_pressed()) && !inner.response.contains_pointer();
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if clicked_out || esc {
            self.ui.light_color_popup = None;
            self.commit_episode();
        }
    }
}
