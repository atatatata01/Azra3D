// Poly/NURBS modeling types — extracted from types.rs
use eframe::egui;
use glam::Vec3;
use std::collections::HashSet;

// ==================================================================
// Modeling Context — per-object context switching
// ==================================================================

/// The active modeling context, derived from the selected node's type.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ModelingContext {
    None,
    Sdf,
    Polygonal,
    Nurbs,
    Light,
    Camera,
    ImportedMesh,
}

/// Sub-element work mode for polygonal editing.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PolyWorkMode {
    Object,
    Vertex,
    Edge,
    Face,
    /// Mouse-select whole polygroups (Tab pie · GROUP). Only offered when polygroups exist.
    Group,
}

/// Sub-element work mode for NURBS editing.
#[derive(Clone, Copy, PartialEq, Debug)]
/// Sub-element work mode for NURBS (same labels as poly: Object / Face / Edge / Vertex).
pub enum NurbsWorkMode {
    Object,
    Vertex,
    Edge,
    Face,
}

/// Active tool in polygonal editing context.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PolyTool {
    Idle,
    ExtrudeFace,
    BevelEdge,
    LoopCut,
    Knife,
    MergeVertices,
    InsetFace,
    /// Even U×V grid cuts on selected faces (Ctrl+Alt+D / context menu Dice Faces).
    Dice,
}

/// Active tool in NURBS editing context.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NurbsTool {
    Idle,
    MoveCv,
    InsertKnotU,
    InsertKnotV,
    AdjustWeight,
    Loft,
    Revolve,
}

/// Sub-element selection state for polygonal mesh editing.
pub struct PolySelection {
    pub selected_vertices: HashSet<u32>,
    pub selected_edges: HashSet<u32>,
    pub selected_faces: HashSet<u32>,
    pub hovered_vertex: Option<u32>,
    pub hovered_edge: Option<u32>,
    pub hovered_face: Option<u32>,
    /// Face hover that landed on a poly body OTHER than the active one — `(NodeId.0, face_id)`.
    /// Face picking is scene-wide (a nearer body in front of the active one must win: the owner's
    /// disc-in-a-shell-hole repro), but a face id from another mesh is meaningless on the active
    /// mesh and `face(FaceId)` is unchecked, so it is parked HERE instead of in `hovered_face`.
    /// The click path (`poly_click_select`) makes that body active + `reconcile_poly_sel_body()`
    /// FIRST, then promotes this to `hovered_face`. Never index the active mesh with it.
    pub hovered_face_other: Option<(u64, u32)>,
    /// Vertex-centric hidden state: an edge/face is hidden if ANY of its vertices are hidden.
    pub hidden_vertices: HashSet<u32>,
    /// NodeId.0 of the poly body these component ids belong to. Component ids are
    /// per-mesh indices — meaningless (and CRASH-prone: face(FaceId) is unchecked) on any
    /// other mesh. A per-frame reconciler clears everything above when the active poly
    /// body changes (owner crash 2026-08-02: add-cube with stale selected face 7 from the
    /// previous 8-face box → index out of bounds on the fresh 6-face cube).
    pub body_stamp: Option<u64>,
}

impl PolySelection {
    /// Drop every component id + hover + hidden set (they index a mesh we no longer edit).
    pub fn clear_components(&mut self) {
        self.selected_vertices.clear();
        self.selected_edges.clear();
        self.selected_faces.clear();
        self.hovered_vertex = None;
        self.hovered_edge = None;
        self.hovered_face = None;
        self.hovered_face_other = None;
        self.hidden_vertices.clear();
    }
}

impl Default for PolySelection {
    fn default() -> Self {
        Self {
            selected_vertices: HashSet::new(),
            selected_edges: HashSet::new(),
            selected_faces: HashSet::new(),
            hovered_vertex: None,
            hovered_edge: None,
            hovered_face: None,
            hovered_face_other: None,
            hidden_vertices: HashSet::new(),
            body_stamp: None,
        }
    }
}

/// Screen-space pick cache for O(k) poly hover picking.
/// Caches projected screen positions + grid cells, invalidated by camera/mesh changes.
pub struct PolyPickCache {
    // Projected vertex screen positions: (vertex_id, screen_x, screen_y)
    pub vert_screen: Vec<(u32, f32, f32)>,
    // Projected edge endpoint screen positions: (half_edge_id, src_x, src_y, dst_x, dst_y)
    pub edge_screen: Vec<(u32, f32, f32, f32, f32)>,
    /// Screen-space grid: cell_index -> indices into `vert_screen` or `edge_screen`.
    ///
    /// **`u32`, and it must stay wide enough to address every entry of those Vecs.** This was
    /// `u16`, and `rebuild_pick_cache` gated registration on `if idx <= u16::MAX` — so past
    /// 65 536 projected elements the surplus sat in `*_screen` but in NO cell, and
    /// `poly_hover_pick` (which only ever walks cells) could not hover or click them. On a
    /// 40 000-quad sheet that silently cost 14 864 of 80 400 edges, and because
    /// `canonical_edges` walks half-edge index ≈ creation order, the casualties were the NEWEST
    /// geometry — the edges the user had just made (owner 2026-08-11: "edge select works well
    /// when I start using the program but after a while it starts to degrade"). Gates:
    /// `poly_selection::tests_pick_grid`. Never narrow this, and never add a capacity gate that
    /// drops elements instead of indexing them.
    pub grid: Vec<Vec<u32>>,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub rect_min_x: f32,
    pub rect_min_y: f32,
    // Cache key for invalidation
    pub cam_key: u64,
    pub mode: u8, // 0=vert, 1=edge, 2=face
    pub node_key: u64,
}

impl PolyPickCache {
    pub fn new() -> Self {
        Self {
            vert_screen: Vec::new(),
            edge_screen: Vec::new(),
            grid: Vec::new(),
            grid_cols: 0, grid_rows: 0,
            cell_w: 1.0, cell_h: 1.0,
            rect_min_x: 0.0, rect_min_y: 0.0,
            cam_key: 0, mode: 255, node_key: 0,
        }
    }

    pub fn cell_of(&self, x: f32, y: f32) -> (u32, u32) {
        let col = ((x - self.rect_min_x) / self.cell_w).floor() as i32;
        let row = ((y - self.rect_min_y) / self.cell_h).floor() as i32;
        (col.clamp(0, self.grid_cols as i32 - 1) as u32,
         row.clamp(0, self.grid_rows as i32 - 1) as u32)
    }

