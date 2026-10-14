//! DIAGNOSTIC probe for Alt+X symmetrize when the mirror plane comes from a SELECTED FACE.
//!
//! The app path (`app/src/sym_pie.rs::poly_sym_plane_from_selection`) builds the plane as
//! `(average face centroid, average face normal)` and then calls `HalfEdgeMesh::symmetrize`.
//! Existing kernel tests only ever cover axis-aligned planes at the geometry center; this probe
//! exercises the shapes a user actually clicks: a surface (tangent) face, a non-planar quad, a
//! tilted face on a curved mesh, and a multi-face average.
//!
//! Run: `cargo run -p core_math --example sym_face_probe`

use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_TWIN};
use glam::Vec3;

/// The plane the app derives from ONE selected face.
fn face_plane(m: &HalfEdgeMesh, f: FaceId) -> (Vec3, Vec3) {
    let vs = m.face_vertices(f);
    let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
    (c, m.compute_face_normal(f).normalize_or_zero())
}

fn boundary_edges(m: &HalfEdgeMesh) -> usize {
    (0..m.half_edges.len() as u32)
        .filter(|&i| m.is_edge_live(HalfEdgeId(i)) && m.he(HalfEdgeId(i)).twin == NO_TWIN)
        .count()
}

/// Faces with ~zero area — slivers the user reads as "bad geo".
fn degenerate_faces(m: &HalfEdgeMesh) -> usize {
    let mut n = 0;
    for fi in 0..m.faces.len() as u32 {
        let f = FaceId(fi);
        if !m.is_face_live(f) {
            continue;
        }
        let vs = m.face_vertices(f);
        if vs.len() < 3 {
            n += 1;
            continue;
        }
        let mut area = Vec3::ZERO;
        for i in 0..vs.len() {
            let a = m.vert(vs[i]).position;
            let b = m.vert(vs[(i + 1) % vs.len()]).position;
            area += a.cross(b);
        }
        if area.length() * 0.5 < 1e-7 {
            n += 1;
        }
    }
    n
}

fn live_verts(m: &HalfEdgeMesh) -> usize {
    (0..m.vertices.len() as u32)
        .filter(|&i| m.is_vert_live(VertexId(i)))
        .count()
}

fn report(tag: &str, m: &HalfEdgeMesh) {
    println!(
        "  {:<26} faces={:<5} verts={:<5} boundary={:<4} degen={:<3} manifold={} validate={}",
        tag,
        m.face_count(),
        live_verts(m),
        boundary_edges(m),
        degenerate_faces(m),
        match m.is_manifold() {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("NO: {e}"),
        },
        match m.validate() {
            Ok(()) => "ok".to_string(),
            Err(e) => e,
        },
    );
}

fn find_face(m: &HalfEdgeMesh, pred: impl Fn(Vec3) -> bool) -> FaceId {
    (0..m.faces.len() as u32)
        .map(FaceId)
        .find(|&f| m.is_face_live(f) && pred(m.compute_face_normal(f)))
        .expect("no matching face")
}

