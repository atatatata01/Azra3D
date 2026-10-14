//! Alt+A Align / Flatten pie (poly edit) — view-direction flatten + workplane.
//!
//! Interaction lives in `input_keyboard/handle.rs` using the **same hold-pie template**
//! as Z / boolean / T pies: open on key *press*, hover while held, confirm on key
//! *release* via `PieMenuState::release()`. Holding never re-triggers open or apply.

use std::collections::HashSet;
use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, PolyWorkMode, DirtyFlags};

/// Screen directions for 4 slices (match `sector_center_angle(i, 4)` / sym pie).
const SLICE_SCREEN_DIRS: [egui::Vec2; 4] = [
    egui::vec2(0.0, -1.0), // up
    egui::vec2(1.0, 0.0),  // right
    egui::vec2(0.0, 1.0),  // down
    egui::vec2(-1.0, 0.0), // left
];

impl AzraApp {
    /// True when Alt+A pie is allowed (poly edit + 2+ verts from selection).
    pub(crate) fn align_pie_can_open(&self) -> bool {
        if self.modeling_context != crate::ModelingContext::Polygonal {
            return false;
        }
        if self.poly_work_mode == PolyWorkMode::Object {
            return false;
        }
        let Some(nid) = self.active_poly_nid() else {
            return false;
        };
        self.poly_align_gather_verts(nid).len() >= 2
    }

    /// Slice idx → view-relative flatten axis (screen up/right/down/left).
    pub(crate) fn apply_align_pie(&mut self, idx: usize) {
        let dir = SLICE_SCREEN_DIRS
            .get(idx)
            .copied()
            .unwrap_or(egui::vec2(1.0, 0.0));
        // Camera basis: right = +X, up = +Y in orbit space (egui up is -Y screen).
        let orbit = self.viewport.camera.orbit;
        let cam_right = (orbit * Vec3::X).normalize_or_zero();
        let cam_up = (orbit * Vec3::Y).normalize_or_zero();
        // screen_dir.x → right, screen_dir.y → down → −up
        let axis = (cam_right * dir.x + cam_up * (-dir.y)).normalize_or_zero();
        if axis.length_squared() < 0.25 {
            self.toast("Align: bad view axis");
            return;
        }
        let label = match idx {
            0 => "Align Top",
            1 => "Align Right",
            2 => "Align Bottom",
            3 => "Align Left",
            _ => "Align",
        };
        self.poly_flatten_along_world_axis(axis, label);
    }

    /// Project selection onto the active workplane (center of pie).
    pub(crate) fn apply_align_workplane(&mut self) {
        let wp = self.viewport.wp_resolved;
        let n = wp.normal.normalize_or_zero();
        if n.length_squared() < 0.25 {
            self.toast("Align: no workplane");
            return;
        }
        // Flatten onto plane: remove component of (p - origin) along normal.
        self.poly_flatten_to_plane(wp.origin, n, "Flatten to Workplane");
    }

