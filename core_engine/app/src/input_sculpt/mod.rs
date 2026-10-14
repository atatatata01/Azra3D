//! Sculpt input handlers — moved from input.rs.

use eframe::egui;
use glam::Vec3;
use core_math::sdf_volume::BrushType;
use crate::*;

mod stroke;
mod brush_apply;
mod falloff;
mod ui_state;


// ---- P-B chunked voxel meshing helpers -------------------------------------------------
/// Cells per chunk axis. A brush touches only a handful of chunks, so re-meshing + re-uploading
/// just those keeps the per-frame cost proportional to the brush, not the whole surface.
///
/// Size matters a LOT: a chunk re-mesh scans the WHOLE (chunk+1)³ cell range every time it's
/// touched, no matter how small the edit. Benchmarked per small-brush edit @192³ (opt-3): 32³ =
/// 5.8ms (≈ the live 8.8ms hitch at opt-2), 16³ = 0.84ms (7×), 8³ = 0.23ms. 16 is the sweet spot —
/// 8³ would mean ~13.8k chunk buffers (heavy bookkeeping + a many-frame upload on create), while
/// 16³ keeps it at ~1.7k for 192³ yet makes a re-meshed chunk roughly brush-sized.
pub(crate) const VOXEL_CHUNK: u32 = 16;

/// Iteration count for the "Smooth All" panel button — one click runs this many global Laplacian
/// relax passes over the whole sculpt. Enough to visibly relax noise/steps without over-melting fine
/// detail in a single press (repeat the click for more).
pub(crate) const SMOOTH_ALL_ITERS: u32 = 3;

/// Starting solid shape for a new voxel sculpt object (Add menu / left panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoxelCreateShape {
    Sphere,
    Box,
    Cylinder,
}

/// Remove the first `n` `kind` markers from the FRONT of the shared `voxel_undo_order` stream. Called
/// after a backing stack/ring evicts its `n` OLDEST entries, so the order stream (which interleaves
/// Cpu/Atlas) can never outlive its entries. Eviction is oldest-first and markers are FIFO, so the
/// surplus markers are always the oldest of that kind at the front — removing the first `n` keeps the
/// stream aligned exactly (each kind still owns its own backing stack). See finding #4.
pub(crate) fn prune_front_undo_markers(order: &mut std::collections::VecDeque<UndoKind>, kind: UndoKind, mut n: usize) {
    if n == 0 { return; }
    order.retain(|&k| if n > 0 && k == kind { n -= 1; false } else { true });
}

/// Chunks per axis for a grid resolution = ceil((res-1) cells / VOXEL_CHUNK).
pub(crate) fn voxel_chunk_dim(res: u32) -> u32 {
    let cells = res.saturating_sub(1).max(1);
    (cells + VOXEL_CHUNK - 1) / VOXEL_CHUNK
}

/// Anchor cell range [a0..=a1] (clamped) for chunk linear index `ci` in a `chunk_dim³` grid.
pub(crate) fn voxel_chunk_anchor(ci: u32, chunk_dim: u32, res: u32) -> ([u32; 3], [u32; 3]) {
    let cmax = res.saturating_sub(1).max(1) - 1;
    let (cgx, cgy, cgz) = (ci % chunk_dim, (ci / chunk_dim) % chunk_dim, ci / (chunk_dim * chunk_dim));
    let a0 = [cgx * VOXEL_CHUNK, cgy * VOXEL_CHUNK, cgz * VOXEL_CHUNK];
    let a1 = [
        ((cgx + 1) * VOXEL_CHUNK - 1).min(cmax),
        ((cgy + 1) * VOXEL_CHUNK - 1).min(cmax),
        ((cgz + 1) * VOXEL_CHUNK - 1).min(cmax),
    ];
    (a0, a1)
}

/// Inclusive range of chunk coords (per axis) touched by a VOXEL cell AABB, expanded by 1 cell on
/// BOTH sides. A chunk's Surface-Nets mesh reads a ±1 cell halo around its anchor range — the −1
/// low halo (`mesh_region`'s `vx0 = a0-1`) AND the +1 corner of its last cell (`cx+1` at `cx=a1`).
/// So an edit near EITHER chunk boundary changes the neighbour chunk's seam. Expanding only the min
/// side left high-boundary edits stale → torn seams / holes at chunk boundaries when carving across
/// them (the visible "chunk tearing" during sculpt). ±1 cell here = exactly that halo dependency.
pub(crate) fn voxel_chunks_for_region(region: [u32; 6], chunk_dim: u32, res: u32) -> [u32; 6] {
    let cmax = res.saturating_sub(1).max(1) - 1;
    let lo = |v: u32| (v.saturating_sub(1) / VOXEL_CHUNK).min(chunk_dim - 1);
    let hi = |v: u32| ((v.min(cmax) + 1) / VOXEL_CHUNK).min(chunk_dim - 1);
    [lo(region[0]), lo(region[1]), lo(region[2]), hi(region[3]), hi(region[4]), hi(region[5])]
}

