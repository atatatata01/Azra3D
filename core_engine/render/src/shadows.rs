use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};

use crate::mesh_gpu::{InstanceData, RasterVertex};

// ============================================================
// Constants
// ============================================================

/// Number of cascades in the shadow map.
pub const NUM_CASCADES: usize = 3;
/// Default per-cascade shadow map resolution. The live resolution is
/// `ShadowMapSystem::resolution` (owner "Shadow Map Resolution" quality control:
/// 1024 / 2048 / 4096 / 8192); this constant only seeds it.
pub const DEFAULT_SHADOW_MAP_SIZE: u32 = 1024;
/// Allowed per-cascade resolutions (UI combo + poll clamp).
pub const SHADOW_MAP_RESOLUTIONS: [u32; 4] = [1024, 2048, 4096, 8192];
/// Default PCF radius in shadow texels (owner "Shadow Softness" control, 0–3).
/// 0 = hard 1-tap; 0.5 reproduces the classic tight 2x2 kernel.
pub const DEFAULT_PCF_RADIUS_TEXELS: f32 = 0.5;

/// PSSM "practical split" blend factor: 0 = uniform, 1 = pure logarithmic.
/// 0.85 leans logarithmic, which is what puts texels where the subject is.
const CASCADE_SPLIT_LAMBDA: f32 = 0.85;

/// Cascade split fractions of `[near, far]`, computed per frame by
/// `cascade_split_fractions()`.
///
/// This REPLACED a fixed `[0.0, 0.08, 0.3, 1.0]` on 2026-08-06. Those numbers were
/// hand-tuned against a far plane hard-floored at 500 world units; once the cascade
/// far plane became scene-fitted (`raster_gbuffer.rs`, same day), 8% of a ~17-unit
/// range put cascade 0 entirely in front of the subject and pushed a cube at 3 units
/// into cascade 1 — measurably coarser. Fixed fractions cannot serve both a 500-unit
/// and a 17-unit range; a near/far-relative split can.
fn cascade_split_fractions(near: f32, far: f32) -> [f32; NUM_CASCADES + 1] {
    let n = NUM_CASCADES as f32;
    let near = near.max(1e-4);
    let far = far.max(near * 1.001);
    let ratio = far / near;
    let mut out = [0.0f32; NUM_CASCADES + 1];
    out[NUM_CASCADES] = 1.0;
    for i in 1..NUM_CASCADES {
        let f = i as f32 / n;
        let log_d = near * ratio.powf(f);
        let uni_d = near + (far - near) * f;
        let d = CASCADE_SPLIT_LAMBDA * log_d + (1.0 - CASCADE_SPLIT_LAMBDA) * uni_d;
        // Back to a [0,1] fraction of [near, far], which is what the slice lerp wants.
        out[i] = ((d - near) / (far - near)).clamp(0.0, 1.0);
    }
    out
}

// ============================================================
// Light View Uniform (GPU)
// ============================================================

/// Uniform sent to the shadow vertex shader.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LightViewUniforms {
    pub light_view_proj: [[f32; 4]; 4],
}

/// Live-tunable receiver-bias controls ("Sun Shadows → Bias" sliders).
///
/// These exist because the correct values are scene- and scale-dependent, and three
/// successive hand-picked constants each shipped a visible artifact (2026-08-06). They
/// ride previously-unused padding lanes of [`ShadowUniforms`], so the uniform's size and
/// layout are BYTE-IDENTICAL and no bind group or size test changes.
///
/// The two kinds are not interchangeable, and that distinction is the whole point:
/// - `normal_offset_texels` moves the sample POSITION along the surface normal. It is
///   effective against acne but it also MOVES THE SHADOW BOUNDARY along the surface —
///   that is peter-panning, and it is why the owner saw the shadow "not landing in the
///   right position". Raise it only until acne clears.
/// - `depth_bias` / `slope_depth_bias` shift the compared DEPTH instead, so they fight
///   acne WITHOUT moving the boundary. Prefer these. The slope term is the sanctioned
///   home for grazing-angle compensation (see the reverted normal-offset slope scale in
///   `normal_offset_texels`' doc comment in gbuffer.wgsl).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowBias {
    /// Receiver offset along the normal, in TEXELS, ADDED to the PCF radius.
    /// Default 1.0 reproduces the historical hardcoded `pcf_radius + 1`.
    pub normal_offset_texels: f32,
    /// Constant bias subtracted from the reference depth, in TEXELS of the selected
    /// cascade. `sample_shadow_cascade` converts texels → NDC z.
    ///
    /// Units are texels and NOT raw NDC deliberately: the cascade's ortho depth window
    /// carries ~50 world units of caster headroom, so receivers occupy a band only
    /// ~0.03 NDC wide and an NDC-denominated slider would have all of its useful travel
    /// in the first ~1% (caught in adversarial review, 2026-08-06, before shipping).
    pub depth_bias_texels: f32,
    /// Additional depth bias in TEXELS, multiplied by tan(angle between surface and
    /// light). Targets grazing angles only.
    pub slope_depth_bias_texels: f32,
}

impl Default for ShadowBias {
    /// The historical behaviour exactly: `pcf_radius + 1` texels of normal offset and
    /// no depth bias. Defaults MUST stay neutral — `shadow_bias_defaults_are_neutral`
    /// pins this so the sliders can never silently become a behaviour change.
    fn default() -> Self {
        Self { normal_offset_texels: 1.0, depth_bias_texels: 0.0, slope_depth_bias_texels: 0.0 }
    }
}

/// Shadow data passed to the main G-buffer shader for sampling.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ShadowUniforms {
    /// Light view-projection matrices for each cascade.
    pub cascade_vp: [[[f32; 4]; 4]; NUM_CASCADES],
    /// Cascade split distances in view-space (z values).
    /// xyz = split distances (view-space z); **w = ShadowBias::slope_depth_bias_texels**.
    pub cascade_splits: [f32; 4],
    /// World-space size of one shadow-map texel per cascade; **w = ShadowBias::depth_bias_texels**.
    /// The G-buffer shader scales its normal-offset (anti-acne) bias by this, so the
    /// bias adapts to cascade footprint instead of being a hand-tuned world constant.
    pub texel_world: [f32; 4],
    /// x: 1.0 = a sun (visible directional light) exists and shadows apply; 0.0 = no
    /// sun → `compute_shadow()` returns 1.0 WITHOUT sampling anything. This is the
    /// structural gate for light deletion (round-4 zero-light artifact): when the last
    /// sun is deleted, `light_config` retains the dead light's direction (its sync
    /// early-outs with zero lights), so without this flag the shadow chain kept baking
    /// a stale directional shadow into gbuf_material.z — which the deferred pass
    /// multiplied into the AMBIENT term, dark-banding every face turned away from a
    /// light that no longer existed. **yzw = direction TO the sun** — read by
    /// gbuffer.wgsl's `shadow_depth_bias()` for its slope term.
    pub flags: [f32; 4],
    /// Shadow-quality params (2026-07-17f). x = per-cascade map resolution in texels
    /// (the shader derives `texel_size = 1/x` — never a hardcoded map size); y = PCF
    /// radius in TEXELS ("Shadow Softness", 0.5 = the classic 2x2 kernel). The
    /// receiver normal-offset anti-acne bias is DERIVED from y (radius + 1 texel of
    /// caster depth quantization), so widening the penumbra can never reintroduce
    /// acne — one derivation, no second magic constant.
    /// **z = PCF rotation** (progressive refine); **w = ShadowBias::normal_offset_texels**.
    pub params: [f32; 4],
}

// ============================================================
// Shadow Map System
// ============================================================

/// Manages cascaded shadow maps for the rasterization pipeline.
pub struct ShadowMapSystem {
    /// Depth textures, one per cascade.
    pub depth_textures: [Option<wgpu::Texture>; NUM_CASCADES],
    pub depth_views: [Option<wgpu::TextureView>; NUM_CASCADES],
    /// Shadow depth-only render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    /// Per-cascade light uniform buffer.
    pub light_buffers: [wgpu::Buffer; NUM_CASCADES],
    pub bind_groups: [Option<wgpu::BindGroup>; NUM_CASCADES],
    /// Shadow uniform buffer (cascade VPs + splits) for the main shader.
    pub shadow_uniform_buffer: wgpu::Buffer,
    /// Whether shadow maps have been created.
    pub initialized: bool,
    /// Per-cascade map resolution (1024 / 2048 / 4096). Changed ONLY via
    /// `set_resolution()`, which is called ONLY from `poll_background_tasks`
    /// (Frame Ordering Contract — no texture recreation in panel callbacks or
    /// the render phase).
    pub resolution: u32,
    /// Generation counter, bumped on every texture recreation (`set_resolution`).
    /// Consumers holding bind groups over `depth_views` (RasterPipeline's
    /// `shadow_bind_group`) compare their cached generation and rebuild on
    /// mismatch — GPU Resource Lifetime rule: generation counters, never timing.
    pub generation: u64,
    /// Group(1) layout for the caster's material displacement lookup (materials
    /// storage + texture array + sampler, all VERTEX-visible). The shadow pipeline
    /// binds it so `vs_shadow` can apply the SAME per-vertex displacement as the
    /// G-buffer — a displaced surface must cast a displaced shadow (2026-07-18).
    pub material_bgl: wgpu::BindGroupLayout,
    /// Bind group over the shared raster material resources (RasterPipeline's
    /// `material_buffer` + the app's `MaterialTextureArray` view/sampler). Rebuilt
    /// by `rebuild_material_bind_group` whenever the raster material bind group is
    /// rebuilt (same trigger — texture regrow or material-buffer realloc), so the
    /// caster never references a freed buffer/texture.
    pub material_bind_group: Option<wgpu::BindGroup>,
}

