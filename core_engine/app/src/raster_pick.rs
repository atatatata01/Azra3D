// ============================================================
// Hybrid Rasterization — GPU Node Picking (Slice 20)
// ============================================================
//
// Byte-for-byte relocation of `pick_node_at` + `poll_pending_pick` out of
// `raster_render.rs`. GPU color-ID picking: renders all rasterized meshes with
// flat node IDs into an off-screen target, reads back the pixel under the cursor
// one frame late, and resolves the NodeId. Selection is P0 — these MUST keep
// working. Stays `impl AzraApp` (NOT free fns): reads `&self.cache.mesh_cache` /
// `&self.scene.scene_graph` while `let Some(gpu) = &mut self.gpu` is live, which
// only compiles because gpu/cache/scene are disjoint fields (see PLAN §C).

use eframe::wgpu;
use eframe::wgpu::util::DeviceExt;
use core_math::scene_graph::NodePayload;
use crate::PendingPick;
use render::camera::CameraUniforms;
use render::mesh_gpu::InstanceData;
// Shared free-fn draw helpers (Slice 18) — live in raster_common.rs; imported here so the
// unqualified call sites in the picking pass keep resolving.
use crate::raster_common::{
    draw_resident_pool_with_instance, draw_sdf_pool_multi, draw_sdf_pool_slot,
    draw_sdf_resident_slot,
};

/// When true, prim positions are stored in body-local space.
/// Prims must NOT be re-offset when baking the DC mesh (they are already local).
/// The instance transform must include body_rotation and body_scale so the mesh
/// is placed correctly in world space.
use crate::BODY_LOCAL_PRIMS;

