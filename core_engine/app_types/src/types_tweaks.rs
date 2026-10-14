// Developer Tweak Settings — runtime-adjustable tool parameters — extracted from types.rs
use serde::{Serialize, Deserialize};

// ==================================================================
// Developer Tweak Settings — runtime-adjustable tool parameters
// ==================================================================
#[derive(Clone, Serialize, Deserialize)]
pub struct ToolSettings {
    // -- Transform (G/R/S modal + gizmo drag) --
    pub grab_speed: f32,            // world-units per pixel (scaled by cam_dist)
    pub rotate_speed: f32,          // degrees per pixel
    pub scale_speed: f32,           // scale-factor per pixel

    // -- Camera --
    pub cam_orbit_speed: f32,       // radians per pixel
    pub cam_pan_speed: f32,         // fraction of cam_dist per pixel
    pub cam_zoom_speed: f32,        // per scroll unit
    pub cam_zoom_min: f32,
    pub cam_zoom_max: f32,

    // -- Face Push/Pull --
    pub face_drag_speed: f32,       // world-units per pixel (scaled by cam_dist)

    // -- Extrude --
    /// Stacked coplanar wall cleanup on extrude confirm: 0 = Off, nonzero = On
    /// (SketchUp push/pull — inward AND outward). `1` (legacy "Intrude only") is
    /// treated as On: an outward inner-wall extrude must merge adjacent planes too.
    #[serde(default = "default_extrude_wall_cleanup")]
    pub extrude_wall_cleanup: u8,
    /// Push-through DEBRIS merge radius, as a FRACTION of the result's bounding-box diagonal.
    /// 0 = off. Default 1e-4.
    ///
    /// This is a SEPARATE knob from [`Self::extrude_wall_cleanup`], which only collapses stacked
    /// coplanar WALLS. It targets the other failure the owner sees as "the side plane is not
    /// clean": a through-cut leaves clusters of near-duplicate vertices where the cutter grazes an
    /// existing corner, and those pin the flat side planes into triangle fans, folded coplanar
    /// pairs, and the odd zero-area face. Each cut then inherits the previous one's debris, so it
    /// compounds.
    ///
    /// It is the TOOL's own pass, deliberately NOT a loosening of the boolean kernel's internal
    /// `collapse_epsilon_debris`. That one is capped at bbox·1e-5 and gated on preserving every
    /// input vertex — both on purpose ("a boolean must not change geometry it did not cut"), and
    /// relaxing them would change every boolean everywhere. Measured on the owner's own capture:
    /// nothing welds at all below ~2e-4, the mesh is materially cleaner from 3e-4 (v 41→36,
    /// f 42→33, tris 21→13, zero-area face gone, volume drift 0.016%), and it starts breaking
    /// manifoldness at 5e-3. Relative to the bbox diagonal, 1e-4 lands in the middle of that band
    /// for any mesh size. Authored edges (Smart Select tags / sharp / seam / crease) are protected
    /// and never welded.
    #[serde(default = "default_extrude_debris_merge")]
    pub extrude_debris_merge: f32,
    /// When false (default): new extrude walls inherit each source face's assigned `material_id`.
    /// When true: new walls use [`Self::extrude_chosen_material_id`] instead.
    #[serde(default)]
    pub extrude_use_chosen_material: bool,
    /// Material id applied to new extrude walls when `extrude_use_chosen_material` is on.
    /// 0 = body default (no shared material).
    #[serde(default)]
    pub extrude_chosen_material_id: u32,
    /// Knife `P` pipe target: true = its own object, false = an island inside the active mesh.
    pub pipe_own_object: bool,

    // -- Bevel --
    pub bevel_scroll_speed: f32,    // fraction of box size per scroll
    pub bevel_drag_speed: f32,      // radius per pixel
    pub bevel_hover_px: f32,        // edge hover detection radius (pixels)
    pub blend_k_speed: f32,         // blend-k per pixel

    // -- Raycast (CPU picking) --
    pub ray_max_steps: u32,
    pub ray_hit_threshold: f32,
    pub ray_normal_eps: f32,
    #[serde(default = "default_ray_max_dist")]
    pub ray_max_dist: f32,          // max raymarch distance (floor; scales with cam_dist)

    // -- Camera (extra) --
    #[serde(default = "default_cam_dist")]
    pub default_cam_dist: f32,      // initial camera distance on reset view
    #[serde(default = "default_cam_pitch_clamp")]
    pub cam_pitch_clamp: f32,       // max pitch in radians (prevents gimbal flip)

