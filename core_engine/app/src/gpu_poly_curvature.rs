use super::{compile_material_pipeline, wgsl_source_hash, CachedMatBakePipe};
use super::preview::{submit_surface_preview, ResidentMaterialCache, ResidentPreviewStatus};
use core_math::half_edge::{FaceId, HalfEdgeMesh, newell_normal};
use core_math::mesh_curvature::{mesh_curvature_edges, RingMesh};
use core_math::pbr_material::{MapKind, PbrMaterial};
use core_math::scene_graph::{MeshCacheId, NodeId, NodePayload};
use crate::shader_nodes::{NodeGraph, NodeType};
use eframe::wgpu;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver};
use wgpu::util::DeviceExt;

#[cfg(test)]
#[path = "gpu_poly_curvature_viewport_test.rs"]
mod viewport_tests;

const MAX_SURFACE_CACHE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TRIANGLES: usize = 4096;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Triangle { positions: [[f32; 4]; 3], normals: [[f32; 4]; 3], edges: [u32; 4] }
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Edge { a: [f32; 4], b: [f32; 4], n0: [f32; 4], n1: [f32; 4] }
#[derive(Default)]
pub(crate) struct PolyCurvatureCache {
    pipeline: Option<CachedMatBakePipe>,
    compiling: Option<Receiver<Result<CachedMatBakePipe, String>>>,
    pipeline_error: Option<String>,
    records: HashMap<NodeId, Record>,
    errors: HashMap<NodeId, String>,
    blocked: HashMap<NodeId, ((MeshCacheId, u64, u32), u64)>,
    graph_sources: HashMap<u32, GraphSource>,
    contexts: HashMap<NodeId, ((MeshCacheId, u64), u32)>,
    scan_cursor: usize,
    work: VecDeque<NodeId>,
    queued: HashSet<NodeId>,
    generation: u64,
    resident: ResidentMaterialCache,
}
#[derive(Clone)]
struct GraphSource { code: std::sync::Arc<str>, fingerprint: u64, valid: Result<(), String>, curvature: bool }
struct Record {
    source: (MeshCacheId, u64, u32),
    mesh: MeshCacheId,
    surface: Option<wgpu::Buffer>,
    generation: u64,
    code: u64,
    material: u32,
    material_signature: u64,
    pbr: Option<PbrMaterial>,
    texture_generation: u64,
    used: u64,
}
struct Atlas {
    triangles: Vec<Triangle>, edges: Vec<Edge>, vertices: Vec<[f32; 8]>, indices: Vec<u32>, cells: u32,
    bvh: Vec<core_math::bvh::BvhNode>, order: Vec<u32>, edge_count: u32,
}

