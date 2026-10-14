//! Flatten scene graph into outliner rows.
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::super::*; // OlKind, etc. from ui2
use super::types::*;

/// The `outliner.renaming` key for a NODE row. Chosen from the PAYLOAD, never from `OlItem`:
/// `OlItem::Node` collapses SdfBody / Light / poly / camera into one variant, while the rename
/// state keys an SdfBody as `Body(nid)` and a light as `Light(light.id)`. Handing those a
/// `Node(nid)` would never match the state and the rename would silently never start.
/// `Root` stores no name, so it is not renameable.
/// Materials use `Node(nid)` (same as `trigger_rename` from the Materials section / F2).
pub(crate) fn ol_rename_target(payload: &NodePayload, nid: NodeId) -> Option<SceneNodeId> {
    match payload {
        NodePayload::Root => None,
        NodePayload::SdfBody { .. } => Some(SceneNodeId::Body(nid)),
        NodePayload::Light(data) => Some(SceneNodeId::Light(data.id)),
        // Explicit Material arm so a future payload-split cannot reintroduce silent no-op rename.
        NodePayload::Material { .. } => Some(SceneNodeId::Node(nid)),
        _ => Some(SceneNodeId::Node(nid)),
    }
}

/// Every EXISTING graph material id → does it bind an albedo map. Built ONCE per outliner rebuild.
///
/// `SceneGraph::material_by_id` is a LINEAR SCAN of every node, so resolving it per prim per
/// frame would be O(prims x nodes) — a Frame Budget Rule violation on any real scene. One pass
/// over the materials up front turns each prim's lookup into a hash hit.
///
/// Presence in the map is load-bearing, not just the bool: a prim whose `material_id` does not
/// resolve must fall back to its embedded material, so "absent" and "present but untextured"
/// are different answers (see `ol_prim_tex`).
pub(crate) fn ol_material_maps(sg: &core_math::scene_graph::SceneGraph) -> std::collections::HashMap<u32, bool> {
    sg.materials().map(|(_, id, _, m)| (id.0, m.albedo_map.is_set())).collect()
}

/// How many textures a PRIM actually binds: its own atlas entry (`texture_id`, 1+ = a slot in
/// the texture atlas) plus its material's albedo map. `albedo_map` is the ONLY map slot in the
/// whole material model, so 2 is the hard ceiling — this is a real count, not a stand-in.
///
/// Material resolution mirrors the canonical order exactly (`body_first_albedo_map`, the GPU
/// serializer): a graph `material_id` wins, and a `material_id` that does NOT resolve falls back
/// to the prim's embedded copy rather than counting as untextured.
pub(crate) fn ol_prim_tex(mats: &std::collections::HashMap<u32, bool>, p: &core_math::primitives::SdfPrimitive) -> u32 {
    let has_map = match mats.get(&p.material_id.0) {
        Some(&has) if !p.material_id.is_none() => has,
        _ => p.material.albedo_map.is_set(),
    };
    (p.texture_id != 0) as u32 + has_map as u32
}

/// Textures bound by a NODE's OWN material(s).
///
/// SdfBody is deliberately absent: its `material` is documented as auto-synced from its first
/// prim, so counting it would re-report that prim's texture one row up rather than describe the
/// body. A body row's Tex cell is honestly blank.
pub(crate) fn ol_node_tex(payload: &NodePayload) -> u32 {
    match payload {
        NodePayload::PolygonalBody { material, .. }
        | NodePayload::NurbsBody { material, .. }
        | NodePayload::NurbsSolid { material, .. } => material.albedo_map.is_set() as u32,
        NodePayload::ImportedMesh { materials, .. } =>
            materials.iter().filter(|m| m.albedo_map.is_set()).count() as u32,
        _ => 0,
    }
}

