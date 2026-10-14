//! Shared bool-op / seam / extrude defaults (pure-relocated from ui_panels.rs).
//! ONE source for every picker (Boolean card, outliner/context menus, X pie, sub-body panel).

use eframe::egui;
use core_math::primitives::BooleanOp;

/// Fresh-pick Seam parameter defaults — the ONE source for every Seam picker
/// (Boolean card, outliner/context menus, X boolean pie, sub-body panel).
pub(crate) const SEAM_DEFAULT_WIDTH: f32 = 0.05;
pub(crate) const SEAM_DEFAULT_DEPTH: f32 = 0.03;
pub(crate) const SEAM_DEFAULT_PROFILE: u8 = 0; // V (chamfer)

/// Seam op for a pick: keeps its params when the target is already a Seam; fresh
/// picks get the shared defaults. `raised: None` preserves the current polarity
/// (Boolean card behavior); `Some(r)` forces it (menu/pie groove-vs-bead entries).
pub(crate) fn seam_op_defaults(current: &BooleanOp, raised: Option<bool>) -> BooleanOp {
    match current {
        BooleanOp::Seam { width, depth, profile, raised: cur } =>
            BooleanOp::Seam { width: *width, depth: *depth, profile: *profile, raised: raised.unwrap_or(*cur) },
        _ => BooleanOp::Seam {
            width: SEAM_DEFAULT_WIDTH, depth: SEAM_DEFAULT_DEPTH,
            profile: SEAM_DEFAULT_PROFILE, raised: raised.unwrap_or(false),
        },
    }
}

/// Fresh-pick Extrude parameter defaults + handle/slider bound — the ONE source for
/// every Extrude picker (Boolean card, context menus, X boolean pie, sub-body panel).
/// Default polarity is RAISE (+strength = extrude out — matches the "extrude" mental
/// model); the viewport handle's vertical axis flips through zero to recess.
pub(crate) const EXTRUDE_DEFAULT_STRENGTH: f32 = 0.05;
pub(crate) const EXTRUDE_DEFAULT_WIDTH: f32 = 0.05;
/// Strength slider/handle magnitude bound (±).
pub(crate) const EXTRUDE_STRENGTH_MAX: f32 = 0.5;

/// Extrude op for a pick: keeps its params when the target is already an Extrude;
/// fresh picks get the shared defaults. `raise: None` preserves the current polarity
/// (pie re-pick behavior); `Some(r)` forces it (menu raise/recess entries).
pub(crate) fn extrude_op_defaults(current: &BooleanOp, raise: Option<bool>) -> BooleanOp {
    match current {
        BooleanOp::Extrude { strength, width } => {
            let s = match raise {
                None => *strength,
                Some(r) => {
                    let mag = strength.abs().max(0.002);
                    if r { mag } else { -mag }
                }
            };
            BooleanOp::Extrude { strength: s, width: *width }
        }
        _ => BooleanOp::Extrude {
            strength: if raise.unwrap_or(true) { EXTRUDE_DEFAULT_STRENGTH } else { -EXTRUDE_DEFAULT_STRENGTH },
            width: EXTRUDE_DEFAULT_WIDTH,
        },
    }
}

/// Blend-k to carry over when switching op TYPE while preserving smoothness
/// (context-menu + X boolean pie semantics — a hard Intersect contributes 0).
pub(crate) fn bool_op_preserved_k(current: &BooleanOp) -> f32 {
    match current {
        BooleanOp::Union { k } | BooleanOp::Subtract { k } => *k,
        BooleanOp::Intersect => 0.0,
        _ => current.blend_k(), // normalize: deprecated ChamferUnion/ChamferSubtract
    }
}

/// Boolean op menu items. Returns Some(new_op) if user picked one.
/// `show_smooth_variants`: true = 7 items with hard/smooth union+subtract (outliner),
/// false = 5 combined items preserving current k (context menu).
pub(crate) fn bool_op_menu_items(ui: &mut egui::Ui, current_op: &BooleanOp, show_smooth_variants: bool) -> Option<BooleanOp> {
    let cur_k = if show_smooth_variants && matches!(current_op, BooleanOp::Intersect) { 0.15 }
                else { bool_op_preserved_k(current_op) };
    let mut result = None;
    // Seam/Extrude keep their params when re-picked; fresh picks get workable defaults.
    let seam_defaults = |raised: bool| seam_op_defaults(current_op, Some(raised));
    let ext_defaults = |raise: bool| extrude_op_defaults(current_op, Some(raise));
    if show_smooth_variants {
        if ui.button("\u{222a}  Union (hard)").clicked() { result = Some(BooleanOp::Union{k:0.0}); ui.close_menu(); }
        if ui.button("\u{222a}~ Union (smooth)").clicked() { result = Some(BooleanOp::Union{k:cur_k.max(0.01)}); ui.close_menu(); }
        if ui.button("\u{2212}  Subtract (hard)").clicked() { result = Some(BooleanOp::Subtract{k:0.0}); ui.close_menu(); }
        if ui.button("\u{2212}~ Subtract (smooth)").clicked() { result = Some(BooleanOp::Subtract{k:cur_k.max(0.01)}); ui.close_menu(); }
        if ui.button("\u{2229}  Intersect").clicked() { result = Some(BooleanOp::Intersect); ui.close_menu(); }
        if ui.button("\u{2248}\u{2212} Seam (groove)").clicked() { result = Some(seam_defaults(false)); ui.close_menu(); }
        if ui.button("\u{2248}+ Seam (bead)").clicked() { result = Some(seam_defaults(true)); ui.close_menu(); }
        if ui.button("\u{25b4}  Extrude (raise)").clicked() { result = Some(ext_defaults(true)); ui.close_menu(); }
        if ui.button("\u{25be}  Extrude (recess)").clicked() { result = Some(ext_defaults(false)); ui.close_menu(); }
    } else {
        if ui.button("Add (Union)").clicked() { result = Some(BooleanOp::Union{k:cur_k}); ui.close_menu(); }
        if ui.button("Subtract").clicked() { result = Some(BooleanOp::Subtract{k:cur_k}); ui.close_menu(); }
        if ui.button("Intersect").clicked() { result = Some(BooleanOp::Intersect); ui.close_menu(); }
        if ui.button("Seam (groove)").clicked() { result = Some(seam_defaults(false)); ui.close_menu(); }
        if ui.button("Seam (bead)").clicked() { result = Some(seam_defaults(true)); ui.close_menu(); }
        if ui.button("Extrude (raise)").clicked() { result = Some(ext_defaults(true)); ui.close_menu(); }
        if ui.button("Extrude (recess)").clicked() { result = Some(ext_defaults(false)); ui.close_menu(); }
    }
    result
}
