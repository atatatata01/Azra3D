//! High → low **normal bake** for Detail LO bake-preview (Quick Normal).
//!
//! Casts rays from each low-poly vertex along ±low normal, hits the high-poly mesh,
//! and writes the high surface normal onto the low vertex. Object-space transfer —
//! no UVs required. Viewport shading on the LO mesh then shows whether the LO
//! topology is good enough for a normal bake (soft-K fillets, edges, etc.).

use glam::Vec3;

/// Bake high-poly normals onto low-poly vertices (object space).
///
/// `max_dist` = max ray length (world). Misses keep the low normal.
pub fn bake_normals_high_to_low(
    high_pos: &[Vec3],
    high_idx: &[u32],
    high_nrm: &[Vec3],
    low_pos: &[Vec3],
    low_nrm: &[Vec3],
    max_dist: f32,
) -> Vec<Vec3> {
    let n_low = low_pos.len();
    let mut out = low_nrm.to_vec();
    if n_low == 0 || high_idx.len() < 3 || high_pos.is_empty() {
        return out;
    }
    if out.len() != n_low {
        out = vec![Vec3::Y; n_low];
        for i in 0..n_low.min(low_nrm.len()) {
            out[i] = low_nrm[i];
        }
    }

    let grid = SpatialTriGrid::build(high_pos, high_idx, 24);
    let max_dist = max_dist.max(1e-4);
    let bias = (max_dist * 0.02).clamp(1e-4, 0.05);

    for i in 0..n_low {
        let p = low_pos[i];
        let mut n = low_nrm.get(i).copied().unwrap_or(Vec3::Y);
        if n.length_squared() < 1e-12 {
            n = Vec3::Y;
        } else {
            n = n.normalize();
        }
        // Prefer hit from outside (inflate along low normal) casting inward — classic cage.
        let origin_out = p + n * bias;
        let hit = raycast_closest(&grid, high_pos, high_idx, high_nrm, origin_out, -n, max_dist)
            .or_else(|| {
                // Fallback: from slightly inside outward.
                raycast_closest(
                    &grid,
                    high_pos,
                    high_idx,
                    high_nrm,
                    p - n * bias,
                    n,
                    max_dist,
                )
            });
        if let Some(hn) = hit {
            let hn = if hn.dot(n) < 0.0 { -hn } else { hn };
            out[i] = hn.normalize_or_zero();
            if out[i].length_squared() < 1e-12 {
                out[i] = n;
            }
        }
    }
    out
}

/// Uniform spatial hash of high-mesh triangles for ray queries.
struct SpatialTriGrid {
    cell: f32,
    origin: Vec3,
    dims: [i32; 3],
    /// cell_key -> triangle indices into high_idx/3
    cells: std::collections::HashMap<u32, Vec<u32>>,
}

impl SpatialTriGrid {
    fn build(pos: &[Vec3], idx: &[u32], target_cells: i32) -> Self {
        let mut bmin = Vec3::splat(f32::MAX);
        let mut bmax = Vec3::splat(f32::MIN);
        for t in idx.chunks_exact(3) {
            for &vi in t {
                if let Some(&p) = pos.get(vi as usize) {
                    bmin = bmin.min(p);
                    bmax = bmax.max(p);
                }
            }
        }
        if !bmin.is_finite() || !bmax.is_finite() {
            bmin = Vec3::ZERO;
            bmax = Vec3::ONE;
        }
        let extent = (bmax - bmin).max(Vec3::splat(1e-4));
        let max_e = extent.x.max(extent.y).max(extent.z);
        let target = target_cells.max(4) as f32;
        let cell = (max_e / target).max(1e-4);
        let dims = [
            ((extent.x / cell).ceil() as i32).clamp(1, 64),
            ((extent.y / cell).ceil() as i32).clamp(1, 64),
            ((extent.z / cell).ceil() as i32).clamp(1, 64),
        ];
        let mut cells: std::collections::HashMap<u32, Vec<u32>> =
            std::collections::HashMap::with_capacity((dims[0] * dims[1] * dims[2]) as usize);
        let n_tri = (idx.len() / 3) as u32;
        for ti in 0..n_tri {
            let i0 = idx[ti as usize * 3] as usize;
            let i1 = idx[ti as usize * 3 + 1] as usize;
            let i2 = idx[ti as usize * 3 + 2] as usize;
            let (Some(&p0), Some(&p1), Some(&p2)) =
                (pos.get(i0), pos.get(i1), pos.get(i2))
            else {
                continue;
            };
            let tmin = p0.min(p1).min(p2);
            let tmax = p0.max(p1).max(p2);
            let c0 = Self::cell_of(tmin, bmin, cell, dims);
            let c1 = Self::cell_of(tmax, bmin, cell, dims);
            for z in c0[2]..=c1[2] {
                for y in c0[1]..=c1[1] {
                    for x in c0[0]..=c1[0] {
                        let key = Self::pack(x, y, z, dims);
                        cells.entry(key).or_default().push(ti);
                    }
                }
            }
        }
        Self {
            cell,
            origin: bmin,
            dims,
            cells,
        }
    }

    fn cell_of(p: Vec3, origin: Vec3, cell: f32, dims: [i32; 3]) -> [i32; 3] {
        let q = (p - origin) / cell;
        [
            (q.x.floor() as i32).clamp(0, dims[0] - 1),
            (q.y.floor() as i32).clamp(0, dims[1] - 1),
            (q.z.floor() as i32).clamp(0, dims[2] - 1),
        ]
    }

