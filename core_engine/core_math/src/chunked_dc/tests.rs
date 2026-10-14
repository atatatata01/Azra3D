// chunked_dc/tests.rs -- pure relocation from chunked_dc tests module.
// GOD_FILES_SPLIT_PLAN: chunked_dc/{mod,grid,extract,stitch,tests}.rs

use glam::Vec3;

use super::*;

/// Build a simple 4^3-cell grid (5^3 vertices) with a sphere SDF.
fn make_sphere_grid(res: usize, radius: f32) -> Vec<f32> {
    let nverts = res + 1;
    let mut grid = vec![0.0f32; nverts * nverts * nverts];
    for gz in 0..nverts {
        for gy in 0..nverts {
            for gx in 0..nverts {
                // Map [0, nverts-1] to [-1, 1]
                let x = (gx as f32 / res as f32) * 2.0 - 1.0;
                let y = (gy as f32 / res as f32) * 2.0 - 1.0;
                let z = (gz as f32 / res as f32) * 2.0 - 1.0;
                let d = (x * x + y * y + z * z).sqrt() - radius;
                grid[gx + gy * nverts + gz * nverts * nverts] = d;
            }
        }
    }
    grid
}

#[test]
fn test_new_chunked_mesh() {
    let cm = ChunkedMesh::new(128, 32, Vec3::splat(-1.0), Vec3::splat(1.0), true);
    assert_eq!(cm.chunks_per_axis, 4);
    assert_eq!(cm.chunk_size, 32);
    assert_eq!(cm.grid_resolution, 128);
    assert_eq!(cm.chunk_count(), 0);
}

#[test]
fn test_dirty_chunk_coords() {
    let cm = ChunkedMesh::new(128, 32, Vec3::splat(-1.0), Vec3::splat(1.0), true);
    // Dirty region spanning chunk (1,1,1) only
    let coords = cm.dirty_chunk_coords([32, 32, 32], [63, 63, 63]);
    assert_eq!(coords.len(), 1);
    assert_eq!(coords[0], (1, 1, 1));

    // Dirty region spanning 3x3x3 chunks (cells 30..65 touch chunks 0,1,2 per axis)
    let coords = cm.dirty_chunk_coords([30, 30, 30], [65, 65, 65]);
    assert_eq!(coords.len(), 27); // 3x3x3
}

#[test]
fn test_extract_chunk_produces_mesh() {
    let res = 8;
    let grid = make_sphere_grid(res, 0.5);
    let chunk = ChunkedMesh::extract_chunk(
        &grid, res as u32, (0, 0, 0), 8,
        Vec3::splat(-1.0), Vec3::splat(1.0), true, true,
    );
    // A sphere of radius 0.5 in a [-1,1] box should produce some vertices
    assert!(!chunk.vertices.is_empty(), "expected vertices from sphere chunk");
    assert_eq!(chunk.vertices.len(), chunk.normals.len());
}

#[test]
fn repair_fills_zero_normals_with_oriented_geometric() {
    // Reproduces the gray-faces class directly: a quad-referenced vertex whose gradient normal
    // came back ZERO (flat-tap cancellation on a CSG crease/plateau) must be repaired to the
    // chunk's outward geometric normal. Without `repair_degenerate_normals` this stays ZERO →
    // `ndl=0` → flat gray. One CCW quad in the XY plane → geometric normal +Z.
    let verts = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
    ];
    let quads = vec![[0u32, 1, 2, 3]];

    // Vertex 2 is degenerate; the other three carry valid +Z gradient normals.
    let mut normals = vec![Vec3::Z, Vec3::Z, Vec3::ZERO, Vec3::Z];
    repair_degenerate_normals(&verts, &mut normals, &quads);
    assert!((normals[2] - Vec3::Z).length() < 1e-5,
        "zero normal must be repaired to the oriented geometric normal +Z, got {:?}", normals[2]);
    assert_eq!(normals[0], Vec3::Z, "valid normals must be left untouched");

    // Orientation follows the chunk's winding vote: if the valid normals point -Z, so must the
    // repaired one (a chunk's winding is globally consistent, so one sign serves all).
    let mut flipped = vec![Vec3::NEG_Z, Vec3::NEG_Z, Vec3::ZERO, Vec3::NEG_Z];
    repair_degenerate_normals(&verts, &mut flipped, &quads);
    assert!((flipped[2] - Vec3::NEG_Z).length() < 1e-5,
        "repair must align with the winding-vote orientation (-Z), got {:?}", flipped[2]);
}

#[test]
fn repair_is_noop_when_all_normals_valid() {
    // No degenerate normals → the smooth, seamless gradient normals must be preserved EXACTLY
    // (no churn from blending in faceted geometric normals on good surfaces).
    let verts = vec![Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y];
    let quads = vec![[0u32, 1, 2, 3]];
    let original = vec![Vec3::Z, Vec3::Z, Vec3::Z, Vec3::Z];
    let mut normals = original.clone();
    repair_degenerate_normals(&verts, &mut normals, &quads);
    assert_eq!(normals, original, "valid normals must be preserved exactly");
}

#[test]
fn test_extract_all_and_merge() {
    let res = 8;
    let grid = make_sphere_grid(res, 0.5);
    let mut cm = ChunkedMesh::new(res as u32, 4, Vec3::splat(-1.0), Vec3::splat(1.0), true);
    cm.extract_all(&grid);
    assert!(cm.chunk_count() > 0, "expected non-empty chunks from sphere");

    let (verts, norms, quads) = cm.merge_to_mesh();
    assert_eq!(verts.len(), norms.len());
    assert!(!quads.is_empty(), "expected quads from sphere");

    // All quad indices must be in range
    for q in &quads {
        for &idx in q {
            assert!((idx as usize) < verts.len(), "quad index out of range");
        }
    }
}

#[test]
fn test_update_dirty_preserves_clean() {
    let res = 8;
    let grid = make_sphere_grid(res, 0.5);
    let mut cm = ChunkedMesh::new(res as u32, 4, Vec3::splat(-1.0), Vec3::splat(1.0), true);
    cm.extract_all(&grid);
    let count_before = cm.chunk_count();
    let verts_before = cm.total_vertices();

    // "Update" the same grid with a dirty region covering just one chunk
    cm.update_dirty_chunks(&grid, [0, 0, 0], [3, 3, 3]);
    // Chunk count should be the same (we replaced one chunk in-place)
    assert_eq!(cm.chunk_count(), count_before);
    // Vertex count should be approximately the same (same data)
    assert_eq!(cm.total_vertices(), verts_before);
}

/// Verify chunked extraction produces the same quad count as monolithic DC.
/// A mismatch means quads are being dropped (cracks) or duplicated at chunk boundaries.
#[test]
fn test_chunked_matches_monolithic() {
    use crate::mesh::{dual_contouring_from_grid, DcGridConfig};

    let res = 8;
    let grid = make_sphere_grid(res, 0.5);
    let bmin = Vec3::splat(-1.0);
    let bmax = Vec3::splat(1.0);

    // Monolithic DC (single extraction over the full grid)
    let config = DcGridConfig {
        nverts: (res + 1) as u32,
        bounds_min: bmin,
        bounds_max: bmax,
        sharp_features: true,
    };
    let mono = dual_contouring_from_grid(&grid, &config);

    // Chunked DC with 4-cell chunks (2x2x2 = 8 chunks)
    let mut cm = ChunkedMesh::new(res as u32, 4, bmin, bmax, true);
    cm.extract_all(&grid);
    let chunked = cm.to_mesh();

    assert_eq!(
        mono.quads.len(), chunked.quads.len(),
        "quad count mismatch: monolithic={} chunked={} (cracks or duplicates at boundaries)",
        mono.quads.len(), chunked.quads.len()
    );
}

#[test]
fn test_to_mesh() {
    let res = 8;
    let grid = make_sphere_grid(res, 0.5);
    let mut cm = ChunkedMesh::new(res as u32, 4, Vec3::splat(-1.0), Vec3::splat(1.0), true);
    cm.extract_all(&grid);
    let mesh = cm.to_mesh();
    assert!(!mesh.vertices.is_empty());
    assert!(!mesh.quads.is_empty());
    assert_eq!(mesh.vertices.len(), mesh.normals.len());
}

/// Phase 3 parity: BrickGridAccessor must produce identical mesh to FlatGridAccessor.
#[test]
fn test_brick_accessor_parity() {
    let brick_dim: u32 = 8;
    let _brick_voxels: u32 = brick_dim * brick_dim * brick_dim;

    // Use resolution 16 (divisible by brick_dim=8, gives 2 bricks per axis)
    let res: u32 = 16;
    let grid = make_sphere_grid(res as usize, 0.5);
    let bmin = Vec3::splat(-1.0);
    let bmax = Vec3::splat(1.0);

    // Dense extraction
    let flat = FlatGridAccessor::new(&grid, res);
    let mut cm_flat = ChunkedMesh::new(res, 8, bmin, bmax, true);
    cm_flat.extract_all_from(&flat);
    let mesh_flat = cm_flat.to_mesh();

    // Build brick data from the dense grid (simulates classify_bricks_for_body)
    let nverts = res + 1;
    let dims: [u32; 3] = [
        (nverts + brick_dim - 1) / brick_dim,
        (nverts + brick_dim - 1) / brick_dim,
        (nverts + brick_dim - 1) / brick_dim,
    ];
    let total_bricks = (dims[0] * dims[1] * dims[2]) as usize;
    let mut indirection = vec![0xFFFF_FFFFu32; total_bricks];
    let mut cpu_data = Vec::new();
    let mut slot = 0u32;

    for bz in 0..dims[2] {
        for by in 0..dims[1] {
            for bx in 0..dims[0] {
                // Classify: check if any voxel in this brick is near surface
                let mut active = false;
                for lz in 0..brick_dim {
                    for ly in 0..brick_dim {
                        for lx in 0..brick_dim {
                            let gx = bx * brick_dim + lx;
                            let gy = by * brick_dim + ly;
                            let gz = bz * brick_dim + lz;
                            if gx < nverts && gy < nverts && gz < nverts {
                                let val = grid[(gx + gy * nverts + gz * nverts * nverts) as usize];
                                if val.abs() < 1.0 { active = true; }
                            }
                        }
                    }
                }
                if !active { continue; }

                // Allocate brick
                let flat_idx = (bx + by * dims[0] + bz * dims[0] * dims[1]) as usize;
                indirection[flat_idx] = slot;

                // Copy voxel data
                for lz in 0..brick_dim {
                    for ly in 0..brick_dim {
                        for lx in 0..brick_dim {
                            let gx = bx * brick_dim + lx;
                            let gy = by * brick_dim + ly;
                            let gz = bz * brick_dim + lz;
                            let val = if gx < nverts && gy < nverts && gz < nverts {
                                grid[(gx + gy * nverts + gz * nverts * nverts) as usize]
                            } else {
                                100.0
                            };
                            cpu_data.push(val);
                        }
                    }
                }
                slot += 1;
            }
        }
    }

    // Build slot map: sequential slot → dense index (identity in this test)
    let slot_map: std::collections::HashMap<u32, usize> = (0..slot).map(|s| (s, s as usize)).collect();

    // Brick extraction
    let brick = BrickGridAccessor::new(&indirection, &cpu_data, &slot_map, dims, brick_dim, res);
    let mut cm_brick = ChunkedMesh::new(res, 8, bmin, bmax, true);
    cm_brick.extract_all_from(&brick);
    let mesh_brick = cm_brick.to_mesh();

    // Parity checks
    assert_eq!(
        mesh_flat.vertices.len(), mesh_brick.vertices.len(),
        "vertex count mismatch: flat={} brick={}",
        mesh_flat.vertices.len(), mesh_brick.vertices.len()
    );
    assert_eq!(
        mesh_flat.quads.len(), mesh_brick.quads.len(),
        "quad count mismatch: flat={} brick={}",
        mesh_flat.quads.len(), mesh_brick.quads.len()
    );

    // Sort vertices by position for order-independent comparison
    // (HashMap chunk iteration order is non-deterministic)
    let mut vf: Vec<[f32; 3]> = mesh_flat.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let mut vb: Vec<[f32; 3]> = mesh_brick.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    vf.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    vb.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    for (i, (f, b)) in vf.iter().zip(vb.iter()).enumerate() {
        let dist = ((f[0]-b[0]).powi(2) + (f[1]-b[1]).powi(2) + (f[2]-b[2]).powi(2)).sqrt();
        assert!(dist < 1e-6, "vertex {} position mismatch: flat={:?} brick={:?} dist={}", i, f, b, dist);
    }
}

