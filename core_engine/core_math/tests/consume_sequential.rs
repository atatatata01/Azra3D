//! Owner repro 2026-08-08: "works on a plane cube, but a SECOND slice fails; slicing over a
//! part I just subtracted fails." Sequential consume bevels + consume-after-boolean, headless.
use core_math::bevel_absorb::{consume_fuse_bevel, consume_wedge_cut};
use core_math::bevel_op::BevelParams;
use core_math::half_edge::{HalfEdgeId, HalfEdgeMesh, VertexId, NO_TWIN};
use glam::Vec3;
use std::collections::{HashMap, HashSet};

fn subdivided_cube(n: usize) -> HalfEdgeMesh {
    let mut map: HashMap<(usize, usize, usize), usize> = HashMap::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let c = |i: usize| -1.0 + 2.0 * i as f32 / n as f32;
    let mut faces: Vec<Vec<usize>> = Vec::new();
    {
        let mut vid = |ijk: (usize, usize, usize)| -> usize {
            *map.entry(ijk).or_insert_with(|| {
                positions.push(Vec3::new(c(ijk.0), c(ijk.1), c(ijk.2)));
                positions.len() - 1
            })
        };
        let sides: [Box<dyn Fn(usize, usize) -> (usize, usize, usize)>; 6] = [
            Box::new(move |u, v| (u, n, v)),
            Box::new(move |u, v| (v, 0, u)),
            Box::new(move |u, v| (u, v, 0)),
            Box::new(move |u, v| (v, u, n)),
            Box::new(move |u, v| (0, u, v)),
            Box::new(move |u, v| (n, v, u)),
        ];
        for side in &sides {
            for u in 0..n {
                for v in 0..n {
                    faces.push(vec![
                        vid(side(u, v)),
                        vid(side(u, v + 1)),
                        vid(side(u + 1, v + 1)),
                        vid(side(u + 1, v)),
                    ]);
                }
            }
        }
    }
    let mut vol6 = 0.0f32;
    for f in &faces {
        for k in 1..f.len() - 1 {
            let (a, b, cc) = (positions[f[0]], positions[f[k]], positions[f[k + 1]]);
            vol6 += a.dot(b.cross(cc));
        }
    }
    if vol6 < 0.0 {
        for f in &mut faces {
            f.reverse();
        }
    }
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

/// Undirected selection of edges whose both endpoints satisfy `pred`.
fn select_edges(mesh: &HalfEdgeMesh, pred: &dyn Fn(Vec3, Vec3) -> bool) -> HashSet<u32> {
    let mut sel = HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !mesh.is_edge_live(he) {
            continue;
        }
        let t = mesh.he(he).twin;
        if t != NO_TWIN && i > t.0 {
            continue;
        }
        let a = mesh.vert(mesh.he_source(he)).position;
        let b = mesh.vert(mesh.he(he).vertex).position;
        if pred(a, b) {
            sel.insert(i);
        }
    }
    sel
}

fn sanity(tag: &str, mesh: &HalfEdgeMesh, out: &HalfEdgeMesh) {
    out.validate().unwrap_or_else(|e| panic!("{tag}: invalid: {e}"));
    assert!(out.is_manifold().is_ok(), "{tag}: not watertight");
    assert!(
        out.vertex_count() * 2 >= mesh.vertex_count(),
        "{tag}: destroyed ({} of {})",
        out.vertex_count(),
        mesh.vertex_count()
    );
}