/// Which context menu a NODE row gets, from its payload. `is_root_body` = this is the scene's
/// root SdfBody, which cannot be dissolved.
pub(crate) fn ol_node_menu(payload: &NodePayload, is_root_body: bool) -> OlMenu {
    match payload {
        NodePayload::Root => OlMenu::Root,
        NodePayload::SdfBody { .. } => OlMenu::SdfBody { is_root_body },
        NodePayload::PolygonalBody { .. } => OlMenu::Poly,
        NodePayload::VoxelBody { .. } => OlMenu::Voxel,
        // convert_*_to_poly accept these. VoxelBody has no convert path
        // (duplicate_voxel_object snapshots to a static mesh instead).
        NodePayload::ImportedMesh { .. }
        | NodePayload::NurbsBody { .. }
        | NodePayload::NurbsSolid { .. } => OlMenu::Convertible,
        NodePayload::Group { .. } => OlMenu::Group,
        NodePayload::Light(data) => OlMenu::Light {
            id: data.id,
            light_type: data.light_type,
        },
        // Materials use a dedicated menu (in-use delete, no hierarchy nest ops).
        NodePayload::Material { id, .. } => OlMenu::Material { mid: id.0 },
        _ => OlMenu::Generic,
    }
}

/// True when this node is a graph material (drawn only via the Materials path, never as a
/// generic scene-tree cube — v1 skipped them in the walk for the same reason).
#[inline]
pub(crate) fn ol_is_material_node(payload: &NodePayload) -> bool {
    matches!(payload, NodePayload::Material { .. })
}

impl FourthLayerApp {
    pub(crate) fn ol_build(&self) -> Vec<OlRow> {
        let mut out = Vec::new();
        let filter = self.ui2.ol_filter.trim().to_lowercase();
        // Resolved ONCE for the whole rebuild — see ol_material_maps.
        let mats = ol_material_maps(&self.scene.scene_graph);
        let root = self.scene.scene_graph.root();
        // Material nodes are children of root but must NOT appear as generic cubes in the
        // scene walk (v1 skips them too). They are appended after bodies with a proper menu.
        let tops: Vec<NodeId> = self
            .scene
            .scene_graph
            .children(root)
            .iter()
            .copied()
            .filter(|&c| {
                self.scene
                    .scene_graph
                    .get(c)
                    .map(|n| !ol_is_material_node(&n.payload))
                    .unwrap_or(false)
                    && (filter.is_empty() || self.ol_matches(c, &filter))
            })
            .collect();
        let mat_nodes = self.ol_collect_materials(&filter);
        // The Scene root is a real row (`.n-root`); bodies hang off it at depth 1.
        let rn = self.scene.scene_graph.get(root);
        let expanded = !filter.is_empty() || self.ol_is_expanded(root);
        let has_children = !tops.is_empty() || !mat_nodes.is_empty();
        out.push(OlRow {
            item: OlItem::Node(root.0), depth: 0, prefix: Vec::new(), is_last: true, is_root: true,
            name: rn.map(|n| n.payload.display_name().to_string()).unwrap_or_else(|| "Scene".to_string()),
            icon: ph::STACK, color: ui_theme::RENDER_ORANGE,
            visible: rn.map_or(true, |n| n.visible), locked: rn.map_or(false, |n| n.locked),
            has_children, expanded, badge: None, mods: 0, tex: 0, uv: false,
            selected: self.sel.usel.selected.contains(&SceneItemId::Node(root)),
            rename: None, menu: OlMenu::Root, active_body: false, manifold: None,
        });
        if expanded {
            let n_tops = tops.len();
            let n_mats = mat_nodes.len();
            let mut prefix: Vec<bool> = Vec::new();
            for (i, c) in tops.iter().enumerate() {
                // When materials follow, last body is not last child of Scene.
                let is_last = n_mats == 0 && i + 1 == n_tops;
                self.ol_walk(*c, 1, &filter, &mats, &mut prefix, is_last, &mut out);
            }
            for (i, (nid, mid, name, tex_count, map_kids)) in mat_nodes.into_iter().enumerate() {
                let is_last = i + 1 == n_mats;
                self.ol_push_material_row(
                    nid,
                    mid,
                    name,
                    tex_count,
                    map_kids,
                    1,
                    &mut prefix,
                    is_last,
                    !filter.is_empty(),
                    &mut out,
                );
            }
        }
        out
    }

