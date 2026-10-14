// shader_nodes/editor.rs
use crate::ui_menus::AzraMenus;
use eframe::egui;
use super::node_type::NodeType;
use super::node_data::{Node, NodeId};
use super::graph::{Connection, NodeGraph};
use super::ops::*;
use super::types::*;
use super::palette::NodePalette;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

const NOISE_PREVIEW_TEXTURE_PX: usize = 64;
const NOISE_PREVIEW_DRAW_SIZE: f32 = 112.0;
const NOISE_PREVIEW_ROWS: usize = 6;
const MAX_NOISE_PREVIEWS_PER_FRAME: usize = 2;


#[derive(Default)]
pub struct NodeEditorOutcome {
    pub changed: bool,
    /// G7: a DISCRETE gesture completed this frame (link connect/cut/insert, node
    /// add/delete/duplicate/mute, F connect, G confirm) — the caller makes ONE undo
    /// entry. False for continuous field scrubs (`field_changed`), which would spam
    /// an entry per frame. The caller snapshots BEFORE `commit_material_node_graph_edit`
    /// inserts the working copy, so the entry holds the pre-gesture graph.
    pub discrete: bool,
    pub atomic_change: bool,
    /// Graph state from before the input event that produced `changed`.
    pub graph_before_edit: Option<NodeGraph>,
    /// User clicked Load Image on ImageTexture — app must call the SAME `request_map_file_dialog`
    /// path as Properties map UI (`MapTarget::Graph` / Prim).
    pub load_map_kind: Option<u32>, // MapKind::wire_tag
}

// ====================================================================
// UI: Minimal node editor drawn with egui
// ====================================================================

