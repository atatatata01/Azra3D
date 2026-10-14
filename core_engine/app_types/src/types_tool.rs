// Tool Mode + ActiveTool + ToolState — extracted from types.rs
use eframe::egui;
use crate::*;

/// State for the pivot editing mode (D key / Edit Pivot).
///
/// Two targets:
/// - **SDF prim** (`poly_nid = None`): moves the prim's position (shape center).
/// - **Poly / body node** (`poly_nid = Some`): moves `SceneNode.pivot` in world space;
///   geometry stays put. Drag the yellow marker or use G (grab) while the mode is live.
#[derive(Clone)]
#[allow(dead_code)]
pub struct PivotEditState {
    /// Pre-edit pivot (body-local for SDF prim; **world** for node pivot).
    pub original_pivot: glam::Vec3,
    pub original_rot: glam::Vec3,
    pub dragging: bool,
    pub drag_start_mouse: egui::Pos2,
    /// When set, edit this node's `pivot.position` (poly mesh / any node) instead of an SDF prim.
    pub node_id: Option<core_math::scene_graph::NodeId>,
}

/// Options for Extract Intersection (door-from-wall / create-pieces solid extract).
/// Works on **Subtract** cutters (plug from hole) and **Intersect** cutters
/// (freeze the intersection result into a new root body).
///
/// Default piece workflow: new body gets the solid; original cutter becomes **Subtract**.
#[derive(Clone, Debug)]
pub struct ExtractIntersectionSettings {
    /// false = only same body stack as the cutter (default).
    /// true  = any geometric AABB overlap in the scene.
    pub geometric_all: bool,
    /// true = keep source materials on copies (default). false = default material.
    pub keep_material: bool,
    /// false (default) = convert original cutter to **Subtract** (keep hole, create pieces).
    /// true = delete the original cutter after extract.
    pub remove_cutter: bool,
}

impl Default for ExtractIntersectionSettings {
    fn default() -> Self {
        Self {
            geometric_all: false,
            keep_material: true,
            remove_cutter: false,
        }
    }
}

/// Shift+D session: duplicates placed via **gizmo** (not free-mouse grab).
/// Tool params (offset / rotation / scale / count) live here and drive the layout.
/// Scroll adjusts count; C = circular; I = instance; M = Array-as-modifier; Space/Esc cancels.
///
/// Two targets: **SDF prims** (`source_uid` / `primary_uids`) or **poly bodies**
/// (`source_nid` / `primary_nids`). Exactly one path is active.
#[derive(Clone)]
pub struct ArrayDupSession {
    /// Original prim uid (stays put). 0 when this is a poly-body session.
    pub source_uid: u32,
    /// Body-local / node-local position of the original at session start.
    pub origin_local: glam::Vec3,
    /// Original rotation (deg) at session start.
    pub origin_rot: glam::Vec3,
    /// Original scale at session start.
    pub origin_scale: glam::Vec3,
    /// End-copy relative offset from origin. Primary sits at origin+offset (linear).
    pub offset: glam::Vec3,
    /// End-copy relative rotation delta (deg).
    pub rotation_deg: glam::Vec3,
    /// End-copy scale multiplier (1,1,1 = same as original).
    pub scale: glam::Vec3,
    /// Number of copies along the path (1 = just the primary dup; 2+ = intermediates).
    pub array_count: u32,
    /// Intermediate array prim uids (not the primary dragged duplicate).
    pub array_prim_uids: Vec<u32>,
    /// Primary duplicate uids (currently selected / gizmo targets).
    pub primary_uids: Vec<u32>,
    /// Scroll accumulation (one wheel notch ≈ one count step).
    pub scroll_accum: f32,
    /// Poly body path: original NodeId (None = SDF prim session).
    pub source_nid: Option<core_math::scene_graph::NodeId>,
    /// Poly: intermediate body NodeIds.
    pub array_nids: Vec<core_math::scene_graph::NodeId>,
    /// Poly: primary duplicate NodeIds (gizmo targets).
    pub primary_nids: Vec<core_math::scene_graph::NodeId>,
    /// C key: circular array around origin (empty center) vs linear along offset.
    pub array_circle: bool,
    /// M key: when true, Enter applies Array/ArrayCircular modifier on the source
    /// instead of keeping real body copies.
    pub as_modifier: bool,
    /// I key: copies share the source mesh Arc (instance-like; COW on edit).
    pub as_instance: bool,
}

// ==================================================================
// Tool Mode — persistent mode state (mutually exclusive)
// ==================================================================
#[derive(PartialEq, Clone, Debug)]
pub enum ToolMode {
    Select,
    CreateArmed { shape_idx: usize },
    Bevel,
    Extrude,
    BoolCut,
}

impl ToolMode {
    pub fn is_select(&self) -> bool { matches!(self, Self::Select) }
    pub fn is_create_armed(&self) -> bool { matches!(self, Self::CreateArmed { .. }) }
    pub fn is_bevel(&self) -> bool { matches!(self, Self::Bevel) }
    pub fn is_extrude(&self) -> bool { matches!(self, Self::Extrude) }
    pub fn is_bool_cut(&self) -> bool { matches!(self, Self::BoolCut) }
    pub fn armed_shape_idx(&self) -> Option<usize> {
        match self { Self::CreateArmed { shape_idx } => Some(*shape_idx), _ => None }
    }
}

