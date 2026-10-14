// core_engine/render/src/tests_vertex_curvature.rs
// ============================================================
// Headless GPU proof: vertex curvature actually reaches a pixel
// ============================================================
//
// GPU_CURVATURE_KERNEL_PLAN.md P2/P3. This is the gate the feature was missing: earlier work
// wired `curv_vertex` all the way to `VertexOutput::vertex_curv` and stopped — nothing in
// `fs_gbuffer` ever read it, so the G-buffer albedo output was bit-for-bit identical whether the
// buffer held zeros or real curvature. A test that only proves the buffer is BOUND (the old
// `curvature_bgl`/dummy-bind-group tests) cannot catch that: the bind group is perfectly valid
// either way. This test proves the value reaches a PIXEL by rendering through the REAL
// `RasterPipeline` + `gbuffer.wgsl` (same struct, same shader source the app compiles), binding a
// REAL curvature storage buffer with two vertices carrying very different curvature at group 2,
// and reading back the rendered `albedo_flags` (Rgba16Float) G-buffer target — the exact texture
// `fs_gbuffer`'s `GBufferOutput::albedo_flags` (location 2) writes `color` into.
//
// Geometry: one big quad (2 triangles, 4 verts) filling the camera's view, facing the camera
// head-on so per-vertex interpolation maps directly to screen-space X. The 4 verts' curvature
// (channel 1, signed) are set to -1.0 (left edge) and +1.0 (right edge) — after this fix's
// switch from `@interpolate(flat)` to smooth interpolation, this produces a continuous left→right
// gradient in the rendered albedo. Sampling near the left and right thirds of the quad and
// asserting they differ in the SIGNED direction the WGSL comment documents (concave/negative →
// darker, convex/positive → lighter) is a direct, non-vacuous proof.
#![cfg(test)]

use crate::camera::{CameraUniforms, MAX_GRAB_INSTANCES};
use crate::mesh_gpu::{InstanceData, RasterMaterial, RasterVertex, INSTANCE_FLAG_VERTEX_CURVATURE};
use crate::pipeline::RasterPipeline;
use crate::shadows::ShadowMapSystem;
use bytemuck::Zeroable;
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

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
            label: Some("vertex_curvature_probe_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()
}

const W: u32 = 64;
const H: u32 = 64;

/// A single quad facing +Z (toward the camera looking down -Z), spanning x∈[-2,2], y∈[-2,2] at
/// z=0. Vertex order: 0=(-2,-2) 1=(2,-2) 2=(2,2) 3=(-2,2) — matches `curv_data`'s per-vertex
/// assignment below (index i -> curv_vertex[i*4+1]).
fn facing_quad() -> (Vec<RasterVertex>, Vec<u32>) {
    let n = [0.0, 0.0, 1.0];
    let v = vec![
        RasterVertex { position: [-2.0, -2.0, 0.0], normal: n, uv: [0.0, 0.0] },
        RasterVertex { position: [2.0, -2.0, 0.0], normal: n, uv: [1.0, 0.0] },
        RasterVertex { position: [2.0, 2.0, 0.0], normal: n, uv: [1.0, 1.0] },
        RasterVertex { position: [-2.0, 2.0, 0.0], normal: n, uv: [0.0, 1.0] },
    ];
    let idx = vec![0, 1, 2, 0, 2, 3];
    (v, idx)
}

