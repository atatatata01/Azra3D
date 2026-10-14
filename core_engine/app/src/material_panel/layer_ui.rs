//! Compact layer stack + **docked** settings pane (Substance-style).
//!
//! The layer **tree stays clean** (only stack rows). Settings are a **sibling
//! panel section outside the tree** — drawn by [`draw_layer_inspector`] below
//! the CHANNELS list (not nested under Base color / any channel).

use crate::ui_menus::{AzraMenus, AzraContextMenu};
use eframe::egui;
use egui_phosphor::regular as ph;
use core_math::pbr_material::MapProjection;
use crate::ui_theme as t;

use super::lower::generator_source_label;
use super::model::*;

/// Active-body hints for generator UI (same signals as the panel GenCtx).
#[derive(Clone, Copy, Default)]
pub(crate) struct GenCtx {
    pub has_sdf: bool,
    /// The active body is POLYGONAL — curvature is available by measuring its half-edge mesh
    /// (`POLY_CURVATURE_PLAN.md`), even though there is no analytical field.
    ///
    /// Gates **Curvature and Cavity only**. AO, Thickness, Direction and Position stay on
    /// `has_sdf`: each of those needs either ray casting against the mesh or a field gradient, and
    /// neither exists here — enabling them would put back exactly the "plausible-looking mask with
    /// no relationship to the geometry" this work removed.
    pub has_poly_mesh: bool,
    pub radius_floor: Option<f32>,
    /// Size-derived DEFAULT radius for this body (see `curvature_auto_radius_aabb`).
    /// `None` when there is no body to measure — the Auto button hides rather than guessing.
    pub auto_radius: Option<f32>,
    pub has_uvs: bool,
}

/// A graph socket the panel can use as a `ParamDrive` source (float outputs).
#[derive(Clone, Debug)]
pub(crate) struct DriveCandidate {
    pub node: u32,
    pub output: u32,
    pub label: String,
}

/// Collect float-output sockets from a material graph for the Const|Driven picker.
/// Excludes `MaterialOutput` (terminal, not a driver). Order is stable by node id.
pub(crate) fn collect_drive_candidates(
    graph: &crate::shader_nodes::NodeGraph,
) -> Vec<DriveCandidate> {
    use crate::shader_nodes::nodes as sn;
    use crate::shader_nodes::types::SocketType;
    use crate::shader_nodes::NodeType;
    let mut out = Vec::new();
    let mut nodes: Vec<_> = graph.nodes.iter().collect();
    nodes.sort_by_key(|n| n.id);
    for n in nodes {
        if matches!(n.node_type, NodeType::MaterialOutput) {
            continue;
        }
        let sockets = sn::outputs(&n.node_type);
        for (oi, sock) in sockets.iter().enumerate() {
            if !matches!(sock.socket_type, SocketType::Float) {
                continue;
            }
            let type_lbl = sn::label(&n.node_type);
            let label = if !n.label.is_empty() && !n.label.starts_with("mp:") {
                n.label.clone()
            } else if n.label.starts_with("mp:") {
                // Panel-owned node: short type + id so the user can still pick a noise layer.
                format!("{type_lbl} · n{}", n.id)
            } else {
                format!("{type_lbl} · n{}", n.id)
            };
            out.push(DriveCandidate {
                node: n.id,
                output: oi as u32,
                label,
            });
            // One float output per node is enough for the picker (radius/scale are single floats).
            break;
        }
    }
    out
}

/// Per-frame edit signals from one channel's layer stack.
#[derive(Default)]
pub(crate) struct ChannelRowOut {
    pub toggled: bool,
    pub edit_in_tree: bool,
    pub changed: bool,
    pub drag_started: bool,
    pub remove: bool,
    pub revert: bool,
    pub load_layer: Option<usize>,
    pub load_mask: Option<usize>,
    pub dropped_lib_item: Option<usize>,
    /// An image file dragged from the OS file explorer onto this channel row.
    /// Handled by the SAME downstream path as `dropped_lib_item` — the only
    /// difference is that the path is already absolute, so no library lookup runs.
    pub dropped_os_file: Option<std::path::PathBuf>,
    pub conflict_yield_graph: bool,
    pub conflict_keep_panel: bool,
    /// Copy this channel's whole layer stack into another (empty) channel kind.
    pub duplicate_to: Option<ChannelKind>,
    /// Retarget this channel to another kind, moving its stack (no copy left behind).
    pub retype_to: Option<ChannelKind>,
    /// Copy ONE layer into another channel: `(layer index, destination kind)`.
    pub copy_layer_to: Option<(usize, ChannelKind)>,
    /// User toggled 8/16-bit — apply `TexMap.high_precision` + re-bake maps.
    pub bit_depth_toggled: bool,
    /// User changed map bake target resolution (`doc.bake_res` / `graph.bake_res`).
    pub bake_res_changed: bool,
    /// User switched Live ↔ Bake eval mode.
    pub eval_mode_changed: bool,
}

/// Material-scoped selection: `(channel_kind as u32, layer_index)`.
pub(crate) type LayerSel = (u32, usize);

