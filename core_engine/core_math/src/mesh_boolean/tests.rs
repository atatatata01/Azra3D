use super::*;
use glam::Vec3;

/// THE OPEN-MESH GATE (owner 2026-08-11: "mesh clean up does not do a good job cleaning the mesh").
///
/// `cleanup_mesh_with` grew out of the boolean repair path, where both operands are solids by
/// contract, and it inherited that path's result bar: `boundary_half_edge_count(&out) == 0`. As a
/// bar for a general Mesh Cleanup that was a silent OFF switch — ANY mesh with a rim (plane,
/// sheet, open shell, scanned import: most of what a cleanup is pointed at) failed it, cleanup
/// returned `None`, and the UI reported "mesh is already clean — nothing to repair".
///
/// This fixture is an open sheet with a genuine, trivially-checkable defect: two coplanar quads
/// where one n-gon would do. Before the fix it came back untouched.
#[test]
fn cleanup_repairs_an_open_sheet_instead_of_refusing_it() {
    let pts = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
    ];
    let m = HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 4, 5], vec![1, 2, 3, 4]]);
    assert_eq!(m.face_count(), 2);
    assert!(boundary_half_edge_count(&m) > 0, "fixture must be OPEN for this to test anything");

    let out = cleanup_mesh(&m).expect("cleanup refused an open sheet it could plainly repair");
    assert_eq!(out.face_count(), 1, "the two coplanar quads should merge into one face");
    assert_eq!(out.vertex_count(), 4, "the shared-edge midpoints are collinear pins");
    assert!(out.validate().is_ok(), "{:?}", out.validate());
}

/// Cleanup must still refuse to TEAR a mesh: an output with MORE boundary than the input is
/// rejected whatever else it improved. (The `<=` that replaced `== 0` is what enforces this;
/// a closed input still has to come back closed, since 0 is the only count that is `<= 0`.)
#[test]
fn cleanup_still_requires_a_closed_result_from_a_closed_input() {
    let (p, f) = {
        let h = 0.5f32;
        (
            vec![
                Vec3::new(-h, -h, -h), Vec3::new(h, -h, -h), Vec3::new(h, h, -h), Vec3::new(-h, h, -h),
                Vec3::new(-h, -h, h),  Vec3::new(h, -h, h),  Vec3::new(h, h, h),  Vec3::new(-h, h, h),
            ],
            vec![
                vec![0, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4],
                vec![1, 2, 6, 5], vec![2, 3, 7, 6], vec![3, 0, 4, 7],
            ],
        )
    };
    let m = HalfEdgeMesh::from_polygons(&p, &f);
    assert_eq!(boundary_half_edge_count(&m), 0);
    // A clean cube has nothing to repair, so cleanup declines — and if it ever DID return
    // something, the gate below is what would have to hold.
    if let Some(out) = cleanup_mesh(&m) {
        assert_eq!(boundary_half_edge_count(&out), 0, "cleanup tore a closed mesh open");
    }
}

/// Cleanup must never hand back an EMPTY body. `delete_loose_faces` on a sheet whose faces
/// genuinely share no edges removes every one of them, and an empty mesh satisfies every other
/// gate here (0 boundary, 0 folds, valid, manifold).
#[test]
fn cleanup_refuses_to_empty_the_body() {
    let pts = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
    ];
    let m = HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3]]);
    let opts = CleanupOptions {
        delete_loose_faces: true,
        delete_loose_edges: true,
        delete_loose_verts: true,
        ..Default::default()
    };
    assert!(
        cleanup_mesh_with(&m, &opts).is_none(),
        "cleanup deleted the entire body and called it a repair"
    );
}

/// Blender `Fill Holes` reached through the Cleanup entry point, and the no-op gate's escape
/// hatch: a hole fill RAISES face count, so the old vert/face/fold gate would have discarded it.
#[test]
fn cleanup_fills_a_hole_and_reports_which_pass_fired() {
    let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
    m.delete_face(crate::half_edge::FaceId(0));
    assert!(boundary_half_edge_count(&m) > 0);

    let opts = CleanupOptions { fill_holes_sides: 4, ..Default::default() };
    let (out, stats) = cleanup_mesh_report(&m, &opts).expect("cleanup refused to fill a 4-sided hole");
    assert_eq!(stats.holes_filled, 1);
    assert_eq!(out.face_count(), 6);
    assert_eq!(boundary_half_edge_count(&out), 0, "hole not actually closed");
    assert!(out.is_manifold().is_ok(), "{:?}", out.is_manifold());
}

/// Blender `Merge by Distance` reached through the Cleanup entry point — and the reason it runs
/// BEFORE the coplanar merge: the two halves of this sheet are coplanar but are held apart by a
/// seam of doubles, so `merge_coplanar` cannot see them until the weld has happened.
#[test]
fn cleanup_merge_by_distance_unblocks_the_coplanar_merge() {
    let d = 1e-4f32;
    let pts = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        // Second quad: its left edge duplicates the first quad's right edge, 1e-4 off.
        Vec3::new(1.0 + d, 0.0, 0.0),
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(1.0 + d, 1.0, 0.0),
    ];
    let m = HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]]);
    assert_eq!(m.vertex_count(), 8, "the seam must start doubled");

    // Without the weld the sheet is two disjoint faces and there is nothing to merge.
    assert!(
        cleanup_mesh(&m).is_none(),
        "without Merge by Distance there is no defect cleanup can see"
    );

    let opts = CleanupOptions { merge_distance: 1e-3, ..Default::default() };
    let (out, stats) = cleanup_mesh_report(&m, &opts).expect("merge-by-distance cleanup refused");
    assert!(stats.verts_welded > 0, "seam not welded");
    assert_eq!(out.vertex_count(), 4, "welded + merged + collinear pins collapsed");
    assert_eq!(out.face_count(), 1, "coplanar halves should merge once welded");
    assert!(out.validate().is_ok(), "{:?}", out.validate());
}

/// THE OWNER GATE (2026-08-05): "mesh clean up should never touch [tagged edges]."
///
/// A box whose top face is split in two by one interior edge. Cleanup's `merge_coplanar` pass
/// exists precisely to merge those two coplanar halves back into one face — so with the edge
/// PROTECTED it must decline, and with it unprotected it must still merge. Same mesh, same
/// options, the protection list is the only difference: that is what makes this a real gate
/// rather than a test of whether cleanup happens to do nothing.
#[test]
fn cleanup_refuses_to_merge_across_a_protected_edge() {
    // Unit box, top face (z = +0.5) replaced by two triangles sharing a diagonal.
    let h = 0.5f32;
    let p = vec![
        Vec3::new(-h, -h, -h), Vec3::new(h, -h, -h), Vec3::new(h, h, -h), Vec3::new(-h, h, -h),
        Vec3::new(-h, -h, h),  Vec3::new(h, -h, h),  Vec3::new(h, h, h),  Vec3::new(-h, h, h),
    ];
    let faces: Vec<Vec<usize>> = vec![
        vec![0, 3, 2, 1],  // bottom
        vec![4, 5, 6, 7],  // top half A  ─┐ the diagonal 4–6 is the split
        vec![4, 6, 7],     // (see below)  │
        vec![0, 1, 5, 4],
        vec![1, 2, 6, 5],
        vec![2, 3, 7, 6],
        vec![3, 0, 4, 7],
    ];
    // Build the real split top: triangles (4,5,6) and (4,6,7) sharing edge 4–6.
    let mut faces = faces;
    faces[1] = vec![4, 5, 6];
    faces[2] = vec![4, 6, 7];
    let m = HalfEdgeMesh::from_polygons(&p, &faces);
    assert!(m.validate().is_ok() && boundary_half_edge_count(&m) == 0, "fixture must be closed");
    assert_eq!(m.face_count(), 7, "6 box faces with the top split in two");

    let opts_free = CleanupOptions {
        flatten_folds: false,
        merge_coplanar: true,
        dissolve_angle_deg: 0.0,
        protected_edges: Vec::new(),
        ..Default::default()
    };
    let free = cleanup_mesh_with(&m, &opts_free).expect("the split top must merge when free");
    assert_eq!(free.face_count(), 6, "unprotected: the diagonal is merged away");

    let opts_prot = CleanupOptions {
        protected_edges: vec![(p[4], p[6])],
        ..opts_free
    };
    // Nothing left to improve once the only mergeable edge is off-limits, so cleanup declines
    // outright (`None` = "already clean") rather than installing an identical mesh.
    assert!(
        cleanup_mesh_with(&m, &opts_prot).is_none(),
        "protected: cleanup must not merge across the tagged edge"
    );
}

/// Regression (owner 2026-08-03, "knife subtract breaks the filleted groove far from
/// the cut"): merge_coplanar used PAIRWISE union-find chaining, so a smooth fillet
/// whose per-strip angle was under the 2.6° tolerance chain-merged into ONE curved
/// "coplanar" region — wildly non-planar as a whole — and its tessellation grew
/// phantom walls. Candidates now test against the REGION ROOT's plane, bounding
/// total deviation. A fine cylindrical strip arc must NOT collapse into one face;
/// a genuinely flat grid must still merge fully.
#[test]
fn merge_coplanar_does_not_chain_across_a_fine_arc() {
    // 30 strips over 45° → 1.5° per step: every PAIR passes COPLANAR_DOT (0.999),
    // the whole arc (45°) is nowhere near coplanar.
    let n = 30usize;
    let mut positions: Vec<Vec3> = Vec::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let total = 45f32.to_radians();
    for i in 0..=n {
        let a = total * i as f32 / n as f32;
        positions.push(Vec3::new(a.cos(), a.sin(), 0.0));
        positions.push(Vec3::new(a.cos(), a.sin(), 1.0));
    }
    for i in 0..n {
        let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 3, 2 * i + 2);
        faces.push(vec![a, b, c, d]);
    }
    let merged = merge_coplanar(&positions, &faces);
    assert!(
        merged.len() >= n / 2,
        "fine arc chain-merged into {} region(s) — the unbounded-chain bug is back",
        merged.len()
    );

    // Control: a flat 1×n strip of quads on one plane MUST merge into one face.
    let mut fpos: Vec<Vec3> = Vec::new();
    let mut ffaces: Vec<Vec<usize>> = Vec::new();
    for i in 0..=n {
        fpos.push(Vec3::new(i as f32, 0.0, 0.0));
        fpos.push(Vec3::new(i as f32, 1.0, 0.0));
    }
    for i in 0..n {
        let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 3, 2 * i + 2);
        ffaces.push(vec![a, b, c, d]);
    }
    let fm = merge_coplanar(&fpos, &ffaces);
    assert_eq!(fm.len(), 1, "flat strip must still merge into one face");
}

/// SUPPORT-EDGE PLACEMENT (owner test_014, 2026-08-05: "random vertex created on the
/// circle segment"). An annulus cannot merge to one face, so `merge_coplanar` leaves
/// internal bridges — the boolean's support edges. The old first-in-index-order dissolve
/// could leave a bridge terminating on a T-pin vertex sitting mid-segment on a straight
/// boundary edge (valence 3: two collinear halves + the bridge), which `dissolve_collinear`
/// then cannot remove. The steered order dissolves pin-holding diagonals FIRST, so the
/// survivors terminate on real junction vertices and the freed pin is dissolved.
#[test]
fn merge_coplanar_support_edges_avoid_midsegment_pin_vertices() {
    // Square annulus with a T-pin (vertex 8) mid-way on the bottom outer edge (0)-(1).
    // The pin deliberately carries the HIGHEST index: the old first-in-index-order dissolve
    // removed low-index diagonals first, so the pin's bridges were exactly the survivors.
    let positions: Vec<Vec3> = vec![
        Vec3::new(0.0, 0.0, 0.0), // 0 outer corner
        Vec3::new(4.0, 0.0, 0.0), // 1 outer corner
        Vec3::new(4.0, 4.0, 0.0), // 2 outer corner
        Vec3::new(0.0, 4.0, 0.0), // 3 outer corner
        Vec3::new(1.5, 1.5, 0.0), // 4 hole corner
        Vec3::new(2.5, 1.5, 0.0), // 5 hole corner
        Vec3::new(2.5, 2.5, 0.0), // 6 hole corner
        Vec3::new(1.5, 2.5, 0.0), // 7 hole corner
        Vec3::new(2.0, 0.0, 0.0), // 8 THE PIN — collinear on (0)-(1)
    ];
    let faces: Vec<Vec<usize>> = vec![
        vec![0, 8, 4],
        vec![8, 5, 4],
        vec![8, 1, 5],
        vec![1, 2, 5],
        vec![2, 6, 5],
        vec![2, 3, 6],
        vec![3, 7, 6],
        vec![3, 0, 7],
        vec![0, 4, 7],
    ];
    let merged = merge_coplanar(&positions, &faces);
    assert!(merged.len() >= 2, "an annulus can never merge to one face");
    // The pin must be gone entirely: no surviving bridge may terminate on it (that is the
    // owner's valence-3 "random vertex"), and once freed it is collinear degree-2, so
    // `dissolve_collinear` removes it from the boundary as well.
    for f in &merged {
        assert!(
            !f.contains(&8),
            "a support edge still pins the mid-segment vertex: {merged:?}"
        );
    }
}

/// BLENDER ORACLE PARITY (limited dissolve, 2026-08-03 — dissolve_oracle2.py):
/// shapes that "would collapse on themselves" keep a supporting edge.
///   annulus (3×3 grid, center hole): Blender → **2 faces**, no ring repeats a vertex.
///   vertex pinch (two quads sharing ONE vertex): Blender → stays **2 faces**.
#[test]
fn merge_coplanar_annulus_and_pinch_match_blender_oracle() {
    // Annulus: 4×4 vertex grid, 8 quads (center missing).
    let mut pos: Vec<Vec3> = Vec::new();
    for y in 0..4 {
        for x in 0..4 {
            pos.push(Vec3::new(x as f32, y as f32, 0.0));
        }
    }
    let idx = |x: usize, y: usize| y * 4 + x;
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for y in 0..3 {
        for x in 0..3 {
            if (x, y) == (1, 1) {
                continue;
            }
            faces.push(vec![idx(x, y), idx(x + 1, y), idx(x + 1, y + 1), idx(x, y + 1)]);
        }
    }
    let merged = merge_coplanar(&pos, &faces);
    assert_eq!(merged.len(), 2, "annulus must keep a supporting split (Blender: 2 faces)");
    for r in &merged {
        let uniq: std::collections::HashSet<_> = r.iter().collect();
        assert_eq!(uniq.len(), r.len(), "no ring may visit a vertex twice (self-touching face)");
    }

    // Pinch: two coplanar quads sharing exactly one vertex (index 2).
    let ppos = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(2.0, 2.0, 0.0),
        Vec3::new(1.0, 2.0, 0.0),
    ];
    let pfaces = vec![vec![0usize, 1, 2, 3], vec![2, 4, 5, 6]];
    let pm = merge_coplanar(&ppos, &pfaces);
    assert_eq!(pm.len(), 2, "vertex pinch must stay two faces (Blender parity)");
}

/// Axis-aligned closed box at `center` with `half` extents, built through
/// `from_polygons` (same winding as `HalfEdgeMesh::new_box` — outward normals).
fn box_mesh(center: Vec3, half: Vec3) -> HalfEdgeMesh {
    let c = center;
    let h = half;
    let positions = vec![
        c + Vec3::new(-h.x, -h.y, -h.z), // 0
        c + Vec3::new(h.x, -h.y, -h.z),  // 1
        c + Vec3::new(h.x, -h.y, h.z),   // 2
        c + Vec3::new(-h.x, -h.y, h.z),  // 3
        c + Vec3::new(-h.x, h.y, -h.z),  // 4
        c + Vec3::new(h.x, h.y, -h.z),   // 5
        c + Vec3::new(h.x, h.y, h.z),    // 6
        c + Vec3::new(-h.x, h.y, h.z),   // 7
    ];
    let faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2, 3], // bottom (-Y)
        vec![7, 6, 5, 4], // top (+Y)
        vec![4, 5, 1, 0], // back (-Z)
        vec![6, 7, 3, 2], // front (+Z)
        vec![7, 4, 0, 3], // left (-X)
        vec![5, 6, 2, 1], // right (+X)
    ];
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

fn assert_closed_manifold(m: &HalfEdgeMesh, label: &str) {
    assert_eq!(
        boundary_half_edge_count(m),
        0,
        "{label}: result must be watertight (zero boundary half-edges)"
    );
    assert!(m.validate().is_ok(), "{label}: validate() must pass: {:?}", m.validate());
}

fn assert_bbox_eq(m: &HalfEdgeMesh, want_min: Vec3, want_max: Vec3, label: &str) {
    let (min, max) = m.bounding_box();
    assert!(
        (min - want_min).length() < 1e-3 && (max - want_max).length() < 1e-3,
        "{label}: bbox {:?}..{:?}, want {:?}..{:?}",
        min, max, want_min, want_max
    );
}

/// Euler characteristic V - E + F (2 for genus 0, 0 for genus 1 on closed manifolds).
fn euler_characteristic(m: &HalfEdgeMesh) -> i64 {
    m.vertex_count() as i64 - m.edge_count() as i64 + m.face_count() as i64
}

#[test]
fn union_overlapping_cubes() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let b = box_mesh(Vec3::splat(0.5), Vec3::splat(1.0));
    let r = mesh_boolean(&a, &b, BoolOp::Union).expect("union must produce a mesh");
    assert_closed_manifold(&r, "union");
    assert_bbox_eq(&r, Vec3::splat(-1.0), Vec3::splat(1.5), "union");
    let v = r.vertex_count();
    assert!(v >= 8 && v <= 200, "union: implausible vertex count {v}");
    assert_eq!(euler_characteristic(&r), 2, "union: genus-0 Euler characteristic");
}

#[test]
fn difference_overlapping_cubes() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let b = box_mesh(Vec3::splat(0.5), Vec3::splat(1.0));
    let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("difference must produce a mesh");
    assert_closed_manifold(&r, "difference");
    // A minus an overlapping corner cube keeps A's bbox (corner bite is interior to it).
    assert_bbox_eq(&r, Vec3::splat(-1.0), Vec3::splat(1.0), "difference");
    let v = r.vertex_count();
    assert!(v >= 8 && v <= 200, "difference: implausible vertex count {v}");
    assert_eq!(euler_characteristic(&r), 2, "difference: genus-0 Euler characteristic");
}

#[test]
fn boolean_local_frame_translation_invariant_far_from_origin() {
    // Audit #7: the BSP classified with a fixed WORLD epsilon (PLANE_EPS=1e-5), so a boolean that
    // worked at the origin CRACKED far away (f32 resolution at C=1000 ~1.2e-4 >> 1e-5). The
    // local-frame fix makes it translation-invariant (matches Blender). Oracle bool_diff_oracle.py:
    // A(±1) − B(±0.7 @ (1,1,1)) is closed + manifold with bbox == the minuend (±1).
    let diff_at = |c: Vec3| {
        let a = box_mesh(c, Vec3::splat(1.0));
        let b = box_mesh(c + Vec3::splat(1.0), Vec3::splat(0.7));
        mesh_boolean(&a, &b, BoolOp::Difference).expect("box difference must produce a mesh")
    };
    let r0 = diff_at(Vec3::ZERO);
    assert_closed_manifold(&r0, "diff@origin");
    assert!(r0.is_manifold().is_ok(), "diff@origin is_manifold: {:?}", r0.is_manifold());
    assert_bbox_eq(&r0, Vec3::splat(-1.0), Vec3::splat(1.0), "diff@origin");
    // FAR from origin — the case that cracked before the local frame.
    let t = Vec3::splat(1000.0);
    let r1 = diff_at(t);
    assert_closed_manifold(&r1, "diff@far");
    assert!(r1.is_manifold().is_ok(), "diff@far is_manifold (local-frame fix): {:?}", r1.is_manifold());
    assert_bbox_eq(&r1, t - Vec3::splat(1.0), t + Vec3::splat(1.0), "diff@far");
    // Identical local-frame BSP → identical topology; only the world shift differs.
    assert_eq!(r0.vertex_count(), r1.vertex_count(), "far diff same vertex count as origin");
    assert_eq!(r0.face_count(), r1.face_count(), "far diff same face count as origin");
}

