// App-bar menus (File…Help). Keep shell.rs for chrome layout only.
//
// STRUCTURE (owner-confirmed 2026-07-24): File · Edit · Add · Select · Object ·
// Mesh · Materials · Render · View · Window · Help. Comprehensive — every engine
// feature reachable here, checklist = docs/UI/UNWIRED_ACTIONS.md.
//
// DISPATCH RULE (INTEGRATION_RULES §0): every item calls the SAME fn the v1
// menu / keybinding action already calls (ops::*, add_light, set_prims_boolean_op,
// apply_sdf_bool_pie, assign_material_to_selection, …). No reimplemented mutation.
//
// New menu items: shared fillers / label(ACT_*) — see INTEGRATION_RULES.md.
use crate::ui_menus::DefaultMenu;
use crate::ui_menus::AzraMenus;
use eframe::egui;
use crate::*;
use super::*;
use glam::Vec3;

impl FourthLayerApp {
    // ── Shortcut-hint helper ──────────────────────────────────────────────
    // ONE helper, used everywhere: label left, weak chord right-aligned (egui
    // Button::shortcut_text). `act` is looked up in ACTION_DEFS; an empty chord
    // (unbound action) renders as a plain button. Never invents a key label.
    fn menu_item_with_shortcut(
        ui: &mut egui::Ui,
        label: &str,
        chord: &str,
    ) -> egui::Response {
        let btn = egui::Button::new(label).shortcut_text(chord);
        ui.add(btn)
    }

    /// Same, but resolves the chord from an ACTION_DEFS id via keybindings.label.
    fn menu_act(&self, ui: &mut egui::Ui, label: &str, act: &str) -> egui::Response {
        let chord = self.keybindings.label(act);
        Self::menu_item_with_shortcut(ui, label, &chord)
    }

