//! Bind-group rebuilds for fog / PT / RT-shadow / PT denoise.

use eframe::wgpu;
use crate::GpuCtx;

use super::pipelines_lazy::{ensure_pt_env_cdf_buffer, ensure_pt_stats_buffers};


/// Rebuild fog bind group after light-buffer realloc (or any fog_bg invalidation) without a full
/// G-buffer resize. Requires fog textures + gbuf_normal already allocated.
pub(crate) fn rebuild_fog_bg(gpu: &mut GpuCtx) -> bool {
    let (Some(gn), Some(fog), Some(fog_prev)) =
        (&gpu.gbuf_normal, &gpu.fog_texture, &gpu.fog_prev_texture)
    else {
        return false;
    };
    let gn_view = gn.create_view(&wgpu::TextureViewDescriptor::default());
    let fog_view = fog.create_view(&wgpu::TextureViewDescriptor::default());
    let fog_prev_view = fog_prev.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fog_bg"),
        layout: &gpu.bind_groups.fog_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 1,
                resource: gpu.buffers.uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: gpu.gpu_volume.gpu_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: gpu.buffers.prim_buffer.as_entire_binding(),
            },
            // T1 height_maps — live in the fog module via scene_sdf -> eval_prim.
            wgpu::BindGroupEntry {
                binding: 15,
                resource: gpu
                    .buffers
                    .height_map_buffer
                    .as_ref()
                    .unwrap_or(&gpu.buffers.brick_pool_dummy_buf)
                    .as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 12,
                resource: wgpu::BindingResource::TextureView(&gn_view),
            },
            wgpu::BindGroupEntry {
                binding: 17,
                resource: wgpu::BindingResource::TextureView(&fog_view),
            },
            wgpu::BindGroupEntry {
                binding: 24,
                resource: wgpu::BindingResource::TextureView(&fog_prev_view),
            },
            wgpu::BindGroupEntry {
                binding: 25,
                resource: gpu.buffers.fog_prev_camera_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 26,
                resource: gpu.buffers.light_buffer.as_entire_binding(),
            },
        ],
    });
    gpu.bind_groups.fog_bg = Some(bg);
    true
}


