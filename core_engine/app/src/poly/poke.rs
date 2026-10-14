use core_math::mesh_ops::PolyEdit;
use crate::AzraApp;

impl AzraApp {
    /// Poke Faces (Blender `bpy.ops.mesh.poke`): fan each selected face into a triangle fan from a NEW
    /// center vertex at Blender's DEFAULT `MEDIAN_WEIGHTED` center (`half_edge::poke_faces`). Offset is
    /// 0 (a menu-only op, no interactive drag). Face mode.
    ///
    /// Poke rebuilds via `from_polygons`, which re-keys face ids, so on success the face selection is
    /// cleared (same as `poly_subdivide`); on a clean refusal `poly_report_batch` re-selects the refused
    /// faces (ids stay valid — the refusal left the mesh byte-identical).
    pub(crate) fn poly_poke_faces(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Modo implicit full selection: empty selection pokes EVERY face (SAFE — adds a center vertex per
        // face, never removes geometry). The old "select first" toast is gone; empty now just runs on all.
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() { return; }
        let out = self.poly_op(nid, PolyEdit::PokeFaces { faces, offset: 0.0 }, "Poke Faces");
        if out.any_applied() { self.poly_clear_selection(); }
        self.poly_report_batch(&out, "poked", &[]);
    }
}
