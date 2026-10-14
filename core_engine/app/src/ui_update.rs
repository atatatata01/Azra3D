// core_engine/app/src/ui_update.rs
// Extracted update() sub-blocks — menu bar, keyboard, viewport header, left tool strip,
// viewport interaction, GPU dispatch, viewport overlays, context menu.
use crate::{
    AzraApp, WorkMode, DirtyFlags, ModelingContext, SceneItemId,
    CompositorEffectType, CompositorEffect, CompositorMaskSource,
};
use graphics::gpu_volume::SdfGpuVolume;

use crate::CompositorStack;

/// Build per-prim light link bitmask buffer.
/// **Re-export of `light_pack::build_light_link_masks`** — targets must be **flat prim indices**
/// (resolve prim_uid at pack time; see P3.1 design).
pub(crate) use crate::light_pack::build_light_link_masks;

pub(crate) fn pack_compositor_buf(c: &CompositorStack, vp_w: u32, vp_h: u32) -> Vec<f32> {
        let mut buf = vec![0.0f32; 256];
        buf[0] = if c.enabled { 1.0 } else { 0.0 };
        buf[1] = 0.0; // time — TODO: wire elapsed time
        buf[2] = vp_w as f32;
        buf[3] = vp_h as f32;

        // Helper: find first enabled effect of a given type
        let find_effect = |etype: CompositorEffectType| -> Option<&CompositorEffect> {
            c.effects.iter().find(|e| e.effect_type == etype && e.enabled)
        };

        // --- Bloom [4..11] ---
        if let Some(e) = find_effect(CompositorEffectType::Bloom) {
            buf[4] = 1.0; buf[5] = e.bloom_threshold; buf[6] = e.bloom_intensity; buf[7] = e.bloom_radius;
            buf[8] = e.bloom_tint[0]; buf[9] = e.bloom_tint[1]; buf[10] = e.bloom_tint[2];
            buf[11] = e.bloom_anamorphic;
        }
        // --- DoF [12..17] ---
        if let Some(e) = find_effect(CompositorEffectType::DepthOfField) {
            buf[12] = 1.0; buf[13] = e.dof_focus_dist; buf[14] = e.dof_aperture; buf[15] = e.dof_max_blur;
            buf[16] = e.dof_bokeh_shape as f32; buf[17] = e.dof_chromatic;
        }
        // --- Motion Blur [18..22] ---
        if let Some(e) = find_effect(CompositorEffectType::MotionBlur) {
            buf[18] = 1.0; buf[19] = e.mblur_intensity; buf[20] = e.mblur_samples as f32;
            buf[21] = e.mblur_max_length; buf[22] = e.mblur_shutter_angle;
        }
        // --- Chromatic Aberration [23..25] ---
        if let Some(e) = find_effect(CompositorEffectType::ChromaticAberration) {
            buf[23] = 1.0; buf[24] = e.ca_intensity; buf[25] = e.ca_mode as f32;
        }
        // --- Sharpen [26..30] ---
        if let Some(e) = find_effect(CompositorEffectType::Sharpen) {
            buf[26] = 1.0; buf[27] = e.sharpen_strength; buf[28] = e.sharpen_radius;
            buf[29] = e.sharpen_threshold; buf[30] = if e.sharpen_clarity { 1.0 } else { 0.0 };
        }
        // --- Lens Distortion [31..34] ---
        if let Some(e) = find_effect(CompositorEffectType::LensDistortion) {
            buf[31] = 1.0; buf[32] = e.distort_barrel;
            buf[33] = if e.distort_chromatic { 1.0 } else { 0.0 }; buf[34] = e.distort_scale;
        }
        // --- Fog [35..44] ---
        if let Some(e) = find_effect(CompositorEffectType::Fog) {
            buf[35] = 1.0; buf[36] = e.fog_color[0]; buf[37] = e.fog_color[1]; buf[38] = e.fog_color[2];
            buf[39] = e.fog_density; buf[40] = e.fog_start; buf[41] = e.fog_end;
            buf[42] = e.fog_mode as f32; buf[43] = e.fog_height_density; buf[44] = e.fog_height_baseline;
        }
        // --- Outline [45..56] ---
        if let Some(e) = find_effect(CompositorEffectType::Outline) {
            buf[45] = 1.0; buf[46] = e.outline_source as f32; buf[47] = e.outline_width;
            buf[48] = e.outline_color[0]; buf[49] = e.outline_color[1]; buf[50] = e.outline_color[2]; buf[51] = e.outline_color[3];
            buf[52] = e.outline_normal_thresh; buf[53] = e.outline_depth_thresh;
            buf[54] = if e.outline_surface_id { 1.0 } else { 0.0 };
            buf[55] = e.outline_fill_mode as f32; buf[56] = e.outline_cel_steps as f32;
        }
        // --- Vignette [57..65] ---
        if let Some(e) = find_effect(CompositorEffectType::Vignette) {
            buf[57] = 1.0; buf[58] = e.vignette_intensity;
            buf[59] = e.vignette_color[0]; buf[60] = e.vignette_color[1]; buf[61] = e.vignette_color[2];
            buf[62] = e.vignette_shape as f32; buf[63] = e.vignette_radius; buf[64] = e.vignette_softness;
            buf[65] = e.vignette_blend as f32;
        }
        // --- Film Grain [66..73] ---
        if let Some(e) = find_effect(CompositorEffectType::FilmGrain) {
            buf[66] = 1.0; buf[67] = e.grain_noise_type as f32; buf[68] = e.grain_intensity;
            buf[69] = e.grain_size; buf[70] = if e.grain_monochromatic { 1.0 } else { 0.0 };
            buf[71] = if e.grain_animate { 1.0 } else { 0.0 }; buf[72] = e.grain_blend as f32;
            buf[73] = if e.grain_luminance_respond { 1.0 } else { 0.0 };
        }
        // --- Color Grading [74..91] ---
        if let Some(e) = find_effect(CompositorEffectType::ColorGrading) {
            buf[74] = 1.0; buf[75] = e.grade_hue_shift; buf[76] = e.grade_saturation; buf[77] = e.grade_vibrance;
            buf[78] = e.grade_brightness; buf[79] = e.grade_contrast; buf[80] = e.grade_gamma;
            buf[81] = e.grade_highlights; buf[82] = e.grade_shadows;
            buf[83] = e.grade_balance_shadows[0]; buf[84] = e.grade_balance_shadows[1]; buf[85] = e.grade_balance_shadows[2];
            buf[86] = e.grade_balance_midtones[0]; buf[87] = e.grade_balance_midtones[1]; buf[88] = e.grade_balance_midtones[2];
            buf[89] = e.grade_balance_highlights[0]; buf[90] = e.grade_balance_highlights[1]; buf[91] = e.grade_balance_highlights[2];
        }
        // --- Color Management [92..97] ---
        buf[92] = c.tonemapper as f32;
        buf[93] = c.cm_exposure;
        buf[94] = if c.cm_auto_exposure { 1.0 } else { 0.0 };
        buf[95] = c.cm_white_balance_temp;
        buf[96] = c.cm_white_balance_tint;
        buf[97] = c.lut_intensity;
        // Mask preview: 0=off, 1..11 = effect index + 1
        buf[98] = c.mask_preview_effect.map(|i| (i + 1) as f32).unwrap_or(0.0);

        // --- Mask params [100..165] (11 effects × 6 floats) ---
        // For each effect, pack the first mask layer if present
        let effect_order = [
            CompositorEffectType::Bloom,
            CompositorEffectType::DepthOfField,
            CompositorEffectType::MotionBlur,
            CompositorEffectType::ChromaticAberration,
            CompositorEffectType::Sharpen,
            CompositorEffectType::Outline,
            CompositorEffectType::Fog,
            CompositorEffectType::LensDistortion,
            CompositorEffectType::Vignette,
            CompositorEffectType::FilmGrain,
            CompositorEffectType::ColorGrading,
        ];
        for (i, etype) in effect_order.iter().enumerate() {
            let base = 100 + i * 6;
            if let Some(e) = c.effects.iter().find(|e| e.effect_type == *etype) {
                if let Some(ml) = e.masks.first() {
                    buf[base] = match ml.source {
                        CompositorMaskSource::None => 0.0,
                        CompositorMaskSource::Noise3D { .. } => 1.0,
                        CompositorMaskSource::SurfaceId { .. } => 2.0,
                        CompositorMaskSource::SdfDistance { .. } => 3.0,
                        CompositorMaskSource::Curvature { .. } => 4.0,
                        CompositorMaskSource::Depth { .. } => 5.0,
                        CompositorMaskSource::NormalDirection { .. } => 6.0,
                        CompositorMaskSource::Fresnel { .. } => 7.0,
                        CompositorMaskSource::Vignette { .. } => 8.0,
                        CompositorMaskSource::MaterialId { .. } => 9.0,
                        CompositorMaskSource::Texture { .. } => 10.0,
                    };
                    buf[base + 1] = if ml.invert { 1.0 } else { 0.0 };
                    // Pack source-specific params into p0..p3
                    match &ml.source {
                        CompositorMaskSource::Noise3D { scale, octaves, persistence, contrast, .. } => {
                            buf[base + 2] = *scale; buf[base + 3] = *octaves as f32;
                            buf[base + 4] = *persistence; buf[base + 5] = *contrast;
                        }
                        CompositorMaskSource::SurfaceId { ids, .. } => {
                            buf[base + 2] = ids.first().copied().unwrap_or(0) as f32;
                        }
                        CompositorMaskSource::MaterialId { ids, .. } => {
                            buf[base + 2] = ids.first().copied().unwrap_or(0) as f32;
                        }
                        CompositorMaskSource::Texture { slot, channel, .. } => {
                            buf[base + 2] = *slot as f32; buf[base + 3] = *channel as f32;
                        }
                        CompositorMaskSource::SdfDistance { near, far, .. } | CompositorMaskSource::Depth { near, far, .. } => {
                            buf[base + 2] = *near; buf[base + 3] = *far;
                        }
                        CompositorMaskSource::Curvature { min_k, max_k, .. } => {
                            buf[base + 2] = *min_k; buf[base + 3] = *max_k;
                        }
                        CompositorMaskSource::NormalDirection { threshold, .. } => {
                            buf[base + 2] = *threshold;
                        }
                        CompositorMaskSource::Fresnel { power, .. } => {
                            buf[base + 2] = *power;
                        }
                        CompositorMaskSource::Vignette { radius, softness } => {
                            buf[base + 2] = *radius; buf[base + 3] = *softness;
                        }
                        _ => {}
                    }
                }
            }
        }

        buf
}

