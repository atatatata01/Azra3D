//! Viewport header bar — moved from viewport.rs.

use crate::ui_menus::DefaultMenu;
use eframe::egui;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_viewport_header(&mut self, ctx: &egui::Context) {
        if !self.layout.show_viewport_header { return; }
        egui::TopBottomPanel::top("viewport_header").exact_height(self.layout.header_height).show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                // ---- Workspace tabs ----
                let ws_count = self.workspace_mgr.workspaces.len();
                let active_ws = self.workspace_mgr.active_idx;
                for idx in 0..ws_count {
                    let name = self.workspace_mgr.workspaces[idx].name.clone();
                    let is_active = idx == active_ws;
                    let col = if is_active { egui::Color32::from_rgb(100, 200, 255) }
                              else { egui::Color32::from_gray(120) };
                    let resp = ui.add(egui::Button::new(
                        egui::RichText::new(&name).small().color(col))
                        .frame(false));
                    if resp.clicked() {
                        self.workspace_mgr.active_idx = idx;
                        self.workspace_mgr.save();
                    }
                    if resp.secondary_clicked() {
                        self.ui.ws_context_menu = Some(idx);
                    }
                }
                if ui.small_button("+").on_hover_text("New Workspace").clicked() {
                    let mut new_ws = self.workspace_mgr.active().clone();
                    new_ws.name = format!("Custom {}", ws_count + 1);
                    new_ws.builtin = false;
                    self.workspace_mgr.workspaces.push(new_ws);
                    self.workspace_mgr.active_idx = self.workspace_mgr.workspaces.len() - 1;
                    self.workspace_mgr.save();
                }
                // Edit mode toggle
                let edit_lbl = if self.workspace_mgr.edit_mode { "Done" } else { "Edit" };
                let edit_col = if self.workspace_mgr.edit_mode { egui::Color32::from_rgb(255, 100, 100) } else { egui::Color32::from_gray(110) };
                if ui.add(egui::Button::new(egui::RichText::new(edit_lbl).small().color(edit_col)).frame(false))
                    .on_hover_text("Customize panels (ZBrush-style)").clicked() {
                    self.workspace_mgr.edit_mode = !self.workspace_mgr.edit_mode;
                    if !self.workspace_mgr.edit_mode { self.workspace_mgr.save(); }
                }
                ui.separator();
                let (mlbl,mcol) = match self.modeling_context {
                    ModelingContext::Polygonal => {
                        let sub = match self.poly_work_mode {
                            PolyWorkMode::Object => "Object",
                            PolyWorkMode::Vertex => "Vertex",
                            PolyWorkMode::Edge => "Edge",
                            PolyWorkMode::Face => "Face",
                            PolyWorkMode::Group => "Group",
                        };
                        (format!("\u{25B3} Poly: {}", sub), egui::Color32::from_rgb(100, 200, 255))
                    }
                    ModelingContext::Nurbs => {
                        let sub = match self.nurbs_work_mode {
                            NurbsWorkMode::Object => "Object",
                            NurbsWorkMode::Vertex => "Vertex",
                            NurbsWorkMode::Edge => "Edge",
                            NurbsWorkMode::Face => "Face",
                        };
                        (format!("\u{1F4D0} NURBS: {}", sub), egui::Color32::from_rgb(200, 140, 255))
                    }
                    ModelingContext::ImportedMesh => ("\u{1F4E6} Imported".into(), egui::Color32::from_rgb(180, 180, 180)),
                    _ => {
                        match self.tool.work_mode {
                            WorkMode::Object => ("\u{1F537} Object".into(), egui::Color32::from_rgb(80,200,140)),
                            WorkMode::Sculpt => ("\u{1F58C} Sculpt".into(),  egui::Color32::from_rgb(200,140,80)),
                            WorkMode::Vertex => ("\u{25CF} Vertex".into(),  egui::Color32::from_rgb(80,200,140)),
                            WorkMode::Edge   => ("\u{2500} Edge".into(),    egui::Color32::from_rgb(80,200,140)),
                            WorkMode::Face   => ("\u{25A0} Face".into(),    egui::Color32::from_rgb(80,200,140)),
                        }
                    }
                };
                ui.colored_label(mcol, mlbl);
                // Hidden vertex count indicator (poly edit mode)
                if self.modeling_context == ModelingContext::Polygonal
                    && !self.poly_sel.hidden_vertices.is_empty()
                {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 180, 80),
                        format!("{} hidden", self.poly_sel.hidden_vertices.len()),
                    );
                }
                // Viewport render mode badge
                {
                    let (rm_lbl, rm_col) = match self.render.viewport_mode {
                        ViewportRenderMode::FullSdf => ("SDF", egui::Color32::from_rgb(100, 180, 255)),
                        ViewportRenderMode::Hybrid  => ("Hybrid", egui::Color32::from_rgb(255, 200, 80)),
                    };
                    ui.colored_label(rm_col, rm_lbl);
                }
                // Edit mode info
                if let Some(target) = self.sel.edit_target {
                    let name = self.scene.primitives.get(target).map(|p| p.name.as_str()).unwrap_or("?");
                    let n = self.sel.selected_faces.len();
                    let info = if n > 0 { format!("Edit: {} — {} face(s)", name, n) }
                    else { format!("Edit: {}", name) };
                    ui.colored_label(egui::Color32::from_rgb(100, 180, 255), info);
                }
                ui.separator();
                for (lbl, mode) in [("PBR",ShadingMode::Pbr),("MatCap",ShadingMode::MatCap),("Lit",ShadingMode::Lit),("Wire",ShadingMode::Wireframe),("Solid",ShadingMode::SolidHdr),("RT",ShadingMode::Raytrace),("PT",ShadingMode::PathTrace)] {
                    let resp = ui.selectable_label(self.render.shading_mode==mode,lbl);
                    if resp.clicked(){
                        self.render.shading_mode=mode;
                        if mode==ShadingMode::PathTrace{self.render.pt_accumulated_frames=0;}
                        // Dither stays off by default for texturing (banding fix is optional).
                        // Only force OFF for modes where dither is pure noise (MatCap/Solid/Lit).
                        match mode {
                            ShadingMode::MatCap | ShadingMode::Lit
                            | ShadingMode::SolidHdr | ShadingMode::Wireframe => self.render.dither_enabled = false,
                            ShadingMode::Pbr | ShadingMode::Raytrace => {} // keep user preference
                            _ => {}
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if resp.secondary_clicked() {
                        self.render.shading_mode = mode;
                        match mode {
                            ShadingMode::MatCap | ShadingMode::Lit
                            | ShadingMode::SolidHdr | ShadingMode::Wireframe => self.render.dither_enabled = false,
                            ShadingMode::Pbr | ShadingMode::Raytrace => {} // keep preference; default off
                            _ => {}
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    let popup_id = ui.id().with(format!("shading_popup_{:?}", mode));
                    if resp.secondary_clicked() {
                        ui.memory_mut(|m| m.toggle_popup(popup_id));
                    }
                    egui::popup_below_widget(ui, popup_id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui: &mut egui::Ui| {
                        ui.set_min_width(220.0);
                        match mode {
                            ShadingMode::Pbr => {
                                ui.label("PBR Settings");
                                if self.env_hdr_width > 0 {
                                    ui.separator();
                                    if ui.add(egui::Slider::new(&mut self.render.exposure, 0.01..=5.0).text("Exposure").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                    ui.separator();
                                    ui.label("Environment IBL");
                                    if ui.add(egui::Slider::new(&mut self.render.env_brightness, 0.0..=5.0).text("Brightness").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                    if ui.add(egui::Slider::new(&mut self.render.env_contrast, 0.1..=3.0).text("Contrast").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                    if ui.add(egui::Slider::new(&mut self.render.env_saturation, 0.0..=2.0).text("Saturation").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                    if ui.add(egui::Slider::new(&mut self.render.env_blur, 0.0..=1.0).text("Background blur").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                    if ui.add(egui::Slider::new(&mut self.render.env_reflection_blur, 0.0..=1.0).text("Reflection blur").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                    // Hides the HDRI IMAGE only — IBL fill + metal reflections keep it.
                                    if ui.checkbox(&mut self.render.env_bg_visible, "Show HDRI in background")
.default_menu("render.env_bg_visible", serde_json::json!(self.render.env_bg_visible)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                }
                                ui.separator();
                                ui.label("Post FX");
                                if ui.checkbox(&mut self.render.bloom_enabled, "Bloom")
.default_menu("render.bloom_enabled", serde_json::json!(self.render.bloom_enabled)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                if self.render.bloom_enabled {
                                    if ui.add(egui::Slider::new(&mut self.render.bloom_threshold, 0.1..=3.0).text("Threshold")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                    if ui.add(egui::Slider::new(&mut self.render.bloom_intensity, 0.0..=2.0).text("Intensity")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                }
                                if ui.checkbox(&mut self.render.ssr_enabled, "SSR")
.default_menu("render.ssr_enabled", serde_json::json!(self.render.ssr_enabled)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                if ui.checkbox(&mut self.render.fog_enabled, "Fog")
.default_menu("render.fog_enabled", serde_json::json!(self.render.fog_enabled)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                // Background color removed — always uses HDR environment
                            }
                            ShadingMode::MatCap => {
                                ui.label("MatCap Settings");
                                ui.separator();
                                // Flat BG color (default = UI panel dark gray 0x1c1c1c)
                                {
                                    let mut col32 = egui::Color32::from_rgb(
                                        (self.render.matcap_bg_color[0]*255.0) as u8,
                                        (self.render.matcap_bg_color[1]*255.0) as u8,
                                        (self.render.matcap_bg_color[2]*255.0) as u8,
                                    );
                                    ui.horizontal(|ui| {
                                        ui.label(if self.render.matcap_bg_gradient { "BG Bottom" } else { "BG Color" });
                                        if ui.color_edit_button_srgba(&mut col32).changed() {
                                            self.render.matcap_bg_color = [col32.r() as f32/255.0, col32.g() as f32/255.0, col32.b() as f32/255.0];
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if !self.render.matcap_bg_gradient
                                            && ui.small_button("UI gray").on_hover_text("Reset to panel dark gray (0x1c1c1c)").clicked()
                                        {
                                            self.render.matcap_bg_color = [0.1098, 0.1098, 0.1098];
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    });
                                }
                                if ui.checkbox(&mut self.render.matcap_bg_gradient, "Gradient BG")
.default_menu("render.matcap_bg_gradient", serde_json::json!(self.render.matcap_bg_gradient)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                                if self.render.matcap_bg_gradient {
                                    let mut top32 = egui::Color32::from_rgb(
                                        (self.render.matcap_bg_gradient_top[0]*255.0) as u8,
                                        (self.render.matcap_bg_gradient_top[1]*255.0) as u8,
                                        (self.render.matcap_bg_gradient_top[2]*255.0) as u8,
                                    );
                                    ui.horizontal(|ui| {
                                        ui.label("BG Top");
                                        if ui.color_edit_button_srgba(&mut top32).changed() {
                                            self.render.matcap_bg_gradient_top = [top32.r() as f32/255.0, top32.g() as f32/255.0, top32.b() as f32/255.0];
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    });
                                }
                                if ui.add(egui::Slider::new(&mut self.render.matcap_rotation, -180.0..=180.0).text("Rotation")).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                                // Shared with the v2 MatCap panel — ONE definition (render_settings.rs).
                                self.matcap_material_tint_ui(ui);
                                ui.separator();
                                // Edge highlight
                                if ui.checkbox(&mut self.render.matcap_edge_enabled, "Edge Lines")
.default_menu("render.matcap_edge_enabled", serde_json::json!(self.render.matcap_edge_enabled)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                                if self.render.matcap_edge_enabled {
                                    {
                                        let mut ec32 = egui::Color32::from_rgb(
                                            (self.render.matcap_edge_color[0]*255.0) as u8,
                                            (self.render.matcap_edge_color[1]*255.0) as u8,
                                            (self.render.matcap_edge_color[2]*255.0) as u8,
                                        );
                                        ui.horizontal(|ui| {
                                            ui.label("Edge Color");
                                            if ui.color_edit_button_srgba(&mut ec32).changed() {
                                                self.render.matcap_edge_color = [ec32.r() as f32/255.0, ec32.g() as f32/255.0, ec32.b() as f32/255.0];
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }
                                        });
                                    }
                                    if ui.add(egui::Slider::new(&mut self.render.matcap_edge_thickness, 1.0..=3.0).text("Thickness").step_by(1.0)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                                    if ui.add(egui::Slider::new(&mut self.render.matcap_edge_threshold, 0.05..=1.0).text("Threshold")).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                                }
                            }
                            ShadingMode::Wireframe => {
                                ui.label("DC Wire Settings");
                                ui.separator();
                                if ui.checkbox(&mut self.render.wire_overlay_enabled, "Mesh Overlay (DC + mods)")
.default_menu("render.wire_overlay_enabled", serde_json::json!(self.render.wire_overlay_enabled)).changed() {
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                                if ui.checkbox(&mut self.render.wire_overlay_backface, "Show Backfaces")
.default_menu("render.wire_overlay_backface", serde_json::json!(self.render.wire_overlay_backface)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                if ui.add(egui::Slider::new(&mut self.render.wire_overlay_thickness, 0.0..=20.0).text("Thickness").clamping(egui::SliderClamping::Never)).changed() {
                                    self.render.wire_overlay_thickness = self.render.wire_overlay_thickness.max(0.0);
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                                if ui.add(egui::Slider::new(&mut self.render.wire_overlay_opacity, 0.0..=1.0).text("Opacity")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                {
                                    let mut c32 = egui::Color32::from_rgb(
                                        (self.render.wire_overlay_color[0] * 255.0) as u8,
                                        (self.render.wire_overlay_color[1] * 255.0) as u8,
                                        (self.render.wire_overlay_color[2] * 255.0) as u8,
                                    );
                                    ui.horizontal(|ui| {
                                        ui.label("Color");
                                        if ui.color_edit_button_srgba(&mut c32).changed() {
                                            self.render.wire_overlay_color = [
                                                c32.r() as f32 / 255.0,
                                                c32.g() as f32 / 255.0,
                                                c32.b() as f32 / 255.0,
                                            ];
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    });
                                }
                            }
                            ShadingMode::SolidHdr => {
                                ui.label("Solid Settings");
                                if self.env_hdr_width > 0 {
                                    ui.separator();
                                    if ui.add(egui::Slider::new(&mut self.render.exposure, 0.01..=5.0).text("Exposure").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                }
                            }
                            ShadingMode::Raytrace => {
                                ui.label("Raytrace Settings");
                                ui.separator();
                                if ui.add(egui::Slider::new(&mut self.render.rt_bounces, 1..=8).text("Bounces")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                if ui.add(egui::Slider::new(&mut self.render.rt_shadow_samples, 1..=16).text("Shadow Samples")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                if ui.add(egui::Slider::new(&mut self.render.rt_env_samples, 1..=16).text("Env Samples")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                let mut caustics = self.render.rt_caustics;
                                if ui.checkbox(&mut caustics, "Caustics")
.default_menu("render.rt_caustics", serde_json::json!(self.render.rt_caustics)).changed() { self.render.rt_caustics = caustics; self.mark_dirty(DirtyFlags::RENDER); }
                            }
                            ShadingMode::PathTrace => {
                                ui.label("Path Trace Settings");
                                ui.separator();
                                if ui.button("Open Settings Panel...").clicked() {
                                    self.ui.show_pt_settings = true;
                                    ui.close_menu();
                                }
                            }
                            ShadingMode::Lit => {
                                ui.label("Lit Settings");
                                ui.separator();
                                if ui.add(egui::Slider::new(&mut self.render.exposure, 0.01..=5.0).text("Exposure").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                            }
                        }
                    });
                }
                ui.separator();
                // DC mesh + mesh-modifiers wireframe overlay (works on top of any shading mode).
                {
                    let on = self.render.wire_overlay_enabled;
                    let lbl = if on { "Wires ON" } else { "Wires" };
                    let col = if on {
                        egui::Color32::from_rgb(100, 220, 255)
                    } else {
                        egui::Color32::from_gray(140)
                    };
                    let resp = ui
                        .add(egui::Button::new(egui::RichText::new(lbl).small().color(col)).frame(false))
                        .on_hover_text(
                            "DC mesh TOPOLOGY wireframe (Shift+Z): real Dual Contouring \
                             triangle edges, same transform as the solid mesh — not image edges.",
                        );
                    if resp.clicked() {
                        self.render.wire_overlay_enabled = !self.render.wire_overlay_enabled;
                        self.overlay_lines_fp = u64::MAX;
                        if self.render.wire_overlay_enabled {
                            self.wire_local_cache.clear();
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if resp.secondary_clicked() {
                        let popup_id = ui.id().with("wire_overlay_popup");
                        ui.memory_mut(|m| m.toggle_popup(popup_id));
                    }
                    let popup_id = ui.id().with("wire_overlay_popup");
                    egui::popup_below_widget(
                        ui,
                        popup_id,
                        &resp,
                        egui::PopupCloseBehavior::CloseOnClickOutside,
                        |ui: &mut egui::Ui| {
                            ui.set_min_width(220.0);
                            ui.label("Mesh Wire Overlay");
                            ui.weak("DC surface + body mesh modifiers");
                            ui.separator();
                            if ui
                                .checkbox(&mut self.render.wire_overlay_enabled, "Enabled (Shift+Z)")
.default_menu("render.wire_overlay_enabled", serde_json::json!(self.render.wire_overlay_enabled))
                                .changed()
                            {
                                self.overlay_lines_fp = u64::MAX;
                                if self.render.wire_overlay_enabled {
                                    self.wire_local_cache.clear();
                                }
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui
                                .checkbox(&mut self.render.wire_overlay_backface, "Show backfaces (X-Ray)")
.default_menu("render.wire_overlay_backface", serde_json::json!(self.render.wire_overlay_backface))
                                .changed()
                            {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.render.wire_overlay_opacity, 0.05..=1.0)
                                        .text("Opacity"),
                                )
                                .changed()
                            {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.render.wire_overlay_thickness, 0.5..=5.0)
                                        .text("Thickness"),
                                )
                                .changed()
                            {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            {
                                let mut col32 = egui::Color32::from_rgb(
                                    (self.render.wire_overlay_color[0] * 255.0) as u8,
                                    (self.render.wire_overlay_color[1] * 255.0) as u8,
                                    (self.render.wire_overlay_color[2] * 255.0) as u8,
                                );
                                ui.horizontal(|ui| {
                                    ui.label("Color");
                                    if ui.color_edit_button_srgba(&mut col32).changed() {
                                        self.render.wire_overlay_color = [
                                            col32.r() as f32 / 255.0,
                                            col32.g() as f32 / 255.0,
                                            col32.b() as f32 / 255.0,
                                        ];
                                        self.overlay_lines_fp = u64::MAX;
                                        self.mark_dirty(DirtyFlags::RENDER);
                                    }
                                });
                            }
                        },
                    );
                }
                ui.separator();
                for (i,lbl) in ["\u{26A1}","\u{1F527}","\u{1F3A8}","\u{2728}","\u{1F48E}"].iter().enumerate() {
                    if ui.selectable_label(self.render.render_quality==i as u32,*lbl)
                        .on_hover_text(["Draft","Low","Medium","High","Ultra"][i]).clicked(){
                        self.render.render_quality=i as u32; self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                // (BVH toggle REMOVED 2026-07-14 — GLSL BVH analytical display path stripped)
                // Debug render mode toggle
                if self.ui.debug_render_mode > 0 {
                    ui.separator();
                    let lbl = match self.ui.debug_render_mode {
                        1 => "DBG:March",
                        2 => "DBG:Steps",
                        3 => "DBG:Bricks",
                        _ => "DBG:?",
                    };
                    if ui.selectable_label(true, lbl).on_hover_text("Click to cycle debug modes").clicked() {
                        self.ui.debug_render_mode = (self.ui.debug_render_mode + 1) % 4;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                if self.tool.current.is_create() {
                    ui.separator();
                    let op = self.tool.current.as_create().unwrap().op;
                    ui.colored_label(op.color(), op.label());
                }
                if self.viewport.smart_snap_on {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(255, 200, 0), "\u{1F9F2}Snap");
                }
                if self.tool.mode.is_bevel() {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(255, 180, 60), format!("{}:Bevel", self.keybindings.label(ACT_BEVEL_TOGGLE)));
                }
                if self.tool.mode.is_extrude() {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(100, 200, 255), format!("{}:Extrude", self.keybindings.label(ACT_EXTRUDE_TOGGLE)));
                }
                if self.tool.current.is_pivot_edit() {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("{}:Pivot", self.keybindings.label(ACT_PIVOT_EDIT)));
                }
                // Macro recording indicator (blinking red dot)
                if self.macro_recording {
                    ui.separator();
                    let t = ui.input(|i| i.time);
                    let blink = (t * 2.5).sin() > 0.0;
                    let col = if blink { egui::Color32::from_rgb(255, 40, 40) } else { egui::Color32::from_rgb(120, 20, 20) };
                    ui.colored_label(col, format!("\u{23FA} REC ({})", self.macro_buffer.len()));
                    ctx.request_repaint(); // keep blinking
                }
                if let Some(ref dp) = self.tool.depth_pick {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(220, 60, 220), format!("DEPTH {}/{}", dp.index + 1, dp.candidates.len()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let sel=self.sel.usel.selected.len(); let tot=self.scene.scene_graph.all_nodes().filter(|(_,n)| n.payload.is_renderable()).count();
                    if sel>0 { ui.weak(format!("{}/{} selected",sel,tot)); } else { ui.weak(format!("{} objects",tot)); }
                    ui.separator();
                    // Occlusion-aware selection toggle ("visible only" vs X-ray through-select).
                    // ON = pick only the front-most visible sub-element (DCC default);
                    // OFF = ray selects through all geometry (X-ray).
                    let occl_lbl = if self.sel.visible_only { "\u{25C9} Visible" } else { "\u{25CE} X-Ray" };
                    let occl_col = if self.sel.visible_only { egui::Color32::from_rgb(100, 200, 255) } else { egui::Color32::from_rgb(255, 180, 80) };
                    if ui.add(egui::Label::new(egui::RichText::new(occl_lbl).color(occl_col).size(12.0)).sense(egui::Sense::click()))
                        .on_hover_text("Selection occlusion — Visible: pick only front-most (occlusion-aware) | X-Ray: select through geometry")
                        .clicked() {
                        self.sel.visible_only = !self.sel.visible_only;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    ui.separator();
                    // Proportional Edit ("soft select") toggle — O key. No prior UI made it discoverable,
                    // so an accidental O left it ON with no obvious way to turn it back off (owner report).
                    let prop_on = self.poly_proportional;
                    let prop_lbl = if prop_on { "\u{25C9} Soft" } else { "\u{25CE} Soft" };
                    let prop_col = if prop_on { egui::Color32::from_rgb(255, 180, 80) } else { egui::Color32::from_gray(140) };
                    if ui.add(egui::Label::new(egui::RichText::new(prop_lbl).color(prop_col).size(12.0)).sense(egui::Sense::click()))
                        .on_hover_text("Proportional Edit / soft select (O key) — ON: a move/rotate/scale falls off to nearby vertices; scroll during the edit to change radius. Click to toggle OFF.")
                        .clicked() {
                        self.poly_proportional = !self.poly_proportional;
                        self.toast(if self.poly_proportional { "Proportional Edit ON" } else { "Proportional Edit OFF" });
                    }
                    ui.separator();
                    // Ortho/Persp toggle
                    let proj_lbl = if self.render.ortho { "Ortho" } else { "Persp" };
                    if ui.selectable_label(self.render.ortho, proj_lbl).on_hover_text("Toggle orthographic / perspective").clicked() {
                        self.set_user_projection_ortho(!self.render.ortho);
                    }
                    ui.separator();
                    // Grid toggle (LMB=toggle, RMB=cycle mode)
                    let grid_lbl = if !self.viewport.grid_visible { "\u{2592} Grid" }
                        else if self.viewport.grid_mode == 1 { "\u{2588} Shadow" }
                        else { "\u{2588} Grid" };
                    let grid_col = if self.viewport.grid_visible { egui::Color32::from_rgb(180, 180, 180) } else { egui::Color32::from_rgb(100, 100, 100) };
                    let grid_resp = ui.add(egui::Label::new(
                        egui::RichText::new(grid_lbl).color(grid_col).size(12.0)
                    ).sense(egui::Sense::click()));
                    grid_resp.clone().on_hover_text("LMB: toggle | RMB: Wire / Shadow Catcher");
                    if grid_resp.clicked() {
                        self.viewport.grid_visible = !self.viewport.grid_visible;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if grid_resp.secondary_clicked() {
                        self.viewport.grid_mode = if self.viewport.grid_mode == 0 { 1 } else { 0 };
                        if !self.viewport.grid_visible { self.viewport.grid_visible = true; }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    ui.separator();
                    // Work Plane button
                    let wp_lbl = match self.viewport.wp_mode {
                        WorkPlaneMode::Auto   => "\u{2B21} WP",
                        WorkPlaneMode::Locked => "\u{2B21} WP \u{25CF}",
                    };
                    let wp_col = match self.viewport.wp_mode {
                        WorkPlaneMode::Auto   => egui::Color32::from_rgb(140, 140, 140),
                        WorkPlaneMode::Locked => egui::Color32::from_rgb(100, 200, 255),
                    };
                    let wp_btn = ui.add(egui::Label::new(
                        egui::RichText::new(wp_lbl).color(wp_col).size(12.0)
                    ).sense(egui::Sense::click()));
                    if wp_btn.clicked() {
                        self.reset_workplane();
                    }
                    if wp_btn.secondary_clicked() {
                        self.viewport.wp_show_settings = !self.viewport.wp_show_settings;
                    }
                    wp_btn.on_hover_text("Work Plane — LMB: reset | RMB: settings | Home: lock to surface");
                    ui.separator();
                    if self.render.ssr_enabled { ui.colored_label(egui::Color32::from_rgb(100,200,255),"SSR"); }
                    if self.render.bloom_enabled { ui.colored_label(egui::Color32::from_rgb(255,200,100),"Bloom"); }
                    if self.render.fog_enabled { ui.colored_label(egui::Color32::from_rgb(150,200,150),"Fog"); }
                    if self.env_hdr_width > 0 { ui.weak(format!("EV:{:.1}", self.render.exposure)); }
                });
            });
        });
    }
}
