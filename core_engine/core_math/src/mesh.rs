// core_engine/core_math/src/mesh.rs
// Dual Contouring mesh extraction from SDF + OBJ export
use glam::Vec3;
use rayon::prelude::*;
use crate::primitives::{SdfPrimitive, eval_scene, eval_normal};

// ============================================================
// Mesh Data Structure
// ============================================================
pub struct Mesh {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub quads: Vec<[u32; 4]>,
}

impl Mesh {
    /// Convert quads to triangles (for engines that don't support quads)
    pub fn triangulated_indices(&self) -> Vec<[u32; 3]> {
        let mut tris = Vec::with_capacity(self.quads.len() * 2);
        for q in &self.quads {
            tris.push([q[0], q[1], q[2]]);
            tris.push([q[0], q[2], q[3]]);
        }
        tris
    }

    pub fn triangle_count(&self) -> usize { self.quads.len() * 2 }
    pub fn quad_count(&self) -> usize { self.quads.len() }
    pub fn vertex_count(&self) -> usize { self.vertices.len() }

    /// Convert to GPU-ready raster data: flat vertex array (pos+normal+uv) + triangle index array.
    ///
    /// UVs are set to zero (placeholder for future texturing).
    /// Returns `(vertices, indices)` where each vertex is `[px, py, pz, nx, ny, nz, u, v]`.
    pub fn to_raster_data(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        let mut verts = Vec::with_capacity(self.vertices.len());
        for i in 0..self.vertices.len() {
            let v = self.vertices[i];
            let n = if i < self.normals.len() {
                self.normals[i]
            } else {
                Vec3::Y
            };
            verts.push([v.x, v.y, v.z, n.x, n.y, n.z, 0.0, 0.0]);
        }
        let mut indices = Vec::with_capacity(self.quads.len() * 6);
        for q in &self.quads {
            // Two triangles per quad
            indices.push(q[0]);
            indices.push(q[1]);
            indices.push(q[2]);
            indices.push(q[0]);
            indices.push(q[2]);
            indices.push(q[3]);
        }
        (verts, indices)
    }

