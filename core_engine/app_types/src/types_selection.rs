// SceneNodeId + OutlinerState + SelectionState + UnifiedSelection — extracted from types.rs
use eframe::egui;
use indexmap::IndexSet;
use core_math::scene_graph::NodeId;

// ==================================================================
// Scene Item ID — stable identifier for any selectable scene item
// ==================================================================
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SceneItemId {
    Prim(u32),       // prim_uid — stable across reorder/delete
    Node(NodeId),    // scene graph node (SdfBody, Poly, NURBS, Group, Empty, Camera)
    Light(u32),      // light.id — stable
}

// ==================================================================
// Unified Selection — canonical source of truth (replaces old fields)
// ==================================================================
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UnifiedSelection {
    pub selected: IndexSet<SceneItemId>,
    pub active: Option<SceneItemId>,
    pub last_clicked: Option<SceneItemId>,
}

impl UnifiedSelection {
    pub fn selected_prims(&self) -> impl Iterator<Item = u32> + '_ {
        self.selected.iter().filter_map(|id| match id {
            SceneItemId::Prim(uid) => Some(*uid),
            _ => None,
        })
    }
    pub fn selected_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.selected.iter().filter_map(|id| match id {
            SceneItemId::Node(nid) => Some(*nid),
            _ => None,
        })
    }
    pub fn selected_lights(&self) -> impl Iterator<Item = u32> + '_ {
        self.selected.iter().filter_map(|id| match id {
            SceneItemId::Light(lid) => Some(*lid),
            _ => None,
        })
    }
    pub fn active_prim_uid(&self) -> Option<u32> {
        match self.active { Some(SceneItemId::Prim(uid)) => Some(uid), _ => None }
    }
    pub fn active_node(&self) -> Option<NodeId> {
        match self.active { Some(SceneItemId::Node(nid)) => Some(nid), _ => None }
    }
    pub fn active_light(&self) -> Option<u32> {
        match self.active { Some(SceneItemId::Light(lid)) => Some(lid), _ => None }
    }
    pub fn has_prims(&self) -> bool { self.selected.iter().any(|id| matches!(id, SceneItemId::Prim(_))) }
    pub fn has_nodes(&self) -> bool { self.selected.iter().any(|id| matches!(id, SceneItemId::Node(_))) }
    pub fn has_lights(&self) -> bool { self.selected.iter().any(|id| matches!(id, SceneItemId::Light(_))) }
    pub fn is_mixed(&self) -> bool { [self.has_prims(), self.has_nodes(), self.has_lights()].iter().filter(|&&b| b).count() > 1 }
    pub fn is_empty(&self) -> bool { self.selected.is_empty() }
    pub fn len(&self) -> usize { self.selected.len() }
}

// ==================================================================
// Scene Node ID — stable identifier for outliner nodes
// ==================================================================
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SceneNodeId {
    Prim(u32),    // prim_uid
    Body(core_math::scene_graph::NodeId), // SdfBody node for body-level reparent
    Light(u32), // light.id (stable across deletions)
    Node(core_math::scene_graph::NodeId), // any scene graph node (Poly, NURBS, Group, Empty, etc.)
}

// ==================================================================
// Outliner State — all outliner interaction state in one place
// ==================================================================
/// Cached prim index buckets for a single SDF body.
/// Built once per dirty cycle, used for clip-rect virtualized drawing.
#[derive(Clone, Debug, Default)]
pub struct BodyPrimBuckets {
    /// Flat prim list for this body.
    pub flat: Vec<usize>,
}

/// Shape of the drop indicator a resolved drop slot wants drawn.
/// `Line` = a BETWEEN slot (horizontal insertion line at the receiving container's
/// indent, with a filled circle at its left end — Blender style). `Highlight` = a
/// drop-INTO slot (rounded rect over the whole target row). Lines mean "between",
/// highlights mean "inside" — never mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropVisualKind {
    Line,
    Highlight,
}

/// Indicator geometry + human label carried by a resolved drop slot. Built by the pure
/// slot layer (app/src/outliner_drop_slots.rs) at hit-test time; the egui layer ONLY
/// renders what this says (one painter in outliner_dnd.rs) and appends `label` to the
/// drag ghost so the user reads exactly what a release will do.
#[derive(Clone, Debug, PartialEq)]
pub struct DropVisual {
    pub kind: DropVisualKind,
    /// Left x of the RECEIVING container (line indent / highlight rect left).
    pub x_left: f32,
    /// Line y, or highlight rect top.
    pub y: f32,
    /// 0.0 for a line; row height for a highlight.
    pub height: f32,
    /// Resolved action verb: "Move after Box.003", "Copy into Body.002", "New body", …
    pub label: String,
    /// Draw the label faintly next to the indicator itself (root / new-body zone hint).
    pub show_hint: bool,
}