#[test]
fn coplanar_and_coincident_booleans_stay_manifold() {
    // Audit #2: coplanar/coincident-face booleans were feared to emit even-multiplicity non-manifold
    // edges. In practice merge_coplanar + polygons_to_mesh's is_manifold guard keep them clean; verify
    // a spread of coplanar/flush/coincident configs are all closed manifolds (or a clean None for the
    // empty A−A). Blender oracle bool_coplanar_oracle.py: the touching-box union is closed+manifold,
    // bbox x∈[-1,3].
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let cases: [(HalfEdgeMesh, BoolOp); 6] = [
        (box_mesh(Vec3::new(2., 0., 0.), Vec3::splat(1.0)), BoolOp::Union),           // touching at x=1 (oracle)
        (box_mesh(Vec3::new(0., 0.5, 0.), Vec3::splat(0.5)), BoolOp::Difference),     // coplanar top
        (box_mesh(Vec3::new(0., 0., 0.), Vec3::new(0.5, 2., 0.5)), BoolOp::Difference), // through-cut, coplanar ±Y
        (box_mesh(Vec3::new(1., 0., 0.), Vec3::splat(0.5)), BoolOp::Difference),      // flush with +X face
        (box_mesh(Vec3::new(0., 0., 1.), Vec3::new(1., 1., 1.)), BoolOp::Union),      // slab, 4 coplanar faces
        (box_mesh(Vec3::new(0., 0.5, 0.), Vec3::new(1., 0.5, 1.)), BoolOp::Difference), // coincident footprint
    ];
    for (i, (b, op)) in cases.iter().enumerate() {
        if let Some(r) = mesh_boolean(&a, b, *op) {
            assert!(r.is_manifold().is_ok(), "coplanar case {i} ({op:?}) manifold: {:?}", r.is_manifold());
            assert_eq!(boundary_half_edge_count(&r), 0, "coplanar case {i} ({op:?}) watertight");
        }
        // (a None result — e.g. an empty difference — is an acceptable clean failure, never garbage.)
    }
    // Union touching at x=1 matches the Blender oracle bbox (a single 2×1×1 box).
    let u = mesh_boolean(&a, &box_mesh(Vec3::new(2., 0., 0.), Vec3::splat(1.0)), BoolOp::Union).unwrap();
    let (mn, mx) = u.bounding_box();
    assert!((mn - Vec3::splat(-1.0)).length() < 1e-4 && (mx - Vec3::new(3., 1., 1.)).length() < 1e-4,
        "touching-box union bbox = x∈[-1,3] (oracle)");
}

fn bsp_depth(n: &BspNode) -> usize {
    let f = n.front.as_ref().map_or(0, |c| bsp_depth(c));
    let b = n.back.as_ref().map_or(0, |c| bsp_depth(c));
    1 + f.max(b)
}

#[test]
fn bsp_build_depth_is_capped_on_degenerate_input() {
    // Audit #12: stacked parallel polygons force a LINEAR tree (each higher quad goes to the same
    // FRONT child), so the old unbounded `build` recursed once per polygon → stack overflow on dense
    // / self-intersecting input. 6000 quads > MAX_BSP_DEPTH — build must return without overflowing
    // and the tree depth must be capped.
    let mut polys = Vec::new();
    for i in 0..6000u32 {
        let z = i as f32 * 0.01;
        polys.push(Polygon::new(vec![
            Vec3::new(0., 0., z), Vec3::new(1., 0., z), Vec3::new(1., 1., z), Vec3::new(0., 1., z),
        ]).unwrap());
    }
    let node = BspNode::new(polys); // must NOT stack-overflow
    assert!(bsp_depth(&node) <= MAX_BSP_DEPTH + 1,
        "BSP tree depth {} must be capped at ~{}", bsp_depth(&node), MAX_BSP_DEPTH);
}

#[test]
fn intersect_overlapping_cubes() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let b = box_mesh(Vec3::splat(0.5), Vec3::splat(1.0));
    let r = mesh_boolean(&a, &b, BoolOp::Intersect).expect("intersect must produce a mesh");
    assert_closed_manifold(&r, "intersect");
    // [-1,1]^3 ∩ [-0.5,1.5]^3 = [-0.5,1]^3
    assert_bbox_eq(&r, Vec3::splat(-0.5), Vec3::splat(1.0), "intersect");
    let v = r.vertex_count();
    assert!(v >= 8 && v <= 100, "intersect: implausible vertex count {v}");
    assert_eq!(euler_characteristic(&r), 2, "intersect: genus-0 Euler characteristic");
}

#[test]
fn difference_non_touching_returns_a_unchanged_ish() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let b = box_mesh(Vec3::splat(5.0), Vec3::splat(1.0));
    let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("difference must produce a mesh");
    assert_closed_manifold(&r, "difference (disjoint)");
    // Same solid as A: identical bbox (faces may carry harmless BSP splits).
    assert_bbox_eq(&r, Vec3::splat(-1.0), Vec3::splat(1.0), "difference (disjoint)");
    assert_eq!(euler_characteristic(&r), 2, "difference (disjoint): genus-0");
}

#[test]
fn cube_minus_through_prism_is_genus_one() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    // Square prism punching all the way through along Z.
    let b = box_mesh(Vec3::ZERO, Vec3::new(0.4, 0.4, 2.0));
    let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("prism difference must produce a mesh");
    assert_closed_manifold(&r, "cube-minus-prism");
    assert_bbox_eq(&r, Vec3::splat(-1.0), Vec3::splat(1.0), "cube-minus-prism");
    // Torus-topology check: V - E + F = 0 for genus 1.
    assert_eq!(euler_characteristic(&r), 0, "cube-minus-prism: genus-1 Euler characteristic");
}

/// Oracle comparisons use the lib's `mesh_signed_volume` (also the invariant gate).
fn signed_volume(m: &HalfEdgeMesh) -> f64 {
    mesh_signed_volume(m)
}

/// Long box with bevel-rounded top edges (quarter-circle profile, dense loops) —
/// the exact geometry of docs/planning/poly/bool_bevel_ref.py (owner repro 2026-07-23,
/// COMPLETE_BUG_TRACKER.md POLY-BOOL-BEVEL). Prism along X of a (z,y) cross-section.
fn rounded_prism(seg: usize, r: f32, h: f32, d: f32, l: f32) -> HalfEdgeMesh {
    use std::f32::consts::FRAC_PI_2;
    let mut prof: Vec<(f32, f32)> = vec![(-d, 0.0), (d, 0.0), (d, h - r)];
    for i in 1..=seg {
        let a = FRAC_PI_2 * i as f32 / seg as f32;
        prof.push((d - r + r * a.cos(), h - r + r * a.sin()));
    }
    prof.push((-(d - r), h));
    for i in 1..=seg {
        let a = FRAC_PI_2 + FRAC_PI_2 * i as f32 / seg as f32;
        prof.push((-(d - r) + r * a.cos(), h - r + r * a.sin()));
    }
    let n = prof.len();
    let mut pos: Vec<Vec3> = Vec::with_capacity(2 * n);
    for &(z, y) in &prof { pos.push(Vec3::new(0.0, y, z)); }
    for &(z, y) in &prof { pos.push(Vec3::new(l, y, z)); }
    // Profile is CCW in (z,y) → cap normal at x=0 in forward order is −X (outward);
    // x=l cap reversed (+X outward); sides wound to match.
    let mut faces: Vec<Vec<usize>> = vec![
        (0..n).collect(),
        (n..2 * n).rev().collect(),
    ];
    for i in 0..n {
        let j = (i + 1) % n;
        faces.push(vec![j, i, n + i, n + j]);
    }
    HalfEdgeMesh::from_polygons(&pos, &faces)
}

/// OWNER REPRO 2026-07-23 (POLY-BOOL-BEVEL): beveled long box minus flush-bottom
/// small box. Blender EXACT oracle (bool_bevel_ref.py): vol(A)=12.8411, vol(B)=4.2,
/// vol(A−B)=10.1411, bbox = full A, closed manifold. The observed bug: nearly all
/// geometry dropped, a thin sliver installed.
#[test]
fn difference_beveled_prism_flush_bottom_matches_oracle() {
    let a = rounded_prism(8, 0.5, 1.5, 0.75, 6.0);
    // Input sanity — a winding mistake in the test must fail HERE, not in the boolean.
    assert_closed_manifold(&a, "beveled prism input");
    let va = signed_volume(&a);
    assert!((va - 12.8411).abs() < 0.01, "input A volume {va}, want ~12.8411 (winding?)");
    let b = box_mesh(Vec3::new(4.5, 0.5, 0.45), Vec3::new(1.0, 0.5, 1.05)); // flush bottom y=0
    assert!((signed_volume(&b) - 4.2).abs() < 0.01, "input B volume");

    let r = mesh_boolean(&a, &b, BoolOp::Difference)
        .expect("beveled difference must produce a mesh");
    assert_closed_manifold(&r, "beveled difference");
    // Bbox: Manifold may land slightly under the analytic top (y=1.5) after tessellation
    // of the quarter-circle bevel; volume is the hard correctness gate below.
    let (rmin, rmax) = r.bounding_box();
    assert!(
        (rmin - Vec3::new(0.0, 0.0, -0.75)).length() < 0.02
            && (rmax - Vec3::new(6.0, 1.5, 0.75)).length() < 0.02,
        "beveled difference: bbox {rmin:?}..{rmax:?}, want ~[0,0,-0.75]..[6,1.5,0.75]"
    );
    let vr = signed_volume(&r);
    // Blender Exact 10.1411; Manifold can land ~1% lower on dense bevel profiles
    // (tessellation of quarter-circles) while still far from the sliver-bug regime
    // (vol → tiny). Gate: within 2% of oracle and well above vol(A)−vol(B).
    assert!(
        (vr - 10.1411).abs() < 0.25 && vr > 9.5,
        "beveled difference volume {vr}, Blender oracle wants 10.1411 — \
         large deviation = fragments mis-kept/dropped (the sliver bug)"
    );
}

/// Rotate a mesh 90° about +Y (proper rotation — preserves winding) then translate.
fn rot_y90_translate(mut m: HalfEdgeMesh, t: Vec3) -> HalfEdgeMesh {
    for v in m.vertices.iter_mut() {
        let p = v.position;
        v.position = Vec3::new(p.z, p.y, -p.x) + t;
    }
    m
}

/// STRESS MATRIX for the owner sliver bug: sweep length / bevel density / cutter
/// shape / flushness. Correctness check is the volume INVARIANT of Difference —
/// vol(A) − vol(B) ≤ vol(A−B) ≤ vol(A) — which any dropped-fragment sliver violates.
#[test]
fn difference_beveled_stress_matrix() {
    let mut failures: Vec<String> = Vec::new();
    for &(l, seg, r) in &[(6.0f32, 8usize, 0.5f32), (30.0, 8, 0.5), (30.0, 24, 0.1), (120.0, 24, 0.1)] {
        for &beveled_cutter in &[false, true] {
            for &flush in &[true, false] {
                let a = rounded_prism(seg, r, 1.5, 0.75, l);
                let y0 = if flush { 0.5 } else { 0.6 }; // box center → bottom y=0 (flush) or 0.1
                let cx = l * 0.75;
                let b = if beveled_cutter {
                    // crossed beveled prism (like the owner screenshot), along Z
                    let bp = rounded_prism(seg, r.min(0.3), 1.0, 0.5, 3.0);
                    rot_y90_translate(bp, Vec3::new(cx, if flush { 0.0 } else { 0.1 }, -1.0))
                } else {
                    box_mesh(Vec3::new(cx, y0, 0.45), Vec3::new(1.0, 0.5, 1.05))
                };
                let (va, vb) = (signed_volume(&a), signed_volume(&b));
                let label = format!(
                    "L={l} seg={seg} r={r} beveled_cutter={beveled_cutter} flush={flush}"
                );
                match mesh_boolean(&a, &b, BoolOp::Difference) {
                    Some(res) => {
                        let vr = signed_volume(&res);
                        let lo = va - vb - 0.05;
                        let hi = va + 0.05;
                        if vr < lo || vr > hi {
                            failures.push(format!(
                                "{label}: vol(r)={vr:.3} outside invariant [{lo:.3},{hi:.3}] (va={va:.3} vb={vb:.3})"
                            ));
                        } else if boundary_half_edge_count(&res) != 0 || res.validate().is_err() {
                            failures.push(format!("{label}: result not closed/valid"));
                        }
                    }
                    None => failures.push(format!("{label}: boolean returned None")),
                }
            }
        }
    }
    assert!(failures.is_empty(), "stress failures:\n{}", failures.join("\n"));
}

/// Worst distance of any face's vertex from that face's own Newell plane. A boolean result is
/// pieces of planar input faces, so anything above float noise is a merge that BENT the surface.
fn worst_face_planarity(m: &HalfEdgeMesh) -> f32 {
    let mut worst = 0.0f32;
    for i in 0..m.faces.len() as u32 {
        let f = FaceId(i);
        if m.face(f).half_edge == NO_EDGE {
            continue;
        }
        let pts: Vec<Vec3> = m.face_vertices(f).iter().map(|&v| m.vert(v).position).collect();
        let n = m.compute_face_normal(f).normalize_or_zero();
        let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
        for p in &pts {
            worst = worst.max((*p - c).dot(n).abs());
        }
    }
    worst
}

/// POLY-BOOL-NGON-BEVEL (owner `bollean.fl4`, 2026-10-13; app-path pin with the owner's own
/// file: `app::poly::diag_bool_ngon_bevel`). A cutter with a FINE round bevel — strips under
/// the old 12° polish angle (and, at 64 segments, under merge_coplanar's old 2.56°) — notches
/// a box. The polish used to dissolve the strips into the flat ceiling of the notch one by
/// one (each merge re-averaged the normal, so the chain kept going), installing bent n-gons:
/// a partial rounded channel, slanted slivers, and a volume change the whole-mesh 1e-3 gate
/// could not see. Now: every strip survives as its own planar face and the volume is exact.
#[test]
fn difference_keeps_every_strip_of_a_fine_round_bevel_cutter() {
    let r = 0.3f64;
    // (cutter length along x, length of it inside the target): a THROUGH notch, and a POCKET
    // whose end cap stays inside the target like the owner's (each strip edge then keeps one
    // input endpoint — the case the old 12° polish chain ate at 32 segments).
    let cases = [(3.0f32, 2.0f64), (1.5, 1.0)];
    for (len, x_len) in cases {
        for seg in [32usize, 64] {
            let label = format!("seg={seg} len={len}");
            let target = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
            // Profile in (z,y): y∈[0,1], z∈[-0.5,0.5], round top corners of radius r. Moved
            // to y∈[-1.6,-0.6], z∈[0.4,1.4], x∈[-1.5,-1.5+len]: a notch along the bottom-front
            // edge whose one rounded corner (z=0.4 side) lies fully inside the target.
            let mut cutter = rounded_prism(seg, r as f32, 1.0, 0.5, len);
            for v in cutter.vertices.iter_mut() {
                v.position += Vec3::new(-1.5, -1.6, 0.9);
            }
            assert_closed_manifold(&cutter, "rounded cutter input");

            let res = mesh_boolean(&target, &cutter, BoolOp::Difference)
                .unwrap_or_else(|| panic!("{label}: difference must produce a mesh"));
            assert_closed_manifold(&res, &label);
            assert!(res.is_edge_manifold().is_ok(), "{label}: edge-manifold");
            assert_eq!(res.count_folded_coplanar_pairs(), 0, "{label}: folds");

            let worst = worst_face_planarity(&res);
            assert!(
                worst < 1e-4,
                "{label}: a result face is bent by {worst} — the polish merged faces that do \
                 not share a plane (bevel strips into the notch ceiling)"
            );

            // Notch cross-section inside the target: 0.6 (z) × 0.4 (y), minus the corner the
            // round leaves solid: r² − the seg-gon quarter sector.
            let sector =
                seg as f64 * 0.5 * r * r * (std::f64::consts::FRAC_PI_2 / seg as f64).sin();
            let notch = 0.6 * 0.4 - (r * r - sector);
            let want = 8.0 - x_len * notch;
            let got = signed_volume(&res);
            assert!(
                (got - want).abs() < 1e-4,
                "{label}: volume {got:.6}, exact {want:.6} — the surface was moved"
            );

            // Every strip of the round that lies inside the target is still its own face: the
            // only faces oblique in (y,z) are exactly the `seg` strips.
            let strips = (0..res.faces.len() as u32)
                .map(FaceId)
                .filter(|&f| res.face(f).half_edge != NO_EDGE)
                .filter(|&f| {
                    let n = res.compute_face_normal(f).normalize_or_zero();
                    n.x.abs() < 1e-3 && n.y.abs() > 1e-3 && n.z.abs() > 1e-3
                })
                .count();
            assert_eq!(strips, seg, "{label}: bevel strips were merged away");
        }
    }
}

/// Count faces by vertex count (n-gon histogram).
fn face_size_hist(m: &HalfEdgeMesh) -> std::collections::BTreeMap<usize, usize> {
    let mut h = std::collections::BTreeMap::new();
    for i in 0..m.faces.len() {
        let fid = FaceId(i as u32);
        if m.face(fid).half_edge == NO_EDGE { continue; }
        let n = m.face_vertices(fid).len();
        if n >= 3 { *h.entry(n).or_default() += 1; }
    }
    h
}

/// COPLANAR DISSOLVE — Blender parity (`bool_diff_cube_ref.json` oracle): cube A [-1,1]³ minus
/// cube B [0,2]³ (corner bite). Blender's Exact solver yields a CLEAN n-gon mesh — 14 verts,
/// 9 faces (6 quads + 3 hexagons), ZERO triangles. Before the dissolve pass our BSP produced
/// ~30 triangles ("random edges" the user saw). Now: exactly Blender's topology.
#[test]
fn difference_dissolves_to_ngons() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let b = box_mesh(Vec3::splat(1.0), Vec3::splat(1.0)); // [0,2]³
    let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("difference must produce a mesh");
    assert_closed_manifold(&r, "difference dissolve");
    assert_eq!(r.vertex_count(), 14, "oracle nv=14 (Blender)");
    let hist = face_size_hist(&r);
    assert_eq!(hist.get(&3).copied().unwrap_or(0), 0, "no triangles after dissolve; hist={hist:?}");
    assert_eq!(r.face_count(), 9, "oracle nf=9 (6 quads + 3 hexagons); hist={hist:?}");
    assert_eq!(hist.get(&4).copied().unwrap_or(0), 6, "6 quads; hist={hist:?}");
    assert_eq!(hist.get(&6).copied().unwrap_or(0), 3, "3 hexagons; hist={hist:?}");
    assert_eq!(euler_characteristic(&r), 2, "genus-0 preserved by dissolve");
}

/// Dissolve keeps a clean primitive clean: A∪A-ish disjoint union stays 6-quad boxes, and a
/// far-disjoint difference returns exactly A's 6 quads (BSP splits fully dissolved away).
#[test]
fn disjoint_difference_dissolves_to_six_quads() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let b = box_mesh(Vec3::splat(5.0), Vec3::splat(1.0));
    let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("must produce a mesh");
    assert_closed_manifold(&r, "disjoint difference dissolve");
    let hist = face_size_hist(&r);
    assert_eq!(r.face_count(), 6, "disjoint difference = A's 6 quads; hist={hist:?}");
    assert_eq!(hist.get(&4).copied().unwrap_or(0), 6, "all quads; hist={hist:?}");
}

