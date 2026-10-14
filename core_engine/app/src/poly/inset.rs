use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::{FaceId, HalfEdgeMesh};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, DirtyFlags};

/// What one [`apply_inset`] produced. RUNTIME-ONLY.
#[derive(Clone, Debug, Default)]
pub(crate) struct InsetApplied {
    /// The inset ISLAND's face ids (rim quads excluded — Blender leaves the island selected).
    pub inner: Vec<u32>,
    /// Vertex ids the inset ALLOCATED (one per region-boundary vertex, per set). Bevel Smart
    /// Select's tagging derives the created edges from these — see
    /// [`crate::poly::op_tags::edges_incident_to_verts`]. Reported explicitly at emission rather
    /// than diffed from positions: at thickness 0 (the interactive drag builds topology there)
    /// every new vertex is COINCIDENT with the original it was split from, so a position diff
    /// cannot see them at all (BEVEL.md §6 — position diffs fail for ops that move vertices).
    pub new_verts: Vec<u32>,
}

/// THE single inset producer: apply `thickness` / `depth` to `faces` in Region or Individual mode.
///
/// * Region (Blender's `I` DEFAULT) = ONE set, so only the selection's outer boundary is inset and the
///   shared interior edges survive.
/// * Individual (`I,I`) = one single-face set per face; every edge is then a boundary edge, which is
///   exactly a per-face inset.
///
/// Both go through `inset_region`, so there is ONE even-offset rule and the (vertex, displacement)
/// pairing is by IDENTITY — never by ring order (POLY_POLISH_PLAN §8: `face_vertices` is destination-
/// ordered, so zipping against it twists the inset).
pub(crate) fn apply_inset(mesh: &mut HalfEdgeMesh, faces: &[u32], thickness: f32, depth: f32,
                          individual: bool) -> InsetApplied {
    let sets: Vec<HashSet<FaceId>> = if individual || faces.len() == 1 {
        faces.iter().map(|&f| std::iter::once(FaceId(f)).collect()).collect()
    } else {
        vec![faces.iter().map(|&f| FaceId(f)).collect()]
    };
    let mut out = InsetApplied::default();
    for set in &sets {
        let res = mesh.inset_region(set, thickness, depth);
        for f in &res.inner { out.inner.push(f.0); }
        // `boundary` pairs each NEW ring vertex with its displacement — the new ids are its keys.
        for &(v, _d) in &res.boundary { out.new_verts.push(v.0); }
    }
    out
}

impl AzraApp {
    /// Record what an INSET created into the Bevel Smart Select sidecar (owner 2026-08-05:
    /// "inset: tag all geo created by inset should be tagged not to bevel").
    ///
    /// `old` is the mesh as it stood BEFORE the inset — the numbering `cache.poly_custom_data`
    /// is currently keyed by. `inset_region` deletes the region faces and re-creates them, so
    /// the freed half-edge ids get recycled: carrying the base by id would leave an earlier
    /// operation's tags pointing at strangers. The sidecar is therefore re-keyed by POSITION
    /// first and only then are this inset's own edges stamped (same contract as the bevel and
    /// boolean installs).
    ///
    /// The created set is [`crate::poly::op_tags::edges_incident_to_verts`] over the vertices the
    /// inset allocated — see that function for why the region's OUTER boundary edge is
    /// deliberately excluded.
    pub(crate) fn poly_inset_stamp_tags(
        &mut self,
        nid: NodeId,
        old: &HalfEdgeMesh,
        new_verts: &[u32],
    ) {
        let Some(new) = self.poly_node_mesh(nid) else { return };
        let created = crate::poly::op_tags::edges_incident_to_verts(&new, new_verts);
        let carried = crate::poly::op_tags::carry_op_tags(
            self.cache.poly_custom_data.get(&nid),
            old,
            &new,
            &created,
        );
        match carried {
            Some(cd) => {
                self.cache.poly_custom_data.insert(nid, cd);
            }
            None => {
                self.cache.poly_custom_data.remove(&nid);
            }
        }
    }