    /// Gather verts like To Sphere (mode selection; empty = all in edit modes).
    pub(crate) fn poly_align_gather_verts(&self, nid: NodeId) -> Vec<u32> {
        let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh,
            _ => return Vec::new(),
        };
        let mode_empty = match self.poly_work_mode {
            PolyWorkMode::Vertex => self.poly_sel.selected_vertices.is_empty(),
            PolyWorkMode::Edge => self.poly_sel.selected_edges.is_empty(),
            PolyWorkMode::Face | PolyWorkMode::Group => self.poly_sel.selected_faces.is_empty(),
            PolyWorkMode::Object => return Vec::new(),
        };
        if mode_empty {
            return self.poly_verts_or_all(nid);
        }
        let mut set: HashSet<u32> = HashSet::new();
        match self.poly_work_mode {
            PolyWorkMode::Vertex => set.extend(self.poly_sel.selected_vertices.iter().copied()),
            PolyWorkMode::Edge => {
                for &e in &self.poly_sel.selected_edges {
                    let h = HalfEdgeId(e);
                    if !mesh.is_edge_live(h) {
                        continue;
                    }
                    set.insert(mesh.he_source(h).0);
                    set.insert(mesh.he(h).vertex.0);
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                for &f in &self.poly_sel.selected_faces {
                    if (f as usize) >= mesh.faces.len() {
                        continue;
                    }
                    if mesh.face(FaceId(f)).half_edge == NO_EDGE {
                        continue;
                    }
                    for v in mesh.face_vertices(FaceId(f)) {
                        set.insert(v.0);
                    }
                }
            }
            PolyWorkMode::Object => {}
        }
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// Flatten selected verts onto plane through selection centroid with normal `axis_world`.
    /// (Blender-style scale-to-0 on that axis.)
    pub(crate) fn poly_flatten_along_world_axis(&mut self, axis_world: Vec3, label: &str) {
        let axis_w = axis_world.normalize_or_zero();
        if axis_w.length_squared() < 0.25 {
            return;
        }
        let Some(nid) = self.active_poly_nid() else { return };
        let verts = self.poly_align_gather_verts(nid);
        if verts.len() < 2 {
            self.toast("Align: need 2+ verts");
            return;
        }
        let world = self.scene.scene_graph.world_transform(nid);
        let inv = world.inverse();
        // Axis in mesh local.
        let axis_l = inv.transform_vector3(axis_w).normalize_or_zero();
        if axis_l.length_squared() < 0.25 {
            return;
        }

        let mut centroid = Vec3::ZERO;
        let positions: Vec<(u32, Vec3)> = {
            let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::PolygonalBody { mesh, .. }) => mesh,
                _ => return,
            };
            let mut out = Vec::with_capacity(verts.len());
            for &vid in &verts {
                let v = VertexId(vid);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                let p = mesh.vert(v).position;
                centroid += p;
                out.push((vid, p));
            }
            out
        };
        if positions.len() < 2 {
            return;
        }
        centroid /= positions.len() as f32;

        let label_owned = label.to_string();
        let n = positions.len();
        self.transaction(&label_owned, |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    for &(vid, p) in &positions {
                        let d = (p - centroid).dot(axis_l);
                        mesh.vert_mut(VertexId(vid)).position = p - axis_l * d;
                    }
                    mesh.recompute_normals();
                }
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast(&format!("{label_owned}: {n} verts"));
        });
    }

    /// Project verts onto plane (origin_world, normal_world).
    pub(crate) fn poly_flatten_to_plane(
        &mut self,
        origin_world: Vec3,
        normal_world: Vec3,
        label: &str,
    ) {
        let n_w = normal_world.normalize_or_zero();
        if n_w.length_squared() < 0.25 {
            return;
        }
        let Some(nid) = self.active_poly_nid() else { return };
        let verts = self.poly_align_gather_verts(nid);
        if verts.is_empty() {
            self.toast("Align: no verts");
            return;
        }
        let world = self.scene.scene_graph.world_transform(nid);
        let inv = world.inverse();
        let origin_l = inv.transform_point3(origin_world);
        let n_l = inv.transform_vector3(n_w).normalize_or_zero();
        if n_l.length_squared() < 0.25 {
            return;
        }

        let positions: Vec<(u32, Vec3)> = {
            let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::PolygonalBody { mesh, .. }) => mesh,
                _ => return,
            };
            verts
                .iter()
                .filter_map(|&vid| {
                    let v = VertexId(vid);
                    if !mesh.is_vert_live(v) {
                        return None;
                    }
                    Some((vid, mesh.vert(v).position))
                })
                .collect()
        };
        if positions.is_empty() {
            return;
        }
        let n = positions.len();
        let label_owned = label.to_string();
        self.transaction(&label_owned, |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    for &(vid, p) in &positions {
                        let d = (p - origin_l).dot(n_l);
                        mesh.vert_mut(VertexId(vid)).position = p - n_l * d;
                    }
                    mesh.recompute_normals();
                }
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast(&format!("{label_owned}: {n} verts"));
        });
    }
}
