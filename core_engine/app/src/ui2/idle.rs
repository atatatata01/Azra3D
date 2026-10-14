//! Idle / live UI gating for full-UI floor under ~1 ms.
//!
//! Immediate-mode docks rebuild thousands of widgets every viewport frame even when the
//! pointer is over the 3D view and nothing in the shell is interactive. That is the bulk
//! of the ~4 ms full-UI residual (scoreboard `lay=` / post-Central `[LAY-DOCK]`).
//!
//! Policy:
//! - **Live** when the pointer is over that dock (or float / rename / drag / text focus /
//!   open pie / forced env). Full widgets.
//! - **Idle** otherwise: the panel's LAST LIVE PAINT is replayed as raw egui shapes. Visual
//!   continuity for orbit / sculpt; the first frame the pointer enters the dock goes live so
//!   clicks hit real widgets. A panel with no usable snapshot draws LIVE, never a stub.
//!
//! Override: `AZRA_UI_ALWAYS_LIVE=1` disables the idle path (debug / A-B measure).

use eframe::egui;
use crate::*;
use super::*;

/// Margin around dock rect that forces live widgets (enter before the pointer is fully inside).
const LIVE_MARGIN: f32 = 10.0;

impl FourthLayerApp {
    /// Force full interactive widgets on every card (drag / rename / text / pie / env).
    /// Without this, **per-card** idle still applies when the pointer is only over one pane.
    pub(crate) fn ui2_dock_force_live(&self, ctx: &egui::Context) -> bool {
        // OFF BY DEFAULT (owner 2026-08-05: "the UI commit from performance was too intense,
        // brought a lot of regressions, those optimizations were too strong").
        //
        // Idle gating is now OPT-IN via AZRA_UI_IDLE=1. It is kept, not deleted, because the
        // measurement it came from is real - the docks rebuild thousands of widgets per viewport
        // frame - but a UI that is correct at 4ms beats one that is fast and wrong, and this
        // shipped three distinct regressions in one afternoon (blank panel bodies, an invisible
        // top bar, panels legible only on hover). It goes back on when it can be turned on
        // without the owner noticing anything except the frame time.
        if !std::env::var_os("AZRA_UI_IDLE").is_some_and(|v| v == "1") {
            return true;
        }
        if std::env::var_os("AZRA_UI_ALWAYS_LIVE").is_some() {
            return true;
        }
        self.ui2.tab_drag.is_some()
            || self.ui2.tab_float_drag.is_some()
            || self.ui2.edge_drag.is_some()
            || self.outliner.renaming.is_some()
            || self.ui2.ol_multi_rename.is_some()
            || self.outliner.dragging.is_some()
            || self.outliner.dragging_body.is_some()
            || ctx.wants_keyboard_input()
            || self.ui2_any_modal_chrome()
    }

    /// True when this dock *column* should consider live widgets (pointer near column, or forced).
    /// Individual cards still gate with [`Self::ui2_card_wants_live`] so hovering Outliner
    /// does not rebuild the Material tree on the sibling Properties card.
    pub(crate) fn ui2_dock_wants_live(
        &self,
        ctx: &egui::Context,
        side: DockSide,
        prect: egui::Rect,
    ) -> bool {
        if self.ui2_dock_force_live(ctx) {
            return true;
        }
        // Floating groups on this frame always live (small; interaction-heavy).
        if side == DockSide::Float {
            return true;
        }
        let Some(pos) = ctx.pointer_hover_pos().or_else(|| {
            ctx.input(|i| i.pointer.interact_pos())
        }) else {
            return false;
        };
        prect.expand(LIVE_MARGIN).contains(pos)
    }

    /// Open menus / pies that must not freeze the shell under them.
    pub(crate) fn ui2_any_modal_chrome(&self) -> bool {
        self.ui.ctx_menu_open
            || self.ui.q_menu_open
            || self.ui.show_add_popup
            || self.ui2.add_menu_open
            || self.ui.pivot_center_menu_open
            || self.ui.z_pie.open
            || self.ui.tab_pie.open
            || self.ui.view_pie.open
            || self.ui.reset_pie.open
            || self.ui.f_pie.open
            || self.ui.d_pie.open
            || self.ui.t_pie.open
            || self.ui.bool_pie.open
            || self.ui.sdf_bool_pie.open
            || self.ui.sdf_special_pie.open
            || self.ui.spine_corner_pie.open
            || self.ui.res_pie.open
            || self.ui.sym_pie.open
            || self.ui.align_pie.open
            || self.ui.merge_pie.open
            || self.ui.shape_swap_pie_open
    }

