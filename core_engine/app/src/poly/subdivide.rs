use core_math::mesh_ops::PolyEdit;
use crate::AzraApp;

impl AzraApp {
    /// Apply Catmull-Clark subdivision to the selected polygonal mesh (delta undo —
    /// POLY_FOUNDATION_PLAN §4.6, Phase 2). Whole-mesh subdivide uses the honest O(body) Snapshot.
    pub(crate) fn poly_subdivide(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let selected_faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        let whole_mesh = selected_faces.is_empty();
        let op = if whole_mesh {
            PolyEdit::SubdivideCatmullClark
        } else {
            PolyEdit::SubdivideFaces { faces: selected_faces }
        };
        let out = self.poly_op(nid, op, "Subdivide");
        // Subdivide RE-KEYS every element id, so a successful run invalidates all three domains —
        // cross-domain staleness is subdivide-specific and stays here rather than polluting the
        // shared reporter (which owns exactly one domain). Order matters: clear first, then let the
        // reporter re-select the faces if the op REFUSED (in which case ids never moved).
        if out.any_applied() { self.poly_clear_selection(); }
        let verb = if whole_mesh { "subdivided (Catmull-Clark)" } else { "subdivided" };
        self.poly_report_batch(&out, verb, &[]);
    }
}
