// core_engine/app/src/pie_menu.rs
// ═══════════════════════════════════════════════════════════════════════════════
// AZRA — Professional Pie Menu System v3
//
// Visual redesign: floating rectangular pill buttons orbiting a center hub,
// with connector lines, per-item accent colors, glow-on-hover, and submenu
// support. Matches the PieMenuDesign.jsx reference.
// ═══════════════════════════════════════════════════════════════════════════════

use eframe::egui;
use std::f32::consts::{FRAC_PI_2, TAU};

// ================================================================
// Persistent mode toggle (center button)
// ================================================================

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PieMode {
    Sdf,
    Poly,
    Nurbs,
}

impl PieMode {
    /// Cycle forward: SDF → Poly → NURBS → SDF
    pub(crate) fn cycle_forward(self) -> Self {
        match self { PieMode::Sdf => PieMode::Poly, PieMode::Poly => PieMode::Nurbs, PieMode::Nurbs => PieMode::Sdf }
    }
    #[allow(dead_code)]
    pub(crate) fn label(self) -> &'static str {
        match self { PieMode::Sdf => "SDF", PieMode::Poly => "POLY", PieMode::Nurbs => "NURBS" }
    }
}

// ================================================================
// Color palette (matches JSX C object)
// ================================================================

const BG_SLICE: egui::Color32      = egui::Color32::from_rgb(0x24, 0x24, 0x24);
const BORDER: egui::Color32        = egui::Color32::from_rgb(0x36, 0x36, 0x36);
const TEXT: egui::Color32          = egui::Color32::from_rgb(0xe8, 0xe8, 0xe8);
const TEXT_DIM: egui::Color32      = egui::Color32::from_rgb(0x77, 0x77, 0x77);
const TEXT_BRIGHT: egui::Color32   = egui::Color32::from_rgb(0xff, 0xff, 0xff);
const SHORTCUT_COL: egui::Color32  = egui::Color32::from_rgb(0x55, 0x55, 0x55);

const PRIMARY: egui::Color32       = egui::Color32::from_rgb(0xFF, 0x9E, 0x3D);
const SECONDARY: egui::Color32     = egui::Color32::from_rgb(0x5C, 0xB8, 0x5C);
const VIOLET: egui::Color32        = egui::Color32::from_rgb(0x9B, 0x6D, 0xFF);

// ================================================================
// Data types
// ================================================================

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub(crate) enum Accent {
    Primary,   // amber — top/bottom items
    Secondary, // green — side items
    Violet,    // violet — submenu / special
}

impl Accent {
    fn color(self) -> egui::Color32 {
        match self { Accent::Primary => PRIMARY, Accent::Secondary => SECONDARY, Accent::Violet => VIOLET }
    }
    fn dim(self) -> egui::Color32 {
        let c = self.color();
        egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 30)
    }
    fn glow(self) -> egui::Color32 {
        let c = self.color();
        egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90)
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct PieItem {
    pub(crate) label: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) shortcut: &'static str,
    pub(crate) color: egui::Color32, // kept for API compat (active ring)
    pub(crate) id: usize,
    pub(crate) accent: Accent,
    pub(crate) square: bool,         // icon-only compact button
    /// Smaller square (polygroup cube) — visual only, same select path as other slices.
    pub(crate) compact: bool,
    /// Draw multi-colour isometric cube instead of `icon` text (polygroup mode).
    pub(crate) polygroup_cube: bool,
    pub(crate) has_submenu: bool,
    pub(crate) hidden: bool,         // spacer slot — not drawn, not selectable
    /// Yellow pivot/center control dot drawn to the right of the pill (OBJECT).
    pub(crate) pivot_dot: bool,
    /// Drawn dimmed, hoverable (so the description explains WHY), but the confirm
    /// path must refuse it (resolution slices beyond the sparse ceiling).
    pub(crate) disabled: bool,
}

impl PieItem {
    fn new(label: &'static str, icon: &'static str, shortcut: &'static str, id: usize, accent: Accent) -> Self {
        Self {
            label, icon, shortcut, color: accent.color(), id, accent,
            square: false, compact: false, polygroup_cube: false,
            has_submenu: false, hidden: false, pivot_dot: false,
            disabled: false,
        }
    }
    fn with_pivot_dot(mut self) -> Self {
        self.pivot_dot = true;
        self
    }
    fn with_disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }
    #[allow(dead_code)]
    fn square(icon: &'static str, label: &'static str, shortcut: &'static str, id: usize, accent: Accent) -> Self {
        Self {
            label, icon, shortcut, color: accent.color(), id, accent,
            square: true, compact: false, polygroup_cube: false,
            has_submenu: false, hidden: false, pivot_dot: false,
            disabled: false,
        }
    }
    /// Small square with multi-colour polygroup cube icon (GROUP slice).
    fn group_cube(label: &'static str, shortcut: &'static str, id: usize, accent: Accent) -> Self {
        Self {
            label,
            icon: "",
            shortcut,
            color: accent.color(),
            id,
            accent,
            square: true,
            compact: true,
            polygroup_cube: true,
            has_submenu: false,
            hidden: false,
            pivot_dot: false,
            disabled: false,
        }
    }
    fn spacer() -> Self {
        Self {
            label: "", icon: "", shortcut: "", color: egui::Color32::TRANSPARENT, id: 0,
            accent: Accent::Primary, square: false, compact: false, polygroup_cube: false,
            has_submenu: false, hidden: true, pivot_dot: false,
            disabled: false,
        }
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct PieRing {
    pub(crate) items: Vec<PieItem>,
    pub(crate) radius: f32,
    pub(crate) item_radius: f32,
}

#[derive(Clone)]
pub(crate) struct PieMenuDef {
    pub(crate) inner: PieRing,
    pub(crate) outer: Option<PieRingSet>,
    pub(crate) bg_radius: f32,
    pub(crate) center_label: &'static str,
    pub(crate) description_fn: Option<fn(usize) -> &'static str>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct PieRingSet {
    pub(crate) rings: Vec<Vec<PieItem>>,
    pub(crate) radius: f32,
    pub(crate) item_radius: f32,
}

// ================================================================
// Runtime state
// ================================================================

#[derive(Clone)]
pub(crate) struct PieMenuState {
    pub(crate) open: bool,
    pub(crate) center: egui::Pos2,
    pub(crate) open_time: Option<std::time::Instant>, // used by click-mode pies to ignore open-frame LMB
    pub(crate) hovered_inner: Option<usize>,
    pub(crate) hovered_outer: Option<usize>,
    pub(crate) hovered_center: bool,
    /// Hover on the yellow pivot/center accessory next to OBJECT.
    pub(crate) hovered_object_pivot: bool,
    /// LMB already handled the yellow dot this open — Tab release must Cancel (no mode change).
    pub(crate) pivot_click_consumed: bool,
    anim_progress: f32,
    inner_hover_anim: Vec<f32>,
    outer_hover_anim: Vec<f32>,
    closing: bool,
    pub(crate) key_selection: Option<usize>,
    /// True once the mouse has left the center dead zone at least once.
    /// Prevents accidental center toggle on quick Tab tap.
    pub(crate) left_deadzone: bool,
    /// Position where pie was opened — used for movement threshold.
    pub(crate) open_pos: egui::Pos2,
    /// True once the mouse has moved >= MOVE_THRESHOLD from open_pos.
    /// Gates ALL item selection — nothing highlights until the mouse moves.
    pub(crate) has_moved: bool,
    /// Blender-style: after a quick key release with no direction, the pie stays open
    /// for LMB click selection (`interface_region_menu_pie` click-mode).
    pub(crate) click_mode: bool,
}

impl Default for PieMenuState {
    fn default() -> Self {
        Self {
            open: false, center: egui::Pos2::ZERO, open_time: None,
            hovered_inner: None, hovered_outer: None, hovered_center: false,
            hovered_object_pivot: false,
            pivot_click_consumed: false,
            anim_progress: 0.0, inner_hover_anim: Vec::new(), outer_hover_anim: Vec::new(),
            closing: false, key_selection: None, left_deadzone: false,
            open_pos: egui::Pos2::ZERO, has_moved: false,
            click_mode: false,
        }
    }
}

/// Yellow pivot accessory — plain disc, well clear of the OBJECT pill.
const PIVOT_DOT_R: f32 = 5.5;
const PIVOT_DOT_GAP: f32 = 36.0; // pill edge → disc center (keep away so OBJECT hit is separate)
const PIVOT_DOT_COL: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xC1, 0x07);

#[derive(Debug, Clone, Copy)]
pub(crate) enum PieResult {
    Cancel,
    QuickTap,
    Inner(usize),
    Outer { inner: usize, outer: usize },
    CenterToggle,
    /// Yellow pivot/center dot next to OBJECT was selected.
    ObjectPivot,
}

// ================================================================
// Animation helpers
// ================================================================

fn ease_out_cubic(t: f32) -> f32 { let t = t.clamp(0.0, 1.0); 1.0 - (1.0 - t).powi(3) }
fn lerp_f32(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t.clamp(0.0, 1.0) }

