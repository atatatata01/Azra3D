//! Channel row + layer rows + badges (panel section split from view.rs).

use crate::ui_menus::{AzraMenus, AzraContextMenu};
use eframe::egui;
use egui_phosphor::regular as ph;
use core_math::pbr_material::MapProjection;
use crate::ui_theme as t;
use super::super::layer_ui::{self, ChannelRowOut, GenCtx};
use super::super::model::*;
use super::channel_menu::{
    kind_renderer_chip, kind_scalar_range, draw_channel_kind_menu, ALL_KINDS,
};
use super::{
    kind_label, generator_label, kind_bit, trim_f, lin_to_c32,
    ROW_H, LAYER_ROW_H,
};

/// Effective runtime path (after user Live/Bake choice + stack capabilities).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChannelRuntime {
    Value,
    /// User chose Live — GPU path when possible (noise / simple graphs).
    Live,
    /// User chose Bake — maps always.
    Bake,
    /// Pure vertex curvature (always GPU vertex buffer; independent of Live/Bake).
    Vertex,
}

/// True when the channel can show Live/Bake and 8/16 controls (has a map slot + content).
pub(super) fn channel_has_eval_controls(ch: &Channel) -> bool {
    ch.kind.map_kind().is_some()
        && (matches!(ch.state, ChannelState::Layered | ChannelState::GraphDriven { .. })
            || !ch.layers.is_empty())
}

pub(super) fn channel_runtime(ch: &Channel, mat: &core_math::primitives::PbrMaterial) -> ChannelRuntime {
    use super::super::lower::channel_is_vertex_rung_only;
    if channel_is_vertex_rung_only(ch) {
        return ChannelRuntime::Vertex;
    }
    if matches!(ch.state, ChannelState::Value(_)) && ch.layers.is_empty() {
        return ChannelRuntime::Value;
    }
    // User choice first (owner: not auto-inferred from stack).
    match ch.eval_mode {
        ChannelEvalMode::Live => {
            // Live still falls back to bake for stacks the GPU live path cannot own
            // (images, Mult stacks, etc.) — but the **choice** stays Live until they switch.
            let _ = mat; // live GPU flag applied at commit time
            ChannelRuntime::Live
        }
        ChannelEvalMode::Bake => ChannelRuntime::Bake,
    }
}

