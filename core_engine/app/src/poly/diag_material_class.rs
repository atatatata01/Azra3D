//! APP-PATH verification for the per-face material-loss class (owner reports 2026-08-07).
//!
//! A green `core_math` test proves the kernel is right; it does NOT prove the user's Ctrl+X or
//! Alt+X does the right thing, because the app layer sits in between (selection → `PolyEdit` batch
//! → `poly_op_batch` → kernel → mesh install → display sync). Project rule
//! `feedback_verify_app_path_not_kernel`: verify through the REAL tool entry.
//!
//! So these tests build a headless `AzraApp`, install a poly body whose faces carry DISTINCT
//! materials, drive the real entries — `poly_dissolve()` (Ctrl+X) and `poly_symmetrize_op()`
//! (Alt+X) — and read the materials back off the INSTALLED mesh in the scene graph.
//!
//! They assert EXACT ids. "Not zero" is not enough: a fix that floods one surviving material over
//! the whole body would pass a non-zero check while destroying the user's assignment just as
//! thoroughly.
//!
//! Run: `cargo test -p app poly::diag_material_class -- --nocapture`

#[cfg(test)]
mod tests {
    use crate::{AzraApp, SceneItemId};
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_TWIN};
    use core_math::scene_graph::{NodeId, NodePayload};
    use glam::Vec3;

    /// Flat 2x1 quad grid in the XY plane sharing the middle edge (x = 1).
    /// Face 0 (x in 0..1) gets `m0`, face 1 (x in 1..2) gets `m1`.
    fn two_quads(m0: u32, m1: u32) -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
        ];
        let f = vec![vec![0, 1, 4, 3], vec![1, 2, 5, 4]];
        HalfEdgeMesh::from_polygons_with_materials(&p, &f, &[m0, m1])
    }

    fn app_with(mesh: HalfEdgeMesh) -> (AzraApp, NodeId) {
        let mut app = AzraApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "MatProbe".into(),
                mesh: std::sync::Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(SceneItemId::Node(nid));
        (app, nid)
    }

    /// Materials of every live face of the body as it is INSTALLED in the scene graph.
    fn installed_materials(app: &AzraApp, nid: NodeId) -> Vec<u32> {
        let m = app.poly_node_mesh(nid).expect("poly body still present");
        (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|f| m.face(*f).half_edge != NO_EDGE)
            .map(|f| m.face(f).material_id)
            .collect()
    }

    /// The half-edge id of the shared edge between face 0 and face 1.
    fn shared_edge(app: &AzraApp, nid: NodeId) -> u32 {
        let m = app.poly_node_mesh(nid).expect("poly body");
        (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .find(|&h| {
                m.is_edge_live(h) && m.he(h).face == FaceId(0) && {
                    let tw = m.he(h).twin;
                    tw != NO_TWIN && m.he(tw).face == FaceId(1)
                }
            })
            .expect("faces 0 and 1 share an edge")
            .0
    }

    // ============================================================
    // Ctrl+X — Dissolve Edges, through `poly_dissolve()`
    // ============================================================

    #[test]
    fn app_ctrl_x_dissolve_edge_keeps_material() {
        // Owner report: "Dissolving edge (Ctrl+X) removes material". Both faces are material 7, so
        // the merged n-gon has exactly one correct answer — 7. Before the fix it came back 0.
        let (mut app, nid) = app_with(two_quads(7, 7));
        let he = shared_edge(&app, nid);

        app.poly_work_mode = crate::PolyWorkMode::Edge;
        app.poly_sel.selected_edges.clear();
        app.poly_sel.selected_edges.insert(he);

        app.poly_dissolve(); // ← the real Ctrl+X entry

        let mats = installed_materials(&app, nid);
        assert_eq!(
            mats,
            vec![7],
            "Ctrl+X dissolve must leave ONE n-gon still carrying material 7 (got {mats:?})"
        );
    }

    #[test]
    fn app_ctrl_x_dissolve_never_falls_back_to_default() {
        // Distinct materials: the merged face must carry a REAL one (the lowest-FaceId source),
        // never the default 0. Asserts the exact id, not merely non-zero.
        let (mut app, nid) = app_with(two_quads(3, 8));
        let he = shared_edge(&app, nid);

        app.poly_work_mode = crate::PolyWorkMode::Edge;
        app.poly_sel.selected_edges.clear();
        app.poly_sel.selected_edges.insert(he);

        app.poly_dissolve();

        let mats = installed_materials(&app, nid);
        assert!(!mats.is_empty(), "dissolve must leave geometry");
        assert!(
            !mats.contains(&0),
            "no face may fall back to the default material (got {mats:?})"
        );
        assert_eq!(mats, vec![3], "merged face takes the lowest-FaceId source material");
    }

    // ============================================================
    // Alt+X — Symmetrize, through `poly_symmetrize_op()`
    // ============================================================

    #[test]
    fn app_alt_x_symmetrize_mirrors_material() {
        // Owner report: "Symmetrize (Alt+X): I lose my material that is assigned; it should
        // symmetrize material by default." Plane at x = 1, keeping the +X half (face 1 = material
        // 9). Every face of the result — kept AND mirrored — must be 9.
        let (mut app, nid) = app_with(two_quads(7, 9));

        app.poly_symmetrize_op(nid, Vec3::new(1.0, 0.0, 0.0), Vec3::X, false); // ← the real Alt+X entry

        let mats = installed_materials(&app, nid);
        assert!(!mats.is_empty(), "symmetrize must leave geometry");
        assert!(
            mats.iter().all(|&m| m == 9),
            "symmetrize must mirror the kept half's material by default (got {mats:?})"
        );
        assert!(
            mats.len() >= 2,
            "symmetrize must actually produce a mirrored half (got {} face(s))",
            mats.len()
        );
    }

    #[test]
    fn app_alt_x_symmetrize_keeps_distinct_materials() {
        // Kept half carries TWO different materials — the mirror must map each mirrored face to
        // ITS OWN source, not flood one material over the whole body.
        let p = vec![
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(3.0, 1.0, 0.0),
        ];
        let f = vec![vec![0, 1, 4, 3], vec![1, 2, 5, 4]];
        let mesh = HalfEdgeMesh::from_polygons_with_materials(&p, &f, &[4, 5]);
        let (mut app, nid) = app_with(mesh);

        // Plane at x = 1 with +X kept: both faces survive, both get mirrored.
        app.poly_symmetrize_op(nid, Vec3::new(1.0, 0.0, 0.0), Vec3::X, false);

        let mats = installed_materials(&app, nid);
        assert!(!mats.contains(&0), "no face may drop to the default (got {mats:?})");
        assert!(
            mats.contains(&4) && mats.contains(&5),
            "both authored materials must survive symmetrize (got {mats:?})"
        );
        assert_eq!(
            mats.iter().filter(|&&m| m == 4).count(),
            mats.iter().filter(|&&m| m == 5).count(),
            "the mirror is symmetric, so the two materials must appear equally often ({mats:?})"
        );
    }
}
