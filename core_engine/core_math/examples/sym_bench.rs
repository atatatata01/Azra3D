//! Headless timing probe for the Alt+X Symmetrize kernel path on sculpt-sized meshes
//! (owner report 2026-08-23: "symmetrize alt x with large poly object in sculpt mode is quite slow").
//!
//! Times each phase of `HalfEdgeMesh::symmetrize` separately — clone, bisect_plane,
//! delete_half_eps, mirrored_across — so the dominant cost is measured, not guessed
//! (instrument-then-cut). Run with:
//!   cargo run -p core_math --release --example sym_bench

use core_math::half_edge::HalfEdgeMesh;
use glam::Vec3;
use std::time::Instant;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn bench(segments: u32, rings: u32) {
    let t = Instant::now();
    let base = HalfEdgeMesh::new_uv_sphere(1.0, segments, rings);
    let build_ms = ms(t);
    let faces = base.face_count();
    let hes = base.half_edges.len();
    println!(
        "--- sphere {}x{}: {} faces, {} half-edges (built in {:.0} ms) ---",
        segments, rings, faces, hes, build_ms
    );

    let origin = Vec3::ZERO;
    let normal = Vec3::X;
    let thr = 0.001f32; // app default merge_threshold magnitude
    let eps = thr.max(1e-5);

    // Phase-by-phase (mirrors symmetrize()'s exact sequence).
    let t = Instant::now();
    let mut m = base.clone();
    println!("clone            {:>10.1} ms", ms(t));

    let t = Instant::now();
    m.bisect_plane(origin, normal, eps);
    println!("bisect_plane     {:>10.1} ms", ms(t));

    let t = Instant::now();
    m.delete_half_eps(origin, normal, eps);
    println!("delete_half_eps  {:>10.1} ms", ms(t));

    let t = Instant::now();
    let m2 = m.mirrored_across(origin, normal, true, thr);
    println!("mirrored_across  {:>10.1} ms", ms(t));
    drop(m2);

    // Whole call, as the app runs it.
    let t = Instant::now();
    let mut whole = base.clone();
    whole.symmetrize(origin, normal, true, thr);
    println!("symmetrize TOTAL {:>10.1} ms  ({} faces out)", ms(t), whole.face_count());
}

fn main() {
    // Small poly-modeling size, mid sculpt size, large sculpt size.
    bench(100, 100);
    bench(320, 320);
    bench(560, 560);
}
