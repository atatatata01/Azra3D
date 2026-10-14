// core_engine/app/src/voxel_surface_sculpt.rs
//
// 3D-Coat-style SURFACE sculpting mode for the voxel sculpt system.
//
// Voxel mode (default, unchanged): brushes write the density field; Surface Nets remeshes;
// topology is free. Surface mode (this module): supported brushes displace the EXISTING
// Surface-Nets chunk-mesh VERTICES directly — no density writes during the stroke, fixed
// topology, much faster/crisper for fine detail. While in Surface mode the MESH is the
// authority and the density field is stale over `VoxelObject::surface_dirty_region`; flipping
// back to Voxel (or save / leave-edit via `voxel_sync_node_mesh`) re-voxelizes the current
// mesh into the field (mesh→SDF over exactly the dirty region, on a worker thread, landing
// through the SAME `voxel_land_move_bake` machinery the Move bake uses — one landing path).
//
// Source-of-truth discipline: density is authoritative in Voxel mode; mesh is authoritative in
// Surface mode; the transition points (mode toggle, save, leave-edit) are the ONLY places the
// two sync, and both directions are explicit (mesh from density = chunk remesh on land; density
// from mesh = region_mesh_to_sdf + write_region_from_sdf).
//
// Undo: each surface stroke = ONE `UndoKind::Surface` entry on the shared voxel order stream,
// restoring the affected chunks' pre-stroke vertex arrays. Density-kind entries below
// un-voxelized surface edits are BARRIERED (undo() refuses with a toast) — restoring density
// and remeshing would wipe the mesh edits. Revoxelization clears the surface entries (their
// vertex snapshots are invalidated by the authoritative remesh) and pushes one Cpu density
// delta for the whole surface session, so Ctrl+Z after the toggle reverts the session at once.

use crate::{DirtyFlags, FourthLayerApp, UndoKind};
use core_math::voxel_sculpt::{falloff_weight_preset, grab_soft_round_falloff, pinch_axis_pull, FalloffPreset, VoxelBrush};
use glam::Vec3;

/// Brushes that have an honest vertex-displacement analog in Surface mode.
///
/// Supported (real per-vertex displacement, per BLENDER_BRUSH_SPEC): Draw, Erase, Smooth, Flatten,
/// Inflate, Move, Pinch (stroke-axis squeeze), Clay/ClayBuildup (area-plane + parabolic band),
/// Scrape (inverse Clay, one-sided), Crease (draw+pinch), Snake Hook (moving-anchor tube over the
/// pristine surface).
///
/// Still GREYED OUT (no honest vertex form — the UI shows a tooltip saying why):
///   • Fill — inverse-Scrape volumetric DEPOSIT into cavities below the plane; a vertex push would
///     be Draw, not a cavity fill (density-native by design).
///   • Smudge — density "taffy" warp: it drags the DENSITY FIELD along the cursor with a continuous
///     taper. Its vertex-drag analog IS the Move brush's soft-round grab already present in surface
///     mode, so exposing Smudge as a second vertex brush would duplicate Move (No-Hacks: one path).
pub(crate) fn surface_brush_supported(brush: VoxelBrush) -> bool {
    matches!(
        brush,
        VoxelBrush::Draw
            | VoxelBrush::Erase
            | VoxelBrush::Smooth
            | VoxelBrush::Flatten
            | VoxelBrush::Inflate
            | VoxelBrush::Move
            | VoxelBrush::Pinch
            | VoxelBrush::Clay
            | VoxelBrush::ClayBuildup
            | VoxelBrush::Scrape
            | VoxelBrush::Crease
            | VoxelBrush::SnakeHook
    )
}

/// One chunk's working copy while a surface edit batch is in flight: loaded in phase 1, displaced
/// in place, normal-welded across ALL chunks in phase 2, written back (upload + AABB registry) in
/// phase 3. Shared by the dab, Snake Hook, and Move-commit paths so all three obey the SAME
/// all-copies-move discipline.
pub(crate) struct ChunkWork {
    pub(crate) ci_u: u32,
    pub(crate) cid: core_math::scene_graph::MeshCacheId,
    pub(crate) verts: Vec<[f32; 8]>,
    pub(crate) indices: Vec<u32>,
    pub(crate) moved: bool,
    pub(crate) normals_changed: bool,
}