    pub fn cell_idx(&self, col: u32, row: u32) -> usize {
        (row * self.grid_cols + col) as usize
    }
}

/// Sub-element selection for NURBS solids (Plasticity / poly-like).
///
/// **Stable ids, never face-list indices** (PLAN R12). Fillet inserts faces and
/// rewrites the list — indices go stale and produce floating yellow edges.
/// Overlay always resolves `NurbsFaceId` → current geometry every frame.
pub struct NurbsSelection {
    /// Selected control vertices as (row, col) on the active face.
    pub selected_cvs: HashSet<(u32, u32)>,
    pub hovered_cv: Option<(u32, u32)>,
    /// Faces keyed by stable [`core_math::nurbs::NurbsFaceId`].
    pub selected_faces: HashSet<core_math::nurbs::NurbsFaceId>,
    pub hovered_face: Option<core_math::nurbs::NurbsFaceId>,
    /// Edges: (`NurbsFaceId`, edge 0..3 on that face's UV boundary).
    pub selected_edges: HashSet<(core_math::nurbs::NurbsFaceId, u8)>,
    pub hovered_edge: Option<(core_math::nurbs::NurbsFaceId, u8)>,
}

impl Default for NurbsSelection {
    fn default() -> Self {
        Self {
            selected_cvs: HashSet::new(),
            hovered_cv: None,
            selected_faces: HashSet::new(),
            hovered_face: None,
            selected_edges: HashSet::new(),
            hovered_edge: None,
        }
    }
}

impl NurbsSelection {
    pub fn clear_sub(&mut self) {
        self.selected_cvs.clear();
        self.hovered_cv = None;
        self.selected_faces.clear();
        self.hovered_face = None;
        self.selected_edges.clear();
        self.hovered_edge = None;
    }

    pub fn has_sub_selection(&self) -> bool {
        !self.selected_cvs.is_empty()
            || !self.selected_faces.is_empty()
            || !self.selected_edges.is_empty()
    }

    /// After a regenerate: drop face/edge keys whose ids no longer exist.
    /// Surviving ids keep selection without re-keying (ids are stable).
    pub fn prune_dead_ids(&mut self, live: &std::collections::HashSet<core_math::nurbs::NurbsFaceId>) {
        self.selected_faces.retain(|id| live.contains(id));
        self.selected_edges.retain(|(id, _)| live.contains(id));
        if self.hovered_face.map(|id| !live.contains(&id)).unwrap_or(false) {
            self.hovered_face = None;
        }
        if self
            .hovered_edge
            .map(|(id, _)| !live.contains(&id))
            .unwrap_or(false)
        {
            self.hovered_edge = None;
        }
    }
}

#[derive(Clone)] pub enum SelectionDrag { Box{start:egui::Pos2}, Lasso{center:egui::Pos2} }
#[derive(PartialEq,Clone,Copy,Debug)] pub enum WorkMode { Object, Sculpt, Vertex, Edge, Face }

pub struct BevelDrag {
    pub prim_idx: usize,
    pub target_edges: Vec<u8>,  // edge indices to modify (snapshot of selected at drag start)
    pub start_mouse_x: f32,
    pub start_mouse_y: f32,
    // Start values captured at drag start — angle/profile compute as start + dx*speed (not accumulate)
    pub start_chamfer_angle_bot: f32,
    pub start_chamfer_angle_top: f32,
    pub start_chamfer_angle_vert: f32,
    pub start_bevel_profile_bot: f32,
    pub start_bevel_profile_top: f32,
    pub start_bevel_profile_vert: f32,
    /// Box edge radii at drag start (12 slots). Used to detect real radius edits for undo —
    /// without this, radius-only drags were treated as no-ops and cancelled (no Ctrl+Z).
    pub start_box_radii: [f32; 12],
    /// Cylinder rim radii at drag start (top, bot).
    pub start_cyl_top: f32,
    pub start_cyl_bot: f32,
    // mode & radius derived from drag direction: ← = Fillet (Round),  → = Chamfer
    /// True when started from the yellow Uniform square: drag = all edges + auto Uniform algo;
    /// click (no drag) = reset all bevel.
    pub uniform_all: bool,
}

/// Detected boolean intersection edge curve (where two SDF surfaces meet)
pub struct BoolEdgeCurve {
    pub prim_idx: usize,              // which primitive's boolean op created this edge
    pub points_world: Vec<Vec3>,      // 3D polyline points along the edge
    pub points_screen: Vec<egui::Pos2>, // projected 2D screen positions
}

/// Drag state for adjusting boolean blend k by clicking an edge curve
pub struct BoolEdgeBevelDrag {
    pub prim_idx: usize,
    pub start_mouse_x: f32,
    pub original_k: f32,
}

// ---- BoolCut tool state ----

#[derive(Clone, Copy, PartialEq)]
pub enum BoolCutPhase { Drawing, Depth }

#[derive(Clone, Copy, PartialEq)]
pub enum BoolCutOp { Cut, Add, Intersect }

pub struct BoolCutState {
    pub phase: BoolCutPhase,
    pub op: BoolCutOp,
    pub hit_point: Vec3,
    pub hit_normal: Vec3,
    pub tangent_u: Vec3,
    pub tangent_v: Vec3,
    pub anchor: Vec3,
    pub local_u: f32,
    pub local_v: f32,
    pub depth: f32,
    pub depth_start_mouse_y: f32,
    pub corner_radius: f32,
    pub corner_chamfer: bool,
    pub blend_k: f32,
    pub prim_idx: usize,
    pub num_buf: String,
}

/// Push/Pull face drag state (Face mode: asymmetric box extrusion)
pub struct FaceDrag {
    pub prim_idx: usize,
    pub local_normal: Vec3,       // local-space face normal (e.g. Vec3::X, Vec3::NEG_Y)
    pub start_mouse: egui::Pos2,
    pub original_pos: Vec3,       // world-space center before drag
    pub original_extents: Vec3,   // half_extents before drag (stored, not visual)
    pub original_scale: Vec3,     // prim.scale at drag start
    pub accumulated_delta: f32,   // signed world-space distance
    /// CurveExtrude only: profile UV verts at drag start so X/Z shrink scales from a fixed snapshot
    /// (avoids compounding when rewriting every frame). Empty for other shapes.
    pub original_curve_uv: Vec<glam::Vec2>,
}

