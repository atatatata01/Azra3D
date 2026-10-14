//! High-level CUDA renderer: owns context + buffers, dispatches full pipeline.

use crate::device::CudaContext;
use crate::buffers::CudaBufferSet;
use crate::CudaBackend;
use cudarc::driver::{LaunchAsync, LaunchConfig};

#[cfg(feature = "optix")]
use crate::optix_accel::{HwRtMaterial, OptixAccel, OptixTriangleAccel};
#[cfg(feature = "optix")]
use crate::optix_pipeline::OptixPipeline;
#[cfg(feature = "optix")]
use crate::optix_tri_pipeline::OptixTriPipeline;

/// Post-processing flags (matches WGSL post_flags bitfield).
#[allow(dead_code)]
const POST_FLAG_ACES: u32 = 1;
const POST_FLAG_BLOOM: u32 = 2;
const POST_FLAG_SSR: u32 = 4;
const POST_FLAG_FOG: u32 = 8;

/// Thread block dimensions for 2D kernels.
const BLOCK_X: u32 = 16;
const BLOCK_Y: u32 = 16;

/// Create a 2D launch config for a given pixel grid.
fn grid_2d(w: u32, h: u32) -> LaunchConfig {
    LaunchConfig {
        grid_dim: ((w + BLOCK_X - 1) / BLOCK_X, (h + BLOCK_Y - 1) / BLOCK_Y, 1),
        block_dim: (BLOCK_X, BLOCK_Y, 1),
        shared_mem_bytes: 0,
    }
}

/// OptiX state: SDF AABB GAS pipeline (deferred G-buffer path).
#[cfg(feature = "optix")]
pub struct OptixState {
    pub pipeline: OptixPipeline,
    pub accel: OptixAccel,
}

/// OptiX triangle query state (hardware RT cores, no material shading).
#[cfg(feature = "optix")]
pub struct OptixTriState {
    pub pipeline: OptixTriPipeline,
    pub accel: OptixTriangleAccel,
    /// Scene generation last built — skip rebuild when unchanged.
    pub scene_generation: u64,
}

/// The main CUDA rendering pipeline.
pub struct CudaRenderer {
    pub ctx: CudaContext,
    pub bufs: CudaBufferSet,
    pub backend: CudaBackend,
    #[cfg(feature = "optix")]
    pub optix: Option<OptixState>,
    #[cfg(feature = "optix")]
    pub optix_tri: Option<OptixTriState>,
}

