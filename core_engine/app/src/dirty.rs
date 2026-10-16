//! Dirty flag management and rebake pipeline — core frame-boundary logic.

use crate::*;

/// When true, prim positions are stored in body-local space.
/// Body translate/rotate/scale only updates the instance matrix — no volume rebake needed.
/// `trigger_body_vol_bake` is already gated by `dirty_bodies.is_empty()` inside
/// `BodyVolumeCache::trigger_bake()`, so body-only transforms never cause spurious rebakes.
use crate::BODY_LOCAL_PRIMS;

impl FourthLayerApp {
    // ---- Dirty Flags ----
    /// Set a dirty flag. Also sets legacy `is_dirty` for backward compatibility.
    /// Persisted-data flags (SCENE, LIGHT, MESH) also mark the document as modified
    /// so autosave and the unsaved-changes warning work even without an undo entry.
    ///
    /// B3d: `#[track_caller]` records the first call site of the frame into
    /// `perf.first_dirty_caller` (`file:line`) for `[IDLE_SPIN]` diagnostics.
    #[track_caller]
    pub(crate) fn mark_dirty(&mut self, flag: u32) {
        if self.perf.first_dirty_caller.is_none() {
            let loc = std::panic::Location::caller();
            // Keep only the crate-relative tail for readable logs.
            let file = loc.file();
            let short = file
                .rsplit(['/', '\\'])
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("/");
            self.perf.first_dirty_caller = Some(format!("{short}:{}", loc.line()));
        }
        self.dirty.set(flag);
        self.is_dirty = true;
        self.perf.dirty_flags_this_frame |= flag;
        // Document is modified whenever persisted scene data changes
        if flag & (DirtyFlags::SCENE | DirtyFlags::LIGHT | DirtyFlags::MESH) != 0 {
            self.project.scene_modified = true;
        }
        // Prim buffer needs re-upload when scene data changes
        if flag & DirtyFlags::SCENE != 0 {
            self.cache.prim_buf_stale = true;
            self.cache.prim_buf_dirty_body = None; // structural change → full serialize
        }
        // B.1: structural draw stream — rebuild when bodies/meshes appear/disappear/change.
        if flag & (DirtyFlags::SCENE | DirtyFlags::MESH) != 0 {
            self.draw_stream_stale = true;
            // Structural draw list (transforms/instances): same dirty surface as the NodeId stream.
            // Camera-only frames must NOT rebuild — only the per-frame cull mask runs.
            self.structural_draw_stale = true;
            self.poly_meshes_drawable_ok = false;
            self.raster_upload_plane_key = 0; // force instance re-upload after mesh change
        }
        // Light buffer only needs re-serialize when lights actually change.
        // SDF prim edits (SCENE) never modify light data — only undo/load
        // (which set LIGHT explicitly) need this.
        if flag & DirtyFlags::LIGHT != 0 {
            self.cache.light_gpu_data_stale = true;
        }
    }

