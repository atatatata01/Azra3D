//! Outliner panel — outer frame, filter bar, section dispatch.

use core_math::primitives::BooleanOp;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::*;

/// Transient per-frame outliner actions — set during tree draw, processed after.
#[derive(Default)]
pub(crate) struct OutlinerDeferred {
    // Scene graph node actions
    pub toggle_vis: Option<NodeId>,
    pub toggle_lock: Option<NodeId>,
    pub delete_node: Option<NodeId>,
    pub duplicate_node: Option<NodeId>,
    pub subdivide_node: Option<NodeId>,
    pub export_node: Option<NodeId>,
    pub convert_action: Option<(NodeId, String)>,
    /// LAB poly→SDF: convert this poly node into a MeshVolume SDF primitive (destructive).
    pub convert_to_sdf: Option<NodeId>,
    /// LAB poly→SDF: LINK this poly node as a live SDF modifier (poly kept, hidden).
    pub link_to_sdf: Option<NodeId>,
    /// 3D-Coat Surface sculpt on this poly or voxel node.
    pub surface_sculpt: Option<NodeId>,

    // Light actions (index = light.id)
    pub light_duplicate: Option<u32>,
    pub light_delete: Option<u32>,

    // Material node actions (Materials section — outliner_draw_material.rs)
    pub mat_create: bool,
    pub mat_duplicate: Option<NodeId>,
    pub mat_delete: Option<NodeId>,
    /// "Clear Map" on a material's map-channel child row: (material node, channel).
    pub mat_clear_map: Option<(NodeId, core_math::pbr_material::MapKind)>,

    // SDF prim actions
    pub rename_commit: Option<(usize, String)>,
    pub bool_change: Option<(u32, BooleanOp)>,

    // Layer actions
    pub layer_vis_toggle: Option<u32>,
    pub layer_rename_commit: Option<(u32, String)>,
    pub layer_select: Option<u32>,
    pub dissolve_layer_id: Option<u32>,

    // Object actions
    pub obj_select: Option<u32>,
    pub obj_rename_commit: Option<(u32, String)>,
    pub dissolve_obj_id: Option<u32>,

    // Drag reparent (resolved at release) — Vec for multi-selection support
    pub reparent: Option<(Vec<usize>, SceneNodeId)>,

    // Alt-solo (defer scene_graph mutation out of draw phase)
    pub solo_node: Option<NodeId>,

    // Body library
    pub save_to_library: Option<NodeId>,

    // (Body nest-on-drop is handled inline in outliner_dnd.rs — supports multi-body + Ctrl-copy.)

    // Modifier key snapshot (captured once at frame start)
    pub ctrl: bool,
}

