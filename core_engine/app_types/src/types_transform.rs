// Transform, Undo, Toast, and related types — extracted from types.rs
use eframe::egui;
use glam::Vec3;
use serde::{Serialize, Deserialize};
use core_math::primitives::SdfPrimitive;
use std::collections::VecDeque;
use std::sync::Arc;
use crate::*;

// ==================================================================
// Undo, Toast, Modal Transform, Selection, WorkMode
// ==================================================================
#[derive(Clone)]
pub struct UndoEntry {
    pub description: String,
    pub payload: UndoPayload,
    pub active_body_nid: core_math::scene_graph::NodeId,
    pub usel: UnifiedSelection,
    /// Poly sub-element selection at snapshot time. Restored on Full/Poly undo so a
    /// G/R/S transform Ctrl+Z reverts geometry WITHOUT dropping the face/edge/vert selection
    /// (Blender parity). Topology-changing ops may leave stale ids — apply path sanitizes.
    pub poly_verts: std::collections::HashSet<u32>,
    pub poly_edges: std::collections::HashSet<u32>,
    pub poly_faces: std::collections::HashSet<u32>,
    pub poly_active_vertex: Option<u32>,
    /// Material panel docs (`mid → JSON`). Layer Noise Scale / Adjust / etc. live here, not
    /// only on `PbrMaterial`. `None` = geometry-only entry (do not touch panel docs on restore).
    pub material_panel_docs_json: Option<std::sync::Arc<std::collections::HashMap<u32, String>>>,
    /// Material node graphs (`mid → JSON`). Noise params / Mult stacks compile into these.
    pub material_node_graphs_json: Option<std::sync::Arc<std::collections::HashMap<u32, String>>>,
    /// The poly `CustomData` sidecar (`nid → columns`) at snapshot time.
    ///
    /// The sidecar is RUNTIME app state (`cache.poly_custom_data`), NOT part of the scene graph,
    /// so a `Full` scene-graph snapshot does NOT cover it. Everything authored there — Mark Sharp,
    /// the Auto Smooth angle, UVs, seams, creases, polygroups — was therefore invisible to Ctrl+Z.
    /// Captured here so undo restores it with the geometry, in the SAME entry (no parallel undo
    /// path). `None` = an entry that does not own sidecar state (selection-only) — do not touch it
    /// on restore. `Arc` so consecutive entries share the allocation, like the prim snapshot.
    pub poly_custom_data: Option<
        std::sync::Arc<
            std::collections::HashMap<
                core_math::scene_graph::NodeId,
                core_math::poly_mesh::CustomData,
            >,
        >,
    >,
}

impl UndoEntry {
    /// Geometry / selection entries that do not own material-panel state.
    pub fn with_no_material_ui(mut self) -> Self {
        self.material_panel_docs_json = None;
        self.material_node_graphs_json = None;
        self
    }
}

/// A snapshot of BOTH selection systems for selection-only undo. `UnifiedSelection` (objects/nodes/prims)
/// lives on every `UndoEntry` already; the poly sub-element sets (`selected_*`) were previously NOT
/// snapshotted anywhere, so this carries them too. Cheap to clone (a handful of `HashSet<u32>`).
#[derive(Clone, Default)]
pub struct SelectionSnapshot {
    pub usel: UnifiedSelection,
    pub poly_verts: std::collections::HashSet<u32>,
    pub poly_edges: std::collections::HashSet<u32>,
    pub poly_faces: std::collections::HashSet<u32>,
    /// The active poly vertex ("merge at last" target) — part of selection state.
    pub poly_active_vertex: Option<u32>,
}

impl SelectionSnapshot {
    /// True when two snapshots represent the SAME selection (used to coalesce / skip no-op recordings).
    /// `IndexSet` equality is order-independent, which is what we want (selection identity, not order).
    pub fn same_as(&self, other: &SelectionSnapshot) -> bool {
        self.usel.selected == other.usel.selected
            && self.usel.active == other.usel.active
            && self.poly_verts == other.poly_verts
            && self.poly_edges == other.poly_edges
            && self.poly_faces == other.poly_faces
            && self.poly_active_vertex == other.poly_active_vertex
    }
}

