// io_bridge/src/mesh_to_sdf.rs
// OBJ → SDF volume converter for AZRA
//
// Pipeline:
//   tobj::load_obj  → gather vertices + indices
//   center + scale  → fit mesh inside [-grid_bounds, grid_bounds]³
//   mesh_to_sdf     → compute signed distance field on regular grid
//   output          → flat Vec<f32> ready to memcpy into SdfVolume::data

use glam::{UVec3, Vec3, uvec3};

/// Result of a successful OBJ → SDF conversion.
/// `data` is a flat f32 array indexed as `x + y*res + z*res*res`,
/// matching the layout of SdfVolume::data.
pub struct ObjImportResult {
    pub data:         Vec<f32>,
    pub resolution:   u32,
    pub vertex_count: usize,
    pub face_count:   usize,
    pub mesh_name:    String,
}

/// Load an OBJ file and convert it to a signed-distance-field volume.
///
/// `target_res`  — grid resolution (e.g. 128 → 128³ voxels).
/// `grid_bounds` — half-extent of the world cube (e.g. 2.0 → [−2, 2]³).
///                 Must match the cpu_volume.grid_bounds used in the app.
pub fn obj_to_sdf(
    path:        &str,
    target_res:  u32,
    grid_bounds: f32,
) -> Result<ObjImportResult, String> {
    // ---- 1. Load OBJ -------------------------------------------------------
    let (models, _) = tobj::load_obj(
        path,
        &tobj::LoadOptions { single_index: true, triangulate: true, ..Default::default() },
    )
    .map_err(|e| format!("OBJ load error: {}", e))?;

    if models.is_empty() {
        return Err("OBJ file contains no meshes".into());
    }

    let mesh_name = models[0].name.clone();

    // ---- 2. Collect vertices + indices from all sub-meshes -----------------
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    let mut indices:  Vec<u32>      = Vec::new();

    for model in &models {
        let offset = vertices.len() as u32;
        let pos    = &model.mesh.positions;
        if pos.len() % 3 != 0 {
            return Err(format!(
                "OBJ mesh '{}': positions length {} is not a multiple of 3",
                model.name, pos.len()
            ));
        }
        for chunk in pos.chunks_exact(3) {
            vertices.push([chunk[0], chunk[1], chunk[2]]);
        }
        let vert_count = vertices.len() as u32;
        for &idx in &model.mesh.indices {
            let global = offset + idx;
            if global >= vert_count {
                eprintln!(
                    "WARNING: OBJ mesh '{}': index {} (global {}) out of bounds (vertex count {}), skipping",
                    model.name, idx, global, vert_count
                );
                continue;
            }
            indices.push(global);
        }
    }

    if vertices.is_empty() || indices.is_empty() {
        return Err("OBJ mesh has no geometry".into());
    }

    let vertex_count = vertices.len();
    let face_count   = indices.len() / 3;

    // ---- 3. Center + scale to fit in [-bounds*0.88, bounds*0.88] -----------
    let mut bb_min = Vec3::splat(f32::MAX);
    let mut bb_max = Vec3::splat(f32::MIN);
    for v in &vertices {
        let p = Vec3::from_array(*v);
        bb_min = bb_min.min(p);
        bb_max = bb_max.max(p);
    }
    let center     = (bb_min + bb_max) * 0.5;
    let half_ext   = (bb_max - bb_min) * 0.5;
    let max_half   = half_ext.x.max(half_ext.y).max(half_ext.z);
    let scale      = if max_half > 1e-6 { grid_bounds * 0.88 / max_half } else { 1.0 };

    for v in &mut vertices {
        let p = (Vec3::from_array(*v) - center) * scale;
        *v = p.to_array();
    }

    // ---- 4. Build mesh-to-sdf grid matching SdfVolume grid points ----------
    // SdfVolume samples at: p_i = -bounds + i * (2*bounds / (res-1))
    // for i in 0..res  →  res grid points along each axis.
    // mesh-to-sdf::Grid::from_bounding_box places cell CENTERS inside [min, max]
    // with cell_size = (max-min)/cell_count.  Using res cells with min/max set so
    // that the first center = -bounds and the last center = +bounds:
    //   cell_size = 2*bounds / (res-1)
    //   adjusted min = -bounds - cell_size/2
    //   adjusted max = +bounds + cell_size/2
    let res       = target_res as usize;
    let cell_size = (grid_bounds * 2.0) / (res as f32 - 1.0);
    let adj_min   = -grid_bounds - cell_size * 0.5;
    let adj_max   =  grid_bounds + cell_size * 0.5;
    let bb3_min   = [adj_min, adj_min, adj_min];
    let bb3_max   = [adj_max, adj_max, adj_max];

    let grid = mesh_to_sdf::Grid::from_bounding_box(&bb3_min, &bb3_max, [res, res, res]);

    // ---- 5. Choose sign method (Raycast if watertight, Normal otherwise) ---
    let sign_method = if is_watertight(&indices) {
        mesh_to_sdf::SignMethod::Raycast
    } else {
        mesh_to_sdf::SignMethod::Normal
    };

    // ---- 6. Compute SDF (BVH-accelerated) ----------------------------------
    // generate_grid_sdf returns a Vec<f32> indexed as x + y*cx + z*cx*cy,
    // which matches SdfVolume layout (x + y*rx + z*rx*ry). No reordering needed.
    let data = mesh_to_sdf::generate_grid_sdf(
        &vertices,
        mesh_to_sdf::Topology::TriangleList(Some(&indices)),
        &grid,
        sign_method,
    );

    if data.len() != res * res * res {
        return Err(format!(
            "SDF grid size mismatch: expected {} got {}",
            res * res * res,
            data.len()
        ));
    }

    Ok(ObjImportResult { data, resolution: target_res, vertex_count, face_count, mesh_name })
}

/// Region SDF sample of a triangle mesh, aligned to a `VoxelGrid`'s exact sample lattice.
///
/// Used by the voxel-sculpt MOVE tool's Phase-2 mesh→SDF re-voxelize: the live drag deforms the
/// surface MESH (vertex grab); on release the deformed mesh is sampled back into the density grid
/// over only the region the grab swept. This is the Meta-Medium mesh→SDF round-trip.
///
/// Coordinate contract — the sample points MUST coincide with `VoxelGrid::cell_world_pos`:
///   `p_i = -bounds + i * (2*bounds / (res-1))`  for `i in 0..res`.
/// We build a `mesh_to_sdf::Grid` whose cell CENTERS land exactly on the sub-lattice
/// `[gx0..=gx1] × [gy0..=gy1] × [gz0..=gz1]` of that full `res³` lattice (mirrors the cubic math in
/// `obj_to_sdf` above, lines ~101-117, but restricted to the AABB and per-axis).
///
/// `verts`/`indices` — the WHOLE object's current (deformed) triangle mesh in voxel-LOCAL space
///   (positions only; same space as `cell_world_pos`). The whole surface is closed even though only
///   a subset of verts moved, so sign computation has no open-boundary problem.
/// `bounds`/`res` — the target `VoxelGrid`'s `bounds` and `res` (the lattice we sample onto).
/// `aabb` — `[gx0,gy0,gz0, gx1,gy1,gz1]` inclusive CELL range to sample (already clamped by caller).
///
/// Returns the signed distances in **VoxelGrid sub-region order** (`x + y*nx + z*nx*ny`, with
/// `nx = gx1-gx0+1`, etc.) so the caller maps cell `(gx0+x, gy0+y, gz0+z)` → `out[x + y*nx + z*nx*ny]`
/// directly. The transpose from the `mesh_to_sdf` crate's own `z + y*nz + x*ny*nz` cell ordering is
/// done HERE via `get_cell_idx`, so the caller never has to know the crate's layout. `None` if the
/// AABB is degenerate (empty range) or the mesh is empty.
pub fn region_mesh_to_sdf(
    verts: &[glam::Vec3],
    indices: &[u32],
    bounds: f32,
    res: u32,
    aabb: [u32; 6],
) -> Option<Vec<f32>> {
    // Fast parallel path (Move bake hot path). Full closed mesh kept for Normal signs —
    // only the *sample lattice* is the local AABB. ~5–10× faster than mesh_to_sdf crate
    // on multi-core for typical sculpt regions.
    region_mesh_to_sdf_parallel(verts, indices, bounds, res, aabb)
}