fn with_alpha(col: egui::Color32, alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha)
}

// ================================================================
// Constants
// ================================================================

const ANIM_OPEN_DURATION: f32 = 0.12;
const HOVER_ANIM_SPEED: f32 = 14.0;
const CONFIRM_DELAY_MS: u128 = 50;
const QUICK_TAP_MS: u128 = 200;
const DEAD_ZONE: f32 = 22.0;
/// Mouse must move this far from the open position before any item can be selected.
const MOVE_THRESHOLD: f32 = 5.0;

const SLICE_H: f32 = 26.0;       // button height (Blender-compact)
const SLICE_PAD_H: f32 = 10.0;   // horizontal padding
const SLICE_GAP: f32 = 6.0;      // icon-label gap
const SLICE_ROUNDING: f32 = 4.0;  // corner radius
const HUB_R: f32 = 16.0;         // center hub radius

const FONT_ICON: f32 = 12.0;
const FONT_LABEL: f32 = 11.0;
const FONT_SHORTCUT: f32 = 9.0;
const FONT_CENTER: f32 = 9.0;

// ================================================================
// State logic
// ================================================================

impl PieMenuState {
    pub(crate) fn open(&mut self, pos: egui::Pos2, inner_count: usize) {
        self.open = true;
        self.closing = false;
        self.center = pos;
        self.open_time = Some(std::time::Instant::now());
        self.hovered_inner = None;
        self.hovered_outer = None;
        self.hovered_center = false;
        self.hovered_object_pivot = false;
        self.pivot_click_consumed = false;
        self.left_deadzone = false;
        self.open_pos = pos;
        self.has_moved = false;
        self.click_mode = false;
        self.anim_progress = 0.0;
        self.key_selection = None;
        self.inner_hover_anim = vec![0.0; inner_count];
        self.outer_hover_anim = vec![0.0; 8];
    }

    /// Instant close (e.g. yellow-dot opened another pie — no fade, no selection).
    /// Always zeros `anim_progress` so the pie cannot stick drawn with `open=false`.
    pub(crate) fn force_close(&mut self) {
        self.open = false;
        self.closing = false;
        self.anim_progress = 0.0;
        self.open_time = None;
        self.hovered_inner = None;
        self.hovered_outer = None;
        self.hovered_center = false;
        self.hovered_object_pivot = false;
        self.pivot_click_consumed = true;
        self.key_selection = None;
        self.click_mode = false;
        self.has_moved = false;
    }

    pub(crate) fn tick(&mut self, dt: f32) -> bool {
        if !self.open && self.anim_progress <= 0.0 { return false; }
        let mut needs_repaint = false;

        if self.open && !self.closing {
            if self.anim_progress < 1.0 {
                self.anim_progress = (self.anim_progress + dt / ANIM_OPEN_DURATION).min(1.0);
                needs_repaint = true;
            }
        } else if self.closing {
            self.anim_progress = (self.anim_progress - dt / ANIM_OPEN_DURATION).max(0.0);
            if self.anim_progress <= 0.0 { self.open = false; self.closing = false; }
            needs_repaint = true;
        }

        for (i, anim) in self.inner_hover_anim.iter_mut().enumerate() {
            let target = if self.hovered_inner == Some(i) { 1.0 } else { 0.0 };
            let prev = *anim;
            *anim = lerp_f32(*anim, target, dt * HOVER_ANIM_SPEED);
            if (*anim - prev).abs() > 0.001 { needs_repaint = true; }
        }
        for (i, anim) in self.outer_hover_anim.iter_mut().enumerate() {
            let target = if self.hovered_outer == Some(i) { 1.0 } else { 0.0 };
            let prev = *anim;
            *anim = lerp_f32(*anim, target, dt * HOVER_ANIM_SPEED);
            if (*anim - prev).abs() > 0.001 { needs_repaint = true; }
        }
        needs_repaint
    }

