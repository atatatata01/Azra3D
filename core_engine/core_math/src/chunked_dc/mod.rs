// core_engine/core_math/src/chunked_dc/mod.rs
// Chunked Dual Contouring: divide the SDF grid into 32^3 chunks and only
// re-extract DC for chunks that overlap a dirty region.
//
// Module layout (pure relocation, GOD_FILES_SPLIT_PLAN):
//   grid.rs    — VolumeAccessor + sparse brick classify/window
//   extract.rs — per-chunk DC extract + ChunkedMesh
//   stitch.rs  — merge chunks to unified mesh (merge_to_mesh / to_mesh)
//   tests.rs   — unit/integration tests

mod grid;
pub use grid::{
    VolumeAccessor, FlatGridAccessor, BrickGridAccessor,
    BRICK_TAG_EXTERIOR, BRICK_TAG_INTERIOR, BRICK_FAR,
    SparseSdfBricks, classify_grid_to_sparse,
    NONLIPSCHITZ_MARGIN_FACTOR, bricks_per_axis, classifier_margin,
    SparseIndexOnly, BrickClass, BrickClassifier,
    classify_prims_to_sparse_index, classify_with_classifier,
    sparse_index_from_verdicts, sparse_index_translate,
    dilate_voxel_window, sparse_window_bricks_all_handled,
    SparseWindowIncompat, sparse_window_tags_compatible,
    SparseWindowAlloc, sparse_window_allocate_incompatible,
};

// Private helpers elevated pub(super) in grid; re-import for in-module tests
// (same pattern as display_mesh_ops::weld_by_distance).
#[cfg(test)]
use grid::{dilate_surface_bricks_1ring, emit_sparse_index};

mod extract;
pub use extract::{ChunkMesh, ChunkedMesh};
pub(crate) use extract::repair_degenerate_normals;

mod stitch; // ChunkedMesh::merge_to_mesh / to_mesh

#[cfg(test)]
mod tests;