/// Below this tri count, brute-force nearest-triangle is faster than the spatial-hash
/// ring walk (LAB 12-tri box: hash ring expansion on exterior samples dominated ~1s @128³).
const BRUTE_FORCE_TRI_LIMIT: usize = 128;

/// Triangle soup entry with outward normal + AABB. Shared by the full sampler
/// (`region_mesh_to_sdf_parallel`) and the narrow-band sampler (`region_mesh_to_sdf_band`).
struct Tri {
    a: Vec3, b: Vec3, c: Vec3, n: Vec3,
    bmin: Vec3, bmax: Vec3,
}

/// Keep triangles whose AABB overlaps `[lo, hi]` (inclusive). Degenerate index triples
/// are dropped the same way [`tri_soup`] would drop them.
fn filter_indices_overlapping(
    verts: &[Vec3],
    indices: &[u32],
    lo: Vec3,
    hi: Vec3,
) -> Vec<u32> {
    let mut out = Vec::with_capacity(indices.len());
    for t in indices.chunks(3) {
        if t.len() < 3 {
            break;
        }
        let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if ia >= verts.len() || ib >= verts.len() || ic >= verts.len() {
            continue;
        }
        let a = verts[ia];
        let b = verts[ib];
        let c = verts[ic];
        let tmin = a.min(b).min(c);
        let tmax = a.max(b).max(c);
        if tmax.x < lo.x || tmin.x > hi.x
            || tmax.y < lo.y || tmin.y > hi.y
            || tmax.z < lo.z || tmin.z > hi.z
        {
            continue;
        }
        out.extend_from_slice(t);
    }
    out
}

/// Build the triangle soup (winding a→b→c → outward normal). Degenerate / out-of-range
/// triangles are skipped. `None` if nothing survives.
fn tri_soup(verts: &[glam::Vec3], indices: &[u32]) -> Option<Vec<Tri>> {
    let mut tris: Vec<Tri> = Vec::with_capacity(indices.len() / 3);
    for t in indices.chunks(3) {
        if t.len() < 3 { break; }
        let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if ia >= verts.len() || ib >= verts.len() || ic >= verts.len() { continue; }
        let a = verts[ia];
        let b = verts[ib];
        let c = verts[ic];
        let e1 = b - a;
        let e2 = c - a;
        let mut n = e1.cross(e2);
        let nl = n.length();
        if nl < 1e-12 { continue; }
        n /= nl;
        let bmin = a.min(b).min(c);
        let bmax = a.max(b).max(c);
        tris.push(Tri { a, b, c, n, bmin, bmax });
    }
    if tris.is_empty() { None } else { Some(tris) }
}

/// Snap points onto the nearest surface point of a triangle mesh, bounded by `max_dist`.
///
/// Points with no triangle within `max_dist` come back UNCHANGED. That bound is the whole safety
/// argument for using this on a voxel remesh: where the remesh deliberately CHANGED topology —
/// sealed an internal void, bridged a gap thinner than a voxel, rounded off sub-voxel detail —
/// there is no corresponding source surface nearby, and an unbounded snap would drag those
/// vertices onto unrelated geometry and tear the result. Clamping to about one cell keeps the
/// projection to "undo the voxel quantisation", never "undo the remesh".
///
/// Uses the same triangle soup and `closest_on_tri` as the SDF samplers (reuse rule). The bin grid
/// is sized so `bin >= max_dist`, which is what makes a fixed 3×3×3 neighbourhood provably
/// sufficient — no ring expansion, so per-point work is bounded rather than data-dependent.
///
/// Returns `None` only for a degenerate mesh (no usable triangles).
pub fn project_points_to_mesh(
    points: &[Vec3],
    verts: &[Vec3],
    indices: &[u32],
    max_dist: f32,
) -> Option<Vec<Vec3>> {
    project_points_to_mesh_filtered(points, verts, indices, max_dist, None)
}

/// Same as [`project_points_to_mesh`], but only source triangles whose AABB overlaps
/// `clip` (expanded by `max_dist`) are considered. Points whose nearest triangle is
/// outside that window come back unchanged — the same miss path as the `max_dist`
/// clamp, so a caller that already subset the *points* to a dirty box can also
/// subset the *source* without changing any projected result inside the box.
///
/// `clip = None` is bit-identical to [`project_points_to_mesh`].
pub fn project_points_to_mesh_filtered(
    points: &[Vec3],
    verts: &[Vec3],
    indices: &[u32],
    max_dist: f32,
    clip: Option<(Vec3, Vec3)>,
) -> Option<Vec<Vec3>> {
    use rayon::prelude::*;
    let tris = if let Some((mn, mx)) = clip {
        let pad = Vec3::splat(max_dist.max(0.0));
        let filtered = filter_indices_overlapping(verts, indices, mn - pad, mx + pad);
        if filtered.is_empty() {
            return if indices.len() < 3 {
                None
            } else {
                Some(points.to_vec())
            };
        }
        tri_soup(verts, &filtered)?
    } else {
        tri_soup(verts, indices)?
    };
    if points.is_empty() || max_dist <= 0.0 {
        return Some(points.to_vec());
    }

    let mut bmin = Vec3::splat(f32::MAX);
    let mut bmax = Vec3::splat(f32::MIN);
    for t in &tris {
        bmin = bmin.min(t.bmin);
        bmax = bmax.max(t.bmax);
    }
    // bin >= max_dist (see above); the /64 floor keeps the bucket count bounded for a tiny
    // max_dist on a large mesh.
    let bin = max_dist.max((bmax - bmin).max_element() / 64.0).max(1e-6);
    let dim = |e: f32| ((e / bin).ceil() as i64).max(1);
    let (nx, ny, nz) = (
        dim(bmax.x - bmin.x),
        dim(bmax.y - bmin.y),
        dim(bmax.z - bmin.z),
    );
    let bidx = |x: i64, y: i64, z: i64| (x + y * nx + z * nx * ny) as usize;

    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); (nx * ny * nz) as usize];
    let cell_of = |p: Vec3| -> (i64, i64, i64) {
        (
            (((p.x - bmin.x) / bin).floor() as i64).clamp(0, nx - 1),
            (((p.y - bmin.y) / bin).floor() as i64).clamp(0, ny - 1),
            (((p.z - bmin.z) / bin).floor() as i64).clamp(0, nz - 1),
        )
    };
    for (i, t) in tris.iter().enumerate() {
        let (x0, y0, z0) = cell_of(t.bmin);
        let (x1, y1, z1) = cell_of(t.bmax);
        for z in z0..=z1 {
            for y in y0..=y1 {
                for x in x0..=x1 {
                    buckets[bidx(x, y, z)].push(i as u32);
                }
            }
        }
    }

    let max_d2 = max_dist * max_dist;
    Some(
        points
            .par_iter()
            .map(|&p| {
                let (cx, cy, cz) = cell_of(p);
                let mut best = f32::MAX;
                let mut hit = p;
                for z in (cz - 1).max(0)..=(cz + 1).min(nz - 1) {
                    for y in (cy - 1).max(0)..=(cy + 1).min(ny - 1) {
                        for x in (cx - 1).max(0)..=(cx + 1).min(nx - 1) {
                            for &ti in &buckets[bidx(x, y, z)] {
                                let (q, d2) = closest_on_tri(p, &tris[ti as usize]);
                                if d2 < best {
                                    best = d2;
                                    hit = q;
                                }
                            }
                        }
                    }
                }
                if best <= max_d2 { hit } else { p }
            })
            .collect(),
    )
}

