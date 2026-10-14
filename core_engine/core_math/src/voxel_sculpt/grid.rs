// voxel_sculpt/grid.rs — density storage (DensityStorage, BrickGrid), VoxelGrid core
// (records, sampling, fills, resample), picking, surface-nets meshing + VoxelMeshCache.
// Split from voxel_sculpt.rs (pure relocation).

use glam::Vec3;
use crate::mesh::Mesh;
use super::*;

/// Density storage backend for a `VoxelGrid`. This is the abstraction boundary (sculpt sparse storage):
/// every density read/write goes through these methods instead of indexing a raw `Vec`, so the dense
/// and sparse impls coexist and swap WITHOUT touching any consumer or the GPU shaders.
///
/// - `Dense` — a flat `res³` `Vec<f32>`, behaviour-identical to the original `density: Vec<f32>` field.
/// - `Sparse` — a 16³ brick pool with EMPTY/FULL/ALLOCATED tags (`BrickGrid`); 512³ solid clay costs
///   near-zero (FULL bricks are implicit) vs 536 MB dense. Point reads decode a brick; the mesh hot
///   path gathers a chunk into a dense scratch (Phase 3 later) so the inner loop stays flat-`Vec` fast.
///
/// `PartialEq` is by VALUE (`iter`), so a Dense and a Sparse grid holding the same field compare equal
/// regardless of representation — the keystone differential test relies on this via `to_dense()`.
#[derive(Debug, Clone)]
pub enum DensityStorage {
    /// Flat `res³` density in `[0,1]`, indexed `x + y·res + z·res²`.
    Dense { d: Vec<f32>, res: u32 },
    /// Sparse 16³ brick pool (see `BrickGrid`).
    Sparse(BrickGrid),
}

impl PartialEq for DensityStorage {
    fn eq(&self, other: &Self) -> bool {
        self.res() == other.res() && self.iter().eq(other.iter())
    }
}

impl DensityStorage {
    /// A dense grid of `res³` voxels all set to `val`.
    pub fn dense(res: u32, val: f32) -> Self {
        DensityStorage::Dense { d: vec![val; (res * res * res) as usize], res }
    }
    /// A sparse (brick) grid of `res³` voxels all set to `val` (`val` 0.0/1.0 → all implicit bricks).
    pub fn sparse(res: u32, val: f32) -> Self { DensityStorage::Sparse(BrickGrid::new(res, val)) }
    #[inline]
    pub fn res(&self) -> u32 {
        match self { DensityStorage::Dense { res, .. } => *res, DensityStorage::Sparse(b) => b.res }
    }
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            DensityStorage::Dense { d, .. } => d.len(),
            DensityStorage::Sparse(b) => (b.res as usize).pow(3),
        }
    }
    /// OOB-clamped point read (outside the grid is empty → 0.0). The old `VoxelGrid::at` body.
    #[inline]
    pub fn at_i(&self, x: i32, y: i32, z: i32) -> f32 {
        match self {
            DensityStorage::Dense { d, res } => {
                let r = *res as i32;
                if x < 0 || y < 0 || z < 0 || x >= r || y >= r || z >= r { return 0.0; }
                d[(x as u32 + y as u32 * res + z as u32 * res * res) as usize]
            }
            DensityStorage::Sparse(b) => b.at_oob(x, y, z),
        }
    }
    /// Read by flat index (caller computed `x + y·res + z·res²`). Used by the brush inner loop,
    /// snapshots, undo deltas, and the mesh corner reads (all in-bounds).
    #[inline]
    pub fn get_idx(&self, i: usize) -> f32 {
        match self { DensityStorage::Dense { d, .. } => d[i], DensityStorage::Sparse(b) => b.get_idx(i) }
    }
    /// Write by flat index.
    #[inline]
    pub fn set_idx(&mut self, i: usize, v: f32) {
        match self { DensityStorage::Dense { d, .. } => d[i] = v, DensityStorage::Sparse(b) => b.set_idx(i, v) }
    }
    /// Set every voxel to `val` (grid clear).
    pub fn fill(&mut self, val: f32) {
        match self {
            DensityStorage::Dense { d, .. } => d.iter_mut().for_each(|x| *x = val),
            DensityStorage::Sparse(b) => b.fill(val),
        }
    }
    /// Iterate every voxel density in flat order (tests, parity checks). Boxed because the two arms
    /// yield different concrete iterators; this is a cold path (tests, `PartialEq`, `to_dense`).
    pub fn iter(&self) -> Box<dyn Iterator<Item = f32> + '_> {
        match self {
            DensityStorage::Dense { d, .. } => Box::new(d.iter().copied()),
            DensityStorage::Sparse(b) => Box::new((0..(b.res as usize).pow(3)).map(move |i| b.get_idx(i))),
        }
    }
    /// Materialise the whole grid as one dense `Vec` (one-shot: persistent-buffer init, resample). NOT
    /// a per-frame path (Phase 3's GPU mesh patches a persistent buffer by dirty brick instead).
    pub fn to_dense(&self) -> Vec<f32> {
        match self { DensityStorage::Dense { d, .. } => d.clone(), DensityStorage::Sparse(b) => b.to_dense() }
    }
    /// Borrow the backing dense buffer for a zero-copy GPU upload. Valid only for `Dense` — the Phase-3c
    /// GPU mesh path is rewritten to a persistent buffer patched by dirty brick, so it never calls this
    /// on `Sparse` (and the runtime switch to Sparse only ships after that rewrite).
    #[inline]
    pub fn as_dense_slice(&self) -> &[f32] {
        match self {
            DensityStorage::Dense { d, .. } => d,
            DensityStorage::Sparse(_) => unreachable!("as_dense_slice on Sparse — use the persistent mesh buffer (Phase 3c)"),
        }
    }
    /// Mutable twin of [`Self::as_dense_slice`] — lets a Dense grid be written in parallel
    /// over disjoint z-planes (`write_region_from_sdf`'s fast path). Same `Dense`-only
    /// contract: callers must branch on storage, never call this on `Sparse`.
    #[inline]
    pub fn as_dense_slice_mut(&mut self) -> &mut [f32] {
        match self {
            DensityStorage::Dense { d, .. } => d,
            DensityStorage::Sparse(_) => unreachable!("as_dense_slice_mut on Sparse — use set_idx"),
        }
    }
    /// Gather the full-XY Z-slice range `[z0..=z1]` as a dense `Vec` (the GPU brush upload). For Dense
    /// this is the contiguous sub-slice the brush already copied via `.to_vec()`; for Sparse it
    /// materialises the slice from bricks (resolving EMPTY→0 / FULL→1 / ALLOC→value).
    pub fn z_slice(&self, z0: u32, z1: u32) -> Vec<f32> {
        match self {
            DensityStorage::Dense { d, res } => {
                let r = *res as usize;
                d[z0 as usize * r * r..(z1 as usize + 1) * r * r].to_vec()
            }
            DensityStorage::Sparse(b) => b.z_slice(z0, z1),
        }
    }
    /// Gather a 3D BOX `[x0..=x1, y0..=y1, z0..=z1]` into a dense `Vec` (box-local layout: index =
    /// `(x-x0) + (y-y0)·dx + (z-z0)·dx·dy`). The GPU brush working buffer (Phase 4) — bounded by the
    /// brush AABB in ALL axes, so it never scales with res² (the 1024³ enabler).
    pub fn subvolume(&self, aabb: [u32; 6]) -> Vec<f32> {
        match self {
            DensityStorage::Dense { d, res } => {
                let [x0, y0, z0, x1, y1, z1] = aabb;
                let r = *res as usize;
                let (dx, dy, dz) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize, (z1 - z0 + 1) as usize);
                let mut out = Vec::with_capacity(dx * dy * dz);
                for z in z0..=z1 { for y in y0..=y1 { for x in x0..=x1 {
                    out.push(d[x as usize + y as usize * r + z as usize * r * r]);
                }}}
                out
            }
            DensityStorage::Sparse(b) => b.subvolume(aabb),
        }
    }
    /// Sparse brick-atlas mirror for the GPU mesh (Phase 4b). `None` for `Dense` (which keeps the dense
    /// mesh-buffer path). The GPU INDEX/POOL buffers upload these slices verbatim.
    pub fn index_slice(&self) -> Option<&[u32]> {
        match self { DensityStorage::Sparse(b) => Some(b.index_slice()), DensityStorage::Dense { .. } => None }
    }
    pub fn bricks_slice(&self) -> Option<&[f32]> {
        match self { DensityStorage::Sparse(b) => Some(b.bricks_slice()), DensityStorage::Dense { .. } => None }
    }
    pub fn brick_res(&self) -> u32 {
        match self { DensityStorage::Sparse(b) => b.brick_res(), DensityStorage::Dense { res, .. } => (res + 15) / 16 }
    }
    /// True when this grid uses the sparse brick backing (→ the GPU mesh atlas path).
    pub fn is_sparse(&self) -> bool { matches!(self, DensityStorage::Sparse(_)) }

    // --- GPU brick-atlas sync state (Phase 4b-2): delegate to BrickGrid; Dense never uses the atlas
    //     (it keeps the dense res³ mesh buffer), so these report "nothing to sync".
    /// Whole atlas must be re-uploaded (fresh grid, fill/fill_from reset, or gc_arena remap). Dense → false.
    pub fn gc_pending(&self) -> bool {
        match self { DensityStorage::Sparse(b) => b.gc_pending(), DensityStorage::Dense { .. } => false }
    }
    /// Inclusive `(lo, hi)` brick range whose INDEX tag changed, or `None`. Dense → None.
    pub fn index_dirty_range(&self) -> Option<(u32, u32)> {
        match self { DensityStorage::Sparse(b) => b.index_dirty_range(), DensityStorage::Dense { .. } => None }
    }
    /// Brick indices whose POOL block changed since the last `clear_sync_dirty`. Dense → None.
    pub fn dirty_bricks(&self) -> Option<&std::collections::HashSet<u32>> {
        match self { DensityStorage::Sparse(b) => Some(b.dirty_bricks()), DensityStorage::Dense { .. } => None }
    }
    /// `Some((handle, &block))` for brick `bi` IF it currently holds an allocated 16³ POOL block (handle
    /// into the arena + its `BRICK_VOX` floats), else `None` (implicit EMPTY/FULL → its slot is unused).
    /// Lets the app patch a GPU pool slot for a dirty brick WITHOUT knowing the tag encoding. Dense → None.
    pub fn brick_pool_block(&self, bi: u32) -> Option<(u32, &[f32])> {
        match self { DensityStorage::Sparse(b) => b.brick_pool_block(bi), DensityStorage::Dense { .. } => None }
    }
    /// Reset sync bookkeeping once the GPU atlas mirrors the current CPU state. No-op for Dense.
    pub fn clear_sync_dirty(&mut self) {
        if let DensityStorage::Sparse(b) = self { b.clear_sync_dirty(); }
    }

    /// Compact EVERY brick that is uniform (all 0.0 → EMPTY, all 1.0 → FULL) back to an implicit tag,
    /// then reclaim the arena memory. One-shot after a full fill / resample — this is where a solid
    /// model's interior bricks become 0-byte FULL tags (the memory win). No-op for `Dense`.
    pub fn compact_all(&mut self) {
        if let DensityStorage::Sparse(b) = self { b.compact_all(); }
    }
    /// Compact uniform bricks overlapping a voxel AABB back to implicit tags (slots recycled, no arena
    /// GC). Per-stroke cleanup over the edited region. No-op for `Dense`.
    pub fn compact_aabb(&mut self, aabb: [u32; 6]) {
        if let DensityStorage::Sparse(b) = self { b.compact_aabb(aabb); }
    }
    /// Phase 5b: allocate a POOL slot for every implicit brick overlapping `aabb` (so a GPU-resident
    /// brush can write those slots in place). No-op for Dense. Marks the bricks dirty for the atlas sync.
    pub fn materialize_box(&mut self, aabb: [u32; 6]) {
        if let DensityStorage::Sparse(b) = self { b.materialize_box(aabb); }
    }
    /// Phase A (GPU-authority undo): materialize like `materialize_box` and report `(bi, prev_tag, handle)`
    /// for every overlapped brick — `prev_tag` = its pre-call INDEX tag, `handle` = its LIVE pool handle
    /// after materialize. Empty for Dense (the box path uses CPU-delta undo).
    pub fn materialize_box_report(&mut self, aabb: [u32; 6]) -> Vec<(u32, u32, u32)> {
        match self { DensityStorage::Sparse(b) => b.materialize_box_report(aabb), DensityStorage::Dense { .. } => Vec::new() }
    }
    /// Materialize only the explicitly listed sparse bricks and report `(bi, prev_tag, handle)` for
    /// GPU first-touch undo. Input may contain duplicates or be unsorted; the implementation emits a
    /// deterministic, ascending, unique report. Empty for Dense.
    pub fn materialize_bricks_report(&mut self, brick_indices: &[u32]) -> Vec<(u32, u32, u32)> {
        match self {
            DensityStorage::Sparse(b) => b.materialize_bricks_report(brick_indices),
            DensityStorage::Dense { .. } => Vec::new(),
        }
    }
    /// Phase A: a brick's CURRENT INDEX tag (handle or TAG_EMPTY/FULL) — for the redo snapshot. Dense → 0.
    pub fn index_tag(&self, bi: u32) -> u32 {
        match self { DensityStorage::Sparse(b) => b.index_tag(bi), DensityStorage::Dense { .. } => 0 }
    }
    /// Phase A: true if `tag` is an allocated POOL handle (vs an implicit EMPTY/FULL tag). The app uses
    /// this to decide whether a brick's snapshot POOL slot must be copied back on undo (implicit bricks
    /// are restored by the INDEX tag alone — their pool slot is a don't-care). Encapsulates the tag
    /// encoding so callers never compare against the raw constants.
    pub fn tag_is_allocated(tag: u32) -> bool { tag != TAG_EMPTY && tag != TAG_FULL }
    /// Phase A (GPU-authority undo): restore each brick's INDEX `tag` (marks INDEX dirty so the next sync
    /// re-uploads only the index; does NOT free handles or touch the POOL dirty set). No-op for Dense.
    pub fn restore_atlas_undo(&mut self, bricks: &[(u32, u32)]) {
        if let DensityStorage::Sparse(b) = self { b.restore_atlas_undo(bricks); }
    }
    /// Phase B (sparse materialise): f32 count in the brick POOL arena. The GPU pool mirrors it 1:1, so
    /// the app reads back exactly this many floats. Dense / all-implicit Sparse → 0 (the INDEX alone
    /// already describes the field — nothing to materialise).
    pub fn arena_floats(&self) -> usize {
        match self { DensityStorage::Sparse(b) => b.arena_floats(), DensityStorage::Dense { .. } => 0 }
    }
    /// Phase B (sparse materialise): overwrite the brick POOL arena with GPU-read contents (handle-indexed,
    /// identical layout). Refreshes only the stale brick CONTENTS after GPU atlas strokes — the INDEX is
    /// already authoritative, so NO dense res³ readback is needed. No-op for Dense.
    pub fn refresh_pool_arena(&mut self, pool: &[f32]) {
        if let DensityStorage::Sparse(b) = self { b.refresh_pool_arena(pool); }
    }
    /// Overwrite one pool handle's 16³ block (4096 floats) from GPU materialise of a single slot.
    pub fn refresh_pool_handle(&mut self, handle: u32, block: &[f32]) {
        if let DensityStorage::Sparse(b) = self { b.refresh_pool_handle(handle, block); }
    }
    /// Allocated pool handles for bricks overlapping a density AABB (for partial GPU→CPU materialise).
    pub fn allocated_handles_in_aabb(&self, aabb: [u32; 6]) -> Vec<u32> {
        match self {
            DensityStorage::Sparse(b) => b.allocated_handles_in_aabb(aabb),
            DensityStorage::Dense { .. } => Vec::new(),
        }
    }
    /// Fill the WHOLE grid from a per-voxel function `f(x,y,z) -> density`. For Sparse this is
    /// brick-aware: a brick whose voxels are uniformly 0.0/1.0 becomes an implicit EMPTY/FULL tag with
    /// NO allocation — so filling a solid sphere/box never transiently materialises its interior (the
    /// ~400 MB spike a per-voxel `set_idx` fill would cause at 512³). The output is identical to a
    /// per-voxel fill; no `compact_all` is needed afterwards.
    pub fn fill_from<F: Fn(u32, u32, u32) -> f32>(&mut self, f: F) {
        match self {
            DensityStorage::Dense { d, res } => {
                let r = *res;
                for z in 0..r { for y in 0..r { for x in 0..r {
                    d[(x + y * r + z * r * r) as usize] = f(x, y, z);
                }}}
            }
            DensityStorage::Sparse(b) => b.fill_from(f),
        }
    }

    /// Parallel `fill_from` (rayon). BYTE-IDENTICAL output to the serial `fill_from`: it runs the SAME
    /// per-voxel evaluation across cores. Used by `VoxelGrid::resample` — the closure reads the immutable
    /// source field, so it is `Sync`-safe. Sparse routes through `BrickGrid::par_fill_from` (the big win:
    /// ~1B trilinear samples at 1024³ across all cores); Dense parallelises the dense fill over z-slices.
    pub fn par_fill_from<F: Fn(u32, u32, u32) -> f32 + Sync>(&mut self, f: F) {
        self.par_fill_from_skip(f, |_, _, _| None);
    }

    /// `par_fill_from` with a per-brick `skip(bx,by,bz) -> Option<f32>` fast path (sparse only). For
    /// Sparse this skips the 4096-voxel sample loop where `skip` proves the brick is uniform; for Dense
    /// the per-voxel parallel fill runs unchanged (no brick tags → `skip` is ignored). Output is identical
    /// to `par_fill_from` whenever `skip` only fires on provably-uniform bricks (see `region_const`).
    pub fn par_fill_from_skip<F, S>(&mut self, f: F, skip: S)
    where
        F: Fn(u32, u32, u32) -> f32 + Sync,
        S: Fn(u32, u32, u32) -> Option<f32> + Sync,
    {
        match self {
            DensityStorage::Dense { d, res } => {
                use rayon::prelude::*;
                let r = *res;
                let plane = (r * r) as usize;
                // Each z-slice is an independent `plane`-sized chunk → write disjoint regions in parallel.
                d.par_chunks_mut(plane).enumerate().for_each(|(z, slice)| {
                    let z = z as u32;
                    for y in 0..r { for x in 0..r {
                        slice[(x + y * r) as usize] = f(x, y, z);
                    }}
                });
            }
            DensityStorage::Sparse(b) => b.par_fill_from_skip(f, skip),
        }
    }

    /// Sparse-aware resample fast path: `Some(0.0)`/`Some(1.0)` if the source INDEX over this voxel-coord
    /// AABB is uniformly empty/full (reads only tags), else `None` (mixed / handle brick / non-empty OOB
    /// span). Dense → `None` (no brick tags; the dense resample path samples every voxel). See
    /// `BrickGrid::region_const`.
    pub fn region_const(&self, aabb: [i32; 6]) -> Option<f32> {
        match self {
            DensityStorage::Sparse(b) => b.region_const(aabb[0], aabb[1], aabb[2], aabb[3], aabb[4], aabb[5]),
            DensityStorage::Dense { .. } => None,
        }
    }
}

/// Sparse density storage: a 2-level brick pool. The grid is divided into `BRICK³`-voxel bricks; a
/// DENSE top-level `index` (one `u32` per brick) tags each brick `EMPTY` (all 0.0, implicit), `FULL`
/// (all 1.0, implicit), or a HANDLE into the `bricks` arena (a materialised 16³ block). Only the thin
/// surface shell + non-uniform regions allocate bricks — a 512³ solid is FULL-dominated → near-zero
/// field memory (vs 536 MB dense). Point access is O(1): one index load + (if allocated) one arena
/// load, pure shift/mask, no hashing, no division on the hot read.
///
/// Phase 3a: core read/write/materialise + `to_dense`/`z_slice`. (`brick_minmax` for the surface mask,
/// compaction, and the per-chunk gather come in 3c–3e.)
#[derive(Debug, Clone)]
pub struct BrickGrid {
    res: u32,
    brick_res: u32, // bricks per axis = ceil(res / BRICK)
    pub(super) index: Vec<u32>, // brick_res³ tags: TAG_EMPTY | TAG_FULL | handle
    pub(super) bricks: Vec<f32>, // flat arena; brick handle h occupies [h·BRICK_VOX .. +BRICK_VOX]
    pub(super) free: Vec<u32>, // recycled brick handles
    // --- GPU brick-atlas sync state (Phase 4b-2) ---
    // What changed since the last `clear_sync_dirty`, so the app patches the GPU INDEX/POOL per-brick
    // instead of re-uploading the whole grid. CPU-only bookkeeping; never affects `to_dense`/`at`.
    dirty: std::collections::HashSet<u32>, // brick indices (bi) whose POOL block changed
    idx_lo: u32,    // min bi whose INDEX tag changed since clear (idx_lo > idx_hi ⇒ none)
    idx_hi: u32,
    gc_dirty: bool, // whole atlas must be re-uploaded (fresh fill, or gc_arena remapped handles)
}

