//! SDF material, DNA, procedural layers — extracted from properties_sdf.rs.

use crate::ui_menus::AzraMenus;
use eframe::egui;
use glam::Vec3;
use core_math::primitives::{PbrMaterial, MaterialQuality, DnaRule, DnaCondition, DnaInput, DnaBlendMode, MaterialDna, MaterialId};
use crate::*;
use crate::properties_sdf::PropsContext;

// --- sRGB <-> linear color space conversion helpers ---
// egui color pickers operate in sRGB space. PbrMaterial stores linear values
// consumed by the GPU. These functions convert at the UI<->data boundary.

/// Convert a single channel from sRGB to linear (apply inverse gamma).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Convert a single channel from linear to sRGB (apply gamma).
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Convert a linear `Vec3` color to an egui `Color32` (sRGB).
fn linear_vec3_to_color32(v: Vec3) -> egui::Color32 {
    egui::Color32::from_rgb(
        (linear_to_srgb(v.x.clamp(0.0, 1.0)) * 255.0) as u8,
        (linear_to_srgb(v.y.clamp(0.0, 1.0)) * 255.0) as u8,
        (linear_to_srgb(v.z.clamp(0.0, 1.0)) * 255.0) as u8,
    )
}

/// Convert an egui `Color32` (sRGB) back to a linear `Vec3`.
fn color32_to_linear_vec3(c: egui::Color32) -> Vec3 {
    Vec3::new(
        srgb_to_linear(c.r() as f32 / 255.0),
        srgb_to_linear(c.g() as f32 / 255.0),
        srgb_to_linear(c.b() as f32 / 255.0),
    )
}

/// Convert a linear `Vec3` to an sRGB `[f32; 3]` for `color_edit_button_rgb`.
fn linear_vec3_to_srgb_array(v: Vec3) -> [f32; 3] {
    [linear_to_srgb(v.x.clamp(0.0, 1.0)), linear_to_srgb(v.y.clamp(0.0, 1.0)), linear_to_srgb(v.z.clamp(0.0, 1.0))]
}

/// Convert an sRGB `[f32; 3]` from `color_edit_button_rgb` back to a linear `Vec3`.
fn srgb_array_to_linear_vec3(c: [f32; 3]) -> Vec3 {
    Vec3::new(srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2]))
}