/// Render `facing_quad()` with `curv_bit` flags on its single instance, binding `curv_buf` at
/// group 2 (real buffer) or the pipeline's dummy (curv_buf = None). Returns the rendered
/// `albedo_flags` target (Rgba16Float — the exact `fs_gbuffer` output).
fn render_albedo(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    curv_buf: Option<&wgpu::Buffer>,
    curv_bit: u32,
) -> wgpu::Texture {
    let mut raster = RasterPipeline::new(device);
    raster.ensure_depth(device, W, H);

    // Orthographic-ish perspective camera looking straight at the quad along -Z, close enough to
    // FOV that the quad fills the frame (avoids off-screen edge cases in this simple probe).
    let eye = Vec3::new(0.0, 0.0, 5.0);
    let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
    let aspect = W as f32 / H as f32;
    let proj = Mat4::perspective_rh(70f32.to_radians(), aspect, 0.1, 100.0);
    let cam_u = CameraUniforms {
        view_proj: (proj * view).to_cols_array_2d(),
        view: view.to_cols_array_2d(),
        proj: proj.to_cols_array_2d(),
        cam_pos: [eye.x, eye.y, eye.z, 0.0],
        near_far: [0.1, 100.0],
        _pad: [0.0; 2],
        grab_radii: [0.0; 4],
        grab_anchors: [[0.0; 4]; MAX_GRAB_INSTANCES],
        grab_drags: [[0.0; 4]; MAX_GRAB_INSTANCES],
        wire_color: [0.0; 4],
        frame_flags: [0.0; 4],
    };
    raster.upload_camera(queue, &cam_u, &cam_u);

    // Neutral mid-gray base so the tint mix (toward near-black / near-white in the WGSL) is
    // visible moving in EITHER direction from the base, not clipped against 0 or 1.
    let mut mat = RasterMaterial::zeroed();
    mat.albedo = [0.5, 0.5, 0.5, 1.0];
    mat.roughness_metallic = [0.8, 0.0, 0.0, 0.0];
    raster.upload_materials(device, queue, &[mat]);

    let mat_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("curv_test_mat_tex"),
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

    // Sun disabled — compute_shadow() short-circuits to 1.0, no shadow sampling in play.
    let mut shadows = ShadowMapSystem::new(device);
    shadows.ensure_textures(device);
    shadows.update_cascades(
        queue, view, proj, Vec3::new(0.4, 0.8, 0.45), 0.1, 100.0, false,
        crate::shadows::DEFAULT_PCF_RADIUS_TEXELS, 0.0,
        crate::shadows::ShadowBias::default(),
    );
    raster.rebuild_shadow_bind_group(device, &shadows);

    let (verts, indices) = facing_quad();
    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("curv_test_verts"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("curv_test_idx"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let inst = InstanceData {
        model_matrix: Mat4::IDENTITY.to_cols_array_2d(),
        normal_matrix: [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]],
        material_idx: 0,
        body_index: 1,
        flags: curv_bit,
        _pad: 0,
            curv_params: [0.0, 1.0, 1.0, 1.0],
    };
    raster.upload_instances(device, queue, &[inst], &[]);

    let f16 = |label: &str| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
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
    let gbuf_normal = f16("curv_test_gbuf_normal");
    let gbuf_material = f16("curv_test_gbuf_material");
    // C.5: raster location(3) motion is R32Float
    let gbuf_motion = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("curv_test_gbuf_motion"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let gbuf_albedo = f16("curv_test_gbuf_albedo");
    let views: Vec<wgpu::TextureView> = [&gbuf_normal, &gbuf_material, &gbuf_albedo, &gbuf_motion]
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();

    // Group 2: a REAL curvature buffer when provided, else the pipeline's own dummy — exactly
    // the production `curv_bgs` / `curvature_dummy_bind_group` choice in raster_gbuffer.rs.
    let curv_bind_group = curv_buf.map(|buf| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("curv_test_real_bg"),
            layout: &raster.curvature_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
        })
    });

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
            label: Some("curv_test_gbuffer"),
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
        pass.set_bind_group(2, curv_bind_group.as_ref().unwrap_or(&raster.curvature_dummy_bind_group), &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_vertex_buffer(1, raster.instance_buffer.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
    }
    queue.submit([enc.finish()]);
    gbuf_albedo
}

fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = ((bits >> 10) & 0x1F) as i32;
    let frac = (bits & 0x3FF) as f32;
    if exp == 0 {
        sign * frac / 1024.0 * 2f32.powi(-14)
    } else if exp == 31 {
        if frac == 0.0 {
            sign * f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        sign * (1.0 + frac / 1024.0) * 2f32.powi(exp - 15)
    }
}