/// Analytic box SDF on a [-1,1]^3 grid. Faces, edges and corners give the
/// sharp-feature stress + a true interior (deeply negative) the sphere lacks.
fn make_box_grid(res: usize, half: f32) -> Vec<f32> {
    let nverts = res + 1;
    let mut grid = vec![0.0f32; nverts * nverts * nverts];
    for gz in 0..nverts {
        for gy in 0..nverts {
            for gx in 0..nverts {
                let p = [
                    (gx as f32 / res as f32) * 2.0 - 1.0,
                    (gy as f32 / res as f32) * 2.0 - 1.0,
                    (gz as f32 / res as f32) * 2.0 - 1.0,
                ];
                let q = [p[0].abs() - half, p[1].abs() - half, p[2].abs() - half];
                let outside = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2) + q[2].max(0.0).powi(2)).sqrt();
                let inside = q[0].max(q[1]).max(q[2]).min(0.0);
                grid[gx + gy * nverts + gz * nverts * nverts] = outside + inside;
            }
        }
    }
    grid
}

/// Build sparse bricks the NAIVE (buggy) way — strict sign-change surface only, NO apron, a SINGLE
/// `+far` sentinel for every non-allocated brick (the pre-Phase-3 single-tag behaviour). Used ONLY
/// to prove the adversarial geometry actually breaks; production uses `classify_grid_to_sparse`.
fn build_naive_bricks(grid: &[f32], nverts: u32, bd: u32) -> SparseSdfBricks {
    let bpa = (nverts + bd - 1) / bd;
    let dims = [bpa, bpa, bpa];
    let total = (dims[0] * dims[1] * dims[2]) as usize;
    let bidx = |bx: u32, by: u32, bz: u32| (bx + by * dims[0] + bz * dims[0] * dims[1]) as usize;
    let g = |gx: u32, gy: u32, gz: u32| -> f32 {
        if gx < nverts && gy < nverts && gz < nverts {
            grid[(gx + gy * nverts + gz * nverts * nverts) as usize]
        } else { BRICK_FAR }
    };
    let mut index = vec![BRICK_TAG_EXTERIOR; total];
    let mut pool: Vec<f32> = Vec::new();
    let mut slot_map = std::collections::HashMap::new();
    let mut slot = 0u32;
    for bz in 0..dims[2] {
        for by in 0..dims[1] {
            for bx in 0..dims[0] {
                let (mut any_neg, mut any_pos) = (false, false);
                for lz in 0..bd { for ly in 0..bd { for lx in 0..bd {
                    if g(bx*bd+lx, by*bd+ly, bz*bd+lz) <= 0.0 { any_neg = true; } else { any_pos = true; }
                }}}
                let bi = bidx(bx, by, bz);
                if any_neg && any_pos { // strict sign-change surface only
                    index[bi] = slot;
                    slot_map.insert(slot, slot as usize);
                    for lz in 0..bd { for ly in 0..bd { for lx in 0..bd {
                        pool.push(g(bx*bd+lx, by*bd+ly, bz*bd+lz));
                    }}}
                    slot += 1;
                } // else: stays EXTERIOR → +far (the bug: interior bricks also read +far)
            }
        }
    }
    SparseSdfBricks { index, pool, slot_map, dims_bricks: dims, brick_dim: bd }
}

fn dc_mesh(sb: &SparseSdfBricks, res: u32, bmin: Vec3, bmax: Vec3) -> crate::mesh::Mesh {
    let acc = sb.accessor(res);
    let mut cm = ChunkedMesh::new(res, 8, bmin, bmax, true);
    cm.extract_all_from(&acc);
    cm.to_mesh()
}

/// SPIKE A (Phase 3.0) — adversarial brick-seam apron parity.
///
/// Geometry: a box whose faces land EXACTLY on brick AND chunk boundaries (the seam
/// the recon flagged), with a genuine deep interior. This is the case the existing
/// `test_brick_accessor_parity` masks (its wide |d|<1.0 classifier incidentally
/// allocates every boundary brick).
///
/// Proves two things:
///   1. The Phase-3 scheme (surface band + 1-ring real apron + signed empty tags)
///      makes sparse DC == dense DC BIT-EXACT (vertex/quad count + sorted positions).
///   2. The naive scheme (tight surface, no apron, single +far sentinel) does NOT
///      match dense — confirming the geometry actually exercises the hazard, so (1)
///      is meaningful rather than vacuous.
#[test]
fn brick_accessor_apron_parity_adversarial() {
    let res: u32 = 24; // 25 verts → 4 bricks/axis (dim 8); chunk size 8 → 3 chunks/axis
    let half = 1.0 / 3.0; // faces at world ±1/3 → grid coords 8 and 16 (brick+chunk boundaries)
    let grid = make_box_grid(res as usize, half);
    let bmin = Vec3::splat(-1.0);
    let bmax = Vec3::splat(1.0);

    // Dense reference.
    let flat = FlatGridAccessor::new(&grid, res);
    let mut cm_flat = ChunkedMesh::new(res, 8, bmin, bmax, true);
    cm_flat.extract_all_from(&flat);
    let mesh_flat = cm_flat.to_mesh();
    assert!(!mesh_flat.quads.is_empty(), "box must mesh");

    // (2) Sanity: the adversarial geometry must break the naive scheme, else (1) proves nothing.
    let mesh_naive = dc_mesh(&build_naive_bricks(&grid, res + 1, 8), res, bmin, bmax);
    assert_ne!(
        mesh_flat.quads.len(), mesh_naive.quads.len(),
        "ADVERSARIAL SANITY FAILED: naive single-sentinel/no-apron matched dense \
         ({} quads) — the test geometry does not exercise the apron hazard, so the \
         parity assertion below would be vacuous.",
        mesh_flat.quads.len()
    );

    // (1) The PRODUCTION classifier (band = brick diagonal) must be bit-exact with dense.
    let band = 8.0 * (2.0 / res as f32) * 3.0f32.sqrt(); // brick diagonal in world units
    let mesh_correct = dc_mesh(&classify_grid_to_sparse(&grid, res + 1, 8, band), res, bmin, bmax);
    assert_eq!(
        mesh_flat.quads.len(), mesh_correct.quads.len(),
        "quad count mismatch (seam crack/dupe): dense={} sparse-correct={}",
        mesh_flat.quads.len(), mesh_correct.quads.len()
    );
    assert_eq!(
        mesh_flat.vertices.len(), mesh_correct.vertices.len(),
        "vertex count mismatch: dense={} sparse-correct={}",
        mesh_flat.vertices.len(), mesh_correct.vertices.len()
    );

    let mut vf: Vec<[f32; 3]> = mesh_flat.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let mut vc: Vec<[f32; 3]> = mesh_correct.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let key = |a: &[f32; 3], b: &[f32; 3]| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
    vf.sort_by(key);
    vc.sort_by(key);
    for (i, (f, c)) in vf.iter().zip(vc.iter()).enumerate() {
        let dist = ((f[0]-c[0]).powi(2) + (f[1]-c[1]).powi(2) + (f[2]-c[2]).powi(2)).sqrt();
        assert!(dist < 1e-6, "vertex {} mismatch at seam: dense={:?} sparse={:?} dist={}", i, f, c, dist);
    }
}

// ====================================================================
// S2.1 — PRE-BAKE prims classifier: SUPERSET parity vs the grid oracle
// ====================================================================

use crate::primitives::{BooleanOp, SdfPrimitive, SdfShape};
use crate::field_eval::eval_scene_body_mirrored;
use crate::sdf_shapes::BoxBevel;

fn sphere_op(pos: Vec3, radius: f32, op: BooleanOp) -> SdfPrimitive {
    let mut p = SdfPrimitive::new("s".into(), SdfShape::Sphere { radius }, pos);
    p.operation = op;
    p
}

fn box_op(pos: Vec3, he: Vec3, op: BooleanOp) -> SdfPrimitive {
    let mut p = SdfPrimitive::new(
        "b".into(),
        SdfShape::Box { half_extents: he, bevel: BoxBevel::default() },
        pos,
    );
    p.operation = op;
    p
}

/// Local dense bake mirroring `bake_body_grid` (app/src/bake_utils.rs): nverts³ vertices,
/// endpoint-inclusive vertex→world mapping, canonical `eval_scene_body_mirrored`. Kept local
/// on purpose — core_math cannot (and must not) import app code.
fn bake_dense_grid(
    prims: &[SdfPrimitive],
    nverts: u32,
    grid_min: Vec3,
    grid_max: Vec3,
    mirror_flags: u8,
    mirror_center: Vec3,
) -> Vec<f32> {
    let visible: Vec<&SdfPrimitive> = prims.iter().filter(|p| p.visible).collect();
    let denom = if nverts > 1 { (nverts - 1) as f32 } else { 1.0 };
    let extent = grid_max - grid_min;
    let mut grid = vec![100.0f32; (nverts as usize).pow(3)];
    for iz in 0..nverts {
        for iy in 0..nverts {
            for ix in 0..nverts {
                let t = Vec3::new(ix as f32 / denom, iy as f32 / denom, iz as f32 / denom);
                let p = grid_min + t * extent;
                grid[(ix + iy * nverts + iz * nverts * nverts) as usize] =
                    eval_scene_body_mirrored(p, &visible, mirror_flags, 0, mirror_center);
            }
        }
    }
    grid
}

