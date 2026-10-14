//! Voxel sculpt UI/lifecycle state: create, enter/leave edit, remesh popup, bulk actions.
use super::*;

impl AzraApp {
    /// Create a voxel SPHERE and enter Sculpt (Ctrl+Z removes the object via scene undo).
    pub(crate) fn new_voxel_sphere(&mut self) { self.voxel_create(VoxelCreateShape::Sphere); }

    /// Create a voxel BOX and enter Sculpt.
    pub(crate) fn new_voxel_box(&mut self) { self.voxel_create(VoxelCreateShape::Box); }

    /// Create a voxel CYLINDER (Y-up) and enter Sculpt.
    pub(crate) fn new_voxel_cylinder(&mut self) { self.voxel_create(VoxelCreateShape::Cylinder); }

    /// Clear the voxel object back to empty material.
    pub(crate) fn clear_voxels(&mut self) {
        if self.sculpt.voxel.is_some() {
            // Wholesale grid change → fully resync the incremental cache, then remesh.
            // CANCEL any in-flight Move bake first (review P0): the user is deleting everything, so
            // the un-baked move is discarded with it — landing it later would resurrect a floating
            // patch of the deleted surface into the cleared grid + push undo onto the reset stacks.
            self.voxel_cancel_move_bake();
            // `grid.clear()` sets gc_pending → the brick handles the atlas-undo snapshots reference are
            // invalidated, so a later undo would corrupt the cleared grid. Reset BOTH undo systems first.
            self.voxel_undo_reset();
            if let Some(v) = self.sculpt.voxel.as_mut() { v.grid.clear(); }
            self.voxel_build_all_chunks(); // wholesale change → rebuild every chunk
            self.toast("Voxels cleared");
        }
    }

    /// Create a NEW voxel sculpt object (sphere / box / cylinder): density grid → Surface Nets
    /// display mesh → `VoxelBody` scene node. Wrapped in `transaction()` so **Ctrl+Z** undoes
    /// the create (scene-graph restore). Enters Sculpt mode with the new body active.
    pub(crate) fn voxel_create(&mut self, shape: VoxelCreateShape) {
        // 256³ sparse default: ~128 voxels across a unit sphere → smooth Surface Nets when zoomed.
        // 128³ only had ~64 voxels across → clear "voxel stair" facets (owner: base sphere not smooth).
        self.voxel_create_with_res(shape, 256);
    }