    // -- Selection / Interaction --
    #[serde(default = "default_selection_circle_px")]
    pub selection_circle_px: f32,   // selection highlight radius (pixels)
    #[serde(default = "default_depth_pick_drift_px")]
    pub depth_pick_drift_px: f32,   // mouse drift to cancel depth pick (pixels)
    #[serde(default = "default_pie_menu_radius")]
    pub pie_menu_radius: f32,       // pie menu activation radius (pixels)

    // -- Smart Mirror --
    #[serde(default = "default_mirror_rotate_speed")]
    pub mirror_rotate_speed: f32,   // radians per scroll unit
    #[serde(default = "default_mirror_snap_angle")]
    pub mirror_snap_angle: f32,     // snap increment when Ctrl held (degrees)

    // -- Misc --
    pub duplicate_offset: f32,      // X offset for Shift+D
    pub through_cut_depth: f32,     // min half-height for C-key infinite cut
    pub falloff_hover_px: f32,      // falloff handle detection radius (pixels)
    pub light_azimuth_speed: f32,   // degrees per pixel
    pub light_elevation_speed: f32, // degrees per pixel
    #[serde(default = "default_instance_offset")]
    pub instance_offset: f32,       // X offset for instance creation
    #[serde(default = "default_toast_duration")]
    pub toast_duration: f32,        // seconds
    #[serde(default = "default_sdf_placement_bias")]
    pub sdf_placement_bias: f32,    // inflate committed shapes to match visual surface

    // -- Bevel limits (yellow: artifacts if too extreme) --
    #[serde(default = "default_bevel_max_radius")]
    pub bevel_max_radius: f32,      // maximum bevel radius clamp
    #[serde(default = "default_blend_k_max")]
    pub blend_k_max: f32,           // max blend-k value for booleans
    #[serde(default = "default_bevel_scroll_mult")]
    pub bevel_scroll_mult: f32,     // scroll-to-bevel multiplier

    // -- Raymarcher (red: affects stability/perf) --
    #[serde(default = "default_ray_step_min")]
    pub ray_step_min: f32,          // minimum step size in raymarcher
    #[serde(default = "default_ray_dist_mult")]
    pub ray_dist_mult: f32,         // cam_dist * this = actual max ray distance

    // -- Create tool --
    #[serde(default = "default_create_sink_depth")]
    pub create_sink_depth: f32,     // embed depth for boolean cutter placement
    #[serde(default = "default_create_alt_rot_speed")]
    pub create_alt_rot_speed: f32,  // degrees per pixel for Alt rotate in create tool
    #[serde(default = "default_create_min_extent")]
    pub create_min_extent: f32,     // minimum shape extent during drag-to-size

    // -- Visual (green: safe) --
    #[serde(default = "default_snap_indicator_px")]
    pub snap_indicator_px: f32,     // snap diamond indicator size (pixels)
    #[serde(default = "default_arrow_tip_px")]
    pub arrow_tip_px: f32,          // face normal arrow tip length (pixels)
    #[serde(default = "default_face_outline_width")]
    pub face_outline_width: f32,    // hovered face outline stroke width
    #[serde(default = "default_hud_font_size")]
    pub hud_font_size: f32,         // HUD text size in viewport

    // -- System (green/yellow) --
    #[serde(default = "default_repaint_ms")]
    pub repaint_ms: u32,            // minimum repaint interval (ms)
    #[serde(default = "default_server_timeout_ms")]
    pub server_timeout_ms: u32,     // AI texture server startup timeout
    #[serde(default = "default_floor_detect_threshold")]
    pub floor_detect_threshold: f32, // normal.y > this = horizontal surface
    #[serde(default = "default_scale_min_clamp")]
    pub scale_min_clamp: f32,       // minimum allowed scale component

    // -- UI Throttle (green) --
    #[serde(default = "default_throttle_panels")]
    pub throttle_panels_during_interaction: bool, // throttle outliner data prep during orbit/drag
    #[serde(default = "default_panel_refresh_interval_ms")]
    pub panel_refresh_interval_ms: u32, // min ms between outliner data rebuilds during interaction
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            grab_speed: 0.003,
            extrude_wall_cleanup: 2,
            extrude_debris_merge: 1e-4,
            extrude_use_chosen_material: false,
            extrude_chosen_material_id: 0,
            pipe_own_object: true,
            rotate_speed: 0.5,
            scale_speed: 0.005,

            cam_orbit_speed: 0.005,
            cam_pan_speed: 0.002,
            cam_zoom_speed: 0.002,
            cam_zoom_min: 0.5,
            cam_zoom_max: 200.0,

