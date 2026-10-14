// Create Tool types — extracted from types.rs
use eframe::egui;
use glam::Vec3;
use core_math::primitives::BooleanOp;

// ==================================================================
// Unified Creation Tool (Modo-style: Ctrl+Click instant, drag footprint+extrude)
// ==================================================================
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CreatePhase { Footprint, Extrude }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CreateOp { Union, Subtract, Intersect, Slice }

impl CreateOp {
    pub fn label(&self) -> &'static str {
        match self { Self::Union => "UNION", Self::Subtract => "SUBTRACT", Self::Intersect => "INTERSECT", Self::Slice => "SLICE" }
    }
    pub fn color(&self) -> egui::Color32 {
        match self {
            Self::Union     => egui::Color32::from_rgb(80, 220, 80),
            Self::Subtract  => egui::Color32::from_rgb(255, 80, 80),
            Self::Intersect => egui::Color32::from_rgb(80, 140, 255),
            Self::Slice     => egui::Color32::from_rgb(255, 220, 60),
        }
    }
    pub fn to_boolean_op(&self, k: f32) -> BooleanOp {
        match self {
            Self::Union     => BooleanOp::Union { k },
            Self::Subtract  => BooleanOp::Subtract { k },
            Self::Intersect => BooleanOp::Intersect,
            Self::Slice     => BooleanOp::Subtract { k: 0.0 },
        }
    }
}

pub struct CreateState {
    pub phase: CreatePhase,
    pub op: CreateOp,
    pub shape_idx: usize,       // which shape preset (0=Sphere..7=SpiralStair)
    // Surface hit info (locked at creation start)
    pub hit_point: Vec3,
    pub hit_normal: Vec3,
    pub tangent_u: Vec3,
    pub tangent_v: Vec3,
    pub anchor: Vec3,
    // Footprint rectangle
    pub local_u: f32,
    pub local_v: f32,
    // Depth (ray-to-axis closest-point projection)
    pub depth: f32,
    pub depth_start_t: f32, // axis parameter t at extrude start (subtracted each frame)
    // Bevel (box-only)
    pub corner_radius: f32,     // vertical edges (8-11)
    pub corner_radius_tb: f32,  // top + bottom edges (0-7)
    pub corner_chamfer: bool,
    // Blend
    pub blend_k: f32,
    // Live primitive index
    pub prim_idx: usize,
    // Numeric input
    pub num_buf: String,
    // Ctrl+Shift = center-based creation (symmetric growth from click point)
    pub center_mode: bool,
    // Alt rotation
    pub rotation_angle: f32,       // degrees of rotation from Alt drag
    pub base_rotation_deg: Vec3,   // initial rotation from tangent frame
    // Set true during MMB orbit to freeze depth accumulation
    pub depth_orbit_frozen: bool,
}

// ==================================================================
// Create Tool Settings (persistent, numeric, configurable)
// ==================================================================
#[derive(Clone)]
pub struct CreateToolSettings {
    // Dimension overrides (0.0 = use drag value, >0 = locked)
    pub width: f32,
    pub depth: f32,
    pub height: f32,
    pub lock_width: bool,
    pub lock_depth: bool,
    pub lock_height: bool,
    // Constraints
    pub snap_grid_size: f32,
}

impl Default for CreateToolSettings {
    fn default() -> Self {
        Self {
            width: 0.0, depth: 0.0, height: 0.0,
            lock_width: false, lock_depth: false, lock_height: false,
            snap_grid_size: 0.5,
        }
    }
}

// ==================================================================
// Smart Snap (shared types — used by smart_snap.rs)
// ==================================================================
#[derive(PartialEq, Clone, Copy, Debug, Default)]
pub enum SnapFilter {
    #[default]
    All,       // center-of-mass / selection center / C filter
    Vertex,    // V: vertices/corners/origins only
    Edge,      // E: edge midpoints or along-edge
    Face,      // F: face centers only
    Grid,      // G: grid lattice (visual kind)
    Volume,    // solid AABB center / surface hit
}
impl SnapFilter {
    pub fn label(&self) -> &'static str {
        match self {
            Self::All => "Center",
            Self::Vertex => "Vertex",
            Self::Edge => "Edge",
            Self::Face => "Face",
            Self::Grid => "Grid",
            Self::Volume => "Volume",
        }
    }
    /// Tie-break priority (lower wins when screen distances are nearly equal).
    pub fn priority(self) -> u8 {
        match self {
            Self::Vertex => 0,
            Self::Edge => 1,
            Self::Face => 2,
            Self::Volume => 3,
            Self::All => 4,
            Self::Grid => 5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SnapCandidate {
    pub world_pos: Vec3,
    pub kind: SnapFilter,  // which category this point belongs to
    pub label: String,      // "Box.001 corner" etc.
    pub prim_idx: usize,
    /// When kind is Edge, optional segment endpoints for highlight / along-edge.
    pub edge: Option<(Vec3, Vec3)>,
    /// World-space surface normal when known (face centers, volume surface, grid floor).
    /// Used by Align Rotation to Target (Blender parity).
    pub normal: Option<Vec3>,
}

impl SnapCandidate {
    pub fn point(world_pos: Vec3, kind: SnapFilter, label: impl Into<String>, prim_idx: usize) -> Self {
        Self {
            world_pos,
            kind,
            label: label.into(),
            prim_idx,
            edge: None,
            normal: None,
        }
    }
    pub fn edge_pt(
        world_pos: Vec3,
        a: Vec3,
        b: Vec3,
        label: impl Into<String>,
        prim_idx: usize,
    ) -> Self {
        Self {
            world_pos,
            kind: SnapFilter::Edge,
            label: label.into(),
            prim_idx,
            edge: Some((a, b)),
            normal: None,
        }
    }
    /// Attach a world-space normal for Align Rotation to Target.
    pub fn with_normal(mut self, n: Vec3) -> Self {
        let nn = n.normalize_or_zero();
        self.normal = if nn.length_squared() > 1e-8 { Some(nn) } else { None };
        self
    }
}

/// Live hover / transform snap hit (pre-highlight + G/R/S).
#[derive(Clone, Debug)]
pub struct SmartSnapHit {
    pub world_pos: Vec3,
    pub label: String,
    pub kind: SnapFilter,
    pub edge: Option<(Vec3, Vec3)>,
    /// World-space surface normal when known (face / volume surface / grid).
    pub normal: Option<Vec3>,
}

impl SmartSnapHit {
    pub fn from_candidate(c: &SnapCandidate) -> Self {
        Self {
            world_pos: c.world_pos,
            label: c.label.clone(),
            kind: c.kind,
            edge: c.edge,
            normal: c.normal,
        }
    }
}
