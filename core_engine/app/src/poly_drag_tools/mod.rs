// core_engine/app/src/poly_drag_tools/mod.rs
// Interactive drag operations: extrude drag, inset drag, G/R/S transform, bevel modal, bevel preview.
// Pure relocation of poly_drag_tools.rs (god-files split).

mod extrude_drag;
mod extrude_sweep;
mod inset_drag;
mod shrink_fatten;
mod transform_drag;
mod bevel_drag;
mod bevel_preview;
mod offset_loop_drag;
mod edge_weight;

use std::collections::HashSet;
use glam::Vec3;
use core_math::half_edge::{FaceId, NO_FACE, NO_EDGE};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, PolyWorkMode};

impl AzraApp {
    /// Gather vertex indices affected by the current selection (verts/edges/faces).
    fn poly_gather_affected_vertices(&self) -> HashSet<u32> {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return HashSet::new() };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return HashSet::new() };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return HashSet::new(),
        };
        let mut verts = HashSet::new();
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                verts = self.poly_sel.selected_vertices.clone();
            }
            PolyWorkMode::Edge => {
                for &eid in &self.poly_sel.selected_edges {
                    if eid as usize >= mesh.half_edges.len() { continue; }
                    let he_id = core_math::half_edge::HalfEdgeId(eid);
                    let he = mesh.he(he_id);
                    if he.face == NO_FACE { continue; }
                    verts.insert(he.vertex.0);
                    let src = mesh.he_source(he_id);
                    verts.insert(src.0);
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                for &fid in &self.poly_sel.selected_faces {
                    let f = FaceId(fid);
                    if fid as usize >= mesh.faces.len() { continue; }
                    if mesh.face(f).half_edge == NO_EDGE { continue; }
                    for vid in mesh.face_vertices(f) {
                        verts.insert(vid.0);
                    }
                }
            }
            _ => {}
        }
        verts
    }

}
