use core_math::mesh_ops::PolyEdit;
use core_math::scene_graph::NodePayload;
use crate::AzraApp;

impl AzraApp {
    /// Connect Vertex Path (Blender `bpy.ops.mesh.vert_connect_path`, key `J`): connect the selected
    /// vertices with new diagonal edges, each splitting the shared face (`half_edge::split_face_between`,
    /// ID-preserving). Vertex mode.
    ///
    /// SELECTION ORDER: Blender walks the vertices in selection-history order; AZRA's `PolySelection` is a
    /// `HashSet` with no history, so a valid path is RECONSTRUCTED from shared-face adjacency
    /// (`connect_path_order`, starting from the active vertex when it is a path endpoint). For the SIMPLE
    /// 2-vertex case order is irrelevant, so that case is exact. Vertex ids are preserved by the split, so
    /// the connected vertices stay selected on success; on refusal they stay selected too (via the
    /// reporter's refused-target retention).
    pub(crate) fn poly_connect_vertices(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let sel: std::collections::HashSet<u32> = self.poly_sel.selected_vertices.clone();
        if sel.len() < 2 { self.toast("Select 2+ vertices to connect"); return; }

        // Reconstruct a path order from shared-face adjacency (no selection history exists).
        let order: Vec<u32> = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh, _ => return };
            mesh.connect_path_order(&sel, self.poly_active_vertex).iter().map(|v| v.0).collect()
        };
        let keep: Vec<u32> = order.clone(); // vertex ids survive the split → keep them selected.
        let out = self.poly_op(nid, PolyEdit::ConnectVertexPath { verts: order }, "Connect Vertices");
        self.poly_report_batch(&out, "connected", &keep);
    }
}
