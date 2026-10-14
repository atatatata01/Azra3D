// chunked_dc/extract.rs -- per-chunk dual contouring extract + ChunkedMesh.
// GOD_FILES_SPLIT_PLAN: chunked_dc/{mod,grid,extract,stitch,tests}.rs
// Pure relocation from chunked_dc (sampling helpers, ChunkMesh, ChunkedMesh).

use std::collections::HashMap;
use glam::Vec3;
use crate::mesh::{solve_qef, CORNER, EDGES};

use super::{VolumeAccessor, FlatGridAccessor};

// ============================================================
// SDF gradient normals via trilinear interpolation
// ============================================================

/// Trilinear interpolation of the SDF grid at arbitrary fractional grid coordinates.
fn trilinear_sample(accessor: &dyn VolumeAccessor, nverts: usize, fx: f32, fy: f32, fz: f32) -> f32 {
    let max = nverts as f32 - 1.0;
    let fx = fx.clamp(0.0, max);
    let fy = fy.clamp(0.0, max);
    let fz = fz.clamp(0.0, max);

    let ix = (fx.floor() as usize).min(nverts.saturating_sub(2));
    let iy = (fy.floor() as usize).min(nverts.saturating_sub(2));
    let iz = (fz.floor() as usize).min(nverts.saturating_sub(2));

    let wx = fx - ix as f32;
    let wy = fy - iy as f32;
    let wz = fz - iz as f32;

    let c000 = accessor.sample(ix, iy, iz);
    let c100 = accessor.sample(ix + 1, iy, iz);
    let c010 = accessor.sample(ix, iy + 1, iz);
    let c110 = accessor.sample(ix + 1, iy + 1, iz);
    let c001 = accessor.sample(ix, iy, iz + 1);
    let c101 = accessor.sample(ix + 1, iy, iz + 1);
    let c011 = accessor.sample(ix, iy + 1, iz + 1);
    let c111 = accessor.sample(ix + 1, iy + 1, iz + 1);

    let c00 = c000 * (1.0 - wx) + c100 * wx;
    let c10 = c010 * (1.0 - wx) + c110 * wx;
    let c01 = c001 * (1.0 - wx) + c101 * wx;
    let c11 = c011 * (1.0 - wx) + c111 * wx;
    let c0 = c00 * (1.0 - wy) + c10 * wy;
    let c1 = c01 * (1.0 - wy) + c11 * wy;
    c0 * (1.0 - wz) + c1 * wz
}

/// SDF gradient normal via tetrahedron method (4 trilinear samples).
/// `fx/fy/fz` are fractional grid-vertex coordinates. Epsilon is half a voxel.
fn sdf_gradient_normal(accessor: &dyn VolumeAccessor, nverts: usize, fx: f32, fy: f32, fz: f32) -> Vec3 {
    let e = 0.5; // half-voxel in grid coordinates
    let k = [
        Vec3::new( 1.0, -1.0, -1.0),
        Vec3::new(-1.0, -1.0,  1.0),
        Vec3::new(-1.0,  1.0, -1.0),
        Vec3::new( 1.0,  1.0,  1.0),
    ];
    let mut n = Vec3::ZERO;
    for ki in &k {
        let d = trilinear_sample(accessor, nverts, fx + ki.x * e, fy + ki.y * e, fz + ki.z * e);
        n += *ki * d;
    }
    n.normalize_or_zero()
}