    /// Export as Wavefront OBJ (quads)
    pub fn export_obj(&self, path: &str) -> Result<(), String> {
        use std::io::Write;
        let mut f = std::fs::File::create(path).map_err(|e| format!("Cannot create {}: {}", path, e))?;
        writeln!(f, "# AZRA — Dual Contouring Export").map_err(|e| e.to_string())?;
        writeln!(f, "# Vertices: {} | Quads: {} | Triangles: {}",
            self.vertices.len(), self.quads.len(), self.quads.len() * 2).map_err(|e| e.to_string())?;
        writeln!(f, "o Azra_Export").map_err(|e| e.to_string())?;

        // Vertices
        for v in &self.vertices {
            writeln!(f, "v {:.6} {:.6} {:.6}", v.x, v.y, v.z).map_err(|e| e.to_string())?;
        }

        // Normals
        for n in &self.normals {
            writeln!(f, "vn {:.6} {:.6} {:.6}", n.x, n.y, n.z).map_err(|e| e.to_string())?;
        }

        // Faces (quads, 1-indexed)
        for q in &self.quads {
            writeln!(f, "f {}//{} {}//{} {}//{} {}//{}",
                q[0]+1, q[0]+1, q[1]+1, q[1]+1, q[2]+1, q[2]+1, q[3]+1, q[3]+1)
                .map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    /// Export as OBJ with triangles only (max compatibility)
    pub fn export_obj_tris(&self, path: &str) -> Result<(), String> {
        use std::io::Write;
        let mut f = std::fs::File::create(path).map_err(|e| format!("Cannot create {}: {}", path, e))?;
        writeln!(f, "# AZRA — Dual Contouring Export (Triangulated)").map_err(|e| e.to_string())?;
        writeln!(f, "# Vertices: {} | Triangles: {}",
            self.vertices.len(), self.quads.len() * 2).map_err(|e| e.to_string())?;
        writeln!(f, "o Azra_Export").map_err(|e| e.to_string())?;

        for v in &self.vertices {
            writeln!(f, "v {:.6} {:.6} {:.6}", v.x, v.y, v.z).map_err(|e| e.to_string())?;
        }
        for n in &self.normals {
            writeln!(f, "vn {:.6} {:.6} {:.6}", n.x, n.y, n.z).map_err(|e| e.to_string())?;
        }
        for q in &self.quads {
            let (a, b, c, d) = (q[0]+1, q[1]+1, q[2]+1, q[3]+1);
            writeln!(f, "f {}//{} {}//{} {}//{}", a, a, b, b, c, c).map_err(|e| e.to_string())?;
            writeln!(f, "f {}//{} {}//{} {}//{}", a, a, c, c, d, d).map_err(|e| e.to_string())?;
        }

        Ok(())
    }
}

// ============================================================
// Dual Contouring Configuration
// ============================================================
pub struct DcConfig {
    pub resolution: u32,      // grid cells per axis (64, 128, 256)
    pub bounds_min: Vec3,      // world-space AABB minimum
    pub bounds_max: Vec3,      // world-space AABB maximum
    pub sharp_features: bool,  // use QEF for sharp edges (vs simple averaging)
    pub normal_eps: f32,       // epsilon for normal computation
}

impl Default for DcConfig {
    fn default() -> Self {
        Self { resolution: 64, bounds_min: Vec3::splat(-3.0), bounds_max: Vec3::splat(3.0), sharp_features: true, normal_eps: 0.005 }
    }
}

// ============================================================
// Dual Contouring Algorithm
// ============================================================

/// Corner offsets for a cell: 8 corners indexed 0-7
pub(crate) const CORNER: [(usize, usize, usize); 8] = [
    (0,0,0), (1,0,0), (0,1,0), (1,1,0),
    (0,0,1), (1,0,1), (0,1,1), (1,1,1),
];

/// 12 edges of a cell: each connects two corners
pub(crate) const EDGES: [(usize, usize); 12] = [
    (0,1), (2,3), (4,5), (6,7),   // X-axis edges
    (0,2), (1,3), (4,6), (5,7),   // Y-axis edges
    (0,4), (1,5), (2,6), (3,7),   // Z-axis edges
];

/// Run Dual Contouring on the SDF scene defined by primitives
pub fn dual_contouring(primitives: &[SdfPrimitive], config: &DcConfig) -> Mesh {
    let res = config.resolution as usize;
    let bmin = config.bounds_min;
    let extent = config.bounds_max - config.bounds_min;
    let cell_size = extent / res as f32; // per-axis cell size (Vec3)
    let max_cell = cell_size.max_element(); // scalar for QEF sanity check
    let nverts = res + 1; // grid vertices per axis

    // Build visible list once — avoids a Vec allocation on every scene_sdf/scene_normal call.
    let visible: Vec<&SdfPrimitive> = primitives.iter().filter(|p| p.visible).collect();

    // Step 1: Sample SDF at all grid vertices (parallel over all nverts³ points)
    let total = nverts * nverts * nverts;
    let nv2 = nverts * nverts;
    let grid: Vec<f32> = (0..total).into_par_iter().map(|idx| {
        let z = idx / nv2;
        let y = (idx / nverts) % nverts;
        let x = idx % nverts;
        let p = Vec3::new(
            bmin.x + x as f32 * cell_size.x,
            bmin.y + y as f32 * cell_size.y,
            bmin.z + z as f32 * cell_size.z,
        );
        eval_scene(p, &visible)
    }).collect();

    // Step 2: For each cell containing a sign change, compute dual vertex via QEF
    let mut cell_vertex_idx: Vec<Option<u32>> = vec![None; res * res * res];
    let mut vertices: Vec<Vec3> = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();

    for z in 0..res {
        for y in 0..res {
            for x in 0..res {
                // Sample 8 corners of this cell
                let mut corners = [0.0f32; 8];
                for (i, &(dx, dy, dz)) in CORNER.iter().enumerate() {
                    corners[i] = grid[(x+dx) + (y+dy)*nverts + (z+dz)*nverts*nverts];
                }

                // Find edges with sign changes — collect crossing points and normals
                let mut crossings: Vec<(Vec3, Vec3)> = Vec::new();
                for &(a, b) in &EDGES {
                    if (corners[a] > 0.0) != (corners[b] > 0.0) {
                        // Linear interpolation to find surface crossing
                        let t = corners[a] / (corners[a] - corners[b]);
                        let (ax, ay, az) = CORNER[a];
                        let (bx, by, bz) = CORNER[b];
                        let pa = Vec3::new(
                            bmin.x + (x+ax) as f32 * cell_size.x,
                            bmin.y + (y+ay) as f32 * cell_size.y,
                            bmin.z + (z+az) as f32 * cell_size.z,
                        );
                        let pb = Vec3::new(
                            bmin.x + (x+bx) as f32 * cell_size.x,
                            bmin.y + (y+by) as f32 * cell_size.y,
                            bmin.z + (z+bz) as f32 * cell_size.z,
                        );
                        let crossing = pa + (pb - pa) * t;
                        let n = eval_normal(crossing, &visible, config.normal_eps);
                        crossings.push((crossing, n));
                    }
                }

                if crossings.is_empty() { continue; }

                // Compute cell center (fallback position)
                let cell_center = Vec3::new(
                    bmin.x + (x as f32 + 0.5) * cell_size.x,
                    bmin.y + (y as f32 + 0.5) * cell_size.y,
                    bmin.z + (z as f32 + 0.5) * cell_size.z,
                );

                // Solve QEF or average
                let vertex = if config.sharp_features && crossings.len() >= 3 {
                    solve_qef(&crossings, cell_center, max_cell)
                } else {
                    // Simple average of crossing points (Surface Nets fallback)
                    let sum: Vec3 = crossings.iter().map(|(p, _)| *p).fold(Vec3::ZERO, |a, b| a + b);
                    sum / crossings.len() as f32
                };

                // Clamp vertex to cell bounds (prevent escaping)
                let cell_min = Vec3::new(
                    bmin.x + x as f32 * cell_size.x,
                    bmin.y + y as f32 * cell_size.y,
                    bmin.z + z as f32 * cell_size.z,
                );
                let cell_max = cell_min + cell_size;
                let vertex = vertex.clamp(cell_min, cell_max);

                // Compute normal at the vertex position
                let normal = eval_normal(vertex, &visible, config.normal_eps);

                let idx = vertices.len() as u32;
                vertices.push(vertex);
                normals.push(normal);
                cell_vertex_idx[x + y * res + z * res * res] = Some(idx);
            }
        }
    }

    // Step 3: Generate quads — for each grid edge with sign change, connect 4 adjacent cells
    let mut quads: Vec<[u32; 4]> = Vec::new();

    let cell_idx = |cx: usize, cy: usize, cz: usize| -> Option<u32> {
        if cx < res && cy < res && cz < res {
            cell_vertex_idx[cx + cy * res + cz * res * res]
        } else {
            None
        }
    };

    // X-axis edges: from grid vertex (x,y,z) to (x+1,y,z)
    // Adjacent cells: (x, y-1, z-1), (x, y, z-1), (x, y-1, z), (x, y, z)
    for z in 1..res {
        for y in 1..res {
            for x in 0..res {
                let d0 = grid[x + y*nverts + z*nverts*nverts];
                let d1 = grid[(x+1) + y*nverts + z*nverts*nverts];
                if (d0 > 0.0) == (d1 > 0.0) { continue; }

                let v0 = cell_idx(x, y-1, z-1);
                let v1 = cell_idx(x, y,   z-1);
                let v2 = cell_idx(x, y,   z);
                let v3 = cell_idx(x, y-1, z);

                if let (Some(a), Some(b), Some(c), Some(d)) = (v0, v1, v2, v3) {
                    if d0 > 0.0 { quads.push([a, b, c, d]); }
                    else         { quads.push([d, c, b, a]); }
                }
            }
        }
    }

    // Y-axis edges: from grid vertex (x,y,z) to (x,y+1,z)
    // Adjacent cells: (x-1, y, z-1), (x, y, z-1), (x, y, z), (x-1, y, z)
    for z in 1..res {
        for y in 0..res {
            for x in 1..res {
                let d0 = grid[x + y*nverts + z*nverts*nverts];
                let d1 = grid[x + (y+1)*nverts + z*nverts*nverts];
                if (d0 > 0.0) == (d1 > 0.0) { continue; }

                let v0 = cell_idx(x-1, y, z-1);
                let v1 = cell_idx(x,   y, z-1);
                let v2 = cell_idx(x,   y, z);
                let v3 = cell_idx(x-1, y, z);

                if let (Some(a), Some(b), Some(c), Some(d)) = (v0, v1, v2, v3) {
                    if d0 > 0.0 { quads.push([a, b, c, d]); }
                    else         { quads.push([d, c, b, a]); }
                }
            }
        }
    }

    // Z-axis edges: from grid vertex (x,y,z) to (x,y,z+1)
    // Adjacent cells: (x-1, y-1, z), (x, y-1, z), (x, y, z), (x-1, y, z)
    for z in 0..res {
        for y in 1..res {
            for x in 1..res {
                let d0 = grid[x + y*nverts + z*nverts*nverts];
                let d1 = grid[x + y*nverts + (z+1)*nverts*nverts];
                if (d0 > 0.0) == (d1 > 0.0) { continue; }

                let v0 = cell_idx(x-1, y-1, z);
                let v1 = cell_idx(x,   y-1, z);
                let v2 = cell_idx(x,   y,   z);
                let v3 = cell_idx(x-1, y,   z);

                if let (Some(a), Some(b), Some(c), Some(d)) = (v0, v1, v2, v3) {
                    if d0 > 0.0 { quads.push([a, b, c, d]); }
                    else         { quads.push([d, c, b, a]); }
                }
            }
        }
    }

    Mesh { vertices, normals, quads }
}

// ============================================================
// Dual Contouring from pre-sampled grid (no SDF evaluation)
// ============================================================

/// Configuration for grid-based Dual Contouring.
pub struct DcGridConfig {
    pub nverts: u32,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub sharp_features: bool,
}

/// Run Dual Contouring on a pre-sampled SDF grid.
///
/// `grid` must have `nverts³` elements, indexed as `[x + y*nverts + z*nverts*nverts]`.
/// Normals are computed from the grid via central finite differences — no SDF
/// evaluation needed, making this significantly faster than `dual_contouring()`
/// when the grid is already available (e.g., from a body volume bake).
pub fn dual_contouring_from_grid(grid: &[f32], config: &DcGridConfig) -> Mesh {
    let nverts = config.nverts as usize;
    let res = nverts - 1;
    if res == 0 || grid.len() < nverts * nverts * nverts {
        return Mesh { vertices: Vec::new(), normals: Vec::new(), quads: Vec::new() };
    }

    let bmin = config.bounds_min;
    let cell_size = (config.bounds_max - config.bounds_min) / res as f32;
    let max_cell = cell_size.max_element();
    let nv2 = nverts * nverts;

    let sample = |x: usize, y: usize, z: usize| -> f32 {
        grid[x + y * nverts + z * nv2]
    };

    let grid_normal = |x: usize, y: usize, z: usize| -> Vec3 {
        let xm = if x > 0 { x - 1 } else { x };
        let xp = if x + 1 < nverts { x + 1 } else { x };
        let ym = if y > 0 { y - 1 } else { y };
        let yp = if y + 1 < nverts { y + 1 } else { y };
        let zm = if z > 0 { z - 1 } else { z };
        let zp = if z + 1 < nverts { z + 1 } else { z };
        let dx = sample(xp, y, z) - sample(xm, y, z);
        let dy = sample(x, yp, z) - sample(x, ym, z);
        let dz = sample(x, y, zp) - sample(x, y, zm);
        Vec3::new(dx, dy, dz).normalize_or_zero()
    };

    let interp_normal = |gx: f32, gy: f32, gz: f32| -> Vec3 {
        let ix = (gx.round() as usize).min(nverts - 1);
        let iy = (gy.round() as usize).min(nverts - 1);
        let iz = (gz.round() as usize).min(nverts - 1);
        grid_normal(ix, iy, iz)
    };

    let mut cell_vertex_idx: Vec<Option<u32>> = vec![None; res * res * res];
    let mut vertices: Vec<Vec3> = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();

    for z in 0..res {
        for y in 0..res {
            for x in 0..res {
                let mut corners = [0.0f32; 8];
                for (i, &(dx, dy, dz)) in CORNER.iter().enumerate() {
                    corners[i] = sample(x + dx, y + dy, z + dz);
                }

                let mut crossings: Vec<(Vec3, Vec3)> = Vec::new();
                for &(a, b) in &EDGES {
                    if (corners[a] > 0.0) != (corners[b] > 0.0) {
                        let t = corners[a] / (corners[a] - corners[b]);
                        let (ax, ay, az) = CORNER[a];
                        let (bx, by, bz) = CORNER[b];
                        let pa = Vec3::new(
                            bmin.x + (x + ax) as f32 * cell_size.x,
                            bmin.y + (y + ay) as f32 * cell_size.y,
                            bmin.z + (z + az) as f32 * cell_size.z,
                        );
                        let pb = Vec3::new(
                            bmin.x + (x + bx) as f32 * cell_size.x,
                            bmin.y + (y + by) as f32 * cell_size.y,
                            bmin.z + (z + bz) as f32 * cell_size.z,
                        );
                        let crossing = pa + (pb - pa) * t;
                        let gx = (x + ax) as f32 + t * (bx as f32 - ax as f32);
                        let gy = (y + ay) as f32 + t * (by as f32 - ay as f32);
                        let gz = (z + az) as f32 + t * (bz as f32 - az as f32);
                        crossings.push((crossing, interp_normal(gx, gy, gz)));
                    }
                }

                if crossings.is_empty() { continue; }

                let cell_center = Vec3::new(
                    bmin.x + (x as f32 + 0.5) * cell_size.x,
                    bmin.y + (y as f32 + 0.5) * cell_size.y,
                    bmin.z + (z as f32 + 0.5) * cell_size.z,
                );

                let vertex = if config.sharp_features && crossings.len() >= 3 {
                    solve_qef(&crossings, cell_center, max_cell)
                } else {
                    let sum: Vec3 = crossings.iter().map(|(p, _)| *p).fold(Vec3::ZERO, |a, b| a + b);
                    sum / crossings.len() as f32
                };

                let cell_min = Vec3::new(
                    bmin.x + x as f32 * cell_size.x,
                    bmin.y + y as f32 * cell_size.y,
                    bmin.z + z as f32 * cell_size.z,
                );
                let vertex = vertex.clamp(cell_min, cell_min + cell_size);

                let gx = (vertex.x - bmin.x) / cell_size.x;
                let gy = (vertex.y - bmin.y) / cell_size.y;
                let gz = (vertex.z - bmin.z) / cell_size.z;

                vertices.push(vertex);
                normals.push(interp_normal(gx, gy, gz));
                cell_vertex_idx[x + y * res + z * res * res] = Some(vertices.len() as u32 - 1);
            }
        }
    }

    let mut quads: Vec<[u32; 4]> = Vec::new();
    let cell_idx = |cx: usize, cy: usize, cz: usize| -> Option<u32> {
        if cx < res && cy < res && cz < res {
            cell_vertex_idx[cx + cy * res + cz * res * res]
        } else { None }
    };

    for z in 1..res {
        for y in 1..res {
            for x in 0..res {
                let d0 = sample(x, y, z);
                let d1 = sample(x + 1, y, z);
                if (d0 > 0.0) == (d1 > 0.0) { continue; }
                if let (Some(a), Some(b), Some(c), Some(d)) =
                    (cell_idx(x, y-1, z-1), cell_idx(x, y, z-1), cell_idx(x, y, z), cell_idx(x, y-1, z))
                {
                    if d0 > 0.0 { quads.push([a,b,c,d]); } else { quads.push([d,c,b,a]); }
                }
            }
        }
    }
    for z in 1..res {
        for y in 0..res {
            for x in 1..res {
                let d0 = sample(x, y, z);
                let d1 = sample(x, y+1, z);
                if (d0 > 0.0) == (d1 > 0.0) { continue; }
                if let (Some(a), Some(b), Some(c), Some(d)) =
                    (cell_idx(x-1, y, z-1), cell_idx(x, y, z-1), cell_idx(x, y, z), cell_idx(x-1, y, z))
                {
                    if d0 > 0.0 { quads.push([a,b,c,d]); } else { quads.push([d,c,b,a]); }
                }
            }
        }
    }
    for z in 0..res {
        for y in 1..res {
            for x in 1..res {
                let d0 = sample(x, y, z);
                let d1 = sample(x, y, z+1);
                if (d0 > 0.0) == (d1 > 0.0) { continue; }
                if let (Some(a), Some(b), Some(c), Some(d)) =
                    (cell_idx(x-1, y-1, z), cell_idx(x, y-1, z), cell_idx(x, y, z), cell_idx(x-1, y, z))
                {
                    if d0 > 0.0 { quads.push([a,b,c,d]); } else { quads.push([d,c,b,a]); }
                }
            }
        }
    }

    Mesh { vertices, normals, quads }
}

// ============================================================
// QEF Solver (Quadratic Error Function)
// Minimizes: E(v) = Σ (n_i · (v - p_i))²
// Normal equation: A^T A v = A^T b
// ============================================================

pub(crate) fn solve_qef(crossings: &[(Vec3, Vec3)], fallback: Vec3, cell_size: f32) -> Vec3 {
    // Build A^T A (3x3 symmetric) and A^T b (3-vector)
    // A^T A = Σ n_i ⊗ n_i  (outer product sum)
    // A^T b = Σ (n_i · p_i) * n_i
    let mut ata = [[0.0f32; 3]; 3]; // row-major
    let mut atb = [0.0f32; 3];

    for &(p, n) in crossings {
        let np = n.dot(p);
        // Accumulate outer product n ⊗ n
        ata[0][0] += n.x * n.x; ata[0][1] += n.x * n.y; ata[0][2] += n.x * n.z;
        ata[1][0] += n.y * n.x; ata[1][1] += n.y * n.y; ata[1][2] += n.y * n.z;
        ata[2][0] += n.z * n.x; ata[2][1] += n.z * n.y; ata[2][2] += n.z * n.z;
        // Accumulate A^T b
        atb[0] += np * n.x;
        atb[1] += np * n.y;
        atb[2] += np * n.z;
    }

    // Add Tikhonov regularization to pull toward cell center (prevents singularity)
    let reg = 0.01 * crossings.len() as f32;
    ata[0][0] += reg; ata[1][1] += reg; ata[2][2] += reg;
    atb[0] += reg * fallback.x;
    atb[1] += reg * fallback.y;
    atb[2] += reg * fallback.z;

    // Solve 3x3 system via Cramer's rule
    let det = ata[0][0] * (ata[1][1]*ata[2][2] - ata[1][2]*ata[2][1])
            - ata[0][1] * (ata[1][0]*ata[2][2] - ata[1][2]*ata[2][0])
            + ata[0][2] * (ata[1][0]*ata[2][1] - ata[1][1]*ata[2][0]);

    if det.abs() < 1e-10 {
        // Degenerate — fallback to average of crossing points
        let sum: Vec3 = crossings.iter().map(|(p, _)| *p).fold(Vec3::ZERO, |a, b| a + b);
        return sum / crossings.len() as f32;
    }

    let inv_det = 1.0 / det;

    let x = inv_det * (atb[0] * (ata[1][1]*ata[2][2] - ata[1][2]*ata[2][1])
                      - ata[0][1] * (atb[1]*ata[2][2] - ata[1][2]*atb[2])
                      + ata[0][2] * (atb[1]*ata[2][1] - ata[1][1]*atb[2]));

    let y = inv_det * (ata[0][0] * (atb[1]*ata[2][2] - ata[1][2]*atb[2])
                      - atb[0] * (ata[1][0]*ata[2][2] - ata[1][2]*ata[2][0])
                      + ata[0][2] * (ata[1][0]*atb[2] - atb[1]*ata[2][0]));

    let z = inv_det * (ata[0][0] * (ata[1][1]*atb[2] - atb[1]*ata[2][1])
                      - ata[0][1] * (ata[1][0]*atb[2] - atb[1]*ata[2][0])
                      + atb[0] * (ata[1][0]*ata[2][1] - ata[1][1]*ata[2][0]));

    // Sanity check: if result is too far from cell center, fall back
    let result = Vec3::new(x, y, z);
    if (result - fallback).length() > cell_size * 1.5 {
        let sum: Vec3 = crossings.iter().map(|(p, _)| *p).fold(Vec3::ZERO, |a, b| a + b);
        return sum / crossings.len() as f32;
    }

    // Mesher 2.0 feature projection lives on the GPU path only (gated by B2 toggle via
    // `set_dc_mesher2_feature`). CPU export dual contour stays stock QEF for stripability.
    result
}

/// Accumulating Dual-Contouring QEF (REMESH_ADAPTIVE_OCTREE_PLAN Gate 2 step 1).
///
/// Packed symmetric `AᵀA` (6 unique coeffs) + `Aᵀb` + `bᵀb` so an octree node can
/// *sum* quadrics and still compute the residual `E(v) = vᵀ(AᵀA)v − 2vᵀ(Aᵀb) + bᵀb`.
/// Tikhonov toward `fallback` is applied once at [`Qef::solve`], never in [`Qef::add_plane`],
/// matching [`solve_qef`]. `mass` is the raw sum of crossing positions (degenerate centroid),
/// not the regularisation target.
#[derive(Clone, Copy, Debug, Default)]
pub struct Qef {
    /// Symmetric AᵀA: xx, xy, xz, yy, yz, zz.
    pub ata: [f32; 6],
    pub atb: [f32; 3],
    pub mass: Vec3,
    pub n: u32,
    pub btb: f32,
}

impl Qef {
    #[inline]
    pub fn add_plane(&mut self, p: Vec3, nrm: Vec3) {
        let np = nrm.dot(p);
        self.ata[0] += nrm.x * nrm.x;
        self.ata[1] += nrm.x * nrm.y;
        self.ata[2] += nrm.x * nrm.z;
        self.ata[3] += nrm.y * nrm.y;
        self.ata[4] += nrm.y * nrm.z;
        self.ata[5] += nrm.z * nrm.z;
        self.atb[0] += np * nrm.x;
        self.atb[1] += np * nrm.y;
        self.atb[2] += np * nrm.z;
        self.btb += np * np;
        self.mass += p;
        self.n += 1;
    }

    #[inline]
    pub fn add(&mut self, o: &Qef) {
        for i in 0..6 {
            self.ata[i] += o.ata[i];
        }
        for i in 0..3 {
            self.atb[i] += o.atb[i];
        }
        self.btb += o.btb;
        self.mass += o.mass;
        self.n += o.n;
    }

    /// Same solve as [`solve_qef`]: Tikhonov toward `fallback`, Cramer, centroid fallback.
    pub fn solve(&self, fallback: Vec3, cell_size: f32) -> Vec3 {
        if self.n == 0 {
            return fallback;
        }
        let n = self.n as f32;
        let mut ata = [
            [self.ata[0], self.ata[1], self.ata[2]],
            [self.ata[1], self.ata[3], self.ata[4]],
            [self.ata[2], self.ata[4], self.ata[5]],
        ];
        let mut atb = self.atb;
        let reg = 0.01 * n;
        ata[0][0] += reg;
        ata[1][1] += reg;
        ata[2][2] += reg;
        atb[0] += reg * fallback.x;
        atb[1] += reg * fallback.y;
        atb[2] += reg * fallback.z;
        let det = ata[0][0] * (ata[1][1] * ata[2][2] - ata[1][2] * ata[2][1])
            - ata[0][1] * (ata[1][0] * ata[2][2] - ata[1][2] * ata[2][0])
            + ata[0][2] * (ata[1][0] * ata[2][1] - ata[1][1] * ata[2][0]);
        let centroid = self.mass / n;
        if det.abs() < 1e-10 {
            return centroid;
        }
        let inv_det = 1.0 / det;
        let x = inv_det
            * (atb[0] * (ata[1][1] * ata[2][2] - ata[1][2] * ata[2][1])
                - ata[0][1] * (atb[1] * ata[2][2] - ata[1][2] * atb[2])
                + ata[0][2] * (atb[1] * ata[2][1] - ata[1][1] * atb[2]));
        let y = inv_det
            * (ata[0][0] * (atb[1] * ata[2][2] - ata[1][2] * atb[2])
                - atb[0] * (ata[1][0] * ata[2][2] - ata[1][2] * ata[2][0])
                + ata[0][2] * (ata[1][0] * atb[2] - atb[1] * ata[2][0]));
        let z = inv_det
            * (ata[0][0] * (ata[1][1] * atb[2] - atb[1] * ata[2][1])
                - ata[0][1] * (ata[1][0] * atb[2] - atb[1] * ata[2][0])
                + atb[0] * (ata[1][0] * ata[2][1] - ata[1][1] * ata[2][0]));
        let result = Vec3::new(x, y, z);
        if (result - fallback).length() > cell_size * 1.5 {
            return centroid;
        }
        result
    }

    /// E(v) = vᵀ(AᵀA)v − 2vᵀ(Aᵀb) + bᵀb. Drives octree collapse; unused until Gate 2 step 3.
    pub fn residual(&self, v: Vec3) -> f32 {
        let ata_v = Vec3::new(
            self.ata[0] * v.x + self.ata[1] * v.y + self.ata[2] * v.z,
            self.ata[1] * v.x + self.ata[3] * v.y + self.ata[4] * v.z,
            self.ata[2] * v.x + self.ata[4] * v.y + self.ata[5] * v.z,
        );
        v.dot(ata_v) - 2.0 * Vec3::from(self.atb).dot(v) + self.btb
    }
}

// ============================================================
// Auto-detect bounds from primitives
// ============================================================
/// Compute tight AABB for all visible primitives using bounding_radius().
/// Returns (aabb_min, aabb_max) with 20% margin.
pub fn compute_scene_bounds(primitives: &[SdfPrimitive]) -> (Vec3, Vec3) {
    if primitives.is_empty() { return (Vec3::splat(-2.0), Vec3::splat(2.0)); }
    let mut aabb_min = Vec3::splat(f32::MAX);
    let mut aabb_max = Vec3::splat(f32::MIN);
    for p in primitives.iter().filter(|p| p.visible) {
        // A HalfSpace slice is an infinite plane — its bounding_radius() is a large sentinel (1e6)
        // so the per-prim cull never far-skips it. It cannot define a finite scene bound, so exclude
        // it here or a body containing a slice would blow the export/scene AABB up to 1e6.
        if matches!(p.shape, crate::primitives::SdfShape::HalfSpace) { continue; }
        let r = p.bounding_radius();
        aabb_min = aabb_min.min(p.position - Vec3::splat(r));
        aabb_max = aabb_max.max(p.position + Vec3::splat(r));
    }
    if aabb_min.x > aabb_max.x {
        return (Vec3::splat(-2.0), Vec3::splat(2.0));
    }
    let half_extent = (aabb_max - aabb_min) * 0.5;
    let margin = (half_extent * 0.2).max(Vec3::splat(0.1));
    (aabb_min - margin, aabb_max + margin)
}

#[cfg(test)]
mod qef_tests {
    use super::*;

    #[test]
    fn qef_solve_matches_solve_qef() {
        let crossings = [
            (Vec3::new(0.0, 0.0, 0.0), Vec3::X),
            (Vec3::new(1.0, 0.0, 0.0), Vec3::Y),
            (Vec3::new(0.5, 0.5, 0.0), Vec3::new(0.0, 0.0, 1.0)),
            (Vec3::new(0.2, 0.3, 0.4), Vec3::new(0.6, 0.8, 0.0).normalize()),
        ];
        let fallback = Vec3::splat(0.5);
        let cell = 1.0;
        let mut q = Qef::default();
        for &(p, n) in &crossings {
            q.add_plane(p, n);
        }
        let a = q.solve(fallback, cell);
        let b = solve_qef(&crossings, fallback, cell);
        assert_eq!(a.to_array(), b.to_array(), "Qef::solve must match solve_qef");
        assert!(q.residual(a) <= q.residual(fallback) + 1e-5);
    }

    #[test]
    fn qef_add_sums_planes() {
        let planes = [
            (Vec3::new(0.0, 0.0, 0.0), Vec3::X),
            (Vec3::new(1.0, 0.0, 0.0), Vec3::Y),
            (Vec3::new(0.5, 0.5, 0.0), Vec3::Z),
            (Vec3::new(0.2, 0.3, 0.4), Vec3::new(0.6, 0.8, 0.0).normalize()),
        ];
        let mut all = Qef::default();
        let mut a = Qef::default();
        let mut b = Qef::default();
        for (i, &(p, n)) in planes.iter().enumerate() {
            all.add_plane(p, n);
            if i < 2 {
                a.add_plane(p, n);
            } else {
                b.add_plane(p, n);
            }
        }
        a.add(&b);
        assert_eq!(a.n, all.n);
        assert_eq!(a.ata, all.ata);
        assert_eq!(a.atb, all.atb);
        assert_eq!(a.btb, all.btb);
        let fallback = Vec3::splat(0.5);
        assert_eq!(
            a.solve(fallback, 1.0).to_array(),
            all.solve(fallback, 1.0).to_array()
        );
    }
}

#[cfg(test)]
mod convert_probe {
    /// PROBE — `HalfEdgeMesh::from_mesh` on a `Mesh` with EMPTY quads.
    ///
    /// `MeshCache::insert_imported` builds exactly this shape (`quads: Vec::new()`, "imported
    /// meshes use triangles directly"), and `convert_imported_to_poly` feeds it to `from_mesh`.
    #[test]
    fn from_mesh_without_quads_yields_no_faces() {
        use crate::half_edge::HalfEdgeMesh;
        let m = super::Mesh {
            vertices: vec![
                glam::Vec3::ZERO,
                glam::Vec3::X,
                glam::Vec3::Y,
                glam::Vec3::new(1.0, 1.0, 0.0),
            ],
            normals: vec![glam::Vec3::Z; 4],
            quads: Vec::new(),
        };
        let he = HalfEdgeMesh::from_mesh(&m);
        eprintln!(
            "[PROBE] from_mesh(quads=[]) -> {} verts, {} faces",
            he.vertices.len(),
            he.faces.len()
        );
        assert_eq!(he.vertices.len(), 4, "vertices survive");
        assert_eq!(
            he.faces.len(),
            0,
            "PROBE RESULT: an imported mesh converts to a FACELESS point cloud"
        );
    }
}