/// Closest point on triangle → (point, unsigned dist²). Ericson barycentric.
#[inline]
fn closest_on_tri(p: Vec3, t: &Tri) -> (Vec3, f32) {
    let ab = t.b - t.a;
    let ac = t.c - t.a;
    let ap = p - t.a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (t.a, (p - t.a).length_squared());
    }
    let bp = p - t.b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (t.b, (p - t.b).length_squared());
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        let q = t.a + ab * v;
        return (q, (p - q).length_squared());
    }
    let cp = p - t.c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (t.c, (p - t.c).length_squared());
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        let q = t.a + ac * w;
        return (q, (p - q).length_squared());
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let q = t.b + (t.c - t.b) * w;
        return (q, (p - q).length_squared());
    }
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    let q = t.a + ab * v + ac * w;
    (q, (p - q).length_squared())
}

#[inline]
fn signed_from_best(p: Vec3, best_d2: f32, best_n: Vec3, best_q: Vec3) -> f32 {
    let sign = if (p - best_q).dot(best_n) >= 0.0 { 1.0 } else { -1.0 };
    sign * best_d2.sqrt()
}

/// Parallel nearest-triangle SDF over a local lattice. Sign = Normal (outward winding).
/// Same coordinate contract as the legacy `mesh_to_sdf` path.
pub fn region_mesh_to_sdf_parallel(
    verts: &[glam::Vec3],
    indices: &[u32],
    bounds: f32,
    res: u32,
    aabb: [u32; 6],
) -> Option<Vec<f32>> {
    use rayon::prelude::*;
    if verts.is_empty() || indices.is_empty() || res < 2 { return None; }
    let [gx0, gy0, gz0, gx1, gy1, gz1] = aabb;
    if gx0 > gx1 || gy0 > gy1 || gz0 > gz1 { return None; }
    let nx = (gx1 - gx0 + 1) as usize;
    let ny = (gy1 - gy0 + 1) as usize;
    let nz = (gz1 - gz0 + 1) as usize;
    let cell_size = (bounds * 2.0) / (res as f32 - 1.0);
    let first = |g: u32| -bounds + g as f32 * cell_size;

    let tris = tri_soup(verts, indices)?;

    // ── FAST PATH: few triangles ────────────────────────────────────────────
    // Spatial hash + ring walk is for dense meshes. On a 12-tri box the bin
    // grid has many empty exterior cells; each sample expands rings until it
    // hits the mesh → O(ring³) overhead dominates. Brute-force is O(T) per
    // sample with T≈12 and is ~10–50× faster for LAB poly prims.
    if tris.len() <= BRUTE_FORCE_TRI_LIMIT {
        let mut out = vec![0.0f32; nx * ny * nz];
        out.par_chunks_mut(nx * ny).enumerate().for_each(|(zi, plane)| {
            let gz = gz0 + zi as u32;
            let z = first(gz);
            for yi in 0..ny {
                let gy = gy0 + yi as u32;
                let y = first(gy);
                for xi in 0..nx {
                    let gx = gx0 + xi as u32;
                    let x = first(gx);
                    let p = Vec3::new(x, y, z);
                    let mut best_d2 = f32::INFINITY;
                    let mut best_n = Vec3::Y;
                    let mut best_q = p;
                    for t in &tris {
                        let (q, d2) = closest_on_tri(p, t);
                        if d2 < best_d2 {
                            best_d2 = d2;
                            best_n = t.n;
                            best_q = q;
                        }
                    }
                    plane[xi + yi * nx] = signed_from_best(p, best_d2, best_n, best_q);
                }
            }
        });
        return Some(out);
    }

    // ── SPATIAL HASH PATH: dense meshes ─────────────────────────────────────
    let mut mesh_min = tris[0].bmin;
    let mut mesh_max = tris[0].bmax;
    for t in &tris {
        mesh_min = mesh_min.min(t.bmin);
        mesh_max = mesh_max.max(t.bmax);
    }
    let extent = (mesh_max - mesh_min).max(Vec3::splat(1e-4));
    let bin = (extent.max_element() / 32.0).max(cell_size * 2.0);
    let inv_bin = 1.0 / bin;
    let dims = ((extent * inv_bin).ceil().as_uvec3() + uvec3(1, 1, 1)).max(UVec3::ONE);
    let dx = dims.x as usize;
    let dy = dims.y as usize;
    let dz = dims.z as usize;
    let n_bins = dx * dy * dz;
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); n_bins];
    let bin_of = |p: Vec3| -> (i32, i32, i32) {
        let q = (p - mesh_min) * inv_bin;
        (q.x.floor() as i32, q.y.floor() as i32, q.z.floor() as i32)
    };
    let bin_idx = |ix: i32, iy: i32, iz: i32| -> Option<usize> {
        if ix < 0 || iy < 0 || iz < 0 { return None; }
        let (ux, uy, uz) = (ix as u32, iy as u32, iz as u32);
        if ux >= dims.x || uy >= dims.y || uz >= dims.z { return None; }
        Some((ux + uy * dims.x + uz * dims.x * dims.y) as usize)
    };
    for (ti, t) in tris.iter().enumerate() {
        let (i0, j0, k0) = bin_of(t.bmin);
        let (i1, j1, k1) = bin_of(t.bmax);
        for iz in k0..=k1 {
            for iy in j0..=j1 {
                for ix in i0..=i1 {
                    if let Some(bi) = bin_idx(ix, iy, iz) {
                        bins[bi].push(ti as u32);
                    }
                }
            }
        }
    }

    let signed_dist = |p: Vec3| -> f32 {
        let (bx, by, bz) = bin_of(p);
        let mut best_d2 = f32::INFINITY;
        let mut best_n = Vec3::Y;
        let mut best_q = p;
        // Expand search ring until we have a hit (usually ring 0–2).
        for ring in 0i32..48 {
            let mut any = false;
            for iz in (bz - ring)..=(bz + ring) {
                for iy in (by - ring)..=(by + ring) {
                    for ix in (bx - ring)..=(bx + ring) {
                        // Only the shell of the cube for ring > 0.
                        if ring > 0
                            && ix > bx - ring && ix < bx + ring
                            && iy > by - ring && iy < by + ring
                            && iz > bz - ring && iz < bz + ring
                        {
                            continue;
                        }
                        let Some(bi) = bin_idx(ix, iy, iz) else { continue; };
                        for &ti in &bins[bi] {
                            any = true;
                            let t = &tris[ti as usize];
                            let (q, d2) = closest_on_tri(p, t);
                            if d2 < best_d2 {
                                best_d2 = d2;
                                best_n = t.n;
                                best_q = q;
                            }
                        }
                    }
                }
            }
            // Early out once best distance is inside the current ring radius.
            if best_d2.is_finite() && best_d2 < (ring as f32 * bin).max(0.0).powi(2) {
                break;
            }
            if !any && ring > 12 {
                break;
            }
        }
        if !best_d2.is_finite() {
            // Fallback: brute-force all tris (rare — degenerate binning).
            for t in &tris {
                let (q, d2) = closest_on_tri(p, t);
                if d2 < best_d2 {
                    best_d2 = d2;
                    best_n = t.n;
                    best_q = q;
                }
            }
        }
        signed_from_best(p, best_d2, best_n, best_q)
    };

    // Parallel fill in VoxelGrid order: x + y*nx + z*nx*ny.
    let mut out = vec![0.0f32; nx * ny * nz];
    out.par_chunks_mut(nx * ny).enumerate().for_each(|(zi, plane)| {
        let gz = gz0 + zi as u32;
        let z = first(gz);
        for yi in 0..ny {
            let gy = gy0 + yi as u32;
            let y = first(gy);
            for xi in 0..nx {
                let gx = gx0 + xi as u32;
                let x = first(gx);
                let p = Vec3::new(x, y, z);
                plane[xi + yi * nx] = signed_dist(p);
            }
        }
    });
    Some(out)
}