/// Draw the node graph editor. Uses `NodePalette` for add + drag-link menus (no duplicate lists).
/// `prim_choices`: scene prims for Prim SDF Mask / SDF Curvature pickers `(prim_uid, name)`.
pub fn draw_node_editor(
    ui: &mut egui::Ui,
    graph: &mut NodeGraph,
    drag_state: &mut NodeDragState,
    prim_choices: &[(u32, String)],
) -> NodeEditorOutcome {
    let mut outcome = NodeEditorOutcome::default();
    let mut changed = false;
    let mut atomic_graph_before_edit = None;
    let mut noise_previews_rendered = 0usize;
    let noise_preview_fingerprint = shader_graph_fingerprint(graph);
    let _origin = ui.min_rect().min;
    let pointer_edit_pressed = ui.input(|i| {
        i.pointer.button_pressed(egui::PointerButton::Primary)
            || i.pointer.button_pressed(egui::PointerButton::Secondary)
    });
    if pointer_edit_pressed && drag_state.graph_before_pointer_edit.is_none() {
        drag_state.graph_before_pointer_edit = Some(graph.clone());
    }
    let graph_before_keyboard_edit = ui.input(|i| {
        i.events.iter().any(|event| matches!(event,
            egui::Event::Copy
            | egui::Event::Cut
            | egui::Event::Paste(_)
            | egui::Event::Text(_)
            | egui::Event::Key { pressed: true, .. }
            | egui::Event::MouseWheel { .. }
            | egui::Event::Ime(egui::ImeEvent::Commit(_))
        ))
    }).then(|| graph.clone());

    // --- Toolbar row ---
    ui.horizontal(|ui| {
        ui.azra_menu_button("+ Add Node", |ui| {
            ui.set_min_width(220.0);
            let search = &mut drag_state.toolbar_add_search;
            let te = ui.add(
                egui::TextEdit::singleline(search)
                    .desired_width(200.0)
                    .hint_text("Search nodes…")
                    .font(egui::TextStyle::Body),
            );
            // Focus search when menu opens / when user starts typing into the menu.
            te.request_focus();
            ui.separator();
            let filter = search.clone();
            let mut any = false;
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    for (cat_name, entries) in NodePalette::categories() {
                        let matches: Vec<_> = entries
                            .iter()
                            .filter(|(nt, label)| {
                                if *nt == NodeType::MaterialOutput
                                    && graph
                                        .nodes
                                        .iter()
                                        .any(|n| n.node_type == NodeType::MaterialOutput)
                                {
                                    return false;
                                }
                                palette_matches(&filter, cat_name, label)
                            })
                            .collect();
                        if matches.is_empty() {
                            continue;
                        }
                        any = true;
                        // Flat list when searching; category headers when browsing.
                        if filter.trim().is_empty() {
                            ui.label(egui::RichText::new(*cat_name).small().weak());
                        }
                        for (nt, label) in matches {
                            let show = if filter.trim().is_empty() {
                                (*label).to_string()
                            } else {
                                format!("{cat_name} · {label}")
                            };
                            if ui.selectable_label(false, show).clicked() {
                                let id = graph.add_node(nt.clone(), [150.0, 150.0]);
                                apply_palette_preset(graph, id, label);
                                drag_state.select_single(id);
                                drag_state.toolbar_add_search.clear();
                                changed = true;
                                ui.close_menu();
                            }
                        }
                    }
                    if !any {
                        ui.label(egui::RichText::new("No matches").weak().italics());
                    }
                });
        });
        // Settings live on nodes (Noise params, Master Material bake res) — no panel clutter.
        if drag_state.connecting_from.is_some() {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Drag to a socket, or release on empty canvas to pick a node",
            );
        }
        if !drag_state.selected.is_empty() && ui.button("Delete Node").clicked() {
            let removable: Vec<NodeId> = graph
                .nodes
                .iter()
                .filter(|node| {
                    drag_state.selected.contains(&node.id)
                        && !matches!(node.node_type, NodeType::MaterialOutput)
                })
                .map(|node| node.id)
                .collect();
            if !removable.is_empty() {
                for node_id in removable {
                    graph.remove_node(node_id);
                }
                drag_state.clear_selection();
                changed = true;
            }
        }
        if !drag_state.selected.is_empty() && ui.button("Reset Values").on_hover_text("Restore selected nodes' numeric and color values; keep connections and node types").clicked() {
            atomic_graph_before_edit = Some(graph.clone());
            for node in &mut graph.nodes {
                if !drag_state.selected.contains(&node.id) { continue; }
                let defaults = Node::new(node.id, node.node_type.clone(), node.pos);
                node.input_defaults = defaults.input_defaults;
                node.const_float = defaults.const_float;
                node.ramp_color0 = defaults.ramp_color0;
                node.ramp_color1 = defaults.ramp_color1;
                node.ramp_pos0 = defaults.ramp_pos0;
                node.ramp_pos1 = defaults.ramp_pos1;
                node.noise_black = defaults.noise_black;
                node.noise_white = defaults.noise_white;
                node.noise_curve_k = defaults.noise_curve_k;
                node.proj_off_x = defaults.proj_off_x;
                node.proj_off_y = defaults.proj_off_y;
                node.proj_off_z = defaults.proj_off_z;
                node.proj_rot_x = defaults.proj_rot_x;
                node.proj_rot_y = defaults.proj_rot_y;
                node.proj_rot_z = defaults.proj_rot_z;
            }
            graph.wgsl_cache = None;
            changed = true;
        }
    });

    ui.add_space(4.0);

    // Esc or RMB on canvas cancels pending connection
    if drag_state.connecting_from.is_some() {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            drag_state.connecting_from = None;
        }
    }

    // ── G6 Blender keymap — every verb operates on the SELECTION SET ─────────────────
    // The ops live on NodeGraph (remove_nodes / duplicate_nodes / toggle_mute /
    // toggle_collapse) and are headlessly tested in node_graph_bake::g6_ops; this block is
    // only the binding.
    if !drag_state.selected.is_empty() && !drag_state.field_interact && !ui.ctx().wants_keyboard_input() {
        let (del, shift_d, m_key, h_key, g_key, f_key) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Delete) || (!i.modifiers.ctrl && i.key_pressed(egui::Key::X)),
                i.modifiers.shift && i.key_pressed(egui::Key::D),
                i.key_pressed(egui::Key::M),
                i.key_pressed(egui::Key::H),
                i.key_pressed(egui::Key::G),
                i.key_pressed(egui::Key::F),
            )
        });
        if del {
            let removed = graph.remove_nodes(&drag_state.selected);
            if removed > 0 {
                drag_state.selected.retain(|id| graph.find_node(*id).is_some());
                drag_state.selected_node =
                    drag_state.selected_node.filter(|id| graph.find_node(*id).is_some());
                changed = true;
            }
        }
        if shift_d {
            let map = graph.duplicate_nodes(&drag_state.selected, [30.0, 30.0]);
            if !map.is_empty() {
                // Blender: the duplicate becomes the selection, ready to place.
                drag_state.selected = map.values().copied().collect();
                drag_state.selected_node = drag_state
                    .selected_node
                    .and_then(|old| map.get(&old).copied())
                    .or_else(|| map.values().next().copied());
                changed = true;
            }
        }
        if m_key {
            graph.toggle_mute(&drag_state.selected);
            changed = true;
        }
        if h_key {
            graph.toggle_collapse(&drag_state.selected);
        }
        if f_key && drag_state.selected.len() == 2 {
            // Blender F: connect the two selected nodes, left → right by position.
            // First output of the left node into the right node's first FREE
            // compatible input; occupied inputs are skipped, never stolen.
            let mut pair: Vec<(NodeId, f32)> = drag_state
                .selected
                .iter()
                .filter_map(|id| graph.find_node(*id).map(|n| (*id, n.pos[0])))
                .collect();
            pair.sort_by(|a, b| a.1.total_cmp(&b.1));
            if let [(left, _), (right, _)] = pair[..] {
                let free: Vec<usize> = graph
                    .find_node(right)
                    .map(|n| {
                        (0..Node::input_defs(&n.node_type).len())
                            .filter(|i| !graph.is_input_connected(right, *i))
                            .collect()
                    })
                    .unwrap_or_default();
                for i in free {
                    if graph.connect(left, 0, right, i) {
                        changed = true;
                        break;
                    }
                }
            }
        }
        if g_key && drag_state.grab_orig.is_none() {
            drag_state.graph_before_grab = graph_before_keyboard_edit.clone();
            // Blender G: free-move the selection with the bare pointer; LMB confirms,
            // Esc restores. Original positions captured for the cancel path.
            drag_state.grab_orig = Some(
                drag_state
                    .selected
                    .iter()
                    .filter_map(|id| graph.find_node(*id).map(|n| (*id, n.pos)))
                    .collect(),
            );
        }
    }
    // G grab: pointer motion moves the selection; LMB confirms; Esc cancels+restores.
    if let Some(orig) = &drag_state.grab_orig {
        let delta = ui.input(|i| i.pointer.delta());
        for (id, _) in orig.iter() {
            if let Some(n) = graph.find_node_mut(*id) {
                n.pos[0] += delta.x;
                n.pos[1] += delta.y;
            }
        }
        let (confirm, cancel) = ui.input(|i| {
            (
                i.pointer.button_clicked(egui::PointerButton::Primary),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if cancel {
            let orig = drag_state.grab_orig.take().unwrap();
            drag_state.graph_before_grab = None;
            for (id, pos) in orig {
                if let Some(n) = graph.find_node_mut(id) {
                    n.pos = pos;
                }
            }
        } else if confirm {
            drag_state.grab_orig = None;
            atomic_graph_before_edit = drag_state.graph_before_grab.take();
            changed = true;
        }
    }

    // Legacy single-node arms (kept: Ctrl+D alias, single-select delete when set is empty).
    if let Some(sel) = drag_state.selected_node.filter(|_| !drag_state.field_interact && !ui.ctx().wants_keyboard_input()) {
        if drag_state.selected.is_empty()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || (!i.modifiers.ctrl && i.key_pressed(egui::Key::X)))
        {
            if let Some(n) = graph.find_node(sel) {
                if n.node_type != NodeType::MaterialOutput {
                    graph.remove_node(sel);
                    drag_state.selected_node = None;
                    changed = true;
                }
                drag_state.clear_selection();
                changed = true;
            }
        }
        if ui.input(|i| i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::D)) {
            let copies = graph.duplicate_nodes(&HashSet::from([sel]), [30.0, 30.0]);
            if let Some(new_id) = copies.get(&sel) {
                drag_state.select_single(*new_id);
                changed = true;
            }
        }
    }

    // --- Draw nodes (no ScrollArea — manual pan with MMB) ---
    // CRITICAL: canvas must NOT use Sense::click_and_drag() — that steals LMB drag from
    // on-node DragValues (Scale / Octaves / …) so click-drag-to-edit feels broken.
    // Pan uses raw pointer delta (MMB / Alt+LMB) instead of claiming primary drag.
    //
    // Size = caller's reserved rect only (win_node_editor reserves footer first).
    // Never allocate more than available — extra widgets below used to grow the Window
    // every continuous-repaint frame (viewport MMB orbit).
    let canvas_size = ui.available_size().max(egui::vec2(80.0, 60.0));
    // CRITICAL: painter uses Sense::click() only — NOT click_and_drag — so on-node DragValues
    // (Scale / Octaves / …) keep LMB scrub. Window Area must not steal empty-canvas / node-header
    // drags either: register a drag SINK under the nodes (outliner_tree_dnd_guard pattern).
    let (resp, mut painter) = ui.allocate_painter(canvas_size, egui::Sense::click());
    let canvas_rect = resp.rect;
    // Drag sink: claims LMB drag over the canvas so the floating Window does not move.
    // Registered before node widgets — but it MUST yield while the pointer is on a FIELD:
    // registered unconditionally it FIGHTS the DragValue for the drag frame-by-frame and
    // swallows most pointer deltas (owner 2026-07-23: a 150px scrub delivered ~19px of value
    // change — "choky, no live feedback"). `field_interact` is set during node draw (later),
    // so gate on LAST frame's value (one-frame lag is invisible; a drag that starts on a
    // field simply never arms the sink).
    let field_hover_prev = drag_state.field_interact;
    if !field_hover_prev {
        let _ = ui.interact(
            canvas_rect,
            ui.id().with("node_editor_canvas_drag_guard"),
            egui::Sense::drag(),
        );
    }
    let canvas_origin = egui::pos2(
        canvas_rect.min.x + drag_state.canvas_offset[0],
        canvas_rect.min.y + drag_state.canvas_offset[1],
    );
    // Reset per-frame: true while pointer is on a field so header/node drag won't fight DragValue.
    drag_state.field_interact = false;
    drag_state.field_changed = false;
    drag_state.color_swatch_hit = false;

    // Clip to canvas
    painter.set_clip_rect(canvas_rect);

    // CRITICAL (owner #5 "shader panel keeps expanding"): node bodies are placed at absolute
    // screen positions (canvas_origin + node.pos), and panning can push them BELOW / RIGHT of
    // canvas_rect. `ui.put()` / `ui.allocate_ui_at_rect()` on the PARENT ui grow its min_rect to
    // enclose those widget rects, which the enclosing Window reports as content size → the Window
    // grows → available_size() grows next frame → nodes can sit even lower → unbounded ratchet on
    // every continuous-repaint frame. Isolate all node-body widgets in a child ui pinned to
    // canvas_rect: `new_child` does NOT advance the parent cursor, so the parent's reported size
    // stays exactly canvas_rect no matter where nodes are dragged. Widget input still works (shared
    // context); only the size feedback is severed. `nui` is clipped to the canvas too.
    let mut nui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(canvas_rect)
            .layout(*ui.layout()),
    );
    nui.set_clip_rect(canvas_rect);
    nui.style_mut().override_font_id = Some(egui::FontId::proportional(10.0));
    nui.spacing_mut().interact_size = egui::vec2(28.0, 16.0);
    nui.spacing_mut().button_padding = egui::vec2(2.0, 1.0);

    // Grid background
    {
        let grid_col = egui::Color32::from_gray(35);
        let step = 40.0;
        let ox = drag_state.canvas_offset[0] % step;
        let oy = drag_state.canvas_offset[1] % step;
        let mut x = canvas_rect.min.x + ox;
        while x < canvas_rect.max.x { painter.line_segment([egui::pos2(x, canvas_rect.min.y), egui::pos2(x, canvas_rect.max.y)], egui::Stroke::new(0.5, grid_col)); x += step; }
        let mut y = canvas_rect.min.y + oy;
        while y < canvas_rect.max.y { painter.line_segment([egui::pos2(canvas_rect.min.x, y), egui::pos2(canvas_rect.max.x, y)], egui::Stroke::new(0.5, grid_col)); y += step; }
    }

    // MMB pan or Alt+LMB pan — ONLY when the pointer is over this canvas.
    // Global MMB (viewport orbit) must not pan the node graph or fight layout.
    let pointer_over_canvas = ui.rect_contains_pointer(canvas_rect)
        || ui
            .input(|i| i.pointer.interact_pos())
            .map(|p| canvas_rect.contains(p))
            .unwrap_or(false);
    let mmb_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
    let alt_lmb =
        ui.input(|i| i.modifiers.alt && i.pointer.button_down(egui::PointerButton::Primary));
    if pointer_over_canvas && (mmb_down || alt_lmb) {
        let delta = ui.input(|i| i.pointer.delta());
        drag_state.canvas_offset[0] += delta.x;
        drag_state.canvas_offset[1] += delta.y;
    }

    // ── G6 BOX SELECT (Blender: LMB drag on empty canvas; pan stays MMB / Alt+LMB) ──────
    // Armed on drag start over the canvas when nothing else owns the pointer; resolved on
    // release against each node's screen rect (computed below in the node loop — the loop
    // records rects into `box_hits` for this frame). Shift extends, plain replaces.
    let mut box_rect_now: Option<egui::Rect> = None;
    {
        let lmb_pressed_bg =
            ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        let lmb_down_bg = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
        if lmb_pressed_bg
            && pointer_over_canvas
            && !mmb_down
            && !alt_lmb
            && drag_state.connecting_from.is_none()
            && drag_state.dragging_node.is_none()
            && drag_state.grab_orig.is_none()
            && !drag_state.field_interact
        {
            // Only arm when the press is NOT on a node — decided in the node loop, which
            // disarms by setting `box_select_from = None` when a header/body is hit.
            if let Some(mp) = ui.input(|i| i.pointer.interact_pos()) {
                drag_state.box_select_from = Some(mp);
            }
        }
        if let (Some(from), Some(cur)) = (
            drag_state.box_select_from,
            ui.input(|i| i.pointer.interact_pos()),
        ) {
            if lmb_down_bg {
                let r = egui::Rect::from_two_pos(from, cur);
                // Ignore sub-4px jitters so a plain click never reads as a box.
                if r.width() > 4.0 || r.height() > 4.0 {
                    box_rect_now = Some(r);
                }
            }
        }
    }
    // Screen rect per node this frame — filled by the node loop, consumed on box release.
    let mut box_hits: Vec<(NodeId, egui::Rect)> = Vec::new();

    // ── G7 LINK CUT (Blender Ctrl+RMB drag): freehand stroke; on release every link
    // whose noodle the stroke crosses is removed — exactly the crossed ones.
    {
        let rmb_pressed = ui.input(|i| {
            i.pointer.button_pressed(egui::PointerButton::Secondary) && i.modifiers.ctrl
        });
        let rmb_down =
            ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
        if rmb_pressed && pointer_over_canvas && drag_state.cut_stroke.is_none() {
            if let Some(mp) = ui.input(|i| i.pointer.interact_pos()) {
                drag_state.cut_stroke = Some(vec![mp]);
            }
        }
        if let Some(stroke) = &mut drag_state.cut_stroke {
            if rmb_down {
                if let Some(mp) = ui.input(|i| i.pointer.interact_pos()) {
                    if stroke.last().map(|l| l.distance(mp) > 4.0).unwrap_or(true) {
                        stroke.push(mp);
                    }
                }
                for w in stroke.windows(2) {
                    painter.line_segment(
                        [w[0], w[1]],
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(230, 80, 80)),
                    );
                }
            } else {
                // Release: sample each noodle with draw_noodle's OWN control points and
                // drop every connection a stroke segment crosses.
                let stroke = drag_state.cut_stroke.take().unwrap();
                if stroke.len() >= 2 {
                    let crossed: Vec<usize> = graph
                        .connections
                        .iter()
                        .enumerate()
                        .filter_map(|(ci, conn)| {
                            let fp = node_output_pos(
                                graph, conn.from_node, conn.from_output, canvas_origin,
                            )?;
                            let tp = node_input_pos(
                                graph, conn.to_node, conn.to_input, canvas_origin,
                            )?;
                            noodle_crosses_stroke(fp, tp, &stroke).then_some(ci)
                        })
                        .collect();
                    if !crossed.is_empty() {
                        for ci in crossed.into_iter().rev() {
                            graph.connections.remove(ci);
                        }
                        graph.wgsl_cache = None;
                        changed = true;
                    }
                }
            }
        }
    }

    // P8: pending "frame this node" request (material panel → graph). Center the
    // node in the canvas by solving for the pan offset, then select it. Runs after
    // the pan handlers so it wins this frame. One-shot: cleared once applied.
    if let Some(fid) = drag_state.frame_request {
        if let Some(node) = graph.find_node(fid) {
            // Node top-left in screen space = canvas_rect.min + offset + node.pos.
            // Aim its visual center (body width/2, ~header+one row) at the canvas center.
            let node_center_local = egui::vec2(
                node_body_w(&node.node_type) * 0.5,
                node_body_top_local(&node.node_type) + NODE_ROW_H,
            );
            let want = canvas_rect.center() - canvas_rect.min.to_vec2();
            drag_state.canvas_offset[0] = want.x - node.pos[0] - node_center_local.x;
            drag_state.canvas_offset[1] = want.y - node.pos[1] - node_center_local.y;
            drag_state.select_single(fid);
        }
        // Clear whether or not the node still exists (stale request must not loop).
        drag_state.frame_request = None;
    }

    // Click on empty canvas: cancel connection or deselect (not while editing a field)
    if resp.clicked() && drag_state.dragging_node.is_none() && !drag_state.field_interact {
        // Defer empty-canvas deselect until after widgets — field_interact is set during node draw.
        // Actual deselect runs in post-pass below if still empty.
        drag_state.pending_canvas_click = true;
    } else {
        drag_state.pending_canvas_click = false;
    }
    if resp.secondary_clicked() {
        drag_state.connecting_from = None;
    }

    // On LMB release: complete drag-to-connect on a socket, OR open the full node picker.
    let pointer_released = ui.input(|i| i.pointer.any_released());
    let mouse_pos = ui.input(|i| i.pointer.hover_pos());

    if pointer_released && drag_state.connecting_from.is_some() && drag_state.link_drop_menu.is_none() {
        if let Some(mp) = mouse_pos {
            let mut completed = false;
            let (from_nid, from_idx, from_is_output) = drag_state.connecting_from.unwrap();
            let node_ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
            for nid in &node_ids {
                let node = match graph.find_node(*nid) { Some(n) => n, None => continue };
                let np = egui::pos2(canvas_origin.x + node.pos[0], canvas_origin.y + node.pos[1]);
                let body_y0 = np.y + node_body_top_local(&node.node_type);
                if from_is_output {
                    let inputs = Node::input_defs(&node.node_type);
                    for (i, _) in inputs.iter().enumerate() {
                        let sy = body_y0 + i as f32 * NODE_ROW_H + NODE_ROW_H * 0.5;
                        let dot = egui::pos2(np.x, sy);
                        if (mp.x - dot.x).abs() < 12.0 && (mp.y - dot.y).abs() < 12.0 && *nid != from_nid {
                            if graph.connect(from_nid, from_idx, *nid, i) {
                                changed = true; completed = true; break;
                            }
                        }
                    }
                } else {
                    let outputs = Node::output_defs(&node.node_type);
                    for (i, _) in outputs.iter().enumerate() {
                        let sy = body_y0 + i as f32 * NODE_ROW_H + NODE_ROW_H * 0.5;
                        let dot = egui::pos2(np.x + NODE_BODY_W, sy);
                        if (mp.x - dot.x).abs() < 12.0 && (mp.y - dot.y).abs() < 12.0 && *nid != from_nid {
                            if graph.connect(*nid, i, from_nid, from_idx) {
                                changed = true; completed = true; break;
                            }
                        }
                    }
                }
                if completed { break; }
            }
            if completed {
                drag_state.connecting_from = None;
            } else {
                // Dropped on empty canvas → open full compatible node menu (user request).
                let sock = socket_type_of(graph, from_nid, from_idx, from_is_output)
                    .unwrap_or(SocketType::Float);
                // Convert screen pos to graph space for new node placement
                let place = [
                    mp.x - canvas_origin.x,
                    mp.y - canvas_origin.y,
                ];
                drag_state.link_drop_menu = Some(LinkDropMenu {
                    screen_pos: [mp.x, mp.y],
                    place_pos: place,
                    from_nid,
                    from_idx,
                    from_is_output,
                    sock,
                    search: String::new(),
                });
                // Keep connecting_from until menu resolves or cancel
            }
        }
    }

    // Shift+A Add Node palette (full catalog) — placed under cursor in graph space.
    if let Some(mut menu) = drag_state.add_node_menu.clone() {
        // Convert screen → graph coords now that canvas_origin is known.
        menu.place_pos = [
            menu.screen_pos[0] - canvas_origin.x,
            menu.screen_pos[1] - canvas_origin.y,
        ];

        let mut picked: Option<(NodeType, &'static str)> = None;
        let mut cancel = false;
        let mut search = menu.search.clone();
        egui::Area::new(egui::Id::new("node_shift_a_add_menu"))
            .fixed_pos(egui::pos2(menu.screen_pos[0], menu.screen_pos[1]))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    ui.set_max_height(440.0);
                    ui.label(egui::RichText::new("Add node").strong());
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut search)
                            .desired_width(200.0)
                            .hint_text("Search… (type to filter)")
                            .font(egui::TextStyle::Body),
                    );
                    te.request_focus();
                    ui.separator();
                    let filter = search.clone();
                    let mut any = false;
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for (cat_name, entries) in NodePalette::categories() {
                            let matches: Vec<_> = entries
                                .iter()
                                .filter(|(nt, label)| {
                                    if *nt == NodeType::MaterialOutput
                                        && graph
                                            .nodes
                                            .iter()
                                            .any(|n| n.node_type == NodeType::MaterialOutput)
                                    {
                                        return false;
                                    }
                                    palette_matches(&filter, cat_name, label)
                                })
                                .collect();
                            if matches.is_empty() {
                                continue;
                            }
                            any = true;
                            if filter.trim().is_empty() {
                                ui.label(egui::RichText::new(*cat_name).small().weak());
                            }
                            for (nt, label) in matches {
                                let show = if filter.trim().is_empty() {
                                    (*label).to_string()
                                } else {
                                    format!("{cat_name} · {label}")
                                };
                                if ui.selectable_label(false, show).clicked() {
                                    picked = Some((nt.clone(), *label));
                                }
                            }
                        }
                        if !any {
                            ui.label(egui::RichText::new("No matches").weak().italics());
                        }
                    });
                    ui.separator();
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        menu.search = search;
        drag_state.add_node_menu = Some(menu.clone());

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        // Enter picks the first match when searching.
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && picked.is_none() {
            let filter = menu.search.clone();
            'outer: for (cat_name, entries) in NodePalette::categories() {
                for (nt, label) in *entries {
                    if *nt == NodeType::MaterialOutput
                        && graph
                            .nodes
                            .iter()
                            .any(|n| n.node_type == NodeType::MaterialOutput)
                    {
                        continue;
                    }
                    if palette_matches(&filter, cat_name, label) {
                        picked = Some((nt.clone(), *label));
                        break 'outer;
                    }
                }
            }
        }
        // Click outside cancels (ignore the frame we opened on).
        if ui.input(|i| i.pointer.any_click()) && picked.is_none() && !cancel {
            if let Some(mp) = mouse_pos {
                let r = egui::Rect::from_min_size(
                    egui::pos2(menu.screen_pos[0], menu.screen_pos[1]),
                    egui::vec2(240.0, 460.0),
                );
                if !r.contains(mp) {
                    cancel = true;
                }
            }
        }
        if let Some((nt, label)) = picked {
            let new_id = graph.add_node(nt, menu.place_pos);
            apply_palette_preset(graph, new_id, label);
            drag_state.select_single(new_id);
            drag_state.add_node_menu = None;
            changed = true;
        } else if cancel {
            drag_state.add_node_menu = None;
        }
    }

    // Link-drop popup: all compatible options from the shared NodePalette
    if let Some(mut menu) = drag_state.link_drop_menu.clone() {
        let mut picked: Option<(NodeType, &'static str, usize)> = None;
        let mut cancel = false;
        let mut search = menu.search.clone();
        egui::Area::new(egui::Id::new("node_link_drop_menu"))
            .fixed_pos(egui::pos2(menu.screen_pos[0], menu.screen_pos[1]))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    ui.label(egui::RichText::new("Add node").strong());
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut search)
                            .desired_width(200.0)
                            .hint_text("Search… (type to filter)"),
                    );
                    te.request_focus();
                    ui.separator();
                    let filter = search.clone();
                    let targets = NodePalette::link_targets(menu.from_is_output, menu.sock);
                    if targets.is_empty() {
                        ui.label(egui::RichText::new("No compatible nodes").weak());
                    } else {
                        let mut any = false;
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            for (cat_name, entries) in NodePalette::categories() {
                                let cat_items: Vec<_> = targets
                                    .iter()
                                    .filter(|(nt, label, _)| {
                                        entries.iter().any(|(e, _)| e == nt)
                                            && palette_matches(&filter, cat_name, label)
                                    })
                                    .cloned()
                                    .collect();
                                if cat_items.is_empty() {
                                    continue;
                                }
                                any = true;
                                if filter.trim().is_empty() {
                                    ui.label(egui::RichText::new(*cat_name).small().weak());
                                }
                                for (nt, label, sock_i) in cat_items {
                                    let show = if filter.trim().is_empty() {
                                        label.to_string()
                                    } else {
                                        format!("{cat_name} · {label}")
                                    };
                                    if ui.selectable_label(false, show).clicked() {
                                        picked = Some((nt, label, sock_i));
                                    }
                                }
                            }
                            if !any {
                                ui.label(egui::RichText::new("No matches").weak().italics());
                            }
                        });
                    }
                    ui.separator();
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        menu.search = search;
        drag_state.link_drop_menu = Some(menu.clone());
        // Click outside cancels
        if ui.input(|i| i.pointer.any_click()) && picked.is_none() && !cancel {
            if let Some(mp) = mouse_pos {
                let r = egui::Rect::from_min_size(
                    egui::pos2(menu.screen_pos[0], menu.screen_pos[1]),
                    egui::vec2(220.0, 320.0),
                );
                if !r.contains(mp) {
                    cancel = true;
                }
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if let Some((nt, label, sock_i)) = picked {
            let new_id = graph.add_node(nt, menu.place_pos);
            apply_palette_preset(graph, new_id, label);
            let ok = if menu.from_is_output {
                graph.connect(menu.from_nid, menu.from_idx, new_id, sock_i)
            } else {
                graph.connect(new_id, sock_i, menu.from_nid, menu.from_idx)
            };
            if ok {
                changed = true;
            }
            drag_state.select_single(new_id);
            drag_state.link_drop_menu = None;
            drag_state.connecting_from = None;
        } else if cancel {
            drag_state.link_drop_menu = None;
            drag_state.connecting_from = None;
        }
    }

    // Draw existing connections (bezier curves)
    for conn in &graph.connections {
        let from_pos = node_output_pos(graph, conn.from_node, conn.from_output, canvas_origin);
        let to_pos = node_input_pos(graph, conn.to_node, conn.to_input, canvas_origin);
        if let (Some(fp), Some(tp)) = (from_pos, to_pos) {
            draw_noodle(&painter, fp, tp, egui::Color32::from_gray(180), 2.0);
        }
    }

    // Draw pending connection noodle (from source socket to cursor)
    if let Some((from_nid, from_out, is_output)) = drag_state.connecting_from {
        let source_pos = if is_output {
            node_output_pos(graph, from_nid, from_out, canvas_origin)
        } else {
            node_input_pos(graph, from_nid, from_out, canvas_origin)
        };
        if let Some(sp) = source_pos {
            if let Some(mouse) = mouse_pos {
                draw_noodle(&painter, sp, mouse, egui::Color32::from_rgb(255, 200, 50), 2.5);
            }
        }
    }

    // Collect interaction data before mutable graph access
    struct NodeInteraction {
        nid: NodeId,
        node_rect: egui::Rect,
        header_rect: egui::Rect,
        output_dots: Vec<egui::Pos2>,
        input_dots: Vec<egui::Pos2>,
    }
    let mut interactions: Vec<NodeInteraction> = Vec::new();

    // Draw each node — unconnected inputs get live DragValue / color fields (wire OR type).
    let mut node_ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
    // G8: Frames paint before (= beneath) every other node — stable partition.
    node_ids.sort_by_key(|id| {
        graph
            .find_node(*id)
            .map(|n| !matches!(n.node_type, NodeType::Frame))
            .unwrap_or(true)
    });
    for nid in &node_ids {
        // Pad defaults so every socket has an editable value.
        if let Some(n) = graph.find_node_mut(*nid) {
            n.pad_input_defaults();
        }
        let (node_type, label, node_xy, has_const_value, const_f, frame_size, projection, noise_type) = {
            let Some(node) = graph.find_node(*nid) else { continue };
            let proj = if node_has_builtin_projection(&node.node_type) {
                Some(node.projection)
            } else {
                None
            };
            let ntype = if matches!(node.node_type, NodeType::Noise) {
                Some(node.noise_type)
            } else {
                None
            };
            let display_label = match node.node_type {
                NodeType::MathBinary => format!("Value {}", node.math_binary_op.label()),
                NodeType::MathUnary => node.math_unary_op.label().to_string(),
                NodeType::MixRgb if node.mix_blend != MixBlend::Mix => {
                    node.mix_blend.label().to_string()
                }
                _ => node.label.clone(),
            };
            (
                node.node_type.clone(),
                display_label,
                [node.pos[0], node.pos[1]],
                matches!(node.node_type, NodeType::ConstantValue),
                node.const_float,
                node.frame_size,
                proj,
                ntype,
            )
        };
        let node_pos = egui::pos2(canvas_origin.x + node_xy[0], canvas_origin.y + node_xy[1]);

        let inputs = Node::input_defs(&node_type);
        let outputs = Node::output_defs(&node_type);
        // Vec3 rows need a bit more height for three mini fields.
        let row_h = NODE_ROW_H;
        let extra = if has_const_value { 1 } else { 0 };
        // On-node param rows (dropdowns + per-side proj transforms).
        let (param_rows, is_collapsed) = {
            let Some(node) = graph.find_node(*nid) else { continue };
            // G6 `H` collapse: hide the PARAM rows (what makes nodes tall) but keep socket
            // rows drawn and connectable. Wire endpoints (`node_input_pos`/`node_output_pos`)
            // depend only on socket-row index, so collapsing never detaches a noodle — and
            // `param_rows == 0` is the existing single gate for the whole param-widget block.
            if node.collapsed {
                (0, true)
            } else {
                (node_param_row_count_for(node), false)
            }
        };
        // Master Material: +1 visual row for "── Advanced ──" separator.
        let master_sep = if matches!(node_type, NodeType::MaterialOutput) {
            1
        } else {
            0
        };
        let row_count = inputs.len().max(outputs.len()).max(1) + extra + param_rows + master_sep;
        // Extra strip under header for baked-in projection badge.
        let header_h = if projection.is_some() || noise_type.is_some() {
            32.0
        } else {
            20.0
        };
        let node_h = header_h + 4.0 + row_count as f32 * row_h;
        // Master Material is wider — long Advanced labels (Clearcoat Rough, Emission Intensity).
        let node_w = if matches!(node_type, NodeType::MaterialOutput) {
            NODE_BODY_W + 36.0
        } else {
            NODE_BODY_W
        };
        // G8 Frame: the box IS the node — size from `frame_size` (legacy [0,0] → default).
        let is_frame = matches!(node_type, NodeType::Frame);
        let (node_w, node_h) = if is_frame {
            (frame_size[0].max(260.0), frame_size[1].max(160.0))
        } else {
            (node_w, node_h)
        };

        let node_rect = egui::Rect::from_min_size(node_pos, egui::vec2(node_w, node_h));
        // Box select: a press that lands on a node is a click/drag, not a box.
        // Frames: only the HEADER claims the pointer — a body press starts a marquee
        // over the frame's children (Blender behavior); and frames stay out of
        // `box_hits` so the marquee never selects the box itself.
        if let Some(from) = drag_state.box_select_from {
            let claim = if is_frame {
                egui::Rect::from_min_size(node_pos, egui::vec2(node_w, 20.0))
            } else {
                node_rect
            };
            if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
                && claim.contains(from)
            {
                drag_state.box_select_from = None;
            }
        }
        if !is_frame {
            box_hits.push((*nid, node_rect));
        }

        // Node background + selection highlight
        let is_selected =
            drag_state.selected.contains(nid) || drag_state.selected_node == Some(*nid);
        let is_muted = graph.find_node(*nid).map(|n| n.muted).unwrap_or(false);
        if is_selected {
            painter.rect_stroke(node_rect.expand(2.0), 5.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 160, 255)), egui::StrokeKind::Middle);
        }
        if is_frame {
            painter.rect_filled(
                node_rect,
                6.0,
                egui::Color32::from_rgba_unmultiplied(70, 70, 84, 60),
            );
            painter.rect_stroke(
                node_rect,
                6.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(140, 140, 165, 120)), egui::StrokeKind::Middle,
            );
            // Bottom-right resize grip (canvas units 1:1 with screen — no zoom).
            let grip = egui::Rect::from_min_max(
                node_rect.max - egui::vec2(14.0, 14.0),
                node_rect.max,
            );
            painter.rect_filled(grip, 2.0, egui::Color32::from_rgba_unmultiplied(150, 150, 175, 140));
            let gresp = ui.interact(
                grip,
                ui.id().with(("frame_grip", *nid)),
                egui::Sense::drag(),
            );
            if gresp.dragged() {
                let d = gresp.drag_delta();
                if let Some(n) = graph.find_node_mut(*nid) {
                    // Same no-`changed` contract as node moves: layout persists with
                    // the next real commit, never triggers a rebake.
                    n.frame_size = [
                        (node_w + d.x).max(120.0),
                        (node_h + d.y).max(60.0),
                    ];
                }
                drag_state.field_interact = true;
            }
        } else {
            painter.rect_filled(node_rect, 4.0, egui::Color32::from_gray(45));
        }
        if is_muted {
            // Blender mute look: dimmed body + badge. The REAL bypass lives in the evaluator
            // (eval_node's mute arm) — this is only the honest indication of it.
            painter.rect_filled(
                node_rect,
                4.0,
                egui::Color32::from_rgba_unmultiplied(20, 20, 20, 140),
            );
            painter.text(
                egui::pos2(node_rect.right() - 10.0, node_rect.top() + 10.0),
                egui::Align2::CENTER_CENTER,
                "M",
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(255, 180, 80),
            );
        }
        if is_collapsed {
            // Collapsed marker (H toggles back) — params hidden, sockets still live.
            painter.text(
                egui::pos2(node_rect.left() + 8.0, node_rect.top() + 10.0),
                egui::Align2::CENTER_CENTER,
                "\u{25b8}",
                egui::FontId::proportional(11.0),
                egui::Color32::from_gray(170),
            );
        }

        // Header — generators show baked-in projection (default Triplanar) so users don't
        // think they must plug a separate Projection node.
        let header_color = node_header_color(&node_type);
        let header_rect = egui::Rect::from_min_size(node_pos, egui::vec2(node_w, header_h));
        painter.rect_filled(header_rect, egui::Rounding{ nw: 4, ne: 4, sw: 0, se: 0 }, header_color);
        if let Some(ps) = projection {
            painter.text(
                egui::pos2(header_rect.center().x, node_pos.y + 10.0),
                egui::Align2::CENTER_CENTER,
                &label,
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
            let badge = if let Some(nt) = noise_type {
                format!("{} · ⊞ {}", nt.short(), ps.short())
            } else {
                format!("⊞ {}", ps.short())
            };
            painter.text(
                egui::pos2(header_rect.center().x, node_pos.y + 22.0),
                egui::Align2::CENTER_CENTER,
                badge,
                egui::FontId::proportional(9.0),
                egui::Color32::from_rgb(200, 230, 255),
            );
        } else {
            painter.text(
                header_rect.center(), egui::Align2::CENTER_CENTER,
                &label, egui::FontId::proportional(11.0), egui::Color32::WHITE,
            );
        }
        let body_top = node_pos.y + header_h + 4.0;

        let mut output_dots = Vec::new();
        let mut input_dots = Vec::new();

        // Draw input sockets + interactive defaults when unconnected
        for (i, inp) in inputs.iter().enumerate() {
            // Master Material: insert Advanced section separator after base sockets.
            if matches!(node_type, NodeType::MaterialOutput) && i == NodeGraph::MO_ADVANCED_START {
                let sep_y = body_top + (NodeGraph::MO_BASE_COUNT as f32) * row_h + row_h * 0.5;
                painter.line_segment(
                    [
                        egui::pos2(node_pos.x + 10.0, sep_y),
                        egui::pos2(node_pos.x + node_w - 10.0, sep_y),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 70, 70)),
                );
                painter.text(
                    egui::pos2(node_pos.x + node_w * 0.5, sep_y),
                    egui::Align2::CENTER_CENTER,
                    "▸ Advanced",
                    egui::FontId::proportional(10.0),
                    egui::Color32::from_rgb(220, 160, 160),
                );
            }
            let row = if matches!(node_type, NodeType::MaterialOutput) {
                master_input_row(i, true)
            } else {
                i
            };
            let y = body_top + row as f32 * row_h + row_h * 0.5;
            let dot = egui::pos2(node_pos.x, y);
            input_dots.push(dot);
            let connected = graph.connections.iter().any(|c| c.to_node == *nid && c.to_input == i);
            let col = socket_color(&inp.socket_type);
            let hover_glow = if drag_state.connecting_from.is_some() {
                if let Some(mp) = mouse_pos {
                    (mp.x - dot.x).abs() < 12.0 && (mp.y - dot.y).abs() < 12.0
                } else { false }
            } else { false };
            let radius = if hover_glow { 8.0 } else { 6.0 };
            painter.circle_filled(dot, radius, if connected || hover_glow { col } else { egui::Color32::from_gray(60) });
            painter.circle_stroke(dot, radius, egui::Stroke::new(1.5, col));

            // Name — Advanced sockets slightly dimmer so Base reads first.
            let name_col = if matches!(node_type, NodeType::MaterialOutput)
                && i >= NodeGraph::MO_ADVANCED_START
            {
                egui::Color32::from_rgb(180, 160, 160)
            } else {
                egui::Color32::from_gray(200)
            };
            painter.text(
                egui::pos2(node_pos.x + 12.0, y), egui::Align2::LEFT_CENTER,
                inp.name, egui::FontId::proportional(10.0), name_col,
            );

            // Unconnected → editable value (click-drag / type). Connected → wire drives it.
            if !connected {
                // Keep clear of socket hit zone (left) and output sockets (right).
                let field_x = node_pos.x + if matches!(node_type, NodeType::MaterialOutput) {
                    108.0
                } else {
                    72.0
                };
                let field_w = if matches!(inp.socket_type, SocketType::Vec3) {
                    114.0
                } else if matches!(node_type, NodeType::MaterialOutput) {
                    100.0
                } else {
                    90.0
                };
                let field_rect = egui::Rect::from_min_size(
                    egui::pos2(field_x, y - 9.0),
                    egui::vec2(field_w, 18.0),
                );
                if canvas_rect.intersects(field_rect) {
                    let mut field_changed = false;
                    let mut select_me = false;
                    if let Some(node) = graph.find_node_mut(*nid) {
                        if i < node.input_defaults.len() {
                            match inp.socket_type {
                                SocketType::Float => {
                                    // No hard ranges — type any value (Scale 100, Octaves 20, etc.).
                                    // Drag speed only; clamps live in eval for div0 / runaway cost.
                                    let speed = if matches!(node_type, NodeType::Noise) {
                                        match i {
                                            1 => 0.1,  // Scale
                                            2 => 0.25, // Octaves
                                            3 => 0.02, // Lacunarity
                                            4 => 0.01, // Gain
                                            5 => 0.01, // Distortion
                                            6 => 0.1,  // Seed
                                            _ => 0.01,
                                        }
                                    } else {
                                        0.01
                                    };
                                    // Unique id per (node, socket) — without this, DragValues
                                    // collide and click-drag edit breaks.
                                    let resp = ui
                                        .push_id(("ndv", *nid, i as u32), |ui| {
                                            nui.put(
                                                field_rect,
                                                egui::DragValue::new(
                                                    &mut node.input_defaults[i].value_f32,
                                                )
                                                .speed(speed)
                                                .min_decimals(0)
                                                .max_decimals(4),
                                            )
                                        })
                                        .inner;
                                    note_field_interact(drag_state, &resp);
                                    if resp.changed() {
                                        // Octaves: keep integer-ish only (no max — user types what they want).
                                        if matches!(node_type, NodeType::Noise) && i == 2 {
                                            node.input_defaults[i].value_f32 =
                                                node.input_defaults[i].value_f32.round();
                                        }
                                        field_changed = true;
                                    }
                                    if resp.clicked() || resp.dragged() || resp.has_focus() {
                                        select_me = true;
                                    }
                                }
                                SocketType::Color => {
                                    let open = drag_state.color_popup.as_ref().is_some_and(|p| {
                                        p.target
                                            == ColorPopupTarget::Input {
                                                node: *nid,
                                                socket: i,
                                            }
                                    });
                                    let r = ui
                                        .push_id(("ncol", *nid, i as u32), |ui| {
                                            nui.allocate_ui_at_rect(field_rect, |ui| {
                                                ui.spacing_mut().item_spacing.x = 2.0;
                                                color_swatch_button(
                                                    ui,
                                                    &node.input_defaults[i].value_vec3,
                                                    open,
                                                )
                                            })
                                            .inner
                                        })
                                        .inner;
                                    note_field_interact(drag_state, &r);
                                    if r.clicked() {
                                        select_me = true;
                                        open_color_popup(
                                            drag_state,
                                            ColorPopupTarget::Input {
                                                node: *nid,
                                                socket: i,
                                            },
                                            r.rect,
                                        );
                                    }
                                }
                                SocketType::Vec3 => {
                                    ui.push_id(("nv3", *nid, i as u32), |ui| {
                                        nui.allocate_ui_at_rect(field_rect, |ui| {
                                            ui.spacing_mut().item_spacing.x = 1.0;
                                            ui.horizontal(|ui| {
                                                for axis in 0..3 {
                                                    let r = ui
                                                        .push_id(axis, |ui| {
                                                            ui.add_sized(
                                                                [36.0, 16.0],
                                                                egui::DragValue::new(
                                                                    &mut node.input_defaults[i]
                                                                        .value_vec3[axis],
                                                                )
                                                                .speed(0.01)
                                                                .min_decimals(1)
                                                                .max_decimals(3)
                                                                .prefix(match axis {
                                                                    0 => "x",
                                                                    1 => "y",
                                                                    _ => "z",
                                                                }),
                                                            )
                                                        })
                                                        .inner;
                                                    note_field_interact(drag_state, &r);
                                                    if r.changed() {
                                                        field_changed = true;
                                                    }
                                                    if r.clicked() || r.dragged() {
                                                        select_me = true;
                                                    }
                                                }
                                            });
                                        });
                                    });
                                }
                            }
                            let reset_rect = egui::Rect::from_min_size(
                                egui::pos2(field_rect.right() + 3.0, field_rect.top()),
                                egui::vec2(18.0, 18.0),
                            );
                            let reset = nui.put(reset_rect, egui::Button::new("↺"))
                                .on_hover_text(format!("Reset {} to default", inp.name));
                            note_field_interact(drag_state, &reset);
                            if reset.clicked() {
                                node.input_defaults[i].value_f32 = inp.default_f32;
                                node.input_defaults[i].value_vec3 = inp.default_vec3;
                                field_changed = true;
                                select_me = true;
                            }
                        }
                    }
                    if select_me {
                        drag_state.select_single(*nid);
                    }
                    if field_changed {
                        graph.wgsl_cache = None;
                        changed = true;
                    }
                }
            }
        }

        // ConstantValue: single float on the node body (no sockets).
        if has_const_value {
            let y = body_top + row_h * 0.5;
            let field_rect = egui::Rect::from_min_size(
                egui::pos2(node_pos.x + 12.0, y - 9.0),
                egui::vec2(node_w - 24.0, 18.0),
            );
            if let Some(node) = graph.find_node_mut(*nid) {
                let resp = ui
                    .push_id(("nconst", *nid), |ui| {
                        nui.put(
                            field_rect,
                            egui::DragValue::new(&mut node.const_float)
                                .speed(0.01)
                                .prefix("Value ")
                                .min_decimals(2)
                                .max_decimals(4),
                        )
                    })
                    .inner;
                note_field_interact(drag_state, &resp);
                if resp.changed() {
                    graph.wgsl_cache = None;
                    changed = true;
                }
                if resp.clicked() || resp.dragged() {
                    drag_state.select_single(*nid);
                }
            }
            let _ = const_f;
        }

        // On-node dropdowns (Type / Projection / Math op) — live under the sockets.
        let sock_rows = inputs.len().max(outputs.len()).max(1) + if has_const_value { 1 } else { 0 };
        let param_y0 = body_top + sock_rows as f32 * row_h;
        if param_rows > 0 {
            if let Some(node) = graph.find_node_mut(*nid) {
                let mut prow = 0usize;
                if matches!(node.node_type, NodeType::Noise) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Type").small());
                            egui::ComboBox::from_id_salt(("ntype", node.id))
                                .selected_text(node.noise_type.label())
                                .width(90.0)
                                .show_ui(ui, |ui| {
                                    for nt in NoiseType::all() {
                                        if ui
                                            .selectable_value(
                                                &mut node.noise_type,
                                                *nt,
                                                nt.label(),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                            // Seamless map edges under Repeat sampling.
                            if ui
                                .checkbox(&mut node.noise_tile, "Tile")
                                .on_hover_text(
                                    "Periodic noise so the baked map tiles without edge seams. \
                                     Scale ≈ number of cells across the tile.",
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });
                    });
                    prow += 1;
                    // Advanced: blend curve / levels reshape of the 0..1 noise output.
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Curve")
                                    .small()
                                    .color(egui::Color32::from_rgb(180, 200, 160)),
                            );
                            egui::ComboBox::from_id_salt(("ncurve", node.id))
                                .selected_text(node.noise_curve.label())
                                .width(88.0)
                                .show_ui(ui, |ui| {
                                    for c in NoiseBlendCurve::all() {
                                        if ui
                                            .selectable_value(
                                                &mut node.noise_curve,
                                                *c,
                                                c.label(),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Reshape the noise 0–1 value before output:\n\
                                     • Linear — raw (optional black/white)\n\
                                     • Smooth / Smoother — soft falloff\n\
                                     • Power (K) — contrast gamma\n\
                                     • Levels — black / mid / white like photo levels",
                                );
                        });
                    });
                    prow += 1;
                    // Black / White points (levels-style input range)
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 6.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 12.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Blk").small());
                            let r0 = ui.add_sized(
                                [36.0, 16.0],
                                egui::DragValue::new(&mut node.noise_black)
                                    .speed(0.01)
                                    .min_decimals(2),
                            );
                            note_field_interact(drag_state, &r0);
                            if r0.changed() {
                                changed = true;
                            }
                            ui.label(egui::RichText::new("Wht").small());
                            let r1 = ui.add_sized(
                                [36.0, 16.0],
                                egui::DragValue::new(&mut node.noise_white)
                                    .speed(0.01)
                                    .min_decimals(2),
                            );
                            note_field_interact(drag_state, &r1);
                            if r1.changed() {
                                changed = true;
                            }
                        });
                    });
                    prow += 1;
                    // K / Mid — shown for Power and Levels (always editable; ignored for Linear/Smooth)
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 6.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 12.0, row_h - 2.0),
                    );
                    let k_label = match node.noise_curve {
                        NoiseBlendCurve::Levels => "Mid",
                        NoiseBlendCurve::Power => "K",
                        _ => "K/Mid",
                    };
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(k_label)
                                    .small()
                                    .color(egui::Color32::from_rgb(180, 200, 160)),
                            );
                            let r0 = ui.add_sized(
                                [48.0, 16.0],
                                egui::DragValue::new(&mut node.noise_curve_k)
                                    .speed(0.02)
                                    .min_decimals(2),
                            );
                            note_field_interact(drag_state, &r0);
                            if r0.changed() {
                                changed = true;
                            }
                            ui.label(
                                egui::RichText::new(match node.noise_curve {
                                    NoiseBlendCurve::Levels => "γ mid",
                                    NoiseBlendCurve::Power => "pow",
                                    NoiseBlendCurve::Smooth | NoiseBlendCurve::Smoother => "—",
                                    NoiseBlendCurve::Linear => "—",
                                })
                                .small()
                                .weak(),
                            );
                        });
                    });
                    prow += 1;
                }
                if node_has_builtin_projection(&node.node_type) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Proj").small());
                            egui::ComboBox::from_id_salt(("nproj", node.id))
                                .selected_text(node.projection.label())
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for ps in ProjectionStyle::all() {
                                        if ui
                                            .selectable_value(
                                                &mut node.projection,
                                                *ps,
                                                ps.label(),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                    });
                    prow += 1;
                    // Per-side Position (U/V offset) + Rotation (degrees).
                    let sides: &[(&str, usize)] = match node.projection {
                        ProjectionStyle::None => &[],
                        ProjectionStyle::Triplanar | ProjectionStyle::Box => {
                            &[("X", 0), ("Y", 1), ("Z", 2)]
                        }
                        _ => &[("UV", 2)], // single plane uses Z-side fields
                    };
                    for (label, side) in sides {
                        let r = egui::Rect::from_min_size(
                            egui::pos2(node_pos.x + 6.0, param_y0 + prow as f32 * row_h + 1.0),
                            egui::vec2(node_w - 12.0, row_h - 2.0),
                        );
                        let (off, rot) = match side {
                            0 => (&mut node.proj_off_x, &mut node.proj_rot_x),
                            1 => (&mut node.proj_off_y, &mut node.proj_rot_y),
                            _ => (&mut node.proj_off_z, &mut node.proj_rot_z),
                        };
                        ui.push_id(("nproj_side", node.id, *side as u32), |ui| {
                            nui.allocate_ui_at_rect(r, |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("{label}"))
                                            .small()
                                            .color(egui::Color32::from_rgb(160, 200, 220)),
                                    );
                                    let r0 = ui.add_sized(
                                        [28.0, 16.0],
                                        egui::DragValue::new(&mut off[0])
                                            .speed(0.01)
                                            .prefix("U")
                                            .min_decimals(2),
                                    );
                                    note_field_interact(drag_state, &r0);
                                    if r0.changed() {
                                        changed = true;
                                    }
                                    let r1 = ui.add_sized(
                                        [28.0, 16.0],
                                        egui::DragValue::new(&mut off[1])
                                            .speed(0.01)
                                            .prefix("V")
                                            .min_decimals(2),
                                    );
                                    note_field_interact(drag_state, &r1);
                                    if r1.changed() {
                                        changed = true;
                                    }
                                    let r2 = ui.add_sized(
                                        [36.0, 16.0],
                                        egui::DragValue::new(rot)
                                            .speed(1.0)
                                            .prefix("°")
                                            .range(-360.0..=360.0)
                                            .min_decimals(0)
                                            .max_decimals(1),
                                    );
                                    note_field_interact(drag_state, &r2);
                                    if r2.changed() {
                                        changed = true;
                                    }
                                });
                            });
                        });
                        prow += 1;
                    }
                }
                if matches!(node.node_type, NodeType::MathBinary) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Op").small());
                            egui::ComboBox::from_id_salt(("mbop", node.id))
                                .selected_text(node.math_binary_op.label())
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for op in MathBinaryOp::all() {
                                        if ui
                                            .selectable_value(
                                                &mut node.math_binary_op,
                                                *op,
                                                op.label(),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                    });
                    prow += 1;
                }
                if matches!(node.node_type, NodeType::MathUnary) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Op").small());
                            egui::ComboBox::from_id_salt(("muop", node.id))
                                .selected_text(node.math_unary_op.label())
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for op in MathUnaryOp::all() {
                                        if ui
                                            .selectable_value(
                                                &mut node.math_unary_op,
                                                *op,
                                                op.label(),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                    });
                    prow += 1;
                }
                if matches!(node.node_type, NodeType::VectorMath) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Op").small());
                            egui::ComboBox::from_id_salt(("vmop", node.id))
                                .selected_text(node.vector_math_op.label())
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for op in VectorMathOp::all() {
                                        if ui
                                            .selectable_value(
                                                &mut node.vector_math_op,
                                                *op,
                                                op.label(),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                    });
                    prow += 1;
                }
                if matches!(node.node_type, NodeType::MixRgb) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Blend").small());
                            egui::ComboBox::from_id_salt(("mixblend", node.id))
                                .selected_text(node.mix_blend.label())
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for m in MixBlend::all() {
                                        if ui
                                            .selectable_value(&mut node.mix_blend, *m, m.label())
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                    });
                    prow += 1;
                }
                // ColorRamp stops live on the node (no bottom inspector).
                if matches!(node.node_type, NodeType::ColorRamp) {
                    for stop in 0..2 {
                        let r = egui::Rect::from_min_size(
                            egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                            egui::vec2(node_w - 16.0, row_h - 2.0),
                        );
                        nui.allocate_ui_at_rect(r, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(if stop == 0 { "A" } else { "B" }).small());
                                let col = if stop == 0 {
                                    &node.ramp_color0
                                } else {
                                    &node.ramp_color1
                                };
                                let tgt = if stop == 0 {
                                    ColorPopupTarget::Ramp0 { node: node.id }
                                } else {
                                    ColorPopupTarget::Ramp1 { node: node.id }
                                };
                                let open = drag_state
                                    .color_popup
                                    .as_ref()
                                    .is_some_and(|p| p.target == tgt);
                                let r = color_swatch_button(ui, col, open);
                                note_field_interact(drag_state, &r);
                                if r.clicked() {
                                    open_color_popup(drag_state, tgt, r.rect);
                                }
                                let pos = if stop == 0 {
                                    &mut node.ramp_pos0
                                } else {
                                    &mut node.ramp_pos1
                                };
                                if ui
                                    .add(
                                        egui::DragValue::new(pos)
                                            .speed(0.01)
                                            .range(0.0..=1.0)
                                            .prefix("t "),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            });
                        });
                        prow += 1;
                    }
                }
                if matches!(node.node_type, NodeType::ConstantColor) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 8.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 16.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Color").small());
                            let tgt = ColorPopupTarget::Ramp0 { node: node.id };
                            let open = drag_state
                                .color_popup
                                .as_ref()
                                .is_some_and(|p| p.target == tgt);
                            let cr = color_swatch_button(ui, &node.ramp_color0, open);
                            note_field_interact(drag_state, &cr);
                            if cr.clicked() {
                                open_color_popup(drag_state, tgt, cr.rect);
                            }
                        });
                    });
                    prow += 1;
                }
                if matches!(
                    node.node_type,
                    NodeType::PrimSdfMask | NodeType::SdfCurvature
                ) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 6.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 12.0, row_h - 2.0),
                    );
                    let current = node.sdf_prim_uid;
                    let label = prim_choices
                        .iter()
                        .find(|(u, _)| *u == current)
                        .map(|(_, n)| n.as_str())
                        .unwrap_or(if current == 0 {
                            "Body field"
                        } else {
                            "Prim…"
                        });
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Prim")
                                    .small()
                                    .color(egui::Color32::from_rgb(160, 200, 180)),
                            );
                            egui::ComboBox::from_id_salt(("sdf_prim", node.id))
                                .selected_text(label)
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(current == 0, "Body field (all)")
                                        .clicked()
                                    {
                                        node.sdf_prim_uid = 0;
                                        changed = true;
                                    }
                                    for (uid, name) in prim_choices {
                                        if ui
                                            .selectable_label(
                                                current == *uid,
                                                format!("{name} ({uid})"),
                                            )
                                            .clicked()
                                        {
                                            node.sdf_prim_uid = *uid;
                                            changed = true;
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Primitive whose SDF drives this node. \
                                     Body field = whole body boolean tree. \
                                     Pick a prim for localized mask / curvature.",
                                );
                        });
                    });
                    prow += 1;
                }
                if matches!(node.node_type, NodeType::SdfCurvature) {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 6.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 12.0, row_h - 2.0),
                    );
                    let regions = ["Both", "Convex (edges)", "Concave (cavity)"];
                    let ri = (node.sdf_curv_region as usize).min(2);
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Region").small())
                                .on_hover_text(
                                    "3D surface mean-curvature (not silhouette).\n\
                                     Convex = sharp edges / line art / edge wear.\n\
                                     Concave = cavities / dirt traps.\n\
                                     Bake maps for full quality (CPU projects UV → surface).",
                                );
                            egui::ComboBox::from_id_salt(("sdf_reg", node.id))
                                .selected_text(regions[ri])
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    for (i, lab) in regions.iter().enumerate() {
                                        if ui
                                            .selectable_value(
                                                &mut node.sdf_curv_region,
                                                i as u32,
                                                *lab,
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                    });
                    prow += 1;
                }
                if matches!(node.node_type, NodeType::ImageTexture) {
                    use core_math::pbr_material::MapKind;
                    let r = egui::Rect::from_min_size(
                        egui::pos2(node_pos.x + 6.0, param_y0 + prow as f32 * row_h + 1.0),
                        egui::vec2(node_w - 12.0, row_h - 2.0),
                    );
                    nui.allocate_ui_at_rect(r, |ui| {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("img_chan", node.id))
                                .selected_text(
                                    MapKind::from_wire_tag(node.image_map_kind)
                                        .map(|k| k.label())
                                        .unwrap_or("Albedo"),
                                )
                                .width(72.0)
                                .show_ui(ui, |ui| {
                                    for kind in [
                                        MapKind::Albedo,
                                        MapKind::Roughness,
                                        MapKind::Metallic,
                                        MapKind::Normal,
                                    ] {
                                        if ui
                                            .selectable_value(
                                                &mut node.image_map_kind,
                                                kind.wire_tag(),
                                                kind.label(),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                            if ui.small_button("Load…").clicked() {
                                outcome.load_map_kind = Some(node.image_map_kind);
                            }
                        });
                    });
                    let _ = prow;
                }
                if changed {
                    graph.wgsl_cache = None;
                }
            }
            // Master Material: map bake resolution lives ON the node (not the panel toolbar).
            if matches!(node_type, NodeType::MaterialOutput) {
                // Place below last Advanced socket (separator shifts row indices).
                let last_sock_row = if inputs.is_empty() {
                    0
                } else {
        (0..inputs.len()).map(|input| master_input_row(input, true)).max().unwrap()
                };
                let mo_param_y = body_top + (last_sock_row + 1) as f32 * row_h;
                let r = egui::Rect::from_min_size(
                    egui::pos2(node_pos.x + 8.0, mo_param_y + 1.0),
                    egui::vec2(node_w - 16.0, row_h - 2.0),
                );
                let mut res = graph.bake_res;
                nui.allocate_ui_at_rect(r, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Map res").small());
                        egui::ComboBox::from_id_salt(("mo_bake_res", *nid))
                            .selected_text(format!("{res}²"))
                            .width(72.0)
                            .show_ui(ui, |ui| {
                                for r in [128u32, 256, 512, 1024, 2048, 4096] {
                                    if ui
                                        .selectable_value(&mut res, r, format!("{r}²"))
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                }
                            })
                            .response
                            .on_hover_text(
                                "Map bake TARGET resolution (same as Material panel → Bake N²).\n\
                                 128/256 = fast · 512 = balanced · 1024 = full quality.\n\
                                 2048/4096 = high-res final (grows the material texture array).\n\
                                 Higher = more detail, slower while baking.",
                            );
                    });
                });
                if res != graph.bake_res {
                    graph.bake_res = res;
                    graph.wgsl_cache = None;
                    changed = true;
                }
            }
        }

        if node_has_noise_preview(&node_type) {
            let control_rows = param_rows.saturating_sub(NOISE_PREVIEW_ROWS);
            let preview_size = NOISE_PREVIEW_DRAW_SIZE;
            let preview_rect = egui::Rect::from_min_size(
                egui::pos2(
                    node_pos.x + (node_w - preview_size) * 0.5,
                    param_y0 + control_rows as f32 * row_h + 8.0,
                ),
                egui::vec2(preview_size, preview_size),
            );
            draw_noise_preview(
                ui,
                &painter,
                graph,
                *nid,
                preview_rect,
                noise_preview_fingerprint,
                drag_state,
                &mut noise_previews_rendered,
            );
        }

        // Draw output sockets
        for (i, out) in outputs.iter().enumerate() {
            let y = body_top + i as f32 * row_h + row_h * 0.5;
            let dot = egui::pos2(node_pos.x + node_w, y);
            output_dots.push(dot);
            let connected = graph.connections.iter().any(|c| c.from_node == *nid && c.from_output == i);
            let col = socket_color(&out.socket_type);
            let hover_glow = if drag_state.connecting_from.is_some() {
                if let Some(mp) = mouse_pos {
                    (mp.x - dot.x).abs() < 12.0 && (mp.y - dot.y).abs() < 12.0
                } else { false }
            } else { false };
            let radius = if hover_glow { 8.0 } else { 6.0 };
            painter.circle_filled(dot, radius, if connected || hover_glow { col } else { egui::Color32::from_gray(60) });
            painter.circle_stroke(dot, radius, egui::Stroke::new(1.5, col));
            painter.text(
                egui::pos2(node_pos.x + node_w - 12.0, y), egui::Align2::RIGHT_CENTER,
                out.name, egui::FontId::proportional(10.0), egui::Color32::from_gray(200),
            );
        }

        interactions.push(NodeInteraction {
            nid: *nid,
            node_rect,
            header_rect,
            output_dots,
            input_dots,
        });
    }

    // --- Interaction pass (after drawing, using ui.interact) ---
    let lmb_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
    let lmb_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
    let alt_held = ui.input(|i| i.modifiers.alt);
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let drag_delta = ui.input(|i| i.pointer.delta());

    if lmb_pressed {
        let pointer_in_canvas =
            mouse_pos.is_some_and(|pointer_pos| canvas_rect.contains(pointer_pos));
        drag_state.keyboard_active = pointer_in_canvas && !drag_state.field_interact;
        if drag_state.keyboard_active {
            if let Some(focused) = ui.ctx().memory(|memory| memory.focused()) {
                ui.ctx()
                    .memory_mut(|memory| memory.surrender_focus(focused));
            }
        }
    }

    // Process interactions in reverse order (top-most nodes first for z-order)
    for inter in interactions.iter().rev() {
        let nid = inter.nid;

        // Socket drag-to-connect: check if LMB pressed on a socket dot
        if lmb_pressed
            && !alt_held
            && !drag_state.field_interact
            && drag_state.connecting_from.is_none()
            && drag_state.dragging_node.is_none()
        {
            if let Some(mp) = mouse_pos {
                // Check output sockets
                for (i, dot) in inter.output_dots.iter().enumerate() {
                    if (mp.x - dot.x).abs() < 10.0 && (mp.y - dot.y).abs() < 10.0 {
                        drag_state.connecting_from = Some((nid, i, true));
                        break;
                    }
                }
                // Check input sockets
                if drag_state.connecting_from.is_none() {
                    for (i, dot) in inter.input_dots.iter().enumerate() {
                        if (mp.x - dot.x).abs() < 10.0 && (mp.y - dot.y).abs() < 10.0 {
                            drag_state.connecting_from = Some((nid, i, false));
                            break;
                        }
                    }
                }
            }
        }

        // RMB on socket: disconnect. Ctrl+RMB is the LINK CUT (Blender) — the cut
        // owns the button while Ctrl is held, so the disconnect requires bare RMB.
        if ui.input(|i| {
            i.pointer.button_pressed(egui::PointerButton::Secondary) && !i.modifiers.ctrl
        }) {
            if let Some(mp) = mouse_pos {
                for (i, dot) in inter.output_dots.iter().enumerate() {
                    if (mp.x - dot.x).abs() < 10.0 && (mp.y - dot.y).abs() < 10.0 {
                        if let Some(before) = disconnect_socket(graph, nid, i, true) {
                            atomic_graph_before_edit.get_or_insert(before);
                            changed = true;
                        }
                    }
                }
                for (i, dot) in inter.input_dots.iter().enumerate() {
                    if (mp.x - dot.x).abs() < 10.0 && (mp.y - dot.y).abs() < 10.0 {
                        if let Some(before) = disconnect_socket(graph, nid, i, false) {
                            atomic_graph_before_edit.get_or_insert(before);
                            changed = true;
                        }
                    }
                }
            }
        }

        // Header: LMB press = select + start drag (never when scrubbing a DragValue)
        if lmb_pressed
            && !alt_held
            && !drag_state.field_interact
            && drag_state.connecting_from.is_none()
            && drag_state.dragging_node.is_none()
        {
            if let Some(mp) = mouse_pos {
                if inter.header_rect.contains(mp) {
                    let shift = ui.input(|i| i.modifiers.shift);
                    if shift || ctrl_held {
                        drag_state.toggle_selected(nid);
                    } else if !drag_state.selected.contains(&nid) {
                        // Plain click on an UNSELECTED node: replace the set. Clicking a node
                        // already in the set keeps the set (so a multi-select can be dragged).
                        drag_state.select_single(nid);
                    }
                    if drag_state.selected.contains(&nid) { drag_state.selected_node = Some(nid); }
                    drag_state.dragging_node = Some(nid);
                    drag_state.drag_moved = false;
                }
            }
        }
    }

    // Continue node drag (blocked while a field owns the pointer)
    // G7: node dropped this frame — the post-pass (after node rects exist) checks
    // whether it landed on a link and splices it in.
    let mut dropped_node: Option<NodeId> = None;
    if let Some(drag_nid) = drag_state.dragging_node {
        if lmb_down && !alt_held && !drag_state.field_interact {
            if drag_delta != egui::Vec2::ZERO {
                drag_state.drag_moved = true;
            }
            // G8: dragging a FRAME header moves every node whose center its box covers
            // (graph-space containment, computed live so drop-in/drop-out just works).
            let frame_carry: Vec<NodeId> = graph
                .find_node(drag_nid)
                .filter(|n| matches!(n.node_type, NodeType::Frame))
                .map(|f| {
                    let w = f.frame_size[0].max(260.0);
                    let h = f.frame_size[1].max(160.0);
                    let (fx, fy) = (f.pos[0], f.pos[1]);
                    graph
                        .nodes
                        .iter()
                        .filter(|n| n.id != drag_nid)
                        .filter(|n| {
                            let cx = n.pos[0] + NODE_BODY_W * 0.5;
                            let cy = n.pos[1] + 20.0;
                            cx >= fx && cx <= fx + w && cy >= fy && cy <= fy + h
                        })
                        .map(|n| n.id)
                        .collect()
                })
                .unwrap_or_default();
            for id in frame_carry {
                if let Some(n) = graph.find_node_mut(id) {
                    n.pos[0] += drag_delta.x;
                    n.pos[1] += drag_delta.y;
                }
            }
            // G6: dragging any selected header moves the WHOLE selection (relative offsets
            // preserved by construction — one shared delta).
            if drag_state.selected.contains(&drag_nid) && drag_state.selected.len() > 1 {
                let sel: Vec<NodeId> = drag_state.selected.iter().copied().collect();
                for id in sel {
                    if let Some(n) = graph.find_node_mut(id) {
                        n.pos[0] += drag_delta.x;
                        n.pos[1] += drag_delta.y;
                    }
                }
            } else if let Some(n) = graph.find_node_mut(drag_nid) {
                n.pos[0] += drag_delta.x;
                n.pos[1] += drag_delta.y;
            }
        } else if !lmb_down {
            if drag_state.drag_moved && drag_state.selected.len() <= 1 {
                dropped_node = Some(drag_nid);
            }
            drag_state.dragging_node = None;
        }
    }

    // ── G7 DROP-ON-LINK INSERT: splice the dropped node into the first noodle its
    // body covers — link.from → node.in, node.out → link.to. Both ends must be
    // type-compatible BEFORE any mutation (no half-splice), and the node must not
    // already be an endpoint of that link.
    if let Some(nid) = dropped_node {
        if let Some((_, rect)) = box_hits.iter().find(|(id, _)| *id == nid) {
            let node_ty = graph.find_node(nid).map(|n| n.node_type.clone());
            if let Some(node_ty) = node_ty {
                let n_ins = Node::input_defs(&node_ty);
                let n_outs = Node::output_defs(&node_ty);
                if !n_ins.is_empty() && !n_outs.is_empty() {
                    let hit = graph.connections.iter().enumerate().find_map(|(ci, c)| {
                        if c.from_node == nid || c.to_node == nid {
                            return None;
                        }
                        let fp = node_output_pos(graph, c.from_node, c.from_output, canvas_origin)?;
                        let tp = node_input_pos(graph, c.to_node, c.to_input, canvas_origin)?;
                        if !noodle_crosses_rect(fp, tp, *rect) {
                            return None;
                        }
                        // First compatible pair: link-source type → a node input, and a
                        // node output → the link-target input type.
                        let src_ty = socket_type_of(graph, c.from_node, c.from_output, true)?;
                        let dst_ty = socket_type_of(graph, c.to_node, c.to_input, false)?;
                        let in_idx =
                            n_ins.iter().position(|d| src_ty.compatible(&d.socket_type))?;
                        let out_idx =
                            n_outs.iter().position(|d| d.socket_type.compatible(&dst_ty))?;
                        Some((ci, in_idx, out_idx))
                    });
                    if let Some((ci, in_idx, out_idx)) = hit {
                        let c = graph.connections[ci].clone();
                        // connect() replaces the wire on c.to_input, removing the old link.
                        if graph.connect(c.from_node, c.from_output, nid, in_idx) {
                            let _ = graph.connect(nid, out_idx, c.to_node, c.to_input);
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    // ── G6 box select: draw the marquee; resolve on release ─────────────────────────
    if let Some(r) = box_rect_now {
        painter.rect_filled(r, 0.0, egui::Color32::from_rgba_unmultiplied(100, 160, 255, 24));
        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(100, 160, 255)), egui::StrokeKind::Middle);
        // A live marquee suppresses the click-deselect that would otherwise fire on release.
        drag_state.pending_canvas_click = false;
    }
    if drag_state.box_select_from.is_some()
        && !ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary))
    {
        let from = drag_state.box_select_from.take().unwrap();
        if let Some(cur) = ui.input(|i| i.pointer.interact_pos()) {
            let r = egui::Rect::from_two_pos(from, cur);
            if r.width() > 4.0 || r.height() > 4.0 {
                let extend = ui.input(|i| i.modifiers.shift || i.modifiers.ctrl);
                if !extend {
                    drag_state.clear_selection();
                }
                for (id, rect) in &box_hits {
                    if r.intersects(*rect) {
                        drag_state.selected.insert(*id);
                        drag_state.selected_node = Some(*id);
                    }
                }
                drag_state.pending_canvas_click = false;
            }
        }
    }

    let clipboard_shortcuts = drag_state.keyboard_active && !drag_state.field_interact && !ui.ctx().wants_keyboard_input();
    if clipboard_shortcuts && ui.input(|i| {
        (i.modifiers.ctrl && i.key_pressed(egui::Key::C))
            || i.events.iter().any(|event| matches!(event, egui::Event::Copy))
    }) {
        if let Some(clipboard) = copy_selected_nodes(graph, &drag_state.selected) {
            drag_state.node_clipboard = Some(clipboard);
        }
    }
    if clipboard_shortcuts && ui.input(|i| {
        (i.modifiers.ctrl && i.key_pressed(egui::Key::X))
            || i.events.iter().any(|event| matches!(event, egui::Event::Cut))
    }) {
        if let Some(clipboard) = copy_selected_nodes(graph, &drag_state.selected) {
            atomic_graph_before_edit.get_or_insert_with(|| graph.clone());
            let cut_ids: Vec<NodeId> = clipboard.nodes.iter().map(|node| node.id).collect();
            drag_state.node_clipboard = Some(clipboard);
            for node_id in cut_ids {
                graph.remove_node(node_id);
            }
            drag_state.clear_selection();
            changed = true;
        }
    }
    if clipboard_shortcuts && ui.input(|i| {
        (i.modifiers.ctrl && i.key_pressed(egui::Key::V))
            || i.events.iter().any(|event| matches!(event, egui::Event::Paste(_)))
    }) {
        if let Some(clipboard) = drag_state.node_clipboard.as_mut() {
            atomic_graph_before_edit.get_or_insert_with(|| graph.clone());
            let target = mouse_pos
                .filter(|position| canvas_rect.contains(*position))
                .map(|position| {
                    [
                        position.x - canvas_origin.x,
                        position.y - canvas_origin.y,
                    ]
                })
                .unwrap_or([
                    canvas_rect.center().x - canvas_origin.x,
                    canvas_rect.center().y - canvas_origin.y,
                ]);
            let pasted = paste_node_clipboard(graph, clipboard, target);
            if !pasted.is_empty() {
                drag_state.selected_node = pasted.iter().copied().next();
                drag_state.selected = pasted;
                changed = true;
            }
        }
    }

    // Empty-canvas click deselect — only if we never hit a field this frame.
    // Also close sticky color picker when clicking empty canvas.
    if drag_state.pending_canvas_click && !drag_state.field_interact {
        if drag_state.connecting_from.is_some() {
            drag_state.connecting_from = None;
        } else if drag_state.dragging_node.is_none() {
            drag_state.clear_selection();
        }
        // Canvas bg click → dismiss color panel
        if drag_state.color_popup.is_some() && !drag_state.color_swatch_hit {
            drag_state.color_popup = None;
        }
    }
    drag_state.pending_canvas_click = false;

    // Sticky color picker (after nodes so it draws on top; stays open while dragging).
    if draw_sticky_color_popup(ui, graph, drag_state) {
        changed = true;
        graph.wgsl_cache = None;
    }

    // No bottom inspector — all inputs / Type / Projection / colors live on the node body.

    outcome.changed = changed;
    outcome.discrete = changed && (!drag_state.field_changed || atomic_graph_before_edit.is_some());
    outcome.atomic_change = outcome.discrete;
    if changed {
        outcome.graph_before_edit = atomic_graph_before_edit
            .or(drag_state.graph_before_pointer_edit.take())
            .or(graph_before_keyboard_edit);
    } else if ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
        drag_state.graph_before_pointer_edit = None;
    }
    drag_state
        .noise_preview_cache
        .retain(|node_id, _| graph.find_node(*node_id).is_some());
    outcome
}

