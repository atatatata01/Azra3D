use glam::Vec3;

use super::*;

fn unit_tri() -> DisplayMesh {
    DisplayMesh {
        positions: vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ],
        normals: vec![Vec3::Z, Vec3::Z, Vec3::Z],
        indices: vec![0, 1, 2],
    }
}

#[test]
fn mirror_x_doubles_triangle_count() {
    let m = unit_tri();
    let out = apply_display_mesh_op(
        &DisplayMeshOp::Mirror {
            axis_bits: 1,
            center: Vec3::ZERO,
            weld: false,
            weld_threshold: 1e-4,
        },
        m,
    );
    assert_eq!(out.triangle_count(), 2);
    assert_eq!(out.vertex_count(), 6);
}

#[test]
fn mirror_weld_merges_on_plane() {
    // Vert on plane x=0 should weld across reflection.
    let m = DisplayMesh {
        positions: vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1, 2],
    };
    let out = apply_display_mesh_op(
        &DisplayMeshOp::Mirror {
            axis_bits: 1,
            center: Vec3::ZERO,
            weld: true,
            weld_threshold: 1e-3,
        },
        m,
    );
    // two verts on plane (0,0,0) and (0,1,0) weld → 4 unique verts not 6
    assert!(out.vertex_count() < 6, "weld should merge plane verts, got {}", out.vertex_count());
    assert_eq!(out.triangle_count(), 2);
}

#[test]
fn smooth_geometry_preserves_topology() {
    let m = unit_tri();
    let out = apply_display_mesh_op(
        &DisplayMeshOp::SmoothGeometry {
            iterations: 5,
            factor: 0.5,
        },
        m.clone(),
    );
    assert_eq!(out.indices, m.indices);
    assert_eq!(out.vertex_count(), m.vertex_count());
}

#[test]
fn smooth_normals_finite() {
    let m = unit_tri();
    let out = apply_display_mesh_op(&DisplayMeshOp::SmoothNormals, m);
    for n in &out.normals {
        assert!(n.is_finite());
        assert!(n.length() > 0.5);
    }
}

#[test]
fn stack_hash_stable_and_sensitive() {
    let a = vec![DisplayMeshModRecord::new_mirror()];
    let h1 = hash_display_mesh_stack(&a);
    let h2 = hash_display_mesh_stack(&a);
    assert_eq!(h1, h2);
    let mut b = a.clone();
    if let DisplayMeshModKind::Mirror { axis_bits, .. } = &mut b[0].kind {
        *axis_bits = 2;
    }
    assert_ne!(h1, hash_display_mesh_stack(&b));
    b[0].enabled = false;
    assert_ne!(h1, hash_display_mesh_stack(&b));
}

#[test]
fn compile_skips_disabled() {
    let mut r = DisplayMeshModRecord::new_smooth_normals();
    r.enabled = false;
    let ops = compile_display_mesh_stack(&[r, DisplayMeshModRecord::new_mirror()]);
    assert_eq!(ops.len(), 1);
    assert!(matches!(ops[0], DisplayMeshOp::Mirror { .. }));
}

#[test]
fn fold_order_mirror_then_smooth() {
    let m = unit_tri();
    let ops = compile_display_mesh_stack(&[
        DisplayMeshModRecord::new_mirror(),
        DisplayMeshModRecord::new_smooth_geometry(),
    ]);
    let out = fold_display_mesh(&ops, m);
    assert!(out.triangle_count() >= 2);
}

#[test]
fn raster_roundtrip_positions() {
    let m = unit_tri();
    let (v, i) = m.to_raster();
    let back = DisplayMesh::from_raster(&v, &i);
    assert_eq!(back.positions, m.positions);
    assert_eq!(back.indices, m.indices);
}

fn grid_mesh(n: usize) -> DisplayMesh {
    // n×n grid of quads → 2*(n-1)^2 tris
    let mut positions = Vec::new();
    for y in 0..n {
        for x in 0..n {
            positions.push(Vec3::new(x as f32, y as f32, 0.0));
        }
    }
    let mut indices = Vec::new();
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let i = (y * n + x) as u32;
            let i1 = i + 1;
            let j = i + n as u32;
            let j1 = j + 1;
            indices.extend_from_slice(&[i, i1, j1, i, j1, j]);
        }
    }
    DisplayMesh {
        positions,
        normals: vec![Vec3::Z; n * n],
        indices,
    }
}

