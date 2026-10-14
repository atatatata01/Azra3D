//! Universal clipboard — copy/cut/paste any scene object.
//!
//! Captures scene graph subtrees (nodes + children) as serialized snapshots.
//! Handles: SDF prims, SDF bodies, lights, cameras, poly/NURBS bodies,
//! groups, materials, empties — and any future NodePayload variant.
//!
//! Decoupled from paste position: callers specify WHERE via `PastePosition`,
//! enabling tools/macros to build custom paste behaviors (stamp tool,
//! paste-at-cursor, paste-on-surface, etc.).

use glam::Vec3;
use core_math::scene_graph::{NodeId, NodePayload, PolyModifier, SceneNode};
use core_math::primitives::SdfPrimitive;
use crate::*;

// ============================================================
// Clipboard item — a snapshot of something copyable
// ============================================================

/// A single copied prim (within a body context).
#[derive(Clone, Debug)]
pub(crate) struct ClipboardPrim {
    pub prim: SdfPrimitive,
    /// Body NodeId the prim was copied FROM (for same-body paste detection).
    pub source_body_nid: NodeId,
}

/// A copied poly modifier, plus the body it came from.
///
/// The source body is kept so paste can rebase local-space authored state
/// (`pipe_path`, `circ_center`, `array_offset`) into the TARGET body's local space —
/// see `PolyModifier::rebased_to`, which owns that policy.
#[derive(Clone, Debug)]
pub(crate) struct ClipboardPolyModifier {
    pub modifier: PolyModifier,
    pub source_body_nid: NodeId,
}

/// A snapshot of a scene graph subtree (node + all descendants).
/// Stored as a flat list of (parent_index, node_snapshot) pairs where
/// parent_index is an index into the same list (-1 for root of copied subtree).
#[derive(Clone, Debug)]
pub(crate) struct SubtreeSnapshot {
    pub nodes: Vec<(i32, NodeSnapshot)>,
}

/// Snapshot of a single node (payload + local transform).
/// Stripped of runtime IDs (NodeId, MeshCacheId) — reassigned on paste.
#[derive(Clone, Debug)]
pub(crate) struct NodeSnapshot {
    pub payload: NodePayload,
    pub local_position: Vec3,
    pub local_rotation: Vec3,
    pub local_scale: Vec3,
    pub visible: bool,
}

// ============================================================
// Clipboard — the container
// ============================================================

/// What's on the clipboard.
#[derive(Clone, Debug)]
pub(crate) enum ClipboardContent {
    /// One or more prims (from the same or different bodies).
    Prims(Vec<ClipboardPrim>),
    /// One or more scene graph subtrees (bodies, groups, lights, etc.).
    Nodes(Vec<SubtreeSnapshot>),
    /// A single poly modifier lifted out of a body's stack.
    PolyMod(ClipboardPolyModifier),
    /// Empty clipboard.
    Empty,
}

/// Universal clipboard state.
#[derive(Clone, Debug)]
pub(crate) struct Clipboard {
    pub content: ClipboardContent,
    /// Frame number when the copy was made (for staleness detection).
    pub copy_frame: u64,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self { content: ClipboardContent::Empty, copy_frame: 0 }
    }
}

impl Clipboard {
    pub fn is_empty(&self) -> bool {
        matches!(&self.content, ClipboardContent::Empty)
    }

    pub fn description(&self) -> String {
        match &self.content {
            ClipboardContent::Empty => "empty".into(),
            ClipboardContent::Prims(prims) => format!("{} prim(s)", prims.len()),
            ClipboardContent::Nodes(trees) => {
                let total: usize = trees.iter().map(|t| t.nodes.len()).sum();
                format!("{} node(s)", total)
            }
            ClipboardContent::PolyMod(m) => format!("modifier \"{}\"", m.modifier.label()),
        }
    }
}

// ============================================================
// Paste position — decoupled from clipboard content
// ============================================================

/// Where to place pasted items. Callers construct this; the paste
/// system interprets it. Designed for tools/macros to extend.
#[derive(Clone, Debug)]
pub(crate) enum PastePosition {
    /// Same position as the original (default Ctrl+V).
    Original,
    /// Offset from original by a fixed delta.
    Offset(Vec3),
    /// At a specific world-space point.
    WorldPoint(Vec3),
}