fn disconnect_socket(
    graph: &mut NodeGraph,
    node_id: NodeId,
    socket_index: usize,
    is_output: bool,
) -> Option<NodeGraph> {
    let before = graph.clone();
    let changed = if is_output {
        graph.disconnect_output(node_id, socket_index)
    } else {
        graph.disconnect(node_id, socket_index)
    };
    changed.then_some(before)
}

/// Mark that an on-node control is hovered/dragged so canvas/node-move don't steal LMB.
fn note_field_interact(drag_state: &mut NodeDragState, resp: &egui::Response) {
    if resp.hovered() || resp.dragged() || resp.changed() {
        drag_state.field_interact = true;
    }
    // G7 scrub detector: only an actual VALUE change marks the frame as a scrub —
    // hover/focus must not suppress the undo entry of a concurrent discrete gesture.
    if resp.changed() {
        drag_state.field_changed = true;
    }
}

/// Which color field a sticky picker is editing.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorPopupTarget {
    /// `node.input_defaults[socket].value_vec3`
    Input { node: NodeId, socket: usize },
    /// ColorRamp stop A / ConstantColor
    Ramp0 { node: NodeId },
    /// ColorRamp stop B
    Ramp1 { node: NodeId },
}

/// Sticky color picker — stays open while dragging; closes only on outside click / Esc.
#[derive(Clone, Debug)]
pub struct ColorPopupState {
    pub target: ColorPopupTarget,
    /// Screen-space top-left of the popup (under the swatch).
    pub pos: [f32; 2],
}

