// TODO: Split further — over 300 line limit.
//! SDF properties sub-panel — extracted from properties.rs.

use eframe::egui;
use core_math::primitives::{BooleanOp, BlendCurve, BlendProfile};
use crate::ui_theme as t;
use crate::*;

/// UI profile choices: Smooth + Chamfer only.
/// `Curve` / `Circular` remain in the enum for save-format compat but are not offered
/// (owner: curve feature removed — it never worked reliably).
pub(crate) const BLEND_PROFILE_CHOICES: [(BlendProfile, &str); 2] = [
    (BlendProfile::Polynomial, "Smooth"),
    (BlendProfile::Chamfer, "Chamfer"),
];

/// Display name for a blend profile (legacy Curve/Circular display as Smooth).
pub(crate) fn blend_profile_label(p: BlendProfile) -> &'static str {
    match p {
        BlendProfile::Polynomial
        | BlendProfile::Circular
        | BlendProfile::Curve => "Smooth",
        BlendProfile::Chamfer => "Chamfer",
    }
}

// ---------------------------------------------------------------------------
// Blend-curve presets (~/.azra/blend_curve_presets/*.json)
// ---------------------------------------------------------------------------

fn blend_curve_presets_dir() -> Option<std::path::PathBuf> {
    let dir = crate::KeyBindings::config_dir()?.join("blend_curve_presets");
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    Some(dir)
}

/// List user-saved blend-curve preset names (file stems).
pub(crate) fn list_blend_curve_presets() -> Vec<String> {
    let Some(dir) = blend_curve_presets_dir() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

pub(crate) fn save_blend_curve_preset(name: &str, curve: &BlendCurve) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Preset name is empty".into());
    }
    // Sanitize filename
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim().to_string();
    if safe.is_empty() {
        return Err("Invalid preset name".into());
    }
    let dir = blend_curve_presets_dir().ok_or_else(|| "No config dir".to_string())?;
    let path = dir.join(format!("{safe}.json"));
    let json = serde_json::to_string_pretty(curve).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

pub(crate) fn load_blend_curve_preset(name: &str) -> Result<BlendCurve, String> {
    let dir = blend_curve_presets_dir().ok_or_else(|| "No config dir".to_string())?;
    let path = dir.join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut curve: BlendCurve = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    curve.normalize();
    Ok(curve)
}

/// Map curve (x,y) in 0..1 to screen pos inside `rect` (y-up for curve values).
fn curve_to_screen(rect: egui::Rect, x: f32, y: f32) -> egui::Pos2 {
    egui::pos2(
        rect.left() + x.clamp(0.0, 1.0) * rect.width(),
        rect.bottom() - y.clamp(0.0, 1.0) * rect.height(),
    )
}

fn screen_to_curve(rect: egui::Rect, p: egui::Pos2) -> [f32; 2] {
    let x = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    let y = ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0);
    [x, y]
}

/// Interactive multi-point blend-profile curve editor.
/// - Drag points · right-click remove · double-click empty add · double-click handle soft/sharp
/// - Soft = smooth cubic (Photoshop) · Sharp = corner
/// - Reset / Save / Load presets under ~/.azra
/// Returns true if the curve or shape changed.
pub(crate) fn draw_blend_curve_editor(
    ui: &mut egui::Ui,
    profile: BlendProfile,
    curve: &mut BlendCurve,
    shape: &mut f32,
) -> bool {
    let mut changed = false;
    // Do NOT normalize every frame — re-sorting mid-drag jumps the grabbed index.

    let desired = egui::vec2(ui.available_width().min(320.0).max(180.0), 120.0);
    let (rect, resp) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(28));
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(70)), egui::StrokeKind::Middle,
    );
    for t in [0.25, 0.5, 0.75] {
        let x = rect.left() + t * rect.width();
        let y = rect.bottom() - t * rect.height();
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0, egui::Color32::from_gray(40)),
        );
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(40)),
        );
    }

    // Smooth evaluated polyline (shows soft cubic vs sharp corners)
    {
        let n_samp = 48usize;
        let mut poly = Vec::with_capacity(n_samp + 1);
        for i in 0..=n_samp {
            let t = i as f32 / n_samp as f32;
            let y = curve.eval(t);
            poly.push(curve_to_screen(rect, t, y));
        }
        if poly.len() >= 2 {
            painter.add(egui::Shape::line(
                poly,
                egui::Stroke::new(2.5, egui::Color32::from_rgb(80, 180, 255)),
            ));
        }
    }

    let screen_pts: Vec<egui::Pos2> = curve
        .points
        .iter()
        .map(|p| curve_to_screen(rect, p.x, p.y))
        .collect();

    let sel_id = ui.id().with("blend_curve_sel");
    let drag_id = ui.id().with("blend_curve_drag");
    let mut selected: Option<usize> = ui.ctx().data(|d| d.get_temp(sel_id)).unwrap_or(None);
    let mut dragging: Option<usize> = ui.ctx().data(|d| d.get_temp(drag_id)).unwrap_or(None);

    const HIT_R: f32 = 16.0;
    const GRAB_R: f32 = 22.0;

    let primary_down = ui.input(|i| i.pointer.primary_down());
    let pointer = if dragging.is_some() || (resp.dragged() && primary_down) {
        ui.input(|i| i.pointer.latest_pos())
            .or_else(|| resp.interact_pointer_pos())
    } else {
        resp.interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
    };

    let nearest = |pos: egui::Pos2| -> Option<(usize, f32)> {
        let mut best: Option<(usize, f32)> = None;
        for (i, sp) in screen_pts.iter().enumerate() {
            let d = sp.distance(pos);
            if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        best
    };

    // Right-click: remove nearest interior point
    if resp.secondary_clicked() {
        if let Some(p) = pointer {
            if let Some((i, d)) = nearest(p) {
                if d <= GRAB_R && i > 0 && i + 1 < curve.points.len() && curve.points.len() > 2 {
                    curve.remove_point(i);
                    selected = None;
                    dragging = None;
                    changed = true;
                }
            }
        }
    }

    // Left press / drag-start: grab nearest handle
    let want_grab = primary_down
        && dragging.is_none()
        && (resp.is_pointer_button_down_on() || resp.drag_started());
    if want_grab {
        if let Some(p) = pointer {
            if let Some((i, d)) = nearest(p) {
                if d <= GRAB_R {
                    selected = Some(i);
                    dragging = Some(i);
                }
            }
        }
    }

    // Move grabbed point (stable index — no normalize until release)
    if let Some(i) = dragging {
        if primary_down {
            if let Some(p) = pointer {
                if i < curve.points.len() {
                    let mut c = screen_to_curve(rect, p);
                    let n = curve.points.len();
                    if i == 0 {
                        c[0] = 0.0;
                    } else if i + 1 == n {
                        c[0] = 1.0;
                    } else {
                        let lo = curve.points[i - 1].x + 0.015;
                        let hi = curve.points[i + 1].x - 0.015;
                        c[0] = c[0].clamp(lo, hi.max(lo));
                    }
                    c[1] = c[1].clamp(0.0, 1.0);
                    let knot = &mut curve.points[i];
                    if (knot.x - c[0]).abs() > 1e-6 || (knot.y - c[1]).abs() > 1e-6 {
                        knot.x = c[0];
                        knot.y = c[1];
                        changed = true;
                    }
                }
            }
        }
    }

    if dragging.is_some() && (!primary_down || resp.drag_stopped()) {
        let keep_x = dragging.and_then(|i| curve.points.get(i).map(|p| p.x));
        curve.normalize();
        if let Some(x) = keep_x {
            selected = curve
                .points
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (a.x - x)
                        .abs()
                        .partial_cmp(&(b.x - x).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i);
        }
        dragging = None;
    }

    // Primary click (no drag): select
    if resp.clicked() && dragging.is_none() {
        if let Some(p) = pointer {
            selected = nearest(p)
                .filter(|(_, d)| *d <= HIT_R)
                .map(|(i, _)| i);
        }
    }

    // Double-click handle → toggle soft/sharp; empty → add soft point
    if resp.double_clicked() {
        if let Some(p) = pointer {
            if let Some((i, d)) = nearest(p) {
                if d <= HIT_R {
                    curve.toggle_soft(i);
                    selected = Some(i);
                    changed = true;
                } else if curve.points.len() < BlendCurve::MAX_POINTS {
                    let c = screen_to_curve(rect, p);
                    if c[0] > 0.02 && c[0] < 0.98 {
                        curve.add_point(c[0], c[1]); // soft by default
                        selected = curve
                            .points
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| *i > 0 && *i + 1 < curve.points.len())
                            .min_by(|(_, a), (_, b)| {
                                (a.x - c[0])
                                    .abs()
                                    .partial_cmp(&(b.x - c[0]).abs())
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(i, _)| i);
                        changed = true;
                    }
                }
            }
        }
    }

    let hover_idx = pointer.and_then(|p| {
        nearest(p)
            .filter(|(_, d)| *d <= GRAB_R)
            .map(|(i, _)| i)
    });

    // Handles: circle = soft, diamond = sharp (Photoshop-style)
    for (i, p) in curve.points.iter().enumerate() {
        let sp = curve_to_screen(rect, p.x, p.y);
        let is_active = dragging == Some(i) || selected == Some(i) || hover_idx == Some(i);
        let fill = if is_active {
            egui::Color32::from_rgb(255, 230, 90)
        } else if p.soft {
            egui::Color32::from_rgb(255, 200, 80)
        } else {
            egui::Color32::from_rgb(220, 140, 255) // sharp = purple diamond
        };
        let r = if is_active { 8.0 } else { 7.0 };
        if is_active {
            painter.circle_filled(
                sp,
                r + 4.0,
                egui::Color32::from_rgba_unmultiplied(255, 220, 80, 50),
            );
        }
        if p.soft {
            painter.circle_filled(sp, r, fill);
            painter.circle_stroke(sp, r, egui::Stroke::new(1.5, egui::Color32::WHITE));
        } else {
            // Diamond for sharp corner
            let d = r + 1.0;
            let diamond = vec![
                egui::pos2(sp.x, sp.y - d),
                egui::pos2(sp.x + d, sp.y),
                egui::pos2(sp.x, sp.y + d),
                egui::pos2(sp.x - d, sp.y),
            ];
            painter.add(egui::Shape::convex_polygon(
                diamond.clone(),
                fill,
                egui::Stroke::new(1.5, egui::Color32::WHITE),
            ));
        }
    }

    ui.ctx().data_mut(|d| {
        d.insert_temp(sel_id, selected);
        d.insert_temp(drag_id, dragging);
    });

    // Toolbar
    ui.horizontal(|ui| {
        if ui
            .small_button("Reset")
            .on_hover_text("Restore default curve for this profile type")
            .clicked()
        {
            *curve = BlendCurve::preset(profile);
            selected = None;
            ui.ctx().data_mut(|d| d.insert_temp(sel_id, selected));
            changed = true;
        }
        if ui
            .small_button("+ Point")
            .on_hover_text("Add a soft control point (or double-click empty)")
            .clicked()
        {
            if curve.points.len() < BlendCurve::MAX_POINTS {
                let mut best_i = 0usize;
                let mut best_len = 0.0f32;
                for (i, pair) in curve.points.windows(2).enumerate() {
                    let len = pair[1].x - pair[0].x;
                    if len > best_len {
                        best_len = len;
                        best_i = i;
                    }
                }
                if best_i + 1 < curve.points.len() {
                    let a = curve.points[best_i];
                    let b = curve.points[best_i + 1];
                    curve.add_point((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
                    selected = Some(best_i + 1);
                    ui.ctx().data_mut(|d| d.insert_temp(sel_id, selected));
                    changed = true;
                }
            }
        }
        let can_remove = selected
            .map(|i| i > 0 && i + 1 < curve.points.len() && curve.points.len() > 2)
            .unwrap_or(false);
        if ui
            .add_enabled(can_remove, egui::Button::new("− Point").small())
            .on_hover_text("Remove selected point (or right-click it)")
            .clicked()
        {
            if let Some(i) = selected {
                curve.remove_point(i);
                selected = None;
                ui.ctx().data_mut(|d| d.insert_temp(sel_id, selected));
                changed = true;
            }
        }
        // Soft / Sharp toggle for selected point
        if let Some(i) = selected {
            if i < curve.points.len() {
                let is_soft = curve.points[i].soft;
                let lbl = if is_soft { "● Soft" } else { "◆ Sharp" };
                if ui
                    .small_button(lbl)
                    .on_hover_text("Toggle soft (smooth) vs sharp (corner) — or double-click the handle")
                    .clicked()
                {
                    curve.toggle_soft(i);
                    changed = true;
                }
            }
        }
        ui.label(
            egui::RichText::new(format!("{} pts", curve.points.len()))
                .small()
                .weak(),
        );
    });

    // Keyboard: Delete removes selected interior point
    if resp.hovered()
        && can_remove_selected(selected, curve)
        && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
    {
        if let Some(i) = selected {
            curve.remove_point(i);
            selected = None;
            ui.ctx().data_mut(|d| d.insert_temp(sel_id, selected));
            changed = true;
        }
    }

    // Shape scale (legacy handle — multiplies blend K)
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Shape").small().weak());
        let r = ui.add(
            egui::DragValue::new(shape)
                .speed(0.02)
                .range(0.25..=4.0),
        );
        if r.changed() {
            changed = true;
        }
        ui.label(
            egui::RichText::new(match profile {
                BlendProfile::Chamfer => "flat bevel · drag points",
                _ => "soft round · drag points",
            })
            .small()
            .weak(),
        );
    });

    // Presets row
    ui.horizontal(|ui| {
        let name_id = ui.id().with("blend_curve_preset_name");
        let mut name: String = ui
            .ctx()
            .data(|d| d.get_temp::<String>(name_id))
            .unwrap_or_else(|| "MyCurve".into());
        ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(90.0)
                .hint_text("preset name"),
        );
        if ui
            .small_button("Save")
            .on_hover_text("Save curve to ~/.azra/blend_curve_presets/")
            .clicked()
        {
            match save_blend_curve_preset(&name, curve) {
                Ok(()) => {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(ui.id().with("blend_curve_status"), format!("Saved “{name}”"));
                    });
                }
                Err(e) => {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(ui.id().with("blend_curve_status"), format!("Save failed: {e}"));
                    });
                }
            }
        }
        ui.ctx().data_mut(|d| d.insert_temp(name_id, name));

        let presets = list_blend_curve_presets();
        egui::ComboBox::from_id_salt("blend_curve_load")
            .selected_text("Load…")
            .width(80.0)
            .show_ui(ui, |ui| {
                if presets.is_empty() {
                    ui.label(egui::RichText::new("(no saved presets)").weak().small());
                }
                for p in &presets {
                    if ui.selectable_label(false, p).clicked() {
                        if let Ok(loaded) = load_blend_curve_preset(p) {
                            *curve = loaded;
                            changed = true;
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(
                                    ui.id().with("blend_curve_status"),
                                    format!("Loaded “{p}”"),
                                );
                            });
                        }
                        ui.close_menu();
                    }
                }
            });
    });

    if let Some(status) = ui
        .ctx()
        .data(|d| d.get_temp::<String>(ui.id().with("blend_curve_status")))
    {
        ui.label(egui::RichText::new(status).small().weak());
    }

    ui.label(
        egui::RichText::new(
            "Drag · right-click remove · double-click handle = soft/sharp · empty = add · ● soft ◆ sharp",
        )
        .small()
        .weak(),
    );

    changed
}

