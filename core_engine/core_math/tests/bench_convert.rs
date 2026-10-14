//! Where does "Convert to Mesh" spend its ~1137 ms?
//!
//! The owner's soak measured `convert_sdf_to_poly` at 1137 ms of synchronous work plus a 1843 ms
//! frame stall, on a DC mesh of ~98k faces at 256³. This times the CPU stages in isolation so the
//! hot one is identified before anything is optimised.
//!
//! `#[ignore]` — a timing rig. RUN IN --release; this is scalar mesh code and a debug build
//! overstates it enough to point at the wrong stage.

use core_math::half_edge::HalfEdgeMesh;
use glam::Vec3;
use std::time::Instant;

fn ms(f: impl FnOnce()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

/// A closed triangle grid roughly the size of a 256³ DC surface.
fn tri_soup(target_tris: usize) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let n = ((target_tris / 2) as f32).sqrt().ceil() as usize + 1;
    let mut pos = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 / n as f32, y as f32 / n as f32);
            // Mild displacement so no two vertices coincide (welding would change the shape).
            pos.push(Vec3::new(fx, (fx * 12.0).sin() * 0.05 + (fy * 9.0).cos() * 0.05, fy));
        }
    }
    let mut tris = Vec::with_capacity(target_tris);
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let a = (y * n + x) as u32;
            let (b, c, d) = (a + 1, a + n as u32, a + n as u32 + 1);
            tris.push([a, c, b]);
            tris.push([b, c, d]);
        }
    }
    (pos, tris)
}

#[test]
#[ignore = "timing rig - run with --release --ignored --nocapture"]
fn bench_convert_stages() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers point at the wrong stage. Re-run with --release.\n");
    }
    eprintln!("\n=== convert_sdf_to_poly CPU stages (ms) ===");
    for target in [10_000usize, 50_000, 100_000] {
        let (pos, tris) = tri_soup(target);
        let normals = vec![Vec3::Y; pos.len()];

        let mut mesh = None;
        let t_build = ms(|| {
            mesh = Some(HalfEdgeMesh::from_triangle_soup(&pos, &normals, &tris));
        });
        let mesh = mesh.unwrap();

        let t_validate = ms(|| {
            let _ = mesh.validate();
        });
        let t_manifold = ms(|| {
            let _ = mesh.is_manifold();
        });
        let t_raster = ms(|| {
            let _ = mesh.to_raster_data();
        });

        eprintln!(
            "{:>7} tris ({:>6} verts): from_triangle_soup {t_build:>8.1}  validate {t_validate:>7.1}  \
             is_manifold {t_manifold:>7.1}  to_raster {t_raster:>7.1}",
            tris.len(), pos.len()
        );
    }
}
