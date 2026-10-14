//! Profiling harness for the POLY Smooth (Laplacian) modifier — `HalfEdgeMesh::smooth_laplacian`,
//! the kernel behind `PolyModifierType::Smooth` (`app/src/poly/mesh_sync.rs`).
//!
//! Distinct from `bench_display_mesh_ops` (that one profiles the post-DC `DisplayMesh` fold, a
//! different data structure and a different stack). Answers "where does a slider drag on the Smooth
//! modifier actually spend its time" with measurements rather than assumptions.
//!
//! ```text
//! cargo test -p core_math --release --test bench_poly_smooth -- --ignored --nocapture
//! ```
//!
//! RUN IT IN --release. A debug build overstates scalar float work by ~10-30x.

use core_math::half_edge::*;
use glam::Vec3;

/// A quad UV-sphere with `rings * segs` faces — stands in for an authored poly cage: closed,
/// manifold, all-quad, valence 4 everywhere except the two poles.
fn uv_sphere_quads(rings: usize, segs: usize) -> HalfEdgeMesh {
    let mut positions: Vec<Vec3> = Vec::with_capacity((rings + 1) * segs);
    for r in 0..=rings {
        let phi = (r as f32 / rings as f32) * std::f32::consts::PI;
        for s in 0..segs {
            let theta = (s as f32 / segs as f32) * std::f32::consts::TAU;
            positions.push(Vec3::new(
                phi.sin() * theta.cos(),
                phi.cos(),
                phi.sin() * theta.sin(),
            ));
        }
    }
    let mut faces: Vec<Vec<usize>> = Vec::with_capacity(rings * segs);
    for r in 0..rings {
        for s in 0..segs {
            let s1 = (s + 1) % segs;
            let a = r * segs + s;
            let b = r * segs + s1;
            let c = (r + 1) * segs + s1;
            let d = (r + 1) * segs + s;
            faces.push(vec![a, b, c, d]);
        }
    }
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

fn ms(f: impl FnOnce()) -> f64 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

/// Median of 5 — one-shot timings on Windows are noisy enough to invert an ordering.
fn ms5(mut f: impl FnMut()) -> f64 {
    let mut v: Vec<f64> = (0..5).map(|_| ms(&mut f)).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[2]
}

fn sizes() -> Vec<(usize, usize)> {
    // (rings, segs) -> faces = rings*segs: ~2.5k, 10k, 40k, 160k quads
    vec![(50, 50), (100, 100), (200, 200), (400, 400)]
}

#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_smooth_by_mesh_size() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    eprintln!("\n=== poly smooth_laplacian vs mesh size (median of 5, ms) ===");
    eprintln!(
        "{:>8} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "quads", "verts", "clone", "x1", "x5", "x20"
    );
    for (r, s) in sizes() {
        let mesh = uv_sphere_quads(r, s);
        let (f, v) = (mesh.face_count(), mesh.vertex_count());

        let t_clone = ms5(|| {
            let _ = mesh.clone();
        });
        let t1 = ms5(|| {
            let mut m = mesh.clone();
            m.smooth_laplacian(1, 0.5);
        }) - t_clone;
        let t5 = ms5(|| {
            let mut m = mesh.clone();
            m.smooth_laplacian(5, 0.5);
        }) - t_clone;
        let t20 = ms5(|| {
            let mut m = mesh.clone();
            m.smooth_laplacian(20, 0.5);
        }) - t_clone;

        eprintln!("{f:>8} {v:>8} {t_clone:>10.2} {t1:>10.2} {t5:>10.2} {t20:>10.2}");
    }
}

