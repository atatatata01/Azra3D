//! Live · Sculpt Tool Parameters — Blender VIEW3D tool-header + popovers.
//!
//! The previous panel only bound Radius / Strength / Lazy. Owner: "I only have
//! strength and radius, where are all the other" — every control below is a
//! REAL `VoxelObject` / `SculptState` field (same state the stroke engine reads).
//!
//! Maps to Blender `properties_paint_common.py`:
//!   brush_shared_settings  → Direction, Size, Strength
//!   brush_settings         → Accumulate, Front Faces, Auto Smooth, Direction
//!   brush_texture_settings → Mapping, Angle, Contrast
//!   StrokePanel            → stroke_method, Spacing, Jitter, Stabilize, Rate

use eframe::egui;
use crate::ui_fields as f;
use crate::ui_theme as t;
use crate::{SculptStrokeMethod, TipOrientation};

impl crate::FourthLayerApp {
    pub(crate) fn draw_tool_params_sculpt(&mut self, ui: &mut egui::Ui) {
        use core_math::voxel_sculpt::VoxelBrush;
        ui.label(
            egui::RichText::new("Live · Sculpt")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );

        const BRUSHES: [(VoxelBrush, &str); 14] = [
            (VoxelBrush::Draw, "Draw"),
            (VoxelBrush::Erase, "Erase"),
            (VoxelBrush::Smooth, "Smooth"),
            (VoxelBrush::Flatten, "Flatten"),
            (VoxelBrush::Clay, "Clay"),
            (VoxelBrush::Scrape, "Scrape"),
            (VoxelBrush::Crease, "Crease"),
            (VoxelBrush::Fill, "Fill"),
            (VoxelBrush::ClayBuildup, "Build"),
            (VoxelBrush::Inflate, "Inflate"),
            (VoxelBrush::Pinch, "Pinch"),
            (VoxelBrush::Move, "Move"),
            (VoxelBrush::Smudge, "Smudge"),
            (VoxelBrush::SnakeHook, "Snake"),
        ];

        if let Some(v) = self.sculpt.voxel.as_mut() {
            f::section(ui, "live/Sculpt/brush", "Brush", None, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (b, name) in BRUSHES {
                        if ui.selectable_label(v.brush == b, name).clicked() {
                            v.set_active_brush(b);
                        }
                    }
                });
                let mut add = v.brush_add;
                if f::enum_seg(ui, "Direction", &mut add, &[("Add", true), ("Subtract", false)]) {
                    v.brush_add = add;
                }
                let mut rad = v.radius;
                let mut stren = v.strength;
                if f::num(ui, "Size", &mut rad, 0.005, 0.01..=2.0, None).changed() {
                    v.radius = rad;
                }
                if f::num(ui, "Strength", &mut stren, 0.01, 0.0..=1.0, None).changed() {
                    v.strength = stren;
                }
                let _ = f::bool_row(ui, "Pressure → Size", &mut v.pressure_to_radius);
                let _ = f::bool_row(ui, "Pressure → Strength", &mut v.pressure_to_strength);
                if v.pressure_to_radius || v.pressure_to_strength {
                    v.pressure_enabled = true;
                }
                let mut fo = v.falloff_preset.label().to_string();
                if f::enum_combo_id(
                    ui,
                    "live/Sculpt/falloff",
                    "Falloff",
                    &mut fo,
                    &[
                        "Smooth", "Smoother", "Sphere", "Root", "Sharp", "Linear", "InvSq",
                        "Constant", "Blend",
                    ],
                ) {
                    if let Some(p) = core_math::voxel_sculpt::FalloffPreset::ALL
                        .iter()
                        .copied()
                        .find(|p| p.label() == fo)
                    {
                        v.falloff_preset = p;
                    }
                }
                let mut hard = v.falloff;
                if f::num(ui, "Hardness", &mut hard, 0.01, 0.0..=1.0, None).changed() {
                    v.falloff = hard;
                }
                f::axis_triad(ui, "Mirror", &mut v.symmetry);
            });

            f::section(ui, "live/Sculpt/brush_settings", "Brush Settings", None, |ui| {
                let _ = f::bool_row(ui, "Per-brush settings", &mut v.per_brush_settings);
                let _ = f::bool_row(ui, "Sculpt on Surface", &mut v.sculpt_on_surface);
                let _ = f::bool_row(ui, "Accumulate", &mut v.accumulate);
                ui.add_enabled_ui(!v.sculpt_on_surface, |ui| {
                    let _ = f::bool_row(ui, "Front Faces Only", &mut v.front_faces_only);
                });
                let _ = f::bool_row(ui, "Auto Smooth", &mut v.auto_smooth);
                if v.auto_smooth {
                    let mut a = v.auto_smooth_intensity;
                    if f::num(ui, "Auto smooth", &mut a, 0.01, 0.05..=1.0, None).changed() {
                        v.auto_smooth_intensity = a;
                    }
                }
                let mut dir = v.brush_dir.label().to_string();
                const DIRS: &[&str] = &[
                    "Surface", "Normal", "View", "Brush", "Average", "Original", "Tangent", "X",
                    "Y", "Z",
                ];
                if f::enum_combo_id(ui, "live/Sculpt/brush_dir", "Normal", &mut dir, DIRS) {
                    if let Some(d) = core_math::voxel_sculpt::BrushDirection::ALL
                        .iter()
                        .copied()
                        .find(|d| d.label() == dir)
                    {
                        v.brush_dir = d;
                    }
                }
                let mut am = v.auto_mask.label().to_string();
                if f::enum_combo_id(
                    ui,
                    "live/Sculpt/automask",
                    "Auto Mask",
                    &mut am,
                    &["Off", "Curv", "Peak", "Cavity", "Flat"],
                ) {
                    if let Some(m) = core_math::voxel_sculpt::AutoMaskMode::ALL
                        .iter()
                        .copied()
                        .find(|m| m.label() == am)
                    {
                        v.auto_mask = m;
                    }
                }
                if v.auto_mask.is_on() {
                    let mut ami = v.auto_mask_intensity;
                    if f::num(ui, "AM str", &mut ami, 0.01, 0.05..=1.0, None).changed() {
                        v.auto_mask_intensity = ami;
                    }
                    let _ = f::bool_row(ui, "AM invert", &mut v.auto_mask_invert);
                }
                if ui.button("Reset factory").clicked() {
                    v.apply_brush_factory(v.brush);
                }
                if ui.button("Remesh…").clicked() {
                    v.remesh_open = true;
                    v.remesh_target_res = v.grid.res;
                }
            });

            f::section(ui, "live/Sculpt/texture", "Texture", None, |ui| {
                let tip = if v.brush_tip_name.is_empty() {
                    "(no tip — radial falloff)"
                } else {
                    v.brush_tip_name.as_str()
                };
                ui.label(
                    egui::RichText::new(tip)
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
                let mut map = TipOrientation::from_u32(v.tip_orient).label().to_string();
                if f::enum_combo_id(
                    ui,
                    "live/Sculpt/tex_map",
                    "Mapping",
                    &mut map,
                    &["Fixed", "Follow", "View", "Random"],
                ) {
                    if let Some(o) = TipOrientation::ALL.iter().copied().find(|o| o.label() == map) {
                        v.tip_orient = o.as_u32();
                        v.tip_follow_stroke = o == TipOrientation::FollowStroke;
                    }
                }
                let mut ang = v.tip_angle;
                if f::num(ui, "Angle", &mut ang, 1.0, 0.0..=360.0, Some("°")).changed() {
                    v.tip_angle = ang;
                }
                let mut con = v.tip_contrast;
                if f::num(ui, "Contrast", &mut con, 0.01, 0.25..=2.0, None).changed() {
                    v.tip_contrast = con;
                }
                let mut rr = v.dyn_random_rotation;
                if f::num(ui, "Random rot", &mut rr, 0.01, 0.0..=1.0, None).changed() {
                    v.dyn_random_rotation = rr;
                }
            });
        } else {
            // Poly object — same header knobs as voxel, bound to sculpt.poly_*.
            self.sculpt.poly_ensure_defaults();
            let mut brush = self.sculpt.poly_brush.unwrap_or(core_math::voxel_sculpt::VoxelBrush::Draw);
            f::section(ui, "live/Sculpt/brush", "Brush", None, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (b, name) in BRUSHES {
                        if ui.selectable_label(brush == b, name).clicked() {
                            brush = b;
                        }
                    }
                });
                draw_sculpt_brush_core(
                    ui,
                    &mut self.sculpt.poly_tool,
                    &mut self.sculpt.poly_radius,
                    &mut self.sculpt.poly_strength,
                    &mut self.sculpt.poly_hardness,
                    &mut self.sculpt.poly_symmetry,
                    &mut self.sculpt.poly_pressure_to_radius,
                    &mut self.sculpt.poly_pressure_to_strength,
                );
            });
            f::section(ui, "live/Sculpt/brush_settings", "Brush Settings", None, |ui| {
                draw_sculpt_brush_settings(ui, &mut self.sculpt.poly_tool);
            });
            f::section(ui, "live/Sculpt/texture", "Texture", None, |ui| {
                draw_sculpt_texture(ui, &mut self.sculpt.poly_tool, &self.sculpt.poly_tip_name);
            });
            self.sculpt.poly_brush = Some(brush);
        }

        // Stroke is tool-global (`SculptState`) — same fields as the viewport Stroke popover.
        f::section(ui, "live/Sculpt/stroke", "Stroke", None, |ui| {
            let mut method = self.sculpt.stroke_method.label().to_string();
            const METHODS: &[&str] = &[
                "Dots", "Drag Dot", "Space", "Airbrush", "Anchored", "Line", "Curve",
            ];
            if f::enum_combo_id(ui, "live/Sculpt/stroke_method", "Method", &mut method, METHODS) {
                if let Some(m) = SculptStrokeMethod::ALL
                    .iter()
                    .copied()
                    .find(|m| m.label() == method)
                {
                    self.sculpt.stroke_method = m;
                    if !m.implemented() {
                        self.toast(&format!("{} not wired yet — using Space", m.label()));
                    }
                }
            }
            let (sp, jit, lazy) = if let Some(v) = self.sculpt.voxel.as_mut() {
                (&mut v.stroke_spacing, &mut v.dyn_jitter, &mut v.lazy)
            } else {
                (
                    &mut self.sculpt.poly_tool.stroke_spacing,
                    &mut self.sculpt.poly_tool.dyn_jitter,
                    &mut self.sculpt.poly_tool.lazy,
                )
            };
            let _ = f::num(ui, "Spacing", sp, 0.005, 0.03..=0.40, None);
            let _ = f::num(ui, "Jitter", jit, 0.01, 0.0..=1.0, None);
            let _ = f::num(ui, "Stabilize", lazy, 0.01, 0.0..=1.0, None);
            if self.sculpt.stroke_method.effective() == SculptStrokeMethod::Airbrush {
                let mut rate = if self.sculpt.airbrush_rate > 1e-4 {
                    self.sculpt.airbrush_rate
                } else {
                    0.09
                };
                if f::num(ui, "Rate", &mut rate, 0.005, 0.016..=0.50, Some("s")).changed() {
                    self.sculpt.airbrush_rate = rate;
                }
            }
        });

        ui.label(
            egui::RichText::new("LMB stroke · Shift smooth · Ctrl invert")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
    }
}