/// SUPERSET gate: every brick the post-bake grid classifier STORES must be stored by the
/// pre-bake prims classifier (a miss = a hole in the sparse bake), and sentinel SIGNS must
/// never flip (grid-INTERIOR ⇒ prims not EXTERIOR, grid-EXTERIOR ⇒ prims not INTERIOR — a
/// flipped `±far` constant plants a phantom surface at the brick boundary).
/// Returns (grid_allocated, prims_allocated) for the over-allocation ratio.
fn check_prims_superset(
    name: &str,
    prims: &[SdfPrimitive],
    mirror_flags: u8,
    mirror_center: Vec3,
    grid_min: Vec3,
    grid_max: Vec3,
    nverts: u32,
    bd: u32,
    band: f32,
) -> (usize, usize) {
    let grid = bake_dense_grid(prims, nverts, grid_min, grid_max, mirror_flags, mirror_center);
    let by_grid = classify_grid_to_sparse(&grid, nverts, bd, band);
    let by_prims = classify_prims_to_sparse_index(
        prims, mirror_flags, mirror_center, grid_min, grid_max, nverts, bd, band,
    );

    assert_eq!(by_grid.dims_bricks, by_prims.dims_bricks, "{name}: brick dims must match");
    assert_eq!(by_grid.index.len(), by_prims.index.len(), "{name}: index length must match");
    assert_eq!(
        by_prims.allocated,
        by_prims.index.iter().filter(|&&h| h < BRICK_TAG_INTERIOR).count(),
        "{name}: allocated count must equal the number of handles in the index"
    );

    for (bi, (&hg, &hp)) in by_grid.index.iter().zip(by_prims.index.iter()).enumerate() {
        if hg < BRICK_TAG_INTERIOR {
            assert!(
                hp < BRICK_TAG_INTERIOR,
                "{name}: brick {bi} is stored (surface+apron) by the grid oracle but tagged \
                 {} by the prims classifier — a pre-bake HOLE",
                if hp == BRICK_TAG_INTERIOR { "INTERIOR" } else { "EXTERIOR" },
            );
        } else if hg == BRICK_TAG_INTERIOR {
            assert_ne!(
                hp, BRICK_TAG_EXTERIOR,
                "{name}: brick {bi} is truly INTERIOR (-far) but prims tagged it EXTERIOR \
                 (+far) — sign flip plants a phantom surface at the brick boundary"
            );
        } else {
            assert_ne!(
                hp, BRICK_TAG_INTERIOR,
                "{name}: brick {bi} is truly EXTERIOR (+far) but prims tagged it INTERIOR \
                 (-far) — sign flip plants phantom solid"
            );
        }
    }

    let ga = by_grid.allocated_bricks();
    assert!(ga > 0, "{name}: the oracle stored no bricks — the scene misses the grid, test is vacuous");
    (ga, by_prims.allocated)
}

/// SUPERSET parity across the 4 canonical scenes (sphere / smooth blend / subtract /
/// mirrored). The dense grid baked with the SAME eval + bounds + band is the oracle; the
/// prims classifier must store a superset of its stored set with no sentinel sign flips.
#[test]
fn prims_classifier_superset_of_grid_classifier() {
    // 96 (12 bricks/axis) rather than 64 (8/axis): at 8/axis the 2×diag margin covers the
    // whole domain and the prims classifier stores EVERY brick — superset would hold
    // trivially. At 12/axis it stores a proper subset, so the assertion has teeth.
    let nverts = 96u32;
    let bd = 8u32;
    let gmin = Vec3::splat(-1.5);
    let gmax = Vec3::splat(1.5);
    // Tight band (≈ the 1.5-cell gradient stencil), same convention as the apron-parity test.
    let band = 1.5 * ((gmax - gmin) / (nverts - 1) as f32).max_element();

    // 1. Plain sphere.
    let sphere = vec![sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 })];
    let (g1, p1) = check_prims_superset(
        "sphere", &sphere, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band,
    );

    // 2. Smooth-blend (k > 0) union of two spheres.
    let blend = vec![
        sphere_op(Vec3::new(-0.45, 0.0, 0.0), 0.5, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.45, 0.0, 0.0), 0.5, BooleanOp::Union { k: 0.3 }),
    ];
    let (g2, p2) = check_prims_superset(
        "smooth-blend", &blend, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band,
    );

    // 3. Box minus sphere (smooth subtract carves a fillet).
    let subtract = vec![
        box_op(Vec3::ZERO, Vec3::splat(0.7), BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
    ];
    let (g3, p3) = check_prims_superset(
        "box-minus-sphere", &subtract, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band,
    );

    // 4. MIRRORED body (mirror_flags != 0): off-center box on -X, mirrored across x=0.
    //    Bounds are mirror-expanded (±1.5 covers both halves) per the Body Mirror AABB rule.
    let mirrored = vec![box_op(
        Vec3::new(-0.8, 0.0, 0.0),
        Vec3::splat(0.35),
        BooleanOp::Union { k: 0.0 },
    )];
    let (g4, p4) = check_prims_superset(
        "mirrored", &mirrored, 0b001, Vec3::ZERO, gmin, gmax, nverts, bd, band,
    );
    // The mirror must actually be exercised: the reflected +X half must allocate bricks.
    {
        let grid = bake_dense_grid(&mirrored, nverts, gmin, gmax, 0b001, Vec3::ZERO);
        let by_grid = classify_grid_to_sparse(&grid, nverts, bd, band);
        let dims = by_grid.dims_bricks;
        let has_pos_x = by_grid.index.iter().enumerate().any(|(bi, &h)| {
            h < BRICK_TAG_INTERIOR && (bi as u32 % dims[0]) >= dims[0] / 2 + 1
        });
        assert!(has_pos_x, "mirrored scene must allocate bricks on the reflected +X half");
    }

    println!(
        "superset surface-brick counts (grid oracle -> prims, ratio): \
         sphere {g1}->{p1} ({:.2}x), blend {g2}->{p2} ({:.2}x), \
         subtract {g3}->{p3} ({:.2}x), mirrored {g4}->{p4} ({:.2}x)",
        p1 as f32 / g1 as f32,
        p2 as f32 / g2 as f32,
        p3 as f32 / g3 as f32,
        p4 as f32 / g4 as f32,
    );
}

/// ADVERSARIAL smooth blend — the non-Lipschitz killer: a LARGE-k union whose true surface
/// bulges into the gap, far from either sphere's own surface. The blended field is not the
/// true distance to that bulge, so a classifier trusting per-prim surfaces (or a bare,
/// margin-less band test on 9 samples) misses bricks. The superset property must hold, and
/// the margin must be demonstrably LOAD-BEARING (bare band on the same 9 samples would miss
/// stored bricks — vacuity guard).
#[test]
fn prims_classifier_superset_adversarial_blend() {
    let nverts = 96u32; // 12 bricks/axis — see the superset test for why not 8/axis
    let bd = 8u32;
    let gmin = Vec3::splat(-1.6);
    let gmax = Vec3::splat(1.6);
    let band = 1.5 * ((gmax - gmin) / (nverts - 1) as f32).max_element();

    // Spheres r=0.4 at ±0.6 with Union k=1.0: individual surfaces are 0.2 away from the
    // origin, yet the blend bulge is SOLID there.
    let prims = vec![
        sphere_op(Vec3::new(-0.6, 0.0, 0.0), 0.4, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.6, 0.0, 0.0), 0.4, BooleanOp::Union { k: 1.0 }),
    ];

    // Adversarial sanity: the bulge exists (origin is solid though 0.2 outside each sphere).
    let visible: Vec<&SdfPrimitive> = prims.iter().collect();
    let d_origin = eval_scene_body_mirrored(Vec3::ZERO, &visible, 0, 0, Vec3::ZERO);
    assert!(
        d_origin < 0.0,
        "adversarial sanity: the k=1.0 blend bulge must be solid at the origin \
         (each sphere's own surface is 0.2 away), got d={d_origin}"
    );

    let (ga, pa) = check_prims_superset(
        "adversarial-blend", &prims, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band,
    );

    // VACUITY GUARD: the margin must be load-bearing. Re-run the 9-sample test with NO
    // margin (bare band) and count grid-STORED bricks it would fail to flag as surface —
    // there must be some, otherwise this test proves nothing about the margin.
    let grid = bake_dense_grid(&prims, nverts, gmin, gmax, 0, Vec3::ZERO);
    let by_grid = classify_grid_to_sparse(&grid, nverts, bd, band);
    let dims = by_grid.dims_bricks;
    let denom = (nverts - 1) as f32;
    let extent = gmax - gmin;
    let last = nverts - 1;
    let mut missed_by_bare_band = 0usize;
    for (bi, &h) in by_grid.index.iter().enumerate() {
        if h >= BRICK_TAG_INTERIOR { continue; }
        let bi = bi as u32;
        let bx = bi % dims[0];
        let by = (bi / dims[0]) % dims[1];
        let bz = bi / (dims[0] * dims[1]);
        let v0 = [(bx * bd).min(last) as f32, (by * bd).min(last) as f32, (bz * bd).min(last) as f32];
        let v1 = [
            (bx * bd + bd - 1).min(last) as f32,
            (by * bd + bd - 1).min(last) as f32,
            (bz * bd + bd - 1).min(last) as f32,
        ];
        let world = |g: [f32; 3]| gmin + Vec3::new(g[0], g[1], g[2]) / denom * extent;
        let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
        for c in 0..8u32 {
            let corner = [
                if c & 1 != 0 { v1[0] } else { v0[0] },
                if c & 2 != 0 { v1[1] } else { v0[1] },
                if c & 4 != 0 { v1[2] } else { v0[2] },
            ];
            let d = eval_scene_body_mirrored(world(corner), &visible, 0, 0, Vec3::ZERO);
            mn = mn.min(d);
            mx = mx.max(d);
        }
        let center = [(v0[0] + v1[0]) * 0.5, (v0[1] + v1[1]) * 0.5, (v0[2] + v1[2]) * 0.5];
        let d = eval_scene_body_mirrored(world(center), &visible, 0, 0, Vec3::ZERO);
        mn = mn.min(d);
        mx = mx.max(d);
        if !(mn <= band && mx >= -band) {
            missed_by_bare_band += 1;
        }
    }
    assert!(
        missed_by_bare_band > 0,
        "VACUITY: bare (margin-less) 9-sample band test missed no stored brick — the \
         adversarial scene does not exercise the margin, so the superset pass is vacuous"
    );
    println!(
        "adversarial-blend: grid stored {ga}, prims stored {pa} ({:.2}x); \
         bare-band 9-sample test would miss {missed_by_bare_band} stored bricks (margin is load-bearing)",
        pa as f32 / ga as f32,
    );
}

