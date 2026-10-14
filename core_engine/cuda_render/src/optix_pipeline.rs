//! OptiX pipeline: context creation, module compilation, SBT setup, and launch.
//!
//! Wraps the C shim functions from optix_host.c to provide a safe Rust interface.
//! Initialized once at startup; the GAS is rebuilt per-frame by OptixAccel.

use crate::buffers::CudaBufferSet;
use crate::optix_accel::dev_ptr;
use crate::optix_sys::*;
use cudarc::driver::{CudaDevice, CudaSlice};
use std::ffi::c_void;
use std::sync::Arc;

/// SBT record size = OPTIX_SBT_RECORD_HEADER_SIZE (32 bytes, no per-record data).
const SBT_RECORD_SIZE: usize = 32;

/// Launch parameters matching the CUDA `OptixLaunchParams` struct in optix_programs.cu.
///
/// Layout: 5 device pointers (8B each) + 2 u32 + 1 u64 = 56 bytes.
#[repr(C)]
struct LaunchParams {
    cfg: u64,           // CUdeviceptr → GpuUniforms
    prim_data: u64,     // CUdeviceptr → prim f32 buffer
    gbuf_normal: u64,   // CUdeviceptr → float4 per pixel
    gbuf_material: u64, // CUdeviceptr → float4 per pixel
    gbuf_albedo: u64,   // CUdeviceptr → uchar4 per pixel
    width: u32,
    height: u32,
    traversable: u64,   // OptixTraversableHandle
}

/// Owns the OptiX device context, compiled pipeline, program groups, and SBT.
pub struct OptixPipeline {
    ctx: *mut c_void,
    pipeline: *mut c_void,
    _raygen_pg: *mut c_void,
    _miss_pg: *mut c_void,
    _hitgroup_pg: *mut c_void,
    /// SBT records on device (raygen + miss + hitgroup = 3 × 32 bytes).
    sbt_buf: CudaSlice<u8>,
    /// Launch params struct on device (56 bytes).
    params_buf: CudaSlice<u8>,
}

impl OptixPipeline {
    /// Initialize OptiX: load runtime, create context, compile module,
    /// build pipeline, pack SBT. Returns None if OptiX is unavailable.
    pub fn try_new(device: &Arc<CudaDevice>) -> Result<Self, String> {
        // 1. Initialize OptiX runtime (loads nvoptix.dll / libnvoptix.so)
        let rc = unsafe { fl_optix_init() };
        if rc != 0 {
            return Err(format!("optixInit failed: error {}", rc));
        }

        // 2. Create OptiX context from current CUDA context
        let mut ctx: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { fl_optix_context_create(std::ptr::null_mut(), &mut ctx) };
        if rc != 0 {
            return Err(format!("optixDeviceContextCreate failed: error {}", rc));
        }

        // 3. Load OptiX PTX (compiled from optix_programs.cu by build.rs)
        let out_dir = env!("OUT_DIR", "OUT_DIR not set — run via cargo build");
        let ptx_path = format!("{}/optix_programs.ptx", out_dir);
        let ptx_text = std::fs::read_to_string(&ptx_path)
            .map_err(|e| format!("read optix_programs.ptx: {}", e))?;

        // PTX must be null-terminated for OptiX
        let mut ptx_bytes = ptx_text.into_bytes();
        if ptx_bytes.last() != Some(&0) {
            ptx_bytes.push(0);
        }

        // 4. Create module + program groups + pipeline (all-in-one C call)
        let mut pipeline: *mut c_void = std::ptr::null_mut();
        let mut raygen_pg: *mut c_void = std::ptr::null_mut();
        let mut miss_pg: *mut c_void = std::ptr::null_mut();
        let mut hitgroup_pg: *mut c_void = std::ptr::null_mut();

        let rc = unsafe {
            fl_optix_create_module_and_pipeline(
                ctx,
                ptx_bytes.as_ptr(),
                ptx_bytes.len() as u32,
                8, // num_payload_values (hit pos[3] + t + normal[3] + prim_idx)
                1, // num_attribute_values (prim_idx from reportIntersection)
                b"params\0".as_ptr(),
                b"__raygen__primary\0".as_ptr(),
                b"__miss__background\0".as_ptr(),
                b"__closesthit__sdf\0".as_ptr(),
                b"__intersection__sdf\0".as_ptr(),
                1, // max_trace_depth
                &mut pipeline,
                &mut raygen_pg,
                &mut miss_pg,
                &mut hitgroup_pg,
            )
        };
        if rc != 0 {
            unsafe { fl_optix_context_destroy(ctx) };
            return Err(format!("OptiX module/pipeline creation failed: error {}", rc));
        }

        // 5. Pack SBT records (header only, no per-record data)
        let sbt_total = SBT_RECORD_SIZE * 3;
        let mut sbt_host = vec![0u8; sbt_total];

        let rc1 = unsafe { fl_optix_pack_sbt_header(raygen_pg, sbt_host.as_mut_ptr()) };
        let rc2 = unsafe {
            fl_optix_pack_sbt_header(miss_pg, sbt_host[SBT_RECORD_SIZE..].as_mut_ptr())
        };
        let rc3 = unsafe {
            fl_optix_pack_sbt_header(hitgroup_pg, sbt_host[SBT_RECORD_SIZE * 2..].as_mut_ptr())
        };
        if rc1 != 0 || rc2 != 0 || rc3 != 0 {
            unsafe { fl_optix_context_destroy(ctx) };
            return Err("Failed to pack SBT headers".to_string());
        }

        // Upload SBT to device
        let mut sbt_buf = device
            .alloc_zeros::<u8>(sbt_total)
            .map_err(|e| format!("alloc sbt: {}", e))?;
        device
            .htod_copy_into(sbt_host, &mut sbt_buf)
            .map_err(|e| format!("upload sbt: {}", e))?;

        // 6. Allocate device-side launch params buffer
        let params_size = std::mem::size_of::<LaunchParams>();
        let params_buf = device
            .alloc_zeros::<u8>(params_size)
            .map_err(|e| format!("alloc launch params: {}", e))?;

        eprintln!(
            "[cuda_render] OptiX pipeline initialized (SBT {}B, params {}B)",
            sbt_total, params_size
        );

        Ok(Self {
            ctx,
            pipeline,
            _raygen_pg: raygen_pg,
            _miss_pg: miss_pg,
            _hitgroup_pg: hitgroup_pg,
            sbt_buf,
            params_buf,
        })
    }