/// Poly extrude drag state — topology created at start, vertices slide each frame.
pub struct PolyExtrudeDrag {
    pub node_id: core_math::scene_graph::NodeId,
    pub face_normals: Vec<(u32, Vec3)>,   // (orig face id, face normal)
    pub new_face_ids: Vec<u32>,            // top face IDs from extrude
    pub start_mouse: egui::Pos2,
    pub accumulated_offset: f32,
    /// Blender "Extrude Along Normals" (Alt+E): each cap vertex moves on its own
    /// normal instead of one averaged region direction.
    pub along_normals: bool,
    /// Along-Normals topology mode (only meaningful when `along_normals`):
    /// - `false` = **Individual** (default): each face extrudes alone along its normal
    ///   (planar walls; owner TEST-021).
    /// - `true` = **Grouped**: one connected region shell; verts slide on blended normals
    ///   (owner: press **G** while the tool is active). Mid-drag **I** returns to Individual.
    pub along_normals_grouped: bool,
    /// Owner 2026-08-05 ("FORCE STRAITH FACE"): when true, Extrude Along Normals keeps each
    /// cap **planar** — face-constant normals (no blended bend) + re-project onto the face
    /// plane after each move. Default ON for Along Normals so knife-cut n-gons don't twist.
    pub force_planar: bool,
    /// Frozen at drag start (always — the mid-drag N toggle needs them even when the drag
    /// began in region mode) — `(VertexId.0, unit normal)`.
    pub vert_normals: Vec<(u32, Vec3)>,
    // Pre-tool sub-element selection, snapshotted so cancel restores it (EXCANCELSEL).
    pub orig_sel_faces: std::collections::HashSet<u32>,
    pub orig_sel_edges: std::collections::HashSet<u32>,
    pub orig_sel_vertices: std::collections::HashSet<u32>,
    /// PRE-drag mesh (Arc clone captured BEFORE the extrude topology is built) — the
    /// push-through commit's thickness raycast + boolean input. Poly undo episodes carry
    /// no scene-graph snapshot (live console 2026-08-03), so the drag state owns it.
    pub pre_mesh: Option<std::sync::Arc<core_math::half_edge::HalfEdgeMesh>>,
    /// Region face ids as they exist in `pre_mesh` (BEFORE the extrude renumbered
    /// anything). `new_face_ids` are the caps in the post-extrude mesh and cannot address
    /// the original region -- the redo panel needs these to re-run from `pre_mesh`.
    pub orig_faces: Vec<u32>,
    /// Shift held: extrude direction snapped to the dominant WORLD axis of the region
    /// normal (hold-to-lock, grab free-mode parity). Owner 2026-08-03 / fix 2026-08-06.
    pub world_axis: bool,
    /// Last Shift held state (mirrors world_axis for hold-to-lock; kept for state dump).
    pub shift_was_down: bool,
    /// Last Alt held state — rising-edge detect for the mid-drag free-grab hand-off
    /// (owner 2026-08-07: Alt = free movement, same behavior as free-mode grab).
    /// Starts `true` so the Alt still held from the Alt+E launch chord does not
    /// instantly hand the drag to Grab — a fresh Alt press is required.
    pub alt_was_down: bool,
    /// D mid-drag toggle (Punch-It style): extrude direction locked to the best-aligned
    /// MESH EDGE at the region boundary — the sweep follows existing geometry exactly, so
    /// the cut prism coincides with real walls (no epsilon slivers). None = off.
    pub edge_axis: Option<Vec3>,
    /// Face ids the extrude ALLOCATED at drag start (walls, plus the new top shell on the
    /// open-surface volume path) — `new_face_ids` are the CAPS, which on the common
    /// neighboured path are the re-pointed ORIGINAL region faces and so are not created.
    /// Bevel Smart Select's angle-based tagging runs over these at confirm, when the offset
    /// is final and the seams have a real dihedral (at drag start every wall is degenerate).
    pub created_faces: Vec<u32>,
}

/// Extrude Sweep (Shift+Alt+E): path from selected face center on WP orientation toward the
/// mouse (green point). Scroll = path segments for smoother duct turns. LMB commits + exits.
pub struct PolyExtrudeSweep {
    pub node_id: core_math::scene_graph::NodeId,
    /// PRE-sweep mesh snapshot (restored every preview rebuild).
    pub pre_mesh: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// Region face ids in `pre_mesh` (single face or connected multi-face).
    pub orig_faces: Vec<u32>,
    /// Face-region center in mesh/world space (poly local = world for authored meshes).
    pub face_center: Vec3,
    /// Start extrude direction (region average normal, projected onto WP when possible).
    pub start_dir: Vec3,
    /// Workplane basis for the invisible guide plane (through face_center).
    pub plane_origin: Vec3,
    pub plane_normal: Vec3,
    pub plane_u: Vec3,
    pub plane_v: Vec3,
    /// Live green-point (path end) on the guide plane.
    pub target: Vec3,
    /// Path sample count for the turn (scroll). 1 = single straight segment; higher = smoother.
    pub segments: u32,
    /// When Some, free angle is locked (typed 45/90 or snap); mouse only sets length.
    pub locked_angle_deg: Option<f32>,
    /// Numeric entry buffer while typing angle (e.g. "45").
    pub num_buf: String,
    /// Live cap face ids after last rebuild (for selection on confirm).
    pub cap_faces: Vec<u32>,
    pub orig_sel_faces: std::collections::HashSet<u32>,
    pub orig_sel_edges: std::collections::HashSet<u32>,
    pub orig_sel_vertices: std::collections::HashSet<u32>,
}

/// Poly edge extrude drag — fins from selected edges; new verts slide along a direction.
pub struct PolyEdgeExtrudeDrag {
    pub node_id: core_math::scene_graph::NodeId,
    /// Lifted vertex ids created by `extrude_edges` (moved each drag frame).
    pub new_verts: Vec<u32>,
    /// New edge half-edge ids (selection after confirm).
    pub new_edges: Vec<u32>,
    /// Unit direction to slide new verts (avg incident face normal).
    pub direction: glam::Vec3,
    pub start_mouse: egui::Pos2,
    pub accumulated_offset: f32,
    pub orig_sel_faces: std::collections::HashSet<u32>,
    pub orig_sel_edges: std::collections::HashSet<u32>,
    pub orig_sel_vertices: std::collections::HashSet<u32>,
}

