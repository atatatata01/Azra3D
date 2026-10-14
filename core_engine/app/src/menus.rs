// TODO: Split further — over 300 line limit.
//! Menu bar, context menu, Q menu, add popup — moved from ui_update.rs.

use crate::ui_menus::AzraMenus;
use eframe::egui;
use glam::{Quat, EulerRot, Vec3};
use core_math::primitives::{SdfShape, BooleanOp, BoxBevel};
use crate::*;

// ── Context-menu search filter ───────────────────────────────────────────────
/// `q` is already lowercased (or empty). Empty query → show everything.
fn ctx_q_match(q: &str, label: &str) -> bool {
    q.is_empty() || label.to_lowercase().contains(q)
}

/// Button that hides itself when the live search query does not match its label.
/// Returns true when the user clicked a visible matching item.
fn ctx_btn(ui: &mut egui::Ui, q: &str, label: &str) -> bool {
    if !ctx_q_match(q, label) {
        return false;
    }
    ui.button(label).clicked()
}

/// Like [`ctx_btn`] but chains `on_hover_text` before the click check.
fn ctx_btn_tip(ui: &mut egui::Ui, q: &str, label: &str, tip: &str) -> bool {
    if !ctx_q_match(q, label) {
        return false;
    }
    ui.button(label).on_hover_text(tip).clicked()
}

