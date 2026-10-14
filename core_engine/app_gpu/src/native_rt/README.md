# Native triangle ray queries

`GpuCtx.native_rt` owns `NativeRtRenderer`. This is a triangle acceleration cache,
not a second material implementation. It accepts the resident PT vertex and
triangle buffers. The only generated geometry buffer contains the actual three
global vertex indices from each `PtTri`; vertices, normals, UVs and materials
are neither downloaded nor uploaded again.

## Integration contract

- Request `REQUIRED_FEATURES` on the main Vulkan device only when supported.
  Allocate its PT vertex buffer with `BLAS_INPUT` as well as existing usages.
- Compile `NativeRtPipelines::compile` on the existing background warmup worker,
  never from `prepare` or the frame thread. Pass the canonical PT group-0 layout
  and graphics' separately specialized full and interactive material-preserving
  native ray-query WGSL. Group 1 binding 0 is the acceleration structure. Compile
  each canonical mode before adapting traversal; this module adds no overrides.
  Use `app_gpu::native_rt::build_native_rt_wgsl` to adapt each output of
  `graphics::build_pathtrace_tri_wgsl_with_mode(Some(false/true))`. The canonical
  helper and traversal source live here in the default build; the legacy CUDA
  helper only re-exports this implementation and is not a production dependency.
- Call `set_scene` on scene upload/refit events, not every frame. Use canonical
  `PtInstanceGpu` order (`NativeInstance::from`), all unique triangle ranges, and
  exact used vertex/triangle counts. Offsets are global. Every unique geometry
  in the packed triangle array must have an instance; ranges end at the next
  unique `tri_off` or the total triangle count. Shared geometry shares a BLAS.
- Bump `geometry` whenever geometry contents/layout change, including in-place
  writes. Bump `transforms` for placement/order changes, not material changes.
  Buffer identity/count changes also invalidate geometry. Explicitly `clear`
  on teardown/device replacement. No cache survives device loss.
- Call `prepare` with the frame's owned encoder and scheduler-approved budget.
  It records at most two BLAS builds and one TLAS build. A mesh larger than the
  triangle budget remains pending; `NativeRtStatus` reports its actual size.
  The scheduler must grant an appropriate idle budget; no oversized mesh is
  truncated. TLAS creation is admitted only within `max_tlas_instances`.
- Retain the existing backend/resolution interaction latch. Select this backend
  only with ready pipelines and `NativeRtStatus::Ready`; empty scenes use the
  existing valid floor/background path. Never combine two render techniques.
- Pass the current canonical group-0 bind group into `frame.encode_pt` every
  time. That group owns existing live materials, texture arrays, normal/bump,
  roughness, emission, lights, HDR and accumulation state. Native intersection
  `instance_custom_data` is the unchanged PT GPU instance index; primitive index
  is local to that instance's `tri_off`. No material data is stored here.
- Remaining frame passes use `frame.encoder_mut()`. Do not replace/extract that
  encoder. Finish via `frame.submit(queue)` and then `renderer.commit(token)`.
  A dropped frame never populates the cache. Tokens from stale scene revisions
  or other renderers are rejected, as are out-of-order older frame tokens.
  wgpu owns barriers and resource retention; no raw HAL or external memory is used.
- For timestamp wrappers that already borrow the encoder, first obtain an owned
  `frame.dispatch_lease()`, then call `lease.encode(encoder, pipelines, group0,
  extent, interactive)` inside the wrapper. `encode_pt` delegates to this same
  implementation. The lease retains the real TLAS package and binding but does
  not certify submission, submit commands, or promote caches. Use only the
  originating frame's encoder and finish through that frame's `submit`/`commit`.
  Public wgpu exposes no encoder identity for a runtime identity check; its AS
  build/use validation and tracked barriers remain active. A lease does not make
  an abandoned frame's unsubmitted build ready on a different encoder.

## Version and limitations

wgpu 25.0.2 / Naga 25.0.1 fixes SPIR-V ray-query miss-field reads present in
wgpu 24's Naga. Eframe/egui 0.32.3 uses this same public wgpu generation.
wgpu-core 25 implements acceleration updates as builds, not true hardware
refits. Geometry changes rebuild BLASes; transforms rebuild only the TLAS.
Opaque traversal matches canonical triangle PT (transmission is handled by the
material integrator); alpha-tested candidate rejection is not added here.

This module does not turn on routing. Production completion also requires the
parent's device gates, warmup publication, upload usage/epoch plumbing, canonical
graphics query variant and viewport/latch wiring. Tests do not substitute for a
real material/lighting parity render.

## Validation

Coordinate Cargo/GPU access with the parent before running:

`cargo test -p app_gpu native_rt::tests`

`cargo test -p app_gpu native_rt::tests::gpu_native_global_indices_shared_instances_and_submission_lifecycle -- --ignored --exact`

The ignored test uses actual Vulkan AS builds and checks global indices, shared
BLAS instances, closest hits, misses, transforms, capped build progression,
abandoned encoders and stale submission rejection. It is deliberately a traversal
test, not a full material-parity claim. `cargo test -p graphics` remains required
for the canonical shader changes.