/// Min/max AABB of a chunk's current vertex positions (None for an empty array). Feeds the
/// `surface_moved_aabbs` registry — the displaced-chunk lookup that keeps far-displaced geometry
/// inside every future footprint's chunk list.
fn chunk_verts_aabb(verts: &[[f32; 8]]) -> Option<(Vec3, Vec3)> {
    let mut it = verts.iter();
    let first = it.next()?;
    let mut lo = Vec3::new(first[0], first[1], first[2]);
    let mut hi = lo;
    for v in it {
        let p = Vec3::new(v[0], v[1], v[2]);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    Some((lo, hi))
}

/// Chunks from the displaced-chunk registry whose CURRENT vertex AABB intersects any probe cube
/// `[center − pad, center + pad]`. Sorted (BTree-stable) — HashMap iteration order must never
/// leak into chunk processing order.
fn moved_chunks_hit(
    moved: &std::collections::HashMap<u32, (Vec3, Vec3)>,
    probes: &[(Vec3, f32)],
) -> Vec<u32> {
    let mut out: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for (&ci, &(lo, hi)) in moved.iter() {
        let hit = probes.iter().any(|&(c, pad)| {
            let bl = c - Vec3::splat(pad);
            let bh = c + Vec3::splat(pad);
            lo.x <= bh.x && hi.x >= bl.x && lo.y <= bh.y && hi.y >= bl.y && lo.z <= bh.z && hi.z >= bl.z
        });
        if hit { out.insert(ci); }
    }
    out.into_iter().collect()
}

/// CROSS-CHUNK NORMAL WELD (chunk-border seam fix 2026-07-23, extracted 2026-07-23 so the Snake
/// Hook + Move-commit paths share it — their old per-chunk recompute gave each copy of a border
/// vertex a different normal, which then made normal-driven brushes (Inflate) displace the copies
/// differently: permanent position divergence, the needle class).
///
/// Adjacent chunk meshes DUPLICATE their halo vertices (bit-identical positions from the same
/// mesher on the same density) while each FACE is owned by exactly one chunk. Weld: accumulate
/// area-weighted face normals across ALL chunks in `work` keyed on quantized position — every copy
/// of a position hits the same key, so all copies receive the identical complete 1-ring normal.
/// Assign ONLY where `within(pos)` (the edit support + ~2 voxels — exactly the verts whose
/// adjacent faces changed); verts beyond keep their existing normals so the shading transition
/// follows the brush, never the chunk grid. `within` MUST be a pure function of position: both
/// border copies then evaluate the same verdict — no cracks. The caller's chunk list MUST contain
/// every chunk owning a face of any welded vertex (guaranteed by `voxel_surface_candidate_chunks`'
/// +1-cell halo expansion + the displaced-AABB registry).
fn weld_region_normals(
    work: &mut [ChunkWork],
    voxel_size: f32,
    within: impl Fn(Vec3) -> bool,
    support: Option<(Vec3, Vec3)>,
) {
    if !work.iter().any(|w| w.moved) { return; }
    let eps = (voxel_size.max(1e-6)) * 1e-3;
    let inv_eps = 1.0 / eps;
    let key = |p: Vec3| -> [i64; 3] {
        [
            (p.x * inv_eps).round() as i64,
            (p.y * inv_eps).round() as i64,
            (p.z * inv_eps).round() as i64,
        ]
    };
    // SUPPORT SCOPING (measured hot spot: the face accumulation hashed EVERY corner of EVERY
    // candidate chunk per frame). `support` is the caller's edit-support AABB and MUST contain
    // every position `within` can accept (the callers build both from the same probe set, so
    // this holds by construction; `None` = weld everything, the pre-scoping behavior).
    // Correctness: a face contributes to an assigned vertex only if that vertex is one of ITS
    // OWN corners, and every assigned vertex satisfies `within` ⊆ support — so skipping a face
    // whose three corners all lie outside the (float-safety expanded) support box can never
    // remove a face from any assigned vertex's 1-ring. The skip test is position-pure, so both
    // halo copies of a border vertex still see the identical accumulated set — no cracks.
    // Face-box margin MUST cover the maximum 2-RING reach past an assigned vertex (the normal
    // estimate below sums the assigned position's ring keys' own 1-ring accumulators — faces up
    // to ~2 edges ≈ 4 voxels beyond the support box still contribute). 5 voxels = that bound
    // plus float safety; the `support_scoped_weld_matches_unscoped_inside_support` regression
    // asserts scoped == unscoped bit-identical inside the accept set.
    let face_box = support.map(|(lo, hi)| {
        let m = Vec3::splat(5.0 * voxel_size.max(1e-6));
        (lo - m, hi + m)
    });
    let outside = |p: Vec3, lo: Vec3, hi: Vec3| -> bool {
        p.x < lo.x || p.y < lo.y || p.z < lo.z || p.x > hi.x || p.y > hi.y || p.z > hi.z
    };
    // Accumulate area-weighted face normals per position key + the position-key RING adjacency.
    // The assigned normal is the 2-RING estimate (own accumulated normal + every ring key's):
    // a Surface-Nets mesh contains near-degenerate sliver clusters whose raw 1-ring area-weighted
    // normal is ILL-CONDITIONED — measured (fuzz seed 5364): a 0.016 (≈¾ voxel) displacement
    // swung one vert's true 1-ring normal 40°, and Inflate feeding that normal back into position
    // ran the vert away 0.33 (8× median edge) from its coherent ring. Blender's identical
    // normal-feedback Inflate stays clean on its regular grids (counter-test: 6 inverted strokes,
    // 0 spikes, worst ratio 0.60) — the divergence is OUR mesh's slivers, so the estimator must
    // be robust to them: the 2-ring sum lets surrounding healthy faces dominate a tiny sliver
    // cluster. Still position-pure (both halo copies see the identical complete 2-ring) and
    // area-weighted (no direction bias).
    let mut acc: std::collections::HashMap<[i64; 3], Vec3> = std::collections::HashMap::new();
    let mut adj: std::collections::HashMap<[i64; 3], Vec<[i64; 3]>> = std::collections::HashMap::new();
    let mut push_adj = |from: [i64; 3], to: [i64; 3], to2: [i64; 3]| {
        let e = adj.entry(from).or_default();
        if !e.contains(&to) { e.push(to); }
        if !e.contains(&to2) { e.push(to2); }
    };
    for w in work.iter() {
        let n = w.verts.len();
        for t in w.indices.chunks_exact(3) {
            let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
            if ia >= n || ib >= n || ic >= n { continue; }
            let a = Vec3::new(w.verts[ia][0], w.verts[ia][1], w.verts[ia][2]);
            let b = Vec3::new(w.verts[ib][0], w.verts[ib][1], w.verts[ib][2]);
            let c = Vec3::new(w.verts[ic][0], w.verts[ic][1], w.verts[ic][2]);
            if let Some((lo, hi)) = face_box {
                if outside(a, lo, hi) && outside(b, lo, hi) && outside(c, lo, hi) { continue; }
            }
            let fn_ = (b - a).cross(c - a); // area-weighted (unnormalized)
            let (ka, kb, kc) = (key(a), key(b), key(c));
            *acc.entry(ka).or_insert(Vec3::ZERO) += fn_;
            *acc.entry(kb).or_insert(Vec3::ZERO) += fn_;
            *acc.entry(kc).or_insert(Vec3::ZERO) += fn_;
            push_adj(ka, kb, kc);
            push_adj(kb, ka, kc);
            push_adj(kc, ka, kb);
        }
    }
    for w in work.iter_mut() {
        if w.verts.is_empty() { continue; }
        for v8 in w.verts.iter_mut() {
            let p = Vec3::new(v8[0], v8[1], v8[2]);
            // Cheap AABB reject before the O(probes) `within` (support ⊇ the within set).
            if let Some((lo, hi)) = face_box {
                if outside(p, lo, hi) { continue; }
            }
            if !within(p) { continue; }
            let k = key(p);
            if let Some(sum) = acc.get(&k) {
                // 2-ring estimate: own 1-ring sum + every ring position's 1-ring sum.
                let mut total = *sum;
                if let Some(ring) = adj.get(&k) {
                    for rk in ring {
                        if let Some(s) = acc.get(rk) { total += *s; }
                    }
                }
                let nrm = total.normalize_or_zero();
                if nrm.length_squared() > 1e-12
                    && (v8[3] != nrm.x || v8[4] != nrm.y || v8[5] != nrm.z)
                {
                    v8[3] = nrm.x; v8[4] = nrm.y; v8[5] = nrm.z;
                    w.normals_changed = true;
                }
            }
        }
    }
}

/// Blender `normal_radius_factor` — the sub-sphere (× brush radius) over which the live-mesh
/// area frame (average vertex position + normal) is sampled. Shared by the producer's per-dab
/// re-projection (input_sculpt) and the plane-brush pre-pass below.
pub(crate) const SURFACE_AREA_NORMAL_FACTOR: f32 = 1.35;

/// AXIS-COLLAPSE GUARD for the stroke-axis brushes (Pinch / Crease): the closest a vert may be
/// squeezed toward the stroke axis, as a fraction of brush radius. At high strength the pinch
/// fraction saturated at 1.0 and landed EVERY core vert exactly ON the axis line in one dab —
/// coincident verts, zero-area triangles, `Vec3::Y` fallback normals, and a fold so dense the
/// pick's face budget drowned in it (owner 2026-08-19: "mesh broke … brush no longer has
/// effect"). Radius-adaptive, never a world-units magic number.
pub(crate) const PINCH_AXIS_KEEP: f32 = 0.02;

/// Minimum |mean vertex normal| (unit normals averaged → length ∈ [0,1]) for an area frame to
/// count as a STABLE plane. Inside a deep carve the footprint spans BOTH trench walls: their
/// normals oppose and the mean collapses toward zero — `normalize` of that residual is a
/// confident unit vector in a GARBAGE direction, and the mean position floats mid-air in the
/// trench. A plane brush converging verts onto that phantom plane yanks one side of the
/// footprint laterally/vertically up to 0.5·r per dab, in a direction that flips dab-to-dab →
/// the ragged fins/walls standing along a Clay carve path (owner-reported 2026-07-23). Below
/// this agreement threshold there IS no plane; plane brushes fail closed (skip the dab), the
/// same model as Blender's front-face-filtered area normal.
pub(crate) const SURFACE_MIN_AREA_NORMAL: f32 = 0.30;

/// Accumulate area-frame sums `(Σpos, Σnormal, count)` for a batch of probes from ONE chunk's
/// vertex array. Pure — extracted from `voxel_surface_area_frames` so the frame fit is unit-
/// testable against synthetic trench geometry (the clay-fin repro).
/// `front_refs[pi]` = per-probe FRONT-FACE reference direction (Blender `calc_area_normal`
/// front-face filtering): verts whose normal opposes the reference (dot ≤ 0 — e.g. the far
/// wall of a trench, or an overhang lip) are excluded from that probe's frame, so the plane is
/// fit only from geometry actually facing the brush. A ZERO reference disables filtering.
pub(crate) fn accumulate_area_frames(
    acc: &mut [(Vec3, Vec3, u32)],
    verts: &[[f32; 8]],
    probes: &[(Vec3, f32)],
    front_refs: &[Vec3],
    factor: f32,
) {
    for v8 in verts {
        let p = Vec3::new(v8[0], v8[1], v8[2]);
        let n = Vec3::new(v8[3], v8[4], v8[5]);
        for (pi, &(c, r)) in probes.iter().enumerate() {
            let nr = (factor * r).max(1e-4);
            if (p - c).length_squared() > nr * nr {
                continue;
            }
            let fref = front_refs.get(pi).copied().unwrap_or(Vec3::ZERO);
            if fref.length_squared() > 1e-12 && n.dot(fref) <= 0.0 {
                continue; // back-facing to the dab: the far trench wall must not tilt the plane
            }
            let a = &mut acc[pi];
            a.0 += p;
            a.1 += n;
            a.2 += 1;
        }
    }
}

/// Resolve one probe's accumulated sums into a stable area frame. `None` when the footprint is
/// empty OR the sampled normals disagree below `SURFACE_MIN_AREA_NORMAL` — no stable plane
/// exists (deep-trench walls cancelling) and the caller MUST fail closed, never displace toward
/// a garbage plane.
pub(crate) fn resolve_area_frame(sum_p: Vec3, sum_n: Vec3, count: u32) -> Option<(Vec3, Vec3)> {
    if count == 0 {
        return None;
    }
    let inv = 1.0 / count as f32;
    let mean_n = sum_n * inv;
    if mean_n.length() < SURFACE_MIN_AREA_NORMAL {
        return None; // normals disagree → no stable plane → no-op (the clay-carve fin class)
    }
    let n = mean_n.normalize_or_zero();
    if n.length_squared() < 1e-10 {
        return None;
    }
    Some((sum_p * inv, n))
}

/// One resolved surface dab (voxel-LOCAL space) — position/deposit-direction come from the SAME
/// stroke pipeline as density dabs (interpolator, direction resolve, dynamics, symmetry), so
/// falloff/spacing/strength feel identical across modes.
#[derive(Clone, Copy)]
pub(crate) struct SurfaceDab {
    pub pos: Vec3,
    pub dir: Vec3,
    pub radius: f32,
    pub strength: f32, // already includes per-dab pressure
    /// UNIT stroke tangent for the stroke-axis brushes (Pinch/Crease). ZERO = axisymmetric
    /// fallback (first frame / no motion), exactly like the density Pinch path.
    pub stroke_tan: Vec3,
    /// Tip-alpha rotation (radians), resolved by Alpha Orientation (`resolve_tip_rotations`).
    /// Consumed only when the caller hands `displace_verts_for_dabs` a tip image; 0.0 otherwise.
    pub tip_rotation: f32,
}

impl FourthLayerApp {
    /// Live-mesh AREA FRAMES for a batch of probes `(pos, radius)`: for each probe, the average
    /// (position, normal) of every affected-chunk vertex within `factor × radius` — Blender's
    /// `normal_radius_factor` area normal over the CURRENT mesh (the authority in Surface mode;
    /// the density field is stale there, so per-dab normals must come from here, never
    /// `grid.normal_at`). One scan across the union region's chunks for the whole batch, so a
    /// frame stays crack-free at chunk borders (same reason the Smooth pre-hash exists).
    /// `None` = no vertices in the footprint (probe is off the mesh / past the silhouette) OR
    /// the footprint's normals disagree below `SURFACE_MIN_AREA_NORMAL` (no stable plane —
    /// deep-trench walls cancelling; callers fail closed). `front_refs` = per-probe front-face
    /// reference directions (see `accumulate_area_frames`; ZERO disables filtering).
    pub(crate) fn voxel_surface_area_frames(
        &self,
        probes: &[(Vec3, f32)],
        factor: f32,
        front_refs: &[Vec3],
    ) -> Vec<Option<(Vec3, Vec3)>> {
        if probes.is_empty() {
            return Vec::new();
        }
        let Some((voxel_size, chunk_len)) = self
            .sculpt
            .voxel
            .as_ref()
            .map(|v| (v.grid.voxel_size(), v.chunks.len()))
        else {
            return vec![None; probes.len()];
        };
        // Per-probe candidate chunks (grid cells + the displaced-AABB registry): the frame must be
        // fit from the CURRENT mesh even where earlier strokes dragged geometry far from its home
        // cells, and per-probe scoping (vs the old ONE union box) keeps a symmetry-mirrored batch
        // from scanning every chunk between the mirror octants each dab.
        let sample_probes: Vec<(Vec3, f32)> = probes
            .iter()
            .map(|&(p, r)| (p, (factor * r).max(1e-5) + 2.0 * voxel_size))
            .collect();
        let chunk_ids = self.voxel_surface_candidate_chunks(&sample_probes);
        if chunk_ids.is_empty() {
            return vec![None; probes.len()];
        }
        let mut acc: Vec<(Vec3, Vec3, u32)> = vec![(Vec3::ZERO, Vec3::ZERO, 0); probes.len()];
        for &ci in &chunk_ids {
            let ci = ci as usize;
            if ci >= chunk_len {
                continue;
            }
            let Some(cid) = self.sculpt.voxel.as_ref().map(|v| v.chunks[ci]) else { continue };
            // `chunk_mesh_data` (raster → PERSISTENT fallback), NOT raster-only `chunk_verts`:
            // settled chunks (raster entry consumed by upload_pending) would otherwise be
            // INVISIBLE to the frame, so a footprint spanning a touched chunk and a settled
            // neighbour fit its plane from ONE chunk's verts — the plane jumped at chunk
            // borders and dabs shifted crossing a seam (owner-reported 2026-07-23).
            if let Some(verts) = self.cache.mesh_cache.chunk_mesh_data(cid).map(|(v, _)| v) {
                accumulate_area_frames(&mut acc, verts, probes, front_refs, factor);
            }
        }
        acc.into_iter()
            .map(|a| resolve_area_frame(a.0, a.1, a.2))
            .collect()
    }

    /// Chunk-candidate set for a surface-mode footprint given as probe SPHERES `(center, pad)`.
    /// Union of TWO sources, both mandatory for the all-copies-move invariant:
    ///   1. Per-probe density-grid chunk ranges (`brush_aabb` → `voxel_chunks_for_region`, whose
    ///      ±1-cell expansion covers the mesher's halo duplication) — correct for every chunk whose
    ///      geometry still sits in its HOME cells. Per-probe (not one union box) so a symmetry-
    ///      mirrored batch doesn't sweep in every chunk between the mirror octants.
    ///   2. The displaced-chunk registry (`surface_moved_aabbs`): chunks whose CURRENT vertex AABB
    ///      intersects a probe cube. A vertex belongs forever to the chunk that meshed it; deep or
    ///      repeated strokes drag geometry many cells away from those home cells, and source 1
    ///      alone then misses the owner chunk — its copy of a duplicated border vertex froze while
    ///      the neighbour's copy moved (the needle-spike / torn-rim class this fixes).
    /// Sorted + deduped (BTree) — deterministic order.
    pub(crate) fn voxel_surface_candidate_chunks(&self, probes: &[(Vec3, f32)]) -> Vec<u32> {
        let Some(v) = self.sculpt.voxel.as_ref() else { return Vec::new() };
        let mut set: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for &(c, pad) in probes {
            if let Some(a) = v.grid.brush_aabb(c, pad) {
                for ci in self.voxel_chunks_for_region_linear(a) {
                    set.insert(ci);
                }
            }
        }
        for ci in moved_chunks_hit(&v.surface_moved_aabbs, probes) {
            set.insert(ci);
        }
        set.into_iter().collect()
    }

    /// The ONE write choke-point for surface-mode chunk vertex arrays: in-place mesh_cache update
    /// (CPU staging; the GPU write happens in `upload_pending` — Frame Ordering Contract safe) +
    /// the displaced-chunk AABB registry (`surface_moved_aabbs` — the needle-invariant lookup).
    /// EVERY surface path (dabs, Snake Hook, Move commit, undo/redo restore) lands through here so
    /// the two stay in lockstep by construction.
    /// Returns false on a vert-count mismatch (fail closed — the caller drops its snapshot).
    pub(crate) fn voxel_surface_write_chunk_verts(
        &mut self,
        ci_u: u32,
        cid: core_math::scene_graph::MeshCacheId,
        verts: Vec<[f32; 8]>,
    ) -> bool {
        let aabb = chunk_verts_aabb(&verts);
        if !self.cache.mesh_cache.update_chunk_verts_in_place(cid, verts) {
            return false;
        }
        if let (Some(v), Some(ab)) = (self.sculpt.voxel.as_mut(), aabb) {
            v.surface_moved_aabbs.insert(ci_u, ab);
        }
        // GATE 1: mirror the write into the chunk's resident-pool slot so the pool multi-draw
        // (the surface-mode display path) shows this frame's displacement.
        self.voxel_surface_pool_upload(ci_u, cid, false);
        true
    }

    /// GATE 1 — keep a surface-mode chunk's RESIDENT-POOL slot in sync with its authoritative CPU
    /// chunk mesh (`mesh_cache`), so surface mode draws via the pool's ONE
    /// `multi_draw_indexed_indirect` instead of ~700–1500 per-chunk CPU draws.
    ///
    /// Rules (all fail-closed — a chunk without a current slot simply keeps drawing via its CPU
    /// mesh this frame, the raster loop's per-chunk fallback):
    ///   • `Queue::write_buffer` only — the pool is created/grown ONLY in poll (`voxel_ensure_pool`,
    ///     Frame Ordering Contract). A dry `alloc_slot` flags `voxel_pool_grow_to` and returns.
    ///   • Verts-only vs topology is decided by the GENERATION-KEYED sync map
    ///     (`surface_pool_synced`: chunk → `(slot, content generation, pool generation)` via
    ///     `pool_verts_only_allowed`): a verts-only write lands ONLY over the exact slot content
    ///     this chunk last topology-uploaded. A GPU-mesher rewrite, slot release/re-acquire,
    ///     cap-grow re-seed or pool realloc changes the current key → topology re-upload. The GPU
    ///     mesher's pair is self-consistent but in a DIFFERENT vertex order — a verts-only write
    ///     over it scrambles (the chunk-plate / needle display class this key kills structurally).
    ///   • `topology_changed` = the CPU mesh was fully rebuilt (surface-mode chunk remesh) — force
    ///     a topology re-upload.
    ///   • A chunk whose CPU pair is missing/empty RELEASES any held slot (stale-plate guard).
    pub(crate) fn voxel_surface_pool_upload(
        &mut self,
        ci_u: u32,
        cid: core_math::scene_graph::MeshCacheId,
        topology_changed: bool,
    ) {
        let Some(v) = self.sculpt.voxel.as_ref() else { return };
        if !(v.surface_mode && v.gpu_mesh && v.gpu_resident_mesh) || v.chunks.is_empty() {
            return;
        }
        let cd3 = (v.chunk_dim as usize).pow(3);
        if self.gpu.as_ref().map_or(true, |g| g.voxel_mesh.is_none()) { return; }
        if topology_changed {
            if let Some(vm) = self.sculpt.voxel.as_mut() { vm.surface_pool_synced.remove(&ci_u); }
        }
        let stored_key = self.sculpt.voxel.as_ref()
            .and_then(|v| v.surface_pool_synced.get(&ci_u).copied());
        let ci = ci_u as usize;
        // Disjoint field borrows: mesh data (cache) is read while the GPU pool (gpu) is written.
        // A chunk with NO (or an empty) CPU mesh must not keep a live slot showing whatever the
        // pool last held — the CPU mesh is the display authority in surface mode; a retained slot
        // here IS the chunk-shaped stale-plate class. Fail closed: release the slot (indirect
        // zeroed → multi-draw self-skips) so the chunk simply stops drawing until real data lands.
        let cpu_pair_missing = self.cache.mesh_cache.chunk_mesh_data(cid)
            .map_or(true, |(verts, _)| verts.is_empty());
        let gpu = self.gpu.as_mut().unwrap();
        let crate::GpuCtx { voxel_mesh, voxel_chunk_gpu, voxel_pool_grow_to, device, queue, .. } = gpu;
        if voxel_chunk_gpu.len() < cd3 { voxel_chunk_gpu.resize_with(cd3, || None); }
        let Some(slot_opt) = voxel_chunk_gpu.get_mut(ci) else { return };
        let mesher = voxel_mesh.as_mut().unwrap();
        if cpu_pair_missing {
            if let Some(c) = slot_opt.take() {
                mesher.release_slot(queue, c.pool_slot);
                eprintln!("[SURFACE] chunk {ci_u} has no CPU mesh — released slot {} (stale-plate guard)", c.pool_slot);
            }
            if let Some(vm) = self.sculpt.voxel.as_mut() { vm.surface_pool_synced.remove(&ci_u); }
            return;
        }
        let Some((verts, indices)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { return };
        // HARD CAPACITY GATE (the 950e717 crash class, enforced at the API boundary): a CPU chunk
        // pair larger than the FIXED slot strides can never be pool-drawn. Pre-check BEFORE the
        // allocator so an oversized chunk neither allocates a slot every frame nor — worse — keeps
        // a slot showing its stale pre-refusal geometry while the CPU authority moves on (that
        // frozen chunk's halo copies in the neighbours DO move → border needle spikes). Fail
        // closed: release any held slot (indirect zeroed → multi-draw self-skips) so the raster
        // loop's per-chunk CPU-mesh fallback draws this chunk from here on.
        if !mesher.cpu_pair_fits(verts.len(), indices.len()) {
            if let Some(c) = slot_opt.take() {
                mesher.release_slot(queue, c.pool_slot);
                if let Some(vm) = self.sculpt.voxel.as_mut() { vm.surface_pool_synced.remove(&ci_u); }
                // Logged only on the slot-release transition — never per frame.
                eprintln!("[SURFACE] chunk {} pair (v={} i={}) exceeds pool slot strides — released slot {}, CPU-draw fallback",
                    ci_u, verts.len(), indices.len(), c.pool_slot);
            }
            return;
        }
        if !mesher.ensure_chunk_gpu(slot_opt, device, queue, verts.len() as u32, indices.len() as u32) {
            // Pool exhausted: flag the deferred grow (poll reallocates + clears all slots — every
            // chunk then re-syncs topology on its next write) and fall back to the CPU draw.
            // Grow GEOMETRICALLY from the current capacity, NOT to chunk_dim³: at 512³ cd3=32768
            // requested two ~1GiB pool buffers allocated while the OLD pool was still alive — a
            // mid-stroke multi-GB VRAM spike (device-loss risk with zero validation error).
            // Surface mode's chunk set is fixed at entry, so +50% headroom converges in a few
            // grows; cd3 stays the upper bound (more slots than chunks is pointless).
            let cur = mesher.pool_cap_slots();
            let want = cur.saturating_add((cur / 2).max(256)).min(cd3 as u32);
            *voxel_pool_grow_to = (*voxel_pool_grow_to).max(want);
            return;
        }
        // GENERATION-KEYED verts-only gate (the chunk-plate / needle "verts over foreign vertex
        // order" class, made impossible structurally): verts-only ONLY when the stored sync key
        // equals the slot's CURRENT (slot, content-generation, pool-generation) key. A fresh slot,
        // a GPU-mesher rewrite (dispatch bumps the content generation), a release/re-acquire (the
        // pool-monotonic counter never repeats a pair) or a pool realloc all mismatch → topology
        // re-upload. No reliance on any flag-clearing call site.
        let cur_key = mesher.slot_sync_key(slot_opt.as_ref().unwrap());
        let slot = slot_opt.as_ref().unwrap().pool_slot;
        let need_topology = !crate::voxel_gpu_mesh::pool_verts_only_allowed(
            stored_key, cur_key, topology_changed);
        let ok = mesher.upload_cpu_chunk(
            queue, slot, verts,
            if need_topology { Some(indices) } else { None },
        );
        if ok {
            if let Some(vm) = self.sculpt.voxel.as_mut() { vm.surface_pool_synced.insert(ci_u, cur_key); }
        } else {
            // upload_cpu_chunk refused for any other reason (pool missing / slot out of range
            // after a race) — same fail-closed exit: never leave a stale slot on display.
            if let Some(c) = slot_opt.take() {
                mesher.release_slot(queue, c.pool_slot);
            }
            if let Some(vm) = self.sculpt.voxel.as_mut() { vm.surface_pool_synced.remove(&ci_u); }
        }
    }

    /// Apply a batch of surface-mode dabs to the chunk meshes: per affected chunk, snapshot the
    /// pre-stroke verts once (undo), displace vertices with the shared falloff, weld normals
    /// across all chunks, and write back through the single choke-point
    /// (`voxel_surface_write_chunk_verts`). `sgn` = +1 deposit / −1 carve (the caller resolves
    /// brush_add/Ctrl exactly like density).
    pub(crate) fn voxel_surface_apply_dabs(
        &mut self,
        dabs: &[SurfaceDab],
        brush: VoxelBrush,
        sgn: f32,
        hardness: f32,
        preset: FalloffPreset,
    ) {
        if dabs.is_empty() { return; }
        let Some((voxel_size, chunk_len)) = self.sculpt.voxel.as_ref()
            .map(|v| (v.grid.voxel_size(), v.chunks.len())) else { return };

        // Cell-AABB union of the batch — the STALE-DENSITY bookkeeping region only (revoxelize
        // coverage). Chunk membership is NOT derived from this box any more (see
        // `voxel_surface_candidate_chunks`): one union box across a symmetry-mirrored batch spans
        // the whole object, and grid cells alone miss far-displaced owner chunks.
        let mut union: Option<[u32; 6]> = None;
        for d in dabs {
            let r = d.radius.max(1e-5);
            let Some(a) = self.sculpt.voxel.as_ref().and_then(|v| v.grid.brush_aabb(d.pos, r + 2.0 * voxel_size)) else { continue };
            union = Some(match union {
                None => a,
                Some(u) => [u[0].min(a[0]), u[1].min(a[1]), u[2].min(a[2]),
                            u[3].max(a[3]), u[4].max(a[4]), u[5].max(a[5])],
            });
        }
        let Some(region) = union else { return };
        let probes: Vec<(Vec3, f32)> = dabs.iter()
            .map(|d| (d.pos, d.radius.max(1e-5) + 2.0 * voxel_size))
            .collect();
        let chunk_ids = self.voxel_surface_candidate_chunks(&probes);

        // Smooth needs a cross-chunk spatial neighborhood so duplicated border verts on adjacent
        // chunks compute the SAME average (crack-free). Build it once per frame from all affected
        // chunks' current verts (positions only), hashed on a voxel_size lattice.
        let smooth_kernel = 2.0 * voxel_size;
        let smooth_hash: Option<std::collections::HashMap<[i32; 3], Vec<Vec3>>> =
            if matches!(brush, VoxelBrush::Smooth) {
                let mut h: std::collections::HashMap<[i32; 3], Vec<Vec3>> = std::collections::HashMap::new();
                let cell = smooth_kernel;
                for &ci in &chunk_ids {
                    let ci = ci as usize;
                    if ci >= chunk_len { continue; }
                    let cid = self.sculpt.voxel.as_ref().map(|v| v.chunks[ci]);
                    let Some(cid) = cid else { continue };
                    // Settled chunks must contribute too (chunk_mesh_data, not raster-only
                    // chunk_verts) or border verts see a one-sided neighbourhood → smooth
                    // pulls the two copies apart at chunk seams.
                    if let Some(verts) = self.cache.mesh_cache.chunk_mesh_data(cid).map(|(v, _)| v) {
                        for v8 in verts {
                            let p = Vec3::new(v8[0], v8[1], v8[2]);
                            let k = [(p.x / cell).floor() as i32, (p.y / cell).floor() as i32, (p.z / cell).floor() as i32];
                            h.entry(k).or_default().push(p);
                        }
                    }
                }
                Some(h)
            } else { None };

        // Area-plane pre-pass for the plane-relative brushes (Clay/ClayBuildup/Scrape/Crease).
        // Blender's clay_strips/scrape anchor a plane at the cursor with an AREA NORMAL sampled over
        // a `normal_radius_factor≈1.35×radius` sub-sphere (tighter than the footprint → stabler than
        // the per-dab surface normal). Shared machinery: `voxel_surface_area_frames` (the same live-
        // mesh frames the producer's per-dab re-projection uses) — one plane per dab, PER DAB not per
        // stroke, computed once across all chunks so a plane brush stays crack-free at chunk borders.
        // Front-face filtered against each dab's resolved direction + rejected when the footprint's
        // normals disagree (SURFACE_MIN_AREA_NORMAL): in a deep trench the two walls' normals cancel
        // and the old unconditional fit produced a mid-air plane with a garbage normal — the clay
        // converge then dragged one wall toward it every dab = the fins-along-the-carve class
        // (owner-reported 2026-07-23). `None` (empty footprint or unstable plane) → the plane arms
        // SKIP that dab (fail closed), never a substitute plane.
        let plane_brush = matches!(brush,
            VoxelBrush::Clay | VoxelBrush::ClayBuildup | VoxelBrush::Scrape | VoxelBrush::Crease);
        let area_planes: Vec<Option<(Vec3, Vec3)>> = if plane_brush {
            let probes: Vec<(Vec3, f32)> = dabs.iter().map(|d| (d.pos, d.radius)).collect();
            let refs: Vec<Vec3> = dabs.iter().map(|d| d.dir.normalize_or_zero()).collect();
            self.voxel_surface_area_frames(&probes, SURFACE_AREA_NORMAL_FACTOR, &refs)
        } else { Vec::new() };

        // Accumulate-OFF (layer) mode for the normal-deposit brushes — the surface-path twin
        // of poly_sculpt_apply_dabs' ledger, honoring the SAME Accumulate checkbox (Draw
        // factory: accumulate=false). Before this the surface path always per-dab
        // accumulated: ~20 overlapping dabs per pass at 0.10 spacing piled ~10x the intended
        // layer (owner 2026-08-19 "default 10x too strong").
        let layer_mode = matches!(brush, VoxelBrush::Draw | VoxelBrush::Erase | VoxelBrush::Inflate)
            && !self.sculpt.voxel.as_ref().map_or(true, |v| v.accumulate);

        // Brush tip alpha — surface-mode parity with the GPU density stamp (before 2026-08-19
        // the surface path silently ignored the installed tip: alphas only worked in density
        // mode). Clone once per batch: 64KB worst case vs a wholesale vert clone per chunk.
        let tip_img = self.sculpt.voxel.as_ref().and_then(|v| v.brush_tip.clone());
        let tip_contrast = self.sculpt.voxel.as_ref().map_or(1.0, |v| v.tip_contrast);
        let tip = tip_img.as_ref().map(|t| (t, tip_contrast));

        // PHASE 1 — load + displace every affected chunk, NO uploads yet: the cross-chunk
        // normal weld below needs every chunk's DISPLACED verts before any normal is derived.
        let mut work: Vec<ChunkWork> = Vec::with_capacity(chunk_ids.len());
        let mut touched_any = false;
        for &ci_u in &chunk_ids {
            let ci = ci_u as usize;
            if ci >= chunk_len { continue; }
            let Some(cid) = self.sculpt.voxel.as_ref().map(|v| v.chunks[ci]) else { continue };
            let Some((verts_ref, indices_ref)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            if verts_ref.is_empty() { continue; }
            let mut verts = verts_ref.clone();
            let indices = indices_ref.clone();

            // First-touch pre-stroke snapshot for the ONE-entry-per-stroke undo.
            self.sculpt.surface_stroke_snap.entry(ci_u).or_insert_with(|| verts.clone());

            // Ledger keys = vert rows — stable for the stroke's lifetime (the same identity
            // contract the snapshot + fail-closed count check below already rely on).
            let keys: Vec<u32>;
            let accum = if layer_mode {
                keys = (0..verts.len() as u32).collect();
                Some((keys.as_slice(), self.sculpt.surface_stroke_ledger.entry(ci_u).or_default()))
            } else {
                None
            };
            let moved = displace_verts_for_dabs(
                &mut verts, dabs, brush, sgn, hardness, preset,
                &area_planes, smooth_hash.as_ref(), smooth_kernel,
                tip,
                accum,
            );
            work.push(ChunkWork { ci_u, cid, verts, indices, moved, normals_changed: false });
        }

        // PHASE 2 — cross-chunk normal weld over the dab support (radius + 2 voxels: a moved
        // vertex's 1-ring faces extend at most ~2 voxels past the falloff edge). Both border
        // copies of a vertex evaluate the same position test → same verdict, no cracks. This also
        // keeps Inflate honest: it reads per-vertex normals, so copies must carry identical ones.
        {
            let within = |p: Vec3| -> bool {
                dabs.iter().any(|d| {
                    let wr = d.radius.max(1e-5) + 2.0 * voxel_size;
                    (p - d.pos).length_squared() <= wr * wr
                })
            };
            // Support AABB = union of the same probe spheres `within` tests (⊇ its accept set).
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            for d in dabs {
                let wr = d.radius.max(1e-5) + 2.0 * voxel_size;
                lo = lo.min(d.pos - Vec3::splat(wr));
                hi = hi.max(d.pos + Vec3::splat(wr));
            }
            weld_region_normals(&mut work, voxel_size, within, Some((lo, hi)));
        }

        // PHASE 3 — write every chunk whose geometry moved OR whose border normals were re-welded
        // (an untouched neighbour of a moved chunk must upload too, or its copy of the border
        // verts keeps the old normal → seam). All writes go through the single choke-point.
        for w in work {
            if !(w.moved || w.normals_changed) { continue; }
            let ci = w.ci_u as usize;
            if !self.voxel_surface_write_chunk_verts(w.ci_u, w.cid, w.verts) {
                // Count mismatch would mean topology changed under a surface stroke — the
                // barriers make this unreachable; fail closed (skip) rather than corrupt.
                eprintln!("[SURFACE] chunk {ci} vert-count mismatch — dab skipped");
                self.sculpt.surface_stroke_snap.remove(&w.ci_u);
                self.sculpt.surface_stroke_ledger.remove(&w.ci_u);
                continue;
            }
            touched_any = true;
        }
        if touched_any {
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.surface_dirty_region = Some(match v.surface_dirty_region {
                    None => region,
                    Some(u) => [u[0].min(region[0]), u[1].min(region[1]), u[2].min(region[2]),
                                u[3].max(region[3]), u[4].max(region[4]), u[5].max(region[5])],
                });
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
        // Data-first spike hunting (AZRA_SCULPT_VALIDATE=1): scan the live mesh after the batch,
        // dump a replayable op record on any finding. Zero cost when off (one OnceLock read).
        if crate::surface_validate::live_validate_enabled() {
            let d0 = &dabs[0];
            let ctx = format!(
                "dabs brush={brush:?} sgn={sgn} n={} r0={:.4} s0={:.3} p0=({:.4},{:.4},{:.4}) dir0=({:.3},{:.3},{:.3})",
                dabs.len(), d0.radius, d0.strength, d0.pos.x, d0.pos.y, d0.pos.z, d0.dir.x, d0.dir.y, d0.dir.z
            );
            crate::surface_validate::live_validate_after_op(self, &ctx);
        }
    }
}

/// Displace ONE chunk's vertex array by a batch of dabs — a PURE function of `(verts, dabs,
/// brush params)`: no chunk identity, no app state. This is what makes the all-copies-move
/// invariant hold and be testable: duplicated border copies of a vertex are bit-identical
/// positions, and a pure position-driven displacement produces bit-identical results for every
/// copy — provided every owning chunk is in the caller's chunk list
/// (`voxel_surface_candidate_chunks`) and per-vertex normals (the ONE non-position input, read by
/// Inflate) are weld-identical across copies (`weld_region_normals` maintains that after every
/// batch). Returns true if any vertex moved.
#[allow(clippy::too_many_arguments)]
/// Accumulate-OFF increment for one vertex: `target` is THIS dab's full signed deposit
/// (`amp·fade`), `applied` what the stroke has already given the vertex. Returns the top-up
/// so the vertex ends at the stroke's max coverage — Blender's `BRUSH_ACCUMULATE`-unset
/// model (`position = stroke-start + offset·fade`) expressed incrementally, so scribbling
/// in place builds ONE offset layer and stops instead of stacking a tower. A weaker later
/// dab (vertex now at the rim) returns 0 rather than pulling the deposit back down; a
/// sign flip (Ctrl mid-stroke) snaps to the flipped field, same as the orco model.
#[inline]
fn ledger_step(applied: &mut f32, target: f32) -> f32 {
    let step = if target >= 0.0 {
        (target - *applied).max(0.0)
    } else {
        (target - *applied).min(0.0)
    };
    *applied += step;
    step
}

pub(crate) fn displace_verts_for_dabs(
    verts: &mut [[f32; 8]],
    dabs: &[SurfaceDab],
    brush: VoxelBrush,
    sgn: f32,
    hardness: f32,
    preset: FalloffPreset,
    area_planes: &[Option<(Vec3, Vec3)>],
    smooth_hash: Option<&std::collections::HashMap<[i32; 3], Vec<Vec3>>>,
    smooth_kernel: f32,
    // Brush tip alpha `(image, contrast)`: multiplies the radial falloff of EVERY arm — the
    // CPU mirror of the GPU density stamp's `tip_factor` (voxel_brush.wgsl), sampled in the
    // dab's stamp plane rotated by `SurfaceDab::tip_rotation`. `None` = radial falloff only.
    tip: Option<(&core_math::voxel_sculpt::BrushTipAlpha, f32)>,
    // Accumulate-OFF (layer) state for the normal-deposit arms (Draw/Erase/Inflate):
    // `(stable vert key per `verts` row, stroke ledger of signed deposit already applied)`.
    // `None` = classic per-dab accumulation (Blender BRUSH_ACCUMULATE set). The poly caller
    // passes its stroke ledger; the voxel-surface caller passes its per-chunk
    // `surface_stroke_ledger` when the brush's Accumulate flag is off (2026-08-19 — before
    // that it always per-dab accumulated, ~10x the intended layer per pass). Plane brushes
    // converge to a plane and ignore it by design (matches the factory table:
    // Clay/ClayBuildup/Fill accumulate=true).
    mut accum: Option<(&[u32], &mut std::collections::HashMap<u32, f32>)>,
) -> bool {
    let mut moved = false;
    for (di, d) in dabs.iter().enumerate() {
        let r = d.radius.max(1e-5);
        let r2 = r * r;
        let s = d.strength.clamp(0.0, 4.0);
        if s <= 0.0 { continue; }
        // Tip-alpha multiplier for this dab: sample the alpha in the dab's stamp plane
        // (`tip_basis_for_stamp` — the SAME basis the GPU density path packs), UV normalized
        // by radius. Identity when no tip is set.
        let (tip_t, tip_b) = match tip {
            Some(_) => crate::voxel_gpu_mesh::tip_basis_for_stamp(d.dir, d.tip_rotation),
            None => (Vec3::X, Vec3::Y),
        };
        let tip_w = |p: Vec3| -> f32 {
            match tip {
                Some((map, contrast)) => {
                    let rel = p - d.pos;
                    map.sample_contrast(rel.dot(tip_t) / r, rel.dot(tip_b) / r, contrast)
                }
                None => 1.0,
            }
        };
        match brush {
            VoxelBrush::Draw | VoxelBrush::Erase => {
                // Displace along the resolved deposit direction; Erase's natural action is
                // carve, already folded into `sgn` by the caller (same rule as density).
                // BLENDER-EXACT (sculpt_brush_types.cc SCULPT_do_draw_brush, v3.6):
                // `offset = normal × radius × bstrength`, per-vertex `offset × fade`.
                // No magic gain here — `s` arrives from the producers ALREADY carrying
                // Blender's spacing-overlap attenuation (`stroke_overlap_factor`), which
                // is what bounds a pass to ≈ strength × radius (the old 0.15 constant was
                // an unprincipled stand-in for it).
                let amp = s * r * sgn;
                for (vi, v8) in verts.iter_mut().enumerate() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let w = falloff_weight_preset(1.0 - t, hardness, preset) * tip_w(p);
                    if w <= 0.0 { continue; }
                    let mut mag = amp * w;
                    if let Some((keys, led)) = accum.as_mut() {
                        mag = ledger_step(led.entry(keys[vi]).or_insert(0.0), mag);
                        if mag == 0.0 { continue; }
                    }
                    let np = p + d.dir * mag;
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            VoxelBrush::Inflate => {
                // Along each vertex's OWN normal (classic inflate), signed like density —
                // Blender-identical (do_inflate: `vd.no × fade × radius`, and
                // brush_strength gives inflate 0.25×, deflate 0.125×; sculpt.cc v3.6).
                let amp = if sgn > 0.0 { 0.25 } else { 0.125 } * s * r * sgn;
                for (vi, v8) in verts.iter_mut().enumerate() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let w = falloff_weight_preset(1.0 - t, hardness, preset) * tip_w(p);
                    if w <= 0.0 { continue; }
                    let n = Vec3::new(v8[3], v8[4], v8[5]).normalize_or_zero();
                    let mut mag = amp * w;
                    if let Some((keys, led)) = accum.as_mut() {
                        mag = ledger_step(led.entry(keys[vi]).or_insert(0.0), mag);
                        if mag == 0.0 { continue; }
                    }
                    let np = p + n * mag;
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            VoxelBrush::Flatten => {
                // Project toward the dab's contact plane (pos, dir) — HPolish-style.
                for v8 in verts.iter_mut() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let w = falloff_weight_preset(1.0 - t, hardness, preset) * tip_w(p);
                    if w <= 0.0 { continue; }
                    let h = (p - d.pos).dot(d.dir);
                    let np = p - d.dir * (h * (w * s * 0.5).clamp(0.0, 1.0));
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            VoxelBrush::Smooth => {
                // Spatial (position-hash) Laplacian: average of ALL affected-chunk verts
                // within `smooth_kernel` — duplicated border verts see the same set on
                // both copies, so chunks stay welded.
                let Some(hash) = smooth_hash else { continue };
                let cell = smooth_kernel;
                let k2 = smooth_kernel * smooth_kernel;
                for v8 in verts.iter_mut() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let w = falloff_weight_preset(1.0 - t, hardness, preset) * tip_w(p);
                    if w <= 0.0 { continue; }
                    let kc = [(p.x / cell).floor() as i32, (p.y / cell).floor() as i32, (p.z / cell).floor() as i32];
                    let mut sum = Vec3::ZERO;
                    let mut cnt = 0u32;
                    for dx in -1..=1i32 { for dy in -1..=1i32 { for dz in -1..=1i32 {
                        if let Some(bin) = hash.get(&[kc[0] + dx, kc[1] + dy, kc[2] + dz]) {
                            for q in bin {
                                if (*q - p).length_squared() <= k2 { sum += *q; cnt += 1; }
                            }
                        }
                    }}}
                    if cnt < 2 { continue; }
                    let avg = sum / cnt as f32;
                    let np = p + (avg - p) * (w * s * 0.5).clamp(0.0, 0.9);
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            VoxelBrush::Pinch => {
                // Stroke-axis PINCH (BLENDER_BRUSH_SPEC §2): pull each vertex toward the
                // stroke AXIS LINE. `pinch_axis_pull` (shared with the density Pinch) returns
                // the vector FROM the axis TO the vertex (cross-stroke component); moving the
                // vertex by −that squeezes it onto the crease. `sgn` (Ctrl) flips to a bulge.
                // `translation *= curve*strength` per the spec.
                let amp = s * 0.5;
                for v8 in verts.iter_mut() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let w = falloff_weight_preset(1.0 - t, hardness, preset) * tip_w(p);
                    if w <= 0.0 { continue; }
                    let pull = pinch_axis_pull(p - d.pos, d.dir.normalize_or_zero(), d.stroke_tan);
                    if pull.length_squared() < 1e-14 { continue; }
                    // AXIS-COLLAPSE GUARD (owner 2026-08-19 "mesh broke … brush no longer
                    // has effect"): at strength ≥ 2 the clamp saturated at 1.0, landing EVERY
                    // core vert exactly ON the axis line in one dab → coincident verts →
                    // zero-area triangles → the pinched crater + Vec3::Y fallback normals.
                    // Cap the inward step so a vert keeps ≥ 2% of the radius from the axis
                    // (radius-adaptive, same spirit as poly_smooth_one_ring's mean-edge cap).
                    // Outward (Ctrl bulge) needs no cap.
                    let frac = (w * amp * sgn).clamp(-1.0, 1.0);
                    let frac = if frac > 0.0 {
                        let dist = pull.length();
                        frac.min(((dist - PINCH_AXIS_KEEP * r) / dist).max(0.0))
                    } else {
                        frac
                    };
                    let np = p - pull * frac;
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            VoxelBrush::Clay | VoxelBrush::ClayBuildup => {
                // CLAY STRIPS (BLENDER_BRUSH_SPEC §3): displace toward an area plane pushed
                // PROUD along the area normal by `offset`, weighted by curve × a parabolic
                // z(1−z) normal band (peak at mid-depth → the flat-slab profile). Area plane
                // (center+normal) is the 1.35×radius sub-sphere pre-pass. `sgn` (Ctrl) digs in.
                // DEFERRED (honest): the elongated square tip (brush-local box + x-stretch) —
                // it needs a per-dab brush-local matrix; this ships the plane+parabolic slab
                // (the depth profile the oracle gates on). Radial footprint stays round.
                // FAIL CLOSED on a missing/degenerate area plane (empty footprint, or the
                // footprint's normals cancelling below SURFACE_MIN_AREA_NORMAL — the deep-trench
                // case): a plane brush with no stable plane must no-op, never converge verts
                // onto a garbage plane (the clay-carve fin/wall class, 2026-07-23).
                let Some((ac, an0)) = area_planes[di] else { continue };
                let an = an0.normalize_or_zero() * sgn; // Ctrl digs the plane in (deposit → carve)
                if an.length_squared() < 1e-10 { continue; }
                let offset = 0.20 * r; // proud-plane push along the area normal (Blender clay offset)
                let plane_d = (ac + an * offset).dot(an); // proud plane position along an
                // Normal band half-extent → local_z ∈ [0,1] over ~radius. Also the DESIGN CAP on
                // per-dab travel: verts deeper than `depth` below the plane get z_fac = 0 (band
                // taper), so no vert ever converges more than 0.5·r in one dab (Blender's
                // bstrength·radius bound) — asserted by the trench regression test.
                let depth = 0.5 * r;
                for v8 in verts.iter_mut() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let w = falloff_weight_preset(1.0 - t, hardness, preset) * tip_w(p);
                    if w <= 0.0 { continue; }
                    // Signed gap: how far the vertex sits BELOW the proud plane (deposit fills up
                    // to the plane). Verts already above it (>0) are left — clay builds, never digs.
                    let sd = p.dot(an) - plane_d; // <0 = below proud plane (needs filling)
                    if sd >= 0.0 { continue; }
                    // Parabolic normal band z(1−z)·4, peak mid-depth → the flat-slab-with-tapered-
                    // ends Clay Strips profile (BLENDER_BRUSH_SPEC §3). local_z maps sd∈[−depth,0]→[0,1].
                    let local_z = (1.0 + sd / depth).clamp(0.0, 1.0);
                    let z_fac = (local_z * (1.0 - local_z) * 4.0).max(0.0);
                    // DEPOSIT outward along the area normal toward the proud plane (build up, curve·band·strength).
                    let k = (w * z_fac * s).clamp(0.0, 1.0);
                    let np = p - an * (sd * k); // −sd·an = push up along +an toward the plane
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            VoxelBrush::Scrape => {
                // SCRAPE (BLENDER_BRUSH_SPEC §4): inverse Clay — flatten toward a plane at/below
                // the surface with NO proud offset, only cutting the HIGH spots (verts above the
                // area plane). Reuses the area-plane pre-pass. Ctrl flips the cut side.
                // Same fail-closed rule as Clay: no stable plane → skip the dab.
                let Some((ac, an0)) = area_planes[di] else { continue };
                let an = an0.normalize_or_zero() * sgn;
                if an.length_squared() < 1e-10 { continue; }
                let plane_d = ac.dot(an);
                for v8 in verts.iter_mut() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let w = falloff_weight_preset(1.0 - t, hardness, preset) * tip_w(p);
                    if w <= 0.0 { continue; }
                    let sd = p.dot(an) - plane_d; // >0 = high spot above plane
                    if sd <= 0.0 { continue; } // only shave proud material (one-sided)
                    let k = (w * s * 0.5).clamp(0.0, 1.0);
                    let np = p - an * (sd * k);
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            VoxelBrush::Crease => {
                // CREASE (BLENDER_BRUSH_SPEC §5): Draw (displace inward/outward along the area
                // normal) COMBINED with a pinch toward the stroke axis → a sharp ridge/groove.
                // Reuses Pinch's `pinch_axis_pull` + a normal push. Sharp feel via the curve
                // squared (tighter core). `sgn`/Ctrl: default carves a groove (−normal); Ctrl
                // raises a ridge. Uses the area normal (stabler than the raw dab normal);
                // Crease classifies nothing against a plane POSITION, so when no stable area
                // frame exists the resolved dab normal (continuity-aligned by the producer) is
                // an honest direction fallback — not a substitute plane.
                let an0 = area_planes[di].map(|(_, n)| n)
                    .unwrap_or_else(|| d.dir.normalize_or_zero());
                let an = an0.normalize_or_zero();
                if an.length_squared() < 1e-10 { continue; }
                let draw_amp = s * 0.12 * r * sgn;      // inward groove by default
                let pinch_amp = s * 0.5;                 // inward squeeze (always toward axis)
                for v8 in verts.iter_mut() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let d2 = (p - d.pos).length_squared();
                    if d2 >= r2 { continue; }
                    let t = d2.sqrt() / r;
                    let wc = falloff_weight_preset(1.0 - t, hardness, preset);
                    if wc <= 0.0 { continue; }
                    // Sharpen (crease is high-contrast); tip multiplies ONCE after the square —
                    // same "fall *= tip_factor" order as the GPU stamp, never tip².
                    let w = wc * wc * tip_w(p);
                    let mut np = p - an * (draw_amp * w); // draw component (groove)
                    let pull = pinch_axis_pull(p - d.pos, an, d.stroke_tan);
                    if pull.length_squared() > 1e-14 {
                        // Same AXIS-COLLAPSE GUARD as Pinch: never squeeze a vert closer
                        // than 2% of radius to the axis (saturation at 1.0 collapsed the
                        // whole core onto the line → zero-area fold, the sculpt crater).
                        let dist = pull.length();
                        let frac = (w * pinch_amp)
                            .clamp(0.0, 1.0)
                            .min(((dist - PINCH_AXIS_KEEP * r) / dist).max(0.0));
                        np -= pull * frac; // pinch toward axis
                    }
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            _ => {} // unsupported brushes never reach here (input gate + UI grey-out)
        }
    }
    moved
}

impl FourthLayerApp {
    /// SURFACE-mode Snake Hook (BLENDER_BRUSH_SPEC §5): draw a moving-anchor TUBE out of the surface.
    /// Displaces each vertex over its PRISTINE (pre-stroke) position — the first-touch chunk snapshot
    /// in `surface_stroke_snap`, which is captured lazily here so drag-back un-deforms (elastic, like
    /// Grab). Three superimposed effects, evaluated over the ORIGINAL coords:
    ///   1. Tube pull: `disp = grab_delta * factor`.
    ///   2. Axis pinch: `pinch = 2·(0.5 − crease_pinch)·|grab_delta|/radius`; squeeze `(orig − axis)`
    ///      toward the tube axis by `pinch·factor` (crease_pinch 0.5 = OFF; >0.5 narrows the tip).
    /// The affected region is a CAPSULE (anchor → anchor+grab_delta) — falloff on distance to that
    /// segment. NO relax (would smear the tube). `symmetry` mirrors the anchor+drag across local planes.
    /// Rake is intentionally omitted this pass (documented as a deferred refinement — it needs a
    /// per-vertex partial of the drag-direction change tracked across frames; the tube + axis-pinch
    /// are the defining Snake Hook traits the oracle gates on).
    ///
    /// Writes displaced verts into mesh_cache in place each frame (same in-place upload the dab
    /// brushes use, Frame-Ordering-Contract safe) and unions the surface dirty region + snapshot.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn voxel_surface_apply_snake_hook(
        &mut self,
        anchor: Vec3,
        drag: Vec3,
        radius: f32,
        crease_pinch: f32,
        hardness: f32,
        preset: FalloffPreset,
        symmetry: [bool; 3],
    ) {
        if drag.length() < 1e-6 { return; }
        let Some((voxel_size, chunk_len)) = self.sculpt.voxel.as_ref()
            .map(|v| (v.grid.voxel_size(), v.chunks.len())) else { return };
        let r = radius.max(1e-4);

        // Mirror the (anchor, drag) instance across each enabled local symmetry plane. The mesh lives
        // in voxel-LOCAL space with symmetry planes through the origin axes (same convention as the
        // stamp-level mirror), so reflect BOTH the anchor and the drag vector on each axis.
        let mut insts: Vec<(Vec3, Vec3)> = vec![(anchor, drag)];
        for (axis, on) in symmetry.iter().enumerate() {
            if !on { continue; }
            let flip = |mut p: Vec3| { p[axis] = -p[axis]; p };
            let mirrored: Vec<(Vec3, Vec3)> = insts.iter().map(|(a, d)| (flip(*a), flip(*d))).collect();
            insts.extend(mirrored);
        }

        // Cell-AABB union over all instance capsules — stale-density bookkeeping only.
        let mut union: Option<[u32; 6]> = None;
        let mut probes: Vec<(Vec3, f32)> = Vec::with_capacity(insts.len() * 2);
        for (a, d) in &insts {
            let pad = r + d.length() + 2.0 * voxel_size;
            // Cover both capsule ends (each end box spans the whole capsule since pad ≥ |d|).
            for c in [*a, *a + *d] {
                probes.push((c, pad));
                let Some(bb) = self.sculpt.voxel.as_ref().and_then(|v| v.grid.brush_aabb(c, pad)) else { continue };
                union = Some(match union {
                    None => bb,
                    Some(u) => [u[0].min(bb[0]), u[1].min(bb[1]), u[2].min(bb[2]),
                                u[3].max(bb[3]), u[4].max(bb[4]), u[5].max(bb[5])],
                });
            }
        }
        let Some(region) = union else { return };
        // Candidate chunks = grid cells + displaced-AABB registry (all-copies-move invariant),
        // PLUS every chunk already snapshotted this stroke: Snake Hook RE-DERIVES from the
        // pristine snapshot each frame, so when the capsule SHRINKS (drag-back) a previously
        // touched chunk outside the current capsule must still be re-derived — otherwise it kept
        // last frame's larger deformation (stale tube remnants / border needles on drag-back).
        let mut chunk_ids = self.voxel_surface_candidate_chunks(&probes);
        {
            let mut set: std::collections::BTreeSet<u32> = chunk_ids.iter().copied().collect();
            set.extend(self.sculpt.surface_stroke_snap.keys().copied());
            chunk_ids = set.into_iter().collect();
        }

        let pinch_gain = 2.0 * (0.5 - crease_pinch.clamp(0.0, 1.0)); // >0 narrows, <0 inflates, 0 = off

        // PHASE 1 — re-derive every candidate chunk from its pristine snapshot (NO writes yet;
        // the cross-chunk weld below needs every chunk's displaced verts first).
        let mut work: Vec<ChunkWork> = Vec::with_capacity(chunk_ids.len());
        let mut current: Vec<Vec<[f32; 8]>> = Vec::with_capacity(chunk_ids.len());
        for &ci_u in &chunk_ids {
            let ci = ci_u as usize;
            if ci >= chunk_len { continue; }
            let Some(cid) = self.sculpt.voxel.as_ref().map(|v| v.chunks[ci]) else { continue };
            let Some((verts_ref, indices_ref)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            if verts_ref.is_empty() { continue; }
            let indices = indices_ref.clone();
            let cur = verts_ref.clone();

            // First-touch pristine snapshot (elastic ORIGINAL coords) — the SAME undo snapshot the
            // dab brushes use, so one stroke stays ONE UndoKind::Surface entry.
            let orig = self.sculpt.surface_stroke_snap
                .entry(ci_u).or_insert_with(|| cur.clone()).clone();

            // Displace from ORIGINAL positions (not the current, already-displaced verts) so the tube
            // is a pure function of the accumulated drag → drag-back restores pristine (both the
            // positions AND the normals: `orig` carries the pristine normals, and the weld below
            // re-derives normals only inside the current capsule support).
            let mut verts = orig.clone();
            let mut moved = false;
            for (a, d) in &insts {
                let dl = d.length();
                if dl < 1e-6 { continue; }
                let axis_dir = *d / dl; // tube axis (unit)
                for v8 in verts.iter_mut() {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    // Distance to the capsule segment [a, a+d].
                    let t = ((p - *a).dot(*d) / (dl * dl)).clamp(0.0, 1.0);
                    let closest = *a + *d * t;
                    let dist = (p - closest).length();
                    if dist >= r { continue; }
                    let w = falloff_weight_preset(1.0 - dist / r, hardness, preset);
                    if w <= 0.0 { continue; }
                    // 1) tube pull
                    let mut np = p + *d * w;
                    // 2) axis pinch — squeeze toward the tube axis line through the anchor.
                    if pinch_gain.abs() > 1e-4 {
                        let rel = np - *a;
                        let along = axis_dir * rel.dot(axis_dir);
                        let radial = rel - along; // component ⊥ to the tube axis
                        let squeeze = (pinch_gain * dl / r * w).clamp(-1.0, 1.0);
                        np -= radial * squeeze;
                    }
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            work.push(ChunkWork { ci_u, cid, verts, indices, moved, normals_changed: false });
            current.push(cur);
        }

        // PHASE 2 — cross-chunk normal weld (shared with the dab path; the old per-chunk
        // recompute gave duplicated border copies different normals → seams + later Inflate
        // divergence). Support = the current capsules + 2 voxels: every vertex the tube can have
        // displaced lies inside its capsule, so all moved verts re-weld; verts drag-back restored
        // to pristine outside the capsule keep their pristine (snapshot) normals — also correct.
        {
            let within = |p: Vec3| -> bool {
                insts.iter().any(|(a, d)| {
                    let dl2 = d.length_squared().max(1e-12);
                    let t = ((p - *a).dot(*d) / dl2).clamp(0.0, 1.0);
                    let dist2 = (p - (*a + *d * t)).length_squared();
                    let wr = r + 2.0 * voxel_size;
                    dist2 <= wr * wr
                })
            };
            // Support AABB = union of the capsule endpoints padded by the same capsule radius.
            let wr = r + 2.0 * voxel_size;
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            for (a, d) in &insts {
                for p in [*a, *a + *d] {
                    lo = lo.min(p - Vec3::splat(wr));
                    hi = hi.max(p + Vec3::splat(wr));
                }
            }
            weld_region_normals(&mut work, voxel_size, within, Some((lo, hi)));
        }

        // PHASE 3 — write every chunk whose NEW array differs from what is currently stored
        // (not just "moved this frame": a drag-back RESTORE where the capsule no longer covers
        // the chunk yields moved=false yet must still land, or the old deformation sticks).
        let mut touched_any = false;
        for (w, cur) in work.into_iter().zip(current.into_iter()) {
            if w.verts == cur { continue; }
            if self.voxel_surface_write_chunk_verts(w.ci_u, w.cid, w.verts) {
                touched_any = true;
            } else {
                self.sculpt.surface_stroke_snap.remove(&w.ci_u);
                self.sculpt.surface_stroke_ledger.remove(&w.ci_u);
            }
        }
        if touched_any {
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.surface_dirty_region = Some(match v.surface_dirty_region {
                    None => region,
                    Some(u) => [u[0].min(region[0]), u[1].min(region[1]), u[2].min(region[2]),
                                u[3].max(region[3]), u[4].max(region[4]), u[5].max(region[5])],
                });
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if crate::surface_validate::live_validate_enabled() {
            let ctx = format!(
                "snake_hook anchor=({:.4},{:.4},{:.4}) drag=({:.4},{:.4},{:.4}) r={radius:.4} pinch={crease_pinch:.3}",
                anchor.x, anchor.y, anchor.z, drag.x, drag.y, drag.z
            );
            crate::surface_validate::live_validate_after_op(self, &ctx);
        }
    }

    /// Surface-mode MOVE release: commit the finished mesh-grab (which displayed via the GPU vertex
    /// shader during the drag, exactly as in Voxel mode) directly into the chunk vertex arrays —
    /// same soft-round falloff as the live shader (`grab_soft_round_falloff`), symmetry-expanded.
    /// No density write, no pending-grab stack: the mesh IS the authority in Surface mode.
    pub(crate) fn voxel_surface_commit_move_grab(&mut self) {
        let Some((insts, outer_r)) = self.sculpt.voxel.as_mut().map(|v| {
            let g = std::mem::take(&mut v.mesh_grab);
            (g.mirror_instances(v.symmetry), g.outer_r)
        }) else { return };
        let voxel_size = self.sculpt.voxel.as_ref().map_or(0.01, |v| v.grid.voxel_size());
        let chunk_len = self.sculpt.voxel.as_ref().map_or(0, |v| v.chunks.len());
        let mut union: Option<[u32; 6]> = None;
        let mut probes: Vec<(Vec3, f32)> = Vec::with_capacity(insts.len());
        for (anchor, drag) in &insts {
            if drag.length() < 1e-5 { continue; }
            let pad = outer_r + drag.length() + 2.0 * voxel_size;
            probes.push((*anchor, pad));
            let Some(a) = self.sculpt.voxel.as_ref().and_then(|v| v.grid.brush_aabb(*anchor, pad)) else { continue };
            union = Some(match union {
                None => a,
                Some(u) => [u[0].min(a[0]), u[1].min(a[1]), u[2].min(a[2]),
                            u[3].max(a[3]), u[4].max(a[4]), u[5].max(a[5])],
            });
        }
        let Some(region) = union else { return };
        // Candidate chunks = grid cells + displaced-AABB registry (all-copies-move invariant —
        // a grab over geometry an earlier stroke displaced must still reach its owner chunks).
        let chunk_ids = self.voxel_surface_candidate_chunks(&probes);

        // PHASE 1 — displace (no writes yet; the weld needs all chunks' displaced verts).
        let mut work: Vec<ChunkWork> = Vec::with_capacity(chunk_ids.len());
        for &ci_u in &chunk_ids {
            let ci = ci_u as usize;
            if ci >= chunk_len { continue; }
            let Some(cid) = self.sculpt.voxel.as_ref().map(|v| v.chunks[ci]) else { continue };
            let Some((verts_ref, indices_ref)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            if verts_ref.is_empty() { continue; }
            let mut verts = verts_ref.clone();
            let indices = indices_ref.clone();
            self.sculpt.surface_stroke_snap.entry(ci_u).or_insert_with(|| verts.clone());
            let mut moved = false;
            for v8 in verts.iter_mut() {
                let p = Vec3::new(v8[0], v8[1], v8[2]);
                let mut delta = Vec3::ZERO;
                for (anchor, drag) in &insts {
                    let f = grab_soft_round_falloff((p - *anchor).length(), outer_r);
                    if f > 0.0 { delta += *drag * f; }
                }
                if delta.length_squared() > 1e-14 {
                    let np = p + delta;
                    v8[0] = np.x; v8[1] = np.y; v8[2] = np.z;
                    moved = true;
                }
            }
            work.push(ChunkWork { ci_u, cid, verts, indices, moved, normals_changed: false });
        }

        // PHASE 2 — cross-chunk normal weld (shared with the dab path; the old per-chunk
        // recompute diverged duplicated border copies' normals). Support: displaced verts started
        // within `outer_r` of an anchor and moved by ≤ |drag| → within `outer_r + |drag|` after.
        {
            let within = |p: Vec3| -> bool {
                probes.iter().any(|&(a, pad)| (p - a).length_squared() <= pad * pad)
            };
            // Support AABB = union of the same probe spheres `within` tests.
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            for &(a, pad) in &probes {
                lo = lo.min(a - Vec3::splat(pad));
                hi = hi.max(a + Vec3::splat(pad));
            }
            weld_region_normals(&mut work, voxel_size, within, Some((lo, hi)));
        }

        // PHASE 3 — write through the single choke-point.
        let mut touched = false;
        for w in work {
            if !(w.moved || w.normals_changed) { continue; }
            if self.voxel_surface_write_chunk_verts(w.ci_u, w.cid, w.verts) {
                touched = true;
            } else {
                self.sculpt.surface_stroke_snap.remove(&w.ci_u);
                self.sculpt.surface_stroke_ledger.remove(&w.ci_u);
            }
        }
        if touched {
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.surface_dirty_region = Some(match v.surface_dirty_region {
                    None => region,
                    Some(u) => [u[0].min(region[0]), u[1].min(region[1]), u[2].min(region[2]),
                                u[3].max(region[3]), u[4].max(region[4]), u[5].max(region[5])],
                });
            }
        }
        if crate::surface_validate::live_validate_enabled() {
            crate::surface_validate::live_validate_after_op(self, "move_commit");
        }
    }

    /// Stroke release in Surface mode: drain the per-chunk pre-stroke snapshots into ONE
    /// `UndoKind::Surface` entry (matching the one-entry-per-stroke pattern of every sculpt path).
    /// Thin wrapper over THE one lifecycle exit (`SculptState::surface_end_stroke`) — every other
    /// stroke-ending path (stroke-start heal, undo/redo, SCENE force-clear) calls the same fn.
    pub(crate) fn voxel_surface_commit_stroke(&mut self) {
        self.sculpt.surface_end_stroke();
    }

    /// Apply one Surface undo (or redo): pop the entry, snapshot the CURRENT verts of the same
    /// chunks as the inverse for the opposite stack, and restore in place. Returns true if applied.
    pub(crate) fn voxel_surface_undo_apply(&mut self, redo: bool) -> bool {
        let entry = if redo { self.sculpt.voxel_surface_redo_stack.pop_back() }
                    else { self.sculpt.voxel_surface_undo_stack.pop_back() };
        let Some(entry) = entry else { return false };
        let mut inverse: crate::SurfaceMeshDelta = Vec::with_capacity(entry.len());
        let mut dirty_union: Option<[u32; 6]> = None;
        for (ci_u, verts) in entry {
            let ci = ci_u as usize;
            let Some(cid) = self.sculpt.voxel.as_ref().and_then(|v| v.chunks.get(ci).copied()) else { continue };
            // MUST be `chunk_mesh_data` (raster → PERSISTENT fallback), never `chunk_verts`
            // (raster-only): `upload_pending` consumes the raster entry within a frame of the
            // stroke, so by the time Ctrl+Z arrives every chunk is SETTLED and `chunk_verts`
            // returns None — the entry was popped, nothing restored, history destroyed (the
            // owner-reported "Ctrl+Z does not undo in Surface mode", 2026-07-23).
            // `update_chunk_verts_in_place` already handles the settled case (re-queues the
            // raster entry from the persistent copy), so the restore lands correctly.
            if let Some(cur) = self.cache.mesh_cache.chunk_mesh_data(cid).map(|(v, _)| v.clone()) {
                if cur.len() == verts.len() {
                    inverse.push((ci_u, cur));
                    // Choke-point write: keeps the displaced-AABB registry (and, Gate 1, the
                    // resident-pool slot) in sync with the restored vertex positions too.
                    let _ = self.voxel_surface_write_chunk_verts(ci_u, cid, verts);
                } else {
                    eprintln!("[SURFACE] undo chunk {ci} count mismatch — entry skipped");
                    continue;
                }
            }
            // Keep the stale-density region covering everything a surface undo/redo touches: even
            // fully-undone edits leave the conservative barrier ON until the next revoxelization.
            let (cd, res) = self.sculpt.voxel.as_ref().map(|v| (v.chunk_dim, v.grid.res)).unwrap_or((1, 2));
            let (a0, a1) = crate::input_sculpt::voxel_chunk_anchor(ci_u, cd, res);
            let a = [a0[0], a0[1], a0[2], a1[0], a1[1], a1[2]];
            dirty_union = Some(match dirty_union {
                None => a,
                Some(u) => [u[0].min(a[0]), u[1].min(a[1]), u[2].min(a[2]),
                            u[3].max(a[3]), u[4].max(a[4]), u[5].max(a[5])],
            });
        }
        if inverse.is_empty() { return false; }
        if let Some(d) = dirty_union {
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.surface_dirty_region = Some(match v.surface_dirty_region {
                    None => d,
                    Some(u) => [u[0].min(d[0]), u[1].min(d[1]), u[2].min(d[2]),
                                u[3].max(d[3]), u[4].max(d[4]), u[5].max(d[5])],
                });
            }
        }
        if redo {
            let ev = self.sculpt.voxel_surface_undo_stack.push(inverse);
            self.sculpt.voxel_undo_order.push_back(UndoKind::Surface);
            crate::input_sculpt::prune_front_undo_markers(
                &mut self.sculpt.voxel_undo_order, UndoKind::Surface, ev);
        } else {
            let ev = self.sculpt.voxel_surface_redo_stack.push(inverse);
            self.sculpt.voxel_redo_order.push_back(UndoKind::Surface);
            crate::input_sculpt::prune_front_undo_markers(
                &mut self.sculpt.voxel_redo_order, UndoKind::Surface, ev);
        }
        self.mark_dirty(DirtyFlags::RENDER);
        if crate::surface_validate::live_validate_enabled() {
            crate::surface_validate::live_validate_after_op(
                self, if redo { "surface_redo" } else { "surface_undo" });
        }
        true
    }

    /// Drop all Surface undo/redo entries + their order markers. Called when a revoxelization makes
    /// the vertex snapshots meaningless (the authoritative remesh changes topology) and by the
    /// wholesale undo resets.
    pub(crate) fn voxel_surface_clear_undo(&mut self) {
        self.sculpt.voxel_surface_undo_stack.clear();
        self.sculpt.voxel_surface_redo_stack.clear();
        self.sculpt.voxel_undo_order.retain(|&k| k != UndoKind::Surface);
        self.sculpt.voxel_redo_order.retain(|&k| k != UndoKind::Surface);
    }

    /// Re-voxelize the current (surface-edited) mesh into the density field over the dirty region.
    /// ASYNC: gathers the WHOLE object's chunk mesh (closed surface — sign computation needs it),
    /// spawns a worker running `region_mesh_to_sdf` on the dirty sub-lattice, and lands through the
    /// EXISTING `voxel_land_move_bake` sdf arm (`write_region_from_sdf` + ONE Cpu undo delta + chunk
    /// remesh marks) — no second landing path. Surface undo entries are cleared here (barrier).
    /// Returns false when there is nothing to do or a bake/rebuild is already in flight.
    pub(crate) fn voxel_surface_revoxelize(&mut self) -> bool {
        let Some(region) = self.sculpt.voxel.as_ref().and_then(|v| v.surface_dirty_region) else { return false };
        if self.sculpt.voxel.as_ref().map_or(true, |v| v.pending_move_bake.is_some() || v.pending_rebuild.is_some()) {
            return false;
        }
        let Some((res, bounds, voxel_size)) = self.sculpt.voxel.as_ref()
            .map(|v| (v.grid.res, v.grid.bounds, v.grid.voxel_size())) else { return false };
        // Pad the region a few cells so the iso band at the edit boundary blends seamlessly, and
        // clamp to the sealed border [1, res-2] like every density write.
        let lo = |c: u32| c.saturating_sub(3).max(1);
        let hi = |c: u32| (c + 3).min(res.saturating_sub(2));
        let region = [lo(region[0]), lo(region[1]), lo(region[2]), hi(region[3]), hi(region[4]), hi(region[5])];

        // Gather the whole object's current triangle mesh (voxel-LOCAL space) from the live chunks.
        let (mut verts, mut indices) = (Vec::<Vec3>::new(), Vec::<u32>::new());
        let chunk_list: Vec<core_math::scene_graph::MeshCacheId> = self.sculpt.voxel.as_ref()
            .map(|v| v.chunks.iter().zip(v.chunk_tris.iter())
                .filter(|(_, &t)| t > 0).map(|(c, _)| *c).collect())
            .unwrap_or_default();
        for cid in chunk_list {
            if let Some((cv, cidx)) = self.cache.mesh_cache.chunk_mesh_data(cid) {
                let base = verts.len() as u32;
                verts.extend(cv.iter().map(|v8| Vec3::new(v8[0], v8[1], v8[2])));
                indices.extend(cidx.iter().map(|&i| i + base));
            }
        }
        if verts.is_empty() || indices.len() < 3 {
            // Nothing to voxelize against — clear the flags so the mode isn't wedged.
            if let Some(v) = self.sculpt.voxel.as_mut() { v.surface_dirty_region = None; }
            return false;
        }

        // Barrier: the authoritative remesh on land invalidates every surface vertex snapshot.
        self.voxel_surface_clear_undo();
        self.sculpt.surface_stroke_snap.clear(); // discard (snapshots void after the remesh lands)
        self.sculpt.surface_stroke_ledger.clear();
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.surface_dirty_region = None;
            // The landing remesh re-derives every chunk from density → all geometry returns to
            // its home cells; the displaced-chunk registry is void.
            v.surface_moved_aabbs.clear();
        }

        let w = 2.0 * voxel_size; // iso band, same as the Move bake
        let (tx, rx) = std::sync::mpsc::channel();
        let spawn_ok = std::thread::Builder::new()
            .name("voxel-surface-revoxel".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let sdf = io_bridge::mesh_to_sdf::region_mesh_to_sdf(&verts, &indices, bounds, res, region)
                    .unwrap_or_default();
                eprintln!("[SURFACE] revoxel worker: {:.0}ms cells={} region=[{},{},{}]-[{},{},{}]",
                    t0.elapsed().as_secs_f32() * 1000.0, sdf.len(),
                    region[0], region[1], region[2], region[3], region[4], region[5]);
                let _ = tx.send(crate::MoveBakeResult {
                    union: region,
                    sdf,
                    w,
                    density_writes: Vec::new(),
                    res,
                    bounds,
                    inst_aabbs: Vec::new(),
                });
            }).is_ok();
        if !spawn_ok { return false; }
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.pending_move_bake = Some(crate::PendingMoveBake {
                rx, baked_n: 0, push_undo: true, grew: false, pre_grid: None,
            });
        }
        true
    }

    /// COPY-NORMAL REPAIR at surface-mode entry (fuzz-caught 2026-07-23, seeds 563/3985).
    ///
    /// Halo-duplicated border verts must carry BIT-IDENTICAL normals before any surface stroke
    /// runs: Inflate (and every normal-consuming displacement) is a pure function of
    /// (position, normal) — identical inputs keep the copies welded, divergent normals split
    /// their POSITIONS on the very first dab, permanently (the weld hashes by position, so a
    /// diverged pair never re-welds → compounding needle seed).
    ///
    /// Divergence source (measured: 136/3379 groups after one partial revoxelize): a PARTIAL
    /// remesh — revoxelize dirty-region landing, Move-bake landing — rebuilds only the chunks in
    /// the region; a border pair straddling the region boundary then has one FRESH mesher normal
    /// and one OLD (previous-session weld) normal, positions still bit-identical. The mesher
    /// itself is copy-consistent (per-cell, deterministic); only mixed-history arrays diverge.
    ///
    /// Repair (position-pure, one-shot at entry): group verts by exact position bits, find
    /// groups whose normals mismatch, accumulate area-weighted face normals for exactly those
    /// positions across ALL chunks (the complete cross-chunk 1-ring — same math as
    /// `weld_region_normals`), assign to every copy. Positions untouched → the displaced-AABB
    /// registry is NOT updated (this is not a surface edit; marking would forbid future density
    /// remeshes of these chunks). Pool slots receive a verts-only upload via the normal
    /// choke-point rules.
    pub(crate) fn voxel_surface_repair_copy_normals(&mut self) {
        let Some(v) = self.sculpt.voxel.as_ref() else { return };
        if !v.surface_mode { return; }
        let voxel_size = v.grid.voxel_size();
        let eps = voxel_size.max(1e-6) * 1e-3;
        let inv_eps = 1.0 / eps;
        let key = |p: Vec3| -> [i64; 3] {
            [(p.x * inv_eps).round() as i64, (p.y * inv_eps).round() as i64, (p.z * inv_eps).round() as i64]
        };
        let chunks: Vec<(u32, core_math::scene_graph::MeshCacheId)> = v.chunks.iter()
            .zip(v.chunk_tris.iter()).enumerate()
            .filter(|(_, (_, &t))| t > 0)
            .map(|(ci, (&cid, _))| (ci as u32, cid))
            .collect();
        // Pass 1: find positions whose copies carry mismatched normal BITS.
        let mut seen: std::collections::HashMap<[i64; 3], [u32; 3]> = std::collections::HashMap::new();
        let mut diverged: std::collections::HashSet<[i64; 3]> = std::collections::HashSet::new();
        for &(_, cid) in &chunks {
            let Some((verts, _)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            for v8 in verts {
                let k = key(Vec3::new(v8[0], v8[1], v8[2]));
                let nb = [v8[3].to_bits(), v8[4].to_bits(), v8[5].to_bits()];
                match seen.entry(k) {
                    std::collections::hash_map::Entry::Vacant(e) => { e.insert(nb); }
                    std::collections::hash_map::Entry::Occupied(e) => {
                        if *e.get() != nb { diverged.insert(k); }
                    }
                }
            }
        }
        if diverged.is_empty() { return; }
        // Pass 2: accumulate area-weighted face normals for the diverged positions across ALL
        // chunks (faces are owned by exactly one chunk → each face contributes once; both copies
        // of a position hit the same key → identical accumulated 1-ring).
        let mut acc: std::collections::HashMap<[i64; 3], Vec3> = std::collections::HashMap::new();
        for &(_, cid) in &chunks {
            let Some((verts, indices)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            for t in indices.chunks_exact(3) {
                let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
                if ia >= verts.len() || ib >= verts.len() || ic >= verts.len() { continue; }
                let a = Vec3::new(verts[ia][0], verts[ia][1], verts[ia][2]);
                let b = Vec3::new(verts[ib][0], verts[ib][1], verts[ib][2]);
                let c = Vec3::new(verts[ic][0], verts[ic][1], verts[ic][2]);
                let (ka, kb, kc) = (key(a), key(b), key(c));
                if !(diverged.contains(&ka) || diverged.contains(&kb) || diverged.contains(&kc)) {
                    continue;
                }
                let fnrm = (b - a).cross(c - a); // area-weighted
                for k in [ka, kb, kc] {
                    if diverged.contains(&k) {
                        *acc.entry(k).or_insert(Vec3::ZERO) += fnrm;
                    }
                }
            }
        }
        // Pass 3: assign the welded normal to every copy of a diverged position.
        let mut repaired_chunks = 0u32;
        for &(ci_u, cid) in &chunks {
            let Some((verts_ref, _)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            let mut verts: Option<Vec<[f32; 8]>> = None;
            for (vi, v8) in verts_ref.iter().enumerate() {
                let k = key(Vec3::new(v8[0], v8[1], v8[2]));
                if !diverged.contains(&k) { continue; }
                let Some(sum) = acc.get(&k) else { continue };
                let n = sum.normalize_or_zero();
                if n.length_squared() < 1e-12 { continue; }
                if v8[3] != n.x || v8[4] != n.y || v8[5] != n.z {
                    let vs = verts.get_or_insert_with(|| verts_ref.clone());
                    vs[vi][3] = n.x; vs[vi][4] = n.y; vs[vi][5] = n.z;
                }
            }
            if let Some(vs) = verts {
                // In-place mesh_cache write + pool verts-only mirror; NOT the surface-edit
                // choke-point (positions unchanged → no displaced-AABB registry entry).
                if self.cache.mesh_cache.update_chunk_verts_in_place(cid, vs) {
                    self.voxel_surface_pool_upload(ci_u, cid, false);
                    repaired_chunks += 1;
                }
            }
        }
        if repaired_chunks > 0 {
            eprintln!("[SURFACE] entry normal repair: {} diverged border positions re-welded across {repaired_chunks} chunks",
                diverged.len());
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    /// The ONE mode-toggle entry point (UI). Enforces the sync contract of both directions:
    ///   Voxel→Surface: flush + BLOCK-land any pending Move bake, heal the CPU grid from the atlas,
    ///     and fully drain dirty chunks so the mesh the user starts displacing is current — instant
    ///     otherwise (the mesh already exists).
    ///   Surface→Voxel: spawn the async revoxelization of the dirty region (strokes are paused by
    ///     the in-flight bake, exactly like a Move bake; the mesh keeps displaying meanwhile).
    pub(crate) fn voxel_surface_set_mode(&mut self, surface: bool) {
        let Some(cur) = self.sculpt.voxel.as_ref().map(|v| v.surface_mode) else { return };
        if cur == surface { return; }
        if surface {
            // Density → mesh sync point (reuses the save-path chokepoint machinery).
            let mut guard = 0u32;
            while self.sculpt.voxel.as_ref().map_or(false, |v|
                v.pending_move_bake.is_some() || !v.pending_move_grabs.is_empty()) {
                self.voxel_flush_pending_move();
                self.voxel_land_move_bake(true);
                guard += 1;
                if guard > 8 { break; }
            }
            self.materialize_cpu_from_atlas();
            // Drain queued dirty chunks AND any in-flight GPU-mesh batch while still in Voxel mode
            // (route unchanged). An in-flight batch left un-consumed would land AFTER the mode flip
            // and stomp mesh_cache — the surface stroke's authority — with pre-toggle vertices.
            let mut guard = 0u32;
            while self.sculpt.voxel.as_ref().map_or(false, |v| !v.dirty_chunks.is_empty())
                || self.gpu.as_ref().map_or(false, |g| g.voxel_mesh_inflight.is_some())
            {
                if let Some(g) = self.gpu.as_ref() {
                    if g.voxel_mesh_inflight.is_some() {
                        g.device.poll(eframe::wgpu::PollType::Wait).expect("GPU device polling failed"); // block-land the batch
                    }
                }
                self.voxel_flush_dirty_chunks();
                guard += 1;
                if guard > 512 { break; }
            }
            // Flip the mode FIRST: from here voxel_flush_dirty_chunks routes to the CPU mesher
            // (mesh_cache), never the zero-readback resident dispatch.
            self.sculpt.surface_stroke_snap.clear(); // mode entry: no stroke can be pending
            self.sculpt.surface_stroke_ledger.clear();
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.surface_mode = true;
                v.surface_dirty_region = None;
                v.surface_moved_aabbs.clear(); // fresh density-meshed chunks: all in home cells
                v.surface_pool_synced.clear(); // Gate 1: slots hold GPU-mesher pairs until first surface write
                // A density-only active brush would be un-strokable in Surface mode — fall back
                // to Draw (the UI greys the unsupported ones with a tooltip).
                if !surface_brush_supported(v.brush) {
                    v.set_active_brush(VoxelBrush::Draw);
                }
            }
            // GPU-RESIDENT chunks were meshed straight into the pool (zero readback) — their
            // mesh_cache copies (and chunk_tris) are stale or missing. Surface mode displays,
            // edits, picks, and later revoxelizes FROM mesh_cache, so rebuild exactly those
            // chunks through the CPU mesher now (the flush routes CPU with surface_mode set).
            let resident_dirty: Vec<u32> = self.gpu.as_ref().map(|g| {
                g.voxel_chunk_gpu.iter().enumerate()
                    .filter_map(|(i, s)| s.as_ref().map(|_| i as u32)).collect()
            }).unwrap_or_default();
            if !resident_dirty.is_empty() {
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    v.dirty_chunks.extend(resident_dirty);
                }
                let mut guard = 0u32;
                while self.sculpt.voxel.as_ref().map_or(false, |v| !v.dirty_chunks.is_empty()) {
                    self.voxel_flush_dirty_chunks();
                    guard += 1;
                    if guard > 512 { break; }
                }
            }
            // COPY-NORMAL REPAIR (fuzz seeds 563/3985): after every drain above the chunk arrays
            // can mix histories (fresh mesher chunks vs pre-toggle arrays with stroke-welded
            // normals). Diverged border-copy normals split copy POSITIONS on the first
            // normal-driven dab — re-weld exactly the diverged positions before any stroke runs.
            self.voxel_surface_repair_copy_normals();
            self.toast("Surface mode — brushes displace the mesh directly");
        } else {
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.surface_mode = false;
                // Gate 1: back in Voxel mode the GPU mesher owns the pool slots again — the
                // CPU-synced marks no longer describe the slot contents.
                v.surface_pool_synced.clear();
            }
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some()) {
                if self.voxel_surface_revoxelize() {
                    self.toast("Voxel mode — voxelizing surface edits\u{2026}");
                } else {
                    self.toast("Voxel mode (revoxelize deferred — bake in flight)");
                }
            } else {
                self.toast("Voxel mode");
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }
}

// ============================================================================
// Regression tests — the ALL-COPIES-MOVE invariant (needle-spike class).
// Chunk meshes duplicate their halo border vertices; a stroke is correct only
// if every copy of a position receives bit-identical displacement + normals.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use core_math::voxel_sculpt::{FalloffPreset, VoxelBrush};

    /// Two "chunks" sharing a duplicated border edge (verts (1,0,0) and (1,1,0)),
    /// one quad each — the minimal halo-duplication topology.
    fn two_border_chunks() -> (Vec<[f32; 8]>, Vec<u32>, Vec<[f32; 8]>, Vec<u32>) {
        let n = [0.0, 0.0, 1.0];
        let v = |x: f32, y: f32| [x, y, 0.0, n[0], n[1], n[2], 0.0, 0.0];
        // Chunk A: quad (0,0)-(1,1). Chunk B: quad (1,0)-(2,1). Shared: x=1 edge.
        let va = vec![v(0.0, 0.0), v(1.0, 0.0), v(1.0, 1.0), v(0.0, 1.0)];
        let ia = vec![0, 1, 2, 0, 2, 3];
        let vb = vec![v(1.0, 0.0), v(2.0, 0.0), v(2.0, 1.0), v(1.0, 1.0)];
        let ib = vec![0, 1, 2, 0, 2, 3];
        (va, ia, vb, ib)
    }

    fn pos_key(v: &[f32; 8]) -> [u32; 3] {
        [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()]
    }

    /// A tip alpha that is BLACK on one half must leave that half's verts bit-unmoved
    /// while the white half still deposits — the Photoshop/ZBrush brush-shape contract
    /// for BOTH poly sculpt and voxel Surface mode (they share this kernel).
    #[test]
    fn tip_alpha_masks_the_stamp() {
        // Flat vert grid in the XY plane; dab straight along +Z, radius covers the grid.
        let mut verts: Vec<[f32; 8]> = Vec::new();
        for x in -3..=3i32 {
            for y in -3..=3i32 {
                verts.push([x as f32 * 0.1, y as f32 * 0.1, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            }
        }
        // brush_tip_basis(+Z) = (t=+Y, b=−X) → tip-u maps to world +Y. White u<0, black u>=0.
        let tip = core_math::voxel_sculpt::BrushTipAlpha::from_fn(32, |u, _v| {
            if u < 0.0 { 1.0 } else { 0.0 }
        });
        let dabs = [SurfaceDab {
            pos: Vec3::ZERO,
            dir: Vec3::Z,
            radius: 0.6,
            strength: 1.0,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        }];
        displace_verts_for_dabs(
            &mut verts, &dabs, VoxelBrush::Draw, 1.0, 0.0, FalloffPreset::Smooth,
            &[], None, 0.1, Some((&tip, 1.0)), None,
        );
        let (mut moved_white, mut moved_black) = (0u32, 0u32);
        for v8 in &verts {
            if v8[2].abs() > 1e-7 {
                // Displaced along +Z — which tip half did it sit in?
                if v8[1] < -0.05 { moved_white += 1; } else if v8[1] > 0.05 { moved_black += 1; }
            }
        }
        assert!(moved_white > 0, "the white tip half must deposit");
        assert_eq!(moved_black, 0, "the black tip half must be fully masked");
    }

    /// AXIS-COLLAPSE GUARD: even a saturating-strength Pinch must never land a vert ON
    /// the stroke axis — every vert keeps ≥ PINCH_AXIS_KEEP·r, so triangles keep area
    /// (the owner's "mesh broke, brush dead in that zone" crater class, 2026-08-19).
    #[test]
    fn pinch_never_collapses_verts_onto_the_axis() {
        // Flat grid in the XY plane; dab dir = +Z, stroke along +X → axis = X line at y=0.
        let mut verts: Vec<[f32; 8]> = Vec::new();
        for x in -4..=4i32 {
            for y in -4..=4i32 {
                if y == 0 { continue; } // skip verts already on the axis (nothing to guard)
                verts.push([x as f32 * 0.05, y as f32 * 0.05, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            }
        }
        let r = 0.5f32;
        let dabs = [SurfaceDab {
            pos: Vec3::ZERO,
            dir: Vec3::Z,
            radius: r,
            strength: 4.0, // saturates the old clamp — used to land the whole core ON the axis
            stroke_tan: Vec3::X,
            tip_rotation: 0.0,
        }];
        // Several dabs: repeated pinching must also never cross the keep-out band.
        for _ in 0..8 {
            displace_verts_for_dabs(
                &mut verts, &dabs, VoxelBrush::Pinch, 1.0, 0.0, FalloffPreset::Smooth,
                &[], None, 0.1, None, None,
            );
        }
        let keep = PINCH_AXIS_KEEP * r;
        for v8 in &verts {
            assert!(v8[1].is_finite(), "pinch produced a non-finite position");
            assert!(
                v8[1].abs() >= keep * 0.999,
                "vert squeezed inside the keep-out band: |y|={} < {keep}",
                v8[1].abs()
            );
        }
    }

    /// A dab spanning the chunk border must displace BOTH copies of every duplicated
    /// border vertex to bit-identical positions (the pure-function property the
    /// candidate-chunk selection relies on).
    #[test]
    fn dab_across_border_displaces_all_copies_identically() {
        let (mut va, _ia, mut vb, _ib) = two_border_chunks();
        let dabs = [SurfaceDab {
            pos: Vec3::new(1.0, 0.5, 0.0),
            dir: Vec3::Z,
            radius: 1.4,
            strength: 1.0,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        }];
        let ma = displace_verts_for_dabs(
            &mut va, &dabs, VoxelBrush::Draw, -1.0, 0.5, FalloffPreset::Smooth, &[], None, 0.1, None, None);
        let mb = displace_verts_for_dabs(
            &mut vb, &dabs, VoxelBrush::Draw, -1.0, 0.5, FalloffPreset::Smooth, &[], None, 0.1, None, None);
        assert!(ma && mb, "the dab must move verts in both chunks");
        // Every position that exists in both chunks must be bit-identical after the dab.
        let mut shared = 0;
        for a in &va {
            for b in &vb {
                // Match copies by their ORIGINAL duplication (x==1 border) — after identical
                // displacement they must still agree bit-for-bit.
                if pos_key(a) == pos_key(b) {
                    shared += 1;
                }
            }
        }
        assert_eq!(shared, 2, "both border copies must remain bit-identical (needle = a diverged copy)");
    }

    /// After a cross-border displacement, the weld must give every duplicated copy the
    /// bit-identical (complete 1-ring) normal.
    #[test]
    fn weld_gives_border_copies_identical_normals() {
        let (mut va, ia, mut vb, ib) = two_border_chunks();
        let dabs = [SurfaceDab {
            pos: Vec3::new(1.0, 0.5, 0.0),
            dir: Vec3::Z,
            radius: 1.2,
            strength: 1.0,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        }];
        displace_verts_for_dabs(
            &mut va, &dabs, VoxelBrush::Draw, 1.0, 0.5, FalloffPreset::Smooth, &[], None, 0.1, None, None);
        displace_verts_for_dabs(
            &mut vb, &dabs, VoxelBrush::Draw, 1.0, 0.5, FalloffPreset::Smooth, &[], None, 0.1, None, None);
        let mut work = vec![
            ChunkWork { ci_u: 0, cid: core_math::scene_graph::MeshCacheId(0), verts: va, indices: ia, moved: true, normals_changed: false },
            ChunkWork { ci_u: 1, cid: core_math::scene_graph::MeshCacheId(1), verts: vb, indices: ib, moved: true, normals_changed: false },
        ];
        weld_region_normals(&mut work, 0.05, |_| true, None);
        let (a, b) = (&work[0].verts, &work[1].verts);
        let mut checked = 0;
        for va in a.iter() {
            for vb in b.iter() {
                if pos_key(va) == pos_key(vb) {
                    assert_eq!(va[3].to_bits(), vb[3].to_bits(), "welded normal.x must be bit-identical");
                    assert_eq!(va[4].to_bits(), vb[4].to_bits(), "welded normal.y must be bit-identical");
                    assert_eq!(va[5].to_bits(), vb[5].to_bits(), "welded normal.z must be bit-identical");
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 2, "expected the two duplicated border verts to be compared");
    }

    /// The displaced-chunk registry must re-include a chunk whose CURRENT vertex AABB was
    /// dragged far from its home density cells — the root cause of the needle spikes: grid-cell
    /// chunk lists alone forget who OWNS far-displaced geometry.
    #[test]
    fn moved_chunk_reenters_candidate_set_by_current_aabb() {
        let mut moved: std::collections::HashMap<u32, (Vec3, Vec3)> = std::collections::HashMap::new();
        // Chunk 7's geometry was carved deep: current AABB sits around (10,10,10).
        moved.insert(7, (Vec3::splat(9.5), Vec3::splat(11.0)));
        // A dab at (10.2, 10.2, 10.2) with pad 1.0 overlaps that AABB → chunk 7 must be listed.
        let hit = moved_chunks_hit(&moved, &[(Vec3::splat(10.2), 1.0)]);
        assert_eq!(hit, vec![7]);
        // A dab far away must not.
        let miss = moved_chunks_hit(&moved, &[(Vec3::splat(50.0), 1.0)]);
        assert!(miss.is_empty());
    }

    /// Support-scoped weld must produce BIT-IDENTICAL normals to the unscoped weld for every
    /// vertex the `within` set accepts (the scoping only skips faces that cannot touch an
    /// assigned vertex's 1-ring). Regression for the weld-scoping optimization.
    #[test]
    fn support_scoped_weld_matches_unscoped_inside_support() {
        let (chunks, dabs, vs) = bench_scene(4, 4);
        let displaced: Vec<(Vec<[f32; 8]>, Vec<u32>)> = chunks
            .iter()
            .map(|(v, i)| {
                let mut v = v.clone();
                displace_verts_for_dabs(
                    &mut v, &dabs, VoxelBrush::Draw, 1.0, 0.5, FalloffPreset::Smooth, &[], None, vs, None, None);
                (v, i.clone())
            })
            .collect();
        let mk_work = || -> Vec<ChunkWork> {
            displaced.iter().enumerate().map(|(k, (v, i))| ChunkWork {
                ci_u: k as u32, cid: core_math::scene_graph::MeshCacheId(k as u64),
                verts: v.clone(), indices: i.clone(), moved: true, normals_changed: false,
            }).collect()
        };
        let within = |p: Vec3| dabs.iter().any(|d| {
            let wr = d.radius + 2.0 * vs;
            (p - d.pos).length_squared() <= wr * wr
        });
        let support = {
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            for d in &dabs {
                let wr = d.radius + 2.0 * vs;
                lo = lo.min(d.pos - Vec3::splat(wr));
                hi = hi.max(d.pos + Vec3::splat(wr));
            }
            (lo, hi)
        };
        let mut full = mk_work();
        weld_region_normals(&mut full, vs, within, None);
        let mut scoped = mk_work();
        weld_region_normals(&mut scoped, vs, within, Some(support));
        for (wf, ws) in full.iter().zip(scoped.iter()) {
            for (vf, vsv) in wf.verts.iter().zip(ws.verts.iter()) {
                let p = Vec3::new(vf[0], vf[1], vf[2]);
                if !within(p) { continue; }
                for c in 3..6 {
                    assert_eq!(vf[c].to_bits(), vsv[c].to_bits(),
                        "scoped weld diverged from unscoped inside the support set");
                }
            }
        }
    }

    /// Representative surface-stroke frame for a 256³-class sphere: a sheet of 17×17-vert chunk
    /// meshes (the smooth-surface Surface-Nets shape) + an 8-dab batch crossing the middle.
    fn bench_scene(nx: usize, ny: usize) -> (Vec<(Vec<[f32; 8]>, Vec<u32>)>, Vec<SurfaceDab>, f32) {
        let vs = 2.0 / 255.0; // 256³ over ±1 bounds
        let chunk_w = 16.0 * vs;
        let mut chunks = Vec::new();
        for cy in 0..ny {
            for cx in 0..nx {
                let (mut verts, mut idx) = (Vec::new(), Vec::new());
                for j in 0..17u32 {
                    for i in 0..17u32 {
                        let x = cx as f32 * chunk_w + i as f32 * vs;
                        let y = cy as f32 * chunk_w + j as f32 * vs;
                        verts.push([x, y, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                    }
                }
                for j in 0..16u32 {
                    for i in 0..16u32 {
                        let a = j * 17 + i;
                        idx.extend_from_slice(&[a, a + 1, a + 18, a, a + 18, a + 17]);
                    }
                }
                chunks.push((verts, idx));
            }
        }
        let r = 0.1f32; // ~13-voxel brush
        let mid = ny as f32 * chunk_w * 0.5;
        let dabs: Vec<SurfaceDab> = (0..8)
            .map(|k| SurfaceDab {
                pos: Vec3::new(0.2 + k as f32 * r * 0.15, mid, 0.0),
                dir: Vec3::Z,
                radius: r,
                strength: 1.0,
                stroke_tan: Vec3::X,
                tip_rotation: 0.0,
            })
            .collect();
        (chunks, dabs, vs)
    }

    /// Manual perf probe (run: `cargo test -p app --release bench_surface -- --ignored --nocapture`).
    /// Prints per-frame ms of the displace + weld core for a 256³-class stroke frame, unscoped vs
    /// support-scoped weld. Not asserted on time (machine-dependent) — numbers for the log.
    #[test]
    #[ignore]
    fn bench_surface_dab_frame_cost() {
        // 5×5 sheet ≈ a large-brush candidate set (25 chunks × 289 verts / 512 tris).
        let (chunks, dabs, vs) = bench_scene(5, 5);
        let within = |p: Vec3| dabs.iter().any(|d| {
            let wr = d.radius + 2.0 * vs;
            (p - d.pos).length_squared() <= wr * wr
        });
        let support = {
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            for d in &dabs {
                let wr = d.radius + 2.0 * vs;
                lo = lo.min(d.pos - Vec3::splat(wr));
                hi = hi.max(d.pos + Vec3::splat(wr));
            }
            (lo, hi)
        };
        let frame = |support: Option<(Vec3, Vec3)>| -> f32 {
            let iters = 50;
            let t = std::time::Instant::now();
            for _ in 0..iters {
                let mut work: Vec<ChunkWork> = chunks.iter().enumerate().map(|(k, (v, i))| {
                    let mut verts = v.clone(); // the apply path's working copy
                    displace_verts_for_dabs(
                        &mut verts, &dabs, VoxelBrush::Draw, 1.0, 0.5, FalloffPreset::Smooth, &[], None, vs, None, None);
                    ChunkWork {
                        ci_u: k as u32, cid: core_math::scene_graph::MeshCacheId(k as u64),
                        verts, indices: i.clone(), moved: true, normals_changed: false,
                    }
                }).collect();
                weld_region_normals(&mut work, vs, within, support);
                std::hint::black_box(&work);
            }
            t.elapsed().as_secs_f32() * 1000.0 / iters as f32
        };
        let unscoped = frame(None);
        let scoped = frame(Some(support));
        println!("[BENCH] surface dab frame (25 chunks, 8 dabs): unscoped weld {unscoped:.3} ms/frame, support-scoped {scoped:.3} ms/frame");
    }

    // ------------------------------------------------------------------------
    // CLAY FIN CLASS (owner-reported 2026-07-23): deep-carve trench walls have
    // near-opposing normals; the old unconditional area-frame fit (plain mean)
    // produced a mid-air plane with a garbage normal and the Clay converge
    // dragged one side of the footprint toward it every dab → ragged fins.
    // ------------------------------------------------------------------------

    /// Steep 80°-wall trench (normals nearly oppose, mean length ≈ 0.18) with a slight
    /// footprint asymmetry (one extra left-wall column — real carve footprints are ragged).
    /// Returns (verts, neighbour index pairs at spacing `h` on the same wall).
    fn steep_trench(h: f32) -> (Vec<[f32; 8]>, Vec<(usize, usize)>) {
        let nl = Vec3::new(0.985, 0.174, 0.0); // left wall faces into the trench (+X), 10° up
        let nr = Vec3::new(-0.985, 0.174, 0.0);
        let mut verts: Vec<[f32; 8]> = Vec::new();
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for (wx, n) in [(-0.1f32, nl), (0.1f32, nr)] {
            for iz in 0..8 {
                let col0 = verts.len();
                for iy in 0..8 {
                    let y = -0.4 + iy as f32 * h;
                    let z = -0.2 + iz as f32 * h;
                    verts.push([wx, y, z, n.x, n.y, n.z, 0.0, 0.0]);
                    if iy > 0 {
                        pairs.push((col0 + iy - 1, col0 + iy)); // vertical wall neighbours
                    }
                }
            }
        }
        // Footprint asymmetry: an extra left-wall column so the cancelled mean has a
        // nonzero lateral residual (the garbage direction the old fit normalized).
        for iy in 0..8 {
            let y = -0.4 + iy as f32 * h;
            verts.push([-0.1, y, 0.22, nl.x, nl.y, nl.z, 0.0, 0.0]);
        }
        (verts, pairs)
    }

    fn trench_dab(k: usize, r: f32) -> SurfaceDab {
        SurfaceDab {
            pos: Vec3::new(0.0, -0.2, -0.1 + k as f32 * 0.03), // deep in the trench, moving along z
            dir: Vec3::Y,
            radius: r,
            strength: 1.0,
            stroke_tan: Vec3::Z,
            tip_rotation: 0.0,
        }
    }

    /// Multi-dab Clay CARVE down the trench with the plane refit from the CURRENT verts each
    /// dab (exactly the per-frame loop `voxel_surface_apply_dabs` runs), through the PRODUCTION
    /// fit (`accumulate_area_frames` + `resolve_area_frame`, front-face refs = dab dir).
    /// NO-FIN properties asserted:
    ///   (a) per-dab travel ≤ 0.5·r (the parabolic band's design cap);
    ///   (b) same-wall neighbours never stretch beyond 3× their original spacing.
    /// With the degenerate trench plane REJECTED (mean normal 0.18 < SURFACE_MIN_AREA_NORMAL)
    /// the dabs fail closed and both hold. See the `_head_plane_fit` control below for the
    /// numeric proof that the old unconditional fit violates (b) — 3.80× measured stretch.
    #[test]
    fn clay_carve_in_steep_trench_leaves_no_fins() {
        let h = 0.05f32;
        let (mut verts, pairs) = steep_trench(h);
        let r = 0.3f32;
        for k in 0..8 {
            let dab = trench_dab(k, r);
            let probes = [(dab.pos, dab.radius)];
            let refs = [dab.dir];
            let mut acc = [(Vec3::ZERO, Vec3::ZERO, 0u32)];
            accumulate_area_frames(&mut acc, &verts, &probes, &refs, SURFACE_AREA_NORMAL_FACTOR);
            let plane = resolve_area_frame(acc[0].0, acc[0].1, acc[0].2);
            let before = verts.clone();
            displace_verts_for_dabs(
                &mut verts, std::slice::from_ref(&dab), VoxelBrush::Clay, -1.0, 0.5,
                FalloffPreset::Smooth, &[plane], None, 0.1, None, None);
            for (b, a) in before.iter().zip(verts.iter()) {
                let d = (Vec3::new(a[0], a[1], a[2]) - Vec3::new(b[0], b[1], b[2])).length();
                assert!(d <= 0.5 * r + 1e-4, "per-dab clay travel {d} exceeds the 0.5·r cap");
            }
        }
        let max_stretch = pairs.iter().map(|&(i, j)| {
            let a = Vec3::new(verts[i][0], verts[i][1], verts[i][2]);
            let b = Vec3::new(verts[j][0], verts[j][1], verts[j][2]);
            (a - b).length() / h
        }).fold(0.0f32, f32::max);
        assert!(max_stretch <= 3.0,
            "fin detected: neighbour spacing stretched {max_stretch:.2}× (limit 3×)");
    }

    /// NEGATIVE CONTROL — reproduces the shipped bug numerically. Feed the Clay kernel the
    /// plane the OLD fit produced (plain mean of the whole footprint, no front-face filter, no
    /// agreement threshold): the mid-air garbage plane must stretch same-wall neighbours past
    /// the fin limit within a few dabs (measured 3.80× on this scene). If this control ever
    /// stops failing the no-fin property, the repro is no longer valid and the guard above
    /// proves nothing — investigate.
    #[test]
    fn clay_carve_head_plane_fit_produces_fins_control() {
        let h = 0.05f32;
        let (mut verts, pairs) = steep_trench(h);
        let r = 0.3f32;
        for k in 0..8 {
            let dab = trench_dab(k, r);
            let probes = [(dab.pos, dab.radius)];
            // OLD fit: no refs, and normalize the mean unconditionally (no threshold).
            let mut acc = [(Vec3::ZERO, Vec3::ZERO, 0u32)];
            accumulate_area_frames(&mut acc, &verts, &probes, &[], SURFACE_AREA_NORMAL_FACTOR);
            let plane = (acc[0].2 > 0).then(|| {
                let inv = 1.0 / acc[0].2 as f32;
                (acc[0].0 * inv, (acc[0].1 * inv).normalize_or_zero())
            });
            displace_verts_for_dabs(
                &mut verts, std::slice::from_ref(&dab), VoxelBrush::Clay, -1.0, 0.5,
                FalloffPreset::Smooth, &[plane], None, 0.1, None, None);
        }
        let max_stretch = pairs.iter().map(|&(i, j)| {
            let a = Vec3::new(verts[i][0], verts[i][1], verts[i][2]);
            let b = Vec3::new(verts[j][0], verts[j][1], verts[j][2]);
            (a - b).length() / h
        }).fold(0.0f32, f32::max);
        assert!(max_stretch > 3.0,
            "expected the old unconditional plane fit to produce fins (got {max_stretch:.2}×) — \
             the repro no longer reproduces; re-investigate before trusting the guard");
    }

    /// The agreement threshold: near-cancelling normal sums resolve to None (no stable plane);
    /// agreeing normals resolve to Some.
    #[test]
    fn degenerate_area_frame_resolves_none() {
        // 10 verts, mean normal length ≈ 0.153 (below SURFACE_MIN_AREA_NORMAL 0.30) → None.
        let sum_n = Vec3::new(0.3, 1.5, 0.0);
        assert!(resolve_area_frame(Vec3::ZERO, sum_n, 10).is_none());
        // Agreeing normals (all ~+Y) → Some, unit +Y.
        let (c, n) = resolve_area_frame(Vec3::splat(10.0), Vec3::Y * 10.0, 10).unwrap();
        assert_eq!(c, Vec3::splat(1.0));
        assert!((n - Vec3::Y).length() < 1e-6);
        // Empty footprint → None.
        assert!(resolve_area_frame(Vec3::ZERO, Vec3::ZERO, 0).is_none());
    }

    /// Front-face filtering: back-facing verts (an overhang lip / the far trench wall) must not
    /// tilt or lift the plane — the frame is fit only from geometry facing the brush.
    #[test]
    fn front_face_filter_keeps_floor_anchored_plane() {
        let mut verts: Vec<[f32; 8]> = Vec::new();
        // Floor at y = -0.4, normals +Y.
        for ix in 0..6 {
            for iz in 0..6 {
                verts.push([-0.125 + ix as f32 * 0.05, -0.4, -0.125 + iz as f32 * 0.05,
                            0.0, 1.0, 0.0, 0.0, 0.0]);
            }
        }
        // Overhang lip above, normals pointing DOWN (back-facing to a +Y-referenced dab).
        for ix in 0..6 {
            verts.push([-0.125 + ix as f32 * 0.05, -0.1, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0]);
        }
        let probes = [(Vec3::new(0.0, -0.3, 0.0), 0.3f32)];
        let mut acc = [(Vec3::ZERO, Vec3::ZERO, 0u32)];
        accumulate_area_frames(&mut acc, &verts, &probes, &[Vec3::Y], SURFACE_AREA_NORMAL_FACTOR);
        let (c, n) = resolve_area_frame(acc[0].0, acc[0].1, acc[0].2)
            .expect("floor-facing footprint must yield a stable plane");
        assert!(n.dot(Vec3::Y) > 0.99, "plane normal must stay floor-aligned, got {n:?}");
        assert!((c.y - (-0.4)).abs() < 1e-4, "plane center must sit on the floor, got {c:?}");
    }

    /// AABB helper sanity (feeds the registry the choke-point writes).
    #[test]
    fn chunk_verts_aabb_min_max() {
        let verts = vec![
            [1.0, 2.0, 3.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            [-1.0, 5.0, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0],
        ];
        let (lo, hi) = chunk_verts_aabb(&verts).unwrap();
        assert_eq!(lo, Vec3::new(-1.0, 2.0, 0.5));
        assert_eq!(hi, Vec3::new(1.0, 5.0, 3.0));
        assert!(chunk_verts_aabb(&[]).is_none());
    }
}