impl ShadowMapSystem {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shadow.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // Group(1): material resources for the caster's displacement lookup. Storage +
        // texture + sampler are ALL vertex-visible (the sample happens in vs_shadow). Mirrors
        // the raster material bindings (pipeline.rs group 0 bindings 1..3), re-homed to group 1.
        let material_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow_material_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow_layout"),
            bind_group_layouts: &[&bgl, &material_bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow_depth"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_shadow"),
                buffers: &[RasterVertex::LAYOUT, InstanceData::LAYOUT],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                // Render both faces for shadow maps to avoid light leaking
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: None, // depth-only
            multiview: None,
            cache: None,
        });

        let make_buf = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<LightViewUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };

        let light_buffers = [
            make_buf("shadow_cascade_0"),
            make_buf("shadow_cascade_1"),
            make_buf("shadow_cascade_2"),
        ];

        let shadow_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow_uniforms"),
            size: std::mem::size_of::<ShadowUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            depth_textures: [None, None, None],
            depth_views: [None, None, None],
            pipeline,
            bind_group_layout: bgl,
            light_buffers,
            bind_groups: [None, None, None],
            shadow_uniform_buffer,
            initialized: false,
            resolution: DEFAULT_SHADOW_MAP_SIZE,
            generation: 0,
            material_bgl,
            material_bind_group: None,
        }
    }

    /// (Re)build the caster's group(1) material bind group from the shared raster material
    /// resources. Call whenever the raster material bind group is rebuilt — same trigger
    /// (material texture regrow OR material-buffer realloc) — so the caster never binds a
    /// freed buffer/texture. The shadow pipeline REQUIRES group(1); this must be `Some`
    /// before any cascade draw (the render loop rebuilds it right after the raster bind group).
    pub fn rebuild_material_bind_group(
        &mut self,
        device: &wgpu::Device,
        material_buffer: &wgpu::Buffer,
        mat_tex_view: &wgpu::TextureView,
        mat_tex_sampler: &wgpu::Sampler,
    ) {
        self.material_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow_material_bg"),
            layout: &self.material_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: material_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(mat_tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(mat_tex_sampler),
                },
            ],
        }));
    }

    /// Create shadow map textures at `self.resolution` (first use only — a no-op once
    /// initialized). Resolution CHANGES go through `set_resolution()` from poll.
    pub fn ensure_textures(&mut self, device: &wgpu::Device) {
        if self.initialized { return; }
        self.create_maps(device);
        self.initialized = true;
    }

    /// Apply a new per-cascade map resolution: destroys + recreates the 3 cascade depth
    /// textures/views and bumps `generation` so every bind group referencing the old
    /// views (RasterPipeline `shadow_bind_group`) rebuilds on its next generation check.
    ///
    /// FRAME ORDERING CONTRACT: call ONLY from `poll_background_tasks` — never from a
    /// panel callback (the UI just writes `RenderSettings.shadow_map_resolution`; poll
    /// compares and applies) and never during the render phase.
    ///
    /// No-op when the resolution is unchanged. VRAM at D32: 3 cascades × res² × 4 B =
    /// 12 MB (1024) / 48 MB (2048) / 192 MB (4096) / 768 MB (8192).
    pub fn set_resolution(&mut self, device: &wgpu::Device, resolution: u32) {
        // Snap to a known tier so a stale/corrupt value never allocates a weird size.
        let resolution = SHADOW_MAP_RESOLUTIONS
            .into_iter()
            .find(|&r| r == resolution)
            .unwrap_or_else(|| {
                // Nearest allowed tier (clamp into the set).
                *SHADOW_MAP_RESOLUTIONS
                    .iter()
                    .min_by_key(|&&r| (r as i64 - resolution as i64).unsigned_abs())
                    .unwrap_or(&DEFAULT_SHADOW_MAP_SIZE)
            });
        if resolution == self.resolution {
            return;
        }
        self.resolution = resolution;
        if !self.initialized {
            // First frame hasn't created the maps yet — they'll be created lazily at
            // the new resolution; nothing references the (nonexistent) old views.
            return;
        }
        self.create_maps(device);
        self.generation += 1;
    }

    /// (Re)create the 3 cascade depth textures + views + per-cascade bind groups at
    /// `self.resolution`. Shared by first-use init and `set_resolution`.
    fn create_maps(&mut self, device: &wgpu::Device) {
        for i in 0..NUM_CASCADES {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("shadow_cascade_{}", i)),
                size: wgpu::Extent3d {
                    width: self.resolution,
                    height: self.resolution,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.depth_views[i] = Some(tex.create_view(&Default::default()));
            self.depth_textures[i] = Some(tex);

            self.bind_groups[i] = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("shadow_bg_{}", i)),
                layout: &self.bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.light_buffers[i].as_entire_binding(),
                }],
            }));
        }
    }

    /// Compute cascade light view-projection matrices and upload to GPU.
    ///
    /// `sun_enabled` MUST reflect whether a visible directional light currently exists in
    /// the scene (derived from the scene graph each frame — never cached). With
    /// `sun_enabled == false` the uniforms carry flags.x = 0.0 and `compute_shadow()` in
    /// gbuffer.wgsl returns 1.0 unconditionally — a deleted sun structurally disables the
    /// shadow multiply instead of sampling whatever stale direction `light_config` retains.
    ///
    /// `pcf_radius_texels` = the "Shadow Softness" control (0.5–3.0): the PCF footprint
    /// radius in texels of the selected cascade's map. The shader derives its anti-acne
    /// normal-offset bias from it (radius + 1 texel), so no caller-side retuning is ever
    /// needed when the footprint changes.
    ///
    /// Returns the `ShadowUniforms` that the main G-buffer shader needs.
    /// `pcf_rotation` = per-frame rotation (radians) of the PCF tap pattern
    /// (progressive-refine jitter, PROGRESSIVE_PBR_PLAN.md Phase 2). 0 = the
    /// static pattern — bit-identical to the pre-rotation shader path. Only
    /// nonzero while the idle accumulator is averaging frames, so the rotating
    /// taps converge to a filled penumbra disc instead of boiling.
    pub fn update_cascades(
        &self,
        queue: &wgpu::Queue,
        camera_view: Mat4,
        camera_proj: Mat4,
        dir_to_sun: Vec3,
        near: f32,
        far: f32,
        sun_enabled: bool,
        pcf_radius_texels: f32,
        pcf_rotation: f32,
        bias: ShadowBias,
    ) -> ShadowUniforms {
        let (cascade_vps, split_depths, texel_world) =
            compute_cascade_vps(camera_view, camera_proj, dir_to_sun, near, far, self.resolution);

        for c in 0..NUM_CASCADES {
            // Upload per-cascade uniform
            let uniform = LightViewUniforms {
                light_view_proj: cascade_vps[c],
            };
            queue.write_buffer(&self.light_buffers[c], 0, bytemuck::bytes_of(&uniform));
        }

        // The three previously-unused padding lanes now carry the live bias tweaks
        // (ShadowBias). Same size, same offsets — nothing else about the layout moves.
        let mut cascade_splits = split_depths;
        cascade_splits[3] = bias.slope_depth_bias_texels;
        let mut texel_world = texel_world;
        texel_world[3] = bias.depth_bias_texels;

        let shadow_uniforms = ShadowUniforms {
            cascade_vp: cascade_vps,
            // xyz = per-cascade split depths; w = slope-scaled depth bias.
            cascade_splits,
            // xyz = per-cascade texel world size; w = constant depth bias.
            texel_world,
            // yzw = direction TO the sun. Read by the slope-scaled DEPTH bias in
            // gbuffer.wgsl's `shadow_depth_bias()`. Note this is deliberately the depth
            // bias and NOT the normal offset: scaling the normal offset by slope was
            // tried on 2026-08-06 and reverted as a visual regression (it moves the
            // sample position, smearing the boundary). Repro for both:
            // docs/planning/render/repro/shadow-acne.fl4
            flags: [
                if sun_enabled { 1.0 } else { 0.0 },
                dir_to_sun.x,
                dir_to_sun.y,
                dir_to_sun.z,
            ],
            // z = PCF rotation (progressive refine); w = normal offset in texels.
            params: [
                self.resolution as f32,
                pcf_radius_texels,
                pcf_rotation,
                bias.normal_offset_texels,
            ],
        };
        queue.write_buffer(&self.shadow_uniform_buffer, 0, bytemuck::bytes_of(&shadow_uniforms));

        shadow_uniforms
    }
}

// ============================================================
// Cascade Math (pure — unit-testable without a GPU)
// ============================================================

