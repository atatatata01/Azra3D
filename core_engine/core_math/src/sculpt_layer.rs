// core_engine/core_math/src/sculpt_layer.rs
// Sculpt layer system: accumulative brush stroke storage via sparse SDF delta grids.

use std::collections::HashMap;
use glam::Vec3;
use serde::{Serialize, Deserialize};

// ============================================================
// Sparse SDF Grid — stores signed distance deltas per voxel cell
// ============================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SparseSDFGrid {
    pub cell_size: f32,
    pub cells: HashMap<(i32, i32, i32), f32>,
}

impl Default for SparseSDFGrid {
    fn default() -> Self { Self::new(0.02) }
}

impl SparseSDFGrid {
    pub fn new(cell_size: f32) -> Self {
        Self { cell_size: cell_size.max(0.001), cells: HashMap::new() }
    }

    /// Convert world position to cell coordinate.
    fn world_to_cell(&self, pos: Vec3) -> (i32, i32, i32) {
        let inv = 1.0 / self.cell_size;
        ((pos.x * inv).floor() as i32,
         (pos.y * inv).floor() as i32,
         (pos.z * inv).floor() as i32)
    }

    /// Accumulate a delta value at a world position into the nearest cell.
    pub fn accumulate(&mut self, world_pos: Vec3, delta: f32) {
        if delta.abs() < 1e-7 { return; }
        let cell = self.world_to_cell(world_pos);
        *self.cells.entry(cell).or_insert(0.0) += delta;
    }

    /// Sample the grid at a world position with trilinear interpolation.
    /// Returns 0.0 for untouched cells.
    pub fn sample(&self, world_pos: Vec3) -> f32 {
        let inv = 1.0 / self.cell_size;
        let fx = world_pos.x * inv - 0.5;
        let fy = world_pos.y * inv - 0.5;
        let fz = world_pos.z * inv - 0.5;
        let ix = fx.floor() as i32;
        let iy = fy.floor() as i32;
        let iz = fz.floor() as i32;
        let tx = fx - ix as f32;
        let ty = fy - iy as f32;
        let tz = fz - iz as f32;

        let c = |x: i32, y: i32, z: i32| -> f32 {
            self.cells.get(&(x, y, z)).copied().unwrap_or(0.0)
        };

        let c000 = c(ix, iy, iz);
        let c100 = c(ix + 1, iy, iz);
        let c010 = c(ix, iy + 1, iz);
        let c110 = c(ix + 1, iy + 1, iz);
        let c001 = c(ix, iy, iz + 1);
        let c101 = c(ix + 1, iy, iz + 1);
        let c011 = c(ix, iy + 1, iz + 1);
        let c111 = c(ix + 1, iy + 1, iz + 1);

        let x00 = c000 + (c100 - c000) * tx;
        let x10 = c010 + (c110 - c010) * tx;
        let x01 = c001 + (c101 - c001) * tx;
        let x11 = c011 + (c111 - c011) * tx;

        let y0 = x00 + (x10 - x00) * ty;
        let y1 = x01 + (x11 - x01) * ty;

        y0 + (y1 - y0) * tz
    }

    /// Clear all data in this grid.
    pub fn clear(&mut self) {
        self.cells.clear();
    }

    /// Number of touched cells.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Compute the bounding box of touched cells (world-space).
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        if self.cells.is_empty() { return None; }
        let mut min_c = (i32::MAX, i32::MAX, i32::MAX);
        let mut max_c = (i32::MIN, i32::MIN, i32::MIN);
        for &(x, y, z) in self.cells.keys() {
            min_c.0 = min_c.0.min(x);
            min_c.1 = min_c.1.min(y);
            min_c.2 = min_c.2.min(z);
            max_c.0 = max_c.0.max(x);
            max_c.1 = max_c.1.max(y);
            max_c.2 = max_c.2.max(z);
        }
        let s = self.cell_size;
        Some((Vec3::new(min_c.0 as f32 * s, min_c.1 as f32 * s, min_c.2 as f32 * s),
              Vec3::new((max_c.0 + 1) as f32 * s, (max_c.1 + 1) as f32 * s, (max_c.2 + 1) as f32 * s)))
    }

    /// Average of 6 axis-aligned neighbors (for smooth brush).
    pub fn average_neighbors(&self, world_pos: Vec3) -> f32 {
        let (ix, iy, iz) = self.world_to_cell(world_pos);
        let c = |x: i32, y: i32, z: i32| -> f32 {
            self.cells.get(&(x, y, z)).copied().unwrap_or(0.0)
        };
        (c(ix-1,iy,iz) + c(ix+1,iy,iz) + c(ix,iy-1,iz) + c(ix,iy+1,iz) + c(ix,iy,iz-1) + c(ix,iy,iz+1)) / 6.0
    }

    /// Flatten grid data into a sorted Vec for GPU upload.
    /// Returns (cell_coords, delta_values) as flat arrays.
    pub fn to_gpu_data(&self) -> (Vec<[i32; 4]>, Vec<f32>) {
        let mut coords = Vec::with_capacity(self.cells.len());
        let mut values = Vec::with_capacity(self.cells.len());
        for (&(x, y, z), &v) in &self.cells {
            coords.push([x, y, z, 0]);
            values.push(v);
        }
        (coords, values)
    }

    /// Merge another grid's deltas into this grid (for "merge down").
    pub fn merge_from(&mut self, other: &SparseSDFGrid, opacity: f32) {
        for (&cell, &delta) in &other.cells {
            *self.cells.entry(cell).or_insert(0.0) += delta * opacity;
        }
    }
}