pub struct OutlinerState {
    // Drag-and-drop
    pub dragging: Option<usize>,
    pub drop_target: Option<usize>,
    pub drop_target_y: Option<f32>,
    /// Drop target for reparenting: drag onto a layer/object header
    pub drop_reparent: Option<SceneNodeId>,
    /// True when dropping ON a prim (center zone) to group together
    pub drop_on_prim: bool,
    /// Y of the ROOT-level insertion line: set when a prim drag hovers the empty/root area
    /// (not over any prim row, body header, or drop-into target). On release the dragged prim(s)
    /// are extracted into a brand-new top-level SdfBody (a prim cannot live outside a body).
    pub drop_root_y: Option<f32>,
    /// Insertion slot for a prim drag: (target body, index in that body's `child_order`).
    /// Set by the between-row slots that flat prim indices cannot express: before/after a
    /// sub-body header, and the end-of-body strip. Takes priority over drop_target/drop_reparent.
    pub drop_insert: Option<(core_math::scene_graph::NodeId, usize)>,
    /// Y of the ROOT-level un-nest line for a BODY drag: set when a nested body drag hovers
    /// the empty/root area. On release the dragged sub-body is reparented to the graph root
    /// (un-nested into a top-level body).
    pub body_drop_root_y: Option<f32>,
    /// Indicator + label for the CURRENT resolved drop slot (one per frame; the zones are
    /// disjoint by construction). Set alongside the semantic drop state by every hit site;
    /// rendered by the single indicator painter + appended to the drag ghost.
    pub drop_visual: Option<DropVisual>,
    /// Hover-to-expand while dragging: (collapsed body header hovered, hover start time).
    /// After ~0.6s of hovering the header the body expands (Blender leaves it open).
    pub hover_expand: Option<(core_math::scene_graph::NodeId, f64)>,
    /// Set by the header that is currently feeding `hover_expand` this frame; cleared
    /// end-of-frame — when no header saw the pointer, the timer resets.
    pub hover_expand_hit: bool,
    // Renaming (unified — only one rename active at a time)
    pub renaming: Option<SceneNodeId>,
    pub rename_buf: String,
    /// Grace period counter: skip lost_focus() for first N frames after rename starts.
    /// Prevents the text edit from immediately losing focus due to lingering click events.
    pub rename_grace_frames: u8,
    // Search / filter
    pub search_query: String,
    pub search_active: bool,
    // Pointer-based drag tracking (no ui.interact overlay)
    pub drag_start_pos: Option<egui::Pos2>,
    pub drag_source: Option<usize>,
    // Range selection (last_clicked superseded by UnifiedSelection.last_clicked)
    pub last_clicked: Option<SceneNodeId>,
    // Visual draw order for Shift+range select — the last COMPLETE frame's order.
    // Input handlers (which fire MID-draw) must read this, never the partial in-progress
    // list: rows below the clicked row would not be registered yet, making the range
    // silently fail (the "shift-range no-op" bug). Double-buffered via `visual_order_next`.
    pub visual_order: Vec<SceneItemId>,
    // Build buffer: rows push here DURING the outliner draw; swapped into `visual_order`
    // once the draw completes. Never read for input.
    pub visual_order_next: Vec<SceneItemId>,
    // Body header drag-and-drop (reorder bodies among siblings)
    pub dragging_body: Option<core_math::scene_graph::NodeId>,
    pub body_drag_start_pos: Option<egui::Pos2>,
    pub body_drag_source: Option<core_math::scene_graph::NodeId>,
    pub body_drop_target_nid: Option<core_math::scene_graph::NodeId>,
    pub body_drop_target_y: Option<f32>,
    pub body_drop_before: bool,
    /// True when the body-drag hover is in the CENTER zone of the target body header → drop NESTS the
    /// dragged body as a sub-body of the target (reparent), instead of reordering among siblings.
    pub body_drop_nest: bool,
    // Body header Y positions for drop-position insertion
    pub body_header_bottoms: Vec<(core_math::scene_graph::NodeId, f32)>,
    // SdfBody collapse state in unified outliner (per-body)
    pub sdf_body_collapsed: bool,
    /// USER-owned expansion state — UNIFORM semantics for every body: presence in the
    /// set = expanded, absence = collapsed. Never interpreted differently for the active
    /// body (the old inverted active/non-active semantics made every previously expanded
    /// body flip to "collapsed" the moment another body was activated — the auto-collapse
    /// bug). Selection/activation may INSERT (auto-expand) but must never remove entries.
    pub expanded_bodies: std::collections::HashSet<core_math::scene_graph::NodeId>,
    /// Last active body observed by the outliner draw — used to auto-expand the newly
    /// activated body exactly ONCE on change (so the user can still collapse it manually).
    pub last_active_body: Option<core_math::scene_graph::NodeId>,
    /// USER-owned expansion state for MATERIAL rows (Modo-shader-tree-style children showing the
    /// loaded map channels). Same uniform semantics as `expanded_bodies`: present = expanded.
    pub expanded_materials: std::collections::HashSet<core_math::scene_graph::NodeId>,
    /// Set when a material's map-channel CHILD row is clicked in the outliner: the material
    /// editor highlights + scrolls to that channel's slot row. `(material node, channel,
    /// egui time at click)` — the highlight fades out after a couple of seconds (editor-side).
    pub focus_map_channel: Option<(core_math::scene_graph::NodeId, core_math::pbr_material::MapKind, f64)>,
    // Scroll-to target for "Find in Outliner" (F key)
    pub scroll_to_node: Option<SceneNodeId>,
    /// Dirty flag — when true, visual_order must be rebuilt during the next outliner draw.
    /// Set on any structural scene change (add/remove/reorder/rename/undo/load/selection).
    pub outliner_dirty: bool,
    /// Cached root children list — rebuilt when outliner_dirty. Avoids Vec clone per frame.
    pub cached_root_children: Vec<core_math::scene_graph::NodeId>,
    /// Cached prim-index buckets per SDF body — rebuilt when outliner_dirty.
    /// Key = body NodeId. Enables clip-rect virtualization of prim rows.
    pub cached_body_buckets: std::collections::HashMap<core_math::scene_graph::NodeId, BodyPrimBuckets>,
    /// Last time outliner data was fully refreshed (for interaction throttle).
    pub outliner_last_refresh: std::time::Instant,
}