/// NARROW-BAND signed distance over a local lattice — the fast path for full-object
/// voxelization (poly Remesh modifier, poly→SDF convert).
///
/// The full samplers above compute an exact Euclidean distance at EVERY lattice cell —
/// O(res³ × nearest-triangle work) — even though every consumer immediately saturates the
/// far field (`write_region_from_sdf` clamps at |d| ≥ w/2). This sampler computes exact
/// distances only within `band` of the surface and classifies everything else by sign:
///
///   1. **Scatter** (parallel over z-planes): each triangle writes exact signed distance
///      into the cells of its `band`-dilated AABB, keeping the per-cell minimum. Any cell
///      whose true distance ≤ `band` is covered by its globally-nearest triangle's dilated
///      AABB, so in-band values are the exact global minimum — identical (same
///      `closest_on_tri`, same first-wins tie-break on ascending triangle index) to the
///      brute path above.
///   2. **Sign flood** (span-based 6-conn AIR fill from the region border): walks far cells
///      AND in-band positive (air) cells — so cavities connected through throats narrower
///      than 2·`band` stay open. Unreached far cells are inside (−`band`); unreached
///      in-band AIR cells lie in a sealed void and are flipped to −`band` as well, so
///      sealed voids fill with NO inner surface (the Blender / 3D-Coat voxel-remesh
///      semantic — the full sampler instead kept sealed voids as air).
///
/// Cost: O(surface·band + res³ flood) instead of O(res³ × triangles).
///
/// Contract (same lattice as `region_mesh_to_sdf`): `p_i = -bounds + i * (2*bounds/(res-1))`,
/// output in region order `x + y*nx + z*nx*ny`. Additional requirements:
///   * `band` is clamped up to `√3 · cell` so the solid-side near shell is watertight under
///     6-conn flood; callers should pass ≥ the consumer's saturation width (remesh uses `w`).
///   * The REGION BORDER must be outside the solid (remesh guarantees this structurally —
///     ≥2 border cells of air on every axis). A solid that crosses the border does NOT
///     invert signs (interior cells are not walkable, so they cannot seed) — it makes the
///     openings it cuts read as sealed. A border with NO air at all is refused (`None`).
///   * Open meshes: every cell is border-reachable, so the result is the exact in-band
///     crust around the sheet (a ~2·`band` thick plate — Blender's mesh-to-volume
///     semantic). The old sampler instead solidified the pseudo-normal back half-space;
///     neither is "the" solid of an open mesh — watertight input is the supported contract.
pub fn region_mesh_to_sdf_band(
    verts: &[glam::Vec3],
    indices: &[u32],
    bounds: f32,
    res: u32,
    aabb: [u32; 6],
    band: f32,
) -> Option<Vec<f32>> {
    region_mesh_to_sdf_band_stats(verts, indices, bounds, res, aabb, band).map(|(f, _)| f)
}

/// [`region_mesh_to_sdf_band`] that also reports how many AIR cells were filled as SEALED
/// VOIDS. `0` proves the field has no enclosed void — the precondition the incremental
/// remesh path needs (void filling is one-way, so a field that already contains a filled
/// void cannot have it re-opened by a local region update).
/// Cell counts from the band sampler's sign classification, **split** because the two mean very
/// different things and conflating them is a shipped bug class (`REMESHSOLIDNOINCR`, 2026-08-10).
///
/// This used to be one `usize` named `enclosed`. Its only consumer,
/// `voxel_remesh_full`, read it as "this field had a sealed void" and latched a one-way lockout
/// that permanently banished the mesh from the incremental remesh path. But the count included
/// `far_solid`, so **every closed solid tripped it on the very first eval** — a solid cube at res
/// 128 reports 512000 — and incremental remesh silently never ran on a real model (measured: a
/// solid drag frame at res 256 was 191 ms vs 26 ms on an open crust).
///
/// Keep these separate. A consumer asking "was a void filled?" wants [`Self::sealed_pocket`] and
/// nothing else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BandSignStats {
    /// Far cells the border flood never reached, set solid. For a closed solid this is its
    /// **interior** — expected, ordinary, and not a void. Diagnostics only; do not gate on it.
    pub far_solid: usize,
    /// Cells the surface places on the AIR side (in-band, positive distance) that the border flood
    /// could not reach — the lining of a genuinely **sealed pocket**, filled solid here.
    ///
    /// This is the honest "a void was filled" signal, and the one the incremental remesh lockout
    /// keys on: filling is one-way, so a later edit re-opening the pocket cannot be reproduced by a
    /// local region splice. A solid's interior is all far cells, so it contributes 0 here; a hollow
    /// shell's cavity lining is in-band air and does fire it.
    pub sealed_pocket: usize,
}

pub fn region_mesh_to_sdf_band_stats(
    verts: &[glam::Vec3],
    indices: &[u32],
    bounds: f32,
    res: u32,
    aabb: [u32; 6],
    band: f32,
) -> Option<(Vec<f32>, BandSignStats)> {
    if verts.is_empty() || indices.is_empty() || res < 2 { return None; }
    let [gx0, gy0, gz0, gx1, gy1, gz1] = aabb;
    if gx0 > gx1 || gy0 > gy1 || gz0 > gz1 { return None; }
    let nx = (gx1 - gx0 + 1) as usize;
    let ny = (gy1 - gy0 + 1) as usize;
    let nz = (gz1 - gz0 + 1) as usize;
    let cell_size = (bounds * 2.0) / (res as f32 - 1.0);
    // Seal clamp: a band thinner than the cell diagonal can leave 6-conn gaps in the near
    // shell → the sign flood leaks through the surface. √3·cell guarantees ≥1-cell thickness.
    let band = band.max(cell_size * 1.7321).max(1e-6); // √3 ≈ 1.7321

    let tris = tri_soup(verts, indices)?;
    let mut out = band_scatter(&tris, bounds, res, aabb, band);

    // ── Sign, attempt 1: TOPOLOGICAL (no normals involved). ─────────────────────────
    // Seed EVERY border cell — unlike the legacy flood this pass needs no sign to decide
    // where it may walk, so the caller's air-border contract (the remesh lattice enforces a
    // ≥2-cell margin structurally) is the only precondition, and an isolated wrongly-signed
    // border cell can no longer keep the flood out.
    let cross = band_edge_crossings(&tris, bounds, res, aabb);
    let mut outside = vec![false; nx * ny * nz];
    let stack = seed_border(nx, ny, nz, &mut outside, |_, _, _| true);
    flood_reach_crossings(&cross, nx, ny, nz, stack, &mut outside);
    if outside.iter().any(|&b| !b) {
        let stats = band_finalize_topological(&mut out, &outside, band);
        return Some((out, stats));
    }
    // Nothing is enclosed at this lattice: the surface is open (or holed wider than a cell),
    // so enclosure cannot define a sign and the legacy facing-based path below takes over —
    // that is what still produces Blender's ~2·band crust for an open sheet.

    // ── Sign, attempt 2 (open input): flood by field sign, the historical path. ──────
    // FAIL-CLOSED contract check in the same loop: an in-band NEGATIVE distance on the
    // border proves the solid crosses the region border — the flood would then seed
    // "outside" INSIDE the object and invert the far field. Refuse instead.
    let mut outside = vec![false; nx * ny * nz];
    let stack = seed_border(nx, ny, nz, &mut outside, |x, y, z| {
        band_walkable(out[x + y * nx + z * nx * ny])
    });
    // FAIL CLOSED: no air anywhere on the border means the flood has no starting point and
    // every cell would be classified solid — a degenerate field, not a remesh. (Deliberately
    // NOT "any negative border cell": at an exact geometric tie the pseudo-normal sign can
    // pick either adjacent face, so an isolated -band cell appears on a perfectly valid
    // lattice — a strict check refused plain cubes. Partial crossings are handled correctly
    // by walkability: interior cells cannot seed, so signs are never inverted; the only
    // degradation is that openings cut by the border read as sealed, which is exactly what
    // the remesh lattice's structural air margin exists to prevent.)
    if stack.is_empty() {
        return None;
    }
    flood_air_reach(&out, nx, ny, nz, stack, &mut outside);
    let stats = band_finalize(&mut out, &outside, band);
    Some((out, stats))
}

