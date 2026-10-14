use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use core_math::half_edge::{HalfEdgeId, VertexId, NO_EDGE, NO_TWIN};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

impl AzraApp {
    /// Duplicate selected sub-elements in poly edit mode (Shift+D).
    ///
    /// - **Face**: independent welded island (shared vert map across selected faces).
    /// - **Edge**: independent wire edges between duplicated endpoints (not digon faces, not
    ///   loose verts only). Shared endpoints weld so a chain of selected edges stays connected.
    ///   Stays in Edge mode with the new edges selected (gizmo-ready).
    /// - **Vertex**: independent loose verts.
    pub(crate) fn poly_duplicate_selection(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Refuse before transaction when active node is not a poly mesh.
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Duplicate: active object is not a poly mesh");
            return;
        }

        match self.poly_work_mode {
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
                if faces.is_empty() { return; }
                self.transaction("Duplicate Faces", |s| {
                    let mut new_face_ids = Vec::new();
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                            let mesh = Arc::make_mut(mesh);
                            // Shared vertex map: each SOURCE vertex used by ANY selected face maps to
                            // exactly ONE new vertex, so adjacent duplicated faces stay welded to each
                            // other (create_face pairs interior twins by matching VertexId). Without this
                            // every face-corner got its own fresh vertex → duplicating all 6 faces of a
                            // cube produced 24 unshared verts = 6 disconnected quad shells ("all surfaces
                            // are not connected"). Weld is internal-only: the copy shares nothing with the
                            // source mesh (Blender Shift+D = independent island). Mirrors the Edge branch.
                            let mut vert_map: HashMap<u32, VertexId> = HashMap::new();
                            for &fid in &faces {
                                let verts = mesh.face_vertices(core_math::half_edge::FaceId(fid));
                                let mut new_verts = Vec::with_capacity(verts.len());
                                for &vid in &verts {
                                    let nv = *vert_map.entry(vid.0).or_insert_with(|| {
                                        let pos = mesh.vert(vid).position;
                                        let normal = mesh.vert(vid).normal;
                                        mesh.alloc_vertex(core_math::half_edge::HVertex {
                                            position: pos, normal, half_edge: NO_EDGE,
                                        })
                                    });
                                    new_verts.push(nv);
                                }
                                if let Some(new_fid) = mesh.create_face(&new_verts) {
                                    new_face_ids.push(new_fid.0);
                                }
                            }
                            mesh.recompute_normals();
                        }
                    }
                    s.poly_sel.selected_faces.clear();
                    for f in &new_face_ids { s.poly_sel.selected_faces.insert(*f); }
                    s.poly_request_display_sync(nid);
                    s.mark_dirty(DirtyFlags::RENDER);
                });
            }
            PolyWorkMode::Edge => {
                let edges: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
                if edges.is_empty() { return; }
                self.transaction("Duplicate Edges", |s| {
                    let mut new_edge_ids: Vec<u32> = Vec::new();
                    let mut new_vert_ids: Vec<u32> = Vec::new();
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                            let mesh = Arc::make_mut(mesh);
                            // Shared vert map: chain of adjacent selected edges → one connected
                            // wire polyline (Blender Shift+D), not N disconnected segments.
                            let mut vert_map: HashMap<u32, VertexId> = HashMap::new();
                            let mut seen_undirected: HashSet<u32> = HashSet::new();
                            for &he_idx in &edges {
                                let he = HalfEdgeId(he_idx);
                                if !mesh.is_edge_live(he) {
                                    continue;
                                }
                                // Canonical undirected id so selecting both twins (or either) dups once.
                                let twin = mesh.he(he).twin;
                                let canon = if twin != NO_TWIN {
                                    he_idx.min(twin.0)
                                } else {
                                    he_idx
                                };
                                if !seen_undirected.insert(canon) {
                                    continue;
                                }
                                let src = mesh.he_source(he);
                                let dst = mesh.he(he).vertex;
                                let na = *vert_map.entry(src.0).or_insert_with(|| {
                                    let pos = mesh.vert(src).position;
                                    let normal = mesh.vert(src).normal;
                                    mesh.alloc_vertex(core_math::half_edge::HVertex {
                                        position: pos, normal, half_edge: NO_EDGE,
                                    })
                                });
                                let nb = *vert_map.entry(dst.0).or_insert_with(|| {
                                    let pos = mesh.vert(dst).position;
                                    let normal = mesh.vert(dst).normal;
                                    mesh.alloc_vertex(core_math::half_edge::HVertex {
                                        position: pos, normal, half_edge: NO_EDGE,
                                    })
                                });
                                // Real wire topology (not digon faces, not orphan verts).
                                if let Some(new_he) = mesh.create_wire_edge(na, nb) {
                                    new_edge_ids.push(new_he.0);
                                }
                            }
                            new_vert_ids = vert_map.values().map(|v| v.0).collect();
                        }
                    }
                    // Select the NEW edges (and their endpoints for grab/gizmo), stay in Edge mode.
                    // Owner: "when I duplicate edge I only see vertex" — that was the old path
                    // (verts only + force Vertex mode). Result of edge Shift+D IS edges.
                    s.poly_sel.selected_faces.clear();
                    s.poly_sel.selected_edges.clear();
                    s.poly_sel.selected_vertices.clear();
                    for &eid in &new_edge_ids {
                        s.poly_sel.selected_edges.insert(eid);
                    }
                    for &vid in &new_vert_ids {
                        s.poly_sel.selected_vertices.insert(vid);
                    }
                    s.poly_prefer_work_mode(PolyWorkMode::Edge);
                    s.poly_request_display_sync(nid);
                    s.mark_dirty(DirtyFlags::RENDER);
                });
            }
            PolyWorkMode::Vertex => {
                let verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
                if verts.is_empty() { return; }
                self.transaction("Duplicate Vertices", |s| {
                    let mut new_vert_ids = Vec::new();
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                            let mesh = Arc::make_mut(mesh);
                            for &vid in &verts {
                                let pos = mesh.vert(VertexId(vid)).position;
                                let normal = mesh.vert(VertexId(vid)).normal;
                                let nv = mesh.alloc_vertex(core_math::half_edge::HVertex {
                                    position: pos, normal, half_edge: NO_EDGE,
                                });
                                new_vert_ids.push(nv.0);
                            }
                        }
                    }
                    s.poly_sel.selected_vertices.clear();
                    for v in &new_vert_ids { s.poly_sel.selected_vertices.insert(*v); }
                    s.poly_request_display_sync(nid);
                    s.mark_dirty(DirtyFlags::RENDER);
                });
            }
            PolyWorkMode::Object => {}
        }
    }
}
