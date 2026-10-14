// extrude_sweep.rs — Shift+Alt+E Extrude Sweep
// Path from face-region center on workplane orientation → mouse (green point).
// Scroll = path segments (smoother duct turn). LMB = commit + exit.

use std::collections::HashSet;
use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{FaceId, VertexId, HalfEdgeMesh, NO_EDGE};
use core_math::scene_graph::NodePayload;
use crate::{
    AzraApp, ActiveTool, DirtyFlags, ModelingContext, PolyExtrudeSweep, PolyTool, PolyWorkMode,
    world_to_screen,
};

impl AzraApp {
    /// Shift+Alt+E — arm Extrude Sweep on the current face selection (single or multi).
    pub(crate) fn poly_start_extrude_sweep(&mut self, mouse: egui::Pos2) {
        if self.modeling_context != ModelingContext::Polygonal {
            return;
        }
        if !matches!(
            self.poly_work_mode,
            PolyWorkMode::Face | PolyWorkMode::Group
        ) {
            self.toast("Extrude Sweep: Face mode — select face(s) first");
            return;
        }
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.toast("Extrude Sweep: select a poly body");
                return;
            }
        };
        let faces: Vec<u32> = if !self.poly_sel.selected_faces.is_empty() {
            self.poly_sel.selected_faces.iter().copied().collect()
        } else {
            self.poly_faces_or_all(nid)
        };
        if faces.is_empty() {
            self.toast("Extrude Sweep: select face(s) first");
            return;
        }
        let pre_mesh = match self.scene.scene_graph.get(nid).and_then(|node| match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) {
            Some(m) => m,
            None => {
                self.toast("Extrude Sweep: active object is not a poly mesh");
                return;
            }
        };

        // Region center + average normal.
        let (face_center, avg_n) = {
            let mut c = Vec3::ZERO;
            let mut nsum = Vec3::ZERO;
            let mut count = 0u32;
            for &fid in &faces {
                if (fid as usize) >= pre_mesh.faces.len() {
                    continue;
                }
                if pre_mesh.face(FaceId(fid)).half_edge == NO_EDGE {
                    continue;
                }
                nsum += pre_mesh.compute_face_normal(FaceId(fid));
                for &v in &pre_mesh.face_vertices(FaceId(fid)) {
                    c += pre_mesh.vert(v).position;
                    count += 1;
                }
            }
            if count == 0 {
                self.toast("Extrude Sweep: no valid faces");
                return;
            }
            (c / count as f32, nsum.normalize_or_zero())
        };
        if avg_n.length_squared() < 0.25 {
            self.toast("Extrude Sweep: degenerate face normal");
            return;
        }

        // Guide plane: through face center, WP orientation (user never sees the path line).
        let wp_n = self.viewport.wp_resolved.normal.normalize_or_zero();
        let plane_n = if wp_n.length_squared() > 0.25 {
            wp_n
        } else {
            avg_n
        };
        let (plane_u, plane_v) = {
            let u = self.viewport.wp_resolved.tangent_u.normalize_or_zero();
            let v = self.viewport.wp_resolved.tangent_v.normalize_or_zero();
            if u.length_squared() > 0.25 && v.length_squared() > 0.25 {
                (u, v)
            } else {
                // Orthonormal basis from plane normal.
                let helper = if plane_n.y.abs() < 0.9 {
                    Vec3::Y
                } else {
                    Vec3::X
                };
                let u = plane_n.cross(helper).normalize_or_zero();
                let v = plane_n.cross(u).normalize_or_zero();
                (u, v)
            }
        };

        // Start direction: region normal projected onto guide plane when possible.
        let mut start_dir = avg_n - plane_n * avg_n.dot(plane_n);
        if start_dir.length_squared() < 0.05 {
            start_dir = plane_u; // face coplanar with WP — sweep in-plane along U
        }
        start_dir = start_dir.normalize_or_zero();
        if start_dir.length_squared() < 0.25 {
            start_dir = avg_n;
        }

        // Seed target slightly along start_dir so zero mouse still has a preview.
        let seed_target = face_center + start_dir * 0.25;

        self.begin_episode("Extrude Sweep");
        let orig_sel_faces = self.poly_sel.selected_faces.clone();
        let orig_sel_edges = self.poly_sel.selected_edges.clone();
        let orig_sel_vertices = self.poly_sel.selected_vertices.clone();

        self.start_operation(ActiveTool::PolyExtrudeSweep(PolyExtrudeSweep {
            node_id: nid,
            pre_mesh,
            orig_faces: faces,
            face_center,
            start_dir,
            plane_origin: face_center,
            plane_normal: plane_n,
            plane_u,
            plane_v,
            target: seed_target,
            segments: 4,
            locked_angle_deg: None,
            num_buf: String::new(),
            cap_faces: Vec::new(),
            orig_sel_faces,
            orig_sel_edges,
            orig_sel_vertices,
        }));
        self.set_poly_tool(PolyTool::ExtrudeFace);
        self.ui2.active_tool = "Extrude Sweep".into();
        self.ui2.active_pillar = "poly".into();

        // Initial preview at seed.
        let rect = self.viewport.viewport_rect;
        self.poly_update_extrude_sweep(mouse, rect, false);
        self.toast(
            "Extrude Sweep: move mouse · scroll=segments · type 45/90 · snap · LMB confirm+exit · Esc cancel",
        );
    }

    /// Path samples: quadratic Bezier face_center → (along start_dir) → target.
    /// `segments` = number of steps (1 = single prism; higher = smoother turn).
    pub(crate) fn extrude_sweep_path(
        center: Vec3,
        start_dir: Vec3,
        target: Vec3,
        segments: u32,
    ) -> Vec<Vec3> {
        let segs = segments.clamp(1, 32);
        let chord = target - center;
        let len = chord.length().max(1e-4);
        let sd = start_dir.normalize_or_zero();
        // Control point: halfway along start direction at half chord length → duct turn.
        let ctrl = center + sd * (len * 0.5);
        let mut pts = Vec::with_capacity(segs as usize + 1);
        pts.push(center);
        for i in 1..=segs {
            let t = i as f32 / segs as f32;
            let u = 1.0 - t;
            let p = center * (u * u) + ctrl * (2.0 * u * t) + target * (t * t);
            pts.push(p);
        }
        // Ensure last point is exactly the target.
        if let Some(last) = pts.last_mut() {
            *last = target;
        }
        pts
    }

    /// Rebuild mesh from pre_mesh by multi-step extrude along path (reorient each segment).
    fn extrude_sweep_apply_path(
        mesh: &mut HalfEdgeMesh,
        orig_faces: &[u32],
        path: &[Vec3],
    ) -> Vec<u32> {
        if path.len() < 2 || orig_faces.is_empty() {
            return orig_faces.to_vec();
        }
        let mut set: HashSet<FaceId> = orig_faces.iter().map(|&f| FaceId(f)).collect();
        for i in 0..path.len() - 1 {
            let delta = path[i + 1] - path[i];
            let dlen = delta.length();
            if dlen < 1e-7 {
                continue;
            }
            let dir = delta / dlen;
            // Reorient current caps so their normal tracks the segment (duct continuity).
            let avg_n = {
                let mut s = Vec3::ZERO;
                let mut n = 0u32;
                for &f in &set {
                    if (f.0 as usize) >= mesh.faces.len() {
                        continue;
                    }
                    if mesh.face(f).half_edge == NO_EDGE {
                        continue;
                    }
                    s += mesh.compute_face_normal(f);
                    n += 1;
                }
                if n > 0 {
                    s.normalize_or_zero()
                } else {
                    dir
                }
            };
            if avg_n.length_squared() > 0.25 {
                let axis = avg_n.cross(dir);
                let al = axis.length();
                if al > 1e-4 {
                    let ang = avg_n.dot(dir).clamp(-1.0, 1.0).acos();
                    if ang.abs() > 1e-4 {
                        let ax = axis / al;
                        let pivot = path[i];
                        // Rotate unique verts of current caps around pivot.
                        let mut verts: HashSet<u32> = HashSet::new();
                        for &f in &set {
                            if (f.0 as usize) >= mesh.faces.len() {
                                continue;
                            }
                            if mesh.face(f).half_edge == NO_EDGE {
                                continue;
                            }
                            for &v in &mesh.face_vertices(f) {
                                verts.insert(v.0);
                            }
                        }
                        let q = glam::Quat::from_axis_angle(ax, ang);
                        for vid in verts {
                            let v = VertexId(vid);
                            if (v.0 as usize) >= mesh.vertices.len() {
                                continue;
                            }
                            if mesh.vert(v).half_edge == NO_EDGE {
                                continue;
                            }
                            let p = mesh.vert(v).position;
                            mesh.vert_mut(v).position = pivot + q * (p - pivot);
                        }
                    }
                }
            }
            let caps = mesh.extrude_region(&set, delta);
            if caps.is_empty() {
                break;
            }
            set = caps.into_iter().collect();
        }
        let mut out: Vec<u32> = set.into_iter().map(|f| f.0).collect();
        out.sort_unstable();
        out
    }

    /// Per-frame: mouse → green point (length + angle); rebuild sweep preview.
    pub(crate) fn poly_update_extrude_sweep(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl_snap: bool,
    ) {
        let (
            nid,
            pre,
            faces,
            center,
            start_dir,
            plane_o,
            plane_n,
            plane_u,
            plane_v,
            segments,
            locked_angle,
        ) = {
            let d = match self.tool.current.as_poly_extrude_sweep() {
                Some(d) => d,
                None => return,
            };
            (
                d.node_id,
                d.pre_mesh.clone(),
                d.orig_faces.clone(),
                d.face_center,
                d.start_dir,
                d.plane_origin,
                d.plane_normal,
                d.plane_u,
                d.plane_v,
                d.segments,
                d.locked_angle_deg,
            )
        };

        // Mouse hit on guide plane through face center (WP orientation).
        let hit = self
            .ray_plane_hit(mouse, rect, plane_o, plane_n)
            .unwrap_or(center + start_dir * 0.25);

        // Free direction in plane from center → hit.
        let mut to = hit - center;
        let mut len = to.length();
        if len < 1e-4 {
            to = start_dir;
            len = 0.25;
        }
        // Project to plane.
        to = to - plane_n * to.dot(plane_n);
        if to.length_squared() < 1e-8 {
            to = start_dir - plane_n * start_dir.dot(plane_n);
        }
        let mut dir = to.normalize_or_zero();
        if dir.length_squared() < 0.25 {
            dir = start_dir;
        }

        // Angle in plane from start_dir (signed via plane normal).
        let sd = {
            let s = start_dir - plane_n * start_dir.dot(plane_n);
            if s.length_squared() > 1e-6 {
                s.normalize()
            } else {
                plane_u
            }
        };
        let free_angle = {
            let x = dir.dot(sd);
            let y = dir.dot(plane_n.cross(sd));
            y.atan2(x).to_degrees()
        };

        let mut angle_deg = locked_angle.unwrap_or(free_angle);
        // Viewport angle snap when snap is active and angle not typed-locked.
        if locked_angle.is_none() {
            let snap_deg = self.viewport.snap_rotation_deg;
            if (ctrl_snap || self.is_snap_active(ctrl_snap)) && snap_deg > 0.05 {
                angle_deg = (angle_deg / snap_deg).round() * snap_deg;
            }
        }
        // Rebuild direction from angle; length from mouse (or keep length if locked).
        let rad = angle_deg.to_radians();
        let cos = rad.cos();
        let sin = rad.sin();
        let dir_locked = (sd * cos + plane_n.cross(sd) * sin).normalize_or_zero();
        let final_dir = if dir_locked.length_squared() > 0.25 {
            dir_locked
        } else {
            dir
        };
        // Length: always from mouse distance projected on final_dir (or raw hit distance).
        let length = if locked_angle.is_some() {
            // Mouse distance along free ray still drives length.
            (hit - center).dot(final_dir).abs().max(1e-3)
        } else {
            len.max(1e-3)
        };
        let target = center + final_dir * length;

        let path = Self::extrude_sweep_path(center, start_dir, target, segments);

        // Restore pre_mesh and rebuild.
        let caps = {
            let Some(node) = self.scene.scene_graph.get_mut(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload else {
                return;
            };
            *mesh = pre.clone();
            let mesh = Arc::make_mut(mesh);
            Self::extrude_sweep_apply_path(mesh, &faces, &path)
        };

        self.poly_sel.selected_faces.clear();
        for &f in &caps {
            self.poly_sel.selected_faces.insert(f);
        }
        if let Some(d) = self.tool.current.as_poly_extrude_sweep_mut() {
            d.target = target;
            d.cap_faces = caps;
        }
        self.poly_sync_after_position_edit(nid);
        // Topology changed — full mesh sync for display.
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_extrude_sweep_scroll(&mut self, wheel: f32) {
        let d = match self.tool.current.as_poly_extrude_sweep_mut() {
            Some(d) => d,
            None => return,
        };
        if wheel > 0.0 {
            d.segments = (d.segments + 1).min(32);
        } else if wheel < 0.0 {
            d.segments = d.segments.saturating_sub(1).max(1);
        }
        let segs = d.segments;
        self.toast(&format!("Extrude Sweep: {segs} path segments"));
        let mouse = self.viewport.viewport_mouse;
        let rect = self.viewport.viewport_rect;
        self.poly_update_extrude_sweep(mouse, rect, false);
    }

    pub(crate) fn poly_extrude_sweep_num_input(&mut self, ch: char) {
        let d = match self.tool.current.as_poly_extrude_sweep_mut() {
            Some(d) => d,
            None => return,
        };
        if ch == '\x08' {
            d.num_buf.pop();
        } else if ch.is_ascii_digit() || ch == '.' || ch == '-' {
            if d.num_buf.len() < 8 {
                d.num_buf.push(ch);
            }
        } else {
            return;
        }
        if let Ok(a) = d.num_buf.parse::<f32>() {
            d.locked_angle_deg = Some(a);
        }
        let mouse = self.viewport.viewport_mouse;
        let rect = self.viewport.viewport_rect;
        self.poly_update_extrude_sweep(mouse, rect, false);
    }

    /// LMB: commit + exit (one-shot).
    pub(crate) fn poly_confirm_extrude_sweep(&mut self) {
        let (nid, caps, target, center) = {
            let d = match self.tool.current.as_poly_extrude_sweep() {
                Some(d) => d,
                None => return,
            };
            (
                d.node_id,
                d.cap_faces.clone(),
                d.target,
                d.face_center,
            )
        };
        let extent = (target - center).length();
        if extent < 1e-4 || caps.is_empty() {
            self.poly_cancel_extrude_sweep();
            self.toast("Extrude Sweep: too small — cancelled");
            return;
        }
        self.poly_sel.selected_faces.clear();
        for &f in &caps {
            self.poly_sel.selected_faces.insert(f);
        }
        self.commit_episode();
        self.poly_flush_mesh_dirty();
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.ui2.active_tool.clear();
        self.toast("Extrude Sweep confirmed");
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_cancel_extrude_sweep(&mut self) {
        let restore = self.tool.current.as_poly_extrude_sweep().map(|d| {
            (
                d.node_id,
                d.pre_mesh.clone(),
                d.orig_sel_faces.clone(),
                d.orig_sel_edges.clone(),
                d.orig_sel_vertices.clone(),
            )
        });
        if let Some((nid, pre, f, e, v)) = restore {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = pre.clone();
                }
            }
            self.poly_sel.selected_faces = f;
            self.poly_sel.selected_edges = e;
            self.poly_sel.selected_vertices = v;
            self.poly_sync_mesh_cache(nid);
        }
        let _ = self.pending_episode.take();
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.ui2.active_tool.clear();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Draw green target point only (path line stays invisible per design).
    pub(crate) fn draw_extrude_sweep_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let d = match self.tool.current.as_poly_extrude_sweep() {
            Some(d) => d,
            None => return,
        };
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let green = egui::Color32::from_rgb(80, 255, 120);
        if let Some(sp) = world_to_screen(
            d.target,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        ) {
            painter.circle_filled(sp, 6.0, green);
            painter.circle_stroke(sp, 6.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
        }
        // HUD
        let ang = d.locked_angle_deg.map(|a| format!("{a:.0}°")).unwrap_or_else(|| {
            let sd = d.start_dir;
            let to = (d.target - d.face_center).normalize_or_zero();
            let x = to.dot(sd);
            let y = to.dot(d.plane_normal.cross(sd));
            format!("{:.0}°", y.atan2(x).to_degrees())
        });
        let len = (d.target - d.face_center).length();
        let num = if d.num_buf.is_empty() {
            String::new()
        } else {
            format!(" · type {}", d.num_buf)
        };
        painter.text(
            egui::pos2(rect.center().x, rect.min.y + 48.0),
            egui::Align2::CENTER_TOP,
            format!(
                "Extrude Sweep  {ang}  L={len:.3}  segs={}{num}",
                d.segments
            ),
            egui::FontId::proportional(13.0),
            green,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_path_starts_at_center_ends_at_target() {
        let c = Vec3::ZERO;
        let t = Vec3::new(2.0, 0.0, 1.0);
        let path = AzraApp::extrude_sweep_path(c, Vec3::Y, t, 6);
        assert_eq!(path.len(), 7);
        assert!((path[0] - c).length() < 1e-5);
        assert!((path[path.len() - 1] - t).length() < 1e-5);
    }

    #[test]
    fn sweep_path_one_segment_is_chord() {
        let c = Vec3::ZERO;
        let t = Vec3::new(1.0, 0.0, 0.0);
        let path = AzraApp::extrude_sweep_path(c, Vec3::Y, t, 1);
        assert_eq!(path.len(), 2);
        assert!((path[1] - t).length() < 1e-5);
    }
}
