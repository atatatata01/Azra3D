use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::FaceId;
use core_math::mesh_ops::PolyEdit;
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode, SceneItemId};

impl AzraApp {
    /// Blender **Limited Dissolve** — dissolve edges between faces whose normals differ by ≤
    /// `angle_deg` (app default **4.5°**). Uses face selection when non-empty; otherwise whole mesh.
    ///
    /// Refuse-before-transaction when nothing qualifies.
    pub(crate) fn poly_limited_dissolve(&mut self) {
        self.poly_limited_dissolve_angle(4.5);
    }

    pub(crate) fn poly_limited_dissolve_angle(&mut self, angle_deg: f32) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let face_sel: Option<Vec<u32>> = if matches!(
            self.poly_work_mode,
            PolyWorkMode::Face | PolyWorkMode::Group
        ) && !self.poly_sel.selected_faces.is_empty()
        {
            Some(self.poly_sel.selected_faces.iter().copied().collect())
        } else {
            None
        };

        // Authored edges (Smart Select tag / sharp / seam / crease) are never candidates, however
        // flat they are — owner 2026-08-05, same rule as Mesh Cleanup. Built once from the base
        // mesh: `dissolve_edges_region` mutates in place, so survivor ids stay valid throughout.
        let protect = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast("Limited Dissolve: select a poly mesh");
                return;
            };
            self.poly_protected_edges(nid, mesh)
        };
        let protect_arg = if protect.is_empty() { None } else { Some(&protect) };

        let dry_n = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return;
            };
            let mut scratch = (**mesh).clone();
            let only = face_sel.as_ref().map(|fs| {
                fs.iter().map(|&f| FaceId(f)).collect::<HashSet<_>>()
            });
            scratch.limited_dissolve(angle_deg, only.as_ref(), protect_arg)
        };
        if dry_n == 0 {
            let why = if protect.is_empty() {
                "nothing within angle"
            } else {
                "nothing within angle (tagged edges are protected)"
            };
            self.toast(&format!("Limited Dissolve: {why}"));
            return;
        }

        let old_mesh = self.poly_node_mesh(nid);
        self.transaction("Limited Dissolve", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let only = face_sel.as_ref().map(|fs| {
                        fs.iter().map(|&f| FaceId(f)).collect::<HashSet<_>>()
                    });
                    let n = m.limited_dissolve(
                        angle_deg,
                        only.as_ref(),
                        if protect.is_empty() { None } else { Some(&protect) },
                    );
                    s.toast(&format!("Limited Dissolve: {n} edges"));
                }
            }
            // Sweep sidecar entries whose element the dissolve killed, so a later rebuild cannot
            // resurrect a tag onto a stranger that happens to reuse the id.
            if let (Some(old), Some(new)) = (old_mesh.as_ref(), s.poly_node_mesh(nid)) {
                if let Some(cd) = s.cache.poly_custom_data.get(&nid) {
                    let carried = cd.remap_by_position(old, &new);
                    s.cache.poly_custom_data.insert(nid, carried);
                }
            }
            s.poly_sel.selected_faces.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_vertices.clear();
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        });
    }

    /// Resolve which PolygonalBody Ctrl+X / poly edit ops should hit.
    /// Prefer active poly node, then any selected poly body, then active_body if poly.
    /// (active_node alone was silent-no-op when selection lived only on components.)
    pub(crate) fn resolve_poly_edit_nid(&self) -> Option<core_math::scene_graph::NodeId> {
        if let Some(n) = self.active_poly_nid() {
            return Some(n);
        }
        for nid in self.sel.usel.selected_nodes() {
            if matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::PolygonalBody { .. })
            ) {
                return Some(nid);
            }
        }
        let ab = self.scene.active_body_nid;
        if matches!(
            self.scene.scene_graph.get(ab).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            return Some(ab);
        }
        None
    }

    /// Ctrl+X — owner design (Blender-adjacent, not Blender-identical):
    ///   * Vertex mode → **dissolve** verts (merge surrounding faces)
    ///   * Edge mode   → **dissolve** edges (region merge + valence-2 cleanup)
    ///   * Face mode   → **delete** faces (hole; edges kept) — deliberate diverge from Blender
    ///     face-dissolve; menu "Dissolve Faces" still does n-gon dissolve.
    ///
    /// **DESTRUCTIVE — never use empty=all.** SAFE ops (extrude/inset/bevel/…) expand empty
    /// selection to the whole mesh via `poly_*_or_all`. Ctrl+X must only touch the **explicit**
    /// component selection; empty selection is a no-op (toast), not "delete everything".
    ///
    /// Also runs when leftover component selection exists in Object mode (mode switch left them).
    /// Sorts IDs descending so dissolving highest IDs first minimizes id invalidation in the batch.
    pub(crate) fn poly_dissolve(&mut self) {
        // Group mode: faces only — drop leftover edges/verts so dissolve matches the highlight.
        if self.poly_work_mode == PolyWorkMode::Group {
            self.poly_group_selection_sync();
        }
        let has_v = !self.poly_sel.selected_vertices.is_empty();
        let has_e = !self.poly_sel.selected_edges.is_empty();
        let has_f = !self.poly_sel.selected_faces.is_empty();
        let has_comp = has_v || has_e || has_f;

        Self::ctrx_diag(&format!(
            "poly_dissolve ENTER mode={:?} faces={} edges={} verts={}",
            self.poly_work_mode,
            self.poly_sel.selected_faces.len(),
            self.poly_sel.selected_edges.len(),
            self.poly_sel.selected_vertices.len(),
        ));

        // Infer domain: work mode wins when it matches a non-empty selection; else pick from
        // whatever is selected (Object-mode leftovers / mode mismatch).
        // NEVER expand empty → all (that is SAFE-op only via poly_*_or_all).
        let domain = match self.poly_work_mode {
            PolyWorkMode::Vertex if has_v => PolyWorkMode::Vertex,
            PolyWorkMode::Edge if has_e => PolyWorkMode::Edge,
            PolyWorkMode::Face | PolyWorkMode::Group if has_f => PolyWorkMode::Face,
            _ if has_f => PolyWorkMode::Face,
            _ if has_e => PolyWorkMode::Edge,
            _ if has_v => PolyWorkMode::Vertex,
            PolyWorkMode::Object if !has_comp => {
                Self::ctrx_diag("poly_dissolve EXIT object mode no components");
                return;
            }
            _ => {
                // Edit mode (or Object leftovers) with empty selection: refuse, do not wipe mesh.
                if self.poly_work_mode != PolyWorkMode::Object {
                    self.toast("Ctrl+X: select faces / edges / verts first");
                }
                Self::ctrx_diag("poly_dissolve EXIT empty selection");
                return;
            }
        };

        let Some(nid) = self.resolve_poly_edit_nid() else {
            self.toast("Ctrl+X: select a polygon mesh first");
            Self::ctrx_diag("poly_dissolve EXIT no poly nid");
            return;
        };
        Self::ctrx_diag(&format!("poly_dissolve domain={domain:?} nid={nid:?}"));

        // Point active at the mesh we edit — do NOT insert into multi-selection.
        if self.sel.usel.active_node() != Some(nid) {
            self.sel.usel.active = Some(SceneItemId::Node(nid));
        }

        match domain {
            PolyWorkMode::Vertex => {
                // Explicit selection only — never poly_verts_or_all (destructive).
                let mut verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
                if verts.is_empty() {
                    self.toast("Ctrl+X: select vertices first");
                    return;
                }
                verts.sort_unstable_by(|a, b| b.cmp(a));
                let ops: Vec<PolyEdit> = verts
                    .into_iter()
                    .map(|v| PolyEdit::DissolveVertex { vertex: v })
                    .collect();
                let out = self.poly_op_batch(nid, ops, "Dissolve Vertices");
                self.poly_report_batch(&out, "dissolved", &[]);
            }
            PolyWorkMode::Edge => {
                // Explicit selection only — never poly_edges_or_all (destructive).
                let edges: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
                if edges.is_empty() {
                    self.toast("Ctrl+X: select edges first");
                    return;
                }
                // ONE region op (Blender bmo_dissolve_edges): merges faces bounded by the
                // selected edges into n-gons, then sweeps valence-2 endpoints (use_verts).
                let ops = vec![PolyEdit::DissolveEdgesRegion { edges }];
                let out = self.poly_op_batch(nid, ops, "Dissolve Edges");
                self.poly_report_batch(&out, "dissolved", &[]);
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                // Owner design: faces are REMOVED (hole), not dissolved into one n-gon.
                // poly_delete_faces also refuses empty — never poly_faces_or_all.
                Self::ctrx_diag(&format!(
                    "poly_dissolve DELETE {} face(s)",
                    self.poly_sel.selected_faces.len()
                ));
                self.poly_delete_faces();
            }
            PolyWorkMode::Object => {}
        }
        Self::ctrx_diag("poly_dissolve DONE");
    }
}
