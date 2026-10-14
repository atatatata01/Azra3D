//! GPU pipeline cache + texture server — moved from main.rs.

use eframe::wgpu;

mod cache;
mod bgl;
mod pipelines_bg;
mod pipelines_lazy;

// ---- re-exports (crate::gpu_init::*) — preserve call sites ----
pub(crate) use cache::{load_pipeline_cache_data, pipeline_cache_path, save_pipeline_cache};

pub(crate) use bgl::{
    build_pt_bg, build_pt_bg_half, build_rt_shadow_bg, invalidate_pt_denoise_bgs, rebuild_fog_bg,
};

pub(crate) use pipelines_bg::{
    apply_pt_denoise_frame_budget, clear_texture_safe, compile_classify_finalize_pipelines,
    copy_pt_history_chain, dispatch_pt_denoise_atrous, ensure_deferred_full_pipeline,
    request_deferred_full_pipeline, resolve_pt_denoise_quality, rt_shadow_denoise_dispatch,
    PtDenoiseQuality, WarmupManifest,
};

pub(crate) use pipelines_lazy::{
    ensure_blend_pipeline, ensure_brick_fill_pipeline, ensure_classify_bricks_pipeline,
    ensure_curv_dispatch_args_pipeline, ensure_curv_sum_tile_counters_pipeline,
    ensure_curvature_eval_pipeline, ensure_env_prefilter_pipeline, ensure_fog_pipeline,
    ensure_fxaa_pipeline, ensure_lab_mesh_grid_dummy, ensure_prim_outline_pipeline,
    compile_pt_warmup_stages, ensure_primary_bake_pipeline, ensure_pt_denoise_pipeline, ensure_pt_env_cdf_buffer,
    ensure_pt_reproject_pipeline, ensure_pt_restir_reproject_pipeline, ensure_pt_stats_buffers,
    ensure_pt_interactive_pipeline, ensure_pt_tri_pipeline, ensure_pt_upscale_pipeline, ensure_rt_denoise_pipelines,
    ensure_rt_shadow_pipeline, ensure_ssao_pipelines, ensure_ssgi_pipelines, ensure_temporal_pipeline,
    start_pt_stats_readback, try_complete_pt_stats_readback, LAZY_ANALYTICAL_COMPILE,
};


use graphics::gpu_volume::SdfGpuVolume;


/// Resolve `ai_backend/server/texture_server.py` by walking up from the exe.
pub(crate) fn texture_server_script() -> Option<std::path::PathBuf> {
    // 1) Explicit override
    if let Ok(p) = std::env::var("AZRA_AI_SERVER") {
        let pb = std::path::PathBuf::from(p);
        if pb.exists() { return Some(pb); }
    }
    // 2) Walk up from current_exe
    if let Ok(exe) = std::env::current_exe() {
        let mut dir: &std::path::Path = exe.as_path();
        loop {
            if let Some(parent) = dir.parent() {
                dir = parent;
            } else {
                break;
            }
            let candidate = dir.join("ai_backend").join("server").join("texture_server.py");
            if candidate.exists() { return Some(candidate); }
            if dir.parent().map_or(true, |p| p == dir) { break; }
        }
    }
    // 3) Known project path (dev machine fallback)
    let known = std::path::PathBuf::from(r"F:\PROJECT\FourthLAyerAi\ai_backend\server\texture_server.py");
    if known.exists() { return Some(known); }
    None
}


/// True if something accepts TCP on 127.0.0.1:port.
pub(crate) fn ai_server_listening(port: u16) -> bool {
    let sock: std::net::SocketAddr = match format!("127.0.0.1:{port}").parse() {
        Ok(s) => s,
        Err(_) => return false,
    };
    std::net::TcpStream::connect_timeout(&sock, std::time::Duration::from_millis(200)).is_ok()
}


