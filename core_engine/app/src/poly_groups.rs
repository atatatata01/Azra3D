//! AUTHORED element marks — the hand-made selections modifiers can mask on.
//!
//! * Auto / manual **polygroups**: face selection sets that survive modelling (AUTO_POLYGROUPS.md).
//!   Storage lives on `cache.poly_custom_data[nid]` (`CustomData.groups` + `group_meta`).
//!   Bevel auto-tags **new** strip/cap faces after rebuild (`last_bevel_new_face_range`).
//! * Per-edge **weights** (`CustomData.crease`): `poly_set_edge_weight`.
//!
//! Both are consumed by `GeometryChannel` (`GeoChannelKind::Polygroup` / `EdgeWeight`), re-keyed
//! by position at stack-evaluation time — see `mesh_sync::poly_authored_channels`.

use core_math::half_edge::{FaceId, HalfEdgeId};
use core_math::poly_mesh::{
    CustomData, EdgeKey, PolyGroupBevelParams, PolyGroupKind, PolyGroupMeta,
};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{DirtyFlags, FourthLayerApp, PolyWorkMode};

impl FourthLayerApp {
    /// Ensure a CustomData sidecar exists for this poly body.
    pub(crate) fn poly_cd_mut(&mut self, nid: NodeId) -> &mut CustomData {
        self.cache
            .poly_custom_data
            .entry(nid)
            .or_insert_with(CustomData::new)
    }

    pub(crate) fn poly_cd(&self, nid: NodeId) -> Option<&CustomData> {
        self.cache.poly_custom_data.get(&nid)
    }