impl Default for OutlinerState {
    fn default() -> Self {
        Self {
            dragging: None,
            drop_target: None,
            drop_target_y: None,
            drop_reparent: None,
            drop_on_prim: false,
            drop_root_y: None,
            drop_insert: None,
            body_drop_root_y: None,
            drop_visual: None,
            hover_expand: None,
            hover_expand_hit: false,
            drag_start_pos: None,
            drag_source: None,
            renaming: None,
            rename_buf: String::new(),
            rename_grace_frames: 0,
            search_query: String::new(),
            search_active: false,
            last_clicked: None,
            visual_order: Vec::new(),
            visual_order_next: Vec::new(),
            dragging_body: None,
            body_drag_start_pos: None,
            body_drag_source: None,
            body_drop_target_nid: None,
            body_drop_target_y: None,
            body_drop_before: false,
            body_drop_nest: false,
            body_header_bottoms: Vec::new(),
            sdf_body_collapsed: false,
            expanded_bodies: std::collections::HashSet::new(),
            last_active_body: None,
            expanded_materials: std::collections::HashSet::new(),
            focus_map_channel: None,
            scroll_to_node: None,
            outliner_dirty: true,
            cached_root_children: Vec::new(),
            cached_body_buckets: std::collections::HashMap::new(),
            outliner_last_refresh: std::time::Instant::now(),
        }
    }
}

// ==================================================================
// Selection State
// ==================================================================
pub struct SelectionState {
    pub edit_target: Option<usize>,
    pub selected_faces: Vec<glam::Vec3>,
    /// Unified selection — canonical source of truth (moved from FourthLayerApp — Phase 1).
    pub usel: UnifiedSelection,
    /// Occlusion-aware ("visible only") selection. When true, click-picking of poly
    /// sub-elements (vertex/edge) rejects candidates occluded by the mesh's own front
    /// surface — i.e. the pick ray only "spots" the front-most visible geometry, matching
    /// standard DCC behavior (Blender/Maya "visible only"). When false, the ray selects
    /// through all geometry (X-ray style). Runtime-only UI preference — NOT serialized to
    /// the project file (lives here, not on the serialized UnifiedSelection).
    /// Default ON = the DCC-expected behavior.
    pub visible_only: bool,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self {
            edit_target: None,
            selected_faces: vec![],
            usel: UnifiedSelection::default(),
            visible_only: true,
        }
    }
}
