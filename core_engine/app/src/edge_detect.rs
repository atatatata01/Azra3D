// core_engine/app/src/edge_detect.rs
// Boolean edge detection + overlay + click-to-bevel
use eframe::egui;
use glam::{Quat, Vec3};
use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, eval_scene};
use crate::{AzraApp, BoolEdgeCurve, SceneItemId, world_to_screen};

// ---------------------------------------------------------------------------
// Bounding radius for a primitive (matches drawing.rs raycast_select)
// ---------------------------------------------------------------------------
fn prim_bound_radius(p: &SdfPrimitive) -> f32 {
    (match &p.shape {
        SdfShape::Sphere { radius } => *radius * p.scale.x.max(p.scale.y).max(p.scale.z),
        SdfShape::Box { half_extents: h, .. } => (p.scale * *h).length(),
        SdfShape::Cylinder { radius, half_height } => {
            let r = p.scale.x.max(p.scale.z) * *radius;
            let hh = p.scale.y * *half_height;
            (r * r + hh * hh).sqrt()
        }
        SdfShape::Torus { major_radius, minor_radius } => {
            p.scale.x.max(p.scale.z) * (*major_radius + *minor_radius)
        }
        SdfShape::Cone { radius, half_height } => {
            let r = p.scale.x.max(p.scale.z) * *radius;
            let hh = p.scale.y * *half_height;
            (r * r + hh * hh).sqrt()
        }
        SdfShape::Capsule { radius, half_height } => {
            p.scale.y * *half_height + p.scale.x.max(p.scale.z) * *radius
        }
        SdfShape::Stair { half_extents, .. } => (*half_extents * p.scale).length(),
        SdfShape::SpiralStair { radius, half_height, .. } => {
            p.scale.max_element() * (radius + half_height)
        }
        SdfShape::Billboard { width, height, .. } => {
            let w = p.scale.x * *width * 0.5;
            let h = p.scale.y * *height * 0.5;
            (w * w + h * h).sqrt()
        }
        SdfShape::CurveExtrude { half_depth, .. } => p.scale.max_element() * *half_depth,
        SdfShape::MeshVolume { grid, .. } => (p.scale * grid.corner_abs_max()).length(),
        // Infinite slice plane: a nominal click radius near the origin keeps it selectable
        // (the plane is picked mainly via the outliner; this is a coarse viewport fallback).
        SdfShape::HalfSpace => 0.5,
        SdfShape::SpinePipe { points, radius, point_scale, .. } => {
            let max_pt = points.iter().map(|q| q.length()).fold(0.0f32, f32::max);
            let max_s = point_scale.iter().copied().fold(1.0f32, f32::max).max(1.0);
            p.scale.max_element() * (max_pt + *radius * max_s)
        },
    })
    .max(0.08)
}

// ---------------------------------------------------------------------------
// Does this boolean op create an intersection edge?
// ---------------------------------------------------------------------------
fn op_creates_edge(op: &BooleanOp) -> bool {
    !matches!(op, BooleanOp::Union { .. })
}

/// Extract the blend k from a BooleanOp (0.0 for Intersect / hard Union).
pub(crate) fn op_get_k(op: &BooleanOp) -> f32 {
    op.blend_k()
}

/// Set the blend k on a BooleanOp (no-op for Intersect).
pub(crate) fn op_set_k(op: &mut BooleanOp, new_k: f32) {
    op.set_blend_k(new_k.max(0.0));
}

/// Set blend k clamped to `[0, max_k]` (viewport drag + property sliders share this).
pub(crate) fn op_set_k_clamped(op: &mut BooleanOp, new_k: f32, max_k: f32) {
    let max_k = max_k.max(0.0);
    op.set_blend_k(new_k.clamp(0.0, max_k));
}

// ---------------------------------------------------------------------------
// Viewport blend-k diamond — what the ONE handle edits for a given target
// ---------------------------------------------------------------------------
// The diamond handle (draw_blend_k_handle / BlendKDrag) is the single viewport
// intensity control. Its meaning branches on the target's OP (same-code-path
// rule — never a parallel drag tool):
//   BlendK          → classic blend k (unsigned, HORIZONTAL drag right = smoother)
//   SeamDepth       → 2-AXIS: VERTICAL = SIGNED depth (drag DOWN cuts the groove
//                     deeper (+), dragging UP through zero flips to a raised
//                     bead (−) — physical intuition: up raises material; NOTE
//                     this replaced the pre-2026-07-14 right=deeper horizontal
//                     mapping, owner-directed); HORIZONTAL = width (clamped to
//                     the Boolean-card Width slider range)
//   ExtrudeStrength → 2-AXIS: VERTICAL = SIGNED strength (drag UP raises /
//                     extrudes out (+), dragging DOWN through zero flips to a
//                     recess (−) — same up-raises-material intuition);
//                     HORIZONTAL = width
// The handle keys purely on the boolean op — it no longer touches the legacy
// ModifierType::Emboss/Deboss self-stamp modifiers (those keep their
// properties-panel UI; the pie/handle path is BooleanOp::Extrude now).

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum BlendHandleMode {
    BlendK,
    SeamDepth,
    /// BooleanOp::Extrude — vertical = signed strength, horizontal = width.
    ExtrudeStrength,
}

