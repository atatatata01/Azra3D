//! Minimal FFI declarations for OptiX C wrapper (optix_host.c).
//!
//! These functions are compiled from optix_host.c by build.rs when
//! the `optix` feature is enabled and the OptiX SDK is found.

#![allow(dead_code)]

use std::ffi::c_void;

/// CUDA device pointer (u64 address).
pub type CUdeviceptr = u64;
/// Opaque CUDA context handle.
pub type CUcontext = *mut c_void;
/// Opaque CUDA stream handle.
pub type CUstream = *mut c_void;

extern "C" {
    /// Initialize OptiX runtime (loads nvoptix.dll / libnvoptix.so).
    pub fn fl_optix_init() -> i32;

    /// Create an OptiX device context from a CUDA context.
    /// Pass null for cu_ctx to use the current CUDA context.
    pub fn fl_optix_context_create(cu_ctx: CUcontext, out_ctx: *mut *mut c_void) -> i32;

    /// Destroy an OptiX device context.
    pub fn fl_optix_context_destroy(ctx: *mut c_void);

    /// Create OptiX module from PTX, program groups, and linked pipeline — all in one call.
    ///
    /// Creates: raygen, miss, hitgroup (closesthit + custom intersection) program groups,
    /// plus the linked pipeline. Returns 0 on success, OptiX error code otherwise.
    pub fn fl_optix_create_module_and_pipeline(
        ctx: *mut c_void,
        ptx_src: *const u8,
        ptx_size: u32,
        num_payload_values: i32,
        num_attribute_values: i32,
        launch_params_name: *const u8,
        raygen_name: *const u8,
        miss_name: *const u8,
        closesthit_name: *const u8,
        intersection_name: *const u8,
        max_trace_depth: u32,
        out_pipeline: *mut *mut c_void,
        out_raygen_pg: *mut *mut c_void,
        out_miss_pg: *mut *mut c_void,
        out_hitgroup_pg: *mut *mut c_void,
    ) -> i32;

    /// Query GAS buffer sizes for a given number of AABB primitives.
    pub fn fl_optix_accel_buffer_sizes(
        ctx: *mut c_void,
        num_prims: u32,
        out_temp_size: *mut u64,
        out_output_size: *mut u64,
    ) -> i32;

    /// Build GAS from device-side AABB array.
    pub fn fl_optix_build_gas(
        ctx: *mut c_void,
        stream: CUstream,
        aabbs_device: CUdeviceptr,
        num_prims: u32,
        temp_buf: CUdeviceptr,
        temp_size: u64,
        output_buf: CUdeviceptr,
        output_size: u64,
        out_handle: *mut u64,
    ) -> i32;

    /// Query triangle GAS buffer sizes.
    pub fn fl_optix_tri_accel_buffer_sizes(
        ctx: *mut c_void,
        num_vertices: u32,
        num_triangles: u32,
        out_temp_size: *mut u64,
        out_output_size: *mut u64,
    ) -> i32;

    /// Build triangle GAS (hardware RT core triangle intersection).
    pub fn fl_optix_build_triangle_gas(
        ctx: *mut c_void,
        stream: CUstream,
        vertices_device: CUdeviceptr,
        num_vertices: u32,
        indices_device: CUdeviceptr,
        num_triangles: u32,
        temp_buf: CUdeviceptr,
        temp_size: u64,
        output_buf: CUdeviceptr,
        output_size: u64,
        out_handle: *mut u64,
    ) -> i32;

    /// Create triangle path-trace pipeline (built-in triangle IS, no custom intersection).
    pub fn fl_optix_create_triangle_module_and_pipeline(
        ctx: *mut c_void,
        ptx_src: *const u8,
        ptx_size: u32,
        num_payload_values: i32,
        num_attribute_values: i32,
        launch_params_name: *const u8,
        raygen_name: *const u8,
        miss_name: *const u8,
        closesthit_name: *const u8,
        max_trace_depth: u32,
        out_pipeline: *mut *mut c_void,
        out_raygen_pg: *mut *mut c_void,
        out_miss_pg: *mut *mut c_void,
        out_hitgroup_pg: *mut *mut c_void,
    ) -> i32;

    /// Launch the OptiX pipeline with SBT records.
    pub fn fl_optix_launch(
        pipeline: *mut c_void,
        stream: CUstream,
        launch_params: CUdeviceptr,
        params_size: u64,
        raygen_sbt: CUdeviceptr,
        raygen_sbt_size: u32,
        miss_sbt: CUdeviceptr,
        miss_stride: u32,
        miss_count: u32,
        hitgroup_sbt: CUdeviceptr,
        hitgroup_stride: u32,
        hitgroup_count: u32,
        width: u32,
        height: u32,
    ) -> i32;

    /// Get OPTIX_SBT_RECORD_HEADER_SIZE (32 bytes).
    pub fn fl_optix_sbt_header_size() -> u32;

    /// Pack an SBT record header for a program group.
    pub fn fl_optix_pack_sbt_header(program_group: *mut c_void, out_header: *mut u8) -> i32;
}