// ============================================================
// Sculpt Layer
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub enum LayerBlendMode {
    #[default]
    Add,
    Subtract,
    Max,
    Min,
}

impl LayerBlendMode {
    pub fn label(&self) -> &'static str {
        match self { Self::Add => "Add", Self::Subtract => "Subtract", Self::Max => "Max", Self::Min => "Min" }
    }
    pub const ALL: [LayerBlendMode; 4] = [Self::Add, Self::Subtract, Self::Max, Self::Min];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SculptLayer {
    pub name: String,
    pub id: u32,
    pub delta_grid: SparseSDFGrid,
    pub opacity: f32,
    pub visible: bool,
    pub blend_mode: LayerBlendMode,
}

impl SculptLayer {
    pub fn new(name: String, id: u32) -> Self {
        Self {
            name,
            id,
            delta_grid: SparseSDFGrid::new(0.02),
            opacity: 1.0,
            visible: true,
            blend_mode: LayerBlendMode::Add,
        }
    }

    /// Evaluate this layer's contribution at a world position.
    pub fn evaluate(&self, pos: Vec3) -> f32 {
        if !self.visible { return 0.0; }
        self.delta_grid.sample(pos) * self.opacity
    }
}

// ============================================================
// Brush Falloff
// ============================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BrushFalloff {
    Smooth,
    Round,
    Flat,
    Sharp,
}

impl Default for BrushFalloff {
    fn default() -> Self { Self::Smooth }
}

impl BrushFalloff {
    pub fn evaluate(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Smooth => 1.0 - (t * t * (3.0 - 2.0 * t)),
            Self::Round => (1.0 - t * t).sqrt(),
            Self::Flat => if t < 0.8 { 1.0 } else { 1.0 - ((t - 0.8) / 0.2).powi(2) },
            Self::Sharp => (1.0 - t).powi(2),
        }
    }
    pub fn label(&self) -> &'static str {
        match self { Self::Smooth => "Smooth", Self::Round => "Round", Self::Flat => "Flat", Self::Sharp => "Sharp" }
    }
    pub const ALL: [BrushFalloff; 4] = [Self::Smooth, Self::Round, Self::Flat, Self::Sharp];
}

// ============================================================
// Stroke Interpolator (spacing-resampled path + delayed Catmull-Rom)
// ============================================================

#[derive(Clone, Debug)]
pub struct StrokeSample {
    pub position: Vec3,
    pub normal: Vec3,
    pub pressure: f32,
    /// Multiplier on brush radius for this dab (brush dynamics Random Radius). Default 1.0.
    pub radius_scale: f32,
    /// Tip alpha rotation in radians (brush dynamics Random Rotation). Default 0.
    pub tip_rotation: f32,
}

impl StrokeSample {
    pub fn new(position: Vec3, normal: Vec3, pressure: f32) -> Self {
        Self {
            position,
            normal,
            pressure,
            radius_scale: 1.0,
            tip_rotation: 0.0,
        }
    }
}

impl Default for StrokeSample {
    fn default() -> Self {
        Self::new(Vec3::ZERO, Vec3::Y, 1.0)
    }
}

/// Evenly-spaced stamps along the stroke path.
///
/// Fixes prior bugs: (1) second event re-emitted first dab, (2) CR transition reprocessed
/// a linear segment, (3) last segment never flushed on release.
#[derive(Clone, Debug)]
pub struct StrokeInterpolator {
    history: Vec<StrokeSample>,
    pub spacing: f32,
    /// World-space distance remaining until the next stamp.
    dist_until_next: f32,
    /// Last fully processed segment index (segment i = history[i]→history[i+1]).
    last_emitted_seg: i32,
}

