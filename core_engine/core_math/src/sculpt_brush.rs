// core_engine/core_math/src/sculpt_brush.rs
//
// Unified sculpt brush architecture.
// - BrushKernel trait: one impl per brush type (shared between all backends)
// - SculptBackend trait: one impl per target (SdfVolume now, SculptLayer future)
// - SculptBrush struct: artist-facing unified settings
//
// Adding a new brush = one struct + BrushKernel impl.
// Adding a new backend = one SculptBackend impl.

use glam::Vec3;
use crate::sdf_volume::{BrushType, FalloffCurve};

// ================================================================
// Brush context — passed to every kernel evaluation
// ================================================================
#[derive(Clone, Copy)]
pub struct BrushContext {
    pub sample_pos: Vec3,
    pub brush_center: Vec3,
    pub brush_normal: Vec3,
    pub brush_radius: f32,
    /// Already pressure-modulated strength
    pub strength: f32,
    pub falloff: FalloffCurve,
    /// Direction of current stroke segment (for crease/pinch)
    pub stroke_direction: Vec3,
    /// Reference plane for flatten/trim (normal.xyz, d in w)
    pub ref_plane: Option<[f32; 4]>,
    /// 6-neighbor average at this cell (precomputed for smooth brush)
    pub neighbors_avg: f32,
    /// Current SDF value at sample_pos
    pub current_value: f32,
    /// Distance from sample_pos to brush_center
    pub dist_to_center: f32,
}

// ================================================================
// BrushKernel trait — shared delta computation
// ================================================================

/// Returns the new SDF value at the sample position (NOT a delta — the absolute new value).
/// The backend writes this value directly.
pub trait BrushKernel {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32;
}

// ----------------------------------------------------------------
// Standard voxel displacement — shared base for the additive/subtractive brushes.
//
// Subtracting from a signed-distance field moves the zero level (the surface)
// OUTWARD along the gradient (= surface normal); adding moves it inward. With a
// radial falloff this is a smooth dome that FOLLOWS the existing surface — a
// standard voxel draw, NOT a deposited SDF sphere (the old `smin(field, sphere)`
// made blobby balls). A standard brush moves an existing surface, so an empty
// volume needs a base shape first (see SdfVolume::fill_sphere / fill_box).
//
// Strokes accumulate many stamps, so DRAW_FACTOR is intentionally modest; the
// artist tunes feel with the strength slider.
// ----------------------------------------------------------------
const DRAW_FACTOR: f32 = 0.2;

fn standard_displacement(ctx: &BrushContext, sign: f32) -> f32 {
    let t = ctx.dist_to_center / ctx.brush_radius;
    if t >= 1.0 { return ctx.current_value; }
    let w = ctx.falloff.evaluate(t) * ctx.strength;
    if w < 0.0001 { return ctx.current_value; }
    ctx.current_value + sign * w * ctx.brush_radius * DRAW_FACTOR
}

// Standard (Add) brush — push the surface outward (voxel draw).
pub struct StandardBrush;
impl BrushKernel for StandardBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        standard_displacement(ctx, -1.0)
    }
}

// Subtract brush — push the surface inward (carve); inverse of the draw.
pub struct SubtractBrush;
impl BrushKernel for SubtractBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        standard_displacement(ctx, 1.0)
    }
}

// ----------------------------------------------------------------
// Smooth brush — 6-neighbor average relaxation
// ----------------------------------------------------------------
pub struct SmoothBrush;
impl BrushKernel for SmoothBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        let t = ctx.dist_to_center / ctx.brush_radius;
        let w = ctx.falloff.evaluate(t) * ctx.strength;
        if w < 0.0001 { return ctx.current_value; }
        ctx.current_value + (ctx.neighbors_avg - ctx.current_value) * w * 0.5
    }
}

// ----------------------------------------------------------------
// Flatten brush — pushes surface toward a reference plane
// ----------------------------------------------------------------
pub struct FlattenBrush;
impl BrushKernel for FlattenBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        let t = ctx.dist_to_center / ctx.brush_radius;
        let w = ctx.falloff.evaluate(t) * ctx.strength;
        if w < 0.0001 { return ctx.current_value; }
        let plane_dist = (ctx.sample_pos - ctx.brush_center).dot(ctx.brush_normal);
        ctx.current_value + (plane_dist - ctx.current_value) * w * 0.3
    }
}

