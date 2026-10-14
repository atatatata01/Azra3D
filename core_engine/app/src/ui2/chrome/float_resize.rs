//! Eight-handle resize for floating panels — one API for all float hosts.

use eframe::egui;

/// Resize outcome: position delta + size delta.
#[derive(Clone, Copy, Debug, Default)]
pub struct FloatResizeDelta {
    pub pos: egui::Vec2,
    pub size: egui::Vec2,
}

/// Draw 8 edge/corner handles. Purple on hover. Returns drag delta if any.
pub fn draw_handles(
    ui: &mut egui::Ui,
    id: u64,
    pos: egui::Pos2,
    size: egui::Vec2,
) -> Option<FloatResizeDelta> {
    const PAD: f32 = 6.0;
    let rect = egui::Rect::from_min_size(pos, size);
    let mut out = None;
    for (dir, r) in handle_rects(rect, PAD) {
        let resp = ui.interact(
            r,
            egui::Id::new(("ui2_float_rz", id, dir)),
            egui::Sense::drag(),
        );
        if resp.hovered() || resp.dragged() {
            ui.painter().rect_filled(r, 2.0, crate::ui_theme::ACCENT);
            ui.output_mut(|o| {
                o.cursor_icon = match dir {
                    "e" | "w" => egui::CursorIcon::ResizeHorizontal,
                    "n" | "s" => egui::CursorIcon::ResizeVertical,
                    "ne" | "sw" => egui::CursorIcon::ResizeNeSw,
                    _ => egui::CursorIcon::ResizeNwSe,
                };
            });
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            let mut dp = egui::Vec2::ZERO;
            let mut ds = egui::Vec2::ZERO;
            if dir.contains('e') {
                ds.x += d.x;
            }
            if dir.contains('s') {
                ds.y += d.y;
            }
            if dir.contains('w') {
                dp.x += d.x;
                ds.x -= d.x;
            }
            if dir.contains('n') {
                dp.y += d.y;
                ds.y -= d.y;
            }
            out = Some(FloatResizeDelta { pos: dp, size: ds });
        }
    }
    out
}

fn handle_rects(rect: egui::Rect, pad: f32) -> [(&'static str, egui::Rect); 8] {
    let t = rect.top();
    let b = rect.bottom();
    let l = rect.left();
    let r = rect.right();
    let cx = rect.center().x;
    let cy = rect.center().y;
    [
        ("n", egui::Rect::from_center_size(egui::pos2(cx, t), egui::vec2(rect.width() - 2.0 * pad, pad))),
        ("s", egui::Rect::from_center_size(egui::pos2(cx, b), egui::vec2(rect.width() - 2.0 * pad, pad))),
        ("w", egui::Rect::from_center_size(egui::pos2(l, cy), egui::vec2(pad, rect.height() - 2.0 * pad))),
        ("e", egui::Rect::from_center_size(egui::pos2(r, cy), egui::vec2(pad, rect.height() - 2.0 * pad))),
        ("nw", egui::Rect::from_center_size(egui::pos2(l, t), egui::vec2(pad, pad))),
        ("ne", egui::Rect::from_center_size(egui::pos2(r, t), egui::vec2(pad, pad))),
        ("sw", egui::Rect::from_center_size(egui::pos2(l, b), egui::vec2(pad, pad))),
        ("se", egui::Rect::from_center_size(egui::pos2(r, b), egui::vec2(pad, pad))),
    ]
}
