// core_engine/app/src/app_new.rs
//
// Slice 30 (app-crate de-god, PURE RELOCATION from main.rs): the FourthLayerApp
// constructor (`new`) plus the scene-graph/flat-cache bridge helpers
// (`ensure_sdf_body`, `build_graph_from_flat`, `rebuild_prim_body_map`, `toast`,
// `derive_modeling_context`). Bodies are byte-for-byte identical to their former
// home in main.rs — no logic change, no behavior change.

use crate::*;
use eframe::wgpu;
use glam::Vec3;
use core_math::sdf_volume::SdfVolume;
use core_math::primitives::BooleanOp;
use core_math::scene_graph::{SceneGraph, NodeId, NodePayload};
use graphics::gpu_volume::SdfGpuVolume;
use std::collections::HashMap;

/// Carries everything the UI needs after a successful /generate call.
impl FourthLayerApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let t_total = std::time::Instant::now();
        // ui2 fonts MUST be installed before the first frame: `Context::set_fonts` only stores
        // `new_font_definitions`, which egui applies at the NEXT `begin_frame`. Installing them
        // lazily from `update()` left frame 1 drawing with the default fonts, so the named
        // "azra_semibold" family was unbound and epaint panicked. Same env gate as `update()`.
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            crate::ui_theme::install_fonts(&cc.egui_ctx);
        }
        let render_state_opt = cc.wgpu_render_state.clone();
        let mut gpu_info = GpuInfo::default();
        let cpu_volume = SdfVolume::new_empty(128, 2.0);
        // Create buffers, layouts, shader module (all fast) — pipeline compilation deferred to update()
        let gpu_init = if let Some(rs) = &render_state_opt {
            let device = &rs.device;
            let info = rs.adapter.get_info();
            // Estimate VRAM from adapter type (wgpu doesn't expose memory size directly)
            let vram_estimate = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => {
                    // Heuristic: RTX 4090=24GB, 4070=12GB, 3060=12GB — assume 8GB minimum
                    let limit = rs.adapter.limits().max_buffer_size;
                    if limit >= 8 * 1024 * 1024 * 1024 { 24u64 * 1024 * 1024 * 1024 }  // high-end
                    else if limit >= 2 * 1024 * 1024 * 1024 { 8 * 1024 * 1024 * 1024 }  // mid-range
                    else { 4 * 1024 * 1024 * 1024 }  // entry
                }
                wgpu::DeviceType::IntegratedGpu => 2 * 1024 * 1024 * 1024, // shared memory
                _ => 4 * 1024 * 1024 * 1024, // conservative fallback
            };
            gpu_info = GpuInfo { adapter_name: info.name.clone(), backend: format!("{:?}", info.backend), driver: info.driver.clone(), device_type: format!("{:?}", info.device_type), vram_bytes: vram_estimate };
            eprintln!("[FourthLayer] GPU: {} ({:?}) | Backend: {:?} | Driver: {}", info.name, info.device_type, info.backend, info.driver);
            let t0 = std::time::Instant::now();
            // (Full analytical raymarch shader module REMOVED 2026-07-14 — dead march display path.)
            // Deferred-lite: no material DNA/blended eval, no scene_sdf → compiles instantly
            let df_wgsl = graphics::build_deferred_lite_wgsl();
            let df_lines = df_wgsl.lines().count();
            let df_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("DeferredLite"),source:wgpu::ShaderSource::Wgsl(df_wgsl.into())});
            let pp_wgsl = graphics::process_post_process_wgsl("rgba16float");
            let pp_line_count = pp_wgsl.lines().count();
            let pp_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("PostProcess"),source:wgpu::ShaderSource::Wgsl(pp_wgsl.into())});
            eprintln!("[TIMING] Shader modules (DF {} + PP {} lines): {:?}", df_lines, pp_line_count, t0.elapsed());
            let pipeline_cache = if device.features().contains(wgpu::Features::PIPELINE_CACHE) {
                let cache_data = gpu_init::load_pipeline_cache_data();
                if cache_data.is_some() { eprintln!("[FourthLayer] Pipeline cache loaded from disk"); }
                let cache = unsafe { device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor { label: Some("FourthLayer PipelineCache"), data: cache_data.as_deref(), fallback: true }) };
                eprintln!("[FourthLayer] Pipeline cache enabled");
                Some(cache)
            } else { None };
            // BGL entry helpers (used for layouts not shared with bg thread)
            let tex_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Texture{sample_type:wgpu::TextureSampleType::Float{filterable:false},view_dimension:wgpu::TextureViewDimension::D2,multisampled:false},count:None};
            let sto_entry = |b:u32,fmt:wgpu::TextureFormat| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::StorageTexture{access:wgpu::StorageTextureAccess::WriteOnly,format:fmt,view_dimension:wgpu::TextureViewDimension::D2},count:None};
            let uni_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Uniform,has_dynamic_offset:false,min_binding_size:None},count:None};
            let buf_ro = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Storage{read_only:true},has_dynamic_offset:false,min_binding_size:None},count:None};
            let sto_ro_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Storage{read_only:true},has_dynamic_offset:false,min_binding_size:None},count:None};
            let tex_uint_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Texture{sample_type:wgpu::TextureSampleType::Uint,view_dimension:wgpu::TextureViewDimension::D2,multisampled:false},count:None};
            // Shared BGL descriptors (canonical source — see graphics::bgl)
            let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("raymarch_bgl"),entries:&graphics::bgl::raymarch_bgl_entries()});
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&bgl],push_constant_ranges:&[]});
            let deferred_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("deferred_bgl"),entries:&graphics::bgl::deferred_bgl_entries()});
            let deferred_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&deferred_bgl],push_constant_ranges:&[]});
            // SSR quality pass 2026-07-18: +14 albedo (F0 for the split-sum envelope),
            // +21 env buffer (IBL specular to subtract), +39 BRDF LUT (same envelope
            // as deferred — exact energy-conserving swap).
            // +65/66/67 (Phase 3): auto reflection probe cube + sampler + params —
            // pass_ssr's (1−confidence) fallback. Mirrored in ssr_gpu_test.rs.
            // +38 SSAO (Phase C1): specular_occlusion matches deferred.
            let cube_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Texture{sample_type:wgpu::TextureSampleType::Float{filterable:true},view_dimension:wgpu::TextureViewDimension::Cube,multisampled:false},count:None};
            let samp_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),count:None};
            let ssr_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("ssr_bgl"),entries:&[uni_entry(1),tex_entry(5),tex_entry(12),tex_entry(13),tex_entry(14),sto_entry(15,wgpu::TextureFormat::Rgba16Float),buf_ro(21),tex_entry(38),buf_ro(39),cube_entry(65),samp_entry(66),uni_entry(67)]});
            let ssr_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&ssr_bgl],push_constant_ranges:&[]});
            // Layout lives in graphics::bgl (single source) — see fog_bgl_entries().
            let fog_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("fog_bgl"),entries:&graphics::bgl::fog_bgl_entries()});
            let fog_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&fog_bgl],push_constant_ranges:&[]});
            let upsample_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("upsample_bgl"),entries:&[tex_entry(19),sto_entry(20,wgpu::TextureFormat::Rgba16Float)]});
            let upsample_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&upsample_bgl],push_constant_ranges:&[]});
            let tex3d_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Texture{sample_type:wgpu::TextureSampleType::Float{filterable:true},view_dimension:wgpu::TextureViewDimension::D3,multisampled:false},count:None};
            let sampler_entry = |b:u32| wgpu::BindGroupLayoutEntry{binding:b,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),count:None};
            let pt_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("pt_bgl"),entries:&graphics::bgl::pt_bgl_entries()});
            let pt_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&pt_bgl],push_constant_ranges:&[]});
            // (PT Bounce BGL/PL REMOVED 2026-07-14 — split GLSL PT stripped)
            // 63/64 = progressive PBR refine accumulation in/out (PROGRESSIVE_PBR_PLAN.md
            // Phase 1) — must stay mirrored in tonemap_gpu_test.rs and viewport_render's
            // tonemap_bg.
            let tonemap_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("tonemap_bgl"),entries:&[tex_entry(5),sto_entry(6,wgpu::TextureFormat::Rgba16Float),tex_entry(7),uni_entry(1),tex_entry(16),tex_entry(18),tex_entry(12),tex_entry(14),tex_uint_entry(31),tex_entry(33),tex_entry(35),tex_entry(37),sto_ro_entry(40),tex_entry(62),tex_entry(63),sto_entry(64,wgpu::TextureFormat::Rgba16Float)]});
            let tonemap_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&tonemap_bgl],push_constant_ranges:&[]});
            let bloom_extract_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("bloom_ext"),entries:&[tex_entry(5),sto_entry(8,wgpu::TextureFormat::Rgba16Float),uni_entry(1)]});
            let bloom_ext_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&bloom_extract_bgl],push_constant_ranges:&[]});
            let bloom_blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("bloom_blur"),entries:&[tex_entry(7),sto_entry(8,wgpu::TextureFormat::Rgba16Float),uni_entry(1)]});
            let bloom_blur_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&bloom_blur_bgl],push_constant_ranges:&[]});
            let bevel_preview_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("bevel_bgl"),entries:&[buf_ro(0),uni_entry(1),buf_ro(2),buf_ro(3),sto_entry(4,wgpu::TextureFormat::Rgba16Float),tex_entry(5)]});
            let sculpt_raymarch_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("sculpt_raymarch_bgl"),entries:&graphics::bgl::sculpt_raymarch_bgl_entries()});
            let grid_res: u32 = 8; let grid_total = grid_res * grid_res * grid_res;
            Some(GpuInitState {
                render_state: rs.clone(),
                df_shader, pp_shader, pipeline_cache,
                bgl, deferred_bgl, ssr_bgl, fog_bgl, upsample_bgl, pt_bgl, tonemap_bgl, bloom_extract_bgl, bloom_blur_bgl,
                pl, deferred_pl, ssr_pl, fog_pl, upsample_pl, pt_pl, tonemap_pl, bloom_ext_pl, bloom_blur_pl,
                sculpt_raymarch_bgl,
                ub: device.create_buffer(&wgpu::BufferDescriptor{label:Some("Uniforms"),size:std::mem::size_of::<graphics::GpuUniforms>()as u64,usage:wgpu::BufferUsages::UNIFORM|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                // Allocate a minimal 1³ dummy volume. The full-size SdfGpuVolume
                // (12-96MB) is only needed for sculpt mode, which creates it on entry.
                // Binding slot 2 still needs a valid buffer, so 1³ (4 bytes) satisfies it.
                gv: SdfGpuVolume::new(device, [1, 1, 1]),
                pb: device.create_buffer(&wgpu::BufferDescriptor{label:Some("PrimData"),size:4096,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                tb: device.create_buffer(&wgpu::BufferDescriptor{label:Some("TexData"),size:4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                eb: device.create_buffer(&wgpu::BufferDescriptor{label:Some("EnvData"),size:4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                mcb: device.create_buffer(&wgpu::BufferDescriptor{label:Some("MatCapData"),size:8,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                fog_prev_cam_buf: device.create_buffer(&wgpu::BufferDescriptor{label:Some("FogPrevCam"),size:64,usage:wgpu::BufferUsages::UNIFORM|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                raster_depth_dummy: device.create_texture(&wgpu::TextureDescriptor{label:Some("raster_depth_dummy"),size:wgpu::Extent3d{width:1,height:1,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING,view_formats:&[]}),
                bake_tex_dummy: device.create_texture(&wgpu::TextureDescriptor{label:Some("bake_tex_dummy"),size:wgpu::Extent3d{width:1,height:1,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D3,format:wgpu::TextureFormat::R32Float,usage:wgpu::TextureUsages::STORAGE_BINDING,view_formats:&[]}),
                temporal_dummy_mask: device.create_texture(&wgpu::TextureDescriptor{label:Some("temporal_dummy_mask"),size:wgpu::Extent3d{width:1,height:1,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::R32Uint,usage:wgpu::TextureUsages::TEXTURE_BINDING,view_formats:&[]}),
                temporal_march_flags_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("TemporalMarchFlags"),size:std::mem::size_of::<graphics::TemporalMarchFlags>() as u64,usage:wgpu::BufferUsages::UNIFORM|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                bevel_preview_bgl,
                bevel_edge_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("BevelEdges"),size:std::mem::size_of::<graphics::bevel_preview::GpuBevelEdge>()as u64*256,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                bevel_params_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("BevelParams"),size:std::mem::size_of::<graphics::bevel_preview::GpuBevelParams>()as u64,usage:wgpu::BufferUsages::UNIFORM|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                bevel_grid_cells_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("BevelGridCells"),size:std::mem::size_of::<graphics::bevel_preview::GpuGridCell>()as u64*grid_total as u64,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                bevel_grid_indices_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("BevelGridIdx"),size:4*256*8,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                compositor_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("CompBuf"),size:256*4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                light_link_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("LightLink"),size:1024*4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                light_cluster_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("LightCluster"),size:32*4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                tex_atlas_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("TexAtlas"),size:4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                tex_meta_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("TexMeta"),size:16,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                light_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("LightData"),size:68,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                bvh_node_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("BvhNodes"),size:32,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                bvh_index_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("BvhIndices"),size:4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                raster: render::RasterPipeline::new(device), shadows: render::ShadowMapSystem::new(device), picking: render::PickingPipeline::new(device),
                // Compiled here on the background init thread with every other pipeline
                // (all pipelines compile off the frame thread — no ensure_* fallback needed).
                line_overlay: render::LineOverlayPipeline::new(device),
                face_overlay: render::FaceOverlayPipeline::new(device),
                pipelines: Vec::with_capacity(12),
                step: 0,
                start_time: std::time::Instant::now(),
            })
        } else { None };
        // Spawn texture server before building Self (need both proc + status msg)
        let t_server = std::time::Instant::now();
        let (server_proc, server_msg) = gpu_init::spawn_texture_server(7860);
        eprintln!("[TIMING] Texture server spawn: {:?}", t_server.elapsed());
        let app = Self::construct(gpu_init, gpu_info, cpu_volume, server_proc, Some(server_msg));
        eprintln!("[TIMING] ====== TOTAL STARTUP: {:?} ======", t_total.elapsed());
        app
    }

    /// HEADLESS constructor for in-process integration tests (the surface-sculpt fuzz harness):
    /// the REAL `FourthLayerApp` with `gpu: None` (exactly the state production runs in before
    /// pipeline warmup) and no side-effect spawns (no texture-server process, no startup toast).
    /// Everything else — scene graph, caches, sculpt state, undo history — is the production
    /// construction path (`construct`), NOT a parallel test double.
    #[cfg(test)]
    pub(crate) fn new_headless() -> Self {
        Self::construct(None, GpuInfo::default(), SdfVolume::new_empty(128, 2.0), None, None)
    }

    /// The ONE app-construction body shared by `new` (eframe) and `new_headless` (tests).
    /// PURE relocation of the former tail of `new` — field init + post-init bridge calls.
    fn construct(
        gpu_init: Option<GpuInitState>,
        gpu_info: GpuInfo,
        cpu_volume: SdfVolume,
        server_proc: Option<std::process::Child>,
        server_msg: Option<String>,
    ) -> Self {
        let tweak = ToolSettings::load();
        // IBL: env worker → poll channel + the one-time BRDF LUT worker (env_ibl.rs).
        // The LUT computes off-thread; poll uploads it when ready (env_lut_res gates the shader).
        // (OnceLock-guarded; skipped headless so per-test apps don't each spawn a LUT thread.)
        let (env_load_tx, env_load_rx) = std::sync::mpsc::channel();
        let (poly_lod_tx, poly_lod_rx) = std::sync::mpsc::channel();
        if gpu_init.is_some() {
            crate::env_ibl::spawn_brdf_lut_worker();
        }
        let mut app = Self {
            is_dirty:true, dirty: DirtyFlags::default(), // (light_config → scene, last_rect_size → viewport — Phase 1)
            render: RenderSettings::default(),
            compositor: CompositorStack::default(),
            screenshot_requested: false,
            pending_screenshot: None,
            visdiff_frame: 0,
            visdiff_shot: None,
            nurbsdiff_frame: 0,
            nurbsdiff_seq_ok: false,
            nurbsdiff_rim_ok: false,
            nurbsdiff_loop_ok: false,
            soak: crate::soak::SoakHarness::default(),
            prev_cam_uniforms: graphics::PrevCamera { cam_pos: [0.0;4], cam_forward: [0.0;4], cam_right: [0.0;4], cam_up: [0.0;4] },
            viewport: ViewportState::default(),
            tool: ToolState::default(),
            ui: UiWindowState { startup_toast: server_msg, ..Default::default() },
            commands: Vec::new(),
            command_log: Vec::new(),
            macro_recording: false,
            macro_buffer: Vec::new(),
            brush:BrushSettings::default(), // (work_mode set by ToolState::default — Phase 1)
            sdf_sculpt_op: core_math::primitives::SculptOp::Push,
            sdf_sculpt_radius: 0.5,
            sdf_sculpt_strength: 0.2,
            sdf_sculpt_active: false,
            sdf_brush_preview_pos: None,
            sdf_sculpt_falloff: core_math::sculpt_layer::BrushFalloff::Smooth,
            sdf_stroke_interpolator: core_math::sculpt_layer::StrokeInterpolator::new(0.1),
            sculpt_source_visible: None,
            scene: scene_state::SceneState::new(),
            cache: cache_state::CacheState::new(cpu_volume),
            sel: SelectionState::default(),
            // (usel set by SelectionState::default — Phase 1)
            modeling_context: ModelingContext::None,
            overlay_lines: Vec::new(),
            overlay_lines_fp: 0,
            overlay_lines_depth_count: 0,
            overlay_wire_capped: false,
            overlay_wire_seg_count: 0,
            overlay_wire_cap_toast_shown: false,
            overlay_faces: Vec::new(),
            overlay_faces_fp: 0,
            wire_local_cache: std::collections::HashMap::new(),
            poly_wire_local_cache: std::collections::HashMap::new(),
            quick_normal_ghost: None,
            poly_work_mode: PolyWorkMode::Object,
            nurbs_work_mode: NurbsWorkMode::Object,
            poly_tool: PolyTool::Idle,
            loop_cut_count: 1,
            loop_cut_live: false,
            loop_cut_selected: true, // multi-edge selection drives partial / multi-ring cuts
            loop_cut_follow_curvature: false, // Blender parity: smoothness 0 unless asked for
            loop_cut_curvature: 1.0,
            // MESHmachine Loop Select panel defaults (owner screenshot: Min Angle 41, Steps 3).
            loop_sel_always_loop: true,
            loop_sel_min_angle: 41.0,
            loop_sel_use_edge_limit: false,
            loop_sel_steps: 3,
            poly_inset_individual: false, // Blender default = REGION ("group faces")
            poly_circle_relax: 4,         // AZRA extension (Blender = 0): keep the inner grid unsheared
            knife_points: Vec::new(),
            knife_snap_pos: None,
            knife_snap_kind: 0,
            knife_snap_edge: None,
            knife_free: false,
            knife_free_slice: false,
            knife_free_view: None,
            knife_shape_kind: 0,
            knife_shape_anchor: None,
            knife_shape_cursor: None,
            knife_shape_segments: 3, // default arc density for rect corner bevel / circle
            knife_shape_scroll_accum: 0.0,
            knife_shape_corner_bevel: 0.0,
            knife_shape_screen_a: None,
            knife_shape_screen_b: None,
            knife_shape_from_center: false,
            knife_shape_rotation_rad: 0.0,
            knife_shape_rot_last_mouse: None,
            knife_shape_move_last_mouse: None,
            knife_shape_array_count: 1,
            knife_shape_depth_mode: false,
            knife_shape_depth: 0.0,
            knife_shape_depth_committable: false,
            knife_shape_depth_frozen: false,
            // ON by default (owner 2026-08-11: "knife cut should also auto set selection, set
            // the resulting geo from boolean") — a boolean cut hands back its new faces so the
            // next op (bevel / inset / material) needs no hunting. Off = the old quiet flow.
            knife_select_after_cut: true,
            knife_bool_op: crate::poly_knife::KnifeBoolOp::Subtract,
            knife_last_result_faces: std::collections::HashSet::new(),
            knife_free_plane: None,
            nurbs_tool: NurbsTool::Idle,
            poly_sel: PolySelection::default(),
            poly_vertex_debug: false,
            poly_proportional: false,
            poly_prop_radius: 1.0,
            poly_prop_falloff: PropFalloff::Smooth,
            poly_prop_origin: None,
            poly_prop_handle_hovered: false,
            poly_mesh_dirty: None,
            poly_display_dirty: std::collections::HashSet::new(),
            poly_sync_calls_this_frame: 0,
            poly_sync_calls_last_frame: 0,
            poly_sync_marks_this_frame: 0,
            poly_sync_marks_last_frame: 0,
            poly_pick_rebuilds_this_frame: 0,
            poly_pick_rebuild_ms_this_frame: 0.0,
            extrude_perf_calls: 0,
            extrude_perf_delta_ms: 0.0,
            extrude_perf_sync_ms: 0.0,
            extrude_perf_total_ms: 0.0,
            extrude_update_frame: u64::MAX,
            poly_pick_cache: PolyPickCache::new(),
            poly_display_cache: std::collections::HashMap::new(),
            poly_edit_generation: 0,
            poly_display_key_memo: std::cell::RefCell::new(std::collections::HashMap::new()),
            poly_lod_latch_gen: u64::MAX, // force first LOD poll
            poly_lod_latch_cam_bucket: u64::MAX,
            poly_modifier_evals_this_frame: 0,
            poly_modifier_evals_last_frame: 0,
            poly_sel_outline_ms: 0.0,
            poly_prefix_cache: std::collections::HashMap::new(),
            poly_prefix_total_bytes: 0,
            poly_prefix_clock: 0,
            poly_remesh_incr: None,
            poly_sculpt_working: std::collections::HashMap::new(),
            terrain_world: crate::terrain::TerrainWorld::new(),
            terrain_remesh_jobs: Vec::new(),
            terrain_remesh_gen: std::collections::HashMap::new(),
            poly_remesh_diverged: std::collections::HashSet::new(),
            mesh_cleanup_base: None,
            poly_lod_cache: std::collections::HashMap::new(),
            poly_sync_cost_ms: 8.0,
            poly_sync_last_at: std::time::Instant::now(),
            poly_lod_prev_keys: std::collections::HashSet::new(),
            poly_lod_jobs: std::collections::HashSet::new(),
            poly_lod_tx: poly_lod_tx.clone(),
            poly_lod_rx,
            poly_lod_shared: std::collections::HashMap::new(),
            draw_stream: Vec::new(),
            draw_stream_stale: true,
            draw_stream_rebuilds_last_frame: 0,
            structural_materials: Vec::new(),
            structural_draw_calls: Vec::new(),
            structural_draw_kind: Vec::new(),
            structural_draw_kind_payload: Vec::new(),
            structural_world_aabb: Vec::new(),
            structural_draw_stale: true,
            structural_material_gen: 0,
            structural_draw_rebuilds_last_frame: 0,
            structural_outline_key: 0,
            structural_any_sdf: false,
            raster_upload_plane_key: 0,
            probe_frame_side: 0,
            cull_batches: Vec::new(),
            poly_meshes_drawable_ok: false,
            poly_last_click_t: -1.0,
            poly_last_click_p: eframe::egui::Pos2::ZERO,
            poly_active_vertex: None,
            poly_face_history: Vec::new(),
            poly_edge_history: Vec::new(),
            poly_vert_history: Vec::new(),
            poly_sel_shape: SelectionShape::Box,
            poly_paint_radius: 10.0,
            poly_geom_issues: None,
            poly_geom_issues_key: None,
            poly_geom_issues_last_gen: None,
            poly_geom_issues_at: std::time::Instant::now(),
            light_occluded: std::collections::HashMap::new(),
            light_probe_inflight: Vec::new(),
            poly_pick_image: None,
            poly_pick_pending: None,
            poly_pick_last_cam_key: 0,
            poly_pick_last_mesh_key: 0,
            poly_merge_menu_pos: None,
            nurbs_sel: NurbsSelection::default(),
            curve_pt_sel: std::collections::BTreeSet::new(),
            curve_copy_angle_arm: None,
            curve_pt_hover: None,
            curve_pt_owner: None,
            spine_pt_sel: std::collections::BTreeSet::new(),
            spine_pt_hover: None,
            spine_pt_owner: None,
            spine_pt_drag: None,
            bevel_pv: BevelPreviewState::default(),
            poly_bevel_history: Vec::new(),
            // (viewport_mouse/viewport_rect set by ViewportState::default — Phase 1)
            // (pie menus + context menu moved to ui)
            history:UndoHistory::new(50), pending_episode: None, // (toasts set by UiWindowState::default — Phase 1)
            sculpt: SculptState::default(),
            keybindings:KeyBindings::load(),
            outliner: OutlinerState::default(),
            // layers/objects removed — scene graph is sole hierarchy
            ai: AiState::default(),
            gpu: None, gpu_info, gpu_init, worker_gpus: Vec::new(), job_dispatcher: None,
            shader_dirty: false,
            texture_server_proc: server_proc,
            // (startup_toast moved into ui init above — de-God-Object Phase 1)
            lab_mesh_sdf: crate::LabMeshSdfState::default(),
            show_dirty_regions: false,
            test_blend_overlay: false,
            ssgi_debug: app_types::SsgiDebug::default(),
            log_render_mode: false,
            bool_edges: vec![], bool_edge_hovered: None, bool_edge_dirty: true,
            pending_pick: None,
            pending_material_drop: None,
            material_import_rx: Vec::new(),
            pending_surface_pick: None,
            // (lights + next_light_id moved to scene: SceneState)
            // edit_target + selected_faces moved to sel
            // (raw_tab_events/tab_physically_held/any_text_focused set by InputSystem::default — Phase 1)
            env_hdr_data: Vec::new(), env_hdr_mip_data: Vec::new(),
            env_hdr_width: 0, env_hdr_height: 0,
            env_load_tx, env_load_rx,
            env_load_gen: 0,
            env_clear_pending: false,
            env_prefilter_queue: Vec::new(),
            env_source_path: None,
            brdf_lut_uploaded: false,
            file_dialogs: crate::file_dialog::FileDialogService::new(),
            tip_preview_cache: crate::brush_tip_lib::BrushTipPreviewCache::new(),
            matcap_data: Vec::new(), matcap_width: 0, matcap_height: 0,
            tex_atlas_data: Vec::new(), tex_atlas_meta: Vec::new(), tex_atlas_next_id: 1, tex_atlas_dirty: false,
            thumb_bridge: crate::content_library::ThumbnailBridge::new(),
            library: LibraryState::default(),
            library_scan_rx: None,
            tweak,
            layout: UiLayoutConfig::load(),
            // (command_palette set by UiWindowState::default — Phase 1)
            workspace_mgr: WorkspaceManager::load(),
            // (_ws_dragging/ws_add_popup/ws_add_search/ws_context_menu set by UiWindowState::default — Phase 1)
            // CUDA renderer (feature-gated)
            #[cfg(feature = "cuda")]
            cuda_renderer: {
                if cuda_render::cuda_available() {
                    eprintln!("[FourthLayer] CUDA available — initializing renderer");
                    cuda_render::CudaRenderer::try_new(640, 360)
                } else {
                    eprintln!("[FourthLayer] CUDA not available — using WGSL fallback");
                    None
                }
            },
            // Project state
            project: project_state::ProjectState::new(crate::project::load_recent_files()),
            // (mat_preview_cache set by CacheState::new — Phase 1)
            experimental: experimental::ExperimentalFlags::load(),
            panel_layout: panel::UiLayout::load_from_file().unwrap_or_default(),
            perf: PerfCounters::default(),
            scheduler: frame_scheduler::FrameScheduler::new(),
            diag_frame: 0,
            diag_prev: DiagState::default(),
            interaction_episode: InteractionEpisode::default(),
            prev_sel_hash: 0,
            prev_edit_target: None,
            prev_show_sel_rim: true,
            // (isolated_body_nid moved to scene: SceneState)
            isolate_context_ghost: true,
            prim_clipboard: Vec::new(),
            clipboard: clipboard::Clipboard::default(),
            input_system: input_system::InputSystem::default(),
            // Load persisted v2 layout (dock/widths/hidden/library/workspace) —
            // defaults on missing/unreadable blob. BEFORE the first frame.
            ui2: ui2::persist::ui2_state_load(),
            library_thumbnail_path: None,
            library_thumbnail_focus_nid: None,
            pending_library_save: None,
            pending_library_save_request: None,
            // (post_load_bodies/post_load_warmup set by CacheState::new — Phase 1)
            prev_frame_wall_ms: 0.0,
            // (pending_file_open set by ProjectState::new — Phase 1)
        };
        app.viewport.camera.distance = app.tweak.default_cam_dist;
        app.apply_user_defaults(); // "Set as default" overrides (~/.azra/user_defaults.json)
        // C.3: CPU AABB Hi-Z default OFF (roadmap risk — frame-thread cost). Opt-in to measure.
        // AZRA_NO_HIZ=1 forces off (scoreboard R9 sabotage) even if the toggle is on.
        if std::env::var_os("AZRA_CPU_OCCLUSION").is_some()
            && std::env::var_os("AZRA_NO_HIZ").is_none()
        {
            app.render.occlusion_cull = true;
            eprintln!("[FourthLayer] AZRA_CPU_OCCLUSION: CPU AABB Hi-Z enabled (low-yield skip active)");
        }
        if std::env::var_os("AZRA_NO_HIZ").is_some() {
            app.render.occlusion_cull = false;
            eprintln!("[FourthLayer] AZRA_NO_HIZ: occlusion cull forced OFF");
        }
        app.build_graph_from_flat();
        // Empty start: NO default Body.001. First SDF prim/create path calls
        // `ensure_sdf_body()` via `add_prim_to_graph` and brings the body online then.
        // (Owner 2026-08-06: always needing a body in the outliner is a bug.)
        app.scene.sdf_body_nid = core_math::scene_graph::NodeId(0);
        app.scene.active_body_nid = core_math::scene_graph::NodeId(0);
        // Add default light to scene graph (graph is the sole owner of lights)
        {
            let root = app.scene.scene_graph.root();
            let default_light = core_math::scene_graph::LightData::default();
            app.scene.scene_graph.add_child(root, core_math::scene_graph::NodePayload::Light(default_light));
        }
        // Phase-D kill switches: env vars set the STARTING state; the Developer
        // Tweaks panel flips them live from there (headless runs unchanged).
        app.render.dbg_gpu_cull = !std::env::var("AZRA_NO_GPU_CULL").is_ok_and(|v| v == "1");
        app.render.dbg_hzb = !std::env::var("AZRA_HZB").is_ok_and(|v| v == "0")
            && std::env::var_os("AZRA_NO_HIZ").is_none();
        app.render.dbg_hzb_late = !std::env::var("AZRA_HZB_NO_LATE").is_ok_and(|v| v == "1");
        app.render.dbg_clusters = std::env::var("AZRA_CLUSTERS").is_ok_and(|v| v == "1");
        app.render.dbg_cluster_passthrough =
            std::env::var("AZRA_CLUSTER_CUT_PASSTHROUGH").is_ok_and(|v| v == "1");
        app.render.dbg_cluster_force_l0 =
            std::env::var("AZRA_CLUSTER_FORCE_L0").is_ok_and(|v| v == "1");
        app.render.dbg_cluster_view =
            std::env::var("AZRA_CLUSTER_DEBUG_VIEW").is_ok_and(|v| v == "1");
        // D4 PARKED — owner visual still islands after v6/v7 + cohesion (2026-08-05).
        // Default OFF. AZRA_CLUSTERS=1 is research-only; not a ship path.
        if app.render.dbg_clusters {
            eprintln!(
                "[D4] PARKED (research only) — env forced ON | force_l0={} passthrough={} \
                 debug_view={} | 1x9 still fragment islands despite cohesive bake; \
                 leave OFF for real work",
                u32::from(app.render.dbg_cluster_force_l0),
                u32::from(app.render.dbg_cluster_passthrough),
                u32::from(app.render.dbg_cluster_view),
            );
        }
        app.rebake_no_undo();
        app.register_input_actions();
        // C.1 / B.4: AFTER graph is final (build_graph_from_flat wipes nodes).
        // Example: AZRA_INSTANCE_STRESS=1000 AZRA_SCOREBOARD=1
        if let Ok(s) = std::env::var("AZRA_INSTANCE_STRESS") {
            if let Ok(n) = s.parse::<usize>() {
                if n > 0 {
                    let lines = crate::stress_test::spawn_shared_poly_instances(&mut app, n);
                    for line in &lines {
                        eprintln!("[INSTANCE_STRESS] {line}");
                    }
                }
            }
        }
        // D1 demo scene: terrain + sun + mountains + trees (owner request 2026-08-04).
        // AZRA_DEMO_SCENE=1 → default 400 trees; AZRA_DEMO_SCENE=N → N trees.
        if let Ok(s) = std::env::var("AZRA_DEMO_SCENE") {
            let trees = s.parse::<usize>().ok().filter(|&n| n > 1).unwrap_or(400);
            let lines = crate::stress_test::spawn_demo_terrain_scene(&mut app, trees);
            for line in &lines {
                eprintln!("[DEMO_SCENE] {line}");
            }
            // AZRA_DEMO_PT=1 → start in Path Trace (headless PT perf measurement).
            if std::env::var("AZRA_DEMO_PT").is_ok_and(|v| v == "1") {
                app.render.shading_mode = crate::ShadingMode::PathTrace;
                eprintln!("[DEMO_SCENE] starting in PathTrace mode");
            }
        }
        // D2 C6 interior occlusion gate scene (camera inside a walled room).
        if std::env::var("AZRA_DEMO_INTERIOR").is_ok_and(|v| v == "1") {
            let lines = crate::stress_test::spawn_demo_interior_scene(&mut app);
            for line in &lines {
                eprintln!("[DEMO_INTERIOR] {line}");
            }
        }
        // C.6: multi-mesh gate — N×M interleaved. Example: AZRA_MULTI_MESH_STRESS=5x200
        if let Ok(s) = std::env::var("AZRA_MULTI_MESH_STRESS") {
            let lower = s.to_lowercase();
            let parts: Vec<&str> = lower.split('x').collect();
            if parts.len() == 2 {
                if let (Ok(n), Ok(m)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                    if n > 0 && m > 0 {
                        let lines =
                            crate::stress_test::spawn_multi_mesh_poly_instances(&mut app, n, m);
                        for line in &lines {
                            eprintln!("[C6_MULTI_MESH] {line}");
                        }
                    }
                }
            }
        }
        // Overdraw A/B: AZRA_CAM_DISTANCE=N pulls orbit distance so the scene covers a
        // small screen footprint (triangle count unchanged). If scoreboard `disp` collapses,
        // the multi-mesh cost is fragment/overdraw-bound; if it holds, vertex/submission.
        if let Ok(s) = std::env::var("AZRA_CAM_DISTANCE") {
            if let Ok(d) = s.parse::<f32>() {
                if d > 0.01 {
                    app.viewport.camera.distance = d;
                    eprintln!("[CAM] AZRA_CAM_DISTANCE={d}");
                }
            }
        }
        // Phase D entry (G10): unique dense mesh. AZRA_DENSE_STRESS=1 or 3x8 (bodies×CC levels).
        if let Ok(s) = std::env::var("AZRA_DENSE_STRESS") {
            let lower = s.to_lowercase();
            let (bodies, levels) = if let Some((a, b)) = lower.split_once('x') {
                (
                    a.parse::<usize>().unwrap_or(1).max(1),
                    b.parse::<u32>().unwrap_or(7).max(1),
                )
            } else if let Ok(n) = lower.parse::<usize>() {
                (n.max(1), 7u32)
            } else {
                (1usize, 7u32)
            };
            let lines = crate::stress_test::spawn_dense_poly_bodies(&mut app, bodies, levels);
            for line in &lines {
                eprintln!("[DENSE_STRESS] {line}");
            }
        }
        app
    }

    // ---- Scene Graph ↔ Flat Cache Bridge ----

    /// Ensure at least one SdfBody node exists in the scene graph. Returns its NodeId.
    ///
    /// Called lazily on first SDF prim / mesh link — not at app start. Creates
    /// `Body.00N`, sets `sdf_body_nid` + `active_body_nid` when they point at nothing,
    /// and dirties the outliner so the new body appears immediately.
    pub(crate) fn ensure_sdf_body(&mut self) -> NodeId {
        if let Some((nid, _)) = self.scene.scene_graph.sdf_bodies().next() {
            // Heal pointers if they still point at root / a deleted body.
            if self.scene.scene_graph.body_primitives(self.scene.sdf_body_nid).is_none() {
                self.scene.sdf_body_nid = nid;
            }
            if self.scene.scene_graph.body_primitives(self.scene.active_body_nid).is_none() {
                self.scene.active_body_nid = nid;
            }
            return nid;
        }
        let root = self.scene.scene_graph.root();
        let id = self.scene.next_node_name_id.max(1);
        self.scene.next_node_name_id = id + 1;
        let name = format!("Body.{id:03}");
        let nid = self.scene.scene_graph.add_child(root, NodePayload::SdfBody {
            name,
            primitives: Vec::new(),
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: core_math::primitives::BlendProfile::default(),
            material: core_math::primitives::PbrMaterial::default(),
            child_order: Vec::new(),
            mesh_modifiers: Vec::new(),
            field_modifiers: Vec::new(),
        });
        self.scene.sdf_body_nid = nid;
        self.scene.active_body_nid = nid;
        self.mark_outliner_dirty();
        self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        nid
    }

    /// Build the scene graph from the current flat `self.scene.primitives`.
    /// All primitives go into a single SdfBody under root.
    /// Called once during init and on project load.
    pub(crate) fn build_graph_from_flat(&mut self) {
        let mut graph = SceneGraph::new();
        if !self.scene.primitives.is_empty() {
            let root = graph.root();
            let body = graph.add_child(root, NodePayload::SdfBody {
                name: "Body.001".into(),
                primitives: self.scene.primitives.clone(),
                mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 },
                origin: Vec3::ZERO,

                body_rotation: glam::Quat::IDENTITY,
                body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
                blend_profile: core_math::primitives::BlendProfile::default(), material: core_math::primitives::PbrMaterial::default(),
                child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            // `primitives` were pre-populated above → synthesise child_order (prims-first).
            graph.ensure_body_child_order(body);
        }
        self.scene.scene_graph = graph;
        self.rebuild_prim_body_map();
    }

    /// Rebuild only the body map (used when graph was built FROM flat data,
    /// so primitives are already correct and we just need the index mapping).
    pub(crate) fn rebuild_prim_body_map(&mut self) {
        self.scene.prim_body_map.clear();
        for (nid, idx, _) in self.scene.scene_graph.all_primitives_ordered() {
            self.scene.prim_body_map.push((nid, idx));
        }
    }

    // rebuild_shader_pipelines moved to dirty.rs
    // Undo functions moved to undo.rs

    // Command queue (push_cmd, execute_and_log, process_commands) moved to app_commands.rs

    pub(crate) fn toast(&mut self, msg:&str) { self.ui.toasts.push(Toast{message:msg.into(),remaining:self.tweak.toast_duration}); }

    // Selection functions moved to selection.rs
    // sync_light_to_gpu moved to scene_actions.rs
    // toggle_select, selection_median moved to selection.rs

    // dev_rebuild_and_restart, dev_restart_no_build moved to dev_tools.rs

    // poly_selection_center moved to selection.rs
    // active_rotation_quat moved to camera.rs

    /// Derive the modeling context from the current selection.
    /// Called once per frame before tool/UI logic.
    pub(crate) fn derive_modeling_context(&mut self) {
        use core_math::scene_graph::NodePayload;
        // SDF primitive selection takes priority when in SDF workflow
        if self.active_prim_index_unified().is_some() && self.sel.usel.active_node().is_none() {
            self.modeling_context = ModelingContext::Sdf;
            return;
        }
        if let Some(nid) = self.sel.usel.active_node() {
            if let Some(node) = self.scene.scene_graph.get(nid) {
                self.modeling_context = match &node.payload {
                    NodePayload::SdfBody { .. } => ModelingContext::Sdf,
                    NodePayload::PolygonalBody { .. } => ModelingContext::Polygonal,
                    NodePayload::NurbsBody { .. } | NodePayload::NurbsSolid { .. } => {
                        ModelingContext::Nurbs
                    }
                    NodePayload::ImportedMesh { .. } => ModelingContext::ImportedMesh,
                    NodePayload::VoxelBody { .. } => ModelingContext::ImportedMesh, // P0.5: voxel ctx refined in step 5

                    NodePayload::Light(_) => ModelingContext::Light,
                    NodePayload::Camera { .. } => ModelingContext::Camera,
                    _ => ModelingContext::None,
                };
                return;
            }
        }
        // Fallback: active_body_nid tracks the working body even when nothing is selected
        // (e.g. just entered SDF edit mode, no prim clicked yet)
        if let Some(node) = self.scene.scene_graph.get(self.scene.active_body_nid) {
            if matches!(&node.payload, NodePayload::SdfBody { .. }) {
                self.modeling_context = ModelingContext::Sdf;
                return;
            }
        }
        self.modeling_context = ModelingContext::None;
    }

    // status_line_hint moved to ui_panels.rs
    // cam_ray, raycast_scene, scene_distance, detect_hovered_face, detect_hovered_face_single moved to camera.rs

    // Face push/pull moved to face_ops.rs
    // draw_face_feedback, draw_snap_indicator moved to drawing.rs

    // compute_snap_candidates moved to snapping.rs
    // ---------------------------------------------------------------
    // BoolCut tool helpers
    // ---------------------------------------------------------------

    // surface_tangent_frame, frame_to_rotation_deg, ray_plane_hit moved to camera.rs
    // find_smart_snap, draw_smart_snap_bar, draw_smart_snap_indicator moved to smart_snap.rs
    // pivot editing methods moved to pivot.rs
    // delete_selected, duplicate_selected, focus_selected, modal transform, and gizmo methods moved to gizmo.rs
    // drawing/overlay methods moved to drawing.rs
}
