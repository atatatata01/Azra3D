//! Pure SDF shape presets — single source of truth for default dimensions.
//!
//! Used by: `ops::sdf::*` create, interactive create_tool, future geometry nodes.
//! Does **not** touch scene / App / UI.

use glam::Vec3;
use core_math::primitives::{BoxBevel, CurvePlane, CurveProfile, CurveVertex, SdfShape};

/// Number of indexed footprint-create presets (0..=7).
pub const SHAPE_PRESET_COUNT: usize = 8;

/// Default shape presets with consistent dimensions:
/// - Basic shapes: bounding box ≈ 0.8³
/// - Capsule / torus fitted to that envelope
pub fn by_index(idx: usize) -> SdfShape {
    match idx % SHAPE_PRESET_COUNT {
        0 => sphere(),
        1 => box_shape(),
        2 => cylinder(),
        3 => torus(),
        4 => cone(),
        5 => capsule(),
        6 => stair(),
        _ => spiral_stair(),
    }
}

/// Near-zero presets for footprint-drag grow (create_tool session).
pub fn by_index_tiny(idx: usize) -> SdfShape {
    match idx % SHAPE_PRESET_COUNT {
        0 => SdfShape::Sphere { radius: 0.001 },
        1 => SdfShape::Box {
            half_extents: Vec3::splat(0.001),
            bevel: BoxBevel::default(),
        },
        2 => SdfShape::Cylinder {
            radius: 0.001,
            half_height: 0.001,
        },
        3 => SdfShape::Torus {
            major_radius: 0.001,
            minor_radius: 0.001,
        },
        4 => SdfShape::Cone {
            radius: 0.001,
            half_height: 0.001,
        },
        5 => SdfShape::Capsule {
            radius: 0.001,
            half_height: 0.001,
        },
        6 => SdfShape::Stair {
            half_extents: Vec3::splat(0.001),
            num_steps: 5,
        },
        _ => SdfShape::SpiralStair {
            radius: 0.001,
            half_height: 0.001,
            num_steps: 12,
            step_width: 0.001,
        },
    }
}

pub fn sphere() -> SdfShape {
    SdfShape::Sphere { radius: 0.4 }
}

pub fn box_shape() -> SdfShape {
    SdfShape::Box {
        half_extents: Vec3::splat(0.4),
        bevel: BoxBevel::default(),
    }
}

pub fn cylinder() -> SdfShape {
    SdfShape::Cylinder {
        radius: 0.4,
        half_height: 0.4,
    }
}

pub fn torus() -> SdfShape {
    SdfShape::Torus {
        major_radius: 0.3,
        minor_radius: 0.1,
    }
}

pub fn cone() -> SdfShape {
    SdfShape::Cone {
        radius: 0.4,
        half_height: 0.4,
    }
}

pub fn capsule() -> SdfShape {
    SdfShape::Capsule {
        radius: 0.2,
        half_height: 0.2,
    }
}

pub fn stair() -> SdfShape {
    SdfShape::Stair {
        half_extents: Vec3::new(0.5, 0.45, 0.7),
        num_steps: 5,
    }
}

pub fn spiral_stair() -> SdfShape {
    SdfShape::SpiralStair {
        radius: 1.0,
        half_height: 1.0,
        num_steps: 12,
        step_width: 0.3,
    }
}

/// Square profile extruded curve (default polyline prim).
pub fn curve_profile() -> SdfShape {
    let s = 0.4_f32;
    let verts = vec![
        CurveVertex {
            pos: glam::Vec2::new(-s, -s),
            bevel_radius: 0.0,
        },
        CurveVertex {
            pos: glam::Vec2::new(s, -s),
            bevel_radius: 0.0,
        },
        CurveVertex {
            pos: glam::Vec2::new(s, s),
            bevel_radius: 0.0,
        },
        CurveVertex {
            pos: glam::Vec2::new(-s, s),
            bevel_radius: 0.0,
        },
    ];
    SdfShape::CurveExtrude {
        profile: CurveProfile {
            verts,
            plane: CurvePlane::default(),
        },
        half_depth: 0.4,
        wall_t: 0.0,
        rim: 0.0,
        open: false,
        thickness: 0.0,
        tube: false,
    }
}

/// Half-extent along the "up" of a shape — used for surface offset on click-create.
pub fn half_extent_along_up(shape: &SdfShape) -> f32 {
    match shape {
        SdfShape::Box { half_extents, .. } => half_extents.y,
        SdfShape::Sphere { radius } => *radius,
        SdfShape::Cylinder { half_height, .. }
        | SdfShape::Cone { half_height, .. }
        | SdfShape::Capsule { half_height, .. } => *half_height,
        SdfShape::Torus { minor_radius, .. } => *minor_radius,
        SdfShape::Stair { half_extents, .. } => half_extents.y,
        SdfShape::SpiralStair { half_height, .. } => *half_height,
        SdfShape::Billboard { height, .. } => *height * 0.5,
        SdfShape::CurveExtrude { half_depth, .. } => *half_depth,
        SdfShape::MeshVolume { grid, .. } => grid.corner_abs_max().y,
        SdfShape::HalfSpace => 0.0,
        // Spine sits ON the hit/workplane (no offset) so seed + new points share one plane.
        SdfShape::SpinePipe { .. } => 0.0,
    }
}
