// core_engine/app/src/ui_theme.rs
//
// AZRA design tokens + base egui style — the code authority for the app's look.
// VALUES are reconciled (Gate 0, 2026-07-17) to the owner-approved mockup
// `docs/UI/azra_app_mockup.html` :root — the mockup is the visual source of truth and
// was iterated lighter than the original handoff ("compress the 3 dark grays"). No
// panel may hard-code a color/size/font; reference these constants instead.

use eframe::egui::{self, Color32};

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// ────────────────────────────────────────────────────────────────────────────
// Background stack (dark → light). Exact hex from the handoff.
// ────────────────────────────────────────────────────────────────────────────
// Reconciled to mockup :root (2026-07-17). Changed from handoff: FRAME 0e→16, APPBAR
// 14→18, CARD 202024→222223, FIELD 26262b→282829, WELL 10→17. Unchanged (already
// matched): BAR, TOOLCOL, PANEL, HEADER, CONTROL, SELECTED_ROW.
pub const SURFACE_FRAME:   Color32 = rgb(0x161616); // app frame            (mockup --frame)
pub const SURFACE_APPBAR:  Color32 = rgb(0x181818); // app bar              (mockup --appbar)
pub const SURFACE_BAR:     Color32 = rgb(0x161616); // context bar / bars   (mockup --bar)
pub const SURFACE_TOOLCOL: Color32 = rgb(0x181818); // tool column          (mockup --toolcol)
pub const SURFACE_PANEL:   Color32 = rgb(0x1c1c1c); // viewport / panel body(mockup --panel)
pub const SURFACE_CARD:    Color32 = rgb(0x222223); // cards, menus, popovers(mockup --card)
pub const SURFACE_HEADER:  Color32 = rgb(0x232323); // panel headers, rows  (mockup --header)
pub const SURFACE_FIELD:   Color32 = rgb(0x282829); // inputs               (mockup --field)
pub const SURFACE_CONTROL: Color32 = rgb(0x2c2c2c); // raised / hover       (mockup --control)
pub const SURFACE_WELL:    Color32 = rgb(0x171717); // rail active well     (mockup --well)
pub const SELECTED_ROW:    Color32 = rgb(0x262340); // selected list row    (mockup --sel)
/// Outliner alternate-row zebra — one step above [`SURFACE_PANEL`] (0x1c). Must stay far below
/// hover ([`SURFACE_HEADER`]) and selection ([`SELECTED_ROW`]); owner: prior white@2/3 too loud.
pub const OL_ZEBRA:        Color32 = rgb(0x1d1d1d);

// ────────────────────────────────────────────────────────────────────────────
// Borders / seams
// ────────────────────────────────────────────────────────────────────────────
pub const SEAM:         Color32 = rgb(0x141414); // 1px region seams (mockup --seam; was 0a0a0a)
pub const BORDER:       Color32 = rgb(0x2e2e2f); // mockup --border        (was 2c2c30)
pub const BORDER_SUBTLE:Color32 = rgb(0x262626); // mockup --border-subtle (unchanged)
pub const BORDER_HOVER: Color32 = rgb(0x3d3d3e); // mockup --border-hover  (was 3b3b40)

// ────────────────────────────────────────────────────────────────────────────
// Accent (indigo-violet) + on-accent text
// ────────────────────────────────────────────────────────────────────────────
pub const ACCENT:       Color32 = rgb(0x7b61ff); // active / selection / focus / brand
pub const ACCENT_HOVER: Color32 = rgb(0x8b6fff);
pub const ACCENT_LIGHT: Color32 = rgb(0xa191ff);
pub const ACCENT_DIM:   Color32 = rgb(0x5b48c4); // scrollbar thumb active · locked-chip hover
pub const ACCENT_TEXT:  Color32 = rgb(0xffffff); // text on an accent fill
pub const ACCENT_AMBER: Color32 = rgb(0xf0a629); // active body (outliner) — NOT a second accent