    /// A scene-object TRANSFORM changed (gizmo drag frame, typed transform field, reset).
    /// The baked/poly meshes stay valid — zero rebake — but the raster instance matrices
    /// live in the CACHED structural draw list, and the GPU instance buffer is key-gated
    /// (`upload ⇔ structural rebuild ∨ camera plane-key change`, raster_gbuffer.rs). A
    /// transform-only frame with a still camera would therefore redraw the OLD matrix and
    /// the solid only snaps into place on the next unrelated rebuild ("surface moves only
    /// after I click", owner regression 2026-08-03 — root-level move, SDF and poly alike;
    /// the pre-D1 sampled content-hash used to catch this and was deliberately removed in
    /// e724f19). `mark_dirty(RENDER)` alone is NOT sufficient for a transform write —
    /// route EVERY object-transform writer through this instead.
    #[track_caller]
    pub(crate) fn mark_transform_dirty(&mut self) {
        self.draw_stream_stale = true;
        self.structural_draw_stale = true;
        self.raster_upload_plane_key = 0; // force instance re-upload this frame
        // A move/rotate/scale is persisted scene data; RENDER alone would skip the
        // autosave / unsaved-changes flag.
        self.project.scene_modified = true;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Outliner eye / solo / show-all visibility changed.
    ///
    /// `node.visible` is filtered only during a **full** structural gather
    /// (`raster_gbuffer` rebuild path). Camera-only frames reuse
    /// `structural_draw_calls` with cull=0 and **never re-check** visibility — so
    /// `mark_dirty(RENDER)` alone leaves hidden bodies still drawn (owner 2026-08-05:
    /// "eye does not hide surface in viewport"; same class as transform-dirty).
    /// Visibility is persisted scene data → also mark the document modified.
    #[track_caller]
    pub(crate) fn mark_visibility_dirty(&mut self) {
        self.structural_draw_stale = true;
        self.raster_upload_plane_key = 0; // force instance re-upload this frame
        self.project.scene_modified = true;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Process accumulated dirty flags at frame end. Called once per frame.
    /// Flags are taken (read + cleared) up front so that any new flags set
    /// during processing (e.g., RENDER from trigger_body_vol_bake) survive
    /// and are visible on the next frame.
    pub(crate) fn process_dirty(&mut self) {
        profile_scope!("process_dirty");
        let flags = self.dirty.take();
        if flags == 0 { return; }

        if flags & DirtyFlags::LIGHT != 0 {
            self.sync_light_to_gpu();
        }

        if flags & DirtyFlags::SCENE != 0 {
            if self.tool.work_mode == WorkMode::Sculpt {
                self.cache.cpu_volume.bake_from_primitives(&self.scene.primitives);
                self.sculpt.current_stroke.clear();
                // Force-clearing `stroke_active` swallows the surface release branch
                // (handle_sculpt_input is gated on it) — end the surface stroke through the ONE
                // lifecycle exit FIRST, or its first-touch snapshots leak into the next stroke
                // (the "vertices reverse to an older stroke" class; the stroke-start heal is the
                // backstop, this keeps undo one-entry-per-stroke).
                self.sculpt.surface_end_stroke();
                self.sculpt.stroke_active = false;
            }
            // G2: reclaim GPU-resident SDF mesh slots whose body no longer exists (delete / reparent-out /
            // project load replacing the scene). The draw only references a slot by NodeId, so an orphan
            // is a pure leak — but SCENE is exactly when bodies disappear, so prune here (structural, not
            // per-frame). Disjoint borrows: scene_graph read, gpu.sdf_body_mesh mut. Runs only when slots
            // exist (resident path was used at all).
            if self.gpu.as_ref().map_or(false, |g| !g.sdf_body_mesh.is_empty() || !g.sdf_brick_atlases.is_empty()) {
                let sg = &self.scene.scene_graph;
                if let Some(g) = self.gpu.as_mut() {
                    let live_top_level_body = |nid: &core_math::scene_graph::NodeId| {
                        sg.get(*nid).is_some_and(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                            && sg.parent_body(*nid).is_none()
                    };
                    // Keep the slot only for an EXISTING TOP-LEVEL body. A nested body (parent is
                    // another SdfBody) is subsumed into its ancestor's subtree bake and never draws
                    // its standalone mesh — its resident slot is a pure leak after a nest, so reclaim it.
                    g.sdf_body_mesh.retain(|nid, _| live_top_level_body(nid));
                    // Stage B-1: free the deleted bodies' shared-pool regions too (indirect entry
                    // zeroed → the multi-draw self-skips; slot index recycled; the region hole is
                    // reclaimed by the next pool rebuild). Same liveness rule as the slots above.
                    if let Some(pool) = g.sdf_mesh_pool.as_mut() {
                        let queue = g.queue.clone();
                        pool.retain_bodies(&queue, |nid| live_top_level_body(nid));
                    }
                    // P2-B5: same rule for the per-body sparse brick atlases (+ their LRU stamps) —
                    // an orphan would otherwise linger until the LRU budget happens to evict it.
                    g.sdf_brick_atlases.retain(|nid, _| live_top_level_body(nid));
                    let atlases = &g.sdf_brick_atlases;
                    g.sdf_atlas_lru.retain(|nid, _| atlases.contains_key(nid));
                }
            }
        }

        // Volume bake: dispatch when SCENE flag set OR when dirty_bodies have
        // pending work from mark_prim_dirty_at() (which doesn't set SCENE).
        // This decouples the bake trigger from the prim-buffer-upload flag.
        //
        // Drag-skip: during interactive SDF drags (BlendKDrag, Create, Bevel,
        // BoolEdgeBevel, FalloffAdjust), rebake_no_undo() fires SCENE every
        // frame, but the bake readback is deferred (poll_primary_bake skips
        // during is_busy()), causing throwaway GPU dispatches. Skip the
        // SCENE-triggered bake during these drags — the has_pending_dirty()
        // path still fires for partial bakes, and a final bake dispatches on
        // drag end when rebake_no_undo() fires one last time.
        // First-bake exception: allow through if any dirty body has never been
        // baked (no cached grid), so newly created prims get instant feedback.
        let sdf_drag_active = self.tool.current.is_blend_k_drag()
            || self.tool.current.is_create()
            || self.tool.current.is_bevel()
            || self.tool.current.is_bool_edge_bevel()
            || self.tool.current.is_falloff_adjust();
        let drag_skip_scene_bake = sdf_drag_active
            && !self.cache.body_vol_cache.has_unbaked_dirty_bodies();
        let vol_bake_needed = (flags & DirtyFlags::SCENE != 0 && !drag_skip_scene_bake)
            || (self.cache.use_body_vol_cache && self.cache.body_vol_cache.has_pending_dirty());
        if vol_bake_needed {
            let svo_allowed = self.tool.work_mode == WorkMode::Object
                || (self.tool.work_mode == WorkMode::Vertex && self.modeling_context == ModelingContext::Sdf);
            let body_transform_only = BODY_LOCAL_PRIMS
                && self.cache.use_body_vol_cache
                && !self.cache.body_vol_cache.has_pending_dirty();
            if self.cache.use_body_vol_cache && svo_allowed && !body_transform_only {
                self.trigger_body_vol_bake();
            }
        }

        if flags & DirtyFlags::MESH != 0 {
            self.poly_sync_all_meshes();
        }

    }

    // ---- Rebake ----
    pub(crate) fn rebake_no_undo(&mut self) {
        profile_scope!("rebake_no_undo");
        // Hash-guard: skip if prim data + graph structure unchanged since last rebake.
        // Prevents infinite SCENE dirty loop when a spurious caller fires every frame.
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            // REORDER3: content hash (uid + fields, allocation-free) — NOT a raw byte
            // reinterpret of Vec<SdfPrimitive>. The old byte hash read padding (UB) and
            // hashed heap pointers ({ptr,len,cap}) instead of content, so an in-place
            // realloc edit could leave the bytes identical and spuriously skip a needed
            // rebake. This is the SAME content key the GPU bake path gates on.
            crate::bake_utils::hash_prim_slice(&self.scene.primitives).hash(&mut h);
            // Include graph node count to catch structural changes (reorder, reparent)
            // that don't modify prim data.
            self.scene.scene_graph.all_nodes().count().hash(&mut h);
            h.finish()
        };
        if hash == self.cache.rebake_hash && self.cache.rebake_hash != 0 {
            return;
        }
        self.cache.rebake_hash = hash;
        self.mark_dirty(DirtyFlags::SCENE);
    }
    /// Kick off per-body volume cache bake. Only dirty bodies are rebaked;
    /// clean bodies reuse their cached grids.
    ///
    /// Callers that need ALL bodies rebaked (toggle-on, resolution change,
    /// mode switch) must call `body_vol_cache.mark_all_bodies_dirty()` first.
    /// Normal per-prim edits mark only the affected body via
    /// `mirror_prim_to_graph` / `write_prim_transform` / `mark_node_body_vol_dirty`.
    pub(crate) fn trigger_body_vol_bake(&mut self) {
        // Object Mode (raster DC triangles) is the only render regime — GridBacked always
        // (the SDF-Edit volume-raymarch TextureOnly regime went away with the legacy VP path).
        // Without this clamp, a >cap pick OR adaptive scaling of a large body bakes TextureOnly
        // (empty CPU grid) → no triangles → the body goes invisible.
        self.cache.body_vol_cache.gridbacked_only = true;
        // S2.6: no sparse toggle to sync — `sparse_bake_eligible` keys on `gridbacked_only`
        // (set just above) + res ≥ 512, so the SparseBricks regime is structural.
        let res = self.cache.cpu_volume.resolution[0];
        let bounds = self.cache.cpu_volume.grid_bounds;
        self.cache.body_vol_cache.set_params(res, bounds, &self.scene.scene_graph);

        // Update per-body LOD resolutions from camera distance
        let cam = &self.viewport.camera;
        let cam_pos = cam.target + cam.orbit * glam::Vec3::new(0.0, 0.0, cam.distance);
        let fov_scale = (self.render.fov_deg.to_radians() * 0.5).tan().max(0.01);
        let viewport_h = self.viewport.viewport_rect.height().max(1.0);
        self.cache.body_vol_cache.update_lod_resolutions(
            &self.scene.scene_graph, cam_pos, fov_scale, viewport_h);

        // Only bake bodies already in dirty_bodies — no blanket mark-all.
        // Hash check inside trigger_bake() still skips unchanged bodies.
        // Returns bodies that received a synchronous coarse bake (L4 cascade).
        let sync_baked = self.cache.body_vol_cache.trigger_bake(&self.scene.scene_graph, self.job_dispatcher.as_mut());
        // Dispatch DC extraction for synchronously baked bodies immediately.
        // These bodies have fresh (coarse) grid data — no need to wait for poll().
        for body_nid in sync_baked {
            self.mark_dirty(DirtyFlags::RENDER);
            self.dispatch_grid_dc_async(body_nid);
        }
    }

    /// Mark a node's owning SDF body as volume-dirty so the voxel cache is rebaked.
    /// If `nid` is itself an SdfBody, that body is marked dirty.
    /// Otherwise, walks up to the nearest SdfBody ancestor.
    /// Respects `suppress_body_vol_dirty` (used during batch operations).
    pub(crate) fn mark_node_body_vol_dirty(&mut self, nid: NodeId) {
        if self.cache.suppress_body_vol_dirty { return; }
        if !self.cache.use_body_vol_cache { return; }
        // Resolve the BODY that owns this node: the node itself if it is an SdfBody,
        // otherwise the nearest ancestor SdfBody.
        let body_nid = if matches!(self.scene.scene_graph.get(nid).map(|n| &n.payload), Some(NodePayload::SdfBody { .. })) {
            Some(nid)
        } else {
            self.scene.scene_graph.parent_body(nid)
        };
        if let Some(mut body_nid) = body_nid {
            // Only TOP-LEVEL bodies bake (a nested body is subsumed into its ancestor's subtree bake —
            // see bake_dispatch's parent_body skip). Climb to the top-level ancestor so dirtying a
            // nested body re-bakes the ancestor that actually owns the rendered mesh.
            while let Some(parent) = self.scene.scene_graph.parent_body(body_nid) {
                body_nid = parent;
            }
            self.cache.body_vol_cache.mark_body_dirty(body_nid);
        }
    }

    pub(crate) fn rebake_for_sculpt(&mut self) { self.cache.cpu_volume.bake_from_primitives(&self.scene.primitives); self.project.scene_modified = true; self.mark_dirty(DirtyFlags::RENDER); }
    pub(crate) fn rebake(&mut self, desc:&str) { self.transaction(desc, |s| s.rebake_no_undo()); }

    /// Rebuild compute shader pipelines after node graph WGSL changes.
    /// Uses split shader modules — only rebuilds raymarch + deferred eagerly;
    /// fog + pathtrace are invalidated for lazy re-compilation on next use.
    ///
    /// User node graphs can produce invalid WGSL. In wgpu, `create_shader_module`
    /// panics on Naga validation errors — there is no Result variant. We recover
    /// via `catch_recoverable` (keeps the previous pipelines). That path is a
    /// caught panic: the crash-safety hook must not exit the process.
    pub(crate) fn rebuild_shader_pipelines(&mut self) {
        // Collect all unique node graphs and build uber-function with prim_id dispatch
        let graphs: Vec<(usize, &shader_nodes::NodeGraph)> = self.scene.primitives.iter().enumerate()
            .filter(|(_, p)| p.node_graph.is_some())
            .filter_map(|(i, p)| self.ui.node_graphs.get(&p.prim_uid).map(|g| (i, g)))
            .collect();
        // Material-owned graphs bake to Hybrid maps only — they never enter this uber list.
        // An empty list used to still recompile DF-full on the UI thread (AppHang when Load Preset
        // set shader_dirty for Materials). Skip the no-op rebuild.
        if graphs.is_empty() {
            self.shader_dirty = false;
            return;
        }
        let uber_wgsl = shader_nodes::NodeGraph::build_uber_material_fn(&graphs);
        if let Some(gpu) = &mut self.gpu {
            let device = &gpu.device;
            let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
            let bgl = &gpu.bind_groups.bind_group_layout;
            let df_bgl = &gpu.bind_groups.deferred_bgl;

            // Build all shader modules + pipelines into locals.
            // If any step panics (Naga validation error from bad node graph WGSL),
            // catch_unwind recovers and we keep the previous working pipelines.
            let build_result = crate::panic_safety::catch_recoverable(std::panic::AssertUnwindSafe(|| {
                // (Full analytical raymarch rebuild REMOVED 2026-07-14 — dead march display path.)
                let df_wgsl = graphics::build_deferred_wgsl(Some(&uber_wgsl));
                let df_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor{
                    label: Some("Deferred (rebuilt)"),
                    source: wgpu::ShaderSource::Wgsl(df_wgsl.into()),
                });
                let rm_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("raymarch"),bind_group_layouts:&[bgl],push_constant_ranges:&[]});
                let lite_wgsl = graphics::build_raymarch_lite_wgsl();
                let lite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("RaymarchLite (rebuilt)"),source:wgpu::ShaderSource::Wgsl(lite_wgsl.into())});
                let pipeline_lite = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("raymarch_lite"),layout:Some(&rm_pl),module:&lite_shader,entry_point:Some("raymarch_gbuf"),compilation_options:Default::default(),cache:cache_ref});
                let df_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("deferred"),bind_group_layouts:&[df_bgl],push_constant_ranges:&[]});
                let df_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("deferred"),layout:Some(&df_pl),module:&df_shader,entry_point:Some("deferred_shade"),compilation_options:Default::default(),cache:cache_ref});
                (pipeline_lite, df_pipeline)
            }));

            match build_result {
                Ok((pl, dp)) => {
                    gpu.pipelines.pipeline_lite = Some(pl);
                    gpu.pipelines.deferred_pipeline = Some(dp);
                    gpu.pipelines.df_full_compile_started = true;
                    gpu.pipelines.df_full_compile_rx = None;
                    // Invalidate fog + pathtrace — lazily recompiled on next use
                    gpu.pipelines.fog_pipeline = None;
                    gpu.pipelines.fog_lite_pipeline = None;
                    gpu.pipelines.pt_tri_pipeline = None;
                    gpu.pipelines.sdf_compile_rx = None;
                    gpu.pipelines.wgsl_pipelines_ready = true;
                    gpu.pipelines.shader_compile_error = None;
                    self.shader_dirty = false;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                Err(e) => {
                    let msg = if let Some(s) = e.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = e.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "Unknown shader compilation error".into()
                    };
                    eprintln!("[SHADER] Pipeline rebuild failed (keeping previous pipelines): {}", msg);
                    gpu.pipelines.shader_compile_error = Some(msg);
                    // Don't retry every frame — mark shader_dirty as handled
                    self.shader_dirty = false;
                }
            }
        }
    }
}