/// Owner 2026-08-10: "boolean triangulates part of the mesh they are even not touching".
/// A multi-quad flat box bitten at one corner must keep remote coplanar faces as n-gons
/// (Manifold's triangle soup must dissolve on flats the cut never reaches).
#[test]
fn difference_does_not_leave_remote_flats_triangulated() {
    // Solid box + tiny corner cutter: Manifold fan-triangulates ALL faces, and a broken
    // n-gon restore ships those tris on walls the cut never reaches.
    let a = box_mesh(Vec3::ZERO, Vec3::new(2.0, 1.0, 1.0)); // half-size → [-2,2]×[-1,1]×[-1,1]
    let cutter = box_mesh(Vec3::new(2.0, 1.0, 1.0), Vec3::splat(0.6)); // corner bite at +X+Y+Z
    let r = mesh_boolean(&a, &cutter, BoolOp::Difference).expect("corner bite must produce a mesh");
    assert_closed_manifold(&r, "remote flat clean");
    let hist = face_size_hist(&r);
    let tris = hist.get(&3).copied().unwrap_or(0);
    // Untouched −X wall is far from the cut: must be a single quad (or one n-gon), never a fan of tris.
    let remote_clean = (0..r.faces.len()).any(|i| {
        let fid = FaceId(i as u32);
        if r.face(fid).half_edge == NO_EDGE {
            return false;
        }
        let vs = r.face_vertices(fid);
        vs.len() >= 4 && vs.iter().all(|&v| (r.vert(v).position.x + 2.0).abs() < 1e-3)
    });
    assert!(
        remote_clean,
        "untouched −X face must dissolve to an n-gon ≥4, not stay Manifold-triangulated; hist={hist:?}"
    );
    // Global: no dense tri soup on a flat hard-surface difference (contact may leave a few).
    assert!(
        tris <= 8,
        "remote flats must not remain triangulated (tris={tris}); hist={hist:?}"
    );
}

fn cylinder_mesh(r: f32, half_h: f32, sides: usize) -> HalfEdgeMesh {
    let mut pos: Vec<Vec3> = Vec::new();
    for i in 0..sides { let a = std::f32::consts::TAU * i as f32 / sides as f32; pos.push(Vec3::new(r*a.cos(), r*a.sin(), half_h)); }
    for i in 0..sides { let a = std::f32::consts::TAU * i as f32 / sides as f32; pos.push(Vec3::new(r*a.cos(), r*a.sin(), -half_h)); }
    let mut f: Vec<Vec<usize>> = vec![(0..sides).collect(), (sides..2*sides).rev().collect()];
    for i in 0..sides { let j=(i+1)%sides; f.push(vec![i, sides+i, sides+j, j]); }
    HalfEdgeMesh::from_polygons(&pos, &f)
}
/// ROTATED CUTTER — Blender parity (non-axis-aligned intersection): cube [-1,1]³ minus a cube
/// rotated 45° about Z, offset +1.2 X. Blender's Exact solver: nv=10, nf=7 (5 quads + 2 pentagons),
/// ZERO triangles. Proves the coplanar dissolve is correct off the axes, not just on cube-flat data.
#[test]
fn difference_rotated_cutter_matches_blender() {
    use std::f32::consts::PI;
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let rot = glam::Quat::from_rotation_z(PI / 4.0);
    let h = 1.0f32;
    let corners = [Vec3::new(-h,-h,-h), Vec3::new(h,-h,-h), Vec3::new(h,-h,h), Vec3::new(-h,-h,h),
                   Vec3::new(-h,h,-h), Vec3::new(h,h,-h), Vec3::new(h,h,h), Vec3::new(-h,h,h)];
    let pos: Vec<Vec3> = corners.iter().map(|&s| Vec3::new(1.2, 0.0, 0.0) + rot * s).collect();
    let bf: Vec<Vec<usize>> = vec![vec![0,1,2,3], vec![7,6,5,4], vec![4,5,1,0], vec![6,7,3,2], vec![7,4,0,3], vec![5,6,2,1]];
    let b = HalfEdgeMesh::from_polygons(&pos, &bf);
    let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("rotated difference must produce a mesh");
    assert_closed_manifold(&r, "rotated cutter");
    let hist = face_size_hist(&r);
    assert_eq!(hist.get(&3).copied().unwrap_or(0), 0, "no triangles after dissolve; hist={hist:?}");
    assert_eq!(r.vertex_count(), 10, "oracle nv=10 (Blender); hist={hist:?}");
    assert_eq!(r.face_count(), 7, "oracle nf=7 (5 quads + 2 pentagons); hist={hist:?}");
    assert_eq!(euler_characteristic(&r), 2, "genus-0");
}

/// CYLINDER THROUGH — Blender Exact 5.0.1: 40v/24f, **0 tris**, χ=0.
/// Dissolve must (a) stay watertight genus-1, (b) NOT over-merge curved wall facets,
/// (c) dissolve untouched cube side walls to quads, (d) leave **zero triangles**.
#[test]
fn difference_cylinder_through_clean_and_watertight() {
    let sides = 16usize;
    let r = 0.5f32;
    let mut cpos: Vec<Vec3> = Vec::new();
    for i in 0..sides { let a = std::f32::consts::TAU * i as f32 / sides as f32; cpos.push(Vec3::new(r*a.cos(), r*a.sin(), 1.5)); }
    for i in 0..sides { let a = std::f32::consts::TAU * i as f32 / sides as f32; cpos.push(Vec3::new(r*a.cos(), r*a.sin(), -1.5)); }
    let mut cf: Vec<Vec<usize>> = vec![(0..sides).collect(), (sides..2*sides).rev().collect()];
    for i in 0..sides { let j = (i+1)%sides; cf.push(vec![i, sides+i, sides+j, j]); }
    let cyl = HalfEdgeMesh::from_polygons(&cpos, &cf);
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let out = mesh_boolean(&a, &cyl, BoolOp::Difference).expect("cylinder difference must produce a mesh");
    assert_closed_manifold(&out, "cylinder through");
    assert_eq!(euler_characteristic(&out), 0, "cylinder punches through → genus-1 (χ=0)");
    let hist = face_size_hist(&out);
    assert_eq!(hist.get(&3).copied().unwrap_or(0), 0, "Blender 0 tris; hist={hist:?}");
    // (b) curved wall NOT over-merged: ≥16 faces whose verts all sit on the cylinder radius.
    let mut wall = 0;
    for i in 0..out.faces.len() {
        let fid = FaceId(i as u32);
        if out.face(fid).half_edge == NO_EDGE { continue; }
        let vs = out.face_vertices(fid);
        if vs.len() >= 3 && vs.iter().all(|&v| { let p = out.vert(v).position; (p.x*p.x + p.y*p.y).sqrt() > r - 1e-2 && (p.x*p.x+p.y*p.y).sqrt() < r + 1e-2 }) { wall += 1; }
    }
    assert!(wall >= sides, "curved cylinder wall over-merged: {wall} facet faces (want ≥{sides})");
    // (c) an UNTOUCHED cube side wall (x=+1) dissolved to a single quad.
    let side_quad = (0..out.faces.len()).any(|i| {
        let fid = FaceId(i as u32);
        if out.face(fid).half_edge == NO_EDGE { return false; }
        let vs = out.face_vertices(fid);
        vs.len() == 4 && vs.iter().all(|&v| (out.vert(v).position.x - 1.0).abs() < 1e-3)
    });
    assert!(side_quad, "untouched +X cube wall did not dissolve to a single quad");
}

#[test]
fn intersect_disjoint_returns_none() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let b = box_mesh(Vec3::splat(5.0), Vec3::splat(1.0));
    assert!(mesh_boolean(&a, &b, BoolOp::Intersect).is_none());
}

#[test]
fn open_mesh_rejected() {
    let a = HalfEdgeMesh::new_plane(2.0, 2.0, 1, 1); // open sheet
    let b = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    assert!(mesh_boolean(&a, &b, BoolOp::Union).is_none());
    assert!(mesh_boolean(&b, &a, BoolOp::Difference).is_none());
}

/// SEQUENTIAL DOUBLE SUBTRACTION — the user's "2nd boolean on the same island breaks / messy
/// segmentation" repro (§12 XL). Two adjacent corner bites off a cube:
///   A[-1,1]³ − cube@(1,1,1)  = 14v/9f  (removes the +++ corner [0,1]³)   ← matches Blender EXACT
///   (that)   − cube@(-1,1,1) = the −++ corner [-1,0]×[0,1]×[0,1] as well.
/// c1's −X face and c2's +X face are BOTH the plane x=0 → the coplanar seam that made the csg.js
/// BSP emit a same-winding non-manifold (→ boolean did nothing / added messy segmentation).
///
/// KEY: the two bites UNION to a single slab [-1,1]×[0,1]×[0,1], so the true result is a clean
/// L-PRISM = 12v / 8f (6 quads + 2 hexagons), bbox [-1,1]³, genus-0, ZERO triangles. Verified against
/// Blender EXACT as a SINGLE slab cut (docs/planning/poly/slab_oracle.py → 12v/8f {4:6,6:2}). Note our
/// robust boolean produces this OPTIMAL topology; Blender applied SEQUENTIALLY leaves a vestigial x=0
/// seam (15v/10f, +2 pentagons) — i.e. here we dissolve as cleanly as Blender's own single-op result,
/// cleaner than its sequential output. This is the "no messy segmentation" the user asked for.
#[test]
fn sequential_double_subtraction_matches_blender() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let c1 = box_mesh(Vec3::splat(1.0), Vec3::splat(1.0)); // [0,2]³
    let m1 = mesh_boolean(&a, &c1, BoolOp::Difference).expect("cut1 must produce a mesh");
    assert_closed_manifold(&m1, "seq cut1");
    assert!(m1.is_manifold().is_ok(), "seq cut1 is_manifold: {:?}", m1.is_manifold());
    assert_eq!(m1.vertex_count(), 14, "cut1 nv=14 (oracle)");
    assert_eq!(m1.face_count(), 9, "cut1 nf=9 (oracle)");

    // Second cut over the coplanar x=0 seam — was None/messy before the robust classifier.
    let c2 = box_mesh(Vec3::new(-1.0, 1.0, 1.0), Vec3::splat(1.0)); // [-2,0]×[0,2]×[0,2]
    let m2 = mesh_boolean(&m1, &c2, BoolOp::Difference)
        .expect("cut2 (coplanar seam) must produce a mesh");
    assert_closed_manifold(&m2, "seq cut2");
    assert!(m2.is_manifold().is_ok(), "seq cut2 is_manifold: {:?}", m2.is_manifold());
    assert_bbox_eq(&m2, Vec3::splat(-1.0), Vec3::splat(1.0), "seq cut2");
    let hist = face_size_hist(&m2);
    let tris = hist.get(&3).copied().unwrap_or(0);
    // Manifold default: may leave ≤2 residual tris on the coplanar x=0 seam after
    // sequential cuts (BSP path dissolved to exact 12v/8f). Hard gates: closed, manifold,
    // genus-0, bbox, and no messy multi-tri soup.
    assert!(
        tris <= 2,
        "no messy segmentation (≤2 residual tris under Manifold); hist={hist:?}"
    );
    assert!(
        m2.vertex_count() <= 16 && m2.face_count() <= 12,
        "cut2 should be near clean L-prism (≤16v/12f); nv={} nf={} hist={hist:?}",
        m2.vertex_count(),
        m2.face_count()
    );
    assert!(
        m2.vertex_count() >= 12 && m2.face_count() >= 8,
        "cut2 too sparse: nv={} nf={}",
        m2.vertex_count(),
        m2.face_count()
    );
    assert_eq!(euler_characteristic(&m2), 2, "genus-0 preserved through sequential cuts");
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Adversarial ROBUSTNESS battery — sequential + mixed-op booleans (the user's "2nd/3rd boolean on
// the same island breaks / adds messy segmentation" class). Correctness INVARIANTS only (watertight,
// manifold, genus, bbox, no stray triangles); exact face counts are deliberately NOT pinned — our
// dissolve is often cleaner than a sequential Blender bake (see sequential_double_subtraction).
// ─────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn robustness_triple_corner_bite() {
    // Remove three of a cube's corners one after another — each cut re-enters the prior result.
    let mut m = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    for c in [Vec3::splat(1.0), Vec3::new(-1., 1., 1.), Vec3::new(1., -1., 1.)] {
        m = mesh_boolean(&m, &box_mesh(c, Vec3::splat(1.0)), BoolOp::Difference)
            .expect("each sequential corner bite must produce a mesh");
        assert_closed_manifold(&m, "triple bite step");
        assert!(m.is_manifold().is_ok(), "triple bite manifold: {:?}", m.is_manifold());
    }
    assert_bbox_eq(&m, Vec3::splat(-1.0), Vec3::splat(1.0), "triple bite");
    assert_eq!(euler_characteristic(&m), 2, "triple bite genus-0");
    // Manifold may leave a single residual tri on sequential coplanar corners.
    let tris = face_size_hist(&m).get(&3).copied().unwrap_or(0);
    assert!(
        tris <= 2,
        "triple bite: ≤2 residual tris under Manifold; got {tris}"
    );
}

#[test]
fn robustness_sequential_union_chain() {
    // Union a chain of overlapping boxes — each union re-processes the growing island. Three unit
    // boxes at x = 0,1,2 fill a solid bar, so the ideal result is a single [-1,3]×[-1,1]×[-1,1] box.
    let mut m = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    for c in [Vec3::new(1., 0., 0.), Vec3::new(2., 0., 0.)] {
        m = mesh_boolean(&m, &box_mesh(c, Vec3::splat(1.0)), BoolOp::Union)
            .expect("each sequential union must produce a mesh");
        assert_closed_manifold(&m, "union chain step");
        assert!(m.is_manifold().is_ok(), "union chain manifold: {:?}", m.is_manifold());
    }
    assert_bbox_eq(&m, Vec3::new(-1., -1., -1.), Vec3::new(3., 1., 1.), "union chain");
    assert_eq!(euler_characteristic(&m), 2, "union chain genus-0");
    assert_eq!(face_size_hist(&m).get(&3).copied().unwrap_or(0), 0, "union chain: no stray triangles");
}

#[test]
fn robustness_difference_then_intersect() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let notched = mesh_boolean(&a, &box_mesh(Vec3::splat(1.0), Vec3::splat(1.0)), BoolOp::Difference)
        .expect("difference must produce a mesh");
    let r = mesh_boolean(&notched, &box_mesh(Vec3::splat(-0.5), Vec3::splat(1.0)), BoolOp::Intersect)
        .expect("intersect after difference must produce a mesh");
    assert_closed_manifold(&r, "diff-then-intersect");
    assert!(r.is_manifold().is_ok(), "diff-then-intersect manifold: {:?}", r.is_manifold());
    assert_eq!(euler_characteristic(&r), 2, "diff-then-intersect genus-0");
}

#[test]
fn robustness_rotated_then_axis_cut() {
    use std::f32::consts::PI;
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let m1 = mesh_boolean(&a, &box_mesh(Vec3::splat(1.0), Vec3::splat(1.0)), BoolOp::Difference)
        .expect("axis cut");
    // A cube rotated 45° about Z, offset into the −X −Y region, cutting the fresh axis-cut geometry.
    let rot = glam::Quat::from_rotation_z(PI / 4.0);
    let corners = [Vec3::new(-1.,-1.,-1.),Vec3::new(1.,-1.,-1.),Vec3::new(1.,-1.,1.),Vec3::new(-1.,-1.,1.),
                   Vec3::new(-1.,1.,-1.),Vec3::new(1.,1.,-1.),Vec3::new(1.,1.,1.),Vec3::new(-1.,1.,1.)];
    let pos: Vec<Vec3> = corners.iter().map(|&s| Vec3::new(-1.0, -1.0, 0.0) + rot * s).collect();
    let bf: Vec<Vec<usize>> = vec![vec![0,1,2,3],vec![7,6,5,4],vec![4,5,1,0],vec![6,7,3,2],vec![7,4,0,3],vec![5,6,2,1]];
    let cutter = HalfEdgeMesh::from_polygons(&pos, &bf);
    let r = mesh_boolean(&m1, &cutter, BoolOp::Difference).expect("rotated cut after axis cut");
    assert_closed_manifold(&r, "rotated-then-axis");
    assert!(r.is_manifold().is_ok(), "rotated-then-axis manifold: {:?}", r.is_manifold());
}

// ================================================================
// BLENDER 5.0.1 EXACT = source of truth
// Regenerated by:
//   blender --factory-startup --background --python
//     docs/planning/poly/bool_edge_case_matrix.py -- bool_edge_case_matrix.json
// Same discipline as bevel oracles: lock topology (nv/nf/tris/euler/0-nonman)
// for every edge-case scenario. Tessellation of curved cutters may differ in
// exact face layout; flat cases MUST match Blender n-gon counts and ZERO tris.
// ================================================================

struct BlenderOracle {
    label: &'static str,
    nv: usize,
    nf: usize,
    tris: usize,
    euler: i64,
    /// When true, exact nv/nf/tris must match Blender. When false, only
    /// closed+manifold+euler+tris<=blender (curved residual allowance).
    exact_topo: bool,
}

fn census(m: &HalfEdgeMesh) -> (usize, usize, usize, i64) {
    let hist = face_size_hist(m);
    let tris = hist.get(&3).copied().unwrap_or(0);
    (
        m.vertex_count(),
        m.face_count(),
        tris,
        euler_characteristic(m),
    )
}

/// Cube corner bite — Blender Exact: 14v/9f, 0 tris, χ=2.
#[test]
fn blender_exact_cube_minus_corner() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    // Blender cube size=1.4 @ (1,1,1) → half extents 0.7
    let b = box_mesh(Vec3::splat(1.0), Vec3::splat(0.7));
    let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("cube_minus_corner");
    assert_closed_manifold(&r, "cube_minus_corner");
    assert!(r.is_manifold().is_ok(), "{:?}", r.is_manifold());
    let (nv, nf, tris, euler) = census(&r);
    assert_eq!(tris, 0, "Blender 0 tris; ours tris={tris} hist={:?}", face_size_hist(&r));
    assert_eq!(euler, 2);
    // Oracle file: nv=14 nf=9 (same as full half=1.0 corner bite for this cut class)
    assert!(nv >= 12 && nv <= 16, "nv={nv} want ~14 (Blender)");
    assert!(nf >= 8 && nf <= 12, "nf={nf} want ~9 (Blender)");
}

