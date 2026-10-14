//! Polygonal body properties sub-panel — extracted from properties.rs.

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use crate::*;

impl AzraApp {
    /// Parametric primitive block — the reason poly primitives are now re-parameterizable at all.
    ///
    /// Shown only while the body's mesh still matches the spec that built it (`poly_live_spec`).
    /// Once any tool edits the topology the block disappears and says why, rather than offering
    /// sliders that would silently rebuild the mesh and destroy those edits.
    fn draw_props_poly_primitive(&mut self, ui: &mut egui::Ui, nid: core_math::scene_graph::NodeId) {
        let Some(mut spec) = self.poly_live_spec(nid) else {
            if self.poly_spec_went_stale(nid) {
                ui.label(
                    egui::RichText::new("Edited — no longer parametric")
                        .size(10.5)
                        .color(crate::ui_theme::TEXT_FAINT),
                );
                ui.separator();
            }
            return;
        };

        let mut applied = false;
        egui::CollapsingHeader::new(
            egui::RichText::new(format!("\u{25C8} {}", spec.label()))
                .strong()
                .size(13.0),
        )
        .default_open(true)
        .show(ui, |ui| {
            let e = crate::poly::primitive_params_ui::draw_primitive_spec_params(ui, &mut spec);
            if e.changed {
                self.set_primitive_spec(nid, spec);
            }
            // ONE undo entry per drag, on release — same lifecycle as the F9 panel and the Pipe
            // modifier, not one entry per frame of slider movement.
            if e.released {
                self.transaction_snapshot_only(&format!("Adjust {}", spec.label()));
            }
            ui.add_space(4.0);
            if ui
                .button("Make Editable")
                .on_hover_text(
                    "Bake these parameters into the mesh. The shape stays exactly as it is, but \
                     the parameters go away and the topology becomes freely editable.",
                )
                .clicked()
            {
                applied = true;
            }
        });

        if applied {
            self.poly_make_editable(nid);
            self.transaction_snapshot_only("Make Editable");
            self.toast("Primitive applied — mesh is now editable");
        }
        ui.separator();
    }