/// Wait until the AI server responds (or timeout). Returns true if up.
pub(crate) fn wait_for_ai_server(port: u16, timeout_ms: u64) -> bool {
    let start = std::time::Instant::now();
    while (start.elapsed().as_millis() as u64) < timeout_ms {
        if ai_server_listening(port) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    ai_server_listening(port)
}


/// Candidate Python interpreters (absolute paths first — GUI apps often lack PATH).
fn python_candidates(server_path: &std::path::Path) -> Vec<String> {
    let mut cmds: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("AZRA_AI_PYTHON") {
        if !p.is_empty() { cmds.push(p); }
    }
    // Common Windows installs
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let user = std::env::var("USERPROFILE").unwrap_or_default();
    for p in [
        format!(r"{local}\Programs\Python\Python311\python.exe"),
        format!(r"{local}\Programs\Python\Python312\python.exe"),
        format!(r"{local}\Programs\Python\Python310\python.exe"),
        format!(r"{user}\miniconda3\python.exe"),
        format!(r"{user}\anaconda3\python.exe"),
        format!(r"{user}\miniconda3\envs\trellis\python.exe"),
        r"C:\Python311\python.exe".into(),
        r"C:\Python312\python.exe".into(),
    ] {
        if std::path::Path::new(&p).exists() && !cmds.iter().any(|c| c == &p) {
            cmds.push(p);
        }
    }
    // Project venv (fastapi; may lack torch — classical beauty still works)
    if let Some(ai_backend) = server_path.parent().and_then(|s| s.parent()) {
        let win = ai_backend.join(".venv").join("Scripts").join("python.exe");
        if win.exists() {
            let s = win.to_string_lossy().into_owned();
            if !cmds.iter().any(|c| c == &s) { cmds.push(s); }
        }
    }
    for &cmd in &["python", "python3", "py"] {
        cmds.push(cmd.into());
    }
    cmds
}


pub(crate) fn spawn_texture_server(port: u16) -> (Option<std::process::Child>, String) {
    // Already something listening? Don't double-spawn.
    if ai_server_listening(port) {
        return (None, format!("AI server already running on :{port}"));
    }

    let server_path = match texture_server_script() {
        Some(p) => p,
        None => return (None, "texture_server.py not found — set AZRA_AI_SERVER or check project layout".into()),
    };

    let port_str = port.to_string();
    let log_path = std::env::temp_dir().join("fourthLayer_server.log");
    let server_dir = server_path.parent().map(|p| p.to_path_buf());

    let python_cmds = python_candidates(&server_path);

    for cmd in &python_cmds {
        let stdout_sink = std::fs::OpenOptions::new()
            .create(true).append(true).open(&log_path)
            .map(std::process::Stdio::from)
            .unwrap_or_else(|_| std::process::Stdio::null());
        let stderr_sink = std::fs::OpenOptions::new()
            .create(true).append(true).open(&log_path)
            .map(std::process::Stdio::from)
            .unwrap_or_else(|_| std::process::Stdio::null());

        let mut b = std::process::Command::new(cmd.as_str());
        if cmd == "py" || cmd.ends_with(r"\py.exe") || cmd.ends_with("/py") {
            b.arg("-3");
        }
        // Flux.1 Kontext is the default Azra pipeline model (local ComfyUI weights).
        // Fall back server-side if weights missing.
        b.arg(&server_path)
            .arg("--model")
            .arg("flux-kontext")
            .arg("--port")
            .arg(&port_str);
        if let Some(ref dir) = server_dir {
            b.current_dir(dir);
        }
        b.env("PYTHONIOENCODING", "utf-8");
        b.stdout(stdout_sink).stderr(stderr_sink);

        // Detach on Windows so the server survives Azra exit / GUI launch quirks.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            b.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }

        match b.spawn() {
            Ok(mut child) => {
                std::thread::sleep(std::time::Duration::from_millis(80));
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let msg = format!(
                            "AI server exited immediately via {cmd} ({status}) — see {}",
                            log_path.display()
                        );
                        eprintln!("[Azra] {msg}");
                        continue;
                    }
                    _ => {
                        // Do NOT keep Child handle when detached — Drop/kill must not
                        // tear down the shared AI server used by next Azra session.
                        // Leak the handle so OS reaps independently.
                        let pid = child.id();
                        std::mem::forget(child);
                        let msg = format!(
                            "AI server starting (pid {pid} via {cmd}) — log: {}",
                            log_path.display()
                        );
                        eprintln!("[Azra] {msg}");
                        return (None, msg);
                    }
                }
            }
            Err(e) => {
                eprintln!("[Azra] spawn {cmd}: {e}");
                continue;
            }
        }
    }

    (None, format!(
        "Could not start AI server — no working Python.\n\
         Install Python 3.11+ with torch, or set AZRA_AI_PYTHON.\n\
         Log: {}",
        log_path.display()
    ))
}