impl AzraApp {
    // ================================================================
    // 1. Menu bar
    // ================================================================
    // draw_menu_bar moved to menus.rs

    // ================================================================
    // 1b. Work mode switching (handles edit_target enter/exit)
    // ================================================================
    pub(crate) fn switch_work_mode(&mut self, mode: WorkMode) {
        let is_edit = matches!(mode, WorkMode::Vertex | WorkMode::Edge | WorkMode::Face);
        let is_sdf = self.modeling_context == ModelingContext::Sdf;

        if is_edit {
            if is_sdf {
                // SDF edit mode: no edit_target needed — just switch to prim-level selection.
                // If a body Node is selected, clear it so prim clicks work cleanly.
                self.deselect_scene_node();
            } else {
                // Polygon/NURBS edit mode: lock to the active primitive
                if let Some(idx) = self.active_prim_index_unified() {
                    self.sel.edit_target = Some(idx);
                    self.sel.selected_faces.clear();
                    self.tool.selected_bevel_edges.clear();
                } else {
                    self.toast("Select an object first");
                    return; // stay in current mode
                }
            }
        } else {
            // Exiting edit → Object/Body mode
            if self.sel.edit_target.is_some() {
                self.sel.edit_target = None;
                self.sel.selected_faces.clear();
                self.tool.selected_bevel_edges.clear();
            }
            if is_sdf && mode == WorkMode::Object {
                // Entering Body mode: select the active body as a Node.
                // Clear prim selection — body-level gizmo takes over.
                self.remove_from_selection(|id| matches!(id, SceneItemId::Prim(_)));
                self.select_scene_node(self.scene.active_body_nid);
            }
        }
        if mode == WorkMode::Sculpt {
            // NEW voxel sculpting room. The legacy cpu_volume / SDF-raymarch sculpt path is retired.
            // Do NOT auto-create a starting sphere — the user adds a subject explicitly
            // (● Sphere / ■ Box in the sculpt panel). enter_sculpt_mode() handles the toast hint.
            self.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
        }
        if mode == WorkMode::Object && self.cache.use_body_vol_cache {
            // Need-based, NOT mark_all_bodies_dirty: a work-mode switch changes no bake
            // input, so only bodies with a missing/draft/regime-unservable grid re-bake.
            // The blanket mark re-baked EVERY body per Vertex↔Object toggle (selection-
            // change bake churn — see mark_stale_bodies_dirty).
            self.cache.body_vol_cache.mark_stale_bodies_dirty(&self.scene.scene_graph);
            self.trigger_body_vol_bake();
        }
        // Leaving the Sculpt room (Tab/keyboard/menu path): deactivate the live voxel to its
        // stable Object-Mode display mesh. Mirrors the inverse of the `mode == WorkMode::Sculpt`
        // entry block above.
        if self.tool.work_mode == WorkMode::Sculpt && mode != WorkMode::Sculpt {
            self.restore_sdf_room_render_state();
        }
        self.tool.work_mode = mode;
        self.mark_dirty(DirtyFlags::RENDER);
        // Auto-enable boolean edge overlay in Edge mode
        if mode == WorkMode::Edge && !self.ui.show_bool_edges {
            self.ui.show_bool_edges = true;
            self.bool_edge_dirty = true;
        }
        self.toast(if is_sdf {
            match mode {
                WorkMode::Object => "Body Mode",
                WorkMode::Vertex => "Edit Mode",
                _ => match mode {
                    WorkMode::Sculpt => "Sculpt Mode",
                    WorkMode::Edge   => "Edge Mode",
                    WorkMode::Face   => "Face Mode",
                    _ => "Object Mode",
                },
            }
        } else {
            match mode {
                WorkMode::Object => "Object Mode",
                WorkMode::Sculpt => "Sculpt Mode",
                WorkMode::Vertex => "Vertex Mode",
                WorkMode::Edge   => "Edge Mode",
                WorkMode::Face   => "Face Mode",
            }
        });
    }

    // ================================================================
    // 2. Keyboard handler
    // ================================================================
    // handle_keyboard moved to input.rs

    // ================================================================
    // 5. Viewport interaction (mouse/scroll/drag handling)
    // ================================================================
    // handle_viewport_input moved to input.rs

    // ================================================================
    // 2b. Click selection — extracted from handle_viewport_input
    // ================================================================
    // handle_click_select moved to input.rs

    // handle_object_click moved to input.rs

    // ================================================================
    // Remaining UI helpers live in:
    //   input.rs, viewport.rs, tool_settings.rs, menus.rs, cuda_render_dispatch.rs
    // ================================================================
}
