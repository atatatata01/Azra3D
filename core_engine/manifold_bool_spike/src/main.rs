//! Manifold boolean SPIKE — no production wiring.
//!
//! Cases (from COMPLETE_BUG_TRACKER BOOL-CURVED-SCALE + op_bench --probe):
//!   - box − uv_sphere 16×8 / 32×16  (SPHERE-NOTCLOSED was the old gate; now closed)
//!   - box − CC-subdivided box at densities matching audit (1 536 / 24 576 faces)
//!   - box − box control (must stay fast + correct)
//!
//! For each case: run existing `mesh_boolean` AND Manifold; report time, volume,
//! closed/manifold, face count. Manifold path triangulates on export (n-gons lost).

use core_math::half_edge::{FaceId, HalfEdgeMesh};
use core_math::mesh_boolean::{boundary_half_edge_count, mesh_boolean, mesh_signed_volume, BoolOp};
use glam::Vec3;
use manifold_csg::Manifold;
use std::time::Instant;

// ---------------------------------------------------------------------------
// HalfEdgeMesh ↔ Manifold (triangulating round-trip)
// ---------------------------------------------------------------------------

/// Fan-triangulate every face and build a Manifold (f64 MeshGL path).
/// Material IDs and half-edge topology are intentionally discarded — this is
/// the attribute cost of the candidate kernel (reported in the decision doc).
fn halfedge_to_manifold(mesh: &HalfEdgeMesh) -> Result<Manifold, String> {
    let mut verts: Vec<f64> = Vec::with_capacity(mesh.vertices.len() * 3);
    for v in &mesh.vertices {
        verts.push(v.position.x as f64);
        verts.push(v.position.y as f64);
        verts.push(v.position.z as f64);
    }
    let mut tris: Vec<u64> = Vec::new();
    // Live faces only — free-list holes return empty face_vertices / short rings.
    for fi in 0..mesh.faces.len() {
        let fid = FaceId(fi as u32);
        let vs = mesh.face_vertices(fid);
        if vs.len() < 3 {
            continue;
        }
        // Fan from first vertex: (0, i, i+1)
        let v0 = vs[0].0 as u64;
        for w in vs.windows(2).skip(1) {
            tris.push(v0);
            tris.push(w[0].0 as u64);
            tris.push(w[1].0 as u64);
        }
    }
    Manifold::from_mesh_f64(&verts, 3, &tris).map_err(|e| format!("Manifold::from_mesh_f64: {e:?}"))
}

/// Manifold → triangle-only HalfEdgeMesh via `from_triangles`.
/// Loses: material_id, poly_custom_data, n-gon faces, original element IDs.
fn manifold_to_halfedge(m: &Manifold) -> Result<HalfEdgeMesh, String> {
    m.status().map_err(|e| format!("Manifold status: {e:?}"))?;
    let (props, n_props, tri) = m.to_mesh_f64();
    if n_props < 3 {
        return Err(format!("n_props={n_props} < 3"));
    }
    let n_vert = props.len() / n_props;
    let mut positions = Vec::with_capacity(n_vert);
    for i in 0..n_vert {
        let o = i * n_props;
        positions.push(Vec3::new(
            props[o] as f32,
            props[o + 1] as f32,
            props[o + 2] as f32,
        ));
    }
    let mut tris: Vec<[usize; 3]> = Vec::with_capacity(tri.len() / 3);
    for c in tri.chunks_exact(3) {
        tris.push([c[0] as usize, c[1] as usize, c[2] as usize]);
    }
    Ok(HalfEdgeMesh::from_triangles(&positions, &tris))
}

// ---------------------------------------------------------------------------
// Case builders (match op_bench / audit)
// ---------------------------------------------------------------------------

fn box_at(center: Vec3, half: f32) -> HalfEdgeMesh {
    let mut m = HalfEdgeMesh::new_box(Vec3::splat(half));
    for v in m.vertices.iter_mut() {
        v.position += center;
    }
    m
}

fn sphere_at(center: Vec3, r: f32, segs: u32, rings: u32) -> HalfEdgeMesh {
    let mut m = HalfEdgeMesh::new_uv_sphere(r, segs, rings);
    for v in m.vertices.iter_mut() {
        v.position += center;
    }
    m
}