pub(crate) fn build_pt_bg(gpu: &mut GpuCtx) -> bool {
    ensure_pt_stats_buffers(gpu);
    ensure_pt_env_cdf_buffer(gpu);
    let (Some(hdr), Some(accum), Some(restir_prev), Some(restir_out), Some(gi_prev), Some(gi_out), Some(gbuf_n), Some(gbuf_a)) = (
        &gpu.hdr_texture, &gpu.pt_accum_texture, &gpu.pt_restir_prev, &gpu.pt_restir_out,
        &gpu.pt_gi_prev, &gpu.pt_gi_out, &gpu.gbuf_normal, &gpu.gbuf_albedo,
    ) else { return false };
    let b = &gpu.buffers;
    let (Some(tlas), Some(inst), Some(blas), Some(tris), Some(verts), Some(meta), Some(mats), Some(stats), Some(env_cdf)) = (
        &b.pt_tlas_buffer, &b.pt_instance_buffer, &b.pt_blas_buffer,
        &b.pt_tri_buffer, &b.pt_vert_buffer, &b.pt_meta_buffer, &b.pt_material_buffer,
        &b.pt_stats_buffer, &b.pt_env_cdf_buffer,
    ) else { return false };
    let hdr_view = hdr.create_view(&wgpu::TextureViewDescriptor::default());
    let accum_view = accum.create_view(&wgpu::TextureViewDescriptor::default());
    let (Some(accum_out), Some(moments)) = (&gpu.pt_accum_output, &gpu.buffers.pt_moments_buffer) else { return false };
    let accum_out_view = accum_out.create_view(&wgpu::TextureViewDescriptor::default());
    let restir_prev_view = restir_prev.create_view(&wgpu::TextureViewDescriptor::default());
    let restir_out_view = restir_out.create_view(&wgpu::TextureViewDescriptor::default());
    let gi_prev_view = gi_prev.create_view(&wgpu::TextureViewDescriptor::default());
    let gi_out_view = gi_out.create_view(&wgpu::TextureViewDescriptor::default());
    let gbuf_n_view = gbuf_n.create_view(&wgpu::TextureViewDescriptor::default());
    let gbuf_a_view = gbuf_a.create_view(&wgpu::TextureViewDescriptor::default());
    let vol_view = gpu.gpu_volume.read_texture.create_view(&wgpu::TextureViewDescriptor::default());
    // Borrow mat-tex view/sampler (references only — TextureView/Sampler are not Clone).
    let mat_tex_view = gpu.material_textures.view();
    let mat_tex_samp = gpu.material_textures.sampler();
    let mat_tex_gen = gpu.material_textures.generation;
    let scene_gen = gpu.buffers.pt_scene_generation;
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor{
        label: Some("pt_bg"), layout: &gpu.bind_groups.pt_bgl, entries: &[
            wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&hdr_view)},
            wgpu::BindGroupEntry{binding:1,resource:b.uniform_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:2,resource:gpu.gpu_volume.gpu_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:3,resource:b.prim_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:4,resource:b.tex_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:21,resource:b.env_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:22,resource:b.tex_atlas_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:23,resource:b.tex_meta_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:26,resource:b.light_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:27,resource:wgpu::BindingResource::TextureView(&accum_view)},
            wgpu::BindGroupEntry{binding:60,resource:wgpu::BindingResource::TextureView(&accum_out_view)},
            wgpu::BindGroupEntry{binding:61,resource:moments.as_entire_binding()},
            wgpu::BindGroupEntry{binding:40,resource:wgpu::BindingResource::TextureView(&vol_view)},
            wgpu::BindGroupEntry{binding:41,resource:wgpu::BindingResource::Sampler(&gpu.gpu_volume.sampler)},
            wgpu::BindGroupEntry{binding:42,resource:tlas.as_entire_binding()},
            wgpu::BindGroupEntry{binding:43,resource:inst.as_entire_binding()},
            wgpu::BindGroupEntry{binding:44,resource:blas.as_entire_binding()},
            wgpu::BindGroupEntry{binding:45,resource:tris.as_entire_binding()},
            wgpu::BindGroupEntry{binding:46,resource:verts.as_entire_binding()},
            wgpu::BindGroupEntry{binding:47,resource:meta.as_entire_binding()},
            wgpu::BindGroupEntry{binding:48,resource:mats.as_entire_binding()},
            wgpu::BindGroupEntry{binding:49,resource:wgpu::BindingResource::TextureView(&mat_tex_view)},
            wgpu::BindGroupEntry{binding:50,resource:wgpu::BindingResource::Sampler(&mat_tex_samp)},
            wgpu::BindGroupEntry{binding:59,resource:wgpu::BindingResource::TextureView(gpu.material_textures.linear_view())},
            wgpu::BindGroupEntry{binding:51,resource:stats.as_entire_binding()},
            wgpu::BindGroupEntry{binding:52,resource:wgpu::BindingResource::TextureView(&restir_prev_view)},
            wgpu::BindGroupEntry{binding:53,resource:wgpu::BindingResource::TextureView(&restir_out_view)},
            wgpu::BindGroupEntry{binding:54,resource:env_cdf.as_entire_binding()},
            wgpu::BindGroupEntry{binding:55,resource:wgpu::BindingResource::TextureView(&gi_prev_view)},
            wgpu::BindGroupEntry{binding:56,resource:wgpu::BindingResource::TextureView(&gi_out_view)},
            wgpu::BindGroupEntry{binding:57,resource:wgpu::BindingResource::TextureView(&gbuf_n_view)},
            wgpu::BindGroupEntry{binding:58,resource:wgpu::BindingResource::TextureView(&gbuf_a_view)},
        ]});
    gpu.bind_groups.pt_bg = Some(bg);
    gpu.bind_groups.pt_bg_generation = scene_gen;
    gpu.bind_groups.pt_bg_mat_tex_gen = mat_tex_gen;
    true
}