/// Resolve what the viewport diamond edits for this prim (keyed on the op).
pub(crate) fn prim_handle_mode(p: &SdfPrimitive) -> BlendHandleMode {
    op_handle_mode(&p.operation)
}

/// True if this flat prim can carry a blend-k / seam / extrude diamond (not base of chain,
/// not hard Intersect which has no k slot until upgraded via properties).
pub(crate) fn prim_accepts_blend_handle(scene: &crate::scene_state::SceneState, flat_idx: usize) -> bool {
    if flat_idx >= scene.primitives.len() { return false; }
    if scene.is_base_prim_flat(flat_idx) { return false; }
    !matches!(&scene.primitives[flat_idx].operation, BooleanOp::Intersect)
}

/// Op → handle mode (shared by the prim and body handle targets).
pub(crate) fn op_handle_mode(op: &BooleanOp) -> BlendHandleMode {
    match op {
        BooleanOp::Seam { .. } => BlendHandleMode::SeamDepth,
        BooleanOp::Extrude { .. } => BlendHandleMode::ExtrudeStrength,
        _ => BlendHandleMode::BlendK,
    }
}

/// Signed seam intensity: +depth = groove (cut into the surface),
/// −depth = bead (raised). 0.0 for non-Seam ops.
pub(crate) fn seam_signed_depth(op: &BooleanOp) -> f32 {
    match op {
        BooleanOp::Seam { depth, raised, .. } => if *raised { -*depth } else { *depth },
        _ => 0.0,
    }
}

/// Apply a signed handle value to a Seam op: sign selects polarity
/// (>0 groove, <0 bead — dragging through zero flips), magnitude is the
/// depth, clamped to the Boolean-card Depth slider range (0.002..=0.5).
pub(crate) fn seam_set_signed_depth(op: &mut BooleanOp, s: f32) {
    if let BooleanOp::Seam { depth, raised, .. } = op {
        *raised = s < 0.0;
        *depth = s.abs().clamp(0.002, 0.5);
    }
}

/// Signed extrude strength: +strength = raise (emboss/extrude out),
/// −strength = recess (deboss/extrude in). 0.0 for non-Extrude ops.
pub(crate) fn extrude_signed_strength(op: &BooleanOp) -> f32 {
    match op {
        BooleanOp::Extrude { strength, .. } => *strength,
        _ => 0.0,
    }
}

/// Apply a signed handle value to an Extrude op's strength: dragging through
/// zero flips raise↔recess. Clamped to the Boolean-card Strength slider range
/// (±EXTRUDE_STRENGTH_MAX).
pub(crate) fn extrude_set_signed_strength(op: &mut BooleanOp, s: f32) {
    if let BooleanOp::Extrude { strength, .. } = op {
        *strength = s.clamp(-crate::ui_panels::EXTRUDE_STRENGTH_MAX,
                            crate::ui_panels::EXTRUDE_STRENGTH_MAX);
    }
}

/// Apply the handle's HORIZONTAL axis to a Seam/Extrude op's width, clamped to
/// the Boolean-card Width slider range (0.005..=0.5). The k slot carries width
/// for both ops, so this routes through `set_blend_k` (one mutation path).
pub(crate) fn op_set_width_clamped(op: &mut BooleanOp, w: f32) {
    op.set_blend_k(w.clamp(0.005, 0.5));
}

/// Seam profile label for the viewport handle click-cycle (V=Chamfer, U=Soft,
/// Square=Square). Same three `SEAM_PROFILE_*` values the property panel exposes.
pub(crate) fn seam_profile_label(profile: u8) -> &'static str {
    use core_math::primitives::{SEAM_PROFILE_U, SEAM_PROFILE_SQUARE};
    match profile {
        SEAM_PROFILE_U => "Soft",
        SEAM_PROFILE_SQUARE => "Square",
        _ => "Chamfer", // SEAM_PROFILE_V (and any parked value normalizes here)
    }
}

