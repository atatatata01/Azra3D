// core_engine/app/src/ui_fields.rs
//
// Gate-1 FIELD KIT — the reusable widget vocabulary the mockup's panels are built from
// (`.tprow` / `.xyz` / `.sect` / `.subhead` / segmented enums). Free functions taking a
// `&mut egui::Ui` + `&mut value`, styled to the mockup's measured specs via `ui_theme`
// tokens ONLY. Tool Parameters, Properties and the modifier stack (later gates) all
// compose from these — one field idiom, one look.
//
// Mockup specs (measured from docs/UI/azra_app_mockup.html):
//   row height 22 · label right-aligned dim 11px · gap · side pad
//   Owner tight: label col 70 (was 96) + pad 8 (was 12) — less dead left space in
//   narrow docks without clipping long labels (they truncate in the fixed col).
//   input h23 field/seam r5 mono 11 · xyz cell + 4px axis dot
//   section / subhead caps

use eframe::egui::{self, Color32, Stroke, Vec2, Response};
use crate::ui_theme as t;

const ROW_H: f32 = 22.0;
const LABEL_W: f32 = 70.0;
const FIELD_ROUND: f32 = 5.0;
/// Horizontal inset on field rows (mockup 12; tightened for ui2 docks).
const ROW_PAD_X: f32 = 8.0;
const LABEL_GAP: f32 = 6.0;

/// Adaptive metrics so Tool Parameters still fits when the dock is at minimum width.
/// - `label_w` / `pad`: shrink dead space left of field names
/// - `decimals`: hide extra digits when XYZ cells are tiny
/// - `axis_dots`: off when narrow (and off by default for max field room)
struct FieldLayout {
    label_w: f32,
    pad: f32,
    label_gap: f32,
    decimals: usize,
    axis_dots: bool,
    font: f32,
}

fn field_layout(ui: &egui::Ui) -> FieldLayout {
    let w = ui.available_width();
    if w < 160.0 {
        FieldLayout {
            label_w: 28.0,
            pad: 3.0,
            label_gap: 3.0,
            decimals: 0,
            axis_dots: false,
            font: 10.0,
        }
    } else if w < 210.0 {
        FieldLayout {
            label_w: 42.0,
            pad: 4.0,
            label_gap: 4.0,
            decimals: 1,
            axis_dots: false,
            font: 10.5,
        }
    } else if w < 260.0 {
        FieldLayout {
            label_w: 54.0,
            pad: 6.0,
            label_gap: 5.0,
            decimals: 2,
            axis_dots: false,
            font: 11.0,
        }
    } else {
        FieldLayout {
            label_w: LABEL_W,
            pad: ROW_PAD_X,
            label_gap: LABEL_GAP,
            decimals: 2,
            // Axis colour dots cost ~10px per cell — keep off for denser XYZ rows.
            axis_dots: false,
            font: 11.0,
        }
    }
}

/// Right-aligned dim label. Width adapts to panel size (no fixed dead zone when narrow).
/// CAP-ONLY: fixed allocate + clip so long labels never expand the dock.
fn row_label(ui: &mut egui::Ui, label: &str) {
    let lay = field_layout(ui);
    row_label_w(ui, label, lay.label_w, lay.font);
}

fn row_label_w(ui: &mut egui::Ui, label: &str, label_w: f32, font: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(label_w, ROW_H), egui::Sense::hover());
    let prev = ui.clip_rect();
    ui.set_clip_rect(rect.intersect(prev));
    // Narrow: left-align short labels to reclaim right-side field space.
    let (anchor, pos) = if label_w < 40.0 {
        (egui::Align2::LEFT_CENTER, rect.left_center() + Vec2::new(1.0, 0.0))
    } else {
        (egui::Align2::RIGHT_CENTER, rect.right_center())
    };
    ui.painter().text(
        pos,
        anchor,
        label,
        egui::FontId::new(font, egui::FontFamily::Proportional),
        t::TEXT_DIM,
    );
    ui.set_clip_rect(prev);
}

