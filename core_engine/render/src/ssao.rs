//! Screen-space ambient occlusion (SSAO) pass — the real occlusion source for
//! raster pixels (the 2026-07-17d SHADOWSTALE entry removed the shadow-map-as-AO
//! conflation and left `final_ao = 1.0`; this module closes that trade-off).
//!
//! Shape: 3 compute passes over a HALF-resolution rgba16float target
//! (`ssao.wgsl`: hemisphere estimate → depth-aware separable blur ×2).
//! The blurred result (r = mix(1, ao, strength), g = half-res depth) is sampled
//! by `deferred_entry.wgsl` (binding 38) with a depth-aware bilinear upsample
//! and multiplies the ambient/IBL terms in PBR + Lit only.
//!
//! Ownership (GPU resource ownership rule): `SsaoPipelines` owns the shader +
//! BGL + 3 pipelines; `SsaoResources` owns the two half-res textures, the
//! params uniform and the per-resolution bind groups. The app stores both on
//! `GpuCtx` and recreates `SsaoResources` in the established viewport resize
//! block (same site as the G-buffer, Frame Ordering Contract), which is the
//! ONLY place `gbuf_normal` is recreated — so the cached bind groups can never
//! dangle. `dispatch()` is the single driver used by both the app and the
//! headless readback test (no second code path).

use bytemuck::{Pod, Zeroable};

pub const SSAO_WGSL: &str = include_str!("ssao.wgsl");

/// Uniform parameters for the AO passes (SSAO + GTAO share this buffer — the
/// GTAO tier reads the extra `slices`/`steps`/`thickness` fields, SSAO ignores
/// them). Camera packing follows `GpuUniforms`/`camera_ray`: `cam_pos.w` = ortho
/// half-width (0 = perspective), `cam_forward.w` = fov_scale (tan of half the
/// vertical FOV). MUST match `SsaoParams` in `ssao.wgsl` (112 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SsaoParams {
    pub cam_pos: [f32; 4],
    pub cam_forward: [f32; 4],
    pub cam_right: [f32; 4],
    pub cam_up: [f32; 4],
    pub full_size: [u32; 2],
    pub half_size: [u32; 2],
    /// Hemisphere radius in WORLD units (tweak param — the screen footprint
    /// scales with distance automatically via world-space tap projection).
    pub radius: f32,
    /// 0..1 — final factor is `mix(1.0, ao, strength)` (applied in blur_v).
    pub strength: f32,
    /// World-space occluder bias (self-occlusion / depth-precision guard).
    pub bias: f32,
    /// Progressive-refine frame term for the IGN rotation (PROGRESSIVE_PBR_PLAN.md
    /// Phase 2): 0 = the static per-pixel pattern (bit-identical to the pre-refine
    /// shader); nonzero (frame index mod 64) only while the idle accumulator is
    /// averaging, so the varying pattern converges instead of boiling.
    /// (Was `_pad` — same offset/size.)
    pub frame_jitter: f32,
    /// GTAO azimuthal slice count (directional occlusion resolution).
    pub slices: u32,
    /// GTAO horizon-march steps per side.
    pub steps: u32,
    /// GTAO horizon falloff distance beyond `radius` (world units) — range
    /// check that kills halos and softens thin-occluder over-darkening.
    pub thickness: f32,
    pub _pad2: f32,
}

/// Default world-space bias — flat-surface acne guard. Combined in-shader with
/// a distance-proportional term (`+ t * 0.002`) so far geometry stays clean.
pub const SSAO_DEFAULT_BIAS: f32 = 0.02;

/// GTAO defaults: 3 slices × 6 steps/side = 36 samples/pixel at half res.
pub const GTAO_DEFAULT_SLICES: u32 = 3;
pub const GTAO_DEFAULT_STEPS: u32 = 6;

/// Which AO tier a `dispatch()` runs. Both tiers write the SAME output texture
/// layout (r=AO, g=depth); GTAO additionally packs the octahedral bent normal
/// into b,a (SSAO leaves them 0 → deferred uses the geometric normal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AoMode {
    Ssao,
    Gtao,
}

/// Bind group layout entries — SINGLE SOURCE for the pipeline layout and every
/// bind group. src texture (0), dst storage (1), params uniform (2).
fn ssao_bgl_entries() -> [wgpu::BindGroupLayoutEntry; 3] {
    [
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba16Float,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
    ]
}

/// The three SSAO compute pipelines (one shader module, one shared BGL).
/// Tiny standalone WGSL — compiles in milliseconds (pt_denoise class), so it is
/// safe on both the background warmup thread and the lazy `ensure_*` fallback.
pub struct SsaoPipelines {
    pub bgl: wgpu::BindGroupLayout,
    pub main: wgpu::ComputePipeline,
    pub blur_h: wgpu::ComputePipeline,
    pub blur_v: wgpu::ComputePipeline,
    // GTAO tier — same module, same shared BGL, extra entry points.
    pub gtao_main: wgpu::ComputePipeline,
    pub gtao_blur_h: wgpu::ComputePipeline,
    pub gtao_blur_v: wgpu::ComputePipeline,
}

impl SsaoPipelines {
    pub fn new(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>) -> Self {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ssao_bgl"),
            entries: &ssao_bgl_entries(),
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ssao"),
            source: wgpu::ShaderSource::Wgsl(SSAO_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ssao"),
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
            main: mk("ssao_main"),
            blur_h: mk("ssao_blur_h"),
            blur_v: mk("ssao_blur_v"),
            gtao_main: mk("gtao_main"),
            gtao_blur_h: mk("gtao_blur_h"),
            gtao_blur_v: mk("gtao_blur_v"),
        }
    }
}

