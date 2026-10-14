// chunked_dc/grid.rs -- volume accessors + sparse brick classification.
// GOD_FILES_SPLIT_PLAN: chunked_dc/{mod,grid,extract,stitch,tests}.rs
// Pure relocation from chunked_dc.rs (VolumeAccessor through sparse window alloc).

use glam::Vec3;

// ============================================================
// VolumeAccessor trait — abstracts over dense grid vs sparse bricks
// ============================================================

/// Uniform interface for sampling SDF values from a voxel grid.
/// Implementations: `FlatGridAccessor` (dense array), `BrickGridAccessor` (sparse bricks).
pub trait VolumeAccessor {
    /// Sample SDF at grid vertex coordinates (0..nverts per axis).
    fn sample(&self, x: usize, y: usize, z: usize) -> f32;
    /// Vertex count per axis (grid_resolution + 1).
    fn nverts(&self) -> usize;
}

/// Dense flat-array accessor wrapping `&[f32]` with `nverts^3` elements.
pub struct FlatGridAccessor<'a> {
    pub grid: &'a [f32],
    nverts_val: usize,
}

impl<'a> FlatGridAccessor<'a> {
    pub fn new(grid: &'a [f32], grid_resolution: u32) -> Self {
        Self { grid, nverts_val: grid_resolution as usize + 1 }
    }
}

impl VolumeAccessor for FlatGridAccessor<'_> {
    #[inline]
    fn sample(&self, x: usize, y: usize, z: usize) -> f32 {
        let nv = self.nverts_val;
        self.grid[x + y * nv + z * nv * nv]
    }
    fn nverts(&self) -> usize { self.nverts_val }
}

/// Indirection tag for a non-allocated brick that lies fully OUTSIDE the surface;
/// its constant sample is `+BRICK_FAR`. Matches the voxel atlas `TAG_EMPTY`.
pub const BRICK_TAG_EXTERIOR: u32 = 0xFFFF_FFFF;
/// Indirection tag for a non-allocated brick that lies fully INSIDE the surface;
/// its constant sample is `-BRICK_FAR`. Matches the voxel atlas `TAG_FULL`.
///
/// Phase 3 (sparse SDF): this second sentinel is REQUIRED. A single `+far` tag for
/// every non-allocated brick fabricates a spurious iso=0 surface at the boundary
/// between allocated interior data (negative) and a non-allocated interior brick
/// (which would read `+far` → false sign flip). A deep-interior brick must read a
/// NEGATIVE constant. Proven by `brick_accessor_apron_parity_adversarial`.
pub const BRICK_TAG_INTERIOR: u32 = 0xFFFF_FFFE;
/// Far signed-distance magnitude returned for a non-allocated brick. Must exceed any
/// real |distance| in the grid so a constant brick never forms a crossing with a
/// same-sign neighbour.
pub const BRICK_FAR: f32 = 100.0;

/// Sparse brick pool accessor — reads via indirection table + per-brick voxel data.
/// The brick data is dense-packed on CPU. Pool slot → dense index is resolved via
/// `slot_map`, so memory is proportional to active bricks, not max pool slot.
///
/// Non-allocated bricks read a signed constant by tag: `BRICK_TAG_EXTERIOR` → `+far`,
/// `BRICK_TAG_INTERIOR` → `-far`. Allocated bricks (handle < `BRICK_TAG_INTERIOR`)
/// read real per-voxel data. The surface-band classifier must store every surface
/// brick PLUS its 1-ring of neighbours (the DC apron — cells read ±1 voxel past their
/// brick), and tag every other brick INTERIOR/EXTERIOR by sign.
pub struct BrickGridAccessor<'a> {
    pub indirection: &'a [u32],
    pub brick_cpu_data: &'a [f32],
    pub slot_map: &'a std::collections::HashMap<u32, usize>,
    pub dims_bricks: [u32; 3],
    pub brick_dim: u32,
    pub brick_voxels: u32,
    nverts_val: usize,
}

impl<'a> BrickGridAccessor<'a> {
    pub fn new(
        indirection: &'a [u32],
        brick_cpu_data: &'a [f32],
        slot_map: &'a std::collections::HashMap<u32, usize>,
        dims_bricks: [u32; 3],
        brick_dim: u32,
        grid_resolution: u32,
    ) -> Self {
        Self {
            indirection,
            brick_cpu_data,
            slot_map,
            dims_bricks,
            brick_dim,
            brick_voxels: brick_dim * brick_dim * brick_dim,
            nverts_val: grid_resolution as usize + 1,
        }
    }
}

impl VolumeAccessor for BrickGridAccessor<'_> {
    #[inline]
    fn sample(&self, x: usize, y: usize, z: usize) -> f32 {
        let bd = self.brick_dim as usize;
        let bx = x / bd;
        let by = y / bd;
        let bz = z / bd;
        let dx = self.dims_bricks[0] as usize;
        let dy = self.dims_bricks[1] as usize;
        let flat = bx + by * dx + bz * dx * dy;
        if flat >= self.indirection.len() { return BRICK_FAR; }
        let slot = self.indirection[flat];
        if slot == BRICK_TAG_EXTERIOR { return BRICK_FAR; }   // outside → +far
        if slot == BRICK_TAG_INTERIOR { return -BRICK_FAR; }  // inside  → -far
        let dense_idx = match self.slot_map.get(&slot) {
            Some(&di) => di,
            None => return BRICK_FAR,
        };
        let lx = x % bd;
        let ly = y % bd;
        let lz = z % bd;
        let local = lx + ly * bd + lz * bd * bd;
        let idx = dense_idx * self.brick_voxels as usize + local;
        if idx >= self.brick_cpu_data.len() { return BRICK_FAR; }
        self.brick_cpu_data[idx]
    }
    fn nverts(&self) -> usize { self.nverts_val }
}

/// CPU sparse signed-distance brick set — the index table + dense-packed pool the GPU atlas mirrors and
/// `BrickGridAccessor` reads. Memory is proportional to the SURFACE SHELL, not the volume. Produced by
/// [`classify_grid_to_sparse`]; consumed by the CPU `BrickGridAccessor` (oracle / CPU-DC fallback) and,
/// once uploaded, by the GPU DC mesher (`sdf_mesh.wgsl` sparse `fld`).
///
/// Layout (the cross-language contract proven by SPIKE A/B):
/// - `index[bx + by*dims.x + bz*dims.x*dims.y]` = [`BRICK_TAG_EXTERIOR`] (+far), [`BRICK_TAG_INTERIOR`]
///   (-far), or a pool handle `h`.
/// - `pool[h*brick_voxels + (x%bd) + (y%bd)*bd + (z%bd)*bd²]` for an allocated brick.
/// - Handles are dense-packed 1:1 with pool slots (no compaction), so `slot_map` is the identity map.
pub struct SparseSdfBricks {
    pub index: Vec<u32>,
    pub pool: Vec<f32>,
    /// Identity handle→dense-index map, kept only for the CPU `BrickGridAccessor` (the GPU path indexes
    /// `pool[tag*brick_voxels + local]` directly — no map needed).
    pub slot_map: std::collections::HashMap<u32, usize>,
    pub dims_bricks: [u32; 3],
    pub brick_dim: u32,
}

impl SparseSdfBricks {
    #[inline]
    pub fn brick_voxels(&self) -> u32 { self.brick_dim * self.brick_dim * self.brick_dim }
    /// Count of allocated (real-data) bricks — the surface shell + apron.
    pub fn allocated_bricks(&self) -> usize { self.slot_map.len() }
    /// A `BrickGridAccessor` over this set, for `grid_resolution` CELLS (nverts = res+1).
    pub fn accessor(&self, grid_resolution: u32) -> BrickGridAccessor<'_> {
        BrickGridAccessor::new(&self.index, &self.pool, &self.slot_map, self.dims_bricks, self.brick_dim, grid_resolution)
    }
}

/// Classify a dense signed-distance grid (`nverts³`, linear index `x + y*nv + z*nv²`) into the sparse
/// brick format: every **surface-band** brick PLUS its 1-ring (26-neighbour) apron is stored with REAL
/// voxel data; every other brick is tagged INTERIOR (`-far`) or EXTERIOR (`+far`) by sign. Out-of-bounds
/// voxels in a stored boundary brick read `+far` (outside).
///
/// `band` is in the grid's distance units: a brick is "surface" if its value range reaches within `band`
/// of the iso=0 surface. The APRON (not the band) is what guarantees a crack-free, bit-exact DC mesh, so
/// `band` can be tight (≈ the 1.5-cell gradient stencil) to minimise over-storage — proven bit-exact vs
/// dense DC by `brick_accessor_apron_parity_adversarial` (CPU, SPIKE A) and `sdf_dc_sparse_brick_decode_
/// parity` (GPU, SPIKE B). The two signed empty tags are REQUIRED: a single `+far` tag plants a phantom
/// surface at the boundary of an interior brick.
pub fn classify_grid_to_sparse(grid: &[f32], nverts: u32, brick_dim: u32, band: f32) -> SparseSdfBricks {
    let bd = brick_dim;
    let bpa = (nverts + bd - 1) / bd; // bricks per axis (cubic grid)
    let dims = [bpa, bpa, bpa];
    let total = (dims[0] * dims[1] * dims[2]) as usize;

    // OOB voxels read +far (outside) — matches the grid's implicit exterior + the GPU `fld` OOB clamp.
    let g = |gx: u32, gy: u32, gz: u32| -> f32 {
        if gx < nverts && gy < nverts && gz < nverts {
            grid[(gx + gy * nverts + gz * nverts * nverts) as usize]
        } else {
            BRICK_FAR
        }
    };

    // Pass 1: per-brick min/max → surface flag + interior (fully-inside) flag. This is the O(res³) scan
    // (every voxel read once) — the cost that made the CPU classify ~2.4s at 512³ single-threaded. Each
    // brick's min/max is INDEPENDENT and order-invariant (min/max are associative+commutative), so it
    // parallelizes with rayon to a bit-identical result. The grid read is inlined (not via `g`) so the
    // per-element closure captures only `grid` (a Sync `&[f32]`).
    use rayon::prelude::*;
    let nv = nverts;
    let flags: Vec<(bool, bool)> = (0..total).into_par_iter().map(|bi| {
        let bi = bi as u32;
        let bx = bi % dims[0];
        let by = (bi / dims[0]) % dims[1];
        let bz = bi / (dims[0] * dims[1]);
        let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
        for lz in 0..bd {
            for ly in 0..bd {
                for lx in 0..bd {
                    let (gx, gy, gz) = (bx * bd + lx, by * bd + ly, bz * bd + lz);
                    let v = if gx < nv && gy < nv && gz < nv {
                        grid[(gx + gy * nv + gz * nv * nv) as usize]
                    } else {
                        BRICK_FAR
                    };
                    mn = mn.min(v);
                    mx = mx.max(v);
                }
            }
        }
        (mn <= band && mx >= -band, mx <= 0.0)
    }).collect();
    let is_surface: Vec<bool> = flags.iter().map(|f| f.0).collect();
    let interior: Vec<bool> = flags.iter().map(|f| f.1).collect();

    // Pass 2 + Pass 3 tags/handles are SHARED with the pre-bake prims classifier
    // (`classify_prims_to_sparse_index`) — one dilation, one emission, two Pass-1 front-ends.
    let store = dilate_surface_bricks_1ring(&is_surface, dims);
    let (index, allocated) = emit_sparse_index(&store, &interior);

    // Grid-only pool fill: handles are dense-packed in linear scan order (bx fastest — the exact
    // order the pre-refactor single bz→by→bx loop produced), so a linear walk over the emitted
    // index reproduces the pool and identity slot_map byte-for-byte.
    let mut pool: Vec<f32> = Vec::with_capacity(allocated * (bd * bd * bd) as usize);
    let mut slot_map = std::collections::HashMap::with_capacity(allocated);
    for (bi, &h) in index.iter().enumerate() {
        if h >= BRICK_TAG_INTERIOR { continue; } // sentinel — no data
        let bi = bi as u32;
        let bx = bi % dims[0];
        let by = (bi / dims[0]) % dims[1];
        let bz = bi / (dims[0] * dims[1]);
        slot_map.insert(h, h as usize);
        for lz in 0..bd {
            for ly in 0..bd {
                for lx in 0..bd {
                    pool.push(g(bx * bd + lx, by * bd + ly, bz * bd + lz));
                }
            }
        }
    }

    SparseSdfBricks { index, pool, slot_map, dims_bricks: dims, brick_dim: bd }
}