    /// Hit-test against rectangular buttons positioned radially.
    pub(crate) fn update_hover(&mut self, pointer: egui::Pos2, def: &PieMenuDef, _ui_scale: f32) {
        if !self.open || self.closing { return; }

        // Movement gate: nothing highlights until mouse moves MOVE_THRESHOLD from open position
        if !self.has_moved {
            let moved_dist = (pointer - self.open_pos).length();
            if moved_dist >= MOVE_THRESHOLD {
                self.has_moved = true;
            } else {
                // Mouse hasn't moved enough — clear all hover and return
                self.hovered_inner = None;
                self.hovered_outer = None;
                self.hovered_center = false;
                self.hovered_object_pivot = false;
                return;
            }
        }

        let d = pointer - self.center;
        let dist = d.length();

        // Yellow pivot dots next to OBJECT (priority over pill / outer).
        self.hovered_object_pivot = false;
        {
            let inner_n = def.inner.items.len();
            for (i, item) in def.inner.items.iter().enumerate() {
                if item.hidden || !item.pivot_dot {
                    continue;
                }
                let Some(dot) = pivot_dot_center(self.center, def, i, inner_n, 1.0) else {
                    continue;
                };
                if (pointer - dot).length() <= PIVOT_DOT_R + 8.0 {
                    // Yellow only — do NOT set hovered_inner (that would select OBJECT mode).
                    self.hovered_object_pivot = true;
                    self.hovered_inner = None;
                    self.hovered_outer = None;
                    self.hovered_center = false;
                    return;
                }
            }
        }

        // Check outer items first (anchored at inner item, perpendicular layout)
        if let Some(ref outer_set) = def.outer {
            if let Some(inner_idx) = self.hovered_inner {
                if inner_idx < outer_set.rings.len() {
                    let outer_items = &outer_set.rings[inner_idx];
                    if !outer_items.is_empty() {
                        let inner_n = def.inner.items.len();
                        let item_spacing = outer_set.item_radius * 2.5;
                        let positions = outer_item_positions(
                            self.center, inner_idx, inner_n,
                            def.inner.radius, outer_items.len(), item_spacing,
                        );
                        let hit_r = outer_set.item_radius * 1.3;
                        for (j, opos) in positions.iter().enumerate() {
                            let dx = (pointer.x - opos.x).abs();
                            let dy = (pointer.y - opos.y).abs();
                            if dx < hit_r && dy < hit_r {
                                self.hovered_outer = Some(j);
                                return;
                            }
                        }
                    }
                }
            }
        }
        self.hovered_outer = None;

        // Center hub: always hoverable when mouse is in center zone.
        // The has_moved gate prevents accidental selection on Tab release.
        // Deliberate LMB clicks on center are handled separately and always work.
        if dist < DEAD_ZONE {
            self.hovered_center = true;
            self.hovered_inner = None;
            return;
        }
        self.hovered_center = false;

        // Inner ring: hit-test each button rect
        {
            let inner_n = def.inner.items.len();
            let mut best: Option<(usize, f32)> = None;
            for (i, item) in def.inner.items.iter().enumerate() {
                if item.hidden { continue; }
                let angle = sector_center_angle(i, inner_n);
                let item_pos = egui::pos2(
                    self.center.x + angle.cos() * def.inner.radius,
                    self.center.y + angle.sin() * def.inner.radius,
                );
                let dx = (pointer.x - item_pos.x).abs();
                let dy = (pointer.y - item_pos.y).abs();
                // Generous hit rect (wider than visual for usability)
                let hw = 50.0;
                let hh = SLICE_H * 0.8;
                if dx < hw && dy < hh {
                    let d2 = dx * dx + dy * dy;
                    if best.map_or(true, |(_, bd)| d2 < bd) { best = Some((i, d2)); }
                }
            }
            // Fall back to the angular sector when no rect hit. NO DISTANCE CAP (owner): a pie slice
            // is a DIRECTION, not a button — flicking far past the ring must keep the selection
            // registered. (The old bg_radius+30px cap silently dropped hover on overshoot → release
            // cancelled.) Outer-ring items still win via their rect tests above.
            if best.is_none() {
                let angle = d.y.atan2(d.x);
                let mut sector = angle_to_sector(angle, inner_n);
                // Redirect hidden sector to nearest visible neighbor
                if sector < inner_n && def.inner.items[sector].hidden {
                    let prev = if sector == 0 { inner_n - 1 } else { sector - 1 };
                    let next = (sector + 1) % inner_n;
                    let d_prev = (angle - sector_center_angle(prev, inner_n)).abs();
                    let d_next = (angle - sector_center_angle(next, inner_n)).abs();
                    sector = if d_prev < d_next { prev } else { next };
                }
                best = Some((sector, 0.0));
            }
            self.hovered_inner = best.map(|(i, _)| i);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn handle_key(&mut self, key: usize, max_items: usize) {
        if self.open && key >= 1 && key <= max_items {
            self.key_selection = Some(key - 1);
        }
    }

    pub(crate) fn release(&mut self) -> PieResult {
        if let Some(idx) = self.key_selection.take() {
            self.open = false; self.closing = false; self.anim_progress = 0.0;
            return PieResult::Inner(idx);
        }

        let elapsed_ms = self.open_time.map(|t| t.elapsed().as_millis()).unwrap_or(999);
        let was_quick_tap = elapsed_ms < QUICK_TAP_MS;

        // If mouse never moved from open position, cancel — no accidental selection
        // Yellow-dot was LMB-clicked while Tab was held — do not enter OBJECT mode on release.
        if self.pivot_click_consumed {
            self.pivot_click_consumed = false;
            self.open = false;
            self.closing = false;
            self.anim_progress = 0.0;
            self.open_time = None;
            self.hovered_inner = None;
            self.hovered_outer = None;
            self.hovered_object_pivot = false;
            return PieResult::Cancel;
        }

        let result = if !self.has_moved {
            if was_quick_tap { PieResult::QuickTap } else { PieResult::Cancel }
        } else if self.hovered_center && elapsed_ms >= CONFIRM_DELAY_MS {
            PieResult::CenterToggle
        } else if was_quick_tap && self.hovered_inner.is_none() {
            PieResult::QuickTap
        } else if elapsed_ms < CONFIRM_DELAY_MS {
            PieResult::QuickTap
        } else if let Some(outer_idx) = self.hovered_outer {
            if let Some(inner_idx) = self.hovered_inner {
                PieResult::Outer { inner: inner_idx, outer: outer_idx }
            } else { PieResult::Cancel }
        } else if let Some(inner_idx) = self.hovered_inner {
            PieResult::Inner(inner_idx)
        } else { PieResult::Cancel };

        self.open = false; self.closing = false; self.anim_progress = 0.0;
        self.open_time = None; self.hovered_inner = None; self.hovered_outer = None;
        self.hovered_object_pivot = false;
        result
    }
}

const SLICE_H_COMPACT: f32 = 20.0; // smaller square for GROUP polygroup cube

/// Compute button size for a pie item (shared by hit-test + draw).
fn pie_item_btn_size(painter: Option<&egui::Painter>, item: &PieItem) -> egui::Vec2 {
    if item.square {
        let s = if item.compact { SLICE_H_COMPACT } else { SLICE_H };
        return egui::vec2(s, s);
    }
    let icon_w = if item.icon.is_empty() { 0.0 } else { 14.0 };
    let label_w = if let Some(p) = painter {
        p.layout_no_wrap(
            item.label.to_string(),
            egui::FontId::proportional(FONT_LABEL),
            TEXT,
        )
        .size()
        .x
    } else {
        // Approximate without painter (hover path)
        item.label.len() as f32 * 6.5
    };
    let sc_w = if item.shortcut.is_empty() {
        0.0
    } else if let Some(p) = painter {
        p.layout_no_wrap(
            item.shortcut.to_string(),
            egui::FontId::proportional(FONT_SHORTCUT),
            SHORTCUT_COL,
        )
        .size()
        .x
            + 4.0
    } else {
        10.0
    };
    let submenu_w = if item.has_submenu { 10.0 } else { 0.0 };
    let btn_w = icon_w + SLICE_GAP + label_w + sc_w + submenu_w + SLICE_PAD_H * 2.0;
    egui::vec2(btn_w, SLICE_H)
}

/// Center of the yellow pivot accessory for item `i`.
fn pivot_dot_center(
    pie_center: egui::Pos2,
    def: &PieMenuDef,
    i: usize,
    inner_n: usize,
    anim_scale: f32,
) -> Option<egui::Pos2> {
    let item = def.inner.items.get(i)?;
    if !item.pivot_dot || item.hidden {
        return None;
    }
    let angle = sector_center_angle(i, inner_n);
    let r = def.inner.radius * anim_scale;
    let pos = egui::pos2(
        pie_center.x + angle.cos() * r,
        pie_center.y + angle.sin() * r,
    );
    let btn_sz = pie_item_btn_size(None, item);
    // Dot sits just outside the right edge of the pill (screen-right for top OBJECT).
    Some(egui::pos2(
        pos.x + btn_sz.x * 0.5 + PIVOT_DOT_GAP,
        pos.y,
    ))
}

// ================================================================
// Drawing — v3 floating button design
// ================================================================

pub(crate) fn draw_pie_menu(
    painter: &egui::Painter,
    state: &PieMenuState,
    def: &PieMenuDef,
    active_inner: Option<usize>,
) {
    if !state.open && state.anim_progress <= 0.0 { return; }

    let anim = ease_out_cubic(state.anim_progress);
    if anim < 0.01 { return; }

    let c = state.center;
    let inner_n = def.inner.items.len();
    let global_alpha = (anim * 255.0) as u8;
    let hub_r = HUB_R * lerp_f32(0.6, 1.0, anim);

    // ── Connector lines (hub → each button) ──
    for (i, item) in def.inner.items.iter().enumerate() {
        if item.hidden { continue; }
        let angle = sector_center_angle(i, inner_n);
        let hov_t = if i < state.inner_hover_anim.len() { state.inner_hover_anim[i] } else { 0.0 };
        let r_start = hub_r + 2.0;
        let r_end = def.inner.radius * 0.52;
        let start = egui::pos2(c.x + angle.cos() * r_start, c.y + angle.sin() * r_start);
        let end = egui::pos2(c.x + angle.cos() * r_end, c.y + angle.sin() * r_end);

        let accent = item.accent.color();
        let (col, width) = if hov_t > 0.1 {
            (with_alpha(accent, (lerp_f32(0.15, 0.6, hov_t) * 255.0 * anim) as u8), lerp_f32(0.5, 1.5, hov_t))
        } else {
            (egui::Color32::from_rgba_unmultiplied(0x26, 0x26, 0x26, (0.2 * 255.0 * anim) as u8), 0.5)
        };
        painter.line_segment([start, end], egui::Stroke::new(width, col));
    }

    // ── Inner ring: floating rectangular buttons ──
    for (i, item) in def.inner.items.iter().enumerate() {
        if item.hidden { continue; }
        let angle = sector_center_angle(i, inner_n);
        // Disabled items never take hover styling (no glow, no accent) — they stay a dim
        // pill; the hover tooltip (description_fn) still explains why they are blocked.
        let hov_t = if item.disabled { 0.0 }
            else if i < state.inner_hover_anim.len() { state.inner_hover_anim[i] } else { 0.0 };
        let is_active = active_inner == Some(i) && !item.disabled;

        // Button scale animation
        let scale = lerp_f32(1.0, 1.06, hov_t);
        let r = def.inner.radius * lerp_f32(0.85, 1.0, anim) * scale;
        let pos = egui::pos2(c.x + angle.cos() * r, c.y + angle.sin() * r);

        let accent = item.accent;
        let accent_col = accent.color();

        // Measure button width
        let btn_sz = pie_item_btn_size(Some(painter), item);
        let btn_w = btn_sz.x;
        let btn_h = btn_sz.y;
        let btn_rect = egui::Rect::from_center_size(pos, egui::vec2(btn_w, btn_h));

        // Determine colors (disabled = dim text/icon on the resting pill)
        let bg_col = if hov_t > 0.1 { accent.dim() } else { BG_SLICE };
        let border_col = if hov_t > 0.1 { accent_col } else { BORDER };
        let text_col = if item.disabled { TEXT_DIM } else if hov_t > 0.1 { TEXT_BRIGHT } else { TEXT };
        let icon_col = if item.disabled { SHORTCUT_COL } else if hov_t > 0.1 { accent_col } else { TEXT_DIM };

        // Active item glow ring
        if is_active && hov_t < 0.3 {
            let glow_rect = btn_rect.expand(3.0);
            painter.rect_filled(glow_rect, SLICE_ROUNDING + 2.0,
                with_alpha(accent_col, (25.0 * anim) as u8));
        }

        // Drop shadow
        if hov_t > 0.1 {
            let shadow_rect = btn_rect.translate(egui::vec2(0.0, 2.0)).expand(2.0);
            painter.rect_filled(shadow_rect, SLICE_ROUNDING + 1.0,
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, (lerp_f32(0.0, 0.5, hov_t) * 255.0 * anim) as u8));
            // Glow
            let glow_rect = btn_rect.expand(4.0);
            painter.rect_filled(glow_rect, SLICE_ROUNDING + 3.0,
                with_alpha(accent.glow(), (hov_t * 0.5 * 255.0 * anim) as u8));
        } else {
            // Subtle shadow always
            let shadow_rect = btn_rect.translate(egui::vec2(0.0, 1.0)).expand(1.0);
            painter.rect_filled(shadow_rect, SLICE_ROUNDING,
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, (0.3 * 255.0 * anim) as u8));
        }

        // Background + border
        painter.rect_filled(btn_rect, SLICE_ROUNDING,
            with_alpha(bg_col, (bg_col.a().max(200) as f32 * anim) as u8));
        painter.rect_stroke(btn_rect, SLICE_ROUNDING,
            egui::Stroke::new(1.0, with_alpha(border_col, global_alpha)), egui::StrokeKind::Middle);

        // Content
        if item.square {
            if item.polygroup_cube {
                // Multi-colour isometric cube (polygroup faces), not a text glyph.
                draw_polygroup_cube_icon(painter, pos, btn_h * 0.42, global_alpha);
            } else {
                // Icon-only centered
                painter.text(pos, egui::Align2::CENTER_CENTER, item.icon,
                    egui::FontId::proportional(FONT_ICON + 2.0), with_alpha(icon_col, global_alpha));
            }
        } else {
            // Icon + label + shortcut left-aligned
            let mut x = btn_rect.min.x + SLICE_PAD_H;
            let cy = pos.y;

            if !item.icon.is_empty() {
                painter.text(egui::pos2(x + 7.0, cy), egui::Align2::CENTER_CENTER, item.icon,
                    egui::FontId::proportional(FONT_ICON), with_alpha(icon_col, global_alpha));
                x += 14.0 + SLICE_GAP;
            }

            let font_weight = if hov_t > 0.5 { FONT_LABEL + 0.5 } else { FONT_LABEL };
            painter.text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, item.label,
                egui::FontId::proportional(font_weight), with_alpha(text_col, global_alpha));

            if !item.shortcut.is_empty() {
                let sc_col = if hov_t > 0.1 { accent_col } else { SHORTCUT_COL };
                painter.text(
                    egui::pos2(btn_rect.max.x - SLICE_PAD_H - if item.has_submenu { 10.0 } else { 0.0 }, cy),
                    egui::Align2::RIGHT_CENTER, item.shortcut,
                    egui::FontId::proportional(FONT_SHORTCUT),
                    with_alpha(sc_col, global_alpha));
            }

            if item.has_submenu {
                painter.text(
                    egui::pos2(btn_rect.max.x - SLICE_PAD_H + 2.0, cy),
                    egui::Align2::RIGHT_CENTER, "\u{25B8}",
                    egui::FontId::proportional(8.0),
                    with_alpha(if hov_t > 0.1 { accent_col } else { SHORTCUT_COL }, global_alpha));
            }
        }