/// Full snapshot vs. delta undo payload.
/// Lights are stored in the scene graph snapshot — no separate light field needed.
#[derive(Clone)]
pub enum UndoPayload {
    /// Full snapshot — Arc-shared primitives (consecutive non-prim ops share the same allocation).
    Full {
        primitives: Arc<Vec<SdfPrimitive>>,
        scene_graph_snapshot: Option<core_math::scene_graph::SceneGraph>,
    },
    /// Delta — stores only changed primitive uids + their old values.
    /// Lighter than Full for single-prim transforms/material edits.
    Delta {
        changes: Vec<(u32, SdfPrimitive)>,  // (prim_uid, old_snapshot)
    },
    /// Poly Euler-op delta (POLY_FOUNDATION_PLAN §4.6, Phase 2). One user action = one entry holding
    /// the ORDERED batch of (parametric op, inverse `OpDelta`) applied to the edited body. A single
    /// tool op is a 1-element batch; a multi-select tool (delete N faces, dissolve N edges) is an
    /// N-element batch — still ONE Ctrl+Z. Undo applies the inverses in REVERSE order; redo re-runs
    /// the ops in FORWARD order (parametric redo). Each op carries O(touched) `OpDelta::Element` for
    /// the local ops (split/extrude/loop_cut/flip), honest O(body) `OpDelta::Snapshot` otherwise —
    /// NOT a whole scene-graph clone. Delta-undo win: a local poly edit undoes without copying the
    /// whole body.
    Poly {
        nid: core_math::scene_graph::NodeId,
        ops: Vec<(core_math::mesh_ops::PolyEdit, core_math::mesh_ops::OpDelta)>,
    },
    /// Selection-only undo (Blender parity: Ctrl+Z reverts selection before geometry). Holds the
    /// PRE-change selection of BOTH systems. Apply is O(selection size): swap the snapshot in, mark
    /// only `RENDER` dirty — NO scene-graph restore, NO rebake, NO cache invalidation (selection is
    /// view-only state per the DirtyFlags rule). Kept on the SAME single undo stack (no parallel path).
    Selection {
        snap: SelectionSnapshot,
    },
    /// Sparse PolySculpt stroke (ZB_RESPONSIVE_SCULPT_PLAN §7.5 — completes S0c). Orig
    /// positions of verts FIRST-TOUCHED this episode. `slot == None` ⇒ direct (authored
    /// mesh); `Some(i)` ⇒ Sculpt modifier slot `i`. Apply is a position SWAP (redo holds
    /// the post-stroke set captured at apply); the layer arm rewrites `sculpt_deltas`
    /// FROM THE PREFIX — never raw positions into the delta channel.
    PolySculpt {
        nid: core_math::scene_graph::NodeId,
        slot: Option<usize>,
        orig: Vec<(u32, [f32; 3])>,
    },
}
impl UndoPayload {
    /// Borrow the scene graph snapshot without consuming the payload (extrude push-through
    /// reads the PRE-drag mesh while the episode stays pending).
    pub fn scene_graph_snapshot_ref(&self) -> Option<&core_math::scene_graph::SceneGraph> {
        match self {
            UndoPayload::Full { scene_graph_snapshot, .. } => scene_graph_snapshot.as_ref(),
            _ => None,
        }
    }

    /// Extract the scene graph snapshot, if this is a Full payload that has one.
    pub fn scene_graph_snapshot(self) -> Option<core_math::scene_graph::SceneGraph> {
        match self {
            UndoPayload::Full { scene_graph_snapshot, .. } => scene_graph_snapshot,
            UndoPayload::Delta { .. } => None,
            UndoPayload::Poly { .. } => None,
            UndoPayload::Selection { .. } => None,
            UndoPayload::PolySculpt { .. } => None,
        }
    }
}

pub struct UndoHistory {
    pub undo_stack: VecDeque<UndoEntry>,
    pub redo_stack: VecDeque<UndoEntry>,
    pub max_entries: usize,
    /// Cached Arc for prim snapshots — reused when prims haven't changed.
    cached_prims: Option<Arc<Vec<SdfPrimitive>>>,
    /// Set to true when any operation modifies prims, so next save_undo creates a fresh Arc.
    prims_dirty: bool,
    /// The selection state as of the LAST recorded selection undo entry (or the last geometry
    /// undo/redo that restored a selection). Coalescing key: a selection change is only recorded
    /// if it differs from this, so a click that re-selects the same element records nothing.
    pub last_recorded_selection: Option<SelectionSnapshot>,
}
impl UndoHistory {
    pub fn new(m: usize) -> Self {
        Self { undo_stack: VecDeque::new(), redo_stack: VecDeque::new(), max_entries: m,
               cached_prims: None, prims_dirty: true, last_recorded_selection: None }
    }
    pub fn push(&mut self, e: UndoEntry) {
        self.undo_stack.push_back(e);
        if self.undo_stack.len() > self.max_entries { self.undo_stack.pop_front(); }
        self.redo_stack.clear();
    }
    pub fn can_undo(&self) -> bool { !self.undo_stack.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo_stack.is_empty() }
    /// Snapshot current prims, reusing the cached Arc if prims are clean.
    pub fn snapshot_prims(&mut self, current: &[SdfPrimitive]) -> Arc<Vec<SdfPrimitive>> {
        if !self.prims_dirty {
            if let Some(ref arc) = self.cached_prims {
                return Arc::clone(arc);
            }
        }
        let arc = Arc::new(current.to_vec());
        self.cached_prims = Some(Arc::clone(&arc));
        self.prims_dirty = false;
        arc
    }
    /// Mark prims as changed — next snapshot will clone fresh.
    pub fn mark_prims_dirty(&mut self) { self.prims_dirty = true; }
}
pub struct Toast { pub message: String, pub remaining: f32 }