fn atlas(mesh: &HalfEdgeMesh, vertices: &[[f32; 8]], indices: &[u32], res: u32) -> Result<Atlas, String> {
    let count = indices.len() / 3;
    if count == 0 || count > MAX_TRIANGLES { return Err(format!("Curvature atlas supports 1..{MAX_TRIANGLES} display triangles")); }
    let cells = ((count as f64).sqrt().ceil() as u32).next_power_of_two();
    let tile = res / cells;
    if tile < 8 { return Err("Curvature atlas needs at least 8 texels per triangle".into()); }
    let rings = RingMesh::from_half_edge(mesh);
    let topology = mesh_curvature_edges(&rings)?;
    let normals: Vec<_> = (0..rings.face_count()).map(|face| {
        let points: Vec<_> = rings.face(face).iter().map(|&vertex| rings.positions[vertex as usize]).collect();
        newell_normal(&points).normalize_or_zero().extend(0.0).to_array()
    }).collect();
    let mut components: Vec<_> = (0..rings.face_count()).collect();
    let mut component_sizes = vec![1usize; rings.face_count()];
    for edge in &topology {
        if let [Some(left), Some(right)] = edge.faces {
            let mut left = left as usize; let mut right = right as usize;
            while components[left] != left { left = components[left]; }
            while components[right] != right { right = components[right]; }
            if left != right {
                if component_sizes[left] < component_sizes[right] { std::mem::swap(&mut left, &mut right); }
                components[right] = left; component_sizes[left] += component_sizes[right];
            }
        }
    }
    for face in 0..components.len() { let mut root = face; while components[root] != root { root = components[root]; } components[face] = root; }
    let mut edges = Vec::new();
    let mut feature_counts = vec![0u32; rings.face_count()];
    for edge in topology {
        let component = components[edge.faces[0].unwrap() as usize];
        let mut n0 = edge.faces[0].map_or([0.0; 4], |face| normals[face as usize]);
        let n1 = edge.faces[1].map_or([0.0; 4], |face| normals[face as usize]);
        if n0 == n1 || n1 == [0.0; 4] { continue; }
        n0[3] = component as f32;
        let value = Edge {
            a: rings.positions[edge.vertices[0] as usize].extend(0.0).to_array(),
            b: rings.positions[edge.vertices[1] as usize].extend(0.0).to_array(),
            n0, n1,
        };
        edges.push(value);
        feature_counts[component] += 1;
    }
    let mut face_order: Vec<_> = (0..mesh.faces.len()).filter(|&face| mesh.is_face_live(FaceId(face as u32))).collect();
    face_order.sort_by_key(|&face| mesh.faces[face].material_id);
    let mut owners = Vec::new();
    for face in face_order {
        let corners = rings.face(face).len();
        if corners >= 3 { owners.extend(std::iter::repeat_n(face, corners)); }
    }
    if owners.len() != vertices.len() { return Err("Curvature display corner mapping mismatch".into()); }
    let bounds: Vec<_> = edges.iter().enumerate().map(|(index, edge)| {
        let a = glam::Vec3::from_array([edge.a[0], edge.a[1], edge.a[2]]); let b = glam::Vec3::from_array([edge.b[0], edge.b[1], edge.b[2]]);
        (index as u32, core_math::bvh::Aabb { min: a.min(b).to_array(), max: a.max(b).to_array() })
    }).collect();
    let (mut bvh, mut order) = core_math::bvh::build_bvh(&bounds);
    let edge_count = edges.len() as u32;
    if edges.is_empty() {
        edges.push(Edge { a: [0.0; 4], b: [0.0; 4], n0: [0.0; 4], n1: [0.0; 4] });
        bvh.push(core_math::bvh::BvhNode { aabb_min: [0.0; 3], first: 0, aabb_max: [0.0; 3], count: 0 }); order.push(0);
    }
    let mut output = Atlas { triangles: Vec::with_capacity(count), edges, vertices: Vec::with_capacity(indices.len()), indices: (0..indices.len() as u32).collect(), cells, bvh, order, edge_count };
    for (index, triangle) in indices.chunks_exact(3).enumerate() {
        let face = owners[triangle[0] as usize];
        if triangle.iter().any(|&vertex| owners[vertex as usize] != face) { return Err("Curvature triangle crosses polygon faces".into()); }
        let corners = [vertices[triangle[0] as usize], vertices[triangle[1] as usize], vertices[triangle[2] as usize]];
        output.triangles.push(Triangle {
            positions: corners.map(|vertex| [vertex[0], vertex[1], vertex[2], 0.0]),
            normals: corners.map(|vertex| [vertex[3], vertex[4], vertex[5], 0.0]), edges: [0, 0, components[face] as u32, feature_counts[components[face]]],
        });
        let origin = [(index as u32 % cells) * tile, (index as u32 / cells) * tile];
        for (corner, mut vertex) in corners.into_iter().enumerate() {
            let uv = match corner { 0 => [2, 2], 1 => [tile - 2, 2], _ => [2, tile - 2] };
            vertex[6] = (origin[0] + uv[0]) as f32 / res as f32;
            vertex[7] = (origin[1] + uv[1]) as f32 / res as f32;
            output.vertices.push(vertex);
        }
    }
    Ok(output)
}

fn dispatch_surface(device: &wgpu::Device, queue: &wgpu::Queue, pipeline: &CachedMatBakePipe, atlas: &Atlas, res: u32) -> wgpu::Buffer {
    let buffer = |label, bytes: &[u8], usage| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytes, usage });
    let params = buffer("poly_curvature_params", bytemuck::cast_slice(&[res, atlas.cells, atlas.triangles.len() as u32, atlas.edge_count]), wgpu::BufferUsages::UNIFORM);
    let triangles = buffer("poly_curvature_triangles", bytemuck::cast_slice(&atlas.triangles), wgpu::BufferUsages::STORAGE);
    let edges = buffer("poly_curvature_edges", bytemuck::cast_slice(&atlas.edges), wgpu::BufferUsages::STORAGE);
    let nodes = buffer("poly_curvature_bvh", bytemuck::cast_slice(&atlas.bvh), wgpu::BufferUsages::STORAGE);
    let order = buffer("poly_curvature_order", bytemuck::cast_slice(&atlas.order), wgpu::BufferUsages::STORAGE);
    let surface = device.create_buffer(&wgpu::BufferDescriptor { label: Some("poly_curvature_surface"), size: u64::from(res) * u64::from(res) * 32,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let binding = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("poly_curvature_binding"), layout: &pipeline.bgl,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: triangles.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: edges.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: surface.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 4, resource: nodes.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: order.as_entire_binding() }] });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    { let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline.pipeline); pass.set_bind_group(0, &binding, &[]); pass.dispatch_workgroups(res.div_ceil(8), res.div_ceil(8), 1); }
    queue.submit([encoder.finish()]);
    surface
}