    /// Active poly body for group ops (or None).
    pub(crate) fn poly_active_body(&self) -> Option<NodeId> {
        let nid = self.sel.usel.active_node()?;
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { .. }) => Some(nid),
            _ => None,
        }
    }

    /// `true` when the active poly body has any polygroups.
    pub(crate) fn poly_has_polygroups(&self) -> bool {
        self.poly_active_body()
            .and_then(|nid| self.poly_cd(nid))
            .map(|cd| cd.has_polygroups())
            .unwrap_or(false)
    }

    /// Tab pie exposes **GROUP** when groups exist or colour visualisation is already on
    /// (or user is already in Group mode — so the slice stays while leaving).
    pub(crate) fn poly_pie_shows_group(&self) -> bool {
        self.ui.poly_show_polygroups
            || self.poly_work_mode == crate::PolyWorkMode::Group
            || self.poly_has_polygroups()
    }

    /// Select all faces sharing `gid` on the active body (Face/Group selection sets).
    /// `additive`: shift-click unions; otherwise replaces selection.
    pub(crate) fn poly_select_group_faces(&mut self, gid: u16, additive: bool) {
        if gid == 0 {
            self.toast("Face is ungrouped");
            return;
        }
        let Some(nid) = self.poly_active_body() else {
            return;
        };
        if let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) {
            if let Some(cd) = self.cache.poly_custom_data.get_mut(&nid) {
                cd.purge_empty_groups(&mesh);
            }
        }
        let faces = self
            .poly_cd(nid)
            .map(|cd| cd.faces_in_group(gid))
            .unwrap_or_default();
        if faces.is_empty() {
            self.toast("Empty polygroup");
            return;
        }
        if !additive {
            self.poly_sel.selected_faces.clear();
        }
        // Group mode: faces only. Always drop edge/vert leftovers so tools cannot act on a
        // previous group's edges while the face highlight shows this group.
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_active_vertex = None;
        for f in &faces {
            self.poly_sel.selected_faces.insert(f.0);
        }
        self.mark_dirty(DirtyFlags::RENDER);
        let name = self
            .poly_cd(nid)
            .and_then(|cd| cd.group_meta.iter().find(|m| m.id == gid))
            .map(|m| m.name.as_str())
            .unwrap_or("Group");
        self.toast(&format!("Selected {name} ({} faces)", faces.len()));
    }

    /// Click-select: expand hovered face → whole polygroup.
    pub(crate) fn poly_click_select_group(&mut self, shift: bool) {
        let Some(fid) = self.poly_sel.hovered_face else {
            if !shift {
                // Empty click: wipe the whole selection domain (faces + any stale edges/verts).
                self.poly_sel.selected_faces.clear();
                self.poly_group_selection_sync();
            }
            return;
        };
        let Some(nid) = self.poly_active_body() else {
            return;
        };
        let gid = self
            .poly_cd(nid)
            .and_then(|cd| cd.groups.get(FaceId(fid)).copied())
            .unwrap_or(0);
        if gid == 0 {
            // Ungrouped face: select just that face (so the mode still feels responsive).
            if shift {
                if !self.poly_sel.selected_faces.remove(&fid) {
                    self.poly_sel.selected_faces.insert(fid);
                }
            } else {
                self.poly_sel.selected_faces.clear();
                self.poly_sel.selected_faces.insert(fid);
            }
            // Faces only — never leave a previous group's edges for tools to pick up.
            self.poly_group_selection_sync();
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }
        // Shift on an already fully-selected group → deselect that group.
        if shift {
            let faces = self
                .poly_cd(nid)
                .map(|cd| cd.faces_in_group(gid))
                .unwrap_or_default();
            let all_sel = faces
                .iter()
                .all(|f| self.poly_sel.selected_faces.contains(&f.0));
            if all_sel && !faces.is_empty() {
                for f in faces {
                    self.poly_sel.selected_faces.remove(&f.0);
                }
                self.poly_group_selection_sync();
                self.mark_dirty(DirtyFlags::RENDER);
                return;
            }
        }
        self.poly_select_group_faces(gid, shift);
    }

    /// Assign current face selection to a **new** manual group.
    pub(crate) fn poly_assign_selection_to_new_group(&mut self) {
        let Some(nid) = self.poly_active_body() else {
            self.toast("Select a polygonal body");
            return;
        };
        if self.poly_sel.selected_faces.is_empty() {
            self.toast("Select faces first (Face mode)");
            return;
        }
        let faces: Vec<FaceId> = self
            .poly_sel
            .selected_faces
            .iter()
            .copied()
            .map(FaceId)
            .collect();
        self.transaction_snapshot_only("Assign polygroup");
        let gid = {
            let cd = self.poly_cd_mut(nid);
            let gid = cd.create_group(PolyGroupKind::Manual, None);
            cd.assign_faces_to_group(faces.iter().copied(), gid);
            gid
        };
        let name = self
            .poly_cd(nid)
            .and_then(|cd| cd.group_meta.iter().find(|m| m.id == gid))
            .map(|m| m.name.clone())
            .unwrap_or_else(|| format!("Group {gid}"));
        // A polygroup is no longer visualisation-only — a modifier's geometry channel can mask on
        // it, so the display mesh must re-evaluate. RENDER alone would redraw the stale mesh.
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!("Assigned {n} faces → {name}", n = faces.len()));
    }

    /// Set the AUTHORED EDGE WEIGHT on the current edge selection — the `crease` column, which is
    /// what [`GeoChannelKind::EdgeWeight`](core_math::geometry_channel::GeoChannelKind) reads.
    ///
    /// This is the AUTHORING half of "weighted edges": the column has persisted and survived
    /// rebuilds (`remap_by_position`) since the sidecar landed, and dissolve / Mesh Cleanup already
    /// treat a weighted edge as protected — but nothing in the app ever WROTE one, so the data
    /// could only ever arrive from a file. `w <= 0` clears (an entry of 0.0 would be a weightless
    /// edge that still counts as authored, which is a different thing from an unweighted edge).
    ///
    /// `EdgeKey` is canonical-undirected, so selecting either half-edge of a pair weights the edge
    /// once and the twin agrees by construction.
    pub(crate) fn poly_set_edge_weight(&mut self, w: f32) {
        let Some((nid, keys)) = self.poly_edge_weight_targets("Set Edge Weight") else {
            return;
        };
        let w = w.clamp(0.0, 1.0);
        // `w == 0` CLEARS: an entry of 0.0 would be a weightless edge that still counts as authored,
        // which is a different thing from an unweighted edge.
        let value = if w <= 0.0 { None } else { Some(w) };
        let entries: Vec<(EdgeKey, Option<f32>)> = keys.iter().map(|&k| (k, value)).collect();
        self.transaction_snapshot_only("Set edge weight");
        self.poly_write_edge_weights(nid, &entries);
        self.poly_show_edge_weight_overlay();
        if w <= 0.0 {
            self.toast(&format!("Cleared weight on {} edges", keys.len()));
        } else {
            self.toast(&format!("Weight {w:.2} on {} edges", keys.len()));
        }
    }

    /// Which edges an authored-weight write targets, resolved from the CURRENT selection:
    ///
    /// * **Face / Group mode** — the selected region's BOUNDARY (owner 2026-08-11: *"if poly is
    ///   selected boundery get weited edge"*), through the ONE region-boundary rule
    ///   [`region_boundary_half_edges`](core_math::half_edge::HalfEdgeMesh::region_boundary_half_edges)
    ///   that inset / circle-region already read — not a second definition of "boundary".
    /// * **Any other mode** — the selected edges (what this path always did).
    ///
    /// Shared by the menu presets and by the `W` drag so both agree on the target set. Toasts and
    /// returns `None` when there is nothing to weight, so callers can `let … else { return }`.
    pub(crate) fn poly_edge_weight_targets(&mut self, what: &str) -> Option<(NodeId, Vec<EdgeKey>)> {
        let Some(nid) = self.poly_active_body() else {
            self.toast(&format!("{what}: select a polygonal body"));
            return None;
        };
        let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return None,
        };
        let mut keys: Vec<EdgeKey> = match self.poly_work_mode {
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let faces: std::collections::HashSet<FaceId> =
                    self.poly_sel.selected_faces.iter().map(|&f| FaceId(f)).collect();
                if faces.is_empty() {
                    self.toast(&format!("{what}: select faces first"));
                    return None;
                }
                mesh.region_boundary_half_edges(&faces)
                    .into_iter()
                    .map(|he| EdgeKey::of_half_edge(&mesh, he))
                    .collect()
            }
            _ => {
                if self.poly_sel.selected_edges.is_empty() {
                    self.toast(&format!("{what}: select edges (Edge mode) or faces (Face mode)"));
                    return None;
                }
                self.poly_sel
                    .selected_edges
                    .iter()
                    .map(|&e| HalfEdgeId(e))
                    .filter(|&he| mesh.is_edge_live(he))
                    .map(|he| EdgeKey::of_half_edge(&mesh, he))
                    .collect()
            }
        };
        keys.sort_unstable();
        keys.dedup();
        if keys.is_empty() {
            self.toast(&format!("{what}: selection is stale"));
            return None;
        }
        Some((nid, keys))
    }

    /// THE write path for authored edge weights (`CustomData.crease`) plus every invalidation they
    /// need. `Some(w)` sets the weight; `None` REMOVES the entry.
    ///
    /// That distinction is load-bearing: an entry of `0.0` is a weightless but still AUTHORED edge
    /// (dissolve / Mesh Cleanup treat authored edges as protected), which is a different state from
    /// no entry at all — so a restore path has to be able to say "no entry" exactly.
    ///
    /// Owns no undo framing and no toast: the menu presets wrap it in `transaction_snapshot_only`,
    /// the `W` drag wraps its whole gesture in ONE episode.
    pub(crate) fn poly_write_edge_weights(
        &mut self,
        nid: NodeId,
        entries: &[(EdgeKey, Option<f32>)],
    ) {
        {
            let cd = self.poly_cd_mut(nid);
            for (k, w) in entries {
                match w {
                    Some(w) => cd.crease.set(*k, w.clamp(0.0, 1.0)),
                    None => {
                        cd.crease.remove(*k);
                    }
                }
            }
        }
        // Edge-weight overlay reads CustomData (outside mesh Arc) — force segment rebuild.
        self.overlay_lines_fp = u64::MAX;
        // Re-evaluate the display stack ONLY when something in it can actually observe the column.
        //
        // With no consumer of the column the re-evaluated mesh is bit-identical to the one already
        // cached, so an unconditional sync would burn a full stack evaluation — and
        // `content_signature` folds `crease`, so the key changes every step of a `W` drag: that is a
        // full re-bake PER FRAME on a body where nothing can look different (the display bake is the
        // dominant cost in poly interactivity — see the 40k-quad Smooth slider measurements).
        //
        // WHO the consumers are is `PolyModifier::reads_edge_weight`, NOT a channel test written out
        // here. This site used to ask only "is some modifier masked on the EdgeWeight channel", which
        // misses a Bevel driving its Variable Width from authored weight — that reads the same column
        // with its channel left at `All`. The display cache key already counted that modifier as a
        // reader, so authoring a weight invalidated an entry nothing then asked to rebuild, and
        // Variable Width by weight never updated on screen at all.
        let stack_reads_weight = self
            .scene
            .scene_graph
            .get(nid)
            .map_or(false, |n| match &n.payload {
                NodePayload::PolygonalBody { modifiers, .. } => {
                    modifiers.iter().any(|m| m.reads_edge_weight())
                }
                _ => false,
            });
        if stack_reads_weight {
            self.poly_request_display_sync(nid);
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Turn the edge-weight overlay on after authoring one — the same courtesy the seam path does
    /// for `show_uv_seams`. The overlay ships OFF, so without this the write has no visible effect
    /// at all unless a masked modifier happens to consume the channel.
    pub(crate) fn poly_show_edge_weight_overlay(&mut self) {
        if !self.viewport.show_edge_weights || !self.viewport.show_overlays {
            self.viewport.show_edge_weights = true;
            self.viewport.show_overlays = true;
            self.overlay_lines_fp = u64::MAX;
        }
    }

    /// Select all faces in group `gid` on the active body.
    /// Stays in Group mode when already there; otherwise prefers Face (menu path).
    pub(crate) fn poly_select_group(&mut self, gid: u16) {
        let Some(nid) = self.poly_active_body() else {
            return;
        };
        // Purge dead face ids first so the list stays honest.
        if let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) {
            if let Some(cd) = self.cache.poly_custom_data.get_mut(&nid) {
                cd.purge_empty_groups(&mesh);
            }
        }
        let faces = self
            .poly_cd(nid)
            .map(|cd| cd.faces_in_group(gid))
            .unwrap_or_default();
        if faces.is_empty() {
            self.toast("Empty polygroup");
            return;
        }
        // Sticky Group: never yank the user to Face after picking a group.
        self.poly_prefer_work_mode(crate::PolyWorkMode::Face);
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_active_vertex = None;
        for f in &faces {
            self.poly_sel.selected_faces.insert(f.0);
        }
        self.poly_group_selection_sync();
        self.mark_dirty(DirtyFlags::RENDER);
        let name = self
            .poly_cd(nid)
            .and_then(|cd| cd.group_meta.iter().find(|m| m.id == gid))
            .map(|m| m.name.as_str())
            .unwrap_or("Group");
        self.toast(&format!("Selected {name} ({} faces)", faces.len()));
    }

    /// Select the polygroup of the first selected/hovered face.
    pub(crate) fn poly_select_group_under_cursor(&mut self) {
        let Some(nid) = self.poly_active_body() else {
            return;
        };
        let fid = self
            .poly_sel
            .selected_faces
            .iter()
            .next()
            .copied()
            .or(self.poly_sel.hovered_face);
        let Some(fid) = fid else {
            self.toast("Select or hover a face first");
            return;
        };
        let gid = self
            .poly_cd(nid)
            .and_then(|cd| cd.groups.get(FaceId(fid)).copied())
            .unwrap_or(0);
        if gid == 0 {
            self.toast("Face is ungrouped");
            return;
        }
        self.poly_select_group(gid);
    }

    /// Toggle polygroup colour overlay (edit-mode face tint).
    pub(crate) fn poly_toggle_group_overlay(&mut self) {
        self.ui.poly_show_polygroups = !self.ui.poly_show_polygroups;
        let on = self.ui.poly_show_polygroups;
        self.toast(if on {
            "Polygroup visualisation ON"
        } else {
            "Polygroup visualisation OFF"
        });
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Create a group of `kind` and assign `faces` to it. No-op if `faces` empty.
    pub(crate) fn poly_auto_group_faces(
        &mut self,
        nid: NodeId,
        kind: PolyGroupKind,
        faces: impl IntoIterator<Item = FaceId>,
    ) -> Option<u16> {
        self.poly_auto_group_faces_ex(nid, kind, faces, None)
    }

    /// Like [`Self::poly_auto_group_faces`] with optional bevel reverse params.
    pub(crate) fn poly_auto_group_faces_ex(
        &mut self,
        nid: NodeId,
        kind: PolyGroupKind,
        faces: impl IntoIterator<Item = FaceId>,
        bevel: Option<PolyGroupBevelParams>,
    ) -> Option<u16> {
        let faces: Vec<FaceId> = faces.into_iter().collect();
        if faces.is_empty() {
            return None;
        }
        let cd = self.poly_cd_mut(nid);
        let gid = cd.create_group_ex(kind, None, bevel);
        cd.assign_faces_to_group(faces.iter().copied(), gid);
        Some(gid)
    }

    /// After a successful bevel confirm: tag new strip/cap faces + store tool params on meta.
    pub(crate) fn poly_auto_group_last_bevel(
        &mut self,
        nid: NodeId,
        width: f32,
        segments: u32,
        shape: f32,
        clamp: bool,
    ) {
        let Some((start, end)) = core_math::bevel_op::last_bevel_new_face_range() else {
            return;
        };
        if end <= start {
            return;
        }
        let n_faces = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.faces.len() as u32,
            _ => return,
        };
        let end = end.min(n_faces);
        if end <= start {
            return;
        }
        let faces: Vec<FaceId> = (start..end).map(FaceId).collect();
        let _ = self.poly_auto_group_faces_ex(
            nid,
            PolyGroupKind::Bevel,
            faces,
            Some(PolyGroupBevelParams {
                width,
                segments,
                shape,
                clamp,
            }),
        );
    }

    /// Tag every live face on the body (whole-mesh ops like Solidify).
    pub(crate) fn poly_auto_group_all_faces(&mut self, nid: NodeId, kind: PolyGroupKind) {
        let faces: Vec<FaceId> = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => (0..mesh.faces.len() as u32)
                .filter(|&i| mesh.face(FaceId(i)).half_edge != core_math::half_edge::NO_EDGE)
                .map(FaceId)
                .collect(),
            _ => return,
        };
        let _ = self.poly_auto_group_faces(nid, kind, faces);
    }

    /// Rename a polygroup on the active body.
    pub(crate) fn poly_rename_group(&mut self, gid: u16, name: String) {
        let Some(nid) = self.poly_active_body() else {
            return;
        };
        self.transaction_snapshot_only("Rename polygroup");
        let ok = self.poly_cd_mut(nid).rename_group(gid, name.clone());
        if ok {
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast(&format!("Renamed → {name}"));
        }
    }

    /// Bevel params stored on a group meta, if any.
    pub(crate) fn poly_group_bevel_params(
        &self,
        nid: NodeId,
        gid: u16,
    ) -> Option<PolyGroupBevelParams> {
        self.poly_cd(nid)
            .and_then(|cd| cd.group_meta.iter().find(|m| m.id == gid))
            .and_then(|m| m.bevel.clone())
    }

    /// Meta list for the active body (after purge).
    pub(crate) fn poly_group_list(&mut self) -> Vec<PolyGroupMeta> {
        let Some(nid) = self.poly_active_body() else {
            return Vec::new();
        };
        if let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) {
            if let Some(cd) = self.cache.poly_custom_data.get_mut(&nid) {
                cd.purge_empty_groups(&mesh);
            }
        }
        self.poly_cd(nid)
            .map(|cd| cd.group_meta.clone())
            .unwrap_or_default()
    }

    /// Delete a polygroup (clear face assignments + meta).
    pub(crate) fn poly_delete_group(&mut self, gid: u16) {
        let Some(nid) = self.poly_active_body() else {
            return;
        };
        self.transaction_snapshot_only("Delete polygroup");
        let name = {
            let cd = self.poly_cd_mut(nid);
            let name = cd
                .group_meta
                .iter()
                .find(|m| m.id == gid)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| format!("Group {gid}"));
            let clear: Vec<FaceId> = cd.faces_in_group(gid);
            cd.assign_faces_to_group(clear, 0);
            cd.group_meta.retain(|m| m.id != gid);
            name
        };
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!("Deleted {name}"));
    }

    /// Most recent Bevel polygroup on `nid` (highest id among kind==Bevel).
    pub(crate) fn poly_latest_bevel_group(&self, nid: NodeId) -> Option<u16> {
        self.poly_cd(nid).and_then(|cd| {
            cd.group_meta
                .iter()
                .filter(|m| m.kind == PolyGroupKind::Bevel)
                .max_by_key(|m| m.id)
                .map(|m| m.id)
        })
    }

    /// Face ids of a group.
    pub(crate) fn poly_group_faces(&self, nid: NodeId, gid: u16) -> Vec<u32> {
        self.poly_cd(nid)
            .map(|cd| cd.faces_in_group(gid).into_iter().map(|f| f.0).collect())
            .unwrap_or_default()
    }
}