/// Draw **only** the compact layer list inside a channel (tree stays clean).
/// Selection is material-scoped so the docked inspector outside the tree can read it.
pub(crate) fn draw_layer_stack(
    ui: &mut egui::Ui,
    mid: u32,
    ch: &mut Channel,
    texture_space: TextureSpace,
    gen: GenCtx,
    gpu: Option<&crate::GpuCtx>,
    thumbs: &std::collections::HashMap<u32, egui::TextureId>,
    // Caller-owned selection (`AzraApp.ui.mp_layer_sel`). See its doc comment: egui's temp
    // store never round-tripped this value, so the panel owns it directly.
    sel: &mut Option<LayerSel>,
    // Channels that exist on this material — destinations for "Duplicate to channel".
    present_kinds: &[ChannelKind],
    out: &mut ChannelRowOut,
) {
    let indent = 30.0;
    let n = ch.layers.len();
    let kind_u = ch.kind as u32;
    let centers_id = egui::Id::new(("mp_lyr_centers", mid, kind_u));
    let last_centers: Vec<f32> = ui.data(|d| d.get_temp(centers_id)).unwrap_or_default();
    let mut centers: Vec<f32> = Vec::with_capacity(n);
    let mut reorder: Option<(usize, usize)> = None;
    let mut remove: Option<usize> = None;
    let mut mat_sel: Option<LayerSel> = *sel;
    // Distinguishes a REAL user click in this channel from the auto-select default below.
    // A click must be able to STEAL selection from another channel; an auto-select must not.
    let mut user_picked = false;
    // Drop stale indices; auto-select top visual layer so properties always have a target.
    if mat_sel.is_some_and(|(k, li)| k != kind_u || li >= n) {
        // Keep selection if it points at another channel; only clear if this channel had the bad index.
        if mat_sel.is_some_and(|(k, _)| k == kind_u) {
            mat_sel = None;
        }
    }
    if mat_sel.is_none() && n > 0 {
        mat_sel = Some((kind_u, n - 1)); // top of stack
    }
    let selected: Option<usize> = mat_sel.and_then(|(k, li)| {
        if k == kind_u && li < n {
            Some(li)
        } else {
            None
        }
    });

    // ── Compact stack only (no settings inside the tree) ────────────────────
    for row in 0..n {
        let li = n - 1 - row;
        let block_top = ui.cursor().top();
        let layer = &mut ch.layers[li];
        if let LayerContent::Generator { source, .. } = layer.content {
            if !super::lower::generator_source_supported(source) {
                ui.horizontal(|ui| {
                    ui.add_space(indent);
                    ui.label(
                        egui::RichText::new(format!("(unsupported) {}", generator_source_label(source)))
                            .size(10.0)
                            .color(t::TEXT_FAINT),
                    );
                });
                centers.push(block_top);
                continue;
            }
        }

        let is_sel = selected == Some(li);
        let mut pick = false;
        let row_id = ui.id().with(("mp_lyr_row", mid, kind_u, li));

        // Full-width clickable row (Sense::click on the allocate), then paint controls inside.
        let row_h = 24.0;
        let full_w = ui.available_width().max(80.0);
        let (row_rect, row_sense) = ui.allocate_exact_size(egui::vec2(full_w, row_h), egui::Sense::click());
        let row_resp = ui.interact(row_rect, row_id, egui::Sense::click());
        if is_sel {
            ui.painter().rect_filled(
                row_rect,
                3.0,
                t::SELECTED_ROW.gamma_multiply(0.85),
            );
            ui.painter().rect_stroke(
                row_rect,
                3.0,
                egui::Stroke::new(1.0, t::ACCENT.gamma_multiply(0.75)), egui::StrokeKind::Middle,
            );
        } else if row_resp.hovered() {
            ui.painter().rect_filled(row_rect, 3.0, t::SURFACE_CONTROL.gamma_multiply(0.45));
        }

        ui.allocate_ui_at_rect(row_rect, |ui| {
            ui.set_min_size(row_rect.size());
            ui.horizontal_centered(|ui| {
                ui.add_space(indent.min(row_rect.width() * 0.3));
                // Drag handle
                let hr = ui.add(
                    egui::Label::new(
                        egui::RichText::new(ph::DOTS_SIX_VERTICAL).size(12.0).color(t::TEXT_DIM),
                    )
                    .sense(egui::Sense::click_and_drag()),
                );
                hr.clone().azra_context_menu(|ui| {
                    // COPY ONE LAYER TO ANOTHER CHANNEL (owner 2026-07-28: "right click on
                    // layer to duplicate in one selected channel, so if I need a curvature in
                    // metalness I can duplicate it from base color").
                    //
                    // Only channels that EXIST are offered — this copies a layer INTO a stack,
                    // it does not create channels. Kinds without a map slot are excluded for the
                    // same reason as channel duplicate/retype: the copied layer could never bake
                    // there, so offering it would be a silent no-op.
                    let dests: Vec<ChannelKind> = present_kinds
                        .iter()
                        .copied()
                        .filter(|k| *k != ch.kind && k.map_kind().is_some())
                        .collect();
                    if !dests.is_empty() {
                        ui.azra_menu_button("Duplicate to channel", |ui| {
                            for k in &dests {
                                if ui.button(channel_kind_short(*k)).clicked() {
                                    out.copy_layer_to = Some((li, *k));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    if ui.button("Remove layer").clicked() {
                        remove = Some(li);
                        ui.close_menu();
                    }
                });
                if hr.drag_stopped() {
                    if let Some(pos) = ui.ctx().pointer_interact_pos() {
                        if !last_centers.is_empty() {
                            let mut best = 0usize;
                            let mut bd = f32::MAX;
                            for (i, cy) in last_centers.iter().enumerate() {
                                let d = (cy - pos.y).abs();
                                if d < bd {
                                    bd = d;
                                    best = i;
                                }
                            }
                            let target = n.saturating_sub(1).saturating_sub(best.min(n - 1));
                            if target != li {
                                reorder = Some((li, target));
                            }
                        }
                    }
                } else if hr.dragged() {
                    ui.painter().rect_stroke(
                        hr.rect.expand(2.0),
                        3.0,
                        egui::Stroke::new(1.0, t::ACCENT), egui::StrokeKind::Middle,
                    );
                }
                hr.on_hover_text("Drag to reorder · right-click to remove");

                let eye = if layer.visible { ph::EYE } else { ph::EYE_SLASH };
                if ui
                    .add(egui::Button::new(egui::RichText::new(eye).size(12.0)).frame(false))
                    .on_hover_text("Visibility")
                    .clicked()
                {
                    layer.visible = !layer.visible;
                    out.changed = true;
                    pick = true; // still select this layer
                }

                match &layer.content {
                    LayerContent::Image { tex_id, .. } => {
                        if let Some(tid) = thumbs.get(tex_id) {
                            ui.image(egui::load::SizedTexture::new(*tid, egui::vec2(18.0, 18.0)));
                        }
                    }
                    LayerContent::Constant { value } => {
                        if let ChannelValue::Color(c) = value {
                            let col = egui::Color32::from_rgb(
                                (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                                (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                                (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                            );
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, col);
                        }
                    }
                    _ => {}
                }

                let name = layer_display_name(layer, gpu);
                let name_col = if is_sel { t::ACCENT } else { t::TEXT_SECONDARY };
                // Real button — Labels with Sense::click were unreliable under parent ScrollAreas.
                let name_r = ui.add(
                    egui::Button::new(egui::RichText::new(name).size(11.0).color(name_col).strong())
                        .frame(false)
                        .min_size(egui::vec2(64.0, 18.0)),
                );
                if name_r.clicked() {
                    pick = true;
                }
                if !layer.mask.is_empty() {
                    ui.label(
                        egui::RichText::new(ph::CIRCLE_DASHED)
                            .size(10.0)
                            .color(t::TEXT_FAINT),
                    )
                    .on_hover_text("Has mask");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(4.0);
                    let blabel = layer.blend.short();
                    egui::ComboBox::from_id_salt(("mp_blend", mid, kind_u, li))
                        .selected_text(egui::RichText::new(blabel).size(10.0))
                        .width(54.0)
                        .show_ui(ui, |ui| {
                            for &m in BlendMode::all() {
                                if ui.selectable_label(layer.blend == m, m.label()).clicked()
                                    && layer.blend != m
                                {
                                    layer.blend = m;
                                    out.changed = true;
                                    pick = true;
                                }
                            }
                        });
                    let r = ui
                        .add(
                            egui::DragValue::new(&mut layer.opacity)
                                .speed(0.01)
                                .range(0.0..=1.0)
                                .fixed_decimals(2),
                        )
                        .on_hover_text("Opacity / intensity");
                    if r.changed() || r.drag_started() {
                        pick = true;
                    }
                    out.changed |= r.changed();
                    out.drag_started |= r.drag_started();
                });
            });
        });

        // Click anywhere on the row background selects (including empty padding).
        if row_sense.clicked() || row_resp.clicked() || pick {
            mat_sel = Some((kind_u, li));
            user_picked = true;
        }

        centers.push((block_top + ui.cursor().top()) * 0.5);
    }

    // ── Add layer ───────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.add_space(indent);
        ui.azra_menu_button(
            egui::RichText::new(format!("{} Add layer", ph::PLUS)).size(10.0),
            |ui| {
                ui.set_min_width(120.0);
                if ui.button("Image").clicked() {
                    // MUST promote Value → Layered. Leaving Value makes lower_to_material
                    // clear_map every commit while layers still exist — roughness/metal
                    // Mult noise installs then vanishes (owner: "noise in roughness does nothing").
                    ch.state = ChannelState::Layered;
                    ch.layers.push(new_image_layer_own_tiling(TextureSpace::default()));
                    let li = ch.layers.len() - 1;
                    mat_sel = Some((kind_u, li));
                    user_picked = true;
                    // One-click: open the file browser immediately (no second "Load…").
                    out.load_layer = Some(li);
                    out.changed = true;
                    ui.close_menu();
                }
                if ui.button("Noise").on_hover_text("Procedural noise").clicked() {
                    ch.state = ChannelState::Layered;
                    ch.layers.push(new_noise_layer_own_tiling(TextureSpace::default()));
                    let li = ch.layers.len() - 1;
                    mat_sel = Some((kind_u, li));
                    user_picked = true;
                    out.changed = true;
                    ui.close_menu();
                }
                // Adds a CURVATURE generator layer (`new_generator_layer`), so it follows the
                // Curvature gate, not the SDF-only one.
                let gr = ui
                    .add_enabled(
                        gen.has_sdf || gen.has_poly_mesh,
                        egui::Button::new("Generator"),
                    )
                    .on_disabled_hover_text("Needs an SDF body or a polygonal mesh");
                if gr.clicked() {
                    ch.state = ChannelState::Layered;
                    ch.layers.push(new_generator_layer(gen.radius_floor, gen.has_uvs));
                    let li = ch.layers.len() - 1;
                    mat_sel = Some((kind_u, li));
                    out.changed = true;
                    ui.close_menu();
                }
                if ui.button("Constant / Fill").clicked() {
                    ch.state = ChannelState::Layered;
                    ch.layers.push(new_constant_layer(ch.kind));
                    let li = ch.layers.len() - 1;
                    mat_sel = Some((kind_u, li));
                    out.changed = true;
                    ui.close_menu();
                }
            },
        );
    });

    // Collect-then-apply
    if let Some((from, to)) = reorder {
        if from < ch.layers.len() && to < ch.layers.len() && from != to {
            let l = ch.layers.remove(from);
            ch.layers.insert(to, l);
            if mat_sel == Some((kind_u, from)) {
                mat_sel = Some((kind_u, to));
            } else if let Some((k, s)) = mat_sel {
                if k == kind_u {
                    mat_sel = Some((kind_u, remap_index_after_move(s, from, to)));
                }
            }
            out.changed = true;
        }
    }
    if let Some(li) = remove {
        if li < ch.layers.len() {
            ch.layers.remove(li);
            if mat_sel == Some((kind_u, li)) {
                mat_sel = None;
            } else if let Some((k, s)) = mat_sel {
                if k == kind_u && s > li {
                    mat_sel = Some((kind_u, s - 1));
                }
            }
            out.changed = true;
        }
    }

    ui.data_mut(|d| {
        d.insert_temp(centers_id, centers);
    });
    {
        // SELECTION WRITE-BACK IS CONDITIONAL (fix 2026-07-28: "I select a layer but LAYER
        // PROPERTIES still says 'Select a layer in the tree above'").
        //
        // `sel_id` is MATERIAL-scoped but this function runs ONCE PER CHANNEL. The write used to
        // be unconditional, so every channel that drew its stack after the clicked one stomped
        // the shared key with its OWN auto-selection (the `mat_sel.is_none() && n > 0` default
        // just above). The inspector then read whatever the LAST channel happened to write —
        // usually a different channel's top layer, or nothing at all.
        //
        // Only publish when this channel legitimately owns the selection:
        //   • it points at THIS channel (the user clicked here, or we auto-selected for a
        //     channel that has layers and nothing else had claimed it), and
        //   • no other channel has already published a selection this frame.
        // Otherwise leave the stored value alone — a channel must never clear or hijack a
        // sibling's selection.
        // A real click always wins (it may legitimately steal from another channel); an
        // AUTO-select only publishes when no other channel has claimed the key.
        // A real click always wins (it may legitimately steal from another channel); an
        // AUTO-select only publishes when no other channel has claimed the selection.
        let owns = mat_sel.is_some_and(|(k, _)| k == kind_u);
        let other_channel_owns = sel.is_some_and(|(k, _)| k != kind_u);
        if owns && (user_picked || !other_channel_owns) {
            *sel = mat_sel;
        }
    }
}

/// Docked inspector **outside** the channel tree (sibling of CHANNELS, not inside Base color).
/// Call after the full channel list is drawn.
pub(crate) fn draw_layer_inspector(
    ui: &mut egui::Ui,
    #[allow(unused_variables)] mid: u32,
    doc: &mut MaterialPanelDoc,
    gen: GenCtx,
    gpu: Option<&crate::GpuCtx>,
    thumbs: &std::collections::HashMap<u32, egui::TextureId>,
    // Float-output graph nodes eligible as `ParamDrive` sources (from the material `NodeGraph`).
    drive_sources: &[DriveCandidate],
    // Caller-owned selection (`AzraApp.ui.mp_layer_sel`) — see `draw_layer_stack`.
    sel: Option<LayerSel>,
    out: &mut ChannelRowOut,
) {
    let mat_sel: Option<LayerSel> = sel;

    ui.add_space(8.0);
    // Splitter-like separator (Substance: tree above, properties below).
    let full_w = ui.available_width();
    let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(full_w, 6.0), egui::Sense::hover());
    ui.painter().hline(
        sep_rect.x_range(),
        sep_rect.center().y,
        egui::Stroke::new(1.0, t::BORDER),
    );

    ui.label(
        egui::RichText::new("LAYER PROPERTIES")
            .size(9.5)
            .color(t::TEXT_SECTION)
            .strong(),
    );
    ui.add_space(4.0);

    let Some((kind_u, li)) = mat_sel else {
        ui.label(
            egui::RichText::new("Select a layer in the tree above")
                .size(10.0)
                .color(t::TEXT_FAINT)
                .italics(),
        );
        return;
    };
    let Some(ch) = doc.channels.iter_mut().find(|c| c.kind as u32 == kind_u) else {
        return;
    };
    if li >= ch.layers.len() {
        return;
    }
    let kind_u = ch.kind as u32;
    let texture_space = doc.texture_space;
    let layer = &mut ch.layers[li];

    egui::Frame::none()
        .fill(t::SURFACE_HEADER)
        .stroke(egui::Stroke::new(1.0, t::BORDER_SUBTLE))
        .rounding(3.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(layer_display_name(layer, gpu))
                        .size(12.0)
                        .color(t::TEXT)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(format!("· {}", channel_kind_short(ch.kind)))
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
            });
            ui.add_space(6.0);
            draw_layer_content_settings(ui, layer, li, kind_u, gen, gpu, thumbs, drive_sources, out);
            if matches!(
                layer.content,
                LayerContent::Image { .. } | LayerContent::Noise { .. }
            ) {
                ui.add_space(6.0);
                ui.separator();
                ui.label(
                    egui::RichText::new("Placement · tiling")
                        .size(10.0)
                        .color(t::TEXT_DIM),
                );
                draw_placement_settings(ui, layer, texture_space, out);
            }
            // ADJUST IS SHOWN FOR GENERATORS TOO (fix 2026-07-28: "the curvature softness is
            // still sharp, this is the type of map I need" + a reference showing smooth midtone
            // gradients across a whole shell).
            //
            // The section used to be hidden for `Generator` layers — but `Adjust` ALREADY
            // contains the control that produces that look, and the lowering ALREADY emits it for
            // any layer with a non-default adjust (a `ColorCorrect` node; gamma → socket 4,
            // round-tripped). Only the UI was withholding it.
            //
            // Why this is the fix and Soft/Hard is not: the curvature shaping is
            // `smoothstep(clamp(x * contrast * 4, 0, 1))`, so contrast scales the INPUT before a
            // hard clamp. It can only trade "saturates at x≥0.17 → everything white" (the
            // reported hard bands) for "nothing saturates → everything crushed dark". Measured:
            // at contrast 0.15 the entire curvature range collapses into 0.00–0.47. There is NO
            // contrast value that yields midtones end to end. GAMMA reshapes the OUTPUT after
            // normalization, which is exactly the continuous ramp the reference image shows.
            ui.add_space(6.0);
            ui.separator();
            ui.label(egui::RichText::new("Adjust").size(10.0).color(t::TEXT_DIM));
            draw_adjust_settings(ui, layer, out, ch.kind);
            // BLEND + OPACITY (owner 2026-07-28: "layer opacity should appear in the setting
            // section"). Both already existed but ONLY as the cramped controls in the tree row,
            // where the row is 24px tall and space is shared with the name, eye and drag handle.
            // The settings pane is where the user actually tunes them, and the reference layout
            // pairs them on one line: `Blend [mode]   Opacity [====o] 1.00`.
            // Same fields the row edits — no duplicate state, so the two stay in sync.
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Blend").size(10.0).color(t::TEXT_DIM));
                // Same variant list and labels as the tree row (`mp_blend`) so the two controls
                // cannot drift apart; only the id salt and width differ.
                let blabel = layer.blend.label();
                egui::ComboBox::from_id_salt(("mp_insp_blend", kind_u, li))
                    .selected_text(egui::RichText::new(blabel).size(10.0))
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for &m in BlendMode::all() {
                            if ui.selectable_label(layer.blend == m, m.label()).clicked()
                                && layer.blend != m
                            {
                                layer.blend = m;
                                out.changed = true;
                            }
                        }
                    });
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Opacity").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::Slider::new(&mut layer.opacity, 0.0..=1.0)
                        .fixed_decimals(2)
                        .show_value(true),
                );
                if r.drag_started() {
                    out.drag_started = true;
                }
                if r.changed() {
                    out.changed = true;
                }
            });

            ui.add_space(6.0);
            ui.separator();
            draw_mask_section(ui, layer, li, kind_u, gen, thumbs, out);
        });
}