/// One channel row (+ its expanded layer list). P2: Value channels carry LIVE
/// widgets (DragValue / color button); GraphDriven rows show badge + dot only
/// (C1 — never a slider showing a value it does not control); the row context
/// menu offers Remove / Revert.
pub(super) fn channel_row(
    ui: &mut egui::Ui,
    mid: u32,
    ch: &mut Channel,
    open: bool,
    conflict: bool,
    texture_space: TextureSpace,
    gen: GenCtx,
    gpu: Option<&crate::GpuCtx>,
    thumbs: &std::collections::HashMap<u32, egui::TextureId>,
    // Caller-owned layer selection — threaded from `AzraApp.ui.mp_layer_sel`.
    sel: &mut Option<layer_ui::LayerSel>,
    // Kinds already present on this material. Duplicate/retype must never offer one of these:
    // `MaterialPanelDoc::channel()` resolves by `.find()` (FIRST match wins), so a second
    // channel of the same kind would be silently invisible to every lookup AND would fight the
    // first one for the single TexMap slot that kind maps to.
    present_kinds: &[ChannelKind],
    // Live PbrMaterial for eval-path badges (noise_pattern, map slots).
    mat: &core_math::primitives::PbrMaterial,
) -> ChannelRowOut {
    let mut out = ChannelRowOut::default();
    let mut toggled = false;
    let mut edit_in_tree = false;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click());
    let resp = resp.on_hover_text(format!(
        "{}\nRenderer: {}",
        kind_label(ch.kind),
        kind_renderer_chip(ch.kind)
    ));
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect.shrink2(egui::vec2(2.0, 1.0)), 4.0, t::SURFACE_HEADER);
    }
    // Owner #7: drop a library texture onto a channel row → load it into that channel's map slot.
    // Only channels that actually have a map slot accept a drop (Value-only channels have none).
    if ch.kind.map_kind().is_some() {
        let dragging_lib = egui::DragAndDrop::has_payload_of_type::<crate::LibraryDragPayload>(ui.ctx());
        let hovering = ui.rect_contains_pointer(rect);
        if dragging_lib && hovering {
            // Drop-target affordance: accent outline while a library drag hovers this row.
            ui.painter().rect_stroke(
                rect.shrink2(egui::vec2(2.0, 1.0)),
                4.0,
                egui::Stroke::new(1.5, t::ACCENT), egui::StrokeKind::Middle,
            );
            if ui.input(|i| i.pointer.any_released()) {
                if let Some(pl) = egui::DragAndDrop::take_payload::<crate::LibraryDragPayload>(ui.ctx()) {
                    out.dropped_lib_item = Some(pl.item_idx);
                }
            }
        }
        // Owner 2026-08-06: the same gesture from WINDOWS EXPLORER — drag an image file
        // onto a channel row and it loads into that channel. Deliberately mirrors the
        // library branch above (same hover outline, same row hit-test) so the two drop
        // sources feel identical, and routes into the SAME handler downstream rather
        // than opening a second load path.
        //
        // `hovered_files` is populated while the OS drag is over the window;
        // `dropped_files` lands on the single frame of the release.
        if !dragging_lib {
            let os_hovering = hovering
                && ui.input(|i| {
                    i.raw
                        .hovered_files
                        .iter()
                        .any(|f| f.path.as_deref().map(crate::file_dialog::is_map_image_path).unwrap_or(true))
                });
            if os_hovering {
                ui.painter().rect_stroke(
                    rect.shrink2(egui::vec2(2.0, 1.0)),
                    4.0,
                    egui::Stroke::new(1.5, t::ACCENT), egui::StrokeKind::Middle,
                );
            }
            if hovering {
                // FIRST matching file only: a channel holds one image, and silently
                // taking an arbitrary one from a multi-file drop would be a guess.
                let dropped = ui.input(|i| {
                    i.raw
                        .dropped_files
                        .iter()
                        .filter_map(|f| f.path.clone())
                        .find(|p| crate::file_dialog::is_map_image_path(p))
                });
                if let Some(path) = dropped {
                    out.dropped_os_file = Some(path);
                }
            }
        }
    }
    let p = ui.painter();

    // [chevron] — always expandable (empty channels show Add image / noise).
    // Higher contrast than BORDER_SUBTLE so empty-row carets stay readable.
    let has_layers = !ch.layers.is_empty();
    let chev = if open { ph::CARET_DOWN } else { ph::CARET_RIGHT };
    let chev_col = if open {
        t::TEXT
    } else if has_layers {
        t::TEXT_SECONDARY
    } else {
        // Empty but still expandable — secondary, not the near-invisible seam color.
        t::TEXT_DIM
    };
    p.text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        chev,
        egui::FontId::proportional(12.0),
        chev_col,
    );

    // [swatch] — channel colour: value colour / grey scalar / state tint.
    let sw = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 32.0, rect.center().y),
        egui::vec2(12.0, 12.0),
    );
    let sw_col = match ch.state {
        ChannelState::Value(ChannelValue::Color(c)) => lin_to_c32(c),
        ChannelState::Value(ChannelValue::Scalar(s)) => {
            let g = (s.clamp(0.0, 1.0) * 255.0) as u8;
            egui::Color32::from_rgb(g, g, g)
        }
        ChannelState::Layered => t::MESH.gamma_multiply(0.55),
        ChannelState::GraphDriven { .. } => t::ACCENT.gamma_multiply(0.65),
    };
    p.rect_filled(sw, 3.0, sw_col);
    p.rect_stroke(sw, 3.0, egui::Stroke::new(1.0, t::BORDER_SUBTLE), egui::StrokeKind::Middle);

    // [eye] — temporarily mute the channel (owner 2026-08-06). Sits between the
    // swatch and the name, the same place a layer's eye sits, so the gesture reads
    // the same at both depths. NON-DESTRUCTIVE: `Channel::enabled` only gates
    // lowering, so every layer/image/parameter survives a round trip through off.
    let eye_c = egui::pos2(rect.left() + 34.0, rect.center().y);
    let eye_rect = egui::Rect::from_center_size(eye_c, egui::vec2(16.0, ROW_H));
    let eye_resp = ui.interact(
        eye_rect,
        ui.id().with(("mp_eye", ch.kind as u32)),
        egui::Sense::click(),
    );
    let eye_col = if !ch.enabled {
        t::TEXT_FAINT
    } else if eye_resp.hovered() {
        t::TEXT
    } else {
        t::TEXT_DIM
    };
    p.text(
        eye_c,
        egui::Align2::CENTER_CENTER,
        if ch.enabled { ph::EYE } else { ph::EYE_SLASH },
        egui::FontId::proportional(12.0),
        eye_col,
    );
    let _ = eye_resp.clone().on_hover_text(if ch.enabled {
        "Mute this channel (temporary — layers and images are kept)"
    } else {
        "Channel muted — click to restore"
    });
    if eye_resp.clicked() {
        ch.enabled = !ch.enabled;
        out.changed = true;
    }

    // [name] — dimmed while muted so the row reads as inactive at a glance.
    p.text(
        egui::pos2(rect.left() + 44.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        kind_label(ch.kind),
        egui::FontId::proportional(12.0),
        if ch.enabled { t::TEXT } else { t::TEXT_FAINT },
    );

    // ── 8/16 always when the channel has a map slot (Live AND Bake) ─────────
    // Owner: "available in both systems" — no more dash-out on Live.
    let show_bit = ch.kind.map_kind().is_some()
        && !matches!(channel_runtime(ch, mat), ChannelRuntime::Value);
    let bd_x = rect.left() + 44.0 + kind_label(ch.kind).len() as f32 * 6.8 + 10.0;
    let bd_rect = egui::Rect::from_center_size(
        egui::pos2(bd_x + 10.0, rect.center().y),
        egui::vec2(22.0, 14.0),
    );
    let bd_id = ui.id().with(("mp_bitdepth", ch.kind as u32));
    let bd_resp = ui.interact(bd_rect, bd_id, egui::Sense::click());
    if show_bit {
        let bd_label = match ch.bit_depth {
            ChannelBitDepth::Bit8 => "8",
            ChannelBitDepth::Bit16 => "16",
        };
        let bd_col = if ch.bit_depth == ChannelBitDepth::Bit16 {
            t::ACCENT
        } else {
            t::TEXT_FAINT
        };
        ui.painter().rect_filled(bd_rect, 3.0, t::SURFACE_FIELD);
        ui.painter()
            .rect_stroke(bd_rect, 3.0, egui::Stroke::new(1.0, bd_col.gamma_multiply(0.6)), egui::StrokeKind::Middle);
        ui.painter().text(
            bd_rect.center(),
            egui::Align2::CENTER_CENTER,
            bd_label,
            egui::FontId::proportional(9.5),
            bd_col,
        );
        let mode = ch.eval_mode.label();
        let bd_tip = format!(
            "Map precision (8 or 16-bit) — available in Live and Bake.\n\
             Current mode: {mode}.\n\
             • Bake: re-encodes dual-plane maps now (you should see banding change).\n\
             • Live: stores intent; applies when maps install / you switch to Bake.\n\
             Soft gradients show the difference; flat 8-bit source PNGs often do not."
        );
        if bd_resp.on_hover_text(bd_tip).clicked() {
            ch.bit_depth = ch.bit_depth.toggle();
            out.changed = true;
            out.bit_depth_toggled = true;
        }
    } else {
        // Value-only (no map slot) — no precision control.
        ui.painter().rect_filled(bd_rect, 3.0, t::SURFACE_PANEL.gamma_multiply(0.35));
        ui.painter().text(
            bd_rect.center(),
            egui::Align2::CENTER_CENTER,
            "—",
            egui::FontId::proportional(9.5),
            t::TEXT_FAINT,
        );
        let _ = bd_resp.on_hover_text("No map on this channel — 8/16 does not apply.");
    }

    // Right edge: [connection dot]
    let dot_c = egui::pos2(rect.right() - 10.0, rect.center().y);
    let graph_driven = matches!(ch.state, ChannelState::GraphDriven { .. });
    if graph_driven {
        p.circle_filled(dot_c, 3.5, t::ACCENT);
    } else {
        p.circle_stroke(dot_c, 3.5, egui::Stroke::new(1.0, t::BORDER_HOVER));
    }
    let dot_rect = egui::Rect::from_center_size(dot_c, egui::vec2(16.0, ROW_H));
    let dot_resp = ui.interact(
        dot_rect,
        ui.id().with(("mp_dot", ch.kind as u32)),
        egui::Sense::click(),
    );
    // P8: BOTH dots are routes into the shader tree (spec deliverable #4). The
    // filled (GraphDriven) dot frames the node feeding the master socket; the
    // empty (panel-driven / plain-value) dot frames that channel's panel-generated
    // subgraph head (or the master socket when it is a plain value). One entry
    // point (`mp_frame_shader_node`) resolves which — the row only reports intent.
    let dr = if graph_driven {
        dot_resp.on_hover_text("Graph-driven — click to frame this node in the shader tree")
    } else {
        dot_resp.on_hover_text("Click to open this channel in the shader tree")
    };
    if dr.clicked() {
        edit_in_tree = true;
    }

    // [badge | LIVE value widget] left of the dot.
    let right_x = rect.right() - 22.0;
    if conflict {
        // P0-4: bake-ownership conflict. This badge used to be painter-only —
        // no hover, no click, and the only recovery ("Revert to panel control")
        // was gated on GraphDriven, a state a conflicted channel is NOT in. It
        // is now a real widget offering the two honest resolutions, each wired
        // to an EXISTING mechanism (drop the panel stack, or clear the flag).
        let cr = badge_interactive(
            ui,
            right_x - 64.0,
            rect.center().y,
            "conflict",
            egui::Color32::from_rgb(235, 160, 70),
            ("mp_conflict", ch.kind as u32),
        )
        .on_hover_text(
            "The shader tree baked this channel, but the panel owns it (you hand-loaded a map here), \
so the bake was skipped and the graph's result is not showing.\n\nClick to resolve.",
        );
        if cr.clicked() {
            ui.memory_mut(|m| m.open_popup(ui.id().with(("mp_conflict_pop", ch.kind as u32))));
        }
        let pop_id = ui.id().with(("mp_conflict_pop", ch.kind as u32));
        egui::containers::old_popup::popup_below_widget(
            ui,
            pop_id,
            &cr,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(230.0);
                ui.label(
                    egui::RichText::new("Who drives this channel?")
                        .size(10.5)
                        .color(t::TEXT_SECONDARY),
                );
                ui.add_space(2.0);
                if ui
                    .button("Let the shader tree own it")
                    .on_hover_text(
                        "Drop this channel's panel layers. The graph's bake installs into the map slot from now on.",
                    )
                    .clicked()
                {
                    out.conflict_yield_graph = true;
                }
                if ui
                    .button("Keep my map")
                    .on_hover_text(
                        "Keep the hand-loaded map. The graph's bake for this channel stays ignored.",
                    )
                    .clicked()
                {
                    out.conflict_keep_panel = true;
                }
            },
        );
    }
    // P0-3: a Layered channel that does NOT lower produced NOTHING and said
    // nothing — same `layers` badge as a working channel. Surface the real
    // reason from the SAME decision function the lowering uses.
    if let Some(reason) = super::super::lower::channel_stub_reason(ch, texture_space) {
        badge_interactive(
            ui,
            right_x - if conflict { 128.0 } else { 64.0 },
            rect.center().y,
            reason.badge_text(),
            egui::Color32::from_rgb(228, 120, 100),
            ("mp_stub", ch.kind as u32),
        )
        .on_hover_text(reason.explain());
    }
    let map_only = matches!(ch.kind, ChannelKind::Bump | ChannelKind::Displacement);
    let runtime = channel_runtime(ch, mat);
    // ── Live | Bake CHOICE (user, not auto-inferred) ───────────────────────
    // Owner: "bake vs live should be a choice" — no permanent BAKE badge.
    if channel_has_eval_controls(ch) && !matches!(runtime, ChannelRuntime::Vertex) {
        let mode_x = right_x - 4.0;
        let mode_rect = egui::Rect::from_center_size(
            egui::pos2(mode_x - 28.0, rect.center().y),
            egui::vec2(56.0, 16.0),
        );
        let mode_id = ui.id().with(("mp_eval_mode", ch.kind as u32));
        let mode_resp = ui.interact(mode_rect, mode_id, egui::Sense::click());
        let live_on = ch.eval_mode == ChannelEvalMode::Live;
        let mode_col = if live_on {
            egui::Color32::from_rgb(120, 200, 140)
        } else {
            t::MESH
        };
        ui.painter().rect_filled(mode_rect, 4.0, mode_col.gamma_multiply(0.16));
        ui.painter().rect_stroke(
            mode_rect,
            4.0,
            egui::Stroke::new(1.0, mode_col.gamma_multiply(0.55)), egui::StrokeKind::Middle,
        );
        ui.painter().text(
            mode_rect.center(),
            egui::Align2::CENTER_CENTER,
            ch.eval_mode.label().to_uppercase(),
            egui::FontId::proportional(9.5),
            mode_col,
        );
        if mode_resp
            .on_hover_text(
                "Click to switch Live ↔ Bake (your choice, not automatic).\n\
                 • Live — GPU path when possible (noise); 8/16 still stored for maps.\n\
                 • Bake — layer stack → texture maps at Bake N² + 8/16 precision.\n\
                 Vertex curvature stays on the GPU vertex buffer either way.",
            )
            .clicked()
        {
            ch.eval_mode = ch.eval_mode.toggle();
            out.changed = true;
            out.eval_mode_changed = true;
        }
    } else if matches!(runtime, ChannelRuntime::Vertex) {
        let _ = badge_interactive(
            ui,
            right_x,
            rect.center().y,
            "VERTEX",
            egui::Color32::from_rgb(140, 180, 255),
            ("mp_path", ch.kind as u32),
        )
        .on_hover_text(
            "Per-vertex curvature (GPU kernel). 8/16 applies if you also bake maps; \
             pure vertex content uses the curvature buffer.",
        );
    }
    match &mut ch.state {
        ChannelState::GraphDriven { .. } => {
            // Live/Bake control above; no slider (C1).
        }
        ChannelState::Layered => {}
        ChannelState::Value(_) if map_only => {
            // Bump/Displacement have no scalar field (map-only, model.rs table).
        }
        ChannelState::Value(ChannelValue::Scalar(s)) => {
            // LIVE Slider — a visible track + value, matching the mockup's
            // Metalness/Roughness rows (owner #1). Fixed short width so the row
            // stays ~ROW_H; still transactional via mp_commit (out.changed/drag).
            let (_speed, range) = kind_scalar_range(ch.kind);
            let sl_rect = egui::Rect::from_min_max(
                egui::pos2(right_x - 118.0, rect.top() + 3.0),
                egui::pos2(right_x, rect.bottom() - 3.0),
            );
            let r = ui.put(
                sl_rect,
                egui::Slider::new(s, range)
                    .fixed_decimals(2)
                    .show_value(true)
                    .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.5 }),
            );
            out.changed |= r.changed();
            out.drag_started |= r.drag_started();
        }
        ChannelState::Value(ChannelValue::Color(c)) => {
            // LIVE color button (linear → srgb widget → linear).
            let vr = egui::Rect::from_center_size(
                egui::pos2(right_x - 14.0, rect.center().y),
                egui::vec2(28.0, 16.0),
            );
            // Custom fixed-width color popup (no wobble). Linear↔srgb via the
            // shared helpers (identical convention to the v1 editor).
            let r = ui
                .allocate_new_ui(egui::UiBuilder::new().max_rect(vr), |ui| {
                    crate::ui_fields::color_button_linear_vec3(ui, c)
                })
                .inner;
            if r.changed() {
                out.changed = true;
                out.drag_started = true; // picker scrubs are drags → one undo
            }
        }
    }

    // Context menu: Remove (confirm gated by caller) / Revert for GraphDriven /
    // Add image layer (P3 — converts a Value channel to Layered).
    resp.clone().azra_context_menu(|ui| {
        // P8: "Edit in shader tree" — always available; frames THIS channel's node
        // (subgraph head for driven/panel channels, master socket for plain values).
        if ui.button("Edit in shader tree").clicked() {
            edit_in_tree = true;
            ui.close_menu();
        }
        // P0-4: reachable whenever the graph actually has a claim on this
        // channel — GraphDriven (it drives the socket) OR conflict (it tried to
        // bake and was gated out). Previously GraphDriven-only, which is exactly
        // the state a conflicted channel is NOT in, leaving no recovery path.
        if (matches!(ch.state, ChannelState::GraphDriven { .. }) || conflict)
            && ui.button("Revert to panel control").clicked()
        {
            out.revert = true;
            ui.close_menu();
        }
        if ch.kind.map_kind().is_some()
            && !matches!(ch.state, ChannelState::GraphDriven { .. })
            && ui.button("Add image layer").clicked()
        {
            ch.state = ChannelState::Layered;
            ch.layers.push(new_image_layer());
            out.load_layer = Some(ch.layers.len() - 1);
            out.changed = true;
            ui.close_menu();
        }
        if ch.kind.map_kind().is_some()
            && !matches!(ch.state, ChannelState::GraphDriven { .. })
            && ui.button("Add noise layer").clicked()
        {
            ch.state = ChannelState::Layered;
            ch.layers.push(new_noise_layer());
            out.changed = true;
            ui.close_menu();
        }
        // DUPLICATE / RETYPE (owner 2026-07-28). Both offer only kinds that
        //   (a) have a TexMap slot — `map_kind().is_none()` kinds cannot receive a baked stack,
        //       so retargeting there would silently make the work inert, and
        //   (b) are not already present — see `present_kinds`.
        // If nothing qualifies the submenu is not shown at all, rather than opening an empty
        // menu or listing entries that would quietly do nothing.
        let targets: Vec<ChannelKind> = ALL_KINDS
            .iter()
            .copied()
            .filter(|k| {
                *k != ch.kind && k.map_kind().is_some() && !present_kinds.contains(k)
            })
            .collect();
        if !targets.is_empty() && ch.kind.map_kind().is_some() {
            ui.azra_menu_button("Duplicate to…", |ui| {
                if let Some(k) = draw_channel_kind_menu(ui, &targets) {
                    out.duplicate_to = Some(k);
                    ui.close_menu();
                }
            })
            .response
            .on_hover_text(
                "Copy this channel's layer stack into another channel\n\
                 (grouped by renderer support)",
            );
            ui.azra_menu_button("Change type to…", |ui| {
                if let Some(k) = draw_channel_kind_menu(ui, &targets) {
                    out.retype_to = Some(k);
                    ui.close_menu();
                }
            })
            .response
            .on_hover_text(
                "Move this channel's layer stack to another channel kind\n\
                 (grouped by renderer support)",
            );
        }
        if ui.button("Remove channel").clicked() {
            out.remove = true;
            ui.close_menu();
        }
    });

    // Row / chevron click toggles disclosure — always (empty channels expand to Add…).
    // Ignore clicks that landed on the right-side slider / color / dot.
    if resp.clicked() && !edit_in_tree {
        toggled = true;
    }

    // Expanded — depth 2 (channel → layer). Empty channels still expand so the
    // user can add an image or noise layer without a context menu.
    if open {
        if matches!(ch.state, ChannelState::Layered) {
            layer_ui::draw_layer_stack(ui, mid, ch, texture_space, gen, gpu, thumbs, sel, present_kinds, &mut out);
        } else if !ch.layers.is_empty() {
            for (li, layer) in ch.layers.iter().enumerate() {
                layer_rows(ui, layer, li, texture_space, gpu);
            }
        } else if matches!(ch.state, ChannelState::GraphDriven { .. }) {
            // A GraphDriven channel with no panel layers used to fall through EVERY
            // arm and render an empty expansion — so the channels that actually have
            // content (a library drop makes BaseColor/Roughness/Metallic/Normal
            // GraphDriven) showed nothing at all, while the unused Value channels
            // showed sliders. Owner 2026-08-06: "select the normal channel, I don't
            // see the channel property."
            //
            // C1 still stands — no slider here, because the panel does NOT own this
            // value. What was missing is a way to SEE that and to act on it. Both
            // actions already existed on the right-click menu; this only surfaces
            // them where the user actually looks.
            graph_driven_channel_body(ui, &mut out);
        } else {
            // Empty Value / map-only channel: offer add actions inline.
            empty_channel_body(ui, ch, gen, &mut out);
        }
    }
    out.toggled = toggled;
    out.edit_in_tree = edit_in_tree;
    out
}