    /// `voxel_create` with an explicit grid resolution — the SAME production path; the res
    /// parameter exists so the headless fuzz harness can build smaller (64–128³) objects fast.
    pub(crate) fn voxel_create_with_res(&mut self, shape: VoxelCreateShape, res: u32) {
        use core_math::voxel_sculpt::VoxelGrid;
        let label = match shape {
            VoxelCreateShape::Sphere => "Voxel Sphere",
            VoxelCreateShape::Box => "Voxel Box",
            VoxelCreateShape::Cylinder => "Voxel Cylinder",
        };
        // Snapshot graph BEFORE create so Ctrl+Z removes the new body (and reactivates any prior).
        self.transaction(&format!("Create {label}"), |s| {
            // Bounds 2.0 keeps grow margin for Move; remesh to 512/1024 densifies further (same world size).
            let mut grid = VoxelGrid::new_empty(res, 2.0);
            match shape {
                VoxelCreateShape::Sphere => grid.fill_sphere(Vec3::ZERO, 1.0),
                VoxelCreateShape::Box => grid.fill_box(Vec3::ZERO, Vec3::splat(0.9)),
                // Y-up cylinder: radius 0.85, height ~1.6 (matches sphere diameter feel).
                VoxelCreateShape::Cylinder => grid.fill_cylinder(Vec3::ZERO, 0.85, 0.80),
            }
            // Initial display mesh for the node's single-mesh slot (save/load + picking + Object-Mode).
            let (verts, idx) = grid.object_display_mesh();
            let tris = (idx.len() / 3) as u32;

            // Multi-voxel: persist CURRENT sculpt to its node, free live chunks, then activate the new body.
            if s.sculpt.voxel.is_some() {
                s.voxel_sync_node_mesh();
                if let Some(old) = s.sculpt.voxel.take() {
                    for cid in old.chunks { s.cache.mesh_cache.remove(cid); }
                }
            }

            let mesh_id = s.cache.mesh_cache.alloc_id();
            s.cache.mesh_cache.insert_imported(mesh_id, (verts, idx), tris);
            let root = s.scene.scene_graph.root();
            let (spawn_pos, spawn_rot) = if s.viewport.wp_mode == WorkPlaneMode::Locked {
                let wp = &s.viewport.wp_resolved;
                (wp.origin, Self::euler_from_workplane_normal(wp.normal))
            } else {
                (Vec3::ZERO, Vec3::ZERO)
            };
            // Warm terracotta CLAY (signature sculpt look).
            let mut clay = core_math::primitives::PbrMaterial::default();
            clay.albedo = Vec3::new(0.82, 0.45, 0.38);
            clay.roughness = 0.70;
            clay.metallic = 0.0;
            clay.name = "Voxel Clay".into();
            clay.cavity_dirt.enabled = true;
            clay.cavity_dirt.color = Vec3::new(0.30, 0.14, 0.11);
            clay.cavity_dirt.amount = 0.55;
            clay.cavity_dirt.roughness = 0.85;
            let name = match shape {
                VoxelCreateShape::Sphere => "Voxel Sphere",
                VoxelCreateShape::Box => "Voxel Box",
                VoxelCreateShape::Cylinder => "Voxel Cylinder",
            };
            let node = s.scene.scene_graph.add_child_with_transform(
                root,
                core_math::scene_graph::NodePayload::VoxelBody {
                    name: name.into(),
                    grid: grid.clone(),
                    mesh_cache_id: Some(mesh_id),
                    material: clay,
                },
                spawn_pos,
                spawn_rot,
                Vec3::ONE,
            );
            s.voxel_set_active(node, grid, tris);
            // Always-on alpha (3D-Coat style): Soft Round tip by default — samples on GPU, free.
            if let Some(v) = s.sculpt.voxel.as_mut() {
                if v.brush_tip.is_none() {
                    if let Some((tip, path)) = crate::brush_tip_lib::load_library_tip("Soft Round") {
                        v.brush_tip = Some(tip);
                        v.brush_tip_name = "Soft Round".into();
                        v.brush_tip_path = Some(path.to_string_lossy().to_string());
                    } else {
                        // Library not ready yet — procedural Soft Round in-memory (still GPU).
                        v.brush_tip = Some(core_math::voxel_sculpt::BrushTipAlpha::soft_round(64));
                        v.brush_tip_name = "Soft Round".into();
                        v.brush_tip_path = None;
                    }
                }
            }
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
        });
        let res = self.sculpt.voxel.as_ref().map(|v| v.grid.res).unwrap_or(256);
        self.toast(&format!(
            "{label} created at {res}³ — remesh (Ctrl+R) to 512/1024 for finer clay (Ctrl+Z undoes create)"
        ));
    }

    /// After a scene-graph undo/redo: drop or re-bind the live sculpt working set so it matches
    /// the restored `VoxelBody` nodes (create / delete / duplicate).
    pub(crate) fn voxel_resync_after_graph_change(&mut self) {
        use core_math::scene_graph::NodePayload;
        let active_nid = self.sculpt.voxel.as_ref().map(|v| v.node);
        let active_still_voxel = active_nid.and_then(|nid| self.scene.scene_graph.get(nid)).map_or(false, |n| {
            matches!(n.payload, NodePayload::VoxelBody { .. })
        });
        if !active_still_voxel {
            // Live sculpt pointed at a removed/changed node — tear down without writing back.
            if let Some(old) = self.sculpt.voxel.take() {
                for cid in old.chunks {
                    self.cache.mesh_cache.remove(cid);
                }
            }
            self.voxel_undo_reset();
            self.sculpt.voxel_stroke.clear();
            self.sculpt.stroke_active = false;
            // Prefer selection, else any remaining VoxelBody, as the new active sculpt.
            let mut reenter: Option<(core_math::scene_graph::NodeId, core_math::voxel_sculpt::VoxelGrid)> = None;
            if let Some(nid) = self.sel.usel.active_node() {
                if let Some(n) = self.scene.scene_graph.get(nid) {
                    if let NodePayload::VoxelBody { grid, .. } = &n.payload {
                        reenter = Some((nid, grid.clone()));
                    }
                }
            }
            if reenter.is_none() {
                for (nid, n) in self.scene.scene_graph.all_nodes() {
                    if let NodePayload::VoxelBody { grid, .. } = &n.payload {
                        reenter = Some((nid, grid.clone()));
                        break;
                    }
                }
            }
            if let Some((nid, grid)) = reenter {
                self.voxel_set_active(nid, grid, 0);
            }
        }
        // Object-mode display meshes for any VoxelBody whose cache id was orphaned by undo/redo.
        self.regen_voxel_display_meshes();
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
    }