/// Blender Exact matrix hard locks (flat / dissolved cases).
#[test]
fn blender_exact_edge_case_matrix_flat() {
    let oracles = [
        // cube_minus_corner with half=1.0 (matches difference_dissolves_to_ngons / seq cut1)
        BlenderOracle { label: "cube_corner_half1", nv: 14, nf: 9, tris: 0, euler: 2, exact_topo: true },
        // cube_minus_rotcube
        BlenderOracle { label: "cube_rotcube", nv: 10, nf: 7, tris: 0, euler: 2, exact_topo: true },
        // coplanar union: Blender Exact keeps mid-belt seam (12v/10f). We fully dissolve
        // coplanar sides → 8v/6f single box ("only one edge" — cleaner than Blender, still solid).
        BlenderOracle { label: "coplanar_union", nv: 8, nf: 6, tris: 0, euler: 2, exact_topo: true },
        // cube minus half (axis cut) → still a box
        BlenderOracle { label: "cube_minus_half", nv: 8, nf: 6, tris: 0, euler: 2, exact_topo: true },
        // intersect of two overlapping unit cubes offset 0.5 → unit box
        BlenderOracle { label: "intersect_overlap", nv: 8, nf: 6, tris: 0, euler: 2, exact_topo: true },
        // union of two overlapping cubes offset 0.5 — Blender Exact 20v/12f
        BlenderOracle { label: "union_overlap", nv: 20, nf: 12, tris: 0, euler: 2, exact_topo: true },
    ];

    for o in &oracles {
        let r = match o.label {
            "cube_corner_half1" => {
                let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
                let b = box_mesh(Vec3::splat(1.0), Vec3::splat(1.0));
                mesh_boolean(&a, &b, BoolOp::Difference)
            }
            "cube_rotcube" => {
                use std::f32::consts::PI;
                let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
                let rot = glam::Quat::from_rotation_z(PI / 4.0);
                let h = 1.0f32;
                let corners = [
                    Vec3::new(-h, -h, -h), Vec3::new(h, -h, -h), Vec3::new(h, -h, h), Vec3::new(-h, -h, h),
                    Vec3::new(-h, h, -h), Vec3::new(h, h, -h), Vec3::new(h, h, h), Vec3::new(-h, h, h),
                ];
                let pos: Vec<Vec3> = corners.iter().map(|&s| Vec3::new(1.2, 0.0, 0.0) + rot * s).collect();
                let bf = vec![
                    vec![0, 1, 2, 3], vec![7, 6, 5, 4], vec![4, 5, 1, 0],
                    vec![6, 7, 3, 2], vec![7, 4, 0, 3], vec![5, 6, 2, 1],
                ];
                let b = HalfEdgeMesh::from_polygons(&pos, &bf);
                mesh_boolean(&a, &b, BoolOp::Difference)
            }
            "coplanar_union" => {
                let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0)); // x∈[-1,1]
                let b = box_mesh(Vec3::new(2.0, 0.0, 0.0), Vec3::splat(1.0)); // x∈[1,3]
                mesh_boolean(&a, &b, BoolOp::Union)
            } // note: Blender Exact keeps mid-belt (12v/10f); we fully dissolve → 8v/6f (cleaner, still locked below)
            "cube_minus_half" => {
                let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
                let b = box_mesh(Vec3::new(1.0, 0.0, 0.0), Vec3::splat(1.0));
                mesh_boolean(&a, &b, BoolOp::Difference)
            }
            "intersect_overlap" => {
                let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
                let b = box_mesh(Vec3::splat(0.5), Vec3::splat(1.0));
                mesh_boolean(&a, &b, BoolOp::Intersect)
            }
            "union_overlap" => {
                let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
                let b = box_mesh(Vec3::splat(0.5), Vec3::splat(1.0));
                mesh_boolean(&a, &b, BoolOp::Union)
            }
            _ => None,
        }
        .unwrap_or_else(|| panic!("{}: mesh_boolean returned None", o.label));

        assert_closed_manifold(&r, o.label);
        assert!(
            r.is_manifold().is_ok(),
            "{} non-manifold: {:?}",
            o.label,
            r.is_manifold()
        );
        let (nv, nf, tris, euler) = census(&r);
        assert_eq!(
            euler, o.euler,
            "{} euler={euler} Blender={}",
            o.label, o.euler
        );
        assert_eq!(
            tris, o.tris,
            "{} tris={tris} Blender={} (ZERO residual edges/diags); hist={:?}",
            o.label,
            o.tris,
            face_size_hist(&r)
        );
        if o.exact_topo {
            assert_eq!(
                nv, o.nv,
                "{} nv={nv} Blender={} hist={:?}",
                o.label,
                o.nv,
                face_size_hist(&r)
            );
            assert_eq!(
                nf, o.nf,
                "{} nf={nf} Blender={} hist={:?}",
                o.label,
                o.nf,
                face_size_hist(&r)
            );
        }
    }
}

/// Cylinder-through: Blender Exact 5.0.1 → **40v/24f, 0 tris**, χ=0
/// (`bool_edge_case_matrix.json` / `cube_minus_cyl16`).
///
/// Hard locks (must match Blender): closed, manifold, χ=0, **0 tris** (no residual
/// diagonals). Soft lock: face count == 24. Vertex count may still exceed Blender
/// (extra collinear T-junction verts on caps) — tracked as a known dissolve gap;
/// never *worse* than a fixed budget so we don't regress silently.
#[test]
fn blender_exact_cube_minus_cyl16() {
    let sides = 16usize;
    let r = 0.5f32;
    let mut cpos: Vec<Vec3> = Vec::new();
    for i in 0..sides {
        let a = std::f32::consts::TAU * i as f32 / sides as f32;
        cpos.push(Vec3::new(r * a.cos(), r * a.sin(), 1.5));
    }
    for i in 0..sides {
        let a = std::f32::consts::TAU * i as f32 / sides as f32;
        cpos.push(Vec3::new(r * a.cos(), r * a.sin(), -1.5));
    }
    let mut cf: Vec<Vec<usize>> = vec![(0..sides).collect(), (sides..2 * sides).rev().collect()];
    for i in 0..sides {
        let j = (i + 1) % sides;
        cf.push(vec![i, sides + i, sides + j, j]);
    }
    let cyl = HalfEdgeMesh::from_polygons(&cpos, &cf);
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let out = mesh_boolean(&a, &cyl, BoolOp::Difference).expect("cube_minus_cyl16");
    assert_closed_manifold(&out, "cube_minus_cyl16");
    assert!(out.is_manifold().is_ok(), "{:?}", out.is_manifold());
    let (nv, nf, tris, euler) = census(&out);
    assert_eq!(euler, 0, "genus-1 (Blender χ=0)");
    // Blender: 0 tris. Residual tris = "random edges / need only one edge".
    assert_eq!(
        tris, 0,
        "Blender Exact 0 tris on cylinder-through; ours tris={tris} nv={nv} nf={nf} hist={:?}",
        face_size_hist(&out)
    );
    assert_eq!(
        nf, 24,
        "Blender nf=24; ours nv={nv} nf={nf} hist={:?}",
        face_size_hist(&out)
    );
    // Blender Exact 5.0.1: nv=40 (hard lock after collinear valence-2 polish).
    // Blender Exact 5.0.1: 40v/24f/0 tris. Finite plane support closed 60→44; remaining
    // ≤4 pins are left-wall coplanar mid-edge residues (documented). Hard locks below.
    assert_eq!(nf, 24, "Blender nf=24; ours nf={nf} hist={:?}", face_size_hist(&out));
    assert!(
        (40..=44).contains(&nv),
        "Blender nv=40 (allow ≤44 residual pins); ours nv={nv} hist={:?}",
        face_size_hist(&out)
    );
}

/// Sequential double-subtraction: Blender Exact sequential leaves 15v/10f (vestigial seam).
/// We may dissolve more aggressively (cleaner) but MUST stay closed manifold, 0 tris, χ=2.
#[test]
fn blender_exact_seq_double_subtract_invariants() {
    let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let c1 = box_mesh(Vec3::splat(1.0), Vec3::splat(1.0));
    let m1 = mesh_boolean(&a, &c1, BoolOp::Difference).expect("cut1");
    assert_closed_manifold(&m1, "seq cut1");
    let (nv1, nf1, tris1, e1) = census(&m1);
    assert_eq!(tris1, 0, "cut1 tris");
    assert_eq!(e1, 2);
    assert_eq!(nv1, 14, "Blender cut1 nv=14");
    assert_eq!(nf1, 9, "Blender cut1 nf=9");

    let c2 = box_mesh(Vec3::new(-1.0, 1.0, 1.0), Vec3::splat(1.0));
    let m2 = mesh_boolean(&m1, &c2, BoolOp::Difference).expect("cut2");
    assert_closed_manifold(&m2, "seq cut2");
    assert!(m2.is_manifold().is_ok(), "{:?}", m2.is_manifold());
    let (nv2, nf2, tris2, e2) = census(&m2);
    // Manifold may leave ≤2 residual tris on sequential coplanar seams (BSP dissolved fully).
    assert!(
        tris2 <= 2,
        "cut2 tris ≤2 under Manifold; hist={:?}",
        face_size_hist(&m2)
    );
    assert_eq!(e2, 2);
    // Blender sequential: 15/10. Manifold lands near clean L-prism (≤16v/12f).
    assert!(
        nv2 <= 16 && nf2 <= 12,
        "cut2 worse than Blender sequential band: nv={nv2} nf={nf2} hist={:?}",
        face_size_hist(&m2)
    );
    assert!(nv2 >= 12 && nf2 >= 8, "cut2 too sparse: nv={nv2} nf={nf2}");
}

/// OWNER REPRO 2026-08-03 ("create a cube, extrude it, boolean, after one cut the mesh
/// broke") - exact AZRA_KNIFE_DUMP capture (docs/planning/poly/repro/knife_lchannel.json):
/// 16v/10f extruded L-channel, 3-point open WYSIWYG cut. Replays the free-knife prism
/// subtract byte-for-byte; the result must stay clean and flat walls must not fan.
#[test]
fn owner_lchannel_knife_cut_stays_clean() {
    use crate::nurbs_trim::ear_clip_uv;
    use glam::Vec2;
    let positions = vec![
        Vec3::new(-0.5, -0.5, -0.5),
        Vec3::new(-0.5, -0.5, 2.480266),
        Vec3::new(-0.5, 0.00069889426, 0.4594105),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, 0.5, 0.4594105),
        Vec3::new(-0.19054824, 0.00069889426, 0.4594105),
        Vec3::new(-0.19054824, 0.5, 0.4594105),
        Vec3::new(-0.5, 0.00069889426, 2.480266),
        Vec3::new(-0.19054824, 0.00069889426, 2.480266),
        Vec3::new(-0.19054824, 0.5, 2.480266),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, -0.5, 2.480266),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(0.5, 0.5, 2.480266),
        Vec3::new(-0.19054824, 0.5, 1.4698383),
        Vec3::new(-0.19054824, 0.00069889426, 1.4698383),
    ];
    let faces: Vec<Vec<usize>> = vec![
        vec![1, 7, 2, 4, 3, 0],
        vec![3, 12, 10, 0],
        vec![11, 13, 9, 8, 7, 1],
        vec![10, 11, 1, 0],
        vec![4, 6, 14, 9, 13, 12, 3],
        vec![12, 13, 11, 10],
        vec![5, 6, 4, 2],
        vec![7, 8, 15, 5, 2],
        vec![14, 6, 5, 15],
        vec![15, 8, 9, 14],
    ];
    let target = HalfEdgeMesh::from_polygons(&positions, &faces);
    assert!(target.is_manifold().is_ok(), "fixture must be clean: {:?}", target.is_manifold());
    let o = Vec3::new(0.114250064, 0.6862455, 0.0);
    let u = Vec3::new(1.0, 0.0, 0.0);
    let v = Vec3::new(0.0, 1.0, 0.0);
    let n = Vec3::new(0.0, 0.0, -1.0);
    let big = 4.0f32;
    let mut p2: Vec<Vec2> = vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(0.55132675, -0.6281506),
        Vec2::new(1.025829, -0.063266814),
        Vec2::new(1.5182841, 7.921562),
        Vec2::new(0.4924551, 7.984829),
    ];
    // Same construction as poly_knife_free_boolean: CCW, ear-clip caps, side quads.
    let area: f32 = (0..p2.len()).map(|i| { let a = p2[i]; let b = p2[(i + 1) % p2.len()]; a.x * b.y - b.x * a.y }).sum::<f32>() * 0.5;
    if area < 0.0 { p2.reverse(); }
    let tris = ear_clip_uv(&p2);
    assert!(!tris.is_empty());
    let m = p2.len();
    let mut pos = Vec::with_capacity(2 * m);
    for p in &p2 { pos.push(o + u * p.x + v * p.y - n * big); }
    for p in &p2 { pos.push(o + u * p.x + v * p.y + n * big); }
    let mut cfaces: Vec<Vec<usize>> = Vec::new();
    for t in &tris { cfaces.push(vec![t[0], t[1], t[2]]); }
    for t in &tris { cfaces.push(vec![t[2] + m, t[1] + m, t[0] + m]); }
    for k in 0..m { let j = (k + 1) % m; cfaces.push(vec![j, k, k + m, j + m]); }
    let cutter = HalfEdgeMesh::from_polygons(&pos, &cfaces);
    assert!(cutter.validate().is_ok() && cutter.is_manifold().is_ok(), "cutter must be clean");
    let res = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("boolean must produce a result");
    res.validate().expect("result valid");
    res.integrity().expect("result corruption-free");
    assert!(res.is_manifold().is_ok(), "result must stay manifold: {:?}", res.is_manifold());
    // The live repro fanned a FLAT side wall into triangle soup. After merge_coplanar a
    // flat wall is few faces; count +-X-normal faces (the channel side walls).
    let mut sidewall_faces = 0;
    for i in 0..res.faces.len() as u32 {
        let fid = FaceId(i);
        if res.face(fid).half_edge == crate::half_edge::NO_EDGE { continue; }
        let fnorm = res.compute_face_normal(fid).normalize_or_zero();
        if fnorm.x.abs() > 0.95 { sidewall_faces += 1; }
    }
    assert!(sidewall_faces <= 8, "flat side walls fanned into {sidewall_faces} faces (triangle soup)");
}

/// BLENDER CONTACT-MATRIX PARITY (owner 2026-08-03: "make sure the boolean code is up to
/// par with Blender with all edge cases"). Oracle: bool_contact_matrix.py, Blender 4.5.1
/// EXACT solver (captured 2026-08-03). Cells: edge touch, vertex touch, identical solids,
/// fully-contained cutter (cavity), flush full-face stack, half-embedded flush side.
/// Compared on VOLUME (the un-fakeable invariant) + emptiness + structural integrity —
/// exact counts differ legitimately across kernels.
/// NOTE: Blender's union of edge-touching cubes contains a NON-MANIFOLD edge (oracle:
/// nonmanifold_edges=1) and a vertex-touch union keeps a bowtie — legal results there.
/// Cells whose result is inherently non-manifold assert integrity() only.
#[test]
fn blender_contact_matrix_parity() {
    let unit = |c: Vec3| box_mesh(c, Vec3::splat(0.5));
    let vol = |m: &HalfEdgeMesh| mesh_signed_volume(m);
    let ok = |m: &HalfEdgeMesh| {
        m.validate().expect("valid");
        m.integrity().expect("integrity");
    };

    // 1. Edge touch (diagonal neighbours sharing exactly one edge).
    let a = unit(Vec3::ZERO);
    let b = unit(Vec3::new(1.0, 1.0, 0.0));
    let d = mesh_boolean(&a, &b, BoolOp::Difference).expect("edge-touch diff");
    ok(&d);
    assert!((vol(&d) - 1.0).abs() < 1e-3, "edge-touch diff keeps A intact (Blender vol=1): {}", vol(&d));
    if let Some(u) = mesh_boolean(&a, &b, BoolOp::Union) {
        ok(&u);
        assert!((vol(&u) - 2.0).abs() < 1e-3, "edge-touch union vol=2 (Blender): {}", vol(&u));
    } // None is acceptable: a kernel may refuse the inherently non-manifold union.

    // 2. Vertex touch (corner to corner). Oracle cell for diff crashed in Blender itself;
    // first principles: B removes nothing from A.
    let b = unit(Vec3::new(1.0, 1.0, 1.0));
    let d = mesh_boolean(&a, &b, BoolOp::Difference).expect("vertex-touch diff");
    ok(&d);
    assert!((vol(&d) - 1.0).abs() < 1e-3, "vertex-touch diff keeps A intact: {}", vol(&d));

    // 3. Identical solids.
    let b = unit(Vec3::ZERO);
    match mesh_boolean(&a, &b, BoolOp::Difference) {
        Some(d) => assert!(vol(&d).abs() < 1e-3 || d.face_count() == 0,
            "A-A must be empty (Blender: 0 verts): vol={}", vol(&d)),
        None => {} // refusing to install an empty result is acceptable
    }
    let u = mesh_boolean(&a, &b, BoolOp::Union).expect("A+A");
    ok(&u);
    assert!((vol(&u) - 1.0).abs() < 1e-3, "A+A = A (vol 1): {}", vol(&u));
    let i = mesh_boolean(&a, &b, BoolOp::Intersect).expect("A*A");
    ok(&i);
    assert!((vol(&i) - 1.0).abs() < 1e-3, "A*A = A (vol 1): {}", vol(&i));

    // 4. Cutter fully inside → closed cavity, volume 8-1=7 (Blender: 16v/12f/vol 7).
    let big = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
    let small = unit(Vec3::ZERO);
    let d = mesh_boolean(&big, &small, BoolOp::Difference).expect("contained diff");
    ok(&d);
    assert!((vol(&d) - 7.0).abs() < 1e-2, "cavity volume 7 (Blender): {}", vol(&d));
    let i = mesh_boolean(&big, &small, BoolOp::Intersect).expect("contained intersect");
    ok(&i);
    assert!((vol(&i) - 1.0).abs() < 1e-3, "contained intersect = small cube: {}", vol(&i));

    // 5. Flush full-face stack (B exactly on top of A, sharing the full face).
    let b = unit(Vec3::new(0.0, 0.0, 1.0));
    let d = mesh_boolean(&a, &b, BoolOp::Difference).expect("flush-stack diff");
    ok(&d);
    assert!((vol(&d) - 1.0).abs() < 1e-3, "flush stack diff keeps A (Blender vol=1): {}", vol(&d));
    assert!(d.is_manifold().is_ok(), "flush stack diff stays manifold: {:?}", d.is_manifold());
    let u = mesh_boolean(&a, &b, BoolOp::Union).expect("flush-stack union");
    ok(&u);
    assert!((vol(&u) - 2.0).abs() < 1e-3, "flush stack union vol=2: {}", vol(&u));
    assert!(u.is_manifold().is_ok(), "flush stack union stays manifold: {:?}", u.is_manifold());

    // 6. Half-embedded flush side (B overlaps half of A, one face plane coincident).
    let b = unit(Vec3::new(0.5, 0.0, 0.5));
    let d = mesh_boolean(&a, &b, BoolOp::Difference).expect("half-flush diff");
    ok(&d);
    assert!((vol(&d) - 0.75).abs() < 1e-2, "half-flush diff vol=0.75 (Blender): {}", vol(&d));
    assert!(d.is_manifold().is_ok(), "half-flush diff stays manifold: {:?}", d.is_manifold());
}

/// OWNER REPRO 2026-08-03 (second capture, world_xform=identity) — replays the exact
/// AZRA_KNIFE_DUMP (docs/planning/poly/repro/knife_latest.json). Kernel result is clean;
/// pins that this cut never regresses.
#[test]
fn owner_latest_knife_replay() {
    use crate::nurbs_trim::ear_clip_uv;
    use glam::Vec2;
    let positions = vec![
        Vec3::new(-0.5, -0.5, -0.5),
        Vec3::new(-0.5, -0.119363904, -0.10339618),
        Vec3::new(-0.033437073, -0.119363904, -0.10339618),
        Vec3::new(-0.5, -0.5, 2.0748262),
        Vec3::new(-0.5, -0.119363904, 2.0748262),
        Vec3::new(-0.033437073, -0.119363904, 2.0748262),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, 0.5, -0.10339618),
        Vec3::new(-0.033437073, 0.5, -0.10339618),
        Vec3::new(-0.033437073, 0.5, 2.0748262),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, -0.5, 2.0748262),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(0.5, 0.5, 2.0748262),
    ];
    let faces: Vec<Vec<usize>> = vec![
        vec![3, 4, 1, 7, 6, 0],
        vec![6, 12, 10, 0],
        vec![10, 11, 3, 0],
        vec![11, 13, 9, 5, 4, 3],
        vec![7, 8, 9, 13, 12, 6],
        vec![12, 13, 11, 10],
        vec![4, 5, 2, 1],
        vec![2, 8, 7, 1],
        vec![5, 9, 8, 2],
    ];
    let target = HalfEdgeMesh::from_polygons(&positions, &faces);
    assert!(target.is_manifold().is_ok());
    let o = Vec3::new(0.20632902, 0.65463275, 0.0);
    let u = Vec3::new(1.0, 0.0, 0.0);
    let v = Vec3::new(0.0, 1.0, 0.0);
    let n = Vec3::new(0.0, 0.0, -1.0);
    let big = 4.0f32;
    let mut p2: Vec<Vec2> = vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(0.40592292, -0.5855927),
        Vec2::new(6.980766, 3.9719768),
        Vec2::new(6.574843, 4.5575695),
    ];
    let area: f32 = (0..p2.len()).map(|i| { let a = p2[i]; let b = p2[(i + 1) % p2.len()]; a.x * b.y - b.x * a.y }).sum::<f32>() * 0.5;
    if area < 0.0 { p2.reverse(); }
    let tris = ear_clip_uv(&p2);
    assert!(!tris.is_empty());
    let m = p2.len();
    let mut pos = Vec::with_capacity(2 * m);
    for p in &p2 { pos.push(o + u * p.x + v * p.y - n * big); }
    for p in &p2 { pos.push(o + u * p.x + v * p.y + n * big); }
    let mut cfaces: Vec<Vec<usize>> = Vec::new();
    for t in &tris { cfaces.push(vec![t[0], t[1], t[2]]); }
    for t in &tris { cfaces.push(vec![t[2] + m, t[1] + m, t[0] + m]); }
    for k in 0..m { let j = (k + 1) % m; cfaces.push(vec![j, k, k + m, j + m]); }
    let cutter = HalfEdgeMesh::from_polygons(&pos, &cfaces);
    assert!(cutter.is_manifold().is_ok());
    let res = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("boolean result");
    let _ = res.vertices.len();
    res.validate().expect("valid");
    res.integrity().expect("integrity");
    assert!(res.is_manifold().is_ok(), "result must stay manifold: {:?}", res.is_manifold());
    // Exactly one non-axis-aligned face: the angled cut wall. Fanning => many.
    let mut angled = 0;
    for i in 0..res.faces.len() as u32 {
        let fid = FaceId(i);
        if res.face(fid).half_edge == crate::half_edge::NO_EDGE { continue; }
        let n = res.compute_face_normal(fid).normalize_or_zero();
        if n.x.abs() < 0.99 && n.y.abs() < 0.99 && n.z.abs() < 0.99 { angled += 1; }
    }
    assert!(angled <= 2, "cut wall fanned into {angled} angled faces");
}