/// Deterministic sparse-atlas allocation set for the final stamp AABBs. Unlike mesh-chunk dirty
/// marking, this has no Surface-Nets apron: the GPU brush writes only cells inside each exact AABB.
/// Sorting/dedup means overlap and symmetry can never create duplicate first-touch undo reports.
pub(super) fn voxel_bricks_for_regions<I>(regions: I, brick_res: u32) -> Vec<u32>
where
    I: IntoIterator<Item = [u32; 6]>,
{
    if brick_res == 0 { return Vec::new(); }
    let mut bricks = Vec::new();
    let max_b = brick_res - 1;
    let brick = core_math::voxel_sculpt::VOXEL_BRICK_SIZE;
    for [x0, y0, z0, x1, y1, z1] in regions {
        if x0 > x1 || y0 > y1 || z0 > z1 { continue; }
        let bx0 = (x0 / brick).min(max_b);
        let by0 = (y0 / brick).min(max_b);
        let bz0 = (z0 / brick).min(max_b);
        let bx1 = (x1 / brick).min(max_b);
        let by1 = (y1 / brick).min(max_b);
        let bz1 = (z1 / brick).min(max_b);
        for bz in bz0..=bz1 {
            for by in by0..=by1 {
                for bx in bx0..=bx1 {
                    bricks.push(bx + by * brick_res + bz * brick_res * brick_res);
                }
            }
        }
    }
    bricks.sort_unstable();
    bricks.dedup();
    bricks
}

/// Apply per-dab randomization only to dabs produced this frame, then restore the FIFO backlog in
/// front. Deferred dabs are already fully resolved (surface direction, tip orientation, projection,
/// front-face filtering, and dynamics); touching them again would make stroke shape depend on how
/// many frames the budget needed to drain it.
pub(super) fn apply_fresh_dab_dynamics_and_prepend_deferred(
    fresh: &mut Vec<core_math::sculpt_layer::StrokeSample>,
    deferred: &mut Vec<core_math::sculpt_layer::StrokeSample>,
    radius: f32,
    dynamics: core_math::sculpt_layer::BrushDynamics,
    seed: u32,
) {
    core_math::sculpt_layer::apply_brush_dynamics(fresh, radius, dynamics, seed);
    if deferred.is_empty() { return; }
    let mut ready = std::mem::take(deferred);
    ready.append(fresh);
    *fresh = ready;
}

/// `apply_brush_dynamics` advances its random stream by this amount for every dab. Keeping the
/// absolute dab index in the stroke makes one gesture produce the same dynamics whether its samples
/// arrive in one frame or are split across several frames.
pub(super) const BRUSH_DYNAMICS_DAB_STRIDE: u32 = 0x9E37_79B9;

#[inline]
pub(crate) fn brush_dynamics_seed_at(stroke_seed: u32, dab_index: u32) -> u32 {
    stroke_seed.wrapping_add(dab_index.wrapping_mul(BRUSH_DYNAMICS_DAB_STRIDE))
}

#[inline]
pub(super) fn random_tip_rotation(position: glam::Vec3, stroke_seed: u32, dab_index: u32) -> f32 {
    let seed = ((position.x * 4096.0) as u32)
        .wrapping_add(((position.y * 4096.0) as u32).wrapping_mul(7))
        .wrapping_add(((position.z * 4096.0) as u32).wrapping_mul(13))
        .wrapping_add(brush_dynamics_seed_at(stroke_seed, dab_index));
    let u = core_math::sculpt_layer::dyn_hash01(seed);
    (u * 2.0 - 1.0) * std::f32::consts::PI
}