/// Empty expanded channel: Add image / noise / constant / generator (when allowed).
/// Expanded body for a channel the NODE GRAPH drives.
///
/// The panel deliberately shows no value widgets for these (C1: never a slider
/// showing a value it does not control), but before 2026-08-06 it showed nothing
/// whatsoever — a dead expansion on exactly the channels that carry real content.
/// This states who owns the channel and offers the two existing escape hatches.
pub(super) fn graph_driven_channel_body(ui: &mut egui::Ui, out: &mut ChannelRowOut) {
    let indent = 30.0;
    ui.horizontal(|ui| {
        ui.add_space(indent);
        ui.label(
            egui::RichText::new("Driven by the shader graph")
                .size(10.5)
                .italics()
                .color(t::TEXT_FAINT),
        );
    });
    ui.horizontal(|ui| {
        ui.add_space(indent);
        if ui
            .add(egui::Button::new(egui::RichText::new("Edit in shader tree").size(10.5)))
            .on_hover_text("Frame this channel's nodes in the shader graph editor.")
            .clicked()
        {
            out.edit_in_tree = true;
        }
        if ui
            .add(egui::Button::new(egui::RichText::new("Revert to panel control").size(10.5)))
            .on_hover_text(
                "Hand this channel back to the panel so it can be edited here.

                 DESTRUCTIVE: prunes the channel's subgraph, so the graph nodes                  currently feeding it are removed.",
            )
            .clicked()
        {
            out.revert = true;
        }
    });
}

