//! Outliner drag-and-drop hit testing.
use eframe::egui;
use crate::*;
use super::types::*;

impl FourthLayerApp {
    /// Hover-to-expand dwell while a drag is live (v1 parity — `outliner_draw_body.rs`
    /// runs the same timer for the old shell, same `HOVER_EXPAND_SECS`). Hovering a
    /// COLLAPSED body row for the dwell duration expands it via `ol_set_expanded`
    /// (dual-write invariant, INTEGRATION_RULES §2 — never touch `expanded_bodies`
    /// directly) so the drop can land inside. The timer restarts whenever the hovered
    /// row changes (the `_ =>` arm); the shared release helper (`outliner_dnd_release`)
    /// clears it when no collapsed row saw the pointer this frame or when the drag
    /// ended/cancelled. Rows are rebuilt from `ol_is_expanded` every frame, so the
    /// children appear on the next frame while the drag stays live. Blender leaves the
    /// body open afterwards — so do we (no restore tracking).
    fn ui2_ol_hover_expand_dwell(&mut self, ctx: &egui::Context, nid: NodeId) {
        let now = ctx.input(|i| i.time);
        match self.outliner.hover_expand {
            Some((cur, since)) if cur == nid => {
                if now - since >= crate::outliner_dnd::HOVER_EXPAND_SECS {
                    self.ol_set_expanded(nid, true);
                    self.outliner.hover_expand = None;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                }
            }
            _ => {
                self.outliner.hover_expand = Some((nid, now));
                ctx.request_repaint_after(std::time::Duration::from_millis(650));
            }
        }
        self.outliner.hover_expand_hit = true;
    }