/// Replace degenerate (zero-length) per-vertex normals with gradient-free GEOMETRIC normals.
///
/// The per-vertex SDF-gradient normal collapses to EXACTLY zero wherever the baked field is
/// locally flat at the tetrahedron tap points: the 4 tap weights sum to zero (`Σ kᵢ = 0`), so
/// equal samples give `Σ kᵢ·d = 0` and `normalize_or_zero()` returns `Vec3::ZERO`. This happens on
/// CSG creases / clamped or quantized plateaus in the bake (subtract `max(d,-clip)`, bevel/blend
/// `smin`/`smax`). A zero normal → `normalize(0)`=NaN in the vertex shader → `ndl = 0` in deferred
/// lighting → ambient-only FLAT GRAY faces (the reported "data corruption" on beveled / subtracted
/// SDF boxes).
///
/// Area-weighted face normals are well-defined for every non-degenerate triangle and globally
/// consistent with the quad winding (which already encodes the SDF sign). We OVERRIDE only the
/// degenerate vertices — valid (unit-length) gradient normals are smooth and seamless across chunk
/// seams, so they stay. A single global sign vote (against the valid gradient normals) resolves the
/// winding→outward orientation for the whole chunk, so no per-face guesswork is needed.
/// Voxel meshes are unaffected (separate GPU Surface-Nets path on a smooth density field).
pub(crate) fn repair_degenerate_normals(vertices: &[Vec3], normals: &mut [Vec3], quads: &[[u32; 4]]) {
    // Valid gradient normals are unit length (~1); a degenerate one is exactly zero. 0.25 = (0.5)².
    const DEGEN_LEN2: f32 = 0.25;
    if quads.is_empty() || !normals.iter().any(|n| n.length_squared() < DEGEN_LEN2) {
        return; // nothing degenerate — keep the smooth gradient normals untouched
    }
    let mut geo: Vec<Vec3> = vec![Vec3::ZERO; vertices.len()];
    for q in quads {
        let [a, b, c, d] = *q;
        // Diagonal cross product: ⟂ the quad, magnitude ∝ area (natural area weighting); sign fixed
        // by the consistent winding, resolved to outward by the global vote below.
        let fnormal = (vertices[c as usize] - vertices[a as usize])
            .cross(vertices[d as usize] - vertices[b as usize]);
        geo[a as usize] += fnormal;
        geo[b as usize] += fnormal;
        geo[c as usize] += fnormal;
        geo[d as usize] += fnormal;
    }
    let mut agree = 0.0f32;
    for (i, n) in normals.iter().enumerate() {
        if n.length_squared() >= DEGEN_LEN2 { agree += geo[i].dot(*n); }
    }
    let sign = if agree < 0.0 { -1.0 } else { 1.0 };
    for (i, n) in normals.iter_mut().enumerate() {
        if n.length_squared() < DEGEN_LEN2 {
            *n = (geo[i] * sign).normalize_or_zero();
        }
    }
}


// ============================================================
// Per-chunk mesh data
// ============================================================

/// Mesh extracted from a single 32^3 chunk of the SDF grid.
pub struct ChunkMesh {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub quads: Vec<[u32; 4]>,
}

// ============================================================
// Chunked mesh container
// ============================================================

/// Holds independently-extracted chunk meshes for the full grid.
/// Only dirty chunks are re-extracted; clean chunks are preserved.
pub struct ChunkedMesh {
    pub(super) chunks: HashMap<(u32, u32, u32), ChunkMesh>,
    pub chunks_per_axis: u32,
    pub chunk_size: u32,
    pub grid_resolution: u32,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub sharp_features: bool,
    pub smooth_normals: bool,
}

impl ChunkedMesh {
    /// Create an empty chunked mesh for a given grid resolution and chunk size.
    ///
    /// `grid_resolution` is the number of cells per axis (e.g. 128).
    /// Grid vertex count (nverts) = grid_resolution + 1.
    /// `chunk_size` is cells per chunk axis (typically 32).
    /// The grid must be evenly divisible by chunk_size.
    pub fn new(
        grid_resolution: u32,
        chunk_size: u32,
        bounds_min: Vec3,
        bounds_max: Vec3,
        sharp_features: bool,
    ) -> Self {
        assert!(chunk_size > 0, "chunk_size must be > 0");
        let chunks_per_axis = (grid_resolution + chunk_size - 1) / chunk_size;
        Self {
            chunks: HashMap::new(),
            chunks_per_axis,
            chunk_size,
            grid_resolution,
            bounds_min,
            bounds_max,
            sharp_features,
            smooth_normals: true,
        }
    }

    /// Compute which chunk coordinates overlap a dirty region in voxel-cell space.
    ///
    /// `dirty_min` / `dirty_max` are cell coordinates (0..grid_resolution).
    pub fn dirty_chunk_coords(
        &self,
        dirty_min: [u32; 3],
        dirty_max: [u32; 3],
    ) -> Vec<(u32, u32, u32)> {
        let cs = self.chunk_size;
        let cpa = self.chunks_per_axis;

        let cx_min = dirty_min[0] / cs;
        let cy_min = dirty_min[1] / cs;
        let cz_min = dirty_min[2] / cs;
        let cx_max = (dirty_max[0] / cs).min(cpa - 1);
        let cy_max = (dirty_max[1] / cs).min(cpa - 1);
        let cz_max = (dirty_max[2] / cs).min(cpa - 1);

        let mut coords = Vec::new();
        for cz in cz_min..=cz_max {
            for cy in cy_min..=cy_max {
                for cx in cx_min..=cx_max {
                    coords.push((cx, cy, cz));
                }
            }
        }
        coords
    }