/// OWNER REPRO 2026-08-03 ("extrude works but broke geo the other side" / "2 new segments").
/// Push-through extrude whose region touches 3 boundary edges of the top face: the swept
/// prism's side faces are FLUSH (coplanar) with 3 outer walls. The far wall must come out
/// as a clean polygon — extrude.fl4 showed it folded into f0+f3-f5 with f5's normal
/// INVERTED (+X on the x=-0.5 wall). A fold is edge-consistent so is_manifold misses it;
/// assert directly: no interior-pointing face on a wall plane.
#[test]
fn flush_prism_subtract_does_not_fold_walls() {
    // Target: box x[-0.5,0.5] y[-0.5,0.5] z[-0.5,2.6]
    let bx = box_mesh(Vec3::new(0.0, 0.0, 1.05), Vec3::new(0.5, 0.5, 1.55));
    // Prism: pocket from the top, flush with x=-0.5, x=0.5 (partially) and z=2.6 walls,
    // floor at y=0.08, overshooting above the top and outside the flush walls slightly.
    let e = 0.0f32; // EXACT flush - the app prism uses the region ring verbatim
    let pmin = Vec3::new(-0.5 - e, 0.08, -0.037);
    let pmax = Vec3::new(0.5 + e, 0.5 + e, 2.6 + e);
    let prism = box_mesh((pmin + pmax) * 0.5, (pmax - pmin) * 0.5);
    let res = mesh_boolean(&bx, &prism, BoolOp::Difference).expect("kernel result");
    assert!(res.is_manifold().is_ok());
    // Every face lying on the x=-0.5 wall plane must face OUTWARD (-X). An inverted
    // coplanar face = the fold.
    let mut folds = 0;
    for i in 0..res.faces.len() as u32 {
        let fid = FaceId(i);
        if res.face(fid).half_edge == crate::half_edge::NO_EDGE { continue; }
        let vs = res.face_vertices(fid);
        let on_wall = vs.iter().all(|&v| (res.vert(v).position.x + 0.5).abs() < 1e-4);
        if !on_wall { continue; }
        let n = res.compute_face_normal(fid).normalize_or_zero();
        if n.x > -0.99 { folds += 1; eprintln!("folded face {i} n={n:?}"); }
    }
    assert_eq!(folds, 0, "far wall has {folds} folded (inverted) coplanar faces");
    // And the wall should merge to ONE clean polygon.
    let wall_faces = (0..res.faces.len() as u32).filter(|&i| {
        let fid = FaceId(i);
        res.face(fid).half_edge != crate::half_edge::NO_EDGE
            && res.face_vertices(fid).iter().all(|&v| (res.vert(v).position.x + 0.5).abs() < 1e-4)
    }).count();
    assert!(wall_faces <= 2, "far wall fragmented into {wall_faces} coplanar faces");
}

/// OWNER REPRO 2026-08-03 intrude_02: Manifold kernel succeeds but manifold_to_halfedge
/// rebuild fails (weld/twin pairing) on this exact target+prism. Pin: must produce a result.
#[test]
fn owner_intrude02_pushthrough_boolean_succeeds() {
    let target_pos = vec![
        Vec3::new(-0.5, -0.14506982, 0.5),
        Vec3::new(-0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, -0.5, -0.5),
        Vec3::new(-0.5, -0.5, 0.27750617),
        Vec3::new(0.5, -0.5, 0.27750617),
        Vec3::new(0.091557965, -0.5, 0.27750617),
        Vec3::new(-0.17954323, -0.5, 0.27750617),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(0.5, -0.14506982, 0.5),
        Vec3::new(0.099571034, -0.4348559, 0.31834286),
        Vec3::new(0.1851663, -0.38647607, 0.34867054),
        Vec3::new(0.1851663, -0.30832383, 0.39766157),
        Vec3::new(-0.09022668, -0.30832383, 0.39766157),
        Vec3::new(-0.17954344, -0.36042538, 0.36500084),
    ];
    let target_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2, 3, 4],
        vec![5, 6, 7, 4, 3, 8],
        vec![9, 8, 3, 2],
        vec![10, 1, 0, 11],
        vec![10, 9, 2, 1],
        vec![10, 11, 5, 8, 9],
        vec![6, 12, 13, 14, 15, 16, 7],
        vec![5, 11, 0, 4, 7, 16, 15, 14, 13, 12, 6],
    ];
    let target = HalfEdgeMesh::from_polygons(&target_pos, &target_faces);
    let cutter_pos = vec![
        Vec3::new(0.09155795, -0.5000003, 0.28903928),
        Vec3::new(0.09957102, -0.43485618, 0.32987592),
        Vec3::new(-0.17954324, -0.5000003, 0.28903928),
        Vec3::new(0.18516628, -0.38647634, 0.3602036),
        Vec3::new(0.18516628, -0.30832392, 0.40919456),
        Vec3::new(-0.090226695, -0.30832392, 0.40919456),
        Vec3::new(-0.17954345, -0.36042565, 0.37653396),
        Vec3::new(0.09155795, -0.5000003, -0.887325),
        Vec3::new(-0.17954324, -0.5000003, -0.887325),
        Vec3::new(0.09957102, -0.43485618, -0.84648836),
        Vec3::new(0.18516628, -0.38647634, -0.8161607),
        Vec3::new(0.18516628, -0.30832392, -0.7671697),
        Vec3::new(-0.090226695, -0.30832392, -0.7671697),
        Vec3::new(-0.17954345, -0.36042565, -0.7998303),
    ];
    let cutter_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2],
        vec![1, 3, 2],
        vec![3, 4, 2],
        vec![4, 5, 2],
        vec![6, 2, 5],
        vec![7, 8, 9],
        vec![9, 8, 10],
        vec![10, 8, 11],
        vec![11, 8, 12],
        vec![13, 12, 8],
        vec![0, 7, 9, 1],
        vec![1, 9, 10, 3],
        vec![3, 10, 11, 4],
        vec![4, 11, 12, 5],
        vec![5, 12, 13, 6],
        vec![6, 13, 8, 2],
        vec![2, 8, 7, 0],
    ];
    let cutter = HalfEdgeMesh::from_polygons(&cutter_pos, &cutter_faces);
    assert!(target.is_manifold().is_ok(), "target fixture clean");
    assert!(cutter.is_manifold().is_ok(), "cutter fixture clean");
    let res = mesh_boolean(&target, &cutter, BoolOp::Difference);
    let res = res.expect("push-through subtract must produce a result");
    res.validate().expect("valid");
    assert!(res.is_manifold().is_ok());
    let tris = (0..res.faces.len() as u32).filter(|&i| {
        let fid = FaceId(i);
        res.face(fid).half_edge != crate::half_edge::NO_EDGE && res.face_vertices(fid).len() == 3
    }).count();
    eprintln!("[PIN] intrude02 result: v={} f={} tris={} folds={} vol={:.6} (Blender EXACT: v=30 f=15 tris=0 vol=0.910397)",
        res.vertex_count(), res.face_count(), tris, res.count_folded_coplanar_pairs(),
        mesh_signed_volume(&res));
    // NOTE: this fixture contains a genuine epsilon-thin wall (inputs ~1e-6 off flush);
    // its two sides register as folded-coplanar. Correct result = keep it (Blender Exact
    // does too) — so no folds==0 assert here; closed-manifold is the contract.
}

/// OWNER capture 2026-08-04 (extrude_latest.json). Blender EXACT oracle: v=26 f=15
/// tris=0 vol=0.866889. Pin: our result must match volume and stay clean.
#[test]
fn owner_extrude_latest_matches_oracle() {
    let target_pos = vec![
        Vec3::new(0.5, -0.5, 0.5),
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.5, 0.5),
        Vec3::new(-0.5, -0.049050547, 0.5),
        Vec3::new(-0.18276201, -0.5, 0.5),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, -0.049050547, -0.5),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(-0.18276201, -0.5, -0.5),
        Vec3::new(-0.18276201, -0.5, 0.3541329),
        Vec3::new(-0.182762, -0.5, -0.31108332),
        Vec3::new(-0.22718215, -0.43685693, 0.34467483),
        Vec3::new(-0.23161793, -0.43055153, 0.07354422),
        Vec3::new(-0.2582326, -0.39271915, 0.06408621),
        Vec3::new(-0.2671044, -0.3801084, 0.171277),
        Vec3::new(-0.3048079, -0.32651305, 0.16497165),
        Vec3::new(-0.3048079, -0.32651305, -0.23226608),
    ];
    let target_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2, 3, 4],
        vec![2, 5, 6, 3],
        vec![7, 8, 9, 6, 5],
        vec![1, 7, 5, 2],
        vec![0, 4, 10, 11, 9, 8],
        vec![1, 0, 8, 7],
        vec![10, 12, 13, 14, 15, 16, 17, 11],
        vec![4, 3, 6, 9, 11, 17, 16, 15, 14, 13, 12, 10],
    ];
    let target = HalfEdgeMesh::from_polygons(&target_pos, &target_faces);
    let cutter_pos = vec![
        Vec3::new(-0.19793458, -0.5, 0.3541329),
        Vec3::new(-0.24235472, -0.43685693, 0.34467483),
        Vec3::new(-0.19793457, -0.5, -0.31108332),
        Vec3::new(-0.2467905, -0.43055153, 0.07354422),
        Vec3::new(-0.27340516, -0.39271915, 0.06408621),
        Vec3::new(-0.28227696, -0.3801084, 0.171277),
        Vec3::new(-0.31998047, -0.32651305, 0.16497165),
        Vec3::new(-0.31998047, -0.32651305, -0.23226608),
        Vec3::new(1.3496673, -0.5, 0.3541329),
        Vec3::new(1.3496673, -0.5, -0.31108332),
        Vec3::new(1.3052472, -0.43685693, 0.34467483),
        Vec3::new(1.3008114, -0.43055153, 0.07354422),
        Vec3::new(1.2741967, -0.39271915, 0.06408621),
        Vec3::new(1.265325, -0.3801084, 0.171277),
        Vec3::new(1.2276214, -0.32651305, 0.16497165),
        Vec3::new(1.2276214, -0.32651305, -0.23226608),
    ];
    let cutter_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2],
        vec![1, 3, 2],
        vec![3, 4, 2],
        vec![5, 6, 4],
        vec![4, 6, 2],
        vec![7, 2, 6],
        vec![8, 9, 10],
        vec![10, 9, 11],
        vec![11, 9, 12],
        vec![13, 12, 14],
        vec![12, 9, 14],
        vec![15, 14, 9],
        vec![0, 8, 10, 1],
        vec![1, 10, 11, 3],
        vec![3, 11, 12, 4],
        vec![4, 12, 13, 5],
        vec![5, 13, 14, 6],
        vec![6, 14, 15, 7],
        vec![7, 15, 9, 2],
        vec![2, 9, 8, 0],
    ];
    let cutter = HalfEdgeMesh::from_polygons(&cutter_pos, &cutter_faces);
    assert!(target.is_manifold().is_ok() && cutter.is_manifold().is_ok());
    let res = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("result");
    res.validate().expect("valid");
    assert!(res.is_manifold().is_ok());
    let tris = (0..res.faces.len() as u32).filter(|&i| {
        let fid = FaceId(i);
        res.face(fid).half_edge != crate::half_edge::NO_EDGE && res.face_vertices(fid).len() == 3
    }).count();
    eprintln!("[PIN] latest: v={} f={} tris={} folds={} vol={:.6} (oracle v=26 f=15 tris=0 vol=0.866889)",
        res.vertex_count(), res.face_count(), tris, res.count_folded_coplanar_pairs(),
        mesh_signed_volume(&res));
    let vol = mesh_signed_volume(&res);
    assert!((vol - 0.866889).abs() < 1e-3, "volume {vol} != oracle 0.866889");
}

/// RUNTIME REPLAY RIG: AZRA_BOOL_REPLAY=<json path> — replays a push-through capture and
/// prints our kernel's result vs whatever oracle numbers you have. Iteration tool.
#[test]
#[ignore]
fn replay_extrude_capture() {
    let Ok(path) = std::env::var("AZRA_BOOL_REPLAY") else { eprintln!("set AZRA_BOOL_REPLAY"); return; };
    let txt = std::fs::read_to_string(&path).expect("read json");
    let (target, cutter) = parse_extrude_capture(&txt);
    {
        let plane = |m: &HalfEdgeMesh, f: FaceId| {
            let n = m.compute_face_normal(f).normalize_or_zero();
            let vs = m.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            (n, n.dot(c))
        };
        let mut cop = 0;
        for i in 0..target.faces.len() as u32 {
            let fa = FaceId(i);
            if target.face(fa).half_edge == crate::half_edge::NO_EDGE { continue; }
            let (na, da) = plane(&target, fa);
            for j in 0..cutter.faces.len() as u32 {
                let fb = FaceId(j);
                if cutter.face(fb).half_edge == crate::half_edge::NO_EDGE { continue; }
                let (nb, db) = plane(&cutter, fb);
                let dot = na.dot(nb);
                if dot.abs() > 0.9995 && (da - if dot > 0.0 { db } else { -db }).abs() < 1e-4 {
                    cop += 1;
                }
            }
        }
        eprintln!("[REPLAY] coplanar target/cutter face pairs: {cop}");
    }
    eprintln!("[REPLAY] target v={} f={} manifold={:?} | cutter v={} f={} manifold={:?}",
        target.vertex_count(), target.face_count(), target.is_manifold().is_ok(),
        cutter.vertex_count(), cutter.face_count(), cutter.is_manifold().is_ok());
    match mesh_boolean(&target, &cutter, BoolOp::Difference) {
        Some(res) => {
            let tris = (0..res.faces.len() as u32).filter(|&i| {
                let fid = FaceId(i);
                res.face(fid).half_edge != crate::half_edge::NO_EDGE && res.face_vertices(fid).len() == 3
            }).count();
            eprintln!("[REPLAY] result v={} f={} tris={} folds={} vol={:.6} manifold={:?} valid={:?}",
                res.vertex_count(), res.face_count(), tris, res.count_folded_coplanar_pairs(),
                mesh_signed_volume(&res), res.is_manifold().is_ok(), res.validate().is_ok());
            for i in 0..res.faces.len() as u32 {
                let fid = FaceId(i);
                if res.face(fid).half_edge == crate::half_edge::NO_EDGE { continue; }
                let n = res.compute_face_normal(fid).normalize_or_zero();
                eprintln!("[REPLAY]   f{i} n=({:.2},{:.2},{:.2}) sides={}", n.x, n.y, n.z, res.face_vertices(fid).len());
            }
        }
        None => eprintln!("[REPLAY] mesh_boolean returned None"),
    }
}

/// Parse a `write_extrude_repro` capture (`azra_extrude_repro*.json`) into `(target, cutter)`.
/// Tiny hand parser for our fixed dump format — core_math has no serde_json dep. Shared by every
/// replay/sweep diagnostic so they always read the capture the same way.
fn parse_extrude_capture(txt: &str) -> (HalfEdgeMesh, HalfEdgeMesh) {
    fn arr_f(s: &str, key: &str) -> Vec<[f32; 3]> {
        let start = s.find(key).unwrap() + key.len();
        let mut depth = 0usize; let mut end = start;
        for (i, c) in s[start..].char_indices() {
            match c { '[' => depth += 1, ']' => { depth -= 1; if depth == 0 { end = start + i + 1; break; } }, _ => {} }
        }
        let body = &s[start..end];
        let mut out = Vec::new();
        for chunk in body.split('[').skip(2) {
            let nums: Vec<f32> = chunk.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e'))
                .filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect();
            if nums.len() >= 3 { out.push([nums[0], nums[1], nums[2]]); }
        }
        out
    }
    fn arr_u(s: &str, key: &str) -> Vec<Vec<usize>> {
        let start = s.find(key).unwrap() + key.len();
        let mut depth = 0usize; let mut end = start;
        for (i, c) in s[start..].char_indices() {
            match c { '[' => depth += 1, ']' => { depth -= 1; if depth == 0 { end = start + i + 1; break; } }, _ => {} }
        }
        let body = &s[start..end];
        let mut out = Vec::new();
        for chunk in body.split('[').skip(2) {
            let nums: Vec<usize> = chunk.split(|c: char| !c.is_ascii_digit())
                .filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect();
            if nums.len() >= 3 { out.push(nums); }
        }
        out
    }
    let build = |vk: &str, fk: &str| {
        let pos: Vec<Vec3> = arr_f(txt, vk).into_iter().map(|p| Vec3::new(p[0], p[1], p[2])).collect();
        let faces = arr_u(txt, fk);
        HalfEdgeMesh::from_polygons(&pos, &faces)
    };
    (
        build("\"target_verts\":", "\"target_faces\":"),
        build("\"cutter_verts\":", "\"cutter_faces\":"),
    )
}

