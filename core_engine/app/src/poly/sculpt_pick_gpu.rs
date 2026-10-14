// PR-4b — the 8×8 GPU viewport-crop pick (ZB_RESPONSIVE_SCULPT_PLAN §7.1 steps 1–8).
//
// Draws the EXISTING working-mesh `GpuMesh` (the same VB/IB the G-buffer draws for that
// cid, same instance transform) into a dedicated 8×8 R32Float+depth target and reads the
// winning texel back. Own pipeline, own textures, OWN staging (`picking_staging` is
// banned — mapped-buffer reuse is a device-loss crash class).
//
// **Deviation from K13's `set_viewport` wording, documented:** wgpu validates that a
// viewport lies WITHIN the render target, so a 1920×1080 viewport on an 8×8 texture is a
// validation error — GL-style out-of-bounds viewports don't exist here. The crop is
// instead a CROP MATRIX on clip coordinates: `clip' = C · view_proj · model · v`, where C
// scales NDC by (vp_w/8, vp_h/8) and recenters the cursor pixel at NDC 0. That is the
// SAME mapping `set_viewport(4-cx, 4-cy, vp_w, vp_h)` would produce — the full-viewport
// projection (true aspect, K13's actual requirement) is preserved exactly; only the
// stored window changes. Depth stays standard 0..1, compare Less, never reverse-Z.
//
// Occlusion: this pass draws ONLY the active body. Other objects do not occlude —
// stated product edge (§7.1 step 6), not a bug.

use eframe::wgpu;
use glam::{Mat4, Vec3, Vec4};

pub(crate) const TILE: u32 = 8;
/// wgpu::COPY_BYTES_PER_ROW_ALIGNMENT — 8 texels × 4 B = 32 rounds up to 256.
const ROW_BYTES: u32 = 256;

const SHADER: &str = r#"
struct Cam { mvp: mat4x4<f32> };
@group(0) @binding(0) var<uniform> cam: Cam;

@vertex
fn vs(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return cam.mvp * vec4<f32>(pos, 1.0);
}

// @builtin(position) in the fragment stage carries the viewport-transformed depth in .z
// (standard 0..1). The clear is 2.0, so any texel < 1.5 is a real hit.
@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) f32 {
    return p.z;
}
"#;

pub(crate) struct SculptPickGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    cam_buf: wgpu::Buffer,
    color: wgpu::TextureView,
    color_tex: wgpu::Texture,
    depth: wgpu::TextureView,
    staging: wgpu::Buffer,
}

