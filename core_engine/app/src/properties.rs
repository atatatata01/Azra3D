//! Properties panel — SDF, poly, NURBS, light, viewport — moved from ui_panels.rs.

use eframe::egui;
use core_math::primitives::{PbrMaterial, DnaRule, DnaCondition, DnaInput, DnaSurface, MaterialDna};
use crate::*;

impl AzraApp {
    pub(crate) fn draw_properties_section(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // ---- PROPERTIES ----
        ui.add_space(4.0); ui.separator();
        // Multi-selection badge
        let sel_count = self.sel.usel.selected.len();
        if sel_count > 1 {
            ui.colored_label(egui::Color32::from_rgb(200, 180, 60),
                format!("{} items selected", sel_count));
        }
        ui.colored_label(egui::Color32::from_gray(140), "Properties");
        ui.separator();
        // Multi-prim Blend K — same diamond drag targets, for when several prims are selected.
        let multi_blend: Vec<usize> = self.selected_prim_indices_unified().into_iter()
            .filter(|&i| crate::edge_detect::prim_accepts_blend_handle(&self.scene, i))
            .filter(|&i| {
                matches!(
                    crate::edge_detect::prim_handle_mode(&self.scene.primitives[i]),
                    crate::edge_detect::BlendHandleMode::BlendK
                )
            })
            .collect();
        if multi_blend.len() > 1 {
            ui.label(egui::RichText::new(format!("Blend K — {} prims", multi_blend.len()))
                .strong().color(egui::Color32::from_rgb(60, 150, 230)));
            let mut k = self.scene.primitives[multi_blend[0]].operation.blend_k();
            let k_max = self.tweak.blend_k_max.max(0.1);
            let r = ui.add(
                egui::Slider::new(&mut k, 0.0..=k_max)
                    .text("Blend")
                    .logarithmic(k_max > 2.0),
            );
            if r.drag_started() && !self.has_active_episode() {
                self.begin_episode("Multi blend K");
            }
            if r.changed() {
                for &idx in &multi_blend {
                    if idx < self.scene.primitives.len() {
                        crate::edge_detect::op_set_k_clamped(
                            &mut self.scene.primitives[idx].operation, k, k_max);
                        self.mirror_prim_to_graph(idx);
                    }
                }
                self.rebake_no_undo();
            }
            if r.drag_stopped() {
                if self.has_active_episode() { self.commit_episode(); }
            }
            ui.add_space(4.0);
            ui.separator();
        }
        // Use active prim, falling back to last_clicked prim for "sticky" properties
        let active_prim_idx = self.sel.usel.active_prim_uid()
            .and_then(|uid| self.prim_index_by_uid(uid))
            .or_else(|| {
                match self.sel.usel.last_clicked {
                    Some(SceneItemId::Prim(uid)) => self.prim_index_by_uid(uid),
                    _ => None,
                }
            });
        let mut pctx = properties_sdf::PropsContext {
            changed: false,
            desc: String::new(),
            drag_started: false,
            new_bool_op: None,
            new_mat: None,
            idx: active_prim_idx,
            add_slice: false,
            slice_duplicate: false,
            new_shared_mat_from_prim: false,
            jump_to_material: None,
        };
        // Cap properties scroll height so Render/Env sections below are always visible
        let avail = ui.available_height();
        let props_max = (avail * 0.45).max(200.0).min(500.0);
        egui::ScrollArea::vertical().id_salt("props_scroll").max_height(props_max).show(ui, |ui| {
            self.draw_props_light(ui);

            self.draw_props_poly(ui);

            self.draw_props_nurbs(ui);

            // Node-level properties (when a graph node is selected, no prim active):
            // a Material node shows the material editor; otherwise SdfBody properties.
            if pctx.idx.is_none() {
                if !self.draw_props_material_node(ctx, ui) {
                    self.draw_props_body(ui);
                }
            }

            self.draw_props_sdf(ctx, ui, &mut pctx);
        });
        self.draw_props_viewport(ui);
        // Apply deferred property changes
        // Slice tools: add a HalfSpace cutter (forced Subtract) or slice-duplicate the active body.
        if pctx.add_slice {
            self.add_primitive_with_op(
                core_math::primitives::SdfShape::HalfSpace,
                glam::Vec3::ZERO,
                core_math::primitives::BooleanOp::Subtract { k: 0.0 },
            );
        }
        if pctx.slice_duplicate {
            if let Some(idx) = pctx.idx { self.slice_duplicate(idx); }
        }
        if let Some(op) = pctx.new_bool_op {
            if let Some(idx) = pctx.idx {
                if idx < self.scene.primitives.len() {
                    self.push_cmd(AppCommand::SetBooleanOp(self.scene.primitives[idx].prim_uid, op));
                }
            }
        }
        // Material combo deferred actions (see PropsContext docs).
        if pctx.new_shared_mat_from_prim {
            if let Some(idx) = pctx.idx { self.create_shared_material_from_prim(idx); }
        }
        if let Some(mid) = pctx.jump_to_material {
            if let Some(nid) = self.scene.scene_graph.material_node_id(mid) {
                self.select_single_unified(SceneItemId::Node(nid));
                self.outliner_find_selected();
            }
        }
        if let Some(key) = pctx.new_mat {
            if let Some(idx) = pctx.idx { if idx < self.scene.primitives.len() {
                let m = PbrMaterial::preset(&key);
                self.transaction("Material preset", |s| {
                    s.write_prim(idx, |p| p.material = m);
                    s.mark_dirty(DirtyFlags::RENDER);
                });
            }}
        }
        if pctx.changed {
            if pctx.drag_started && !self.has_active_episode() {
                // First frame of slider drag → start episode
                self.begin_episode(&pctx.desc);
            } else if !pctx.drag_started && !self.has_active_episode() {
                // Button click (no drag) → atomic snapshot
                // Graph not yet updated (mirror below), so snapshot is pre-mutation
                self.transaction_snapshot_only(&pctx.desc);
            }
            // Mirror flat cache → graph for the changed prim
            if let Some(idx) = pctx.idx {
                self.mirror_prim_to_graph(idx);
                if idx < self.scene.primitives.len() {
                    let uid = self.scene.primitives[idx].prim_uid;
                    if self.is_master(idx) { self.sync_instances_of(uid); }
                }
            }
            self.rebake_no_undo();
        }
    }

