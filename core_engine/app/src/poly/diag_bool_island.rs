//! APP-PATH proof that an edit-mode island boolean is scoped to the islands the cutter can
//! actually REACH (owner 2026-08-11: "I was trying to subtract the yellow piece and it did not
//! work" — `EDGE SELECT PROBLEM, BOLEAN PROBLEM.fl4`).
//!
//! That file holds ONE `PolygonalBody` with THREE islands:
//!   * island 0 — 16 faces, CLOSED, material 1 (albedo 1.0/0.497/0.0 = the yellow piece)
//!   * island 1 — 74 faces, CLOSED — the box being carved
//!   * island 2 — 146 faces, **OPEN** (36 boundary half-edges), ~3 units away in −Z
//!
//! The island planner used to fuse EVERY non-cutter face into one "remainder" target mesh. The
//! fused target inherited island 2's 36 boundary edges, so `poly_boolean_execute`'s closed-input
//! preflight refused the run ("target is open — fill holes or Solidify first") even though the
//! cutter and the box were both perfectly closed and island 2 was nowhere near the cut.
//!
//! Project rule `feedback_verify_app_path_not_kernel`: these drive the REAL X-pie entry
//! (`poly_boolean_execute`) on a headless `AzraApp` and read the mesh back out of the scene graph.
//!
//! Run: `cargo test -p app poly::diag_bool_island -- --nocapture`

#[cfg(test)]
mod tests {
    use crate::{AzraApp, PolyWorkMode, SceneItemId};
    use core_math::half_edge::{
        FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_FACE, NO_TWIN,
    };
    use core_math::mesh_boolean::BoolOp;
    use core_math::scene_graph::{NodeId, NodePayload};
    use glam::Vec3;