    /// Extract a single chunk from a flat grid array (convenience wrapper).
    pub fn extract_chunk(
        grid_data: &[f32],
        grid_resolution: u32,
        chunk_coord: (u32, u32, u32),
        chunk_size: u32,
        bounds_min: Vec3,
        bounds_max: Vec3,
        sharp_features: bool,
        smooth_normals: bool,
    ) -> ChunkMesh {
        let accessor = FlatGridAccessor::new(grid_data, grid_resolution);
        Self::extract_chunk_from(&accessor, grid_resolution, chunk_coord, chunk_size, bounds_min, bounds_max, sharp_features, smooth_normals)
    }

    /// Extract a single chunk from any VolumeAccessor (dense grid or sparse bricks).
    pub fn extract_chunk_from(
        accessor: &dyn VolumeAccessor,
        grid_resolution: u32,
        chunk_coord: (u32, u32, u32),
        chunk_size: u32,
        bounds_min: Vec3,
        bounds_max: Vec3,
        sharp_features: bool,
        smooth_normals: bool,
    ) -> ChunkMesh {
        let res = grid_resolution as usize;
        let nverts = res + 1;
        let cs = chunk_size as usize;

        let cell_size = (bounds_max - bounds_min) / res as f32;
        let max_cell = cell_size.max_element();
        let bmin = bounds_min;

        // Chunk cell range (in global cell coords)
        let (cx, cy, cz) = chunk_coord;
        let x0 = (cx as usize) * cs;
        let y0 = (cy as usize) * cs;
        let z0 = (cz as usize) * cs;
        let x1 = (x0 + cs).min(res);
        let y1 = (y0 + cs).min(res);
        let z1 = (z0 + cs).min(res);

        // Expand cell range by 1 on -x/-y/-z sides ("ghost" padding) so that
        // quads on chunk boundaries can reference dual vertices from adjacent cells.
        // Ghost vertices are included in the ChunkMesh — slight duplication across
        // chunks is acceptable for crack-free boundary stitching.
        let px0 = if x0 > 0 { x0 - 1 } else { x0 };
        let py0 = if y0 > 0 { y0 - 1 } else { y0 };
        let pz0 = if z0 > 0 { z0 - 1 } else { z0 };

        let pad_cells_x = x1 - px0;
        let pad_cells_y = y1 - py0;
        let pad_cells_z = z1 - pz0;

        if pad_cells_x == 0 || pad_cells_y == 0 || pad_cells_z == 0 {
            return ChunkMesh { vertices: Vec::new(), normals: Vec::new(), quads: Vec::new() };
        }

        // --- helper closures ---

        let sample = |gx: usize, gy: usize, gz: usize| -> f32 {
            accessor.sample(gx, gy, gz)
        };

        let grid_normal = |gx: usize, gy: usize, gz: usize| -> Vec3 {
            let xm = if gx > 0 { gx - 1 } else { gx };
            let xp = if gx + 1 < nverts { gx + 1 } else { gx };
            let ym = if gy > 0 { gy - 1 } else { gy };
            let yp = if gy + 1 < nverts { gy + 1 } else { gy };
            let zm = if gz > 0 { gz - 1 } else { gz };
            let zp = if gz + 1 < nverts { gz + 1 } else { gz };
            let dx = sample(xp, gy, gz) - sample(xm, gy, gz);
            let dy = sample(gx, yp, gz) - sample(gx, ym, gz);
            let dz = sample(gx, gy, zp) - sample(gx, gy, zm);
            Vec3::new(dx, dy, dz).normalize_or_zero()
        };

        let interp_normal = |fx: f32, fy: f32, fz: f32| -> Vec3 {
            if smooth_normals {
                sdf_gradient_normal(accessor, nverts, fx, fy, fz)
            } else {
                let ix = (fx.round() as usize).min(nverts - 1);
                let iy = (fy.round() as usize).min(nverts - 1);
                let iz = (fz.round() as usize).min(nverts - 1);
                grid_normal(ix, iy, iz)
            }
        };

        // --- Step 1: compute dual vertex for each cell in the padded range ---

        // Local cell-vertex map: indexed by padded local (plx, ply, plz).
        // plx = gx - px0, etc. Includes ghost cells for boundary quad stitching.
        let padded_cells = pad_cells_x * pad_cells_y * pad_cells_z;
        let mut cell_vertex_idx: Vec<Option<u32>> = vec![None; padded_cells];
        let mut vertices: Vec<Vec3> = Vec::new();
        let mut normals: Vec<Vec3> = Vec::new();

        for plz in 0..pad_cells_z {
            for ply in 0..pad_cells_y {
                for plx in 0..pad_cells_x {
                    // Global cell coords
                    let gx = px0 + plx;
                    let gy = py0 + ply;
                    let gz = pz0 + plz;

                    // Sample 8 corners
                    let mut corners = [0.0f32; 8];
                    for (i, &(dx, dy, dz)) in CORNER.iter().enumerate() {
                        corners[i] = sample(gx + dx, gy + dy, gz + dz);
                    }

                    // Find edges with sign changes
                    let mut crossings: Vec<(Vec3, Vec3)> = Vec::new();
                    for &(a, b) in &EDGES {
                        if (corners[a] > 0.0) != (corners[b] > 0.0) {
                            let t = corners[a] / (corners[a] - corners[b]);
                            let (ax, ay, az) = CORNER[a];
                            let (bx, by, bz) = CORNER[b];
                            let pa = Vec3::new(
                                bmin.x + (gx + ax) as f32 * cell_size.x,
                                bmin.y + (gy + ay) as f32 * cell_size.y,
                                bmin.z + (gz + az) as f32 * cell_size.z,
                            );
                            let pb = Vec3::new(
                                bmin.x + (gx + bx) as f32 * cell_size.x,
                                bmin.y + (gy + by) as f32 * cell_size.y,
                                bmin.z + (gz + bz) as f32 * cell_size.z,
                            );
                            let crossing = pa + (pb - pa) * t;
                            let fx = (gx + ax) as f32 + t * (bx as f32 - ax as f32);
                            let fy = (gy + ay) as f32 + t * (by as f32 - ay as f32);
                            let fz = (gz + az) as f32 + t * (bz as f32 - az as f32);
                            crossings.push((crossing, interp_normal(fx, fy, fz)));
                        }
                    }

                    if crossings.is_empty() { continue; }

                    let cell_center = Vec3::new(
                        bmin.x + (gx as f32 + 0.5) * cell_size.x,
                        bmin.y + (gy as f32 + 0.5) * cell_size.y,
                        bmin.z + (gz as f32 + 0.5) * cell_size.z,
                    );

                    let vertex = if sharp_features && crossings.len() >= 3 {
                        solve_qef(&crossings, cell_center, max_cell)
                    } else {
                        let sum: Vec3 = crossings.iter().map(|(p, _)| *p).fold(Vec3::ZERO, |a, b| a + b);
                        sum / crossings.len() as f32
                    };

                    // Clamp vertex to cell bounds
                    let cell_min = Vec3::new(
                        bmin.x + gx as f32 * cell_size.x,
                        bmin.y + gy as f32 * cell_size.y,
                        bmin.z + gz as f32 * cell_size.z,
                    );
                    let vertex = vertex.clamp(cell_min, cell_min + cell_size);

                    // Vertex normal = AVERAGE of the edge-crossing plane normals (the same unit normals
                    // collected for the QEF). Robust at a concave subtract crease: the placed vertex sits
                    // on the V-fold of max(base,-cutter), where a gradient re-sampled at the vertex
                    // straddles both field branches and collapses to ~0 (the source of the jagged crater
                    // rim). The crossing-normal average is the outward bisector of the meeting faces and
                    // never collapses. Matches the GPU mesher (sdf_mesh.wgsl). Falls back to the vertex
                    // gradient only if the crossings cancel; `repair_degenerate_normals` stays as a final net.
                    let nsum: Vec3 = crossings.iter().map(|(_, n)| *n).fold(Vec3::ZERO, |a, b| a + b);
                    let vnormal = if nsum.length_squared() > 1e-12 {
                        nsum.normalize()
                    } else {
                        let fx = (vertex.x - bmin.x) / cell_size.x;
                        let fy = (vertex.y - bmin.y) / cell_size.y;
                        let fz = (vertex.z - bmin.z) / cell_size.z;
                        interp_normal(fx, fy, fz)
                    };

                    let idx = vertices.len() as u32;
                    vertices.push(vertex);
                    normals.push(vnormal);
                    cell_vertex_idx[plx + ply * pad_cells_x + plz * pad_cells_x * pad_cells_y] = Some(idx);
                }
            }
        }

        // --- Step 2: generate quads ---
        // For each grid edge with a sign change, connect 4 adjacent cells.
        // Uses the padded cell map so boundary quads have access to ghost vertices.
        // Each chunk "owns" quads for edges within its original [x0,x1) range.

        let mut quads: Vec<[u32; 4]> = Vec::new();

        let padded_cell_idx = |plx: usize, ply: usize, plz: usize| -> Option<u32> {
            if plx < pad_cells_x && ply < pad_cells_y && plz < pad_cells_z {
                cell_vertex_idx[plx + ply * pad_cells_x + plz * pad_cells_x * pad_cells_y]
            } else {
                None
            }
        };

        // Only iterate over cells in the ORIGINAL chunk range [x0,x1) for edge ownership.
        // Padded local offset: ox = x0 - px0, etc.
        let ox = x0 - px0;
        let oy = y0 - py0;
        let oz = z0 - pz0;
        let chunk_cells_x = x1 - x0;
        let chunk_cells_y = y1 - y0;
        let chunk_cells_z = z1 - z0;

        // X-axis edges: grid vertex (gx,gy,gz) to (gx+1,gy,gz)
        // Adjacent cells: (gx, gy-1, gz-1), (gx, gy, gz-1), (gx, gy, gz), (gx, gy-1, gz)
        for lz in 0..chunk_cells_z {
            for ly in 0..chunk_cells_y {
                for lx in 0..chunk_cells_x {
                    let gx = x0 + lx;
                    let gy = y0 + ly;
                    let gz = z0 + lz;

                    let d0 = sample(gx, gy, gz);
                    let d1 = sample(gx + 1, gy, gz);
                    if (d0 > 0.0) == (d1 > 0.0) { continue; }
                    if gy < 1 || gz < 1 { continue; }

                    // Padded local coords for the 4 cells
                    let plx = ox + lx;
                    let ply = oy + ly;
                    let plz = oz + lz;
                    let v0 = padded_cell_idx(plx, ply - 1, plz - 1);
                    let v1 = padded_cell_idx(plx, ply,     plz - 1);
                    let v2 = padded_cell_idx(plx, ply,     plz);
                    let v3 = padded_cell_idx(plx, ply - 1, plz);

                    if let (Some(a), Some(b), Some(c), Some(d)) = (v0, v1, v2, v3) {
                        if d0 > 0.0 { quads.push([a, b, c, d]); }
                        else         { quads.push([d, c, b, a]); }
                    }
                }
            }
        }

        // Y-axis edges: grid vertex (gx,gy,gz) to (gx,gy+1,gz)
        // Adjacent cells: (gx-1, gy, gz-1), (gx, gy, gz-1), (gx, gy, gz), (gx-1, gy, gz)
        for lz in 0..chunk_cells_z {
            for ly in 0..chunk_cells_y {
                for lx in 0..chunk_cells_x {
                    let gx = x0 + lx;
                    let gy = y0 + ly;
                    let gz = z0 + lz;

                    let d0 = sample(gx, gy, gz);
                    let d1 = sample(gx, gy + 1, gz);
                    if (d0 > 0.0) == (d1 > 0.0) { continue; }
                    if gx < 1 || gz < 1 { continue; }

                    let plx = ox + lx;
                    let ply = oy + ly;
                    let plz = oz + lz;
                    let v0 = padded_cell_idx(plx - 1, ply, plz - 1);
                    let v1 = padded_cell_idx(plx,     ply, plz - 1);
                    let v2 = padded_cell_idx(plx,     ply, plz);
                    let v3 = padded_cell_idx(plx - 1, ply, plz);

                    if let (Some(a), Some(b), Some(c), Some(d)) = (v0, v1, v2, v3) {
                        if d0 > 0.0 { quads.push([a, b, c, d]); }
                        else         { quads.push([d, c, b, a]); }
                    }
                }
            }
        }

        // Z-axis edges: grid vertex (gx,gy,gz) to (gx,gy,gz+1)
        // Adjacent cells: (gx-1, gy-1, gz), (gx, gy-1, gz), (gx, gy, gz), (gx-1, gy, gz)
        for lz in 0..chunk_cells_z {
            for ly in 0..chunk_cells_y {
                for lx in 0..chunk_cells_x {
                    let gx = x0 + lx;
                    let gy = y0 + ly;
                    let gz = z0 + lz;

                    let d0 = sample(gx, gy, gz);
                    let d1 = sample(gx, gy, gz + 1);
                    if (d0 > 0.0) == (d1 > 0.0) { continue; }
                    if gx < 1 || gy < 1 { continue; }

                    let plx = ox + lx;
                    let ply = oy + ly;
                    let plz = oz + lz;
                    let v0 = padded_cell_idx(plx - 1, ply - 1, plz);
                    let v1 = padded_cell_idx(plx,     ply - 1, plz);
                    let v2 = padded_cell_idx(plx,     ply,     plz);
                    let v3 = padded_cell_idx(plx - 1, ply,     plz);

                    if let (Some(a), Some(b), Some(c), Some(d)) = (v0, v1, v2, v3) {
                        if d0 > 0.0 { quads.push([a, b, c, d]); }
                        else         { quads.push([d, c, b, a]); }
                    }
                }
            }
        }

        // --- Step 3: repair degenerate vertex normals with gradient-free geometric normals ---
        repair_degenerate_normals(&vertices, &mut normals, &quads);

        ChunkMesh { vertices, normals, quads }
    }

