//! BIT-EXACT PARITY GATE for the poly Smooth (Laplacian) speedups.
//!
//! The optimisations behind `HalfEdgeMesh::smooth_laplacian` and the corner-normal bake are pure
//! cost reductions — flat CSR tables and reused scratch buffers replacing per-element `HashMap` /
//! `Vec` allocations — and they must not move a single float. A perf change that shifts geometry
//! or shading is not a speedup, it is a silent regression: nothing crashes, nothing errors, the
//! model just quietly differs from what the user authored.
//!
//! So each test carries a REFERENCE implementation copied verbatim from the pre-optimisation code
//! and asserts exact `f32` equality (`==`, not an epsilon) against what ships. An epsilon here
//! would hide exactly the class of drift the gate exists to catch.
//!
//! NOT `#[ignore]` — this is a gate, run by `cargo test -p core_math`.

use core_math::half_edge::*;
use glam::Vec3;

// ── meshes that exercise the branches: closed/quad, open/boundary, ngon+triangle mix,
//    and a mesh with holes punched in it so the free-lists are non-empty ──────────────

fn uv_sphere_quads(rings: usize, segs: usize) -> HalfEdgeMesh {
    let mut positions: Vec<Vec3> = Vec::new();
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
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for r in 0..rings {
        for s in 0..segs {
            let s1 = (s + 1) % segs;
            faces.push(vec![
                r * segs + s,
                r * segs + s1,
                (r + 1) * segs + s1,
                (r + 1) * segs + s,
            ]);
        }
    }
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

/// Open grid — every border vertex is a BOUNDARY vertex (fan does not close, no twin).
fn open_grid(n: usize) -> HalfEdgeMesh {
    let mut positions: Vec<Vec3> = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            // Slight height jitter so smoothing actually moves things.
            let h = ((i * 7 + j * 13) % 5) as f32 * 0.11;
            positions.push(Vec3::new(i as f32, h, j as f32));
        }
    }
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let row = n + 1;
    for j in 0..n {
        for i in 0..n {
            faces.push(vec![j * row + i, j * row + i + 1, (j + 1) * row + i + 1, (j + 1) * row + i]);
        }
    }
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

/// Mixed n-gon / triangle / quad fan — non-uniform ring lengths and valences.
fn mixed_ngon() -> HalfEdgeMesh {
    let positions = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.1, 0.0),
        Vec3::new(1.8, 0.0, 0.7),
        Vec3::new(1.2, 0.3, 1.6),
        Vec3::new(0.1, 0.0, 1.9),
        Vec3::new(-0.9, 0.2, 1.1),
        Vec3::new(-1.1, 0.0, 0.1),
        Vec3::new(0.4, 1.4, 0.9),
        Vec3::new(2.6, 0.6, 1.4),
    ];
    let faces = vec![
        vec![0, 1, 2, 3, 4],    // pentagon
        vec![0, 4, 5],          // triangle
        vec![0, 5, 6],          // triangle
        vec![1, 7, 2],          // triangle sharing vertex 1
        vec![2, 7, 3],          // triangle
        vec![2, 8, 3],          // second face across edge 2-3 → higher valence
    ];
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

/// A mesh with deleted faces, so `free_faces` / `free_verts` are non-empty and the liveness
/// checks (`HashSet::contains` → `Vec<bool>` mask) are actually exercised.
fn punched_sphere() -> HalfEdgeMesh {
    let mut m = uv_sphere_quads(8, 12);
    let victims: Vec<FaceId> = (0..m.faces.len() as u32)
        .filter(|i| i % 11 == 3)
        .map(FaceId)
        .collect();
    for f in victims {
        let _ = m.delete_face(f);
    }
    m
}

fn all_meshes() -> Vec<(&'static str, HalfEdgeMesh)> {
    vec![
        ("uv_sphere_quads", uv_sphere_quads(12, 16)),
        ("open_grid", open_grid(9)),
        ("mixed_ngon", mixed_ngon()),
        ("punched_sphere", punched_sphere()),
    ]
}

// ── reference: the ORIGINAL smooth_laplacian positional pass ────────────────────────────

