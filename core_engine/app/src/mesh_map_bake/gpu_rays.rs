//! GPU any-hit for AO / Thickness / Bent (G7). Same BLAS + hemisphere as CPU.
//!
//! Owns a thread-local wgpu device so the bake worker stays off the frame GPU.
//! Missing adapter → `None` and the job falls back to CPU.

use super::maps;
use super::raster::UvGeoBuffer;
use super::source::PolyCurvSource;
use crate::bake_diag::BakeNotes;
use bytemuck::{Pod, Zeroable};
use core_math::bvh::TriBlas;
use eframe::wgpu;
use glam::Vec3;
use std::cell::RefCell;


const WGSL: &str = include_str!("gpu_rays.wgsl");

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    res: u32,
    rays: u32,
    mode: u32,
    node_count: u32,
    vert_count: u32,
    idx_count: u32,
    order_count: u32,
    texel_count: u32,
    bias: f32,
    ray_len: f32,
    _p0: f32,
    _p1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuTexel {
    pos: [f32; 4],
    nrm: [f32; 4],
    tan: [f32; 4],
    bit: [f32; 4],
    low_n: [f32; 4],
}

struct GpuRayState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
}

thread_local! {
    static STATE: RefCell<Option<GpuRayState>> = const { RefCell::new(None) };
}

pub(crate) fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
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
            label: Some("meshmap_gpu_rays"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()
}

fn ensure_state() -> bool {
    STATE.with(|slot| {
        if slot.borrow().is_some() {
            return true;
        }
        let Some((device, queue)) = try_device() else {
            return false;
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("meshmap_gpu_rays"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("meshmap_gpu_rays_bgl"),
            entries: &[
                storage_entry(0, wgpu::BufferBindingType::Uniform),
                storage_entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                storage_entry(2, wgpu::BufferBindingType::Storage { read_only: true }),
                storage_entry(3, wgpu::BufferBindingType::Storage { read_only: true }),
                storage_entry(4, wgpu::BufferBindingType::Storage { read_only: true }),
                storage_entry(5, wgpu::BufferBindingType::Storage { read_only: true }),
                storage_entry(6, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("meshmap_gpu_rays_pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("meshmap_gpu_rays_pipe"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("bake_rays"),
            compilation_options: Default::default(),
            cache: None,
        });
        *slot.borrow_mut() = Some(GpuRayState {
            device,
            queue,
            pipeline,
            bgl,
        });
        true
    })
}

fn storage_entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// AO (`inward=false`) / Thickness (`inward=true`) on GPU. `None` → use CPU.
pub(crate) fn bake_occlusion(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    inward: bool,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
) -> Option<Vec<u8>> {
    let floats = dispatch(src, buf, if inward { 1 } else { 0 }, rays, notes, high)?;
    Some(pack_grey(&floats))
}

/// Bent normal (TS) on GPU. `None` → use CPU.
pub(crate) fn bake_bent(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
) -> Option<Vec<u8>> {
    let floats = dispatch(src, buf, 2, rays, notes, high)?;
    let n = (buf.res * buf.res) as usize;
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        let o = i * 4;
        if floats[o + 3] <= 0.0 {
            continue;
        }
        let ts = Vec3::new(floats[o], floats[o + 1], floats[o + 2]);
        let rgb = core_math::mikkt::encode_tangent_normal(ts);
        px[o] = rgb[0];
        px[o + 1] = rgb[1];
        px[o + 2] = rgb[2];
        px[o + 3] = 255;
    }
    Some(px)
}

/// Bent 16-bit dual-plane from GPU TS floats.
pub(crate) fn bake_bent_16(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
) -> Option<(Vec<u8>, Vec<u8>)> {
    use crate::material_textures::encode_normal_rgba16_planes;
    let floats = dispatch(src, buf, 2, rays, notes, high)?;
    let n = (buf.res * buf.res) as usize;
    let mut hi = vec![0u8; n * 4];
    let mut lo = vec![0u8; n * 4];
    for i in 0..n {
        let o = i * 4;
        if floats[o + 3] <= 0.0 {
            continue;
        }
        let (h, l) = encode_normal_rgba16_planes([floats[o], floats[o + 1], floats[o + 2]]);
        hi[o..o + 4].copy_from_slice(&h);
        lo[o..o + 4].copy_from_slice(&l);
    }
    Some((hi, lo))
}

fn pack_grey(floats: &[f32]) -> Vec<u8> {
    let n = floats.len() / 4;
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        let o = i * 4;
        if floats[o + 3] <= 0.0 {
            continue;
        }
        let b = (floats[o].clamp(0.0, 1.0) * 255.0).round() as u8;
        px[o] = b;
        px[o + 1] = b;
        px[o + 2] = b;
        px[o + 3] = 255;
    }
    px
}

fn dispatch(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    mode: u32,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
) -> Option<Vec<f32>> {
    if !ensure_state() {
        return None;
    }
    let (positions, indices, blas) = maps::occlusion_scene(src, high, notes)?;
    if blas.is_empty() || positions.is_empty() || indices.len() < 3 {
        return None;
    }
    let rays = rays.clamp(8, 512) as u32;
    let (lo, hi) = buf.bounds;
    let diag = (hi - lo).length().max(1.0e-4);
    let bias = diag * 1.0e-3;
    let ray_len = if mode == 1 { diag * 0.5 } else { diag * 0.75 };
    let n = (buf.res * buf.res) as usize;
    let mut texels = vec![GpuTexel {
        pos: [0.0; 4],
        nrm: [0.0; 4],
        tan: [0.0; 4],
        bit: [0.0; 4],
        low_n: [0.0; 4],
    }; n.max(1)];
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let p = buf.high_position[i];
        let hn = buf.high_normal[i];
        texels[i] = GpuTexel {
            pos: [p.x, p.y, p.z, buf.coverage[i]],
            nrm: [hn.x, hn.y, hn.z, 0.0],
            tan: [
                buf.low_tangent[i].x,
                buf.low_tangent[i].y,
                buf.low_tangent[i].z,
                0.0,
            ],
            bit: [
                buf.low_bitangent[i].x,
                buf.low_bitangent[i].y,
                buf.low_bitangent[i].z,
                0.0,
            ],
            low_n: [buf.normal[i].x, buf.normal[i].y, buf.normal[i].z, 0.0],
        };
    }
    let verts: Vec<[f32; 4]> = positions
        .iter()
        .map(|p| [p[0], p[1], p[2], 0.0])
        .collect();
    let params = Params {
        res: buf.res,
        rays,
        mode,
        node_count: blas.nodes.len() as u32,
        vert_count: verts.len() as u32,
        idx_count: indices.len() as u32,
        order_count: blas.tri_order.len() as u32,
        texel_count: n as u32,
        bias,
        ray_len,
        _p0: 0.0,
        _p1: 0.0,
    };
    STATE.with(|slot| {
        let st = slot.borrow();
        let st = st.as_ref()?;
        run_dispatch(st, &params, &blas, &verts, &indices, &texels, n)
    })
}