// ==================================================================
// Tool State Machine (Phase 3 refactor — extracted from AzraApp)
// ==================================================================
pub enum ActiveTool {
    Idle,
    Transform(TransformState),
    Gizmo(GizmoInteraction),
    FacePushPull(FaceDrag),
    Bevel(BevelDrag),
    BoolEdgeBevel(BoolEdgeBevelDrag),
    FalloffAdjust(FalloffDrag),
    Create(CreateState),
    SmartMirror(SmartMirrorState),
    PivotEdit(PivotEditState),
    SelectionBox(SelectionDrag),
    PolyExtrude(PolyExtrudeDrag),
    /// Shift+Alt+E path sweep extrude (duct turn).
    PolyExtrudeSweep(crate::PolyExtrudeSweep),
    PolyEdgeExtrude(crate::PolyEdgeExtrudeDrag),
    PolyInset(PolyInsetDrag),
    PolyBoxSelect(PolyBoxSelectState),
    PolyCircleSelect(PolyCircleSelectState),
    PolyPaintSelect(PolyPaintSelectState),
    PolyRegionSelect(PolyRegionSelectState),
    PolyTransform(PolyTransformState),
    /// Dragging the Proportional-Edit origin diamond (moves the falloff centre only —
    /// never the rotate/scale pivot, which stays on the Action Centre).
    PolyPropOrigin(crate::PropOriginDrag),
    PolyBevel(PolyBevelDrag),
    /// Alt+X poly: place symmetry plane (hold-tool) then symmetrize / cut / instance.
    PolySymmetry(crate::PolySymmetryState),
    PolyLoopCutSlide(PolyLoopCutSlide),
    PolyVertSlide(PolyVertSlideDrag),
    PolyEdgeSlide(PolyEdgeSlideDrag),
    PolyFaceHingeRotate(crate::PolyFaceHingeRotate),
    PolyShrinkFatten(PolyShrinkFattenDrag),
    NurbsCvMove(NurbsCvDrag),
    /// Ctrl+B rolling-ball fillet drag (radius by mouse-X).
    NurbsFillet(crate::NurbsFilletDrag),
    BlendKDrag(BlendKDrag),
    ClipCurveDraw(ClipCurveDrawState),
    CurvePointTransform(CurvePointDrag),
    CurveBevelTransform(CurveBevelDrag),
    /// Modal SpinePipe point authoring (seed sphere → Shift+add → Space finish).
    /// Exclusive: other shortcuts must not fire while this is active.
    SpinePointEdit { prim_uid: u32 },
    /// `I` in Edge mode — Offset Edge Loop modal
    /// (`docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md` §4 P4). APPENDED at the end of the enum.
    PolyOffsetLoop(PolyOffsetLoopDrag),
    /// Face Dice — even U×V grid cuts (Ctrl+Alt+D). APPENDED (save-format red line: ActiveTool
    /// is runtime-only, not serialized).
    PolyDice(PolyDiceSession),
    /// `W` — live edge-weight (crease) drag. APPENDED, same reason.
    PolyEdgeWeight(crate::PolyEdgeWeightDrag),
}

/// Live Offset Edge Loop drag (`I` in Edge mode) — a parallel copy of a selected CLOSED edge loop
/// at an ABSOLUTE world distance, the original loop KEPT.
///
/// The kernel (`HalfEdgeMesh::offset_edge_loop`) takes a SIGNED distance whose sign selects which
/// adjacent face the new loop lands in — but that sign is derived from the ring walk's
/// lowest-vertex-id start and is therefore geometrically ARBITRARY (plan §7b). So the drag is bound
/// to a side by MEASUREMENT: at arm time the op is probed once per sign on a throwaway clone and
/// the resulting loop centroid is stored in `land_pos_world` / `land_neg_world`; each frame both are
/// projected to screen and the sign whose landing the cursor is heading toward wins.
pub struct PolyOffsetLoopDrag {
    pub node_id: core_math::scene_graph::NodeId,
    /// Drag-start base. EVERY frame re-runs the op from this — never from the previous result.
    pub original_mesh: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// The selected loop as half-edge ids on `original_mesh` (already filtered to live ids).
    pub edges: std::collections::HashSet<u32>,
    pub start_mouse: egui::Pos2,
    /// WORLD centroid of the original loop's vertices — the screen-axis anchor. World, not local:
    /// mesh positions are body-local (CLAUDE.md § Prim Position Space).
    pub anchor_world: glam::Vec3,
    /// UNIT world direction from `anchor_world` toward where the probe put the new loop for
    /// `distance > 0`; `None` when that side is topologically illegal (no off-loop rail / boundary
    /// edge / ambiguous rail / would split one face twice).
    ///
    /// A direction, not the landing POINT, on purpose: the probe offset is deliberately tiny (a few
    /// percent of an edge length, so no vertex hits its rail clamp and the direction is exact), and
    /// a landing point that close to the anchor projects to a sub-pixel screen axis that could not
    /// be told apart from a genuinely edge-on view.
    pub dir_pos_world: Option<glam::Vec3>,
    /// Same for `distance < 0`.
    pub dir_neg_world: Option<glam::Vec3>,
    /// Currently REQUESTED signed offset (|value| = absolute world distance, sign = side).
    pub distance: f32,
    /// The signed offset actually INSTALLED on the body, or `None` while the body still holds the
    /// untouched drag-start mesh. Distinct from `distance` because a frame the kernel refuses
    /// (typically an over-drag) keeps the last good geometry while `distance` moves on — confirming
    /// must report, and commit, what is really on the body. `None` at confirm discards the episode
    /// instead of committing a no-op undo entry.
    pub installed_distance: Option<f32>,
    /// Once-per-drag: already toasted the kernel's refusal reason (typically an over-drag).
    pub warned_refusal: bool,
    /// Once-per-drag: already toasted that the view is edge-on to the offset direction, so the
    /// side cannot be picked by measurement this frame.
    pub warned_edge_on: bool,
    /// P3: rail BOTH adjacent faces of the selected segment (same |distance| each side).
    /// Shift tap toggles. Illegal when only one side has a face to carve into.
    pub both_sides: bool,
    /// Rising-edge latch for Shift. Seeded `true` so arming with Shift held cannot fire the toggle.
    pub shift_was_down: bool,
}

