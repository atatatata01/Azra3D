// app_types/src/keymap/ctx.rs — the context model (INPUT_ARCHITECTURE_PLAN §3.1, §3.3′).
//
// S1 of the gated migration: TYPES ONLY, zero callers. Nothing here is wired into dispatch yet —
// see §8. It exists so later slices (S2+) have the vocabulary. The exhaustive `ToolId::of()` forces
// every `ActiveTool` variant to be classified (structural fix for the missing-ladder-arm class).

use crate::ActiveTool;

// ── Axis A: SURFACE — PUSHED by the panel that drew it, never guessed from screen fractions. ──
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Surface { Unknown = 0, Viewport, Outliner, Properties, Library, ShaderGraph }

/// The pies, for `ToolId::Pie` (used from S9). A pie IS a modal tool.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum PieId { Shading = 0, Mode, View, Reset, F, D, Bool, SdfBool, Symmetry }

// ── Axis B: TOOL — data-only discriminant of ActiveTool's payload-carrying variants. ──
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ToolId {
    Idle, Transform, Gizmo, FacePushPull, Bevel, BoolEdgeBevel, FalloffAdjust,
    Create, SmartMirror, PivotEdit, SelectionBox,
    PolyExtrude, PolyEdgeExtrude, PolyInset, PolyBoxSelect, PolyCircleSelect, PolyPaintSelect,
    PolyRegionSelect,
    PolyTransform, PolyBevel, PolyLoopCutSlide, PolyVertSlide, PolyEdgeSlide, PolyFaceHingeRotate,
    PolyShrinkFatten,
    NurbsCvMove, BlendKDrag, ClipCurveDraw, CurvePointTransform, CurveBevelTransform,
    SpinePointEdit,
    Pie(PieId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Capture { Exclusive, Additive }

impl ToolId {
    /// Bit index for `ToolMask`. `Pie(_)` collapses to ONE bit — which pie is a `Ctx` detail, not a
    /// mask axis. 29 indices fit u32.
    pub const fn idx(self) -> u32 {
        match self {
            ToolId::Idle => 0, ToolId::Transform => 1, ToolId::Gizmo => 2, ToolId::FacePushPull => 3,
            ToolId::Bevel => 4, ToolId::BoolEdgeBevel => 5, ToolId::FalloffAdjust => 6,
            ToolId::Create => 7, ToolId::SmartMirror => 8, ToolId::PivotEdit => 9, ToolId::SelectionBox => 10,
            ToolId::PolyExtrude => 11, ToolId::PolyEdgeExtrude => 12, ToolId::PolyInset => 13,
            ToolId::PolyBoxSelect => 14, ToolId::PolyCircleSelect => 15, ToolId::PolyPaintSelect => 16,
            ToolId::PolyRegionSelect => 17, ToolId::PolyTransform => 18, ToolId::PolyBevel => 19,
            ToolId::PolyLoopCutSlide => 20, ToolId::PolyVertSlide => 21, ToolId::PolyEdgeSlide => 22,
            ToolId::PolyFaceHingeRotate => 23, ToolId::PolyShrinkFatten => 24, ToolId::NurbsCvMove => 25,
            ToolId::BlendKDrag => 26, ToolId::ClipCurveDraw => 27, ToolId::CurvePointTransform => 28,
            ToolId::CurveBevelTransform => 29, ToolId::SpinePointEdit => 30, ToolId::Pie(_) => 31,
        }
    }

    /// EXHAUSTIVE — no `_` arm. A new ActiveTool variant = COMPILE ERROR here (the point).
    pub fn of(t: &ActiveTool) -> Self {
        match t {
            ActiveTool::Idle => ToolId::Idle,
            ActiveTool::Transform(_) => ToolId::Transform,
            ActiveTool::Gizmo(_) => ToolId::Gizmo,
            ActiveTool::FacePushPull(_) => ToolId::FacePushPull,
            ActiveTool::Bevel(_) => ToolId::Bevel,
            ActiveTool::BoolEdgeBevel(_) => ToolId::BoolEdgeBevel,
            ActiveTool::FalloffAdjust(_) => ToolId::FalloffAdjust,
            ActiveTool::Create(_) => ToolId::Create,
            ActiveTool::SmartMirror(_) => ToolId::SmartMirror,
            // Unused hold-tool leftover; poly Alt+X uses the sym pie, not this tool.
            ActiveTool::PolySymmetry(_) => ToolId::SmartMirror,
            ActiveTool::PivotEdit(_) => ToolId::PivotEdit,
            ActiveTool::SelectionBox(_) => ToolId::SelectionBox,
            ActiveTool::PolyExtrude(_) => ToolId::PolyExtrude,
            // Same exclusive modal capture family as PolyExtrude (no free ToolId bit).
            ActiveTool::PolyExtrudeSweep(_) => ToolId::PolyExtrude,
            ActiveTool::PolyEdgeExtrude(_) => ToolId::PolyEdgeExtrude,
            ActiveTool::PolyInset(_) => ToolId::PolyInset,
            ActiveTool::PolyBoxSelect(_) => ToolId::PolyBoxSelect,
            ActiveTool::PolyCircleSelect(_) => ToolId::PolyCircleSelect,
            ActiveTool::PolyPaintSelect(_) => ToolId::PolyPaintSelect,
            ActiveTool::PolyRegionSelect(_) => ToolId::PolyRegionSelect,
            ActiveTool::PolyTransform(_) => ToolId::PolyTransform,
            ActiveTool::PolyBevel(_) => ToolId::PolyBevel,
            ActiveTool::PolyLoopCutSlide(_) => ToolId::PolyLoopCutSlide,
            ActiveTool::PolyVertSlide(_) => ToolId::PolyVertSlide,
            ActiveTool::PolyEdgeSlide(_) => ToolId::PolyEdgeSlide,
            ActiveTool::PolyFaceHingeRotate(_) => ToolId::PolyFaceHingeRotate,
            ActiveTool::PolyShrinkFatten(_) => ToolId::PolyShrinkFatten,
            ActiveTool::NurbsCvMove(_) => ToolId::NurbsCvMove,
            // Exclusive modal like poly bevel (no free ToolId bit — map to PolyBevel capture).
            ActiveTool::NurbsFillet(_) => ToolId::PolyBevel,
            ActiveTool::BlendKDrag(_) => ToolId::BlendKDrag,
            // Proportional-origin diamond: the same shape of interaction as the blend-k
            // diamond — a pointer-held viewport handle that must NOT swallow keys
            // (`Capture::Additive`). `bit()` is a 32-bit mask with 31 as its last free
            // slot, so there is no bit left for a private id; mapping to the sibling with
            // identical capture is the established fallback (PolyExtrudeSweep, NurbsFillet).
            ActiveTool::PolyPropOrigin(_) => ToolId::BlendKDrag,
            ActiveTool::ClipCurveDraw(_) => ToolId::ClipCurveDraw,
            ActiveTool::CurvePointTransform(_) => ToolId::CurvePointTransform,
            ActiveTool::CurveBevelTransform(_) => ToolId::CurveBevelTransform,
            ActiveTool::SpinePointEdit { .. } => ToolId::SpinePointEdit,
            // Offset Edge Loop (`I` in Edge mode): an EXCLUSIVE modal drag with exactly the poly
            // bevel's capture — arm, move the mouse to set an absolute width, LMB/Enter confirm,
            // Esc/RMB cancel. `idx()` is a 32-bit mask with every slot taken (31 = Pie), so it maps
            // to the sibling with identical capture, the established fallback here
            // (PolyExtrudeSweep → PolyExtrude, NurbsFillet → PolyBevel, PolyPropOrigin → BlendKDrag).
            ActiveTool::PolyOffsetLoop(_) => ToolId::PolyBevel,
            // Dice: exclusive modal (drag segs / R rotate / Enter confirm) — same capture as loop-cut.
            ActiveTool::PolyDice(_) => ToolId::PolyLoopCutSlide,
            // Edge Weight (`W`): exclusive modal drag with EXACTLY Shrink/Fatten's capture — armed by
            // a key, mouse drag sets one scalar, LMB/Enter/RMB confirm, Esc cancels. `idx()` is a
            // 32-bit mask with every slot taken (31 = Pie), so it maps to the sibling with identical
            // capture — the established fallback here (PolyExtrudeSweep → PolyExtrude, NurbsFillet →
            // PolyBevel, PolyOffsetLoop → PolyBevel, PolyDice → PolyLoopCutSlide).
            ActiveTool::PolyEdgeWeight(_) => ToolId::PolyShrinkFatten,
        }
    }

    /// EXHAUSTIVE — a new tool must declare its capture or it will not compile. A `Capture::Exclusive`
    /// tool swallows unmatched keys (modal), mirroring a Blender modal operator's RUNNING_MODAL.
    pub const fn capture(self) -> Capture {
        match self {
            ToolId::Transform | ToolId::Create | ToolId::SmartMirror | ToolId::ClipCurveDraw
            | ToolId::PolyExtrude | ToolId::PolyEdgeExtrude | ToolId::PolyInset | ToolId::PolyBevel
            | ToolId::PolyLoopCutSlide
            | ToolId::PolyVertSlide | ToolId::PolyEdgeSlide | ToolId::PolyFaceHingeRotate
            | ToolId::PolyShrinkFatten
            | ToolId::CurvePointTransform | ToolId::CurveBevelTransform | ToolId::PolyTransform
            | ToolId::SpinePointEdit
            | ToolId::Pie(_) => Capture::Exclusive,
            ToolId::Idle | ToolId::Gizmo | ToolId::FacePushPull | ToolId::Bevel | ToolId::BoolEdgeBevel
            | ToolId::FalloffAdjust | ToolId::PivotEdit | ToolId::SelectionBox | ToolId::PolyBoxSelect
            | ToolId::PolyCircleSelect | ToolId::PolyPaintSelect | ToolId::PolyRegionSelect
            | ToolId::NurbsCvMove | ToolId::BlendKDrag => Capture::Additive,
        }
    }
}

// ── Axis C: MODE — three independent sub-fields, matched conjunctively (never one flattened enum). ──
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Pillar { None = 0, Sdf, Polygonal, Voxel, Nurbs, Curve, Light, Camera, ImportedMesh }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum WorkKind { Object = 0, Edit, Sculpt }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum ElemMode { NA = 0, Vertex, Edge, Face }

/// Live modifier snapshot (for qualifier level-sampling, §5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Mods { pub ctrl: bool, pub shift: bool, pub alt: bool, pub cmd: bool }

// ── Masks — hand-rolled, no bitflags dep. ANY = all bits; `is_any()` is true only for the ANY const. ──
macro_rules! mask {
    ($name:ident, $elem:ty, $repr:ty) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub struct $name(pub $repr);
        impl $name {
            pub const ANY: Self = Self(<$repr>::MAX);
            pub const NONE: Self = Self(0);
            #[inline] pub const fn only(e: $elem) -> Self { Self((1u64 << (e as u32)) as $repr) }
            #[inline] pub const fn or(self, e: $elem) -> Self { Self(self.0 | ((1u64 << (e as u32)) as $repr)) }
            #[inline] pub const fn has(self, e: $elem) -> bool { self.0 & ((1u64 << (e as u32)) as $repr) != 0 }
            #[inline] pub const fn intersects(self, o: Self) -> bool { self.0 & o.0 != 0 }
            #[inline] pub const fn is_any(self) -> bool { self.0 == <$repr>::MAX }
        }
    };
}
mask!(SurfaceMask, Surface, u16);
mask!(PillarMask, Pillar, u16);
mask!(WorkMask, WorkKind, u8);
mask!(ElemMask, ElemMode, u8);

// ToolMask is hand-written because ToolId carries a payload (Pie), so it can't be `as`-cast.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ToolMask(pub u32);
impl ToolMask {
    pub const ANY: Self = Self(u32::MAX);
    pub const NONE: Self = Self(0);
    #[inline] pub const fn only(e: ToolId) -> Self { Self(1u32 << e.idx()) }
    #[inline] pub const fn or(self, e: ToolId) -> Self { Self(self.0 | (1u32 << e.idx())) }
    #[inline] pub const fn has(self, e: ToolId) -> bool { self.0 & (1u32 << e.idx()) != 0 }
    #[inline] pub const fn intersects(self, o: Self) -> bool { self.0 & o.0 != 0 }
    #[inline] pub const fn is_any(self) -> bool { self.0 == u32::MAX }
}

/// THE composite predicate — three axes composed because they are independent fields of ONE struct,
/// matched conjunctively.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct When {
    pub surface: SurfaceMask,
    pub tool: ToolMask,
    pub pillar: PillarMask,
    pub work: WorkMask,
    pub elem: ElemMask,
}

