//! OptiX acceleration structure (GAS) management.
//!
//! Builds a BVH over per-primitive AABBs for hardware RT traversal.
//! Each SDF primitive gets one AABB derived from its position + bound_r.
//! Mirror-reflected copies expand the AABB to cover both sides.

use crate::optix_sys::*;
use core_math::FLOATS_PER_PRIM;
use cudarc::driver::{CudaDevice, CudaSlice};
use std::ffi::c_void;
use std::sync::Arc;

/// Get the raw CUdeviceptr (u64 device address) from a CudaSlice.
///
/// Uses cudarc's DeviceRepr implementation which returns a pointer
/// to the internal CUdeviceptr field.
#[inline]
pub(crate) fn dev_ptr<T: cudarc::driver::DeviceRepr>(slice: &CudaSlice<T>) -> u64 {
    use cudarc::driver::DeviceRepr;
    unsafe {
        let p = <&CudaSlice<T> as DeviceRepr>::as_kernel_param(&slice);
        *(p as *const u64)
    }
}

/// Manages the OptiX Geometry Acceleration Structure (GAS).
///
/// Holds device-side AABB buffer and GAS build/output buffers.
/// Rebuilt each frame from CPU-side primitive data.
pub struct OptixAccel {
    aabb_buf: CudaSlice<f32>,
    gas_temp: CudaSlice<u8>,
    gas_output: CudaSlice<u8>,
    traversable: u64,
    allocated_prims: u32,
    temp_size: u64,
    output_size: u64,
}

impl OptixAccel {
    /// Create with initial capacity for `max_prims` primitives.
    pub fn new(device: &Arc<CudaDevice>, max_prims: u32) -> Result<Self, String> {
        let aabb_buf = device
            .alloc_zeros::<f32>((max_prims as usize) * 6)
            .map_err(|e| format!("alloc aabb: {}", e))?;
        let gas_temp = device
            .alloc_zeros::<u8>(256)
            .map_err(|e| format!("alloc gas_temp: {}", e))?;
        let gas_output = device
            .alloc_zeros::<u8>(256)
            .map_err(|e| format!("alloc gas_output: {}", e))?;

        Ok(Self {
            aabb_buf,
            gas_temp,
            gas_output,
            traversable: 0,
            allocated_prims: max_prims,
            temp_size: 256,
            output_size: 256,
        })
    }

    /// Current traversable handle for use in optixTrace launch params.
    pub fn traversable(&self) -> u64 {
        self.traversable
    }

    /// Build (or rebuild) the GAS from CPU-side primitive data.
    ///
    /// Generates AABBs on the CPU, uploads them, then invokes the
    /// OptiX BVH builder on the GPU.
    pub fn build(
        &mut self,
        device: &Arc<CudaDevice>,
        optix_ctx: *mut c_void,
        prim_floats: &[f32],
    ) -> Result<(), String> {
        let num_prims = (prim_floats.len() / FLOATS_PER_PRIM) as u32;
        if num_prims == 0 {
            self.traversable = 0;
            return Ok(());
        }

        // ---- Generate AABBs on CPU ----
        let aabbs = Self::generate_aabbs(prim_floats, num_prims);

        // ---- Grow AABB buffer if needed ----
        if num_prims > self.allocated_prims {
            self.aabb_buf = device
                .alloc_zeros::<f32>((num_prims as usize) * 6)
                .map_err(|e| format!("grow aabb: {}", e))?;
            self.allocated_prims = num_prims;
        }

        // ---- Upload AABBs to device ----
        device
            .htod_copy_into(aabbs, &mut self.aabb_buf)
            .map_err(|e| format!("upload aabb: {}", e))?;

        // ---- Query required buffer sizes ----
        let mut temp_size: u64 = 0;
        let mut output_size: u64 = 0;
        let rc = unsafe {
            fl_optix_accel_buffer_sizes(optix_ctx, num_prims, &mut temp_size, &mut output_size)
        };
        if rc != 0 {
            return Err(format!("optixAccelComputeMemoryUsage failed: {}", rc));
        }

        // ---- Reallocate build buffers if sizes grew ----
        if temp_size > self.temp_size {
            self.gas_temp = device
                .alloc_zeros::<u8>(temp_size as usize)
                .map_err(|e| format!("grow gas_temp: {}", e))?;
            self.temp_size = temp_size;
        }
        if output_size > self.output_size {
            self.gas_output = device
                .alloc_zeros::<u8>(output_size as usize)
                .map_err(|e| format!("grow gas_output: {}", e))?;
            self.output_size = output_size;
        }

        // ---- Build GAS ----
        let aabb_ptr = dev_ptr(&self.aabb_buf);
        let temp_ptr = dev_ptr(&self.gas_temp);
        let output_ptr = dev_ptr(&self.gas_output);

        let mut handle: u64 = 0;
        let rc = unsafe {
            fl_optix_build_gas(
                optix_ctx,
                std::ptr::null_mut(), // default stream
                aabb_ptr,
                num_prims,
                temp_ptr,
                temp_size,
                output_ptr,
                output_size,
                &mut handle,
            )
        };
        if rc != 0 {
            return Err(format!("optixAccelBuild failed: {}", rc));
        }

        self.traversable = handle;

        // Sync to ensure GAS build completes before any launch
        device
            .synchronize()
            .map_err(|e| format!("sync after GAS build: {}", e))?;

        Ok(())
    }