    /// Snapshot materials for the tree (stable MaterialId order). Empty when kind filter
    /// excludes materials (Materials kind, or empty kinds = show all).
    fn ol_collect_materials(
        &self,
        filter: &str,
    ) -> Vec<(
        NodeId,
        u32,
        String,
        u32,
        Vec<(core_math::pbr_material::MapKind, String)>,
    )> {
        let kinds = &self.ui2.ol_kinds;
        let kind_ok = kinds.is_empty() || kinds.contains(&OlKind::Materials);
        if !kind_ok {
            return Vec::new();
        }
        let mut list: Vec<_> = self
            .scene
            .scene_graph
            .materials()
            .map(|(nid, mid, name, data)| {
                let maps: Vec<_> = core_math::pbr_material::MapKind::ALL
                    .iter()
                    .filter(|k| data.map(**k).is_set())
                    .map(|&k| {
                        let tag = match k {
                            core_math::pbr_material::MapKind::Albedo => "Diffuse Color",
                            other => other.label(),
                        };
                        let label = if name.is_empty() {
                            format!("Image — {tag}")
                        } else {
                            format!("{name} — {tag}")
                        };
                        (k, label)
                    })
                    .collect();
                let tex = maps.len() as u32;
                (nid, mid.0, name.to_string(), tex, maps)
            })
            .filter(|(_, _, name, _, _)| {
                filter.is_empty() || name.to_lowercase().contains(filter)
            })
            .collect();
        list.sort_by_key(|(_, mid, _, _, _)| *mid);
        list
    }

    fn ol_push_material_row(
        &self,
        nid: NodeId,
        mid: u32,
        name: String,
        tex: u32,
        map_kids: Vec<(core_math::pbr_material::MapKind, String)>,
        depth: usize,
        prefix: &mut Vec<bool>,
        is_last: bool,
        filtering: bool,
        out: &mut Vec<OlRow>,
    ) {
        let display = if name.is_empty() {
            format!("Material {mid}")
        } else {
            name.clone()
        };
        let selected = self.sel.usel.selected.contains(&SceneItemId::Node(nid));
        // Map children: expanded via ol_collapsed dual-write OR legacy expanded_materials.
        let has_maps = !map_kids.is_empty();
        let expanded = (filtering && has_maps)
            || self.ol_is_expanded(nid)
            || self.outliner.expanded_materials.contains(&nid);
        // Material node carries SceneNode.visible — eye must reflect it (was hardcoded true,
        // so the icon never flipped and clicks were ignored — owner 2026-08-05).
        let mat_node = self.scene.scene_graph.get(nid);
        let mat_vis = mat_node.map(|n| n.visible).unwrap_or(true);
        let mat_lock = mat_node.map(|n| n.locked).unwrap_or(false);
        out.push(OlRow {
            item: OlItem::Node(nid.0),
            depth,
            prefix: prefix.clone(),
            is_last,
            is_root: false,
            name: display,
            icon: ph::PAINT_BRUSH,
            color: egui::Color32::from_rgb(200, 140, 220),
            visible: mat_vis,
            locked: mat_lock,
            has_children: has_maps,
            expanded,
            badge: None,
            mods: 0,
            tex,
            uv: false,
            selected,
            rename: Some(SceneNodeId::Node(nid)),
            menu: OlMenu::Material { mid },
            active_body: false,
            manifold: None,
        });
        if expanded && has_maps && !filtering {
            let pushed = depth >= 1;
            if pushed {
                prefix.push(!is_last);
            }
            let m = map_kids.len();
            for (j, (kind, label)) in map_kids.into_iter().enumerate() {
                let klast = j + 1 == m;
                out.push(OlRow {
                    item: OlItem::Node(nid.0),
                    depth: depth + 1,
                    prefix: prefix.clone(),
                    is_last: klast,
                    is_root: false,
                    name: label,
                    icon: ph::IMAGE,
                    color: egui::Color32::from_rgb(190, 175, 200),
                    visible: true,
                    locked: false,
                    has_children: false,
                    expanded: false,
                    badge: None,
                    mods: 0,
                    tex: 0,
                    uv: false,
                    selected: false,
                    rename: None,
                    menu: OlMenu::MatMap {
                        parent: nid,
                        kind,
                    },
                    active_body: false,
                    manifold: None,
                });
            }
            if pushed {
                prefix.pop();
            }
        }
    }

    /// True if this node's name matches, or any prim / descendant matches.
    /// Also respects the kind funnel (`ol_kinds` empty = all kinds).
    pub(crate) fn ol_matches(&self, nid: NodeId, filter: &str) -> bool {
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return false;
        };
        let kinds = &self.ui2.ol_kinds;
        let kind_ok = |k: OlKind| kinds.is_empty() || kinds.contains(&k);
        let name_ok = filter.is_empty()
            || node.payload.display_name().to_lowercase().contains(filter);

