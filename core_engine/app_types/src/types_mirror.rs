// Smart Mirror + Universal Symmetry types — extracted from types.rs
use eframe::egui;
use glam::Vec3;
use serde::{Serialize, Deserialize};

// ==================================================================
// Smart Mirror Tool (Alt+X)
// ==================================================================
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum SmartMirrorPhase {
    Placing,     // plane tracks mouse (surface/workplane)
    Adjusting,   // plane anchored, user drags/rotates, then confirms
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum SmartMirrorAction {
    Mirror,      // default: set mirror_flags + mirror_center
    Instance,    // Shift: create reflected instances
    DeleteHalf,  // Shift+Alt: delete prims on one side
}

pub struct SmartMirrorState {
    pub phase: SmartMirrorPhase,
    pub plane_origin: Vec3,
    pub plane_normal: Vec3,
    pub base_normal: Vec3,      // pre-rotation normal (for clean rotation accumulation)
    pub surface_normal: Vec3,   // surface normal at hit point (for tangent frame alignment)
    pub tangent_u: Vec3,
    pub tangent_v: Vec3,
    pub snapped_axis: u8,       // 0=X, 1=Y, 2=Z — nearest world axis to plane_normal
    pub from_workplane: bool,
    pub rotation_angle: f32,    // accumulated rotation (radians)
    pub drag_start_mouse: Option<egui::Pos2>,
    pub drag_start_origin: Vec3,
    pub action: SmartMirrorAction,
    pub half_delete_indices: Vec<usize>,
}

// ==================================================================
// Poly Smart Symmetry (Alt+X on polygonal mesh) — place plane + symmetrize
// NOT Smart Mirror (SDF prim unfinished tool). Uses the universal SymmetryPlane visual.
// ==================================================================
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum PolySymmetryPhase {
    Placing,
    Adjusting,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum PolySymmetryAction {
    /// Keep +normal half, mirror onto −normal (mesh.symmetrize).
    Symmetrize,
    /// Delete −normal half only.
    Cut,
    /// Cut + separate mirrored instance object.
    Instance,
}

pub struct PolySymmetryState {
    pub phase: PolySymmetryPhase,
    pub nid: core_math::scene_graph::NodeId,
    pub plane_origin: Vec3,
    pub plane_normal: Vec3,
    pub base_normal: Vec3,
    pub surface_normal: Vec3,
    pub tangent_u: Vec3,
    pub tangent_v: Vec3,
    pub snapped_axis: u8,
    pub rotation_angle: f32,
    pub drag_start_mouse: Option<egui::Pos2>,
    pub drag_start_origin: Vec3,
    pub action: PolySymmetryAction,
}

/// Compute tangent frame for mirror plane aligned to a surface.
/// tangent_u lies in the surface, tangent_v extends along the surface normal.
pub fn mirror_tangent_frame(plane_normal: Vec3, surface_normal: Vec3) -> (Vec3, Vec3) {
    let tu = surface_normal.cross(plane_normal);
    if tu.length_squared() > 0.001 {
        let tu = tu.normalize();
        let tv = plane_normal.cross(tu).normalize();
        (tu, tv)
    } else {
        // Fallback when plane_normal ≈ surface_normal
        let up = if plane_normal.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let tu = up.cross(plane_normal).normalize();
        let tv = plane_normal.cross(tu).normalize();
        (tu, tv)
    }
}

// ==================================================================
// Universal Symmetry Tool
// ==================================================================
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum SymmetrySide { Positive, Negative }

impl Default for SymmetrySide { fn default() -> Self { Self::Positive } }

/// Which symmetry operation is active in the symmetry tool.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SymmetryOp {
    MirrorEdit,     // live symmetric editing mode (toggle)
    Symmetrize,     // one-shot: overwrite target from source
    InstantMirror,  // duplicate + mirror
    Flip,           // reflect in-place
    DeleteHalf,     // remove one side
    MirrorModifier, // add non-destructive modifier
}

/// Persistent symmetry plane — exists per-object and globally.
#[derive(Clone, Serialize, Deserialize)]
pub struct SymmetryPlane {
    pub origin: Vec3,              // point on the plane
    pub normal: Vec3,              // plane normal (unit vector)
    pub enabled: bool,             // is symmetry active
    pub visible: bool,             // render the plane visual
    pub source_side: SymmetrySide, // which side is "truth"
    pub merge_threshold: f32,      // weld distance at the seam (poly/NURBS)
    pub use_topology: bool,        // poly: topology matching vs position matching
    pub clipping: bool,            // prevent verts from crossing plane
    pub radial_count: u8,          // 0 = off, 2..32 = radial N-way
    pub radial_include_mirror: bool,
}

impl Default for SymmetryPlane {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            normal: Vec3::X,           // default: YZ plane (left/right mirror)
            enabled: false,
            visible: true,
            source_side: SymmetrySide::Positive,
            merge_threshold: 0.001,
            use_topology: false,
            clipping: true,
            radial_count: 0,
            radial_include_mirror: false,
        }
    }
}

impl SymmetryPlane {
    /// Reflect a point across this symmetry plane.
    pub fn reflect_point(&self, p: Vec3) -> Vec3 {
        let d = (p - self.origin).dot(self.normal);
        p - 2.0 * d * self.normal
    }

    /// Reflect a direction vector across this symmetry plane.
    pub fn reflect_dir(&self, d: Vec3) -> Vec3 {
        d - 2.0 * d.dot(self.normal) * self.normal
    }

    /// Signed distance from a point to the plane (positive = source side when source_side=Positive).
    pub fn signed_distance(&self, p: Vec3) -> f32 {
        (p - self.origin).dot(self.normal)
    }

    /// Classify which side a point is on.
    pub fn classify_side(&self, p: Vec3) -> SymmetrySide {
        if self.signed_distance(p) >= 0.0 { SymmetrySide::Positive } else { SymmetrySide::Negative }
    }

    /// Whether a point is on the source side (the "truth" side).
    pub fn is_source_side(&self, p: Vec3) -> bool {
        let d = self.signed_distance(p);
        match self.source_side {
            SymmetrySide::Positive => d >= -self.merge_threshold,
            SymmetrySide::Negative => d <= self.merge_threshold,
        }
    }

    /// Whether a point is on the symmetry seam (within merge threshold).
    pub fn is_on_seam(&self, p: Vec3) -> bool {
        self.signed_distance(p).abs() <= self.merge_threshold
    }

    /// Set plane from a world axis (0=X→YZ plane, 1=Y→XZ plane, 2=Z→XY plane).
    pub fn set_axis(&mut self, axis: u8, center: Vec3) {
        self.origin = center;
        self.normal = match axis {
            0 => Vec3::X,
            1 => Vec3::Y,
            _ => Vec3::Z,
        };
    }

    /// Tangent frame for rendering.
    pub fn tangent_frame(&self) -> (Vec3, Vec3) {
        let up = if self.normal.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let tu = up.cross(self.normal).normalize();
        let tv = self.normal.cross(tu).normalize();
        (tu, tv)
    }
}

/// Snap a rotation angle to increments (e.g. 45 degrees).
pub fn snap_angle(angle_rad: f32, increment_deg: f32) -> f32 {
    let inc = increment_deg.to_radians();
    (angle_rad / inc).round() * inc
}