/// Compute the light view-projection matrix + view-space split depth for every cascade.
/// Pure function extracted from `update_cascades` so the depth mapping is unit-testable.
///
/// DEPTH CONVENTION (the black-PBR root cause, fixed 2026-07-17): glam's
/// `Mat4::orthographic_rh(l, r, b, t, near, far)` expects `near`/`far` as POSITIVE
/// distances along -Z of the light view. The old code passed the raw light-view z
/// coordinates (`min_ls.z`, `max_ls.z`, both negative in front of the light) directly,
/// which mapped every caster AND receiver to NDC depth ≈ 2.6–3.6 — outside [0,1]. All
/// casters were clipped from the shadow maps and every receiver's reference depth failed
/// the `LessEqual` compare against the cleared 1.0 → `compute_shadow()` returned 0.0 for
/// ALL raster geometry inside the cascade range. That zero rode gbuf_material.z into the
/// deferred PBR/Lit paths, multiplying both direct light AND ambient to black (Solid mode
/// survived only via its 0.55 floor). Correct call: `-max_ls.z` (near), `-min_ls.z` (far).
/// Returns (cascade VPs, view-space split depths, per-cascade texel world size).
///
/// STABILITY (the shadow-acne/sawtooth fix, 2026-07-17):
/// 1. **Sphere fit, not AABB fit.** The ortho extent is the bounding SPHERE of the frustum
///    slice (±radius), not the light-space AABB of its corners. A rigid camera move (orbit,
///    pan, dolly at fixed near/far/fov) never changes the slice's shape, so the sphere
///    radius — and therefore the texel world-size — is invariant while navigating. An AABB
///    fit re-measures the slice in light space every frame, so its extent (and texel size)
///    pumps as the camera orbits → shimmering, crawling shadow edges.
/// 2. **Texel snapping.** The slice center is quantized to whole shadow-map texels in the
///    light's rotation frame before building the light view. Sub-texel camera motion then
///    produces the IDENTICAL cascade matrix, so shadow edges stay put instead of crawling
///    in staircase steps ("sawtooth blobs").
/// 3. The texel world-size per cascade is exported so the receiver-side normal-offset bias
///    in gbuffer.wgsl adapts to the cascade footprint (no magic world-space constant).
///
/// LIGHT-DIRECTION CONVENTION (the self-shadow-blob root cause, fixed 2026-07-17c):
/// `dir_to_sun` is the SCENE's native convention — the direction FROM the surface TOWARD
/// the sun, exactly what `LightConfig::direction()` / `LightData::direction()` return and
/// what every N·L shading term uses (default sun el=55° → y=+0.82, pointing UP). The
/// propagation direction the light frame needs is derived here, in ONE place. The old
/// signature took `light_dir` meaning propagation while the only production caller
/// (raster_gbuffer.rs) passed the to-sun vector unmodified → the shadow camera sat BELOW
/// the scene, so every object was self-shadowed by its own underside silhouette (the
/// smooth "blob hugging the corner" in PBR). The unit tests hand-wrote downward vectors,
/// which is why they stayed green — the GPU readback harness below pins the app's actual
/// default sun instead.
pub fn compute_cascade_vps(
    camera_view: Mat4,
    camera_proj: Mat4,
    dir_to_sun: Vec3,
    near: f32,
    far: f32,
    map_size: u32,
) -> ([[[f32; 4]; 4]; NUM_CASCADES], [f32; 4], [f32; 4]) {
    // Propagation direction (sun → scene), derived once from the scene convention.
    let light_dir = -dir_to_sun.normalize();
    let inv_view = camera_view.inverse();
    let inv_proj = camera_proj.inverse();

    let mut cascade_vps = [[[0.0f32; 4]; 4]; NUM_CASCADES];
    let mut split_depths = [0.0f32; 4];
    let mut texel_world = [0.0f32; 4];

    // Fallback up-vector when sun is near-vertical (avoids degenerate cross product)
    let light_up = if light_dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
    // Rotation-only light frame used for texel snapping (fixed origin — the snap grid must
    // not move with the slice center, or snapping would be a no-op).
    let light_rot = Mat4::look_at_rh(Vec3::ZERO, light_dir, light_up);
    let inv_light_rot = light_rot.inverse();

    let splits = cascade_split_fractions(near, far);
    for c in 0..NUM_CASCADES {
        // Fractions of [near, far] — use as lerp t directly
        let t_near = splits[c];
        let t_far = splits[c + 1];
        // Store absolute view-space split depth for the main shader
        split_depths[c] = near + (far - near) * t_far;

        // Compute frustum corners in world space for this cascade slice
        let corners = frustum_corners_world(inv_view, inv_proj, t_near, t_far);
        let center = corners.iter().fold(Vec3::ZERO, |a, b| a + *b) / 8.0;

        // Bounding-sphere radius of the slice (rotation-invariant — see doc comment).
        let radius = corners
            .iter()
            .map(|co| (*co - center).length())
            .fold(0.0f32, f32::max)
            .max(1e-3);
        // Quantize the radius (round UP to 1/16 world unit). Mathematically the sphere
        // radius is invariant under camera translation/orbit, but the unproject leaves
        // last-bit float noise; quantizing makes the texel size bit-stable frame to frame
        // so the snap grid below never drifts. The step value only sets quantization
        // granularity — behavior is insensitive to it.
        let radius = (radius * 16.0).ceil() / 16.0;
        let texel = 2.0 * radius / map_size as f32;
        texel_world[c] = texel;

        // Snap the slice center to whole texels in the light's rotation frame.
        let c_rs = light_rot.transform_point3(center);
        let snapped_rs = Vec3::new(
            (c_rs.x / texel).floor() * texel,
            (c_rs.y / texel).floor() * texel,
            // z snapped too: keeps the whole matrix bit-stable under sub-texel camera
            // motion (the depth window has 50 units of caster headroom — a ≤1-texel z
            // shift is covered).
            (c_rs.z / texel).floor() * texel,
        );
        let center = inv_light_rot.transform_point3(snapped_rs);

        let light_view = Mat4::look_at_rh(center - light_dir * 50.0, center, light_up);

        // Symmetric extents around the (snapped) center. +1 texel pad so the sphere stays
        // covered after the ≤1-texel snap displacement.
        let r = radius + texel;
        // Depth: the slice sphere sits at light-view z ∈ [-50-r, -50+r] (eye is 50 from
        // center). glam's orthographic_rh takes POSITIVE distances along -Z (the black-PBR
        // fix, see history above). near = -r extends the depth window 50+... behind the
        // slice toward the light (ortho allows a negative near) so casters between the
        // light and the slice still land in the map — same 50-unit caster headroom as
        // before, now expressed analytically. far = 50 + r covers the whole sphere.
        // Depth range = 50 + 2r on a Depth32Float target — precision is a non-issue.
        let light_proj = Mat4::orthographic_rh(-r, r, -r, r, -r, 50.0 + r);

        cascade_vps[c] = (light_proj * light_view).to_cols_array_2d();
    }

    (cascade_vps, split_depths, texel_world)
}

// ============================================================
// Frustum Corner Computation
// ============================================================

