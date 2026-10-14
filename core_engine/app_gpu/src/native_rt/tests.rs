use super::*;
use glam::{Mat4, Quat, Vec3};

fn instance(tri_off: u32, vert_off: u32, model: Mat4) -> NativeInstance {
    NativeInstance { tri_off, vert_off, model: model.to_cols_array_2d() }
}

#[test]
fn shared_geometry_ranges_preserve_global_triangle_offsets() {
    let layout = scene::scene_layout(30, 12, &[
        instance(7, 20, Mat4::IDENTITY),
        instance(0, 0, Mat4::IDENTITY),
        instance(7, 20, Mat4::from_translation(Vec3::X)),
        instance(3, 10, Mat4::IDENTITY),
    ]).unwrap();
    assert_eq!(layout.ranges.iter().map(|range| (range.first_triangle, range.triangle_count)).collect::<Vec<_>>(), vec![(0, 3), (3, 4), (7, 5)]);
    assert_eq!(layout.placements.iter().map(|(_, geometry)| *geometry).collect::<Vec<_>>(), vec![2, 0, 2, 1]);
}

#[test]
fn native_transform_preserves_affine_math_and_mirroring() {
    let model = Mat4::from_scale_rotation_translation(
        Vec3::new(-2.0, 3.0, 0.5), Quat::from_rotation_y(0.7), Vec3::new(4.0, -2.0, 8.0),
    );
    let rows = scene::instance_transform(model.to_cols_array_2d()).unwrap();
    let point = Vec3::new(0.4, 0.2, -0.8).extend(1.0);
    let actual = Vec3::new(
        glam::Vec4::from_slice(&rows[0..4]).dot(point),
        glam::Vec4::from_slice(&rows[4..8]).dot(point),
        glam::Vec4::from_slice(&rows[8..12]).dot(point),
    );
    assert!(actual.abs_diff_eq(model.transform_point3(point.truncate()), 1e-6));
}

#[test]
fn invalid_geometry_and_transforms_fail_closed() {
    assert!(scene::scene_layout(3, 1, &[instance(1, 0, Mat4::IDENTITY)]).is_err());
    assert!(scene::scene_layout(3, 1, &[instance(0, 3, Mat4::IDENTITY)]).is_err());
    assert!(scene::scene_layout(6, 2, &[instance(0, 0, Mat4::IDENTITY), instance(0, 3, Mat4::IDENTITY)]).is_err());
    assert!(scene::instance_transform(Mat4::ZERO.to_cols_array_2d()).is_err());
    assert!(scene::instance_transform(Mat4::perspective_rh(1.0, 1.0, 0.1, 10.0).to_cols_array_2d()).is_err());
    let mut invalid = Mat4::IDENTITY.to_cols_array_2d();
    invalid[3][0] = f32::NAN;
    assert!(scene::instance_transform(invalid).is_err());
    assert!(scene::scene_layout(0, 0, &[]).unwrap().ranges.is_empty());
}

#[test]
fn material_edits_do_not_change_native_instance_inputs() {
    let mut source: render::pt_accel::PtInstanceGpu = bytemuck::Zeroable::zeroed();
    source.model = Mat4::IDENTITY.to_cols_array_2d();
    let before = NativeInstance::from(&source);
    source.material_base = 173;
    assert_eq!(before, NativeInstance::from(&source));
    assert_eq!(std::mem::size_of::<PtVertex>(), 48);
    assert_eq!(std::mem::size_of::<PtTri>(), 16);
}

#[test]
fn index_pack_wgsl_validates() {
    let module = naga::front::wgsl::parse_str(include_str!("pack_indices.wgsl")).unwrap();
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module).unwrap();
}

#[test]
fn native_query_probe_wgsl_validates() {
    let module = naga::front::wgsl::parse_str(include_str!("test_query.wgsl")).unwrap();
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module).unwrap();
}

#[test]
fn specialized_native_material_variants_validate() {
    for interactive in [false, true] {
        let canonical = graphics::build_pathtrace_tri_wgsl_with_mode(Some(interactive));
        let native = build_native_rt_wgsl(&canonical).unwrap();
        assert!(native.contains(&format!("const PT_INTERACTIVE: bool = {interactive};")));
        assert!(!native.contains("override PT_INTERACTIVE"));
        let module = naga::front::wgsl::parse_str(&native).unwrap();
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).unwrap();
    }
}

