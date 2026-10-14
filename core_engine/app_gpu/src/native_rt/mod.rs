mod pipeline;
mod scene;
mod shader;

pub use pipeline::NativeRtPipelines;
pub use scene::{NativeInstance, NativeRtEpochs};
pub use shader::{build_native_rt_wgsl, NATIVE_RT_ACCELERATION_BINDING, NATIVE_RT_ACCELERATION_GROUP};

use eframe::wgpu::{self, util::DeviceExt};
use render::pt_accel::{PtTri, PtVertex};
use scene::{GeometryRange, SceneLayout};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const REQUIRED_FEATURES: wgpu::Features = wgpu::Features::EXPERIMENTAL_RAY_TRACING_ACCELERATION_STRUCTURE
    .union(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
pub const MAX_BLAS_BUILDS_PER_FRAME: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRtError(pub &'static str);

impl std::fmt::Display for NativeRtError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for NativeRtError {}

#[derive(Clone, Copy, Debug)]
pub struct NativeRtBuildBudget {
    pub max_blas: usize,
    pub max_triangles: u64,
    pub max_tlas_instances: usize,
}

impl Default for NativeRtBuildBudget {
    fn default() -> Self {
        Self { max_blas: MAX_BLAS_BUILDS_PER_FRAME, max_triangles: 1_000_000, max_tlas_instances: 4096 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRtStatus {
    Empty,
    Building { remaining_blas: usize, next_blas_triangles: u32, tlas_instances: usize },
    Ready,
}

struct SceneInputs {
    vertices: wgpu::Buffer,
    triangles: wgpu::Buffer,
    vertex_count: u32,
    triangle_count: u32,
    instances: Vec<NativeInstance>,
    epochs: NativeRtEpochs,
    layout: SceneLayout,
}

struct CachedBlas {
    blas: wgpu::Blas,
    indices: wgpu::Buffer,
    size: wgpu::BlasTriangleGeometrySizeDescriptor,
}

struct ReadyScene {
    package: wgpu::TlasPackage,
    binding: wgpu::BindGroup,
}

pub struct NativeRtRenderer {
    owner: Arc<()>,
    revision: u64,
    scene: Option<SceneInputs>,
    meshes: Vec<Arc<CachedBlas>>,
    ready: Option<Arc<ReadyScene>>,
    next_frame: AtomicU64,
    last_committed_frame: u64,
}

impl Default for NativeRtRenderer {
    fn default() -> Self {
        Self { owner: Arc::new(()), revision: 0, scene: None, meshes: Vec::new(), ready: None, next_frame: AtomicU64::new(1), last_committed_frame: 0 }
    }
}

impl NativeRtRenderer {
    pub fn set_scene(
        &mut self,
        vertices: &wgpu::Buffer,
        triangles: &wgpu::Buffer,
        vertex_count: u32,
        triangle_count: u32,
        instances: &[NativeInstance],
        epochs: NativeRtEpochs,
    ) -> Result<(), NativeRtError> {
        if !vertices.usage().contains(wgpu::BufferUsages::BLAS_INPUT)
            || !triangles.usage().contains(wgpu::BufferUsages::STORAGE)
            || vertices.size() < u64::from(vertex_count) * std::mem::size_of::<PtVertex>() as u64
            || triangles.size() < u64::from(triangle_count) * std::mem::size_of::<PtTri>() as u64
        {
            return Err(NativeRtError("native RT input buffers have incompatible usage or size"));
        }
        let same_geometry = self.scene.as_ref().is_some_and(|previous| {
            previous.epochs.geometry == epochs.geometry
                && previous.vertices == *vertices && previous.triangles == *triangles
                && previous.vertex_count == vertex_count && previous.triangle_count == triangle_count
        });
        if same_geometry && self.scene.as_ref().is_some_and(|previous| previous.epochs == epochs && previous.instances == instances) {
            return Ok(());
        }
        let layout = scene::scene_layout(vertex_count, triangle_count, instances)?;
        if same_geometry && self.scene.as_ref().is_some_and(|previous| previous.layout.ranges != layout.ranges) {
            return Err(NativeRtError("native RT geometry mapping changed without a geometry epoch"));
        }
        self.revision = self.revision.checked_add(1).expect("native RT revision overflow");
        if !same_geometry {
            self.meshes.clear();
        }
        self.ready = None;
        self.scene = Some(SceneInputs {
            vertices: vertices.clone(), triangles: triangles.clone(), vertex_count, triangle_count,
            instances: instances.to_vec(), epochs, layout,
        });
        Ok(())
    }

    /// A committed TLAS exists: `prepare` will hand out a dispatch lease this frame.
    /// Readiness changes only through `set_scene`/`clear` (→ not ready) and `commit`.
    pub fn is_ready(&self) -> bool { self.ready.is_some() }

    pub fn clear(&mut self) {
        self.revision = self.revision.checked_add(1).expect("native RT revision overflow");
        self.scene = None;
        self.meshes.clear();
        self.ready = None;
    }

    pub fn prepare(
        &self,
        device: &wgpu::Device,
        mut encoder: wgpu::CommandEncoder,
        pipelines: &NativeRtPipelines,
        budget: NativeRtBuildBudget,
    ) -> Result<NativeRtFrame, NativeRtError> {
        if !device.features().contains(REQUIRED_FEATURES) {
            return Err(NativeRtError("native RT device features are unavailable"));
        }
        let mut staged = Vec::new();
        let mut ready = self.ready.clone();
        let mut status = NativeRtStatus::Empty;
        if let Some(scene) = &self.scene {
            if !scene.layout.placements.is_empty() {
                let mut triangle_budget = budget.max_triangles;
                for range in scene.layout.ranges.iter().skip(self.meshes.len()).take(budget.max_blas.min(MAX_BLAS_BUILDS_PER_FRAME)) {
                    if u64::from(range.triangle_count) > triangle_budget { break; }
                    triangle_budget -= u64::from(range.triangle_count);
                    staged.push(Arc::new(pack_geometry(device, &mut encoder, pipelines, scene, range)?));
                }
                let entries: Vec<_> = staged.iter().map(|mesh| wgpu::BlasBuildEntry {
                    blas: &mesh.blas,
                    geometry: wgpu::BlasGeometries::TriangleGeometries(vec![wgpu::BlasTriangleGeometry {
                        size: &mesh.size, vertex_buffer: &scene.vertices, first_vertex: 0,
                        vertex_stride: std::mem::size_of::<PtVertex>() as u64,
                        index_buffer: Some(&mesh.indices), first_index: Some(0),
                        transform_buffer: None, transform_buffer_offset: None,
                    }]),
                }).collect();
                let built_count = self.meshes.len() + staged.len();
                if ready.is_none() && built_count == scene.layout.ranges.len()
                    && scene.layout.placements.len() <= budget.max_tlas_instances
                {
                    ready = Some(Arc::new(build_tlas(device, pipelines, &scene.layout, &self.meshes, &staged)));
                    encoder.build_acceleration_structures(entries.iter(), [ &ready.as_ref().expect("TLAS was created").package ]);
                } else if !entries.is_empty() {
                    encoder.build_acceleration_structures(entries.iter(), std::iter::empty());
                }
                status = if ready.is_some() { NativeRtStatus::Ready } else {
                    NativeRtStatus::Building {
                        remaining_blas: scene.layout.ranges.len() - built_count,
                        next_blas_triangles: scene.layout.ranges.get(built_count).map_or(0, |range| range.triangle_count),
                        tlas_instances: scene.layout.placements.len(),
                    }
                };
            }
        }
        Ok(NativeRtFrame {
            encoder, owner: self.owner.clone(), revision: self.revision, base_mesh_count: self.meshes.len(),
            staged, ready, status,
            frame_id: self.next_frame.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| next.checked_add(1)).expect("native RT frame ID overflow"),
        })
    }

    pub fn commit(&mut self, submitted: NativeRtSubmitted) -> bool {
        if !Arc::ptr_eq(&self.owner, &submitted.owner) || self.revision != submitted.revision
            || self.meshes.len() != submitted.base_mesh_count
            || submitted.frame_id <= self.last_committed_frame
        { return false; }
        self.meshes.extend(submitted.staged);
        self.ready = submitted.ready;
        self.last_committed_frame = submitted.frame_id;
        true
    }
}

pub struct NativeRtFrame {
    frame_id: u64,
    encoder: wgpu::CommandEncoder,
    owner: Arc<()>,
    revision: u64,
    base_mesh_count: usize,
    staged: Vec<Arc<CachedBlas>>,
    ready: Option<Arc<ReadyScene>>,
    status: NativeRtStatus,
}

impl NativeRtFrame {
    pub fn status(&self) -> NativeRtStatus { self.status }

    pub fn encoder(&self) -> &wgpu::CommandEncoder { &self.encoder }

    pub fn encoder_mut(&mut self) -> &mut wgpu::CommandEncoder { &mut self.encoder }

    pub fn dispatch_lease(&self) -> Option<NativeRtDispatch> {
        self.ready.as_ref().map(|ready| NativeRtDispatch { ready: ready.clone() })
    }

    pub fn encode_pt(
        &mut self,
        pipelines: &NativeRtPipelines,
        canonical_material_bind_group: &wgpu::BindGroup,
        extent: [u32; 2],
        interactive: bool,
    ) -> Result<(), NativeRtError> {
        let dispatch = self.dispatch_lease().ok_or(NativeRtError("native RT scene is not ready for dispatch"))?;
        dispatch.encode(&mut self.encoder, pipelines, canonical_material_bind_group, extent, interactive)
    }

    pub fn submit(self, queue: &wgpu::Queue) -> NativeRtSubmitted {
        let submission_index = queue.submit([self.encoder.finish()]);
        NativeRtSubmitted {
            frame_id: self.frame_id,
            owner: self.owner, revision: self.revision, base_mesh_count: self.base_mesh_count,
            staged: self.staged, ready: self.ready, submission_index,
        }
    }
}

pub struct NativeRtDispatch {
    ready: Arc<ReadyScene>,
}

impl NativeRtDispatch {
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipelines: &NativeRtPipelines,
        canonical_material_bind_group: &wgpu::BindGroup,
        extent: [u32; 2],
        interactive: bool,
    ) -> Result<(), NativeRtError> {
        if extent.contains(&0) { return Err(NativeRtError("native RT dispatch extent must be nonzero")); }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Native RT canonical PT"), timestamp_writes: None,
        });
        pass.set_pipeline(if interactive { &pipelines.interactive } else { &pipelines.full });
        pass.set_bind_group(0, canonical_material_bind_group, &[]);
        pass.set_bind_group(1, &self.ready.binding, &[]);
        pass.dispatch_workgroups(extent[0].div_ceil(8), extent[1].div_ceil(8), 1);
        Ok(())
    }
}

pub struct NativeRtSubmitted {
    frame_id: u64,
    owner: Arc<()>,
    revision: u64,
    base_mesh_count: usize,
    staged: Vec<Arc<CachedBlas>>,
    ready: Option<Arc<ReadyScene>>,
    pub submission_index: wgpu::SubmissionIndex,
}

fn pack_geometry(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &NativeRtPipelines,
    scene: &SceneInputs,
    range: &GeometryRange,
) -> Result<CachedBlas, NativeRtError> {
    let byte_count = u64::from(range.triangle_count) * 12;
    let limits = device.limits();
    if byte_count > u64::from(limits.max_storage_buffer_binding_size)
        || scene.triangles.size() > u64::from(limits.max_storage_buffer_binding_size)
    { return Err(NativeRtError("native RT index pack exceeds storage binding limits")); }
    let indices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Native RT packed indices"), size: byte_count,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::BLAS_INPUT, mapped_at_creation: false,
    });
    let groups = range.triangle_count.div_ceil(128);
    let groups_x = groups.min(limits.max_compute_workgroups_per_dimension);
    let groups_y = groups.div_ceil(groups_x);
    if groups_y > limits.max_compute_workgroups_per_dimension {
        return Err(NativeRtError("native RT index pack exceeds dispatch limits"));
    }
    let parameters = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Native RT index pack range"),
        contents: bytemuck::cast_slice(&[range.first_triangle, range.triangle_count, groups_x * 128, 0u32]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Native RT index pack binding"), layout: &pipelines.pack_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: scene.triangles.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: indices.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: parameters.as_entire_binding() },
        ],
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Native RT triangle index pack"), timestamp_writes: None,
        });
        pass.set_pipeline(&pipelines.pack);
        pass.set_bind_group(0, &binding, &[]);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    let size = wgpu::BlasTriangleGeometrySizeDescriptor {
        vertex_format: wgpu::VertexFormat::Float32x3, vertex_count: scene.vertex_count,
        index_format: Some(wgpu::IndexFormat::Uint32), index_count: Some(range.triangle_count * 3),
        flags: wgpu::AccelerationStructureGeometryFlags::OPAQUE,
    };
    let blas = device.create_blas(&wgpu::CreateBlasDescriptor {
        label: Some("Native RT triangle BLAS"), flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
        update_mode: wgpu::AccelerationStructureUpdateMode::Build,
    }, wgpu::BlasGeometrySizeDescriptors::Triangles { descriptors: vec![size.clone()] });
    Ok(CachedBlas { blas, indices, size })
}

fn build_tlas(
    device: &wgpu::Device,
    pipelines: &NativeRtPipelines,
    layout: &SceneLayout,
    meshes: &[Arc<CachedBlas>],
    staged: &[Arc<CachedBlas>],
) -> ReadyScene {
    let instances = layout.placements.iter().enumerate().map(|(index, (transform, geometry))| {
        let mesh = if *geometry < meshes.len() { &meshes[*geometry] } else { &staged[*geometry - meshes.len()] };
        Some(wgpu::TlasInstance::new(&mesh.blas, *transform, index as u32, 0xff))
    }).collect();
    let tlas = device.create_tlas(&wgpu::CreateTlasDescriptor {
        label: Some("Native RT instance TLAS"), max_instances: layout.placements.len() as u32,
        flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
        update_mode: wgpu::AccelerationStructureUpdateMode::Build,
    });
    let package = wgpu::TlasPackage::new_with_instances(tlas, instances);
    let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Native RT scene binding"), layout: &pipelines.acceleration_layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: package.as_binding() }],
    });
    ReadyScene { package, binding }
}

#[cfg(test)]
mod tests;