/// The pre-CSR `smooth_laplacian` body, verbatim (HashMap adjacency, per-iteration `Vec` of
/// moved vertices), minus the trailing `recompute_normals()` so this isolates POSITIONS.
fn reference_smooth_positions(mesh: &HalfEdgeMesh, iterations: u32, factor: f32) -> Vec<Vec3> {
    use std::collections::HashMap;
    let mut pos: Vec<Vec3> = mesh.vertices.iter().map(|v| v.position).collect();
    if iterations == 0 || factor.abs() < 1e-9 {
        return pos;
    }
    let mut adj: HashMap<u32, Vec<VertexId>> = HashMap::new();
    for i in 0..mesh.faces.len() as u32 {
        let fid = FaceId(i);
        if mesh.faces[i as usize].half_edge == NO_EDGE {
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
    let mut verts: Vec<VertexId> = adj.keys().copied().map(VertexId).collect();
    verts.sort_by_key(|v| v.0);
    let f = factor.clamp(0.0, 1.0);
    for _ in 0..iterations {
        let next: Vec<(VertexId, Vec3)> = verts
            .iter()
            .filter_map(|&v| {
                let ns = adj.get(&v.0)?;
                if ns.is_empty() {
                    return None;
                }
                let mut sum = Vec3::ZERO;
                for &nb in ns {
                    sum += pos[nb.0 as usize];
                }
                let avg = sum / ns.len() as f32;
                let cur = pos[v.0 as usize];
                Some((v, cur + (avg - cur) * f))
            })
            .collect();
        for (v, p) in next {
            pos[v.0 as usize] = p;
        }
    }
    pos
}

#[test]
fn smooth_laplacian_positions_are_bit_identical_to_the_hashmap_reference() {
    for (name, mesh) in all_meshes() {
        // Iteration counts spanning the panel's 1..=20 slider, and factors at both ends.
        for &(iters, factor) in &[(1u32, 0.5f32), (3, 1.0), (7, 0.25), (20, 0.5)] {
            let expect = reference_smooth_positions(&mesh, iters, factor);
            let mut got = mesh.clone();
            got.smooth_laplacian(iters, factor);

            assert_eq!(
                got.vertices.len(),
                expect.len(),
                "{name}: vertex array length changed"
            );
            for (i, (v, e)) in got.vertices.iter().zip(expect.iter()).enumerate() {
                assert_eq!(
                    v.position.to_array(),
                    e.to_array(),
                    "{name} iters={iters} factor={factor}: vertex {i} moved to {:?}, \
                     reference says {:?} — the CSR adjacency is NOT bit-identical",
                    v.position,
                    e
                );
            }
        }
    }
}

/// `factor = 0` / `iterations = 0` must be exact no-ops, including the normal pass. The early
/// return is what makes a disabled-looking Smooth slot free.
#[test]
fn smooth_laplacian_is_a_no_op_at_zero() {
    for (name, mesh) in all_meshes() {
        for &(iters, factor) in &[(0u32, 0.5f32), (5, 0.0)] {
            let mut got = mesh.clone();
            got.smooth_laplacian(iters, factor);
            for (i, (a, b)) in got.vertices.iter().zip(mesh.vertices.iter()).enumerate() {
                assert_eq!(
                    a.position.to_array(),
                    b.position.to_array(),
                    "{name}: iters={iters} factor={factor} moved vertex {i}"
                );
                assert_eq!(
                    a.normal.to_array(),
                    b.normal.to_array(),
                    "{name}: iters={iters} factor={factor} rewrote normal {i}"
                );
            }
        }
    }
}

// ── reference: the ORIGINAL serial display emitter ─────────────────────────────────────

/// `raster_data_ngon_with_normals` as it was before the parallel rewrite: one face at a time,
/// `base = verts.len()`, material ranges tracked against the growing index buffer.
///
/// The rewrite replaced that running-total bookkeeping with a prefix sum + per-block slices, which
/// is precisely the kind of change that is right on a uniform mesh and wrong at a block boundary,
/// on a degenerate face, or at a material switch. Nothing about the output would look broken in a
/// hash — it would render as a hole or a mis-batched draw — so the reference is the gate.
fn reference_bake(
    mesh: &HalfEdgeMesh,
    cfg: &core_math::poly_mesh::NormalConfig<'_>,
) -> (Vec<[f32; 8]>, Vec<u32>, Vec<(u32, u32, u32)>) {
    use core_math::poly_mesh::{auto_smooth_corner_normal, earclip_polygon};
    let cos_thresh = cfg.cos_thresh();
    let flat = cfg.is_flat();
    let face_normals: Vec<Vec3> = (0..mesh.faces.len())
        .map(|i| {
            let fid = FaceId(i as u32);
            if mesh.face(fid).half_edge == NO_EDGE {
                Vec3::ZERO
            } else {
                mesh.compute_face_normal(fid)
            }
        })
        .collect();

    let mut face_order: Vec<u32> = (0..mesh.faces.len() as u32)
        .filter(|&i| mesh.face(FaceId(i)).half_edge != NO_EDGE)
        .collect();
    face_order.sort_by_key(|&i| mesh.face(FaceId(i)).material_id);

    let mut verts: Vec<[f32; 8]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut ranges: Vec<(u32, u32, u32)> = Vec::new();
    let mut cur_mat: Option<u32> = None;
    let mut range_start: u32 = 0;

    for &fi in &face_order {
        let fid = FaceId(fi);
        let mat = mesh.face(fid).material_id;
        if cur_mat != Some(mat) {
            if let Some(m) = cur_mat {
                let count = indices.len() as u32 - range_start;
                if count > 0 {
                    ranges.push((m, range_start, count));
                }
            }
            cur_mat = Some(mat);
            range_start = indices.len() as u32;
        }
        let fv = mesh.face_vertices(fid);
        if fv.len() < 3 {
            continue;
        }
        let face_n = face_normals[fi as usize];
        let base = verts.len() as u32;
        let pts: Vec<Vec3> = fv.iter().map(|&vid| mesh.vert(vid).position).collect();
        let fhes = mesh.face_half_edges(fid);
        let corner_ns: Vec<Vec3> = if flat {
            let n = if face_n.length_squared() < 1e-12 { Vec3::Y } else { face_n };
            vec![n; fv.len()]
        } else if let Some(custom) = cfg.custom_corner {
            fv.iter()
                .enumerate()
                .map(|(k, _)| {
                    let he = fhes.get(k).copied().unwrap_or(NO_EDGE);
                    let n = custom
                        .get(he.0 as usize)
                        .copied()
                        .filter(|n| n.length_squared() > 1e-12)
                        .unwrap_or(face_n);
                    if n.length_squared() < 1e-12 { Vec3::Y } else { n.normalize_or_zero() }
                })
                .collect()
        } else {
            fv.iter()
                .map(|&vid| {
                    auto_smooth_corner_normal(mesh, &face_normals, vid, fid, cos_thresh, cfg.sharp)
                })
                .collect()
        };
        for (k, p) in pts.iter().enumerate() {
            let n = corner_ns[k];
            verts.push([p.x, p.y, p.z, n.x, n.y, n.z, 0.0, 0.0]);
        }
        for t in earclip_polygon(&pts, face_n) {
            indices.push(base + t[0]);
            indices.push(base + t[1]);
            indices.push(base + t[2]);
        }
    }
    if let Some(m) = cur_mat {
        let count = indices.len() as u32 - range_start;
        if count > 0 {
            ranges.push((m, range_start, count));
        }
    }
    (verts, indices, ranges)
}

fn assert_bake_matches(name: &str, mesh: &HalfEdgeMesh, cfg: &core_math::poly_mesh::NormalConfig<'_>) {
    let (ev, ei, er) = reference_bake(mesh, cfg);
    let (gv, gi, gr) = core_math::poly_mesh::raster_data_ngon_with_normals(mesh, cfg);

    assert_eq!(gv.len(), ev.len(), "{name}: vertex count differs");
    assert_eq!(gi.len(), ei.len(), "{name}: index count differs");
    assert_eq!(gr, er, "{name}: material ranges differ (mis-batched draws)");
    for (i, (a, b)) in gv.iter().zip(ev.iter()).enumerate() {
        for c in 0..8 {
            assert_eq!(
                a[c].to_bits(),
                b[c].to_bits(),
                "{name}: vertex {i} component {c}: parallel {} != serial {}",
                a[c],
                b[c]
            );
        }
    }
    assert_eq!(gi, ei, "{name}: index buffer differs — a block boundary or a short earclip");
}

/// Meshes big enough that `block_bounds` actually splits (the 512-face floor means the small
/// fixtures above all collapse to ONE block and would prove nothing about the parallel path).
#[test]
fn parallel_bake_matches_the_serial_emitter_across_blocks() {
    let cfg = core_math::poly_mesh::NormalConfig::default();

    // 60x60 = 3600 quads → several blocks on any core count.
    let sphere = uv_sphere_quads(60, 60);
    assert!(sphere.face_count() > 1024, "mesh must span multiple blocks");
    assert_bake_matches("sphere_60x60", &sphere, &cfg);

    // Boundary-heavy, and a size that lands awkwardly against the block step.
    assert_bake_matches("open_grid_37", &open_grid(37), &cfg);
}

/// MULTI-MATERIAL is the case the rewrite most easily breaks: ranges used to be measured against
/// the growing index buffer and are now derived from the triangle prefix sum. Every fixture above
/// is single-material, so without this the ranges path is untested.
#[test]
fn parallel_bake_material_ranges_match_across_blocks() {
    let cfg = core_math::poly_mesh::NormalConfig::default();
    let mut m = uv_sphere_quads(50, 50);
    // Several materials, interleaved so the sort genuinely reorders faces, and one material
    // assigned to a single face so a 1-face range is exercised.
    for i in 0..m.faces.len() {
        m.faces[i].material_id = (i % 5) as u32;
    }
    m.faces[7].material_id = 99;
    assert_bake_matches("sphere_5mat", &m, &cfg);

    // Degenerate + deleted faces mixed in: `ring < 3` slots emit nothing but STILL advance the
    // material bookkeeping, which is why the `continue` sits after the switch in both versions.
    let mut punched = uv_sphere_quads(40, 40);
    for i in 0..punched.faces.len() {
        punched.faces[i].material_id = (i % 3) as u32;
    }
    let victims: Vec<FaceId> = (0..punched.faces.len() as u32)
        .filter(|i| i % 13 == 5)
        .map(FaceId)
        .collect();
    for f in victims {
        let _ = punched.delete_face(f);
    }
    assert_bake_matches("punched_3mat", &punched, &cfg);
}

/// The other two normal branches must survive the rewrite too — they take different paths through
/// the same block machinery (Shade Flat and custom corners skip the fan walk entirely).
#[test]
fn parallel_bake_flat_and_custom_corner_branches_match() {
    let mesh = uv_sphere_quads(50, 50);

    let flat = core_math::poly_mesh::NormalConfig {
        angle_deg: 0.0,
        sharp: None,
        custom_corner: None,
    };
    assert_bake_matches("sphere_flat", &mesh, &flat);

    let custom = core_math::half_edge::weighted_corner_normals(&mesh, 180.0, 2, 50, 0.0, None);
    let cc = core_math::poly_mesh::NormalConfig {
        angle_deg: 40.0,
        sharp: None,
        custom_corner: Some(&custom),
    };
    assert_bake_matches("sphere_custom", &mesh, &cc);
}

/// The parallel gather must match the serial one exactly. The kernel forks above a vertex
/// threshold, so a mesh straddling it would otherwise be the only place a divergence shows —
/// and it would show as shading/geometry that changes with mesh size, which is unfalsifiable in
/// the viewport. Compared against the same HashMap reference, above the fork threshold.
#[test]
fn smooth_laplacian_parallel_path_matches_the_reference() {
    // 100 x 100 quads = 10_100 verts, above the 8192-vertex rayon threshold.
    let mesh = uv_sphere_quads(100, 100);
    assert!(
        mesh.vertices.len() > 8192,
        "mesh must exceed the parallel threshold or this test proves nothing"
    );
    let expect = reference_smooth_positions(&mesh, 4, 0.5);
    let mut got = mesh.clone();
    got.smooth_laplacian(4, 0.5);
    for (i, (v, e)) in got.vertices.iter().zip(expect.iter()).enumerate() {
        assert_eq!(
            v.position.to_array(),
            e.to_array(),
            "parallel gather diverged at vertex {i}"
        );
    }
}