/// Half-res PT bind group (Section B): same layout as `pt_bgl`, but bindings 0/27
/// point at `pt_half_hdr` / `pt_half_accum` so the path tracer runs at 25% pixels.
/// Section G: bindings 52/53 → half ReSTIR reservoirs.
/// Section K: binding 54 → env CDF (shared full-res buffer).
pub(crate) fn build_pt_bg_half(gpu: &mut GpuCtx) -> bool {
    ensure_pt_stats_buffers(gpu);
    ensure_pt_env_cdf_buffer(gpu);
    let (Some(half_hdr), Some(half_accum), Some(restir_prev), Some(restir_out), Some(gi_prev), Some(gi_out), Some(gbuf_n), Some(gbuf_a)) = (
        &gpu.pt_half_hdr, &gpu.pt_half_accum, &gpu.pt_half_restir_prev, &gpu.pt_half_restir_out,
        &gpu.pt_half_gi_prev, &gpu.pt_half_gi_out, &gpu.gbuf_normal, &gpu.gbuf_albedo,
    ) else {
        return false;
    };
    let b = &gpu.buffers;
    let (Some(tlas), Some(inst), Some(blas), Some(tris), Some(verts), Some(meta), Some(mats), Some(stats), Some(env_cdf)) = (
        &b.pt_tlas_buffer, &b.pt_instance_buffer, &b.pt_blas_buffer,
        &b.pt_tri_buffer, &b.pt_vert_buffer, &b.pt_meta_buffer, &b.pt_material_buffer,
        &b.pt_stats_buffer, &b.pt_env_cdf_buffer,
    ) else { return false };
    let hdr_view = half_hdr.create_view(&wgpu::TextureViewDescriptor::default());
    let accum_view = half_accum.create_view(&wgpu::TextureViewDescriptor::default());
    let (Some(accum_out), Some(moments)) = (&gpu.pt_half_accum_output, &gpu.buffers.pt_half_moments_buffer) else { return false };
    let accum_out_view = accum_out.create_view(&wgpu::TextureViewDescriptor::default());
    let restir_prev_view = restir_prev.create_view(&wgpu::TextureViewDescriptor::default());
    let restir_out_view = restir_out.create_view(&wgpu::TextureViewDescriptor::default());
    let gi_prev_view = gi_prev.create_view(&wgpu::TextureViewDescriptor::default());
    let gi_out_view = gi_out.create_view(&wgpu::TextureViewDescriptor::default());
    let gbuf_n_view = gbuf_n.create_view(&wgpu::TextureViewDescriptor::default());
    let gbuf_a_view = gbuf_a.create_view(&wgpu::TextureViewDescriptor::default());
    let vol_view = gpu.gpu_volume.read_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mat_tex_view = gpu.material_textures.view();
    let mat_tex_samp = gpu.material_textures.sampler();
    let mat_tex_gen = gpu.material_textures.generation;
    let scene_gen = gpu.buffers.pt_scene_generation;
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor{
        label: Some("pt_bg_half"), layout: &gpu.bind_groups.pt_bgl, entries: &[
            wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&hdr_view)},
            wgpu::BindGroupEntry{binding:1,resource:b.uniform_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:2,resource:gpu.gpu_volume.gpu_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:3,resource:b.prim_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:4,resource:b.tex_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:21,resource:b.env_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:22,resource:b.tex_atlas_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:23,resource:b.tex_meta_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:26,resource:b.light_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:27,resource:wgpu::BindingResource::TextureView(&accum_view)},
            wgpu::BindGroupEntry{binding:60,resource:wgpu::BindingResource::TextureView(&accum_out_view)},
            wgpu::BindGroupEntry{binding:61,resource:moments.as_entire_binding()},
            wgpu::BindGroupEntry{binding:40,resource:wgpu::BindingResource::TextureView(&vol_view)},
            wgpu::BindGroupEntry{binding:41,resource:wgpu::BindingResource::Sampler(&gpu.gpu_volume.sampler)},
            wgpu::BindGroupEntry{binding:42,resource:tlas.as_entire_binding()},
            wgpu::BindGroupEntry{binding:43,resource:inst.as_entire_binding()},
            wgpu::BindGroupEntry{binding:44,resource:blas.as_entire_binding()},
            wgpu::BindGroupEntry{binding:45,resource:tris.as_entire_binding()},
            wgpu::BindGroupEntry{binding:46,resource:verts.as_entire_binding()},
            wgpu::BindGroupEntry{binding:47,resource:meta.as_entire_binding()},
            wgpu::BindGroupEntry{binding:48,resource:mats.as_entire_binding()},
            wgpu::BindGroupEntry{binding:49,resource:wgpu::BindingResource::TextureView(&mat_tex_view)},
            wgpu::BindGroupEntry{binding:50,resource:wgpu::BindingResource::Sampler(&mat_tex_samp)},
            wgpu::BindGroupEntry{binding:59,resource:wgpu::BindingResource::TextureView(gpu.material_textures.linear_view())},
            wgpu::BindGroupEntry{binding:51,resource:stats.as_entire_binding()},
            wgpu::BindGroupEntry{binding:52,resource:wgpu::BindingResource::TextureView(&restir_prev_view)},
            wgpu::BindGroupEntry{binding:53,resource:wgpu::BindingResource::TextureView(&restir_out_view)},
            wgpu::BindGroupEntry{binding:54,resource:env_cdf.as_entire_binding()},
            wgpu::BindGroupEntry{binding:55,resource:wgpu::BindingResource::TextureView(&gi_prev_view)},
            wgpu::BindGroupEntry{binding:56,resource:wgpu::BindingResource::TextureView(&gi_out_view)},
            wgpu::BindGroupEntry{binding:57,resource:wgpu::BindingResource::TextureView(&gbuf_n_view)},
            wgpu::BindGroupEntry{binding:58,resource:wgpu::BindingResource::TextureView(&gbuf_a_view)},
        ]});
    gpu.bind_groups.pt_bg_half = Some(bg);
    gpu.bind_groups.pt_bg_half_generation = scene_gen;
    gpu.bind_groups.pt_bg_half_mat_tex_gen = mat_tex_gen;
    true
}


