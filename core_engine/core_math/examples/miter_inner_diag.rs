//! BEVEL.md §7 — INNER MITER diff against the Blender oracle, on IDENTICAL input.
//!
//! `cargo run -p core_math --example miter_inner_diag`
//!
//! Base mesh is Blender's own `_base` dump (cube ±1 with a 0.6 boss extruded to z=1.8), so the two
//! kernels are fed byte-identical geometry — the §7 rule that topology parity is meaningless unless
//! the input matches. Owner's tool params: width 0.1382, segments 3, shape 0.50, Clamp Overlap on.
//!
//! Blender reference (4.5, generated 2026-08-11):
//!   Miter Inner = SHARP -> nv=28 nf=26     (outer SHARP and ARC both, the corners are all concave)
//!   Miter Inner = ARC   -> nv=60 nf=54

fn main() {
    use core_math::bevel_op::{bevel_edges_offset, BevelParams};
    use core_math::half_edge::*;
    use glam::Vec3;
    use std::collections::HashSet;

    #[rustfmt::skip]
    let pos: Vec<Vec3> = vec![
        Vec3::new(-1.0, -1.0, -1.0), Vec3::new(-0.6, -0.6, 1.0),
        Vec3::new(-1.0,  1.0, -1.0), Vec3::new(-0.6,  0.6, 1.0),
        Vec3::new( 1.0, -1.0, -1.0), Vec3::new( 0.6, -0.6, 1.0),
        Vec3::new( 1.0,  1.0, -1.0), Vec3::new( 0.6,  0.6, 1.0),
        Vec3::new(-1.0, -1.0,  1.0), Vec3::new(-1.0,  1.0, 1.0),
        Vec3::new( 1.0,  1.0,  1.0), Vec3::new( 1.0, -1.0, 1.0),
        Vec3::new( 0.6,  0.6,  1.8), Vec3::new(-0.6,  0.6, 1.8),
        Vec3::new(-0.6, -0.6,  1.8), Vec3::new( 0.6, -0.6, 1.8),
    ];
    #[rustfmt::skip]
    let faces: Vec<Vec<usize>> = vec![
        vec![0, 8, 9, 2],   vec![2, 9, 10, 6], vec![6, 10, 11, 4], vec![4, 11, 8, 0],
        vec![2, 6, 4, 0],
        vec![1, 3, 9, 8],   vec![3, 7, 10, 9], vec![7, 5, 11, 10], vec![5, 1, 8, 11],
        vec![12, 13, 14, 15],
        vec![5, 7, 12, 15], vec![3, 1, 14, 13], vec![1, 5, 15, 14], vec![7, 3, 13, 12],
    ];

    let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);
    println!(
        "BASE  nv={} nf={} manifold={:?}  (Blender base: nv=16 nf=14)",
        mesh.vertex_count(),
        mesh.face_count(),
        mesh.is_manifold().is_ok()
    );

    // The four CONCAVE edges at the boss base: the square (1,3,7,5) at z=1. Selected by explicit
    // vertex pair, not by a concavity classifier, so the selection cannot differ from Blender's.
    let want: [(u32, u32); 4] = [(1, 3), (3, 7), (7, 5), (5, 1)];
    let mut selected: HashSet<u32> = HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        let a = mesh.he_source(h).0;
        let b = mesh.he(h).vertex.0;
        if want.iter().any(|&(x, y)| (a, b) == (x, y) || (a, b) == (y, x)) {
            selected.insert(i);
        }
    }
    // Report the undirected count so a mis-selection is visible rather than silent.
    let undirected: HashSet<u32> = selected
        .iter()
        .map(|&e| e.min(mesh.he(HalfEdgeId(e)).twin.0))
        .collect();
    println!("selected concave edges: {} (undirected {})", selected.len(), undirected.len());

    let params = BevelParams { width: 0.1382, segments: 3, shape: 0.5, clamp: true };
    let out = bevel_edges_offset(&mesh, &selected, &params);
    println!(
        "AZRA  nv={} nf={} manifold={:?} validate={:?}",
        out.vertex_count(),
        out.face_count(),
        out.is_manifold().is_ok(),
        out.validate().is_ok()
    );
    println!("BLENDER inner=SHARP  nv=28 nf=26");
    println!("BLENDER inner=ARC    nv=60 nf=54");

    let mut arity: std::collections::BTreeMap<usize, usize> = Default::default();
    for f in 0..out.face_count() as u32 {
        if out.is_face_live(FaceId(f)) {
            *arity.entry(out.face_vertices(FaceId(f)).len()).or_default() += 1;
        }
    }
    println!("AZRA face arity: {arity:?}");

    // §0.1: counts are necessary, NOT sufficient — emit every position for the vertex-for-vertex
    // diff against the oracle dump. Prefixed so the comparison script can grep them out.
    for v in 0..out.vertex_count() as u32 {
        if !out.is_vert_live(VertexId(v)) {
            continue;
        }
        let p = out.vert(VertexId(v)).position;
        println!("AZRAV {:.6} {:.6} {:.6}", p.x, p.y, p.z);
    }
}
