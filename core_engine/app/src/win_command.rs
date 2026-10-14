// win_command.rs — Command palette, execute_command_by_id, macro manager
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;

impl AzraApp {
    // ================================================================
    // Command Palette (Phase 2)
    // ================================================================
    pub(crate) fn draw_command_palette(&mut self, ctx: &egui::Context) {
        if !self.ui.command_palette.open { return; }

        let mut close = false;
        let mut execute_id: Option<&'static str> = None;

        // Semi-transparent backdrop overlay — makes the palette unmissable
        // egui 0.32 removed `Order::PanelResizeLine` (between Middle and Foreground).
        // The backdrop now shares Foreground with the palette; `set_sublayer` below
        // pins the palette directly above it so the backdrop can never cover it.
        let backdrop_layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("cmd_palette_backdrop"));
        egui::Area::new(backdrop_layer.id)
            .order(backdrop_layer.order)
            .fixed_pos(egui::pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                ui.painter().rect_filled(screen, 0.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120));
                let r = ui.allocate_rect(screen, egui::Sense::click());
                if r.clicked() { close = true; }
            });

        ctx.set_sublayer(backdrop_layer, egui::LayerId::new(egui::Order::Foreground, egui::Id::new("command_palette")));
        egui::Area::new(egui::Id::new("command_palette"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(8.0)
                    .stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 140, 220)))
                    .show(ui, |ui| {
                        ui.set_width(480.0);

                        // Search input
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.ui.command_palette.search)
                                .hint_text("Type a command...")
                                .desired_width(464.0)
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                        r.request_focus();

                        ui.add_space(4.0);

                        // Filter commands
                        let search_lower = self.ui.command_palette.search.to_lowercase();
                        let filtered: Vec<&crate::CommandDef> = COMMAND_DEFS.iter()
                            .filter(|c| {
                                if search_lower.is_empty() { return true; }
                                c.name.to_lowercase().contains(&search_lower)
                                    || c.category.to_lowercase().contains(&search_lower)
                            })
                            .collect();

                        // Clamp selected index
                        if self.ui.command_palette.selected_idx >= filtered.len() {
                            self.ui.command_palette.selected_idx = 0;
                        }

                        // Arrow key navigation
                        let (up, down, enter) = ui.input(|i| (
                            i.key_pressed(egui::Key::ArrowUp),
                            i.key_pressed(egui::Key::ArrowDown),
                            i.key_pressed(egui::Key::Enter),
                        ));
                        if up && self.ui.command_palette.selected_idx > 0 {
                            self.ui.command_palette.selected_idx -= 1;
                        }
                        if down && self.ui.command_palette.selected_idx + 1 < filtered.len() {
                            self.ui.command_palette.selected_idx += 1;
                        }
                        if enter && !filtered.is_empty() {
                            execute_id = Some(filtered[self.ui.command_palette.selected_idx].id);
                            close = true;
                        }

                        // Command list (clickable rows)
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            for (idx, cmd) in filtered.iter().enumerate() {
                                let selected = idx == self.ui.command_palette.selected_idx;
                                let shortcut_label = cmd.shortcut_act
                                    .map(|act| self.keybindings.label(act))
                                    .unwrap_or_default();

                                let cat_color = match cmd.category {
                                    "Poly" => egui::Color32::from_rgb(100, 200, 100),
                                    "NURBS" => egui::Color32::from_rgb(100, 150, 255),
                                    "File" => egui::Color32::from_rgb(200, 200, 100),
                                    "Edit" => egui::Color32::from_rgb(200, 150, 100),
                                    "Transform" => egui::Color32::from_rgb(150, 200, 255),
                                    "View" => egui::Color32::from_rgb(180, 180, 180),
                                    "Tools" => egui::Color32::from_rgb(255, 180, 100),
                                    "UI" => egui::Color32::from_rgb(180, 130, 255),
                                    "Mode" => egui::Color32::from_rgb(255, 130, 180),
                                    _ => egui::Color32::GRAY,
                                };

                                // Build rich label text for selectable row
                                let row_text = if shortcut_label.is_empty() {
                                    format!("{:<10} {}", cmd.category, cmd.name)
                                } else {
                                    format!("{:<10} {:<30} {}", cmd.category, cmd.name, shortcut_label)
                                };

                                let resp = ui.selectable_label(selected, &row_text);
                                // Paint category color over the category portion
                                let text_rect = resp.rect;
                                let cat_rect = egui::Rect::from_min_size(
                                    text_rect.min,
                                    egui::vec2(60.0, text_rect.height()),
                                );
                                ui.painter().text(
                                    cat_rect.left_center(),
                                    egui::Align2::LEFT_CENTER,
                                    cmd.category,
                                    egui::FontId::proportional(11.0),
                                    cat_color,
                                );

                                if resp.clicked() {
                                    execute_id = Some(cmd.id);
                                    close = true;
                                }
                                if resp.hovered() {
                                    self.ui.command_palette.selected_idx = idx;
                                }
                            }
                        });

                        ui.add_space(4.0);
                        ui.colored_label(egui::Color32::from_rgb(120, 120, 120),
                            egui::RichText::new("Esc to close | Arrow keys to navigate | Enter to execute").small());
                    });
            });

        if close {
            self.ui.command_palette.open = false;
            self.ui.command_palette.search.clear();
            self.ui.command_palette.selected_idx = 0;
        }

        if let Some(id) = execute_id {
            self.execute_command_by_id(id);
        }
    }

    /// Execute a command by its CommandDef id string.
    pub(crate) fn execute_command_by_id(&mut self, id: &str) {
        let cmd = match id {
            // File
            "cmd_new" => { self.new_scene(); return; }
            "cmd_open" => { self.open_scene(); return; }
            "cmd_save" => { self.save_scene(); return; }
            "cmd_saveas" => { self.save_scene_as(); return; }
            "cmd_export" => { self.ui.show_export_window = true; return; }
            // Edit
            "cmd_undo" => Some(AppCommand::Undo),
            "cmd_redo" => Some(AppCommand::Redo),
            "cmd_delete" => Some(AppCommand::DeleteSelected),
            "cmd_dup" => Some(AppCommand::DuplicateSelected),
            // Immediate (not queued): same path as Ctrl+I / outliner — no deferred-frame lag.
            "cmd_inst" => { self.create_instance(); return; }
            "cmd_inst_all_bodies" => { self.instance_into_all_bodies(); return; }
            "cmd_inst_break" => { self.make_selected_instances_unique(); return; }
            "cmd_selall" => Some(AppCommand::SelectAll),
            // Transform
            "cmd_grab" | "cmd_rotate" | "cmd_scale" | "cmd_focus" | "cmd_space" => {
                // These are modal operations needing specific state; toast as hint
                self.toast(&format!("Use keyboard shortcut for {}", id));
                return;
            }
            // Poly
            "cmd_poly_extrude" => Some(AppCommand::PolyExtrude),
            "cmd_poly_inset" => Some(AppCommand::PolyInset),
            "cmd_poly_circle" => Some(AppCommand::PolyCircle),
            "cmd_poly_loop_cut" => Some(AppCommand::PolyLoopCut),
            "cmd_poly_merge" => Some(AppCommand::PolyMerge),
            "cmd_poly_knife" => Some(AppCommand::PolyKnife),
            "cmd_poly_bevel" => Some(AppCommand::PolyBevelEdge),
            "cmd_poly_unbevel" => Some(AppCommand::PolyUnbevel),
            "cmd_poly_face_hinge" => Some(AppCommand::PolyFaceHinge),
            "cmd_poly_delete" => Some(AppCommand::PolyDeleteFaces),
            "cmd_poly_subdivide" => Some(AppCommand::PolySubdivide),
            "cmd_poly_dice" => Some(AppCommand::PolyDice),
            "cmd_poly_edge_loop" => Some(AppCommand::PolyEdgeLoop),
            "cmd_poly_edge_ring" => Some(AppCommand::PolyEdgeRing),
            // NURBS
            "cmd_nurbs_knot_u" => Some(AppCommand::NurbsKnotU),
            "cmd_nurbs_knot_v" => Some(AppCommand::NurbsKnotV),
            "cmd_nurbs_weight_up" => Some(AppCommand::NurbsWeightUp),
            "cmd_nurbs_weight_down" => Some(AppCommand::NurbsWeightDown),
            "cmd_nurbs_grab_cv" => Some(AppCommand::NurbsGrabCv),
            // UI toggles
            "cmd_show_tweak" => Some(AppCommand::ToggleWindow("tweak")),
            "cmd_show_shortcuts" => Some(AppCommand::ToggleWindow("shortcuts")),
            "cmd_show_export" => Some(AppCommand::ToggleWindow("export")),
            "cmd_show_grid" => Some(AppCommand::ToggleWindow("grid")),
            "cmd_show_layout" => Some(AppCommand::ToggleWindow("layout")),
            "cmd_dump_ui_layout" => {
                // Needs egui Context for screen rect — deferred via flag consumed in app_update.
                self.ui.request_ui_layout_dump = true;
                return;
            }
            "cmd_toggle_toolpanel" => { self.layout.show_tool_panel = !self.layout.show_tool_panel; self.layout.save(); return; }
            "cmd_toggle_props" => { self.layout.show_properties_panel = !self.layout.show_properties_panel; self.layout.save(); return; }
            "cmd_toggle_header" => { self.layout.show_viewport_header = !self.layout.show_viewport_header; self.layout.save(); return; }
            // Mode
            "cmd_mode_obj" => Some(AppCommand::SetWorkMode(WorkMode::Object)),
            "cmd_mode_sculpt" => Some(AppCommand::SetWorkMode(WorkMode::Sculpt)),
            "cmd_surface_sculpt" => {
                self.begin_surface_sculpt_on_selection();
                return;
            }
            "cmd_mode_face" => Some(AppCommand::SetWorkMode(WorkMode::Face)),
            "cmd_palette" => Some(AppCommand::OpenCommandPalette),
            // Boolean ops via palette
            "cmd_set_op_union" => Some(AppCommand::SetCreateOp(CreateOp::Union)),
            "cmd_set_op_subtract" => Some(AppCommand::SetCreateOp(CreateOp::Subtract)),
            "cmd_set_op_intersect" => Some(AppCommand::SetCreateOp(CreateOp::Intersect)),
            // Macro commands
            "cmd_macro_toggle" => { self.toggle_macro_recording(); return; }
            "cmd_macro_play" => { self.play_macro(); return; }
            "cmd_macro_save" => { self.save_macro_dialog(); return; }
            "cmd_macro_load" => { self.load_macro_dialog(); return; }
            // View
            "cmd_vfront" | "cmd_vright" | "cmd_vtop" | "cmd_ortho" | "cmd_wpplace" => {
                self.toast(&format!("Use keyboard shortcut for {}", id));
                return;
            }
            // Cancel / deselect (was falsely labeled Agent Command Bar — ACT_CMD_BAR)
            "cmd_cmdbar" | "cmd_cancel_deselect" => {
                if self.drop_tool() {
                    // dropped create/modal
                } else {
                    self.deselect_all();
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                return;
            }
            "cmd_layer" => {
                crate::ops::edit::group_layer::run(self);
                return;
            }
            "cmd_inst" => {
                crate::ops::instance::create::run(self);
                return;
            }
            _ => None,
        };
        if let Some(c) = cmd {
            self.push_cmd(c);
        }
    }

    // ================================================================
    // Macro Manager Window
    // ================================================================
    pub(crate) fn draw_macro_manager(&mut self, ctx: &egui::Context) {
        if !self.ui.show_macro_manager { return; }
        let mut open = self.ui.show_macro_manager;
        egui::Window::new("Macro Manager")
            .default_size([300.0, 260.0])
            .open(&mut open)
            .show(ctx, |ui| {
                // Status
                ui.horizontal(|ui| {
                    if self.macro_recording {
                        let t = ui.input(|i| i.time);
                        let blink = (t * 2.5).sin() > 0.0;
                        let col = if blink { egui::Color32::RED } else { egui::Color32::DARK_RED };
                        ui.colored_label(col, "\u{23FA} RECORDING");
                        ctx.request_repaint();
                    } else {
                        ui.label("\u{23F9} Stopped");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!("{} commands", self.macro_buffer.len()));
                    });
                });
                ui.separator();

                // Controls
                ui.horizontal(|ui| {
                    let rec_label = if self.macro_recording { "\u{23F9} Stop" } else { "\u{23FA} Record" };
                    if ui.button(rec_label).clicked() {
                        self.toggle_macro_recording();
                    }
                    if ui.add_enabled(!self.macro_buffer.is_empty() && !self.macro_recording,
                        egui::Button::new("\u{25B6} Play")).clicked() {
                        self.play_macro();
                    }
                    if ui.add_enabled(!self.macro_buffer.is_empty(),
                        egui::Button::new("\u{1F5D1} Clear")).clicked() {
                        self.macro_buffer.clear();
                        self.toast("Macro cleared");
                    }
                });
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.macro_buffer.is_empty(),
                        egui::Button::new("\u{1F4BE} Save...")).clicked() {
                        self.save_macro_dialog();
                    }
                    if ui.button("\u{1F4C2} Load...").clicked() {
                        self.load_macro_dialog();
                    }
                });
                ui.separator();

                // Command list
                if self.macro_buffer.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new("No commands recorded").weak());
                    });
                } else {
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        for (i, cmd) in self.macro_buffer.iter().enumerate() {
                            let label = match cmd {
                                AppCommand::DeleteSelected => "Delete Selected".into(),
                                AppCommand::DuplicateSelected => "Duplicate Selected".into(),
                                AppCommand::SelectSingle(idx) => format!("Select [{}]", idx),
                                AppCommand::DeselectAll => "Deselect All".into(),
                                AppCommand::SelectAll => "Select All".into(),
                                AppCommand::SetBooleanOp(idx, _) => format!("Set Boolean [{}]", idx),
                                AppCommand::SetVisible(idx, v) => format!("Visible [{}] = {}", idx, v),
                                AppCommand::SetLocked(idx, l) => format!("Locked [{}] = {}", idx, l),
                                AppCommand::SetCreateOp(op) => format!("Create Op = {:?}", op),
                                AppCommand::SetPosition(idx, p) => format!("Pos [{}] = ({:.2},{:.2},{:.2})", idx, p.x, p.y, p.z),
                                AppCommand::SetRotation(idx, r) => format!("Rot [{}] = ({:.1},{:.1},{:.1})", idx, r.x, r.y, r.z),
                                AppCommand::SetScale(idx, s) => format!("Scale [{}] = ({:.2},{:.2},{:.2})", idx, s.x, s.y, s.z),
                                AppCommand::Toast(msg) => format!("Toast: {}", msg),
                                _ => "(other command)".into(),
                            };
                            ui.label(format!("{}. {}", i + 1, label));
                        }
                    });
                }
            });
        self.ui.show_macro_manager = open;
    }
}