// ============================================================
// Paste result — what was created
// ============================================================

/// Returned by paste operations so tools/macros know what was created.
#[derive(Clone, Debug, Default)]
pub(crate) struct PasteResult {
    /// NodeIds of top-level nodes that were created.
    pub created_nodes: Vec<NodeId>,
    /// PrimUids of prims that were created (within bodies).
    pub created_prim_uids: Vec<u32>,
}

impl PasteResult {
    pub fn is_empty(&self) -> bool {
        self.created_nodes.is_empty() && self.created_prim_uids.is_empty()
    }
}

// ============================================================
// Copy implementation
// ============================================================

impl FourthLayerApp {
    /// Snapshot a single scene node (without children — those are handled by walk).
    fn snapshot_node(&self, nid: NodeId) -> Option<NodeSnapshot> {
        let node = self.scene.scene_graph.get(nid)?;
        let mut payload = node.payload.clone();
        // B.4: keep MeshCacheId so paste can share GPU mesh (same as Shift+D).
        // VoxelBody still clears on paste (independent grid) — see paste_subtree.
        match &mut payload {
            NodePayload::VoxelBody { mesh_cache_id, .. } => *mesh_cache_id = None,
            NodePayload::SdfBody { mesh_cache_id, .. } => *mesh_cache_id = None,
            NodePayload::PolygonalBody { mesh_cache_id, .. } => *mesh_cache_id = None,
            NodePayload::NurbsBody { mesh_cache_id, .. }
            | NodePayload::NurbsSolid { mesh_cache_id, .. } => *mesh_cache_id = None,
            _ => {}
        }
        Some(NodeSnapshot {
            payload,
            local_position: node.local_position,
            local_rotation: node.local_rotation,
            local_scale: node.local_scale,
            visible: node.visible,
        })
    }

    /// Snapshot a subtree rooted at `nid` (node + all descendants).
    fn snapshot_subtree(&self, nid: NodeId) -> Option<SubtreeSnapshot> {
        let mut nodes = Vec::new();
        self.walk_snapshot(nid, -1, &mut nodes);
        if nodes.is_empty() { return None; }
        Some(SubtreeSnapshot { nodes })
    }

    fn walk_snapshot(&self, nid: NodeId, parent_idx: i32, out: &mut Vec<(i32, NodeSnapshot)>) {
        let snap = match self.snapshot_node(nid) {
            Some(s) => s,
            None => return,
        };
        let my_idx = out.len() as i32;
        out.push((parent_idx, snap));
        let children: Vec<NodeId> = self.scene.scene_graph.children(nid).to_vec();
        for child in children {
            self.walk_snapshot(child, my_idx, out);
        }
    }