impl StrokeInterpolator {
    pub fn new(spacing: f32) -> Self {
        Self {
            history: Vec::with_capacity(8),
            spacing: spacing.max(0.01),
            dist_until_next: 0.0,
            last_emitted_seg: -1,
        }
    }

    pub fn reset(&mut self) {
        self.history.clear();
        self.dist_until_next = 0.0;
        self.last_emitted_seg = -1;
    }

    /// Push a new raw sample; return newly placed evaluation points for this event.
    pub fn push(&mut self, sample: StrokeSample, brush_radius: f32) -> Vec<StrokeSample> {
        let step = (self.spacing * brush_radius.max(1e-6)).max(1e-6);
        self.history.push(sample.clone());
        let n = self.history.len();

        if n == 1 {
            self.dist_until_next = step;
            self.last_emitted_seg = -1;
            return vec![sample];
        }

        let mut out = Vec::new();
        if n == 2 {
            // Linear first segment A→B.
            out.extend(self.emit_segment(0, step, false));
            self.last_emitted_seg = 0;
            return out;
        }
        if n == 3 {
            // Hold B→C until D arrives so CR does not reprocess a linear pass.
            return out;
        }
        // n >= 4: CR on newly completable interior segment (n-3)→(n-2).
        let seg = (n as i32) - 3;
        if seg > self.last_emitted_seg {
            out.extend(self.emit_segment(seg as usize, step, true));
            self.last_emitted_seg = seg;
        }
        if self.history.len() > 4 {
            let drop_n = self.history.len() - 4;
            self.history.drain(0..drop_n);
            self.last_emitted_seg -= drop_n as i32;
        }
        out
    }

    /// Pen-up: emit open tail segment(s) + tip dab if the path ended mid-spacing.
    pub fn flush(&mut self, brush_radius: f32) -> Vec<StrokeSample> {
        if self.history.is_empty() {
            return Vec::new();
        }
        let step = (self.spacing * brush_radius.max(1e-6)).max(1e-6);
        let mut out = Vec::new();
        let n = self.history.len();
        if n >= 2 {
            let last_seg = (n - 2) as i32;
            if last_seg > self.last_emitted_seg {
                for s in (self.last_emitted_seg + 1)..=last_seg {
                    if s >= 0 {
                        out.extend(self.emit_segment(s as usize, step, false));
                        self.last_emitted_seg = s;
                    }
                }
            }
        }
        if self.dist_until_next + 1e-6 < step {
            if let Some(last) = self.history.last() {
                out.push(last.clone());
            }
            self.dist_until_next = step;
        }
        out
    }

    fn emit_segment(&mut self, seg_i: usize, step: f32, use_cr: bool) -> Vec<StrokeSample> {
        if seg_i + 1 >= self.history.len() {
            return Vec::new();
        }
        let a = self.history[seg_i].clone();
        let b = self.history[seg_i + 1].clone();
        let segment_len = (b.position - a.position).length();
        if segment_len < 1e-8 {
            return Vec::new();
        }
        let cr_ok = use_cr && seg_i >= 1 && seg_i + 2 < self.history.len();
        let (p0, p3) = if cr_ok {
            (
                self.history[seg_i - 1].position,
                self.history[seg_i + 2].position,
            )
        } else {
            (a.position, b.position)
        };
        let p1 = a.position;
        let p2 = b.position;

        let mut out = Vec::new();
        let mut d = 0.0f32;
        while self.dist_until_next <= segment_len - d + 1e-8 {
            d += self.dist_until_next;
            if d > segment_len + 1e-5 {
                break;
            }
            let t = (d / segment_len).clamp(0.0, 1.0);
            let pos = if cr_ok {
                catmull_rom(p0, p1, p2, p3, t)
            } else {
                a.position.lerp(b.position, t)
            };
            out.push(StrokeSample {
                position: pos,
                normal: a.normal.lerp(b.normal, t).normalize_or_zero(),
                pressure: a.pressure + (b.pressure - a.pressure) * t,
                radius_scale: a.radius_scale + (b.radius_scale - a.radius_scale) * t,
                tip_rotation: 0.0,
            });
            self.dist_until_next = step;
        }
        let remain = (segment_len - d).max(0.0);
        self.dist_until_next = (self.dist_until_next - remain).max(0.0);
        if self.dist_until_next < 1e-8 {
            self.dist_until_next = step;
        }
        out
    }
}

fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