/// Scope a child ui so a `DragValue`/`TextEdit` inside reads like a mockup input field:
/// FIELD fill, 1px SEAM border, r5, mono numerals, no hover lift (only focus/active
/// borders accent). Restores on drop.
fn field_style(ui: &mut egui::Ui) {
    let r = egui::Rounding::same(FIELD_ROUND as u8);
    let seam = Stroke::new(1.0, t::SEAM);
    let v = &mut ui.visuals_mut().widgets;
    for w in [&mut v.inactive, &mut v.hovered] {
        w.weak_bg_fill = t::SURFACE_FIELD;
        w.bg_fill = t::SURFACE_FIELD;
        w.bg_stroke = seam;
        w.corner_radius = r;
        w.expansion = 0.0;
    }
    // active / open (dragging, editing) — border goes accent, fill stays FIELD.
    for w in [&mut v.active, &mut v.open] {
        w.weak_bg_fill = t::SURFACE_FIELD;
        w.bg_fill = t::SURFACE_FIELD;
        w.bg_stroke = Stroke::new(1.0, t::ACCENT);
        w.corner_radius = r;
        w.expansion = 0.0;
    }
    // numerals are mono in the mockup
    if let Some(fid) = ui.style_mut().text_styles.get_mut(&egui::TextStyle::Button) {
        fid.family = egui::FontFamily::Monospace;
        fid.size = 11.0;
    }
}

/// Width left for the field after label + gap, **minus** the right inset so
/// numbers never paint under the panel edge / scroll thumb.
fn field_w(ui: &egui::Ui) -> f32 {
    let lay = field_layout(ui);
    (ui.available_width() - lay.pad).max(20.0)
}

/// `.tprow` with a numeric `DragValue` (drag-scrub · click-to-type · Shift-fine).
pub fn num(ui: &mut egui::Ui, label: &str, v: &mut f32, speed: f32, range: std::ops::RangeInclusive<f32>, unit: Option<&str>) -> Response {
    let lay = field_layout(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;   // gaps placed by hand → available_width is exact
        ui.add_space(lay.pad);
        row_label_w(ui, label, lay.label_w, lay.font);
        ui.add_space(lay.label_gap);
        let mut dv = egui::DragValue::new(v)
            .speed(speed as f64)
            .range(range)
            .fixed_decimals(lay.decimals);
        // Hide unit suffix when very tight — it steals digit room.
        if let Some(u) = unit {
            if lay.label_w >= 42.0 {
                dv = dv.suffix(format!(" {u}"));
            }
        }
        ui.scope(|ui| {
            field_style(ui);
            if let Some(fid) = ui.style_mut().text_styles.get_mut(&egui::TextStyle::Button) {
                fid.size = lay.font;
            }
            ui.add_sized([field_w(ui), 23.0], dv)
        })
        .inner
    })
    .inner
}

/// Slider that accepts **any typed number** and remaps the soft range to include it.
/// `soft_min`/`soft_max` are the default band; when `*v` leaves the band the upper
/// (and lower, if needed) expand with ~25% headroom so the thumb stays usable.
/// Uses `SliderClamping::Never` so typing past the ends is allowed.
pub fn unbound_slider(
    ui: &mut egui::Ui,
    text: &str,
    v: &mut f32,
    soft_min: f32,
    soft_max: f32,
) -> Response {
    let mut lo = soft_min;
    let mut hi = soft_max.max(soft_min + 1e-6);
    if *v > hi {
        hi = (*v * 1.25).max(*v + 1e-4);
    }
    if *v < lo {
        // Expand downward; keep non-negative floors at 0 when soft_min was ≥ 0.
        if soft_min >= 0.0 {
            lo = 0.0;
            if *v < 0.0 {
                lo = (*v * 1.25).min(*v - 1e-4);
            }
        } else {
            lo = (*v * 1.25).min(*v - 1e-4);
        }
    }
    // Always include the live value so the handle is on-track.
    lo = lo.min(*v);
    hi = hi.max(*v).max(lo + 1e-6);
    ui.add(
        egui::Slider::new(v, lo..=hi)
            .text(text)
            .clamping(egui::SliderClamping::Never)
            .smart_aim(false),
    )
}

/// Integer `.tprow` (segment counts, subdivisions).
pub fn int(ui: &mut egui::Ui, label: &str, v: &mut i32, range: std::ops::RangeInclusive<i32>) -> Response {
    let lay = field_layout(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(lay.pad);
        row_label_w(ui, label, lay.label_w, lay.font);
        ui.add_space(lay.label_gap);
        ui.scope(|ui| {
            field_style(ui);
            ui.add_sized(
                [field_w(ui), 23.0],
                egui::DragValue::new(v).speed(0.2).range(range),
            )
        })
        .inner
    })
    .inner
}