pub(super) fn empty_channel_body(
    ui: &mut egui::Ui,
    ch: &mut Channel,
    gen: GenCtx,
    out: &mut ChannelRowOut,
) {
    let indent = 30.0;
    ui.horizontal(|ui| {
        ui.add_space(indent);
        if ch.kind.map_kind().is_some()
            && ui
                .add(egui::Button::new(
                    egui::RichText::new(format!("{} Add image layer", ph::PLUS)).size(10.5),
                ))
                .clicked()
        {
            ch.state = ChannelState::Layered;
            ch.layers.push(new_image_layer());
            // One-click: open the file browser immediately.
            out.load_layer = Some(ch.layers.len() - 1);
            out.changed = true;
        }
        if ch.kind.map_kind().is_some()
            && ui
                .add(egui::Button::new(
                    egui::RichText::new(format!("{} Add noise layer", ph::PLUS)).size(10.5),
                ))
                .on_hover_text("Procedural noise (FBM) — lowers to a Noise node in the shader graph")
                .clicked()
        {
            ch.state = ChannelState::Layered;
            ch.layers.push(new_noise_layer());
            out.changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.add_space(indent);
        if ui
            .add(egui::Button::new(
                egui::RichText::new(format!("{} Add constant", ph::PLUS)).size(10.5),
            ))
            .clicked()
        {
            ch.state = ChannelState::Layered;
            ch.layers.push(new_constant_layer(ch.kind));
            out.changed = true;
        }
        let gr = ui
            .add_enabled(
                (gen.has_sdf || gen.has_poly_mesh) && ch.kind.map_kind().is_some(),
                egui::Button::new(
                    egui::RichText::new(format!("{} Add generator", ph::PLUS)).size(10.5),
                ),
            )
            .on_disabled_hover_text(
                "Needs an SDF body (field) or a polygonal mesh (measured curvature)",
            );
        if gr.clicked() {
            ch.state = ChannelState::Layered;
            ch.layers.push(new_generator_layer(gen.radius_floor, gen.has_uvs));
            out.changed = true;
        }
    });
}

/// Fresh empty image layer (tex_id 0 → shows a Load… button).
pub(super) fn new_image_layer() -> Layer {
    Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Image { tex_id: 0, strength: 1.0 },
        mask: MaskStack::default(),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
        // Per-layer tiling (own TextureSpace; starts at 1×1 triplanar).
        placement: Placement {
            space: Some(TextureSpace::default()),
        },
        adjust: Adjust::default(),
    }
}