pub(crate) fn has_curvature(graph: &NodeGraph) -> bool {
    graph.output_node().and_then(|output| graph.topo_sort(output.id)).is_some_and(|order| order.into_iter().any(|id| graph.find_node(id).is_some_and(|node| node.node_type == NodeType::Curvature)))
}

fn validate_graph(graph: &NodeGraph) -> Result<(), String> {
    let output = graph.output_node().ok_or("Curvature graph has no output")?;
    if graph.connections.iter().any(|connection| connection.to_node == output.id && ![NodeGraph::MO_ALBEDO, NodeGraph::MO_ROUGHNESS, NodeGraph::MO_METALLIC, NodeGraph::MO_EMISSION].contains(&connection.to_input)) {
        return Err("Mesh Curvature atlas supports Base Color, Roughness, Metallic and Emission outputs only".into());
    }
    let order = graph.output_node().and_then(|output| graph.topo_sort(output.id)).ok_or("Curvature graph has no valid output")?;
    for id in order {
        let node = graph.find_node(id).unwrap();
        if node.node_type == NodeType::Curvature {
            if graph.is_input_connected(node.id, 0) || node.input_defaults[0].value_vec3 != [0.0; 3] {
                return Err("Mesh Curvature Position warping is not supported".into());
            }
        } else if !crate::node_graph_bake::node_type_gpu_bakeable(&node.node_type) {
            return Err(format!("Mesh Curvature GPU graph does not support {:?}", node.node_type));
        }
    }
    let channels = crate::node_graph_bake::channels_from_graph(graph);
    if channels.normal || channels.bump || channels.displacement { return Err("Mesh Curvature atlas currently supports color and scalar channels, not normal/height outputs".into()); }
    Ok(())
}

impl PolyCurvatureCache {
    pub(crate) fn reset(&mut self, meshes: &mut render::mesh_cache::MeshCache) {
        for (_, record) in self.records.drain() { meshes.remove(record.mesh); self.resident.invalidate_surface_binding(record.generation); }
        self.errors.clear(); self.blocked.clear(); self.contexts.clear(); self.graph_sources.clear(); self.work.clear(); self.queued.clear(); self.scan_cursor = 0;
    }
    pub(crate) fn override_for(&self, nid: NodeId, cid: MeshCacheId, stamp: u64) -> Option<(MeshCacheId, &PbrMaterial)> {
        let record = self.records.get(&nid)?;
        (record.source.0 == cid && record.source.1 == stamp).then_some(())?;
        Some((record.mesh, record.pbr.as_ref()?))
    }
}