        // Yellow pivot / center accessory (OBJECT) — plain disc only, offset from the pill.
        if item.pivot_dot {
            let dot = egui::pos2(btn_rect.max.x + PIVOT_DOT_GAP, pos.y);
            let hot = state.hovered_object_pivot && state.hovered_inner == Some(i);
            let dr = if hot { PIVOT_DOT_R + 1.0 } else { PIVOT_DOT_R };
            painter.circle_filled(dot, dr, with_alpha(PIVOT_DOT_COL, global_alpha));
            if hot {
                painter.circle_stroke(
                    dot,
                    dr,
                    egui::Stroke::new(1.0, with_alpha(egui::Color32::WHITE, (180.0 * anim) as u8)),
                );
            }
        }

        // Per-button pop-in: stagger by index
        // (already handled by anim_progress affecting radius lerp)
    }

    // ── Outer items (anchored at inner button, perpendicular layout) ──
    if let Some(ref outer_set) = def.outer {
        if let Some(inner_idx) = state.hovered_inner {
            if inner_idx < outer_set.rings.len() {
                let outer_items = &outer_set.rings[inner_idx];
                let outer_n = outer_items.len();
                if outer_n > 0 {
                    let inner_hov_t = if inner_idx < state.inner_hover_anim.len() {
                        state.inner_hover_anim[inner_idx]
                    } else { 0.0 };
                    let strip_alpha = (inner_hov_t * anim).clamp(0.0, 1.0);
                    if strip_alpha >= 0.05 {
                        let item_spacing = outer_set.item_radius * 2.5;
                        let positions = outer_item_positions(
                            c, inner_idx, inner_n,
                            def.inner.radius, outer_n, item_spacing,
                        );

                        for (j, oitem) in outer_items.iter().enumerate() {
                            let opos = positions[j];
                            let hov_t = if j < state.outer_hover_anim.len() {
                                state.outer_hover_anim[j]
                            } else { 0.0 };

                            let oacc = oitem.accent;
                            let oacc_col = oacc.color();

                            // Small floating button for outer items
                            let obtn_w = SLICE_H;
                            let obtn_h = SLICE_H;
                            let obtn_rect = egui::Rect::from_center_size(opos, egui::vec2(obtn_w, obtn_h));

                            let obg = if hov_t > 0.1 { oacc.dim() } else { BG_SLICE };
                            let oborder = if hov_t > 0.1 { oacc_col } else { BORDER };

                            // Shadow
                            let shadow_rect = obtn_rect.translate(egui::vec2(0.0, 1.0));
                            painter.rect_filled(shadow_rect, SLICE_ROUNDING,
                                egui::Color32::from_rgba_unmultiplied(0, 0, 0, (0.3 * 255.0 * strip_alpha) as u8));

                            // Hover glow
                            if hov_t > 0.1 {
                                let glow_rect = obtn_rect.expand(3.0);
                                painter.rect_filled(glow_rect, SLICE_ROUNDING + 2.0,
                                    with_alpha(oacc.glow(), (hov_t * 0.4 * 255.0 * strip_alpha) as u8));
                            }

                            painter.rect_filled(obtn_rect, SLICE_ROUNDING,
                                with_alpha(obg, (obg.a().max(200) as f32 * strip_alpha) as u8));
                            painter.rect_stroke(obtn_rect, SLICE_ROUNDING,
                                egui::Stroke::new(1.0, with_alpha(oborder, (255.0 * strip_alpha) as u8)), egui::StrokeKind::Middle);

                            let ocol = if hov_t > 0.3 {
                                with_alpha(oacc_col, (255.0 * strip_alpha) as u8)
                            } else {
                                with_alpha(oitem.color, (255.0 * strip_alpha) as u8)
                            };

                            // Icon
                            painter.text(
                                egui::pos2(opos.x, opos.y - 4.0), egui::Align2::CENTER_CENTER,
                                oitem.icon, egui::FontId::proportional(FONT_ICON), ocol);
                            // Label below
                            painter.text(
                                egui::pos2(opos.x, opos.y + 9.0), egui::Align2::CENTER_CENTER,
                                oitem.label, egui::FontId::proportional(9.0),
                                with_alpha(if hov_t > 0.1 { TEXT_BRIGHT } else { TEXT_DIM }, (255.0 * strip_alpha) as u8));
                        }
                    }
                }
            }
        }
    }

    // ── Tooltip ──
    let tooltip_y = c.y + def.bg_radius + 26.0;
    let mut tooltip_text: Option<(String, egui::Color32)> = None;

    if let Some(outer_idx) = state.hovered_outer {
        if let Some(ref outer_set) = def.outer {
            if let Some(inner_idx) = state.hovered_inner {
                if inner_idx < outer_set.rings.len() {
                    let items = &outer_set.rings[inner_idx];
                    if outer_idx < items.len() {
                        let oitem = &items[outer_idx];
                        let text = if oitem.shortcut.is_empty() {
                            oitem.label.to_string()
                        } else { format!("{} ({})", oitem.label, oitem.shortcut) };
                        tooltip_text = Some((text, oitem.accent.color()));
                    }
                }
            }
        }
    } else if let Some(desc_fn) = def.description_fn {
        if let Some(hi) = state.hovered_inner {
            let desc = desc_fn(hi);
            if !desc.is_empty() {
                tooltip_text = Some((desc.to_string(), egui::Color32::from_gray(195)));
            }
        }
    }

    if let Some((text, color)) = tooltip_text {
        let font = egui::FontId::proportional(10.5);
        let galley = painter.layout_no_wrap(text.clone(), font.clone(), color);
        let tw = galley.size().x;
        let pad = 12.0;
        let rect = egui::Rect::from_center_size(
            egui::pos2(c.x, tooltip_y),
            egui::vec2(tw + pad * 2.0, galley.size().y + 10.0),
        );
        painter.rect_filled(rect.translate(egui::vec2(1.0, 2.0)), 5.0,
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, (40.0 * anim) as u8));
        painter.rect_filled(rect, 5.0, with_alpha(egui::Color32::from_rgb(0x1e, 0x1e, 0x1e), global_alpha));
        painter.rect_stroke(rect, 5.0, egui::Stroke::new(0.5, with_alpha(BORDER, global_alpha)), egui::StrokeKind::Middle);
        painter.text(egui::pos2(c.x, tooltip_y), egui::Align2::CENTER_CENTER,
            &text, font, with_alpha(color, global_alpha));
    }

    // ── Center hub — clickable mode toggle button ──
    let center_hov = state.hovered_center;
    let hub_bg = if center_hov {
        egui::Color32::from_rgb(0x2a, 0x2a, 0x2a)
    } else {
        egui::Color32::from_rgb(0x19, 0x19, 0x19)
    };

    // Hover glow
    if center_hov {
        painter.circle_filled(c, hub_r + 3.0,
            with_alpha(PRIMARY, (0.25 * 255.0 * anim) as u8));
    }

    painter.circle_filled(c, hub_r, with_alpha(hub_bg, global_alpha));

    // Direction wedge (tracks hovered sector — only when NOT hovering center)
    if !center_hov {
        if let Some(hi) = state.hovered_inner {
            let hov_t = if hi < state.inner_hover_anim.len() { state.inner_hover_anim[hi] } else { 0.0 };
            let accent_col = if hi < def.inner.items.len() { def.inner.items[hi].accent.color() } else { PRIMARY };

            let wedge_angle = TAU / inner_n as f32;
            let center_angle = sector_center_angle(hi, inner_n);
            let angle_start = center_angle - wedge_angle * 0.5;
            let segs = 16;
            let hub_inner = 5.0;
            let mut wedge_pts = Vec::with_capacity(segs * 2 + 4);
            for s in 0..=segs {
                let t = s as f32 / segs as f32;
                let a = angle_start + wedge_angle * t;
                wedge_pts.push(egui::pos2(c.x + a.cos() * hub_inner, c.y + a.sin() * hub_inner));
            }
            for s in (0..=segs).rev() {
                let t = s as f32 / segs as f32;
                let a = angle_start + wedge_angle * t;
                wedge_pts.push(egui::pos2(c.x + a.cos() * hub_r, c.y + a.sin() * hub_r));
            }
            let wedge_alpha = (lerp_f32(0.2, 0.55, hov_t) * 255.0 * anim) as u8;
            painter.add(egui::Shape::convex_polygon(wedge_pts, with_alpha(accent_col, wedge_alpha), egui::Stroke::NONE));

            painter.circle_stroke(c, hub_r,
                egui::Stroke::new(1.5, with_alpha(accent_col, (hov_t * 0.35 * 255.0 * anim) as u8)));
        }
    }

    // Border
    let border_col = if center_hov { PRIMARY } else { BORDER };
    painter.circle_stroke(c, hub_r,
        egui::Stroke::new(if center_hov { 1.5 } else { 1.0 }, with_alpha(border_col, global_alpha)));

    // Center label — always visible, acts as toggle indicator
    if !def.center_label.is_empty() {
        let label_col = if center_hov { TEXT_BRIGHT } else { egui::Color32::from_gray(140) };
        painter.text(c, egui::Align2::CENTER_CENTER, def.center_label,
            egui::FontId::proportional(FONT_CENTER + 1.0),
            with_alpha(label_col, global_alpha));
    }
}