/// Fresh procedural noise layer (FBM defaults).
pub(super) fn new_noise_layer() -> Layer {
    Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Noise {
            scale: 4.0,
            strength: 1.0,
            octaves: 4.0,
            noise_type: crate::shader_nodes::NoiseType::Fbm,
        },
        mask: MaskStack::default(),
        // Mult is the usual "dirt over map" mode; Normal would fully replace the layer below.
        blend: BlendMode::Multiply,
        opacity: 1.0,
        visible: true,
        placement: Placement {
            space: Some(TextureSpace::default()),
        },
        adjust: Adjust::default(),
    }
}

/// Fresh generator layer (P6): Curvature, signed output, radius seeded at the
/// body's C4 floor (never below it — the floor is also surfaced as a caption).
///
/// GPU_CURVATURE_KERNEL_PLAN.md P3 rung auto-selection (owner's rule, verbatim: "If the mesh has
/// no UVs, use VERTEX; if the mesh has UVs, use UV"). `has_uvs` is the honest per-body signal
/// (`GenCtx::has_uvs`, threaded from `poly_custom_data`) — NOT a scene-wide existential. The user
/// can always override via the rung combo (`GeneratorRung` round-trips through the panel doc).
pub(super) fn new_generator_layer(radius_floor: Option<f32>, has_uvs: bool) -> Layer {
    // Keep in lockstep with layer_ui::new_generator_layer (P0: Multiply always —
    // Normal clobbers the stack in gbuffer; see MATERIAL_FEATURE_COMPLETE_PLAN).
    Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Generator {
            source: GeneratorSource::Curvature,
            radius: radius_floor.unwrap_or(0.02).max(0.02),
            invert: false,
            output: GeneratorOutput::Signed,
            remap: [0.0, 1.0],
            rung: if has_uvs { GeneratorRung::Auto } else { GeneratorRung::VertexCache },
            sharpness: 1.0,
            axis: DirectionAxis::default(),
        },
        mask: MaskStack::default(),
        blend: BlendMode::Multiply,
        opacity: 1.0,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    }
}