fn channel_kind_short(k: ChannelKind) -> &'static str {
    // ONE label source: the canonical panel table (view::kind_label). The old local
    // 5-arm match fell back to a blank "Channel" header for the 13 other kinds
    // (SSS color, IOR, sheen…) — a dup that had already drifted.
    super::view::kind_label(k)
}

fn remap_index_after_move(s: usize, from: usize, to: usize) -> usize {
    if s == from {
        to
    } else if from < to {
        if s > from && s <= to {
            s - 1
        } else {
            s
        }
    } else if s >= to && s < from {
        s + 1
    } else {
        s
    }
}

fn layer_display_name(layer: &Layer, gpu: Option<&crate::GpuCtx>) -> String {
    match &layer.content {
        LayerContent::Image { tex_id, .. } => gpu
            .and_then(|g| g.material_textures.slot_name(*tex_id))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if *tex_id == 0 {
                    "Image".into()
                } else {
                    format!("Image #{tex_id}")
                }
            }),
        LayerContent::Noise { noise_type, .. } => format!("Noise · {}", noise_type.short()),
        LayerContent::Constant { .. } => "Fill".into(),
        LayerContent::Generator { source, .. } => generator_source_label(*source).to_string(),
    }
}

fn new_image_layer_own_tiling(material_ts: TextureSpace) -> Layer {
    Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Image {
            tex_id: 0,
            strength: 1.0,
        },
        mask: MaskStack::default(),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
        // Each layer owns tiling (copy material defaults as starting point).
        placement: Placement {
            space: Some(material_ts),
        },
        adjust: Adjust::default(),
    }
}