        let self_kind = match &node.payload {
            NodePayload::SdfBody { .. } | NodePayload::SdfObject { .. } => OlKind::Bodies,
            NodePayload::Light(_) => OlKind::Lights,
            NodePayload::Camera { .. } => OlKind::Cameras,
            NodePayload::Material { .. } => OlKind::Materials,
            NodePayload::PolygonalBody { .. }
            | NodePayload::ImportedMesh { .. }
            | NodePayload::NurbsBody { .. }
            | NodePayload::NurbsSolid { .. }
            | NodePayload::VoxelBody { .. } => OlKind::Meshes,
            _ => OlKind::Bodies,
        };
        // Materials never appear via ol_matches on the scene walk (filtered at tops);
        // keep kind mapping correct for any caller.
        if ol_is_material_node(&node.payload) {
            return name_ok && kind_ok(OlKind::Materials);
        }

        if name_ok && kind_ok(self_kind) {
            return true;
        }
        if let NodePayload::SdfBody { primitives, .. } = &node.payload {
            if kind_ok(OlKind::Prims)
                && primitives.iter().any(|p| {
                    filter.is_empty() || p.name.to_lowercase().contains(filter)
                })
            {
                return true;
            }
        }
        self.scene
            .scene_graph
            .children(nid)
            .iter()
            .any(|&c| self.ol_matches(c, filter))
    }

    pub(crate) fn ol_walk(&self, nid: NodeId, depth: usize, filter: &str,
               mats: &std::collections::HashMap<u32, bool>,
               prefix: &mut Vec<bool>, is_last: bool, out: &mut Vec<OlRow>) {
        let Some(node) = self.scene.scene_graph.get(nid) else { return };
        // Safety: materials must never enter the generic walk (duplicate / wrong menu).
        if ol_is_material_node(&node.payload) {
            return;
        }
        let filtering = !filter.is_empty();
        let expanded = filtering || self.ol_is_expanded(nid);
        let node_matches = node.payload.display_name().to_lowercase().contains(filter);

        // Ordered children: this body's prims first, then child nodes (matches display order).
        enum Kid { Prim { uid: u32, name: String, vis: bool, locked: bool,
                          icon: &'static str, color: egui::Color32,
                          badge: Option<(egui::Color32, &'static str)>, mods: u32, tex: u32,
                          menu: OlMenu },
                   Node(NodeId) }
        let mut kids: Vec<Kid> = Vec::new();
        let show_prims =
            self.ui2.ol_kinds.is_empty() || self.ui2.ol_kinds.contains(&OlKind::Prims);
        if let NodePayload::SdfBody { primitives, .. } = &node.payload {
            // Resolved ONCE per body: `is_base_prim` re-resolves the body's ENTIRE child_order on
            // every call (allocating as it goes), so asking it per prim would be O(prims^2) with
            // an allocation each — every frame.
            let base_slot = self.scene.scene_graph.body_base_slot(nid);
            let push_prim = |kids: &mut Vec<Kid>, li: usize| {
                let Some(p) = primitives.get(li) else { return };
                if show_prims
                    && (!filtering || node_matches || p.name.to_lowercase().contains(filter))
                {
                    kids.push(Kid::Prim { uid: p.prim_uid, name: p.name.clone(), vis: p.visible,
                        locked: p.locked, icon: prim_shape_icon(&p.shape), color: ui_theme::SDF,
                        badge: op_visual(&p.operation), mods: p.modifiers.len() as u32,
                        tex: ol_prim_tex(mats, p),
                        menu: OlMenu::Prim {
                            op: p.operation.clone(),
                            shape: p.shape.label().to_string(),
                            // `li` is the prim's index in the body's own array = the `local_idx`
                            // the base slot resolves to (NOT a flat index — reorder permutes
                            // child_order while the flat Vec stays put).
                            is_base: base_slot == Some(Ok(li)),
                            instance_master: p.instance_master_id,
                        },
                    });
                }
            };
            // UNIFIED ORDER: rows follow the body's single `child_order` (the boolean chain) —
            // the SAME resolver the bake serializer and the legacy outliner walk. Iterating the
            // prim ARRAY here was the ui2 reorder bug: `reorder_child_in_body` permutes ONLY
            // child_order (the prim Vec stays put), so a DnD reorder changed the boolean result
            // while the rows never moved.
            let resolved = self.scene.scene_graph.resolve_child_order(nid);
            let mut prim_seen = vec![false; primitives.len()];
            let mut body_seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for entry in &resolved {
                match *entry {
                    Ok(li) => {
                        if let Some(s) = prim_seen.get_mut(li) { *s = true; }
                        push_prim(&mut kids, li);
                    }
                    Err(c) => {
                        body_seen.insert(c.0);
                        if !filtering || self.ol_matches(c, filter) { kids.push(Kid::Node(c)); }
                    }
                }
            }
            // Self-heal: anything missing from child_order (stale order mid-mutation) still
            // gets a row — appended at the end, never silently dropped.
            for li in 0..primitives.len() {
                if !prim_seen[li] { push_prim(&mut kids, li); }
            }
            for &c in self.scene.scene_graph.children(nid) {
                if !body_seen.contains(&c.0) && (!filtering || self.ol_matches(c, filter)) {
                    kids.push(Kid::Node(c));
                }
            }
        } else if self.nurbs_is_solid_group(nid) {
            // Plasticity-style NURBS solid: faces are edit-mode sub-elements, not outliner rows.
            // Children stay in the scene graph for tessellation / face pick.
        } else {
            for &c in self.scene.scene_graph.children(nid) {
                if !filtering || self.ol_matches(c, filter) { kids.push(Kid::Node(c)); }
            }
        }
        let has_children = !kids.is_empty();
        let (icon, color) = ol_node_visual(&node.payload);
        let selected = self.sel.usel.selected.contains(&SceneItemId::Node(nid));
        let manifold = match &node.payload {
            NodePayload::PolygonalBody { .. } => self
                .ui2
                .ol_manifold_cache
                .get(&nid.0)
                .map(|&(_, ok)| ok),
            _ => None,
        };
        out.push(OlRow {
            item: OlItem::Node(nid.0), depth, prefix: prefix.clone(), is_last, is_root: false,
            name: node.payload.display_name().to_string(), icon, color,
            visible: node.visible, locked: node.locked, has_children, expanded, badge: None,
            mods: match &node.payload {
                NodePayload::PolygonalBody { modifiers, .. } => modifiers.len() as u32,
                _ => 0,
            },
            tex: ol_node_tex(&node.payload),
            // The runtime UV sidecar is keyed by body NodeId and lives in the app cache, not
            // in the graph (it is never serialized into the node). Absent entry = no UVs.
            uv: self.cache.poly_custom_data.get(&nid).is_some_and(|cd| !cd.uv.is_empty()),
            selected,
            rename: ol_rename_target(&node.payload, nid),
            menu: ol_node_menu(&node.payload, nid == self.scene.sdf_body_nid),
            active_body: nid == self.scene.active_body_nid
                && matches!(node.payload, NodePayload::SdfBody { .. }),
            manifold,
        });

        if expanded && has_children {
            // Only nodes at depth >= 1 contribute a drawable trunk column (top level is flat).
            let pushed = depth >= 1;
            if pushed { prefix.push(!is_last); }
            let m = kids.len();
            for (j, kid) in kids.into_iter().enumerate() {
                let klast = j + 1 == m;
                match kid {
                    Kid::Prim { uid, name, vis, locked, icon, color, badge, mods, tex, menu } => {
                        let sel = self.sel.usel.selected.contains(&SceneItemId::Prim(uid));
                        out.push(OlRow {
                            item: OlItem::Prim(uid), depth: depth + 1, prefix: prefix.clone(), is_last: klast,
                            is_root: false, name, icon, color, visible: vis, locked,
                            has_children: false, expanded: false, badge,
                            // SDF prims have no half-edge mesh, so no UV layer: SDF texturing is
                            // triplanar by default (TexMap.projection 0 = triplanar).
                            mods, tex, uv: false, selected: sel,
                            rename: Some(SceneNodeId::Prim(uid)),
                            menu, active_body: false, manifold: None,
                        });
                    }
                    Kid::Node(c) => self.ol_walk(c, depth + 1, filter, mats, prefix, klast, out),
                }
            }
            if pushed { prefix.pop(); }
        }
    }
}