    /// Copy the current selection to the clipboard.
    /// Handles prims, nodes, or mixed selection.
    pub(crate) fn copy_selection(&mut self) {
        // ── Poly modifier takes precedence over the object copy ──
        // Rationale (owner request: "select a modifier, Ctrl+C, paste it into another
        // object's stack"): rather than mint a second chord, Ctrl+C is routed by context,
        // exactly like Ctrl+X already routes between poly-dissolve and the object cut.
        // The gate cannot go stale silently: `draw_poly_modifier_stack` drops
        // `ui.poly_mod_selected` as soon as the properties panel shows a different body
        // or the stack shrinks, so a live modifier selection always belongs to the body
        // the user is looking at. The modifier cards also carry explicit Copy/Paste
        // buttons for users who don't want to reason about precedence.
        if self.copy_selected_poly_modifier() {
            return;
        }

        // Prims selected?
        let prim_indices = self.selected_prim_indices_unified();
        if !prim_indices.is_empty() {
            let mut items = Vec::new();
            for &idx in &prim_indices {
                if idx < self.scene.primitives.len() && idx < self.scene.prim_body_map.len() {
                    let (body_nid, _) = self.scene.prim_body_map[idx];
                    items.push(ClipboardPrim {
                        prim: self.scene.primitives[idx].clone(),
                        source_body_nid: body_nid,
                    });
                }
            }
            if !items.is_empty() {
                let count = items.len();
                self.clipboard = Clipboard {
                    content: ClipboardContent::Prims(items),
                    copy_frame: self.diag_frame,
                };
                self.toast(&format!("Copied {} prim(s)", count));
                return;
            }
        }

        // Nodes selected?
        let sel_nodes: Vec<NodeId> = self.sel.usel.selected_nodes().collect();
        if !sel_nodes.is_empty() {
            let mut trees = Vec::new();
            for &nid in &sel_nodes {
                // Skip Root — can't copy the root
                if matches!(self.scene.scene_graph.get(nid).map(|n| &n.payload), Some(NodePayload::Root)) {
                    continue;
                }
                if let Some(tree) = self.snapshot_subtree(nid) {
                    trees.push(tree);
                }
            }
            if !trees.is_empty() {
                let count: usize = trees.iter().map(|t| t.nodes.len()).sum();
                self.clipboard = Clipboard {
                    content: ClipboardContent::Nodes(trees),
                    copy_frame: self.diag_frame,
                };
                self.toast(&format!("Copied {} node(s)", count));
                return;
            }
        }

        // Lights selected?
        let sel_lights: Vec<u32> = self.sel.usel.selected_lights().collect();
        if !sel_lights.is_empty() {
            let mut trees = Vec::new();
            for &lid in &sel_lights {
                if let Some(nid) = self.light_graph_nid(lid) {
                    if let Some(tree) = self.snapshot_subtree(nid) {
                        trees.push(tree);
                    }
                }
            }
            if !trees.is_empty() {
                let count = trees.len();
                self.clipboard = Clipboard {
                    content: ClipboardContent::Nodes(trees),
                    copy_frame: self.diag_frame,
                };
                self.toast(&format!("Copied {} light(s)", count));
                return;
            }
        }

        self.toast("Nothing selected to copy");
    }