/// `.tprow.chk` — checkbox + left-aligned label (label right of the box, per the mockup).
/// The label `.truncate()`s so a long one ("Sélection au travers") clips instead of
/// growing the row past the panel width (black-band class).
pub fn bool_row(ui: &mut egui::Ui, label: &str, v: &mut bool) -> Response {
    let lay = field_layout(ui);
    ui.horizontal(|ui| {
        // Align box under the field column: L pad + label + gap − checkbox width.
        ui.add_space((lay.pad + lay.label_w + lay.label_gap - 16.0).max(lay.pad));
        let r = ui.checkbox(v, "");
        ui.add(
            egui::Label::new(
                egui::RichText::new(label).size(lay.font).color(t::TEXT_SECONDARY),
            )
            .truncate(),
        );
        ui.add_space(lay.pad);
        r
    })
    .inner
}

/// Segmented enum (`.tprow` + `.seg`) when the option count is small (≤3 in the mockup):
/// equal-width buttons, selected = SELECTED_ROW fill + ACCENT border. Returns true if
/// the selection changed. `options` is (display, value); `cur` is the live value.
pub fn enum_seg<T: PartialEq + Clone>(ui: &mut egui::Ui, label: &str, cur: &mut T, options: &[(&str, T)]) -> bool {
    let mut changed = false;
    let lay = field_layout(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(lay.pad);
        row_label_w(ui, label, lay.label_w, lay.font);
        ui.add_space(lay.label_gap);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let n = options.len().max(1) as f32;
            let avail = (ui.available_width() - lay.pad).max(12.0);
            let w = ((avail - 2.0 * (n - 1.0)) / n).max(12.0);
            for (disp, val) in options {
                let on = cur == val;
                let (fill, border, text) = if on {
                    (t::SELECTED_ROW, t::ACCENT, t::TEXT)
                } else {
                    (t::SURFACE_FIELD, t::SEAM, t::TEXT_DIM)
                };
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 23.0), egui::Sense::click());
                ui.painter().rect(rect, egui::Rounding::same(4), fill, Stroke::new(1.0, border), egui::StrokeKind::Middle);
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, *disp,
                    egui::FontId::new(10.0, egui::FontFamily::Proportional), text);
                if resp.clicked() && !on { *cur = val.clone(); changed = true; }
            }
        });
    });
    changed
}

/// Enum as a `.tprow` + `ComboBox` — the mockup's choice when there are >3 options
/// (Direction, Operation, AxisConstraint, Falloff). Styled like a field. `cur` holds the
/// selected string; returns true on change.
pub fn enum_combo(ui: &mut egui::Ui, label: &str, cur: &mut String, opts: &[&str]) -> bool {
    enum_combo_id(ui, ("tp_combo", label), label, cur, opts)
}

/// Like [`enum_combo`] but with an explicit egui id (use when the same label appears on
/// multiple rows/prims — otherwise ComboBox state desyncs across selection changes).
pub fn enum_combo_id(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    label: &str,
    cur: &mut String,
    opts: &[&str],
) -> bool {
    let mut changed = false;
    let lay = field_layout(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(lay.pad);
        row_label_w(ui, label, lay.label_w, lay.font);
        ui.add_space(lay.label_gap);
        ui.scope(|ui| {
            field_style(ui);
            let w = field_w(ui).max(24.0); // combo caret lives inside this width
            egui::ComboBox::from_id_source(id_salt)
                .selected_text(cur.clone())
                .width(w)
                .show_ui(ui, |ui| {
                    for o in opts {
                        if ui.selectable_value(cur, (*o).to_owned(), *o).changed() {
                            changed = true;
                        }
                    }
                });
        });
    });
    changed
}

/// Drag-lifecycle summary of a multi-cell row — the union of the cells'
/// egui `Response`s, so callers can route undo correctly (episode on drag
/// start, one-shot snapshot on typed edits). Plain data, deliberately NOT
/// `#[must_use]`: existing value-diff callers (Properties pages) may ignore
/// it and stay valid.
#[derive(Clone, Copy, Default)]
pub struct RowSignals {
    /// Any cell reported `changed()` this frame.
    pub changed: bool,
    /// Any cell began a drag this frame (`drag_started()`).
    pub drag_started: bool,
    /// A drag ended this frame (`drag_stopped()`) — all cells are up
    /// (egui has at most one active drag at a time).
    pub drag_stopped: bool,
}

impl RowSignals {
    fn absorb(&mut self, r: &Response) {
        self.changed |= r.changed();
        self.drag_started |= r.drag_started();
        self.drag_stopped |= r.drag_stopped();
    }
}

