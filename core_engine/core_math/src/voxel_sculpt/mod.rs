// core_engine/core_math/src/voxel_sculpt/mod.rs — facade for the voxel_sculpt module split
// (brush.rs / grid.rs / ops.rs / tests.rs). All public names re-exported: external paths
// core_math::voxel_sculpt::* are unchanged.
//
// NEW voxel sculpting core (3D-Coat-style). DELIBERATELY NOT an SDF:
// - The grid stores DENSITY / occupancy per voxel: 0.0 = empty, 1.0 = solid material.
//   Brushes ADD or REMOVE material (clamped to [0,1]) — you build clay, you don't
//   blend signed-distance spheres.
// - The renderable surface is extracted with NAIVE SURFACE NETS at the iso level
//   (default 0.5), producing a watertight quad mesh. That mesh is what renders (via
//   the existing raster/mesh pipeline) — there is no SDF raymarch.
//
// This module is pure CPU and unit-tested (`cargo test -p core_math`); it has no GPU
// or app dependencies, so its correctness is verified independent of the renderer.

mod brush;
pub use brush::*;
mod grid;
pub use grid::*;
mod octree_dc;
pub use octree_dc::OctreeCollapseCache;
mod ops;
// (no `pub use ops::*;` — ops.rs holds only `impl VoxelGrid` blocks, nothing to re-export)


#[cfg(test)]
mod tests;
