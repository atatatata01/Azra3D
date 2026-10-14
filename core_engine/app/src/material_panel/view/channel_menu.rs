//! Channel-menu UI + texture-space helpers (panel section split from view.rs).

use eframe::egui;
use crate::ui_theme as t;
use super::super::model::*;
use super::kind_label;

/// All channel kinds, in panel order (mirrors `lower::derive`'s ALL).
pub(super) const ALL_KINDS: [ChannelKind; 18] = [
    ChannelKind::BaseColor,
    ChannelKind::Roughness,
    ChannelKind::Metallic,
    ChannelKind::Opacity,
    ChannelKind::Emission,
    ChannelKind::Normal,
    ChannelKind::Bump,
    ChannelKind::Displacement,
    ChannelKind::Clearcoat,
    ChannelKind::ClearcoatRoughness,
    ChannelKind::Sheen,
    ChannelKind::SheenTint,
    ChannelKind::Anisotropy,
    ChannelKind::AnisotropyRotation,
    ChannelKind::Ior,
    ChannelKind::SssRadius,
    ChannelKind::SssColor,
    ChannelKind::GlassTint,
];

/// Category + which renderer(s) actually evaluate the channel.
///
/// Honest mapping from `RasterMaterial` / `gbuffer.wgsl` (Hybrid) and path-trace packing:
/// - **Core maps** and basic PBR scalars: both Hybrid and Path Trace.
/// - **Coat / sheen**: Hybrid approx + Path Trace.
/// - **IOR / glass / SSS**: Path Trace is ground truth; Hybrid has a cheap approx
///   (wrap lighting / tinted glass shade) — not full transport.
/// - **Anisotropy**: packed for analytical SDF PT cold buffer; **not** in Hybrid
///   `RasterMaterial` mesh path — authoring only in Hybrid mesh.
pub(super) struct ChannelMenuGroup {
    /// Short category title (menu section header).
    category: &'static str,
    /// Renderer support chip shown under the category.
    renderer: &'static str,
    /// Hover blurb for the section header.
    hover: &'static str,
    kinds: &'static [ChannelKind],
}

pub(super) const CHANNEL_MENU_GROUPS: &[ChannelMenuGroup] = &[
    ChannelMenuGroup {
        category: "Core",
        renderer: "Hybrid + Path Trace",
        hover: "Base PBR + maps. Live in realtime Hybrid and Path Trace.",
        kinds: &[
            ChannelKind::BaseColor,
            ChannelKind::Roughness,
            ChannelKind::Metallic,
            ChannelKind::Opacity,
            ChannelKind::Emission,
            ChannelKind::Normal,
            ChannelKind::Bump,
            ChannelKind::Displacement,
        ],
    },
    ChannelMenuGroup {
        category: "Coat & sheen",
        renderer: "Hybrid + Path Trace",
        hover: "Clearcoat / sheen lobes. Hybrid uses a realtime approx; PT is full.",
        kinds: &[
            ChannelKind::Clearcoat,
            ChannelKind::ClearcoatRoughness,
            ChannelKind::Sheen,
            ChannelKind::SheenTint,
        ],
    },
    ChannelMenuGroup {
        category: "Transmission & SSS",
        renderer: "Path Trace · Hybrid ≈",
        hover: "Glass IOR / tint and subsurface. Path Trace = ground truth \
                (refraction, caustics, random-walk SSS). Hybrid = approximate only.",
        kinds: &[
            ChannelKind::Ior,
            ChannelKind::GlassTint,
            ChannelKind::SssRadius,
            ChannelKind::SssColor,
        ],
    },
    ChannelMenuGroup {
        category: "Anisotropy",
        renderer: "Path Trace (SDF) · not Hybrid mesh",
        hover: "Anisotropy is in the analytical SDF PT buffer. Hybrid mesh RasterMaterial \
                does not pack these fields yet — no realtime mesh preview.",
        kinds: &[
            ChannelKind::Anisotropy,
            ChannelKind::AnisotropyRotation,
        ],
    },
];

/// Compact renderer chip for a single channel (channel row / button hover).
pub(super) fn kind_renderer_chip(k: ChannelKind) -> &'static str {
    for g in CHANNEL_MENU_GROUPS {
        if g.kinds.contains(&k) {
            return g.renderer;
        }
    }
    "—"
}

