//! ADVERSARIAL AUDIT of the boolean-locality change (2026-08-08).
//!
//! Attack surface, one test per suspicion:
//!  (1) watertightness / T-junctions at the stitch boundary between preserved and rebuilt
//!      regions — single cut and a SEQUENTIAL second cut (stitch of a stitch);
//!  (2) a cutter face lying exactly ON a preserved face plane (flush contact) — the
//!      SEAM_VERTS exemption removes preserve protection from every authored vert under
//!      the flush footprint, so the polish may wipe authored tessellation the cut never
//!      removed (touching-only difference = geometric no-op);
//!  (3) a cutter entirely inside one preserved face (blind notch) — every authored vert
//!      outside the footprint must survive at its exact position;
//!  (4) nested / multiple islands — a slab split into two shells, and a fully-interior
//!      cutter producing a nested void shell;
//!  (5) material_id retention on preserved faces after the polish/locality gates.
//!
//! All through the public `mesh_boolean` entry (Manifold kernel default), same as the app.

use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_EDGE, NO_FACE, NO_TWIN};
use core_math::mesh_boolean::{boundary_half_edge_count, mesh_boolean, mesh_signed_volume, BoolOp};
use glam::Vec3;

// ============================================================================
// fixtures + helpers
// ============================================================================

/// Closed cube [-1,1]^3 with each face split into an n x n quad grid (same fixture as
/// tests/boolean_locality.rs — duplicated because integration tests cannot share modules).
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

/// Axis-aligned closed box [lo, hi].
fn aabb_box(lo: Vec3, hi: Vec3) -> HalfEdgeMesh {
    let p = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let pos = vec![
        p(lo.x, lo.y, lo.z), // 0
        p(hi.x, lo.y, lo.z), // 1
        p(hi.x, hi.y, lo.z), // 2
        p(lo.x, hi.y, lo.z), // 3
        p(lo.x, lo.y, hi.z), // 4
        p(hi.x, lo.y, hi.z), // 5
        p(hi.x, hi.y, hi.z), // 6
        p(lo.x, hi.y, hi.z), // 7
    ];
    let faces = vec![
        vec![0, 3, 2, 1],
        vec![4, 5, 6, 7],
        vec![0, 1, 5, 4],
        vec![2, 3, 7, 6],
        vec![0, 4, 7, 3],
        vec![1, 2, 6, 5],
    ];
    HalfEdgeMesh::from_polygons(&pos, &faces)
}

fn live_positions(m: &HalfEdgeMesh) -> Vec<Vec3> {
    (0..m.vertices.len() as u32)
        .filter_map(|i| {
            let v = VertexId(i);
            (m.vert(v).half_edge != NO_EDGE).then(|| m.vert(v).position)
        })
        .collect()
}

fn has_live_vert_at(m: &HalfEdgeMesh, p: Vec3, eps: f32) -> bool {
    live_positions(m).iter().any(|q| q.distance(p) <= eps)
}

fn assert_watertight(m: &HalfEdgeMesh, label: &str) {
    assert_eq!(
        boundary_half_edge_count(m),
        0,
        "{label}: open boundary half-edges at the stitch"
    );
    assert!(m.validate().is_ok(), "{label}: validate failed: {:?}", m.validate());
    assert!(m.is_manifold().is_ok(), "{label}: non-manifold: {:?}", m.is_manifold());
}

/// T-junction scan: a live vertex sitting on the INTERIOR of a live edge it is not an
/// endpoint of, within `eps`. The stitch boundary between preserved and rebuilt regions
/// is exactly where these appear when twin pairing across the seam goes wrong.
fn t_junction_count(m: &HalfEdgeMesh, eps: f32) -> usize {
    let verts = live_positions(m);
    let mut edges: Vec<(Vec3, Vec3)> = Vec::new();
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !m.is_edge_live(he) {
            continue;
        }
        let tw = m.he(he).twin;
        if tw != NO_TWIN && tw.0 < i {
            continue; // undirected once
        }
        edges.push((
            m.vert(m.he_source(he)).position,
            m.vert(m.he(he).vertex).position,
        ));
    }
    let mut n = 0;
    for &p in &verts {
        for &(a, b) in &edges {
            if p.distance(a) <= eps || p.distance(b) <= eps {
                continue; // endpoint
            }
            let ab = b - a;
            let len2 = ab.length_squared();
            if len2 <= eps * eps {
                continue;
            }
            let t = (p - a).dot(ab) / len2;
            if t <= 0.0 || t >= 1.0 {
                continue;
            }
            if (a + ab * t).distance(p) <= eps {
                n += 1;
            }
        }
    }
    n
}