/// Resolve per-dab tip-alpha rotation into `pts[i].tip_rotation` — Alpha Orientation
/// (ZBrush / 3D-Coat): Fixed / Follow Stroke / View / Random, plus the Angle offset.
/// ONE implementation shared by the voxel stamp producer and the poly stroke driver so the
/// two paths cannot drift. Run BEFORE `apply_brush_dynamics` so Random Rotation ADDS on top.
///
/// `stroke_prev_for_tip` = last stamp position of the PREVIOUS frame (Follow Stroke fallback
/// when this frame carries a single sample). `dab_start` = the stroke's absolute dab index of
/// `pts[0]`, so a gesture split across frames keeps one continuous random stream.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_tip_rotations(
    pts: &mut [core_math::sculpt_layer::StrokeSample],
    tip_orient: u32,
    tip_follow_stroke_legacy: bool,
    tip_angle_deg: f32,
    cam_up: glam::Vec3,
    stroke_seed: u32,
    dab_start: u32,
    stroke_prev_for_tip: Option<glam::Vec3>,
) {
    use crate::TipOrientation;
    let orient = {
        let o = TipOrientation::from_u32(tip_orient);
        // Legacy checkbox: if follow bool set and orient still Fixed, treat as Follow.
        if tip_follow_stroke_legacy && o == TipOrientation::Fixed {
            TipOrientation::FollowStroke
        } else {
            o
        }
    };
    let angle_rad = tip_angle_deg.to_radians();
    for i in 0..pts.len() {
        let (t, b) = core_math::voxel_sculpt::brush_tip_basis(pts[i].normal);
        let base = match orient {
            TipOrientation::Fixed => 0.0,
            TipOrientation::FollowStroke => {
                let sd = if i + 1 < pts.len() {
                    pts[i + 1].position - pts[i].position
                } else if i > 0 {
                    pts[i].position - pts[i - 1].position
                } else if let Some(prev) = stroke_prev_for_tip {
                    pts[i].position - prev
                } else {
                    glam::Vec3::ZERO
                };
                if sd.length_squared() < 1e-12 {
                    0.0
                } else {
                    let sx = sd.dot(t);
                    let sy = sd.dot(b);
                    if sx * sx + sy * sy > 1e-12 { sy.atan2(sx) } else { 0.0 }
                }
            }
            TipOrientation::View => {
                // Camera up projected into stamp plane → tip "up".
                let up = cam_up.normalize_or_zero();
                let up_p = (up - pts[i].normal * up.dot(pts[i].normal)).normalize_or_zero();
                if up_p.length_squared() < 1e-12 {
                    0.0
                } else {
                    let sx = up_p.dot(t);
                    let sy = up_p.dot(b);
                    sy.atan2(sx)
                }
            }
            TipOrientation::Random => {
                // Per-dab full spin from the stroke's absolute stream, so a new frame
                // cannot restart the orientation sequence at local index zero.
                random_tip_rotation(
                    pts[i].position,
                    stroke_seed,
                    dab_start.wrapping_add(i as u32),
                )
            }
        };
        pts[i].tip_rotation = base + angle_rad;
    }
}

/// Select the current sculpt surface without letting a stale CPU field override the live atlas.
/// The g-buffer pick is asynchronous, so reject a previous-cursor point that is too far sideways
/// from the current ray before comparing depths.
pub(crate) fn choose_voxel_surface_hit(
    cpu: Option<(glam::Vec3, glam::Vec3)>,
    gpu: Option<(glam::Vec3, glam::Vec3)>,
    cpu_grid_stale: bool,
    ro: glam::Vec3,
    rd: glam::Vec3,
    max_gpu_lateral: f32,
) -> Option<(glam::Vec3, glam::Vec3)> {
    let gpu = gpu.filter(|(h, _)| {
        let t = (*h - ro).dot(rd);
        t > 1e-5 && (*h - (ro + rd * t)).length() <= max_gpu_lateral
    });
    if cpu_grid_stale {
        return gpu;
    }
    match (cpu, gpu) {
        (Some(c), Some(g)) => {
            let tc = (c.0 - ro).dot(rd);
            let tg = (g.0 - ro).dot(rd);
            if tc > 1e-5 && (tg <= 1e-5 || tc <= tg) { Some(c) } else { Some(g) }
        }
        (c, g) => c.or(g),
    }
}

#[derive(Clone, Copy)]
pub(super) enum VoxelBrushCollectMode {
    /// Advance callbacks only; live input must never wait for the GPU.
    PollOnly,
    /// A lifecycle barrier (pen-up/resample) that must land work before state is committed.
    Drain,
}

#[inline]
pub(super) fn voxel_brush_collect_should_wait(
    mode: VoxelBrushCollectMode,
    state: crate::voxel_gpu_mesh::VoxelBrushMapState,
) -> bool {
    matches!(state, crate::voxel_gpu_mesh::VoxelBrushMapState::Pending)
        && matches!(mode, VoxelBrushCollectMode::Drain)
}

#[inline]
pub(super) fn clear_voxel_stroke_path_latch(latch: &mut Option<bool>) {
    *latch = None;
}

