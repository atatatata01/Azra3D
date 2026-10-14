//! Object-mode long-press LMB drag → release menu (Parent / Match TRS).
//!
//! Gesture (Object mode only, with a selection):
//! 1. Hold LMB still past [`LONG_PRESS_SECS`] → arms the link drag (box-select is cancelled).
//! 2. Drag onto another object (highlight + rubber band).
//! 3. Release on a valid target → popup: Parent / Match Location / Rotation / Scale.
//!
//! Early drag before the long-press arms cancels the gesture so marquee select still works.

use eframe::egui;
use glam::{Mat4, Quat, Vec3};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, DirtyFlags, SceneItemId};
use crate::transform::{quat_to_euler_yxz_deg, TransformResult};

/// Hold duration before link-drag arms (seconds).
const LONG_PRESS_SECS: f64 = 0.35;
/// Move past this before long-press → abandon (let box-select win).
const EARLY_DRAG_CANCEL_PX: f32 = 8.0;

/// Live long-press → drag state (while LMB is down).
#[derive(Clone, Debug)]
pub(crate) struct ObjectLinkGesture {
    pub press_t: f64,
    pub press_pos: egui::Pos2,
    pub sources: Vec<NodeId>,
    pub armed: bool,
    pub hover_target: Option<NodeId>,
}

/// Popup opened on release over a target.
#[derive(Clone, Debug)]
pub(crate) struct ObjectLinkMenu {
    pub pos: egui::Pos2,
    pub target: NodeId,
    pub sources: Vec<NodeId>,
}

impl AzraApp {
    /// Object mode across poly / SDF / NURBS contexts.
    pub(crate) fn object_link_in_object_mode(&self) -> bool {
        match self.modeling_context {
            crate::ModelingContext::Polygonal => {
                self.poly_work_mode == crate::PolyWorkMode::Object
            }
            crate::ModelingContext::Nurbs => {
                self.nurbs_work_mode == crate::NurbsWorkMode::Object
            }
            crate::ModelingContext::Sdf => self.tool.work_mode == crate::WorkMode::Object,
            _ => self.tool.work_mode == crate::WorkMode::Object,
        }
    }

    /// Nodes that can be parents / match sources (bodies + meshes, not lights/materials).
    fn object_link_is_payload(payload: &NodePayload) -> bool {
        matches!(
            payload,
            NodePayload::SdfBody { .. }
                | NodePayload::Group { .. }
                | NodePayload::SdfObject { .. }
                | NodePayload::PolygonalBody { .. }
                | NodePayload::ImportedMesh { .. }
                | NodePayload::VoxelBody { .. }
                | NodePayload::NurbsBody { .. }
                | NodePayload::NurbsSolid { .. }
        )
    }

    /// Top-most selected nodes eligible for link / match.
    pub(crate) fn object_link_collect_sources(&self) -> Vec<NodeId> {
        let sel: Vec<NodeId> = self
            .sel
            .usel
            .selected_nodes()
            .filter(|&n| {
                self.scene
                    .scene_graph
                    .get(n)
                    .map(|x| Self::object_link_is_payload(&x.payload))
                    .unwrap_or(false)
            })
            .collect();
        if sel.is_empty() {
            return Vec::new();
        }
        crate::scene_actions::top_most_nodes(&self.scene.scene_graph, &sel)
    }

    /// Ray-pick a link target under the cursor (not in sources / not under sources).
    pub(crate) fn object_link_pick_target(
        &self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        sources: &[NodeId],
    ) -> Option<NodeId> {
        let invalid = |nid: NodeId| -> bool {
            if sources.iter().any(|&s| s == nid) {
                return true;
            }
            sources
                .iter()
                .any(|&s| self.scene.scene_graph.is_descendant_of(nid, s))
        };

        // Mesh / poly / SDF mesh surface
        let (ro, rd) = self.cam_ray(mouse, rect);
        if let Some((_, _, nid)) = self.raycast_scene_meshes(ro, rd) {
            if !invalid(nid)
                && self
                    .scene
                    .scene_graph
                    .get(nid)
                    .map(|n| Self::object_link_is_payload(&n.payload))
                    .unwrap_or(false)
            {
                return Some(nid);
            }
        }
        // Analytical SDF prim → body
        if let Some(h) = self.raycast_select(mouse, rect, false) {
            let body = if h < self.scene.prim_body_map.len() {
                self.scene.prim_body_map[h].0
            } else {
                self.scene.active_body_nid
            };
            if !invalid(body)
                && self
                    .scene
                    .scene_graph
                    .get(body)
                    .map(|n| Self::object_link_is_payload(&n.payload))
                    .unwrap_or(false)
            {
                return Some(body);
            }
        }
        None
    }