/// Fresh constant layer: color channels get a color, scalar channels a scalar.
pub(super) fn new_constant_layer(kind: ChannelKind) -> Layer {
    let value = match kind {
        ChannelKind::BaseColor
        | ChannelKind::Emission
        | ChannelKind::SssColor
        | ChannelKind::GlassTint => ChannelValue::Color([1.0, 1.0, 1.0]),
        _ => ChannelValue::Scalar(1.0),
    };
    Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Constant { value },
        mask: MaskStack::default(),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
        placement: Placement::default(),
        adjust: Adjust::default(),
    }
}

// layer stack UI: see layer_ui::draw_layer_stack

/// Small rounded badge, right-aligned at `right_x`. Returns the badge RECT so
/// callers that need interaction (conflict / stub — P0-3, P0-4) can allocate a
/// widget over it; purely decorative callers ignore the return.
pub(super) fn badge(ui: &mut egui::Ui, right_x: f32, cy: f32, text: &str, col: egui::Color32) -> egui::Rect {
    let p = ui.painter();
    let galley = p.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(9.5),
        col,
    );
    let pad = egui::vec2(6.0, 3.0);
    let r = egui::Rect::from_min_size(
        egui::pos2(right_x - galley.size().x - pad.x * 2.0, cy - galley.size().y / 2.0 - pad.y),
        galley.size() + pad * 2.0,
    );
    p.rect_filled(r, 4.0, col.gamma_multiply(0.16));
    p.rect_stroke(r, 4.0, egui::Stroke::new(1.0, col.gamma_multiply(0.45)), egui::StrokeKind::Middle);
    p.galley(r.min + pad, galley, col);
    r
}

