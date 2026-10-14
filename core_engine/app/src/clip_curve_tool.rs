//! Clip Curve drawing tool — interactive polygon drawing on the workplane.
//!
//! C key enters draw mode. Click to place polygon vertices on the workplane.
//! Enter/double-click closes the polygon and applies the clip to the selected prim.
//! Escape cancels. The polygon is stored on SdfPrimitive as clip_curve_points.

use glam::{Vec2, Vec3};
use core_math::primitives::{CurveVertex, CurvePlane, CurveProfile, SdfShape};
use crate::*;

impl AzraApp {
    /// Enter clip curve draw mode for the active selected prim or body.
    /// When a body is selected, the clip will be applied to all visible prims in it on commit.
    /// Captures the current workplane as the drawing plane.
    pub(crate) fn start_clip_curve_draw(&mut self) -> bool {
        // Determine target: selected prims, or all visible prims in selected body
        let sel = self.selected_prim_indices_unified();
        let prim_idx = if !sel.is_empty() {
            sel[0]
        } else if self.sel.usel.has_nodes() {
            // Body mode: find first visible prim in selected body as reference
            let sel_nodes: Vec<core_math::scene_graph::NodeId> = self.sel.usel.selected_nodes().collect();
            let mut found = None;
            for &nid in &sel_nodes {
                for (i, &(n, _)) in self.scene.prim_body_map.iter().enumerate() {
                    if n == nid && self.scene.primitives[i].visible { found = Some(i); break; }
                }
                if found.is_some() { break; }
            }
            match found {
                Some(idx) => idx,
                None => { self.toast("No visible prims in selected body"); return false; }
            }
        } else {
            self.toast("Select a primitive or body first");
            return false;
        };
        if prim_idx >= self.scene.primitives.len() { return false; }

        // Capture the workplane frame (world space)
        let wp = &self.viewport.wp_resolved;
        let state = ClipCurveDrawState {
            prim_idx,
            plane_origin: wp.origin,
            plane_normal: wp.normal,
            plane_u: wp.tangent_u,
            plane_v: wp.tangent_v,
            verts: Vec::new(),
            cursor_2d: Vec2::ZERO,
            cursor_valid: false,
            create_prim: false,
        };
        self.start_operation(ActiveTool::ClipCurveDraw(state));
        self.toast(
            "Clip curve: click points · Ctrl=H/V · Shift=angle snap · Enter=close · Esc=cancel",
        );
        true
    }

    /// Enter POLYLINE draw mode — same point-placement machinery as the clip-curve tool
    /// (`clip_curve_add_vertex` / `clip_curve_update_cursor` / overlay / Enter-commit / Esc-RMB
    /// cancel are all shared), but on commit it CREATES a new CurveExtrude primitive from the
    /// drawn profile instead of clipping an existing prim. No prim selection required.
    pub(crate) fn start_polyline_draw(&mut self) -> bool {
        let wp = &self.viewport.wp_resolved;
        let state = ClipCurveDrawState {
            prim_idx: usize::MAX, // unused in create_prim mode
            plane_origin: wp.origin,
            plane_normal: wp.normal,
            plane_u: wp.tangent_u,
            plane_v: wp.tangent_v,
            verts: Vec::new(),
            cursor_2d: Vec2::ZERO,
            cursor_valid: false,
            create_prim: true,
        };
        self.start_operation(ActiveTool::ClipCurveDraw(state));
        let ang = self.viewport.snap_rotation_deg;
        self.toast(&format!(
            "Draw Polyline: click points · Ctrl=H/V · Shift=angle ({ang:.0}°) · Enter=create · Esc=cancel"
        ));
        true
    }