impl AzraApp {
    pub(crate) fn process_outliner_deferred(&mut self, d: OutlinerDeferred) {
        // --- Scene graph node actions ---
        if let Some(nid) = d.toggle_vis {
            self.transaction("Toggle visibility", |s| {
                if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                    node.visible = !node.visible;
                }
                s.mark_visibility_dirty();
                s.mark_node_body_vol_dirty(nid);
                s.cache.prim_buf_stale = true;
                s.outliner.outliner_dirty = true;
            });
        }
        if let Some(nid) = d.toggle_lock {
            self.transaction("Toggle lock", |s| {
                if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                    node.locked = !node.locked;
                }
                s.outliner.outliner_dirty = true;
            });
        }
        if let Some(nid) = d.subdivide_node {
            self.select_single_unified(SceneItemId::Node(nid));
            self.poly_subdivide();
            self.outliner.outliner_dirty = true;
        }
        if let Some(nid) = d.convert_to_sdf {
            self.convert_poly_to_sdf(nid);
            self.outliner.outliner_dirty = true;
        }
        if let Some(nid) = d.link_to_sdf {
            self.link_poly_as_sdf(nid);
            self.outliner.outliner_dirty = true;
        }
        if let Some(nid) = d.surface_sculpt {
            self.select_single_unified(SceneItemId::Node(nid));
            self.begin_surface_sculpt(nid);
            self.outliner.outliner_dirty = true;
        }
        if let Some(nid) = d.duplicate_node {
            // Multi-selection aware + ONE code path (CLAUDE "two code paths" rule): route
            // through duplicate_selected() — the same multi-node path Shift+D uses (top-most
            // filter, per-node duplicate_node_no_undo with fresh-bake wiring, copies selected).
            // If the right-clicked node is NOT part of the selection, single-select it first
            // (select-then-duplicate-single, matching the prim context menu rule). This
            // replaces the former inline duplicate_subtree copy that ignored multi-selection.
            if !self.sel.usel.selected.contains(&SceneItemId::Node(nid)) {
                self.select_single_unified(SceneItemId::Node(nid));
            }
            self.duplicate_selected();
            self.outliner.outliner_dirty = true;
        }
        if let Some((nid, target)) = d.convert_action {
            // convert_* open their own transaction (and refuse before tx on failure).
            // Do NOT wrap in a second "Convert node" entry — that double-pushed undo on
            // success and left an empty outer entry when convert refused.
            match target.as_str() {
                "poly" => {
                    if let Some(node) = self.scene.scene_graph.get(nid) {
                        match &node.payload {
                            NodePayload::SdfBody { .. } => self.convert_sdf_to_poly(nid),
                            NodePayload::ImportedMesh { .. } => self.convert_imported_to_poly(nid),
                            NodePayload::NurbsBody { .. } => self.convert_nurbs_to_poly(nid),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            self.outliner.outliner_dirty = true;
        }
        if let Some(nid) = d.export_node {
            let node_name = self.scene.scene_graph.get(nid)
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
        if let Some(nid) = d.delete_node {
            // Routed through the shared per-node deletion core (two-code-paths rule) —
            // same cleanup as Del / delete_selected: SDF subtree flat prims, subtree mesh
            // + volume caches, active-voxel working set, sdf_body_nid/active/isolation
            // reassignment, shared-source purge, deselect.
            self.transaction("Delete node", |s| {
                s.delete_node_no_undo(nid);
                s.sync_flat_cache();
                s.sanitize_selection();
                s.sanitize_selection_unified();
                s.poly_sync_all_meshes();
                s.rebake_no_undo();
                s.mark_dirty(DirtyFlags::RENDER);
                s.outliner.outliner_dirty = true;
            });
        }

        // --- Alt-solo (sibling-scoped) ---
        if let Some(nid) = d.solo_node {
            let parent = self.scene.scene_graph.get(nid)
                .and_then(|n| n.parent)
                .unwrap_or(self.scene.scene_graph.root());
            let siblings: Vec<NodeId> = self.scene.scene_graph.children(parent).to_vec();
            let already_solo = siblings.iter().all(|&other| {
                self.scene.scene_graph.get(other).map(|n| {
                    (other == nid && n.visible) || (other != nid && !n.visible)
                }).unwrap_or(true)
            });
            if already_solo {
                // Restore only if something is still hidden (single sibling is a no-op).
                let needs = siblings.iter().any(|&other| {
                    self.scene.scene_graph.get(other).map(|n| !n.visible).unwrap_or(false)
                });
                if needs {
                    self.transaction("Restore visibility", |s| {
                        for &other in &siblings {
                            if let Some(n) = s.scene.scene_graph.get_mut(other) { n.visible = true; }
                            s.mark_node_body_vol_dirty(other);
                        }
                        s.mark_dirty(DirtyFlags::RENDER);
                        s.cache.prim_buf_stale = true;
                        s.outliner.outliner_dirty = true;
                    });
                }
            } else {
                self.transaction("Solo", |s| {
                    for &other in &siblings {
                        if let Some(n) = s.scene.scene_graph.get_mut(other) { n.visible = other == nid; }
                        s.mark_node_body_vol_dirty(other);
                    }
                    s.mark_dirty(DirtyFlags::RENDER);
                    s.cache.prim_buf_stale = true;
                    s.outliner.outliner_dirty = true;
                });
            }
        }

        // --- Light actions ---
        if let Some(li) = d.light_duplicate { self.duplicate_light(li); self.outliner.outliner_dirty = true; }
        if let Some(li) = d.light_delete { self.delete_light(li); self.outliner.outliner_dirty = true; }

        // --- Material node actions (Materials section) ---
        if d.mat_create { self.create_material_interactive(); }
        if let Some(nid) = d.mat_duplicate { self.duplicate_material_node(nid); }
        if let Some(nid) = d.mat_delete { self.delete_material_node(nid); }
        if let Some((nid, kind)) = d.mat_clear_map { self.clear_material_map_channel(nid, kind); }

        // (SDF prim reorder: between-row slots resolve to drop_insert and apply inline in
        // outliner_dnd.rs via move_prims_to_body_slot — the ONE placement path.)

        // --- Reparent (drag prim(s) onto layer/object header) ---
        if let Some((indices, target)) = d.reparent {
            let movable: Vec<usize> = indices.iter()
                .filter(|&&fi| fi < self.scene.primitives.len() && !self.scene.primitives[fi].locked)
                .copied().collect();
            let locked_count = indices.len() - movable.len();
            if locked_count > 0 && movable.is_empty() {
                self.toast("Cannot move locked primitive(s)");
            } else if !movable.is_empty() {
                // Layer/Object reparent targets removed — scene graph handles hierarchy.
                // Do NOT open an empty undo entry (was a pure mark-dirty fossil).
                let _ = (target, movable);
            }
        }

        // --- Rename / boolean change ---
        if let Some((idx, new_name)) = d.rename_commit {
            if idx < self.scene.primitives.len() && !new_name.is_empty() {
                // Same name → no empty undo (double-click rename + Enter without edit).
                if self.scene.primitives[idx].name != new_name {
                    self.transaction("Rename primitive", |s| {
                        s.write_prim(idx, |p| p.name = new_name);
                        s.outliner.outliner_dirty = true;
                    });
                }
            }
        }
        if let Some((uid, new_op)) = d.bool_change {
            self.push_cmd(AppCommand::SetBooleanOp(uid, new_op));
        }

        // Layer/object actions removed — scene graph is sole hierarchy

        // --- Save to Library (open dialog: name + kitbash folder) ---
        if let Some(nid) = d.save_to_library {
            // Group / SdfObject / PolygonalBody: save the node itself.
            // SdfBody: top-level body.
            let save_nid = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::Group { .. })
                | Some(NodePayload::SdfObject { .. })
                | Some(NodePayload::PolygonalBody { .. }) => nid,
                _ => self.scene.scene_graph.top_level_body(nid),
            };
            // Dialog only — do NOT force-open Body Library or rescan here.
            // That used to hitch/crash (scan + GPU thumb while PT/bake was busy).
            self.open_library_save_dialog(save_nid);
        }
    }

    /// Recursively collect all mesh cache ids from a node and its descendants.
    /// pub(crate): also used by scene_actions::delete_node_no_undo (shared deletion core).
    pub(crate) fn collect_subtree_caches(
        sg: &core_math::scene_graph::SceneGraph,
        nid: NodeId,
        out: &mut Vec<core_math::scene_graph::MeshCacheId>,
    ) {
        if let Some(node) = sg.get(nid) {
            let cache_id = match &node.payload {
                NodePayload::PolygonalBody { mesh_cache_id, .. } => *mesh_cache_id,
                NodePayload::ImportedMesh { mesh_cache_id, .. } => Some(*mesh_cache_id),
                NodePayload::SdfBody { mesh_cache_id, .. } => *mesh_cache_id,
                _ => None,
            };
            if let Some(cid) = cache_id {
                out.push(cid);
            }
        }
        for &child in sg.children(nid) {
            Self::collect_subtree_caches(sg, child, out);
        }
    }
}
