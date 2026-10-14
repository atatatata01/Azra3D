//! Blender Spin — lathe selected verts/edges around an axis (Ctrl+Alt+R).

use std::collections::HashSet;
use std::sync::Arc;
use glam::Vec3;
use core_math::half_edge::VertexId;
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

/// Default lathe: 8 steps, full turn, merge ends.
const SPIN_DEFAULT_STEPS: u32 = 8;
const SPIN_DEFAULT_ANGLE: f32 = std::f32::consts::TAU;

impl AzraApp {
    /// Spin selected geometry around workplane/symmetry/view axis through selection center.
    ///
    /// Vertex / Edge / Face mode: gathers affected verts (face → all face verts; edge → endpoints).
    /// Requires at least one edge with both ends selected. Refuse-before-transaction.
    pub(crate) fn poly_spin(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };

        let vert_ids: HashSet<u32> = match self.poly_work_mode {
            PolyWorkMode::Vertex => self.poly_sel.selected_vertices.iter().copied().collect(),
            PolyWorkMode::Edge => {
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    self.toast("Spin: not a poly mesh");
                    return;
                };
                let mut v = HashSet::new();
                for &e in &self.poly_sel.selected_edges {
                    let he = core_math::half_edge::HalfEdgeId(e);
                    if !mesh.is_edge_live(he) {
                        continue;
                    }
                    v.insert(mesh.he_source(he).0);
                    v.insert(mesh.he(he).vertex.0);
                }
                if v.is_empty() {
                    if let Some(h) = self.poly_sel.hovered_edge {
                        let he = core_math::half_edge::HalfEdgeId(h);
                        if mesh.is_edge_live(he) {
                            v.insert(mesh.he_source(he).0);
                            v.insert(mesh.he(he).vertex.0);
                        }
                    }
                }
                v
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    self.toast("Spin: not a poly mesh");
                    return;
                };
                let mut v = HashSet::new();
                for &f in &self.poly_sel.selected_faces {
                    for vid in mesh.face_vertices(core_math::half_edge::FaceId(f)) {
                        v.insert(vid.0);
                    }
                }
                v
            }
            PolyWorkMode::Object => {
                self.toast("Spin: enter Vertex / Edge / Face mode");
                return;
            }
        };

        if vert_ids.len() < 2 {
            self.toast("Spin: select verts/edges/faces with ≥1 edge");
            return;
        }

        // Pivot + axis in WORLD, then to body-local (mesh is local).
        let (center_w, axis_w) = self.poly_spin_frame();
        let world = self.scene.scene_graph.world_transform(nid);
        let inv = world.inverse();
        let center = inv.transform_point3(center_w);
        let axis = inv.transform_vector3(axis_w).normalize_or_zero();
        if axis.length_squared() < 0.25 {
            self.toast("Spin: degenerate axis");
            return;
        }

        let steps = SPIN_DEFAULT_STEPS;
        let angle = SPIN_DEFAULT_ANGLE;
        let merge = true;

        let dry_n = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast("Spin: not a poly mesh");
                return;
            };
            let set: HashSet<VertexId> = vert_ids.iter().map(|&v| VertexId(v)).collect();
            let mut scratch = (**mesh).clone();
            scratch.spin(&set, center, axis, angle, steps, merge)
        };
        if dry_n == 0 {
            self.toast("Spin: need edges between selected verts");
            return;
        }

        self.transaction("Spin", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let set: HashSet<VertexId> = vert_ids.iter().map(|&v| VertexId(v)).collect();
                    let n = m.spin(&set, center, axis, angle, steps, merge);
                    s.toast(&format!("Spin: {n} faces ({steps} steps)"));
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        });
    }

    /// World-space spin pivot + axis: selection center × workplane/symmetry/view-up.
    fn poly_spin_frame(&self) -> (Vec3, Vec3) {
        let center = self
            .poly_selection_center()
            .unwrap_or(self.viewport.wp_resolved.origin);

        // Axis: symmetry normal → workplane normal → world Y.
        if self.viewport.symmetry.enabled {
            let n = self.viewport.symmetry.normal.normalize_or_zero();
            if n.length_squared() > 0.25 {
                return (self.viewport.symmetry.origin, n);
            }
        }
        let n = self.viewport.wp_resolved.normal.normalize_or_zero();
        if n.length_squared() > 0.25 {
            return (center, n);
        }
        (center, Vec3::Y)
    }
}