/// Press-drag gesture on the PERSISTENT bevel diamond of a `CurveExtrude` (Polyline) prim's
/// selected profile points. THE one bevel interaction (the old keyboard-B auto-size modal was
/// removed — radius changes ONLY while this diamond drag is live): horizontal drag fine-tunes
/// the |bevel_radius| of every selected point from `start_radius`; an unmoved press+release is
/// a CLICK and cycles the corner type instead. One undo episode covers the whole gesture. The
/// corner TYPE (round/chamfer) is fixed for the gesture and carried in `sign`.
pub struct CurveBevelDrag {
    /// Prim that owns the profile (stable id; resolved to a flat index at apply time).
    pub prim_uid: u32,
    /// Selected profile-vertex indices being beveled.
    pub targets: Vec<usize>,
    /// Original signed bevel_radius per target (for cancel-restore). Parallel to `targets`.
    pub original: Vec<f32>,
    /// Corner polarity to write: +1.0 = round, −1.0 = chamfer (magnitude = radius).
    pub sign: f32,
    /// Selection centroid in profile UV — where the diamond handle is anchored.
    pub anchor_uv: glam::Vec2,
    /// World-space profile-plane frame (handle placement), captured at start.
    pub plane_origin: glam::Vec3,
    pub plane_u: glam::Vec3,
    pub plane_v: glam::Vec3,
    /// Current bevel radius (unsigned magnitude).
    pub radius: f32,
    /// Mouse pos at press (the drag's delta anchor).
    pub start_mouse: egui::Pos2,
    /// Radius at press.
    pub start_radius: f32,
    /// The pointer moved past the click threshold → treat as a radius drag on release;
    /// an unmoved press+release is a CLICK → cycle the corner type instead.
    pub moved: bool,
}

/// Modal transform (G/R/S) state for editing the profile points of a selected
/// `SdfShape::CurveExtrude` (Polyline) prim. Operates in the prim's profile PLANE:
/// positions are 2D UV coords (`a` = prim-local X, `b` = prim-local Z), the world
/// plane frame is captured at drag start so mouse motion maps 1:1 to UV for Grab.
pub struct CurvePointDrag {
    /// Prim that owns the profile (stable id; resolved to a flat index at apply time).
    pub prim_uid: u32,
    pub kind: TransformKind,
    /// In-plane axis lock: `X` = lock to UV.x, `Y` = lock to UV.y, else free.
    pub axis: AxisConstraint,
    /// (profile vertex index, original UV position) snapshot at drag start.
    pub original: Vec<(usize, glam::Vec2)>,
    /// Centroid of the selected points (UV) — pivot for Rotate / Scale.
    pub center: glam::Vec2,
    pub start_mouse: egui::Pos2,
    pub num_buf: String,
    /// World-space profile-plane frame captured at start (for mouse → UV mapping).
    pub plane_origin: glam::Vec3,
    pub plane_normal: glam::Vec3,
    /// World vector spanning +1 unit of UV.x (already includes prim + body scale).
    pub plane_u: glam::Vec3,
    /// World vector spanning +1 unit of UV.y.
    pub plane_v: glam::Vec3,
    /// Direct pointer drag (started on LMB press over a handle/edge) — viewport-driven & committed
    /// on release, vs a keyboard G/R/S modal (`false`, driven from the keyboard handler).
    pub direct: bool,
    /// What the press landed on (direct drags only): 1 = point handle, 2 = edge/segment, 3 = a
    /// freshly shift-added point. 0 = keyboard modal. Drives click-vs-drag release semantics.
    pub press_kind: u8,
    /// True once the pointer moved past the click threshold (direct drags only).
    pub moved: bool,
    /// Candidate edge directions (UV, normalized) for Ctrl+Shift edge-slide: for a single-point drag
    /// the two adjacent edges; for a segment drag the edge's own axis. Empty ⇒ no slide constraint.
    pub slide_dirs: Vec<glam::Vec2>,
    /// Previous-vertex UV for a single-point drag — anchor for Ctrl "corner angle to 45°" snapping.
    pub corner_prev: Option<glam::Vec2>,
}

/// State for the interactive clip-curve polygon drawing tool.
/// User clicks to place vertices on the workplane, Enter to commit, Escape to cancel.
pub struct ClipCurveDrawState {
    /// Target prim flat-cache index.
    pub prim_idx: usize,
    /// Workplane frame captured at tool start (world space).
    pub plane_origin: glam::Vec3,
    pub plane_normal: glam::Vec3,
    pub plane_u: glam::Vec3,
    pub plane_v: glam::Vec3,
    /// Polygon vertices in plane-local 2D coords, accumulated as user clicks.
    pub verts: Vec<core_math::primitives::CurveVertex>,
    /// Current mouse position projected onto the workplane (for preview edge).
    pub cursor_2d: glam::Vec2,
    /// Whether cursor is on a valid workplane position.
    pub cursor_valid: bool,
    /// When true, commit CREATES a new CurveExtrude primitive from the drawn profile
    /// (the "Draw Polyline" authoring flow) instead of applying a clip curve to `prim_idx`.
    pub create_prim: bool,
}