// ============================================================
// Shared sparse-classifier passes (grid + prims Pass-1 front-ends)
// ============================================================

/// Pass 2 (shared): dilate the surface brick set by its full 1-ring (26-neighbour) apron.
///
/// The apron must be the FULL 26-neighbourhood, not the 6 face neighbours: DC cells read voxels
/// ±1 past their brick and the 1.5-cell gradient stencil reaches the CORNER-DIAGONAL neighbour
/// brick (SPIKE A — 6-neighbour dilation leaves cracks at brick corners, proven by
/// `brick_accessor_apron_parity_adversarial`).
pub(super) fn dilate_surface_bricks_1ring(is_surface: &[bool], dims: [u32; 3]) -> Vec<bool> {
    let bidx = |bx: u32, by: u32, bz: u32| (bx + by * dims[0] + bz * dims[0] * dims[1]) as usize;
    let mut store = is_surface.to_vec();
    for bz in 0..dims[2] {
        for by in 0..dims[1] {
            for bx in 0..dims[0] {
                if !is_surface[bidx(bx, by, bz)] { continue; }
                for dz in -1i32..=1 {
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            let (nx, ny, nz) = (bx as i32 + dx, by as i32 + dy, bz as i32 + dz);
                            if nx < 0 || ny < 0 || nz < 0
                                || nx >= dims[0] as i32 || ny >= dims[1] as i32 || nz >= dims[2] as i32 { continue; }
                            store[bidx(nx as u32, ny as u32, nz as u32)] = true;
                        }
                    }
                }
            }
        }
    }
    store
}

/// Pass 3 (shared): emit the sparse index table from per-brick (store, interior) flags.
///
/// Stored bricks get dense-packed handles `0..allocated` in linear scan order (bx fastest —
/// identity slot_map semantics); everything else is tagged [`BRICK_TAG_INTERIOR`] (`-far`) or
/// [`BRICK_TAG_EXTERIOR`] (`+far`). `store` wins over `interior` (an apron brick may be fully
/// inside — it still needs real data). Returns `(index, allocated)`.
pub(super) fn emit_sparse_index(store: &[bool], interior: &[bool]) -> (Vec<u32>, usize) {
    debug_assert_eq!(store.len(), interior.len());
    let mut index = vec![BRICK_TAG_EXTERIOR; store.len()];
    let mut slot = 0u32;
    for bi in 0..store.len() {
        if store[bi] {
            index[bi] = slot;
            slot += 1;
        } else if interior[bi] {
            index[bi] = BRICK_TAG_INTERIOR; // inside → -far
        }
        // else: stays EXTERIOR → +far
    }
    (index, slot as usize)
}

// ============================================================
// PRE-BAKE sparse classification from PRIMITIVES (Phase 3A)
// ============================================================

/// Safety factor applied to the brick diagonal for the prim-sampled classifier margin.
///
/// The 9-point sample set has covering radius = half the brick EDGE, so a Lipschitz-1 field only
/// needs `edge/2` of margin. Smooth booleans, displace noise, chamfer/stairs blend profiles and
/// modifier warps make the composite field NON-Lipschitz (the March-Loop-Safety hazard: blend
/// zones tunnel) — a bare Lipschitz-1 bound WILL miss surface. 2.0 × the full diagonal gives
/// ~7× headroom over the Lipschitz bound. Precedents for inflated classifier margins:
/// `brick_classify.rs` (1.5 × diag) and `brick_allocator.rs` (2.5 × diag) in
/// `app/src/body_volume_cache/`.
pub const NONLIPSCHITZ_MARGIN_FACTOR: f32 = 2.0;

/// Bricks per axis for a cubic `nverts` grid split into `brick_dim`-voxel bricks — the ONE
/// formula shared by [`BrickClassifier`], the grid classifier and the GPU-classify dispatch
/// (which must size its verdict buffer/dispatch to the exact same brick grid).
pub fn bricks_per_axis(nverts: u32, brick_dim: u32) -> u32 {
    (nverts + brick_dim - 1) / brick_dim
}

/// The non-Lipschitz classifier margin ([`NONLIPSCHITZ_MARGIN_FACTOR`] × brick diagonal, world
/// units) — the EXACT expression order [`BrickClassifier::new`] uses (it calls this), exposed so
/// the GPU-classify dispatch can bit-pass the identical threshold to the shader instead of
/// duplicating the formula (REUSE rule; drift here silently flips near-threshold verdicts).
pub fn classifier_margin(grid_min: Vec3, grid_max: Vec3, nverts: u32, brick_dim: u32) -> f32 {
    let denom = if nverts > 1 { (nverts - 1) as f32 } else { 1.0 };
    let extent = grid_max - grid_min;
    NONLIPSCHITZ_MARGIN_FACTOR * ((extent / denom) * brick_dim as f32).length()
}

/// Bricks per axis of a hierarchical-classify SUPERBLOCK (4×4×4 bricks = 32³ voxels at bd 8;
/// a 512³ grid = 64³ bricks = 16³ superblocks). The value only affects SPEED, never output —
/// a proven superblock hands its bricks the exact flags the per-brick path would compute (see
/// [`BrickClassifier::superblock_verdicts`] for the soundness argument).
const SUPERBLOCK_BRICKS: u32 = 4;

/// Sparse brick INDEX without voxel data — the ACTIVE brick set decided BEFORE any bake exists.
/// The GPU bake fills the pool; handles are dense-packed `0..allocated` in scan order (identity
/// slot_map semantics, same contract as [`SparseSdfBricks`]).
#[derive(Clone)]
pub struct SparseIndexOnly {
    pub index: Vec<u32>,
    pub dims_bricks: [u32; 3],
    /// Pool watermark: every live handle is `< allocated` (pool sized `allocated × 512` f32).
    pub allocated: usize,
    /// S2.5f — handles RECLAIMED by the incremental walk (a surface brick the drag vacated:
    /// its needed class flipped to strict Interior/Exterior, so the tag went back to a sentinel
    /// and the pool slot is free). New allocations pop from here (LIFO, deterministic) before
    /// extending `allocated` — the pool stops growing during drags instead of leaking one brick
    /// per vacated surface cell until the next full re-classify. A fresh full classify
    /// ([`classify_prims_to_sparse_index`]) resets this to empty (its handles are dense-packed).
    pub freed: Vec<u32>,
}

/// Per-brick outcome of the 9-sample Pass-1 predicate ([`BrickClassifier::classify_one_brick`]).
///
/// `Surface` wins when a brick satisfies BOTH the (margin-inflated) surface test and the
/// interior test — [`emit_sparse_index`] gives such a brick a pool handle anyway (`store` wins
/// over `interior`, and `store` ⊇ surface since dilation only ADDS bricks), so resolving
/// Surface-first preserves the emitted index bit-for-bit vs the pre-refactor two-flag form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrickClass {
    /// Surface-possible: `min(9) <= band + MARGIN && max(9) >= -(band + MARGIN)`.
    Surface,
    /// CLEARLY inside — not surface-possible, all 9 samples `< -(band + MARGIN)`.
    Interior,
    /// Neither — clearly outside.
    Exterior,
}

