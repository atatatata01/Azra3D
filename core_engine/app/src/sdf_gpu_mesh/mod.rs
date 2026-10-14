//! Phase G — GPU QEF Dual-Contouring mesher (`sdf_mesh.wgsl`).
//!
//! Two layers, sharing the pipeline build + dispatch:
//! - `mesh_sdf_grid_gpu` — G0 readback harness (one submit + blocking readback). Used by the parity
//!   test; proves the GPU mesh matches the CPU `chunked_dc` reference (sharp QEF features).
//! - `SdfMeshGpu` — G1 resident mesher: persistent pipelines, dispatches into caller-owned persistent
//!   buffers, and writes the index count straight into a `DrawIndexedIndirect` args buffer via a GPU
//!   buffer copy — NO readback. This is the device-loss-safe indirect-draw path the live cut-over uses.
//!
//! This is a directory module. The DC plumbing, readback layer, and resident mesher were relocated
//! verbatim into focused submodules (app-crate de-god, slices 3-5); the glob re-exports below keep
//! every `crate::sdf_gpu_mesh::...` path resolving unchanged.

mod dc_pipeline;
mod lod_policy;
mod pool;
mod readback;
mod resident;

pub(crate) use dc_pipeline::*;
pub(crate) use lod_policy::*;
pub(crate) use pool::*;
pub(crate) use readback::*;
pub(crate) use resident::*;

// Test-only sibling modules (app-crate de-god, slices 6-7). Each is `#![cfg(test)]` and pulls the
// re-exported DC/readback/resident items via `use crate::sdf_gpu_mesh::*`. `tests_dc` owns the shared
// `headless_device` helper; `tests_bake` imports it via `use super::tests_dc::headless_device;`.
#[cfg(test)]
mod tests_dc;
#[cfg(test)]
mod tests_bake;
// S2.3: hardware parity harness for the SparseBricks bake mode (output_mode=2) — classify (CPU) →
// real-shader mode-2 dispatch → pool decode → dense-oracle compare. Shares `headless_device` too.
#[cfg(test)]
mod tests_sparse_bake;
// GPU-classify SPIKE (stage 1): per-brick verdict parity — CPU BrickClassifier brute force vs
// the `classify_bricks` compute entry in volume_bake.wgsl. No production dispatch yet.
#[cfg(test)]
mod tests_gpu_classify;
// GPU CURVATURE golden parity gate (GPU_CURVATURE_KERNEL_PLAN.md P1): the production CPU
// `sdf_curvature_masks` vs the real `curvature_eval` entry in volume_bake.wgsl, over the same
// body and the same UV bake grid. Same device/BGL/shader-build harness as the classify gate.
#[cfg(test)]
mod tests_gpu_curvature;
#[cfg(test)]
mod tests_mesh_curvature;