impl When {
    /// == today's Global, and == the actions with `context: None`. `None` now means ANY (correct)
    /// instead of "conflicts with everything" (the old free-text bug).
    pub const ANY: Self = Self {
        surface: SurfaceMask::ANY, tool: ToolMask::ANY,
        pillar: PillarMask::ANY, work: WorkMask::ANY, elem: ElemMask::ANY,
    };

    // ── const builders (used by the S2 legacy prose→When mapping in decl.rs) ──
    pub const fn only_tool(t: ToolId) -> Self {
        Self { surface: SurfaceMask::ANY, tool: ToolMask::only(t), pillar: PillarMask::ANY, work: WorkMask::ANY, elem: ElemMask::ANY }
    }
    pub const fn only_pillar(p: Pillar) -> Self {
        Self { surface: SurfaceMask::ANY, tool: ToolMask::ANY, pillar: PillarMask::only(p), work: WorkMask::ANY, elem: ElemMask::ANY }
    }
    pub const fn only_work(w: WorkKind) -> Self {
        Self { surface: SurfaceMask::ANY, tool: ToolMask::ANY, pillar: PillarMask::ANY, work: WorkMask::only(w), elem: ElemMask::ANY }
    }
    pub const fn only_surface(s: Surface) -> Self {
        Self { surface: SurfaceMask::only(s), tool: ToolMask::ANY, pillar: PillarMask::ANY, work: WorkMask::ANY, elem: ElemMask::ANY }
    }
    pub const fn poly_edit(elem: ElemMask) -> Self {
        Self { surface: SurfaceMask::ANY, tool: ToolMask::ANY, pillar: PillarMask::only(Pillar::Polygonal), work: WorkMask::only(WorkKind::Edit), elem }
    }
    pub const fn pillar_edit(p: Pillar) -> Self {
        Self { surface: SurfaceMask::ANY, tool: ToolMask::ANY, pillar: PillarMask::only(p), work: WorkMask::only(WorkKind::Edit), elem: ElemMask::ANY }
    }