fn new_noise_layer_own_tiling(material_ts: TextureSpace) -> Layer {
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
        blend: BlendMode::Multiply,
        opacity: 1.0,
        visible: true,
        placement: Placement {
            space: Some(material_ts),
        },
        adjust: Adjust::default(),
    }
}

pub(crate) fn new_generator_layer(radius_floor: Option<f32>, has_uvs: bool) -> Layer {
    // Always Multiply (MATERIAL_FEATURE_COMPLETE_PLAN P0 + owner 2026-07-29).
    // Normal lowers to gbuffer fall-through `blended = top` (no blend==0u over arm),
    // which REPLACES everything beneath — "add Mult noise over curvature" silently
    // drew only the curvature. Multiply alone over a light base still reads as
    // curvature; under/over another layer it composites instead of clobbering.
    // Rung still follows UVs: no UVs → VertexCache; UVs → Auto.
    let vertex = !has_uvs;
    Layer {
        id: 0,
        drives: Vec::new(),
        content: LayerContent::Generator {
            source: GeneratorSource::Curvature,
            radius: radius_floor.unwrap_or(0.02).max(0.02),
            invert: false,
            output: GeneratorOutput::Signed,
            remap: [0.0, 1.0],
            rung: if vertex {
                GeneratorRung::VertexCache
            } else {
                GeneratorRung::Auto
            },
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

fn new_constant_layer(kind: ChannelKind) -> Layer {
    let value = match kind {
        ChannelKind::BaseColor | ChannelKind::Emission | ChannelKind::SssColor | ChannelKind::GlassTint => {
            ChannelValue::Color([0.66, 0.66, 0.66])
        }
        _ => ChannelValue::Scalar(0.5),
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

fn draw_layer_content_settings(
    ui: &mut egui::Ui,
    layer: &mut Layer,
    li: usize,
    kind_u: u32,
    gen: GenCtx,
    gpu: Option<&crate::GpuCtx>,
    thumbs: &std::collections::HashMap<u32, egui::TextureId>,
    drive_sources: &[DriveCandidate],
    out: &mut ChannelRowOut,
) {
    // Snapshot drives before content borrow (P3 Const|Driven UI).
    let radius_driven = layer.drives.iter().any(|d| d.param == "radius");
    let scale_driven = layer.drives.iter().any(|d| d.param == "scale");
    let drive_label = |param: &str| -> String {
        layer
            .drive(param)
            .and_then(|d| {
                drive_sources
                    .iter()
                    .find(|c| c.node == d.node && c.output == d.output)
                    .map(|c| c.label.clone())
            })
            .unwrap_or_else(|| "graph node".into())
    };
    let radius_drive_lbl = if radius_driven {
        drive_label("radius")
    } else {
        String::new()
    };
    let scale_drive_lbl = if scale_driven {
        drive_label("scale")
    } else {
        String::new()
    };
    let mut clear_drive: Option<&'static str> = None;
    let mut set_drive: Option<(&'static str, u32, u32)> = None; // param, node, output
    match &mut layer.content {
        LayerContent::Image { tex_id, strength } => {
            ui.horizontal(|ui| {
                if let Some(tid) = thumbs.get(tex_id) {
                    // 72px preview (owner 2026-07-28: "for the image setting use a larger
                    // thumbnails"). The 36px it replaced is the TREE ROW size — correct there,
                    // where rows are 24px tall and the thumbnail is an identifier, but this is
                    // the docked settings pane where the image is the subject and the user is
                    // judging the actual texture. `ui.image` scales the same SizedTexture, so
                    // this costs nothing: no new upload, no second thumbnail cache entry.
                    const INSPECTOR_THUMB: f32 = 72.0;
                    ui.image(egui::load::SizedTexture::new(
                        *tid,
                        egui::vec2(INSPECTOR_THUMB, INSPECTOR_THUMB),
                    ));
                }
                ui.vertical(|ui| {
                    let fname = gpu
                        .and_then(|g| g.material_textures.slot_name(*tex_id))
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            if *tex_id == 0 {
                                "no image".into()
                            } else {
                                format!("slot {tex_id}")
                            }
                        });
                    ui.label(egui::RichText::new(fname).size(11.0).color(t::TEXT_SECONDARY));
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new(egui::RichText::new("Load…").size(10.0)))
                            .clicked()
                        {
                            out.load_layer = Some(li);
                        }
                        ui.label(egui::RichText::new("Strength").size(10.0).color(t::TEXT_DIM));
                        let r = ui.add(
                            egui::DragValue::new(strength)
                                .speed(0.01)
                                .range(0.0..=1.0)
                                .fixed_decimals(2),
                        );
                        out.changed |= r.changed();
                        out.drag_started |= r.drag_started();
                    });
                });
            });
        }
        LayerContent::Constant { value } => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Fill").size(10.0).color(t::TEXT_DIM));
                match value {
                    ChannelValue::Scalar(s) => {
                        let r = ui.add(
                            egui::DragValue::new(s)
                                .speed(0.005)
                                .range(0.0..=1.0)
                                .fixed_decimals(2),
                        );
                        out.changed |= r.changed();
                        out.drag_started |= r.drag_started();
                    }
                    ChannelValue::Color(c) => {
                        if crate::ui_fields::color_button_linear_vec3(ui, c).changed() {
                            out.changed = true;
                            out.drag_started = true;
                        }
                    }
                }
            });
        }
        LayerContent::Noise {
            scale,
            strength,
            octaves,
            noise_type,
        } => {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("mp_nse_ty", kind_u, li))
                    .selected_text(egui::RichText::new(noise_type.label()).size(10.5))
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for nt in crate::shader_nodes::NoiseType::all() {
                            if ui.selectable_label(*noise_type == *nt, nt.label()).clicked() {
                                *noise_type = *nt;
                                out.changed = true;
                            }
                        }
                    });
                ui.label(egui::RichText::new("Scale").size(10.0).color(t::TEXT_DIM));
                if scale_driven {
                    ui.colored_label(t::ACCENT, egui::RichText::new("Driven").size(10.0))
                        .on_hover_text(format!("Scale driven by {scale_drive_lbl}"));
                    if ui.small_button("×").on_hover_text("Clear scale drive").clicked() {
                        clear_drive = Some("scale");
                        out.changed = true;
                    }
                } else {
                    let r = ui.add(
                        egui::DragValue::new(scale)
                            .speed(0.05)
                            .range(0.01..=64.0)
                            .fixed_decimals(2),
                    );
                    out.changed |= r.changed();
                    out.drag_started |= r.drag_started();
                    if let Some((n, o)) =
                        draw_drive_picker(ui, ("mp_drv_scale", kind_u, li), drive_sources)
                    {
                        set_drive = Some(("scale", n, o));
                        out.changed = true;
                    }
                }
                ui.label(egui::RichText::new("Oct").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::DragValue::new(octaves)
                        .speed(0.25)
                        .range(1.0..=12.0)
                        .fixed_decimals(0),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
                ui.label(egui::RichText::new("Str").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::DragValue::new(strength)
                        .speed(0.01)
                        .range(0.0..=1.0)
                        .fixed_decimals(2),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
            });
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
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("mp_gen_src", kind_u, li))
                    .selected_text(egui::RichText::new(generator_source_label(*source)).size(10.5))
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        for s in [
                            GeneratorSource::Curvature,
                            GeneratorSource::Cavity,
                            GeneratorSource::Direction,
                            GeneratorSource::AmbientOcclusion,
                            GeneratorSource::Thickness,
                            GeneratorSource::Position,
                        ] {
                            // Per-source, not per-panel: on a poly body only the two sources that
                            // are a LOCAL DIFFERENTIAL of the surface can be measured from the
                            // mesh. The rest need ray casting or a field and stay disabled.
                            let ok = gen.has_sdf
                                || (gen.has_poly_mesh
                                    && matches!(
                                        s,
                                        GeneratorSource::Curvature | GeneratorSource::Cavity
                                    ));
                            if ui
                                .add_enabled(
                                    ok,
                                    egui::SelectableLabel::new(*source == s, generator_source_label(s)),
                                )
                                .on_disabled_hover_text(
                                    "SDF bodies only — needs ray casting or a distance field, \
                                     which a polygonal mesh does not provide",
                                )
                                .clicked()
                                && *source != s
                            {
                                *source = s;
                                out.changed = true;
                            }
                        }
                    });
                let r_floor = gen.radius_floor.unwrap_or(0.02).max(1e-4);
                if *radius < r_floor {
                    *radius = r_floor;
                    out.changed = true;
                }
                ui.label(egui::RichText::new("Radius").size(10.0).color(t::TEXT_DIM));
                if radius_driven {
                    ui.colored_label(t::ACCENT, egui::RichText::new("Driven").size(10.0))
                        .on_hover_text(format!(
                            "Radius driven by {radius_drive_lbl}. Clear to edit as Const."
                        ));
                    if ui
                        .small_button(egui::RichText::new("×").size(10.0))
                        .on_hover_text("Clear radius drive — restore Const slider")
                        .clicked()
                    {
                        clear_drive = Some("radius");
                        out.changed = true;
                    }
                } else {
                    let r = ui.add(
                        egui::DragValue::new(radius)
                            .speed(0.002)
                            .range(r_floor..=10.0)
                            .fixed_decimals(3),
                    );
                    out.changed |= r.changed();
                    out.drag_started |= r.drag_started();
                    if let Some((n, o)) =
                        draw_drive_picker(ui, ("mp_drv_radius", kind_u, li), drive_sources)
                    {
                        set_drive = Some(("radius", n, o));
                        out.changed = true;
                    }
                }
                // AUTO — set Radius from the body's own size.
                //
                // The floor (1% of the short axis) is a NOISE clamp, not a good default: on a box
                // it marks only pixels within 1% of an edge, which reads as "curvature shows
                // nothing" even though the pipeline is working. Auto uses 8%, which is visible at
                // a normal viewing distance and still well short of neighbouring edge bands
                // merging into a flat wash. Hidden when there is no body to measure — an Auto that
                // guesses is worse than no Auto.
                if let Some(auto_r) = gen.auto_radius {
                    let already = (*radius - auto_r).abs() < 1e-6;
                    if ui
                        .add_enabled(!already, egui::Button::new(
                            egui::RichText::new("Auto").size(10.0)).small())
                        .on_hover_text(format!(
                            "Set Radius from this object's size ({auto_r:.3}).\n\
                             Scales with the body, so the same setting reads the same on a small \
                             bolt and a large wall."))
                        .clicked()
                    {
                        *radius = auto_r;
                        out.changed = true;
                    }
                }
                // Direction: axis picker replaces the curvature output selector.
                if *source == GeneratorSource::Direction {
                    egui::ComboBox::from_id_salt(("mp_gen_axis", kind_u, li))
                        .selected_text(egui::RichText::new(axis.label()).size(10.0))
                        .width(84.0)
                        .show_ui(ui, |ui| {
                            for a in DirectionAxis::ALL {
                                if ui
                                    .selectable_label(*axis == a, a.label())
                                    .clicked()
                                    && *axis != a
                                {
                                    *axis = a;
                                    out.changed = true;
                                }
                            }
                        });
                }
                if *source == GeneratorSource::Curvature {
                    for (o, s, tip) in [
                        (GeneratorOutput::Signed, "±", "Signed"),
                        (GeneratorOutput::ConvexOnly, "Cx", "Convex / edges"),
                        (GeneratorOutput::ConcaveOnly, "Cc", "Concave"),
                    ] {
                        if ui
                            .add(egui::SelectableLabel::new(*output == o, egui::RichText::new(s).size(10.0)))
                            .on_hover_text(tip)
                            .clicked()
                            && *output != o
                        {
                            *output = o;
                            out.changed = true;
                        }
                    }
                }
                if ui.checkbox(invert, egui::RichText::new("Inv").size(10.0)).changed() {
                    out.changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Remap").size(10.0).color(t::TEXT_DIM));
                for i in 0..2 {
                    let r = ui.add(
                        egui::DragValue::new(&mut remap[i])
                            .speed(0.005)
                            .range(-2.0..=2.0)
                            .fixed_decimals(2),
                    );
                    out.changed |= r.changed();
                    out.drag_started |= r.drag_started();
                }
                ui.label(egui::RichText::new("Soft/Hard").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::DragValue::new(sharpness)
                        .speed(0.01)
                        .range(0.001..=64.0)
                        .fixed_decimals(3)
                        .clamp_existing_to_range(false),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
                let rlabel = match rung {
                    GeneratorRung::Auto => "Texture",
                    GeneratorRung::VertexCache => "Vertex",
                };
                // Direction has NO vertex rung — the vertex path is the curvature kernel, not a
                // generic per-vertex evaluator (see `lower::emit_generator_layer`). Show the rung
                // as a fixed caption rather than a combo that offers a choice which silently
                // does nothing.
                if *source == GeneratorSource::Direction {
                    ui.label(
                        egui::RichText::new("Texture")
                            .size(10.0)
                            .color(t::TEXT_DIM),
                    )
                    .on_hover_text("Direction bakes to a texture map (no vertex rung)");
                } else {
                    // BOTH MODES: sampling domain, not compositor. Bake still owns the map.
                    // G6: when the body has no UVs, still show Texture as a *disabled* entry so
                    // the combo is not a single dead "Vertex" that looks broken when toggled.
                    egui::ComboBox::from_id_salt(("mp_gen_rung", kind_u, li))
                        .selected_text(egui::RichText::new(rlabel).size(10.0))
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            if gen.has_uvs {
                                if ui
                                    .selectable_label(*rung == GeneratorRung::Auto, "Texture")
                                    .on_hover_text(
                                        "Planar / UV bake domain (SEED_PLANAR).\n\
                                         Best with real UVs. Composited into the map.",
                                    )
                                    .clicked()
                                {
                                    *rung = GeneratorRung::Auto;
                                    out.changed = true;
                                }
                            } else {
                                ui.add_enabled(
                                    false,
                                    egui::SelectableLabel::new(false, "Texture"),
                                )
                                .on_hover_text(
                                    "Needs UVs — this body has none (SDF/DC).\n\
                                     Vertex surface sampling is used instead.",
                                );
                            }
                            if ui
                                .selectable_label(*rung == GeneratorRung::VertexCache, "Vertex")
                                .on_hover_text(
                                    "Surface-position samples (SEED_POSITIONS / volume seeds).\n\
                                     Best without UVs (SDF/DC). Still lands in the baked map.",
                                )
                                .clicked()
                            {
                                *rung = GeneratorRung::VertexCache;
                                out.changed = true;
                            }
                        })
                        .response
                        .on_hover_text(
                            "Sampling domain — not a second compositor.\n\
                             Texture = planar bake · Vertex = surface-position samples.\n\
                             Both install into the map so Path Trace matches Hybrid.",
                        );
                }
            });
            // "No UVs → Vertex" — but NEVER for Direction/AO, which have no vertex kernel and would
            // become an invisible no-op layer. Keep them pinned to Auto instead (the same rule
            // `lower::normalize_one_generator` enforces on load).
            if matches!(
                *source,
                GeneratorSource::Direction
                    | GeneratorSource::AmbientOcclusion
                    | GeneratorSource::Thickness
                    | GeneratorSource::Position
            ) {
                if *rung != GeneratorRung::Auto {
                    *rung = GeneratorRung::Auto;
                    out.changed = true;
                }
            } else if !gen.has_uvs && *rung != GeneratorRung::VertexCache {
                *rung = GeneratorRung::VertexCache;
                out.changed = true;
            }
        }
        _ => {}
    }
    if let Some(param) = clear_drive {
        layer.drives.retain(|d| d.param != param);
    }
    if let Some((param, node, output)) = set_drive {
        layer.drives.retain(|d| d.param != param);
        layer.drives.push(ParamDrive {
            param: param.to_string(),
            node,
            output,
        });
    }
}