/// Pending "release noodle on empty canvas → pick node" menu.
#[derive(Clone, Debug)]
pub struct LinkDropMenu {
    pub screen_pos: [f32; 2],
    pub place_pos: [f32; 2],
    pub from_nid: NodeId,
    pub from_idx: usize,
    pub from_is_output: bool,
    pub sock: SocketType,
    pub search: String,
}

/// Shift+A "Add Node" popup (full palette — not the primitive creation menu).
#[derive(Clone, Debug, Default)]
pub struct AddNodeMenu {
    pub screen_pos: [f32; 2],
    /// Graph-space place position (filled when the editor draws, if known).
    pub place_pos: [f32; 2],
    /// Live type-to-filter query (matches label + category).
    pub search: String,
}

#[derive(Default)]
pub struct NodeDragState {
    /// Pending connection: (node_id, socket_index, is_output_socket)
    pub connecting_from: Option<(NodeId, usize, bool)>,
    /// ACTIVE node (Blender's active concept — last clicked; badges, frame target).
    pub selected_node: Option<NodeId>,
    /// G6: the SELECTION SET (Blender model: active + selected). Every selection-set verb —
    /// box select, multi-move, Shift+D, X, M, H, G — operates on this. `selected_node` stays
    /// the active member; the two are kept coherent at every write site.
    pub selected: std::collections::HashSet<NodeId>,
    /// G6: box-select drag origin in SCREEN space (`None` = not box-selecting).
    pub box_select_from: Option<egui::Pos2>,
    /// G7: a node field's VALUE changed this frame (scrub) — suppresses the discrete
    /// undo signal. Narrower than `field_interact` (hover must not suppress).
    pub field_changed: bool,
    /// G7: Ctrl+RMB link-cut stroke, screen space. `Some` while the RMB is held.
    pub cut_stroke: Option<Vec<egui::Pos2>>,
    /// G7: the current header drag actually MOVED the node — a plain header click
    /// must never count as "dropped onto a link".
    pub drag_moved: bool,
    /// G6: Blender `G` grab — original positions of the selection, restored on Esc.
    pub grab_orig: Option<Vec<(NodeId, [f32; 2])>>,
    graph_before_grab: Option<NodeGraph>,
    /// The node canvas owns graph shortcuts until another UI surface is clicked.
    keyboard_active: bool,
    /// Node currently being dragged (set on header drag start)
    pub dragging_node: Option<NodeId>,
    /// Canvas pan offset (replaces ScrollArea for proper drag handling)
    pub canvas_offset: [f32; 2],
    /// Open after releasing a link on empty canvas — lists all compatible palette nodes.
    pub link_drop_menu: Option<LinkDropMenu>,
    /// Shift+A add-node palette while the shader editor is open.
    pub add_node_menu: Option<AddNodeMenu>,
    /// Search string for toolbar "+ Add Node" menu.
    pub toolbar_add_search: String,
    /// True this frame while pointer is on a DragValue / color / checkbox on a node body.
    /// Blocks node-move and empty-canvas deselect so click-drag scrubbing works (Blender-like).
    pub field_interact: bool,
    /// Canvas primary click pending; resolved after widgets set `field_interact`.
    pub pending_canvas_click: bool,
    /// Sticky color picker (replaces egui `color_edit_button` popup which closes on first drag).
    pub color_popup: Option<ColorPopupState>,
    /// Set when a color swatch was clicked this frame (prevents open→instant-close).
    pub color_swatch_hit: bool,
    /// Pending "frame this node" request (material panel → graph, P8). When set,
    /// the next `draw_node_editor` pan (`canvas_offset`) centers the node in the
    /// canvas and selects it, then clears the request. Reuses `canvas_offset` +
    /// `selected_node` — no separate zoom/camera system (there is no zoom).
    pub frame_request: Option<NodeId>,
    /// Pre-press graph retained until a pointer edit reports its first changed frame.
    pub graph_before_pointer_edit: Option<NodeGraph>,
    node_clipboard: Option<NodeClipboard>,
    noise_preview_cache: HashMap<NodeId, NoisePreviewTexture>,
    pub(crate) gpu_noise_preview_requests: HashMap<NodeId, u64>,
    pub(crate) gpu_noise_preview_ids: HashMap<NodeId, egui::TextureId>,
    pub(crate) gpu_noise_preview_pending: HashSet<NodeId>,
}