/// Poly inset drag state — topology created at start, vertices interpolate each frame.
pub struct PolyInsetDrag {
    pub node_id: core_math::scene_graph::NodeId,
    /// NEW ring verts at thickness=0: `(VertexId.0, original pos)`. Dirs point inward; signed
    /// thickness slides them (`pos = orig + dir * amount`). Negative = outside (outer segment).
    pub original_positions: Vec<(u32, Vec3)>,
    /// Per-new-ring-vertex EVEN-OFFSET displacement per unit thickness.
    pub offset_dirs: Vec<Vec3>,
    /// Max positive (inward) thickness before the ring collapses (smart clamp).
    /// Computed once at drag start from even-offset dirs. Bevel keeps verts past clamp so the
    /// profile can still change; inset **merges** at clamp for clean complex-shape topology.
    pub max_inward: f32,
    /// True after we toast that clamp engaged (once per drag).
    pub warned_clamp: bool,
    /// The mesh BEFORE the inset. Restores for G/I mode switch and seeds the redo panel on confirm.
    pub before: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// The face selection the inset ran on (ids valid in `before`).
    pub faces: Vec<u32>,
    /// Island face ids (center). Selected when thickness ≥ 0.
    pub new_face_ids: Vec<u32>,
    /// Rim strip face ids (annulus). Selected when thickness < 0 (outer segment ready to thicken).
    pub rim_face_ids: Vec<u32>,
    /// True after rim faces have been flip_face'd for outward normals (amount < 0).
    pub rims_flipped: bool,
    pub start_mouse: egui::Pos2,
    pub accumulated_amount: f32,
    /// Unit average region normal (same as `inset_region`'s depth axis). Drag + Alt uses vertical
    /// mouse delta for depth; bare drag is thickness only (Ctrl = snap on thickness).
    pub depth_normal: Vec3,
    pub accumulated_depth: f32,
    // Pre-tool sub-element selection, snapshotted so cancel restores it (EXCANCELSEL).
    pub orig_sel_faces: std::collections::HashSet<u32>,
    pub orig_sel_edges: std::collections::HashSet<u32>,
    pub orig_sel_vertices: std::collections::HashSet<u32>,
}

/// Poly Shrink/Fatten interactive drag (Blender Alt+S). Moves each selected vertex along its OWN
/// vertex-normal (snapshotted at drag start) by a signed offset — mouse-right = fatten (+),
/// mouse-left = shrink (−). Pure vertex displacement: NO topology change, so it cannot hit the
/// half-edge twin/manifold pitfalls. Works from Vertex/Edge/Face select mode (the affected-vertex
/// set is gathered from the current selection). Undo via episode transaction like the other drags.
pub struct PolyShrinkFattenDrag {
    pub node_id: core_math::scene_graph::NodeId,
    /// (VertexId.0, original position, original unit vertex-normal) snapshotted at drag start.
    pub verts: Vec<(u32, Vec3, Vec3)>,
    pub start_mouse: egui::Pos2,
    pub accumulated_offset: f32,
}

/// Live **edge weight** drag (`W`) — writes the authored `CustomData.crease` value (0..1) on a set
/// of canonical edges while the mouse drags.
///
/// Edge mode weights the SELECTED edges; Face/Group mode weights the selected region's BOUNDARY
/// (`HalfEdgeMesh::region_boundary_half_edges`) — pressing `W` with faces selected is how you crease
/// an island's rim without switching to Edge mode.
///
/// The value is UNIFORM and ABSOLUTE across the whole set, not a per-edge delta: Shift snapping is
/// defined in absolute percent (20/40/60/80/100), and a per-edge offset would make one snap stop
/// mean five different weights on five different edges.
pub struct PolyEdgeWeightDrag {
    pub node_id: core_math::scene_graph::NodeId,
    /// Target edges + each one's PRE-DRAG weight. `None` = the edge carried no entry at all, which
    /// is a different state from an entry of `0.0` (a weightless-but-authored edge), so cancel
    /// writes these back verbatim rather than clamping everything to zero.
    pub keys: Vec<(core_math::poly_mesh::EdgeKey, Option<f32>)>,
    /// Anchor the drag measures from: the cursor position when the tool armed. Re-based on the Ctrl
    /// reset so the climb back up starts from 0 instead of jumping.
    pub start_mouse: egui::Pos2,
    /// Weight the current drag segment started from.
    pub base: f32,
    /// Live weight written to the sidecar this frame (0..1).
    pub weight: f32,
    /// Typed value as PERCENT digits ("40" → 0.4). Non-empty OVERRIDES the mouse (Blender-style
    /// numeric entry): once a number is typed, cursor motion must stop fighting it. Backspaced empty
    /// hands control back to the mouse.
    pub num_buf: String,
    /// Ctrl edge-detect latch — Ctrl resets the weight to 0 ONCE per press, not every frame it is
    /// held (a held-Ctrl reset would make the value un-draggable while snapping with Ctrl down).
    pub ctrl_was: bool,
}