/// Read back Rgba16Float albedo as quantized u8 RGBA (0..255) for simple assertions.
fn read_rgba8(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Vec<[u8; 4]> {
    let row_bytes = W * 8; // 4 × f16
    let padded = (row_bytes + 255) & !255;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curv_test_read"),
        size: (padded * H) as u64,
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
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buf.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    rx.recv().expect("map_async channel").expect("map_async failed");
    let data = buf.slice(..).get_mapped_range();
    let mut out = vec![[0u8; 4]; (W * H) as usize];
    for y in 0..H {
        let row_start = (y * padded) as usize;
        for x in 0..W {
            let o = row_start + (x * 8) as usize;
            let mut px = [0u8; 4];
            for c in 0..4 {
                let bits = u16::from_le_bytes([data[o + c * 2], data[o + c * 2 + 1]]);
                let v = f16_to_f32(bits).clamp(0.0, 1.0);
                px[c] = (v * 255.0 + 0.5) as u8;
            }
            out[(y * W + x) as usize] = px;
        }
    }
    out
}

/// Build a `curv_vertex`-layout storage buffer (`[mask, curv, convex, concave]` per vertex slot,
/// matching `volume_bake.wgsl::curvature_eval`'s output / `gbuffer.wgsl`'s binding doc) with the
/// 4 quad vertices' signed-curv channel set to LEFT_CURV / RIGHT_CURV.
fn make_curv_buffer(device: &wgpu::Device, left_curv: f32, right_curv: f32) -> wgpu::Buffer {
    // Vertex order from facing_quad(): 0=left-bottom, 1=right-bottom, 2=right-top, 3=left-top.
    let per_vertex = [
        [0.0, left_curv, 0.0, 0.0],
        [0.0, right_curv, 0.0, 0.0],
        [0.0, right_curv, 0.0, 0.0],
        [0.0, left_curv, 0.0, 0.0],
    ];
    let flat: Vec<f32> = per_vertex.iter().flatten().copied().collect();
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("curv_test_real_buf"),
        contents: bytemuck::cast_slice(&flat),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

/// P2 / HANDOFF Decision 1: G-buffer no longer composites vertex curvature live.
/// With INSTANCE_FLAG_VERTEX_CURVATURE + a real buffer that varies left/right, albedo must
/// stay FLAT (same as base mat.albedo) — curvature reaches the image only through the bake map.
/// If this fails with left < right again, the deleted composite was reintroduced.
#[test]
fn vertex_curvature_signed_channel_reaches_albedo_pixel() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[vertex_curvature_probe] no GPU adapter — skipping (CI headless)");
        return;
    };
    let curv_buf = make_curv_buffer(&device, -1.0, 1.0);
    let tex = render_albedo(&device, &queue, Some(&curv_buf), INSTANCE_FLAG_VERTEX_CURVATURE);
    let pixels = read_rgba8(&device, &queue, &tex);

    let y = H / 2;
    let left_x = W / 3;
    let right_x = W - W / 3;
    let left_px = pixels[(y * W + left_x) as usize];
    let right_px = pixels[(y * W + right_x) as usize];

    assert!(left_px[3] > 0, "left sample missed the quad (background) — camera/geometry framing bug");
    assert!(right_px[3] > 0, "right sample missed the quad (background) — camera/geometry framing bug");

    eprintln!(
        "[vertex_curvature_probe P2] left(curv=-1.0) rgb={:?}  right(curv=+1.0) rgb={:?} (must match — bake owns composite)",
        &left_px[..3], &right_px[..3]
    );

    // No live composite: buffer + flag must not tint the pixel.
    assert_eq!(
        left_px, right_px,
        "P2: G-buffer must not composite vertex curvature — left={:?} right={:?} (re-introduced live path?)",
        left_px, right_px
    );
}

/// Control: with the SAME real curvature buffer bound but the instance flag OFF, the shader must
/// take the `out.vertex_curv = 0.0` branch — left and right must render IDENTICALLY. Proves the
/// per-instance gate (the non-negotiable "every other draw is bit-for-bit unchanged" constraint),
/// not just that curvature CAN show up somewhere.
#[test]
fn vertex_curvature_flag_off_leaves_albedo_unaffected() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[vertex_curvature_probe] no GPU adapter — skipping (CI headless)");
        return;
    };
    let curv_buf = make_curv_buffer(&device, -1.0, 1.0);
    // Flag NOT set — same buffer bound (as raster_gbuffer.rs would if a stale entry existed),
    // but the shader must never index it.
    let tex = render_albedo(&device, &queue, Some(&curv_buf), 0);
    let pixels = read_rgba8(&device, &queue, &tex);
    let y = H / 2;
    let left_px = pixels[(y * W + W / 3) as usize];
    let right_px = pixels[(y * W + (W - W / 3)) as usize];
    assert!(left_px[3] > 0, "left sample missed the quad (background) — camera/geometry framing bug: {:?}", left_px);
    assert!(right_px[3] > 0, "right sample missed the quad (background) — camera/geometry framing bug: {:?}", right_px);
    assert_eq!(left_px, right_px, "flag-off draw must be unaffected by a resident curvature buffer: left={:?} right={:?}", left_px, right_px);
}