    /// Generate flat AABB array (6 floats per primitive) from prim data.
    ///
    /// Each AABB = [min_x, min_y, min_z, max_x, max_y, max_z].
    /// Mirror flags expand the AABB to cover reflected copies.
    fn generate_aabbs(prim_floats: &[f32], num_prims: u32) -> Vec<f32> {
        let mut aabbs = Vec::with_capacity((num_prims as usize) * 6);

        for i in 0..num_prims as usize {
            let base = i * FLOATS_PER_PRIM;
            let px = prim_floats[base];
            let py = prim_floats[base + 1];
            let pz = prim_floats[base + 2];
            let bound_r = prim_floats[base + 45];

            let mut min_x = px - bound_r;
            let mut min_y = py - bound_r;
            let mut min_z = pz - bound_r;
            let mut max_x = px + bound_r;
            let mut max_y = py + bound_r;
            let mut max_z = pz + bound_r;

            // Mirror handling: expand AABB to cover reflected copies
            let mirror_bits = prim_floats[base + 61].to_bits();
            if mirror_bits != 0 {
                let mc_x = prim_floats[base + 78];
                let mc_y = prim_floats[base + 79];
                let mc_z = prim_floats[base + 80];

                if mirror_bits & 1 != 0 {
                    let rx = 2.0 * mc_x - px;
                    min_x = min_x.min(rx - bound_r);
                    max_x = max_x.max(rx + bound_r);
                }
                if mirror_bits & 2 != 0 {
                    let ry = 2.0 * mc_y - py;
                    min_y = min_y.min(ry - bound_r);
                    max_y = max_y.max(ry + bound_r);
                }
                if mirror_bits & 4 != 0 {
                    let rz = 2.0 * mc_z - pz;
                    min_z = min_z.min(rz - bound_r);
                    max_z = max_z.max(rz + bound_r);
                }
            }

            aabbs.extend_from_slice(&[min_x, min_y, min_z, max_x, max_y, max_z]);
        }

        aabbs
    }
}

// ============================================================
// Triangle GAS — hardware RT core traversal for path tracing
// ============================================================

/// Packed material for OptiX triangle path tracer (32 B, matches device HwRtMaterial).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HwRtMaterial {
    pub albedo: [f32; 3],
    pub roughness: f32,
    pub emission: [f32; 3],
    pub metallic: f32,
}

// Safety: plain POD floats, valid as all-zero, matches device layout.
unsafe impl cudarc::driver::DeviceRepr for HwRtMaterial {}
unsafe impl cudarc::driver::ValidAsZeroBits for HwRtMaterial {}

impl Default for HwRtMaterial {
    fn default() -> Self {
        Self {
            albedo: [0.66, 0.585, 0.53],
            roughness: 0.65,
            emission: [0.0; 3],
            metallic: 0.0,
        }
    }
}