/// SEEDED-BORDER variant of [`region_mesh_to_sdf_band`] for INCREMENTAL dirty-region
/// updates of an existing field (the Remesh modifier's live-edit path; the sculpt
/// Move-bake is the same shape). A dirty sub-region's border legitimately cuts through
/// the solid, so instead of assuming border-is-air this variant asks the CALLER which
/// border cells were air in the previous field: `prev_air(gx, gy, gz)` (GLOBAL lattice
/// coords, queried only on the region's border shell). Border cells that are walkable in
/// the NEW scatter AND were air before seed the flood; everything else follows the same
/// air-connectivity rules as the full variant (same scatter, same flood, same finalize —
/// shared helpers, not a second implementation).
///
/// Soundness rests on TWO caller obligations:
///  1. The dirty region must contain every cell within `band` of any CHANGED geometry (old
///     and new positions — a moved triangle's old AND new AABB, dilated). Then geometry
///     near the border is unchanged, the previous field's border signs are still true, and
///     the spliced result equals a full-lattice resample bit-for-bit in band / sign-for-sign
///     in the far field (golden-gated by `seeded_region_splice_matches_full_resample`).
///  2. **The caller must handle ENCLOSURE CHANGES, which a local splice provably cannot
///     see.** The sign of a far cell is a GLOBAL air-connectivity property: an edit that
///     seals a cavity must flip far cells OUTSIDE the dirty region from air to solid, and
///     an edit that opens one must flip them back. This function only rewrites the region.
///     The caller must therefore re-run a whole-field seal after splicing (see
///     `VoxelGrid::seal_enclosed_air`) AND refuse the incremental path entirely on a field
///     that already contains a filled void — filling is one-way, so an opened void can
///     never be restored locally. `voxel_remesh_mesh_incremental` does both; the
///     `incremental_*` gates in `poly/remesh.rs` pin the behavior.
pub fn region_mesh_to_sdf_band_seeded(
    verts: &[glam::Vec3],
    indices: &[u32],
    bounds: f32,
    res: u32,
    aabb: [u32; 6],
    band: f32,
    prev_air: &dyn Fn(u32, u32, u32) -> bool,
) -> Option<Vec<f32>> {
    region_mesh_to_sdf_band_seeded_stats(verts, indices, bounds, res, aabb, band, prev_air)
        .map(|(f, _)| f)
}

/// [`region_mesh_to_sdf_band_seeded`] that also reports region-local enclosed-void fills.
pub fn region_mesh_to_sdf_band_seeded_stats(
    verts: &[glam::Vec3],
    indices: &[u32],
    bounds: f32,
    res: u32,
    aabb: [u32; 6],
    band: f32,
    prev_air: &dyn Fn(u32, u32, u32) -> bool,
) -> Option<(Vec<f32>, BandSignStats)> {
    if verts.is_empty() || indices.is_empty() || res < 2 { return None; }
    let [gx0, gy0, gz0, gx1, gy1, gz1] = aabb;
    if gx0 > gx1 || gy0 > gy1 || gz0 > gz1 { return None; }
    let nx = (gx1 - gx0 + 1) as usize;
    let ny = (gy1 - gy0 + 1) as usize;
    let nz = (gz1 - gz0 + 1) as usize;
    let cell_size = (bounds * 2.0) / (res as f32 - 1.0);
    let band = band.max(cell_size * 1.7321).max(1e-6); // √3 — same seal clamp as the full variant

    let tris = tri_soup(verts, indices)?;
    let mut out = band_scatter(&tris, bounds, res, aabb, band);

    // Same two-attempt sign as the full variant, with the border seed rule this variant
    // exists for: a dirty region's border legitimately cuts through the solid, so only the
    // border cells that were AIR in the previous field may seed. Blocking is by surface
    // crossing, so no walkability (i.e. no normal) enters the decision.
    let air_before =
        |x: usize, y: usize, z: usize| prev_air(gx0 + x as u32, gy0 + y as u32, gz0 + z as u32);
    let cross = band_edge_crossings(&tris, bounds, res, aabb);
    let mut outside = vec![false; nx * ny * nz];
    let stack = seed_border(nx, ny, nz, &mut outside, air_before);
    flood_reach_crossings(&cross, nx, ny, nz, stack, &mut outside);
    if outside.iter().any(|&b| !b) {
        let stats = band_finalize_topological(&mut out, &outside, band);
        return Some((out, stats));
    }

    // Open input: fall back to the facing-based flood (see the full variant).
    let mut outside = vec![false; nx * ny * nz];
    let stack = seed_border(nx, ny, nz, &mut outside, |x, y, z| {
        band_walkable(out[x + y * nx + z * nx * ny]) && air_before(x, y, z)
    });
    flood_air_reach(&out, nx, ny, nz, stack, &mut outside);
    let stats = band_finalize(&mut out, &outside, band);
    Some((out, stats))
}

/// Mark and collect every cell on the region's 6 border faces that `accept` admits, as flood
/// seeds. Shared by both band samplers (and both of their sign attempts) so the seed set is
/// defined in exactly one place — the two used to carry near-identical copies of this loop.
fn seed_border<F: FnMut(usize, usize, usize) -> bool>(
    nx: usize,
    ny: usize,
    nz: usize,
    outside: &mut [bool],
    mut accept: F,
) -> Vec<(usize, usize, usize)> {
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();
    let mut seed = |x: usize, y: usize, z: usize, outside: &mut [bool], stack: &mut Vec<_>| {
        let i = x + y * nx + z * nx * ny;
        if !outside[i] && accept(x, y, z) {
            outside[i] = true;
            stack.push((x, y, z));
        }
    };
    for y in 0..ny {
        for x in 0..nx {
            seed(x, y, 0, outside, &mut stack);
            seed(x, y, nz - 1, outside, &mut stack);
        }
    }
    for z in 0..nz {
        for x in 0..nx {
            seed(x, 0, z, outside, &mut stack);
            seed(x, ny - 1, z, outside, &mut stack);
        }
        for y in 0..ny {
            seed(0, y, z, outside, &mut stack);
            seed(nx - 1, y, z, outside, &mut stack);
        }
    }
    stack
}

