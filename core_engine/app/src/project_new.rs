//! New scene + recent files + window title.

use std::path::PathBuf;
use crate::{FourthLayerApp, DirtyFlags};
use super::project::save_recent_files;

impl FourthLayerApp {
    /// Drop every DERIVED cache keyed by `NodeId` / mesh signature. Whenever the scene graph
    /// is wholesale REPLACED — New Scene or Open Project — these describe nodes that no longer
    /// exist, and the replacement can reuse the same `NodeId`s for different geometry.
    ///
    /// ONE function on purpose. This block lived only in `new_scene`, so `load_project`
    /// reset **none** of it (it cleared `poly_custom_data` and nothing else) — two code paths
    /// for the same operation, which CLAUDE.md forbids. The consequences of the missing half
    /// are real, not theoretical:
    ///   * `poly_mesh_gpu_key` is the UPLOAD stamp. A stale entry for a reused `NodeId` makes
    ///     the sync early-out and never upload the loaded mesh — geometry silently missing.
    ///   * `poly_display_key_memo` is keyed by `poly_edit_generation`. Without the bump, a
    ///     memo hit returns the PREVIOUS scene's cache key, so the stale display mesh is served.
    ///   * `poly_display_cache` / `poly_prefix_cache` are content-keyed, so only the 64-bit
    ///     signature compare was standing between a load and a wrong mesh.
    ///
    /// Callers must still mark the scene dirty themselves; this only drops caches.
    pub(crate) fn reset_derived_scene_caches(&mut self) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.mat_bake_pipe_cache.poly_curvature.reset(&mut self.cache.mesh_cache);
        }
        self.poly_display_cache.clear(); // stale NodeId->signature entries: the GPU cache is fresh
        self.poly_wire_local_cache.clear();
        self.poly_prefix_cache.clear();
        self.poly_sculpt_working.clear();
        self.poly_prefix_total_bytes = 0;
        self.poly_lod_cache.clear();
        self.poly_lod_shared.clear();
        self.poly_edit_generation = self.poly_edit_generation.wrapping_add(1);
        self.poly_display_key_memo.borrow_mut().clear();
        self.poly_lod_latch_gen = u64::MAX;
        self.poly_lod_latch_cam_bucket = u64::MAX;
        self.draw_stream.clear();
        self.draw_stream_stale = true;
        self.draw_stream_rebuilds_last_frame = 0;
        self.structural_materials.clear();
        self.structural_draw_calls.clear();
        self.structural_world_aabb.clear();
        self.poly_meshes_drawable_ok = false;
        self.raster_upload_plane_key = 0;
        self.structural_draw_kind.clear();
        self.structural_draw_kind_payload.clear();
        self.structural_draw_stale = true;
        self.structural_material_gen = 0;
        self.structural_draw_rebuilds_last_frame = 0;
        self.structural_outline_key = 0;
        self.structural_any_sdf = false;
        self.cache.body_index_map.clear();
        self.cache.poly_mesh_gpu_key.clear();
    }

    pub(crate) fn new_scene(&mut self) {
        // Empty scene: Scene root + default Sun only. NO Body.001 / starter Box —
        // first SDF create (`add_prim_to_graph` → `ensure_sdf_body`) brings the body.
        self.scene.primitives.clear();
        self.scene.next_prim_id = 1;
        self.scene.next_node_name_id = 1;
        self.scene.next_light_id = 2;
        // Selection — reset everything
        self.sel = Default::default();
        self.sel.usel = Default::default();
        // Camera
        self.viewport.camera.orbit = glam::Quat::from_euler(glam::EulerRot::YXZ, -0.5, 0.3, 0.0);
        self.viewport.camera.distance = 5.0;
        self.viewport.camera.target = glam::Vec3::ZERO;
        // Project state
        self.project.project_path = None;
        self.project.save_blocked_reason = None;
        self.project.last_auto_save = std::time::Instant::now();
        self.render.pt_accumulated_frames = 0;
        // Compositor — reset to defaults
        self.compositor = Default::default();
        // Scene graph and mesh cache
        self.scene.scene_graph = core_math::scene_graph::SceneGraph::new();
        self.cache.mesh_cache = render::mesh_cache::MeshCache::new();
        self.reset_pt_accel_state();
        self.reset_derived_scene_caches();
        self.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
        // Edit mode / tool / poly state
        self.modeling_context = crate::ModelingContext::Sdf;
        self.tool.work_mode = crate::WorkMode::Object;
        self.poly_work_mode = crate::PolyWorkMode::Object;
        self.poly_sel = Default::default();
        self.tool.current = crate::ActiveTool::Idle;
        self.tool.mode = crate::ToolMode::Select;
        // Undo history
        self.history = crate::UndoHistory::new(self.history.max_entries);
        // Mesh Terrain runtime map (not in .fl4) — never keep dead NodeIds across New.
        self.terrain_world.clear();
        // Outliner state
        self.outliner = Default::default();
        // Node shader graphs (prim overrides + outliner materials)
        self.ui.node_graphs.clear();
        self.ui.material_node_graphs.clear();
        self.ui.node_graph_target = None;
        self.shader_dirty = true;
        self.mark_dirty(DirtyFlags::RENDER);
        // Texture / environment data — reset to prevent leaking from old scene
        self.env_hdr_data.clear();
        self.env_hdr_mip_data.clear();
        self.env_hdr_width = 0;
        self.env_hdr_height = 0;
        // env_ibl: invalidate in-flight env workers + swap the GPU buffer in poll
        // (Frame Ordering — this runs from a panel/menu path).
        self.env_load_gen = self.env_load_gen.wrapping_add(1);
        self.env_prefilter_queue.clear();
        self.env_clear_pending = true;
        self.render.env_rotation_deg = 0.0; // authored env state (FL4ENVIR) — reset with the env
        self.render.env_reflection_blur = 0.0;
        self.render.env_enabled = true; // unmute on new scene
        self.render.env_bg_visible = true; // un-hide the sky plate with the env
        self.matcap_data.clear();
        self.matcap_width = 0;
        self.matcap_height = 0;
        self.tex_atlas_data.clear();
        self.tex_atlas_meta.clear();
        self.tex_atlas_next_id = 1;
        self.tex_atlas_dirty = false;
        self.cache.rebake_hash = 0;
        self.cache.body_vol_cache.invalidate_all();
        self.cache.use_body_vol_cache = true;
        // Bool edge cache
        self.bool_edges.clear();
        self.bool_edge_hovered = None;
        self.bool_edge_dirty = true;
        // Tool / sculpt / macro transient state
        self.cache.suppress_body_vol_dirty = false;
        self.cache.body_vol_cache.suppress_bake_dispatch = false;
        self.poly_tool = crate::PolyTool::Idle;
        self.nurbs_work_mode = crate::NurbsWorkMode::Object;
        self.knife_points.clear();
        self.knife_snap_pos = None;
        self.bevel_pv = Default::default();
        // Workplane — reset to defaults (M87)
        self.viewport.wp_mode = crate::WorkPlaneMode::Auto;
        self.viewport.wp_settings = crate::WorkPlaneSettings::default();
        self.viewport.wp_resolved = crate::ResolvedWorkPlane::default();
        self.viewport.wp_locked_origin = glam::Vec3::ZERO;
        self.viewport.wp_locked_normal = glam::Vec3::Y;
        self.viewport.wp_saved.clear();
        self.sdf_sculpt_active = false;
        self.sdf_brush_preview_pos = None;
        // WORLD-space point from the outgoing scene — meaningless in the new one.
        self.poly_prop_origin = None;
        self.poly_prop_handle_hovered = false;
        self.sculpt.stroke_active = false;
        self.sculpt.current_stroke.clear();
        self.voxel_undo_reset();
        self.macro_recording = false;
        self.macro_buffer.clear();
        // Rebuild graph — empty prims → no body. Point body nids at root (invalid body)
        // until the first SDF create heals them via ensure_sdf_body.
        self.mark_dirty(DirtyFlags::LIGHT | DirtyFlags::SCENE | DirtyFlags::MESH);
        self.build_graph_from_flat();
        self.scene.sdf_body_nid = core_math::scene_graph::NodeId(0);
        self.scene.active_body_nid = core_math::scene_graph::NodeId(0);
        self.scene.isolated_body_nid = None;
        // Add default light to graph (graph owns lights)
        let default_light = core_math::scene_graph::LightData::default();
        let root = self.scene.scene_graph.root();
        self.scene.scene_graph.add_child(root, core_math::scene_graph::NodePayload::Light(default_light));
        // NO auto "Default Material" node (owner refinement 2026-07-17): materials are items the
        // user explicitly adds (Add → Material). Prims fall back to their embedded material via
        // resolve_prim_material when material_id == NONE, so zero materials is fully functional —
        // and the outliner's Materials section stays hidden until the first real material exists.
        // Clear scene_modified AFTER mark_dirty(SCENE) which sets it to true.
        // A fresh scene has no unsaved changes.
        self.project.scene_modified = false;
        self.toast("New Scene");
    }

    pub(crate) fn add_recent_file(&mut self, path: PathBuf) {
        self.project.recent_files.retain(|p| p != &path);
        self.project.recent_files.insert(0, path);
        if self.project.recent_files.len() > 10 {
            self.project.recent_files.truncate(10);
        }
        save_recent_files(&self.project.recent_files);
    }

    /// Window title: "FourthLayer AI — filename.fl4 *"
    pub(crate) fn window_title(&self) -> String {
        let base = "FourthLayer AI";
        if let Some(ref path) = self.project.project_path {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("scene");
            let star = if self.project.scene_modified { " *" } else { "" };
            format!("{} \u{2014} {}{}", base, name, star)
        } else {
            base.to_string()
        }
    }
}