#[test]
#[ignore = "requires an idle Vulkan RT adapter; coordinate with the parent GPU test owner"]
fn gpu_native_global_indices_shared_instances_and_submission_lifecycle() {
    let gpu_instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN, ..Default::default()
    });
    let adapter = pollster::block_on(gpu_instance.request_adapter(&wgpu::RequestAdapterOptions::default())).unwrap();
    assert!(adapter.features().contains(REQUIRED_FEATURES), "GPU test requires native ray query features");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Native RT validation device"), required_features: REQUIRED_FEATURES,
        required_limits: wgpu::Limits::default(), memory_hints: Default::default(), trace: wgpu::Trace::Off,
    })).unwrap();
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Native RT probe output"), size: 64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None, entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None,
        }],
    });
    let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: output.as_entire_binding() }],
    });
    let pipelines = NativeRtPipelines::compile(&device, &layout, include_str!("test_query.wgsl"), include_str!("test_query.wgsl"), None).unwrap();
    let mut vertices = vec![bytemuck::Zeroable::zeroed(); 9];
    for base in [3, 6] {
        for (offset, pos) in [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]].into_iter().enumerate() {
            vertices[base + offset] = PtVertex { pos, ..bytemuck::Zeroable::zeroed() };
        }
    }
    let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None, contents: bytemuck::cast_slice(&vertices), usage: wgpu::BufferUsages::BLAS_INPUT,
    });
    let triangles = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None, contents: bytemuck::cast_slice(&[
            PtTri { v0: 3, v1: 4, v2: 5, material_idx: 234 },
            PtTri { v0: 6, v1: 7, v2: 8, material_idx: 567 },
        ]), usage: wgpu::BufferUsages::STORAGE,
    });
    let mut renderer = NativeRtRenderer::default();
    let instances = [
        instance(0, 3, Mat4::IDENTITY),
        instance(0, 3, Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0))),
        instance(1, 6, Mat4::from_translation(Vec3::Z)),
    ];
    renderer.set_scene(&vertices, &triangles, 9, 2, &instances, NativeRtEpochs { geometry: 1, transforms: 1 }).unwrap();
    let encoder = || device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let budget = NativeRtBuildBudget { max_blas: 1, ..Default::default() };
    let abandoned = renderer.prepare(&device, encoder(), &pipelines, budget).unwrap();
    assert!(matches!(abandoned.status(), NativeRtStatus::Building { remaining_blas: 1, .. }));
    assert!(abandoned.dispatch_lease().is_none());
    drop(abandoned);
    assert!(renderer.meshes.is_empty());
    let first = renderer.prepare(&device, encoder(), &pipelines, budget).unwrap();
    assert!(renderer.commit(first.submit(&queue)));
    assert_eq!(renderer.meshes.len(), 1);
    let mut frame = renderer.prepare(&device, encoder(), &pipelines, budget).unwrap();
    assert_eq!(frame.status(), NativeRtStatus::Ready);
    let dispatch = frame.dispatch_lease().expect("ready scene provides a dispatch lease");
    dispatch.encode(frame.encoder_mut(), &pipelines, &binding, [4, 1], false).unwrap();
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None, size: 64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
    });
    frame.encoder_mut().copy_buffer_to_buffer(&output, 0, &readback, 0, 64);
    assert!(renderer.commit(frame.submit(&queue)));
    let (sender, receiver) = std::sync::mpsc::channel();
    readback.slice(..).map_async(wgpu::MapMode::Read, move |result| sender.send(result).unwrap());
    device.poll(wgpu::PollType::Wait).unwrap();
    receiver.recv().unwrap().unwrap();
    let mapped = readback.slice(..).get_mapped_range();
    let hits: &[[u32; 4]] = bytemuck::cast_slice(&mapped);
    assert_eq!(hits[0][0..3], [1, 2, 0]);
    assert_eq!(f32::from_bits(hits[0][3]), 4.0);
    assert_eq!(hits[1][0..3], [1, 1, 0]);
    assert_eq!(f32::from_bits(hits[1][3]), 5.0);
    assert_eq!(hits[2], [0, 0, 0, 0]);
    assert_eq!(hits[3][0..3], [1, 0, 0]);
    drop(mapped);
    readback.unmap();
    let old_blas = renderer.meshes[0].blas.clone();
    let mut moved = instances;
    moved[1].model = Mat4::from_translation(Vec3::new(4.0, 0.0, 0.0)).to_cols_array_2d();
    renderer.set_scene(&vertices, &triangles, 9, 2, &moved, NativeRtEpochs { geometry: 1, transforms: 2 }).unwrap();
    let frame = renderer.prepare(&device, encoder(), &pipelines, NativeRtBuildBudget::default()).unwrap();
    assert_eq!(frame.status(), NativeRtStatus::Ready);
    assert!(frame.staged.is_empty());
    let submitted = frame.submit(&queue);
    renderer.clear();
    assert!(!renderer.commit(submitted));
    assert!(renderer.ready.is_none());
    drop(old_blas);
    device.poll(wgpu::PollType::Wait).unwrap();
    assert!(pollster::block_on(device.pop_error_scope()).is_none());
}