impl AzraApp {
    pub(crate) fn draw_menu_bar(&mut self, ctx: &egui::Context) -> (bool, bool) {
        let mut do_import_obj = false;
        let mut do_import_gltf = false;
        egui::TopBottomPanel::top("menu_bar").show(ctx,|ui|{egui::menu::bar(ui,|ui|{
            ui.azra_menu_button("File",|ui|{
                self.menu_fill_file_scene(ui, crate::menu_shared::MenuShortcutStyle::Paren);
                ui.separator();
                self.menu_fill_file_defaults(ui);
                ui.separator();
                if ui.button("Export JSON...").clicked(){
                    let dialog = rfd::FileDialog::new().set_title("Export JSON").add_filter("JSON", &["json"]);
                    if let Some(path) = dialog.save_file() {
                        self.voxel_sync_node_mesh(); // persist the live sculpt, not the stale initial mesh
                        let result = crate::project::ProjectFile::from_app(self)
                            .and_then(|project| crate::project::save_json(&path, &project));
                        match result {
                            Ok(()) => self.toast("Exported JSON"),
                            Err(e) => self.toast(&format!("Export failed: {}", e)),
                        }
                    }
                    ui.close_menu();
                }
                if ui.button("Import JSON...").clicked(){
                    let dialog = rfd::FileDialog::new().set_title("Import JSON").add_filter("JSON", &["json"]);
                    if let Some(path) = dialog.pick_file() {
                        self.load_scene_from_path(&path);
                    }
                    ui.close_menu();
                }
                ui.separator();
                // Recent files
                if !self.project.recent_files.is_empty() {
                    ui.azra_menu_button("Recent Files", |ui| {
                        let recent = self.project.recent_files.clone();
                        for path in &recent {
                            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                            if ui.button(name).on_hover_text(path.display().to_string()).clicked() {
                                self.load_scene_from_path(path);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                }
                if ui.button("\u{1F4C1} Import OBJ as SDF\u{2026}").clicked(){ do_import_obj = true; ui.close_menu(); }
                if ui.button("\u{1F4E6} Import glTF\u{2026}").clicked(){ do_import_gltf = true; ui.close_menu(); }
                if ui.button("\u{1F53A} Export Mesh (OBJ)...").clicked(){ self.ui.show_export_window = true; ui.close_menu(); }
                if ui.button("\u{1F4E6} Export glTF (.glb)...").clicked(){ self.ui.show_gltf_export = true; ui.close_menu(); }
                ui.separator();
                if ui.button("\u{1F3A8} Texture Generator...").clicked(){ self.ui.show_texgen_window = true; ui.close_menu(); }
                if ui.button("\u{1F9E0} AI Texture (Stable Diffusion)...").clicked(){ self.ai.show_ai_tex_window = true; ui.close_menu(); }
                if ui.button("\u{2728} AI Beauty (Path-Tracer Overlay)...").clicked(){ self.ai.show_beauty_window = true; ui.close_menu(); }
                ui.separator();
                if ui.button("\u{2699} Agent Settings...").clicked(){self.ai.show_agent_settings=true;ui.close_menu();}
                if ui.button("\u{1F50D} Code Review (Claude)...").clicked(){ self.ai.show_code_review = true; ui.close_menu(); }
                ui.separator();
                if ui.button("Quit").clicked(){
                    if self.project.scene_modified {
                        self.ui.pending_action_after_save = Some(PendingAction::Quit);
                        self.ui.show_unsaved_dialog = true;
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    ui.close_menu();
                }
            });
            ui.azra_menu_button("Edit",|ui|{
                self.menu_fill_edit_core(ui, crate::menu_shared::MenuShortcutStyle::Paren);
                if ui.button("\u{1F9EA} Experimental...").clicked(){self.ui.show_experimental=true;ui.close_menu();}
                ui.separator();
                ui.azra_menu_button("\u{23FA} Macro", |ui| {
                    let rec_lbl = if self.macro_recording { "\u{23F9} Stop Recording" } else { "\u{23FA} Start Recording" };
                    if ui.button(rec_lbl).clicked() { self.toggle_macro_recording(); ui.close_menu(); }
                    if ui.add_enabled(!self.macro_buffer.is_empty() && !self.macro_recording,
                        egui::Button::new("\u{25B6} Play Macro")).clicked() { self.play_macro(); ui.close_menu(); }
                    ui.separator();
                    if ui.button("\u{1F4BE} Save Macro...").clicked() { self.save_macro_dialog(); ui.close_menu(); }
                    if ui.button("\u{1F4C2} Load Macro...").clicked() { self.load_macro_dialog(); ui.close_menu(); }
                    ui.separator();
                    if ui.button("\u{1F4CB} Macro Manager...").clicked() { self.ui.show_macro_manager = true; ui.close_menu(); }
                });
            });
            ui.azra_menu_button("Add",|ui|{
                // All creates go through ops::* + cursor placement (INTEGRATION_RULES §0 / G6).
                let p = crate::ops::sdf::add_spawn_point(self);
                if ui.button("\u{26AA} Sphere").clicked(){crate::ops::sdf::sphere::run(self,p);ui.close_menu();}
                if ui.button("\u{2B1C} Box").clicked(){crate::ops::sdf::box_shape::run(self,p);ui.close_menu();}
                if ui.button("\u{1F535} Cylinder").clicked(){crate::ops::sdf::cylinder::run(self,p);ui.close_menu();}
                if ui.button("\u{1F369} Torus").clicked(){crate::ops::sdf::torus::run(self,p);ui.close_menu();}
                if ui.button("\u{1F53A} Cone").clicked(){crate::ops::sdf::cone::run(self,p);ui.close_menu();}
                if ui.button("\u{1F48A} Capsule").clicked(){crate::ops::sdf::capsule::run(self,p);ui.close_menu();}
                ui.separator();
                if ui.button("\u{1FA9C} Stair").clicked(){crate::ops::sdf::stair::run(self,p);ui.close_menu();}
                if ui.button("\u{1F300} Spiral Stair").clicked(){crate::ops::sdf::spiral_stair::run(self,p);ui.close_menu();}
                ui.separator();
                if ui.button("\u{2312} Polyline").on_hover_text("Extruded curve profile (square). Edit corners / shell / rim in the shape panel.").clicked(){crate::ops::sdf::polyline::run(self,p);ui.close_menu();}
                if ui.button("\u{270E} Draw Polyline\u{2026}").on_hover_text("Click points on the workplane; Enter to close into a new extruded curve prim, Esc/RMB to cancel.").clicked(){crate::ops::sdf::draw_polyline::run(self);ui.close_menu();}
                if ui.button("\u{223F} Spine / Pipe").on_hover_text("3D path tube — free points, Sharp/Bezier/Chamfer, per-point radius. Space finishes edit.").clicked(){crate::ops::sdf::spine::run(self,p);ui.close_menu();}
                ui.separator();
                // Poly → SDF: convert the selected/first closed polygon mesh into an SDF primitive.
                let has_poly = self.lab_has_convertible_poly();
                if ui.add_enabled(has_poly, egui::Button::new("\u{25C8} Poly \u{2192} SDF Primitive"))
                    .on_hover_text("Convert the selected closed polygon mesh into an SDF primitive\n(union by default — switch to subtract/intersect + blend in the shape panel). The poly is consumed.")
                    .clicked() { self.convert_selected_poly_to_sdf(); ui.close_menu(); }
                if ui.add_enabled(has_poly, egui::Button::new("\u{1F517} Poly \u{2192} SDF Modifier (live)"))
                    .on_hover_text("LINK the selected closed poly as a LIVE SDF modifier: the imprint follows\nthe poly's transform and re-converts when its geometry is edited.\nThe poly is hidden (not deleted).")
                    .clicked() { self.link_selected_poly_as_sdf(); ui.close_menu(); }
                ui.separator();
                ui.azra_menu_button("\u{1F4A1} Light",|ui|{
                    if ui.button("\u{2600} Directional").clicked(){self.add_light(LightType::Directional);ui.close_menu();}
                    if ui.button("\u{1F4A1} Point").clicked(){self.add_light(LightType::Point);ui.close_menu();}
                    if ui.button("\u{1F526} Spot").clicked(){self.add_light(LightType::Spot);ui.close_menu();}
                });
                ui.separator();
                // Materials are explicitly-added items (owner refinement 2026-07-17) — same action
                // as the outliner + menu; the outliner Materials section appears once one exists.
                if ui.button("\u{1F3A8} Material").on_hover_text("Create a material — pick shader type (Standard PBR / Glass / SSS / VFX…), then name it. Appears in the outliner Materials section.").clicked(){self.create_material_interactive();ui.close_menu();}
            });
            ui.azra_menu_button("View",|ui|{
                if ui.button(format!("Front ({})", self.keybindings.label(ACT_VIEW_FRONT))).clicked(){self.viewport.camera.orbit=Quat::from_euler(EulerRot::YXZ,0.0,0.0,0.0);self.mark_dirty(DirtyFlags::RENDER);ui.close_menu();}
                if ui.button(format!("Right ({})", self.keybindings.label(ACT_VIEW_RIGHT))).clicked(){self.viewport.camera.orbit=Quat::from_euler(EulerRot::YXZ,-std::f32::consts::FRAC_PI_2,0.0,0.0);self.mark_dirty(DirtyFlags::RENDER);ui.close_menu();}
                if ui.button(format!("Top ({})", self.keybindings.label(ACT_VIEW_TOP))).clicked(){self.viewport.camera.orbit=Quat::from_euler(EulerRot::YXZ,0.0,-std::f32::consts::FRAC_PI_2,0.0);self.mark_dirty(DirtyFlags::RENDER);ui.close_menu();}
                ui.separator();
                if ui.button(format!("Focus ({})", self.keybindings.label(ACT_FOCUS))).clicked(){crate::ops::edit::frame_selected::run(self);ui.close_menu();}
                if ui.button("Reset View").clicked(){self.viewport.camera.orbit=Quat::from_euler(EulerRot::YXZ,-0.5,0.3,0.0);self.viewport.camera.distance=self.tweak.default_cam_dist;self.viewport.camera.target=Vec3::ZERO;self.mark_dirty(DirtyFlags::RENDER);ui.close_menu();}
                ui.separator();
                {
                    let mut uv = self.ui.show_uv_layout;
                    if ui
                        .checkbox(&mut uv, format!("UV Layout ({})", self.keybindings.label(ACT_VIEW_UV_LAYOUT)))
                        .on_hover_text("2D UV island view over the viewport (like Blender's UV Editor)")
                        .changed()
                    {
                        self.ui.show_uv_layout = uv;
                        self.mark_dirty(DirtyFlags::RENDER);
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.checkbox(&mut self.layout.show_tool_panel, "Tool Panel").changed() { self.layout.save(); ui.close_menu(); }
                if ui.checkbox(&mut self.layout.show_properties_panel, "Properties Panel").changed() { self.layout.save(); ui.close_menu(); }
                if ui.checkbox(&mut self.layout.show_viewport_header, "Viewport Header").changed() { self.layout.save(); ui.close_menu(); }
                ui.separator();
                if ui.button("Layout Settings...").clicked() { self.ui.show_layout_settings = true; ui.close_menu(); }
                if ui.checkbox(&mut self.ui.show_body_library, "Body Library (Kitbash)").changed() {
                    if self.ui.show_body_library {
                        self.ui.body_library_needs_scan = true;
                    }
                    ui.close_menu();
                }
                if ui.checkbox(&mut self.ui.show_tweak_panel, "Developer Tweaks").changed() { ui.close_menu(); }
                if ui.checkbox(&mut self.ui.show_stress_test, "Stress Test").changed() { ui.close_menu(); }
                ui.separator();
                {
                    let mut edit = self.panel_layout.ui_edit_mode;
                    if ui.checkbox(&mut edit, "Edit UI Layout").changed() {
                        self.panel_layout.ui_edit_mode = edit;
                        self.panel_layout.save_to_file();
                        ui.close_menu();
                    }
                }
                if ui.button("Panel List...").clicked() { self.ui.show_panel_list = true; ui.close_menu(); }
                if ui.button("Reset to Default Layout").clicked() {
                    self.panel_layout = crate::panel::UiLayout::default();
                    self.panel_layout.save_to_file();
                    self.toast("Layout reset to default");
                    ui.close_menu();
                }
            });
            ui.azra_menu_button("Render",|ui|{
                if ui.button("Path Tracer Settings...").clicked() {
                    self.ui.show_pt_settings = true;
                    ui.close_menu();
                }
            });
            ui.azra_menu_button("Dev",|ui|{
                if ui.button("\u{1F504} Rebuild & Restart").on_hover_text("cargo build -p app, then relaunch").clicked(){
                    self.dev_rebuild_and_restart();
                    ui.close_menu();
                }
                if ui.button("\u{1F504} Restart (no build)").on_hover_text("Relaunch current exe").clicked(){
                    self.dev_restart_no_build();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("\u{1F9EA} Cache Test Scene").on_hover_text("50 prims, 3 bodies, Hybrid + Per-Body Cache").clicked(){
                    self.create_cache_test_scene();
                    ui.close_menu();
                }
                if ui.button("\u{1F9EA} Advanced Cache Test Scene").on_hover_text("60 prims, 5 bodies, nested booleans + blend + modifiers + scale extremes").clicked(){
                    self.create_advanced_cache_test_scene();
                    ui.close_menu();
                }
            });
            ui.azra_menu_button("Help",|ui|{if ui.button("About").clicked(){self.ui.show_about=true;ui.close_menu();}});

            // GPU backend indicator (right-aligned)
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let backend_icon = match self.gpu_info.backend.as_str() {
                    "Vulkan" => "\u{1F53B} Vulkan",
                    "Dx12" => "\u{1F537} DX12",
                    "Metal" => "\u{1F34E} Metal",
                    "Gl" => "\u{1F7E2} OpenGL",
                    _ => "\u{2699} GPU",
                };
                ui.weak(format!("{} \u{2014} {}", backend_icon, self.gpu_info.adapter_name));
            });

            // Agent / Claude thinking indicator
            if self.ai.agent.is_thinking() {
                ui.separator();
                let secs = self.ai.agent.thinking_seconds();
                let dots = ".".repeat(((secs * 3.0) as usize % 4) + 1);
                ui.colored_label(egui::Color32::from_rgb(130, 200, 255),
                    format!("\u{2728} Thinking{} {:.1}s", dots, secs));
            } else if self.ai.claude.is_thinking {
                ui.separator();
                let secs = self.ai.claude.thinking_seconds();
                let anim = ["\u{280B}","\u{2819}","\u{2839}","\u{2838}","\u{283C}","\u{2834}","\u{2826}","\u{2827}","\u{2807}","\u{280F}"];
                ui.colored_label(egui::Color32::from_rgb(180, 140, 255),
                    format!("{} Claude {:.1}s", anim[(secs * 8.0) as usize % 10], secs));
            }

            // Status indicators
            if let Some(ts)=self.tool.current.as_transform() { ui.separator();
                let m=match ts.kind{TransformKind::Grab=>"GRAB",TransformKind::Rotate=>"ROTATE",TransformKind::Scale=>"SCALE",TransformKind::Elongate=>"ELONGATE"};
                let a: &str=match ts.axis{
                    AxisConstraint::None=>"", AxisConstraint::X=>" [X]", AxisConstraint::Y=>" [Y]", AxisConstraint::Z=>" [Z]",
                    AxisConstraint::PlaneX=>" [YZ plane]", AxisConstraint::PlaneY=>" [XZ plane]", AxisConstraint::PlaneZ=>" [XY plane]",
                };
                let n = if ts.num_buf.is_empty() { String::new() } else { format!(" : {}_", ts.num_buf) };
                ui.colored_label(egui::Color32::from_rgb(255,180,50),format!("\u{26A1} {}{}{}",m,a,n));
            }
            if let Some(gi)=self.tool.current.as_gizmo() { ui.separator();
                let m=match gi.mode{GizmoMode::Translate=>"MOVE",GizmoMode::Rotate=>"ROTATE",GizmoMode::Scale=>"SCALE",GizmoMode::Elongate=>"ELONGATE"};
                let s=match gi.space{TransformSpace::Global=>"G",TransformSpace::Local=>"L"};
                ui.colored_label(egui::Color32::from_rgb(255,180,50),format!("\u{26A1} Gizmo {} [{}] ({})",m,gi.component.label(),s));
            }
            if let Some(ct) = self.create_tool_status_text() { ui.separator();
                ui.colored_label(egui::Color32::from_rgb(100, 200, 255), format!("\u{2B1C} {}", ct));
            }
            if let Some(st) = self.symmetry_status_text() { ui.separator();
                ui.colored_label(egui::Color32::from_rgb(255, 153, 0), format!("\u{2194} {}", st));
            }
            {let sel_count=self.selected_prim_indices_unified().len();if sel_count>1{ui.separator();ui.colored_label(egui::Color32::from_rgb(255,180,50),format!("\u{1F536} {} selected",sel_count));}}
        });});
        (do_import_obj, do_import_gltf)
    }

    pub(crate) fn apply_tab_pie_mode(&mut self, idx: usize) {
        match self.ui.pie_mode {
            crate::pie_menu::PieMode::Sdf => {
                // SDF layout: 0=Object(top), 1=Prim(right), 2=spacer, 3=Cycle(left)
                match idx {
                    0 => {
                        self.ui.sdf_cycle_mode = false;
                        self.switch_work_mode(WorkMode::Object);
                    }
                    1 => {
                        self.ui.sdf_cycle_mode = false;
                        self.switch_work_mode(WorkMode::Vertex);
                    }
                    3 => {
                        // Activate cycle mode
                        self.ui.sdf_cycle_mode = true;
                        self.toast("Cycle mode \u{2014} scroll to step through prims");
                        return;
                    }
                    _ => return, // hidden spacer — no-op
                }
            }
            crate::pie_menu::PieMode::Poly => {
                self.ui.sdf_cycle_mode = false;
                // Poly layout (8 sectors, Blender): 0=Object(top) 2=Face(right)
                // 3=Group(bottom-right when active) 4=Edge(bottom) 5=Sculpt 6=Vertex; 1/7=spacers
                if idx == 5 { self.enter_sculpt_mode(); return; }
                if self.modeling_context == ModelingContext::Polygonal {
                    let pm = match idx {
                        0 => PolyWorkMode::Object,
                        2 => PolyWorkMode::Face,
                        3 => {
                            if !self.poly_pie_shows_group() {
                                return; // spacer when no polygroups
                            }
                            PolyWorkMode::Group
                        }
                        4 => PolyWorkMode::Edge,
                        6 => PolyWorkMode::Vertex,
                        _ => return, // hidden spacer — no-op
                    };
                    self.poly_switch_work_mode(pm);
                } else {
                    let nm = match idx {
                        0 => WorkMode::Object,
                        2 => WorkMode::Face,
                        4 => WorkMode::Edge,
                        6 => WorkMode::Vertex,
                        _ => return, // hidden spacer — no-op
                    };
                    if matches!(self.tool.work_mode, WorkMode::Vertex|WorkMode::Edge|WorkMode::Face) {
                        self.ui.last_edit_mode = Some(self.tool.work_mode);
                    }
                    self.switch_work_mode(nm);
                }
            }
            crate::pie_menu::PieMode::Nurbs => {
                self.ui.sdf_cycle_mode = false;
                // Same indices as poly Tab pie: 0 Object, 2 Face, 4 Edge, 6 Vertex
                let nm = match idx {
                    0 => NurbsWorkMode::Object,
                    2 => NurbsWorkMode::Face,
                    4 => NurbsWorkMode::Edge,
                    6 => NurbsWorkMode::Vertex,
                    _ => return, // spacers
                };
                self.nurbs_switch_work_mode(nm);
            }
        }
    }

    /// No-op — pie no longer has outer tool items.
    pub(crate) fn apply_tab_pie_tool(&mut self, _inner: usize, _outer: usize) {}

    /// F-key utility pie result handler.
    pub(crate) fn apply_f_pie(&mut self, idx: usize) {
        match idx {
            3 => {
                // Focus Camera (left)
                self.frame_selected();
            }
            1 => {
                // Isolate toggle (right)
                if self.scene.isolated_body_nid.is_some() {
                    self.scene.isolated_body_nid = None;
                    self.toast("Isolation off");
                } else {
                    self.scene.isolated_body_nid = Some(self.scene.active_body_nid);
                    let name = self.scene.scene_graph.get(self.scene.active_body_nid)
                        .map(|n| n.payload.display_name().to_string())
                        .unwrap_or_else(|| "Body".into());
                    self.toast(&format!("Isolated: {}", name));
                }
                // Isolation is a view filter, not a data change — no bake needed.
                self.cache.prim_buf_stale = true;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            0 => {
                // Context display toggle (top): ghost ↔ normal
                self.isolate_context_ghost = !self.isolate_context_ghost;
                let mode = if self.isolate_context_ghost { "Ghost" } else { "Normal" };
                self.toast(&format!("Context: {}", mode));
                self.mark_dirty(DirtyFlags::RENDER);
            }
            _ => {}
        }
    }

    // draw_merge_menu (old linear Alt+M popup) removed — replaced by merge_pie (pie_menu::merge_pie_def).

    pub(crate) fn draw_q_menu(&mut self, ctx: &egui::Context) {
        if !self.ui.q_menu_open { return; }
        let pos = self.ui.q_menu_pos;

        egui::Area::new(egui::Id::new("q_operations_menu"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(180.0);

                    // Header
                    let header = if self.modeling_context == ModelingContext::Polygonal {
                        match self.poly_work_mode {
                            PolyWorkMode::Vertex => "\u{25CF} Vertex Ops",
                            PolyWorkMode::Edge => "\u{2500} Edge Ops",
                            PolyWorkMode::Face => "\u{25A0} Face Ops",
                            PolyWorkMode::Group => "\u{25A6} Group Ops",
                            PolyWorkMode::Object => "\u{25C9} Object Ops",
                        }
                    } else {
                        "\u{25C9} Operations"
                    };
                    ui.label(egui::RichText::new(header).strong().size(12.0));
                    ui.separator();

                    if self.modeling_context == ModelingContext::Polygonal {
                        match self.poly_work_mode {
                            PolyWorkMode::Vertex => {
                                if self.q_menu_item(ui, "Bevel Vertices", "Ctrl+B") { self.poly_activate_bevel(); }
                                if self.q_menu_item(ui, "Merge Vertices", "M") { self.poly_merge_vertices(); }
                                if self.q_menu_item(ui, "Connect Vertex Path", "J") { self.poly_connect_vertices(); }
                                if self.q_menu_item(ui, "To Sphere", "") { self.poly_to_sphere(); }
                                if self.q_menu_item(ui, "Select Linked", "L") { self.poly_select_linked_hover(false); }
                                if self.q_menu_item(ui, "Select Linked (sel)", "Ctrl+L") { self.poly_select_linked_from_sel(); }
                                ui.separator();
                                if self.q_menu_item(ui, "Grow Selection", "G") { self.poly_grow_selection(); }
                                if self.q_menu_item(ui, "Shrink Selection", "Shift+G") { self.poly_shrink_selection(); }
                                if self.q_menu_item(ui, "Invert Selection", "Ctrl+I") { crate::ops::poly::invert_selection::run(self); }
                                ui.separator();
                                if self.q_menu_item(ui, "Hide Selected", "H") { self.poly_hide_selected(); }
                                if self.q_menu_item(ui, "Hide Unselected", "Shift+H") { self.poly_hide_unselected(); }
                                if self.q_menu_item(ui, "Unhide All", "Alt+H") { self.poly_unhide_all(); }
                            }
                            PolyWorkMode::Edge => {
                                if self.q_menu_item(ui, "Edge Slide", "Shift+E") { self.poly_start_edge_slide(); }
                                if self.q_menu_item(ui, "Bevel Edge", "Ctrl+B") { self.poly_activate_bevel(); }
                                if self.q_menu_item(ui, "Loop Cut", "Ctrl+R") { self.poly_loop_cut(); }
                                if self.q_menu_item(ui, "Knife", "K") { self.poly_activate_knife(); }
                                if self.q_menu_item(ui, "Bridge", "B") { self.poly_bridge(); }
                                if self.q_menu_item(ui, "Circle (Round Loop)", "Alt+C") { self.poly_circle_edges(); }
                                if self.q_menu_item(ui, "Dissolve", "Ctrl+X") { self.poly_dissolve(); }
                                if self.q_menu_item(ui, "Delete Edges", "X") { self.poly_delete_edges(); }
                                ui.separator();
                                if self.q_menu_item(ui, "Select Edge Loop", "Shift+L") { self.poly_select_edge_loop(); }
                                if self.q_menu_item(ui, "Select Edge Ring", "Shift+R") { self.poly_select_edge_ring(); }
                                if self.q_menu_item(ui, "Select Linked", "L") { self.poly_select_linked_hover(false); }
                                ui.separator();
                                if self.q_menu_item(ui, "Grow Selection", "G") { self.poly_grow_selection(); }
                                if self.q_menu_item(ui, "Shrink Selection", "Shift+G") { self.poly_shrink_selection(); }
                                if self.q_menu_item(ui, "Invert Selection", "Ctrl+I") { crate::ops::poly::invert_selection::run(self); }
                                ui.separator();
                                if self.q_menu_item(ui, "Hide Selected", "H") { self.poly_hide_selected(); }
                                if self.q_menu_item(ui, "Unhide All", "Alt+H") { self.poly_unhide_all(); }
                            }
                            PolyWorkMode::Face | PolyWorkMode::Group => {
                                if self.q_menu_item(ui, "Extrude Faces", "E") { self.poly_extrude_faces(0.3); }
                                if self.q_menu_item(ui, "Extrude Along Normals", "Alt+E") {
                                    self.poly_extrude_faces_along_normals(0.3);
                                }
                                if self.q_menu_item(ui, "Extrude Sweep", "Shift+Alt+E") {
                                    let mouse = self.viewport.viewport_mouse;
                                    self.poly_start_extrude_sweep(mouse);
                                }
                                if self.q_menu_item(ui, "Inset Faces", "I") { self.poly_inset_faces(0.1); }
                                if self.q_menu_item(ui, "Poke Faces", "") { self.poly_poke_faces(); }
                                if self.q_menu_item(ui, "Circle (Round Region)", "Alt+C") { self.poly_circle_faces(); }
                                if self.q_menu_item(ui, "Solidify / Shell", "") { self.poly_solidify(0.1); }
                                if self.q_menu_item(ui, "Delete Faces", "X") { self.poly_delete_faces(); }
                                if self.q_menu_item(ui, "Delete Only Faces", "Ctrl+Del") { self.poly_delete_faces_only(); }
                                ui.separator();
                                if self.q_menu_item(ui, "Subdivide", "Ctrl+D") { self.poly_subdivide(); }
                                if self.q_menu_item(ui, "Dice Faces", "Ctrl+Alt+D") { self.poly_activate_dice(); }
                                if self.q_menu_item(ui, "Mark Island Seams", "C") { self.poly_mark_seam_apply(); }
                                if self.q_menu_item(ui, "Loop Cut", "Ctrl+R") { self.poly_loop_cut(); }
                                ui.separator();
                                if self.q_menu_item(ui, "Select Linked", "L") { self.poly_select_linked_hover(false); }
                                if self.q_menu_item(ui, "Grow Selection", "G") { self.poly_grow_selection(); }
                                if self.q_menu_item(ui, "Shrink Selection", "Shift+G") { self.poly_shrink_selection(); }
                                if self.q_menu_item(ui, "Invert Selection", "Ctrl+I") { crate::ops::poly::invert_selection::run(self); }
                                // Geometry channel select (Modo-style: tris / quads / n-gons / material / …).
                                ui.azra_menu_button("Select by Geometry", |ui| {
                                    use core_math::geometry_channel::{GeoChannelKind, GeometryChannel};
                                    let picks = [
                                        ("All faces", GeoChannelKind::All),
                                        ("Triangles (3)", GeoChannelKind::Tris),
                                        ("Quads (4)", GeoChannelKind::Quads),
                                        ("N-gons (5+)", GeoChannelKind::Ngons),
                                        ("5-sided", GeoChannelKind::NSides),
                                        ("Material 0", GeoChannelKind::Material),
                                        ("Edges ~90°", GeoChannelKind::EdgeAngle),
                                    ];
                                    for (lab, kind) in picks {
                                        if ui.button(lab).clicked() {
                                            let mut ch = GeometryChannel::default();
                                            ch.kind = kind;
                                            if kind == GeoChannelKind::NSides {
                                                ch.n_sides = 5;
                                            }
                                            if kind == GeoChannelKind::EdgeAngle {
                                                ch.angle_min_deg = 80.0;
                                                ch.angle_max_deg = 100.0;
                                            }
                                            self.poly_select_by_geo_channel(ch);
                                            ui.close_menu();
                                        }
                                    }
                                });
                                // AUTHORED EDGE WEIGHT — the authoring half of the EdgeWeight
                                // geometry channel. These are QUICK PRESETS; `W` in the viewport is
                                // the interactive path (drag = strength, Shift = 20 % stops, Ctrl =
                                // reset, RMB/Enter accept). Both resolve their target edges through
                                // `poly_edge_weight_targets`, so Face mode weights the selected
                                // region's boundary here too.
                                ui.separator();
                                ui.azra_menu_button("Set Edge Weight  (W)", |ui| {
                                    for (lab, w) in [
                                        ("1.00  (full)", 1.0f32),
                                        ("0.75", 0.75),
                                        ("0.50  (half)", 0.5),
                                        ("0.25", 0.25),
                                        ("Clear (0)", 0.0),
                                    ] {
                                        if ui.button(lab).clicked() {
                                            self.poly_set_edge_weight(w);
                                            ui.close_menu();
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Weight the selected edges (Face mode: the selection's boundary \
                                     edges). W in the viewport dials it interactively. A Smooth \
                                     modifier set to the \"Edge weight\" geometry channel smooths in \
                                     proportion to this value — tick Invert there to make weight \
                                     mean PROTECT.",
                                );
                                // AUTO_POLYGROUPS: only when groups exist (or for assign/viz always).
                                ui.separator();
                                if self.q_menu_item(ui, "Assign Selection to New Group", "") {
                                    self.poly_assign_selection_to_new_group();
                                }
                                let show_pg = self.ui.poly_show_polygroups;
                                if self.q_menu_item(
                                    ui,
                                    if show_pg {
                                        "Hide Polygroup Visualisation"
                                    } else {
                                        "Show Polygroup Visualisation"
                                    },
                                    "",
                                ) {
                                    self.poly_toggle_group_overlay();
                                }
                                if self.poly_has_polygroups() {
                                    if self.q_menu_item(ui, "Select Polygroup (under face)", "") {
                                        self.poly_select_group_under_cursor();
                                    }
                                    // Submenu of named groups: Select / Delete.
                                    ui.azra_menu_button("Select Polygroup", |ui| {
                                        let list = self.poly_group_list();
                                        if list.is_empty() {
                                            ui.label("No groups");
                                        } else {
                                            for m in list {
                                                let label = format!("{}  ·  {}", m.name, m.kind.label());
                                                if ui.button(&label).clicked() {
                                                    self.poly_select_group(m.id);
                                                    ui.close_menu();
                                                }
                                            }
                                        }
                                    });
                                    ui.azra_menu_button("Delete Polygroup", |ui| {
                                        let list = self.poly_group_list();
                                        if list.is_empty() {
                                            ui.label("No groups");
                                        } else {
                                            for m in list {
                                                if ui
                                                    .button(format!("🗑 {}", m.name))
                                                    .clicked()
                                                {
                                                    self.poly_delete_group(m.id);
                                                    ui.close_menu();
                                                }
                                            }
                                        }
                                    });
                                    ui.azra_menu_button("Rename Polygroup", |ui| {
                                        let list = self.poly_group_list();
                                        if list.is_empty() {
                                            ui.label("No groups");
                                        } else {
                                            for m in list {
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(m.kind.label())
                                                            .size(10.0)
                                                            .weak(),
                                                    );
                                                    let mut name = m.name.clone();
                                                    let r = ui.add(
                                                        egui::TextEdit::singleline(&mut name)
                                                            .desired_width(140.0)
                                                            .font(egui::TextStyle::Body),
                                                    );
                                                    if r.lost_focus()
                                                        && name.trim() != m.name
                                                        && !name.trim().is_empty()
                                                    {
                                                        self.poly_rename_group(m.id, name);
                                                    }
                                                });
                                            }
                                        }
                                    });
                                }
                                ui.separator();
                                if self.q_menu_item(ui, "Hide Selected", "H") { self.poly_hide_selected(); }
                                if self.q_menu_item(ui, "Unhide All", "Alt+H") { self.poly_unhide_all(); }
                            }
                            PolyWorkMode::Object => {
                                if self.q_menu_item(ui, "Duplicate", "Ctrl+D") { self.duplicate_selected(); }
                                if self.q_menu_item(ui, "Delete", "Del") { self.delete_selected(); }
                                ui.separator();
                                if self.q_menu_item(ui, "Select All", "A") { self.poly_select_all_toggle(); }
                            }
                        }
                    } else {
                        // SDF / general operations
                        if self.q_menu_item(ui, "Duplicate", "Ctrl+D") { self.duplicate_selected(); }
                        if self.q_menu_item(ui, "Delete", "Del") { self.delete_selected(); }
                        ui.separator();
                        if self.q_menu_item(ui, "Select All", "A") {
                            let ids: Vec<SceneItemId> = self.scene.primitives.iter()
                                .map(|p| SceneItemId::Prim(p.prim_uid)).collect();
                            self.select_after_create(&ids);
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    }
                });
            });

        // Close on click outside or Escape
        if ctx.input(|i| i.key_pressed(egui::Key::Escape))
            || ctx.input(|i| i.pointer.any_pressed()) && !ctx.is_pointer_over_area()
        {
            self.ui.q_menu_open = false;
        }
    }

    fn q_menu_item(&mut self, ui: &mut egui::Ui, label: &str, shortcut: &str) -> bool {
        let clicked = ui.horizontal(|ui| {
            let resp = ui.selectable_label(false, label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(shortcut);
            });
            resp.clicked()
        }).inner;
        if clicked {
            self.ui.q_menu_open = false;
        }
        clicked
    }

    /// Shift+RMB shape swap pie menu — grid of all shape types for quick swap.
    pub(crate) fn draw_shape_swap_pie(&mut self, ctx: &egui::Context) {
        if !self.ui.shape_swap_pie_open { return; }
        let idx = match self.ui.shape_swap_pie_idx {
            Some(i) if i < self.scene.primitives.len() => i,
            _ => { self.ui.shape_swap_pie_open = false; return; }
        };
        let pos = self.ui.shape_swap_pie_pos;
        let current_label = self.scene.primitives[idx].shape.label().to_string();
        let ctrl = ctx.input(|i| i.modifiers.command);
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if esc { self.ui.shape_swap_pie_open = false; return; }

        // Shape swap uses the same presets as creation (single source of truth)
        let shapes: &[(&str, &str, SdfShape)] = &[
            ("\u{25CB}", "Sphere",   Self::shape_preset(0)),
            ("\u{25A1}", "Box",      Self::shape_preset(1)),
            ("\u{2293}", "Cylinder", Self::shape_preset(2)),
            ("\u{25CE}", "Torus",    Self::shape_preset(3)),
            ("\u{2282}", "Capsule",  Self::shape_preset(5)),
            ("\u{25B3}", "Cone",     Self::shape_preset(4)),
            ("\u{2312}", "Polyline", Self::curve_preset()),
            ("\u{25AD}", "Plane",    SdfShape::Billboard { width: 1.0, height: 1.0, face_camera: false }),
        ];

        let mut do_swap: Option<(SdfShape, bool)> = None; // (shape, reset_attrs)

        // Center the popup on the click position
        let popup_size = egui::vec2(200.0, 0.0);
        let anchor_pos = egui::pos2(pos.x - popup_size.x * 0.5, pos.y - 20.0);

        egui::Area::new(egui::Id::new("shape_swap_pie"))
            .fixed_pos(anchor_pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(180.0);
                    ui.vertical(|ui| {
                        if ctrl {
                            ui.colored_label(egui::Color32::YELLOW, "RESET mode (Ctrl held)");
                        } else {
                            ui.label("Swap Shape (hold Ctrl to reset)");
                        }
                        ui.separator();
                        for (icon, label, shape) in shapes {
                            let is_current = *label == current_label.as_str();
                            let text = format!("{} {}", icon, label);
                            let resp = ui.selectable_label(is_current, &text);
                            if resp.clicked() {
                                if !is_current {
                                    do_swap = Some((shape.clone(), ctrl));
                                }
                                self.ui.shape_swap_pie_open = false;
                            }
                        }
                    });
                });
            });

        if let Some((new_shape, reset)) = do_swap {
            if idx < self.scene.primitives.len() {
                let original_uid = self.scene.primitives[idx].prim_uid;
                if reset {
                    self.transaction_snapshot_only("Swap shape (reset)");
                    let pos = self.scene.primitives[idx].position;
                    let scale = self.scene.primitives[idx].scale;
                    self.write_prim(idx, |p| {
                        let saved_pos = p.position;
                        let saved_scale = p.scale;
                        let saved_uid = p.prim_uid;
                        let saved_name = std::mem::take(&mut p.name);
                        *p = SdfPrimitive::new(saved_name, new_shape, saved_pos);
                        p.prim_uid = saved_uid;
                        p.scale = saved_scale;
                    });
                } else {
                    self.transaction_snapshot_only("Swap shape");
                    self.write_prim(idx, |p| p.shape = new_shape);
                }
                self.rebake_no_undo();
            }
        }
    }

    pub(crate) fn draw_context_menu(&mut self, ctx: &egui::Context) {
        if !self.ui.ctx_menu_open { return; }
        let pos = self.ui.ctx_menu_pos;
        let prim_idx = self.ui.ctx_menu_prim;
        let mut ch = false;
        let mut shape_edit = false; // slider/checkbox shape param — needs Resize episode
        // Deferred actions (applied after UI closure)
        let mut do_duplicate = false;
        let mut do_delete    = false;
        // 0..=7 = shape_preset index; 8 = polyline curve_preset — applied via ops + cursor.
        let mut do_add_idx: Option<u8> = None;
        let mut do_select_all = false;
        let mut do_focus = false;
        let mut do_group = false;
        let mut do_instance = false;
        let mut do_instance_all_bodies = false;
        let mut do_make_unique: Option<usize> = None;
        let mut do_reset_xform: Option<u8> = None; // 0=pos, 1=rot, 2=scale, 3=all
        let mut do_open_reset_pie = false;
        let mut new_op: Option<BooleanOp> = None;
        let mut do_mirror_toggle: Option<u8> = None;  // bit to flip
        let mut do_mirror_flip = false;
        let mut do_change_shape: Option<SdfShape> = None;
        let mut do_hide = false;
        let mut do_lock = false;
        let mut do_commit_create: Option<bool> = None; // Some(infinite)
        let mut do_cancel_create = false;
        let mut do_exit_bevel = false;
        let mut do_clear_bevel_edges = false;
        let mut do_exit_extrude = false;
        let mut do_back_to_object = false;
        let mut do_deselect_all = false;
        let mut do_close = false;
        let mut do_poly_cmd: Option<&str> = None;
        let mut do_convert_poly = false;
        let mut do_link_poly = false;
        let mut do_poly_pivot_to_center = false;
        let mut do_poly_pivot_to_selected = false;
        let mut do_poly_mesh_to_pivot = false;
        let mut do_poly_mesh_to_origin = false;

        // Snapshot context for menu logic
        let wm = self.tool.work_mode;
        let has_sel = self.sel.usel.has_prims();
        let poly_body_selected = self.resolve_poly_body_nid().is_some();
        let has_convertible_poly = self.lab_has_convertible_poly();
        let multi_sel = self.selected_prim_indices_unified().len() > 1;
        // Kitbash Create Group: 1+ selected SdfBody / Group / SdfObject (Ctrl+G path).
        let nestable_sel = self.sel.usel.selected_nodes().any(|n| {
            self.scene
                .scene_graph
                .get(n)
                .map(|x| Self::is_body_drag_payload(&x.payload))
                .unwrap_or(false)
        });
        let show_group = multi_sel || nestable_sel;
        let bevel_mode = self.tool.mode.is_bevel();
        let extrude_mode = self.tool.mode.is_extrude();
        let is_creating = self.tool.current.is_create();

        let mut win_open = self.ui.ctx_menu_open;
        // Escape: clear search first; second Esc (or empty search + Esc) closes the menu.
        if win_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if !self.ui.ctx_menu_search.is_empty() {
                self.ui.ctx_menu_search.clear();
            } else {
                win_open = false;
            }
        }
        // Tooltip layer: must sit above ui2 `.tool-options` (Order::Foreground Box Tool /
        // XYZ bar) — same fix as Shift+A add popup (owner 2026-08-06).
        let win_resp = egui::Window::new("##vp_ctx")
            .fixed_pos(pos)
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .frame(egui::Frame::popup(ctx.style().as_ref()))
            .order(egui::Order::Tooltip)
            .open(&mut win_open)
            .show(ctx, |ui| {
                ui.set_min_width(200.0);

                // ── Search bar (type to filter commands) ─────────────────────
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("\u{1F50D}")
                            .size(12.0)
                            .color(egui::Color32::from_gray(160)),
                    );
                    let te = egui::TextEdit::singleline(&mut self.ui.ctx_menu_search)
                        .id(egui::Id::new("vp_ctx_search"))
                        .desired_width(ui.available_width().max(140.0))
                        .hint_text("Search…")
                        .frame(false)
                        .font(egui::FontId::proportional(12.5));
                    let r = ui.add(te);
                    if self.ui.ctx_menu_focus_search {
                        r.request_focus();
                        self.ui.ctx_menu_focus_search = false;
                    }
                });
                ui.separator();
                // Lowercase once for the filter; empty string shows all items.
                let q = self.ui.ctx_menu_search.to_lowercase();

                // ---- Context: Create tool active ----
                if is_creating {
                    ui.strong("Create Tool");
                    ui.separator();
                    if ctx_btn(ui, &q, "Confirm  Enter") { do_commit_create=Some(false); do_close=true; }
                    if ctx_btn(ui, &q, "Through-cut  C") { do_commit_create=Some(true); do_close=true; }
                    if ctx_btn(ui, &q, "Cancel  Esc") { do_cancel_create=true; do_close=true; }
                    return;
                }

                // ---- Context: Bevel mode ----
                if bevel_mode {
                    ui.strong("Bevel Mode");
                    ui.separator();
                    if ctx_btn(ui, &q, "Exit Bevel Mode  B") { do_exit_bevel=true; do_close=true; }
                    if !self.tool.selected_bevel_edges.is_empty() {
                        if ctx_btn(ui, &q, "Clear Edge Selection") { do_clear_bevel_edges=true; do_close=true; }
                    }
                    return;
                }

                // ---- Context: Extrude mode ----
                if extrude_mode {
                    ui.strong("Extrude Mode");
                    ui.separator();
                    if ctx_btn(ui, &q, "Exit Extrude Mode") { do_exit_extrude=true; do_close=true; }
                    return;
                }

                // ---- Context: Poly edit mode (Polygonal modeling) ----
                if self.modeling_context == ModelingContext::Polygonal && self.poly_work_mode != PolyWorkMode::Object {
                    let mode_label = match self.poly_work_mode {
                        PolyWorkMode::Face => "Poly Face Mode",
                        PolyWorkMode::Group => "Poly Group Mode",
                        PolyWorkMode::Edge => "Poly Edge Mode",
                        PolyWorkMode::Vertex => "Poly Vertex Mode",
                        _ => "Poly Edit",
                    };
                    ui.strong(mode_label);
                    ui.separator();
                    // Pivot → selected face / edge / vertex (owner: Ctrl+RMB context menu).
                    let has_poly_sel = match self.poly_work_mode {
                        PolyWorkMode::Vertex => !self.poly_sel.selected_vertices.is_empty(),
                        PolyWorkMode::Edge => !self.poly_sel.selected_edges.is_empty(),
                        PolyWorkMode::Face | PolyWorkMode::Group => !self.poly_sel.selected_faces.is_empty(),
                        _ => false,
                    };
                    if has_poly_sel {
                        let pivot_label = match self.poly_work_mode {
                            PolyWorkMode::Vertex => "Pivot to Selected Vertex",
                            PolyWorkMode::Edge => "Pivot to Selected Edge",
                            PolyWorkMode::Face => "Pivot to Selected Face",
                            PolyWorkMode::Group => "Pivot to Selected Group",
                            _ => "Pivot to Selected",
                        };
                        if ctx_btn_tip(
                            ui,
                            &q,
                            pivot_label,
                            "Move the object pivot to the center of the current selection.\n\
                             Geometry stays put. Works for face / edge / vertex.",
                        ) {
                            do_poly_pivot_to_selected = true;
                            do_close = true;
                        }
                        ui.separator();
                    }
                    let pwm = self.poly_work_mode;
                    // Full mode-valid tool catalog (tools the current selection domain can accept).
                    // Keep chords in the label so the menu doubles as a discoverability surface.
                    match pwm {
                        PolyWorkMode::Face | PolyWorkMode::Group => {
                            if ctx_btn(ui, &q, "Extrude Faces  E") { do_poly_cmd=Some("extrude"); do_close=true; }
                            if ctx_btn(ui, &q, "Extrude Along Normals  Alt+E") { do_poly_cmd=Some("extrude_normals"); do_close=true; }
                            if ctx_btn(ui, &q, "Extrude Sweep  Shift+Alt+E") { do_poly_cmd=Some("extrude_sweep"); do_close=true; }
                            if ctx_btn(ui, &q, "Inset Faces  I") { do_poly_cmd=Some("inset"); do_close=true; }
                            if ctx_btn(ui, &q, "Bevel  Ctrl+B") { do_poly_cmd=Some("bevel"); do_close=true; }
                            if ctx_btn(ui, &q, "Unbevel / Resize  Ctrl+Alt+B") { do_poly_cmd=Some("unbevel"); do_close=true; }
                            if ctx_btn(ui, &q, "Hinge Rotate  Ctrl+Shift+H") { do_poly_cmd=Some("face_hinge"); do_close=true; }
                            if ctx_btn(ui, &q, "Shrink/Fatten  Alt+S") { do_poly_cmd=Some("shrink_fatten"); do_close=true; }
                            if ctx_btn(ui, &q, "Poke Faces") { do_poly_cmd=Some("poke"); do_close=true; }
                            if ctx_btn(ui, &q, "Circle (Round Region)  Alt+C") { do_poly_cmd=Some("circle"); do_close=true; }
                            if ctx_btn(ui, &q, "Solidify / Shell") { do_poly_cmd=Some("solidify"); do_close=true; }
                            if ctx_btn(ui, &q, "To Sphere") { do_poly_cmd=Some("tosphere"); do_close=true; }
                            if ctx_btn(ui, &q, "Subdivide") { do_poly_cmd=Some("subdivide"); do_close=true; }
                            if ctx_btn(ui, &q, "Dice Faces  Ctrl+Alt+D") { do_poly_cmd=Some("dice"); do_close=true; }
                            if ctx_btn(ui, &q, "Knife  K") { do_poly_cmd=Some("knife"); do_close=true; }
                            if ctx_btn(ui, &q, "Mark Island Seams  C") { do_poly_cmd=Some("toggle_seam"); do_close=true; }
                            ui.separator();
                            if ctx_btn(ui, &q, "Dissolve  Ctrl+X") { do_poly_cmd=Some("dissolve"); do_close=true; }
                            if ctx_btn(ui, &q, "Delete Faces  X") { do_poly_cmd=Some("delete"); do_close=true; }
                            if ctx_btn(ui, &q, "Delete Only Faces  Ctrl+Del") { do_poly_cmd=Some("delete_only_faces"); do_close=true; }
                            if ctx_btn(ui, &q, "Separate Selection  P") { do_poly_cmd=Some("separate"); do_close=true; }
                            if ctx_btn(ui, &q, "Flip Normals") { do_poly_cmd=Some("flip_normals"); do_close=true; }
                        }
                        PolyWorkMode::Edge => {
                            // Transform / slide
                            if ctx_btn(ui, &q, "Edge Slide  Shift+E") { do_poly_cmd=Some("edge_slide"); do_close=true; }
                            if ctx_btn(ui, &q, "Extrude Edges  E") { do_poly_cmd=Some("edge_extrude"); do_close=true; }
                            if ctx_btn(ui, &q, "Bevel Edge  Ctrl+B") { do_poly_cmd=Some("bevel"); do_close=true; }
                            if ctx_btn(ui, &q, "Unbevel / Resize  Ctrl+Alt+B") { do_poly_cmd=Some("unbevel"); do_close=true; }
                            if ctx_btn(ui, &q, "Loop Cut  Ctrl+R") { do_poly_cmd=Some("loop_cut"); do_close=true; }
                            if ctx_btn(ui, &q, "Knife  K") { do_poly_cmd=Some("knife"); do_close=true; }
                            if ctx_btn(ui, &q, "Bridge  B") { do_poly_cmd=Some("bridge"); do_close=true; }
                            // Fill now accepts an EDGE selection (owner: "I did not see the command
                            // to create a connected poly from selection" — it existed but was
                            // Vertex-mode only, so it never appeared in this menu).
                            if ctx_btn(ui, &q, "Fill Face  F") { do_poly_cmd=Some("fill"); do_close=true; }
                            if ctx_btn(ui, &q, "Circle (Round Loop)  Alt+C") { do_poly_cmd=Some("circle"); do_close=true; }
                            if ctx_btn(ui, &q, "To Sphere") { do_poly_cmd=Some("tosphere"); do_close=true; }
                            if ctx_btn(ui, &q, "Subdivide") { do_poly_cmd=Some("subdivide"); do_close=true; }
                            ui.separator();
                            if ctx_btn(ui, &q, "LoopTools Relax") { do_poly_cmd=Some("loop_relax"); do_close=true; }
                            if ctx_btn(ui, &q, "LoopTools Flatten") { do_poly_cmd=Some("loop_flatten"); do_close=true; }
                            if ctx_btn(ui, &q, "LoopTools Space") { do_poly_cmd=Some("loop_space"); do_close=true; }
                            ui.separator();
                            if ctx_btn(ui, &q, "Dissolve  Ctrl+X") { do_poly_cmd=Some("dissolve"); do_close=true; }
                            if ctx_btn(ui, &q, "Delete Edges  X") { do_poly_cmd=Some("delete_edges"); do_close=true; }
                            ui.separator();
                            if ctx_btn(ui, &q, "Edge Loop  Alt+Click") { do_poly_cmd=Some("edge_loop"); do_close=true; }
                            if ctx_btn(ui, &q, "Edge Ring") { do_poly_cmd=Some("edge_ring"); do_close=true; }
                            ui.separator();
                            // Blender's Ctrl+E edge menu: hard edges that survive any Auto Smooth angle.
                            if ctx_btn(ui, &q, "Mark Sharp  Ctrl+Shift+E") { do_poly_cmd=Some("mark_sharp"); do_close=true; }
                            if ctx_btn(ui, &q, "Clear Sharp  Ctrl+Alt+E") { do_poly_cmd=Some("clear_sharp"); do_close=true; }
                            ui.separator();
                            if ctx_btn(ui, &q, "Toggle Seam  C") { do_poly_cmd=Some("toggle_seam"); do_close=true; }
                            if ctx_btn(ui, &q, "Mark Seam") { do_poly_cmd=Some("mark_seam"); do_close=true; }
                            if ctx_btn(ui, &q, "Clear Seam") { do_poly_cmd=Some("clear_seam"); do_close=true; }
                        }
                        PolyWorkMode::Vertex => {
                            if ctx_btn(ui, &q, "Vertex Slide  Shift+V") { do_poly_cmd=Some("vert_slide"); do_close=true; }
                            if ctx_btn(ui, &q, "Merge Vertices  M") { do_poly_cmd=Some("merge"); do_close=true; }
                            if ctx_btn(ui, &q, "Connect Vertex Path  J") { do_poly_cmd=Some("connect"); do_close=true; }
                            if ctx_btn(ui, &q, "Fill Face  F") { do_poly_cmd=Some("fill"); do_close=true; }
                            if ctx_btn(ui, &q, "Bridge  B") { do_poly_cmd=Some("bridge"); do_close=true; }
                            if ctx_btn(ui, &q, "Bevel  Ctrl+B") { do_poly_cmd=Some("bevel"); do_close=true; }
                            if ctx_btn(ui, &q, "Knife  K") { do_poly_cmd=Some("knife"); do_close=true; }
                            if ctx_btn(ui, &q, "To Sphere") { do_poly_cmd=Some("tosphere"); do_close=true; }
                            if ctx_btn(ui, &q, "Subdivide") { do_poly_cmd=Some("subdivide"); do_close=true; }
                            ui.separator();
                            if ctx_btn(ui, &q, "Rip  V") { do_poly_cmd=Some("rip"); do_close=true; }
                            if ctx_btn(ui, &q, "Rip Fill  Alt+V") { do_poly_cmd=Some("rip_fill"); do_close=true; }
                            ui.separator();
                            if ctx_btn(ui, &q, "Dissolve  Ctrl+X") { do_poly_cmd=Some("dissolve"); do_close=true; }
                            if ctx_btn(ui, &q, "Delete Vertices  X") { do_poly_cmd=Some("delete_verts"); do_close=true; }
                        }
                        _ => {}
                    }
                    ui.separator();
                    // Body-level shading (Blender's Object > Shade Smooth / Shade Flat).
                    if ctx_btn(ui, &q, "Shade Smooth  Ctrl+Shift+N") { do_poly_cmd=Some("shade_smooth"); do_close=true; }
                    if ctx_btn(ui, &q, "Shade Flat  Ctrl+Alt+N") { do_poly_cmd=Some("shade_flat"); do_close=true; }
                    ui.separator();
                    // UV items routed through ctx_btn/ctx_btn_tip like the rest of this menu —
                    // master made it search-filterable while this branch was written against
                    // the old plain-ui.button form; a raw button here would ignore the filter.
                    let uv_unwrap_label =
                        format!("Unwrap (Smart)  {}", self.keybindings.label(ACT_POLY_UV_UNWRAP));
                    let uv_layout_label =
                        format!("Show UV Layout  {}", self.keybindings.label(ACT_VIEW_UV_LAYOUT));
                    if q.is_empty() { ui.weak("UV"); }
                    if ctx_btn_tip(
                        ui,
                        &q,
                        &uv_unwrap_label,
                        "Angle-based charts \u{2192} pack into 0\u{2013}1 UV space (like Blender Smart UV Project)",
                    ) {
                        do_poly_cmd = Some("uv_unwrap");
                        do_close = true;
                    }
                    if ctx_btn_tip(
                        ui,
                        &q,
                        "Pack UVs",
                        "Repack the existing UV islands into 0-1 without re-unwrapping (keeps the                          layout you authored).",
                    ) {
                        do_poly_cmd = Some("pack_uvs");
                        do_close = true;
                    }
                    if ctx_btn_tip(
                        ui,
                        &q,
                        "Bake Mesh Maps",
                        "Bake Curvature from the POST-modifier mesh into this body's UV layout,                          as a reusable texture (Substance-style mesh map). Needs UVs.",
                    ) {
                        do_poly_cmd = Some("bake_mesh_maps");
                        do_close = true;
                    }
                    if ctx_btn(ui, &q, &uv_layout_label) {
                        do_poly_cmd = Some("uv_layout");
                        do_close = true;
                    }
                    if ctx_btn_tip(
                        ui,
                        &q,
                        &format!("Toggle Seam  {}", self.keybindings.label(ACT_POLY_SEAM)),
                        "C: toggle selected/hovered edges · face island + C: mark island boundary seams",
                    ) {
                        do_poly_cmd = Some("toggle_seam");
                        do_close = true;
                    }
                    ui.separator();
                    if ctx_btn(ui, &q, "Select All  A") { do_poly_cmd=Some("select_all"); do_close=true; }
                    if ctx_btn(ui, &q, "Invert Selection  Ctrl+I") { do_poly_cmd=Some("invert_sel"); do_close=true; }
                    ui.separator();
                    // Selection Shape — drives RMB region select (Box/Circle/Lasso).
                    // Plain RMB drag = visible; Ctrl/Alt+RMB drag = through/x-ray; Shift+RMB drag = deselect.
                    // Viewport menu is Ctrl+RMB click (plain RMB = cancel / region only).
                    if ctx_q_match(&q, "selection shape box circle lasso") {
                        ui.weak("Selection Shape");
                        for shape in [SelectionShape::Box, SelectionShape::Circle, SelectionShape::Lasso] {
                            if ctx_q_match(&q, shape.label())
                                && ui.radio(self.poly_sel_shape == shape, shape.label()).clicked()
                            {
                                self.poly_sel_shape = shape;
                            }
                        }
                        if q.is_empty() {
                            ui.weak("RMB drag = region • Ctrl/Alt = through • Ctrl+RMB click = menu");
                        }
                    }
                    ui.separator();
                    if ctx_btn(ui, &q, "Back to Object Mode  Tab") { do_back_to_object=true; do_close=true; }
                    return;
                }
                // ---- Context: SDF Face/Edge/Vertex edit modes ----
                if wm == WorkMode::Face || wm == WorkMode::Edge || wm == WorkMode::Vertex {
                    let mode_label = match wm {
                        WorkMode::Face => "Face Mode",
                        WorkMode::Edge => "Edge Mode",
                        WorkMode::Vertex => "Vertex Mode",
                        _ => "Edit Mode",
                    };
                    ui.strong(mode_label);
                    ui.separator();
                    if ctx_btn(ui, &q, "Back to Object Mode  Tab") { do_back_to_object=true; do_close=true; }
                    return;
                }

                // ---- Context: Object mode — prim under cursor ----
                if let Some(idx) = prim_idx {
                    if idx < self.scene.primitives.len() {
                        // Header: name + operation badge
                        let name = self.scene.primitives[idx].name.clone();
                        let op_label = if self.scene.primitives[idx].operation.is_additive() { "Union" }
                            else if matches!(&self.scene.primitives[idx].operation, BooleanOp::Intersect) { "Intersect" }
                            else if matches!(&self.scene.primitives[idx].operation, BooleanOp::Seam { .. }) { "Seam" }
                            else if matches!(&self.scene.primitives[idx].operation, BooleanOp::Extrude { .. }) { "Extrude" }
                            else { "Subtract" };
                        ui.horizontal(|ui| {
                            ui.strong(&name);
                            ui.weak(format!("[{}]", op_label));
                        });
                        ui.separator();

                        // -- Shape quick-edit sliders --
                        match &mut self.scene.primitives[idx].shape {
                            SdfShape::Sphere{radius} => {
                                let rr=ui.add(egui::Slider::new(radius,0.05..=2.0).text("R"));
                                if rr.changed(){ch=true; shape_edit=true;}
                            }
                            SdfShape::Box{half_extents:h,..} => {
                                for (v,l) in [(&mut h.x,"W"),(&mut h.y,"H"),(&mut h.z,"D")] {
                                    let rr=ui.add(egui::Slider::new(v,0.05..=2.0).text(l));
                                    if rr.changed(){ch=true; shape_edit=true;}
                                }
                            }
                            SdfShape::Cylinder{radius,half_height}|SdfShape::Cone{radius,half_height}|SdfShape::Capsule{radius,half_height} => {
                                for (v,l) in [(&mut *radius,"R"),(&mut *half_height,"H")] {
                                    let rr=ui.add(egui::Slider::new(v,0.05..=2.0).text(l));
                                    if rr.changed(){ch=true; shape_edit=true;}
                                }
                            }
                            SdfShape::Torus{major_radius,minor_radius} => {
                                for (v,l) in [(&mut *major_radius,"Maj"),(&mut *minor_radius,"Min")] {
                                    let rr=ui.add(egui::Slider::new(v,0.02..=2.0).text(l));
                                    if rr.changed(){ch=true; shape_edit=true;}
                                }
                            }
                            SdfShape::Stair{half_extents:h,num_steps} => {
                                for (v,l) in [(&mut h.x,"W"),(&mut h.y,"H"),(&mut h.z,"D")] {
                                    let rr=ui.add(egui::Slider::new(v,0.05..=2.0).text(l));
                                    if rr.changed(){ch=true; shape_edit=true;}
                                }
                                let rr=ui.add(egui::Slider::new(num_steps,1..=32).text("Steps"));
                                if rr.changed(){ch=true; shape_edit=true;}
                            }
                            SdfShape::SpiralStair{radius,half_height,num_steps,step_width} => {
                                for (v,l) in [(&mut *radius,"R"),(&mut *half_height,"H"),(&mut *step_width,"StepW")] {
                                    let rr=ui.add(egui::Slider::new(v,0.05..=2.0).text(l));
                                    if rr.changed(){ch=true; shape_edit=true;}
                                }
                                let rr=ui.add(egui::Slider::new(num_steps,1..=64).text("Steps"));
                                if rr.changed(){ch=true; shape_edit=true;}
                            }
                            SdfShape::Billboard{width,height,face_camera} => {
                                for (v,l) in [(&mut *width,"W"),(&mut *height,"H")] {
                                    let rr=ui.add(egui::Slider::new(v,0.1..=10.0).text(l));
                                    if rr.changed(){ch=true; shape_edit=true;}
                                }
                                if ui.checkbox(face_camera,"Face Camera").changed(){ch=true; shape_edit=true;}
                            }
                            SdfShape::CurveExtrude { .. } => { /* not editable from menu */ }
                            SdfShape::MeshVolume { grid, .. } => { ui.label(format!("Mesh volume ({}³ grid)", grid.res)); }
                            SdfShape::HalfSpace => { ui.weak("Slice plane — orient via transform"); }
                            SdfShape::SpinePipe { points, radius, closed, .. } => {
                                ui.label(format!(
                                    "Spine: {}/{} pts  r={:.3}",
                                    points.len(),
                                    core_math::curves::MAX_SPINE_POINTS,
                                    radius
                                ));
                                let rr = ui.add(egui::Slider::new(radius, 0.01..=0.5).text("Radius"));
                                if rr.changed() { ch = true; shape_edit = true; }
                                let mut cl = *closed;
                                if ui.checkbox(&mut cl, "Closed loop").changed() {
                                    *closed = cl;
                                    ch = true;
                                    shape_edit = true;
                                }
                                let _ = points; // Phase B: per-point radius
                            }
                        }
                        ui.separator();

                        // -- Boolean operation submenu --
                        if ctx_q_match(&q, "boolean op union subtract intersect") {
                            ui.azra_menu_button("Boolean Op", |ui| {
                                if let Some(op) = crate::ui_panels::bool_op_menu_items(ui, &self.scene.primitives[idx].operation, false) {
                                    new_op = Some(op);
                                }
                            });
                        }

                        // -- Mirror submenu --
                        if ctx_q_match(&q, "mirror x y z flip") {
                        ui.azra_menu_button("Mirror", |ui| {
                            let mf = self.scene.primitives[idx].mirror_flags;
                            let x_on = mf & 1 != 0;
                            let y_on = mf & 2 != 0;
                            let z_on = mf & 4 != 0;
                            let mx = format!("{} Mirror X", if x_on {"[X]"} else {"[ ]"});
                            let my = format!("{} Mirror Y", if y_on {"[Y]"} else {"[ ]"});
                            let mz = format!("{} Mirror Z", if z_on {"[Z]"} else {"[ ]"});
                            if ctx_btn(ui, &q, &mx) { do_mirror_toggle=Some(1); ui.close_menu(); }
                            if ctx_btn(ui, &q, &my) { do_mirror_toggle=Some(2); ui.close_menu(); }
                            if ctx_btn(ui, &q, &mz) { do_mirror_toggle=Some(4); ui.close_menu(); }
                            // Body-level flip: reflect prims across parent body's mirror plane
                            ui.separator();
                            let body_mirror_on = self.scene.prim_body_map.get(idx)
                                .and_then(|&(body_nid, _)| self.scene.scene_graph.get(body_nid))
                                .map_or(false, |node| {
                                    if let core_math::scene_graph::NodePayload::SdfBody { mirror_flags, .. } = &node.payload { *mirror_flags != 0 } else { false }
                                });
                            ui.add_enabled_ui(body_mirror_on, |ui| {
                                if ctx_btn_tip(ui, &q, "Flip Mirror", "Move prims to the other side of the body mirror plane") { do_mirror_flip = true; ui.close_menu(); }
                            });
                        });
                        } // end Mirror search filter

                        // -- Change Shape submenu --
                        if ctx_q_match(&q, "change shape sphere box cylinder torus capsule cone polyline plane") {
                        ui.azra_menu_button("Change Shape", |ui| {
                            let current_label = self.scene.primitives[idx].shape.label().to_string();
                            // Single source of truth: shape_preset()
                            let shapes: &[(&str, &str, SdfShape)] = &[
                                ("\u{25CB}", "Sphere",   Self::shape_preset(0)),
                                ("\u{25A1}", "Box",      Self::shape_preset(1)),
                                ("\u{2293}", "Cylinder", Self::shape_preset(2)),
                                ("\u{25CE}", "Torus",    Self::shape_preset(3)),
                                ("\u{2282}", "Capsule",  Self::shape_preset(5)),
                                ("\u{25B3}", "Cone",     Self::shape_preset(4)),
                                ("\u{2312}", "Polyline", Self::curve_preset()),
                                ("\u{25AD}", "Plane",    SdfShape::Billboard { width: 1.0, height: 1.0, face_camera: false }),
                            ];
                            for (icon, label, shape) in shapes {
                                let row = format!("{} {}", icon, label);
                                if !ctx_q_match(&q, &row) { continue; }
                                let is_current = *label == current_label.as_str();
                                if ui.selectable_label(is_current, row).clicked() {
                                    if !is_current {
                                        do_change_shape = Some(shape.clone());
                                    }
                                    ui.close_menu();
                                }
                            }
                        });
                        } // end Change Shape search filter

                        ui.separator();

                        // -- Transform actions --
                        if ctx_btn(ui, &q, "\u{1F4CB} Duplicate  Shift+D") { do_duplicate=true; }
                        if ctx_btn_tip(
                            ui, &q,
                            "\u{1F4BE} Save Body to Library\u{2026}",
                            "Save the owning SDF body to kitbash library (name + folder).\nView → Body Library to browse / drag into viewport.",
                        ) {
                            self.open_library_save_for_selection();
                            do_close = true;
                        }
                        if ctx_q_match(&q, "reset transform position rotation scale pie") {
                            ui.azra_menu_button("\u{1F504} Reset Transform", |ui| {
                                if ctx_btn(ui, &q, "Reset Position") { do_reset_xform=Some(0); ui.close_menu(); }
                                if ctx_btn(ui, &q, "Reset Rotation") { do_reset_xform=Some(1); ui.close_menu(); }
                                if ctx_btn(ui, &q, "Reset Scale")    { do_reset_xform=Some(2); ui.close_menu(); }
                                ui.separator();
                                if ctx_btn(ui, &q, "Reset All")      { do_reset_xform=Some(3); ui.close_menu(); }
                                ui.separator();
                                if ctx_btn(ui, &q, "\u{25C9} Pie Menu\u{2026}") { do_open_reset_pie=true; ui.close_menu(); }
                            });
                        }
                        if ctx_btn(ui, &q, "\u{1F50D} Focus  F") { do_focus=true; }
                        let hide_lbl = if self.scene.primitives[idx].visible {"\u{1F441} Hide"} else {"\u{1F441} Show"};
                        let lock_lbl = if self.scene.primitives[idx].locked {"\u{1F513} Unlock"} else {"\u{1F512} Lock"};
                        if ctx_btn(ui, &q, hide_lbl) { do_hide=true; }
                        if ctx_btn(ui, &q, lock_lbl) { do_lock=true; }
                        ui.separator();

                        // -- Instance --
                        if self.scene.primitives[idx].instance_master_id.is_some() {
                            if ctx_btn_tip(
                                ui, &q,
                                "Make Unique / Break Link",
                                "SDF instance → independent prim (stop following master shape/boolean).",
                            ) {
                                do_make_unique = Some(idx);
                            }
                        } else {
                            if ctx_btn(ui, &q, "Create Instance  Ctrl+I") {
                                do_instance = true;
                            }
                            if ctx_btn_tip(
                                ui, &q,
                                "Instance Into All Bodies",
                                "Put a linked instance of this prim into every other top-level SDF body. Shape/material/boolean follow the master; transform is per-instance. Use Make Unique to break a link.",
                            ) {
                                do_instance_all_bodies = true;
                            }
                        }

                        // -- Group / stack freeze --
                        if show_group {
                            let group_label = if nestable_sel && !multi_sel {
                                "Create Group  Ctrl+G"
                            } else if nestable_sel {
                                "Group Selected  Ctrl+G"
                            } else {
                                "Group  Ctrl+G"
                            };
                            if ctx_btn_tip(
                                ui, &q, group_label,
                                "Prims (same body) → boolean sub-body group.\nBodies / Groups → kitbash Group folder (1+ ok).\nDouble-click the group name to rename.",
                            ) {
                                do_group = true;
                            }
                        }
                        if ctx_btn_tip(
                            ui, &q,
                            "Stack → SDF (merge down)",
                            "Freeze selected prim + everything before it in the body stack into one MeshVolume solid (collapse complex history).",
                        ) {
                            self.stack_to_sdf();
                            do_close = true;
                        }
                        ui.separator();
                        if ctx_btn(ui, &q, "\u{1F5D1} Delete  X") { do_delete=true; }
                    }
                } else {
                    // ---- Context: poly object selected (PolygonalBody) — pivot / mesh placement ----
                    // Shown when a poly body is selected even if the RMB hit no SDF prim.
                    if poly_body_selected {
                        ui.strong("Poly Object");
                        ui.separator();
                        // Component selection while in Object mode still holds last component sets —
                        // offer Pivot to Selected when anything is selected.
                        let has_comp = !self.poly_sel.selected_vertices.is_empty()
                            || !self.poly_sel.selected_edges.is_empty()
                            || !self.poly_sel.selected_faces.is_empty();
                        if has_comp {
                            if ctx_btn_tip(
                                ui, &q,
                                "Pivot to Selected",
                                "Move the pivot to the center of the last face / edge / vertex selection.\n\
                                 Geometry stays put.",
                            ) {
                                do_poly_pivot_to_selected = true;
                                do_close = true;
                            }
                        }
                        if ctx_btn_tip(
                            ui, &q,
                            "Pivot to Center Mesh",
                            "Move the pivot to the mesh bounding-box center.\nGeometry stays put in the world.",
                        ) {
                            do_poly_pivot_to_center = true;
                            do_close = true;
                        }
                        if ctx_btn_tip(
                            ui, &q,
                            "Mesh to Pivot",
                            "Shift the mesh so its center sits on the current pivot.\nPivot stays put.",
                        ) {
                            do_poly_mesh_to_pivot = true;
                            do_close = true;
                        }
                        if ctx_btn_tip(
                            ui, &q,
                            "Mesh to Origin",
                            "Center the mesh at local 0 and place the object at world (0, 0, 0).",
                        ) {
                            do_poly_mesh_to_origin = true;
                            do_close = true;
                        }
                        ui.separator();
                    }
                    // ---- Context: Object mode — empty space ----
                    // Defer shape index (0..=7) or polyline (8); apply via ops + cursor (not hardcoded SdfShape).
                    if ctx_q_match(&q, "add primitive sphere box cylinder torus cone capsule stair spiral polyline spine pipe") {
                    ui.azra_menu_button("\u{2795} Add Primitive", |ui| {
                        if ctx_btn(ui, &q, "\u{26AA} Sphere")  { do_add_idx=Some(0); ui.close_menu(); }
                        if ctx_btn(ui, &q, "\u{2B1C} Box")     { do_add_idx=Some(1); ui.close_menu(); }
                        if ctx_btn(ui, &q, "\u{1F535} Cylinder"){ do_add_idx=Some(2); ui.close_menu(); }
                        if ctx_btn(ui, &q, "\u{1F369} Torus")   { do_add_idx=Some(3); ui.close_menu(); }
                        if ctx_btn(ui, &q, "\u{1F53A} Cone")    { do_add_idx=Some(4); ui.close_menu(); }
                        if ctx_btn(ui, &q, "\u{1F48A} Capsule") { do_add_idx=Some(5); ui.close_menu(); }
                        ui.separator();
                        if ctx_btn(ui, &q, "\u{1FA9C} Stair") { do_add_idx=Some(6); ui.close_menu(); }
                        if ctx_btn(ui, &q, "\u{1F300} Spiral Stair") { do_add_idx=Some(7); ui.close_menu(); }
                        ui.separator();
                        if ctx_btn(ui, &q, "\u{2312} Polyline") { do_add_idx=Some(8); ui.close_menu(); }
                        if ctx_btn(ui, &q, "\u{223F} Spine / Pipe") { do_add_idx=Some(9); ui.close_menu(); }
                    });
                    }
                    // Poly → SDF: convert / link the selected/first closed poly.
                    if has_convertible_poly {
                        if ctx_btn_tip(ui, &q, "\u{25C8} Convert Poly \u{2192} SDF Primitive", "Turn the closed polygon mesh into an SDF primitive in the active body\n(union by default; set subtract/intersect + blend in the shape panel). Poly consumed.") { do_convert_poly=true; do_close=true; }
                        if ctx_btn_tip(ui, &q, "\u{1F517} Link Poly \u{2192} SDF Modifier (live)", "LIVE: the imprint follows the poly's transform and re-converts on edits.\nThe poly is hidden (not deleted).") { do_link_poly=true; do_close=true; }
                    }
                    ui.separator();
                    if ctx_btn(ui, &q, "Select All  A") { do_select_all=true; }
                    if has_sel {
                        if ctx_btn(ui, &q, "Deselect All") { do_deselect_all=true; do_close=true; }
                    }
                    ui.separator();
                    if ctx_btn_tip(ui, &q, "\u{1F4BE} Save Body to Library\u{2026}", "Save active / selected SDF body to kitbash library.") {
                        self.open_library_save_for_selection();
                        do_close = true;
                    }
                    if ctx_btn_tip(ui, &q, "\u{2B21} Body Library\u{2026}", "Open kitbash browser — drag bodies into the viewport.") {
                        self.ui.show_body_library = true;
                        self.ui.body_library_needs_scan = true;
                        do_close = true;
                    }
                    ui.separator();
                    if ctx_btn(ui, &q, "\u{1F4E4} Export Mesh...") {
                        self.ui.show_export_window = true;
                        do_close=true;
                    }
                    if ctx_btn(ui, &q, "\u{2699} Settings...") {
                        self.ui.show_shortcut_editor = true;
                        do_close=true;
                    }
                    if ctx_btn(ui, &q, "\u{1F527} Developer Tweaks...") {
                        self.ui.show_tweak_panel = true;
                        do_close=true;
                    }
                }
            });