fn run_dispatch(
    st: &GpuRayState,
    params: &Params,
    blas: &TriBlas,
    verts: &[[f32; 4]],
    indices: &[u32],
    texels: &[GpuTexel],
    n: usize,
) -> Option<Vec<f32>> {
    let device = &st.device;
    let queue = &st.queue;
    let ub = upload(device, queue, bytemuck::bytes_of(params), wgpu::BufferUsages::UNIFORM);
    let nodes = upload(
        device,
        queue,
        bytemuck::cast_slice(&blas.nodes),
        wgpu::BufferUsages::STORAGE,
    );
    let order = upload(
        device,
        queue,
        bytemuck::cast_slice(if blas.tri_order.is_empty() {
            &[0u32][..]
        } else {
            &blas.tri_order
        }),
        wgpu::BufferUsages::STORAGE,
    );
    let vbuf = upload(device, queue, bytemuck::cast_slice(verts), wgpu::BufferUsages::STORAGE);
    let ibuf = upload(
        device,
        queue,
        bytemuck::cast_slice(if indices.is_empty() {
            &[0u32][..]
        } else {
            indices
        }),
        wgpu::BufferUsages::STORAGE,
    );
    let tbuf = upload(device, queue, bytemuck::cast_slice(texels), wgpu::BufferUsages::STORAGE);
    let out_bytes = (n.max(1) * 16) as u64;
    let out = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("meshmap_gpu_out"),
        size: out_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("meshmap_gpu_rays_bg"),
        layout: &st.bgl,
        entries: &[
            bind(0, &ub),
            bind(1, &nodes),
            bind(2, &order),
            bind(3, &vbuf),
            bind(4, &ibuf),
            bind(5, &tbuf),
            bind(6, &out),
        ],
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("meshmap_gpu_rays_enc"),
    });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("meshmap_gpu_rays_pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&st.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        let gx = (params.res + 7) / 8;
        let gy = (params.res + 7) / 8;
        pass.dispatch_workgroups(gx.max(1), gy.max(1), 1);
    }
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("meshmap_gpu_read"),
        size: out_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    enc.copy_buffer_to_buffer(&out, 0, &staging, 0, out_bytes);
    queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().ok()?.ok()?;
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
    drop(data);
    staging.unmap();
    Some(out)
}

fn upload(device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    let size = bytes.len().max(4) as u64;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, bytes);
    buf
}

fn bind(binding: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buf.as_entire_binding(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_rays_wgsl_parses() {
        naga::front::wgsl::parse_str(WGSL).expect("gpu_rays.wgsl");
    }
}