/// Ensure something is listening on the AI port; spawn + wait if not.
/// Returns (ok, status_message).
pub(crate) fn ensure_texture_server(port: u16) -> (bool, String) {
    if ai_server_listening(port) {
        return (true, format!("AI server OK on :{port}"));
    }
    let (_child, msg) = spawn_texture_server(port);
    // Flux.1 Kontext load can take 1–4 min before uvicorn binds (weights load first).
    if wait_for_ai_server(port, 240_000) {
        (true, format!("AI pipeline ready on :{port} ({msg})"))
    } else if ai_server_listening(port) {
        (true, msg)
    } else {
        (false, format!(
            "AI pipeline not reachable on :{port}. {msg}\n\
             Run: Start_AI_Server.bat  OR  python ai_backend/server/texture_server.py --model flux-kontext --port {port}"
        ))
    }
}


/// Binary entry logic — panic hook, profiling, native-backend wgpu config, run_native.
/// `main.rs` keeps only a thin `fn main()` that calls this (the crate root must own `fn main`).
pub(crate) fn run() -> eframe::Result {
    use eframe::egui;
    // Temporary panic hook: write panic info to a file for debugging
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{}\n{:?}", info, std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write(std::env::temp_dir().join("fourthlayer_panic.txt"), &msg);
        eprintln!("PANIC: {}", info);
    }));
    crate::profiling::profile_init();
    // Force native GPU backend: Vulkan (Windows/Linux) or DX12 (Windows fallback) or Metal (Mac)
    // Override with env: WGPU_BACKEND=vulkan | WGPU_BACKEND=dx12
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([800.0, 500.0])
            .with_position([80.0, 80.0])
            .with_maximized(true),
        vsync: false,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            present_mode: wgpu::PresentMode::Mailbox,
            wgpu_setup: eframe::egui_wgpu::WgpuSetupCreateNew {
            instance_descriptor: wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..Default::default()
            },
            power_preference: wgpu::PowerPreference::HighPerformance,
            device_descriptor: std::sync::Arc::new(|adapter| {
                let mut features = wgpu::Features::empty();
                if adapter.features().contains(app_gpu::native_rt::REQUIRED_FEATURES) {
                    features |= app_gpu::native_rt::REQUIRED_FEATURES;
                }
                if adapter.features().contains(wgpu::Features::PIPELINE_CACHE) {
                    features |= wgpu::Features::PIPELINE_CACHE;
                }
                if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
                    features |= wgpu::Features::TIMESTAMP_QUERY;
                }
                if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
                    features |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
                }
                let spirv_supported = adapter.features().contains(wgpu::Features::SPIRV_SHADER_PASSTHROUGH);
                eprintln!("[GPU] SPIRV_SHADER_PASSTHROUGH: {}", spirv_supported);
                if spirv_supported {
                    features |= wgpu::Features::SPIRV_SHADER_PASSTHROUGH;
                }
                if adapter.features().contains(wgpu::Features::FLOAT32_FILTERABLE) {
                    features |= wgpu::Features::FLOAT32_FILTERABLE;
                    eprintln!("[GPU] FLOAT32_FILTERABLE: true");
                }
                let multi_draw_supported = adapter.features().contains(wgpu::Features::MULTI_DRAW_INDIRECT);
                if multi_draw_supported {
                    features |= wgpu::Features::MULTI_DRAW_INDIRECT;
                    eprintln!("[GPU] MULTI_DRAW_INDIRECT: true");
                }
                // Stage B-1 (SdfMeshPool): per-slot instance selection via `first_instance` in the
                // shared indirect args. Adapter-gated like MULTI_DRAW_INDIRECT; absent → the pool
                // draw falls back to a per-slot draw_indexed_indirect loop (same geometry).
                if adapter.features().contains(wgpu::Features::INDIRECT_FIRST_INSTANCE) {
                    features |= wgpu::Features::INDIRECT_FIRST_INSTANCE;
                    eprintln!("[GPU] INDIRECT_FIRST_INSTANCE: true");
                }
                // D4 commit 7: GPU-decided draw count for the cluster multi-draw —
                // skips the fixed-cap empty-slot fetches (measured gbuf overhead).
                // Absent → fixed-cap self-skip fallback (fails closed).
                if adapter.features().contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT) {
                    features |= wgpu::Features::MULTI_DRAW_INDIRECT_COUNT;
                    eprintln!("[GPU] MULTI_DRAW_INDIRECT_COUNT: true");
                }
                // Required for PT half→full / camera-motion GI reservoir invalidation
                // (`CommandEncoder::clear_texture`). Without this, PT crashes with:
                // "To use clear_texture the CLEAR_TEXTURE feature needs to be enabled"
                // (visdump 2026-07-20 while working in Path Trace).
                if adapter.features().contains(wgpu::Features::CLEAR_TEXTURE) {
                    features |= wgpu::Features::CLEAR_TEXTURE;
                    eprintln!("[GPU] CLEAR_TEXTURE: true");
                }
                // Phase D0: 64-bit buffer atomics for visibility-buffer software raster (optional).
                // Probe only — fail closed to hardware raster if missing. No image_atomic_int64.
                if adapter.features().contains(wgpu::Features::SHADER_INT64) {
                    features |= wgpu::Features::SHADER_INT64;
                    eprintln!("[GPU] SHADER_INT64: true");
                } else {
                    eprintln!("[GPU] SHADER_INT64: false");
                }
                if adapter.features().contains(wgpu::Features::SHADER_INT64_ATOMIC_MIN_MAX) {
                    features |= wgpu::Features::SHADER_INT64_ATOMIC_MIN_MAX;
                    eprintln!("[GPU] SHADER_INT64_ATOMIC_MIN_MAX: true");
                } else {
                    eprintln!("[GPU] SHADER_INT64_ATOMIC_MIN_MAX: false");
                }
                if adapter.features().contains(wgpu::Features::SHADER_INT64_ATOMIC_ALL_OPS) {
                    features |= wgpu::Features::SHADER_INT64_ATOMIC_ALL_OPS;
                    eprintln!("[GPU] SHADER_INT64_ATOMIC_ALL_OPS: true");
                }
                // CONSOLE_AUDIT B1: DC wire GPU path needs POLYGON_MODE_LINE at device create.
                // Without this request, pipeline.rs always sees false and falls back to CPU edges
                // even on RTX 4090 / Vulkan (owner log 2026-08-06).
                if adapter.features().contains(wgpu::Features::POLYGON_MODE_LINE) {
                    features |= wgpu::Features::POLYGON_MODE_LINE;
                    eprintln!("[GPU] POLYGON_MODE_LINE: true (requested)");
                } else {
                    eprintln!("[GPU] POLYGON_MODE_LINE: false (adapter lacks feature)");
                }
                let adapter_limits = adapter.limits();
                eprintln!("[GPU] buffer ceilings: max_buffer_size={} ({:.1} GB), max_storage_buffer_binding_size={} ({:.1} GB)",
                    adapter_limits.max_buffer_size, adapter_limits.max_buffer_size as f64 / 1.0e9,
                    adapter_limits.max_storage_buffer_binding_size, adapter_limits.max_storage_buffer_binding_size as f64 / 1.0e9);
                wgpu::DeviceDescriptor {
                    label: Some("FourthLayer GPU"),
                    required_features: features,
                    required_limits: wgpu::Limits {
                        max_storage_textures_per_shader_stage: adapter_limits.max_storage_textures_per_shader_stage,
                        max_storage_buffers_per_shader_stage: adapter_limits.max_storage_buffers_per_shader_stage,
                        max_buffer_size: adapter_limits.max_buffer_size,
                        max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
                        ..wgpu::Limits::default()
                    },
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                }
            }),
            ..Default::default()
            }.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native("FourthLayer AI", options, Box::new(|cc| Ok(Box::new(crate::FourthLayerApp::new(cc)))))
}