impl NodeDragState {
    pub(crate) fn owns_clipboard_shortcuts(&self) -> bool {
        self.keyboard_active
    }

    fn select_single(&mut self, node_id: NodeId) {
        self.selected.clear();
        self.selected.insert(node_id);
        self.selected_node = Some(node_id);
    }

    fn toggle_selected(&mut self, node_id: NodeId) {
        if !self.selected.remove(&node_id) {
            self.selected.insert(node_id);
            self.selected_node = Some(node_id);
        } else if self.selected_node == Some(node_id) {
            self.selected_node = self.selected.iter().copied().next();
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected.clear();
        self.selected_node = None;
        self.dragging_node = None;
        self.box_select_from = None;
    }
}

#[derive(Clone, Debug)]
struct NodeClipboard {
    nodes: Vec<Node>,
    connections: Vec<Connection>,
    min_pos: [f32; 2],
    paste_count: u32,
}

fn copy_selected_nodes(graph: &NodeGraph, selected_nodes: &HashSet<NodeId>) -> Option<NodeClipboard> {
    let nodes: Vec<Node> = graph
        .nodes
        .iter()
        .filter(|node| {
            selected_nodes.contains(&node.id)
                && !matches!(node.node_type, NodeType::MaterialOutput)
        })
        .cloned()
        .collect();
    if nodes.is_empty() {
        return None;
    }
    let copied_ids: HashSet<NodeId> = nodes.iter().map(|node| node.id).collect();
    let connections = graph
        .connections
        .iter()
        .filter(|connection| {
            copied_ids.contains(&connection.from_node) && copied_ids.contains(&connection.to_node)
        })
        .cloned()
        .collect();
    let min_pos = nodes.iter().fold([f32::INFINITY; 2], |minimum, node| {
        [minimum[0].min(node.pos[0]), minimum[1].min(node.pos[1])]
    });
    Some(NodeClipboard {
        nodes,
        connections,
        min_pos,
        paste_count: 0,
    })
}

fn paste_node_clipboard(
    graph: &mut NodeGraph,
    clipboard: &mut NodeClipboard,
    target_pos: [f32; 2],
) -> HashSet<NodeId> {
    clipboard.paste_count = clipboard.paste_count.saturating_add(1);
    let cascade = 24.0 * clipboard.paste_count.saturating_sub(1) as f32;
    let mut id_map = HashMap::new();
    let mut pasted = HashSet::new();
    for source in &clipboard.nodes {
        let new_id = graph.add_node(source.node_type.clone(), source.pos);
        let mut cloned = source.clone();
        cloned.id = new_id;
        cloned.pos = [
            target_pos[0] + source.pos[0] - clipboard.min_pos[0] + cascade,
            target_pos[1] + source.pos[1] - clipboard.min_pos[1] + cascade,
        ];
        if let Some(destination) = graph.find_node_mut(new_id) {
            *destination = cloned;
        }
        id_map.insert(source.id, new_id);
        pasted.insert(new_id);
    }
    for connection in &clipboard.connections {
        let (Some(&from_node), Some(&to_node)) = (
            id_map.get(&connection.from_node),
            id_map.get(&connection.to_node),
        ) else {
            continue;
        };
        let _ = graph.connect(
            from_node,
            connection.from_output,
            to_node,
            connection.to_input,
        );
    }
    graph.wgsl_cache = None;
    pasted
}

struct NoisePreviewTexture {
    fingerprint: u64,
    texture: egui::TextureHandle,
}

/// Paint a color swatch; click opens/toggles the sticky picker (does not use egui's popup).
fn color_swatch_button(ui: &mut egui::Ui, rgb: &[f32; 3], open: bool) -> egui::Response {
    let size = ui.spacing().interact_size;
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let col = egui::Color32::from_rgb(
            (rgb[0].clamp(0.0, 1.0) * 255.0) as u8,
            (rgb[1].clamp(0.0, 1.0) * 255.0) as u8,
            (rgb[2].clamp(0.0, 1.0) * 255.0) as u8,
        );
        let visuals = if open {
            &ui.visuals().widgets.open
        } else {
            ui.style().interact(&response)
        };
        let r = rect.expand(visuals.expansion);
        ui.painter().rect_filled(r, 2.0, col);
        ui.painter().rect_stroke(
            r,
            2.0,
            egui::Stroke::new(if open { 2.0 } else { 1.0 }, visuals.bg_fill), egui::StrokeKind::Middle,
        );
    }
    response
}

