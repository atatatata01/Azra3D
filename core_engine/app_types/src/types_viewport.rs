// Viewport State + Work Plane types — extracted from types.rs
use glam::{Quat, EulerRot, Vec3};
use serde::{Serialize, Deserialize};
use crate::*;

// ==================================================================
// Work Plane System
// ==================================================================
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum WorkPlaneMode {
    Auto,   // orientation follows camera angle, surface hits have priority
    Locked, // Home key placed on surface — fixed orientation, workplane has priority
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum WorkPlaneOrientation {
    AxisSnap,     // snap to nearest world axis plane (XZ/XY/YZ)
    CameraFacing, // always face camera (like grab behavior)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkPlaneSettings {
    pub spacing: f32,          // world units per cell (default 2.0)
    pub color: [f32; 3],       // RGB 0..1
    pub opacity: f32,          // 0..1
    pub orientation: WorkPlaneOrientation,
    pub origin: Vec3,          // adjustable origin
}

impl Default for WorkPlaneSettings {
    fn default() -> Self {
        Self {
            spacing: 2.0,
            color: [0.3, 0.7, 0.9],   // subtle cyan
            opacity: 0.25,
            orientation: WorkPlaneOrientation::AxisSnap,
            origin: Vec3::ZERO,
        }
    }
}

/// Pre-computed each frame — all tools read this instead of computing planes themselves
#[derive(Clone, Copy)]
pub struct ResolvedWorkPlane {
    pub origin: Vec3,
    pub normal: Vec3,
    pub tangent_u: Vec3,
    pub tangent_v: Vec3,
}

impl Default for ResolvedWorkPlane {
    fn default() -> Self {
        Self { origin: Vec3::ZERO, normal: Vec3::Y, tangent_u: Vec3::X, tangent_v: Vec3::Z }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedWorkPlane {
    pub name: String,
    pub origin: Vec3,
    pub normal: Vec3,
}

// ==================================================================
// Camera State (orbit camera — extracted from AzraApp)
// ==================================================================
#[derive(Clone, Copy)]
pub struct CameraState {
    pub orbit: Quat,
    pub distance: f32,
    pub target: Vec3,
    pub prev_billboard_yaw: f32,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            orbit: Quat::from_euler(EulerRot::YXZ, -0.5, 0.3, 0.0),
            distance: 8.0,
            target: Vec3::ZERO,
            prev_billboard_yaw: 0.0,
        }
    }
}

// ==================================================================
// Viewport State (Phase 2 refactor — extracted from AzraApp)
// ==================================================================
pub struct ViewportState {
    // Camera
    pub camera: CameraState,
    // Grid
    pub grid_visible: bool,
    pub grid_mode: u8,
    pub grid_color: [f32; 3],
    pub grid_axis_x_color: [f32; 3],
    pub grid_axis_z_color: [f32; 3],
    pub grid_cell_size: f32,
    pub grid_subdivisions: u32,
    pub grid_opacity: f32,
    pub grid_floor_y: f32,
    pub grid_show_axes: bool,
    pub show_grid_settings: bool,
    // Snap
    pub snap_enabled: bool,
    pub snap_grid: f32,
    pub snap_mode: SnapMode,
    pub snap_rotation_deg: f32,  // rotation snap increment (degrees); 0 = off
    pub snap_indicator: Option<SnapIndicator>,
    // Smart snap
    pub smart_snap_on: bool,
    pub smart_snap_vertex: bool,
    pub smart_snap_face_center: bool,
    pub smart_snap_edge_center: bool,
    pub smart_snap_edge: bool,
    pub smart_snap_center: bool,
    pub smart_snap_result: Option<crate::SmartSnapHit>,
    pub smart_snap_threshold: f32,
    pub smart_snap_grid: bool,
    pub smart_snap_visible_only: bool,
    // Work Plane
    pub wp_mode: WorkPlaneMode,
    pub wp_settings: WorkPlaneSettings,
    pub wp_resolved: ResolvedWorkPlane,
    pub wp_locked_normal: Vec3,
    pub wp_locked_origin: Vec3,
    pub wp_saved: Vec<SavedWorkPlane>,
    pub wp_show_settings: bool,
    // Xray
    pub xray_mode: bool,
    // Viewport overlay toggles (Blender-style)
    pub show_overlays: bool,         // master toggle
    pub show_sdf: bool,              // SDF rendering
    pub show_poly_overlays: bool,    // poly editing overlays
    pub show_workplane: bool,        // work plane overlay
    pub show_light_gizmos: bool,     // light gizmos
    /// Auto reflection probe debug view (owner 2026-08-07): X-ray wire box of the
    /// probe's proxy AABB + center cross, so the auto placement is inspectable.
    pub show_probe_gizmo: bool,
    pub show_selection_ui: bool,     // selection circles/highlights
    pub show_sel_rim: bool,          // selection outline on selected objects
    pub sel_rim_color: [f32; 3],    // outline color (RGB 0-1)
    pub sel_outline_width: f32,     // outline width in pixels (1+)
    pub show_gizmo: bool,            // transform gizmo
    pub show_reset_handle: bool,     // gizmo center reset handle (Rotate/Scale)
    pub show_tool_viewport_ui: bool, // floating tool settings windows
    /// Viewport overlay: draw authored UV seam edges (CustomData.seam) on poly meshes.
    /// Blender-style red. Independent of object wire / poly edit cage.
    pub show_uv_seams: bool,
    /// Viewport overlay: draw Mark Sharp edges (CustomData.sharp). Cyan.
    pub show_sharp_edges: bool,
    /// Viewport overlay: draw per-edge weights / creases (CustomData.crease 0..1).
    /// Magenta; opacity/width scales with weight so soft vs hard reads at a glance.
    pub show_edge_weights: bool,
    // Selection overlay look (egui painter circles — object/prim hover)
    pub sel_color: [f32; 3],         // selection highlight color (RGB 0-1)
    pub sel_opacity: f32,            // selection highlight opacity 0-1
    pub sel_radius: f32,             // selection circle radius (px)
    pub sel_thickness: f32,          // selection circle stroke width (px)
    // Poly edit sub-element selection (vert/edge/face highlights in edit mode)
    pub poly_sel_color: [f32; 3],       // RGB 0-1 — verts, edges, face outline
    pub poly_sel_opacity: f32,          // edge/vert opacity 0-1
    pub poly_sel_face_opacity: f32,     // face fill opacity 0-1
    pub poly_sel_edge_width: f32,       // selected edge stroke width (px)
    pub poly_sel_vert_size: f32,        // selected vertex point radius (px)
    /// When true, poly selection draws always-on-top (see through). When false, skip
    /// faces/edges whose normal points away from the camera (front-facing only).
    pub poly_sel_show_backface: bool,
    // Symmetry
    pub symmetry: SymmetryPlane,     // global symmetry plane
    // Viewport geometry tracking (moved from FourthLayerApp — Phase 1)
    pub last_rect_size: egui::Vec2,
    pub viewport_mouse: egui::Pos2,
    pub viewport_rect: egui::Rect,
    /// Alt+MMB view-snap already fired this middle-drag (edge-trigger, not per-frame spam).
    pub orbit_alt_snap_latched: bool,
    /// Temporary orthographic from Alt+MMB axis snap while the user is in **Perspective**.
    /// Free orbit that leaves the axis view restores perspective. Cleared when the user
    /// explicitly picks Orthographic/Perspective. Permanent Ortho mode never sets this.
    pub ortho_from_view_snap: bool,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            camera: CameraState::default(),
            last_rect_size: egui::vec2(0.0, 0.0),
            viewport_mouse: egui::pos2(0.0, 0.0),
            viewport_rect: egui::Rect::NOTHING,
            grid_visible: false, grid_mode: 0,
            grid_color: [0.35, 0.35, 0.38],
            grid_axis_x_color: [0.7, 0.2, 0.2],
            grid_axis_z_color: [0.2, 0.7, 0.2],
            grid_cell_size: 1.0, grid_subdivisions: 5, grid_opacity: 0.6,
            grid_floor_y: -1.5, grid_show_axes: true, show_grid_settings: false,
            snap_enabled: false, snap_grid: 0.25, snap_mode: SnapMode::Grid, snap_rotation_deg: 15.0, snap_indicator: None,
            smart_snap_on: false, smart_snap_vertex: true, smart_snap_face_center: true,
            smart_snap_edge_center: false, smart_snap_edge: false, smart_snap_center: false,
            smart_snap_result: None, smart_snap_threshold: 20.0,
            smart_snap_grid: true, smart_snap_visible_only: true,
            wp_mode: WorkPlaneMode::Auto,
            wp_settings: WorkPlaneSettings::default(),
            wp_resolved: ResolvedWorkPlane::default(),
            wp_locked_normal: Vec3::Y,
            wp_locked_origin: Vec3::ZERO,
            wp_saved: Vec::new(),
            wp_show_settings: false,
            xray_mode: false,
            show_overlays: true,
            show_sdf: true,
            show_poly_overlays: true,
            show_workplane: true,
            show_light_gizmos: true,
            show_probe_gizmo: false,
            // OFF by default (owner-directed 2026-07-09): selected-prim feedback is the GPU
            // prim outline; this legacy overlay keeps only the hover ring + node outlines.
            show_selection_ui: false,
            show_sel_rim: true,
            sel_rim_color: [1.0, 0.6, 0.1],
            sel_outline_width: 1.0,
            show_gizmo: true,
            show_reset_handle: true,
            show_tool_viewport_ui: true,
            // Off by default — only useful once seams/sharp/weights are authored.
            show_uv_seams: false,
            show_sharp_edges: false,
            show_edge_weights: false,
            sel_color: [1.0, 0.63, 0.24],  // orange
            sel_opacity: 1.0,
            sel_radius: 16.0,
            sel_thickness: 1.5,
            // Poly edit selection — app SDF green (ui_theme::SDF / mockup --sdf #39d98a).
            poly_sel_color: [0.224, 0.851, 0.541], // #39d98a
            poly_sel_opacity: 1.0,
            poly_sel_face_opacity: 0.16,
            poly_sel_edge_width: 3.0,
            // Small tight squares (owner 2026-08-06: occluded verts + large white boxes).
            poly_sel_vert_size: 3.0,
            // OFF by default — GPU face-ID pick hides verts/edges behind solid faces.
            // Toggle on only for intentional x-ray selection.
            poly_sel_show_backface: false,
            symmetry: SymmetryPlane::default(),
            orbit_alt_snap_latched: false,
            ortho_from_view_snap: false,
        }
    }
}