    fn pack(x: i32, y: i32, z: i32, dims: [i32; 3]) -> u32 {
        (z * dims[0] * dims[1] + y * dims[0] + x) as u32
    }
}

fn raycast_closest(
    grid: &SpatialTriGrid,
    pos: &[Vec3],
    idx: &[u32],
    nrm: &[Vec3],
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<Vec3> {
    let dir = dir.normalize_or_zero();
    if dir.length_squared() < 1e-12 {
        return None;
    }
    // Walk cells along the ray (DDA-ish: sample steps).
    let steps = ((max_dist / grid.cell).ceil() as i32).clamp(4, 128);
    let step = max_dist / steps as f32;
    let mut best_t = max_dist;
    let mut best_n = None;
    let mut visited = std::collections::HashSet::new();

    for s in 0..=steps {
        let p = origin + dir * (s as f32 * step);
        let c = SpatialTriGrid::cell_of(p, grid.origin, grid.cell, grid.dims);
        // 3×3×3 neighborhood
        for dz in -1i32..=1 {
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let x = (c[0] + dx).clamp(0, grid.dims[0] - 1);
                    let y = (c[1] + dy).clamp(0, grid.dims[1] - 1);
                    let z = (c[2] + dz).clamp(0, grid.dims[2] - 1);
                    let key = SpatialTriGrid::pack(x, y, z, grid.dims);
                    if !visited.insert(key) {
                        continue;
                    }
                    let Some(tris) = grid.cells.get(&key) else {
                        continue;
                    };
                    for &ti in tris {
                        let base = ti as usize * 3;
                        if base + 2 >= idx.len() {
                            continue;
                        }
                        let i0 = idx[base] as usize;
                        let i1 = idx[base + 1] as usize;
                        let i2 = idx[base + 2] as usize;
                        let (Some(&p0), Some(&p1), Some(&p2)) =
                            (pos.get(i0), pos.get(i1), pos.get(i2))
                        else {
                            continue;
                        };
                        if let Some((t, u, v)) = ray_tri(origin, dir, p0, p1, p2) {
                            if t > 1e-5 && t < best_t {
                                best_t = t;
                                // Interpolate vertex normals if available.
                                let n0 = nrm.get(i0).copied().unwrap_or_else(|| {
                                    (p1 - p0).cross(p2 - p0).normalize_or_zero()
                                });
                                let n1 = nrm.get(i1).copied().unwrap_or(n0);
                                let n2 = nrm.get(i2).copied().unwrap_or(n0);
                                let w = 1.0 - u - v;
                                let hn = (n0 * w + n1 * u + n2 * v).normalize_or_zero();
                                best_n = Some(if hn.length_squared() > 1e-12 {
                                    hn
                                } else {
                                    (p1 - p0).cross(p2 - p0).normalize_or_zero()
                                });
                            }
                        }
                    }
                }
            }
        }
        if best_t < s as f32 * step {
            break; // already found closer than further steps
        }
    }
    best_n
}

/// Möller–Trumbore. Returns (t, u, v) with hit = o + t*d, bary u,v on p1,p2.
fn ray_tri(o: Vec3, d: Vec3, p0: Vec3, p1: Vec3, p2: Vec3) -> Option<(f32, f32, f32)> {
    const EPS: f32 = 1e-7;
    let e1 = p1 - p0;
    let e2 = p2 - p0;
    let pvec = d.cross(e2);
    let det = e1.dot(pvec);
    if det.abs() < EPS {
        return None;
    }
    let inv = 1.0 / det;
    let tvec = o - p0;
    let u = tvec.dot(pvec) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qvec = tvec.cross(e1);
    let v = d.dot(qvec) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(qvec) * inv;
    if t < EPS {
        return None;
    }
    Some((t, u, v))
}

/// Apply baked normals into raster verts `[px,py,pz,nx,ny,nz,u,v]`.
pub fn apply_baked_normals_to_raster(verts: &mut [[f32; 8]], baked: &[Vec3]) {
    let n = verts.len().min(baked.len());
    for i in 0..n {
        let bn = baked[i];
        verts[i][3] = bn.x;
        verts[i][4] = bn.y;
        verts[i][5] = bn.z;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bake_sphere_ish_keeps_normals_finite() {
        // Unit cube high + low
        let high_pos = vec![
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let high_idx = vec![
            0, 1, 2, 0, 2, 3, // -Z
            4, 6, 5, 4, 7, 6, // +Z
            0, 4, 5, 0, 5, 1, // -Y
            2, 6, 7, 2, 7, 3, // +Y
            0, 3, 7, 0, 7, 4, // -X
            1, 5, 6, 1, 6, 2, // +X
        ];
        let high_nrm: Vec<Vec3> = high_pos.iter().map(|p| p.normalize_or_zero()).collect();
        let low_pos = high_pos.clone();
        let low_nrm = high_nrm.clone();
        let out = bake_normals_high_to_low(
            &high_pos, &high_idx, &high_nrm, &low_pos, &low_nrm, 0.5,
        );
        assert_eq!(out.len(), low_pos.len());
        for n in &out {
            assert!(n.is_finite());
            assert!(n.length() > 0.5);
        }
    }
}
