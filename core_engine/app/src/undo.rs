//! Undo/redo system — moved from main.rs for modularity.

use std::sync::Arc;
use eframe::egui;
use crate::*;
use crate::input_sculpt::prune_front_undo_markers;

impl FourthLayerApp {
    // ---- Undo ----
    pub(crate) fn undo(&mut self) {
        self.tool.depth_pick = None; // prim indices invalidated
        // Settle any open drag episode (material layer sliders, gizmo) onto the stack so
        // Ctrl+Z undoes THAT edit instead of something older while the episode still mutates.
        self.commit_episode();
        // In sculpt mode: prefer voxel delta undo (O(changed voxels)) over full primitive rebake
        // NEW voxel room: undo against the density grid (re-meshes via Surface Nets).
        if self.tool.work_mode == WorkMode::Sculpt && self.sculpt.voxel.is_some() {
            // Cancel Move drag (held, not released) — display-only.
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.mesh_grab.is_active()) {
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    v.mesh_grab.cancel();
                    v.move_prev_pos = None;
                }
                self.sculpt.stroke_active = false;
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Undo: cancelled Move drag");
                return;
            }
            // ASYNC Move bake in flight: CANCEL it (never block Ctrl+Z). If grow already ran,
            // restore pre_grid so bounds/res don't stick without an undo entry.
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.pending_move_bake.is_some()) {
                let bake = self.sculpt.voxel.as_mut().and_then(|v| v.pending_move_bake.take());
                if let Some(bake) = bake {
                    let n = bake.baked_n;
                    if let Some(pre) = bake.pre_grid {
                        // Grow already resized the grid — restore snapshot (also clears pending grabs).
                        self.voxel_restore_grid_record(&pre);
                        eprintln!("[MOVEBAKE] undo: cancelled bake + restored pre-grow grid");
                    } else if let Some(v) = self.sculpt.voxel.as_mut() {
                        let n = n.min(v.pending_move_grabs.len());
                        if n > 0 { v.pending_move_grabs.drain(..n); }
                        if v.pending_move_grabs.is_empty() { v.move_voxelize_due = None; }
                    }
                    self.mark_dirty(DirtyFlags::RENDER);
                    self.toast("Undo: cancelled Move (bake dropped)");
                    return;
                }
            }
            // Shader-only pending Move stack (released but not yet landed / deferred).
            if self.sculpt.voxel.as_ref().map_or(false, |v| !v.pending_move_grabs.is_empty()) {
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    let n = 1.max(v.pending_move_grabs.len().min(2)); // at least one release group
                    // Prefer popping one primary (+mirror if present as pair): drain last N where N
                    // is 1 or remaining — without group sizes, pop all for safety if small.
                    let n = if v.pending_move_grabs.len() <= 8 {
                        v.pending_move_grabs.len()
                    } else {
                        n
                    };
                    let start = v.pending_move_grabs.len().saturating_sub(n);
                    v.pending_move_grabs.drain(start..);
                    if v.pending_move_grabs.is_empty() { v.move_voxelize_due = None; }
                }
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Undo: surface Move");
                return;
            }
            // An in-progress (un-committed) SURFACE stroke commits first — the Surface arm below
            // then pops exactly that entry, reverting the in-progress edits (density parity with
            // the voxel_stroke arm just under). Without this, a mid-stroke Ctrl+Z (pen still
            // down — common with tablets) popped the PREVIOUS stroke while the live snapshot kept
            // pre-undo verts ("caused by older stroke", owner-reported 2026-07-23). ONE lifecycle
            // exit: SculptState::surface_end_stroke.
            self.sculpt.surface_end_stroke();
            // An in-progress (un-committed) CPU/box stroke is reverted first — it has no order marker yet.
            if !self.sculpt.voxel_stroke.is_empty() {
                let delta = self.sculpt.voxel_stroke.take();
                let inv = self.voxel_restore_delta(&delta);
                self.sculpt.voxel_redo_stack.push(inv);
                self.sculpt.voxel_redo_order.push_back(UndoKind::Cpu);
                self.sculpt.stroke_active = false;
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Undo: voxel stroke");
                return;
            }
            // SURFACE MODE barrier: while un-voxelized mesh edits exist, a density-kind undo would
            // restore density + remesh, wiping the mesh edits (mesh is the authority). Surface
            // entries themselves pop normally; density entries below them refuse until revoxelized.
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some())
                && !matches!(self.sculpt.voxel_undo_order.back(), Some(UndoKind::Surface))
                && self.sculpt.voxel_undo_order.back().is_some()
            {
                self.toast("Undo blocked by surface edits — switch to Voxel mode to voxelize first");
                return;
            }
            // Phase A.2: order stream decides which system owns the next undo.
            match self.sculpt.voxel_undo_order.pop_back() {
                Some(UndoKind::Surface) => {
                    if self.voxel_surface_undo_apply(false) {
                        self.toast("Undo: surface stroke");
                    } else {
                        self.toast("Nothing to undo (empty surface history)");
                    }
                    return;
                }
                Some(UndoKind::Cpu) => {
                    if let Some(delta) = self.sculpt.voxel_undo_stack.pop_back() {
                        let inv = self.voxel_restore_delta(&delta);
                        self.sculpt.voxel_redo_stack.push(inv);
                        self.sculpt.voxel_redo_order.push_back(UndoKind::Cpu);
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.toast("Undo: voxel stroke");
                    } else {
                        self.toast("Nothing to undo (empty density history)");
                    }
                    return;
                }
                Some(UndoKind::Atlas) => {
                    if self.voxel_atlas_undo_apply(false) {
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.toast("Undo: voxel stroke");
                    }
                    return;
                }
                Some(UndoKind::CpuColor) => {
                    if let Some(delta) = self.sculpt.voxel_color_undo_stack.pop_back() {
                        let inv = self.voxel_restore_color_delta(&delta);
                        self.sculpt.voxel_color_redo_stack.push(inv);
                        self.sculpt.voxel_redo_order.push_back(UndoKind::CpuColor);
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.toast("Undo: voxel paint");
                    }
                    return;
                }
                Some(UndoKind::Grid) => {
                    if let Some(pre) = self.sculpt.voxel_grid_undo_stack.pop_back() {
                        let current = self.sculpt.voxel.as_ref().map(|v| v.grid.to_record());
                        self.voxel_restore_grid_record(&pre);
                        if let Some(cur) = current {
                            let ev = self.sculpt.voxel_grid_redo_stack.push(cur);
                            self.sculpt.voxel_redo_order.push_back(UndoKind::Grid);
                            prune_front_undo_markers(&mut self.sculpt.voxel_redo_order, UndoKind::Grid, ev);
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.toast("Undo: volume / Move grow");
                    } else {
                        self.toast("Nothing to undo (empty grid history)");
                    }
                    return;
                }
                None => {
                    eprintln!("[MOVEBAKE] undo: voxel order stream empty");
                    self.toast("Nothing to undo (sculpt history is empty)");
                    return;
                }
            }
        }
        if self.tool.work_mode == WorkMode::Sculpt {
            // Revert in-progress stroke first (without committing it)
            if !self.sculpt.current_stroke.is_empty() {
                let delta: Vec<(u32, f32)> = self.sculpt.current_stroke.drain().collect();
                let inverse = self.cache.cpu_volume.restore_delta(&delta);
                self.sculpt.voxel_redo_stack.push(inverse); // VS4: re-arm redo
                self.sculpt.stroke_active = false;
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Undo: coup de pinceau");
                return;
            }
            // Pop last committed stroke and restore only the changed voxels
            if let Some(delta) = self.sculpt.voxel_undo_stack.pop_back() {
                let inverse = self.cache.cpu_volume.restore_delta(&delta);
                self.sculpt.voxel_redo_stack.push(inverse); // VS4: re-arm redo
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Undo: coup de pinceau");
                return;
            }
        }
        // Fallback: primitive-level undo (triggers a full rebake in sculpt mode)
        if let Some(e) = self.history.undo_stack.pop_back() {
            let msg = format!("Undo: {}", e.description);
            eprintln!("[Undo] applying: {}", e.description);
            self.apply_undo_entry(e, true);
            // SAME-FRAME GPU upload, exactly like the position-edit drag path. The payload
            // branches above sync the mesh cache and queue a full-replace, but the actual GPU
            // write otherwise waits for the frame's mesh section — and an undo must be VISIBLE
            // the frame it happens (owner 2026-08-13: surface stuck after Ctrl+Z until the next
            // grab forced an upload). `upload_pending` is idempotent and time-budgeted, so doing
            // it here as well is safe on every path that reaches this point.
            if let Some(gpu) = &self.gpu {
                self.cache.mesh_cache.upload_pending(&gpu.device, &gpu.queue);
            }
            self.mark_dirty(DirtyFlags::RENDER);
            eprintln!("[Undo] applied + uploaded");
            self.toast(&msg);
        }
    }
    pub(crate) fn redo(&mut self) {
        self.tool.depth_pick = None;
        self.commit_episode();
        // NEW voxel room redo (symmetric with undo): the order-marker stream decides which system.
        if self.tool.work_mode == WorkMode::Sculpt && self.sculpt.voxel.is_some() {
            // Symmetric with undo: block while an ASYNC Move bake is in flight (see undo()).
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.pending_move_bake.is_some()) {
                self.toast("Voxelizing move — redo in a moment");
                return;
            }
            // Surface-mode Move redo (shader-only pending stack).
            if self.sculpt.voxel.as_ref().map_or(false, |v| !v.pending_move_redo.is_empty()) {
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    if let Some(group) = v.pending_move_redo.pop() {
                        let n = group.len();
                        v.pending_move_grabs.extend(group);
                        v.pending_move_group_sizes.push(n);
                        v.move_voxelize_due = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                    }
                }
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Redo: surface Move");
                return;
            }
            // An in-progress (un-committed) SURFACE stroke is a NEW edit: commit it, which
            // truncates redo across all kinds (same lifecycle exit as undo — see undo()).
            self.sculpt.surface_end_stroke();
            // SURFACE MODE barrier (symmetric with undo): density-kind redos are refused while
            // un-voxelized surface edits exist; Surface redos pop normally.
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some())
                && !matches!(self.sculpt.voxel_redo_order.back(), Some(UndoKind::Surface))
                && self.sculpt.voxel_redo_order.back().is_some()
            {
                self.toast("Redo blocked by surface edits — switch to Voxel mode to voxelize first");
                return;
            }
            match self.sculpt.voxel_redo_order.pop_back() {
                Some(UndoKind::Surface) => {
                    if self.voxel_surface_undo_apply(true) {
                        self.toast("Redo: surface stroke");
                    }
                    return;
                }
                Some(UndoKind::Cpu) => {
                    if let Some(inv) = self.sculpt.voxel_redo_stack.pop_back() {
                        let undo_delta = self.voxel_restore_delta(&inv);
                        self.sculpt.voxel_undo_stack.push(undo_delta);
                        self.sculpt.voxel_undo_order.push_back(UndoKind::Cpu);
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.toast("Redo: voxel stroke");
                    }
                }
                Some(UndoKind::Atlas) => {
                    if self.voxel_atlas_undo_apply(true) { self.toast("Redo: voxel stroke"); }
                }
                Some(UndoKind::CpuColor) => {
                    if let Some(inv) = self.sculpt.voxel_color_redo_stack.pop_back() {
                        let undo_delta = self.voxel_restore_color_delta(&inv);
                        self.sculpt.voxel_color_undo_stack.push(undo_delta);
                        self.sculpt.voxel_undo_order.push_back(UndoKind::CpuColor);
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.toast("Redo: voxel paint");
                    }
                }
                Some(UndoKind::Grid) => {
                    if let Some(rec) = self.sculpt.voxel_grid_redo_stack.pop_back() {
                        let current = self.sculpt.voxel.as_ref().map(|v| v.grid.to_record());
                        self.voxel_restore_grid_record(&rec);
                        if let Some(cur) = current {
                            let ev = self.sculpt.voxel_grid_undo_stack.push(cur);
                            self.sculpt.voxel_undo_order.push_back(UndoKind::Grid);
                            prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, UndoKind::Grid, ev);
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.toast("Redo: volume / Move grow");
                    }
                }
                None => {}
            }
            return;
        }
        // VS4: voxel redo branch (symmetric with undo) — re-applies the most recently
        // undone stroke and re-arms undo. Gated to sculpt mode like the undo branch.
        if self.tool.work_mode == WorkMode::Sculpt {
            if let Some(inverse) = self.sculpt.voxel_redo_stack.pop_back() {
                let undo_delta = self.cache.cpu_volume.restore_delta(&inverse);
                self.sculpt.voxel_undo_stack.push(undo_delta);
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Redo: coup de pinceau");
                return;
            }
        }
        if let Some(e) = self.history.redo_stack.pop_back() {
            let msg = format!("Redo: {}", e.description);
            self.apply_undo_entry(e, false);
            self.toast(&msg);
        }
    }

    /// Phase A.2 (GPU brick-snapshot undo/redo): pop one atlas entry from the undo (or redo) ring, build
    /// its INVERSE entry by snapshotting the CURRENT GPU POOL slots + current INDEX tags for the same
    /// bricks (GPU→GPU copy — NO readback), push the inverse to the opposite ring, then QUEUE the popped
    /// entry's GPU+CPU restore for the poll phase (GPU resource mutations must not run in an input/undo
    /// handler — Frame Ordering Contract). Returns true if an entry was applied.
    ///
    /// Handles are STABLE (atlas-undo strokes are never compacted), so the inverse snapshot can copy the
    /// current pool slot at the SAME handle the original captured — undo and redo round-trip exactly.
    fn voxel_atlas_undo_apply(&mut self, redo: bool) -> bool {
        // 1) Pop the source entry from the correct ring.
        let entry = match self.gpu.as_mut() {
            Some(g) => if redo { g.voxel_atlas_undo.pop_redo() } else { g.voxel_atlas_undo.pop_undo() },
            None => None,
        };
        let Some(entry) = entry else { return false; };

        // 2) Snapshot the CURRENT state for this entry's bricks → the inverse entry. Read current INDEX
        //    tags from the (stale-but-tag-current) CPU grid; tags are CPU bookkeeping kept current through
        //    `restore_atlas_undo`, so they reflect what is ON the GPU right now. Gather (bi, handle) pairs.
        let bricks: Vec<(u32, u32)> = entry.brick_indices.iter().copied()
            .zip(entry.handles.iter().copied()).collect();
        // Precompute current INDEX tags into a bi→tag map (O(1) lookup in the snapshot closure — the
        // entry can hold thousands of bricks, so a per-brick scan would be O(n²)).
        let tag_map: std::collections::HashMap<u32, u32> = match self.sculpt.voxel.as_ref() {
            Some(v) => entry.brick_indices.iter().map(|&bi| (bi, v.grid.density.index_tag(bi))).collect(),
            None => return false,
        };
        let affected = entry.affected_chunks.clone();
        // Build the inverse entry (disjoint borrow of gpu fields; tags closure reads the precomputed map).
        let inverse = {
            let gpu = self.gpu.as_ref().unwrap();
            let Some(atlas) = gpu.voxel_mesh_atlas.as_ref() else { return false; };
            crate::voxel_gpu_mesh::snapshot_current(
                &gpu.device, &gpu.queue, &atlas.pool_buf, &bricks, affected,
                |bi| tag_map[&bi])
        };

        // 3) Push the inverse onto the OPPOSITE ring + record its order marker.
        let g = self.gpu.as_mut().unwrap();
        if redo {
            g.voxel_atlas_undo.push_undo_keep_redo(inverse);
            self.sculpt.voxel_undo_order.push_back(UndoKind::Atlas);
        } else {
            g.voxel_atlas_undo.push_redo(inverse);
            self.sculpt.voxel_redo_order.push_back(UndoKind::Atlas);
        }

        // 4) Queue the popped entry's GPU pool + CPU index restore for the poll phase. push_back (NOT
        // overwrite) so two undos in one frame both apply — the poll drains the whole queue.
        self.sculpt.pending_atlas_restore.push_back(entry);
        true
    }

    /// Apply an undo entry, pushing the reverse operation to the opposite stack.
    fn apply_undo_entry(&mut self, entry: UndoEntry, is_undo: bool) {
        let desc = entry.description.clone();
        // Capture poly selection + material UI before payload is moved out of the entry.
        let poly_restore = UndoEntry {
            description: String::new(),
            payload: UndoPayload::Delta { changes: Vec::new() }, // unused placeholder
            active_body_nid: entry.active_body_nid,
            usel: entry.usel.clone(),
            poly_verts: entry.poly_verts.clone(),
            poly_edges: entry.poly_edges.clone(),
            poly_faces: entry.poly_faces.clone(),
            poly_active_vertex: entry.poly_active_vertex,
            material_panel_docs_json: entry.material_panel_docs_json.clone(),
            material_node_graphs_json: entry.material_node_graphs_json.clone(),
            poly_custom_data: entry.poly_custom_data.clone(),
        };
        // Build reverse entry before applying
        match entry.payload {
            UndoPayload::Full { primitives, scene_graph_snapshot } => {
                let reverse_prims = self.history.snapshot_prims(&self.scene.primitives);
                let (pv, pe, pf, pav) = self.reverse_poly_sel_fields();
                let (rev_docs, rev_graphs) = self.snapshot_material_ui();
                let rev_cd = self.snapshot_poly_custom_data();
                let reverse = UndoEntry {
                    description: desc,
                    payload: UndoPayload::Full {
                        primitives: reverse_prims,
                        scene_graph_snapshot: Some(self.scene.scene_graph.clone()),
                    },
                    active_body_nid: self.scene.active_body_nid,
                    usel: self.sel.usel.clone(),
                    poly_verts: pv,
                    poly_edges: pe,
                    poly_faces: pf,
                    poly_active_vertex: pav,
                    material_panel_docs_json: rev_docs,
                    material_node_graphs_json: rev_graphs,
                    poly_custom_data: rev_cd,
                };
                if is_undo { self.history.redo_stack.push_back(reverse); } else { self.history.undo_stack.push_back(reverse); }
                if let Some(sg) = scene_graph_snapshot {
                    // Restore graph, then rebuild flat cache FROM graph
                    // Lights are restored as part of the scene graph — no separate restore needed.
                    self.scene.scene_graph = sg;
                    // Working mesh is not in the graph snapshot — drop it or the next
                    // eval/display can keep every stroke that this undo just reverted.
                    self.poly_sculpt_working.clear();
                    self.sync_flat_cache();
                    // Restore poly sub-selection from the snapshot (transform undo keeps
                    // face/edge/vert selection; topology changes drop stale ids below).
                    self.restore_poly_sel_from_entry(&poly_restore);
                    // Material panel layer settings (Noise Scale, Adjust, …) + node graphs.
                    self.restore_material_ui_from_entry(&poly_restore);
                    // Refresh ALL poly bodies — do NOT key off live `usel.active_node()` here:
                    // usel is restored only AFTER this match arm, so post-delete undo would skip
                    // display sync (empty selection) and the restored mesh stayed invisible until
                    // a second undo dirtied MESH. Enqueue every poly body in the restored graph.
                    self.poly_sync_all_meshes();
                } else {
                    // Legacy entry without graph — restore flat cache, rebuild graph for consistency.
                    // Unreachable with current save_undo() (always captures graph), kept defensive.
                    eprintln!("[undo] WARNING: restoring undo entry without scene graph snapshot — rebuilding graph from flat cache");
                    self.scene.primitives = Arc::unwrap_or_clone(primitives);
                    self.build_graph_from_flat();
                    self.restore_poly_sel_from_entry(&poly_restore);
                    self.restore_material_ui_from_entry(&poly_restore);
                    self.poly_sync_all_meshes();
                }
                // The poly sidecar is runtime state OUTSIDE the graph snapshot — restored in the
                // same step (AFTER the graph, so the forced re-syncs see the restored meshes) so
                // Mark Sharp / Auto Smooth / UV / polygroup edits undo together with geometry.
                self.restore_poly_custom_data_from_entry(&poly_restore);
                // Undo/redo restores different prims — invalidate cache
                self.history.mark_prims_dirty();
                // MESH, not RENDER (owner 2026-08-13: "surface stuck after Ctrl+Z; it updates when
                // I edit again", console shows sync+upload both ran). A drag COWs the mesh cache id
                // (B.4 Shift+D divergence), so the restored graph can point at a DIFFERENT cid than
                // the structural draw list binds — and dirty.rs is the ONE place that rebuilds that
                // list. RENDER alone repainted the OLD cid's buffer forever; the next edit "fixed"
                // it only because the edit path marks MESH on structural change. Exactly the trap
                // the comment in poly_sync_after_position_edit warns about, on the undo path.
                self.mark_dirty(DirtyFlags::MESH);
            }
            UndoPayload::Poly { nid, ops } => {
                // Symmetric BATCH delta undo/redo (POLY_FOUNDATION_PLAN §4.6) — NOT a whole scene-graph
                // clone. The batch was applied in FORWARD order; therefore:
                //  - UNDO  (is_undo): apply each stored inverse in REVERSE order → PRE-op state. The
                //    redo entry keeps the SAME ops (redo re-runs them forward).
                //  - REDO  (!is_undo): re-run each op forward via `apply_in_place`, capturing a FRESH
                //    inverse per op → POST-op state + the inverses the next undo needs (parametric).
                use core_math::scene_graph::NodePayload;
                let ops_len = ops.len(); // captured before `ops` moves into the reverse entry.
                let mut reverse_ops: Option<Vec<(core_math::mesh_ops::PolyEdit, core_math::mesh_ops::OpDelta)>> = None;
                // Redo refusals, surfaced to the USER after the graph borrow ends (same bug class as
                // the forward path: an Err used to go to stderr only, where nobody sees it).
                let mut redo_lost: Vec<core_math::mesh_ops::OpError> = Vec::new();
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        let m = std::sync::Arc::make_mut(mesh);
                        if is_undo {
                            for (_op, inv) in ops.iter().rev() {
                                inv.apply_inverse(m);
                            }
                            reverse_ops = Some(ops.clone()); // redo re-runs these forward.
                        } else {
                            let mut fresh: Vec<(core_math::mesh_ops::PolyEdit, core_math::mesh_ops::OpDelta)> = Vec::with_capacity(ops.len());
                            for (op, _) in &ops {
                                match op.apply_in_place(m) {
                                    Ok(inv) => fresh.push((op.clone(), inv)),
                                    // Skip-and-continue, matching the forward path (:397) — safe for the
                                    // same reason: apply_in_place is atomic on Err. The old code did
                                    // `ok=false; break;`, which left `reverse_ops = None` (so NO undo
                                    // entry was pushed) even though the ops BEFORE the failing one had
                                    // already mutated the live mesh — an ORPHANED mutation the user
                                    // could not Ctrl+Z out of. Pushing an entry for whatever applied
                                    // keeps every mutation reachable by undo.
                                    Err(e) => redo_lost.push(e),
                                }
                            }
                            reverse_ops = if fresh.is_empty() { None } else { Some(fresh) };
                        }
                    }
                }
                if let Some(rev) = reverse_ops {
                    let (pv, pe, pf, pav) = self.reverse_poly_sel_fields();
                    let reverse = UndoEntry {
                        description: desc,
                        payload: UndoPayload::Poly { nid, ops: rev },
                        active_body_nid: self.scene.active_body_nid,
                        usel: self.sel.usel.clone(),
                        poly_verts: pv,
                        poly_edges: pe,
                        poly_faces: pf,
                        poly_active_vertex: pav,
                        material_panel_docs_json: None,
                        material_node_graphs_json: None,
                        poly_custom_data: None, // Euler-op delta: the sidecar is untouched
                    };
                    if is_undo { self.history.redo_stack.push_back(reverse); } else { self.history.undo_stack.push_back(reverse); }
                }
                // A redo step that refused is now VISIBLE.
                if let Some(e) = redo_lost.first() {
                    let msg = format!("\u{274C} Redo incomplete: {} of {} steps refused — {}",
                                      redo_lost.len(), ops_len, e);
                    self.toast(&msg);
                }
                // Restore poly selection from the entry (sanitize drops ids invalidated by the op).
                self.restore_poly_sel_from_entry(&poly_restore);
                self.poly_force_display_resync(nid);
                self.history.mark_prims_dirty();
                // Same reason as the Full branch above: geometry changed, the structural draw
                // list must rebuild, and only MESH does that.
                self.mark_dirty(DirtyFlags::MESH);
            }
            UndoPayload::Delta { changes } => {
                // Build reverse delta from current values at the same uids
                let reverse_changes: Vec<(u32, SdfPrimitive)> = changes.iter()
                    .filter_map(|(uid, _)| {
                        self.prim_index_by_uid(*uid).map(|idx| (*uid, self.scene.primitives[idx].clone()))
                    })
                    .collect();
                let (pv, pe, pf, pav) = self.reverse_poly_sel_fields();
                let reverse = UndoEntry {
                    description: desc,
                    payload: UndoPayload::Delta {
                        changes: reverse_changes,
                    },
                    active_body_nid: self.scene.active_body_nid,
                    usel: self.sel.usel.clone(),
                    poly_verts: pv,
                    poly_edges: pe,
                    poly_faces: pf,
                    poly_active_vertex: pav,
                    material_panel_docs_json: None,
                    material_node_graphs_json: None,
                    poly_custom_data: None, // prim/selection delta: the sidecar is untouched
                };
                if is_undo { self.history.redo_stack.push_back(reverse); } else { self.history.undo_stack.push_back(reverse); }
                // Apply delta: resolve uid → current idx, write old values, mirror to graph
                for (uid, prim) in changes {
                    if let Some(idx) = self.prim_index_by_uid(uid) {
                        self.scene.primitives[idx] = prim;
                        self.mirror_prim_to_graph(idx);
                    }
                }
                self.history.mark_prims_dirty();
            }
            UndoPayload::PolySculpt { nid, slot, orig } => {
                // Sparse swap (§7.5): restore `orig`, capture the CURRENT positions as the
                // reverse set. Direct: working + authored both restored. Layer: working
                // restored + `sculpt_deltas` REWRITTEN FROM THE PREFIX (delta channel is
                // delta-from-prefix, never raw positions).
                let post = self.poly_sculpt_apply_snapshot(nid, slot, &orig);
                let reverse = UndoEntry {
                    description: desc,
                    payload: UndoPayload::PolySculpt { nid, slot, orig: post },
                    active_body_nid: entry.active_body_nid,
                    usel: entry.usel.clone(),
                    poly_verts: entry.poly_verts.clone(),
                    poly_edges: entry.poly_edges.clone(),
                    poly_faces: entry.poly_faces.clone(),
                    poly_active_vertex: entry.poly_active_vertex,
                    material_panel_docs_json: None,
                    material_node_graphs_json: None,
                    poly_custom_data: None,
                };
                if is_undo {
                    self.history.redo_stack.push_back(reverse);
                } else {
                    self.history.undo_stack.push_back(reverse);
                }
            }
            UndoPayload::Selection { snap } => {
                // Selection-only undo. Build the reverse from the CURRENT selection (same pattern as the
                // Delta arm), swap the snapshot in, and RETURN EARLY — the heavy geometry tail below
                // (usel restore + SCENE|MESH|LIGHT dirty + rebake) must NOT run for a selection revert.
                let (pv, pe, pf, pav) = self.reverse_poly_sel_fields();
                let reverse = UndoEntry {
                    description: desc,
                    payload: UndoPayload::Selection { snap: self.capture_selection_snapshot() },
                    active_body_nid: self.scene.active_body_nid,
                    usel: self.sel.usel.clone(),
                    poly_verts: pv,
                    poly_edges: pe,
                    poly_faces: pf,
                    poly_active_vertex: pav,
                    material_panel_docs_json: None,
                    material_node_graphs_json: None,
                    poly_custom_data: None, // prim/selection delta: the sidecar is untouched
                };
                if is_undo { self.history.redo_stack.push_back(reverse); } else { self.history.undo_stack.push_back(reverse); }
                // Active body follows the snapshot's context so the restored selection is valid.
                self.scene.active_body_nid = entry.active_body_nid;
                self.history.last_recorded_selection = Some(snap.clone());
                self.restore_selection_snapshot(snap); // RENDER-only; sanitizes; no rebake.
                self.mark_outliner_dirty();
                return;
            }
        }
        // Restore full selection state
        self.scene.active_body_nid = entry.active_body_nid;
        self.sel.usel = entry.usel;
        self.sanitize_selection();
        // A geometry undo/redo just changed the live selection (restored usel + cleared poly sub-sel).
        // Re-baseline the coalescing key so the NEXT selection change is measured against this new
        // state — otherwise the first post-undo click would be compared to a stale pre-undo selection
        // and might wrongly skip (or double-record) its entry.
        self.history.last_recorded_selection = Some(self.capture_selection_snapshot());
        self.cache.rebake_hash = 0; // Force next rebake_no_undo to pass hash guard
        // Undo/redo may restore any body — clear stale grids and force rebake.
        // Must use invalidate_for_undo (not mark_all_bodies_dirty) because
        // stale cached grids can match restored hashes, causing trigger_bake
        // to skip the rebake via its hash guard.
        if self.cache.use_body_vol_cache {
            self.cache.body_vol_cache.invalidate_for_undo(&self.scene.scene_graph);
        }
        // Mesh Terrain: `terrain_world` is runtime-only (not in the graph snapshot). Undo/redo
        // can delete TerrainTile_* nodes while the map still holds dead NodeIds — heal from
        // the restored graph (Create Patch → Ctrl+Z is the owner repro).
        self.rebuild_terrain_world_from_scene();
        // Mesh-mod stack may have changed in the restored graph while mesh_cache still holds
        // the pre-undo folded display — force re-sync on next ensure (P1b/P4 robustness).
        self.invalidate_all_mesh_mod_displays();
        self.mark_dirty(DirtyFlags::LIGHT | DirtyFlags::SCENE | DirtyFlags::MESH);
        self.mark_outliner_dirty();
    }
    pub(crate) fn pop_last_undo(&mut self) { self.history.undo_stack.pop_back(); }

    // ==== Transaction API (see TRANSACTION_DESIGN.md) ====

    /// Snapshot material panel docs + node graphs as JSON maps (for Ctrl+Z of layer settings).
    ///
    /// Docs are **refreshed from live graph materials** before serialize so async map uploads
    /// (library drop / file load) that write `PbrMaterial` tex slots without updating the stored
    /// doc (`mp_mark_hand_loaded_map` keeps `tex_id: 0` until display refresh) are not snapshotted
    /// as empty maps. That stale-doc snapshot used to wipe every object material on the next
    /// Full undo via `restore_material_ui_from_entry` → `lower_to_material`.
    fn snapshot_material_ui(
        &self,
    ) -> (
        Option<std::sync::Arc<std::collections::HashMap<u32, String>>>,
        Option<std::sync::Arc<std::collections::HashMap<u32, String>>>,
    ) {
        use std::collections::HashMap;
        use std::sync::Arc;
        let mut docs = HashMap::new();
        for (mid, doc) in &self.ui.material_panel_docs {
            let mut doc = doc.clone();
            if let Some((_, mat)) = self
                .scene
                .scene_graph
                .material_by_id(core_math::primitives::MaterialId(*mid))
            {
                // G3: heal the async-upload tex_id hole only — never a full reverse sync.
                crate::material_panel::lower::heal_doc_fast_texids(&mut doc, mat);
            }
            if let Ok(s) = serde_json::to_string(&doc) {
                docs.insert(*mid, s);
            }
        }
        let mut graphs = HashMap::new();
        for (mid, g) in &self.ui.material_node_graphs {
            if let Ok(s) = serde_json::to_string(g) {
                graphs.insert(*mid, s);
            }
        }
        // Always capture (even empty) so restore can clear after a first-edit snapshot.
        (Some(Arc::new(docs)), Some(Arc::new(graphs)))
    }

    /// Restore material panel docs + node graphs from an undo entry (if present).
    ///
    /// Graph materials are already restored from `scene_graph_snapshot` (Full undo) — that
    /// payload is authoritative for viewport shading. We restore the panel doc store for UI
    /// fidelity, then **refresh docs FROM materials** (not lower docs ONTO materials).
    ///
    /// Re-lowering was wrong: stored docs often lag `tex_id` after async map upload
    /// (`mp_mark_hand_loaded_map` writes `tex_id: 0` and never patches the store). Lowering
    /// those placeholders cleared every map slot → "Ctrl+Z and all objects lose materials".
    fn restore_material_ui_from_entry(&mut self, entry: &UndoEntry) {
        let Some(docs_arc) = entry.material_panel_docs_json.as_ref() else {
            return; // geometry-only entry — leave panel UI alone
        };
        self.ui.material_panel_docs.clear();
        for (mid, json) in docs_arc.iter() {
            if let Ok(doc) = serde_json::from_str::<crate::material_panel::model::MaterialPanelDoc>(json)
            {
                self.ui.material_panel_docs.insert(*mid, doc);
            }
        }
        if let Some(graphs_arc) = entry.material_node_graphs_json.as_ref() {
            self.ui.material_node_graphs.clear();
            for (mid, json) in graphs_arc.iter() {
                if let Ok(g) = serde_json::from_str::<crate::shader_nodes::NodeGraph>(json) {
                    self.ui.material_node_graphs.insert(*mid, g);
                }
            }
            // Keep open node editor in sync if it targets a restored material.
            if let Some(crate::NodeGraphTarget::Material(mid)) = self.ui.node_graph_target {
                if let Some(g) = self.ui.material_node_graphs.get(&mid.0) {
                    self.ui.node_graph = g.clone();
                }
            }
        }
        // Drop docs for materials that no longer exist in the restored graph (deleted mid).
        let live_mids: std::collections::HashSet<u32> = self
            .scene
            .scene_graph
            .materials()
            .map(|(_, mid, _, _)| mid.0)
            .collect();
        self.ui.material_panel_docs.retain(|mid, _| live_mids.contains(mid));
        self.ui.material_node_graphs.retain(|mid, _| live_mids.contains(mid));

        // G3: no restore-side re-sync. Docs, graphs and materials were snapshotted in
        // the SAME frame (`snapshot_material_ui` + scene_graph_snapshot), so the three
        // restored states are consistent by construction; the capture-side heal already
        // patched any async-upload tex_id holes before serialize. The old loop here
        // (refresh docs FROM materials + pull materials INTO graphs) was the reverse
        // sync pair this plan exists to delete.
        // Force structural material list to re-gather even if material_generation
        // numerically matched the pre-undo cache (graph clone restores the old counter).
        self.scene.scene_graph.bump_material_generation();
        self.cache.prim_buf_stale = true;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.temporal_frame_valid = false;
        }
    }

    /// Create an UndoEntry capturing the current pre-mutation state.
    /// All data is eagerly cloned/Arc'd — no references held after return.
    fn snapshot_undo(&mut self, desc: &str) -> UndoEntry {
        let prims_arc = self.history.snapshot_prims(&self.scene.primitives);
        let sg_clone = self.scene.scene_graph.clone();
        let (docs, graphs) = self.snapshot_material_ui();
        let cd_snapshot = self.snapshot_poly_custom_data();
        let entry = UndoEntry {
            description: desc.into(),
            payload: UndoPayload::Full {
                primitives: prims_arc,
                scene_graph_snapshot: Some(sg_clone),
            },
            active_body_nid: self.scene.active_body_nid,
            usel: self.sel.usel.clone(),
            poly_verts: self.poly_sel.selected_vertices.clone(),
            poly_edges: self.poly_sel.selected_edges.clone(),
            poly_faces: self.poly_sel.selected_faces.clone(),
            poly_active_vertex: self.poly_active_vertex,
            material_panel_docs_json: docs,
            material_node_graphs_json: graphs,
            poly_custom_data: cd_snapshot,
        };
        self.history.mark_prims_dirty();
        entry
    }

    /// Snapshot the runtime poly `CustomData` sidecar for an undo entry.
    ///
    /// ALWAYS `Some`, even when the map is empty. `None` means "this entry does not OWN sidecar
    /// state" (the lightweight delta/selection entries) — it must never also mean "the sidecar was
    /// empty", or the very first Mark Sharp on a scene would be un-undoable: its pre-state IS the
    /// empty map, and skipping the restore would leave the marks in place after Ctrl+Z.
    /// Cheap regardless — the columns are sparse maps/sets, orders of magnitude smaller than the
    /// scene-graph clone already riding in the same entry.
    fn snapshot_poly_custom_data(
        &self,
    ) -> Option<Arc<std::collections::HashMap<core_math::scene_graph::NodeId, core_math::poly_mesh::CustomData>>>
    {
        Some(Arc::new(self.cache.poly_custom_data.clone()))
    }

    /// Restore the poly sidecar captured on `entry`, and force the affected bodies to re-bake.
    ///
    /// A no-op when the entry carries none (lightweight delta/selection entries never own it).
    /// Bodies present NOW but absent from the snapshot are dropped — the snapshot is the whole
    /// authored state at that moment, not a patch.
    fn restore_poly_custom_data_from_entry(&mut self, entry: &UndoEntry) {
        let Some(snapshot) = entry.poly_custom_data.as_ref() else { return };
        self.cache.poly_custom_data = (**snapshot).clone();
        // The sidecar feeds the display bake (normals via Mark Sharp / Auto Smooth), and the
        // display cache key folds it — bump the generation so the memo cannot serve a pre-undo key.
        self.bump_poly_edit_generation();
        let nids: Vec<core_math::scene_graph::NodeId> =
            self.cache.poly_custom_data.keys().copied().collect();
        for nid in nids {
            self.poly_force_display_resync(nid);
        }
    }

    /// Fill poly sub-element selection onto a reverse entry (current live state).
    pub(crate) fn reverse_poly_sel_fields(&self) -> (
        std::collections::HashSet<u32>,
        std::collections::HashSet<u32>,
        std::collections::HashSet<u32>,
        Option<u32>,
    ) {
        (
            self.poly_sel.selected_vertices.clone(),
            self.poly_sel.selected_edges.clone(),
            self.poly_sel.selected_faces.clone(),
            self.poly_active_vertex,
        )
    }

    /// Restore poly sub-selection from an undo entry; drop ids that no longer exist on the mesh.
    fn restore_poly_sel_from_entry(&mut self, entry: &UndoEntry) {
        self.poly_sel.selected_vertices = entry.poly_verts.clone();
        self.poly_sel.selected_edges = entry.poly_edges.clone();
        self.poly_sel.selected_faces = entry.poly_faces.clone();
        self.poly_active_vertex = entry.poly_active_vertex;
        self.poly_sel.hovered_face = None;
        self.poly_sel.hovered_edge = None;
        self.poly_sel.hovered_vertex = None;
        // Drop stale face/edge/vert ids after topology-changing undos.
        if let Some(nid) = self.sel.usel.active_node() {
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    self.poly_sel.selected_faces.retain(|&f| {
                        (f as usize) < mesh.faces.len()
                            && mesh.face(core_math::half_edge::FaceId(f)).half_edge
                                != core_math::half_edge::NO_EDGE
                    });
                    self.poly_sel.selected_edges.retain(|&e| mesh.is_edge_live(core_math::half_edge::HalfEdgeId(e)));
                    self.poly_sel.selected_vertices.retain(|&v| {
                        (v as usize) < mesh.vertices.len()
                            && mesh.vert(core_math::half_edge::VertexId(v)).half_edge
                                != core_math::half_edge::NO_EDGE
                    });
                    if let Some(av) = self.poly_active_vertex {
                        if !self.poly_sel.selected_vertices.contains(&av) {
                            self.poly_active_vertex = None;
                        }
                    }
                }
            }
        }
    }

    /// Capture the CURRENT selection of both systems (unified + poly sub-element) into a snapshot.
    /// Cheap — a handful of HashSet/IndexSet clones. The single source for selection undo entries.
    pub(crate) fn capture_selection_snapshot(&self) -> app_types::SelectionSnapshot {
        app_types::SelectionSnapshot {
            usel: self.sel.usel.clone(),
            poly_verts: self.poly_sel.selected_vertices.clone(),
            poly_edges: self.poly_sel.selected_edges.clone(),
            poly_faces: self.poly_sel.selected_faces.clone(),
            poly_active_vertex: self.poly_active_vertex,
        }
    }

    /// Restore both selection systems from a snapshot. Selection is view-only state, so this marks
    /// only RENDER dirty — no rebake, no cache invalidation (DirtyFlags rule). Sanitizes unified
    /// selection against the live graph; poly ids are sanitized by the caller against the active mesh.
    fn restore_selection_snapshot(&mut self, snap: app_types::SelectionSnapshot) {
        self.sel.usel = snap.usel;
        self.poly_sel.selected_vertices = snap.poly_verts;
        self.poly_sel.selected_edges = snap.poly_edges;
        self.poly_sel.selected_faces = snap.poly_faces;
        self.poly_active_vertex = snap.poly_active_vertex;
        self.sanitize_selection();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Record a selection-only undo step (Blender parity: Ctrl+Z reverts selection before geometry).
    /// Call this BEFORE a selection-changing input applies, passing the PRE-change selection. Coalesced:
    /// records nothing if the pre-change selection equals the last-recorded one (a re-click on the same
    /// element, a redundant clear), so the undo stack isn't spammed. One entry per genuine change.
    ///
    /// This is NOT a `transaction()` — wrapping a click in transaction() would clone the whole scene
    /// graph and rebake per click (wrong). It pushes a lightweight `Selection` payload onto the SAME
    /// undo stack (no parallel path), keeping the transaction model intact.
    pub(crate) fn record_selection_undo(&mut self, pre: app_types::SelectionSnapshot) {
        // Only record if this pre-state differs from what we last recorded — the coalescing guard.
        if let Some(last) = &self.history.last_recorded_selection {
            if last.same_as(&pre) {
                return;
            }
        }
        let entry = UndoEntry {
            description: "Selection".into(),
            payload: UndoPayload::Selection { snap: pre.clone() },
            active_body_nid: self.scene.active_body_nid,
            usel: pre.usel.clone(),
            poly_verts: pre.poly_verts.clone(),
            poly_edges: pre.poly_edges.clone(),
            poly_faces: pre.poly_faces.clone(),
            poly_active_vertex: pre.poly_active_vertex,
            material_panel_docs_json: None,
            material_node_graphs_json: None,
            poly_custom_data: None, // selection-only: never touches the sidecar
        };
        self.history.push(entry);
        self.history.last_recorded_selection = Some(pre);
        self.project.scene_modified = true;
    }

    /// Atomic transaction: snapshot pre-state, execute closure, push undo entry.
    /// Use for one-shot mutations (button clicks, menu actions, keyboard shortcuts).
    pub(crate) fn transaction(&mut self, desc: &str, f: impl FnOnce(&mut Self)) {
        self.commit_episode(); // auto-commit any dangling episode
        let entry = self.snapshot_undo(desc);
        f(self);
        self.history.push(entry);
        self.project.scene_modified = true;
        // ACTION-LEVEL outliner refresh: every atomic user mutation may change what the
        // outliner shows (structure, names, order). Marking here — not per call-site —
        // eliminates the "drop applies but panel shows stale rows" class structurally
        // (the DnD release runs MID-draw; see outliner_draw.rs dirty-consumption contract).
        self.mark_outliner_dirty();
    }

    /// Run a BATCH of poly Euler ops on body `nid` as ONE delta-undo action (POLY_FOUNDATION_PLAN
    /// §4.6, Phase 2). The ONE app entry point for the wrapped Euler ops — routes through the EXISTING
    /// undo stack via a single `UndoPayload::Poly` entry (no parallel undo path, CLAUDE.md). Each op
    /// runs IN PLACE on `Arc::make_mut(mesh)`, capturing its inverse `OpDelta` (O(touched) `Element`
    /// for local ops, honest O(body) `Snapshot` otherwise). A local-op undo does NOT clone the whole
    /// body — the delta-undo win. The whole batch is ONE Ctrl+Z.
    ///
    /// Ops that refuse cleanly (bad id/param) are SKIPPED (not recorded) so a stale id in a multi-
    /// select batch never aborts the rest — this per-op partial model matches Blender, which
    /// dissolves the applicable edges of a mixed selection and silently leaves the rest (verified
    /// against a headless Blender oracle, 2026-07-15). Skipping is safe ONLY because
    /// `apply_in_place` is atomic on `Err` (mesh_ops/mod.rs:307).
    ///
    /// Returns a `PolyOpOutcome` CARRYING the refused ops + their `OpError`s — never a bare bool.
    /// The bool could not distinguish "all 5 dissolved" from "1 of 5 dissolved", so every caller
    /// toasted unconditional success and cleared the whole selection. Report via
    /// `poly_report_batch()`; `#[must_use]` on the outcome makes ignoring it a compile warning.
    pub(crate) fn poly_op_batch(&mut self, nid: core_math::scene_graph::NodeId, ops: Vec<core_math::mesh_ops::PolyEdit>, desc: &str) -> crate::poly::outcome::PolyOpOutcome {
        use core_math::scene_graph::NodePayload;
        use crate::poly::outcome::PolyOpOutcome;
        self.commit_episode(); // auto-commit any dangling episode (same discipline as transaction()).

        // Domain BEFORE `ops` is consumed — the reporter needs it even when nothing refuses.
        let domain = ops.first().and_then(|op| op.targets()).map(|(d, _)| d);
        debug_assert!(
            ops.iter().filter_map(|o| o.targets()).all(|(d, _)| Some(d) == domain),
            "mixed-domain poly batch — poly_report_batch owns exactly ONE domain's selection",
        );

        let (applied, refused) = {
            let node = match self.scene.scene_graph.get_mut(nid) { Some(n) => n, None => return PolyOpOutcome::not_run() };
            let mesh = match &mut node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return PolyOpOutcome::not_run(),
            };
            let m = std::sync::Arc::make_mut(mesh);
            let mut applied = Vec::with_capacity(ops.len());
            let mut refused = Vec::new();
            for op in ops {
                match op.apply_in_place(m) {
                    Ok(inv) => applied.push((op, inv)),
                    // The refused op MOVES into the outcome (no clone) carrying its own target ids,
                    // so the caller can keep exactly those elements selected.
                    Err(e) => refused.push((op, e)),
                }
            }
            (applied, refused)
        };
        // Element count BEFORE `applied` moves into the undo entry. Counted from the ops' own
        // targets so a region op (one DissolveFaces over N faces) reports N, not 1.
        let applied_elems = applied.iter()
            .filter_map(|(op, _)| op.targets())
            .map(|(_, ids)| ids.len())
            .sum();
        let out = PolyOpOutcome { applied_ops: applied.len(), applied_elems, refused, domain, ran: true };
        if applied.is_empty() { return out; } // no undo record for a fully-refused batch (unchanged).

        let entry = UndoEntry {
            description: desc.into(),
            payload: UndoPayload::Poly { nid, ops: applied },
            active_body_nid: self.scene.active_body_nid,
            usel: self.sel.usel.clone(),
            poly_verts: self.poly_sel.selected_vertices.clone(),
            poly_edges: self.poly_sel.selected_edges.clone(),
            poly_faces: self.poly_sel.selected_faces.clone(),
            poly_active_vertex: self.poly_active_vertex,
            material_panel_docs_json: None,
            material_node_graphs_json: None,
            poly_custom_data: None, // selection-only: never touches the sidecar
        };
        self.history.push(entry);
        self.history.mark_prims_dirty();
        self.project.scene_modified = true;

        // Hard resync: drop display-cache short-circuit + GPU mesh so the solid surface
        // always rebuilds after dissolve/delete (owner: "Ctrl+X hides all surface until undo").
        self.poly_force_display_resync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        out
    }

    /// Convenience: run a SINGLE poly Euler op with delta undo (1-element batch). Returns the same
    /// `PolyOpOutcome` as the batch — a bool-returning sibling would be a second code path.
    pub(crate) fn poly_op(&mut self, nid: core_math::scene_graph::NodeId, op: core_math::mesh_ops::PolyEdit, desc: &str) -> crate::poly::outcome::PolyOpOutcome {
        self.poly_op_batch(nid, vec![op], desc)
    }

    /// Begin an undo episode. Captures pre-mutation state, held until commit/cancel.
    /// If a previous episode is still active, auto-commits it first.
    /// Use for continuous drags (sliders, gizmo transforms).
    pub(crate) fn begin_episode(&mut self, desc: &str) {
        self.commit_episode(); // auto-commit any previous episode
        let entry = self.snapshot_undo(desc);
        self.pending_episode = Some(entry);
        // Suppress bake dispatch during drag — dirty regions accumulate,
        // one combined bake fires when the episode commits/cancels.
        // EXCEPTION: GPU SDF meshing (Phase G) — the GPU mesh is ~2-5ms, fast enough to re-mesh
        // every drag frame, so bakes must fire live too. Suppressing here was the "no live editing
        // feeling": Object-Mode CPU DC was too slow to bake per frame, so it deferred to release,
        // relying on an analytical march that is disabled. GPU meshing removes that constraint.
        // GATE 3: suppress only when the CPU chunked-DC oracle is the route (too slow for
        // per-frame drag bakes). Any GPU route is fast enough to bake live.
        if self.cache.dc_mesh_route(false) == crate::cache_state::DcMeshRoute::CpuChunkedOracle {
            self.cache.body_vol_cache.suppress_bake_dispatch = true;
        }
        // Draft resolution during interaction is DISABLED — causes atlas slab corruption
        // when draft (64³) writes into a full-res (128³) slab. Partial bakes (10% of grid)
        // already provide fast interaction feedback without resolution mismatch.
        // self.cache.body_vol_cache.draft_resolution = Some((full_res / 2).max(64));
    }

    /// Commit the active episode, pushing the pre-mutation snapshot to undo stack.
    /// No-op if no episode is active.
    pub(crate) fn commit_episode(&mut self) {
        // PR-5/6 DOCUMENT-COMMIT CHOKE POINT (§7.4): every episode closer passes through
        // here (LMB-up, auto-commit, mid-stroke Ctrl+Z — `undo()` commits first — and a
        // later tool's `begin_episode`). A pending PolySculpt episode writes the document
        // ONCE now, then its payload gets the drained first-touch set; a dab-less stroke
        // pushes NOTHING (no junk entries). This runs BEFORE the push and BEFORE
        // `poly_remesh_reconcile_diverged` — order is the correctness (§7.4).
        let sculpt_pending: Option<(core_math::scene_graph::NodeId, Option<usize>)> =
            match self.pending_episode.as_ref().map(|e| &e.payload) {
                Some(UndoPayload::PolySculpt { nid, slot, .. }) => Some((*nid, *slot)),
                _ => None,
            };
        if let Some((nid, slot)) = sculpt_pending {
            // DYNTOPO TOPO-DIVERGED STROKE (DYNTOPO_PLAN B3): the working mesh changed
            // TOPOLOGY, so the sparse position-swap cannot represent this stroke. The
            // document is still PRE-STROKE right here (working is truth mid-stroke), so:
            //   1. capture a standard Full snapshot NOW — that IS the pre-stroke state
            //      (mesh Arcs make the graph clone cheap);
            //   2. install the working mesh through the SAME count-mismatch path the
            //      enter-time remesh bake uses (`poly_sculpt_write_direct`);
            //   3. drop the working — its prefix is stale against the new document; the
            //      next stroke rebuilds from the committed mesh.
            // Undo/redo then flow through the battle-tested Full arm. Direct sculpt only
            // (`slot == None`) — the refine driver refuses layer slots up front.
            let topo_diverged = slot.is_none()
                && self.poly_sculpt_working.get(&nid).is_some_and(|w| {
                    self.scene.scene_graph.get(nid).is_some_and(|n| match &n.payload {
                        core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => {
                            mesh.vertex_count() != w.mesh.vertex_count()
                                || mesh.face_count() != w.mesh.face_count()
                        }
                        _ => false,
                    })
                });
            if topo_diverged {
                let entry = self.snapshot_undo("Dyntopo stroke");
                self.pending_episode = None;
                self.poly_sculpt_write_direct(nid);
                self.poly_sculpt_drop(nid);
                self.history.push(entry);
                self.project.scene_modified = true;
                self.poly_sync_mesh_cache(nid);
                self.overlay_lines_fp = u64::MAX;
                self.mark_dirty(DirtyFlags::MESH | DirtyFlags::RENDER);
                // Episode-commit parity with the shared tail below (pending is None, so
                // its `had_episode` block will not run for this stroke).
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
            }
            let has_touch = self
                .poly_sculpt_working
                .get(&nid)
                .map(|w| !w.first_touch.is_empty())
                .unwrap_or(false);
            if topo_diverged {
                // handled above — pending already cleared
            } else if !has_touch {
                self.pending_episode = None;
            } else {
                // Write FIRST (reads first_touch), then drain it into the payload.
                self.poly_sculpt_commit_document(nid, slot);
                let orig: Vec<(u32, [f32; 3])> = self
                    .poly_sculpt_working
                    .get_mut(&nid)
                    .map(|w| {
                        w.first_touch
                            .drain()
                            .map(|(i, p)| (i, p.to_array()))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(e) = self.pending_episode.as_mut() {
                    if let UndoPayload::PolySculpt { orig: o, .. } = &mut e.payload {
                        *o = orig;
                    }
                }
            }
        }
        let had_episode = self.pending_episode.is_some();
        if let Some(entry) = self.pending_episode.take() {
            self.history.push(entry);
            self.project.scene_modified = true;
        }
        self.cache.body_vol_cache.draft_resolution = None; // restore full res
        // Always clear suppress — even if no episode was active (safety net).
        self.cache.body_vol_cache.suppress_bake_dispatch = false;
        // M1.c — PT restart once per episode commit (not per scrub tick). Hybrid covered the drag.
        if had_episode {
            self.render.pt_accumulated_frames = 0;
            self.render.pt_converged = false;
        }
        // V1 incremental Remesh reconcile: the drag is over. Re-eval while the
        // retained grid is still live so an AABB-preserving drag only pays from_mesh
        // (B: draft → manifold). KEEP the cache — dropping it forced the next grab
        // to start with a 1.25-headroom full remesh (diverged) and another full
        // remesh on release (~20 ms hitch at 110, ~60 ms at 180).
        // Diverged lattices still fall through to a stateless eval inside that
        // resync, which refreshes the cache to the canonical lattice.
        if had_episode {
            self.poly_remesh_reconcile_diverged();
        }
    }

    /// Re-evaluate every body left on a latched remesh lattice, statelessly. Safe to call
    /// with an empty set (no-op) and idempotent — the set is drained first, and each
    /// resync evaluates outside any episode so it cannot re-latch.
    pub(crate) fn poly_remesh_reconcile_diverged(&mut self) {
        if self.poly_remesh_diverged.is_empty() {
            return;
        }
        let pending: Vec<_> = self.poly_remesh_diverged.drain().collect();
        for nid in pending {
            self.poly_force_display_resync(nid);
        }
    }

    /// Cancel the active episode without pushing to undo stack.
    /// Used by gizmo cancel (Escape / right-click during drag).
    pub(crate) fn cancel_episode(&mut self) {
        self.pending_episode = None;
        self.cache.body_vol_cache.suppress_bake_dispatch = false;
        self.cache.body_vol_cache.draft_resolution = None; // restore full res
        // Drop V1 incremental Remesh state. Cancel restores the pre-drag mesh, but the
        // DISPLAY cache still holds latched-lattice geometry keyed by a signature the
        // restore may reproduce exactly (cancel = back to the starting mesh), so the
        // early-outs would keep showing it — reconcile explicitly rather than assuming the
        // next eval re-runs.
        self.poly_remesh_incr = None;
        self.poly_remesh_reconcile_diverged();
    }

    /// Returns true if an episode is currently active (begin called, no commit/cancel yet).
    pub(crate) fn has_active_episode(&self) -> bool {
        self.pending_episode.is_some()
    }

    /// Auto-commit any active episode if pointer is released.
    /// Call once per frame, after all UI drawing.
    pub(crate) fn auto_commit_episodes(&mut self, ctx: &eframe::egui::Context) {
        // Hold episode open for linear-array modal (live free grab until click / Esc).
        if self
            .tool
            .current
            .as_transform()
            .map_or(false, |t| t.shift_duplicate)
        {
            return;
        }
        if self.tool.array_dup.is_some() {
            return;
        }
        // The `W` edge-weight modal deliberately OUTLIVES the pointer press: an LMB release latches
        // the strength so the value can be inspected (and re-dragged) before RMB/Enter accepts. Its
        // episode must therefore stay open across releases — auto-committing here would push the
        // entry mid-gesture and leave `cancel_episode()` with nothing to drop, i.e. a junk undo
        // entry after an Esc. Same hold the shift-duplicate / array modals take above.
        if self.tool.current.is_poly_edge_weight() {
            return;
        }
        // The AZRA_EDIT harness drives a simulated drag with no pointer held, so without this the
        // episode it opens is committed the same frame — and every `pending_episode` gate under
        // test (hover pick, select-buffer key, outliner manifold) silently measures the UNGATED
        // path. Diagnostic-only: the env var is absent in any real session.
        if std::env::var_os("AZRA_EDIT").is_some() {
            return;
        }
        if self.has_active_episode() {
            let pointer_down = ctx.input(|i| i.pointer.any_down());
            if !pointer_down {
                self.commit_episode();
            }
        }
    }

    /// Push an undo snapshot without a closure — for cases where the mutation
    /// already happened (e.g., egui slider mutated field directly, graph not yet updated).
    /// The snapshot captures the CURRENT graph state (pre-mirror_prim_to_graph).
    pub(crate) fn transaction_snapshot_only(&mut self, desc: &str) {
        self.commit_episode(); // auto-commit any dangling episode
        let entry = self.snapshot_undo(desc);
        self.history.push(entry);
        self.project.scene_modified = true;
    }

    /// Pop the last undo entry and apply it, restoring the scene graph + flat cache.
    /// Does NOT push to the redo stack (used for cancel of shift-duplicate transforms).
    pub(crate) fn restore_undo_no_redo(&mut self) {
        if let Some(entry) = self.history.undo_stack.pop_back() {
            if let UndoPayload::Full { scene_graph_snapshot: Some(sg), .. } = entry.payload { // lights restored via graph
                self.scene.scene_graph = sg;
                self.sync_flat_cache();
            }
            self.scene.active_body_nid = entry.active_body_nid;
            self.sel.usel = entry.usel;
            self.history.mark_prims_dirty();
            if self.cache.use_body_vol_cache {
                self.cache.body_vol_cache.invalidate_for_undo(&self.scene.scene_graph);
            }
            self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::MESH);
        }
    }
}