fn draw_sculpt_brush_core(
    ui: &mut egui::Ui,
    tool: &mut crate::VoxelBrushSettings,
    radius: &mut f32,
    strength: &mut f32,
    hardness: &mut f32,
    symmetry: &mut [bool; 3],
    p2r: &mut bool,
    p2s: &mut bool,
) {
    let mut add = tool.brush_add;
    if f::enum_seg(ui, "Direction", &mut add, &[("Add", true), ("Subtract", false)]) {
        tool.brush_add = add;
    }
    let _ = f::num(ui, "Size", radius, 0.005, 0.01..=4.0, None);
    let _ = f::num(ui, "Strength", strength, 0.01, 0.0..=2.0, None);
    let _ = f::bool_row(ui, "Pressure → Size", p2r);
    let _ = f::bool_row(ui, "Pressure → Strength", p2s);
    let mut fo = tool.falloff_preset.label().to_string();
    if f::enum_combo_id(
        ui,
        "live/Sculpt/falloff",
        "Falloff",
        &mut fo,
        &[
            "Smooth", "Smoother", "Sphere", "Root", "Sharp", "Linear", "InvSq", "Constant",
            "Blend",
        ],
    ) {
        if let Some(p) = core_math::voxel_sculpt::FalloffPreset::ALL
            .iter()
            .copied()
            .find(|p| p.label() == fo)
        {
            tool.falloff_preset = p;
        }
    }
    let _ = f::num(ui, "Hardness", hardness, 0.01, 0.0..=1.0, None);
    tool.falloff = *hardness;
    f::axis_triad(ui, "Mirror", symmetry);
}