/// Compute the 8 world-space corners of a frustum slice between z_near and z_far.
fn frustum_corners_world(
    inv_view: Mat4,
    inv_proj: Mat4,
    z_near: f32,
    z_far: f32,
) -> [Vec3; 8] {
    // NDC corners: 4 near + 4 far
    // In right-handed NDC: z ranges from 0 (near) to 1 (far) in wgpu
    let inv_vp = inv_view * inv_proj;

    let mut corners = [Vec3::ZERO; 8];
    let ndc_corners = [
        // Near plane (z = 0 in wgpu NDC)
        Vec4::new(-1.0, -1.0, 0.0, 1.0),
        Vec4::new( 1.0, -1.0, 0.0, 1.0),
        Vec4::new(-1.0,  1.0, 0.0, 1.0),
        Vec4::new( 1.0,  1.0, 0.0, 1.0),
        // Far plane (z = 1 in wgpu NDC)
        Vec4::new(-1.0, -1.0, 1.0, 1.0),
        Vec4::new( 1.0, -1.0, 1.0, 1.0),
        Vec4::new(-1.0,  1.0, 1.0, 1.0),
        Vec4::new( 1.0,  1.0, 1.0, 1.0),
    ];

    // Unproject full frustum
    let mut full_corners = [Vec3::ZERO; 8];
    for (i, ndc) in ndc_corners.iter().enumerate() {
        let world = inv_vp * *ndc;
        full_corners[i] = world.truncate() / world.w;
    }

    // Lerp between near and far planes to get the cascade slice
    for i in 0..4 {
        let near_pt = full_corners[i];
        let far_pt = full_corners[i + 4];
        let ray = far_pt - near_pt;
        corners[i] = near_pt + ray * z_near; // fractional near
        corners[i + 4] = near_pt + ray * z_far; // fractional far
    }

    corners
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_view_uniforms_size() {
        assert_eq!(std::mem::size_of::<LightViewUniforms>(), 64);
    }

    #[test]
    fn shadow_uniforms_size() {
        assert_eq!(
            std::mem::size_of::<ShadowUniforms>(),
            NUM_CASCADES * 64 + 64, // 3 mat4 + 4 vec4 (splits + texel_world + flags + params)
        );
    }

    /// DRIFT GUARD (2026-07-18): shadow.wgsl copies the `MaterialData` struct + the three
    /// `sample_mat_*` helpers + the displacement amplitude formula from gbuffer.wgsl so that
    /// `vs_shadow` displaces EXACTLY like `vs_main`. If the G-buffer versions silently diverge,
    /// a displaced surface casts its shadow from the wrong (undisplaced) place. This pins the
    /// copies: extract each shared item from gbuffer.wgsl, strip comments + whitespace, and
    /// assert shadow.wgsl contains it verbatim. A real token change in gbuffer.wgsl fails this
    /// until shadow.wgsl is re-synced; pure reformatting does NOT trip it. (We can't share one
    /// source: render-crate shaders are `include_str!`'d standalone modules, and gbuffer.wgsl is
    /// deliberately not edited here — the shared-code path would need a build-time concat.)
    #[test]
    fn shadow_material_helpers_match_gbuffer() {
        let gbuf = include_str!("gbuffer.wgsl");
        let shadow = include_str!("shadow.wgsl");
        // Strip line comments (`// …`) then all whitespace — compares CODE TOKENS only.
        let strip = |s: &str| {
            s.lines()
                .map(|l| match l.find("//") { Some(i) => &l[..i], None => l })
                .collect::<String>()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        let shadow_stripped = strip(shadow);

        // Extract `[start … end]` (end inclusive) from gbuffer.wgsl.
        let extract = |start: &str, end: &str| -> String {
            let s = gbuf.find(start).unwrap_or_else(|| panic!("gbuffer.wgsl: {start:?} missing"));
            let e = gbuf[s..]
                .find(end)
                .map(|i| s + i + end.len())
                .unwrap_or_else(|| panic!("gbuffer.wgsl: {end:?} after {start:?} missing"));
            gbuf[s..e].to_string()
        };

        let items = [
            ("MaterialData struct", extract("struct MaterialData {", "};")),
            ("fn sample_mat_triplanar", extract("fn sample_mat_triplanar(", "\n}")),
            ("fn sample_mat_uv", extract("fn sample_mat_uv(", "\n}")),
            ("fn sample_mat_scalar", extract("fn sample_mat_scalar(", "\n}")),
            // The displacement amplitude formula (the one line the guard can't get from a shared
            // fn, since gbuffer applies it inline in vs_main — pin the literal in BOTH files).
            ("displacement amplitude", "(h - 0.5) * 2.0 * clamp(mat.displacement_map.y, 0.0, 4.0)".to_string()),
        ];
        for (name, body) in &items {
            let needle = strip(body);
            assert!(
                strip(gbuf).contains(&needle),
                "drift guard is itself stale: `{name}` no longer found in gbuffer.wgsl — update the guard AND shadow.wgsl"
            );
            assert!(
                shadow_stripped.contains(&needle),
                "shadow.wgsl DRIFTED from gbuffer.wgsl `{name}` — re-sync the copy in shadow.wgsl \
                 (the shadow caster must displace identically to the G-buffer, or shadows detach \
                 from displaced surfaces).\n--- expected (from gbuffer.wgsl) ---\n{body}"
            );
        }
    }

    /// Regression test for the black-PBR-viewport bug (fixed 2026-07-17).
    ///
    /// The cascade ortho projection passed raw light-view z coordinates as glam
    /// `orthographic_rh` near/far (which expects positive distances along -Z), mapping every
    /// caster/receiver to NDC depth outside [0,1]. Result: empty shadow maps + failing depth
    /// compares → compute_shadow() == 0 for all raster geometry → PBR/Lit shaded to black.
    ///
    /// Asserts: a world point inside each cascade's frustum slice projects through that
    /// cascade's light VP to NDC with depth in [0,1] (and XY in bounds for a point at the
    /// slice center) — i.e. the shadow map can actually contain it.
    #[test]
    fn cascade_depth_maps_into_unit_range() {
        // A representative viewport camera: eye slightly above origin looking at a body at origin.
        let eye = Vec3::new(4.0, 3.0, 6.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let (near, far) = (0.1_f32, 100.0_f32);
        let proj = Mat4::perspective_rh(45f32.to_radians(), 16.0 / 9.0, near, far);
        // Default-ish sun direction TOWARD the light (scene convention — up-back-right).
        let light_dir = Vec3::new(0.4, 0.8, 0.45); // dir_to_sun (scene convention)

        let (vps, splits, _texels) =
            compute_cascade_vps(view, proj, light_dir, near, far, DEFAULT_SHADOW_MAP_SIZE);
        assert!(splits[0] > near && splits[2] <= far + 1e-3);

        let inv_view = view.inverse();
        let inv_proj = proj.inverse();
        // Slice fractions must come from the production splitter, not a frozen copy:
        // hardcoding [0.0, 0.08, 0.3] here made this test compare corners from the OLD
        // fixed splits against view-projections built from the new near/far-relative
        // ones, which reported a false invariant break (2026-08-06).
        let frac = cascade_split_fractions(near, far);
        for c in 0..NUM_CASCADES {
            let t_near = frac[c];
            let t_far = frac[c + 1];
            let corners = frustum_corners_world(inv_view, inv_proj, t_near, t_far);
            let center = corners.iter().fold(Vec3::ZERO, |a, b| a + *b) / 8.0;
            let vp = Mat4::from_cols_array_2d(&vps[c]);

            // Slice center must land inside the shadow map with a valid depth.
            let clip = vp * center.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0,
                "cascade {c}: slice center off-map: {ndc:?}");
            assert!((0.0..=1.0).contains(&ndc.z),
                "cascade {c}: depth outside [0,1] (the black-PBR bug): {ndc:?}");

            // Every slice corner must have depth in [0,1] (they define the fitted box).
            for &corner in &corners {
                let clip = vp * corner.extend(1.0);
                let z = clip.z / clip.w;
                assert!((-1e-3..=1.0 + 1e-3).contains(&z),
                    "cascade {c}: corner depth outside [0,1]: {z}");
            }
        }
    }

    /// Bias-derivation sanity: texel world-size per cascade is in the expected range for
    /// the reference camera (far = 500, the app's raster floor), grows monotonically with
    /// cascade index, and the derived normal offset (pcf_radius + 1 texel) stays a tiny
    /// fraction of the cascade extent even at maximum softness (no peter-panning from
    /// over-biasing). Also proves the resolution control's payoff: at 2048 the texel
    /// world-size — and therefore the adaptive bias — halves with no retuning.
    /// THE CASCADE-FIT GATE (2026-08-06). Owner: *"WHY MY SHADOW ARE THAT UGLY ON A
    /// 8K SHADOW MAP ON A SIMPLE CUBE"*.
    ///
    /// 8192 was genuinely applied — the resolution was never the problem. The cascades
    /// were handed the CAMERA's render far plane, `(camera.distance * 20).max(500.0)`,
    /// whose 500 floor dominates every normally framed scene, and the split fractions
    /// were hand-tuned for that 500. Two coupled fixes landed together: a scene-fitted
    /// shadow far plane (`raster_gbuffer.rs`) and near/far-relative PSSM splits
    /// (`cascade_split_fractions`).
    ///
    /// The assertion is SCREEN PIXELS PER SHADOW TEXEL, not world units, because that is
    /// the quantity that decides whether an edge looks crisp or stepped. It is measured
    /// on the cascade that ACTUALLY serves the subject — tightening the far plane moves
    /// the splits, so the subject can change cascade, and asserting on cascade 0
    /// regardless would measure a cascade that is not in use. (Both of those mistakes
    /// were made and caught while writing this test.)
    #[test]
    fn cascade_fit_gives_subpixel_shadow_texels() {
        let subject_depth = 3.0_f32; // a ~2-unit cube inspected from ~3 units
        let view = Mat4::look_at_rh(Vec3::new(2.0, 1.5, 3.0), Vec3::ZERO, Vec3::Y);
        let near = 0.1_f32;
        let light_dir = Vec3::new(0.4, 0.8, 0.45);
        let fov_y = 90f32.to_radians();
        let viewport_h = 1440.0_f32;
        let mk_proj = |far: f32| Mat4::perspective_rh(fov_y, 16.0 / 9.0, near, far);

        // Screen pixels spanned by one shadow texel at the subject.
        let px_per_texel = |texel: f32| {
            texel * (viewport_h / 2.0) / (subject_depth * (fov_y / 2.0).tan())
        };
        // Which cascade compute_shadow() selects for the subject.
        let cascade_for = |far: f32| {
            let fr = cascade_split_fractions(near, far);
            (0..NUM_CASCADES)
                .find(|i| subject_depth < near + (far - near) * fr[i + 1])
                .unwrap_or(NUM_CASCADES - 1)
        };

        let old_far = 500.0_f32; // the .max(500.0) floor
        let new_far = 15.0_f32 * 1.05 + 1.0; // scene-fitted: furthest caster ~15 units

        for res in [1024u32, 8192] {
            let (_, _, old_t) = compute_cascade_vps(view, mk_proj(old_far), light_dir, near, old_far, res);
            let (_, _, new_t) = compute_cascade_vps(view, mk_proj(new_far), light_dir, near, new_far, res);
            let (oc, nc) = (cascade_for(old_far), cascade_for(new_far));
            let (old_px, new_px) = (px_per_texel(old_t[oc]), px_per_texel(new_t[nc]));
            eprintln!(
                "[CASCADE FIT] {res}: old cascade{oc} texel={:.5} ({:.2} screen px)  ->                   new cascade{nc} texel={:.5} ({:.2} screen px)",
                old_t[oc], old_px, new_t[nc], new_px
            );
            // Like-for-like, same resolution: the fit must be a large win.
            assert!(
                new_t[nc] * 4.0 < old_t[oc],
                "{res}: scene fit ({}) is not 4x tighter than the 500-unit fit ({})",
                new_t[nc], old_t[oc]
            );
        }

        // THE HEADLINE: at 8192 one shadow texel must be SUB-PIXEL at inspection range.
        // That is the difference between a crisp edge and the reported staircase — the
        // old fit sat at ~3.3 screen pixels per texel.
        let (_, _, new_8k) = compute_cascade_vps(view, mk_proj(new_far), light_dir, near, new_far, 8192);
        let new_px = px_per_texel(new_8k[cascade_for(new_far)]);
        assert!(new_px < 1.0, "shadow texel still spans {new_px} screen pixels at 8192");

        // And the practical payoff: 1024 must now be usable for close-up work — under
        // ~4 screen px per texel, i.e. no worse than the old 8K fit was.
        let (_, _, new_1k) = compute_cascade_vps(view, mk_proj(new_far), light_dir, near, new_far, 1024);
        let px_1k = px_per_texel(new_1k[cascade_for(new_far)]);
        eprintln!("[CASCADE FIT] 1024 now spans {px_1k:.2} screen px/texel at inspection range");
        assert!(px_1k < 4.5, "1024 still unusable close up: {px_1k} screen px/texel");
    }

    #[test]
    fn texel_world_size_sane() {
        let view = Mat4::look_at_rh(Vec3::new(4.0, 3.0, 6.0), Vec3::ZERO, Vec3::Y);
        let (near, far) = (0.1_f32, 500.0_f32); // raster_gbuffer.rs: far = max(dist*20, 500)
        let light_dir = Vec3::new(0.4, 0.8, 0.45); // dir_to_sun (scene convention)
        let proj = Mat4::perspective_rh(45f32.to_radians(), 16.0 / 9.0, near, far);

        let (_, _, texels) = compute_cascade_vps(view, proj, light_dir, near, far, 1024);

        // Magnitudes are split-dependent and the splits are now near/far-relative
        // (2026-08-06), so this asserts the INVARIANTS rather than three windows that
        // silently encoded the old fixed [0, 0.08, 0.3, 1.0] fractions.
        eprintln!("[texel_world_size] far={far} texels={texels:?}");
        assert!(texels[0] > 0.0 && texels[2] < far * 0.01,
            "texel sizes implausible for far={far}: {texels:?}");
        assert!(texels[0] < texels[1] && texels[1] < texels[2],
            "cascade texel size must grow with index: {texels:?}");
        // Derived normal offset must be far below the cascade extent (r ≈ half_size·texel)
        // at BOTH ends of the softness slider (0.5 → 1.5 texels, 3.0 → 4.0 texels).
        for c in 0..NUM_CASCADES {
            let extent = texels[c] * 1024.0; // = 2r
            assert!((0.5 + 1.0) * texels[c] < extent * 0.01, "cascade {c} over-biased (default softness)");
            assert!((3.0 + 1.0) * texels[c] < extent * 0.01, "cascade {c} over-biased (max softness)");
        }

        // Resolution scaling: the sphere fit is resolution-independent, so 2048 must give
        // exactly half the texel world-size (up to the 1/16 radius quantization) — the
        // adaptive bias shrinks in lockstep, which is why a resolution bump needs zero
        // bias retuning (the 17b payoff).
        let (_, _, texels_2k) = compute_cascade_vps(view, proj, light_dir, near, far, 2048);
        for c in 0..NUM_CASCADES {
            let ratio = texels_2k[c] / texels[c];
            assert!(
                (ratio - 0.5).abs() < 1e-4,
                "cascade {c}: 2048 texel not half of 1024 texel (ratio {ratio})"
            );
        }
    }

    // ========================================================================
    // Headless GPU shadow readback harness (the SHADOWACNE-3 deliverable).
    //
    // Renders the REAL shadow pass (ShadowMapSystem pipeline, real cascade
    // fitting) for a unit cube + floor, then evaluates the REAL
    // `compute_shadow()` WGSL (extracted verbatim from gbuffer.wgsl) at probe
    // points and reads the factors back. No reasoning-only fixes: the numbers
    // decide.
    //
    // The light direction is the app's ACTUAL default sun — `LightConfig
    // { azimuth: 45, elevation: 55 }.direction()` (graphics/src/types.rs:206,
    // formula replicated below), passed to `update_cascades` exactly as
    // `raster_gbuffer.rs:484` passes it. The two prior fixes' tests hand-wrote
    // a DOWNWARD light vector, which is why they stayed green while the app
    // still self-shadowed: the scene stores the direction TOWARD the sun.
    // ========================================================================

    /// The app's default sun: LightConfig::default() run through direction().
    /// (render must not depend on the graphics crate, so the 4-line formula is
    /// replicated; the values are asserted against the known default.)
    fn app_default_sun_direction() -> Vec3 {
        let (az, el) = (45.0f32.to_radians(), 55.0f32.to_radians());
        let d = Vec3::new(az.sin() * el.cos(), el.sin(), -az.cos() * el.cos()).normalize();
        // Sanity-pin to graphics::LightConfig::default().direction()
        assert!((d - Vec3::new(0.40558, 0.81915, -0.40558)).length() < 1e-4);
        d
    }

    fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })).ok()?;
        pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("shadow_readback_test_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            },
        ))
        .ok()
    }

    /// Extract a section of gbuffer.wgsl verbatim (the probe must run the REAL
    /// shader code, not a copy that can drift).
    fn wgsl_section(src: &str, start: &str, end: &str) -> String {
        let s = src.find(start).unwrap_or_else(|| panic!("marker {start:?} missing"));
        let e = src[s..].find(end).map(|i| s + i).unwrap_or_else(|| panic!("marker {end:?} missing"));
        src[s..e].to_string()
    }

    /// Axis-aligned unit-cube geometry (cube spans [-1,1]² in XZ, y ∈ [0,2])
    /// plus a big floor quad at y = 0. Returns (vertices, indices).
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
        // Cube faces (CCW from outside)
        quad([-1., 2., -1.], [-1., 2., 1.], [1., 2., 1.], [1., 2., -1.], [0., 1., 0.]); // top
        quad([-1., 0., -1.], [1., 0., -1.], [1., 0., 1.], [-1., 0., 1.], [0., -1., 0.]); // bottom
        quad([1., 0., -1.], [1., 2., -1.], [1., 2., 1.], [1., 0., 1.], [1., 0., 0.]); // +x
        quad([-1., 0., -1.], [-1., 0., 1.], [-1., 2., 1.], [-1., 2., -1.], [-1., 0., 0.]); // -x
        quad([-1., 0., 1.], [1., 0., 1.], [1., 2., 1.], [-1., 2., 1.], [0., 0., 1.]); // +z
        quad([-1., 0., -1.], [-1., 2., -1.], [1., 2., -1.], [1., 0., -1.], [0., 0., -1.]); // -z
        // Floor
        quad([-12., 0., -12.], [-12., 0., 12.], [12., 0., 12.], [12., 0., -12.], [0., 1., 0.]);
        (v, idx)
    }

    /// CPU oracle: does the ray from `p` toward the sun hit the cube AABB
    /// ([-1,1], [0,2], [-1,1])? Classifies which floor points should be shadowed.
    fn ray_hits_cube(p: Vec3, to_sun: Vec3) -> bool {
        let (bmin, bmax) = (Vec3::new(-1., 0., -1.), Vec3::new(1., 2., 1.));
        let inv = to_sun.recip();
        let t0 = (bmin - p) * inv;
        let t1 = (bmax - p) * inv;
        let tmin = t0.min(t1);
        let tmax = t0.max(t1);
        let enter = tmin.x.max(tmin.y).max(tmin.z);
        let exit = tmax.x.min(tmax.y).min(tmax.z);
        exit > enter.max(1e-3)
    }

    /// Run the full pipeline: cascades → shadow render pass → compute_shadow()
    /// probe → readback. Returns one factor per probe point (1.0 = lit).
    /// `resolution` / `pcf_radius` = the two 2026-07-17f quality controls, exercised
    /// through the REAL system (`ShadowMapSystem.resolution` + uniform params).
    fn shadow_factors(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: Mat4,
        proj: Mat4,
        dir_to_sun: Vec3,
        near: f32,
        far: f32,
        sun_enabled: bool,
        resolution: u32,
        pcf_radius: f32,
        points: &[(Vec3, Vec3)], // (world_pos, normal)
    ) -> Vec<f32> {
        use wgpu::util::DeviceExt;
        let mut shadows = ShadowMapSystem::new(device);
        shadows.resolution = resolution; // pre-init: maps are created at this size below
        shadows.ensure_textures(device);
        // EXACTLY the raster_gbuffer.rs call: the scene's sun direction, unmodified.
        shadows.update_cascades(queue, view, proj, dir_to_sun, near, far, sun_enabled, pcf_radius, 0.0,
            ShadowBias::default());

        // --- Real shadow render pass, all cascades ---
        let (verts, indices) = cube_and_floor();
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe_verts"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe_idx"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let ident = Mat4::IDENTITY.to_cols_array_2d();
        let inst = InstanceData {
            model_matrix: ident,
            normal_matrix: [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]],
            material_idx: 0,
            body_index: 1,
            flags: 0,
            _pad: 0,
            curv_params: [0.0, 1.0, 1.0, 1.0],
        };
        let inst_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe_inst"),
            contents: bytemuck::bytes_of(&inst),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // Group(1): caster material resources for the displacement lookup. These probes use
        // material_idx 0 with a zeroed material (displacement_map.x = 0 → no displacement), so
        // the caster is a NO-OP and the shadow output is identical to the pre-displacement path
        // — the existing SHADOWACNE-3 assertions therefore still hold unchanged.
        // One MaterialData, all zero ⇒ displacement_map.x = 0. Sized from RasterMaterial so a
        // layout change can never desync this probe again (the 304-literal did on 2026-08-20).
        let zero_mat = vec![0u8; std::mem::size_of::<crate::mesh_gpu::RasterMaterial>()];
        let mat_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe_mat"),
            contents: &zero_mat,
            usage: wgpu::BufferUsages::STORAGE,
        });
        let mat_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe_mat_tex"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mat_tex_view = mat_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let mat_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("probe_mat_sampler"),
            ..Default::default()
        });
        shadows.rebuild_material_bind_group(device, &mat_buf, &mat_tex_view, &mat_sampler);
        let shadow_mat_bg = shadows.material_bind_group.as_ref().unwrap();

        let mut enc = device.create_command_encoder(&Default::default());
        for c in 0..NUM_CASCADES {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow_probe_pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: shadows.depth_views[c].as_ref().unwrap(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&shadows.pipeline);
            pass.set_bind_group(0, shadows.bind_groups[c].as_ref().unwrap(), &[]);
            pass.set_bind_group(1, shadow_mat_bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.set_vertex_buffer(1, inst_buf.slice(..));
            pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
        }

        // --- compute_shadow() probe: REAL WGSL extracted from gbuffer.wgsl ---
        let gbuf_src = include_str!("gbuffer.wgsl");
        let mut struct_end = 0usize; // ShadowData struct incl. closing "};"
        let sd_start = gbuf_src.find("struct ShadowData").expect("ShadowData struct");
        if let Some(e) = gbuf_src[sd_start..].find("};") { struct_end = sd_start + e + 2; }
        let shadow_data_struct = &gbuf_src[sd_start..struct_end];
        let shadow_fns = wgsl_section(gbuf_src, "// ---- Shadow Sampling ----", "// ---- Fragment ----");

        let probe_src = format!(
            r#"
struct CameraProbe {{ view: mat4x4<f32> }};
{shadow_data_struct}
@group(0) @binding(0) var<uniform> camera: CameraProbe;
@group(0) @binding(1) var<uniform> shadow_data: ShadowData;
@group(0) @binding(2) var shadow_map_0: texture_depth_2d;
@group(0) @binding(3) var shadow_map_1: texture_depth_2d;
@group(0) @binding(4) var shadow_map_2: texture_depth_2d;
@group(0) @binding(5) var shadow_sampler: sampler_comparison;
@group(0) @binding(6) var<storage, read> probe_pts: array<vec4<f32>>;
{shadow_fns}
@vertex
fn vs_probe(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {{
    // Fullscreen triangle
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) & 1) * 4.0 - 1.0;
    return vec4(x, y, 0.0, 1.0);
}}
@fragment
fn fs_probe(@builtin(position) fragpos: vec4<f32>) -> @location(0) vec4<f32> {{
    let i = u32(fragpos.x);
    let p = probe_pts[2u * i].xyz;
    let n = probe_pts[2u * i + 1u].xyz;
    return vec4(compute_shadow(p, n), 0.0, 0.0, 1.0);
}}
"#
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow_probe"),
            source: wgpu::ShaderSource::Wgsl(probe_src.into()),
        });

        // Same comparison sampler as the raster pipeline (pipeline.rs:159).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("probe_shadow_sampler"),
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let cam_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe_cam"),
            contents: bytemuck::cast_slice(&view.to_cols_array()),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut pts_raw: Vec<[f32; 4]> = Vec::new();
        for (p, n) in points {
            pts_raw.push([p.x, p.y, p.z, 1.0]);
            pts_raw.push([n.x, n.y, n.z, 0.0]);
        }
        let pts_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe_pts"),
            contents: bytemuck::cast_slice(&pts_raw),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("probe_bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: cam_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: shadows.shadow_uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(shadows.depth_views[0].as_ref().unwrap()) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(shadows.depth_views[1].as_ref().unwrap()) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(shadows.depth_views[2].as_ref().unwrap()) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 6, resource: pts_buf.as_entire_binding() },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("probe_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let n_pts = points.len() as u32;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe_target"),
            size: wgpu::Extent3d { width: n_pts, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());
        let probe_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("probe_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_probe"), buffers: &[], compilation_options: Default::default() },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_probe"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("probe_eval"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&probe_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        // Readback (bytes_per_row 256-aligned)
        let row_bytes = n_pts * 16;
        let padded = (row_bytes + 255) & !255;
        let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe_read"),
            size: padded as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::ImageCopyBuffer {
                buffer: &read_buf,
                layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            wgpu::Extent3d { width: n_pts, height: 1, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        read_buf.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        rx.recv().expect("map_async channel").expect("map_async failed");
        let data = read_buf.slice(..).get_mapped_range();
        let floats: &[f32] = bytemuck::cast_slice(&data);
        (0..n_pts as usize).map(|i| floats[i * 4]).collect()
    }

    /// The SHADOWACNE-3 spec, parameterized over the 2026-07-17f quality controls so
    /// every (resolution, softness) combination is held to the SAME contract: lit
    /// faces ≥0.95 (no acne), floor-in-shadow ≤0.2 (the cast shadow still LANDS — no
    /// peter-panning), floor-out ≥0.95.
    fn run_sun_harness(resolution: u32, pcf_radius: f32) {
        let Some((device, queue)) = try_device() else {
            eprintln!("shadow_readback: no GPU adapter — skipped");
            return;
        };
        let to_sun = app_default_sun_direction();

        // App-representative camera: orbiting eye looking at the body.
        let eye = Vec3::new(4.0, 3.0, 6.0);
        let view = Mat4::look_at_rh(eye, Vec3::new(0.0, 1.0, 0.0), Vec3::Y);
        let (near, far) = (0.1_f32, 500.0_f32); // raster_gbuffer.rs: far = max(dist*20, 500)
        let proj = Mat4::perspective_rh(45f32.to_radians(), 16.0 / 9.0, near, far);

        // Probe points. Cube spans [-1,1]×[0,2]×[-1,1]; sun is up/+x/-z, so the
        // top (+y), +x and -z faces are light-facing and must be FULLY lit.
        let mut points: Vec<(Vec3, Vec3)> = Vec::new();
        let mut groups: Vec<(&str, std::ops::Range<usize>)> = Vec::new();
        let mark = |points: &Vec<(Vec3, Vec3)>| points.len();

        let s = mark(&points);
        for &x in &[-0.9f32, -0.5, 0.0, 0.5, 0.9] {
            for &z in &[-0.9f32, -0.5, 0.0, 0.5, 0.9] {
                points.push((Vec3::new(x, 2.0, z), Vec3::Y)); // top face
            }
        }
        groups.push(("cube top face", s..points.len()));

        let s = mark(&points);
        for &y in &[0.3f32, 1.0, 1.7] {
            for &t in &[-0.7f32, 0.0, 0.7] {
                points.push((Vec3::new(1.0, y, t), Vec3::X)); // +x face (light-facing)
                points.push((Vec3::new(t, y, -1.0), Vec3::NEG_Z)); // -z face (light-facing)
            }
        }
        groups.push(("light-facing side faces", s..points.len()));

        // Floor: a point inside the cube's cast shadow (CPU-oracle verified)
        // and points well outside it.
        let s = mark(&points);
        let prop = -to_sun;
        let t_floor = 1.0 / -prop.y; // from cube center (0,1,0) down to y=0
        let in_shadow = Vec3::new(0.0, 1.0, 0.0) + prop * t_floor;
        assert!(ray_hits_cube(in_shadow + Vec3::Y * 1e-3, to_sun), "oracle: point must be occluded");
        points.push((in_shadow, Vec3::Y));
        groups.push(("floor in cast shadow", s..points.len()));

        let s = mark(&points);
        for p in [Vec3::new(6.0, 0.0, -6.0), Vec3::new(-6.0, 0.0, 6.0), Vec3::new(5.0, 0.0, 5.0)] {
            assert!(!ray_hits_cube(p, to_sun), "oracle: point must be lit");
            points.push((p, Vec3::Y));
        }
        groups.push(("floor outside shadow", s..points.len()));

        // The app path: raster_gbuffer.rs passes light_config.direction() (= to_sun),
        // sun_enabled = true (the default sun exists).
        let factors = shadow_factors(
            &device, &queue, view, proj, to_sun, near, far, true, resolution, pcf_radius, &points,
        );

        let mut failures = Vec::new();
        for (name, range) in &groups {
            for i in range.clone() {
                let f = factors[i];
                let (p, _) = points[i];
                let ok = match *name {
                    "floor in cast shadow" => f <= 0.2,
                    _ => f >= 0.95,
                };
                if !ok {
                    failures.push(format!("{name}: p=({:.2},{:.2},{:.2}) shadow={f:.3}", p.x, p.y, p.z));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "shadow readback ({resolution}², pcf {pcf_radius}): {} probe(s) wrong:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// THE SHADOWACNE-3 regression harness (default quality: 1024², pcf 0.5). On the
    /// pre-fix code (light frame built from the TO-SUN vector, i.e. shadow camera
    /// under the floor) this FAILS: every cube face and floor point reads ~0 lit
    /// while the floor's cast-shadow point reads lit.
    #[test]
    fn shadow_readback_default_sun_cube_and_floor() {
        run_sun_harness(DEFAULT_SHADOW_MAP_SIZE, DEFAULT_PCF_RADIUS_TEXELS);
    }

    /// 2026-07-17f quality-control variants of the same spec:
    /// - 2048² maps: the texel_world halves → the adaptive normal-offset bias halves with
    ///   it; the SAME assertions (no acne on lit faces, no peter-panning of the floor
    ///   contact shadow) prove a resolution bump needs ZERO bias retuning — the payoff of
    ///   the 17b texel-derived derivation.
    /// - max softness (3-texel PCF radius) at 1024: the bias derives from the footprint
    ///   (radius + 1 texel), so the widest penumbra still shows no acne, and 4 texels of
    ///   offset is still far too small to detach the contact shadow.
    #[test]
    fn shadow_readback_2048_bias_adapts() {
        run_sun_harness(2048, DEFAULT_PCF_RADIUS_TEXELS);
    }

    /// The bias sliders must default to EXACTLY the pre-slider behaviour.
    ///
    /// This is the gate that lets the sliders exist at all: a tuning control that
    /// silently changes the default image is a behaviour change wearing a slider's
    /// clothes. `normal_offset_texels = 1.0` reproduces the historical hardcoded
    /// `pcf_radius + 1`, and both depth biases are 0 — and gbuffer.wgsl's
    /// `shadow_depth_bias()` early-outs on `slope <= 0.0`, so the default path is
    /// bit-identical rather than merely close.
    #[test]
    fn shadow_bias_defaults_are_neutral() {
        let d = ShadowBias::default();
        assert_eq!(d.normal_offset_texels, 1.0, "default normal offset must reproduce `pcf_radius + 1`");
        assert_eq!(d.depth_bias_texels, 0.0, "default depth bias must be neutral");
        assert_eq!(d.slope_depth_bias_texels, 0.0, "default slope bias must be neutral");
        // `RenderSettings` carries a SECOND set of defaults and neither crate can see
        // the other (render <-> app_types have no dependency edge either way), so the
        // agreement between them is gated in the `app` crate instead:
        // app::raster_gbuffer::tests::shadow_bias_defaults_agree_across_crates.
    }

    /// gbuffer.wgsl recovers the cascade's ortho depth range FROM THE MATRIX
    /// (`1 / |row2(cascade_vp).xyz|`) to convert the depth bias from texels to NDC z.
    /// This pins that identity against the CPU-side construction.
    ///
    /// Why it earns its keep: an adversarial review (2026-08-06) caught the depth-bias
    /// sliders shipping in raw NDC, where ~50 units of caster headroom compress every
    /// receiver into ~0.03 NDC and the whole useful travel lands in the first 1% of the
    /// slider. The texel↔NDC conversion is the fix, and it is only correct while this
    /// identity holds — if someone changes the 50-unit headroom or swaps the projection,
    /// the shader must not silently keep using the old scale.
    #[test]
    fn cascade_depth_range_is_recoverable_from_the_matrix() {
        let view = Mat4::look_at_rh(Vec3::new(0.0, 3.0, 9.0), Vec3::ZERO, Vec3::Y);
        let proj = Mat4::perspective_rh(60f32.to_radians(), 16.0 / 9.0, 0.1, 100.0);
        let map = 1024u32;
        let (vps, _splits, texel_world) =
            compute_cascade_vps(view, proj, Vec3::new(0.4, 0.8, 0.45).normalize(), 0.1, 40.0, map);

        for c in 0..NUM_CASCADES {
            let m = Mat4::from_cols_array_2d(&vps[c]);
            // Row 2 of a column-major mat4 = (col0.z, col1.z, col2.z) — exactly what the
            // shader reads as `vec3(cascade_vp[0].z, cascade_vp[1].z, cascade_vp[2].z)`.
            let z_row = Vec3::new(m.x_axis.z, m.y_axis.z, m.z_axis.z);
            let recovered = 1.0 / z_row.length();
            // shadows.rs builds orthographic_rh(.., -r, 50+r) with r = radius + texel and
            // texel = 2*radius/map, so far-near = 50 + 2r = 50 + texel*(map+2).
            let expected = 50.0 + texel_world[c] * (map as f32 + 2.0);
            assert!(
                (recovered - expected).abs() <= expected * 1e-3,
                "cascade {c}: matrix-recovered depth range {recovered} != constructed {expected} \
                 — gbuffer.wgsl's texel→NDC bias conversion is now wrong"
            );
            // The property that actually makes the sliders TUNABLE: full travel must stay
            // a small fraction of the receiver depth band, so dragging sweeps a usable
            // range instead of jumping from "no effect" to "all shadows gone".
            //
            // (An earlier version of this test also asserted the 50-unit headroom always
            // dominates the band. It does not — cascade 1 of a far=40 scene measures 0.51
            // — and the claim was never needed: the texel→NDC conversion is exact at any
            // ratio. Only this bound is load-bearing.)
            let receiver_band = texel_world[c] * (map as f32 + 2.0) / recovered;
            let slider_max_ndc = 8.0 * texel_world[c] / recovered;
            assert!(
                slider_max_ndc < receiver_band * 0.05,
                "cascade {c}: Depth Bias at full travel is {slider_max_ndc} NDC against a \
                 receiver band of {receiver_band} — the slider is too coarse to tune"
            );
            // Same for the slope term, whose tan() multiplier is bounded at 16 in the shader.
            let slope_max_ndc = 8.0 * 16.0 * texel_world[c] / recovered;
            assert!(
                slope_max_ndc < receiver_band,
                "cascade {c}: Slope Bias at full travel ({slope_max_ndc} NDC) exceeds the \
                 whole receiver band ({receiver_band}) — it can erase every shadow"
            );
        }
    }

    /// The tweak lanes must stay on the unused padding, never resize the uniform.
    #[test]
    fn shadow_bias_rides_padding_lanes() {
        assert_eq!(
            std::mem::size_of::<ShadowUniforms>(),
            NUM_CASCADES * 64 + 64,
            "ShadowUniforms grew — the bias tweaks must ride existing padding lanes \
             (params.w / texel_world.w / cascade_splits.w), not new fields"
        );
    }

    /// The NORMAL offset must stay flat in texels — no grazing-angle slope scale.
    ///
    /// A slope scale (`base * (1 + min(tan(theta), 4))`) was tried on 2026-08-06 and
    /// REVERTED the same day: it made the image visibly worse (owner: "WORST"), because
    /// moving the sample position by up to 5 texels on near-edge-on faces smears the
    /// lookup into a soft gradient. Slope compensation belongs on the shadow pipeline's
    /// depth bias, which does not move the sample. This guard exists so the dead end is
    /// not re-derived. Repro: docs/planning/render/repro/shadow-acne.fl4
    #[test]
    fn shadow_normal_offset_is_not_slope_scaled() {
        let src = include_str!("gbuffer.wgsl");
        let f = src
            .split("fn normal_offset_texels")
            .nth(1)
            .expect("normal_offset_texels missing");
        let body = &f[..f.find('}').expect("unterminated normal_offset_texels")];
        assert!(
            !body.contains("tan_t") && !body.contains("flags.yzw"),
            "normal_offset_texels reintroduced a grazing-angle slope scale — reverted \
             2026-08-06 as a visual regression; see the doc comment in gbuffer.wgsl"
        );
    }

    #[test]
    fn shadow_readback_max_softness_no_acne() {
        run_sun_harness(DEFAULT_SHADOW_MAP_SIZE, 3.0);
    }



    /// OWNER 2026-07-29 (screenshot): Base color = [Noise·FBM Mult] over [Curvature Norm], and the
    /// noise does not appear.
    ///
    /// ROOT CAUSE, pinned here rather than in prose: a Vertex-rung curvature layer set to Normal
    /// blend REPLACES everything beneath it. `BlendMode::Normal` lowers to blend code 0
    /// (node_graph_bake.rs), and gbuffer.wgsl's vertex-curvature composite treats 0 as the
    /// fall-through `else`: `blended = top`, then `color = mix(base, blended, opacity)`. At
    /// opacity 1.0 that is `color = top` — `base` (which carries the BAKED noise map) is discarded.
    ///
    /// So the noise is computed correctly, baked correctly, connected correctly in the graph, and
    /// then overwritten in the shader. Every upstream test passes; only the final composite loses
    /// it. That is why this took several rounds to find.
    ///
    /// This test reads the SHIPPED shader text, so it fails if the branch is restructured — the
    /// point is that a change here is deliberate, not accidental.
    #[test]
    fn vertex_curvature_normal_blend_replaces_the_baked_map() {
        // P2 / HANDOFF Decision 1: the G-buffer live composite is DELETED. Bake owns layers.
        // Guard: do not re-introduce `blended = top` / mix(base, blended, opacity) over maps.
        let src = include_str!("gbuffer.wgsl");
        assert!(
            src.contains("Vertex-curvature live composite REMOVED")
                || src.contains("P2) Vertex-curvature live composite REMOVED"),
            "P2 removal marker missing from gbuffer.wgsl — restore the bake-only albedo path"
        );
        assert!(
            !src.contains("blended = top;"),
            "live `blended = top` re-introduced — that is the dual-compositor bug Decision 1 deletes"
        );
        assert!(
            !src.contains("let as_mask = as_mask_early;"),
            "as_mask composite block re-introduced"
        );
    }

    /// M2 gate — Hybrid and PT must use the SAME absolute-map albedo contract (bit1 of pack).
    /// Full pixel soak needs a running app; this fences the dual-compositor class in both
    /// consumers: REPLACE when absolute, tint Mult otherwise; skip live noise over absolute.
    #[test]
    fn m2_pt_and_hybrid_absolute_albedo_contract_match() {
        let gbuf = include_str!("gbuffer.wgsl");
        // pathtrace lives in graphics; relative from render/src.
        let pt = include_str!("../../graphics/src/pathtrace_tri_entry.wgsl");
        for (name, src) in [("gbuffer", gbuf), ("pathtrace", pt)] {
            assert!(
                src.contains("amap_pack") || src.contains("(amap_pack & 2u)") || src.contains("& 2u"),
                "{name}: absolute bit (pack & 2) required for layered bake REPLACE"
            );
            // Absolute → REPLACE (mix scalar, sample), not free Mult.
            assert!(
                src.contains("mix(albedo") || src.contains("mix(pre_map_color") || src.contains("mix(color"),
                "{name}: must mix toward sample for absolute/tint paths"
            );
        }
        // Both skip live noise over absolute Mult stacks.
        assert!(
            gbuf.contains("!amap_is_absolute") || gbuf.contains("&& !amap_is_absolute"),
            "gbuffer: live noise must skip absolute maps"
        );
        assert!(
            pt.contains("!amap_is_absolute") || pt.contains("&& !amap_is_absolute"),
            "pathtrace: live noise must skip absolute maps (parity with Hybrid)"
        );
        // PT documents REPLACE for absolute (same as Hybrid).
        assert!(
            pt.contains("absolute") && (pt.contains("REPLACE") || pt.contains("& 2u")),
            "pathtrace must document absolute Mult-stack REPLACE"
        );
    }

    /// M2 sabotage fence — bake is the only layer compositor (PLAN_FOR_GROK_MATERIALS M2).
    /// Hybrid albedo samples the baked map like PT; live noise must not Mult over absolute stacks.
    #[test]
    fn m2_gbuffer_is_bake_display_not_second_compositor() {
        let src = include_str!("gbuffer.wgsl");
        assert!(
            src.contains("ONLY layer compositor") || src.contains("the ONLY layer compositor"),
            "albedo path must document bake-owned compositing"
        );
        assert!(
            src.contains("amap_absolute") || src.contains("amap_is_absolute"),
            "absolute Mult-stack bake path required (no double Mult with live noise)"
        );
        // Live noise must refuse absolute maps (belt-and-braces vs dual Mult).
        assert!(
            src.contains("!amap_is_absolute") || src.contains("&& !amap_is_absolute"),
            "live noise_pattern must skip when albedo map is absolute layered bake"
        );
        // M2 sabotage: code after the removal marker must not *use* vertex_curv as a value
        // (comments may still name it). Strip // comments, then reject live uses.
        let after_remove = src
            .split("Vertex-curvature live composite REMOVED")
            .nth(1)
            .unwrap_or("");
        let code_only: String = after_remove
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code_only.contains("vertex_curv"),
            "vertex_curv must not be used in code after P2 removal (second compositor)"
        );
        assert!(
            !code_only.contains("as_mask_early"),
            "as_mask_early composite must stay deleted"
        );
    }

    /// OWNER 2026-07-29: the owner reports the shadow not matching the geometry at 4096² with
    /// softness turned up — the exact combination no existing variant covers (they test 1024/2048,
    /// and max softness only at 1024). If the normal-offset/PCF coupling detaches the contact
    /// shadow, it detaches HERE.
    #[test]
    fn shadow_readback_4096_max_softness() {
        run_sun_harness(4096, 3.0);
    }

    /// Round-4 harness (owner clue: "the dark blob persists EVEN AFTER DELETING THE SUN").
    ///
    /// With zero lights a PBR pixel is ambient-only, so its brightness is whatever still
    /// multiplies the ambient term. The app keeps calling `update_cascades` with the STALE
    /// `light_config` direction after the last sun is deleted (`sync_light_to_gpu` early-outs
    /// when no light exists), and `fs_gbuffer` keeps baking `compute_shadow()` into
    /// gbuf_material.z — which the deferred pass multiplies into ambient for raster pixels.
    ///
    /// SPEC: with no sun in the scene, every surface probe's shadow factor must read 1.0
    /// (a deleted light structurally disables the shadow multiply — no stale state).
    /// Probes sit on the cube faces that face AWAY from the stale default sun (bottom,
    /// -x, +z) — exactly where the owner sees the dark streaky banding.
    #[test]
    fn shadow_readback_zero_lights_full_ambient() {
        let Some((device, queue)) = try_device() else {
            eprintln!("shadow_readback: no GPU adapter — skipped");
            return;
        };
        let stale_to_sun = app_default_sun_direction(); // what light_config still holds post-delete

        let eye = Vec3::new(4.0, -1.0, 6.0); // slightly below — the owner's viewpoint
        let view = Mat4::look_at_rh(eye, Vec3::new(0.0, 1.0, 0.0), Vec3::Y);
        let (near, far) = (0.1_f32, 500.0_f32);
        let proj = Mat4::perspective_rh(45f32.to_radians(), 16.0 / 9.0, near, far);

        // Away-facing cube faces (n · to_sun < 0): bottom, -x, +z.
        let mut points: Vec<(Vec3, Vec3)> = Vec::new();
        for &a in &[-0.7f32, 0.0, 0.7] {
            for &b in &[-0.7f32, 0.0, 0.7] {
                points.push((Vec3::new(a, 0.0, b), Vec3::NEG_Y)); // bottom face
            }
        }
        for &y in &[0.3f32, 1.0, 1.7] {
            for &t in &[-0.7f32, 0.0, 0.7] {
                points.push((Vec3::new(-1.0, y, t), Vec3::NEG_X)); // -x face
                points.push((Vec3::new(t, y, 1.0), Vec3::Z)); // +z face
            }
        }

        // The app path after deleting the sun: cascades still updated from the stale config
        // and the stale shadow maps still RENDERED (the harness draws all 3 cascade passes) —
        // sun_enabled = false alone must neutralize them. Proven to fail on HEAD 14f4809
        // (no flag → 27/27 probes dark: 0.000 across the face interiors, 0.159–0.435 partial
        // PCF near the top edges — the owner's "soft streaky blob" gradient, numerically).
        let factors = shadow_factors(
            &device, &queue, view, proj, stale_to_sun, near, far, false,
            DEFAULT_SHADOW_MAP_SIZE, DEFAULT_PCF_RADIUS_TEXELS, &points,
        );

        let failures: Vec<String> = points
            .iter()
            .zip(&factors)
            .filter(|(_, f)| **f < 0.999)
            .map(|((p, n), f)| {
                format!(
                    "p=({:.2},{:.2},{:.2}) n=({:.0},{:.0},{:.0}) shadow={f:.3} (must be 1.0 — no sun exists)",
                    p.x, p.y, p.z, n.x, n.y, n.z
                )
            })
            .collect();
        assert!(
            failures.is_empty(),
            "zero-light spec: {} of {} probes still shadow-darkened by the DELETED sun:\n{}",
            failures.len(),
            points.len(),
            failures.join("\n")
        );
    }

    /// Texel-snapping stability: identical inputs → bit-identical cascade matrices, and a
    /// sub-texel camera translation leaves the snapped light-space origin unchanged on at
    /// least one side of the texel boundary (floor-snap can cross at most one boundary per
    /// direction, so of +δ and −δ one MUST be a no-op).
    #[test]
    fn texel_snapping_is_stable() {
        let (near, far) = (0.1_f32, 500.0_f32);
        let proj = Mat4::perspective_rh(45f32.to_radians(), 16.0 / 9.0, near, far);
        let light_dir = Vec3::new(0.4, 0.8, 0.45); // dir_to_sun (scene convention)
        let view_at = |eye: Vec3, target: Vec3| Mat4::look_at_rh(eye, target, Vec3::Y);

        let eye = Vec3::new(4.0, 3.0, 6.0);
        let sz = DEFAULT_SHADOW_MAP_SIZE;
        let base = view_at(eye, Vec3::ZERO);
        let (vps_a, _, texels) = compute_cascade_vps(base, proj, light_dir, near, far, sz);

        // Determinism: same inputs → identical matrices.
        let (vps_b, _, _) = compute_cascade_vps(base, proj, light_dir, near, far, sz);
        assert_eq!(vps_a, vps_b);

        // Translate camera AND target by ±0.05 texel of cascade 0 (pure translation keeps
        // the slice shape → radius/texel identical; only the center moves sub-texel).
        let d = Vec3::X * (texels[0] * 0.05);
        let (vps_p, _, _) = compute_cascade_vps(view_at(eye + d, d), proj, light_dir, near, far, sz);
        let (vps_m, _, _) = compute_cascade_vps(view_at(eye - d, -d), proj, light_dir, near, far, sz);
        for c in 0..NUM_CASCADES {
            let unchanged = vps_p[c] == vps_a[c] || vps_m[c] == vps_a[c];
            assert!(unchanged, "cascade {c}: sub-texel camera move changed the cascade matrix in both directions — snapping broken");
        }
    }
}