/// (Re)build the RT-shadow bind group. Requires the vis texture + gbuf_normal (created in the
/// viewport resize block) AND the PT scene SSBOs (created by update_pt_accel in poll — the
/// Stage-0 residency decouple keeps them live outside PathTrace). Returns false if any resource
/// is missing (the caller then skips the dispatch → deferred keeps the legacy soft_shadow this
/// frame). Binds only the BVH SUBSET (42-47) + gbuf_normal(12) + light(26) + vis output(0) +
/// config(1) — NOT materials(48), NOT accum(27).
pub(crate) fn build_rt_shadow_bg(gpu: &mut GpuCtx) -> bool {
    if gpu.bind_groups.rt_shadow_bgl.is_none() { return false; }
    if gpu.rt_shadow_vis.is_none() || gpu.rt_shadow_vis_hi.is_none() || gpu.gbuf_normal.is_none() {
        return false;
    }
    // Lazily create the RtShadowParams uniform (binding 25) — 16 B, rewritten each RT-shadow frame
    // in viewport_render with the live softness/samples/frame. Created here so the bind group can
    // always reference it (Stage 3).
    if gpu.buffers.rt_shadow_params_buffer.is_none() {
        gpu.buffers.rt_shadow_params_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt_shadow_params"),
            size: std::mem::size_of::<graphics::RtShadowParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
    let bgl = gpu.bind_groups.rt_shadow_bgl.as_ref().unwrap();
    let (vis, vis_hi, gbuf_normal) = (
        gpu.rt_shadow_vis.as_ref().unwrap(),
        gpu.rt_shadow_vis_hi.as_ref().unwrap(),
        gpu.gbuf_normal.as_ref().unwrap(),
    );
    let b = &gpu.buffers;
    let rt_params = b.rt_shadow_params_buffer.as_ref().unwrap();
    let (Some(tlas), Some(inst), Some(blas), Some(tris), Some(verts), Some(meta)) = (
        &b.pt_tlas_buffer, &b.pt_instance_buffer, &b.pt_blas_buffer,
        &b.pt_tri_buffer, &b.pt_vert_buffer, &b.pt_meta_buffer,
    ) else { return false };
    let vis_view = vis.create_view(&wgpu::TextureViewDescriptor::default());
    let vis_hi_view = vis_hi.create_view(&wgpu::TextureViewDescriptor::default());
    let normal_view = gbuf_normal.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rt_shadow_bg"), layout: bgl, entries: &[
            wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&vis_view)},
            wgpu::BindGroupEntry{binding:1,resource:b.uniform_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:12,resource:wgpu::BindingResource::TextureView(&normal_view)},
            wgpu::BindGroupEntry{binding:25,resource:rt_params.as_entire_binding()},
            wgpu::BindGroupEntry{binding:26,resource:b.light_buffer.as_entire_binding()},
            wgpu::BindGroupEntry{binding:28,resource:wgpu::BindingResource::TextureView(&vis_hi_view)},
            wgpu::BindGroupEntry{binding:42,resource:tlas.as_entire_binding()},
            wgpu::BindGroupEntry{binding:43,resource:inst.as_entire_binding()},
            wgpu::BindGroupEntry{binding:44,resource:blas.as_entire_binding()},
            wgpu::BindGroupEntry{binding:45,resource:tris.as_entire_binding()},
            wgpu::BindGroupEntry{binding:46,resource:verts.as_entire_binding()},
            wgpu::BindGroupEntry{binding:47,resource:meta.as_entire_binding()},
        ]});
    gpu.bind_groups.rt_shadow_bg = Some(bg);
    gpu.bind_groups.rt_shadow_bg_generation = gpu.buffers.pt_scene_generation;
    true
}