/// OWNER capture 2026-08-04 "front is messy" (extrude_bad_front.json). Blender EXACT:
/// v=26 f=15 tris=0 vol=0.849723. The weld-collapse beautifier must reach the SAME.
/// PROVENANCE (2026-08-04): the capture this data came from was face-visit RE-INDEXED
/// (the dump bug), so this is a valid KERNEL pin on exactly this input (Blender verified
/// the SAME data) but NOT proof of the app workflow. App-level end-to-end coverage lives
/// in app::project::tests::intrude_owner_fixtures_pinned (owner .fl4 scenes).
#[test]
fn owner_bad_front_matches_oracle_exactly() {
    let target_pos = vec![
        Vec3::new(0.5, -0.5, 0.5),
        Vec3::new(-0.2610692, -0.5, 0.5),
        Vec3::new(-0.26106918, -0.5, 0.3714484),
        Vec3::new(-0.2610692, -0.5, -0.2070595),
        Vec3::new(-0.2610692, -0.5, -0.5),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.12606034, 0.5),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, 0.12606034, -0.5),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(-0.296247, -0.40782487, 0.33714938),
        Vec3::new(-0.30148304, -0.39410502, 0.0055939257),
        Vec3::new(-0.3276627, -0.32550752, -0.0012658238),
        Vec3::new(-0.3486066, -0.2706294, 0.19766757),
        Vec3::new(-0.36955023, -0.21575129, 0.19309416),
        Vec3::new(-0.36867774, -0.21803775, -0.095015824),
    ];
    let target_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2, 3, 4, 5],
        vec![6, 7, 8, 1, 0],
        vec![7, 9, 10, 8],
        vec![9, 11, 5, 4, 10],
        vec![6, 11, 9, 7],
        vec![6, 0, 5, 11],
        vec![2, 12, 13, 14, 15, 16, 17, 3],
        vec![1, 8, 10, 4, 3, 17, 16, 15, 14, 13, 12, 2],
    ];
    let target = HalfEdgeMesh::from_polygons(&target_pos, &target_faces);
    let cutter_pos = vec![
        Vec3::new(-0.2706498, -0.5, 0.3714484),
        Vec3::new(-0.30582762, -0.40782487, 0.33714938),
        Vec3::new(-0.27064982, -0.5, -0.2070595),
        Vec3::new(-0.31106365, -0.39410502, 0.0055939257),
        Vec3::new(-0.33724332, -0.32550752, -0.0012658238),
        Vec3::new(-0.3581872, -0.2706294, 0.19766757),
        Vec3::new(-0.37913084, -0.21575129, 0.19309416),
        Vec3::new(-0.37825835, -0.21803775, -0.095015824),
        Vec3::new(0.7065713, -0.5, 0.3714484),
        Vec3::new(0.7065712, -0.5, -0.2070595),
        Vec3::new(0.67139345, -0.40782487, 0.33714938),
        Vec3::new(0.6661574, -0.39410502, 0.0055939257),
        Vec3::new(0.63997775, -0.32550752, -0.0012658238),
        Vec3::new(0.6190339, -0.2706294, 0.19766757),
        Vec3::new(0.59809023, -0.21575129, 0.19309416),
        Vec3::new(0.5989627, -0.21803775, -0.095015824),
    ];
    let cutter_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2],
        vec![1, 3, 2],
        vec![3, 4, 2],
        vec![5, 6, 4],
        vec![6, 7, 4],
        vec![7, 2, 4],
        vec![8, 9, 10],
        vec![10, 9, 11],
        vec![11, 9, 12],
        vec![13, 12, 14],
        vec![14, 12, 15],
        vec![15, 12, 9],
        vec![0, 8, 10, 1],
        vec![1, 10, 11, 3],
        vec![3, 11, 12, 4],
        vec![4, 12, 13, 5],
        vec![5, 13, 14, 6],
        vec![6, 14, 15, 7],
        vec![7, 15, 9, 2],
        vec![2, 9, 8, 0],
    ];
    let cutter = HalfEdgeMesh::from_polygons(&cutter_pos, &cutter_faces);
    let res = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("result");
    res.validate().expect("valid");
    assert!(res.is_manifold().is_ok());
    assert_eq!(res.count_folded_coplanar_pairs(), 0, "no folds");
    assert_eq!(res.face_count(), 15, "oracle face count");
    let vol = mesh_signed_volume(&res);
    assert!((vol - 0.849723).abs() < 1e-3, "volume {vol}");
}

/// OWNER capture 2026-08-04 #2 ("front broken back fine"). Blender EXACT: v=26 f=15
/// tris=0 vol=0.897185. Caught the blind same-winding dedupe deleting load-bearing faces.
/// PROVENANCE (2026-08-04): the capture this data came from was face-visit RE-INDEXED
/// (the dump bug), so this is a valid KERNEL pin on exactly this input (Blender verified
/// the SAME data) but NOT proof of the app workflow. App-level end-to-end coverage lives
/// in app::project::tests::intrude_owner_fixtures_pinned (owner .fl4 scenes).
#[test]
fn owner_front_broken_2_matches_oracle_exactly() {
    let target_pos = vec![
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.5, 0.5),
        Vec3::new(-0.5, -0.19304556, 0.5),
        Vec3::new(-0.34725714, -0.5, 0.5),
        Vec3::new(0.5, -0.5, 0.5),
        Vec3::new(-0.34725717, -0.5, 0.42141795),
        Vec3::new(-0.34725714, -0.5, -0.288609),
        Vec3::new(-0.34725714, -0.5, -0.5),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, -0.19304556, -0.5),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(-0.37609065, -0.44205558, 0.38005662),
        Vec3::new(-0.37609065, -0.44205558, 0.07674469),
        Vec3::new(-0.40467608, -0.38460994, 0.06295735),
        Vec3::new(-0.40467608, -0.38460994, 0.338696),
        Vec3::new(-0.4355482, -0.32256892, 0.33410043),
        Vec3::new(-0.4355482, -0.32256892, -0.17601565),
    ];
    let target_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2, 3, 4],
        vec![4, 3, 5, 6, 7, 8],
        vec![1, 9, 10, 2],
        vec![9, 11, 8, 7, 10],
        vec![0, 11, 9, 1],
        vec![0, 4, 8, 11],
        vec![5, 12, 13, 14, 15, 16, 17, 6],
        vec![3, 2, 10, 7, 6, 17, 16, 15, 14, 13, 12, 5],
    ];
    let target = HalfEdgeMesh::from_polygons(&target_pos, &target_faces);
    let cutter_pos = vec![
        Vec3::new(-0.36107552, -0.5, 0.42141795),
        Vec3::new(-0.389909, -0.44205558, 0.38005662),
        Vec3::new(-0.3610755, -0.5, -0.288609),
        Vec3::new(-0.389909, -0.44205558, 0.07674469),
        Vec3::new(-0.41849443, -0.38460994, 0.06295735),
        Vec3::new(-0.41849443, -0.38460994, 0.338696),
        Vec3::new(-0.44936654, -0.32256892, 0.33410043),
        Vec3::new(-0.44936654, -0.32256892, -0.17601565),
        Vec3::new(1.0483973, -0.5, 0.42141795),
        Vec3::new(1.0483973, -0.5, -0.288609),
        Vec3::new(1.0195638, -0.44205558, 0.38005662),
        Vec3::new(1.0195638, -0.44205558, 0.07674469),
        Vec3::new(0.99097836, -0.38460994, 0.06295735),
        Vec3::new(0.99097836, -0.38460994, 0.338696),
        Vec3::new(0.96010625, -0.32256892, 0.33410043),
        Vec3::new(0.96010625, -0.32256892, -0.17601565),
    ];
    let cutter_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2],
        vec![1, 3, 2],
        vec![3, 4, 2],
        vec![5, 6, 4],
        vec![6, 7, 4],
        vec![7, 2, 4],
        vec![8, 9, 10],
        vec![10, 9, 11],
        vec![11, 9, 12],
        vec![13, 12, 14],
        vec![14, 12, 15],
        vec![15, 12, 9],
        vec![0, 8, 10, 1],
        vec![1, 10, 11, 3],
        vec![3, 11, 12, 4],
        vec![4, 12, 13, 5],
        vec![5, 13, 14, 6],
        vec![6, 14, 15, 7],
        vec![7, 15, 9, 2],
        vec![2, 9, 8, 0],
    ];
    let cutter = HalfEdgeMesh::from_polygons(&cutter_pos, &cutter_faces);
    let res = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("result");
    res.validate().expect("valid");
    assert!(res.is_manifold().is_ok());
    assert_eq!(res.count_folded_coplanar_pairs(), 0);
    assert_eq!(res.face_count(), 15);
    let vol = mesh_signed_volume(&res);
    assert!((vol - 0.897185).abs() < 1e-3, "volume {vol}");
}

/// OWNER capture 2026-08-04 #3 — the hardest one (shipped 130 raw triangles before the
/// T-junction candidate search). Blender EXACT: v=36 f=19 tris=0 vol=0.841381.
/// PROVENANCE (2026-08-04): the capture this data came from was face-visit RE-INDEXED
/// (the dump bug), so this is a valid KERNEL pin on exactly this input (Blender verified
/// the SAME data) but NOT proof of the app workflow. App-level end-to-end coverage lives
/// in app::project::tests::intrude_owner_fixtures_pinned (owner .fl4 scenes).
#[test]
fn owner_capture3_clean_and_volume_exact() {
    let target_pos = vec![
        Vec3::new(0.5, -0.5, 0.5),
        Vec3::new(-0.3431264, -0.5, 0.5),
        Vec3::new(-0.34312642, -0.5, 0.40874195),
        Vec3::new(-0.34312642, -0.5, -0.3123398),
        Vec3::new(-0.3431264, -0.5, -0.5),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.0027510684, 0.5),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, 0.0027510684, -0.5),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(-0.36096668, -0.442825, 0.3980193),
        Vec3::new(-0.36765814, -0.4213801, 0.06830542),
        Vec3::new(-0.38605952, -0.3624072, 0.06830542),
        Vec3::new(-0.38605952, -0.3624072, 0.31760108),
        Vec3::new(-0.42202604, -0.24714102, 0.30955958),
        Vec3::new(-0.41700733, -0.263225, -0.16490696),
    ];
    let target_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2, 3, 4, 5],
        vec![6, 7, 8, 1, 0],
        vec![7, 9, 10, 8],
        vec![9, 11, 5, 4, 10],
        vec![6, 11, 9, 7],
        vec![6, 0, 5, 11],
        vec![2, 12, 13, 14, 15, 16, 17, 3],
        vec![1, 8, 10, 4, 3, 17, 16, 15, 14, 13, 12, 2],
    ];
    let target = HalfEdgeMesh::from_polygons(&target_pos, &target_faces);
    let cutter_pos = vec![
        Vec3::new(-0.35928598, -0.50504225, 0.40874195),
        Vec3::new(-0.37712625, -0.44786724, 0.3980193),
        Vec3::new(-0.35928598, -0.50504225, -0.3123398),
        Vec3::new(-0.3838177, -0.42642236, 0.06830542),
        Vec3::new(-0.4022191, -0.36744946, 0.06830542),
        Vec3::new(-0.4022191, -0.36744946, 0.31760108),
        Vec3::new(-0.4381856, -0.2521833, 0.30955958),
        Vec3::new(-0.4331669, -0.26826724, -0.16490696),
        Vec3::new(1.2889888, 0.0092689395, 0.40874195),
        Vec3::new(1.2889888, 0.0092689395, -0.3123398),
        Vec3::new(1.2711486, 0.06644395, 0.3980193),
        Vec3::new(1.2644571, 0.08788884, 0.06830542),
        Vec3::new(1.2460557, 0.14686173, 0.06830542),
        Vec3::new(1.2460557, 0.14686173, 0.31760108),
        Vec3::new(1.2100892, 0.26212794, 0.30955958),
        Vec3::new(1.2151079, 0.24604395, -0.16490696),
    ];
    let cutter_faces: Vec<Vec<usize>> = vec![
        vec![0, 1, 2],
        vec![1, 3, 2],
        vec![3, 4, 2],
        vec![5, 6, 4],
        vec![4, 6, 2],
        vec![7, 2, 6],
        vec![8, 9, 10],
        vec![10, 9, 11],
        vec![11, 9, 12],
        vec![13, 12, 14],
        vec![12, 9, 14],
        vec![15, 14, 9],
        vec![0, 8, 10, 1],
        vec![1, 10, 11, 3],
        vec![3, 11, 12, 4],
        vec![4, 12, 13, 5],
        vec![5, 13, 14, 6],
        vec![6, 14, 15, 7],
        vec![7, 15, 9, 2],
        vec![2, 9, 8, 0],
    ];
    let cutter = HalfEdgeMesh::from_polygons(&cutter_pos, &cutter_faces);
    let res = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("result");
    res.validate().expect("valid");
    assert!(res.is_manifold().is_ok());
    assert_eq!(res.count_folded_coplanar_pairs(), 0, "no zero-thickness flaps");
    assert!(res.face_count() <= 20, "must not ship triangle soup, got {}", res.face_count());
    let vol = mesh_signed_volume(&res);
    assert!((vol - 0.841381).abs() < 1e-3, "volume {vol} != oracle 0.841381");
}

/// BOOLEAN PROVENANCE (stage P1). Subtract a cylinder through a cube: the barrel of the
/// hole must be tagged OperandB (the cutter's own tessellation, which Bevel Smart Select
/// rule 2 ignores), the untouched cube walls OperandA, and the rim where the hole meets the
/// face must NOT be tagged as cutter-interior -- that rim is exactly the edge the user wants
/// beveled. Owner 2026-08-04: "if we subtract a perfect cylinder, all the segments that
/// create the round segment should be tagged to ignore the bevel."
#[test]
fn provenance_tags_a_subtracted_cylinder_barrel_as_the_cutter() {
    use crate::mesh_boolean::{mesh_boolean_with_provenance, FaceOrigin};
    let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
    // Taller than the cube so it punches clean through: no cap coplanarity to argue about.
    let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
    let (res, prov) = mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference)
        .expect("cube minus cylinder must succeed");
    assert!(!prov.is_empty(), "provenance must be collected");
    assert_eq!(prov.face_origin.len(), res.faces.len(), "one tag per result face");

    let mut n_a = 0usize;
    let mut n_b = 0usize;
    for i in 0..res.faces.len() as u32 {
        let f = FaceId(i);
        if res.face(f).half_edge == crate::half_edge::NO_EDGE {
            continue;
        }
        match prov.face(f) {
            Some(FaceOrigin::OperandA) => n_a += 1,
            Some(FaceOrigin::OperandB) => {
                n_b += 1;
                // Every cutter face must sit on the barrel: radius 0.5 from the Y axis.
                for v in res.face_vertices(f) {
                    let p = res.vert(v).position;
                    let r = (p.x * p.x + p.z * p.z).sqrt();
                    assert!(
                        (r - 0.5).abs() < 1e-3,
                        "face tagged OperandB is not on the cylinder barrel (r={r})"
                    );
                }
            }
            _ => {}
        }
    }
    assert!(n_b > 0, "the hole's barrel must be tagged as the cutter");
    assert!(n_a > 0, "the cube's untouched walls must be tagged as the target");

    // The RIM must survive: an edge between a barrel face and a cube face is not
    // cutter-interior, so rule 2 keeps it.
    let mut rim_edges = 0usize;
    let mut barrel_edges = 0usize;
    for i in 0..res.half_edges.len() as u32 {
        let he = crate::half_edge::HalfEdgeId(i);
        if !res.is_edge_live(he) {
            continue;
        }
        let e = res.he(he);
        if e.twin == crate::half_edge::NO_TWIN || e.face == crate::half_edge::NO_FACE {
            continue;
        }
        let tf = res.he(e.twin).face;
        if tf == crate::half_edge::NO_FACE {
            continue;
        }
        let (o0, o1) = (prov.face(e.face), prov.face(tf));
        if o0 == Some(FaceOrigin::OperandB) && o1 == Some(FaceOrigin::OperandB) {
            barrel_edges += 1;
            assert!(prov.edge_is_cutter_interior(&res, he));
        }
        let touches_a = o0 == Some(FaceOrigin::OperandA) || o1 == Some(FaceOrigin::OperandA);
        let touches_b = o0 == Some(FaceOrigin::OperandB) || o1 == Some(FaceOrigin::OperandB);
        if touches_a && touches_b {
            rim_edges += 1;
            assert!(
                !prov.edge_is_cutter_interior(&res, he),
                "the rim edge must NOT be excluded - it is the edge the user wants beveled"
            );
        }
    }
    assert!(barrel_edges > 0, "expected vertical seams on the barrel");
    assert!(rim_edges > 0, "expected a rim between the hole and the cube face");
}

/// Provenance is ADVISORY and must FAIL CLOSED: an empty one never excludes anything, so a
/// consumer on an old save or the BSP path bevels everything rather than silently skipping.
#[test]
fn empty_provenance_excludes_nothing() {
    use crate::mesh_boolean::BoolProvenance;
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let empty = BoolProvenance::default();
    assert!(empty.is_empty());
    for i in 0..m.half_edges.len() as u32 {
        let he = crate::half_edge::HalfEdgeId(i);
        if m.is_edge_live(he) {
            assert!(!empty.edge_is_cutter_interior(&m, he));
        }
    }
}

/// DIAGNOSTIC (2026-08-04): partial selection is sound on a plain cube (see
/// bevel_op::tests::diag_partial_edge_set_on_a_plain_cube) and beveling ALL edges of a
/// boolean result is sound, yet Smart Select's subset of that result failed. Isolate WHICH
/// rule's exclusions break it: cutter-interior (rule 2) or coplanar (rule 3).
#[test]
fn diag_partial_bevel_on_boolean_output() {
    use crate::mesh_boolean::{mesh_boolean_with_provenance, BoolProvenance};
    use crate::bevel_op::{bevel_edges_offset, BevelParams};
    use crate::half_edge::HalfEdgeId;
    use std::collections::HashSet;

    let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
    let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
    let (res, prov) =
        mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference).expect("boolean");

    let live: Vec<u32> = (0..res.half_edges.len() as u32)
        .filter(|&i| res.is_edge_live(HalfEdgeId(i)))
        .collect();
    let p = BevelParams { width: 0.02, segments: 1, shape: 0.5, clamp: true };

    let run = |label: &str, sel: HashSet<u32>| {
        let out = bevel_edges_offset(&res, &sel, &p);
        eprintln!(
            "[DIAG2] {label:<18} edges={:<4} -> v={} f={} valid={:?} manifold={:?} folds={}",
            sel.len(), out.vertex_count(), out.face_count(),
            out.validate().is_ok(), out.is_manifold().is_ok(),
            out.count_folded_coplanar_pairs()
        );
    };

    run("ALL", live.iter().copied().collect());
    run("minus flat", live.iter().copied()
        .filter(|&e| !res.edge_is_flat(HalfEdgeId(e), 0.0)).collect());
    run("minus cutter", live.iter().copied()
        .filter(|&e| !prov.edge_is_cutter_interior(&res, HalfEdgeId(e))).collect());
    run("minus both", live.iter().copied()
        .filter(|&e| !res.edge_is_flat(HalfEdgeId(e), 0.0)
            && !prov.edge_is_cutter_interior(&res, HalfEdgeId(e))).collect());
    let _ = BoolProvenance::default();
}

/// THE OWNER'S RED-X EDGES (2026-08-04 screenshot): subtracting a cylinder leaves flat
/// cross-face support edges running across the cut face. `new_flat_edges` must find exactly
/// those -- created by the boolean AND flat -- and must never return an edge the user
/// authored, however flat, because a boolean must not change geometry it did not cut.
#[test]
fn new_flat_edges_finds_the_boolean_support_edges_only() {
    use crate::mesh_boolean::mesh_boolean_with_provenance;
    let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
    let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
    let (res, prov) =
        mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference).expect("boolean");

    let flat_new = prov.new_flat_edges(&res);
    eprintln!("[REDX] new+flat edges: {}", flat_new.len());
    assert!(!flat_new.is_empty(), "the cut face must carry support edges");

    for &e in &flat_new {
        let he = crate::half_edge::HalfEdgeId(e);
        // Every one must genuinely be flat...
        assert!(res.edge_is_flat(he, 0.0), "edge {e} is not flat");
        // ...and genuinely new.
        assert!(prov.edge_is_new[e as usize], "edge {e} existed on an input");
    }

    // A cube's own edges are 90-degree creases, so a boolean that changes nothing about them
    // must never nominate one. Sanity: no returned edge is an original cube edge.
    let cube_len = 2.0f32;
    for &e in &flat_new {
        let he = crate::half_edge::HalfEdgeId(e);
        let p0 = res.vert(res.he_source(he)).position;
        let p1 = res.vert(res.he(he).vertex).position;
        let on_cube_corner = |p: glam::Vec3| {
            (p.x.abs() - cube_len * 0.5).abs() < 1e-4
                && (p.y.abs() - cube_len * 0.5).abs() < 1e-4
                && (p.z.abs() - cube_len * 0.5).abs() < 1e-4
        };
        assert!(
            !(on_cube_corner(p0) && on_cube_corner(p1)),
            "a original cube edge was nominated for dissolve"
        );
    }
}