impl crate::AzraApp {
    pub(crate) fn poll_poly_curvature_cache(&mut self) {
        if self.ui.material_node_graphs.is_empty() && self.gpu.as_ref().is_none_or(|gpu| gpu.mat_bake_pipe_cache.poly_curvature.records.is_empty()) { return; }
        use crate::frame_scheduler::{WorkClass, WorkTicket};
        const MAX_OBJECTS_PER_FRAME: usize = 8;
        const MAX_GEOMETRY_BUILDS_PER_FRAME: usize = 1;
        const MAX_CONTEXTS_PER_FRAME: usize = 64;
        const MAX_GRAPH_PREPARES_PER_FRAME: usize = 2;
        const MAX_ATTEMPTS_PER_FRAME: usize = 32;
        const MAX_MATERIAL_CONTEXTS: usize = 4096;
        let ticket = WorkTicket { class: WorkClass::Medium, cost_hint_ms: 2.0, name: "poly_curvature_cache" };
        if !self.scheduler.can_run(&ticket) { self.scheduler.record_skipped(&ticket); return; }
        let started = std::time::Instant::now();
        let mut invalidated = false;
        let preferred = self.active_poly_nid();
        let Some(gpu) = self.gpu.as_mut() else { return; };
        let cache = &mut gpu.mat_bake_pipe_cache.poly_curvature;
        if cache.pipeline_error.is_some() { return; }
        if let Some(rx) = cache.compiling.as_ref() {
            match rx.try_recv() {
                Ok(Ok(pipeline)) => { cache.pipeline = Some(pipeline); cache.compiling = None; }
                Ok(Err(error)) => { self.ui.toasts.push(crate::Toast { message: error.clone(), remaining: self.tweak.toast_duration }); cache.pipeline_error = Some(error); cache.compiling = None; return; }
                Err(mpsc::TryRecvError::Disconnected) => { cache.compiling = None; return; }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut candidates = Vec::with_capacity(MAX_CONTEXTS_PER_FRAME * 2 + 1);
        if let Some(nid) = preferred { candidates.push(nid); }
        for _ in 0..cache.work.len().min(MAX_CONTEXTS_PER_FRAME) {
            if let Some(nid) = cache.work.pop_front() { cache.queued.remove(&nid); candidates.push(nid); }
        }
        if self.draw_stream.is_empty() {
            candidates.extend(self.scene.scene_graph.all_nodes().take(MAX_CONTEXTS_PER_FRAME).map(|(nid, _)| nid));
        } else {
            for _ in 0..self.draw_stream.len().min(MAX_CONTEXTS_PER_FRAME) {
                cache.scan_cursor %= self.draw_stream.len(); candidates.push(self.draw_stream[cache.scan_cursor]); cache.scan_cursor += 1;
            }
        }
        let mut seen = HashSet::new();
        let mut jobs = Vec::new();
        let mut graph_prepares = 0;
        for nid in candidates.into_iter().filter(|nid| seen.insert(*nid)) {
            let context = self.scene.scene_graph.get(nid).and_then(|node| {
                let NodePayload::PolygonalBody { mesh_cache_id: Some(cid), mesh, .. } = &node.payload else { return None; };
                let stamp = self.cache.mesh_cache.get(*cid)?.content_stamp;
                if let Some((source, mid)) = cache.contexts.get(&nid).filter(|(source, mid)| *source == (*cid, stamp) && self.scene.scene_graph.material_data_if_visible(core_math::primitives::MaterialId(*mid)).is_some()) { return Some((*cid, source.1, *mid)); }
                if mesh.faces.len() > MAX_TRIANGLES * 2 { return None; }
                let mid = mesh.faces.iter().find(|face| face.half_edge != core_math::half_edge::NO_EDGE && face.material_id != 0 && self.scene.scene_graph.material_data_if_visible(core_math::primitives::MaterialId(face.material_id)).is_some())?.material_id;
                if cache.contexts.len() >= MAX_MATERIAL_CONTEXTS && !cache.contexts.contains_key(&nid) {
                    if let Some(evict) = cache.contexts.keys().next().copied() { cache.contexts.remove(&evict); }
                }
                cache.contexts.insert(nid, ((*cid, stamp), mid)); Some((*cid, stamp, mid))
            });
            let mut active = false;
            if let Some((cid, _, mid)) = context {
                if let Some(graph) = self.ui.material_node_graphs.get_mut(&mid) {
                    if graph.wgsl_cache.is_none() || !cache.graph_sources.contains_key(&mid) {
                        if graph_prepares >= MAX_GRAPH_PREPARES_PER_FRAME { if cache.queued.insert(nid) { cache.work.push_back(nid); } continue; }
                        graph_prepares += 1;
                        let curvature = has_curvature(graph);
                        let code = graph.compile_wgsl();
                        graph.wgsl_cache = Some(code.clone());
                        let fingerprint = wgsl_source_hash(&code);
                        cache.graph_sources.insert(mid, GraphSource { code: code.into(), fingerprint, valid: if curvature { validate_graph(graph) } else { Ok(()) }, curvature });
                    }
                    active = cache.graph_sources[&mid].curvature;
                    if active { jobs.push((nid, cid, mid)); if cache.queued.insert(nid) { cache.work.push_back(nid); } }
                }
            }
            if !active {
                if self.scene.scene_graph.get(nid).is_none() { cache.contexts.remove(&nid); }
                cache.errors.remove(&nid); cache.blocked.remove(&nid);
                if let Some(record) = cache.records.remove(&nid) {
                    self.cache.mesh_cache.remove(record.mesh); cache.resident.invalidate_surface_binding(record.generation);
                    invalidated = true; self.structural_draw_stale = true; self.render.pt_converged = false; self.render.pt_accumulated_frames = 0;
                }
            }
        }
        if jobs.is_empty() { if invalidated { self.mark_dirty(crate::DirtyFlags::RENDER); } return; }
        if cache.pipeline.is_none() {
            if cache.compiling.is_none() {
                let device = gpu.device.clone(); let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || { let _ = tx.send(compile_material_pipeline(&device,
                    include_str!("../../graphics/src/poly_curvature_cache.wgsl"), &graphics::bgl::poly_curvature_cache_bgl_entries(), "poly_curvature_cache")); });
                cache.compiling = Some(rx);
            }
            self.mark_dirty(crate::DirtyFlags::RENDER); return;
        }
        let mut processed = 0;
        let mut geometry_builds = 0;
        let mut attempts = 0;
        let rotation = cache.work.len().min(MAX_ATTEMPTS_PER_FRAME);
        cache.work.rotate_left(rotation);
        for (nid, cid, mid) in jobs {
            if processed >= MAX_OBJECTS_PER_FRAME || attempts >= MAX_ATTEMPTS_PER_FRAME { break; }
            attempts += 1;
            let Some(stamp) = self.cache.mesh_cache.get(cid).map(|mesh| mesh.content_stamp) else { continue; };
            let source_graph = self.gpu.as_ref().unwrap().mat_bake_pipe_cache.poly_curvature.graph_sources[&mid].clone();
            let graph = &self.ui.material_node_graphs[&mid];
            let mut result = source_graph.valid;
            let mut material_signature = 0;
            if let Some((_, material)) = self.scene.scene_graph.material_by_id(core_math::primitives::MaterialId(mid)) {
                let ch = crate::node_graph_bake::channels_from_graph(graph);
                use std::hash::Hasher;
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                hash.write(bytemuck::bytes_of(&render::mesh_gpu::RasterMaterial::from_pbr(material)));
                material_signature = hash.finish();
                let generated = [ch.albedo, ch.roughness, ch.metallic, false, ch.emission, ch.normal, ch.bump, ch.displacement];
                for kind in MapKind::ALL {
                    let map = material.map(kind);
                    let generated_kind = match kind { MapKind::Albedo => generated[0], MapKind::Roughness => generated[1], MapKind::Metallic => generated[2], MapKind::Emission => generated[4], _ => false };
                    if map.tex_id != 0 && !generated_kind && map.projection == 1 {
                        result = Err("Mesh Curvature atlas cannot replace an object's retained authored-UV maps".into());
                    }
                }
            }
            let code = source_graph.code;
            let fingerprint = source_graph.fingerprint;
            let gpu = self.gpu.as_mut().unwrap();
            let res = gpu.material_textures.layer_res();
            let source = (cid, stamp, res);
            let cache = &mut gpu.mat_bake_pipe_cache.poly_curvature;
            let recovery = fingerprint ^ material_signature ^ gpu.material_textures.generation;
            if cache.blocked.get(&nid) == Some(&(source, recovery)) { continue; }
            let required = u64::from(res) * u64::from(res) * 32;
            if required > u64::from(gpu.device.limits().max_storage_buffer_binding_size) || required > gpu.device.limits().max_buffer_size || required > MAX_SURFACE_CACHE_BYTES {
                result = Err("Mesh Curvature atlas exceeds this device's surface-cache memory budget".into());
            }
            if let Err(error) = result {
                if cache.errors.get(&nid) != Some(&error) { self.ui.toasts.push(crate::Toast { message: error.clone(), remaining: self.tweak.toast_duration }); cache.errors.insert(nid, error); }
                if let Some(record) = cache.records.remove(&nid) { self.cache.mesh_cache.remove(record.mesh); cache.resident.invalidate_surface_binding(record.generation); invalidated = true; self.structural_draw_stale = true; self.render.pt_converged = false; self.render.pt_accumulated_frames = 0; }
                cache.blocked.insert(nid, (source, recovery));
                continue;
            }
            cache.generation += 1;
            if let Some(record) = cache.records.get_mut(&nid) {
                record.used = cache.generation;
                if record.source == source && record.material == mid && record.material_signature == material_signature && record.code == fingerprint && record.texture_generation == gpu.material_textures.generation && record.pbr.is_some() { continue; }
            }
            if cache.records.get(&nid).is_none_or(|record| record.source != source || record.surface.is_none()) {
                if geometry_builds >= MAX_GEOMETRY_BUILDS_PER_FRAME { continue; }
                geometry_builds += 1;
                while cache.records.iter().filter(|(owner, _)| **owner != nid).filter_map(|(_, record)| record.surface.as_ref()).map(|surface| surface.size()).sum::<u64>() + required > MAX_SURFACE_CACHE_BYTES {
                    if let Some(evict) = cache.records.iter().filter(|(owner, record)| **owner != nid && record.surface.is_some()).min_by_key(|(_, record)| record.used).map(|(owner, _)| *owner) {
                        let record = cache.records.get_mut(&evict).unwrap();
                        cache.resident.invalidate_surface_binding(record.generation); record.surface = None;
                    } else { break; }
                }
                let Some(display) = self.poly_cached_display_mesh(nid) else { continue; };
                if display.faces.iter().any(|face| face.half_edge != core_math::half_edge::NO_EDGE && face.material_id != mid) {
                    self.toast("Mesh Curvature atlas does not yet support multiple face materials");
                    let cache = &mut self.gpu.as_mut().unwrap().mat_bake_pipe_cache.poly_curvature;
                    cache.blocked.insert(nid, (source, recovery));
                    if let Some(record) = cache.records.remove(&nid) { self.cache.mesh_cache.remove(record.mesh); cache.resident.invalidate_surface_binding(record.generation); invalidated = true; self.structural_draw_stale = true; }
                    continue;
                }
                let Some((vertices, indices)) = self.poly_export_raster(nid) else { continue; };
                let built = match atlas(&display, &vertices, &indices, res) {
                    Ok(built) => built,
                    Err(error) => {
                        self.ui.toasts.push(crate::Toast { message: error, remaining: self.tweak.toast_duration });
                        let cache = &mut self.gpu.as_mut().unwrap().mat_bake_pipe_cache.poly_curvature;
                        cache.blocked.insert(nid, (source, recovery));
                        if let Some(record) = cache.records.remove(&nid) { self.cache.mesh_cache.remove(record.mesh); cache.resident.invalidate_surface_binding(record.generation); invalidated = true; self.structural_draw_stale = true; }
                        continue;
                    }
                };
                let gpu = self.gpu.as_mut().unwrap();
                let cache = &mut gpu.mat_bake_pipe_cache.poly_curvature;
                let previous = cache.records.remove(&nid);
                self.structural_draw_stale = true;
                invalidated = true;
                let preserve_mesh = previous.as_ref().is_some_and(|record| record.source == source);
                let mesh = if let Some(record) = previous {
                    cache.resident.invalidate_surface_binding(record.generation);
                    if preserve_mesh { record.mesh }
                    else { self.cache.mesh_cache.remove(record.mesh); self.cache.mesh_cache.alloc_id() }
                } else { self.cache.mesh_cache.alloc_id() };
                let surface = dispatch_surface(&gpu.device, &gpu.queue, cache.pipeline.as_ref().unwrap(), &built, res);
                if !preserve_mesh { self.cache.mesh_cache.insert_imported(mesh, (built.vertices, built.indices), built.triangles.len() as u32); }
                cache.generation += 1;
                cache.records.insert(nid, Record { source, mesh, surface: Some(surface), generation: cache.generation, code: 0, material: mid, material_signature: 0, pbr: None, texture_generation: 0, used: cache.generation });
            }
            let gpu = self.gpu.as_mut().unwrap();
            processed += 1;
            let cache = &mut gpu.mat_bake_pipe_cache.poly_curvature;
            let record = cache.records.get_mut(&nid).unwrap();
            let ch = crate::node_graph_bake::channels_from_graph(&self.ui.material_node_graphs[&mid]);
            let bits = u32::from(ch.albedo) | (u32::from(ch.roughness) << 1) | (u32::from(ch.metallic) << 2) | (u32::from(ch.emission) << 3);
            let status = submit_surface_preview(&gpu.device, &gpu.queue, &mut gpu.material_textures, &mut cache.resident,
                &code, &format!("poly_curvature_{}", nid.0), bits, bits, Some((record.surface.as_ref().unwrap(), record.generation)));
            match status {
                ResidentPreviewStatus::Submitted(slots) => {
                    let Some((_, material)) = self.scene.scene_graph.material_by_id(core_math::primitives::MaterialId(mid)) else { continue; };
                    let mut pbr = material.clone();
                    for (index, kind) in [MapKind::Albedo, MapKind::Roughness, MapKind::Metallic, MapKind::Emission].into_iter().enumerate() {
                        if slots[index] == 0 { continue; }
                        let map = pbr.map_mut(kind); map.tex_id = slots[index]; map.strength = 1.0; map.projection = 1; map.uv_scale = 1.0; map.high_precision = true; map.raw_precision = false; map.absolute = true;
                    }
                    record.pbr = Some(pbr); record.code = fingerprint; record.texture_generation = gpu.material_textures.generation; record.material_signature = material_signature;
                    gpu.mat_bake_pipe_cache.poly_curvature.errors.remove(&nid);
                    self.mark_dirty(crate::DirtyFlags::RENDER); self.render.pt_accumulated_frames = 0;
                    self.structural_draw_stale = true; self.render.pt_converged = false;
                }
                ResidentPreviewStatus::Warming => { self.mark_dirty(crate::DirtyFlags::RENDER); }
                ResidentPreviewStatus::Failed(error) => {
                    self.ui.toasts.push(crate::Toast { message: error, remaining: self.tweak.toast_duration });
                    let cache = &mut gpu.mat_bake_pipe_cache.poly_curvature;
                    cache.blocked.insert(nid, (source, recovery));
                    if let Some(record) = cache.records.remove(&nid) { self.cache.mesh_cache.remove(record.mesh); cache.resident.invalidate_surface_binding(record.generation); }
                    invalidated = true; self.structural_draw_stale = true; self.render.pt_converged = false; self.render.pt_accumulated_frames = 0;
                }
            }
        }
        self.scheduler.record(&ticket, started.elapsed().as_secs_f32() * 1000.0);
        if invalidated { self.mark_dirty(crate::DirtyFlags::RENDER); self.render.pt_converged = false; self.render.pt_accumulated_frames = 0; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cube_surface_cache_and_resident_graph_are_edge_local_and_reusable() {
        let (_lock, device, queue) = crate::gpu_test_harness::require_adapter();
        let device = std::sync::Arc::new(device);
        let mesh = HalfEdgeMesh::new_box(glam::Vec3::ONE);
        let (vertices, indices) = core_math::poly_mesh::raster_data_ngon(&mesh);
        let atlas = atlas(&mesh, &vertices, &indices, 256).unwrap();
        let pipeline = compile_material_pipeline(&device, include_str!("../../graphics/src/poly_curvature_cache.wgsl"),
            &graphics::bgl::poly_curvature_cache_bgl_entries(), "poly_curvature_cache").unwrap();
        let surface = dispatch_surface(&device, &queue, &pipeline, &atlas, 256);
        let staging = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: surface.size(), usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&surface, 0, &staging, 0, surface.size()); queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel(); staging.slice(..).map_async(wgpu::MapMode::Read, move |result| { tx.send(result).unwrap(); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed"); rx.recv().unwrap().unwrap();
        let mapped = staging.slice(..).get_mapped_range();
        let samples: &[[f32; 8]] = bytemuck::cast_slice(&mapped);
        let mut faces = std::collections::HashSet::new();
        let mut edges = std::collections::HashSet::new();
        let mut centers = Vec::new();
        let mut rims = Vec::new();
        for (index, value) in samples.iter().enumerate() {
            let point = glam::Vec3::from_array([value[0], value[1], value[2]]);
            if point == glam::Vec3::ZERO { continue; }
            assert!((value[7] - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
            let coord = point.to_array();
            let boundary: Vec<_> = (0..3).filter(|&axis| coord[axis].abs() > 0.99).collect();
            if boundary.len() == 1 && coord.iter().filter(|component| component.abs() < 0.15).count() == 2 {
                faces.insert((boundary[0], coord[boundary[0]] > 0.0)); centers.push(index);
                assert!(value[3] > 0.8);
            }
            if boundary.len() == 2 {
                let axis = (0..3).find(|axis| !boundary.contains(axis)).unwrap();
                if coord[axis].abs() < 0.2 {
                    edges.insert((axis, coord[boundary[0]] > 0.0, coord[boundary[1]] > 0.0)); rims.push(index);
                    assert!(value[3] < 0.02);
                }
            }
        }
        assert_eq!(faces.len(), 6); assert_eq!(edges.len(), 12);
        drop(mapped); staging.unmap();
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let curvature = graph.add_node(NodeType::Curvature, [0.0; 2]);
        graph.find_node_mut(curvature).unwrap().input_defaults[1].value_f32 = 0.2;
        assert!(graph.connect(curvature, 0, output, NodeGraph::MO_ALBEDO));
        let code = graph.compile_wgsl();
        let mut textures = crate::material_textures::MaterialTextureArray::new(&device, 256);
        let mut cache = ResidentMaterialCache::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let slots = loop {
            match submit_surface_preview(&device, &queue, &mut textures, &mut cache, &code, "poly_curvature_100", 1, 1, Some((&surface, 1))) {
                ResidentPreviewStatus::Submitted(slots) => break slots,
                ResidentPreviewStatus::Failed(error) => panic!("{error}"),
                ResidentPreviewStatus::Warming => { assert!(std::time::Instant::now() < deadline); std::thread::sleep(std::time::Duration::from_millis(10)); }
            }
        };
        let pixels = textures.snapshot_slot(&device, &queue, slots[0]).unwrap();
        for index in centers { assert!(pixels[index * 4] <= 1, "center {index}: {:?}", &pixels[index * 4..index * 4 + 4]); }
        for index in rims { assert!(pixels[index * 4] > 100); }
        graph.find_node_mut(curvature).unwrap().input_defaults[3].value_f32 = 0.0;
        let changed = graph.compile_wgsl();
        assert_eq!(super::super::parameterize_graph_wgsl(&code).0, super::super::parameterize_graph_wgsl(&changed).0);
        assert!(matches!(submit_surface_preview(&device, &queue, &mut textures, &mut cache, &changed, "poly_curvature_100", 1, 1, Some((&surface, 1))), ResidentPreviewStatus::Submitted(updated) if updated == slots));
        assert!(textures.snapshot_slot(&device, &queue, slots[0]).unwrap().chunks_exact(4).all(|pixel| pixel[0] <= 1));
        graph.find_node_mut(curvature).unwrap().input_defaults[3].value_f32 = 1.0;
        let changed = graph.compile_wgsl();
        let mut scaled = mesh.clone();
        for vertex in &mut scaled.vertices { vertex.position *= 0.1; }
        let (verts, indices) = core_math::poly_mesh::raster_data_ngon(&scaled);
        let scaled_atlas = super::atlas(&scaled, &verts, &indices, 256).unwrap();
        let scaled_surface = dispatch_surface(&device, &queue, &pipeline, &scaled_atlas, 256);
        let generation = textures.generation;
        let other = match submit_surface_preview(&device, &queue, &mut textures, &mut cache, &changed, "poly_curvature_101", 1, 1, Some((&scaled_surface, 2))) {
            ResidentPreviewStatus::Submitted(other) => other, _ => panic!("object change should reuse pipeline"),
        };
        assert_ne!(slots[0], other[0]); assert!(textures.generation >= generation);
        assert_ne!(textures.snapshot_slot(&device, &queue, other[0]).unwrap(), pixels);
        assert!(matches!(submit_surface_preview(&device, &queue, &mut textures, &mut cache, &code, "poly_curvature_100", 1, 1, Some((&surface, 3))), ResidentPreviewStatus::Submitted(updated) if updated == slots));
        assert_eq!(textures.snapshot_slot(&device, &queue, slots[0]).unwrap(), pixels);
        assert!(textures.slot_rgba(slots[0]).is_none());
        let mut subdivided = mesh.clone();
        for _ in 0..2 {
            let selected = (0..subdivided.faces.len() as u32).collect();
            subdivided.subdivide_faces(&selected);
        }
        let (verts, indices) = core_math::poly_mesh::raster_data_ngon(&subdivided);
        let subdivided_atlas = super::atlas(&subdivided, &verts, &indices, 256).unwrap();
        let subdivided_surface = dispatch_surface(&device, &queue, &pipeline, &subdivided_atlas, 256);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&subdivided_surface, 0, &staging, 0, subdivided_surface.size()); queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel(); staging.slice(..).map_async(wgpu::MapMode::Read, move |result| { tx.send(result).unwrap(); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed"); rx.recv().unwrap().unwrap();
        let mapped = staging.slice(..).get_mapped_range();
        let samples: &[[f32; 8]] = bytemuck::cast_slice(&mapped);
        for value in samples {
            let point = glam::Vec3::from_array([value[0], value[1], value[2]]);
            if point == glam::Vec3::ZERO { continue; }
            let expected = atlas.edges.iter().map(|edge| {
                let start = glam::Vec3::from_array([edge.a[0], edge.a[1], edge.a[2]]);
                let end = glam::Vec3::from_array([edge.b[0], edge.b[1], edge.b[2]]);
                let along = ((point - start).dot(end - start) / (end - start).length_squared()).clamp(0.0, 1.0);
                (point - start - along * (end - start)).length()
            }).fold(f32::INFINITY, f32::min);
            assert!((value[3] - expected).abs() < 1e-5, "subdivision changed curvature distance: {} vs {expected}", value[3]);
            assert!((value[7] - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        }
        drop(mapped); staging.unmap();
    }

    #[test]
    fn unsupported_curvature_contexts_fail_closed_and_unused_nodes_are_ignored() {
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let curvature = graph.add_node(NodeType::Curvature, [0.0; 2]);
        graph.add_node(NodeType::ImageTexture, [0.0; 2]);
        assert!(!has_curvature(&graph));
        graph.connect(curvature, 0, output, NodeGraph::MO_ALBEDO);
        assert!(validate_graph(&graph).is_ok());
        graph.find_node_mut(curvature).unwrap().input_defaults[0].value_vec3 = [1.0, 0.0, 0.0];
        assert!(validate_graph(&graph).unwrap_err().contains("Position"));
        let mesh = HalfEdgeMesh::new_box(glam::Vec3::ONE);
        let (vertices, indices) = core_math::poly_mesh::raster_data_ngon(&mesh);
        assert!(atlas(&mesh, &vertices, &indices, 16).is_err());
    }
}
