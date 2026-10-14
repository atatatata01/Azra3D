//! Scene-edit commands invoked from gizmo/hotkeys: delete, duplicate, focus.
//! Pure relocation from gizmo.rs (degod slice 22 content — stays under gizmo/**).

use glam::Vec3;

/// When true, prim.position is body-local; body-level transforms only update
/// body.origin / body_rotation / body_scale — no prim mutations, no rebake.
/// Must stay in sync with the same flag in body_volume_cache.rs, serialize.rs,
/// and project_load.rs.
use crate::BODY_LOCAL_PRIMS;

use crate::{
    FourthLayerApp, ActiveTool, ToolMode, PolyTool, GizmoMode, DirtyFlags,
    WorkMode, SceneItemId,
};

impl FourthLayerApp {
    pub(crate) fn delete_selected(&mut self) {
        eprintln!("[ACT] delete_selected active_node={:?} has_prims={} active_body={:?}",
            self.sel.usel.active_node(), self.sel.usel.has_prims(), self.scene.active_body_nid);
        self.tool.depth_pick = None;

        // --- SDF prim deletion (existing) ---
        if self.sel.usel.has_prims() {
            let idx: Vec<usize> = self.selected_prim_indices_unified();
            // Empty resolve (stale selection) → no empty undo.
            if idx.is_empty() { return; }
            let c = idx.len();
            self.transaction("Delete selected", |s| {
                s.remove_prims_from_graph(&idx);
                // Clear only prim items from unified selection; node/light preserved.
                s.remove_from_selection(|id| matches!(id, SceneItemId::Prim(_)));
                if s.sel.edit_target.is_some() {
                    s.clear_edit_selection();
                    s.tool.work_mode = WorkMode::Object;
                }
                s.rebake_no_undo(); s.mark_dirty(DirtyFlags::RENDER);
            });
            self.toast(&format!("Deleted {} object(s)", c));
            return;
        }

        // --- Scene graph node deletion — loops EVERY selected node (multi-select),
        // ONE transaction / undo entry. Kind gate: anything except Root.
        //
        // Lights are graph nodes (`NodePayload::Light`). ui2 outliner selects them as
        // `SceneItemId::Node` (not `SceneItemId::Light`), so they MUST go through this
        // arm — filtering them out made the default Sun undeletable (owner 2026-08-06).
        // SceneItemId::Light multi-select still uses the light arm below.
        // TOP-MOST ancestor filter prevents double-deleting a child whose parent is also selected.
        if self.sel.usel.has_nodes() {
            use core_math::scene_graph::NodePayload;
            let sel_nodes: Vec<core_math::scene_graph::NodeId> = self.sel.usel.selected_nodes()
                .filter(|&nid| self.scene.scene_graph.get(nid).map_or(false, |n|
                    !matches!(n.payload, NodePayload::Root)))
                .collect();
            let tops = crate::scene_actions::top_most_nodes(&self.scene.scene_graph, &sel_nodes);
            if !tops.is_empty() {
                let n = tops.len();
                let any_light = tops.iter().any(|&nid| {
                    self.scene.scene_graph.get(nid).map_or(false, |node|
                        matches!(node.payload, NodePayload::Light(_)))
                });
                let desc = if n == 1 { "Delete".to_string() } else { format!("Delete {} items", n) };
                self.transaction(&desc, |s| {
                    for &nid in &tops {
                        s.delete_node_no_undo(nid);
                    }
                    s.sync_flat_cache();
                    s.sanitize_selection();
                    s.sanitize_selection_unified();
                    s.poly_sel.selected_faces.clear();
                    s.poly_sel.selected_edges.clear();
                    s.poly_sel.selected_vertices.clear();
                    s.poly_sync_all_meshes();
                    s.rebake_no_undo();
                    s.mark_dirty(DirtyFlags::RENDER);
                    if any_light {
                        s.mark_dirty(DirtyFlags::LIGHT);
                    }
                    s.mark_outliner_dirty();
                });
                let msg = if n == 1 { "Deleted".to_string() } else { format!("Deleted {} items", n) };
                self.toast(&msg);
                return;
            }
        }

        // --- Light deletion (SceneItemId::Light selection — v1 outliner / pick) ---
        // Scene with zero lights is allowed (HDR / matcap only).
        if self.sel.usel.has_lights() {
            let light_ids: Vec<u32> = self.sel.usel.selected_lights().collect();
            if light_ids.is_empty() { return; }
            if light_ids.len() == 1 {
                self.delete_light(light_ids[0]);
            } else {
                let ids = light_ids.clone();
                let n = ids.len();
                self.transaction("Delete light(s)", |s| {
                    for &lid in &ids {
                        if let Some(nid) = s.light_graph_nid(lid) {
                            s.delete_node_no_undo(nid);
                        }
                    }
                    s.remove_from_selection(|id| matches!(id, SceneItemId::Light(_)));
                    s.mark_dirty(DirtyFlags::LIGHT | DirtyFlags::RENDER);
                    s.mark_outliner_dirty();
                });
                self.toast(&format!("Deleted {n} light(s)"));
            }
        }
    }
    /// After Duplicate (or at the start of it): drop Bevel / Create / Extrude / busy ops
    /// and arm **Grab (Translate)** so the new copy is immediately ready to move.
    pub(crate) fn exit_tools_arm_grab(&mut self) {
        // Finish or cancel any in-flight drag so we don't leave a dangling episode.
        if self.tool.current.is_busy() {
            let _ = self.confirm_operation();
        }
        if self.tool.bool_cut.is_some() {
            self.cancel_bool_cut();
        }
        // ToolMode::Select clears Bevel edge selection + exclusive modes (set_mode).
        if !matches!(self.tool.mode, ToolMode::Select) {
            self.set_mode(ToolMode::Select);
        } else {
            self.tool.hovered_bevel_edge = None;
            self.tool.selected_bevel_edges.clear();
            self.tool.uniform_bevel_hovered = false;
        }
        self.poly_tool = PolyTool::Idle;
        self.knife_points.clear();
        self.knife_snap_pos = None;
        self.tool.current = ActiveTool::Idle;
        self.tool.gizmo_mode = GizmoMode::Translate;
        self.tool.gizmo_armed = true;
    }