    /// Snap a draw-cursor / placed vertex relative to the last point.
    ///
    /// - **Ctrl**: horizontal or vertical (whichever is closer) — pure axis lock.
    /// - **Shift**: angle snap to `snap_rotation_deg` (viewport snap, default 15°).
    /// - **Ctrl+Shift**: H/V first (axis wins over general angle).
    /// - First point (no prev): no snap.
    fn clip_curve_apply_snap(&self, pos: Vec2, ctrl: bool, shift: bool) -> (Vec2, u8) {
        let prev = match self.tool.current.as_clip_curve_draw() {
            Some(s) if !s.verts.is_empty() => s.verts.last().unwrap().pos,
            _ => return (pos, 0),
        };
        if ctrl {
            // Axis lock: project onto H or V from prev.
            let d = pos - prev;
            let snapped = if d.x.abs() >= d.y.abs() {
                Vec2::new(pos.x, prev.y) // horizontal
            } else {
                Vec2::new(prev.x, pos.y) // vertical
            };
            return (snapped, 1); // 1 = axis
        }
        if shift {
            let inc = self.viewport.snap_rotation_deg;
            if inc < 0.5 {
                return (pos, 0);
            }
            let v = pos - prev;
            let len = v.length();
            if len < 1e-6 {
                return (pos, 0);
            }
            let step = inc.to_radians();
            let ang = (v.y.atan2(v.x) / step).round() * step;
            let snapped = prev + Vec2::new(ang.cos(), ang.sin()) * len;
            return (snapped, 2); // 2 = angle
        }
        (pos, 0)
    }