/// A single XYZ cell: FIELD frame + optional axis-colour dot + a `DragValue`.
/// CAP-ONLY: `allocate_exact_size(w)` reserves exactly `w` (never grows the parent).
/// Dots are omitted when narrow so digits keep the full cell.
/// Returns the `DragValue`'s `Response` — drag lifecycle must reach callers.
fn vec_cell(ui: &mut egui::Ui, w: f32, axis: Color32, v: &mut f32, decimals: usize, show_dot: bool) -> Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 23.0), egui::Sense::hover());
    ui.painter().rect(
        rect,
        egui::Rounding::same(FIELD_ROUND as u8),
        t::SURFACE_FIELD,
        Stroke::new(1.0, t::SEAM), egui::StrokeKind::Middle,
    );
    let left_inset = if show_dot {
        ui.painter()
            .circle_filled(egui::pos2(rect.left() + 7.0, rect.center().y), 2.0, axis);
        12.0
    } else {
        4.0 // no dead zone for the missing dot
    };
    let dv_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + left_inset, rect.top()),
        egui::pos2((rect.right() - 2.0).max(rect.left() + left_inset + 4.0), rect.bottom()),
    );
    ui.put(
        dv_rect,
        egui::DragValue::new(v)
            .speed(0.01)
            .fixed_decimals(decimals),
    )
}

/// `.xyz` — three equal cells (Offset / Scale / Half extents…).
/// Narrow docks: no axis dots, fewer decimals, tighter padding.
/// Returns the union of the three cells' drag/change signals so callers can
/// use the episode undo pattern (begin on `drag_started`, snapshot on typed).
pub fn vec3_row(ui: &mut egui::Ui, label: &str, v: &mut [f32; 3]) -> RowSignals {
    let lay = field_layout(ui);
    if !label.is_empty() {
        subhead(ui, label);
    }
    ui.horizontal(|ui| {
        ui.add_space(lay.pad);
        let gap = if lay.pad <= 4.0 { 2.0 } else { 4.0 };
        ui.spacing_mut().item_spacing.x = gap;
        {
            let vis = &mut ui.visuals_mut().widgets;
            for wg in [&mut vis.inactive, &mut vis.hovered, &mut vis.active, &mut vis.open] {
                wg.bg_stroke = Stroke::NONE;
                wg.weak_bg_fill = Color32::TRANSPARENT;
                wg.bg_fill = Color32::TRANSPARENT;
            }
        }
        if let Some(fid) = ui.style_mut().text_styles.get_mut(&egui::TextStyle::Button) {
            fid.family = egui::FontFamily::Monospace;
            fid.size = lay.font;
        }
        // L pad spent; reserve R pad + gaps between three cells.
        let avail = (ui.available_width() - lay.pad - gap * 2.0).max(30.0);
        let w = (avail / 3.0).max(18.0);
        let mut sig = RowSignals::default();
        sig.absorb(&vec_cell(ui, w, t::AXIS_X, &mut v[0], lay.decimals, lay.axis_dots));
        sig.absorb(&vec_cell(ui, w, t::AXIS_Y, &mut v[1], lay.decimals, lay.axis_dots));
        sig.absorb(&vec_cell(ui, w, t::AXIS_Z, &mut v[2], lay.decimals, lay.axis_dots));
        ui.add_space(lay.pad);
        sig
    })
    .inner
}

/// The X/Y/Z lock triad (`.axlock`) — three toggles tinted their axis colour when on.
pub fn axis_triad(ui: &mut egui::Ui, label: &str, v: &mut [bool; 3]) {
    let lay = field_layout(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(lay.pad);
        row_label_w(ui, label, lay.label_w, lay.font);
        ui.add_space(lay.label_gap);
        let axes = [("X", t::AXIS_X), ("Y", t::AXIS_Y), ("Z", t::AXIS_Z)];
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let avail = (ui.available_width() - lay.pad - 6.0).max(36.0);
            let w = (avail / 3.0).max(12.0);
            for (i, (name, col)) in axes.iter().enumerate() {
                let on = v[i];
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 23.0), egui::Sense::click());
                let (fill, border, text) = if on {
                    (Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 70), *col, Color32::WHITE)
                } else {
                    (t::SURFACE_FIELD, t::SEAM, t::TEXT_DIM)
                };
                ui.painter().rect(rect, egui::Rounding::same(4), fill, Stroke::new(1.0, border), egui::StrokeKind::Middle);
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, *name,
                    egui::FontId::new(11.0, egui::FontFamily::Proportional), text);
                if resp.clicked() { v[i] = !v[i]; }
            }
        });
    });
}