/// Per-resolution SSAO resources: two half-res ping-pong textures + params
/// uniform + cached bind groups. Created in the app's viewport resize block
/// (alongside the G-buffer — Frame Ordering Contract); bind groups are created
/// lazily at dispatch (they need the pipelines' BGL) and live exactly as long
/// as this struct, which is recreated whenever `gbuf_normal` is.
pub struct SsaoResources {
    pub half_size: (u32, u32),
    tex_a: wgpu::Texture,
    view_a: wgpu::TextureView,
    #[allow(dead_code)]
    tex_b: wgpu::Texture,
    view_b: wgpu::TextureView,
    pub params_buffer: wgpu::Buffer,
    bg_main: Option<wgpu::BindGroup>,
    bg_blur_h: Option<wgpu::BindGroup>,
    bg_blur_v: Option<wgpu::BindGroup>,
    /// True while the output texture holds the identity factor (1.0 everywhere).
    /// Creation white-fills; `dispatch()` clears the flag; `ensure_white()`
    /// restores it once when the toggle goes off (deferred keeps sampling the
    /// texture unconditionally — disabled must read as 1.0, the 17d behavior).
    pub is_white: bool,
}

impl SsaoResources {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, full_w: u32, full_h: u32) -> Self {
        let hw = (full_w + 1) / 2;
        let hh = (full_h + 1) / 2;
        let desc = wgpu::TextureDescriptor {
            label: Some("ssao_a"),
            size: wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        };
        let tex_a = device.create_texture(&desc);
        let tex_b = device.create_texture(&wgpu::TextureDescriptor { label: Some("ssao_b"), ..desc });
        let view_a = tex_a.create_view(&Default::default());
        let view_b = tex_b.create_view(&Default::default());
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ssao_params"),
            size: std::mem::size_of::<SsaoParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut s = Self {
            half_size: (hw, hh),
            tex_a,
            view_a,
            tex_b,
            view_b,
            params_buffer,
            bg_main: None,
            bg_blur_h: None,
            bg_blur_v: None,
            is_white: false,
        };
        // Start as identity: deferred binds this texture from frame one, possibly
        // before the first SSAO dispatch (pipeline still compiling) or with the
        // toggle off — it must read 1.0 until real data lands.
        s.fill_white(queue);
        s
    }

    /// The texture deferred_entry.wgsl samples at binding 38 (blur_v output).
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.view_a
    }

    /// Fill the output with the identity factor (r = 1.0, depth = 0 — the
    /// deferred upsample treats depth 0 texels as invalid and returns 1.0).
    pub fn fill_white(&mut self, queue: &wgpu::Queue) {
        let (hw, hh) = self.half_size;
        // rgba16float texel [1.0, 0.0, 0.0, 0.0] → f16 bits [0x3C00, 0, 0, 0].
        let px: [u16; 4] = [0x3C00, 0, 0, 0];
        let row: Vec<u8> = px.iter().flat_map(|v| v.to_le_bytes()).collect();
        let data: Vec<u8> = row.iter().copied().cycle().take((hw * hh) as usize * 8).collect();
        queue.write_texture(
            self.tex_a.as_image_copy(),
            &data,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(hw * 8), rows_per_image: Some(hh) },
            wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
        );
        self.is_white = true;
    }

    /// One-shot restore of the identity factor when SSAO is toggled off
    /// (Frame Budget: the dispatches are skipped entirely; this write happens
    /// exactly once per off-transition, not per frame).
    pub fn ensure_white(&mut self, queue: &wgpu::Queue) {
        if !self.is_white {
            self.fill_white(queue);
        }
    }
}

/// Encode the 3 SSAO passes: hemisphere estimate → blur H → blur V.
/// The single dispatch driver for the app and the headless test.
/// `tsw_*`: optional GPU timestamp writes per pass (the app passes its profiler
/// slot on the main pass only; pass `None` when the slot is unavailable — never
/// mark a slot for a pass that does not dispatch).
#[allow(clippy::too_many_arguments)]
pub fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &SsaoPipelines,
    res: &mut SsaoResources,
    gbuf_normal_view: &wgpu::TextureView,
    params: &SsaoParams,
    mode: AoMode,
    tsw_main: Option<wgpu::ComputePassTimestampWrites>,
    tsw_blur_h: Option<wgpu::ComputePassTimestampWrites>,
    tsw_blur_v: Option<wgpu::ComputePassTimestampWrites>,
) {
    queue.write_buffer(&res.params_buffer, 0, bytemuck::bytes_of(params));
    if res.bg_main.is_none() {
        // src/dst wiring is identical for both tiers (gbuf→A, A→B, B→A), so the
        // GTAO tier reuses these exact bind groups — only the pipelines differ.
        let mk = |label: &str, src: &wgpu::TextureView, dst: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &pipelines.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(dst) },
                    wgpu::BindGroupEntry { binding: 2, resource: res.params_buffer.as_entire_binding() },
                ],
            })
        };
        res.bg_main = Some(mk("ao_main_bg", gbuf_normal_view, &res.view_a));
        res.bg_blur_h = Some(mk("ao_blur_h_bg", &res.view_a, &res.view_b));
        res.bg_blur_v = Some(mk("ao_blur_v_bg", &res.view_b, &res.view_a));
    }
    let (hw, hh) = res.half_size;
    let (gx, gy) = ((hw + 7) / 8, (hh + 7) / 8);
    let (p_main, p_bh, p_bv, l_main, l_bh, l_bv) = match mode {
        AoMode::Ssao => (
            &pipelines.main, &pipelines.blur_h, &pipelines.blur_v,
            "ssao_main", "ssao_blur_h", "ssao_blur_v",
        ),
        AoMode::Gtao => (
            &pipelines.gtao_main, &pipelines.gtao_blur_h, &pipelines.gtao_blur_v,
            "gtao_main", "gtao_blur_h", "gtao_blur_v",
        ),
    };
    let passes: [(&str, &wgpu::ComputePipeline, &wgpu::BindGroup, Option<wgpu::ComputePassTimestampWrites>); 3] = [
        (l_main, p_main, res.bg_main.as_ref().unwrap(), tsw_main),
        (l_bh, p_bh, res.bg_blur_h.as_ref().unwrap(), tsw_blur_h),
        (l_bv, p_bv, res.bg_blur_v.as_ref().unwrap(), tsw_blur_v),
    ];
    for (label, pipeline, bg, tsw) in passes {
        let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: tsw,
        });
        cp.set_pipeline(pipeline);
        cp.set_bind_group(0, bg, &[]);
        cp.dispatch_workgroups(gx, gy, 1);
    }
    res.is_white = false;
}

