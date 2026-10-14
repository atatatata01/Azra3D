use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::{HalfEdgeId, FaceId};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, PolyWorkMode, DirtyFlags};

impl AzraApp {
    /// Gather the vertex ids the current selection touches, in ANY poly mode: Vertex = the selected
    /// verts; Edge = both endpoints of every selected edge; Face = every vertex of every selected face.
    /// Dead/stale edge slots are skipped. Deterministic (sorted) so the op is repeatable.
    fn poly_tosphere_verts(&self, nid: core_math::scene_graph::NodeId) -> Vec<u32> {
        let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh,
            _ => return Vec::new(),
        };
        // Modo implicit full selection: an EMPTY mode selection spherizes the WHOLE mesh (all live
        // verts). To Sphere is a SAFE op — it only moves existing verts, never removes geometry — so
        // empty=all via the shared `poly_verts_or_all` resolver. Object mode has no sub-selection and
        // stays a no-op (To Sphere is an edit-mode op).
        let mode_empty = match self.poly_work_mode {
            PolyWorkMode::Vertex => self.poly_sel.selected_vertices.is_empty(),
            PolyWorkMode::Edge => self.poly_sel.selected_edges.is_empty(),
            PolyWorkMode::Face | PolyWorkMode::Group => self.poly_sel.selected_faces.is_empty(),
            PolyWorkMode::Object => return Vec::new(),
        };
        if mode_empty { return self.poly_verts_or_all(nid); }
        let mut set: HashSet<u32> = HashSet::new();
        match self.poly_work_mode {
            PolyWorkMode::Vertex => set.extend(self.poly_sel.selected_vertices.iter().copied()),
            PolyWorkMode::Edge => {
                for &e in &self.poly_sel.selected_edges {
                    let h = HalfEdgeId(e);
                    if !mesh.is_edge_live(h) { continue; }
                    set.insert(mesh.he_source(h).0);
                    set.insert(mesh.he(h).vertex.0);
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                for &f in &self.poly_sel.selected_faces {
                    if (f as usize) >= mesh.faces.len() { continue; }
                    for v in mesh.face_vertices(FaceId(f)) { set.insert(v.0); }
                }
            }
            PolyWorkMode::Object => {}
        }
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// To Sphere (Blender `bpy.ops.transform.tosphere`): push the selection toward a sphere (center =
    /// mean of the verts, radius = mean distance). Works in EVERY poly edit mode — Vertex/Edge/Face all
    /// resolve to their vertex set. Opens the "Adjust To Sphere" redo panel with a Factor slider (0..1)
    /// so the strength is tweakable after the fact (mirrors Circle/Inset). Pure position edit → ids
    /// preserved → the selection stays put and chains. Uses the transaction + redo-panel path (not
    /// poly_op) so a live re-run never stacks.
    pub(crate) fn poly_to_sphere(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let verts = self.poly_tosphere_verts(nid);
        if verts.len() < 2 {
            self.toast("To Sphere: select 2+ vertices / edges / faces");
            return;
        }
        let before = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return,
        };
        // Dry-run at factor 1.0: to_sphere returns false when the selection has no spread (all verts at
        // one point) — refuse with NO undo entry (transaction() pushes unconditionally).
        let set: HashSet<u32> = verts.iter().copied().collect();
        let mut next = (*before).clone();
        if !next.to_sphere(&set, 1.0) {
            self.toast("To Sphere: the selection has no spread to spherize");
            return;
        }
        self.transaction("To Sphere", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = Arc::new(next);
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.ui.poly_tosphere_adjust = Some(crate::types::PolyToSphereAdjust {
            nid, verts, before, factor: 1.0, needs_settle: false,
        });
    }

    /// "Adjust Last Operation" (F9) redo panel for To Sphere — a Factor slider that re-runs the spherize
    /// from the PRE-op snapshot each frame (so tweaking never stacks) and settles into ONE undo entry on
    pub(crate) fn poly_tosphere_adjust_active(&self) -> bool {
        let Some(a) = self.ui.poly_tosphere_adjust.as_ref() else {
            return false;
        };
        self.scene.scene_graph.get(a.nid).is_some() && self.sel.usel.active_node() == Some(a.nid)
    }

    /// Shared last-to-sphere UI + apply (Tool Parameters dock + legacy float).
    pub(crate) fn poly_tosphere_adjust_panel(&mut self, ui: &mut eframe::egui::Ui) -> bool {
        use eframe::egui;
        let mut a = match self.ui.poly_tosphere_adjust.clone() {
            Some(a) => a,
            None => return false,
        };
        if self.scene.scene_graph.get(a.nid).is_none() || self.sel.usel.active_node() != Some(a.nid)
        {
            self.ui.poly_tosphere_adjust = None;
            return false;
        }

        let mut changed = false;
        let mut released = false;
        let mut close = false;
        ui.label(
            egui::RichText::new("Last · To Sphere")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        let resp = ui
            .add(egui::Slider::new(&mut a.factor, 0.0..=1.0).text("Factor"))
            .on_hover_text(
                "Blend toward the best-fit sphere.\n0 = original shape, 1 = full sphere.",
            );
        changed |= resp.changed();
        released |= resp.drag_stopped() || resp.lost_focus();
        ui.separator();
        if ui.button("Done").clicked() {
            close = true;
        }

        if changed {
            a.needs_settle = true;
            let nid = a.nid;
            let set: HashSet<u32> = a.verts.iter().copied().collect();
            let mut m = (*a.before).clone();
            m.to_sphere(&set, a.factor);
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = Arc::new(m);
                }
            }
            self.poly_invalidate_pick_cache();
            self.poly_request_display_sync(nid);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        self.ui.poly_tosphere_adjust = Some(a.clone());
        if released {
            self.poly_request_display_sync(a.nid);
            if a.needs_settle {
                self.transaction_snapshot_only("Adjust To Sphere");
                if let Some(adj) = self.ui.poly_tosphere_adjust.as_mut() {
                    adj.needs_settle = false;
                }
            }
        }
        if close {
            self.poly_request_display_sync(a.nid);
            self.ui.poly_tosphere_adjust = None;
        }
        true
    }

    /// Legacy floating window (old UI only).
    pub(crate) fn draw_poly_tosphere_adjust(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui;
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            return;
        }
        if !self.poly_tosphere_adjust_active() {
            return;
        }
        egui::Window::new("Adjust To Sphere")
            .id(egui::Id::new("poly_tosphere_adjust"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                let _ = self.poly_tosphere_adjust_panel(ui);
            });
    }
}
