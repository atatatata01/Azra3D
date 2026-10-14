# RTX triangle queries: prerequisite, not a production renderer

## Status

`hw_rt_available()` remains false. `CudaRenderer::render` rejects path tracing
with an explicit parity diagnostic before uploading data or dispatching a legacy
kernel. The optional OptiX build compiles `optix_query_tri.cu`, not the old flat
material path tracer or analytical SDF OptiX program. Runtime initialization no
longer starts the analytical SDF OptiX pipeline. Default app features are unchanged.

The query kernel performs real RT-core traversal of the existing triangle GAS.
It returns distance, barycentrics and the caller's original triangle/instance
identity. It does not sample, approximate, replace or shade any material. Directions
are not normalized, preserving parametric distances. Invalid rays are explicitly
marked invalid, not reported as valid misses. Empty scenes overwrite previous hits
with misses. Geometry uploads reject malformed indices and non-finite vertices.

`trace_triangle_queries` consumes device-resident buffers on the renderer's CUDA
device/stream and synchronizes before returning, without downloading hit results.
It is an opt-in diagnostic prerequisite, not scheduled viewport work. Callers must
provide the identity mapping for the exact flattened scene generation. It currently
uses the existing mesh-upload API, whose flat material data is unused by queries.

## Preferred architecture: native Vulkan ray queries

The installed wgpu 22.1 / wgpu-hal 22 Vulkan backend exposes
`RAY_TRACING_ACCELERATION_STRUCTURE` and `RAY_QUERY`. Its HAL implements BLAS/TLAS
creation/builds, acceleration-structure bindings and compute ray queries. Public
wgpu 22 does not expose the complete API: wgpu-core 22.1
`binding_model.rs:347` and `device/resource.rs:1779` have `todo!()` for acceleration
structure bindings. Enabling two feature bits is therefore NOT an integration.

`build_native_rt_wgsl` accepts the canonical
`graphics::build_pathtrace_tri_wgsl()` source. It redirects only `pt_trace_scene`
and `pt_occluded`, preserving the rest of the source byte-for-byte, including live
material texture arrays, high-precision channels, normal/bump maps, roughness,
emission, authored lights, HDR and the existing integrator. Its native traversal
adds group 1 binding 0; group 0 must reuse `graphics::bgl::pt_bgl_entries()`.
Original instance indices become TLAS custom data, and primitive IDs map through
the canonical instance's `tri_off`. Native queries are opaque/two-sided, matching
the current software triangle traversal; material transmission stays in shading.

The exact installed Naga 22.1 SPIR-V writer has upstream-confirmed undefined
behavior on ray misses: it fetches intersection fields before checking the kind.
Do not run the native variant through that compiler. The isolated validation
compiler pins Naga 25.0.1, containing the upstream fix. It does not change the app's
wgpu/Naga dependencies or the workspace lockfile. Authored shaders remain WGSL;
the emitted SPIR-V file is a diagnostic compiler output, not a restored legacy
GLSL/SDF shader path.

References: https://github.com/gfx-rs/wgpu/issues/6761 and
https://github.com/gfx-rs/wgpu/pull/6752 (merged 2025-01-17).

Required parent-owned integration, not implemented or enabled here:

1. Request Vulkan AS/ray-query features at device creation in
   `app/src/gpu_init/mod.rs`. Existing devices cannot acquire those features later.
2. Own native BLAS/TLAS, pipeline, descriptors, scratch and deferred retirement in
   GpuSubsystem. Build/refit from canonical triangle scene generations, with
   correctly packed indices and AS-input/device-address-capable geometry buffers.
   `PtTriData` includes a material word and is not a packed Vulkan index stream.
3. Bind the actual live buffers and texture views through supported HAL access,
   not CPU material snapshots. Preserve canonical sampler descriptors. Public
   wgpu 22 has no sampler/bind-group HAL accessor; those native descriptors need
   explicit ownership. Material/light/environment edits must remain independently
   invalidated, without rebuilding unchanged geometry.
4. Record native builds and compute via `CommandEncoder::as_hal_mut` on the same
   device/queue. This avoids CUDA interop and all production CPU ray readbacks,
   but HAL access does not update wgpu's resource tracker. Define exact incoming
   and outgoing buffer/image states, AS barriers and submission lifetimes before
   activating it. An arbitrary raw callback is not a safe dispatch boundary.
5. Compile with a verified fixed compiler on the background pipeline worker, cap
   builds through FrameScheduler, latch the backend during interaction and
   invalidate accumulation correctly. Preserve depth/guides/postprocessing.
6. Validate native GPU hit/miss/instancing results and same-scene material images.
   Source preservation and pipeline creation alone are not rendered-image parity.

`validation/native_rt_pipeline.rs` is a standalone diagnostic using the real wgpu
device and canonical material BGL plus the native AS group. It creates the full
driver pipeline but intentionally does not dispatch it or alter viewport routing.

## Alternative architecture: OptiX wavefront integration

Keep the existing WGSL `pt_hit_material`, normal/bump/texture decoding, BRDF, light,
HDR environment and integration logic as the single shading implementation. Replace
only traversal with OptiX. The current WGSL integrator calls traversal synchronously
inside bounce and shadow loops, so this requires a real wavefront split: WGSL emits
rays, OptiX writes hits, WGSL shades them and emits continuation/shadow rays.

This larger alternative still lacks:

