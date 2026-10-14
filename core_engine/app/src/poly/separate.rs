//! Blender "Separate by Selection" (P) — selected faces become a new PolygonalBody.

use std::sync::Arc;
use core_math::half_edge::FaceId;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, DirtyFlags};

impl AzraApp {
    /// Separate selected faces into a **new** poly body and delete them from the source
    /// (Blender Mesh → Separate → Selection).
    ///
    /// Face mode only. Refuse-before-transaction when selection is empty / not a poly mesh /
    /// extraction fails. ONE undo entry covers both the hole in the source and the new object.
    pub(crate) fn poly_separate_selection(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        if faces.is_empty() {
            self.toast("Separate: select faces first");
            return;
        }

        // Dry-run: build the detached mesh and ensure we still have something to delete.
        let prepared = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, material, name, .. } = &node.payload else {
                self.toast("Separate: active object is not a poly mesh");
                return;
            };
            let fids: Vec<FaceId> = faces.iter().map(|&f| FaceId(f)).collect();
            let Some(part) = mesh.faces_as_new_mesh(&fids) else {
                self.toast("Separate: could not build mesh from selection");
                return;
            };
            if part.face_count() == 0 {
                self.toast("Separate: nothing to separate");
                return;
            }
            // Refuse if separating ALL faces (would leave empty source) — user should duplicate body.
            // `face_count()` is the live count (excludes free slots).
            if faces.len() >= mesh.face_count() {
                self.toast("Separate: select a subset (not every face)");
                return;
            }
            Some((part, material.clone(), name.clone()))
        };
        let Some((part, material, src_name)) = prepared else { return };

        let mut new_nid: Option<NodeId> = None;
        self.transaction("Separate Selection", |s| {
            // 1) Delete faces from source (descending ids — free high slots first).
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let mut sorted = faces.clone();
                    sorted.sort_unstable_by(|a, b| b.cmp(a));
                    for f in sorted {
                        m.delete_face(FaceId(f));
                    }
                    m.recompute_normals();
                }
            }
            s.poly_request_display_sync(nid);

            // 2) New body as sibling under root (same material, no modifiers).
            let parent = s.scene.scene_graph.root();
            let iname = format!("{}_sep", src_name);
            let nn = s.scene.scene_graph.add_child(
                parent,
                NodePayload::PolygonalBody {
                    name: iname,
                    mesh: Arc::new(part),
                    source: Default::default(),
                    mesh_cache_id: None,
                    material,
                    modifiers: Vec::new(),
                },
            );
            s.poly_request_display_sync(nn);
            new_nid = Some(nn);

            s.poly_sel.selected_faces.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_vertices.clear();
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
            s.mark_outliner_dirty();
            s.toast("Separated selection");
        });

        // Activate the new body (Blender leaves the separated object selected).
        if let Some(nn) = new_nid {
            self.select_scene_node(nn);
        }
    }
}
