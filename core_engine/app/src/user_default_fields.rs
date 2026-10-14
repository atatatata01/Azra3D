// user_default_fields.rs — startup apply of the user's "Set as default" overrides.
//
// The ONE table of app-state toggles that can be defaulted: key → field. Adding a toggle =
// one line here + `.default_menu(KEY, json!(value))` on its widget (`ui_menus::DefaultMenu`).
// Tool options that are read when the tool starts (e.g. "poly.extrude_along_normals_grouped")
// do NOT live here — they call `app_types::user_defaults::get_bool` at tool start.
use crate::*;

macro_rules! bool_default_fields {
    ($($key:literal => $($path:ident).+),* $(,)?) => {
        impl FourthLayerApp {
            fn user_default_bool_field(&mut self, key: &str) -> Option<&mut bool> {
                match key {
                    $($key => Some(&mut self.$($path).+),)*
                    _ => None,
                }
            }
        }
    };
}

bool_default_fields! {
    "render.auto_reflection_probe"     => render.auto_reflection_probe,
    "render.bloom_enabled"             => render.bloom_enabled,
    "render.dither_enabled"            => render.dither_enabled,
    "render.env_bg_visible"            => render.env_bg_visible,
    "render.env_enabled"               => render.env_enabled,
    "render.fog_enabled"               => render.fog_enabled,
    "render.foveated_enabled"          => render.foveated_enabled,
    "render.fxaa_enabled"              => render.fxaa_enabled,
    "render.interaction_half_res"      => render.interaction_half_res,
    "render.interaction_shadows"       => render.interaction_shadows,
    "render.lod_dist_enabled"          => render.lod_dist_enabled,
    "render.march_adaptive_enabled"    => render.march_adaptive_enabled,
    "render.matcap_bg_gradient"        => render.matcap_bg_gradient,
    "render.matcap_edge_enabled"       => render.matcap_edge_enabled,
    "render.matcap_material_tint"      => render.matcap_material_tint,
    "render.pbr_progressive_enabled"   => render.pbr_progressive_enabled,
    "render.pbr_refine_gi"             => render.pbr_refine_gi,
    "render.pbr_refine_light_probe"    => render.pbr_refine_light_probe,
    "render.pbr_refine_reflections"    => render.pbr_refine_reflections,
    "render.pbr_refine_rt_sun"         => render.pbr_refine_rt_sun,
    "render.physical_light_falloff"    => render.physical_light_falloff,
    "render.pt_adaptive_spp"           => render.pt_adaptive_spp,
    "render.pt_denoise"                => render.pt_denoise,
    "render.pt_denoise_end_on_hdr"     => render.pt_denoise_end_on_hdr,
    "render.pt_draft_soft_reset"       => render.pt_draft_soft_reset,
    "render.pt_fast_mode"              => render.pt_fast_mode,
    "render.pt_live_draft_mesh"        => render.pt_live_draft_mesh,
    "render.pt_live_hybrid_edit"       => render.pt_live_hybrid_edit,
    "render.pt_show_spp"               => render.pt_show_spp,
    "render.pt_soft_hybrid_resume"     => render.pt_soft_hybrid_resume,
    "render.pt_temporal_reproject"     => render.pt_temporal_reproject,
    "render.pt_transparent_bg"         => render.pt_transparent_bg,
    "render.rt_caustics"               => render.rt_caustics,
    "render.rt_shadow_full_res"        => render.rt_shadow_full_res,
    "render.rt_shadows"                => render.rt_shadows,
    "render.sdf_edge_aa_enabled"       => render.sdf_edge_aa_enabled,
    "render.ssao_enabled"              => render.ssao_enabled,
    "render.ssgi_enabled"              => render.ssgi_enabled,
    "render.ssr_enabled"               => render.ssr_enabled,
    "render.upsample_enabled"          => render.upsample_enabled,
    "render.volume_ao_enabled"         => render.volume_ao_enabled,
    "render.volume_ao_in_orbit"        => render.volume_ao_in_orbit,
    "render.volume_ao_invert"          => render.volume_ao_invert,
    "render.volume_cone_step"          => render.volume_cone_step,
    "render.volume_early_out"          => render.volume_early_out,
    "render.volume_tricubic"           => render.volume_tricubic,
    "render.wire_aa_enabled"           => render.wire_aa_enabled,
    "render.wire_overlay_backface"     => render.wire_overlay_backface,
    "render.wire_overlay_enabled"      => render.wire_overlay_enabled,
    "viewport.grid_show_axes"          => viewport.grid_show_axes,
    "viewport.grid_visible"            => viewport.grid_visible,
    "viewport.poly_sel_show_backface"  => viewport.poly_sel_show_backface,
    "viewport.show_edge_weights"       => viewport.show_edge_weights,
    "viewport.show_gizmo"              => viewport.show_gizmo,
    "viewport.show_light_gizmos"       => viewport.show_light_gizmos,
    "viewport.show_overlays"           => viewport.show_overlays,
    "viewport.show_poly_overlays"      => viewport.show_poly_overlays,
    "viewport.show_probe_gizmo"        => viewport.show_probe_gizmo,
    "viewport.show_reset_handle"       => viewport.show_reset_handle,
    "viewport.show_sdf"                => viewport.show_sdf,
    "viewport.show_sel_rim"            => viewport.show_sel_rim,
    "viewport.show_selection_ui"       => viewport.show_selection_ui,
    "viewport.show_sharp_edges"        => viewport.show_sharp_edges,
    "viewport.show_tool_viewport_ui"   => viewport.show_tool_viewport_ui,
    "viewport.show_uv_seams"           => viewport.show_uv_seams,
    "viewport.show_workplane"          => viewport.show_workplane,
    "viewport.smart_snap_center"       => viewport.smart_snap_center,
    "viewport.smart_snap_edge"         => viewport.smart_snap_edge,
    "viewport.smart_snap_edge_center"  => viewport.smart_snap_edge_center,
    "viewport.smart_snap_face_center"  => viewport.smart_snap_face_center,
    "viewport.smart_snap_grid"         => viewport.smart_snap_grid,
    "viewport.smart_snap_on"           => viewport.smart_snap_on,
    "viewport.smart_snap_vertex"       => viewport.smart_snap_vertex,
    "viewport.smart_snap_visible_only" => viewport.smart_snap_visible_only,
}

impl FourthLayerApp {
    /// Apply every saved default once at startup (before the first frame).
    pub(crate) fn apply_user_defaults(&mut self) {
        let mut saved = Vec::new();
        app_types::user_defaults::for_each(|k, v| saved.push((k.to_string(), v.clone())));
        for (key, val) in saved {
            if let Some(b) = val.as_bool() {
                if let Some(f) = self.user_default_bool_field(&key) { *f = b; }
                // The Snap menu writes both flags together; keep them in lockstep here too.
                if key == "viewport.smart_snap_on" { self.viewport.snap_enabled = b; }
            } else if key == "render.matcap_bg_color" {
                if let Some(c) = app_types::user_defaults::get_color3(&key) {
                    self.render.matcap_bg_color = c;
                }
            }
        }
    }
}