/// Next seam profile in the click-cycle: V (chamfer) → U (soft) → Square → V.
pub(crate) fn seam_next_profile(cur: u8) -> u8 {
    use core_math::primitives::{SEAM_PROFILE_V, SEAM_PROFILE_U, SEAM_PROFILE_SQUARE};
    match cur {
        SEAM_PROFILE_V => SEAM_PROFILE_U,
        SEAM_PROFILE_U => SEAM_PROFILE_SQUARE,
        _ => SEAM_PROFILE_V, // Square (and any parked value) wraps back to V
    }
}

// ---------------------------------------------------------------------------
// Generate ordered contour samples on cutter surface (shape-aware)
// ---------------------------------------------------------------------------

/// Sample ordered contour lines along the cutter's geometry.
/// Returns multiple contour strips (one per edge/circle).
fn sample_cutter_contours(prim: &SdfPrimitive) -> Vec<Vec<Vec3>> {
    let rot = prim.rotation_quat();
    let pos = prim.position;
    let sc = prim.scale;
    let n_per_edge = 48_usize;

    match &prim.shape {
        SdfShape::Box { half_extents: he, .. } => {
            // Sample along each of the 12 box edges in world space
            let he_w = *he; // local half extents (scale applied via world transform)
            let corners_local: [Vec3; 8] = [
                Vec3::new(-he_w.x, -he_w.y, -he_w.z),
                Vec3::new( he_w.x, -he_w.y, -he_w.z),
                Vec3::new(-he_w.x,  he_w.y, -he_w.z),
                Vec3::new( he_w.x,  he_w.y, -he_w.z),
                Vec3::new(-he_w.x, -he_w.y,  he_w.z),
                Vec3::new( he_w.x, -he_w.y,  he_w.z),
                Vec3::new(-he_w.x,  he_w.y,  he_w.z),
                Vec3::new( he_w.x,  he_w.y,  he_w.z),
            ];
            let edges: [(usize, usize); 12] = [
                (0,1),(2,3),(4,5),(6,7), // bottom/top X edges
                (0,2),(1,3),(4,6),(5,7), // Y edges
                (0,4),(1,5),(2,6),(3,7), // Z edges
            ];
            let mut contours = Vec::with_capacity(12);
            for &(a, b) in &edges {
                let mut strip = Vec::with_capacity(n_per_edge);
                for i in 0..n_per_edge {
                    let t = i as f32 / (n_per_edge - 1) as f32;
                    let local = corners_local[a] * (1.0 - t) + corners_local[b] * t;
                    strip.push(pos + rot * (local * sc));
                }
                contours.push(strip);
            }
            contours
        }
        SdfShape::Sphere { radius } => {
            // 3 great circles (XY, XZ, YZ) + 4 latitude rings
            let r = *radius;
            let mut contours = Vec::new();
            let n = n_per_edge;
            for ring in 0..7 {
                let mut strip = Vec::with_capacity(n + 1);
                for i in 0..=n {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    let local = match ring {
                        0 => Vec3::new(t.cos() * r, t.sin() * r, 0.0),             // XY
                        1 => Vec3::new(t.cos() * r, 0.0, t.sin() * r),             // XZ
                        2 => Vec3::new(0.0, t.cos() * r, t.sin() * r),             // YZ
                        3 => Vec3::new(t.cos() * r * 0.7, r * 0.7, t.sin() * r * 0.7), // lat +
                        4 => Vec3::new(t.cos() * r * 0.7, -r * 0.7, t.sin() * r * 0.7), // lat -
                        5 => Vec3::new(r * 0.7, t.cos() * r * 0.7, t.sin() * r * 0.7),
                        _ => Vec3::new(-r * 0.7, t.cos() * r * 0.7, t.sin() * r * 0.7),
                    };
                    strip.push(pos + rot * (local * sc));
                }
                contours.push(strip);
            }
            contours
        }
        SdfShape::Cylinder { radius, half_height } => {
            let r = *radius;
            let hh = *half_height;
            let n = n_per_edge;
            let mut contours = Vec::new();
            // Top + bottom circles + 4 vertical lines
            for y in [-hh, 0.0, hh] {
                let mut strip = Vec::with_capacity(n + 1);
                for i in 0..=n {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    let local = Vec3::new(t.cos() * r, y, t.sin() * r);
                    strip.push(pos + rot * (local * sc));
                }
                contours.push(strip);
            }
            for angle in [0.0, std::f32::consts::FRAC_PI_2, std::f32::consts::PI, 3.0 * std::f32::consts::FRAC_PI_2] {
                let mut strip = Vec::with_capacity(n);
                for i in 0..n {
                    let t = i as f32 / (n - 1) as f32;
                    let y = -hh + t * 2.0 * hh;
                    let local = Vec3::new(angle.cos() * r, y, angle.sin() * r);
                    strip.push(pos + rot * (local * sc));
                }
                contours.push(strip);
            }
            contours
        }
        _ => {
            // Fallback: 3 great circles at bounding radius
            let r = prim_bound_radius(prim);
            let n = n_per_edge;
            let mut contours = Vec::new();
            for ring in 0..3 {
                let mut strip = Vec::with_capacity(n + 1);
                for i in 0..=n {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    let local = match ring {
                        0 => Vec3::new(t.cos(), t.sin(), 0.0),
                        1 => Vec3::new(t.cos(), 0.0, t.sin()),
                        _ => Vec3::new(0.0, t.cos(), t.sin()),
                    };
                    strip.push(pos + rot * (local * r));
                }
                contours.push(strip);
            }
            contours
        }
    }
}