/// Worst-case number of symmetry instances generated by one primary dab. The live budget is in
/// final GPU/CPU stamps, so reserve this multiplier before expanding instead of letting XYZ
/// symmetry silently turn one budgeted batch into eight times the work.
#[inline]
pub(super) fn voxel_symmetry_instance_count(symmetry: [bool; 3]) -> usize {
    1usize << symmetry.into_iter().filter(|enabled| *enabled).count()
}

#[inline]
pub(super) fn voxel_primary_stamp_budget(max_expanded_stamps: usize, symmetry: [bool; 3]) -> usize {
    (max_expanded_stamps / voxel_symmetry_instance_count(symmetry)).max(1)
}

/// Expand primary dabs in the historic axis order while avoiding duplicate deposits on a symmetry
/// plane. A point within half a voxel of a plane already represents both sides of that plane.
pub(crate) fn expand_voxel_symmetry(
    samples: &mut Vec<core_math::sculpt_layer::StrokeSample>,
    symmetry: [bool; 3],
    plane_epsilon: f32,
) {
    let plane_epsilon = plane_epsilon.max(0.0);
    for axis in 0..3 {
        if !symmetry[axis] { continue; }
        let source_len = samples.len();
        for i in 0..source_len {
            if samples[i].position[axis].abs() <= plane_epsilon { continue; }
            let mut mirrored = samples[i].clone();
            mirrored.position[axis] = -mirrored.position[axis];
            mirrored.normal[axis] = -mirrored.normal[axis];
            samples.push(mirrored);
        }
    }
}

/// Atlas brush scratch is allocated twice (live density + frozen neighbourhood snapshot). Keep each
/// transient box to at most 16 MiB per buffer even when consecutive dabs or symmetry copies are far
/// apart. Persistent bricks are still materialised for the exact stamp AABBs, independently of this
/// batching policy.
pub(super) const VOXEL_ATLAS_MAX_SCRATCH_VOXELS: u64 = 4 * 1024 * 1024;

#[inline]
pub(super) fn voxel_aabb_union(a: [u32; 6], b: [u32; 6]) -> [u32; 6] {
    [
        a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2]),
        a[3].max(b[3]), a[4].max(b[4]), a[5].max(b[5]),
    ]
}

#[inline]
pub(super) fn voxel_aabb_volume(aabb: [u32; 6]) -> u64 {
    if aabb[0] > aabb[3] || aabb[1] > aabb[4] || aabb[2] > aabb[5] { return 0; }
    let dx = (aabb[3] - aabb[0] + 1) as u64;
    let dy = (aabb[4] - aabb[1] + 1) as u64;
    let dz = (aabb[5] - aabb[2] + 1) as u64;
    dx.saturating_mul(dy).saturating_mul(dz)
}

/// Greedy, order-preserving atlas work planner. A far symmetry copy starts a new batch instead of
/// expanding the transient scratch across the empty space between the two stamps.
pub(super) fn voxel_atlas_region_batches(
    regions: &[[u32; 6]],
    max_scratch_voxels: u64,
) -> Vec<(std::ops::Range<usize>, [u32; 6])> {
    if regions.is_empty() { return Vec::new(); }
    let limit = max_scratch_voxels.max(1);
    let mut batches = Vec::new();
    let mut start = 0usize;
    let mut union = regions[0];
    for i in 1..regions.len() {
        let candidate = voxel_aabb_union(union, regions[i]);
        if voxel_aabb_volume(candidate) > limit {
            batches.push((start..i, union));
            start = i;
            union = regions[i];
        } else {
            union = candidate;
        }
    }
    batches.push((start..regions.len(), union));
    batches
}

pub(super) fn voxel_regions_for_bricks(bricks: &[u32], brick_res: u32, res: u32) -> Vec<[u32; 6]> {
    if brick_res == 0 || res == 0 { return Vec::new(); }
    let brick = core_math::voxel_sculpt::VOXEL_BRICK_SIZE;
    let plane = brick_res.saturating_mul(brick_res);
    bricks.iter().filter_map(|&bi| {
        if bi >= plane.saturating_mul(brick_res) { return None; }
        let bx = bi % brick_res;
        let by = (bi / brick_res) % brick_res;
        let bz = bi / plane;
        let lo = [bx * brick, by * brick, bz * brick];
        Some([
            lo[0], lo[1], lo[2],
            (lo[0] + brick - 1).min(res - 1),
            (lo[1] + brick - 1).min(res - 1),
            (lo[2] + brick - 1).min(res - 1),
        ])
    }).collect()
}