/// Reusable per-brick 9-sample classifier — the SINGLE implementation of the Pass-1 predicate,
/// shared by [`classify_prims_to_sparse_index`] (full classify, rayon over all bricks) and
/// [`sparse_window_tags_compatible`] (S2.5b window mini-classify, sentinel bricks only).
/// Extracted (not duplicated) so the two callers can NEVER drift: same 9 sample points, same
/// endpoint-inclusive vertex→world mapping, same non-Lipschitz margin, same thresholds.
/// See [`classify_prims_to_sparse_index`] for the sampling/margin rationale and caller contract
/// (mirror-expanded bounds, bake-identical `nverts`/`band`).
pub struct BrickClassifier<'a> {
    visible: Vec<&'a crate::primitives::SdfPrimitive>,
    /// Per-visible-prim `(position, bounding_radius)` — precomputed ONCE (the radius walks the
    /// modifier stack) for the sphere-containment PROOF (`provably_exterior`). A prim whose
    /// field is spatially unbounded (`has_unbounded_field`, e.g. sculpt layers — feature on
    /// ice but the guard is a soundness backstop) gets `f32::INFINITY`: the proof can never
    /// clear it, so such prims always fall through to the exact 9-eval classify.
    influence: Vec<(Vec3, f32)>,
    /// P1-#6c interior-proof DONORS: `(position, inscribed_radius)` of every ADDITIVE
    /// (category 0) visible prim whose inscribed sphere is usable (`inscribed_radius() > 0` —
    /// pristine prims only; see the donor contract on `SdfPrimitive::inscribed_radius`).
    /// A brick deep enough inside ANY donor is deep inside the whole union chain (unions —
    /// hard, smooth or chamfer — only pull the field DOWN), provided no carver reaches it.
    donors: Vec<(Vec3, f32)>,
    /// Influence spheres of the SUBTRACTIVE (category 1) prims only — a subtract carves ONLY
    /// within its influence sphere (+ blend k, already inside `bounding_radius`), so a brick
    /// outside every carver's reach (in every mirror combo) keeps the donor's depth.
    carvers: Vec<(Vec3, f32)>,
    /// False when any INTERSECTIVE (category 2) prim exists: intersection carves EVERYWHERE
    /// outside the intersector — the "outside influence" logic INVERTS, so interior proofs are
    /// disabled body-wide (bricks fall through to the exact 9-eval classify, as before P1-#6c).
    interior_enabled: bool,
    /// Scope-aware field program (nested sub-body boolean trees). `None` = flat semantics
    /// (`eval_scene_body_mirrored` over `visible` — the historical path, and still what every
    /// prim-slice caller gets). When set via [`with_field`](Self::with_field), the 9-eval
    /// classify samples THIS field — the same field the GPU bake writes for nested bodies.
    field: Option<crate::field_eval::BodyField>,
    /// False when the flat sphere-containment proofs are UNSOUND for the attached field
    /// (nested non-Union scopes / scope blend k / nested mirror / bevel — see
    /// `BodyField::sphere_proofs_valid`). All bricks then fall through to the exact 9-eval
    /// classify: conservative, never wrong.
    proofs_enabled: bool,
    mirror_flags: u8,
    /// Mirror flip + cut bits (`body_field_ops::MIRROR_CUT_BIT`). 0 = the legacy union, which
    /// is what every field-less caller (tests, flat prim slices) gets. Adopted automatically
    /// from the attached field in [`with_field`](Self::with_field) — the field is the
    /// authority on the body's mirror semantics, so the proofs can never disagree with the
    /// eval they must dominate.
    mirror_sign: u8,
    mirror_center: Vec3,
    grid_min: Vec3,
    extent: Vec3,
    denom: f32,
    nverts: u32,
    brick_dim: u32,
    band: f32,
    margin: f32,
    dims: [u32; 3],
}

impl<'a> BrickClassifier<'a> {
    /// Same argument contract as [`classify_prims_to_sparse_index`].
    pub fn new(
        prims: &'a [crate::primitives::SdfPrimitive],
        mirror_flags: u8,
        mirror_center: Vec3,
        grid_min: Vec3,
        grid_max: Vec3,
        nverts: u32,
        brick_dim: u32,
        band: f32,
    ) -> Self {
        let bd = brick_dim;
        let bpa = bricks_per_axis(nverts, bd);
        // Endpoint-inclusive vertex→world mapping — identical to the dense bake oracle.
        let denom = if nverts > 1 { (nverts - 1) as f32 } else { 1.0 };
        let extent = grid_max - grid_min;
        let visible: Vec<&crate::primitives::SdfPrimitive> =
            prims.iter().filter(|p| p.visible).collect();
        // Precompute the per-prim influence spheres ONCE — bounding_radius() walks the modifier
        // stack, so paying it per brick × 9 in provably_exterior would undo the proof's cheapness.
        // Unbounded fields (sculpt layers — on ice, soundness backstop) disable the proof for
        // that prim via an infinite radius.
        let influence: Vec<(Vec3, f32)> = visible.iter()
            .map(|p| (p.position, if p.has_unbounded_field() { f32::INFINITY } else { p.bounding_radius() }))
            .collect();
        // P1-#6c interior-proof precompute (see the field docs): donors from pristine additive
        // prims, carvers = the subtractive prims' influence spheres (radius shared with the
        // exterior proof — an unbounded carver's INFINITY correctly blocks all interior proofs
        // within... everywhere), enabled only in intersect-free bodies.
        let donors: Vec<(Vec3, f32)> = visible.iter()
            .filter(|p| p.operation.category() == 0)
            .filter_map(|p| {
                let r = p.inscribed_radius();
                (r > 0.0).then(|| (p.position, r))
            })
            .collect();
        // Category 1 (Subtract family), category 3 (Seam) AND category 4 (Extrude): a seam
        // GROOVE carves up to `depth` below the pristine additive surface, entirely inside
        // the seam prim's influence sphere (bounding_radius pads by width + depth) — so it
        // blocks interior proofs exactly like a subtract. (A raised seam only ADDS near the
        // surface; treating it as a carver too is merely conservative.) An extrude RECESS
        // (strength<0) likewise raises the chain value by up to |strength|, entirely inside
        // its influence sphere (bounding_radius pads by width + |strength|); a raise only
        // pulls the chain further down, so covering both signs is correct-or-conservative.
        // The EXTERIOR proof needs no extrude special case: the field only changes where
        // b < width — inside the prim's influence sphere, which is already in `influence`.
        let carvers: Vec<(Vec3, f32)> = visible.iter().zip(influence.iter())
            .filter(|(p, _)| matches!(p.operation.category(), 1 | 3 | 4))
            .map(|(_, inf)| *inf)
            .collect();
        let interior_enabled = !visible.iter().any(|p| p.operation.category() == 2);
        Self {
            visible,
            influence,
            donors,
            carvers,
            interior_enabled,
            field: None,
            proofs_enabled: true,
            mirror_flags,
            mirror_sign: 0,
            mirror_center,
            grid_min,
            extent,
            denom,
            nverts,
            brick_dim: bd,
            band,
            margin: classifier_margin(grid_min, grid_max, nverts, bd),
            dims: [bpa, bpa, bpa],
        }
    }

    /// Attach the SCOPE-AWARE field program of the body subtree (built via
    /// `BodyField::from_graph` in the SAME body-local frame as `prims`/`grid_min`). The
    /// 9-eval classify then samples the nested boolean tree the GPU bake writes, instead of
    /// the flat prim chain. The sphere-containment proofs (built from the FLAT prim set)
    /// stay enabled only when they remain sound for this field
    /// ([`BodyField::sphere_proofs_valid`]); otherwise every brick falls through to the
    /// exact classify — strictly conservative. For a body with NO nested children the field
    /// evaluates the identical `fold_prim` chain as the flat path (same code, bit-identical
    /// verdicts) and all proofs stay on.
    pub fn with_field(mut self, field: crate::field_eval::BodyField) -> Self {
        let sound = field.sphere_proofs_valid();
        self.proofs_enabled = sound;
        self.interior_enabled &= sound;
        // The field's root scope carries the authoritative mirror mode (union vs cut). Adopting
        // it here is what keeps the containment proofs dominating the eval they gate: under a
        // CUT the identity combo is NOT evaluated, so a proof that assumed it would be unsound.
        self.mirror_sign = field.root.mirror_sign;
        self.field = Some(field);
        self
    }

    /// Field-less callers (flat prim slices) that need the CUT mirror semantics in the proofs.
    /// `with_field` sets this automatically; this is the explicit override for the flat path.
    pub fn with_mirror_sign(mut self, mirror_sign: u8) -> Self {
        self.mirror_sign = mirror_sign;
        self
    }

    /// True when the body mirror deletes the non-source half (one fold) instead of unioning
    /// both sides — see `body_field_ops::MIRROR_CUT_BIT`.
    #[inline]
    fn mirror_is_cut(&self) -> bool {
        self.mirror_flags != 0 && crate::body_field_ops::mirror_cut_enabled(self.mirror_sign)
    }

    /// THE field the classify samples: the scope-aware program when attached, else the flat
    /// visible-prim chain — both apply the body mirror as a true union.
    #[inline]
    fn eval_field(&self, p: Vec3) -> f32 {
        match &self.field {
            Some(f) => f.eval_or(p, 100.0),
            None => crate::field_eval::eval_scene_body_mirrored(
                p, &self.visible, self.mirror_flags, self.mirror_sign, self.mirror_center),
        }
    }

    pub fn dims_bricks(&self) -> [u32; 3] {
        self.dims
    }

    /// Endpoint-inclusive vertex→world mapping — identical to the dense bake oracle. The ONE
    /// implementation shared by the classify and both containment proofs: the parity gates pin
    /// bit-identical coordinates, so a drifted copy of this expression would silently unpin them.
    fn vertex_world(&self, g: [f32; 3]) -> Vec3 {
        self.grid_min + Vec3::new(g[0], g[1], g[2]) / self.denom * self.extent
    }

    /// Clamped vertex span of the INCLUSIVE brick range `[b_lo, b_hi]` — exactly the voxels the
    /// dense oracle samples for those bricks (padding voxels past `nverts` read +far there and
    /// are never surface). A single brick passes `b_lo == b_hi`; the superblock pass passes its
    /// corner bricks. Returns `(v0, v1)` in vertex coordinates.
    fn brick_span_verts(&self, b_lo: [u32; 3], b_hi: [u32; 3]) -> ([f32; 3], [f32; 3]) {
        let bd = self.brick_dim;
        let last = self.nverts - 1;
        let v0 = [
            (b_lo[0] * bd).min(last) as f32,
            (b_lo[1] * bd).min(last) as f32,
            (b_lo[2] * bd).min(last) as f32,
        ];
        let v1 = [
            (b_hi[0] * bd + bd - 1).min(last) as f32,
            (b_hi[1] * bd + bd - 1).min(last) as f32,
            (b_hi[2] * bd + bd - 1).min(last) as f32,
        ];
        (v0, v1)
    }

    /// World-space bounding ball `(center, half_diag)` of an inclusive brick range's clamped
    /// vertex span — the region argument of the containment proofs. The clamp makes a PARTIAL
    /// edge superblock's ball cover exactly its real bricks, never voxels past the grid.
    fn brick_span_ball(&self, b_lo: [u32; 3], b_hi: [u32; 3]) -> (Vec3, f32) {
        let (v0, v1) = self.brick_span_verts(b_lo, b_hi);
        let c0 = self.vertex_world(v0);
        let c1 = self.vertex_world(v1);
        ((c0 + c1) * 0.5, (c1 - c0).length() * 0.5)
    }

    /// Classify ONE brick from 9 analytic samples (8 corners + center) of the exact bake field —
    /// the Pass-1 per-brick body of [`classify_prims_to_sparse_index`], verbatim.
    pub fn classify_one_brick(&self, bx: u32, by: u32, bz: u32) -> BrickClass {
        // Vertex span of this brick, clamped to the grid (see `brick_span_verts`).
        let (v0, v1) = self.brick_span_verts([bx, by, bz], [bx, by, bz]);

        let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
        for c in 0..8u32 {
            let corner = [
                if c & 1 != 0 { v1[0] } else { v0[0] },
                if c & 2 != 0 { v1[1] } else { v0[1] },
                if c & 4 != 0 { v1[2] } else { v0[2] },
            ];
            let d = self.eval_field(self.vertex_world(corner));
            mn = mn.min(d);
            mx = mx.max(d);
        }
        let center = [(v0[0] + v1[0]) * 0.5, (v0[1] + v1[1]) * 0.5, (v0[2] + v1[2]) * 0.5];
        let d = self.eval_field(self.vertex_world(center));
        mn = mn.min(d);
        mx = mx.max(d);

        if mn <= self.band + self.margin && mx >= -(self.band + self.margin) {
            BrickClass::Surface
        } else if mx <= -self.margin {
            BrickClass::Interior
        } else {
            BrickClass::Exterior
        }
    }

