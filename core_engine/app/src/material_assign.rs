// core_engine/app/src/material_assign.rs
// Material Assignment System â€” Ctrl+M popup, face-level assignment, material library ops.

use std::sync::Arc;
use eframe::egui;
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, SceneItemId, SceneNodeId};

impl AzraApp {
    /// Draw the material assignment popup (Ctrl+M).
    /// ONE menu: search + list + bottom action that swaps Create â†” Assign.
    /// Selecting an existing material highlights it and shows Assign;
    /// clearing the selection (or never selecting) keeps the create-new fields.
    pub(crate) fn draw_mat_assign_popup(&mut self, ctx: &egui::Context) {
        if !self.ui.show_mat_assign { return; }

        let popup_pos = self.ui.mat_assign_pos;
        let mut close = false;
        let mut assign_id: Option<u32> = None;
        let mut remove_mat = false;
        let mut create_mat: Option<String> = None;

        // Snapshot current material info for the selected object/faces
        let has_current_mat = self.selected_material_id().is_some();

        // P5 (owner D1): preset choices for create-new â€” built-ins + user presets.
        let presets: Vec<crate::material_panel::preset::MaterialPreset> = {
            let mut v = crate::material_panel::preset::builtin_presets();
            v.extend(self.mp_user_presets().iter().cloned());
            v
        };

        // Seed a default new-name once if empty (always-visible create fields).
        if self.ui.mat_assign_new_name.is_empty() {
            self.ui.mat_assign_new_name =
                format!("Material {}", self.scene.scene_graph.material_count() + 1);
        }

        // Tooltip layer: above tool-options / docks. Viewport pick is blocked while open
        // via floating_popup_blocks_viewport (input runs earlier in the frame than this draw).
        let area_resp = egui::Area::new(egui::Id::new("mat_assign_popup"))
            .fixed_pos(popup_pos)
            .order(egui::Order::Tooltip)
            .interactable(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(240.0);
                    ui.set_max_width(300.0);

                    ui.label(egui::RichText::new("Assign Material").strong().size(13.0));
                    ui.separator();

                    // Remove material option (if selection has one)
                    if has_current_mat {
                        if ui.button("Remove Material").clicked() {
                            remove_mat = true;
                            close = true;
                        }
                        ui.separator();
                    }

                    // Search field
                    let search_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.ui.mat_assign_search)
                            .hint_text("Search materials…")
                            .desired_width(f32::INFINITY)
                    );
                    if search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }

                    // Material list (from scene graph Material nodes)
                    let search_lower = self.ui.mat_assign_search.to_lowercase();
                    let mats: Vec<(u32, String, [f32; 3])> = self.scene.scene_graph
                        .materials()
                        .filter(|(_, _, name, _)| search_lower.is_empty() || name.to_lowercase().contains(&search_lower))
                        .map(|(_, mid, name, data)| (mid.0, name.to_string(), [data.albedo.x, data.albedo.y, data.albedo.z]))
                        .collect();

                    let selected = self.ui.mat_assign_selected;
                    egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                        for (id, name, albedo) in &mats {
                            ui.horizontal(|ui| {
                                // Color swatch
                                let color = egui::Color32::from_rgb(
                                    (albedo[0] * 255.0) as u8,
                                    (albedo[1] * 255.0) as u8,
                                    (albedo[2] * 255.0) as u8,
                                );
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                ui.painter().rect_filled(rect, 2.0, color);
                                ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::GRAY), egui::StrokeKind::Middle);

                                let label = if name.is_empty() { format!("Material {}", id) } else { name.clone() };
                                let on = selected == Some(*id);
                                // Click = select (bottom becomes Assign). Double-click = assign+close.
                                let r = ui.selectable_label(on, &label);
                                if r.double_clicked() {
                                    assign_id = Some(*id);
                                    close = true;
                                } else if r.clicked() {
                                    if on {
                                        // Toggle off â†’ back to create mode
                                        self.ui.mat_assign_selected = None;
                                    } else {
                                        self.ui.mat_assign_selected = Some(*id);
                                    }
                                }
                            });
                        }
                        if mats.is_empty() && !search_lower.is_empty() {
                            ui.label(egui::RichText::new("No matching materials").weak().italics());
                        }
                        if mats.is_empty() && search_lower.is_empty() {
                            ui.label(egui::RichText::new("No materials yet â€” create one below").weak().italics());
                        }
                    });

                    ui.separator();

                    // Bottom action: Assign (when a list item is selected) OR create-new fields.
                    if let Some(sel) = self.ui.mat_assign_selected {
                        let label = mats
                            .iter()
                            .find(|(id, _, _)| *id == sel)
                            .map(|(_, n, _)| {
                                if n.is_empty() { format!("Material {}", sel) } else { n.clone() }
                            })
                            .unwrap_or_else(|| format!("Material {}", sel));
                        ui.label(
                            egui::RichText::new(format!("Selected: {label}"))
                                .size(11.0)
                                .color(egui::Color32::from_gray(180)),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(
                                    egui::vec2(ui.available_width() - 72.0, 24.0),
                                    egui::Button::new("Assign"),
                                )
                                .clicked()
                            {
                                assign_id = Some(sel);
                                close = true;
                            }
                            if ui.button("New…").on_hover_text("Clear selection and create a new material").clicked() {
                                self.ui.mat_assign_selected = None;
                            }
                        });
                    } else {
                        // Always-visible create-new — SHADER TYPE (not a quality tier).
                        // Octane-style: Diffuse / Standard PBR / Glass / SSS / … at creation.
                        ui.horizontal(|ui| {
                            ui.label("Shader type:");
                            egui::ComboBox::from_id_salt("mat_assign_preset")
                                .width(150.0)
                                .selected_text(self.ui.mat_assign_preset.clone())
                                .show_ui(ui, |ui| {
                                    for p in &presets {
                                        ui.selectable_value(
                                            &mut self.ui.mat_assign_preset,
                                            p.name.clone(),
                                            &p.name,
                                        );
                                    }
                                });
                        })
                        .response
                        .on_hover_text(
                            "Material shader model for the new material — not a quality tier. \
                             Standard PBR, Diffuse, Glass, Skin/SSS, VFX, Advanced each open with \
                             the matching channel set.",
                        );
                        ui.horizontal(|ui| {
                            ui.label("Name:");
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.ui.mat_assign_new_name)
                                    .desired_width(120.0)
                            );
                            let do_create = (resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                                || ui.button("Create").clicked();
                            if do_create {
                                let name = self.ui.mat_assign_new_name.trim().to_string();
                                if !name.is_empty() {
                                    create_mat = Some(name);
                                    close = true;
                                }
                            }
                        });
                    }
                });
            });

        // Click outside closes popup — use the real Area rect (not a guessed 300×380 box).
        // Viewport pick is already blocked while open, so this click does not select mesh.
        if ctx.input(|i| i.pointer.any_click()) && !close {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                let popup_rect = area_resp.response.rect.expand(4.0);
                if !popup_rect.contains(pos) {
                    close = true;
                }
            }
        }

        // Escape closes
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }

        // Process actions
        if let Some(name) = create_mat {
            let preset = presets
                .iter()
                .find(|p| p.name == self.ui.mat_assign_preset)
                .cloned()
                .unwrap_or_else(crate::material_panel::preset::preset_standard_pbr);
            self.create_material_with_preset(&name, &preset);
        } else if let Some(id) = assign_id {
            let name = self.scene.scene_graph.material_by_id(core_math::primitives::MaterialId(id))
                .map(|(_, m)| m.name.clone()).unwrap_or_default();
            let label = if name.is_empty() { format!("Material {}", id) } else { name };
            if !self.has_material_assign_target() {
                self.toast("Select a prim, SDF body, poly object, or faces first");
            } else if self.selection_material_already(id) {
                self.toast(&format!("{} already on selection", label));
            } else if self.assign_material_to_selection(id) {
                let n_faces = self.poly_sel.selected_faces.len();
                let face_hint = if matches!(
                    self.poly_work_mode,
                    crate::PolyWorkMode::Face | crate::PolyWorkMode::Group
                ) && n_faces > 0
                {
                    format!(" → {n_faces} face(s)")
                } else {
                    String::new()
                };
                self.toast(&format!("{label} assigned{face_hint}"));
            } else {
                self.toast("Could not assign material to this selection");
            }
        } else if remove_mat {
            if !self.has_material_assign_target() {
                self.toast("Select a prim, SDF body, or poly object first");
            } else if self.assign_material_to_selection(0) {
                self.toast("Material removed from selection");
            } else {
                self.toast("Nothing to remove (already embedded / no material)");
            }
        }

        if close {
            self.ui.show_mat_assign = false;
            self.ui.mat_assign_search.clear();
            self.ui.mat_assign_selected = None;
            self.ui.mat_assign_new_name.clear();
        }
    }

    // ============================================================
    // Material node actions (outliner Materials section + menus)
    // ============================================================

    /// Create a new graph material — open the assign popup in CREATE mode so the
    /// user picks a SHADER TYPE (Standard PBR / Glass / SSS / …) before the material
    /// exists. Octane-style: type is a create-time choice, not a panel tier switch.
    pub(crate) fn create_material_interactive(&mut self) {
        self.ui.show_mat_assign = true;
        self.ui.mat_assign_selected = None; // create mode, not assign
        self.ui.mat_assign_search.clear();
        if self.ui.mat_assign_new_name.is_empty() {
            self.ui.mat_assign_new_name = self.next_unique_name("Material");
        }
        // Place near viewport centre so the create fields are obvious.
        let rect = self.viewport.viewport_rect;
        if rect.width() > 1.0 && rect.height() > 1.0 {
            self.ui.mat_assign_pos = egui::pos2(rect.center().x - 120.0, rect.center().y - 80.0);
        }
    }

    /// Create a shared graph material SEEDED from a prim's embedded material and assign it to that
    /// prim â€” the prim material-combo's "+ New Material" entry (the prim's current look becomes the
    /// shared material, so nothing visibly changes on creation). One undo entry.
    pub(crate) fn create_shared_material_from_prim(&mut self, idx: usize) {
        if idx >= self.scene.primitives.len() { return; }
        let name = self.next_unique_name("Material");
        self.transaction("New Material", |s| {
            let seed = s.scene.primitives[idx].material.clone();
            let mid = s.scene.scene_graph.add_material(&name, seed);
            s.ui
                .material_node_graphs
                .insert(mid.0, crate::shader_nodes::NodeGraph::default());
            s.write_prim(idx, |p| p.material_id = mid);
            s.cache.prim_buf_stale = true;
            s.mark_dirty(DirtyFlags::RENDER);
            s.outliner.outliner_dirty = true;
        });
        self.toast(&format!("{} created and assigned", name));
    }
    /// NEW-1 — promote the EMBEDDED material the current selection actually renders
    /// into a real Material graph node, and assign it. Returns the new id.
    ///
    /// Why this exists: `resolve_prim_material` falls back to a prim's embedded
    /// `material` when `material_id` is NONE, and a fresh scene ships with ZERO
    /// Material nodes (`project_new.rs:116-123`) — so what a new scene renders is
    /// an embedded material that the v2 Material page (keyed entirely by
    /// `MaterialId`: panel docs, node graphs, bake conflicts, presets) cannot
    /// address. Promotion gives that look a real id, after which the whole panel
    /// works on it normally.
    ///
    /// NON-DESTRUCTIVE: the new material is SEEDED from the embedded one, so the
    /// object's appearance is unchanged at the moment of promotion.
    ///
    /// Selection routing mirrors `props_material_resolve` (view.rs) exactly, so
    /// the button promotes the same thing the panel was showing:
    /// 1. active prim → that prim; 2. SDF body → every prim in the body;
    /// 3. poly body → every face. One undo entry in all cases.
    pub(crate) fn promote_embedded_material_for_selection(
        &mut self,
        mut authored_graph: Option<crate::shader_nodes::NodeGraph>,
    ) -> Option<core_math::primitives::MaterialId> {
        let name = self.next_unique_name("Material");
        let mut created: Option<core_math::primitives::MaterialId> = None;

        // 1 — active prim (the common case: a selected SDF primitive).
        if let Some(idx) = self.active_prim_index_unified() {
            if idx < self.scene.primitives.len() {
                self.transaction("Create material from object", |s| {
                    let seed = s.scene.primitives[idx].material.clone();
                    let mid = s.create_material_with_graph(&name, seed, authored_graph.take());
                    s.write_prim(idx, |p| p.material_id = mid);
                    s.cache.prim_buf_stale = true;
                    s.mark_dirty(DirtyFlags::RENDER);
                    s.outliner.outliner_dirty = true;
                    created = Some(mid);
                });
            }
        }

        // 2 / 3 — a body is active: promote the whole body's look.
        if created.is_none() {
            let nid = self.sel.usel.active_node()?;
            match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { .. }) => {
                    let seed = self
                        .scene
                        .scene_graph
                        .body_primitives(nid)
                        .and_then(|ps| ps.first().map(|p| p.material.clone()))
                        .unwrap_or_default();
                    self.transaction("Create material from object", |s| {
                        let mid = s.create_material_with_graph(&name, seed, authored_graph.take());
                        if let Some(prims) = s.scene.scene_graph.body_primitives_mut(nid) {
                            for p in prims.iter_mut() {
                                p.material_id = mid;
                            }
                        }
                        s.sync_flat_cache();
                        s.cache.prim_buf_stale = true;
                        s.mark_dirty(DirtyFlags::RENDER);
                        s.outliner.outliner_dirty = true;
                        created = Some(mid);
                    });
                }
                Some(NodePayload::PolygonalBody { material, .. }) => {
                    // Face material_id is 0 here (else the panel would have
                    // resolved a real material), so the body's embedded material
                    // IS what renders — seed from it.
                    let seed = material.clone();
                    self.transaction("Create material from object", |s| {
                        let mid = s.create_material_with_graph(&name, seed, authored_graph.take());
                        s.apply_material_all_faces_no_undo(nid, mid.0);
                        s.cache.prim_buf_stale = true;
                        s.mark_dirty(DirtyFlags::RENDER);
                        s.outliner.outliner_dirty = true;
                        created = Some(mid);
                    });
                }
                _ => return None,
            }
        }

        if created.is_some() {
            self.toast(&format!("{name} created from this object"));
        }
        created
    }

    fn create_material_with_graph(
        &mut self,
        name: &str,
        mut material: core_math::pbr_material::PbrMaterial,
        authored_graph: Option<crate::shader_nodes::NodeGraph>,
    ) -> core_math::primitives::MaterialId {
        let graph = match authored_graph {
            Some(graph) => {
                graph.push_to_pbr(&mut material);
                graph
            }
            None => {
                let mut graph = crate::shader_nodes::NodeGraph::default();
                graph.pull_from_pbr(&material);
                graph
            }
        };
        let mid = self.scene.scene_graph.add_material(name, material);
        self.ui.material_node_graphs.insert(mid.0, graph);
        mid
    }

    /// Duplicate a material node (values copied, fresh MaterialId, unique name). One undo entry.
    pub(crate) fn duplicate_material_node(&mut self, nid: core_math::scene_graph::NodeId) {
        let Some((name, data)) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::Material { name, data, .. } => Some((name.clone(), data.clone())),
            _ => None,
        }) else { return };
        let new_name = self.next_unique_name(&name);
        let mut created: Option<core_math::scene_graph::NodeId> = None;
        // Copy shader graph if present (unified system).
        let src_mid = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                NodePayload::Material { id, .. } => Some(*id),
                _ => None,
            });
        let graph_clone = src_mid.and_then(|id| self.ui.material_node_graphs.get(&id.0).cloned());
        self.transaction("Duplicate material", |s| {
            let mid = s.scene.scene_graph.add_material(&new_name, data);
            if let Some(g) = graph_clone {
                s.ui.material_node_graphs.insert(mid.0, g);
            } else {
                s.ui
                    .material_node_graphs
                    .insert(mid.0, crate::shader_nodes::NodeGraph::default());
            }
            created = s.scene.scene_graph.material_node_id(mid);
            s.outliner.outliner_dirty = true;
        });
        if let Some(new_nid) = created {
            self.select_single_unified(SceneItemId::Node(new_nid));
        }
    }

    /// Delete a material node WITH the in-use guard: every user (prim `material_id`, poly face
    /// `material_id`) is reverted to the default material in the SAME transaction (the friendlier
    /// option vs blocking ÔÇö one Ctrl+Z restores the material AND all assignments), and the toast
    /// names what was reverted. Clearing refs is mandatory, not cosmetic: `add_material` allocates
    /// max+1, so a dangling id could be REUSED by the next material and silently re-skin old users.
    pub(crate) fn delete_material_node(&mut self, nid: core_math::scene_graph::NodeId) {
        let Some((mid, name)) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::Material { id, name, .. } => Some((*id, name.clone())),
            _ => None,
        }) else { return };
        let (prim_refs, face_refs) = self.scene.scene_graph.material_use_counts(mid);
        // Route through the SHARED per-node deletion core (two-code-paths rule) ÔÇö it clears the
        // material's references (prims ÔåÆ NONE, faces ÔåÆ 0) and deselects; we add the flat-cache
        // sync + render marks the Del-key path also performs.
        self.transaction("Delete material", |s| {
            s.delete_node_no_undo(nid);
            s.sync_flat_cache();               // flat prim cache carries material_id
            s.sanitize_selection_unified();    // the deleted node may have been selected
            s.mark_dirty(DirtyFlags::RENDER);
            s.outliner.outliner_dirty = true;
        });
        let label = if name.is_empty() { format!("Material {}", mid.0) } else { name };
        if prim_refs + face_refs > 0 {
            self.toast(&format!(
                "Deleted '{}' ÔÇö {} prim(s) and {} face(s) reverted to default (Ctrl+Z restores)",
                label, prim_refs, face_refs));
        } else {
            self.toast(&format!("Deleted '{}'", label));
        }
    }

    /// Clear ONE map channel of a graph material (outliner child-row "Clear Map" ÔÇö the same
    /// mutation as the editor's per-slot Clear button, routed through a transaction because the
    /// outliner defers actions instead of using the editor's snapshot write-back). Undoable;
    /// the texture-array SLOT is untouched (slots are append-only; only the reference clears).
    pub(crate) fn clear_material_map_channel(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        kind: core_math::pbr_material::MapKind,
    ) {
        // Already clear ÔåÆ no empty undo.
        let already_clear = self.scene.scene_graph.get(nid).map(|n| {
            match &n.payload {
                NodePayload::Material { data, .. } => !data.map(kind).is_set(),
                _ => true,
            }
        }).unwrap_or(true);
        if already_clear {
            return;
        }
        self.transaction("Clear map", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::Material { data, .. } = &mut node.payload {
                    *data.map_mut(kind) = Default::default();
                }
            }
            // Same marks as the material editor's write-back: materials never move geometry ÔÇö
            // re-upload + re-render, no bake.
            s.cache.prim_buf_stale = true;
            s.mark_dirty(DirtyFlags::RENDER);
            s.outliner.outliner_dirty = true;
        });
        self.toast(&format!("Cleared {} map (Ctrl+Z restores)", kind.label()));
    }

    /// Get the material_id of the current selection (for the "Remove Material" row).
    /// Covers SDF prims, SDF body prims, and poly faces/bodies.
    fn selected_material_id(&self) -> Option<u32> {
        // SDF prim selection first
        for &idx in &self.selected_prim_indices_unified() {
            if idx < self.scene.primitives.len() {
                let mid = self.scene.primitives[idx].material_id;
                if !mid.is_none() {
                    return Some(mid.0);
                }
            }
        }
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => {
                if let Some(&fid) = self.poly_sel.selected_faces.iter().next() {
                    if (fid as usize) < mesh.faces.len() {
                        let mid = mesh.face(core_math::half_edge::FaceId(fid)).material_id;
                        if mid > 0 {
                            return Some(mid);
                        }
                    }
                }
                // Object-level: first face with a material
                mesh.faces.iter().find(|f| f.material_id > 0).map(|f| f.material_id)
            }
            NodePayload::SdfBody { .. } => self
                .scene
                .scene_graph
                .body_primitives(nid)
                .and_then(|ps| ps.iter().find(|p| !p.material_id.is_none()).map(|p| p.material_id.0)),
            _ => None,
        }
    }

    /// True when every live face already carries `mat_id`.
    fn poly_body_material_already(&self, nid: core_math::scene_graph::NodeId, mat_id: u32) -> bool {
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => {
                mesh.faces.iter().all(|f| f.material_id == mat_id
                    || f.half_edge == core_math::half_edge::NO_EDGE)
            }
            // Non-poly nodes are not "already assigned" here ÔÇö callers must branch by payload.
            _ => false,
        }
    }

    /// True when assignment would change nothing (empty undo prevention).
    /// Covers **SDF prims**, **SdfBody prims**, and **poly faces/bodies**.
    ///
    /// BUGFIX (2026-07-19): the old poly-only helper returned `true` when `active_node()` was
    /// `None` (typical for `SceneItemId::Prim` selection) and also treated every non-poly
    /// payload as "already assigned" ÔÇö so Ctrl+M on an SDF cube never wrote `material_id`.
    pub(crate) fn selection_material_already(&self, mat_id: u32) -> bool {
        use core_math::primitives::MaterialId;
        let mid = MaterialId(mat_id);

        // 1) Selected SDF prims (unified selection ÔÇö the common Ctrl+M case)
        let prim_idxs = self.selected_prim_indices_unified();
        if !prim_idxs.is_empty() {
            return prim_idxs.iter().all(|&idx| {
                self.scene
                    .primitives
                    .get(idx)
                    .map(|p| p.material_id == mid)
                    .unwrap_or(true)
            });
        }

        // 2) Node selection (body / poly)
        let mut nodes: Vec<core_math::scene_graph::NodeId> =
            self.sel.usel.selected_nodes().collect();
        if nodes.is_empty() {
            if let Some(n) = self.sel.usel.active_node() {
                nodes.push(n);
            }
        }
        if nodes.is_empty() {
            // Nothing assignable selected ÔÇö treat as no-op (caller shows "select first").
            return true;
        }

        let in_face_mode = matches!(
            self.poly_work_mode,
            crate::PolyWorkMode::Face | crate::PolyWorkMode::Group
        ) && !self.poly_sel.selected_faces.is_empty();

        nodes.iter().all(|&nid| {
            match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::PolygonalBody { mesh, .. }) => {
                    if in_face_mode {
                        self.poly_sel.selected_faces.iter().all(|&fid| {
                            (fid as usize) >= mesh.faces.len()
                                || mesh.faces[fid as usize].material_id == mat_id
                        })
                    } else {
                        self.poly_body_material_already(nid, mat_id)
                    }
                }
                Some(NodePayload::SdfBody { .. }) => self
                    .scene
                    .scene_graph
                    .body_primitives(nid)
                    .map(|ps| ps.is_empty() || ps.iter().all(|p| p.material_id == mid))
                    .unwrap_or(true),
                // Material / light / etc. ÔÇö not assignable; ignore for already-check
                // (apply_material_to_selection also skips them).
                _ => true,
            }
        })
    }

    /// True if selection includes something material assignment can write
    /// (SDF prim, SdfBody, or PolygonalBody) ÔÇö not a Material/Light/etc. node alone.
    pub(crate) fn has_material_assign_target(&self) -> bool {
        if !self.selected_prim_indices_unified().is_empty() {
            return true;
        }
        let mut nodes: Vec<core_math::scene_graph::NodeId> =
            self.sel.usel.selected_nodes().collect();
        if nodes.is_empty() {
            if let Some(n) = self.sel.usel.active_node() {
                nodes.push(n);
            }
        }
        nodes.iter().any(|&nid| {
            matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::PolygonalBody { .. } | NodePayload::SdfBody { .. })
            )
        })
    }

    /// Assign a material ID to the current selection (with undo).
    /// SDF prims, SDF bodies, poly faces/bodies. Returns false if nothing assignable was
    /// selected, or if the selection already has this material (no empty undo).
    pub(crate) fn assign_material_to_selection(&mut self, mat_id: u32) -> bool {
        if !self.has_material_assign_target() {
            return false;
        }
        if self.selection_material_already(mat_id) {
            return false;
        }
        let mut ok = false;
        self.transaction("Assign Material", |s| {
            ok = s.apply_material_to_selection(mat_id);
        });
        ok
    }

    /// Inner: apply material to selection without capturing undo.
    /// Called from transaction closures that already have an undo snapshot.
    ///
    /// Targets (in order):
    /// 1. Selected SDF prims ÔåÆ `SdfPrimitive.material_id`
    /// 2. Selected SdfBody node(s) ÔåÆ every prim in the body
    /// 3. Poly face mode ÔåÆ selected faces
    /// 4. Poly body object mode ÔåÆ all faces
    ///
    /// Returns true if at least one target was written.
    pub(crate) fn apply_material_to_selection(&mut self, mat_id: u32) -> bool {
        use core_math::primitives::MaterialId;
        let mid = MaterialId(mat_id);
        let mut wrote = false;

        // --- 1) SDF prim selection (Ctrl+M on a selected cube/sphere/etc.) ---
        let prim_idxs = self.selected_prim_indices_unified();
        if !prim_idxs.is_empty() {
            for idx in prim_idxs {
                if idx < self.scene.primitives.len() {
                    self.write_prim(idx, |p| p.material_id = mid);
                    wrote = true;
                }
            }
            if wrote {
                self.cache.prim_buf_stale = true;
                self.mark_dirty(DirtyFlags::RENDER);
                self.outliner.outliner_dirty = true;
            }
            return wrote;
        }

        // --- 2) Node selection (body / poly object) ---
        let mut nodes: Vec<core_math::scene_graph::NodeId> =
            self.sel.usel.selected_nodes().collect();
        if nodes.is_empty() {
            if let Some(n) = self.sel.usel.active_node() {
                nodes.push(n);
            }
        }
        if nodes.is_empty() {
            return false;
        }

        let in_face_mode = matches!(
            self.poly_work_mode,
            crate::PolyWorkMode::Face | crate::PolyWorkMode::Group
        ) && !self.poly_sel.selected_faces.is_empty();

        let mut poly_nids_need_resync: Vec<core_math::scene_graph::NodeId> = Vec::new();
        for nid in nodes {
            let payload_kind = self.scene.scene_graph.get(nid).map(|n| match &n.payload {
                NodePayload::PolygonalBody { .. } => 0u8,
                NodePayload::SdfBody { .. } => 1u8,
                _ => 2u8,
            });
            match payload_kind {
                Some(0) => {
                    // Poly body
                    if in_face_mode {
                        let mut face_wrote = false;
                        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                                let mesh = Arc::make_mut(mesh);
                                for &fid in &self.poly_sel.selected_faces {
                                    if (fid as usize) < mesh.faces.len() {
                                        mesh.faces[fid as usize].material_id = mat_id;
                                        face_wrote = true;
                                    }
                                }
                            }
                        }
                        if face_wrote {
                            wrote = true;
                            poly_nids_need_resync.push(nid);
                        }
                    } else {
                        self.apply_material_all_faces_no_undo(nid, mat_id);
                        wrote = true;
                        poly_nids_need_resync.push(nid);
                    }
                }
                Some(1) => {
                    // SDF body ÔåÆ all prims in the body
                    if let Some(prims) = self.scene.scene_graph.body_primitives_mut(nid) {
                        for p in prims.iter_mut() {
                            p.material_id = mid;
                            wrote = true;
                        }
                    }
                    if wrote {
                        self.sync_flat_cache();
                    }
                }
                _ => {}
            }
        }

        if wrote {
            self.cache.prim_buf_stale = true;
            self.mark_dirty(DirtyFlags::RENDER);
            self.outliner.outliner_dirty = true;
            // Rebuild solid mesh so multi-material index ranges update (face assign is visual).
            for nid in poly_nids_need_resync {
                self.poly_force_display_resync(nid);
            }
        }
        wrote
    }

    /// Inner: write `mat_id` onto EVERY face of a poly body (object-level assignment —
    /// per-face `HFace.material_id`). No undo — call inside a transaction.
    pub(crate) fn apply_material_all_faces_no_undo(&mut self, nid: core_math::scene_graph::NodeId, mat_id: u32) {
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mesh = Arc::make_mut(mesh);
                for face in &mut mesh.faces {
                    face.material_id = mat_id;
                }
            }
        }
    }

    /// Assign a graph material to a whole poly body (all faces) in ONE transaction ÔÇö the poly
    /// properties Material combo. `mat_id == 0` clears back to the embedded material.
    /// Already-assigned ÔåÆ no empty undo entry.
    pub(crate) fn assign_material_to_poly_body(&mut self, nid: core_math::scene_graph::NodeId, mat_id: u32) {
        if self.poly_body_material_already(nid, mat_id) {
            return;
        }
        self.transaction("Assign Material", |s| {
            s.apply_material_all_faces_no_undo(nid, mat_id);
            s.mark_dirty(DirtyFlags::RENDER);
            s.outliner.outliner_dirty = true;
            s.poly_force_display_resync(nid);
        });
    }

    /// Assign a graph material to every prim of an SdfBody in ONE transaction ÔÇö the SDF body
    /// properties Material combo. `MaterialId::NONE` reverts prims to their embedded materials.
    /// Already-assigned ÔåÆ no empty undo entry.
    pub(crate) fn assign_material_to_body_prims(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        mid: core_math::primitives::MaterialId,
    ) {
        let already = self.scene.scene_graph.body_primitives(nid)
            .map(|prims| prims.iter().all(|p| p.material_id == mid))
            .unwrap_or(true);
        if already {
            return;
        }
        self.transaction("Assign Material", |s| {
            if let Some(prims) = s.scene.scene_graph.body_primitives_mut(nid) {
                for p in prims.iter_mut() { p.material_id = mid; }
            }
            s.sync_flat_cache();               // flat cache mirrors prim material_id
            s.cache.prim_buf_stale = true;     // resolved material data rides the prim buffer
            s.mark_dirty(DirtyFlags::RENDER);
        });
    }

    /// Outliner material menu: select every poly face that uses `mat_id`, entering Face mode
    /// on the best poly body. Also multi-selects SDF prims that reference the same material.
    /// Owner 2026-08-06: "Material to Selection — select all the poly with this material".
    pub(crate) fn select_by_material(&mut self, mat_id: u32) {
        use core_math::primitives::MaterialId;
        let mid = MaterialId(mat_id);

        // Prefer the active poly body if it has matching faces; else first poly body with any.
        let active = self.sel.usel.active_node();
        let mut best: Option<(core_math::scene_graph::NodeId, Vec<u32>)> = None;
        let mut active_hit: Option<(core_math::scene_graph::NodeId, Vec<u32>)> = None;

        for (nid, node) in self.scene.scene_graph.all_nodes() {
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                continue;
            };
            let faces: Vec<u32> = mesh
                .faces
                .iter()
                .enumerate()
                .filter(|(_, f)| f.material_id == mat_id)
                .map(|(i, _)| i as u32)
                .collect();
            if faces.is_empty() {
                continue;
            }
            if Some(nid) == active {
                active_hit = Some((nid, faces));
            } else if best.is_none() {
                best = Some((nid, faces));
            }
        }
        let poly_hit = active_hit.or(best);

        // SDF prims with this material_id
        let prim_uids: Vec<u32> = self
            .scene
            .primitives
            .iter()
            .filter(|p| p.material_id == mid)
            .map(|p| p.prim_uid)
            .collect();

        if let Some((nid, faces)) = poly_hit {
            self.modeling_context = crate::ModelingContext::Polygonal;
            self.select_single_unified(SceneItemId::Node(nid));
            self.poly_prefer_work_mode(crate::PolyWorkMode::Face);
            self.poly_sel.selected_faces.clear();
            self.poly_sel.selected_edges.clear();
            self.poly_sel.selected_vertices.clear();
            for fid in &faces {
                self.poly_sel.selected_faces.insert(*fid);
            }
            // Additive: keep prims that also use this material in the object selection.
            for uid in &prim_uids {
                self.sel.usel.selected.insert(SceneItemId::Prim(*uid));
            }
            self.outliner.outliner_dirty = true;
            self.mark_dirty(DirtyFlags::RENDER);
            let n = faces.len();
            let extra = if prim_uids.is_empty() {
                String::new()
            } else {
                format!(" + {} prim(s)", prim_uids.len())
            };
            self.toast(&format!("Selected {n} face(s) with this material{extra}"));
            return;
        }

        if !prim_uids.is_empty() {
            self.clear_selection_unified();
            for uid in &prim_uids {
                self.sel.usel.selected.insert(SceneItemId::Prim(*uid));
            }
            if let Some(&first) = prim_uids.first() {
                self.sel.usel.active = Some(SceneItemId::Prim(first));
                self.sel.usel.last_clicked = Some(SceneItemId::Prim(first));
            }
            self.outliner.outliner_dirty = true;
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast(&format!(
                "Selected {} prim(s) with this material",
                prim_uids.len()
            ));
            return;
        }

        self.toast("No geometry uses this material");
    }
}