impl SculptPickGpu {
    /// Create once (poll phase — Frame Ordering Contract; never mid-input).
    pub(crate) fn create(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sculpt_pick_crop"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let cam_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sculpt_pick_cam"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sculpt_pick_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sculpt_pick_bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sculpt_pick_pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sculpt_pick_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    // RasterVertex is [f32; 8]: pos(3) + nrm(3) + uv(2). Only pos feeds the pick.
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
                }],
            },
            primitive: wgpu::PrimitiveState {
                // No culling: parity with `ray_face_hit`, which tests both windings.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let mk_tex = |format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("sculpt_pick_tile"),
                size: wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color_tex = mk_tex(
            wgpu::TextureFormat::R32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let color = color_tex.create_view(&Default::default());
        let depth = mk_tex(
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sculpt_pick_staging"),
            size: (ROW_BYTES * TILE) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bind_group,
            cam_buf,
            color,
            color_tex,
            depth,
            staging,
        }
    }

    /// The core pick. `cursor_rel` in viewport pixels (y-down, relative to the viewport
    /// origin), `vp` the viewport size in pixels. Blocking (~sub-ms at 8×8): submit,
    /// map (Result via channel — GPU Safety rule), unproject the winning texel.
    /// Returns the WORLD-space hit.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pick(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vb: &wgpu::Buffer,
        ib: &wgpu::Buffer,
        index_count: u32,
        index_format: wgpu::IndexFormat,
        view_proj: Mat4,
        model: Mat4,
        cursor_rel: (f32, f32),
        vp: (f32, f32),
    ) -> Option<Vec3> {
        if index_count == 0 || vp.0 < 1.0 || vp.1 < 1.0 {
            return None;
        }
        // Crop matrix: cursor pixel → NDC center, window scaled to the 8×8 tile.
        let ndc_cx = 2.0 * cursor_rel.0 / vp.0 - 1.0;
        let ndc_cy = 1.0 - 2.0 * cursor_rel.1 / vp.1;
        let sx = vp.0 / TILE as f32;
        let sy = vp.1 / TILE as f32;
        let crop = Mat4::from_cols(
            Vec4::new(sx, 0.0, 0.0, 0.0),
            Vec4::new(0.0, sy, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(-ndc_cx * sx, -ndc_cy * sy, 0.0, 1.0),
        );
        let mvp = crop * view_proj * model;
        queue.write_buffer(&self.cam_buf, 0, bytemuck::cast_slice(&mvp.to_cols_array()));

        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sculpt_pick_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.color,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Clear 2.0 = "no hit" sentinel (real depths are ≤ 1.0).
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 2.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, vb.slice(..));
            pass.set_index_buffer(ib.slice(..), index_format);
            pass.draw_indexed(0..index_count, 0, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.color_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(ROW_BYTES),
                    rows_per_image: Some(TILE),
                },
            },
            wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        self.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let map_ok = matches!(rx.try_recv(), Ok(Ok(())));
        if !map_ok {
            self.staging.unmap();
            return None;
        }
        let mut tile = [[2.0f32; TILE as usize]; TILE as usize];
        {
            let data = self.staging.slice(..).get_mapped_range();
            for y in 0..TILE as usize {
                let row = &data[y * ROW_BYTES as usize..][..(TILE * 4) as usize];
                tile[y].copy_from_slice(bytemuck::cast_slice(row));
            }
        }
        self.staging.unmap();

        // Winning texel: valid depth closest to the tile center (the cursor pixel);
        // neighbours are the recovery ring for 1px silhouette holes (§7.1 step 3).
        let mut best: Option<(f32, u32, u32, f32)> = None; // (dist2, tx, ty, depth)
        for y in 0..TILE {
            for x in 0..TILE {
                let d = tile[y as usize][x as usize];
                if d < 1.5 {
                    let dx = x as f32 + 0.5 - TILE as f32 / 2.0;
                    let dy = y as f32 + 0.5 - TILE as f32 / 2.0;
                    let dist2 = dx * dx + dy * dy;
                    if best.map_or(true, |(bd, ..)| dist2 < bd) {
                        best = Some((dist2, x, y, d));
                    }
                }
            }
        }
        let (_, txl, tyl, depth) = best?;
        // Unproject the WINNING texel, not always the cursor (§7.1 step 4): tile → screen
        // px → NDC through the ORIGINAL (uncropped) view_proj.
        let px = cursor_rel.0 + (txl as f32 + 0.5 - TILE as f32 / 2.0);
        let py = cursor_rel.1 + (tyl as f32 + 0.5 - TILE as f32 / 2.0);
        let ndc = Vec3::new(2.0 * px / vp.0 - 1.0, 1.0 - 2.0 * py / vp.1, depth);
        let inv = (view_proj).inverse();
        let h = inv * Vec4::new(ndc.x, ndc.y, ndc.z, 1.0);
        if h.w.abs() < 1e-12 {
            return None;
        }
        Some(h.truncate() / h.w)
    }
}

impl crate::AzraApp {
    /// GPU crop pick for the active body's displayed mesh. `None` (resources absent, no
    /// cid/GpuMesh, whole-tile miss) → the caller's CPU DDA takes over (§7.1 fallback).
    pub(crate) fn poly_sculpt_gpu_pick(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        cursor: eframe::egui::Pos2,
        rect: eframe::egui::Rect,
    ) -> Option<Vec3> {
        let cid = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(core_math::scene_graph::NodePayload::PolygonalBody { mesh_cache_id, .. }) => {
                (*mesh_cache_id)?
            }
            _ => return None,
        };
        let model = self.scene.scene_graph.world_transform(nid);
        let gpu = self.gpu.as_ref()?;
        let pick = gpu.sculpt_pick.as_ref()?;
        let gm = self.cache.mesh_cache.get(cid)?.gpu_mesh.as_ref()?;
        let vp = (rect.width(), rect.height());
        let (near, far) =
            render::camera::orbit_clip_planes(self.viewport.camera.distance, self.render.ortho);
        let cam = render::camera::CameraUniforms::from_orbit(
            self.viewport.camera.orbit,
            self.viewport.camera.distance,
            self.viewport.camera.target,
            vp.0 / vp.1.max(1.0),
            self.render.fov_deg,
            near,
            far,
            self.render.ortho,
        );
        let view_proj = Mat4::from_cols_array_2d(&cam.view_proj);
        pick.pick(
            &gpu.device,
            &gpu.queue,
            &gm.vertex_buffer,
            &gm.index_buffer,
            gm.index_count,
            gm.index_format,
            view_proj,
            model,
            (cursor.x - rect.left(), cursor.y - rect.top()),
            vp,
        )
    }
}