/// Every input vertex whose distance to the cutter's AABB exceeds `margin` must survive
/// at its exact position (1e-4). Returns the list of the lost ones.
fn lost_far_verts(input: &HalfEdgeMesh, cutter_lo: Vec3, cutter_hi: Vec3, margin: f32, out: &HalfEdgeMesh) -> Vec<Vec3> {
    let mut lost = Vec::new();
    for p in live_positions(input) {
        let c = p.clamp(cutter_lo, cutter_hi);
        if c.distance(p) <= margin {
            continue; // near the cutter — the boolean owns this region
        }
        if !has_live_vert_at(out, p, 1e-4) {
            lost.push(p);
        }
    }
    lost
}

/// Connected shells (face components over shared live edges).
fn shell_count(m: &HalfEdgeMesh) -> usize {
    let nf = m.faces.len();
    let mut uf: Vec<usize> = (0..nf).collect();
    fn find(uf: &mut Vec<usize>, mut x: usize) -> usize {
        while uf[x] != x {
            uf[x] = uf[uf[x]];
            x = uf[x];
        }
        x
    }
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !m.is_edge_live(he) {
            continue;
        }
        let tw = m.he(he).twin;
        if tw == NO_TWIN {
            continue;
        }
        let (fa, fb) = (m.he(he).face, m.he(tw).face);
        if fa == NO_FACE || fb == NO_FACE {
            continue;
        }
        let (ra, rb) = (find(&mut uf, fa.0 as usize), find(&mut uf, fb.0 as usize));
        if ra != rb {
            uf[ra] = rb;
        }
    }
    let mut roots = std::collections::HashSet::new();
    for i in 0..nf as u32 {
        if m.face(FaceId(i)).half_edge == NO_EDGE {
            continue;
        }
        let r = find(&mut uf, i as usize);
        roots.insert(r);
    }
    roots.len()
}

// ============================================================================
// (2) flush contact: cutter face exactly ON a preserved face plane, no material removed
// ============================================================================

/// A cutter sitting flush ON the subdivided top face (its bottom face coplanar with
/// z = +1, body entirely OUTSIDE the cube). `A - B` removes NOTHING — the correct result
/// is the input, tessellation intact. The SEAM_VERTS exemption strips preservation from
/// every authored vertex under the flush footprint, so a wipe here is the exact bug class
/// the locality change claims to kill, resurrected through its own exemption.
///
/// STATUS 2026-08-08: FAILS (intentionally red, defect record — same convention as
/// tests/boolean_locality.rs). 386v -> 371v: the 5x5 lattice block under the footprint is
/// deleted on a cut that removes zero material. ATTRIBUTION (AZRA_BOOL_DEBUG=1): the RAW
/// Manifold kernel already returns v=371 — Manifold v3 simplifies the coplanar contact
/// facets before the wrap ever sees them, so the locality gates (which baseline on kernel
/// OUTPUT) are blind to it by construction. Note ALSO: even with a kernel-side fix, these
/// verts are all SEAM_VERTS (they lie on the cutter's flush face), so every polish
/// protection is switched off over the footprint — a kernel fix alone would hand the same
/// wipe to `limited_dissolve`. Fix needs BOTH: baseline preserved verts on the INPUTS
/// (not the kernel output) and a seam exemption limited to verts whose incident faces
/// actually changed, not merely verts touching the cutter's surface.
#[test]
#[ignore = "OPEN DEFECT (2026-08-08): flush coplanar contact wipes footprint lattice — Manifold \
kernel simplifies contact facets upstream of every gate AND the seam exemption disables polish \
protection there; fix = input-baselined preserve count + change-based (not footprint-based) seam \
exemption. Un-ignore when fixed. Repro: run with -- --ignored."]
fn flush_touching_difference_is_a_geometric_noop_keeps_lattice() {
    let cube = grid_cube(8);
    let v_in = cube.vertex_count();
    // Bottom face z = 1.0 EXACTLY coplanar with the cube's subdivided top face; footprint
    // [-0.55, 0.55]^2 covers a 4x4 block of the 0.25-pitch lattice including verts at
    // x,y in {-0.5, -0.25, 0, 0.25, 0.5}.
    let cutter = aabb_box(Vec3::new(-0.55, -0.55, 1.0), Vec3::new(0.55, 0.55, 1.6));
    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("touching difference must succeed");
    assert_watertight(&res, "flush-noop");
    assert!(
        (mesh_signed_volume(&res) - mesh_signed_volume(&cube)).abs() < 1e-3,
        "volume changed on a touching-only difference"
    );
    // Every authored vertex must survive — the cut removed no material anywhere.
    let mut lost = Vec::new();
    for p in live_positions(&cube) {
        if !has_live_vert_at(&res, p, 1e-4) {
            lost.push(p);
        }
    }
    eprintln!(
        "[ADV-flush] in v={} out v={} lost={} {:?}",
        v_in,
        res.vertex_count(),
        lost.len(),
        &lost[..lost.len().min(8)]
    );
    assert!(
        lost.is_empty(),
        "flush touching-only difference DELETED {} authored verts (SEAM exemption wipe): {:?}",
        lost.len(),
        &lost[..lost.len().min(8)]
    );
}

