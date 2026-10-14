//! Pure geometry kernels — **no App**, no UI.
//!
//! Future procedural / geometry nodes call these for shape data and placement math.
//! App-facing create still goes through `ops::sdf::*` / `ops::create::*`.
//!
//! | Kernel | Module | Callable later as GeoOp node |
//! |--------|--------|------------------------------|
//! | Default SDF presets | `shape_presets` | Sphere / Box / … / Curve profile |
//! | Ray → create plane | `placement_math::compute_creation_plane` | Place On Surface |
//! | Extrude fence depth | `placement_math::compute_extrude_depth` | Extrude Depth |

pub mod shape_presets;
pub mod placement_math;

pub use shape_presets::{
    by_index, by_index_tiny, curve_profile, half_extent_along_up, SHAPE_PRESET_COUNT,
    sphere, box_shape, cylinder, torus, cone, capsule, stair, spiral_stair,
};
pub use placement_math::{compute_creation_plane, compute_extrude_depth};