/// Walkability for the air-connectivity sign flood: far (NaN) or in-band POSITIVE (air
/// near a wall). Walking through in-band air is load-bearing: a cavity connected to the
/// outside through a throat narrower than 2·band has NO far cells in the throat — a
/// far-only flood would wall it off and fill genuinely open space (mug-neck phantom).
/// Only the in-band NEGATIVE layer (solid) blocks, and it is ≥ band ≥ √3·cell thick on
/// the solid side of every watertight surface — the seal.
#[inline]
fn band_walkable(v: f32) -> bool {
    v.is_nan() || v > 0.0
}

/// World-coordinate span → inclusive region index range on one axis (clamped; `None` if the
/// span misses the region). Shared by the band scatter and the crossing pass so both agree
/// cell-for-cell on which lattice points a triangle can touch.
#[inline]
fn region_axis_range(
    wmin: f32,
    wmax: f32,
    bounds: f32,
    cell_size: f32,
    g0: u32,
    n: usize,
) -> Option<(usize, usize)> {
    let lo = ((wmin + bounds) / cell_size).ceil() as i64 - g0 as i64;
    let hi = ((wmax + bounds) / cell_size).floor() as i64 - g0 as i64;
    let lo = lo.max(0);
    let hi = hi.min(n as i64 - 1);
    if lo > hi { None } else { Some((lo as usize, hi as usize)) }
}

/// Per-z-plane triangle buckets: plane `zi` lists every triangle whose AABB, dilated by
/// `dilate`, reaches that lattice plane. Ascending triangle order is preserved (the band
/// scatter's first-wins tie-break depends on it).
fn plane_buckets(
    tris: &[Tri],
    bounds: f32,
    cell_size: f32,
    gz0: u32,
    nz: usize,
    dilate: f32,
) -> Vec<Vec<u32>> {
    let mut out: Vec<Vec<u32>> = vec![Vec::new(); nz];
    for (ti, t) in tris.iter().enumerate() {
        if let Some((z0, z1)) =
            region_axis_range(t.bmin.z - dilate, t.bmax.z + dilate, bounds, cell_size, gz0, nz)
        {
            for zi in z0..=z1 {
                out[zi].push(ti as u32);
            }
        }
    }
    out
}

/// Narrow-band scatter shared by both band samplers: exact signed distance for every cell
/// within `band` of the surface, NaN elsewhere. Buckets preserve ascending triangle order
/// and best-d² uses strict `<`, so ties resolve to the same triangle as the brute path
/// (first wins) — the source of the bit-exactness the golden tests assert.
///
/// The SIGN written here is the nearest triangle's facing (`(p−q)·n`), which is only valid
/// for a closed, consistently outward-wound, non-self-intersecting mesh. It is no longer
/// what decides inside/outside — see [`band_edge_crossings`] — but it is still the value the
/// agreeing cells keep bit-for-bit, and `sealed_pocket` is still defined against it.
fn band_scatter(tris: &[Tri], bounds: f32, res: u32, aabb: [u32; 6], band: f32) -> Vec<f32> {
    use rayon::prelude::*;
    let [gx0, gy0, gz0, gx1, gy1, gz1] = aabb;
    let nx = (gx1 - gx0 + 1) as usize;
    let ny = (gy1 - gy0 + 1) as usize;
    let nz = (gz1 - gz0 + 1) as usize;
    let cell_size = (bounds * 2.0) / (res as f32 - 1.0);
    let first = |g: u32| -bounds + g as f32 * cell_size;

    // Per-plane triangle buckets. Dilate by band + one cell of float-rounding margin, so
    // no cell whose true distance is ≤ band can be missed by index truncation.
    let dilate = band + cell_size;
    let axis_range = |wmin: f32, wmax: f32, g0: u32, n: usize| -> Option<(usize, usize)> {
        region_axis_range(wmin, wmax, bounds, cell_size, g0, n)
    };
    let plane_tris = plane_buckets(tris, bounds, cell_size, gz0, nz, dilate);

    // Squared distance from a lattice point to a triangle's (undilated) AABB — ~10-flop
    // pre-cull for the ~50-flop `closest_on_tri`. Output-preserving: aabb_d2 > band² ⇒
    // true d² > band² ⇒ this triangle's value could only land on a cell the band filter
    // discards anyway (and can never beat an in-band winner).
    let aabb_d2 = |p: Vec3, bmin: Vec3, bmax: Vec3| -> f32 {
        let d = (bmin - p).max(p - bmax).max(Vec3::ZERO);
        d.length_squared()
    };
    let band2 = band * band;
    let mut out = vec![f32::NAN; nx * ny * nz];
    out.par_chunks_mut(nx * ny).enumerate().for_each(|(zi, plane)| {
        let cand = &plane_tris[zi];
        if cand.is_empty() { return; }
        let z = first(gz0 + zi as u32);
        // (best d², sign at best) per plane cell — sqrt deferred to the in-band winners.
        let mut best = vec![(f32::INFINITY, 0.0f32); nx * ny];
        for &ti in cand {
            let t = &tris[ti as usize];
            let Some((x0, x1)) = axis_range(t.bmin.x - dilate, t.bmax.x + dilate, gx0, nx) else { continue };
            let Some((y0, y1)) = axis_range(t.bmin.y - dilate, t.bmax.y + dilate, gy0, ny) else { continue };
            for yi in y0..=y1 {
                let y = first(gy0 + yi as u32);
                for xi in x0..=x1 {
                    let p = Vec3::new(first(gx0 + xi as u32), y, z);
                    if aabb_d2(p, t.bmin, t.bmax) > band2 {
                        continue;
                    }
                    let (q, d2) = closest_on_tri(p, t);
                    let b = &mut best[xi + yi * nx];
                    if d2 < b.0 {
                        let s = if (p - q).dot(t.n) >= 0.0 { 1.0 } else { -1.0 };
                        *b = (d2, s);
                    }
                }
            }
        }
        for (i, &(d2, s)) in best.iter().enumerate() {
            if d2 <= band2 {
                // Same arithmetic as `signed_from_best`: sign × √d² — bit-identical.
                plane[i] = s * d2.sqrt();
            }
        }
    });
    out
}

/// Does the segment `p → p + dir` intersect triangle `t`? Möller–Trumbore, **two-sided** —
/// no determinant-sign cull, so the answer does not depend on triangle winding.
///
/// The epsilons are deliberately INCLUSIVE (they widen the triangle and the segment by a hair)
/// because this test is a barrier: a false positive costs one over-blocked lattice segment,
/// a false negative punches a hole through the surface and lets the sign flood invert an
/// entire interior. A crossing that lands exactly on a shared edge must be reported by at
/// least one of the two triangles, and a crossing exactly at a lattice point blocks both of
/// the segments meeting there.
#[inline]
fn seg_hits_tri(p: Vec3, dir: Vec3, t: &Tri) -> bool {
    const E: f32 = 1e-6;
    let e1 = t.b - t.a;
    let e2 = t.c - t.a;
    let pv = dir.cross(e2);
    let det = e1.dot(pv);
    if det.abs() < 1e-20 {
        return false; // segment parallel to the triangle plane
    }
    let inv = 1.0 / det;
    let tv = p - t.a;
    let u = tv.dot(pv) * inv;
    if !(-E..=1.0 + E).contains(&u) {
        return false;
    }
    let qv = tv.cross(e1);
    let v = dir.dot(qv) * inv;
    if v < -E || u + v > 1.0 + E {
        return false;
    }
    let s = e2.dot(qv) * inv;
    (-E..=1.0 + E).contains(&s)
}

