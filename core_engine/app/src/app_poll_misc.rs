// core_engine/app/src/app_poll_misc.rs
//
// Slice (app_poll de-god, PURE RELOCATION) — byte-for-byte method bodies moved out of app_poll.rs;
// no logic change. poll_background_tasks (orchestrator) stays in app_poll.rs and calls these.

use crate::*;
use eframe::egui;

impl AzraApp {
    /// Level 6 — Predictive warming: during idle frames, pre-bake bodies
    /// that don't yet have a cached grid so switching to them is instant.
    pub(crate) fn poll_warming(&mut self) {
        let _spike_t0 = std::time::Instant::now();
        if !self.cache.use_body_vol_cache { return; }
        // Track active body changes for MRU (runs every frame, cheap).
        // NESTED SCOPE: warm/track the TOP-LEVEL ancestor — nested sub-bodies never own a
        // grid, so tracking the raw nested nid pollutes the MRU with an unbakeable entry.
        let active = self.active_top_body();
        self.cache.warming.track_active_body(active);
        // Only warm during idle (no interaction for >= 3 frames)
        if self.render.interaction_idle_frames < 3 { return; }
        // Don't warm while bakes are already in flight
        if self.cache.body_vol_cache.has_pending() { return; }

        let target_res = self.cache.body_vol_cache.resolution;
        let candidate = self.cache.warming.pick_warming_candidate(
            active,
            &self.scene.scene_graph,
            |nid| self.cache.body_vol_cache.has_cached_grid_at_res(nid, target_res),
        );
        if let Some(body_nid) = candidate {
            self.cache.body_vol_cache.mark_body_dirty(body_nid);
            // Clear debounce so trigger_bake doesn't mistake this idle warming
            // bake for a rapid slider drag (which would enter auto-draft mode
            // and downgrade already-cached 128³ grids to 64³).
            self.cache.body_vol_cache.clear_debounce();
            self.trigger_body_vol_bake();
        }
        let _ms = _spike_t0.elapsed().as_secs_f64() * 1000.0;
        if _ms > 10.0 { eprintln!("[SPIKE] poll_warming: {:.1}ms", _ms); }
    }

    /// Poll GPU worker job results (non-blocking).
    /// Processes completed body volume bakes: uploads grid to viewport GPU volume.
    pub(crate) fn poll_gpu_jobs(&mut self) {
        if let Some(dispatcher) = &mut self.job_dispatcher {
            let results = dispatcher.poll_results();
            for result in results {
                match result.kind {
                    crate::gpu_jobs::GpuJobResultKind::BodyVolume { body_nid, grid, resolution, bake_window } => {
                        if self.cache.body_vol_cache.accept_gpu_result(body_nid, grid, result.generation, result.source_hash, None) {
                            self.mark_dirty(DirtyFlags::RENDER);
                            // GATE 2 — accept-time production of the single DirtyMeshRegion
                            // (worker-GPU path), BEFORE the upload/DC consumers below.
                            self.cache.body_vol_cache.note_bake_accepted(body_nid, bake_window, resolution);
                            // cut#1 (worker-GPU path): sync rv.buffer to the just-accepted grid before the
                            // DC reads it, so field-direct engages instead of the 512MB slot.field fallback
                            // (mirrors poll_primary_bake + the poll_body_vol_cache CPU path). Active body, Object Mode.
                            // NESTED SCOPE: bakes are top-level; resolve the active nid to its
                            // ancestor or this sync silently skips inside a sub-body scope.
                            if body_nid == self.active_top_body() {
                                if let Some(gpu) = &self.gpu {
                                    self.cache.body_vol_cache.update_resident_volume(
                                        body_nid, &gpu.device, &gpu.queue);
                                }
                            }
                            // Classify bricks from accepted grid
                            self.cache.body_vol_cache.classify_bricks_for_accepted_result(body_nid);
                            // Fast path: dispatch grid-based DC for the freshly baked body
                            self.dispatch_grid_dc_async(body_nid);
                            // Level 4: advance progressive resolution cascade (may push next level)
                            self.cache.body_vol_cache.advance_cascade(body_nid, &self.scene.scene_graph);
                        }
                    }
                    crate::gpu_jobs::GpuJobResultKind::Error { body_nid, message } => {
                        eprintln!("[WorkerGPU] Bake error for {:?}: {}", body_nid, message);
                        // Clear in-flight and mark dirty for CPU fallback retry
                        self.cache.body_vol_cache.clear_in_flight(body_nid);
                    }
                }
            }
        }
    }