/// Same flush plane but a REAL cut: the cutter's top face is coplanar with the cube's
/// subdivided top face and the body recesses INTO the solid. Authored verts on the top
/// face OUTSIDE the footprint are far from every cutter face except the coplanar one —
/// they lie on its PLANE but not its surface, and must survive.
#[test]
fn flush_recess_keeps_out_of_footprint_top_lattice() {
    let cube = grid_cube(8);
    // Top face z = 1.0 coincident with cube top; recess depth 0.5; footprint offset from
    // the lattice pitch (0.25) so the walls are in generic position.
    let (lo, hi) = (Vec3::new(-0.32, -0.32, 0.5), Vec3::new(0.32, 0.32, 1.0));
    let cutter = aabb_box(lo, hi);
    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("flush recess must succeed");
    assert_watertight(&res, "flush-recess");
    let want = mesh_signed_volume(&cube) - 0.64 * 0.64 * 0.5;
    assert!(
        (mesh_signed_volume(&res) - want).abs() < 1e-2,
        "recess volume wrong: {} vs {}",
        mesh_signed_volume(&res),
        want
    );
    let lost = lost_far_verts(&cube, lo, hi, 0.05, &res);
    eprintln!(
        "[ADV-flush-recess] out v={} f={} lost_far={} {:?}",
        res.vertex_count(),
        res.face_count(),
        lost.len(),
        &lost[..lost.len().min(8)]
    );
    assert!(
        lost.is_empty(),
        "flush recess deleted {} authored verts OUTSIDE the footprint: {:?}",
        lost.len(),
        &lost[..lost.len().min(8)]
    );
}

// ============================================================================
// (3) cutter entirely inside ONE preserved face (blind notch)
// ============================================================================

#[test]
fn blind_notch_inside_one_face_keeps_every_far_vert() {
    let cube = grid_cube(8);
    // Straddles the top face only: z in [0.8, 1.2], footprint in generic position.
    let (lo, hi) = (Vec3::new(-0.31, -0.29, 0.8), Vec3::new(0.33, 0.27, 1.2));
    let cutter = aabb_box(lo, hi);
    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("blind notch must succeed");
    assert_watertight(&res, "blind-notch");
    assert_eq!(t_junction_count(&res, 2e-5), 0, "T-junctions at the notch stitch");
    let want = mesh_signed_volume(&cube) - (0.64 * 0.56 * 0.2) as f64;
    assert!(
        (mesh_signed_volume(&res) - want).abs() < 1e-2,
        "notch volume wrong: {} vs {}",
        mesh_signed_volume(&res),
        want
    );
    let lost = lost_far_verts(&cube, lo, hi, 0.05, &res);
    eprintln!(
        "[ADV-notch] out v={} f={} lost_far={}",
        res.vertex_count(),
        res.face_count(),
        lost.len()
    );
    assert!(
        lost.is_empty(),
        "blind notch deleted {} authored verts far from the cutter: {:?}",
        lost.len(),
        &lost[..lost.len().min(8)]
    );
}

// ============================================================================
// (1) stitch watertightness — sequential cuts (the stitch of a stitch)
// ============================================================================

#[test]
fn two_sequential_cuts_stay_watertight_and_local() {
    let cube = grid_cube(8);
    let (lo1, hi1) = (Vec3::new(-0.31, -0.29, 0.8), Vec3::new(0.33, 0.27, 1.2));
    let first = mesh_boolean(&cube, &aabb_box(lo1, hi1), BoolOp::Difference).expect("cut 1");
    assert_watertight(&first, "cut-1");
    // Second cut on a DIFFERENT face, far from the first: -X face notch.
    let (lo2, hi2) = (Vec3::new(-1.2, -0.41, -0.38), Vec3::new(-0.8, 0.22, 0.19));
    let second = mesh_boolean(&first, &aabb_box(lo2, hi2), BoolOp::Difference).expect("cut 2");
    assert_watertight(&second, "cut-2");
    assert_eq!(
        t_junction_count(&second, 2e-5),
        0,
        "T-junctions after sequential cuts (stitch across generations)"
    );
    // The first notch's rim geometry (result of cut 1) is authored input for cut 2 and is
    // FAR from cutter 2 — it must survive verbatim, as must the original far lattice.
    let lost = lost_far_verts(&first, lo2, hi2, 0.05, &second);
    eprintln!(
        "[ADV-seq] cut1 v={} cut2 v={} lost_far={}",
        first.vertex_count(),
        second.vertex_count(),
        lost.len()
    );
    assert!(
        lost.is_empty(),
        "second cut deleted {} verts far away from it (incl. first cut's rim): {:?}",
        lost.len(),
        &lost[..lost.len().min(8)]
    );
}