/// Manages an OptiX triangle GAS built from world-space mesh data.
///
/// Vertices/indices live on device; rebuild when the PT scene generation changes.
pub struct OptixTriangleAccel {
    pos_buf: CudaSlice<f32>,
    nrm_buf: CudaSlice<f32>,
    idx_buf: CudaSlice<u32>,
    tri_mat_buf: CudaSlice<u32>,
    mat_buf: CudaSlice<HwRtMaterial>,
    gas_temp: CudaSlice<u8>,
    gas_output: CudaSlice<u8>,
    traversable: u64,
    num_vertices: u32,
    num_triangles: u32,
    num_materials: u32,
    temp_size: u64,
    output_size: u64,
    pos_cap: usize,
    nrm_cap: usize,
    idx_cap: usize,
    tri_mat_cap: usize,
    mat_cap: usize,
}

impl OptixTriangleAccel {
    pub fn new(device: &Arc<CudaDevice>) -> Result<Self, String> {
        Ok(Self {
            pos_buf: device.alloc_zeros::<f32>(3).map_err(|e| format!("alloc pos: {}", e))?,
            nrm_buf: device.alloc_zeros::<f32>(3).map_err(|e| format!("alloc nrm: {}", e))?,
            idx_buf: device.alloc_zeros::<u32>(3).map_err(|e| format!("alloc idx: {}", e))?,
            tri_mat_buf: device.alloc_zeros::<u32>(1).map_err(|e| format!("alloc tri_mat: {}", e))?,
            mat_buf: device
                .alloc_zeros::<HwRtMaterial>(1)
                .map_err(|e| format!("alloc mat: {}", e))?,
            gas_temp: device.alloc_zeros::<u8>(256).map_err(|e| format!("alloc gas_temp: {}", e))?,
            gas_output: device
                .alloc_zeros::<u8>(256)
                .map_err(|e| format!("alloc gas_output: {}", e))?,
            traversable: 0,
            num_vertices: 0,
            num_triangles: 0,
            num_materials: 0,
            temp_size: 256,
            output_size: 256,
            pos_cap: 3,
            nrm_cap: 3,
            idx_cap: 3,
            tri_mat_cap: 1,
            mat_cap: 1,
        })
    }

    pub fn traversable(&self) -> u64 {
        self.traversable
    }

    pub fn num_triangles(&self) -> u32 {
        self.num_triangles
    }

    pub fn num_materials(&self) -> u32 {
        self.num_materials
    }

    pub fn pos_ptr(&self) -> u64 {
        dev_ptr(&self.pos_buf)
    }
    pub fn nrm_ptr(&self) -> u64 {
        dev_ptr(&self.nrm_buf)
    }
    pub fn idx_ptr(&self) -> u64 {
        dev_ptr(&self.idx_buf)
    }
    pub fn tri_mat_ptr(&self) -> u64 {
        dev_ptr(&self.tri_mat_buf)
    }
    pub fn mat_ptr(&self) -> u64 {
        dev_ptr(&self.mat_buf)
    }

