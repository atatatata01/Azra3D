//! LOCALITY DEFECT DEMONSTRATIONS (2026-08-08 diagnosis task, BEVEL.md §11 2026-08-08b/c).
//!
//! A mesh boolean must be LOCAL: faces the cutter never intersects keep their topology.
//! Both tests below FAIL against the current global-rebuild pipeline and are left in place
//! to be flipped green by the locality fix:
//!
//!   (i)  a scribed (zero-dihedral, knife-slice) edge on a face the cutter does NOT touch
//!        is deleted by the boolean;
//!   (ii) a subdivided cube (8x8 coplanar lattice per face) minus a small corner notch
//!        collapses to ~a plain cube — the whole untouched lattice is merged away.
//!
//! Root causes (diagnosis): the Manifold kernel wrap re-triangulates the WHOLE mesh and the
//! rebuild chain runs GLOBAL passes over it (`merge_coplanar`, `post_boolean_polish`'s
//! `limited_dissolve(…, None, None)` at rebuild.rs which explicitly bypasses the preserve
//! set) — plus upstream Manifold v3 itself simplifies coplanar facets before the wrap
//! ever sees them.

use core_math::half_edge::{HalfEdgeMesh, VertexId, NO_EDGE};
use core_math::mesh_boolean::{boundary_half_edge_count, mesh_boolean, BoolOp};
use glam::Vec3;

/// Closed axis-aligned box [-1,1]^3 from explicit polygons (no helper dependency).
fn unit_cube() -> HalfEdgeMesh {
    let p = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let pos = vec![
        p(-1., -1., -1.), // 0
        p(1., -1., -1.),  // 1
        p(1., 1., -1.),   // 2
        p(-1., 1., -1.),  // 3
        p(-1., -1., 1.),  // 4
        p(1., -1., 1.),   // 5
        p(1., 1., 1.),    // 6
        p(-1., 1., 1.),   // 7
    ];
    // Outward-wound quads.
    let faces = vec![
        vec![0, 3, 2, 1], // z = -1
        vec![4, 5, 6, 7], // z = +1
        vec![0, 1, 5, 4], // y = -1
        vec![2, 3, 7, 6], // y = +1
        vec![0, 4, 7, 3], // x = -1
        vec![1, 2, 6, 5], // x = +1
    ];
    HalfEdgeMesh::from_polygons(&pos, &faces)
}

/// Translate every live vertex of `m` by `d`.
fn translate(m: &mut HalfEdgeMesh, d: Vec3) {
    for i in 0..m.vertices.len() as u32 {
        let v = VertexId(i);
        if m.vert(v).half_edge != NO_EDGE {
            m.vert_mut(v).position += d;
        }
    }
}

/// Does `m` have a live undirected edge whose endpoints match (a, b) within `eps`?
fn has_edge_at(m: &HalfEdgeMesh, a: Vec3, b: Vec3, eps: f32) -> bool {
    use core_math::half_edge::HalfEdgeId;
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !m.is_edge_live(he) {
            continue;
        }
        let p0 = m.vert(m.he_source(he)).position;
        let p1 = m.vert(m.he(he).vertex).position;
        if (p0.distance(a) <= eps && p1.distance(b) <= eps)
            || (p0.distance(b) <= eps && p1.distance(a) <= eps)
        {
            return true;
        }
    }
    false
}

