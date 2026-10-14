//! Outliner action application + selection/hierarchy helpers.
use crate::*;
use super::types::*;
use super::build::ol_rename_target;


impl FourthLayerApp {
    // ── Expand SoT helpers (INTEGRATION_RULES §2) ──────────────────────────
    // ui2 primary: `ol_collapsed` (in set = collapsed, default open).
    // Always dual-write `expanded_bodies` so v1 find/focus and ui2 stay aligned.

    /// Whether a node is expanded in the outliner tree (default: open).
    #[inline]
    pub(crate) fn ol_is_expanded(&self, nid: NodeId) -> bool {
        !self.ui2.ol_collapsed.contains(&nid.0)
    }

    /// Set expand state; keeps `OutlinerState.expanded_bodies` in sync.
    pub(crate) fn ol_set_expanded(&mut self, nid: NodeId, expanded: bool) {
        if expanded {
            self.ui2.ol_collapsed.remove(&nid.0);
            self.outliner.expanded_bodies.insert(nid);
        } else {
            self.ui2.ol_collapsed.insert(nid.0);
            self.outliner.expanded_bodies.remove(&nid);
        }
    }

    /// Expand only (never collapses) — used by find/focus.
    pub(crate) fn ol_ensure_expanded(&mut self, nid: NodeId) {
        self.ol_set_expanded(nid, true);
    }

    /// Renamable (target, current_name) for every selected item — active first.
    pub(crate) fn ol_renamable_selection(&self) -> Vec<(SceneNodeId, String)> {
        let try_item = |id: SceneItemId| -> Option<(SceneNodeId, String)> {
            match id {
                SceneItemId::Prim(uid) => {
                    let name = self
                        .scene
                        .primitives
                        .iter()
                        .find(|p| p.prim_uid == uid)
                        .map(|p| p.name.clone())?;
                    Some((SceneNodeId::Prim(uid), name))
                }
                SceneItemId::Node(nid) => {
                    let node = self.scene.scene_graph.get(nid)?;
                    let t = ol_rename_target(&node.payload, nid)?;
                    Some((t, node.payload.display_name().to_string()))
                }
                SceneItemId::Light(lid) => {
                    let name = self.scene.light_by_id(lid).map(|d| d.name.clone())?;
                    Some((SceneNodeId::Light(lid), name))
                }
            }
        };
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        if let Some(a) = self.sel.usel.active {
            ids.push(a);
        }
        for id in &self.sel.usel.selected {
            if Some(*id) != self.sel.usel.active {
                ids.push(*id);
            }
        }
        for id in ids {
            if let Some((t, name)) = try_item(id) {
                let key = match t {
                    SceneNodeId::Prim(u) => format!("p{u}"),
                    SceneNodeId::Light(u) => format!("l{u}"),
                    SceneNodeId::Body(n) | SceneNodeId::Node(n) => format!("n{}", n.0),
                };
                if seen.insert(key) {
                    out.push((t, name));
                }
            }
        }
        out
    }