    pub(crate) fn duplicate_selected(&mut self) {
        // Always leave Bevel (and any other exclusive tool) so the duplicate is free to grab.
        self.exit_tools_arm_grab();
        eprintln!("[ACT] duplicate_selected active_node={:?} active_prim={:?} work_mode={:?}",
            self.sel.usel.active_node(), self.active_prim_index_unified(), self.tool.work_mode);
        // Voxel sculpt object: the grid is a SINGLETON, so Shift+D snapshots it into a
        // STATIC ImportedMesh copy rather than a second live voxel (multi-voxel sculpt is a
        // later phase — PRIMITIVE_SYSTEM_PLAN.md P5). Handled here because ImportedMesh has
        // no generic duplicate path and the current geometry must be re-extracted from the
        // grid (the node payload mesh is the un-sculpted base). Takes priority in the sculpt
        // room and whenever the voxel node itself is the active selection.
        // Voxel object duplicate (P0.5 multi-voxel): fire when a VoxelBody node is the active
        // selection (works in any mode, active or inactive voxel), OR — in the sculpt room with
        // nothing else selected — for the active live voxel (so Shift+D works mid-sculpt). NEVER
        // hijack a deliberately-selected prim/body/light; those fall through to the normal path.
        let sel_node = self.sel.usel.active_node();
        let sel_is_voxel = sel_node.map_or(false, |n| matches!(
            self.scene.scene_graph.get(n).map(|x| &x.payload),
            Some(core_math::scene_graph::NodePayload::VoxelBody { .. })));
        let dup_voxel = if sel_is_voxel {
            sel_node
        } else if self.tool.work_mode == WorkMode::Sculpt
            && self.active_prim_index_unified().is_none()
            && sel_node.is_none()
            && self.sel.usel.active_light().is_none() {
            self.sculpt.voxel.as_ref().map(|v| v.node)
        } else { None };
        if let Some(vn) = dup_voxel {
            self.duplicate_voxel_object(vn);
            return;
        }
        // SDF primitive duplicate — duplicates EVERY selected prim (multi-select),
        // ONE undo entry, new copies become the selection (originals deselected).
        let sel_indices: Vec<usize> = self.selected_prim_indices_unified();
        if !sel_indices.is_empty() {
            let desc = if sel_indices.len() == 1 {
                format!("Duplicate {}", self.scene.primitives.get(sel_indices[0]).map(|p| p.name.clone()).unwrap_or_default())
            } else {
                format!("Duplicate {} prims", sel_indices.len())
            };
            let sel_uids: Vec<u32> = sel_indices.iter()
                .filter_map(|&i| self.scene.primitives.get(i).map(|p| p.prim_uid))
                .collect();
            self.transaction(&desc, |s| {
                let new_uids = s.duplicate_prims_no_undo(&sel_uids);
                let new_ids: Vec<SceneItemId> = new_uids.iter().map(|&u| SceneItemId::Prim(u)).collect();
                s.select_after_create(&new_ids);
                // Grab ready (Translate armed) — mode already set by exit_tools_arm_grab.
                s.tool.gizmo_mode = app_types::GizmoMode::Translate;
                s.tool.gizmo_armed = true;
                s.rebake_no_undo();
            });
            return;
        }
        // Scene graph node duplicate — iterates EVERY selected node (multi-select), ONE undo
        // entry, new copies become the selection. SdfBody → duplicate_subtree (canonical);
        // Poly/NURBS/Empty/Group → duplicate_scene_node_no_undo; other kinds skip gracefully
        // (voxel is handled by the dedicated arm above when active; lights are SceneItemId::Light,
        // not nodes). Mirrors the d2e7d15 multi-prim pattern.
        let mut sel_nodes: Vec<core_math::scene_graph::NodeId> = self.sel.usel.selected_nodes().collect();
        if !sel_nodes.is_empty() {
            // Active node first → first duplicate becomes the new active (select_after_create).
            if let Some(active) = self.sel.usel.active_node() {
                if let Some(pos) = sel_nodes.iter().position(|&n| n == active) {
                    sel_nodes.swap(0, pos);
                }
            }
            // TOP-MOST filter (same rule Blender applies): if a selected node's ANCESTOR is also
            // selected, skip it — duplicate_subtree on the ancestor already copies the whole
            // subtree, so duplicating the descendant too would double it (once inside the
            // ancestor's copy, once standalone). READ PHASE: filter + collect BEFORE mutating —
            // duplication grows the graph, so ancestry must be resolved against the pre-dup graph.
            let tops: Vec<core_math::scene_graph::NodeId> =
                crate::scene_actions::top_most_nodes(&self.scene.scene_graph, &sel_nodes);
            let desc = if tops.len() == 1 { "Duplicate Body".to_string() }
                       else { format!("Duplicate {} objects", tops.len()) };
            let mut n_dup = 0usize;
            self.transaction(&desc, |s| {
                let mut new_ids: Vec<SceneItemId> = Vec::with_capacity(tops.len());
                let mut first_sdf: Option<core_math::scene_graph::NodeId> = None;
                for &nid in &tops {
                    if let Some(new_nid) = s.duplicate_node_no_undo(nid) {
                        if first_sdf.is_none() && s.scene.scene_graph.get(new_nid)
                            .map(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                            .unwrap_or(false)
                        {
                            first_sdf = Some(new_nid);
                        }
                        new_ids.push(SceneItemId::Node(new_nid));
                    }
                }
                n_dup = new_ids.len();
                if !new_ids.is_empty() {
                    // Same active-body switch as the single path (first SDF duplicate wins).
                    if let Some(b) = first_sdf { s.scene.active_body_nid = b; }
                    // All copies selected, originals deselected, active = first copy.
                    s.select_after_create(&new_ids);
                    s.tool.gizmo_mode = app_types::GizmoMode::Translate;
                    s.tool.gizmo_armed = true;
                }
            });
            match n_dup {
                0 => self.toast("Nothing duplicated — selected node kind(s) not duplicable"),
                1 => self.toast("Duplicated object"),
                n => self.toast(&format!("Duplicated {n} objects")),
            }
            return;
        }
        // Light duplicate — iterates EVERY selected light (multi-select), ONE undo entry,
        // copies become the selection (originals deselected). Mirrors the multi-prim /
        // multi-node arms above; per-light logic = duplicate_light_no_undo (scene_actions.rs),
        // the same no-undo-core extraction pattern as batch-1's delete_node_no_undo.
        let sel_lights: Vec<u32> = self.sel.usel.selected_lights().collect();
        if !sel_lights.is_empty() {
            let desc = if sel_lights.len() == 1 { "Duplicate Light".to_string() }
                       else { format!("Duplicate {} lights", sel_lights.len()) };
            let n = sel_lights.len();
            self.transaction(&desc, |s| {
                let mut new_ids: Vec<SceneItemId> = Vec::with_capacity(sel_lights.len());
                for &lid in &sel_lights {
                    if let Some(new_id) = s.duplicate_light_no_undo(lid) {
                        new_ids.push(SceneItemId::Light(new_id));
                    }
                }
                s.select_after_create(&new_ids);
                s.mark_dirty(DirtyFlags::LIGHT);
                s.mark_outliner_dirty();
            });
            self.toast(&if n == 1 { "Light duplicated".to_string() }
                       else { format!("Duplicated {n} lights") });
        }
    }
    /// Per-prim duplication primitive (NO transaction, NO selection, NO toast) — clones each
    /// prim into its SOURCE body with a fresh uid + name. Position is NOT offset: the copy
    /// sits on the original until the user moves it (grab is armed by `duplicate_selected` /
    /// Shift+D). Matches `duplicate_prim_for_transform`. Body/voxel paths still use
    /// `tweak.duplicate_offset`. THE prim-copy core (two-code-paths rule): looped by
    /// `duplicate_selected()`'s prim arm AND by the outliner Ctrl-drag-duplicate drop (which
    /// then places the copies via `move_prims_to_body_slot`). Returns the new uids in input
    /// order. Caller owns the surrounding `transaction()`, selection, and `rebake_no_undo()`.
    pub(crate) fn duplicate_prims_no_undo(&mut self, prim_uids: &[u32]) -> Vec<u32> {
        // READ PHASE: clone all sources + their body nids FIRST — flat indices go
        // stale as soon as the first add_prim_to_graph() runs sync_flat_cache().
        let sources: Vec<(crate::SdfPrimitive, core_math::scene_graph::NodeId)> = prim_uids.iter()
            .filter_map(|&uid| self.prim_index_by_uid(uid))
            .filter(|&i| i < self.scene.prim_body_map.len())
            .map(|i| (self.scene.primitives[i].clone(), self.scene.prim_body_map[i].0))
            .collect();
        let mut new_uids: Vec<u32> = Vec::with_capacity(sources.len());
        for (mut d, source_body) in sources {
            // Ensure duplicate goes into the SOURCE prim's body, not a stale active_body_nid
            self.scene.active_body_nid = source_body;
            let uid = self.scene.next_prim_id;
            self.scene.next_prim_id += 1;
            d.prim_uid = uid; d.instance_master_id = None;
            d.name = self.next_name(d.shape.label());
            // No position shift — user moves with the armed grab gizmo.
            self.add_prim_to_graph(d);
            // Track by STABLE uid, never by add_prim_to_graph's returned flat index:
            // that index is the GLOBAL primitives.len()-1, which only matches the new
            // prim when the active body is LAST in DFS order (Stable-IDs rule).
            new_uids.push(uid);
        }
        new_uids
    }
    /// Per-node duplication primitive (NO transaction, NO selection, NO toast) — called in a
    /// loop by `duplicate_selected()`'s node arm inside ONE transaction, and by the outliner
    /// Ctrl-drag-duplicate body drop. Dispatch mirrors the historical single-node arm:
    /// SdfBody → subtree copy + fresh-bake wiring;
    /// Light → the canonical `duplicate_light_no_undo` core;
    /// Poly/NURBS/Empty/Group → shallow node copy. Unsupported kinds return None (skipped).
    pub(crate) fn duplicate_node_no_undo(&mut self, nid: core_math::scene_graph::NodeId) -> Option<core_math::scene_graph::NodeId> {
        let is_sdf_body = self.scene.scene_graph.get(nid)
            .map(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
            .unwrap_or(false);
        if is_sdf_body {
            return self.duplicate_sdf_body_no_undo(nid);
        }
        // Lights are graph nodes (`NodePayload::Light`) and the ui2 outliner selects them as
        // `SceneItemId::Node` (its rows only carry `OlItem::Node`/`OlItem::Prim`) — exactly the
        // same route that once made the default Sun undeletable (`delete_selected`, 2026-08-06).
        // Shift+D on an outliner-selected light therefore lands HERE, not in
        // `duplicate_selected()`'s `SceneItemId::Light` arm, and `duplicate_scene_node_no_undo`
        // has no Light case: it returned None, the node arm `return`ed on "not duplicable", and
        // the light arm below it never ran. Delegate to THE light-copy core rather than growing a
        // second light-copy path (id allocation, `Point.NNN` naming, and the +X offset all live
        // there); both routes now produce identical copies.
        let light_src = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            core_math::scene_graph::NodePayload::Light(d) => Some(d.id),
            _ => None,
        });
        if let Some(src_id) = light_src {
            let new_id = self.duplicate_light_no_undo(src_id)?;
            self.mark_dirty(DirtyFlags::LIGHT);
            self.mark_outliner_dirty();
            // Node form for the caller's selection — matches how the outliner selects lights.
            return self.light_graph_nid(new_id);
        }
        self.duplicate_scene_node_no_undo(nid)
    }
    /// SdfBody duplication primitive (NO transaction/selection/toast — caller owns those).
    /// Reuses the canonical `duplicate_subtree()` (scene_actions.rs), then wires the copy to
    /// bake EXACTLY like a fresh body.
    fn duplicate_sdf_body_no_undo(&mut self, nid: core_math::scene_graph::NodeId) -> Option<core_math::scene_graph::NodeId> {
        let parent = self.scene.scene_graph.get(nid).and_then(|n| n.parent)
            .unwrap_or(self.scene.scene_graph.root());
        let new_nid = self.duplicate_subtree(nid, parent)?;
        let src_prims = self.scene.scene_graph.body_primitives(nid).map(|p| p.len()).unwrap_or(0);
        let new_prims_graph = self.scene.scene_graph.body_primitives(new_nid).map(|p| p.len()).unwrap_or(0);
        eprintln!("[ACT] duplicate_body src={nid:?}({src_prims} prims) new={new_nid:?}({new_prims_graph} prims in GRAPH node)");
        // Offset the duplicated body's origin.
        // With BODY_LOCAL_PRIMS: prim positions are body-local so they stay
        // relative to the origin — only the origin needs offsetting.
        let offset = Vec3::new(self.tweak.duplicate_offset, 0.0, 0.0);
        if let Some(node) = self.scene.scene_graph.get_mut(new_nid) {
            if let core_math::scene_graph::NodePayload::SdfBody { origin, primitives, .. } = &mut node.payload {
                *origin += offset;
                if !BODY_LOCAL_PRIMS {
                    // Legacy: prims are world-space, must be shifted too
                    for p in primitives.iter_mut() {
                        p.position += offset;
                    }
                }
            }
        }
        self.sync_flat_cache();
        let pbm_count = self.scene.prim_body_map.iter().filter(|(b, _)| *b == new_nid).count();
        eprintln!("[ACT] duplicate_body after sync: new={new_nid:?} prim_body_map_count={pbm_count} (this is what the OUTLINER lists) flat_total={}", self.scene.primitives.len());
        // Make the duplicate bake EXACTLY like a fresh body: allocate its mesh-cache id
        // and mark it dirty, then let the normal bake → DC pipeline produce the mesh.
        //
        // The previous "instant appearance" path (clone source grid via duplicate_body, then
        // PRE-DISPATCH dispatch_grid_dc_async at generation 0) is exactly what BROKE the
        // duplicate: that gen-0 DC job ran ~266ms in-flight, which DEDUPED the real bake's DC
        // dispatch (app_poll.rs:556 `if pending_dc_jobs has this body { return }`), and the
        // gen-0 result was then discarded as stale (generation < current). Net: the duplicate's
        // mesh never landed until a manual full rebake (the "toggle cache size makes it appear"
        // symptom). A fresh body issues exactly ONE clean DC, so it renders reliably.
        if self.cache.use_body_vol_cache {
            let cid = self.cache.mesh_cache.alloc_id();
            if let Some(node) = self.scene.scene_graph.get_mut(new_nid) {
                if let core_math::scene_graph::NodePayload::SdfBody { mesh_cache_id, .. } = &mut node.payload {
                    *mesh_cache_id = Some(cid);
                }
            }
            self.cache.body_vol_cache.mark_body_dirty(new_nid);
        }
        // Atlas slot allocation deferred to dispatch_pending_primary_bakes().
        // Single owner: poll/dispatch phase owns all atlas mutations.
        // Duplicate appears after one frame delay (same as Blender).
        self.mark_node_body_vol_dirty(new_nid);
        self.mark_dirty(DirtyFlags::SCENE);
        self.mark_outliner_dirty();
        Some(new_nid)
    }
    /// Shallow scene-node duplication primitive (Poly/NURBS/Empty/Group; NO transaction/
    /// selection/toast — caller owns those). Returns None for unsupported payload kinds.
    fn duplicate_scene_node_no_undo(&mut self, nid: core_math::scene_graph::NodeId) -> Option<core_math::scene_graph::NodeId> {
        use core_math::scene_graph::NodePayload;
        // Extract cloned data first so the scene_graph borrow ends before we call next_unique_name
        use core_math::primitives::PbrMaterial;
        enum DupData {
            Poly(
                String,
                std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
                PbrMaterial,
                Vec<core_math::scene_graph::PolyModifier>,
            ),
            Nurbs(String, std::sync::Arc<core_math::nurbs::NurbsSurface>, PbrMaterial),
            Empty(String),
            Group(String),
        }
        let (parent, pos, rot, scale, dup_data) = {
            let node = self.scene.scene_graph.get(nid)?;
            let parent = node.parent.unwrap_or(self.scene.scene_graph.root());
            let data = match &node.payload {
                NodePayload::PolygonalBody {
                    name,
                    mesh,
                    material,
                    modifiers,
                    ..
                } => DupData::Poly(
                    name.clone(),
                    mesh.clone(),
                    material.clone(),
                    modifiers.clone(),
                ),
                NodePayload::NurbsBody { name, surface, material, .. } =>
                    DupData::Nurbs(name.clone(), surface.clone(), material.clone()),
                NodePayload::Empty { name } => DupData::Empty(name.clone()),
                NodePayload::Group { name } => DupData::Group(name.clone()),
                _ => return None, // unsupported kind — caller skips gracefully
            };
            (parent, node.local_position, node.local_rotation, node.local_scale, data)
        };
        // Poly / NURBS / Empty / Group: stay in place (Blender Shift+D). SDF bodies still
        // use duplicate_offset so the copy is visible beside the original.
        let offset = Vec3::ZERO;
        let payload = match dup_data {
            DupData::Poly(name, mesh, material, modifiers) => NodePayload::PolygonalBody {
                name: self.next_unique_name(&name),
                mesh,
                source: Default::default(),
                mesh_cache_id: None,
                material,
                modifiers,
            },
            DupData::Nurbs(name, surface, material) => NodePayload::NurbsBody {
                name: self.next_unique_name(&name),
                surface, mesh_cache_id: None, material,
            },
            DupData::Empty(name) => NodePayload::Empty { name: self.next_unique_name(&name) },
            DupData::Group(name) => NodePayload::Group { name: self.next_unique_name(&name) },
        };
        let new_nid = self.scene.scene_graph.add_child_with_transform(parent, payload, pos + offset, rot, scale);
        self.poly_request_display_sync(new_nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.mark_outliner_dirty();
        Some(new_nid)
    }
    /// Ctrl/Shift+D on a voxel: make a REAL independent `VoxelBody` copy — its own cloned grid plus a
    /// fresh display mesh — placed beside `src_nid`. If `src_nid` is the ACTIVE voxel, its live sculpt
    /// is flushed (atlas → CPU grid → node) first so the copy captures the current surface. The copy is
    /// INACTIVE (renders via its display mesh); select it + enter Sculpt (`voxel_enter_edit`) to sculpt
    /// it. Multi-voxel object model (P0.5) — replaces the old frozen ImportedMesh snapshot.
    pub(crate) fn duplicate_voxel_object(&mut self, src_nid: core_math::scene_graph::NodeId) {
        use core_math::scene_graph::NodePayload;
        // Active voxel? Flush its live sculpt into the node grid first (no-op for an inactive voxel,
        // whose node grid is already current). materialize_cpu_from_atlas handles the stale-CPU case.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.node == src_nid) {
            self.materialize_cpu_from_atlas();
            self.voxel_sync_node_mesh();
        }
        let (parent, pos, rot, scale, grid, material) = match self.scene.scene_graph.get(src_nid) {
            Some(n) => {
                let parent = n.parent.unwrap_or(self.scene.scene_graph.root());
                match &n.payload {
                    NodePayload::VoxelBody { grid, material, .. } =>
                        (parent, n.local_position, n.local_rotation, n.local_scale, grid.clone(), material.clone()),
                    _ => return, // not a voxel — caller guards this, but stay defensive
                }
            }
            None => return,
        };
        let (verts, idx) = grid.object_display_mesh(); // Display Invariant: bounded mesh (downsampled >512³)
        let tris = (idx.len() / 3) as u32;
        if verts.is_empty() || idx.is_empty() {
            self.toast("Nothing to duplicate — voxel has no surface");
            return;
        }
        let name = self.next_unique_name("Voxel Sculpt");
        let offset = Vec3::new(self.tweak.duplicate_offset, 0.0, 0.0);
        self.transaction("Duplicate Voxel", |s| {
            let mid = s.cache.mesh_cache.alloc_id();
            s.cache.mesh_cache.insert_imported(mid, (verts, idx), tris);
            let new_nid = s.scene.scene_graph.add_child_with_transform(parent, NodePayload::VoxelBody {
                name, grid, mesh_cache_id: Some(mid), material,
            }, pos + offset, rot, scale);
            s.select_single_unified(SceneItemId::Node(new_nid));
            s.mark_dirty(DirtyFlags::RENDER);
            s.mark_outliner_dirty();
        });
        self.toast("Duplicated voxel object");
    }
    /// Re-aim look-at at the selection **without moving the camera eye** (Blender focus parity).
    ///
    /// Only `camera.target` and `distance` change so the same world point stays under the
    /// view ray. Never called from plain click-select — that path must not reframe.
    pub(crate) fn focus_selected(&mut self) {
        let world_pos = if let Some(idx) = self.active_prim_index_unified() {
            if idx >= self.scene.primitives.len() {
                return;
            }
            // BODY_LOCAL_PRIMS: prim.position is body-local; convert to world-space for camera target.
            if BODY_LOCAL_PRIMS {
                if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                    let bt = self.scene.scene_graph.body_world_transform(body_nid).unwrap_or_default();
                    bt.local_to_world(self.scene.primitives[idx].position)
                } else {
                    self.scene.primitives[idx].position
                }
            } else {
                self.scene.primitives[idx].position
            }
        } else if let Some(nid) = self.sel.usel.active_node() {
            match self.scene.scene_graph.get(nid) {
                Some(node) => node.local_position,
                None => return,
            }
        } else if let Some(lid) = self.sel.usel.active_light() {
            match self.scene.light_by_id(lid) {
                // All lights (incl. sun) use gizmo_position = authored position.
                Some(data) => data.gizmo_position(),
                None => return,
            }
        } else {
            return;
        };
        self.retarget_camera_keep_eye(world_pos);
        self.mark_dirty(DirtyFlags::RENDER);
    }
}