/// (Re)build the RT spatial-denoise bind group for pack0: A=vis, B=denoised.
pub(super) fn build_rt_denoise_bg(gpu: &mut GpuCtx) {
    let (Some(bgl), Some(a), Some(b), Some(gn), Some(pbuf)) = (
        gpu.bind_groups.rt_denoise_bgl.as_ref(), gpu.rt_shadow_vis.as_ref(),
        gpu.rt_shadow_denoised.as_ref(), gpu.gbuf_normal.as_ref(),
        gpu.buffers.rt_denoise_params_buffer.as_ref(),
    ) else { return };
    let a_view = a.create_view(&wgpu::TextureViewDescriptor::default());
    let b_view = b.create_view(&wgpu::TextureViewDescriptor::default());
    let gn_view = gn.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rt_denoise_bg"), layout: bgl, entries: &[
            wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&a_view)},
            wgpu::BindGroupEntry{binding:1,resource:wgpu::BindingResource::TextureView(&gn_view)},
            wgpu::BindGroupEntry{binding:2,resource:wgpu::BindingResource::TextureView(&b_view)},
            wgpu::BindGroupEntry{binding:3,resource:pbuf.as_entire_binding()},
        ]});
    gpu.bind_groups.rt_denoise_bg = Some(bg);
}


/// P3.2: spatial denoise bind group for pack1 (vis_hi → denoised_hi).
pub(super) fn build_rt_denoise_bg_hi(gpu: &mut GpuCtx) {
    let (Some(bgl), Some(a), Some(b), Some(gn), Some(pbuf)) = (
        gpu.bind_groups.rt_denoise_bgl.as_ref(), gpu.rt_shadow_vis_hi.as_ref(),
        gpu.rt_shadow_denoised_hi.as_ref(), gpu.gbuf_normal.as_ref(),
        gpu.buffers.rt_denoise_params_buffer.as_ref(),
    ) else { return };
    let a_view = a.create_view(&wgpu::TextureViewDescriptor::default());
    let b_view = b.create_view(&wgpu::TextureViewDescriptor::default());
    let gn_view = gn.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rt_denoise_bg_hi"), layout: bgl, entries: &[
            wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&a_view)},
            wgpu::BindGroupEntry{binding:1,resource:wgpu::BindingResource::TextureView(&gn_view)},
            wgpu::BindGroupEntry{binding:2,resource:wgpu::BindingResource::TextureView(&b_view)},
            wgpu::BindGroupEntry{binding:3,resource:pbuf.as_entire_binding()},
        ]});
    gpu.bind_groups.rt_denoise_bg_hi = Some(bg);
}


/// (Re)build the RT temporal-reprojection bind group for pack0.
pub(super) fn build_rt_temporal_bg(gpu: &mut GpuCtx) {
    let (Some(bgl), Some(a), Some(b), Some(c), Some(gn), Some(prev_nd), Some(pbuf)) = (
        gpu.bind_groups.rt_temporal_bgl.as_ref(), gpu.rt_shadow_vis.as_ref(),
        gpu.rt_shadow_denoised.as_ref(), gpu.rt_shadow_history.as_ref(),
        gpu.gbuf_normal.as_ref(), gpu.temporal_prev_normal_dist.as_ref(),
        gpu.buffers.rt_temporal_params_buffer.as_ref(),
    ) else { return };
    let out_view = a.create_view(&wgpu::TextureViewDescriptor::default());
    let cur_vis_view = b.create_view(&wgpu::TextureViewDescriptor::default());
    let hist_view = c.create_view(&wgpu::TextureViewDescriptor::default());
    let gn_view = gn.create_view(&wgpu::TextureViewDescriptor::default());
    let prev_view = prev_nd.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rt_temporal_bg"), layout: bgl, entries: &[
            wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&gn_view)},
            wgpu::BindGroupEntry{binding:1,resource:pbuf.as_entire_binding()},
            wgpu::BindGroupEntry{binding:2,resource:wgpu::BindingResource::TextureView(&prev_view)},
            wgpu::BindGroupEntry{binding:3,resource:wgpu::BindingResource::TextureView(&hist_view)},
            wgpu::BindGroupEntry{binding:4,resource:wgpu::BindingResource::TextureView(&cur_vis_view)},
            wgpu::BindGroupEntry{binding:5,resource:wgpu::BindingResource::TextureView(&out_view)},
        ]});
    gpu.bind_groups.rt_temporal_bg = Some(bg);
}