/// CC-subdivided box (the audit's "curved" dense solid). levels=4 → 1 536 faces,
/// levels=7 → 24 576 faces (matches COMPLETE_BUG_TRACKER BOOL-CURVED-SCALE wording).
fn cc_ball_at(center: Vec3, half: f32, levels: u32) -> HalfEdgeMesh {
    let mut m = HalfEdgeMesh::new_box(Vec3::splat(half));
    for _ in 0..levels {
        m.subdivide_catmull_clark();
    }
    for v in m.vertices.iter_mut() {
        v.position += center;
    }
    m
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

struct CaseResult {
    name: String,
    faces_a: usize,
    faces_b: usize,
    // existing kernel
    ours_ok: bool,
    ours_ms: f64,
    ours_faces: usize,
    ours_boundary: usize,
    ours_manifold: bool,
    ours_vol: f64,
    // manifold
    man_ok: bool,
    man_ms: f64,
    man_faces: usize,
    man_boundary: usize,
    man_manifold: bool,
    man_vol: f64,
    man_native_vol: f64,
    man_native_tri: usize,
    man_err: String,
}

fn run_case(name: &str, a: &HalfEdgeMesh, b: &HalfEdgeMesh, op: BoolOp) -> CaseResult {
    // --- existing mesh_boolean ---
    let t0 = Instant::now();
    let ours = mesh_boolean(a, b, op);
    let ours_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let (ours_ok, ours_faces, ours_boundary, ours_manifold, ours_vol) = match &ours {
        Some(m) => (
            true,
            m.face_count(),
            boundary_half_edge_count(m),
            m.is_manifold().is_ok(),
            mesh_signed_volume(m),
        ),
        None => (false, 0, 0, false, 0.0),
    };

    // --- Manifold ---
    let t1 = Instant::now();
    let man_result = (|| -> Result<(HalfEdgeMesh, f64, usize), String> {
        let ma = halfedge_to_manifold(a)?;
        let mb = halfedge_to_manifold(b)?;
        let out = match op {
            BoolOp::Difference => ma.difference(&mb),
            BoolOp::Union => ma.union(&mb),
            BoolOp::Intersect => ma.intersection(&mb),
        };
        out.status().map_err(|e| format!("boolean status: {e:?}"))?;
        let native_vol = out.volume();
        let native_tri = out.num_tri();
        let he = manifold_to_halfedge(&out)?;
        Ok((he, native_vol, native_tri))
    })();
    let man_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let (man_ok, man_faces, man_boundary, man_manifold, man_vol, man_native_vol, man_native_tri, man_err) =
        match man_result {
            Ok((m, nv, nt)) => (
                true,
                m.face_count(),
                boundary_half_edge_count(&m),
                m.is_manifold().is_ok(),
                mesh_signed_volume(&m),
                nv,
                nt,
                String::new(),
            ),
            Err(e) => (false, 0, 0, false, 0.0, 0.0, 0, e),
        };

    CaseResult {
        name: name.into(),
        faces_a: a.face_count(),
        faces_b: b.face_count(),
        ours_ok,
        ours_ms,
        ours_faces,
        ours_boundary,
        ours_manifold,
        ours_vol,
        man_ok,
        man_ms,
        man_faces,
        man_boundary,
        man_manifold,
        man_vol,
        man_native_vol,
        man_native_tri,
        man_err,
    }
}

fn print_case(r: &CaseResult) {
    println!("\n=== {} ===", r.name);
    println!(
        "  inputs: faces_a={} faces_b={}",
        r.faces_a, r.faces_b
    );
    println!(
        "  mesh_boolean:  ok={}  {:>10.1} ms  faces={} boundary={} manifold={} vol={:.6}",
        r.ours_ok, r.ours_ms, r.ours_faces, r.ours_boundary, r.ours_manifold, r.ours_vol
    );
    if r.man_ok {
        println!(
            "  manifold-csg:  ok={}  {:>10.1} ms  faces={} (native_tri={}) boundary={} manifold={} vol={:.6} (native_vol={:.6})",
            r.man_ok,
            r.man_ms,
            r.man_faces,
            r.man_native_tri,
            r.man_boundary,
            r.man_manifold,
            r.man_vol,
            r.man_native_vol
        );
    } else {
        println!(
            "  manifold-csg:  FAIL  {:>10.1} ms  err={}",
            r.man_ms, r.man_err
        );
    }
    // Correctness gates vs Blender-style invariants (closed, manifold, positive volume).
    if r.man_ok {
        let closed_ok = r.man_boundary == 0;
        let man_ok = r.man_manifold;
        let vol_ok = r.man_native_vol.is_finite() && r.man_native_vol > 0.0;
        let gate = if closed_ok && man_ok && vol_ok {
            "PASS"
        } else {
            "FAIL"
        };
        println!(
            "  manifold gates: closed={} manifold={} vol_positive={} → {}",
            closed_ok, man_ok, vol_ok, gate
        );
    }
}

fn main() {
    println!("manifold_bool_spike — Manifold vs mesh_boolean (SPIKE, no production wiring)");
    println!("crate: manifold-csg 0.3  (Apache-2.0 OR MIT; C++ manifold3d via manifold-csg-sys)");
    println!("existing mesh_boolean is NOT modified/deleted.\n");

    let mut results: Vec<CaseResult> = Vec::new();

    // Control: box − box (known-good class for our kernel)
    {
        let a = box_at(Vec3::ZERO, 1.0);
        let b = box_at(Vec3::splat(1.0), 0.7);
        results.push(run_case("control box-box diff", &a, &b, BoolOp::Difference));
    }

    // SPHERE cases (op_bench --probe) — now closed after SPHERE-NOTCLOSED fix
    {
        let a = box_at(Vec3::ZERO, 0.8);
        let b = sphere_at(Vec3::new(0.8, 0.0, 0.0), 0.6, 16, 8);
        results.push(run_case("box-sphere16x8 diff", &a, &b, BoolOp::Difference));
    }
    {
        let a = box_at(Vec3::ZERO, 0.8);
        let b = sphere_at(Vec3::new(0.8, 0.0, 0.0), 0.6, 32, 16);
        results.push(run_case("box-sphere32x16 diff", &a, &b, BoolOp::Difference));
    }
    {
        let a = sphere_at(Vec3::ZERO, 1.0, 16, 8);
        let b = box_at(Vec3::new(1.0, 0.0, 0.0), 0.5);
        results.push(run_case("sphere16-box diff", &a, &b, BoolOp::Difference));
    }
    {
        let a = box_at(Vec3::ZERO, 0.8);
        let b = sphere_at(Vec3::new(0.8, 0.0, 0.0), 0.6, 16, 8);
        results.push(run_case("box-sphere16 union", &a, &b, BoolOp::Union));
    }

    // BOOL-CURVED-SCALE: CC ball densities (box faces grow 6·4^L).
    // L4 → 1 536 faces (audit "None"), L6 → 24 576 faces (audit "281 s + wrong 6-face").
    // L5 intermediate. Skip L7 (98 304 faces) unless needed — already past production density.
    for levels in [4u32, 5, 6] {
        let a = box_at(Vec3::ZERO, 0.8);
        let b = cc_ball_at(Vec3::new(0.8, 0.0, 0.0), 0.75, levels);
        results.push(run_case(
            &format!("box-CC L{levels} (faces_b={}) diff", b.face_count()),
            &a,
            &b,
            BoolOp::Difference,
        ));
    }

    for r in &results {
        print_case(r);
    }

    // Summary table
    println!("\n\n======== SUMMARY ========");
    println!(
        "{:<42} {:>10} {:>10} {:>8} {:>8} {:>10} {:>10}",
        "case", "ours_ms", "man_ms", "ours_ok", "man_ok", "ours_vol", "man_vol"
    );
    for r in &results {
        println!(
            "{:<42} {:>10.1} {:>10.1} {:>8} {:>8} {:>10.4} {:>10.4}",
            r.name,
            r.ours_ms,
            r.man_ms,
            r.ours_ok,
            r.man_ok,
            r.ours_vol,
            if r.man_ok {
                r.man_native_vol
            } else {
                f64::NAN
            }
        );
    }

    // Attribute / round-trip note
    println!(
        "\nRound-trip note: Manifold path fan-triangulates input n-gons and exports triangles only.\n\
         material_id / poly_custom_data / original element IDs are NOT preserved (BOOL-MATLOSS class).\n\
         Existing mesh_boolean already loses materials on from_polygons install — same class of gap."
    );
}