// ============================================================================
// PR-4b G1 device test (§7.1 step 8): OFF-CENTER cursor, GPU world hit vs the
// classic full walk, within 1e-3 × extent. A centered any-hit is explicitly not
// enough. Soft-skips without an adapter (gpu_test_harness).
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use eframe::wgpu::util::DeviceExt;
    use std::collections::HashSet;

    #[test]
    fn g1_gpu_crop_pick_matches_full_walk_off_center() {
        let Some((_lock, device, queue)) = crate::gpu_test_harness::headless_device() else {
            eprintln!("[g1_gpu] no adapter — skipped");
            return;
        };
        // Bumpy plane, world-transformed so nothing is axis-trivial.
        let mesh = {
            let n = 60usize;
            let mut pos = Vec::new();
            for j in 0..=n {
                for i in 0..=n {
                    pos.push(Vec3::new(
                        i as f32 * 0.2,
                        ((i * 13 + j * 7) % 11) as f32 * 0.05,
                        j as f32 * 0.2,
                    ));
                }
            }
            let at = |i: usize, j: usize| j * (n + 1) + i;
            let mut faces = Vec::new();
            for j in 0..n {
                for i in 0..n {
                    faces.push(vec![at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
            let mut m = core_math::half_edge::HalfEdgeMesh::from_polygons(&pos, &faces);
            m.recompute_normals();
            m
        };
        let model = Mat4::from_rotation_y(0.3) * Mat4::from_translation(Vec3::new(-6.0, 0.0, -6.0));
        let cfg = core_math::poly_mesh::NormalConfig::default();
        let (verts, indices, _) =
            core_math::poly_mesh::raster_data_ngon_with_normals(&mesh, &cfg);
        let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let pick = SculptPickGpu::create(&device);

        let (vp_w, vp_h) = (1600.0f32, 900.0f32);
        let (near, far) = render::camera::orbit_clip_planes(18.0, false);
        let cam = render::camera::CameraUniforms::from_orbit(
            glam::Quat::from_euler(glam::EulerRot::YXZ, 0.4, -0.9, 0.0),
            18.0,
            Vec3::new(0.0, 0.0, 0.0),
            vp_w / vp_h,
            45.0,
            near,
            far,
            false,
        );
        let view_proj = Mat4::from_cols_array_2d(&cam.view_proj);
        let inv = view_proj.inverse();
        let hidden = HashSet::new();
        let mut compared = 0;
        // Several OFF-CENTER cursors (fractions of the viewport, none at 0.5).
        for &(fx, fy) in &[(0.62, 0.41), (0.33, 0.68), (0.71, 0.55), (0.45, 0.30)] {
            let (cx, cy) = (vp_w * fx, vp_h * fy);
            // CPU reference ray through the SAME pixel + same convention.
            let ndc = |z: f32| Vec4::new(2.0 * cx / vp_w - 1.0, 1.0 - 2.0 * cy / vp_h, z, 1.0);
            let p0 = inv * ndc(0.0);
            let p1 = inv * ndc(1.0);
            let (a, b) = (p0.truncate() / p0.w, p1.truncate() / p1.w);
            let rd = (b - a).normalize();
            let cpu = crate::poly_selection::ray_pick_mesh_face(&mesh, model, a, rd, &hidden)
                .map(|(t, _)| a + rd * t);
            let gpu_hit = pick.pick(
                &device,
                &queue,
                &vb,
                &ib,
                indices.len() as u32,
                wgpu::IndexFormat::Uint32,
                view_proj,
                model,
                (cx, cy),
                (vp_w, vp_h),
            );
            match (cpu, gpu_hit) {
                (Some(c), Some(g)) => {
                    let extent = 12.0f32 * 1.44; // plane span, rotated
                    assert!(
                        (c - g).length() < 1e-3 * extent,
                        "cursor ({fx},{fy}): cpu {c:?} vs gpu {g:?} — {} > eps",
                        (c - g).length()
                    );
                    compared += 1;
                }
                (None, None) => {}
                (c, g) => panic!("cursor ({fx},{fy}): cpu={c:?} gpu={g:?} — one path missed"),
            }
        }
        assert!(compared >= 3, "fixture must hit ({compared}/4) or the gate is vacuous");
    }
}
