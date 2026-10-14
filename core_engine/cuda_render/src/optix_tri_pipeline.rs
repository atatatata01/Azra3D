//! Material-independent OptiX triangle queries for a future WGSL wavefront integrator.

use crate::optix_accel::{dev_ptr, OptixTriangleAccel};
use crate::optix_sys::*;
use crate::{TriangleHit, TriangleIdentity, TriangleRay};
use cudarc::driver::{CudaDevice, CudaSlice, DeviceSlice};
use std::ffi::c_void;
use std::sync::Arc;

const SBT_RECORD_SIZE: usize = 32;

/// Runtime ABI matching TriangleQueryParams in optix_query_tri.cu.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LaunchParamsTri {
    rays: u64,
    hits: u64,
    identities: u64,
    traversable: u64,
    ray_count: u32,
    reserved: u32,
}

const _: () = {
    assert!(std::mem::size_of::<LaunchParamsTri>() == 40);
    assert!(std::mem::offset_of!(LaunchParamsTri, traversable) == 24);
    assert!(std::mem::offset_of!(LaunchParamsTri, ray_count) == 32);
};

/// Triangle query OptiX pipeline + SBT.
pub struct OptixTriPipeline {
    ctx: *mut c_void,
    pipeline: *mut c_void,
    _raygen_pg: *mut c_void,
    _miss_pg: *mut c_void,
    _hitgroup_pg: *mut c_void,
    sbt_buf: CudaSlice<u8>,
    params_buf: CudaSlice<u8>,
}

impl OptixTriPipeline {
    /// Initialize only built-in triangle traversal; no material or SDF programs.
    pub fn try_new(device: &Arc<CudaDevice>) -> Result<Self, String> {
        device.bind_to_thread().map_err(|error| format!("bind CUDA context: {error}"))?;
        ensure_nvoptix_search_path();

        let rc = unsafe { fl_optix_init() };
        if rc != 0 {
            return Err(format!("optixInit failed: error {}", rc));
        }

        let mut ctx: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { fl_optix_context_create(std::ptr::null_mut(), &mut ctx) };
        if rc != 0 {
            return Err(format!("optixDeviceContextCreate failed: error {}", rc));
        }

        let mut ptx_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/optix_query_tri.ptx")).to_vec();
        if ptx_bytes.last() != Some(&0) {
            ptx_bytes.push(0);
        }

        let mut pipeline: *mut c_void = std::ptr::null_mut();
        let mut raygen_pg: *mut c_void = std::ptr::null_mut();
        let mut miss_pg: *mut c_void = std::ptr::null_mut();
        let mut hitgroup_pg: *mut c_void = std::ptr::null_mut();

        let rc = unsafe {
            fl_optix_create_triangle_module_and_pipeline(
                ctx,
                ptx_bytes.as_ptr(),
                ptx_bytes.len() as u32,
                4,
                2, // triangle barycentric attributes (built-in)
                b"params\0".as_ptr(),
                b"__raygen__triangle_query\0".as_ptr(),
                b"__miss__triangle_query\0".as_ptr(),
                b"__closesthit__triangle_query\0".as_ptr(),
                1,
                &mut pipeline,
                &mut raygen_pg,
                &mut miss_pg,
                &mut hitgroup_pg,
            )
        };
        if rc != 0 {
            unsafe { fl_optix_context_destroy(ctx) };
            return Err(format!(
                "OptiX triangle module/pipeline creation failed: error {}",
                rc
            ));
        }

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
            return Err("Failed to pack triangle SBT headers".into());
        }

        let mut sbt_buf = device
            .alloc_zeros::<u8>(sbt_total)
            .map_err(|e| format!("alloc sbt: {}", e))?;
        device
            .htod_copy_into(sbt_host, &mut sbt_buf)
            .map_err(|e| format!("upload sbt: {}", e))?;

        let params_size = std::mem::size_of::<LaunchParamsTri>();
        let params_buf = device
            .alloc_zeros::<u8>(params_size)
            .map_err(|e| format!("alloc launch params: {}", e))?;