    /// Copy the modifier currently selected in the poly modifier stack, if any.
    /// Returns true when it took ownership of the copy.
    pub(crate) fn copy_selected_poly_modifier(&mut self) -> bool {
        let (nid, mi) = match self.ui.poly_mod_selected {
            Some(x) => x,
            None => return false,
        };
        let modifier = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { modifiers, .. }) => match modifiers.get(mi) {
                Some(m) => m.clone(),
                None => return false,
            },
            _ => return false,
        };
        let label = modifier.label();
        self.clipboard = Clipboard {
            content: ClipboardContent::PolyMod(ClipboardPolyModifier {
                modifier,
                source_body_nid: nid,
            }),
            copy_frame: self.diag_frame,
        };
        self.toast(&format!("Copied modifier \"{label}\""));
        true
    }

    /// Append a copied modifier to `target`'s stack, rebased into its local space.
    /// Returns false (with a toast) when the target is not a poly body.
    pub(crate) fn paste_poly_modifier_into(
        &mut self,
        clip: &ClipboardPolyModifier,
        target: NodeId,
    ) -> bool {
        if !matches!(
            self.scene.scene_graph.get(target).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Paste modifier: select a polygon mesh first");
            return false;
        }
        let src_world = self.scene.scene_graph.world_transform(clip.source_body_nid);
        let dst_world = self.scene.scene_graph.world_transform(target);
        let m = clip.modifier.rebased_to(src_world, dst_world);
        let label = m.label();
        self.transaction(&format!("Paste Modifier \"{label}\""), |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(target) {
                if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                    modifiers.push(m);
                }
            }
            s.poly_sync_mesh_cache(target);
            s.overlay_lines_fp = u64::MAX;
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast(&format!("Pasted modifier \"{label}\""));
        true
    }

    /// Poly display-pipeline trace. **Off by default**; enable with `AZRA_POLY_TRACE=1`.
    /// One line per event across mutate → sync → upload → structural rebuild → draw, so a
    /// frozen-solid repro shows exactly which stage went stale. stderr only (no file).
    pub(crate) fn poly_trace(msg: &str) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static ON: AtomicBool = AtomicBool::new(false);
        static INIT: AtomicBool = AtomicBool::new(false);
        if !INIT.swap(true, Ordering::Relaxed) {
            let en = std::env::var_os("AZRA_POLY_TRACE").map(|v| v != "0" && v != "false").unwrap_or(false);
            ON.store(en, Ordering::Relaxed);
        }
        if ON.load(Ordering::Relaxed) {
            eprintln!("[PTRACE] {msg}");
        }
    }

    /// Live Ctrl+X / poly_sync diagnostic. **Off by default** (PLAN_FOR_GROK_VIEWPORT_PERF:
    /// every-sync logging was a material share of idle poll cost). Enable with `AZRA_CTRLX=1`.
    /// When on: stderr + append-only `ctrlx_diag.log`.
    pub(crate) fn ctrx_diag(msg: &str) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static ON: AtomicBool = AtomicBool::new(false);
        static INIT: AtomicBool = AtomicBool::new(false);
        if !INIT.swap(true, Ordering::Relaxed) {
            let en = std::env::var_os("AZRA_CTRLX").map(|v| v != "0" && v != "false").unwrap_or(false);
            ON.store(en, Ordering::Relaxed);
        }
        if !ON.load(Ordering::Relaxed) {
            return;
        }
        let line = format!("[CTRLX] {msg}");
        eprintln!("{line}");
        use std::io::Write;
        let paths = [
            std::path::PathBuf::from(
                r"F:\PROJECT\FourthLAyerAi\.claude\worktrees\ui-polish\core_engine\ctrlx_diag.log",
            ),
            std::path::PathBuf::from("ctrlx_diag.log"),
        ];
        for path in &paths {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "{line}");
                break;
            }
        }
    }

    /// True when Ctrl+X must run the poly component path (never clipboard-cut the body).
    ///
    /// Edit mode **always** owns Ctrl+X (even with empty selection — quiet no-op, never body cut).
    /// Object mode with leftover face/edge/vert selection also owns it.
    pub(crate) fn poly_cut_targets_components(&self) -> bool {
        if self.poly_work_mode != PolyWorkMode::Object {
            return true;
        }
        !self.poly_sel.selected_faces.is_empty()
            || !self.poly_sel.selected_edges.is_empty()
            || !self.poly_sel.selected_vertices.is_empty()
    }

    /// Any poly mesh (or convert-to-mesh body) that Ctrl+X must never remove as an object.
    fn is_polygonal_body_nid(&self, nid: NodeId) -> bool {
        matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. } | NodePayload::ImportedMesh { .. })
        )
    }

    /// True if this chord is in poly-modeling territory (must never body-cut a mesh).
    fn poly_ctrl_x_owns_chord(&self) -> bool {
        if self.poly_work_mode != PolyWorkMode::Object {
            return true;
        }
        if self.poly_cut_targets_components() {
            return true;
        }
        if self.modeling_context == ModelingContext::Polygonal {
            return true;
        }
        if self.active_poly_nid().is_some() {
            return true;
        }
        self.sel
            .usel
            .selected_nodes()
            .any(|n| self.is_polygonal_body_nid(n))
    }

    /// Ctrl+X / Edit→Cut.
    ///
    /// **Poly (owner design):**
    ///   - Face/Edge/Vertex mode + selection → `poly_dissolve`
    ///     (vert/edge dissolve, face delete-hole)
    ///   - Face/Edge/Vertex/Group mode + **empty** selection → Mesh Cleanup
    ///     (flatten + merge + **4.5°** limited dissolve), arm left Tool Parameters,
    ///     select-all, result popup (owner 2026-08-11; dissolve default 4.5°)
    ///   - Object mode + poly selected, no components → **refuse** body cut (use **Del**)
    ///
    /// **SDF / lights / other nodes:** normal clipboard cut (copy + delete).
    /// Poly mesh nodes are **never** removed in the clipboard-cut loop.
    pub(crate) fn cut_selection(&mut self) {
        let nf = self.poly_sel.selected_faces.len();
        let ne = self.poly_sel.selected_edges.len();
        let nv = self.poly_sel.selected_vertices.len();
        let sel_nodes: Vec<NodeId> = self.sel.usel.selected_nodes().collect();
        let active = self.sel.usel.active_node();
        Self::ctrx_diag(&format!(
            "cut_selection ENTER work_mode={:?} modeling={:?} faces={nf} edges={ne} verts={nv} \
             active={active:?} nodes={sel_nodes:?} prims={} lights={}",
            self.poly_work_mode,
            self.modeling_context,
            self.sel.usel.has_prims(),
            self.sel.usel.has_lights(),
        ));

        // ── HARD GATE: any poly context never falls through to object-delete ──
        if self.poly_ctrl_x_owns_chord() {
            let has_comp = nf > 0 || ne > 0 || nv > 0;
            Self::ctrx_diag(&format!(
                "poly owns chord → has_comp={has_comp} work_mode={:?}",
                self.poly_work_mode
            ));
            if has_comp {
                self.poly_dissolve();
                Self::ctrx_diag("poly_dissolve returned");
            } else if self.poly_work_mode != crate::PolyWorkMode::Object {
                // Edit mode + empty selection: Mesh Cleanup @ 4.5° limited dissolve
                // (owner 2026-08-11 / dissolve default). Arms Tool Parameters + result popup.
                Self::ctrx_diag("empty edit selection → poly_ctrl_x_mesh_cleanup");
                self.poly_ctrl_x_mesh_cleanup();
            } else {
                self.toast(
                    "Ctrl+X: select faces / edges / verts first (Del deletes the object)",
                );
                Self::ctrx_diag("refused: object mode, no component selection");
            }
            return;
        }

        // Copy first
        self.copy_selection();
        if self.clipboard.is_empty() {
            Self::ctrx_diag("clipboard empty after copy — abort");
            return;
        }
        let desc = format!("Cut {}", self.clipboard.description());
        Self::ctrx_diag(&format!("clipboard cut path: {desc}"));
        // Then delete (reuses existing delete logic)
        self.transaction(&desc, |s| {
            // Delete prims
            let prim_indices = s.selected_prim_indices_unified();
            if !prim_indices.is_empty() {
                Self::ctrx_diag(&format!("removing {} prim(s)", prim_indices.len()));
                s.remove_prims_from_graph(&prim_indices);
                s.remove_from_selection(|id| matches!(id, SceneItemId::Prim(_)));
            }
            // Never remove a poly body while component/edit path should own Ctrl+X,
            // and never remove PolygonalBody via this chord (Del / outliner delete still can).
            if s.poly_cut_targets_components() {
                Self::ctrx_diag("ABORT body delete: poly_cut_targets_components mid-tx");
                return;
            }
            let sel_nodes: Vec<NodeId> = s.sel.usel.selected_nodes().collect();
            let mut removed = Vec::new();
            for &nid in &sel_nodes {
                let payload_kind = s
                    .scene
                    .scene_graph
                    .get(nid)
                    .map(|n| match &n.payload {
                        NodePayload::Root => "Root",
                        NodePayload::PolygonalBody { .. } => "PolygonalBody",
                        NodePayload::ImportedMesh { .. } => "ImportedMesh",
                        NodePayload::SdfBody { .. } => "SdfBody",
                        NodePayload::Group { .. } => "Group",
                        _ => "Other",
                    })
                    .unwrap_or("Missing");
                if matches!(
                    s.scene.scene_graph.get(nid).map(|n| &n.payload),
                    Some(NodePayload::Root)
                ) {
                    Self::ctrx_diag(&format!("skip Root {nid:?}"));
                    continue;
                }
                // Hard block: Ctrl+X must never remove a poly mesh body.
                if s.is_polygonal_body_nid(nid) {
                    Self::ctrx_diag(&format!("BLOCK remove {nid:?} ({payload_kind})"));
                    continue;
                }
                Self::ctrx_diag(&format!("REMOVE node {nid:?} ({payload_kind})"));
                s.cache.body_vol_cache.remove_body(nid);
                s.scene.scene_graph.remove(nid);
                removed.push(nid);
            }
            if !removed.is_empty() {
                s.sync_flat_cache();
                s.deselect_items(
                    &removed
                        .iter()
                        .map(|&nid| SceneItemId::Node(nid))
                        .collect::<Vec<_>>(),
                );
            }
            Self::ctrx_diag(&format!("cut done removed={removed:?}"));
            s.rebake_no_undo();
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::LIGHT);
            s.mark_outliner_dirty();
        });
    }
}