/// Small "Drive…" menu: pick a float-output graph node to own this parameter.
fn draw_drive_picker(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    drive_sources: &[DriveCandidate],
) -> Option<(u32, u32)> {
    let mut picked = None;
    // Push a unique id so Scale and Radius Drive menus on the same layer don't collide.
    ui.push_id(id_salt, |ui| {
        ui.azra_menu_button(egui::RichText::new("Drive…").size(10.0), |ui| {
            ui.set_min_width(160.0);
            if drive_sources.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "No float nodes in the shader tree.\nAdd Value or Noise, then drive.",
                    )
                    .size(10.0)
                    .color(t::TEXT_FAINT),
                );
                return;
            }
            for c in drive_sources {
                if ui
                    .button(egui::RichText::new(&c.label).size(10.5))
                    .on_hover_text(format!(
                        "Connect node {} output {} → this param",
                        c.node, c.output
                    ))
                    .clicked()
                {
                    picked = Some((c.node, c.output));
                    ui.close_menu();
                }
            }
        })
        .response
        .on_hover_text("Drive this parameter from a graph node (Const → Driven)");
    });
    picked
}

fn draw_placement_settings(
    ui: &mut egui::Ui,
    layer: &mut Layer,
    material_ts: TextureSpace,
    out: &mut ChannelRowOut,
) {
    // Ensure layer owns a space (each layer has its own tiling).
    if layer.placement.space.is_none() {
        layer.placement.space = Some(material_ts);
        out.changed = true;
    }
    let Some(space) = layer.placement.space.as_mut() else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Projection").size(10.0).color(t::TEXT_DIM));
        for (label, code) in [("Box (triplanar)", 0u32), ("UV", 1u32)] {
            let on = space.projection == code;
            if ui
                .add(egui::SelectableLabel::new(on, egui::RichText::new(label).size(10.0)))
                .clicked()
                && !on
            {
                space.projection = code;
                out.changed = true;
            }
        }
    });
    if space.projection == 0 {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Blend").size(10.0).color(t::TEXT_DIM));
            let r = ui.add(
                egui::DragValue::new(&mut space.blend)
                    .speed(0.05)
                    .range(0.25..=7.75)
                    .fixed_decimals(2),
            );
            if r.changed() {
                out.changed = true;
                out.drag_started |= r.drag_started();
            }
            r.on_hover_text("Smoother plane seams (low) ↔ harder edges (high)");
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Rotate 90°").size(10.0).color(t::TEXT_DIM));
            for (label, get_set) in [
                ("X", 0u8),
                ("Y", 1u8),
                ("Z", 2u8),
            ] {
                let q = match get_set {
                    0 => space.rot_x & 3,
                    1 => space.rot_y & 3,
                    _ => space.rot_z & 3,
                };
                let text = format!("{label} {}°", q as u32 * 90);
                if ui
                    .add(egui::Button::new(egui::RichText::new(text).size(10.0)))
                    .on_hover_text("Click to rotate this projection plane by 90°")
                    .clicked()
                {
                    match get_set {
                        0 => space.rot_x = (space.rot_x + 1) & 3,
                        1 => space.rot_y = (space.rot_y + 1) & 3,
                        _ => space.rot_z = (space.rot_z + 1) & 3,
                    }
                    out.changed = true;
                }
            }
        });
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Tiling").size(10.0).color(t::TEXT_DIM));
        ui.label(egui::RichText::new("U").size(9.5).color(t::TEXT_FAINT));
        let r0 = ui.add(
            egui::DragValue::new(&mut space.tiling[0])
                .speed(0.02)
                .range(0.05..=32.0)
                .fixed_decimals(2),
        );
        ui.label(egui::RichText::new("V").size(9.5).color(t::TEXT_FAINT));
        let r1 = ui.add(
            egui::DragValue::new(&mut space.tiling[1])
                .speed(0.02)
                .range(0.05..=32.0)
                .fixed_decimals(2),
        );
        // Link U=V (common for triplanar)
        if ui
            .add(egui::SelectableLabel::new(
                (space.tiling[0] - space.tiling[1]).abs() < 1e-4,
                egui::RichText::new(ph::LINK).size(11.0),
            ))
            .on_hover_text("Link U = V")
            .clicked()
        {
            space.tiling[1] = space.tiling[0];
            out.changed = true;
        }
        if r0.changed() {
            if (space.tiling[0] - space.tiling[1]).abs() < 1e-3 {
                // keep linked feel when nearly equal
            }
            out.changed = true;
            out.drag_started |= r0.drag_started();
        }
        if r1.changed() {
            out.changed = true;
            out.drag_started |= r1.drag_started();
        }
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Offset").size(10.0).color(t::TEXT_DIM));
        ui.label(egui::RichText::new("U").size(9.5).color(t::TEXT_FAINT));
        let r0 = ui.add(
            egui::DragValue::new(&mut space.offset[0])
                .speed(0.01)
                .range(-8.0..=8.0)
                .fixed_decimals(2),
        );
        ui.label(egui::RichText::new("V").size(9.5).color(t::TEXT_FAINT));
        let r1 = ui.add(
            egui::DragValue::new(&mut space.offset[1])
                .speed(0.01)
                .range(-8.0..=8.0)
                .fixed_decimals(2),
        );
        out.changed |= r0.changed() | r1.changed();
        out.drag_started |= r0.drag_started() | r1.drag_started();
    });
    let _ = MapProjection::Box; // keep import useful if we expand
}