    /// Update only the chunks overlapping the dirty region (flat grid convenience wrapper).
    pub fn update_dirty_chunks(
        &mut self,
        grid_data: &[f32],
        dirty_min: [u32; 3],
        dirty_max: [u32; 3],
    ) {
        let accessor = FlatGridAccessor::new(grid_data, self.grid_resolution);
        self.update_dirty_chunks_from(&accessor, dirty_min, dirty_max);
    }

    /// Update only the chunks overlapping the dirty region using any VolumeAccessor.
    pub fn update_dirty_chunks_from(
        &mut self,
        accessor: &dyn VolumeAccessor,
        dirty_min: [u32; 3],
        dirty_max: [u32; 3],
    ) {
        let coords = self.dirty_chunk_coords(dirty_min, dirty_max);
        for coord in coords {
            let chunk = Self::extract_chunk_from(
                accessor,
                self.grid_resolution,
                coord,
                self.chunk_size,
                self.bounds_min,
                self.bounds_max,
                self.sharp_features,
                self.smooth_normals,
            );
            self.chunks.insert(coord, chunk);
        }
    }

    /// Re-extract ALL chunks from the grid (flat grid convenience wrapper).
    pub fn extract_all(&mut self, grid_data: &[f32]) {
        let accessor = FlatGridAccessor::new(grid_data, self.grid_resolution);
        self.extract_all_from(&accessor);
    }

