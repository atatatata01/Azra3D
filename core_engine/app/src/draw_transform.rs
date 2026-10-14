// draw_transform.rs — Live transform values HUD, smart mirror tool overlay
// Extracted from drawing.rs
use eframe::egui;
use glam::Vec3;
use core_math::scene_graph::NodePayload;
use crate::*;

impl AzraApp {
    /// Paint a transform value badge at a screen position (shared by gizmo + modal HUD).
    fn paint_transform_badge(painter: &egui::Painter, sp: egui::Pos2, text: &str, extra: &str) {
        let font = egui::FontId::monospace(12.0);
        let col = egui::Color32::from_rgb(100, 220, 255);
        let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
        let full = if extra.is_empty() { text.to_string() } else { format!("{}  {}", text, extra) };
        let text_pos = egui::pos2(sp.x, sp.y - 30.0);
        let galley = painter.layout_no_wrap(full.clone(), font.clone(), col);
        let text_rect = egui::Rect::from_min_size(
            egui::pos2(text_pos.x - galley.size().x * 0.5 - 4.0, text_pos.y - 2.0),
            egui::vec2(galley.size().x + 8.0, galley.size().y + 4.0),
        );
        painter.rect_filled(text_rect, 3.0, bg);
        painter.text(text_pos, egui::Align2::CENTER_TOP, full, font, col);
    }

