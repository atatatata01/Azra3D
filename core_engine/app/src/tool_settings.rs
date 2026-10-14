//! Tool property panels — left tool strip, create tool, poly tool props — moved from ui_update.rs.

use eframe::egui;
use glam::Vec3;
use core_math::primitives::SdfShape;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_left_tool_strip(&mut self, ctx: &egui::Context) {
        self.draw_vertex_debug_window(ctx); // debug board (Shift+I) — independent of the tool panel
        if !self.layout.show_tool_panel { return; }
        self.draw_workspace_panels(ctx);
    }

    /// DEBUG board (Shift+I): a scrollable table of every vertex ID + X/Y/Z, easy to cross-reference
    /// against Blender. Paired with the magenta viewport dots (which show just the ID).
    pub(crate) fn draw_vertex_debug_window(&self, ctx: &egui::Context) {
        if !self.poly_vertex_debug { return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return,
        };
        egui::Window::new("Vertex Debug (Shift+I)")
            .default_pos(egui::pos2(900.0, 80.0))
            .default_width(250.0)
            .show(ctx, |ui| {
                ui.label(format!("{} verts · {} faces", mesh.vertices.len(), mesh.faces.len()));
                ui.separator();
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    egui::Grid::new("vtx_dbg_grid").striped(true).num_columns(4).spacing([10.0, 2.0]).show(ui, |ui| {
                        ui.strong("ID"); ui.strong("X"); ui.strong("Y"); ui.strong("Z"); ui.end_row();
                        for (i, v) in mesh.vertices.iter().enumerate() {
                            ui.monospace(format!("{i}"));
                            ui.monospace(format!("{:.3}", v.position.x));
                            ui.monospace(format!("{:.3}", v.position.y));
                            ui.monospace(format!("{:.3}", v.position.z));
                            ui.end_row();
                        }
                    });
                });
            });
    }

    pub(crate) fn draw_create_tool_settings(&mut self, ctx: &egui::Context) {
        if !self.viewport.show_tool_viewport_ui { return; }
        // Show only when a shape tool is armed or actively creating
        if !self.tool.mode.is_create_armed() && !self.tool.current.is_create() { return; }

        egui::Window::new("Tool Settings")
            .fixed_pos(egui::pos2(58.0, 60.0))
            .default_width(180.0)
            .resizable(false)
            .collapsible(true)
            .title_bar(true)
            .show(ctx, |ui| {
                let dim_color = egui::Color32::from_rgb(140, 200, 255);
                ui.label(egui::RichText::new("DIMENSIONS").size(9.0).color(egui::Color32::from_gray(120)));

                // During active creation, show live values from CreateState
                let (live_w, live_d, live_h) = if let Some(cs) = self.tool.current.as_create() {
                    (cs.local_u.abs(), cs.local_v.abs(), cs.depth.abs())
                } else {
                    (self.tool.create_tool_settings.width, self.tool.create_tool_settings.depth, self.tool.create_tool_settings.height)
                };

                // Width
                ui.horizontal(|ui| {
                    let lock = &mut self.tool.create_tool_settings.lock_width;
                    if ui.selectable_label(*lock, if *lock {"W"} else {"W"}).on_hover_text("Lock width").clicked() { *lock = !*lock; }
                    if *lock {
                        ui.add(egui::DragValue::new(&mut self.tool.create_tool_settings.width).speed(0.01).range(0.001..=100.0).suffix(" u"));
                    } else {
                        ui.colored_label(dim_color, format!("{:.3}", live_w));
                    }
                });
                // Depth
                ui.horizontal(|ui| {
                    let lock = &mut self.tool.create_tool_settings.lock_depth;
                    if ui.selectable_label(*lock, if *lock {"D"} else {"D"}).on_hover_text("Lock depth").clicked() { *lock = !*lock; }
                    if *lock {
                        ui.add(egui::DragValue::new(&mut self.tool.create_tool_settings.depth).speed(0.01).range(0.001..=100.0).suffix(" u"));
                    } else {
                        ui.colored_label(dim_color, format!("{:.3}", live_d));
                    }
                });
                // Height
                ui.horizontal(|ui| {
                    let lock = &mut self.tool.create_tool_settings.lock_height;
                    if ui.selectable_label(*lock, if *lock {"H"} else {"H"}).on_hover_text("Lock height").clicked() { *lock = !*lock; }
                    if *lock {
                        ui.add(egui::DragValue::new(&mut self.tool.create_tool_settings.height).speed(0.01).range(0.001..=100.0).suffix(" u"));
                    } else {
                        ui.colored_label(dim_color, format!("{:.3}", live_h));
                    }
                });

                ui.separator();
                ui.label(egui::RichText::new("CONSTRAINTS").size(9.0).color(egui::Color32::from_gray(120)));
                ui.horizontal(|ui| {
                    ui.label("Grid:");
                    ui.add(egui::DragValue::new(&mut self.tool.create_tool_settings.snap_grid_size)
                        .speed(0.01).range(0.01..=10.0).suffix(" u"));
                });
                ui.horizontal(|ui| {
                    ui.label("Blend:");
                    ui.add(egui::DragValue::new(&mut self.tool.create_blend_k)
                        .speed(0.02)
                        .range(0.0..=self.tweak.blend_k_max.max(0.1)));
                });

                // Live feedback during creation
                if let Some(cs) = self.tool.current.as_create() {
                    ui.separator();
                    let phase = match cs.phase {
                        CreatePhase::Footprint => "Footprint",
                        CreatePhase::Extrude => "Extrude",
                    };
                    ui.colored_label(cs.op.color(), format!("{} — {}", cs.op.label(), phase));
                }
            });

        // Apply locked dimensions to the active create state
        if self.tool.current.is_create() {
            let cts = &self.tool.create_tool_settings;
            let mut changed = false;
            if cts.lock_width {
                if let Some(cs) = self.tool.current.as_create_mut() {
                    let target = cts.width * cs.local_u.signum().max(0.001_f32.copysign(1.0));
                    if (cs.local_u - target).abs() > 0.0001 { cs.local_u = target; changed = true; }
                }
            }
            if cts.lock_depth {
                if let Some(cs) = self.tool.current.as_create_mut() {
                    let target = cts.depth * cs.local_v.signum().max(0.001_f32.copysign(1.0));
                    if (cs.local_v - target).abs() > 0.0001 { cs.local_v = target; changed = true; }
                }
            }
            if cts.lock_height && self.tool.current.as_create().map(|cs| cs.phase) == Some(CreatePhase::Extrude) {
                if let Some(cs) = self.tool.current.as_create_mut() {
                    let target = cts.height * cs.depth.signum().max(0.001_f32.copysign(1.0));
                    if (cs.depth - target).abs() > 0.0001 { cs.depth = target; changed = true; }
                }
            }
            if changed {
                // Re-apply the locked dimensions to the primitive
                if let Some(cs) = self.tool.current.as_create() {
                    let prim_idx = cs.prim_idx;
                    let lu = cs.local_u; let lv = cs.local_v;
                    let depth = cs.depth;
                    let anchor = cs.anchor; let tu = cs.tangent_u; let tv = cs.tangent_v;
                    let hit_normal = cs.hit_normal;
                    if prim_idx < self.scene.primitives.len() {
                        let he_x = (lu.abs() * 0.5).max(0.001);
                        let he_z = (lv.abs() * 0.5).max(0.001);
                        let he_y = (depth.abs() * 0.5).max(0.001);
                        let draw_center = anchor + tu * (lu * 0.5) + tv * (lv * 0.5);
                        let pos = draw_center + hit_normal * (depth * 0.5);
                        let he = Vec3::new(he_x, he_y, he_z);
                        self.write_prim(prim_idx, |p| {
                            p.position = pos;
                            if let SdfShape::Box { ref mut half_extents, .. } = p.shape { *half_extents = he; }
                        });
                        self.rebake_no_undo();
                    }
                }
            }
        }

        // Update settings from live creation values (for display & persistence)
        if let Some(cs) = self.tool.current.as_create() {
            if !self.tool.create_tool_settings.lock_width { self.tool.create_tool_settings.width = cs.local_u.abs(); }
            if !self.tool.create_tool_settings.lock_depth { self.tool.create_tool_settings.depth = cs.local_v.abs(); }
            if !self.tool.create_tool_settings.lock_height { self.tool.create_tool_settings.height = cs.depth.abs(); }
        }
    }

    /// Extract Intersection options (X pie → MORE… → Extract).
    /// Shown for Subtract **or** Intersect cutters (same command freezes either into a new body).
    pub(crate) fn draw_extract_intersection_settings(&mut self, ctx: &egui::Context) {
        if !self.viewport.show_tool_viewport_ui {
            return;
        }
        use core_math::primitives::BooleanOp;
        let is_cutter = |op: &BooleanOp| {
            matches!(
                op,
                BooleanOp::Subtract { .. }
                    | BooleanOp::ChamferSubtract { .. }
                    | BooleanOp::Intersect
                    | BooleanOp::SmoothIntersect { .. }
            )
        };
        let has_cutter = self.selected_prim_indices_unified().iter().any(|&i| {
            i < self.scene.primitives.len() && is_cutter(&self.scene.primitives[i].operation)
        }) || self.sel.usel.selected_nodes().any(|nid| {
            self.scene
                .scene_graph
                .body_op(nid)
                .map_or(false, is_cutter)
        });
        if self.modeling_context != ModelingContext::Sdf || !has_cutter {
            return;
        }

        egui::Window::new("Tool Settings")
            .id(egui::Id::new("extract_intersection_settings"))
            .fixed_pos(egui::pos2(58.0, 60.0))
            .default_width(200.0)
            .resizable(false)
            .collapsible(true)
            .title_bar(true)
            .show(ctx, |ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 160, 80),
                    egui::RichText::new("EXTRACT INTERSECTION")
                        .size(10.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new("New body = frozen MeshVolume · original → Subtract")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                ui.label(
                    egui::RichText::new("X pie → center → Extract  (create pieces)")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                ui.separator();
                let s = &mut self.tool.extract_intersection;
                ui.checkbox(&mut s.geometric_all, "All geometric overlaps")
                    .on_hover_text("Off = same body stack only (default). On = any overlapping prim in the scene.");
                ui.checkbox(&mut s.keep_material, "Keep material")
                    .on_hover_text("On = MeshVolume keeps source material (default). Off = default material.");
                ui.checkbox(&mut s.remove_cutter, "Remove original cutter")
                    .on_hover_text(
                        "Off (default, pieces): convert original cutter to Subtract so the \
                         source body keeps the cut and the extract is a separate solid.\n\
                         On: delete the original cutter after extract (no hole left).",
                    );
                ui.separator();
                if ui.button("Extract now").clicked() {
                    self.extract_intersection();
                }
                ui.add_space(6.0);
                ui.colored_label(
                    egui::Color32::from_rgb(140, 180, 220),
                    egui::RichText::new("STACK → SDF")
                        .size(10.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new("Merge selected + prims before it → one MeshVolume")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                if ui
                    .button("Stack → SDF (merge down)")
                    .on_hover_text(
                        "Freezes the body stack from the top through the selected prim \
                         into a single MeshVolume solid. Use on complex stacks to collapse history.",
                    )
                    .clicked()
                {
                    self.stack_to_sdf();
                }
            });
    }

    /// Linear array (Ctrl+Shift+D) — reuses the same "Tool Settings" window pattern
    /// as create tools (fixed left dock, no new floating chrome).
    pub(crate) fn draw_linear_array_tool_settings(&mut self, ctx: &egui::Context) {
        if !self.viewport.show_tool_viewport_ui {
            return;
        }
        let Some(ts) = self.tool.current.as_transform() else { return };
        if !ts.shift_duplicate {
            return;
        }

        // Live values from the free-grab primary
        let origin = ts.dup_origin_local;
        let count = ts.array_count.max(1);
        let (offset, rot, scale) = if let Some(t) = ts.multi_targets.first() {
            if let Some(idx) = self.prim_index_by_uid(t.prim_uid) {
                let p = &self.scene.primitives[idx];
                (
                    p.position - origin,
                    p.rotation_deg - t.original_rot,
                    {
                        let s0 = t.original_scale.abs().max(Vec3::splat(1e-4)) * t.original_scale.signum();
                        p.scale / s0
                    },
                )
            } else {
                (Vec3::ZERO, Vec3::ZERO, Vec3::ONE)
            }
        } else {
            (Vec3::ZERO, Vec3::ZERO, Vec3::ONE)
        };

        let mut edit_offset = offset;
        let mut edit_rot = rot;
        let mut edit_scale = scale;
        let mut edit_count = count as i32;
        let mut changed = false;
        let mut count_changed = false;

        egui::Window::new("Tool Settings")
            .id(egui::Id::new("linear_array_tool_settings"))
            .fixed_pos(egui::pos2(58.0, 60.0))
            .default_width(200.0)
            .resizable(false)
            .collapsible(true)
            .title_bar(true)
            .show(ctx, |ui| {
                let circle = self
                    .tool
                    .current
                    .as_transform()
                    .map_or(false, |t| t.array_circle);
                ui.colored_label(
                    egui::Color32::from_rgb(120, 190, 255),
                    egui::RichText::new(if circle {
                        "CIRCLE ARRAY"
                    } else {
                        "LINEAR ARRAY"
                    })
                    .size(10.0)
                    .strong(),
                );
                ui.label(
                    egui::RichText::new("Ctrl+Shift+D · C = circle · scroll = count")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                ui.separator();

                ui.label(
                    egui::RichText::new("COPIES")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                if ui
                    .add(
                        egui::DragValue::new(&mut edit_count)
                            .speed(0.2)
                            .range(1..=50)
                            .prefix("× "),
                    )
                    .changed()
                {
                    count_changed = true;
                    changed = true;
                }

                ui.separator();
                ui.label(
                    egui::RichText::new("OFFSET")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                for (lbl, v) in [
                    ("X", &mut edit_offset.x),
                    ("Y", &mut edit_offset.y),
                    ("Z", &mut edit_offset.z),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(lbl);
                        if ui
                            .add(egui::DragValue::new(v).speed(0.02).fixed_decimals(3))
                            .changed()
                        {
                            changed = true;
                        }
                    });
                }

                ui.separator();
                ui.label(
                    egui::RichText::new("ROTATION °")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                for (lbl, v) in [
                    ("X", &mut edit_rot.x),
                    ("Y", &mut edit_rot.y),
                    ("Z", &mut edit_rot.z),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(lbl);
                        if ui
                            .add(egui::DragValue::new(v).speed(0.5).fixed_decimals(1))
                            .changed()
                        {
                            changed = true;
                        }
                    });
                }

                ui.separator();
                ui.label(
                    egui::RichText::new("SCALE")
                        .size(9.0)
                        .color(egui::Color32::from_gray(120)),
                );
                for (lbl, v) in [
                    ("X", &mut edit_scale.x),
                    ("Y", &mut edit_scale.y),
                    ("Z", &mut edit_scale.z),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(lbl);
                        if ui
                            .add(
                                egui::DragValue::new(v)
                                    .speed(0.01)
                                    .fixed_decimals(3)
                                    .range(0.01..=100.0),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });
                }

                ui.separator();
                ui.weak("Shift = axis lock · scroll/Shift+scroll = count · C = circle · LMB confirm");
            });

        if count_changed {
            if let Some(ts) = self.tool.current.as_transform_mut() {
                let new_c = edit_count.clamp(1, 50) as u32;
                let delta = new_c as i32 - ts.array_count as i32;
                if delta != 0 {
                    // Drive existing scroll rebuild path
                    self.transform_array_scroll(delta as f32 * 40.0);
                }
            }
        }
        if changed {
            // Apply offset/rot/scale to primary end-copy, then redistribute intermediates.
            let (primary_uid, origin, o_rot, o_scale) = {
                let Some(ts) = self.tool.current.as_transform() else { return };
                let Some(t) = ts.multi_targets.first() else { return };
                (t.prim_uid, ts.dup_origin_local, t.original_rot, t.original_scale)
            };
            if let Some(idx) = self.prim_index_by_uid(primary_uid) {
                self.scene.primitives[idx].position = origin + edit_offset;
                self.scene.primitives[idx].rotation_deg = o_rot + edit_rot;
                self.scene.primitives[idx].scale = o_scale
                    * Vec3::new(
                        edit_scale.x.max(0.01),
                        edit_scale.y.max(0.01),
                        edit_scale.z.max(0.01),
                    );
                self.mirror_prim_to_graph(idx);
            }
            // Also update multi_targets original so free-grab continues from new base?
            // Keep original_pos as drag start — for param edit we force positions via update_array.
            self.update_array_positions_public();
            // Force full layout including primary already set
            if let Some(ts) = self.tool.current.as_transform() {
                let origin = ts.dup_origin_local;
                let count = ts.array_count.max(1);
                let array_uids = ts.array_prim_uids.clone();
                let primary_uid = ts.multi_targets.first().map(|t| t.prim_uid);
                if let Some(puid) = primary_uid {
                    if let Some(pidx) = self.prim_index_by_uid(puid) {
                        let end_pos = self.scene.primitives[pidx].position;
                        let end_rot = self.scene.primitives[pidx].rotation_deg;
                        let end_scl = self.scene.primitives[pidx].scale;
                        // Need source rot/scale at origin for lerp
                        let src_rot = o_rot;
                        let src_scl = o_scale;
                        for (i, &uid) in array_uids.iter().enumerate() {
                            let t = (i + 1) as f32 / count as f32;
                            if let Some(idx) = self.prim_index_by_uid(uid) {
                                self.scene.primitives[idx].position =
                                    origin + (end_pos - origin) * t;
                                self.scene.primitives[idx].rotation_deg =
                                    src_rot + (end_rot - src_rot) * t;
                                self.scene.primitives[idx].scale = src_scl.lerp(end_scl, t);
                                self.mirror_prim_to_graph(idx);
                            }
                        }
                    }
                }
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    /// Public wrapper so tool settings can refresh intermediate array copies.
    pub(crate) fn update_array_positions_public(&mut self) {
        // Delegates to private path used by modal transform updates.
        // (Defined next to transform_array_scroll in gizmo.rs — call via rebuild.)
        if self
            .tool
            .current
            .as_transform()
            .map_or(false, |t| t.shift_duplicate)
        {
            // Trigger a no-op scroll of 0 via internal rebuild by re-applying current count
            let sc = 0.0_f32;
            let _ = sc;
            // Use the same layout as transform path: update_array_positions is private;
            // call transform_array_scroll with 0 steps won't rebuild — call apply via scroll
            // of 0 accum. Just re-run position layout by temporarily scrolling +1 then -1
            // is wasteful. Inline minimal:
            let (origin, count, primary_uid, array_uids) = {
                let Some(ts) = self.tool.current.as_transform() else { return };
                let uid = ts.multi_targets.first().map(|t| t.prim_uid);
                (
                    ts.dup_origin_local,
                    ts.array_count.max(1),
                    uid,
                    ts.array_prim_uids.clone(),
                )
            };
            let Some(puid) = primary_uid else { return };
            let Some(pidx) = self.prim_index_by_uid(puid) else { return };
            let end = self.scene.primitives[pidx].position;
            let offset = end - origin;
            for (i, &uid) in array_uids.iter().enumerate() {
                let t = (i + 1) as f32 / count as f32;
                if let Some(idx) = self.prim_index_by_uid(uid) {
                    self.scene.primitives[idx].position = origin + offset * t;
                    self.mirror_prim_to_graph(idx);
                }
            }
        }
    }

    pub(crate) fn draw_poly_tool_properties(&mut self, ctx: &egui::Context) {
        if !self.viewport.show_tool_viewport_ui { return; }
        if self.modeling_context != ModelingContext::Polygonal { return; }

        // Determine what to show based on active tool state
        let title;
        let show;
        if self.tool.current.is_poly_bevel() {
            title = "Bevel";
            show = true;
        } else if self.tool.current.is_poly_extrude() {
            title = "Extrude";
            show = true;
        } else if self.tool.current.is_poly_inset() {
            title = "Inset";
            show = true;
        } else if self.tool.current.is_poly_transform() {
            title = "Transform";
            show = true;
        } else if self.poly_tool == PolyTool::LoopCut {
            title = "Loop Cut";
            show = true;
        } else if self.poly_tool == PolyTool::Knife {
            title = "Knife";
            show = true;
        } else {
            show = false;
            title = "";
        }

        if !show { return; }

        egui::Window::new(format!("Tool: {}", title))
            .fixed_pos(egui::pos2(58.0, 90.0))
            .default_width(160.0)
            .resizable(false)
            .collapsible(false)
            .title_bar(true)
            .show(ctx, |ui| {
                let accent = egui::Color32::from_rgb(140, 200, 255);
                if self.tool.current.is_poly_bevel() {
                    if let Some(d) = self.tool.current.as_poly_bevel() {
                        ui.colored_label(accent, format!("Width: {:.3}", d.width));
                        ui.colored_label(accent, format!("Segments: {}", d.segments));
                        ui.colored_label(accent, format!("Shape: {:.2}", d.shape));
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Drag = width, Scroll = segments").size(9.0).color(egui::Color32::from_gray(120)));
                    ui.label(egui::RichText::new("LMB/Enter = confirm, Esc = cancel").size(9.0).color(egui::Color32::from_gray(120)));
                } else if self.tool.current.is_poly_extrude() {
                    if let Some(d) = self.tool.current.as_poly_extrude() {
                        ui.colored_label(accent, format!("Offset: {:.3}", d.accumulated_offset));
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Drag = offset · Shift = world axis · Ctrl = snap").size(9.0).color(egui::Color32::from_gray(120)));
                } else if self.tool.current.is_poly_inset() {
                    if let Some(d) = self.tool.current.as_poly_inset() {
                        ui.colored_label(accent, format!("Thickness: {:.3}", d.accumulated_amount));
                        ui.colored_label(accent, format!("Depth: {:.3}", d.accumulated_depth));
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Drag R=in / L=out, Alt+Drag=depth, Ctrl=snap").size(9.0).color(egui::Color32::from_gray(120)));
                } else if self.tool.current.is_poly_transform() {
                    if let Some(ts) = self.tool.current.as_poly_transform() {
                        let kind_str = match ts.kind {
                            TransformKind::Grab => "Grab",
                            TransformKind::Rotate => "Rotate",
                            TransformKind::Scale => "Scale",
                            TransformKind::Elongate => "Elongate",
                        };
                        ui.colored_label(accent, kind_str);
                        if ts.axis != AxisConstraint::None {
                            let axis_str = match ts.axis {
                                AxisConstraint::X => "X",
                                AxisConstraint::Y => "Y",
                                AxisConstraint::Z => "Z",
                                AxisConstraint::PlaneX => "YZ plane",
                                AxisConstraint::PlaneY => "XZ plane",
                                AxisConstraint::PlaneZ => "XY plane",
                                AxisConstraint::None => "",
                            };
                            ui.colored_label(accent, format!("Axis: {}", axis_str));
                        }
                        if !ts.num_buf.is_empty() {
                            ui.colored_label(accent, format!("Value: {}", ts.num_buf));
                        }
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("X/Y/Z = axis, type number, Enter/LMB = confirm").size(9.0).color(egui::Color32::from_gray(120)));
                } else if self.poly_tool == PolyTool::LoopCut {
                    ui.colored_label(accent, format!("Cuts: {}", self.loop_cut_count));
                    ui.separator();
                    ui.label(egui::RichText::new("Scroll = count, LMB = cut, Esc = cancel").size(9.0).color(egui::Color32::from_gray(120)));
                } else if self.poly_tool == PolyTool::Knife {
                    ui.colored_label(accent, format!("Points: {}", self.knife_points.len()));
                    if let Some(sp) = self.knife_snap_pos {
                        ui.colored_label(egui::Color32::from_gray(160), format!("({:.2}, {:.2}, {:.2})", sp.x, sp.y, sp.z));
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("LMB = place, Enter = cut, Esc = cancel").size(9.0).color(egui::Color32::from_gray(120)));
                }
            });
    }

}