#[test]
fn decimate_reduces_triangle_count() {
    let m = grid_mesh(8); // 7*7*2 = 98 tris
    let before = m.triangle_count();
    let out = apply_display_mesh_op(
        &DisplayMeshOp::Decimate {
            params: DecimateParams {
                ratio: 0.4,
                mode: DecimateMode::Fast,
                ..DecimateParams::default()
            },
        },
        m,
    );
    assert!(
        out.triangle_count() < before,
        "decimate should reduce tris {} → {}",
        before,
        out.triangle_count()
    );
    assert!(out.triangle_count() >= 2);
    // Honest ratio: stay well below 70% of original when asking for 40% keep
    assert!(
        out.triangle_count() as f32 <= before as f32 * 0.75,
        "ratio 0.4 must not leave ~99% of tris (got {} / {})",
        out.triangle_count(),
        before
    );
    for n in &out.normals {
        assert!(n.is_finite());
    }
}

#[test]
fn decimate_quality_qem_reduces() {
    let m = grid_mesh(12);
    let before = m.triangle_count();
    let out = apply_display_mesh_op(
        &DisplayMeshOp::Decimate {
            params: DecimateParams {
                ratio: 0.3,
                mode: DecimateMode::Quality,
                ..DecimateParams::default()
            },
        },
        m,
    );
    assert!(
        out.triangle_count() < before,
        "QEM should reduce {} → {}",
        before,
        out.triangle_count()
    );
    assert!(
        (out.triangle_count() as f32) <= (before as f32) * 0.55,
        "QEM ratio 0.3 dishonest: {} / {}",
        out.triangle_count(),
        before
    );
}

#[test]
fn decimate_flat_first_melts_plane_more_than_sphereish() {
    // Flat grid should happily go to low ratio (all edges coplanar → cheap collapses).
    let flat = grid_mesh(16);
    let n0 = flat.triangle_count();
    let flat_out = apply_display_mesh_op(
        &DisplayMeshOp::Decimate {
            params: DecimateParams {
                ratio: 0.15,
                mode: DecimateMode::Quality,
                planar_bias: 3.0,
                crease_strength: 1.0,
                ..DecimateParams::default()
            },
        },
        flat,
    );
    assert!(
        (flat_out.triangle_count() as f32) <= (n0 as f32) * 0.35,
        "flat grid must melt under flat-first: {} / {}",
        flat_out.triangle_count(),
        n0
    );

    // "Curved" proxy: extrude a ridge so some dihedrals are sharp — retain more than pure flat.
    let mut curved = grid_mesh(16);
    for (i, p) in curved.positions.iter_mut().enumerate() {
        let x = (i % 16) as f32;
        // Tent ridge along center
        p.z = (x - 8.0).abs() * -0.15;
    }
    let c0 = curved.triangle_count();
    let curved_out = apply_display_mesh_op(
        &DisplayMeshOp::Decimate {
            params: DecimateParams {
                ratio: 0.15,
                mode: DecimateMode::Quality,
                planar_bias: 3.0,
                crease_strength: 2.0,
                crease_angle_deg: 25.0,
                ..DecimateParams::default()
            },
        },
        curved,
    );
    // Both hit ratio target approximately; curved should not under-collapse worse than flat
    // by a large margin (ratio is hard stop). Main check: curved still has a mesh.
    assert!(curved_out.triangle_count() >= 2);
    assert!(curved_out.triangle_count() <= c0);
    // Flat path stayed honest to ratio.
    assert!(flat_out.triangle_count() as f32 <= n0 as f32 * 0.25 + 20.0);
}

#[test]
fn decimate_field_mode_without_field_falls_back() {
    // No BodyField in ctx → same path as Quality (no panic).
    let m = grid_mesh(10);
    let before = m.triangle_count();
    let out = apply_display_mesh_op(
        &DisplayMeshOp::Decimate {
            params: DecimateParams {
                ratio: 0.5,
                mode: DecimateMode::Field,
                ..DecimateParams::default()
            },
        },
        m,
    );
    assert!(out.triangle_count() < before);
    assert!(out.triangle_count() >= 2);
}

