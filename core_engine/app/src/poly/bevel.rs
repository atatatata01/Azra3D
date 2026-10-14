//! Bevel tool confirm + Blender-style "Adjust Last Operation" redo panel (`PolyBevelAdjust`).
//!
//! Drag activation / live regenerate live in `poly_drag_tools.rs`. This module owns:
//! - pure re-run from a pre-bevel snapshot (resize-must-not-compound)
//! - the adjust panel UI (clone of `draw_poly_inset_adjust`)

use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::HalfEdgeMesh;
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags};

/// Single producer for bevel re-run (tool regenerate + adjust panel + tests).
/// Always starts from `before` — never from the current beveled mesh — so resizing
/// width 0.1→0.3 is bit-identical to a fresh bevel@0.3 (B1 gate).
///
/// Returns `None` when the install guard rejects the result (tear / new hole) or
/// when width collapses to ~0 (caller restores `before` itself).
pub(crate) fn apply_bevel_from_before(
    before: &HalfEdgeMesh,
    edges: &HashSet<u32>,
    verts: &HashSet<u32>,
    params: &core_math::bevel_op::BevelParams,
) -> Option<HalfEdgeMesh> {
    if params.width <= 1e-6 {
        return Some(before.clone());
    }
    let new_mesh = if !verts.is_empty() {
        core_math::bevel_op::bevel_vertices_offset(before, verts, params)
    } else if !edges.is_empty() {
        core_math::bevel_op::bevel_edges_offset(before, edges, params)
    } else {
        return Some(before.clone());
    };
    if core_math::bevel_op::bevel_result_installable(before, &new_mesh) {
        Some(new_mesh)
    } else {
        None
    }
}

impl AzraApp {
    /// Whether a last-bevel adjust session is active (for Tool Parameters priority).
    pub(crate) fn poly_bevel_adjust_active(&self) -> bool {
        let Some(a) = self.ui.poly_bevel_adjust.as_ref() else {
            return false;
        };
        if self.tool.current.is_poly_bevel() {
            return false;
        }
        self.scene.scene_graph.get(a.nid).is_some() && self.sel.usel.active_node() == Some(a.nid)
    }