// ================================================================
// Geometry helpers
// ================================================================

fn angle_to_sector(angle: f32, n: usize) -> usize {
    let sector_size = TAU / n as f32;
    let shifted = angle + FRAC_PI_2 + sector_size * 0.5;
    let normalized = shifted.rem_euclid(TAU);
    let idx = (normalized / sector_size) as usize;
    idx.min(n - 1)
}

fn sector_center_angle(i: usize, n: usize) -> f32 {
    let sector_size = TAU / n as f32;
    -FRAC_PI_2 + i as f32 * sector_size
}

/// Compute position and layout direction for outer (secondary) items.
/// Anchored beyond the inner item's outer edge, extending perpendicular:
/// - Top/bottom items → horizontal spread, pushed further above/below
/// - Side items → vertical spread, pushed further left/right
fn outer_item_positions(
    center: egui::Pos2,
    inner_idx: usize,
    inner_n: usize,
    inner_radius: f32,
    outer_count: usize,
    item_spacing: f32,
) -> Vec<egui::Pos2> {
    let angle = sector_center_angle(inner_idx, inner_n);
    let cos_a = angle.cos();
    let sin_a = angle.sin();

    // Anchor = pushed well beyond the inner button's outer edge
    let radial_push = inner_radius + 100.0;
    let anchor = egui::pos2(
        center.x + cos_a * radial_push,
        center.y + sin_a * radial_push,
    );

    // Determine if item is more top/bottom or side based on angle
    let is_top_bottom = cos_a.abs() < sin_a.abs();

    // Tight spacing so secondary items don't reach neighboring primaries
    let tight_spacing = item_spacing.min(42.0);
    let total_span = (outer_count as f32 - 1.0) * tight_spacing;
    let half_span = total_span * 0.5;

    (0..outer_count).map(|j| {
        let offset = j as f32 * tight_spacing - half_span;
        if is_top_bottom {
            // Top/bottom → spread horizontally from anchor
            egui::pos2(anchor.x + offset, anchor.y)
        } else {
            // Side → spread vertically from anchor
            egui::pos2(anchor.x, anchor.y + offset)
        }
    }).collect()
}

// ================================================================
// Predefined pie menu definitions
// ================================================================

// Accent shortcuts
const P: Accent = Accent::Primary;
const S: Accent = Accent::Secondary;
#[allow(dead_code)]
const V: Accent = Accent::Violet;

fn tab_desc_poly(idx: usize) -> &'static str {
    // Blender layout on 8 sectors: 0=Object(top), 2=Face(right), 3=Group(bottom-right when active),
    // 4=Edge(bottom), 6=Vertex(left)
    match idx {
        0 => "Select whole objects \u{2022} transform, duplicate",
        2 => "Select faces \u{2022} extrude, inset, delete",
        3 => "Select polygroups \u{2022} click a face to select its whole group",
        4 => "Select edges \u{2022} bevel, loop cut, ring",
        5 => "Sculpt \u{2022} Surface sculpt on a mesh (3D-Coat) / voxel room on a voxel",
        6 => "Select vertices \u{2022} merge, weld, adjust",
        _ => "",
    }
}

/// Small isometric cube with three distinct face colours (polygroup look).
fn draw_polygroup_cube_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    half: f32,
    alpha: u8,
) {
    // Distinct saturated faces (match polygroup palette feel: green / blue / orange).
    let top = with_alpha(egui::Color32::from_rgb(0x6D, 0xE0, 0x3A), alpha);
    let left = with_alpha(egui::Color32::from_rgb(0x3A, 0x7B, 0xE0), alpha);
    let right = with_alpha(egui::Color32::from_rgb(0xE0, 0x9E, 0x3A), alpha);
    let edge = with_alpha(egui::Color32::from_rgb(0x18, 0x18, 0x18), (alpha as f32 * 0.85) as u8);

    // Isometric proportions
    let hx = half * 0.95;
    let hy = half * 0.55;
    let hz = half * 0.85; // vertical

    // Center of cube slightly above button center so it sits balanced.
    let c = egui::pos2(center.x, center.y + half * 0.08);

    // 7 key points of a classic isometric cube
    let top_c = egui::pos2(c.x, c.y - hz * 0.55);
    let mid_l = egui::pos2(c.x - hx, c.y - hy * 0.15);
    let mid_r = egui::pos2(c.x + hx, c.y - hy * 0.15);
    let mid_f = egui::pos2(c.x, c.y + hy * 0.35); // front ridge of top face
    let bot_l = egui::pos2(c.x - hx, c.y + hy * 0.15 + hz * 0.45);
    let bot_r = egui::pos2(c.x + hx, c.y + hy * 0.15 + hz * 0.45);
    let bot_f = egui::pos2(c.x, c.y + hy * 0.35 + hz * 0.55);

    // Top face (diamond)
    painter.add(egui::Shape::convex_polygon(
        vec![top_c, mid_r, mid_f, mid_l],
        top,
        egui::Stroke::new(0.8, edge),
    ));
    // Left face
    painter.add(egui::Shape::convex_polygon(
        vec![mid_l, mid_f, bot_f, bot_l],
        left,
        egui::Stroke::new(0.8, edge),
    ));
    // Right face
    painter.add(egui::Shape::convex_polygon(
        vec![mid_r, mid_f, bot_f, bot_r],
        right,
        egui::Stroke::new(0.8, edge),
    ));
}

fn tab_desc_sdf(idx: usize) -> &'static str {
    match idx {
        0 => "Select whole SDF objects \u{2022} transform, boolean ops",
        1 => "Select individual SDF primitives \u{2022} transform, edit params",
        3 => "Cycle mode \u{2022} scroll wheel steps through prims in active body",
        _ => "",
    }
}

fn tab_desc_nurbs(idx: usize) -> &'static str {
    // Same layout / names as poly pie (Object / Face / Edge / Vertex).
    match idx {
        0 => "OBJECT — whole NURBS body \u{2022} transform, duplicate",
        2 => "FACE — surface patch \u{2022} select whole surface",
        4 => "EDGE — hull / isoparm edges \u{2022} row-col CV chains",
        6 => "VERTEX — control points (CVs) \u{2022} G/R/S shape the surface",
        _ => "",
    }
}

/// Build the Tab pie for the current modeling pillar.
///
/// `polygroups_active`: when true (poly body has groups and/or group colours are on), the Poly pie
/// exposes **GROUP** on the top-right diagonal for mouse polygroup selection. Hidden otherwise.
pub(crate) fn tab_pie_def(mode: PieMode) -> PieMenuDef {
    tab_pie_def_ex(mode, false)
}

