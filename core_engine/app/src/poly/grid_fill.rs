//! Blender Grid Fill (Ctrl+Alt+F) — fill a closed edge loop with a quad grid.

use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::HalfEdgeId;
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

impl AzraApp {
    /// Grid Fill: closed edge loop → regular grid of quads (Blender **Grid Fill**).
    ///
    /// Edge mode: `selected_edges` must form one simple closed ring (even length ≥ 4).  
    /// Vertex mode: edges between selected verts that form such a ring.  
    /// Face mode: not supported (toast). Auto span + offset 0 (square-ish sides).
    pub(crate) fn poly_grid_fill(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };

        let edge_ids: Vec<u32> = match self.poly_work_mode {
            PolyWorkMode::Edge => {
                let e: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
                if e.is_empty() {
                    self.toast("Grid Fill: select a closed edge loop");
                    return;
                }
                e
            }
            PolyWorkMode::Vertex => {
                let verts: HashSet<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
                if verts.len() < 4 {
                    self.toast("Grid Fill: select ≥4 verts of a closed loop");
                    return;
                }
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    self.toast("Grid Fill: not a poly mesh");
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
                self.toast("Grid Fill: use Edge or Vertex mode");
                return;
            }
            PolyWorkMode::Object => {
                self.toast("Grid Fill: enter Edge / Vertex mode");
                return;
            }
        };

        if edge_ids.is_empty() {
            self.toast("Grid Fill: no edges in selection");
            return;
        }

        let dry_n = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast("Grid Fill: not a poly mesh");
                return;
            };
            let mut scratch = (**mesh).clone();
            let set: HashSet<HalfEdgeId> = edge_ids.iter().map(|&e| HalfEdgeId(e)).collect();
            scratch.grid_fill(&set, None, 0).len()
        };
        if dry_n == 0 {
            self.toast("Grid Fill: need one closed even-length loop (L≥4)");
            return;
        }

        self.transaction("Grid Fill", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let set: HashSet<HalfEdgeId> = edge_ids.iter().map(|&e| HalfEdgeId(e)).collect();
                    let faces = m.grid_fill(&set, None, 0);
                    for f in &faces {
                        s.poly_sel.selected_faces.insert(f.0);
                    }
                    s.toast(&format!("Grid Fill: {} faces", faces.len()));
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        });
    }
}