fn draw_adjust_settings(
    ui: &mut egui::Ui,
    layer: &mut Layer,
    out: &mut ChannelRowOut,
    kind: crate::material_panel::model::ChannelKind,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Contrast").size(10.0).color(t::TEXT_DIM));
        let r = ui.add(
            egui::DragValue::new(&mut layer.adjust.contrast)
                .speed(0.01)
                .range(-1.0..=1.0)
                .fixed_decimals(2),
        );
        out.changed |= r.changed();
        out.drag_started |= r.drag_started();
        ui.label(egui::RichText::new("Sat").size(10.0).color(t::TEXT_DIM));
        let r = ui.add(
            egui::DragValue::new(&mut layer.adjust.saturation)
                .speed(0.01)
                .range(0.0..=2.0)
                .fixed_decimals(2),
        );
        out.changed |= r.changed();
        out.drag_started |= r.drag_started();
        ui.label(egui::RichText::new("Gamma").size(10.0).color(t::TEXT_DIM));
        let r = ui.add(
            egui::DragValue::new(&mut layer.adjust.gamma)
                .speed(0.01)
                .range(0.1..=4.0)
                .fixed_decimals(2),
        );
        out.changed |= r.changed();
        out.drag_started |= r.drag_started();
        {
            // Normal maps: Invert = reverse XY slopes (bumps in↔out), not full RGB invert.
            let inv_hint = if matches!(kind, crate::material_panel::model::ChannelKind::Normal) {
                "Flip normal-map detail (convex ↔ concave). Keeps blue/Z outward."
            } else {
                "Invert channel values (1 − x)."
            };
            if ui
                .checkbox(
                    &mut layer.adjust.invert,
                    egui::RichText::new("Invert").size(10.0),
                )
                .on_hover_text(inv_hint)
                .changed()
            {
                out.changed = true;
            }
        }
    });
}