// ============================================================================
// Headless GPU readback harness (the shadow-saga standard: no reasoning-only
// verification — render the real raster G-buffer, run the real SSAO passes,
// read the numbers back).
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{CameraUniforms, MAX_GRAB_INSTANCES};
    use crate::mesh_gpu::{InstanceData, RasterMaterial, RasterVertex};
    use crate::pipeline::RasterPipeline;
    use crate::shadows::ShadowMapSystem;
    use glam::{Mat4, Vec3};
    use wgpu::util::DeviceExt;

    /// Static Naga validation — a WGSL typo must be a test failure, not a
    /// runtime GPU panic (line_overlay precedent).
    #[test]
    fn ssao_wgsl_parses() {
        let module = naga::front::wgsl::parse_str(SSAO_WGSL).expect("ssao.wgsl parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator.validate(&module).expect("ssao.wgsl validate");
    }

    #[test]
    fn ssao_params_size() {
        // Must match the WGSL SsaoParams uniform (112 bytes, 16-aligned —
        // grew by one vec4 for the GTAO slices/steps/thickness fields).
        assert_eq!(std::mem::size_of::<SsaoParams>(), 112);
    }

    fn try_device(want_timestamps: bool) -> Option<(wgpu::Device, wgpu::Queue, bool)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })).ok()?;
        let has_ts = want_timestamps && adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let features = if has_ts { wgpu::Features::TIMESTAMP_QUERY } else { wgpu::Features::empty() };
        let (d, q) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("ssao_readback_test_device"),
                required_features: features,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            },
        ))
        .ok()?;
        Some((d, q, has_ts))
    }

    /// Unit cube (xz ∈ [-1,1], y ∈ [0,2]) over a 24×24 floor at y = 0 — the
    /// shadow harness's scene (shadows.rs::cube_and_floor), replicated with the
    /// same dimensions so probe geometry matches the proven precedent.
    fn cube_and_floor() -> (Vec<RasterVertex>, Vec<u32>) {
        let mut v = Vec::new();
        let mut idx = Vec::new();
        let mut quad = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3], n: [f32; 3]| {
            let base = v.len() as u32;
            for p in [a, b, c, d] {
                v.push(RasterVertex { position: p, normal: n, uv: [0.0, 0.0] });
            }
            idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        };
        quad([-1., 2., -1.], [-1., 2., 1.], [1., 2., 1.], [1., 2., -1.], [0., 1., 0.]); // top
        quad([-1., 0., -1.], [1., 0., -1.], [1., 0., 1.], [-1., 0., 1.], [0., -1., 0.]); // bottom
        quad([1., 0., -1.], [1., 2., -1.], [1., 2., 1.], [1., 0., 1.], [1., 0., 0.]); // +x
        quad([-1., 0., -1.], [-1., 0., 1.], [-1., 2., 1.], [-1., 2., -1.], [-1., 0., 0.]); // -x
        quad([-1., 0., 1.], [1., 0., 1.], [1., 2., 1.], [-1., 2., 1.], [0., 0., 1.]); // +z
        quad([-1., 0., -1.], [-1., 2., -1.], [1., 2., -1.], [1., 0., -1.], [0., 0., -1.]); // -z
        quad([-12., 0., -12.], [-12., 0., 12.], [12., 0., 12.], [12., 0., -12.], [0., 1., 0.]); // floor
        (v, idx)
    }

    struct TestCamera {
        eye: Vec3,
        forward: Vec3,
        right: Vec3,
        up: Vec3,
        fov_scale: f32,
        /// Ortho half-HEIGHT (production packs this in cam_pos.w; 0 = perspective).
        /// Named `_hw`/"half-width" in several shaders — the docs are wrong, the
        /// code is right (`ortho_hw * aspect` for x, `ortho_hw` for y).
        ortho_half_h: Option<f32>,
        width: u32,
        height: u32,
    }

    impl TestCamera {
        fn new(eye: Vec3, target: Vec3, fov_y_deg: f32, width: u32, height: u32) -> Self {
            let forward = (target - eye).normalize();
            let right = forward.cross(Vec3::Y).normalize();
            let up = right.cross(forward);
            Self {
                eye, forward, right, up,
                fov_scale: (fov_y_deg.to_radians() * 0.5).tan(),
                ortho_half_h: None,
                width, height,
            }
        }
        /// Orthographic camera. `half_h` matches production, which uses the orbit
        /// distance as the ortho half-height (`render::camera::from_orbit`).
        fn new_ortho(eye: Vec3, target: Vec3, half_h: f32, width: u32, height: u32) -> Self {
            let mut c = Self::new(eye, target, 55.0, width, height);
            c.ortho_half_h = Some(half_h);
            c
        }
        fn view(&self) -> Mat4 {
            Mat4::look_at_rh(self.eye, self.eye + self.forward, Vec3::Y)
        }
        fn proj(&self) -> Mat4 {
            let aspect = self.width as f32 / self.height as f32;
            match self.ortho_half_h {
                Some(h) => Mat4::orthographic_rh(-h * aspect, h * aspect, -h, h, 0.1, 100.0),
                None => Mat4::perspective_rh((self.fov_scale.atan() * 2.0).max(1e-3), aspect, 0.1, 100.0),
            }
        }
        /// CPU replica of `camera_ray()` / `ssao_camera_ray()` for a pixel CENTER.
        /// Perspective: shared origin at the eye + per-pixel normalized direction.
        /// Ortho: per-pixel origin shifted perpendicular to forward, dir = forward.
        /// The inverse of `project_px`.
        fn camera_ray_px(&self, px: i32, py: i32) -> (Vec3, Vec3) {
            let w = self.width as f32;
            let h = self.height as f32;
            let aspect = w / h;
            let uv_x = ((px as f32 + 0.5) / w) * 2.0 - 1.0;
            let uv_y = -(((py as f32 + 0.5) / h) * 2.0 - 1.0);
            match self.ortho_half_h {
                Some(hh) => (
                    self.eye + self.right * (uv_x * hh * aspect) + self.up * (uv_y * hh),
                    self.forward,
                ),
                None => (
                    self.eye,
                    (self.forward
                        + self.right * (uv_x * aspect * self.fov_scale)
                        + self.up * (uv_y * self.fov_scale))
                        .normalize(),
                ),
            }
        }
        /// The depth `gbuf_depth()` (render/src/gbuffer.wgsl) must write for this
        /// world point: the ray parameter along camera_ray(). Perspective = |rel|;
        /// ortho = forward distance.
        fn expected_depth(&self, world: Vec3) -> f32 {
            let rel = world - self.eye;
            match self.ortho_half_h {
                Some(_) => rel.dot(self.forward),
                None => rel.length(),
            }
        }
        /// CPU replica of ssao.wgsl's projection — locates a world point's
        /// FULL-res pixel so probes can address the AO texture.
        fn project_px(&self, world: Vec3) -> (i32, i32) {
            let aspect = self.width as f32 / self.height as f32;
            let d = world - self.eye;
            let (uv_x, uv_y) = match self.ortho_half_h {
                Some(h) => {
                    assert!(d.dot(self.forward) > 0.0, "probe behind camera");
                    (d.dot(self.right) / (h * aspect), d.dot(self.up) / h)
                }
                None => {
                    let fz = d.dot(self.forward);
                    assert!(fz > 0.0, "probe behind camera");
                    (
                        d.dot(self.right) / (fz * aspect * self.fov_scale),
                        d.dot(self.up) / (fz * self.fov_scale),
                    )
                }
            };
            assert!(uv_x.abs() < 1.0 && uv_y.abs() < 1.0, "probe off-screen: uv=({uv_x},{uv_y})");
            let px = (uv_x + 1.0) * 0.5 * self.width as f32 - 0.5;
            let py = (1.0 - uv_y) * 0.5 * self.height as f32 - 0.5;
            (px.round() as i32, py.round() as i32)
        }
        fn ssao_params(&self, radius: f32, strength: f32) -> SsaoParams {
            SsaoParams {
                cam_pos: [self.eye.x, self.eye.y, self.eye.z, self.ortho_half_h.unwrap_or(0.0)],
                cam_forward: [self.forward.x, self.forward.y, self.forward.z, self.fov_scale],
                cam_right: [self.right.x, self.right.y, self.right.z, 0.0],
                cam_up: [self.up.x, self.up.y, self.up.z, 0.0],
                full_size: [self.width, self.height],
                half_size: [(self.width + 1) / 2, (self.height + 1) / 2],
                radius,
                strength,
                bias: SSAO_DEFAULT_BIAS,
                frame_jitter: 0.0,
                slices: GTAO_DEFAULT_SLICES,
                steps: GTAO_DEFAULT_STEPS,
                thickness: radius,
                _pad2: 0.0,
            }
        }

        /// CPU replica of the shader's octahedral decode — for reading bent
        /// normals back from the b,a channels of the GTAO output.
        fn oct_decode(e: [f32; 2]) -> Vec3 {
            let mut n = Vec3::new(e[0], e[1], 1.0 - e[0].abs() - e[1].abs());
            if n.z < 0.0 {
                let snx = if n.x >= 0.0 { 1.0 } else { -1.0 };
                let sny = if n.y >= 0.0 { 1.0 } else { -1.0 };
                let nx = (1.0 - n.y.abs()) * snx;
                let ny = (1.0 - n.x.abs()) * sny;
                n.x = nx;
                n.y = ny;
            }
            n.normalize()
        }
    }

    /// Render the cube-over-floor scene through the REAL raster G-buffer path
    /// (RasterPipeline + gbuffer.wgsl, all 4 targets + hardware depth) and
    /// return the gbuf_normal texture (xyz = world normal, w = world distance).
    fn render_gbuffer(device: &wgpu::Device, queue: &wgpu::Queue, cam: &TestCamera) -> wgpu::Texture {
        let mut raster = RasterPipeline::new(device);
        raster.ensure_depth(device, cam.width, cam.height);

        // Camera uniforms — same struct the app uploads (mesh_presentation path).
        let cam_u = CameraUniforms {
            view_proj: (cam.proj() * cam.view()).to_cols_array_2d(),
            view: cam.view().to_cols_array_2d(),
            proj: cam.proj().to_cols_array_2d(),
            // w = ortho half-height (0 = perspective) — drives gbuf_depth()'s branch.
            cam_pos: [cam.eye.x, cam.eye.y, cam.eye.z, cam.ortho_half_h.unwrap_or(0.0)],
            near_far: [0.1, 100.0],
            _pad: [0.0; 2],
            grab_radii: [0.0; 4],
            grab_anchors: [[0.0; 4]; MAX_GRAB_INSTANCES],
            grab_drags: [[0.0; 4]; MAX_GRAB_INSTANCES],
            wire_color: [0.0; 4],
            frame_flags: [0.0; 4],
        };
        raster.upload_camera(queue, &cam_u, &cam_u);

        let mut mat = RasterMaterial::zeroed();
        mat.albedo = [0.8, 0.8, 0.8, 1.0];
        mat.roughness_metallic = [0.8, 0.0, 0.0, 0.0];
        raster.upload_materials(device, queue, &[mat]);

        // 1×1 dummy material texture array + sampler (no material maps in play).
        let mat_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ssao_test_mat_tex"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mat_view = mat_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let mat_sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        raster.rebuild_bind_group(device, &mat_view, &mat_view, &mat_sampler);

        // Shadow system with the sun DISABLED (flags.x = 0 → compute_shadow()
        // returns 1.0 without sampling — the zero-light scenario of 17d).
        let mut shadows = ShadowMapSystem::new(device);
        shadows.ensure_textures(device);
        shadows.update_cascades(
            queue, cam.view(), cam.proj(), Vec3::new(0.4, 0.8, 0.45), 0.1, 100.0, false,
            crate::shadows::DEFAULT_PCF_RADIUS_TEXELS, 0.0,
            crate::shadows::ShadowBias::default(),
        );
        raster.rebuild_shadow_bind_group(device, &shadows);

        let (verts, indices) = cube_and_floor();
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ssao_test_verts"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ssao_test_idx"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let inst = InstanceData {
            model_matrix: Mat4::IDENTITY.to_cols_array_2d(),
            normal_matrix: [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]],
            material_idx: 0,
            body_index: 1,
            flags: 0,
            _pad: 0,
            curv_params: [0.0, 1.0, 1.0, 1.0],
        };
        raster.upload_instances(device, queue, &[inst], &[]);

        // G-buffer color targets (the 4 the raster pipeline writes).
        // C.5: location(3) motion is R32Float; other slots stay Rgba16Float.
        let f16 = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: cam.width, height: cam.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let gbuf_normal = f16("ssao_test_gbuf_normal");
        let gbuf_material = f16("ssao_test_gbuf_material");
        let gbuf_motion = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ssao_test_gbuf_motion"),
            size: wgpu::Extent3d { width: cam.width, height: cam.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let gbuf_albedo = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ssao_test_gbuf_albedo"),
            size: wgpu::Extent3d { width: cam.width, height: cam.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let views: Vec<wgpu::TextureView> = [&gbuf_normal, &gbuf_material, &gbuf_albedo, &gbuf_motion]
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();

        let mut enc = device.create_command_encoder(&Default::default());
        {
            let attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = views
                .iter()
                .map(|v| {
                    Some(wgpu::RenderPassColorAttachment {
                        view: v,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ssao_test_gbuffer"),
                color_attachments: &attachments,
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: raster.depth_view.as_ref().unwrap(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&raster.pipeline);
            pass.set_bind_group(0, raster.bind_group.as_ref().unwrap(), &[]);
            pass.set_bind_group(1, raster.shadow_bind_group.as_ref().unwrap(), &[]);
            // GPU_CURVATURE_KERNEL_PLAN.md P2: group 2 (vertex-curvature carry) must ALWAYS be
            // bound or pipeline validation fails outright — this test draws plain geometry with
            // no curvature buffer, so it binds the same dummy production draws use.
            pass.set_bind_group(2, &raster.curvature_dummy_bind_group, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.set_vertex_buffer(1, raster.instance_buffer.slice(..));
            pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
        }
        queue.submit([enc.finish()]);
        gbuf_normal
    }

    fn f16_to_f32(bits: u16) -> f32 {
        let sign = if bits & 0x8000 != 0 { -1.0f32 } else { 1.0 };
        let exp = ((bits >> 10) & 0x1F) as i32;
        let frac = (bits & 0x3FF) as f32;
        if exp == 0 {
            sign * frac / 1024.0 * 2f32.powi(-14)
        } else if exp == 31 {
            if frac == 0.0 { sign * f32::INFINITY } else { f32::NAN }
        } else {
            sign * (1.0 + frac / 1024.0) * 2f32.powi(exp - 15)
        }
    }

    /// Read back the full AO output texture as f32 (r channel per texel) plus
    /// the raw bytes (for the determinism compare).
    fn read_ao(device: &wgpu::Device, queue: &wgpu::Queue, res: &SsaoResources) -> (Vec<f32>, Vec<u8>) {
        let (hw, hh) = res.half_size;
        let row_bytes = hw * 8;
        let padded = (row_bytes + 255) & !255;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ssao_test_read"),
            size: (padded * hh) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            res.tex_a.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(hh),
                },
            },
            wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        rx.recv().expect("map_async channel").expect("map_async failed");
        let data = buf.slice(..).get_mapped_range();
        let mut ao = vec![0.0f32; (hw * hh) as usize];
        let mut raw = Vec::with_capacity((row_bytes * hh) as usize);
        for y in 0..hh {
            let row_start = (y * padded) as usize;
            raw.extend_from_slice(&data[row_start..row_start + row_bytes as usize]);
            for x in 0..hw {
                let o = row_start + (x * 8) as usize;
                let bits = u16::from_le_bytes([data[o], data[o + 1]]);
                ao[(y * hw + x) as usize] = f16_to_f32(bits);
            }
        }
        (ao, raw)
    }

    fn ao_at(cam: &TestCamera, res: &SsaoResources, ao: &[f32], world: Vec3) -> f32 {
        let (px, py) = cam.project_px(world);
        let (hx, hy) = (px / 2, py / 2);
        let (hw, hh) = res.half_size;
        assert!(hx >= 0 && hy >= 0 && (hx as u32) < hw && (hy as u32) < hh);
        ao[(hy as u32 * hw + hx as u32) as usize]
    }

    /// Read back the full rgba16float AO texture as [r,g,b,a] per texel
    /// (r=AO, g=depth, b,a = octahedral bent normal for the GTAO tier).
    fn read_ao_rgba(device: &wgpu::Device, queue: &wgpu::Queue, res: &SsaoResources) -> Vec<[f32; 4]> {
        let (hw, hh) = res.half_size;
        let row_bytes = hw * 8;
        let padded = (row_bytes + 255) & !255;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ao_test_read_rgba"),
            size: (padded * hh) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            res.tex_a.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(hh) },
            },
            wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        rx.recv().expect("map_async channel").expect("map_async failed");
        let data = buf.slice(..).get_mapped_range();
        let mut out = vec![[0.0f32; 4]; (hw * hh) as usize];
        for y in 0..hh {
            let row_start = (y * padded) as usize;
            for x in 0..hw {
                let o = row_start + (x * 8) as usize;
                let mut px = [0.0f32; 4];
                for c in 0..4 {
                    let b = o + c * 2;
                    px[c] = f16_to_f32(u16::from_le_bytes([data[b], data[b + 1]]));
                }
                out[(y * hw + x) as usize] = px;
            }
        }
        out
    }

    /// Decoded bent normal at a world probe (from b,a of the GTAO output).
    fn bent_at(cam: &TestCamera, res: &SsaoResources, tex: &[[f32; 4]], world: Vec3) -> Vec3 {
        let (px, py) = cam.project_px(world);
        let (hx, hy) = (px / 2, py / 2);
        let (hw, _) = res.half_size;
        let t = tex[(hy as u32 * hw + hx as u32) as usize];
        TestCamera::oct_decode([t[2], t[3]])
    }

    /// THE SSAO readback gate. Renders the cube-over-floor G-buffer via the real
    /// raster path, runs the real 3-pass SSAO (default radius 0.5 / strength 0.7
    /// / bias 0.02), reads the factor texture back and asserts:
    ///   (a) open flat floor far from the cube ≥ 0.9 (no flat-surface acne),
    ///   (b) floor-cube contact corner visibly occluded ≤ 0.75,
    ///   (c) top-center of the cube's top face ≥ 0.9,
    ///   (d) two runs are byte-identical (deterministic, single-frame stable),
    ///   plus the zero-light ambient-crush guard: every open-surface probe
    ///   ≥ 0.5, so `ambient × AO ≥ ambient × 0.5` (the cube stays visible with
    ///   zero lights — the 17d regression class).
    /// Read an arbitrary Rgba16Float texture back as [r,g,b,a] per texel.
    fn read_rgba16f(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        w: u32,
        h: u32,
    ) -> Vec<[f32; 4]> {
        let row_bytes = w * 8;
        let padded = (row_bytes + 255) & !255;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rgba16f_read"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        rx.recv().expect("map_async channel").expect("map_async failed");
        let data = buf.slice(..).get_mapped_range();
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let row = &data[(y * padded) as usize..][..row_bytes as usize];
            for x in 0..w {
                let t = &row[(x * 8) as usize..][..8];
                out.push([
                    f16_to_f32(u16::from_le_bytes([t[0], t[1]])),
                    f16_to_f32(u16::from_le_bytes([t[2], t[3]])),
                    f16_to_f32(u16::from_le_bytes([t[4], t[5]])),
                    f16_to_f32(u16::from_le_bytes([t[6], t[7]])),
                ]);
            }
        }
        drop(data);
        buf.unmap();
        out
    }

    /// THE G-buffer depth-convention gate (2026-08-06).
    ///
    /// `gbuf_normal.w` must hold the ray parameter `t` along `camera_ray()` — the
    /// quantity every consumer reconstructs a world position with. In PERSPECTIVE
    /// that equals `|world - cam_pos|`; in ORTHOGRAPHIC it equals
    /// `dot(world - cam_pos, forward)`, because `camera_ray()` shifts the origin
    /// per pixel and keeps `dir == forward`.
    ///
    /// The raster shader wrote `length(rel)` unconditionally until 2026-08-06.
    /// Perspective was unaffected (the two are identical), so nothing caught it;
    /// orthographic silently broke every screen-space consumer — SSR's hit test
    /// `ssr_metric(p) - sample_depth > 0` could never fire, so ortho reflections
    /// were ~zero, and SSAO compared a Euclidean distance against an ortho depth.
    ///
    /// This test asserts BOTH branches, and asserts the two conventions actually
    /// differ at the probe — otherwise it would pass against the old shader too.
    #[test]
    fn gbuffer_depth_is_ray_parameter_in_both_projections() {
        let Some((device, queue, _)) = try_device(false) else {
            eprintln!("gbuffer_depth: no GPU adapter — skipped");
            return;
        };
        let eye = Vec3::new(4.0, 3.0, 6.0);
        let target = Vec3::new(0.0, 0.8, 0.0);

        // Probes differ per projection because the two frusta cover different
        // ground: the 55-degree perspective view is narrower than the ortho box,
        // so the far-off-axis probe the ortho discriminator needs falls outside it.
        // Both sit on the 24x24 floor, clear of the cube footprint, in front of
        // the near plane, with nothing between them and the camera.
        for ortho in [false, true] {
            let (cam, probe) = if ortho {
                (
                    // half_h = orbit distance, matching render::camera::from_orbit.
                    TestCamera::new_ortho(eye, target, (target - eye).length(), 512, 512),
                    Vec3::new(4.0, 0.0, 0.0),
                )
            } else {
                (TestCamera::new(eye, target, 55.0, 512, 512), Vec3::new(2.0, 0.0, 0.0))
            };
            let gbuf = render_gbuffer(&device, &queue, &cam);
            let texels = read_rgba16f(&device, &queue, &gbuf, cam.width, cam.height);

            let (px, py) = cam.project_px(probe);
            let t = texels[(py as u32 * cam.width + px as u32) as usize];
            let read_depth = t[3];

            // Evaluate the invariant at the pixel the rasterizer actually shaded,
            // not at the analytic probe: project_px rounds to a pixel, and on a
            // grazing floor half a pixel is ~0.05 world units of depth. Reconstruct
            // that pixel's ray exactly as camera_ray() does, intersect the floor
            // (y = 0), and the ray parameter of the hit IS the depth the shader
            // must have written — that is the whole contract, stated directly.
            let (o, d) = cam.camera_ray_px(px, py);
            let t_floor = -o.y / d.y;
            let p_true = o + d * t_floor;
            let expected = cam.expected_depth(p_true);
            let euclid = (p_true - eye).length();
            let forward_d = (p_true - eye).dot(cam.forward);

            eprintln!(
                "[gbuf depth] ortho={ortho} px=({px},{py}) read={read_depth:.4} \
                 expected={expected:.4} euclid={euclid:.4} forward={forward_d:.4}"
            );

            // The probe must actually be a geometry pixel, not background.
            assert!(read_depth > 0.0, "probe hit background (depth sentinel 0.0)");
            // CPU replica of gbuf_depth() must agree with the ray parameter of the
            // reconstructed hit — if these disagree the test itself is wrong.
            assert!(
                (expected - t_floor).abs() < 1e-3,
                "test bug: expected_depth {expected} != ray parameter {t_floor}"
            );
            // f16 at this range has ~0.004 resolution; 0.02 is comfortably tight.
            assert!(
                (read_depth - expected).abs() < 0.02,
                "ortho={ortho}: gbuf depth {read_depth} != ray parameter {expected}"
            );

            if ortho {
                // Discriminator: prove the test would FAIL against the old shader,
                // which wrote the Euclidean distance here.
                assert!(
                    (euclid - forward_d).abs() > 0.3,
                    "probe too close to the view axis to discriminate the two conventions \
                     (euclid={euclid}, forward={forward_d}) — move it further off-axis"
                );
                assert!(
                    (read_depth - euclid).abs() > 0.3,
                    "ortho gbuf depth still matches the Euclidean distance — the \
                     gbuf_depth() ortho branch is not taking effect"
                );
            } else {
                // Perspective: the two conventions coincide by construction, so the
                // only thing to assert is that this path did NOT change.
                assert!(
                    (read_depth - euclid).abs() < 0.02,
                    "perspective depth changed: {read_depth} != {euclid}"
                );
            }
        }
    }

    #[test]
    fn ssao_readback_cube_over_floor() {
        let Some((device, queue, _)) = try_device(false) else {
            eprintln!("ssao_readback: no GPU adapter — skipped");
            return;
        };
        let cam = TestCamera::new(Vec3::new(4.0, 3.0, 6.0), Vec3::new(0.0, 0.8, 0.0), 55.0, 512, 512);
        let gbuf_normal = render_gbuffer(&device, &queue, &cam);
        let gn_view = gbuf_normal.create_view(&Default::default());

        let pipelines = SsaoPipelines::new(&device, None);
        let mut res = SsaoResources::new(&device, &queue, cam.width, cam.height);
        let params = cam.ssao_params(0.5, 0.7);

        let run = |res: &mut SsaoResources| {
            let mut enc = device.create_command_encoder(&Default::default());
            dispatch(&mut enc, &device, &queue, &pipelines, res, &gn_view, &params, AoMode::Ssao, None, None, None);
            queue.submit([enc.finish()]);
        };
        run(&mut res);
        let (ao, raw1) = read_ao(&device, &queue, &res);

        // (a) Open floor, ≥ 1.5 world units clear of the cube footprint.
        let floor_open = ao_at(&cam, &res, &ao, Vec3::new(3.0, 0.0, 2.5));
        // (b) Floor-cube contact: 0.08 in front of the +z face (camera-visible).
        let contact = ao_at(&cam, &res, &ao, Vec3::new(0.0, 0.0, 1.08));
        // (c) Top-center of the cube's top face.
        let top_center = ao_at(&cam, &res, &ao, Vec3::new(0.0, 2.0, 0.0));
        // Ambient-crush guard extra probe: middle of the open +x side face.
        let side_face = ao_at(&cam, &res, &ao, Vec3::new(1.0, 1.0, 0.0));

        eprintln!(
            "[SSAO readback] floor_open={floor_open:.3} contact={contact:.3} top_center={top_center:.3} side_face={side_face:.3}"
        );
        assert!(floor_open >= 0.9, "(a) flat open floor self-darkens (acne class): {floor_open}");
        assert!(contact <= 0.75, "(b) floor-cube contact not visibly occluded: {contact}");
        assert!(top_center >= 0.9, "(c) open top face darkened: {top_center}");
        // Zero-light visibility: ambient × AO must stay ≥ ambient × 0.5 on
        // every open face (AO is the only ambient multiplier for raster pixels).
        for (name, v) in [("floor_open", floor_open), ("top_center", top_center), ("side_face", side_face)] {
            assert!(v >= 0.5, "ambient-crush guard: open probe {name} = {v} < 0.5");
        }

        // (d) Determinism: run the full pass chain again — byte-identical.
        run(&mut res);
        let (_, raw2) = read_ao(&device, &queue, &res);
        assert_eq!(raw1, raw2, "(d) SSAO output not deterministic across runs");
    }

    /// THE GTAO readback gate. Same cube-over-floor G-buffer, GTAO tier. Asserts
    /// the AO factor behaves (open floor clean, contact darkened) AND the two
    /// load-bearing bent-normal properties:
    ///   (a) open floor far from the cube: AO ≥ 0.9, bent ≈ geometric normal.
    ///   (b) floor-cube contact: AO occluded (≤ 0.75), and the bent normal is
    ///       BENT AWAY from the cube — dot(bent, toward_cube) < dot(geo_n,
    ///       toward_cube). This is what makes IBL diffuse read directional.
    ///   (c) a cube face turned away from an occluder darkens vs an open face.
    ///   (d) determinism (byte-identical across runs).
    /// Plus the two-face directional-variation proof (the flat-cube fix): the
    /// bent normals of two differently-exposed cube faces differ.
    #[test]
    fn gtao_readback_cube_over_floor() {
        let Some((device, queue, _)) = try_device(false) else {
            eprintln!("gtao_readback: no GPU adapter — skipped");
            return;
        };
        let cam = TestCamera::new(Vec3::new(4.0, 3.0, 6.0), Vec3::new(0.0, 0.8, 0.0), 55.0, 512, 512);
        let gbuf_normal = render_gbuffer(&device, &queue, &cam);
        let gn_view = gbuf_normal.create_view(&Default::default());

        let pipelines = SsaoPipelines::new(&device, None);
        let mut res = SsaoResources::new(&device, &queue, cam.width, cam.height);
        let params = cam.ssao_params(0.6, 1.0);

        let run = |res: &mut SsaoResources| {
            let mut enc = device.create_command_encoder(&Default::default());
            dispatch(&mut enc, &device, &queue, &pipelines, res, &gn_view, &params, AoMode::Gtao, None, None, None);
            queue.submit([enc.finish()]);
        };
        run(&mut res);
        let ao: Vec<f32> = read_ao(&device, &queue, &res).0;
        let rgba = read_ao_rgba(&device, &queue, &res);

        // ── AO factor ──
        let floor_open = ao_at(&cam, &res, &ao, Vec3::new(3.0, 0.0, 2.5));
        let contact = ao_at(&cam, &res, &ao, Vec3::new(0.0, 0.0, 1.10));
        eprintln!("[GTAO readback] floor_open={floor_open:.3} contact={contact:.3}");
        assert!(floor_open >= 0.9, "(a) flat open floor self-darkens (acne): {floor_open}");
        assert!(contact <= 0.75, "(b) floor-cube contact not occluded: {contact}");

        // ── (a) bent ≈ geometric normal on the open floor (geo = +Y) ──
        let bent_open = bent_at(&cam, &res, &rgba, Vec3::new(3.0, 0.0, 2.5));
        let floor_align = bent_open.dot(Vec3::Y);
        eprintln!("[GTAO bent] floor_open·Y={floor_align:.3} bent_open={bent_open:?}");
        assert!(floor_align >= 0.9, "(a) open-floor bent normal not ≈ geometric +Y: {floor_align}");

        // ── (b) bent normal bends AWAY from the cube at the contact ──
        // Contact point just in front of the cube's +Z face; the cube lies in
        // the -Z direction from the probe → toward_cube = -Z.
        let contact_p = Vec3::new(0.0, 0.0, 1.10);
        let toward_cube = Vec3::new(0.0, 0.0, -1.0);
        let bent_contact = bent_at(&cam, &res, &rgba, contact_p);
        let geo_n = Vec3::Y; // floor normal
        let bent_dot = bent_contact.dot(toward_cube);
        let geo_dot = geo_n.dot(toward_cube);
        eprintln!("[GTAO bent] contact bent·toward_cube={bent_dot:.3} geo·toward_cube={geo_dot:.3} bent={bent_contact:?}");
        assert!(
            bent_dot < geo_dot - 1e-3,
            "(b) bent normal not bent away from occluder: bent·tc={bent_dot} !< geo·tc={geo_dot}"
        );

        // ── (c)+directional proof: two cube faces with different exposure ──
        // The +Z face (faces the open camera side) vs the -X face. Probe each
        // near the floor contact where occlusion differs, and prove their bent
        // normals differ (directional self-shading — the flat-cube fix).
        let bent_pz = bent_at(&cam, &res, &rgba, Vec3::new(0.0, 0.25, 1.0)); // +Z face, near base
        let bent_px = bent_at(&cam, &res, &rgba, Vec3::new(1.0, 0.25, 0.0)); // +X face, near base
        let face_ao_pz = ao_at(&cam, &res, &ao, Vec3::new(0.0, 1.6, 1.0)); // +Z upper (open)
        let face_ao_base = ao_at(&cam, &res, &ao, Vec3::new(0.0, 0.15, 1.0)); // +Z base (occluded by floor)
        let face_variation = (bent_pz - bent_px).length();
        eprintln!(
            "[GTAO bent] +Zface bent={bent_pz:?} +Xface bent={bent_px:?} |Δ|={face_variation:.3} ao_open={face_ao_pz:.3} ao_base={face_ao_base:.3}"
        );
        assert!(
            face_variation > 0.05,
            "(c) two cube faces show no bent-normal variation (flat look): |Δ|={face_variation}"
        );
        assert!(
            face_ao_base <= face_ao_pz + 1e-3,
            "(c) cube base (floor-occluded) not darker than open face: base={face_ao_base} open={face_ao_pz}"
        );

        // ── (d) determinism ──
        run(&mut res);
        let rgba2 = read_ao_rgba(&device, &queue, &res);
        for (i, (a, b)) in rgba.iter().zip(rgba2.iter()).enumerate() {
            assert_eq!(a, b, "(d) GTAO output not deterministic at texel {i}");
        }
    }

    /// GPU timing at a 1080p-class viewport (half-res 960×540). Reported, and
    /// gated loosely (perf target on the owner's 4090 is ≤ 1.5 ms; the loose
    /// bound only catches pathological regressions on any adapter).
    /// Shared 1080p timing driver for both AO tiers (whole 3-pass chain via a
    /// begin-on-main / end-on-blur_v timestamp span). Returns ms, or None when
    /// no adapter / no timestamp support.
    fn time_ao_1080p(mode: AoMode) -> Option<f32> {
        let (device, queue, has_ts) = try_device(true)?;
        if !has_ts {
            eprintln!("ao_timing: TIMESTAMP_QUERY unavailable — skipped");
            return None;
        }
        let cam = TestCamera::new(Vec3::new(4.0, 3.0, 6.0), Vec3::new(0.0, 0.8, 0.0), 55.0, 1920, 1080);
        let gbuf_normal = render_gbuffer(&device, &queue, &cam);
        let gn_view = gbuf_normal.create_view(&Default::default());
        let pipelines = SsaoPipelines::new(&device, None);
        let mut res = SsaoResources::new(&device, &queue, cam.width, cam.height);
        let params = cam.ssao_params(0.5, 0.7);

        let qs = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("ao_timing_qs"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ao_timing_resolve"),
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ao_timing_staging"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // Warm once (bind group creation etc.), then time.
        let mut enc = device.create_command_encoder(&Default::default());
        dispatch(&mut enc, &device, &queue, &pipelines, &mut res, &gn_view, &params, mode, None, None, None);
        queue.submit([enc.finish()]);
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");

        let mut enc = device.create_command_encoder(&Default::default());
        dispatch(
            &mut enc, &device, &queue, &pipelines, &mut res, &gn_view, &params, mode,
            Some(wgpu::ComputePassTimestampWrites {
                query_set: &qs,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: None,
            }),
            None,
            Some(wgpu::ComputePassTimestampWrites {
                query_set: &qs,
                beginning_of_pass_write_index: None,
                end_of_pass_write_index: Some(1),
            }),
        );
        enc.resolve_query_set(&qs, 0..2, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &staging, 0, 16);
        queue.submit([enc.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        rx.recv().expect("map channel").expect("map failed");
        let data = staging.slice(..).get_mapped_range();
        let ts: &[u64] = bytemuck::cast_slice(&data);
        let period = queue.get_timestamp_period() as f64;
        Some(((ts[1].saturating_sub(ts[0])) as f64 * period / 1_000_000.0) as f32)
    }

    #[test]
    fn ssao_gpu_timing_1080p() {
        let Some(ms) = time_ao_1080p(AoMode::Ssao) else {
            eprintln!("ssao_timing: skipped");
            return;
        };
        eprintln!("[SSAO timing] 1920x1080 (half 960x540), 3 passes: {ms:.3} ms");
        assert!(ms < 15.0, "SSAO pathologically slow: {ms} ms at 1080p");
    }

    #[test]
    fn gtao_gpu_timing_1080p() {
        let Some(ms) = time_ao_1080p(AoMode::Gtao) else {
            eprintln!("gtao_timing: skipped");
            return;
        };
        eprintln!("[GTAO timing] 1920x1080 (half 960x540), 3 passes: {ms:.3} ms");
        // GTAO is pricier than SSAO (directional slices) but half-res keeps it
        // well under budget; the loose bound only catches pathological cases.
        assert!(ms < 20.0, "GTAO pathologically slow: {ms} ms at 1080p");
    }
}