// ============================================================
// Brush Dynamics (ZBrush / Photoshop-style dab randomization)
// ============================================================

/// Knobs for per-dab randomization. Applied once after stroke interpolation, before symmetry.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrushDynamics {
    pub jitter: f32,
    pub scatter: f32,
    pub random_rotation: f32,
    pub random_strength: f32,
    pub random_radius: f32,
    pub stroke_noise: f32,
}

/// Deterministic u32 hash → [0, 1).
#[inline]
pub fn dyn_hash01(seed: u32) -> f32 {
    let mut x = seed.wrapping_mul(747796405).wrapping_add(2891336453);
    x = ((x >> ((x >> 28) + 4)) ^ x).wrapping_mul(277803737);
    x = (x >> 22) ^ x;
    (x as f32) * (1.0 / 4_294_967_295.0)
}

#[inline]
fn dyn_signed(seed: u32, channel: u32) -> f32 {
    dyn_hash01(seed.wrapping_add(channel.wrapping_mul(0x9E37_79B9))) * 2.0 - 1.0
}

fn stamp_plane_basis(normal: Vec3) -> (Vec3, Vec3) {
    let n = normal.normalize_or_zero();
    if n.length_squared() < 1e-12 {
        return (Vec3::X, Vec3::Z);
    }
    let up = if n.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let t = n.cross(up).normalize_or_zero();
    let b = n.cross(t).normalize_or_zero();
    (t, b)
}

/// Apply dynamics in-place to interpolated dabs (before symmetry expansion).
pub fn apply_brush_dynamics(
    pts: &mut [StrokeSample],
    brush_radius: f32,
    knobs: BrushDynamics,
    seed: u32,
) {
    if pts.is_empty() {
        return;
    }
    let r = brush_radius.max(1e-6);
    for (i, pt) in pts.iter_mut().enumerate() {
        let s = seed.wrapping_add((i as u32).wrapping_mul(0x9E37_79B9));
        let (t, b) = stamp_plane_basis(pt.normal);
        let mut offset = Vec3::ZERO;
        if knobs.jitter > 1e-6 {
            offset += (t * dyn_signed(s, 1) + b * dyn_signed(s, 2)) * (knobs.jitter * r * 0.35);
        }
        if knobs.scatter > 1e-6 {
            offset += (t * dyn_signed(s, 3) + b * dyn_signed(s, 4)) * (knobs.scatter * r);
        }
        if knobs.stroke_noise > 1e-6 {
            let ps = ((pt.position.x * 8.0) as u32)
                .wrapping_add(((pt.position.y * 8.0) as u32).wrapping_mul(17))
                .wrapping_add(((pt.position.z * 8.0) as u32).wrapping_mul(31));
            offset += (t * dyn_signed(ps, 5) + b * dyn_signed(ps, 6)) * (knobs.stroke_noise * r);
        }
        pt.position += offset;
        if knobs.random_rotation > 1e-6 {
            pt.tip_rotation += dyn_signed(s, 7) * knobs.random_rotation * std::f32::consts::PI;
        }
        if knobs.random_strength > 1e-6 {
            let f = 1.0 + dyn_signed(s, 8) * knobs.random_strength;
            pt.pressure = (pt.pressure * f).clamp(0.01, 2.0);
        }
        if knobs.random_radius > 1e-6 {
            let f = 1.0 + dyn_signed(s, 9) * knobs.random_radius;
            // Compose with pressure-to-radius / interpolated radius instead of replacing it.
            pt.radius_scale = (pt.radius_scale * f.clamp(0.15, 2.5)).max(1e-4);
        }
    }
}

#[cfg(test)]
mod stroke_interp_tests {
    use super::*;

    #[test]
    fn interpolated_dabs_preserve_pressure_radius_scale() {
        let mut interp = StrokeInterpolator::new(0.5);
        let mut a = StrokeSample::new(Vec3::ZERO, Vec3::Y, 1.0);
        a.radius_scale = 0.25;
        let mut b = StrokeSample::new(Vec3::new(2.0, 0.0, 0.0), Vec3::Y, 1.0);
        b.radius_scale = 1.0;

        let first = interp.push(a, 1.0);
        assert_eq!(first[0].radius_scale.to_bits(), 0.25f32.to_bits());
        let segment = interp.push(b, 1.0);
        assert!(!segment.is_empty());
        assert!(segment[0].radius_scale > 0.25 && segment[0].radius_scale < 1.0);
    }

