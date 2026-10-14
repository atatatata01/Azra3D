//! CUDA render dispatch — extracted from viewport_render.rs (de-god slice 16).
//!
//! `dispatch_cuda_render` is a self-contained `#[cfg(feature = "cuda")]` path that
//! re-derives RenderConfig, serializes prims, calls cuda.render, and blits the
//! result to the wgpu texture. The whole module (and its sole method) is
//! feature-gated so a default (no-cuda) build excludes it identically to before.

#![cfg(feature = "cuda")]

use eframe::egui;
use eframe::wgpu;
use crate::*;

pub(crate) fn cuda_viewport_eligible(is_path_trace: bool, ai_exclusive: bool, renderer_ready: bool) -> bool {
    is_path_trace && !ai_exclusive && renderer_ready
}

#[cfg(test)]
mod routing_tests {
    use super::cuda_viewport_eligible;

    #[test]
    fn cuda_viewport_requires_all_readiness_conditions() {
        assert!(!cuda_viewport_eligible(true, false, ::cuda_render::OPTIX_MATERIAL_PARITY_READY));
        for is_path_trace in [false, true] {
            for ai_exclusive in [false, true] {
                for renderer_ready in [false, true] {
                    assert_eq!(
                        cuda_viewport_eligible(is_path_trace, ai_exclusive, renderer_ready),
                        is_path_trace && !ai_exclusive && renderer_ready,
                    );
                }
            }
        }
    }
}