/// P3.2: temporal bind group for pack1.
pub(super) fn build_rt_temporal_bg_hi(gpu: &mut GpuCtx) {
    let (Some(bgl), Some(a), Some(b), Some(c), Some(gn), Some(prev_nd), Some(pbuf)) = (
        gpu.bind_groups.rt_temporal_bgl.as_ref(), gpu.rt_shadow_vis_hi.as_ref(),
        gpu.rt_shadow_denoised_hi.as_ref(), gpu.rt_shadow_history_hi.as_ref(),
        gpu.gbuf_normal.as_ref(), gpu.temporal_prev_normal_dist.as_ref(),
        gpu.buffers.rt_temporal_params_buffer.as_ref(),
    ) else { return };
    let out_view = a.create_view(&wgpu::TextureViewDescriptor::default());
    let cur_vis_view = b.create_view(&wgpu::TextureViewDescriptor::default());
    let hist_view = c.create_view(&wgpu::TextureViewDescriptor::default());
    let gn_view = gn.create_view(&wgpu::TextureViewDescriptor::default());
    let prev_view = prev_nd.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rt_temporal_bg_hi"), layout: bgl, entries: &[
            wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&gn_view)},
            wgpu::BindGroupEntry{binding:1,resource:pbuf.as_entire_binding()},
            wgpu::BindGroupEntry{binding:2,resource:wgpu::BindingResource::TextureView(&prev_view)},
            wgpu::BindGroupEntry{binding:3,resource:wgpu::BindingResource::TextureView(&hist_view)},
            wgpu::BindGroupEntry{binding:4,resource:wgpu::BindingResource::TextureView(&cur_vis_view)},
            wgpu::BindGroupEntry{binding:5,resource:wgpu::BindingResource::TextureView(&out_view)},
        ]});
    gpu.bind_groups.rt_temporal_bg_hi = Some(bg);
}


/// Clear all cached PT denoise bind groups (full + half, all pass topologies).
/// Call when any of src/temp/dst/gbuf textures are recreated.
pub(crate) fn invalidate_pt_denoise_bgs(gpu: &mut GpuCtx) {
    gpu.bind_groups.pt_denoise_bg = None;
    gpu.bind_groups.pt_denoise_bg_p0_temp = None;
    gpu.bind_groups.pt_denoise_bg_ping = None;
    gpu.bind_groups.pt_denoise_bg_pong = None;
    gpu.bind_groups.pt_denoise_bg_half = None;
    gpu.bind_groups.pt_denoise_bg_half_p0_temp = None;
    gpu.bind_groups.pt_denoise_bg_half_ping = None;
    gpu.bind_groups.pt_denoise_bg_half_pong = None;
}


pub(super) fn make_pt_denoise_bg(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    label: &str,
    src: &wgpu::Texture,
    dst: &wgpu::Texture,
    gbuf_normal: &wgpu::Texture,
    gbuf_albedo: &wgpu::Texture,
    params_buf: &wgpu::Buffer,
    moments_buf: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let src_v = src.create_view(&wgpu::TextureViewDescriptor::default());
    let dst_v = dst.create_view(&wgpu::TextureViewDescriptor::default());
    let gn_v = gbuf_normal.create_view(&wgpu::TextureViewDescriptor::default());
    let alb_v = gbuf_albedo.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&src_v),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&gn_v),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&dst_v),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&alb_v),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: moments_buf.as_entire_binding(),
            },
        ],
    })
}
