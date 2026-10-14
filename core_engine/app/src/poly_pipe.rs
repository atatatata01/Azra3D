//! Knife `P` — turn the drawn polyline into a PROCEDURAL PIPE (owner 2026-08-04).
//!
//! Staging A2 + A3 of `docs/planning/poly/KNIFE_LIVE_CUT_AND_PIPE.md`.
//!
//! **Why the pipe is created immediately instead of previewed with an overlay:** the Pipe
//! modifier is procedural and cheap to re-evaluate, so pressing `P` builds the real thing at
//! once and the drag then edits the real modifier. The user sees actual shaded geometry
//! rather than a 2D stand-in — and it reuses the existing "adjust last operation" lifecycle
//! (live regen on change, ONE settled undo, Esc removes) instead of inventing a preview path
//! that could disagree with the committed result.
//!
//! Target is the owner's choice (`tweak.pipe_own_object`):
//! * **own object** — a new polygonal body whose BASE MESH IS EMPTY and whose Pipe modifier
//!   generates everything. Selected as the active object on commit.
//! * **inside the current mesh** — the Pipe modifier is appended to the ACTIVE body, so the
//!   tube merges into that body's display mesh as a new island.
//!
//! Both are the same modifier; only the host body differs. Radius and segments therefore stay
//! editable forever in the modifier stack, which is what the owner ratified.

use eframe::egui;
use glam::Vec3;
use crate::{AzraApp, DirtyFlags, PolyTool, SceneItemId};
use core_math::scene_graph::{NodePayload, PolyModifier, PolyModifierType};

/// Live drag state for a just-created pipe: horizontal drag = radius, scroll = segments.
#[derive(Clone)]
pub(crate) struct PipeAdjust {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// Index of the Pipe modifier in that body's stack.
    pub(crate) mod_index: usize,
    /// True when the pipe owns its body (Esc deletes the body, not just the modifier).
    pub(crate) own_object: bool,
    pub(crate) start_mouse: egui::Pos2,
    pub(crate) start_radius: f32,
    /// Live change since the last settle.
    pub(crate) needs_settle: bool,
}