/// Resolve mutable RGB for a sticky color target.
fn color_popup_rgb_mut<'a>(
    graph: &'a mut NodeGraph,
    target: &ColorPopupTarget,
) -> Option<&'a mut [f32; 3]> {
    match *target {
        ColorPopupTarget::Input { node, socket } => {
            let n = graph.find_node_mut(node)?;
            n.input_defaults.get_mut(socket).map(|d| &mut d.value_vec3)
        }
        ColorPopupTarget::Ramp0 { node } => {
            graph.find_node_mut(node).map(|n| &mut n.ramp_color0)
        }
        ColorPopupTarget::Ramp1 { node } => {
            graph.find_node_mut(node).map(|n| &mut n.ramp_color1)
        }
    }
}

/// Draw sticky color picker; returns true if color changed.
/// Closes only on: Escape, primary-click outside the panel (not while dragging on it).
fn draw_sticky_color_popup(
    ui: &mut egui::Ui,
    graph: &mut NodeGraph,
    drag_state: &mut NodeDragState,
) -> bool {
    let Some(popup) = drag_state.color_popup.clone() else {
        return false;
    };
    // While open, canvas must not steal LMB / deselect.
    drag_state.field_interact = true;

    let mut color_changed = false;

    let area_id = match &popup.target {
        ColorPopupTarget::Input { node, socket } => {
            egui::Id::new(("ncp", "in", *node, *socket as u32))
        }
        ColorPopupTarget::Ramp0 { node } => egui::Id::new(("ncp", "r0", *node)),
        ColorPopupTarget::Ramp1 { node } => egui::Id::new(("ncp", "r1", *node)),
    };

    let area_resp = egui::Area::new(area_id)
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(popup.pos[0], popup.pos[1]))
        .constrain(true)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .show(ui, |ui| {
                    ui.set_min_width(220.0);
                    ui.spacing_mut().slider_width = 200.0;
                    let mut rgb = [0.5f32; 3];
                    if let Some(c) = color_popup_rgb_mut(graph, &popup.target) {
                        rgb = *c;
                    }
                    let mut rgba = egui::Rgba::from_rgb(rgb[0], rgb[1], rgb[2]);
                    let mut hsva: egui::ecolor::Hsva = rgba.into();
                    if egui::color_picker::color_picker_hsva_2d(
                        ui,
                        &mut hsva,
                        egui::color_picker::Alpha::Opaque,
                    ) {
                        rgba = hsva.into();
                        rgb = [rgba.r(), rgba.g(), rgba.b()];
                        if let Some(c) = color_popup_rgb_mut(graph, &popup.target) {
                            *c = rgb;
                        }
                        color_changed = true;
                    }
                })
                .response
        })
        .response;

    let popup_rect = area_resp.rect;
    if area_resp.hovered() || area_resp.contains_pointer() || area_resp.dragged() {
        drag_state.field_interact = true;
    }

    // Close: Escape
    let mut want_close = ui.input(|i| i.key_pressed(egui::Key::Escape));

    // Close: completed primary click outside the panel (not a drag on the spectrum).
    // Do NOT use any_click / clicked_elsewhere — those fire when canvas drag-guards see the press.
    if ui.input(|i| i.pointer.primary_clicked()) && !drag_state.color_swatch_hit {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            if !popup_rect.expand(6.0).contains(pos) {
                want_close = true;
            }
        }
    }

    if want_close {
        drag_state.color_popup = None;
    }

    color_changed
}