impl FourthLayerApp {
    pub(crate) fn dispatch_cuda_render(&mut self, ctx: &egui::Context, rect: egui::Rect, cuda: &mut ::cuda_render::CudaRenderer) -> bool {
        if !cuda_viewport_eligible(
            matches!(self.render.shading_mode, ShadingMode::PathTrace),
            self.ai.ai_gpu_exclusive(),
            cuda.hw_rt_available(),
        ) {
            return false;
        }
        if !self.is_dirty { return true; }

        let full_w = rect.width().max(64.0) as u32;
        let full_h = rect.height().max(64.0) as u32;
        let width = full_w;
        let height = full_h;

        // Resize CUDA buffers if needed
        if let Err(e) = cuda.resize(width, height) {
            eprintln!("[CUDA] resize failed: {}", e);
            return false;
        }

        // Serialize scene
        let hg = std::collections::HashSet::new(); // layers removed — no hidden groups
        let sel_set2: std::collections::HashSet<usize> = self.selected_prim_indices_for_gpu().into_iter().collect();
        let empty_sel2 = std::collections::HashSet::new();
        let gpu_sel2 = if self.viewport.show_sel_rim { &sel_set2 } else { &empty_sel2 };
        let (nv, pb) = graphics::serialize_primitives_bytes_ex(
            &self.scene.primitives, gpu_sel2, &self.ai.ai_tex_textured, &hg,
        );

        // Render mode mapping (mirrors WGSL path)
        let rm: u32 = match self.render.shading_mode {
            ShadingMode::Pbr => 0, ShadingMode::MatCap => 1, ShadingMode::Wireframe => 2,
            ShadingMode::SolidHdr => 3, ShadingMode::Raytrace => 4, ShadingMode::PathTrace => 5, ShadingMode::Lit => 6,
        };
        let vm: u32 = match self.render.viewport_mode {
            ViewportRenderMode::FullSdf => 0, _ => 0,
        };

        let fog_d = if self.render.fog_enabled { self.render.fog_density } else { 0.0 };
        let bloom_flags: u32 = (if self.render.bloom_enabled { 2 } else { 0 }) | (if self.render.dither_enabled { 16 } else { 0 });

        let mut cfg = graphics::RenderConfig {
            width: width as usize, height: height as usize,
            cam_orbit: self.viewport.camera.orbit, cam_dist: self.viewport.camera.distance, cam_target: self.viewport.camera.target,
            grid_res: self.cache.cpu_volume.resolution[0], grid_bounds: self.cache.cpu_volume.grid_bounds,
            num_primitives: nv, render_mode: rm, view_mode: vm,
            render_quality: self.render.render_quality, light: self.scene.light_config,
            tex_size: self.ai.ai_tex_dim, tex_tiling: self.ai.ai_tex_tiling,
            exposure: if self.env_hdr_width > 0 { self.render.exposure } else { 1.0 },
            bloom_threshold: self.render.bloom_threshold, bloom_intensity: self.render.bloom_intensity, bloom_flags,
            fog_density: fog_d, fog_height_falloff: self.render.fog_height_falloff,
            fog_start: 1.0, fog_scatter: self.render.fog_scatter,
            env_width: if self.render.env_enabled { self.env_hdr_width } else { 0 },
            env_height: if self.render.env_enabled { self.env_hdr_height } else { 0 },
            env_reflection_blur: if self.env_hdr_width > 0 && self.render.env_enabled {
                self.render.env_reflection_blur
            } else {
                0.0
            },
            bg_color_top: if self.render.shading_mode == ShadingMode::MatCap {
                if self.render.matcap_bg_gradient { self.render.matcap_bg_gradient_top } else { self.render.matcap_bg_color }
            } else { self.render.bg_color_top },
            bg_color_bottom: if self.render.shading_mode == ShadingMode::MatCap {
                self.render.matcap_bg_color
            } else { self.render.bg_color_bottom },
            ortho: self.render.ortho, fov_deg: self.render.fov_deg,
            env_brightness: if self.env_hdr_width > 0 { self.render.env_brightness } else { 0.0 },
            env_contrast:   if self.env_hdr_width > 0 { self.render.env_contrast }   else { 0.0 },
            env_saturation: if self.env_hdr_width > 0 { self.render.env_saturation } else { 0.0 },
            env_blur:       if self.env_hdr_width > 0 && self.render.env_enabled { self.render.env_blur } else { 0.0 },
            fog_phase_g: self.render.fog_phase_g, fog_absorption: self.render.fog_absorption,
            fog_noise_scale: self.render.fog_noise_scale, fog_noise_strength: self.render.fog_noise_strength,
            fog_max_distance: self.render.fog_max_distance, fog_shadow_steps: self.render.fog_shadow_steps,
            fog_temporal_blend: self.render.fog_temporal_blend, fog_frame_index: self.render.fog_frame_index,
            fog_color: self.render.fog_color,
            rt_bounces: self.render.rt_bounces, rt_shadow_samples: self.render.rt_shadow_samples,
            pt_max_bounces: self.render.pt_max_bounces, pt_samples_per_frame: self.render.pt_samples_per_frame,
            pt_accumulated: self.render.pt_accumulated_frames, pt_frame_seed: self.render.pt_frame_seed,
            pt_focus_uv: [-1.0, -1.0], pt_focus_radius: self.render.pt_focus_radius, pt_focus_falloff: self.render.pt_dof_falloff,
            rt_env_samples: self.render.rt_env_samples, rt_caustics: self.render.rt_caustics,
            pt_dof_aperture: self.render.pt_dof_aperture, pt_denoise: self.render.pt_denoise,
            pt_clamp: self.render.pt_clamp,
            ..Default::default()
        };
        let (_near, far) =
            render::camera::orbit_clip_planes(self.viewport.camera.distance, self.render.ortho);
        cfg.far_clip = far;
        cfg.max_steps = cfg.max_steps.max(160);

        let mut uniforms = cfg.to_gpu_uniforms();
        { let sym = &self.viewport.symmetry;
          uniforms.sym_enabled = if sym.enabled { 1 } else { 0 };
          uniforms.sym_origin = [sym.origin.x, sym.origin.y, sym.origin.z, 0.0];
          uniforms.sym_normal = [sym.normal.x, sym.normal.y, sym.normal.z, sym.radial_count as f32];
        }
        uniforms.sel_rim_color = if self.sel.usel.has_nodes() && !self.sel.usel.has_prims() {
            [0.15, 0.9, 0.3]
        } else {
            self.viewport.sel_rim_color
        };
        uniforms.sel_rim_enabled = if self.viewport.show_sel_rim { 1 } else { 0 };
        uniforms.sel_rim_intensity = 0.0;
        uniforms.sel_rim_power = self.viewport.sel_outline_width;
        uniforms.sel_rim_base = 0.0;
        uniforms.dither_strength = self.render.dither_strength;
        let uniform_bytes = uniforms.as_bytes();

        // Convert prim_bytes (u8) to f32 for CUDA upload
        let prim_floats: Vec<f32> = pb.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let post_flags = 1u32
            | if self.render.bloom_enabled { 2 } else { 0 }
            | if self.render.ssr_enabled { 4 } else { 0 }
            | if self.render.fog_enabled { 8 } else { 0 }
            | if self.render.dither_enabled { 16 } else { 0 };

        // AI exclusive: never path-trace — free VRAM for Flux / Neural Beauty.
        let use_pt = matches!(self.render.shading_mode, ShadingMode::PathTrace)
            && !self.ai.ai_gpu_exclusive();
        let use_upsample = false; // textures always full-res

        // Hardware RT: feed world-space triangles into the OptiX triangle GAS
        // when PathTrace is active and the HW pipeline is live.
        #[cfg(feature = "optix")]
        if use_pt && cuda.hw_rt_available() {
            self.upload_hw_rt_triangle_scene(cuda);
        }

        match cuda.render(uniform_bytes, &prim_floats, post_flags, use_pt, use_upsample) {
            Ok(rgba_data) => {
                // Upload RGBA8 result to wgpu texture for egui display
                if let Some(gpu) = &mut self.gpu {
                    let queue = &gpu.queue;
                    if let Some(ref t) = gpu.texture {
                        queue.write_texture(
                            wgpu::ImageCopyTexture { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                            &rgba_data,
                            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
                            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                        );
                        self.is_dirty = false;
                        return true;
                    }
                }
            }
            Err(e) => {
                eprintln!("[CUDA] render failed: {}", e);
            }
        }
        false
    }

    /// Flatten the PT accel scene into world-space triangles and rebuild the
    /// OptiX triangle GAS when the PT scene generation has moved.
    #[cfg(feature = "optix")]
    fn upload_hw_rt_triangle_scene(&mut self, cuda: &mut ::cuda_render::CudaRenderer) {
        let scene_gen = self
            .gpu
            .as_ref()
            .map(|g| g.buffers.pt_scene_generation)
            .unwrap_or(0);
        if scene_gen == 0 {
            return;
        }

        // Re-enumerate + assemble so CUDA sees the same geometry as WGSL PT.
        let (instances, materials) = self.gather_pt_instances();
        if instances.is_empty() {
            return;
        }
        let scene = self.cache.pt_accel.assemble(&instances);
        if scene.is_empty() || scene.tris.is_empty() {
            return;
        }

        // World-space flatten (body-local verts × instance model). Verts are
        // duplicated per instance so a single triangle GAS stays correct under
        // independent body transforms.
        let mut out_pos: Vec<[f32; 3]> = Vec::new();
        let mut out_nrm: Vec<[f32; 3]> = Vec::new();
        let mut out_idx: Vec<u32> = Vec::new();
        let mut out_tri_mat: Vec<u32> = Vec::new();

        for inst in &scene.instances {
            let model = glam::Mat4::from_cols_array_2d(&inst.model);
            let nmat = glam::Mat3::from_cols(
                glam::Vec3::new(inst.normal_mat[0][0], inst.normal_mat[0][1], inst.normal_mat[0][2]),
                glam::Vec3::new(inst.normal_mat[1][0], inst.normal_mat[1][1], inst.normal_mat[1][2]),
                glam::Vec3::new(inst.normal_mat[2][0], inst.normal_mat[2][1], inst.normal_mat[2][2]),
            );
            let tri_start = inst.tri_off as usize;
            let tri_end = scene
                .instances
                .iter()
                .map(|i| i.tri_off as usize)
                .filter(|&t| t > tri_start)
                .min()
                .unwrap_or(scene.tris.len());

            let base = out_pos.len() as u32;
            let mut local_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
            let mut next_local = 0u32;

            for ti in tri_start..tri_end.min(scene.tris.len()) {
                let tri = scene.tris[ti];
                let mut corners = [0u32; 3];
                for (k, gv) in [tri.v0, tri.v1, tri.v2].into_iter().enumerate() {
                    let loc = *local_map.entry(gv).or_insert_with(|| {
                        let v = scene.verts.get(gv as usize);
                        let (p, n) = if let Some(v) = v {
                            let pw = model.transform_point3(glam::Vec3::from_array(v.pos));
                            let nw = nmat.mul_vec3(glam::Vec3::from_array(v.normal)).normalize_or_zero();
                            (pw.to_array(), nw.to_array())
                        } else {
                            ([0.0; 3], [0.0, 1.0, 0.0])
                        };
                        out_pos.push(p);
                        out_nrm.push(n);
                        let id = next_local;
                        next_local += 1;
                        id
                    });
                    corners[k] = base + loc;
                }
                out_idx.extend_from_slice(&corners);
                let mat_idx = if tri.material_idx != 0 {
                    tri.material_idx
                } else {
                    inst.material_base
                };
                out_tri_mat.push(mat_idx);
            }
        }

        // Convert RasterMaterial → HwRtMaterial
        let hw_mats: Vec<::cuda_render::HwRtMaterial> = materials
            .iter()
            .map(|m| {
                let em_i = m.roughness_metallic[2];
                ::cuda_render::HwRtMaterial {
                    albedo: [m.albedo[0], m.albedo[1], m.albedo[2]],
                    roughness: m.roughness_metallic[0],
                    emission: [
                        m.emission_color[0] * em_i,
                        m.emission_color[1] * em_i,
                        m.emission_color[2] * em_i,
                    ],
                    metallic: m.roughness_metallic[1],
                }
            })
            .collect();
        let hw_mats = if hw_mats.is_empty() {
            vec![::cuda_render::HwRtMaterial::default()]
        } else {
            hw_mats
        };

        if out_idx.is_empty() {
            return;
        }

        match cuda.update_triangle_scene(
            scene_gen,
            &out_pos,
            &out_nrm,
            &out_idx,
            &out_tri_mat,
            &hw_mats,
        ) {
            Ok(true) => {
                eprintln!(
                    "[HW-RT] triangle GAS built: {} tris, {} verts, gen={}",
                    out_tri_mat.len(),
                    out_pos.len(),
                    scene_gen
                );
            }
            Ok(false) => {}
            Err(e) => eprintln!("[HW-RT] triangle GAS build failed: {}", e),
        }
    }
}