    /// Tick the long-press gesture (call each frame from viewport input).
    /// Returns true when the gesture owns the pointer (box-select must not arm).
    pub(crate) fn object_link_tick(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        rect: egui::Rect,
    ) -> bool {
        // Menu open: no gesture.
        if self.ui.object_link_menu.is_some() {
            return false;
        }

        let primary_down = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
        let primary_pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        let primary_released = ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
        let now = ctx.input(|i| i.time);
        let mouse = response
            .interact_pointer_pos()
            .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
            .unwrap_or(self.viewport.viewport_mouse);

        // Escape cancels
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.ui.object_link_gesture = None;
            self.ui.object_link_menu = None;
            return false;
        }

        // Start candidate on LMB press in object mode with selection, not on gizmo.
        if primary_pressed
            && self.object_link_in_object_mode()
            && self.tool.gizmo_hovered.is_none()
            && !self.tool.suppress_gizmo_drag
            && self.tool.current.is_idle()
            && response.hovered()
            && rect.contains(mouse)
        {
            let sources = self.object_link_collect_sources();
            if !sources.is_empty() {
                self.ui.object_link_gesture = Some(ObjectLinkGesture {
                    press_t: now,
                    press_pos: mouse,
                    sources,
                    armed: false,
                    hover_target: None,
                });
            }
        }

        let Some(mut g) = self.ui.object_link_gesture.take() else {
            return false;
        };

        if !primary_down && !primary_released {
            // Lost pointer without clean release
            return false;
        }

        let dist = mouse.distance(g.press_pos);

        if !g.armed {
            // Early drag → abandon (marquee select owns the gesture)
            if dist > EARLY_DRAG_CANCEL_PX && (now - g.press_t) < LONG_PRESS_SECS {
                return false;
            }
            if (now - g.press_t) >= LONG_PRESS_SECS {
                g.armed = true;
                // Cancel box select if it started during the hold
                if self.tool.current.is_selection_box() {
                    let _ = self.tool.current.take_selection_box();
                }
                self.toast("Link / Match — drop on target");
                ctx.request_repaint();
            } else {
                // Still holding toward arm — keep gesture, block nothing yet unless near arm
                self.ui.object_link_gesture = Some(g);
                // Nearing arm: repaint for feedback
                ctx.request_repaint_after(std::time::Duration::from_millis(40));
                return false;
            }
        }

        // Armed: track hover target
        g.hover_target = self.object_link_pick_target(mouse, rect, &g.sources);

        if primary_released {
            if let Some(target) = g.hover_target {
                self.ui.object_link_menu = Some(ObjectLinkMenu {
                    pos: mouse,
                    target,
                    sources: g.sources,
                });
                // Swallow this click so object click-select does not re-pick
                self.tool.suppress_gizmo_drag = true;
            }
            // else cancel silently
            return true; // owned the release
        }