    // ── File ──────────────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_file(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.azra_menu_button("File", |ui| {
            ui.set_min_width(220.0);
            self.menu_fill_file_scene(ui, crate::menu_shared::MenuShortcutStyle::Tab);
            ui.separator();
            self.menu_fill_file_defaults(ui);
            ui.separator();
            ui.azra_menu_button("Import", |ui| {
                if ui.button("OBJ as SDF…").clicked() {
                    self.ui2_import_obj_as_sdf();
                    ui.close_menu();
                }
                if ui.button("glTF Mesh…").clicked() {
                    self.import_gltf_dialog();
                    ui.close_menu();
                }
                if ui.button("JSON Scene…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Import JSON")
                        .add_filter("JSON", &["json"])
                        .pick_file()
                    {
                        self.load_scene_from_path(&path);
                    }
                    ui.close_menu();
                }
            });
            ui.azra_menu_button("Export", |ui| {
                if self
                    .menu_act(ui, "Mesh OBJ…", ACT_EXPORT)
                    .clicked()
                {
                    self.ui.show_export_window = true;
                    ui.close_menu();
                }
                if ui.button("glTF (.glb)…").clicked() {
                    self.ui.show_gltf_export = true;
                    ui.close_menu();
                }
                if ui.button("JSON Scene…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Export JSON")
                        .add_filter("JSON", &["json"])
                        .save_file()
                    {
                        self.voxel_sync_node_mesh();
                        let result = crate::project::ProjectFile::from_app(self)
                            .and_then(|project| crate::project::save_json(&path, &project));
                        match result {
                            Ok(()) => self.toast("Exported JSON"),
                            Err(e) => self.toast(&format!("Export failed: {e}")),
                        }
                    }
                    ui.close_menu();
                }
            });
            if !self.project.recent_files.is_empty() {
                ui.separator();
                ui.azra_menu_button("Recent Files", |ui| {
                    let recent = self.project.recent_files.clone();
                    for path in &recent {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("?");
                        if ui
                            .button(name)
                            .on_hover_text(path.display().to_string())
                            .clicked()
                        {
                            self.load_scene_from_path(path);
                            ui.close_menu();
                        }
                    }
                });
            }
            ui.separator();
            if ui.button("Quit").clicked() {
                if self.project.scene_modified {
                    self.ui.pending_action_after_save = Some(PendingAction::Quit);
                    self.ui.show_unsaved_dialog = true;
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ui.close_menu();
            }
        });
    }

    fn ui2_import_obj_as_sdf(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Wavefront OBJ", &["obj"])
            .set_title("Import OBJ as SDF Volume")
            .pick_file()
        {
            let path_str = path.to_string_lossy().to_string();
            let res = self.cache.cpu_volume.resolution[0];
            let bounds = self.cache.cpu_volume.grid_bounds;
            match io_bridge::mesh_to_sdf::obj_to_sdf(&path_str, res, bounds) {
                Ok(result) => {
                    let mesh_name = result.mesh_name.clone();
                    let vertex_count = result.vertex_count;
                    let face_count = result.face_count;
                    self.transaction("Import OBJ to SDF", |s| {
                        s.cache.cpu_volume.data = result.data;
                        s.cache
                            .cpu_volume
                            .mark_dirty([0, 0, 0], [res - 1, res - 1, res - 1]);
                        if let Some(prims) =
                            s.scene.scene_graph.body_primitives_mut(s.scene.sdf_body_nid)
                        {
                            prims.clear();
                        }
                        s.sync_flat_cache();
                        s.deselect_all();
                        s.voxel_undo_reset();
                        s.sculpt.current_stroke.clear();
                        s.sculpt.stroke_active = false;
                        s.tool.work_mode = WorkMode::Sculpt;
                        s.mark_dirty(DirtyFlags::SCENE);
                    });
                    self.toast(&format!(
                        "Imported {mesh_name} ({vertex_count}v / {face_count}f) → {res}³ SDF"
                    ));
                }
                Err(e) => self.toast(&format!("OBJ import failed: {e}")),
            }
        }
    }

    // ── Edit ──────────────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_edit(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Edit", |ui| {
            ui.set_min_width(220.0);
            // Undo/Redo/Delete/Duplicate/Select-All/Deselect/Shortcuts…
            self.menu_fill_edit_core(ui, crate::menu_shared::MenuShortcutStyle::Tab);
            ui.separator();
            // Clipboard — same fns the keyboard dispatch calls (input_keyboard.rs
            // ACT_COPY/CUT/PASTE → copy_selection / cut_selection / paste).
            if self.menu_act(ui, "Cut", ACT_CUT).clicked() {
                self.cut_selection();
                ui.close_menu();
            }
            if self.menu_act(ui, "Copy", ACT_COPY).clicked() {
                self.copy_selection();
                ui.close_menu();
            }
            if self.menu_act(ui, "Paste", ACT_PASTE).clicked() {
                self.paste(crate::clipboard::PastePosition::Original);
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Experimental…").clicked() {
                self.ui.show_experimental = true;
                ui.close_menu();
            }
            ui.azra_menu_button("Macro", |ui| {
                let rec_lbl = if self.macro_recording {
                    "Stop Recording"
                } else {
                    "Start Recording"
                };
                if ui.button(rec_lbl).clicked() {
                    self.toggle_macro_recording();
                    ui.close_menu();
                }
                if ui
                    .add_enabled(
                        !self.macro_buffer.is_empty() && !self.macro_recording,
                        egui::Button::new("Play Macro"),
                    )
                    .clicked()
                {
                    self.play_macro();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Save Macro…").clicked() {
                    self.save_macro_dialog();
                    ui.close_menu();
                }
                if ui.button("Load Macro…").clicked() {
                    self.load_macro_dialog();
                    ui.close_menu();
                }
                if ui.button("Macro Manager…").clicked() {
                    self.ui.show_macro_manager = true;
                    ui.close_menu();
                }
            });
            ui.separator();
            if ui.button("Preferences (Developer Tweaks)…").clicked() {
                self.ui.show_tweak_panel = true;
                ui.close_menu();
            }
        });
    }

    // ── Add (all creation) ────────────────────────────────────────────────
    pub(crate) fn ui2_menu_add(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Add", |ui| {
            ui.set_min_width(220.0);
            // All creates route through ops::* + cursor placement (add_spawn_point).
            ui.azra_menu_button("SDF Primitive", |ui| {
                let p = crate::ops::sdf::add_spawn_point(self);
                if ui.button("Sphere").clicked() { crate::ops::sdf::sphere::run(self, p); ui.close_menu(); }
                if ui.button("Box").clicked() { crate::ops::sdf::box_shape::run(self, p); ui.close_menu(); }
                if ui.button("Cylinder").clicked() { crate::ops::sdf::cylinder::run(self, p); ui.close_menu(); }
                if ui.button("Torus").clicked() { crate::ops::sdf::torus::run(self, p); ui.close_menu(); }
                if ui.button("Cone").clicked() { crate::ops::sdf::cone::run(self, p); ui.close_menu(); }
                if ui.button("Capsule").clicked() { crate::ops::sdf::capsule::run(self, p); ui.close_menu(); }
                ui.separator();
                if ui.button("Stair").clicked() { crate::ops::sdf::stair::run(self, p); ui.close_menu(); }
                if ui.button("Spiral Stair").clicked() { crate::ops::sdf::spiral_stair::run(self, p); ui.close_menu(); }
            });
            ui.azra_menu_button("Curve", |ui| {
                let p = crate::ops::sdf::add_spawn_point(self);
                if ui
                    .button("Polyline")
                    .on_hover_text("Extruded curve profile")
                    .clicked()
                {
                    crate::ops::sdf::polyline::run(self, p);
                    ui.close_menu();
                }
                if ui
                    .button("Draw Polyline…")
                    .on_hover_text("Click points on the workplane; Enter to finish")
                    .clicked()
                {
                    crate::ops::sdf::draw_polyline::run(self);
                    ui.close_menu();
                }
                if ui
                    .button("Spine / Pipe")
                    .on_hover_text("3D path tube — free points, Sharp/Bezier/Chamfer, per-point radius")
                    .clicked()
                {
                    crate::ops::sdf::spine::run(self, p);
                    ui.close_menu();
                }
            });
            ui.azra_menu_button("Polygon Mesh", |ui| {
                let p = crate::ops::sdf::add_spawn_point(self);
                if ui.button("Box").clicked() { crate::ops::poly::mesh_create::box_mesh(self, p); ui.close_menu(); }
                if ui.button("Plane").clicked() { crate::ops::poly::mesh_create::plane(self, p); ui.close_menu(); }
                if ui.button("Circle").clicked() { crate::ops::poly::mesh_create::circle(self, p); ui.close_menu(); }
                        if ui.button("Sphere").clicked() { crate::ops::poly::mesh_create::sphere(self, p); ui.close_menu(); }
                        if ui.button("Ico Sphere").clicked() { crate::ops::poly::mesh_create::ico_sphere(self, p); ui.close_menu(); }
                if ui.button("Cylinder").clicked() { crate::ops::poly::mesh_create::cylinder(self, p); ui.close_menu(); }
                if ui.button("Torus").clicked() { crate::ops::poly::mesh_create::torus(self, p); ui.close_menu(); }
                if ui.button("Cone").clicked() { crate::ops::poly::mesh_create::cone(self, p); ui.close_menu(); }
            });
            ui.azra_menu_button("Voxel Sculpt", |ui| {
                if ui.button("Voxel Sphere").clicked() { self.new_voxel_sphere(); ui.close_menu(); }
                if ui.button("Voxel Box").clicked() { self.new_voxel_box(); ui.close_menu(); }
            });
            ui.azra_menu_button("NURBS Surface", |ui| {
                if ui.button("Plane").clicked() { self.create_nurbs_surface("plane"); ui.close_menu(); }
                if ui.button("Cylinder").clicked() { self.create_nurbs_surface("cylinder"); ui.close_menu(); }
                if ui.button("Sphere").clicked() { self.create_nurbs_surface("sphere"); ui.close_menu(); }
                if ui.button("Torus").clicked() { self.create_nurbs_surface("torus"); ui.close_menu(); }
                ui.separator();
                if ui.button("Revolve").clicked() { self.create_nurbs_surface("revolve"); ui.close_menu(); }
                if ui.button("Loft").clicked() { self.create_nurbs_surface("loft"); ui.close_menu(); }
            });
            ui.separator();
            ui.azra_menu_button("Light", |ui| {
                if ui.button("Directional").clicked() { self.add_light(LightType::Directional); ui.close_menu(); }
                if ui.button("Point").clicked() { self.add_light(LightType::Point); ui.close_menu(); }
                if ui.button("Spot").clicked() { self.add_light(LightType::Spot); ui.close_menu(); }
            });
            if ui.button("Camera").clicked() {
                self.add_camera();
                ui.close_menu();
            }
            if ui
                .button("Material")
                .on_hover_text("Create a material node (appears in the outliner Materials section)")
                .clicked()
            {
                self.create_material_interactive();
                ui.close_menu();
            }
            ui.separator();
            // Convert Poly → SDF (creation-adjacent, absorbs Item's Convert group).
            let has_poly = self.lab_has_convertible_poly();
            if ui
                .add_enabled(has_poly, egui::Button::new("Convert Poly → SDF Primitive"))
                .on_hover_text("Convert selected closed poly mesh into an SDF prim")
                .clicked()
            {
                self.convert_selected_poly_to_sdf();
                ui.close_menu();
            }
            if ui
                .add_enabled(has_poly, egui::Button::new("Link Poly → SDF Modifier (live)"))
                .on_hover_text("Link selected poly as a live SDF modifier")
                .clicked()
            {
                self.link_selected_poly_as_sdf();
                ui.close_menu();
            }
        });
    }

    // ── Select ────────────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_select(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Select", |ui| {
            ui.set_min_width(220.0);
            if self.menu_act(ui, "Select All", ACT_SELECT_ALL).clicked() {
                let ids: Vec<SceneItemId> = self
                    .scene
                    .primitives
                    .iter()
                    .filter(|p| p.visible && !p.locked)
                    .map(|p| SceneItemId::Prim(p.prim_uid))
                    .collect();
                self.extend_selection_unified(&ids);
                ui.close_menu();
            }
            if self.menu_act(ui, "Deselect All", ACT_DESELECT_ALL).clicked() {
                self.deselect_all();
                ui.close_menu();
            }
            ui.separator();
            if self.modeling_context == ModelingContext::Polygonal {
                if self.menu_act(ui, "Invert Selection", ACT_POLY_INVERT_SEL).clicked() {
                    crate::ops::poly::invert_selection::run(self);
                    ui.close_menu();
                }
                if self.menu_act(ui, "Grow Selection", ACT_POLY_GROW_SEL).clicked() {
                    crate::ops::poly::grow_selection::run(self);
                    ui.close_menu();
                }
                if self.menu_act(ui, "Shrink Selection", ACT_POLY_SHRINK_SEL).clicked() {
                    crate::ops::poly::shrink_selection::run(self);
                    ui.close_menu();
                }
                ui.separator();
                if self.menu_act(ui, "Select Linked", ACT_POLY_LINKED).clicked() {
                    crate::ops::poly::select_linked::run(self);
                    ui.close_menu();
                }
                if ui
                    .button("Select Linked from Selection")
                    .on_hover_text("Grow selection to the whole connected shell")
                    .clicked()
                {
                    self.poly_select_linked_from_sel();
                    ui.close_menu();
                }
                if matches!(self.poly_work_mode, PolyWorkMode::Edge) {
                    ui.separator();
                    if self.menu_act(ui, "Edge Loop", ACT_POLY_EDGE_LOOP).clicked() {
                        self.poly_select_edge_loop();
                        ui.close_menu();
                    }
                    if self.menu_act(ui, "Edge Ring", ACT_POLY_EDGE_RING).clicked() {
                        self.poly_select_edge_ring();
                        ui.close_menu();
                    }
                }
            } else {
                ui.label(
                    egui::RichText::new("Enter a poly edit mode for more select ops")
                        .small()
                        .weak(),
                );
            }
        });
    }

    // ── Object (was "Item") — viewport RMB context-menu contents ──────────
    pub(crate) fn ui2_menu_object(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Object", |ui| {
            ui.set_min_width(220.0);
            // Basic verbs
            if self.menu_act(ui, "Duplicate", ACT_DUPLICATE).clicked() {
                self.duplicate_selected();
                ui.close_menu();
            }
            if self.menu_act(ui, "Delete", ACT_DELETE).clicked() {
                self.delete_selected();
                ui.close_menu();
            }
            ui.separator();
            // Boolean — selection-wide, same path as the X pie (apply_sdf_bool_pie).
            ui.azra_menu_button("Boolean", |ui| {
                if ui.button("Union").clicked() { self.apply_sdf_bool_pie(0); ui.close_menu(); }
                if ui.button("Subtract").clicked() { self.apply_sdf_bool_pie(2); ui.close_menu(); }
                if ui.button("Intersect").clicked() { self.apply_sdf_bool_pie(4); ui.close_menu(); }
                ui.separator();
                if ui
                    .button("Extrude (footprint)")
                    .on_hover_text("BooleanOp::Extrude — raise/recess by vertical drag")
                    .clicked()
                { self.apply_sdf_bool_pie(1); ui.close_menu(); }
                if ui
                    .button("Seam")
                    .on_hover_text("BooleanOp::Seam — groove/bead by signed-depth drag")
                    .clicked()
                { self.apply_sdf_bool_pie(5); ui.close_menu(); }
            });
            ui.separator();
            // Instance
            if self.menu_act(ui, "Create Instance", ACT_INSTANCE).clicked() {
                crate::ops::instance::create::run(self);
                ui.close_menu();
            }
            if ui
                .button("Instance Into All Bodies")
                .on_hover_text("Linked instance of this prim into every other top-level SDF body")
                .clicked()
            {
                self.instance_into_all_bodies();
                ui.close_menu();
            }
            if ui
                .button("Make Unique / Break Link")
                .on_hover_text("Turn selected instances into independent prims")
                .clicked()
            {
                self.make_selected_instances_unique();
                ui.close_menu();
            }
            ui.separator();
            // Group
            if self.menu_act(ui, "Group", ACT_LAYER).clicked() {
                self.group_selected_bodies_or_prims();
                ui.close_menu();
            }
            // Stack → SDF (merge down)
            if ui
                .button("Stack → SDF (merge down)")
                .on_hover_text("Freeze selected prim + everything before it into one MeshVolume solid")
                .clicked()
            {
                self.stack_to_sdf();
                ui.close_menu();
            }
            ui.separator();
            // Reset transform
            ui.azra_menu_button("Reset Transform", |ui| {
                if ui.button("Reset Position").clicked() { self.reset_position(); ui.close_menu(); }
                if ui.button("Reset Rotation").clicked() { self.reset_rotation(); ui.close_menu(); }
                if ui.button("Reset Scale").clicked() { self.reset_scale(); ui.close_menu(); }
                ui.separator();
                if ui.button("Reset All").clicked() { self.reset_transform(); ui.close_menu(); }
            });
            ui.separator();
            // Isolation
            if ui.button("Isolate Active Body").clicked() {
                if self.scene.isolated_body_nid.is_some() {
                    self.scene.isolated_body_nid = None;
                    self.toast("Isolation off");
                } else {
                    self.scene.isolated_body_nid = Some(self.scene.active_body_nid);
                    self.toast("Isolated active body");
                }
                self.cache.prim_buf_stale = true;
                self.mark_dirty(DirtyFlags::RENDER);
                ui.close_menu();
            }
            if ui.button("Clear Isolation").clicked() {
                self.scene.isolated_body_nid = None;
                self.cache.prim_buf_stale = true;
                self.mark_dirty(DirtyFlags::RENDER);
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button("Save Body to Library…")
                .on_hover_text("Save the owning SDF body to the kitbash library")
                .clicked()
            {
                self.open_library_save_for_selection();
                ui.close_menu();
            }
        });
    }

    // ── Mesh (poly edit ops) ──────────────────────────────────────────────
    pub(crate) fn ui2_menu_mesh(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Mesh", |ui| {
            ui.set_min_width(220.0);
            let is_poly = self.modeling_context == ModelingContext::Polygonal;
            if !is_poly {
                ui.label(
                    egui::RichText::new("Convert an object to editable poly first")
                        .small()
                        .weak(),
                );
                ui.separator();
            }
            // Always list (v1 parity) — same ops::poly::* fns the chords use.
            if self.menu_act(ui, "Extrude", ACT_POLY_EXTRUDE).clicked() {
                crate::ops::poly::extrude::run(self);
                ui.close_menu();
            }
            if ui.button("Extrude Edges").clicked() {
                let mouse = self.viewport.viewport_mouse;
                self.poly_start_edge_extrude_drag(mouse);
                ui.close_menu();
            }
            if self.menu_act(ui, "Inset", ACT_POLY_INSET).clicked() {
                crate::ops::poly::inset::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Bevel", ACT_POLY_BEVEL).clicked() {
                crate::ops::poly::bevel_activate::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Unbevel / Resize Bevel", ACT_POLY_UNBEVEL).clicked() {
                self.poly_activate_unbevel();
                ui.close_menu();
            }
            if self.menu_act(ui, "Face Hinge Rotate", ACT_POLY_FACE_HINGE).clicked() {
                self.poly_activate_face_hinge();
                ui.close_menu();
            }
            if self.menu_act(ui, "Loop Cut", ACT_POLY_LOOP_CUT).clicked() {
                crate::ops::poly::loop_cut::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Edge Slide", ACT_POLY_EDGE_SLIDE).clicked() {
                self.poly_start_edge_slide();
                ui.close_menu();
            }
            if self.menu_act(ui, "Vertex Slide", ACT_POLY_VERT_SLIDE).clicked() {
                self.poly_start_vert_slide();
                ui.close_menu();
            }
            if self.menu_act(ui, "Knife", ACT_POLY_KNIFE).clicked() {
                self.push_cmd(AppCommand::PolyKnife);
                ui.close_menu();
            }
            if self.menu_act(ui, "Bridge", ACT_POLY_BRIDGE).clicked() {
                crate::ops::poly::bridge::run(self);
                ui.close_menu();
            }
            if ui.button("Solidify / Shell").clicked() {
                crate::ops::poly::solidify::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Subdivide", ACT_POLY_SUBDIVIDE).clicked() {
                crate::ops::poly::subdivide::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Dice Faces", ACT_POLY_DICE).clicked() {
                self.poly_activate_dice();
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button("Bake Mesh Maps")
                .on_hover_text("Bake Curvature / AO / Normal / Position into the UV unwrap")
                .clicked()
            {
                self.poly_request_mesh_map_bake();
                ui.close_menu();
            }
            ui.separator();
            if self.menu_act(ui, "Merge Vertices", ACT_POLY_MERGE).clicked() {
                crate::ops::poly::merge_vertices::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Dissolve", ACT_POLY_DISSOLVE).clicked() {
                crate::ops::poly::dissolve::run(self);
                ui.close_menu();
            }
            ui.azra_menu_button("Delete Elements", |ui| {
                if self.menu_act(ui, "Delete Faces", ACT_POLY_DELETE_FACE).clicked() {
                    crate::ops::poly::delete_faces::run(self);
                    ui.close_menu();
                }
                if ui.button("Delete Edges").clicked() {
                    crate::ops::poly::delete_edges::run(self);
                    ui.close_menu();
                }
                if ui.button("Delete Vertices").clicked() {
                    crate::ops::poly::delete_vertices::run(self);
                    ui.close_menu();
                }
            });
            ui.separator();
            if self.menu_act(ui, "Fill", ACT_POLY_FILL).clicked() {
                crate::ops::poly::fill::run(self);
                ui.close_menu();
            }
            if ui.button("Flip Normals").clicked() {
                crate::ops::poly::flip_normals::run(self);
                ui.close_menu();
            }
            if ui.button("Recalculate Normals").clicked() {
                crate::ops::poly::recalculate_normals::run(self);
                ui.close_menu();
            }
            // Blender's `Mesh > Clean Up` submenu. Every entry runs the SAME cleanup install path
            // as the dialog with one extra pass enabled, so each is gated, undoable and reported.
            ui.azra_menu_button("Clean Up", |ui| {
                if ui
                    .button("Mesh Cleanup…")
                    .on_hover_text("All passes, with settings.")
                    .clicked()
                {
                    crate::ops::poly::mesh_cleanup::run(self);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Merge by Distance").clicked() {
                    crate::ops::poly::cleanup_passes::merge_by_distance(self);
                    ui.close_menu();
                }
                if ui.button("Degenerate Dissolve").clicked() {
                    crate::ops::poly::cleanup_passes::degenerate_dissolve(self);
                    ui.close_menu();
                }
                if ui.button("Delete Loose").clicked() {
                    crate::ops::poly::cleanup_passes::delete_loose(self);
                    ui.close_menu();
                }
                if ui.button("Fill Holes").clicked() {
                    crate::ops::poly::cleanup_passes::fill_holes(self);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Make Planar Faces").clicked() {
                    crate::ops::poly::cleanup_passes::make_planar_faces(self);
                    ui.close_menu();
                }
                if ui.button("Split Non-Planar Faces").clicked() {
                    crate::ops::poly::cleanup_passes::split_non_planar_faces(self);
                    ui.close_menu();
                }
                if ui.button("Split Concave Faces").clicked() {
                    crate::ops::poly::cleanup_passes::split_concave_faces(self);
                    ui.close_menu();
                }
            });
            ui.separator();
            if self.menu_act(ui, "Grow Selection", ACT_POLY_GROW_SEL).clicked() {
                crate::ops::poly::grow_selection::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Shrink Selection", ACT_POLY_SHRINK_SEL).clicked() {
                crate::ops::poly::shrink_selection::run(self);
                ui.close_menu();
            }
            ui.separator();
            if self.menu_act(ui, "Hide Selected", ACT_POLY_HIDE).clicked() {
                crate::ops::poly::hide_selected::run(self);
                ui.close_menu();
            }
            if ui.button("Hide Unselected").clicked() {
                crate::ops::poly::hide_unselected::run(self);
                ui.close_menu();
            }
            if self.menu_act(ui, "Unhide All", ACT_UNHIDE_ALL).clicked() {
                crate::ops::poly::unhide_all::run(self);
                ui.close_menu();
            }
        });
    }

    // ── Materials ─────────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_materials(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Materials", |ui| {
            ui.set_min_width(220.0);
            if ui.button("Add Material").clicked() {
                self.create_material_interactive();
                ui.close_menu();
            }
            // Assign to Selection — the dead Ctrl+M popup is v1-gated in v2, so we
            // surface the assign directly (same assign_material_to_selection path).
            let has_target = self.has_material_assign_target();
            let mats: Vec<(u32, String)> = self
                .scene
                .scene_graph
                .materials()
                .map(|(_, mid, name, _)| (mid.0, name.to_string()))
                .collect();
            ui.add_enabled_ui(has_target && !mats.is_empty(), |ui| {
                ui.azra_menu_button("Assign to Selection", |ui| {
                    for (id, name) in &mats {
                        let label = if name.is_empty() {
                            format!("Material {id}")
                        } else {
                            name.clone()
                        };
                        if ui.button(label).clicked() {
                            if self.assign_material_to_selection(*id) {
                                self.toast("Material assigned");
                            } else {
                                self.toast("Could not assign material to this selection");
                            }
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.button("Remove (embedded)").clicked() {
                        self.assign_material_to_selection(0);
                        ui.close_menu();
                    }
                });
            });
            if !has_target {
                ui.label(
                    egui::RichText::new("Select a prim / body / poly to assign")
                        .small()
                        .weak(),
                );
            }
            ui.separator();
            if ui.button("Shader / Node Editor…").clicked() {
                self.reveal_shader_node_editor();
                ui.close_menu();
            }
            if ui
                .button("Material Library…")
                .on_hover_text("Open the library band (Materials category)")
                .clicked()
            {
                // v2 library band. Category is chosen via the band's funnel;
                // no single settable field, so we only reveal the band here.
                self.ui2.library.open = true;
                ui.close_menu();
            }
        });
    }

    // ── Render ────────────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_render(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Render", |ui| {
            ui.set_min_width(200.0);
            ui.label(egui::RichText::new("Shading").strong().small());
            for (label, mode) in [
                ("Solid", ShadingMode::SolidHdr),
                ("Material (PBR)", ShadingMode::Pbr),
                ("DC Wire", ShadingMode::Wireframe),
                ("MatCap", ShadingMode::MatCap),
                ("Raytrace", ShadingMode::Raytrace),
                ("Path Trace", ShadingMode::PathTrace),
            ] {
                let on = self.render.shading_mode == mode
                    || (mode == ShadingMode::SolidHdr
                        && matches!(
                            self.render.shading_mode,
                            ShadingMode::SolidHdr | ShadingMode::Lit
                        ));
                if ui.selectable_label(on, label).clicked() {
                    self.render.shading_mode = mode;
                    self.mark_dirty(DirtyFlags::RENDER);
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui
                .checkbox(
                    &mut self.render.wire_overlay_enabled,
                    format!(
                        "Wire on Shaded  ({})",
                        self.keybindings.label(ACT_WIRE_OVERLAY)
                    ),
                )
.default_menu("render.wire_overlay_enabled", serde_json::json!(self.render.wire_overlay_enabled))
                .changed()
            {
                self.mark_dirty(DirtyFlags::RENDER);
            }
            ui.separator();
            if ui.button("Path Tracer Settings…").clicked() {
                self.ui.show_pt_settings = true;
                ui.close_menu();
            }
            if ui
                .button("AI Beauty (Path-Tracer Overlay)…")
                .clicked()
            {
                self.ai.show_beauty_window = true;
                ui.close_menu();
            }
        });
    }

    // ── View ──────────────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_view(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("View", |ui| {
            ui.set_min_width(220.0);
            ui.azra_menu_button("Camera", |ui| {
                if self.menu_act(ui, "Front", ACT_VIEW_FRONT).clicked() {
                    self.viewport.camera.orbit = Quat::from_euler(EulerRot::YXZ, 0.0, 0.0, 0.0);
                    self.mark_dirty(DirtyFlags::RENDER);
                    ui.close_menu();
                }
                if self.menu_act(ui, "Right", ACT_VIEW_RIGHT).clicked() {
                    self.viewport.camera.orbit =
                        Quat::from_euler(EulerRot::YXZ, -std::f32::consts::FRAC_PI_2, 0.0, 0.0);
                    self.mark_dirty(DirtyFlags::RENDER);
                    ui.close_menu();
                }
                if self.menu_act(ui, "Top", ACT_VIEW_TOP).clicked() {
                    self.viewport.camera.orbit =
                        Quat::from_euler(EulerRot::YXZ, 0.0, -std::f32::consts::FRAC_PI_2, 0.0);
                    self.mark_dirty(DirtyFlags::RENDER);
                    ui.close_menu();
                }
                ui.separator();
                if self.menu_act(ui, "Focus Selected", ACT_FOCUS).clicked() {
                    crate::ops::edit::frame_selected::run(self);
                    ui.close_menu();
                }
                if ui.button("Reset View").clicked() {
                    self.viewport.camera.orbit = Quat::from_euler(EulerRot::YXZ, -0.5, 0.3, 0.0);
                    self.viewport.camera.distance = self.tweak.default_cam_dist;
                    self.viewport.camera.target = Vec3::ZERO;
                    self.mark_dirty(DirtyFlags::RENDER);
                    ui.close_menu();
                }
            });
            ui.separator();
            // v2 dock panel toggles — checked state derives from live Ui2State,
            // toggling goes through the ui2 dock system (v1 layout.show_* untouched).
            let tools_kinds = [
                PanelKind::ToolsSdf,
                PanelKind::ToolsPoly,
                PanelKind::ToolsSculpt,
                PanelKind::ToolsNurbs,
            ];
            let mut on = self.ui2_panel_visible(&tools_kinds);
            if ui.checkbox(&mut on, "Tools Panel").changed() {
                self.ui2_set_panel_visible(&tools_kinds, DockSide::Left, on);
            }
            let mut on = self.ui2_panel_visible(&[PanelKind::Outliner]);
            if ui.checkbox(&mut on, "Outliner").changed() {
                self.ui2_set_panel_visible(&[PanelKind::Outliner], DockSide::Right, on);
            }
            let mut on = self.ui2_panel_visible(&[PanelKind::Properties]);
            if ui.checkbox(&mut on, "Properties Panel").changed() {
                self.ui2_set_panel_visible(&[PanelKind::Properties], DockSide::Right, on);
            }
            let mut on = self.ui2_panel_visible(&[PanelKind::History]);
            if ui.checkbox(&mut on, "History").changed() {
                self.ui2_set_panel_visible(&[PanelKind::History], DockSide::Right, on);
            }
            // Geometry Channels: not default-docked; user opens when needed.
            let mut on = self.ui2_panel_visible(&[PanelKind::Channels]);
            if ui.checkbox(&mut on, "Geometry Channels").changed() {
                self.ui2_set_panel_visible(&[PanelKind::Channels], DockSide::Right, on);
            }
            if ui.checkbox(&mut self.ui2.library.open, "Library").changed() {}
            ui.separator();
            if ui.button("Grid Settings…").clicked() {
                self.viewport.show_grid_settings = true;
                ui.close_menu();
            }
            if ui.button("Work Plane Settings…").clicked() {
                self.viewport.wp_show_settings = true;
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Layout Settings…").clicked() {
                self.ui.show_layout_settings = true;
                ui.close_menu();
            }
            if ui.button("Panel List…").clicked() {
                self.ui.show_panel_list = true;
                ui.close_menu();
            }
            if ui.button("Reset Default Layout").clicked() {
                self.panel_layout = crate::panel::UiLayout::default();
                self.panel_layout.save_to_file();
                self.ui2_reset_layout();
                self.toast("Layout reset to default");
                ui.close_menu();
            }
        });
    }

    // ── Window / Dev ──────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_window(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Window", |ui| {
            ui.set_min_width(220.0);
            if ui.checkbox(&mut self.ui.show_tweak_panel, "Developer Tweaks").changed() {}
            if ui
                .checkbox(&mut self.ui.show_body_library, "Body Library")
                .changed()
            {
                if self.ui.show_body_library {
                    self.ui.body_library_needs_scan = true;
                }
            }
            ui.separator();
            if ui.button("Shader / Node Editor…").clicked() {
                self.reveal_shader_node_editor();
                ui.close_menu();
            }
            if ui.button("Texture Generator…").clicked() {
                self.ui.show_texgen_window = true;
                ui.close_menu();
            }
            if ui.button("AI Texture (Stable Diffusion)…").clicked() {
                self.ai.show_ai_tex_window = true;
                ui.close_menu();
            }
            if ui.button("AI Agent Settings…").clicked() {
                self.ai.show_agent_settings = true;
                ui.close_menu();
            }
            ui.separator();
            if self.menu_act(ui, "Shortcuts…", ACT_SHORTCUT_EDITOR).clicked() {
                self.ui.show_keybind_settings = true;
                self.ui.show_shortcut_editor = true;
                ui.close_menu();
            }
            if ui.checkbox(&mut self.ui.show_perf_hud, "Performance HUD").changed() {}
            if ui.checkbox(&mut self.ui.show_stress_test, "Stress Test").changed() {}
        });
    }

    // ── Help ──────────────────────────────────────────────────────────────
    pub(crate) fn ui2_menu_help(&mut self, ui: &mut egui::Ui) {
        ui.azra_menu_button("Help", |ui| {
            ui.set_min_width(200.0);
            if ui.button("About").clicked() {
                self.ui.show_about = true;
                ui.close_menu();
            }
            ui.separator();
            ui.label(egui::RichText::new("Developer").strong().small());
            if ui
                .button("Code Review (Claude)…")
                .clicked()
            {
                self.ai.show_code_review = true;
                ui.close_menu();
            }
            if ui
                .button("Rebuild & Restart")
                .on_hover_text("cargo build -p app, then relaunch")
                .clicked()
            {
                self.dev_rebuild_and_restart();
                ui.close_menu();
            }
            if ui.button("Restart (no build)").clicked() {
                self.dev_restart_no_build();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Cache Test Scene").clicked() {
                self.create_cache_test_scene();
                ui.close_menu();
            }
            if ui.button("Advanced Cache Test Scene").clicked() {
                self.create_advanced_cache_test_scene();
                ui.close_menu();
            }
        });
    }
}