    /// S2.5f reclaim PROOF (cheap): is this brick PROVABLY strict-Exterior by bounding-sphere
    /// containment alone — no field evals? True when every point of the brick is farther than
    /// `band + margin` from every visible prim's bounding sphere, for EVERY active mirror combo
    /// (the mirrored field is the min over reflected sample points, so all combos must clear).
    ///
    /// The lower bound `d(p) ≥ |p − pos| − radius` is a CONTAINMENT CONTRACT on
    /// `bounding_radius()` (shape extent + scale + elongation + blend k + the full modifier-STACK
    /// pad + prim mirror folds — see `SdfPrimitive::modifier_pad`), pinned by the
    /// `bounding_radius_contains_world_surface` property test. Unlike `eval_scene`'s COMPARATIVE
    /// cull (which degrades to blend artifacts when the bound is tight), this proof is ABSOLUTE —
    /// an under-bound here drops bricks (holes), which is why unbounded fields (sculpt layers)
    /// get an infinite radius and always fall through to the exact classify. Strictly more
    /// conservative than the 9-sample classify (a brick this proves Exterior, the classify would
    /// also call Exterior — never the reverse). Cost: `prims × 2^mirror` distance checks per
    /// brick vs `9 × prims` field evals — the difference between reclaiming the vacated handle
    /// sheet for free and doubling the drag trigger cost (owner-measured 15-23ms/frame when
    /// reclaim classified every window handle).
    pub fn provably_exterior(&self, bx: u32, by: u32, bz: u32) -> bool {
        let (center, half_diag) = self.brick_span_ball([bx, by, bz], [bx, by, bz]);
        self.provably_exterior_region(center, half_diag)
    }

    /// Proof core of [`provably_exterior`], generalized to ANY region given by its bounding
    /// ball `(center, half_diag)` — the proof is SCALE-AGNOSTIC: every point of the region is
    /// beyond `band + margin` of every influence sphere when the center clears each sphere by
    /// `radius + band + margin + half_diag`. `margin` stays the BRICK-scale non-Lipschitz
    /// margin ([`NONLIPSCHITZ_MARGIN_FACTOR`] × brick_diag) at every region size: it exists to
    /// bound the 9-sample undersampling of a BRICK, and this proof takes no samples at all —
    /// only containment — so brick-margin + region-half-diag is correct and conservative for
    /// a superblock too (the per-brick classify the proof must dominate never changes scale).
    /// Callers: the per-brick wrapper and the superblock pass ([`superblock_verdicts`]) — ONE
    /// proof body, so brick and superblock scales can never drift.
    pub fn provably_exterior_region(&self, center: Vec3, half_diag: f32) -> bool {
        if !self.proofs_enabled {
            return false; // nested non-Union/mirrored/beveled scopes — flat spheres unsound
        }
        let clearance = self.band + self.margin + half_diag;
        !self.any_sphere_reaches(center, clearance, &self.influence)
    }

    /// Shared reach test of both region proofs: does ANY sphere of `spheres` (the influence or
    /// carver set — `(position, radius)`, radius modifier-STACK-padded and INFINITE for
    /// unbounded fields, which can never be proven away) come within `clearance` of `center`
    /// under ANY active mirror combo? Mirror combos reflect the region CENTER exactly as
    /// `eval_scene_body_mirrored` reflects its sample point; bit k of the combo mirrors axis k
    /// about `mirror_center`.
    ///
    /// CUT mirror: the field at `q` is the chain at `fold(q)` and NOTHING else, so only the
    /// FOLDED center is tested. `fold` is 1-Lipschitz, so `fold(region) ⊆ ball(fold(center),
    /// half_diag)` and the same clearance bounds it. Testing one point instead of 2ⁿ is both
    /// sound (a strict subset of the combo positions, which the union mode must cover) and
    /// strictly stronger — more regions prove out, fewer bricks fall through to the 9-eval
    /// classify.
    fn any_sphere_reaches(&self, center: Vec3, clearance: f32, spheres: &[(Vec3, f32)]) -> bool {
        if self.mirror_is_cut() {
            let p = crate::field_eval::fold_for_cut(
                center, self.mirror_flags, self.mirror_sign, self.mirror_center);
            return spheres.iter().any(|&(pos, radius)| (p - pos).length() <= radius + clearance);
        }
        let combos = 1u8 << self.mirror_flags.count_ones();
        for combo in 0..combos {
            let mut p = center;
            let mut bit = 0u32;
            for axis in 0..3usize {
                if self.mirror_flags & (1 << axis) != 0 {
                    if combo & (1 << bit) != 0 {
                        p[axis] = 2.0 * self.mirror_center[axis] - p[axis];
                    }
                    bit += 1;
                }
            }
            for &(pos, radius) in spheres {
                if (p - pos).length() <= radius + clearance {
                    return true; // within reach — no containment proof possible
                }
            }
        }
        false
    }

    /// P1-#6c reclaim/prefilter PROOF (cheap): is this brick PROVABLY strict-Interior by
    /// sphere containment alone — no field evals? The dual of [`provably_exterior`], for the
    /// scale-up case it left open: when a prim GROWS, its old surface bricks end up deep
    /// INSIDE the solid where the exterior proof can never fire, so their handles leaked until
    /// the next full classify (owner-measured pool 41k→153k bricks in one scale gesture).
    ///
    /// True when BOTH hold:
    /// 1. ∃ DONOR (pristine additive prim, `inscribed_radius() > 0`) whose inscribed sphere
    ///    contains the whole brick with `band + margin` to spare —
    ///    `|center − pos| + clearance ≤ r_in`. By the donor contract every brick point is then
    ///    ≥ `band+margin` DEEP inside that prim, and union arms (hard/smooth/chamfer smin all
    ///    ≤ min) can only pull the chain value further down.
    /// 2. ∀ mirror combos, ∀ CARVERS (subtractive prims): the reflected brick center clears
    ///    the carver's influence sphere by `clearance` — a subtract only carves within its
    ///    influence (+ blend k, inside `bounding_radius`), so the donor's depth survives.
    ///    Intersective prims disable the proof body-wide (`interior_enabled`).
    ///
    /// Together: the chain value ≤ −(band+margin) at every brick point ⟹ `classify_one_brick`
    /// would verdict Interior (its `max(9) ≤ −(band+margin) < −margin`, and surface-possible
    /// needs `max ≥ −(band+margin)`) — STRICTLY conservative, so the prefilter stays
    /// bit-identical to the exact classify. Crack safety mirrors the exterior proof's
    /// argument: a face-neighbour brick can classify Surface only CONSERVATIVELY (its true
    /// field is ≤ −band everywhere by 1-Lipschitz reach, margin = 2·brick-diag), so it holds
    /// no real crossing and the DC reads consistent negative signs across the seam.
    pub fn provably_interior(&self, bx: u32, by: u32, bz: u32) -> bool {
        let (center, half_diag) = self.brick_span_ball([bx, by, bz], [bx, by, bz]);
        self.provably_interior_region(center, half_diag)
    }

    /// Proof core of [`provably_interior`], generalized to ANY region given by its bounding
    /// ball `(center, half_diag)` — scale-agnostic for the same reason as
    /// [`provably_exterior_region`] (containment only, no samples; `margin` stays the
    /// BRICK-scale non-Lipschitz margin that bounds a brick's 9-sample undersampling).
    /// Callers: the per-brick wrapper and the superblock pass ([`superblock_verdicts`]).
    pub fn provably_interior_region(&self, center: Vec3, half_diag: f32) -> bool {
        if !self.interior_enabled || self.donors.is_empty() {
            return false;
        }
        let clearance = self.band + self.margin + half_diag;

        // 1) Donor depth — the UNREFLECTED region center (the identity mirror combo evaluates
        // the chain at the original position; the body mirror is a min over combos and can
        // only deepen further).
        //
        // CUT mirror: the identity combo is NOT evaluated — the chain is sampled at `fold(q)`
        // and only there — so the unreflected center proves nothing (a brick deep inside a
        // donor on the DISCARDED side would falsely prove Interior and plant solid where the
        // cut removed material). Test the FOLDED center instead: `fold` is 1-Lipschitz, so
        // every point of the region lands inside `ball(fold(center), half_diag)`, which the
        // same clearance covers.
        let probe = if self.mirror_is_cut() {
            crate::field_eval::fold_for_cut(
                center, self.mirror_flags, self.mirror_sign, self.mirror_center)
        } else {
            center
        };
        if !self.donors.iter().any(|&(pos, r_in)| (probe - pos).length() + clearance <= r_in) {
            return false;
        }
        // 2) Carver reach — every mirror combo of the region center must clear every carver
        // (same combo reflection as the exterior proof: a mirrored subtract carves at the
        // reflected position too).
        !self.any_sphere_reaches(center, clearance, &self.carvers)
    }