/// SURFACE CROSSINGS on the lattice segments, one byte per region cell:
/// bit 0 = a source triangle intersects the segment to the +X neighbour, bit 1 = +Y, bit 2 = +Z.
///
/// This is the sign oracle. "Which side of the nearest triangle's plane am I on" answers
/// inside/outside only for a mesh that is closed, consistently outward-wound AND free of
/// self-intersection; it is wrong — silently, locally, by a full ±band — for every mesh that
/// is not, which is most real models:
///   * a model assembled from interpenetrating parts (body + ear + leg) reads AIR at points
///     genuinely inside one part, because the nearest triangle belongs to the other part and
///     faces away;
///   * a handful of inconsistently-wound polygons inverts the field around them, which
///     punches a channel through the near shell and floods the whole interior;
///   * an inside-out mesh inverts everywhere.
/// Those wrong in-band signs are exactly the cells the iso-surface is extracted from, so they
/// come out as blades sticking through the surface, internal shells and torn patches.
///
/// "Can I walk here from outside without passing through the surface" has none of those
/// failure modes: it uses no normal, so winding cannot affect it, and it is a property of the
/// *geometry*, so a point enclosed by any part of a self-intersecting union is correctly
/// enclosed. By Jordan–Brouwer, a 6-connected lattice path from the region border to an
/// interior cell must cross the surface, and the crossing lies on one of the path's segments —
/// so the blocked-segment barrier is watertight whenever the input surface is.
///
/// A triangle can only cross a segment whose origin lies within one cell of that triangle (the
/// crossing point is on both), so one cell of dilation is exact rather than a tuned constant.
fn band_edge_crossings(tris: &[Tri], bounds: f32, res: u32, aabb: [u32; 6]) -> Vec<u8> {
    use rayon::prelude::*;
    let [gx0, gy0, gz0, gx1, gy1, gz1] = aabb;
    let nx = (gx1 - gx0 + 1) as usize;
    let ny = (gy1 - gy0 + 1) as usize;
    let nz = (gz1 - gz0 + 1) as usize;
    let cell_size = (bounds * 2.0) / (res as f32 - 1.0);
    let first = |g: u32| -bounds + g as f32 * cell_size;
    // One cell, plus a hair so `ceil`/`floor` index truncation cannot drop a boundary point.
    let dilate = cell_size * 1.0001;
    let buckets = plane_buckets(tris, bounds, cell_size, gz0, nz, dilate);

    let mut out = vec![0u8; nx * ny * nz];
    out.par_chunks_mut(nx * ny).enumerate().for_each(|(zi, plane)| {
        let cand = &buckets[zi];
        if cand.is_empty() {
            return;
        }
        let z = first(gz0 + zi as u32);
        let has_zn = zi + 1 < nz;
        for &ti in cand {
            let t = &tris[ti as usize];
            let Some((x0, x1)) = region_axis_range(
                t.bmin.x - dilate, t.bmax.x + dilate, bounds, cell_size, gx0, nx,
            ) else { continue };
            let Some((y0, y1)) = region_axis_range(
                t.bmin.y - dilate, t.bmax.y + dilate, bounds, cell_size, gy0, ny,
            ) else { continue };
            for yi in y0..=y1 {
                let y = first(gy0 + yi as u32);
                let has_yn = yi + 1 < ny;
                for xi in x0..=x1 {
                    let b = &mut plane[xi + yi * nx];
                    if *b == 0b111 {
                        continue;
                    }
                    let p = Vec3::new(first(gx0 + xi as u32), y, z);
                    if *b & 1 == 0 && xi + 1 < nx && seg_hits_tri(p, Vec3::X * cell_size, t) {
                        *b |= 1;
                    }
                    if *b & 2 == 0 && has_yn && seg_hits_tri(p, Vec3::Y * cell_size, t) {
                        *b |= 2;
                    }
                    if *b & 4 == 0 && has_zn && seg_hits_tri(p, Vec3::Z * cell_size, t) {
                        *b |= 4;
                    }
                }
            }
        }
    });
    out
}

/// Span-based 6-conn flood over EVERY cell, blocked by [`band_edge_crossings`] instead of by
/// field sign: a step is legal exactly when no triangle separates the two cells. `outside`
/// must already be `true` for the seeds. Same span structure as [`flood_air_reach`] — runs of
/// mutually-connected cells along x are expanded as a unit — with the run boundary now being a
/// crossing rather than a sign change.
fn flood_reach_crossings(
    cross: &[u8],
    nx: usize,
    ny: usize,
    nz: usize,
    mut stack: Vec<(usize, usize, usize)>,
    outside: &mut [bool],
) {
    let plane = nx * ny;
    while let Some((sx, y, z)) = stack.pop() {
        let row = y * nx + z * plane;
        // Expand to the full connected run [x0, x1] containing sx on row (y, z).
        let mut x0 = sx;
        while x0 > 0 && cross[row + x0 - 1] & 1 == 0 && !outside[row + x0 - 1] {
            x0 -= 1;
            outside[row + x0] = true;
        }
        let mut x1 = sx;
        while x1 + 1 < nx && cross[row + x1] & 1 == 0 && !outside[row + x1 + 1] {
            x1 += 1;
            outside[row + x1] = true;
        }
        // Scan the 4 neighbour rows. `gate_row` is the row holding the crossing bit for the
        // step (always the LOWER of the two cells on that axis).
        let mut visit_row = |nrow: usize,
                             gate_row: usize,
                             bit: u8,
                             ry: usize,
                             rz: usize,
                             stack: &mut Vec<(usize, usize, usize)>| {
            let mut x = x0;
            while x <= x1 {
                if cross[gate_row + x] & bit == 0 && !outside[nrow + x] {
                    outside[nrow + x] = true;
                    stack.push((x, ry, rz));
                    // Skip the rest of this connected run — the seed expands it on pop.
                    while x + 1 <= x1 && cross[nrow + x] & 1 == 0 && !outside[nrow + x + 1] {
                        x += 1;
                    }
                }
                x += 1;
            }
        };
        if y > 0 {
            let n = (y - 1) * nx + z * plane;
            visit_row(n, n, 2, y - 1, z, &mut stack);
        }
        if y + 1 < ny {
            visit_row((y + 1) * nx + z * plane, row, 2, y + 1, z, &mut stack);
        }
        if z > 0 {
            let n = y * nx + (z - 1) * plane;
            visit_row(n, n, 4, y, z - 1, &mut stack);
        }
        if z + 1 < nz {
            visit_row(y * nx + (z + 1) * plane, row, 4, y, z + 1, &mut stack);
        }
    }
}

/// Span-based 6-conn air flood shared by both band samplers: marks every cell reachable
/// from the pre-marked `stack` seeds through walkable cells (see [`band_walkable`]).
/// Spans of consecutive walkable cells along x are flooded as units (scanline fill in 3D:
/// each popped span scans its 4 neighbour rows for overlapping unvisited walkable runs).
/// `outside` must already be `true` exactly for the seeds.
fn flood_air_reach(
    out: &[f32],
    nx: usize,
    ny: usize,
    nz: usize,
    mut stack: Vec<(usize, usize, usize)>,
    outside: &mut [bool],
) {
    while let Some((sx, y, z)) = stack.pop() {
        // Expand to the full walkable run [x0, x1] containing sx on row (y, z).
        let row = y * nx + z * nx * ny;
        let mut x0 = sx;
        while x0 > 0 && band_walkable(out[row + x0 - 1]) && !outside[row + x0 - 1] {
            x0 -= 1;
            outside[row + x0] = true;
        }
        let mut x1 = sx;
        while x1 + 1 < nx && band_walkable(out[row + x1 + 1]) && !outside[row + x1 + 1] {
            x1 += 1;
            outside[row + x1] = true;
        }
        // Scan the 4 neighbour rows for unvisited walkable cells overlapping [x0, x1].
        let mut visit_row = |ry: usize, rz: usize, stack: &mut Vec<(usize, usize, usize)>| {
            let nrow = ry * nx + rz * nx * ny;
            let mut x = x0;
            while x <= x1 {
                if band_walkable(out[nrow + x]) && !outside[nrow + x] {
                    outside[nrow + x] = true;
                    stack.push((x, ry, rz));
                    // Skip the rest of this contiguous run — the seed expands it on pop.
                    while x + 1 <= x1 && band_walkable(out[nrow + x + 1]) && !outside[nrow + x + 1] {
                        x += 1;
                    }
                }
                x += 1;
            }
        };
        if y > 0      { visit_row(y - 1, z, &mut stack); }
        if y + 1 < ny { visit_row(y + 1, z, &mut stack); }
        if z > 0      { visit_row(y, z - 1, &mut stack); }
        if z + 1 < nz { visit_row(y, z + 1, &mut stack); }
    }
}

