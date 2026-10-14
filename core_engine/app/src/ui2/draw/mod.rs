// Split from ui2 god file. Prefer <500 lines; shared chrome lives in ui2::chrome.
//! ui2 draw — entry + pointer-over + pies / placement / viewport-split modules.

mod pies;
mod placement;
mod viewport_split;

use eframe::egui;
use crate::*;
use super::*;

impl FourthLayerApp {
    fn ui2_pointer_over_panel_kind(&self, ctx: &egui::Context, kind: PanelKind) -> bool {
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
            return false;
        };
        let group_shows_kind = |id: u64| {
            self.ui2
                .dock
                .iter()
                .find(|group| group.id == id)
                .and_then(|group| group.active_kind())
                == Some(kind)
        };
        for &(id, rect) in self
            .ui2
            .left_rects
            .iter()
            .chain(self.ui2.right_rects.iter())
            .chain(self.ui2.top_rects.iter())
            .chain(self.ui2.bottom_rects.iter())
        {
            if group_shows_kind(id) && rect.contains(pos) {
                return true;
            }
        }
        self.ui2.dock.iter().any(|group| {
            group.side == DockSide::Float
                && group.active_kind() == Some(kind)
                && egui::Rect::from_min_size(group.pos, group.size).contains(pos)
        })
    }

    /// Same-frame: pointer is over an Outliner pane (uses last-frame dock rects + float rects).
    /// Prefer this over `ol_pointer_over` alone for Space cancel vs tree expand.
    pub(crate) fn ui2_pointer_over_outliner(&self, ctx: &egui::Context) -> bool {
        self.ui2_pointer_over_panel_kind(ctx, PanelKind::Outliner)
            || self.ui2.ol_pointer_over
    }

    pub(crate) fn ui2_pointer_over_node_editor(&self, ctx: &egui::Context) -> bool {
        self.ui2_pointer_over_panel_kind(ctx, PanelKind::NodeEditor)
    }

    pub(crate) fn ui2_draw(&mut self, ctx: &egui::Context) {
        self.ui2.left_rects.clear();
        self.ui2.right_rects.clear();
        self.ui2.top_rects.clear();
        self.ui2.bottom_rects.clear();
        // Reset each frame; `ui2_outliner` latches true when pointer is over the pane.
        // `handle_keyboard` reads the *previous* frame's value (it runs before draw).
        self.ui2.ol_pointer_over = false;
        // Keep Tool Parameters in sync with the Tools palette (egui temp = single stub bus).
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(ACTIVE_TOOL_ID), self.ui2.active_tool.clone());
            d.insert_temp(egui::Id::new(ACTIVE_PILLAR_ID), self.ui2.active_pillar.clone());
        });
        // Scoreboard drill-down for the `lay` bucket: a dense body in the scene costs ~7-8 ms
        // of layout at 3.1M tris even DESELECTED (PERF_TARGETS residual) — attribute it.
        let lay_probe = std::env::var_os("AZRA_SCOREBOARD").is_some();
        let mut lay_t = std::time::Instant::now();
        let mut lay_ms = [0f32; 6];
        let mut lap = |slot: usize, t: &mut std::time::Instant| {
            lay_ms[slot] = t.elapsed().as_secs_f32() * 1000.0;
            *t = std::time::Instant::now();
        };
        // Idle chrome: paint-only bars when pointer is in the viewport (not over menus/pills).
        // Full widget chrome only when the pointer is in the top/bottom bands or a modal is open.
        // CHROME BARS AND THE TAB STRIPS ARE ALWAYS LIVE (owner 2026-08-05, with the top bar
        // circled: "all this panel need to stay always there").
        //
        // They were hover-gated along with the docks, and that was the wrong call twice over.
        // First it kept breaking: a `TopBottomPanel` both PAINTS and RESERVES layout space, so an
        // idle path that only replays shapes without opening the panel loses the reservation, the
        // body track expands over the strip, and the bar is overdrawn — invisible until hovered,
        // exactly what was reported. Second, and the reason not to just patch that: the cost this
        // whole feature exists to remove is the DOCKS rebuilding thousands of widgets per frame
        // (the scoreboard's `library` line — tens of ms). An app bar, a ctxbar, a status bar and a
        // few tab strips are a few dozen widgets between them; gating them bought approximately
        // nothing and risked the entire top of the UI. Snapshot replay stays where the widgets
        // actually are: the pane bodies.
        self.ui2_app_bar(ctx);
        self.ui2_ctxbar(ctx);
        self.ui2_status_bar(ctx);
        if lay_probe { lap(0, &mut lay_t); }
        // Body cell AFTER top/bottom chrome, BEFORE side docks — the mockup .body track.
        let _body = ctx.available_rect();
        let al = self.ui2_dock(ctx, DockSide::Left);
        if lay_probe { lap(1, &mut lay_t); }
        let ar = self.ui2_dock(ctx, DockSide::Right);
        if lay_probe { lap(2, &mut lay_t); }
        let at = self.ui2_horizontal_dock(ctx, DockSide::Top);
        let ab = self.ui2_horizontal_dock(ctx, DockSide::Bottom);
        // Hole = egui cell AFTER L/R docks (`avail`), then NEVER expand past dock
        // edges. Prior bug: `right.max(left+40)` re-entered Properties without
        // re-clamp → library painted under the right column (owner screenshots).
        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let avail = ctx.available_rect();
        let lp = self.ui2.left_panel_rect;
        let rp = self.ui2.right_panel_rect;
        let mut left = snap(avail.left());
        let mut right = snap(avail.right());
        if lp.width() > 1.0 {
            left = left.max(snap(lp.right()));
        }
        if rp.width() > 1.0 {
            right = right.min(snap(rp.left()));
        }
        // Keep a usable hole but NEVER expand into docks.
        if right < left + 40.0 {
            right = (left + 40.0).min(snap(avail.right()));
            left = (right - 40.0).max(snap(avail.left()));
        }
        let top = snap(avail.top());
        let bottom = snap(avail.bottom());
        self.ui2.viewport_hole = egui::Rect::from_min_max(
            egui::pos2(left, top),
            egui::pos2(right, bottom),
        );
        if lay_probe { lay_t = std::time::Instant::now(); }
        let af = self.ui2_floating(ctx);
        if lay_probe { lap(3, &mut lay_t); }
        self.ui2_handle(ctx, al);
        self.ui2_handle(ctx, ar);
        self.ui2_handle(ctx, at);
        self.ui2_handle(ctx, ab);
        self.ui2_handle(ctx, af);
        if lay_probe { lap(4, &mut lay_t); }
        // Shift+A: use legacy `draw_add_popup` only (app_update). Calling
        // `ui2_add_menu` here opened a second "ADD" panel on top of it.
        // Gate 6 pies: Tab / Z / View / F / … — input still runs in handle_keyboard;
        // draw was skipped in v2 because viewport_overlays is suppressed. Paint them
        // here on a top layer so hold-to-select works again.
        self.ui2_draw_pies(ctx);
        // Save-to-Part / kitbash dialog is drawn ONCE in app_update (after ui2_draw).
        // Drawing it here as well double-processed TextEdit events → every keystroke
        // typed twice (owner: "text input register 2 time the key").
        // Debounced layout persistence — fingerprint change detection, writes
        // once the layout has settled (never per-frame file I/O).
        self.ui2_layout_autosave();
        if lay_probe {
            lap(5, &mut lay_t);
            let total: f32 = lay_ms.iter().sum();
            if total > 4.0 {
                use std::cell::Cell;
                use std::time::Instant;
                thread_local! { static LAST: Cell<Option<Instant>> = Cell::new(None); }
                let now = Instant::now();
                let fire = LAST.with(|c| {
                    let due = c.get().map(|t| now.duration_since(t).as_secs_f32() >= 2.0).unwrap_or(true);
                    if due { c.set(Some(now)); }
                    due
                });
                if fire {
                    eprintln!(
                        "[LAY] total={total:.1} chrome={:.2} dockL={:.2} dockR={:.2} float={:.2} handles={:.2} tail={:.2}",
                        lay_ms[0], lay_ms[1], lay_ms[2], lay_ms[3], lay_ms[4], lay_ms[5],
                    );
                }
            }
        }
    }
}