impl super::AzraApp {
    /// GPU color-ID pick at a viewport pixel. Renders all rasterized meshes
    /// with flat node IDs, reads back the pixel under the cursor, and returns
    /// the NodeId (or None if background was clicked).
    ///
    /// This is a blocking call (~1ms) — only call on click, not every frame.
    pub(crate) fn pick_node_at(
        &mut self,
        pixel_x: u32,
        pixel_y: u32,
        width: u32,
        height: u32,
        ctrl: bool,
        shift: bool,
    ) -> Option<core_math::scene_graph::NodeId> {
        // GUARD: never start a second GPU pick while one is still in flight.
        //
        // `start_readback()` leaves `picking_staging` mapped until `try_readback_node_id()`
        // unmaps it. Re-entering here would `copy_pixel()` into that still-mapped buffer and
        // then `queue.submit()`, which wgpu rejects with a VALIDATION ERROR — a hard panic
        // ("Buffer with 'picking_staging' label is still mapped"), i.e. a device-level crash
        // from nothing worse than two clicks in consecutive frames. Reproduced 2026-08-08 via
        // the SDF knife; 2026-08-10 owner crash also hit this after `pending_pick = None`
        // aborted a pick WITHOUT unmapping (tool-drop empty click).
        //
        // Drain first: if last frame's pick has landed, `poll_pending_pick()` unmaps and clears
        // it. If still pending, ABANDON (wait+unmap / recreate staging) so this click can
        // proceed safely — never leave the buffer mapped and never skip permanently.
        if self.pending_pick.is_some() {
            let _ = self.poll_pending_pick();
            if self.pending_pick.is_some() {
                self.cancel_pending_pick();
            }
        }

        // --- Gather draw calls with node IDs ---
        let mut draw_calls: Vec<(core_math::scene_graph::MeshCacheId, u32, core_math::scene_graph::NodeId)> = Vec::new();
        let mut resident_draws: std::collections::HashMap<usize, usize> = std::collections::HashMap::new(); // Phase 6
        // G2: draw_call index → (SDF body NodeId, pooled) picked from its GPU-resident slot (indirect
        // draw). Picking is P0 — resident bodies MUST be selectable, so they get the same indirect draw
        // the G-buffer uses; a POOLED body (stage B-1) is picked via the SAME shared-pool multi-draw.
        let mut sdf_resident_pick: std::collections::HashMap<usize, (core_math::scene_graph::NodeId, bool)> = std::collections::HashMap::new();

        for (nid, node) in self.scene.scene_graph.all_nodes() {
            if !node.visible { continue; }
            let cid = match &node.payload {
                NodePayload::SdfBody { mesh_cache_id: Some(cid), .. } => {
                    // Selection is P0: a top-level body defined entirely by nested sub-bodies (empty own
                    // prims, valid resident slot) MUST stay clickable. Canonical predicate — same one the
                    // draw loop uses. Nested bodies fall out via the mesh_cache-absence check below.
                    if !self.scene.scene_graph.body_has_renderable_content(nid) { continue; }
                    // G2: a resident body has no mesh_cache entry — pick it from its GPU slot instead.
                    // GATE 4: the SAME facade call the G-buffer gather uses — lockstep by construction
                    // (resident + Stage B-1 pooled + mesh_cache presence in one answer).
                    let pres = self.sdf_mesh_presentation(nid, Some(*cid));
                    if pres.is_resident() {
                        sdf_resident_pick.insert(draw_calls.len(), (nid, pres.is_pooled()));
                    }
                    else if !pres.has_any_mesh() { continue; }
                    *cid
                }
                NodePayload::ImportedMesh { mesh_cache_id, .. } => {
                    // Voxel: pick against the per-chunk meshes (what's actually rendered), not the
                    // node's single mesh (only refreshed on save) — mirror the raster path so
                    // click-to-select hits the live sculpted surface, not stale geometry.
                    let is_voxel = self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid && !v.chunks.is_empty());
                    if is_voxel {
                        let node_id_u32 = nid.0 as u32;
                        if let Some(v) = self.sculpt.voxel.as_ref() {
                            // SURFACE MODE (Gate 1): pool slots receive every surface vertex write
                            // (voxel_surface_pool_upload), so picking uses the pool exactly like the
                            // G-buffer draw — lockstep via the ONE classification predicate (same
                            // body gate incl. the paint CPU-path force, so pick == display always).
                            let resident_base = v.gpu_resident_mesh && !v.cpu_paint_path();
                            for (i, &ccid) in v.chunks.iter().enumerate() {
                                if crate::raster_common::voxel_chunk_draws_from_pool(self.gpu.as_ref(), resident_base, i) {
                                    resident_draws.insert(draw_calls.len(), i);
                                    draw_calls.push((ccid, node_id_u32, nid));
                                } else if v.chunk_tris.get(i).copied().unwrap_or(0) > 0 {
                                    draw_calls.push((ccid, node_id_u32, nid)); // not-yet-resident → CPU mesh
                                }
                            }
                        }
                        continue; // pushed per-chunk draw calls directly; skip the single-mesh path
                    }
                    if self.cache.mesh_cache.get(*mesh_cache_id).is_none() { continue; }
                    *mesh_cache_id
                }
                NodePayload::VoxelBody { mesh_cache_id, .. } => {
                    // P0.5: pick the active voxel against its per-chunk meshes (live surface, mirrors the
                    // ImportedMesh voxel path); a non-active VoxelBody picks against its cached display mesh.
                    let is_voxel = self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid && !v.chunks.is_empty());
                    if is_voxel {
                        let node_id_u32 = nid.0 as u32;
                        if let Some(v) = self.sculpt.voxel.as_ref() {
                            // SURFACE MODE (Gate 1): pool slots receive every surface vertex write
                            // (voxel_surface_pool_upload), so picking uses the pool exactly like the
                            // G-buffer draw — lockstep via the ONE classification predicate (same
                            // body gate incl. the paint CPU-path force, so pick == display always).
                            let resident_base = v.gpu_resident_mesh && !v.cpu_paint_path();
                            for (i, &ccid) in v.chunks.iter().enumerate() {
                                if crate::raster_common::voxel_chunk_draws_from_pool(self.gpu.as_ref(), resident_base, i) {
                                    resident_draws.insert(draw_calls.len(), i);
                                    draw_calls.push((ccid, node_id_u32, nid));
                                } else if v.chunk_tris.get(i).copied().unwrap_or(0) > 0 {
                                    draw_calls.push((ccid, node_id_u32, nid));
                                }
                            }
                        }
                        continue; // pushed per-chunk picks directly
                    }
                    match mesh_cache_id {
                        Some(cid) if self.cache.mesh_cache.get(*cid).is_some() => *cid,
                        _ => continue,
                    }
                }
                NodePayload::PolygonalBody { mesh_cache_id: Some(cid), .. } => {
                    if self.cache.mesh_cache.get(*cid).is_none() { continue; }
                    *cid
                }
                NodePayload::NurbsBody {
                    mesh_cache_id: Some(cid),
                    ..
                }
                | NodePayload::NurbsSolid {
                    mesh_cache_id: Some(cid),
                    ..
                } => {
                    if self.cache.mesh_cache.get(*cid).is_none() {
                        continue;
                    }
                    *cid
                }
                _ => continue,
            };
            let transform = self.scene.scene_graph.world_transform(nid);
            let (scale, rot, pos) = transform.to_scale_rotation_translation();
            let rot_deg = rot.to_euler(glam::EulerRot::YXZ);
            let node_id_u32 = nid.0 as u32;
            draw_calls.push((cid, node_id_u32, nid));