impl AzraApp {
    pub(crate) fn draw_props_sdf_material(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, pctx: &mut PropsContext, idx: usize) {
        // ---- Material / Shader Panel ----
        egui::CollapsingHeader::new(egui::RichText::new("🎨 Material").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
            .default_open(true).show(ui, |ui| {
            // --- Material assignment (always visible) ---
            // A referenced graph material WINS over the embedded one (resolve_prim_material), so
            // when linked the inert embedded editor is hidden and a jump button opens the shared
            // material's editor. "+ New Material" seeds a shared material from this prim's look.
            let current_mid = self.scene.primitives[idx].material_id;
            let linked = !current_mid.is_none()
                && self.scene.scene_graph.material_by_id(current_mid).is_some();
            {
                let mut graph_mats: Vec<(MaterialId, String)> = self.scene.scene_graph
                    .materials()
                    .map(|(_, mid, name, _)| (mid, name.to_string()))
                    .collect();
                graph_mats.sort_by_key(|(mid, _)| mid.0); // HashMap order is random — sort
                ui.horizontal(|ui| {
                    ui.label("Material:");
                    let current_label = if current_mid.is_none() {
                        "Embedded (local)".to_string()
                    } else {
                        self.scene.scene_graph.material_by_id(current_mid)
                            .map(|(_, m)| {
                                if m.name.is_empty() { format!("Material {}", current_mid.0) }
                                else { m.name.clone() }
                            })
                            .unwrap_or_else(|| format!("Missing ({})", current_mid.0))
                    };
                    egui::ComboBox::from_id_salt("prim_mat_ref").width(140.0).selected_text(&current_label)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(current_mid.is_none(), "Embedded (local)").clicked() && !current_mid.is_none() {
                                pctx.drag_started = true; pctx.changed = true; pctx.desc = "Clear material ref".into();
                                self.scene.primitives[idx].material_id = MaterialId::NONE;
                            }
                            for (mid, name) in &graph_mats {
                                let label = if name.is_empty() { format!("Material {}", mid.0) } else { name.clone() };
                                if ui.selectable_label(current_mid == *mid, &label).clicked() && current_mid != *mid {
                                    pctx.drag_started = true; pctx.changed = true; pctx.desc = "Assign material".into();
                                    self.scene.primitives[idx].material_id = *mid;
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(false, "\u{2795} New Material")
                                .on_hover_text("Create a shared material from this prim's current look and assign it")
                                .clicked()
                            {
                                pctx.new_shared_mat_from_prim = true;
                            }
                        });
                    if linked {
                        if ui.small_button("\u{270F}")
                            .on_hover_text("Edit this material (opens its editor / selects it in the outliner)")
                            .clicked()
                        {
                            pctx.jump_to_material = Some(current_mid);
                        }
                    }
                });
                if linked {
                    ui.label(egui::RichText::new(
                        "Shared material — edits apply to every prim using it. \
                         The embedded editor below is hidden (the shared material overrides it).")
                        .weak().italics().size(10.0));
                }
                ui.add_space(2.0);
            }
            // --- Shader Node Graph ---
            // Primary path: outliner Material owns the graph. When linked, jump there.
            // Prim-level graph remains a local override for unlinked / advanced use.
            if linked {
                ui.horizontal(|ui| {
                    if ui
                        .button("Edit Material Shader Graph")
                        .on_hover_text("Opens the graph on the shared outliner Material")
                        .clicked()
                    {
                        self.open_material_shader_graph(current_mid);
                    }
                    if self.material_has_shader_graph(current_mid) {
                        ui.label(
                            egui::RichText::new("\u{1F517}")
                                .color(egui::Color32::from_rgb(120, 200, 255)),
                        );
                    }
                });
            }
            ui.horizontal(|ui|{
                let has_graph = self.scene.primitives[idx].node_graph.is_some();
                let btn_text = if has_graph {
                    "Edit Prim Graph (override)"
                } else {
                    "Add Prim Graph (override)"
                };
                if ui
                    .button(btn_text)
                    .on_hover_text(
                        "Legacy per-prim graph. Prefer creating a Material in the outliner \
                         and assigning it so the look is shared.",
                    )
                    .clicked()
                {
                    self.open_prim_shader_graph(idx);
                }
                if has_graph && ui.small_button("X").on_hover_text("Remove prim shader graph").clicked() {
                    self.transaction_snapshot_only("Remove shader graph");
                    let uid = self.scene.primitives[idx].prim_uid;
                    self.write_prim(idx, |p| p.node_graph = None);
                    self.ui.node_graphs.remove(&uid);
                    self.clear_node_graph_maps(uid);
                    self.shader_dirty = true;
                    self.mark_dirty(DirtyFlags::RENDER);
                    pctx.changed = true; pctx.desc = "Remove shader graph".into();
                }
            });
            // ===== EMBEDDED (local) material editor — hidden when a shared material is linked
            // (the resolver ignores `p.material` then; showing dead sliders was the old confusing
            // behaviour). Prim-level extras that stay live regardless (shader graph, DNA, texture
            // atlas id) are OUTSIDE this guard. =====
            if !linked {
            // --- Material Preview Swatch ---
            {
                let preview_id = self.cache.mat_preview_cache.get_or_create(ctx, &self.scene.primitives[idx].material);
                ui.horizontal(|ui| {
                    ui.image(egui::load::SizedTexture::new(preview_id, egui::vec2(36.0, 36.0)));
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&self.scene.primitives[idx].material.name).strong());
                        let m = &self.scene.primitives[idx].material;
                        ui.label(egui::RichText::new(format!(
                            "R:{:.2} M:{:.2}", m.roughness, m.metallic
                        )).small().weak());
                    });
                });
            }
            // --- Procedural presets on prim (override). Prefer Material presets on the outliner material. ---
            ui.horizontal(|ui| {
                ui.label("Presets (prim):");
                ui.azra_menu_button("Load Preset...", |ui| {
                    for (name, desc, builder) in crate::shader_nodes::MaterialPresets::list() {
                        if ui.button(format!("{} — {}", name, desc)).clicked() {
                            self.transaction_snapshot_only("Apply shader preset");
                            let mut graph = builder();
                            let uid = self.scene.primitives[idx].prim_uid;
                            // Keep p.node_graph so the UI shows "Edit Prim Graph", but do NOT
                            // set shader_dirty: Hybrid shows baked maps; DF-full recompile hung.
                            let wgsl = graph.generate_wgsl();
                            self.write_prim(idx, |p| p.node_graph = Some(wgsl));
                            self.ui.node_graphs.insert(uid, graph);
                            self.enqueue_node_graph_bake(uid);
                            self.mark_dirty(DirtyFlags::RENDER);
                            self.toast(&format!("Applied + baked prim preset: {}", name));
                            ui.close_menu();
                        }
                    }
                });
            });
            let mat=&mut self.scene.primitives[idx].material;
            // --- Top bar: Name + Type + Quality ---
            ui.horizontal(|ui|{
                ui.label("Name:");
                let te=ui.add(egui::TextEdit::singleline(&mut mat.name).hint_text("Material").desired_width(130.0));
                if te.changed(){pctx.changed=true;pctx.desc="Mat name".into();}
            });
            ui.horizontal(|ui|{
                ui.label("Quality:");
                for (mq,lbl,tip) in [(MaterialQuality::Optimize,"O","Optimize: skip procedural layers"),
                    (MaterialQuality::Normal,"N","Normal: standard quality"),(MaterialQuality::God,"G","God: maximum fidelity")] {
                    if ui.selectable_label(mat.mat_quality==mq,lbl).on_hover_text(tip).clicked() && mat.mat_quality!=mq {
                        mat.mat_quality=mq; pctx.drag_started=true; pctx.changed=true; pctx.desc="Mat quality".into();
                    }
                }
            });
            ui.add_space(2.0);
            // --- Master Presets (with swatches) ---
            // Drop mat borrow temporarily to access preview cache
            let _ = mat;
            ui.horizontal_wrapped(|ui|{
                for (key,label) in PbrMaterial::preset_names() {
                    let preset_mat = PbrMaterial::preset(key);
                    let tid = self.cache.mat_preview_cache.get_or_create(ctx, &preset_mat);
                    let btn = ui.vertical(|ui|{
                        let img = ui.image(egui::load::SizedTexture::new(tid, egui::vec2(24.0, 24.0)));
                        let lbl = ui.label(egui::RichText::new(*label).small());
                        img | lbl
                    });
                    if btn.inner.interact(egui::Sense::click()).clicked() {
                        pctx.new_mat = Some(key.to_string());
                    }
                }
            });
            ui.add_space(4.0); ui.separator();
            // --- Base Properties + Albedo Map (shared unified editor — kills the 4× dup) ---
            let ai_albedo = !self.ai.ai_tex_maps_png[0].is_empty();
            let prim_uid = self.scene.primitives[idx].prim_uid;
            let body_nid = self.scene.active_body_nid;
            // Albedo thumbnail (copy the slot id first — map_slot_thumb needs &mut self).
            let amap_slot = self.scene.primitives[idx].material.albedo_map.tex_id;
            let albedo_thumb = if amap_slot != 0 { self.map_slot_thumb(ctx, amap_slot, false) } else { None };
            // Data-map thumbs (MapKind::DATA_SLOTS order) resolved before mut borrow.
            let data_thumbs = {
                use core_math::pbr_material::MapKind;
                let mut t = [None; 7];
                for (i, kind) in MapKind::DATA_SLOTS.iter().enumerate() {
                    let m = self.scene.primitives[idx].material.map(*kind);
                    if m.is_set() {
                        t[i] = self.map_slot_thumb(ctx, m.tex_id, kind.is_data());
                    }
                }
                t
            };
            let mat_outcome = {
                let mat = &mut self.scene.primitives[idx].material;
                // uv_allowed = false: SDF DC meshes carry no UVs (E4 honest gating).
                let mut o = crate::material_editor::draw_pbr_material(ui, mat, ai_albedo, albedo_thumb, false);
                o.merge(crate::material_editor::draw_data_map_slots(ui, mat, data_thumbs, false));
                o
            };
            if mat_outcome.changed { pctx.changed = true; }
            if mat_outcome.drag_started { pctx.drag_started = true; }
            if !mat_outcome.desc.is_empty() && (mat_outcome.changed || mat_outcome.drag_started) {
                pctx.desc = mat_outcome.desc.into();
            }
            self.handle_albedo_map_outcome(&mat_outcome, idx, prim_uid, body_nid, pctx);
            {
            let mat=&mut self.scene.primitives[idx].material;
            // Emission
            let mut em_col=linear_vec3_to_color32(mat.emission_color);
            ui.horizontal(|ui|{
                ui.label("Emission:");
                if ui.color_edit_button_srgba(&mut em_col).changed(){
                    mat.emission_color=color32_to_linear_vec3(em_col);
                    pctx.drag_started=true; pctx.changed=true; pctx.desc="Emission color".into();
                }
            });
            let r=ui.add(egui::Slider::new(&mut mat.emission_intensity,0.0..=50.0).text("Emission Int").logarithmic(true));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Emission intensity".into();}
            ui.add_space(2.0); ui.separator();
            } // end base-PBR/emission `mat` borrow scope
            // --- Shader mode + Advanced (shared with graph-material Properties) ---
            {
                let prim_uid = self.scene.primitives[idx].prim_uid;
                let mat = &mut self.scene.primitives[idx].material;
                let adv = crate::material_editor::draw_shader_mode_and_advanced(
                    ui,
                    mat,
                    prim_uid as u64,
                );
                if adv.changed {
                    pctx.changed = true;
                }
                if adv.drag_started {
                    pctx.drag_started = true;
                }
                if !adv.desc.is_empty() && (adv.changed || adv.drag_started) {
                    pctx.desc = adv.desc.into();
                }
            }
            // --- SDF-Gradient Layers (Edge Wear + Cavity Dirt) ---
            egui::CollapsingHeader::new(egui::RichText::new("SDF Procedural Layers").size(11.0))
                .default_open(false)
                .show(ui, |ui| {
                let mat = &mut self.scene.primitives[idx].material;
                // -- Edge Wear (convex / exposed edges) --
                ui.label(egui::RichText::new("Edge Wear").strong().size(10.0).color(egui::Color32::from_rgb(220, 160, 80)));
                let r = ui.checkbox(&mut mat.edge_wear.enabled, "Enable Edge Wear");
                if r.changed() { pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edge wear toggle".into(); }
                if mat.edge_wear.enabled {
                    let r = ui.add(egui::Slider::new(&mut mat.edge_wear.amount, 0.0..=1.0).text("Amount"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "EW amount".into(); }
                    let r = ui.add(egui::Slider::new(&mut mat.edge_wear.sharpness, 0.5..=10.0).text("Sharpness"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "EW sharpness".into(); }
                    let mut ec = linear_vec3_to_color32(mat.edge_wear.color);
                    ui.horizontal(|ui| {
                        ui.label("Color:");
                        if ui.color_edit_button_srgba(&mut ec).changed() {
                            mat.edge_wear.color = color32_to_linear_vec3(ec);
                            pctx.drag_started = true; pctx.changed = true; pctx.desc = "EW color".into();
                        }
                    });
                    let r = ui.add(egui::Slider::new(&mut mat.edge_wear.roughness, 0.0..=1.0).text("Roughness"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "EW roughness".into(); }
                    let r = ui.add(egui::Slider::new(&mut mat.edge_wear.metallic, 0.0..=1.0).text("Metallic"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "EW metallic".into(); }
                }
                ui.add_space(4.0);
                // -- Cavity Dirt (concave / recessed areas) --
                ui.label(egui::RichText::new("Cavity Dirt").strong().size(10.0).color(egui::Color32::from_rgb(140, 100, 70)));
                let r = ui.checkbox(&mut mat.cavity_dirt.enabled, "Enable Cavity Dirt");
                if r.changed() { pctx.drag_started = true; pctx.changed = true; pctx.desc = "Cavity dirt toggle".into(); }
                if mat.cavity_dirt.enabled {
                    let r = ui.add(egui::Slider::new(&mut mat.cavity_dirt.amount, 0.0..=1.0).text("Amount"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "CD amount".into(); }
                    let r = ui.add(egui::Slider::new(&mut mat.cavity_dirt.sharpness, 0.5..=10.0).text("Sharpness"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "CD sharpness".into(); }
                    let mut cc = linear_vec3_to_color32(mat.cavity_dirt.color);
                    ui.horizontal(|ui| {
                        ui.label("Color:");
                        if ui.color_edit_button_srgba(&mut cc).changed() {
                            mat.cavity_dirt.color = color32_to_linear_vec3(cc);
                            pctx.drag_started = true; pctx.changed = true; pctx.desc = "CD color".into();
                        }
                    });
                    let r = ui.add(egui::Slider::new(&mut mat.cavity_dirt.roughness, 0.0..=1.0).text("Roughness"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "CD roughness".into(); }
                    let r = ui.add(egui::Slider::new(&mut mat.cavity_dirt.metallic, 0.0..=1.0).text("Metallic"));
                    if r.drag_started() { pctx.drag_started = true; } if r.changed() { pctx.changed = true; pctx.desc = "CD metallic".into(); }
                }
            });
            } // ===== end embedded-material editor (if !linked) =====
            // --- Texture atlas controls ---
            if self.scene.primitives[idx].texture_id > 0 {
                ui.add_space(4.0); ui.separator();
                ui.label(egui::RichText::new("Texture").strong().size(11.0));
                ui.horizontal(|ui|{
                    ui.label(format!("Atlas ID: {}", self.scene.primitives[idx].texture_id));
                    if ui.small_button("Remove Texture").clicked() {
                        self.scene.primitives[idx].texture_id = 0;
                        pctx.drag_started = true; pctx.changed = true; pctx.desc = "Remove Texture".into();
                    }
                });
            }
            // --- AI Material (writes the EMBEDDED material — hidden when linked) ---
            if !linked {
            ui.add_space(4.0); ui.separator();
            ui.horizontal(|ui|{
                ui.label("AI:");
                let te = ui.add(egui::TextEdit::singleline(&mut self.ai.ai_mat_query).hint_text("brushed copper...").desired_width(120.0));
                if (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) || ui.small_button("Apply").clicked()) && !self.ai.ai_mat_query.is_empty() {
                    self.ai.ai_service.send(crate::ai_client::AiRequest::TextToProfile { description: self.ai.ai_mat_query.clone() });
                }
            });
            if ui.small_button("Analyze Image...").on_hover_text("CLIP material detection from reference image").clicked() {
                // Deferred dialog (file_dialog.rs — rfd modal-in-frame crash-class fix):
                // picker + file read run on a worker; poll forwards bytes to the AI service.
                let opened = self.file_dialogs.request(
                    "Analyze Reference Image",
                    &[("Image", &["png", "jpg", "jpeg", "bmp", "webp"])],
                    crate::file_dialog::DialogPurpose::AnalyzeImage,
                );
                if !opened { self.toast("A file dialog is already open"); }
            }
            if let Some(ref st) = self.ai.ai_service_status {
                ui.label(egui::RichText::new(st).small().weak());
            }
            } // end AI material (if !linked)
            // (The former second "🔗 Node Editor..." button was DELETED — it duplicated the
            // "Edit/Add Shader Graph" row above; two entry points to the same window.)

            // ============================================
            // Material DNA — Geometry-Responsive Rules
            // ============================================
            ui.add_space(4.0); ui.separator();
            let dna = &self.scene.primitives[idx].material_dna;
            let rule_count = dna.rules.len();
            let dna_label = if rule_count > 0 { format!("Material DNA ({} rules)", rule_count) } else { "Material DNA".into() };
            egui::CollapsingHeader::new(egui::RichText::new(dna_label).strong().size(11.0))
                .default_open(rule_count > 0)
                .show(ui, |ui| {
                // Age slider
                if rule_count > 0 {
                    ui.horizontal(|ui| {
                        ui.label("Age:");
                        let r = ui.add(egui::Slider::new(&mut self.scene.primitives[idx].material_dna.age, 0.0..=1.0).step_by(0.01));
                        if r.drag_started() { pctx.drag_started = true; }
                        if r.changed() { pctx.changed = true; pctx.desc = "DNA age".into(); }
                    });
                    ui.add_space(4.0);
                }

                // Rule stack (bottom-to-top display: first rule is base)
                let mut remove_idx: Option<usize> = None;
                let mut move_up_idx: Option<usize> = None;
                let mut move_down_idx: Option<usize> = None;
                let rules_len = self.scene.primitives[idx].material_dna.rules.len();
                for ri in 0..rules_len {
                    let rule = &mut self.scene.primitives[idx].material_dna.rules[ri];
                    let rule_id = egui::Id::new(("dna_rule", idx, ri));

                    egui::Frame::group(ui.style())
                        .fill(ui.visuals().extreme_bg_color)
                        .inner_margin(4.0)
                        .show(ui, |ui: &mut egui::Ui| {
                        // Header row: eye toggle, name, condition summary, delete
                        ui.horizontal(|ui: &mut egui::Ui| {
                            // Visibility toggle
                            let eye = if rule.visible { "V" } else { "H" };
                            if ui.small_button(eye).on_hover_text("Toggle visibility").clicked() {
                                rule.visible = !rule.visible;
                                pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into();
                            }
                            // Lock for base rule
                            if rule.locked { ui.label("B"); }
                            // Intensity
                            let r = ui.add(egui::DragValue::new(&mut rule.intensity).range(0.0..=2.0).speed(0.01).max_decimals(2).prefix(""));
                            if r.drag_started() { pctx.drag_started = true; }
                            if r.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                            // Name
                            let r = ui.add(egui::TextEdit::singleline(&mut rule.name).desired_width(80.0).font(egui::TextStyle::Small));
                            if r.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                            // Condition label
                            ui.weak(format!("[{}]", rule.condition.label()));
                            // Reorder buttons
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                                if ui.small_button("X").on_hover_text("Remove rule").clicked() { remove_idx = Some(ri); }
                                if ri + 1 < rules_len { if ui.small_button("^").on_hover_text("Move up").clicked() { move_up_idx = Some(ri); } }
                                if ri > 0 { if ui.small_button("v").on_hover_text("Move down").clicked() { move_down_idx = Some(ri); } }
                            });
                        });

                        // Expandable detail panel
                        egui::CollapsingHeader::new("Details")
                            .id_salt(rule_id)
                            .default_open(false)
                            .show(ui, |ui| {
                            // -- Condition editor --
                            ui.label(egui::RichText::new("Condition").strong().size(10.0));
                            let cond_types = ["Always", "Range", "Greater Than", "Less Than", "Direction", "Noise Threshold", "Altitude Band", "Boolean Zone"];
                            let cur_type = rule.condition.type_index() as usize;
                            egui::ComboBox::from_id_salt(("dna_cond_type", idx, ri))
                                .selected_text(cond_types[cur_type])
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for (ci, label) in cond_types.iter().enumerate() {
                                        if ui.selectable_label(cur_type == ci, *label).clicked() && cur_type != ci {
                                            rule.condition = match ci {
                                                0 => DnaCondition::Always,
                                                1 => DnaCondition::Range { input: DnaInput::Curvature, min: -1.0, max: 1.0, falloff: 0.1 },
                                                2 => DnaCondition::GreaterThan { input: DnaInput::Curvature, threshold: 0.3, falloff: 0.1 },
                                                3 => DnaCondition::LessThan { input: DnaInput::Curvature, threshold: -0.3, falloff: 0.1 },
                                                4 => DnaCondition::Direction { direction: glam::Vec3::Y, angle: 0.5, falloff: 0.1 },
                                                5 => DnaCondition::NoiseThreshold { scale: 5.0, threshold: 0.5, falloff: 0.1 },
                                                6 => DnaCondition::AltitudeBand { min_y: 0.0, max_y: 1.0, falloff: 0.1 },
                                                7 => DnaCondition::BooleanZone { blend_weight_min: 0.1 },
                                                _ => DnaCondition::Always,
                                            };
                                            pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into();
                                        }
                                    }
                                });

                            // Condition-specific parameters
                            match &mut rule.condition {
                                DnaCondition::Always => {},
                                DnaCondition::Range { input, min, max, falloff } => {
                                    Self::dna_input_combo(ui, input, idx, ri, "range");
                                    let r1 = ui.add(egui::Slider::new(min, -2.0..=2.0).text("Min").step_by(0.01));
                                    let r2 = ui.add(egui::Slider::new(max, -2.0..=2.0).text("Max").step_by(0.01));
                                    let r3 = ui.add(egui::Slider::new(falloff, 0.0..=1.0).text("Falloff").step_by(0.01));
                                    if r1.drag_started() || r2.drag_started() || r3.drag_started() { pctx.drag_started = true; }
                                    if r1.changed() || r2.changed() || r3.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                },
                                DnaCondition::GreaterThan { input, threshold, falloff } => {
                                    Self::dna_input_combo(ui, input, idx, ri, "gt");
                                    let r1 = ui.add(egui::Slider::new(threshold, -2.0..=2.0).text("Threshold").step_by(0.01));
                                    let r2 = ui.add(egui::Slider::new(falloff, 0.0..=1.0).text("Falloff").step_by(0.01));
                                    if r1.drag_started() || r2.drag_started() { pctx.drag_started = true; }
                                    if r1.changed() || r2.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                },
                                DnaCondition::LessThan { input, threshold, falloff } => {
                                    Self::dna_input_combo(ui, input, idx, ri, "lt");
                                    let r1 = ui.add(egui::Slider::new(threshold, -2.0..=2.0).text("Threshold").step_by(0.01));
                                    let r2 = ui.add(egui::Slider::new(falloff, 0.0..=1.0).text("Falloff").step_by(0.01));
                                    if r1.drag_started() || r2.drag_started() { pctx.drag_started = true; }
                                    if r1.changed() || r2.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                },
                                DnaCondition::Direction { direction, angle, falloff: _ } => {
                                    let r1 = ui.add(egui::Slider::new(&mut direction.x, -1.0..=1.0).text("Dir X").step_by(0.01));
                                    let r2 = ui.add(egui::Slider::new(&mut direction.y, -1.0..=1.0).text("Dir Y").step_by(0.01));
                                    let r3 = ui.add(egui::Slider::new(&mut direction.z, -1.0..=1.0).text("Dir Z").step_by(0.01));
                                    let r4 = ui.add(egui::Slider::new(angle, 0.0..=3.14).text("Angle (rad)").step_by(0.01));
                                    if r1.drag_started() || r2.drag_started() || r3.drag_started() || r4.drag_started() { pctx.drag_started = true; }
                                    if r1.changed() || r2.changed() || r3.changed() || r4.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                },
                                DnaCondition::NoiseThreshold { scale, threshold, falloff } => {
                                    let r1 = ui.add(egui::Slider::new(scale, 0.1..=100.0).text("Scale").step_by(0.1));
                                    let r2 = ui.add(egui::Slider::new(threshold, 0.0..=1.0).text("Threshold").step_by(0.01));
                                    let r3 = ui.add(egui::Slider::new(falloff, 0.0..=1.0).text("Falloff").step_by(0.01));
                                    if r1.drag_started() || r2.drag_started() || r3.drag_started() { pctx.drag_started = true; }
                                    if r1.changed() || r2.changed() || r3.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                },
                                DnaCondition::AltitudeBand { min_y, max_y, falloff } => {
                                    let r1 = ui.add(egui::Slider::new(min_y, -10.0..=10.0).text("Min Y").step_by(0.01));
                                    let r2 = ui.add(egui::Slider::new(max_y, -10.0..=10.0).text("Max Y").step_by(0.01));
                                    let r3 = ui.add(egui::Slider::new(falloff, 0.0..=1.0).text("Falloff").step_by(0.01));
                                    if r1.drag_started() || r2.drag_started() || r3.drag_started() { pctx.drag_started = true; }
                                    if r1.changed() || r2.changed() || r3.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                },
                                DnaCondition::BooleanZone { blend_weight_min } => {
                                    let r1 = ui.add(egui::Slider::new(blend_weight_min, 0.0..=1.0).text("Min Weight").step_by(0.01));
                                    if r1.drag_started() { pctx.drag_started = true; }
                                    if r1.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                },
                            }

                            // Age gate
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new("Age Gate").strong().size(10.0));
                            ui.horizontal(|ui| {
                                let r1 = ui.add(egui::DragValue::new(&mut rule.age_min).range(0.0..=1.0).speed(0.01).prefix("Min: "));
                                let r2 = ui.add(egui::DragValue::new(&mut rule.age_max).range(0.0..=1.0).speed(0.01).prefix("Max: "));
                                if r1.drag_started() || r2.drag_started() { pctx.drag_started = true; }
                                if r1.changed() || r2.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                            });

                            // -- Surface editor (per-property enable) --
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new("Surface Outputs").strong().size(10.0));
                            // Albedo
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut rule.surface.en_albedo, "Albedo").changed() { pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                if rule.surface.en_albedo {
                                    let mut c = linear_vec3_to_srgb_array(rule.surface.albedo);
                                    if ui.color_edit_button_rgb(&mut c).changed() {
                                        rule.surface.albedo = srgb_array_to_linear_vec3(c);
                                        pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into();
                                    }
                                }
                            });
                            // Roughness
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut rule.surface.en_roughness, "Roughness").changed() { pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                if rule.surface.en_roughness {
                                    let r = ui.add(egui::Slider::new(&mut rule.surface.roughness, 0.0..=1.0));
                                    if r.drag_started() { pctx.drag_started = true; }
                                    if r.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                }
                            });
                            // Metalness
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut rule.surface.en_metalness, "Metalness").changed() { pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                if rule.surface.en_metalness {
                                    let r = ui.add(egui::Slider::new(&mut rule.surface.metalness, 0.0..=1.0));
                                    if r.drag_started() { pctx.drag_started = true; }
                                    if r.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                }
                            });
                            // Emission
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut rule.surface.en_emission, "Emission").changed() { pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                if rule.surface.en_emission {
                                    let mut ec = linear_vec3_to_srgb_array(rule.surface.emission);
                                    if ui.color_edit_button_rgb(&mut ec).changed() {
                                        rule.surface.emission = srgb_array_to_linear_vec3(ec);
                                        pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into();
                                    }
                                }
                            });
                            // Height
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut rule.surface.en_height, "Height").changed() { pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                if rule.surface.en_height {
                                    let r = ui.add(egui::Slider::new(&mut rule.surface.height, -0.5..=0.5).step_by(0.005));
                                    if r.drag_started() { pctx.drag_started = true; }
                                    if r.changed() { pctx.changed = true; pctx.desc = "Edit DNA rule".into(); }
                                }
                            });

                            // Blend mode
                            ui.add_space(2.0);
                            egui::ComboBox::from_id_salt(("dna_blend", idx, ri))
                                .selected_text(rule.blend_mode.label())
                                .width(80.0)
                                .show_ui(ui, |ui| {
                                    for bm in DnaBlendMode::ALL {
                                        if ui.selectable_label(rule.blend_mode == bm, bm.label()).clicked()
                                            && rule.blend_mode != bm
                                        {
                                            rule.blend_mode = bm;
                                            pctx.drag_started = true; pctx.changed = true; pctx.desc = "Edit DNA rule".into();
                                        }
                                    }
                                });
                        });
                    });

                }

                // Handle reorder/remove
                if let Some(ri) = remove_idx {
                    self.transaction_snapshot_only("Remove DNA rule");
                    self.scene.primitives[idx].material_dna.rules.remove(ri);
                    pctx.changed = true; pctx.desc = "Remove DNA rule".into();
                }
                if let Some(ri) = move_up_idx {
                    self.transaction_snapshot_only("Reorder DNA rule");
                    self.scene.primitives[idx].material_dna.rules.swap(ri, ri + 1);
                    pctx.changed = true; pctx.desc = "Reorder DNA rule".into();
                }
                if let Some(ri) = move_down_idx {
                    self.transaction_snapshot_only("Reorder DNA rule");
                    self.scene.primitives[idx].material_dna.rules.swap(ri, ri - 1);
                    pctx.changed = true; pctx.desc = "Reorder DNA rule".into();
                }

                // Add rule button + presets
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("+ Add Rule").clicked() && rules_len < 8 {
                        self.transaction_snapshot_only("Add DNA rule");
                        self.scene.primitives[idx].material_dna.rules.push(DnaRule::default());
                        pctx.changed = true; pctx.desc = "Add DNA rule".into();
                    }
                    // Quick presets
                    egui::ComboBox::from_id_salt(("dna_preset", idx))
                        .selected_text("Preset...")
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(false, "Weathered Steel").clicked() {
                                self.transaction_snapshot_only("DNA preset: Weathered Steel");
                                let dna = Self::dna_preset_weathered_steel();
                                self.write_prim(idx, |p| p.material_dna = dna);
                                pctx.changed = true; pctx.desc = "DNA preset".into();
                            }
                            if ui.selectable_label(false, "Aged Wood").clicked() {
                                self.transaction_snapshot_only("DNA preset: Aged Wood");
                                let dna = Self::dna_preset_aged_wood();
                                self.write_prim(idx, |p| p.material_dna = dna);
                                pctx.changed = true; pctx.desc = "DNA preset".into();
                            }
                            if ui.selectable_label(false, "Snow Top").clicked() {
                                self.transaction_snapshot_only("DNA preset: Snow Top");
                                let dna = Self::dna_preset_snow_top();
                                self.write_prim(idx, |p| p.material_dna = dna);
                                pctx.changed = true; pctx.desc = "DNA preset".into();
                            }
                            if ui.selectable_label(false, "Clear").clicked() {
                                // Already empty DNA → no empty undo.
                                let empty = !self.scene.primitives[idx].material_dna.is_active()
                                    && self.scene.primitives[idx].material_dna.age.abs() < 1e-6;
                                if !empty {
                                    self.transaction_snapshot_only("Clear DNA");
                                    let dna = MaterialDna::default();
                                    self.write_prim(idx, |p| p.material_dna = dna);
                                    pctx.changed = true; pctx.desc = "Clear DNA".into();
                                }
                            }
                        });
                });
            });
        });
    }
}
