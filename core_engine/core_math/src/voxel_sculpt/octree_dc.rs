//! Gate 2 octree Dual Contouring (REMESH_ADAPTIVE_OCTREE_PLAN §7).
//!
//! Step 1 (`Qef`) already lives in `mesh.rs`. This file is steps 2–4 at **one**
//! collapse level (2³ blocks), using density min/max — not `BrickClassifier`
//! (plan G2-A: remesh has no SDF primitives).
//!
//! `collapse_eps == 0` is a thin call to [`VoxelGrid::surface_nets_placed`] so
//! remesh output stays bit-identical. `collapse_eps > 0` classifies each 2³
//! block first and QEFs **only** the parent (or leftover fine cells). Collapse
//! rides the remesh S2 extract (parallel even-z slabs, surface-chunk skip).

use glam::Vec3;
use crate::mesh::{Mesh, Qef};
use super::*;

const CORNER: [[u32; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [0, 1, 0],
    [1, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [0, 1, 1],
    [1, 1, 1],
];
const EDGE: [[usize; 2]; 12] = [
    [0, 1],
    [0, 2],
    [0, 4],
    [1, 3],
    [1, 5],
    [2, 3],
    [2, 6],
    [3, 7],
    [4, 5],
    [4, 6],
    [5, 7],
    [6, 7],
];

#[derive(Clone, Copy)]
struct Leaf {
    qef: Qef,
    pos: Vec3,
    nrm: Vec3,
}

#[derive(Clone, Copy)]
struct CachedParent {
    bx: u32,
    by: u32,
    bz: u32,
    pos: Vec3,
    nrm: Vec3,
    kid_mask: u8,
}

#[derive(Clone, Copy)]
struct CachedLeaf {
    cxy: u32,
    pos: Vec3,
    nrm: Vec3,
}

/// Retained parent-first 2³ collapse extract. Runtime-only; never serialized.
///
/// Cache unit is an even-z parent slab plus leftover fine cells per cz — the same
/// emission order as a full collapse extract, so a reused slab cannot remumber
/// clean verts. Quad emission still walks the whole surface.
#[derive(Default)]
pub struct OctreeCollapseCache {
    res: u32,
    collapse_eps_bits: u32,
    parent_slabs: Vec<Vec<CachedParent>>,
    leftover_slabs: Vec<Vec<CachedLeaf>>,
    pub primed: bool,
    pub recomputed_parent_slabs: usize,
    pub recomputed_leftover_slabs: usize,
    pub parent_recompute_lo: usize,
    pub parent_recompute_hi: usize,
    pub leftover_recompute_lo: usize,
    pub leftover_recompute_hi: usize,
    /// Last projected positions, same order as parent/leftover emission.
    pub proj_parent: Vec<Vec<Vec3>>,
    pub proj_leftover: Vec<Vec<Vec3>>,
    /// How many extract verts the last slab project actually snapped.
    pub last_project_dirty_verts: usize,
}

impl OctreeCollapseCache {
    pub fn parent_slab_count(&self) -> usize {
        self.parent_slabs.len()
    }

    pub fn leftover_slab_count(&self) -> usize {
        self.leftover_slabs.len()
    }

    pub fn parent_vert_counts(&self) -> Vec<usize> {
        self.parent_slabs.iter().map(|s| s.len()).collect()
    }

    pub fn leftover_vert_counts(&self) -> Vec<usize> {
        self.leftover_slabs.iter().map(|s| s.len()).collect()
    }
}

/// Which even-z parent slabs and leftover cz planes a dirty point box touches.
fn collapse_dirty_ranges(
    dirty: DirtyCells,
    cells: usize,
    even_hi: usize,
    cs: usize,
    reusable: bool,
    n_parent_z: usize,
) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    if !reusable || matches!(dirty, DirtyCells::All) {
        return (0..n_parent_z, 0..cells);
    }
    match dirty {
        DirtyCells::None => (0..0, 0..0),
        DirtyCells::All => (0..n_parent_z, 0..cells),
        DirtyCells::Points { lo, hi } => {
            // Same cell dilation as uniform Phase A (QEF gradient ±1 voxel + f32
            // slack), then ±1 surface-chunk because the live mask reads neighbours.
            let c_lo = (lo[2] as usize).saturating_sub(4);
            let c_hi = ((hi[2] as usize) + 2).min(cells.saturating_sub(1));
            let b0 = (c_lo / cs).saturating_sub(1) * cs;
            let b1 = (((c_hi / cs) + 2) * cs).min(cells);
            // Parent 3×3×3 reads bz..bz+2. One extra even parent each side.
            let p_bz0 = b0.saturating_sub(2) & !1;
            let p_bz1 = ((b1 + 3) & !1).min(even_hi);
            let p_lo = p_bz0 / 2;
            let p_hi = (p_bz1 / 2).min(n_parent_z);
            let l_lo = b0.min(p_bz0);
            let l_hi = b1.max(p_bz1.saturating_add(1)).min(cells);
            (p_lo..p_hi, l_lo..l_hi)
        }
    }
}

impl VoxelGrid {
    /// Dual-contour extract with optional 2³ QEF collapse.
    ///
    /// `collapse_eps == 0` → [`Self::surface_nets_placed`] (`CellPlacement::Qef`), bit-identical.
    /// `collapse_eps > 0` → collapse a 2³ block when `Qef::residual(v) / n < collapse_eps`.
    pub fn surface_nets_octree(&self, smooth: bool, collapse_eps: f32) -> Mesh {
        if collapse_eps <= 0.0 {
            return self.surface_nets_placed(smooth, CellPlacement::Qef);
        }
        self.surface_nets_octree_collapsed(smooth, collapse_eps, None)
    }

    /// [`Self::surface_nets_octree`] with a retained parent/leftover cache.
    ///
    /// Reused slabs are bit-identical to a full collapse extract when `dirty`
    /// covers every cell the last write could have changed (gated by
    /// `cached_collapse_extract_matches_full`).
    pub fn surface_nets_octree_cached(
        &self,
        smooth: bool,
        collapse_eps: f32,
        cache: &mut OctreeCollapseCache,
        dirty: DirtyCells,
    ) -> Mesh {
        if collapse_eps <= 0.0 {
            return self.surface_nets_placed(smooth, CellPlacement::Qef);
        }
        self.surface_nets_octree_collapsed(smooth, collapse_eps, Some((cache, dirty)))
    }

    fn cell_hermite_qef(&self, cx: u32, cy: u32, cz: u32) -> Option<Leaf> {
        let mut d = [0.0f32; 8];
        for (k, c) in CORNER.iter().enumerate() {
            d[k] = self.density.get_idx(self.idx(cx + c[0], cy + c[1], cz + c[2]));
        }
        let vs = self.voxel_size();
        match self.hermite_from_corners(self.cell_world_pos(cx, cy, cz), vs, &d) {
            Some(leaf) => Some(leaf),
            None => {
                let (pos, nrm) = self.cell_surface_vertex_d(cx, cy, cz, &d)?;
                Some(Leaf {
                    qef: Qef::default(),
                    pos,
                    nrm,
                })
            }
        }
    }

    fn hermite_from_corners(&self, origin: Vec3, cell: f32, d: &[f32; 8]) -> Option<Leaf> {
        let iso = self.iso;
        let mut inside = 0u32;
        for &v in d {
            if v >= iso {
                inside += 1;
            }
        }
        if inside == 0 || inside == 8 {
            return None;
        }
        let vs = self.voxel_size();
        let mut qef = Qef::default();
        let mut nsum = Vec3::ZERO;
        for e in EDGE.iter() {
            let (a, b) = (e[0], e[1]);
            let (da, db) = (d[a], d[b]);
            if (da >= iso) == (db >= iso) {
                continue;
            }
            let tt = (iso - da) / (db - da);
            let ca = CORNER[a];
            let cb = CORNER[b];
            let pa = Vec3::new(ca[0] as f32, ca[1] as f32, ca[2] as f32);
            let pb = Vec3::new(cb[0] as f32, cb[1] as f32, cb[2] as f32);
            let world = origin + pa.lerp(pb, tt) * cell;
            let n = self.gradient_normal(world, vs);
            if n.length_squared() > 1e-12 {
                qef.add_plane(world, n);
                nsum += n;
            }
        }
        if qef.n == 0 {
            return None;
        }
        let center = origin + Vec3::splat(0.5 * cell);
        let pos = if qef.n >= 3 {
            qef.solve(center, cell).clamp(origin, origin + Vec3::splat(cell))
        } else {
            qef.mass / qef.n as f32
        };
        let nrm = if nsum.length_squared() > 1e-12 {
            nsum.normalize()
        } else {
            self.gradient_normal(pos, vs)
        };
        Some(Leaf { qef, pos, nrm })
    }

    /// One parent QEF from the 2³ coarse cube. `kid_mask` bit `dx + 2·dy + 4·dz` is a
    /// fine child with a sign change. `None` if the block should stay at leaf resolution.
    fn try_parent_collapse(&self, bx: u32, by: u32, bz: u32, collapse_eps: f32) -> Option<(Leaf, u8)> {
        let n = self.res;
        if bx + 2 >= n || by + 2 >= n || bz + 2 >= n {
            return None;
        }
        let iso = self.iso;
        let mut d3 = [0.0f32; 27];
        for z in 0..3u32 {
            for y in 0..3u32 {
                for x in 0..3u32 {
                    d3[(x + 3 * y + 9 * z) as usize] =
                        self.density.get_idx(self.idx(bx + x, by + y, bz + z));
                }
            }
        }
        let mut kid_mask = 0u8;
        let mut kids = 0u32;
        for dz in 0..2u32 {
            for dy in 0..2u32 {
                for dx in 0..2u32 {
                    let mut inside = 0u32;
                    for c in &CORNER {
                        let i = (dx + c[0]) + 3 * (dy + c[1]) + 9 * (dz + c[2]);
                        if d3[i as usize] >= iso {
                            inside += 1;
                        }
                    }
                    if inside > 0 && inside < 8 {
                        kid_mask |= 1 << (dx + 2 * dy + 4 * dz);
                        kids += 1;
                    }
                }
            }
        }
        if kids < 2 {
            return None;
        }
        let mut d8 = [0.0f32; 8];
        for (k, c) in CORNER.iter().enumerate() {
            d8[k] = d3[(c[0] * 2 + 3 * c[1] * 2 + 9 * c[2] * 2) as usize];
        }
        let vs = self.voxel_size();
        let vs2 = vs * 2.0;
        let origin = self.cell_world_pos(bx, by, bz);
        // Two perpendicular planes have residual ≈ 0, so residual alone collapses
        // box rims into one vertex (the spikes on the volume). Reject when coarse
        // crossing normals disagree by more than ~35°.
        let mut nrms = [Vec3::ZERO; 12];
        let mut n_count = 0usize;
        let mut min_dot = 1.0f32;
        for e in EDGE.iter() {
            let (a, b) = (e[0], e[1]);
            let (da, db) = (d8[a], d8[b]);
            if (da >= iso) == (db >= iso) {
                continue;
            }
            let tt = (iso - da) / (db - da);
            let ca = CORNER[a];
            let cb = CORNER[b];
            let pa = Vec3::new(ca[0] as f32, ca[1] as f32, ca[2] as f32);
            let pb = Vec3::new(cb[0] as f32, cb[1] as f32, cb[2] as f32);
            let world = origin + pa.lerp(pb, tt) * vs2;
            let n = self.gradient_normal(world, vs);
            if n.length_squared() <= 1e-12 {
                continue;
            }
            let n = n.normalize();
            for i in 0..n_count {
                min_dot = min_dot.min(n.dot(nrms[i]));
            }
            nrms[n_count] = n;
            n_count += 1;
        }
        if n_count < 3 || min_dot < 0.82 {
            return None;
        }
        let leaf = self.hermite_from_corners(origin, vs2, &d8)?;
        if leaf.qef.n < 3 {
            return None;
        }
        let err = leaf.qef.residual(leaf.pos) / leaf.qef.n as f32;
        if err >= collapse_eps {
            return None;
        }
        Some((leaf, kid_mask))
    }

    /// Classify each 2³ first: one coarse QEF per parent that passes residual,
    /// fine QEF only on leftover surface cells. Same S2 scaffolding as remesh
    /// (surface-chunk skip, chunk-sparse `cell_vert`, parallel quad planes).
    fn surface_nets_octree_collapsed(
        &self,
        smooth: bool,
        collapse_eps: f32,
        mut cached: Option<(&mut OctreeCollapseCache, DirtyCells)>,
    ) -> Mesh {
        use rayon::prelude::*;
        let n = self.res;
        let cells = n.saturating_sub(1) as usize;
        if cells < 2 {
            return self.surface_nets_placed(smooth, CellPlacement::Qef);
        }
        let iso = self.iso;

        const SN_CHUNK: u32 = 16;
        let cd = (((cells as u32 + SN_CHUNK - 1) / SN_CHUNK).max(1)) as usize;
        let mask = self.surface_chunk_mask(SN_CHUNK);
        let cs = SN_CHUNK as usize;
        let live = |cx: usize, cy: usize, cz: usize| -> bool {
            let ci = (cx / cs) + (cy / cs) * cd + (cz / cs) * cd * cd;
            mask.get(ci).copied().unwrap_or(true)
        };

        let chunk_count = cd * cd * cd;
        let blk = cs * cs * cs;
        let mut chunk_slot = vec![u32::MAX; chunk_count];
        let mut live_chunks = 0usize;
        for (ci, slot) in chunk_slot.iter_mut().enumerate() {
            if mask.get(ci).copied().unwrap_or(true) {
                *slot = live_chunks as u32;
                live_chunks += 1;
            }
        }
        let cv_slot = |cx: usize, cy: usize, cz: usize| -> Option<usize> {
            let ci = (cx / cs) + (cy / cs) * cd + (cz / cs) * cd * cd;
            let slot = *chunk_slot.get(ci)?;
            if slot == u32::MAX {
                return None;
            }
            Some(slot as usize * blk + (cx % cs) + (cy % cs) * cs + (cz % cs) * cs * cs)
        };

        // 16³ chunks are even-aligned, so a 2³ parent never straddles two chunks.
        let even_hi = cells.saturating_sub(1);
        let n_parent_z = if even_hi < 2 {
            0
        } else {
            (even_hi - 1) / 2 + 1
        };

        let mut parent_slabs = vec![Vec::new(); n_parent_z];
        let mut leftover_slabs = vec![Vec::new(); cells];
        let mut parent_recompute = 0..n_parent_z;
        let mut leftover_recompute = 0..cells;
        if let Some((c, dirty)) = cached.as_mut() {
            let reusable = c.primed
                && c.res == n
                && c.collapse_eps_bits == collapse_eps.to_bits()
                && c.parent_slabs.len() == n_parent_z
                && c.leftover_slabs.len() == cells;
            let (pr, lr) = collapse_dirty_ranges(
                *dirty, cells, even_hi, cs, reusable, n_parent_z,
            );
            parent_recompute = pr;
            leftover_recompute = lr;
            if reusable {
                parent_slabs = std::mem::take(&mut c.parent_slabs);
                leftover_slabs = std::mem::take(&mut c.leftover_slabs);
            }
        }

        let compute_parent_slab = |bz: usize| -> Vec<CachedParent> {
            let mut hits = Vec::new();
            for by in (0..even_hi).step_by(2) {
                let mut bx = 0usize;
                while bx < even_hi {
                    let block_end = ((bx / cs) + 1) * cs;
                    if !live(bx, by, bz) {
                        bx = block_end;
                        continue;
                    }
                    let end = block_end.min(even_hi);
                    let mut x = bx;
                    while x < end {
                        if let Some((leaf, mask)) =
                            self.try_parent_collapse(x as u32, by as u32, bz as u32, collapse_eps)
                        {
                            hits.push(CachedParent {
                                bx: x as u32,
                                by: by as u32,
                                bz: bz as u32,
                                pos: leaf.pos,
                                nrm: leaf.nrm,
                                kid_mask: mask,
                            });
                        }
                        x += 2;
                    }
                    bx = end;
                }
            }
            hits
        };

        if !parent_recompute.is_empty() {
            let fresh: Vec<(usize, Vec<CachedParent>)> = parent_recompute
                .clone()
                .into_par_iter()
                .map(|pi| (pi, compute_parent_slab(pi * 2)))
                .collect();
            for (pi, v) in fresh {
                parent_slabs[pi] = v;
            }
        }

        let n_slot = live_chunks * blk;
        let mut cell_vert = vec![u32::MAX; n_slot];
        let mut vertices: Vec<Vec3> = Vec::new();
        let mut normals: Vec<Vec3> = Vec::new();
        for hits in &parent_slabs {
            for p in hits {
                let vi = vertices.len() as u32;
                vertices.push(p.pos);
                normals.push(p.nrm);
                for dz in 0..2usize {
                    for dy in 0..2usize {
                        for dx in 0..2usize {
                            let bit = dx + 2 * dy + 4 * dz;
                            if p.kid_mask & (1 << bit) == 0 {
                                continue;
                            }
                            if let Some(i) =
                                cv_slot(p.bx as usize + dx, p.by as usize + dy, p.bz as usize + dz)
                            {
                                cell_vert[i] = vi;
                            }
                        }
                    }
                }
            }
        }

        {
            let cell_vert_ref = &cell_vert;
            if !leftover_recompute.is_empty() {
                let fresh: Vec<(usize, Vec<CachedLeaf>)> = leftover_recompute
                    .clone()
                    .into_par_iter()
                    .map(|cz| {
                        let mut out: Vec<CachedLeaf> = Vec::new();
                        for cy in 0..cells {
                            let mut cx = 0usize;
                            while cx < cells {
                                let block_end = ((cx / cs) + 1) * cs;
                                if !live(cx, cy, cz) {
                                    cx = block_end;
                                    continue;
                                }
                                let end = block_end.min(cells);
                                for cx in cx..end {
                                    match cv_slot(cx, cy, cz) {
                                        Some(i) if cell_vert_ref[i] == u32::MAX => {}
                                        _ => continue,
                                    }
                                    if let Some(leaf) =
                                        self.cell_hermite_qef(cx as u32, cy as u32, cz as u32)
                                    {
                                        out.push(CachedLeaf {
                                            cxy: (cx + cy * cells) as u32,
                                            pos: leaf.pos,
                                            nrm: leaf.nrm,
                                        });
                                    }
                                }
                                cx = end;
                            }
                        }
                        (cz, out)
                    })
                    .collect();
                for (cz, v) in fresh {
                    leftover_slabs[cz] = v;
                }
            }
        }
        for (cz, slab) in leftover_slabs.iter().enumerate() {
            for leaf in slab {
                let cx = leaf.cxy as usize % cells;
                let cy = leaf.cxy as usize / cells;
                if let Some(i) = cv_slot(cx, cy, cz) {
                    cell_vert[i] = vertices.len() as u32;
                }
                vertices.push(leaf.pos);
                normals.push(leaf.nrm);
            }
        }

        let cells_i = cells as i64;
        let cell_vert_ref = &cell_vert;
        let plane_quads: Vec<Vec<[u32; 4]>> = (0..n)
            .into_par_iter()
            .map(|z| {
                let mut quads: Vec<[u32; 4]> = Vec::new();
                let solid = |x: u32, y: u32, z: u32| self.density.get_idx(self.idx(x, y, z)) >= iso;
                let cv = |cx: i64, cy: i64, cz: i64| -> u32 {
                    if cx < 0 || cy < 0 || cz < 0 || cx >= cells_i || cy >= cells_i || cz >= cells_i {
                        return u32::MAX;
                    }
                    match cv_slot(cx as usize, cy as usize, cz as usize) {
                        Some(i) => cell_vert_ref[i],
                        None => u32::MAX,
                    }
                };
                if z as usize >= cells {
                    return quads;
                }
                for y in 0..n {
                    if y as usize >= cells {
                        continue;
                    }
                    let mut xs = 0usize;
                    while xs < cells {
                        let block_end = ((xs / cs) + 1) * cs;
                        if !live(xs, y as usize, z as usize) {
                            xs = block_end;
                            continue;
                        }
                        let end = block_end.min(cells);
                        for x in (xs as u32)..(end as u32) {
                            let (xi, yi, zi) = (x as i64, y as i64, z as i64);
                            if x + 1 < n {
                                let s0 = solid(x, y, z);
                                let s1 = solid(x + 1, y, z);
                                if s0 != s1 {
                                    let c = [
                                        cv(xi, yi - 1, zi - 1),
                                        cv(xi, yi, zi - 1),
                                        cv(xi, yi, zi),
                                        cv(xi, yi - 1, zi),
                                    ];
                                    push_dc_face(&mut quads, c, !s0);
                                }
                            }
                            if y + 1 < n {
                                let s0 = solid(x, y, z);
                                let s1 = solid(x, y + 1, z);
                                if s0 != s1 {
                                    let c = [
                                        cv(xi - 1, yi, zi - 1),
                                        cv(xi - 1, yi, zi),
                                        cv(xi, yi, zi),
                                        cv(xi, yi, zi - 1),
                                    ];
                                    push_dc_face(&mut quads, c, !s0);
                                }
                            }
                            if z + 1 < n {
                                let s0 = solid(x, y, z);
                                let s1 = solid(x, y, z + 1);
                                if s0 != s1 {
                                    let c = [
                                        cv(xi - 1, yi - 1, zi),
                                        cv(xi, yi - 1, zi),
                                        cv(xi, yi, zi),
                                        cv(xi - 1, yi, zi),
                                    ];
                                    push_dc_face(&mut quads, c, !s0);
                                }
                            }
                        }
                        xs = end;
                    }
                }
                quads
            })
            .collect();
        let mut quads: Vec<[u32; 4]> = Vec::with_capacity(plane_quads.iter().map(|p| p.len()).sum());
        for p in plane_quads {
            quads.extend(p);
        }

        if let Some((c, _)) = cached {
            c.res = n;
            c.collapse_eps_bits = collapse_eps.to_bits();
            c.parent_slabs = parent_slabs;
            c.leftover_slabs = leftover_slabs;
            c.primed = true;
            c.recomputed_parent_slabs = parent_recompute.len();
            c.recomputed_leftover_slabs = leftover_recompute.len();
            c.parent_recompute_lo = parent_recompute.start;
            c.parent_recompute_hi = parent_recompute.end;
            c.leftover_recompute_lo = leftover_recompute.start;
            c.leftover_recompute_hi = leftover_recompute.end;
        }

        if smooth {
            relax_mesh(self, &mut vertices, &mut normals, &quads);
        }
        Mesh {
            vertices,
            normals,
            quads,
        }
    }
}

/// Quad if 4 distinct verts; triangle `[a,b,c,a]` if a collapsed parent shares
/// two corners (T-junction at a coarse/fine seam). `from_mesh` unwraps the wrap.
fn push_dc_face(quads: &mut Vec<[u32; 4]>, c: [u32; 4], flip: bool) {
    if c.iter().any(|&v| v == u32::MAX) {
        return;
    }
    let r = if flip {
        [c[3], c[2], c[1], c[0]]
    } else {
        c
    };
    let mut tmp = [0u32; 4];
    let mut n = 0usize;
    for i in 0..4 {
        if n == 0 || tmp[n - 1] != r[i] {
            tmp[n] = r[i];
            n += 1;
        }
    }
    if n >= 2 && tmp[0] == tmp[n - 1] {
        n -= 1;
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if tmp[i] == tmp[j] {
                return;
            }
        }
    }
    match n {
        4 => quads.push([tmp[0], tmp[1], tmp[2], tmp[3]]),
        3 => quads.push([tmp[0], tmp[1], tmp[2], tmp[0]]),
        _ => {}
    }
}