/// Final rewrite shared by both band samplers. Far cells: reached → +band, enclosed →
/// −band. In-band AIR cells that the flood could NOT reach lie inside a sealed void: flip
/// them solid too, so the void fills with NO inner iso crossing (a kept positive ring
/// beside a −band core would extract a phantom floating shell — the double-crossing bug
/// the air-walk + this flip eliminate together).
/// Returns the number of AIR cells that were filled because the flood could not reach them
/// (i.e. cells belonging to a SEALED VOID). Zero means the field has no enclosed void —
/// which is what lets the incremental remesh path prove a local splice is sound (see
/// `voxel_remesh_mesh_incremental`): void FILLING is one-way, so a field that already
/// contains a filled void can never have that void re-opened by a local update.
/// Finalize for the TOPOLOGICAL sign (the [`band_edge_crossings`] flood): `outside[i]` already
/// says which side each cell is on, so the scatter's normal-derived sign is only kept for its
/// MAGNITUDE and for `sealed_pocket`.
///
/// Cells where the two agree are left untouched — not recomputed — so a well-formed mesh
/// produces a bit-identical field to the legacy path (what the golden gates assert). A cell
/// where they disagree is negated, which is exact in binary floating point.
fn band_finalize_topological(out: &mut [f32], outside: &[bool], band: f32) -> BandSignStats {
    let mut s = BandSignStats::default();
    for (i, v) in out.iter_mut().enumerate() {
        if v.is_nan() {
            if outside[i] {
                *v = band;
            } else {
                *v = -band;
                s.far_solid += 1;
            }
        } else if outside[i] {
            // Reachable from the border without crossing the surface ⇒ air, whatever the
            // nearest triangle's facing claimed.
            if *v < 0.0 {
                *v = -*v;
            }
        } else if *v > 0.0 {
            // Enclosed, yet the nearest triangle puts it on the air side: a sealed pocket's
            // lining (or, on a self-intersecting mesh, a cell the facing test got wrong).
            // Either way it is enclosed, so it fills solid — and either way the incremental
            // path must not splice this field locally, which is what the count gates.
            *v = -*v;
            s.sealed_pocket += 1;
        }
    }
    s
}

fn band_finalize(out: &mut [f32], outside: &[bool], band: f32) -> BandSignStats {
    let mut s = BandSignStats::default();
    for (i, v) in out.iter_mut().enumerate() {
        if v.is_nan() {
            if outside[i] {
                *v = band;
            } else {
                // Far cell the flood never reached. For a closed solid this is simply its
                // interior — the overwhelmingly common case, and NOT evidence of a void.
                *v = -band;
                s.far_solid += 1;
            }
        } else if *v > 0.0 && !outside[i] {
            // The surface puts this cell on the AIR side, yet the border flood could not reach
            // it. That is a sealed pocket's lining — the only signal that a genuine void was
            // filled. See `BandSignStats::sealed_pocket`.
            *v = -band;
            s.sealed_pocket += 1;
        }
    }
    s
}

/// Check if every edge is shared by exactly 2 triangles (closed, watertight mesh).
fn is_watertight(indices: &[u32]) -> bool {
    use std::collections::HashMap;
    let mut edge_count: HashMap<(u32, u32), u32> = HashMap::new();
    for tri in indices.chunks(3) {
        if tri.len() < 3 { return false; }
        for i in 0..3 {
            let a = tri[i].min(tri[(i + 1) % 3]);
            let b = tri[i].max(tri[(i + 1) % 3]);
            *edge_count.entry((a, b)).or_insert(0) += 1;
        }
    }
    edge_count.values().all(|&c| c == 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watertight, centred, axis-aligned cube of half-extent `h` (12 triangles, outward winding).
    fn cube(h: f32) -> (Vec<glam::Vec3>, Vec<u32>) {
        let v: Vec<glam::Vec3> = [
            [-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h], // z-
            [-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h],     // z+
        ].iter().map(|a| glam::Vec3::new(a[0], a[1], a[2])).collect();
        // CCW outward faces.
        let idx: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, // z- (facing -z)
            4, 5, 6, 4, 6, 7, // z+
            0, 1, 5, 0, 5, 4, // y-
            3, 7, 6, 3, 6, 2, // y+
            0, 4, 7, 0, 7, 3, // x-
            1, 2, 6, 1, 6, 5, // x+
        ];
        (v, idx)
    }

    #[test]
    fn region_mesh_to_sdf_aligns_to_lattice_and_signs() {
        // MOVE Phase-2 helper. A centred cube (half 0.5) on a 32³ grid over bounds 1.0. Sampling the
        // FULL grid back: the centre cell must be deep inside (SDF very negative), a corner cell outside
        // (SDF positive), and the count of inside cells must be ~the cube's voxel volume (lattice aligned
        // — a transpose or half-cell offset bug would shift/rotate the inside region and break this).
        let (verts, indices) = cube(0.5);
        let (res, bounds) = (32u32, 1.0f32);
        let aabb = [0u32, 0, 0, res - 1, res - 1, res - 1];
        let sdf = region_mesh_to_sdf(&verts, &indices, bounds, res, aabb).expect("sdf");
        let n = res as usize;
        assert_eq!(sdf.len(), n * n * n);
        let at = |x: usize, y: usize, z: usize| sdf[x + y * n + z * n * n];
        let c = n / 2;
        assert!(at(c, c, c) < -0.2, "cube centre must be well inside (sdf {} )", at(c, c, c));
        assert!(at(0, 0, 0) > 0.0, "grid corner must be outside the cube (sdf {})", at(0, 0, 0));
        assert!(at(n - 1, n - 1, n - 1) > 0.0, "far corner must be outside");
        // Lattice check: cell at world (0,0,0) is the centre → inside; cell at world ~(0.9,0,0) → outside.
        // (x index for world 0.9: i = (0.9+1)/2 * (res-1) ≈ 29.4 → 29.)
        assert!(at(c, c, c) < 0.0 && at(29, c, c) > 0.0,
            "the inside region must be centred on the cube, not transposed/offset");
        // Inside-cell count ≈ a 1.0-side cube over a 2.0 box at 32³ ⇒ ~half-extent in cells each axis.
        let inside = sdf.iter().filter(|&&d| d < 0.0).count();
        // The cube spans [-0.5,0.5] of the [-1,1] box = half each axis ⇒ ~ (res/2)³ cells, allow slack.
        let lo = (n / 4) * (n / 4) * (n / 4); // generous lower bound
        assert!(inside > lo, "inside count {inside} too small — lattice misaligned? (lo {lo})");
    }
}