/// Open or retarget sticky color popup from a swatch click.
fn open_color_popup(
    drag_state: &mut NodeDragState,
    target: ColorPopupTarget,
    swatch_rect: egui::Rect,
) {
    drag_state.color_swatch_hit = true;
    drag_state.field_interact = true;
    // Toggle off if same target already open
    if drag_state
        .color_popup
        .as_ref()
        .map(|p| p.target == target)
        .unwrap_or(false)
    {
        drag_state.color_popup = None;
        return;
    }
    drag_state.color_popup = Some(ColorPopupState {
        target,
        pos: [swatch_rect.left(), swatch_rect.bottom() + 4.0],
    });
}

/// Case-insensitive match of filter against node label and/or category.
fn palette_matches(filter: &str, cat: &str, label: &str) -> bool {
    let f = filter.trim();
    if f.is_empty() {
        return true;
    }
    let f = f.to_lowercase();
    label.to_lowercase().contains(&f)
        || cat.to_lowercase().contains(&f)
        || (f == "math" && cat == "Operations")
}

fn apply_palette_preset(graph: &mut NodeGraph, node_id: NodeId, label: &str) {
    let Some(node) = graph.find_node_mut(node_id) else {
        return;
    };
    match node.node_type {
        NodeType::MathBinary => {
            let operation_label = label.strip_prefix("Value ").unwrap_or(label);
            if let Some(op) = MathBinaryOp::all()
                .iter()
                .find(|op| op.label() == operation_label)
            {
                node.math_binary_op = *op;
            }
        }
        NodeType::MathUnary => {
            if let Some(op) = MathUnaryOp::all().iter().find(|op| op.label() == label) {
                node.math_unary_op = *op;
            }
        }
        NodeType::MixRgb if label == "Multiply" => {
            node.mix_blend = MixBlend::Multiply;
        }
        _ => {}
    }
}

fn socket_type_of(
    graph: &NodeGraph,
    nid: NodeId,
    idx: usize,
    is_output: bool,
) -> Option<SocketType> {
    let node = graph.find_node(nid)?;
    if is_output {
        Node::output_defs(&node.node_type).get(idx).map(|s| s.socket_type)
    } else {
        Node::input_defs(&node.node_type).get(idx).map(|s| s.socket_type)
    }
}

fn node_header_color(nt: &NodeType) -> egui::Color32 {
    match nt {
        NodeType::MaterialOutput => egui::Color32::from_rgb(140, 50, 50),
        NodeType::TexCoord | NodeType::ObjectInfo | NodeType::ConstantColor => {
            egui::Color32::from_rgb(80, 50, 120)
        }
        NodeType::Noise | NodeType::Voronoi => egui::Color32::from_rgb(60, 100, 60),
        NodeType::PrimSdfMask | NodeType::SdfCurvature => egui::Color32::from_rgb(50, 110, 90),
        NodeType::MathBinary | NodeType::MathUnary | NodeType::Remap => {
            egui::Color32::from_rgb(80, 80, 100)
        }
        NodeType::Triplanar
        | NodeType::BoxMapping
        | NodeType::CylindricalMapping
        | NodeType::SphericalMapping
        | NodeType::PlanarMapping
        | NodeType::UVTransform => egui::Color32::from_rgb(50, 100, 120),
        NodeType::TextureSample
        | NodeType::GradientTexture
        | NodeType::WaveTexture
        | NodeType::MusgraveTexture
        | NodeType::BrickTexture
        | NodeType::ImageTexture => egui::Color32::from_rgb(120, 80, 40),
        _ => egui::Color32::from_rgb(100, 80, 50),
    }
}

/// G7: does the noodle from `from` to `to` cross the freehand cut stroke?
/// Samples the SAME cubic bezier `draw_noodle` draws (control points must stay in
/// lockstep with it) and runs segment-segment tests against the stroke.
fn noodle_crosses_stroke(from: egui::Pos2, to: egui::Pos2, stroke: &[egui::Pos2]) -> bool {
    let mid_x = (from.x + to.x) * 0.5;
    let (c1, c2) = (egui::pos2(mid_x, from.y), egui::pos2(mid_x, to.y));
    const N: usize = 24;
    let bez = |t: f32| -> egui::Pos2 {
        let u = 1.0 - t;
        egui::pos2(
            u * u * u * from.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * to.x,
            u * u * u * from.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * to.y,
        )
    };
    let mut prev = bez(0.0);
    for i in 1..=N {
        let cur = bez(i as f32 / N as f32);
        for w in stroke.windows(2) {
            if segments_intersect(prev, cur, w[0], w[1]) {
                return true;
            }
        }
        prev = cur;
    }
    false
}

/// G7: does the noodle pass through `rect`? Same bezier sampling as
/// `noodle_crosses_stroke` — the two must stay in lockstep with `draw_noodle`.
fn noodle_crosses_rect(from: egui::Pos2, to: egui::Pos2, rect: egui::Rect) -> bool {
    let mid_x = (from.x + to.x) * 0.5;
    let (c1, c2) = (egui::pos2(mid_x, from.y), egui::pos2(mid_x, to.y));
    const N: usize = 24;
    for i in 0..=N {
        let t = i as f32 / N as f32;
        let u = 1.0 - t;
        let p = egui::pos2(
            u * u * u * from.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * to.x,
            u * u * u * from.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * to.y,
        );
        if rect.contains(p) {
            return true;
        }
    }
    false
}

fn segments_intersect(a: egui::Pos2, b: egui::Pos2, c: egui::Pos2, d: egui::Pos2) -> bool {
    let cross = |o: egui::Pos2, p: egui::Pos2, q: egui::Pos2| -> f32 {
        (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x)
    };
    let d1 = cross(c, d, a);
    let d2 = cross(c, d, b);
    let d3 = cross(a, b, c);
    let d4 = cross(a, b, d);
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
}

fn draw_noodle(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32) {
    let mid_x = (from.x + to.x) * 0.5;
    let bezier = egui::epaint::CubicBezierShape::from_points_stroke(
        [from, egui::pos2(mid_x, from.y), egui::pos2(mid_x, to.y), to],
        false, egui::Color32::TRANSPARENT,
        egui::Stroke::new(width, color),
    );
    painter.add(bezier);
}

fn socket_color(st: &SocketType) -> egui::Color32 {
    match st {
        SocketType::Float => egui::Color32::from_rgb(160, 160, 160),
        SocketType::Vec3 => egui::Color32::from_rgb(100, 100, 200),
        SocketType::Color => egui::Color32::from_rgb(200, 200, 50),
    }
}

/// Canonical node body size — keep in sync with draw_node_editor paint loop.
/// Wider so per-side U/V/° projection rows fit.
const NODE_BODY_W: f32 = 248.0;
const NODE_ROW_H: f32 = 22.0;

fn node_has_noise_preview(node_type: &NodeType) -> bool {
    matches!(
        node_type,
        NodeType::Noise | NodeType::Voronoi | NodeType::MusgraveTexture
    )
}

fn shader_graph_fingerprint(graph: &NodeGraph) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    graph.bake_res.hash(&mut hasher);
    for node in &graph.nodes {
        node.id.hash(&mut hasher);
        std::mem::discriminant(&node.node_type).hash(&mut hasher);
        node.math_binary_op.label().hash(&mut hasher);
        node.math_unary_op.label().hash(&mut hasher);
        node.vector_math_op.label().hash(&mut hasher);
        node.mix_blend.label().hash(&mut hasher);
        node.noise_type.label().hash(&mut hasher);
        node.projection.label().hash(&mut hasher);
        node.noise_curve.label().hash(&mut hasher);
        node.noise_tile.hash(&mut hasher);
        node.image_map_kind.hash(&mut hasher);
        node.image_tex_id.hash(&mut hasher);
        node.sdf_prim_uid.hash(&mut hasher);
        node.sdf_curv_region.hash(&mut hasher);
        for value in [
            node.const_float,
            node.ramp_pos0,
            node.ramp_pos1,
            node.proj_rot_x,
            node.proj_rot_y,
            node.proj_rot_z,
            node.noise_curve_k,
            node.noise_black,
            node.noise_white,
        ] {
            value.to_bits().hash(&mut hasher);
        }
        for values in [
            node.ramp_color0,
            node.ramp_color1,
            [node.proj_off_x[0], node.proj_off_x[1], 0.0],
            [node.proj_off_y[0], node.proj_off_y[1], 0.0],
            [node.proj_off_z[0], node.proj_off_z[1], 0.0],
        ] {
            for value in values {
                value.to_bits().hash(&mut hasher);
            }
        }
        for input in &node.input_defaults {
            input.value_f32.to_bits().hash(&mut hasher);
            for value in input.value_vec3 {
                value.to_bits().hash(&mut hasher);
            }
        }
    }
    for connection in &graph.connections {
        connection.from_node.hash(&mut hasher);
        connection.from_output.hash(&mut hasher);
        connection.to_node.hash(&mut hasher);
        connection.to_input.hash(&mut hasher);
    }
    hasher.finish()
}

fn draw_noise_preview(
    ui: &egui::Ui,
    painter: &egui::Painter,
    graph: &NodeGraph,
    node_id: NodeId,
    rect: egui::Rect,
    fingerprint: u64,
    drag_state: &mut NodeDragState,
    previews_rendered: &mut usize,
) {
    if !ui.is_rect_visible(rect) { return; }
    drag_state.gpu_noise_preview_requests.insert(node_id, fingerprint);
    if let Some(texture) = drag_state.gpu_noise_preview_ids.get(&node_id) {
        painter.image(*texture, rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::from_gray(90)), egui::StrokeKind::Middle);
        return;
    }
    if drag_state.gpu_noise_preview_pending.contains(&node_id) {
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(20));
        return;
    }
    let needs_refresh = drag_state
        .noise_preview_cache
        .get(&node_id)
        .is_none_or(|cached| cached.fingerprint != fingerprint);
    let defer_refresh =
        drag_state.field_interact && ui.input(|input| input.pointer.primary_down());
    if needs_refresh && !defer_refresh && *previews_rendered < MAX_NOISE_PREVIEWS_PER_FRAME {
        if let Some(rgba) =
            crate::node_graph_bake::render_noise_preview_rgba(
                graph,
                node_id,
                NOISE_PREVIEW_TEXTURE_PX,
            )
        {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [NOISE_PREVIEW_TEXTURE_PX, NOISE_PREVIEW_TEXTURE_PX],
                &rgba,
            );
            if let Some(cached) = drag_state.noise_preview_cache.get_mut(&node_id) {
                cached.texture.set(image, egui::TextureOptions::LINEAR);
                cached.fingerprint = fingerprint;
            } else {
                let texture = ui.ctx().load_texture(
                    format!("shader_noise_preview_{node_id}"),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                drag_state.noise_preview_cache.insert(
                    node_id,
                    NoisePreviewTexture {
                        fingerprint,
                        texture,
                    },
                );
            }
            *previews_rendered += 1;
        }
    }

    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(20));
    if let Some(cached) = drag_state.noise_preview_cache.get(&node_id) {
        painter.image(
            cached.texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(90)), egui::StrokeKind::Middle,
    );
}

/// Visual row index for a Master Material input (inserts 1 separator row before Advanced).
fn master_input_row(input_idx: usize, _advanced_open: bool) -> usize {
    if input_idx == NodeGraph::MO_BUMP_STRENGTH {
        NodeGraph::MO_HEIGHT + 2
    } else if input_idx > NodeGraph::MO_HEIGHT {
        input_idx + 2
    } else if input_idx >= NodeGraph::MO_ADVANCED_START {
        input_idx + 1
    } else {
        input_idx
    }
}

/// Extra body rows for on-node controls (dropdowns / color / image channel).
/// Note: projection side Pos/Rot rows depend on live `node.projection` — use
/// `node_param_row_count_for(node)` when painting.
fn node_param_row_count(nt: &NodeType) -> usize {
    node_param_row_count_proj(nt, ProjectionStyle::Triplanar)
}

fn node_param_row_count_for(node: &Node) -> usize {
    node_param_row_count_proj(&node.node_type, node.projection)
}

fn node_param_row_count_proj(nt: &NodeType, proj: ProjectionStyle) -> usize {
    let mut n = 0;
    if matches!(nt, NodeType::Noise) {
        n += 1; // type + tile
        n += 3; // Curve + Black/White + K/Mid
    }
    if matches!(nt, NodeType::PrimSdfMask | NodeType::SdfCurvature) {
        n += 1; // prim picker
    }
    if matches!(nt, NodeType::SdfCurvature) {
        n += 1; // region combo
    }
    if node_has_builtin_projection(nt) {
        n += 1; // Proj type dropdown
        n += match proj {
            ProjectionStyle::None => 0,
            ProjectionStyle::Triplanar | ProjectionStyle::Box => 3, // X/Y/Z sides
            _ => 1, // single plane: Pos + Rot (Z-side fields)
        };
    }
    if matches!(
        nt,
        NodeType::MathBinary | NodeType::MathUnary | NodeType::VectorMath
    ) {
        n += 1; // op
    }
    if matches!(nt, NodeType::MixRgb) {
        n += 1; // blend mode combo
    }
    if matches!(nt, NodeType::ColorRamp) {
        n += 2;
    }
    if matches!(nt, NodeType::ConstantColor) {
        n += 1;
    }
    if matches!(nt, NodeType::ImageTexture) {
        n += 1;
    }
    if matches!(nt, NodeType::MaterialOutput) {
        n += 1; // Map res dropdown
    }
    if node_has_noise_preview(nt) {
        n += NOISE_PREVIEW_ROWS;
    }
    n
}

fn node_header_h(nt: &NodeType) -> f32 {
    if node_has_builtin_projection(nt) { 32.0 } else { 20.0 }
}

fn node_body_top_local(nt: &NodeType) -> f32 {
    node_header_h(nt) + 4.0
}

fn node_body_w(nt: &NodeType) -> f32 {
    if matches!(nt, NodeType::MaterialOutput) {
        NODE_BODY_W + 36.0
    } else {
        NODE_BODY_W
    }
}