fn main() {
    // --------------------------------------------------------------------
    // 1. Tangent face: user clicks a face on the SURFACE of a closed mesh.
    //    The plane touches the mesh, so the whole body sits on ONE side.
    // --------------------------------------------------------------------
    println!("\n== 1. tangent face on a box (+X face) ==");
    let base = HalfEdgeMesh::new_box(Vec3::ONE);
    let f = find_face(&base, |n| n.x > 0.9);
    let (o, n) = face_plane(&base, f);
    println!("  plane origin={o:?} normal={n:?}");
    report("input", &base);
    let mut a = base.clone();
    a.symmetrize(o, -n, true, 0.001);
    report("keep-inward (-n)", &a);
    let mut b = base.clone();
    b.symmetrize(o, n, true, 0.001);
    report("keep-outward (+n)", &b);

    // --------------------------------------------------------------------
    // 2. Non-planar selected quad — any deformed / sculpted mesh. The derived
    //    plane does NOT contain the quad's own vertices.
    // --------------------------------------------------------------------
    println!("\n== 2. NON-PLANAR selected quad ==");
    let mut warped = HalfEdgeMesh::new_box(Vec3::ONE);
    let fw = find_face(&warped, |n| n.x > 0.9);
    let v0 = warped.face_vertices(fw)[0];
    warped.vert_mut(v0).position += Vec3::new(0.35, 0.0, 0.0);
    warped.recompute_normals();
    let (o, n) = face_plane(&warped, fw);
    let sd: Vec<f32> = warped
        .face_vertices(fw)
        .iter()
        .map(|&v| (warped.vert(v).position - o).dot(n))
        .collect();
    println!("  plane origin={o:?} normal={n:?}");
    println!("  signed dist of the quad's OWN verts to its plane: {sd:?}");
    report("input", &warped);
    let mut a = warped.clone();
    a.symmetrize(o, -n, true, 0.001);
    report("keep-inward (-n)", &a);
    let mut b = warped.clone();
    b.symmetrize(o, n, true, 0.001);
    report("keep-outward (+n)", &b);

    // 2b. Step-by-step: where does the open boundary come from?
    println!("\n== 2b. stage dump for the non-planar quad (keep-inward) ==");
    {
        let mut m = warped.clone();
        let kn = -n;
        let eps = 0.001f32;
        let sd_of = |m: &HalfEdgeMesh, v: VertexId| (m.vert(v).position - o).dot(kn);
        m.bisect_plane(o, kn, eps);
        let straddlers = (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| m.is_face_live(f))
            .filter(|&f| {
                let vs = m.face_vertices(f);
                vs.iter().any(|&v| sd_of(&m, v) > eps) && vs.iter().any(|&v| sd_of(&m, v) < -eps)
            })
            .count();
        println!("  after bisect: faces={} STILL-STRADDLING={}", m.face_count(), straddlers);
        m.delete_half_eps(o, kn, eps);
        let mut max_abs = 0.0f32;
        let mut bcount = 0;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) || m.he(h).twin != NO_TWIN {
                continue;
            }
            bcount += 1;
            for v in [m.he_source(h), m.he(h).vertex] {
                max_abs = max_abs.max(sd_of(&m, v).abs());
            }
        }
        println!(
            "  after delete_half: faces={} boundary={} max|sd| on boundary verts={:.6} (weld threshold=0.001)",
            m.face_count(),
            bcount,
            max_abs
        );
        // Seam loop shape: how many boundary loops, and does any vertex appear twice (a pinch)?
        let mut bverts: Vec<u32> = Vec::new();
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if m.is_edge_live(h) && m.he(h).twin == NO_TWIN {
                bverts.push(m.he_source(h).0);
            }
        }
        let mut dup = bverts.clone();
        dup.sort_unstable();
        let uniq = {
            let mut d = dup.clone();
            d.dedup();
            d.len()
        };
        println!(
            "  seam: {} boundary half-edges over {} distinct source verts (pinch if fewer)",
            bverts.len(),
            uniq
        );

        let out = m.mirrored_across(o, kn, true, 0.001);
        report("after mirror", &out);

        // Which faces share a directed edge (the non-manifold report)?
        use std::collections::HashMap;
        let mut dir: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        for i in 0..out.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !out.is_edge_live(h) {
                continue;
            }
            dir.entry((out.he_source(h).0, out.he(h).vertex.0))
                .or_default()
                .push(out.he(h).face.0);
        }
        for ((a, b), faces) in dir.iter() {
            if faces.len() > 1 {
                println!(
                    "  DUP directed edge {a}->{b} pos {:?} -> {:?} faces={faces:?}",
                    out.vert(VertexId(*a)).position,
                    out.vert(VertexId(*b)).position
                );
                for &fi in faces {
                    let ring = out.face_vertices(FaceId(fi));
                    println!(
                        "     face {fi}: {:?}",
                        ring.iter().map(|v| v.0).collect::<Vec<_>>()
                    );
                }
            }
        }
    }

    // --------------------------------------------------------------------
    // 3. Tilted face on a curved mesh — plane slices the sphere at an angle.
    // --------------------------------------------------------------------
    println!("\n== 3. tilted face on a UV sphere ==");
    let sph = HalfEdgeMesh::new_uv_sphere(1.0, 16, 8);
    let fs = find_face(&sph, |n| n.x > 0.3 && n.y > 0.3 && n.z > 0.3);
    let (o, n) = face_plane(&sph, fs);
    println!("  plane origin={o:?} normal={n:?}");
    report("input", &sph);
    let mut a = sph.clone();
    a.symmetrize(o, -n, true, 0.001);
    report("keep-inward (-n)", &a);
    let mut b = sph.clone();
    b.symmetrize(o, n, true, 0.001);
    report("keep-outward (+n)", &b);

    // --------------------------------------------------------------------
    // 4. TWO selected faces — the app averages centroids AND normals, so the
    //    origin lies on NEITHER face.
    // --------------------------------------------------------------------
    println!("\n== 4. two adjacent faces (+X and +Y), averaged ==");
    let two = HalfEdgeMesh::new_box(Vec3::ONE);
    let (o1, n1) = face_plane(&two, find_face(&two, |n| n.x > 0.9));
    let (o2, n2) = face_plane(&two, find_face(&two, |n| n.y > 0.9));
    let o = (o1 + o2) * 0.5;
    let n = (n1 + n2).normalize_or_zero();
    println!("  plane origin={o:?} normal={n:?}");
    let mut a = two.clone();
    a.symmetrize(o, -n, true, 0.001);
    report("keep-inward (-n)", &a);
    let mut b = two.clone();
    b.symmetrize(o, n, true, 0.001);
    report("keep-outward (+n)", &b);

    // --------------------------------------------------------------------
    // 5. Control: the axis-aligned midplane case the kernel tests DO cover.
    // --------------------------------------------------------------------
    println!("\n== 5. control: geometry-center midplane (covered by tests) ==");
    let mut c = HalfEdgeMesh::new_box(Vec3::ONE);
    c.subdivide_catmull_clark();
    c.symmetrize(Vec3::ZERO, Vec3::X, true, 0.001);
    report("midplane +X", &c);

    // --------------------------------------------------------------------
    // 6. A face on a SUBDIVIDED flat wall: the plane is coplanar with a whole
    //    band of faces, not just the clicked one.
    // --------------------------------------------------------------------
    println!("\n== 6. one face of a subdivided flat wall ==");
    let mut sub = HalfEdgeMesh::new_box(Vec3::ONE);
    sub.subdivide_catmull_clark();
    sub.subdivide_catmull_clark();
    let fsub = find_face(&sub, |n| n.x > 0.9);
    let (o, n) = face_plane(&sub, fsub);
    println!("  plane origin={o:?} normal={n:?}");
    report("input", &sub);
    let mut a = sub.clone();
    a.symmetrize(o, -n, true, 0.001);
    report("keep-inward (-n)", &a);
    let mut b = sub.clone();
    b.symmetrize(o, n, true, 0.001);
    report("keep-outward (+n)", &b);
}
