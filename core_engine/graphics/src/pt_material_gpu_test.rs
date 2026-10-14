use glam::{Mat4, Vec2, Vec3};
use render::pt_accel::{trace_scene_cpu, PtAccelCache, PtInstanceSource, PtVertex};
use wgpu::util::DeviceExt;

#[test]
fn pt_vertex_uv_layout_matches_pt_and_rt_shadow_readers() {
    for source in [
        crate::build_pathtrace_tri_wgsl(),
        crate::build_rt_shadow_wgsl(),
    ] {
        let module = naga::front::wgsl::parse_str(&source).unwrap();
        let (_, vertex) = module
            .types
            .iter()
            .find(|(_, kind)| kind.name.as_deref() == Some("PtVertexData"))
            .unwrap();
        let naga::TypeInner::Struct { members, span } = &vertex.inner else {
            panic!("vertex must be a struct")
        };
        assert_eq!(*span as usize, std::mem::size_of::<PtVertex>());
        for (name, offset) in [
            ("pos", std::mem::offset_of!(PtVertex, pos)),
            ("normal", std::mem::offset_of!(PtVertex, normal)),
            ("uv", std::mem::offset_of!(PtVertex, uv)),
        ] {
            assert_eq!(
                members
                    .iter()
                    .find(|member| member.name.as_deref() == Some(name))
                    .unwrap()
                    .offset as usize,
                offset
            );
        }
        let (_, buffer) = module
            .global_variables
            .iter()
            .find(|(_, variable)| {
                variable
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.binding == 46)
            })
            .unwrap();
        let naga::TypeInner::Array { stride, .. } = module.types[buffer.ty].inner else {
            panic!("vertex binding must be an array")
        };
        assert_eq!(stride as usize, std::mem::size_of::<PtVertex>());
    }
    let precision = include_str!("material_precision.wgsl");
    let materials = [
        (crate::build_pathtrace_tri_wgsl(), "PtMaterial"),
        (crate::build_rt_shadow_wgsl(), "PtMaterial"),
        (
            format!(
                "{precision}\n{}",
                include_str!("../../render/src/gbuffer.wgsl")
            ),
            "MaterialData",
        ),
        (
            include_str!("../../render/src/shadow.wgsl").to_owned(),
            "MaterialData",
        ),
    ];
    for (source, name) in materials {
        let module = naga::front::wgsl::parse_str(&source).unwrap();
        let (_, material) = module
            .types
            .iter()
            .find(|(_, kind)| kind.name.as_deref() == Some(name))
            .unwrap();
        let naga::TypeInner::Struct { members, span } = &material.inner else {
            panic!("material must be a struct")
        };
        assert_eq!(*span, 320);
        assert_eq!(
            *span as usize,
            std::mem::size_of::<render::mesh_gpu::RasterMaterial>()
        );
        macro_rules! material_offset {
            ($field:ident) => {
                (
                    stringify!($field),
                    std::mem::offset_of!(render::mesh_gpu::RasterMaterial, $field),
                )
            };
        }
        for (field, offset) in [
            material_offset!(albedo),
            material_offset!(roughness_metallic),
            material_offset!(emission_color),
            material_offset!(noise_params),
            material_offset!(noise_color),
            material_offset!(edge_wear),
            material_offset!(edge_wear_color),
            material_offset!(cavity_dirt),
            material_offset!(cavity_dirt_color),
            material_offset!(albedo_map),
            material_offset!(normal_map),
            material_offset!(roughness_map),
            material_offset!(metallic_map),
            material_offset!(emission_map),
            material_offset!(bump_map),
            material_offset!(displacement_map),
            material_offset!(sss_color_map),
            material_offset!(adv_params),
            material_offset!(adv_sss),
            material_offset!(adv_glass),
        ] {
            assert_eq!(
                members
                    .iter()
                    .find(|member| member.name.as_deref() == Some(field))
                    .unwrap()
                    .offset as usize,
                offset,
                "{name}.{field}"
            );
        }
    }
}