            face_drag_speed: 0.003,

            bevel_scroll_speed: 0.02,
            bevel_drag_speed: 0.002,
            bevel_hover_px: 8.0,
            blend_k_speed: 0.002,

            ray_max_steps: 128,
            ray_hit_threshold: 0.005,
            ray_normal_eps: 0.002,
            ray_max_dist: 50.0,

            default_cam_dist: 5.0,
            cam_pitch_clamp: 1.5,

            selection_circle_px: 16.0,
            depth_pick_drift_px: 20.0,
            pie_menu_radius: 20.0,

            mirror_rotate_speed: 0.015,
            mirror_snap_angle: 45.0,

            duplicate_offset: 0.3,
            through_cut_depth: 10.0,
            falloff_hover_px: 10.0,
            light_azimuth_speed: 0.5,
            light_elevation_speed: 0.3,
            instance_offset: 1.5,
            toast_duration: 2.5,
            sdf_placement_bias: 0.002,

            bevel_max_radius: 2.0,
            blend_k_max: 16.0,
            bevel_scroll_mult: 15.0,

            ray_step_min: 0.01,
            ray_dist_mult: 1.0,

            create_sink_depth: 0.02,
            create_alt_rot_speed: 0.5,
            create_min_extent: 0.001,

            snap_indicator_px: 7.0,
            arrow_tip_px: 22.0,
            face_outline_width: 1.5,
            hud_font_size: 12.0,

            repaint_ms: 16,
            server_timeout_ms: 1500,
            floor_detect_threshold: 0.7,
            scale_min_clamp: 0.001,

            throttle_panels_during_interaction: true,
            panel_refresh_interval_ms: 200, // 5Hz
        }
    }
}

// serde default functions for new fields (backward-compat with old tweaks.json)
fn default_ray_max_dist() -> f32 { 50.0 }

fn default_extrude_wall_cleanup() -> u8 {
    2
}
fn default_extrude_debris_merge() -> f32 { 1e-4 }
fn default_cam_dist() -> f32 { 5.0 }
fn default_cam_pitch_clamp() -> f32 { 1.5 }
fn default_selection_circle_px() -> f32 { 16.0 }
fn default_depth_pick_drift_px() -> f32 { 20.0 }
fn default_pie_menu_radius() -> f32 { 20.0 }
fn default_mirror_rotate_speed() -> f32 { 0.015 }
fn default_mirror_snap_angle() -> f32 { 45.0 }
fn default_instance_offset() -> f32 { 1.5 }
fn default_toast_duration() -> f32 { 2.5 }
fn default_sdf_placement_bias() -> f32 { 0.002 }
fn default_bevel_max_radius() -> f32 { 2.0 }
/// Default max soft-boolean k. Was 2.0 but UI sliders hard-capped at 1.0 for years —
/// too small for world-scale SDF (terrain / large booleans). 16 m is a usable product default;
/// Dev Tweaks can raise further (up to 64).
fn default_blend_k_max() -> f32 { 16.0 }
fn default_bevel_scroll_mult() -> f32 { 15.0 }
fn default_ray_step_min() -> f32 { 0.01 }
fn default_ray_dist_mult() -> f32 { 1.0 }
fn default_create_sink_depth() -> f32 { 0.02 }
fn default_create_alt_rot_speed() -> f32 { 0.5 }
fn default_create_min_extent() -> f32 { 0.001 }
fn default_snap_indicator_px() -> f32 { 7.0 }
fn default_arrow_tip_px() -> f32 { 22.0 }
fn default_face_outline_width() -> f32 { 1.5 }
fn default_hud_font_size() -> f32 { 12.0 }
fn default_repaint_ms() -> u32 { 16 }
fn default_server_timeout_ms() -> u32 { 1500 }
fn default_floor_detect_threshold() -> f32 { 0.7 }
fn default_scale_min_clamp() -> f32 { 0.001 }
fn default_throttle_panels() -> bool { true }
fn default_panel_refresh_interval_ms() -> u32 { 200 }

impl ToolSettings {
    /// Save current settings to ~/.azra/tweaks.json
    pub fn save(&self) {
        let Some(home) = dirs::home_dir() else { return };
        let dir = home.join(".azra");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tweaks.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
    /// Load saved settings from ~/.azra/tweaks.json, falling back to defaults
    pub fn load() -> Self {
        let Some(home) = dirs::home_dir() else { return Self::default() };
        let path = home.join(".azra").join("tweaks.json");
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}
