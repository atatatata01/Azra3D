// FUTURE INTEGRATION POINTS for dispatch_blend():
//
// 1. Debug SDF Overlay: analytical surface → overlay_texture → AlphaOver with tint
// 2. Wireframe on Shaded: wireframe pass → overlay_texture → ReplaceHit
// 3. Selection Highlight: selection rim → overlay_texture → Additive
// 4. Multi-body Volume: per-body volume → sequential blend into accumulator
// 5. X-ray Mode: hidden body → overlay_texture → AlphaOver at low opacity
// 6. AI Preview: generated texture → overlay_texture → AlphaOver
// 7. Dirty Region Debug: AABB wireframe → overlay_texture → Additive

use eframe::wgpu;
use crate::GpuCtx;

// ---- Blend mode enum ----

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u32)]
pub(crate) enum BlendMode {
    AlphaOver = 0,    // Standard transparency (debug overlays, highlights)
    #[allow(dead_code)]
    Additive = 1,     // Glow, emission overlays
    #[allow(dead_code)]
    Multiply = 2,     // AO overlay, shadow visualization
    #[allow(dead_code)]
    ReplaceHit = 3,   // X-ray, wireframe (replace only where overlay has data)
    /// Selected-prim outline compositing: overlay = the prim_outline pass's coverage mask
    /// (r=additive, g=cutter, a=coverage) → 3×3 edge-detect contour + pale cutter fill.
    MaskOutline = 4,
}

// ---- GPU-side uniform struct (must match BlendParams in composite_blend.wgsl) ----

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BlendParamsGpu {
    pub tint: [f32; 4],     // 16B — vec4<f32> first for natural alignment
    pub mode: u32,          // 4B
    pub opacity: f32,       // 4B
    pub _pad: [f32; 2],     // 8B — alignment to 32B total
}

// ---- Rust-side parameters ----

pub(crate) struct BlendParams {
    pub mode: BlendMode,
    pub tint: [f32; 4],
    pub opacity: f32,
}

impl Default for BlendParams {
    fn default() -> Self {
        Self {
            mode: BlendMode::AlphaOver,
            tint: [1.0, 1.0, 1.0, 1.0],
            opacity: 0.5,
        }
    }
}

impl BlendParams {
    pub fn to_gpu(&self) -> BlendParamsGpu {
        BlendParamsGpu {
            tint: self.tint,
            mode: self.mode as u32,
            opacity: self.opacity,
            _pad: [0.0; 2],
        }
    }
}

// ---- Overlay texture helpers ----

/// Ensure overlay_texture exists at the given dimensions. Lazy-allocates on first call,
/// recreates if viewport resized. Returns true if just created (caller should populate).
pub(crate) fn ensure_overlay_texture(gpu: &mut GpuCtx, width: u32, height: u32) -> bool {
    let need_create = match &gpu.overlay_texture {
        Some(tex) => {
            let s = tex.size();
            s.width != width || s.height != height
        }
        None => true,
    };
    if need_create {
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blend_overlay"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.overlay_view = Some(tex.create_view(&wgpu::TextureViewDescriptor::default()));
        gpu.overlay_texture = Some(tex);
    }
    // Also ensure blend_temp exists at same size
    let need_temp = match &gpu.blend_temp_texture {
        Some(tex) => {
            let s = tex.size();
            s.width != width || s.height != height
        }
        None => true,
    };
    if need_temp {
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blend_temp"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.blend_temp_view = Some(tex.create_view(&wgpu::TextureViewDescriptor::default()));
        gpu.blend_temp_texture = Some(tex);
    }
    need_create
}

// ---- Dispatch (two-phase to avoid borrow conflicts) ----

/// Phase 1: Ensure pipeline is ready and upload blend params to GPU.
/// Call this BEFORE borrowing texture views from GpuCtx.
/// Returns false if pipeline is not available (skip dispatch).
pub(crate) fn prepare_blend(gpu: &mut GpuCtx, params: &BlendParams) -> bool {
    crate::gpu_init::ensure_blend_pipeline(gpu);
    if gpu.pipelines.blend_pipeline.is_none() || gpu.bind_groups.blend_bgl.is_none() {
        return false;
    }
    // Ensure params buffer exists
    let params_buf = gpu.buffers.blend_params_buffer.get_or_insert_with(|| {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blend_params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    });
    let gpu_params = params.to_gpu();
    gpu.queue.write_buffer(params_buf, 0, bytemuck::bytes_of(&gpu_params));
    true
}

/// Phase 2: Create bind group and dispatch the blend compute shader.
///
/// CONSTRAINT: base_view and output_view MUST reference different textures.
/// WGSL forbids reading and writing the same texture in one dispatch.
/// For in-place blending, copy base → blend_temp_texture first, then
/// blend blend_temp + overlay → base.
///
/// Caller must call prepare_blend() first. Takes &GpuCtx (immutable)
/// so texture views borrowed from gpu don't conflict.
pub(crate) fn dispatch_blend(
    gpu: &GpuCtx,
    encoder: &mut wgpu::CommandEncoder,
    base_view: &wgpu::TextureView,
    overlay_view: &wgpu::TextureView,
    output_view: &wgpu::TextureView,
    width: u32,
    height: u32,
) {
    let pipeline = match gpu.pipelines.blend_pipeline {
        Some(ref p) => p,
        None => return,
    };
    let bgl = match gpu.bind_groups.blend_bgl {
        Some(ref b) => b,
        None => return,
    };
    let params_buf = match gpu.buffers.blend_params_buffer {
        Some(ref b) => b,
        None => return,
    };

    // Create one-shot bind group (textures vary per call)
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("blend_bg"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(base_view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(overlay_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(output_view) },
            wgpu::BindGroupEntry { binding: 3, resource: params_buf.as_entire_binding() },
        ],
    });

    // Dispatch
    let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("blend"),
        timestamp_writes: None,
    });
    cp.set_pipeline(pipeline);
    cp.set_bind_group(0, &bg, &[]);
    cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
}