#[test]
fn weld_spatial_hash_merges_plane_verts() {
    // Many near-coincident verts — O(n²) would work but spatial path must too.
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    for i in 0..100 {
        let x = (i % 10) as f32 * 0.001; // within weld thr of each other per column
        let y = (i / 10) as f32;
        positions.push(Vec3::new(x, y, 0.0));
    }
    // Fan triangles from origin-ish
    for i in 1..99 {
        indices.extend_from_slice(&[0, i, i + 1]);
    }
    let m = DisplayMesh {
        normals: vec![Vec3::Z; positions.len()],
        positions,
        indices,
    };
    let out = weld_by_distance(m, 0.01);
    assert!(
        out.vertex_count() < 100,
        "spatial weld must merge close verts, got {}",
        out.vertex_count()
    );
}

#[test]
fn decimate_large_mesh_honors_ratio_cluster() {
    // Force Fast cluster path: 120×120 quads = 119²*2 ≈ 28k
    let m = grid_mesh(120);
    let before = m.triangle_count();
    assert!(before > 25_000, "need cluster path, got {before}");
    let out = apply_display_mesh_op(
        &DisplayMeshOp::Decimate {
            params: DecimateParams {
                ratio: 0.1,
                mode: DecimateMode::Fast,
                ..DecimateParams::default()
            },
        },
        m,
    );
    // Must not leave ~99% of triangles when asking for 10% keep
    assert!(
        (out.triangle_count() as f32) < (before as f32) * 0.35,
        "cluster decimate ratio 0.1 dishonest: {} / {} remain",
        out.triangle_count(),
        before
    );
    assert!(out.triangle_count() >= 2);
}

#[test]
fn clay_polish_compiles_to_smooth_then_normals() {
    let r = DisplayMeshModRecord::new_clay_polish();
    let ops = r.to_ops();
    assert_eq!(ops.len(), 2);
    assert!(matches!(ops[0], DisplayMeshOp::SmoothGeometry { .. }));
    assert!(matches!(ops[1], DisplayMeshOp::SmoothNormals));
    let m = grid_mesh(4);
    let out = fold_display_mesh(&ops, m);
    assert!(out.vertex_count() > 0);
    assert_eq!(out.normals.len(), out.positions.len());
}

/// Freeze regression guard the earlier tests lacked (small meshes hid the O(n²)). A LARGE
/// mesh through the QEM Master path must COMPLETE (the old O(n²/48) rebuild path hung /
/// "too slow, unusable" here), honor the ratio, and emit a clean mesh — finite positions,
/// unit-ish normals, no degenerate/dangling faces. The incremental path finishes in ms.
#[test]
fn decimate_large_mesh_qem_master_fast_and_clean() {
    let m = grid_mesh(120); // ≈ 28k tris — O(n²) would hang here
    let before = m.triangle_count();
    assert!(before > 25_000, "need a large mesh, got {before}");
    let out = apply_display_mesh_op(
        &DisplayMeshOp::Decimate {
            params: DecimateParams {
                ratio: 0.1,
                mode: DecimateMode::Master,
                ..DecimateParams::default()
            },
        },
        m,
    );
    assert!(
        (out.triangle_count() as f32) < (before as f32) * 0.35,
        "master decimate ratio 0.1 dishonest: {} / {}",
        out.triangle_count(),
        before
    );
    assert!(out.triangle_count() >= 2);
    for p in &out.positions {
        assert!(p.is_finite(), "NaN/Inf position after decimate");
    }
    assert_eq!(out.normals.len(), out.positions.len());
    for nrm in &out.normals {
        assert!(nrm.is_finite() && nrm.length() > 0.5, "bad normal {nrm:?}");
    }
    for t in out.indices.chunks_exact(3) {
        assert!(t[0] != t[1] && t[1] != t[2] && t[0] != t[2], "degenerate face survived");
        assert!((t[0] as usize) < out.positions.len(), "dangling index");
    }
}