// ────────────────────────────────────────────────────────────────────────────
// Semantic hues — meaning, not decoration
// ────────────────────────────────────────────────────────────────────────────
pub const SDF:          Color32 = rgb(0x39d98a); // SDF pillar · boolean union   (mockup --sdf)
pub const MESH:         Color32 = rgb(0x3e9bff); // mesh pillar · links          (mockup --mesh)
pub const LIGHT:        Color32 = rgb(0xffd54a); // lights                       (mockup --light)
pub const CAM:          Color32 = rgb(0xff5555); // cameras                      (mockup --cam)
pub const SCULPT_ORANGE:Color32 = rgb(0xff8a3d); // sculpt pillar                (mockup --sculpt)
pub const NURBS:        Color32 = rgb(0xa191ff); // NURBS pillar (aliases ACCENT_LIGHT — mockup --nurbs)
pub const CYAN:         Color32 = rgb(0x4ecdc4); // convert / cross-rep ops      (mockup --cyan)
pub const GEO:          Color32 = rgb(0x7486a3); // geometry / neutral pillar    (mockup --geo)
pub const ROSE:         Color32 = rgb(0xff6bb3); // accent hue                   (mockup --rose)
pub const AXIS_X:       Color32 = rgb(0xe05252); // mockup --ax-x
pub const AXIS_Y:       Color32 = rgb(0x7dc242); // mockup --ax-y
pub const AXIS_Z:       Color32 = rgb(0x4a8fe0); // mockup --ax-z
pub const FOLDER_GOLD:  Color32 = rgb(0xc9a24a); // groups / folders (outliner — not a mockup :root token)
pub const RENDER_ORANGE:Color32 = rgb(0xff9d4d); // scene root · modifier wrench
pub const WARNING:      Color32 = rgb(0xffb020); // boolean intersect
pub const DANGER:       Color32 = rgb(0xf0645c); // hidden eye · subtract · non-manifold
pub const DESTRUCTIVE:  Color32 = rgb(0xe08a80); // destructive menu text — NOT DANGER (mockup --destructive)

// ────────────────────────────────────────────────────────────────────────────
// Text
// ────────────────────────────────────────────────────────────────────────────
pub const TEXT:           Color32 = rgb(0xe6e6e6); // primary
pub const TEXT_SECONDARY: Color32 = rgb(0xc8c8c8); // secondary
pub const TEXT_LABEL:     Color32 = rgb(0xa0a0a0); // field labels
pub const TEXT_DIM:       Color32 = rgb(0x909090); // labels / inactive
pub const TEXT_SECTION:   Color32 = rgb(0x8a8a8a); // uppercase section headers
pub const TEXT_FAINT:     Color32 = rgb(0x646464); // metadata / disabled

// ────────────────────────────────────────────────────────────────────────────
// Type scale (px)
// ────────────────────────────────────────────────────────────────────────────
// Type scale — mockup :root --ui / --caps / --mono (13 / 10 / 11).
pub const FONT_CAPS:  f32 = 10.0; // uppercase section labels (--caps)
pub const FONT_UI:    f32 = 13.0; // body / buttons / tool names (--ui:13px)
pub const FONT_TITLE: f32 = 13.0; // panel titles (same as body)
pub const FONT_MONO:  f32 = 11.0; // numeric values / keycaps (--mono)
/// App-bar menu labels (mockup .menu font-size:12.5px).
pub const FONT_MENU:  f32 = 12.5;
/// Workspace / ctx pills (mockup .tab 12px · .pill 12px).
pub const FONT_TAB:   f32 = 12.0;