fn can_remove_selected(selected: Option<usize>, curve: &BlendCurve) -> bool {
    selected
        .map(|i| i > 0 && i + 1 < curve.points.len() && curve.points.len() > 2)
        .unwrap_or(false)
}

/// Legacy single-handle curve (kept for any remaining call sites).
pub(crate) fn draw_blend_profile_curve(
    ui: &mut egui::Ui,
    profile: BlendProfile,
    shape: &mut f32,
) -> bool {
    let mut curve = BlendCurve::preset(profile);
    draw_blend_curve_editor(ui, profile, &mut curve, shape)
}

/// Shared mutable state passed between properties sub-panels.
pub(crate) struct PropsContext {
    pub changed: bool,
    pub desc: String,
    pub drag_started: bool,
    pub new_bool_op: Option<BooleanOp>,
    pub new_mat: Option<String>,
    pub idx: Option<usize>,
    /// Deferred: add a HalfSpace slice cutter (Subtract) to the active body (applied post-draw).
    pub add_slice: bool,
    /// Deferred: slice-duplicate the active body along the selected slice prim (applied post-draw).
    pub slice_duplicate: bool,
    /// Deferred: create a shared graph material seeded from the active prim's embedded material
    /// and assign it (the material combo's "+ New Material" entry; applied post-draw in its own
    /// transaction so the undo snapshot is genuinely pre-mutation).
    pub new_shared_mat_from_prim: bool,
    /// Deferred: select this material's graph node so its editor shows (the combo's jump button).
    pub jump_to_material: Option<core_math::primitives::MaterialId>,
}