/// `.subhead` — a small uppercase section sub-label. `.truncate()` so it can't grow the row.
pub fn subhead(ui: &mut egui::Ui, text: &str) {
    let lay = field_layout(ui);
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(lay.pad);
        ui.add(
            egui::Label::new(
                egui::RichText::new(text.to_uppercase())
                    .size(9.0)
                    .color(t::TEXT_SECTION)
                    .extra_letter_spacing(0.4),
            )
            .truncate(),
        );
    });
}

/// `.sect` — a collapsible titled section: caret + uppercase caps label + optional right
/// badge, animated. `add_body` renders the fields inside.
pub fn section(ui: &mut egui::Ui, id: &str, title: &str, badge: Option<&str>, add_body: impl FnOnce(&mut egui::Ui)) {
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(), egui::Id::new(("ui_fields_sect", id)), true);
    let title_txt = |ui: &mut egui::Ui| {
        ui.add(egui::Label::new(egui::RichText::new(title.to_uppercase()).size(9.5)
            .color(t::TEXT_SECTION).extra_letter_spacing(0.7)).truncate());
    };
    // Tighten collapsing body so field rows pack denser in the left dock.
    ui.spacing_mut().item_spacing.y = 2.0;
    state.show_header(ui, |ui| {
        ui.add_space(ROW_PAD_X);
        // Badge first (right-aligned), then the title fills — and TRUNCATES into — what's
        // left, so a long section name can never grow the header past the panel width.
        if let Some(b) = badge {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(b).size(9.5).color(t::TEXT_FAINT).monospace());
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), title_txt);
            });
        } else {
            title_txt(ui);
        }
    }).body(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        add_body(ui);
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Color swatch + custom FIXED-WIDTH popup.
//
// WHY custom: egui 0.29's `color_edit_button_srgba` popup (`srgba_edit_ui`,
// color_picker.rs:399) builds `DragValue::new(r).prefix("R ")` with NO fixed
// width, so the R/G/B/A fields auto-size to their digit count (246 vs 6 vs 128)
// and the whole popup — an Order::Foreground Area we cannot reach into — reflows
// every drag frame → visible wobble. Wrapping the trigger in a fixed container
// does NOT help (the popup is a separate Area). So we own the popup.
//
// Fixed-width enforcement: every numeric cell is `ui.add_sized([FIELD_W, H], …)`,
// so the cell rect is CONSTANT regardless of value (1 vs 255). The SV square,
// hue bar and alpha bar are all `allocate_exact_size` at fixed dims. Nothing in
// the popup depends on a value's string length → zero reflow while dragging.
//
// srgb/linear: this helper works entirely in srgba u8 space (what the swatch and
// numeric fields show). Material sites convert with the SAME shared helpers they
// already use (`material_editor::linear_vec3_to_color32` / `color32_to_linear_vec3`),
// so the on-screen↔stored convention is byte-identical to before.

const SWATCH_SIZE: Vec2 = Vec2::new(28.0, 16.0);
const SV_SIZE: Vec2 = Vec2::new(180.0, 120.0);
const BAR_H: f32 = 14.0;
const CELL_W: f32 = 44.0; // fixed numeric-cell width — constant for any 0..=255 value
const CELL_H: f32 = 20.0;

/// srgba swatch button that opens OUR fixed-width popup. `alpha` enables the A
/// row + alpha bar (checkerboard). Returns a `Response` whose `.changed()` is set
/// the frames the color was edited. The popup persists its Hsva across frames so
/// hue survives at s=0 / v=0 (same reason egui caches it).
pub fn color_button_srgba(ui: &mut egui::Ui, srgba: &mut Color32, alpha: bool) -> Response {
    // Trigger swatch — keep the 28x16 look of the old inline button.
    let (rect, mut resp) = ui.allocate_exact_size(SWATCH_SIZE, egui::Sense::click());
    let p = ui.painter();
    // checker under the swatch so alpha reads, then the color over it.
    paint_checker(&p, rect);
    p.rect_filled(rect, egui::Rounding::same(3), *srgba);
    p.rect_stroke(rect, egui::Rounding::same(3), Stroke::new(1.0, t::SEAM), egui::StrokeKind::Middle);

    let popup_id = ui.make_persistent_id(("fl_color_popup", resp.id));
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }

    if ui.memory(|m| m.is_popup_open(popup_id)) {
        // Load / seed the Hsva picker state (keeps hue stable at grays).
        let hsva_key = egui::Id::new(("fl_color_hsva", resp.id));
        let mut hsva: egui::ecolor::Hsva = ui.data_mut(|d| {
            d.get_temp(hsva_key)
                .unwrap_or_else(|| egui::ecolor::Hsva::from_srgba_unmultiplied(srgba.to_array()))
        });
        // If the stored color changed underneath us (e.g. undo), resync.
        {
            let cur = egui::ecolor::Hsva::from_srgba_unmultiplied(srgba.to_array());
            if cur.to_srgba_unmultiplied() != hsva.to_srgba_unmultiplied() {
                hsva = cur;
            }
        }

        let area = egui::Area::new(popup_id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_bottom() + Vec2::new(0.0, 4.0))
            .constrain(true);
        let area_resp = area.show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .fill(t::SURFACE_PANEL)
                .stroke(Stroke::new(1.0, t::SEAM))
                .show(ui, |ui| {
                    color_popup_body(ui, &mut hsva, alpha)
                })
                .inner
        });

        let changed = area_resp.inner;
        if changed {
            let [r, g, b, a] = hsva.to_srgba_unmultiplied();
            *srgba = if alpha { Color32::from_rgba_unmultiplied(r, g, b, a) }
                     else { Color32::from_rgb(r, g, b) };
            resp.mark_changed();
        }
        ui.data_mut(|d| d.insert_temp(hsva_key, hsva));

        // Standard close-on-click-outside / Esc pattern.
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            || (area_resp.response.clicked_elsewhere() && resp.clicked_elsewhere())
        {
            ui.memory_mut(|m| m.close_popup(popup_id));
        }
    }
    resp
}