/// The shared 512³ timing scene — 6 prims: base box + blended spheres + smooth subtract +
/// torus + cylinder cutter. ONE definition for every `*_timing_512` probe so the numbers
/// stay comparable across probes.
fn timing_scene_6prims() -> Vec<SdfPrimitive> {
    vec![
        box_op(Vec3::ZERO, Vec3::new(0.8, 0.4, 0.8), BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(-0.5, 0.4, 0.0), 0.35, BooleanOp::Union { k: 0.25 }),
        sphere_op(Vec3::new(0.5, 0.4, 0.0), 0.35, BooleanOp::Union { k: 0.25 }),
        sphere_op(Vec3::new(0.0, 0.6, 0.0), 0.3, BooleanOp::Subtract { k: 0.15 }),
        {
            let mut p = SdfPrimitive::new(
                "t".into(),
                SdfShape::Torus { major_radius: 0.5, minor_radius: 0.12 },
                Vec3::new(0.0, -0.3, 0.0),
            );
            p.operation = BooleanOp::Union { k: 0.1 };
            p
        },
        {
            let mut p = SdfPrimitive::new(
                "c".into(),
                SdfShape::Cylinder { radius: 0.15, half_height: 1.0 },
                Vec3::new(0.0, 0.0, 0.4),
            );
            p.operation = BooleanOp::Subtract { k: 0.0 };
            p
        },
    ]
}

/// Settle-bake budget probe: wall-time of the pre-bake classifier at production resolution.
/// Run with: `cargo test -p core_math classify_prims_timing -- --ignored --nocapture`
#[test]
#[ignore]
fn classify_prims_timing_512() {
    let nverts = 512u32;
    let bd = 8u32; // 64³ = 262,144 bricks × 9 evals ≈ 2.36M scene evals
    let gmin = Vec3::splat(-1.6);
    let gmax = Vec3::splat(1.6);
    let band = 1.5 * ((gmax - gmin) / (nverts - 1) as f32).max_element();
    let prims = timing_scene_6prims();

    for (label, mf) in [("no mirror", 0u8), ("mirror X", 0b001u8)] {
        let t0 = std::time::Instant::now();
        let sp = classify_prims_to_sparse_index(&prims, mf, Vec3::ZERO, gmin, gmax, nverts, bd, band);
        let dt = t0.elapsed();
        let total = (sp.dims_bricks[0] * sp.dims_bricks[1] * sp.dims_bricks[2]) as usize;
        println!(
            "classify_prims_to_sparse_index nverts=512 bd=8 ({total} bricks, 6 prims, {label}): \
             {dt:?} — stored {} bricks ({:.1}%)",
            sp.allocated,
            100.0 * sp.allocated as f32 / total as f32,
        );
    }
}

/// 3A-perf cost probe: absolute wall-time of the HIERARCHICAL full classify at production
/// resolution, plus the superblock verdict breakdown (how much of the grid the proofs
/// skip). There is no "before" arm to run — the pre-hierarchical cost is the
/// owner-measured 23-33ms `[SparseFull]` probe this pass targets.
/// Run with: `cargo test -p core_math hierarchical_classify_timing_512 -- --ignored --nocapture`
#[test]
#[ignore]
fn hierarchical_classify_timing_512() {
    let nverts = 512u32;
    let bd = 8u32; // 64³ = 262,144 bricks = 16³ superblocks
    let (gmin, gmax) = (Vec3::splat(-1.6), Vec3::splat(1.6));
    let band = 1.5 * ((gmax - gmin) / (nverts - 1) as f32).max_element();
    let prims = timing_scene_6prims();

    for (label, mf) in [("no mirror", 0u8), ("mirror X", 0b001u8)] {
        let clf = BrickClassifier::new(&prims, mf, Vec3::ZERO, gmin, gmax, nverts, bd, band);
        let (verdicts, sdims) = clf.superblock_verdicts();
        let n_ext = verdicts.iter().filter(|v| **v == Some((false, false))).count();
        let n_int = verdicts.iter().filter(|v| **v == Some((false, true))).count();
        let stotal = (sdims[0] * sdims[1] * sdims[2]) as usize;

        let t0 = std::time::Instant::now();
        let sp = classify_prims_to_sparse_index(&prims, mf, Vec3::ZERO, gmin, gmax, nverts, bd, band);
        let dt = t0.elapsed();
        let total = (sp.dims_bricks[0] * sp.dims_bricks[1] * sp.dims_bricks[2]) as usize;
        println!(
            "hierarchical classify nverts=512 bd=8 ({total} bricks, 6 prims, {label}): {dt:?} — \
             stored {} bricks; superblocks {stotal}: {n_ext} proven-exterior, \
             {n_int} proven-interior, {} per-brick",
            sp.allocated,
            stotal - n_ext - n_int,
        );
    }
}

/// S2.5 — the frozen-index partial window check + its one-brick dilation, unit-tested on a
/// hand-built index (no GPU): handled windows pass, any sentinel (EXTERIOR or INTERIOR)
/// overlap fails, dilation clamps at the grid edge, out-of-range bricks are conservative.
#[test]
fn sparse_window_check_and_dilation() {
    // 3×3×3 brick grid (bd=8 → res 24). Handles everywhere except two sentinels:
    // EXTERIOR at brick (2,0,0), INTERIOR at brick (0,2,2).
    let dims = [3u32, 3, 3];
    let bidx = |bx: u32, by: u32, bz: u32| (bx + by * 3 + bz * 9) as usize;
    let mut index: Vec<u32> = (0..27).collect();
    index[bidx(2, 0, 0)] = BRICK_TAG_EXTERIOR;
    index[bidx(0, 2, 2)] = BRICK_TAG_INTERIOR;
    let res = [24u32; 3];

    // Fully-handled window (bricks (0..2, 0..2, 0..2) minus the sentinels' bricks).
    assert!(sparse_window_bricks_all_handled(&index, dims, 8, [0, 0, 0], [16, 16, 16]));
    // Window touching the EXTERIOR brick (voxel x=16 → brick 2) → escalate.
    assert!(!sparse_window_bricks_all_handled(&index, dims, 8, [10, 0, 0], [17, 8, 8]));
    // Window touching the INTERIOR brick → escalate (both sentinel kinds must fail).
    assert!(!sparse_window_bricks_all_handled(&index, dims, 8, [0, 16, 16], [8, 24, 24]));
    // Single-voxel window inside a handled brick.
    assert!(sparse_window_bricks_all_handled(&index, dims, 8, [9, 9, 9], [10, 10, 10]));
    // Empty window is vacuously true.
    assert!(sparse_window_bricks_all_handled(&index, dims, 8, [5, 5, 5], [5, 5, 5]));
    // Truncated index (out-of-range brick) is conservative → escalate.
    assert!(!sparse_window_bricks_all_handled(&index[..26], dims, 8, [16, 16, 16], [24, 24, 24]));

    // Dilation: one brick each side, clamped to [0, res).
    let (dmin, dmax) = dilate_voxel_window([9, 0, 20], [10, 4, 24], 8, res);
    assert_eq!(dmin, [1, 0, 12]);
    assert_eq!(dmax, [18, 12, 24]);
    // Dilation makes a window one voxel from a sentinel brick SEE that brick:
    // undilated [8..16) on x stays clear of brick 2; dilated reaches it.
    assert!(sparse_window_bricks_all_handled(&index, dims, 8, [8, 0, 0], [16, 8, 8]));
    let (dmin, dmax) = dilate_voxel_window([8, 0, 0], [16, 8, 8], 8, res);
    assert!(!sparse_window_bricks_all_handled(&index, dims, 8, dmin, dmax));
}

// ====================================================================
// S2.5b — tag-compatibility window check (mini-classify vs frozen tag)
// ====================================================================

/// World AABB → voxel window, trigger_bake's mapping (floor for the inclusive min,
/// ceil + 1 for the exclusive max, clamped) — shared by the S2.5b window tests.
fn world_to_voxel_window(
    wmin: Vec3, wmax: Vec3, gmin: Vec3, gmax: Vec3, nverts: u32,
) -> ([u32; 3], [u32; 3]) {
    let denom = (nverts - 1) as f32;
    let t0 = (wmin - gmin) / (gmax - gmin);
    let t1 = (wmax - gmin) / (gmax - gmin);
    (
        [
            (t0.x * denom).floor().max(0.0) as u32,
            (t0.y * denom).floor().max(0.0) as u32,
            (t0.z * denom).floor().max(0.0) as u32,
        ],
        [
            ((t1.x * denom).ceil() as u32 + 1).min(nverts),
            ((t1.y * denom).ceil() as u32 + 1).min(nverts),
            ((t1.z * denom).ceil() as u32 + 1).min(nverts),
        ],
    )
}