    /// Hierarchical Pass 0 of [`classify_prims_to_sparse_index`]: the SAME two containment
    /// proofs, run once per [`SUPERBLOCK_BRICKS`]³-brick superblock instead of per brick.
    /// Returns `(verdicts, sdims)`: `verdicts[si] = Some(flags)` means EVERY brick of
    /// superblock `si` takes the Pass-1 `(surface, interior)` flags `flags` with zero
    /// per-brick work; `None` means the superblock straddles the field and its bricks fall
    /// through to the per-brick path. rayon-parallel; superblocks are independent →
    /// order-invariant output.
    ///
    /// SOUNDNESS — why a superblock verdict is FLAG-IDENTICAL to the per-brick path:
    /// - Every point of every member brick (in particular all 9 of its classify samples, which
    ///   lie in the brick's clamped vertex AABB) is inside the superblock's clamped AABB ⊆ its
    ///   bounding ball ([`brick_span_ball`]; partial edge superblocks clamp `b_hi` to the real
    ///   brick range, so the ball covers exactly their bricks).
    /// - Proven EXTERIOR ⟹ every such point is > band+margin from every influence sphere in
    ///   every mirror combo ⟹ the brick's own path lands on (false, false): its
    ///   `provably_exterior` fires, or `classify_one_brick` reads `mn > band + margin` →
    ///   Exterior. (The argument goes through SAMPLE containment, not ball-in-ball: a corner
    ///   brick's own ball can poke past a PARTIAL superblock's ball, so the per-brick proof
    ///   itself is not always implied — the classify verdict is.) `provably_interior` cannot
    ///   fire on such a brick: its center is beyond every donor's bounding ⊇ inscribed sphere,
    ///   so the donor-depth arm fails.
    /// - Proven INTERIOR ⟹ every such point is ≥ band+margin DEEP inside a donor
    ///   (`|q − pos| ≤ |sb_center − pos| + sb_half_diag ≤ r_in − (band + margin)`; union arms
    ///   only pull the chain down) and beyond every carver's reach in every combo ⟹ the brick
    ///   lands on (false, true): its `provably_interior` fires, or `classify_one_brick` reads
    ///   `mx ≤ −(band + margin)` → Interior. `provably_exterior` cannot fire — the brick
    ///   center sits inside the donor's bounding sphere.
    /// - `margin` stays the BRICK-scale non-Lipschitz margin in both proofs (see
    ///   [`provably_exterior_region`]): it bounds a BRICK's 9-sample undersampling; the
    ///   superblock proof needs no samples, only containment.
    /// Pinned by `hierarchical_classify_parity` (byte-identity of the emitted index vs the
    /// pure per-brick pipeline).
    pub(super) fn superblock_verdicts(&self) -> (Vec<Option<(bool, bool)>>, [u32; 3]) {
        use rayon::prelude::*;
        let dims = self.dims;
        let sb = SUPERBLOCK_BRICKS;
        let sdims = [
            (dims[0] + sb - 1) / sb,
            (dims[1] + sb - 1) / sb,
            (dims[2] + sb - 1) / sb,
        ];
        let stotal = (sdims[0] * sdims[1] * sdims[2]) as usize;
        let verdicts: Vec<Option<(bool, bool)>> = (0..stotal).into_par_iter().map(|si| {
            let si = si as u32;
            let sx = si % sdims[0];
            let sy = (si / sdims[0]) % sdims[1];
            let sz = si / (sdims[0] * sdims[1]);
            self.superblock_verdict_one([sx, sy, sz])
        }).collect();
        (verdicts, sdims)
    }

    /// ONE superblock's containment verdict — the per-superblock body of
    /// [`superblock_verdicts`] (see its soundness argument), extracted so the frozen-partial
    /// window walk ([`sparse_window_allocate_incompatible`]) can prove window superblocks with
    /// the SAME code (REUSE rule — brick/superblock scales can never drift). `s` is the
    /// superblock coordinate on the GRID-ALIGNED lattice (`brick / SUPERBLOCK_BRICKS`).
    fn superblock_verdict_one(&self, s: [u32; 3]) -> Option<(bool, bool)> {
        let dims = self.dims;
        let sb = SUPERBLOCK_BRICKS;
        let b_lo = [s[0] * sb, s[1] * sb, s[2] * sb];
        let b_hi = [
            (s[0] * sb + sb - 1).min(dims[0] - 1),
            (s[1] * sb + sb - 1).min(dims[1] - 1),
            (s[2] * sb + sb - 1).min(dims[2] - 1),
        ];
        let (center, half_diag) = self.brick_span_ball(b_lo, b_hi);
        if self.provably_exterior_region(center, half_diag) {
            Some((false, false)) // Exterior — no surface, not interior
        } else if self.provably_interior_region(center, half_diag) {
            Some((false, true)) // Interior — no surface
        } else {
            None // straddles the field — per-brick path
        }
    }
}

/// PRE-BAKE sparse brick classifier: decide the ACTIVE (surface + apron) brick set directly from
/// PRIMITIVES, before any dense grid exists — Phase 3A needs the allocation up front so the GPU
/// bake can write straight into a sparse pool. Index-only: no voxel data is produced.
///
/// **Sampling (why 9 points + margin):** each brick is classified from 9 analytic SDF samples
/// (8 corners + center) of the exact bake field, [`eval_scene_body_mirrored`] — the same eval the
/// dense bake oracle (`bake_body_grid`) uses, with the same endpoint-inclusive vertex→world
/// mapping (`vertex g → grid_min + g/(nverts-1) · extent`). 9 samples cannot bound a brick's true
/// min/max, so both tests are inflated by `MARGIN = `[`NONLIPSCHITZ_MARGIN_FACTOR`]` ×
/// brick_diagonal`: smooth booleans / noise / blend profiles are NON-Lipschitz, and a bare
/// Lipschitz-1 bound (half the brick edge) WILL miss bulging blend surfaces.
///
/// - surface-possible: `min(9) <= band + MARGIN && max(9) >= -(band + MARGIN)`
/// - interior: `max(9) <= -MARGIN` (only CLEARLY-inside bricks get the `-far` sentinel)
///
/// **Conservative bias:** when unsure, a brick is surface-possible. A false-positive surface
/// brick merely over-allocates pool memory (the bake fills it with valid off-surface data); a
/// false INTERIOR or EXTERIOR tag replaces real data with a constant `∓far` → sign flips at the
/// brick boundary → HOLES or phantom walls in the DC mesh. Gate:
/// `prims_classifier_superset_of_grid_classifier` proves every brick the post-bake grid
/// classifier ([`classify_grid_to_sparse`]) stores is also stored here.
///
/// **Dilation:** the surface set is dilated by the full 26-neighbour 1-ring (shared Pass 2) —
/// DC corner reads + the 1.5-cell gradient stencil reach the corner-DIAGONAL neighbour brick;
/// 6-neighbour dilation cracks at brick corners (SPIKE A).
///
/// **Caller contract:** `grid_min`/`grid_max` MUST be the body's MIRROR-EXPANDED bake bounds
/// (Body Mirror AABB rule — `expand_aabb_for_mirror`): the mirror is a true union, so a tight
/// AABB puts the reflected half outside every sampled brick and kills it. Same bounds/res/band
/// as the bake that will fill the pool.
///
/// rayon-parallel over bricks; per-brick results are independent → order-invariant output.
pub fn classify_prims_to_sparse_index(
    prims: &[crate::primitives::SdfPrimitive],
    mirror_flags: u8,
    mirror_center: Vec3,
    grid_min: Vec3,
    grid_max: Vec3,
    nverts: u32,
    brick_dim: u32,
    band: f32,
) -> SparseIndexOnly {
    let clf = BrickClassifier::new(
        prims, mirror_flags, mirror_center, grid_min, grid_max, nverts, brick_dim, band,
    );
    classify_with_classifier(&clf)
}

/// The classify PASSES of [`classify_prims_to_sparse_index`], over an already-built
/// classifier — lets scope-aware callers attach a `BodyField` (`with_field`) before
/// classifying, without a second copy of the pipeline. `classify_prims_to_sparse_index`
/// (flat, unchanged signature) routes through here.
pub fn classify_with_classifier(clf: &BrickClassifier) -> SparseIndexOnly {
    use rayon::prelude::*;

    let dims = clf.dims_bricks();
    let total = (dims[0] * dims[1] * dims[2]) as usize;

    // Pass 0 (hierarchical): prove whole 4×4×4-brick SUPERBLOCKS Exterior/Interior with the
    // same containment proofs at superblock scale — see `superblock_verdicts` for the
    // soundness argument (flag-identical output, superblocks only skip work). Even the cheap
    // per-brick proofs are ~262k × prims × combos distance checks at 512³ (owner-measured
    // 23-33ms for the full classify); this pass drops that to ~4k proof pairs plus per-brick
    // work only where a superblock straddles the field.
    let (sb_verdicts, sdims) = clf.superblock_verdicts();

    // Pass 1 (prims): the shared 9-sample predicate ([`BrickClassifier::classify_one_brick`])
    // per brick → (surface, interior) flags. Each brick is independent → rayon-parallel,
    // order-invariant (same pattern as the grid Pass 1).
    //
    // BrickClass → flags note: a brick can satisfy BOTH raw predicates; `classify_one_brick`
    // resolves it Surface-first, so such a brick maps to (true, false) where the pre-refactor
    // form produced (true, true). Harmless — `emit_sparse_index` reads `interior` only for
    // bricks NOT in `store`, and `store` ⊇ surface (dilation only adds) — the emitted index is
    // bit-identical (pinned by `classify_one_brick_consistent_with_emitted_index` + the
    // superset gates).
    let flags: Vec<(bool, bool)> = (0..total).into_par_iter().map(|bi| {
        let bi = bi as u32;
        let bx = bi % dims[0];
        let by = (bi / dims[0]) % dims[1];
        let bz = bi / (dims[0] * dims[1]);
        // Pass 0 lookup: a proven superblock hands every member brick its flags directly —
        // flag-identical to the per-brick path below (see `superblock_verdicts`).
        let si = bx / SUPERBLOCK_BRICKS
            + (by / SUPERBLOCK_BRICKS) * sdims[0]
            + (bz / SUPERBLOCK_BRICKS) * sdims[0] * sdims[1];
        if let Some(proven) = sb_verdicts[si as usize] {
            return proven;
        }
        // Cheap bounding-sphere prefilter: a brick provably beyond every prim's influence sphere
        // (+ band + margin) is Exterior — skip the 9 field evals. A cube fills ~10% of its grid,
        // so this drops the ~90% empty bricks that dominate the full classify (owner-measured
        // ~100ms escape fulls at 512³). SOUND + bit-identical: provably_exterior ⟹ every point of
        // the brick, hence all 9 samples, is > band+margin from every prim ⟹ classify_one_brick
        // returns Exterior. The prefilter is `prims × 2^mirror` distance checks vs `9 × prims`
        // field evals; near-surface bricks fail the proof and fall through to the exact classify.
        if clf.provably_exterior(bx, by, bz) {
            return (false, false); // Exterior — no surface, not interior
        }
        // P1-#6c: the dual prefilter — a solid's DEEP-INTERIOR bricks are the other big
        // 9-eval population a plain cube pays for (the exterior prefilter only drops the
        // empty ~90%). Bit-identical: provably_interior ⟹ classify_one_brick = Interior.
        if clf.provably_interior(bx, by, bz) {
            return (false, true); // Interior — no surface
        }
        match clf.classify_one_brick(bx, by, bz) {
            BrickClass::Surface => (true, false),
            BrickClass::Interior => (false, true),
            BrickClass::Exterior => (false, false),
        }
    }).collect();

    // Pass 2 + Pass 3 (dilation + handle allocation) — the SHARED finalize, one implementation
    // for this CPU front-end and the GPU `classify_bricks` front-end (stage-2 wiring).
    sparse_index_from_flags(&flags, dims)
}

