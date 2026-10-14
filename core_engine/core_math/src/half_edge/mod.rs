// ============================================================
// Half-Edge Mesh — Topology-aware polygonal mesh for editing
// ============================================================

use glam::{Vec2, Vec3};
use geom::best_fit_circle_2d;

mod ids;
mod types;
mod geom;
mod alloc;
mod access;
mod nav;
mod normals;
mod euler;
mod validate;
mod builders;
mod rebuild_ids;
mod ops_extrude;
mod ops_inset;
mod ops_loop_cut;
mod ops_cap;
mod ops_cleanup;
mod ops_dice;
mod ops_dissolve;
mod ops_dyntopo;
mod ops_misc;

pub use ids::*;
pub use types::*;
pub use geom::*;
pub use ops_cleanup::LooseCounts;
#[cfg(test)]
pub(crate) use rebuild_ids::test_support as rebuild_ids_test_support;
pub use ops_dyntopo::{DyntopoParams, DyntopoResult, DYNTOPO_COLLAPSE_RATIO};
pub use normals::{smooth_corner_normals, smooth_corner_normals_ex, weighted_corner_normals};

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests;

/// Per-face `material_id` survival across mesh-rebuilding ops (dissolve / mirror / symmetrize).
#[cfg(test)]
mod tests_materials;

/// Element-id survival across mesh-rebuilding ops (dice / subdivide / poke / weld …).
#[cfg(test)]
mod tests_ids;