    /// Re-extract ALL chunks using any VolumeAccessor (full rebuild).
    pub fn extract_all_from(&mut self, accessor: &dyn VolumeAccessor) {
        self.chunks.clear();
        let cpa = self.chunks_per_axis;
        for cz in 0..cpa {
            for cy in 0..cpa {
                for cx in 0..cpa {
                    let coord = (cx, cy, cz);
                    let chunk = Self::extract_chunk_from(
                        accessor,
                        self.grid_resolution,
                        coord,
                        self.chunk_size,
                        self.bounds_min,
                        self.bounds_max,
                        self.sharp_features,
                        self.smooth_normals,
                    );
                    if !chunk.vertices.is_empty() {
                        self.chunks.insert(coord, chunk);
                    }
                }
            }
        }
    }

    /// Number of chunks currently stored.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Total vertex count across all chunks.
    pub fn total_vertices(&self) -> usize {
        self.chunks.values().map(|c| c.vertices.len()).sum()
    }

    /// Total quad count across all chunks.
    pub fn total_quads(&self) -> usize {
        self.chunks.values().map(|c| c.quads.len()).sum()
    }

    /// Translate all cached vertex positions by `offset`.
    /// Used when body origin shifts so the stale mesh stays at the correct
    /// world position until the next re-extraction.
    pub fn translate_all_vertices(&mut self, offset: Vec3) {
        for chunk in self.chunks.values_mut() {
            for v in &mut chunk.vertices {
                *v += offset;
            }
        }
        self.bounds_min += offset;
        self.bounds_max += offset;
    }

    /// Clear a specific chunk (e.g., when a region becomes empty).
    pub fn clear_chunk(&mut self, coord: (u32, u32, u32)) {
        self.chunks.remove(&coord);
    }

    /// Clear all chunks.
    pub fn clear(&mut self) {
        self.chunks.clear();
    }
}