    pub(crate) fn dna_input_combo(ui: &mut egui::Ui, input: &mut DnaInput, prim_idx: usize, rule_idx: usize, salt: &str) {
        egui::ComboBox::from_id_salt(("dna_input", prim_idx, rule_idx, salt))
            .selected_text(input.label())
            .width(100.0)
            .show_ui(ui, |ui| {
                for di in DnaInput::ALL {
                    // Re-clicking the active option is a no-op (caller may snapshot on any change).
                    if ui.selectable_label(*input == di, di.label()).clicked() && *input != di {
                        *input = di;
                    }
                }
            });
    }

    pub(crate) fn dna_preset_weathered_steel() -> MaterialDna {
        use glam::Vec3;
        MaterialDna {
            age: 0.5,
            rules: vec![
                DnaRule {
                    name: "Base Steel".into(), condition: DnaCondition::Always, locked: true,
                    surface: DnaSurface { albedo: Vec3::new(0.7, 0.7, 0.72), roughness: 0.3, metalness: 1.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    ..Default::default()
                },
                DnaRule {
                    name: "Machining Marks".into(),
                    condition: DnaCondition::NoiseThreshold { scale: 20.0, threshold: 0.4, falloff: 0.1 },
                    surface: DnaSurface { albedo: Vec3::new(0.68, 0.68, 0.7), roughness: 0.15, metalness: 1.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    intensity: 0.3, ..Default::default()
                },
                DnaRule {
                    name: "Edge Wear".into(),
                    condition: DnaCondition::GreaterThan { input: DnaInput::Curvature, threshold: 0.4, falloff: 0.15 },
                    surface: DnaSurface { albedo: Vec3::new(0.8, 0.8, 0.75), roughness: 0.6, metalness: 0.9, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    age_min: 0.1, ..Default::default()
                },
                DnaRule {
                    name: "Rust".into(),
                    condition: DnaCondition::GreaterThan { input: DnaInput::Curvature, threshold: 0.3, falloff: 0.2 },
                    surface: DnaSurface { albedo: Vec3::new(0.45, 0.22, 0.08), roughness: 0.85, metalness: 0.3, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    intensity: 0.8, age_min: 0.4, ..Default::default()
                },
                DnaRule {
                    name: "Cavity Dirt".into(),
                    condition: DnaCondition::LessThan { input: DnaInput::Curvature, threshold: -0.3, falloff: 0.2 },
                    surface: DnaSurface { albedo: Vec3::new(0.05, 0.04, 0.03), roughness: 0.95, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    age_min: 0.2, ..Default::default()
                },
            ],
        }
    }

    pub(crate) fn dna_preset_aged_wood() -> MaterialDna {
        use glam::Vec3;
        MaterialDna {
            age: 0.5,
            rules: vec![
                DnaRule {
                    name: "Base Wood".into(), condition: DnaCondition::Always, locked: true,
                    surface: DnaSurface { albedo: Vec3::new(0.4, 0.26, 0.13), roughness: 0.7, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    ..Default::default()
                },
                DnaRule {
                    name: "Wood Grain".into(),
                    condition: DnaCondition::NoiseThreshold { scale: 8.0, threshold: 0.3, falloff: 0.15 },
                    surface: DnaSurface { albedo: Vec3::new(0.3, 0.18, 0.08), roughness: 0.75, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    intensity: 0.5, ..Default::default()
                },
                DnaRule {
                    name: "Edge Wear".into(),
                    condition: DnaCondition::GreaterThan { input: DnaInput::Curvature, threshold: 0.5, falloff: 0.15 },
                    surface: DnaSurface { albedo: Vec3::new(0.5, 0.35, 0.2), roughness: 0.5, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    age_min: 0.2, ..Default::default()
                },
                DnaRule {
                    name: "Moisture Stain".into(),
                    condition: DnaCondition::LessThan { input: DnaInput::Curvature, threshold: -0.2, falloff: 0.3 },
                    surface: DnaSurface { albedo: Vec3::new(0.15, 0.1, 0.05), roughness: 0.9, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    age_min: 0.3, ..Default::default()
                },
            ],
        }
    }

    pub(crate) fn dna_preset_snow_top() -> MaterialDna {
        use glam::Vec3;
        MaterialDna {
            age: 0.5,
            rules: vec![
                DnaRule {
                    name: "Base Rock".into(), condition: DnaCondition::Always, locked: true,
                    surface: DnaSurface { albedo: Vec3::new(0.35, 0.33, 0.3), roughness: 0.8, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    ..Default::default()
                },
                DnaRule {
                    name: "Snow (top)".into(),
                    condition: DnaCondition::Direction { direction: Vec3::Y, angle: 0.6, falloff: 0.1 },
                    surface: DnaSurface { albedo: Vec3::new(0.95, 0.95, 0.97), roughness: 0.4, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    intensity: 0.9, ..Default::default()
                },
                DnaRule {
                    name: "Moss (cavity)".into(),
                    condition: DnaCondition::LessThan { input: DnaInput::Curvature, threshold: -0.4, falloff: 0.2 },
                    surface: DnaSurface { albedo: Vec3::new(0.1, 0.25, 0.08), roughness: 0.9, metalness: 0.0, en_albedo: true, en_roughness: true, en_metalness: true, ..Default::default() },
                    intensity: 0.6, age_min: 0.3, ..Default::default()
                },
            ],
        }
    }
}