impl AzraApp {
    pub(crate) fn draw_props_sdf(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, pctx: &mut PropsContext) {
        let in_obj = matches!(self.tool.work_mode, WorkMode::Object|WorkMode::Vertex|WorkMode::Edge|WorkMode::Face);
        if self.modeling_context == ModelingContext::Sdf && self.sel.usel.active_light().is_none() && in_obj {
            // Compact gizmo + transform-space row
            ui.horizontal(|ui| {
                for (icon, tip, mode) in [("↔","Move",GizmoMode::Translate),("⟳","Rotate",GizmoMode::Rotate),("⤢","Scale",GizmoMode::Scale)] {
                    let active = self.tool.gizmo_mode == mode;
                    if ui.add(egui::Button::new(icon).min_size(egui::vec2(28.0,22.0))
                        .fill(if active {egui::Color32::from_rgba_unmultiplied(60,130,200,40)} else {egui::Color32::TRANSPARENT}))
                        .on_hover_text(tip).clicked() { self.tool.gizmo_mode = mode; }
                }
                ui.separator();
                for (lbl, is_local) in [("🌐",false),("📐",true)] {
                    let active = matches!((&self.tool.transform_space, is_local), (TransformSpace::Global,false)|(TransformSpace::Local,true));
                    if ui.add(egui::Button::new(lbl).min_size(egui::vec2(28.0,22.0))
                        .fill(if active {egui::Color32::from_rgba_unmultiplied(60,130,200,40)} else {egui::Color32::TRANSPARENT}))
                        .on_hover_text(if is_local {"Local"} else {"Global"}).clicked() {
                        self.tool.transform_space = if is_local {TransformSpace::Local} else {TransformSpace::Global};
                    }
                }
            });
            ui.add_space(4.0);
            if let Some(idx) = pctx.idx {
                if idx < self.scene.primitives.len() {
                    // Instance notice + break / scatter
                    if self.scene.primitives[idx].instance_master_id.is_some() {
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgba_unmultiplied(120, 200, 255, 30))
                            .rounding(3.0).inner_margin(4.0)
                            .show(ui, |ui| {
                                ui.colored_label(
                                    egui::Color32::from_rgb(120, 200, 255),
                                    "◇ Instance — shape / material / boolean follow master",
                                );
                                if ui
                                    .button("Make Unique / Break Link")
                                    .on_hover_text(
                                        "Stop following master (SDF instance → independent prim).",
                                    )
                                    .clicked()
                                {
                                    self.make_instance_unique(idx);
                                }
                            });
                        ui.add_space(2.0);
                    } else {
                        // Scatter linked copies into every other body (command palette too).
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("Instance → All Bodies")
                                .on_hover_text(
                                    "Linked instance in every other top-level body. \
                                     Same shape & boolean; edit master to update all.",
                                )
                                .clicked()
                            {
                                self.instance_into_all_bodies();
                            }
                            if self.is_master(idx) {
                                ui.weak("(has linked copies)");
                            }
                        });
                    }
                    self.draw_props_sdf_transform(ui, pctx, idx);
                    self.draw_props_sdf_shape(ui, pctx, idx);
                    self.draw_props_sdf_material(ctx, ui, pctx, idx);
                    // Mirror is handled by properties.rs gated on pctx.changed
                    // Re-enable UI after instance-disabled sections
                    #[allow(deprecated)]
                    ui.set_enabled(true);
                }
            } else {
                // No active prim — show a short hint but don't block the rest of the panel
                ui.add_space(4.0);
                ui.weak(if self.sel.usel.is_empty(){"No object selected"}else{"Multiple objects selected"});
                ui.add_space(4.0);
            }
        } else if self.tool.work_mode == WorkMode::Sculpt {
            // Voxel sculpt controls live on the LEFT tool panel now.
            ui.colored_label(egui::Color32::from_rgb(120, 200, 255), "🧊 Voxel Sculpt");
            ui.weak("Base shape, brush, and undo/redo are in the left tool panel.");
        }
    }

    /// Body properties panel — shown when an SdfBody node is the active selection.
    pub(crate) fn draw_props_body(&mut self, ui: &mut egui::Ui) {
        use core_math::scene_graph::NodePayload;
        // Show body properties when a body node is active and no prim is selected
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let (body_name, body_op, blend_prof, flags, sign, center, cur_bevel) = match self.scene.scene_graph.get(nid) {
            Some(n) => match &n.payload {
                NodePayload::SdfBody { name, body_op, blend_profile, mirror_flags, mirror_sign, mirror_center, bevel_radius, .. } => {
                    (name.clone(), body_op.clone(), *blend_profile, *mirror_flags, *mirror_sign, *mirror_center, *bevel_radius)
                },
                _ => return,
            },
            None => return,
        };

        ui.colored_label(egui::Color32::from_rgb(200, 160, 70),
            egui::RichText::new(format!("\u{2B21} {}", body_name)).strong());
        ui.separator();

        // --- Material assignment (body-level: sets material_id on EVERY prim of this body) ---
        {
            use core_math::primitives::MaterialId;
            let prim_mids: Vec<MaterialId> = self.scene.scene_graph.body_primitives(nid)
                .map(|ps| ps.iter().map(|p| p.material_id).collect())
                .unwrap_or_default();
            let first_mid = prim_mids.first().copied().unwrap_or(MaterialId::NONE);
            let uniform = prim_mids.iter().all(|&m| m == first_mid);
            let linked = uniform && !first_mid.is_none()
                && self.scene.scene_graph.material_by_id(first_mid).is_some();
            let mut graph_mats: Vec<(MaterialId, String)> = self.scene.scene_graph
                .materials()
                .map(|(_, mid, name, _)| (mid, name.to_string()))
                .collect();
            graph_mats.sort_by_key(|(mid, _)| mid.0);
            let current_label = if !uniform {
                "Mixed (per-prim)".to_string()
            } else if first_mid.is_none() {
                "Embedded (per-prim)".to_string()
            } else {
                self.scene.scene_graph.material_by_id(first_mid)
                    .map(|(_, m)| if m.name.is_empty() { format!("Material {}", first_mid.0) } else { m.name.clone() })
                    .unwrap_or_else(|| format!("Missing ({})", first_mid.0))
            };
            ui.horizontal(|ui| {
                ui.label("Material:");
                egui::ComboBox::from_id_salt("body_mat_ref").width(140.0).selected_text(&current_label)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(uniform && first_mid.is_none(), "Embedded (per-prim)").clicked()
                            && !(uniform && first_mid.is_none())
                        {
                            self.assign_material_to_body_prims(nid, MaterialId::NONE);
                        }
                        for (gmid, gname) in &graph_mats {
                            let label = if gname.is_empty() { format!("Material {}", gmid.0) } else { gname.clone() };
                            let is_cur = uniform && first_mid == *gmid;
                            if ui.selectable_label(is_cur, &label).clicked() && !is_cur {
                                self.assign_material_to_body_prims(nid, *gmid);
                            }
                        }
                        ui.separator();
                        if ui.selectable_label(false, "\u{2795} New Material")
                            .on_hover_text("Create a shared material from this body's first prim and assign it to every prim")
                            .clicked()
                        {
                            let new_name = self.next_unique_name("Material");
                            let seed = self.scene.scene_graph.body_primitives(nid)
                                .and_then(|ps| ps.first().map(|p| p.material.clone()))
                                .unwrap_or_default();
                            // Create + assign in ONE transaction → one Ctrl+Z.
                            self.transaction("New Material", |s| {
                                let mid = s.scene.scene_graph.add_material(&new_name, seed);
                                if let Some(prims) = s.scene.scene_graph.body_primitives_mut(nid) {
                                    for p in prims.iter_mut() { p.material_id = mid; }
                                }
                                s.sync_flat_cache();
                                s.cache.prim_buf_stale = true;
                                s.mark_dirty(DirtyFlags::RENDER);
                                s.outliner.outliner_dirty = true;
                            });
                            self.toast(&format!("{} created and assigned", new_name));
                        }
                    });
                if linked {
                    if ui.small_button("\u{270F}")
                        .on_hover_text("Edit this material (opens its editor / selects it in the outliner)")
                        .clicked()
                    {
                        if let Some(mnid) = self.scene.scene_graph.material_node_id(first_mid) {
                            self.select_single_unified(SceneItemId::Node(mnid));
                            self.outliner_find_selected();
                        }
                    }
                }
            });
            ui.add_space(2.0);
        }

        // SDF → Voxel boolean stamp: when a voxel sculpt is the active editing target, this SDF body's
        // field can be stamped into its density grid (Display Invariant: the SDF enters as a boolean
        // density MODIFIER, never a second renderer). Sculpt-undo path, not the SDF transaction().
        if self.sculpt.voxel.is_some() {
            ui.horizontal(|ui| {
                if ui.button("Stamp into Active Sculpt")
                    .on_hover_text("Add this SDF body's solid into the active voxel sculpt (union)")
                    .clicked() {
                    self.stamp_sdf_into_active_sculpt(nid, true);
                }
                if ui.button("Carve")
                    .on_hover_text("Subtract this SDF body's solid from the active voxel sculpt")
                    .clicked() {
                    self.stamp_sdf_into_active_sculpt(nid, false);
                }
            });
            ui.separator();
        }

        // Boolean op editor — top-level bodies are always Union (disabled)
        let is_top_level = self.scene.scene_graph.get(nid)
            .and_then(|n| n.parent)
            .map(|p| p == self.scene.scene_graph.root())
            .unwrap_or(true);

        {
            let mut op_changed = false;

            // Op type index: 0=Union, 1=Subtract, 2=Intersect (hard or smooth), 3=Seam, 4=Extrude
            let mut op_idx: usize = if body_op.is_additive() { 0 }
                else if matches!(&body_op, BooleanOp::Intersect | BooleanOp::SmoothIntersect { .. }) { 2 }
                else if matches!(&body_op, BooleanOp::Seam { .. }) { 3 }
                else if matches!(&body_op, BooleanOp::Extrude { .. }) { 4 }
                else { 1 };
            let cur_k = body_op.blend_k();
            let mut k = cur_k;
            let mut prof = blend_prof;
            // Seam params (persisted from the current op when it already is a Seam;
            // fresh defaults come from the ONE shared source in ui_panels).
            let (mut s_width, mut s_depth, mut s_profile, mut s_raised) = match &body_op {
                BooleanOp::Seam { width, depth, profile, raised } => (*width, *depth, *profile, *raised),
                _ => (crate::ui_panels::SEAM_DEFAULT_WIDTH, crate::ui_panels::SEAM_DEFAULT_DEPTH,
                      crate::ui_panels::SEAM_DEFAULT_PROFILE, false),
            };
            // Extrude params (persisted when already an Extrude; shared fresh defaults).
            let (mut e_strength, mut e_width) = match &body_op {
                BooleanOp::Extrude { strength, width } => (*strength, *width),
                _ => (crate::ui_panels::EXTRUDE_DEFAULT_STRENGTH, crate::ui_panels::EXTRUDE_DEFAULT_WIDTH),
            };

            ui.add_enabled_ui(!is_top_level, |ui| {
                const OP_LABELS: [&str; 5] = ["Union", "Subtract", "Intersect", "Seam", "Extrude"];
                let combo_resp = egui::ComboBox::from_label("Boolean")
                    .selected_text(OP_LABELS[op_idx])
                    .show_ui(ui, |ui| {
                        for (i, label) in OP_LABELS.iter().enumerate() {
                            if ui.selectable_label(op_idx == i, *label).clicked() {
                                if op_idx != i {
                                    self.transaction_snapshot_only("Change body op");
                                    op_idx = i;
                                    op_changed = true;
                                }
                            }
                        }
                    });
                if is_top_level {
                    combo_resp.response.on_hover_text("Top-level bodies always Union");
                }

                if op_idx == 3 {
                    // ---- Seam parameters (sub-body feature line along the parent surface) ----
                    let r = ui.add(egui::Slider::new(&mut s_width, 0.005..=0.5).text("Width").logarithmic(true));
                    if r.drag_started() { self.begin_episode("Adjust seam width"); }
                    if r.changed() { op_changed = true; }
                    let r = ui.add(egui::Slider::new(&mut s_depth, 0.002..=0.5).text("Depth").logarithmic(true));
                    if r.drag_started() { self.begin_episode("Adjust seam depth"); }
                    if r.changed() { op_changed = true; }
                    let cur = match s_profile { 1 => "U (round)", 2 => "Square", _ => "V (chamfer)" };
                    egui::ComboBox::from_label("Seam Profile").selected_text(cur).show_ui(ui, |ui| {
                        for (pv, lbl) in [(0u8, "V (chamfer)"), (1u8, "U (round)"), (2u8, "Square")] {
                            if ui.selectable_label(s_profile == pv, lbl).clicked() && s_profile != pv {
                                self.transaction_snapshot_only("Change seam profile");
                                s_profile = pv; op_changed = true;
                            }
                        }
                    });
                    let mut r_flag = s_raised;
                    if ui.checkbox(&mut r_flag, "Raised (bead)").changed() {
                        self.transaction_snapshot_only("Toggle seam polarity");
                        s_raised = r_flag; op_changed = true;
                    }
                } else if op_idx == 4 {
                    // ---- Extrude parameters (sub-body footprint raise/recess) ----
                    let smax = crate::ui_panels::EXTRUDE_STRENGTH_MAX;
                    let r = ui.add(egui::Slider::new(&mut e_strength, -smax..=smax).text("Strength"))
                        .on_hover_text("+ raises the parent surface within this sub-body's footprint, \u{2212} recesses it");
                    if r.drag_started() { self.begin_episode("Adjust extrude strength"); }
                    if r.changed() { op_changed = true; }
                    let r = ui.add(egui::Slider::new(&mut e_width, 0.005..=0.5).text("Width").logarithmic(true));
                    if r.drag_started() { self.begin_episode("Adjust extrude width"); }
                    if r.changed() { op_changed = true; }
                } else {
                    // K slider — smooth blend amount (Union, Subtract, and smooth Intersect)
                    {
                        let k_max = self.tweak.blend_k_max.max(0.1);
                        let k_resp = ui.add(
                            egui::Slider::new(&mut k, 0.0..=k_max)
                                .text("Blend K")
                                .step_by(0.005)
                                .logarithmic(k_max > 2.0),
                        )
                        .on_hover_text(format!(
                            "Soft boolean radius. Max = {k_max:.1} (Dev Tweaks → Max blend-k)."
                        ));
                        if k_resp.drag_started() {
                            self.begin_episode("Adjust blend K");
                        }
                        if k_resp.changed() {
                            k = k.clamp(0.0, k_max);
                            op_changed = true;
                        }
                    }

                    // Blend profile — only meaningful when K > 0
                    if k > 0.0 {
                        let prof_resp = egui::ComboBox::from_label("Blend Profile")
                            .selected_text(blend_profile_label(prof))
                            .show_ui(ui, |ui| {
                                for (p, label) in BLEND_PROFILE_CHOICES {
                                    if ui.selectable_label(prof == p, label).clicked() {
                                        if prof != p {
                                            self.transaction_snapshot_only("Change blend profile");
                                            prof = p;
                                            op_changed = true;
                                        }
                                    }
                                }
                            });
                        let _ = prof_resp;
                    }
                }
            });

            // Write back if anything changed
            if op_changed && !is_top_level {
                let new_op = match op_idx {
                    0 => BooleanOp::Union { k },
                    1 => BooleanOp::Subtract { k },
                    3 => BooleanOp::Seam { width: s_width, depth: s_depth, profile: s_profile, raised: s_raised },
                    4 => BooleanOp::Extrude { strength: e_strength, width: e_width },
                    _ => if k > 0.0 { BooleanOp::SmoothIntersect { k } } else { BooleanOp::Intersect },
                };
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::SdfBody { body_op, blend_profile, .. } = &mut node.payload {
                        *body_op = new_op;
                        *blend_profile = prof;
                    }
                }
                self.mark_dirty(DirtyFlags::SCENE);
                self.mark_node_body_vol_dirty(nid);
            }
        }
        ui.add_space(4.0);

        // P2: Field Modifiers (volume bake) — replaces standalone Body Mirror / Edge Bevel UI.
        // Hardwired mirror_*/bevel_radius stay as bake inputs; stack is authoring source after seed.
        let _ = (flags, sign, center, cur_bevel); // still read above for seed context if needed
        self.seed_and_sync_body_field_stack(nid);
        self.draw_props_body_field_modifiers(ui, nid);

        // Post-DC mesh modifier stack (function UI — look later). Path B: export/materialize fold.
        self.draw_props_body_mesh_modifiers(ui, nid);
    }

    /// True when `nid` is an SdfBody nested under another body/group (not a top-level root child).
    /// Nested bodies combine into the parent via `body_op` + blend K — same role as prim boolean.
    pub(crate) fn is_nested_sdf_body(&self, nid: core_math::scene_graph::NodeId) -> bool {
        use core_math::scene_graph::NodePayload;
        let Some(n) = self.scene.scene_graph.get(nid) else {
            return false;
        };
        if !matches!(n.payload, NodePayload::SdfBody { .. }) {
            return false;
        }
        let parent = n.parent.unwrap_or_else(|| self.scene.scene_graph.root());
        parent != self.scene.scene_graph.root()
    }

    /// Active nested SdfBody from selection (node selected, not a prim).
    pub(crate) fn active_nested_sdf_body(&self) -> Option<core_math::scene_graph::NodeId> {
        if self.active_prim_index_unified().is_some() {
            return None;
        }
        let nid = self.sel.usel.active_node()?;
        if self.is_nested_sdf_body(nid) {
            Some(nid)
        } else {
            None
        }
    }

    /// Boolean op + Blend K + profile for a **nested** SdfBody (sub-body).
    /// Same controls as prim Boolean / legacy `draw_props_body` op editor.
    /// No-ops (returns false) for top-level bodies (always Union into the scene).
    ///
    /// Used by:
    /// - Object properties (ui2) when a sub-body is selected
    /// - Tool Parameters panel (idle + during G/R/S)
    pub(crate) fn draw_subbody_boolean_params(
        &mut self,
        ui: &mut egui::Ui,
        nid: core_math::scene_graph::NodeId,
    ) -> bool {
        use core_math::scene_graph::NodePayload;
        if !self.is_nested_sdf_body(nid) {
            return false;
        }
        let (body_op, blend_prof, body_name) = match self.scene.scene_graph.get(nid) {
            Some(n) => match &n.payload {
                NodePayload::SdfBody {
                    name,
                    body_op,
                    blend_profile,
                    ..
                } => (body_op.clone(), *blend_profile, name.clone()),
                _ => return false,
            },
            None => return false,
        };

        ui.label(
            egui::RichText::new(format!("Sub-body · {body_name}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        ui.label(
            egui::RichText::new("How this body combines into its parent (like prim boolean)")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
        ui.add_space(2.0);

        let mut op_changed = false;
        let mut op_idx: usize = if body_op.is_additive() {
            0
        } else if matches!(
            &body_op,
            BooleanOp::Intersect | BooleanOp::SmoothIntersect { .. }
        ) {
            2
        } else if matches!(&body_op, BooleanOp::Seam { .. }) {
            3
        } else if matches!(&body_op, BooleanOp::Extrude { .. }) {
            4
        } else {
            1
        };
        let mut k = body_op.blend_k();
        let mut prof = blend_prof;
        let (mut s_width, mut s_depth, mut s_profile, mut s_raised) = match &body_op {
            BooleanOp::Seam {
                width,
                depth,
                profile,
                raised,
            } => (*width, *depth, *profile, *raised),
            _ => (
                crate::ui_panels::SEAM_DEFAULT_WIDTH,
                crate::ui_panels::SEAM_DEFAULT_DEPTH,
                crate::ui_panels::SEAM_DEFAULT_PROFILE,
                false,
            ),
        };
        let (mut e_strength, mut e_width) = match &body_op {
            BooleanOp::Extrude { strength, width } => (*strength, *width),
            _ => (
                crate::ui_panels::EXTRUDE_DEFAULT_STRENGTH,
                crate::ui_panels::EXTRUDE_DEFAULT_WIDTH,
            ),
        };

        const OP_LABELS: [&str; 5] = ["Union", "Subtract", "Intersect", "Seam", "Extrude"];
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Boolean").size(11.0).color(t::TEXT_DIM));
            egui::ComboBox::from_id_salt(("subbody_bool", nid.0))
                .selected_text(OP_LABELS[op_idx])
                .width(120.0)
                .show_ui(ui, |ui| {
                    for (i, label) in OP_LABELS.iter().enumerate() {
                        if ui.selectable_label(op_idx == i, *label).clicked() && op_idx != i {
                            self.transaction_snapshot_only("Change sub-body op");
                            op_idx = i;
                            op_changed = true;
                        }
                    }
                });
        });

        if op_idx == 3 {
            let r = ui.add(
                egui::Slider::new(&mut s_width, 0.005..=0.5)
                    .text("Width")
                    .logarithmic(true),
            );
            if r.drag_started() {
                self.begin_episode("Adjust seam width");
            }
            if r.changed() {
                op_changed = true;
            }
            let r = ui.add(
                egui::Slider::new(&mut s_depth, 0.002..=0.5)
                    .text("Depth")
                    .logarithmic(true),
            );
            if r.drag_started() {
                self.begin_episode("Adjust seam depth");
            }
            if r.changed() {
                op_changed = true;
            }
            let cur = match s_profile {
                1 => "U (round)",
                2 => "Square",
                _ => "V (chamfer)",
            };
            egui::ComboBox::from_id_salt(("subbody_seam_prof", nid.0))
                .selected_text(cur)
                .show_ui(ui, |ui| {
                    for (pv, lbl) in [(0u8, "V (chamfer)"), (1u8, "U (round)"), (2u8, "Square")] {
                        if ui.selectable_label(s_profile == pv, lbl).clicked() && s_profile != pv {
                            self.transaction_snapshot_only("Change seam profile");
                            s_profile = pv;
                            op_changed = true;
                        }
                    }
                });
            let mut r_flag = s_raised;
            if ui.checkbox(&mut r_flag, "Raised (bead)").changed() {
                self.transaction_snapshot_only("Toggle seam polarity");
                s_raised = r_flag;
                op_changed = true;
            }
        } else if op_idx == 4 {
            let smax = crate::ui_panels::EXTRUDE_STRENGTH_MAX;
            let r = ui
                .add(egui::Slider::new(&mut e_strength, -smax..=smax).text("Strength"))
                .on_hover_text(
                    "+ raises the parent surface within this sub-body's footprint, − recesses it",
                );
            if r.drag_started() {
                self.begin_episode("Adjust extrude strength");
            }
            if r.changed() {
                op_changed = true;
            }
            let r = ui.add(
                egui::Slider::new(&mut e_width, 0.005..=0.5)
                    .text("Width")
                    .logarithmic(true),
            );
            if r.drag_started() {
                self.begin_episode("Adjust extrude width");
            }
            if r.changed() {
                op_changed = true;
            }
        } else {
            let k_max = self.tweak.blend_k_max.max(0.1);
            let k_resp = ui
                .add(
                    egui::Slider::new(&mut k, 0.0..=k_max)
                        .text("Blend K")
                        .step_by(0.005)
                        .logarithmic(k_max > 2.0),
                )
                .on_hover_text(format!(
                    "Soft boolean radius for this sub-body into its parent. Max = {k_max:.1}."
                ));
            if k_resp.drag_started() {
                self.begin_episode("Adjust sub-body Blend K");
            }
            if k_resp.changed() {
                k = k.clamp(0.0, k_max);
                op_changed = true;
            }
            if k > 0.0 {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Blend Profile")
                            .size(10.0)
                            .color(t::TEXT_DIM),
                    );
                    let cur_ui = match prof {
                        BlendProfile::Chamfer => BlendProfile::Chamfer,
                        _ => BlendProfile::Polynomial,
                    };
                    for (p, lbl) in BLEND_PROFILE_CHOICES {
                        if ui.selectable_label(cur_ui == p, lbl).clicked() && prof != p {
                            self.transaction_snapshot_only("Change sub-body blend profile");
                            prof = p;
                            op_changed = true;
                        }
                    }
                });
            }
        }

        if op_changed {
            let new_op = match op_idx {
                0 => BooleanOp::Union { k },
                1 => BooleanOp::Subtract { k },
                3 => BooleanOp::Seam {
                    width: s_width,
                    depth: s_depth,
                    profile: s_profile,
                    raised: s_raised,
                },
                4 => BooleanOp::Extrude {
                    strength: e_strength,
                    width: e_width,
                },
                _ => {
                    if k > 0.0 {
                        BooleanOp::SmoothIntersect { k }
                    } else {
                        BooleanOp::Intersect
                    }
                }
            };
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::SdfBody {
                    body_op,
                    blend_profile,
                    ..
                } = &mut node.payload
                {
                    *body_op = new_op;
                    *blend_profile = prof;
                }
            }
            self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            self.mark_node_body_vol_dirty(nid);
        }
        true
    }

    /// Seed empty field stack from hardwired, then write hardwired from stack (bake/GPU parity).
    pub(crate) fn seed_and_sync_body_field_stack(&mut self, nid: core_math::scene_graph::NodeId) {
        use core_math::body_field_ops::{
            hardwired_is_active, resolve_hardwired, seed_from_hardwired, BodyFieldHardwired,
        };
        use core_math::scene_graph::NodePayload;

        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::SdfBody {
                field_modifiers,
                mirror_flags,
                mirror_sign,
                mirror_center,
                bevel_radius,
                ..
            } = &mut node.payload
            {
                let hw = BodyFieldHardwired {
                    mirror_flags: *mirror_flags,
                    mirror_sign: *mirror_sign,
                    mirror_center: *mirror_center,
                    bevel_radius: *bevel_radius,
                };
                if field_modifiers.is_empty() && hardwired_is_active(hw) {
                    *field_modifiers = seed_from_hardwired(hw);
                }
                let resolved = resolve_hardwired(field_modifiers);
                *mirror_flags = resolved.mirror_flags;
                *mirror_sign = resolved.mirror_sign;
                *mirror_center = resolved.mirror_center;
                *bevel_radius = resolved.bevel_radius;
            }
        }
    }

    /// Apply field stack → hardwired only (no seed). Call after stack edits.
    pub(crate) fn sync_body_field_stack_to_hardwired(&mut self, nid: core_math::scene_graph::NodeId) {
        use core_math::body_field_ops::resolve_hardwired;
        use core_math::scene_graph::NodePayload;

        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::SdfBody {
                field_modifiers,
                mirror_flags,
                mirror_sign,
                mirror_center,
                bevel_radius,
                ..
            } = &mut node.payload
            {
                let resolved = resolve_hardwired(field_modifiers);
                *mirror_flags = resolved.mirror_flags;
                *mirror_sign = resolved.mirror_sign;
                *mirror_center = resolved.mirror_center;
                *bevel_radius = resolved.bevel_radius;
            }
        }
    }

    /// Body Field Modifiers (volume / bake). Edits dirty the body volume.
    /// Also drawn from the Gate-5 Properties → Modifiers tab (OBJECT LEVEL).
    pub(crate) fn draw_props_body_field_modifiers(&mut self, ui: &mut egui::Ui, nid: core_math::scene_graph::NodeId) {
        use core_math::body_field_ops::{BodyFieldModKind, BodyFieldModRecord};
        use core_math::scene_graph::NodePayload;

        let mut mods: Vec<BodyFieldModRecord> = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                NodePayload::SdfBody { field_modifiers, .. } => Some(field_modifiers.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let mut changed = false;
        let mut episode_drag = false;
        // Center-plane sources (work planes / selection / origins), snapshotted BEFORE the
        // closure — it mutates `mods` and cannot also borrow `self`.
        let center_sources = self.mirror_center_sources(nid);
        let mut pending_toast: Option<String> = None;
        egui::CollapsingHeader::new("Field Modifiers (volume)")
            .default_open(!mods.is_empty())
            .show(ui, |ui| {
                ui.weak("Affects bake / volume. Volume Mirror = field-level (not mesh-only).");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("add_body_field_mod")
                        .selected_text("+ Add Field Modifier")
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            if ui.button("Volume Mirror").clicked() {
                                let mut m = BodyFieldModRecord::new_volume_mirror();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                            if ui.button("Body Bevel").clicked() {
                                let mut m = BodyFieldModRecord::new_body_bevel();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                            if ui.button("Body Smooth").clicked() {
                                let mut m = BodyFieldModRecord::new_body_smooth();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                            if ui.button("Displacement").clicked() {
                                let mut m = BodyFieldModRecord::new_body_displacement();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                        });
                });

                let n = mods.len();
                let mut remove: Option<usize> = None;
                let mut swap: Option<(usize, usize)> = None;
                for i in 0..n {
                    let label = mods[i].label().to_string();
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(100, 180, 255, 20))
                        .inner_margin(4.0)
                        .rounding(4.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut mods[i].enabled, "").changed() {
                                    changed = true;
                                }
                                ui.strong(format!("{}. {}", i + 1, label));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("X").clicked() {
                                        remove = Some(i);
                                    }
                                    if i + 1 < n && ui.small_button("v").clicked() {
                                        swap = Some((i, i + 1));
                                    }
                                    if i > 0 && ui.small_button("^").clicked() {
                                        swap = Some((i, i - 1));
                                    }
                                });
                            });
                            match &mut mods[i].kind {
                                BodyFieldModKind::VolumeMirror {
                                    axis_bits,
                                    sign_bits,
                                    center,
                                } => {
                                    ui.horizontal(|ui| {
                                        let mut mx = (*axis_bits & 1) != 0;
                                        let mut my = (*axis_bits & 2) != 0;
                                        let mut mz = (*axis_bits & 4) != 0;
                                        if ui.checkbox(&mut mx, "X").changed() {
                                            *axis_bits = (*axis_bits & !1) | (mx as u8);
                                            changed = true;
                                        }
                                        if ui.checkbox(&mut my, "Y").changed() {
                                            *axis_bits = (*axis_bits & !2) | ((my as u8) << 1);
                                            changed = true;
                                        }
                                        if ui.checkbox(&mut mz, "Z").changed() {
                                            *axis_bits = (*axis_bits & !4) | ((mz as u8) << 2);
                                            changed = true;
                                        }
                                    });
                                    // Cut = one-sided fold: the non-source half is deleted and
                                    // the body is evaluated ONCE per voxel instead of 2^axes
                                    // times. Off = the legacy true union (both sides kept).
                                    // Flip only means something under Cut — it picks which
                                    // half is the source — so it is disabled with it.
                                    let mut cut = (*sign_bits
                                        & core_math::body_field_ops::MIRROR_CUT_BIT)
                                        != 0;
                                    if ui
                                        .checkbox(&mut cut, "Cut (delete other half)")
                                        .on_hover_text(
                                            "On: keeps the source half, deletes the other and \
                                             mirrors it — one field eval per voxel.\n\
                                             Off: true union, both halves survive — 2^axes evals \
                                             per voxel (slower bake).",
                                        )
                                        .changed()
                                    {
                                        *sign_bits = (*sign_bits
                                            & !core_math::body_field_ops::MIRROR_CUT_BIT)
                                            | if cut {
                                                core_math::body_field_ops::MIRROR_CUT_BIT
                                            } else {
                                                0
                                            };
                                        changed = true;
                                    }
                                    ui.horizontal(|ui| {
                                        #[allow(deprecated)]
                                        ui.set_enabled(cut);
                                        ui.label("Keep:")
                                            .on_hover_text("Which half is the source geometry.");
                                        let mut fx = (*sign_bits & 1) != 0;
                                        let mut fy = (*sign_bits & 2) != 0;
                                        let mut fz = (*sign_bits & 4) != 0;
                                        if ui.checkbox(&mut fx, "−X").changed() {
                                            *sign_bits = (*sign_bits & !1) | (fx as u8);
                                            changed = true;
                                        }
                                        if ui.checkbox(&mut fy, "−Y").changed() {
                                            *sign_bits = (*sign_bits & !2) | ((fy as u8) << 1);
                                            changed = true;
                                        }
                                        if ui.checkbox(&mut fz, "−Z").changed() {
                                            *sign_bits = (*sign_bits & !4) | ((fz as u8) << 2);
                                            changed = true;
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Center:");
                                        for (c, p) in [
                                            (&mut center.x, "X:"),
                                            (&mut center.y, "Y:"),
                                            (&mut center.z, "Z:"),
                                        ] {
                                            let r = ui.add(egui::DragValue::new(c).prefix(p).speed(0.02));
                                            if r.drag_started() {
                                                episode_drag = true;
                                            }
                                            if r.changed() {
                                                changed = true;
                                            }
                                        }
                                    });
                                    // Pick the center line from a work plane, the selection, or an
                                    // origin instead of typing three numbers. Values are already in
                                    // this body's local frame (`mirror_center_sources` converts).
                                    egui::ComboBox::from_id_salt(("mirror_center_from", nid.0, i))
                                        .selected_text("Center from…")
                                        .width(180.0)
                                        .show_ui(ui, |ui| {
                                            for src in &center_sources {
                                                let hint = match (src.axis_bit, src.tilted) {
                                                    (Some(_), _) => "sets center + axis",
                                                    (None, true) => "center only — plane is tilted",
                                                    (None, false) => "sets center",
                                                };
                                                if ui
                                                    .button(&src.label)
                                                    .on_hover_text(hint)
                                                    .clicked()
                                                {
                                                    *center = src.center;
                                                    if let Some(bit) = src.axis_bit {
                                                        *axis_bits = bit;
                                                    }
                                                    if src.tilted {
                                                        // The volume mirror reflects PER AXIS, so a
                                                        // tilted plane cannot be its mirror plane.
                                                        // Say so rather than snapping to the nearest
                                                        // axis and mirroring about a plane the user
                                                        // never chose.
                                                        pending_toast = Some(format!(
                                                            "Center set from “{}”. Its normal is not \
                                                             an X/Y/Z axis of this body, so the mirror \
                                                             axis was left unchanged.",
                                                            src.label
                                                        ));
                                                    }
                                                    changed = true;
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                }
                                BodyFieldModKind::BodyBevel { radius } => {
                                    let r = ui.add(
                                        egui::Slider::new(radius, 0.0..=0.2)
                                            .text("Radius")
                                            .step_by(0.001),
                                    );
                                    if r.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r.changed() {
                                        changed = true;
                                    }
                                }
                                BodyFieldModKind::BodySmooth {
                                    mode,
                                    strength,
                                    radius,
                                } => {
                                    use core_math::body_field_ops::BodySmoothMode;
                                    ui.horizontal(|ui| {
                                        ui.label("Mode:");
                                        egui::ComboBox::from_id_salt(format!("body_smooth_mode_{i}"))
                                            .selected_text(mode.label())
                                            .width(110.0)
                                            .show_ui(ui, |ui| {
                                                for m in BodySmoothMode::all() {
                                                    if ui
                                                        .selectable_label(*mode == *m, m.label())
                                                        .clicked()
                                                    {
                                                        *mode = *m;
                                                        changed = true;
                                                    }
                                                }
                                            });
                                    });
                                    let r_s = ui.add(
                                        egui::Slider::new(strength, 0.0..=0.15)
                                            .text("Strength")
                                            .step_by(0.001),
                                    );
                                    if r_s.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r_s.changed() {
                                        changed = true;
                                    }
                                    let r_r = ui.add(
                                        egui::Slider::new(radius, 0.005..=0.25)
                                            .text("Feature Radius")
                                            .step_by(0.001),
                                    );
                                    if r_r.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r_r.changed() {
                                        changed = true;
                                    }
                                    ui.weak("Field smooth (CPU bake). Peak/Convex/Concave/Curvature/Occlusion.");
                                }
                                BodyFieldModKind::BodyDisplacement {
                                    strength,
                                    frequency,
                                    seed,
                                } => {
                                    let r_s = ui.add(
                                        egui::Slider::new(strength, -0.1..=0.1)
                                            .text("Strength")
                                            .step_by(0.001),
                                    );
                                    if r_s.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r_s.changed() {
                                        changed = true;
                                    }
                                    let r_f = ui.add(
                                        egui::Slider::new(frequency, 0.5..=64.0)
                                            .text("Frequency")
                                            .logarithmic(true)
                                            .step_by(0.1),
                                    );
                                    if r_f.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r_f.changed() {
                                        changed = true;
                                    }
                                    let r_seed = ui.add(
                                        egui::DragValue::new(seed).prefix("Seed: ").speed(1.0),
                                    );
                                    if r_seed.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r_seed.changed() {
                                        changed = true;
                                    }
                                    ui.weak(
                                        "Procedural surface displacement on the volume (CPU bake).\n\
                                         +Strength expands solid where noise is high.",
                                    );
                                }
                            }
                        });
                }
                if let Some(i) = remove {
                    mods.remove(i);
                    changed = true;
                }
                if let Some((a, b)) = swap {
                    mods.swap(a, b);
                    changed = true;
                }
            });

        if changed {
            if episode_drag {
                self.begin_episode("Edit body field modifiers");
            } else {
                self.transaction_snapshot_only("Edit body field modifiers");
            }
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::SdfBody { field_modifiers, .. } = &mut node.payload {
                    *field_modifiers = mods;
                }
            }
            self.sync_body_field_stack_to_hardwired(nid);
            // Field domain → volume re-bake (not mesh-only).
            self.mark_dirty(DirtyFlags::SCENE);
            self.mark_node_body_vol_dirty(nid);
            // Folded mesh base is field-dependent (+ wire caches).
            self.invalidate_mesh_mod_display(nid);
        }
        if let Some(msg) = pending_toast {
            self.toast(&msg);
        }
    }

    /// Body Mesh Modifiers (post-DC). Edits do NOT re-bake the volume field.
    /// Also drawn from the Gate-5 Properties → Modifiers tab (OBJECT LEVEL).
    pub(crate) fn draw_props_body_mesh_modifiers(&mut self, ui: &mut egui::Ui, nid: core_math::scene_graph::NodeId) {
        use core_math::display_mesh_ops::{DisplayMeshModKind, DisplayMeshModRecord};
        use core_math::scene_graph::NodePayload;

        let mut mods: Vec<DisplayMeshModRecord> = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                NodePayload::SdfBody { mesh_modifiers, .. } => Some(mesh_modifiers.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let mut changed = false;
        // Slider/DragValue drags fire .changed() every frame; without this an entire drag would
        // push one undo entry PER FRAME (dozens of Ctrl+Z to undo one drag). Mirror the field-
        // modifier path: a drag routes through begin_episode() = ONE coalesced undo per drag.
        let mut episode_drag = false;
        egui::CollapsingHeader::new("Mesh Modifiers (post-DC)")
            .default_open(!mods.is_empty())
            .show(ui, |ui| {
                ui.weak("Display only — no volume re-bake. Live viewport + export.");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("add_body_mesh_mod")
                        .selected_text("+ Add Mesh Modifier")
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            if ui.button("Mirror").clicked() {
                                let mut m = DisplayMeshModRecord::new_mirror();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                            if ui.button("Smooth Geometry").clicked() {
                                let mut m = DisplayMeshModRecord::new_smooth_geometry();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                            if ui.button("Smooth Normals").clicked() {
                                let mut m = DisplayMeshModRecord::new_smooth_normals();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                            if ui.button("Decimate").clicked() {
                                let mut m = DisplayMeshModRecord::new_decimate();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                            if ui.button("Clay Polish").clicked() {
                                let mut m = DisplayMeshModRecord::new_clay_polish();
                                m.id = mods.iter().map(|x| x.id).max().unwrap_or(0).saturating_add(1);
                                mods.push(m);
                                changed = true;
                                ui.close_menu();
                            }
                        });
                });

                let n = mods.len();
                let mut remove: Option<usize> = None;
                let mut swap: Option<(usize, usize)> = None;
                for i in 0..n {
                    let label = mods[i].label().to_string();
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(100, 255, 150, 18))
                        .inner_margin(4.0)
                        .rounding(4.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut mods[i].enabled, "").changed() {
                                    changed = true;
                                }
                                ui.strong(format!("{}. {}", i + 1, label));
                                if matches!(mods[i].kind, DisplayMeshModKind::Mirror { .. }) {
                                    ui.weak("(mesh only)");
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("X").clicked() {
                                        remove = Some(i);
                                    }
                                    if i + 1 < n && ui.small_button("v").clicked() {
                                        swap = Some((i, i + 1));
                                    }
                                    if i > 0 && ui.small_button("^").clicked() {
                                        swap = Some((i, i - 1));
                                    }
                                });
                            });
                            match &mut mods[i].kind {
                                DisplayMeshModKind::Mirror {
                                    axis_bits,
                                    center,
                                    weld,
                                    weld_threshold,
                                } => {
                                    ui.horizontal(|ui| {
                                        let mut mx = (*axis_bits & 1) != 0;
                                        let mut my = (*axis_bits & 2) != 0;
                                        let mut mz = (*axis_bits & 4) != 0;
                                        if ui.checkbox(&mut mx, "X").changed() {
                                            *axis_bits = (*axis_bits & !1) | (mx as u8);
                                            changed = true;
                                        }
                                        if ui.checkbox(&mut my, "Y").changed() {
                                            *axis_bits = (*axis_bits & !2) | ((my as u8) << 1);
                                            changed = true;
                                        }
                                        if ui.checkbox(&mut mz, "Z").changed() {
                                            *axis_bits = (*axis_bits & !4) | ((mz as u8) << 2);
                                            changed = true;
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Center:");
                                        for (c, p) in [
                                            (&mut center.x, "X:"),
                                            (&mut center.y, "Y:"),
                                            (&mut center.z, "Z:"),
                                        ] {
                                            let r = ui.add(egui::DragValue::new(c).prefix(p).speed(0.02));
                                            if r.drag_started() { episode_drag = true; }
                                            if r.changed() { changed = true; }
                                        }
                                    });
                                    if ui.checkbox(weld, "Weld").changed() {
                                        changed = true;
                                    }
                                    if *weld {
                                        let r = ui.add(
                                            egui::Slider::new(weld_threshold, 1e-5..=0.1)
                                                .text("Threshold")
                                                .logarithmic(true),
                                        );
                                        if r.drag_started() { episode_drag = true; }
                                        if r.changed() { changed = true; }
                                    }
                                }
                                DisplayMeshModKind::SmoothGeometry {
                                    iterations,
                                    factor,
                                } => {
                                    let r = ui.add(egui::Slider::new(iterations, 1..=20).text("Iterations"));
                                    if r.drag_started() { episode_drag = true; }
                                    if r.changed() { changed = true; }
                                    let r = ui.add(egui::Slider::new(factor, 0.0..=1.0).text("Factor"));
                                    if r.drag_started() { episode_drag = true; }
                                    if r.changed() { changed = true; }
                                }
                                DisplayMeshModKind::SmoothNormals => {
                                    ui.weak("Recompute area-weighted vertex normals");
                                }
                                DisplayMeshModKind::Decimate {
                                    ratio,
                                    mode,
                                    crease_angle_deg,
                                    crease_strength,
                                    boundary_strength,
                                    field_strength,
                                    field_radius,
                                    max_error,
                                    planar_bias,
                                } => {
                                    use core_math::display_mesh_ops::DecimateMode;
                                    ui.horizontal(|ui| {
                                        ui.label("Mode:");
                                        egui::ComboBox::from_id_salt(format!("decimate_mode_{i}"))
                                            .selected_text(mode.label())
                                            .width(120.0)
                                            .show_ui(ui, |ui| {
                                                for m in DecimateMode::all() {
                                                    if ui
                                                        .selectable_label(*mode == *m, m.label())
                                                        .clicked()
                                                    {
                                                        *mode = *m;
                                                        changed = true;
                                                    }
                                                }
                                            });
                                    });
                                    match *mode {
                                        DecimateMode::Master => ui.weak(
                                            "Master: Decimation Master-style importance + QEM + flat-first.",
                                        ),
                                        DecimateMode::Quality => ui.weak(
                                            "Quality: Blender Collapse-like QEM + flat-first dihedral.",
                                        ),
                                        DecimateMode::Planar => ui.weak(
                                            "Planar: melt only flats (Blender Planar). Crease angle = limit.",
                                        ),
                                        DecimateMode::Fast => ui.weak(
                                            "Fast: length / flatness. Interactive scrub.",
                                        ),
                                        DecimateMode::Field => ui.weak(
                                            "Field: Master + optional SDF assist.",
                                        ),
                                    };
                                    let r = ui.add(
                                        egui::Slider::new(ratio, 0.05..=1.0).text("Keep ratio"),
                                    );
                                    if r.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r.changed() {
                                        changed = true;
                                    }

                                    // Feature controls only matter for Quality / Field.
// Principle #1 — always exposed.
                                    ui.separator();
                                    ui.strong("Flat-first (principle #1)");
                                    let r = ui.add(
                                        egui::Slider::new(planar_bias, 0.0..=4.0).text("Flat bias"),
                                    );
                                    if r.drag_started() {
                                        episode_drag = true;
                                    }
                                    if r.changed() {
                                        changed = true;
                                    }
                                    ui.weak("Higher = remove more tris on flats, keep curves.");

                                    if !matches!(*mode, DecimateMode::Fast) {
                                        ui.separator();
                                        ui.strong("Curvature protect");
                                        let r = ui.add(
                                            egui::Slider::new(crease_angle_deg, 5.0..=90.0)
                                                .text("Crease angle deg")
                                                .suffix(" deg"),
                                        );
                                        if r.drag_started() {
                                            episode_drag = true;
                                        }
                                        if r.changed() {
                                            changed = true;
                                        }
                                        ui.weak("Edges sharper than this are protected.");
                                        let r = ui.add(
                                            egui::Slider::new(crease_strength, 0.0..=3.0)
                                                .text("Crease protect"),
                                        );
                                        if r.drag_started() {
                                            episode_drag = true;
                                        }
                                        if r.changed() {
                                            changed = true;
                                        }
                                        let r = ui.add(
                                            egui::Slider::new(boundary_strength, 0.0..=3.0)
                                                .text("Boundary protect"),
                                        );
                                        if r.drag_started() {
                                            episode_drag = true;
                                        }
                                        if r.changed() {
                                            changed = true;
                                        }
                                        let r = ui.add(
                                            egui::Slider::new(max_error, 0.0..=0.05)
                                                .text("Max error")
                                                .logarithmic(true)
                                                .smallest_positive(1e-6),
                                        );
                                        if r.drag_started() {
                                            episode_drag = true;
                                        }
                                        if r.changed() {
                                            changed = true;
                                        }
                                        ui.weak("0 = ratio only. >0 stops when QEM error exceeds.");
                                    }
                                    if matches!(*mode, DecimateMode::Field) {
                                        ui.separator();
                                        ui.strong("SDF assist (secondary)");
                                        let r = ui.add(
                                            egui::Slider::new(field_strength, 0.0..=3.0)
                                                .text("SDF weight"),
                                        );
                                        if r.drag_started() {
                                            episode_drag = true;
                                        }
                                        if r.changed() {
                                            changed = true;
                                        }
                                        ui.weak("Optional; mesh dihedral already retains curves.");
                                        let r = ui.add(
                                            egui::Slider::new(field_radius, 0.25..=2.0)
                                                .text("Sample radius × edge"),
                                        );
                                        if r.drag_started() {
                                            episode_drag = true;
                                        }
                                        if r.changed() {
                                            changed = true;
                                        }
                                        ui.weak("FD scale for |H| (x average edge length).");
                                    }
                                }
                                DisplayMeshModKind::ClayPolish {
                                    iterations,
                                    factor,
                                } => {
                                    ui.weak("Preset: Smooth Geometry → Smooth Normals");
                                    let r = ui.add(egui::Slider::new(iterations, 1..=20).text("Iterations"));
                                    if r.drag_started() { episode_drag = true; }
                                    if r.changed() { changed = true; }
                                    let r = ui.add(egui::Slider::new(factor, 0.0..=1.0).text("Factor"));
                                    if r.drag_started() { episode_drag = true; }
                                    if r.changed() { changed = true; }
                                }
                            }
                        });
                }
                if let Some(i) = remove {
                    mods.remove(i);
                    changed = true;
                }
                if let Some((a, b)) = swap {
                    mods.swap(a, b);
                    changed = true;
                }
            });

        if changed {
            if episode_drag {
                self.begin_episode("Edit body mesh modifiers");
            } else {
                self.transaction_snapshot_only("Edit body mesh modifiers");
            }
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::SdfBody { mesh_modifiers, .. } = &mut node.payload {
                    *mesh_modifiers = mods;
                }
            }
            // Invalidate display stamp so sync rebuilds fold; keep raw base if field+res unchanged.
            self.cache.mesh_mod_display_stamp.remove(&nid);
            self.overlay_lines_fp = u64::MAX; // live wire update with Decimate scrub
            if let Some(NodePayload::SdfBody {
                mesh_cache_id: Some(cid),
                ..
            }) = self.scene.scene_graph.get(nid).map(|n| &n.payload)
            {
                self.wire_local_cache.remove(&cid.0);
            }
            // Mesh stack only — no volume bake.
            self.mark_dirty(DirtyFlags::RENDER);
            // Immediate live update (path B fold into mesh_cache → CachedCpu draw).
            if self.sync_body_mesh_mod_display(nid) {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
    }

    /// Body-level mirror: three axis checkboxes + flip side + mirror center Vec3 editor + workplane button.
    fn draw_props_body_mirror(&mut self, ui: &mut egui::Ui, nid: core_math::scene_graph::NodeId, mut flags: u8, mut sign: u8, mut center: glam::Vec3) {
        use core_math::scene_graph::NodePayload;

        let mut changed = false;
        egui::CollapsingHeader::new("Body Mirror").default_open(flags != 0).show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut mx = (flags & 1) != 0;
                let mut my = (flags & 2) != 0;
                let mut mz = (flags & 4) != 0;
                if ui.checkbox(&mut mx, "X").changed() { changed = true; }
                if ui.checkbox(&mut my, "Y").changed() { changed = true; }
                if ui.checkbox(&mut mz, "Z").changed() { changed = true; }
                flags = (mx as u8) | ((my as u8) << 1) | ((mz as u8) << 2);
            });
            if flags != 0 {
                // Flip Side toggles: select which side of the mirror plane to keep
                ui.horizontal(|ui| {
                    ui.label("Flip Side:");
                    let axes = ["X", "Y", "Z"];
                    for (bit, label) in axes.iter().enumerate() {
                        if (flags >> bit) & 1 != 0 {
                            let mut flipped = (sign >> bit) & 1 != 0;
                            if ui.checkbox(&mut flipped, *label).on_hover_text(format!("Keep {} side of {} mirror", if flipped { "negative" } else { "positive" }, label)).changed() {
                                sign ^= 1 << bit;
                                changed = true;
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Center:");
                    let mut v = [center.x, center.y, center.z];
                    for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                        ui.label(*axis);
                        if ui.add(egui::DragValue::new(&mut v[i]).speed(0.01).range(-100.0..=100.0)).changed() {
                            changed = true;
                        }
                    }
                    center = glam::Vec3::new(v[0], v[1], v[2]);
                });
                ui.horizontal(|ui| {
                    if ui.button("Set from Workplane").on_hover_text("Copy workplane origin to mirror center").clicked() {
                        let wp = self.viewport.wp_resolved.origin;
                        // Already at workplane origin → no empty undo.
                        if (center - wp).length_squared() > 1e-12 {
                            center = wp;
                            changed = true;
                        }
                    }
                    if ui.button("Flip").on_hover_text("Move all prims to the other side of the mirror plane").clicked() {
                        self.flip_body_mirror(nid, flags, center);
                    }
                });
            }
        });

        if changed {
            // Skip snapshot when values already match the node (e.g. slider noise / no-op edit).
            let already = self.scene.scene_graph.get(nid).map(|n| {
                match &n.payload {
                    NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, .. } => {
                        *mirror_flags == flags
                            && *mirror_sign == sign
                            && (*mirror_center - center).length_squared() < 1e-12
                    }
                    _ => true,
                }
            }).unwrap_or(true);
            if !already {
                self.transaction_snapshot_only("Body mirror");
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, .. } = &mut node.payload {
                        *mirror_flags = flags;
                        *mirror_sign = sign;
                        *mirror_center = center;
                    }
                }
                self.mark_dirty(DirtyFlags::SCENE);
                self.mark_node_body_vol_dirty(nid);
            }
        }
    }

    /// SDF → Voxel boolean stamp (the differentiator). Evaluate the SDF body `sdf_nid`'s field through
    /// the `FieldEvaluator` (`SceneEvaluator`, never an inlined SDF eval) and stamp it into the ACTIVE
    /// voxel sculpt's density grid: `additive` → union (add solid), else carve (subtract solid).
    ///
    /// Coordinate spaces: the voxel grid's `cell_world_pos` is in the voxel NODE's local frame; the SDF
    /// evaluator wants true WORLD space. So each voxel is mapped grid-local → world via the voxel node's
    /// `world_transform`, then `eval_body(sdf_nid, world)` gives the source distance (body-transform aware).
    ///
    /// This is a SCULPT mutation → it uses the voxel/sculpt undo path (a CPU density delta + Cpu order
    /// marker), NOT the SDF `transaction()`. The grid write marks its bricks dirty (atlas resync) and we
    /// mark RENDER so the live mesh/raymarch updates; the source SDF body is left untouched.
    pub(crate) fn stamp_sdf_into_active_sculpt(
        &mut self,
        sdf_nid: core_math::scene_graph::NodeId,
        additive: bool,
    ) {
        use core_math::field_eval::{FieldEvaluator, SceneEvaluator};
        use core_math::scene_graph::NodePayload;

        // An ASYNC Move bake writes the density grid on land — BLOCK-land it first (no-op when
        // none) so the stamp applies to the FINAL surface and the land can't stomp the stamped
        // region inside the move's union AABB (review P0). Grid-writer barrier, same as
        // smooth_all / symmetrize / resample.
        self.voxel_land_move_bake(true);

        // The voxel node's local→world transform (and its inverse), so we can map grid-local voxel
        // positions to the world space the evaluator expects, and the SDF body's world AABB back to
        // grid-local for a bounded sweep.
        let vnode = match self.sculpt.voxel.as_ref() { Some(v) => v.node, None => { self.toast("No active voxel sculpt"); return; } };
        let vworld = self.scene.scene_graph.world_transform(vnode);
        let vworld_inv = vworld.inverse();

        // Bound the sweep to the SDF body's world AABB mapped into grid-local space (so a small stamp
        // doesn't scan the whole grid). Built from the body's body-local prim bounds → world → grid-local.
        let bounds_local: Option<[glam::Vec3; 2]> = (|| {
            let (lmin, lmax) = match self.scene.scene_graph.get(sdf_nid)?.payload {
                NodePayload::SdfBody { ref primitives, mirror_flags, mirror_center, bevel_radius, .. } => {
                    let vis: Vec<_> = primitives.iter().filter(|p| p.visible).cloned().collect();
                    if vis.is_empty() { return None; }
                    crate::bake_utils::compute_body_bake_bounds(&vis, mirror_flags, mirror_center, bevel_radius)
                }
                _ => return None,
            };
            let body_world = self.scene.scene_graph.effective_body_transform(sdf_nid)?;
            // Transform all 8 corners of the body-local AABB → world → grid-local, take the enclosure.
            let mut gmin = glam::Vec3::splat(f32::MAX);
            let mut gmax = glam::Vec3::splat(f32::MIN);
            for i in 0..8 {
                let c = glam::Vec3::new(
                    if i & 1 == 0 { lmin.x } else { lmax.x },
                    if i & 2 == 0 { lmin.y } else { lmax.y },
                    if i & 4 == 0 { lmin.z } else { lmax.z },
                );
                let g = vworld_inv.transform_point3(body_world.transform_point3(c));
                gmin = gmin.min(g);
                gmax = gmax.max(g);
            }
            Some([gmin, gmax])
        })();

        // Stamp. Split disjoint field borrows so the evaluator (&self.scene) and the grid (&mut self.sculpt.voxel)
        // don't alias through `self`. The closure maps grid-local → world, then evaluates the source body.
        let res = self.sculpt.voxel.as_ref().map_or(0, |v| v.grid.res);
        let region = {
            // `new` (not a struct literal) so the evaluator carries its per-body BodyField memo —
            // this closure is invoked ONCE PER VOXEL by fill_from_sdf, and the field was previously
            // rebuilt (deep-cloning every prim) on each of those calls. Measured 12-16x.
            let eval = SceneEvaluator::new(&self.scene.scene_graph);
            let rec = &mut self.sculpt.voxel_stroke;
            rec.begin(res);
            let voxel = match self.sculpt.voxel.as_mut() { Some(v) => v, None => return };
            voxel.grid.fill_from_sdf(
                |local_p| {
                    let world_p = vworld.transform_point3(local_p);
                    eval.eval_body(sdf_nid, world_p)
                },
                additive,
                bounds_local,
                Some(rec),
            )
        };

        // Commit the recorded density delta as ONE entry on the voxel undo stack (CPU box path), exactly
        // like a finished CPU brush stroke — this stamp is its own undo step.
        if !self.sculpt.voxel_stroke.is_empty() {
            let delta = self.sculpt.voxel_stroke.take();
            let mut evicted = self.sculpt.voxel_undo_stack.push(delta);
            self.sculpt.voxel_undo_order.push_back(crate::UndoKind::Cpu);
            // Prune the same number of front Cpu markers the stack just evicted, so the shared order
            // stream can't outlive its backing entries (mirrors input_sculpt::prune_front_undo_markers,
            // which is module-private; the commit site there owns input_sculpt.rs which this task may not).
            if evicted > 0 {
                self.sculpt.voxel_undo_order.retain(|&k| if evicted > 0 && k == crate::UndoKind::Cpu { evicted -= 1; false } else { true });
            }
            self.sculpt.voxel_redo_stack.clear(); // a new edit truncates redo
            self.sculpt.voxel_redo_order.clear();
        }

        // Re-mesh the touched chunks and refresh the display. The grid write already marked its sparse
        // bricks dirty, so the next atlas sync re-uploads them; RENDER un-gates viewport_render this frame.
        if let Some(region) = region {
            self.voxel_mark_chunks_in_region(region);
            if let Some(v) = self.sculpt.voxel.as_mut() { v.grid.take_dirty(); v.cpu_grid_stale = true; }
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast(if additive { "Stamped SDF into sculpt" } else { "Carved SDF from sculpt" });
        } else {
            self.toast("SDF body does not overlap the voxel sculpt");
        }
    }
}