1. CUDA/wgpu external-memory allocation/import and external semaphore handoffs.
   None exists in the application. Match CUDA and graphics adapters by identity;
   do not assume device ordinal zero matches the wgpu adapter. Plain wgpu texture
   handles cannot be passed as CUDA pointers. No CPU material readback substitute.
2. GPU-shared bounded ray/hit/identity queues with generation-checked ownership.
   The temporary diagnostic synchronization must not become a viewport frame stall.
3. Wavefront continuation, shadow visibility and opacity/material-dependent hit
   rejection wired into the same WGSL shading path. Current queries are opaque,
   two-sided closest-hit queries, not an alpha-aware renderer.
4. Complete scene flattening/instancing identity mapping and invalidation. Geometry,
   material values, resident textures and environment/light changes need independent
   generation tracking. A geometry-only cache key cannot validate material parity.
5. FrameScheduler budgeting, interaction pipeline latch, accumulation invalidation,
   selection/depth/postprocessing preservation and same-scene image comparisons.

Do not restore `optix_programs_tri.cu` flat shading as a fallback. Its 32-byte
`HwRtMaterial` loses texture slots, UV/material mapping, high-precision channels,
normal/bump, authored light types and HDR lighting. The new query path deliberately
does not consume this representation for shading.

## Validation

Host used for the prerequisite: RTX 4090, driver 616.92, CUDA toolkit 12.8,
vendored OptiX 8.1 headers, MSVC 14.44. Queries are compiled without fast-math so
invalid-ray finite checks are retained. Query PTX is embedded in the Rust binary,
not loaded from a build-machine path at runtime.

Tests cover ABI sizes/offsets, invalid geometry, readiness remaining false, nearest
hit selection, barycentrics, original triangle/instance identity, misses, clipping,
backfaces, unnormalized directions, invalid rays, buffer count/device mismatch,
non-default-stream ordering and empty-scene clearing.

Parent-coordinated Cargo validation from `core_engine`:

```powershell
$env:MANIFOLD_CSG_LIB_DIR='F:/PROJECT/FourthLAyerAi/dev/manifold-prebuilt'
$env:MANIFOLD_CSG_LIB_KIND='static'
$env:CARGO_INCREMENTAL='0'
cargo build -p app
cargo test -p cuda_render
cargo test -p cuda_render --features optix -- --include-ignored --test-threads=1
cargo build -p app --features optix
cargo test -p graphics
```

Use an MSVC developer environment supported by the installed CUDA toolkit for the
optional build. No save format, scene transactions, UI or parent-owned GPU code
is changed here. `app/src/cuda_render_dispatch.rs` now rejects ineligible manual
CUDA dispatches and returns whether it actually handled the frame. Its caller
must restore the renderer and return only on `true`; on `false`, continue WGSL.
`cuda_viewport_eligible` supplies the same readiness predicate to the parent.

Validation artifacts on this host are in
`C:/Users/Proprio/AppData/Local/Temp/azra-optix-query-validation/`:

- `query-gpu-tests.log`: four tests passed, including actual RTX 4090 OptiX hits.
- `query-gpu-tests-all.log`: rebuilt optional CUDA+OptiX test harness; all six
  tests passed after adding the native shader/source-preservation checks.
- `native-rt-source-tests.log`: two hook/source-preservation tests passed.
- `native-rt-shader-compile.log`: the full canonical material shader plus native
  traversal parsed/validated/compiled with Naga 25.0.1 (185872-byte SPIR-V).
- `native-rt-pipeline.log`: RTX 4090 exposes both required Vulkan features, but
  full-shader driver pipeline creation did NOT complete. The diagnostic was
  stopped after more than two minutes, with host working set observed at
  8622546944 bytes, to protect concurrent parent validation. No native ray-query
  dispatch or rendered-image parity is validated. Cold compilation cost is an
  additional readiness blocker to investigate before enabling this backend.
- `app-build.log`: default `cargo build -p app` passed after the coherent changes.

The native compiler diagnostic has its own Cargo workspace at
`validation/rt_shader_compiler/Cargo.toml`. Keep its `CARGO_TARGET_DIR` outside the
main workspace target to avoid contending with parent validation. Export the
canonical shader with `validation/export_triangle_shader.rs`, then pass its path
and a diagnostic output path to the compiler. The app routing test is CUDA-feature
gated and has not been run by the standalone source tests.

## Files owned by this task

All paths below are relative to `F:/PROJECT/FourthLAyerAi/`:

- `core_engine/app/src/cuda_render_dispatch.rs`
- `core_engine/cuda_render/build.rs`
- `core_engine/cuda_render/src/lib.rs`
- `core_engine/cuda_render/src/dispatch.rs`
- `core_engine/cuda_render/src/optix_accel.rs`
- `core_engine/cuda_render/src/optix_tri_pipeline.rs`
- `core_engine/cuda_render/src/triangle_query.rs`
- `core_engine/cuda_render/src/triangle_query_gpu_tests.rs`
- `core_engine/cuda_render/kernels/optix_query_tri.cu`
- `core_engine/cuda_render/src/native_rt_shader.rs`
- `core_engine/cuda_render/src/native_rt_traversal.wgsl`
- `core_engine/cuda_render/validation/export_triangle_shader.rs`
- `core_engine/cuda_render/validation/native_rt_pipeline.rs`
- `core_engine/cuda_render/validation/rt_shader_compiler/Cargo.toml`
- `core_engine/cuda_render/validation/rt_shader_compiler/Cargo.lock`
- `core_engine/cuda_render/validation/rt_shader_compiler/src/main.rs`
- `core_engine/cuda_render/RTX_PARITY.md`