// ============================================================================
// (i) Scribed slice on the TOP face survives a cutter piercing the BOTTOM face
// ============================================================================
//
// Simulates the knife SCRIBE: the top face (z=+1) is pre-split by an interior
// coplanar edge from (-1,0,1) to (1,0,1) — zero dihedral, exactly what
// `poly_knife_free_slice_commit` authors. The cutter then subtracts a small
// notch from the BOTTOM face (z=-1), nowhere near the scribed edge. A local
// boolean keeps the scribed edge; the current pipeline deletes it.
#[test]
fn scribed_coplanar_edge_far_from_cut_survives_boolean() {
    // Build the pre-split cube directly from polygons: top face = two half-quads
    // sharing the scribe edge y=0 on plane z=+1.
    let p = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let pos = vec![
        p(-1., -1., -1.), // 0
        p(1., -1., -1.),  // 1
        p(1., 1., -1.),   // 2
        p(-1., 1., -1.),  // 3
        p(-1., -1., 1.),  // 4
        p(1., -1., 1.),   // 5
        p(1., 1., 1.),    // 6
        p(-1., 1., 1.),   // 7
        p(-1., 0., 1.),   // 8  scribe end (left)
        p(1., 0., 1.),    // 9  scribe end (right)
    ];
    let faces = vec![
        vec![0, 3, 2, 1],    // z=-1
        vec![4, 5, 9, 8],    // z=+1 lower half (y in [-1,0])
        vec![8, 9, 6, 7],    // z=+1 upper half (y in [0,1])
        vec![0, 1, 5, 4],    // y=-1
        vec![2, 3, 7, 6],    // y=+1
        vec![0, 4, 8, 7, 3], // x=-1 (scribe endpoint 8 lies on this wall's top edge)
        vec![1, 2, 6, 9, 5], // x=+1 (scribe endpoint 9 likewise)
    ];
    let cube = HalfEdgeMesh::from_polygons(&pos, &faces);
    assert_eq!(boundary_half_edge_count(&cube), 0, "fixture must be closed");
    assert!(cube.validate().is_ok(), "fixture must validate");
    assert!(
        has_edge_at(&cube, p(-1., 0., 1.), p(1., 0., 1.), 1e-4),
        "fixture must carry the scribed edge"
    );

    // Small cutter box notching the BOTTOM face near a corner: spans z in [-1.2, -0.6],
    // x,y in [0.3, 0.7]. It touches only the z=-1 face and nothing else.
    let mut cutter = unit_cube();
    // scale to a 0.2-half-extent box then translate
    for i in 0..cutter.vertices.len() as u32 {
        let v = VertexId(i);
        if cutter.vert(v).half_edge != NO_EDGE {
            let q = cutter.vert(v).position * 0.2;
            cutter.vert_mut(v).position = q;
        }
    }
    translate(&mut cutter, Vec3::new(0.5, 0.5, -1.0)); // z in [-1.2,-0.8]: pierces z=-1 only

    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("boolean must succeed");
    assert_eq!(boundary_half_edge_count(&res), 0);

    // THE ASSERTION: the scribed edge lies on the far (top) face, untouched by the cutter.
    // Any two collinear sub-segments along y=0/z=1 count as survival (a mid-split is fine);
    // total absence is the defect.
    let survived = has_edge_at(&res, p(-1., 0., 1.), p(1., 0., 1.), 1e-4)
        || has_edge_at(&res, p(-1., 0., 1.), p(0., 0., 1.), 1e-4)
        || has_edge_at(&res, p(0., 0., 1.), p(1., 0., 1.), 1e-4)
        || res
            .vertices
            .iter()
            .enumerate()
            .any(|(i, v)| {
                v.half_edge != NO_EDGE
                    && (v.position.z - 1.0).abs() < 1e-4
                    && v.position.y.abs() < 1e-4
                    && {
                        // any live vertex ON the scribe line still connected along it
                        let vid = VertexId(i as u32);
                        cube_vert_on_scribe_line(&res, vid)
                    }
            });
    eprintln!(
        "[LOCALITY-i] result v={} f={} scribe_survived={}",
        res.vertex_count(),
        res.face_count(),
        survived
    );
    assert!(
        survived,
        "boolean DELETED the scribed coplanar edge on the un-cut top face \
         (result v={} f={})",
        res.vertex_count(),
        res.face_count()
    );
}

/// Any live edge at `v` running along the scribe direction (+/-X at y=0, z=1)?
fn cube_vert_on_scribe_line(m: &HalfEdgeMesh, v: VertexId) -> bool {
    for &he in m.vertex_fan(v).iter() {
        let q = m.vert(m.he(he).vertex).position;
        let p = m.vert(v).position;
        let d = (q - p).normalize_or_zero();
        if d.x.abs() > 0.99 && (q.z - 1.0).abs() < 1e-4 && q.y.abs() < 1e-4 {
            return true;
        }
    }
    false
}

// ============================================================================
// (ii) Subdivided cube keeps its untouched lattice through a small notch subtract
// ============================================================================