#[derive(PartialEq,Clone,Copy,Debug,Serialize,Deserialize)] pub enum ShadingMode { Pbr, MatCap, Lit, Wireframe, SolidHdr, Raytrace, PathTrace }
/// Viewport rendering backend.
#[derive(PartialEq,Clone,Copy,Debug)] pub enum ViewportRenderMode { FullSdf, Hybrid }
#[derive(PartialEq,Clone,Copy)] pub enum TransformKind { Grab, Rotate, Scale, Elongate }
#[derive(PartialEq,Clone,Copy)] pub enum AxisConstraint { None, X, Y, Z, PlaneX, PlaneY, PlaneZ }
pub struct TransformState {
    pub kind: TransformKind, pub axis: AxisConstraint,
    pub original_pos: Vec3, pub original_rot: Vec3, pub original_scale: Vec3,
    pub start_mouse: egui::Pos2, pub num_buf: String,
    pub node_target: Option<core_math::scene_graph::NodeId>,
    pub confirm_on_release: bool, pub free_mode: bool,
    pub multi_targets: Vec<TransformSnapshot>,
    pub light_targets: Vec<LightTransformSnapshot>,
    /// Frozen at modal start (G/R/S). Live `tool.transform_space` must NOT be re-read mid-drag.
    pub space: TransformSpace,
    /// Frozen world orientation at modal start (body and/or prim). Used for Local axes + rotate
    /// composition. Must NOT call active_rotation_quat() each frame (that breaks Local after R).
    pub object_orient: glam::Quat,
    /// Shift+Drag duplicate: the transform operates on duplicated prims.
    pub shift_duplicate: bool,
    /// Number of array copies (1 = just the duplicate, 2+ = evenly-spaced intermediates).
    pub array_count: u32,
    /// UIDs of intermediate array prims (NOT the main dragged duplicate).
    pub array_prim_uids: Vec<u32>,
    /// Body-local position of the ORIGINAL prim (before duplication), for array interpolation.
    pub dup_origin_local: Vec3,
    /// Original (pre-duplicate) prim uid — source master for M→instances / array-as-modifier.
    pub array_source_uid: Option<u32>,
    /// Accumulated scroll delta for array count (one mouse-wheel notch ≈ one step).
    pub array_scroll_accum: f32,
    /// Circle/radial array (C key): copies on a circle around `dup_origin_local`.
    /// false = linear array along origin→primary.
    pub array_circle: bool,
}

/// Snapshot of a light's transform state for multi-light modal transforms.
#[derive(Clone)]
pub struct LightTransformSnapshot {
    pub light_id: u32,
    pub original_pos: Vec3,
    pub original_azimuth: f32,
    pub original_elevation: f32,
    /// Attenuation radius at drag start — S (Scale) on a light scales this
    /// (owner 2026-08-07: "S should modify the radius of the light").
    pub original_range: f32,
}

/// Composable falloff for tool operations (replaces hardcoded proportional editing).
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum FalloffKind {
    /// No falloff — only selected vertices move.
    Uniform,
    /// Proportional editing: unselected verts within radius move with falloff weight.
    Proportional { falloff: PropFalloff, radius: f32 },
}

impl FalloffKind {
    /// Compute weight for a vertex at `pos` relative to transform `center`.
    /// Returns 0.0 for vertices outside the radius (or for Uniform mode).
    pub fn weight(&self, pos: Vec3, center: Vec3) -> f32 {
        match self {
            FalloffKind::Uniform => 0.0,
            FalloffKind::Proportional { falloff, radius } => {
                let dist = (pos - center).length();
                if dist >= *radius { return 0.0; }
                falloff.weight(dist / radius)
            }
        }
    }
}

