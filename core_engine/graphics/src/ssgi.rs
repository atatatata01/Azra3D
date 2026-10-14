//! Screen-space GI — pipelines, per-resolution resources, and the single dispatch
//! driver. Plan: `docs/planning/render/SSGI_PLAN.md`.
//!
//! Shape: 3 compute passes over a HALF-resolution rgba16float ping-pong pair
//! (`ssgi_entry.wgsl`: gather → depth-aware separable blur ×2). The result is a
//! signed radiance DELTA carrying its own guide depth in alpha; `pass_tonemap`
//! upsamples and adds it beside the SSR add.
//!
//! Ownership mirrors `render/src/ssao.rs`, which is this codebase's cleanest
//! precedent: `SsgiPipelines` owns the shader + BGL + 3 pipelines;
//! `SsgiResources` owns the two half-res textures, the params uniform and the
//! per-resolution bind groups. The app recreates `SsgiResources` in the existing
//! viewport resize block — the same site that recreates the G-buffer it reads —
//! so a cached bind group can never dangle. `dispatch()` is the single driver
//! used by both the app and the headless tests (no second code path).
//!
//! **Off-state contract.** `pass_tonemap` samples the delta unconditionally when
//! the composite bit is set, and that bit is assembled ~1480 lines before the
//! dispatch is known to happen. So a frame where the pipeline is still compiling,
//! or the texture was just reallocated, must not composite stale radiance:
//! `ensure_zero()` is the additive analogue of SSAO's `ensure_white()`. Zero is
//! the identity for an add, exactly as 1.0 is for a multiply.

use crate::bgl;
use crate::types::SsgiParams;

/// The three SSGI compute pipelines (one shader module, one shared BGL).
pub struct SsgiPipelines {
    pub bgl: wgpu::BindGroupLayout,
    pub gather: wgpu::ComputePipeline,
    pub blur_h: wgpu::ComputePipeline,
    pub blur_v: wgpu::ComputePipeline,
}

impl SsgiPipelines {
    pub fn new(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>) -> Self {
        let bgl_desc = bgl::ssgi_bgl_entries();
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ssgi_bgl"),
            entries: &bgl_desc,
        });
        let src = crate::shader_build::build_ssgi_wgsl();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ssgi"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ssgi"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let mk = |entry: &'static str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache,
            })
        };
        Self {
            bgl,
            gather: mk("ssgi_gather"),
            blur_h: mk("ssgi_blur_h"),
            blur_v: mk("ssgi_blur_v"),
        }
    }
}

/// Per-resolution SSGI resources: two half-res ping-pong textures + params
/// uniform + cached bind groups.
pub struct SsgiResources {
    pub half_size: (u32, u32),
    tex_a: wgpu::Texture,
    view_a: wgpu::TextureView,
    #[allow(dead_code)]
    tex_b: wgpu::Texture,
    view_b: wgpu::TextureView,
    pub params_buffer: wgpu::Buffer,
    bg_gather: Option<wgpu::BindGroup>,
    bg_blur_h: Option<wgpu::BindGroup>,
    bg_blur_v: Option<wgpu::BindGroup>,
    /// True while the output holds the additive identity (all zero).
    is_zero: bool,
}

fn make_tex(device: &wgpu::Device, label: &str, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl SsgiResources {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, full_w: u32, full_h: u32) -> Self {
        let hw = (full_w + 1) / 2;
        let hh = (full_h + 1) / 2;
        let tex_a = make_tex(device, "ssgi_a", hw, hh);
        let tex_b = make_tex(device, "ssgi_b", hw, hh);
        let view_a = tex_a.create_view(&Default::default());
        let view_b = tex_b.create_view(&Default::default());
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ssgi_params"),
            size: std::mem::size_of::<SsgiParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut r = Self {
            half_size: (hw, hh),
            tex_a,
            view_a,
            tex_b,
            view_b,
            params_buffer,
            bg_gather: None,
            bg_blur_h: None,
            bg_blur_v: None,
            is_zero: false,
        };
        // A freshly allocated texture is undefined; the composite may sample it
        // on this very frame if the pipeline is not ready yet.
        r.fill_zero(queue);
        r
    }

    /// The view `pass_tonemap` samples. `blur_v` writes back into A, so A is
    /// always the final result — independent of whether the blurs ran, which
    /// matters because the `skip_blur` dev toggle makes that conditional.
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.view_a
    }

    /// The texture behind `output_view`, for readback in the headless gates.
    pub fn output_texture(&self) -> &wgpu::Texture {
        &self.tex_a
    }

    /// Write the additive identity over the output. `queue.write_texture` rather
    /// than a clear pass: no encoder, no pipeline, callable from anywhere.
    pub fn fill_zero(&mut self, queue: &wgpu::Queue) {
        let (w, h) = self.half_size;
        let zeros = vec![0u8; (w * h * 8) as usize]; // rgba16f = 8 B/texel, 0 bits = +0.0
        queue.write_texture(
            self.tex_a.as_image_copy(),
            &zeros,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.is_zero = true;
    }

    /// Drop the cached bind groups so the next dispatch rebuilds them.
    ///
    /// Required whenever a resource they reference is REPLACED without this struct
    /// being recreated. The textures are safe (they are recreated together in the
    /// viewport resize block), but `env_buffer` is not: `env_ibl.rs` reallocates it
    /// on every HDRI load/clear, with no resize involved. The cached group would
    /// then hold a dead buffer. Every site that clears `deferred_bg` for that reason
    /// must call this too — see the Feature Dependency Audit rule.
    pub fn invalidate_bind_groups(&mut self) {
        self.bg_gather = None;
        self.bg_blur_h = None;
        self.bg_blur_v = None;
    }

    /// One-shot zero fill for frames the pass does NOT dispatch. Idempotent, so
    /// calling it every disabled frame costs one bool test.
    pub fn ensure_zero(&mut self, queue: &wgpu::Queue) {
        if !self.is_zero {
            self.fill_zero(queue);
        }
    }
}