impl AzraApp {
    /// `P` while the knife has a usable polyline: build the pipe and enter the live adjust.
    pub(crate) fn poly_knife_make_pipe(&mut self) {
        if self.poly_tool != PolyTool::Knife {
            return;
        }
        let path: Vec<Vec3> = self.knife_points.clone();
        if path.len() < 2 {
            self.toast("Pipe: draw at least two knife points first");
            return;
        }
        // A path that returns to its start is a closed loop — weld the last ring to the first
        // rather than capping two coincident ends.
        let closed = path.len() >= 3
            && path[0].distance(path[path.len() - 1])
                < 1e-4 * self.viewport.camera.distance.max(0.1);

        // Radius seeds from the path's own scale so the first frame is never a degenerate
        // sliver or a blimp: 4% of the total path length.
        let mut span = 0.0f32;
        for w in path.windows(2) {
            span += w[0].distance(w[1]);
        }
        let radius = (span * 0.04).clamp(1e-3, 100.0);
        let own_object = self.tweak.pipe_own_object;
        let active = self.sel.usel.active_node().filter(|&n| self.poly_node_mesh(n).is_some());

        // Leaving the knife BEFORE the transaction: the polyline has been consumed.
        self.knife_points.clear();
        self.knife_snap_pos = None;
        self.knife_free = false;
        self.knife_free_plane = None;
        self.poly_tool = PolyTool::Idle;

        let target = match (own_object, active) {
            (false, Some(nid)) => Some(nid),
            (false, None) => {
                self.toast("Pipe: no active mesh to add the island to — made its own object");
                None
            }
            (true, _) => None,
        };

        let mut created: Option<(core_math::scene_graph::NodeId, usize, bool)> = None;
        self.transaction("Create Pipe", |s| {
            let (nid, own) = match target {
                Some(nid) => (nid, false),
                None => {
                    let root = s.scene.scene_graph.root();
                    let nid = s.scene.scene_graph.add_child(
                        root,
                        NodePayload::PolygonalBody {
                            name: "Pipe".to_string(),
                            // EMPTY base mesh: the modifier generates the whole object.
                            mesh: std::sync::Arc::new(core_math::half_edge::HalfEdgeMesh::new()),
                            source: Default::default(),
                            mesh_cache_id: None,
                            material: core_math::primitives::PbrMaterial::default(),
                            modifiers: Vec::new(),
                        },
                    );
                    (nid, true)
                }
            };
            // The path is authored in WORLD space (knife points are world). Store it in the
            // host body's LOCAL space so the pipe rides the body's transform like any other
            // geometry — CLAUDE.md: never inline body<->world math.
            let inv = s.scene.scene_graph.world_transform(nid).inverse();
            let local: Vec<Vec3> = path
                .iter()
                .map(|p| {
                    let v = inv * glam::Vec4::new(p.x, p.y, p.z, 1.0);
                    Vec3::new(v.x, v.y, v.z)
                })
                .collect();
            let mut m = PolyModifier::new(PolyModifierType::Pipe);
            m.pipe_path = local;
            m.pipe_radius = radius;
            m.pipe_closed = closed;
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                    modifiers.push(m);
                    created = Some((nid, modifiers.len() - 1, own));
                }
            }
            if own {
                s.select_after_create(&[SceneItemId::Node(nid)]);
            }
            s.poly_request_display_sync(nid);
            // A new body is STRUCTURAL; a modifier on an existing body is not.
            s.mark_dirty(if own {
                DirtyFlags::SCENE | DirtyFlags::RENDER
            } else {
                DirtyFlags::RENDER
            });
        });

        let Some((nid, mod_index, own)) = created else {
            self.toast("Pipe: could not create");
            return;
        };
        self.ui.pipe_adjust = Some(PipeAdjust {
            nid,
            mod_index,
            own_object: own,
            start_mouse: self.viewport.viewport_mouse,
            start_radius: radius,
            needs_settle: false,
        });
        self.ui2.active_tool = "Pipe".into();
        self.ui2.active_pillar = "poly".into();
        self.toast(if own {
            "Pipe: drag = radius · scroll = segments · LMB/Enter confirm · Esc cancel (own object)"
        } else {
            "Pipe: drag = radius · scroll = segments · LMB/Enter confirm · Esc cancel (island in this mesh)"
        });
    }

    pub(crate) fn pipe_adjust_active(&self) -> bool {
        self.ui.pipe_adjust.is_some()
    }

    /// Read/modify the live Pipe modifier. Returns false when it has gone (undo, deletion).
    fn pipe_with_modifier(&mut self, f: impl FnOnce(&mut PolyModifier)) -> bool {
        let Some(a) = self.ui.pipe_adjust.clone() else { return false };
        let mut ok = false;
        if let Some(node) = self.scene.scene_graph.get_mut(a.nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                if let Some(m) = modifiers.get_mut(a.mod_index) {
                    if m.mod_type == PolyModifierType::Pipe {
                        f(m);
                        ok = true;
                    }
                }
            }
        }
        if ok {
            self.poly_request_display_sync(a.nid);
            self.mark_dirty(DirtyFlags::RENDER);
            if let Some(adj) = self.ui.pipe_adjust.as_mut() {
                adj.needs_settle = true;
            }
        } else {
            self.ui.pipe_adjust = None; // modifier vanished — stand down rather than panic
        }
        ok
    }

    /// Horizontal drag sets the radius (right = fatter), scaled by the seed radius so the
    /// feel is consistent whether the pipe is 1cm or 10m.
    pub(crate) fn pipe_adjust_drag(&mut self, mouse: egui::Pos2) {
        let Some(a) = self.ui.pipe_adjust.clone() else { return };
        let dx = (mouse.x - a.start_mouse.x) as f32;
        let r = (a.start_radius * (1.0 + dx * 0.01)).clamp(1e-3, 1000.0);
        self.pipe_with_modifier(|m| m.pipe_radius = r);
    }

    /// Scroll changes the segment count.
    pub(crate) fn pipe_adjust_scroll(&mut self, delta: f32) {
        if delta.abs() < 0.5 {
            return;
        }
        let step = if delta > 0.0 { 1i32 } else { -1 };
        self.pipe_with_modifier(|m| {
            m.pipe_segments = (m.pipe_segments as i32 + step).clamp(3, 256) as u32;
        });
    }

    /// LMB / Enter — keep the pipe and settle ONE undo entry for the whole adjust.
    pub(crate) fn pipe_adjust_confirm(&mut self) {
        let Some(a) = self.ui.pipe_adjust.take() else { return };
        if a.needs_settle {
            self.transaction_snapshot_only("Adjust Pipe");
        }
        self.poly_request_display_sync(a.nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast("Pipe created — radius and segments stay editable in the modifier stack");
    }

    /// Esc — remove what `P` created. An own-object pipe deletes its body; an island pipe
    /// drops just the modifier, leaving the host mesh exactly as it was.
    pub(crate) fn pipe_adjust_cancel(&mut self) {
        let Some(a) = self.ui.pipe_adjust.take() else { return };
        self.transaction("Cancel Pipe", |s| {
            if a.own_object {
                s.scene.scene_graph.remove(a.nid);
                s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            } else {
                if let Some(node) = s.scene.scene_graph.get_mut(a.nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                        if modifiers.get(a.mod_index).map(|m| m.mod_type)
                            == Some(PolyModifierType::Pipe)
                        {
                            modifiers.remove(a.mod_index);
                        }
                    }
                }
                s.poly_request_display_sync(a.nid);
                s.mark_dirty(DirtyFlags::RENDER);
            }
        });
        self.toast("Pipe cancelled");
    }

    /// Tool Parameters — Live · Pipe.
    pub(crate) fn draw_tool_params_poly_pipe(&mut self, ui: &mut egui::Ui) {
        let Some(a) = self.ui.pipe_adjust.clone() else { return };
        ui.label(
            egui::RichText::new("Live · Pipe")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        let (mut radius, mut segments, mut caps, mut closed, pts) = {
            let m = self.scene.scene_graph.get(a.nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody { modifiers, .. } => modifiers.get(a.mod_index),
                _ => None,
            });
            match m {
                Some(m) => (m.pipe_radius, m.pipe_segments, m.pipe_caps, m.pipe_closed, m.pipe_path.len()),
                None => {
                    self.ui.pipe_adjust = None;
                    return;
                }
            }
        };
        let mut changed = false;
        changed |= ui
            .add(egui::Slider::new(&mut radius, 0.001..=5.0).logarithmic(true).text("Radius"))
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut segments, 3..=64).text("Segments"))
            .changed();
        changed |= ui.checkbox(&mut caps, "Caps").changed();
        changed |= ui.checkbox(&mut closed, "Closed loop").changed();
        if changed {
            self.pipe_with_modifier(|m| {
                m.pipe_radius = radius;
                m.pipe_segments = segments;
                m.pipe_caps = caps;
                m.pipe_closed = closed;
            });
        }
        ui.weak(format!(
            "{pts} path point(s) · {}",
            if a.own_object { "own object" } else { "island in the active mesh" }
        ));
        ui.separator();
        ui.weak("Drag = radius · scroll = segments");
        ui.horizontal(|ui| {
            if ui.button("Confirm").clicked() {
                self.pipe_adjust_confirm();
            }
            if ui.button("Cancel").clicked() {
                self.pipe_adjust_cancel();
            }
        });
    }
}