impl CudaRenderer {
    /// Create a new CUDA renderer. Returns None if CUDA is unavailable.
    pub fn try_new(width: u32, height: u32) -> Option<Self> {
        let ctx = CudaContext::init().ok()?;
        let bufs = CudaBufferSet::new(&ctx.device, width, height, 256).ok()?;

        #[cfg(feature = "optix")]
        let optix = None;

        // Triangle path-trace pipeline (hardware RT cores) — independent of SDF path
        #[cfg(feature = "optix")]
        let optix_tri = if ctx.is_rtx() {
            match OptixTriPipeline::try_new(&ctx.device) {
                Ok(pipeline) => match OptixTriangleAccel::new(&ctx.device) {
                    Ok(accel) => {
                        eprintln!("[cuda_render] OptiX triangle queries available; production material parity is not ready");
                        Some(OptixTriState {
                            pipeline,
                            accel,
                            scene_generation: 0,
                        })
                    }
                    Err(e) => {
                        eprintln!("[cuda_render] OptiX triangle accel init failed: {}", e);
                        None
                    }
                },
                Err(e) => {
                    eprintln!("[cuda_render] OptiX triangle pipeline init failed: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Some(Self {
            ctx,
            bufs,
            backend: CudaBackend::Ptx,
            #[cfg(feature = "optix")]
            optix,
            #[cfg(feature = "optix")]
            optix_tri,
        })
    }

    /// Is this an RTX GPU (OptiX-capable)?
    pub fn is_rtx(&self) -> bool {
        self.ctx.is_rtx()
    }

    /// Production routing requires both traversal and the shared shading integration.
    pub fn hw_rt_available(&self) -> bool {
        #[cfg(feature = "optix")]
        {
            self.optix_tri.is_some() && crate::OPTIX_MATERIAL_PARITY_READY
        }
        #[cfg(not(feature = "optix"))]
        {
            false
        }
    }

    /// GPU info string for UI.
    pub fn device_info(&self) -> String {
        self.ctx.device_name()
    }

    /// Upload a world-space triangle scene and build the hardware triangle GAS.
    ///
    /// Call whenever the PT mesh generation changes. Skips rebuild when
    /// `scene_generation` matches the last successful build.
    #[cfg(feature = "optix")]
    pub fn update_triangle_scene(
        &mut self,
        scene_generation: u64,
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        indices: &[u32],
        tri_materials: &[u32],
        materials: &[HwRtMaterial],
    ) -> Result<bool, String> {
        let Some(ref mut tri) = self.optix_tri else {
            return Ok(false);
        };
        if tri.scene_generation == scene_generation && scene_generation != 0 {
            return Ok(false);
        }
        tri.scene_generation = 0;
        let ctx_handle = tri.pipeline.ctx_handle();
        tri.accel.build(
            &self.ctx.device,
            ctx_handle,
            positions,
            normals,
            indices,
            tri_materials,
            materials,
        )?;
        tri.scene_generation = scene_generation;
        Ok(true)
    }

    #[cfg(feature = "optix")]
    pub fn trace_triangle_queries(
        &mut self,
        rays: &cudarc::driver::CudaSlice<crate::TriangleRay>,
        hits: &mut cudarc::driver::CudaSlice<crate::TriangleHit>,
        identities: &cudarc::driver::CudaSlice<crate::TriangleIdentity>,
    ) -> Result<(), String> {
        let tri = self.optix_tri.as_mut().ok_or("OptiX triangle queries unavailable")?;
        tri.pipeline.launch_queries(&self.ctx.device, &tri.accel, rays, hits, identities)
    }



    /// Resize render buffers if viewport changed.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<bool, String> {
        self.bufs.resize_if_needed(&self.ctx.device, width, height)
    }

    /// Full pipeline render. Returns RGBA8 pixel data.
    ///
    /// `uniform_bytes`: 320-byte GpuUniforms blob
    /// `prim_floats`: flat f32 primitive buffer
    /// `post_flags`: bitfield controlling post-processing passes
    /// `use_pathtracer`: true = path trace mode instead of deferred
    /// `use_upsample`: true = run CAS upsample pass
    pub fn render(
        &mut self,
        uniform_bytes: &[u8],
        prim_floats: &[f32],
        post_flags: u32,
        use_pathtracer: bool,
        use_upsample: bool,
    ) -> Result<Vec<u8>, String> {
        if use_pathtracer {
            return Err(crate::OPTIX_MATERIAL_PARITY_GAP.into());
        }
        let device = &self.ctx.device;

        // Upload scene data
        self.bufs.upload_uniforms(device, uniform_bytes)?;
        self.bufs.upload_primitives(device, prim_floats)?;

        {
            // ---- Pass 1: G-Buffer (OptiX SDF AABB or PTX sphere tracing) ----
            #[cfg(feature = "optix")]
            {
                if self.backend == CudaBackend::OptiX {
                    if let Some(ref mut optix) = self.optix {
                        let ctx_handle = optix.pipeline.ctx_handle();
                        optix.accel.build(&self.ctx.device, ctx_handle, prim_floats)?;
                        let trav = optix.accel.traversable();
                        let w = self.bufs.width;
                        let h = self.bufs.height;
                        optix.pipeline.launch(&self.ctx.device, &self.bufs, trav, w, h)?;
                    } else {
                        self.dispatch_raymarch_gbuf()?;
                    }
                } else {
                    self.dispatch_raymarch_gbuf()?;
                }
            }
            #[cfg(not(feature = "optix"))]
            self.dispatch_raymarch_gbuf()?;

            if use_pathtracer {
                self.dispatch_pathtrace()?;
            } else {
                self.dispatch_deferred_shade()?;
            }
        }

        // ---- Pass 3: SSR (optional) ----
        if post_flags & POST_FLAG_SSR != 0 {
            self.dispatch_ssr()?;
        }

        // ---- Pass 4: Fog (optional) ----
        if post_flags & POST_FLAG_FOG != 0 {
            self.dispatch_fog()?;
        }

        // ---- Pass 5: Bloom (optional) ----
        if post_flags & POST_FLAG_BLOOM != 0 {
            self.dispatch_bloom()?;
        }

        // ---- Pass 6: Tonemap (HDR → LDR) ----
        self.dispatch_tonemap()?;

        // ---- Pass 7: Upsample (optional) ----
        if use_upsample {
            self.dispatch_upsample()?;
            return self.bufs.download_upsample(&self.ctx.device);
        }

        self.bufs.download_ldr(&self.ctx.device)
    }

    /// Clear path tracer accumulation (call on camera/scene change).
    pub fn reset_accumulation(&mut self) -> Result<(), String> {
        self.bufs.clear_pt_accum(&self.ctx.device)
    }

    // ================================================================
    // Individual pass dispatchers
    // ================================================================

    fn dispatch_raymarch_gbuf(&mut self) -> Result<(), String> {
        let func = match self.ctx.get_func("raymarch_gbuf", "raymarch_gbuf_kernel") {
            Some(f) => f,
            None => return Ok(()),
        };
        let w = self.bufs.width;
        let h = self.bufs.height;
        unsafe {
            func.launch(grid_2d(w, h), (
                &self.bufs.uniform_buf,
                &self.bufs.prim_buf,
                &mut self.bufs.gbuf_normal,
                &mut self.bufs.gbuf_material,
                &mut self.bufs.gbuf_albedo,
                w, h,
            ))
        }.map_err(|e| format!("raymarch_gbuf: {}", e))
    }

    fn dispatch_deferred_shade(&mut self) -> Result<(), String> {
        let func = match self.ctx.get_func("deferred_shade", "deferred_shade_kernel") {
            Some(f) => f,
            None => return Ok(()),
        };
        let w = self.bufs.width;
        let h = self.bufs.height;
        unsafe {
            func.launch(grid_2d(w, h), (
                &self.bufs.uniform_buf,
                &self.bufs.prim_buf,
                &self.bufs.gbuf_normal,
                &self.bufs.gbuf_material,
                &self.bufs.gbuf_albedo,
                &mut self.bufs.hdr_buf,
                w, h,
            ))
        }.map_err(|e| format!("deferred_shade: {}", e))
    }

    fn dispatch_pathtrace(&mut self) -> Result<(), String> {
        let func = match self.ctx.get_func("pathtrace", "pathtrace_kernel") {
            Some(f) => f,
            None => return Ok(()),
        };
        let w = self.bufs.width;
        let h = self.bufs.height;
        unsafe {
            func.launch(grid_2d(w, h), (
                &self.bufs.uniform_buf,
                &self.bufs.prim_buf,
                &self.bufs.pt_accum,
                &mut self.bufs.hdr_buf,
                w, h,
            ))
        }.map_err(|e| format!("pathtrace: {}", e))
    }

    fn dispatch_ssr(&mut self) -> Result<(), String> {
        let func = match self.ctx.get_func("ssr", "ssr_kernel") {
            Some(f) => f,
            None => return Ok(()),
        };
        let w = self.bufs.width;
        let h = self.bufs.height;
        unsafe {
            func.launch(grid_2d(w, h), (
                &self.bufs.uniform_buf,
                &self.bufs.gbuf_normal,
                &self.bufs.gbuf_material,
                &self.bufs.hdr_buf,
                &mut self.bufs.ssr_buf,
                w, h,
            ))
        }.map_err(|e| format!("ssr: {}", e))
    }

    fn dispatch_fog(&mut self) -> Result<(), String> {
        let func = match self.ctx.get_func("fog", "fog_kernel") {
            Some(f) => f,
            None => return Ok(()),
        };
        let w = self.bufs.width;
        let h = self.bufs.height;
        unsafe {
            func.launch(grid_2d(w, h), (
                &self.bufs.uniform_buf,
                &self.bufs.prim_buf,
                &self.bufs.gbuf_normal,
                &mut self.bufs.fog_buf,
                w, h,
            ))
        }.map_err(|e| format!("fog: {}", e))
    }

    fn dispatch_bloom(&mut self) -> Result<(), String> {
        let w = self.bufs.width;
        let h = self.bufs.height;
        let half_w = w / 2;
        let half_h = h / 2;

        // Extract: HDR → half-res bright pixels
        if let Some(func) = self.ctx.get_func("bloom", "bloom_extract_kernel") {
            unsafe {
                func.launch(grid_2d(half_w, half_h), (
                    &self.bufs.uniform_buf,
                    &self.bufs.hdr_buf,
                    &mut self.bufs.bloom_buf,
                    w, h, half_w, half_h,
                ))
            }.map_err(|e| format!("bloom extract: {}", e))?;
        }

        // Blur H: bloom_buf → bloom_tmp
        if let Some(func) = self.ctx.get_func("bloom", "bloom_blur_kernel") {
            unsafe {
                func.launch(grid_2d(half_w, half_h), (
                    &self.bufs.bloom_buf,
                    &mut self.bufs.bloom_tmp,
                    half_w, half_h, 1i32, 0i32,
                ))
            }.map_err(|e| format!("bloom blur h: {}", e))?;
        }

        // Blur V: bloom_tmp → bloom_buf
        if let Some(func) = self.ctx.get_func("bloom", "bloom_blur_kernel") {
            unsafe {
                func.launch(grid_2d(half_w, half_h), (
                    &self.bufs.bloom_tmp,
                    &mut self.bufs.bloom_buf,
                    half_w, half_h, 0i32, 1i32,
                ))
            }.map_err(|e| format!("bloom blur v: {}", e))?;
        }

        Ok(())
    }

    fn dispatch_tonemap(&mut self) -> Result<(), String> {
        let func = match self.ctx.get_func("tonemap", "tonemap_kernel") {
            Some(f) => f,
            None => return Ok(()),
        };
        let w = self.bufs.width;
        let h = self.bufs.height;
        let half_w = w / 2;
        let half_h = h / 2;
        unsafe {
            func.launch(grid_2d(w, h), (
                &self.bufs.uniform_buf,
                &self.bufs.hdr_buf,
                &self.bufs.bloom_buf,
                &self.bufs.ssr_buf,
                &self.bufs.fog_buf,
                &mut self.bufs.ldr_buf,
                w, h, half_w, half_h,
            ))
        }.map_err(|e| format!("tonemap: {}", e))
    }

    fn dispatch_upsample(&mut self) -> Result<(), String> {
        let func = match self.ctx.get_func("upsample", "upsample_kernel") {
            Some(f) => f,
            None => return Ok(()),
        };
        let in_w = self.bufs.width;
        let in_h = self.bufs.height;
        // TODO: wire viewport dimensions for actual upscaling
        let out_w = in_w;
        let out_h = in_h;
        unsafe {
            func.launch(grid_2d(out_w, out_h), (
                &self.bufs.ldr_buf,
                &mut self.bufs.upsample_buf,
                in_w, in_h, out_w, out_h,
            ))
        }.map_err(|e| format!("upsample: {}", e))
    }
}
