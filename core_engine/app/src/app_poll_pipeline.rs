// core_engine/app/src/app_poll_pipeline.rs
//
// Slice (app_poll de-god, PURE RELOCATION) — byte-for-byte method bodies moved out of app_poll.rs;
// no logic change. poll_background_tasks (orchestrator) stays in app_poll.rs and calls these.

use crate::*;
use eframe::egui;
use eframe::wgpu;

impl AzraApp {
    /// Poll background WGSL pipeline compilation (hot-swaps each pipeline as it finishes).
    pub(crate) fn poll_sdf_compile(&mut self, ctx: &egui::Context) {
        let mut got_any = false;
        if let Some(gpu) = &mut self.gpu {
            // On-demand DF-full (first Pbr/Lit — never blocks the frame thread).
            if let Some(rx) = gpu.pipelines.df_full_compile_rx.take() {
                match rx.try_recv() {
                    Ok(p) => {
                        gpu.pipelines.deferred_pipeline = Some(p);
                        gpu.bind_groups.deferred_bg = None;
                        got_any = true;
                        eprintln!("[Azra] DF-full pipeline hot-swapped (on-demand) — full PBR materials active");
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        gpu.pipelines.df_full_compile_rx = Some(rx);
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        // Thread died without a pipeline — allow a later retry.
                        if gpu.pipelines.deferred_pipeline.is_none() {
                            gpu.pipelines.df_full_compile_started = false;
                            eprintln!("[Azra] DF-full on-demand compile disconnected without result — will retry on next PBR frame");
                        }
                    }
                }
            }
            if let Some(rx) = gpu.pipelines.sdf_compile_rx.take() {
                loop {
                    match rx.try_recv() {
                        Ok(msg) => {
                            got_any = true;
                            match msg {
                                PipelineReady::DeferredLite(p) => {
                                    gpu.pipelines.deferred_lite_pipeline = Some(p);
                                    gpu.bind_groups.deferred_bg = None;
                                    eprintln!("[Azra] DF-lite pipeline hot-swapped");
                                }
                                PipelineReady::RaymarchLite(p) => {
                                    gpu.pipelines.pipeline_lite = Some(p);
                                    eprintln!("[Azra] Raymarch-lite pipeline hot-swapped");
                                }
                                PipelineReady::SculptRaymarch(p) => {
                                    gpu.pipelines.volume_preview_sculpt_pipeline = Some(p);
                                    eprintln!("[Azra] Sculpt-voxel-raymarch pipeline hot-swapped");
                                }
                                PipelineReady::DeferredFull(p) => {
                                    gpu.pipelines.deferred_pipeline = Some(p);
                                    gpu.pipelines.df_full_compile_started = true;
                                    gpu.pipelines.df_full_compile_rx = None; // drop on-demand race if any
                                    gpu.bind_groups.deferred_bg = None;
                                    eprintln!("[Azra] DF-full pipeline hot-swapped — full materials active");
                                }
                                PipelineReady::Fog { full, lite } => {
                                    gpu.pipelines.fog_pipeline = Some(full);
                                    gpu.pipelines.fog_lite_pipeline = Some(lite);
                                    eprintln!("[Azra] Fog pipelines (full+lite) hot-swapped (background)");
                                }
                                PipelineReady::Fxaa(p) => {
                                    // BGL created lazily in viewport_render on first use
                                    gpu.pipelines.fxaa_pipeline = Some(p);
                                    eprintln!("[Azra] FXAA pipeline hot-swapped (background)");
                                }
                                PipelineReady::Pathtrace(p) => {
                                    // Carries the TRIANGLE PT (the only PT since the analytical delete).
                                    gpu.pipelines.pt_tri_pipeline = Some(p);
                                    eprintln!("[Azra] Pathtrace-tri pipeline hot-swapped (background)");
                                }
                                PipelineReady::PtDenoise(p) => {
                                    // Create BGL + params buffer on main thread (bg thread only compiled the pipeline).
                                    // Section J: always rebuild BGL from single source (albedo binding 4).
                                    let device = &gpu.device;
                                    gpu.bind_groups.pt_denoise_bgl = Some(device.create_bind_group_layout(
                                        &wgpu::BindGroupLayoutDescriptor {
                                            label: Some("pt_denoise_bgl"),
                                            entries: &graphics::bgl::pt_denoise_bgl_entries(),
                                        },
                                    ));
                                    if gpu.buffers.pt_denoise_params_buffer.as_ref().map_or(true, |b| b.size() < 32) {
                                        gpu.buffers.pt_denoise_params_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                                            label: Some("pt_denoise_params"), size: 32,
                                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                                            mapped_at_creation: false,
                                        }));
                                    }
                                    crate::gpu_init::invalidate_pt_denoise_bgs(gpu);
                                    gpu.pipelines.pt_denoise_pipeline = Some(p);
                                    eprintln!("[Azra] PT denoise pipeline hot-swapped (background)");
                                }
                                PipelineReady::PrimaryBake(p) => {
                                    // Create BGL + params buffer on main thread.
                                    // BGL entries — SINGLE SOURCE: graphics::bgl::bake_bgl_entries()
                                    // (must match the background-compiled pipeline's layout).
                                    if gpu.bind_groups.primary_bake_bgl.is_none() {
                                        let device = &gpu.device;
                                        // BGL Single Source rule: entries live in graphics::bgl::bake_bgl_entries(),
                                        // shared with the worker path (app_gpu gpu_jobs.rs ensure_bake_pipeline).
                                        gpu.bind_groups.primary_bake_bgl = Some(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                                            label: Some("primary_bake_bgl"),
                                            entries: &graphics::bgl::bake_bgl_entries(),
                                        }));
                                        gpu.buffers.primary_bake_params_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                                            label: Some("primary_bake_params"),
                                            size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
                                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                                            mapped_at_creation: false,
                                        }));
                                    }
                                    crate::gpu_init::ensure_lab_mesh_grid_dummy(gpu);
                                    gpu.pipelines.primary_bake_pipeline = Some(p);
                                    eprintln!("[Azra] Primary bake pipeline hot-swapped (background)");
                                }
                                PipelineReady::ClassifyBricks(p, f) => {
                                    // STAGE-2 GPU CLASSIFY: same BGL/params as the bake (created in
                                    // the PrimaryBake arm / lazy ensure) — only the pipelines swap.
                                    // STAGE-3: the finalize set swaps atomically with the classify.
                                    gpu.pipelines.classify_bricks_pipeline = Some(p);
                                    gpu.pipelines.classify_finalize_pipelines = Some(f);
                                    eprintln!("[Azra] classify_bricks(+finalize) pipelines hot-swapped (background)");
                                }
                                PipelineReady::BrickFill(p) => {
                                    // Create BGL on main thread
                                    if gpu.bind_groups.brick_fill_bgl.is_none() {
                                        let device = &gpu.device;
                                        gpu.bind_groups.brick_fill_bgl = Some(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                                            label: Some("brick_fill_bgl"),
                                            entries: &[
                                                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
                                                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                                                // binding(2): atlas texture (brick fill writes directly to atlas)
                                                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::R32Float, view_dimension: wgpu::TextureViewDimension::D3 }, count: None },
                                                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
                                                // binding(4): clip_curves
                                                wgpu::BindGroupLayoutEntry { binding: 4, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
                                            ],
                                        }));
                                    }
                                    gpu.pipelines.brick_fill_pipeline = Some(p);
                                    eprintln!("[Azra] Brick fill pipeline hot-swapped (background)");
                                }
                                PipelineReady::Temporal(p) => {
                                    // Create BGL + params buffer on main thread
                                    if gpu.bind_groups.temporal_reproject_bgl.is_none() {
                                        let device = &gpu.device;
                                        gpu.bind_groups.temporal_reproject_bgl = Some(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                                            label: Some("temporal_reproject_bgl"),
                                            entries: &[
                                                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                                                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                                                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::R32Uint, view_dimension: wgpu::TextureViewDimension::D2 }, count: None },
                                            ],
                                        }));
                                        gpu.buffers.temporal_params_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                                            label: Some("TemporalParams"),
                                            size: std::mem::size_of::<graphics::TemporalParams>() as u64,
                                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                                            mapped_at_creation: false,
                                        }));
                                    }
                                    gpu.pipelines.temporal_reproject_pipeline = Some(p);
                                    eprintln!("[Azra] Temporal reproject pipeline hot-swapped (background)");
                                }
                                PipelineReady::SdfMesher(m) => {
                                    // Install ONLY if the lazy fallback didn't beat us to it — the
                                    // live mesher's BGL is referenced by every dispatch this frame,
                                    // and both instances are identical compiles of the same source.
                                    if self.cache.sdf_mesher.is_none() {
                                        self.cache.sdf_mesher = Some(m);
                                        eprintln!("[Azra] SDF DC mesher hot-swapped (background) — first select pays no compile");
                                    } else {
                                        eprintln!("[Azra] SDF DC mesher (background) arrived after lazy compile — dropped");
                                    }
                                }
                                PipelineReady::Ssao(p) => {
                                    // Install only if the lazy ensure didn't beat us: the live
                                    // SsaoResources bind groups reference the installed BGL, and
                                    // swapping pipelines under them would dangle those groups.
                                    if gpu.pipelines.ssao.is_none() {
                                        gpu.pipelines.ssao = Some(p);
                                        eprintln!("[Azra] SSAO pipelines hot-swapped (background)");
                                    } else {
                                        eprintln!("[Azra] SSAO pipelines (background) arrived after lazy compile — dropped");
                                    }
                                }
                                PipelineReady::RtShadow { bgl, pipeline } => {
                                    // CONSOLE_AUDIT B3: install only if lazy ensure did not win.
                                    if gpu.pipelines.rt_shadow_pipeline.is_none() {
                                        gpu.bind_groups.rt_shadow_bgl = Some(bgl);
                                        gpu.pipelines.rt_shadow_pipeline = Some(pipeline);
                                        eprintln!("[Azra] RT-shadow pipeline hot-swapped (background)");
                                    } else {
                                        eprintln!("[Azra] RT-shadow (background) arrived after lazy compile — dropped");
                                    }
                                }
                            }
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            gpu.pipelines.sdf_compile_rx = Some(rx);
                            ctx.request_repaint_after(std::time::Duration::from_millis(100));
                            break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            eprintln!("[Azra] Background WGSL compilation complete — lite={}", gpu.pipelines.pipeline_lite.is_some());
                            break;
                        }
                    }
                }
                let was_ready = gpu.pipelines.wgsl_pipelines_ready;
                // Tier 1: gate "ready" on the PRODUCTION pair (lite + volume-preview), NOT the analytical
                // raymarch `pipeline` — which is now compiled lazily and may legitimately stay None in the
                // default volume/raster workflow. Waiting on it would force the GLSL-BVH fallback forever.
                gpu.pipelines.wgsl_pipelines_ready = gpu.pipelines.pipeline_lite.is_some();
                if gpu.pipelines.wgsl_pipelines_ready && !was_ready {
                    eprintln!("[Azra] *** wgsl_pipelines_ready BECAME TRUE ***");
                }
                if got_any {
                    let count = crate::gpu_init::WarmupManifest::warm_count(&gpu.pipelines);
                    if crate::gpu_init::WarmupManifest::is_all_warm(&gpu.pipelines) {
                        eprintln!("[Azra] *** All {} pipelines warm ***", count);
                    } else if crate::gpu_init::WarmupManifest::is_fully_warm(&gpu.pipelines) {
                        eprintln!("[Azra] Required pipelines warm ({}/12 total)", count);
                    }
                }
            }
        }
        if got_any {
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }
}
