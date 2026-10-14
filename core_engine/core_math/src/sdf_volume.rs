// core_engine/core_math/src/sdf_volume.rs
use glam::Vec3;
use crate::primitives::{SdfPrimitive, BooleanOp};

// If you add rayon = "1.10" to Cargo.toml, uncomment these for parallel bake/brush:
// use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirtyRegion { pub min: [u32; 3], pub max: [u32; 3] }

// ================================================================
// Brush types & falloff
// ================================================================
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BrushType { Add, Subtract, Smooth, Flatten, Inflate, Pinch, Clay, Crease, Trim }

impl BrushType {
    pub const ALL: [BrushType; 9] = [
        Self::Add, Self::Subtract, Self::Smooth, Self::Flatten,
        Self::Inflate, Self::Pinch, Self::Clay, Self::Crease, Self::Trim,
    ];
    pub fn label(&self) -> &'static str {
        match self {
            Self::Add => "Standard", Self::Subtract => "Subtract", Self::Smooth => "Smooth",
            Self::Flatten => "Flatten", Self::Inflate => "Inflate", Self::Pinch => "Pinch",
            Self::Clay => "Clay", Self::Crease => "Crease", Self::Trim => "Trim",
        }
    }
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Add => "\u{25CF}", Self::Subtract => "\u{25CB}", Self::Smooth => "\u{25CB}",
            Self::Flatten => "\u{25AC}", Self::Inflate => "\u{25C9}", Self::Pinch => "\u{25C7}",
            Self::Clay => "\u{25A4}", Self::Crease => "\u{2228}", Self::Trim => "\u{229F}",
        }
    }
    /// Returns the inverted brush type when Ctrl is held.
    pub fn inverted(&self) -> Self {
        match self {
            Self::Add => Self::Subtract,
            Self::Subtract => Self::Add,
            Self::Clay => Self::Trim,
            Self::Trim => Self::Clay,
            other => *other,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FalloffCurve { Linear, Smooth, Sharp }

impl FalloffCurve {
    /// t in 0..1 (0 = center, 1 = edge). Returns weight in 0..1.
    pub fn evaluate(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FalloffCurve::Linear => 1.0 - t,
            FalloffCurve::Smooth => {
                let s = 1.0 - t;
                s * s * (3.0 - 2.0 * s)
            }
            FalloffCurve::Sharp => (1.0 - t * t * t).max(0.0),
        }
    }
    pub fn label(&self) -> &str {
        match self { FalloffCurve::Linear=>"Linear", FalloffCurve::Smooth=>"Smooth", FalloffCurve::Sharp=>"Sharp" }
    }
}

#[derive(Clone, Copy)]
pub struct BrushSettings {
    pub brush_type: BrushType,
    pub size: f32,
    pub strength: f32,
    pub falloff: FalloffCurve,
    pub symmetry_x: bool,
}
impl Default for BrushSettings {
    fn default() -> Self {
        Self { brush_type: BrushType::Add, size: 0.15, strength: 0.5, falloff: FalloffCurve::Smooth, symmetry_x: false }
    }
}

// ================================================================
// SdfVolume
// ================================================================
pub struct SdfVolume {
    pub data: Vec<f32>,
    pub resolution: [u32; 3],
    pub grid_bounds: f32,
    pub dirty_region: Option<DirtyRegion>,
}

impl SdfVolume {
    pub fn new(res: u32, bounds: f32) -> Self {
        let mut v = Self::new_empty(res, bounds);
        let brush = BrushSettings { brush_type: BrushType::Add, size: 1.0, strength: 1.0, falloff: FalloffCurve::Smooth, symmetry_x: false };
        v.apply_brush(Vec3::ZERO, Vec3::Y, &brush);
        v
    }
    pub fn new_empty(res: u32, bounds: f32) -> Self {
        Self {
            data: vec![100.0; (res * res * res) as usize],
            resolution: [res, res, res],
            grid_bounds: bounds,
            dirty_region: Some(DirtyRegion { min: [0, 0, 0], max: [res - 1, res - 1, res - 1] }),
        }
    }
    pub fn mark_dirty(&mut self, min: [u32; 3], max: [u32; 3]) {
        self.dirty_region = Some(match self.dirty_region {
            None => DirtyRegion { min, max },
            Some(ex) => DirtyRegion {
                min: [ex.min[0].min(min[0]), ex.min[1].min(min[1]), ex.min[2].min(min[2])],
                max: [ex.max[0].max(max[0]), ex.max[1].max(max[1]), ex.max[2].max(max[2])],
            },
        });
    }
    pub fn take_dirty_region(&mut self) -> Option<DirtyRegion> { self.dirty_region.take() }

    /// World-space position of voxel cell (x,y,z) — inverse of the get_distance mapping.
    #[inline]
    fn cell_world_pos(&self, x: u32, y: u32, z: u32) -> Vec3 {
        let b = self.grid_bounds;
        let res_f = (self.resolution[0] - 1).max(1) as f32;
        Vec3::new(
            (x as f32 / res_f) * 2.0 * b - b,
            (y as f32 / res_f) * 2.0 * b - b,
            (z as f32 / res_f) * 2.0 * b - b,
        )
    }

    /// Reset the whole grid to a solid SPHERE — a "basic voxel primitive": a starting
    /// lump of material to sculpt. We store a scalar field (the isosurface renderer and
    /// the displacement brushes both read it); this is voxel-grid sculpting, not the
    /// analytical SDF-prim system. Brushes then build/carve this lump.
    pub fn fill_sphere(&mut self, center: Vec3, radius: f32) {
        let res = self.resolution;
        for z in 0..res[2] {
            for y in 0..res[1] {
                for x in 0..res[0] {
                    let p = self.cell_world_pos(x, y, z);
                    let idx = (x + y * res[0] + z * res[0] * res[0]) as usize;
                    self.data[idx] = (p - center).length() - radius;
                }
            }
        }
        self.mark_dirty([0, 0, 0], [res[0] - 1, res[1] - 1, res[2] - 1]);
    }

    /// Reset the whole grid to a solid BOX — basic voxel primitive (see `fill_sphere`).
    pub fn fill_box(&mut self, center: Vec3, half_extents: Vec3) {
        let res = self.resolution;
        for z in 0..res[2] {
            for y in 0..res[1] {
                for x in 0..res[0] {
                    let p = self.cell_world_pos(x, y, z) - center;
                    let q = p.abs() - half_extents;
                    let d = q.max(Vec3::ZERO).length() + q.max_element().min(0.0);
                    let idx = (x + y * res[0] + z * res[0] * res[0]) as usize;
                    self.data[idx] = d;
                }
            }
        }
        self.mark_dirty([0, 0, 0], [res[0] - 1, res[1] - 1, res[2] - 1]);
    }

    /// Clear the grid back to empty (no material).
    pub fn clear_to_empty(&mut self) {
        let res = self.resolution;
        self.data.fill(100.0);
        self.mark_dirty([0, 0, 0], [res[0] - 1, res[1] - 1, res[2] - 1]);
    }

    fn sample_voxel(&self, x: i32, y: i32, z: i32) -> f32 {
        let r = self.resolution[0] as i32;
        self.data[(x.clamp(0, r - 1) + y.clamp(0, r - 1) * r + z.clamp(0, r - 1) * r * r) as usize]
    }
    /// Sample from a compact z-slab snapshot (z0..=z1 of the full volume).
    /// x/y clamp to [0, rx-1]/[0, ry-1]; z clamps to [z0, z1] then offsets into the slab.
    #[inline]
    #[allow(dead_code)] // FUTURE: sculpt volume operations
    fn sample_snap(snap: &[f32], rx: u32, ry: u32, x: i32, y: i32, z: i32, z0: i32, z1: i32) -> f32 {
        let rxr = rx as i32; let ryr = ry as i32;
        let xc = x.clamp(0, rxr - 1);
        let yc = y.clamp(0, ryr - 1);
        let zc = z.clamp(z0, z1) - z0;
        snap[(xc + yc * rxr + zc * rxr * ryr) as usize]
    }

    pub fn get_distance(&self, p: Vec3) -> f32 {
        let b = self.grid_bounds;
        let q = p.abs() - Vec3::splat(b);
        let d_box = q.max(Vec3::ZERO).length() + q.max_element().min(0.0);
        if d_box > 0.0 { return d_box + 0.1; }
        let res_f = (self.resolution[0] - 1) as f32;
        let gp = ((p + Vec3::splat(b)) / (2.0 * b)) * res_f;
        let bx = gp.x.floor() as i32; let by = gp.y.floor() as i32; let bz = gp.z.floor() as i32;
        let fx = gp.x - gp.x.floor(); let fy = gp.y - gp.y.floor(); let fz = gp.z - gp.z.floor();
        let x00 = self.sample_voxel(bx, by, bz) * (1.0 - fx) + self.sample_voxel(bx + 1, by, bz) * fx;
        let x10 = self.sample_voxel(bx, by + 1, bz) * (1.0 - fx) + self.sample_voxel(bx + 1, by + 1, bz) * fx;
        let x01 = self.sample_voxel(bx, by, bz + 1) * (1.0 - fx) + self.sample_voxel(bx + 1, by, bz + 1) * fx;
        let x11 = self.sample_voxel(bx, by + 1, bz + 1) * (1.0 - fx) + self.sample_voxel(bx + 1, by + 1, bz + 1) * fx;
        let y0 = x00 * (1.0 - fy) + x10 * fy;
        let y1 = x01 * (1.0 - fy) + x11 * fy;
        y0 * (1.0 - fz) + y1 * fz
    }

    /// World-space size of one voxel cell. Single canonical definition (VS11) —
    /// brush_aabb / bake_from_primitives / SculptBackend all reference this formula.
    pub fn voxel_size(&self) -> f32 {
        (self.grid_bounds * 2.0) / ((self.resolution[0].max(2) - 1) as f32)
    }

    pub fn normal_at(&self, p: Vec3) -> Vec3 {
        // VS11: finite-difference eps = one full voxel, so the central difference
        // straddles a cell boundary and reads the reconstructed gradient instead of
        // sampling inside one trilinear cell (was a fixed 0.01 → noisy normals,
        // skewing Flatten/Clay/Trim/Pinch reference planes).
        let e = self.voxel_size();
        Vec3::new(
            self.get_distance(p + Vec3::X * e) - self.get_distance(p - Vec3::X * e),
            self.get_distance(p + Vec3::Y * e) - self.get_distance(p - Vec3::Y * e),
            self.get_distance(p + Vec3::Z * e) - self.get_distance(p - Vec3::Z * e),
        ).normalize_or_zero()
    }

    pub fn raycast(&self, ro: Vec3, rd: Vec3) -> Option<Vec3> {
        // VS11: thresholds derived from voxel_size instead of magic constants.
        // Full sphere-trace step (field is Lipschitz) with a vs*0.25 floor to avoid
        // stalling in the near-zero band; hit tolerance = half a voxel; distance cap =
        // the sampled box diameter (past which get_distance returns the analytic bound).
        let vs = self.voxel_size();
        let hit_eps = vs * 0.5;
        let max_t = 2.0 * self.grid_bounds + vs;
        let mut t = 0.0;
        for _ in 0..128 {
            let p = ro + rd * t;
            let d = self.get_distance(p);
            if d < hit_eps { return Some(p); }
            t += d.max(vs * 0.25);
            if t > max_t { break; }
        }
        None
    }

    pub fn smin(a: f32, b: f32, k: f32) -> f32 {
        if k <= 1e-6 { return a.min(b); }
        let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
        a * h + b * (1.0 - h) - k * h * (1.0 - h)
    }

    /// Chamfer min — creates a 45° bevel between surfaces
    pub fn chamfer_min(a: f32, b: f32, k: f32) -> f32 {
        a.min(b).min((a - k + b) * std::f32::consts::FRAC_1_SQRT_2)
    }

    // ================================================================
    // Undo / delta helpers
    // ================================================================

    /// Compute the voxel-space AABB for a spherical brush. Mirrors the padding used in
    /// apply_brush_advanced so the snapshot covers exactly the region that will change.
    /// Returns [cx, cy, cz, mx, my, mz] in voxel coords, or None if fully outside.
    pub fn brush_aabb(&self, center: Vec3, radius: f32) -> Option<[u32; 6]> {
        let rx = self.resolution[0]; let ry = self.resolution[1]; let rz = self.resolution[2];
        let vs = (self.grid_bounds * 2.0) / ((rx - 1) as f32);
        let pad = radius + vs * 2.0 + 0.15; // matches apply_brush_advanced
        let bounds = self.grid_bounds;
        let min_f = (center - Vec3::splat(pad) + Vec3::splat(bounds)) / vs;
        let max_f = (center + Vec3::splat(pad) + Vec3::splat(bounds)) / vs;
        let cx = min_f.x.floor().clamp(0.0, (rx - 1) as f32) as u32;
        let cy = min_f.y.floor().clamp(0.0, (ry - 1) as f32) as u32;
        let cz = min_f.z.floor().clamp(0.0, (rz - 1) as f32) as u32;
        let mx = max_f.x.ceil().clamp(0.0, (rx - 1) as f32) as u32;
        let my = max_f.y.ceil().clamp(0.0, (ry - 1) as f32) as u32;
        let mz = max_f.z.ceil().clamp(0.0, (rz - 1) as f32) as u32;
        if cx > mx || cy > my || cz > mz { return None; }
        Some([cx, cy, cz, mx, my, mz])
    }

    /// Snapshot (flat_index, value) for every voxel in the given AABB.
    /// Called BEFORE apply_brush. The caller deduplicates with HashMap::entry().or_insert()
    /// so only the pre-stroke original value is kept across multiple drag frames.
    pub fn snapshot_region(&self, cx: u32, cy: u32, cz: u32, mx: u32, my: u32, mz: u32) -> Vec<(u32, f32)> {
        let rx = self.resolution[0];
        let cap = ((mx - cx + 1) * (my - cy + 1) * (mz - cz + 1)) as usize;
        let mut out = Vec::with_capacity(cap);
        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let idx = x + y * rx + z * rx * rx;
                    out.push((idx, self.data[idx as usize]));
                }
            }
        }
        out
    }

    /// Restore voxels from an (index, original_value) delta slice.
    /// Marks only the affected AABB as dirty — far cheaper than a full-volume upload.
    /// Returns the dirty region so callers can propagate it if needed.
    /// Restore voxels from an (index, value) delta. Returns the INVERSE delta (the
    /// values that were overwritten), so the same call replayed on the inverse
    /// reproduces the prior state — this is what makes voxel undo/redo symmetric (VS4).
    pub fn restore_delta(&mut self, delta: &[(u32, f32)]) -> Vec<(u32, f32)> {
        if delta.is_empty() { return Vec::new(); }
        let res = self.resolution[0];
        let mut min_v = [u32::MAX; 3];
        let mut max_v = [0u32; 3];
        let mut inverse = Vec::with_capacity(delta.len());
        for &(idx, val) in delta {
            inverse.push((idx, self.data[idx as usize])); // capture current value before overwrite
            self.data[idx as usize] = val;
            let x = idx % res;
            let y = (idx / res) % res;
            let z = idx / (res * res);
            if x < min_v[0] { min_v[0] = x; } if x > max_v[0] { max_v[0] = x; }
            if y < min_v[1] { min_v[1] = y; } if y > max_v[1] { max_v[1] = y; }
            if z < min_v[2] { min_v[2] = z; } if z > max_v[2] { max_v[2] = z; }
        }
        self.mark_dirty(min_v, max_v);
        inverse
    }

    pub fn bake_from_primitives(&mut self, primitives: &[SdfPrimitive]) {
        let rx = self.resolution[0]; let ry = self.resolution[1]; let rz = self.resolution[2];
        let vs = (self.grid_bounds * 2.0) / ((rx - 1) as f32);
        let visible: Vec<&SdfPrimitive> = primitives.iter().filter(|p| p.visible).collect();
        if visible.is_empty() {
            self.data.fill(100.0);
            self.mark_dirty([0, 0, 0], [rx - 1, ry - 1, rz - 1]);
            return;
        }
        let bounds = self.grid_bounds;
        let data = &mut self.data;
        // Parallel bake by Z-slice
        let slice_size = (rx * ry) as usize;
        data.chunks_mut(slice_size).enumerate().for_each(|(z, slice)| {
            let z = z as u32;
            for y in 0..ry {
                for x in 0..rx {
                    let p = Vec3::new(
                        x as f32 * vs - bounds,
                        y as f32 * vs - bounds,
                        z as f32 * vs - bounds,
                    );
                    let idx = (x + y * rx) as usize;
                    let mut dist = visible[0].world_distance(p);
                    for prim in visible.iter().skip(1) {
                        let d = prim.world_distance(p);
                        let fm = crate::sdf_math::blend_k_multiplier(prim, p);
                        dist = match &prim.operation {
                            BooleanOp::Union    { k } => { let kl = *k * fm; if kl > 0.0 { Self::smin(dist, d, kl) } else { dist.min(d) } }
                            BooleanOp::Subtract { k } => { let kl = *k * fm; if kl > 0.0 { -Self::smin(-dist, d, kl) } else { dist.max(-d) } }
                            BooleanOp::Intersect          => dist.max(d),
                            BooleanOp::SmoothIntersect { k } => { let kl = *k * fm; if kl > 0.0 { -Self::smin(-dist, -d, kl) } else { dist.max(d) } }
                            BooleanOp::ChamferUnion    { k } => { let kl = *k * fm; if kl > 0.0 { Self::chamfer_min(dist, d, kl) } else { dist.min(d) } }
                            BooleanOp::ChamferSubtract { k } => { let kl = *k * fm; if kl > 0.0 { -Self::chamfer_min(-dist, d, kl) } else { dist.max(-d) } }
                            // Shared seam combine (sdf_math) — width falloff-modulated like k.
                            BooleanOp::Seam { width, depth, profile, raised } =>
                                crate::sdf_math::seam_op(dist, d, *width * fm, *depth, *profile, *raised),
                            // Shared extrude combine (sdf_math) — width falloff-modulated like k.
                            BooleanOp::Extrude { strength, width } =>
                                crate::sdf_math::extrude_op(dist, d, *width * fm, *strength),
                        };
                    }
                    slice[idx] = dist;
                }
            }
        });
        self.mark_dirty([0, 0, 0], [rx - 1, ry - 1, rz - 1]);
    }

        // Legacy brush (kept for compatibility)
    pub fn apply_brush(&mut self, center: Vec3, _normal: Vec3, brush: &BrushSettings) {
        self.apply_brush_advanced(center, Vec3::Y, brush, Vec3::Z);
    }

    /// Main brush entry point — delegates to unified BrushKernel system.
    /// stroke_dir is the tangent of the current stroke segment (for crease/pinch).
    pub fn apply_brush_advanced(&mut self, base_center: Vec3, surface_normal: Vec3, brush: &BrushSettings, stroke_dir: Vec3) {
        use crate::sculpt_brush::{SculptBrush};
        let sb = SculptBrush {
            brush_type: brush.brush_type,
            radius: brush.size,
            strength: brush.strength,
            falloff: brush.falloff,
            symmetry_x: brush.symmetry_x,
            ..SculptBrush::default()
        };
        sb.apply_stamp(self, base_center, surface_normal, stroke_dir);
    }
}