/// Dilation + handle-allocation FINALIZE shared by the two Pass-1 front-ends: the CPU classify
/// above ([`classify_prims_to_sparse_index`]) and the GPU `classify_bricks` verdict readback
/// (which decodes shader verdicts to [`BrickClass`] and calls [`sparse_index_from_verdicts`]).
/// ONE implementation — dilation/emission determinism is what makes the GPU-classified index
/// bit-comparable to a CPU classify of the same verdicts.
fn sparse_index_from_flags(flags: &[(bool, bool)], dims: [u32; 3]) -> SparseIndexOnly {
    debug_assert_eq!(flags.len(), (dims[0] * dims[1] * dims[2]) as usize);
    let is_surface: Vec<bool> = flags.iter().map(|f| f.0).collect();
    let interior: Vec<bool> = flags.iter().map(|f| f.1).collect();

    let store = dilate_surface_bricks_1ring(&is_surface, dims);
    let (index, allocated) = emit_sparse_index(&store, &interior);
    SparseIndexOnly { index, dims_bricks: dims, allocated, freed: Vec::new() }
}

/// GPU-classify finalize: per-brick [`BrickClass`] verdicts (decoded from the `classify_bricks`
/// shader output, brick-linear order `bx + by·dx + bz·dx·dy`) → dilated + allocated sparse
/// index. Same [`BrickClass`] → flags mapping as the CPU Pass 1 (Surface wins ties there too),
/// then the SHARED [`sparse_index_from_flags`] — so a verdict-identical GPU pass yields an
/// index BIT-IDENTICAL to [`classify_prims_to_sparse_index`].
pub fn sparse_index_from_verdicts(verdicts: &[BrickClass], dims: [u32; 3]) -> SparseIndexOnly {
    let flags: Vec<(bool, bool)> = verdicts.iter().map(|v| match v {
        BrickClass::Surface => (true, false),
        BrickClass::Interior => (false, true),
        BrickClass::Exterior => (false, false),
    }).collect();
    sparse_index_from_flags(&flags, dims)
}

/// P1 scrolling-grid escape: TRANSLATE a frozen sparse index by an integer brick shift — the
/// pure kernel op behind the brick-aligned window scroll (the grid bounds move by exactly
/// `shift × brick_world` per axis, so every RETAINED brick's world-space voxel positions are
/// unchanged and its pool content stays valid; sentinels are per-brick constants and therefore
/// translation-invariant by construction).
///
/// Coordinate convention: `shift` is the window translation in BRICKS (new_grid_min =
/// old_grid_min + shift·brick_world). A world-fixed brick at old coord `b` lands at new coord
/// `b - shift`, so for every destination brick `d` the source is `s = d + shift`:
/// - `s` in range → the tag/handle copies verbatim (retained content).
/// - `s` out of range → [`BRICK_TAG_EXTERIOR`] — PROVISIONAL: the caller MUST cover the exposed
///   slab with the window re-classify ([`sparse_window_allocate_incompatible`]) before the next
///   mesh, which retags/allocates it from the live prims (the scroll itself never classifies).
/// - A handle whose destination `d = s - shift` falls out of range is SCROLLED OUT: its pool
///   slot is pushed onto `freed` (deterministic (z,y,x) scan order, matching the reclaim
///   convention) for the window-allocate to recycle — typically feeding the exposed slab's new
///   surface bricks, so the pool does not grow across a scroll.
///
/// `allocated` (the pool watermark) is unchanged: scrolled-out slots are FREED, not popped from
/// the arena. Returns a new [`SparseIndexOnly`]; the input is not mutated (the caller installs
/// the result atomically with the new grid bounds — they are meaningless apart).
pub fn sparse_index_translate(sp: &SparseIndexOnly, shift: [i32; 3]) -> SparseIndexOnly {
    let dims = sp.dims_bricks;
    let (dx, dy, dz) = (dims[0] as i64, dims[1] as i64, dims[2] as i64);
    let lin = |bx: i64, by: i64, bz: i64| (bx + by * dx + bz * dx * dy) as usize;
    let mut index = vec![BRICK_TAG_EXTERIOR; sp.index.len()];
    let mut freed = sp.freed.clone();
    // (z,y,x) scan of the SOURCE so scrolled-out handles free in deterministic scan order.
    for bz in 0..dz {
        for by in 0..dy {
            for bx in 0..dx {
                let tag = sp.index[lin(bx, by, bz)];
                let (nx, ny, nz) = (
                    bx - shift[0] as i64,
                    by - shift[1] as i64,
                    bz - shift[2] as i64,
                );
                let in_range =
                    nx >= 0 && nx < dx && ny >= 0 && ny < dy && nz >= 0 && nz < dz;
                if in_range {
                    index[lin(nx, ny, nz)] = tag;
                } else if tag < BRICK_TAG_INTERIOR {
                    freed.push(tag); // handle scrolled out of the window — recycle its slot
                }
            }
        }
    }
    SparseIndexOnly { index, dims_bricks: dims, allocated: sp.allocated, freed }
}

/// S2.5 (frozen-index partial sparse bake): dilate a voxel window by one brick (`brick_dim`
/// voxels) on each side, clamped to `[0, res)` per axis — the same expand-by-pad-then-clamp
/// pattern as the `atlas_pad` window dilation in the bake dispatch (`app_poll.rs` bake_min/
/// bake_max). `win_max`/the returned max are EXCLUSIVE, matching the bake window convention.
///
/// Purpose: a partial bake against a FROZEN brick index must verify that the surface did not
/// move into an unallocated (sentinel) brick. The dirty window covers everywhere the field
/// changed; dilating it by one brick makes the coverage check ([`sparse_window_bricks_all_handled`])
/// also see the 1-ring bricks the DC mesher reads around the moved surface — a formerly-
/// INTERIOR/EXTERIOR brick that should now be surface (or apron) falls inside the dilated
/// window, reads as a sentinel, and forces escalation to a full re-classify.
pub fn dilate_voxel_window(
    win_min: [u32; 3],
    win_max: [u32; 3],
    brick_dim: u32,
    res: [u32; 3],
) -> ([u32; 3], [u32; 3]) {
    let mut dmin = [0u32; 3];
    let mut dmax = [0u32; 3];
    for a in 0..3 {
        dmin[a] = win_min[a].saturating_sub(brick_dim);
        dmax[a] = (win_max[a] + brick_dim).min(res[a]);
    }
    (dmin, dmax)
}

/// S2.5 (frozen-index partial sparse bake): does EVERY brick overlapping the voxel window
/// `[win_min, win_max)` carry a pool handle in the FROZEN `index` (no [`BRICK_TAG_EXTERIOR`] /
/// [`BRICK_TAG_INTERIOR`] sentinel)?
///
/// **SUPERSEDED in production by [`sparse_window_tags_compatible`] (S2.5b).** Sentinel ABSENCE
/// is the wrong question for solids: a dragged prim's dirty region covers its deep inside,
/// which is INTERIOR-tagged BY DESIGN — this check therefore escalated EVERY drag frame at
/// production scale (owner-measured: 85-104ms full classify+bake per frame at 512³). Kept for
/// the S2.5b repro contrast test and the original unit test; do not route new callers here.
///
/// - `true`  → a windowed mode-2 bake against the frozen index updates the pool IN PLACE for
///   every voxel the window touches (the shader routes each voxel through its brick handle;
///   sentinel bricks would silently DROP the write — `volume_bake.wgsl` `output_mode == 2u`).
///   Sentinel bricks OUTSIDE the window are fine: the field there did not change, so the
///   mesher's ±far reads for them are as valid as they were at full-bake time.
/// - `false` → the surface moved into an unallocated brick: the caller must ESCALATE to a
///   full re-classify + full sparse bake (fresh index + full-box dispatch).
///
/// Brick `b` covers voxels `[b·brick_dim, (b+1)·brick_dim)`; the overlapped brick range for the
/// window is `win_min/brick_dim ..= (win_max-1)/brick_dim`, clamped to `dims_bricks`. An empty
/// window (`win_max <= win_min` on any axis) trivially returns `true` (no bricks to check).
pub fn sparse_window_bricks_all_handled(
    index: &[u32],
    dims_bricks: [u32; 3],
    brick_dim: u32,
    win_min: [u32; 3],
    win_max: [u32; 3],
) -> bool {
    let bd = brick_dim.max(1);
    let mut b0 = [0u32; 3];
    let mut b1 = [0u32; 3]; // inclusive
    for a in 0..3 {
        if win_max[a] <= win_min[a] { return true; } // empty window — nothing to check
        b0[a] = (win_min[a] / bd).min(dims_bricks[a].saturating_sub(1));
        b1[a] = ((win_max[a] - 1) / bd).min(dims_bricks[a].saturating_sub(1));
    }
    let (dx, dy) = (dims_bricks[0] as usize, dims_bricks[1] as usize);
    for bz in b0[2]..=b1[2] {
        for by in b0[1]..=b1[1] {
            for bx in b0[0]..=b1[0] {
                let bi = bx as usize + by as usize * dx + bz as usize * dx * dy;
                // Out-of-range = no data behind it — treat as unallocated (conservative).
                if index.get(bi).map_or(true, |&h| h >= BRICK_TAG_INTERIOR) {
                    return false;
                }
            }
        }
    }
    true
}

/// S2.5b escalation evidence: the first dilated-window brick whose NEEDED class (mini-classify
/// of the MOVED prims) is incompatible with its FROZEN index tag. Feeds the `[SparseEscalate]`
/// telemetry — the owner must be able to SEE which brick/class/tag forced a full re-classify.
#[derive(Clone, Copy, Debug)]
pub struct SparseWindowIncompat {
    /// Brick coordinates in the frozen index's brick grid.
    pub brick: [u32; 3],
    /// What the moved field needs this brick to be.
    pub needed: BrickClass,
    /// The frozen index tag: a pool handle (`< BRICK_TAG_INTERIOR`), a sentinel, or `None` when
    /// the brick is out of the index's range (corrupt/truncated index — always escalates).
    pub frozen_tag: Option<u32>,
}

impl SparseWindowIncompat {
    /// Human-readable frozen tag for telemetry: `EXTERIOR`, `INTERIOR`, `handle=N`, or
    /// `missing(out-of-range)`.
    pub fn tag_str(&self) -> String {
        match self.frozen_tag {
            None => "missing(out-of-range)".into(),
            Some(BRICK_TAG_EXTERIOR) => "EXTERIOR".into(),
            Some(BRICK_TAG_INTERIOR) => "INTERIOR".into(),
            Some(h) => format!("handle={h}"),
        }
    }
}