/// Poly bevel interactive drag state — Blender-style modal bevel.
/// Ctrl+B starts, mouse drag = width, scroll = segments, LMB/Enter = confirm, Esc/RMB = cancel.
pub struct PolyBevelDrag {
    pub node_id: core_math::scene_graph::NodeId,
    pub original_mesh: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// Sidecar (`CustomData`) snapshot RE-KEYED ONTO `original_mesh` at drag start.
    ///
    /// The bevel REBUILDS the mesh every frame from `original_mesh`, which renumbers every
    /// `HalfEdgeId`/`FaceId`/`VertexId`. The runtime sidecar in `cache.poly_custom_data` is keyed by
    /// those ids, so accumulating into it per frame (or leaving a previous bevel's tags in place)
    /// leaves stale garbage: Smart Select's `op_created` tags from an EARLIER bevel end up pointing
    /// at unrelated edges of the new mesh ("bevel another edge first and the tags break").
    /// The base is captured ONCE here; each frame the install does
    /// `base.remap_by_position(original_mesh, new_mesh)` — positions survive renumbering — and then
    /// stamps this bevel's own created edges on top. Same contract the boolean install uses.
    pub original_custom: Option<core_math::poly_mesh::CustomData>,
    pub edges: std::collections::HashSet<u32>,
    /// VERTEX bevel: when non-empty, `edges` is ignored and these vertex ids are beveled
    /// (`affect=VERTICES`). Set when Ctrl+B is pressed in Vertex work mode with a vertex selection.
    pub verts: std::collections::HashSet<u32>,
    pub start_mouse: egui::Pos2,
    pub width: f32,
    /// Width at the moment the modal armed (or last HUD set). Mouse-X drag ADDS
    /// `dx.abs() * scale` on top of this so a seeded vertex bevel is not wiped to 0 on the
    /// first post-arm frame (when dx=0 the old absolute formula wrote width=0 and regenerate
    /// restored the original mesh — owner "i tried and did not work" 2026-08-10).
    pub width_base: f32,
    pub segments: u32,
    pub shape: f32, // profile: 0=flat chamfer, 0.5=circular fillet, 1=convex bulge [Codex P2]
    pub scroll_accum: f32,
    /// Clamp Overlap (Blender). true (default) = limit max width so a face can't invert.
    /// Toggle OFF (C key) to bevel wider than the geometry would otherwise allow.
    pub clamp: bool,
    /// Once-per-drag: already toasted that this selection forced segments→1 (can't round).
    pub warned_flat_force: bool,
    /// Once-per-drag: already toasted that Clamp Overlap is holding width below the drag request
    /// (geometry stuck while HUD width still grows — "going wider does nothing").
    pub warned_clamp_force: bool,
    /// Once-per-drag: already toasted that the install guard rejected the bevel result
    /// (silent snap-back — "nothing happens" while dragging).
    pub warned_install_reject: bool,
    /// Bevel CONSUME GEOMETRY (CAD-style, same behaviour as the Bevel modifier toggle):
    /// when width reaches the next parallel edge loop, that loop is absorbed
    /// (`core_math::bevel_absorb`, re-run from `original_mesh` every frame — so shrinking the
    /// width brings absorbed rings BACK during the drag; only confirm makes it destructive).
    /// Quad-grid rows only; barriers fall back to the normal clamp. HUD checkbox toggles it.
    pub consume: bool,
    /// True when this session was opened via Unbevel (Ctrl+Alt+B). Ctrl tap = remove full bevel + exit.
    pub is_unbevel: bool,
    /// Rising-edge latch for Ctrl remove (true while Ctrl held; remove fires on press after release).
    pub ctrl_was_down: bool,
    /// Rising-edge latch for the SHIFT tap that toggles `consume` (owner 2026-08-09). SHIFT is a
    /// modifier, not an `egui::Key`, so it cannot be expressed as an `ACTION_DEFS` chord — the
    /// modal owns it, exactly like `ctrl_was_down` above. Profile shape moved to ALT+move.
    /// Initialised `true` at every activation: a rebound SHIFT-carrying chord must not read as a
    /// tap on frame 1, and suppressing an edge the user has not made yet costs nothing.
    pub shift_was_down: bool,
}

/// Poly loop-cut SESSION (Blender Ctrl+R, single interactive state). Entered automatically when the
/// Loop Cut tool is active and an edge is hovered — no place-click. While not dragging it FOLLOWS the
/// hovered edge's ring (draggable preview immediately); LMB-drag slides; scroll = cut count; S =
/// paired support loops; C = clamp; RMB/Enter = confirm (bake); Esc = cancel. `live` (tool-settings
/// toggle) shows the real cut while sliding (re-cut from `original_mesh`); otherwise a yellow preview
/// until confirm. `limit` = partial cut (a multi-edge selection restricts the span).
///
/// A multi-edge selection that spans several independent rings cuts EVERY one of them: the primary
/// ring lives in `start_edge`/`limit`/`segment_mode` (it owns the slide rail, the preview seed and
/// R / K), the rest in `extra_rings`. See [`PolyLoopCutRing`].
pub struct PolyLoopCutSlide {
    pub node_id: core_math::scene_graph::NodeId,
    /// Pre-cut snapshot — restore point for `live` re-cut and for cancel.
    pub original_mesh: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// Current ring seed. Follows the hovered edge until a drag locks it.
    pub start_edge: u32,
    pub count: u32,
    /// Slide amount in [-1, 1] (0 = centre). Clamp OFF lets it exceed (t still kept geometrically valid).
    pub factor: f32,
    pub clamp: bool,
    /// Paired support-loop mode: cuts come in mirrored pairs straddling the ring centre (S toggles).
    pub paired: bool,
    /// WYSIWYG: re-cut the real mesh while sliding (vs a preview-only yellow line until confirm).
    pub live: bool,
    /// True while the LMB is held for a slide (locks the ring; otherwise the session follows hover).
    pub dragging: bool,
    /// Once the user drags or scrolls, the ring locks (stops following the hovered edge).
    pub locked: bool,
    /// Partial cut: when a run of edges is selected, restrict the ring to that span.
    /// In `segment_mode`, these are the edges that each get a mid (or slid) cut.
    pub limit: Option<HashSet<u32>>,
    /// No quad loop found: cut each edge in `limit` (or the seed edge) at the cut fraction(s)
    /// instead of walking a ring. Drag still slides placement along the seed edge axis.
    pub segment_mode: bool,
    pub start_mouse: egui::Pos2,
    /// Hovered-edge (rail) endpoints in WORLD space — the slide axis, projected each frame to map
    /// mouse motion → factor.
    pub rail_a_world: Vec3,
    pub rail_b_world: Vec3,
    pub scroll_accum: f32,
    pub num_buf: String,
    /// Follow-curvature strength for this session (0 = straight chord = the classic behaviour).
    /// Latched from the tool settings at session start; the Tool Parameters panel edits it live.
    pub curvature: f32,
    /// Rings beyond the primary, from a multi-edge selection — one loop cut each, sharing the
    /// session's count / factor / paired settings. EMPTY for a normal hover session, which is why
    /// single-ring behaviour is untouched.
    pub extra_rings: Vec<PolyLoopCutRing>,
}

/// One ring a loop-cut session will cut: where to seed the ring walk, how far to let it run, and
/// whether this seed has a real quad ring at all.
///
/// The primary ring is stored inline on [`PolyLoopCutSlide`]; this type carries the extra rings a
/// multi-edge selection produces. Same three fields, same meaning — so every apply path can treat
/// primary and extra identically instead of special-casing the first one.
#[derive(Clone, Debug)]
pub struct PolyLoopCutRing {
    /// Ring seed (a half-edge the cut runs perpendicular to).
    pub start_edge: u32,
    /// Partial cut: restrict the ring to this span. In `segment_mode` these are the edges that
    /// each get a cut. `None` = free full ring.
    pub limit: Option<HashSet<u32>>,
    /// No quad ring from this seed: cut each edge in `limit` (or the seed alone) instead of
    /// walking a strip.
    pub segment_mode: bool,
}

