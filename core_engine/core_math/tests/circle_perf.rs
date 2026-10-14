//! Timed reproduction for owner bug #6 ("Alt+C … the calcul take way too long like 5 seconde plus").
//! Measures each stage the app's Circle path runs so we fix the MEASURED hotspot, not a guess.
//! Run: cargo test -p core_math --test circle_perf -- --nocapture

use std::collections::HashSet;
use std::time::Instant;
use core_math::half_edge::{FaceId, HalfEdgeMesh};
use core_math::poly_mesh::raster_data_ngon;

fn time_ms<F: FnMut()>(iters: u32, mut f: F) -> f64 {
    // warm
    f();
    let t = Instant::now();
    for _ in 0..iters { f(); }
    t.elapsed().as_secs_f64() * 1000.0 / iters as f64
}

#[test]
fn measure_circle_stages() {
    println!("\n=== Alt+C Circle perf reproduction ===");

    // (i) plain grid, ~400 faces — realistic small region
    {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, 20, 20);
        let faces: HashSet<FaceId> = (0..base.faces.len() as u32).map(FaceId).collect();
        let ms_bl = time_ms(20, || { let _ = base.region_boundary_loops(&faces); });
        let ms_circle = time_ms(20, || {
            let mut m = base.clone();
            let _ = m.circle_region(&faces, None, true, true, 4);
        });
        let ms_raster = time_ms(20, || { let _ = raster_data_ngon(&base); });
        println!("(i)   grid 20x20 ({} faces): boundary_loops={:.3}ms  circle_region(relax4)={:.3}ms  raster_ngon={:.3}ms",
                 base.faces.len(), ms_bl, ms_circle, ms_raster);
    }

    // (ii) a body WITH a Subdivide modifier at levels 3-4 — the app re-runs this on EVERY sync
    {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4); // 16 faces before subdivide
        for lvl in 1..=4u32 {
            let ms = time_ms(3, || {
                let mut m = base.clone();
                for _ in 0..lvl { m.subdivide_catmull_clark(); }
            });
            let mut cnt = base.clone();
            for _ in 0..lvl { cnt.subdivide_catmull_clark(); }
            println!("(ii)  subdivide_catmull_clark x{} from 16-face plane -> {} faces: {:.3}ms",
                     lvl, cnt.faces.len(), ms);
        }
        // full display-mesh rebuild (subdivide + raster) at level 4, as apply_poly_modifiers does
        let ms_full = time_ms(3, || {
            let mut m = base.clone();
            for _ in 0..4 { m.subdivide_catmull_clark(); }
            let _ = raster_data_ngon(&m);
        });
        let mut big = base.clone();
        for _ in 0..4 { big.subdivide_catmull_clark(); }
        let ms_raster_big = time_ms(5, || { let _ = raster_data_ngon(&big); });
        println!("(ii)  FULL apply_poly_modifiers(subdiv=4)+raster -> {} faces: {:.3}ms   (raster alone {:.3}ms)",
                 big.faces.len(), ms_full, ms_raster_big);
    }

    // (iii) a large selected region on a big grid (circle over many boundary verts + relax interior)
    {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, 60, 60); // 3600 faces
        let faces: HashSet<FaceId> = (0..base.faces.len() as u32).map(FaceId).collect();
        let ms_bl = time_ms(10, || { let _ = base.region_boundary_loops(&faces); });
        let ms_circle = time_ms(10, || {
            let mut m = base.clone();
            let _ = m.circle_region(&faces, None, true, true, 4);
        });
        println!("(iii) grid 60x60 ({} faces) select-all: boundary_loops={:.3}ms  circle_region(relax4)={:.3}ms",
                 base.faces.len(), ms_bl, ms_circle);
    }

    // (iv) redo-panel loop: N re-syncs in a row (the panel re-runs reshape+sync every slider frame).
    // Case A: NO modifier (just circle+raster). Case B: WITH subdiv=4 modifier (the suspected killer).
    {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, 10, 10);
        let faces: HashSet<FaceId> = (0..base.faces.len() as u32).map(FaceId).collect();
        let n = 30u32;

        let t = Instant::now();
        for _ in 0..n {
            let mut m = base.clone();
            let _ = m.circle_region(&faces, None, true, true, 4);
            let _ = raster_data_ngon(&m);
        }
        let a = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        for _ in 0..n {
            let mut m = base.clone();
            let _ = m.circle_region(&faces, None, true, true, 4);
            // apply_poly_modifiers: subdivide x4 then raster
            let mut disp = m.clone();
            for _ in 0..4 { disp.subdivide_catmull_clark(); }
            let _ = raster_data_ngon(&disp);
        }
        let b = t.elapsed().as_secs_f64() * 1000.0;

        println!("(iv)  redo panel {} re-syncs: NO modifier total={:.1}ms ({:.2}ms/frame)  |  WITH subdiv=4 total={:.1}ms ({:.2}ms/frame)",
                 n, a, a / n as f64, b, b / n as f64);
    }
    println!("=== end ===\n");
}
#[test]
fn measure_subdiv_scaling_and_boss() {
    println!("\n=== subdivide scaling + realistic boss drag ===");

    // Scaling check: does subdivide_catmull_clark stay ~linear in output faces?
    for (w, lvl) in [(4u32, 4u32), (10, 3), (20, 3), (30, 3), (40, 2), (60, 2)] {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, w, w);
        let mut m = base.clone();
        let t = Instant::now();
        for _ in 0..lvl { m.subdivide_catmull_clark(); }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let out = m.faces.len();
        println!("subdiv x{lvl} from {}x{} ({} faces) -> {} faces: {:.2}ms ({:.4}ms/kface-out)",
                 w, w, base.faces.len(), out, ms, ms / (out as f64 / 1000.0));
    }

    // Realistic: user selects a face region on a moderately dense body that carries a subdiv=3
    // modifier, then drags the redo-panel slider ~30 frames.
    {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, 30, 30); // 900 faces authored
        let faces: HashSet<FaceId> = (0..base.faces.len() as u32).map(FaceId).collect();
        let n = 30u32;
        let t = Instant::now();
        for _ in 0..n {
            let mut m = base.clone();
            let _ = m.circle_region(&faces, None, true, true, 4);
            let mut disp = m.clone();
            for _ in 0..3 { disp.subdivide_catmull_clark(); }
            let _ = raster_data_ngon(&disp);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("BOSS: 900-face body + subdiv=3 modifier, {n}-frame redo drag = {:.0}ms total ({:.1}ms/frame)",
                 ms, ms / n as f64);
    }

    // AFTER: what a SKIPPED sync now costs — content_signature on the BASE cage instead of the full
    // modifier re-eval. This is what the release+close redo syncs and undo's re-sync of unchanged
    // bodies now pay in place of a subdivide.
    for w in [10u32, 30, 60] {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, w, w);
        let iters = 200u32;
        let t = Instant::now();
        let mut acc = 0u64;
        for _ in 0..iters { acc ^= base.content_signature(); }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        std::hint::black_box(acc);
        println!("SKIP cost: content_signature on {}x{} base ({} faces) = {:.4}ms",
                 w, w, base.faces.len(), ms);
    }
    println!("=== end ===\n");
}