    /// Hit-test one painted row during a live drag → fills `outliner.drop_*` state.
    pub(crate) fn ui2_ol_dnd_hit(
        &mut self,
        ctx: &egui::Context,
        row: &OlRow,
        rect: egui::Rect,
        ptr: egui::Pos2,
    ) {
        use crate::outliner_drop_slots::{
            classify_zone, resolve_body_over_body_header, resolve_prim_over_body_header,
            resolve_prim_over_prim_row, slot_label, BodyOverBodyHeader, HeaderZone,
            PrimOverBodyHeader, PrimOverPrimRow, SlotAction,
        };
        use app_types::{DropVisual, DropVisualKind};
        use core_math::scene_graph::BodyChildRef;

        let zone = classify_zone(rect.top(), rect.bottom(), ptr.y, 1.0 / 3.0);
        let indent_x = rect.left() + OL_EYE_W + 8.0 + row.depth as f32 * OL_INDENT;

        if self.outliner.dragging.is_some() {
            match row.item {
                OlItem::Prim(uid) => {
                    let Some(fi) = self.prim_index_by_uid(uid) else {
                        return;
                    };
                    let Some(&(body, _)) = self.scene.prim_body_map.get(fi) else {
                        return;
                    };
                    let order = self.scene.scene_graph.body_child_order(body);
                    match resolve_prim_over_prim_row(&order, body, uid, zone) {
                        PrimOverPrimRow::Insert { body, index } => {
                            self.outliner.drop_insert = Some((body, index));
                            let y = if zone == HeaderZone::Top {
                                rect.top()
                            } else {
                                rect.bottom()
                            };
                            let act = if zone == HeaderZone::Top {
                                SlotAction::Before(&row.name)
                            } else {
                                SlotAction::After(&row.name)
                            };
                            self.outliner.drop_visual = Some(DropVisual {
                                kind: DropVisualKind::Line,
                                x_left: indent_x,
                                y,
                                height: 0.0,
                                label: slot_label(act, false),
                                show_hint: false,
                            });
                        }
                        PrimOverPrimRow::Group => {
                            self.outliner.drop_target = Some(fi);
                            self.outliner.drop_on_prim = true;
                            self.outliner.drop_visual = Some(DropVisual {
                                kind: DropVisualKind::Highlight,
                                x_left: rect.left() + 4.0,
                                y: rect.top(),
                                height: rect.height(),
                                label: slot_label(SlotAction::Group(&row.name), false),
                                show_hint: false,
                            });
                        }
                    }
                }
                OlItem::Node(n) => {
                    let nid = NodeId(n);
                    if !matches!(
                        self.scene.scene_graph.get(nid).map(|x| &x.payload),
                        Some(NodePayload::SdfBody { .. })
                    ) {
                        return;
                    }
                    let parent_pos = self.scene.scene_graph.parent_body(nid).and_then(|p| {
                        let order = self.scene.scene_graph.body_child_order(p);
                        order
                            .iter()
                            .position(|&r| r == BodyChildRef::Body(nid))
                            .map(|i| (p, i))
                    });
                    match resolve_prim_over_body_header(nid, parent_pos, zone) {
                        PrimOverBodyHeader::InsertIntoOrder { body, index } => {
                            self.outliner.drop_insert = Some((body, index));
                            let y = if zone == HeaderZone::Top {
                                rect.top()
                            } else {
                                rect.bottom()
                            };
                            let act = if zone == HeaderZone::Top {
                                SlotAction::Before(&row.name)
                            } else {
                                SlotAction::After(&row.name)
                            };
                            self.outliner.drop_visual = Some(DropVisual {
                                kind: DropVisualKind::Line,
                                x_left: indent_x,
                                y,
                                height: 0.0,
                                label: slot_label(act, false),
                                show_hint: false,
                            });
                        }
                        PrimOverBodyHeader::ReparentInto { body } => {
                            self.outliner.drop_reparent = Some(SceneNodeId::Body(body));
                            self.outliner.drop_visual = Some(DropVisual {
                                kind: DropVisualKind::Highlight,
                                x_left: rect.left() + 4.0,
                                y: rect.top(),
                                height: rect.height(),
                                label: slot_label(SlotAction::Into(&row.name), false),
                                show_hint: false,
                            });
                        }
                    }
                    if !row.expanded {
                        self.ui2_ol_hover_expand_dwell(ctx, nid);
                    }
                }
            }
        } else if let Some(drag_nid) = self.outliner.dragging_body {
            if let OlItem::Node(n) = row.item {
                let target = NodeId(n);
                if target == drag_nid {
                    return;
                }
                let zone_b = classify_zone(rect.top(), rect.bottom(), ptr.y, 0.30);
                let dragged_set = self.dragged_body_set(drag_nid);
                let inside = dragged_set
                    .iter()
                    .any(|&b| self.scene.scene_graph.is_descendant_of(target, b));
                match resolve_body_over_body_header(&dragged_set, target, inside, zone_b) {
                    BodyOverBodyHeader::PlaceNextTo { before } => {
                        self.outliner.body_drop_target_nid = Some(target);
                        self.outliner.body_drop_before = before;
                        self.outliner.body_drop_nest = false;
                        self.outliner.drop_visual = Some(DropVisual {
                            kind: DropVisualKind::Line,
                            x_left: indent_x,
                            y: if before { rect.top() } else { rect.bottom() },
                            height: 0.0,
                            label: slot_label(
                                if before {
                                    SlotAction::Before(&row.name)
                                } else {
                                    SlotAction::After(&row.name)
                                },
                                false,
                            ),
                            show_hint: false,
                        });
                    }
                    BodyOverBodyHeader::NestInto => {
                        self.outliner.body_drop_target_nid = Some(target);
                        self.outliner.body_drop_nest = true;
                        self.outliner.drop_visual = Some(DropVisual {
                            kind: DropVisualKind::Highlight,
                            x_left: rect.left() + 4.0,
                            y: rect.top(),
                            height: rect.height(),
                            label: slot_label(SlotAction::Into(&row.name), false),
                            show_hint: false,
                        });
                    }
                    BodyOverBodyHeader::Rejected => {}
                }
                // Hover-to-expand applies to body drags too (v1 parity: the v1 timer
                // runs for `dragging || dragging_body`).
                if !row.expanded
                    && matches!(
                        self.scene.scene_graph.get(target).map(|x| &x.payload),
                        Some(NodePayload::SdfBody { .. })
                    )
                {
                    self.ui2_ol_hover_expand_dwell(ctx, target);
                }
            }
        }
    }
}