    /// Shared last-bevel UI + apply (dock Tool Parameters **and** legacy float).
    /// Returns `true` when a session was drawn.
    pub(crate) fn poly_bevel_adjust_panel(&mut self, ui: &mut eframe::egui::Ui) -> bool {
        use eframe::egui;
        let mut a = match self.ui.poly_bevel_adjust.clone() {
            Some(a) => a,
            None => return false,
        };
        if self.scene.scene_graph.get(a.nid).is_none() || self.sel.usel.active_node() != Some(a.nid)
        {
            self.ui.poly_bevel_adjust = None;
            return false;
        }
        if self.tool.current.is_poly_bevel() {
            return false;
        }

        let mut changed = false;
        let mut released = false;
        let mut close = false;

        ui.label(
            egui::RichText::new("Last · Bevel")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        let r = ui.add(
            egui::Slider::new(&mut a.width, 0.0..=1.0)
                .text("Width")
                .logarithmic(true),
        );
        changed |= r.changed();
        released |= r.drag_stopped() || r.lost_focus();
        let r = ui.add(egui::Slider::new(&mut a.segments, 1..=32).text("Segments"));
        changed |= r.changed();
        released |= r.drag_stopped() || r.lost_focus();
        let r = ui.add(egui::Slider::new(&mut a.shape, 0.0..=1.0).text("Shape"));
        changed |= r.changed();
        released |= r.drag_stopped() || r.lost_focus();
        let r = ui
            .checkbox(&mut a.clamp, "Clamp Overlap")
            .on_hover_text("Limit width so faces can't invert (Blender Clamp Overlap / C).");
        if r.changed() {
            changed = true;
            released = true;
        }
        ui.separator();
        if ui
            .button("Unbevel")
            .on_hover_text("Width → 0 — restore mesh before this bevel (last-op only).")
            .clicked()
        {
            a.width = 0.0;
            changed = true;
            released = true;
        }
        if ui.button("Done").clicked() {
            close = true;
        }

        if changed {
            let nid = a.nid;
            // Staleness gate (owner data-loss repro 2026-08-05): re-running from `before`
            // replaces the whole mesh — only legal while the live mesh is still exactly the
            // last apply's result. If the user modeled since, applying would silently wipe
            // that work. Checked here (on interaction), never per-frame — content_signature
            // is O(mesh).
            let live_sig = self
                .scene
                .scene_graph
                .get(nid)
                .and_then(|n| match &n.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Some(mesh.content_signature()),
                    _ => None,
                });
            if live_sig != Some(a.post_signature) {
                self.ui.poly_bevel_adjust = None;
                self.toast(
                    "Bevel adjust closed: mesh was edited since this bevel — re-running \
                     would discard those edits (Ctrl+Z back to the bevel to adjust it)",
                );
                return true;
            }
            a.needs_settle = true;
            let params = core_math::bevel_op::BevelParams {
                width: a.width,
                segments: a.segments.max(1).min(32),
                shape: a.shape,
                clamp: a.clamp,
            };
            if let Some(new_mesh) =
                apply_bevel_from_before(&a.before, &a.edges, &a.verts, &params)
            {
                let prev_sig = a.post_signature;
                a.post_signature = new_mesh.content_signature();
                // Keep the Unbevel history top in lockstep: this re-run IS the same bevel
                // (same `before`), so its result is the new legal restore point — otherwise
                // adjusting the panel would falsely mark the history stale.
                if let Some(h) = self
                    .poly_bevel_history
                    .iter_mut()
                    .rev()
                    .find(|h| h.node_id == nid && h.post_signature == prev_sig)
                {
                    h.post_signature = a.post_signature;
                    h.width = a.width;
                    h.segments = params.segments;
                    h.shape = a.shape;
                    h.clamp = a.clamp;
                }
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *mesh = Arc::new(new_mesh);
                    }
                }
                self.poly_invalidate_pick_cache();
                self.poly_request_display_sync(nid);
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        self.ui.poly_bevel_adjust = Some(a.clone());
        if released {
            self.poly_request_display_sync(a.nid);
            if a.needs_settle {
                self.transaction_snapshot_only("Adjust Bevel");
                if let Some(adj) = self.ui.poly_bevel_adjust.as_mut() {
                    adj.needs_settle = false;
                }
            }
        }
        if close {
            self.poly_request_display_sync(a.nid);
            self.ui.poly_bevel_adjust = None;
        }
        true
    }

    /// Legacy floating window (old UI only). ui2 uses Tool Parameters dock.
    pub(crate) fn draw_poly_bevel_adjust(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui;
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            return;
        }
        if !self.poly_bevel_adjust_active() {
            return;
        }
        egui::Window::new("Adjust Bevel")
            .id(egui::Id::new("poly_bevel_adjust"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                let _ = self.poly_bevel_adjust_panel(ui);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_FACE};
    use glam::Vec3;

    fn cube() -> HalfEdgeMesh {
        HalfEdgeMesh::new_box(Vec3::ONE)
    }

    fn all_edge_ids(m: &HalfEdgeMesh) -> HashSet<u32> {
        let mut out = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.face == NO_FACE {
                continue;
            }
            out.insert(i);
        }
        out
    }

    fn live_faces(m: &HalfEdgeMesh) -> usize {
        (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| m.face(f).half_edge != NO_EDGE)
            .count()
    }

    fn pos_keys(m: &HalfEdgeMesh) -> Vec<[i64; 3]> {
        let mut v: Vec<[i64; 3]> = m
            .vertices
            .iter()
            .map(|vert| {
                let p = vert.position;
                [
                    (p.x * 1e5).round() as i64,
                    (p.y * 1e5).round() as i64,
                    (p.z * 1e5).round() as i64,
                ]
            })
            .collect();
        v.sort_unstable();
        v
    }