    /// Inset the selected faces (one-shot: menu / command palette). `poly_inset_individual` picks the
    /// Blender mode — false (DEFAULT, `I`) = REGION / "group faces", true (`I,I`) = INDIVIDUAL.
    ///
    /// Leaves the inset island SELECTED (the user almost always extrudes straight after an inset, so
    /// dropping the selection would force a re-pick) and arms the Blender-style redo panel so the
    /// settings stay adjustable after the op.
    pub(crate) fn poly_inset_faces(&mut self, amount: f32) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Modo implicit full selection: empty selection insets EVERY face (SAFE — adds inset ring
        // geometry, never removes it) via the shared `poly_faces_or_all` resolver.
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() { return; }
        let individual = self.poly_inset_individual;
        let before = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return,
        };
        let label = if individual { "Inset Individual" } else { "Inset Region" };
        if self.poly_apply_inset_settled(nid, &faces, amount, 0.0, individual, label) {
            self.ui.poly_inset_adjust = Some(crate::types::PolyInsetAdjust {
                nid, faces, before, thickness: amount, depth: 0.0, individual, needs_settle: false,
            });
        }
    }

    /// Run an inset inside ONE transaction (undo) and leave the inset island selected.
    ///
    /// Dry-runs on a scratch clone first (circle.rs pattern). `transaction()` snapshots and
    /// pushes UNCONDITIONALLY — opening it around a refuse would silently eat the next Ctrl+Z
    /// (Phase 0 honesty / POLY_POLISH_PLAN circle note).
    fn poly_apply_inset_settled(&mut self, nid: NodeId, faces: &[u32], thickness: f32, depth: f32,
                                individual: bool, label: &str) -> bool {
        // Dry-run: refuse without touching history if the op produces no island.
        let dry_ok = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return false };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return false };
            let mut scratch = (**mesh).clone();
            !apply_inset(&mut scratch, faces, thickness, depth, individual).inner.is_empty()
        };
        if !dry_ok {
            return false;
        }

        // SMART SELECT: hold the PRE-inset mesh so the sidecar can be re-keyed across the
        // rebuild before this inset's own edges are stamped. The Arc clone also forces
        // `Arc::make_mut` below to copy-on-write instead of mutating it out from under us.
        let old = self.poly_node_mesh(nid);
        let mut applied = InsetApplied::default();
        self.transaction(label, |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    applied = apply_inset(m, faces, thickness, depth, individual);
                }
            }
            if !applied.inner.is_empty() {
                if let Some(old) = old.as_deref() {
                    s.poly_inset_stamp_tags(nid, old, &applied.new_verts);
                }
                s.poly_sel.selected_faces.clear();
                for &f in &applied.inner { s.poly_sel.selected_faces.insert(f); }
                s.poly_invalidate_pick_cache();
                s.poly_request_display_sync(nid);
                s.mark_dirty(DirtyFlags::RENDER);
            }
        });
        !applied.inner.is_empty()
    }

    /// Blender's "Adjust Last Operation" (F9) redo panel for Inset — it STAYS after confirm so Thickness
    /// / Depth / Region-vs-Individual can be tweaked with a LIVE re-run. Every change re-runs from the
    /// PRE-inset snapshot (so tweaking never stacks insets) and settles into ONE undo entry on release.
    pub(crate) fn poly_inset_adjust_active(&self) -> bool {
        let Some(a) = self.ui.poly_inset_adjust.as_ref() else {
            return false;
        };
        if self.tool.current.is_poly_inset() {
            return false;
        }
        self.scene.scene_graph.get(a.nid).is_some() && self.sel.usel.active_node() == Some(a.nid)
    }

    /// Shared last-inset UI + apply (Tool Parameters dock + legacy float).
    pub(crate) fn poly_inset_adjust_panel(&mut self, ui: &mut eframe::egui::Ui) -> bool {
        use eframe::egui;
        let mut a = match self.ui.poly_inset_adjust.clone() {
            Some(a) => a,
            None => return false,
        };
        if self.scene.scene_graph.get(a.nid).is_none() || self.sel.usel.active_node() != Some(a.nid)
        {
            self.ui.poly_inset_adjust = None;
            return false;
        }
        if self.tool.current.is_poly_inset() {
            return false;
        }

        let mut changed = false;
        let mut released = false;
        let mut close = false;
        ui.label(
            egui::RichText::new("Last · Inset")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        // Negative thickness = outward rim (body corners FIXED; only new ring slides);
        // positive = classic inward inset.
        let r = ui.add(egui::Slider::new(&mut a.thickness, -2.0..=2.0).text("Thickness"));
        changed |= r.changed();
        released |= r.drag_stopped() || r.lost_focus();
        let r = ui.add(egui::Slider::new(&mut a.depth, -2.0..=2.0).text("Depth"));
        changed |= r.changed();
        released |= r.drag_stopped() || r.lost_focus();
        let r = ui.checkbox(&mut a.individual, "Individual").on_hover_text(
            "Off = Region (Blender's I default): the selection is ONE region, only its outer boundary is inset.\nOn = Individual (I,I): every selected face gets its own ring.",
        );
        if r.changed() {
            changed = true;
            released = true;
        }
        ui.separator();
        if ui.button("Done").clicked() {
            close = true;
        }

        if changed {
            a.needs_settle = true;
            let nid = a.nid;
            let mut m = (*a.before).clone();
            let applied = apply_inset(&mut m, &a.faces, a.thickness, a.depth, a.individual);
            // SMART SELECT: the sidecar is keyed onto whatever this panel installed on the
            // PREVIOUS tick, so that is the mesh to re-key FROM (not `a.before`). The last
            // tick's inset ring moved, so its tags drop out and are re-stamped below —
            // fail closed rather than carried onto a stranger.
            let old = self.poly_node_mesh(nid);
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = Arc::new(m);
                }
            }
            if let Some(old) = old.as_deref() {
                self.poly_inset_stamp_tags(nid, old, &applied.new_verts);
            }
            self.poly_sel.selected_faces.clear();
            for &f in &applied.inner {
                self.poly_sel.selected_faces.insert(f);
            }
            self.poly_inset_individual = a.individual;
            self.poly_invalidate_pick_cache();
            self.poly_request_display_sync(nid);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        self.ui.poly_inset_adjust = Some(a.clone());
        if released {
            self.poly_request_display_sync(a.nid);
            if a.needs_settle {
                self.transaction_snapshot_only("Adjust Inset");
                if let Some(adj) = self.ui.poly_inset_adjust.as_mut() {
                    adj.needs_settle = false;
                }
            }
        }
        if close {
            self.poly_request_display_sync(a.nid);
            self.ui.poly_inset_adjust = None;
        }
        true
    }

    /// Legacy floating window (old UI only).
    pub(crate) fn draw_poly_inset_adjust(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui;
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            return;
        }
        if !self.poly_inset_adjust_active() {
            return;
        }
        egui::Window::new("Adjust Inset")
            .id(egui::Id::new("poly_inset_adjust"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                let _ = self.poly_inset_adjust_panel(ui);
            });
    }
}