/// Linear-Vec3 convenience for the material sites: converts with the shared
/// `material_editor` helpers so the srgb↔linear convention is identical to the
/// code it replaces. Alpha is not applicable (rgb material colors).
pub fn color_button_linear_vec3(ui: &mut egui::Ui, v: &mut [f32; 3]) -> Response {
    let mut c32 = crate::material_editor::linear_vec3_to_color32(glam::Vec3::from(*v));
    let resp = color_button_srgba(ui, &mut c32, false);
    if resp.changed() {
        *v = crate::material_editor::color32_to_linear_vec3(c32).into();
    }
    resp
}

/// Popup interior: SV square + hue bar (+ alpha bar) + a FIXED-WIDTH R/G/B(/A)
/// numeric row. Returns true if any control changed the color this frame.
fn color_popup_body(ui: &mut egui::Ui, hsva: &mut egui::ecolor::Hsva, alpha: bool) -> bool {
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
    let mut changed = false;

    // 2D saturation / value square (x = saturation, y = value, top = bright).
    changed |= sv_square(ui, hsva);
    // Hue bar 0..1.
    changed |= hue_bar(ui, hsva);
    if alpha {
        changed |= alpha_bar(ui, hsva);
    }

    // FIXED-WIDTH numeric row — the whole point. Each cell is add_sized at CELL_W
    // so its rect never depends on the digit count.
    let [mut r, mut g, mut b, mut a] = hsva.to_srgba_unmultiplied();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let mut cell_changed = false;
        {
            let vis = &mut ui.visuals_mut().widgets;
            for w in [&mut vis.inactive, &mut vis.hovered, &mut vis.active, &mut vis.open] {
                w.weak_bg_fill = t::SURFACE_FIELD;
                w.bg_fill = t::SURFACE_FIELD;
                w.bg_stroke = Stroke::new(1.0, t::SEAM);
                w.corner_radius = egui::Rounding::same(FIELD_ROUND as u8);
                w.expansion = 0.0;
            }
        }
        for (lbl, val) in [("R", &mut r), ("G", &mut g), ("B", &mut b)] {
            ui.label(egui::RichText::new(lbl).size(10.0).color(t::TEXT_DIM));
            cell_changed |= ui
                .add_sized([CELL_W, CELL_H], egui::DragValue::new(val).range(0..=255).speed(0.5))
                .changed();
        }
        if alpha {
            ui.label(egui::RichText::new("A").size(10.0).color(t::TEXT_DIM));
            cell_changed |= ui
                .add_sized([CELL_W, CELL_H], egui::DragValue::new(&mut a).range(0..=255).speed(0.5))
                .changed();
        }
        if cell_changed {
            *hsva = egui::ecolor::Hsva::from_srgba_unmultiplied([r, g, b, if alpha { a } else { 255 }]);
            changed = true;
        }
    });

    changed
}