    /// Poll pending async GPU pick (one-frame-late picking for rasterized mesh nodes).
    pub(crate) fn handle_pending_pick(&mut self) {
        let Some(ref pending) = self.pending_pick else {
            // No pick in flight — a stale material-drop marker (e.g. GPU lost) must not
            // hijack a FUTURE click's pick into an import.
            self.pending_material_drop = None;
            return;
        };
        let ctrl = pending.ctrl;
        let shift = pending.shift;
        let cpu_resolved = pending.cpu_resolved;
        if let Some(nid) = self.poll_pending_pick() {
            // The same-frame CPU raycast already handled this click. If the GPU agrees,
            // there is nothing to do - acting again would double-toggle (ctrl+click added
            // the node, the confirm removed it: "cannot select multiple objects"). A
            // DISAGREEING GPU result still corrects, since the GPU pick is the authority
            // on what the pixel actually shows.
            if cpu_resolved == Some(nid) {
                return;
            }
            if let Some(cpu_nid) = cpu_resolved {
                if ctrl || shift {
                    // Correction under multi-select: undo the CPU guess, apply the GPU truth.
                    self.toggle_select_unified(SceneItemId::Node(cpu_nid));
                }
            }
            // Library material drop (material_import.rs): the pick was dispatched by
            // `drop_library_material_on_viewport` — consume the hit as the DROP TARGET
            // instead of a selection click. Multi-select: paint every selected body
            // (and include the hit if it was not already selected).
            if let Some(item_idx) = self.pending_material_drop.take() {
                let mut targets = self.collect_library_material_import_targets();
                if targets.len() > 1 {
                    if !targets.contains(&nid) {
                        targets.push(nid);
                    }
                    self.import_library_material_to_nodes(item_idx, &targets);
                } else {
                    self.import_library_material_to_node(item_idx, nid);
                }
                return;
            }
            // Only override selection for mesh/non-SDF nodes (SDF bodies are
            // already handled by raycast_select on the click frame).
            if let Some(node) = self.scene.scene_graph.get(nid) {
                let multi = ctrl || shift;
                match &node.payload {
                    core_math::scene_graph::NodePayload::ImportedMesh { .. }
                    | core_math::scene_graph::NodePayload::VoxelBody { .. }
                    | core_math::scene_graph::NodePayload::PolygonalBody { .. }
                    | core_math::scene_graph::NodePayload::NurbsBody { .. } => {
                        if multi {
                            self.toggle_select_unified(SceneItemId::Node(nid));
                        } else {
                            self.select_scene_node(nid);
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    core_math::scene_graph::NodePayload::SdfBody { .. } => {
                        // SDF body — set active body + select
                        self.scene.active_body_nid = nid;
                        if multi {
                            self.toggle_select_unified(SceneItemId::Node(nid));
                        } else {
                            self.select_scene_node(nid);
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    _ => {}
                }
            }
        } else if self.pending_pick.is_none() {
            // Readback COMPLETED on background (poll cleared the pending pick, no node) —
            // a queued material drop has no target; drop it with feedback.
            if self.pending_material_drop.take().is_some() {
                self.toast("Drop on an object to apply the material");
            } else if !ctrl && !shift {
                // Empty-space click (GPU pixel id 0).
                //
                // Tool-drop escalation FIRST (owner 2026-08-08 SDF Bevel): first empty
                // click must exit Bevel/Extrude/G/R/S and KEEP the prim selection; only a
                // second empty click deselects. The click frame often sets `pending_pick`
                // (Hybrid `pick_node_at`) so `handle_object_click` skips the drop path —
                // without this, GPU empty-finish deselects while Bevel is still armed, or
                // deselects on the same gesture that dropped the tool.
                if self.drop_active_tool_only() {
                    self.mark_dirty(crate::DirtyFlags::RENDER);
                } else {
                    // No tool to drop → Object-mode Blender parity: clear selection.
                    let pre = self.capture_selection_snapshot();
                    self.deselect_scene_node();
                    self.deselect_all();
                    if !self.capture_selection_snapshot().same_as(&pre) {
                        self.record_selection_undo(pre);
                        self.sync_orbit_center_to_selection();
                        self.mark_dirty(crate::DirtyFlags::RENDER);
                    }
                }
            }
        }
        // pending_pick is cleared by poll_pending_pick when result arrives
    }

    /// Poll mesh export (non-blocking).
    pub(crate) fn poll_export(&mut self, ctx: &egui::Context) {
        if self.ui.export_rx.is_some() { ctx.request_repaint(); }
        if let Some(result) = self.ui.export_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.ui.export_rx = None;
            match result {
                Ok((filename, verts, faces, secs, is_quads)) => {
                    let face_type = if is_quads { "quads" } else { "tris" };
                    self.ui.last_mesh_stats = Some((verts, faces));
                    self.ui.export_status = Some(format!(
                        "\u{2705} {} \u{2014} {} verts, {} {} ({:.1}s)", filename, verts, faces, face_type, secs
                    ));
                    self.toast(&format!("\u{2705} Exported \u{2192} {}", filename));
                }
                Err(e) => { self.ui.export_status = Some(format!("\u{274C} {}", e)); }
            }
        }
    }
}