fn node_output_pos(graph: &NodeGraph, node_id: NodeId, output_idx: usize, canvas_origin: egui::Pos2) -> Option<egui::Pos2> {
    let node = graph.find_node(node_id)?;
    let x = canvas_origin.x + node.pos[0] + node_body_w(&node.node_type);
    let y = canvas_origin.y + node.pos[1] + node_body_top_local(&node.node_type)
        + output_idx as f32 * NODE_ROW_H + NODE_ROW_H * 0.5;
    Some(egui::pos2(x, y))
}

fn node_input_pos(graph: &NodeGraph, node_id: NodeId, input_idx: usize, canvas_origin: egui::Pos2) -> Option<egui::Pos2> {
    let node = graph.find_node(node_id)?;
    let row = if matches!(node.node_type, NodeType::MaterialOutput) {
        master_input_row(input_idx, true)
    } else {
        input_idx
    };
    let x = canvas_origin.x + node.pos[0];
    let y = canvas_origin.y + node.pos[1] + node_body_top_local(&node.node_type)
        + row as f32 * NODE_ROW_H + NODE_ROW_H * 0.5;
    Some(egui::pos2(x, y))
}

// ============================================================================
// G7 — link-cut / insert geometry pins. The cut promise is "exactly the crossed
// links": these pin the bezier sampling + intersection kernel that decides it.
// ============================================================================
#[cfg(test)]
mod g7_geom {
    use super::*;

    #[test]
    fn segments_cross_and_miss() {
        let p = egui::pos2;
        assert!(segments_intersect(p(0.0, 0.0), p(10.0, 10.0), p(0.0, 10.0), p(10.0, 0.0)));
        assert!(!segments_intersect(p(0.0, 0.0), p(10.0, 0.0), p(0.0, 5.0), p(10.0, 5.0)));
        assert!(!segments_intersect(p(0.0, 0.0), p(4.0, 0.0), p(5.0, -1.0), p(5.0, 1.0)));
    }

    #[test]
    fn cut_stroke_hits_only_the_crossed_noodle() {
        let p = egui::pos2;
        // Horizontal noodle A at y=100, B at y=200; vertical stroke crosses A only.
        let stroke = vec![p(50.0, 60.0), p(50.0, 140.0)];
        assert!(noodle_crosses_stroke(p(0.0, 100.0), p(100.0, 100.0), &stroke), "crossed");
        assert!(
            !noodle_crosses_stroke(p(0.0, 200.0), p(100.0, 200.0), &stroke),
            "the parallel noodle below must NOT be cut"
        );
        // The bezier bows between differing endpoint heights — a stroke through the
        // midline must still hit it.
        let stroke2 = vec![p(50.0, 100.0), p(50.0, 160.0)];
        assert!(noodle_crosses_stroke(p(0.0, 100.0), p(100.0, 160.0), &stroke2));
    }

    #[test]
    fn drop_rect_hit_uses_the_same_curve() {
        let p = egui::pos2;
        let on = egui::Rect::from_center_size(p(50.0, 130.0), egui::vec2(30.0, 30.0));
        let off = egui::Rect::from_center_size(p(50.0, 300.0), egui::vec2(30.0, 30.0));
        assert!(noodle_crosses_rect(p(0.0, 100.0), p(100.0, 160.0), on));
        assert!(!noodle_crosses_rect(p(0.0, 100.0), p(100.0, 160.0), off));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_values_preserves_wires_and_returns_undo_snapshot() {
        let context = egui::Context::default();
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let noise = graph.add_node(NodeType::Noise, [30.0, 30.0]);
        assert!(graph.connect(noise, 0, output, NodeGraph::MO_HEIGHT));
        graph.find_node_mut(noise).unwrap().input_defaults[1].value_f32 = 9.0;
        graph.find_node_mut(noise).unwrap().noise_white = 16.15;
        let mut drag = NodeDragState::default();
        drag.select_single(noise);
        let mut position = None;
        let mut outcome = NodeEditorOutcome::default();
        for frame in 0..4 {
            let events = if frame >= 2 {
                let position = position.unwrap();
                vec![egui::Event::PointerMoved(position), egui::Event::PointerButton {
                    pos: position, button: egui::PointerButton::Primary, pressed: frame == 2, modifiers: egui::Modifiers::NONE,
                }]
            } else { Vec::new() };
            let output = context.run(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 1000.0))),
                events, ..Default::default()
            }, |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    outcome = draw_node_editor(ui, &mut graph, &mut drag, &[]);
                });
            });
            if frame < 2 {
                for shape in output.shapes {
                    if let egui::Shape::Text(text) = shape.shape {
                        if text.galley.job.text == "Reset Values" { position = Some(text.pos + text.galley.size() * 0.5); }
                    }
                }
            }
        }
        let node = graph.find_node(noise).unwrap();
        assert_eq!(node.input_defaults[1].value_f32, 4.0);
        assert_eq!(node.noise_white, 1.0);
        assert_eq!(graph.connection_source(output, NodeGraph::MO_HEIGHT), Some((noise, 0)));
        assert_eq!(node.pos, [30.0, 30.0]);
        assert!(outcome.changed && outcome.atomic_change);
        let before = outcome.graph_before_edit.unwrap();
        assert_eq!(before.find_node(noise).unwrap().noise_white, 16.15);
        assert_eq!(before.find_node(noise).unwrap().input_defaults[1].value_f32, 9.0);
    }

    #[test]
    fn bump_strength_is_next_to_height_without_changing_socket_ids() {
        let inputs = Node::input_defs(&NodeType::MaterialOutput);
        assert_eq!(inputs[NodeGraph::MO_BUMP_STRENGTH].name, "Bump Strength");
        assert_eq!(master_input_row(NodeGraph::MO_BUMP_STRENGTH, true), master_input_row(NodeGraph::MO_HEIGHT, true) + 1);
        let mut rows: Vec<_> = (0..inputs.len()).map(|input| master_input_row(input, true)).collect();
        rows.sort_unstable();
        rows.dedup();
        assert_eq!(rows.len(), inputs.len());
        assert_eq!(rows.last().copied(), Some(inputs.len()));
        let mut graph = NodeGraph::default();
        let output = graph.add_node(NodeType::MaterialOutput, [0.0, 0.0]);
        let height = node_input_pos(&graph, output, NodeGraph::MO_HEIGHT, egui::Pos2::ZERO).unwrap();
        let strength = node_input_pos(&graph, output, NodeGraph::MO_BUMP_STRENGTH, egui::Pos2::ZERO).unwrap();
        assert_eq!(strength.y - height.y, NODE_ROW_H);
    }

    #[test]
    fn node_fields_fit_rows_with_large_shell_fonts() {
        let context = egui::Context::default();
        context.style_mut(|style| {
            style.override_font_id = Some(egui::FontId::proportional(20.0));
            style.spacing.interact_size = egui::vec2(90.0, 32.0);
            style.spacing.button_padding = egui::vec2(8.0, 6.0);
        });
        let mut graph = NodeGraph::default();
        graph.add_node(NodeType::Noise, [30.0, 30.0]);
        let mut drag = NodeDragState::default();
        let mut field_count = 0;
        for _ in 0..2 {
            let output = context.run(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 1000.0))),
                ..Default::default()
            }, |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    draw_node_editor(ui, &mut graph, &mut drag, &[]);
                });
            });
            for shape in output.shapes {
                if let egui::Shape::Text(text) = shape.shape {
                    if text.galley.job.text == "4" || text.galley.job.text.starts_with("x0") {
                        assert!(text.galley.size().y <= 18.0, "numeric field exceeds row: {:?}", text.galley.size());
                        field_count += 1;
                    }
                }
            }
        }
        assert!(field_count > 0);
    }

    #[test]
    fn operation_palette_matches_specific_and_legacy_searches() {
        assert!(palette_matches("multiply", "Color", "Multiply"));
        assert!(palette_matches("multiply", "Operations", "Value Multiply"));
        assert!(palette_matches("math", "Operations", "Value Multiply"));
        assert!(!palette_matches("color", "Operations", "Value Multiply"));
    }

    #[test]
    fn multiply_search_prefers_photoshop_color_blend() {
        let first_match = NodePalette::categories()
            .iter()
            .flat_map(|(category, entries)| {
                entries
                    .iter()
                    .map(move |(node_type, label)| (*category, node_type, *label))
            })
            .find(|(category, _, label)| palette_matches("multiply", category, label))
            .expect("Multiply must remain searchable");

        assert_eq!(*first_match.1, NodeType::MixRgb);
        assert_eq!(first_match.2, "Multiply");
    }

    #[test]
    fn operation_palette_entry_configures_created_node() {
        let mut graph = NodeGraph::default();
        let node_id = graph.add_node(NodeType::MathBinary, [0.0, 0.0]);

        apply_palette_preset(&mut graph, node_id, "Value Multiply");

        assert_eq!(
            graph.find_node(node_id).unwrap().math_binary_op,
            MathBinaryOp::Multiply
        );
    }

    #[test]
    fn multiply_palette_entry_creates_photoshop_color_blend() {
        let mut graph = NodeGraph::default();
        let node_id = graph.add_node(NodeType::MixRgb, [0.0, 0.0]);

        apply_palette_preset(&mut graph, node_id, "Multiply");

        assert_eq!(graph.find_node(node_id).unwrap().mix_blend, MixBlend::Multiply);
    }

    #[test]
    fn node_clipboard_pastes_subgraph_across_materials() {
        let mut source = NodeGraph::default();
        let color_id = source.add_node(NodeType::ConstantColor, [20.0, 40.0]);
        let noise_id = source.add_node(NodeType::Noise, [20.0, 180.0]);
        let blend_id = source.add_node(NodeType::MixRgb, [260.0, 90.0]);
        source.find_node_mut(color_id).unwrap().ramp_color0 = [0.8, 0.1, 0.05];
        source.find_node_mut(blend_id).unwrap().mix_blend = MixBlend::Multiply;
        assert!(source.connect(color_id, 0, blend_id, 1));
        assert!(source.connect(noise_id, 0, blend_id, 2));

        let selected = HashSet::from([color_id, noise_id, blend_id]);
        let mut clipboard = copy_selected_nodes(&source, &selected).unwrap();
        let mut destination = NodeGraph::default();
        let pasted = paste_node_clipboard(&mut destination, &mut clipboard, [100.0, 200.0]);

        assert_eq!(pasted.len(), 3);
        assert_eq!(destination.connections.len(), 2);
        let pasted_blend = destination
            .nodes
            .iter()
            .find(|node| pasted.contains(&node.id) && matches!(node.node_type, NodeType::MixRgb))
            .unwrap();
        assert_eq!(pasted_blend.mix_blend, MixBlend::Multiply);
        assert_eq!(pasted_blend.pos, [340.0, 250.0]);
        assert!(destination.connections.iter().all(|connection| {
            pasted.contains(&connection.from_node) && pasted.contains(&connection.to_node)
        }));
    }

    #[test]
    fn right_click_detach_is_atomic_and_ignores_empty_sockets() {
        let mut graph = NodeGraph::default();
        let value_id = graph.add_node(NodeType::ConstantValue, [0.0, 0.0]);
        let output_id = graph.add_node(NodeType::MaterialOutput, [300.0, 0.0]);
        assert!(graph.connect(value_id, 0, output_id, NodeGraph::MO_ALBEDO));
        assert!(graph.connect(value_id, 0, output_id, NodeGraph::MO_ROUGHNESS));

        let before_output_detach = disconnect_socket(&mut graph, value_id, 0, true)
            .expect("connected output must detach");
        assert_eq!(before_output_detach.connections.len(), 2);
        assert!(graph.connections.is_empty());
        assert!(disconnect_socket(&mut graph, value_id, 0, true).is_none());

        assert!(graph.connect(value_id, 0, output_id, NodeGraph::MO_ALBEDO));
        let before_input_detach = disconnect_socket(
            &mut graph,
            output_id,
            NodeGraph::MO_ALBEDO,
            false,
        )
        .expect("connected input must detach");
        assert_eq!(before_input_detach.connections.len(), 1);
        assert!(graph.connections.is_empty());
        assert!(disconnect_socket(
            &mut graph,
            output_id,
            NodeGraph::MO_ALBEDO,
            false,
        )
        .is_none());
    }

    #[test]
    fn noise_preview_fingerprint_tracks_values_not_node_position() {
        let mut graph = NodeGraph::default();
        let node_id = graph.add_node(NodeType::Noise, [0.0, 0.0]);
        let initial = shader_graph_fingerprint(&graph);

        graph.find_node_mut(node_id).unwrap().input_defaults[1].value_f32 = 9.0;
        let changed_value = shader_graph_fingerprint(&graph);
        assert_ne!(initial, changed_value);

        graph.find_node_mut(node_id).unwrap().pos = [300.0, 200.0];
        assert_eq!(changed_value, shader_graph_fingerprint(&graph));
    }
}
// ============================================================================
// GROWTH PROBE (owner 2026-08-16, recurrence of #5 "shader panel keeps
// expanding"): run the REAL window structure (footer reservation + ceiling +
// draw_node_editor) headlessly and MEASURE the window rect across frames.
// A ratchet is a per-frame height increase — assert it cannot happen, with a
// populated graph including the G8 Frame and a node dragged far BELOW the
// canvas (the historical trigger).
// ============================================================================
#[cfg(test)]
mod growth_probe {
    use super::*;

    #[test]
    fn window_height_is_stable_across_frames() {
        let ctx = egui::Context::default();
        let mut graph = NodeGraph::default();
        let n1 = graph.add_node(NodeType::Noise, [40.0, 60.0]);
        let _ = n1;
        let f = graph.add_node(NodeType::Frame, [10.0, 10.0]);
        if let Some(n) = graph.find_node_mut(f) {
            n.frame_size = [400.0, 300.0];
        }
        // The historical trigger: a node far below the visible canvas.
        let low = graph.add_node(NodeType::Checker, [120.0, 2400.0]);
        let _ = low;
        let mut ds = NodeDragState::default();
        let win_id = egui::Id::new("shader_node_editor_probe");
        let mut heights: Vec<f32> = Vec::new();
        for _ in 0..15 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1920.0, 1080.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::Window::new("probe")
                    .id(win_id)
                    .default_size([820.0, 500.0])
                    .min_width(360.0)
                    .min_height(260.0)
                    .max_size(ctx.screen_rect().size() * 0.95)
                    .show(ctx, |ui| {
                        ui.label("Editing: probe");
                        ui.separator();
                        const FOOTER_H: f32 = 36.0;
                        let avail = ui.available_size();
                        let canvas_area =
                            egui::vec2(avail.x.max(80.0), (avail.y - FOOTER_H).max(60.0));
                        ui.allocate_ui(canvas_area, |ui| {
                            ui.set_min_size(canvas_area);
                            ui.set_max_size(canvas_area);
                            let _ = draw_node_editor(ui, &mut graph, &mut ds, &[]);
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            let _ = ui.button("Apply");
                        });
                    });
            });
            let rect = ctx.memory(|m| m.area_rect(win_id));
            heights.push(rect.map(|r| r.height()).unwrap_or(-1.0));
        }
        // Frames 0-2 may settle (first-layout, font sizing). After that: FLAT.
        let settled = heights[3];
        for (i, h) in heights.iter().enumerate().skip(4) {
            assert!(
                (*h - settled).abs() < 0.5,
                "window height RATCHETS: frame {i} = {h}, settled = {settled}; all: {heights:?}"
            );
        }
    }
}

