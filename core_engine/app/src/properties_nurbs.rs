//! NURBS body properties sub-panel — extracted from properties.rs.

use eframe::egui;
use glam::Vec3;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_props_nurbs(&mut self, ui: &mut egui::Ui) {
        // ---- NURBS Body properties (deferred-change pattern) ----
        if self.modeling_context == ModelingContext::Nurbs {
            let last_node = match self.sel.usel.last_clicked { Some(SceneItemId::Node(nid)) => Some(nid), _ => None };
            if let Some(nid) = self.sel.usel.active_node().or(last_node) {
                let snap = self.scene.scene_graph.get(nid).and_then(|node| {
                    if let core_math::scene_graph::NodePayload::NurbsBody { name, surface, material, .. } = &node.payload {
                        let (nu, nv) = surface.grid_size();
                        Some((name.clone(), surface.degree_u, surface.degree_v, nu, nv,
                              surface.knots_u.len(), surface.knots_v.len(),
                              surface.tess_tolerance,
                              node.local_position, node.local_rotation, node.local_scale, material.clone()))
                    } else { None }
                });
                if let Some((name, deg_u, deg_v, nu, nv, knots_u_len, knots_v_len, mut tess_tol, mut pos, mut rot, mut scl, mut mat)) = snap {
                    ui.colored_label(egui::Color32::from_rgb(200, 140, 255), format!("\u{1F4D0} {}", name));
                    ui.separator();
                    let mut xform_changed = false;
                    egui::CollapsingHeader::new(egui::RichText::new("\u{1F4D0} Transform").strong().size(13.0))
                        .default_open(true).show(ui, |ui| {
                        for (label, val) in [("X", &mut pos.x), ("Y", &mut pos.y), ("Z", &mut pos.z)] {
                            let r = ui.add(egui::Slider::new(val, -20.0..=20.0).text(format!("Pos {}", label)));
                            // Episode only on real change — drag_started click-release was empty undo.
                            if r.changed() {
                                if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                                xform_changed = true;
                            }
                        }
                        for (label, val) in [("X", &mut rot.x), ("Y", &mut rot.y), ("Z", &mut rot.z)] {
                            let r = ui.add(egui::Slider::new(val, -180.0..=180.0).text(format!("Rot {}", label)));
                            if r.changed() {
                                if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                                xform_changed = true;
                            }
                        }
                        for (label, val) in [("X", &mut scl.x), ("Y", &mut scl.y), ("Z", &mut scl.z)] {
                            let r = ui.add(egui::Slider::new(val, 0.01..=10.0).text(format!("Scl {}", label)));
                            if r.changed() {
                                if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                                xform_changed = true;
                            }
                        }
                    });
                    let mut tess_changed = false;
                    egui::CollapsingHeader::new(egui::RichText::new("\u{1F4D0} Surface Info").strong().size(13.0))
                        .default_open(true).show(ui, |ui| {
                        ui.label(format!("Degree: {} x {}", deg_u, deg_v));
                        ui.label(format!("CVs: {} x {}", nu, nv));
                        ui.label(format!("Knots U: {}", knots_u_len));
                        ui.label(format!("Knots V: {}", knots_v_len));
                        ui.add_space(4.0);
                        let r = ui.add(egui::Slider::new(&mut tess_tol, 0.1..=4.0)
                            .logarithmic(true).text("Tess Quality"));
                        if r.changed() {
                            if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                            tess_changed = true;
                        }
                        ui.weak("Lower = finer tessellation");
                    });
                    // CV Weight editor (shown when CVs are selected)
                    let sel_cvs: Vec<(u32, u32)> = self.nurbs_sel.selected_cvs.iter().copied().collect();
                    let mut weight_changed = false;
                    if !sel_cvs.is_empty() {
                        // Snapshot average weight of selected CVs
                        let avg_weight = self.scene.scene_graph.get(nid).map(|node| {
                            if let core_math::scene_graph::NodePayload::NurbsBody { surface, .. } = &node.payload {
                                let sum: f32 = sel_cvs.iter().filter_map(|&(r, c)| {
                                    surface.control_points.get(r as usize)
                                        .and_then(|row| row.get(c as usize))
                                        .map(|cp| cp.weight)
                                }).sum();
                                sum / sel_cvs.len() as f32
                            } else { 1.0 }
                        }).unwrap_or(1.0);
                        let mut w = avg_weight;
                        egui::CollapsingHeader::new(egui::RichText::new("\u{2699} CV Weight").strong().size(13.0))
                            .default_open(true).show(ui, |ui| {
                            ui.label(format!("{} CV(s) selected", sel_cvs.len()));
                            let r = ui.add(egui::Slider::new(&mut w, 0.01..=10.0)
                                .logarithmic(true).text("Weight"));
                            if r.changed() {
                                if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                                weight_changed = true;
                            }
                        });
                        if weight_changed {
                            if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                                if let core_math::scene_graph::NodePayload::NurbsBody { surface, .. } = &mut n.payload {
                                    let surface = std::sync::Arc::make_mut(surface);
                                    for &(r, c) in &sel_cvs {
                                        if let Some(row) = surface.control_points.get_mut(r as usize) {
                                            if let Some(cp) = row.get_mut(c as usize) {
                                                cp.weight = w;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let mut mat_changed = false;
                    egui::CollapsingHeader::new(egui::RichText::new("\u{1F3A8} Material").strong().size(13.0))
                        .default_open(false).show(ui, |ui| {
                        let mut c = [mat.albedo[0], mat.albedo[1], mat.albedo[2]];
                        if ui.color_edit_button_rgb(&mut c).changed() {
                            if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                            mat.albedo = Vec3::from_array(c); mat_changed = true;
                        }
                        let r = ui.add(egui::Slider::new(&mut mat.roughness, 0.0..=1.0).text("Roughness"));
                        if r.changed() {
                            if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                            mat_changed = true;
                        }
                        let r = ui.add(egui::Slider::new(&mut mat.metallic, 0.0..=1.0).text("Metallic"));
                        if r.changed() {
                            if !self.has_active_episode() { self.begin_episode("Edit NURBS"); }
                            mat_changed = true;
                        }
                    });
                    if xform_changed {
                        if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                            n.local_position = pos; n.local_rotation = rot; n.local_scale = scl;
                        }
                        self.mark_transform_dirty();
                    }
                    if tess_changed || weight_changed {
                        if tess_changed {
                            if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                                if let core_math::scene_graph::NodePayload::NurbsBody { surface, .. } = &mut n.payload {
                                    let surface = std::sync::Arc::make_mut(surface);
                                    surface.tess_tolerance = tess_tol;
                                }
                            }
                        }
                        self.nurbs_sync_mesh_cache(nid);
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if mat_changed {
                        if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                            if let core_math::scene_graph::NodePayload::NurbsBody { material: m, .. } = &mut n.payload { *m = mat; }
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    ui.add_space(8.0);
                }

                // ── NurbsSolid: selected Fillet face → Adjust / Remove anytime (P4) ──
                if matches!(
                    self.scene.scene_graph.get(nid).map(|n| &n.payload),
                    Some(core_math::scene_graph::NodePayload::NurbsSolid { .. })
                ) {
                    self.draw_props_nurbs_solid_fillet(ui, nid);
                }
            }
        }
    }

    /// Plasticity parity: select a Fillet/FilletLinear/FilletLaw face → show radius,
    /// [Adjust] (re-solve), [Remove Fillet] (un-shrink). Undoable transactions.
    fn draw_props_nurbs_solid_fillet(
        &mut self,
        ui: &mut egui::Ui,
        solid: core_math::scene_graph::NodeId,
    ) {
        use core_math::nurbs::FaceOrigin;
        use core_math::nurbs_fillet::{
            adjust_one_planar_fillet_radius, remove_one_planar_fillet,
        };

        // Prefer explicitly selected face; else first selected edge's face.
        let face_id = self
            .nurbs_sel
            .selected_faces
            .iter()
            .copied()
            .next()
            .or_else(|| {
                self.nurbs_sel
                    .selected_edges
                    .iter()
                    .map(|(id, _)| *id)
                    .next()
            });
        let Some(face_id) = face_id else {
            return;
        };
        let Some(fi) = self.nurbs_face_index(solid, face_id) else {
            return;
        };

        let (origin_label, display_r, is_fillet) = {
            let Some(node) = self.scene.scene_graph.get(solid) else {
                return;
            };
            let core_math::scene_graph::NodePayload::NurbsSolid { faces, .. } = &node.payload
            else {
                return;
            };
            let Some(face) = faces.get(fi as usize) else {
                return;
            };
            let is_fillet = matches!(
                face.origin,
                FaceOrigin::Fillet { .. }
                    | FaceOrigin::FilletLinear { .. }
                    | FaceOrigin::FilletLaw { .. }
            );
            let label = match &face.origin {
                FaceOrigin::Fillet { .. } => "Fillet (const)",
                FaceOrigin::FilletLinear { .. } => "Fillet (linear)",
                FaceOrigin::FilletLaw { .. } => "Fillet (law)",
                FaceOrigin::Corner { .. } => "Corner",
                FaceOrigin::Chamfer { .. } => "Chamfer",
                FaceOrigin::Base => "Base",
            };
            (label, face.origin.display_radius(), is_fillet)
        };
        if !is_fillet {
            return;
        }
        let mut r = display_r.unwrap_or(0.1);
        let max_r = 0.5f32;

        ui.separator();
        egui::CollapsingHeader::new(
            egui::RichText::new(format!("\u{25C8} {origin_label}")).strong().size(13.0),
        )
        .default_open(true)
        .show(ui, |ui| {
            ui.label(format!("Face id {}", face_id.0));
            let resp = ui.add(
                egui::Slider::new(&mut r, 0.005..=max_r)
                    .text("Radius")
                    .logarithmic(false),
            );
            if resp.drag_stopped() || (resp.lost_focus() && resp.changed()) {
                let solid = solid;
                let fid = face_id;
                let new_r = r;
                self.transaction("Adjust Fillet", |s| {
                    if let Some(node) = s.scene.scene_graph.get_mut(solid) {
                        if let core_math::scene_graph::NodePayload::NurbsSolid { faces, .. } =
                            &mut node.payload
                        {
                            match adjust_one_planar_fillet_radius(faces, fid, new_r) {
                                Ok(_) => {
                                    s.nurbs_sync_mesh_cache(solid);
                                    s.nurbs_prune_selection(solid);
                                    s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                                }
                                Err(e) => {
                                    use core_math::nurbs_fillet::FilletError;
                                    let msg = match e {
                                        FilletError::CurvedFace { reason } => {
                                            format!("Adjust Fillet: {reason}")
                                        }
                                        other => format!("Adjust Fillet failed: {other:?}"),
                                    };
                                    s.toast(&msg);
                                }
                            }
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                if ui
                    .button("Remove Fillet")
                    .on_hover_text(
                        "Un-shrink support faces, delete fillet + corners — sharp edge restored",
                    )
                    .clicked()
                {
                    let solid = solid;
                    let fid = face_id;
                    self.transaction("Remove Fillet", |s| {
                        if let Some(node) = s.scene.scene_graph.get_mut(solid) {
                            if let core_math::scene_graph::NodePayload::NurbsSolid {
                                faces, ..
                            } = &mut node.payload
                            {
                                match remove_one_planar_fillet(faces, fid) {
                                    Ok(()) => {
                                        s.nurbs_clear_selection();
                                        s.nurbs_sync_mesh_cache(solid);
                                        s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                                        s.toast("Fillet removed — sharp edge restored");
                                    }
                                    Err(e) => {
                                        use core_math::nurbs_fillet::FilletError;
                                        let msg = match e {
                                            FilletError::CurvedFace { reason } => {
                                                format!("Remove Fillet: {reason}")
                                            }
                                            other => {
                                                format!("Remove Fillet failed: {other:?}")
                                            }
                                        };
                                        s.toast(&msg);
                                    }
                                }
                            }
                        }
                    });
                }
            });
            ui.weak("Plasticity parity — edit/delete anytime after commit");
        });
    }
}