// ----------------------------------------------------------------
// Inflate brush — pushes surface outward uniformly
// ----------------------------------------------------------------
pub struct InflateBrush;
impl BrushKernel for InflateBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        let t = ctx.dist_to_center / ctx.brush_radius;
        let w = ctx.falloff.evaluate(t) * ctx.strength;
        if w < 0.0001 { return ctx.current_value; }
        let surface_proximity = (-ctx.current_value.abs() * 8.0).exp();
        ctx.current_value - w * surface_proximity * 0.05
    }
}

// ----------------------------------------------------------------
// Pinch brush — pulls voxels toward the stroke center line
// ----------------------------------------------------------------
pub struct PinchBrush;
impl BrushKernel for PinchBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        let t = ctx.dist_to_center / ctx.brush_radius;
        let w = ctx.falloff.evaluate(t) * ctx.strength;
        if w < 0.0001 { return ctx.current_value; }
        // Pull toward brush center — decrease distance for voxels near surface
        let surface_proximity = (-ctx.current_value.abs() * 6.0).exp();
        let toward_center = ctx.brush_center - ctx.sample_pos;
        let pull = toward_center.dot(ctx.brush_normal).abs() * 0.1;
        ctx.current_value - w * surface_proximity * pull
    }
}

// ----------------------------------------------------------------
// Clay brush — standard additive draw (builds material up). Same voxel
// surface-displacement model as Standard for now ("just standard, voxel tech for
// the moment"); no SDF-sphere deposit.
// ----------------------------------------------------------------
pub struct ClayBrush;
impl BrushKernel for ClayBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        standard_displacement(ctx, -1.0)
    }
}

// ----------------------------------------------------------------
// Crease brush — directional groove along stroke direction
// ----------------------------------------------------------------
pub struct CreaseBrush;
impl BrushKernel for CreaseBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        let t = ctx.dist_to_center / ctx.brush_radius;
        let w = ctx.falloff.evaluate(t) * ctx.strength;
        if w < 0.0001 { return ctx.current_value; }
        let surface_proximity = (-ctx.current_value.abs() * 6.0).exp();
        // Crease effect: stronger at center of stroke, V-shaped cross section
        let lateral = if ctx.stroke_direction.length_squared() > 0.001 {
            let perp = (ctx.sample_pos - ctx.brush_center)
                - ctx.stroke_direction * (ctx.sample_pos - ctx.brush_center).dot(ctx.stroke_direction);
            perp.length() / ctx.brush_radius
        } else {
            t
        };
        let v_shape = (1.0 - lateral.clamp(0.0, 1.0)).powi(2);
        ctx.current_value + w * surface_proximity * v_shape * 0.08
    }
}

// ----------------------------------------------------------------
// Trim brush — cuts flat at a reference plane
// ----------------------------------------------------------------
pub struct TrimBrush;
impl BrushKernel for TrimBrush {
    fn compute_new_value(&self, ctx: &BrushContext) -> f32 {
        let t = ctx.dist_to_center / ctx.brush_radius;
        let w = ctx.falloff.evaluate(t) * ctx.strength;
        if w < 0.0001 { return ctx.current_value; }
        // Trim: boolean-cut anything below the brush plane
        let plane_dist = (ctx.sample_pos - ctx.brush_center).dot(ctx.brush_normal);
        let trimmed = ctx.current_value.max(-plane_dist);
        ctx.current_value + (trimmed - ctx.current_value) * w
    }
}

// ================================================================
// Kernel dispatch — get the right kernel for a BrushType
// ================================================================

pub fn brush_kernel(bt: BrushType) -> &'static dyn BrushKernel {
    match bt {
        BrushType::Add      => &StandardBrush,
        BrushType::Subtract => &SubtractBrush,
        BrushType::Smooth   => &SmoothBrush,
        BrushType::Flatten  => &FlattenBrush,
        BrushType::Inflate  => &InflateBrush,
        BrushType::Pinch    => &PinchBrush,
        BrushType::Clay     => &ClayBrush,
        BrushType::Crease   => &CreaseBrush,
        BrushType::Trim     => &TrimBrush,
    }
}

// ================================================================
// SculptBackend trait
// ================================================================

