//! LoopTools family: Relax / Flatten / Space (Wave 6 F2–F4).
//! Circle remains in `circle.rs` (F1 / S4).

use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::{HalfEdgeId, VertexId};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

impl AzraApp {
    /// Gather selected vertex ids for LoopTools reshape ops (V / E / F / all-on-empty faces).
    fn poly_looptools_verts(&self, nid: core_math::scene_graph::NodeId) -> HashSet<u32> {
        match self.poly_work_mode {
            PolyWorkMode::Vertex => self.poly_sel.selected_vertices.iter().copied().collect(),
            PolyWorkMode::Edge => {
                let Some(node) = self.scene.scene_graph.get(nid) else {
                    return HashSet::new();
                };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    return HashSet::new();
                };
                let mut v = HashSet::new();
                for &e in &self.poly_sel.selected_edges {
                    let he = HalfEdgeId(e);
                    if !mesh.is_edge_live(he) {
                        continue;
                    }
                    v.insert(mesh.he_source(he).0);
                    v.insert(mesh.he(he).vertex.0);
                }
                v
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let faces = self.poly_faces_or_all(nid);
                let Some(node) = self.scene.scene_graph.get(nid) else {
                    return HashSet::new();
                };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    return HashSet::new();
                };
                let mut v = HashSet::new();
                for f in faces {
                    for vid in mesh.face_vertices(core_math::half_edge::FaceId(f)) {
                        v.insert(vid.0);
                    }
                }
                v
            }
            PolyWorkMode::Object => HashSet::new(),
        }
    }

    /// LoopTools **Relax** — Laplacian smooth selected verts (1 iter × factor 1, boundary locked).
    pub(crate) fn poly_loop_relax(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        if self.poly_work_mode == PolyWorkMode::Object {
            self.toast("Relax: enter Vertex / Edge / Face mode");
            return;
        }
        let vids = self.poly_looptools_verts(nid);
        if vids.is_empty() {
            self.toast("Relax: select verts / edges / faces");
            return;
        }
        let set: HashSet<VertexId> = vids.iter().map(|&v| VertexId(v)).collect();
        let dry = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast("Relax: not a poly mesh");
                return;
            };
            let mut scratch = (**mesh).clone();
            scratch.loop_relax(&set, 1, 1.0, true)
        };
        if dry == 0 {
            self.toast("Relax: no movable verts (boundary locked?)");
            return;
        }
        self.transaction("LoopTools Relax", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let n = m.loop_relax(&set, 1, 1.0, true);
                    s.toast(&format!("Relax: {n} verts"));
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        });
    }

    /// LoopTools **Flatten** — project selection onto best-fit plane.
    pub(crate) fn poly_loop_flatten(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        if self.poly_work_mode == PolyWorkMode::Object {
            self.toast("Flatten: enter Vertex / Edge / Face mode");
            return;
        }
        let vids = self.poly_looptools_verts(nid);
        if vids.len() < 3 {
            self.toast("Flatten: need ≥3 verts");
            return;
        }
        let set: HashSet<VertexId> = vids.iter().map(|&v| VertexId(v)).collect();
        let dry = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast("Flatten: not a poly mesh");
                return;
            };
            let mut scratch = (**mesh).clone();
            scratch.loop_flatten(&set)
        };
        if dry == 0 {
            self.toast("Flatten: degenerate plane");
            return;
        }
        self.transaction("LoopTools Flatten", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let n = m.loop_flatten(&set);
                    s.toast(&format!("Flatten: {n} verts"));
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        });
    }

    /// LoopTools **Space** — equalize spacing along selected edge loops/chains.
    pub(crate) fn poly_loop_space(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        if self.poly_work_mode == PolyWorkMode::Object {
            self.toast("Space: enter Edge / Vertex mode");
            return;
        }

        let edge_ids: Vec<u32> = match self.poly_work_mode {
            PolyWorkMode::Edge => self.poly_sel.selected_edges.iter().copied().collect(),
            PolyWorkMode::Vertex => {
                let verts: HashSet<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    self.toast("Space: not a poly mesh");
                    return;
                };
                let mut out = Vec::new();
                for i in 0..mesh.half_edges.len() as u32 {
                    let he = HalfEdgeId(i);
                    if !mesh.is_edge_live(he) {
                        continue;
                    }
                    let a = mesh.he_source(he).0;
                    let b = mesh.he(he).vertex.0;
                    if verts.contains(&a) && verts.contains(&b) {
                        out.push(i);
                    }
                }
                out
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                // Boundary edges of the face region.
                let faces: HashSet<u32> = self.poly_faces_or_all(nid).into_iter().collect();
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    self.toast("Space: not a poly mesh");
                    return;
                };
                let fset: HashSet<_> = faces.iter().map(|&f| core_math::half_edge::FaceId(f)).collect();
                mesh.region_boundary_half_edges(&fset)
                    .into_iter()
                    .map(|h| h.0)
                    .collect()
            }
            PolyWorkMode::Object => return,
        };

        if edge_ids.is_empty() {
            self.toast("Space: select an edge loop or chain");
            return;
        }

        let dry = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast("Space: not a poly mesh");
                return;
            };
            let set: HashSet<HalfEdgeId> = edge_ids.iter().map(|&e| HalfEdgeId(e)).collect();
            let mut scratch = (**mesh).clone();
            scratch.loop_space(&set)
        };
        if dry == 0 {
            self.toast("Space: need a simple path/loop (no branches)");
            return;
        }
        self.transaction("LoopTools Space", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let set: HashSet<HalfEdgeId> = edge_ids.iter().map(|&e| HalfEdgeId(e)).collect();
                    let n = m.loop_space(&set);
                    s.toast(&format!("Space: {n} verts"));
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        });
    }
}