/// Closed cube [-1,1]^3 with each face split into an n x n quad grid.
fn grid_cube(n: usize) -> HalfEdgeMesh {
    let mut pos: Vec<Vec3> = Vec::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let mut idx: std::collections::HashMap<[i64; 3], usize> = std::collections::HashMap::new();
    let vid = |pos: &mut Vec<Vec3>, idx: &mut std::collections::HashMap<[i64; 3], usize>, p: Vec3| {
        let k = [
            (p.x * 1e5).round() as i64,
            (p.y * 1e5).round() as i64,
            (p.z * 1e5).round() as i64,
        ];
        *idx.entry(k).or_insert_with(|| {
            pos.push(p);
            pos.len() - 1
        })
    };
    let axes = [(0usize, 1usize, 2usize), (1, 2, 0), (2, 0, 1)];
    for &(a, b, c) in &axes {
        for &s in &[1.0f32, -1.0] {
            for i in 0..n {
                for j in 0..n {
                    let f = |u: f32, v: f32| {
                        let mut p = [0.0f32; 3];
                        p[a] = s;
                        p[b] = -1.0 + 2.0 * u;
                        p[c] = -1.0 + 2.0 * v;
                        Vec3::new(p[0], p[1], p[2])
                    };
                    let (u0, u1) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
                    let (v0, v1) = (j as f32 / n as f32, (j + 1) as f32 / n as f32);
                    let quad = [f(u0, v0), f(u1, v0), f(u1, v1), f(u0, v1)];
                    let ids: Vec<usize> = quad.iter().map(|&p| vid(&mut pos, &mut idx, p)).collect();
                    let n0 = (quad[1] - quad[0]).cross(quad[2] - quad[0]);
                    let outward = {
                        let mut d = [0.0f32; 3];
                        d[a] = s;
                        Vec3::new(d[0], d[1], d[2])
                    };
                    if n0.dot(outward) >= 0.0 {
                        faces.push(ids);
                    } else {
                        faces.push(vec![ids[3], ids[2], ids[1], ids[0]]);
                    }
                }
            }
        }
    }
    HalfEdgeMesh::from_polygons(&pos, &faces)
}

/// Triangular 45° chamfer wedge along part of the +Y/+Z cube edge (x in [x0,x1]),
/// width `w` — the documented BEVEL.md §11 2026-08-08b/c "small wedge" cutter class
/// (`consume_wedge_cut`). Pokes 0.05 outside the cube so no cutter face runs exactly
/// coplanar with a cube wall.
fn wedge_prism(w: f32, x0: f32, x1: f32) -> HalfEdgeMesh {
    let e = 0.05f32;
    let c0 = Vec3::new(0.0, 1.0 - w, 1.0 + e); // on the top face, inset w
    let c1 = Vec3::new(0.0, 1.0 + e, 1.0 - w); // on the +Y face, inset w
    let c2 = Vec3::new(0.0, 1.0 + e, 1.0 + e); // outside the corner
    let a0 = c0 + Vec3::new(x0, 0.0, 0.0);
    let a1 = c1 + Vec3::new(x0, 0.0, 0.0);
    let a2 = c2 + Vec3::new(x0, 0.0, 0.0);
    let b0 = c0 + Vec3::new(x1, 0.0, 0.0);
    let b1 = c1 + Vec3::new(x1, 0.0, 0.0);
    let b2 = c2 + Vec3::new(x1, 0.0, 0.0);
    let pos = vec![a0, a1, a2, b0, b1, b2];
    let faces = vec![
        vec![0, 2, 1],    // cap at x0
        vec![3, 4, 5],    // cap at x1
        vec![0, 1, 4, 3], // slanted 45° face
        vec![1, 2, 5, 4], // y = 1+e face
        vec![2, 0, 3, 5], // z = 1+e face
    ];
    HalfEdgeMesh::from_polygons(&pos, &faces)
}

#[test]
fn subdivided_cube_minus_small_wedge_keeps_untouched_lattice() {
    let cube = grid_cube(8);
    assert!(cube.is_manifold().is_ok(), "grid cube must be manifold");
    assert_eq!(boundary_half_edge_count(&cube), 0);
    let v_in = cube.vertex_count();

    // Small wedge chamfering only the MIDDLE segment of one cube edge (x in
    // [-0.4, 0.4], w=0.6): it intersects a band of the top and +Y faces near
    // that edge and NOTHING else — the bottom, ±X and −Y lattices plus most of
    // the top/+Y lattices are untouched.
    //
    // NOTE: an axis-aligned box notch through the MIDDLE of a face does NOT
    // reproduce the wipe (the hole makes the coplanar region an annulus, which
    // the polish's region-merge refuses) — the edge-clipping wedge leaves every
    // region a simple disk and the global dissolve then eats the whole lattice.
    // That asymmetry is itself part of the defect record.
    let cutter = wedge_prism(0.6, -0.4, 0.4);

    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("boolean must succeed");
    assert_eq!(boundary_half_edge_count(&res), 0);
    eprintln!(
        "[LOCALITY-ii] in v={} f={}  out v={} f={}",
        v_in,
        cube.face_count(),
        res.vertex_count(),
        res.face_count()
    );

    // The wedge band touches at most ~3 lattice rows on two faces. 8x8x6 grid
    // cube has 386 verts. A LOCAL boolean keeps the untouched ones; losing more
    // than half means the global pipeline wiped the lattice (BEVEL.md §11
    // 2026-08-08c documented 386v → 10v on this class of cut).
    assert!(
        res.vertex_count() > v_in / 2,
        "boolean wiped the untouched lattice: {} -> {} verts",
        v_in,
        res.vertex_count()
    );
}