pub trait SculptBackend {
    fn write_value(&mut self, ix: u32, iy: u32, iz: u32, value: f32);
    fn read_value(&self, ix: u32, iy: u32, iz: u32) -> f32;
    fn read_neighbors_avg(&self, ix: i32, iy: i32, iz: i32) -> f32;
    fn world_to_cell(&self, pos: Vec3) -> (i32, i32, i32);
    fn cell_to_world(&self, ix: u32, iy: u32, iz: u32) -> Vec3;
    fn in_bounds(&self, ix: i32, iy: i32, iz: i32) -> bool;
    fn resolution(&self) -> [u32; 3];
    fn voxel_size(&self) -> f32;
    fn grid_bounds(&self) -> f32;
    fn mark_dirty(&mut self, min: [u32; 3], max: [u32; 3]);
}

// ================================================================
// SculptBrush — unified artist-facing brush settings
// ================================================================

#[derive(Clone)]
pub struct SculptBrush {
    pub brush_type: BrushType,
    pub radius: f32,
    pub strength: f32,
    pub falloff: FalloffCurve,
    pub symmetry_x: bool,
    pub spacing: f32,
    pub lazy_mouse_enabled: bool,
    pub lazy_mouse_radius: f32,
}

impl Default for SculptBrush {
    fn default() -> Self {
        Self {
            brush_type: BrushType::Add,
            radius: 0.15,
            strength: 0.5,
            falloff: FalloffCurve::Smooth,
            symmetry_x: false,
            spacing: 0.12,
            lazy_mouse_enabled: false,
            lazy_mouse_radius: 0.05,
        }
    }
}

impl SculptBrush {
    /// Apply one brush stamp to the backend at the given center + normal.
    /// stroke_direction is the tangent of the current stroke segment.
    pub fn apply_stamp(
        &self,
        backend: &mut dyn SculptBackend,
        center: Vec3,
        normal: Vec3,
        stroke_direction: Vec3,
    ) {
        let kernel = brush_kernel(self.brush_type);
        let res = backend.resolution();
        let vs = backend.voxel_size();
        let bounds = backend.grid_bounds();
        let radius = self.radius;
        let blend = 0.15_f32;
        let pad = radius + vs * 2.0 + blend;

        let mut centers = vec![(center, normal)];
        if self.symmetry_x {
            centers.push((
                Vec3::new(-center.x, center.y, center.z),
                Vec3::new(-normal.x, normal.y, normal.z),
            ));
        }

        // For smooth brush: pre-snapshot needed (handled by the optimized voxel path)
        // This generic path is slightly slower for smooth but works for all backends.

        for (c, n) in &centers {
            let min_f = (*c - Vec3::splat(pad) + Vec3::splat(bounds)) / vs;
            let max_f = (*c + Vec3::splat(pad) + Vec3::splat(bounds)) / vs;
            let cx = min_f.x.floor().max(0.0) as u32;
            let cy = min_f.y.floor().max(0.0) as u32;
            let cz = min_f.z.floor().max(0.0) as u32;
            let mx = (max_f.x.ceil() as u32).min(res[0] - 1);
            let my = (max_f.y.ceil() as u32).min(res[1] - 1);
            let mz = (max_f.z.ceil() as u32).min(res[2] - 1);
            if cx > mx || cy > my || cz > mz { continue; }

            let mut touched = false;
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        let p = backend.cell_to_world(x, y, z);
                        let dist = p.distance(*c);
                        if dist > radius + blend { continue; }

                        let cur = backend.read_value(x, y, z);
                        let neighbors_avg = if self.brush_type == BrushType::Smooth {
                            backend.read_neighbors_avg(x as i32, y as i32, z as i32)
                        } else {
                            0.0
                        };

                        let ctx = BrushContext {
                            sample_pos: p,
                            brush_center: *c,
                            brush_normal: *n,
                            brush_radius: radius,
                            strength: self.strength,
                            falloff: self.falloff,
                            stroke_direction,
                            ref_plane: None,
                            neighbors_avg,
                            current_value: cur,
                            dist_to_center: dist,
                        };

                        let new_val = kernel.compute_new_value(&ctx);
                        if (new_val - cur).abs() > 0.0001 {
                            backend.write_value(x, y, z, new_val);
                            touched = true;
                        }
                    }
                }
            }
            if touched {
                backend.mark_dirty([cx, cy, cz], [mx, my, mz]);
            }
        }
    }
}

// ================================================================
// Helpers
// ================================================================

#[allow(dead_code)] // SDF smooth-min — no longer used by the standard displacement brushes
fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 1e-6 { return a.min(b); }
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    a * h + b * (1.0 - h) - k * h * (1.0 - h)
}