impl ActiveTool {
    // ---- Is checks ----
    pub fn is_idle(&self) -> bool { matches!(self, Self::Idle) }
    pub fn is_clip_curve_draw(&self) -> bool { matches!(self, Self::ClipCurveDraw(_)) }
    pub fn is_busy(&self) -> bool { !self.is_idle() }
    pub fn is_transform(&self) -> bool { matches!(self, Self::Transform(_)) }
    pub fn is_gizmo(&self) -> bool { matches!(self, Self::Gizmo(_)) }
    pub fn is_create(&self) -> bool { matches!(self, Self::Create(_)) }
    pub fn is_face_push_pull(&self) -> bool { matches!(self, Self::FacePushPull(_)) }
    pub fn is_bevel(&self) -> bool { matches!(self, Self::Bevel(_)) }
    pub fn is_selection_box(&self) -> bool { matches!(self, Self::SelectionBox(_)) }
    pub fn is_smart_mirror(&self) -> bool { matches!(self, Self::SmartMirror(_)) }
    pub fn is_pivot_edit(&self) -> bool { matches!(self, Self::PivotEdit(_)) }
    pub fn is_falloff_adjust(&self) -> bool { matches!(self, Self::FalloffAdjust(_)) }
    pub fn is_bool_edge_bevel(&self) -> bool { matches!(self, Self::BoolEdgeBevel(_)) }
    pub fn is_poly_extrude(&self) -> bool { matches!(self, Self::PolyExtrude(_)) }
    pub fn is_poly_extrude_sweep(&self) -> bool { matches!(self, Self::PolyExtrudeSweep(_)) }
    pub fn is_poly_inset(&self) -> bool { matches!(self, Self::PolyInset(_)) }
    pub fn is_poly_edge_extrude(&self) -> bool { matches!(self, Self::PolyEdgeExtrude(_)) }
    pub fn is_poly_box_select(&self) -> bool { matches!(self, Self::PolyBoxSelect(_)) }
    pub fn is_poly_circle_select(&self) -> bool { matches!(self, Self::PolyCircleSelect(_)) }
    pub fn is_poly_paint_select(&self) -> bool { matches!(self, Self::PolyPaintSelect(_)) }
    pub fn is_poly_region_select(&self) -> bool { matches!(self, Self::PolyRegionSelect(_)) }
    /// True while any rubber-band / paint / scene box select owns the pointer.
    ///
    /// Used to hard-gate scroll-bevel / knife-shape segment wheels so they can never mutate
    /// geometry or tool state mid-selection (owner 2026-08-06: "scroll bevel do not mess up
    /// my drag selection").
    pub fn is_drag_select_tool(&self) -> bool {
        matches!(
            self,
            Self::PolyBoxSelect(_)
                | Self::PolyCircleSelect(_)
                | Self::PolyPaintSelect(_)
                | Self::PolyRegionSelect(_)
                | Self::SelectionBox(_)
        )
    }
    pub fn is_poly_transform(&self) -> bool { matches!(self, Self::PolyTransform(_)) }
    pub fn is_poly_prop_origin(&self) -> bool { matches!(self, Self::PolyPropOrigin(_)) }
    /// Any poly modal that OWNS the keyboard until confirmed or cancelled.
    ///
    /// Drives the Tool Exit Convention ("switching tools = confirm current work, then switch"):
    /// pressing a tool key while one of these is live confirms it instead of being swallowed.
    /// Deliberately EXCLUDES `PolyTransform` — that IS the tool being switched to, so including it
    /// would make G confirm-and-restart itself. Keep in lockstep with
    /// `AzraApp::confirm_poly_modal`.
    pub fn is_poly_modal(&self) -> bool {
        matches!(
            self,
            Self::PolyLoopCutSlide(_)
                | Self::PolyBevel(_)
                | Self::PolyFaceHingeRotate(_)
                | Self::PolyExtrude(_)
                | Self::PolyExtrudeSweep(_)
                | Self::PolyEdgeExtrude(_)
                | Self::PolyInset(_)
                | Self::PolyShrinkFatten(_)
                | Self::PolyOffsetLoop(_)
                | Self::PolyDice(_)
        )
    }
    pub fn is_poly_offset_loop(&self) -> bool { matches!(self, Self::PolyOffsetLoop(_)) }
    pub fn is_poly_dice(&self) -> bool { matches!(self, Self::PolyDice(_)) }
    pub fn as_poly_dice(&self) -> Option<&PolyDiceSession> {
        match self { Self::PolyDice(d) => Some(d), _ => None }
    }
    pub fn as_poly_dice_mut(&mut self) -> Option<&mut PolyDiceSession> {
        match self { Self::PolyDice(d) => Some(d), _ => None }
    }
    pub fn is_nurbs_cv_move(&self) -> bool { matches!(self, Self::NurbsCvMove(_)) }
    pub fn is_nurbs_fillet(&self) -> bool { matches!(self, Self::NurbsFillet(_)) }
    pub fn is_poly_bevel(&self) -> bool { matches!(self, Self::PolyBevel(_)) }
    pub fn is_poly_symmetry(&self) -> bool { matches!(self, Self::PolySymmetry(_)) }
    pub fn is_poly_loop_cut_slide(&self) -> bool { matches!(self, Self::PolyLoopCutSlide(_)) }
    pub fn is_poly_vert_slide(&self) -> bool { matches!(self, Self::PolyVertSlide(_)) }
    pub fn is_poly_edge_slide(&self) -> bool { matches!(self, Self::PolyEdgeSlide(_)) }
    pub fn is_poly_slide(&self) -> bool { self.is_poly_vert_slide() || self.is_poly_edge_slide() }
    pub fn is_poly_face_hinge(&self) -> bool { matches!(self, Self::PolyFaceHingeRotate(_)) }
    pub fn is_blend_k_drag(&self) -> bool { matches!(self, Self::BlendKDrag(_)) }
    pub fn is_curve_point_transform(&self) -> bool { matches!(self, Self::CurvePointTransform(_)) }
    pub fn is_curve_bevel(&self) -> bool { matches!(self, Self::CurveBevelTransform(_)) }
    pub fn is_spine_point_edit(&self) -> bool { matches!(self, Self::SpinePointEdit { .. }) }