pub(crate) fn tab_pie_def_ex(mode: PieMode, polygroups_active: bool) -> PieMenuDef {
    match mode {
        PieMode::Sdf => tab_pie_def_sdf(),
        PieMode::Poly => tab_pie_def_poly(polygroups_active),
        PieMode::Nurbs => tab_pie_def_nurbs(),
    }
}

fn tab_pie_def_poly(polygroups_active: bool) -> PieMenuDef {
    // 8-sector ring, Blender Tab-pie layout: cardinals hold the modes,
    // diagonals are spacers except Sculpt (bottom-left) and optional GROUP (bottom-right).
    // GROUP is a small cube icon only — does not change pie mechanics.
    let bottom_right = if polygroups_active {
        PieItem::group_cube("GROUP", "G", 3, V)
    } else {
        PieItem::spacer()
    };
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("OBJECT", "\u{25C9}", "1", 0, P).with_pivot_dot(), // top — + yellow center
                PieItem::spacer(),                                 // top-right diag (idx 1)
                PieItem::new("FACE",   "\u{25A0}", "4", 2, S),    // right  (idx 2)
                bottom_right,                                      // bottom-right diag (idx 3) — GROUP
                PieItem::new("EDGE",   "\u{2500}", "3", 4, S),    // bottom (idx 4)
                PieItem::square("\u{1F9CA}", "SCULPT", "5", 5, V), // bottom-left — Surface on poly, voxel room on voxel
                PieItem::new("VERTEX", "\u{25CF}", "2", 6, P),    // left   (idx 6)
                PieItem::spacer(),                                 // top-left diag (idx 7)
            ],
            radius: 90.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 96.0,
        center_label: "POLY",
        description_fn: Some(tab_desc_poly),
    }
}

fn tab_pie_def_sdf() -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("OBJECT", "\u{25C9}", "1", 0, P).with_pivot_dot(), // top — + yellow center
                PieItem::new("PRIM",   "\u{25C6}", "2", 1, S),   // right  (idx 1)
                PieItem::spacer(),                                 // bottom (idx 2) — hidden
                PieItem::new("CYCLE",  "\u{21BB}", "3", 2, V),   // left   (idx 3)
            ],
            radius: 90.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 96.0,
        center_label: "SDF",
        description_fn: Some(tab_desc_sdf),
    }
}

fn tab_pie_def_nurbs() -> PieMenuDef {
    // Match poly Tab-pie layout & labels (Object / Face / Edge / Vertex).
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("OBJECT", "\u{25C9}", "1", 0, P).with_pivot_dot(), // top
                PieItem::spacer(),                                 // top-right
                PieItem::new("FACE",   "\u{25A0}", "4", 2, S),    // right
                PieItem::spacer(),                                 // bottom-right
                PieItem::new("EDGE",   "\u{2500}", "3", 4, S),    // bottom
                PieItem::spacer(),                                 // bottom-left
                PieItem::new("VERTEX", "\u{25CF}", "2", 6, P),    // left — CVs
                PieItem::spacer(),                                 // top-left
            ],
            radius: 90.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 96.0,
        center_label: "NURBS",
        description_fn: Some(tab_desc_nurbs),
    }
}

fn view_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Front orthographic view (-Z)",
        1 => "Right orthographic view (+X)",
        2 => "Bottom orthographic view (+Y)",
        3 => "Back orthographic view (+Z)",
        4 => "Left orthographic view (-X)",
        5 => "Top orthographic view (-Y)",
        _ => "",
    }
}

pub(crate) fn view_pie_def() -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Front",  "\u{2191}", "1", 0, P),
                PieItem::new("Right",  "\u{2192}", "2", 1, S),
                PieItem::new("Bottom", "\u{2299}", "3", 2, S),
                PieItem::new("Back",   "\u{2193}", "4", 3, P),
                PieItem::new("Left",   "\u{2190}", "5", 4, S),
                PieItem::new("Top",    "\u{2295}", "6", 5, S),
            ],
            radius: 90.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 96.0,
        center_label: "VIEW",
        description_fn: Some(view_desc),
    }
}

fn z_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Physically-based shading with full lighting",
        1 => "Real-time ray tracing for reflections and shadows",
        2 => "Path tracing for global illumination",
        3 => "MatCap shading for quick sculpt preview",
        4 => "Direct lighting with materials, no reflections",
        5 => "Solid color with ambient occlusion",
        6 => "Wireframe overlay for topology inspection",
        _ => "",
    }
}

/// Alt+A Align / Flatten pie (poly edit) — Blender MACHIN3 Align Pie layout:
/// Top / Right / Bottom / Left (view-relative). Flatten = scale-to-0 on that axis
/// through the selection centroid. Center = flatten onto active workplane.
pub(crate) fn align_pie_def() -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Top", "\u{2191}", "8", 0, P),
                PieItem::new("Right", "\u{2192}", "6", 1, S),
                PieItem::new("Bottom", "\u{2193}", "2", 2, P),
                PieItem::new("Left", "\u{2190}", "4", 3, S),
            ],
            radius: 90.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 94.0,
        center_label: "ALIGN",
        description_fn: Some(align_pie_desc),
    }
}

fn align_pie_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Align Top — flatten selection along view up",
        1 => "Align Right — flatten selection along view right",
        2 => "Align Bottom — flatten selection along view down",
        3 => "Align Left — flatten selection along view left",
        _ => "Center: flatten onto active workplane",
    }
}

/// Alt+X Symmetry pie — 4 DIRECTION slices (up/right/down/left, matching `sector_center_angle(i, 4)`).
/// The flicked direction is the mirror direction (flick right = copy left half onto right). Default
/// flick = Mirror; c/i/m keys run Cut / Instance / Modifier in the pointed direction; o/p set the
/// center. `center_label` shows the active center (GEOMETRY/ORIGIN/PIVOT). See sym_pie.rs.
pub(crate) fn sym_pie_def(center_label: &'static str) -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Up",    "\u{2191}", "", 0, P),
                PieItem::new("Right", "\u{2192}", "", 1, S),
                PieItem::new("Down",  "\u{2193}", "", 2, P),
                PieItem::new("Left",  "\u{2190}", "", 3, S),
            ],
            radius: 90.0,
            item_radius: 24.0,
        },
        outer: None,
        bg_radius: 94.0,
        center_label,
        description_fn: None,
    }
}

pub(crate) fn z_pie_def() -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("PBR",    "\u{2726}", "1", 0, P),
                PieItem::new("RT",     "\u{2604}", "2", 1, S),
                PieItem::new("PT",     "\u{2728}", "3", 2, S),
                PieItem::new("MatCap", "\u{25C9}", "4", 3, P),
                PieItem::new("Lit",    "\u{2600}", "5", 4, S),
                PieItem::new("Solid",  "\u{25C6}", "6", 5, S),
                PieItem::new("Wire",   "\u{2B21}", "7", 6, S),
            ],
            radius: 90.0,
            item_radius: 24.0,
        },
        outer: None,
        bg_radius: 94.0,
        center_label: "",
        description_fn: Some(z_desc),
    }
}

// ============================================================
// Shift+O: Viewport Overlay pie (master + per-layer toggles)
// ============================================================

fn overlay_pie_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Toggle light icons / light gizmos in the viewport",
        1 => "Toggle the workplane overlay",
        2 => "Toggle the floor / world grid",
        3 => "Toggle the transform gizmo (G/R/S handles)",
        4 => "Toggle selection outline / rim on selected objects",
        5 => "Toggle poly edit overlays (verts / edges / faces)",
        6 => "Toggle wireframe overlay on shaded meshes",
        7 => "Toggle reflection-probe debug box",
        _ => "",
    }
}

/// Shift+O overlay pie. Center hub = master Overlays ON/OFF.
/// LAYOUT: Lights · Workplane · Grid · Gizmo · SelRim · Poly · Wire · Probe
pub(crate) fn overlay_pie_def(master_on: bool) -> PieMenuDef {
    let center = if master_on { "ON" } else { "OFF" };
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Lights",    "\u{2600}", "1", 0, P),
                PieItem::new("Workplane", "\u{2B1A}", "2", 1, S),
                PieItem::new("Grid",      "\u{25A6}", "3", 2, S),
                PieItem::new("Gizmo",     "\u{2922}", "4", 3, P),
                PieItem::new("Sel Rim",   "\u{25C9}", "5", 4, V),
                PieItem::new("Poly",      "\u{25A3}", "6", 5, S),
                PieItem::new("Wire",      "\u{2B21}", "7", 6, S),
                PieItem::new("Probe",     "\u{25CE}", "8", 7, V),
            ],
            radius: 108.0,
            item_radius: 24.0,
        },
        outer: None,
        bg_radius: 112.0,
        center_label: center,
        description_fn: Some(overlay_pie_desc),
    }
}

fn reset_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Reset position to world origin (0, 0, 0)",
        1 => "Reset rotation to zero (0\u{00B0}, 0\u{00B0}, 0\u{00B0})",
        2 => "Reset scale to uniform (1, 1, 1)",
        3 => "Reset position, rotation, and scale to defaults",
        _ => "",
    }
}