/// 2D S/V picker. Fixed size; background is the hue's SV gradient (two layered
/// linear gradients approximated with a fine mesh would be heavier — we paint a
/// coarse gradient via per-column value ramps which is plenty for a small square).
fn sv_square(ui: &mut egui::Ui, hsva: &mut egui::ecolor::Hsva) -> bool {
    let (rect, resp) = ui.allocate_exact_size(SV_SIZE, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    // Paint the gradient as a mesh: 4 corners — white, hue, black, black.
    use egui::epaint::{Mesh, Vertex};
    let hue_color: Color32 = egui::ecolor::Hsva::new(hsva.h, 1.0, 1.0, 1.0).into();
    let mut mesh = Mesh::default();
    // top-left white, top-right hue, bottom-left black, bottom-right black.
    let cols = [Color32::WHITE, hue_color, Color32::BLACK, Color32::BLACK];
    let corners = [rect.left_top(), rect.right_top(), rect.left_bottom(), rect.right_bottom()];
    for (pos, col) in corners.iter().zip(cols) {
        mesh.vertices.push(Vertex { pos: *pos, uv: egui::epaint::WHITE_UV, color: col });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 2, 1, 3]);
    painter.add(mesh);
    painter.rect_stroke(rect, egui::Rounding::ZERO, Stroke::new(1.0, t::SEAM), egui::StrokeKind::Middle);

    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let s = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let v = 1.0 - ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            hsva.s = s;
            hsva.v = v;
            changed = true;
        }
    }
    // Cursor marker.
    let cx = rect.left() + hsva.s * rect.width();
    let cy = rect.top() + (1.0 - hsva.v) * rect.height();
    let ring = if hsva.v > 0.5 { Color32::BLACK } else { Color32::WHITE };
    painter.circle_stroke(egui::pos2(cx, cy), 4.0, Stroke::new(1.5, ring));
    changed
}

/// Horizontal hue bar 0..1. Fixed height, full width of the SV square.
fn hue_bar(ui: &mut egui::Ui, hsva: &mut egui::ecolor::Hsva) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(SV_SIZE.x, BAR_H), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    // Paint hue as vertical strips.
    let n = 48usize;
    for i in 0..n {
        let t0 = i as f32 / n as f32;
        let x0 = rect.left() + t0 * rect.width();
        let x1 = rect.left() + (i + 1) as f32 / n as f32 * rect.width();
        let col: Color32 = egui::ecolor::Hsva::new(t0, 1.0, 1.0, 1.0).into();
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())), egui::Rounding::ZERO, col);
    }
    painter.rect_stroke(rect, egui::Rounding::ZERO, Stroke::new(1.0, t::SEAM), egui::StrokeKind::Middle);

    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            hsva.h = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            changed = true;
        }
    }
    let mx = rect.left() + hsva.h * rect.width();
    painter.rect_stroke(
        egui::Rect::from_center_size(egui::pos2(mx, rect.center().y), Vec2::new(3.0, rect.height())),
        egui::Rounding::ZERO, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Middle);
    changed
}

/// Alpha bar over a checkerboard. Fixed height.
fn alpha_bar(ui: &mut egui::Ui, hsva: &mut egui::ecolor::Hsva) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(SV_SIZE.x, BAR_H), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    paint_checker(&painter, rect);
    let opaque: Color32 = egui::ecolor::Hsva::new(hsva.h, hsva.s, hsva.v, 1.0).into();
    // left transparent → right opaque
    use egui::epaint::{Mesh, Vertex};
    let mut mesh = Mesh::default();
    let clear = Color32::from_rgba_unmultiplied(opaque.r(), opaque.g(), opaque.b(), 0);
    let cols = [clear, opaque, clear, opaque];
    let corners = [rect.left_top(), rect.right_top(), rect.left_bottom(), rect.right_bottom()];
    for (pos, col) in corners.iter().zip(cols) {
        mesh.vertices.push(Vertex { pos: *pos, uv: egui::epaint::WHITE_UV, color: col });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 2, 1, 3]);
    painter.add(mesh);
    painter.rect_stroke(rect, egui::Rounding::ZERO, Stroke::new(1.0, t::SEAM), egui::StrokeKind::Middle);

    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            hsva.a = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            changed = true;
        }
    }
    let mx = rect.left() + hsva.a * rect.width();
    painter.rect_stroke(
        egui::Rect::from_center_size(egui::pos2(mx, rect.center().y), Vec2::new(3.0, rect.height())),
        egui::Rounding::ZERO, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Middle);
    changed
}