// ================================================================
// SculptBackend impl for SdfVolume
// ================================================================
use crate::sculpt_brush::SculptBackend;

impl SculptBackend for SdfVolume {
    fn write_value(&mut self, ix: u32, iy: u32, iz: u32, value: f32) {
        let rx = self.resolution[0];
        let ry = self.resolution[1];
        let idx = (ix + iy * rx + iz * rx * ry) as usize;
        if idx < self.data.len() {
            self.data[idx] = value;
        }
    }

    fn read_value(&self, ix: u32, iy: u32, iz: u32) -> f32 {
        let rx = self.resolution[0];
        let ry = self.resolution[1];
        let idx = (ix + iy * rx + iz * rx * ry) as usize;
        if idx < self.data.len() { self.data[idx] } else { 100.0 }
    }

    fn read_neighbors_avg(&self, ix: i32, iy: i32, iz: i32) -> f32 {
        (self.sample_voxel(ix - 1, iy, iz)
            + self.sample_voxel(ix + 1, iy, iz)
            + self.sample_voxel(ix, iy - 1, iz)
            + self.sample_voxel(ix, iy + 1, iz)
            + self.sample_voxel(ix, iy, iz - 1)
            + self.sample_voxel(ix, iy, iz + 1)) / 6.0
    }

    fn world_to_cell(&self, pos: Vec3) -> (i32, i32, i32) {
        let vs = self.voxel_size();
        let b = self.grid_bounds;
        let gp = (pos + Vec3::splat(b)) / vs;
        (gp.x.round() as i32, gp.y.round() as i32, gp.z.round() as i32)
    }

    fn cell_to_world(&self, ix: u32, iy: u32, iz: u32) -> Vec3 {
        let vs = self.voxel_size();
        let b = self.grid_bounds;
        Vec3::new(ix as f32 * vs - b, iy as f32 * vs - b, iz as f32 * vs - b)
    }

    fn in_bounds(&self, ix: i32, iy: i32, iz: i32) -> bool {
        let [rx, ry, rz] = self.resolution;
        ix >= 0 && iy >= 0 && iz >= 0
            && (ix as u32) < rx && (iy as u32) < ry && (iz as u32) < rz
    }

    fn resolution(&self) -> [u32; 3] { self.resolution }

    fn voxel_size(&self) -> f32 {
        (self.grid_bounds * 2.0) / ((self.resolution[0] - 1) as f32)
    }

    fn grid_bounds(&self) -> f32 { self.grid_bounds }

    fn mark_dirty(&mut self, min: [u32; 3], max: [u32; 3]) {
        SdfVolume::mark_dirty(self, min, max);
    }
}