pub(crate) fn reset_pie_def() -> PieMenuDef {
    let col_reset = Accent::Primary;
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Position", "\u{2316}", "", 0, col_reset),
                PieItem::new("Rotation", "\u{21BB}", "", 1, col_reset),
                PieItem::new("Scale",    "\u{2922}", "", 2, col_reset),
                {
                    let mut item = PieItem::new("All", "\u{2718}", "", 3, col_reset);
                    item.color = egui::Color32::from_rgb(255, 120, 80);
                    item
                },
            ],
            radius: 90.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 94.0,
        center_label: "RESET",
        description_fn: Some(reset_desc),
    }
}

// ============================================================
// Alt+M: Poly Merge pie (Average / Last / By Distance)
// ============================================================

fn merge_pie_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Merge Average \u{2022} weld selected verts to their centroid",
        1 => "Merge Last \u{2022} weld onto the active (last-selected) vertex",
        2 => "Merge by Distance \u{2022} weld only verts closer than a small threshold",
        _ => "",
    }
}

/// Alt+M merge pie. Requires Vertex mode + 2+ selected verts.
/// LAYOUT: 3 equal slices — Average (top) / Last (lower-right) / By Distance (lower-left).
/// No spacer dead-zones (those used to swallow confirm on release).
pub(crate) fn merge_pie_def() -> PieMenuDef {
    let p = Accent::Primary;
    let s = Accent::Secondary;
    let v = Accent::Violet;
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Merge Average",     "\u{25CB}", "1", 0, p),
                PieItem::new("Merge Last",        "\u{25C9}", "2", 1, s),
                PieItem::new("Merge by Distance", "\u{2922}", "3", 2, v),
            ],
            radius: 100.0,
            item_radius: 28.0,
        },
        outer: None,
        bg_radius: 105.0,
        center_label: "MERGE",
        description_fn: Some(merge_pie_desc),
    }
}

/// B-key spine corner type pie (Sharp / Bezier / Chamfer) for SpinePipe.
fn spine_corner_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Hard corner (polyline join)",
        1 => "Bezier smooth — drag handles to shape",
        2 => "Chamfer / bevel join",
        _ => "",
    }
}

pub(crate) fn spine_corner_pie_def() -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Sharp",   "◆", "", 0, Accent::Primary),
                PieItem::new("Bezier",  "∿", "", 1, Accent::Secondary),
                PieItem::new("Chamfer", "◢", "", 2, Accent::Violet),
            ],
            radius: 88.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 92.0,
        center_label: "JOIN",
        description_fn: Some(spine_corner_desc),
    }
}

// ── F-Key Utility Pie ──

fn f_pie_desc(idx: usize) -> &'static str {
    match idx {
        3 => "Frame camera on active body",
        1 => "Toggle body isolation",
        0 => "Toggle context display: ghost / normal / hidden",
        _ => "",
    }
}

// ============================================================
// D-key: Pivot placement pie menu
// ============================================================

fn pivot_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Pivot at AABB / mass center (bounds)",
        1 => "Pivot at world origin (0, 0, 0)",
        2 => "Pivot at workplane origin",
        3 => "Pivot at active prim / object origin",
        4 => "Pivot at surface under cursor",
        5 => "Drag pivot freely in the viewport",
        6 => "Toggle gizmo space: Local ↔ Global",
        _ => "",
    }
}

/// Full DCC-style center / pivot pie — used by D-key and the yellow OBJECT dot.
/// Center hub toggles Persistent / Temporary pivot.
pub(crate) fn d_pie_def(persistent: bool) -> PieMenuDef {
    let center = if persistent { "Persistent" } else { "Temporary" };
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Mass",     "\u{25CE}", "1", 0, P), // top — bounds / mass
                PieItem::new("Origin",   "\u{2316}", "2", 1, S), // right — world 0
                PieItem::new("Plane",    "\u{2B1A}", "3", 2, S), // bottom-right — workplane
                PieItem::new("Object",   "\u{25C9}", "4", 3, P), // bottom — active object
                PieItem::new("Surface",  "\u{25C8}", "5", 4, V), // bottom-left — under cursor
                PieItem::new("Drag",     "\u{2725}", "6", 5, V), // left — free drag
                PieItem::new("Local/W",  "\u{2928}", "7", 6, S), // top-left — local/global
            ],
            radius: 105.0,
            item_radius: 26.0,
        },
        outer: None,
        bg_radius: 110.0,
        center_label: center,
        description_fn: Some(pivot_desc),
    }
}

/// Alias for the yellow-dot opener (same pie as D-key).
pub(crate) fn center_pie_def(persistent: bool) -> PieMenuDef {
    d_pie_def(persistent)
}

// ============================================================
// T-key: Action Center pie (Modo-style)
// ============================================================

fn action_center_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Automatic \u{2022} selection center; Shift+RMB places pivot (snaps face/edge centers)",
        1 => "Selection \u{2022} center + axes from selection normals",
        2 => "Workplane \u{2022} workplane origin + axes (location and orientation)",
        3 => "Element \u{2022} live face/edge/vert frame (+Y normal, +X edge); Shift+RMB also picks",
        4 => "Screen \u{2022} view frame axes, camera target center",
        5 => "Origin \u{2022} world (0,0,0), world axes",
        6 => "Pivot \u{2022} item/body pivot (D pie places it)",
        7 => "None \u{2022} selection center; clears placed Automatic pivot",
        _ => "",
    }
}

/// T-key Action Center pie. Center hub shows active mode.
/// LAYOUT: Automatic · Selection · Workplane · Element · Screen · Origin · Pivot · None.
pub(crate) fn t_pie_def(center_label: &'static str) -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Automatic",  "\u{2726}", "1", 0, P),
                PieItem::new("Selection",  "\u{25A3}", "2", 1, S),
                PieItem::new("Workplane",  "\u{2B1A}", "3", 2, S),
                PieItem::new("Element",    "\u{25C9}", "4", 3, V),
                PieItem::new("Screen",     "\u{25A1}", "5", 4, S),
                PieItem::new("Origin",     "\u{2316}", "6", 5, P),
                PieItem::new("Pivot",      "\u{25CE}", "7", 6, S),
                PieItem::new("None",       "\u{2014}", "8", 7, V),
            ],
            radius: 108.0,
            item_radius: 24.0,
        },
        outer: None,
        bg_radius: 112.0,
        center_label,
        description_fn: Some(action_center_desc),
    }
}

// ============================================================
// X-key: Poly mesh boolean pie (cutter → targets)
// ============================================================

fn boolean_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Union \u{2022} merge cutter into active target",
        2 => "Subtract \u{2022} carve cutter out of active target (select target last)",
        4 => "Intersect \u{2022} keep only the overlap on active target",
        5 => "Cut \u{2022} imprint edge loops where shapes meet (no volume remove)",
        6 => "Toggle: keep the cutter object/island after applying",
        _ => "",
    }
}

/// X-key poly boolean pie. Active poly object = TARGET (carved); other selected =
/// CUTTER. Edit-mode island selection = cutter. Center = Live Boolean (reserved).
///
/// LAYOUT ANCHOR: Union top / Subtract right / Intersect bottom is the muscle-memory
/// geometry EVERY boolean pie mirrors (see `sdf_boolean_pie_def`). Mains never move;
/// new ops append to LEFT extras, then diagonals. Slot 5 = Cut (imprint), slot 6 = Keep Cutter.
pub(crate) fn boolean_pie_def(keep_cutter: bool) -> PieMenuDef {
    let keep_label = if keep_cutter { "Keep Cutter: ON" } else { "Keep Cutter: OFF" };
    let keep_accent = if keep_cutter { S } else { V };
    // 8-sector ring so Cut + KeepCutter sit on the left without displacing mains
    // (same layout rule as the SDF boolean pie).
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Union",     "\u{222A}", "1", 0, P), // top            — MAIN
                PieItem::spacer(),                                // top-right
                PieItem::new("Subtract",  "\u{2212}", "2", 2, S), // right          — MAIN (was "Difference")
                PieItem::spacer(),                                // bottom-right
                PieItem::new("Intersect", "\u{2229}", "3", 4, P), // bottom         — MAIN
                PieItem::new("Cut",       "\u{2702}", "5", 5, V), // bottom-left    — imprint edge loops
                PieItem::new(keep_label,  "\u{25D0}", "4", 6, keep_accent), // left — toggle
                PieItem::spacer(),                                // top-left
            ],
            radius: 100.0,
            item_radius: 28.0,
        },
        outer: None,
        bg_radius: 105.0,
        center_label: "LIVE\u{2026}",
        description_fn: Some(boolean_desc),
    }
}

// ============================================================
// X-key: SDF boolean pie (op type for selected prims / sub-bodies)
// ============================================================

