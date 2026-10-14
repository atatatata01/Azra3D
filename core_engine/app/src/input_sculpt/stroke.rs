//! Stroke lifecycle: sculpt input entry points + pen-up release tail.
use super::*;

impl AzraApp {
    /// Submit interpolator/deferred dabs that only become available at pen-up while the atlas is
    /// still authoritative. This must run before the stroke's atlas undo builder is finished and
    /// before GPU-to-CPU materialisation, otherwise the readback overwrites a CPU-only tail.
    pub(super) fn voxel_dispatch_release_tail_atlas(
        &mut self,
        primary: &[core_math::sculpt_layer::StrokeSample],
    ) -> bool {
        use core_math::voxel_sculpt::VoxelBrush;

        if primary.is_empty() { return true; }
        let (
            brush, radius, strength, hardness, brush_add, symmetry, gres, gbounds, giso,
            tip_frame, tip_contrast,
        ) = match self.sculpt.voxel.as_ref() {
            Some(v) => (
                v.brush,
                v.radius,
                v.strength,
                v.falloff,
                v.brush_add,
                v.symmetry,
                v.grid.res,
                v.grid.bounds,
                v.grid.iso,
                v.brush_tip.clone(),
                v.tip_contrast.clamp(0.05, 4.0),
            ),
            None => return false,
        };
        let op_code = match brush {
            VoxelBrush::Draw => 0,
            VoxelBrush::Erase => 1,
            VoxelBrush::Smooth => 2,
            VoxelBrush::Flatten => 3,
            VoxelBrush::Clay => 4,
            VoxelBrush::Scrape => 5,
            VoxelBrush::Crease => 6,
            VoxelBrush::Fill => 7,
            VoxelBrush::ClayBuildup => 8,
            VoxelBrush::Inflate => 9,
            VoxelBrush::Pinch => 10,
            // Move/Smudge are CPU-forced grabs; Snake Hook is a CPU-only surface tube. None have a
            // GPU op-code — bail to the CPU/surface path.
            VoxelBrush::Move | VoxelBrush::Smudge | VoxelBrush::SnakeHook => return false,
        };
        // Stroke-axis Pinch tangent for this batch: net displacement across the primary samples,
        // normalized. <2 samples (or a still stroke) → ZERO → axisymmetric fallback (matches CPU).
        let stroke_tan = if brush == VoxelBrush::Pinch && primary.len() >= 2 {
            (primary[primary.len() - 1].position - primary[0].position).normalize_or_zero()
        } else {
            glam::Vec3::ZERO
        };
        // The STROKE's latched polarity (Shift/Ctrl/Alt/brush_add folded at the stamp-frame
        // resolve) — never recomputed from raw brush_add here, which dropped Ctrl/Alt and
        // flipped the tail's sign at pen-up.
        let _ = brush_add;
        let invert = self.sculpt.voxel_stroke_invert;
        let sgn = if invert { -1.0 } else { 1.0 };

        let mut expanded = primary.to_vec();
        let voxel_size = self.sculpt.voxel.as_ref().unwrap().grid.voxel_size().max(1e-6);
        expand_voxel_symmetry(&mut expanded, symmetry, voxel_size * 0.5);
        let stamps: Vec<GpuVoxelStamp> = {
            let v = self.sculpt.voxel.as_ref().unwrap();
            expanded.iter().filter_map(|pt| {
                let dab_r = (radius * pt.radius_scale).max(1e-4);
                let dab_s = strength * pt.pressure;
                v.grid.brush_aabb(pt.position, dab_r).map(|aabb| {
                    (op_code, pt.position, pt.normal, dab_r, dab_s, sgn, aabb, pt.tip_rotation)
                })
            }).collect()
        };
        if stamps.is_empty() { return true; }

        let Some(gpu) = self.gpu.as_ref() else { return false; };
        if gpu.voxel_brush.is_none() {
            let device = gpu.device.clone();
            self.gpu.as_mut().unwrap().voxel_brush =
                Some(crate::voxel_gpu_mesh::VoxelBrushGpu::new(&device));
        }

        let brick_res = self.sculpt.voxel.as_ref().unwrap().grid.density.brick_res();
        let touched_bricks = voxel_bricks_for_regions(stamps.iter().map(|stamp| stamp.6), brick_res);
        let report = self.sculpt.voxel.as_mut().map_or_else(Vec::new, |v| {
            v.grid.density.materialize_bricks_report(&touched_bricks)
        });
        self.voxel_sync_mesh_atlas();

        let stamp_regions: Vec<[u32; 6]> = stamps.iter().map(|stamp| stamp.6).collect();
        let batches = voxel_atlas_region_batches(
            &stamp_regions,
            VOXEL_ATLAS_MAX_SCRATCH_VOXELS,
        );
        let submitted = {
            let Some(gpu) = self.gpu.as_mut() else { return false; };
            let GpuCtx {
                device,
                queue,
                voxel_mesh_atlas,
                voxel_brush,
                voxel_atlas_undo_building,
                ..
            } = gpu;
            let (Some(atlas), Some(gpu_brush)) =
                (voxel_mesh_atlas.as_ref(), voxel_brush.as_ref())
            else {
                return false;
            };
            let (device, queue) = (&**device, &**queue);
            let builder = voxel_atlas_undo_building.get_or_insert_with(Default::default);
            builder.capture(device, queue, &atlas.pool_buf, &report);
            let preset = self.sculpt.voxel.as_ref().map_or(0, |v| v.falloff_preset.as_u32());
            let pinch = self.sculpt.voxel.as_ref().map_or(1.0, |v| v.pinch_intensity);
            let plane = self.sculpt.voxel.as_ref()
                .map(crate::voxel_gpu_mesh::gpu_plane_knobs_from_voxel)
                .unwrap_or_default();
            for (range, union) in &batches {
                gpu_brush.dispatch_stamps_region_atlas(
                    device,
                    queue,
                    gres,
                    gbounds,
                    giso,
                    brick_res,
                    &atlas.index_buf,
                    &atlas.pool_buf,
                    *union,
                    hardness,
                    preset,
                    pinch,
                    plane,
                    tip_frame.as_ref(),
                    tip_contrast,
                    stroke_tan,
                    &stamps[range.clone()],
                );
            }
            !batches.is_empty()
        };
        if submitted {
            for stamp in &stamps {
                self.voxel_mark_chunks_for_brush(stamp.1, stamp.3);
                self.voxel_expand_stroke_aabb(stamp.6);
            }
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.atlas_stroke = true;
                v.cpu_grid_stale = true;
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
        submitted
    }

    pub(crate) fn handle_sculpt_input(&mut self, response: &egui::Response, rect: egui::Rect, shift: bool, ctrl: bool, alt: bool) {
        // Brush owns LMB in Sculpt. Drop a paint/box/region that armed earlier this frame
        // (Face-mode paint used to start on press before this function ran).
        if self.sculpt_brush_owns_lmb()
            && (self.tool.current.is_poly_paint_select()
                || self.tool.current.is_poly_region_select()
                || self.tool.current.is_poly_box_select()
                || self.tool.current.is_poly_circle_select()
                || self.tool.current.is_selection_box())
        {
            self.tool.current = crate::ActiveTool::Idle;
        }
        // ── RMB-drag brush adjust (ZBrush, owner 2026-08-19): drag RIGHT = bigger radius,
        // drag UP = stronger. Works on whichever sculpt target is live (voxel or poly).
        // A short RMB click (below egui's drag threshold) never enters here, so its
        // existing meaning (context arms) is untouched; a real drag never reports
        // `clicked_by`, so nothing double-fires on release.
        if self.tool.work_mode == crate::WorkMode::Sculpt
            && !self.tool.current.is_gizmo()
            && !self.tool.current.is_transform()
            && !self.sculpt.stroke_active
        {
            if response.drag_started_by(egui::PointerButton::Secondary)
                && self.sculpt.rmb_adjust.is_none()
            {
                let anchor = response
                    .interact_pointer_pos()
                    .or_else(|| response.ctx.input(|i| i.pointer.press_origin()))
                    .unwrap_or_else(|| rect.center());
                let (r0, s0) = if let Some(v) = self.sculpt.voxel.as_ref() {
                    (v.radius, v.strength)
                } else {
                    self.sculpt.poly_ensure_defaults();
                    (self.sculpt.poly_radius, self.sculpt.poly_strength)
                };
                self.sculpt.rmb_adjust = Some((glam::vec2(anchor.x, anchor.y), r0, s0));
            }
            if let Some((anchor, r0, s0)) = self.sculpt.rmb_adjust {
                let rmb_down = response
                    .ctx
                    .input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
                if rmb_down {
                    if let Some(mp) = response
                        .interact_pointer_pos()
                        .or_else(|| response.ctx.input(|i| i.pointer.latest_pos()))
                    {
                        let dx = mp.x - anchor.x;
                        let dy = anchor.y - mp.y; // screen up = positive
                        // Radius is MULTIPLICATIVE (doubles every 160 px) so the gesture feels
                        // even at any size; strength is linear (full 0→1 over ~300 px).
                        let radius = r0 * (dx / 160.0).exp2();
                        let strength = s0 + dy / 300.0;
                        if let Some(v) = self.sculpt.voxel.as_mut() {
                            v.radius = radius.clamp(0.01, 2.0);
                            v.strength = strength.clamp(0.0, 1.0);
                        } else {
                            self.sculpt.poly_radius = radius.clamp(0.01, 4.0);
                            self.sculpt.poly_strength = strength.clamp(0.05, 2.0);
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                } else {
                    self.sculpt.rmb_adjust = None;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                return; // the gesture owns the pointer — no stroke handling this frame
            }
        } else if self.sculpt.rmb_adjust.is_some() {
            // Mode/tool changed mid-gesture — drop the latch, never leave it stale.
            self.sculpt.rmb_adjust = None;
        }
        // PolySculpt (C3): selected polygonal body in Sculpt mode is Surface sculpt —
        // even when a leftover voxel is still attached. That leftover was why a remeshed
        // cube + Tab pie SCULPT looked like "brushes do nothing" (strokes hit the voxel).
        let poly_drag = self.tool.work_mode == crate::WorkMode::Sculpt
            && !self.tool.current.is_gizmo()
            && !self.tool.current.is_transform()
            && (response.dragged_by(egui::PointerButton::Primary)
                || response.drag_started_by(egui::PointerButton::Primary));
        if poly_drag && self.sculpt.poly_edit.is_none() {
            if let Some(nid) = self.poly_active_body() {
                self.begin_surface_sculpt(nid);
            }
        }
        if self.sculpt.poly_edit.is_some()
            || (self.sculpt.voxel.is_none() && self.poly_sculpt_target().is_some())
        {
            let sculpting = self.tool.work_mode == crate::WorkMode::Sculpt
                && !self.tool.current.is_gizmo()
                && !self.tool.current.is_transform();
            if sculpting && response.dragged_by(egui::PointerButton::Primary) {
                self.handle_poly_sculpt_input(response, rect, shift, ctrl, alt);
                self.sculpt.stroke_active = true;
            } else if self.sculpt.stroke_active {
                self.commit_episode();
                self.sdf_stroke_interpolator.reset();
                self.sculpt.poly_stroke_last = None;
                // Release the Grab anchor HERE, the one stroke exit — a surviving anchor would
                // make the next grab drag the PREVIOUS stroke's frozen vertex set from its
                // original positions (the cross-stroke stale-state class `surface_end_stroke`
                // exists to prevent on the voxel side).
                self.sculpt.poly_grab = None;
                self.sculpt.stroke_active = false;
                self.mark_dirty(DirtyFlags::RENDER);
            } else {
                // Hover: refresh the surface cursor hit so the ring sits ON the mesh
                // (Blender paint_cursor parity — see poly_update_cursor_hit).
                self.poly_update_cursor_hit(response, rect);
            }
            return;
        }
        // NEW voxel room: when a voxel object exists, route to the density-grid sculpt
        // path (Surface Nets mesh) instead of the legacy cpu_volume SDF raymarch path.
        if self.sculpt.voxel.is_some() {
            let sculpting = self.tool.work_mode == WorkMode::Sculpt
                && !self.tool.current.is_gizmo() && !self.tool.current.is_transform();
            if sculpting && response.dragged_by(egui::PointerButton::Primary) {
                time_block!("handle_voxel_sculpt_input", self.handle_voxel_sculpt_input(response, rect, shift, ctrl, alt));
                self.sculpt.stroke_active = true;
            } else if self.sculpt.stroke_active {
                // ---- SURFACE MODE release (3D-Coat Surface sculpting) ----
                // Surface strokes never dispatched density/atlas work, so the whole density release
                // machinery below is skipped. Move commits its finished mesh-grab into the chunk
                // verts (same soft-round falloff as the live shader), then ONE UndoKind::Surface
                // entry restores the affected chunks' pre-stroke vertex arrays on Ctrl+Z.
                if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_mode) {
                    if self.sculpt.voxel.as_ref().map_or(false, |v| v.mesh_grab.is_active()) {
                        self.voxel_surface_commit_move_grab();
                    }
                    self.sdf_stroke_interpolator.reset();
                    self.voxel_surface_commit_stroke();
                    if let Some(v) = self.sculpt.voxel.as_mut() {
                        v.stroke_aabb = None;
                        v.move_prev_pos = None;
                        v.snake_anchor = None; // end the Snake Hook tube (fresh anchor next stroke)
                        clear_voxel_stroke_path_latch(&mut v.stroke_uses_atlas);
                    }
                    self.sculpt.stroke_active = false;
                    self.mark_dirty(DirtyFlags::RENDER);
                    return;
                }
                // Stroke ended → commit ONE undo entry for the whole stroke. First drain any in-flight
                // async GPU brush batch (the final drag frame's stamps may still be on the GPU) so its
                // pre-values are recorded BEFORE take() — otherwise the last edits would be missing from
                // the undo entry (a silent partial undo). poll() collected it already in the normal
                // case; this is the mandatory release barrier.
                time_block!("release:voxel_brush_collect", self.voxel_brush_drain());
                // Flush interpolator tail + any deferred budget dabs (one last apply before polish).
                {
                    let r = self.sculpt.voxel.as_ref().map(|v| v.radius).unwrap_or(0.2);
                    // Dots / Drag Dot already emitted every pointer sample — no chord tail.
                    let mut tail = if self.sculpt.stroke_method.effective().interpolates() {
                        self.sdf_stroke_interpolator.flush(r)
                    } else {
                        Vec::new()
                    };
                    if let Some(v) = self.sculpt.voxel.as_mut() {
                        let fresh_count = tail.len() as u32;
                        let dynamics = core_math::sculpt_layer::BrushDynamics {
                            jitter: v.dyn_jitter,
                            scatter: v.dyn_scatter,
                            random_rotation: v.dyn_random_rotation,
                            random_strength: v.dyn_random_strength,
                            random_radius: v.dyn_random_radius,
                            stroke_noise: v.dyn_stroke_noise,
                        };
                        let seed = brush_dynamics_seed_at(v.dyn_stroke_seed, v.dyn_dab_index);
                        apply_fresh_dab_dynamics_and_prepend_deferred(
                            &mut tail,
                            &mut v.deferred_stamps,
                            r,
                            dynamics,
                            seed,
                        );
                        v.dyn_dab_index = v.dyn_dab_index.wrapping_add(fresh_count);
                    }
                    if !tail.is_empty() {
                        let release_uses_atlas = self.sculpt.voxel.as_ref().map_or(false, |v| {
                            v.stroke_uses_atlas == Some(true)
                        });
                        if release_uses_atlas {
                            // Atlas remains the sole source of truth until all final dabs and their
                            // first-touch snapshots have been submitted.
                            if !self.voxel_dispatch_release_tail_atlas(&tail) {
                                // This is only reachable if the GPU context vanished after the stroke
                                // latch was set. Keep the resolved FIFO for retry instead of writing it
                                // into a stale CPU grid and letting readback erase it.
                                if let Some(v) = self.sculpt.voxel.as_mut() {
                                    v.deferred_stamps = tail;
                                }
                            }
                        } else if let Some(v) = self.sculpt.voxel.as_mut() {
                            let symmetry = v.symmetry;
                            let plane_epsilon = v.grid.voxel_size().max(1e-6) * 0.5;
                            expand_voxel_symmetry(&mut tail, symmetry, plane_epsilon);
                            let hardness = v.falloff;
                            let strength = v.strength;
                            let brush = v.brush;
                            // The stroke's LATCHED polarity — same value every mid-stroke
                            // stamp used (Shift/Ctrl/Alt folded). Recomputing from raw
                            // brush_add here dropped Ctrl/Alt at pen-up.
                            let invert = self.sculpt.voxel_stroke_invert;
                            let rec = &mut self.sculpt.voxel_stroke;
                            for pt in &tail {
                                let dab_r = (r * pt.radius_scale).max(1e-4);
                                let dab_s = strength * pt.pressure;
                                v.grid.apply_brush(
                                    brush, pt.position, pt.normal, dab_r, dab_s, invert, hardness,
                                    glam::Vec3::ZERO, Some(rec),
                                );
                            }
                        }
                    }
                    self.sdf_stroke_interpolator.reset();
                }
                // Coat continuous-clay finish on pen-up (box/CPU path):
                //   1) Always light ISO-BAND polish → melts stamp terrace rings (even without Auto Smooth)
                //   2) Optional Auto Smooth Laplacian when enabled (stronger, user knob)
                // Atlas strokes MUST NOT write CPU here (pool is still authority); see atlas branch.
                let atlas_stroke_release = self.sculpt.voxel.as_ref().map_or(false, |v| v.atlas_stroke);
                {
                    use core_math::voxel_sculpt::VoxelBrush;
                    let (do_polish, do_as, as_amt) = if atlas_stroke_release {
                        (false, false, 0.0)
                    } else {
                        self.sculpt.voxel.as_ref().map_or((false, false, 0.0), |v| {
                            // Only soft deposit brushes get iso polish — Erase/Crease/Scrape/Pinch
                            // stay sharp (pen-up was homogenizing every non-Move brush).
                            let deposit = matches!(
                                v.brush,
                                VoxelBrush::Draw
                                    | VoxelBrush::Clay
                                    | VoxelBrush::ClayBuildup
                                    | VoxelBrush::Fill
                                    | VoxelBrush::Flatten
                                    | VoxelBrush::Inflate
                            );
                            let asmooth = v.auto_smooth && deposit;
                            let amt = (v.auto_smooth_intensity * 0.45).clamp(0.05, 0.5);
                            (deposit, asmooth, amt)
                        })
                    };
                    if do_polish || do_as {
                        if let (Some(region), Some(v)) = (
                            self.sculpt.voxel.as_ref().and_then(|v| v.stroke_aabb).filter(|&aabb| {
                                voxel_aabb_volume(aabb) <= VOXEL_ATLAS_MAX_SCRATCH_VOXELS
                            }),
                            self.sculpt.voxel.as_mut(),
                        ) {
                            let rec = &mut self.sculpt.voxel_stroke;
                            if do_polish {
                                // 2 mild iso-shell passes — Coat-like continuous surface after long strokes.
                                let iters = if v.grid.res >= 512 { 2u32 } else { 1 };
                                v.grid.polish_iso_band(region, iters, 0.40, Some(rec));
                            }
                            if do_as {
                                v.grid.relax_region(region, 1, as_amt, true, Some(rec));
                            }
                            self.voxel_mark_chunks_in_region(region);
                        }
                    }
                }
                // MOVE MESH-GRAB release (Meta-Medium mesh→SDF). The surface MESH was deformed live during the
                // drag (GPU vertex shader, display-only). Two release modes (3D-Coat's "voxelize immediately"):
                //   • DEFERRED / surface mode (default): do NOT re-voxelize now. Accumulate the finished grab's
                //     symmetry-expanded instances into `pending_move_grabs` (kept displaying via the shader),
                //     arm the ~2s auto-voxelize timer, and return — no undo, no drain, no 3s pause. The stack is
                //     baked ONCE on demand (Voxelize button / Enter / 2s idle / buffer full).
                //   • IMMEDIATE (toggle off): today's exact path — voxelize on release + ONE undo + full-drain.
                if self.sculpt.voxel.as_ref().map_or(false, |v| v.mesh_grab.is_active()) {
                    let deferred = self.sculpt.voxel.as_ref().map_or(true, |v| v.move_deferred);
                    if deferred {
                        // Take the grab the SAME way the commit does (consume mesh_grab) but DON'T voxelize.
                        let grab_syms = self.sculpt.voxel.as_mut().map(|v| {
                            let g = std::mem::take(&mut v.mesh_grab);
                            (g.mirror_instances(v.symmetry), g.inner_r, g.outer_r, g.last_drag.length())
                        });
                        if let Some((insts, inner_r, outer_r, drag_len)) = grab_syms {
                            // A grab with no movement records nothing (a click, or a degenerate view plane).
                            if drag_len >= 1e-5 {
                                // OVERFLOW: if pushing these instances would exceed MAX_GRAB_INSTANCES, FIRST
                                // voxelize the existing batch (flush) so the shader cap is never overrun — the
                                // accumulation-buffer-full auto-flush. After the flush the stack is empty.
                                let new_n = insts.len();
                                let would = self.sculpt.voxel.as_ref().map_or(0, |v| v.pending_move_grabs.len()) + new_n;
                                if would > render::camera::MAX_GRAB_INSTANCES {
                                    // Shader cap: spawn bake for the existing stack (async — no block).
                                    // If still full after, skip pushing this release (toast) rather than
                                    // freeze the UI waiting for the worker.
                                    time_block!("release:overflow_flush_pending_move", {
                                        self.voxel_flush_pending_move();
                                    });
                                }
                                let can_push = self.sculpt.voxel.as_ref().map_or(false, |v| {
                                    v.pending_move_grabs.len() + insts.len()
                                        <= render::camera::MAX_GRAB_INSTANCES
                                        && v.pending_move_bake.is_none()
                                }) || self.sculpt.voxel.as_ref().map_or(false, |v| {
                                    // Allow push while bake in flight only if room remains under cap.
                                    v.pending_move_grabs.len() + insts.len()
                                        <= render::camera::MAX_GRAB_INSTANCES
                                });
                                if can_push {
                                    if let Some(v) = self.sculpt.voxel.as_mut() {
                                        let group_n = insts.len();
                                        for (anchor, drag) in insts {
                                            v.pending_move_grabs.push(crate::PendingGrab { anchor, drag, inner_r, outer_r });
                                        }
                                        v.pending_move_group_sizes.push(group_n);
                                        v.pending_move_redo.clear();
                                        v.move_voxelize_due = Some(
                                            std::time::Instant::now() + std::time::Duration::from_secs(2),
                                        );
                                    }
                                } else {
                                    self.toast("Move stack full — wait for voxelize, then retry");
                                }
                            }
                        }
                        if let Some(v) = self.sculpt.voxel.as_mut() {
                            v.stroke_aabb = None;
                            v.move_prev_pos = None;
                            clear_voxel_stroke_path_latch(&mut v.stroke_uses_atlas);
                        }
                        self.sculpt.stroke_active = false;
                        self.mark_dirty(DirtyFlags::RENDER);
                        return;
                    }
                    // IMMEDIATE (3D-Coat Voxelize immediately): spawn ASYNC local mesh→SDF only.
                    // NEVER block-land on the UI thread (owner freeze was block_land 3s + GROW thrash).
                    // Live preview stays on the GPU mesh shader until land; undo lands with the bake.
                    let _delta = time_block!("release:voxel_commit_move_grab", self.voxel_commit_move_grab());
                    if let Some(v) = self.sculpt.voxel.as_mut() {
                        v.stroke_aabb = None;
                        v.move_prev_pos = None;
                        clear_voxel_stroke_path_latch(&mut v.stroke_uses_atlas);
                    }
                    self.sculpt.stroke_active = false;
                    self.mark_dirty(DirtyFlags::RENDER);
                    return;
                }
                // LEVEL-SET MOVE / Smudge release: field already holds warp(snapshot, total_drag).
                // Full local redistance (relax) over the capsule dirty region, then ONE density undo.
                if self.sculpt.voxel.as_ref().map_or(false, |v| v.elastic_grab.is_active()) {
                    let region = self.sculpt.voxel.as_ref().and_then(|v| v.stroke_aabb);
                    // Release-quality local redistance (proposal: full local repair after movement).
                    if let Some(region) = region {
                        if let Some(v) = self.sculpt.voxel.as_mut() {
                            v.grid.grab_redistance_region(region);
                            let _ = v.grid.take_dirty(); // redistance dirties; region already tracked
                        }
                        self.voxel_mark_chunks_in_region(region);
                    }
                    let delta = self.sculpt.voxel.as_mut().map(|v| v.elastic_grab.finish()).unwrap_or_default();
                    if !delta.is_empty() {
                        let evicted = self.sculpt.voxel_undo_stack.push(delta);
                        self.sculpt.voxel_undo_order.push_back(UndoKind::Cpu);
                        prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, UndoKind::Cpu, evicted);
                        self.sculpt.voxel_redo_stack.clear();
                        self.sculpt.voxel_grid_redo_stack.clear();
                        self.sculpt.voxel_redo_order.clear();
                    }
                    if let Some(v) = self.sculpt.voxel.as_mut() {
                        v.stroke_aabb = None;
                        v.move_prev_pos = None;
                        clear_voxel_stroke_path_latch(&mut v.stroke_uses_atlas);
                    }
                    self.sculpt.stroke_active = false;
                    self.mark_dirty(DirtyFlags::RENDER);
                    return;
                }
                let atlas_stroke = self.sculpt.voxel.as_ref().map_or(false, |v| v.atlas_stroke);
                if atlas_stroke {
                    // Phase A.2: finalise GPU brick-snapshot undo (no readback for undo). Then bring the
                    // CPU grid up to the GPU pool BEFORE any remesh/AutoSmooth/resample — otherwise the
                    // surface vanishes on release (mesh/remesh read stale pre-stroke CPU) and the next
                    // atlas sync can upload that stale CPU over the live pool (stroke "erases itself").
                    let stroke_region = self.sculpt.voxel.as_ref().and_then(|v| v.stroke_aabb);
                    // Pen-up polish/relax over a long diagonal's global AABB is both volumetric work
                    // and impossible to undo from stamp-only snapshots. Keep the finish pass for local
                    // strokes only. Before finishing the builder, snapshot every brick that this bounded
                    // pass can write; large swept strokes retain their live GPU brush result unchanged.
                    let finish_region = stroke_region.filter(|&aabb| {
                        voxel_aabb_volume(aabb) <= VOXEL_ATLAS_MAX_SCRATCH_VOXELS
                    });
                    if let Some(region) = finish_region {
                        let brick_res = self.sculpt.voxel.as_ref()
                            .map_or(0, |v| v.grid.density.brick_res());
                        let finish_bricks = voxel_bricks_for_regions([region], brick_res);
                        let report = self.sculpt.voxel.as_mut().map_or_else(Vec::new, |v| {
                            v.grid.density.materialize_bricks_report(&finish_bricks)
                        });
                        self.voxel_sync_mesh_atlas();
                        if let Some(gpu) = self.gpu.as_mut() {
                            let GpuCtx {
                                device,
                                queue,
                                voxel_mesh_atlas,
                                voxel_atlas_undo_building,
                                ..
                            } = gpu;
                            if let (Some(atlas), Some(builder)) =
                                (voxel_mesh_atlas.as_ref(), voxel_atlas_undo_building.as_mut())
                            {
                                builder.capture(device, queue, &atlas.pool_buf, &report);
                            }
                        }
                    }
                    let (captured_bricks, brick_res, grid_res) = self.sculpt.voxel.as_ref()
                        .map(|v| {
                            let captured = self.gpu.as_ref()
                                .and_then(|g| g.voxel_atlas_undo_building.as_ref())
                                .map(|b| b.captured_brick_indices().to_vec())
                                .unwrap_or_default();
                            (captured, v.grid.density.brick_res(), v.grid.res)
                        })
                        .unwrap_or_default();
                    let mut affected = Vec::new();
                    for region in voxel_regions_for_bricks(&captured_bricks, brick_res, grid_res) {
                        affected.extend(self.voxel_chunks_for_region_linear(region));
                    }
                    affected.sort_unstable();
                    affected.dedup();
                    let entry = self.gpu.as_mut()
                        .and_then(|g| g.voxel_atlas_undo_building.take())
                        .and_then(|b| b.finish(affected));
                    if let Some(entry) = entry {
                        let evicted = self.gpu.as_mut().unwrap().voxel_atlas_undo.push_undo(entry);
                        self.sculpt.voxel_undo_order.push_back(UndoKind::Atlas);
                        // The ring evicted its `evicted` OLDEST entries to stay in budget; drop that many
                        // front Atlas markers so the order stream can't outlive its entries (else undo
                        // pops a marker whose entry is gone → undo silently stops early — finding #4).
                        prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, UndoKind::Atlas, evicted);
                        self.sculpt.voxel_redo_order.clear(); // a new stroke truncates redo
                    }
                    // GPU pool → CPU: prefer STROKE-REGION handle readback at high res (not whole shell).
                    // Drop atlas_stroke BEFORE Auto Smooth / remesh sync so voxel_sync_mesh_atlas treats
                    // CPU as authoritative again (otherwise AS dirties are skipped as "GPU authority").
                    time_block!("release:materialize_cpu_from_atlas", {
                        if !self.materialize_cpu_from_atlas_bricks(&captured_bricks) {
                            self.materialize_cpu_from_atlas_prefer(stroke_region);
                        }
                    });
                    if let Some(v) = self.sculpt.voxel.as_mut() {
                        v.atlas_stroke = false;
                    }
                    // Coat finish + Pinch redistance + Auto Smooth now that CPU matches the stroke.
                    {
                        use core_math::voxel_sculpt::VoxelBrush;
                        let (do_pinch_relax, do_polish, do_as, as_amt) =
                            self.sculpt.voxel.as_ref().map_or((false, false, false, 0.0), |v| {
                                let pinch = matches!(v.brush, VoxelBrush::Pinch) || v.stack_gather;
                                let deposit = matches!(
                                    v.brush,
                                    VoxelBrush::Draw
                                        | VoxelBrush::Clay
                                        | VoxelBrush::ClayBuildup
                                        | VoxelBrush::Fill
                                        | VoxelBrush::Flatten
                                        | VoxelBrush::Inflate
                                );
                                let asmooth = v.auto_smooth && deposit;
                                let amt = (v.auto_smooth_intensity * 0.45).clamp(0.05, 0.5);
                                (pinch, deposit, asmooth, amt)
                            });
                        if let Some(region) = finish_region {
                            if do_pinch_relax || do_polish || do_as {
                                if let Some(v) = self.sculpt.voxel.as_mut() {
                                    let rec = &mut self.sculpt.voxel_stroke;
                                    if do_pinch_relax {
                                        // One mild pass — mid-stroke skip made Pinch fluid on atlas.
                                        v.grid.relax_region(region, 1, 0.12, true, Some(rec));
                                    }
                                    if do_polish {
                                        let iters = if v.grid.res >= 512 { 2u32 } else { 1 };
                                        v.grid.polish_iso_band(region, iters, 0.40, Some(rec));
                                    }
                                    if do_as {
                                        v.grid.relax_region(region, 1, as_amt, true, Some(rec));
                                    }
                                }
                                // Push polished density back to GPU atlas (raymarch + remesh match).
                                self.voxel_sync_mesh_atlas();
                            }
                        }
                    }
                    // Commit mesh of the stroke region (raymarch stays live ≥512; mesh used for pick/export).
                    if let Some(aabb) = finish_region {
                        self.voxel_mark_chunks_in_region(aabb);
                    }
                    if let Some(v) = self.sculpt.voxel.as_mut() {
                        v.stroke_aabb = None; // do NOT compact — handles must stay stable for undo
                    }
                } else {
                    // Box-path Pinch relax BEFORE undo commit so first-touch voxels enter the delta
                    // (was: take() then relax → undo missed relax-only voxels).
                    {
                        use core_math::voxel_sculpt::VoxelBrush;
                        let do_pinch = self.sculpt.voxel.as_ref().map_or(false, |v| {
                            matches!(v.brush, VoxelBrush::Pinch) || v.stack_gather
                        });
                        if do_pinch {
                            if let Some(region) = self.sculpt.voxel.as_ref()
                                .and_then(|v| v.stroke_aabb)
                                .filter(|&aabb| {
                                    voxel_aabb_volume(aabb) <= VOXEL_ATLAS_MAX_SCRATCH_VOXELS
                                })
                            {
                                if let Some(v) = self.sculpt.voxel.as_mut() {
                                    let rec = &mut self.sculpt.voxel_stroke;
                                    v.grid.relax_region(region, 1, 0.12, true, Some(rec));
                                }
                                self.voxel_mark_chunks_in_region(region);
                            }
                        }
                    }
                    // Box / Dense path (CPU-authoritative): the collect above already wrote every edit
                    // into the CPU grid + recorded the delta. Commit it as ONE undo entry.
                    if !self.sculpt.voxel_stroke.is_empty() {
                        let delta = self.sculpt.voxel_stroke.take();
                        let evicted = self.sculpt.voxel_undo_stack.push(delta);
                        self.sculpt.voxel_undo_order.push_back(UndoKind::Cpu);
                        // Prune `evicted` front Cpu markers the stack just dropped (see finding #4).
                        prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, UndoKind::Cpu, evicted);
                        self.sculpt.voxel_redo_stack.clear(); // a new stroke truncates redo
                        self.sculpt.voxel_redo_order.clear();
                    }
                    // Sparse compaction: demote any brick this stroke filled uniformly solid/empty back
                    // to an implicit tag (recycles its arena slot) so the brick pool doesn't grow
                    // unboundedly as a session builds material. No-op for a Dense grid. Runs after the
                    // final mesh of the stroke landed (collect above), so a freed brick is never read by
                    // an in-flight mesh. NOT done for the atlas path (handles must stay stable for undo).
                    //
                    // CRITICAL: also skip it while ANY atlas-undo entry is live. compact_aabb frees POOL
                    // handles; a live AtlasUndoEntry references stable handles, so freeing one lets a later
                    // atlas Draw recycle it for a DIFFERENT brick — and undoing the old entry would then
                    // restore its snapshot into the wrong (now-live) pool slot: silent GPU-pool corruption.
                    // Compaction is a transient memory optimization (slots are reused by later edits; a full
                    // GC happens at the next grid-identity voxel_undo_reset), so deferring it while undo
                    // history is live is lossless. Enforced HERE at the single compact call site.
                    let atlas_undo_live = self.gpu.as_ref().map_or(false, |g| {
                        g.voxel_atlas_undo.has_entries() || g.voxel_atlas_undo_building.is_some()
                    });
                    if let Some(aabb) = self.sculpt.voxel.as_mut().and_then(|v| v.stroke_aabb.take()) {
                        // ONE authoritative full mesh of the whole stroke region on RELEASE. The
                        // zero-remesh preview paths (COLOUR recolor / DISPLACEMENT reproject) wrote the
                        // chunks in place during the drag; recolor is byte-identical to mesh_region and
                        // reproject matches it away from drift, but re-marking the stroke region here and
                        // letting the next voxel_flush_dirty_chunks (stroke_active==false → preview gates
                        // OFF → full mesh_region_with_cells) re-extract it guarantees the COMMITTED
                        // surface is the canonical one regardless of which preview ran. Bounded by the
                        // stroke AABB (cheap), idempotent for the density brushes that already meshed
                        // correctly during the drag. Mirrors the Move/Smudge elastic-grab release remesh.
                        self.voxel_mark_chunks_in_region(aabb);
                        if !atlas_undo_live {
                            if let Some(v) = self.sculpt.voxel.as_mut() { v.grid.density.compact_aabb(aabb); }
                        }
                    }
                }
                self.sculpt.stroke_active = false;
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    v.stroke_last_surface = None;
                    v.stroke_prev_pos = None;
                    clear_voxel_stroke_path_latch(&mut v.stroke_uses_atlas);
                }
                // The final canonical mesh of the stroke region was queued above; the next
                // voxel_flush_dirty_chunks (preview gates off) re-extracts it from the committed field.
            }
            return;
        }
        if self.tool.work_mode == WorkMode::Sculpt && !self.tool.current.is_gizmo() && !self.tool.current.is_transform()
           && response.dragged_by(egui::PointerButton::Primary) {
            // VS3: reset the stroke resampler once at stroke start so a residual from a
            // previous stroke can't bleed into this one.
            if response.drag_started_by(egui::PointerButton::Primary) {
                self.sdf_stroke_interpolator.reset();
            }
            if let Some(mp) = response.interact_pointer_pos() {
                // VS11: canonical ray builder — handles orthographic projection. The
                // old hand-rolled ray was perspective-only, mis-picking in ortho view.
                let (cam_pos, rd) = self.cam_ray(mp, rect);
                if let Some(hit) = self.cache.cpu_volume.raycast(cam_pos, rd) {
                    let n = self.cache.cpu_volume.normal_at(hit);
                    let mut active_brush = self.brush;
                    if shift { active_brush.brush_type = BrushType::Smooth; }
                    else if ctrl {
                        active_brush.brush_type = active_brush.brush_type.inverted();
                    }
                    // VS3: resample the stroke path into evenly-spaced stamps so a fast
                    // pointer move produces a continuous band instead of beads/gaps.
                    let sample = core_math::sculpt_layer::StrokeSample::new(hit, n, 1.0);
                    let mut pts = self.sdf_stroke_interpolator.push(sample.clone(), active_brush.size);
                    // Frame-budget guard: a pathological teleport (alt-tab, then back) could
                    // emit a huge number of stamps — cap and fall back to a single stamp.
                    const MAX_STAMPS_PER_FRAME: usize = 64;
                    if pts.len() > MAX_STAMPS_PER_FRAME {
                        pts = vec![sample];
                    }
                    let mut prev_pos: Option<Vec3> = None;
                    for pt in &pts {
                        // Snapshot the affected region BEFORE stamping (undo). or_insert
                        // keeps the earliest pre-stroke value across all stamps this frame.
                        if let Some([cx, cy, cz, mx, my, mz]) = self.cache.cpu_volume.brush_aabb(pt.position, active_brush.size) {
                            for (idx, val) in self.cache.cpu_volume.snapshot_region(cx, cy, cz, mx, my, mz) {
                                self.sculpt.current_stroke.entry(idx).or_insert(val);
                            }
                        }
                        let stroke_dir = match prev_pos {
                            Some(p) => {
                                let d = (pt.position - p).normalize_or_zero();
                                if d.length_squared() > 1e-12 { d } else { Vec3::Z }
                            }
                            None => Vec3::Z,
                        };
                        self.cache.cpu_volume.apply_brush_advanced(pt.position, pt.normal, &active_brush, stroke_dir);
                        prev_pos = Some(pt.position);
                    }
                }
            }
            self.sculpt.stroke_active = true;
        } else if self.sculpt.stroke_active && self.tool.work_mode == WorkMode::Sculpt {
            if !self.sculpt.current_stroke.is_empty() {
                let delta: Vec<(u32, f32)> = self.sculpt.current_stroke.drain().collect();
                // VS5: eviction (byte + count budget) is owned by VoxelUndoStack::push.
                self.sculpt.voxel_undo_stack.push(delta);
                // VS4: a new committed stroke truncates the redo branch.
                self.sculpt.voxel_redo_stack.clear();
            }
            self.sculpt.stroke_active = false;
            // VS3: clear the resampler so a residual can't leak into the next stroke.
            self.sdf_stroke_interpolator.reset();
        }
        if self.cache.cpu_volume.dirty_region.is_some() { self.mark_dirty(DirtyFlags::RENDER); }
    }


    /// Voxel sculpt stroke. Phase 1: the raw cursor hit is pushed through the
    /// StrokeInterpolator so a fast drag resamples into evenly-spaced stamps (no
    /// beads/gaps); all stamps apply, then the mesh is re-extracted ONCE per frame.
    pub(super) fn handle_voxel_sculpt_input(&mut self, response: &egui::Response, rect: egui::Rect, shift: bool, ctrl: bool, alt: bool) {
        use core_math::voxel_sculpt::VoxelBrush;
        if !response.dragged_by(egui::PointerButton::Primary) { return; }
        // EFFECTIVE brush for this stroke = the ONE resolved value every gate + both dispatch paths
        // (density atlas/box AND surface mesh) must consume. Shift temporarily substitutes Smooth for
        // the active brush — including when the active brush is Move (a mesh-grab). Resolving it ONCE
        // here (instead of re-deriving `if shift { Smooth }` at each gate, or keying gates off the raw
        // `v.brush`) is what makes Shift+Move smooth like every other brush+Shift: the Move mesh-grab
        // early-returns and its no-hit-fallback resurrection must see the effective brush, not the base.
        let base_brush_now = self.sculpt.voxel.as_ref().map_or(VoxelBrush::Draw, |v| v.brush);
        let eff_brush = if shift { VoxelBrush::Smooth } else { base_brush_now };
        let eff_is_move = matches!(eff_brush, VoxelBrush::Move);
        // Pause density sculpt while a resolution rebuild is in flight. MOVE is display-only
        // (shader displace) so it may continue on the still-visible mesh — blocking Move here
        // was a major "tool freezes after every free pull" feel killer.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.pending_rebuild.is_some()) {
            if !eff_is_move { return; }
        }
        // Pause density strokes while an async Move bake lands (race with land write). Mesh-grab
        // Move is display-only until release — MUST stay interactive or the user can only do ONE
        // stroke per bake (owner regression). Release queues into pending_move_grabs if a bake is
        // already in flight; land drains the baked prefix and re-flushes the rest.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.pending_move_bake.is_some()) {
            if !eff_is_move {
                return;
            }
        }
        // Reset the resampler at stroke start so no residual leaks from the last stroke.
        if response.drag_started_by(egui::PointerButton::Primary) {
            // SURFACE-MODE GATE: a density brush must never fight a stale grid. If deferred Move grabs are
            // pending and THIS stroke is NOT another Move (Shift→Smooth, Draw, Erase, …), voxelize the pending
            // stack FIRST so the brush edits the baked surface, not the pre-move grid. A Move stroke keeps
            // accumulating (it adds to the same stack), so it does not flush.
            let starting_brush = eff_brush;
            if !matches!(starting_brush, VoxelBrush::Move)
                && self.sculpt.voxel.as_ref().map_or(false, |v| !v.pending_move_grabs.is_empty()) {
                // ASYNC: the flush bakes on a worker now — this stroke would edit the PRE-move grid,
                // so spawn the bake and IGNORE the stroke; re-stroke once it lands (~a second).
                self.voxel_flush_pending_move();
                self.toast("Voxelizing move…");
                return;
            }
            // SURFACE MODE fail-closed gate: density-native brushes (Clay/Fill/Scrape/…) have no
            // honest vertex analog — they are greyed in the UI; refuse here too (never a silent
            // wrong result if reached via shortcut/preset).
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_mode)
                && !crate::voxel_surface_sculpt::surface_brush_supported(starting_brush)
            {
                self.toast("This brush is density-only — switch to Voxel mode to use it");
                return;
            }
            // Snake Hook is SURFACE-mode only (moving-anchor vertex tube). It has no density version,
            // so refuse it in Voxel mode (fail-closed; greyed in the UI with the same message).
            if self.sculpt.voxel.as_ref().map_or(false, |v| !v.surface_mode)
                && matches!(starting_brush, VoxelBrush::SnakeHook)
            {
                self.toast("Snake Hook is surface-mode only — switch to Surface mode to use it");
                return;
            }
            // STROKE-LIFECYCLE SELF-HEAL (the cross-stroke stale-snapshot class, 2026-07-23):
            // if ANY exit path missed the release commit (SCENE-dirty force-cleared stroke_active,
            // a focus loss ate the release frame, …), the previous stroke's first-touch snapshots
            // are still pending. Commit them as their OWN undo entry NOW — the new stroke must
            // NEVER inherit them: Snake Hook/Move treat this map as PRISTINE and would REWIND
            // every leaked chunk to the OLD stroke's start ("vertices reversing", owner-reported),
            // and the release commit would push a two-strokes-old undo entry.
            if self.sculpt.surface_end_stroke() {
                eprintln!("[SURFACE] stroke-start heal: pending stroke snapshot committed (missed release path)");
            }
            let spacing = self.sculpt.voxel.as_ref().map_or(0.10, |v| v.stroke_spacing.clamp(0.03, 0.50));
            self.sdf_stroke_interpolator.spacing = spacing;
            self.sdf_stroke_interpolator.reset();
            self.sculpt.airbrush_last = None;
            // Reset the undo recorder's dedup mask for this stroke (sized to the current grid).
            let res = self.sculpt.voxel.as_ref().map_or(0, |v| v.grid.res);
            self.sculpt.voxel_stroke.begin(res);
            // CRITICAL: if CPU density is behind the GPU atlas (previous atlas stroke), raycast
            // hits the OLD field or misses → zero stamps ("can't sculpt on the sphere").
            // Heal CPU from the atlas ONCE at stroke start so pick + deposit see live clay.
            let need_heal = self.sculpt.voxel.as_ref().map_or(false, |v| v.cpu_grid_stale);
            if need_heal {
                self.materialize_cpu_from_atlas();
            }
            // LATCH the brush path for the WHOLE stroke (InteractionEpisode pattern): GPU-resident vs
            // box must not flip mid-stroke — mixing CPU- and GPU-authoritative frames would corrupt the
            // atlas accumulator + undo.
            if let Some(v) = self.sculpt.voxel.as_mut() {
                // Move is mesh-grab (not density atlas). Pinch/+Gather use GPU Advect on atlas —
                // relax is deferred to pen-up (was per-frame box readback + full-union relax = SUPER SLOW).
                // Mask forces the BOX GPU path (not atlas): mask is sampled from a box-local buffer.
                // ATLAS-FIRST: sparse + GPU brush → resident atlas whenever possible.
                let mask_blocks_atlas = v.mask_paint || v.grid.has_mask() || v.auto_mask.is_on();
                // Effective brush: a Shift-smooth over a Move base is a normal Smooth stroke, not a
                // mesh-grab — it must NOT force the Move box-post path (which disables the atlas).
                let needs_box_post = eff_is_move;
                v.stroke_uses_atlas = Some(
                    !v.surface_mode // surface strokes never touch the density atlas
                        && v.gpu_brush
                        && v.gpu_brush_atlas
                        && v.grid.density.is_sparse()
                        && !needs_box_post
                        && !mask_blocks_atlas,
                );
                v.move_prev_pos = None; // fresh Move stroke: the first stamp warps nothing (no prev)
                v.first_point_normal = None; // Original Normal re-latches
                v.first_point_pos = None;
                v.average_normal_sum = glam::Vec3::ZERO;
                v.average_normal_count = 0;
                v.stroke_prev_pos = None; // Brush / Tangent path restarts
                v.stroke_last_surface = None; // long-stroke stick re-latches on first accepted hit
                v.deferred_stamps.clear();
                // One random stream per interaction episode. Per-frame wall-clock seeds made the
                // same tablet gesture jitter differently when GPU load changed the frame partition.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                v.dyn_stroke_seed = (now as u32) ^ ((now >> 32) as u32);
                v.dyn_dab_index = 0;
                // Drop any stale mesh-grab from an interrupted prior stroke (e.g. a body switch /
                // remesh that ended the drag without a release) so this stroke re-latches cleanly
                // against the CURRENT chunks — never against dropped/old mesh ids.
                v.mesh_grab.cancel();
                v.snake_anchor = None; // fresh Snake Hook anchor for this stroke
            }
        }
        let Some(mp_raw) = response.interact_pointer_pos() else { return };
        // LazyMouse stabilizer: brush trails the cursor by a rope of `lazy * 70` screen px.
        // MOVE skips LazyMouse entirely — 3D-Coat free Move must track the cursor 1:1; any lag
        // reads as "tool is broken / sticky / late".
        let mp = {
            let started = response.drag_started_by(egui::PointerButton::Primary);
            let is_move = eff_is_move;
            let lazy = if is_move { 0.0 } else { self.sculpt.voxel.as_ref().map_or(0.0, |v| v.lazy) };
            let radius_px = lazy * 70.0;
            if radius_px < 1.0 {
                if let Some(v) = self.sculpt.voxel.as_mut() { v.lazy_pos = None; }
                mp_raw
            } else {
                let cur = glam::Vec2::new(mp_raw.x, mp_raw.y);
                let lp0 = if started { cur } else {
                    self.sculpt.voxel.as_ref().and_then(|v| v.lazy_pos).unwrap_or(cur)
                };
                let d = cur - lp0;
                let dist = d.length();
                let lp = if dist > radius_px { lp0 + d * (1.0 - radius_px / dist) } else { lp0 };
                if let Some(v) = self.sculpt.voxel.as_mut() { v.lazy_pos = Some(lp); }
                egui::pos2(lp.x, lp.y)
            }
        };
        let (mut ro, mut rd) = self.cam_ray(mp, rect);

        // P2: transform the world ray into the voxel object's LOCAL space so sculpting
        // follows the object after the gizmo moves/rotates/scales its node. The mesh is
        // extracted in local space and placed in the world by the node transform, so the
        // sculpt must operate in that same local space.
        let vnode = self.sculpt.voxel.as_ref().map(|v| v.node);
        if let Some(w) = vnode.map(|n| self.scene.scene_graph.world_transform(n)) {
            let inv = w.inverse();
            ro = inv.transform_point3(ro);
            rd = inv.transform_vector3(rd).normalize_or_zero();
        }

        // Surface hit + normal for the brush.
        // SIMPLE + RELIABLE: density raycast OR gpu pick OR mid-stroke plane stick.
        // Stamp center on the iso (no outward offset). No normal flip games.
        let stroke_dragging = response.dragged_by(egui::PointerButton::Primary);
        let hit_info = self.sculpt.voxel.as_ref().and_then(|v| {
            let prev = v.stroke_last_surface;
            let max_jump = (v.radius * 3.0).max(v.grid.voxel_size() * 12.0);

            let cpu = v.grid.raycast(ro, rd).map(|h| {
                let n = v.grid.normal_at(h);
                (h, if n.length_squared() > 1e-12 { n.normalize_or_zero() } else { glam::Vec3::Y })
            });
            let gpu = v.last_surface_hit.filter(|(_, n)| n.length_squared() > 1e-12)
                .map(|(h, n)| (h, n.normalize_or_zero()));

            // The atlas is authoritative during a resident stroke. Never let its older CPU mirror
            // win merely because the stale surface is closer (especially visible while carving).
            let max_gpu_lateral = (v.radius * 1.25).max(v.grid.voxel_size() * 4.0);
            // Surface mode: once mesh edits exist the density raycast hits the STALE pre-edit
            // field — treat it like the atlas-stale case and prefer the rendered-surface pick.
            let density_stale = v.cpu_grid_stale || v.surface_dirty_region.is_some();
            let mut raw = choose_voxel_surface_hit(
                cpu,
                gpu,
                density_stale,
                ro,
                rd,
                max_gpu_lateral,
            );

            // Mid-stroke: stick on miss so long strokes don't die at silhouette.
            if raw.is_none() && stroke_dragging {
                if let Some((ph, pn)) = prev {
                    let pn = pn.normalize_or_zero();
                    if pn.length_squared() > 1e-12 {
                        let denom = rd.dot(pn);
                        if denom.abs() > 1e-5 {
                            let t = (ph - ro).dot(pn) / denom;
                            if t > 0.0 {
                                let p = ro + rd * t;
                                if (p - ph).length() <= max_jump * 2.0 {
                                    raw = Some((p, pn));
                                }
                            }
                        }
                    }
                }
            }

            raw.map(|(h, n_raw)| {
                let n_d = if v.cpu_grid_stale || v.surface_dirty_region.is_some() {
                    glam::Vec3::ZERO
                } else { v.grid.normal_at(h) };
                let n = if n_d.length_squared() > 1e-12 {
                    n_d.normalize_or_zero()
                } else if n_raw.length_squared() > 1e-12 {
                    n_raw.normalize_or_zero()
                } else {
                    glam::Vec3::Y
                };
                (h, n, v.radius, v.strength, v.brush, v.mesh_id)
            })
        });
        let (hit, normal, mut radius, strength, base_brush, mid) = match hit_info {
            Some((h, n, r, s, b, m)) => (h, n, r, s, b, m),
            None => {
                // Mid-drag past silhouette: keep Move/Smudge alive on the grab VIEW PLANE under the
                // mouse. Density brushes: no hit → no stroke (stick already tried above).
                // Effective brush: only a live Move/Smudge grab (never a Shift-smooth over a Move base)
                // may coast on the view plane past the silhouette.
                let eff_is_smudge = matches!(eff_brush, VoxelBrush::Smudge);
                let v = match self.sculpt.voxel.as_ref() {
                    Some(v) if v.mesh_grab.is_active() && eff_is_move => v,
                    Some(v) if v.elastic_grab.is_active() && (eff_is_move || eff_is_smudge) => v,
                    _ => return,
                };
                let (anchor, plane_n, first_n) = if v.mesh_grab.is_active() {
                    (v.mesh_grab.anchor, v.mesh_grab.plane_n, v.mesh_grab.normal)
                } else {
                    let n = if v.elastic_grab.surface_n.length_squared() > 1e-8 {
                        v.elastic_grab.surface_n
                    } else {
                        v.elastic_grab.plane_n
                    };
                    (v.elastic_grab.anchor, v.elastic_grab.plane_n, n)
                };
                let denom = rd.dot(plane_n);
                let p = if denom.abs() > 1e-5 {
                    let t = (anchor - ro).dot(plane_n) / denom;
                    if t > 0.0 { ro + rd * t } else { anchor }
                } else {
                    anchor
                };
                let n = if first_n.length_squared() > 1e-8 { first_n } else { plane_n };
                (p, n, v.radius, v.strength, v.brush, v.mesh_id)
            }
        };
        let base_radius = radius.max(1e-6);
        // Latch last stable surface for next frame's stick + oriented ring continuity.
        if let Some(v) = self.sculpt.voxel.as_mut() {
            if normal.length_squared() > 1e-12 {
                v.stroke_last_surface = Some((hit, normal));
            }
        }
        // ---- PEN / TABLET PRESSURE (single producer) ----
        // Read the latest stylus force once. Radius is resolved here because it affects interpolator
        // spacing/AABBs. Strength stays as the base tool value: the routed pressure factor travels in
        // StrokeSample and is multiplied exactly once by both CPU and GPU stamp producers.
        // SOURCE: egui forwards `Event::Touch { force: Option<f32> }` (0..1) from egui_winit, which reads
        // winit's pen/touch `Force`. A touchscreen / WM_POINTER pen delivers a real force; an ordinary
        // mouse — and on Windows a graphics-tablet pen that arrives as plain mouse events (the common case,
        // since neither eframe nor winit-by-default opt in to WM_POINTER via EnableMouseInPointer) — emits
        // NO force, so `pressure` falls back to 1.0 and the brush behaves exactly as with pressure off.
        let (pressure_enabled, pressure_to_strength, pressure_to_radius, pressure_radius_amount) =
            self.sculpt.voxel.as_ref().map_or((false, false, false, 0.60), |v| {
                (
                    v.pressure_enabled,
                    v.pressure_to_strength,
                    v.pressure_to_radius,
                    v.pressure_radius_amount,
                )
            });
        let pen_pressure = if pressure_enabled {
            // Latest force among this frame's touch events (most recent wins); 1.0 if none reported.
            response.ctx.input(|i| {
                let mut p = None;
                for ev in &i.events {
                    if let egui::Event::Touch { force: Some(f), .. } = ev { p = Some(*f); }
                }
                p
            }).unwrap_or(1.0).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let (pressure_strength_factor, pressure_radius_factor) = voxel_pressure_factors(
            pressure_enabled,
            pressure_to_strength,
            pressure_to_radius,
            pen_pressure,
            pressure_radius_amount,
        );
        radius *= pressure_radius_factor;
        // Smudge returns before StrokeSample creation, so route its strength explicitly once.
        let smudge_strength = strength * pressure_strength_factor;
        // Anti-alias floor: a brush smaller than ~1.5 voxels brushes SUB-VOXEL and aliases against the grid
        // lattice (the concentric-ring artifact the user hit "way too fast"). Clamp the effective radius to
        // >= 1.5 × voxel_size so the brush can NEVER be finer than the grid can represent. Real detail comes
        // from raising resolution (128³ default, 256³+ sparse), not from an unrepresentably small brush.
        if let Some(v) = self.sculpt.voxel.as_ref() {
            radius = radius.max(1.5 * v.grid.voxel_size());
        }
        // Prioritise the chunk under the brush for the per-frame mesh budget (see voxel_flush_gpu_async):
        // the region the user is looking at always re-meshes this frame; any overflow trails the
        // periphery. `hit` is local grid space, so the cell map mirrors `grid_cell_world`'s inverse.
        if let Some(v) = self.sculpt.voxel.as_mut() {
            let (res, bounds, cd) = (v.grid.res, v.grid.bounds, v.chunk_dim.max(1));
            let span = res.saturating_sub(1).max(1) as f32;
            let chunk_of = |p: f32| {
                let cell = (((p + bounds) / (2.0 * bounds)) * span).round().clamp(0.0, span) as u32;
                (cell / VOXEL_CHUNK).min(cd - 1)
            };
            v.focus_chunk = Some([chunk_of(hit.x), chunk_of(hit.y), chunk_of(hit.z)]);
        }
        // Shift = Smooth (a distinct brush). Ctrl = reverse polarity of the *current* brush
        // (Add↔Remove) so every brush inverts — never a hard switch to Erase.
        // Permanent Add/Remove is a brush setting (`brush_add`); Ctrl XORs it for this stroke.
        // Shift wins over Ctrl (Smooth has no Add/Remove toggle mid-stroke).
        // ONE resolved effective brush (computed at stroke entry) — the density atlas/box path AND
        // the surface mesh path both dispatch off THIS, never the raw base brush. `base_brush`
        // (== v.brush) is retained only for asserting the resolution is consistent.
        debug_assert_eq!(eff_brush, if shift { VoxelBrush::Smooth } else { base_brush });
        let brush = eff_brush;
        let brush_add = self.sculpt.voxel.as_ref().map_or(true, |v| v.brush_add);
        // Zadd/Zsub → GPU/CPU `invert` (sgn = invert ? -1 : +1):
        //   Add ops:   cur + sgn*w   → need invert=false to deposit
        //   Sub ops:   cur - sgn*w   → need invert=false to carve (Crease/Erase/Scrape)
        // User Mode Add  = build / ridge / Zadd
        // User Mode Remove = carve / groove / Zsub
        // Ctrl flips for this stroke.
        // Carve-native: natural forward is carve (Remove), so invert=false when Mode=Remove.
        // Deposit-native: natural forward is deposit (Add), so invert=false when Mode=Add.
        let invert = if shift {
            false
        } else {
            let want_build = brush_add ^ (ctrl || alt); // true = Zadd this stroke (Alt = ZBrush-style invert, owner 2026-08-18)
            let carve_native = matches!(
                brush,
                VoxelBrush::Erase | VoxelBrush::Crease | VoxelBrush::Scrape
            );
            // invert=true flips the op's natural action.
            // deposit-native + want_build → no flip; carve-native + want_build → flip to ridge
            if carve_native { want_build } else { !want_build }
        };
        // Latch the stroke's resolved polarity for the pen-up release tails — they flush
        // deferred dabs AFTER the last stamp frame, when re-reading Ctrl/Alt is unreliable
        // (the user may release the modifier before LMB). Recomputing from raw brush_add
        // there was the "Ctrl-carve stroke ends with a deposited blob" polarity flip.
        self.sculpt.voxel_stroke_invert = invert;

        // ---- MOVE = Blender Grab (mesh vertices) | SMUDGE = soft density warp ----
        //
        // Blender Grab (sculpt grab.cc pattern):
        //   • Cache ORIGINAL surface / vertex positions at stroke start (our mesh is the resident pool;
        //     the VS displaces from the undeformed buffer each frame — same as orig_co + delta).
        //   • grab_delta = current_cursor_3d − initial_cursor_3d on the VIEW PLANE through first hit
        //     (plane normal = camera ray at stroke start).
        //   • Each vertex: co = orig_co + grab_delta * falloff(|orig_co − anchor|) * strength
        //     → same delta for every vert, only magnitude scales → rigid block translate (not smear).
        //   • Strength multiplies the delta (we were missing this — Blender always applies bstrength).
        //   • Original normal: latched at first click; not re-picked mid-air.
        //
        // Density inverse-warp CANNOT match that: different cells move different amounts and the field
        // thins (taffy). Meta Medium / 3D-Coat free Move deform the MESH live, voxelize on release.
        // Move → mesh_grab + GPU vs_main. Smudge → elastic density soft falloff (intentional smear).
        // Ctrl / Remove polarity reverse the drag (same as every other brush's reverse).
        if brush == VoxelBrush::Move {
            let t_brush = std::time::Instant::now();
            if !self.sculpt.voxel.as_ref().map_or(false, |v| v.mesh_grab.is_active()) {
                // Soft round brush tip: falloff is (1-t²)² over outer_r (inner unused). Matches
                // gbuffer.wgsl + MeshGrab::plateau. No hard plateau — owner wanted soft tip image.
                let outer_r = radius;
                let inner_r = 0.0;
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    // Latch first-click surface normal + view-plane normal (Blender orig normal / plane).
                    v.mesh_grab.begin(hit, normal, inner_r, outer_r, rd);
                    v.first_point_normal = Some(normal);
                    v.first_point_pos = Some(hit);
                    v.elastic_grab.cancel(); // density path off for Move
                }
            }
            // View-plane grab_delta scaled by shared Strength × Elasticity × Pull Strength.
            // Blender ALWAYS multiplies grab_delta by the brush strength (grab.cc / BLENDER_BRUSH_SPEC
            // §1 "Gap: Blender scales drag by bstrength"). Strength was previously inert for Move, so a
            // soft grab was impossible — now Strength=0.5 halves the carried distance, Strength=1.0 is
            // a full 1:1 pull (factory default). Elasticity/Pull stay as AZRA's extra feel knobs on top.
            // Polarity: Add = drag with cursor; Remove / Ctrl = reverse drag (same invert rule as clay).
            let strength_move = (strength * pressure_strength_factor).clamp(0.0, 1.0);
            let drag = self.sculpt.voxel.as_ref().map_or(glam::Vec3::ZERO, |v| {
                let plane_n = v.mesh_grab.plane_n;
                let anchor = v.mesh_grab.anchor;
                let denom = rd.dot(plane_n);
                let view_drag = if denom.abs() < 1e-5 {
                    glam::Vec3::ZERO
                } else {
                    let t = (anchor - ro).dot(plane_n) / denom;
                    (ro + rd * t) - anchor
                };
                let pol = if invert { -1.0f32 } else { 1.0 };
                let raw = view_drag * pol;
                let el = v.move_elasticity.clamp(0.0, 2.0);
                let pull = v.move_pull_strength.clamp(0.0, 2.0);
                raw * el * pull * strength_move
            });
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.mesh_grab.last_drag = drag;
            }
            // No density write mid-drag — GPU vertex shader is the live preview (gbuffer.wgsl).
            // Release path (mesh_grab active) voxelizes + undo.
            let brush_ms = t_brush.elapsed().as_secs_f32() * 1000.0;
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.profile.brush_ms = brush_ms;
                v.profile.stamps = 1;
                if brush_ms > v.profile.brush_peak { v.profile.brush_peak = brush_ms; }
            }
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        if brush == VoxelBrush::SnakeHook {
            // SURFACE-mode Snake Hook (BLENDER_BRUSH_SPEC §5): a moving-anchor TUBE pulled out of the
            // surface. Vertices displace over their PRISTINE (pre-stroke) positions — the per-chunk
            // `surface_stroke_snap` captured on first touch — so dragging back un-deforms (elastic,
            // like Grab). The anchor is latched at stroke start; grab_delta = cursor-on-plane − anchor.
            // No relax (would smear the tube). CPU vertex write into mesh_cache each frame (same
            // in-place upload the dab brushes use) — NOT the mesh_grab GPU block-translate preview.
            let t_brush = std::time::Instant::now();
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.snake_anchor.is_none()) {
                // Latch anchor + view-plane normal (camera-facing plane through the hit).
                let plane_n = (-rd).normalize_or_zero();
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    v.snake_anchor = Some(hit);
                    v.snake_plane_n = if plane_n.length_squared() > 1e-8 { plane_n } else { glam::Vec3::Z };
                    v.first_point_normal = Some(normal);
                    v.first_point_pos = Some(hit);
                    v.mesh_grab.cancel();
                    v.elastic_grab.cancel();
                }
            }
            let strength_move = (strength * pressure_strength_factor).clamp(0.0, 2.0);
            let (anchor, drag) = self.sculpt.voxel.as_ref().map(|v| {
                let anchor = v.snake_anchor.unwrap_or(hit);
                let plane_n = v.snake_plane_n;
                let denom = rd.dot(plane_n);
                let view_drag = if denom.abs() < 1e-5 {
                    glam::Vec3::ZERO
                } else {
                    let t = (anchor - ro).dot(plane_n) / denom;
                    (ro + rd * t) - anchor // cursor projected onto the anchor plane
                };
                let pol = if invert { -1.0f32 } else { 1.0 };
                let el = v.move_elasticity.clamp(0.0, 2.0);
                let pull = v.move_pull_strength.clamp(0.0, 2.0);
                (anchor, view_drag * pol * el * pull * strength_move)
            }).unwrap_or((hit, glam::Vec3::ZERO));
            let crease_pinch = self.sculpt.voxel.as_ref().map_or(0.5, |v| v.pinch_amount.clamp(0.0, 1.0));
            let (hardness, preset) = self.sculpt.voxel.as_ref()
                .map(|v| (v.falloff, v.falloff_preset))
                .unwrap_or((core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS,
                            core_math::voxel_sculpt::FalloffPreset::Smooth));
            let symmetry = self.sculpt.voxel.as_ref().map_or([false; 3], |v| v.symmetry);
            self.voxel_surface_apply_snake_hook(anchor, drag, radius, crease_pinch, hardness, preset, symmetry);
            let brush_ms = t_brush.elapsed().as_secs_f32() * 1000.0;
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.profile.brush_ms = brush_ms;
                v.profile.stamps = 1;
                if brush_ms > v.profile.brush_peak { v.profile.brush_peak = brush_ms; }
            }
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        if brush == VoxelBrush::Smudge {
            // Soft density taffy (intentional smear) — NOT the Move grab path.
            if !self.sculpt.voxel.as_ref().map_or(false, |v| v.elastic_grab.is_active()) {
                let hardness = self.sculpt.voxel.as_ref().map_or(
                    core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS, |v| v.falloff);
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    v.elastic_grab.begin(hit, radius, hardness, rd, normal);
                    v.mesh_grab.cancel();
                }
            }
            // Smudge returns before stamps are built, so use its once-routed local strength.
            let drag = self.sculpt.voxel.as_ref().map_or(glam::Vec3::ZERO, |v| {
                let plane_n = v.elastic_grab.plane_n;
                let denom = rd.dot(plane_n);
                let view_drag = if denom.abs() < 1e-5 {
                    glam::Vec3::ZERO
                } else {
                    let t = (v.elastic_grab.anchor - ro).dot(plane_n) / denom;
                    (ro + rd * t) - v.elastic_grab.anchor
                };
                let pull = v.move_pull_strength.clamp(0.0, 2.0) * v.move_elasticity.clamp(0.0, 2.0);
                view_drag * smudge_strength.clamp(0.0, 1.0) * pull
            });
            let t_brush = std::time::Instant::now();
            let symmetry = self.sculpt.voxel.as_ref().map_or([false; 3], |v| v.symmetry);
            let anchor = self.sculpt.voxel.as_ref().map(|v| v.elastic_grab.anchor).unwrap_or(hit);
            let instances = core_math::voxel_sculpt::VoxelGrid::grab_symmetry_instances(anchor, drag, symmetry);
            if let Some(v) = self.sculpt.voxel.as_mut() {
                let crate::VoxelObject { grid, elastic_grab, .. } = v;
                grid.elastic_grab_preview_multi(elastic_grab, &instances, false);
            }
            let region = self.sculpt.voxel.as_mut().and_then(|v| v.grid.take_dirty())
                .map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
            if let Some(region) = region {
                self.voxel_mark_chunks_in_region(region);
                self.voxel_expand_stroke_aabb(region);
            }
            let brush_ms = t_brush.elapsed().as_secs_f32() * 1000.0;
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.profile.brush_ms = brush_ms;
                v.profile.stamps = 1;
                if brush_ms > v.profile.brush_peak { v.profile.brush_peak = brush_ms; }
            }
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        // ---- DEPOSIT DIRECTION (sculpt-plane mode) ----
        // Keep this path SIMPLE — aggressive feel hacks here have zeroed stamps before.
        // Plane/normal brushes use Direction (FirstPoint / Surface / View / …). Move ignores stamp normal.
        let surface_extrude = self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_extrude);
        let brush_dir = self.sculpt.voxel.as_ref().map_or(
            core_math::voxel_sculpt::BrushDirection::Surface, |v| v.brush_dir);
        let effective_dir = if surface_extrude {
            core_math::voxel_sculpt::BrushDirection::Surface
        } else {
            brush_dir
        };
        let view_dir = (-rd).normalize_or_zero();
        let (avg_region, smooth_n, grid_res_dir) = self.sculpt.voxel.as_ref()
            .map(|v| (v.dir_average_region.clamp(0.25, 4.0), v.dir_smooth_normal.clamp(0.0, 1.0), v.grid.res))
            .unwrap_or((1.0, 0.0, 64));
        // Only sample area when the user asked for Average / Smooth normal (cheap default path).
        let need_area = effective_dir.wants_average() || smooth_n > 1e-4;
        let area_n = if need_area {
            self.sculpt.voxel.as_ref().map(|v| {
                let reg = if grid_res_dir >= 896 { avg_region.min(2.5) } else { avg_region };
                v.grid.sample_area_normal_fast(hit, radius, reg)
            }).unwrap_or(normal)
        } else {
            normal
        };
        if let Some(v) = self.sculpt.voxel.as_mut() {
            if effective_dir.wants_first_point_latch() && v.first_point_normal.is_none() {
                v.first_point_normal = Some(normal);
                v.first_point_pos = Some(hit);
            }
            if need_area && area_n.length_squared() > 1e-12 {
                v.average_normal_sum = area_n;
                v.average_normal_count = 1;
            }
        }
        let first_point = self.sculpt.voxel.as_ref().and_then(|v| v.first_point_normal);
        let first_point_pos = self.sculpt.voxel.as_ref().and_then(|v| v.first_point_pos);
        let stroke_dir = self.sculpt.voxel.as_ref().and_then(|v| {
            v.stroke_prev_pos.map(|prev| hit - prev)
        });
        let resolve_dir = |surface: glam::Vec3, stroke: Option<glam::Vec3>, area: glam::Vec3| -> glam::Vec3 {
            let ctx = core_math::voxel_sculpt::BrushDirContext {
                surface,
                view: view_dir,
                first_point,
                average: if area.length_squared() > 1e-12 { Some(area) } else { None },
                stroke_dir: stroke,
            };
            let mut d = if brush == VoxelBrush::Move {
                surface
            } else {
                effective_dir.resolve(ctx)
            };
            if brush != VoxelBrush::Move && smooth_n > 1e-4 && area.length_squared() > 1e-12 {
                d = (d * (1.0 - smooth_n) + area * smooth_n).normalize_or_zero();
                if d.length_squared() < 1e-12 { d = area; }
            }
            d
        };
        // Prefer density hit normal; if zero, fall back to +Y (never leave zero stamp normal).
        let surface_n = if normal.length_squared() > 1e-12 { normal } else { glam::Vec3::Y };
        let dir = {
            let d = resolve_dir(surface_n, stroke_dir, area_n);
            if d.length_squared() > 1e-12 { d } else { surface_n }
        };
        let stroke_prev_for_tip = self.sculpt.voxel.as_ref().and_then(|v| v.stroke_prev_pos);
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.stroke_prev_pos = Some(hit);
        }

        {
            let spacing = self.sculpt.voxel.as_ref().map_or(0.10, |v| v.stroke_spacing.clamp(0.03, 0.50));
            self.sdf_stroke_interpolator.spacing = spacing;
        }
        // Carry only the enabled strength route. Radius-only pressure must not alter stamp strength.
        let mut sample = core_math::sculpt_layer::StrokeSample::new(hit, dir, pressure_strength_factor);
        // Radius pressure belongs to the dab, just like strength pressure. Keeping the resolved scale
        // on StrokeSample prevents a deferred dab from inheriting the next frame's tablet force.
        sample.radius_scale = (radius / base_radius).max(1e-4);
        // Blender stroke_method: Dots = one dab per sample; Space = interpolator;
        // Airbrush = Space + timed dabs while the cursor is still.
        let mut pts = match self.sculpt.stroke_method.effective() {
            SculptStrokeMethod::Dots | SculptStrokeMethod::DragDot => vec![sample.clone()],
            SculptStrokeMethod::Airbrush => {
                let mut pts = self.sdf_stroke_interpolator.push(sample.clone(), radius);
                if pts.is_empty() {
                    let now = std::time::Instant::now();
                    let due = self.sculpt.airbrush_last.map_or(true, |t| {
                        now.duration_since(t) >= self.sculpt.airbrush_interval()
                    });
                    if due {
                        self.sculpt.airbrush_last = Some(now);
                        pts = vec![sample.clone()];
                    }
                } else {
                    self.sculpt.airbrush_last = Some(std::time::Instant::now());
                }
                pts
            }
            _ => self.sdf_stroke_interpolator.push(sample.clone(), radius),
        };
        // From here every dab carries its own resolved radius scale; all backends multiply by the
        // stable tool radius so live, deferred, and pen-up tail dabs use the same value.
        radius = base_radius;
        // ---- PER-DAB SURFACE FOLLOW (Blender stroke model, 2026-07-23) ----
        // The interpolator emits dabs on the world-space CHORD between last frame's and this
        // frame's surface hits; across curvature the chord cuts inside (or floats above) the
        // skin, so a stroke could not wrap a sphere. Blender re-projects every dab onto the
        // CURRENT surface and rebuilds the brush frame (position + normal) per dab. Do the
        // same HERE — the single dab producer, BEFORE the surface/density mode fork — so both
        // paths receive on-surface dabs.
        //   • position: snapped back onto the isosurface along the local gradient (voxel mode,
        //     fresh field) / onto the live-mesh footprint plane (surface mode).
        //   • normal: refreshed at the dab (grid gradient / mesh area normal), continuity-
        //     aligned with the previous dab so one gradient glitch cannot flip the deposit
        //     (the historic "brick-flipped ring on long strokes over ridges" failure class).
        //   • Original (FirstPoint) keeps its locked-plane identity: no snap — its dedicated
        //     first-plane projection below IS that option's point (explicit, not the default).
        //   • Sparse ATLAS strokes: the CPU field mirror is stale-by-this-stroke from the first
        //     GPU write — snapping to it would drag dabs back onto the PRE-stroke skin, so
        //     positions stay on the chord between live GPU-pick hits (short at frame rate) and
        //     normals read the pre-stroke field (the pre-existing Surface-direction source).
        //   • Grab family (Move/SnakeHook/Smudge) never reaches here (early-returned above) —
        //     they correctly anchor to first contact + original coords.
        let in_surface_mode = self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_mode);
        let follow_surface = !pts.is_empty() && !effective_dir.wants_first_point_latch();
        let mut dab_surf: Vec<glam::Vec3> = Vec::new();
        // Profiler: the surface-mode per-dab FOLLOW pass (live-mesh area frames) is real per-frame
        // brush work that runs BEFORE the apply-dabs timer — measure it here and fold it into
        // `profile.brush_ms` at the surface dispatch below so the readout reflects the whole
        // stroke-frame cost (candidate scan + displace + weld + pool upload + this).
        let mut surface_follow_ms = 0.0f32;
        if follow_surface {
            if in_surface_mode {
                // Mesh is the authority in Surface mode (density stale over surface_dirty_region):
                // per-dab live-mesh area frames, ONE chunk scan for the whole batch.
                let t_follow = std::time::Instant::now();
                let probes: Vec<(glam::Vec3, f32)> = pts.iter()
                    .map(|pt| (pt.position, (radius * pt.radius_scale).max(1e-4)))
                    .collect();
                // Front-face reference = the stroke's continuity normal: verts facing away
                // (the far wall of a carved trench) must not tilt the follow frame, and a
                // degenerate footprint (opposing walls, mean normal below the agreement
                // threshold) resolves None → the `last_surf` fallback below, fail closed.
                let refs = vec![normal; probes.len()];
                let frames = self.voxel_surface_area_frames(
                    &probes,
                    crate::voxel_surface_sculpt::SURFACE_AREA_NORMAL_FACTOR,
                    &refs,
                );
                surface_follow_ms = t_follow.elapsed().as_secs_f32() * 1000.0;
                let vs_local = self.sculpt.voxel.as_ref().map_or(1e-3, |v| v.grid.voxel_size());
                let mut last_surf = normal;
                dab_surf = (0..pts.len()).map(|i| {
                    if let Some((c, n)) = frames.get(i).copied().flatten() {
                        // Project the chord point onto the local surface fit (plane through the
                        // footprint centroid) — the dab rides the CURRENT mesh, not the chord.
                        let p = pts[i].position;
                        let d = (p - c).dot(n);
                        let max_snap = (radius * pts[i].radius_scale).max(vs_local);
                        if d.abs() <= max_snap {
                            pts[i].position = p - n * d;
                        }
                        let n = if n.dot(last_surf) < 0.0 { last_surf } else { n };
                        last_surf = n;
                        n
                    } else {
                        last_surf // off-mesh dab (past silhouette): keep the previous frame
                    }
                }).collect();
            } else if let Some(v) = self.sculpt.voxel.as_ref() {
                let field_fresh = !v.cpu_grid_stale;
                let max_snap = radius.max(v.grid.voxel_size() * 4.0);
                // 1024³: per-dab gradient walks are the cost — process every 2nd dab, odd dabs
                // reuse the neighbour frame (same throttle the old per-dab normal loop used).
                let n_step = if v.grid.res >= 896 && pts.len() > 8 { 2usize } else { 1 };
                let mut last_surf = normal;
                dab_surf = (0..pts.len()).map(|i| {
                    if i % n_step == 0 || i + 1 == pts.len() {
                        if field_fresh {
                            // `last_surf` hints the walk direction where the gradient is zero
                            // (field saturates a couple voxels under the shell).
                            if let Some(sp) = v.grid.snap_to_surface(pts[i].position, last_surf, max_snap) {
                                pts[i].position = sp;
                            }
                        }
                        let n = v.grid.normal_at(pts[i].position);
                        let n = if n.length_squared() > 1e-12 { n.normalize_or_zero() } else { last_surf };
                        let n = if n.dot(last_surf) < 0.0 { last_surf } else { n };
                        last_surf = n;
                    }
                    last_surf
                }).collect();
            }
        }
        // Re-resolve dab DIRECTIONS only when the plane can change along the path. Original+
        // smooth0 / fixed axes keep a constant dir (interpolator is enough). Live / Average /
        // stroke-dir / smooth consume the per-dab surface frames acquired above — REUSE frame
        // area_n (no N× area sample). Normal = one plane for the whole frame (cursor hit) by
        // its documented contract; its dab POSITIONS still ride the surface via the snap above.
        let need_per_dab = brush != VoxelBrush::Move
            && (brush.is_direction_aware()
                || surface_extrude
                || effective_dir.wants_live_surface()
                || effective_dir.wants_average()
                || effective_dir.wants_stroke_dir()
                || smooth_n > 1e-4
                || matches!(
                    effective_dir,
                    core_math::voxel_sculpt::BrushDirection::View
                        | core_math::voxel_sculpt::BrushDirection::Surface
                ));
        if need_per_dab && !pts.is_empty() {
            if let Some(v) = self.sculpt.voxel.as_ref() {
                let want_live_surf = effective_dir.wants_live_surface()
                    || effective_dir.wants_average()
                    || smooth_n > 1e-4
                    || surface_extrude;
                // 1024³: per-dab normal_at is expensive — sample every Nth dab, reuse tip for rest.
                let hi_res = v.grid.res >= 896;
                let n_step = if hi_res && pts.len() > 8 { 2usize } else { 1 };
                let mut last_surf = normal;
                for i in 0..pts.len() {
                    let pos = pts[i].position;
                    let surf = if want_live_surf {
                        if let Some(&s) = dab_surf.get(i) {
                            s // per-dab frame from the surface-follow pass (grid or live mesh)
                        } else if i % n_step == 0 || i + 1 == pts.len() {
                            // Fallback (e.g. Original + Smooth-normal slider): sample the field.
                            let n = v.grid.normal_at(pos);
                            last_surf = if n.length_squared() > 1e-12 { n } else { pts[i].normal };
                            last_surf
                        } else {
                            last_surf
                        }
                    } else {
                        pts[i].normal
                    };
                    // Surface mode: `area_n` was sampled from the DENSITY field, which is stale
                    // once mesh edits exist — `surf` already IS the live-mesh area normal
                    // (footprint average over normal_radius_factor × radius). Never mix them.
                    let area = if need_area && !in_surface_mode { area_n } else { surf };
                    let sd = if effective_dir.wants_stroke_dir() {
                        if i + 1 < pts.len() {
                            Some(pts[i + 1].position - pts[i].position)
                        } else if i > 0 {
                            Some(pts[i].position - pts[i - 1].position)
                        } else {
                            stroke_dir
                        }
                    } else {
                        None
                    };
                    pts[i].normal = resolve_dir(surf, sd, area);
                }
            }
        }
        // Alpha Orientation (ZBrush/Coat): Fixed / Follow Stroke / View / Random + Angle offset.
        // Runs before dynamics so Random Rotation dynamics ADDS on top of base orientation.
        // ONE resolver shared with the poly stroke driver (input_sculpt::resolve_tip_rotations).
        if !pts.is_empty() {
            let (orient, follow, angle_deg, stroke_seed, dab_start) = self.sculpt.voxel.as_ref()
                .map(|v| (v.tip_orient, v.tip_follow_stroke, v.tip_angle, v.dyn_stroke_seed, v.dyn_dab_index))
                .unwrap_or((0, false, 0.0, 0, 0));
            let cam_up = self.viewport.camera.orbit * glam::Vec3::Y;
            crate::input_sculpt::resolve_tip_rotations(
                &mut pts, orient, follow, angle_deg, cam_up, stroke_seed, dab_start,
                stroke_prev_for_tip,
            );
        }
        // Original Normal (FirstPoint): keep stamp CENTERS on the first-hit plane so Draw Add
        // does not climb the growing pile (Remove already felt controlled; Add ballooned upward).
        // Direction still uses first_point_normal; only position is projected.
        if effective_dir.wants_first_point_latch() {
            if let (Some(p0), Some(n0)) = (first_point_pos, first_point) {
                let n0 = n0.normalize_or_zero();
                if n0.length_squared() > 1e-12 {
                    for pt in pts.iter_mut() {
                        let d = (pt.position - p0).dot(n0);
                        pt.position = pt.position - n0 * d;
                    }
                }
            }
        }
        // Front Faces Only: drop dabs that clearly face away from the camera.
        // Do NOT auto-flip normals (that inverted deposit / plane fill and looked like "dead brush").
        // Sculpt on Surface alone does NOT cull — only the explicit Front faces toggle does.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.front_faces_only) && !pts.is_empty() {
            let before = pts.len();
            pts.retain(|pt| pt.normal.dot(view_dir) > -0.05);
            if pts.is_empty() && before > 0 {
                // Never empty a valid stroke — restore tip sample so paint still lands.
                pts.push(sample.clone());
            }
        }
        // Brush Dynamics (Jitter / Scatter / Random Rotation / Strength / Radius / Stroke Noise).
        // Applied AFTER interpolator + direction resolve, BEFORE symmetry so each dab randomizes
        // once and mirrors inherit the offset (not a second independent roll).
        {
            let dyn_knobs = self.sculpt.voxel.as_ref().map(|v| core_math::sculpt_layer::BrushDynamics {
                jitter: v.dyn_jitter,
                scatter: v.dyn_scatter,
                random_rotation: v.dyn_random_rotation,
                random_strength: v.dyn_random_strength,
                random_radius: v.dyn_random_radius,
                stroke_noise: v.dyn_stroke_noise,
            }).unwrap_or_default();
            // Only this frame's new interpolated dabs pass through direction/orientation/dynamics.
            // The FIFO backlog is already final and is prepended afterwards, so a frame-budget spill
            // cannot re-jitter, re-project, or re-orient the same dab on every retry.
            if let Some(v) = self.sculpt.voxel.as_mut() {
                let fresh_count = pts.len() as u32;
                let seed = brush_dynamics_seed_at(v.dyn_stroke_seed, v.dyn_dab_index);
                apply_fresh_dab_dynamics_and_prepend_deferred(
                    &mut pts,
                    &mut v.deferred_stamps,
                    radius,
                    dyn_knobs,
                    seed,
                );
                v.dyn_dab_index = v.dyn_dab_index.wrapping_add(fresh_count);
            } else {
                core_math::sculpt_layer::apply_brush_dynamics(&mut pts, radius, dyn_knobs, 0);
            }
        }
        // Path selection BEFORE the stamp budget: GPU stamps are NOT the same cost model as CPU.
        let is_move = matches!(brush, VoxelBrush::Move);
        // Paint Color forces the CPU brush path: the per-voxel colour deposit (`deposit_color_sphere`)
        // lives ONLY on the CPU branch below — the GPU brush kernel writes density, never the parallel
        // colour buffer. Mirror how Move self-forces CPU.
        let cpu_paint = self.sculpt.voxel.as_ref().map_or(false, |v| v.cpu_paint_path());
        // Mask PAINT mode writes the mask buffer on the CPU path only (no density). Active protect-mask
        // no longer forces full CPU for density stamps — the GPU box kernel samples a box-local mask
        // buffer (binding 3). Atlas is disabled when mask is present (see stroke_uses_atlas latch).
        let mask_paint_only = self.sculpt.voxel.as_ref().map_or(false, |v| v.mask_paint);
        // GPU path available for this stroke (stamp list may still be empty — checked at dispatch).
        let gpu_available = self.sculpt.voxel.as_ref().map_or(false, |v| v.gpu_brush)
            && self.gpu.is_some() && !is_move && !cpu_paint && !mask_paint_only;

        // Frame-budget guard AFTER symmetry expansion so mirrored stamps count toward the cap.
        // CPU path: a stamp touches ~(2r/voxel_size)³ voxels — cap by estimated voxel work.
        // GPU path: async dispatch cost scales with stamp count / box size on GPU, not UI-thread
        // voxel loops — cap by count only so strokes stay continuous.
        let vs = self.sculpt.voxel.as_ref().map_or(1.0, |v| v.grid.voxel_size()).max(1e-6);
        // Stack multi-op (Gather/Flatten/Fill) multiplies per-voxel work — budget for it.
        let stack_ops = self.sculpt.voxel.as_ref().map_or(0.0, |v| {
            (v.stack_gather as u8
                + v.stack_flatten as u8
                + v.stack_fill as u8
                + v.stack_crease as u8
                + v.stack_inflate as u8) as f32
        });
        // Pinch is GPU Advect on atlas now — was 4× which starved stamps on the old box path.
        let brush_cost = match brush {
            VoxelBrush::Pinch => 1.5,
            VoxelBrush::Smooth => 2.5,
            VoxelBrush::Inflate => 2.0,
            _ => 1.0,
        } * (1.0 + stack_ops * 0.55);
        let per_stamp = ((2.0 * radius / vs) + 1.0).max(1.0).powi(3) * brush_cost;
        // 3D-Coat continuous clay: GPU stamps are cheap on UI thread — keep counts high enough
        // for dense dabs. CPU path still voxel-budgeted. High-res auto-mask/box is the limiter.
        let res_now = self.sculpt.voxel.as_ref().map_or(64, |v| v.grid.res);
        // Free = headroom for denser clay; tight = protect FPS but never starve continuous dabs.
        let free_frame = self.prev_frame_wall_ms < 14.0;
        let tight_frame = self.prev_frame_wall_ms > 22.0;
        let voxel_budget: f32 = if res_now >= 896 {
            if free_frame { 1_400_000.0 } else if tight_frame { 800_000.0 } else { 1_000_000.0 }
        } else if res_now >= 768 {
            if free_frame { 1_800_000.0 } else { 1_400_000.0 }
        } else if res_now >= 512 {
            if free_frame { 2_800_000.0 } else { 2_200_000.0 }
        } else {
            4_500_000.0
        };
        // Tips sample on GPU (voxel_brush.wgsl tip_alpha) — no CPU force.
        // Atlas stamps are near-free on the UI thread → high counts for continuous Coat-like clay.
        // Box path still uses the same cap (async collect; no Wait hitch mid-dispatch).
        let atlas_stroke = self.sculpt.voxel.as_ref().map_or(false, |v| v.stroke_uses_atlas == Some(true));
        let gpu_max = if atlas_stroke {
            // 1024³ atlas: cap stamps so materialize_box + index sync stay under budget.
            // Continuous clay still needs dozens of dabs; free frames can go higher.
            if res_now >= 896 {
                if free_frame { 56 } else if tight_frame { 24 } else { 40 }
            } else if res_now >= 512 {
                if free_frame { 96 } else if tight_frame { 48 } else { 72 }
            } else {
                if free_frame { 128 } else { 96 }
            }
        } else if res_now >= 896 {
            // Box path at 1024: keep low — union box gather/readback is the cost.
            if free_frame { 20 } else if tight_frame { 10 } else { 14 }
        } else if res_now >= 768 {
            if free_frame { 40 } else { 28 }
        } else if res_now >= 512 {
            if free_frame { 56 } else { 36 }
        } else {
            96
        };
        let cpu_max = if res_now >= 896 {
            if free_frame { 14 } else { 10 }
        } else if res_now >= 768 {
            18
        } else if res_now >= 512 {
            32
        } else {
            64
        };
        let max_expanded_stamps = if gpu_available {
            gpu_max
        } else {
            // CPU: keep voxel budget. Never collapse a multi-sample stroke to a single stamp when we
            // can afford 2+ (single-stamp collapse left beads on fast flicks).
            let by_budget = ((voxel_budget / per_stamp) as usize).clamp(1, cpu_max);
            if by_budget == 1 && pts.len() >= 2 && per_stamp * 2.0 <= voxel_budget * 1.25 {
                2
            } else {
                by_budget
            }
        };
        let raw_stamp_count = pts.len();
        // Budget BEFORE symmetry so mirror pairs stay intact. Excess dabs are DEFERRED to the
        // next frame (not dropped) so deposit mass does not depend on FPS/machine.
        let symmetry = self.sculpt.voxel.as_ref().map_or([false; 3], |v| v.symmetry);
        let max_primary_stamps = voxel_primary_stamp_budget(max_expanded_stamps, symmetry);
        if pts.len() > max_primary_stamps {
            let rest: Vec<_> = pts.drain(max_primary_stamps..).collect();
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.deferred_stamps.extend(rest);
            }
        }
        let primary_stamp_count = pts.len();
        // Symmetry AFTER budget: mirrors of the kept primary set only. The primary budget reserves
        // the worst-case multiplier, and on-plane dabs are kept once so strength is not multiplied.
        expand_voxel_symmetry(&mut pts, symmetry, vs * 0.5);

        // ---- SURFACE MODE dispatch (3D-Coat Surface sculpting) ----
        // The dabs above went through the SAME interpolator / direction / dynamics / symmetry
        // pipeline as density dabs, so falloff/spacing/strength feel identical across modes.
        // They displace the existing chunk-mesh vertices directly — NO density writes.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_mode) {
            if !crate::voxel_surface_sculpt::surface_brush_supported(brush) {
                return; // fail-closed (gated at stroke start + greyed in the UI)
            }
            let t_brush = std::time::Instant::now();
            let (hardness, preset) = self.sculpt.voxel.as_ref()
                .map(|v| (v.falloff, v.falloff_preset))
                .unwrap_or((core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS,
                            core_math::voxel_sculpt::FalloffPreset::Smooth));
            // Polarity flows from the SINGLE `invert` resolved above (Shift/Ctrl/brush_add) — the
            // exact same flag the density path (Zadd/Zsub, `sgn` below) consumes. There is NO parallel
            // invert computation: `base` here is only the brush's NATURAL displacement sign (Erase
            // carves; density expresses that via a subtract op instead). Ctrl therefore inverts every
            // supported surface brush identically to how it inverts the density path.
            let base = if matches!(brush, VoxelBrush::Erase) { -1.0f32 } else { 1.0 };
            let sgn = if invert { -base } else { base };
            // Frame stroke tangent (unit) for the stroke-axis brushes (Pinch/Crease) — net
            // displacement across this frame's dabs, matching the density Pinch path (input_sculpt
            // 4715). Non-axis brushes ignore it; ZERO (single/still dab) → axisymmetric fallback.
            let surf_stroke_tan = if matches!(brush, VoxelBrush::Pinch | VoxelBrush::Crease) && pts.len() >= 2 {
                (pts[pts.len() - 1].position - pts[0].position).normalize_or_zero()
            } else {
                glam::Vec3::ZERO
            };
            // Blender spacing-overlap attenuation (brush_strength, sculpt.cc v3.6):
            // Draw-family × overlap_factor; Smooth/Move/etc keep raw strength.
            let overlap = match brush {
                VoxelBrush::Draw | VoxelBrush::Erase | VoxelBrush::Inflate => {
                    let (sp, hd, pre) = self.sculpt.voxel.as_ref()
                        .map(|v| (v.stroke_spacing, v.falloff, v.falloff_preset))
                        .unwrap_or((0.10, 0.0, core_math::voxel_sculpt::FalloffPreset::Smooth));
                    core_math::voxel_sculpt::stroke_overlap_factor(sp.clamp(0.03, 0.50), hd, pre)
                }
                _ => 1.0,
            };
            let dabs: Vec<crate::voxel_surface_sculpt::SurfaceDab> = pts.iter().map(|pt| {
                crate::voxel_surface_sculpt::SurfaceDab {
                    pos: pt.position,
                    dir: pt.normal.normalize_or_zero(),
                    radius: (radius * pt.radius_scale).max(1e-4),
                    strength: strength * overlap * pt.pressure,
                    stroke_tan: surf_stroke_tan,
                    tip_rotation: pt.tip_rotation,
                }
            }).collect();
            let n_dabs = dabs.len() as u32;
            self.voxel_surface_apply_dabs(&dabs, brush, sgn, hardness, preset);
            // + the per-dab surface-follow pass measured above — the full stroke-frame brush cost.
            let brush_ms = t_brush.elapsed().as_secs_f32() * 1000.0 + surface_follow_ms;
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.profile.brush_ms = brush_ms;
                v.profile.stamps = n_dabs;
                if brush_ms > v.profile.brush_peak { v.profile.brush_peak = brush_ms; }
            }
            return;
        }

        // Apply every stamp, deferring the re-mesh. apply_brush records each MODIFIED voxel's
        // pre-stroke value into voxel_stroke (dense-mask dedup) for undo — no separate full-box
        // snapshot pass and no per-voxel HashMap, which were the high-res frame killers.
        let t_brush = std::time::Instant::now();
        // GPU brush path (toggle): deposit ops run on the GPU (CPU cost moves off the UI thread).
        // Neighbourhood ops (Smooth/Inflate/Pinch) read a pre-frame slice snapshot in the kernel;
        // Pinch's redistancing relax runs once on the CPU after writeback (below).
        let op_code: u32 = match brush {
            VoxelBrush::Draw => 0, VoxelBrush::Erase => 1, VoxelBrush::Smooth => 2, VoxelBrush::Flatten => 3,
            VoxelBrush::Clay => 4, VoxelBrush::Scrape => 5, VoxelBrush::Crease => 6, VoxelBrush::Fill => 7,
            VoxelBrush::ClayBuildup => 8, VoxelBrush::Inflate => 9, VoxelBrush::Pinch => 10,
            // Move and Smudge are CPU-only (forced off the GPU path above); these op-codes are never
            // dispatched — they run the live elastic-grab path (early-returned above).
            // Snake Hook is surface-mode only (greyed in Voxel mode); Move/Smudge are CPU grabs.
            // None dispatch a density op-code here.
            VoxelBrush::Move | VoxelBrush::Smudge | VoxelBrush::SnakeHook => u32::MAX,
        };
        // Tips stay on GPU — use_gpu_brush is only gated by Move/paint/mask paint, not tip alpha.
        let use_gpu_brush = gpu_available && !pts.is_empty();

        if use_gpu_brush {
            // ASYNC readback: dispatch this frame's stamps WITHOUT waiting; the edited slice is
            // collected next frame in `voxel_brush_collect` (poll phase, before the next raycast). The
            // ~4ms blocking readback is gone from the input path. First ensure no prior batch is still
            // in flight (the poll collect normally already cleared it) — brush writes are NOT
            // idempotent, so two batches must never overlap on the persistent density buffer.
            self.voxel_brush_collect();
            if self.gpu.as_ref().map_or(false, |g| g.voxel_brush.is_none()) {
                let device = self.gpu.as_ref().unwrap().device.clone();
                let b = crate::voxel_gpu_mesh::VoxelBrushGpu::new(&device);
                self.gpu.as_mut().unwrap().voxel_brush = Some(b);
            }
            let (gres, gbounds, giso) = { let v = self.sculpt.voxel.as_ref().unwrap(); (v.grid.res, v.grid.bounds, v.grid.iso) };
            // Falloff hardness 0..1 (same value for the whole batch — one tool-state choice). Threaded
            // into the GPU brush params so the DEFAULT GPU path uses the SAME continuous curve as the CPU.
            let g_hardness = self.sculpt.voxel.as_ref().map_or(core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS, |v| v.falloff);
            let sgn = if invert { -1.0f32 } else { 1.0 };
            // Auto Mask PRE GPU dispatch: write protect mask so box_mask / mask_factor gate this frame.
            // High-res: throttle (every Nth stamp / first of frame) — full per-stamp Laplacian @1024³
            // was a measured UI hitch (Coat-like protect doesn't recompute every dab).
            if let Some(v) = self.sculpt.voxel.as_mut() {
                if v.auto_mask.is_on() {
                    let am = v.auto_mask;
                    let ai = v.auto_mask_intensity.clamp(0.0, 1.0);
                    let ainv = v.auto_mask_invert;
                    // 1024³ auto-mask is Laplacian-heavy — every 5th dab + first/last only.
                    let step = if v.grid.res >= 896 { 5usize } else if v.grid.res >= 512 { 2 } else { 1 };
                    let n_pts = pts.len();
                    for (i, pt) in pts.iter().enumerate() {
                        if i % step != 0 && i + 1 != n_pts { continue; }
                        let r = (radius * pt.radius_scale).max(1e-4);
                        v.grid.deposit_auto_mask_sphere(pt.position, r, g_hardness, am, ai, ainv);
                    }
                }
            }
            // (op, center, normal, r, strength, sgn, aabb, tip_rotation) — tip samples on GPU.
            let stamps: Vec<GpuVoxelStamp> = {
                let v = self.sculpt.voxel.as_ref().unwrap();
                pts.iter().filter_map(|pt| {
                    // Per-dab radius/strength from brush dynamics (Random Radius / Random Strength).
                    let r = (radius * pt.radius_scale).max(1e-4);
                    let s = strength * pt.pressure;
                    v.grid.brush_aabb(pt.position, r)
                        .map(|aabb| (op_code, pt.position, pt.normal, r, s, sgn, aabb, pt.tip_rotation))
                }).collect()
            };
            let tip_frame = self.sculpt.voxel.as_ref().and_then(|v| v.brush_tip.clone());
            let tip_contrast = self.sculpt.voxel.as_ref().map_or(1.0, |v| v.tip_contrast.clamp(0.05, 4.0));
            // Stroke-axis Pinch tangent (BLENDER_BRUSH_SPEC §2): net displacement across this frame's
            // stamps, normalized. Non-Pinch or a single/still dab → ZERO → axisymmetric fallback (the
            // GPU kernel and the CPU stamp path both take the radial branch on a zero tangent).
            let stroke_tan = if matches!(brush, VoxelBrush::Pinch) && stamps.len() >= 2 {
                (stamps[stamps.len() - 1].1 - stamps[0].1).normalize_or_zero()
            } else {
                glam::Vec3::ZERO
            };
            if !stamps.is_empty() {
                let (mut umin, mut umax) = ([u32::MAX; 3], [0u32; 3]);
                for s in &stamps {
                    let a = s.6;
                    umin = [umin[0].min(a[0]), umin[1].min(a[1]), umin[2].min(a[2])];
                    umax = [umax[0].max(a[3]), umax[1].max(a[4]), umax[2].max(a[5])];
                }
                let union = [umin[0], umin[1], umin[2], umax[0], umax[1], umax[2]];
                let is_pinch = matches!(brush, VoxelBrush::Pinch);
                // GPU-RESIDENT path (Phase 5a/5c): Sparse grid + atlas sub-toggle + non-Pinch. The brush
                // gathers/writes the GPU atlas POOL IN PLACE — no per-frame CPU box round-trip — and the
                // mesh reads the same POOL → live surface. The CPU grid is synced ONCE on stroke release.
                // Path latched at stroke start (not recomputed per frame) — see the drag-start latch.
                // Pinch-ness is folded into the latch (a Pinch stroke latches to the box path).
                let use_atlas = self.sculpt.voxel.as_ref().map_or(false, |v| v.stroke_uses_atlas == Some(true));
                if use_atlas {
                    // Pre-materialise only bricks overlapped by a final stamp AABB; union-corner bricks
                    // stay implicit and remain eligible for sparse raymarch empty-space skipping.
                    // then sync the atlas so index/pool are current, THEN gather→brush→scatter on the GPU.
                    // Phase A.2: the sparse report returns each touched brick's (bi, prev_tag,
                    // live handle) so the GPU brick-snapshot undo can record the pre-stroke POOL slot.
                    let brick_res = self.sculpt.voxel.as_ref().unwrap().grid.density.brick_res();
                    let touched_bricks = voxel_bricks_for_regions(stamps.iter().map(|s| s.6), brick_res);
                    let report = match self.sculpt.voxel.as_mut() {
                        Some(v) => v.grid.density.materialize_bricks_report(&touched_bricks),
                        None => Vec::new(),
                    };
                    self.voxel_sync_mesh_atlas();
                    let stamp_regions: Vec<[u32; 6]> = stamps.iter().map(|s| s.6).collect();
                    let atlas_batches = voxel_atlas_region_batches(
                        &stamp_regions,
                        VOXEL_ATLAS_MAX_SCRATCH_VOXELS,
                    );
                    let mut submitted = false;
                    {
                        let gpu = self.gpu.as_mut().unwrap();
                        // Split disjoint field borrows up front so the snapshot builder (&mut one field)
                        // and the atlas/brush (&another field) don't alias through `gpu`.
                        let GpuCtx { device, queue, voxel_mesh_atlas, voxel_brush, voxel_atlas_undo_building, .. } = gpu;
                        let (device, queue) = (&**device, &**queue);
                        if let (Some(atlas), Some(brush)) = (voxel_mesh_atlas.as_ref(), voxel_brush.as_ref()) {
                            // Snapshot every newly-touched brick's pre-scatter POOL slot (first-touch dedup
                            // across the stroke's frames). Runs AFTER the sync wrote the materialised
                            // implicit fills, BEFORE the scatter overwrites them — so the snapshot IS the
                            // pre-stroke state. NO readback. The builder accumulates across frames.
                            let builder = voxel_atlas_undo_building.get_or_insert_with(Default::default);
                            builder.capture(device, queue, &atlas.pool_buf, &report);
                            let g_preset = self.sculpt.voxel.as_ref().map_or(0, |v| v.falloff_preset.as_u32());
                            let g_pinch = self.sculpt.voxel.as_ref().map_or(1.0, |v| v.pinch_intensity);
                            let plane = self.sculpt.voxel.as_ref().map(crate::voxel_gpu_mesh::gpu_plane_knobs_from_voxel).unwrap_or_default();
                            for (range, batch_union) in &atlas_batches {
                                brush.dispatch_stamps_region_atlas(
                                    device, queue, gres, gbounds, giso, brick_res,
                                    &atlas.index_buf, &atlas.pool_buf, *batch_union, g_hardness,
                                    g_preset, g_pinch, plane, tip_frame.as_ref(), tip_contrast,
                                    stroke_tan,
                                    &stamps[range.clone()],
                                );
                            }
                            submitted = !atlas_batches.is_empty();
                        }
                    }
                    // Mark ONLY the chunks each stamp's SPHERE overlaps (not its AABB box, and NOT the
                    // union — for a fast diagonal flick that box is ~20× the swept tube). Sphere-culling
                    // also drops the ~48% AABB-corner chunks the brush never touches, so the re-mesh
                    // budget isn't spent on un-edited chunks → the trailing slivers clear. The scatter
                    // already wrote the atlas this frame, so meshing now is correct. Release readback
                    // still uses `union` to bound the changed region. (s.1 = centre, s.3 = radius.)
                    if submitted {
                        for s in &stamps {
                            self.voxel_mark_chunks_for_brush(s.1, s.3);
                            self.voxel_expand_stroke_aabb(s.6);
                        }
                    }
                    // Flag the CPU grid stale at the FIRST atlas write, not at release: the GPU pool now
                    // diverges from the pre-stroke CPU arena THIS frame. If a save/auto-save/resample/
                    // duplicate runs mid-stroke (e.g. auto-save crossing the 120s boundary while a stroke
                    // is held), materialize_cpu_from_atlas must know to read the GPU pool back — otherwise
                    // it no-ops on a "fresh" flag and serializes the stale pre-stroke surface. No hot-path
                    // cost: materialize is never on the sculpt path, only on those rare consumers.
                    if submitted {
                        if let Some(v) = self.sculpt.voxel.as_mut() {
                            v.atlas_stroke = true;
                            v.cpu_grid_stale = true;
                        }
                    }
                    // The brush just wrote the GPU atlas (index+pool) IN PLACE — that atlas IS the sculpt
                    // raymarch's source. Mark RENDER so viewport_render() doesn't early-return at its
                    // `if !is_dirty` gate and the raymarch re-dispatches THIS frame, showing the live edit.
                    // Previously the only per-frame RENDER dirty came from voxel_flush_dirty_chunks (the mesh
                    // flush, line 661), which is now skipped during a sparse stroke (the raymarch is the live
                    // display). Without this the surface only reappeared on release — the atlas write is the
                    // render-relevant mutation, so it must mark its own dirty, not lean on the mesh flush.
                    if submitted { self.mark_dirty(DirtyFlags::RENDER); }
                } else {
                    // Box-round-trip path (Phase 4): gather the union box from the CPU grid (authoritative
                    // accumulator), dispatch on a per-dispatch box buffer, read back + write CPU at collect.
                    // RECONCILE FIRST (source-of-truth invariant): if a prior atlas stroke on this sparse
                    // grid set `cpu_grid_stale`, gathering without materialize clobbers GPU truth.
                    // A prior async box batch may still be mapping. Never overwrite it, and never drop
                    // this frame's samples: put the resolved PRIMARY prefix back ahead of the later
                    // budget overflow. Symmetry copies are regenerated when that prefix dispatches.
                    if self.gpu.as_ref().map_or(false, |g| g.voxel_brush_inflight.is_some()) {
                        if let Some(v) = self.sculpt.voxel.as_mut() {
                            requeue_primary_dabs_front(
                                &mut v.deferred_stamps,
                                &pts,
                                primary_stamp_count,
                            );
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    } else {
                    self.materialize_cpu_from_atlas();
                    let (box_density, box_mask): (Vec<f32>, Vec<f32>) = {
                        let v = self.sculpt.voxel.as_ref().unwrap();
                        let mask = if v.grid.has_mask() { v.grid.mask_subvolume(union) } else { Vec::new() };
                        (v.grid.density.subvolume(union), mask)
                    };
                    let infl = {
                        let g_preset = self.sculpt.voxel.as_ref().map_or(0, |v| v.falloff_preset.as_u32());
                        let g_pinch = self.sculpt.voxel.as_ref().map_or(1.0, |v| v.pinch_intensity);
                        let plane = self.sculpt.voxel.as_ref().map(crate::voxel_gpu_mesh::gpu_plane_knobs_from_voxel).unwrap_or_default();
                        let need_pinch_relax = is_pinch || plane.stack_gather; // Gather stack also needs pinch relax
                        let gpu = self.gpu.as_ref().unwrap();
                        gpu.voxel_brush.as_ref().unwrap()
                            .dispatch_stamps_region(
                                &gpu.device, &gpu.queue, gres, gbounds, giso, box_density, box_mask, union,
                                need_pinch_relax, g_hardness, g_preset, g_pinch, plane,
                                tip_frame.as_ref(), tip_contrast, stroke_tan, &stamps,
                            )
                    };
                    self.gpu.as_mut().unwrap().voxel_brush_inflight = Some(infl);
                    // Coat-class: NEVER Maintain::Wait on the sculpt input path.
                    // Collect is async — poll lands density next frame before the next raycast
                    // (see app_poll voxel_brush_collect). Same-frame Wait was the multi-ms hitch
                    // that made box-path clay feel nothing like 3D-Coat continuous volume sculpt.
                    // Release still does a blocking collect for a clean undo commit.
                    self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
            }
        } else {
            // Sculpt-vertex-paint: when the Paint Color toggle is on, every stamp ALSO deposits the chosen
            // BRUSH paint colour onto the voxels it touches (Blender-style). The colour is the tool's
            // `paint_color` — SEPARATE from the body material albedo (the panel's Albedo swatch, which is
            // the object's base material), so the painted voxels read distinct against the base. DENSE CPU
            // path only — deposit_color_sphere no-ops on a sparse grid (vertex-paint there is deferred).
            // Packed once per frame.
            let paint = self.sculpt.voxel.as_ref().filter(|v| v.paint_color_on)
                .map(|v| core_math::voxel_sculpt::pack_voxel_color(v.paint_color));
            let hardness = self.sculpt.voxel.as_ref().map_or(core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS, |v| v.falloff);
            // PHASE 0 (BRUSH_SYSTEM_ARCHITECTURE.md): build the active brush's `BrushRecipe` ONCE per
            // frame from the live tool state, then per-stamp fold it into a flat `StampPlan` POD and
            // run the new `apply_stamp_plan`. This is the single-producer site. The recipe carries the
            // real Direction / paint / pressure / symmetry for losslessness; the numeric resolution
            // (Direction → pt.normal, pressure → strength*pressure) already happened above/below, so
            // the StampPlan packs those resolved scalars and the result is BIT-IDENTICAL to the prior
            // `apply_brush` call (guarded by the golden parity test in core_math). Paint colour keeps
            // its existing `deposit_color_sphere` plumbing (Phase 0 writes only DENSITY through the plan).
            let recipe = {
                let v = self.sculpt.voxel.as_ref();
                let brush_dir = v.map_or(core_math::voxel_sculpt::BrushDirection::Surface, |v| v.brush_dir);
                let (p_str, p_rad) = v.map_or((false, false), |v| (v.pressure_enabled && v.pressure_to_strength,
                                                                    v.pressure_enabled && v.pressure_to_radius));
                let paint_packed = v.map_or(0, |v| core_math::voxel_sculpt::pack_voxel_color(v.paint_color));
                let paint_on = v.map_or(false, |v| v.paint_color_on);
                let sym = v.map_or([false; 3], |v| v.symmetry);
                let lazy_amt = v.map_or(0.0, |v| v.lazy);
                let spacing = v.map_or(0.10, |v| v.stroke_spacing.clamp(0.03, 0.50));
                let preset = v.map_or(0, |v| v.falloff_preset.as_u32());
                Some(core_math::brush::BrushRecipe::from_builtin_ex(
                    brush.to_builtin(), radius, strength, hardness, preset, brush_dir.to_recipe(),
                    paint_packed, paint_on, p_str, p_rad, sym, spacing, lazy_amt, true,
                ))
            };
            let mask_paint = self.sculpt.voxel.as_ref().map_or(false, |v| v.mask_paint);
            // Clone tip once per frame so apply can borrow it while `v.grid` is mutably borrowed.
            let tip_frame = self.sculpt.voxel.as_ref().and_then(|v| v.brush_tip.clone());
            let pinch_i = self.sculpt.voxel.as_ref().map_or(1.0, |v| v.pinch_intensity.max(0.0));
            // Frame stroke tangent (unit) for Pinch's stroke-axis basis. `stroke_dir` = hit - prev_pos
            // in voxel-local space; normalize so the Pinch axis is scale-free. ZERO on the first frame
            // of a stroke (no prior motion) → Pinch degrades to the axisymmetric point gather.
            let pinch_stroke_tangent = stroke_dir
                .map(|d| d.normalize_or_zero())
                .unwrap_or(glam::Vec3::ZERO);
            for pt in &pts {
                let rec = &mut self.sculpt.voxel_stroke; // disjoint field borrow from self.sculpt.voxel below
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    // Per-dab radius from brush dynamics (Random Radius); strength via pressure (incl. Random Strength).
                    let dab_r = (radius * pt.radius_scale).max(1e-4);
                    let dab_s = strength * pt.pressure;
                    if mask_paint {
                        // Mask paint mode: deposit protection (255) or clear (0 with Ctrl) — no density.
                        let val = if invert { 0u8 } else { 255u8 };
                        v.grid.deposit_mask_sphere(pt.position, dab_r, val, hardness);
                        continue;
                    }
                    // Move/Grab: the per-stamp DRAG DELTA is this stamp's displacement from the previous
                    // one in the stroke (carried across frames in `move_prev_pos`). The Move brush advects
                    // the density field along it; every other brush ignores it (passes Vec3::ZERO so there
                    // is no warp). `pt.position` is voxel-LOCAL, the same space `apply_stamp_plan` works in.
                    let drag_delta = if is_move {
                        let prev = v.move_prev_pos.unwrap_or(pt.position); // first stamp → zero delta
                        v.move_prev_pos = Some(pt.position);
                        pt.position - prev
                    } else if brush == VoxelBrush::Pinch {
                        // Pinch pulls toward the STROKE AXIS LINE (BLENDER_BRUSH_SPEC §2), not the dab
                        // point. It needs the stroke tangent: carry the frame stroke direction (unit) in
                        // drag_delta — the tangent axis of stroke_xz. Zero on the first stamp (no prior
                        // motion) falls back to the axisymmetric gather. Every other brush passes ZERO.
                        pinch_stroke_tangent
                    } else { glam::Vec3::ZERO };
                    // pt.pressure carries tablet pressure (1.0 for mouse) so strength scales with pen force.
                    // Fold the recipe + this stamp's resolved hit into a flat StampPlan, then apply it.
                    let hit = core_math::brush::SurfaceHit {
                        center: pt.position, normal: pt.normal, radius: dab_r, strength: dab_s,
                    };
                    let mods = core_math::brush::StrokeMods {
                        invert,
                        drag_delta,
                        pinch_intensity: pinch_i,
                        layer_height: v.layer_height.max(0.0),
                        layer_gain: v.layer_gain.max(0.0),
                        plane_offset: v.plane_offset.max(0.0),
                        plane_bias: v.plane_bias,
                        build_rate: v.build_rate.max(0.0),
                        // 1.0 = off (no density ceiling). Do NOT floor at 0.05 — that forced a live
                        // cap and made apply_cap dig solid down toward iso (brushes looked dead).
                        height_limit: v.height_limit.clamp(0.0, 1.0),
                        flatten_amount: v.flatten_amount.clamp(0.0, 2.0),
                        volume_preservation: v.volume_preservation.clamp(0.0, 1.0),
                        accumulate: v.accumulate,
                        inflate_speed: v.inflate_speed.clamp(0.0, 4.0),
                        inflate_direction: v.inflate_direction,
                        thickness_clamp: v.thickness_clamp.clamp(0.05, 1.0),
                        pinch_width: v.pinch_width.clamp(0.05, 4.0),
                        pinch_amount: v.pinch_amount.clamp(0.0, 2.0),
                        crease_width: v.crease_width.clamp(0.1, 4.0),
                        crease_depth: v.crease_depth.clamp(0.0, 4.0),
                        plane_angle: v.plane_angle.clamp(-1.0, 1.0),
                        cut_depth: v.cut_depth.clamp(0.0, 4.0),
                        fill_holes: v.fill_holes,
                        stack_gather: v.stack_gather,
                        stack_flatten: v.stack_flatten,
                        stack_fill: v.stack_fill,
                        stack_crease: v.stack_crease,
                        stack_inflate: v.stack_inflate,
                    };
                    // Auto Mask PRE-stamp (throttled at high-res — see GPU path comment).
                    // Skip Move/Smudge. Tint optional (Show).
                    if v.auto_mask.is_on()
                        && !matches!(brush, VoxelBrush::Move | VoxelBrush::Smudge)
                    {
                        // Throttle uses stamp index via pts position in loop — approximate by
                        // only running when strength*pressure is high enough OR first of frame.
                        // Full per-stamp at ≥512 was too costly; step by res.
                        let step = if v.grid.res >= 896 { 3u32 } else if v.grid.res >= 512 { 2 } else { 1 };
                        // Use a cheap hash of position so we still cover the stroke path.
                        let hx = (pt.position.x * 17.0 + pt.position.y * 31.0 + pt.position.z * 7.0).abs() as u32;
                        if hx % step == 0 {
                            v.grid.deposit_auto_mask_sphere(
                                pt.position,
                                dab_r,
                                hardness,
                                v.auto_mask,
                                v.auto_mask_intensity.clamp(0.0, 1.0),
                                v.auto_mask_invert,
                            );
                        }
                    }
                    // `recipe` is `Some` for every non-Mask brush (Mask `continue`s above), so this is the
                    // density path; unwrap is sound (the only `None` case is Mask, handled above).
                    let mut plan = core_math::brush::fold_recipe(recipe.as_ref().unwrap(), &hit, &mods);
                    // Random Rotation / Follow Stroke → tip UV basis (f32 bits in _pad1).
                    plan._pad1 = pt.tip_rotation.to_bits();
                    // Alpha Contrast rides drag.w (unused for non-Move ops).
                    plan.drag[3] = v.tip_contrast.clamp(0.05, 4.0);
                    v.grid.apply_stamp_plan_with_tip(&plan, tip_frame.as_ref(), Some(rec));
                    // Auto Smooth: mild post-stamp relax (skip pure Smooth brush — already relaxes).
                    // Mid-stroke Auto Smooth: skip at high res (release does one finish pass).
                    let defer_as = v.grid.res >= 512;
                    if v.auto_smooth
                        && !defer_as
                        && !matches!(brush, VoxelBrush::Smooth | VoxelBrush::Move | VoxelBrush::Smudge)
                    {
                        let amt = (v.auto_smooth_intensity * 0.45).clamp(0.05, 0.5);
                        if let Some(aabb) = v.grid.brush_aabb(pt.position, dab_r) {
                            let box6 = [aabb[0], aabb[1], aabb[2], aabb[3], aabb[4], aabb[5]];
                            v.grid.relax_region(box6, 1, amt, true, Some(rec));
                        }
                    }
                    if let Some(packed) = paint {
                        v.grid.deposit_color_sphere(pt.position, dab_r, packed);
                    }
                }
            }
        }
        // Profiler: record the BRUSH phase (density writes) for this frame.
        let brush_ms = t_brush.elapsed().as_secs_f32() * 1000.0;
        let stamps = pts.len() as u32;
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.profile.brush_ms = brush_ms;
            v.profile.stamps = stamps;
            v.profile.stamps_raw = raw_stamp_count as u32; // diagnostic: pre-subsample stamp count
            if brush_ms > v.profile.brush_peak { v.profile.brush_peak = brush_ms; }
        }

        // Queue ONLY the chunks each stamp actually passes through (a thin path) for re-mesh —
        // never the bounding box of the whole frame's stamps, whose box for a fast flick spans the
        // grid and used to re-scan every empty chunk in it (the 1.8s freeze). The queue is drained
        // at a capped rate by voxel_flush_dirty_chunks() in the per-frame mesh pass, so no single
        // frame can do unbounded re-mesh work (Frame Budget Rule). Wide-stencil normals keep the
        // live shading smooth (no faceted→smooth flip on release).
        let _ = mid; // single-mesh id no longer used during sculpt (chunked path renders)
        // Mark touched chunks for re-mesh. CPU path: the edit already landed in the grid this frame, so
        // mark the thin per-stamp path now. GPU (async) path: the edit lands NEXT frame at collect, so
        // `voxel_brush_collect` marks the (union) chunks then — marking here would mesh stale density.
        if !use_gpu_brush {
            let stamp_regions: Vec<[u32; 6]> = self.sculpt.voxel.as_ref().map(|v|
                pts.iter().filter_map(|pt| {
                    let r = (radius * pt.radius_scale).max(1e-4);
                    v.grid.brush_aabb(pt.position, r)
                }).collect()
            ).unwrap_or_default();
            let wrote = !stamp_regions.is_empty();
            for region in stamp_regions { self.voxel_mark_chunks_in_region(region); self.voxel_expand_stroke_aabb(region); }
            // The CPU brush wrote `grid.density` IN PLACE this frame (Move, or any brush with gpu_brush
            // off). Mark RENDER so viewport_render() re-meshes/re-draws the live edit THIS frame and
            // doesn't early-return at its `if !is_dirty` gate — the in-place density write is the
            // render-relevant mutation (mirrors the atlas path's RENDER mark; Voxel Sculpt rule).
            if wrote { self.mark_dirty(DirtyFlags::RENDER); }
        }
        if let Some(v) = self.sculpt.voxel.as_mut() { v.grid.take_dirty(); } // chunks tracked directly; drop the AABB
    }

    pub(crate) fn handle_sdf_sculpt_input(&mut self, response: &egui::Response, rect: egui::Rect, shift: bool, ctrl: bool) {
        if self.tool.work_mode != WorkMode::Object && self.tool.work_mode != WorkMode::Sculpt { return; }
        if self.modeling_context != ModelingContext::Sdf { return; }
        let prim_idx = match self.active_prim_index_unified() { Some(i) => i, None => return };
        if prim_idx >= self.scene.primitives.len() { return; }
        if !self.sdf_sculpt_active { return; }

        // Raycast helper (cursor → hit point on selected prim in local space)
        let raycast_prim = |app: &AzraApp, mp: egui::Pos2| -> Option<(Vec3, Vec3, Vec3)> {
            // VS11: canonical ray builder — handles orthographic projection.
            let (cam_pos, rd) = app.cam_ray(mp, rect);
            let prim = &app.scene.primitives[prim_idx];
            let mut t = 0.01_f32;
            for _ in 0..128 {
                let p = cam_pos + rd * t;
                let d = prim.world_distance(p);
                if d.abs() < 0.002 {
                    let eps = 0.001;
                    let nx = prim.world_distance(p + Vec3::X * eps) - prim.world_distance(p - Vec3::X * eps);
                    let ny = prim.world_distance(p + Vec3::Y * eps) - prim.world_distance(p - Vec3::Y * eps);
                    let nz = prim.world_distance(p + Vec3::Z * eps) - prim.world_distance(p - Vec3::Z * eps);
                    let world_normal = Vec3::new(nx, ny, nz).normalize_or_zero();
                    // Convert to local space
                    let inv_rot = prim.rotation_quat().inverse();
                    let local_hit = inv_rot * (p - prim.position) / prim.scale;
                    let local_normal = (inv_rot * world_normal).normalize_or_zero();
                    return Some((p, local_hit, local_normal));
                }
                if t > 100.0 { break; }
                t += d.max(0.002);
            }
            None
        };

        // Update brush preview position (for cursor drawing)
        if let Some(mp) = response.hover_pos() {
            if let Some((world_hit, _local_hit, _normal)) = raycast_prim(self, mp) {
                self.sdf_brush_preview_pos = Some(world_hit);
            } else {
                self.sdf_brush_preview_pos = None;
            }
        }

        // Determine effective sculpt op
        use core_math::primitives::*;
        let mut op = self.sdf_sculpt_op;
        if shift { op = SculptOp::Flatten; }
        else if ctrl { op = op.inverted(); }

        // On drag or hover with LMB: update the Sculpt modifier position
        let dragging = response.dragged_by(egui::PointerButton::Primary);
        let drag_started = response.drag_started_by(egui::PointerButton::Primary);
        if dragging || drag_started {
            if drag_started {
                self.begin_episode("Sculpt Stroke");
            }
            if let Some(mp) = response.interact_pointer_pos() {
                if let Some((_world_hit, local_hit, _normal)) = raycast_prim(self, mp) {
                    let radius = self.sdf_sculpt_radius;
                    let strength = self.sdf_sculpt_strength;
                    let op_f32 = op.to_f32();

                    self.write_prim(prim_idx, |prim| {
                        // Find or create the Sculpt modifier
                        let si = if let Some(i) = prim.modifiers.iter().position(|m| m.mod_type == ModifierType::Sculpt) {
                            i
                        } else {
                            prim.modifiers.push(Modifier {
                                mod_type: ModifierType::Sculpt,
                                strength,
                                axis: 0,
                                param2: op_f32,
                                height_map: None,
                                mask: VolumeMask {
                                    shape: MaskShape::Sphere,
                                    center: Vec3::ZERO,
                                    extents: Vec3::splat(radius),
                                    feather: 0.4,
                                    ..Default::default()
                                },
                            });
                            prim.modifiers.len() - 1
                        };

                        // Update modifier with current brush state
                        prim.modifiers[si].mask.center = local_hit;
                        prim.modifiers[si].mask.extents = Vec3::splat(radius);
                        prim.modifiers[si].strength = strength;
                        prim.modifiers[si].param2 = op_f32;
                        prim.modifiers[si].mask.shape = MaskShape::Sphere;

                        // Sync to legacy GPU slot
                        prim.sync_modifiers_to_gpu_fields();
                    });
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
        }
    }
}