            // We need the InstanceData with node_id encoded in material_idx
            let _ = (pos, rot_deg, scale); // used below
        }

        if draw_calls.is_empty() { return None; }

        let Some(gpu) = &mut self.gpu else { return None };
        let device = &gpu.device;
        let queue = &gpu.queue;

        // Ensure picking textures + bind group
        gpu.picking.ensure_textures(device, width, height);
        let (near, far) =
            render::camera::orbit_clip_planes(self.viewport.camera.distance, self.render.ortho);
        let camera = CameraUniforms::from_orbit(
            self.viewport.camera.orbit, self.viewport.camera.distance, self.viewport.camera.target,
            width as f32 / height as f32, self.render.fov_deg, near, far, self.render.ortho,
        );
        gpu.picking.upload_camera(queue, &camera);
        if gpu.picking.bind_group.is_none() {
            gpu.picking.rebuild_bind_group(device);
        }

        // Build instance data with node_id in material_idx slot (values kept — stage B-1 writes the
        // pooled bodies' entries into the shared pool instance buffer below), then per-draw buffers.
        let instance_data: Vec<InstanceData> = draw_calls.iter().map(|(_, node_id_u32, nid)| {
            // SdfBody: mesh is body-local, instance places it at world position with
            // full TRS (body_rotation + body_scale) so picking aligns with the raster pass.
            // Other nodes: use world_transform as before.
            let inst = if BODY_LOCAL_PRIMS {
                if let Some(node) = self.scene.scene_graph.get(*nid) {
                    if let NodePayload::SdfBody { origin, body_rotation, body_scale, .. } = &node.payload {
                        // MUST match the render instance matrix (raster_render ~420): compose the parent
                        // SdfObject's placement so picking aligns with what's drawn (else clicks miss).
                        let mut model = self.scene.scene_graph.effective_body_transform(*nid)
                            .unwrap_or_else(|| glam::Mat4::from_scale_rotation_translation(*body_scale, *body_rotation, *origin));
                        // P1-#6b (fixed): same EXPLICIT rigid-translate re-anchor as the G-buffer
                        // instance — picking MUST stay in lockstep with the drawn mesh (the P0
                        // selection rule).
                        let off = self.cache.body_vol_cache.mesh_rigid_offset(*nid);
                        if off.length_squared() > 0.0 {
                            model *= glam::Mat4::from_translation(off);
                        }
                        let normal_mat = glam::Mat3::IDENTITY; // picking pass doesn't need normals
                        render::mesh_gpu::InstanceData {
                            model_matrix: model.to_cols_array_2d(),
                            normal_matrix: [
                                [normal_mat.x_axis.x, normal_mat.x_axis.y, normal_mat.x_axis.z, 0.0],
                                [normal_mat.y_axis.x, normal_mat.y_axis.y, normal_mat.y_axis.z, 0.0],
                                [normal_mat.z_axis.x, normal_mat.z_axis.y, normal_mat.z_axis.z, 0.0],
                            ],
                            material_idx: *node_id_u32,
                            body_index: 0,
                            flags: 0,
                            _pad: 0,
                            curv_params: [0.0, 1.0, 1.0, 1.0],
                        }
                    } else {
                        // Non-SdfBody node: use world transform
                        let transform = self.scene.scene_graph.world_transform(*nid);
                        let (s, r, p) = transform.to_scale_rotation_translation();
                        let rd = r.to_euler(glam::EulerRot::YXZ);
                        InstanceData::from_transform(
                            p,
                            glam::Vec3::new(rd.1.to_degrees(), rd.0.to_degrees(), rd.2.to_degrees()),
                            s,
                            *node_id_u32,
                            0,
                        )
                    }
                } else {
                    InstanceData::from_transform(glam::Vec3::ZERO, glam::Vec3::ZERO, glam::Vec3::ONE, *node_id_u32, 0)
                }
            } else {
                // Legacy: SdfBody uses translation-only, others use world_transform
                let (pos, rot_deg_vec, scale) = if self.scene.scene_graph.body_origin(*nid).is_some() {
                    let origin = self.scene.scene_graph.body_origin(*nid).unwrap();
                    (origin, glam::Vec3::ZERO, glam::Vec3::ONE)
                } else {
                    let transform = self.scene.scene_graph.world_transform(*nid);
                    let (s, r, p) = transform.to_scale_rotation_translation();
                    let rd = r.to_euler(glam::EulerRot::YXZ);
                    (p, glam::Vec3::new(rd.1.to_degrees(), rd.0.to_degrees(), rd.2.to_degrees()), s)
                };
                InstanceData::from_transform(pos, rot_deg_vec, scale, *node_id_u32, 0)
            };
            inst
        }).collect();
        let instance_buffers: Vec<wgpu::Buffer> = instance_data.iter().map(|inst| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("picking_instance"),
                contents: bytemuck::bytes_of(inst),
                usage: wgpu::BufferUsages::VERTEX,
            })
        }).collect();

        // Stage B-1 pool pick prep (write_buffer only): zero every pool entry's instance_count,
        // then enable exactly the pooled bodies gathered by THIS pick's filters, writing their
        // PICKING InstanceData (node id in material_idx) into the shared pool instance buffer.
        // Same slots, same geometry, same args as the G-buffer draw — lockstep by construction
        // (P0: click-select must hit exactly what is rendered). The next frame's G-buffer prep
        // rewrites these entries with render instances before its own submit.
        if let Some(pool) = gpu.sdf_mesh_pool.as_mut() {
            pool.clear_frame_visibility(queue);
            for (i, (nid, pooled)) in sdf_resident_pick.iter() {
                if *pooled { pool.mark_drawn(queue, *nid, &instance_data[*i]); }
            }
        }

        // Encode picking render pass
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("picking"),
        });

        {
            let cv = gpu.picking.color_view.as_ref().unwrap();
            let dv = gpu.picking.depth_view.as_ref().unwrap();

            let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("picking_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: cv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_pipeline(&gpu.picking.pipeline);
            if let Some(ref bg) = gpu.picking.bind_group {
                rpass.set_bind_group(0, bg, &[]);
            }

            // POOL fix: click-select hits the GPU-resident live surface (P0 selection MUST work) via ONE
            // multi_draw over the shared pool. The picking pass uses per-draw instance buffers, but all
            // resident chunks of the body carry the SAME node_id → bind the FIRST resident draw's instance.
            if let Some(&first_dc) = resident_draws.keys().min() {
                draw_resident_pool_with_instance(&mut rpass, gpu, true, instance_buffers[first_dc].slice(..));
            }
            // Stage B-1: pooled sparse SDF bodies picked via the SAME shared-pool multi-draw as the
            // G-buffer (visibility + picking instances pre-written above). Fallback loops per slot.
            // GPU_CURVATURE_KERNEL_PLAN.md P2: the picking pipeline's own bind-group layout is
            // camera-only (`picking.wgsl: @group(0) @binding(0)`) — it has NO group 2, so `None`
            // here means these shared draw helpers skip the `set_bind_group(2, ..)` call entirely
            // rather than fail pipeline validation against a layout that doesn't have that slot.
            let sdf_pool_handled = draw_sdf_pool_multi(&mut rpass, gpu, None);
            for (i, (cid, _, _)) in draw_calls.iter().enumerate() {
                if resident_draws.contains_key(&i) { continue; } // drawn via the pool multi-draw above
                if let Some(&(res_nid, pooled)) = sdf_resident_pick.get(&i) {
                    if pooled {
                        if !sdf_pool_handled {
                            draw_sdf_pool_slot(&mut rpass, gpu, res_nid, instance_buffers[i].slice(..), None);
                        }
                        continue;
                    }
                    // G2: SDF body's GPU-resident slot — same indirect draw as the G-buffer, so click-select
                    // hits the live resident surface (selection MUST work — P0).
                    draw_sdf_resident_slot(&mut rpass, gpu, res_nid, instance_buffers[i].slice(..), None);
                    continue;
                }
                if let Some(cached) = self.cache.mesh_cache.get(*cid) {
                    if let Some(ref gpu_mesh) = cached.gpu_mesh {
                        rpass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                        rpass.set_vertex_buffer(1, instance_buffers[i].slice(..));
                        rpass.set_index_buffer(gpu_mesh.index_buffer.slice(..), gpu_mesh.index_format);
                        rpass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
                    }
                }
            }
        }

        // Copy pixel at click position to staging buffer
        gpu.picking.copy_pixel(&mut enc, pixel_x, pixel_y);

        // Submit + complete readback BEFORE returning so `picking_staging` is never left
        // mapped across later submits this frame (mesh sync, voxel regen, poly pick, …).
        // 2026-08-10 owner crashes (08:18 + 09:43): async map left the buffer mapped and
        // any subsequent `queue.submit` that touched it panicked wgpu (exit 0xcfffffff).
        // One-pixel Wait is typically sub-ms; selection still applies via `handle_pending_pick`.
        queue.submit(Some(enc.finish()));
        let map_ready = gpu.picking.start_readback();
        let resolved_raw = gpu
            .picking
            .blocking_readback_node_id(&gpu.device, &map_ready);
        self.pending_pick = Some(PendingPick {
            map_ready,
            resolved_raw: Some(resolved_raw),
            ctrl,
            shift,
            cpu_resolved: None,
        });
        None
    }

    /// Poll pending GPU pick result. Called each frame.
    /// Returns `Some(NodeId)` when the readback is ready, `None` otherwise.
    pub(crate) fn poll_pending_pick(&mut self) -> Option<core_math::scene_graph::NodeId> {
        let pending = self.pending_pick.as_ref()?;
        // Prefer the click-frame blocking decode — staging is already unmapped.
        if let Some(raw_id) = pending.resolved_raw {
            self.pending_pick = None;
            return if raw_id == 0 {
                None
            } else {
                Some(core_math::scene_graph::NodeId(raw_id))
            };
        }
        let Some(gpu) = &self.gpu else {
            self.pending_pick = None;
            return None;
        };
        let result = gpu.picking.try_readback_node_id(&gpu.device, &pending.map_ready);
        match result {
            Some(raw_id) => {
                self.pending_pick = None;
                if raw_id == 0 { None } else { Some(core_math::scene_graph::NodeId(raw_id)) }
            }
            None => None, // not ready yet, try next frame
        }
    }

    /// Abort an in-flight Hybrid GPU pick and leave `picking_staging` unmapped.
    ///
    /// Must be used instead of `self.pending_pick = None` whenever a pick is cancelled mid-flight
    /// (tool-drop empty click, etc.). Clearing the slot alone can leave the staging buffer mapped;
    /// the next `queue.submit` that copies into it panics wgpu and kills the process
    /// (`fourthlayer_panic.txt` 2026-08-10).
    pub(crate) fn cancel_pending_pick(&mut self) {
        let Some(pending) = self.pending_pick.take() else { return };
        // Blocking path already unmapped when `resolved_raw` is set — nothing to abandon.
        if pending.resolved_raw.is_some() {
            return;
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.picking
                .abandon_pixel_readback(&gpu.device, &pending.map_ready);
        }
    }
}
