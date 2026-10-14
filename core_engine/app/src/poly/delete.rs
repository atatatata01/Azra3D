use core_math::mesh_ops::PolyEdit;
use crate::AzraApp;

impl AzraApp {
    /// Delete selected faces (delta undo — POLY_FOUNDATION_PLAN §4.6, Phase 2).
    ///
    /// **DESTRUCTIVE — explicit selection only.** Never use `poly_faces_or_all` (empty→all is
    /// for SAFE ops like extrude/inset only). Empty selection is a no-op.
    pub(crate) fn poly_delete_faces(&mut self) {
        let nid = match self.resolve_poly_edit_nid() {
            Some(n) => n,
            None => return,
        };
        let faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        if faces.is_empty() {
            self.toast("Delete Faces: select faces first");
            return;
        }
        // ONE batched DeleteFaces — never one PolyEdit per face. A UV sphere with A+Del used to
        // clone+validate the whole mesh once per face and hang the app (owner 2026-08-07).
        let out = self.poly_op_batch(
            nid,
            vec![PolyEdit::DeleteFaces { faces }],
            "Delete Faces",
        );
        // keep=&[] — deleted faces deselect; the reporter re-selects any that REFUSED so the user
        // can see and retry exactly those (was: blanket clear + unconditional success toast).
        self.poly_report_batch(&out, "deleted", &[]);
    }

    /// Blender "Delete > Only Faces" — drop the selected faces but KEEP their edges + vertices; a loose
    /// edge survives as a WIRE edge (`half_edge::delete_faces_only`). Distinct from `poly_delete_faces`
    /// (Blender "Delete Faces"), which removes the now-loose edges too. ONE batched `DeleteFacesOnly` op
    /// (not one-per-face) so the whole selection is a single undo entry — it only retires face records,
    /// no id re-keying, so batching is order-independent.
    pub(crate) fn poly_delete_faces_only(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        if faces.is_empty() { return; }
        let out = self.poly_op_batch(nid, vec![PolyEdit::DeleteFacesOnly { faces }], "Delete Only Faces");
        self.poly_report_batch(&out, "deleted (kept edges)", &[]);
    }

    /// Blender "Delete Vertices" — remove selected verts + every face/edge using them (Vertex mode).
    /// Descending id order so freeing higher ids first minimizes id churn across the batch.
    pub(crate) fn poly_delete_vertices(&mut self) {
        let nid = match self.resolve_poly_edit_nid() {
            Some(n) => n,
            None => return,
        };
        let mut verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        if verts.is_empty() {
            return;
        }
        verts.sort_unstable_by(|a, b| b.cmp(a));
        let ops: Vec<PolyEdit> = verts
            .into_iter()
            .map(|v| PolyEdit::DeleteVertex { vertex: v })
            .collect();
        let out = self.poly_op_batch(nid, ops, "Delete Vertices");
        self.poly_report_batch(&out, "deleted", &[]);
    }

    /// Blender "Delete Edges" — remove selected edges + the faces using them (Edge mode).
    pub(crate) fn poly_delete_edges(&mut self) {
        let nid = match self.resolve_poly_edit_nid() {
            Some(n) => n,
            None => return,
        };
        let mut edges: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
        if edges.is_empty() {
            return;
        }
        edges.sort_unstable_by(|a, b| b.cmp(a));
        let ops: Vec<PolyEdit> = edges
            .into_iter()
            .map(|e| PolyEdit::DeleteEdge { edge: e })
            .collect();
        let out = self.poly_op_batch(nid, ops, "Delete Edges");
        self.poly_report_batch(&out, "deleted", &[]);
    }
}