    /// Live, faced, twinless half-edges — the exact quantity the boolean preflight rejects on.
    fn boundary_count(m: &HalfEdgeMesh) -> usize {
        (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).face != NO_FACE && m.he(h).twin == NO_TWIN)
            .count()
    }

    fn live_faces(m: &HalfEdgeMesh) -> usize {
        (0..m.faces.len() as u32)
            .filter(|&i| m.face(FaceId(i)).half_edge != NO_EDGE)
            .count()
    }

    /// A single quad — one face, four boundary half-edges. Stands in for the owner's 146-face
    /// open shell: what matters is that it is OPEN and out of the cutter's reach.
    fn open_plane(offset: Vec3) -> HalfEdgeMesh {
        let p = [
            offset + Vec3::new(-1.0, -1.0, 0.0),
            offset + Vec3::new(1.0, -1.0, 0.0),
            offset + Vec3::new(1.0, 1.0, 0.0),
            offset + Vec3::new(-1.0, 1.0, 0.0),
        ];
        HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3]])
    }

    fn translated(mut m: HalfEdgeMesh, by: Vec3) -> HalfEdgeMesh {
        for v in m.vertices.iter_mut() {
            v.position += by;
        }
        m.recompute_normals();
        m
    }

    /// One body, three islands, in this face order:
    ///   faces 0..5   = cutter box (overlaps the target box) — material 1, the "yellow piece"
    ///   faces 6..11  = target box, closed                   — material 0
    ///   face  12     = open plane, far away in −Z           — material 0
    /// Returns (app, body node, cutter face ids).
    fn three_island_body() -> (AzraApp, NodeId, Vec<u32>) {
        let mut cutter = translated(HalfEdgeMesh::new_box(Vec3::splat(0.5)), Vec3::new(0.0, 1.0, 0.0));
        for f in cutter.faces.iter_mut() {
            f.material_id = 1;
        }
        let target = HalfEdgeMesh::new_box(Vec3::splat(1.0));
        let plane = open_plane(Vec3::new(0.0, 0.0, -10.0));

        assert_eq!(boundary_count(&cutter), 0, "cutter box must be closed");
        assert_eq!(boundary_count(&target), 0, "target box must be closed");
        assert_eq!(boundary_count(&plane), 4, "the plane island must be OPEN");

        let merged = crate::poly::boolean::merge_mesh_list(&[&cutter, &target, &plane]);
        let n_cut = live_faces(&cutter) as u32;
        assert_eq!(
            boundary_count(&merged),
            4,
            "the fused body carries the open island's boundary edges — the whole point"
        );

        let mut app = AzraApp::new_headless();
        app.modeling_context = crate::ModelingContext::Polygonal;
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "box_sep".into(),
                mesh: std::sync::Arc::new(merged),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(SceneItemId::Node(nid));

        // Edit mode, Face select, cutter island selected — the owner's gesture.
        app.poly_work_mode = PolyWorkMode::Face;
        app.poly_sel.body_stamp = Some(nid.0);
        let cutter_faces: Vec<u32> = (0..n_cut).collect();
        app.poly_sel.selected_faces = cutter_faces.iter().copied().collect();
        (app, nid, cutter_faces)
    }

    /// Drive the X-pie and return (before, after, toasts). Face COUNT is not a usable
    /// discriminator here — the carved box happens to land on the same total as the three
    /// uncut islands — so callers compare `content_signature()` instead.
    fn run_subtract(keep_cutter: bool) -> (std::sync::Arc<HalfEdgeMesh>, std::sync::Arc<HalfEdgeMesh>, Vec<String>) {
        let (mut app, nid, _) = three_island_body();
        let before = app.poly_node_mesh(nid).expect("body present");
        app.ui.bool_pie_keep_cutter = keep_cutter;
        app.ui.toasts.clear();
        app.poly_boolean_execute(BoolOp::Difference); // ← the real X-pie entry
        let toasts: Vec<String> = app.ui.toasts.iter().map(|t| t.message.clone()).collect();
        for t in &toasts {
            eprintln!("[toast] {t}");
        }
        let after = app.poly_node_mesh(nid).expect("body still present");
        (before, after, toasts)
    }

    /// Connected components over faces (shared-vertex). The cutter island being CONSUMED means
    /// the body drops from three components to two — face counts and materials cannot say that
    /// (the default cutter-material policy legitimately paints the carved pocket with the
    /// cutter's material 1, and the carved box happens to land on the same face total).
    fn island_count(m: &HalfEdgeMesh) -> usize {
        let mut owner: std::collections::HashMap<u32, usize> = Default::default();
        let mut parent: Vec<usize> = (0..m.faces.len()).collect();
        fn find(p: &mut Vec<usize>, mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        let mut roots = std::collections::HashSet::new();
        let live: Vec<usize> = (0..m.faces.len())
            .filter(|&i| m.face(FaceId(i as u32)).half_edge != NO_EDGE)
            .collect();
        for &i in &live {
            for v in m.face_vertices(FaceId(i as u32)) {
                match owner.get(&v.0) {
                    Some(&o) => {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, o));
                        if a != b {
                            parent[a] = b;
                        }
                    }
                    None => {
                        owner.insert(v.0, i);
                    }
                }
            }
        }
        for i in live {
            let r = find(&mut parent, i);
            roots.insert(r);
        }
        roots.len()
    }

    fn materials(m: &HalfEdgeMesh) -> Vec<u32> {
        (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|f| m.face(*f).half_edge != NO_EDGE)
            .map(|f| m.face(f).material_id)
            .collect()
    }

    /// THE regression. Before the fix the run bailed with
    /// "Boolean Subtract: target is open (boundary edges) … nothing changed" and the body came
    /// back byte-identical, because the fused remainder inherited the far island's 36/4
    /// boundary edges.
    #[test]
    fn island_subtract_ignores_an_unreachable_open_island() {
        let (before, after, toasts) = run_subtract(false);

        assert!(
            !toasts.iter().any(|t| t.contains("is open")),
            "no operand may be reported open — the open island is out of the cutter's reach \
             and must never be handed to the solid kernel: {toasts:?}"
        );
        assert_ne!(
            after.content_signature(),
            before.content_signature(),
            "the boolean must have changed the body — an unreachable OPEN island must not veto \
             a cut between two closed islands"
        );
        // The open island survives untouched: exactly its own 4 boundary edges, no more, no less.
        assert_eq!(
            boundary_count(&after),
            4,
            "the unreachable open island must be carried through unchanged (4 boundary edges): \
             neither carved, nor healed, nor dropped"
        );
        assert!(after.validate().is_ok(), "installed mesh must validate: {:?}", after.validate());
        assert!(
            after.is_manifold().is_ok(),
            "installed mesh must stay manifold: {:?}",
            after.is_manifold()
        );
    }

    /// Pins the BUG ITSELF as an assertion (the `canonical_edges_keeps_the_rim_of_an_open_mesh`
    /// pattern): the mesh the OLD planner handed the kernel — every non-cutter face fused into
    /// one "remainder" — is OPEN, while the island the cutter actually reaches is CLOSED. If this
    /// ever stops holding, the regression above has stopped testing anything.
    #[test]
    fn the_fused_remainder_is_open_but_the_reached_island_is_closed() {
        let (app, nid, cutter_faces) = three_island_body();
        let mesh = app.poly_node_mesh(nid).expect("body present");
        let cutter: std::collections::HashSet<u32> = cutter_faces.iter().copied().collect();
        let n_live = live_faces(&mesh) as u32;
        assert_eq!(n_live, 13, "fixture layout: 6 cutter + 6 box + 1 plane");

        // What the old code built: EVERY other face, fused.
        let rest: std::collections::HashSet<u32> = (0..n_live).filter(|i| !cutter.contains(i)).collect();
        let fused = crate::poly::boolean::mesh_from_faces_diag(&mesh, &rest).expect("remainder");
        assert!(
            boundary_count(&fused) > 0,
            "the FUSED remainder must be OPEN — that is the input the old planner fed a solid \
             CSG kernel, and why the preflight answered \"target is open\""
        );

        // What the new code builds: only the island the cutter's AABB reaches (the box, faces
        // 6..=11) — closed, so the kernel accepts it.
        let reached: std::collections::HashSet<u32> = (6..12).collect();
        let scoped = crate::poly::boolean::mesh_from_faces_diag(&mesh, &reached).expect("box island");
        assert_eq!(
            boundary_count(&scoped),
            0,
            "the island the cutter reaches is CLOSED — scoping the target is what unblocks the cut"
        );
    }

    /// The cutter island is CONSUMED and the target box really got carved — the subtract did
    /// work, it did not merely "not crash".
    #[test]
    fn island_subtract_removes_the_cutter_and_carves_the_target() {
        let (before, after, _) = run_subtract(false);

        assert_eq!(island_count(&before), 3, "fixture is cutter + box + open plane");
        assert_eq!(
            island_count(&after),
            2,
            "the cutter island must be CONSUMED, leaving the carved box and the untouched plane"
        );
        // The carved pocket carries the cutter's material by default policy — that is the
        // notch surface, not a surviving cutter shell (island_count above proves the shell is
        // gone). Both materials must be present: target walls 0, pocket 1.
        let mats = materials(&after);
        assert!(mats.contains(&0), "the box + plane keep material 0: {mats:?}");
        assert!(
            mats.contains(&1),
            "the carved pocket takes the cutter's material 1 (default policy): {mats:?}"
        );
        // A notch cut into the box top adds geometry the three loose islands never had.
        assert!(
            after.vertex_count() > before.vertex_count() - 8,
            "the box must be carved, not merely stripped of the 8 cutter verts \
             (before {} verts, after {})",
            before.vertex_count(),
            after.vertex_count()
        );
    }

    /// Keep-cutter re-appends the cutter island AND still carries the unreachable island.
    #[test]
    fn island_subtract_keep_cutter_still_carries_the_unreachable_island() {
        let (before, after, toasts) = run_subtract(true);

        assert!(
            !toasts.iter().any(|t| t.contains("is open")),
            "keep-cutter must not feed the unreachable open island to the kernel either: {toasts:?}"
        );
        assert_ne!(
            after.content_signature(),
            before.content_signature(),
            "keep-cutter must still perform the cut"
        );
        assert_eq!(
            boundary_count(&after),
            4,
            "keep-cutter must not lose or re-cut the unreachable open island"
        );
        assert_eq!(
            island_count(&after),
            3,
            "keep-cutter must re-append the cutter island: carved box + plane + cutter"
        );
        assert!(
            materials(&after).contains(&1),
            "the re-appended cutter island keeps material 1: {:?}",
            materials(&after)
        );
    }
}