/// An INTERACTIVE badge (P0-3 / P0-4): paints the same chip, then allocates a
/// real widget over it so it can carry hover text and clicks. Without this a
/// badge is painter-only — the exact defect behind the dead conflict badge.
pub(super) fn badge_interactive(
    ui: &mut egui::Ui,
    right_x: f32,
    cy: f32,
    text: &str,
    col: egui::Color32,
    id_salt: impl std::hash::Hash,
) -> egui::Response {
    let r = badge(ui, right_x, cy, text, col);
    let resp = ui.interact(r, ui.id().with(id_salt), egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter()
            .rect_stroke(r, 4.0, egui::Stroke::new(1.0, col), egui::StrokeKind::Middle);
    }
    resp
}

/// Read-only rows for one layer inside an expanded channel (flat — no third
/// disclosure level; image-adjust is a summary LINE, per spec §2).
pub(super) fn layer_rows(
    ui: &mut egui::Ui,
    layer: &Layer,
    li: usize,
    texture_space: TextureSpace,
    gpu: Option<&crate::GpuCtx>,
) {
    let indent = 44.0;
    let mut row = |icon: &str, main: String, sub: Option<String>, dim: bool| {
        let w = ui.available_width();
        let h = if sub.is_some() { LAYER_ROW_H + 12.0 } else { LAYER_ROW_H };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        let p = ui.painter();
        let main_col = if dim { t::TEXT_DIM } else { t::TEXT_SECONDARY };
        p.text(
            egui::pos2(rect.left() + indent - 14.0, rect.top() + LAYER_ROW_H / 2.0),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(11.0),
            t::TEXT_FAINT,
        );
        p.text(
            egui::pos2(rect.left() + indent, rect.top() + LAYER_ROW_H / 2.0),
            egui::Align2::LEFT_CENTER,
            main,
            egui::FontId::proportional(11.5),
            main_col,
        );
        if let Some(s) = sub {
            p.text(
                egui::pos2(rect.left() + indent, rect.top() + LAYER_ROW_H + 2.0),
                egui::Align2::LEFT_TOP,
                s,
                egui::FontId::proportional(9.5),
                t::TEXT_FAINT,
            );
        }
    };
    let _ = li;
    match &layer.content {
        LayerContent::Image { tex_id, strength } => {
            let fname = gpu
                .and_then(|g| g.material_textures.slot_name(*tex_id))
                .filter(|n| !n.is_empty())
                .map(|n| n.to_string())
                .unwrap_or_else(|| format!("Image #{tex_id}"));
            // Effective placement caption: "UV map 1 · 2×2 · inherited" (owner §8).
            let (space, tag) = match layer.placement.space {
                Some(s) => (s, "override"),
                None => (texture_space, "inherited"),
            };
            let proj = match MapProjection::from_u32(space.projection) {
                MapProjection::Uv => "UV map 1".to_string(),
                MapProjection::Box => "Triplanar".to_string(),
                other => other.label().to_string(),
            };
            row(
                ph::IMAGE,
                fname,
                Some(format!(
                    "{proj} · {}×{} · {tag}",
                    trim_f(space.tiling[0]),
                    trim_f(space.tiling[1])
                )),
                false,
            );
            row(
                ph::SLIDERS_HORIZONTAL,
                format!(
                    "Image adjust · strength {strength:.2} · opacity {:.2} · {:?}",
                    layer.opacity, layer.blend
                ),
                None,
                true,
            );
        }
        LayerContent::Noise {
            scale,
            strength,
            octaves,
            noise_type,
        } => {
            row(
                ph::WAVES,
                format!(
                    "Noise · {} · scale {scale:.1} · {octaves:.0} oct",
                    noise_type.short()
                ),
                Some(format!("strength {strength:.2} · opacity {:.2}", layer.opacity)),
                false,
            );
        }
        LayerContent::Generator {
            source,
            radius,
            invert,
            output,
            remap,
            rung,
            sharpness,
            axis,
        } => {
            // Generators: NO projection/tiling caption (spec §2 — mesh-derived).
            // P7: the rung is part of the collapsed caption (spec §6.5 — always
            // visible whether the value is live or baked).
            let rung_s = match rung {
                GeneratorRung::Auto => " · texture (baked)",
                GeneratorRung::VertexCache => " · vertex",
            };
            let _ = (invert, sharpness);
            let out_s = match (source, output) {
                // Cavity is concave-only and Direction is axis-driven — neither has a
                // curvature output selector, so neither shows one.
                (GeneratorSource::Cavity | GeneratorSource::Direction, _) => "",
                (_, GeneratorOutput::Signed) => " · signed",
                (_, GeneratorOutput::ConvexOnly) => " · convex",
                (_, GeneratorOutput::ConcaveOnly) => " · concave",
            };
            let axis_s = if *source == GeneratorSource::Direction {
                format!(" · {}", axis.label())
            } else {
                String::new()
            };
            let remap_s = if *remap != [0.0, 1.0] {
                format!(" · remap {:.2}–{:.2}", remap[0], remap[1])
            } else {
                String::new()
            };
            row(
                ph::WAVEFORM,
                format!(
                    "{} · radius {radius:.3}{axis_s}{out_s}{remap_s}{}{rung_s}",
                    generator_label(*source),
                    if *invert { " · inverted" } else { "" }
                ),
                None,
                false,
            );
        }
        LayerContent::Constant { value } => {
            let v = match value {
                ChannelValue::Scalar(s) => format!("{s:.2}"),
                ChannelValue::Color(c) => format!("{:.2} {:.2} {:.2}", c[0], c[1], c[2]),
            };
            row(ph::CIRCLE_HALF, format!("Constant · {v}"), None, false);
        }
    }
    if let Some(mask) = layer.mask.primary_content() {
        let m = match mask {
            LayerContent::Generator { source, radius, .. } => {
                format!("Mask · {} · radius {radius:.3}", generator_label(*source))
            }
            LayerContent::Image { tex_id, .. } => format!("Mask · image #{tex_id}"),
            LayerContent::Constant { .. } => "Mask · constant".to_string(),
            LayerContent::Noise {
                scale,
                noise_type,
                ..
            } => format!("Mask · {} noise s={scale:.1}", noise_type.short()),
        };
        row(ph::CIRCLE_DASHED, m, None, true);
    }
}