/// S2.5b window check — tag COMPATIBILITY (mini-classify), not sentinel absence.
///
/// [`sparse_window_bricks_all_handled`] (S2.5) escalated whenever ANY window brick was a
/// sentinel. At production scale that made partials UNREACHABLE for solids: a dragged prim's
/// dirty region covers the prim's deep inside, which is INTERIOR-tagged by design, so every
/// drag frame escalated to a full classify + full bake. The right question is not "is every
/// window brick handled?" but "is every window brick's frozen tag still VALID for the moved
/// field?" — answered by re-running the SAME 9-sample Pass-1 predicate the full classifier
/// uses ([`BrickClassifier::classify_one_brick`] — one implementation, never a copy) on the
/// window bricks:
///
/// | needs (mini-classify) | frozen tag | verdict |
/// |-----------------------|------------|---------|
/// | Surface  | handle    | OK — the windowed partial rewrites the brick's pool data |
/// | Surface  | sentinel  | ESCALATE — surface entered an unallocated brick (mode-2 drops the write) |
/// | Interior | INTERIOR  | OK — still decodes `-far`, no write needed |
/// | Interior | handle    | OK — pool gets deep-negative values, decode stays sign-correct |
/// | Interior | EXTERIOR  | ESCALATE — sign flip without a handle (`+far` would plant a wall) |
/// | Exterior | EXTERIOR  | OK |
/// | Exterior | handle    | OK — pool gets positive off-surface values |
/// | Exterior | INTERIOR  | ESCALATE — sign flip without a handle (`-far` would plant solid) |
///
/// Collapsed: a HANDLE is compatible with every needed class (no classify needed — the partial
/// bake rewrites it, and off-surface data decodes sign-correct either way), so only SENTINEL
/// window bricks pay the 9-eval mini-classify. A sentinel is compatible iff the needed class is
/// the same sentinel. `needed == Interior/Exterior` are the STRICT variants (not
/// surface-possible: all 9 samples beyond `±(band + margin)`), so accepting a matching sentinel
/// is exactly as sound as the full classifier's own margin — the true surface is provably
/// outside the brick by the same non-Lipschitz headroom, and a constant `∓far` read stays
/// sign-correct (no crossing → the DC mesher places nothing there).
///
/// `classifier` MUST be built with the same bounds/res/band as the frozen index (the caller has
/// them in scope for `classify_prims_to_sparse_index` anyway; dims are debug-asserted). Window
/// convention identical to [`sparse_window_bricks_all_handled`]: `win_max` exclusive, empty
/// window vacuously compatible, out-of-range index reads escalate (conservative).
///
/// Returns `Ok(())` when a windowed partial against the frozen index is safe, `Err` with the
/// first incompatible brick for telemetry. Cost: sentinel-bricks-in-window × 9 analytic evals,
/// only on partial-candidate frames — sequential with early exit; rayon deliberately not used.
/// Measured (`window_tag_check_timing_512`, debug profile): 3.3ms for a 5832-brick window with
/// 477 sentinel mini-classifies on a 6-prim scene at 512³, vs 80ms for the full classify it
/// replaces on every non-escalating drag frame (~24×; release is lower still).
pub fn sparse_window_tags_compatible(
    index: &[u32],
    dims_bricks: [u32; 3],
    brick_dim: u32,
    win_min: [u32; 3],
    win_max: [u32; 3],
    classifier: &BrickClassifier<'_>,
) -> Result<(), SparseWindowIncompat> {
    debug_assert_eq!(
        classifier.dims_bricks(), dims_bricks,
        "window classifier must be built with the frozen index's bounds/res/brick_dim"
    );
    let bd = brick_dim.max(1);
    let mut b0 = [0u32; 3];
    let mut b1 = [0u32; 3]; // inclusive
    for a in 0..3 {
        if win_max[a] <= win_min[a] { return Ok(()); } // empty window — nothing to check
        b0[a] = (win_min[a] / bd).min(dims_bricks[a].saturating_sub(1));
        b1[a] = ((win_max[a] - 1) / bd).min(dims_bricks[a].saturating_sub(1));
    }
    let (dx, dy) = (dims_bricks[0] as usize, dims_bricks[1] as usize);
    for bz in b0[2]..=b1[2] {
        for by in b0[1]..=b1[1] {
            for bx in b0[0]..=b1[0] {
                let bi = bx as usize + by as usize * dx + bz as usize * dx * dy;
                let Some(&tag) = index.get(bi) else {
                    // Out-of-range = no data behind it — escalate (conservative), reporting the
                    // needed class for the telemetry line.
                    return Err(SparseWindowIncompat {
                        brick: [bx, by, bz],
                        needed: classifier.classify_one_brick(bx, by, bz),
                        frozen_tag: None,
                    });
                };
                if tag < BRICK_TAG_INTERIOR {
                    continue; // handle — compatible with every needed class (see table)
                }
                let needed = classifier.classify_one_brick(bx, by, bz);
                let compatible = matches!(
                    (needed, tag),
                    (BrickClass::Interior, BRICK_TAG_INTERIOR) | (BrickClass::Exterior, BRICK_TAG_EXTERIOR)
                );
                if !compatible {
                    return Err(SparseWindowIncompat { brick: [bx, by, bz], needed, frozen_tag: Some(tag) });
                }
            }
        }
    }
    Ok(())
}

/// Result of [`sparse_window_allocate_incompatible`]: handles were APPENDED for the window's
/// newly-Surface sentinel bricks (plus their apron ring), and sentinel↔sentinel sign flips were
/// RETAGGED in place. The dispatch window MUST be expanded to cover
/// `new_bricks_min..=new_bricks_max` — a newly allocated brick's pool content is garbage until
/// the windowed mode-2 bake writes it. Retags need no bake (they decode `∓far` from the tag
/// alone) but DO require the updated index to reach the GPU + the frozen copy (the caller's
/// `index_update` path) — a stale `+far` inside now-solid space plants a phantom wall.
#[derive(Clone, Copy, Debug)]
pub struct SparseWindowAlloc {
    /// Bricks that received a handle this call — recycled from `freed` first, then dense-packed
    /// extension handles. Every one has GARBAGE pool content until the windowed bake writes it.
    pub appended: u32,
    /// Of `appended`, how many reused a reclaimed handle (pool did NOT grow for these).
    pub reused: u32,
    /// Sentinel bricks whose tag flipped EXTERIOR↔INTERIOR in place (no pool handle needed —
    /// a strict-Interior/Exterior brick is fully described by its sentinel).
    pub retagged: u32,
    /// S2.5f — HANDLE bricks the drag vacated (needed class flipped to strict
    /// Interior/Exterior): retagged back to the sentinel, handle pushed to `freed`.
    pub reclaimed: u32,
    /// Inclusive brick-coordinate box of every appended brick (meaningless when `appended == 0`).
    pub new_bricks_min: [u32; 3],
    pub new_bricks_max: [u32; 3],
    /// Wall-clock ms of phase 1 (parallel mini-classify) — attribution for the caller's
    /// `[TriggerBake]` telemetry (the walk runs on the frame thread; this is the number the
    /// owner's 5-22ms logs were made of). Diagnostic only — never affects verdicts.
    pub classify_ms: f32,
    /// Wall-clock ms of phase 2 (sequential apply/alloc). Diagnostic only.
    pub apply_ms: f32,
}