/// Intermediate state for progressive pipeline compilation (one per frame).
/// Created in `new()`, consumed pipeline-by-pipeline in `update()`, then assembled into GpuCtx.
/// Fields are `pub(crate)` so the constructor (app_new.rs) and consumer (app_update.rs) reach them.
#[allow(dead_code)]
pub(crate) struct GpuInitState {
    pub(crate) render_state: eframe::egui_wgpu::RenderState,
    pub(crate) df_shader: wgpu::ShaderModule,
    pub(crate) pp_shader: wgpu::ShaderModule,
    pub(crate) pipeline_cache: Option<wgpu::PipelineCache>,
    pub(crate) bgl: wgpu::BindGroupLayout,
    pub(crate) deferred_bgl: wgpu::BindGroupLayout,
    pub(crate) ssr_bgl: wgpu::BindGroupLayout,
    pub(crate) fog_bgl: wgpu::BindGroupLayout,
    pub(crate) upsample_bgl: wgpu::BindGroupLayout,
    pub(crate) pt_bgl: wgpu::BindGroupLayout,
    pub(crate) tonemap_bgl: wgpu::BindGroupLayout,
    pub(crate) bloom_extract_bgl: wgpu::BindGroupLayout,
    pub(crate) bloom_blur_bgl: wgpu::BindGroupLayout,
    pub(crate) pl: wgpu::PipelineLayout,
    pub(crate) deferred_pl: wgpu::PipelineLayout,
    pub(crate) ssr_pl: wgpu::PipelineLayout,
    pub(crate) fog_pl: wgpu::PipelineLayout,
    pub(crate) upsample_pl: wgpu::PipelineLayout,
    pub(crate) pt_pl: wgpu::PipelineLayout,
    pub(crate) tonemap_pl: wgpu::PipelineLayout,
    pub(crate) bloom_ext_pl: wgpu::PipelineLayout,
    pub(crate) bloom_blur_pl: wgpu::PipelineLayout,
    pub(crate) ub: wgpu::Buffer, pub(crate) gv: SdfGpuVolume, pub(crate) pb: wgpu::Buffer, pub(crate) tb: wgpu::Buffer,
    pub(crate) eb: wgpu::Buffer, pub(crate) mcb: wgpu::Buffer, pub(crate) fog_prev_cam_buf: wgpu::Buffer,
    pub(crate) raster_depth_dummy: wgpu::Texture,
    pub(crate) bake_tex_dummy: wgpu::Texture,
    pub(crate) temporal_dummy_mask: wgpu::Texture,
    pub(crate) temporal_march_flags_buffer: wgpu::Buffer,
    pub(crate) sculpt_raymarch_bgl: wgpu::BindGroupLayout,
    pub(crate) bevel_preview_bgl: wgpu::BindGroupLayout,
    pub(crate) bevel_edge_buffer: wgpu::Buffer, pub(crate) bevel_params_buffer: wgpu::Buffer,
    pub(crate) bevel_grid_cells_buffer: wgpu::Buffer, pub(crate) bevel_grid_indices_buffer: wgpu::Buffer,
    pub(crate) compositor_buffer: wgpu::Buffer, pub(crate) light_link_buffer: wgpu::Buffer,
    pub(crate) light_cluster_buffer: wgpu::Buffer,
    pub(crate) tex_atlas_buffer: wgpu::Buffer, pub(crate) tex_meta_buffer: wgpu::Buffer, pub(crate) light_buffer: wgpu::Buffer,
    pub(crate) bvh_node_buffer: wgpu::Buffer, pub(crate) bvh_index_buffer: wgpu::Buffer,
    pub(crate) raster: render::RasterPipeline, pub(crate) shadows: render::ShadowMapSystem, pub(crate) picking: render::PickingPipeline,
    pub(crate) line_overlay: render::LineOverlayPipeline,
    pub(crate) face_overlay: render::FaceOverlayPipeline,
    pub(crate) pipelines: Vec<wgpu::ComputePipeline>,
    pub(crate) step: u32,
    pub(crate) start_time: std::time::Instant,
}