/// Poly DICE session — even U×V grid cuts on face(s).
/// L/R drag = vertical segments (U); U/D drag = horizontal segments (V);
/// Shift = lock equal segment counts; R = rotate grid about face normal (15° steps).
pub struct PolyDiceSession {
    pub node_id: core_math::scene_graph::NodeId,
    /// Faces to dice (selected set, or single hovered face).
    pub faces: HashSet<u32>,
    pub segs_u: u32,
    pub segs_v: u32,
    /// Radians about face normal.
    pub rotation: f32,
    pub start_mouse: egui::Pos2,
    pub last_mouse: egui::Pos2,
    pub dragging: bool,
    /// Accumulated drag in screen px before a segment step (avoids 1-step-per-pixel spam).
    pub drag_accum: egui::Vec2,
}

/// Poly VERTEX SLIDE (Blender Shift+V). One vertex rides along one of its incident edges: the drag
/// direction picks which neighbour (`chosen`), and the factor gives P = V0 + t·(N − V0). Pure vertex
/// move — no topology change. Snapshot for live re-move + cancel; committed as one undo on confirm.
pub struct PolyVertSlideDrag {
    pub node_id: core_math::scene_graph::NodeId,
    pub original_mesh: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    pub vert: u32,
    /// Candidate rails: each incident neighbour as (vertex id, WORLD position). The drag picks one.
    pub neighbors: Vec<(u32, Vec3)>,
    pub orig_world: Vec3,
    /// Index into `neighbors` — the active rail (re-picked each frame by best screen-direction match).
    pub chosen: usize,
    /// Slide amount; clamp ON keeps it in [0,1] (V0..neighbour), OFF lets it extrapolate.
    pub factor: f32,
    pub clamp: bool,
    pub start_mouse: egui::Pos2,
}

/// Last confirmed poly bevel — enables exact Unbevel / resize without topology guess.
/// Runtime only (not serialized).
///
/// **Staleness invariant:** restoring `original_mesh` is only legal while the live mesh is
/// still EXACTLY the post-bevel result this entry produced (`post_signature` matches).
/// Any later modeling makes the entry stale — using it would silently revert that work
/// (owner data-loss repro 2026-08-05). Stale entries are refused at use time, never dropped:
/// undoing back to the post-bevel state makes them valid again.
#[derive(Clone)]
pub struct PolyBevelHistory {
    pub node_id: core_math::scene_graph::NodeId,
    /// Mesh snapshot BEFORE this bevel was applied.
    pub original_mesh: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// `content_signature()` of the mesh right AFTER this bevel confirmed — the staleness gate.
    pub post_signature: u64,
    pub edges: std::collections::HashSet<u32>,
    pub verts: std::collections::HashSet<u32>,
    pub width: f32,
    pub segments: u32,
    pub shape: f32,
    pub clamp: bool,
}

/// Hinge rotate session phase.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PolyFaceHingePhase {
    /// Tool armed — click a mesh edge (segment) to choose the hinge.
    PickEdge,
    /// Hinge locked — drag to set angle.
    Rotate,
}

/// Face hinge rotate — sequential modal:
/// 1. Select poly body → activate tool
/// 2. Click a segment (edge) = hinge
/// 3. Drag to rotate the panel around that edge
///
/// Free verts of the hinge patch orbit the axis; connected side edges lengthen (planar-preserving).
///
/// Multi-face: the swinging patch is EVERY selected face (`faces`), not just one — a subdivided
/// door panel folds as one piece.
pub struct PolyFaceHingeRotate {
    pub node_id: core_math::scene_graph::NodeId,
    pub phase: PolyFaceHingePhase,
    /// Faces preselected at activate (Face-mode selection, sorted). Empty → resolve from the picked edge.
    pub preferred_faces: Vec<u32>,
    /// Hovered edge while picking (viewport highlight).
    pub hover_edge: Option<u32>,
    /// Snapshot taken when Rotate phase starts (pre-rotation mesh).
    pub original_mesh: Option<std::sync::Arc<core_math::half_edge::HalfEdgeMesh>>,
    /// The swinging patch — every face that folds around `hinge_edge` (never empty in Rotate).
    pub faces: Vec<u32>,
    /// The patch face that bounds `hinge_edge` — supplies the rest normal / swing direction.
    pub anchor_face: u32,
    pub hinge_edge: u32,
    pub axis_origin: glam::Vec3,
    pub axis_dir: glam::Vec3,
    /// Midpoint of the hinge segment (world) — where the rotate ring is centred.
    pub axis_mid: glam::Vec3,
    pub moving_verts: Vec<u32>,
    pub orig_positions: Vec<glam::Vec3>,
    pub start_mouse: egui::Pos2,
    pub angle_deg: f32,
    /// Dragging the rotation gizmo ring.
    pub gizmo_dragging: bool,
    /// Cursor over gizmo (highlight).
    pub gizmo_hover: bool,
}

/// Poly EDGE SLIDE (Blender GG / Ctrl+E → Edge Slide). The selected edge loop rides its two adjacent
/// face loops: each loop vertex moves to V0 + t·(rail_pos − V0) for t ≥ 0, V0 + |t|·(rail_neg − V0) for
/// t < 0 (single factor −1..1). Rails come from `HalfEdgeMesh::edge_slide_rails`. Pure vertex move.
pub struct PolyEdgeSlideDrag {
    pub node_id: core_math::scene_graph::NodeId,
    pub original_mesh: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// Per loop vertex: (vertex id, orig LOCAL pos, +rail LOCAL pos, −rail LOCAL pos).
    pub loop_verts: Vec<(u32, Vec3, Vec3, Vec3)>,
    pub factor: f32,
    pub clamp: bool,
    pub start_mouse: egui::Pos2,
    /// Representative slide axis in WORLD space (a loop vertex's −rail→+rail), projected to map mouse→t.
    pub rail_a_world: Vec3,
    pub rail_b_world: Vec3,
    /// When true (Alt+S multi-vert), re-pick each vert's ±rails from its fan using mouse direction.
    /// When false (Shift+E edge loop), rails stay coherent face-rails from `edge_slide_rails_loop`.
    pub mouse_pick_rails: bool,
}

