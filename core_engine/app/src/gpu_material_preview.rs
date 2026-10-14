use super::{CachedMatBakePipe, compile_material_pipeline, parameterize_graph_wgsl, wgsl_source_hash};
use crate::material_textures::MaterialTextureArray;
use eframe::wgpu;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};

const MAX_RESIDENT_GRAPHS: usize = 48;
const MAX_RESIDENT_COMPILES: usize = 2;

#[derive(Default)]
pub(crate) struct ResidentMaterialCache {
    entries: HashMap<u64, ResidentEntry>,
}

impl ResidentMaterialCache {
    pub(crate) fn invalidate_surface_binding(&mut self, generation: u64) {
        for entry in self.entries.values_mut() {
            if let ResidentEntry::Ready(resources) = entry { resources.bindings.retain(|(_, surface), _| *surface != generation); }
        }
    }
}

enum ResidentEntry {
    Compiling(Receiver<Result<CachedMatBakePipe, String>>),
    Compiled(CachedMatBakePipe),
    Ready(ResidentResources),
    Failed(String),
}

struct ResidentResources {
    compiled: CachedMatBakePipe,
    graph_params: wgpu::Buffer,
    dispatch_params: wgpu::Buffer,
    bindings: HashMap<(u64, u64), wgpu::BindGroup>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResidentParams {
    res: u32,
    channels: u32,
    high_precision: u32,
    _pad: u32,
    slots: [u32; 4],
    detail_slots: [u32; 4],
}

pub(crate) enum ResidentPreviewStatus {
    Warming,
    Submitted([u32; 6]),
    Failed(String),
}

#[derive(Default)]
pub(crate) struct NodePreviewCache {
    textures: Option<MaterialTextureArray>,
    pipelines: ResidentMaterialCache,
    owner: Option<crate::NodeGraphTarget>,
    records: HashMap<crate::shader_nodes::NodeId, (u64, u32, eframe::egui::TextureId)>,
    generation: u64,
}

impl crate::AzraApp {
    pub(crate) fn poll_node_gpu_previews(&mut self) {
        use crate::frame_scheduler::{WorkClass, WorkTicket};
        use crate::shader_nodes::{NodeGraph, NodeType};
        const MAX_NODE_PREVIEWS_PER_FRAME: usize = 2;
        let requests = std::mem::take(&mut self.ui.node_drag_state.gpu_noise_preview_requests);
        if requests.is_empty() { return; }
        let ticket = WorkTicket { class: WorkClass::High, cost_hint_ms: 0.5, name: "gpu_node_previews" };
        if !self.scheduler.can_run(&ticket) { self.scheduler.record_skipped(&ticket); return; }
        let started = std::time::Instant::now();
        let Some(gpu) = self.gpu.as_mut() else { return; };
        let cache = &mut gpu.mat_bake_pipe_cache.thumbnails;
        let state = &mut self.ui.node_drag_state;
        if cache.owner != self.ui.node_graph_target {
            let mut renderer = gpu.render_state.renderer.write();
            for (_, _, texture) in cache.records.values() { renderer.free_texture(texture); }
            cache.records.clear();
            state.gpu_noise_preview_ids.clear();
            state.gpu_noise_preview_pending.clear();
            cache.textures = None;
            cache.owner = self.ui.node_graph_target;
        }
        let mut submitted = 0;
        for (node_id, fingerprint) in requests {
            if submitted >= MAX_NODE_PREVIEWS_PER_FRAME { break; }
            if cache.records.get(&node_id).is_some_and(|record| record.0 == fingerprint) { continue; }
            if cache.records.len() >= MAX_RESIDENT_GRAPHS && !cache.records.contains_key(&node_id) { continue; }
            let mut graph = self.ui.node_graph.clone();
            let outputs: Vec<_> = graph.nodes.iter().filter(|node| matches!(node.node_type, NodeType::MaterialOutput)).map(|node| node.id).collect();
            for output in outputs { graph.remove_node(output); }
            let output = graph.add_node(NodeType::MaterialOutput, [0.0, 0.0]);
            if !graph.connect(node_id, 0, output, NodeGraph::MO_ALBEDO)
                || crate::node_graph_bake::graph_gpu_bake_reject_reason_for(&graph, None).is_some()
            { state.gpu_noise_preview_pending.remove(&node_id); continue; }
            state.gpu_noise_preview_pending.insert(node_id);
            let textures = cache.textures.get_or_insert_with(|| MaterialTextureArray::new(&gpu.device, 64));
            submitted += 1;
            match submit_resident_preview(&gpu.device, &gpu.queue, textures, &mut cache.pipelines, &graph.compile_wgsl(), node_id, 1, 0) {
                ResidentPreviewStatus::Warming => {}
                ResidentPreviewStatus::Failed(error) => {
                    eprintln!("[NodePreview] {error}");
                    state.gpu_noise_preview_pending.remove(&node_id);
                }
                ResidentPreviewStatus::Submitted(slots) => {
                    let mut renderer = gpu.render_state.renderer.write();
                    if textures.generation != cache.generation {
                        for (id, (_, slot, texture)) in &mut cache.records {
                            renderer.free_texture(texture);
                            *texture = renderer.register_native_texture(&gpu.device, &textures.slot_view(*slot), wgpu::FilterMode::Linear);
                            state.gpu_noise_preview_ids.insert(*id, *texture);
                        }
                        cache.generation = textures.generation;
                    }
                    let texture = if let Some(record) = cache.records.get(&node_id) { record.2 }
                        else { renderer.register_native_texture(&gpu.device, &textures.slot_view(slots[0]), wgpu::FilterMode::Linear) };
                    cache.records.insert(node_id, (fingerprint, slots[0], texture));
                    state.gpu_noise_preview_ids.insert(node_id, texture);
                    state.gpu_noise_preview_pending.remove(&node_id);
                }
            }
        }
        self.scheduler.record(&ticket, started.elapsed().as_secs_f32() * 1000.0);
    }
}

pub(crate) fn submit_resident_preview(
    device: &std::sync::Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    textures: &mut MaterialTextureArray,
    cache: &mut ResidentMaterialCache,
    node_wgsl: &str,
    material_id: u32,
    channels: u32,
    high_precision: u32,
) -> ResidentPreviewStatus {
    submit_surface_preview(device, queue, textures, cache, node_wgsl, &format!("mat_{material_id}"), channels, high_precision, None)
}

pub(crate) fn submit_surface_preview(
    device: &std::sync::Arc<wgpu::Device>, queue: &wgpu::Queue,
    textures: &mut MaterialTextureArray, cache: &mut ResidentMaterialCache,
    node_wgsl: &str, owner: &str, channels: u32, high_precision: u32,
    surface: Option<(&wgpu::Buffer, u64)>,
) -> ResidentPreviewStatus {
    match textures.prepare_gpu_filtering() {
        Ok(true) => {}
        Ok(false) => return ResidentPreviewStatus::Warming,
        Err(error) => return ResidentPreviewStatus::Failed(error),
    }
    for entry in cache.entries.values_mut() {
        if let ResidentEntry::Compiling(rx) = entry {
            match rx.try_recv() {
                Ok(Ok(compiled)) => *entry = ResidentEntry::Compiled(compiled),
                Ok(Err(error)) => *entry = ResidentEntry::Failed(error),
                Err(TryRecvError::Disconnected) => *entry = ResidentEntry::Failed("Material preview compiler stopped".into()),
                Err(TryRecvError::Empty) => {}
            }
        }
    }
    let (code, values) = parameterize_graph_wgsl(node_wgsl);
    let mut entry = include_str!("../../graphics/src/material_resident.wgsl").to_string();
    let mut code = code;
    if surface.is_some() {
        code = code.replace(crate::shader_nodes::nodes::curvature::CONTEXT_STUB,
            "fn sn_mesh_curvature(position: vec3<f32>, radius: f32) -> f32 { let value = mesh_surfaces[mesh_pixel]; return abs(value.normal.w) / 1.57079632679 * (1.0 - smoothstep(0.0, max(radius, 1e-6), value.position.w)); }");
        entry = format!("struct MeshSurface {{ position: vec4<f32>, normal: vec4<f32> }}\n@group(0) @binding(2) var<storage, read> mesh_surfaces: array<MeshSurface>;\nvar<private> mesh_pixel: u32;\n{entry}")
            .replace("    let material = eval_node_material(0u, vec3(uv * 2.0 - 1.0, 0.0), vec3(0.0, 0.0, 1.0));",
                "    mesh_pixel = gid.y * resident.res + gid.x;\n    let material = eval_node_material(0u, mesh_surfaces[mesh_pixel].position.xyz, mesh_surfaces[mesh_pixel].normal.xyz);");
    }
    let source = graphics::shader_build::build_material_graph_wgsl(&code, &entry);
    let key = wgsl_source_hash(&source);
    if !cache.entries.contains_key(&key) {
        let compiling = cache.entries.values().filter(|entry| matches!(entry, ResidentEntry::Compiling(_))).count();
        if compiling >= MAX_RESIDENT_COMPILES { return ResidentPreviewStatus::Warming; }
        if cache.entries.len() >= MAX_RESIDENT_GRAPHS {
            if let Some(evict) = cache.entries.iter().find_map(|(key, entry)| {
                (!matches!(entry, ResidentEntry::Compiling(_))).then_some(*key)
            }) { cache.entries.remove(&evict); }
        }
        let (tx, rx) = mpsc::channel();
        let device = device.clone();
        std::thread::spawn(move || {
            let mut entries = graphics::bgl::material_resident_bgl_entries();
            if source.contains("mesh_surfaces:") { entries.push(graphics::bgl::poly_surface_entry()); }
            let result = compile_material_pipeline(
                &device, &source, &entries, "material_resident",
            );
            let _ = tx.send(result);
        });
        cache.entries.insert(key, ResidentEntry::Compiling(rx));
        return ResidentPreviewStatus::Warming;
    }
    let entry = cache.entries.get_mut(&key).unwrap();
    if matches!(entry, ResidentEntry::Compiled(_)) {
        if let ResidentEntry::Compiled(compiled) = std::mem::replace(entry, ResidentEntry::Failed(String::new())) {
                *entry = ResidentEntry::Ready(ResidentResources {
                    compiled,
                    graph_params: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("resident_graph_params"), size: (values.len() * 4) as u64,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    dispatch_params: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("resident_dispatch_params"), size: std::mem::size_of::<ResidentParams>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    bindings: HashMap::new(),
                });
        }
    }
    let resources = match entry {
        ResidentEntry::Ready(resources) => resources,
        ResidentEntry::Failed(error) => return ResidentPreviewStatus::Failed(error.clone()),
        ResidentEntry::Compiling(_) => return ResidentPreviewStatus::Warming,
        ResidentEntry::Compiled(_) => unreachable!(),
    };
    let mut slots = [0; 6];
    for (channel, suffix) in ["albedo", "rough", "metal", "emission", "normal", "bump"].into_iter().enumerate() {
        if channels & (1 << channel) != 0 {
            slots[channel] = textures.reserve_gpu_map(device, queue, &format!("proc_{owner}_{suffix}"));
        }
    }
    let surface_generation = surface.map_or(0, |(_, generation)| generation);
    resources.bindings.retain(|(generation, _), _| *generation == textures.generation);
    let binding_key = (textures.generation, surface_generation);
    if !resources.bindings.contains_key(&binding_key) {
        let storage_view = textures.storage_view();
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: resources.dispatch_params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&storage_view) },
            wgpu::BindGroupEntry { binding: 5, resource: resources.graph_params.as_entire_binding() },
        ];
        if let Some((surface, _)) = surface { entries.push(wgpu::BindGroupEntry { binding: 2, resource: surface.as_entire_binding() }); }
        resources.bindings.insert(binding_key, device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resident_material_binding"), layout: &resources.compiled.bgl,
            entries: &entries,
        }));
    }
    queue.write_buffer(&resources.graph_params, 0, bytemuck::cast_slice(&values));
    queue.write_buffer(&resources.dispatch_params, 0, bytemuck::bytes_of(&ResidentParams {
        res: textures.layer_res(), channels, high_precision, _pad: 0,
        slots: [slots[0], slots[1], slots[2], slots[3]],
        detail_slots: [slots[4], slots[5], 0, 0],
    }));
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&resources.compiled.pipeline);
        pass.set_bind_group(0, &resources.bindings[&binding_key], &[]);
        let groups = textures.layer_res().div_ceil(8);
        pass.dispatch_workgroups(groups, groups, 1);
    }
    for (channel, slot) in slots.into_iter().enumerate() {
        if slot != 0 { textures.mark_gpu_authored(slot, high_precision & (1 << channel) != 0, channel == 5 && high_precision & (1 << channel) != 0, channel == 0 || channel == 3); }
    }
    textures.encode_gpu_mips(device, queue, &mut encoder, &slots);
    queue.submit([encoder.finish()]);
    ResidentPreviewStatus::Submitted(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shader_nodes::{NodeGraph, NodeType};

    #[test]
    fn resident_mask_and_spatial_bump_strength_update_without_pipeline_recompile() {
        let (_lock, device, queue) = crate::gpu_test_harness::require_adapter();
        let device = std::sync::Arc::new(device);
        let mut textures = MaterialTextureArray::new(&device, 64);
        let mut cache = ResidentMaterialCache::default();
        let mut graph = NodeGraph::default();
        graph.bake_res = 64;
        let output = graph.output_node().unwrap().id;
        let noise = graph.add_node(NodeType::Noise, [0.0, 0.0]);
        let mask = crate::shader_nodes::Node::mask_input_index(&NodeType::Noise).unwrap();
        graph.find_node_mut(noise).unwrap().input_defaults[mask].value_f32 = 0.0;
        assert!(graph.connect(noise, 0, output, NodeGraph::MO_HEIGHT));
        for stage in 0..2 {
            let bits = if stage == 0 { 32 } else { 16 };
            if stage == 1 {
                let coord = graph.add_node(NodeType::TexCoord, [0.0, 0.0]);
                let separate = graph.add_node(NodeType::SeparateXyz, [0.0, 0.0]);
                assert!(graph.connect(coord, 0, separate, 0));
                assert!(graph.connect(separate, 0, output, NodeGraph::MO_HEIGHT));
                assert!(graph.connect(noise, 0, output, NodeGraph::MO_BUMP_STRENGTH));
            }
            graph.find_node_mut(noise).unwrap().input_defaults[mask].value_f32 = 0.0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let slots = loop {
                match submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, bits, bits) {
                    ResidentPreviewStatus::Submitted(slots) => break slots,
                    ResidentPreviewStatus::Failed(error) => panic!("{error}"),
                    ResidentPreviewStatus::Warming => {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            };
            let slot = slots[if stage == 0 { 5 } else { 4 }];
            let zero = textures.snapshot_slot(&device, &queue, slot).unwrap();
            if stage == 0 { assert!(zero.chunks_exact(4).all(|pixel| pixel[..3] == [0, 0, 0])); }
            else {
                let expected = crate::material_textures::encode_normal_rgba16_planes([0.0, 0.0, 1.0]);
                assert!(zero.chunks_exact(4).all(|pixel| pixel == expected.0));
            }
            let pipeline_count = cache.entries.len();
            let generation = textures.generation;
            graph.find_node_mut(noise).unwrap().input_defaults[mask].value_f32 = 1.0;
            assert!(matches!(submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, bits, bits), ResidentPreviewStatus::Submitted(updated) if updated == slots));
            let full = textures.snapshot_slot(&device, &queue, slot).unwrap();
            assert_ne!(zero, full);
            assert!(full.chunks_exact(4).collect::<std::collections::HashSet<_>>().len() > 20);
            graph.find_node_mut(noise).unwrap().input_defaults[mask].value_f32 = 0.25;
            assert!(matches!(submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, bits, bits), ResidentPreviewStatus::Submitted(updated) if updated == slots));
            assert_ne!(full, textures.snapshot_slot(&device, &queue, slot).unwrap());
            assert_eq!(cache.entries.len(), pipeline_count);
            assert_eq!(textures.generation, generation);
            assert!(textures.slot_rgba(slot).is_none());
        }
    }

    #[test]
    fn resident_height_normal_uses_real_derivatives_and_reuses_slider_pipeline() {
        let (_lock, device, queue) = crate::gpu_test_harness::require_adapter();
        let device = std::sync::Arc::new(device);
        let mut textures = MaterialTextureArray::new(&device, 64);
        let mut cache = ResidentMaterialCache::default();
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let coord = graph.add_node(NodeType::TexCoord, [0.0, 0.0]);
        let separate = graph.add_node(NodeType::SeparateXyz, [340.0, 0.0]);
        let noise = graph.add_node(NodeType::Noise, [0.0, 300.0]);
        assert!(graph.connect(coord, 0, separate, 0));
        assert!(graph.connect(separate, 0, output, NodeGraph::MO_NORMAL));
        assert!(graph.connect(noise, 0, output, NodeGraph::MO_HEIGHT));
        let converter = graph.connection_source(output, NodeGraph::MO_NORMAL).unwrap().0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let slots = loop {
            match submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 48, 48) {
                ResidentPreviewStatus::Submitted(slots) => break slots,
                ResidentPreviewStatus::Failed(error) => panic!("{error}"),
                ResidentPreviewStatus::Warming => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        };
        let expected = crate::material_textures::encode_normal_rgba16_planes([-1.0, 0.0, 1.0]);
        let high = textures.snapshot_slot(&device, &queue, slots[4]).unwrap();
        assert_eq!(&high[..4], &expected.0);
        assert!(high.chunks_exact(4).all(|pixel| pixel == &high[..4]));
        let bump_before = textures.snapshot_slot(&device, &queue, slots[5]).unwrap();
        let distinct: std::collections::HashSet<_> = bump_before.chunks_exact(4).map(|pixel| pixel[0]).collect();
        assert!(distinct.len() > 20);
        let generation = textures.generation;
        graph.find_node_mut(converter).unwrap().input_defaults[1].value_f32 = 2.0;
        graph.find_node_mut(noise).unwrap().input_defaults[1].value_f32 = 12.0;
        assert!(matches!(submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 48, 48), ResidentPreviewStatus::Submitted(updated) if updated == slots));
        let expected = crate::material_textures::encode_normal_rgba16_planes([-2.0, 0.0, 1.0]);
        assert_eq!(&textures.snapshot_slot(&device, &queue, slots[4]).unwrap()[..4], &expected.0);
        assert_ne!(textures.snapshot_slot(&device, &queue, slots[5]).unwrap(), bump_before);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(textures.generation, generation);
        assert!(textures.slot_rgba(slots[4]).is_none());
        assert!(textures.slot_rgba(slots[5]).is_none());
        assert!(graph.connect(noise, 0, converter, 0));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 48, 48) {
                ResidentPreviewStatus::Submitted(updated) => { assert_eq!(updated, slots); break; }
                ResidentPreviewStatus::Failed(error) => panic!("{error}"),
                ResidentPreviewStatus::Warming => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        let noise_normal = textures.snapshot_slot(&device, &queue, slots[4]).unwrap();
        let distinct: std::collections::HashSet<_> = noise_normal.chunks_exact(4).collect();
        assert!(distinct.len() > 100, "noise normals must vary with the actual height field");
        let mut times = Vec::new();
        for step in 0..30 {
            graph.find_node_mut(noise).unwrap().input_defaults[1].value_f32 = 12.0 + step as f32 * 0.1;
            let started = std::time::Instant::now();
            assert!(matches!(submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 48, 48), ResidentPreviewStatus::Submitted(updated) if updated == slots));
            device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        eprintln!("Resident noise bump + normal 64: GPU-complete p95={:.3}ms", times[28]);
        assert_ne!(noise_normal, textures.snapshot_slot(&device, &queue, slots[4]).unwrap());
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(textures.generation, generation);
        assert!(graph.connect(separate, 0, output, NodeGraph::MO_HEIGHT));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 32, 32) {
                ResidentPreviewStatus::Submitted(updated) => { assert_eq!(updated[5], slots[5]); break; }
                ResidentPreviewStatus::Failed(error) => panic!("{error}"),
                ResidentPreviewStatus::Warming => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        textures.upload_map(&device, &queue, &vec![128; 128 * 128 * 4], "height_resize_probe");
        let high = textures.snapshot_slot(&device, &queue, slots[5]).unwrap();
        let low = textures.snapshot_slot(&device, &queue, slots[5] + 1).unwrap();
        for column in 66..126 {
            let source_position = (column as f32 + 0.5) * 0.5 - 0.5;
            let lower = source_position.floor();
            let fraction = source_position.fract();
            let left = (((lower + 0.5) / 64.0 * 2.0 - 1.0) * 65535.0).round();
            let right = (((lower + 1.5) / 64.0 * 2.0 - 1.0) * 65535.0).round();
            let height = (left + (right - left) * fraction).round() / 65535.0;
            let expected = crate::material_textures::encode_data_scalar_rgba16_planes(height);
            let offset = column * 4;
            assert_eq!(&high[offset..offset + 4], &expected.0);
            assert_eq!(&low[offset..offset + 4], &expected.1);
        }
    }

    #[test]
    fn resident_slider_updates_preserve_slots_and_gpu_pixels() {
        let (_lock, device, queue) = crate::gpu_test_harness::require_adapter();
        let device = std::sync::Arc::new(device);
        let mut textures = MaterialTextureArray::new(&device, 64);
        let mut cache = ResidentMaterialCache::default();
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let color = graph.add_node(NodeType::ConstantColor, [0.0, 0.0]);
        graph.find_node_mut(color).unwrap().ramp_color0 = [0.8, 0.2, 0.1];
        assert!(graph.connect(color, 0, output, NodeGraph::MO_ALBEDO));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let slots = loop {
            match submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 1, 1) {
                ResidentPreviewStatus::Submitted(slots) => break slots,
                ResidentPreviewStatus::Failed(error) => panic!("{error}"),
                ResidentPreviewStatus::Warming => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        };
        let expected = crate::material_textures::encode_srgb_color_rgba16_planes([0.8, 0.2, 0.1]);
        let high = textures.snapshot_slot(&device, &queue, slots[0]).unwrap();
        let low = textures.snapshot_slot(&device, &queue, slots[0] + 1).unwrap();
        assert_eq!(&high[..4], &expected.0);
        assert_eq!(&low[..4], &expected.1);
        assert!(textures.slot_rgba(slots[0]).is_none());
        let generation = textures.generation;
        for step in 0..50 {
            graph.find_node_mut(color).unwrap().ramp_color0 = [step as f32 / 50.0, 0.2, 0.1];
            assert!(matches!(submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 1, 1), ResidentPreviewStatus::Submitted(updated) if updated == slots));
        }
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(textures.generation, generation);
        let expected = crate::material_textures::encode_srgb_color_rgba16_planes([0.98, 0.2, 0.1]);
        assert_eq!(&textures.snapshot_slot(&device, &queue, slots[0]).unwrap()[..4], &expected.0);
        assert_eq!(&textures.snapshot_slot(&device, &queue, slots[0] + 1).unwrap()[..4], &expected.1);
        assert!(matches!(submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 2, 1, 0), ResidentPreviewStatus::Submitted(other) if other[0] != slots[0]));
        assert!(textures.generation > generation);
        assert_eq!(&textures.snapshot_slot(&device, &queue, slots[0]).unwrap()[..4], &expected.0);
        let saved = textures.export_slots(&device, &queue).unwrap();
        assert!(saved.iter().skip(1).all(|pixels| pixels.len() == 64 * 64 * 4));
        textures.upload_map(&device, &queue, &vec![128; 128 * 128 * 4], "larger");
        assert_eq!(textures.layer_res(), 128);
        assert_eq!(&textures.snapshot_slot(&device, &queue, slots[0]).unwrap()[..4], &expected.0);
    }

    #[test]
    fn resident_noise_scrub_gpu_timing() {
        let (_lock, device, queue) = crate::gpu_test_harness::require_adapter();
        let device = std::sync::Arc::new(device);
        let mut textures = MaterialTextureArray::new(&device, 1024);
        let mut cache = ResidentMaterialCache::default();
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let noise = graph.add_node(NodeType::Noise, [0.0, 0.0]);
        assert!(graph.connect(noise, 0, output, NodeGraph::MO_ALBEDO));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 1, 1) {
                ResidentPreviewStatus::Submitted(_) => break,
                ResidentPreviewStatus::Failed(error) => panic!("{error}"),
                ResidentPreviewStatus::Warming => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let before = textures.snapshot_slot(&device, &queue, 1).unwrap();
        let generation = textures.generation;
        let mut times = Vec::new();
        let mut submit_times = Vec::new();
        for step in 0..60 {
            let node = graph.find_node_mut(noise).unwrap();
            node.input_defaults[1].value_f32 = 4.0 + step as f32 * 0.05;
            node.input_defaults[3].value_f32 = 2.0 + step as f32 * 0.001;
            node.input_defaults[4].value_f32 = 0.5 + step as f32 * 0.001;
            let started = std::time::Instant::now();
            assert!(matches!(submit_resident_preview(&device, &queue, &mut textures, &mut cache, &graph.compile_wgsl(), 1, 1, 1), ResidentPreviewStatus::Submitted(_)));
            submit_times.push(started.elapsed().as_secs_f64() * 1000.0);
            device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        submit_times.sort_by(f64::total_cmp);
        eprintln!("Resident noise 1024: CPU submit p50={:.3}ms p95={:.3}ms; GPU-complete p50={:.3}ms p95={:.3}ms", submit_times[30], submit_times[57], times[30], times[57]);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(textures.generation, generation);
        let after = textures.snapshot_slot(&device, &queue, 1).unwrap();
        assert_ne!(before, after, "noise slider updates must change resident pixels");
        let distinct: std::collections::HashSet<_> = after.iter().step_by(4).collect();
        assert!(distinct.len() > 20, "noise output must not be uniform");
    }
}