    /// Conjunctive match against a live context.
    #[inline]
    pub fn accepts(&self, c: &Ctx) -> bool {
        self.surface.has(c.surface) && self.tool.has(c.tool)
            && self.pillar.has(c.pillar) && self.work.has(c.work) && self.elem.has(c.elem)
    }

    /// Can two predicates be satisfied simultaneously? Pure arithmetic — the sound conflict test.
    #[inline]
    pub fn overlaps(&self, o: &Self) -> bool {
        self.surface.intersects(o.surface) && self.tool.intersects(o.tool)
            && self.pillar.intersects(o.pillar) && self.work.intersects(o.work)
            && self.elem.intersects(o.elem)
    }
}

/// The live snapshot — the ONE place the mode fields are read (built by `build_ctx` from S4).
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub surface: Surface,
    pub tool: ToolId,
    pub tool_modal: Option<ToolId>, // Some iff a modal drag owns the keyboard
    pub pillar: Pillar,
    pub work: WorkKind,
    pub elem: ElemMode,
    pub mods: Mods,
    pub text_focus: bool,
}

impl When {
    /// Short human label of the constrained axes, for the rebind editor. Empty for ANY (Global).
    pub fn describe(&self) -> String {
        let mut p: Vec<&str> = Vec::new();
        if !self.pillar.is_any() {
            for (m, s) in [(Pillar::Sdf, "SDF"), (Pillar::Polygonal, "Poly"), (Pillar::Voxel, "Voxel"),
                           (Pillar::Nurbs, "NURBS"), (Pillar::Curve, "Curve"), (Pillar::Light, "Light"),
                           (Pillar::Camera, "Camera"), (Pillar::ImportedMesh, "Mesh"), (Pillar::None, "None")] {
                if self.pillar.has(m) { p.push(s); }
            }
        }
        if !self.work.is_any() {
            for (m, s) in [(WorkKind::Object, "Object"), (WorkKind::Edit, "Edit"), (WorkKind::Sculpt, "Sculpt")] {
                if self.work.has(m) { p.push(s); }
            }
        }
        if !self.elem.is_any() {
            for (m, s) in [(ElemMode::Vertex, "Vert"), (ElemMode::Edge, "Edge"), (ElemMode::Face, "Face"), (ElemMode::NA, "NA")] {
                if self.elem.has(m) { p.push(s); }
            }
        }
        if !self.tool.is_any() { p.push("tool"); }
        if !self.surface.is_any() { p.push("panel"); }
        p.join(" ")
    }
}

/// Layer is DERIVED from the When, never declared (no second source of truth). No `Modal` layer —
/// modal is handled by `Capture` gating the scan (§3.4), which keeps this derivation sound.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Layer { Tool, Mode, Surface, Global }

pub fn layer_of(w: &When) -> Layer {
    if !w.tool.is_any() { Layer::Tool }
    else if !w.pillar.is_any() || !w.work.is_any() || !w.elem.is_any() { Layer::Mode }
    else if !w.surface.is_any() { Layer::Surface }
    else { Layer::Global }
}