        eprintln!(
            "[cuda_render] OptiX triangle query pipeline ready (SBT {}B, params {}B); material shading not integrated",
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

    pub fn ctx_handle(&self) -> *mut c_void {
        self.ctx
    }

    /// Trace a bounded batch of device-resident rays without shading or CPU readback.
    pub fn launch_queries(
        &mut self,
        device: &Arc<CudaDevice>,
        accel: &OptixTriangleAccel,
        rays: &CudaSlice<TriangleRay>,
        hits: &mut CudaSlice<TriangleHit>,
        identities: &CudaSlice<TriangleIdentity>,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(device, &rays.device())
            || !Arc::ptr_eq(device, &hits.device())
            || !Arc::ptr_eq(device, &identities.device())
            || !Arc::ptr_eq(device, &self.params_buf.device())
        {
            return Err("triangle query buffers must share the pipeline CUDA device and stream".into());
        }
        device.bind_to_thread().map_err(|error| format!("bind CUDA context: {error}"))?;
        if hits.len() != rays.len() {
            return Err("triangle query ray and hit counts differ".into());
        }
        if identities.len() != accel.num_triangles() as usize {
            return Err("triangle query identities do not match the GAS".into());
        }
        let ray_count = u32::try_from(rays.len())
            .map_err(|_| "triangle query batch exceeds u32::MAX")?;
        if ray_count == 0 {
            return Ok(());
        }

        let params = LaunchParamsTri {
            rays: dev_ptr(rays),
            hits: dev_ptr(hits),
            identities: dev_ptr(identities),
            traversable: accel.traversable(),
            ray_count,
            reserved: 0,
        };

        let params_bytes = bytemuck::bytes_of(&params).to_vec();
        device
            .htod_copy_into(params_bytes, &mut self.params_buf)
            .map_err(|e| format!("upload tri launch params: {}", e))?;

        let sbt_ptr = dev_ptr(&self.sbt_buf);
        let params_ptr = dev_ptr(&self.params_buf);
        let params_size = std::mem::size_of::<LaunchParamsTri>() as u64;

        let rc = unsafe {
            fl_optix_launch(
                self.pipeline,
                (*device.cu_stream()).cast(),
                params_ptr,
                params_size,
                sbt_ptr,
                SBT_RECORD_SIZE as u32,
                sbt_ptr + SBT_RECORD_SIZE as u64,
                SBT_RECORD_SIZE as u32,
                1,
                sbt_ptr + (SBT_RECORD_SIZE * 2) as u64,
                SBT_RECORD_SIZE as u32,
                1,
                ray_count,
                1,
            )
        };
        if rc != 0 {
            return Err(format!("optixLaunch triangle query failed: error {}", rc));
        }

        device
            .synchronize()
            .map_err(|e| format!("sync after triangle query: {}", e))?;
        Ok(())
    }
}

impl Drop for OptixTriPipeline {
    fn drop(&mut self) {
        unsafe {
            fl_optix_context_destroy(self.ctx);
        }
    }
}

/// Ensure Windows can find `nvoptix.dll` (often only under DriverStore).
fn ensure_nvoptix_search_path() {
    #[cfg(windows)]
    {
        use std::path::PathBuf;
        // Already loadable?
        let ok = unsafe {
            let h = windows_load_library("nvoptix.dll");
            if !h.is_null() {
                windows_free_library(h);
                true
            } else {
                false
            }
        };
        if ok {
            return;
        }

        // Scan DriverStore for nvoptix.dll and prepend its directory to PATH.
        let repo = PathBuf::from(r"C:\Windows\System32\DriverStore\FileRepository");
        if let Ok(entries) = std::fs::read_dir(&repo) {
            for entry in entries.flatten() {
                let p = entry.path().join("nvoptix.dll");
                if p.is_file() {
                    if let Some(dir) = p.parent() {
                        let dir_s = dir.to_string_lossy();
                        let path = std::env::var_os("PATH").unwrap_or_default();
                        let mut new_path = std::ffi::OsString::from(dir_s.as_ref());
                        new_path.push(";");
                        new_path.push(&path);
                        std::env::set_var("PATH", new_path);
                        eprintln!("[cuda_render] Added nvoptix.dll path: {}", dir_s);
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
fn windows_load_library(name: &str) -> *mut c_void {
    use std::ffi::CString;
    extern "system" {
        fn LoadLibraryA(name: *const i8) -> *mut c_void;
    }
    let c = CString::new(name).unwrap_or_default();
    unsafe { LoadLibraryA(c.as_ptr()) }
}

#[cfg(windows)]
fn windows_free_library(h: *mut c_void) {
    extern "system" {
        fn FreeLibrary(h: *mut c_void) -> i32;
    }
    unsafe {
        FreeLibrary(h);
    }
}