// ============================================================================
// (4) islands: slab split into two shells; fully-interior cutter -> nested void
// ============================================================================

#[test]
fn slab_split_produces_two_watertight_shells_preserving_far_lattice() {
    let cube = grid_cube(8);
    // Full-overhang slab in generic position (avoids the 0.25 lattice planes).
    let (lo, hi) = (Vec3::new(-1.3, -1.3, -0.29), Vec3::new(1.3, 1.3, 0.31));
    let res = mesh_boolean(&cube, &aabb_box(lo, hi), BoolOp::Difference).expect("slab split");
    assert_watertight(&res, "slab-split");
    assert_eq!(shell_count(&res), 2, "expected exactly two disjoint shells");
    let want = mesh_signed_volume(&cube) - (4.0 * 0.6) as f64;
    assert!(
        (mesh_signed_volume(&res) - want).abs() < 1e-2,
        "split volume wrong: {} vs {}",
        mesh_signed_volume(&res),
        want
    );
    let lost = lost_far_verts(&cube, lo, hi, 0.05, &res);
    assert!(
        lost.is_empty(),
        "slab split deleted {} verts outside the slab: {:?}",
        lost.len(),
        &lost[..lost.len().min(8)]
    );
}

#[test]
fn interior_cutter_produces_nested_void_shell() {
    let cube = grid_cube(4);
    // Fully interior cutter — every cutter vert is strictly inside A (this is the case the
    // SEAM comment claims "never survives the kernel", which is false for difference).
    let cutter = aabb_box(Vec3::new(-0.4, -0.4, -0.4), Vec3::new(0.4, 0.4, 0.4));
    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("hollow must succeed");
    assert_eq!(boundary_half_edge_count(&res), 0, "hollow: open boundary");
    assert!(res.validate().is_ok(), "hollow: validate failed");
    assert_eq!(shell_count(&res), 2, "expected outer shell + nested void shell");
    let want = mesh_signed_volume(&cube) - (0.8f32 * 0.8 * 0.8) as f64;
    assert!(
        (mesh_signed_volume(&res) - want).abs() < 1e-2,
        "hollow volume wrong: {} vs {}",
        mesh_signed_volume(&res),
        want
    );
    // Outer lattice untouched; void shell must carry the cutter's 8 corners.
    for p in live_positions(&cube) {
        assert!(
            has_live_vert_at(&res, p, 1e-4),
            "hollow: outer authored vert {p:?} deleted"
        );
    }
    for p in live_positions(&cutter) {
        assert!(
            has_live_vert_at(&res, p, 1e-4),
            "hollow: void-shell vert {p:?} missing"
        );
    }
}

// ============================================================================
// (5) material_id on preserved faces
// ============================================================================

#[test]
fn materials_survive_on_untouched_faces_after_local_cut() {
    let mut cube = grid_cube(8);
    // Material by outward axis: +X=1 -X=2 +Y=3 -Y=4 +Z=5 -Z=6.
    for i in 0..cube.faces.len() as u32 {
        let fid = FaceId(i);
        if cube.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let n = cube.compute_face_normal(fid).normalize_or_zero();
        let mat = if n.x > 0.9 {
            1
        } else if n.x < -0.9 {
            2
        } else if n.y > 0.9 {
            3
        } else if n.y < -0.9 {
            4
        } else if n.z > 0.9 {
            5
        } else {
            6
        };
        cube.face_mut(fid).material_id = mat;
    }
    // Notch the top face only.
    let cutter = aabb_box(Vec3::new(-0.31, -0.29, 0.8), Vec3::new(0.33, 0.27, 1.2));
    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("material cut");
    assert_watertight(&res, "materials");
    let mut wrong = Vec::new();
    for i in 0..res.faces.len() as u32 {
        let fid = FaceId(i);
        if res.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs = res.face_vertices(fid);
        let c = vs.iter().map(|&v| res.vert(v).position).sum::<Vec3>() / vs.len() as f32;
        // Untouched planar groups, sampled away from any edge/corner ambiguity.
        let want = if (c.z + 1.0).abs() < 1e-3 {
            Some(6) // bottom
        } else if (c.y + 1.0).abs() < 1e-3 {
            Some(4) // -Y wall
        } else if (c.x + 1.0).abs() < 1e-3 {
            Some(2) // -X wall
        } else {
            None
        };
        if let Some(w) = want {
            let got = res.face(fid).material_id;
            if got != w {
                wrong.push((c, got, w));
            }
        }
    }
    eprintln!("[ADV-mat] out f={} wrong={}", res.face_count(), wrong.len());
    assert!(
        wrong.is_empty(),
        "{} preserved faces lost their material: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(6)]
    );
}