#[test]
fn pt_real_hits_sample_mesh_atlas_uv_and_nonbinary_precision() {
    let Some((device, queue)) = super::rt_shadow_gpu_test::try_device() else {
        eprintln!("[PT-MATERIAL-GPU] no GPU adapter — skipping");
        return;
    };
    let vertices = vec![
        [-1.0, -1.0, 0.0, 0.0, 0.0, 1.0, 1.5 / 16.0, 1.5 / 16.0],
        [1.0, -1.0, 0.0, 0.0, 0.0, 1.0, 5.5 / 16.0, 1.5 / 16.0],
        [-1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.5 / 16.0, 5.5 / 16.0],
        [2.0, -1.0, 0.0, 0.0, 0.0, 1.0, 9.5 / 16.0, 1.5 / 16.0],
        [4.0, -1.0, 0.0, 0.0, 0.0, 1.0, 13.5 / 16.0, 1.5 / 16.0],
        [2.0, 1.0, 0.0, 0.0, 0.0, 1.0, 9.5 / 16.0, 5.5 / 16.0],
    ];
    let mut cache = PtAccelCache::new();
    let cache_id = core_math::scene_graph::MeshCacheId(71);
    cache.request(cache_id, 1, vertices, vec![0, 1, 2, 3, 4, 5]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !cache.poll() {
        assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
        std::thread::yield_now();
    }
    let model = Mat4::from_scale_rotation_translation(
        Vec3::new(1.3, 0.7, 1.1),
        glam::Quat::from_rotation_z(0.3),
        Vec3::new(4.0, -2.0, 0.5),
    );
    let scene = cache.assemble(&[PtInstanceSource {
        cache_id,
        model,
        material_base: 0,
        material_id_remap: None,
    }]);
    let probes: Vec<[f32; 4]> = [-0.5, 2.5]
        .into_iter()
        .map(|column| {
            model
                .transform_point3(Vec3::new(column, -0.5, 2.0))
                .extend(0.0)
                .to_array()
        })
        .collect();
    let expected_uvs: Vec<_> = probes
        .iter()
        .map(|probe| {
            let hit =
                trace_scene_cpu(&scene, Vec3::new(probe[0], probe[1], probe[2]), -Vec3::Z).unwrap();
            let triangle = scene.tris[hit.tri as usize];
            assert!((hit.t - 2.2).abs() < 1e-5);
            Vec2::from_array(scene.verts[triangle.v0 as usize].uv) * 0.5
                + Vec2::from_array(scene.verts[triangle.v1 as usize].uv) * 0.25
                + Vec2::from_array(scene.verts[triangle.v2 as usize].uv) * 0.25
        })
        .collect();
    assert!((expected_uvs[0].x - 2.5 / 16.0).abs() < 1e-5);
    assert!((expected_uvs[1].x - 10.5 / 16.0).abs() < 1e-5);
    let words = [[0x1f91u16, 0x817e, 0xc123], [0x916e, 0x3789, 0x6543]];
    let srgb_to_linear = |value: f32| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode_byte = |value: u8| {
        let value = f32::from(value) / 255.0;
        let encoded = if value <= 0.0031308 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0 + 0.5) as u8
    };
    let mut source = crate::build_pathtrace_tri_wgsl();
    let primary_albedo = crate::PATHTRACE_TRI_ENTRY_WGSL
        .split_once("            // Albedo map: absolute")
        .unwrap()
        .1
        .split_once("            // Roughness / metalness maps REPLACE")
        .unwrap()
        .0;
    let primary_albedo = primary_albedo
        .split_once("            let amap_slot")
        .unwrap()
        .1;
    let primary_albedo = format!("let amap_slot{primary_albedo}");
    source.push_str(r#"
@group(0) @binding(60) var<storage, read> material_probes: array<vec4<f32>>;
@group(0) @binding(61) var<storage, read_write> material_samples: array<vec4<f32>>;
@compute @workgroup_size(1)
fn probe_pt_material(@builtin(global_invocation_id) gid: vec3<u32>) {
    let hit = pt_trace_scene(material_probes[gid.x].xyz, vec3(0.0, 0.0, -1.0), 100.0);
    let uv = pt_hit_uv(hit);
    var albedo: vec3<f32>;
    var roughness: f32;
    var metallic: f32;
    var emission: vec3<f32>;
    pt_gi_hit_mat(hit, &albedo, &roughness, &metallic, &emission);
    let material = pt_hit_material(hit);
    let color = pt_sample_mat_rgb(bitcast<u32>(material.emission_map.x), vec3(0.0), vec3(0.0, 0.0, 1.0), uv, 1.0, bitcast<u32>(material.emission_map.w));
    material_samples[gid.x * 4u] = vec4(uv, hit.u, hit.v);
    material_samples[gid.x * 4u + 1u] = vec4(albedo, roughness);
    material_samples[gid.x * 4u + 2u] = vec4(color, metallic);
    let mat = material;
    let p_local = vec3(0.0);
    let n_local = vec3(0.0, 0.0, 1.0);
    let face_uv = uv;
    albedo = mat.albedo.rgb;
    PRIMARY_ALBEDO
    material_samples[gid.x * 4u + 3u] = vec4(albedo, 1.0);
}
"#);
    source = source.replace("PRIMARY_ALBEDO", &primary_albedo);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pt_material_probe"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pt_material_probe"),
        layout: None,
        module: &module,
        entry_point: Some("probe_pt_material"),
        compilation_options: Default::default(),
        cache: None,
    });
    let tlas = super::rt_shadow_gpu_test::upload(&device, &queue, &scene.tlas, "probe_tlas");
    let instances =
        super::rt_shadow_gpu_test::upload(&device, &queue, &scene.instances, "probe_instances");
    let blas = super::rt_shadow_gpu_test::upload(&device, &queue, &scene.blas_nodes, "probe_blas");
    let triangles =
        super::rt_shadow_gpu_test::upload(&device, &queue, &scene.tris, "probe_triangles");
    let vertices =
        super::rt_shadow_gpu_test::upload(&device, &queue, &scene.verts, "probe_vertices");
    let meta = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("probe_meta"),
        contents: bytemuck::cast_slice(&[
            scene.tlas.len() as u32,
            scene.instances.len() as u32,
            scene.blas_nodes.len() as u32,
            scene.tris.len() as u32,
            scene.verts.len() as u32,
            1,
            0,
            0,
        ]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let probes = super::rt_shadow_gpu_test::upload(&device, &queue, &probes, "material_probes");
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material_samples"),
        size: 128,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material_readback"),
        size: 128,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    for raw in [false, true] {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("material_atlas"),
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 3,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });
        for plane in 0..2 {
            let pixels: Vec<u8> = (0..256)
                .flat_map(|pixel| {
                    let mut color = [0u8; 4];
                    color[3] = 255;
                    for channel in 0..3 {
                        let word = words[usize::from(pixel % 16 >= 8)][channel];
                        let byte = if plane == 0 {
                            (word >> 8) as u8
                        } else {
                            word as u8
                        };
                        color[channel] = if raw { byte } else { encode_byte(byte) };
                    }
                    color
                })
                .collect();
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: plane + 1,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(64),
                    rows_per_image: Some(16),
                },
                wgpu::Extent3d {
                    width: 16,
                    height: 16,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let linear = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let mut pbr = core_math::pbr_material::PbrMaterial::default();
        for map in [
            &mut pbr.albedo_map,
            &mut pbr.roughness_map,
            &mut pbr.metallic_map,
            &mut pbr.emission_map,
        ] {
            map.tex_id = 1;
            map.projection = 1;
            map.absolute = true;
            map.high_precision = true;
            map.raw_precision = raw;
            map.green_dx = true;
            map.rot_x = 1;
            map.rot_y = 2;
            map.rot_z = 3;
            map.strength = 1.0;
        }
        let materials = super::rt_shadow_gpu_test::upload(
            &device,
            &queue,
            &[render::mesh_gpu::RasterMaterial::from_pbr(&pbr)],
            "probe_materials",
        );
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pt_material_probe"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 42,
                    resource: tlas.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 43,
                    resource: instances.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 44,
                    resource: blas.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 45,
                    resource: triangles.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 46,
                    resource: vertices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 47,
                    resource: meta.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 48,
                    resource: materials.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 49,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 50,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 59,
                    resource: wgpu::BindingResource::TextureView(&linear),
                },
                wgpu::BindGroupEntry {
                    binding: 60,
                    resource: probes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 61,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &binding, &[]);
            pass.dispatch_workgroups(2, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, 128);
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap();
            });
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        receiver.recv().unwrap().unwrap();
        let mapped = staging.slice(..).get_mapped_range();
        let samples: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
        for index in 0..2 {
            let hit = samples[index * 4];
            assert!((Vec2::new(hit[0], hit[1]) - expected_uvs[index]).length() < 1e-5);
            assert!((hit[2] - 0.25).abs() < 1e-5 && (hit[3] - 0.25).abs() < 1e-5);
            let decoded_words = words[index].map(|word| {
                if raw {
                    f32::from(word) / 65535.0
                } else {
                    let high = srgb_to_linear(f32::from(encode_byte((word >> 8) as u8)) / 255.0);
                    let low = srgb_to_linear(f32::from(encode_byte(word as u8)) / 255.0);
                    (high * 65280.0 + low * 255.0) / 65535.0
                }
            });
            for channel in 0..3 {
                let expected = srgb_to_linear(decoded_words[channel]);
                let tolerance = if raw { 2e-6 } else { 0.003 };
                assert!(
                    (samples[index * 4 + 1][channel] - expected).abs() < tolerance,
                    "GI albedo tile={index} channel={channel} raw={raw}: {:?}, expected {expected}",
                    samples[index * 4 + 1]
                );
                assert!(
                    (samples[index * 4 + 2][channel] - expected).abs() < tolerance,
                    "emission tile={index} channel={channel} raw={raw}"
                );
                assert!(
                    (samples[index * 4 + 3][channel] - expected).abs() < tolerance,
                    "primary albedo tile={index} channel={channel} raw={raw}"
                );
            }
            let scalar = decoded_words[0];
            assert!((samples[index * 4 + 1][3] - scalar).abs() < if raw { 2e-6 } else { 0.003 });
            assert!((samples[index * 4 + 2][3] - scalar).abs() < if raw { 2e-6 } else { 0.003 });
        }
        drop(mapped);
        staging.unmap();
    }
}