    /// Keyboard has nothing to do this frame (no keys, no text, no modal pie grab).
    pub(crate) fn ui2_keyboard_idle(&self, ctx: &egui::Context, mode_pie_pressed: bool) -> bool {
        if mode_pie_pressed || self.input_system.mode_pie_held {
            return false;
        }
        if ctx.wants_keyboard_input() {
            return false;
        }
        if self.ui.rebinding_action.is_some() {
            return false;
        }
        if self.ui2_any_modal_chrome() {
            return false;
        }
        // Continuous sculpt brush size while a key is held — keys_down non-empty keeps us live.
        // egui-winit swallows command+X/C into Event::Cut/Copy WITHOUT a Key event (see
        // KeyBind::matches). If we only watched Key/Text/Paste, a lone Cut frame with an empty
        // keys_down (Ctrl already released, or OS-synthesised Cut) early-returned the entire
        // keyboard path — Ctrl+X did nothing on poly faces (owner TEST_020 2026-08-06).
        ctx.input(|i| {
            i.keys_down.is_empty()
                && i.events.iter().all(|e| {
                    !matches!(
                        e,
                        egui::Event::Key { .. }
                            | egui::Event::Text(_)
                            | egui::Event::Paste(_)
                            | egui::Event::Cut
                            | egui::Event::Copy
                    )
                })
        })
    }

}

/// Paint pane body without allocating interactive widgets (idle dock).
impl FourthLayerApp {
    /// True when pointer is over top app bar / context bar (menus & pills need live widgets).
    pub(crate) fn ui2_chrome_top_wants_live(&self, ctx: &egui::Context) -> bool {
        if std::env::var_os("AZRA_UI_ALWAYS_LIVE").is_some() {
            return true;
        }
        if self.ui2_any_modal_chrome() || ctx.wants_keyboard_input() {
            return true;
        }
        let Some(pos) = ctx.pointer_hover_pos() else {
            return false;
        };
        let screen = ctx.screen_rect();
        let top_h = ui_theme::H_APPBAR + ui_theme::H_CONTEXT + LIVE_MARGIN;
        pos.y <= screen.top() + top_h
    }

    /// True when pointer is over the status bar strip.
    pub(crate) fn ui2_chrome_status_wants_live(&self, ctx: &egui::Context) -> bool {
        if std::env::var_os("AZRA_UI_ALWAYS_LIVE").is_some() {
            return true;
        }
        if self.ui2_any_modal_chrome() {
            return true;
        }
        let Some(pos) = ctx.pointer_hover_pos() else {
            return false;
        };
        let screen = ctx.screen_rect();
        pos.y >= screen.bottom() - ui_theme::H_STATUS - LIVE_MARGIN
    }



}


// =====================================================================================
// PANEL SNAPSHOT REPLAY
//
// Owner 2026-08-05: "each panel only appear when i hover on top of it, this performance
// feature is too strong; they should display a cached version of the last panel, updating
// when we move the mouse over it."
//
// The first cut of the idle path RE-PAINTED each panel from cached data with hand-written
// painter code, which meant every panel kind needed its own idle painter — and any kind
// without one fell through to a stub that drew nothing but its name. That is what made
// panels look empty until hovered.
//
// This replays the panel's LAST LIVE PAINT as raw egui shapes instead. One implementation
// covers every panel kind, the result is pixel-identical to what the panel actually showed,
// and no per-panel idle code can drift from the live code it is imitating.
// =====================================================================================