    /// Resolve the active selection into (world_position, formatted_text) for the transform HUD.
    fn resolve_hud_data(&self, translate: bool, rotate: bool) -> Option<(Vec3, String)> {
        match self.sel.usel.active {
            Some(SceneItemId::Prim(uid)) => {
                if let Some(idx) = self.prim_index_by_uid(uid) {
                    if idx < self.scene.primitives.len() {
                        let p = &self.scene.primitives[idx];
                        let text = if translate {
                            format!("X:{:.3}  Y:{:.3}  Z:{:.3}", p.position.x, p.position.y, p.position.z)
                        } else if rotate {
                            format!("Rx:{:.1}  Ry:{:.1}  Rz:{:.1}", p.rotation_deg.x, p.rotation_deg.y, p.rotation_deg.z)
                        } else {
                            format!("Sx:{:.3}  Sy:{:.3}  Sz:{:.3}", p.scale.x, p.scale.y, p.scale.z)
                        };
                        return Some((p.position, text));
                    }
                }
                None
            }
            Some(SceneItemId::Node(nid)) => {
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    // A Light node's transform lives in `LightData` — reading `local_position`
                    // here printed X/Y/Z 0,0,0 for an outliner-selected light while the gizmo
                    // moved it. Same formatter as the `SceneItemId::Light` arm below.
                    if let NodePayload::Light(d) = &node.payload {
                        return Some(Self::light_hud_data(d, translate, rotate));
                    }
                    let (pos, rot, scale) = if let NodePayload::SdfBody { origin, .. } = &node.payload {
                        (*origin, Vec3::ZERO, Vec3::ONE)
                    } else {
                        (node.local_position, node.local_rotation, node.local_scale)
                    };
                    let text = if translate {
                        format!("X:{:.3}  Y:{:.3}  Z:{:.3}", pos.x, pos.y, pos.z)
                    } else if rotate {
                        format!("Rx:{:.1}  Ry:{:.1}  Rz:{:.1}", rot.x, rot.y, rot.z)
                    } else {
                        format!("Sx:{:.3}  Sy:{:.3}  Sz:{:.3}", scale.x, scale.y, scale.z)
                    };
                    Some((pos, text))
                } else { None }
            }
            Some(SceneItemId::Light(lid)) => {
                self.scene.light_by_id(lid)
                    .map(|d| Self::light_hud_data(d, translate, rotate))
            }
            None => None,
        }
    }

    /// HUD badge for a light, shared by both selection forms (viewport pick =
    /// `SceneItemId::Light`, outliner row = `SceneItemId::Node` with a Light payload).
    /// Reports what the mode actually edits: G = position, R = aim, S = attenuation radius —
    /// the old text printed X/Y/Z under S, where nothing about the position changes.
    fn light_hud_data(
        d: &core_math::scene_graph::LightData,
        translate: bool,
        rotate: bool,
    ) -> (Vec3, String) {
        let pos = d.gizmo_position();
        let text = if rotate {
            format!("Az:{:.1}  El:{:.1}", d.azimuth, d.elevation)
        } else if translate {
            format!("X:{:.3}  Y:{:.3}  Z:{:.3}", pos.x, pos.y, pos.z)
        } else {
            format!("Radius:{:.3}", d.range)
        };
        (pos, text)
    }

    // ---- Live transform values during gizmo drag ----
    pub(crate) fn draw_live_transform_values(&self, painter: &egui::Painter, rect: egui::Rect) {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let multi_badge = if self.sel.usel.selected.len() > 1 {
            format!("(+{} more)", self.sel.usel.selected.len() - 1)
        } else { String::new() };

        // Show values during gizmo drag
        if let Some(gi) = self.tool.current.as_gizmo() {
            let translate = gi.mode == GizmoMode::Translate;
            let rotate = gi.mode == GizmoMode::Rotate;
            if let Some((world_pos, text)) = self.resolve_hud_data(translate, rotate) {
                if let Some(sp) = world_to_screen(world_pos, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                    Self::paint_transform_badge(painter, sp, &text, &multi_badge);
                }
            }
        }
        // Show values during modal transform (G/R/S key)
        if let Some(ts) = self.tool.current.as_transform() {
            let translate = ts.kind == TransformKind::Grab;
            let rotate = ts.kind == TransformKind::Rotate;
            if let Some((world_pos, text)) = self.resolve_hud_data(translate, rotate) {
                if let Some(sp) = world_to_screen(world_pos, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                    Self::paint_transform_badge(painter, sp, &text, &multi_badge);
                }
            }
        }
        // Poly extrude drag HUD
        if let Some(d) = self.tool.current.as_poly_extrude() {
            let font = egui::FontId::monospace(12.0);
            let col = egui::Color32::from_rgb(100, 220, 255);
            let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
            let mode = if d.along_normals {
                let base = if d.along_normals_grouped {
                    "Along N · Group"
                } else {
                    "Along N · Indiv"
                };
                if d.force_planar {
                    format!("{base} · planar")
                } else {
                    base.into()
                }
            } else if d.world_axis {
                "World axis".into()
            } else if d.edge_axis.is_some() {
                "Edge axis".into()
            } else {
                "Region N".into()
            };
            let text = format!("Extrude {mode}: {:.3}", d.accumulated_offset);
            let pos = egui::pos2(rect.center().x, rect.min.y + 60.0);
            let galley = painter.layout_no_wrap(text.clone(), font.clone(), col);
            let tr = egui::Rect::from_min_size(egui::pos2(pos.x - galley.size().x * 0.5 - 4.0, pos.y - 2.0), egui::vec2(galley.size().x + 8.0, galley.size().y + 4.0));
            painter.rect_filled(tr, 3.0, bg);
            painter.text(pos, egui::Align2::CENTER_TOP, text, font, col);
        }
        // Poly inset drag HUD
        if let Some(d) = self.tool.current.as_poly_inset() {
            let font = egui::FontId::monospace(12.0);
            let col = egui::Color32::from_rgb(100, 220, 255);
            let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
            let text = format!("Inset: {:.3}", d.accumulated_amount);
            let pos = egui::pos2(rect.center().x, rect.min.y + 60.0);
            let galley = painter.layout_no_wrap(text.clone(), font.clone(), col);
            let tr = egui::Rect::from_min_size(egui::pos2(pos.x - galley.size().x * 0.5 - 4.0, pos.y - 2.0), egui::vec2(galley.size().x + 8.0, galley.size().y + 4.0));
            painter.rect_filled(tr, 3.0, bg);
            painter.text(pos, egui::Align2::CENTER_TOP, text, font, col);
        }
        // NURBS Fillet HUD
        if let Some(d) = self.tool.current.as_nurbs_fillet() {
            use core_math::nurbs_fillet::Radius;
            let font = egui::FontId::monospace(12.0);
            let col = egui::Color32::from_rgb(200, 140, 255);
            let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
            let law = match &d.radius {
                Radius::Const(r) => format!("Const r={r:.3}"),
                Radius::Linear { r0, r1 } => format!("Linear {r0:.3}→{r1:.3}"),
                Radius::Law(s) => format!("Law {}stn peak={:.3}", s.len(), d.radius.peak()),
            };
            let text = if d.num_buf.is_empty() {
                format!("{law}  (drag X · V=mode · N=stn · type · Enter · Esc)")
            } else {
                format!("{law}  [typed: {}]  (Enter · Esc · Backspace)", d.num_buf)
            };
            let pos = egui::pos2(rect.center().x, rect.min.y + 60.0);
            let galley = painter.layout_no_wrap(text.clone(), font.clone(), col);
            let tr = egui::Rect::from_min_size(
                egui::pos2(pos.x - galley.size().x * 0.5 - 4.0, pos.y - 2.0),
                egui::vec2(galley.size().x + 8.0, galley.size().y + 4.0),
            );
            painter.rect_filled(tr, 3.0, bg);
            painter.text(pos, egui::Align2::CENTER_TOP, text, font, col);
        }
        // NURBS G/R/S HUD (parity with poly transform feedback)
        if let Some(d) = self.tool.current.as_nurbs_cv_move() {
            let font = egui::FontId::monospace(12.0);
            let col = egui::Color32::from_rgb(200, 140, 255);
            let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
            let mode = match d.kind {
                crate::TransformKind::Grab => "Grab",
                crate::TransformKind::Rotate => "Rotate",
                crate::TransformKind::Scale | crate::TransformKind::Elongate => "Scale",
            };
            let free = if d.free_mode { " free" } else { "" };
            let axis = match d.axis {
                crate::AxisConstraint::X => " X",
                crate::AxisConstraint::Y => " Y",
                crate::AxisConstraint::Z => " Z",
                crate::AxisConstraint::None => "",
                _ => " plane",
            };
            let delta_text = format!("NURBS {mode}{free}{axis}");
            let pos = egui::pos2(rect.center().x, rect.min.y + 60.0);
            let galley = painter.layout_no_wrap(delta_text.clone(), font.clone(), col);
            let tr = egui::Rect::from_min_size(egui::pos2(pos.x - galley.size().x * 0.5 - 4.0, pos.y - 2.0), egui::vec2(galley.size().x + 8.0, galley.size().y + 4.0));
            painter.rect_filled(tr, 3.0, bg);
            painter.text(pos, egui::Align2::CENTER_TOP, delta_text, font, col);
        }
    }


    // ---- Smart Mirror tool: interactive plane + HUD ----
    pub(crate) fn draw_smart_mirror(&self, painter: &egui::Painter, rect: egui::Rect) {
        let st = match self.tool.current.as_smart_mirror() {
            Some(s) => s,
            None => return,
        };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        // Plane size scales with camera distance
        let half = self.viewport.camera.distance * 0.8;

        // Colors based on action mode
        let (plane_fill, plane_border, mode_label) = match st.action {
            crate::SmartMirrorAction::Mirror => (
                egui::Color32::from_rgba_unmultiplied(60, 180, 255, 30),
                egui::Color32::from_rgba_unmultiplied(60, 180, 255, 160),
                "Mirror",
            ),
            crate::SmartMirrorAction::Instance => (
                egui::Color32::from_rgba_unmultiplied(100, 255, 120, 30),
                egui::Color32::from_rgba_unmultiplied(100, 255, 120, 160),
                "Instance",
            ),
            crate::SmartMirrorAction::DeleteHalf => (
                egui::Color32::from_rgba_unmultiplied(255, 80, 80, 30),
                egui::Color32::from_rgba_unmultiplied(255, 80, 80, 160),
                "Delete Half",
            ),
        };

        // Draw plane quad
        let corners_3d = [
            st.plane_origin + st.tangent_u * half + st.tangent_v * half,
            st.plane_origin - st.tangent_u * half + st.tangent_v * half,
            st.plane_origin - st.tangent_u * half - st.tangent_v * half,
            st.plane_origin + st.tangent_u * half - st.tangent_v * half,
        ];
        let corners_2d: Vec<egui::Pos2> = corners_3d.iter()
            .filter_map(|&c| world_to_screen(c, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg))
            .collect();
        if corners_2d.len() == 4 {
            // Filled quad
            let mesh_pts: Vec<egui::epaint::Vertex> = corners_2d.iter().map(|p| egui::epaint::Vertex {
                pos: *p, uv: egui::epaint::WHITE_UV, color: plane_fill,
            }).collect();
            let indices = vec![0u32, 1, 2, 0, 2, 3];
            let mesh = egui::epaint::Mesh { indices, vertices: mesh_pts, texture_id: egui::TextureId::default() };
            painter.add(egui::Shape::mesh(mesh));
            // Border
            for i in 0..4 {
                painter.line_segment([corners_2d[i], corners_2d[(i + 1) % 4]],
                    egui::Stroke::new(2.0, plane_border));
            }
        }

        // Normal arrow from plane center
        let arrow_end = st.plane_origin + st.plane_normal * (half * 0.3);
        if let (Some(sp), Some(ep)) = (
            world_to_screen(st.plane_origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            world_to_screen(arrow_end, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
        ) {
            painter.arrow(sp, ep - sp, egui::Stroke::new(2.5, plane_border));
        }

        // Center dot
        if let Some(cp) = world_to_screen(st.plane_origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            painter.circle_filled(cp, 6.0, plane_border);
            painter.circle_stroke(cp, 6.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
        }

        // Axis color for HUD
        let axis_col = match st.snapped_axis {
            0 => egui::Color32::from_rgb(240, 80, 80),
            1 => egui::Color32::from_rgb(130, 220, 80),
            _ => egui::Color32::from_rgb(80, 140, 240),
        };
        let axis_name = match st.snapped_axis { 0 => "X", 1 => "Y", _ => "Z" };

        // Delete half preview: red X over objects to be deleted
        if st.action == crate::SmartMirrorAction::DeleteHalf {
            for &idx in &st.half_delete_indices {
                if let Some(prim) = self.scene.primitives.get(idx) {
                    if let Some(sp) = world_to_screen(prim.position, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                        let sz = 12.0;
                        let stroke = egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 60, 60));
                        painter.line_segment(
                            [egui::pos2(sp.x - sz, sp.y - sz), egui::pos2(sp.x + sz, sp.y + sz)], stroke);
                        painter.line_segment(
                            [egui::pos2(sp.x + sz, sp.y - sz), egui::pos2(sp.x - sz, sp.y + sz)], stroke);
                    }
                }
            }
        }

        // HUD (top-left of viewport)
        {
            let font = egui::FontId::monospace(12.0);
            let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200);
            let phase_str = match st.phase {
                crate::SmartMirrorPhase::Placing => "Placing",
                crate::SmartMirrorPhase::Adjusting => "Adjusting",
            };
            let line1 = format!("Smart Mirror | {phase_str} | Axis: {axis_name} | {mode_label}");
            let line2 = match st.phase {
                crate::SmartMirrorPhase::Placing => "LMB=Place  X/Y/Z=Axis  Esc=Cancel",
                crate::SmartMirrorPhase::Adjusting => "LMB=Confirm  Drag=Move  Scroll=Rotate  Ctrl=Snap",
            };
            let line3 = "Shift=Instance  Shift+Alt=Delete Half";
            let x = rect.left() + 10.0;
            let y = rect.top() + 10.0;
            let g1 = painter.layout_no_wrap(line1.clone(), font.clone(), egui::Color32::WHITE);
            let g2 = painter.layout_no_wrap(line2.to_string(), font.clone(), egui::Color32::from_gray(160));
            let g3 = painter.layout_no_wrap(line3.to_string(), font.clone(), egui::Color32::from_gray(130));
            let w = g1.size().x.max(g2.size().x).max(g3.size().x) + 16.0;
            let h = g1.size().y + g2.size().y + g3.size().y + 16.0;
            let hud_rect = egui::Rect::from_min_size(egui::pos2(x - 4.0, y - 4.0), egui::vec2(w, h));
            painter.rect_filled(hud_rect, 4.0, bg);
            painter.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, &line1, font.clone(), axis_col);
            painter.text(egui::pos2(x, y + g1.size().y + 2.0), egui::Align2::LEFT_TOP, line2, font.clone(),
                egui::Color32::from_gray(160));
            painter.text(egui::pos2(x, y + g1.size().y + g2.size().y + 4.0), egui::Align2::LEFT_TOP, line3,
                font, egui::Color32::from_gray(130));
        }
    }

    // draw_smart_snap_indicator + draw_smart_snap_bar moved to smart_snap.rs
}