// ============================================================
// Paste implementation
// ============================================================

impl FourthLayerApp {
    /// Paste clipboard contents at the given position.
    /// Returns a `PasteResult` describing what was created (for tools/macros).
    pub(crate) fn paste(&mut self, position: PastePosition) -> PasteResult {
        if self.clipboard.is_empty() {
            self.toast("Clipboard is empty");
            return PasteResult::default();
        }
        let content = self.clipboard.content.clone();
        let desc = format!("Paste {}", self.clipboard.description());
        let mut result = PasteResult::default();

        match content {
            ClipboardContent::Prims(prims) => {
                self.paste_prims(prims, &position, &desc, &mut result);
            }
            ClipboardContent::Nodes(trees) => {
                self.paste_nodes(trees, &position, &desc, &mut result);
            }
            ClipboardContent::PolyMod(clip) => {
                // Target = the poly body the user is on (active node), NOT the source.
                // Pasting back onto the source body is legal (duplicates the modifier).
                if let Some(target) = self.active_poly_nid() {
                    self.paste_poly_modifier_into(&clip, target);
                } else {
                    self.toast("Paste modifier: select a polygon mesh first");
                }
            }
            ClipboardContent::Empty => {}
        }
        result
    }

    fn paste_prims(
        &mut self,
        prims: Vec<ClipboardPrim>,
        position: &PastePosition,
        desc: &str,
        result: &mut PasteResult,
    ) {
        let pos = position.clone();
        let mut created_uids = Vec::new();
        self.transaction(desc, |s| {
            for cp in &prims {
                let mut p = cp.prim.clone();
                // Reassign UID to avoid collision
                p.prim_uid = s.scene.next_prim_id;
                s.scene.next_prim_id += 1;
                // Clear stale references
                p.object_id = None;
                p.instance_master_id = None;
                // Apply paste position
                match &pos {
                    PastePosition::Original => {} // keep original position
                    PastePosition::Offset(delta) => p.position += *delta,
                    PastePosition::WorldPoint(pt) => p.position = *pt,
                }
                created_uids.push(p.prim_uid);
                s.add_prim_to_graph(p);
            }
            // Select all pasted prims
            s.clear_selection_unified();
            for &uid in &created_uids {
                s.toggle_select_unified(SceneItemId::Prim(uid));
            }
            s.rebake_no_undo();
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
        });
        let count = created_uids.len();
        result.created_prim_uids = created_uids;
        self.toast(&format!("Pasted {} prim(s)", count));
    }