// ---------------------------------------------------------------------------
// Core: detect boolean intersection edges
// ---------------------------------------------------------------------------

/// Detect intersection edges for all boolean primitives in the scene.
/// Uses cutter geometry contours for clean, ordered edge curves.
pub(crate) fn detect_bool_edges(prims: &[SdfPrimitive]) -> Vec<BoolEdgeCurve> {
    let mut curves = Vec::new();
    let visible: Vec<(usize, &SdfPrimitive)> = prims
        .iter()
        .enumerate()
        .filter(|(_, p)| p.visible)
        .collect();

    if visible.len() < 2 {
        return curves;
    }

    for vi in 1..visible.len() {
        let (orig_idx, prim) = visible[vi];
        if !op_creates_edge(&prim.operation) {
            continue;
        }

        // NOTE (nested sub-bodies, 2026-07-09): this overlay deliberately stays on the FLAT
        // per-prefix eval. It is a cosmetic hover heuristic over the flat prim cache of the
        // ACTIVE body ("where does cutter vi meet the surface built by prims 0..vi"), not a
        // field oracle — prefix semantics have no meaning across scope boundaries, and the
        // flat cache carries no scope info. Scope-aware field queries use
        // `core_math::field_eval::BodyField` (see COMPLETE_BUG_TRACKER 2026-07-09).
        let before: Vec<&SdfPrimitive> = visible[..vi].iter().map(|(_, p)| *p).collect();
        if before.is_empty() {
            continue;
        }

        let bound_r = prim_bound_radius(prim);
        let edge_threshold = 0.03 * bound_r.max(0.05);

        // Walk each contour strip on the cutter; keep segments where scene-before ≈ 0
        let contours = sample_cutter_contours(prim);
        for strip in &contours {
            // For each point on the strip, test if it's near the scene-before surface
            let mut segments: Vec<Vec3> = Vec::new();
            let mut was_on = false;

            for &wp in strip {
                let d_before = eval_scene(wp, &before).abs();
                let on_edge = d_before < edge_threshold;

                if on_edge {
                    segments.push(wp);
                    was_on = true;
                } else if was_on && segments.len() >= 2 {
                    // Flush current segment
                    let smoothed = smooth_chain(&segments, 3);
                    curves.push(BoolEdgeCurve {
                        prim_idx: orig_idx,
                        points_world: smoothed,
                        points_screen: Vec::new(),
                    });
                    segments = Vec::new();
                    was_on = false;
                } else {
                    segments.clear();
                    was_on = false;
                }
            }
            // Flush remaining
            if segments.len() >= 2 {
                let smoothed = smooth_chain(&segments, 3);
                curves.push(BoolEdgeCurve {
                    prim_idx: orig_idx,
                    points_world: smoothed,
                    points_screen: Vec::new(),
                });
            }
        }
    }

    curves
}

// ---------------------------------------------------------------------------
// Laplacian smoothing — average each interior point with its neighbors
// ---------------------------------------------------------------------------
fn smooth_chain(pts: &[Vec3], iterations: usize) -> Vec<Vec3> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut buf = pts.to_vec();
    let n = buf.len();
    for _ in 0..iterations {
        let prev = buf.clone();
        for i in 1..n - 1 {
            buf[i] = prev[i - 1] * 0.25 + prev[i] * 0.5 + prev[i + 1] * 0.25;
        }
    }
    buf
}