/// NURBS CV drag state — CVs move in screen plane.
/// NURBS sub-element G/R/S — same fields/feel as `PolyTransformState` (compute_transform).
pub struct NurbsCvDrag {
    /// Solid (or single NurbsBody) node.
    pub node_id: core_math::scene_graph::NodeId,
    /// LOCAL CV originals: (face_idx, list of ((row,col), pos)).
    pub multi_face_idx: Vec<(u32, Vec<((u32, u32), Vec3)>)>,
    pub start_mouse: egui::Pos2,
    pub kind: crate::TransformKind,
    pub axis: crate::AxisConstraint,
    /// WORLD pivot (matches gizmo / Action Center).
    pub center: Vec3,
    pub free_mode: bool,
    pub frozen_orient: glam::Quat,
    pub frozen_space: crate::TransformSpace,
    pub confirm_on_release: bool,
    pub num_buf: String,
}

/// Interactive NURBS edge fillet (Ctrl+B) — mouse-X sets peak radius, Enter/LMB confirm, Esc cancel.
/// `V` cycles Const → Linear → Law → Const; typed digits set peak (`with_peak` preserves shape).
/// Law: 3-station seed (start/mid/end); `N` cycles station count 2/3/4.
pub struct NurbsFilletDrag {
    pub solid: core_math::scene_graph::NodeId,
    /// Edges at start: (face_idx, edge_id).
    pub edges: Vec<(u32, u8)>,
    pub start_mouse: egui::Pos2,
    /// Full radius law (Const / Linear / multi-station Law).
    pub radius: core_math::nurbs_fillet::Radius,
    pub max_radius: f32,
    /// Pre-fillet face snapshot for re-apply on drag.
    pub faces_snapshot: Vec<core_math::nurbs::NurbsFace>,
    /// Typed peak-radius override (poly G/R/S parity). Empty = mouse-X drives peak.
    pub num_buf: String,
}

/// Poly box select state — LMB drag draws a rectangle, release selects.
pub struct PolyBoxSelectState {
    pub start: egui::Pos2,
    pub current: egui::Pos2,
    pub shift: bool, // additive selection
}

/// Poly circle select state — persistent brush mode, LMB paints selection.
pub struct PolyCircleSelectState {
    pub radius: f32,  // screen-space radius in pixels
    pub active: bool, // LMB held = painting
}

/// Region-select shape chosen from the viewport RMB menu (persists in app state; default Box).
/// Drives the Alt+LMB region select in poly edit mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SelectionShape {
    #[default]
    Box,
    Circle,
    Lasso,
}

impl SelectionShape {
    pub fn label(self) -> &'static str {
        match self { Self::Box => "Box", Self::Circle => "Circle", Self::Lasso => "Lasso" }
    }
}

/// Poly REGION select (RMB drag). One tool covers all three shapes — the shape only changes the
/// screen-space predicate fed to `poly_screen_test`; `front_only` is the single occlusion flag
/// (plain RMB = front/visible, Ctrl/Alt+RMB = through/x-ray). Captured at drag start.
pub struct PolyRegionSelectState {
    pub shape: SelectionShape,
    /// true = front-facing only (occlusion-tested); false = through (x-ray, predicate only).
    pub front_only: bool,
    /// true = Shift+RMB drag: REMOVE hits from selection (deselect).
    /// false = plain RMB: replace (clear then add hits).
    pub subtract: bool,
    /// Rubber-band anchor (Box: rect corner; Circle: center). Screen space.
    pub start: egui::Pos2,
    /// Current cursor (Box: opposite corner; Circle: radius endpoint). Screen space.
    pub current: egui::Pos2,
    /// Lasso freehand path (screen space); also seeded with `start` for Box/Circle (unused there).
    pub path: Vec<egui::Pos2>,
}

/// Poly PAINT select (LMB drag). LMB press starts this; a sub-threshold release is a click-select,
/// a past-threshold drag paints every element under a small cursor brush (additive add per frame).
pub struct PolyPaintSelectState {
    pub start: egui::Pos2,
    pub last: egui::Pos2,
    /// true once the pointer moved past the click→drag threshold (→ paint; else release = click).
    pub painted: bool,
    pub shift: bool, // Shift held at press (additive click / preserved for click-select)
}

/// SDF Bevel Preview tool state (Sprint 21)
pub struct BevelPreviewState {
    pub active: bool,
    pub width: f32,
    pub depth: f32,
    pub shape: f32, // 0=concave, 0.5=flat, 1=convex
    pub profile: graphics::bevel_preview::BevelProfile,
    pub edge_count: u32,
    pub needs_edge_upload: bool,
    pub needs_param_upload: bool,
    /// Snapshot of selected edges at activation — tool reads from this, not live selection
    pub stored_edges: std::collections::HashSet<u32>,
    /// Cached bbox from last edge upload (avoids re-extracting every frame)
    pub cached_bbox_min: glam::Vec3,
    pub cached_bbox_max: glam::Vec3,
    /// Screen-space tile bounds (workgroup coords) for tiled dispatch
    pub tile_min: [u32; 2],
    pub tile_max: [u32; 2],
}

impl Default for BevelPreviewState {
    fn default() -> Self {
        Self {
            active: false,
            width: 0.05,
            depth: 0.0,
            shape: 1.0,
            profile: graphics::bevel_preview::BevelProfile::Arc,
            edge_count: 0,
            needs_edge_upload: false,
            needs_param_upload: false,
            stored_edges: std::collections::HashSet::new(),
            cached_bbox_min: glam::Vec3::ZERO,
            cached_bbox_max: glam::Vec3::ZERO,
            tile_min: [0; 2],
            tile_max: [0; 2],
        }
    }
}

/// Target for the blend-k diamond handle — one or more prims, or a body.
#[derive(Clone)]
pub enum BlendKTarget {
    /// Flat cache indices of every prim the drag mutates (single- or multi-select).
    /// Order: anchor (handle position / start_k source) first, then the rest.
    Prims(Vec<usize>),
    Body(core_math::scene_graph::NodeId),     // scene graph body node
}