/// Refactor bit-parity: [`BrickClassifier::classify_one_brick`] (the extracted Pass-1
/// predicate) must be CONSISTENT with the index [`classify_prims_to_sparse_index`] emits —
/// sentinels appear exactly where the per-brick class says Interior/Exterior (and dilation
/// didn't store), and every Surface brick carries a handle. Swept over TWO scenes because
/// no single centered solid can have all three classes live at 128³/±2 (the 2×diag margin
/// forces `he ≥ 1.42` for INTERIOR sentinels but `he < 1.25` for EXTERIOR ones): the deep
/// box covers Surface+Interior, the small far sphere covers Surface+Exterior. Together with
/// the untouched superset gates this pins that the refactor did not change the emitted
/// index.
#[test]
fn classify_one_brick_consistent_with_emitted_index() {
    let nverts = 128u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let band = 1.5 * ((gmax - gmin) / (nverts - 1) as f32).max_element();

    let sweep = |name: &str, prims: &[SdfPrimitive]| -> (usize, usize, usize) {
        let sp = classify_prims_to_sparse_index(prims, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
        let clf = BrickClassifier::new(prims, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
        assert_eq!(clf.dims_bricks(), sp.dims_bricks, "{name}: dims mismatch");
        let dims = sp.dims_bricks;
        let (mut n_srf, mut n_int, mut n_ext) = (0usize, 0usize, 0usize);
        for bz in 0..dims[2] {
            for by in 0..dims[1] {
                for bx in 0..dims[0] {
                    let bi = (bx + by * dims[0] + bz * dims[0] * dims[1]) as usize;
                    let tag = sp.index[bi];
                    let class = clf.classify_one_brick(bx, by, bz);
                    match class {
                        BrickClass::Surface => {
                            n_srf += 1;
                            assert!(tag < BRICK_TAG_INTERIOR,
                                "{name}: brick ({bx},{by},{bz}) classifies Surface but has no handle");
                        }
                        BrickClass::Interior => n_int += 1,
                        BrickClass::Exterior => n_ext += 1,
                    }
                    // Sentinels only ever come from the exactly-matching class (dilation
                    // may promote Interior/Exterior bricks to handles, never to the WRONG
                    // sentinel).
                    match tag {
                        BRICK_TAG_INTERIOR => assert_eq!(class, BrickClass::Interior,
                            "{name}: INTERIOR sentinel at ({bx},{by},{bz}) but per-brick class is {class:?}"),
                        BRICK_TAG_EXTERIOR => assert_eq!(class, BrickClass::Exterior,
                            "{name}: EXTERIOR sentinel at ({bx},{by},{bz}) but per-brick class is {class:?}"),
                        _ => {}
                    }
                }
            }
        }
        (n_srf, n_int, n_ext)
    };

    let deep_box = vec![box_op(Vec3::ZERO, Vec3::splat(1.45), BooleanOp::Union { k: 0.0 })];
    let (s1, i1, e1) = sweep("deep-box", &deep_box);
    assert!(s1 > 0 && i1 > 0, "deep-box must have live Surface+Interior (got {s1}/{i1}/{e1})");

    let far_sphere = vec![sphere_op(Vec3::splat(-1.2), 0.3, BooleanOp::Union { k: 0.0 })];
    let (s2, i2, e2) = sweep("far-sphere", &far_sphere);
    assert!(s2 > 0 && e2 > 0, "far-sphere must have live Surface+Exterior (got {s2}/{i2}/{e2})");
}

/// Stage-2 finalize-split pin: [`sparse_index_from_verdicts`] (the extracted dilation +
/// handle-allocation phase, the GPU-classify entry point) fed the brute-forced per-brick
/// [`BrickClassifier::classify_one_brick`] verdicts must be BIT-IDENTICAL to the full
/// [`classify_prims_to_sparse_index`] (whose superblock/containment-proof passes are
/// flag-identical speedups). Scenes exercise all three verdicts (Surface+Interior and
/// Surface+Exterior), plus a mirrored body and a subtract blend.
#[test]
fn sparse_index_from_verdicts_matches_full_classify() {
    let nverts = 128u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let band = 1.5 * ((gmax - gmin) / (nverts - 1) as f32).max_element();

    let check = |name: &str, prims: &[SdfPrimitive], mf: u8| {
        let full = classify_prims_to_sparse_index(prims, mf, Vec3::ZERO, gmin, gmax, nverts, bd, band);
        let clf = BrickClassifier::new(prims, mf, Vec3::ZERO, gmin, gmax, nverts, bd, band);
        let dims = clf.dims_bricks();
        let total = (dims[0] * dims[1] * dims[2]) as usize;
        let verdicts: Vec<BrickClass> = (0..total as u32).map(|bi| {
            clf.classify_one_brick(bi % dims[0], (bi / dims[0]) % dims[1], bi / (dims[0] * dims[1]))
        }).collect();
        let split = sparse_index_from_verdicts(&verdicts, dims);
        assert!(split.allocated > 0, "{name}: vacuous scene — nothing allocated");
        assert_eq!(split.allocated, full.allocated, "{name}: allocated diverged");
        assert_eq!(split.dims_bricks, full.dims_bricks, "{name}: dims diverged");
        assert_eq!(split.index, full.index, "{name}: finalize-split index not bit-identical");
        assert!(split.freed.is_empty() && full.freed.is_empty(), "{name}: fresh classify resets freed");
    };

    check("deep-box", &[box_op(Vec3::ZERO, Vec3::splat(1.45), BooleanOp::Union { k: 0.0 })], 0);
    check("far-sphere", &[sphere_op(Vec3::splat(-1.2), 0.3, BooleanOp::Union { k: 0.0 })], 0);
    check("subtract-blend", &[
        sphere_op(Vec3::ZERO, 0.9, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.8, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
    ], 0);
    check("mirrored", &[sphere_op(Vec3::new(-1.0, 0.0, 0.0), 0.4, BooleanOp::Union { k: 0.0 })], 0b001);
}

/// S2.5b REPRO — the owner-measured flaw: a small nudge of a SOLID whose deep inside is
/// INTERIOR-tagged by design. The old sentinel-absence rule escalates (this is the pinned
/// pre-fix behavior: every drag frame → full classify+bake); the new tag-compatibility rule
/// says partial-OK because the window's INTERIOR sentinels still mini-classify Interior for
/// the moved field. (The GPU half — dispatching the windowed partial and decoding against
/// the moved-scene oracle — lives in `app::sdf_gpu_mesh::tests_sparse_bake`.)
#[test]
fn window_tag_compat_interior_inside_solid_is_partial_ok() {
    let nverts = 128u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let cell = ((gmax - gmin) / (nverts - 1) as f32).max_element();
    let band = 1.5 * cell;
    // Deep box: interior core beyond the 2×diag margin + dilation ring → INTERIOR sentinels
    // INSIDE the prim (the dragged-cube case).
    let he = Vec3::splat(1.45);
    let nudge = Vec3::new(0.04, 0.0, 0.0); // ≈ 1.3 voxels — a drag-frame step
    let round1 = vec![box_op(Vec3::ZERO, he, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![box_op(nudge, he, BooleanOp::Union { k: 0.0 })];

    let sp = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let total = (sp.dims_bricks[0] * sp.dims_bricks[1] * sp.dims_bricks[2]) as usize;
    let n_int = sp.index.iter().filter(|&&t| t == BRICK_TAG_INTERIOR).count();
    assert!(sp.allocated < total && n_int > 0,
        "premise: sentinel INTERIOR bricks must exist inside the solid (allocated={} total={total} interior={n_int})",
        sp.allocated);

    // Dirty window = old ∪ new AABB + 2-voxel margin (mark_prim_dirty_aabb semantics),
    // voxel-mapped and brick-dilated exactly as trigger_bake does.
    let margin = Vec3::splat(2.0 * cell);
    let (win_min, win_max) = world_to_voxel_window(
        (-he).min(-he + nudge) - margin, he.max(he + nudge) + margin, gmin, gmax, nverts);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, bd, [nverts; 3]);
    // The window must actually contain INTERIOR sentinels (what trips the old rule) …
    let (dx, dy) = (sp.dims_bricks[0] as usize, sp.dims_bricks[1] as usize);
    let mut int_in_window = 0usize;
    for bz in dil_min[2] / bd..=(dil_max[2] - 1) / bd {
        for by in dil_min[1] / bd..=(dil_max[1] - 1) / bd {
            for bx in dil_min[0] / bd..=(dil_max[0] - 1) / bd {
                if sp.index[bx as usize + by as usize * dx + bz as usize * dx * dy] == BRICK_TAG_INTERIOR {
                    int_in_window += 1;
                }
            }
        }
    }
    assert!(int_in_window > 0, "premise: dilated window must cover INTERIOR sentinels");

    // PRE-FIX behavior, pinned: sentinel absence escalates on the by-design INTERIOR bricks.
    assert!(
        !sparse_window_bricks_all_handled(&sp.index, sp.dims_bricks, bd, dil_min, dil_max),
        "old S2.5 rule unexpectedly passed — the repro scene no longer covers the flaw"
    );
    // POST-FIX behavior: tag compatibility says partial-OK for the same window.
    let clf = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let verdict = sparse_window_tags_compatible(
        &sp.index, sp.dims_bricks, bd, dil_min, dil_max, &clf);
    assert!(verdict.is_ok(),
        "S2.5b must allow the partial (INTERIOR sentinels still needed Interior): {verdict:?}");
}

/// S2.5b escalation stays REAL: the surface moving into a formerly-EXTERIOR brick (the
/// genuine frontier cross) must escalate under the new rule, with Surface-vs-EXTERIOR
/// evidence.
#[test]
fn window_tag_compat_escalates_surface_into_sentinel() {
    let nverts = 128u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let cell = ((gmax - gmin) / (nverts - 1) as f32).max_element();
    let band = 1.5 * cell;
    let (p0, p1, r) = (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), 0.4f32);
    let round1 = vec![sphere_op(p0, r, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![sphere_op(p1, r, BooleanOp::Union { k: 0.0 })];

    let sp = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    assert!(sp.index.iter().any(|&t| t == BRICK_TAG_EXTERIOR),
        "premise: EXTERIOR sentinels must exist");

    let margin = Vec3::splat(2.0 * cell);
    let (win_min, win_max) = world_to_voxel_window(
        (p0 - Vec3::splat(r)).min(p1 - Vec3::splat(r)) - margin,
        (p0 + Vec3::splat(r)).max(p1 + Vec3::splat(r)) + margin,
        gmin, gmax, nverts);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, bd, [nverts; 3]);

    let clf = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let inc = sparse_window_tags_compatible(&sp.index, sp.dims_bricks, bd, dil_min, dil_max, &clf)
        .expect_err("surface entered formerly-EXTERIOR bricks — MUST escalate");
    assert_eq!(inc.needed, BrickClass::Surface, "escalation evidence: {inc:?}");
    assert_eq!(inc.frozen_tag, Some(BRICK_TAG_EXTERIOR), "escalation evidence: {inc:?}");
}

/// S2.5b sign-flip escalations: a window brick whose needed sign contradicts its frozen
/// sentinel (no handle to absorb the write) must escalate — BOTH directions:
/// EXTERIOR-tagged brick that now needs Interior, and INTERIOR-tagged brick that now needs
/// Exterior. A wrong `∓far` constant would plant a phantom wall / phantom solid.
#[test]
fn window_tag_compat_escalates_on_sign_flip() {
    let nverts = 128u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let band = 1.5 * ((gmax - gmin) / (nverts - 1) as f32).max_element();
    // Scene A: tiny far-corner sphere → the grid center is EXTERIOR-tagged.
    let far_sphere = vec![sphere_op(Vec3::splat(-1.2), 0.3, BooleanOp::Union { k: 0.0 })];
    // Scene B: deep box → the grid center needs Interior (beyond band + margin).
    let deep_box = vec![box_op(Vec3::ZERO, Vec3::splat(1.45), BooleanOp::Union { k: 0.0 })];
    // Window: the central brick block only.
    let (win_min, win_max) = ([56u32; 3], [72u32; 3]);

    // Direction 1: frozen EXTERIOR, now needs Interior.
    let sp_a = classify_prims_to_sparse_index(&far_sphere, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let center_bi = (8 + 8 * sp_a.dims_bricks[0] + 8 * sp_a.dims_bricks[0] * sp_a.dims_bricks[1]) as usize;
    assert_eq!(sp_a.index[center_bi], BRICK_TAG_EXTERIOR, "premise: center EXTERIOR in scene A");
    let clf_b = BrickClassifier::new(&deep_box, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let inc = sparse_window_tags_compatible(&sp_a.index, sp_a.dims_bricks, bd, win_min, win_max, &clf_b)
        .expect_err("EXTERIOR brick now needing Interior MUST escalate (+far would plant a wall)");
    assert_eq!(inc.needed, BrickClass::Interior, "escalation evidence: {inc:?}");
    assert_eq!(inc.frozen_tag, Some(BRICK_TAG_EXTERIOR), "escalation evidence: {inc:?}");

    // Direction 2: frozen INTERIOR, now needs Exterior.
    let sp_b = classify_prims_to_sparse_index(&deep_box, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    assert_eq!(sp_b.index[center_bi], BRICK_TAG_INTERIOR, "premise: center INTERIOR in scene B");
    let clf_a = BrickClassifier::new(&far_sphere, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let inc = sparse_window_tags_compatible(&sp_b.index, sp_b.dims_bricks, bd, win_min, win_max, &clf_a)
        .expect_err("INTERIOR brick now needing Exterior MUST escalate (-far would plant solid)");
    assert_eq!(inc.needed, BrickClass::Exterior, "escalation evidence: {inc:?}");
    assert_eq!(inc.frozen_tag, Some(BRICK_TAG_INTERIOR), "escalation evidence: {inc:?}");
}

/// S2.5b cost probe at production scale: full classify vs the window tag check for a
/// typical drag window, same build profile.
/// Run with: `cargo test -p core_math window_tag_check_timing_512 -- --ignored --nocapture`
#[test]
#[ignore]
fn window_tag_check_timing_512() {
    let nverts = 512u32;
    let bd = 8u32; // 64³ = 262,144 bricks
    let (gmin, gmax) = (Vec3::splat(-1.6), Vec3::splat(1.6));
    let cell = ((gmax - gmin) / (nverts - 1) as f32).max_element();
    let band = 1.5 * cell;
    // The shared 6-prim timing scene.
    let prims = timing_scene_6prims();

    let t0 = std::time::Instant::now();
    let sp = classify_prims_to_sparse_index(&prims, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let t_full = t0.elapsed();

    // Typical drag window: the -X sphere prim's AABB + a small nudge + 2-voxel margin.
    let (p0, r) = (Vec3::new(-0.5, 0.4, 0.0), 0.35f32);
    let nudge = Vec3::new(0.01, 0.0, 0.0);
    let margin = Vec3::splat(2.0 * cell);
    let (win_min, win_max) = world_to_voxel_window(
        (p0 - Vec3::splat(r)).min(p0 + nudge - Vec3::splat(r)) - margin,
        (p0 + Vec3::splat(r)).max(p0 + nudge + Vec3::splat(r)) + margin,
        gmin, gmax, nverts);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, bd, [nverts; 3]);
    let n_bricks: u64 = (0..3).map(|a| ((dil_max[a] - 1) / bd - dil_min[a] / bd + 1) as u64).product();
    let (dx, dy) = (sp.dims_bricks[0] as usize, sp.dims_bricks[1] as usize);
    let mut n_sentinel = 0usize;
    for bz in dil_min[2] / bd..=(dil_max[2] - 1) / bd {
        for by in dil_min[1] / bd..=(dil_max[1] - 1) / bd {
            for bx in dil_min[0] / bd..=(dil_max[0] - 1) / bd {
                if sp.index[bx as usize + by as usize * dx + bz as usize * dx * dy] >= BRICK_TAG_INTERIOR {
                    n_sentinel += 1;
                }
            }
        }
    }

    let prims_moved: Vec<SdfPrimitive> = {
        let mut p = prims.clone();
        p[1].position += nudge;
        p
    };
    let clf = BrickClassifier::new(&prims_moved, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let t1 = std::time::Instant::now();
    let verdict = sparse_window_tags_compatible(&sp.index, sp.dims_bricks, bd, dil_min, dil_max, &clf);
    let t_win = t1.elapsed();
    println!(
        "512³/64³ bricks: full classify {t_full:?} (allocated {}), window tag check {t_win:?} \
         ({n_bricks} window bricks, {n_sentinel} sentinels mini-classified) → verdict {verdict:?}",
        sp.allocated,
    );
}

// ================================================================
// P1 scrolling-grid escape — sparse_index_translate
// ================================================================

/// Mechanics on a hand-built index: retained mapping, provisional EXTERIOR fill,
/// scrolled-out handle freeing (deterministic, appended to existing freed), watermark
/// unchanged. Dims are deliberately non-cubic (4×3×2) to catch axis-order mistakes.
#[test]
fn sparse_index_translate_mechanics() {
    let dims = [4u32, 3, 2];
    let lin = |bx: u32, by: u32, bz: u32| (bx + by * 4 + bz * 12) as usize;
    let mut index = vec![BRICK_TAG_EXTERIOR; 24];
    // Handles 0..3 at known coords + one INTERIOR sentinel.
    index[lin(0, 0, 0)] = 0;
    index[lin(3, 1, 0)] = 1;
    index[lin(1, 2, 1)] = 2;
    index[lin(2, 1, 1)] = BRICK_TAG_INTERIOR;
    let sp = SparseIndexOnly { index, dims_bricks: dims, allocated: 3, freed: vec![7] };

    // Shift +1 brick in x: window origin moves +x, so world content lands at bx-1.
    let t = sparse_index_translate(&sp, [1, 0, 0]);
    assert_eq!(t.dims_bricks, dims);
    assert_eq!(t.allocated, 3, "watermark never changes on translate");
    // Handle 0 was at bx=0 → destination bx=-1 → scrolled out → freed (after existing 7).
    assert_eq!(t.freed, vec![7, 0]);
    // Handle 1 (3,1,0) → (2,1,0); handle 2 (1,2,1) → (0,2,1); INTERIOR (2,1,1) → (1,1,1).
    assert_eq!(t.index[lin(2, 1, 0)], 1);
    assert_eq!(t.index[lin(0, 2, 1)], 2);
    assert_eq!(t.index[lin(1, 1, 1)], BRICK_TAG_INTERIOR);
    // Exposed slab (bx=3, the src side that left range) is provisional EXTERIOR.
    for by in 0..3 {
        for bz in 0..2 {
            assert_eq!(t.index[lin(3, by, bz)], BRICK_TAG_EXTERIOR,
                "exposed slab must be provisional EXTERIOR");
        }
    }
    // Multi-axis negative shift: origin moves (-1,+1,0) bricks → content moves (+1,-1,0).
    // Handle 0 (0,0,0)→(1,-1,0) out low in y; handle 1 (3,1,0)→(4,0,0) out high in x —
    // both freed, in (z,y,x) scan order of their SOURCE coords: 0 then 1.
    let t2 = sparse_index_translate(&sp, [-1, 1, 0]);
    assert_eq!(t2.freed, vec![7, 0, 1]);
    assert_eq!(t2.index[lin(2, 1, 1)], 2, "(1,2,1)→(2,1,1)");
    assert_eq!(t2.index[lin(3, 0, 1)], BRICK_TAG_INTERIOR, "(2,1,1)→(3,0,1)");
    assert_eq!(
        t2.index.iter().filter(|&&t| t < BRICK_TAG_INTERIOR).count(), 1,
        "exactly one handle retained after the multi-axis shift"
    );
}

/// The coordinate-convention gold test: translate a REAL classify vs a fresh classify at the
/// brick-shifted bounds. Every retained brick (source in range) must agree in CLASS
/// (handle ⁄ INTERIOR ⁄ EXTERIOR) — handle NUMBERS differ (oracle dense-packs; translate
/// preserves originals), classes must not. The exposed slab is excluded (provisional by
/// contract — the window re-classify owns it). This is the test that catches a `d+shift` vs
/// `d-shift` sign mistake: with the sign flipped, retained classes shear off the oracle.
#[test]
fn sparse_index_translate_matches_shifted_classify() {
    let nverts = 64u32;
    let bd = 8u32; // 8³ bricks
    let (gmin0, gmax0) = (Vec3::splat(-1.6), Vec3::splat(1.6));
    let cell = (gmax0 - gmin0) / (nverts - 1) as f32;
    let band = 2.0 * cell.min_element();
    let prims = vec![
        box_op(Vec3::new(0.2, -0.1, 0.0), Vec3::new(0.5, 0.4, 0.45), BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(-0.3, 0.3, 0.2), 0.4, BooleanOp::Union { k: 0.2 }),
    ];
    let sp0 = classify_prims_to_sparse_index(&prims, 0, Vec3::ZERO, gmin0, gmax0, nverts, bd, band);

    for &shift in &[[1i32, 0, 0], [-2, 1, 0], [0, -1, 2]] {
        let brick_w = cell * bd as f32; // world size of one brick step per axis
        let delta = Vec3::new(
            shift[0] as f32 * brick_w.x,
            shift[1] as f32 * brick_w.y,
            shift[2] as f32 * brick_w.z,
        );
        let (gmin1, gmax1) = (gmin0 + delta, gmax0 + delta);
        let translated = sparse_index_translate(&sp0, shift);
        let oracle = classify_prims_to_sparse_index(&prims, 0, Vec3::ZERO, gmin1, gmax1, nverts, bd, band);

        let dims = sp0.dims_bricks;
        let lin = |b: [i64; 3]| (b[0] + b[1] * dims[0] as i64 + b[2] * (dims[0] * dims[1]) as i64) as usize;
        let class_of = |tag: u32| -> u8 {
            if tag == BRICK_TAG_EXTERIOR { 0 } else if tag == BRICK_TAG_INTERIOR { 1 } else { 2 }
        };
        let mut retained = 0usize;
        let mut handles = 0usize;
        for bz in 0..dims[2] as i64 {
            for by in 0..dims[1] as i64 {
                for bx in 0..dims[0] as i64 {
                    let src = [bx + shift[0] as i64, by + shift[1] as i64, bz + shift[2] as i64];
                    let in_range = (0..3).all(|a| src[a] >= 0 && src[a] < dims[a] as i64);
                    if !in_range { continue; } // exposed slab — provisional, excluded
                    retained += 1;
                    let t_tag = translated.index[lin([bx, by, bz])];
                    let o_tag = oracle.index[lin([bx, by, bz])];
                    if class_of(t_tag) == 2 { handles += 1; }
                    assert_eq!(class_of(t_tag), class_of(o_tag),
                        "retained brick ({bx},{by},{bz}) shift {shift:?}: translated class {} != oracle class {} \
                         (t_tag={t_tag:#x} o_tag={o_tag:#x})",
                        class_of(t_tag), class_of(o_tag));
                    // Retained handle content identity: the translated handle still points at
                    // the ORIGINAL pool slot (content valid because world positions are equal).
                    if class_of(t_tag) == 2 {
                        assert_eq!(t_tag, sp0.index[lin(src)], "handle must be the original slot");
                    }
                }
            }
        }
        assert!(retained > 0 && handles > 0,
            "test scene must retain surface bricks across shift {shift:?} (retained={retained} handles={handles})");
    }
}

/// Freed-list accounting across a translate: every scrolled-out handle lands in `freed`
/// exactly once, no retained handle does, and `allocated` is untouched — the invariants the
/// window-allocate's LIFO recycle depends on.
#[test]
fn sparse_index_translate_freed_accounting() {
    let nverts = 64u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-1.6), Vec3::splat(1.6));
    let cell = ((gmax - gmin) / (nverts - 1) as f32).min_element();
    let band = 2.0 * cell;
    let prims = vec![
        // Surface near the -x wall so a +x window shift scrolls bricks out.
        sphere_op(Vec3::new(-1.1, 0.0, 0.0), 0.35, BooleanOp::Union { k: 0.0 }),
    ];
    let sp = classify_prims_to_sparse_index(&prims, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    assert!(sp.allocated > 0);
    let shift = [3i32, 0, 0]; // window moves +x → content moves -x → -x-wall bricks drop
    let t = sparse_index_translate(&sp, shift);

    let dims = sp.dims_bricks;
    let lin = |bx: u32, by: u32, bz: u32|
        (bx + by * dims[0] + bz * dims[0] * dims[1]) as usize;
    let mut expected_freed: Vec<u32> = Vec::new();
    let mut retained_handles = std::collections::HashSet::new();
    for bz in 0..dims[2] {
        for by in 0..dims[1] {
            for bx in 0..dims[0] {
                let tag = sp.index[lin(bx, by, bz)];
                if tag >= BRICK_TAG_INTERIOR { continue; }
                if (bx as i64 - shift[0] as i64) < 0 {
                    expected_freed.push(tag);
                } else {
                    retained_handles.insert(tag);
                }
            }
        }
    }
    assert_eq!(t.freed, expected_freed, "freed = scrolled-out handles in scan order");
    for h in &t.freed {
        assert!(!retained_handles.contains(h), "freed handle {h} still referenced");
    }
    assert_eq!(t.allocated, sp.allocated);
    // And every retained handle is still present in the translated index exactly once.
    let live: Vec<u32> = t.index.iter().copied().filter(|&x| x < BRICK_TAG_INTERIOR).collect();
    assert_eq!(live.len(), retained_handles.len());
    assert_eq!(live.iter().copied().collect::<std::collections::HashSet<_>>(), retained_handles);
}

// ================================================================
// P1-#6c — provably_interior (prefilter + reclaim)
// ================================================================

/// Bit-identity of the prefiltered classify vs the exact per-brick classify, on scenes
/// chosen to stress every proof arm: pure-union donors, a subtract carving INTO a donor
/// (carver reach must block the proof around the carve), body mirror (combos reach), an
/// intersect body (proofs disabled body-wide), and a modified prim (never donates).
/// provably_interior must be STRICTLY conservative — any divergence here is fabricated
/// interior inside real surface = invisible holes.
#[test]
fn interior_prefilter_classify_parity() {
    // 256³ (32³ bricks): the margin is 2×brick-diag, so at toy resolutions (64³/8³ bricks →
    // margin ≈ 1.4 world units) NOTHING classifies Interior and the test is vacuous. 256³
    // puts margin ≈ 0.33 — representative of the production 512³ regime the proof targets.
    let nverts = 256u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-1.6), Vec3::splat(1.6));
    let band = 2.0 * ((gmax - gmin) / (nverts - 1) as f32).min_element();

    let cube = |he: f32, op: BooleanOp| box_op(Vec3::new(0.05, -0.08, 0.02), Vec3::splat(he), op);
    let scenes: Vec<(&str, Vec<SdfPrimitive>, u8, Vec3)> = vec![
        ("solid cube (donor fires on the deep interior)",
            vec![cube(0.9, BooleanOp::Union { k: 0.0 })], 0, Vec3::ZERO),
        // Carver geometry note: the proof region is the donor ball SHRUNK by clearance
        // (≈0.43 at this res) MINUS the carver's influence GROWN by clearance — a
        // subtractor big/central enough leaves no provable lens and the anti-vacuity
        // trips. r=0.22 at the +x face keeps most of the donor ball provable while the
        // bricks near the carve exercise the carver-blocking arm.
        ("cube + subtract sphere carving in (carver blocks the proof near the carve)",
            vec![
                cube(0.9, BooleanOp::Union { k: 0.0 }),
                sphere_op(Vec3::new(0.9, 0.0, 0.0), 0.22, BooleanOp::Subtract { k: 0.0 }),
            ], 0, Vec3::ZERO),
        ("mirrored cube + subtract (combo reach on the reflected side)",
            vec![
                cube(1.0, BooleanOp::Union { k: 0.0 }),
                sphere_op(Vec3::new(0.5, 0.3, 0.0), 0.2, BooleanOp::Subtract { k: 0.0 }),
            ], 1, Vec3::new(0.1, 0.0, 0.0)),
        ("intersect present (interior proofs disabled body-wide)",
            vec![
                cube(0.9, BooleanOp::Union { k: 0.0 }),
                { let mut p = sphere_op(Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 });
                  p.operation = BooleanOp::Intersect; p },
            ], 0, Vec3::ZERO),
        ("smooth-union blob (donors under smooth k)",
            vec![
                sphere_op(Vec3::new(-0.3, 0.0, 0.0), 0.55, BooleanOp::Union { k: 0.0 }),
                sphere_op(Vec3::new(0.35, 0.1, -0.05), 0.5, BooleanOp::Union { k: 0.25 }),
            ], 0, Vec3::ZERO),
    ];

    for (label, prims, mflags, mc) in scenes {
        let fast = classify_prims_to_sparse_index(&prims, mflags, mc, gmin, gmax, nverts, bd, band);
        let clf = BrickClassifier::new(&prims, mflags, mc, gmin, gmax, nverts, bd, band);
        let dims = clf.dims_bricks();
        let n = (dims[0] * dims[1] * dims[2]) as usize;

        // (1) SOUNDNESS of the new proof, per brick: provably_interior may fire ONLY where
        // the exact 9-eval classify verdicts Interior — a divergence here is fabricated
        // interior inside possible surface = invisible holes. (The EXTERIOR proof is NOT
        // held to flag-identity: the eval's bounded box distance makes the exact classify
        // conservatively call far empty corner bricks "Surface", which provably_exterior
        // correctly overrides — a PRE-EXISTING S2.5h behavior, benign because those bricks
        // hold no true crossing; the GPU decode-parity gates pin that.)
        // (2) BASELINE identity: the emitted index must be byte-identical to the
        // pre-P1-#6c pipeline (exterior prefilter + exact classify, shared dilation +
        // emission) — since prov_int ⟹ exact Interior, adding the interior arm must
        // change NOTHING in the output, only skip 9-eval work.
        let mut is_surface = vec![false; n];
        let mut interior = vec![false; n];
        let mut interior_proofs = 0usize;
        for bz in 0..dims[2] {
            for by in 0..dims[1] {
                for bx in 0..dims[0] {
                    let bi = (bx + by * dims[0] + bz * dims[0] * dims[1]) as usize;
                    let pe = clf.provably_exterior(bx, by, bz);
                    if clf.provably_interior(bx, by, bz) {
                        assert!(!pe, "brick ({bx},{by},{bz}) proved BOTH interior and exterior: {label}");
                        assert_eq!(clf.classify_one_brick(bx, by, bz), BrickClass::Interior,
                            "UNSOUND interior proof at ({bx},{by},{bz}): {label}");
                        interior_proofs += 1;
                    }
                    // Baseline verdict = the pre-#6c pipeline.
                    if pe { continue; }
                    match clf.classify_one_brick(bx, by, bz) {
                        BrickClass::Surface => is_surface[bi] = true,
                        BrickClass::Interior => interior[bi] = true,
                        BrickClass::Exterior => {}
                    }
                }
            }
        }
        let store = dilate_surface_bricks_1ring(&is_surface, dims);
        let (baseline_index, baseline_allocated) = emit_sparse_index(&store, &interior);
        assert_eq!(fast.index, baseline_index,
            "interior prefilter changed the emitted index (must be output-neutral): {label}");
        assert_eq!(fast.allocated, baseline_allocated, "{label}");
        // Anti-vacuity: the donor scenes must actually exercise the proof.
        if !label.starts_with("intersect") {
            assert!(interior_proofs > 0, "vacuous: interior proof never fired in `{label}`");
        } else {
            assert_eq!(interior_proofs, 0, "interior proofs must be disabled body-wide: {label}");
        }
    }
}

// ================================================================
// 3A-perf — hierarchical (superblock) classify
// ================================================================

/// Byte-identity of the hierarchical classify: [`classify_prims_to_sparse_index`]'s FULL
/// OUTPUT (index vec + allocated) vs a brute construction that uses ONLY the per-brick
/// path (per-brick prefilters + classify_one_brick + shared dilation + emission). Scenes =
/// the interior-parity set (donor/subtract/mirror/intersect/smooth-blob) + a grid-filling
/// donor ball: at 256³ the parity solids are too small for a whole 4×4×4-brick region to
/// prove INTERIOR (the superblock clearance ≈ 0.71 world units eats their donor depth), so
/// the ball scene is what makes the interior arm non-vacuous.
#[test]
fn hierarchical_classify_parity() {
    let nverts = 256u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-1.6), Vec3::splat(1.6));
    let band = 2.0 * ((gmax - gmin) / (nverts - 1) as f32).min_element();

    let cube = |he: f32, op: BooleanOp| box_op(Vec3::new(0.05, -0.08, 0.02), Vec3::splat(he), op);
    let scenes: Vec<(&str, Vec<SdfPrimitive>, u8, Vec3)> = vec![
        ("solid cube (donor fires on the deep interior)",
            vec![cube(0.9, BooleanOp::Union { k: 0.0 })], 0, Vec3::ZERO),
        ("cube + subtract sphere carving in (carver blocks the proof near the carve)",
            vec![
                cube(0.9, BooleanOp::Union { k: 0.0 }),
                sphere_op(Vec3::new(0.9, 0.0, 0.0), 0.22, BooleanOp::Subtract { k: 0.0 }),
            ], 0, Vec3::ZERO),
        ("mirrored cube + subtract (combo reach on the reflected side)",
            vec![
                cube(1.0, BooleanOp::Union { k: 0.0 }),
                sphere_op(Vec3::new(0.5, 0.3, 0.0), 0.2, BooleanOp::Subtract { k: 0.0 }),
            ], 1, Vec3::new(0.1, 0.0, 0.0)),
        ("intersect present (interior proofs disabled body-wide)",
            vec![
                cube(0.9, BooleanOp::Union { k: 0.0 }),
                { let mut p = sphere_op(Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 });
                  p.operation = BooleanOp::Intersect; p },
            ], 0, Vec3::ZERO),
        ("smooth-union blob (donors under smooth k)",
            vec![
                sphere_op(Vec3::new(-0.3, 0.0, 0.0), 0.55, BooleanOp::Union { k: 0.0 }),
                sphere_op(Vec3::new(0.35, 0.1, -0.05), 0.5, BooleanOp::Union { k: 0.25 }),
            ], 0, Vec3::ZERO),
        ("donor ball (superblock interior proof fires)",
            vec![sphere_op(Vec3::new(0.03, -0.02, 0.05), 1.4, BooleanOp::Union { k: 0.0 })],
            0, Vec3::ZERO),
    ];

    for (label, prims, mflags, mc) in scenes {
        let fast = classify_prims_to_sparse_index(&prims, mflags, mc, gmin, gmax, nverts, bd, band);
        let clf = BrickClassifier::new(&prims, mflags, mc, gmin, gmax, nverts, bd, band);
        let dims = clf.dims_bricks();
        let n = (dims[0] * dims[1] * dims[2]) as usize;

        // Brute baseline: the per-brick path ONLY (both prefilters + exact classify) into
        // the shared dilation + emission — exactly the pre-hierarchical pipeline.
        let mut is_surface = vec![false; n];
        let mut interior = vec![false; n];
        for bz in 0..dims[2] {
            for by in 0..dims[1] {
                for bx in 0..dims[0] {
                    let bi = (bx + by * dims[0] + bz * dims[0] * dims[1]) as usize;
                    if clf.provably_exterior(bx, by, bz) {
                        continue;
                    }
                    if clf.provably_interior(bx, by, bz) {
                        interior[bi] = true;
                        continue;
                    }
                    match clf.classify_one_brick(bx, by, bz) {
                        BrickClass::Surface => is_surface[bi] = true,
                        BrickClass::Interior => interior[bi] = true,
                        BrickClass::Exterior => {}
                    }
                }
            }
        }
        let store = dilate_surface_bricks_1ring(&is_surface, dims);
        let (baseline_index, baseline_allocated) = emit_sparse_index(&store, &interior);
        assert_eq!(fast.index, baseline_index,
            "hierarchical classify changed the emitted index (must be output-neutral): {label}");
        assert_eq!(fast.allocated, baseline_allocated, "{label}");

        // Anti-vacuity — assert only where the geometry GUARANTEES a proof (the mirrored
        // cube's √3 bounding sphere + combo reach leave its corner superblocks knife-edge
        // unprovable at this grid, so no per-scene exterior assert there).
        let (verdicts, _sdims) = clf.superblock_verdicts();
        let n_ext = verdicts.iter().filter(|v| **v == Some((false, false))).count();
        let n_int = verdicts.iter().filter(|v| **v == Some((false, true))).count();
        if label.starts_with("solid cube")
            || label.starts_with("cube + subtract")
            || label.starts_with("donor ball")
        {
            assert!(n_ext > 0,
                "vacuous: superblock EXTERIOR proof never fired in `{label}` — whole \
                 superblocks at the far grid corners must prove");
        }
        if label.starts_with("donor ball") {
            assert!(n_int > 0, "vacuous: superblock INTERIOR proof never fired in `{label}`");
        }
        if label.starts_with("intersect") {
            assert_eq!(n_int, 0, "interior proofs must stay disabled body-wide: {label}");
        }
    }
}

/// The owner's scale-up leak, reproduced and pinned: growing a cube swallows its old
/// surface shell — those handles must RECLAIM via the interior proof in the window walk
/// (before P1-#6c only the exterior proof existed, so swallowed handles leaked until a
/// full re-classify; owner-measured pool 41k→153k bricks in one scale gesture).
#[test]
fn interior_reclaim_on_scale_up() {
    // 256³ for the same margin-scale reason as the parity test. The grown prim is a SPHERE
    // so its inscribed ball IS the solid — the strict ==0 assert below needs the whole
    // swallowed shell inside the proof reach, and a box donor can never cover its own
    // corners with a ball. Numbers: reach = r_in − (band+margin+half_diag) ≈ 1.4 − 0.43 =
    // 0.97; the small cube's CONSERVATIVE handle shell (surface-possible extends
    // band+margin ≈ 0.35 beyond the true surface, plus the 1-ring apron) peaks at
    // ≈ 0.18·√3 + 0.35 + 0.16 ≈ 0.82 — two bricks of slack. (Box-corner conservatism at
    // production 512³ is real but benign: unproven handles just wait for the settle full.)
    let nverts = 256u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 2.0 * ((gmax - gmin) / (nverts - 1) as f32).min_element();
    let center = Vec3::new(0.03, -0.02, 0.05);

    let small = vec![box_op(center, Vec3::splat(0.18), BooleanOp::Union { k: 0.0 })];
    let grown = vec![sphere_op(center, 1.4, BooleanOp::Union { k: 0.0 })];

    let sp_small = classify_prims_to_sparse_index(&small, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    assert!(sp_small.allocated > 0);

    // Whole-grid window (the production scale gesture unions old∪new AABB ≈ everything).
    let clf_grown = BrickClassifier::new(&grown, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let mut updated = SparseIndexOnly {
        index: sp_small.index.clone(), dims_bricks: sp_small.dims_bricks,
        allocated: sp_small.allocated, freed: Vec::new(),
    };
    let alloc = sparse_window_allocate_incompatible(
        &mut updated, bd, [0, 0, 0], [nverts; 3], &clf_grown,
    ).expect("scale-up must allocate, not escalate");

    assert!(alloc.appended > 0, "the grown surface needs new handles");
    assert!(alloc.reclaimed > 0,
        "swallowed old-shell handles must reclaim via the interior proof (got 0 — the leak)");
    assert!(alloc.reused > 0, "reclaimed slots must recycle into the new shell");
    // Watermark law (same as the far-move test).
    assert_eq!(updated.allocated, sp_small.allocated + (alloc.appended - alloc.reused) as usize);
    // The leak assert: the live handle count after the walk must track the GROWN scene's
    // own classify (new shell + aprons), not old-shell + new-shell.
    let fresh_grown = classify_prims_to_sparse_index(&grown, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    let live = updated.index.iter().filter(|&&t| t < BRICK_TAG_INTERIOR).count();
    assert!(live <= fresh_grown.allocated + fresh_grown.allocated / 10 + 64,
        "pool leak: {live} live handles vs fresh grown classify {}", fresh_grown.allocated);
    // And every brick the grown classify calls Interior that held a small-scene handle
    // must no longer hold one (the exact swallowed-shell population).
    let mut swallowed_still_handled = 0usize;
    let dims = updated.dims_bricks;
    for (bi, &t) in updated.index.iter().enumerate() {
        if t < BRICK_TAG_INTERIOR
            && fresh_grown.index[bi] == BRICK_TAG_INTERIOR
            && sp_small.index[bi] < BRICK_TAG_INTERIOR
        {
            if swallowed_still_handled < 6 {
                let bx = bi as u32 % dims[0];
                let by = (bi as u32 / dims[0]) % dims[1];
                let bz = bi as u32 / (dims[0] * dims[1]);
                eprintln!("  survivor ({bx},{by},{bz}): prov_int={} classify={:?}",
                    clf_grown.provably_interior(bx, by, bz),
                    clf_grown.classify_one_brick(bx, by, bz));
            }
            swallowed_still_handled += 1;
        }
    }
    assert_eq!(swallowed_still_handled, 0,
        "old-shell handles inside the grown solid survived the interior reclaim");
}

/// PROFILE (not a gate — `--ignored`, run `--release`): the frozen-index PARTIAL window
/// walk at the owner's scene shape — multi-prim 512³ body, drag windows from a small nudge
/// up to the 1/8-of-grid escalation threshold (206ea1b). Prints classify-phase vs
/// apply-phase ms per window size; the numbers behind the "[TriggerBake] 5-22ms during
/// heavy multi-prim drags" report.
#[test]
#[ignore = "profiling harness — run manually: cargo test -p core_math --release profile_window_walk -- --ignored --nocapture"]
fn profile_window_walk_512_multiprim() {
    let nverts = 512u32;
    let bd = 8u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let cell = ((gmax - gmin) / (nverts - 1) as f32).min_element();
    let band = 2.0 * cell;

    // Multi-prim body: 40 scattered prims (boxes + spheres, smooth-k, a few subtracts) —
    // the heavy-drag scene class behind the owner's [TriggerBake] 5-22ms logs (a sentinel
    // brick's classify is 9 × P field evals, so prim count P drives the walk cost).
    let scene = |dx: f32| -> Vec<SdfPrimitive> {
        let mut prims = Vec::new();
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((rng >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0 // [-1, 1)
        };
        for i in 0..40 {
            let pos = Vec3::new(next(), next(), next()) * 0.9 + Vec3::splat(dx);
            let op = match i % 8 {
                5 => BooleanOp::Subtract { k: 0.05 },
                3 => BooleanOp::Union { k: 0.15 },
                _ => BooleanOp::Union { k: 0.0 },
            };
            if i % 2 == 0 {
                prims.push(sphere_op(pos, 0.12 + 0.1 * next().abs(), op));
            } else {
                prims.push(box_op(pos, Vec3::splat(0.1 + 0.08 * next().abs()), op));
            }
        }
        prims
    };
    let prims0 = scene(0.0);
    let frozen = classify_prims_to_sparse_index(&prims0, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);
    // Moved field: a drag nudge of the whole cluster (multi-prim drag).
    let prims1 = scene(3.0 * cell * bd as f32);
    let clf = BrickClassifier::new(&prims1, 0, Vec3::ZERO, gmin, gmax, nverts, bd, band);

    // Drag windows centered on the cluster, voxel-sized: nudge → heavy → max (just under
    // the 1/8 escalation: 32³ of 64³ bricks).
    let mk_win = |half_vox: u32| -> ([u32; 3], [u32; 3]) {
        let c = nverts / 2;
        ([c - half_vox; 3], [(c + half_vox).min(nverts); 3])
    };
    let windows: Vec<(&str, [u32; 3], [u32; 3])> = [
        ("nudge  ~6³ bricks", 24u32),
        ("small  ~12³ bricks", 48),
        ("medium ~20³ bricks", 80),
        ("heavy  ~28³ bricks", 112),
        ("max    32³ bricks (1/8 threshold)", 128),
    ]
    .into_iter()
    .map(|(l, h)| { let (a, b) = mk_win(h); (l, a, b) })
    // Fast-drag SWEEP: long window through mostly-empty space (one prim dragged across
    // the grid) — the shape of the owner's escalation-class windows, where the proof
    // tiers do the work.
    .chain(std::iter::once((
        "sweep  64×10×10 bricks (fast drag)",
        [0u32, 400, 400], [nverts, 480, 480],
    )))
    .collect();
    for (label, wmin, wmax) in windows {
        // Warm + 5 timed iterations, report the median-ish (min of maxima keeps it simple).
        let mut best = (f32::INFINITY, f32::INFINITY, SparseWindowAlloc {
            appended: 0, reused: 0, retagged: 0, reclaimed: 0,
            new_bricks_min: [0; 3], new_bricks_max: [0; 3],
            classify_ms: 0.0, apply_ms: 0.0 });
        for _ in 0..5 {
            let mut updated = frozen.clone();
            let alloc = sparse_window_allocate_incompatible(&mut updated, bd, wmin, wmax, &clf)
                .expect("profile walk must not escalate");
            if alloc.classify_ms + alloc.apply_ms < best.0 + best.1 {
                best = (alloc.classify_ms, alloc.apply_ms, alloc);
            }
        }
        let wb: u64 = (0..3).map(|a| (((wmax[a] - wmin[a]) + bd - 1) / bd) as u64).product();
        eprintln!(
            "[ProfileWalk] {label}: {wb} bricks  classify={:.2}ms apply={:.2}ms  \
             (+{} retag={} reclaim={})",
            best.0, best.1, best.2.appended, best.2.retagged, best.2.reclaimed);
    }
}