// ---------------------------------------------------------------------------
// Project 3D polylines to screen space
// ---------------------------------------------------------------------------
pub(crate) fn project_edges(
    edges: &mut [BoolEdgeCurve],
    cam_pos: Vec3,
    cam_orbit: Quat,
    rect: egui::Rect,
    aspect: f32,
    ortho_hw: f32,
    fov_deg: f32,
) {
    for curve in edges.iter_mut() {
        curve.points_screen.clear();
        curve.points_screen.reserve(curve.points_world.len());
        for wp in &curve.points_world {
            if let Some(sp) = world_to_screen(*wp, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg) {
                curve.points_screen.push(sp);
            } else {
                // Behind camera — insert a sentinel NaN so we break the polyline
                curve.points_screen.push(egui::pos2(f32::NAN, f32::NAN));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Hover detection: which edge curve is closest to mouse?
// ---------------------------------------------------------------------------
pub(crate) fn detect_edge_hover(
    edges: &[BoolEdgeCurve],
    mouse: egui::Pos2,
    threshold: f32,
) -> Option<usize> {
    let mut best_idx: Option<usize> = None;
    let mut best_dist = threshold;

    for (ci, curve) in edges.iter().enumerate() {
        if curve.points_screen.len() < 2 {
            continue;
        }
        for i in 0..curve.points_screen.len() - 1 {
            let a = curve.points_screen[i];
            let b = curve.points_screen[i + 1];
            // Skip NaN sentinels
            if a.x.is_nan() || b.x.is_nan() {
                continue;
            }
            let d = pt_to_seg_dist(mouse, a, b);
            if d < best_dist {
                best_dist = d;
                best_idx = Some(ci);
            }
        }
    }

    best_idx
}

/// Screen-space distance from point to segment (same as bool_cut.rs)
fn pt_to_seg_dist(m: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let am = m - a;
    let len2 = ab.x * ab.x + ab.y * ab.y;
    if len2 < 0.0001 {
        return am.length();
    }
    let t = ((am.x * ab.x + am.y * ab.y) / len2).clamp(0.0, 1.0);
    let closest = a + egui::vec2(ab.x * t, ab.y * t);
    (m - closest).length()
}

// ---------------------------------------------------------------------------
// Draw boolean edge overlays
// ---------------------------------------------------------------------------
impl AzraApp {
    pub(crate) fn draw_bool_edges(&self, painter: &egui::Painter) {
        if !self.ui.show_bool_edges || self.bool_edges.is_empty() {
            return;
        }

        for (ci, curve) in self.bool_edges.iter().enumerate() {
            if curve.points_screen.len() < 2 {
                continue;
            }

            let is_hovered = self.bool_edge_hovered == Some(ci);
            let is_selected = if curve.prim_idx < self.scene.primitives.len() {
                self.sel.usel.selected.contains(&SceneItemId::Prim(self.scene.primitives[curve.prim_idx].prim_uid))
            } else { false };
            let is_dragging = self
                .tool.current.as_bool_edge_bevel()
                .map_or(false, |d| d.prim_idx == curve.prim_idx);

            let (color, width) = if is_dragging {
                (egui::Color32::from_rgb(255, 220, 60), 3.5)
            } else if is_hovered {
                (egui::Color32::from_rgb(255, 255, 180), 3.0)
            } else if is_selected {
                (egui::Color32::from_rgb(255, 170, 40), 2.5)
            } else {
                (egui::Color32::from_rgba_unmultiplied(80, 220, 255, 160), 1.5)
            };

            for i in 0..curve.points_screen.len() - 1 {
                let a = curve.points_screen[i];
                let b = curve.points_screen[i + 1];
                if a.x.is_nan() || b.x.is_nan() {
                    continue;
                }
                painter.line_segment([a, b], egui::Stroke::new(width, color));
            }
        }

        // Tooltip for hovered edge
        if let Some(ci) = self.bool_edge_hovered {
            if let Some(curve) = self.bool_edges.get(ci) {
                // Find midpoint of the curve on screen for tooltip placement
                let mid_idx = curve.points_screen.len() / 2;
                if let Some(&sp) = curve.points_screen.get(mid_idx) {
                    if !sp.x.is_nan() {
                        let k = self
                            .scene.primitives
                            .get(curve.prim_idx)
                            .map(|p| op_get_k(&p.operation))
                            .unwrap_or(0.0);
                        let label = if self.tool.current.is_bool_edge_bevel() {
                            format!("Bevel k: {:.3}", k)
                        } else {
                            format!("Bool edge — drag to bevel (k={:.2})", k)
                        };
                        painter.text(
                            sp + egui::vec2(12.0, -16.0),
                            egui::Align2::LEFT_CENTER,
                            label,
                            egui::FontId::proportional(11.0),
                            egui::Color32::from_rgb(255, 255, 200),
                        );
                    }
                }
            }
        }
    }
}
