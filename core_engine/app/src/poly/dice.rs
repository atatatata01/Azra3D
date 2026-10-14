//! Poly Dice — even U×V grid cuts on selected/hovered faces.
//!
//! Interaction (while session active):
//! - L/R drag → vertical segment count (`segs_u`)
//! - U/D drag → horizontal segment count (`segs_v`)
//! - Shift held → lock equal U/V counts
//! - R → rotate grid 15° about face normal
//! - Enter / LMB → confirm · Esc / RMB → cancel

use std::collections::HashSet;
use std::sync::Arc;
use eframe::egui;
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyTool, ActiveTool, PolyDiceSession, PolyWorkMode};

/// Screen pixels of drag accumulation per segment step.
const SEG_DRAG_PX: f32 = 28.0;

impl AzraApp {
    /// Activate Dice: faces from selection, else hovered face. Starts a modal session.
    pub(crate) fn poly_activate_dice(&mut self) {
        if self.modeling_context != crate::ModelingContext::Polygonal {
            return;
        }
        if self.tool.current.is_poly_dice() {
            self.poly_confirm_dice();
            return;
        }
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.toast("Dice: select a poly mesh");
                return;
            }
        };
        let mut faces: HashSet<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        if faces.is_empty() {
            if let Some(h) = self.poly_sel.hovered_face {
                faces.insert(h);
            }
        }
        if faces.is_empty() {
            // Safe full-face fallback only when already in Face mode (implicit all).
            if matches!(self.poly_work_mode, PolyWorkMode::Face | PolyWorkMode::Group) {
                faces = self.poly_faces_or_all(nid).into_iter().collect();
            }
        }
        if faces.is_empty() {
            self.toast("Dice: select or hover face(s)");
            return;
        }

        // Validate body is polygonal.
        {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            if !matches!(&node.payload, NodePayload::PolygonalBody { .. }) {
                self.toast("Dice: active object is not a poly mesh");
                return;
            }
        }

        let mouse = self.viewport.viewport_mouse;
        self.set_poly_tool(PolyTool::Dice);
        self.start_operation(ActiveTool::PolyDice(PolyDiceSession {
            node_id: nid,
            faces,
            segs_u: 2,
            segs_v: 2,
            rotation: 0.0,
            start_mouse: mouse,
            last_mouse: mouse,
            dragging: false,
            drag_accum: egui::Vec2::ZERO,
        }));
        self.toast(
            "Dice: L/R=vertical segs · U/D=horizontal · Shift=equal · R=rotate 15° · Enter confirm · Esc cancel",
        );
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_dice_is_active(&self) -> bool {
        self.poly_tool == PolyTool::Dice || self.tool.current.is_poly_dice()
    }

    pub(crate) fn poly_dice_begin_drag(&mut self, mp: egui::Pos2) {
        if let Some(d) = self.tool.current.as_poly_dice_mut() {
            d.dragging = true;
            d.start_mouse = mp;
            d.last_mouse = mp;
            d.drag_accum = egui::Vec2::ZERO;
        }
    }

    pub(crate) fn poly_dice_end_drag(&mut self) {
        if let Some(d) = self.tool.current.as_poly_dice_mut() {
            d.dragging = false;
            d.drag_accum = egui::Vec2::ZERO;
        }
    }

    /// Drag axes adjust segment counts. Shift locks segs_u == segs_v.
    pub(crate) fn poly_update_dice(&mut self, mp: egui::Pos2, shift: bool) {
        let Some(d) = self.tool.current.as_poly_dice_mut() else { return };
        if !d.dragging {
            d.last_mouse = mp;
            return;
        }
        let delta = mp - d.last_mouse;
        d.last_mouse = mp;
        d.drag_accum += delta;

        let mut su = d.segs_u as i32;
        let mut sv = d.segs_v as i32;
        let mut du = 0i32;
        let mut dv = 0i32;

        // L/R → vertical segments (U)
        while d.drag_accum.x >= SEG_DRAG_PX {
            d.drag_accum.x -= SEG_DRAG_PX;
            du += 1;
        }
        while d.drag_accum.x <= -SEG_DRAG_PX {
            d.drag_accum.x += SEG_DRAG_PX;
            du -= 1;
        }
        // U/D → horizontal segments (V). Screen Y grows down → drag down increases.
        while d.drag_accum.y >= SEG_DRAG_PX {
            d.drag_accum.y -= SEG_DRAG_PX;
            dv += 1;
        }
        while d.drag_accum.y <= -SEG_DRAG_PX {
            d.drag_accum.y += SEG_DRAG_PX;
            dv -= 1;
        }

        if du == 0 && dv == 0 {
            return;
        }

        su = (su + du).clamp(1, 64);
        sv = (sv + dv).clamp(1, 64);

        if shift {
            // Equal lock: drive both from the axis that moved this frame.
            let n = if du != 0 { su } else { sv };
            d.segs_u = n as u32;
            d.segs_v = n as u32;
        } else {
            d.segs_u = su as u32;
            d.segs_v = sv as u32;
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// R — rotate grid 15° about face normal.
    pub(crate) fn poly_dice_rotate(&mut self) {
        let Some(d) = self.tool.current.as_poly_dice_mut() else { return };
        d.rotation = (d.rotation + std::f32::consts::PI / 12.0) % std::f32::consts::TAU;
        let deg = (d.rotation.to_degrees().round() as i32).rem_euclid(360);
        let (su, sv) = (d.segs_u, d.segs_v);
        self.toast(&format!("Dice rotation: {deg}°  ·  {su}×{sv}"));
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_confirm_dice(&mut self) {
        let (nid, faces, su, sv, rot) = match self.tool.current.as_poly_dice() {
            Some(d) => (
                d.node_id,
                d.faces.clone(),
                d.segs_u.max(1),
                d.segs_v.max(1),
                d.rotation,
            ),
            None => {
                self.poly_tool = PolyTool::Idle;
                self.tool.current = ActiveTool::Idle;
                return;
            }
        };

        if faces.is_empty() {
            self.poly_cancel_dice();
            return;
        }

        // Dry-run: refuse empty history when mesh would be identical (1×1 on all faces).
        let will_change = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                self.poly_cancel_dice();
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.poly_cancel_dice();
                return;
            };
            if su == 1 && sv == 1 {
                false
            } else {
                let before_f = mesh.face_count();
                let mut scratch = (**mesh).clone();
                scratch.dice_faces(&faces, su, sv, rot);
                scratch.face_count() != before_f
            }
        };
        if !will_change {
            self.toast("Dice: no cuts (need >1 segment)");
            self.poly_cancel_dice();
            return;
        }

        self.transaction(&format!("Dice {}×{}", su, sv), |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    m.dice_faces(&faces, su, sv, rot);
                    m.recompute_normals();
                }
            }
            s.poly_sel.selected_faces.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_vertices.clear();
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
            s.toast(&format!("Diced {}×{}", su, sv));
        });

        self.poly_tool = PolyTool::Idle;
        self.tool.current = ActiveTool::Idle;
    }

    pub(crate) fn poly_cancel_dice(&mut self) {
        if self.tool.current.is_poly_dice() || self.poly_tool == PolyTool::Dice {
            self.poly_tool = PolyTool::Idle;
            self.tool.current = ActiveTool::Idle;
            self.toast("Dice cancelled");
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }
}