/// Voxels per brick axis. NOT tied to `VOXEL_CHUNK` (a chunk is 16 *cells* = 17 voxels) — same number,
/// different meaning. The per-chunk mesh gather (Phase 3) spans whatever bricks a chunk touches.
/// Edge length of one sparse voxel-density brick. Public so the app-side atlas work planner uses the
/// exact same cell-to-brick mapping as `BrickGrid` instead of duplicating a magic `16`.
pub const VOXEL_BRICK_SIZE: u32 = 16;
pub(super) const BRICK: u32 = VOXEL_BRICK_SIZE;
pub(super) const BRICK_VOX: usize = (BRICK * BRICK * BRICK) as usize; // 4096
pub(super) const TAG_EMPTY: u32 = 0xFFFF_FFFF; // implicit all-0.0
pub(super) const TAG_FULL: u32 = 0xFFFF_FFFE; // implicit all-1.0

impl BrickGrid {
    pub(super) fn new(res: u32, val: f32) -> Self {
        let brick_res = (res + BRICK - 1) / BRICK;
        let nb = (brick_res as usize).pow(3);
        let mut g = BrickGrid {
            res,
            brick_res,
            index: vec![TAG_EMPTY; nb],
            bricks: Vec::new(),
            free: Vec::new(),
            dirty: std::collections::HashSet::new(),
            idx_lo: u32::MAX, // empty range
            idx_hi: 0,
            gc_dirty: true, // a fresh grid → the first atlas sync does a full upload
        };
        if val != 0.0 {
            if val == 1.0 {
                g.index.iter_mut().for_each(|t| *t = TAG_FULL);
            } else {
                // Uniform fractional fill: materialise every brick (rare; `new_empty` uses 0.0).
                for bi in 0..nb {
                    let h = g.alloc_brick(val);
                    g.index[bi] = h;
                }
            }
        }
        g
    }

    /// P0.5 persistence (D4): export EMPTY/FULL tags verbatim and each ALLOCATED brick remapped to a
    /// SEQUENTIAL storage index, its 4096 floats appended in that order. GPU-sync bookkeeping
    /// (dirty/idx_lo/idx_hi/gc_dirty/free) is intentionally NOT exported — `from_parts` rebuilds it fresh.
    /// Returns (compact_tags[brick_res³], brick_data).
    pub(crate) fn to_parts(&self) -> (Vec<u32>, Vec<f32>) {
        let mut tags = Vec::with_capacity(self.index.len());
        let mut data: Vec<f32> = Vec::new();
        let mut next: u32 = 0;
        for &t in &self.index {
            match t {
                TAG_EMPTY | TAG_FULL => tags.push(t),
                h => {
                    let o = h as usize * BRICK_VOX;
                    data.extend_from_slice(&self.bricks[o..o + BRICK_VOX]);
                    tags.push(next);
                    next += 1;
                }
            }
        }
        (tags, data)
    }

    /// Rebuild from `to_parts`. The sequential storage index in `tags` IS the arena handle (bricks were
    /// appended in order), so `index = tags` and `bricks = data` reconstruct the field directly. Sync
    /// state starts fresh (gc_dirty=true → the next GPU atlas sync re-uploads).
    pub(crate) fn from_parts(res: u32, tags: Vec<u32>, data: Vec<f32>) -> Self {
        BrickGrid {
            res,
            brick_res: (res + BRICK - 1) / BRICK,
            index: tags,
            bricks: data,
            free: Vec::new(),
            dirty: std::collections::HashSet::new(),
            idx_lo: u32::MAX,
            idx_hi: 0,
            gc_dirty: true,
        }
    }

    #[inline]
    fn brick_idx(&self, x: u32, y: u32, z: u32) -> usize {
        let (bx, by, bz) = (x / BRICK, y / BRICK, z / BRICK);
        (bx + by * self.brick_res + bz * self.brick_res * self.brick_res) as usize
    }

    /// Allocate (or recycle) a brick filled with `fill`; returns its handle.
    fn alloc_brick(&mut self, fill: f32) -> u32 {
        if let Some(h) = self.free.pop() {
            let o = h as usize * BRICK_VOX;
            self.bricks[o..o + BRICK_VOX].iter_mut().for_each(|x| *x = fill);
            h
        } else {
            let h = (self.bricks.len() / BRICK_VOX) as u32;
            self.bricks.resize(self.bricks.len() + BRICK_VOX, fill);
            h
        }
    }

    /// Extend the INDEX-changed brick range (materialise or demote of brick `bi`). See `index_dirty_range`.
    #[inline]
    fn mark_idx(&mut self, bi: u32) {
        self.idx_lo = self.idx_lo.min(bi);
        self.idx_hi = self.idx_hi.max(bi);
    }

    /// In-bounds point read (`x,y,z < res`).
    #[inline]
    pub(super) fn at(&self, x: u32, y: u32, z: u32) -> f32 {
        let bi = self.brick_idx(x, y, z);
        match self.index[bi] {
            TAG_EMPTY => 0.0,
            TAG_FULL => 1.0,
            h => {
                let li = ((x % BRICK) + (y % BRICK) * BRICK + (z % BRICK) * BRICK * BRICK) as usize;
                self.bricks[h as usize * BRICK_VOX + li]
            }
        }
    }

    /// OOB-clamped read (outside the grid → 0.0).
    #[inline]
    fn at_oob(&self, x: i32, y: i32, z: i32) -> f32 {
        let r = self.res as i32;
        if x < 0 || y < 0 || z < 0 || x >= r || y >= r || z >= r { return 0.0; }
        self.at(x as u32, y as u32, z as u32)
    }

    /// Sparse-aware resample fast path: is the INDEX over this voxel-coord AABB uniformly EMPTY (every
    /// covered voxel reads 0.0) or uniformly FULL (every covered voxel reads 1.0)? Reads ONLY index tags
    /// (no arena), expanding the voxel AABB to whole bricks (only ENLARGES the tested set — never makes
    /// uniformity easier to satisfy). Returns `Some(0.0)` / `Some(1.0)` for a proven-uniform region, else
    /// `None` (mixed, an allocated handle brick — conservatively treated as possibly-mixed — or an OOB
    /// span that touches a non-empty brick). Used by `resample` to emit a destination brick's tag without
    /// 4096 trilinear samples when the SOURCE region it reads is uniform. OOB reads are 0.0 (`at_oob`), so
    /// an OOB span can be EMPTY but NEVER FULL — `oob` forces `all_full=false`.
    fn region_const(&self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) -> Option<f32> {
        let r = self.res as i32;
        let oob = x0 < 0 || y0 < 0 || z0 < 0 || x1 >= r || y1 >= r || z1 >= r;
        let br = self.brick_res;
        let bclamp = |v: i32| ((v.max(0) as u32) / BRICK).min(br - 1);
        let (mut all_empty, mut all_full) = (true, true);
        if oob { all_full = false; } // an OOB corner reads 0.0 → the region cannot be uniformly FULL
        for bz in bclamp(z0)..=bclamp(z1) {
            for by in bclamp(y0)..=bclamp(y1) {
                for bx in bclamp(x0)..=bclamp(x1) {
                    let bi = (bx + by * br + bz * br * br) as usize;
                    match self.index[bi] {
                        TAG_EMPTY => all_full = false,
                        TAG_FULL => all_empty = false,
                        _ => return None, // allocated handle brick → may be mixed → never skip
                    }
                    if !all_empty && !all_full { return None; }
                }
            }
        }
        if all_empty { Some(0.0) } else if all_full { Some(1.0) } else { None }
    }

    #[inline]
    fn decode(&self, i: usize) -> (u32, u32, u32) {
        let r = self.res as usize;
        ((i % r) as u32, ((i / r) % r) as u32, (i / (r * r)) as u32)
    }

    #[inline]
    fn get_idx(&self, i: usize) -> f32 {
        let (x, y, z) = self.decode(i);
        self.at(x, y, z)
    }

    pub(super) fn set(&mut self, x: u32, y: u32, z: u32, v: f32) {
        let bi = self.brick_idx(x, y, z);
        let li = ((x % BRICK) + (y % BRICK) * BRICK + (z % BRICK) * BRICK * BRICK) as usize;
        match self.index[bi] {
            // Writing the brick's implicit value back is a no-op (keeps the brick implicit → memory).
            TAG_EMPTY => {
                if v != 0.0 {
                    let h = self.alloc_brick(0.0);
                    self.index[bi] = h;
                    self.bricks[h as usize * BRICK_VOX + li] = v;
                    self.mark_idx(bi as u32); // tag → handle: INDEX entry changed
                    self.dirty.insert(bi as u32); // POOL block now holds real data
                }
            }
            TAG_FULL => {
                if v != 1.0 {
                    let h = self.alloc_brick(1.0);
                    self.index[bi] = h;
                    self.bricks[h as usize * BRICK_VOX + li] = v;
                    self.mark_idx(bi as u32); // tag → handle: INDEX entry changed
                    self.dirty.insert(bi as u32);
                }
            }
            h => {
                self.bricks[h as usize * BRICK_VOX + li] = v;
                self.dirty.insert(bi as u32); // POOL block mutated (conservative: marks even no-op writes)
            }
        }
    }

    #[inline]
    fn set_idx(&mut self, i: usize, v: f32) {
        let (x, y, z) = self.decode(i);
        self.set(x, y, z, v);
    }

    fn fill(&mut self, val: f32) {
        self.bricks.clear();
        self.free.clear();
        self.gc_dirty = true; // whole grid reset → re-upload everything
        if val == 0.0 {
            self.index.iter_mut().for_each(|t| *t = TAG_EMPTY);
        } else if val == 1.0 {
            self.index.iter_mut().for_each(|t| *t = TAG_FULL);
        } else {
            for bi in 0..self.index.len() {
                let h = self.alloc_brick(val);
                self.index[bi] = h;
            }
        }
    }

    fn to_dense(&self) -> Vec<f32> { self.materialize_z(0, self.res - 1) }

    fn z_slice(&self, z0: u32, z1: u32) -> Vec<f32> { self.materialize_z(z0, z1) }