    /// Upload world-space triangle mesh and build a hardware triangle GAS.
    ///
    /// `positions` / `normals`: length = num_vertices, xyz each.
    /// `indices`: length = num_triangles * 3.
    /// `tri_materials`: length = num_triangles (index into `materials`).
    pub fn build(
        &mut self,
        device: &Arc<CudaDevice>,
        optix_ctx: *mut c_void,
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        indices: &[u32],
        tri_materials: &[u32],
        materials: &[HwRtMaterial],
    ) -> Result<(), String> {
        self.traversable = 0;
        self.num_triangles = 0;
        crate::triangle_query::validate_triangle_scene(
            positions, normals, indices, tri_materials, materials.len(),
        )?;
        let num_vertices = positions.len() as u32;
        let num_triangles = (indices.len() / 3) as u32;
        if num_vertices == 0 || num_triangles == 0 {
            self.traversable = 0;
            self.num_vertices = 0;
            self.num_triangles = 0;
            return Ok(());
        }

        // Flatten positions/normals to f32 arrays
        let mut pos_flat = Vec::with_capacity(positions.len() * 3);
        let mut nrm_flat = Vec::with_capacity(normals.len() * 3);
        for p in positions {
            pos_flat.extend_from_slice(p);
        }
        for n in normals {
            nrm_flat.extend_from_slice(n);
        }

        // Grow buffers
        if pos_flat.len() > self.pos_cap {
            self.pos_buf = device
                .alloc_zeros::<f32>(pos_flat.len())
                .map_err(|e| format!("grow pos: {}", e))?;
            self.pos_cap = pos_flat.len();
        }
        if nrm_flat.len() > self.nrm_cap {
            self.nrm_buf = device
                .alloc_zeros::<f32>(nrm_flat.len())
                .map_err(|e| format!("grow nrm: {}", e))?;
            self.nrm_cap = nrm_flat.len();
        }
        if indices.len() > self.idx_cap {
            self.idx_buf = device
                .alloc_zeros::<u32>(indices.len())
                .map_err(|e| format!("grow idx: {}", e))?;
            self.idx_cap = indices.len();
        }
        if tri_materials.len() > self.tri_mat_cap {
            self.tri_mat_buf = device
                .alloc_zeros::<u32>(tri_materials.len())
                .map_err(|e| format!("grow tri_mat: {}", e))?;
            self.tri_mat_cap = tri_materials.len();
        }
        let mat_len = materials.len().max(1);
        if mat_len > self.mat_cap {
            self.mat_buf = device
                .alloc_zeros::<HwRtMaterial>(mat_len)
                .map_err(|e| format!("grow mat: {}", e))?;
            self.mat_cap = mat_len;
        }

        device
            .htod_copy_into(pos_flat, &mut self.pos_buf)
            .map_err(|e| format!("upload pos: {}", e))?;
        device
            .htod_copy_into(nrm_flat, &mut self.nrm_buf)
            .map_err(|e| format!("upload nrm: {}", e))?;
        device
            .htod_copy_into(indices.to_vec(), &mut self.idx_buf)
            .map_err(|e| format!("upload idx: {}", e))?;
        device
            .htod_copy_into(tri_materials.to_vec(), &mut self.tri_mat_buf)
            .map_err(|e| format!("upload tri_mat: {}", e))?;
        if !materials.is_empty() {
            device
                .htod_copy_into(materials.to_vec(), &mut self.mat_buf)
                .map_err(|e| format!("upload mat: {}", e))?;
        }

        let mut temp_size: u64 = 0;
        let mut output_size: u64 = 0;
        let rc = unsafe {
            fl_optix_tri_accel_buffer_sizes(
                optix_ctx,
                num_vertices,
                num_triangles,
                &mut temp_size,
                &mut output_size,
            )
        };
        if rc != 0 {
            return Err(format!("optix tri AccelComputeMemoryUsage failed: {}", rc));
        }

        if temp_size > self.temp_size {
            self.gas_temp = device
                .alloc_zeros::<u8>(temp_size as usize)
                .map_err(|e| format!("grow gas_temp: {}", e))?;
            self.temp_size = temp_size;
        }
        if output_size > self.output_size {
            self.gas_output = device
                .alloc_zeros::<u8>(output_size as usize)
                .map_err(|e| format!("grow gas_output: {}", e))?;
            self.output_size = output_size;
        }

        let mut handle: u64 = 0;
        let rc = unsafe {
            fl_optix_build_triangle_gas(
                optix_ctx,
                (*device.cu_stream()).cast(),
                dev_ptr(&self.pos_buf),
                num_vertices,
                dev_ptr(&self.idx_buf),
                num_triangles,
                dev_ptr(&self.gas_temp),
                temp_size,
                dev_ptr(&self.gas_output),
                output_size,
                &mut handle,
            )
        };
        if rc != 0 {
            return Err(format!("optixAccelBuild triangle GAS failed: {}", rc));
        }

        self.traversable = handle;
        self.num_vertices = num_vertices;
        self.num_triangles = num_triangles;
        self.num_materials = materials.len() as u32;

        device
            .synchronize()
            .map_err(|e| format!("sync after triangle GAS: {}", e))?;

        Ok(())
    }
}