    /// Raw OptiX device context handle (for GAS building).
    pub fn ctx_handle(&self) -> *mut c_void {
        self.ctx
    }

    /// Launch the OptiX pipeline to fill the G-buffer.
    ///
    /// The raygen program generates camera rays, RT cores traverse the BVH,
    /// custom intersection does per-prim SDF sphere tracing, and the
    /// closest-hit program writes the G-buffer (same format as raymarch_gbuf).
    pub fn launch(
        &mut self,
        device: &Arc<CudaDevice>,
        bufs: &CudaBufferSet,
        traversable: u64,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if traversable == 0 {
            return Ok(()); // No geometry to render
        }

        // Fill launch params with device pointers to scene/buffer data
        let params = LaunchParams {
            cfg: dev_ptr(&bufs.uniform_buf),
            prim_data: dev_ptr(&bufs.prim_buf),
            gbuf_normal: dev_ptr(&bufs.gbuf_normal),
            gbuf_material: dev_ptr(&bufs.gbuf_material),
            gbuf_albedo: dev_ptr(&bufs.gbuf_albedo),
            width,
            height,
            traversable,
        };

        // Upload params to device
        let params_bytes: Vec<u8> = unsafe {
            std::slice::from_raw_parts(
                &params as *const LaunchParams as *const u8,
                std::mem::size_of::<LaunchParams>(),
            )
            .to_vec()
        };
        device
            .htod_copy_into(params_bytes, &mut self.params_buf)
            .map_err(|e| format!("upload launch params: {}", e))?;

        // Build SBT descriptor from device pointers
        let sbt_ptr = dev_ptr(&self.sbt_buf);
        let params_ptr = dev_ptr(&self.params_buf);
        let params_size = std::mem::size_of::<LaunchParams>() as u64;

        let rc = unsafe {
            fl_optix_launch(
                self.pipeline,
                std::ptr::null_mut(), // default stream
                params_ptr,
                params_size,
                // raygen SBT record
                sbt_ptr,
                SBT_RECORD_SIZE as u32,
                // miss SBT record
                sbt_ptr + SBT_RECORD_SIZE as u64,
                SBT_RECORD_SIZE as u32,
                1, // miss count
                // hitgroup SBT record
                sbt_ptr + (SBT_RECORD_SIZE * 2) as u64,
                SBT_RECORD_SIZE as u32,
                1, // hitgroup count
                // launch dimensions
                width,
                height,
            )
        };
        if rc != 0 {
            return Err(format!("optixLaunch failed: error {}", rc));
        }

        // Sync to ensure G-buffer is written before subsequent passes
        device
            .synchronize()
            .map_err(|e| format!("sync after OptiX launch: {}", e))?;

        Ok(())
    }
}

impl Drop for OptixPipeline {
    fn drop(&mut self) {
        // Destroying the OptiX context cleans up the pipeline and program groups
        unsafe {
            fl_optix_context_destroy(self.ctx);
        }
    }
}
