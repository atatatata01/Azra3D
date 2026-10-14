// GPU Surface Nets — runs `voxel_mesh.wgsl` (sn_vertex + sn_quads) to mesh a cell range on the
// GPU, the GPU counterpart of `VoxelGrid::mesh_region`. Phase 1b of GPU_SCULPT_PLAN.md.
//
// This module currently provides a PARITY-CHECK path only: it builds the pipeline on demand, does
// a BLOCKING readback, and is invoked from a debug button — so we can validate the shader produces
// the same surface as the CPU mesher BEFORE wiring it into the live render path (Phase 1c). It
// touches NO render state and persists nothing, so it carries zero regression risk.
//
// Directory module layout (god-file split, pure relocation):
//   mesh_build.rs — Surface-Nets mesher, VoxelChunkGpu, pool, inflight readback, parity helpers
//   upload.rs     — GPU brush density stamp path
//   atlas.rs      — VoxelAtlasGpu + atlas-path GPU undo
// Facade re-exports keep every `crate::voxel_gpu_mesh::*` path resolving unchanged.

mod mesh_build;
mod upload;
mod atlas;

pub(crate) use mesh_build::*;
pub(crate) use upload::*;
pub(crate) use atlas::*;