    // ---- Diagnostic ----
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Idle => "Idle", Self::Transform(_) => "Transform",
            Self::Gizmo(_) => "Gizmo", Self::FacePushPull(_) => "Push/Pull",
            Self::Bevel(_) => "Bevel", Self::BoolEdgeBevel(_) => "Bool Edge Bevel",
            Self::FalloffAdjust(_) => "Falloff Adjust", Self::Create(_) => "Create",
            Self::SmartMirror(_) => "Smart Mirror", Self::PivotEdit(_) => "Pivot Edit",
            Self::SelectionBox(_) => "Selection Box", Self::PolyExtrude(_) => "Poly Extrude",
            Self::PolyExtrudeSweep(_) => "Extrude Sweep",
            Self::PolyEdgeExtrude(_) => "Poly Edge Extrude",
            Self::PolyInset(_) => "Poly Inset", Self::PolyBoxSelect(_) => "Poly Box Select",
            Self::PolyCircleSelect(_) => "Poly Circle Select",
            Self::PolyPaintSelect(_) => "Poly Paint Select", Self::PolyRegionSelect(_) => "Poly Region Select",
            Self::PolyTransform(_) => "Poly Transform",
            Self::PolyPropOrigin(_) => "Prop Origin Move",
            Self::PolyBevel(_) => "Poly Bevel",
            Self::PolySymmetry(_) => "Poly Symmetry",
            Self::PolyLoopCutSlide(_) => "Loop Cut Slide",
            Self::PolyVertSlide(_) => "Vertex Slide", Self::PolyEdgeSlide(_) => "Edge Slide",
            Self::PolyFaceHingeRotate(_) => "Face Hinge Rotate",
            Self::PolyShrinkFatten(_) => "Shrink/Fatten",
            Self::NurbsCvMove(_) => "NURBS CV Move",
            Self::NurbsFillet(_) => "NURBS Fillet",
            Self::BlendKDrag(_) => "Blend K Drag", Self::ClipCurveDraw(_) => "Clip Curve Draw",
            Self::CurvePointTransform(_) => "Curve Point Transform",
            Self::CurveBevelTransform(_) => "Curve Bevel",
            Self::SpinePointEdit { .. } => "Spine Point Edit",
            Self::PolyOffsetLoop(_) => "Offset Edge Loop",
            Self::PolyDice(_) => "Dice",
            Self::PolyEdgeWeight(_) => "Edge Weight",
        }
    }

    // ---- Ref accessors ----
    pub fn as_transform(&self) -> Option<&TransformState> {
        match self { Self::Transform(t) => Some(t), _ => None }
    }
    pub fn as_transform_mut(&mut self) -> Option<&mut TransformState> {
        match self { Self::Transform(t) => Some(t), _ => None }
    }
    pub fn as_gizmo(&self) -> Option<&GizmoInteraction> {
        match self { Self::Gizmo(g) => Some(g), _ => None }
    }
    pub fn as_gizmo_mut(&mut self) -> Option<&mut GizmoInteraction> {
        match self { Self::Gizmo(g) => Some(g), _ => None }
    }
    pub fn as_create(&self) -> Option<&CreateState> {
        match self { Self::Create(cs) => Some(cs), _ => None }
    }
    pub fn as_create_mut(&mut self) -> Option<&mut CreateState> {
        match self { Self::Create(cs) => Some(cs), _ => None }
    }
    pub fn as_face_push_pull(&self) -> Option<&FaceDrag> {
        match self { Self::FacePushPull(fd) => Some(fd), _ => None }
    }
    pub fn as_face_push_pull_mut(&mut self) -> Option<&mut FaceDrag> {
        match self { Self::FacePushPull(fd) => Some(fd), _ => None }
    }
    pub fn as_bevel(&self) -> Option<&BevelDrag> {
        match self { Self::Bevel(bd) => Some(bd), _ => None }
    }
    pub fn as_bool_edge_bevel(&self) -> Option<&BoolEdgeBevelDrag> {
        match self { Self::BoolEdgeBevel(bd) => Some(bd), _ => None }
    }
    pub fn as_falloff_adjust(&self) -> Option<&FalloffDrag> {
        match self { Self::FalloffAdjust(fd) => Some(fd), _ => None }
    }
    pub fn as_smart_mirror(&self) -> Option<&SmartMirrorState> {
        match self { Self::SmartMirror(sm) => Some(sm), _ => None }
    }
    pub fn as_smart_mirror_mut(&mut self) -> Option<&mut SmartMirrorState> {
        match self { Self::SmartMirror(sm) => Some(sm), _ => None }
    }
    pub fn as_pivot_edit(&self) -> Option<&PivotEditState> {
        match self { Self::PivotEdit(pe) => Some(pe), _ => None }
    }
    pub fn as_pivot_edit_mut(&mut self) -> Option<&mut PivotEditState> {
        match self { Self::PivotEdit(pe) => Some(pe), _ => None }
    }
    pub fn as_selection_box(&self) -> Option<&SelectionDrag> {
        match self { Self::SelectionBox(sd) => Some(sd), _ => None }
    }
    pub fn as_poly_extrude(&self) -> Option<&PolyExtrudeDrag> {
        match self { Self::PolyExtrude(d) => Some(d), _ => None }
    }
    pub fn as_poly_extrude_mut(&mut self) -> Option<&mut PolyExtrudeDrag> {
        match self { Self::PolyExtrude(d) => Some(d), _ => None }
    }
    pub fn as_poly_extrude_sweep(&self) -> Option<&crate::PolyExtrudeSweep> {
        match self { Self::PolyExtrudeSweep(d) => Some(d), _ => None }
    }
    pub fn as_poly_extrude_sweep_mut(&mut self) -> Option<&mut crate::PolyExtrudeSweep> {
        match self { Self::PolyExtrudeSweep(d) => Some(d), _ => None }
    }
    pub fn as_poly_edge_extrude(&self) -> Option<&crate::PolyEdgeExtrudeDrag> {
        match self { Self::PolyEdgeExtrude(d) => Some(d), _ => None }
    }
    pub fn as_poly_edge_extrude_mut(&mut self) -> Option<&mut crate::PolyEdgeExtrudeDrag> {
        match self { Self::PolyEdgeExtrude(d) => Some(d), _ => None }
    }
    pub fn as_poly_inset(&self) -> Option<&PolyInsetDrag> {
        match self { Self::PolyInset(d) => Some(d), _ => None }
    }
    pub fn as_poly_inset_mut(&mut self) -> Option<&mut PolyInsetDrag> {
        match self { Self::PolyInset(d) => Some(d), _ => None }
    }
    pub fn is_poly_edge_weight(&self) -> bool { matches!(self, Self::PolyEdgeWeight(_)) }
    pub fn as_poly_edge_weight(&self) -> Option<&crate::PolyEdgeWeightDrag> {
        match self { Self::PolyEdgeWeight(d) => Some(d), _ => None }
    }
    pub fn as_poly_edge_weight_mut(&mut self) -> Option<&mut crate::PolyEdgeWeightDrag> {
        match self { Self::PolyEdgeWeight(d) => Some(d), _ => None }
    }
    pub fn is_poly_shrink_fatten(&self) -> bool { matches!(self, Self::PolyShrinkFatten(_)) }
    pub fn as_poly_shrink_fatten(&self) -> Option<&PolyShrinkFattenDrag> {
        match self { Self::PolyShrinkFatten(d) => Some(d), _ => None }
    }
    pub fn as_poly_shrink_fatten_mut(&mut self) -> Option<&mut PolyShrinkFattenDrag> {
        match self { Self::PolyShrinkFatten(d) => Some(d), _ => None }
    }
    pub fn as_poly_transform(&self) -> Option<&PolyTransformState> {
        match self { Self::PolyTransform(t) => Some(t), _ => None }
    }
    pub fn as_poly_transform_mut(&mut self) -> Option<&mut PolyTransformState> {
        match self { Self::PolyTransform(t) => Some(t), _ => None }
    }
    pub fn as_poly_prop_origin(&self) -> Option<&crate::PropOriginDrag> {
        match self { Self::PolyPropOrigin(d) => Some(d), _ => None }
    }
    pub fn as_nurbs_cv_move(&self) -> Option<&NurbsCvDrag> {
        match self { Self::NurbsCvMove(d) => Some(d), _ => None }
    }
    pub fn as_nurbs_cv_move_mut(&mut self) -> Option<&mut NurbsCvDrag> {
        match self { Self::NurbsCvMove(d) => Some(d), _ => None }
    }
    pub fn as_nurbs_fillet(&self) -> Option<&crate::NurbsFilletDrag> {
        match self { Self::NurbsFillet(d) => Some(d), _ => None }
    }
    pub fn as_nurbs_fillet_mut(&mut self) -> Option<&mut crate::NurbsFilletDrag> {
        match self { Self::NurbsFillet(d) => Some(d), _ => None }
    }
    pub fn as_poly_bevel(&self) -> Option<&PolyBevelDrag> {
        match self { Self::PolyBevel(d) => Some(d), _ => None }
    }
    pub fn as_poly_bevel_mut(&mut self) -> Option<&mut PolyBevelDrag> {
        match self { Self::PolyBevel(d) => Some(d), _ => None }
    }
    pub fn as_poly_symmetry(&self) -> Option<&PolySymmetryState> {
        match self { Self::PolySymmetry(d) => Some(d), _ => None }
    }
    pub fn as_poly_symmetry_mut(&mut self) -> Option<&mut PolySymmetryState> {
        match self { Self::PolySymmetry(d) => Some(d), _ => None }
    }
    pub fn as_poly_loop_cut_slide(&self) -> Option<&PolyLoopCutSlide> {
        match self { Self::PolyLoopCutSlide(d) => Some(d), _ => None }
    }
    pub fn as_poly_loop_cut_slide_mut(&mut self) -> Option<&mut PolyLoopCutSlide> {
        match self { Self::PolyLoopCutSlide(d) => Some(d), _ => None }
    }
    pub fn as_poly_vert_slide(&self) -> Option<&PolyVertSlideDrag> {
        match self { Self::PolyVertSlide(d) => Some(d), _ => None }
    }
    pub fn as_poly_vert_slide_mut(&mut self) -> Option<&mut PolyVertSlideDrag> {
        match self { Self::PolyVertSlide(d) => Some(d), _ => None }
    }
    pub fn as_poly_edge_slide(&self) -> Option<&PolyEdgeSlideDrag> {
        match self { Self::PolyEdgeSlide(d) => Some(d), _ => None }
    }
    pub fn as_poly_edge_slide_mut(&mut self) -> Option<&mut PolyEdgeSlideDrag> {
        match self { Self::PolyEdgeSlide(d) => Some(d), _ => None }
    }
    pub fn as_poly_face_hinge(&self) -> Option<&crate::PolyFaceHingeRotate> {
        match self { Self::PolyFaceHingeRotate(d) => Some(d), _ => None }
    }
    pub fn as_poly_face_hinge_mut(&mut self) -> Option<&mut crate::PolyFaceHingeRotate> {
        match self { Self::PolyFaceHingeRotate(d) => Some(d), _ => None }
    }
    pub fn as_blend_k_drag(&self) -> Option<&BlendKDrag> {
        match self { Self::BlendKDrag(d) => Some(d), _ => None }
    }
    pub fn as_clip_curve_draw(&self) -> Option<&ClipCurveDrawState> {
        match self { Self::ClipCurveDraw(s) => Some(s), _ => None }
    }
    pub fn as_clip_curve_draw_mut(&mut self) -> Option<&mut ClipCurveDrawState> {
        match self { Self::ClipCurveDraw(s) => Some(s), _ => None }
    }
    pub fn as_curve_point_transform(&self) -> Option<&CurvePointDrag> {
        match self { Self::CurvePointTransform(d) => Some(d), _ => None }
    }
    pub fn as_curve_point_transform_mut(&mut self) -> Option<&mut CurvePointDrag> {
        match self { Self::CurvePointTransform(d) => Some(d), _ => None }
    }
    pub fn as_curve_bevel(&self) -> Option<&CurveBevelDrag> {
        match self { Self::CurveBevelTransform(d) => Some(d), _ => None }
    }
    pub fn as_curve_bevel_mut(&mut self) -> Option<&mut CurveBevelDrag> {
        match self { Self::CurveBevelTransform(d) => Some(d), _ => None }
    }
    pub fn as_poly_offset_loop(&self) -> Option<&PolyOffsetLoopDrag> {
        match self { Self::PolyOffsetLoop(d) => Some(d), _ => None }
    }
    pub fn as_poly_offset_loop_mut(&mut self) -> Option<&mut PolyOffsetLoopDrag> {
        match self { Self::PolyOffsetLoop(d) => Some(d), _ => None }
    }

    // ---- Take accessors (replace self with Idle, return inner) ----
    pub fn take_face_push_pull(&mut self) -> Option<FaceDrag> {
        match std::mem::replace(self, Self::Idle) {
            Self::FacePushPull(fd) => Some(fd),
            other => { *self = other; None }
        }
    }
    pub fn take_smart_mirror(&mut self) -> Option<SmartMirrorState> {
        match std::mem::replace(self, Self::Idle) {
            Self::SmartMirror(sm) => Some(sm),
            other => { *self = other; None }
        }
    }
    pub fn take_poly_symmetry(&mut self) -> Option<PolySymmetryState> {
        match std::mem::replace(self, Self::Idle) {
            Self::PolySymmetry(s) => Some(s),
            other => { *self = other; None }
        }
    }
    pub fn take_selection_box(&mut self) -> Option<SelectionDrag> {
        match std::mem::replace(self, Self::Idle) {
            Self::SelectionBox(sd) => Some(sd),
            other => { *self = other; None }
        }
    }
    pub fn take_create(&mut self) -> Option<CreateState> {
        match std::mem::replace(self, Self::Idle) {
            Self::Create(cs) => Some(cs),
            other => { *self = other; None }
        }
    }
}