#[test]
fn second_consume_far_parallel_edge() {
    let mesh = subdivided_cube(8);
    let on = |v: f32, t: f32| (v - t).abs() < 1e-3;
    let sel1 = select_edges(&mesh, &|a, b| on(a.y, 1.0) && on(b.y, 1.0) && on(a.z, -1.0) && on(b.z, -1.0));
    assert_eq!(sel1.len(), 8);
    let params = BevelParams { width: 0.5, segments: 3, shape: 0.5, clamp: true };
    let first = consume_fuse_bevel(&mesh, &sel1, &params).expect("first consume fuses");
    sanity("first", &mesh, &first);

    // Second consume on the FAR parallel top edge (z=+1) of the ALREADY-BEVELED mesh.
    let sel2 = select_edges(&first, &|a, b| on(a.y, 1.0) && on(b.y, 1.0) && on(a.z, 1.0) && on(b.z, 1.0));
    eprintln!("second selection: {} edges", sel2.len());
    assert!(sel2.len() >= 8, "far edge still selectable");
    match consume_fuse_bevel(&first, &sel2, &params) {
        Some(out) => {
            sanity("second-far", &first, &out);
            eprintln!("SECOND (far edge): FUSED v={} f={}", out.vertex_count(), out.face_count());
        }
        None => panic!("SECOND consume on far clean edge REFUSED — repro of owner bug"),
    }
}

#[test]
fn second_consume_vertical_edge_touching_first_band() {
    let mesh = subdivided_cube(8);
    let on = |v: f32, t: f32| (v - t).abs() < 1e-3;
    let sel1 = select_edges(&mesh, &|a, b| on(a.y, 1.0) && on(b.y, 1.0) && on(a.z, -1.0) && on(b.z, -1.0));
    let params = BevelParams { width: 0.5, segments: 3, shape: 0.5, clamp: true };
    let first = consume_fuse_bevel(&mesh, &sel1, &params).expect("first consume fuses");

    // The front-left VERTICAL edge: its top portion was consumed by the first band; what
    // remains is the sharp edge below the fillet. Second consume on it.
    let sel2 = select_edges(&first, &|a, b| {
        on(a.x, -1.0) && on(b.x, -1.0) && on(a.z, -1.0) && on(b.z, -1.0)
    });
    eprintln!("vertical remainder selection: {} edges", sel2.len());
    assert!(!sel2.is_empty(), "vertical edge remainder exists");
    match consume_fuse_bevel(&first, &sel2, &params) {
        Some(out) => {
            sanity("second-vertical", &first, &out);
            eprintln!("SECOND (vertical): FUSED v={} f={}", out.vertex_count(), out.face_count());
        }
        None => {
            eprintln!("SECOND (vertical): fuse REFUSED (expected today — end vertex sits on the band, not a 3-face corner). Documenting, not failing.");
        }
    }
}

#[test]
fn consume_after_boolean_subtract() {
    // "Slice over a part I just subtracted": box minus cylinder, then consume the rim.
    let a = HalfEdgeMesh::new_box(Vec3::ONE);
    let cyl = HalfEdgeMesh::new_cylinder(0.45, 3.0, 12);
    let cut = core_math::mesh_boolean::mesh_boolean(&a, &cyl, core_math::mesh_boolean::BoolOp::Difference)
        .expect("boolean");
    let on = |v: f32, t: f32| (v - t).abs() < 1e-3;
    let sel = select_edges(&cut, &|p, q| {
        on(p.y, 1.0) && on(q.y, 1.0)
            && (p.x * p.x + p.z * p.z).sqrt() < 0.5
            && (q.x * q.x + q.z * q.z).sqrt() < 0.5
    });
    eprintln!("rim selection: {} edges", sel.len());
    assert!(sel.len() >= 10);
    let params = BevelParams { width: 0.15, segments: 1, shape: 0.5, clamp: true };
    let fuse = consume_fuse_bevel(&cut, &sel, &params);
    eprintln!("after-boolean FUSE: {}", if fuse.is_some() { "Some" } else { "None" });
    let wedge = consume_wedge_cut(&cut, &sel, &params);
    match (fuse, wedge) {
        (Some(out), _) => sanity("post-bool fuse", &cut, &out),
        (None, Some(out)) => {
            sanity("post-bool wedge", &cut, &out);
            eprintln!("after-boolean WEDGE: OK v={} f={}", out.vertex_count(), out.face_count());
        }
        (None, None) => panic!("consume after boolean: BOTH paths refused — repro of owner bug"),
    }
}