fn sdf_bool_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Union \u{2022} add to the body (keeps blend k)",
        1 => "Extrude \u{2022} raise/recess the surface in this prim's footprint (boolean \u{2022} handle: vertical strength, horizontal width)",
        2 => "Subtract \u{2022} carve from the body (keeps blend k)",
        4 => "Intersect \u{2022} keep only the overlap",
        6 => "Seam \u{2022} scribed groove (handle: vertical depth \u{2014} up flips to a raised bead \u{2014} horizontal width)",
        _ => "",
    }
}

/// X-key SDF boolean pie — replaces the old blind Union→Subtract→Intersect cycle.
///
/// LAYOUT RULE (muscle memory — owner-locked 2026-07-14): the 3 mains sit at the
/// EXACT same spots as the poly boolean pie (`boolean_pie_def`), FOREVER:
///   Union TOP (key 1) \u{2022} Subtract/Difference RIGHT (key 2) \u{2022} Intersect BOTTOM (key 3).
/// The mains NEVER move. Additional SDF ops append to the remaining positions:
/// LEFT first (key 4), then diagonals (keys 5-8) — without ever displacing a main.
/// Current: Seam LEFT (single slot — groove default; the viewport diamond handle
/// drags signed depth vertically / width horizontally and flips groove↔bead
/// through zero), Extrude top-right (single slot — the `BooleanOp::Extrude`
/// FOOTPRINT boolean, raise default; the handle drags signed strength vertically
/// / width horizontally and flips raise↔recess through zero — owner 2026-07-14,
/// supersedes the old Emboss/Deboss relief-modifier push, which self-stamped the
/// prim's own field and read as a scale). Bottom-right (idx 3) is a free spacer.
///
/// Mirrors `ui_panels::bool_op_menu_items` (context-menu set): core op types only —
/// smooth/chamfer variants stay on the viewport blend-k / profile controls (owner
/// decision). Applied per selected non-base prim (or selected sub-body), preserving
/// each target's blend k / seam params. Same hold→hover→release interaction as the
/// poly boolean pie above.
pub(crate) fn sdf_boolean_pie_def() -> PieMenuDef {
    // 8-sector ring: cardinals = mains + Seam, diagonals = extra ops (Extrude).
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Union",     "\u{222A}",         "1", 0, P), // top            (idx 0) — MAIN, never moves
                PieItem::new("Extrude",   "\u{25B4}",         "5", 1, S), // top-right diag (idx 1) — BooleanOp::Extrude (raise default; handle flips raise↔recess)
                PieItem::new("Subtract",  "\u{2212}",         "2", 2, S), // right          (idx 2) — MAIN, never moves
                PieItem::spacer(),                                        // bottom-right   (idx 3) — free spacer
                PieItem::new("Intersect", "\u{2229}",         "3", 4, P), // bottom         (idx 4) — MAIN, never moves
                PieItem::spacer(),                                        // bottom-left    (idx 5) — next op goes here
                PieItem::new("Seam",      "\u{2248}\u{2212}", "4", 6, V), // left           (idx 6)
                PieItem::spacer(),                                        // top-left       (idx 7) — then here
            ],
            radius: 100.0,
            item_radius: 28.0,
        },
        outer: None,
        bg_radius: 105.0,
        // Center opens the special-commands pie (Extract Intersection, …).
        center_label: "MORE\u{2026}",
        description_fn: Some(sdf_bool_desc),
    }
}

// ============================================================
// X-key SDF boolean pie CENTER → special commands (extensible)
// ============================================================

fn sdf_special_desc(idx: usize) -> &'static str {
    match idx {
        0 => "Extract Intersection \u{2022} new body = solid from Subtract or Intersect result",
        _ => "",
    }
}

/// Second-level pie opened from the center of the SDF boolean pie.
/// Slot 0 = Extract Intersection; remaining slots reserved for future special ops.
pub(crate) fn sdf_special_pie_def() -> PieMenuDef {
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new("Extract", "\u{25A3}", "1", 0, P), // top — Extract from Subtract/Intersect
                PieItem::spacer(),
                PieItem::spacer(),
                PieItem::spacer(),
            ],
            radius: 100.0,
            item_radius: 28.0,
        },
        outer: None,
        bg_radius: 105.0,
        center_label: "BACK",
        description_fn: Some(sdf_special_desc),
    }
}

/// Slot in `sdf_boolean_pie_def` matching the given op (active-item highlight).
/// Keys purely on the boolean op — the pie no longer reflects (or touches) the
/// legacy Emboss/Deboss relief modifiers.
pub(crate) fn sdf_bool_op_slot(op: &core_math::primitives::BooleanOp) -> usize {
    use core_math::primitives::BooleanOp;
    match op {
        BooleanOp::Seam { .. } => 6,    // one Seam slot — polarity lives on the handle
        BooleanOp::Extrude { .. } => 1, // one Extrude slot — polarity lives on the handle
        BooleanOp::Intersect | BooleanOp::SmoothIntersect { .. } => 4,
        op if op.is_additive() => 0, // Union + deprecated ChamferUnion
        _ => 2,                      // Subtract + deprecated ChamferSubtract
    }
}

/// Active-item slot for a PRIM target — keys on the op (same as the handle).
pub(crate) fn sdf_bool_pie_active_slot(p: &core_math::primitives::SdfPrimitive) -> usize {
    sdf_bool_op_slot(&p.operation)
}

// ============================================================
// Ctrl+R: SDF bake-resolution pie (per-top-level-body grid)
// ============================================================

/// Slot descriptions mirror the Grid picker's hover texts (properties_render.rs).
fn res_desc(idx: usize) -> &'static str {
    match idx {
        0 => "64\u{B3} \u{2022} draft bake \u{2014} fastest, coarse",
        1 => "128\u{B3} \u{2022} light preview",
        2 => "256\u{B3} \u{2022} balanced detail",
        3 => "512\u{B3} \u{2022} DC triangles (sparse bricks when active). 12GB+ GPU recommended",
        4 => "1024\u{B3} \u{2022} sparse bricks \u{2192} DC triangles (~4\u{D7} the 512\u{B3} pool). 16GB+ GPU recommended",
        5 => "2048\u{B3} \u{2022} sparse bricks, opt-in (2048-virtual) \u{2014} self-heals down a notch at the GPU buffer ceiling. 16-24GB GPU",
        _ => "",
    }
}

/// Same, with the blocked slots (beyond the dense ceiling) explaining WHY — the same
/// explanation the Grid picker shows on its disabled 1024/2048 buttons.
fn res_desc_blocked(idx: usize) -> &'static str {
    match idx {
        4 | 5 => "Needs the sparse brick pipeline (Developer Tweaks \u{2192} Sparse bake)",
        i => res_desc(i),
    }
}

/// Ctrl+R SDF resolution pie: the six ladder steps of `properties_render::RES_LADDER`
/// (64³…2048³, keep in sync), applied to the ACTIVE top-level body via
/// `apply_body_grid_resolution` (the shared picker apply path). Slots above `sel_max`
/// draw disabled with the picker's explanation; the confirm path refuses them.
pub(crate) fn resolution_pie_def(sel_max: u32) -> PieMenuDef {
    // Mirrors properties_render::RES_LADDER — labels and blocking must track it.
    const LADDER: [u32; 6] = [64, 128, 256, 512, 1024, 2048];
    const LABELS: [&str; 6] = ["64\u{B3}", "128\u{B3}", "256\u{B3}", "512\u{B3}", "1024\u{B3}", "2048\u{B3}"];
    const KEYS:   [&str; 6] = ["1", "2", "3", "4", "5", "6"];
    let items = (0..6)
        .map(|i| {
            let accent = if LADDER[i] >= 1024 { V } else if LADDER[i] >= 512 { S } else { P };
            PieItem::new(LABELS[i], "\u{25A6}", KEYS[i], i, accent)
                .with_disabled(LADDER[i] > sel_max)
        })
        .collect();
    let blocked = LADDER[4] > sel_max;
    PieMenuDef {
        inner: PieRing { items, radius: 90.0, item_radius: 24.0 },
        outer: None,
        bg_radius: 94.0,
        center_label: "RES",
        description_fn: Some(if blocked { res_desc_blocked } else { res_desc }),
    }
}

pub(crate) fn f_pie_def(isolated: bool, context_ghost: bool) -> PieMenuDef {
    let center = if isolated { "Isolated" } else { "Scene" };
    // Context display label: only relevant when isolated
    let ctx_label = if context_ghost { "Context: Ghost" } else { "Context: Normal" };
    let ctx_accent = if context_ghost { V } else { S };
    PieMenuDef {
        inner: PieRing {
            items: vec![
                PieItem::new(ctx_label, "\u{25D1}", "", 0, ctx_accent),               // top — context toggle
                PieItem::new("Isolate",      "\u{25CE}", "", 1, if isolated { S } else { V }), // right
                PieItem::spacer(),                                                    // bottom (reserved)
                PieItem::new("Focus Camera", "\u{2316}", "", 3, P),                   // left
            ],
            radius: 100.0,
            item_radius: 28.0,
        },
        outer: None,
        bg_radius: 105.0,
        center_label: center,
        description_fn: Some(f_pie_desc),
    }
}
