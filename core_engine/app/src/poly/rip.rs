//! Blender Rip (V) / Rip Fill (Alt+V) — open a boundary slit along selected edges,
//! optionally sealing each slit with a zero-area fill face.

use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::HalfEdgeId;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

impl AzraApp {
    /// Rip selected edges into a boundary slit (Blender **V**).
    pub(crate) fn poly_rip(&mut self) {
        self.poly_rip_impl(false);
    }

    /// Rip Fill — rip then seal each slit with a fill quad (Blender **Alt+V**).
    pub(crate) fn poly_rip_fill(&mut self) {
        self.poly_rip_impl(true);
    }

    /// Shared selection + refuse-before-transaction path for Rip / Rip Fill.
    ///
    /// Edge mode: uses `selected_edges` (or hovered edge).  
    /// Face mode: rips the **boundary of the face selection** (shared exterior edges of the region).  
    /// Vertex mode: rips edges that connect two selected vertices.
    fn poly_rip_impl(&mut self, fill: bool) {
        let label = if fill { "Rip Fill" } else { "Rip" };
        let Some((nid, edge_ids)) = self.poly_rip_collect_edges(label) else {
            return;
        };

        let dry_n = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast(&format!("{label}: not a poly mesh"));
                return;
            };
            let mut scratch = (**mesh).clone();
            let set: HashSet<HalfEdgeId> = edge_ids.iter().map(|&e| HalfEdgeId(e)).collect();
            if fill {
                scratch.rip_fill_edges(&set)
            } else {
                scratch.rip_edges(&set)
            }
        };
        if dry_n == 0 {
            self.toast(&format!(
                "{label}: no interior edges (already boundary?)"
            ));
            return;
        }

        self.transaction(label, |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let set: HashSet<HalfEdgeId> = edge_ids.iter().map(|&e| HalfEdgeId(e)).collect();
                    let n = if fill {
                        m.rip_fill_edges(&set)
                    } else {
                        m.rip_edges(&set)
                    };
                    if fill {
                        s.toast(&format!("Rip Fill: {n} faces"));
                    } else {
                        s.toast(&format!("Ripped {n} edges"));
                    }
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        });
    }

    /// Collect target edge ids for Rip / Rip Fill from the current work mode.
    /// Returns `None` after toasting on refuse.
    fn poly_rip_collect_edges(&mut self, label: &str) -> Option<(NodeId, Vec<u32>)> {
        let nid = self.sel.usel.active_node()?;

        let edge_ids: Vec<u32> = match self.poly_work_mode {
            PolyWorkMode::Edge => {
                let mut e: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
                if e.is_empty() {
                    if let Some(h) = self.poly_sel.hovered_edge {
                        e.push(h);
                    }
                }
                e
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let faces: HashSet<u32> = self.poly_sel.selected_faces.iter().copied().collect();
                if faces.is_empty() {
                    self.toast(&format!("{label}: select faces or edges"));
                    return None;
                }
                // Boundary of selection: HE on a selected face whose twin face is unselected/none.
                let node = self.scene.scene_graph.get(nid)?;
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    self.toast(&format!("{label}: not a poly mesh"));
                    return None;
                };
                let mut out = Vec::new();
                for &fid in &faces {
                    for he in mesh.face_half_edges(core_math::half_edge::FaceId(fid)) {
                        let tw = mesh.he(he).twin;
                        let outside = tw == core_math::half_edge::NO_TWIN
                            || {
                                let nf = mesh.he(tw).face;
                                nf == core_math::half_edge::NO_FACE || !faces.contains(&nf.0)
                            };
                        // Rip needs an INTERIOR edge of the mesh that is on the region boundary —
                        // i.e. twin exists and goes to unselected face.
                        if outside && tw != core_math::half_edge::NO_TWIN {
                            out.push(he.0);
                        }
                    }
                }
                out
            }
            PolyWorkMode::Vertex => {
                let verts: HashSet<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
                if verts.len() < 2 {
                    self.toast(&format!(
                        "{label}: select edges, or ≥2 verts, or a face region"
                    ));
                    return None;
                }
                let node = self.scene.scene_graph.get(nid)?;
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                    self.toast(&format!("{label}: not a poly mesh"));
                    return None;
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
            PolyWorkMode::Object => {
                self.toast(&format!("{label}: enter Vertex / Edge / Face mode"));
                return None;
            }
        };

        if edge_ids.is_empty() {
            self.toast(&format!("{label}: no edges to rip"));
            return None;
        }
        Some((nid, edge_ids))
    }
}