fn draw_sculpt_brush_settings(ui: &mut egui::Ui, tool: &mut crate::VoxelBrushSettings) {
    let _ = f::bool_row(ui, "Sculpt on Surface", &mut tool.sculpt_on_surface);
    let _ = f::bool_row(ui, "Accumulate", &mut tool.accumulate);
    ui.add_enabled_ui(!tool.sculpt_on_surface, |ui| {
        let _ = f::bool_row(ui, "Front Faces Only", &mut tool.front_faces_only);
    });
    let _ = f::bool_row(ui, "Auto Smooth", &mut tool.auto_smooth);
    if tool.auto_smooth {
        let _ = f::num(ui, "Auto smooth", &mut tool.auto_smooth_intensity, 0.01, 0.05..=1.0, None);
    }
    let mut dir = tool.brush_dir.label().to_string();
    const DIRS: &[&str] = &[
        "Surface", "Normal", "View", "Brush", "Average", "Original", "Tangent", "X", "Y", "Z",
    ];
    if f::enum_combo_id(ui, "live/Sculpt/brush_dir", "Normal", &mut dir, DIRS) {
        if let Some(d) = core_math::voxel_sculpt::BrushDirection::ALL
            .iter()
            .copied()
            .find(|d| d.label() == dir)
        {
            tool.brush_dir = d;
        }
    }
    let mut am = tool.auto_mask.label().to_string();
    if f::enum_combo_id(
        ui,
        "live/Sculpt/automask",
        "Auto Mask",
        &mut am,
        &["Off", "Curv", "Peak", "Cavity", "Flat"],
    ) {
        if let Some(m) = core_math::voxel_sculpt::AutoMaskMode::ALL
            .iter()
            .copied()
            .find(|m| m.label() == am)
        {
            tool.auto_mask = m;
        }
    }
    if tool.auto_mask.is_on() {
        let _ = f::num(ui, "AM str", &mut tool.auto_mask_intensity, 0.01, 0.05..=1.0, None);
        let _ = f::bool_row(ui, "AM invert", &mut tool.auto_mask_invert);
    }
}

fn draw_sculpt_texture(ui: &mut egui::Ui, tool: &mut crate::VoxelBrushSettings, tip_name: &str) {
    let tip = if tip_name.is_empty() {
        "(no tip — radial falloff)"
    } else {
        tip_name
    };
    ui.label(egui::RichText::new(tip).size(10.0).color(t::TEXT_FAINT));
    let mut map = TipOrientation::from_u32(tool.tip_orient).label().to_string();
    if f::enum_combo_id(
        ui,
        "live/Sculpt/tex_map",
        "Mapping",
        &mut map,
        &["Fixed", "Follow", "View", "Random"],
    ) {
        if let Some(o) = TipOrientation::ALL.iter().copied().find(|o| o.label() == map) {
            tool.tip_orient = o.as_u32();
            tool.tip_follow_stroke = o == TipOrientation::FollowStroke;
        }
    }
    let _ = f::num(ui, "Angle", &mut tool.tip_angle, 1.0, 0.0..=360.0, Some("°"));
    let _ = f::num(ui, "Contrast", &mut tool.tip_contrast, 0.01, 0.25..=2.0, None);
    let _ = f::num(ui, "Random rot", &mut tool.dyn_random_rotation, 0.01, 0.0..=1.0, None);
}