/// Cheap fingerprint of everything the docks DISPLAY. When it moves, every idle snapshot is
/// stale and its panel takes one live frame to retake it. Without this, an idle Properties
/// panel keeps showing the previous object after a viewport click — a cache that lies is worse
/// than a slow panel.
pub(crate) fn ui2_content_epoch(app: &FourthLayerApp) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    app.sel.usel.len().hash(&mut h);
    if let Some(a) = app.sel.usel.active {
        std::mem::discriminant(&a).hash(&mut h);
        format!("{a:?}").hash(&mut h);
    }
    app.poly_edit_generation.hash(&mut h);
    app.outliner.outliner_dirty.hash(&mut h);
    (app.modeling_context as u8).hash(&mut h);
    (app.poly_work_mode as u8).hash(&mut h);
    std::mem::discriminant(&app.tool.current).hash(&mut h);
    std::mem::discriminant(&app.poly_tool).hash(&mut h);
    app.scene.active_body_nid.0.hash(&mut h);
    h.finish()
}

/// Snapshot slot ids. Pane bodies key on group + tab; every other replayed region gets its own
/// constant. Distinct slots so a tab strip can never replay a body's picture, or vice versa.
pub(crate) mod slot {
    use super::PanelKind;
    pub(crate) const APP_BAR: u64 = 1;
    pub(crate) const CTXBAR: u64 = 2;
    pub(crate) const STATUS_BAR: u64 = 3;
    /// Group ids start at 0 and are dense, so shift them clear of the constants above.
    pub(crate) fn pane_body(group_id: u64, kind: PanelKind) -> u64 {
        (group_id << 16) | 0x1000 | (kind as u64 & 0xFFF)
    }
    pub(crate) fn tab_strip(group_id: u64) -> u64 {
        (group_id << 16) | 0x2000
    }
}

/// Number of shapes already queued on the BACKGROUND layer — the capture cursor. Docks, panels
/// and bars all paint there (egui puts every `SidePanel` / `TopBottomPanel` in
/// `LayerId::background()`), so one cursor covers chrome and pane bodies alike.
fn bg_shape_count(ctx: &egui::Context) -> usize {
    ctx.graphics_mut(|gs| gs.entry(egui::LayerId::background()).all_entries().len())
}

/// Run `draw` LIVE and keep the shapes it emitted as `slot`'s idle snapshot.
///
/// `rect` is the region the shapes belong to; replay translates by the delta so a moved or
/// resized region still draws in the right place.
pub(crate) fn capture_idle(
    app: &mut FourthLayerApp,
    ctx: &egui::Context,
    slot: u64,
    rect: egui::Rect,
    draw: impl FnOnce(&mut FourthLayerApp),
) {
    let before = bg_shape_count(ctx);
    draw(app);
    let shapes: Vec<egui::epaint::ClippedShape> = ctx.graphics_mut(|gs| {
        gs.entry(egui::LayerId::background())
            .all_entries()
            .skip(before)
            .cloned()
            .collect()
    });
    if shapes.is_empty() {
        return; // nothing painted (collapsed / zero-size) — keep any older snapshot
    }
    let epoch = ui2_content_epoch(app);
    app.ui2
        .idle_snapshots
        .insert(slot, crate::ui2::types::IdlePanelSnapshot { rect, shapes, epoch });
}

/// Replay `slot`'s snapshot. Returns false when there is nothing usable (never captured, or the
/// content epoch moved) — the caller then draws LIVE, so a region is NEVER blank.
pub(crate) fn replay_idle(
    app: &FourthLayerApp,
    ctx: &egui::Context,
    slot: u64,
    rect: egui::Rect,
) -> bool {
    let Some(snap) = app.ui2.idle_snapshots.get(&slot) else {
        return false;
    };
    if snap.epoch != ui2_content_epoch(app) {
        return false; // content moved while idle — retake it live this frame
    }
    let delta = rect.min - snap.rect.min;
    ctx.graphics_mut(|gs| {
        let pl = gs.entry(egui::LayerId::background());
        for cs in &snap.shapes {
            let mut shape = cs.shape.clone();
            if delta != egui::Vec2::ZERO {
                shape.translate(delta);
            }
            // Clip to the CURRENT region: a shrunken dock must not paint outside itself.
            let clip = cs.clip_rect.translate(delta).intersect(rect);
            if clip.is_positive() {
                pl.add(clip, shape);
            }
        }
    });
    true
}