// ────────────────────────────────────────────────────────────────────────────
// Metrics — exact from the mockup (azra_app_mockup.html)
// ────────────────────────────────────────────────────────────────────────────
pub const RADIUS_SEAM:    f32 = 3.0; // seams / panels
pub const RADIUS_CONTROL: f32 = 5.0; // tabs / inputs / grip · brand mark
pub const RADIUS_BUTTON:  f32 = 6.0; // buttons / chips / icon buttons · pills
pub const RADIUS_CARD:    f32 = 8.0; // cards · workspace tab strip
pub const RADIUS_PANEL:   f32 = 8.0;
pub const ROW_TOOL:       f32 = 28.0;
pub const ROW_HEIGHT:     f32 = 26.0; // list / property rows
/// `.app{grid-template-rows:44px 36px 1fr 24px}`
pub const H_APPBAR:       f32 = 44.0;
pub const H_CONTEXT:      f32 = 36.0;
pub const H_PANEL_HEADER: f32 = 34.0;
pub const H_STATUS:       f32 = 24.0;
/// Body columns: `var(--railw,200px) 5px 1fr 5px var(--dockw,300px)`
pub const RAIL_DEFAULT:   f32 = 200.0;
pub const RAIL_MIN:       f32 = 150.0;
pub const RAIL_MAX:       f32 = 360.0;
pub const DOCK_DEFAULT:   f32 = 300.0;
pub const DOCK_MIN:       f32 = 220.0;
pub const DOCK_MAX:       f32 = 560.0;
pub const SEAM_W:         f32 = 5.0;
/// Icon button hit box (mockup .ibtn 28×28 · .shading .s 28×24).
pub const IBTN_SIZE:      f32 = 28.0;

// ────────────────────────────────────────────────────────────────────────────
// Motion (ms)
// ────────────────────────────────────────────────────────────────────────────
pub const MOTION_SURFACE_MS: f32 = 120.0;
pub const MOTION_PRESS_MS:   f32 = 80.0;
pub const MOTION_FOCUS_MS:   f32 = 0.0;
pub const MOTION_POPOVER_MS: f32 = 120.0;
pub const MOTION_TOAST_MS:   f32 = 180.0;

/// A faint tint of the accent (for hovers / soft fills). Selected rows should use
/// the opaque `SELECTED_ROW` instead so the tint doesn't stack over striping.
pub fn accent_tint(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), alpha)
}