/// S2.5c — INCREMENTAL brick allocation: the fast-drag replacement for escalation.
///
/// [`sparse_window_tags_compatible`]'s escalation is CORRECT but ~1000× too heavy for its most
/// common trigger: a fast drag moves the surface past the classifier's conservative margin into
/// formerly-EXTERIOR bricks every frame, and the response was a full-domain re-classify + full
/// 512³ re-bake. The key correctness fact this function builds on: **a sentinel is pure
/// compression** — assigning a pool handle to ANY brick and letting the windowed mode-2 bake
/// write its 512 real distance values is ALWAYS at least as correct as the sentinel it replaces
/// (the decode simply reads truth from the pool instead of the ±far shortcut).
///
/// Two phases:
/// 1. **Parallel classify** (rayon over z-slabs, the full classifier's own parallelism — the
///    single-threaded first cut cost 118ms on a real drag window of ~43k bricks, ~40× the full
///    classifier's per-brick rate, and dominated `[TriggerBake]`): EVERY brick in the window is
///    mini-classified with the MOVED prims — sentinels for compatibility, handles for
///    reclamation (S2.5f; parallelism is what makes classifying the handle sheet affordable).
///    Only actionable bricks are collected, in deterministic (z,y,x) scan order.
/// 2. **Sequential apply**, preserving [`emit_sparse_index`]'s handle-numbering determinism:
///    - SENTINEL + needed `Surface` → assign a handle (recycle `freed` LIFO first, else extend
///      `allocated`) and give its 26-neighborhood ring handles too (the apron invariant — same
///      1-ring reach as [`dilate_surface_bricks_1ring`], expressed as a direct neighbor loop
///      because the input here is a handful of bricks, not a full-grid flag vector). Neighbors
///      already holding handles are left untouched; the ring may extend ONE brick beyond the
///      window — covered by the returned `new_bricks_*` box the caller must union into the
///      dispatch window (recycled handles hold the OLD brick's stale content, so every
///      newly-assigned brick MUST be in the bake box regardless of handle provenance).
///    - SENTINEL + needed the opposite sentinel → RETAG in place. A fast drag's swept volume
///      is mostly these sign flips; handles here would bloat the pool with constant-content
///      bricks the decode expresses for free as `∓far`.
///    - HANDLE + needed strict `Interior`/`Exterior` → RECLAIM (S2.5f): retag to the sentinel,
///      push the handle onto `freed` for recycling. This is the vacated trailing edge of a
///      drag — without it every surface cell the drag leaves keeps its pool brick forever and
///      the pool grows monotonically (owner-measured 53k→127k bricks in ONE drag), forcing
///      mid-drag `ensure_capacity` growths (buffer realloc + 100-200MB GPU copy + atlas
///      generation churn). The strict classes carry the classifier's own non-Lipschitz margin
///      (2×brick diagonal), so a reclaimed brick is provably ≥ margin from any surface — the
///      same headroom that makes the apron safe. Handles whose needed class is `Surface` are
///      kept untouched (the windowed bake rewrites their content).
///
/// `sp` is mutated in place: tags, `allocated` (grow-only watermark) and `freed`. Returns the
/// counts + new-brick box, or `Err` on an out-of-range index read (corrupt/truncated index —
/// the one case that still demands a full re-classify). Dropped-bake safety is unchanged: the
/// caller applies this to a CLONE installed only at bake completion, so a superseded dispatch
/// never leaks reclaimed handles into the live frozen index.
pub fn sparse_window_allocate_incompatible(
    sp: &mut SparseIndexOnly,
    brick_dim: u32,
    win_min: [u32; 3],
    win_max: [u32; 3],
    classifier: &BrickClassifier<'_>,
) -> Result<SparseWindowAlloc, SparseWindowIncompat> {
    use rayon::prelude::*;
    debug_assert_eq!(
        classifier.dims_bricks(), sp.dims_bricks,
        "window classifier must be built with the frozen index's bounds/res/brick_dim"
    );
    let dims_bricks = sp.dims_bricks;
    let bd = brick_dim.max(1);
    let mut out = SparseWindowAlloc {
        appended: 0,
        reused: 0,
        retagged: 0,
        reclaimed: 0,
        new_bricks_min: [u32::MAX; 3],
        new_bricks_max: [0; 3],
        classify_ms: 0.0,
        apply_ms: 0.0,
    };
    let mut b0 = [0u32; 3];
    let mut b1 = [0u32; 3]; // inclusive
    for a in 0..3 {
        if win_max[a] <= win_min[a] { return Ok(out); } // empty window — nothing to do
        b0[a] = (win_min[a] / bd).min(dims_bricks[a].saturating_sub(1));
        b1[a] = ((win_max[a] - 1) / bd).min(dims_bricks[a].saturating_sub(1));
    }
    let (dx, dy) = (dims_bricks[0] as usize, dims_bricks[1] as usize);
    let lin = |bx: u32, by: u32, bz: u32| bx as usize + by as usize * dx + bz as usize * dx * dy;
    // One up-front bounds check replaces per-brick `.get` (b0/b1 are clamped to dims, so the
    // max window linear index is the only read that can be out of range on a truncated index).
    if lin(b1[0], b1[1], b1[2]) >= sp.index.len() {
        return Err(SparseWindowIncompat {
            brick: b1,
            needed: classifier.classify_one_brick(b1[0], b1[1], b1[2]),
            frozen_tag: None,
        });
    }

    let t_classify = std::time::Instant::now();
    // Phase 0 — window superblock proofs, the hierarchical Pass-0 of the full classify REUSED
    // over the window's grid-aligned superblock range ([`BrickClassifier::superblock_verdict_one`],
    // one code body — see `superblock_verdicts` for the flag-identity soundness argument).
    // A proven superblock hands every member SENTINEL its needed class with zero per-brick
    // work; HANDLE bricks keep their own per-brick reclaim proofs (a member brick's proof is
    // NOT implied by the superblock's — the doc's partial-ball caveat — and reclaim keys on
    // the per-brick proofs by design). This is what keeps a near-threshold window (32³ bricks
    // at 512) off the frame-thread budget: ~512 O(P) proofs instead of 32k.
    let sbd = SUPERBLOCK_BRICKS;
    let s0 = [b0[0] / sbd, b0[1] / sbd, b0[2] / sbd];
    let s1 = [b1[0] / sbd, b1[1] / sbd, b1[2] / sbd];
    let (swx, swy) = ((s1[0] - s0[0] + 1) as usize, (s1[1] - s0[1] + 1) as usize);
    let sb_verdicts: Vec<Option<(bool, bool)>> = (s0[2]..=s1[2])
        .flat_map(|sz| (s0[1]..=s1[1]).map(move |sy| (sy, sz)))
        .flat_map(|(sy, sz)| (s0[0]..=s1[0]).map(move |sx| [sx, sy, sz]))
        .map(|s| classifier.superblock_verdict_one(s))
        .collect();
    let sb_lookup = |bx: u32, by: u32, bz: u32| -> Option<(bool, bool)> {
        sb_verdicts[(bx / sbd - s0[0]) as usize
            + (by / sbd - s0[1]) as usize * swx
            + (bz / sbd - s0[2]) as usize * swx * swy]
    };
    // Phase 1 — parallel mini-classify. `collect` over the ordered z-range keeps slab order,
    // and each slab scans (y,x) sequentially → the flattened list is exact (z,y,x) scan order,
    // so phase 2's handle numbering is deterministic (identical to the sequential walk).
    let idx: &[u32] = &sp.index;
    // Entries carry `was_handle` — the brick's PHASE-1 state. Phase 2 mutates tags (aprons hand
    // sentinels handles), so the reclaim/retag arms must key on what the brick was BEFORE the
    // walk, not on its current tag: reclaiming a brick an apron just assigned would strip a
    // live apron ring; retagging keys on "still a sentinel" as before.
    let slabs: Vec<Vec<([u32; 3], BrickClass, bool)>> = (b0[2]..=b1[2])
        .into_par_iter()
        .map(|bz| {
            let mut slab: Vec<([u32; 3], BrickClass, bool)> = Vec::new();
            for by in b0[1]..=b1[1] {
                for bx in b0[0]..=b1[0] {
                    let tag = idx[lin(bx, by, bz)];
                    if tag < BRICK_TAG_INTERIOR {
                        // Handle: reclaim ONLY on the cheap bounding-sphere PROOFS — never the
                        // 9-eval classify (running it on the whole window handle sheet every
                        // frame doubled the drag trigger cost). Unproven handles are simply
                        // kept: conservative, and a proof catches them once the edit moves
                        // the prim's influence sphere off (exterior) or swallows them deep
                        // inside a growing solid (interior — P1-#6c, the scale-up leak where
                        // old surface handles inside the grown prim never freed and the pool
                        // ballooned 41k→153k bricks in one gesture).
                        if classifier.provably_exterior(bx, by, bz) {
                            slab.push(([bx, by, bz], BrickClass::Exterior, true));
                        } else if classifier.provably_interior(bx, by, bz) {
                            slab.push(([bx, by, bz], BrickClass::Interior, true));
                        }
                        continue;
                    }
                    // Sentinel: actionable when the needed class disagrees with the tag.
                    // Same cheap containment-proof prefilters as the full classify's Pass 1
                    // ([`classify_with_classifier`]) — REUSED, and bit-identical by the same
                    // arguments (provably_exterior ⟹ classify_one_brick = Exterior; dually
                    // interior). Most of a drag window's sentinels are far-exterior or
                    // deep-interior bricks, so this drops the 9 × P field evals to P distance
                    // checks for them — the frame-thread cost the owner's [TriggerBake]
                    // 5-22ms multi-prim-drag logs were made of. Only near-surface sentinels
                    // fall through to the exact 9-eval classify.
                    let needed = if let Some((_, interior)) = sb_lookup(bx, by, bz) {
                        // Phase-0 hit: the superblock proof is flag-identical to the
                        // per-brick path below (see `superblock_verdicts`).
                        if interior { BrickClass::Interior } else { BrickClass::Exterior }
                    } else if classifier.provably_exterior(bx, by, bz) {
                        BrickClass::Exterior
                    } else if classifier.provably_interior(bx, by, bz) {
                        BrickClass::Interior
                    } else {
                        classifier.classify_one_brick(bx, by, bz)
                    };
                    let compatible = matches!(
                        (needed, tag),
                        (BrickClass::Interior, BRICK_TAG_INTERIOR)
                            | (BrickClass::Exterior, BRICK_TAG_EXTERIOR)
                    );
                    if !compatible {
                        slab.push(([bx, by, bz], needed, false));
                    }
                }
            }
            slab
        })
        .collect(); // indexed over the z-range → slab order == z order, unconditionally
    out.classify_ms = t_classify.elapsed().as_secs_f32() * 1000.0;
    let t_apply = std::time::Instant::now();
    let actionable = slabs.into_iter().flatten();

    // Phase 2 — sequential apply in scan order.
    // Hands `b` a pool handle if it currently holds a sentinel: recycled from `freed` (LIFO,
    // deterministic) first, else the next dense-packed extension. Tracks the new-brick box —
    // recycled handles carry the vacated brick's STALE content, so both kinds must be baked.
    let mut alloc_brick = |sp: &mut SparseIndexOnly, out: &mut SparseWindowAlloc, b: [u32; 3]| {
        let bi = lin(b[0], b[1], b[2]);
        if sp.index[bi] >= BRICK_TAG_INTERIOR {
            sp.index[bi] = match sp.freed.pop() {
                Some(h) => { out.reused += 1; h }
                None => { let h = sp.allocated as u32; sp.allocated += 1; h }
            };
            out.appended += 1;
            for a in 0..3 {
                out.new_bricks_min[a] = out.new_bricks_min[a].min(b[a]);
                out.new_bricks_max[a] = out.new_bricks_max[a].max(b[a]);
            }
        }
    };
    for ([bx, by, bz], needed, was_handle) in actionable {
        let bi = lin(bx, by, bz);
        match needed {
            BrickClass::Surface => {
                alloc_brick(sp, &mut out, [bx, by, bz]);
                // Apron: the DC corner reads + gradient stencil reach the corner-DIAGONAL
                // brick (SPIKE A) — allocate the full 26-ring of a newly-Surface brick.
                for nz in bz.saturating_sub(1)..=(bz + 1).min(dims_bricks[2] - 1) {
                    for ny in by.saturating_sub(1)..=(by + 1).min(dims_bricks[1] - 1) {
                        for nx in bx.saturating_sub(1)..=(bx + 1).min(dims_bricks[0] - 1) {
                            alloc_brick(sp, &mut out, [nx, ny, nz]);
                        }
                    }
                }
            }
            BrickClass::Interior | BrickClass::Exterior => {
                let sentinel = if needed == BrickClass::Interior {
                    BRICK_TAG_INTERIOR
                } else {
                    BRICK_TAG_EXTERIOR
                };
                if was_handle {
                    // RECLAIM a vacated handle brick. Keyed on the PHASE-1 tag: aprons never
                    // touch bricks that already hold handles, so this brick's current tag is
                    // still its original handle — retag to the sentinel and recycle it. (A
                    // brick an apron assigned THIS walk was a phase-1 SENTINEL; its verdict
                    // was collected on the sentinel side and can never reach this arm.)
                    let h = sp.index[bi];
                    debug_assert!(h < BRICK_TAG_INTERIOR,
                        "phase-1 handle brick mutated before its reclaim — apron touched a handle?");
                    sp.index[bi] = sentinel;
                    sp.freed.push(h);
                    out.reclaimed += 1;
                } else if sp.index[bi] >= BRICK_TAG_INTERIOR {
                    // Sign flip between sentinels: retag, no pool. Skip if an earlier Surface
                    // apron handed this brick a handle this walk — the bake writes its truth.
                    sp.index[bi] = sentinel;
                    out.retagged += 1;
                }
            }
        }
    }
    out.apply_ms = t_apply.elapsed().as_secs_f32() * 1000.0;
    Ok(out)
}