    /// Commit a drawn polyline as a NEW CurveExtrude primitive sitting on the draw plane.
    ///
    /// CurveExtrude profiles live in local XZ with extrusion along local Y. We build a prim
    /// frame from the captured workplane: **X = plane_u**, **Y = plane_normal**, **Z = plane_v**,
    /// place the prim at the plane origin, and keep UV coords as local (X,Z). That way the solid
    /// lands exactly on the workplane the user drew on — Auto or Locked, any orientation.
    fn polyline_create_commit(&mut self) {
        let (verts, plane_origin, plane_u, plane_v, plane_normal) = {
            let state = match self.tool.current.as_clip_curve_draw() { Some(s) => s, None => return };
            if state.verts.len() < 3 { self.toast("Need at least 3 points"); return; }
            (state.verts.clone(), state.plane_origin, state.plane_u, state.plane_v, state.plane_normal)
        };
        // UV already in plane frame → profile XZ (no re-projection needed when R matches frame).
        let profile_verts: Vec<CurveVertex> = verts.iter().map(|v| {
            CurveVertex { pos: Vec2::new(v.pos.x, v.pos.y), bevel_radius: v.bevel_radius }
        }).collect();
        let shape = SdfShape::CurveExtrude {
            profile: CurveProfile { verts: profile_verts, plane: CurvePlane::default() },
            half_depth: 0.4, wall_t: 0.0, rim: 0.0,
            open: false, thickness: 0.0, tube: false,
        };
        // Full plane frame → Euler (YXZ), not just normal-align (that dropped U/V twist).
        let rot_deg = Self::euler_from_workplane_frame(plane_u, plane_normal, plane_v);
        // World plane origin → body-local position when BODY_LOCAL_PRIMS is on.
        let pos_local = if Self::BODY_LOCAL_PRIMS {
            let bt = self.scene.scene_graph.body_world_transform(self.scene.active_body_nid)
                .unwrap_or_default();
            bt.world_to_local(plane_origin)
        } else {
            plane_origin
        };

        self.tool.current = ActiveTool::Idle;

        // Create with explicit plane pose (add_primitive_immediate only applies WP rot when Locked).
        if self.sel.usel.active_node().map(|nid| {
            self.scene.scene_graph.get(nid)
                .map(|n| matches!(&n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                .unwrap_or(false)
        }).unwrap_or(false) {
            if let Some(nid) = self.sel.usel.active_node() {
                self.scene.active_body_nid = nid;
            }
        }
        let uid = self.scene.next_prim_id;
        let name = self.next_name("Curve");
        let op = self.tool.create_op.to_boolean_op(self.tool.create_blend_k);
        let mut prim = core_math::primitives::SdfPrimitive::with_uid(name, shape, pos_local, uid);
        prim.rotation_deg = rot_deg;
        prim.operation = op;
        prim.material_id = self.inherited_material_for_create();
        let n_pts = verts.len();
        self.transaction("Add Polyline", |s| {
            let idx = s.add_prim_to_graph(prim);
            s.select_single(idx);
            if s.modeling_context == ModelingContext::Sdf && s.tool.work_mode != WorkMode::Object {
                s.switch_work_mode(WorkMode::Object);
            }
            s.tool.gizmo_armed = true;
            s.rebake_no_undo();
        });
        self.toast(&format!("Polyline created ({} points) on workplane", n_pts));
    }

    /// While no points yet, keep the draw plane glued to the live workplane so Auto/Locked
    /// orientation stays correct if the camera or Home-lock changes before the first click.
    fn clip_curve_sync_plane_from_workplane(&mut self) {
        let live = {
            let Some(state) = self.tool.current.as_clip_curve_draw() else { return };
            if !state.verts.is_empty() { return; }
            true
        };
        if !live { return; }
        let wp = self.viewport.wp_resolved;
        if let Some(state) = self.tool.current.as_clip_curve_draw_mut() {
            state.plane_origin = wp.origin;
            state.plane_normal = wp.normal;
            state.plane_u = wp.tangent_u;
            state.plane_v = wp.tangent_v;
        }
    }

    /// Project a mouse position onto the clip curve drawing plane.
    /// Returns the 2D coordinates in the plane's local UV frame.
    pub(crate) fn clip_curve_project_mouse(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<Vec2> {
        let state = self.tool.current.as_clip_curve_draw()?;
        let world_hit = self.ray_plane_hit(mouse, rect, state.plane_origin, state.plane_normal)?;
        let rel = world_hit - state.plane_origin;
        Some(Vec2::new(rel.dot(state.plane_u), rel.dot(state.plane_v)))
    }

    /// Add a vertex to the polygon being drawn.
    /// `ctrl` = H/V axis snap from last point; `shift` = angle snap (`snap_rotation_deg`).
    pub(crate) fn clip_curve_add_vertex(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
        shift: bool,
    ) {
        self.clip_curve_sync_plane_from_workplane();
        if let Some(pos_2d) = self.clip_curve_project_mouse(mouse, rect) {
            let (pos_2d, _) = self.clip_curve_apply_snap(pos_2d, ctrl, shift);
            if let Some(state) = self.tool.current.as_clip_curve_draw_mut() {
                state.verts.push(CurveVertex {
                    pos: pos_2d,
                    bevel_radius: 0.0,
                });
            }
        }
    }

    /// Update the cursor preview position (called every frame during draw).
    /// Same snap modifiers as [`clip_curve_add_vertex`] so the rubber-band matches the click.
    pub(crate) fn clip_curve_update_cursor(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
        shift: bool,
    ) {
        self.clip_curve_sync_plane_from_workplane();
        let proj = self.clip_curve_project_mouse(mouse, rect);
        let snapped = proj.map(|p| self.clip_curve_apply_snap(p, ctrl, shift).0);
        if let Some(state) = self.tool.current.as_clip_curve_draw_mut() {
            if let Some(p) = snapped {
                state.cursor_2d = p;
                state.cursor_valid = true;
            } else {
                state.cursor_valid = false;
            }
        }
    }

    /// Close the polygon and apply clip curve to the target prim.
    /// Requires at least 3 vertices.
    pub(crate) fn clip_curve_commit(&mut self) {
        // Polyline authoring mode: create a new prim instead of clipping an existing one.
        if self.tool.current.as_clip_curve_draw().map_or(false, |s| s.create_prim) {
            self.polyline_create_commit();
            return;
        }
        let (prim_idx, verts, plane_origin, plane_normal, plane_u, plane_v) = {
            let state = match self.tool.current.as_clip_curve_draw() {
                Some(s) => s,
                None => return,
            };
            if state.verts.len() < 3 {
                self.toast("Need at least 3 points");
                return;
            }
            (state.prim_idx, state.verts.clone(),
             state.plane_origin, state.plane_normal, state.plane_u, state.plane_v)
        };

        if prim_idx >= self.scene.primitives.len() {
            self.tool.current = ActiveTool::Idle;
            return;
        }

        // Determine which prims to apply the clip to:
        // - If prims were selected: just those prims
        // - If body was selected: all visible prims in the body
        let sel_prims = self.selected_prim_indices_unified();
        let target_indices: Vec<usize> = if !sel_prims.is_empty() {
            sel_prims
        } else if self.sel.usel.has_nodes() {
            let sel_nodes: Vec<core_math::scene_graph::NodeId> = self.sel.usel.selected_nodes().collect();
            (0..self.scene.primitives.len())
                .filter(|&i| {
                    self.scene.prim_body_map.get(i).map_or(false, |&(n, _)| sel_nodes.contains(&n))
                    && self.scene.primitives[i].visible
                })
                .collect()
        } else {
            vec![prim_idx]
        };

        if target_indices.is_empty() {
            self.tool.current = ActiveTool::Idle;
            return;
        }

        // Build a ClipCurve per target prim (each needs its own local-space transform)
        let clips: Vec<(usize, core_math::primitives::ClipCurve)> = target_indices.iter().filter_map(|&idx| {
            if idx >= self.scene.primitives.len() { return None; }
            let prim = &self.scene.primitives[idx];
            let inv_rot = prim.rotation_quat().inverse();
            let inv_s = Vec3::new(1.0 / prim.scale.x.max(1e-6), 1.0 / prim.scale.y.max(1e-6), 1.0 / prim.scale.z.max(1e-6));

            let lo = inv_s * (inv_rot * (plane_origin - prim.position));
            let ln = (inv_s * (inv_rot * plane_normal)).normalize_or_zero();
            let lu = (inv_s * (inv_rot * plane_u)).normalize_or_zero();
            let lv = (inv_s * (inv_rot * plane_v)).normalize_or_zero();

            let sv: Vec<CurveVertex> = verts.iter().map(|v| {
                let wo = plane_u * v.pos.x + plane_v * v.pos.y;
                let loc = inv_s * (inv_rot * wo);
                CurveVertex { pos: Vec2::new(loc.dot(lu), loc.dot(lv)), bevel_radius: v.bevel_radius }
            }).collect();

            Some((idx, core_math::primitives::ClipCurve {
                profile: CurveProfile { verts: sv, plane: CurvePlane { origin: lo, normal: ln, tangent_u: lu, tangent_v: lv } },
                mode: 0, depth: 0.0, enabled: true,
            }))
        }).collect();

        let count = clips.len();
        // No targets resolved to a valid clip → exit without empty undo.
        if count == 0 {
            self.tool.current = ActiveTool::Idle;
            self.toast("Clip curve: no valid targets");
            return;
        }
        self.transaction("Clip Curve", |s| {
            for (idx, clip) in clips {
                s.write_prim(idx, |p| { p.clip_curves.push(clip); });
            }
            s.mark_dirty(DirtyFlags::RENDER);
        });

        self.tool.current = ActiveTool::Idle;
        let label = if count > 1 { format!("Clip curve applied to {} prims ({} pts)", count, verts.len()) }
                    else { format!("Clip curve applied ({} points)", verts.len()) };
        self.toast(&label);
    }

    /// Draw the clip curve overlay (polygon outline, vertices, cursor preview).
    pub(crate) fn draw_clip_curve_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let state = match self.tool.current.as_clip_curve_draw() {
            Some(s) => s,
            None => return,
        };

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_orbit = self.viewport.camera.orbit;
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance * (self.render.fov_deg * 0.5).to_radians().tan() } else { 0.0 };

        let to_screen = |pos_2d: Vec2| -> Option<egui::Pos2> {
            let world = state.plane_origin
                + state.plane_u * pos_2d.x
                + state.plane_v * pos_2d.y;
            world_to_screen(world, cam_pos, cam_orbit, rect, aspect, ortho_hw, self.render.fov_deg)
        };

        let vert_color = egui::Color32::from_rgb(0, 200, 255);
        let edge_color = egui::Color32::from_rgba_unmultiplied(0, 200, 255, 180);
        let preview_color = egui::Color32::from_rgba_unmultiplied(0, 200, 255, 100);

        // Draw placed edges
        for i in 0..state.verts.len() {
            let j = (i + 1) % state.verts.len();
            if j == 0 && state.verts.len() < 3 { continue; } // don't close until 3+ verts
            if let (Some(a), Some(b)) = (to_screen(state.verts[i].pos), to_screen(state.verts[j].pos)) {
                if j != 0 || state.verts.len() >= 3 {
                    // Only draw closing edge as dashed hint
                    let color = if j == 0 { preview_color } else { edge_color };
                    painter.line_segment([a, b], egui::Stroke::new(2.0, color));
                }
            }
        }

        // Draw vertices as dots
        for v in &state.verts {
            if let Some(sp) = to_screen(v.pos) {
                painter.circle_filled(sp, 4.0, vert_color);
            }
        }

        // Draw preview edge from last vertex to cursor (orange when axis/angle snap is active).
        if state.cursor_valid && !state.verts.is_empty() {
            let last = state.verts.last().unwrap().pos;
            let d = state.cursor_2d - last;
            let axis_snap = d.x.abs() < 1e-4 || d.y.abs() < 1e-4;
            // Angle-ish: non-zero edge that is near a snap_rotation multiple.
            let ang_deg = d.y.atan2(d.x).to_degrees();
            let step = self.viewport.snap_rotation_deg.max(1.0);
            let nearest = (ang_deg / step).round() * step;
            let angle_snap = !axis_snap && d.length() > 1e-4 && (ang_deg - nearest).abs() < 0.6;
            let preview_stroke = if axis_snap {
                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 180, 60)) // H/V
            } else if angle_snap {
                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 140, 80)) // angle
            } else {
                egui::Stroke::new(1.5, preview_color)
            };
            if let (Some(a), Some(b)) = (to_screen(last), to_screen(state.cursor_2d)) {
                painter.line_segment([a, b], preview_stroke);
                // Cursor gem
                let gem = if axis_snap {
                    egui::Color32::from_rgb(255, 180, 60)
                } else if angle_snap {
                    egui::Color32::from_rgb(255, 140, 80)
                } else {
                    vert_color
                };
                painter.circle_filled(b, 4.5, gem);
            }
            // Also draw from cursor to first vertex (closing preview)
            if state.verts.len() >= 2 {
                let first = state.verts[0].pos;
                if let (Some(a), Some(b)) = (to_screen(state.cursor_2d), to_screen(first)) {
                    painter.line_segment([a, b], egui::Stroke::new(1.0, preview_color));
                }
            }
        }

        // HUD: snap keys (bottom of viewport).
        let ang = self.viewport.snap_rotation_deg;
        let mode = if state.create_prim {
            "Draw Polyline"
        } else {
            "Clip Curve"
        };
        painter.text(
            egui::pos2(rect.center().x, rect.max.y - 28.0),
            egui::Align2::CENTER_BOTTOM,
            format!(
                "{mode}: LMB place · Ctrl=H/V · Shift=angle ({ang:.0}°) · Enter commit · Esc/RMB cancel"
            ),
            egui::FontId::proportional(13.0),
            egui::Color32::from_rgba_unmultiplied(220, 230, 240, 220),
        );
    }
}