/// STAGE SPLIT — the number that decided what to optimize. `smooth_laplacian` is three stages:
/// build the 1-ring adjacency table, run N Jacobi passes, then `recompute_normals()`.
/// The adjacency build and the normal pass are per-CALL costs that do not shrink with iterations,
/// and together they were 90% of the op (40k quads: 9.5 + 12.8 of 25 ms).
///
/// The `adjacency` column keeps the ORIGINAL `HashMap` build inline, so it now reads as the cost
/// of the approach that was replaced — the shipped CSR build is the difference between the total
/// (`bench_smooth_by_mesh_size`) and the other two columns.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_smooth_stage_split() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    eprintln!("\n=== smooth stage split (median of 5, ms) ===");
    eprintln!(
        "{:>8} {:>8} {:>12} {:>12} {:>12} {:>12}",
        "quads", "verts", "oldHashMap", "table+iter", "normals", "TOTAL x1"
    );
    eprintln!("        oldHashMap = the REPLACED build, kept as the historical baseline.");
    eprintln!("        table+iter = shipped CSR build + 1 Jacobi pass (total minus normals).");
    for (r, s) in sizes() {
        let mesh = uv_sphere_quads(r, s);
        let (fc, vc) = (mesh.face_count(), mesh.vertex_count());

        // Stage A: the ORIGINAL HashMap adjacency build, kept as the historical baseline.
        let t_adj = ms5(|| {
            let mut adj: std::collections::HashMap<u32, Vec<VertexId>> =
                std::collections::HashMap::new();
            for i in 0..mesh.faces.len() as u32 {
                let fid = FaceId(i);
                if mesh.face(fid).half_edge == NO_EDGE {
                    continue;
                }
                let ring = mesh.face_vertices(fid);
                for k in 0..ring.len() {
                    let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                    adj.entry(a.0).or_default().push(b);
                    adj.entry(b.0).or_default().push(a);
                }
            }
            for ns in adj.values_mut() {
                ns.sort_by_key(|v| v.0);
                ns.dedup_by_key(|v| v.0);
            }
            std::hint::black_box(&adj);
        });

        // Stage C: the trailing normal recompute.
        let t_norm = ms5(|| {
            let mut m = mesh.clone();
            m.recompute_normals();
        }) - ms5(|| {
            let _ = mesh.clone();
        });

        // Stage B by difference: total(1 iter) - adjacency - normals.
        let t_clone = ms5(|| {
            let _ = mesh.clone();
        });
        let t_total1 = ms5(|| {
            let mut m = mesh.clone();
            m.smooth_laplacian(1, 0.5);
        }) - t_clone;
        // The SHIPPED table build + one Jacobi pass, by difference: everything in
        // `smooth_laplacian` that is not the trailing normal recompute.
        let t_table = (t_total1 - t_norm).max(0.0);

        eprintln!(
            "{fc:>8} {vc:>8} {t_adj:>12.2} {t_table:>12.2} {t_norm:>12.2} {t_total1:>12.2}"
        );
    }
}