    /// Refresh manifold cache for all polygonal bodies (only when Manifold column is on).
    ///
    /// `is_manifold()` (O(mesh)) re-runs only when the identity below changes. Stale entries
    /// (deleted bodies) are pruned. Callers must also patch `ol_rows_cache[].manifold` after this
    /// (see `ui2/outliner/mod.rs`) — the cache updates every frame but the outliner ROWS only
    /// rebuild on hierarchy dirty, which is how the column once stayed green forever.
    ///
    /// MERGE RESOLUTION 2026-08-05 — two branches wrote this function differently and BOTH were
    /// right about something:
    /// * `fix/ui-floor-perf` made the identity **O(1)** (`Arc::as_ptr` + topology counts), because
    ///   walking every half-edge per body per frame is exactly the full-UI cost that branch exists
    ///   to remove.
    /// * `master` had just fixed a real bug proving that key is NOT sufficient: an in-place edit
    ///   that rewires twins keeps the Arc pointer AND the counts (twin rewire, same-winding double
    ///   use, knife rebuild that free+allocs slots), so the column stayed green after going
    ///   non-manifold.
    ///
    /// Taking either side alone would have shipped either a per-frame mesh walk or a lying warning
    /// column. The identity is now O(1) AND catches in-place edits, by folding in
    /// `poly_edit_generation` — the app's existing mutation choke-point counter, already trusted by
    /// the display-key memo and the LOD latch. One mutation signal for the whole app rather than a
    /// second private one that can drift out of sync with it.
    ///
    /// PERF 2026-08-12 — the generation is in the fingerprint on purpose (above), but that means
    /// the fingerprint MISSES on every frame the generation moves, and `is_manifold()` then runs
    /// for every polygonal body from a UI refresh. The owner's capture put `layout` at 40–88 ms on
    /// a 323k-face body, and the generation churns both during a drag (once per frame) and at rest.
    /// So: while a drag EPISODE is open, keep the cached values. A mid-gesture mesh is transiently
    /// non-manifold by construction, so a live column would be reporting noise anyway; the walk
    /// runs on the first refresh after the commit, which is when the answer is actionable.
    /// `auto_commit_episodes` bounds the hold to one frame past the release.
    ///
    /// This is the same trap as `viewport_frame_geometry_issues` and `poly_pick_mesh_key`: a memo
    /// keyed on the mutation counter degrades to "no cache" exactly when the work is hot.
    pub(crate) fn ol_refresh_manifold_cache(&mut self) {
        if self.pending_episode.is_some() {
            return;
        }
        // Read once: `all_nodes()` borrows the graph below, and this is per-app not per-body.
        let gen_fp = self.poly_edit_generation.rotate_left(33);
        let mut live: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for (nid, node) in self.scene.scene_graph.all_nodes() {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                live.insert(nid.0);
                // Arc identity (COW replacement / whole-mesh swap) + topology counts
                // (unique-`make_mut` edits: ptr stable, counts move) + the poly edit generation
                // (in-place edits that move NEITHER — the green-forever bug). NEVER a content hash.
                let arc_fp = std::sync::Arc::as_ptr(mesh) as u64;
                let counts_fp = (mesh.vertex_count() as u64)
                    ^ ((mesh.face_count() as u64) << 21)
                    ^ ((mesh.edge_count() as u64) << 42);
                let fp = arc_fp.rotate_left(17) ^ counts_fp ^ gen_fp;
                match self.ui2.ol_manifold_cache.get(&nid.0) {
                    Some(&(cached_fp, _)) if cached_fp == fp => {}
                    _ => {
                        // Full solid manifold (edge multiplicity + bowtie). Matches boolean
                        // install bar — open sheets with bowties show as warning, which is
                        // what the column is for.
                        let ok = mesh.is_manifold().is_ok();
                        self.ui2.ol_manifold_cache.insert(nid.0, (fp, ok));
                    }
                }
            }
        }
        self.ui2.ol_manifold_cache.retain(|k, _| live.contains(k));
    }

    /// Expand `nid` and every descendant (for Expand Branch / Expand All helpers).
    pub(crate) fn ol_expand_descendants(&mut self, nid: NodeId) {
        self.ol_set_expanded(nid, true);
        let kids: Vec<NodeId> = self
            .scene
            .scene_graph
            .get(nid)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        for k in kids {
            self.ol_expand_descendants(k);
        }
    }

    /// Collapse `nid` and every descendant.
    pub(crate) fn ol_collapse_descendants(&mut self, nid: NodeId) {
        self.ol_set_expanded(nid, false);
        let kids: Vec<NodeId> = self
            .scene
            .scene_graph
            .get(nid)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        for k in kids {
            self.ol_collapse_descendants(k);
        }
    }

    /// Apply one collected outliner context-menu action. Every arm delegates to a verified,
    /// self-transacting backend path — this function adds no undo entries of its own.
    pub(crate) fn ui2_ol_apply(&mut self, act: Option<OlAct>) {
        let Some(act) = act else { return };
        match act {
            OlAct::StartRename(t, cur) => self.trigger_rename(t, &cur),
            // Selection-driven and multi-select aware (top-most-ancestor filtered), the same
            // paths Shift+D / Del use. Never `duplicate_subtree` directly — that copy ignored
            // multi-selection.
            OlAct::Duplicate => self.duplicate_selected(),
            OlAct::Delete => self.delete_selected(),
            OlAct::Group => self.group_selected_into_body(None),
            OlAct::Dissolve(nid) => self.dissolve_sdf_body(nid),
            OlAct::SaveAsPart(nid) => self.open_library_save_dialog(nid),
            OlAct::EnterScope(nid) => self.enter_body_scope(nid),
            // NOT wrapped in a transaction: convert_* self-transact. v1 double-wraps them
            // (outliner_panel.rs:106) and `transaction()` has no reentrancy guard, so that
            // pushes TWO undo entries and costs the artist a second Ctrl+Z.
            OlAct::ConvertToPoly(nid) => {
                match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::SdfBody { .. }) => self.convert_sdf_to_poly(nid),
                    Some(NodePayload::ImportedMesh { .. }) => self.convert_imported_to_poly(nid),
                    Some(NodePayload::NurbsBody { .. }) => self.convert_nurbs_to_poly(nid),
                    _ => {}
                }
            }
            // Routed as a command so macro record/replay keeps seeing boolean changes.
            OlAct::SetBool(uid, op) => self.push_cmd(AppCommand::SetBooleanOp(uid, op)),
            OlAct::SetShape(uid, shape) => self.set_prim_shape(uid, shape),
            OlAct::AddMod(uid, mt) => self.add_prim_modifier(uid, mt),
            OlAct::Solo(item) => match item {
                OlItem::Node(n) => self.solo_node(NodeId(n)),
                OlItem::Prim(u) => self.solo_prim(u),
            },
            OlAct::ShowAll(item) => match item {
                OlItem::Node(n) => self.show_all_siblings(NodeId(n)),
                OlItem::Prim(u) => self.show_all_body_prims(u),
            },
            OlAct::Frame => {
                self.frame_selected();
            }
            OlAct::ExpandBranch(n) => {
                self.ol_expand_descendants(NodeId(n));
            }
            OlAct::CollapseBranch(n) => {
                self.ol_collapse_descendants(NodeId(n));
            }
            // Same as click / Enter Scope for undo + selection consistency.
            OlAct::SetActiveBody(nid) => self.enter_body_scope(nid),
            OlAct::SelectChildren(nid) => self.ol_select_children(nid),
            OlAct::SelectHierarchy(nid) => self.ol_select_hierarchy(nid),
            OlAct::SelectParent(nid) => {
                if let Some(p) = self.scene.scene_graph.get(nid).and_then(|n| n.parent) {
                    if !matches!(
                        self.scene.scene_graph.get(p).map(|n| &n.payload),
                        Some(NodePayload::Root)
                    ) {
                        self.select_single_unified(SceneItemId::Node(p));
                    }
                }
            }
            OlAct::Unparent => self.ol_unparent_selection(),
            OlAct::Parent => {
                // Group selected prims into a sub-body (same backend as context Group)
                self.group_selected_into_body(None);
            }
            OlAct::SetLightType(id, lt) => self.set_light_type(id, lt),
            OlAct::CreateMaterial => self.create_material_interactive(),
            OlAct::DuplicateMaterial(nid) => self.duplicate_material_node(nid),
            // delete_material_node self-transacts + clears in-use refs (never raw Del).
            OlAct::DeleteMaterial(nid) => self.delete_material_node(nid),
            OlAct::AssignMaterialToSelection(mid) => {
                let name = self
                    .scene
                    .scene_graph
                    .material_by_id(core_math::primitives::MaterialId(mid))
                    .map(|(_, m)| m.name.clone())
                    .unwrap_or_default();
                let label = if name.is_empty() {
                    format!("Material {mid}")
                } else {
                    name
                };
                if !self.has_material_assign_target() {
                    self.toast("Select a prim, SDF body, poly object, or faces first");
                } else if self.selection_material_already(mid) {
                    self.toast(&format!("{label} already on selection"));
                } else if self.assign_material_to_selection(mid) {
                    self.toast(&format!("{label} assigned to selection"));
                } else {
                    self.toast("Could not assign material to this selection");
                }
            }
            OlAct::SelectByMaterial(mid) => self.select_by_material(mid),
            OlAct::ClearMap(nid, kind) => self.clear_material_map_channel(nid, kind),
            OlAct::FocusMapChannel(nid, kind, time) => {
                self.select_single_unified(SceneItemId::Node(nid));
                self.outliner.focus_map_channel = Some((nid, kind, time));
            }
            // ── Instance ──
            OlAct::CreateInstance(uid) => {
                self.select_single_unified(SceneItemId::Prim(uid));
                crate::ops::instance::create::run(self);
            }
            OlAct::MakeUnique(uid) => {
                if let Some(i) = self.prim_index_by_uid(uid) {
                    self.make_instance_unique(i);
                }
            }
            OlAct::SelectMaster(master_uid) => {
                self.select_single_unified(SceneItemId::Prim(master_uid));
                self.mark_dirty(DirtyFlags::RENDER);
            }
            OlAct::InstanceIntoAll(uid) => {
                self.select_single_unified(SceneItemId::Prim(uid));
                self.instance_into_all_bodies();
            }
            // ── Poly / mesh ──
            OlAct::Subdivide(nid) => {
                self.select_single_unified(SceneItemId::Node(nid));
                self.poly_subdivide();
                self.outliner.outliner_dirty = true;
            }
            OlAct::ConvertToSdf(nid) => {
                self.convert_poly_to_sdf(nid);
                self.outliner.outliner_dirty = true;
            }
            OlAct::LinkAsSdf(nid) => {
                self.link_poly_as_sdf(nid);
                self.outliner.outliner_dirty = true;
            }
            OlAct::ExportObj(nid) => {
                let node_name = self
                    .scene
                    .scene_graph
                    .get(nid)
                    .map(|n| n.payload.display_name().to_string())
                    .unwrap_or_else(|| "mesh".into());
                let default_name = format!("{}.obj", node_name.replace(' ', "_"));
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export as OBJ")
                    .add_filter("Wavefront OBJ", &["obj"])
                    .set_file_name(&default_name)
                    .save_file()
                {
                    self.export_node_obj(nid, &path.to_string_lossy());
                }
            }
            OlAct::BakeMeshMaps(nid) => {
                self.select_single_unified(SceneItemId::Node(nid));
                self.poly_request_mesh_map_bake_for(nid, None);
            }
            OlAct::SurfaceSculpt(nid) => {
                self.select_single_unified(SceneItemId::Node(nid));
                self.begin_surface_sculpt(nid);
            }
        }
    }

    fn ol_select_children(&mut self, nid: NodeId) {
        self.clear_selection_unified();
        let mut ids: Vec<SceneItemId> = Vec::new();
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::SdfBody { primitives, .. } = &node.payload {
                for p in primitives {
                    ids.push(SceneItemId::Prim(p.prim_uid));
                }
            }
            for &c in &node.children {
                ids.push(SceneItemId::Node(c));
            }
        }
        self.extend_selection_unified(&ids);
    }

    fn ol_select_hierarchy(&mut self, nid: NodeId) {
        self.clear_selection_unified();
        // Collect the whole subtree first (read-only walk), then go through the
        // canonical bulk writer. `nid` is pushed first, so it becomes active.
        fn walk(app: &FourthLayerApp, nid: NodeId, ids: &mut Vec<SceneItemId>) {
            ids.push(SceneItemId::Node(nid));
            if let Some(node) = app.scene.scene_graph.get(nid) {
                if let NodePayload::SdfBody { primitives, .. } = &node.payload {
                    for p in primitives {
                        ids.push(SceneItemId::Prim(p.prim_uid));
                    }
                }
                for &c in &node.children {
                    walk(app, c, ids);
                }
            }
        }
        let mut ids = Vec::new();
        walk(self, nid, &mut ids);
        self.extend_selection_unified(&ids);
    }

    /// Unparent: nested body/mesh → graph root; prim → new top-level body (or parent body if nested).
    fn ol_unparent_selection(&mut self) {
        // Prefer active node if hierarchy-nested (SDF under body OR poly under poly / group).
        if let Some(nid) = self.sel.usel.active_node() {
            if self.is_outliner_nested_node(nid) {
                let root = self.scene.scene_graph.root();
                self.transaction("Un-nest", |s| {
                    s.reparent_body_node(nid, root);
                    s.select_after_create(&[SceneItemId::Node(nid)]);
                });
                return;
            }
        }
        let prims: Vec<u32> = self.sel.usel.selected_prims().collect();
        if !prims.is_empty() {
            self.transaction("Unparent prims", |s| {
                s.move_prims_to_new_root_body(&prims);
            });
        }
    }
}
