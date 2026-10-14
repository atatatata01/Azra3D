//! Owner repro 2026-08-08: "the same cut with small placement variation does not give me
//! the same result." Jitter fuzz: one canonical subtract, many sub-millimetre placement
//! variations — every outcome must be EQUIVALENT (valid + watertight + a hole exists +
//! counts inside a tight band). Any variation that flips outcome class is the bug.
use core_math::half_edge::{HalfEdgeMesh, VertexId};
use core_math::mesh_boolean::{mesh_boolean, BoolOp};
use glam::Vec3;

/// Deterministic pseudo-random offsets (no RNG — reproducible).
fn jitters() -> Vec<Vec3> {
    let mut out = Vec::new();
    // Scales from 1e-4 (numeric-noise class) to 5e-2 (visible nudge), 8 directions each.
    for &s in &[1e-4f32, 1e-3, 5e-3, 1e-2, 5e-2] {
        for k in 0..8 {
            let a = k as f32 * std::f32::consts::TAU / 8.0 + 0.37;
            out.push(Vec3::new(a.cos() * s, (k as f32 * 0.13).sin() * s * 0.5, a.sin() * s));
        }
    }
    out
}

fn translated(mesh: &HalfEdgeMesh, d: Vec3) -> HalfEdgeMesh {
    let mut m = mesh.clone();
    for i in 0..m.vertices.len() as u32 {
        let v = VertexId(i);
        if m.is_vert_live(v) {
            m.vert_mut(v).position += d;
        }
    }
    m
}

/// Verdict of one subtract, compressed for comparison.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// verts, faces bucketed loosely; hole = verts near the cut circle exist.
    Ok { hole: bool },
    Refused,
    Invalid(&'static str),
}

fn run_one(base: &HalfEdgeMesh, cutter: &HalfEdgeMesh) -> (Outcome, usize, usize) {
    match mesh_boolean(base, cutter, BoolOp::Difference) {
        None => (Outcome::Refused, 0, 0),
        Some(out) => {
            if out.validate().is_err() {
                return (Outcome::Invalid("validate"), out.vertex_count(), out.face_count());
            }
            if out.is_manifold().is_err() {
                return (Outcome::Invalid("manifold"), out.vertex_count(), out.face_count());
            }
            // Hole present: some vertex clearly inside the top face ring (not the box rim)
            // at the cut radius.
            let hole = (0..out.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| out.is_vert_live(v))
                .map(|v| out.vert(v).position)
                .any(|p| {
                    (p.y - 1.0).abs() < 1e-3 && (p.x * p.x + p.z * p.z).sqrt() < 0.75
                });
            (Outcome::Ok { hole }, out.vertex_count(), out.face_count())
        }
    }
}

/// The gate: every jittered placement of the same cylinder subtract on a box must land in
/// the same outcome class with counts in a tight band. Prints the full profile; fails on
/// any Invalid outcome or any outcome-class flip.
#[test]
fn same_cut_jittered_placements_agree() {
    let base = HalfEdgeMesh::new_box(Vec3::ONE);
    let cyl = HalfEdgeMesh::new_cylinder(0.45, 3.0, 12);

    let (base_outcome, v0, f0) = run_one(&base, &cyl);
    eprintln!("canonical: {:?} v={} f={}", base_outcome, v0, f0);
    assert_eq!(base_outcome, Outcome::Ok { hole: true }, "canonical cut sane");

    let mut flips = 0usize;
    let mut invalids = 0usize;
    let (mut vmin, mut vmax) = (v0, v0);
    for (i, d) in jitters().into_iter().enumerate() {
        let cutter = translated(&cyl, d);
        let (o, v, f) = run_one(&base, &cutter);
        vmin = vmin.min(v);
        vmax = vmax.max(v);
        let same = o == Outcome::Ok { hole: true };
        if !same {
            flips += 1;
            if matches!(o, Outcome::Invalid(_)) {
                invalids += 1;
            }
            eprintln!("jitter[{i}] d=({:.5},{:.5},{:.5}) OUTCOME {:?} v={v} f={f}", d.x, d.y, d.z, o);
        }
    }
    eprintln!("jitter profile: flips={flips} invalids={invalids} vert range {vmin}..{vmax} (canonical {v0})");
    assert_eq!(invalids, 0, "{invalids} placements produced INVALID meshes");
    assert_eq!(flips, 0, "{flips} placements flipped outcome class");
    // Count stability: same cut ± tiny nudge should not change vertex count wildly.
    assert!(
        vmax.saturating_sub(vmin) <= v0 / 2,
        "vertex count unstable across jitter: {vmin}..{vmax}"
    );
}

/// Same fuzz on the SUBDIVIDED-face case (lattice cube): jittered small cylinder through
/// the top face. Today the kernel's coplanar merge makes counts collapse; after the
/// boolean-locality fix these must be stable AND keep the lattice.
#[test]
fn jittered_cut_on_subdivided_face_is_stable() {
    // Lattice cube built from grid faces (same construction as bevel_absorb tests).
    let n = 8usize;
    let mut map = std::collections::HashMap::new();
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
    let base = HalfEdgeMesh::from_polygons(&positions, &faces);
    let cyl = HalfEdgeMesh::new_cylinder(0.3, 3.0, 12);

    let (o0, v0, f0) = run_one(&base, &cyl);
    eprintln!("lattice canonical: {:?} v={} f={} (input v={})", o0, v0, f0, base.vertex_count());
    let mut invalids = 0usize;
    let (mut vmin, mut vmax) = (v0, v0);
    for (i, d) in jitters().into_iter().enumerate() {
        let (o, v, _f) = run_one(&base, &translated(&cyl, d));
        vmin = vmin.min(v);
        vmax = vmax.max(v);
        if matches!(o, Outcome::Invalid(_)) {
            invalids += 1;
            eprintln!("lattice jitter[{i}] d=({:.5},{:.5},{:.5}) INVALID", d.x, d.y, d.z);
        }
    }
    eprintln!("lattice jitter profile: invalids={invalids} vert range {vmin}..{vmax} (canonical {v0}, input {})", base.vertex_count());
    assert_eq!(invalids, 0, "jittered lattice cuts produced invalid meshes");
    assert!(
        vmax.saturating_sub(vmin) <= v0.max(1) / 2,
        "lattice cut count unstable: {vmin}..{vmax}"
    );
}