/// WHAT IS LEFT IN THE BAKE, stage by stage — the input to "is another 2x worth chasing, and
/// with which technique". Each stage is timed by replaying it against the public API, so the
/// numbers are attributable rather than a single opaque total.
///
/// The split that matters is TOPOLOGY-INVARIANT vs POSITION-DEPENDENT. During a slider drag the
/// base mesh's connectivity never changes, so any stage in the first group is recomputed every
/// frame for nothing and is a candidate for a retained cache; stages in the second group can only
/// be made faster, not skipped.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_bake_stage_split() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    use core_math::poly_mesh::{
        auto_smooth_corner_normal_pre, earclip_polygon, fan_face_areas, FanWorkspace,
    };
    eprintln!("\n=== bake stage split (median of 5, ms) ===");
    eprintln!(
        "{:>8} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "quads", "faceNrm", "areas", "order*", "cornerN", "earclip*", "TOTAL"
    );
    eprintln!("        (* = topology-invariant: identical every frame of a slider drag)");
    eprintln!("        stage columns are SERIAL replays, for attribution only.");
    eprintln!("        TOTAL is the real bake, which runs those stages in PARALLEL — so TOTAL is");
    eprintln!("        expected to be SMALLER than the columns, not their sum.");
    for (r, s) in sizes() {
        let mesh = uv_sphere_quads(r, s);
        let fc = mesh.face_count();
        let cfg = core_math::poly_mesh::NormalConfig::default();

        let t_fnrm = ms5(|| {
            let (mut ring, mut pts) = (Vec::new(), Vec::new());
            let v: Vec<glam::Vec3> = (0..mesh.faces.len())
                .map(|i| mesh.compute_face_normal_into(FaceId(i as u32), &mut ring, &mut pts))
                .collect();
            std::hint::black_box(&v);
        });
        let t_area = ms5(|| {
            std::hint::black_box(fan_face_areas(&mesh));
        });
        let t_order = ms5(|| {
            let mut o: Vec<u32> = (0..mesh.faces.len() as u32)
                .filter(|&i| mesh.face(FaceId(i)).half_edge != NO_EDGE)
                .collect();
            o.sort_by_key(|&i| mesh.face(FaceId(i)).material_id);
            std::hint::black_box(&o);
        });

        let face_normals: Vec<glam::Vec3> = (0..mesh.faces.len())
            .map(|i| mesh.compute_face_normal(FaceId(i as u32)))
            .collect();
        let areas = fan_face_areas(&mesh);
        let t_corner = ms5(|| {
            let mut ws = FanWorkspace::default();
            let mut fv: Vec<VertexId> = Vec::new();
            let mut acc = glam::Vec3::ZERO;
            for i in 0..mesh.faces.len() as u32 {
                let fid = FaceId(i);
                if mesh.face(fid).half_edge == NO_EDGE {
                    continue;
                }
                mesh.face_vertices_into(fid, &mut fv);
                for k in 0..fv.len() {
                    acc += auto_smooth_corner_normal_pre(
                        &mesh,
                        &face_normals,
                        Some(&areas),
                        fv[k],
                        fid,
                        cfg.cos_thresh(),
                        None,
                        &mut ws,
                    );
                }
            }
            std::hint::black_box(acc);
        });
        let t_clip = ms5(|| {
            let mut fv: Vec<VertexId> = Vec::new();
            let mut pts: Vec<glam::Vec3> = Vec::new();
            let mut n = 0usize;
            for i in 0..mesh.faces.len() as u32 {
                let fid = FaceId(i);
                if mesh.face(fid).half_edge == NO_EDGE {
                    continue;
                }
                mesh.face_vertices_into(fid, &mut fv);
                pts.clear();
                pts.extend(fv.iter().map(|&v| mesh.vert(v).position));
                n += earclip_polygon(&pts, face_normals[i as usize]).len();
            }
            std::hint::black_box(n);
        });
        let t_total = ms5(|| {
            let _ = core_math::poly_mesh::raster_data_ngon_with_normals(&mesh, &cfg);
        });

        eprintln!(
            "{fc:>8} {t_fnrm:>9.2} {t_area:>9.2} {t_order:>9.2} {t_corner:>9.2} {t_clip:>9.2} {t_total:>9.2}"
        );
    }
}

/// THE WHOLE LIVE FRAME. Dragging the Smooth modifier's slider costs more than the kernel: the
/// app re-applies the modifier from the cached stack prefix and then re-bakes the display mesh
/// (`raster_data_ngon_with_normals`, `app/src/poly/mesh_sync.rs:691`). Optimising only the kernel
/// while the bake dominates would not change how the drag FEELS, so both are measured together.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_live_slider_frame() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    eprintln!("\n=== one Smooth-slider frame (median of 5, ms) ===");
    eprintln!(
        "{:>8} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "quads", "verts", "clone", "smooth", "bake", "TOTAL"
    );
    for (r, s) in sizes() {
        let mesh = uv_sphere_quads(r, s);
        let (fc, vc) = (mesh.face_count(), mesh.vertex_count());

        let t_clone = ms5(|| {
            let _ = mesh.clone();
        });
        let t_smooth = ms5(|| {
            let mut m = mesh.clone();
            m.smooth_laplacian(3, 0.5);
        }) - t_clone;

        let smoothed = {
            let mut m = mesh.clone();
            m.smooth_laplacian(3, 0.5);
            m
        };
        let cfg = core_math::poly_mesh::NormalConfig::default();
        let t_bake = ms5(|| {
            let _ = core_math::poly_mesh::raster_data_ngon_with_normals(&smoothed, &cfg);
        });

        let total = t_clone + t_smooth + t_bake;
        eprintln!(
            "{fc:>8} {vc:>8} {t_clone:>10.2} {t_smooth:>10.2} {t_bake:>10.2} {total:>10.2}"
        );
    }
}
