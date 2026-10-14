use super::{atlas, compile_material_pipeline, dispatch_surface, Atlas};
use super::super::preview::{submit_surface_preview, ResidentMaterialCache, ResidentPreviewStatus};
use crate::material_textures::MaterialTextureArray;
use crate::shader_nodes::{NodeGraph, NodeType};
use core_math::half_edge::HalfEdgeMesh;
use core_math::pbr_material::PbrMaterial;
use eframe::wgpu;
use glam::{Mat4, Vec3};
use render::camera::{CameraUniforms, MAX_GRAB_INSTANCES};
use render::mesh_gpu::{InstanceData, RasterMaterial, RasterVertex};
use render::pipeline::RasterPipeline;
use render::shadows::{ShadowMapSystem, DEFAULT_PCF_RADIUS_TEXELS};
use wgpu::util::DeviceExt;

const VIEW_SIZE: u32 = 128;
const VIEW_HALF_EXTENT: f32 = 1.2;

fn render_curvature_viewport(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    surface_atlas: &Atlas,
    textures: &MaterialTextureArray,
    material: &PbrMaterial,
    view_center: Vec3,
    view_half_extent: f32,
) -> Vec<[f32; 4]> {
    let mut raster = RasterPipeline::new(device);
    raster.ensure_depth(device, VIEW_SIZE, VIEW_SIZE);
    let eye = view_center + Vec3::new(0.0, 0.0, 5.0);
    let view = Mat4::look_at_rh(eye, view_center, Vec3::Y);
    let proj = Mat4::orthographic_rh(
        -view_half_extent, view_half_extent,
        -view_half_extent, view_half_extent, 0.1, 100.0,
    );
    let camera = CameraUniforms {
        view_proj: (proj * view).to_cols_array_2d(),
        view: view.to_cols_array_2d(),
        proj: proj.to_cols_array_2d(),
        cam_pos: eye.extend(0.0).to_array(),
        near_far: [0.1, 100.0],
        _pad: [0.0; 2],
        grab_radii: [0.0; 4],
        grab_anchors: [[0.0; 4]; MAX_GRAB_INSTANCES],
        grab_drags: [[0.0; 4]; MAX_GRAB_INSTANCES],
        wire_color: [0.0; 4],
        frame_flags: [0.0; 4],
    };
    raster.upload_camera(queue, &camera, &camera);
    raster.upload_materials(device, queue, &[RasterMaterial::from_pbr(material)]);
    raster.rebuild_bind_group(device, textures.view(), textures.linear_view(), textures.sampler());
    let mut shadows = ShadowMapSystem::new(device);
    shadows.ensure_textures(device);
    shadows.update_cascades(
        queue, view, proj, Vec3::new(0.4, 0.8, 0.45), 0.1, 100.0, false,
        DEFAULT_PCF_RADIUS_TEXELS, 0.0, render::shadows::ShadowBias::default(),
    );
    raster.rebuild_shadow_bind_group(device, &shadows);
    let vertices: Vec<_> = surface_atlas.vertices.iter().map(|vertex| RasterVertex {
        position: [vertex[0], vertex[1], vertex[2]],
        normal: [vertex[3], vertex[4], vertex[5]],
        uv: [vertex[6], vertex[7]],
    }).collect();
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("curvature_viewport_vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("curvature_viewport_indices"),
        contents: bytemuck::cast_slice(&surface_atlas.indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    raster.upload_instances(device, queue, &[InstanceData {
        model_matrix: Mat4::IDENTITY.to_cols_array_2d(),
        normal_matrix: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
        material_idx: 0,
        body_index: 1,
        flags: 0,
        _pad: 0,
        curv_params: [0.0, 1.0, 1.0, 1.0],
    }], &[]);
    let targets: Vec<_> = [
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::R32Float,
    ].into_iter().map(|format| device.create_texture(&wgpu::TextureDescriptor {
        label: Some("curvature_viewport_gbuffer"),
        size: wgpu::Extent3d { width: VIEW_SIZE, height: VIEW_SIZE, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })).collect();
    let views: Vec<_> = targets.iter().map(|texture| texture.create_view(&Default::default())).collect();
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let attachments: Vec<_> = views.iter().map(|view| Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })).collect();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("curvature_real_raster_viewport"),
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
        pass.set_bind_group(2, &raster.curvature_dummy_bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, raster.instance_buffer.slice(..));
        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..surface_atlas.indices.len() as u32, 0, 0..1);
    }
    let bytes_per_row = (VIEW_SIZE * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curvature_viewport_albedo_readback"),
        size: u64::from(bytes_per_row) * u64::from(VIEW_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        targets[2].as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(VIEW_SIZE),
            },
        },
        wgpu::Extent3d { width: VIEW_SIZE, height: VIEW_SIZE, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |result| sender.send(result).unwrap());
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    receiver.recv().unwrap().unwrap();
    let mapped = staging.slice(..).get_mapped_range();
    let pixels = (0..VIEW_SIZE * VIEW_SIZE).map(|pixel| {
        let offset = ((pixel / VIEW_SIZE) * bytes_per_row + (pixel % VIEW_SIZE) * 8) as usize;
        std::array::from_fn(|channel| {
            let offset = offset + channel * 2;
            crate::ai_beauty::f16_bits_to_f32(u16::from_le_bytes([mapped[offset], mapped[offset + 1]]))
        })
    }).collect();
    drop(mapped);
    staging.unmap();
    pixels
}