/// State for transforming poly sub-element vertices (G/R/S in edit mode).
pub struct PolyTransformState {
    pub kind: TransformKind,
    pub axis: AxisConstraint,
    /// Original positions keyed by vertex index.
    pub original_positions: Vec<(u32, Vec3)>,
    /// Centroid of affected vertices (pivot for rotate/scale).
    pub center: Vec3,
    /// WORLD point that proportional influence is measured FROM — deliberately SEPARATE
    /// from `center`. These used to be one field, which meant the (new) user-placed
    /// proportional origin would have silently dragged the rotate/scale pivot with it:
    /// moving the falloff ring would have changed what R and S orbit around. Defaults to
    /// `center` (Action Centre) and only diverges when `AzraApp::poly_prop_origin` is set.
    pub falloff_center: Vec3,
    pub start_mouse: egui::Pos2,
    pub num_buf: String,
    /// Falloff mode: Uniform (no proportional) or Proportional { falloff, radius }.
    pub falloff: FalloffKind,
    /// Snapshot of all unselected vertex original positions (for proportional editing).
    pub all_vert_originals: Vec<(u32, Vec3)>,
    pub confirm_on_release: bool,
    /// Mirror correspondence: (source_vert, mirror_vert). Built when symmetry is enabled.
    pub mirror_pairs: Vec<(u32, u32)>,
    /// Original positions of mirror vertices (for correct delta application).
    pub mirror_originals: Vec<(u32, Vec3)>,
    /// Free mode: no gizmo axis constraint, workplane-locked transform.
    pub free_mode: bool,
    /// Frozen Action Center orientation at drag start (Local axes under Selection/Element/…).
    pub frozen_orient: glam::Quat,
    /// Frozen transform space at drag start (Global vs Local under Action Center).
    pub frozen_space: crate::TransformSpace,
}

/// Proportional editing falloff types.
#[derive(PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum PropFalloff {
    Smooth,
    Sphere,
    Root,
    InverseSquare,
    Sharp,
    Linear,
    Constant,
    Random,
}
impl PropFalloff {
    pub fn label(&self) -> &'static str {
        match self { Self::Smooth=>"Smooth", Self::Sphere=>"Sphere", Self::Root=>"Root",
            Self::InverseSquare=>"Inv Square", Self::Sharp=>"Sharp", Self::Linear=>"Linear",
            Self::Constant=>"Constant", Self::Random=>"Random" }
    }
    pub fn next(&self) -> Self {
        match self { Self::Smooth=>Self::Sphere, Self::Sphere=>Self::Root, Self::Root=>Self::InverseSquare,
            Self::InverseSquare=>Self::Sharp, Self::Sharp=>Self::Linear, Self::Linear=>Self::Constant,
            Self::Constant=>Self::Random, Self::Random=>Self::Smooth }
    }
    /// Map normalized distance (0..1) to weight (0..1). d=0 → full effect, d=1 → no effect.
    pub fn weight(&self, d: f32) -> f32 {
        let d = d.clamp(0.0, 1.0);
        match self {
            Self::Smooth    => { let t = 1.0 - d; t * t * (3.0 - 2.0 * t) } // hermite
            Self::Sphere    => (1.0 - d * d).max(0.0).sqrt(),
            Self::Root      => (1.0 - d).max(0.0).sqrt(),
            Self::InverseSquare => { let t = 1.0 - d; t * t }
            Self::Sharp     => { let t = 1.0 - d; t * t * t }
            Self::Linear    => (1.0 - d).max(0.0),
            Self::Constant  => if d < 1.0 { 1.0 } else { 0.0 },
            Self::Random    => if d < 1e-6 { 1.0 } else if d < 1.0 { (1.0 - d) * (0.3 + 0.7 * ((d * 12345.6789).sin() * 0.5 + 0.5)) } else { 0.0 },
        }
    }
}

#[derive(PartialEq,Clone,Copy,Debug,Serialize,Deserialize)] pub enum SnapMode { Grid, Increment, Surface, Center }
pub struct SnapIndicator { pub world_pos: Vec3, pub label: &'static str, pub fade: u8 }

#[derive(Clone, Serialize, Deserialize)]
pub struct PrimLayer {
    pub id: u32,
    pub name: String,
    pub collapsed: bool,
    pub visible: bool,
    pub position: Vec3,
    pub modifier: core_math::primitives::Modifier,
}
impl PrimLayer {
    pub fn new(id: u32, name: String) -> Self {
        Self { id, name, collapsed: false, visible: true, position: Vec3::ZERO, modifier: core_math::primitives::Modifier::default() }
    }
}

/// A compound object grouping multiple primitives. First primitive = "Base" (no boolean op).
#[derive(Clone, Serialize, Deserialize)]
pub struct SceneObject {
    pub id: u32,
    pub name: String,
    pub collapsed: bool,
    pub locked: bool,
    pub layer_id: Option<u32>,
}
impl SceneObject {
    pub fn new(id: u32, name: String) -> Self {
        Self { id, name, collapsed: false, locked: false, layer_id: None }
    }
}

/// Action to perform after the "Save Changes?" dialog resolves.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PendingAction { NewScene, OpenFile, Quit }