        // Click outside (viewport or chrome) closes the menu — viewport input is blocked
        // while open (`floating_popup_blocks_viewport`), so this is the only path that
        // dismisses on LMB in empty space (owner 2026-08-06: "when right click menu is out
        // click in viewport need to close"). Primary only: the opening RMB must not
        // immediately re-close on the same frame.
        if win_open && !do_close {
            let primary_click = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
            if primary_click {
                if let Some(click_pos) = ctx.input(|i| i.pointer.interact_pos()) {
                    let menu_rect = win_resp
                        .as_ref()
                        .map(|r| r.response.rect.expand(4.0))
                        .unwrap_or_else(|| egui::Rect::from_min_size(pos, egui::vec2(220.0, 320.0)));
                    if !menu_rect.contains(click_pos) {
                        win_open = false;
                    }
                }
            }
        }

        // Sync back the X button close / outside click
        self.ui.ctx_menu_open = win_open && !do_close;

        // ---- Apply deferred actions ----
        if do_duplicate { self.duplicate_selected(); self.ui.ctx_menu_open=false; }
        if do_delete    { self.delete_selected(); self.ui.ctx_menu_open=false; }
        if do_focus     { self.focus_selected(); self.ui.ctx_menu_open=false; }
        if do_group {
            self.group_selected_bodies_or_prims();
            self.ui.ctx_menu_open = false;
        }
        if do_instance  { crate::ops::instance::create::run(self); self.ui.ctx_menu_open=false; }
        if do_instance_all_bodies {
            self.instance_into_all_bodies();
            self.ui.ctx_menu_open = false;
        }
        if do_open_reset_pie {
            let pos = self.ui.ctx_menu_pos;
            self.ui.reset_pie.open(pos, 4);
            self.ui.ctx_menu_open=false;
        }
        if let Some(idx) = do_make_unique {
            self.make_instance_unique(idx);
            self.ui.ctx_menu_open = false;
        }
        if let Some(idx) = do_add_idx {
            let p = crate::ops::sdf::add_spawn_point(self);
            match idx {
                0 => crate::ops::sdf::sphere::run(self, p),
                1 => crate::ops::sdf::box_shape::run(self, p),
                2 => crate::ops::sdf::cylinder::run(self, p),
                3 => crate::ops::sdf::torus::run(self, p),
                4 => crate::ops::sdf::cone::run(self, p),
                5 => crate::ops::sdf::capsule::run(self, p),
                6 => crate::ops::sdf::stair::run(self, p),
                7 => crate::ops::sdf::spiral_stair::run(self, p),
                8 => crate::ops::sdf::polyline::run(self, p),
                9 => crate::ops::sdf::spine::run(self, p),
                _ => {}
            }
            self.ui.ctx_menu_open = false;
        }
        if do_convert_poly { self.convert_selected_poly_to_sdf(); self.ui.ctx_menu_open=false; }
        if do_link_poly { self.link_selected_poly_as_sdf(); self.ui.ctx_menu_open=false; }
        if do_poly_pivot_to_center {
            self.poly_pivot_to_center_mesh();
            self.ui.ctx_menu_open = false;
        }
        if do_poly_pivot_to_selected {
            self.poly_pivot_to_selected();
            self.ui.ctx_menu_open = false;
        }
        if do_poly_mesh_to_pivot {
            self.poly_mesh_to_pivot();
            self.ui.ctx_menu_open = false;
        }
        if do_poly_mesh_to_origin {
            self.poly_mesh_to_origin();
            self.ui.ctx_menu_open = false;
        }
        if do_select_all {
            // INTENTIONAL: additive select-all with visible+unlocked filter
            for p in &self.scene.primitives{if p.visible&&!p.locked{self.sel.usel.selected.insert(SceneItemId::Prim(p.prim_uid));}}
            if let Some(p)=self.scene.primitives.first(){self.sel.usel.active=Some(SceneItemId::Prim(p.prim_uid));}
            self.mark_dirty(DirtyFlags::RENDER); self.ui.ctx_menu_open=false;
        }
        if let Some(kind) = do_reset_xform {
            match kind {
                0 => self.reset_position(),
                1 => self.reset_rotation(),
                2 => self.reset_scale(),
                _ => self.reset_transform(),
            }
            ch = true;
            self.ui.ctx_menu_open=false;
        }
        if let Some(op) = new_op {
            if let Some(idx) = prim_idx {
                if idx < self.scene.primitives.len() {
                    // Shared path filters no-ops (same op re-picked).
                    let uid = self.scene.primitives[idx].prim_uid;
                    self.set_prims_boolean_op(&[(uid, op)]);
                    ch = true;
                }
            }
            self.ui.ctx_menu_open=false;
        }
        if let Some(bit) = do_mirror_toggle {
            if let Some(idx) = prim_idx {
                if idx < self.scene.primitives.len() {
                    self.transaction_snapshot_only("Toggle Mirror");
                    let new_flags = self.scene.primitives[idx].mirror_flags ^ bit;
                    self.write_prim(idx, |p| p.mirror_flags = new_flags);
                    ch = true;
                }
            }
            self.ui.ctx_menu_open=false;
        }
        if do_mirror_flip {
            if let Some(idx) = prim_idx {
                if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                    if let Some(node) = self.scene.scene_graph.get(body_nid) {
                        if let core_math::scene_graph::NodePayload::SdfBody { mirror_flags, mirror_center, .. } = &node.payload {
                            let (f, c) = (*mirror_flags, *mirror_center);
                            self.flip_body_mirror(body_nid, f, c);
                            ch = true;
                        }
                    }
                }
            }
            self.ui.ctx_menu_open=false;
        }
        if let Some(new_shape) = do_change_shape {
            if let Some(idx) = prim_idx {
                if idx < self.scene.primitives.len() {
                    self.transaction_snapshot_only("Change shape");
                    self.write_prim(idx, |p| p.shape = new_shape);
                    ch = true;
                }
            }
            self.ui.ctx_menu_open=false;
        }
        if do_hide {
            if let Some(idx) = prim_idx {
                if idx < self.scene.primitives.len() {
                    let v = !self.scene.primitives[idx].visible;
                    self.transaction(if v { "Show" } else { "Hide" }, |s| {
                        s.write_prim(idx, |p| p.visible = v);
                    });
                    ch = true;
                }
            }
            self.ui.ctx_menu_open=false;
        }
        if do_lock {
            if let Some(idx) = prim_idx {
                if idx < self.scene.primitives.len() {
                    let lk = !self.scene.primitives[idx].locked;
                    self.transaction(if lk { "Lock" } else { "Unlock" }, |s| {
                        s.write_prim(idx, |p| p.locked = lk);
                    });
                }
            }
            self.ui.ctx_menu_open=false;
        }
        // Deferred tool-context actions
        if let Some(infinite) = do_commit_create { crate::ops::create::session::commit(self, infinite); }
        if do_cancel_create { crate::ops::create::session::cancel(self); }
        if do_exit_bevel { self.set_mode(ToolMode::Select); self.toast("Bevel mode OFF"); }
        if do_clear_bevel_edges { self.tool.selected_bevel_edges.clear(); }
        if do_exit_extrude {
            self.set_mode(ToolMode::Select);
            self.tool.work_mode = WorkMode::Object;
            self.sel.edit_target = None;
            self.sel.selected_faces.clear();
            self.toast("Extrude mode OFF");
        }
        if do_back_to_object {
            self.tool.work_mode = WorkMode::Object;
            self.sel.edit_target = None;
            self.sel.selected_faces.clear();
        }
        if let Some(cmd) = do_poly_cmd {
            match cmd {
                "extrude" => self.push_cmd(AppCommand::PolyExtrude),
                "extrude_normals" => {
                    if self.poly_work_mode == PolyWorkMode::Face
                        && !self.poly_sel.selected_faces.is_empty()
                        && self.tool.current.is_idle()
                    {
                        let mouse = self.viewport.viewport_mouse;
                        self.poly_start_extrude_along_normals_drag(mouse);
                    }
                }
                "extrude_sweep" => {
                    if self.tool.current.is_idle() {
                        let mouse = self.viewport.viewport_mouse;
                        self.poly_start_extrude_sweep(mouse);
                    }
                }
                "inset" => self.push_cmd(AppCommand::PolyInset),
                "circle" => self.push_cmd(AppCommand::PolyCircle),
                "delete" => self.push_cmd(AppCommand::PolyDeleteFaces),
                "delete_only_faces" => self.poly_delete_faces_only(),
                "delete_edges" => self.poly_delete_edges(),
                "delete_verts" => self.poly_delete_vertices(),
                "subdivide" => self.push_cmd(AppCommand::PolySubdivide),
                "loop_cut" => self.push_cmd(AppCommand::PolyLoopCut),
                "edge_loop" => self.push_cmd(AppCommand::PolyEdgeLoop),
                "edge_ring" => self.push_cmd(AppCommand::PolyEdgeRing),
                "merge" => self.push_cmd(AppCommand::PolyMerge),
                "bridge" => self.push_cmd(AppCommand::PolyBridge),
                "poke" => self.push_cmd(AppCommand::PolyPoke),
                "connect" => self.push_cmd(AppCommand::PolyConnect),
                "tosphere" => self.push_cmd(AppCommand::PolyToSphere),
                "bevel" => self.push_cmd(AppCommand::PolyBevelEdge),
                "unbevel" => self.push_cmd(AppCommand::PolyUnbevel),
                "face_hinge" => self.push_cmd(AppCommand::PolyFaceHinge),
                "edge_extrude" => {
                    let mouse = self.viewport.viewport_mouse;
                    self.poly_start_edge_extrude_drag(mouse);
                }
                "knife" => self.push_cmd(AppCommand::PolyKnife),
                "solidify" => self.push_cmd(AppCommand::PolySolidify),
                "edge_slide" => self.poly_start_edge_slide(),
                "vert_slide" => self.poly_start_vert_slide(),
                "shrink_fatten" => {
                    let mouse = self.viewport.viewport_mouse;
                    self.poly_start_shrink_fatten(mouse);
                }
                "dissolve" => self.poly_dissolve(),
                "separate" => self.poly_separate_selection(),
                "flip_normals" => self.poly_flip_normals(),
                "uv_unwrap" => self.poly_auto_uv_unwrap(),
                "pack_uvs" => self.poly_pack_uvs(),
                "bake_mesh_maps" => {
                    self.poly_request_mesh_map_bake();
                }
                "uv_layout" => {
                    self.ui.show_uv_layout = !self.ui.show_uv_layout;
                    self.toast(if self.ui.show_uv_layout { "UV Layout ON" } else { "UV Layout OFF" });
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                "mark_sharp" => self.poly_set_sharp(true),
                "clear_sharp" => self.poly_set_sharp(false),
                "toggle_seam" => self.poly_mark_seam_apply(),
                "mark_seam" => self.poly_set_seam(true),
                "clear_seam" => self.poly_set_seam(false),
                "dice" => self.poly_activate_dice(),
                "shade_smooth" => self.poly_shade_smooth(),
                "shade_flat" => self.poly_shade_flat(),
                "fill" => self.poly_fill_face(),
                "rip" => self.poly_rip(),
                "rip_fill" => self.poly_rip_fill(),
                "loop_relax" => self.poly_loop_relax(),
                "loop_flatten" => self.poly_loop_flatten(),
                "loop_space" => self.poly_loop_space(),
                "select_all" => self.poly_select_all_toggle(),
                "invert_sel" => crate::ops::poly::invert_selection::run(self),
                _ => {}
            }
        }
        if do_deselect_all { self.deselect_all(); self.mark_dirty(DirtyFlags::RENDER); }
        if do_close { self.ui.ctx_menu_open = false; }
        if ch {
            // Shape-slider path only: episode on real change (drag + typed). Other `ch`
            // sources (reset/bool/mirror/hide) already own their own undo entries.
            if shape_edit && !self.has_active_episode() {
                self.begin_episode("Resize shape");
            }
            if let Some(idx) = prim_idx { self.mirror_prim_to_graph(idx); }
            self.rebake_no_undo();
        }
    }

    pub(crate) fn draw_add_popup(&mut self, ctx: &egui::Context) {
        if !self.ui.show_add_popup { return; }
        let sdf_sub = self.ui.add_popup_sdf_subbody;
        let mut close = false;
        let title = if sdf_sub { "Add SDF (sub-body)" } else { "Add" };
        // Tooltip layer: must sit above ui2 `.tool-options` (Order::Foreground) so
        // Shift+A is never covered by the bottom-center Box Tool / XYZ bar.
        let win_resp = egui::Window::new(title)
            .fixed_pos(self.ui.add_popup_pos)
            .auto_sized()
            .collapsible(false)
            .title_bar(false)
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.set_max_width(200.0);
                // Search filter
                let sr = ui.add(egui::TextEdit::singleline(&mut self.ui.add_search)
                    .hint_text("Search...")
                    .desired_width(180.0));
                if sr.changed() || self.ui.add_search_focus {
                    sr.request_focus();
                    self.ui.add_search_focus = false;
                }
                let filter = self.ui.add_search.to_lowercase();
                let f = |name: &str| -> bool { filter.is_empty() || name.to_lowercase().contains(&filter) };
                ui.separator();

                // Compact item helper
                let small = egui::FontId::proportional(12.0);
                let item = |ui: &mut egui::Ui, icon: &str, label: &str| -> bool {
                    if !f(label) { return false; }
                    let text = format!("{} {}", icon, label);
                    let btn = egui::Button::new(egui::RichText::new(text).font(small.clone()))
                        .frame(false);
                    ui.add(btn).clicked()
                };

                // Ctrl+Shift+A: SDF primitives only — created inside a sub-body of the selection.
                if sdf_sub {
                    ui.colored_label(
                        egui::Color32::from_rgb(80, 200, 140),
                        egui::RichText::new("SDF in sub-body").size(11.0),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Sub-body gets selected prim's boolean; that prim is first inside",
                        )
                        .small()
                        .weak(),
                    );
                    let mut pick: Option<SdfShape> = None;
                    if item(ui, "\u{25CB}", "Sphere") { pick = Some(Self::shape_preset(0)); }
                    if item(ui, "\u{25A1}", "Box") { pick = Some(Self::shape_preset(1)); }
                    if item(ui, "\u{25CE}", "Cylinder") { pick = Some(Self::shape_preset(2)); }
                    if item(ui, "\u{25D4}", "Torus") { pick = Some(Self::shape_preset(3)); }
                    if item(ui, "\u{25B3}", "Cone") { pick = Some(Self::shape_preset(4)); }
                    if item(ui, "\u{2282}", "Capsule") { pick = Some(Self::shape_preset(5)); }
                    if item(ui, "\u{1FA9C}", "Stair") { pick = Some(Self::shape_preset(6)); }
                    if item(ui, "\u{1F300}", "Spiral Stair") { pick = Some(Self::shape_preset(7)); }
                    if item(ui, "\u{2312}", "Polyline") { pick = Some(Self::curve_preset()); }
                    if item(ui, "\u{223F}", "Spine / Pipe") { pick = Some(Self::spine_preset()); }
                    if let Some(shape) = pick {
                        self.prepare_add_into_selected_sub_body();
                        self.add_primitive_immediate(shape, Vec3::ZERO);
                        close = true;
                    }
                    if item(ui, "\u{270E}", "Draw Polyline\u{2026}") {
                        self.prepare_add_into_selected_sub_body();
                        self.start_polyline_draw();
                        close = true;
                    }
                } else {
                // Contextual section order (Blender-ish Shift+A):
                //   poly mode / poly selected  → Polygon Meshes first
                //   SDF mode / SDF selected    → SDF Primitives first
                //   sculpt / voxel selected    → Voxel first
                //   NURBS mode / NURBS selected → NURBS first
                // Priority: geometry selection → Tools pillar (mode) → work modes →
                // modeling_context → active body → default SDF.
                #[derive(Clone, Copy, PartialEq)]
                enum AddSec { Voxel, Sdf, Poly, Nurbs, Lights }
                let primary = {
                    use core_math::scene_graph::NodePayload;
                    let sec_for_nid = |nid: core_math::scene_graph::NodeId| -> Option<AddSec> {
                        self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                            NodePayload::PolygonalBody { .. } | NodePayload::ImportedMesh { .. } => {
                                Some(AddSec::Poly)
                            }
                            NodePayload::SdfBody { .. } | NodePayload::SdfObject { .. } => {
                                Some(AddSec::Sdf)
                            }
                            NodePayload::NurbsBody { .. } => Some(AddSec::Nurbs),
                            NodePayload::VoxelBody { .. } => Some(AddSec::Voxel),
                            // Groups / empties / lights / cameras don't set add-menu family.
                            _ => None,
                        })
                    };

                    // 1) Active node, then any selected node (active may be Prim/Light).
                    let from_sel = self
                        .sel
                        .usel
                        .active_node()
                        .and_then(sec_for_nid)
                        .or_else(|| {
                            self.sel
                                .usel
                                .selected_nodes()
                                .find_map(sec_for_nid)
                        });
                    if let Some(sec) = from_sel {
                        sec
                    } else if let Some(sec) = match self.ui2.active_pillar.as_str() {
                        // 2) Tools chrome tab — what the user sees as "poly mode" / "sdf mode".
                        "poly" => Some(AddSec::Poly),
                        "sdf" => Some(AddSec::Sdf),
                        "sculpt" => Some(AddSec::Voxel),
                        "nurbs" => Some(AddSec::Nurbs),
                        _ => None,
                    } {
                        sec
                    } else if self.poly_work_mode != PolyWorkMode::Object {
                        // 3) In poly component edit even if pillar string is stale.
                        AddSec::Poly
                    } else if self.tool.work_mode == WorkMode::Sculpt {
                        AddSec::Voxel
                    } else {
                        // 4) modeling_context from selection/body, then active body payload.
                        match self.modeling_context {
                            ModelingContext::Polygonal | ModelingContext::ImportedMesh => {
                                AddSec::Poly
                            }
                            ModelingContext::Nurbs => AddSec::Nurbs,
                            ModelingContext::Sdf => AddSec::Sdf,
                            ModelingContext::None
                            | ModelingContext::Light
                            | ModelingContext::Camera => sec_for_nid(self.scene.active_body_nid)
                                .unwrap_or(AddSec::Sdf),
                        }
                    }
                };
                // Primary family first, then the rest in a stable order (skipping primary).
                let rest = [AddSec::Sdf, AddSec::Poly, AddSec::Voxel, AddSec::Nurbs, AddSec::Lights];
                let mut order = vec![primary];
                for s in rest {
                    if s != primary {
                        order.push(s);
                    }
                }

                // Section metadata: bit for user-open mask, title, colour, search keywords.
                let sec_bit = |s: AddSec| -> u8 {
                    match s {
                        AddSec::Voxel => 1,
                        AddSec::Sdf => 2,
                        AddSec::Poly => 4,
                        AddSec::Nurbs => 8,
                        AddSec::Lights => 16,
                    }
                };
                let sec_meta = |s: AddSec| -> (&'static str, egui::Color32, &'static [&'static str]) {
                    match s {
                        AddSec::Voxel => (
                            "Voxel Sculpt",
                            egui::Color32::from_rgb(120, 200, 255),
                            &["voxel", "sculpt", "clay"],
                        ),
                        AddSec::Sdf => (
                            "SDF Primitives",
                            egui::Color32::from_rgb(80, 200, 140),
                            &[
                                "sphere", "box", "cylinder", "torus", "cone", "capsule",
                                "polyline", "curve", "spine", "stair", "spiral", "sdf",
                                "terrain", "island", "mesh terrain",
                            ],
                        ),
                        AddSec::Poly => (
                            "Polygon Meshes",
                            egui::Color32::from_rgb(100, 200, 255),
                            &[
                                "poly", "mesh", "box", "plane", "sphere", "cylinder",
                                "torus", "cone", "circle", "ico",
                            ],
                        ),
                        AddSec::Nurbs => (
                            "NURBS Surfaces",
                            egui::Color32::from_rgb(200, 140, 255),
                            &[
                                "nurbs", "cube", "box", "plane", "cylinder", "sphere", "torus",
                                "revolve", "loft",
                            ],
                        ),
                        AddSec::Lights => (
                            "Lights",
                            egui::Color32::from_rgb(255, 200, 60),
                            &["light", "directional", "point", "spot", "sun"],
                        ),
                    }
                };

                // Workflow: only the primary group is open by default. Secondary groups are
                // collapsed headers — click ▶ to expand, or type in Search to reveal matches.
                // Owner: "in edit mode other primitive groups should be collapsed so the ones
                // exposed are the ones the user is using most of the time."
                let searching = !filter.is_empty();

                for sec in order {
                    let (title, color, keys) = sec_meta(sec);
                    let kw_hit = searching && keys.iter().any(|s| filter.contains(s));
                    // Also open a secondary section if the typed query matches an item label
                    // inside it (e.g. "poly box" while SDF is primary).
                    let label_hit = searching && match sec {
                        AddSec::Voxel => ["voxel sphere", "voxel box", "voxel cylinder"]
                            .iter().any(|n| f(n)),
                        AddSec::Sdf => [
                            "sphere", "box", "cylinder", "torus", "cone", "capsule", "stair",
                            "spiral stair", "polyline", "draw polyline", "spine / pipe",
                            "terrain island", "terrain patch", "terrain stream",
                            "expand terrain", "demo tunnel carve", "rebuild terrain mesh",
                            "5x5", "3x3",
                        ]
                        .iter()
                        .any(|n| f(n)),
                        AddSec::Poly => [
                            "poly box", "poly plane", "poly circle", "poly sphere",
                            "poly ico sphere", "poly cylinder", "poly torus", "poly cone",
                        ]
                        .iter()
                        .any(|n| f(n)),
                        AddSec::Nurbs => [
                            "nurbs cube", "nurbs plane", "nurbs cylinder", "nurbs sphere",
                            "nurbs torus", "nurbs revolve", "nurbs loft",
                        ]
                        .iter()
                        .any(|n| f(n)),
                        AddSec::Lights => ["directional", "point", "spot"].iter().any(|n| f(n)),
                    };
                    let bit = sec_bit(sec);
                    let is_primary = sec == primary;
                    let user_open = (self.ui.add_sec_user_open & bit) != 0;
                    // Primary always expanded when not searching; while searching, only
                    // sections with matches. Secondary: user toggle OR search hit.
                    let open = if searching {
                        kw_hit || label_hit || (is_primary && filter.is_empty())
                    } else {
                        is_primary || user_open
                    };

                    // Section header — primary is a static label; secondary is clickable.
                    if !searching || open || kw_hit || label_hit || is_primary {
                        if is_primary && !searching {
                            ui.colored_label(color, egui::RichText::new(title).size(11.0).strong());
                        } else {
                            let caret = if open { "\u{25BC}" } else { "\u{25B6}" }; // ▼ / ▶
                            let head = format!("{caret}  {title}");
                            let resp = ui.add(
                                egui::Label::new(egui::RichText::new(head).size(11.0).color(color))
                                    .sense(egui::Sense::click()),
                            );
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if resp.clicked() && !searching {
                                self.ui.add_sec_user_open ^= bit;
                            }
                            if !open {
                                // Collapsed: skip body (still show header so user can expand).
                                continue;
                            }
                        }
                    } else {
                        continue;
                    }

                    if !open {
                        continue;
                    }

                    match sec {
                        AddSec::Voxel => {
                            if item(ui, "\u{1F9CA}", "Voxel Sphere") {
                                self.new_voxel_sphere();
                                close = true;
                            }
                            if item(ui, "\u{25A0}", "Voxel Box") {
                                self.new_voxel_box();
                                close = true;
                            }
                            if item(ui, "\u{25CE}", "Voxel Cylinder") {
                                self.new_voxel_cylinder();
                                close = true;
                            }
                        }
                        AddSec::Sdf => {
                            let p = crate::ops::sdf::add_spawn_point(self);
                            // Mesh Terrain: Shift+A only creates an island. Patch size, stream,
                            // expand, rebuild, export live in Tool Properties (tile selected).
                            if item(ui, "\u{26F0}", "Terrain") {
                                crate::ops::sdf::terrain_island::run(self);
                                close = true;
                            }
                            if item(ui, "\u{25CB}", "Sphere") {
                                crate::ops::sdf::sphere::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25A1}", "Box") {
                                crate::ops::sdf::box_shape::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25CE}", "Cylinder") {
                                crate::ops::sdf::cylinder::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25D4}", "Torus") {
                                crate::ops::sdf::torus::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25B3}", "Cone") {
                                crate::ops::sdf::cone::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{2282}", "Capsule") {
                                crate::ops::sdf::capsule::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{1FA9C}", "Stair") {
                                crate::ops::sdf::stair::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{1F300}", "Spiral Stair") {
                                crate::ops::sdf::spiral_stair::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{2312}", "Polyline") {
                                crate::ops::sdf::polyline::run(self, p);
                                close = true;
                            }
                            if item(ui, "\u{270E}", "Draw Polyline\u{2026}") {
                                crate::ops::sdf::draw_polyline::run(self);
                                close = true;
                            }
                            if item(ui, "\u{223F}", "Spine / Pipe") {
                                crate::ops::sdf::spine::run(self, p);
                                close = true;
                            }
                        }
                        AddSec::Poly => {
                            let p = crate::ops::sdf::add_spawn_point(self);
                            if item(ui, "\u{25A1}", "Poly Box") {
                                crate::ops::poly::mesh_create::box_mesh(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25AD}", "Poly Plane") {
                                crate::ops::poly::mesh_create::plane(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25CB}", "Poly Circle") {
                                crate::ops::poly::mesh_create::circle(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25CB}", "Poly Sphere") {
                                crate::ops::poly::mesh_create::sphere(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25C7}", "Poly Ico Sphere") {
                                crate::ops::poly::mesh_create::ico_sphere(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25CE}", "Poly Cylinder") {
                                crate::ops::poly::mesh_create::cylinder(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25D4}", "Poly Torus") {
                                crate::ops::poly::mesh_create::torus(self, p);
                                close = true;
                            }
                            if item(ui, "\u{25B3}", "Poly Cone") {
                                crate::ops::poly::mesh_create::cone(self, p);
                                close = true;
                            }
                        }
                        AddSec::Nurbs => {
                            if item(ui, "\u{25A1}", "NURBS Cube") {
                                self.create_nurbs_surface("cube");
                                close = true;
                            }
                            if item(ui, "\u{25AD}", "NURBS Plane") {
                                self.create_nurbs_surface("plane");
                                close = true;
                            }
                            if item(ui, "\u{25CE}", "NURBS Cylinder") {
                                self.create_nurbs_surface("cylinder");
                                close = true;
                            }
                            if item(ui, "\u{25CB}", "NURBS Sphere") {
                                self.create_nurbs_surface("sphere");
                                close = true;
                            }
                            if item(ui, "\u{25D4}", "NURBS Torus") {
                                self.create_nurbs_surface("torus");
                                close = true;
                            }
                            if item(ui, "\u{21BA}", "NURBS Revolve") {
                                self.create_nurbs_surface("revolve");
                                close = true;
                            }
                            if item(ui, "\u{2195}", "NURBS Loft") {
                                self.create_nurbs_surface("loft");
                                close = true;
                            }
                        }
                        AddSec::Lights => {
                            if item(ui, "\u{2600}", "Directional") {
                                self.add_light(LightType::Directional);
                                close = true;
                            }
                            if item(ui, "\u{25CB}", "Point") {
                                self.add_light(LightType::Point);
                                close = true;
                            }
                            if item(ui, "\u{25D3}", "Spot") {
                                self.add_light(LightType::Spot);
                                close = true;
                            }
                        }
                    }
                    ui.separator();
                }
                } // end full menu
            });
        if close {
            self.ui.show_add_popup = false;
            self.ui.add_popup_sdf_subbody = false;
            self.ui.add_search.clear();
            self.ui.add_search_focus = false;
            self.ui.add_sec_user_open = 0;
        }
        // Close on click outside or Esc
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.ui.show_add_popup = false;
            self.ui.add_popup_sdf_subbody = false;
            self.ui.add_search.clear();
            self.ui.add_sec_user_open = 0;
        }
        if ctx.input(|i| i.pointer.any_pressed()) && !close {
            let pos = ctx.input(|i| i.pointer.interact_pos().unwrap_or(egui::Pos2::ZERO));
            // Use actual window rect from egui instead of hardcoded estimate —
            // the old 400px height was shorter than the full popup (~550px),
            // causing clicks on the Lights section to be treated as "outside".
            let win_rect = win_resp
                .and_then(|r| Some(r.response.rect))
                .unwrap_or(egui::Rect::from_min_size(self.ui.add_popup_pos, egui::vec2(180.0, 600.0)));
            if !win_rect.contains(pos) {
                self.ui.show_add_popup = false;
                self.ui.add_popup_sdf_subbody = false;
                self.ui.add_search.clear();
                self.ui.add_sec_user_open = 0;
            }
        }
    }

}
