//! GPU buffer management for CUDA rendering pipeline.
//!
//! Mirrors the wgpu buffer/texture layout used by the WGSL pipeline:
//! - uniform_buf: GpuUniforms (320 bytes)
//! - prim_buf: N * FLOATS_PER_PRIM * 4 bytes
//! - G-buffer: normal+depth, material, albedo (per-pixel)
//! - HDR, LDR, bloom, SSR, fog textures (per-pixel)
//! - Path tracer accumulation buffer

use cudarc::driver::{CudaDevice, CudaSlice, DeviceSlice};
use std::sync::Arc;

/// Bytes per uniform block (GpuUniforms = 320 bytes).
const UNIFORM_SIZE: usize = 320;

/// All device-side buffers for the rendering pipeline.
pub struct CudaBufferSet {
    // Scene data (uploaded from CPU each frame)
    pub uniform_buf: CudaSlice<u8>,
    pub prim_buf: CudaSlice<f32>,
    pub prim_capacity: usize, // max primitives allocated

    // G-buffer (per-pixel, rgba16float simulated as f32x4)
    pub gbuf_normal: CudaSlice<f32>,  // xyz=normal, w=depth
    pub gbuf_material: CudaSlice<f32>, // roughness, metallic, ao, flags
    pub gbuf_albedo: CudaSlice<u8>,    // rgba8

    // HDR pipeline
    pub hdr_buf: CudaSlice<f32>,       // rgba f32 per pixel
    pub ldr_buf: CudaSlice<u8>,        // rgba8 final output

    // Post-processing intermediates
    pub bloom_buf: CudaSlice<f32>,     // half-res rgba f32
    pub bloom_tmp: CudaSlice<f32>,     // half-res temp for separable blur
    pub ssr_buf: CudaSlice<f32>,       // full-res rgba f32
    pub fog_buf: CudaSlice<f32>,       // full-res rgba f32

    // Path tracer accumulation
    pub pt_accum: CudaSlice<f32>,      // rgba f32 progressive accumulation

    // Upsampled output (viewport resolution, may differ from render resolution)
    pub upsample_buf: CudaSlice<u8>,   // rgba8

    // Current dimensions
    pub width: u32,
    pub height: u32,
}

impl CudaBufferSet {
    /// Allocate all buffers for a given render resolution.
    pub fn new(device: &Arc<CudaDevice>, width: u32, height: u32, max_prims: usize) -> Result<Self, String> {
        let npx = (width * height) as usize;
        let half_npx = ((width / 2) * (height / 2)) as usize;

        let uniform_buf = device.alloc_zeros::<u8>(UNIFORM_SIZE)
            .map_err(|e| format!("alloc uniform_buf: {}", e))?;

        let floats_per_prim = 108usize;
        let prim_buf = device.alloc_zeros::<f32>(max_prims * floats_per_prim)
            .map_err(|e| format!("alloc prim_buf: {}", e))?;

        let gbuf_normal = device.alloc_zeros::<f32>(npx * 4)
            .map_err(|e| format!("alloc gbuf_normal: {}", e))?;
        let gbuf_material = device.alloc_zeros::<f32>(npx * 4)
            .map_err(|e| format!("alloc gbuf_material: {}", e))?;
        let gbuf_albedo = device.alloc_zeros::<u8>(npx * 4)
            .map_err(|e| format!("alloc gbuf_albedo: {}", e))?;

        let hdr_buf = device.alloc_zeros::<f32>(npx * 4)
            .map_err(|e| format!("alloc hdr_buf: {}", e))?;
        let ldr_buf = device.alloc_zeros::<u8>(npx * 4)
            .map_err(|e| format!("alloc ldr_buf: {}", e))?;

        let bloom_buf = device.alloc_zeros::<f32>(half_npx * 4)
            .map_err(|e| format!("alloc bloom_buf: {}", e))?;
        let bloom_tmp = device.alloc_zeros::<f32>(half_npx * 4)
            .map_err(|e| format!("alloc bloom_tmp: {}", e))?;
        let ssr_buf = device.alloc_zeros::<f32>(npx * 4)
            .map_err(|e| format!("alloc ssr_buf: {}", e))?;
        let fog_buf = device.alloc_zeros::<f32>(npx * 4)
            .map_err(|e| format!("alloc fog_buf: {}", e))?;

        let pt_accum = device.alloc_zeros::<f32>(npx * 4)
            .map_err(|e| format!("alloc pt_accum: {}", e))?;

        let upsample_buf = device.alloc_zeros::<u8>(npx * 4)
            .map_err(|e| format!("alloc upsample_buf: {}", e))?;

        Ok(Self {
            uniform_buf,
            prim_buf,
            prim_capacity: max_prims,
            gbuf_normal,
            gbuf_material,
            gbuf_albedo,
            hdr_buf,
            ldr_buf,
            bloom_buf,
            bloom_tmp,
            ssr_buf,
            fog_buf,
            pt_accum,
            upsample_buf,
            width,
            height,
        })
    }

    /// Resize all buffers if resolution changed. Returns true if reallocation occurred.
    pub fn resize_if_needed(&mut self, device: &Arc<CudaDevice>, width: u32, height: u32) -> Result<bool, String> {
        if self.width == width && self.height == height {
            return Ok(false);
        }
        *self = Self::new(device, width, height, self.prim_capacity)?;
        Ok(true)
    }

    /// Upload uniform data (320 bytes) to device.
    pub fn upload_uniforms(&mut self, device: &Arc<CudaDevice>, data: &[u8]) -> Result<(), String> {
        assert_eq!(data.len(), UNIFORM_SIZE, "GpuUniforms must be exactly 320 bytes");
        device.htod_copy_into(data.to_vec(), &mut self.uniform_buf)
            .map_err(|e| format!("upload uniforms: {}", e))
    }

    /// Upload primitive buffer (flat f32 array) to device.
    pub fn upload_primitives(&mut self, device: &Arc<CudaDevice>, data: &[f32]) -> Result<(), String> {
        let floats_per_prim = 108usize;
        let needed = data.len();
        let capacity = self.prim_capacity * floats_per_prim;

        if needed > capacity {
            // Grow buffer
            let new_cap = (needed / floats_per_prim) + 64; // headroom
            self.prim_buf = device.alloc_zeros::<f32>(new_cap * floats_per_prim)
                .map_err(|e| format!("grow prim_buf: {}", e))?;
            self.prim_capacity = new_cap;
        }

        device.htod_copy_into(data.to_vec(), &mut self.prim_buf)
            .map_err(|e| format!("upload prims: {}", e))
    }

    /// Download LDR output (RGBA8) from device to host.
    pub fn download_ldr(&self, device: &Arc<CudaDevice>) -> Result<Vec<u8>, String> {
        let npx = (self.width * self.height) as usize;
        device.dtoh_sync_copy(&self.ldr_buf)
            .map_err(|e| format!("download ldr: {}", e))
    }

    /// Download upsampled output (RGBA8) from device to host.
    pub fn download_upsample(&self, device: &Arc<CudaDevice>) -> Result<Vec<u8>, String> {
        device.dtoh_sync_copy(&self.upsample_buf)
            .map_err(|e| format!("download upsample: {}", e))
    }

    /// Clear path tracer accumulation (e.g., on camera move).
    pub fn clear_pt_accum(&mut self, device: &Arc<CudaDevice>) -> Result<(), String> {
        let npx = (self.width * self.height) as usize;
        let zeros = vec![0.0f32; npx * 4];
        device.htod_copy_into(zeros, &mut self.pt_accum)
            .map_err(|e| format!("clear pt_accum: {}", e))
    }
}