/// Encode the SSGI chain. `skip_blur` is the §4.10 dev toggle (gather only).
///
/// `tsw` times the GATHER only (see the note at the take() below) — pass it only
/// from a call site that is certain to dispatch. Marking a timestamp slot for a
/// pass that then does not run resolves unwritten queries, which hangs the GPU.
#[allow(clippy::too_many_arguments)]
pub fn dispatch(
    enc: &mut wgpu::CommandEncoder,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &SsgiPipelines,
    res: &mut SsgiResources,
    uniform_buffer: &wgpu::Buffer,
    hdr_view: &wgpu::TextureView,
    gbuf_normal: &wgpu::TextureView,
    gbuf_material: &wgpu::TextureView,
    gbuf_albedo: &wgpu::TextureView,
    raster_depth: &wgpu::TextureView,
    env_buffer: &wgpu::Buffer,
    ssao_view: &wgpu::TextureView,
    params: &SsgiParams,
    skip_blur: bool,
    tsw: Option<wgpu::ComputePassTimestampWrites>,
) {
    queue.write_buffer(&res.params_buffer, 0, params.as_bytes());

    if res.bg_gather.is_none() {
        let mk = |label: &str, dst: &wgpu::TextureView, src: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &pipelines.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(dst) },
                    wgpu::BindGroupEntry { binding: 1, resource: uniform_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(hdr_view) },
                    wgpu::BindGroupEntry { binding: 12, resource: wgpu::BindingResource::TextureView(gbuf_normal) },
                    wgpu::BindGroupEntry { binding: 13, resource: wgpu::BindingResource::TextureView(gbuf_material) },
                    wgpu::BindGroupEntry { binding: 14, resource: wgpu::BindingResource::TextureView(gbuf_albedo) },
                    wgpu::BindGroupEntry { binding: 29, resource: wgpu::BindingResource::TextureView(raster_depth) },
                    wgpu::BindGroupEntry { binding: 21, resource: env_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 38, resource: wgpu::BindingResource::TextureView(ssao_view) },
                    wgpu::BindGroupEntry { binding: 60, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 61, resource: res.params_buffer.as_entire_binding() },
                ],
            })
        };
        // Ping-pong: gather →A, blur_h A→B, blur_v B→A, so A is always the result
        // whether or not the blurs ran (the `skip_blur` toggle makes that optional).
        //
        // The gather's slot 60 binds **B**, not A. `ssgi_gather` never reads
        // `ssgi_src`, but the three entries share one layout so the binding must be
        // filled — and filling it with A would bind the same texture as both the
        // storage-write target and a sampled resource in one dispatch, which is an
        // exclusive-usage conflict wgpu rejects at dispatch time:
        //   "Attempted to use Texture 'ssgi_a' with conflicting usages.
        //    Current usage STORAGE_READ_WRITE and new usage RESOURCE"
        // Caught by ssgi_gate1_composite_is_neutral_and_hits_only_receivers. It is
        // the same constraint blend_dispatch documents as "base and output MUST
        // reference different textures".
        res.bg_gather = Some(mk("ssgi_gather_bg", &res.view_a, &res.view_b));
        res.bg_blur_h = Some(mk("ssgi_blur_h_bg", &res.view_b, &res.view_a));
        res.bg_blur_v = Some(mk("ssgi_blur_v_bg", &res.view_a, &res.view_b));
    }

    let (hw, hh) = res.half_size;
    let (gx, gy) = ((hw + 7) / 8, (hh + 7) / 8);

    let passes: &[(&wgpu::ComputePipeline, &Option<wgpu::BindGroup>, &str)] = if skip_blur {
        &[(&pipelines.gather, &res.bg_gather, "ssgi_gather")]
    } else {
        &[
            (&pipelines.gather, &res.bg_gather, "ssgi_gather"),
            (&pipelines.blur_h, &res.bg_blur_h, "ssgi_blur_h"),
            (&pipelines.blur_v, &res.bg_blur_v, "ssgi_blur_v"),
        ]
    };
    // `tsw.take()` puts the timestamp on the FIRST pass only (the gather); the
    // blurs run untimed. Three dispatches cannot share one pass — wgpu's usage
    // scope is the whole pass, and tex_a is storage-written by the gather and
    // sampled by blur_h, which is the exclusive-usage conflict caught by the Gate
    // 1 test. So the HUD number is the gather alone: a lower bound, and Gate 5
    // must say so rather than compare it to the full-chain budget.
    let mut tsw = tsw;
    for (pl, bg, label) in passes {
        let Some(bg) = bg.as_ref() else { continue };
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: tsw.take(),
        });
        cp.set_pipeline(pl);
        cp.set_bind_group(0, bg, &[]);
        cp.dispatch_workgroups(gx, gy, 1);
    }
    res.is_zero = false;
}