pub struct DepthPickState {
    pub candidates: Vec<u32>,          // prim_uid values, nearest first (stable across add/delete)
    pub index: usize,                  // current pick within candidates
    pub origin_mouse: egui::Pos2,      // for drift cancellation (updated each scroll)
    pub prev_usel: UnifiedSelection,   // restore on Esc
    pub scroll_accum: f32,             // accumulator for smooth scroll debouncing
}

/// Viewport light control handle (zone / softness / range rings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightHandleKind {
    /// Outer volumetric reach ring.
    Zone,
    /// Inner softness-feather ring (fraction of zone).
    Softness,
    /// Illumination range sphere (point/spot).
    Range,
}

/// Live drag of a light viewport handle (not a full ActiveTool — no modal steal).
#[derive(Clone, Debug)]
pub struct LightHandleDrag {
    pub light_id: u32,
    pub kind: LightHandleKind,
    pub start_mouse: egui::Pos2,
    /// zone / softness / range at drag start.
    pub start_value: f32,
    /// Zone at drag start (needed when dragging Softness as a world radius).
    pub start_zone: f32,
}

pub struct ToolState {
    pub current: ActiveTool,
    /// High-level work mode (Object/Sculpt/etc.) — moved from FourthLayerApp (Phase 1).
    pub work_mode: WorkMode,
    pub mode: ToolMode,
    pub gizmo_mode: GizmoMode,
    pub transform_space: TransformSpace,
    /// Modo-style Action Center (T pie) — center + axis policy for transforms.
    pub action_center: ActionCenter,
    /// For [`ActionCenter::SelectionCenterAutoAxis`]: 0=X, 1=Y, 2=Z (world).
    pub action_auto_axis: u8,
    /// Last Element pick: (world center, world orientation quat local→world).
    pub action_element: Option<(glam::Vec3, glam::Quat)>,
    /// Automatic Action Center: temporary user-placed world pivot (Shift+RMB).
    /// Lives while G/R/S is armed; **cleared when GRS is deactivated** so the default
    /// pivot is the **selection center** again (`None` = track selection median).
    /// Also cleared on Action Center → None. T-pie re-pick of Automatic keeps a place
    /// only if GRS is still armed.
    pub action_automatic_center: Option<glam::Vec3>,
    pub gizmo_hovered: Option<GizmoComponent>,
    /// For Elongate mode: true = negative face (-X/-Y/-Z), false = positive (+X/+Y/+Z).
    pub elongate_face_negative: bool,
    // Saved state before G grab
    pub pre_grab_mode: ToolMode,
    pub pre_grab_work_mode: Option<WorkMode>,
    // Hover state
    pub hovered_bevel_edge: Option<u8>,
    pub selected_bevel_edges: Vec<u8>,
    /// Hovered yellow Uniform-bevel square (box bevel mode).
    pub uniform_bevel_hovered: bool,
    pub falloff_hovered: Option<FalloffHandle>,
    /// Hovered light viewport handle (zone/softness/range) for selected lights.
    pub light_handle_hovered: Option<(u32, LightHandleKind)>,
    /// Active light-handle drag (zone/softness/range).
    pub light_handle_drag: Option<LightHandleDrag>,
    pub blend_k_hovered: bool,
    /// When Some, a boolean type popup is open for this target (right-click on diamond).
    pub blend_k_popup: Option<BlendKTarget>,
    pub hovered_face: Option<(usize, glam::Vec3)>,  // (prim_idx, local_normal)
    pub hovered_prim: Option<usize>,
    /// Suppresses gizmo drag start for one frame after click-to-select,
    /// preventing accidental grab when clicking near object center.
    pub suppress_gizmo_drag: bool,
    /// Gizmo is armed (visible, waiting for user to click an axis).
    /// Set by G/S/R press. Cleared on axis click, Esc, or Space.
    pub gizmo_armed: bool,
    /// The body a persistent per-object mode-tool (Bevel/Extrude) belongs to.
    /// Set when entering the mode; a per-frame reconciler drops the mode-tool
    /// when the active body switches away from this (DCC "switch object → drop
    /// tool" behaviour). `None` when no body-scoped mode-tool is active.
    /// `NodeId` is `Copy`; stored as its raw value would lose the type, so we
    /// keep the typed id.
    pub mode_body_nid: Option<core_math::scene_graph::NodeId>,
    /// True from a curve point-editor press that consumed the LMB as a pure selection
    /// change (shift-toggle — no tool started) until the primary button releases.
    /// Suppresses the viewport SelectionBox drag-start AND the click-select fallback
    /// for that same press: egui classifies a jittery click as a drag, and either path
    /// would re-run object selection with shift and toggle the whole prim out —
    /// clearing the point multi-selection ("shift can't select 2 points" bug).
    pub curve_press_hold: bool,
    /// Object-mode rectangular select was started with RMB drag (not LMB).
    /// Finish/cancel on Secondary release; plain RMB click still opens context menu.
    /// Needed because finish was gated on `!primary_down`, which is always true during an
    /// RMB-only drag and would collapse the rubber-band on the arm frame.
    pub selection_box_secondary: bool,
    /// Shift was held when SelectionBox armed (Shift+RMB = deselect marquee).
    pub selection_box_subtract: bool,
    /// An RMB PRESS already confirmed a modal tool (loop cut) this gesture, but egui's `clicked`
    /// only fires on the RELEASE frame — after the confirm has reset the tool state, so the
    /// per-frame `rmb_consumed` local can no longer see it. This flag carries the "swallow the
    /// upcoming RMB click" intent across frames so the context menu never opens on top of the
    /// accept. Cleared on the next fresh RMB press or when the click is swallowed.
    pub rmb_swallow_click: bool,
    /// Shift+D array session: gizmo-driven duplicate with scroll array count.
    /// Held until Enter (confirm copies), Esc (cancel), or M (Array modifier).
    pub array_dup: Option<ArrayDupSession>,
    // BoolCut tool state
    pub bool_cut: Option<BoolCutState>,
    // Depth scroll pick
    pub depth_pick: Option<DepthPickState>,
    /// Accumulated scroll for Alt+Scroll body cycling (threshold-based stepping)
    pub body_cycle_scroll_accum: f32,
    /// Accumulated scroll for SHIFT+Scroll edge-loop selection walk (threshold-based stepping)
    pub loop_walk_scroll_accum: f32,
    // Create tool settings
    pub create_op: CreateOp,
    pub create_blend_k: f32,
    pub create_tool_settings: CreateToolSettings,
    /// Extract Intersection (X pie → center → special commands) options.
    pub extract_intersection: ExtractIntersectionSettings,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            current: ActiveTool::Idle,
            work_mode: WorkMode::Object,
            mode: ToolMode::Select,
            gizmo_mode: GizmoMode::Translate,
            transform_space: TransformSpace::Global,
            action_center: ActionCenter::None,
            action_auto_axis: 1, // Y
            action_element: None,
            action_automatic_center: None,
            gizmo_hovered: None,
            elongate_face_negative: false,
            pre_grab_mode: ToolMode::Select,
            pre_grab_work_mode: None,
            hovered_bevel_edge: None, selected_bevel_edges: vec![],
            uniform_bevel_hovered: false,
            falloff_hovered: None,
            light_handle_hovered: None,
            light_handle_drag: None,
            blend_k_hovered: false, blend_k_popup: None, hovered_face: None, hovered_prim: None, suppress_gizmo_drag: false, gizmo_armed: false,
            mode_body_nid: None, curve_press_hold: false,
            selection_box_secondary: false, selection_box_subtract: false,
            rmb_swallow_click: false,
            array_dup: None,
            bool_cut: None, depth_pick: None, body_cycle_scroll_accum: 0.0, loop_walk_scroll_accum: 0.0,
            create_op: CreateOp::Union, create_blend_k: 0.0,
            create_tool_settings: CreateToolSettings::default(),
            extract_intersection: ExtractIntersectionSettings::default(),
        }
    }
}