#[cfg(test)]
mod reassign_tests {
    //! RASTER-MATERIAL-REASSIGN-STALE (2026-10-13): the raster structural draw list caches each
    //! drawable's RESOLVED material and re-gathers only when `material_generation` moves (or
    //! SCENE/MESH). Re-assigning an EXISTING material moved neither, so the raster kept the
    //! previous material while PT showed the new one. These drive the real write funnels.
    use crate::AzraApp;
    use core_math::pbr_material::PbrMaterial;
    use core_math::primitives::{SdfPrimitive, SdfShape};

    fn app_with_sphere() -> AzraApp {
        let mut app = AzraApp::new_headless();
        app.add_prim_to_graph(SdfPrimitive::new(
            "s".into(),
            SdfShape::Sphere { radius: 0.8 },
            glam::Vec3::ZERO,
        ));
        app
    }

    #[test]
    fn reassigning_existing_material_moves_material_generation() {
        let mut app = app_with_sphere();
        let a = app.scene.scene_graph.add_material("A", PbrMaterial::default());
        let b = app.scene.scene_graph.add_material("B", PbrMaterial::default());
        app.transaction("assign A", |s| s.write_prim(0, |p| p.material_id = a));
        app.transaction("assign B", |s| s.write_prim(0, |p| p.material_id = b));
        // The bug: both materials already exist — no add_material bump can rescue this one.
        let g0 = app.scene.scene_graph.material_generation();
        app.transaction("re-assign A", |s| s.write_prim(0, |p| p.material_id = a));
        assert_ne!(app.scene.scene_graph.material_generation(), g0,
            "write_prim material reassign must invalidate the raster structural material table");

        // Body-wide graph-direct path (SDF body combo / Ctrl+M on the body) → sync_flat_cache.
        let nid = app.scene.prim_body_map[0].0;
        let g1 = app.scene.scene_graph.material_generation();
        app.assign_material_to_body_prims(nid, b);
        assert_eq!(app.scene.primitives[0].material_id, b);
        assert_ne!(app.scene.scene_graph.material_generation(), g1,
            "body-wide reassign (sync_flat_cache funnel) must invalidate too");
    }

    #[test]
    fn embedded_material_edit_moves_material_generation() {
        let mut app = app_with_sphere();
        let g0 = app.scene.scene_graph.material_generation();
        app.write_prim(0, |p| p.material.roughness = 0.123);
        assert_ne!(app.scene.scene_graph.material_generation(), g0);
    }

    /// Sabotage guard: non-material prim writes and unchanged flat-cache syncs must NOT bump —
    /// otherwise every drag frame / sync would force a structural re-gather.
    #[test]
    fn non_material_writes_leave_material_generation_alone() {
        let mut app = app_with_sphere();
        let g0 = app.scene.scene_graph.material_generation();
        app.write_prim(0, |p| p.shape = SdfShape::Sphere { radius: 0.5 });
        app.sync_flat_cache();
        assert_eq!(app.scene.scene_graph.material_generation(), g0);
    }
}