#[test]
fn real_mesh_curvature_reaches_viewport() {
    let (_gpu_lock, device, queue) = crate::gpu_test_harness::require_adapter();
    let device = std::sync::Arc::new(device);
    let cube = HalfEdgeMesh::new_box(Vec3::ONE);
    let (vertices, indices) = core_math::poly_mesh::raster_data_ngon(&cube);
    let surface_atlas = atlas(&cube, &vertices, &indices, 512).unwrap();
    let pipeline = compile_material_pipeline(
        &device,
        include_str!("../../graphics/src/poly_curvature_cache.wgsl"),
        &graphics::bgl::poly_curvature_cache_bgl_entries(),
        "poly_curvature_cache",
    ).unwrap();
    let surface = dispatch_surface(&device, &queue, &pipeline, &surface_atlas, 512);
    let mut graph = NodeGraph::default();
    let output = graph.output_node().unwrap().id;
    let curvature = graph.add_node(NodeType::Curvature, [0.0; 2]);
    graph.find_node_mut(curvature).unwrap().input_defaults[1].value_f32 = 0.2;
    assert!(graph.connect(curvature, 0, output, NodeGraph::MO_ALBEDO));
    let mut textures = MaterialTextureArray::new(&device, 512);
    let mut cache = ResidentMaterialCache::default();
    let code = graph.compile_wgsl();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let slots = loop {
        match submit_surface_preview(
            &device, &queue, &mut textures, &mut cache, &code,
            "curvature_viewport_regression", 1, 1, Some((&surface, 1)),
        ) {
            ResidentPreviewStatus::Submitted(slots) => break slots,
            ResidentPreviewStatus::Failed(error) => panic!("Curvature material failed: {error}"),
            ResidentPreviewStatus::Warming => {
                assert!(std::time::Instant::now() < deadline, "Curvature material compilation timed out");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    };
    assert_ne!(slots[0], 0);
    let mut material = PbrMaterial::default();
    material.albedo_map.tex_id = slots[0];
    material.albedo_map.strength = 1.0;
    material.albedo_map.projection = 1;
    material.albedo_map.uv_scale = 1.0;
    material.albedo_map.high_precision = true;
    material.albedo_map.raw_precision = false;
    material.albedo_map.absolute = true;
    let pixels = render_curvature_viewport(
        &device, &queue, &surface_atlas, &textures, &material, Vec3::ZERO, VIEW_HALF_EXTENT,
    );
    let sample = |horizontal: f32, vertical: f32| {
        let column = ((horizontal / VIEW_HALF_EXTENT + 1.0) * 0.5 * VIEW_SIZE as f32) as u32;
        let row = ((1.0 - vertical / VIEW_HALF_EXTENT) * 0.5 * VIEW_SIZE as f32) as u32;
        pixels[(row * VIEW_SIZE + column) as usize]
    };
    let center = sample(0.0, 0.0);
    assert!(center[3] > 0.0, "Cube face center missed the real raster draw: {center:?}");
    assert!(center[..3].iter().all(|value| *value < 0.01), "Flat face center is not dark: {center:?}");
    let mut boundaries = Vec::new();
    for along in [-0.5, 0.0, 0.5] {
        for (horizontal, vertical) in [(0.95, along), (-0.95, along), (along, 0.95), (along, -0.95)] {
            let pixel = sample(horizontal, vertical);
            assert!(pixel[3] > 0.0, "Cube boundary missed the real raster draw");
            assert!(pixel[..3].iter().all(|value| *value > 0.65), "Real cube edge is not light at ({horizontal}, {vertical}): {pixel:?}");
            boundaries.push(pixel[0]);
        }
    }
    for diagonal in [-0.65, -0.35, 0.0, 0.35, 0.65] {
        for vertical in [diagonal, -diagonal] {
            let pixel = sample(diagonal, vertical);
            assert!(pixel[3] > 0.0);
            assert!(pixel[..3].iter().all(|value| *value < 0.01), "Triangulation introduced a fake diagonal edge: {pixel:?}");
        }
    }
    for horizontal in [0.86, 0.88, 0.9, 0.92] {
        let column = ((horizontal / VIEW_HALF_EXTENT + 1.0) * 0.5 * VIEW_SIZE as f32) as u32;
        let world_x = ((column as f32 + 0.5) / VIEW_SIZE as f32 * 2.0 - 1.0) * VIEW_HALF_EXTENT;
        let distance_fraction = (1.0 - world_x) / 0.2;
        let expected = 1.0 - distance_fraction * distance_fraction * (3.0 - 2.0 * distance_fraction);
        let pixel = sample(horizontal, 0.2);
        assert!(expected > 0.1 && expected < 0.9);
        assert!(pixel[3] > 0.0);
        assert!(pixel[..3].iter().all(|value| (*value - expected).abs() < 0.04),
            "Resident dual-plane albedo decoding changed the intermediate falloff at x={world_x}: expected={expected}, rendered={pixel:?}");
    }
    assert!(sample(1.1, 0.0).iter().all(|value| *value == 0.0), "Raster framing control should remain background");
    eprintln!("Real raster Curvature->Albedo: center={center:?}, real boundary samples={boundaries:?}, both diagonals dark");
    let mut defaults = NodeGraph::default();
    let default_curvature = defaults.add_node(NodeType::Curvature, [0.0; 2]);
    let radius = defaults.find_node(default_curvature).unwrap().input_defaults[1].value_f32;
    assert_eq!(radius, 0.02);
    graph.find_node_mut(curvature).unwrap().input_defaults[1].value_f32 = radius;
    let default_atlas = atlas(&cube, &vertices, &indices, 256).unwrap();
    let default_surface = dispatch_surface(&device, &queue, &pipeline, &default_atlas, 256);
    let mut default_textures = MaterialTextureArray::new(&device, 256);
    while !default_textures.prepare_gpu_filtering().expect("Default texture filter compilation failed") {
        assert!(std::time::Instant::now() < deadline, "Default texture filter compilation timed out");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let default_slots = match submit_surface_preview(
        &device, &queue, &mut default_textures, &mut cache, &graph.compile_wgsl(),
        "curvature_default_viewport_regression", 1, 1, Some((&default_surface, 2)),
    ) {
        ResidentPreviewStatus::Submitted(slots) => slots,
        ResidentPreviewStatus::Failed(error) => panic!("Default Curvature material failed: {error}"),
        ResidentPreviewStatus::Warming => panic!("Radius-only edits must reuse the resident GPU pipeline"),
    };
    material.albedo_map.tex_id = default_slots[0];
    let zoom_center = Vec3::new(0.97, 0.0, 0.0);
    let zoom_half_extent = 0.08;
    let default_pixels = render_curvature_viewport(
        &device, &queue, &default_atlas, &default_textures, &material,
        zoom_center, zoom_half_extent,
    );
    let mut default_edge_peak = 0.0f32;
    let mut edge_samples = 0;
    let mut flat_samples = 0;
    for column in 0..VIEW_SIZE {
        let world_x = zoom_center.x + ((column as f32 + 0.5) / VIEW_SIZE as f32 * 2.0 - 1.0) * zoom_half_extent;
        let pixel = default_pixels[(VIEW_SIZE / 2 * VIEW_SIZE + column) as usize];
        if world_x > 0.98 && world_x < 1.0 {
            assert!(pixel[3] > 0.0, "Default-radius edge sample missed the cube");
            assert!(pixel[..3].iter().all(|value| value.is_finite()));
            default_edge_peak = default_edge_peak.max(pixel[0]);
            edge_samples += 1;
        } else if world_x < 0.95 {
            assert!(pixel[3] > 0.0);
            assert!(pixel[..3].iter().all(|value| *value < 0.01), "Default curvature spread into the flat face: {pixel:?}");
            flat_samples += 1;
        } else if world_x > 1.01 {
            assert!(pixel.iter().all(|value| *value == 0.0), "Zoomed raster background control failed");
        }
    }
    assert!(edge_samples > 0 && flat_samples > 0);
    assert!(default_edge_peak > 0.2, "Default radius 0.02 disappeared in the production 256 atlas: peak={default_edge_peak}");
    eprintln!("Default 0.02 radius, 256 atlas, zoomed real raster edge: peak={default_edge_peak}, samples={edge_samples}");
    let generation = default_textures.generation;
    let mut submission_times = Vec::new();
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let batch_started = std::time::Instant::now();
    for step in 0..32 {
        graph.find_node_mut(curvature).unwrap().input_defaults[3].value_f32 = step as f32 / 31.0;
        let code = graph.compile_wgsl();
        let started = std::time::Instant::now();
        match submit_surface_preview(
            &device, &queue, &mut default_textures, &mut cache, &code,
            "curvature_default_viewport_regression", 1, 1, Some((&default_surface, 2)),
        ) {
            ResidentPreviewStatus::Submitted(slots) => assert_eq!(slots, default_slots),
            _ => panic!("Warm curvature sliders must reuse their pipeline and output slots"),
        }
        submission_times.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(default_textures.generation, generation);
    }
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let average_completed_ms = batch_started.elapsed().as_secs_f64() * 1000.0 / 32.0;
    submission_times.sort_by(f64::total_cmp);
    eprintln!("Warm 256 curvature sliders: median CPU submission={:.3}ms, p95={:.3}ms, completed batch average={average_completed_ms:.3}ms/update; no recompilation or geometry rebuild",
        submission_times[16], submission_times[30]);
}