/// Drag state for adjusting boolean blend-k via viewport handle.
/// Unified: same diamond, same drag, works for both prims and bodies.
/// 2-AXIS for Seam/Extrude targets (ONE episode = one undo for both axes):
/// vertical = the mode's signed primary value (seam depth / extrude strength),
/// horizontal = width. Classic blend-k targets use the horizontal axis only.
pub struct BlendKDrag {
    pub target: BlendKTarget,
    pub start_mouse_x: f32,
    pub start_mouse_y: f32,
    /// Drag-start PRIMARY value: blend k (BlendK mode), signed seam depth
    /// (SeamDepth), or signed extrude strength (ExtrudeStrength).
    pub start_k: f32,
    /// Drag-start SECONDARY value: seam/extrude width (0.0 for BlendK mode).
    pub start_w: f32,
}

/// Drag of the poly Proportional-Edit ORIGIN handle (the diamond at the centre of the
/// falloff ring). Moves `AzraApp::poly_prop_origin` — the point influence is measured
/// FROM — and nothing else. Deliberately NOT reusing `FalloffDrag` below: that one is the
/// SDF per-prim falloff gizmo (keyed by `prim_idx`, carries offset/length/k/direction),
/// a different subsystem with no overlapping field.
///
/// The drag is a pure screen→plane projection: the origin slides on the plane through its
/// own drag-start position whose normal is the view direction, so it tracks the cursor 1:1
/// at any camera angle without needing a mesh hit.
pub struct PropOriginDrag {
    /// Origin position (WORLD) at drag start — the plane point, and the Escape restore value.
    pub start_origin: Vec3,
    /// Cursor position at drag start; the delta is applied on the view plane.
    pub start_mouse: egui::Pos2,
    /// `poly_prop_origin` before the drag. `None` = it was auto (following the Action
    /// Centre), so Escape must restore auto rather than pinning it to `start_origin`.
    pub prev_origin: Option<Vec3>,
}

// Falloff gizmo drag handle types
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FalloffHandle { Origin, End, Radius }

pub struct FalloffDrag {
    pub prim_idx: usize,
    pub handle: FalloffHandle,
    pub start_mouse: egui::Pos2,
    pub start_offset: f32,     // original origin_offset at drag start
    pub start_length: f32,     // original length at drag start
    pub start_k: f32,          // original blend k at drag start
    pub start_direction: Vec3, // original direction at drag start (for end handle rotation)
}

// Key flags structs for contextual key handling
/// `Default` = "no key pressed this frame" — every field is a bool, so it costs nothing and
/// lets a dispatch test set the one or two chords it is exercising (`..Default::default()`)
/// instead of listing sixty falses.
#[derive(Default)]
pub struct PolyKeyFlags {
    pub delete_only_face: bool,
    pub extrude: bool,
    /// `E` while a knife rect/circle drag is anchored → lock the shape, pull the cut DEPTH.
    /// Consumed before `extrude` (state split, same chord — see `ACT_POLY_KNIFE_DEPTH`).
    pub knife_depth: bool,
    /// `U` while the knife is armed → shape cut ADDS material (Union); again = back to Subtract.
    pub knife_bool_union: bool,
    /// `I` while the knife is armed → shape cut KEEPS only the enclosed material (Intersect);
    /// again = back to Subtract. Consumed before `inset` (state split, same chord).
    pub knife_bool_intersect: bool,
    /// Alt+E Extrude Along Normals.
    pub extrude_normals: bool,
    /// Shift+Alt+E Extrude Sweep.
    pub extrude_sweep: bool,
    pub inset: bool,
    pub loop_cut: bool,
    pub vert_slide: bool,
    pub edge_slide: bool,
    pub shrink_fatten: bool,
    pub merge: bool,
    pub merge_menu: bool,
    pub knife: bool,
    pub bevel: bool,
    /// Ctrl+Alt+B — Unbevel / resize existing bevel strip.
    pub unbevel: bool,
    /// Ctrl+Shift+R — Face hinge rotate around preselected edge.
    pub face_hinge: bool,
    pub delete_face: bool,
    pub subdivide: bool,
    /// Ctrl+Alt+D — Dice selected faces into a U×V grid.
    pub dice: bool,
    pub edge_loop: bool,
    pub edge_ring: bool,
    pub box_select: bool,
    pub circle_select: bool,
    /// Plain C — smart UV seam (toggle edges / face-island boundary).
    pub seam: bool,
    pub circle_region: bool, // Alt+C — reshape selected face region's boundary onto a circle
    pub fill: bool,
    /// Ctrl+Alt+F Grid Fill.
    pub grid_fill: bool,
    /// Ctrl+Alt+R Spin.
    pub spin: bool,
    /// LoopTools Relax / Flatten / Space.
    pub loop_relax: bool,
    pub loop_flatten: bool,
    pub loop_space: bool,
    pub dissolve: bool,
    /// Ctrl+Shift+X Limited Dissolve.
    pub limited_dissolve: bool,
    /// Shift+G Select Similar.
    pub select_similar: bool,
    /// V — Rip edges.
    pub rip: bool,
    /// Alt+V — Rip Fill.
    pub rip_fill: bool,
    pub proportional: bool,
    pub prop_falloff: bool,
    pub recalc_normals: bool,
    pub flip_normals: bool,
    pub confirm_bevel: bool,
    pub cancel_bevel: bool,
    pub bridge: bool,
    pub connect: bool,
    /// P — Separate selected faces into a new poly body.
    pub separate: bool,
    /// `P` while the knife has a polyline: turn it into a procedural pipe.
    pub pipe: bool,
    /// Ctrl+Shift+B — Bisect by plane.
    pub bisect: bool,
    /// Ctrl+Alt+Shift+B — Bisect + clear −normal half.
    pub bisect_clear: bool,
    /// Ctrl+Shift+E / Ctrl+Alt+E — Mark / Clear Sharp on the selected edges.
    pub mark_sharp: bool,
    pub clear_sharp: bool,
    /// `W` — live edge-weight drag (authored `CustomData.crease`) on the selected edges, or on the
    /// selected face region's boundary. Same family as Mark Sharp / Mark Seam, but a VALUE, so it
    /// is dialled by drag instead of toggled.
    pub edge_weight: bool,
    /// Ctrl+Shift+N / Ctrl+Alt+N — Shade Smooth / Shade Flat on the body.
    pub shade_smooth: bool,
    pub shade_flat: bool,
    /// Ctrl+U — Smart UV Unwrap.
    pub uv_unwrap: bool,
    /// Alt+U — toggle UV Layout viewport overlay.
    pub uv_layout: bool,
}

pub struct NurbsKeyFlags {
    pub knot_u: bool,
    pub knot_v: bool,
    pub weight_up: bool,
    pub weight_down: bool,
    pub grab_cv: bool,
}