/// Install the mockup's typography, once per process: **Geist** (UI sans, Medium as the
/// working weight; SemiBold for titles via the "azra_semibold" family) and **Geist Mono**
/// (all numerics/keycaps). Embedded at compile time from `app/assets/fonts` (OFL 1.1) —
/// no system-font dependency. egui's default fonts stay in the fallback chain so emoji
/// and unusual glyphs still render.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("geist".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Geist-Medium.ttf"))));
    fonts.font_data.insert("geist_semibold".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Geist-SemiBold.ttf"))));
    fonts.font_data.insert("geist_mono".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/GeistMono-Regular.ttf"))));
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "geist".to_owned());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "geist_mono".to_owned());
    // Named family for headings/titles that want the heavier cut.
    fonts.families.insert(egui::FontFamily::Name("azra_semibold".into()), vec![
        "geist_semibold".to_owned(), "geist".to_owned(),
    ]);
    // Phosphor icon glyphs (same set as the mockup) — same placement as
    // `egui_phosphor::add_to_fonts` (Proportional, index 1). Inserted from the raw
    // font bytes because egui-phosphor 0.7's `FontDefinitions` is egui 0.29's type.
    fonts.font_data.insert("phosphor".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(egui_phosphor::Variant::Regular.font_bytes())));
    if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        keys.insert(1, "phosphor".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Apply the AZRA graphite theme to an egui `Style` in place. Flat, dark, and
/// cohesive: surfaces from the background stack, hover lifts the surface (no hue),
/// the single active item is an accent fill. Purely visual; safe every frame.
pub fn apply_style(style: &mut egui::Style) {
    let r_ctl = egui::CornerRadius::same(RADIUS_CONTROL as u8);
    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT_LIGHT;
    v.warn_fg_color = WARNING;
    v.error_fg_color = DESTRUCTIVE;

    // Surfaces
    v.panel_fill = SURFACE_PANEL;
    v.window_fill = SURFACE_CARD;
    v.faint_bg_color = SURFACE_HEADER;   // striped rows / weak fills
    // extreme_bg is shared: scrollbar TRACK wants --well; inputs that need --field
    // paint their own Frame. Default track fidelity wins (mockup scrollbar rule).
    v.extreme_bg_color = SURFACE_WELL;
    v.code_bg_color = SURFACE_WELL;
    v.window_stroke = egui::Stroke::new(1.0, BORDER);
    v.window_corner_radius = egui::CornerRadius::same(RADIUS_CARD as u8);
    v.menu_corner_radius = egui::CornerRadius::same(RADIUS_CARD as u8);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 10], blur: 32, spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 8], blur: 26, spread: 0,
        color: Color32::from_black_alpha(130),
    };

    // Selection = accent (opaque violet-tinted row + solid stroke)
    v.selection.bg_fill = SELECTED_ROW;
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);

    // Widget state ladder — FLAT. Rest is borderless on the panel; hover lifts the
    // surface a step; the single active item is an accent fill. Only inputs keep a
    // hairline (via the field surfaces), giving the modern flat-button look.
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE_PANEL;
    w.noninteractive.weak_bg_fill = SURFACE_PANEL;
    w.noninteractive.bg_stroke = egui::Stroke::NONE;
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_DIM);
    w.noninteractive.corner_radius = r_ctl;

    // Scroll handle uses widgets.*.bg_fill (scroll_area paint). Mockup thumb:
    // --control rest · --border-hover hover · --accent-dim active. weak_bg stays
    // for button rest (HEADER), so buttons and scroll thumbs can differ.
    w.inactive.bg_fill = SURFACE_CONTROL;
    w.inactive.weak_bg_fill = SURFACE_HEADER; // button rest fill (flat, no border)
    w.inactive.bg_stroke = egui::Stroke::NONE;
    w.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_SECONDARY);
    w.inactive.corner_radius = r_ctl;

    w.hovered.bg_fill = BORDER_HOVER;
    w.hovered.weak_bg_fill = SURFACE_CONTROL;
    w.hovered.bg_stroke = egui::Stroke::NONE;
    w.hovered.fg_stroke = egui::Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = r_ctl;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = ACCENT_DIM;
    w.active.weak_bg_fill = ACCENT;
    w.active.bg_stroke = egui::Stroke::NONE;
    w.active.fg_stroke = egui::Stroke::new(1.0, ACCENT_TEXT);
    w.active.corner_radius = r_ctl;
    w.active.expansion = 0.0;

    w.open.bg_fill = SURFACE_CONTROL;
    w.open.weak_bg_fill = SURFACE_HEADER;
    w.open.bg_stroke = egui::Stroke::new(1.0, BORDER);
    w.open.fg_stroke = egui::Stroke::new(1.0, TEXT);
    w.open.corner_radius = r_ctl;

    // Spacing — dense desktop tool.
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(6.0, 4.0);
    s.button_padding = egui::vec2(8.0, 4.0);
    s.interact_size.y = 22.0;
    s.window_margin = egui::Margin::same(8);
    s.menu_margin = egui::Margin::same(5);
    s.menu_spacing = 2.0;
    // Scrollbar — mockup `::-webkit-scrollbar`:
    //   track = --well, thumb = --control, hover = --border-hover, active = --accent-dim
    //   13px lane, 3px CSS border inset on thumb → ~7px visible handle.
    // CRITICAL: egui Default::floating() sets foreground_color=true, which paints the
    // handle with fg_stroke (light text). We MUST force solid + foreground_color=false
    // so the handle uses widgets.inactive.bg_fill (= SURFACE_CONTROL).
    s.scroll = egui::style::ScrollStyle {
        floating: false,
        bar_width: 7.0,
        handle_min_length: 20.0,
        bar_inner_margin: 3.0,
        bar_outer_margin: 3.0,
        floating_width: 2.0,
        floating_allocated_width: 0.0,
        foreground_color: false, // ← handle = bg_fill (CONTROL), not light text
        dormant_background_opacity: 1.0,
        active_background_opacity: 1.0,
        interact_background_opacity: 1.0,
        dormant_handle_opacity: 1.0,
        active_handle_opacity: 1.0,
        interact_handle_opacity: 1.0,
    };

    // Type scale on the embedded families — Proportional resolves to Geist, Monospace to
    // Geist Mono (both inserted at index 0 by install_fonts).
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles.insert(TextStyle::Small,     FontId::new(FONT_CAPS,  FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body,      FontId::new(FONT_UI,    FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Button,    FontId::new(FONT_UI,    FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Heading,   FontId::new(FONT_TITLE, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Monospace, FontId::new(FONT_MONO,  FontFamily::Monospace));
}