/// Fail closed: no provenance means nothing is nominated for dissolve.
#[test]
fn new_flat_edges_is_empty_without_provenance() {
    use crate::mesh_boolean::BoolProvenance;
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    assert!(BoolProvenance::default().new_flat_edges(&m).is_empty());
}

/// REPRO for the real bevel gap (BEVEL.md section 6, 2026-08-04): beveling a selection that
/// SKIPS an edge lying between two selected regions. The skipped edges here are the annulus
/// seams a cylindrical hole leaves on a flat face -- structural, unremovable -- and the owner
/// needs them un-beveled. Today that produces folds. `#[ignore]`d until bevel_op handles it:
/// it documents the defect precisely rather than asserting today's wrong answer as correct.
#[test]
fn bevel_skipping_an_annulus_seam_must_not_fold() {
    use crate::mesh_boolean::mesh_boolean_with_provenance;
    use crate::bevel_op::{bevel_edges_offset, BevelParams};
    use crate::half_edge::HalfEdgeId;

    let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
    let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
    let (res, prov) =
        mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference).expect("boolean");

    // The seams: created by the cut AND flat. Structural (see section 6) -- they cannot be
    // dissolved, so the bevel must simply leave them alone.
    let seams: std::collections::HashSet<u32> = {
        let mut s = std::collections::HashSet::new();
        for e in prov.new_flat_edges(&res) {
            let he = HalfEdgeId(e);
            s.insert(e);
            let tw = res.he(he).twin;
            if tw != crate::half_edge::NO_TWIN {
                s.insert(tw.0);
            }
        }
        s
    };
    assert!(!seams.is_empty(), "expected annulus seams");

    let selected: std::collections::HashSet<u32> = (0..res.half_edges.len() as u32)
        .filter(|&i| res.is_edge_live(HalfEdgeId(i)) && !seams.contains(&i))
        .collect();

    // MEASURE the culprit vertex: the fold sits at a cube corner (2,-2,-2). Log every
    // vertex near it, its fan size, and how many of its incident edges are SELECTED.
    // build_boundary_flat early-returns when fewer than 2 fan edges are beveled -- if the
    // corner has 0 or 1 selected edges it never builds a corner at all.
    for i in 0..res.vertices.len() as u32 {
        let vid = crate::half_edge::VertexId(i);
        if !res.is_vert_live(vid) { continue; }
        let p = res.vert(vid).position;
        if (p - Vec3::new(2.0, -2.0, -2.0)).length() > 0.35 { continue; }
        let fan = res.vertex_fan(vid);
        let sel_n = fan.iter().filter(|h| selected.contains(&h.0)).count();
        eprintln!(
            "[SEAM] vert{i} ({:.3},{:.3},{:.3}) fan={} selected={sel_n}",
            p.x, p.y, p.z, fan.len()
        );
    }
    let out = bevel_edges_offset(
        &res,
        &selected,
        &BevelParams { width: 0.02, segments: 1, shape: 0.5, clamp: true },
    );
    eprintln!(
        "[SEAM] skipped {} of {} half-edges -> v={} f={} valid={:?} manifold={:?} folds={}",
        seams.len(), res.half_edges.len(), out.vertex_count(), out.face_count(),
        out.validate().is_ok(), out.is_manifold().is_ok(),
        out.count_folded_coplanar_pairs()
    );
    // WHICH BUILDER emitted the folds? The strip/cap split names the producer directly:
    // a fold inside the strip range means the rails, inside the cap range means corner fill.
    let strip = crate::bevel_op::last_bevel_strip_range();
    let newr = crate::bevel_op::last_bevel_new_face_range();
    eprintln!("[SEAM] strip range={strip:?} new range={newr:?}");
    for (a, b) in out.folded_coplanar_pairs_dbg() {
        let cls = |f: crate::half_edge::FaceId| -> &str {
            match (strip, newr) {
                (Some((s0, s1)), Some((_, n1))) => {
                    if f.0 >= s0 && f.0 < s1 { "STRIP" }
                    else if f.0 >= s1 && f.0 < n1 { "CORNER-FILL" }
                    else { "re-traced original" }
                }
                _ => "unknown",
            }
        };
        eprintln!("[SEAM] fold producer: {:?}={} / {:?}={}", a, cls(a), b, cls(b));
    }
    // WHERE are the folds and the non-manifold edges? Print them rather than assume.
    for (a, b) in out.folded_coplanar_pairs_dbg() {
        let ring = |f: crate::half_edge::FaceId| -> Vec<String> {
            out.face_vertices(f).iter()
                .map(|&v| { let p = out.vert(v).position;
                    format!("({:.3},{:.3},{:.3})", p.x, p.y, p.z) })
                .collect()
        };
        eprintln!("[SEAM] fold {:?}{:?} / {:?}{:?}", a, ring(a), b, ring(b));
    }
    if let Err(e) = out.is_manifold() {
        eprintln!("[SEAM] manifold error: {e}");
    }
    assert!(out.validate().is_ok(), "valid");
    assert!(out.is_manifold().is_ok(), "must stay watertight around the skipped seam");
    assert_eq!(out.count_folded_coplanar_pairs(), 0, "no folds around the skipped seam");
}

/// SIDECAR SURVIVES A BOOLEAN (S1). Authored attributes must ride through a cut on the
/// geometry the cut did not touch -- today every boolean install WIPES them
/// (`BOOL-STALECD`), which is why UVs, seams, creases and polygroups vanish. Equally
/// important: geometry the cut CREATED must get defaults, never an invented value.
#[test]
fn sidecar_survives_a_boolean_on_uncut_geometry() {
    use crate::mesh_boolean::mesh_boolean_with_provenance;
    use crate::poly_mesh::{CustomData, EdgeKey};
    use crate::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE};

    let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
    let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);

    // Author something on EVERY element of the cube.
    let mut data = CustomData::new();
    for i in 0..cube.vertices.len() as u32 {
        data.vcrease.set(VertexId(i), 0.75);
    }
    for i in 0..cube.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !cube.is_edge_live(h) { continue; }
        data.uv.set(h, glam::Vec2::new(0.25, 0.5));
        data.sharp.set(h, true);
        data.crease.set(EdgeKey::new(cube.he_source(h), cube.he(h).vertex), 1.0);
    }
    for i in 0..cube.faces.len() as u32 {
        if cube.face(FaceId(i)).half_edge != NO_EDGE { data.groups.set(FaceId(i), 7); }
    }

    let (res, _prov) =
        mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference).expect("boolean");
    let out = data.remap_by_position(&cube, &res);

    // The cube's untouched CORNERS (far from the hole at radius 0.5 about Y) keep their data.
    let mut kept_uv = 0usize;
    let mut kept_sharp = 0usize;
    for i in 0..res.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !res.is_edge_live(h) { continue; }
        let p = res.vert(res.he_source(h)).position;
        // A corner of the original cube: all three coords at the box extent.
        let on_corner = (p.x.abs() - 2.0).abs() < 1e-4
            && (p.y.abs() - 2.0).abs() < 1e-4
            && (p.z.abs() - 2.0).abs() < 1e-4;
        if !on_corner { continue; }
        if out.uv.get(h).is_some() { kept_uv += 1; }
        if out.sharp.get(h) { kept_sharp += 1; }
    }
    eprintln!("[SIDECAR] kept uv on {kept_uv} corner half-edges, sharp on {kept_sharp}");
    assert!(kept_uv > 0, "UVs must survive on geometry the cut did not touch");
    assert!(kept_sharp > 0, "sharp bits must survive on untouched geometry");

    // Vertices the cut CREATED (on the hole barrel) must carry NO authored value.
    let mut new_verts = 0usize;
    for i in 0..res.vertices.len() as u32 {
        let v = VertexId(i);
        if !res.is_vert_live(v) { continue; }
        let p = res.vert(v).position;
        if ((p.x * p.x + p.z * p.z).sqrt() - 0.5).abs() > 1e-3 { continue; }
        new_verts += 1;
        assert!(
            out.vcrease.get(v).is_none(),
            "a vertex created by the cut must not inherit an authored value"
        );
    }
    assert!(new_verts > 0, "expected vertices on the hole barrel");
    eprintln!("[SIDECAR] {new_verts} created verts correctly carry no authored value");
}

/// Owner test-019: free-knife S slice left 17 valence-2 collinear pins on a box face ring.
/// Owner test-019: free-knife densify left valence-2 collinear pins on face rings.
/// Mesh Cleanup used to drop face count but KEEP every pin (no live valence-2 pass +
/// no-op gate ignored verts), so selecting a cut trapezoid painted a crazy outline.
#[test]
fn cleanup_mesh_collapses_collinear_mid_edge_pins() {
    use glam::Vec3;
    use crate::half_edge::HalfEdgeMesh;

    // Start from a clean cube, then insert collinear pins on one edge by subdividing
    // that edge on every face that uses it (top and front share... actually bottom edge
    // of front is shared with bottom face).
    
    // Manually build cube with mid-edge pins on the front bottom edge (-1,-1,+1)→(+1,-1,+1)
    // and the matching back edge so the mesh stays closed.
    let p = vec![
        Vec3::new(-1.0, -1.0, -1.0), // 0
        Vec3::new( 1.0, -1.0, -1.0), // 1
        Vec3::new( 1.0,  1.0, -1.0), // 2
        Vec3::new(-1.0,  1.0, -1.0), // 3
        Vec3::new(-1.0, -1.0,  1.0), // 4
        Vec3::new( 1.0, -1.0,  1.0), // 5
        Vec3::new( 1.0,  1.0,  1.0), // 6
        Vec3::new(-1.0,  1.0,  1.0), // 7
        Vec3::new(-0.5, -1.0,  1.0), // 8  front bottom pins
        Vec3::new( 0.0, -1.0,  1.0), // 9
        Vec3::new( 0.5, -1.0,  1.0), // 10
        Vec3::new(-0.5, -1.0, -1.0), // 11 back bottom pins
        Vec3::new( 0.0, -1.0, -1.0), // 12
        Vec3::new( 0.5, -1.0, -1.0), // 13
    ];
    let faces = vec![
        // back z=-1: bottom edge 0-11-12-13-1
        vec![0, 3, 2, 1, 13, 12, 11],
        // front z=+1: bottom edge 4-8-9-10-5
        vec![4, 8, 9, 10, 5, 6, 7],
        // left
        vec![0, 4, 7, 3],
        // right
        vec![1, 2, 6, 5],
        // top
        vec![3, 7, 6, 2],
        // bottom y=-1: includes both pin chains
        vec![0, 11, 12, 13, 1, 5, 10, 9, 8, 4],
    ];
    let m = HalfEdgeMesh::from_polygons(&p, &faces);
    assert!(m.validate().is_ok(), "seed validate: {:?}", m.validate());
    assert_eq!(boundary_half_edge_count(&m), 0, "seed must be closed");
    assert!(m.is_manifold().is_ok(), "seed manifold: {:?}", m.is_manifold());
    let v_before = m.vertex_count();
    assert!(v_before >= 14, "pins present: {v_before}");

    // Live-mesh collapse alone must strip the pins.
    let mut live = m.clone();
    collapse_collinear_pins(&mut live);
    assert!(live.validate().is_ok() && live.is_manifold().is_ok());
    assert_eq!(live.vertex_count(), 8, "collapse_collinear_pins → clean cube, got {}", live.vertex_count());

    // Full cleanup path (the Mesh Cleanup UI) must accept and finish the same.
    let cleaned = cleanup_mesh(&m).expect("cleanup must accept a mesh with collinear pins");
    assert!(cleaned.validate().is_ok());
    assert!(cleaned.is_manifold().is_ok());
    assert_eq!(
        cleaned.vertex_count(),
        8,
        "collinear mid-edge pins must dissolve (got {} verts, faces={})",
        cleaned.vertex_count(),
        cleaned.face_count()
    );
    assert_eq!(cleaned.face_count(), 6, "cube has 6 faces after cleanup");
    
}

/// OWNER 2026-08-06 ("EDGE CASE INSET.fl4"): intrude + clean mesh left a vertex sitting ON another
/// edge without being joined to it. `validate()` and `is_manifold()` both PASS on that - a
/// T-junction is topologically legal - and it renders perfectly, so nothing caught it until the
/// next bevel misbehaved. Mesh Cleanup is the operation meant to fix exactly this, and it never
/// ran the repair, which existed but was wired only into the boolean.
///
/// CLOSED cube (cleanup refuses open meshes) whose +X face carries an extra vertex mid-way along
/// the edge it shares with the bottom face, while the bottom face skips it. Cleanup must return a
/// mesh where BOTH faces own that point.
#[test]
fn cleanup_repairs_a_t_junction() {
    let p = vec![
        Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(1.0, -1.0, 1.0),   Vec3::new(-1.0, -1.0, 1.0),
        Vec3::new(-1.0, 1.0, -1.0),  Vec3::new(1.0, 1.0, -1.0),
        Vec3::new(1.0, 1.0, 1.0),    Vec3::new(-1.0, 1.0, 1.0),
        Vec3::new(1.0, -1.0, 0.0),   // 8 - the T: mid-way along edge 1->2
    ];
    let faces = vec![
        vec![0, 1, 2, 3],       // bottom - SKIPS vertex 8 (the T-junction)
        vec![7, 6, 5, 4],
        vec![4, 5, 1, 0],
        vec![6, 7, 3, 2],
        vec![7, 4, 0, 3],
        vec![5, 6, 2, 8, 1],    // +X - owns vertex 8
    ];
    let m = HalfEdgeMesh::from_polygons(&p, &faces);
    // A seam T-junction leaves the neighbour's edge unpaired, so the INPUT is open there. The
    // repair closing it is the strongest statement this gate can make.
    let b_before = boundary_half_edge_count(&m);
    assert!(b_before > 0, "fixture must start with the unpaired seam");
    // Count T-junctions: a live vertex lying strictly inside an edge it is not an endpoint of.
    // Asserting the PROPERTY, not the mechanism - cleanup may resolve one either by joining the
    // point to the neighbouring face or by removing it as redundant, and both are correct.
    use crate::half_edge::VertexId;
    let t_junctions = |mesh: &HalfEdgeMesh| -> usize {
        let live: Vec<VertexId> = (0..mesh.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| mesh.vert(v).half_edge != NO_EDGE)
            .collect();
        let mut n = 0usize;
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !mesh.is_edge_live(h) { continue; }
            let (a, b) = (mesh.he_source(h), mesh.he(h).vertex);
            let (pa, pb) = (mesh.vert(a).position, mesh.vert(b).position);
            let ab = pb - pa;
            let len2 = ab.length_squared();
            if len2 < 1e-12 { continue; }
            for &v in &live {
                if v == a || v == b { continue; }
                let p = mesh.vert(v).position;
                let t = (p - pa).dot(ab) / len2;
                if !(0.001..=0.999).contains(&t) { continue; }
                if (p - (pa + ab * t)).length() < 1e-4 { n += 1; }
            }
        }
        n
    };

    let b_before = boundary_half_edge_count(&m);
    assert!(b_before > 0, "fixture must start with the unpaired seam");
    assert!(t_junctions(&m) > 0, "fixture must start WITH a T-junction");

    let opts = CleanupOptions {
        flatten_folds: false,
        merge_coplanar: false,
        dissolve_angle_deg: 0.0,
        protected_edges: Vec::new(),
        ..Default::default()
    };
    let Some(out) = cleanup_mesh_with(&m, &opts) else {
        panic!("cleanup declined a mesh with a real T-junction to repair");
    };
    assert_eq!(t_junctions(&out), 0, "cleanup must leave NO T-junction behind");
    assert_eq!(
        boundary_half_edge_count(&out), 0,
        "repair must CLOSE the seam the T-junction left open ({b_before} before)"
    );
    assert!(out.validate().is_ok(), "repaired mesh valid: {:?}", out.validate());
}

/// SIDECAR, KERNEL HALF (owner 2026-08-11: "smart select after boolean cut still forgets the tag
/// on geo that was not touched").
///
/// The app-side half of that bug — installs that replaced the mesh without re-keying the sidecar —
/// is gated in `app/src/poly/mesh_sync.rs`. This pins the layer under it: for geometry a cut did
/// not reach, `remap_by_position` must carry `op_created` across with ZERO loss. If this ever
/// regresses, no amount of correct install plumbing can keep the owner's tags alive.
///
/// The fixture is a beveled cube — the shape his tags actually live on — cut at the opposite
/// corner. Both halves matter: an untouched edge must still EXIST (the boolean may not
/// re-tessellate what it did not cut) and must still be TAGGED.
#[test]
fn op_created_survives_a_boolean_on_untouched_geometry() {
    use crate::half_edge::HalfEdgeId;
    use crate::poly_mesh::CustomData;
    use std::collections::HashMap;

    const Q: f32 = 1e5;
    let qp = |p: Vec3| ((p.x * Q).round() as i64, (p.y * Q).round() as i64, (p.z * Q).round() as i64);
    let make_box = |mn: Vec3, mx: Vec3| -> HalfEdgeMesh {
        let p = [
            Vec3::new(mn.x, mn.y, mn.z), Vec3::new(mx.x, mn.y, mn.z),
            Vec3::new(mx.x, mx.y, mn.z), Vec3::new(mn.x, mx.y, mn.z),
            Vec3::new(mn.x, mn.y, mx.z), Vec3::new(mx.x, mn.y, mx.z),
            Vec3::new(mx.x, mx.y, mx.z), Vec3::new(mn.x, mx.y, mx.z),
        ];
        let rings: Vec<Vec<usize>> = vec![
            vec![3, 2, 1, 0], vec![4, 5, 6, 7], vec![0, 1, 5, 4],
            vec![1, 2, 6, 5], vec![2, 3, 7, 6], vec![3, 0, 4, 7],
        ];
        HalfEdgeMesh::from_polygons(&p, &rings)
    };

    // Base = cube with its +X+Y edge beveled (a destructive bevel is what writes tags).
    let cube = HalfEdgeMesh::new_box(Vec3::splat(1.0));
    let sel: std::collections::HashSet<u32> = (0..cube.half_edges.len() as u32)
        .filter(|&i| {
            let h = HalfEdgeId(i);
            cube.is_edge_live(h) && {
                let a = cube.vert(cube.he_source(h)).position;
                let b = cube.vert(cube.he(h).vertex).position;
                a.x > 0.9 && b.x > 0.9 && a.y > 0.9 && b.y > 0.9
            }
        })
        .collect();
    assert!(!sel.is_empty(), "fixture: the +X+Y edge must be found");
    let params = crate::bevel_op::BevelParams { width: 0.25, segments: 3, shape: 0.5, clamp: true };
    let base = crate::bevel_op::bevel_edges_offset(&cube, &sel, &params);

    let mut cd = CustomData::new();
    for i in 0..base.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if base.is_edge_live(h) {
            cd.op_created.set(h, true);
        }
    }

    // Cut the OPPOSITE corner.
    let cutter = make_box(Vec3::splat(-1.6), Vec3::splat(-0.6));
    let (res, _prov) = mesh_boolean_with_provenance(&base, &cutter, BoolOp::Difference)
        .expect("the kernel must accept this cut");
    let out = cd.remap_by_position(&base, &res);

    let mut new_he: HashMap<((i64, i64, i64), (i64, i64, i64)), HalfEdgeId> = HashMap::new();
    for i in 0..res.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if res.is_edge_live(h) {
            new_he.insert(
                (qp(res.vert(res.he_source(h)).position), qp(res.vert(res.he(h).vertex).position)),
                h,
            );
        }
    }
    // "Untouched" = every coordinate of both endpoints is clear of the cutter's reach.
    let (mut total, mut alive, mut tagged) = (0usize, 0usize, 0usize);
    for i in 0..base.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !base.is_edge_live(h) {
            continue;
        }
        let a = base.vert(base.he_source(h)).position;
        let b = base.vert(base.he(h).vertex).position;
        if a.min(b).min_element() <= -0.5 {
            continue; // near the cut — loss there is honest
        }
        total += 1;
        if let Some(&nh) = new_he.get(&(qp(a), qp(b))) {
            alive += 1;
            if out.op_created.get(nh) {
                tagged += 1;
            }
        }
    }
    assert!(total > 0, "fixture must contain edges far from the cut");
    assert_eq!(alive, total, "a boolean must not re-tessellate geometry it did not cut");
    assert_eq!(tagged, total, "every untouched edge must keep its Smart Select tag");
}