fn draw_mask_section(
    ui: &mut egui::Ui,
    layer: &mut Layer,
    li: usize,
    kind_u: u32,
    gen: GenCtx,
    thumbs: &std::collections::HashMap<u32, egui::TextureId>,
    out: &mut ChannelRowOut,
) {
    let is_gen = matches!(layer.content, LayerContent::Generator { .. });
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Mask")
                .size(10.0)
                .color(t::TEXT_DIM)
                .strong(),
        );
        if !layer.mask.is_empty() {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!("{} Remove", ph::X))
                            .size(10.0)
                            .color(t::TEXT_SECONDARY),
                    )
                    .frame(false),
                )
                .on_hover_text("Remove mask")
                .clicked()
            {
                layer.mask.clear();
                out.changed = true;
            }
        } else if !is_gen {
            ui.azra_menu_button(
                egui::RichText::new(format!("{} Add mask", ph::PLUS))
                    .size(10.0)
                    .color(t::TEXT_DIM),
                |ui| {
                    if ui.button("Image").clicked() {
                        layer.mask.set_single(LayerContent::Image {
                            tex_id: 0,
                            strength: 1.0,
                        });
                        // One-click: open browser for the mask image.
                        out.load_mask = Some(li);
                        out.changed = true;
                        ui.close_menu();
                    }
                    if ui.button("Constant").clicked() {
                        layer.mask.set_single(LayerContent::Constant {
                            value: ChannelValue::Scalar(1.0),
                        });
                        out.changed = true;
                        ui.close_menu();
                    }
                    if ui.button("Noise").clicked() {
                        layer.mask.set_single(LayerContent::Noise {
                            scale: 4.0,
                            strength: 1.0,
                            octaves: 4.0,
                            noise_type: crate::shader_nodes::NoiseType::Fbm,
                        });
                        out.changed = true;
                        ui.close_menu();
                    }
                    let gr = ui
                        .add_enabled(
                            gen.has_sdf || gen.has_poly_mesh,
                            egui::Button::new("Curvature"),
                        )
                        .on_disabled_hover_text("Needs an SDF body or a polygonal mesh");
                    if gr.clicked() {
                        layer.mask.set_single(LayerContent::Generator {
                            source: GeneratorSource::Curvature,
                            radius: gen.radius_floor.unwrap_or(0.02).max(0.02),
                            invert: false,
                            output: GeneratorOutput::ConvexOnly,
                            remap: [0.0, 1.0],
                            rung: if gen.has_uvs {
                                GeneratorRung::Auto
                            } else {
                                GeneratorRung::VertexCache
                            },
                            sharpness: 1.0,
                            axis: DirectionAxis::default(),
                        });
                        out.changed = true;
                        ui.close_menu();
                    }
                    let ao = ui
                        .add_enabled(gen.has_sdf, egui::Button::new("Ambient Occlusion"))
                        .on_disabled_hover_text("SDF bodies only");
                    if ao.clicked() {
                        layer.mask.set_single(LayerContent::Generator {
                            source: GeneratorSource::AmbientOcclusion,
                            radius: gen.radius_floor.unwrap_or(0.02).max(0.02),
                            invert: false,
                            output: GeneratorOutput::default(),
                            remap: [0.0, 1.0],
                            // Texture rung — AO is a bake/sample analyzer, no vertex kernel.
                            rung: GeneratorRung::Auto,
                            sharpness: 1.0,
                            axis: DirectionAxis::default(),
                        });
                        out.changed = true;
                        ui.close_menu();
                    }
                    let dr = ui
                        .add_enabled(gen.has_sdf, egui::Button::new("Direction"))
                        .on_disabled_hover_text("SDF bodies only");
                    if dr.clicked() {
                        layer.mask.set_single(LayerContent::Generator {
                            source: GeneratorSource::Direction,
                            radius: gen.radius_floor.unwrap_or(0.02).max(0.02),
                            invert: false,
                            output: GeneratorOutput::default(),
                            remap: [0.0, 1.0],
                            // Texture rung always — Direction has no vertex kernel.
                            rung: GeneratorRung::Auto,
                            sharpness: 1.0,
                            axis: DirectionAxis::default(),
                        });
                        out.changed = true;
                        ui.close_menu();
                    }
                },
            );
        }
    });

    // Nested mask: "Add mask on mask" when primary exists (P3 recursive stacks).
    if !layer.mask.is_empty() {
        ui.horizontal(|ui| {
            if ui
                .small_button(egui::RichText::new("+ mask on mask").size(9.0).color(t::TEXT_DIM))
                .on_hover_text("Add a nested mask stack entry (mask the mask)")
                .clicked()
            {
                if let Some(top) = layer.mask.layers.last_mut() {
                    if top.mask.layers.len() < MASK_STACK_MAX_DEPTH {
                        top.mask.layers.push(MaskLayer::from_content(LayerContent::Noise {
                            scale: 8.0,
                            strength: 1.0,
                            octaves: 3.0,
                            noise_type: crate::shader_nodes::NoiseType::Fbm,
                        }));
                        out.changed = true;
                    }
                }
            }
        });
    }

    let Some(mask) = layer.mask.primary_content_mut() else {
        return;
    };
    match mask {
        LayerContent::Image { tex_id, strength } => {
            ui.horizontal(|ui| {
                if let Some(tid) = thumbs.get(tex_id) {
                    ui.image(egui::load::SizedTexture::new(*tid, egui::vec2(18.0, 18.0)));
                }
                if ui
                    .add(egui::Button::new(egui::RichText::new("Load…").size(10.0)))
                    .clicked()
                {
                    out.load_mask = Some(li);
                }
                ui.label(egui::RichText::new("Strength").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::DragValue::new(strength)
                        .speed(0.01)
                        .range(0.0..=1.0)
                        .fixed_decimals(2),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
            });
        }
        LayerContent::Constant { value } => {
            let s = match value {
                ChannelValue::Scalar(s) => s,
                ChannelValue::Color(c) => {
                    *value = ChannelValue::Scalar(c[0]);
                    if let ChannelValue::Scalar(s) = value {
                        s
                    } else {
                        unreachable!()
                    }
                }
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Value").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::DragValue::new(s)
                        .speed(0.005)
                        .range(0.0..=1.0)
                        .fixed_decimals(2),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
            });
        }
        LayerContent::Noise {
            scale,
            strength,
            octaves,
            noise_type,
        } => {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("mp_msk_nse", kind_u, li))
                    .selected_text(egui::RichText::new(noise_type.short()).size(10.0))
                    .width(56.0)
                    .show_ui(ui, |ui| {
                        for nt in crate::shader_nodes::NoiseType::all() {
                            if ui.selectable_label(*noise_type == *nt, nt.label()).clicked() {
                                *noise_type = *nt;
                                out.changed = true;
                            }
                        }
                    });
                let r = ui.add(
                    egui::DragValue::new(scale)
                        .speed(0.05)
                        .range(0.01..=64.0)
                        .fixed_decimals(2),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
                let r = ui.add(
                    egui::DragValue::new(octaves)
                        .speed(0.25)
                        .range(1.0..=12.0)
                        .fixed_decimals(0),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
                let r = ui.add(
                    egui::DragValue::new(strength)
                        .speed(0.01)
                        .range(0.0..=1.0)
                        .fixed_decimals(2),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
            });
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
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(generator_source_label(*source))
                        .size(10.0)
                        .color(t::TEXT_SECONDARY),
                );
                let r_floor = gen.radius_floor.unwrap_or(0.02).max(1e-4);
                if *radius < r_floor {
                    *radius = r_floor;
                    out.changed = true;
                }
                // Direction has no vertex kernel — pin it to the texture rung even on a body
                // with no UVs, or the mask silently contributes nothing (see lower.rs).
                if *source == GeneratorSource::Direction {
                    *rung = GeneratorRung::Auto;
                } else if !gen.has_uvs {
                    *rung = GeneratorRung::VertexCache;
                }
                ui.label(egui::RichText::new("R").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::DragValue::new(radius)
                        .speed(0.002)
                        .range(r_floor..=10.0)
                        .fixed_decimals(3),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
                if *source == GeneratorSource::Direction {
                    egui::ComboBox::from_id_salt(("mp_mskgen_axis", kind_u, li))
                        .selected_text(egui::RichText::new(axis.label()).size(10.0))
                        .width(84.0)
                        .show_ui(ui, |ui| {
                            for a in DirectionAxis::ALL {
                                if ui.selectable_label(*axis == a, a.label()).clicked()
                                    && *axis != a
                                {
                                    *axis = a;
                                    out.changed = true;
                                }
                            }
                        });
                }
                if *source == GeneratorSource::Curvature {
                    for (o, s) in [
                        (GeneratorOutput::Signed, "±"),
                        (GeneratorOutput::ConvexOnly, "Cx"),
                        (GeneratorOutput::ConcaveOnly, "Cc"),
                    ] {
                        if ui
                            .add(egui::SelectableLabel::new(*output == o, egui::RichText::new(s).size(10.0)))
                            .clicked()
                            && *output != o
                        {
                            *output = o;
                            out.changed = true;
                        }
                    }
                }
                if ui.checkbox(invert, egui::RichText::new("Inv").size(10.0)).changed() {
                    out.changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Remap").size(10.0).color(t::TEXT_DIM));
                for i in 0..2 {
                    let r = ui.add(
                        egui::DragValue::new(&mut remap[i])
                            .speed(0.01)
                            .range(-1.0..=1.0)
                            .fixed_decimals(2),
                    );
                    out.changed |= r.changed();
                    out.drag_started |= r.drag_started();
                }
                ui.label(egui::RichText::new("Soft/Hard").size(10.0).color(t::TEXT_DIM));
                let r = ui.add(
                    egui::DragValue::new(sharpness)
                        .speed(0.01)
                        .range(0.001..=64.0)
                        .fixed_decimals(3)
                        .clamp_existing_to_range(false),
                );
                out.changed |= r.changed();
                out.drag_started |= r.drag_started();
            });
        }
    }
}
