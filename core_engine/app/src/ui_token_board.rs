// core_engine/app/src/ui_token_board.rs
//
// Gate-0 THROWAWAY theme-verification window. Renders every design token as a colour
// swatch with (a) the hex the CODE produces from `ui_theme::*` and (b) the hex the
// owner-approved mockup (`docs/UI/azra_app_mockup.html` :root) specifies, side by side,
// so parity is eyeballed at a glance. Plus a row of native widgets (DragValue,
// CollapsingHeader, Button, Checkbox, ComboBox, TextEdit) styled by `apply_style` so
// their base look can be compared to the mockup.
//
// Remove this file + its `mod`/call/flag once Gate 0 is signed off.

use eframe::egui::{self, Color32};
use crate::ui_theme;
use crate::FourthLayerApp;

fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// (label, code color, mockup :root target hex). When the two hexes match the row is
/// green; a mismatch shows amber so a drift is obvious.
type Row = (&'static str, Color32, &'static str);

impl FourthLayerApp {
    pub(crate) fn draw_token_board(&mut self, ctx: &egui::Context) {
        if !self.ui.show_token_board { return; }
        let mut open = true;

        let surfaces: &[Row] = &[
            ("FRAME",   ui_theme::SURFACE_FRAME,   "#161616"),
            ("APPBAR",  ui_theme::SURFACE_APPBAR,  "#181818"),
            ("BAR",     ui_theme::SURFACE_BAR,     "#161616"),
            ("TOOLCOL", ui_theme::SURFACE_TOOLCOL, "#181818"),
            ("PANEL",   ui_theme::SURFACE_PANEL,   "#1c1c1c"),
            ("CARD",    ui_theme::SURFACE_CARD,    "#222223"),
            ("HEADER",  ui_theme::SURFACE_HEADER,  "#232323"),
            ("FIELD",   ui_theme::SURFACE_FIELD,   "#282829"),
            ("CONTROL", ui_theme::SURFACE_CONTROL, "#2c2c2c"),
            ("WELL",    ui_theme::SURFACE_WELL,    "#171717"),
            ("SEAM",    ui_theme::SEAM,            "#141414"),
            ("BORDER",        ui_theme::BORDER,        "#2e2e2f"),
            ("BORDER_SUBTLE", ui_theme::BORDER_SUBTLE, "#262626"),
            ("BORDER_HOVER",  ui_theme::BORDER_HOVER,  "#3d3d3e"),
        ];
        let accents: &[Row] = &[
            ("ACCENT",       ui_theme::ACCENT,       "#7b61ff"),
            ("ACCENT_HOVER", ui_theme::ACCENT_HOVER, "#8b6fff"),
            ("ACCENT_LIGHT", ui_theme::ACCENT_LIGHT, "#a191ff"),
            ("ACCENT_DIM",   ui_theme::ACCENT_DIM,   "#5b48c4"),
            ("SELECTED_ROW", ui_theme::SELECTED_ROW, "#262340"),
        ];
        let hues: &[Row] = &[
            ("SDF",           ui_theme::SDF,           "#39d98a"),
            ("MESH",          ui_theme::MESH,          "#3e9bff"),
            ("LIGHT",         ui_theme::LIGHT,         "#ffd54a"),
            ("CAM",           ui_theme::CAM,           "#ff5555"),
            ("SCULPT_ORANGE", ui_theme::SCULPT_ORANGE, "#ff8a3d"),
            ("NURBS",         ui_theme::NURBS,         "#a191ff"),
            ("CYAN",          ui_theme::CYAN,          "#4ecdc4"),
            ("GEO",           ui_theme::GEO,           "#7486a3"),
            ("ROSE",          ui_theme::ROSE,          "#ff6bb3"),
            ("AXIS_X",        ui_theme::AXIS_X,        "#e05252"),
            ("AXIS_Y",        ui_theme::AXIS_Y,        "#7dc242"),
            ("AXIS_Z",        ui_theme::AXIS_Z,        "#4a8fe0"),
            ("DESTRUCTIVE",   ui_theme::DESTRUCTIVE,   "#e08a80"),
        ];
        let text: &[Row] = &[
            ("TEXT",           ui_theme::TEXT,           "#e6e6e6"),
            ("TEXT_SECONDARY", ui_theme::TEXT_SECONDARY, "#c8c8c8"),
            ("TEXT_DIM",       ui_theme::TEXT_DIM,       "#909090"),
            ("TEXT_SECTION",   ui_theme::TEXT_SECTION,   "#8a8a8a"),
            ("TEXT_FAINT",     ui_theme::TEXT_FAINT,     "#646464"),
        ];

        let swatches = |ui: &mut egui::Ui, rows: &[Row]| {
            for (name, col, target) in rows {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 16.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 3.0, *col);
                    ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(1.0, ui_theme::BORDER));
                    ui.add_sized([120.0, 16.0], egui::Label::new(egui::RichText::new(*name).color(ui_theme::TEXT)));
                    let code = hex(*col);
                    let ok = code.eq_ignore_ascii_case(target);
                    let mark_col = if ok { ui_theme::SDF } else { ui_theme::WARNING };
                    ui.monospace(egui::RichText::new(&code).color(ui_theme::TEXT_SECONDARY));
                    ui.label(egui::RichText::new("→").color(ui_theme::TEXT_FAINT));
                    ui.monospace(egui::RichText::new(*target).color(mark_col));
                    ui.label(egui::RichText::new(if ok { "✓" } else { "≠" }).color(mark_col));
                });
            }
        };

        egui::Window::new("Token Board — Gate 0 (code hex → mockup target)")
            .open(&mut open)
            .default_width(360.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(600.0).show(ui, |ui| {
                    ui.label(egui::RichText::new("SURFACES").color(ui_theme::TEXT_SECTION).small());
                    swatches(ui, surfaces);
                    ui.separator();
                    ui.label(egui::RichText::new("ACCENT").color(ui_theme::TEXT_SECTION).small());
                    swatches(ui, accents);
                    ui.separator();
                    ui.label(egui::RichText::new("PILLAR / SEMANTIC HUES").color(ui_theme::TEXT_SECTION).small());
                    swatches(ui, hues);
                    ui.separator();
                    ui.label(egui::RichText::new("TEXT").color(ui_theme::TEXT_SECTION).small());
                    swatches(ui, text);
                    ui.separator();

                    // Native widgets under apply_style — compare their base look to the
                    // mockup. Demo state rides egui temp memory (no UiState fields for a
                    // throwaway board).
                    ui.label(egui::RichText::new("NATIVE WIDGETS (apply_style)").color(ui_theme::TEXT_SECTION).small());
                    let id_num = egui::Id::new("tb_num");
                    let id_bool = egui::Id::new("tb_bool");
                    let id_combo = egui::Id::new("tb_combo_v");
                    let id_text = egui::Id::new("tb_text");
                    ui.horizontal(|ui| {
                        let mut n = ui.data_mut(|d| d.get_temp::<f64>(id_num).unwrap_or(0.15));
                        ui.add(egui::DragValue::new(&mut n).speed(0.01));
                        ui.data_mut(|d| d.insert_temp(id_num, n));
                        let mut b = ui.data_mut(|d| d.get_temp::<bool>(id_bool).unwrap_or(false));
                        ui.checkbox(&mut b, "Checkbox");
                        ui.data_mut(|d| d.insert_temp(id_bool, b));
                        let mut cur = ui.data_mut(|d| d.get_temp::<String>(id_combo).unwrap_or_else(|| "Union".to_owned()));
                        egui::ComboBox::from_id_source("tb_combo").selected_text(&cur).show_ui(ui, |ui| {
                            for o in ["Union", "Subtract", "Intersect"] { ui.selectable_value(&mut cur, o.to_owned(), o); }
                        });
                        ui.data_mut(|d| d.insert_temp(id_combo, cur));
                    });
                    ui.horizontal(|ui| {
                        let _ = ui.button("Button");
                        let _ = ui.selectable_label(true, "Selected");
                        let _ = ui.selectable_label(false, "Unselected");
                    });
                    let mut txt = ui.data_mut(|d| d.get_temp::<String>(id_text).unwrap_or_default());
                    if ui.add(egui::TextEdit::singleline(&mut txt).hint_text("field (extreme_bg = FIELD)")).changed() {
                        ui.data_mut(|d| d.insert_temp(id_text, txt));
                    }
                    egui::CollapsingHeader::new("CollapsingHeader")
                        .default_open(true)
                        .show(ui, |ui| { ui.label("body row on panel_fill"); });

                    ui.separator();
                    ui.label(egui::RichText::new("FIELD KIT (Gate 1) — one of each, on FIELD/SEAM tokens").color(ui_theme::TEXT_SECTION).small());
                    ui.add_space(2.0);
                    crate::ui_fields::demo(ui);

                    ui.separator();
                    ui.label(egui::RichText::new("TOOL PARAMETERS (Gate 2) — pick a tool/pillar; compare to the mockup panel").color(ui_theme::TEXT_SECTION).small());
                    ui.add_space(2.0);
                    // Stub active-tool selector (stands in for real tool/selection state).
                    let tool_id = egui::Id::new("tb_tp_tool");
                    let pil_id = egui::Id::new("tb_tp_pillar");
                    let mut tool = ui.data_mut(|d| d.get_temp::<String>(tool_id).unwrap_or_else(|| "Box".to_owned()));
                    let mut pillar = ui.data_mut(|d| d.get_temp::<String>(pil_id).unwrap_or_else(|| "sdf".to_owned()));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Tool").size(11.0).color(ui_theme::TEXT_DIM));
                        egui::ComboBox::from_id_source("tb_tool_sel").selected_text(&tool).show_ui(ui, |ui| {
                            for tset in [
                                &["Box","Sphere","Cylinder","Cone","Torus","Capsule","Plane","Slice","Union","Elongate"][..],
                                &["Cube","Bevel","Loop Cut","Merge by Distance","Inset","Solidify"][..],
                                &["Box Select","Circle Select","Move","Rotate","Clay","Draw","Paint","Bridge"][..],
                            ] { for t in tset { ui.selectable_value(&mut tool, (*t).to_owned(), *t); } }
                        });
                        ui.label(egui::RichText::new("Pillar").size(11.0).color(ui_theme::TEXT_DIM));
                        egui::ComboBox::from_id_source("tb_pillar_sel").selected_text(&pillar).show_ui(ui, |ui| {
                            for p in ["sdf","poly","nurbs"] { ui.selectable_value(&mut pillar, p.to_owned(), p); }
                        });
                    });
                    ui.data_mut(|d| { d.insert_temp(tool_id, tool.clone()); d.insert_temp(pil_id, pillar.clone()); });
                    egui::Frame::none().fill(ui_theme::SURFACE_PANEL).inner_margin(egui::Margin::symmetric(0, 4))
                        .show(ui, |ui| crate::tool_params::draw(ui, &tool, &pillar));
                });
            });

        if !open { self.ui.show_token_board = false; }
    }
}