    /// Activate `node` (a VoxelBody) as the live sculpt working set: build the transient VoxelObject from
    /// `grid`, set the sculpt render state, reset undo, (re)build the live chunk meshes, and select it.
    /// SHARED by voxel_create (new object) and voxel_enter_edit (switch to an existing one). Brush / radius
    /// / strength / lazy are TOOL state — they carry across switches. `mesh_id` is read from the node.
    pub(super) fn voxel_set_active(&mut self, node: core_math::scene_graph::NodeId, grid: core_math::voxel_sculpt::VoxelGrid, tris: u32) {
        use core_math::voxel_sculpt::VoxelBrush;
        use core_math::scene_graph::NodePayload;
        let mesh_id = match self.scene.scene_graph.get(node).and_then(|n|
            if let NodePayload::VoxelBody { mesh_cache_id, .. } = &n.payload { *mesh_cache_id } else { None }) {
            Some(m) => m,
            None => return, // not a VoxelBody / no display mesh — nothing to activate
        };
        // Brush / radius / strength / lazy / symmetry / pressure are TOOL state — they carry across body
        // switches so the user's brush setup survives switching the active voxel object.
        let (brush, radius, strength, lazy, stroke_spacing, symmetry, pressure, falloff, brush_dir, paint_color_on, paint_color) = self.sculpt.voxel.as_ref()
            .map(|v| (v.brush, v.radius, v.strength, v.lazy, v.stroke_spacing, v.symmetry,
                      (v.pressure_enabled, v.pressure_to_strength, v.pressure_to_radius), v.falloff, v.brush_dir,
                      v.paint_color_on, v.paint_color))
            // P0 feel-perf defaults (fix/sculpt-feel-perf): smaller radius + lighter strength
            // (old 0.4/0.5 ballooned the stamp box and stripped stamps).
            // stroke_spacing 0.10 matches Blender ~10% radius dab spacing.
            // lazy 0.0 = LazyMouse OFF by default (Gate 0, feel audit 2026-07-23): the old 0.10
            // default was a 7px rope that made the brush trail the cursor. Stabilization is
            // opt-in via the slider, matching Blender (stabilize stroke off by default).
            .unwrap_or((VoxelBrush::Draw, 0.20, 0.35, 0.0, 0.10, [false; 3], (false, true, false),
                        core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS,
                        core_math::voxel_sculpt::BrushDirection::Surface,
                        false, glam::Vec3::splat(0.5)));
        let (pressure_enabled, pressure_to_strength, pressure_to_radius) = pressure;
        let pressure_radius_amount = self.sculpt.voxel.as_ref()
            .map(|v| v.pressure_radius_amount)
            .unwrap_or(0.60);
        // Per-brush settings toggle + saved map are TOOL state — carry them across body switches too.
        // Per-brush ON by default: every base op keeps its own factory + user tweaks (easy tweak).
        let (per_brush_settings, brush_settings) = self.sculpt.voxel.as_ref()
            .map(|v| (v.per_brush_settings, v.brush_settings.clone()))
            .unwrap_or((true, std::collections::HashMap::new()));
        let (falloff_preset, mask_paint) = self.sculpt.voxel.as_ref()
            .map(|v| (v.falloff_preset, v.mask_paint))
            .unwrap_or((core_math::voxel_sculpt::FalloffPreset::Smooth, false));
        // Pinch strength + tip image are TOOL state — carry across body switches.
        let (pinch_intensity, brush_tip, brush_tip_name, brush_tip_path) = self.sculpt.voxel.as_ref()
            .map(|v| (v.pinch_intensity, v.brush_tip.clone(), v.brush_tip_name.clone(), v.brush_tip_path.clone()))
            .unwrap_or((1.0, None, String::new(), None));
        let (tip_follow_stroke, tip_contrast, front_faces_only) = self.sculpt.voxel.as_ref()
            .map(|v| (v.tip_follow_stroke, v.tip_contrast, v.front_faces_only))
            .unwrap_or((false, 1.0, false));
        let (move_elasticity, move_pull_strength, move_connected_only) = self.sculpt.voxel.as_ref()
            .map(|v| (v.move_elasticity, v.move_pull_strength, v.move_connected_only))
            .unwrap_or((1.0, 1.0, false));
        let (auto_smooth, auto_smooth_intensity, surface_extrude) = self.sculpt.voxel.as_ref()
            .map(|v| (v.auto_smooth, v.auto_smooth_intensity, v.surface_extrude))
            .unwrap_or((false, 0.35, false));
        let (layer_height, layer_gain) = self.sculpt.voxel.as_ref()
            .map(|v| (v.layer_height, v.layer_gain))
            .unwrap_or((0.14, 0.22));
        let (plane_offset, plane_bias, build_rate, height_limit) = self.sculpt.voxel.as_ref()
            .map(|v| (v.plane_offset, v.plane_bias, v.build_rate, v.height_limit))
            .unwrap_or((0.10, 0.0, 2.0, 1.0));
        let (flatten_amount, volume_preservation, accumulate) = self.sculpt.voxel.as_ref()
            .map(|v| (v.flatten_amount, v.volume_preservation, v.accumulate))
            .unwrap_or((0.5, 0.0, true));
        let (inflate_speed, inflate_direction, thickness_clamp) = self.sculpt.voxel.as_ref()
            .map(|v| (v.inflate_speed, v.inflate_direction, v.thickness_clamp))
            .unwrap_or((1.0, 0, 1.0));
        let (pinch_width, pinch_amount, crease_width, crease_depth) = self.sculpt.voxel.as_ref()
            .map(|v| (v.pinch_width, v.pinch_amount, v.crease_width, v.crease_depth))
            .unwrap_or((1.0, 1.0, 1.0, 1.0));
        let (plane_angle, cut_depth, fill_holes) = self.sculpt.voxel.as_ref()
            .map(|v| (v.plane_angle, v.cut_depth, v.fill_holes))
            .unwrap_or((0.0, 1.0, false));
        let (auto_mask, auto_mask_intensity, auto_mask_invert) = self.sculpt.voxel.as_ref()
            .map(|v| (v.auto_mask, v.auto_mask_intensity, v.auto_mask_invert))
            .unwrap_or((core_math::voxel_sculpt::AutoMaskMode::Off, 0.75, false));
        let (stack_gather, stack_flatten, stack_fill, stack_crease, stack_inflate) = self.sculpt.voxel.as_ref()
            .map(|v| (v.stack_gather, v.stack_flatten, v.stack_fill, v.stack_crease, v.stack_inflate))
            .unwrap_or((false, false, false, false, false));
        // Brush Dynamics — TOOL state (carry across body switches).
        let (dyn_jitter, dyn_scatter, dyn_random_rotation, dyn_random_strength, dyn_random_radius, dyn_stroke_noise) =
            self.sculpt.voxel.as_ref()
                .map(|v| (v.dyn_jitter, v.dyn_scatter, v.dyn_random_rotation, v.dyn_random_strength, v.dyn_random_radius, v.dyn_stroke_noise))
                .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0, 0.0));
        self.sculpt.voxel = Some(crate::VoxelObject {
            grid, node, mesh_id, brush, radius, strength, falloff, falloff_preset, mask_paint,
            sparse_move_warned: false,
            brush_dir, brush_add: true, dir_average_region: 1.0, dir_smooth_normal: 0.0,
            first_point_normal: None,
            first_point_pos: None,
            average_normal_sum: glam::Vec3::ZERO, average_normal_count: 0,
            stroke_prev_pos: None, stroke_last_surface: None,
            deferred_stamps: Vec::new(),
            dyn_stroke_seed: 0, dyn_dab_index: 0,
            move_prev_pos: None,
            elastic_grab: core_math::voxel_sculpt::ElasticGrab::default(),
            mesh_grab: crate::MeshGrab::default(), pending_move_grabs: Vec::new(),
            pending_move_group_sizes: Vec::new(), pending_move_redo: Vec::new(),
            // 3D-Coat Move default (user Options_Presets: VoxelizeImmediately=true): live drag is
            // SURFACE mesh; on release re-voxelize. Bake is ASYNC — never block the UI (Coat has a
            // short pause; our old block_land was multi-second because of grid GROW + whole-grid work).
            move_deferred: false, move_voxelize_due: None,
            surface_mode: false, surface_dirty_region: None,
            surface_moved_aabbs: std::collections::HashMap::new(),
            surface_pool_synced: std::collections::HashMap::new(),
            snake_anchor: None, snake_plane_n: glam::Vec3::Z,
            move_elasticity, move_pull_strength, move_connected_only,
            symmetry, tris,
            remesh_open: false, remesh_target_res: 64, lazy, lazy_pos: None, stroke_spacing,
            chunk_dim: 0, chunks: Vec::new(), chunk_tris: Vec::new(),
            surface_chunk_count: 0,
            dirty_chunks: std::collections::HashSet::new(), pending_rebuild: None, pending_move_bake: None,
            profile: crate::SculptProfile::default(), parity_msg: String::new(),
            gpu_mesh: true, gpu_resident_mesh: true, gpu_brush: true, gpu_brush_atlas: true,
            paint_color_on, paint_color, stroke_aabb: None, focus_chunk: None, last_surface_hit: None,
            resident_cap: 96, atlas_stroke: false, stroke_uses_atlas: None, cpu_grid_stale: false,
            pressure_enabled, pressure_to_strength, pressure_to_radius, pressure_radius_amount,
            per_brush_settings, brush_settings,
            pinch_intensity, brush_tip, brush_tip_name, brush_tip_path,
            tip_follow_stroke, tip_orient: if tip_follow_stroke { 1 } else { 0 }, tip_angle: 0.0,
            tip_contrast, front_faces_only,
            auto_smooth, auto_smooth_intensity, surface_extrude, sculpt_on_surface: true,
            layer_height, layer_gain,
            plane_offset, plane_bias, build_rate, height_limit,
            flatten_amount, volume_preservation, accumulate,
            inflate_speed, inflate_direction, thickness_clamp,
            pinch_width, pinch_amount, crease_width, crease_depth,
            plane_angle, cut_depth, fill_holes,
            dyn_jitter, dyn_scatter, dyn_random_rotation, dyn_random_strength, dyn_random_radius, dyn_stroke_noise,
            auto_mask, auto_mask_intensity, auto_mask_invert,
            stack_gather, stack_flatten, stack_fill, stack_crease, stack_inflate,
        });
        // Seed the active base op into the brush system (factory recipe) so knobs match the op.
        if let Some(v) = self.sculpt.voxel.as_mut() {
            if !v.brush_settings.contains_key(&v.brush) {
                let s = crate::VoxelObject::factory_settings_for(v.brush, v.capture_brush_settings());
                // Keep the feel-perf default radius if this is a brand-new body (already on v.radius).
                let r = v.radius;
                v.apply_brush_settings(s);
                v.radius = r;
                v.brush_settings.insert(v.brush, v.capture_brush_settings());
            }
        }
        self.select_single_unified(crate::SceneItemId::Node(node));
        // Raster mesh path active; clear the legacy SDF sculpt volume so its raymarch shows nothing.
        self.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
        self.cache.cpu_volume.clear_to_empty();
        self.tool.work_mode = WorkMode::Sculpt;
        self.voxel_undo_reset();
        self.sculpt.current_stroke.clear();
        self.sculpt.voxel_stroke.clear();
        self.sculpt.stroke_active = false;
        self.voxel_build_all_chunks();
        self.mark_dirty(DirtyFlags::SCENE);
    }

    /// Step 6b: switch the active sculpt to an EXISTING VoxelBody `nid` (select it + enter Sculpt). Flushes
    /// the current voxel to its node (so it persists as a separate object + keeps its display mesh), then
    /// loads `nid`'s grid as the working set. No-op if already editing `nid`.
    pub(crate) fn voxel_enter_edit(&mut self, nid: core_math::scene_graph::NodeId) {
        use core_math::scene_graph::NodePayload;
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid) { return; }
        self.voxel_leave_edit(); // flush + free the current voxel before loading the new one (shared)
        let grid = match self.scene.scene_graph.get(nid).and_then(|n|
            if let NodePayload::VoxelBody { grid, .. } = &n.payload { Some(grid.clone()) } else { None }) {
            Some(g) => g,
            None => return, // not a VoxelBody
        };
        self.voxel_set_active(nid, grid, 0); // tris fills in as chunks mesh
        self.toast("Editing voxel");
    }

    /// Leaving the Sculpt room for Object mode: flush the active voxel's live sculpt into its node
    /// (display mesh + grid), free its live per-chunk meshes, and clear the working set. The voxel then
    /// renders in Object mode via its STABLE display mesh (a plain mesh node, like ImportedMesh) instead
    /// of the live per-chunk meshes — fixing the "voxel disappears until I move the camera" render-gate
    /// miss (the chunk path needs a repaint pump the bare mode-switch never provided; the display mesh
    /// just needs one RENDER mark). Selection is preserved, so re-entering Sculpt reloads this voxel via
    /// `voxel_enter_edit`. No-op when not editing a voxel. Shared by `voxel_enter_edit` (switch voxels)
    /// and the Object-mode entry (`restore_sdf_room_render_state`) — one deactivation path, never two.
    pub(crate) fn voxel_leave_edit(&mut self) {
        if self.sculpt.voxel.is_none() { return; }
        // End any pending surface stroke against the still-live chunks BEFORE the object (and its
        // chunk meshes) are dropped — otherwise the snapshot would leak into the NEXT voxel edit
        // session's first stroke (the map now lives on SculptState and survives the take() below).
        self.sculpt.surface_end_stroke();
        self.voxel_sync_node_mesh();
        // Force GPU upload this frame so Object mode never shows empty until next poll.
        if let Some(gpu) = self.gpu.as_ref() {
            self.cache.mesh_cache.upload_pending(&gpu.device, &gpu.queue);
        }
        let mid = self.sculpt.voxel.as_ref().map(|v| v.mesh_id);
        let tris = mid.and_then(|id| self.cache.mesh_cache.get(id).map(|m| m.triangle_count)).unwrap_or(0);
        if let Some(old) = self.sculpt.voxel.take() {
            for cid in old.chunks { self.cache.mesh_cache.remove(cid); }
        }
        if tris == 0 {
            self.toast("Voxel display mesh empty on leave — remesh or re-enter Sculpt if needed");
        }
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
    }

    /// Bake the voxel's CURRENT full surface into its scene-node single mesh (`mesh_id`). The live
    /// surface renders via per-chunk meshes, but the node's single mesh is what gets SERIALIZED
    /// (save/load) and PICKED — and sculpting/resample only update the chunks, leaving `mesh_id`
    /// stale. Call before saving so the persisted/picked mesh matches what's on screen. O(surface);
    /// for one-shot use (save), never per-frame. No-op when there's no voxel.
    pub(crate) fn voxel_sync_node_mesh(&mut self) {
        // SURFACE MODE: pending deferred Move grabs are TRANSIENT (held in the shader, not yet in the grid).
        // Flush them FIRST so the synced/serialized mesh + grid include the moves — this is the save-path
        // bake (this is the single chokepoint for save / body-switch / leave-edit), so a save can never drop
        // an un-voxelized surface-mode move. No-op when nothing is pending. ASYNC: drain the WHOLE
        // stack in a flush+BLOCK-land loop — a single flush+land would miss grabs appended while a
        // bake was in flight (Move strokes are allowed then), and leave-edit's `voxel.take()` would
        // destroy them (review P1). Strokes can't append inside this loop (single-threaded UI).
        //
        // SURFACE SCULPT MODE: mesh edits live only in the chunk vertex arrays; the grid (which is
        // what gets serialized) is stale over the dirty region. Spawn the mesh→SDF revoxelization —
        // the BLOCK-land drain loop below lands it (`pending_move_bake` covers it), so the persisted
        // grid always matches the on-screen surface. The mode flag itself stays as the user set it.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some()) {
            self.voxel_surface_revoxelize();
        }
        let mut _drain_guard = 0u32;
        while self.sculpt.voxel.as_ref().map_or(false, |v|
            v.pending_move_bake.is_some() || !v.pending_move_grabs.is_empty()) {
            self.voxel_flush_pending_move();
            self.voxel_land_move_bake(true);
            _drain_guard += 1;
            if _drain_guard > 8 { break; } // backstop; every land drains ≥1 baked/abandoned grab
        }
        // Phase A.2/B: the node's single mesh (serialized on save + used for picking) is extracted from
        // the CPU grid, which the atlas path leaves stale. Materialise it from the GPU atlas first
        // (no-op if not stale) so the saved/picked mesh matches the on-screen surface.
        self.materialize_cpu_from_atlas();
        let data = self.sculpt.voxel.as_ref().map(|v| {
            (v.mesh_id, v.grid.object_display_mesh())
        });
        if let Some((mid, (verts, idx))) = data {
            let tris = (idx.len() / 3) as u32;
            self.cache.mesh_cache.update_editable_mesh(mid, (verts, idx), tris);
            if let Some(v) = self.sculpt.voxel.as_mut() { v.tris = tris; }
        }
        // P0.5 (v7 save): copy the live working grid into its VoxelBody node so the saved file (which
        // serializes node grids via VoxelGridRecord) persists the CURRENT sculpt — the node's grid is
        // otherwise the stale initial clone from voxel_create. Clone first to drop the self.sculpt.voxel borrow.
        let node_grid = self.sculpt.voxel.as_ref().map(|v| (v.node, v.grid.clone()));
        if let Some((node, grid)) = node_grid {
            if let Some(n) = self.scene.scene_graph.get_mut(node) {
                if let core_math::scene_graph::NodePayload::VoxelBody { grid: g, .. } = &mut n.payload {
                    *g = grid;
                }
            }
        }
    }

    /// P0.5: (re)build the Object-Mode display mesh for any VoxelBody node whose `mesh_cache_id` is
    /// missing from the cache — a LOADED scene with no saved mesh sidecar or a PASTED node
    /// (mesh_cache_id set to None). Extracted from the node's grid. Without it such voxels render
    /// INVISIBLE + unpickable, because `self.sculpt.voxel` is only the ACTIVE working set (loaded/pasted nodes
    /// are never it). Idempotent: skips nodes that already have a live cache entry. CPU-only here; the
    /// GPU upload happens on the next render of the new mesh.
    pub(crate) fn regen_voxel_display_meshes(&mut self) {
        use core_math::scene_graph::{NodeId, NodePayload};
        let nids: Vec<NodeId> = self.scene.scene_graph.all_nodes()
            .filter_map(|(nid, n)| matches!(n.payload, NodePayload::VoxelBody { .. }).then_some(nid))
            .collect();
        for nid in nids {
            let mcid = self.scene.scene_graph.get(nid).and_then(|n|
                if let NodePayload::VoxelBody { mesh_cache_id, .. } = &n.payload { *mesh_cache_id } else { None });
            if mcid.map_or(false, |c| self.cache.mesh_cache.get(c).is_some()) { continue; } // already live
            let mesh = self.scene.scene_graph.get(nid).and_then(|n| {
                if let NodePayload::VoxelBody { grid, .. } = &n.payload {
                    Some(grid.object_display_mesh())
                } else { None }
            });
            if let Some((verts, idx)) = mesh {
                let tris = (idx.len() / 3) as u32;
                let cid = self.cache.mesh_cache.alloc_id();
                self.cache.mesh_cache.insert_imported(cid, (verts, idx), tris);
                if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::VoxelBody { mesh_cache_id, .. } = &mut n.payload { *mesh_cache_id = Some(cid); }
                }
            }
        }
    }

    /// Open the Ctrl+R remesh popup, seeding the target resolution from the current grid.
    pub(crate) fn voxel_open_remesh(&mut self) {
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.remesh_target_res = v.grid.res;
            v.remesh_open = true;
        } else {
            self.toast("No voxel object to remesh");
        }
    }

    /// Ctrl+R in Sculpt mode on a POLY body (owner 2026-08-19: "remesh does not work for
    /// poly object"): open the poly Sculpt Remesh popup — a one-shot voxel remesh of the
    /// authored mesh, the same kernel the Remesh (Voxel) modifier runs.
    pub(crate) fn poly_sculpt_open_remesh(&mut self) {
        if self.sculpt.poly_remesh_res == 0 {
            self.sculpt.poly_remesh_res = 128;
        }
        self.sculpt.poly_remesh_open = true;
    }

    /// The poly Sculpt Remesh popup (Ctrl+R while poly sculpting) — the poly twin of
    /// `draw_voxel_remesh_popup`. Apply runs `poly_sculpt_remesh_now` (one transaction,
    /// Ctrl+Z restores the pre-remesh mesh).
    pub(crate) fn draw_poly_remesh_popup(&mut self, ctx: &egui::Context) {
        if !self.sculpt.poly_remesh_open {
            return;
        }
        let Some(nid) = self
            .sculpt
            .poly_edit
            .map(|(nid, _)| nid)
            .or_else(|| self.poly_sculpt_target())
        else {
            self.sculpt.poly_remesh_open = false;
            return;
        };
        let cur_faces = {
            use core_math::scene_graph::NodePayload;
            self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.faces.len()),
                _ => None,
            })
        };
        let mut open = true;
        let mut target = self.sculpt.poly_remesh_res.clamp(16, 256);
        let mut apply = false;
        egui::Window::new("\u{1F501} Remesh Poly (Voxel)")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                if let Some(f) = cur_faces {
                    ui.label(format!("Current: {f} faces"));
                    ui.separator();
                }
                ui.add(egui::Slider::new(&mut target, 16..=256).text("resolution").step_by(8.0));
                ui.weak("Voxel remesh: even quads, closed input fills solid, open input = crust.");
                if target >= 192 {
                    ui.weak("⚠ high — the transient grid is dense; a moment of UI stall is normal");
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                    if ui.small_button("64").clicked() { target = 64; apply = true; }
                    if ui.small_button("128").clicked() { target = 128; apply = true; }
                    if ui.small_button("192").clicked() { target = 192; apply = true; }
                    if ui.small_button("256").clicked() { target = 256; apply = true; }
                });
                ui.weak("Rebuilds topology (heals pinches/folds). Ctrl+Z restores.");
            });
        self.sculpt.poly_remesh_res = target;
        self.sculpt.poly_remesh_open = open && !apply;
        if apply {
            self.poly_sculpt_remesh_now(nid, target);
        }
    }

    /// The remesh popup (Ctrl+R): choose a target POLYGON COUNT (or the exact resolution) and
    /// apply. Polygon count is the headline control — it scales ~res² with the current surface,
    /// so it's estimated from the live mesh — while the resolution slider is the precise lever;
    /// the two stay linked. Apply resamples the grid + re-extracts (resets sculpt undo).
    pub(crate) fn draw_voxel_remesh_popup(&mut self, ctx: &egui::Context) {
        let Some((mut open, cur_res, cur_tris, mut target)) = self.sculpt.voxel.as_ref()
            .map(|v| (v.remesh_open, v.grid.res, v.tris, v.remesh_target_res)) else { return; };
        if !open { return; }
        // Polygons ≈ current_polys * (res / current_res)² (surface area scales with res²).
        let est_polys = |res: u32| -> u32 {
            if cur_res >= 2 { (cur_tris as f32 * (res as f32 / cur_res as f32).powi(2)).max(0.0) as u32 }
            else { cur_tris }
        };
        let mut apply = false;
        egui::Window::new("\u{1F501} Remesh Voxel")
            .collapsible(false).resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Current: {}\u{B3} \u{2022} {} polygons", cur_res, cur_tris));
                ui.separator();
                ui.label("Target polygon count:");
                let mut polys = est_polys(target);
                if ui.add(egui::DragValue::new(&mut polys).speed(200.0)).changed() {
                    let ratio = (polys as f32 / cur_tris.max(1) as f32).sqrt();
                    target = ((cur_res as f32 * ratio).round() as i64).clamp(16, 1024) as u32;
                }
                // ≥256 switches the grid to the SPARSE brick backing (memory-scalable; the GPU atlas is
                // the session authority, no per-stroke readback). 1024³ is sparse → viable now that the
                // readback wall is gone; watch VRAM (resident chunk buffers ≈ surface area).
                ui.add(egui::Slider::new(&mut target, 16..=1024).text("resolution").step_by(8.0));
                if target >= 256 { ui.weak("⚡ sparse (brick) storage"); }
                if target >= 512 {
                    ui.weak("Live sculpt: raymarch (no mesh tear); triangles on leave / ≤511³");
                }
                if target >= 768 {
                    ui.weak("⚠ high — remesh worker streams off-UI; VRAM for surface shell");
                }
                if target >= 1024 {
                    ui.weak("1024³: sparse bricks + off-thread remesh · keep brush moderate");
                }
                ui.weak(format!("\u{2192} {}\u{B3} \u{2248} {} polygons", target, est_polys(target)));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() { apply = true; }
                    if ui.small_button("64").clicked() { target = 64; apply = true; }
                    if ui.small_button("128").clicked() { target = 128; apply = true; }
                    if ui.small_button("256").clicked() { target = 256; apply = true; }
                    if ui.small_button("512").clicked() { target = 512; apply = true; }
                    if ui.small_button("1024").clicked() { target = 1024; apply = true; }
                });
                ui.weak("Higher = smoother & more detail. Ctrl+Z restores previous res.");
            });
        if apply { open = false; }
        if let Some(v) = self.sculpt.voxel.as_mut() { v.remesh_open = open; v.remesh_target_res = target; }
        if apply { self.voxel_resample_to(target); }
    }
}