/// Put an undispatched primary batch back in front of any later budget overflow. `expanded` may also
/// contain symmetry copies, but only its original prefix belongs in `deferred_stamps`: symmetry is
/// regenerated when the primary dabs are eventually dispatched.
pub(super) fn requeue_primary_dabs_front(
    deferred: &mut Vec<core_math::sculpt_layer::StrokeSample>,
    expanded: &[core_math::sculpt_layer::StrokeSample],
    primary_count: usize,
) {
    let primary_count = primary_count.min(expanded.len());
    if primary_count == 0 { return; }
    let mut retry = expanded[..primary_count].to_vec();
    retry.append(deferred);
    *deferred = retry;
}

/// A GPU mesh flush only owns the render gate when it changed visible mesh work or must be
/// polled again. In particular, merely visiting the flush while idle must not keep repainting.
#[inline]
pub(super) fn voxel_gpu_flush_needs_render(
    consumed_or_dispatched: usize,
    requeued: bool,
    poll_pending: bool,
) -> bool {
    consumed_or_dispatched > 0 || requeued || poll_pending
}

/// Decode an IEEE-754 half (f16) bit pattern to f32 — for reading the `gbuf_normal` (Rgba16Float)
/// cursor pixel in the Phase C depth-raycast. Dependency-free; handles normals + finite world_dist
/// (the subnormal/inf branches are exercised rarely but kept correct).
#[inline]
pub(super) fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign << 31 // ±0
        } else {
            // subnormal → normalize into f32
            let mut e = -1i32;
            let mut m = mant;
            while (m & 0x400) == 0 { m <<= 1; e -= 1; }
            m &= 0x3ff;
            (sign << 31) | (((e + 127 - 15) as u32) << 23) | (m << 13)
        }
    } else if exp == 0x1f {
        (sign << 31) | (0xff << 23) | (mant << 13) // inf / NaN
    } else {
        (sign << 31) | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

/// Resolve tablet pressure into the two independent voxel-brush routes.
///
/// Strength remains a per-dab factor so interpolation and Random Strength can compose with it
/// exactly once at the CPU/GPU stamp producers. Radius is resolved once before stamp spacing/AABBs.
#[inline]
pub(super) fn voxel_pressure_factors(
    enabled: bool,
    to_strength: bool,
    to_radius: bool,
    pressure: f32,
    radius_amount: f32,
) -> (f32, f32) {
    if !enabled {
        return (1.0, 1.0);
    }
    let pressure = pressure.clamp(0.0, 1.0);
    let strength_factor = if to_strength { pressure } else { 1.0 };
    let radius_factor = if to_radius {
        let amount = radius_amount.clamp(0.0, 1.0);
        (1.0 - amount) + amount * pressure
    } else {
        1.0
    };
    (strength_factor, radius_factor)
}

pub(super) type GpuVoxelStamp = (
    u32,
    glam::Vec3,
    glam::Vec3,
    f32,
    f32,
    f32,
    [u32; 6],
    f32,
);


#[cfg(test)]
mod chunk_region_tests {
    use super::{
        apply_fresh_dab_dynamics_and_prepend_deferred, brush_dynamics_seed_at,
        choose_voxel_surface_hit, clear_voxel_stroke_path_latch, random_tip_rotation,
        expand_voxel_symmetry, voxel_atlas_region_batches, voxel_bricks_for_regions,
        voxel_primary_stamp_budget, voxel_regions_for_bricks,
        requeue_primary_dabs_front, voxel_chunk_dim, voxel_chunks_for_region,
        voxel_brush_collect_should_wait, voxel_pressure_factors, VoxelBrushCollectMode,
    };
    use crate::voxel_gpu_mesh::VoxelBrushMapState;
    use core_math::sculpt_layer::{apply_brush_dynamics, BrushDynamics, StrokeSample};

    // Regression guard for the chunk-seam tearing bug: a chunk's Surface-Nets mesh depends on a ±1
    // cell halo, so an edit at EITHER boundary must mark the neighbour chunk or its seam tears.
    #[test]
    pub(super) fn edit_marks_neighbour_chunk_on_both_boundaries() {
        let res = 65; // cells = 64 = 4 chunks of VOXEL_CHUNK(16) per axis
        let cd = voxel_chunk_dim(res);
        assert_eq!(cd, 4);
        // Editing the LAST cell of chunk 0 (cell 15) must include chunk 1 (reads 15 as its low halo).
        // This is the exact case the old code missed (high side wasn't expanded).
        let r = voxel_chunks_for_region([15, 0, 0, 15, 0, 0], cd, res);
        assert!(r[3] >= 1, "editing chunk-0's last cell must mark chunk 1 (got {r:?})");
        // Editing the FIRST cell of chunk 1 (cell 16) must include chunk 0 (its +1 corner is read there).
        let r = voxel_chunks_for_region([16, 0, 0, 16, 0, 0], cd, res);
        assert!(r[0] == 0 && r[3] >= 1, "editing chunk-1's first cell must mark chunk 0 (got {r:?})");
    }

    #[test]
    pub(super) fn region_stays_in_bounds_at_grid_max() {
        let res = 65;
        let cd = voxel_chunk_dim(res);
        let last = res - 2; // last cell index
        let r = voxel_chunks_for_region([last, last, last, last, last, last], cd, res);
        for &c in &r { assert!(c < cd, "chunk coord {c} out of range (chunk_dim {cd})"); }
    }

    #[test]
    pub(super) fn diagonal_stamp_regions_do_not_allocate_union_corners() {
        // Two disjoint 16^3 stamp boxes at opposite corners of a 64^3 atlas touch exactly two bricks.
        // The old union-box materialization allocated all 4^3 = 64 bricks between them.
        let bricks = voxel_bricks_for_regions(
            [[0, 0, 0, 15, 15, 15], [48, 48, 48, 63, 63, 63]],
            4,
        );
        assert_eq!(bricks, vec![0, 63]);
    }

    #[test]
    pub(super) fn distant_atlas_stamps_are_split_before_union_scratch_explodes() {
        let regions = [
            [0, 0, 0, 31, 31, 31],
            [32, 0, 0, 63, 31, 31],
            [992, 992, 992, 1023, 1023, 1023],
        ];
        let batches = voxel_atlas_region_batches(&regions, 4 * 1024 * 1024);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].0, 0..2);
        assert_eq!(batches[0].1, [0, 0, 0, 63, 31, 31]);
        assert_eq!(batches[1].0, 2..3);
        assert_eq!(batches[1].1, regions[2]);
    }

    #[test]
    pub(super) fn atlas_undo_regions_follow_captured_bricks_not_their_global_union() {
        let regions = voxel_regions_for_bricks(&[0, 63], 4, 64);
        assert_eq!(regions, vec![
            [0, 0, 0, 15, 15, 15],
            [48, 48, 48, 63, 63, 63],
        ]);
    }

    #[test]
    pub(super) fn pressure_strength_is_routed_once_and_composes_with_random_strength() {
        let (pressure_factor, radius_factor) =
            voxel_pressure_factors(true, true, false, 0.5, 0.6);
        assert!((pressure_factor - 0.5).abs() < f32::EPSILON);
        assert!((radius_factor - 1.0).abs() < f32::EPSILON);

        let mut routed = [StrokeSample::new(glam::Vec3::ZERO, glam::Vec3::Y, pressure_factor)];
        let mut full_force = [StrokeSample::new(glam::Vec3::ZERO, glam::Vec3::Y, 1.0)];
        let dynamics = BrushDynamics { random_strength: 0.75, ..Default::default() };
        apply_brush_dynamics(&mut routed, 1.0, dynamics, 0x5EED);
        apply_brush_dynamics(&mut full_force, 1.0, dynamics, 0x5EED);

        let base_strength = 0.8;
        let routed_stamp_strength = base_strength * routed[0].pressure;
        let expected_once = base_strength * full_force[0].pressure * pressure_factor;
        assert!((routed_stamp_strength - expected_once).abs() < 1e-6);
    }

    #[test]
    pub(super) fn radius_only_pressure_does_not_change_strength() {
        let (pressure_factor, radius_factor) =
            voxel_pressure_factors(true, false, true, 0.25, 0.60);
        assert!((pressure_factor - 1.0).abs() < f32::EPSILON);
        assert!((radius_factor - 0.55).abs() < 1e-6);

        let disabled = voxel_pressure_factors(false, true, true, 0.0, 1.0);
        assert_eq!(disabled, (1.0, 1.0));
    }

    #[test]
    pub(super) fn deferred_dabs_keep_their_resolved_shape_when_budget_spans_frames() {
        let mut resolved = StrokeSample::new(
            glam::Vec3::new(4.0, 5.0, 6.0),
            glam::Vec3::new(0.0, 0.0, 1.0),
            0.42,
        );
        resolved.radius_scale = 0.37;
        resolved.tip_rotation = 1.25;
        let expected = resolved.clone();
        let mut deferred = vec![resolved];
        let mut fresh = vec![StrokeSample::new(glam::Vec3::ZERO, glam::Vec3::Y, 1.0)];
        let dynamics = BrushDynamics {
            jitter: 1.0,
            scatter: 1.0,
            random_rotation: 1.0,
            random_strength: 1.0,
            random_radius: 1.0,
            stroke_noise: 1.0,
        };

        apply_fresh_dab_dynamics_and_prepend_deferred(
            &mut fresh,
            &mut deferred,
            2.0,
            dynamics,
            0xBACC_10C,
        );

        assert!(deferred.is_empty());
        assert_eq!(fresh.len(), 2);
        assert_eq!(fresh[0].position, expected.position);
        assert_eq!(fresh[0].normal, expected.normal);
        assert_eq!(fresh[0].pressure.to_bits(), expected.pressure.to_bits());
        assert_eq!(fresh[0].radius_scale.to_bits(), expected.radius_scale.to_bits());
        assert_eq!(fresh[0].tip_rotation.to_bits(), expected.tip_rotation.to_bits());
    }

    #[test]
    pub(super) fn release_tail_keeps_deferred_fifo_before_fresh_samples() {
        let sample = |x: f32| {
            StrokeSample::new(glam::Vec3::new(x, 0.0, 0.0), glam::Vec3::Y, 1.0)
        };
        let mut deferred = vec![sample(1.0), sample(2.0)];
        let mut fresh = vec![sample(3.0)];
        apply_fresh_dab_dynamics_and_prepend_deferred(
            &mut fresh,
            &mut deferred,
            1.0,
            BrushDynamics::default(),
            0x5EED,
        );
        let xs: Vec<f32> = fresh.iter().map(|dab| dab.position.x).collect();
        assert_eq!(xs, vec![1.0, 2.0, 3.0]);
        assert!(deferred.is_empty());
    }

    #[test]
    pub(super) fn brush_dynamics_are_independent_of_frame_partition() {
        let sample = |x: f32| {
            StrokeSample::new(
                glam::Vec3::new(x, x * 0.25, 1.0 - x * 0.1),
                glam::Vec3::new(0.2, 1.0, 0.3).normalize(),
                0.7,
            )
        };
        let source = vec![sample(0.0), sample(0.3), sample(0.8), sample(1.4)];
        let dynamics = BrushDynamics {
            jitter: 0.8,
            scatter: 0.4,
            random_rotation: 0.6,
            random_strength: 0.7,
            random_radius: 0.5,
            stroke_noise: 0.3,
        };
        let stroke_seed = 0xA2A2_5EED;

        let mut one_frame = source.clone();
        apply_brush_dynamics(&mut one_frame, 2.5, dynamics, stroke_seed);

        let mut split = source.clone();
        apply_brush_dynamics(&mut split[..2], 2.5, dynamics, stroke_seed);
        apply_brush_dynamics(
            &mut split[2..],
            2.5,
            dynamics,
            brush_dynamics_seed_at(stroke_seed, 2),
        );

        for (whole, partitioned) in one_frame.iter().zip(&split) {
            assert_eq!(whole.position.x.to_bits(), partitioned.position.x.to_bits());
            assert_eq!(whole.position.y.to_bits(), partitioned.position.y.to_bits());
            assert_eq!(whole.position.z.to_bits(), partitioned.position.z.to_bits());
            assert_eq!(whole.pressure.to_bits(), partitioned.pressure.to_bits());
            assert_eq!(whole.radius_scale.to_bits(), partitioned.radius_scale.to_bits());
            assert_eq!(whole.tip_rotation.to_bits(), partitioned.tip_rotation.to_bits());
        }
    }

    #[test]
    pub(super) fn random_tip_orientation_is_independent_of_frame_partition() {
        let positions = [
            glam::Vec3::new(0.1, 0.2, 0.3),
            glam::Vec3::new(0.4, 0.5, 0.6),
            glam::Vec3::new(0.7, 0.8, 0.9),
            glam::Vec3::new(1.0, 1.1, 1.2),
        ];
        let seed = 0x71F0_5EED;
        let whole: Vec<u32> = positions.iter().enumerate()
            .map(|(i, &p)| random_tip_rotation(p, seed, i as u32).to_bits())
            .collect();
        let split: Vec<u32> = positions[..2].iter().enumerate()
            .map(|(i, &p)| random_tip_rotation(p, seed, i as u32).to_bits())
            .chain(positions[2..].iter().enumerate()
                .map(|(i, &p)| random_tip_rotation(p, seed, 2 + i as u32).to_bits()))
            .collect();
        assert_eq!(whole, split);
    }

    #[test]
    pub(super) fn stale_cpu_surface_cannot_override_live_atlas_hit_or_normal() {
        let ro = glam::Vec3::ZERO;
        let rd = glam::Vec3::Z;
        let cpu = Some((glam::Vec3::new(0.0, 0.0, 1.0), glam::Vec3::X));
        let gpu = Some((glam::Vec3::new(0.0, 0.0, 1.2), glam::Vec3::Y));

        let live = choose_voxel_surface_hit(cpu, gpu, true, ro, rd, 0.25).unwrap();
        assert_eq!(live.0, glam::Vec3::new(0.0, 0.0, 1.2));
        assert_eq!(live.1, glam::Vec3::Y);

        // A previous-frame g-buffer point well off the current cursor ray is not a valid hit.
        let lagged_gpu = Some((glam::Vec3::new(1.0, 0.0, 1.2), glam::Vec3::Y));
        assert!(choose_voxel_surface_hit(cpu, lagged_gpu, true, ro, rd, 0.25).is_none());
    }

    #[test]
    pub(super) fn gpu_backpressure_requeues_primary_fifo_without_symmetry_copies() {
        let sample = |x: f32| StrokeSample::new(glam::Vec3::new(x, 0.0, 0.0), glam::Vec3::Y, 1.0);
        // Expanded layout keeps the two primary dabs first, followed by their symmetry copies.
        let expanded = vec![sample(1.0), sample(2.0), sample(-1.0), sample(-2.0)];
        let mut later_budget_overflow = vec![sample(3.0), sample(4.0)];

        requeue_primary_dabs_front(&mut later_budget_overflow, &expanded, 2);

        let xs: Vec<f32> = later_budget_overflow.iter().map(|dab| dab.position.x).collect();
        assert_eq!(xs, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    pub(super) fn brush_collect_waits_only_at_explicit_lifecycle_barriers() {
        assert!(!voxel_brush_collect_should_wait(
            VoxelBrushCollectMode::PollOnly,
            VoxelBrushMapState::Pending,
        ));
        assert!(!voxel_brush_collect_should_wait(
            VoxelBrushCollectMode::Drain,
            VoxelBrushMapState::Ready,
        ));
        assert!(!voxel_brush_collect_should_wait(
            VoxelBrushCollectMode::Drain,
            VoxelBrushMapState::Failed,
        ));
        assert!(voxel_brush_collect_should_wait(
            VoxelBrushCollectMode::Drain,
            VoxelBrushMapState::Pending,
        ));
    }

    #[test]
    pub(super) fn release_clears_both_atlas_and_box_path_latches() {
        let mut atlas = Some(true);
        let mut box_path = Some(false);
        clear_voxel_stroke_path_latch(&mut atlas);
        clear_voxel_stroke_path_latch(&mut box_path);
        assert_eq!(atlas, None);
        assert_eq!(box_path, None);
    }

    #[test]
    pub(super) fn symmetry_respects_final_budget_and_does_not_duplicate_plane_dabs() {
        let symmetry = [true, true, true];
        let primary_budget = voxel_primary_stamp_budget(24, symmetry);
        assert_eq!(primary_budget, 3);

        let sample = |position: glam::Vec3, normal: glam::Vec3| {
            StrokeSample::new(position, normal, 1.0)
        };
        let mut off_plane = vec![sample(glam::Vec3::splat(2.0), glam::Vec3::splat(1.0))];
        expand_voxel_symmetry(&mut off_plane, symmetry, 0.01);
        assert_eq!(off_plane.len(), 8);

        let mut on_origin = vec![sample(glam::Vec3::ZERO, glam::Vec3::Y)];
        expand_voxel_symmetry(&mut on_origin, symmetry, 0.01);
        assert_eq!(on_origin.len(), 1);

        let mut on_x = vec![sample(
            glam::Vec3::new(0.0, 2.0, 3.0),
            glam::Vec3::new(0.0, 1.0, 1.0),
        )];
        expand_voxel_symmetry(&mut on_x, symmetry, 0.01);
        assert_eq!(on_x.len(), 4);
        assert!(on_x.iter().all(|dab| dab.position.x == 0.0));
    }
}

#[cfg(test)]
mod voxel_gpu_flush_render_tests {
    use super::voxel_gpu_flush_needs_render;

    #[test]
    pub(super) fn idle_flush_does_not_render_but_real_work_and_polling_do() {
        assert!(!voxel_gpu_flush_needs_render(0, false, false));
        assert!(voxel_gpu_flush_needs_render(1, false, false));
        assert!(voxel_gpu_flush_needs_render(0, true, false));
        assert!(voxel_gpu_flush_needs_render(0, false, true));
    }
}