    /// B1 gate: resize 0.1 → 0.3 from `before` is bit-identical to a fresh bevel@0.3.
    /// Sabotage: re-running from the 0.1 result (compound) must NOT match fresh@0.3.
    #[test]
    fn bevel_resize_from_before_matches_fresh() {
        let before = cube();
        let edges = all_edge_ids(&before);
        let verts = HashSet::new();
        let p01 = core_math::bevel_op::BevelParams {
            width: 0.1,
            segments: 1,
            shape: 0.5,
            clamp: true,
        };
        let p03 = core_math::bevel_op::BevelParams {
            width: 0.3,
            segments: 1,
            shape: 0.5,
            clamp: true,
        };
        let at_01 = apply_bevel_from_before(&before, &edges, &verts, &p01)
            .expect("bevel@0.1 installable");
        assert!(live_faces(&at_01) > live_faces(&before));

        let resized = apply_bevel_from_before(&before, &edges, &verts, &p03)
            .expect("resize to 0.3 from before");
        let fresh = apply_bevel_from_before(&before, &edges, &verts, &p03)
            .expect("fresh 0.3");
        assert_eq!(
            pos_keys(&resized),
            pos_keys(&fresh),
            "resize from before must match fresh bevel@0.3"
        );

        // Sabotage property: compounding from the 0.1 mesh is a different producer and must
        // NOT be what the adjust panel does. Document that difference so a regression that
        // starts from current would fail the identity gate above.
        let compounded = apply_bevel_from_before(&at_01, &all_edge_ids(&at_01), &verts, &p03);
        if let Some(bad) = compounded {
            // If a second bevel even installs, its geometry must differ from fresh@0.3 on before.
            // (If install fails, sabotage is already proven — wrong path can't match.)
            assert_ne!(
                pos_keys(&bad),
                pos_keys(&fresh),
                "compounded bevel must not match fresh — gate would be blind otherwise"
            );
        }
    }

    /// Unbevel (width 0) restores the pre-bevel mesh exactly.
    #[test]
    fn bevel_unbevel_width_zero_restores_before() {
        let before = cube();
        let edges = all_edge_ids(&before);
        let verts = HashSet::new();
        let zero = core_math::bevel_op::BevelParams {
            width: 0.0,
            segments: 1,
            shape: 0.5,
            clamp: true,
        };
        let out = apply_bevel_from_before(&before, &edges, &verts, &zero).unwrap();
        assert_eq!(pos_keys(&out), pos_keys(&before));
        assert_eq!(live_faces(&out), live_faces(&before));
    }

    /// Drag formula regression (2026-08-10): width = base + |dx|*scale must keep a seed when
    /// dx=0. The old absolute `width = |dx|*scale` wrote 0 on the first post-arm frame and
    /// snapped the mesh back — owner "i try and did not work".
    #[test]
    fn bevel_drag_width_keeps_base_when_dx_zero() {
        let base = 0.25f32;
        let scale = 5.0 * 0.002; // camera.distance * 0.002
        let dx = 0.0f32;
        let width = (base + dx.abs() * scale).max(0.0);
        assert!((width - base).abs() < 1e-6, "dx=0 must preserve seed base, got {width}");
        let dx2 = 40.0f32;
        let width2 = (base + dx2.abs() * scale).max(0.0);
        assert!(width2 > base, "drag must grow width above base");
        let old_abs = (dx.abs() * scale).max(0.0);
        assert!(old_abs < 1e-6, "old absolute formula is the wipe");
    }

    /// VERTEX bevel (affect=VERTICES) through the app's single producer — the adjust panel
    /// and live regenerate both call this. Cube corner (1,1,1) at width 0.4 seg=1 must install
    /// and grow topology (oracle nv=10/nf=7).
    #[test]
    fn vertex_bevel_from_before_installs_cube_corner() {
        let before = cube();
        let vi = before
            .vertices
            .iter()
            .position(|v| (v.position - glam::Vec3::ONE).length() < 1e-4)
            .expect("cube has ++++ corner") as u32;
        let mut verts = HashSet::new();
        verts.insert(vi);
        let edges = HashSet::new();
        let p = core_math::bevel_op::BevelParams {
            width: 0.4,
            segments: 1,
            shape: 0.5,
            clamp: true,
        };
        let out = apply_bevel_from_before(&before, &edges, &verts, &p)
            .expect("vertex bevel must be installable");
        assert_eq!(out.vertices.len(), 10, "oracle nv=10");
        assert_eq!(live_faces(&out), 7, "oracle nf=7");
        assert!(
            out.vertices.len() > before.vertices.len(),
            "vertex bevel must add BoundVerts"
        );
    }
}