/// Draw a categorized channel picker. Returns the clicked kind (if any).
/// Only kinds present in `available` are shown; empty groups are skipped.
pub(super) fn draw_channel_kind_menu(ui: &mut egui::Ui, available: &[ChannelKind]) -> Option<ChannelKind> {
    let mut picked = None;
    let mut any = false;
    for (gi, g) in CHANNEL_MENU_GROUPS.iter().enumerate() {
        let group_kinds: Vec<ChannelKind> = g
            .kinds
            .iter()
            .copied()
            .filter(|k| available.contains(k))
            .collect();
        if group_kinds.is_empty() {
            continue;
        }
        if any {
            ui.add_space(4.0);
        }
        any = true;
        // Section header: category + renderer support.
        ui.label(
            egui::RichText::new(g.category)
                .size(10.5)
                .strong()
                .color(t::TEXT_SECONDARY),
        )
        .on_hover_text(g.hover);
        ui.label(
            egui::RichText::new(g.renderer)
                .size(9.5)
                .italics()
                .color(match gi {
                    // Core / coat — full both → greenish
                    0 | 1 => egui::Color32::from_rgb(120, 170, 140),
                    // Transmission — PT primary → blue
                    2 => egui::Color32::from_rgb(110, 160, 200),
                    // Anisotropy limited → amber
                    _ => egui::Color32::from_rgb(190, 160, 90),
                }),
        )
        .on_hover_text(g.hover);
        for k in group_kinds {
            let resp = ui
                .button(kind_label(k))
                .on_hover_text(format!("{}\n{}", kind_label(k), g.hover));
            if resp.clicked() {
                picked = Some(k);
            }
        }
    }
    // Fallback: any available kind not listed in a group (forward-compat).
    let known: std::collections::HashSet<ChannelKind> = CHANNEL_MENU_GROUPS
        .iter()
        .flat_map(|g| g.kinds.iter().copied())
        .collect();
    let orphans: Vec<ChannelKind> = available
        .iter()
        .copied()
        .filter(|k| !known.contains(k))
        .collect();
    if !orphans.is_empty() {
        if any {
            ui.add_space(4.0);
        }
        ui.label(
            egui::RichText::new("Other")
                .size(10.5)
                .strong()
                .color(t::TEXT_SECONDARY),
        );
        for k in orphans {
            if ui.button(kind_label(k)).clicked() {
                picked = Some(k);
            }
        }
    }
    picked
}

/// Drag speed + range per scalar channel (engine clamp ranges — push_to_pbr).
pub(super) fn kind_scalar_range(k: ChannelKind) -> (f32, std::ops::RangeInclusive<f32>) {
    match k {
        ChannelKind::Ior => (0.005, 1.0..=3.0),
        ChannelKind::Anisotropy => (0.005, -1.0..=1.0),
        ChannelKind::SssRadius => (0.01, 0.0..=10.0),
        ChannelKind::Normal => (0.01, 0.0..=4.0),
        _ => (0.005, 0.0..=1.0),
    }
}

/// Set the material texture space; layers whose override equals the OLD space
/// were effectively inheriting — collapse them to `None` so they follow (the
/// §8 inherited/override split; explicit differing overrides are untouched).
pub(super) fn mp_set_texture_space(doc: &mut MaterialPanelDoc, f: impl FnOnce(&mut TextureSpace)) {
    let old = doc.texture_space;
    for ch in &mut doc.channels {
        for layer in &mut ch.layers {
            if layer.placement.space == Some(old) {
                layer.placement.space = None;
            }
        }
    }
    f(&mut doc.texture_space);
}

/// Read-only "label ......... value" row (Texture space group).
pub(super) fn ro_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(label).size(11.0).color(t::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(value).size(11.0).color(t::TEXT_SECONDARY));
        });
    });
}

pub(super) fn caption(ui: &mut egui::Ui, text: &str) {
    // Owner #9: long informational captions (e.g. the per-layer offset note) must WRAP inside
    // the panel, never run past the right edge. `ui.horizontal` gives children infinite width so
    // `.wrap()` never triggers — instead reserve an indented sub-rect whose max width is the
    // available panel width minus the left indent, then let the wrapped Label fill it.
    const INDENT: f32 = 8.0;
    let width = (ui.available_width() - INDENT).max(40.0);
    ui.horizontal(|ui| {
        ui.add_space(INDENT);
        ui.allocate_ui_with_layout(
            egui::vec2(width, 0.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.set_max_width(width);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text).size(9.5).color(t::TEXT_FAINT).italics(),
                    )
                    .wrap(),
                );
            },
        );
    });
}

// ChannelRowOut + GenCtx live in `layer_ui` (stack + inspector).