    /// Gather a 3D BOX `[x0..=x1, y0..=y1, z0..=z1]` into a dense `Vec`, box-local layout (index =
    /// `(x-x0) + (y-y0)·dx + (z-z0)·dx·dy`). Unlike `z_slice` (full-XY) this is bounded in ALL axes, so
    /// the GPU brush working buffer scales with the brush AABB, not res² — the Phase-4 key to 1024³.
    ///
    /// BRICK-LEVEL (mirrors `materialize_z`): EMPTY bricks leave the zero-init output, FULL bricks fill
    /// 1.0 per contiguous x-run, ALLOCATED bricks `copy_from_slice` each x-run. A big brush box at the
    /// surface is mostly EMPTY/FULL bricks, so the gather drops from O(box) per-voxel `at()` decodes to
    /// ~O(shell) — the fix for the multi-million-voxel-per-frame brush stutter at 512³+ (a per-voxel
    /// decode of a large box was the dominant input-handler cost).
    fn subvolume(&self, aabb: [u32; 6]) -> Vec<f32> {
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let (dx, dy, dz) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize, (z1 - z0 + 1) as usize);
        let mut out = vec![0.0f32; dx * dy * dz];
        let br = self.brick_res;
        for bz in (z0 / BRICK)..=(z1 / BRICK) {
            let (gz_lo, gz_hi) = ((bz * BRICK).max(z0), (bz * BRICK + BRICK - 1).min(z1));
            for by in (y0 / BRICK)..=(y1 / BRICK) {
                let (gy_lo, gy_hi) = ((by * BRICK).max(y0), (by * BRICK + BRICK - 1).min(y1));
                for bx in (x0 / BRICK)..=(x1 / BRICK) {
                    let tag = self.index[(bx + by * br + bz * br * br) as usize];
                    if tag == TAG_EMPTY { continue; } // output already 0.0
                    let (gx_lo, gx_hi) = ((bx * BRICK).max(x0), (bx * BRICK + BRICK - 1).min(x1));
                    let run = (gx_hi - gx_lo + 1) as usize;
                    for gz in gz_lo..=gz_hi {
                        let lz = (gz & 15) as usize;
                        for gy in gy_lo..=gy_hi {
                            let ob = (gx_lo - x0) as usize + (gy - y0) as usize * dx + (gz - z0) as usize * dx * dy;
                            if tag == TAG_FULL {
                                for v in &mut out[ob..ob + run] { *v = 1.0; }
                            } else {
                                let sb = tag as usize * BRICK_VOX + (gx_lo & 15) as usize + (gy & 15) as usize * 16 + lz * 256;
                                out[ob..ob + run].copy_from_slice(&self.bricks[sb..sb + run]);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Materialise the full-XY Z-slice range `[z0..=z1]` into a dense `Vec` — BRICK-LEVEL so empty
    /// space costs nothing and the per-frame band gather stays fast at 512³ (a per-voxel `at()` decode
    /// would be ~4 M decodes/frame). EMPTY bricks leave the zero-init output; FULL bricks fill 1.0;
    /// ALLOCATED bricks `copy_from_slice` each contiguous x-run. Byte-identical to the per-voxel form.
    fn materialize_z(&self, z0: u32, z1: u32) -> Vec<f32> {
        let r = self.res as usize;
        let nz = (z1 - z0 + 1) as usize;
        let mut out = vec![0.0f32; nz * r * r];
        let br = self.brick_res;
        for bz in (z0 / BRICK)..=(z1 / BRICK) {
            for by in 0..br {
                for bx in 0..br {
                    let tag = self.index[(bx + by * br + bz * br * br) as usize];
                    if tag == TAG_EMPTY { continue; } // output already 0.0
                    let gx0 = bx * BRICK;
                    let gx1 = ((bx + 1) * BRICK).min(self.res); // exclusive
                    let gy1 = ((by + 1) * BRICK).min(self.res);
                    let gz_lo = (bz * BRICK).max(z0);
                    let gz_hi = (((bz + 1) * BRICK).min(self.res)).min(z1 + 1); // exclusive
                    let xlen = (gx1 - gx0) as usize;
                    for gz in gz_lo..gz_hi {
                        let lz = (gz & 15) as usize;
                        for gy in (by * BRICK)..gy1 {
                            let ly = (gy & 15) as usize;
                            let ob = (gz - z0) as usize * r * r + gy as usize * r + gx0 as usize;
                            if tag == TAG_FULL {
                                for v in &mut out[ob..ob + xlen] { *v = 1.0; }
                            } else {
                                let sb = tag as usize * BRICK_VOX + ly * 16 + lz * 256; // local x = 0 (brick-aligned)
                                out[ob..ob + xlen].copy_from_slice(&self.bricks[sb..sb + xlen]);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Demote one ALLOCATED brick to an implicit EMPTY/FULL tag IF every voxel is uniformly 0.0/1.0,
    /// recycling its arena slot. A brick uniformly 0.6 (above iso but not 1.0) holds erasable data and
    /// is NOT demoted. Value-preserving: an all-1.0 ALLOCATED brick reads identically to a FULL tag.
    pub(super) fn compact_brick(&mut self, bi: usize) {
        let h = self.index[bi];
        if h == TAG_EMPTY || h == TAG_FULL { return; }
        let o = h as usize * BRICK_VOX;
        let first = self.bricks[o];
        if first == 0.0 && self.bricks[o..o + BRICK_VOX].iter().all(|&v| v == 0.0) {
            self.index[bi] = TAG_EMPTY;
            self.free.push(h);
            self.mark_idx(bi as u32); // handle → tag: INDEX entry changed (POOL slot now dead, never read)
        } else if first == 1.0 && self.bricks[o..o + BRICK_VOX].iter().all(|&v| v == 1.0) {
            self.index[bi] = TAG_FULL;
            self.free.push(h);
            self.mark_idx(bi as u32);
        }
    }

    /// Compact uniform bricks overlapping a voxel AABB (recycles slots; no arena GC — freed slots are
    /// reused by later edits). Per-stroke cleanup.
    fn compact_aabb(&mut self, aabb: [u32; 6]) {
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let br = self.brick_res;
        let bclamp = |v: u32| (v / BRICK).min(br - 1);
        for bz in bclamp(z0)..=bclamp(z1) {
            for by in bclamp(y0)..=bclamp(y1) {
                for bx in bclamp(x0)..=bclamp(x1) {
                    let bi = (bx + by * br + bz * br * br) as usize;
                    self.compact_brick(bi);
                }
            }
        }
    }

    /// Materialise (allocate a POOL handle for) every IMPLICIT brick overlapping a voxel AABB, filled
    /// with its current implicit value (EMPTY→0.0, FULL→1.0). After this, every brick in the box holds
    /// a real handle, so a GPU brush can WRITE those POOL slots in place (Phase 5b — the GPU-resident
    /// brush has no GPU-side allocator, so the CPU pre-allocates the slots it will write). Marks the
    /// touched bricks dirty so the next atlas sync uploads them. `compact_aabb` demotes the still-uniform
    /// ones back on stroke release, so the arena growth is transient (the edited shell stays allocated).
    fn materialize_box(&mut self, aabb: [u32; 6]) {
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let br = self.brick_res;
        let bclamp = |v: u32| (v / BRICK).min(br - 1);
        for bz in bclamp(z0)..=bclamp(z1) {
            for by in bclamp(y0)..=bclamp(y1) {
                for bx in bclamp(x0)..=bclamp(x1) {
                    let bi = (bx + by * br + bz * br * br) as usize;
                    let fill = match self.index[bi] {
                        TAG_EMPTY => 0.0,
                        TAG_FULL => 1.0,
                        _ => continue, // already an allocated handle
                    };
                    self.alloc_handle_at(bi, fill);
                }
            }
        }
    }

    /// Like `materialize_box`, but ALSO reports every brick overlapping `aabb` as
    /// `(bi, prev_tag, handle)`:
    /// - `prev_tag` is the brick's INDEX entry BEFORE this call (a handle if already allocated, else
    ///   TAG_EMPTY/FULL) — the tag the GPU-authority undo (Phase A) must RESTORE on undo (a brick
    ///   reported with TAG_EMPTY/FULL must return to that implicit tag, not its freshly-allocated handle).
    /// - `handle` is the brick's LIVE pool handle AFTER this call (== `prev_tag` for an already-allocated
    ///   brick; the new handle for a freshly-materialised one). The undo capture snapshots the POOL slot
    ///   at `handle * BRICK_VOX` BEFORE the brush scatter overwrites it.
    ///
    /// All overlapped bricks are reported every frame; the caller first-touch-dedups so each brick is
    /// snapshotted once, before the scatter. (A brick reported with an EMPTY/FULL `prev_tag` always has a
    /// real `handle` here — `materialize` allocated it — so the snapshot reads the just-filled implicit
    /// block; on undo the INDEX tag is restored, making the pool slot a don't-care.)
    fn materialize_box_report(&mut self, aabb: [u32; 6]) -> Vec<(u32, u32, u32)> {
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let br = self.brick_res;
        let bclamp = |v: u32| (v / BRICK).min(br - 1);
        let mut report = Vec::new();
        for bz in bclamp(z0)..=bclamp(z1) {
            for by in bclamp(y0)..=bclamp(y1) {
                for bx in bclamp(x0)..=bclamp(x1) {
                    let bi = (bx + by * br + bz * br * br) as usize;
                    let prev_tag = self.index[bi];
                    let handle = match prev_tag {
                        TAG_EMPTY => self.alloc_handle_at(bi, 0.0),
                        TAG_FULL => self.alloc_handle_at(bi, 1.0),
                        h => h, // already allocated — its handle IS prev_tag
                    };
                    report.push((bi as u32, prev_tag, handle));
                }
            }
        }
        report
    }

    /// Explicit-brick form of `materialize_box_report`. Sorting and dedup happen here—not at callers—so
    /// duplicate overlapping stamp AABBs can never snapshot a newly allocated handle as a second
    /// "previous" state. That keeps GPU first-touch undo valid by construction.
    fn materialize_bricks_report(&mut self, brick_indices: &[u32]) -> Vec<(u32, u32, u32)> {
        let mut unique = brick_indices.to_vec();
        unique.retain(|&bi| (bi as usize) < self.index.len());
        unique.sort_unstable();
        unique.dedup();

        let mut report = Vec::with_capacity(unique.len());
        for bi in unique {
            let i = bi as usize;
            let prev_tag = self.index[i];
            let handle = match prev_tag {
                TAG_EMPTY => self.alloc_handle_at(i, 0.0),
                TAG_FULL => self.alloc_handle_at(i, 1.0),
                h => h,
            };
            report.push((bi, prev_tag, handle));
        }
        report
    }

    /// Materialise brick `bi` (currently an implicit tag) into a pool slot filled with `fill`, marking
    /// it dirty for the atlas sync. Returns the new handle. Helper for `materialize_box_report` (and a
    /// drop-in for `materialize_box`'s inline body) so the alloc+mark logic lives once.
    fn alloc_handle_at(&mut self, bi: usize, fill: f32) -> u32 {
        let h = self.alloc_brick(fill);
        self.index[bi] = h;
        self.mark_idx(bi as u32);
        self.dirty.insert(bi as u32);
        h
    }

    /// Read a brick's CURRENT INDEX tag (a handle, or TAG_EMPTY/TAG_FULL). The GPU-authority redo
    /// snapshot reads this to capture the post-undo (i.e. pre-redo) tag for each brick. Pure read.
    pub(super) fn index_tag(&self, bi: u32) -> u32 { self.index[bi as usize] }

    /// GPU-authority undo (Phase A): restore each brick's INDEX tag to `tag` and mark the INDEX dirty if
    /// it changed, so the next atlas sync re-uploads ONLY the restored INDEX range. Does NOT free handles
    /// (atlas-undo strokes are never compacted, so handles stay stable across undo/redo) and does NOT
    /// touch the POOL `dirty` set (the GPU pool was already restored by the app via a GPU→GPU copy — if
    /// we marked the pool dirty, the next sync would overwrite that restore with the stale CPU arena).
    fn restore_atlas_undo(&mut self, bricks: &[(u32, u32)]) {
        for &(bi, tag) in bricks {
            if self.index[bi as usize] != tag {
                self.index[bi as usize] = tag;
                self.mark_idx(bi);
            }
        }
    }

    /// Compact every brick, then GC the arena so freed interior bricks actually reclaim memory.
    fn compact_all(&mut self) {
        for bi in 0..self.index.len() {
            self.compact_brick(bi);
        }
        self.gc_arena();
    }

    /// Rebuild the `bricks` arena to hold ONLY still-referenced handles (remapping `index`), reclaiming
    /// the memory of compacted-away bricks. Without this, the arena `Vec` stays at its peak size after a
    /// fill demotes the interior to tags. Handles are reassigned in ascending order (stable, cache-tidy).
    fn gc_arena(&mut self) {
        let nbricks = self.bricks.len() / BRICK_VOX;
        let mut live = vec![false; nbricks];
        for &t in &self.index {
            if t != TAG_EMPTY && t != TAG_FULL { live[t as usize] = true; }
        }
        if live.iter().all(|&l| l) && self.free.is_empty() { return; } // nothing freed
        let mut remap = vec![u32::MAX; nbricks];
        let mut new_bricks: Vec<f32> = Vec::with_capacity(live.iter().filter(|&&l| l).count() * BRICK_VOX);
        let mut new_h = 0u32;
        for (h, &is_live) in live.iter().enumerate() {
            if is_live {
                remap[h] = new_h;
                let o = h * BRICK_VOX;
                new_bricks.extend_from_slice(&self.bricks[o..o + BRICK_VOX]);
                new_h += 1;
            }
        }
        for t in self.index.iter_mut() {
            if *t != TAG_EMPTY && *t != TAG_FULL { *t = remap[*t as usize]; }
        }
        self.bricks = new_bricks;
        self.free.clear();
        self.gc_dirty = true; // handles remapped → POOL layout changed → whole atlas must re-upload
    }

    /// Count of currently-materialised bricks (arena slots minus the free list). Memory ≈ this × 16 KiB.
    pub(super) fn allocated_bricks(&self) -> usize {
        self.bricks.len() / BRICK_VOX - self.free.len()
    }

    // --- GPU brick-atlas mirror (Phase 4b): the GPU INDEX + POOL buffers are a byte-for-byte copy of
    //     these. The shader's `read_density(x,y,z)` reproduces `at()` exactly: decode brick `(x>>4) +
    //     (y>>4)·brick_res + (z>>4)·brick_res²`, read the tag, EMPTY→0 / FULL→1 / handle→pool[h·4096 +
    //     ((x&15) + (y&15)·16 + (z&15)·256)]. (Proven by `read_density_formula_matches_at`.)
    pub fn index_slice(&self) -> &[u32] { &self.index }
    pub fn bricks_slice(&self) -> &[f32] { &self.bricks }
    pub fn brick_res(&self) -> u32 { self.brick_res }

    /// `Some((handle, &block))` for brick `bi` if it holds an allocated 16³ block, else `None` (implicit).
    fn brick_pool_block(&self, bi: u32) -> Option<(u32, &[f32])> {
        let h = self.index[bi as usize];
        if h == TAG_EMPTY || h == TAG_FULL { return None; }
        let o = h as usize * BRICK_VOX;
        Some((h, &self.bricks[o..o + BRICK_VOX]))
    }

    /// Phase B (sparse materialise): number of f32 in the POOL arena. The GPU pool mirrors the arena 1:1
    /// (handle `h` ↔ `[h·BRICK_VOX .. +BRICK_VOX]` on BOTH sides — atlas strokes never compact, so the
    /// layout is stable), so a GPU→CPU materialise reads back exactly this many floats from pool slot 0.
    fn arena_floats(&self) -> usize { self.bricks.len() }

    /// Phase B (sparse materialise): overwrite the POOL arena with GPU-read pool contents. The INDEX is
    /// already CPU-authoritative (tags are current even mid-stroke); only the brick CONTENTS go stale when
    /// a GPU atlas stroke writes the pool in place. This refreshes ONLY those contents — no dense res³
    /// readback, no `fill_from`. The GPU pool may be longer than the live arena (capacity headroom); only
    /// the live prefix is meaningful, so copy the overlapping prefix. Sync bookkeeping is untouched: the
    /// CPU now MATCHES the GPU, so the existing dirty set still correctly describes what (if anything) a
    /// future sync owes — refreshing from the GPU does not create new GPU debt.
    fn refresh_pool_arena(&mut self, pool: &[f32]) {
        let n = self.bricks.len().min(pool.len());
        self.bricks[..n].copy_from_slice(&pool[..n]);
    }

    fn refresh_pool_handle(&mut self, handle: u32, block: &[f32]) {
        let o = handle as usize * BRICK_VOX;
        if block.len() >= BRICK_VOX && o + BRICK_VOX <= self.bricks.len() {
            self.bricks[o..o + BRICK_VOX].copy_from_slice(&block[..BRICK_VOX]);
        }
    }

    /// Handles for materialised bricks whose cells overlap `aabb` (expanded to brick bounds).
    fn allocated_handles_in_aabb(&self, aabb: [u32; 6]) -> Vec<u32> {
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let br = self.brick_res.max(1);
        let bclamp = |v: u32| (v / BRICK).min(br - 1);
        let mut out = Vec::new();
        for bz in bclamp(z0)..=bclamp(z1) {
            for by in bclamp(y0)..=bclamp(y1) {
                for bx in bclamp(x0)..=bclamp(x1) {
                    let bi = (bx + by * br + bz * br * br) as usize;
                    if bi >= self.index.len() { continue; }
                    let t = self.index[bi];
                    if t != TAG_EMPTY && t != TAG_FULL {
                        out.push(t);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    // --- GPU brick-atlas sync state (Phase 4b-2): the app reads these once per frame to patch the GPU
    //     INDEX/POOL buffers per-brick, then calls `clear_sync_dirty`. `gc_pending` overrides the rest
    //     (full re-upload). All CPU-only bookkeeping; never affects `at`/`to_dense`/parity.
    /// Brick indices whose POOL block changed since the last `clear_sync_dirty` (handle-relative writes).
    pub fn dirty_bricks(&self) -> &std::collections::HashSet<u32> { &self.dirty }
    /// Inclusive `(lo, hi)` brick range whose INDEX tag changed (materialise/demote), or `None` if clean.
    pub fn index_dirty_range(&self) -> Option<(u32, u32)> {
        if self.idx_lo > self.idx_hi { None } else { Some((self.idx_lo, self.idx_hi)) }
    }
    /// Whole atlas must be re-uploaded: fresh grid, `fill`/`fill_from` reset, or `gc_arena` remap.
    pub fn gc_pending(&self) -> bool { self.gc_dirty }
    /// Reset sync bookkeeping once the GPU atlas mirrors the current CPU state.
    pub fn clear_sync_dirty(&mut self) {
        self.dirty.clear();
        self.idx_lo = u32::MAX;
        self.idx_hi = 0;
        self.gc_dirty = false;
    }

    /// Fill the whole grid from `f(x,y,z)`, BRICK-AWARE: a brick whose IN-GRID voxels are uniformly
    /// 0.0/1.0 becomes an EMPTY/FULL tag with no allocation. Only boundary (mixed) bricks allocate, so
    /// the peak arena ≈ the surface shell — a solid interior never transiently materialises. Resets
    /// the grid first. Output identical to a per-voxel fill.
    pub(super) fn fill_from<F: Fn(u32, u32, u32) -> f32>(&mut self, f: F) {
        self.bricks.clear();
        self.free.clear();
        self.gc_dirty = true; // whole grid rebuilt → re-upload everything
        self.index.iter_mut().for_each(|t| *t = TAG_EMPTY);
        let br = self.brick_res;
        let mut tmp = [0.0f32; BRICK_VOX];
        for bz in 0..br { for by in 0..br { for bx in 0..br {
            let (mut all0, mut all1) = (true, true);
            for lz in 0..BRICK {
                let z = bz * BRICK + lz; if z >= self.res { break; }
                for ly in 0..BRICK {
                    let y = by * BRICK + ly; if y >= self.res { break; }
                    for lx in 0..BRICK {
                        let x = bx * BRICK + lx; if x >= self.res { break; }
                        let v = f(x, y, z);
                        tmp[(lx + ly * BRICK + lz * BRICK * BRICK) as usize] = v;
                        if v != 0.0 { all0 = false; }
                        if v != 1.0 { all1 = false; }
                    }
                }
            }
            let bi = (bx + by * br + bz * br * br) as usize;
            if all0 {
                self.index[bi] = TAG_EMPTY;
            } else if all1 {
                self.index[bi] = TAG_FULL;
            } else {
                let h = self.alloc_brick(0.0);
                let o = h as usize * BRICK_VOX;
                self.bricks[o..o + BRICK_VOX].copy_from_slice(&tmp); // padding voxels (out-of-grid) never read
                self.index[bi] = h;
            }
        }}}
    }

    /// Parallel `fill_from`. BYTE-IDENTICAL to the serial form: same tags, same arena values, same handle
    /// order. The closure `f` reads the (immutable) source field, so it is safe to call from many threads.
    ///
    /// Two phases:
    /// 1. PARALLEL classification (`rayon`): each brick samples its in-grid 16³ voxels via `f` into a local
    ///    `[f32; BRICK_VOX]` (padding voxels left 0.0, never read) and classifies itself Empty / Full /
    ///    Mixed(block) using the EXACT same rules as serial `fill_from` (`all0` → Empty, else `all1` →
    ///    Full, else keep the block). Only MIXED bricks keep a 16 KiB block (≈ surface shell), so the
    ///    arena never transiently materialises a solid interior — same memory profile as serial.
    /// 2. SERIAL assembly in ASCENDING `bi` order: reset the grid, then for each brick set the tag, or for
    ///    a Mixed brick `alloc_brick(0.0)` + `copy_from_slice(block)`. Serial so handle assignment is
    ///    deterministic and matches the serial path exactly (first mixed brick → handle 0, next → 1, …).
    pub(super) fn par_fill_from<F: Fn(u32, u32, u32) -> f32 + Sync>(&mut self, f: F) {
        self.par_fill_from_skip(f, |_, _, _| None);
    }

    /// `par_fill_from` with a per-brick `skip(bx,by,bz) -> Option<f32>` fast path. When `skip` returns
    /// `Some(c)` for a destination brick, the brick is classified directly from `c` (0.0 → EMPTY, else
    /// FULL) WITHOUT running the 4096-voxel `f` loop — BYTE-IDENTICAL to full sampling whenever `skip`
    /// only fires where every voxel of the brick would evaluate to exactly `c` (the resample skip closure
    /// guarantees this via `region_const` over the trilinear footprint, including the +1 neighbor). When
    /// `skip` returns `None` the brick runs the unchanged full classify. Phase-2 assembly is identical, so
    /// the resulting INDEX / arena / free list match `par_fill_from(f)` bit-for-bit. `par_fill_from` is
    /// just this with `skip = |_,_,_| None`. (`skip` reads the immutable source, so it is `Sync`-safe.)
    fn par_fill_from_skip<F, S>(&mut self, f: F, skip: S)
    where
        F: Fn(u32, u32, u32) -> f32 + Sync,
        S: Fn(u32, u32, u32) -> Option<f32> + Sync,
    {
        use rayon::prelude::*;
        let br = self.brick_res;
        let res = self.res;
        let nb = (br as usize).pow(3);
        // Phase 1: classify every brick in parallel. `BrickClass::Mixed` carries the boxed 16³ block.
        enum BrickClass { Empty, Full, Mixed(Box<[f32; BRICK_VOX]>) }
        let classes: Vec<BrickClass> = (0..nb).into_par_iter().map(|bi| {
            // Decode bx,by,bz from bi (same layout as serial: bi = bx + by·br + bz·br²).
            let bx = (bi as u32) % br;
            let by = ((bi as u32) / br) % br;
            let bz = (bi as u32) / (br * br);
            // Fast path: source region uniform → emit the tag without sampling. The 4096-voxel value
            // would be exactly `c` everywhere (proven by the caller's footprint test), so this matches
            // the full-classify result (all0 ⇔ c==0.0 → Empty; all1 ⇔ c==1.0 → Full).
            if let Some(c) = skip(bx, by, bz) {
                return if c == 0.0 { BrickClass::Empty } else { BrickClass::Full };
            }
            let mut tmp = Box::new([0.0f32; BRICK_VOX]);
            let (mut all0, mut all1) = (true, true);
            for lz in 0..BRICK {
                let z = bz * BRICK + lz; if z >= res { break; }
                for ly in 0..BRICK {
                    let y = by * BRICK + ly; if y >= res { break; }
                    for lx in 0..BRICK {
                        let x = bx * BRICK + lx; if x >= res { break; }
                        let v = f(x, y, z);
                        tmp[(lx + ly * BRICK + lz * BRICK * BRICK) as usize] = v;
                        if v != 0.0 { all0 = false; }
                        if v != 1.0 { all1 = false; }
                    }
                }
            }
            if all0 { BrickClass::Empty }
            else if all1 { BrickClass::Full }
            else { BrickClass::Mixed(tmp) }
        }).collect();
        // Phase 2: serial assembly in ascending bi order (matches serial handle assignment).
        self.bricks.clear();
        self.free.clear();
        self.gc_dirty = true; // whole grid rebuilt → re-upload everything
        // Pre-reserve the arena for exactly the Mixed bricks so `alloc_brick`'s incremental `resize`
        // never reallocates+memcpys the growing arena mid-assembly (a hidden super-linear cost at 1024³).
        let mixed = classes.iter().filter(|c| matches!(c, BrickClass::Mixed(_))).count();
        self.bricks.reserve(mixed * BRICK_VOX);
        for (bi, class) in classes.into_iter().enumerate() {
            match class {
                BrickClass::Empty => self.index[bi] = TAG_EMPTY,
                BrickClass::Full => self.index[bi] = TAG_FULL,
                BrickClass::Mixed(block) => {
                    let h = self.alloc_brick(0.0);
                    let o = h as usize * BRICK_VOX;
                    self.bricks[o..o + BRICK_VOX].copy_from_slice(&block[..]); // padding voxels never read
                    self.index[bi] = h;
                }
            }
        }
    }
}

/// Voxel grid coord → world position, standalone (so a closure can use it without borrowing the grid).
#[inline]
pub(super) fn grid_cell_world(x: u32, y: u32, z: u32, res: u32, bounds: f32) -> Vec3 {
    let s = (res - 1) as f32;
    Vec3::new(
        (x as f32 / s) * 2.0 * bounds - bounds,
        (y as f32 / s) * 2.0 * bounds - bounds,
        (z as f32 / s) * 2.0 * bounds - bounds,
    )
}

/// A cubic density grid. `res` samples per axis, spanning world `[-bounds, +bounds]`.
#[derive(Clone, Debug)]
pub struct VoxelGrid {
    pub density: DensityStorage, // 0 = empty, 1 = solid; see DensityStorage (sparse-ready boundary)
    pub res: u32,
    pub bounds: f32,
    pub iso: f32,
    pub dirty: Option<VoxelDirty>,
    /// Optional per-voxel PAINT colour, parallel to a DENSE density buffer (one `u32` per voxel,
    /// `res³` entries). Packed `0x_FF_RR_GG_BB`: the high byte is a "painted" flag (0 = never
    /// painted → the mesher emits the body material; non-zero → emit this colour). `Vec::new()`
    /// (empty) until the first paint stamp allocates it lazily — an unpainted sculpt pays nothing.
    /// DENSE-ONLY for now: the sparse (≥256³) backing + the GPU atlas/resident mesh paths do NOT
    /// carry colour yet (vertex-paint deferred there — see the task report). Not serialised (save/
    /// load drops paint colour until the persistence side-channel is extended).
    pub colors: Vec<u32>,
    /// Optional per-voxel SCULPT MASK. `0` = free to sculpt, `255` = fully protected.
    /// Dense grids: full `res³` buffer (lazy). Sparse grids leave this empty and use `mask_sparse`
    /// so ≥256³ never pays res³ bytes of zeros. Not serialized yet.
    pub mask: Vec<u8>,
    /// Sparse non-zero protect-mask cells (flat index → 0..255). Only used when density is sparse.
    /// Empty map = no mask. Migrated (both directions, dense↔sparse) on resample via
    /// `migrate_mask_into` — a resample used to silently drop the mask.
    pub mask_sparse: std::collections::HashMap<u32, u8>,
    /// When true (default), `chunk_vertex_color` darkens masked surface verts so the protect-mask
    /// is visible in the viewport (ZBrush-style black overlay). Toggle off + remesh hides the tint
    /// without clearing the mask data (sculpt protection still applies while the buffer is non-empty).
    pub show_mask: bool,
}

/// Pack a linear-RGB paint colour (each channel 0..1) into the `0x_FF_RR_GG_BB` voxel-colour word.
/// The `0xFF` high byte is the "painted" flag; channels are stored as sRGB-ish u8 (the raster shader
/// reads them back as the stored albedo, matching how `gbuf_albedo` already holds display-space rgb).
#[inline]
pub fn pack_voxel_color(rgb: Vec3) -> u32 {
    let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    0xFF00_0000 | (q(rgb.x) << 16) | (q(rgb.y) << 8) | q(rgb.z)
}

/// At/above this resolution a new grid uses the SPARSE (brick) backing — dense plateaus (192³ = 28 MB,
/// 256³ = 67 MB, 512³ = 536 MB) make the resident grid + the 134 MB undo mask + the GPU upload buffer
/// untenable, while a sculpted shell touches a fraction of the bricks. Below it, dense is simpler and
/// the memory is trivial. The runtime switch (vs a compile-time generic) lets small grids stay dense.
pub const SPARSE_RES_THRESHOLD: u32 = 256;

/// Compact, serde-friendly snapshot of a `VoxelGrid`'s density (P0.5 persistence side-channel). The live
/// runtime types derive only `Clone` (no serde), and `BrickGrid` carries GPU-sync bookkeeping that must
/// never round-trip (D4) — so this stores only field VALUES (the dense buffer, or sparse occupied-brick
/// tags + data). `VoxelGrid::from_record` reconstructs a value-equal grid.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum VoxelGridPayload {
    Dense(Vec<f32>),
    Sparse { tags: Vec<u32>, bricks: Vec<f32> },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoxelGridRecord {
    pub res: u32,
    pub bounds: f32,
    pub iso: f32,
    pub payload: VoxelGridPayload,
}

#[cfg(test)]
mod voxel_record_tests {
    use super::*;
    #[test]
    fn voxelgrid_record_roundtrip_dense() {
        let mut g = VoxelGrid::new_empty(64, 2.0); // <256 → dense
        assert!(!g.density.is_sparse());
        g.density.set_idx(100, 0.7);
        g.density.set_idx(2000, 1.0);
        g.density.set_idx(50_000, 0.3);
        let g2 = VoxelGrid::from_record(&g.to_record());
        assert_eq!((g2.res, g2.bounds, g2.iso), (g.res, g.bounds, g.iso));
        assert_eq!(g.density, g2.density); // PartialEq by value
    }
    #[test]
    fn voxelgrid_record_roundtrip_sparse() {
        let mut g = VoxelGrid::new_empty(256, 2.0); // >=256 → sparse
        assert!(g.density.is_sparse());
        let r = 256usize;
        // voxels in three different bricks (force allocations)
        g.density.set_idx(1 + r + r * r, 1.0);
        g.density.set_idx(40 + 40 * r + 40 * r * r, 0.6);
        g.density.set_idx(200 + 200 * r + 200 * r * r, 0.9);
        let rec = g.to_record();
        if let VoxelGridPayload::Sparse { bricks, .. } = &rec.payload {
            assert!(!bricks.is_empty(), "sparse test must actually allocate bricks");
        } else {
            panic!("256³ grid should be sparse");
        }
        let g2 = VoxelGrid::from_record(&rec);
        assert_eq!((g2.res, g2.bounds, g2.iso), (g.res, g.bounds, g.iso));
        assert_eq!(g.density, g2.density);
    }
}

impl VoxelGrid {
    pub fn new_empty(res: u32, bounds: f32) -> Self {
        let res = res.max(2);
        let density = if res >= SPARSE_RES_THRESHOLD {
            DensityStorage::sparse(res, 0.0)
        } else {
            DensityStorage::dense(res, 0.0)
        };
        Self::with_density(res, bounds, density)
    }

    /// `new_empty` with DENSE storage regardless of [`SPARSE_RES_THRESHOLD`]. For TRANSIENT
    /// grids that are written once and read everywhere immediately (the poly Remesh
    /// modifier's voxelize→extract round trip): the sparse threshold trades read speed for
    /// resident memory, which is the wrong trade for a short-lived grid — brick indirection
    /// on the ~10⁸ density reads of a 256³ Surface-Nets extract dominates, while 256³ dense
    /// is a 67 MB allocation that lives for one eval. Persistent sculpt grids keep
    /// `new_empty`'s auto-selection.
    pub fn new_empty_dense(res: u32, bounds: f32) -> Self {
        let res = res.max(2);
        Self::with_density(res, bounds, DensityStorage::dense(res, 0.0))
    }

    fn with_density(res: u32, bounds: f32, density: DensityStorage) -> Self {
        Self {
            density,
            res,
            bounds,
            iso: 0.5,
            dirty: Some(VoxelDirty { min: [0, 0, 0], max: [res - 1, res - 1, res - 1] }),
            colors: Vec::new(),
            mask: Vec::new(),
            mask_sparse: std::collections::HashMap::new(),
            show_mask: true,
        }
    }

    /// Snapshot density → a compact serde record (P0.5 save side-channel). Dense grids store the flat
    /// buffer; sparse grids store only occupied bricks (a 512³ solid is a few tags — near-zero).
    pub fn to_record(&self) -> VoxelGridRecord {
        let payload = match &self.density {
            DensityStorage::Dense { d, .. } => VoxelGridPayload::Dense(d.clone()),
            DensityStorage::Sparse(b) => {
                let (tags, bricks) = b.to_parts();
                VoxelGridPayload::Sparse { tags, bricks }
            }
        };
        VoxelGridRecord { res: self.res, bounds: self.bounds, iso: self.iso, payload }
    }

    /// Reconstruct a grid from a record (P0.5 load). Value-equal to the saved grid (`PartialEq` via
    /// `DensityStorage::iter`); GPU-sync state is fresh and the whole grid is marked dirty.
    pub fn from_record(r: &VoxelGridRecord) -> VoxelGrid {
        let res = r.res.max(2);
        // Validate before constructing — a truncated / hand-edited / version-skewed file could otherwise
        // drive from_parts / idx() out of bounds (hard panic, lost session). Invalid → empty grid (data
        // lost, but the load survives). Valid records (round-trip from to_record) always take a real arm.
        let density = match &r.payload {
            VoxelGridPayload::Dense(d) if d.len() == (res as usize).pow(3) =>
                DensityStorage::Dense { d: d.clone(), res },
            VoxelGridPayload::Sparse { tags, bricks } if Self::sparse_record_valid(res, tags, bricks) =>
                DensityStorage::Sparse(BrickGrid::from_parts(res, tags.clone(), bricks.clone())),
            _ => {
                debug_assert!(false, "from_record: malformed VoxelGridRecord (res={res}) — loading empty grid");
                if res >= SPARSE_RES_THRESHOLD { DensityStorage::sparse(res, 0.0) } else { DensityStorage::dense(res, 0.0) }
            }
        };
        VoxelGrid {
            density, res, bounds: r.bounds, iso: r.iso,
            dirty: Some(VoxelDirty { min: [0, 0, 0], max: [res - 1, res - 1, res - 1] }),
            colors: Vec::new(),
            mask: Vec::new(),
            mask_sparse: std::collections::HashMap::new(),
            show_mask: true,
        }
    }

    /// Validate a Sparse `VoxelGridRecord` before reconstruction (corrupt/truncated-file guard): `tags`
    /// must be exactly brick_res³, `bricks` a whole number of 4096-float bricks, and every allocated
    /// handle (a small sequential index, not EMPTY/FULL) must reference an existing brick.
    fn sparse_record_valid(res: u32, tags: &[u32], bricks: &[f32]) -> bool {
        let brick_res = (res + BRICK - 1) / BRICK;
        if tags.len() != (brick_res as usize).pow(3) { return false; }
        if bricks.len() % BRICK_VOX != 0 { return false; }
        let num_bricks = (bricks.len() / BRICK_VOX) as u32;
        tags.iter().all(|&t| t == TAG_EMPTY || t == TAG_FULL || t < num_bricks)
    }

    #[inline]
    pub fn voxel_size(&self) -> f32 {
        (self.bounds * 2.0) / (self.res - 1) as f32
    }

    #[inline]
    pub(super) fn idx(&self, x: u32, y: u32, z: u32) -> usize {
        (x + y * self.res + z * self.res * self.res) as usize
    }

    /// Public flat index for cell `(x,y,z)` — `x + y*res + z*res²`, the layout every density read/write
    /// uses. Mirrors the private `idx`; exposed for callers that record an undo delta keyed by flat index
    /// (e.g. the MOVE tool's Phase-2 mesh→SDF re-voxelize).
    #[inline]
    pub fn flat_index(&self, x: u32, y: u32, z: u32) -> usize {
        (x + y * self.res + z * self.res * self.res) as usize
    }

    #[inline]
    pub fn cell_world_pos(&self, x: u32, y: u32, z: u32) -> Vec3 {
        let b = self.bounds;
        let s = (self.res - 1) as f32;
        Vec3::new(
            (x as f32 / s) * 2.0 * b - b,
            (y as f32 / s) * 2.0 * b - b,
            (z as f32 / s) * 2.0 * b - b,
        )
    }

    /// World position → fractional grid coordinate. Inverse of `cell_world_pos`.
    #[inline]
    pub fn world_to_grid(&self, p: Vec3) -> Vec3 {
        let b = self.bounds;
        let s = (self.res - 1) as f32;
        ((p + Vec3::splat(b)) / (2.0 * b)) * s
    }

    #[inline]
    fn at(&self, x: i32, y: i32, z: i32) -> f32 {
        self.density.at_i(x, y, z)
    }

    /// Trilinear density sample at a world position.
    pub fn sample(&self, p: Vec3) -> f32 {
        let g = self.world_to_grid(p);
        let bx = g.x.floor() as i32;
        let by = g.y.floor() as i32;
        let bz = g.z.floor() as i32;
        let fx = g.x - bx as f32;
        let fy = g.y - by as f32;
        let fz = g.z - bz as f32;
        let c000 = self.at(bx, by, bz);
        let c100 = self.at(bx + 1, by, bz);
        let c010 = self.at(bx, by + 1, bz);
        let c110 = self.at(bx + 1, by + 1, bz);
        let c001 = self.at(bx, by, bz + 1);
        let c101 = self.at(bx + 1, by, bz + 1);
        let c011 = self.at(bx, by + 1, bz + 1);
        let c111 = self.at(bx + 1, by + 1, bz + 1);
        let x00 = c000 * (1.0 - fx) + c100 * fx;
        let x10 = c010 * (1.0 - fx) + c110 * fx;
        let x01 = c001 * (1.0 - fx) + c101 * fx;
        let x11 = c011 * (1.0 - fx) + c111 * fx;
        let y0 = x00 * (1.0 - fy) + x10 * fy;
        let y1 = x01 * (1.0 - fy) + x11 * fy;
        y0 * (1.0 - fz) + y1 * fz
    }

    pub(super) fn mark_dirty(&mut self, min: [u32; 3], max: [u32; 3]) {
        self.dirty = Some(match self.dirty {
            None => VoxelDirty { min, max },
            Some(d) => VoxelDirty {
                min: [d.min[0].min(min[0]), d.min[1].min(min[1]), d.min[2].min(min[2])],
                max: [d.max[0].max(max[0]), d.max[1].max(max[1]), d.max[2].max(max[2])],
            },
        });
    }

    pub fn take_dirty(&mut self) -> Option<VoxelDirty> { self.dirty.take() }

    // ------------------------------------------------------------------
    // Base primitives — a starting lump of material to sculpt.
    // ------------------------------------------------------------------

    pub fn clear(&mut self) {
        self.density.fill(0.0);
        let r = self.res - 1;
        self.mark_dirty([0, 0, 0], [r, r, r]);
    }

    /// Anti-alias band for solid prim fills (sphere/box/cylinder), in voxels.
    /// ~2.0 keeps Surface Nets off cell stairsteps without bloating the iso band.
    /// Wider (2.25+) + wide normal stencils caused matcap latitude rings / equator seams.
    const FILL_AA_VOXELS: f32 = 2.0;

    /// Fill a solid sphere of material. Density is anti-aliased over ~2 voxels at the
    /// boundary (smoothstep), so Surface Nets produces a smooth surface (not a voxel ball).
    pub fn fill_sphere(&mut self, center: Vec3, radius: f32) {
        let (vs, res, bounds) = (self.voxel_size(), self.res, self.bounds);
        let aa = (vs * Self::FILL_AA_VOXELS).max(1e-6);
        // Brick-aware fill: solid interior bricks become FULL tags directly (no transient allocation).
        self.density.fill_from(|x, y, z| {
            let p = grid_cell_world(x, y, z, res, bounds);
            let d = (p - center).length() - radius; // signed dist to sphere (for AA only)
            smoothstep01(-d / aa) // 1 inside, 0 outside
        });
        let r = self.res - 1;
        self.mark_dirty([0, 0, 0], [r, r, r]);
    }

    /// Fill a solid box of material.
    pub fn fill_box(&mut self, center: Vec3, half: Vec3) {
        let (vs, res, bounds) = (self.voxel_size(), self.res, self.bounds);
        let aa = (vs * Self::FILL_AA_VOXELS).max(1e-6);
        self.density.fill_from(|x, y, z| {
            let p = grid_cell_world(x, y, z, res, bounds) - center;
            let q = p.abs() - half;
            let d = q.max(Vec3::ZERO).length() + q.max_element().min(0.0);
            smoothstep01(-d / aa)
        });
        let r = self.res - 1;
        self.mark_dirty([0, 0, 0], [r, r, r]);
    }

    /// Fill a solid Y-axis cylinder (capped). `radius` is radial extent, `half_height` along Y.
    /// Density is anti-aliased over ~1.75 voxels at the boundary (same as sphere/box).
    pub fn fill_cylinder(&mut self, center: Vec3, radius: f32, half_height: f32) {
        let (vs, res, bounds) = (self.voxel_size(), self.res, self.bounds);
        let aa = (vs * Self::FILL_AA_VOXELS).max(1e-6);
        let radius = radius.max(1e-4);
        let half_height = half_height.max(1e-4);
        self.density.fill_from(|x, y, z| {
            let p = grid_cell_world(x, y, z, res, bounds) - center;
            // 2D disc in XZ + slab in Y (standard SDF cylinder).
            let radial = (p.x * p.x + p.z * p.z).sqrt() - radius;
            let axial = p.y.abs() - half_height;
            let d = radial.max(0.0).hypot(axial.max(0.0)) + radial.max(axial).min(0.0);
            smoothstep01(-d / aa)
        });
        let r = self.res - 1;
        self.mark_dirty([0, 0, 0], [r, r, r]);
    }

    /// Resample the density field into a NEW grid at `(new_res, new_bounds)` — the GROW variant of
    /// `resample`. Unlike `resample` (which keeps `bounds` and only changes resolution), this ALSO changes
    /// the world extent, so the existing field occupies the CENTRAL sub-region of a larger grid and new
    /// cells outside the old `[-bounds, bounds]` extent are EMPTY (density 0). The source field is sampled
    /// trilinearly at each new cell's world position; `sample` reads 0 for any world position outside the
    /// old extent (`DensityStorage::at_i` returns 0 out of range), so the empty margin falls out for free —
    /// no special-casing. `new_bounds == self.bounds` reduces EXACTLY to `resample` (same cell world
    /// positions, same samples). The world SHAPE is preserved: a voxel at world `p` keeps its density.
    ///
    /// Used by the voxel Move tool's dynamic grid GROW: when a Move would clamp at the sealed border, the
    /// grid is regridded to `(new_res, new_bounds)` so the displaced surface fits with margin instead of
    /// filling to a cube.
    pub fn resample_to(&self, new_res: u32, new_bounds: f32) -> VoxelGrid {
        let new_res = new_res.clamp(8, 1024);
        let new_bounds = new_bounds.max(1e-4);
        let mut g = VoxelGrid::new_empty(new_res, new_bounds);
        g.iso = self.iso;
        let (gb, gr) = (g.bounds, g.res);
        // Sample the SOURCE field at each NEW cell's world position. `self.sample` returns 0 outside the
        // old extent → the larger grid's outer cells are empty (the grow margin) automatically.
        let f = |x, y, z| self.sample(grid_cell_world(x, y, z, gr, gb)).clamp(0.0, 1.0);
        if self.density.is_sparse() {
            // SPARSE-AWARE fast path (mirrors `resample`): a destination brick whose SOURCE footprint is
            // uniform emits its tag from a few index reads instead of 4096 trilinear samples. The footprint
            // `map` reproduces the SAME two-op expression `f` uses — grid_cell_world (NEW bounds) then the
            // source `world_to_grid` (OLD bounds) — so the floor() boundaries match `sample` exactly. With
            // the bounds change a NEW edge brick can map ENTIRELY outside the old grid; `region_const` over
            // a fully-out-of-range span returns the OOB constant (0 = EMPTY), so those bricks stay empty.
            let old_b = self.bounds;
            let old_s = (self.res - 1) as f32;
            let new_s = (gr - 1) as f32;
            let map = move |xv: u32| -> f32 {
                let world = (xv as f32 / new_s) * 2.0 * gb - gb; // grid_cell_world, NEW bounds (one axis)
                ((world + old_b) / (2.0 * old_b)) * old_s        // source world_to_grid, OLD bounds (one axis)
            };
            let foot = move |b: u32| -> (i32, i32) {
                let v0 = b * BRICK;
                let v1 = (b * BRICK + (BRICK - 1)).min(new_res - 1); // dest brick's last in-grid voxel
                (map(v0).floor() as i32, map(v1).floor() as i32 + 1) // [lo .. hi]; +1 = trilinear neighbor
            };
            let skip = move |bx: u32, by: u32, bz: u32| -> Option<f32> {
                let (sx0, sx1) = foot(bx);
                let (sy0, sy1) = foot(by);
                let (sz0, sz1) = foot(bz);
                self.density.region_const([sx0, sy0, sz0, sx1, sy1, sz1])
            };
            g.density.par_fill_from_skip(f, skip);
        } else {
            // Dense source (<256³): sample every voxel (the Move grow is always this path).
            g.density.par_fill_from(f);
        }
        let r = new_res - 1;
        g.mark_dirty([0, 0, 0], [r, r, r]);
        self.migrate_mask_into(&mut g);
        g
    }

    /// Resample the density field into a NEW grid at `new_res` over the SAME bounds,
    /// trilinearly sampling the (smooth) source field — the resolution / remesh control.
    /// Higher res = smoother surface + more detail capacity; lower res = lighter mesh.
    /// The iso surface is preserved because we sample the continuous field, not nearest voxel.
    pub fn resample(&self, new_res: u32) -> VoxelGrid {
        let new_res = new_res.clamp(8, 1024);
        let mut g = VoxelGrid::new_empty(new_res, self.bounds);
        g.iso = self.iso;
        let (gb, gr) = (g.bounds, g.res);
        // Brick-aware resample: the new Sparse grid's solid interior becomes FULL tags directly, so a
        // 512³ resample never transiently allocates the whole arena. `self` (source) is read by sample.
        // PARALLEL: ~1B trilinear samples at 1024³ are spread across all cores. The closure only READS
        // the immutable source `self`, so it is `Sync`; output is byte-identical to the serial fill.
        let f = |x, y, z| self.sample(grid_cell_world(x, y, z, gr, gb)).clamp(0.0, 1.0);
        if self.density.is_sparse() {
            // SPARSE-AWARE fast path: the source is mostly uniform (EMPTY exterior / FULL interior); only
            // the surface shell is mixed. For a destination brick whose SOURCE footprint is uniform, emit
            // its tag from a handful of index-tag reads instead of 4096 trilinear samples (each 8 scattered
            // arena loads) → cost tracks the SURFACE, not the volume, and the cache thrash vanishes.
            //
            // The footprint must be BYTE-EXACT: `map` reproduces the SAME two-op float expression `f`
            // evaluates per axis — grid_cell_world (new) then world_to_grid (old) — NOT pre-simplified to
            // xv·(old-1)/(new-1) (algebraically equal but can floor() differently at edges). `sample`
            // floors g and reads voxels `floor(g)` and `floor(g)+1`, so the source-voxel span a dest brick
            // reads on each axis is [floor(map(first_voxel)) .. floor(map(last_voxel)) + 1] — the `+1` is
            // the trilinear neighbor and is MANDATORY (dropping it would skip a brick whose far corner
            // reads into an adjacent mixed/empty brick). `region_const` over that span is uniform ⟺ every
            // corner `sample` reads is the constant ⟺ the per-voxel result is that constant everywhere.
            let old_s = (self.res - 1) as f32;
            let new_s = (gr - 1) as f32;
            let map = move |xv: u32| -> f32 {
                let world = (xv as f32 / new_s) * 2.0 * gb - gb; // grid_cell_world (one axis)
                ((world + gb) / (2.0 * gb)) * old_s              // world_to_grid (one axis)
            };
            let foot = move |b: u32| -> (i32, i32) {
                let v0 = b * BRICK;
                let v1 = (b * BRICK + (BRICK - 1)).min(new_res - 1); // dest brick's last in-grid voxel
                (map(v0).floor() as i32, map(v1).floor() as i32 + 1) // [lo .. hi]; +1 = trilinear neighbor
            };
            let skip = move |bx: u32, by: u32, bz: u32| -> Option<f32> {
                let (sx0, sx1) = foot(bx);
                let (sy0, sy1) = foot(by);
                let (sz0, sz1) = foot(bz);
                self.density.region_const([sx0, sy0, sz0, sx1, sy1, sz1])
            };
            g.density.par_fill_from_skip(f, skip);
        } else {
            // Dense source (<256³): no brick tags to test → sample every voxel (unchanged).
            g.density.par_fill_from(f);
        }
        let r = new_res - 1;
        g.mark_dirty([0, 0, 0], [r, r, r]);
        self.migrate_mask_into(&mut g);
        g
    }

    /// Migrate the protect-mask into a freshly resampled grid `g` — the mask counterpart of the
    /// density migration in [`Self::resample`] / [`Self::resample_to`] (world-shape preserving:
    /// a masked world position stays masked at the new resolution/bounds). Before this, a
    /// resample silently DROPPED the mask — and a dense↔sparse conversion left the OLD storage
    /// (`mask` vs `mask_sparse`) populated but dead, since `mask_factor_at` branches on
    /// `density.is_sparse()` (voxel_sculpt_split_findings.md).
    ///
    /// DEST-driven nearest-voxel sampling (upsampling never leaves holes), restricted to the
    /// image of the source's non-zero-mask AABB so cost tracks the masked region, not `res³`.
    /// Writes to whichever storage the DEST density uses: dense → lazy `res³` buffer, sparse →
    /// `mask_sparse` entries for non-zero cells only. `show_mask` carries over.
    fn migrate_mask_into(&self, g: &mut VoxelGrid) {
        g.show_mask = self.show_mask;
        if !self.has_mask() { return; }
        let (old_res, old_b) = (self.res, self.bounds);
        // Source non-zero AABB (dense: scan the buffer — dense is <256³; sparse: iterate keys).
        let (mut lo, mut hi) = ([u32::MAX; 3], [0u32; 3]);
        let mut expand = |i: u32| {
            let (x, y, z) = (i % old_res, (i / old_res) % old_res, i / (old_res * old_res));
            lo = [lo[0].min(x), lo[1].min(y), lo[2].min(z)];
            hi = [hi[0].max(x), hi[1].max(y), hi[2].max(z)];
        };
        if self.density.is_sparse() {
            for (&i, &m) in &self.mask_sparse { if m != 0 { expand(i); } }
        } else {
            for (i, &m) in self.mask.iter().enumerate() { if m != 0 { expand(i as u32); } }
        }
        if lo[0] == u32::MAX { return; } // stored mask is all zeros
        // Source cell → dest cell mapping (world-preserving, same convention as the density `f`):
        // world = grid_cell_world(src) then dest world_to_grid — per axis.
        let old_s = (old_res - 1) as f32;
        let new_s = (g.res - 1) as f32;
        let (gb, gr) = (g.bounds, g.res);
        let to_dest = |v: u32| -> f32 {
            let world = (v as f32 / old_s) * 2.0 * old_b - old_b; // grid_cell_world, OLD bounds
            ((world + gb) / (2.0 * gb)) * new_s                   // dest world_to_grid, NEW bounds
        };
        let to_src = |v: u32| -> i32 {
            let world = (v as f32 / new_s) * 2.0 * gb - gb;       // grid_cell_world, NEW bounds
            (((world + old_b) / (2.0 * old_b)) * old_s).round() as i32 // nearest source voxel
        };
        // Dest AABB = image of the source AABB, ±1 for rounding, clamped in-grid.
        let d0 = |a: u32| (to_dest(a).floor() as i32 - 1).max(0) as u32;
        let d1 = |a: u32| ((to_dest(a).ceil() as i32 + 1).max(0) as u32).min(gr - 1);
        let (x0, y0, z0) = (d0(lo[0]), d0(lo[1]), d0(lo[2]));
        let (x1, y1, z1) = (d1(hi[0]), d1(hi[1]), d1(hi[2]));
        let src_at = |xi: i32, yi: i32, zi: i32| -> u8 {
            if xi < 0 || yi < 0 || zi < 0 { return 0; }
            let (x, y, z) = (xi as u32, yi as u32, zi as u32);
            if x >= old_res || y >= old_res || z >= old_res { return 0; }
            let i = (x + y * old_res + z * old_res * old_res) as usize;
            if self.density.is_sparse() {
                self.mask_sparse.get(&(i as u32)).copied().unwrap_or(0)
            } else {
                self.mask.get(i).copied().unwrap_or(0)
            }
        };
        let dest_sparse = g.density.is_sparse();
        if !dest_sparse && g.mask.len() != (gr as usize).pow(3) {
            g.mask = vec![0u8; (gr as usize).pow(3)]; // lazy alloc, same as deposit_mask_sphere
        }
        for z in z0..=z1 { for y in y0..=y1 { for x in x0..=x1 {
            let m = src_at(to_src(x), to_src(y), to_src(z));
            if m == 0 { continue; }
            let i = g.idx(x, y, z);
            if dest_sparse { g.mask_sparse.insert(i as u32, m); } else { g.mask[i] = m; }
        }}}
    }
}

impl VoxelGrid {
    // ------------------------------------------------------------------
    // Picking — march a ray and return the first iso crossing (entering solid).
    // ------------------------------------------------------------------
    pub fn raycast(&self, ro: Vec3, rd: Vec3) -> Option<Vec3> {
        let vs = self.voxel_size();
        let step = vs * 0.5;
        let max_t = 4.0 * self.bounds;
        let mut t = 0.0;
        let mut prev = self.sample(ro);
        while t < max_t {
            t += step;
            let p = ro + rd * t;
            let cur = self.sample(p);
            if prev < self.iso && cur >= self.iso {
                // refine by bisection along the last segment
                let mut a = t - step; let mut b = t;
                for _ in 0..8 {
                    let m = 0.5 * (a + b);
                    if self.sample(ro + rd * m) >= self.iso { b = m; } else { a = m; }
                }
                return Some(ro + rd * (0.5 * (a + b)));
            }
            prev = cur;
        }
        None
    }

    /// Outward surface normal at a world point (density decreases outward).
    pub fn normal_at(&self, p: Vec3) -> Vec3 {
        self.gradient_normal(p, self.voxel_size())
    }

    /// Re-project a point near the surface back onto the isosurface along the local gradient
    /// (Blender stroke model: every dab rides the CURRENT surface). The stroke interpolator
    /// emits dabs on the world-space CHORD between two surface hits — across curvature the
    /// chord cuts inside (or floats above) the skin; this pulls each dab back to the iso.
    /// Walks along the outward normal (out of solid / into empty toward the surface) up to
    /// `max_dist`, then bisects the crossing. `hint` = fallback outward direction for points
    /// where the density gradient is ZERO (the field saturates a couple of voxels off the
    /// shell — exactly where deep chord dabs land); callers pass the running stroke normal.
    /// `None` = no crossing within range (silhouette / off-surface dab — caller keeps the
    /// chord position).
    pub fn snap_to_surface(&self, p: Vec3, hint: Vec3, max_dist: f32) -> Option<Vec3> {
        let mut n = self.normal_at(p);
        if n.length_squared() < 1e-12 {
            n = hint;
        }
        if n.length_squared() < 1e-12 {
            return None;
        }
        let n = n.normalize_or_zero();
        let step = self.voxel_size() * 0.5;
        let v0 = self.sample(p);
        let inside0 = v0 >= self.iso;
        // Inside solid → walk OUT along +n; outside → walk IN along −n. Both end at the iso.
        let dir = if inside0 { n } else { -n };
        let mut t = 0.0f32;
        let mut prev_inside = inside0;
        while t < max_dist {
            t += step;
            let cur_inside = self.sample(p + dir * t) >= self.iso;
            if cur_inside != prev_inside {
                // Bisect the crossing segment [t-step, t] (same refinement as `raycast`).
                let (mut a, mut b) = (t - step, t);
                for _ in 0..8 {
                    let m = 0.5 * (a + b);
                    if (self.sample(p + dir * m) >= self.iso) == inside0 {
                        a = m;
                    } else {
                        b = m;
                    }
                }
                return Some(p + dir * (0.5 * (a + b)));
            }
            prev_inside = cur_inside;
        }
        None
    }

    /// **Area / Average Normal** (Blender Area Plane, ZBrush / 3D-Coat average under brush).
    /// Full quality (3 rings × 8 probes). Prefer [`sample_area_normal_fast`] mid-stroke.
    pub fn sample_area_normal(&self, center: Vec3, brush_radius: f32, region: f32) -> Vec3 {
        self.sample_area_normal_quality(center, brush_radius, region, false)
    }

    /// Fast mid-stroke area normal: 1 ring × 6 probes with point normals (not wide gradients).
    /// Keels the same feel as full quality for plane knobs without the ~24× gradient cost.
    pub fn sample_area_normal_fast(&self, center: Vec3, brush_radius: f32, region: f32) -> Vec3 {
        self.sample_area_normal_quality(center, brush_radius, region, true)
    }

    /// `fast` = mid-stroke: 1 ring / 6 angles / `normal_at`. Full = 3 rings / 8 angles / wider gradient.
    pub fn sample_area_normal_quality(
        &self,
        center: Vec3,
        brush_radius: f32,
        region: f32,
        fast: bool,
    ) -> Vec3 {
        let n0 = self.normal_at(center);
        if n0.length_squared() < 1e-12 {
            return Vec3::Y;
        }
        let r = (brush_radius.max(1e-6) * region.clamp(0.25, 4.0)).max(self.voxel_size() * 2.0);
        let (t, b) = brush_tip_basis(n0);
        let e = if fast {
            self.voxel_size()
        } else {
            (self.voxel_size() * (1.0 + region * 0.35)).max(self.voxel_size())
        };
        let mut sum = n0;
        let mut count = 1.0f32;
        let rings: &[f32] = if fast { &[0.75] } else { &[0.35, 0.70, 1.0] };
        let n_ang: i32 = if fast { 6 } else { 8 };
        for &ring in rings {
            let rr = r * ring;
            for k in 0..n_ang {
                let a = (k as f32) * (std::f32::consts::TAU / n_ang as f32);
                let p = center + t * (a.cos() * rr) + b * (a.sin() * rr);
                let n = if fast {
                    self.normal_at(p)
                } else {
                    self.gradient_normal(p, e)
                };
                if n.length_squared() > 1e-12 {
                    let n = if n.dot(n0) < 0.0 { -n } else { n };
                    sum += n;
                    count += 1.0;
                }
            }
        }
        let avg = (sum / count).normalize_or_zero();
        if avg.length_squared() > 1e-12 { avg } else { n0 }
    }

    /// Outward normal from the density gradient with an explicit sample radius `e`.
    /// A wider `e` averages over more voxels → smoother shading (used by the mesher, where a
    /// one-voxel stencil re-facets from trilinear axis-bias); a tight `e` is sharper (picking).
    pub fn gradient_normal(&self, p: Vec3, e: f32) -> Vec3 {
        let g = Vec3::new(
            self.sample(p + Vec3::X * e) - self.sample(p - Vec3::X * e),
            self.sample(p + Vec3::Y * e) - self.sample(p - Vec3::Y * e),
            self.sample(p + Vec3::Z * e) - self.sample(p - Vec3::Z * e),
        );
        (-g).normalize_or_zero()
    }

    // ------------------------------------------------------------------
    // Surface Nets — watertight quad mesh at the iso level.
    // ------------------------------------------------------------------
    /// Extract the iso surface. `smooth=true` runs the (heavier) relaxation + wide-stencil
    /// normal pass for a polished result — use it for one-shot extracts (create, remesh,
    /// stroke release, duplicate). `smooth=false` skips it for the cheap per-frame re-extract
    /// during an active sculpt drag, so high-res grids don't hitch (Frame Budget Rule).
    ///
    /// PARALLEL internals (2026-08-08, Remesh-modifier live-edit path): rayon over z-slabs
    /// for both the cell-vertex pass and the quad pass. Output is BIT-IDENTICAL to the
    /// serial reference — indexed `collect()` preserves slab order, slabs iterate (cy,cx)
    /// like the serial loops, and the serial→parallel equality is test-gated by
    /// `surface_nets_parallel_matches_serial_reference` (tests.rs). 128³ sphere: 37.6ms → ~5ms.
    pub fn surface_nets(&self, smooth: bool) -> Mesh {
        self.surface_nets_placed(smooth, CellPlacement::Centroid)
    }

    /// [`Self::surface_nets`] with explicit vertex placement.
    ///
    /// `Centroid` is the historical Surface-Nets behaviour and is BIT-IDENTICAL to what
    /// `surface_nets` always produced — the sculpt display path is untouched. `Qef` is Dual
    /// Contouring for the poly Remesh modifier, where the crossing centroid's systematic pull
    /// toward the cell centre is what reads as terracing on a curved surface.
    pub fn surface_nets_placed(&self, smooth: bool, placement: CellPlacement) -> Mesh {
        self.surface_nets_impl(smooth, placement, None)
    }

    /// [`Self::surface_nets_placed`] with a retained **Phase A** cache — the regional extract.
    ///
    /// Phase A (the per-cell QEF solve) is the only part of this function whose work is local to a
    /// cell; the serial stitch, quad emission and `relax_mesh` are all coupled through GLOBAL
    /// vertex numbering and must keep running whole-grid (see §12d of
    /// `docs/planning/poly/REMESH_ADAPTIVE_OCTREE_PLAN.md`). So the cache unit is the **cz slab**,
    /// which is exactly the shape Phase A already emits: reusing a slab preserves emission order
    /// by construction, so vertex numbering is identical to a full extract.
    ///
    /// `dirty` is in **grid POINT** coordinates (the same box `write_region_from_sdf_ex` was given).
    /// Output is bit-identical to `surface_nets_placed` — gated by
    /// `cached_extract_matches_full_extract`.
    pub fn surface_nets_placed_cached(
        &self,
        smooth: bool,
        placement: CellPlacement,
        cache: &mut SurfaceNetsCache,
        dirty: DirtyCells,
    ) -> Mesh {
        self.surface_nets_impl(smooth, placement, Some((cache, dirty)))
    }

    fn surface_nets_impl(
        &self,
        smooth: bool,
        placement: CellPlacement,
        cached: Option<(&mut SurfaceNetsCache, DirtyCells)>,
    ) -> Mesh {
        use rayon::prelude::*;
        let (mut cache_ref, dirty_in) = match cached {
            Some((c, d)) => (Some(c), d),
            None => (None, DirtyCells::All),
        };
        let n = self.res;
        let cells = (n - 1) as usize;
        let iso = self.iso;

        // SURFACE-CHUNK SKIP (2026-08-08): both passes below are O(res³) in cell visits even
        // though the surface occupies a thin shell — at 256³ that was 55ms for ~0.2% useful
        // work. `surface_chunk_mask` (the same mask the display chunk mesher uses, already
        // 1-chunk dilated and neighbour-aware, so it can never hide a boundary cell whose
        // corners straddle two chunks) lets both passes skip whole 16³ blocks.
        // Output is UNCHANGED, not approximated: a skipped chunk provably contains no cell
        // that emits a vertex, and every quad references its own `(x,y,z)` cell — so a quad
        // can only exist where that cell has a vertex, i.e. inside a masked chunk. Iteration
        // order is untouched, so vertex numbering still matches the serial reference exactly
        // (gate: `surface_nets_parallel_matches_serial_reference`, LCG-speckled grids).
        const SN_CHUNK: u32 = 16; // matches app VOXEL_CHUNK / object_display_mesh
        let cd = (((cells as u32 + SN_CHUNK - 1) / SN_CHUNK).max(1)) as usize;
        let mask = self.surface_chunk_mask(SN_CHUNK);
        let cs = SN_CHUNK as usize;
        let live = |cx: usize, cy: usize, cz: usize| -> bool {
            let ci = (cx / cs) + (cy / cs) * cd + (cz / cs) * cd * cd;
            mask.get(ci).copied().unwrap_or(true)
        };

        // ── Phase A: cell vertices, parallel per cz-slab ────────────────────
        // Each slab emits (plane-local cell index, pos, nrm) in (cy,cx) order; the serial
        // stitch below assigns global vertex indices in slab order → same numbering as the
        // serial reference. The plane-local index is resolution-only — it does NOT depend on
        // `chunk_slot`, which renumbers whenever the live set changes — so a retained slab stays
        // addressable across frames.
        let compute_slab = |cz: usize| -> Vec<(u32, Vec3, Vec3)> {
                let mut out: Vec<(u32, Vec3, Vec3)> = Vec::new();
                for cy in 0..cells {
                    // Hoist the chunk test out of the inner loop: step cx by whole chunks
                    // and skip unmasked blocks wholesale.
                    let mut cx = 0usize;
                    while cx < cells {
                        let block_end = ((cx / cs) + 1) * cs;
                        if !live(cx, cy, cz) {
                            cx = block_end;
                            continue;
                        }
                        let end = block_end.min(cells);
                        for cx in cx..end {
                            if let Some((world, nrm)) = self.cell_surface_vertex_placed(
                                cx as u32, cy as u32, cz as u32, placement,
                            ) {
                                out.push(((cx + cy * cells) as u32, world, nrm));
                            }
                        }
                        cx = end;
                    }
                }
                out
        };

        // Which cz slabs must be recomputed this frame.
        //
        // DILATION — derived, not guessed. `cell_surface_vertex_qef_d` calls
        // `gradient_normal(world, vs)` at every edge crossing with `eps` = one full voxel, whose
        // trilinear fetch reaches grid points `[cx-1, cx+3]` (`cx+3` is genuinely reachable: the
        // density map `0.5 - d/w` hits exactly `iso` at `d == 0`, i.e. `t == 1.0`). Inverting,
        // a dirty grid POINT `g` can change cells `[g-3, g+1]`; +-1 more absorbs the f32
        // round-trip through `world_to_grid ∘ cell_world_pos`. Then snap out to chunk bounds and
        // add one chunk each way, because `surface_chunk_mask`'s `need` pass reads the 6
        // neighbour chunks — a chunk can flip live/dead because something 16 cells away moved.
        let (mut slabs, recompute): (Vec<Vec<(u32, Vec3, Vec3)>>, std::ops::Range<usize>) =
            match cache_ref.as_deref_mut() {
                None => (vec![Vec::new(); cells], 0..cells),
                Some(c) => {
                    let dirty = dirty_in;
                    let reusable = c.primed
                        && c.res == n
                        && c.placement == placement
                        && c.slabs.len() == cells;
                    let prev = std::mem::take(&mut c.slabs);
                    let range = match (reusable, dirty) {
                        (false, _) | (_, DirtyCells::All) => 0..cells,
                        // Grid provably byte-identical ⇒ every slab is still valid.
                        (true, DirtyCells::None) => 0..0,
                        (true, DirtyCells::Points { lo, hi }) => {
                            let c_lo = (lo[2] as usize).saturating_sub(4);
                            let c_hi = ((hi[2] as usize) + 2).min(cells.saturating_sub(1));
                            let b0 = (c_lo / cs).saturating_sub(1) * cs;
                            let b1 = (((c_hi / cs) + 2) * cs).min(cells);
                            b0..b1
                        }
                    };
                    let base = if reusable { prev } else { vec![Vec::new(); cells] };
                    (base, range)
                }
            };
        if slabs.len() != cells {
            slabs = vec![Vec::new(); cells];
        }
        if !recompute.is_empty() {
            let fresh: Vec<(usize, Vec<(u32, Vec3, Vec3)>)> = recompute
                .clone()
                .into_par_iter()
                .map(|cz| (cz, compute_slab(cz)))
                .collect();
            for (cz, v) in fresh {
                slabs[cz] = v;
            }
        }
        let recomputed_slabs = recompute.len();
        // cell (cx,cy,cz) -> vertex index, or u32::MAX if no surface in that cell.
        //
        // CHUNK-SPARSE, not dense (2026-08-09). A dense `vec![u32::MAX; cells³]` is **66 MB at
        // 256³**, and measuring this function's phases showed the allocate-and-memset alone was
        // **13.7 ms of ~26 ms — more than half the extract** — to hold ~74k live entries. It is
        // stored per live 16³ chunk instead: a chunk the surface mask marks dead provably emits
        // no vertex (that is exactly the surface-chunk skip's invariant, since Phase A never
        // visits it), so dead chunks need no storage and read as `u32::MAX` — the same value the
        // dense table held there. Output is unchanged, and the existing
        // `surface_nets_parallel_matches_serial_reference` gate pins that.
        let chunk_count = cd * cd * cd;
        let blk = cs * cs * cs;
        let mut chunk_slot = vec![u32::MAX; chunk_count];
        let mut live_chunks = 0usize;
        for (ci, slot) in chunk_slot.iter_mut().enumerate() {
            // Same predicate as `live()` — including its `unwrap_or(true)`, so a short mask
            // stays conservative (treated live) exactly as before.
            if mask.get(ci).copied().unwrap_or(true) {
                *slot = live_chunks as u32;
                live_chunks += 1;
            }
        }
        let mut cell_vert = vec![u32::MAX; live_chunks * blk];
        // Flat slot for a cell, or `None` when its chunk is dead (⇒ no vertex, reads as MAX).
        let cv_slot = |cx: usize, cy: usize, cz: usize| -> Option<usize> {
            let ci = (cx / cs) + (cy / cs) * cd + (cz / cs) * cd * cd;
            let slot = *chunk_slot.get(ci)?;
            if slot == u32::MAX {
                return None;
            }
            Some(slot as usize * blk + (cx % cs) + (cy % cs) * cs + (cz % cs) * cs * cs)
        };
        let total: usize = slabs.iter().map(|s| s.len()).sum();
        let mut vertices: Vec<Vec3> = Vec::with_capacity(total);
        let mut normals: Vec<Vec3> = Vec::with_capacity(total);
        for (cz, slab) in slabs.iter().enumerate() {
            for &(cxy, world, nrm) in slab {
                let cx = cxy as usize % cells;
                let cy = cxy as usize / cells;
                // Phase A only visits live chunks, so this is always `Some` — but resolve it
                // rather than assume, and keep vertex numbering identical either way.
                match cv_slot(cx, cy, cz) {
                    Some(i) => cell_vert[i] = vertices.len() as u32,
                    None => debug_assert!(false, "Phase A emitted a vertex in a dead chunk"),
                }
                vertices.push(world);
                normals.push(nrm);
            }
        }

        // ── Phase B: quads, parallel per z-plane ────────────────────────────
        // For each interior grid edge with a sign change, connect the 4 cells sharing that
        // edge. Winding chosen so the normal points from solid → empty. `cell_vert` is
        // read-only here; per-plane vecs concatenate in z order → same quad order as serial.
        let cells_i = cells as i64;
        let cell_vert_ref = &cell_vert;
        let plane_quads: Vec<Vec<[u32; 4]>> = (0..n)
            .into_par_iter()
            .map(|z| {
                let mut quads: Vec<[u32; 4]> = Vec::new();
                let dens = |x: u32, y: u32, z: u32| self.density.get_idx(self.idx(x, y, z));
                let solid = |x: u32, y: u32, z: u32| dens(x, y, z) >= iso;
                // Bounds-safe cell-vertex lookup: returns u32::MAX for any out-of-range cell
                // (valid cell coords are 0..cells-1). Prevents the boundary OOB that crashed
                // when a stroke pushed a sign change into the last grid layer.
                let cv = |cx: i64, cy: i64, cz: i64| -> u32 {
                    if cx < 0 || cy < 0 || cz < 0 || cx >= cells_i || cy >= cells_i || cz >= cells_i {
                        u32::MAX
                    } else {
                        // Dead chunk ⇒ no vertex, which is the same `u32::MAX` the dense table
                        // stored there.
                        match cv_slot(cx as usize, cy as usize, cz as usize) {
                            Some(i) => cell_vert_ref[i],
                            None => u32::MAX,
                        }
                    }
                };
                // Every quad type references cv(xi,yi,zi), so a quad can only exist where
                // cell (x,y,z) itself carries a vertex — i.e. inside a masked chunk, and
                // never on the last grid plane (that cell index is out of range).
                if z as usize >= cells {
                    return quads;
                }
                for y in 0..n {
                    if y as usize >= cells {
                        continue;
                    }
                    // Chunk-stepped like the vertex pass: skipping an unmasked 16-wide block
                    // in one jump, rather than testing (and looping over) each grid point,
                    // is what actually removes the O(res³) walk.
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
                        // +X edge: shared by cells cx=x, cy∈{y-1,y}, cz∈{z-1,z}
                        if x + 1 < n {
                            let s0 = solid(x, y, z);
                            let s1 = solid(x + 1, y, z);
                            if s0 != s1 {
                                let c = [
                                    cv(xi, yi - 1, zi - 1), cv(xi, yi, zi - 1),
                                    cv(xi, yi, zi), cv(xi, yi - 1, zi),
                                ];
                                if c.iter().all(|&v| v != u32::MAX) {
                                    // s0 solid → outward = +X → this CCW order; else reverse.
                                    if s0 { quads.push([c[0], c[1], c[2], c[3]]); }
                                    else { quads.push([c[3], c[2], c[1], c[0]]); }
                                }
                            }
                        }
                        // +Y edge: shared by cells cy=y, cx∈{x-1,x}, cz∈{z-1,z}
                        if y + 1 < n {
                            let s0 = solid(x, y, z);
                            let s1 = solid(x, y + 1, z);
                            if s0 != s1 {
                                let c = [
                                    cv(xi - 1, yi, zi - 1), cv(xi - 1, yi, zi),
                                    cv(xi, yi, zi), cv(xi, yi, zi - 1),
                                ];
                                if c.iter().all(|&v| v != u32::MAX) {
                                    if s0 { quads.push([c[0], c[1], c[2], c[3]]); }
                                    else { quads.push([c[3], c[2], c[1], c[0]]); }
                                }
                            }
                        }
                        // +Z edge: shared by cells cz=z, cx∈{x-1,x}, cy∈{y-1,y}
                        if z + 1 < n {
                            let s0 = solid(x, y, z);
                            let s1 = solid(x, y, z + 1);
                            if s0 != s1 {
                                let c = [
                                    cv(xi - 1, yi - 1, zi), cv(xi, yi - 1, zi),
                                    cv(xi, yi, zi), cv(xi - 1, yi, zi),
                                ];
                                if c.iter().all(|&v| v != u32::MAX) {
                                    if s0 { quads.push([c[0], c[1], c[2], c[3]]); }
                                    else { quads.push([c[3], c[2], c[1], c[0]]); }
                                }
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

        if let Some(c) = cache_ref.as_deref_mut() {
            c.res = n;
            c.placement = placement;
            c.slabs = slabs;
            c.primed = true;
            c.recomputed_slabs = recomputed_slabs;
            c.recompute_lo = recompute.start;
            c.recompute_hi = recompute.end;
        }

        // Smoothing: relaxation + wide-stencil normals (shared with the incremental cache).
        if smooth { relax_mesh(self, &mut vertices, &mut normals, &quads); }

        Mesh { vertices, normals, quads }
    }

    /// Whole-field enclosure reconcile: 6-connected flood from the grid BORDER through AIR
    /// (density < `iso`); every air cell the flood cannot reach belongs to a SEALED VOID and
    /// is set solid (1.0). Returns how many cells were filled.
    ///
    /// This is the grid-space twin of the band sampler's `band_finalize` step, and exists
    /// because "is this air enclosed?" is a GLOBAL property: an incremental dirty-region
    /// resample rewrites only its region, so an edit that seals a cavity would otherwise
    /// leave every far cell outside that region at its stale air sign (phantom inner shell
    /// on extract). `voxel_remesh_mesh_incremental` runs this after every region splice.
    ///
    /// One-way by construction — it can fill a void but never re-open one. A caller whose
    /// field already contains a filled void must therefore NOT use incremental updates (the
    /// remesh path tracks this as `had_enclosed_void` and falls back to a full resample).
    ///
    /// Assumes the grid BORDER is air, which every remesh lattice guarantees structurally
    /// (≥2 cells of margin); a fully-solid border simply means nothing is reachable.
    pub fn seal_enclosed_air(&mut self) -> usize {
        use rayon::prelude::*;
        let n = self.res as usize;
        if n == 0 {
            return 0;
        }
        let iso = self.iso;

        // RUN-BASED CONNECTED COMPONENTS, not a cell flood (2026-08-09). Same 6-connectivity,
        // same border seeding, same output — pinned bit-for-bit by
        // `core_math/tests/seal_enclosed_air_parity.rs` against a verbatim copy of the old body.
        //
        // The old span flood chased pointers over every reachable air cell (~16.7M at 256³) and
        // cost ~24 ms even after the bitset rework. But air is overwhelmingly RUN-STRUCTURED: the
        // exterior of a remesh lattice is one or two contiguous spans per row, so the whole grid
        // is ~131k runs rather than 16.7M cells. Labelling runs instead of cells turns the flood
        // into two linear passes over a ~100× smaller problem.
        //
        // Connectivity is preserved exactly: cells inside a run are x-adjacent by construction,
        // and two runs in y±1 / z±1 neighbour rows are 6-connected iff their x ranges OVERLAP —
        // which is what the two-pointer merge below tests. No other adjacency exists in a 6-
        // connected grid.

        // ── Pass 1: air runs per row, in parallel (one linear read of the density) ──────────
        // Row id = y + z·n; runs are (x_first, x_last) inclusive and emitted in ascending x, which
        // the merge below relies on.
        let rows: Vec<Vec<(u32, u32)>> = (0..n * n)
            .into_par_iter()
            .map(|row| {
                let base = row * n;
                let mut out: Vec<(u32, u32)> = Vec::new();
                // Match the old body's Dense/Sparse split: a per-cell storage dispatch in this
                // loop is what made the naive form cost more than a full resample at 256³.
                let mut scan = |is_air: &dyn Fn(usize) -> bool| {
                    let mut x = 0usize;
                    while x < n {
                        if !is_air(base + x) {
                            x += 1;
                            continue;
                        }
                        let start = x;
                        while x + 1 < n && is_air(base + x + 1) {
                            x += 1;
                        }
                        out.push((start as u32, x as u32));
                        x += 1;
                    }
                };
                match &self.density {
                    DensityStorage::Dense { d, .. } => scan(&|i: usize| d[i] < iso),
                    DensityStorage::Sparse(_) => scan(&|i: usize| self.density.get_idx(i) < iso),
                }
                out
            })
            .collect();

        // Flatten to CSR so a row's runs are a contiguous slice: row_start[r]..row_start[r+1].
        let mut row_start: Vec<u32> = Vec::with_capacity(n * n + 1);
        let mut runs: Vec<(u32, u32)> = Vec::with_capacity(rows.iter().map(|r| r.len()).sum());
        row_start.push(0);
        for r in &rows {
            runs.extend_from_slice(r);
            row_start.push(runs.len() as u32);
        }
        drop(rows);
        if runs.is_empty() {
            return 0;
        }

        // ── Pass 2: union-find over runs. Node 0 is a virtual OUTSIDE; run i is node i+1. ─────
        let mut uf: Vec<u32> = (0..runs.len() as u32 + 1).collect();
        fn find(uf: &mut [u32], mut a: u32) -> u32 {
            while uf[a as usize] != a {
                uf[a as usize] = uf[uf[a as usize] as usize]; // path halving
                a = uf[a as usize];
            }
            a
        }
        fn union(uf: &mut [u32], a: u32, b: u32) {
            let (ra, rb) = (find(uf, a), find(uf, b));
            if ra != rb {
                uf[rb as usize] = ra;
            }
        }
        let last = (n - 1) as u32;
        for z in 0..n {
            for y in 0..n {
                let row = y + z * n;
                let (s, e) = (row_start[row] as usize, row_start[row + 1] as usize);
                // Border seeding, matching the old body's seed loop exactly: a cell is a border
                // cell when ANY axis is at 0 or n-1. So a whole row is border when its y or z is,
                // and otherwise only its x=0 / x=n-1 ends are.
                let row_on_border = y == 0 || y == last as usize || z == 0 || z == last as usize;
                for ri in s..e {
                    let (x0, x1) = runs[ri];
                    if row_on_border || x0 == 0 || x1 == last {
                        union(&mut uf, 0, ri as u32 + 1);
                    }
                }
                // Merge with the two already-visited neighbour rows. Both lists are sorted by x,
                // so one two-pointer walk finds every overlapping pair.
                let mut merge = |uf: &mut Vec<u32>, other: usize| {
                    let (os, oe) = (row_start[other] as usize, row_start[other + 1] as usize);
                    let (mut i, mut j) = (s, os);
                    while i < e && j < oe {
                        let (a0, a1) = runs[i];
                        let (b0, b1) = runs[j];
                        if a0 <= b1 && b0 <= a1 {
                            union(uf, i as u32 + 1, j as u32 + 1);
                        }
                        if a1 < b1 {
                            i += 1;
                        } else {
                            j += 1;
                        }
                    }
                };
                if y > 0 {
                    merge(&mut uf, (y - 1) + z * n);
                }
                if z > 0 {
                    merge(&mut uf, y + (z - 1) * n);
                }
            }
        }

        // ── Pass 3: any run not joined to OUTSIDE is a sealed void — fill it solid. ──────────
        let out_root = find(&mut uf, 0);

        // CLOSURE INVARIANT — the single design-agnostic check on this pass: no unreached air
        // cell may be 6-adjacent to a reached one. Any under-reach (the FALSE-SEAL direction,
        // which is the unrecoverable one — filling is one-way and latches `had_enclosed_void`)
        // violates it, and verifying it needs no topology reasoning, so it holds for a flood, a
        // run-CCL, or anything else that replaces this later. Ported from the parallel fix on
        // `work/poly-perf` (`03ac5da`), which caught this same bug class independently.
        // Debug-only: O(n³) with 6 neighbour probes.
        #[cfg(debug_assertions)]
        {
            let cell_total = n * n * n;
            let mut air_dbg = vec![false; cell_total];
            let mut reach_dbg = vec![false; cell_total];
            for row in 0..n * n {
                let base = row * n;
                for ri in row_start[row] as usize..row_start[row + 1] as usize {
                    let reached = find(&mut uf, ri as u32 + 1) == out_root;
                    let (x0, x1) = runs[ri];
                    for x in x0 as usize..=x1 as usize {
                        air_dbg[base + x] = true;
                        reach_dbg[base + x] = reached;
                    }
                }
            }
            let idx = |x: usize, y: usize, z: usize| x + y * n + z * n * n;
            for z in 0..n {
                for y in 0..n {
                    for x in 0..n {
                        let i = idx(x, y, z);
                        if !air_dbg[i] || reach_dbg[i] {
                            continue;
                        }
                        let touches_reached = (x > 0 && reach_dbg[idx(x - 1, y, z)])
                            || (x + 1 < n && reach_dbg[idx(x + 1, y, z)])
                            || (y > 0 && reach_dbg[idx(x, y - 1, z)])
                            || (y + 1 < n && reach_dbg[idx(x, y + 1, z)])
                            || (z > 0 && reach_dbg[idx(x, y, z - 1)])
                            || (z + 1 < n && reach_dbg[idx(x, y, z + 1)]);
                        debug_assert!(
                            !touches_reached,
                            "seal under-reach at ({x},{y},{z}): open air adjacent to reached \
                             air was left unreached and would be filled SOLID"
                        );
                    }
                }
            }
        }

        let mut filled = 0usize;
        for row in 0..n * n {
            let (s, e) = (row_start[row] as usize, row_start[row + 1] as usize);
            if s == e {
                continue;
            }
            let base = row * n;
            for ri in s..e {
                if find(&mut uf, ri as u32 + 1) == out_root {
                    continue;
                }
                let (x0, x1) = runs[ri];
                for x in x0 as usize..=x1 as usize {
                    self.density.set_idx(base + x, 1.0);
                    filled += 1;
                }
            }
        }
        if filled > 0 {
            self.mark_dirty([0, 0, 0], [self.res - 1, self.res - 1, self.res - 1]);
        }
        filled
    }

    /// SERIAL reference implementation of [`Self::surface_nets`] — the pre-2026-08-08 body,
    /// kept verbatim as the equality oracle for the parallel rewrite. Test-only: never ships.
    #[cfg(test)]
    pub(crate) fn surface_nets_serial(&self, smooth: bool) -> Mesh {
        self.surface_nets_serial_placed(smooth, CellPlacement::Centroid)
    }

    /// Serial reference for [`Self::surface_nets_placed`] (parity gate).
    pub(crate) fn surface_nets_serial_placed(
        &self,
        smooth: bool,
        placement: CellPlacement,
    ) -> Mesh {
        let n = self.res;
        let cells = (n - 1) as usize;
        let iso = self.iso;
        // cell (cx,cy,cz) -> vertex index, or u32::MAX if no surface in that cell.
        let mut cell_vert = vec![u32::MAX; cells * cells * cells];
        let cidx = |cx: usize, cy: usize, cz: usize| cx + cy * cells + cz * cells * cells;

        let mut vertices: Vec<Vec3> = Vec::new();
        let mut normals: Vec<Vec3> = Vec::new();

        for cz in 0..cells {
            for cy in 0..cells {
                for cx in 0..cells {
                    if let Some((world, nrm)) = self.cell_surface_vertex_placed(
                        cx as u32, cy as u32, cz as u32, placement,
                    ) {
                        cell_vert[cidx(cx, cy, cz)] = vertices.len() as u32;
                        vertices.push(world);
                        normals.push(nrm);
                    }
                }
            }
        }

        let mut quads: Vec<[u32; 4]> = Vec::new();
        let dens = |x: u32, y: u32, z: u32| self.density.get_idx(self.idx(x, y, z));
        let solid = |x: u32, y: u32, z: u32| dens(x, y, z) >= iso;
        let cells_i = cells as i64;
        let cv = |cx: i64, cy: i64, cz: i64| -> u32 {
            if cx < 0 || cy < 0 || cz < 0 || cx >= cells_i || cy >= cells_i || cz >= cells_i {
                u32::MAX
            } else {
                cell_vert[(cx + cy * cells_i + cz * cells_i * cells_i) as usize]
            }
        };

        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    let (xi, yi, zi) = (x as i64, y as i64, z as i64);
                    if x + 1 < n {
                        let s0 = solid(x, y, z);
                        let s1 = solid(x + 1, y, z);
                        if s0 != s1 {
                            let c = [
                                cv(xi, yi - 1, zi - 1), cv(xi, yi, zi - 1),
                                cv(xi, yi, zi), cv(xi, yi - 1, zi),
                            ];
                            if c.iter().all(|&v| v != u32::MAX) {
                                if s0 { quads.push([c[0], c[1], c[2], c[3]]); }
                                else { quads.push([c[3], c[2], c[1], c[0]]); }
                            }
                        }
                    }
                    if y + 1 < n {
                        let s0 = solid(x, y, z);
                        let s1 = solid(x, y + 1, z);
                        if s0 != s1 {
                            let c = [
                                cv(xi - 1, yi, zi - 1), cv(xi - 1, yi, zi),
                                cv(xi, yi, zi), cv(xi, yi, zi - 1),
                            ];
                            if c.iter().all(|&v| v != u32::MAX) {
                                if s0 { quads.push([c[0], c[1], c[2], c[3]]); }
                                else { quads.push([c[3], c[2], c[1], c[0]]); }
                            }
                        }
                    }
                    if z + 1 < n {
                        let s0 = solid(x, y, z);
                        let s1 = solid(x, y, z + 1);
                        if s0 != s1 {
                            let c = [
                                cv(xi - 1, yi - 1, zi), cv(xi, yi - 1, zi),
                                cv(xi, yi, zi), cv(xi - 1, yi, zi),
                            ];
                            if c.iter().all(|&v| v != u32::MAX) {
                                if s0 { quads.push([c[0], c[1], c[2], c[3]]); }
                                else { quads.push([c[3], c[2], c[1], c[0]]); }
                            }
                        }
                    }
                }
            }
        }

        if smooth { relax_mesh(self, &mut vertices, &mut normals, &quads); }

        Mesh { vertices, normals, quads }
    }

    /// Surface-Nets vertex for one cell (cx,cy,cz): the average of the 12 edge zero-crossings
    /// in world space, plus its (tight-stencil) gradient normal. None if the cell is fully
    /// inside or outside the iso surface. Shared by `surface_nets` and `VoxelMeshCache`.
    pub fn cell_surface_vertex(&self, cx: u32, cy: u32, cz: u32) -> Option<(Vec3, Vec3)> {
        self.cell_surface_vertex_placed(cx, cy, cz, CellPlacement::Centroid)
    }

    /// [`Self::cell_surface_vertex`] with explicit placement.
    pub fn cell_surface_vertex_placed(
        &self,
        cx: u32,
        cy: u32,
        cz: u32,
        placement: CellPlacement,
    ) -> Option<(Vec3, Vec3)> {
        let mut d = [0.0f32; 8];
        for (k, c) in CORNER.iter().enumerate() {
            d[k] = self.density.get_idx(self.idx(cx + c[0], cy + c[1], cz + c[2]));
        }
        match placement {
            CellPlacement::Centroid => self.cell_surface_vertex_d(cx, cy, cz, &d),
            CellPlacement::Qef => self.cell_surface_vertex_qef_d(cx, cy, cz, &d),
        }
    }

    /// DUAL CONTOURING vertex for one cell: solve the QEF over Hermite crossings (each edge
    /// zero-crossing plus the field gradient there) instead of averaging the crossing positions.
    ///
    /// Why this is not a tweak of the centroid: Surface Nets places the vertex at the mean of the
    /// crossings, which on a curved surface sits systematically INSIDE the true surface by an
    /// amount that varies with how the surface cuts the cell — periodic with the grid, which is
    /// what reads as terracing. The QEF instead finds the point minimising squared distance to the
    /// crossing TANGENT PLANES, which lands on the surface for planar and near-planar patches and
    /// recovers creases where planes meet.
    ///
    /// Three details are load-bearing, all learned by the SDF path in `chunked_dc::extract`:
    /// * `crossings.len() >= 3` before solving — fewer planes leave the system underdetermined and
    ///   the solve slides the vertex along the surface for no benefit.
    /// * the result is CLAMPED to the cell — an ill-conditioned QEF otherwise flings the vertex
    ///   across the mesh, and a spike is far worse than a step.
    /// * the normal is the AVERAGE OF THE CROSSING NORMALS, never a gradient re-sampled at the
    ///   vertex. On a concave crease (a subtract, e.g. a drilled hole) a re-sampled gradient
    ///   straddles both field branches and collapses toward zero — `chunked_dc::extract` records
    ///   that exact failure as "the jagged crater rim".
    pub fn cell_surface_vertex_qef_d(
        &self,
        cx: u32,
        cy: u32,
        cz: u32,
        d: &[f32; 8],
    ) -> Option<(Vec3, Vec3)> {
        let iso = self.iso;
        let mut inside = 0u32;
        for &v in d.iter() { if v >= iso { inside += 1; } }
        if inside == 0 || inside == 8 { return None; }

        let vs = self.voxel_size();
        let origin = self.cell_world_pos(cx, cy, cz);
        let mut crossings: Vec<(Vec3, Vec3)> = Vec::with_capacity(12);
        for e in EDGE.iter() {
            let (a, b) = (e[0], e[1]);
            let (da, db) = (d[a], d[b]);
            if (da >= iso) == (db >= iso) { continue; }
            let tt = (iso - da) / (db - da);
            let ca = CORNER[a];
            let cb = CORNER[b];
            let pa = Vec3::new(ca[0] as f32, ca[1] as f32, ca[2] as f32);
            let pb = Vec3::new(cb[0] as f32, cb[1] as f32, cb[2] as f32);
            let world = origin + pa.lerp(pb, tt) * vs;
            // Stencil matches the centroid path's normal (half the AA ramp) so the gradient stays
            // off the saturated plateau of the narrow band.
            let n = self.gradient_normal(world, vs);
            if n.length_squared() > 1e-12 {
                crossings.push((world, n));
            }
        }
        if crossings.is_empty() {
            return self.cell_surface_vertex_d(cx, cy, cz, d);
        }

        let center = origin + Vec3::splat(0.5 * vs);
        let pos = if crossings.len() >= 3 {
            let v = crate::mesh::solve_qef(&crossings, center, vs);
            v.clamp(origin, origin + Vec3::splat(vs))
        } else {
            let sum: Vec3 = crossings.iter().map(|(p, _)| *p).fold(Vec3::ZERO, |a, b| a + b);
            sum / crossings.len() as f32
        };

        let nsum: Vec3 = crossings.iter().map(|(_, n)| *n).fold(Vec3::ZERO, |a, b| a + b);
        let nrm = if nsum.length_squared() > 1e-12 {
            nsum.normalize()
        } else {
            self.gradient_normal(pos, vs)
        };
        Some((pos, nrm))
    }

    /// Same as `cell_surface_vertex` but with the 8 cell-corner densities supplied (in `CORNER`
    /// order) — lets the chunk mesher read corners from a small cache-local buffer instead of
    /// striding the full density grid 8× per cell (the high-res re-mesh hot path).
    pub fn cell_surface_vertex_d(&self, cx: u32, cy: u32, cz: u32, d: &[f32; 8]) -> Option<(Vec3, Vec3)> {
        let world = self.cell_surface_pos_d(cx, cy, cz, d)?;
        // Stencil = HALF the AA ramp (FILL_AA_VOXELS = 2.0 → e = 1.0 voxel). The fill ramp is
        // one-sided (density 0 at the surface, 1 at −2 voxels; iso sits −1 voxel in), so a
        // central difference stays UNSATURATED only for e ≤ ramp/2. At e = 2.0 every off-normal
        // sample clipped to the 0/1 plateau → the gradient direction quantized with latitude
        // (concentric matcap rings) and degenerated to zero at the poles (black blotches).
        Some((world, self.gradient_normal(world, self.voxel_size() * 1.0)))
    }

    /// Just the iso-crossing vertex POSITION for a cell, given its 8 corner densities (CORNER
    /// order). Splitting position from normal lets the chunk mesher pair the cheap position with
    /// a corner-gradient normal (no scattered field samples) — see `mesh_region`.
    pub fn cell_surface_pos_d(&self, cx: u32, cy: u32, cz: u32, d: &[f32; 8]) -> Option<Vec3> {
        let iso = self.iso;
        let mut inside = 0u32;
        for &v in d.iter() { if v >= iso { inside += 1; } }
        if inside == 0 || inside == 8 { return None; } // fully in or out → no surface
        let mut acc = Vec3::ZERO;
        let mut count = 0.0f32;
        for e in EDGE.iter() {
            let (a, b) = (e[0], e[1]);
            let (da, db) = (d[a], d[b]);
            if (da >= iso) == (db >= iso) { continue; }
            let tt = (iso - da) / (db - da);
            let ca = CORNER[a]; let cb = CORNER[b];
            let pa = Vec3::new(ca[0] as f32, ca[1] as f32, ca[2] as f32);
            let pb = Vec3::new(cb[0] as f32, cb[1] as f32, cb[2] as f32);
            acc += pa.lerp(pb, tt);
            count += 1.0;
        }
        if count == 0.0 { return None; }
        let local = acc / count; // in [0,1]^3 cell space
        Some(self.cell_world_pos(cx, cy, cz) + local * self.voxel_size())
    }

    /// P-B chunk mesher: mesh a CELL sub-range whose ANCHOR cells are `[a0..=a1]` into a
    /// self-contained flat raster mesh (pos+normal+uv verts + triangle indices). Quads are
    /// only those anchored in `[a0..=a1]` (so adjacent chunks never emit the same quad), and
    /// each quad reads cells down to its −1 neighbours — so vertices are computed over the
    /// expanded range `[a0−1 ..= a1]` (clamped). That −1 layer duplicates the previous chunk's
    /// boundary vertices, keeping the surface gap-free across chunk borders (tiny overdraw).
    /// Re-mesh only the chunk(s) a brush touched → per-frame cost scales with the brush.
    /// Object-Mode / remesh display mesh for the WHOLE grid.
    ///
    /// **Always native resolution** via surface-chunk stitch (O(surface), not O(res³)).
    /// Prior path downsampled 1024³ → 512³ which re-introduced chunky voxel rings after remesh
    /// (owner vs 3D-Coat). Full grid still lives in the sculpt atlas; this is the triangle view.
    pub fn object_display_mesh(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        const CHUNK: u32 = 16; // matches app VOXEL_CHUNK
        self.object_display_mesh_chunked(CHUNK)
    }

    /// Parallel surface-chunk Surface-Nets → one stitched mesh (`chunk_size` = cells/axis, typically 16).
    pub fn object_display_mesh_chunked(&self, chunk_size: u32) -> (Vec<[f32; 8]>, Vec<u32>) {
        let cs = chunk_size.max(1);
        let cells = self.res.saturating_sub(1).max(1);
        let cd = ((cells + cs - 1) / cs).max(1);
        let mask = self.surface_chunk_mask(cs);
        let surface: Vec<u32> = mask
            .iter()
            .enumerate()
            .filter_map(|(i, &has)| if has { Some(i as u32) } else { None })
            .collect();
        if surface.is_empty() {
            let cm = self.res.saturating_sub(2);
            return self.mesh_region([0, 0, 0], [cm, cm, cm]);
        }
        let cmax = self.res.saturating_sub(2);
        use rayon::prelude::*;
        let pieces: Vec<(Vec<[f32; 8]>, Vec<u32>)> = surface
            .par_iter()
            .map(|&ci| {
                let cx = ci % cd;
                let cy = (ci / cd) % cd;
                let cz = ci / (cd * cd);
                let a0 = [cx * cs, cy * cs, cz * cs];
                let a1 = [
                    ((cx + 1) * cs - 1).min(cmax),
                    ((cy + 1) * cs - 1).min(cmax),
                    ((cz + 1) * cs - 1).min(cmax),
                ];
                self.mesh_region(a0, a1)
            })
            .collect();
        let mut total_v = 0usize;
        let mut total_i = 0usize;
        for (v, i) in &pieces {
            total_v += v.len();
            total_i += i.len();
        }
        let mut verts = Vec::with_capacity(total_v);
        let mut idx = Vec::with_capacity(total_i);
        for (v, i) in pieces {
            let base = verts.len() as u32;
            verts.extend_from_slice(&v);
            idx.extend(i.into_iter().map(|j| j + base));
        }
        (verts, idx)
    }

    pub fn mesh_region(&self, a0: [u32; 3], a1: [u32; 3]) -> (Vec<[f32; 8]>, Vec<u32>) {
        let (verts, idx, _cells) = self.mesh_region_with_cells(a0, a1);
        (verts, idx)
    }

    /// Same as `mesh_region` but ALSO returns the per-vertex CELL ID (`[cx,cy,cz]`, the Surface-Nets
    /// cell each vertex was emitted from), parallel to `verts` (one id per vertex, same order). The
    /// zero-remesh preview paths store this provenance so they can re-derive a vertex's colour
    /// (`recolor_chunk_vertices`) or position (`reproject_chunk_positions`) from the live density
    /// without re-running quad emission. `mesh_region` is a thin wrapper that drops the ids, so every
    /// existing caller (object_display_mesh, the release remesh, tests) is byte-identical.
    pub fn mesh_region_with_cells(&self, a0: [u32; 3], a1: [u32; 3]) -> (Vec<[f32; 8]>, Vec<u32>, Vec<[u32; 3]>) {
        let n = self.res;
        // Clamped quad-anchor + vertex-emission cell ranges — the SINGLE source of truth shared with
        // `reproject_chunk_positions`' drift scan (so the two never disagree about this chunk's cells).
        let ((a_lo, a_hi), (v_lo, v_hi)) = match self.chunk_cell_ranges(a0, a1) {
            Some(r) => r,
            None => return (Vec::new(), Vec::new(), Vec::new()),
        };
        let (ax0, ay0, az0) = (a_lo[0], a_lo[1], a_lo[2]);
        let (ax1, ay1, az1) = (a_hi[0], a_hi[1], a_hi[2]);
        let (vx0, vy0, vz0) = (v_lo[0], v_lo[1], v_lo[2]);
        let (vx1, vy1, vz1) = (v_hi[0], v_hi[1], v_hi[2]);
        let lnx = (vx1 - vx0 + 1) as usize;
        let lny = (vy1 - vy0 + 1) as usize;
        let lidx = |x: u32, y: u32, z: u32| -> usize {
            ((x - vx0) as usize) + ((y - vy0) as usize) * lnx + ((z - vz0) as usize) * lnx * lny
        };
        let mut local_vert = vec![u32::MAX; lnx * lny * ((vz1 - vz0 + 1) as usize)];
        let mut verts: Vec<[f32; 8]> = Vec::new();
        // Per-vertex cell provenance, pushed in lockstep with `verts` (so cells[i] is the cell that
        // produced verts[i]). The preview paths re-read this exact cell to derive colour / position.
        let mut cell_ids: Vec<[u32; 3]> = Vec::new();
        for cz in vz0..=vz1 { for cy in vy0..=vy1 { for cx in vx0..=vx1 {
            // 8 corner densities in CORNER order (shared with the reproject preview).
            let d = self.cell_corner_densities(cx, cy, cz);
            if let Some((w, nm)) = self.cell_surface_vertex_d(cx, cy, cz, &d) {
                local_vert[lidx(cx, cy, cz)] = verts.len() as u32;
                let ci = self.idx(cx, cy, cz);
                let (cu, cv) = self.chunk_vertex_color(ci);
                verts.push([w.x, w.y, w.z, nm.x, nm.y, nm.z, cu, cv]);
                cell_ids.push([cx, cy, cz]);
            }
        }}}
        let (ix0, iy0, iz0) = (vx0 as i64, vy0 as i64, vz0 as i64);
        let (ix1, iy1, iz1) = (vx1 as i64, vy1 as i64, vz1 as i64);
        let cv = |x: i64, y: i64, z: i64| -> u32 {
            if x < ix0 || y < iy0 || z < iz0 || x > ix1 || y > iy1 || z > iz1 { return u32::MAX; }
            local_vert[lidx(x as u32, y as u32, z as u32)]
        };
        let iso = self.iso;
        let solid = |x: u32, y: u32, z: u32| self.density.get_idx(self.idx(x, y, z)) >= iso;
        let mut idx: Vec<u32> = Vec::new();
        for z in az0..=az1 { for y in ay0..=ay1 { for x in ax0..=ax1 {
            let (xi, yi, zi) = (x as i64, y as i64, z as i64);
            for (axis, c) in [
                // (can-emit, c[]) for +X, +Y, +Z — same cell arrays + winding as surface_nets.
                (x + 1 < n && solid(x, y, z) != solid(x + 1, y, z),
                    [cv(xi, yi-1, zi-1), cv(xi, yi, zi-1), cv(xi, yi, zi), cv(xi, yi-1, zi)]),
                (y + 1 < n && solid(x, y, z) != solid(x, y + 1, z),
                    [cv(xi-1, yi, zi-1), cv(xi-1, yi, zi), cv(xi, yi, zi), cv(xi, yi, zi-1)]),
                (z + 1 < n && solid(x, y, z) != solid(x, y, z + 1),
                    [cv(xi-1, yi-1, zi), cv(xi, yi-1, zi), cv(xi, yi, zi), cv(xi-1, yi, zi)]),
            ] {
                if axis && c.iter().all(|&v| v != u32::MAX) {
                    let s0 = solid(x, y, z);
                    let q = if s0 { [c[0],c[1],c[2],c[3]] } else { [c[3],c[2],c[1],c[0]] };
                    idx.push(q[0]); idx.push(q[1]); idx.push(q[2]);
                    idx.push(q[0]); idx.push(q[2]); idx.push(q[3]);
                }
            }
        }}}
        debug_assert_eq!(verts.len(), cell_ids.len(), "cell-id provenance must be parallel to verts");
        (verts, idx, cell_ids)
    }

    /// ZERO-REMESH COLOUR PREVIEW: rewrite ONLY the per-vertex colour word (`verts[i][6..8]`) in
    /// place from the live density's colour/mask state, using the per-vertex cell provenance from
    /// `mesh_region_with_cells`. Positions, normals, vertex count, and topology are UNCHANGED — a
    /// Paint / Mask stroke alters colour only, so a full Surface-Nets re-mesh is pure waste. Colour
    /// is packed through the SAME `chunk_vertex_color` helper `mesh_region` uses, so the previewed
    /// colour is byte-identical to a from-scratch mesh (no pop on stroke release).
    ///
    /// `verts` and `cells` MUST be the parallel pair a prior `mesh_region_with_cells` produced for
    /// this chunk (one cell id per vertex). A length mismatch is a provenance desync — debug-asserts,
    /// and in release leaves the shorter tail untouched rather than indexing out of bounds.
    pub fn recolor_chunk_vertices(&self, verts: &mut [[f32; 8]], cells: &[[u32; 3]]) {
        debug_assert_eq!(verts.len(), cells.len(), "recolor: verts/cells must be parallel");
        for (v, &[cx, cy, cz]) in verts.iter_mut().zip(cells.iter()) {
            let (cu, cv) = self.chunk_vertex_color(self.idx(cx, cy, cz));
            v[6] = cu;
            v[7] = cv;
        }
    }

    /// IN-PLACE COLOUR WRITE for GPU-meshed chunk vertices (Decision B). The resident/non-resident GPU
    /// mesher emits the surface but writes NO colour (uv = 0). This re-derives each vertex's anchor cell
    /// from its world POSITION (`world_to_grid().floor()` — the inverse of the `cell_world_pos + local`
    /// the mesher placed it at, so the floor recovers the SAME `[cx,cy,cz]` anchor `mesh_region` reads)
    /// and packs its colour word through the SAME `chunk_vertex_color` helper. Used after a GPU mesh
    /// dispatch lands its CPU verts (the readback), so a paint stroke's topology comes from the GPU and
    /// only the colour is a cheap CPU word rewrite — never a CPU per-frame Surface-Nets re-mesh. Touches
    /// ONLY verts[i][6..8]; positions/normals/indices untouched. Byte-identical colour to a CPU mesh
    /// (same anchor cell, same helper) so there is no colour pop when the release mesh_region runs.
    pub fn recolor_vertices_by_position(&self, verts: &mut [[f32; 8]]) {
        let cells = self.res.saturating_sub(1);
        if cells == 0 { return; }
        let cmax = cells - 1;
        for v in verts.iter_mut() {
            let g = self.world_to_grid(Vec3::new(v[0], v[1], v[2]));
            // Clamp to the valid cell-anchor range; a boundary vertex can sit exactly on the max face.
            let cx = (g.x.floor().max(0.0) as u32).min(cmax);
            let cy = (g.y.floor().max(0.0) as u32).min(cmax);
            let cz = (g.z.floor().max(0.0) as u32).min(cmax);
            let (cu, cv) = self.chunk_vertex_color(self.idx(cx, cy, cz));
            v[6] = cu;
            v[7] = cv;
        }
    }

    /// 8 corner densities (CORNER order) for the cell anchored at (cx,cy,cz). Shared by the chunk
    /// mesher and the reproject preview so the iso-crossing test reads the EXACT same samples.
    #[inline]
    fn cell_corner_densities(&self, cx: u32, cy: u32, cz: u32) -> [f32; 8] {
        [
            self.density.get_idx(self.idx(cx, cy, cz)),       self.density.get_idx(self.idx(cx + 1, cy, cz)),
            self.density.get_idx(self.idx(cx, cy + 1, cz)),   self.density.get_idx(self.idx(cx + 1, cy + 1, cz)),
            self.density.get_idx(self.idx(cx, cy, cz + 1)),   self.density.get_idx(self.idx(cx + 1, cy, cz + 1)),
            self.density.get_idx(self.idx(cx, cy + 1, cz + 1)), self.density.get_idx(self.idx(cx + 1, cy + 1, cz + 1)),
        ]
    }

    /// The two clamped cell ranges a chunk anchored at `[a0,a1]` owns: `(anchor0, anchor1)` — the QUAD
    /// scan range (clamped to valid cells) — and `(vert0, vert1)` — the VERTEX-emission range, which is
    /// the anchor range extended by the −1 halo on the low side (Surface-Nets emits a vertex for the
    /// cell one below each anchor; the high side is the anchor max, since quads read cells ≤ their
    /// anchor). Returns `None` when the chunk meshes EMPTY (degenerate grid, or a clamped-out / inverted
    /// range). This is the SINGLE source of truth for "which cells this chunk meshes," shared by
    /// `mesh_region_with_cells` (the emit loops) and `reproject_chunk_positions` (the drift "newly
    /// crosses" scan), so the two can never disagree about a chunk's cell extent — a divergence there
    /// would let a topology change slip past the drift guard (a dropped bump that pops in on release).
    #[inline]
    fn chunk_cell_ranges(&self, a0: [u32; 3], a1: [u32; 3]) -> Option<(([u32; 3], [u32; 3]), ([u32; 3], [u32; 3]))> {
        let cells = self.res.saturating_sub(1);
        if cells == 0 { return None; }
        let cmax = cells - 1;
        let an0 = [a0[0].min(cmax), a0[1].min(cmax), a0[2].min(cmax)];
        let an1 = [a1[0].min(cmax), a1[1].min(cmax), a1[2].min(cmax)];
        if an1[0] < an0[0] || an1[1] < an0[1] || an1[2] < an0[2] { return None; }
        let v0 = [an0[0].saturating_sub(1), an0[1].saturating_sub(1), an0[2].saturating_sub(1)];
        Some(((an0, an1), (v0, an1)))
    }

    /// ZERO-REMESH DISPLACEMENT PREVIEW: re-derive ONLY the position+normal (`verts[i][0..6]`) of the
    /// EXISTING chunk vertices from the CURRENT density, using the per-vertex cell provenance. Used by
    /// the topology-STABLE deform brushes (Move / Smudge — material is advected, the surface shifts but
    /// the set of crossing cells does not change), so a full Surface-Nets re-mesh is avoidable. The
    /// vertex count, index buffer, and colour words are left exactly as `mesh_region_with_cells` built
    /// them — only the geometry moves.
    ///
    /// Returns FALSE (topology drift) WITHOUT mutating `verts` if the surface is no longer stable, i.e.:
    ///   - any cached cell no longer crosses the iso surface (`cell_surface_vertex_d` → None), OR
    ///   - any cell this chunk MESHES newly crosses (a vertex would have to appear that this cached
    ///     array can't represent). Scanned over the chunk's FULL vertex-emission range (shared with the
    ///     mesher via `chunk_cell_ranges`), NOT just the cached vertices' bbox — a grab can pull
    ///     material into an empty region of the chunk far from the current surface.
    /// `a0`/`a1` are the chunk's cell-anchor range (same as passed to `mesh_region_with_cells`).
    /// On drift the caller MUST fall back to a full `mesh_region` for that chunk (mandatory — a
    /// reprojected-only chunk would otherwise show stale topology). On TRUE every position+normal has
    /// been rewritten in place and the chunk is ready for an in-place vertex upload.
    pub fn reproject_chunk_positions(&self, verts: &mut [[f32; 8]], cells: &[[u32; 3]], a0: [u32; 3], a1: [u32; 3]) -> bool {
        debug_assert_eq!(verts.len(), cells.len(), "reproject: verts/cells must be parallel");
        if cells.is_empty() { return true; }
        // Pass 1 — every cached cell must STILL cross (no vanished vertex). Re-derive its pos+normal
        // into a scratch buffer; abort on the first None so we never leave a partial mutation.
        let mut new_pn: Vec<(Vec3, Vec3)> = Vec::with_capacity(cells.len());
        for &[cx, cy, cz] in cells.iter() {
            let d = self.cell_corner_densities(cx, cy, cz);
            match self.cell_surface_vertex_d(cx, cy, cz, &d) {
                Some(pn) => new_pn.push(pn),
                None => return false, // a vertex's own cell stopped crossing → topology changed
            }
        }
        // Pass 2 — no cell this chunk MESHES may NEWLY cross (a vertex would have to appear). Scan the
        // chunk's FULL vertex-emission cell range — the SAME range `mesh_region_with_cells` emits from
        // (shared via `chunk_cell_ranges`), NOT just the cached vertices' bbox: a grab can pull material
        // into an empty cell ANYWHERE the chunk meshes, including outside the current surface extent.
        // The first crossing cell not already represented is drift. Bounded by the chunk's own cell span
        // (≤ (VOXEL_CHUNK+1)³), with no quad emission / allocation (cheaper than a full re-mesh).
        use std::collections::HashSet;
        let (_, (v_lo, v_hi)) = match self.chunk_cell_ranges(a0, a1) {
            Some(r) => r,
            None => return true, // degenerate / empty chunk — nothing this chunk could newly cross
        };
        let have: HashSet<[u32; 3]> = cells.iter().copied().collect();
        let iso = self.iso;
        for cz in v_lo[2]..=v_hi[2] { for cy in v_lo[1]..=v_hi[1] { for cx in v_lo[0]..=v_hi[0] {
            if have.contains(&[cx, cy, cz]) { continue; }
            let d = self.cell_corner_densities(cx, cy, cz);
            let mut inside = 0u32;
            for &v in d.iter() { if v >= iso { inside += 1; } }
            if inside != 0 && inside != 8 { return false; } // a NEW crossing cell → topology changed
        }}}
        // Stable — commit the re-derived geometry in place (colour words + indices untouched).
        for (v, (w, nm)) in verts.iter_mut().zip(new_pn.into_iter()) {
            v[0] = w.x; v[1] = w.y; v[2] = w.z;
            v[3] = nm.x; v[4] = nm.y; v[5] = nm.z;
        }
        true
    }

    /// Pack ONE chunk vertex's colour word (the `uv.x`, `uv.y` pair) for the cell anchored at flat
    /// index `ci`. The SINGLE source of truth for the chunk-mesh colour packing — `mesh_region`,
    /// `recolor_chunk_vertices`, and the zero-remesh paint preview all call this so they can never
    /// diverge (any divergence = a visible colour pop on stroke release).
    ///
    /// PAINT: the unused UV slot carries the per-vertex paint colour when present. uv.x = the packed
    /// 0x_FF_RR_GG_BB word reinterpreted as f32 (bit-exact; the raster shader bitcasts it back);
    /// uv.y = 1.0 painted / 0.0 unpainted (the shader's gate so an unpainted vertex keeps the body
    /// material). Unpainted voxels emit (0,0) → no change.
    ///
    /// MASK TINT (P3a): when `show_mask` is on and this cell is protected, darken the base colour
    /// toward black by mask factor (ZBrush-style protect overlay). Unpainted+masked cells emit a
    /// mid clay grey darkened by mask so the tint reads without requiring paint. Fully free cells
    /// keep the paint-only / body-material path above.
    #[inline]
    fn chunk_vertex_color(&self, ci: usize) -> (f32, f32) {
        let paint = self.color_idx(ci);
        let mask_f = if self.show_mask { self.mask_factor_at(ci) } else { 0.0 };
        if mask_f < 1e-4 {
            return match paint {
                Some(packed) => (f32::from_bits(packed), 1.0),
                None => (0.0, 0.0),
            };
        }
        // Base RGB: paint if present, else a neutral clay so mask is visible on body material.
        let (r, g, b) = match paint {
            Some(packed) => (
                ((packed >> 16) & 0xFF) as f32 / 255.0,
                ((packed >> 8) & 0xFF) as f32 / 255.0,
                (packed & 0xFF) as f32 / 255.0,
            ),
            None => (0.55, 0.55, 0.58),
        };
        // Full protect → ~15% residual (reads as near-black, not pure black so form still shows).
        let k = 1.0 - mask_f * 0.85;
        let packed = pack_voxel_color(Vec3::new(r * k, g * k, b * k));
        (f32::from_bits(packed), 1.0)
    }

    /// Per-chunk mask (linear chunk index, `chunk_size` cells/axis, `chunk_dim³` grid) of which
    /// chunks COULD contain surface — so a full rebuild skips empty interior/exterior chunks.
    /// Sparse (≥256³): uses brick INDEX tags (EMPTY/FULL skip voxel scan) — the 769ms@848³ fix.
    /// Dense: parallel z-plane voxel min/max (legacy path).
    pub fn surface_chunk_mask(&self, chunk_size: u32) -> Vec<bool> {
        let res = self.res;
        let cells = res.saturating_sub(1).max(1);
        let cd = (((cells + chunk_size - 1) / chunk_size).max(1)) as usize;
        let n = cd * cd * cd;
        let iso = self.iso;
        let (cmin, cmax) = if let DensityStorage::Sparse(ref bg) = self.density {
            Self::chunk_minmax_sparse(bg, chunk_size, cd, n)
        } else {
            Self::chunk_minmax_dense(self, chunk_size, cd, n)
        };
        let has = |ci: usize| cmin[ci] < iso && cmax[ci] >= iso;
        let solid_present = |ci: usize| cmax[ci] >= iso;
        let empty_present = |ci: usize| cmin[ci] < iso;
        let mut mask = vec![false; n];
        for cz in 0..cd { for cy in 0..cd { for cx in 0..cd {
            let ci = cx + cy * cd + cz * cd * cd;
            let mut need = has(ci);
            if !need {
                let nb = [
                    (cx.wrapping_sub(1), cy, cz), (cx + 1, cy, cz),
                    (cx, cy.wrapping_sub(1), cz), (cx, cy + 1, cz),
                    (cx, cy, cz.wrapping_sub(1)), (cx, cy, cz + 1),
                ];
                for (nx, ny, nz) in nb {
                    if nx < cd && ny < cd && nz < cd {
                        let nci = nx + ny * cd + nz * cd * cd;
                        if has(nci)
                            || (solid_present(ci) && empty_present(nci))
                            || (empty_present(ci) && solid_present(nci))
                        { need = true; break; }
                    }
                }
            }
            mask[ci] = need;
        }}}
        mask
    }

    /// Dense: parallel per-voxel min/max into chunk bins (O(res³) reads).
    fn chunk_minmax_dense(grid: &VoxelGrid, chunk_size: u32, cd: usize, n: usize) -> (Vec<f32>, Vec<f32>) {
        let res = grid.res;
        let plane = cd * cd;
        let (mut cmin, mut cmax) = (vec![f32::INFINITY; n], vec![f32::NEG_INFINITY; n]);
        use rayon::prelude::*;
        cmin.par_chunks_mut(plane).zip(cmax.par_chunks_mut(plane)).enumerate().for_each(|(cz, (cmin_p, cmax_p))| {
            let z_start = cz as u32 * chunk_size;
            let z_end = if cz == cd - 1 { res } else { ((cz as u32 + 1) * chunk_size).min(res) };
            for z in z_start..z_end {
                for y in 0..res {
                    let cy = ((y / chunk_size) as usize).min(cd - 1);
                    let row = (y * res + z * res * res) as usize;
                    for x in 0..res {
                        let cx = ((x / chunk_size) as usize).min(cd - 1);
                        let v = grid.density.get_idx(row + x as usize);
                        let ci = cx + cy * cd;
                        if v < cmin_p[ci] { cmin_p[ci] = v; }
                        if v > cmax_p[ci] { cmax_p[ci] = v; }
                    }
                }
            }
        });
        (cmin, cmax)
    }

    /// Sparse: EMPTY/FULL bricks contribute min/max without reading voxels; only allocated 16³
    /// shells are scanned. At 848³ a solid is mostly tags → ~10–50× faster than dense scan.
    fn chunk_minmax_sparse(bg: &BrickGrid, chunk_size: u32, cd: usize, n: usize) -> (Vec<f32>, Vec<f32>) {
        let res = bg.res;
        let br = bg.brick_res as usize;
        let (mut cmin, mut cmax) = (vec![f32::INFINITY; n], vec![f32::NEG_INFINITY; n]);
        // Per-brick parallel: each brick updates one or more chunk bins (usually 1:1 when sizes match).
        use rayon::prelude::*;
        let brick_stats: Vec<(usize, f32, f32)> = (0..br * br * br)
            .into_par_iter()
            .filter_map(|bi| {
                let tag = bg.index[bi];
                let (mn, mx) = match tag {
                    TAG_EMPTY => (0.0f32, 0.0f32),
                    TAG_FULL => (1.0f32, 1.0f32),
                    h => {
                        let o = h as usize * BRICK_VOX;
                        if o + BRICK_VOX > bg.bricks.len() {
                            return None;
                        }
                        let block = &bg.bricks[o..o + BRICK_VOX];
                        let mut a = f32::INFINITY;
                        let mut b = f32::NEG_INFINITY;
                        for &v in block {
                            if v < a { a = v; }
                            if v > b { b = v; }
                        }
                        if !a.is_finite() { return None; }
                        (a, b)
                    }
                };
                // Map brick → primary chunk index (voxel origin of brick).
                let bx = bi % br;
                let by = (bi / br) % br;
                let bz = bi / (br * br);
                let x0 = (bx as u32 * BRICK).min(res.saturating_sub(1));
                let y0 = (by as u32 * BRICK).min(res.saturating_sub(1));
                let z0 = (bz as u32 * BRICK).min(res.saturating_sub(1));
                let cx = ((x0 / chunk_size) as usize).min(cd - 1);
                let cy = ((y0 / chunk_size) as usize).min(cd - 1);
                let cz = ((z0 / chunk_size) as usize).min(cd - 1);
                let ci = cx + cy * cd + cz * cd * cd;
                Some((ci, mn, mx))
            })
            .collect();
        for (ci, mn, mx) in brick_stats {
            if mn < cmin[ci] { cmin[ci] = mn; }
            if mx > cmax[ci] { cmax[ci] = mx; }
        }
        // Chunks never touched (shouldn't happen if brick grid covers res) stay at ±inf — treat as empty.
        for i in 0..n {
            if !cmin[i].is_finite() {
                cmin[i] = 0.0;
                cmax[i] = 0.0;
            }
        }
        (cmin, cmax)
    }
}

/// The 8 corners of a cell indexed 0..8 by (dx,dy,dz), and its 12 edges as (corner_a, corner_b).
/// How the extractor places a cell's vertex.
///
/// Which cells changed since the cached extract was taken, in **grid POINT** coordinates — the
/// same inclusive box handed to `write_region_from_sdf_ex`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DirtyCells {
    /// The grid is provably byte-identical: reuse every cached slab, recompute nothing.
    None,
    /// Only this point box was rewritten.
    Points { lo: [u32; 3], hi: [u32; 3] },
    /// Nothing is reusable (first frame, parameter change, or a whole-field write such as
    /// `seal_enclosed_air` actually filling something).
    All,
}

/// Retained Phase A output for [`VoxelGrid::surface_nets_placed_cached`] — the regional extract's
/// cache. Runtime-only; never serialized.
///
/// Keyed per **cz slab** because that is the unit Phase A already emits, so reuse cannot perturb
/// emission order and therefore cannot perturb vertex numbering. The stored `u32` is the
/// PLANE-LOCAL cell index (`cx + cy*cells`), which depends only on resolution — deliberately not
/// the compacted chunk slot, which renumbers whenever the live chunk set changes and would then
/// silently address a different chunk.
#[derive(Default)]
pub struct SurfaceNetsCache {
    res: u32,
    placement: CellPlacement,
    slabs: Vec<Vec<(u32, Vec3, Vec3)>>,
    pub primed: bool,
    /// How many slabs the last call actually recomputed — the anti-vacuity signal a perf test
    /// needs (a cache that silently recomputes everything still passes a parity test).
    pub recomputed_slabs: usize,
    /// Exclusive cz range last recomputed (`lo..hi`). Used by remesh S3 to project only
    /// those slabs; reused slabs keep last projected positions keyed by cz, not compact index.
    pub recompute_lo: usize,
    pub recompute_hi: usize,
}

impl SurfaceNetsCache {
    /// Vertex count per cz slab, in emission order (same order as `Mesh.vertices`).
    pub fn slab_vert_counts(&self) -> Vec<usize> {
        self.slabs.iter().map(|s| s.len()).collect()
    }
}

/// A MODE on one extractor, not a second extractor: the cell loop, quad emission and vertex
/// numbering are shared, so the two settings differ only in `cell_surface_vertex_placed`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CellPlacement {
    /// Surface Nets — centroid of the edge crossings. The historical behaviour; the sculpt
    /// display path uses this and is bit-identical to before `Qef` existed.
    #[default]
    Centroid,
    /// Dual Contouring — QEF over Hermite crossings. Used by the poly Remesh modifier, where the
    /// centroid's cell-centre bias is what shows up as terracing on curved surfaces.
    Qef,
}

const CORNER: [[u32; 3]; 8] = [
    [0,0,0],[1,0,0],[0,1,0],[1,1,0],[0,0,1],[1,0,1],[0,1,1],[1,1,1],
];
const EDGE: [[usize; 2]; 12] = [
    [0,1],[0,2],[0,4],[1,3],[1,5],[2,3],[2,6],[3,7],[4,5],[4,6],[5,7],[6,7],
];

/// Laplacian relaxation of Surface-Nets vertices + gradient normals.
/// Pulls each vertex toward the centroid of its quad-edge neighbours (removes the staircase)
/// and re-derives normals with a moderate stencil (kills diamond faceting without equator seams).
pub(super) fn relax_mesh(grid: &VoxelGrid, vertices: &mut [Vec3], normals: &mut [Vec3], quads: &[[u32; 4]]) {
    // Gate 0 detail preservation (feel audit 2026-07-23): 2 iters @ 0.5 removed 50-75% of a
    // 1-voxel ridge and flattened the difference between falloff curves — the deposited detail
    // was correct, then erased by this low-pass. One iteration @ 0.25 still kills the
    // Surface-Nets staircase while keeping brush-deposited creases/ridges readable.
    const RELAX_ITERS: usize = 1;
    const RELAX_RATE: f32 = 0.25;
    if vertices.is_empty() { return; }
    let mut nbr_sum = vec![Vec3::ZERO; vertices.len()];
    let mut nbr_cnt = vec![0u32; vertices.len()];
    for _ in 0..RELAX_ITERS {
        for s in nbr_sum.iter_mut() { *s = Vec3::ZERO; }
        for c in nbr_cnt.iter_mut() { *c = 0; }
        for q in quads {
            for k in 0..4 {
                let a = q[k] as usize;
                let b = q[(k + 1) & 3] as usize;
                nbr_sum[a] += vertices[b]; nbr_cnt[a] += 1;
                nbr_sum[b] += vertices[a]; nbr_cnt[b] += 1;
            }
        }
        for i in 0..vertices.len() {
            if nbr_cnt[i] == 0 { continue; }
            let avg = nbr_sum[i] / nbr_cnt[i] as f32;
            vertices[i] = vertices[i].lerp(avg, RELAX_RATE);
        }
    }
    let eps = grid.voxel_size() * 1.0; // match cell_surface_vertex_d (= FILL_AA_VOXELS / 2, unsaturated)
    // Wide-stencil normals: a pure per-vertex map (6 field samples each), so it parallelizes
    // exactly. It was the bulk of relax's 18ms at 256³ (150k verts × 6 trilinear samples).
    // Same arithmetic per vertex ⇒ bit-identical to the serial form.
    {
        use rayon::prelude::*;
        normals
            .par_iter_mut()
            .zip(vertices.par_iter())
            .for_each(|(nrm, v)| {
                // Keep the per-cell normal if the stencil degenerates to zero on a flat
                // saturated patch — never emit a zero normal (black-facet shading bug).
                let n2 = grid.gradient_normal(*v, eps);
                if n2 != Vec3::ZERO { *nrm = n2; }
            });
    }
}

/// Persistent Surface-Nets cache for INCREMENTAL re-meshing. The brush edits only a small
/// region each frame, so re-scanning the whole grid (O(res³)) made 128³/192³ unusable. This
/// keeps a per-cell vertex slot across frames: `update()` re-scans only the edited region
/// (O(brush³)); `build()` regenerates the flat mesh from the live cells (O(surface)). The quad
/// pass is the verbatim Surface-Nets rule restricted to occupied cells — exactly the full loop
/// minus iterations it already drops when the anchor cell c[2]=(x,y,z) is unoccupied.
pub struct VoxelMeshCache {
    res: u32,
    cells: u32,            // res - 1
    cell_slot: Vec<i32>,   // cell index → vertex slot, or -1 (no surface)
    slot_cell: Vec<u32>,   // slot → cell index (valid when live)
    pos: Vec<Vec3>,        // per-slot world position
    nrm: Vec<Vec3>,        // per-slot normal
    live: Vec<bool>,       // slot alive?
    free: Vec<u32>,        // dead slots available for reuse
}

impl VoxelMeshCache {
    pub fn new(res: u32) -> Self {
        let res = res.max(2);
        let cells = res - 1;
        Self {
            res, cells,
            cell_slot: vec![-1; (cells * cells * cells) as usize],
            slot_cell: Vec::new(), pos: Vec::new(), nrm: Vec::new(),
            live: Vec::new(), free: Vec::new(),
        }
    }

    #[inline]
    fn cell_index(&self, cx: u32, cy: u32, cz: u32) -> usize {
        (cx + cy * self.cells + cz * self.cells * self.cells) as usize
    }

    fn alloc(&mut self, cell: u32, p: Vec3, nrm: Vec3) -> u32 {
        if let Some(s) = self.free.pop() {
            let su = s as usize;
            self.live[su] = true; self.slot_cell[su] = cell; self.pos[su] = p; self.nrm[su] = nrm;
            s
        } else {
            let s = self.pos.len() as u32;
            self.pos.push(p); self.nrm.push(nrm); self.slot_cell.push(cell); self.live.push(true);
            s
        }
    }

    /// Recompute Surface-Nets vertices for the edited region (a VOXEL AABB). `None` re-scans
    /// the whole grid (initial build / after a resolution change — which also resets the cache).
    /// A voxel `v` feeds cells `[v-1 .. v]`, so the min is lowered by 1 (clamped to a cell).
    pub fn update(&mut self, grid: &VoxelGrid, region: Option<[u32; 6]>) {
        let region = if self.res != grid.res {
            *self = VoxelMeshCache::new(grid.res);
            None // resolution changed → full rebuild at the new res
        } else { region };
        let cells = self.cells;
        if cells == 0 { return; }
        let cmax = cells - 1;
        let (cx0, cy0, cz0, cx1, cy1, cz1) = match region {
            Some([vx0, vy0, vz0, vx1, vy1, vz1]) => (
                vx0.saturating_sub(1).min(cmax), vy0.saturating_sub(1).min(cmax), vz0.saturating_sub(1).min(cmax),
                vx1.min(cmax), vy1.min(cmax), vz1.min(cmax),
            ),
            None => (0, 0, 0, cmax, cmax, cmax),
        };
        for cz in cz0..=cz1 {
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    let ci = self.cell_index(cx, cy, cz);
                    let slot = self.cell_slot[ci];
                    match (slot, grid.cell_surface_vertex(cx, cy, cz)) {
                        (-1, Some((p, nrm))) => {
                            let s = self.alloc(ci as u32, p, nrm);
                            self.cell_slot[ci] = s as i32;
                        }
                        (s, Some((p, nrm))) if s >= 0 => {
                            let su = s as usize;
                            self.pos[su] = p; self.nrm[su] = nrm;
                        }
                        (s, None) if s >= 0 => {
                            self.live[s as usize] = false;
                            self.free.push(s as u32);
                            self.cell_slot[ci] = -1;
                        }
                        _ => {} // (-1, None): still empty
                    }
                }
            }
        }
    }

    /// Build the flat raster mesh (pos+normal+uv, triangle indices) from the live cells.
    /// O(surface): compact live slots → vertices, then emit quads by iterating occupied cells
    /// only (same body + winding as `VoxelGrid::surface_nets`). `smooth` runs `relax_mesh`.
    pub fn build(&self, grid: &VoxelGrid, smooth: bool) -> (Vec<[f32; 8]>, Vec<u32>) {
        let cells = self.cells;
        if cells == 0 { return (Vec::new(), Vec::new()); }
        let iso = grid.iso;
        // Pre-size the output buffers to the slot count so they don't reallocate mid-build
        // (the per-frame realloc churn that shows up as intermittent stroke "choking").
        let mut compact = vec![u32::MAX; self.pos.len()];
        let mut vertices: Vec<Vec3> = Vec::with_capacity(self.pos.len());
        let mut normals: Vec<Vec3> = Vec::with_capacity(self.pos.len());
        for slot in 0..self.pos.len() {
            if self.live[slot] {
                compact[slot] = vertices.len() as u32;
                vertices.push(self.pos[slot]);
                normals.push(self.nrm[slot]);
            }
        }
        let cells_i = cells as i64;
        // cell coords → compact vertex index (or MAX if out of range / empty). Bounds clamp
        // kept verbatim from surface_nets — guards the boundary OOB the panic test covers.
        let cv = |cx: i64, cy: i64, cz: i64| -> u32 {
            if cx < 0 || cy < 0 || cz < 0 || cx >= cells_i || cy >= cells_i || cz >= cells_i {
                return u32::MAX;
            }
            let s = self.cell_slot[(cx + cy * cells_i + cz * cells_i * cells_i) as usize];
            if s < 0 { u32::MAX } else {
                debug_assert!(self.live[s as usize]);
                compact[s as usize]
            }
        };
        let n = self.res;
        let solid = |x: u32, y: u32, z: u32| grid.density.get_idx(grid.idx(x, y, z)) >= iso;
        let mut quads: Vec<[u32; 4]> = Vec::with_capacity(self.pos.len());
        for slot in 0..self.pos.len() {
            if !self.live[slot] { continue; }
            let ci = self.slot_cell[slot];
            let (x, y, z) = (ci % cells, (ci / cells) % cells, ci / (cells * cells));
            let (xi, yi, zi) = (x as i64, y as i64, z as i64);
            if x + 1 < n {
                let s0 = solid(x, y, z);
                if s0 != solid(x + 1, y, z) {
                    let c = [cv(xi, yi - 1, zi - 1), cv(xi, yi, zi - 1), cv(xi, yi, zi), cv(xi, yi - 1, zi)];
                    if c.iter().all(|&v| v != u32::MAX) {
                        if s0 { quads.push([c[0], c[1], c[2], c[3]]); } else { quads.push([c[3], c[2], c[1], c[0]]); }
                    }
                }
            }
            if y + 1 < n {
                let s0 = solid(x, y, z);
                if s0 != solid(x, y + 1, z) {
                    let c = [cv(xi - 1, yi, zi - 1), cv(xi - 1, yi, zi), cv(xi, yi, zi), cv(xi, yi, zi - 1)];
                    if c.iter().all(|&v| v != u32::MAX) {
                        if s0 { quads.push([c[0], c[1], c[2], c[3]]); } else { quads.push([c[3], c[2], c[1], c[0]]); }
                    }
                }
            }
            if z + 1 < n {
                let s0 = solid(x, y, z);
                if s0 != solid(x, y, z + 1) {
                    let c = [cv(xi - 1, yi - 1, zi), cv(xi, yi - 1, zi), cv(xi, yi, zi), cv(xi - 1, yi, zi)];
                    if c.iter().all(|&v| v != u32::MAX) {
                        if s0 { quads.push([c[0], c[1], c[2], c[3]]); } else { quads.push([c[3], c[2], c[1], c[0]]); }
                    }
                }
            }
        }
        if smooth { relax_mesh(grid, &mut vertices, &mut normals, &quads); }
        let mut verts = Vec::with_capacity(vertices.len());
        for i in 0..vertices.len() {
            let v = vertices[i];
            let nm = if i < normals.len() { normals[i] } else { Vec3::Y };
            verts.push([v.x, v.y, v.z, nm.x, nm.y, nm.z, 0.0, 0.0]);
        }
        let mut indices = Vec::with_capacity(quads.len() * 6);
        for q in &quads {
            indices.push(q[0]); indices.push(q[1]); indices.push(q[2]);
            indices.push(q[0]); indices.push(q[2]); indices.push(q[3]);
        }
        (verts, indices)
    }
}

#[inline]
pub(super) fn smoothstep01(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Read a neighbour of local cell `(lx,ly,lz)` from a dense `span_x·span_y·span_z` snapshot,
/// clamping out-of-range offsets to the center cell. Shared by every neighbourhood read
/// (Smooth/Inflate gradient + `relax_region`) so the stencil logic lives in ONE place.
#[inline]
pub(super) fn sub_at(s: &[f32], span_x: usize, span_y: usize, span_z: usize, lx: usize, ly: usize, lz: usize, dx: i32, dy: i32, dz: i32) -> f32 {
    let nx = lx as i32 + dx; let ny = ly as i32 + dy; let nz = lz as i32 + dz;
    if nx < 0 || ny < 0 || nz < 0 || nx >= span_x as i32 || ny >= span_y as i32 || nz >= span_z as i32 {
        s[lx + ly * span_x + lz * span_x * span_y]
    } else {
        s[nx as usize + ny as usize * span_x + nz as usize * span_x * span_y]
    }
}

/// Trilinear sample of a dense `span_x·span_y·span_z` snapshot at fractional LOCAL coords.
/// Returns `None` when the sample (or its +1 neighbour) falls outside the snapshot — callers
/// treat that as "no contribution" (advection brushes have ~0 falloff weight at the AABB edge,
/// so the missing data never matters). Used by Pinch's semi-Lagrangian gather.
#[inline]
pub(super) fn sample_sub(s: &[f32], span_x: usize, span_y: usize, span_z: usize, x: f32, y: f32, z: f32) -> Option<f32> {
    if x < 0.0 || y < 0.0 || z < 0.0 { return None; }
    let x0 = x.floor() as usize; let y0 = y.floor() as usize; let z0 = z.floor() as usize;
    if x0 + 1 >= span_x || y0 + 1 >= span_y || z0 + 1 >= span_z { return None; }
    let fx = x - x0 as f32; let fy = y - y0 as f32; let fz = z - z0 as f32;
    let g = |xi: usize, yi: usize, zi: usize| s[xi + yi * span_x + zi * span_x * span_y];
    let c00 = g(x0,y0,z0)   * (1.0-fx) + g(x0+1,y0,z0)   * fx;
    let c10 = g(x0,y0+1,z0) * (1.0-fx) + g(x0+1,y0+1,z0) * fx;
    let c01 = g(x0,y0,z0+1) * (1.0-fx) + g(x0+1,y0,z0+1) * fx;
    let c11 = g(x0,y0+1,z0+1)*(1.0-fx) + g(x0+1,y0+1,z0+1)*fx;
    let c0 = c00 * (1.0-fy) + c10 * fy;
    let c1 = c01 * (1.0-fy) + c11 * fy;
    Some(c0 * (1.0-fz) + c1 * fz)
}
