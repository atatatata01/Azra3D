//! APP-PATH pin for POLY-BOOL-NGON-BEVEL (owner 2026-10-13, `bollean.fl4`, preserved as
//! `docs/planning/poly/repro/bool_ngon_bevel_2026-10-13.fl4`): *"boolean subtract with a
//! complex rounded-bevel cutter shape on an n-gon mesh does not create the proper result"*.
//!
//! The file holds ONE `PolygonalBody` with two closed islands: an arch block (534 faces, big
//! 36-gon sides, a finely-segmented curved top) and the cutter (74 faces: a box whose corner
//! carries a round bevel in 2.8° and 1.4° strips). The owner selected the cutter island in
//! edit mode and ran the X-pie Subtract.
//!
//! Root cause (core_math `mesh_boolean/rebuild.rs`): the post-boolean polish merged faces by a
//! 12° NORMAL ANGLE (and `merge_coplanar` by 2.56°), not by coplanarity. The cutter's strips
//! were dissolved into the notch floor and into the arch's own facets, installing bent n-gons
//! (worst 0.011 out of plane) — the partial rounded channel, slanted slivers and stray edges
//! of the screenshots — and moving the solid by 0.0045. Now every merge must keep the merged
//! face on one plane at float precision.
//!
//! Project rule `feedback_verify_app_path_not_kernel`: the regression drives the REAL X-pie
//! entry (`poly_boolean_execute`) on a headless `AzraApp` and reads the mesh back.
//!
//! Run: cargo test -p app diag_bool_ngon_bevel -- --nocapture
#[cfg(test)]
mod tests {
    use crate::project::load_binary;
    use crate::{AzraApp, PolyWorkMode, SceneItemId};
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_FACE, NO_TWIN};
    use core_math::mesh_boolean::{mesh_signed_volume, BoolOp};
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;
    use std::collections::HashSet;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\..\..\docs\planning\poly\repro\bool_ngon_bevel_2026-10-13.fl4"
    );

    /// Exact difference volume of the two islands, from Manifold's own resolved solids
    /// (`ma.difference(&mb).volume()`, measured 2026-10-13). The pre-fix install measured
    /// 12.662348 — the bent faces had moved the surface by 0.0045.
    const EXACT_VOLUME: f64 = 12.657803;

    fn boundary_count(m: &HalfEdgeMesh) -> usize {
        (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).face != NO_FACE && m.he(h).twin == NO_TWIN)
            .count()
    }

    fn live_faces(m: &HalfEdgeMesh) -> Vec<FaceId> {
        (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| m.face(f).half_edge != NO_EDGE)
            .collect()
    }

    /// Face islands via shared vertices (the X-pie planner's rule), largest first.
    fn islands(m: &HalfEdgeMesh) -> Vec<HashSet<u32>> {
        let fs = live_faces(m);
        let mut parent: Vec<usize> = (0..m.faces.len()).collect();
        fn find(p: &mut Vec<usize>, mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        let mut owner: std::collections::HashMap<u32, usize> = Default::default();
        for &f in &fs {
            for v in m.face_vertices(f) {
                match owner.get(&v.0) {
                    Some(&o) => {
                        let (a, b) = (find(&mut parent, f.0 as usize), find(&mut parent, o));
                        if a != b {
                            parent[a] = b;
                        }
                    }
                    None => {
                        owner.insert(v.0, f.0 as usize);
                    }
                }
            }
        }
        let mut by_root: std::collections::BTreeMap<usize, HashSet<u32>> = Default::default();
        for &f in &fs {
            let r = find(&mut parent, f.0 as usize);
            by_root.entry(r).or_default().insert(f.0);
        }
        let mut out: Vec<HashSet<u32>> = by_root.into_values().collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.len()));
        out
    }

    /// Worst distance of a face's vertex from the face's own Newell plane, and how many faces
    /// exceed `tol`.
    fn planarity(m: &HalfEdgeMesh, tol: f32) -> (f32, usize) {
        let (mut worst, mut bent) = (0.0f32, 0usize);
        for f in live_faces(m) {
            let pts: Vec<Vec3> = m.face_vertices(f).iter().map(|&v| m.vert(v).position).collect();
            let n = m.compute_face_normal(f).normalize_or_zero();
            let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
            let d = pts.iter().map(|p| (*p - c).dot(n).abs()).fold(0.0f32, f32::max);
            worst = worst.max(d);
            if d > tol {
                bent += 1;
            }
        }
        (worst, bent)
    }

    /// Load the owner's body into a headless app and replay his gesture: edit mode, Face
    /// select, the cutter island selected, X-pie Subtract (cutter consumed).
    fn run_owner_subtract() -> (HalfEdgeMesh, Vec<String>) {
        let pf = load_binary(std::path::Path::new(FIXTURE)).expect("load bool_ngon_bevel fixture");
        let g = pf.scene_graph.as_ref().expect("graph");
        let (_, node) = g
            .all_nodes()
            .find(|(_, n)| matches!(n.payload, NodePayload::PolygonalBody { .. }))
            .expect("the fixture holds one poly body");
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { unreachable!() };
        let mesh: HalfEdgeMesh = (**mesh).clone();
        let isl = islands(&mesh);
        assert_eq!(isl.len(), 2, "fixture: arch block + cutter island");
        assert_eq!((isl[0].len(), isl[1].len()), (534, 74), "fixture island sizes");
        let cutter_faces = isl[1].clone();

        let mut app = AzraApp::new_headless();
        app.modeling_context = crate::ModelingContext::Polygonal;
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Poly.box".into(),
                mesh: std::sync::Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(SceneItemId::Node(nid));
        app.poly_work_mode = PolyWorkMode::Face;
        app.poly_sel.body_stamp = Some(nid.0);
        app.poly_sel.selected_faces = cutter_faces.into_iter().collect();
        app.ui.bool_pie_keep_cutter = false;
        app.ui.toasts.clear();
        app.poly_boolean_execute(BoolOp::Difference); // ← the real X-pie entry
        let toasts: Vec<String> = app.ui.toasts.iter().map(|t| t.message.clone()).collect();
        let after = app.poly_node_mesh(nid).expect("body still present");
        ((*after).clone(), toasts)
    }

    /// THE regression: the owner's subtract installs a closed, unbent, volume-exact result.
    #[test]
    fn owner_rounded_bevel_subtract_is_planar_and_exact() {
        let (res, toasts) = run_owner_subtract();
        for t in &toasts {
            eprintln!("[toast] {t}");
        }
        assert_eq!(islands(&res).len(), 1, "the cutter island is consumed; one solid remains");
        assert_eq!(boundary_count(&res), 0, "watertight");
        assert!(res.validate().is_ok(), "valid: {:?}", res.validate().err());
        assert!(res.is_edge_manifold().is_ok(), "edge-manifold");
        assert_eq!(res.count_folded_coplanar_pairs(), 0, "no folded flaps");

        // Pre-fix: 9 faces bent by up to 0.011 (a 27-gon floor that swallowed the channel,
        // five 18-gon arch facets that swallowed grazing cutter strips, ...).
        let (worst, bent) = planarity(&res, 1e-4);
        assert_eq!(bent, 0, "bent n-gons installed (worst {worst}) — the polish merged across a curve");

        let vol = mesh_signed_volume(&res);
        assert!(
            (vol - EXACT_VOLUME).abs() < 1e-4,
            "volume {vol:.6} vs exact {EXACT_VOLUME:.6} — the installed surface was moved"
        );
    }

    /// Result health of the owner's subtract. Diagnostic only.
    /// Run: cargo test -p app diag_bool_ngon_bevel_dump -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diag_bool_ngon_bevel_dump() {
        let (res, toasts) = run_owner_subtract();
        let mut hist = std::collections::BTreeMap::new();
        for f in live_faces(&res) {
            *hist.entry(res.face_vertices(f).len()).or_insert(0usize) += 1;
        }
        let (worst, bent) = planarity(&res, 1e-4);
        eprintln!(
            "result v={} f={} boundary={} folds={} vol={:.6} bent={bent} worst={worst:.6} ring-hist={hist:?} toasts={toasts:?}",
            res.vertex_count(),
            res.face_count(),
            boundary_count(&res),
            res.count_folded_coplanar_pairs(),
            mesh_signed_volume(&res),
        );
        // AZRA_BNG_OBJ=<path>: write the result as OBJ, bent faces (> 1e-4) in group `bent`.
        if let Ok(path) = std::env::var("AZRA_BNG_OBJ") {
            let mut s = String::new();
            for v in &res.vertices {
                s += &format!("v {} {} {}\n", v.position.x, v.position.y, v.position.z);
            }
            for f in live_faces(&res) {
                let vs = res.face_vertices(f);
                let pts: Vec<Vec3> = vs.iter().map(|&v| res.vert(v).position).collect();
                let n = res.compute_face_normal(f).normalize_or_zero();
                let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
                let d = pts.iter().map(|p| (*p - c).dot(n).abs()).fold(0.0f32, f32::max);
                s += if d > 1e-4 { "g bent\n" } else { "g ok\n" };
                s += "f";
                for v in vs {
                    s += &format!(" {}", v.0 + 1);
                }
                s += "\n";
            }
            std::fs::write(&path, s).expect("write obj");
            eprintln!("wrote {path}");
        }
    }
}
