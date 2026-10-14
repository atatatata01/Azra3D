//! Owner workflow regression: plane → multi loop-cut both orientations (4+4) → knife L @ 45°.
//!
//! The "knife fails to produce straight lines" report after subdividing a plane tracked back to
//! **broken topology from the orthogonal multi loop-cut bug** (directed rail keys skipped faces).
//! With that fixed, a 45° L knife on the resulting grid must stay manifold and put new cut verts
//! on the drawn polyline.
use glam::{Vec2, Vec3};
use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_EDGE, NO_FACE};
use core_math::knife::knife_cut_screen;
use std::collections::HashSet;

fn grid_from_loop_cuts(nu: usize, nv: usize) -> HalfEdgeMesh {
    let positions = [
        Vec3::new(-1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, 1.0),
        Vec3::new(-1.0, 0.0, 1.0),
    ];
    let mut mesh = HalfEdgeMesh::from_quads(&positions[..], &[[0usize, 1, 2, 3]]);
    let he_u = mesh.face(FaceId(0)).half_edge;
    let ts_u: Vec<f32> = (1..=nu).map(|i| i as f32 / (nu as f32 + 1.0)).collect();
    let _ = mesh.loop_cut_multi(he_u, &ts_u, None);
    let he_v = (0..mesh.half_edges.len())
        .map(|i| HalfEdgeId(i as u32))
        .find(|&he| {
            if !mesh.is_edge_live(he) || mesh.he(he).face == NO_FACE {
                return false;
            }
            let a = mesh.vert(mesh.he_source(he)).position;
            let b = mesh.vert(mesh.he(he).vertex).position;
            (b - a).normalize_or_zero().z.abs() > 0.7
        })
        .expect("Z edge for V loop cut");
    let ts_v: Vec<f32> = (1..=nv).map(|i| i as f32 / (nv as f32 + 1.0)).collect();
    let _ = mesh.loop_cut_multi(he_v, &ts_v, None);
    mesh
}

fn corners_l45() -> [Vec3; 3] {
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let rot = |p: Vec3| Vec3::new(p.x * s - p.z * s, 0.0, p.x * s + p.z * s);
    [
        rot(Vec3::new(-0.55, 0.0, -0.25)),
        rot(Vec3::new(0.15, 0.0, -0.25)),
        rot(Vec3::new(0.15, 0.0, 0.45)),
    ]
}

#[test]
fn knife_l45_after_loop_cut_4x4_stays_straight() {
    let mut m = grid_from_loop_cuts(4, 4);
    assert_eq!(m.face_count(), 25, "4 U + 4 V multi → 5×5 quads");
    assert!(
        m.validate().is_ok() && m.is_manifold().is_ok(),
        "loop-cut grid must be clean before knife: val={:?} man={:?}",
        m.validate(),
        m.is_manifold()
    );

    let c = corners_l45();
    let before: HashSet<(i32, i32, i32)> = (0..m.vertices.len() as u32)
        .filter(|&i| m.vert(VertexId(i)).half_edge != NO_EDGE)
        .map(|i| {
            let p = m.vert(VertexId(i)).position;
            (
                (p.x * 1e4).round() as i32,
                (p.y * 1e4).round() as i32,
                (p.z * 1e4).round() as i32,
            )
        })
        .collect();

    // Plane from_quads faces −Y; through=true matches silhouette cuts without a facing fight.
    let project = |p: Vec3| -> Option<Vec2> { Some(Vec2::new(p.x, p.z)) };
    let poly = [
        Vec2::new(c[0].x, c[0].z),
        Vec2::new(c[1].x, c[1].z),
        Vec2::new(c[2].x, c[2].z),
    ];
    let cuts = knife_cut_screen(
        &mut m,
        &project,
        &poly,
        None,
        false,
        true,
        Vec3::NEG_Y,
        None,
        None,
    );
    assert!(cuts >= 2, "L cut must produce geometry, got {cuts}");
    assert!(m.validate().is_ok(), "after knife: {:?}", m.validate());
    assert!(m.is_manifold().is_ok(), "after knife: {:?}", m.is_manifold());

    let mut max_d = 0.0f32;
    let mut n_new = 0u32;
    for i in 0..m.vertices.len() as u32 {
        if m.vert(VertexId(i)).half_edge == NO_EDGE {
            continue;
        }
        let p = m.vert(VertexId(i)).position;
        let key = (
            (p.x * 1e4).round() as i32,
            (p.y * 1e4).round() as i32,
            (p.z * 1e4).round() as i32,
        );
        if before.contains(&key) {
            continue;
        }
        n_new += 1;
        let mut best = f32::MAX;
        for w in c.windows(2) {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let l2 = ab.length_squared().max(1e-20);
            let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
            best = best.min((p - (a + ab * t)).length());
        }
        max_d = max_d.max(best);
    }
    assert!(n_new >= 2, "need new cut verts, got {n_new}");
    assert!(
        max_d < 1e-3,
        "new cut verts must lie on the L polyline (straight), max_d={max_d}"
    );
}