    pub(crate) fn draw_props_poly(&mut self, ui: &mut egui::Ui) {
        // ---- Polygonal Body properties (deferred-change pattern) ----
        if self.modeling_context == ModelingContext::Polygonal {
            let last_node = match self.sel.usel.last_clicked { Some(SceneItemId::Node(nid)) => Some(nid), _ => None };
            if let Some(nid) = self.sel.usel.active_node().or(last_node) {
                // Snapshot ends the scene borrow before the rest of the panel mutates self.
                // Body-level material = per-face material_id (object assignment writes ALL faces).
                //
                // D0 / dense-mesh: never re-scan all faces every frame — at 1.5M quads the O(faces)
                // `all()` was ~7 ms of scoreboard `lay=` (UI-layout-bound false tri-bound). Cache by
                // Arc identity (COW mutates → new ptr).
                let snap = self.scene.scene_graph.get(nid).and_then(|node| {
                    let core_math::scene_graph::NodePayload::PolygonalBody {
                        name, mesh, material, ..
                    } = &node.payload
                    else {
                        return None;
                    };
                    let first_mid = mesh.faces.first().map(|f| f.material_id).unwrap_or(0);
                    let mesh_key = std::sync::Arc::as_ptr(mesh) as usize;
                    Some((
                        name.clone(),
                        mesh.vertices.len(),
                        mesh.half_edges.len(),
                        mesh.faces.len(),
                        node.local_position,
                        node.local_rotation,
                        node.local_scale,
                        material.clone(),
                        first_mid,
                        mesh_key,
                        // Defer uniform scan until after this borrow ends (cache hit is free).
                        std::sync::Arc::clone(mesh),
                    ))
                });
                if let Some((
                    name,
                    vert_count,
                    he_count,
                    face_count,
                    mut pos,
                    mut rot,
                    mut scl,
                    mut mat,
                    first_mid,
                    mesh_key,
                    mesh_arc,
                )) = snap
                {
                    let mid_uniform = match self.ui.poly_mat_uniform_cache {
                        Some((cnid, ckey, cmid, cuni))
                            if cnid == nid && ckey == mesh_key && cmid == first_mid =>
                        {
                            cuni
                        }
                        _ => {
                            let u = mesh_arc.faces.iter().all(|f| f.material_id == first_mid);
                            self.ui.poly_mat_uniform_cache = Some((nid, mesh_key, first_mid, u));
                            u
                        }
                    };
                    drop(mesh_arc);
                    ui.colored_label(egui::Color32::from_rgb(100, 200, 255), format!("\u{25B3} {}", name));
                    ui.separator();
                    self.draw_props_poly_primitive(ui, nid);
                    let mut xform_changed = false;
                    egui::CollapsingHeader::new(egui::RichText::new("\u{1F4D0} Transform").strong().size(13.0))
                        .default_open(true).show(ui, |ui| {
                        for (label, val) in [("X", &mut pos.x), ("Y", &mut pos.y), ("Z", &mut pos.z)] {
                            let r = ui.add(egui::Slider::new(val, -20.0..=20.0).text(format!("Pos {}", label)));
                            // Episode only on real change — drag_started click-release was empty undo.
                            if r.changed() {
                                if !self.has_active_episode() { self.begin_episode("Transform Poly"); }
                                xform_changed = true;
                            }
                        }
                        for (label, val) in [("X", &mut rot.x), ("Y", &mut rot.y), ("Z", &mut rot.z)] {
                            let r = ui.add(egui::Slider::new(val, -180.0..=180.0).text(format!("Rot {}", label)));
                            if r.changed() {
                                if !self.has_active_episode() { self.begin_episode("Transform Poly"); }
                                xform_changed = true;
                            }
                        }
                        for (label, val) in [("X", &mut scl.x), ("Y", &mut scl.y), ("Z", &mut scl.z)] {
                            let r = ui.add(egui::Slider::new(val, 0.01..=10.0).text(format!("Scl {}", label)));
                            if r.changed() {
                                if !self.has_active_episode() { self.begin_episode("Transform Poly"); }
                                xform_changed = true;
                            }
                        }
                    });
                    egui::CollapsingHeader::new(egui::RichText::new("\u{25B3} Mesh Info").strong().size(13.0))
                        .default_open(true).show(ui, |ui| {
                        ui.label(format!("Vertices: {}", vert_count));
                        ui.label(format!("Edges: {}", he_count / 2));
                        ui.label(format!("Faces: {}", face_count));
                    });
                    // Sub-element selection info (edit modes)
                    if self.poly_work_mode != PolyWorkMode::Object {
                        let nv = self.poly_sel.selected_vertices.len();
                        let ne = self.poly_sel.selected_edges.len();
                        let nf = self.poly_sel.selected_faces.len();
                        if nv > 0 || ne > 0 || nf > 0 {
                            ui.add_space(2.0);
                            ui.colored_label(egui::Color32::from_rgb(200, 180, 100),
                                format!("{} verts  {} edges  {} faces", nv, ne, nf));
                        }
                    }
                    // Bevel Preview section (edge mode only)
                    if self.poly_work_mode == PolyWorkMode::Edge {
                        egui::CollapsingHeader::new(egui::RichText::new("\u{25C6} Bevel Preview").strong().size(13.0))
                            .default_open(true).show(ui, |ui| {
                            let mut active = self.bevel_pv.active;
                            if ui.checkbox(&mut active, "Active (Ctrl+B)").changed() {
                                self.bevel_pv.active = active;
                                if active {
                                    // Snapshot current selection on activation
                                    self.bevel_pv.stored_edges = self.poly_sel.selected_edges.clone();
                                    self.bevel_pv.needs_edge_upload = true;
                                } else {
                                    self.bevel_pv.stored_edges.clear();
                                    self.bevel_pv.edge_count = 0;
                                }
                            }
                            if self.bevel_pv.active {
                                if ui.add(egui::Slider::new(&mut self.bevel_pv.width, 0.001..=0.5).text("Width").logarithmic(true)).changed() {
                                    self.bevel_pv.needs_edge_upload = true;
                                }
                                if ui.add(egui::Slider::new(&mut self.bevel_pv.depth, -0.2..=0.2).text("Depth")).changed() {
                                    self.bevel_pv.needs_param_upload = true;
                                }
                                if ui.add(egui::Slider::new(&mut self.bevel_pv.shape, 0.0..=1.0).text("Shape")).changed() {
                                    self.bevel_pv.needs_param_upload = true;
                                }
                                let profiles = ["Chamfer", "Arc", "Ogee", "Step"];
                                let mut idx = self.bevel_pv.profile as usize;
                                if egui::ComboBox::from_label("Profile").show_index(ui, &mut idx, profiles.len(), |i| profiles[i]).changed() {
                                    self.bevel_pv.profile = match idx {
                                        0 => graphics::bevel_preview::BevelProfile::Chamfer,
                                        1 => graphics::bevel_preview::BevelProfile::Arc,
                                        2 => graphics::bevel_preview::BevelProfile::Ogee,
                                        _ => graphics::bevel_preview::BevelProfile::Step,
                                    };
                                    self.bevel_pv.needs_param_upload = true;
                                }
                                ui.label(format!("Edges: {}", self.bevel_pv.edge_count));
                                ui.separator();
                                if ui.button("Apply Bevel (Enter)").clicked() {
                                    self.poly_confirm_bevel();
                                }
                            }
                        });
                    }
                    let mut mat_changed = false;
                    let ai_albedo = !self.ai.ai_tex_maps_png[0].is_empty();
                    let mut poly_map_outcome = crate::material_editor::MaterialEditOutcome::default();
                    egui::CollapsingHeader::new(egui::RichText::new("\u{1F3A8} Material").strong().size(13.0))
                        .default_open(false).show(ui, |ui| {
                        // --- Material assignment combo (body-level: writes ALL faces) ---
                        let linked = mid_uniform && first_mid > 0
                            && self.scene.scene_graph.material_by_id(core_math::primitives::MaterialId(first_mid)).is_some();
                        {
                            let mut graph_mats: Vec<(u32, String)> = self.scene.scene_graph
                                .materials()
                                .map(|(_, mid, name, _)| (mid.0, name.to_string()))
                                .collect();
                            graph_mats.sort_by_key(|(mid, _)| *mid);
                            let current_label = if !mid_uniform {
                                "Mixed (per-face)".to_string()
                            } else if first_mid == 0 {
                                "Embedded (local)".to_string()
                            } else {
                                self.scene.scene_graph.material_by_id(core_math::primitives::MaterialId(first_mid))
                                    .map(|(_, m)| if m.name.is_empty() { format!("Material {}", first_mid) } else { m.name.clone() })
                                    .unwrap_or_else(|| format!("Missing ({})", first_mid))
                            };
                            ui.horizontal(|ui| {
                                ui.label("Material:");
                                egui::ComboBox::from_id_salt("poly_mat_ref").width(140.0).selected_text(&current_label)
                                    .show_ui(ui, |ui| {
                                        if ui.selectable_label(mid_uniform && first_mid == 0, "Embedded (local)").clicked()
                                            && !(mid_uniform && first_mid == 0)
                                        {
                                            self.assign_material_to_poly_body(nid, 0);
                                        }
                                        for (gmid, gname) in &graph_mats {
                                            let label = if gname.is_empty() { format!("Material {}", gmid) } else { gname.clone() };
                                            let is_cur = mid_uniform && first_mid == *gmid;
                                            if ui.selectable_label(is_cur, &label).clicked() && !is_cur {
                                                self.assign_material_to_poly_body(nid, *gmid);
                                            }
                                        }
                                        ui.separator();
                                        if ui.selectable_label(false, "\u{2795} New Material")
                                            .on_hover_text("Create a shared material from this body's current look and assign it")
                                            .clicked()
                                        {
                                            let new_name = self.next_unique_name("Material");
                                            let seed = mat.clone();
                                            // Create + assign in ONE transaction → one Ctrl+Z.
                                            self.transaction("New Material", |s| {
                                                let mid = s.scene.scene_graph.add_material(&new_name, seed);
                                                s.apply_material_all_faces_no_undo(nid, mid.0);
                                                s.mark_dirty(DirtyFlags::RENDER);
                                                s.outliner.outliner_dirty = true;
                                            });
                                            self.toast(&format!("{} created and assigned", new_name));
                                        }
                                    });
                                if linked {
                                    if ui.small_button("\u{270F}")
                                        .on_hover_text("Edit this material (opens its editor / selects it in the outliner)")
                                        .clicked()
                                    {
                                        if let Some(mnid) = self.scene.scene_graph.material_node_id(core_math::primitives::MaterialId(first_mid)) {
                                            self.select_single_unified(SceneItemId::Node(mnid));
                                            self.outliner_find_selected();
                                        }
                                    }
                                }
                            });
                            if linked {
                                ui.label(egui::RichText::new(
                                    "Shared material — the embedded editor below is hidden (the shared material overrides it).")
                                    .weak().italics().size(10.0));
                            }
                            ui.add_space(2.0);
                        }
                        // Scene material assignments summary
                        {
                            let face_mat_summary: Vec<(u32, usize)> = if let Some(n) = self.scene.scene_graph.get(nid) {
                                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh: m, .. } = &n.payload {
                                    let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
                                    for f in &m.faces { *counts.entry(f.material_id).or_insert(0) += 1; }
                                    counts.into_iter().collect()
                                } else { Vec::new() }
                            } else { Vec::new() };
                            let has_assigned = face_mat_summary.iter().any(|&(mid, _)| mid > 0);
                            if !has_assigned {
                                ui.label(egui::RichText::new("No material assigned").weak().italics());
                            } else {
                                for &(mid, count) in &face_mat_summary {
                                    if mid == 0 { continue; }
                                    let graph_mat = self.scene.scene_graph.material_by_id(core_math::primitives::MaterialId(mid));
                                    let name = graph_mat
                                        .map(|(_, m)| m.name.clone())
                                        .unwrap_or_else(|| format!("Material {}", mid));
                                    let color = graph_mat
                                        .map(|(_, m)| egui::Color32::from_rgb(
                                            (m.albedo.x * 255.0) as u8,
                                            (m.albedo.y * 255.0) as u8,
                                            (m.albedo.z * 255.0) as u8))
                                        .unwrap_or(egui::Color32::GRAY);
                                    ui.horizontal(|ui| {
                                        let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                                        ui.painter().rect_filled(r, 2.0, color);
                                        ui.label(format!("{} ({} faces)", name, count));
                                    });
                                }
                            }
                            ui.label(egui::RichText::new("Ctrl+M to assign material").weak().size(10.0));
                            ui.add_space(4.0);
                        }
                        // Shared unified PBR editor (base color/rough/metal + albedo map row + AI button).
                        // Hidden when a shared material is linked — the resolver ignores the embedded
                        // material then (dead sliders would mislead; edit the shared material instead).
                        if !linked {
                            // Albedo + data-map thumbs; poly meshes carry UVs → uv_allowed = true.
                            let ctx = ui.ctx().clone();
                            let albedo_thumb = if mat.albedo_map.is_set() {
                                self.map_slot_thumb(&ctx, mat.albedo_map.tex_id, false)
                            } else { None };
                            let data_thumbs = {
                                use core_math::pbr_material::MapKind;
                                let mut t = [None; 7];
                                for (i, kind) in MapKind::DATA_SLOTS.iter().enumerate() {
                                    let m = mat.map(*kind);
                                    if m.is_set() {
                                        t[i] = self.map_slot_thumb(&ctx, m.tex_id, kind.is_data());
                                    }
                                }
                                t
                            };
                            let mut o = crate::material_editor::draw_pbr_material(ui, &mut mat, ai_albedo, albedo_thumb, true);
                            o.merge(crate::material_editor::draw_shader_mode_and_advanced(
                                ui,
                                &mut mat,
                                nid.0 as u64,
                            ));
                            o.merge(crate::material_editor::draw_data_map_slots(ui, &mut mat, data_thumbs, true));
                            // Episode only on real change (not drag_started click-release).
                            if o.changed {
                                if !self.has_active_episode() { self.begin_episode("Change Material"); }
                                mat_changed = true;
                            }
                            poly_map_outcome = o;
                        }
                    });
                    // Albedo-map intents (file load / AI maps) → deferred upload targeting this body.
                    self.handle_body_albedo_map_outcome(&poly_map_outcome, nid);
                    // Apply deferred changes. Episode already opened on first changed() above;
                    // if somehow missing, open a snapshot so typing path still undoes (WORK_ORDER #8).
                    if xform_changed {
                        if self.pending_episode.is_none() {
                            self.transaction_snapshot_only("Transform Poly");
                        }
                        if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                            n.local_position = pos; n.local_rotation = rot; n.local_scale = scl;
                        }
                        self.mark_transform_dirty();
                    }
                    if mat_changed {
                        if self.pending_episode.is_none() {
                            self.transaction_snapshot_only("Change Material");
                        }
                        if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                            if let core_math::scene_graph::NodePayload::PolygonalBody { material: m, .. } = &mut n.payload { *m = mat; }
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }

                    // Poly modifier stack also lives on Properties → Modifiers (unified panel).
                    self.draw_poly_modifier_stack(ui, nid, false);
                    ui.add_space(8.0);
                }
            }
        }
    }

    /// Shared poly display-mesh modifier stack (Object page + Properties → Modifiers wrench).
    ///
    /// `default_open`: true on the dedicated Modifiers tab so the stack is visible immediately.
    /// Blender-parity normal control for one poly body.
    ///
    /// Two independent controls (same split as Blender): the per-body **Auto Smooth angle**
    /// (Shade Flat = 0°, Shade Smooth = 180°, anything between = Smooth by Angle) and the per-edge
    /// **Mark Sharp** bits, which break the smooth fan whatever the angle is.
    ///
    /// UNDO: the buttons go through `transaction()`; the angle slider runs an
    /// `begin_episode`/`commit_episode` drag so a scrub is ONE Ctrl+Z, not one per frame.
    fn draw_poly_normals_section(
        &mut self,
        ui: &mut egui::Ui,
        nid: core_math::scene_graph::NodeId,
        default_open: bool,
    ) {
        let (mut angle, authored, sharp_edges) = match self.poly_cd(nid) {
            Some(cd) => (cd.auto_smooth_angle(), cd.auto_smooth_deg.is_some(), cd.sharp.len()),
            None => (core_math::poly_mesh::AUTO_SMOOTH_ANGLE_DEG, false, 0),
        };
        // Deferred actions — egui closures borrow `ui`, so nothing mutates `self` inside them.
        let mut act_smooth = false;
        let mut act_flat = false;
        let mut act_reset = false;
        let mut act_mark = false;
        let mut act_clear = false;
        let mut act_clear_all = false;
        let mut act_from_tags = false;
        let mut angle_drag_start = false;
        let mut angle_changed = false;
        let mut angle_drag_end = false;
        let selected_edges = self.poly_sel.selected_edges.len();

        egui::CollapsingHeader::new(
            egui::RichText::new("\u{1F506} Shading (Normals)")
                .strong()
                .size(13.0)
                .color(egui::Color32::from_rgb(255, 205, 120)),
        )
        .id_salt(if default_open { "poly_normals_props_tab" } else { "poly_normals_object" })
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Shade Smooth")
                    .on_hover_text("Smooth across every edge (Auto Smooth 180\u{b0}).\n\
                                    Edges you marked Sharp stay hard. (Ctrl+Shift+N)")
                    .clicked()
                {
                    act_smooth = true;
                }
                if ui
                    .button("Shade Flat")
                    .on_hover_text("Every face keeps its own normal (Auto Smooth 0\u{b0}). (Ctrl+Alt+N)")
                    .clicked()
                {
                    act_flat = true;
                }
                if ui
                    .add_enabled(authored, egui::Button::new("Reset"))
                    .on_hover_text(format!(
                        "Drop the authored angle and go back to the {:.0}\u{b0} default.",
                        core_math::poly_mesh::AUTO_SMOOTH_ANGLE_DEG
                    ))
                    .clicked()
                {
                    act_reset = true;
                }
            });
            let resp = ui.add(
                egui::Slider::new(&mut angle, 0.0..=180.0)
                    .text("Auto Smooth")
                    .suffix("\u{b0}"),
            );
            if resp.drag_started() {
                angle_drag_start = true;
            }
            if resp.changed() {
                angle_changed = true;
            }
            if resp.drag_stopped() || resp.lost_focus() {
                angle_drag_end = true;
            }
            ui.label(
                egui::RichText::new(
                    "Faces meeting within this angle shade smooth; sharper edges stay hard.",
                )
                .size(10.0)
                .weak(),
            );

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(selected_edges > 0, egui::Button::new("Mark Sharp"))
                    .on_hover_text("Force the selected edges hard, whatever the angle. (Ctrl+Shift+E)")
                    .clicked()
                {
                    act_mark = true;
                }
                if ui
                    .add_enabled(selected_edges > 0, egui::Button::new("Clear Sharp"))
                    .on_hover_text("Remove the hard-edge mark from the selected edges. (Ctrl+Alt+E)")
                    .clicked()
                {
                    act_clear = true;
                }
                if ui
                    .add_enabled(sharp_edges > 0, egui::Button::new("Clear All"))
                    .on_hover_text("Remove EVERY sharp mark on this mesh.")
                    .clicked()
                {
                    act_clear_all = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(if sharp_edges == 0 {
                        "No sharp edges".to_string()
                    } else {
                        format!("{sharp_edges} sharp corners")
                    })
                    .size(10.0)
                    .weak(),
                );
                if ui
                    .button("From Smart Select")
                    .on_hover_text(
                        "Mark every edge an operation CREATED (bevel strips, boolean seams \u{2026}) \
                         as sharp \u{2014} reuses the provenance tags the modelling ops already write.",
                    )
                    .clicked()
                {
                    act_from_tags = true;
                }
            });
        });

        // ---- apply (outside the egui borrow) ----
        if act_smooth {
            self.poly_shade_smooth();
        }
        if act_flat {
            self.poly_shade_flat();
        }
        if act_reset {
            self.poly_shade_auto_smooth_default();
        }
        if act_mark {
            self.poly_set_sharp(true);
        }
        if act_clear {
            self.poly_set_sharp(false);
        }
        if act_clear_all {
            self.transaction("Clear All Sharp", |s| {
                s.poly_cd_mut(nid).sharp = core_math::poly_mesh::BitLayer::new();
                s.overlay_lines_fp = u64::MAX;
                s.poly_force_display_resync(nid);
                s.mark_dirty(crate::DirtyFlags::RENDER);
            });
        }
        if act_from_tags {
            self.poly_mark_sharp_from_smart_select();
        }
        // Angle scrub: ONE undo entry per drag (episode), live preview every frame.
        if angle_drag_start {
            self.begin_episode("Auto Smooth Angle");
        }
        if angle_changed {
            self.poly_write_auto_smooth_angle(nid, Some(angle));
        }
        if angle_drag_end {
            self.commit_episode();
        }
    }

    pub(crate) fn draw_poly_modifier_stack(
        &mut self,
        ui: &mut egui::Ui,
        nid: core_math::scene_graph::NodeId,
        default_open: bool,
    ) {
        use core_math::scene_graph::{PolyModifier, PolyModifierType};

        // Blender-style normal control (Shade Smooth / Shade Flat / Auto Smooth / Mark Sharp).
        // Drawn ABOVE the modifier stack: it authors the BASE body's shading, not a stack entry.
        self.draw_poly_normals_section(ui, nid, default_open);

        let mut mods: Vec<PolyModifier> = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| {
                if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                    &n.payload
                {
                    Some(modifiers.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        // Named polygroups for the geometry-channel picker. Collected UP FRONT because `mods` is a
        // clone (so `self` stays free here) — the picker must show the user the names they
        // authored, not a raw id they would have to guess.
        let channel_groups: Vec<(u16, String)> = self
            .poly_cd(nid)
            .map(|cd| {
                let mut g: Vec<(u16, String)> =
                    cd.group_meta.iter().map(|m| (m.id, m.name.clone())).collect();
                g.sort_by_key(|(id, _)| *id);
                g
            })
            .unwrap_or_default();
        let mut mods_changed = false;
        let mut pending_copy = false;
        let mut pending_paste = false;
        // MULTI-OBJECT MODIFIER ADD (owner part 2): every OTHER selected polygonal body
        // gets the same freshly-added modifier. Collected up front so the add closure can
        // stay a pure `mods.push`. `nid` itself is excluded — it goes through `mods`.
        let extra_targets: Vec<core_math::scene_graph::NodeId> = self
            .sel
            .usel
            .selected_nodes()
            .filter(|&n| n != nid)
            .filter(|&n| {
                matches!(
                    self.scene.scene_graph.get(n).map(|x| &x.payload),
                    Some(core_math::scene_graph::NodePayload::PolygonalBody { .. })
                )
            })
            .collect();
        let mut add_to_extras: Option<PolyModifierType> = None;
        let mut apply_idx: Option<usize> = None;
        let mut edit_sculpt_mi: Option<usize> = None;

        let open_id = if default_open {
            "poly_mod_stack_props_tab"
        } else {
            "poly_mod_stack_object"
        };
        egui::CollapsingHeader::new(
            egui::RichText::new("\u{1F527} Mesh Modifiers (Poly)")
                .strong()
                .size(13.0)
                .color(egui::Color32::from_rgb(100, 200, 255)),
        )
        .id_salt(open_id)
        .default_open(default_open)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Display mesh stack — Subdivide / Mirror / Bevel / Array / Duplicate / \u{2026}",
                )
                    .size(10.0)
                    .weak(),
            );
            ui.horizontal(|ui| {
                // Searchable 2-column picker (owner: see more modifiers at once + filter).
                // LIVE apply arms only (mesh_sync::apply_poly_modifiers).
                const POLY_MOD_TYPES: &[(&str, PolyModifierType)] = &[
                    ("Subdivide", PolyModifierType::Subdivide),
                    ("Mirror", PolyModifierType::Mirror),
                    ("Solidify", PolyModifierType::Solidify),
                    ("Smooth (Laplacian)", PolyModifierType::Smooth),
                    ("Bend", PolyModifierType::Bend),
                    ("Triangulate", PolyModifierType::Triangulate),
                    ("Decimate", PolyModifierType::Decimate),
                    ("Duplicate", PolyModifierType::Duplicate),
                    ("Array (Linear)", PolyModifierType::Array),
                    ("Array (Circular)", PolyModifierType::ArrayCircular),
                    ("Pipe", PolyModifierType::Pipe),
                    ("Scatter", PolyModifierType::Scatter),
                    ("Inset", PolyModifierType::Inset),
                    ("Vertex Merge", PolyModifierType::VertexMerge),
                    ("Mesh Cleanup", PolyModifierType::MeshCleanup),
                    ("Bevel", PolyModifierType::Bevel),
                    ("Boolean", PolyModifierType::Boolean),
                    ("Weighted Normal", PolyModifierType::WeightedNormal),
                    ("Smooth Normal", PolyModifierType::SmoothNormal),
                    ("Remesh (Voxel)", PolyModifierType::Remesh),
                    ("Sculpt", PolyModifierType::Sculpt),
                ];
                let search_mem_id = egui::Id::new(("poly_mod_add_search", open_id));
                let focus_once_id = search_mem_id.with("focus_once");
                egui::ComboBox::from_id_salt(format!("add_poly_mod_{open_id}"))
                    .selected_text("+ Add Modifier")
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        // Wide enough for two name columns + search.
                        ui.set_min_width(320.0);
                        ui.set_max_width(340.0);

                        let mut query = ui.ctx().data(|d| {
                            d.get_temp::<String>(search_mem_id).unwrap_or_default()
                        });
                        let search_edit = ui.add(
                            egui::TextEdit::singleline(&mut query)
                                .hint_text("Search modifiers\u{2026}")
                                .desired_width(f32::INFINITY)
                                .id_salt(("poly_mod_add_te", open_id)),
                        );
                        // Focus search the first time this popup paints so typing filters
                        // immediately. Reset on pick so the next open focuses again.
                        let already_focused = ui
                            .ctx()
                            .data(|d| d.get_temp::<bool>(focus_once_id).unwrap_or(false));
                        if !already_focused {
                            search_edit.request_focus();
                            ui.ctx()
                                .data_mut(|d| d.insert_temp(focus_once_id, true));
                        }
                        ui.ctx().data_mut(|d| d.insert_temp(search_mem_id, query.clone()));

                        let q = query.trim().to_ascii_lowercase();
                        let filtered: Vec<(&str, PolyModifierType)> = POLY_MOD_TYPES
                            .iter()
                            .copied()
                            .filter(|(name, _)| {
                                q.is_empty() || name.to_ascii_lowercase().contains(&q)
                            })
                            .collect();

                        if filtered.is_empty() {
                            ui.label(
                                egui::RichText::new("No match")
                                    .weak()
                                    .italics()
                                    .size(11.0),
                            );
                        } else {
                            const COL_W: f32 = 152.0;
                            const ROW_H: f32 = 22.0;
                            // Cap height so a long list scrolls; two columns halves the rows.
                            let rows = (filtered.len() + 1) / 2;
                            let list_h = ((rows as f32) * (ROW_H + 2.0)).clamp(ROW_H, 280.0);
                            egui::ScrollArea::vertical()
                                .max_height(list_h)
                                .show(ui, |ui| {
                                    for pair in filtered.chunks(2) {
                                        ui.horizontal(|ui| {
                                            for (name, mt) in pair {
                                                if ui
                                                    .add_sized(
                                                        [COL_W, ROW_H],
                                                        egui::Button::new(*name),
                                                    )
                                                    .clicked()
                                                {
                                                    let mut m = PolyModifier::new(*mt);
                                                    if *mt == PolyModifierType::Mirror {
                                                        if let Some(null_nid) = self
                                                            .maybe_create_mirror_center_from_workplane()
                                                        {
                                                            m.mirror_object_nid = null_nid.0;
                                                        }
                                                    }
                                                    mods.push(m);
                                                    if !extra_targets.is_empty() {
                                                        add_to_extras = Some(*mt);
                                                    }
                                                    mods_changed = true;
                                                    // Reset filter + focus for next open.
                                                    ui.ctx().data_mut(|d| {
                                                        d.insert_temp(
                                                            search_mem_id,
                                                            String::new(),
                                                        );
                                                        d.insert_temp(focus_once_id, false);
                                                    });
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                    }
                                });
                        }
                    });
                let can_paste =
                    matches!(&self.clipboard.content, crate::clipboard::ClipboardContent::PolyMod(_));
                if ui
                    .add_enabled(can_paste, egui::Button::new("Paste Modifier"))
                    .on_hover_text(
                        "Append the copied modifier to this mesh's stack (Ctrl+V).\n\
                         Local-space fields (Pipe path, circular-array centre, array offset) \
                         are rebased into this body's space; a Boolean keeps the same cutter.",
                    )
                    .clicked()
                {
                    pending_paste = true;
                }
                if !extra_targets.is_empty() {
                    ui.weak(format!("+{} more selected", extra_targets.len()))
                        .on_hover_text(
                            "Adding a modifier adds it to every selected polygon mesh.",
                        );
                }
                if ui
                    .button("Mesh \u{2192} SDF")
                    .on_hover_text(
                        "Convert this poly body to an SDF volume (existing pipeline).\n\
                         Non-destructive link: Outliner right-click \u{2192} Link as SDF modifier.",
                    )
                    .clicked()
                {
                    self.convert_poly_to_sdf(nid);
                }
            });

            let mod_count = mods.len();
            let mut remove_idx: Option<usize> = None;
            let mut copy_mi: Option<usize> = None;
            let mut swap_pair: Option<(usize, usize)> = None;
            let mut select_mi: Option<usize> = None;
            // Drop stale selection if stack shrank or body changed.
            if let Some((snid, si)) = self.ui.poly_mod_selected {
                if snid != nid || si >= mod_count {
                    self.ui.poly_mod_selected = None;
                }
            }
            // Gate 2: before → after vert/face stats per card (prefix evaluation).
            // Memoized by (nid, base content signature, stack hash) so scrubbing a slider
            // re-evaluates once after change, not every frame on a frozen stack.
            let prefix_stats: Vec<(u32, u32, u32, u32)> = {
                let base_mesh = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                    core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => {
                        Some(mesh.clone())
                    }
                    _ => None,
                });
                match base_mesh {
                    Some(base) => {
                        let key = (
                            nid.0,
                            base.content_signature(),
                            Self::poly_modifier_stack_hash(&mods),
                        );
                        let hit = self
                            .ui
                            .poly_mod_prefix_stats
                            .as_ref()
                            .filter(|(k, _)| *k == key)
                            .map(|(_, s)| s.clone());
                        if let Some(s) = hit {
                            s
                        } else if Self::poly_stack_needs_tagged_eval(&mods) {
                            // Tagged Smart-Bevel / Mesh-Cleanup stacks never build a ladder
                            // or stamp stats — manual walk (pre-existing approximation:
                            // evaluates without tags).
                            let stats = self.poly_mod_stats_uncached(nid, &base, &mods);
                            self.ui.poly_mod_prefix_stats = Some((key, stats.clone()));
                            stats
                        } else {
                            // ONE shared evaluation, not a second full stack run:
                            // apply_poly_modifiers_cached stamps this memo as a byproduct
                            // of its walk (counts captured before eviction can drop the
                            // ladder, so heavy over-budget stacks cannot degrade back to
                            // an uncached walk). When the display sync already built the
                            // ladder this is a full-hit hash walk — no mesh work at all.
                            let cutters = self.poly_ref_meshes_for(nid, &mods);
                            // Panel stats path: no fresh display-cache key here, so recompute.
                            let _ =
                                self.apply_poly_modifiers_cached(nid, &base, &mods, &cutters, None);
                            let stats = self
                                .ui
                                .poly_mod_prefix_stats
                                .as_ref()
                                .filter(|(k, _)| *k == key)
                                .map(|(_, s)| s.clone())
                                .unwrap_or_else(|| {
                                    // Defensive only — the stamp above always matches `key`.
                                    self.poly_mod_stats_uncached(nid, &base, &mods)
                                });
                            self.ui.poly_mod_prefix_stats = Some((key, stats.clone()));
                            stats
                        }
                    }
                    None => vec![(0, 0, 0, 0); mods.len()],
                }
            };

            // Master enable (owner: "Alt+H could hide all modifiers — need a checkmark at the top").
            // Checked = every stack entry is live; click toggles ALL on or ALL off. Per-card
            // checkboxes still work independently after that.
            if mod_count > 0 {
                ui.horizontal(|ui| {
                    let all_on = mods.iter().all(|m| m.enabled);
                    let any_on = mods.iter().any(|m| m.enabled);
                    let mut master = all_on;
                    let r = ui
                        .checkbox(&mut master, "")
                        .on_hover_text(
                            "Enable / disable ALL modifiers in this stack.\n\
                             Unchecked = hide the whole stack (base mesh only).\n\
                             Checked = every entry live. Mixed state shows unchecked — \
                             click once to enable all.",
                        );
                    if r.changed() {
                        // If mixed (some on), first click from unchecked path enables all
                        // when master was set true; when user unchecks, disable all.
                        let turn_on = master;
                        for m in mods.iter_mut() {
                            m.enabled = turn_on;
                        }
                        mods_changed = true;
                    } else if !all_on && any_on {
                        // Visual hint for mixed: draw a dim partial label (egui checkbox is
                        // binary; the empty box + label communicates "not all on").
                        ui.label(
                            egui::RichText::new("All (mixed)")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(160, 150, 100)),
                        )
                        .on_hover_text(
                            "Some modifiers are off. Click the checkbox to enable all, \
                             or uncheck individual cards.",
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(if all_on {
                                "All enabled"
                            } else {
                                "All hidden"
                            })
                            .size(11.0)
                            .weak(),
                        );
                    }
                });
                ui.add_space(2.0);
            }

            for mi in 0..mod_count {
                let mt = mods[mi].mod_type;
                let is_sel = self.ui.poly_mod_selected == Some((nid, mi));
                let card_col = if is_sel {
                    egui::Color32::from_rgba_unmultiplied(255, 160, 40, 55)
                } else {
                    match mt {
                        PolyModifierType::Subdivide => {
                            egui::Color32::from_rgba_unmultiplied(100, 200, 255, 25)
                        }
                        PolyModifierType::Mirror => {
                            egui::Color32::from_rgba_unmultiplied(100, 255, 200, 25)
                        }
                        PolyModifierType::Solidify => {
                            egui::Color32::from_rgba_unmultiplied(255, 200, 100, 20)
                        }
                        PolyModifierType::Smooth => {
                            egui::Color32::from_rgba_unmultiplied(100, 255, 150, 20)
                        }
                        PolyModifierType::Bevel => {
                            egui::Color32::from_rgba_unmultiplied(200, 150, 255, 20)
                        }
                        PolyModifierType::Inset => {
                            egui::Color32::from_rgba_unmultiplied(120, 220, 255, 25)
                        }
                        PolyModifierType::Boolean => {
                            egui::Color32::from_rgba_unmultiplied(255, 120, 120, 25)
                        }
                        PolyModifierType::WeightedNormal | PolyModifierType::SmoothNormal => {
                            egui::Color32::from_rgba_unmultiplied(180, 140, 255, 25)
                        }
                        PolyModifierType::Duplicate => {
                            egui::Color32::from_rgba_unmultiplied(120, 200, 255, 25)
                        }
                        PolyModifierType::Array | PolyModifierType::ArrayCircular => {
                            egui::Color32::from_rgba_unmultiplied(255, 180, 80, 25)
                        }
                        PolyModifierType::Scatter => {
                            egui::Color32::from_rgba_unmultiplied(140, 255, 120, 25)
                        }
                        PolyModifierType::VertexMerge => {
                            egui::Color32::from_rgba_unmultiplied(255, 200, 140, 25)
                        }
                        PolyModifierType::MeshCleanup => {
                            egui::Color32::from_rgba_unmultiplied(200, 220, 160, 25)
                        }
                        PolyModifierType::Remesh => {
                            egui::Color32::from_rgba_unmultiplied(100, 180, 255, 30)
                        }
                        PolyModifierType::Bend => {
                            egui::Color32::from_rgba_unmultiplied(255, 150, 200, 25)
                        }
                        PolyModifierType::Sculpt => {
                            egui::Color32::from_rgba_unmultiplied(255, 140, 90, 30)
                        }
                        _ => egui::Color32::from_rgba_unmultiplied(180, 180, 180, 15),
                    }
                };
                let frame = egui::Frame::none()
                    .fill(card_col)
                    .inner_margin(4.0)
                    .rounding(4.0)
                    .stroke(if is_sel {
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 170, 50))
                    } else {
                        egui::Stroke::NONE
                    });
                let resp = frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let mut en = mods[mi].enabled;
                            if ui.checkbox(&mut en, "").changed() {
                                mods[mi].enabled = en;
                                mods_changed = true;
                            }
                            let title = format!("{}. {}", mi + 1, mods[mi].label());
                            if ui
                                .selectable_label(is_sel, egui::RichText::new(title).strong())
                                .on_hover_text(
                                    "Click to select — viewport shows edit wire \
                                     (Boolean: cutter wire in op colour)",
                                )
                                .clicked()
                            {
                                select_mi = Some(mi);
                            }
                            // Gate 2: verts / faces before → after this stack entry.
                            if let Some(&(bv, bf, av, af)) = prefix_stats.get(mi) {
                                let same = bv == av && bf == af;
                                let stat = if same {
                                    format!("{av}v {af}f")
                                } else {
                                    format!("{bv}\u{2192}{av}v  {bf}\u{2192}{af}f")
                                };
                                ui.label(
                                    egui::RichText::new(stat)
                                        .size(10.0)
                                        .color(if same {
                                            egui::Color32::from_rgb(130, 135, 145)
                                        } else {
                                            egui::Color32::from_rgb(100, 200, 255)
                                        }),
                                )
                                .on_hover_text(
                                    "Topology before \u{2192} after this modifier \
                                     (disabled entries show unchanged counts)",
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("X").on_hover_text("Remove").clicked() {
                                        remove_idx = Some(mi);
                                    }
                                    if mt == PolyModifierType::Sculpt {
                                        let editing =
                                            self.sculpt.poly_edit == Some((nid, mi));
                                        let lab = if editing { "Done" } else { "Edit" };
                                        if ui
                                            .small_button(lab)
                                            .on_hover_text(
                                                "Enter PolySculpt — use the sculpt brushes \
                                                 on this mesh (LMB stroke). Done leaves brush mode.",
                                            )
                                            .clicked()
                                        {
                                            edit_sculpt_mi = Some(mi);
                                        }
                                    }
                                    // Apply = bake only THIS card into the base mesh and remove it.
                                    if ui
                                        .small_button("Apply")
                                        .on_hover_text(
                                            "Bake this modifier into the base mesh and remove it \
                                             from the stack.\n\
                                             Modifiers above stay as cards but turn off (already \
                                             included in the bake). Modifiers below stay live.",
                                        )
                                        .clicked()
                                    {
                                        apply_idx = Some(mi);
                                    }
                                    // Copy this modifier (same clipboard Ctrl+C uses).
                                    if ui
                                        .small_button("\u{2398}")
                                        .on_hover_text(
                                            "Copy this modifier (Ctrl+C when selected) — \
                                             paste into another mesh's stack",
                                        )
                                        .clicked()
                                    {
                                        copy_mi = Some(mi);
                                    }
                                    // RESULT WIREFRAME (owner 2026-08-04: "for the modifier
                                    // I need to be able to see the result wireframe, add a
                                    // toggle here"). Binds the existing GPU wire overlay —
                                    // which draws the EVALUATED display mesh, i.e. the
                                    // modifier stack's result — rather than inventing a
                                    // second wire path. Same flag on every card, so the
                                    // highlight always tells the truth about the state.
                                    let wire_on = self.ui.poly_mod_result_wire;
                                    // The draw is EDIT-MODE ONLY (owner 2026-08-08). Show that
                                    // in the icon: lit orange only when it is actually drawing,
                                    // dimmed-but-still-toggleable in Object mode with a tooltip
                                    // saying why. An "on" highlight over a suppressed draw is
                                    // exactly the lie this button already told once, when it set
                                    // a flag the draw additionally gated on a selected card.
                                    let wire_drawing =
                                        wire_on && self.poly_work_mode != PolyWorkMode::Object;
                                    let wire_btn = egui::RichText::new("\u{25a6}").color(
                                        if wire_drawing {
                                            egui::Color32::from_rgb(255, 190, 80)
                                        } else if wire_on {
                                            egui::Color32::from_rgb(150, 120, 60) // on, but Object mode
                                        } else {
                                            egui::Color32::from_rgb(150, 155, 170)
                                        },
                                    );
                                    let wire_resp = ui
                                        .small_button(wire_btn)
                                        .on_hover_text(if self.poly_work_mode == PolyWorkMode::Object {
                                            "Show the modifier RESULT as wireframe \u{2014} \
                                             Edit mode only (Tab into the mesh to see it)\n\
                                             Right-click: colour / width / opacity"
                                        } else {
                                            "Show the modifier RESULT as wireframe \
                                             (viewport wire overlay)\n\
                                             Right-click: colour / width / opacity"
                                        });
                                    if wire_resp.clicked() {
                                        self.ui.poly_mod_result_wire = !wire_on;
                                        self.mark_dirty(DirtyFlags::RENDER);
                                    }
                                    // RIGHT-CLICK = style popup (owner 2026-08-08: "right click
                                    // on the wire frame button bring setting to control the
                                    // modifier wireframe so we can set colour, size"). Left-click
                                    // stays the pure on/off verb.
                                    wire_resp.azra_context_menu(|ui| {
                                        ui.label(
                                            egui::RichText::new("Modifier result wire")
                                                .strong()
                                                .size(12.0),
                                        );
                                        ui.separator();
                                        let mut changed = false;
                                        ui.horizontal(|ui| {
                                            changed |= ui
                                                .color_edit_button_rgb(&mut self.render.mod_wire_color)
                                                .changed();
                                            ui.label("Colour");
                                        });
                                        changed |= ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut self.render.mod_wire_thickness,
                                                    0.5..=5.0,
                                                )
                                                .text("Width")
                                                .suffix(" px"),
                                            )
                                            .changed();
                                        changed |= ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut self.render.mod_wire_opacity,
                                                    0.0..=1.0,
                                                )
                                                .text("Opacity"),
                                            )
                                            .changed();
                                        ui.separator();
                                        if ui.button("Reset to black").clicked() {
                                            self.render.mod_wire_color = [0.0, 0.0, 0.0];
                                            self.render.mod_wire_opacity = 0.95;
                                            self.render.mod_wire_thickness = 1.25;
                                            changed = true;
                                        }
                                        if changed {
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    });
                                    if mi + 1 < mod_count {
                                        if ui.small_button("v").on_hover_text("Move down").clicked()
                                        {
                                            swap_pair = Some((mi, mi + 1));
                                        }
                                    }
                                    if mi > 0 {
                                        if ui.small_button("^").on_hover_text("Move up").clicked() {
                                            swap_pair = Some((mi, mi - 1));
                                        }
                                    }
                                },
                            );
                        });
                        match mt {
                            PolyModifierType::Subdivide => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].subdiv_levels, 1..=4)
                                            .text("Levels"),
                                    )
                                    .changed();
                            }
                            PolyModifierType::Mirror => {
                                // Bitmask encoding (X=1, Y=2, Z=4) — same as MirrorAxes.
                                ui.horizontal(|ui| {
                                    let mut mx = (mods[mi].mirror_axis & 1) != 0;
                                    let mut my = (mods[mi].mirror_axis & 2) != 0;
                                    let mut mz = (mods[mi].mirror_axis & 4) != 0;
                                    if ui.checkbox(&mut mx, "X").changed() {
                                        mods[mi].mirror_axis ^= 1;
                                        mods_changed = true;
                                    }
                                    if ui.checkbox(&mut my, "Y").changed() {
                                        mods[mi].mirror_axis ^= 2;
                                        mods_changed = true;
                                    }
                                    if ui.checkbox(&mut mz, "Z").changed() {
                                        mods[mi].mirror_axis ^= 4;
                                        mods_changed = true;
                                    }
                                });
                                mods_changed |=
                                    ui.checkbox(&mut mods[mi].mirror_merge, "Merge").changed();
                                if mods[mi].mirror_merge {
                                    mods_changed |= ui
                                        .add(
                                            egui::Slider::new(
                                                &mut mods[mi].mirror_merge_threshold,
                                                0.0001..=0.1,
                                            )
                                            .text("Threshold")
                                            .logarithmic(true),
                                        )
                                        .changed();
                                }
                                // Mirror center object (Empty / null / any node origin).
                                let center_label = if mods[mi].mirror_object_nid == 0 {
                                    "Local origin".to_string()
                                } else {
                                    let cnid =
                                        core_math::scene_graph::NodeId(mods[mi].mirror_object_nid);
                                    self.scene
                                        .scene_graph
                                        .get(cnid)
                                        .map(|n| n.payload.display_name().to_string())
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or_else(|| {
                                            format!("#{}", mods[mi].mirror_object_nid)
                                        })
                                };
                                let center_choices: Vec<(u64, String)> = self
                                    .scene
                                    .scene_graph
                                    .all_nodes()
                                    .filter(|(id, n)| {
                                        id.0 != nid.0
                                            && !matches!(
                                                n.payload,
                                                core_math::scene_graph::NodePayload::Root
                                                    | core_math::scene_graph::NodePayload::Material {
                                                        ..
                                                    }
                                            )
                                    })
                                    .map(|(id, n)| {
                                        let name = n.payload.display_name();
                                        let label = if name.is_empty() {
                                            format!("Node {}", id.0)
                                        } else {
                                            name.to_string()
                                        };
                                        (id.0, label)
                                    })
                                    .collect();
                                egui::ComboBox::from_id_salt(format!("mir_ctr_{open_id}_{mi}"))
                                    .selected_text(format!("Center: {center_label}"))
                                    .width(200.0)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                mods[mi].mirror_object_nid == 0,
                                                "Local origin (mesh space)",
                                            )
                                            .clicked()
                                        {
                                            mods[mi].mirror_object_nid = 0;
                                            mods_changed = true;
                                        }
                                        for (id, name) in &center_choices {
                                            if ui
                                                .selectable_label(
                                                    mods[mi].mirror_object_nid == *id,
                                                    name,
                                                )
                                                .clicked()
                                            {
                                                mods[mi].mirror_object_nid = *id;
                                                mods_changed = true;
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "Mirror plane passes through this object's origin.\n\
                                         Axes still select which planes (X/Y/Z).\n\
                                         When adding Mirror with a locked workplane, a null is \
                                         created at the workplane origin automatically.",
                                    );
                                ui.horizontal(|ui| {
                                    if ui
                                        .small_button("Null at Workplane")
                                        .on_hover_text(
                                            "Create an Empty at the active workplane origin and \
                                             use it as the mirror center.",
                                        )
                                        .clicked()
                                    {
                                        if let Some(null_nid) =
                                            self.create_mirror_center_null_at_workplane()
                                        {
                                            mods[mi].mirror_object_nid = null_nid.0;
                                            mods_changed = true;
                                        }
                                    }
                                    if mods[mi].mirror_object_nid != 0
                                        && ui.small_button("Clear center").clicked()
                                    {
                                        mods[mi].mirror_object_nid = 0;
                                        mods_changed = true;
                                    }
                                });
                            }
                            PolyModifierType::Solidify => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].solidify_thickness,
                                            0.001..=2.0,
                                        )
                                        .text("Thickness")
                                        .logarithmic(true),
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].solidify_offset,
                                            -1.0..=1.0,
                                        )
                                        .text("Offset"),
                                    )
                                    .changed();
                            }
                            PolyModifierType::Smooth => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].smooth_iterations,
                                            1..=20,
                                        )
                                        .text("Iterations"),
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].smooth_factor, 0.0..=1.0)
                                            .text("Factor"),
                                    )
                                    .on_hover_text(
                                        "Blend toward the 1-ring average per iteration.\n\n\
                                         Prefer 0.5. On a quad mesh (anything straight out of \
                                         Remesh) the vertex adjacency is bipartite, so the \
                                         checkerboard mode is damped by |1 - 2\u{00D7}factor|: at \
                                         0.5 it dies in one pass, at 1.0 it never decays at all \
                                         and extra iterations only shrink volume.",
                                    )
                                    .changed();
                                mods_changed |= Self::draw_geo_channel_ui(
                                    ui,
                                    &mut mods[mi].geo_channel,
                                    &format!("geo_ch_smooth_{open_id}_{mi}"),
                                    &channel_groups,
                                );
                                // Feather is meaningless for a CONTINUOUS channel — the authored
                                // per-edge weight already IS the ramp, and dilating it would only
                                // smear a mask the user painted deliberately.
                                if !mods[mi].geo_channel.is_all()
                                    && !mods[mi].geo_channel.kind.is_continuous()
                                {
                                    mods_changed |= ui
                                        .add(
                                            egui::Slider::new(
                                                &mut mods[mi].smooth_falloff,
                                                0..=12,
                                            )
                                            .text("Falloff")
                                            .suffix(" rings"),
                                        )
                                        .on_hover_text(
                                            "Fade the mask out over this many edge rings.\n\n\
                                             0 = hard edge: the last masked vertex is fully \
                                             smoothed and its neighbour is fully pinned, which \
                                             leaves a visible ridge along the channel boundary.",
                                        )
                                        .changed();
                                }
                                ui.weak("Geometry channel selects where smoothing applies");
                            }
                            PolyModifierType::Triangulate => {
                                mods_changed |= Self::draw_geo_channel_ui(
                                    ui,
                                    &mut mods[mi].geo_channel,
                                    &format!("geo_ch_tri_{open_id}_{mi}"),
                                    &channel_groups,
                                );
                                ui.weak(
                                    "Geometry channel selects which faces become triangles \
                                     (All = entire mesh).",
                                );
                            }
                            PolyModifierType::Decimate => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].decimate_angle,
                                            0.1..=45.0,
                                        )
                                        .text("Angle \u{00B0}")
                                        .logarithmic(true),
                                    )
                                    .on_hover_text(
                                        "Limited dissolve: merge faces whose dihedral is within \
                                         this angle (Blender Limited Dissolve).",
                                    )
                                    .changed();
                            }
                            PolyModifierType::VertexMerge => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].merge_distance,
                                            0.00001..=1.0,
                                        )
                                        .text("Distance")
                                        .logarithmic(true),
                                    )
                                    .on_hover_text(
                                        "Merge by Distance (Blender): weld vertices closer than \
                                         this threshold. Each cluster is placed at the average \
                                         of its positions.",
                                    )
                                    .changed();
                                ui.weak("Weld verts within Distance \u{2192} average position");
                            }
                            PolyModifierType::MeshCleanup => {
                                mods_changed |= ui
                                    .checkbox(
                                        &mut mods[mi].cleanup_flatten_folds,
                                        "Flatten folds",
                                    )
                                    .on_hover_text(
                                        "Flatten zero-thickness coplanar flaps (poisoned booleans).",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(
                                        &mut mods[mi].cleanup_merge_coplanar,
                                        "Merge coplanar",
                                    )
                                    .on_hover_text(
                                        "Merge coplanar faces back into n-gons.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].cleanup_dissolve_angle,
                                            0.0..=45.0,
                                        )
                                        .text("Dissolve \u{00B0}"),
                                    )
                                    .on_hover_text(
                                        "Limited Dissolve after cleanup (0 = off). Same as \
                                         Decimate angle when set.",
                                    )
                                    .changed();
                                ui.weak("Also repairs T-junctions + collinear pins");
                            }
                            PolyModifierType::Array => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].array_count, 2..=20)
                                            .text("Count"),
                                    )
                                    .on_hover_text("Total instances including the original")
                                    .changed();
                                ui.horizontal(|ui| {
                                    ui.label("Offset:");
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].array_offset[0])
                                                .prefix("X:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].array_offset[1])
                                                .prefix("Y:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].array_offset[2])
                                                .prefix("Z:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                });
                            }
                            PolyModifierType::Duplicate => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].array_count, 1..=20)
                                            .text("Copies"),
                                    )
                                    .on_hover_text(
                                        "Number of EXTRA copies (original stays; each copy \
                                         is offset by Copies \u{00D7} Offset)",
                                    )
                                    .changed();
                                ui.horizontal(|ui| {
                                    ui.label("Offset:");
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].array_offset[0])
                                                .prefix("X:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].array_offset[1])
                                                .prefix("Y:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].array_offset[2])
                                                .prefix("Z:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                });
                                ui.weak("Non-destructive · merge into display mesh");
                            }
                            PolyModifierType::Pipe => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].pipe_radius, 0.001..=2.0)
                                            .logarithmic(true)
                                            .text("Radius"),
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].pipe_segments, 3..=64)
                                            .text("Segments"),
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].pipe_caps, "Caps")
                                    .on_hover_text("Close the two ends of an open pipe")
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].pipe_closed, "Closed loop")
                                    .on_hover_text("Weld the last ring back onto the first")
                                    .changed();
                                let pts = mods[mi].pipe_path.len();
                                ui.weak(format!("Path: {pts} point(s) \u{00B7} editable forever"));
                            }
                            PolyModifierType::ArrayCircular => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].array_count, 2..=36)
                                            .text("Count"),
                                    )
                                    .on_hover_text("Total instances including the original at 0\u{00B0}")
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].circ_angle_deg,
                                            1.0..=360.0,
                                        )
                                        .text("Angle \u{00B0}"),
                                    )
                                    .on_hover_text(
                                        "360\u{00B0} = full ring (even spacing, no double at 0). \
                                         Partial arc includes both endpoints.",
                                    )
                                    .changed();
                                ui.horizontal(|ui| {
                                    ui.label("Axis:");
                                    for (lab, ax) in [("X", 0u8), ("Y", 1), ("Z", 2)] {
                                        if ui
                                            .selectable_label(mods[mi].circ_axis == ax, lab)
                                            .clicked()
                                        {
                                            mods[mi].circ_axis = ax;
                                            mods_changed = true;
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Center:");
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].circ_center[0])
                                                .prefix("X:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].circ_center[1])
                                                .prefix("Y:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].circ_center[2])
                                                .prefix("Z:")
                                                .speed(0.05),
                                        )
                                        .changed();
                                });
                                mods_changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut mods[mi].circ_radius)
                                            .prefix("Radius+:")
                                            .speed(0.02),
                                    )
                                    .on_hover_text(
                                        "Optional extra radial push per step (local units \
                                         along center\u{2192}centroid, rotated).",
                                    )
                                    .changed();
                            }
                            PolyModifierType::Inset => {
                                mods_changed |= Self::draw_geo_channel_ui(
                                    ui,
                                    &mut mods[mi].geo_channel,
                                    &format!("geo_ch_inset_{open_id}_{mi}"),
                                    &channel_groups,
                                );
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].inset_thickness,
                                            -1.0..=1.0,
                                        )
                                        .text("Thickness")
                                        .logarithmic(false),
                                    )
                                    .on_hover_text(
                                        "Inset amount (positive = inward). Same as interactive I.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].inset_depth, -1.0..=1.0)
                                            .text("Depth"),
                                    )
                                    .on_hover_text(
                                        "Extra push along face normals after inset (Blender Depth).",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].inset_individual, "Individual")
                                    .on_hover_text(
                                        "Off = Region (group faces, Blender I default).\n\
                                         On = per-face inset (Blender I,I).\n\
                                         Note: when Region has no boundary (e.g. All faces on a \
                                         closed solid), the stack auto-falls back to Individual \
                                         so the modifier is never a silent no-op.",
                                    )
                                    .changed();
                                ui.weak("Geometry channel selects which faces to inset");
                            }
                            PolyModifierType::Bevel => {
                                mods_changed |= Self::draw_geo_channel_ui(
                                    ui,
                                    &mut mods[mi].geo_channel,
                                    &format!("geo_ch_bevel_{open_id}_{mi}"),
                                    &channel_groups,
                                );
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].bevel_width, 0.001..=1.0)
                                            .text("Width")
                                            .logarithmic(true),
                                    )
                                    .on_hover_text(
                                        "The bevel width. A per-edge WEIGHT painted with W scales \
                                         this down on that edge \u{2014} weight is a percentage of \
                                         Width, always live, no mode to turn on. Unpainted edges \
                                         get the full Width.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].bevel_segments, 1..=8)
                                            .text("Segments"),
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].bevel_shape, 0.0..=1.0)
                                            .text("Shape"),
                                    )
                                    .on_hover_text(
                                        "0 = flat chamfer, 0.5 = circular fillet (Blender default), 1 = convex",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].bevel_clamp, "Clamp Overlap")
                                    .on_hover_text(
                                        "Limit width so faces can't invert (Blender Clamp Overlap).",
                                    )
                                    .changed();
                                // AUTO MERGE VERTICES — Clamp Overlap's limit applied PER EDGE
                                // instead of as one global minimum, so a pair of edges beveled too
                                // near each other pins at its meet point (where the kernel welds the
                                // two rails into one vertex) while every other edge still reaches
                                // Width. Measured: a cube with a 0.06-wide loop-cut band bevels at
                                // 0.030 everywhere when 0.2 is requested; per-edge, the roomy
                                // corners reach the full 0.200.
                                //
                                // Needs Clamp Overlap (no limit ⇒ no meet position ⇒ nothing to
                                // merge), and Variable Width IMPLIES it (grading is meaningless
                                // under a global minimum), so it shows checked+disabled there rather
                                // than leaving the state invisible.
                                let consume_on = mods[mi].bevel_consume;
                                let clamp_on = mods[mi].bevel_clamp;
                                let var_on = mods[mi].bevel_var_width != 0;
                                ui.add_enabled_ui(!consume_on && clamp_on && !var_on, |ui| {
                                    let mut merge = mods[mi].bevel_auto_merge || var_on;
                                    let r = ui
                                        .checkbox(&mut merge, "Auto Merge Vertices")
                                        .on_hover_text(if consume_on {
                                            "Unavailable while Consume Geometry is on: absorbing an \
                                             edge loop is decided from a single width, so a per-edge \
                                             limit would disagree with the geometry it produces."
                                        } else if !clamp_on {
                                            "Needs Clamp Overlap: with no limit there is no meet \
                                             position for the vertices to merge at."
                                        } else if var_on {
                                            "Always on with Variable Width \u{2014} per-edge widths \
                                             need per-edge limits, or one global minimum would cap \
                                             every edge to the smallest and erase the variation."
                                        } else {
                                            "Apply the Clamp Overlap limit PER EDGE instead of \
                                             globally: two edges beveled too near each other stop at \
                                             their meet point and their vertices merge there, while \
                                             every other edge still reaches Width. This is NOT a \
                                             merge by distance \u{2014} nothing looks for nearby \
                                             vertices, so small detail can't be swallowed."
                                        });
                                    if r.changed() {
                                        mods[mi].bevel_auto_merge = merge;
                                        mods_changed = true;
                                    }
                                });
                                // VARIABLE WIDTH — one width over a selection that mixes scales is
                                // wrong either way: with Clamp Overlap on, the single smallest edge
                                // drags every other edge down to its own limit; with it off, the
                                // small detail folds. Mutually exclusive with Consume Geometry,
                                // which decides which rings to absorb from ONE width.
                                ui.add_enabled_ui(!consume_on, |ui| {
                                    let mut on = var_on;
                                    let r = ui.checkbox(&mut on, "Variable Width").on_hover_text(
                                        if consume_on {
                                            "Unavailable while Consume Geometry is on: absorbing an \
                                             edge loop is decided from a single width, so a per-edge \
                                             width would disagree with the geometry it produces."
                                        } else {
                                            "Give every edge its own width between Min Width and \
                                             Width instead of one width everywhere \u{2014} small \
                                             detail scales itself down instead of dragging the whole \
                                             bevel down to its limit (or folding). No effect on \
                                             uniform geometry."
                                        },
                                    );
                                    if r.changed() {
                                        // Default driver = auto by local size: the mode that needs
                                        // nothing authored.
                                        mods[mi].bevel_var_width = if on { 1 } else { 0 };
                                        mods_changed = true;
                                    }
                                    if mods[mi].bevel_var_width != 0 {
                                        let cur = mods[mi].bevel_var_width;
                                        egui::ComboBox::from_id_salt(format!(
                                            "bevel_var_driver_{open_id}_{mi}"
                                        ))
                                        // A file saved with the retired driver 2 shows (and behaves
                                        // as) segment size; its weights now apply on top regardless.
                                        .selected_text(match cur {
                                            3 => "Auto by corner span",
                                            _ => "Auto by segment size",
                                        })
                                        .show_ui(ui, |ui| {
                                            if ui
                                                .selectable_label(cur == 1, "Auto by segment size")
                                                .on_hover_text(
                                                    "Each edge takes a share of Width proportional \
                                                     to its own local room, so a small feature gets \
                                                     a small bevel automatically.\n\nSubdividing a \
                                                     face shortens its segments, so the SAME shape \
                                                     then asks for a smaller bevel.",
                                                )
                                                .clicked()
                                            {
                                                mods[mi].bevel_var_width = 1;
                                                mods_changed = true;
                                            }
                                            if ui
                                                .selectable_label(cur == 3, "Auto by corner span")
                                                .on_hover_text(
                                                    "Measure ACROSS the corner at each end of the \
                                                     edge instead of along one segment, sliding \
                                                     both neighbours by the SHORTER of the two.\n\n\
                                                     A 2 m edge meeting a 1 cm edge still reads as \
                                                     small detail and gets a small bevel.",
                                                )
                                                .clicked()
                                            {
                                                mods[mi].bevel_var_width = 3;
                                                mods_changed = true;
                                            }
                                            // "Authored weight" is NOT a driver any more. The
                                            // per-edge weight is a PERCENTAGE of Width and applies
                                            // on top of whichever driver is chosen, with no mode to
                                            // enable — burying it in here is why painting weights
                                            // looked like it did nothing (owner 2026-08-11).
                                        });
                                        let hi = mods[mi].bevel_width.max(0.001);
                                        mods_changed |= ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut mods[mi].bevel_width_min,
                                                    0.0..=hi,
                                                )
                                                .text("Min Width"),
                                            )
                                            .on_hover_text(
                                                "Small end of the band. 0 = purely proportional. An \
                                                 edge with less room than this still gets only what \
                                                 it can hold \u{2014} the floor never forces a fold.",
                                            )
                                            .changed();
                                        if mods[mi].bevel_width_min > hi {
                                            mods[mi].bevel_width_min = hi;
                                        }
                                    }
                                });
                                let consume_r = ui
                                    .checkbox(&mut mods[mi].bevel_consume, "Consume Geometry")
                                    .on_hover_text(
                                        "CAD-style fillet growth: when the width reaches the next edge loop, \
                                         that loop is absorbed (non-destructively, in the modifier only) so the \
                                         bevel keeps growing instead of sticking. Quad-grid rows only; anything \
                                         else falls back to the normal clamp.",
                                    );
                                mods_changed |= consume_r.changed();
                                if consume_r.changed() && mods[mi].bevel_consume {
                                    // Make the exclusivity REAL, not just greyed out. Consume can
                                    // fall back to the plain bevel path, where a per-edge width or
                                    // limit left set would still act while its controls are hidden —
                                    // invisible state driving visible geometry.
                                    mods[mi].bevel_var_width = 0;
                                    mods[mi].bevel_auto_merge = false;
                                }
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].bevel_smart_select, "Smart Select")
                                    .on_hover_text(
                                        "Skip the tessellation of a subtracted cylinder \u{2014}                                          identified exactly from the boolean, not guessed.                                          The rim where the hole meets the face is always kept.",
                                    )
                                    .changed();
                                // Blender's Bevel "Limit Method: Angle". Independent of Smart
                                // Select and applied last, so the angle is the final word.
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].bevel_limit_angle, "Limit by Angle")
                                    .on_hover_text(
                                        "Blender's Bevel \u{201c}Limit Method: Angle\u{201d} \u{2014} bevel an edge only when its two faces deviate by at least the angle below. Applied last, so it always wins.",
                                    )
                                    .changed();
                                if mods[mi].bevel_limit_angle {
                                    mods_changed |= ui
                                        .add(
                                            egui::Slider::new(
                                                &mut mods[mi].bevel_angle_deg,
                                                0.0..=180.0,
                                            )
                                            .text("Angle")
                                            .suffix("\u{00b0}"),
                                        )
                                        .on_hover_text(
                                            "Angle between the two face normals: 0\u{00b0} = coplanar, 90\u{00b0} = square corner. Blender's default is 30\u{00b0}.",
                                        )
                                        .changed();
                                }
                                ui.weak(if mods[mi].bevel_smart_select {
                                    "Manifold edges, minus cutter tessellation"
                                } else {
                                    "All manifold edges"
                                });
                            }
                            PolyModifierType::Boolean => {
                                let op_lab = match mods[mi].bool_op {
                                    1 => "Union",
                                    2 => "Intersect",
                                    _ => "Difference",
                                };
                                egui::ComboBox::from_id_salt(format!("bool_op_{open_id}_{mi}"))
                                    .selected_text(op_lab)
                                    .width(120.0)
                                    .show_ui(ui, |ui| {
                                        for (lab, v) in
                                            [("Difference", 0u8), ("Union", 1), ("Intersect", 2)]
                                        {
                                            if ui
                                                .selectable_label(mods[mi].bool_op == v, lab)
                                                .clicked()
                                            {
                                                mods[mi].bool_op = v;
                                                mods_changed = true;
                                            }
                                        }
                                    });
                                // Cutter picker: list other poly bodies
                                let mut cutter_name = if mods[mi].bool_cutter_nid == 0 {
                                    "(pick cutter…)".to_string()
                                } else {
                                    self.scene
                                        .scene_graph
                                        .get(core_math::scene_graph::NodeId(mods[mi].bool_cutter_nid))
                                        .and_then(|n| match &n.payload {
                                            core_math::scene_graph::NodePayload::PolygonalBody {
                                                name,
                                                ..
                                            } => Some(name.clone()),
                                            _ => None,
                                        })
                                        .unwrap_or_else(|| format!("#{}", mods[mi].bool_cutter_nid))
                                };
                                let poly_choices: Vec<(u64, String)> = self
                                    .scene
                                    .scene_graph
                                    .poly_bodies()
                                    .filter(|(id, _)| id.0 != nid.0)
                                    .map(|(id, n)| {
                                        let name = match &n.payload {
                                            core_math::scene_graph::NodePayload::PolygonalBody {
                                                name,
                                                ..
                                            } => name.clone(),
                                            _ => format!("Poly {}", id.0),
                                        };
                                        (id.0, name)
                                    })
                                    .collect();
                                egui::ComboBox::from_id_salt(format!("bool_cut_{open_id}_{mi}"))
                                    .selected_text(&cutter_name)
                                    .width(160.0)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                mods[mi].bool_cutter_nid == 0,
                                                "(none)",
                                            )
                                            .clicked()
                                        {
                                            mods[mi].bool_cutter_nid = 0;
                                            mods_changed = true;
                                        }
                                        for (id, name) in &poly_choices {
                                            if ui
                                                .selectable_label(
                                                    mods[mi].bool_cutter_nid == *id,
                                                    name,
                                                )
                                                .clicked()
                                            {
                                                mods[mi].bool_cutter_nid = *id;
                                                mods_changed = true;
                                            }
                                        }
                                    });
                                if mods[mi].bool_cutter_nid == 0 {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(230, 170, 90),
                                        "Select a cutter poly body",
                                    );
                                } else {
                                    ui.weak("Closed meshes only · result stays on this body");
                                }
                                let _ = &mut cutter_name;
                            }
                            PolyModifierType::WeightedNormal => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].weighted_keep_sharp_deg,
                                            0.0..=180.0,
                                        )
                                        .text("Keep Sharp \u{00B0}"),
                                    )
                                    .on_hover_text(
                                        "Faces steeper than this (vs the dominant face at each \
                                         vertex) keep their own normal. 180\u{00B0} = fully smooth.",
                                    )
                                    .changed();
                                let modes = ["Corner Angle", "Face Area", "Face Area & Angle"];
                                let cur = (mods[mi].weighted_mode as usize).min(2);
                                egui::ComboBox::from_label("Mode")
                                    .selected_text(modes[cur])
                                    .show_ui(ui, |ui| {
                                        for (i, name) in modes.iter().enumerate() {
                                            if ui
                                                .selectable_label(cur == i, *name)
                                                .clicked()
                                            {
                                                mods[mi].weighted_mode = i as u8;
                                                mods_changed = true;
                                            }
                                        }
                                    });
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].weighted_weight, 1..=100)
                                            .text("Weight"),
                                    )
                                    .on_hover_text(
                                        "How strongly the highest-weighted face wins. 50 = the \
                                         original behaviour.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].weighted_threshold,
                                            0.0..=1.0,
                                        )
                                        .text("Threshold"),
                                    )
                                    .on_hover_text(
                                        "Faces this close to the top weight count as tied and \
                                         blend, instead of one being picked arbitrarily.",
                                    )
                                    .changed();
                            }
                            PolyModifierType::SmoothNormal => {
                                // Legacy Smooth Normal never exposed knobs — stacks kept the
                                // shared Default (angle 30° / strength 0.5) while shading was
                                // always full-smooth. Upgrade once so UI matches the bake.
                                if (mods[mi].weighted_keep_sharp_deg - 30.0).abs() < 1e-3
                                    && (mods[mi].smooth_factor - 0.5).abs() < 1e-3
                                    && mods[mi].weighted_weight == 50
                                    && mods[mi].weighted_mode == 2
                                    && (mods[mi].weighted_threshold - 0.01).abs() < 1e-4
                                {
                                    mods[mi].weighted_keep_sharp_deg = 180.0;
                                    mods[mi].smooth_factor = 1.0;
                                    mods_changed = true;
                                }
                                ui.weak(
                                    "Smooth shading · last enabled Normal mod in the stack drives the bake",
                                );
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].weighted_keep_sharp_deg,
                                            0.0..=180.0,
                                        )
                                        .text("Angle \u{00B0}"),
                                    )
                                    .on_hover_text(
                                        "Keep-sharp angle (auto-smooth style). Faces steeper than \
                                         this vs the dominant face keep a harder split. \
                                         180\u{00B0} = fully smooth (default).",
                                    )
                                    .changed();
                                let modes = ["Corner Angle", "Face Area", "Face Area & Angle"];
                                let cur = (mods[mi].weighted_mode as usize).min(2);
                                egui::ComboBox::from_label("Mode")
                                    .selected_text(modes[cur])
                                    .show_ui(ui, |ui| {
                                        for (i, name) in modes.iter().enumerate() {
                                            if ui
                                                .selectable_label(cur == i, *name)
                                                .clicked()
                                            {
                                                mods[mi].weighted_mode = i as u8;
                                                mods_changed = true;
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "How faces contribute at each corner. \
                                         Corner Angle = angle-based; Face Area & Angle = default.",
                                    );
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].weighted_weight, 1..=100)
                                            .text("Weight"),
                                    )
                                    .on_hover_text(
                                        "How strongly the highest-weighted face wins (Blender Weight). \
                                         50 = neutral.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].weighted_threshold,
                                            0.0..=1.0,
                                        )
                                        .text("Threshold"),
                                    )
                                    .on_hover_text(
                                        "Near-tied faces blend instead of one winning alone.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].smooth_factor, 0.0..=1.0)
                                            .text("Strength"),
                                    )
                                    .on_hover_text(
                                        "Amount of smooth shading. 0 = face-hard normals, \
                                         1 = full smooth (default).",
                                    )
                                    .changed();
                            }
                            PolyModifierType::Scatter => {
                                // WHICH MESH IS WHICH. Both combos shipped unlabelled, and the
                                // owner immediately wired it backwards (2026-08-05: "I add a cube
                                // as the mesh to generate point and a cylinder to duplicate, and
                                // the cylinder uses the cube and its own vertex"). They had put
                                // the modifier on the CYLINDER — so the cylinder's own vertices
                                // were the points, which is exactly what the code does and not at
                                // all what they meant. A modifier necessarily lives on the body it
                                // modifies, so the points can only come from THIS body; the picker
                                // can only be the thing being instanced. Say both out loud, naming
                                // the actual body, instead of leaving two anonymous dropdowns.
                                let this_name = self
                                    .scene
                                    .scene_graph
                                    .get(nid)
                                    .and_then(|n| match &n.payload {
                                        core_math::scene_graph::NodePayload::PolygonalBody {
                                            name,
                                            ..
                                        } => Some(name.clone()),
                                        _ => None,
                                    })
                                    .unwrap_or_else(|| "this mesh".to_string());
                                ui.label(
                                    egui::RichText::new("Object to scatter (copied onto this mesh)")
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(150, 158, 175)),
                                );
                                // Source object picker — same list + same "(none)" fallback as
                                // the Boolean cutter, so a dangling reference reads identically.
                                let src_name = if mods[mi].scatter_source_nid == 0 {
                                    "(pick object\u{2026})".to_string()
                                } else {
                                    self.scene
                                        .scene_graph
                                        .get(core_math::scene_graph::NodeId(
                                            mods[mi].scatter_source_nid,
                                        ))
                                        .and_then(|n| match &n.payload {
                                            core_math::scene_graph::NodePayload::PolygonalBody {
                                                name,
                                                ..
                                            } => Some(name.clone()),
                                            _ => None,
                                        })
                                        .unwrap_or_else(|| {
                                            format!("#{} (missing)", mods[mi].scatter_source_nid)
                                        })
                                };
                                let poly_choices: Vec<(u64, String)> = self
                                    .scene
                                    .scene_graph
                                    .poly_bodies()
                                    .filter(|(id, _)| id.0 != nid.0)
                                    .map(|(id, n)| {
                                        let name = match &n.payload {
                                            core_math::scene_graph::NodePayload::PolygonalBody {
                                                name,
                                                ..
                                            } => name.clone(),
                                            _ => format!("Poly {}", id.0),
                                        };
                                        (id.0, name)
                                    })
                                    .collect();
                                egui::ComboBox::from_id_salt(format!("scat_src_{open_id}_{mi}"))
                                    .selected_text(&src_name)
                                    .width(160.0)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                mods[mi].scatter_source_nid == 0,
                                                "(none)",
                                            )
                                            .clicked()
                                        {
                                            mods[mi].scatter_source_nid = 0;
                                            mods_changed = true;
                                        }
                                        for (id, name) in &poly_choices {
                                            if ui
                                                .selectable_label(
                                                    mods[mi].scatter_source_nid == *id,
                                                    name,
                                                )
                                                .clicked()
                                            {
                                                mods[mi].scatter_source_nid = *id;
                                                mods_changed = true;
                                            }
                                        }
                                    });
                                if mods[mi].scatter_source_nid == 0 {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(230, 170, 90),
                                        "Select an object to scatter",
                                    );
                                }
                                // Point source — named after the body it actually reads, so the
                                // "which mesh generates the points" question answers itself.
                                ui.label(
                                    egui::RichText::new(format!("Scatter points from “{this_name}”"))
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(150, 158, 175)),
                                );
                                let srcs = [
                                    ("Vertices", core_math::scatter::SCATTER_SRC_VERTICES),
                                    ("Face Centers", core_math::scatter::SCATTER_SRC_FACE_CENTERS),
                                    ("Random on Faces", core_math::scatter::SCATTER_SRC_RANDOM_FACES),
                                ];
                                let cur = srcs
                                    .iter()
                                    .find(|(_, v)| *v == mods[mi].scatter_source)
                                    .map(|(n, _)| *n)
                                    .unwrap_or("Vertices");
                                egui::ComboBox::from_id_salt(format!("scat_pts_{open_id}_{mi}"))
                                    .selected_text(cur)
                                    .width(160.0)
                                    .show_ui(ui, |ui| {
                                        for (name, v) in &srcs {
                                            if ui
                                                .selectable_label(
                                                    mods[mi].scatter_source == *v,
                                                    *name,
                                                )
                                                .clicked()
                                            {
                                                mods[mi].scatter_source = *v;
                                                mods_changed = true;
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "Where the instances go: this mesh's vertices, its face \
                                         centres, or random points spread by face AREA.",
                                    );
                                if mods[mi].scatter_source
                                    == core_math::scatter::SCATTER_SRC_RANDOM_FACES
                                {
                                    let cap = core_math::scatter::MAX_SCATTER_INSTANCES as u32;
                                    mods_changed |= ui
                                        .add(
                                            egui::Slider::new(
                                                &mut mods[mi].scatter_count,
                                                1..=cap.min(2000),
                                            )
                                            .text("Count"),
                                        )
                                        .on_hover_text(
                                            "Number of instances, spread by face area. Raising it \
                                             ADDS points — the existing ones do not move.",
                                        )
                                        .changed();
                                }
                                mods_changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut mods[mi].scatter_seed)
                                            .prefix("Seed: ")
                                            .speed(1.0),
                                    )
                                    .on_hover_text(
                                        "One seed drives every random channel. Same seed = same \
                                         scatter, always.",
                                    )
                                    .changed();
                                ui.separator();
                                mods_changed |= ui
                                    .checkbox(
                                        &mut mods[mi].scatter_align_normal,
                                        "Align to Normal",
                                    )
                                    .on_hover_text(
                                        "Point each copy's local +Y along the surface normal.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].scatter_spin_deg,
                                            0.0..=360.0,
                                        )
                                        .text("Spin \u{00B0}"),
                                    )
                                    .on_hover_text("Random spin about the normal (0..this).")
                                    .changed();
                                ui.horizontal(|ui| {
                                    ui.label("Scale:");
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].scatter_scale_min)
                                                .prefix("min:")
                                                .speed(0.01)
                                                .range(0.001..=100.0),
                                        )
                                        .changed();
                                    mods_changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut mods[mi].scatter_scale_max)
                                                .prefix("max:")
                                                .speed(0.01)
                                                .range(0.001..=100.0),
                                        )
                                        .changed();
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Axis scale min");
                                    for a in 0..3usize {
                                        mods_changed |= ui
                                            .add(
                                                egui::DragValue::new(
                                                    &mut mods[mi].scatter_scale_axis_min[a],
                                                )
                                                .prefix(["X:", "Y:", "Z:"][a])
                                                .speed(0.01)
                                                .range(0.001..=100.0),
                                            )
                                            .changed();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Axis scale max");
                                    for a in 0..3usize {
                                        mods_changed |= ui
                                            .add(
                                                egui::DragValue::new(
                                                    &mut mods[mi].scatter_scale_axis_max[a],
                                                )
                                                .prefix(["X:", "Y:", "Z:"][a])
                                                .speed(0.01)
                                                .range(0.001..=100.0),
                                            )
                                            .changed();
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Per-axis scale, multiplied on top of the uniform scale.",
                                );
                                ui.horizontal(|ui| {
                                    ui.label("Rot jitter \u{00B0}");
                                    for a in 0..3usize {
                                        mods_changed |= ui
                                            .add(
                                                egui::DragValue::new(
                                                    &mut mods[mi].scatter_rot_jitter_deg[a],
                                                )
                                                .prefix(["X:", "Y:", "Z:"][a])
                                                .speed(1.0)
                                                .range(0.0..=180.0),
                                            )
                                            .changed();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Pos jitter");
                                    for a in 0..3usize {
                                        mods_changed |= ui
                                            .add(
                                                egui::DragValue::new(
                                                    &mut mods[mi].scatter_pos_jitter[a],
                                                )
                                                .prefix(["X:", "Y:", "Z:"][a])
                                                .speed(0.01)
                                                .range(0.0..=100.0),
                                            )
                                            .changed();
                                    }
                                });
                                ui.weak(
                                    "Copies merge in as separate islands \u{00B7} this mesh is \
                                     never modified",
                                );
                            }
                            PolyModifierType::Scatter => {
                                ui.weak("Scatter source / density / seed — see full Scatter panel above");
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].scatter_count, 1..=5000)
                                            .text("Count"),
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].scatter_seed, 0..=99999)
                                            .text("Seed"),
                                    )
                                    .changed();
                            }
                            PolyModifierType::Remesh => {
                                ui.weak(
                                    "Voxel remesh: mesh \u{2192} SDF field \u{2192} Surface Nets \
                                     (same backend as sculpt remesh). Base mesh stays editable.",
                                );
                                if mods.iter().skip(mi + 1).any(|m| {
                                    m.enabled
                                        && m.mod_type == PolyModifierType::Sculpt
                                }) {
                                    ui.colored_label(
                                        crate::ui_theme::WARNING,
                                        "Editing this slot rebases the PolySculpt layer below.",
                                    );
                                }
                                let resp = ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].remesh_resolution,
                                            crate::poly::remesh::REMESH_RES_MIN
                                                ..=crate::poly::remesh::REMESH_RES_MAX,
                                        )
                                        .text("Resolution")
                                        .logarithmic(true),
                                    )
                                    .on_hover_text(format!(
                                        "Voxel grid size ({}\u{2013}{}). Live up to {}; above \
                                         that the remesh applies on slider release.",
                                        crate::poly::remesh::REMESH_RES_MIN,
                                        crate::poly::remesh::REMESH_RES_MAX,
                                        crate::poly::remesh::REMESH_LIVE_SCRUB_MAX,
                                    ));
                                // Top-range scrub deferral (REMESH_LIVE_SCRUB_MAX docs):
                                // while dragging above the live threshold, the value tracks
                                // the pointer (egui derives it from position each frame)
                                // but commit + re-eval wait for release — a per-notch eval
                                // at 256–512 is hundreds of ms to seconds, a frozen drag.
                                // drag_stopped() commits the final value; a value-less click
                                // is absorbed by the display-key early-outs.
                                let scrub_defer = resp.dragged()
                                    && mods[mi].remesh_resolution
                                        > crate::poly::remesh::REMESH_LIVE_SCRUB_MAX;
                                mods_changed |= (resp.changed() && !scrub_defer)
                                    || resp.drag_stopped();
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].remesh_smooth, "Smooth extract")
                                    .on_hover_text(
                                        "Laplacian relaxation + wide-stencil normals on the \
                                         Surface Nets extract (sculpt release quality).",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(
                                        &mut mods[mi].remesh_union_islands,
                                        "Union islands first",
                                    )
                                    .on_hover_text(
                                        "Before remesh, fuse every mesh island into one body.\n\
                                         Closed islands use Boolean Union (overlapping shells \
                                         become one solid). Open pieces soup-merge so one lattice \
                                         covers all.\n\n\
                                         Off = remesh the multi-island mesh as-is (separate \
                                         shells stay separate).",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].remesh_padding,
                                            0.0..=0.5,
                                        )
                                        .text("Padding")
                                        .suffix(" \u{00D7} half-extent"),
                                    )
                                    .on_hover_text(
                                        "AABB margin so the sealed grid border stays empty \
                                         (watertight Surface Nets).",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].remesh_project,
                                            0.0..=1.0,
                                        )
                                        .text("Project to surface"),
                                    )
                                    .on_hover_text(
                                        "Snap the remeshed vertices back onto the ORIGINAL \
                                         surface after extraction (bounded to ~1 voxel of \
                                         travel).\n\n0 = raw voxel output, 1 = fully projected.\n\
                                         This is the detail control, not Resolution: it removes \
                                         the lattice stepping without the 8\u{00D7} memory a \
                                         finer grid would cost. Detail SMALLER than one voxel is \
                                         gone before this runs and cannot be recovered \u{2014} \
                                         raise Resolution for that.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(&mut mods[mi].remesh_collapse, "Collapse flats")
                                    .on_hover_text(
                                        "Merge near-planar 2\u{00d7}2\u{00d7}2 cells (octree \
                                         dual contour). Fewer faces and a cheaper rebuild. \
                                         Off = uniform lattice, bit-identical to the previous \
                                         remesh.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(
                                        &mut mods[mi].remesh_skip_relax_drag,
                                        "Skip relax while dragging",
                                    )
                                    .on_hover_text(
                                        "During a vertex grab, skip Laplacian smooth on the \
                                         remesh (faster live update). Mouse-up still uses \
                                         Smooth extract.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(
                                        &mut mods[mi].remesh_skip_seal_drag,
                                        "Skip seal while dragging",
                                    )
                                    .on_hover_text(
                                        "During a vertex grab, skip the whole-grid enclosed-air \
                                         flood (~4 ms at 256). Mouse-up still seals if a pocket \
                                         could have formed.",
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .checkbox(
                                        &mut mods[mi].remesh_drop_res_drag,
                                        "Draft res while dragging",
                                    )
                                    .on_hover_text(
                                        "During any live edit (grab, bevel, extrude, \u{2026}), \
                                         remesh at 128 so the drag stays live. Mouse-up remeshes \
                                         at the Resolution slider. Off below 128 (already live).",
                                    )
                                    .changed();
                            }
                            PolyModifierType::Bend => {
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].bend_angle_deg,
                                            -360.0..=360.0,
                                        )
                                        .text("Angle \u{00B0}"),
                                    )
                                    .on_hover_text(
                                        "Total bend across the limit band. 0 = no deformation; \
                                         negative curls the other way.",
                                    )
                                    .changed();
                                ui.horizontal(|ui| {
                                    ui.label("Axis:")
                                        .on_hover_text(
                                            "The axis the geometry runs ALONG \u{2014} its \
                                             coordinate drives the angle.",
                                        );
                                    for (lab, ax) in [("X", 0u8), ("Y", 1), ("Z", 2)] {
                                        if ui
                                            .selectable_label(mods[mi].bend_axis == ax, lab)
                                            .clicked()
                                        {
                                            mods[mi].bend_axis = ax;
                                            mods_changed = true;
                                        }
                                    }
                                });
                                // Name the axis the geometry curls toward at 0°, straight from the
                                // frame the deform itself uses — a hand-written label here would go
                                // stale the moment that cycle changed.
                                let (_, up0, side0) =
                                    crate::poly::mesh_sync::bend_frame(mods[mi].bend_axis);
                                let axis_name = |v: glam::Vec3| {
                                    if v.x != 0.0 {
                                        "+X"
                                    } else if v.y != 0.0 {
                                        "+Y"
                                    } else {
                                        "+Z"
                                    }
                                };
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(
                                            &mut mods[mi].bend_dir_deg,
                                            0.0..=360.0,
                                        )
                                        .text("Direction \u{00B0}"),
                                    )
                                    .on_hover_text(format!(
                                        "Which way the arc curls, as a rotation about the bend \
                                         axis.\n\n0\u{00B0} curls toward {}, 90\u{00B0} toward {}.",
                                        axis_name(up0),
                                        axis_name(side0),
                                    ))
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].bend_limit_lo, 0.0..=1.0)
                                            .text("Limit lower"),
                                    )
                                    .changed();
                                mods_changed |= ui
                                    .add(
                                        egui::Slider::new(&mut mods[mi].bend_limit_hi, 0.0..=1.0)
                                            .text("Limit upper"),
                                    )
                                    .on_hover_text(
                                        "The band that bends, as a fraction of the mesh's own \
                                         extent along the axis. Geometry outside it stays \
                                         straight and rides off along the arc's end tangent \
                                         \u{2014} 0\u{2013}0.5 bends the first half and leaves the \
                                         rest rigid.",
                                    )
                                    .changed();
                                ui.horizontal(|ui| {
                                    ui.label("Center:").on_hover_text(
                                        "The one point the bend leaves untouched (local space). \
                                         At the default 0,0,0 a mesh sitting entirely off to one \
                                         side swings around the origin instead of bending in \
                                         place \u{2014} move this into the mesh to fix that.",
                                    );
                                    for ax in 0..3 {
                                        mods_changed |= ui
                                            .add(
                                                egui::DragValue::new(
                                                    &mut mods[mi].bend_center[ax],
                                                )
                                                .prefix(["X:", "Y:", "Z:"][ax])
                                                .speed(0.05),
                                            )
                                            .changed();
                                    }
                                });
                                ui.weak(
                                    "Deforms positions only \u{2014} topology, seams and edge \
                                     tags all survive.",
                                );
                            }
                            PolyModifierType::Sculpt => {
                                ui.weak(
                                    "PolySculpt \u{2014} same brushes as the sculpt room, \
                                     applied after Remesh / Smooth. No dynamesh.",
                                );
                                ui.weak(
                                    "Editing Remesh / Smooth above this layer rebases deltas.",
                                );
                                ui.weak(
                                    "A Bevel with Smart Select above this slot disables the \
                                     remesh cache (tagged stack).",
                                );
                                ui.label(format!(
                                    "{} stored deltas  (gen {})",
                                    mods[mi].sculpt_deltas.len(),
                                    mods[mi].sculpt_delta_gen
                                ));
                                let editing = self.sculpt.poly_edit == Some((nid, mi));
                                if editing {
                                    ui.colored_label(
                                        crate::ui_theme::WARNING,
                                        "Editing — LMB strokes this layer",
                                    );
                                }
                                let cur = self
                                    .sculpt
                                    .poly_brush
                                    .unwrap_or(core_math::voxel_sculpt::VoxelBrush::Draw);
                                egui::ComboBox::from_id_salt(("poly_sculpt_brush", nid.0, mi))
                                    .selected_text(
                                        crate::VoxelObject::factory_label(cur),
                                    )
                                    .show_ui(ui, |ui| {
                                        use core_math::voxel_sculpt::VoxelBrush::*;
                                        for b in [
                                            Draw, Erase, Smooth, Flatten, Inflate, Move, Pinch,
                                            Clay, ClayBuildup, Scrape, Crease,
                                        ] {
                                            if ui
                                                .selectable_label(cur == b, crate::VoxelObject::factory_label(b))
                                                .clicked()
                                            {
                                                self.sculpt.poly_brush = Some(b);
                                            }
                                        }
                                        // No poly Snake Hook yet (POLYSCULPT-PLANELESS) — the
                                        // stamp gates it fail-closed; keep the picker honest.
                                        ui.add_enabled(
                                            false,
                                            egui::Label::new("Snake Hook — Surface mode only"),
                                        );
                                        ui.add_enabled(
                                            false,
                                            egui::Label::new("Fill — density-only"),
                                        );
                                        ui.add_enabled(
                                            false,
                                            egui::Label::new("Smudge — use Move/Grab"),
                                        );
                                    });
                                if self.sculpt.poly_radius <= 1e-4 {
                                    self.sculpt.poly_radius = 0.15;
                                }
                                if self.sculpt.poly_strength <= 1e-4 {
                                    self.sculpt.poly_strength = 1.0;
                                }
                                ui.add(
                                    egui::Slider::new(&mut self.sculpt.poly_radius, 0.01..=2.0)
                                        .text("Size")
                                        .logarithmic(true),
                                );
                                ui.add(
                                    egui::Slider::new(&mut self.sculpt.poly_strength, 0.05..=2.0)
                                        .text("Strength"),
                                );
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(self.sculpt.poly_tool.brush_add, "Add")
                                        .clicked()
                                    {
                                        self.sculpt.poly_tool.brush_add = true;
                                    }
                                    if ui
                                        .selectable_label(!self.sculpt.poly_tool.brush_add, "Subtract")
                                        .clicked()
                                    {
                                        self.sculpt.poly_tool.brush_add = false;
                                    }
                                });
                                egui::ComboBox::from_id_salt(("poly_sculpt_fo", nid.0, mi))
                                    .selected_text(self.sculpt.poly_tool.falloff_preset.label())
                                    .show_ui(ui, |ui| {
                                        for fo in core_math::voxel_sculpt::FalloffPreset::ALL {
                                            if ui
                                                .selectable_label(
                                                    self.sculpt.poly_tool.falloff_preset == fo,
                                                    fo.label(),
                                                )
                                                .clicked()
                                            {
                                                self.sculpt.poly_tool.falloff_preset = fo;
                                            }
                                        }
                                    });
                                ui.add(
                                    egui::Slider::new(&mut self.sculpt.poly_hardness, 0.0..=1.0)
                                        .text("Hardness"),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Full Brush / Texture / Stroke settings: Tool Parameters",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                        }
                    });
                // Click empty card chrome → select modifier (viewport edit / boolean wire).
                if resp.response.clicked() {
                    select_mi = Some(mi);
                }
                ui.add_space(2.0);
            }
            if let Some(mi) = copy_mi {
                // Selecting first keeps ONE copy path: the button and Ctrl+C both go
                // through `copy_selected_poly_modifier`, never a parallel implementation.
                self.ui.poly_mod_selected = Some((nid, mi));
                pending_copy = true;
            }
            if let Some(mi) = select_mi {
                self.ui.poly_mod_selected = Some((nid, mi));
                self.overlay_lines_fp = u64::MAX; // rebuild wires next frame
            }
            if let Some(ri) = remove_idx {
                if self.ui.poly_mod_selected == Some((nid, ri)) {
                    self.ui.poly_mod_selected = None;
                }
                mods.remove(ri);
                mods_changed = true;
            }
            if let Some((a, b)) = swap_pair {
                mods.swap(a, b);
                mods_changed = true;
                // Keep selection on the same slot identity after reorder.
                if let Some((snid, si)) = self.ui.poly_mod_selected {
                    if snid == nid {
                        if si == a {
                            self.ui.poly_mod_selected = Some((nid, b));
                        } else if si == b {
                            self.ui.poly_mod_selected = Some((nid, a));
                        }
                    }
                }
            }
        });

        // Apply bakes mesh + rewrites the stack itself — must not also write the local `mods` clone.
        if let Some(ai) = apply_idx {
            self.apply_poly_modifier_through(nid, ai);
            return;
        }
        if let Some(ei) = edit_sculpt_mi {
            if self.sculpt.poly_edit == Some((nid, ei)) {
                self.poly_sculpt_leave_edit();
            } else {
                self.poly_sculpt_enter_edit(nid, ei);
            }
        }

        if mods_changed {
            // ONE undo entry per DRAG, not per frame (owner 2026-08-05: "sliding a modifier
            // slider creates a large number of Ctrl+Z when sliding; Ctrl+Z should only
            // trigger after the slide"). `transaction_snapshot_only` fires on every frame a
            // value changes, so dragging a width slider wrote dozens of entries and buried
            // the user's real history. An EPISODE snapshots once at the start and commits on
            // pointer release (`auto_commit_episodes` end-of-frame catches the release),
            // which is the same pattern the Transform sliders at the top of this panel
            // already use. Begun BEFORE the write-back, so the snapshot holds the PRE-edit
            // stack - `mods` is a clone, so the graph still carries the old values here.
            if !self.has_active_episode() {
                self.begin_episode("Modify Poly Stack");
            }
            if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                    &mut n.payload
                {
                    *modifiers = mods;
                }
            }
            // Multi-object add: same modifier type, default params, onto every other
            // selected mesh. Same snapshot/transaction as the primary body → ONE undo step.
            if let Some(mt) = add_to_extras {
                for &t in &extra_targets {
                    if let Some(n) = self.scene.scene_graph.get_mut(t) {
                        if let core_math::scene_graph::NodePayload::PolygonalBody {
                            modifiers, ..
                        } = &mut n.payload
                        {
                            modifiers.push(PolyModifier::new(mt));
                        }
                    }
                    self.poly_sync_mesh_cache(t);
                }
                self.toast(&format!(
                    "Added {:?} to {} meshes",
                    mt,
                    extra_targets.len() + 1
                ));
            }
            // Re-fold display mesh so viewport updates immediately.
            // Immediate: Properties paint is AFTER Hybrid mesh drain — mark would lag one frame.
            self.poly_sync_mesh_cache(nid);
            self.overlay_lines_fp = u64::MAX;
            self.mark_dirty(DirtyFlags::RENDER);
        }

        // Deferred so they observe the stack AFTER `mods` was written back to the graph.
        if pending_copy {
            self.copy_selected_poly_modifier();
        }
        if pending_paste {
            if let crate::clipboard::ClipboardContent::PolyMod(clip) = self.clipboard.content.clone()
            {
                self.paste_poly_modifier_into(&clip, nid);
            }
        }
    }

    /// Geometry channel UI (Modo-style filter): which faces/edges a modifier affects.
    /// Returns true if any control changed.
    /// `groups`: the active body's named polygroups, for the `Polygroup` kind's picker.
    pub(crate) fn draw_geo_channel_ui(
        ui: &mut egui::Ui,
        ch: &mut core_math::geometry_channel::GeometryChannel,
        id_salt: &str,
        groups: &[(u16, String)],
    ) -> bool {
        use core_math::geometry_channel::{channel_summary, GeoChannelKind};
        let mut changed = false;
        ui.separator();
        ui.label(
            egui::RichText::new("Geometry channel")
                .size(11.0)
                .strong()
                .color(egui::Color32::from_rgb(160, 200, 255)),
        );
        ui.label(
            egui::RichText::new("What to modify (Modo-style filter)")
                .size(9.5)
                .color(egui::Color32::from_rgb(120, 128, 145)),
        );
        let label = channel_summary(ch);
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(label)
            .width(160.0)
            .show_ui(ui, |ui| {
                for k in GeoChannelKind::all() {
                    if ui
                        .selectable_label(ch.kind == *k, k.label())
                        .clicked()
                    {
                        ch.kind = *k;
                        changed = true;
                    }
                }
            });
        match ch.kind {
            GeoChannelKind::NSides => {
                changed |= ui
                    .add(egui::Slider::new(&mut ch.n_sides, 3..=32).text("Sides"))
                    .on_hover_text("Exact number of vertices per face (3 = tri, 4 = quad, …).")
                    .changed();
            }
            GeoChannelKind::Material => {
                changed |= ui
                    .add(egui::DragValue::new(&mut ch.material_id).prefix("Material id:"))
                    .on_hover_text("Face material_id (0 = body default).")
                    .changed();
            }
            GeoChannelKind::EdgeLength => {
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::DragValue::new(&mut ch.edge_len_min).prefix("Min:").speed(0.001))
                        .changed();
                    changed |= ui
                        .add(egui::DragValue::new(&mut ch.edge_len_max).prefix("Max:").speed(0.001))
                        .changed();
                });
            }
            GeoChannelKind::EdgeAngle => {
                changed |= ui
                    .add(
                        egui::Slider::new(&mut ch.angle_min_deg, 0.0..=180.0)
                            .text("Angle min")
                            .suffix("°"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut ch.angle_max_deg, 0.0..=180.0)
                            .text("Angle max")
                            .suffix("°"),
                    )
                    .on_hover_text("Dihedral: 0° = coplanar, 90° = square corner.")
                    .changed();
            }
            GeoChannelKind::Polygroup => {
                if groups.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 170, 90),
                        "No polygroups on this body",
                    );
                    ui.weak(
                        "Select faces in Edit mode → Poly menu → \"Assign Selection to New Group\".",
                    );
                } else {
                    let sel = groups
                        .iter()
                        .find(|(id, _)| *id == ch.group_id)
                        .map(|(_, n)| n.clone())
                        .unwrap_or_else(|| format!("Group {}", ch.group_id));
                    egui::ComboBox::from_id_salt(format!("{id_salt}_pg"))
                        .selected_text(sel)
                        .show_ui(ui, |ui| {
                            for (id, name) in groups {
                                if ui
                                    .selectable_label(ch.group_id == *id, name)
                                    .clicked()
                                {
                                    ch.group_id = *id;
                                    changed = true;
                                }
                            }
                        });
                    ui.weak("Faces you marked into this group.");
                }
            }
            GeoChannelKind::EdgeWeight => {
                changed |= ui
                    .add(egui::Slider::new(&mut ch.weight_min, 0.0..=1.0).text("Weight min"))
                    .changed();
                changed |= ui
                    .add(egui::Slider::new(&mut ch.weight_max, 0.0..=1.0).text("Weight max"))
                    .changed();
                ui.weak(
                    "Authored per-edge weight. Where a modifier supports it the value is the \
                     STRENGTH, so a 0.5 edge is half-affected — not just in/out.",
                );
                ui.weak("Set it: Edit mode → Poly menu → \"Set Edge Weight\".");
            }
            _ => {}
        }
        if ch.kind != GeoChannelKind::All {
            changed |= ui
                .checkbox(&mut ch.invert, "Invert")
                .on_hover_text("Modify / select everything that does NOT match.")
                .changed();
        }
        changed
    }

    /// Select faces (or edges) on the active poly body that match a geometry channel.
    /// Face mode → face channel filters; Edge mode → edge filters preferred.
    pub(crate) fn poly_select_by_geo_channel(
        &mut self,
        ch: core_math::geometry_channel::GeometryChannel,
    ) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.toast("Select a poly body first");
                return;
            }
        };
        let Some(mesh) = self.poly_node_mesh(nid) else {
            self.toast("Active object is not a poly mesh");
            return;
        };

        let edge_mode =
            matches!(self.poly_work_mode, PolyWorkMode::Edge) || ch.kind.is_edge_filter();

        // Authored channels (polygroup / edge weight) read the sidecar — without it they match
        // nothing, so Select-by-Channel would silently come back empty for exactly the kinds the
        // user hand-authored. Built off the BASE mesh, which is the authoring domain here.
        let auth = self.poly_authored_channels(nid, &mesh);

        if edge_mode {
            let edges = ch.matching_edges_auth(&mesh, Some(&auth));
            self.poly_prefer_work_mode(PolyWorkMode::Edge);
            self.poly_sel.selected_faces.clear();
            self.poly_sel.selected_vertices.clear();
            self.poly_sel.selected_edges.clear();
            for e in &edges {
                self.poly_sel.selected_edges.insert(*e);
            }
            self.toast(&format!(
                "Geometry channel: {} edges ({})",
                edges.len(),
                core_math::geometry_channel::channel_summary(&ch)
            ));
        } else {
            let faces = ch.matching_faces_auth(&mesh, Some(&auth));
            self.poly_prefer_work_mode(PolyWorkMode::Face);
            self.poly_sel.selected_edges.clear();
            self.poly_sel.selected_vertices.clear();
            self.poly_sel.selected_faces.clear();
            for f in &faces {
                self.poly_sel.selected_faces.insert(*f);
            }
            self.toast(&format!(
                "Geometry channel: {} faces ({})",
                faces.len(),
                core_math::geometry_channel::channel_summary(&ch)
            ));
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }
}