/// Small checkerboard so alpha reads under swatches / the alpha bar.
fn paint_checker(painter: &egui::Painter, rect: egui::Rect) {
    let s = 4.0;
    let (c0, c1) = (Color32::from_gray(90), Color32::from_gray(140));
    painter.rect_filled(rect, egui::Rounding::same(2), c0);
    let cols = (rect.width() / s).ceil() as i32;
    let rows = (rect.height() / s).ceil() as i32;
    for r in 0..rows {
        for c in 0..cols {
            if (r + c) % 2 == 0 { continue; }
            let x0 = rect.left() + c as f32 * s;
            let y0 = rect.top() + r as f32 * s;
            let cell = egui::Rect::from_min_max(
                egui::pos2(x0, y0),
                egui::pos2((x0 + s).min(rect.right()), (y0 + s).min(rect.bottom())),
            );
            painter.rect_filled(cell, egui::Rounding::ZERO, c1);
        }
    }
}

// ── Gate-1 demo — one of each field, bound to egui temp memory (no struct fields).
// Call from the throwaway board; delete with it. The KIT above is permanent.
pub fn demo(ui: &mut egui::Ui) {
    let id = |k: &str| egui::Id::new(("ui_fields_demo", k));
    section(ui, "dims", "Dimensions", Some("local"), |ui| {
        let mut he = ui.data_mut(|d| d.get_temp::<[f32;3]>(id("he")).unwrap_or([0.4,0.4,0.4]));
        vec3_row(ui, "Half extents", &mut he);
        ui.data_mut(|d| d.insert_temp(id("he"), he));
        let mut r = ui.data_mut(|d| d.get_temp::<f32>(id("r")).unwrap_or(0.4));
        num(ui, "Radius", &mut r, 0.01, 0.0..=5.0, None);
        ui.data_mut(|d| d.insert_temp(id("r"), r));
        let mut seg = ui.data_mut(|d| d.get_temp::<i32>(id("seg")).unwrap_or(1));
        int(ui, "Segments", &mut seg, 1..=128);
        ui.data_mut(|d| d.insert_temp(id("seg"), seg));
    });
    section(ui, "corners", "Corners", Some("local"), |ui| {
        let mut ch = ui.data_mut(|d| d.get_temp::<bool>(id("ch")).unwrap_or(false));
        bool_row(ui, "Chamfer", &mut ch);
        ui.data_mut(|d| d.insert_temp(id("ch"), ch));
    });
    section(ui, "boolean", "Boolean", Some("local"), |ui| {
        let mut op = ui.data_mut(|d| d.get_temp::<String>(id("op")).unwrap_or_else(|| "Union".into()));
        let opts = [("Union","Union".to_string()),("Subtract","Subtract".to_string()),("Intersect","Intersect".to_string())];
        if enum_seg(ui, "Operation", &mut op, &opts) {}
        ui.data_mut(|d| d.insert_temp(id("op"), op));
        let mut k = ui.data_mut(|d| d.get_temp::<f32>(id("k")).unwrap_or(0.15));
        num(ui, "Blend K", &mut k, 0.01, 0.0..=1.0, None);
        ui.data_mut(|d| d.insert_temp(id("k"), k));
    });
    section(ui, "transform", "Transform", Some("local"), |ui| {
        for (key, name, def) in [("off","Offset",[0.0,0.0,0.0]),("rot","Rotation",[0.0,0.0,0.0]),("scl","Scale",[1.0,1.0,1.0])] {
            let mut vv = ui.data_mut(|d| d.get_temp::<[f32;3]>(id(key)).unwrap_or(def));
            vec3_row(ui, name, &mut vv);
            ui.data_mut(|d| d.insert_temp(id(key), vv));
        }
        let mut ax = ui.data_mut(|d| d.get_temp::<[bool;3]>(id("ax")).unwrap_or([false,false,false]));
        axis_triad(ui, "Lock", &mut ax);
        ui.data_mut(|d| d.insert_temp(id("ax"), ax));
    });
}