    #[test]
    fn random_radius_composes_with_existing_dab_scale() {
        let knobs = BrushDynamics { random_radius: 1.0, ..Default::default() };
        let mut unit = [StrokeSample::new(Vec3::ZERO, Vec3::Y, 1.0)];
        let mut pressured = unit.clone();
        pressured[0].radius_scale = 0.4;

        apply_brush_dynamics(&mut unit, 1.0, knobs, 0x51CA_1E);
        apply_brush_dynamics(&mut pressured, 1.0, knobs, 0x51CA_1E);

        assert!((pressured[0].radius_scale - unit[0].radius_scale * 0.4).abs() < 1e-6);
    }

    #[test]
    fn first_point_not_reemitted_on_second_push() {
        let mut it = StrokeInterpolator::new(0.25);
        let p0 = it.push(StrokeSample::new(Vec3::ZERO, Vec3::Y, 1.0), 1.0);
        assert_eq!(p0.len(), 1);
        let p1 = it.push(StrokeSample::new(Vec3::new(1.0, 0.0, 0.0), Vec3::Y, 1.0), 1.0);
        for s in &p1 {
            assert!(s.position.x > 0.1, "re-emitted first dab at {:?}", s.position);
        }
    }

    #[test]
    fn flush_emits_held_tail() {
        let mut it = StrokeInterpolator::new(0.5);
        it.push(StrokeSample::new(Vec3::ZERO, Vec3::Y, 1.0), 1.0);
        it.push(StrokeSample::new(Vec3::new(0.4, 0.0, 0.0), Vec3::Y, 1.0), 1.0);
        let mid = it.push(StrokeSample::new(Vec3::new(0.8, 0.0, 0.0), Vec3::Y, 1.0), 1.0);
        assert!(mid.is_empty());
        assert!(!it.flush(1.0).is_empty());
    }
}

// ============================================================
// Brush delta computation
// ============================================================

/// Compute the SDF delta for a standard brush at a sample point.
pub fn standard_brush_delta(
    sample_pos: Vec3,
    brush_center: Vec3,
    _brush_normal: Vec3,
    brush_radius: f32,
    strength: f32,
    falloff: &BrushFalloff,
) -> f32 {
    let dist_to_center = (sample_pos - brush_center).length();
    let t = dist_to_center / brush_radius;
    if t > 1.0 { return 0.0; }
    -strength * falloff.evaluate(t)
}

/// Compute the SDF delta for a clay brush (flat-strip-of-clay feel).
pub fn clay_brush_delta(
    sample_pos: Vec3,
    brush_center: Vec3,
    brush_normal: Vec3,
    brush_radius: f32,
    strength: f32,
    falloff: &BrushFalloff,
) -> f32 {
    let offset = sample_pos - brush_center;
    let t = (offset.length() / brush_radius).clamp(0.0, 1.0);
    if t > 1.0 { return 0.0; }
    let influence = falloff.evaluate(t);
    let height_above_plane = offset.dot(brush_normal);
    let trim_height = strength * 0.5;
    let room = (trim_height - height_above_plane).max(0.0);
    -room.min(strength * influence)
}

/// Compute the SDF delta for a smooth brush.
pub fn smooth_brush_delta(
    sample_pos: Vec3,
    brush_center: Vec3,
    brush_radius: f32,
    strength: f32,
    falloff: &BrushFalloff,
    grid: &SparseSDFGrid,
) -> f32 {
    let t = ((sample_pos - brush_center).length() / brush_radius).clamp(0.0, 1.0);
    if t > 1.0 { return 0.0; }
    let influence = falloff.evaluate(t) * strength;
    let current = grid.sample(sample_pos);
    let avg = grid.average_neighbors(sample_pos);
    (avg - current) * influence
}

// ============================================================
// Undo support
// ============================================================

#[derive(Clone, Debug)]
pub struct SculptStrokeUndo {
    pub layer_id: u32,
    pub affected_cells: HashMap<(i32, i32, i32), f32>, // cell coord → old value before stroke
}

impl SculptStrokeUndo {
    pub fn new(layer_id: u32) -> Self {
        Self { layer_id, affected_cells: HashMap::new() }
    }

    /// Record a cell's current value before it's modified.
    pub fn record(&mut self, cell: (i32, i32, i32), old_value: f32) {
        self.affected_cells.entry(cell).or_insert(old_value);
    }

    /// Restore all recorded cells back to their old values.
    pub fn restore(&self, grid: &mut SparseSDFGrid) {
        for (&cell, &old_val) in &self.affected_cells {
            if old_val.abs() < 1e-7 {
                grid.cells.remove(&cell);
            } else {
                grid.cells.insert(cell, old_val);
            }
        }
    }
}