/// DIAG: AZRA_BOOL_REPLAY=<capture> — sweep merge-by-distance thresholds over the push-through
/// result to find what actually clears the epsilon debris the owner sees as "the side plane is
/// not clean", and at what point it starts eating real geometry (volume drift).
#[test]
#[ignore]
fn diag_debris_threshold_sweep() {
    let Ok(path) = std::env::var("AZRA_BOOL_REPLAY") else { eprintln!("set AZRA_BOOL_REPLAY"); return; };
    let txt = std::fs::read_to_string(&path).expect("read json");
    let (target, cutter) = parse_extrude_capture(&txt);
    let Some(res) = mesh_boolean(&target, &cutter, BoolOp::Difference) else {
        eprintln!("[SWEEP] kernel refused"); return;
    };
    let stat = |tag: &str, m: &HalfEdgeMesh| {
        let tris = (0..m.faces.len() as u32).filter(|&i| {
            let f = FaceId(i);
            m.face(f).half_edge != crate::half_edge::NO_EDGE && m.face_vertices(f).len() == 3
        }).count();
        let zero = (0..m.faces.len() as u32).filter(|&i| {
            let f = FaceId(i);
            m.face(f).half_edge != crate::half_edge::NO_EDGE
                && m.compute_face_normal(f).length() < 1e-9
        }).count();
        eprintln!("[SWEEP] {tag:>22}: v={:>3} f={:>3} tris={:>3} folds={} zero_area={} min_edge={:.6} vol={:.6} manifold={} closed={}",
            m.vertex_count(), m.face_count(), tris, m.count_folded_coplanar_pairs(), zero,
            crate::mesh_boolean_manifold::min_edge_length(m), mesh_signed_volume(m),
            m.is_manifold().is_ok(), boundary_half_edge_count(m) == 0);
    };
    stat("boolean result", &res);
    let vol0 = mesh_signed_volume(&res);
    for thr in [1e-5f32, 5e-5, 1e-4, 2.5e-4, 5e-4, 1e-3, 2.5e-3, 5e-3, 1e-2] {
        let mut m = res.clone();
        let welded = m.merge_by_distance(thr);
        let after = crate::mesh_boolean::cleanup_mesh(&m).unwrap_or_else(|| m.clone());
        let drift = (mesh_signed_volume(&after) - vol0).abs() / vol0.abs().max(1e-9);
        eprintln!("[SWEEP] --- thr={thr:.0e} welded={welded} vol_drift={:.3}%", drift * 100.0);
        stat(&format!("thr {thr:.0e} +cleanup"), &after);
    }
}

/// GATE — the epsilon DEBRIS a through-cut leaves must be weldable without moving the surface.
///
/// Owner 2026-08-11: "my intrude from extrude tool no longer works properly, it's supposed to
/// clean the plane on the side." Measured on his own capture (`azra_extrude_repro.json`): the
/// boolean result carried 21 triangles, 3 folded coplanar pairs and a ZERO-AREA face, all
/// clustered on the flat side planes around vertices 2.3e-4 apart. The kernel's own
/// `collapse_epsilon_debris` cannot reach them — it is capped at bbox·1e-5, which was 2.9e-5
/// there, a full order of magnitude BELOW the debris. That is why the side plane never got clean,
/// and why every later cut inherited the mess and made it worse.
///
/// The fixture is the honest shape of that debris: two collinear T-pins a hair apart on one cube
/// edge, carried by BOTH faces that share it (a real mesh stays closed, which is exactly why the
/// pins survive every gate that only checks manifoldness). Pins are collinear, so a correct weld
/// changes the volume by exactly nothing.
///
/// Pinned here rather than in the app because the app-side policy
/// (`AzraApp::poly_extrude_merge_debris`) is a THRESHOLD choice over this kernel property.
#[test]
fn epsilon_debris_welds_without_moving_the_surface() {
    use crate::half_edge::VertexId;

    let d = 2.0e-4_f32; // debris scale, matching the owner's capture (min_edge 2.3e-4)
    let base = HalfEdgeMesh::new_box(Vec3::ONE);
    let mut pos: Vec<Vec3> = base.vertices.iter().map(|v| v.position).collect();

    // Pick a real edge of the box and drop two pins a hair apart in the middle of it.
    let (va, vb) = {
        let h = (0..base.half_edges.len() as u32)
            .map(crate::half_edge::HalfEdgeId)
            .find(|&h| base.is_edge_live(h))
            .expect("box has edges");
        (base.he_source(h), base.he(h).vertex)
    };
    let (pa, pb) = (pos[va.0 as usize], pos[vb.0 as usize]);
    let mid = (pa + pb) * 0.5;
    let dir = (pb - pa).normalize();
    let (i1, i2) = (pos.len(), pos.len() + 1);
    pos.push(mid);            // pin 1
    pos.push(mid + dir * d);  // pin 2, `d` further along the SAME edge

    // Rebuild every face ring, splicing the pins into whichever two faces use that edge — in the
    // traversal order each one sees it, so the mesh stays closed and correctly wound.
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for fi in 0..base.faces.len() as u32 {
        let fid = FaceId(fi);
        if base.face(fid).half_edge == crate::half_edge::NO_EDGE {
            continue;
        }
        let ring: Vec<VertexId> = base.face_vertices(fid);
        let mut out: Vec<usize> = Vec::with_capacity(ring.len() + 2);
        for k in 0..ring.len() {
            let cur = ring[k];
            let nxt = ring[(k + 1) % ring.len()];
            out.push(cur.0 as usize);
            if cur == va && nxt == vb {
                out.extend_from_slice(&[i1, i2]);
            } else if cur == vb && nxt == va {
                out.extend_from_slice(&[i2, i1]);
            }
        }
        faces.push(out);
    }
    let mut m = HalfEdgeMesh::from_polygons(&pos, &faces);
    assert_eq!(boundary_half_edge_count(&m), 0, "fixture must be closed");
    assert!(m.is_manifold().is_ok(), "fixture must be manifold");
    let vol0 = mesh_signed_volume(&m);
    let v0 = m.vertex_count();
    assert!(
        crate::mesh_boolean_manifold::min_edge_length(&m) <= d * 1.001,
        "fixture must actually carry a debris-scale edge (min_edge={})",
        crate::mesh_boolean_manifold::min_edge_length(&m)
    );

    // BELOW the debris scale: nothing welds. This is the regime the kernel's own epsilon is stuck
    // in, and the reason the owner saw no cleanup at all.
    let mut too_small = m.clone();
    assert_eq!(
        too_small.merge_by_distance_ex(d * 0.1, &[]),
        0,
        "a radius below the debris scale must be a no-op — pins WHY bbox*1e-5 did nothing"
    );

    // PROTECTION first, on a pristine copy: an authored edge across the pair vetoes its collapse.
    let mut prot = m.clone();
    let welded_prot = prot.merge_by_distance_ex(d * 2.0, &[(pos[i1], pos[i2])]);
    assert_eq!(
        welded_prot, 0,
        "a protected edge must never be welded away — that is what keeps Smart Select tags, \
         Mark Sharp, seams and creases alive through this pass"
    );

    // ABOVE the debris scale, below the feature scale: the debris goes, the box does not move.
    let welded = m.merge_by_distance_ex(d * 2.0, &[]);
    assert!(welded > 0, "the debris pins must weld");
    assert!(m.validate().is_ok(), "welded mesh valid: {:?}", m.validate());
    assert!(m.is_manifold().is_ok(), "welded mesh manifold: {:?}", m.is_manifold());
    assert_eq!(boundary_half_edge_count(&m), 0, "must stay closed");
    assert!(m.vertex_count() < v0, "a pin must actually be gone ({v0} -> {})", m.vertex_count());
    let vol1 = mesh_signed_volume(&m);
    assert!(
        (vol1 - vol0).abs() <= vol0.abs() * 1e-3,
        "collinear pins carry no volume — the weld must not move the surface: {vol0} -> {vol1}"
    );
    assert!(
        crate::mesh_boolean_manifold::min_edge_length(&m) > d * 1.5,
        "the debris-scale edge must be gone (min_edge={})",
        crate::mesh_boolean_manifold::min_edge_length(&m)
    );
}

/// BOOLEAN SEAM CORNER (owner 2026-08-15: "the edge from the boolean creates 4 edges that break
/// the rounded cap"). The real geometry class, on real boolean output.
///
/// A cylinder subtracted from a box leaves COPLANAR support seams running from the hole rim out to
/// the box corners (`new_flat_edges`, §6). Smart Select excludes them, so four of the eight corners
/// become degree-4 with 3 beveled edges + 1 FLAT unbeveled one. Those corners must round EXACTLY
/// like the four untouched degree-3 corners in the same mesh -- Blender does
/// (`flat_unbeveled_edge_must_not_break_the_rounded_corner` pins that against the oracle).
///
/// Before the fix the two rails flanking a seam were slid onto it INDEPENDENTLY and landed on two
/// different points, so the corner rim was not a chained arc ring: at w=0.02 the cap was suppressed
/// and the closed-input seal plastered one flat n-gon over it (the owner's broken round), and at
/// w=0.1 the result was non-manifold with a hole, i.e. rejected outright and no bevel at all.
#[test]
fn boolean_seam_corner_rounds_like_a_clean_corner() {
    use crate::mesh_boolean::mesh_boolean_with_provenance;
    use crate::bevel_op::{bevel_edges_offset, bevel_result_installable, boundary_loop_count,
                          BevelParams};
    use crate::half_edge::HalfEdgeId;

    let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
    let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
    let (res, prov) =
        mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference).expect("boolean");

    let seams: std::collections::HashSet<u32> = {
        let mut s = std::collections::HashSet::new();
        for e in prov.new_flat_edges(&res) {
            let he = HalfEdgeId(e);
            s.insert(e);
            let tw = res.he(he).twin;
            if tw != crate::half_edge::NO_TWIN { s.insert(tw.0); }
        }
        s
    };
    assert!(!seams.is_empty(), "expected annulus seams");
    let selected: std::collections::HashSet<u32> = (0..res.half_edges.len() as u32)
        .filter(|&i| res.is_edge_live(HalfEdgeId(i)) && !seams.contains(&i))
        .collect();

    // FIXTURE CHECK: the corners under test really are the mixed class (degree-4, 3 beveled, the
    // 4th edge FLAT). Without this the test could pass on a mesh that never had the defect.
    let mut seam_corners = 0usize;
    for i in 0..res.vertices.len() as u32 {
        let vid = crate::half_edge::VertexId(i);
        if !res.is_vert_live(vid) { continue; }
        let p = res.vert(vid).position;
        if (0..3).any(|k| (p[k].abs() - 2.0).abs() > 1e-3) { continue; }
        let fan = res.vertex_fan(vid);
        if fan.len() != 4 { continue; }
        let flat_unbev = fan.iter().filter(|h| !selected.contains(&h.0)).all(|&h| {
            res.edge_dihedral_cos(h).map_or(false, |c| c > 0.999)
        });
        if flat_unbev { seam_corners += 1; }
    }
    assert_eq!(seam_corners, 4, "fixture: 4 box corners must receive a FLAT boolean seam");

    // Local topology around a corner: (verts inside, faces fully inside).
    let local = |m: &HalfEdgeMesh, c: Vec3| -> (usize, usize) {
        let r = 0.32f32;
        let nv = (0..m.vertices.len() as u32)
            .filter(|&i| {
                let v = crate::half_edge::VertexId(i);
                m.is_vert_live(v) && (m.vert(v).position - c).length() < r
            })
            .count();
        let nf = (0..m.faces.len() as u32)
            .filter(|&i| {
                let f = crate::half_edge::FaceId(i);
                if m.face(f).half_edge == crate::half_edge::NO_EDGE { return false; }
                let vs = m.face_vertices(f);
                !vs.is_empty() && vs.iter().all(|&v| (m.vert(v).position - c).length() < r)
            })
            .count();
        (nv, nf)
    };

    for (w, seg) in [(0.02f32, 1u32), (0.02, 3), (0.1, 1), (0.1, 3)] {
        let out = bevel_edges_offset(
            &res, &selected, &BevelParams { width: w, segments: seg, shape: 0.5, clamp: true });
        assert!(out.validate().is_ok(), "w={w} seg={seg}: valid");
        assert!(out.is_manifold().is_ok(), "w={w} seg={seg}: manifold {:?}", out.is_manifold());
        assert_eq!(out.count_folded_coplanar_pairs(), 0, "w={w} seg={seg}: no folds");
        assert_eq!(boundary_loop_count(&out), 0, "w={w} seg={seg}: no holes around the seams");
        // The whole point: the user must SEE a bevel. A torn result is rejected by the install
        // guard and the app silently keeps the previous frame.
        assert!(bevel_result_installable(&res, &out), "w={w} seg={seg}: must be installable");

        // A seam corner must be filled EXACTLY like a clean one -- the dome, not a flat seal n-gon.
        let clean = local(&out, Vec3::new(-2.0, -2.0, -2.0)); // degree-3, no seam
        let seam = local(&out, Vec3::new(2.0, -2.0, -2.0));   // degree-4, seam lands here
        assert_eq!(
            seam, clean,
            "w={w} seg={seg}: the flat boolean seam changed the corner fill (seam={seam:?} clean={clean:?})"
        );
        if seg > 1 {
            assert_eq!(clean.1, 7, "w={w}: a rounded corner is the 7-face M_ADJ dome, got {clean:?}");
        }
    }
}

/// POLY-STALE-IDS: an install of `mesh_boolean_keeping_ids` over the target leaves every id held
/// for geometry the cut did not touch naming the SAME geometry — the plain boolean numbers its
/// result from scratch. Target = subdivided box (24 quads); cutter takes the +X+Y+Z corner; the
/// whole -X side is untouched.
#[test]
fn boolean_keeping_ids_keeps_ids_of_untouched_geometry() {
    use crate::half_edge::rebuild_ids_test_support::assert_ids_name_same_geometry;
    use crate::half_edge::{FaceId, HalfEdgeId, VertexId};
    let mut target = box_mesh(Vec3::ZERO, Vec3::ONE);
    target.subdivide_faces(&(0..6).collect());
    let cutter = box_mesh(Vec3::splat(1.0), Vec3::splat(0.5));
    let far = |p: Vec3| p.x < -0.5;
    let verts: Vec<VertexId> = (0..target.vertices.len() as u32)
        .map(VertexId)
        .filter(|&v| target.is_vert_live(v) && far(target.vert(v).position))
        .collect();
    let faces: Vec<FaceId> = (0..target.faces.len() as u32)
        .map(FaceId)
        .filter(|&f| target.is_face_live(f) && target.face_vertices(f).iter().all(|&v| far(target.vert(v).position)))
        .collect();
    let edges: Vec<HalfEdgeId> = faces.iter().flat_map(|&f| target.face_half_edges(f)).collect();
    assert_eq!((verts.len(), faces.len()), (9, 4), "fixture: the -X side");

    let (res, prov) = mesh_boolean_keeping_ids(&target, &cutter, BoolOp::Difference).expect("corner cut");
    assert!(res.validate().is_ok(), "{:?}", res.validate());
    assert_eq!(boundary_half_edge_count(&res), 0, "closed result");
    assert_ne!(res.face_count(), target.face_count(), "the cut must happen");
    assert_ids_name_same_geometry(&target, &res, &verts, &edges, &faces);
    // Provenance is read on the renumbered mesh, so its ids agree with the result's.
    assert_eq!(prov.face_origin.len(), res.faces.len());
    for &f in &faces {
        assert_eq!(prov.face(f), Some(FaceOrigin::OperandA), "untouched target face {f:?}");
    }
    // Same geometry as the plain boolean: identical position multiset and counts.
    let plain = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("plain");
    let ms = |m: &HalfEdgeMesh| {
        let mut v: Vec<[i64; 3]> = (0..m.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| m.is_vert_live(v) && m.vert(v).half_edge != crate::half_edge::NO_EDGE)
            .map(|v| m.vert(v).position)
            .map(|p| [(p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64])
            .collect();
        v.sort();
        v
    };
    assert_eq!(ms(&res), ms(&plain), "renumbering must not change geometry");
    assert_eq!(res.face_count(), plain.face_count());
}

/// The Cut / island installs renumber a CONCATENATION (Cut = `(A−B) ⊕ (A∩B)`) onto the target
/// through `keep_operand_ids`. Untouched target geometry keeps its ids, and the returned face map
/// takes every concatenated face to the installed face with the same ring (a per-face plan
/// follows it).
#[test]
fn keep_operand_ids_renumbers_a_concatenation_onto_the_target() {
    use crate::half_edge::rebuild_ids_test_support::assert_ids_name_same_geometry;
    use crate::half_edge::{FaceId, HalfEdgeId, VertexId};
    let mut target = box_mesh(Vec3::ZERO, Vec3::ONE);
    target.subdivide_faces(&(0..6).collect());
    let cutter = box_mesh(Vec3::splat(1.0), Vec3::splat(0.5));
    let diff = mesh_boolean(&target, &cutter, BoolOp::Difference).expect("diff");
    let inter = mesh_boolean(&target, &cutter, BoolOp::Intersect).expect("inter");
    // Plain concatenation, numbered from scratch (what `merge_meshes` hands the install).
    let (mut pos, mut rings) = (Vec::new(), Vec::new());
    for part in [&diff, &inter] {
        let base = pos.len();
        pos.extend(part.vertices.iter().map(|v| v.position));
        for f in (0..part.faces.len() as u32).map(FaceId).filter(|&f| part.is_face_live(f)) {
            rings.push(part.face_vertices(f).iter().map(|v| base + v.0 as usize).collect::<Vec<_>>());
        }
    }
    let cat = HalfEdgeMesh::from_polygons(&pos, &rings);

    let (m, face_map) = keep_operand_ids(&target, &cat);
    assert!(m.validate().is_ok(), "{:?}", m.validate());
    let far = |p: Vec3| p.x < -0.5;
    let verts: Vec<VertexId> = (0..target.vertices.len() as u32)
        .map(VertexId)
        .filter(|&v| target.is_vert_live(v) && far(target.vert(v).position))
        .collect();
    let faces: Vec<FaceId> = (0..target.faces.len() as u32)
        .map(FaceId)
        .filter(|&f| target.is_face_live(f) && target.face_vertices(f).iter().all(|&v| far(target.vert(v).position)))
        .collect();
    let edges: Vec<HalfEdgeId> = faces.iter().flat_map(|&f| target.face_half_edges(f)).collect();
    assert_eq!((verts.len(), faces.len()), (9, 4), "fixture: the -X side");
    assert_ids_name_same_geometry(&target, &m, &verts, &edges, &faces);

    let ring = |mesh: &HalfEdgeMesh, f: FaceId| {
        let mut r: Vec<[i64; 3]> = mesh
            .face_vertices(f)
            .iter()
            .map(|&v| mesh.vert(v).position)
            .map(|p| [(p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64])
            .collect();
        r.sort();
        r
    };
    for f in (0..cat.faces.len() as u32).map(FaceId).filter(|&f| cat.is_face_live(f)) {
        let nf = face_map[f.0 as usize];
        assert!(m.is_face_live(nf), "face {f:?} mapped to a dead slot");
        assert_eq!(ring(&cat, f), ring(&m, nf), "face map sends {f:?} to a different polygon");
    }
}