        self.ui.object_link_gesture = Some(g);
        true // owns pointer while armed
    }

    /// Draw rubber-band + target highlight while armed.
    pub(crate) fn object_link_draw_overlay(&self, ctx: &egui::Context, rect: egui::Rect) {
        let Some(g) = &self.ui.object_link_gesture else {
            return;
        };
        if !g.armed {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("object_link_overlay"),
        ));
        let cur = ctx
            .input(|i| i.pointer.hover_pos())
            .unwrap_or(g.press_pos);
        let color = if g.hover_target.is_some() {
            egui::Color32::from_rgb(120, 200, 140)
        } else {
            egui::Color32::from_rgb(220, 180, 80)
        };
        painter.line_segment(
            [g.press_pos, cur],
            egui::Stroke::new(2.0, color.gamma_multiply(0.9)),
        );
        painter.circle_filled(g.press_pos, 4.0, color);
        painter.circle_stroke(cur, 8.0, egui::Stroke::new(2.0, color));
        if let Some(tid) = g.hover_target {
            let name = self
                .scene
                .scene_graph
                .get(tid)
                .map(|n| n.payload.display_name().to_string())
                .unwrap_or_else(|| "target".into());
            painter.text(
                cur + egui::vec2(12.0, -10.0),
                egui::Align2::LEFT_BOTTOM,
                format!("→ {name}"),
                egui::FontId::proportional(12.0),
                color,
            );
        }
        let _ = rect;
    }

    /// Popup menu after a successful drop.
    pub(crate) fn object_link_draw_menu(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.ui.object_link_menu.clone() else {
            return;
        };
        let target_name = self
            .scene
            .scene_graph
            .get(menu.target)
            .map(|n| n.payload.display_name().to_string())
            .unwrap_or_else(|| "Target".into());

        let mut close = false;
        let mut act: Option<ObjectLinkAction> = None;

        egui::Area::new(egui::Id::new("object_link_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.pos)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_min_width(200.0);
                        ui.label(
                            egui::RichText::new(format!("→ {target_name}"))
                                .strong()
                                .size(12.0),
                        );
                        ui.separator();
                        if ui
                            .button("Parent Selected to Target")
                            .on_hover_text("Make selected object(s) children of the target")
                            .clicked()
                        {
                            act = Some(ObjectLinkAction::Parent);
                        }
                        ui.separator();
                        if ui
                            .button("Match Location")
                            .on_hover_text("Copy target world position onto the selection")
                            .clicked()
                        {
                            act = Some(ObjectLinkAction::MatchLocation);
                        }
                        if ui
                            .button("Match Rotation")
                            .on_hover_text("Copy target world rotation onto the selection")
                            .clicked()
                        {
                            act = Some(ObjectLinkAction::MatchRotation);
                        }
                        if ui
                            .button("Match Scale")
                            .on_hover_text("Copy target world scale onto the selection")
                            .clicked()
                        {
                            act = Some(ObjectLinkAction::MatchScale);
                        }
                        ui.separator();
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
            });

        // Click outside closes
        if ctx.input(|i| i.pointer.any_click()) {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                // Rough: if click far from menu origin, close (Area handles internal)
                let d = p.distance(menu.pos);
                if d > 220.0 {
                    close = true;
                }
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }

        if let Some(a) = act {
            self.object_link_apply(a, &menu);
            self.ui.object_link_menu = None;
        } else if close {
            self.ui.object_link_menu = None;
        }
    }

    fn object_link_apply(&mut self, act: ObjectLinkAction, menu: &ObjectLinkMenu) {
        let sources: Vec<NodeId> = menu
            .sources
            .iter()
            .copied()
            .filter(|&s| s != menu.target)
            .collect();
        if sources.is_empty() {
            self.toast("Nothing to apply");
            return;
        }
        match act {
            ObjectLinkAction::Parent => {
                let target = menu.target;
                let n = sources.len();
                self.transaction("Parent to target", |s| {
                    let mut applied = 0usize;
                    for &child in &sources {
                        if s.reparent_body_node(child, target)
                            == crate::scene_actions::ReparentBodyResult::Applied
                        {
                            applied += 1;
                        }
                    }
                    if applied > 0 {
                        s.ol_set_expanded(target, true);
                        s.clear_selection_unified();
                        let ids: Vec<SceneItemId> =
                            sources.iter().map(|&n| SceneItemId::Node(n)).collect();
                        s.select_after_create(&ids);
                        let unit = if applied == 1 { "object" } else { "objects" };
                        s.toast(&format!("Parented {applied} {unit}"));
                    } else {
                        s.toast("Could not parent (cycle or invalid)");
                    }
                });
                let _ = n;
            }
            ObjectLinkAction::MatchLocation => {
                self.object_link_match_trs(&sources, menu.target, true, false, false);
            }
            ObjectLinkAction::MatchRotation => {
                self.object_link_match_trs(&sources, menu.target, false, true, false);
            }
            ObjectLinkAction::MatchScale => {
                self.object_link_match_trs(&sources, menu.target, false, false, true);
            }
        }
    }

    /// World TRS of a node (pos, rot euler deg, scale).
    fn object_link_world_trs(&self, nid: NodeId) -> Option<(Vec3, Vec3, Vec3)> {
        // Prefer effective body matrix for SDF (includes parent SdfObject placement).
        if let Some(m) = self.scene.scene_graph.effective_body_transform(nid) {
            let (s, r, t) = m.to_scale_rotation_translation();
            if s.is_finite() && r.is_finite() && t.is_finite() {
                return Some((t, quat_to_euler_yxz_deg(r), s));
            }
        }
        let m = self.scene.scene_graph.world_transform(nid);
        let (s, r, t) = m.to_scale_rotation_translation();
        if !s.is_finite() || !r.is_finite() || !t.is_finite() {
            return None;
        }
        Some((t, quat_to_euler_yxz_deg(r), s))
    }

    fn object_link_match_trs(
        &mut self,
        sources: &[NodeId],
        target: NodeId,
        loc: bool,
        rot: bool,
        scale: bool,
    ) {
        let Some((tp, tr, ts)) = self.object_link_world_trs(target) else {
            self.toast("Target has no transform");
            return;
        };
        let label = if loc && !rot && !scale {
            "Match Location"
        } else if rot && !loc && !scale {
            "Match Rotation"
        } else if scale && !loc && !rot {
            "Match Scale"
        } else {
            "Match Transform"
        };
        let srcs = sources.to_vec();
        self.transaction(label, |s| {
            for &nid in &srcs {
                let Some((cp, cr, cs)) = s.object_link_world_trs(nid) else {
                    continue;
                };
                let new_p = if loc { tp } else { cp };
                let new_r = if rot { tr } else { cr };
                let new_s = if scale { ts } else { cs };
                s.object_link_set_world_trs(nid, new_p, new_r, new_s);
            }
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast(label);
        });
    }

    /// Write world TRS onto a node (local for mesh; body fields for SdfBody).
    fn object_link_set_world_trs(&mut self, nid: NodeId, pos: Vec3, rot_euler: Vec3, scale: Vec3) {
        let is_sdf = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. })
        );
        if is_sdf {
            // apply_to_scene_node treats position/rotation/scale as body origin/rot/scale.
            self.apply_to_scene_node(
                nid,
                &TransformResult {
                    position: pos,
                    rotation: rot_euler,
                    scale,
                    elongation: Vec3::ZERO,
                },
            );
            return;
        }
        // Mesh / group / etc.: convert desired world → local under current parent.
        let parent = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| n.parent)
            .unwrap_or_else(|| self.scene.scene_graph.root());
        let parent_world = self.scene.scene_graph.world_transform(parent);
        let rot = Quat::from_euler(
            glam::EulerRot::YXZ,
            rot_euler.y.to_radians(),
            rot_euler.x.to_radians(),
            rot_euler.z.to_radians(),
        );
        let world = Mat4::from_scale_rotation_translation(scale, rot, pos);
        let local = parent_world.inverse() * world;
        let (ls, lr, lt) = local.to_scale_rotation_translation();
        if ls.is_finite() && lr.is_finite() && lt.is_finite() {
            self.apply_to_scene_node(
                nid,
                &TransformResult {
                    position: lt,
                    rotation: quat_to_euler_yxz_deg(lr),
                    scale: ls,
                    elongation: Vec3::ZERO,
                },
            );
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ObjectLinkAction {
    Parent,
    MatchLocation,
    MatchRotation,
    MatchScale,
}