    fn paste_nodes(
        &mut self,
        trees: Vec<SubtreeSnapshot>,
        position: &PastePosition,
        desc: &str,
        result: &mut PasteResult,
    ) {
        let pos = position.clone();
        let mut created_top_nodes = Vec::new();
        self.transaction(desc, |s| {
            let root = s.scene.scene_graph.root();
            for tree in &trees {
                let nids = s.instantiate_subtree(tree, root, &pos);
                created_top_nodes.extend(nids);
            }
            s.sync_flat_cache();
            // Select all pasted top-level nodes
            s.clear_selection_unified();
            for &nid in &created_top_nodes {
                s.toggle_select_unified(SceneItemId::Node(nid));
                // Mark for volume bake
                s.cache.body_vol_cache.mark_body_dirty(nid);
            }
            s.rebake_no_undo();
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::LIGHT);
            s.mark_outliner_dirty();
        });
        let count = created_top_nodes.len();
        result.created_nodes = created_top_nodes;
        self.toast(&format!("Pasted {} node(s)", count));
    }

    /// Instantiate a subtree snapshot into the scene graph under `parent`.
    /// Returns the NodeId of the top-level created node.
    fn instantiate_subtree(
        &mut self,
        tree: &SubtreeSnapshot,
        parent: NodeId,
        position: &PastePosition,
    ) -> Vec<NodeId> {
        if tree.nodes.is_empty() { return Vec::new(); }

        // Map from snapshot index → created NodeId
        let mut idx_to_nid: Vec<Option<NodeId>> = vec![None; tree.nodes.len()];
        let mut top_nodes = Vec::new();

        for (i, (parent_idx, snap)) in tree.nodes.iter().enumerate() {
            let target_parent = if *parent_idx < 0 {
                parent
            } else {
                idx_to_nid[*parent_idx as usize].unwrap_or(parent)
            };

            let mut payload = snap.payload.clone();

            // Reassign IDs to avoid collisions
            match &mut payload {
                // B.4: keep mesh_cache_id for Poly/NURBS/SDF/Import (shared GPU mesh).
                NodePayload::SdfBody { name, primitives, child_order, .. } => {
                    *name = self.next_unique_name(name);
                    for p in primitives.iter_mut() {
                        p.prim_uid = self.scene.next_prim_id;
                        self.scene.next_prim_id += 1;
                        p.instance_master_id = None;
                    }
                    // child_order references the SOURCE body's old prim uids / child nids — stale
                    // after re-id. Clear; reconciled lazily on read + by ensure after graph insert.
                    child_order.clear();
                }
                NodePayload::Light(data) => {
                    data.name = self.next_unique_name(&data.name);
                    data.id = self.scene.next_light_id;
                    self.scene.next_light_id += 1;
                }
                NodePayload::Group { name } | NodePayload::Empty { name } => {
                    *name = self.next_unique_name(name);
                }
                NodePayload::Camera { name, .. } => {
                    *name = self.next_unique_name(name);
                }
                NodePayload::PolygonalBody { name, .. } | NodePayload::NurbsBody { name, .. } => {
                    *name = self.next_unique_name(name);
                    // keep mesh_cache_id
                }
                NodePayload::NurbsSolid { name, mesh_cache_id, .. } => {
                    *name = self.next_unique_name(name);
                    *mesh_cache_id = None;
                }
                NodePayload::Material { name, .. } => {
                    *name = self.next_unique_name(name);
                    // MaterialId kept from source — if collision occurs, graph
                    // handles it at reference time. Full ID remapping is Phase 2.
                }
                NodePayload::ImportedMesh { name, .. } => {
                    *name = self.next_unique_name(name);
                    // keep mesh_cache_id (was already shared path)
                }
                NodePayload::VoxelBody { name, mesh_cache_id, .. } => {
                    *name = self.next_unique_name(name);
                    *mesh_cache_id = None; // independent density
                }
                NodePayload::SdfObject { name, .. } => {
                    *name = self.next_unique_name(name);
                    // keep mesh_cache_id if any
                }
                NodePayload::Root => continue, // never paste root
            }

            // Apply position offset to top-level nodes only
            let mut local_pos = snap.local_position;
            if *parent_idx < 0 {
                match position {
                    PastePosition::Original => {}
                    PastePosition::Offset(delta) => {
                        // For SDF bodies, offset the origin; for others, offset local_position
                        match &mut payload {
                            NodePayload::SdfBody { origin, .. } => *origin += *delta,
                            _ => local_pos += *delta,
                        }
                    }
                    PastePosition::WorldPoint(pt) => {
                        match &mut payload {
                            NodePayload::SdfBody { origin, .. } => *origin = *pt,
                            _ => local_pos = *pt,
                        }
                    }
                }
            }

            let nid = self.scene.scene_graph.add_child_with_transform(
                target_parent, payload,
                local_pos, snap.local_rotation, snap.local_scale,
            );
            // Preserve visibility
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                node.visible = snap.visible;
            }

            idx_to_nid[i] = Some(nid);
            if *parent_idx < 0 {
                top_nodes.push(nid);
            }
        }

        // child_order: all pasted nodes now exist with their final NodeIds + re-id'd prims, so
        // synthesise a canonical order for every pasted SdfBody (cleared above; reconcile fills it
        // prims-first-then-bodies — legacy order).
        for slot in idx_to_nid.iter().flatten() {
            self.scene.scene_graph.ensure_body_child_order(*slot);
        }

        // P0.5: a pasted VoxelBody had mesh_cache_id set to None — rebuild its display mesh from the
        // (deep-cloned) grid so it renders + picks; else it's invisible and orphaned.
        self.regen_voxel_display_meshes();
        top_nodes
    }
}
