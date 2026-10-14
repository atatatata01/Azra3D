// core_engine/core_math/src/brush/mod.rs
//
// COMPOSABLE VOXEL BRUSH — data model (Phase 0 of BRUSH_SYSTEM_ARCHITECTURE.md).
//
// This module introduces the `BrushRecipe` -> `StampPlan` data pipeline BEHIND today's
// `VoxelBrush` enum, with ZERO behaviour change. A brush is a fixed-shape struct of typed
// slots (Kernel, Direction, Channels, OperatorStack, Modulators); the CPU folds a recipe +
// a surface hit into ONE flat POD per stamp (`StampPlan`, see `plan.rs`) which is the only
// thing the apply engine (and, in a later phase, the GPU) consumes.
//
// PURE DATA. No GPU, no engine references — `core_math` has no wgpu/app deps, so the data
// model is unit-tested independent of the renderer (see the golden parity test in
// `voxel_sculpt.rs` tests). Field order = evaluation order: the contract the CPU and GPU
// share (§1 of the architecture doc).
//
// PHASE 0 SCOPE (deliberate simplifications, all faithful to the POD contract — see the
// task report for what is deferred to Phase 1+):
//   * The operator stack is ALWAYS a single op (`op_count == 1`). It is stored as a
//     fixed-size `[OpEntry; MAX_OPS]` + `op_count`, NOT a `SmallVec`. This matches the
//     `StampPlan.op_list` POD shape exactly and avoids a new dependency; the `SmallVec`
//     heap-free small-stack is an implementation detail that lands with the op-stack UI
//     (Phase 3). The data contract (a bounded, ordered op list) is identical.
//   * `ChannelMask` is a plain bit-constant newtype (not the `bitflags!` macro) to keep
//     Phase 0 dependency-free; the bit values (DENSITY=1, COLOR=2, ...) are the load-bearing
//     part and are identical to the spec.
//   * Kernel/Direction/Operator carry richer variants than Phase 0 exercises; only the
//     subset reached by the 12 built-in mappings (§2) is on the hot path here.

use glam::Vec3;
use serde::{Deserialize, Serialize};

pub mod plan;
pub mod eval;

pub use plan::{InflateDirection, PackedOp, StampPlan, MAX_OPS};
pub use eval::{fold_recipe, SurfaceHit, StrokeMods};

/// Stable per-brush id (presets, undo labels, scene refs — §5). Built-ins use small fixed
/// ids; user brushes get a random one. Phase 0 only needs it on the recipe struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct BrushUid(pub u64);

// ---------------------------------------------------------------------------------------
// SLOT 1 — KERNEL (radial weight curve w(t), t = dist/radius ∈ [0,1))
// ---------------------------------------------------------------------------------------

/// The falloff axis. Today's single `hardness` knob (`falloff_weight`) is the `Smooth`
/// kernel; Crease's cubed falloff and ClayBuildup's squared falloff become `Power{exp}`.
///
/// IMPORTANT (Phase 0 parity): the discriminants are the frozen CPU↔GPU op contract and are
/// append-only (§5). Today's apply math is reproduced EXACTLY by carrying the legacy
/// `hardness` in `KernelParams.hardness` and routing through `falloff_weight` — the `Power`
/// exponent is applied ON TOP of that base falloff, matching the legacy `fall*fall*fall`
/// (Crease) and `fall*fall` (ClayBuildup) arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum Kernel {
    /// `falloff_weight(1-t, hardness)` — today's default radial curve.
    Smooth = 0,
    /// `1 - t` (reserved; not reached by a Phase-0 built-in).
    Linear = 1,
    /// `1.0` hard edge (reserved).
    Constant = 2,
    /// `sqrt(1-t²)` rounded cap (reserved).
    Sphere = 3,
    /// `falloff_weight(1-t, hardness)^exp` — `exp = 2` == ClayBuildup, `exp = 3` == Crease.
    Power = 4,
}

/// Kernel parameters kept FLAT (POD-friendly): the `Power` exponent and the legacy hardness
/// knob both ride here so the fold can pack them into the plan without an enum payload.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct KernelParams {
    /// `Power` exponent (1.0 for non-Power kernels — a no-op so the base falloff passes through).
    pub exp: f32,
    /// Continuous radial hardness (0 = soft round, 1 = sharp core). Feeds `falloff_weight` when
    /// `falloff_preset` is Blend (F1a).
    pub hardness: f32,
    /// `FalloffPreset` as u32 (see `voxel_sculpt::FalloffPreset`). Packed into `StampPlan._pad0`.
    #[serde(default)]
    pub falloff_preset: u32,
}

impl Default for KernelParams {
    fn default() -> Self {
        // Blend (8) keeps hardness-aware F1a for golden parity / legacy apply_brush paths.
        KernelParams { exp: 1.0, hardness: 0.0, falloff_preset: 8 }
    }
}

// ---------------------------------------------------------------------------------------
// SLOT 2 — DIRECTION (plane normal / advection axis; resolved to a vec3 CPU-side)
// ---------------------------------------------------------------------------------------

/// How the per-stamp deposit direction (the normal fed to plane/normal-relative brushes) is
/// SOURCED. This is the recipe-side mirror of `voxel_sculpt::BrushDirection`; the producer in
/// `input_sculpt` already resolves the surface/view/first-point/axis vectors, so the fold only
/// needs to record the resulting concrete vec3 — the GPU stays camera-agnostic (zero branches).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u32)]
pub enum Direction {
    /// Live surface normal at the contact point (today's behaviour / default).
    #[default]
    SurfaceNormal = 0,
    /// Toward the camera (view direction).
    ViewDirection = 1,
    /// Surface normal latched at stroke start (steady plane).
    FirstPoint = 2,
    AxisX = 3,
    AxisY = 4,
    AxisZ = 5,
}

// ---------------------------------------------------------------------------------------
// SLOT 3 — CHANNELS (which fields this brush writes — the "N channels")
// ---------------------------------------------------------------------------------------

/// Bit mask of the fields a stamp writes. Plain newtype (Phase-0 dependency-free); the bit
/// values are the load-bearing contract and match the spec's `bitflags!`. `chan_mask` in the
/// POD is exactly `ChannelMask::bits()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChannelMask(pub u32);

impl ChannelMask {
    pub const DENSITY: ChannelMask = ChannelMask(1);
    pub const COLOR: ChannelMask = ChannelMask(2);
    pub const ROUGHNESS: ChannelMask = ChannelMask(4);
    pub const DISPLACE: ChannelMask = ChannelMask(8);

    #[inline]
    pub const fn empty() -> Self { ChannelMask(0) }
    #[inline]
    pub const fn bits(self) -> u32 { self.0 }
    #[inline]
    pub const fn from_bits_truncate(bits: u32) -> Self { ChannelMask(bits & 0b1111) }
    #[inline]
    pub const fn contains(self, other: ChannelMask) -> bool { (self.0 & other.0) == other.0 }
    #[inline]
    pub fn insert(&mut self, other: ChannelMask) { self.0 |= other.0; }
    #[inline]
    pub fn remove(&mut self, other: ChannelMask) { self.0 &= !other.0; }
}

impl std::ops::BitOr for ChannelMask {
    type Output = ChannelMask;
    #[inline]
    fn bitor(self, rhs: ChannelMask) -> ChannelMask { ChannelMask(self.0 | rhs.0) }
}

/// Multi-field stamp: sculpt AND paint in one stamp. Phase 0 only writes DENSITY through the
/// apply engine (paint colour keeps its existing `deposit_color_sphere` plumbing — see §6
/// Phase 0: "Paint/pressure/Move keep their current plumbing"), but COLOR is carried so the
/// recipe losslessly describes the in-flight paint feature.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelSet {
    pub mask: ChannelMask,
    /// Packed RGBA8 when COLOR is set (the tool's paint colour, `pack_voxel_color` format).
    pub color: u32,
    pub roughness: f32,
    /// Per-channel weight (1.0 default; 0.0 == pure paint, no density write).
    pub density_scale: f32,
    pub color_scale: f32,
}

impl Default for ChannelSet {
    fn default() -> Self {
        ChannelSet {
            mask: ChannelMask::DENSITY,
            color: 0,
            roughness: 0.0,
            density_scale: 1.0,
            color_scale: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------------------
// SLOT 4 — OPERATOR STACK / OpEntry (the ONE ordered axis; Phase 0 is always 1 op)
// ---------------------------------------------------------------------------------------

/// The density operator family — the frozen CPU↔GPU `op_code` contract. APPEND-ONLY: these
/// discriminants are the GPU switch arms AND saved-recipe tags (§5). Phase 0 exercises every
/// variant via the 12 built-in mappings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum Operator {
    /// Draw — `cur + sgn*w`.
    Add = 0,
    /// Erase / Crease — `cur - sgn*w` (Crease pairs it with `Kernel::Power{3}`).
    Subtract = 1,
    /// Smooth — relax toward the 6-neighbour average (reads pre-stamp snapshot).
    Relax = 2,
    /// Flatten — push toward the plane, bidirectional.
    PlanePush = 3,
    /// Clay / Fill / ClayBuildup — deposit on `side`, optional shave above.
    PlaneFill = 4,
    /// Scrape — subtract on `side`.
    PlaneClip = 5,
    /// Inflate — gradient puff (reads pre-stamp snapshot).
    Inflate = 6,
    /// Pinch (tangential) / Move (drag-vector) — semi-Lagrangian gather (see `AdvectSrc`).
    Advect = 7,
}

/// Which half of the contact plane an op acts on. Folds Scrape and Fill into one `PlaneClip`/
/// `PlaneFill` family by data, not code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum PlaneSide {
    Both = 0,
    Above = 1,
    Below = 2,
}

/// How an op's delta combines with the prior op's output (Phase 0 is single-op, so always
/// `Accumulate`). Carried for the Phase-3 op stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum OpBlend {
    Accumulate = 0,
    Max = 1,
    Min = 2,
    Replace = 3,
}

/// Advection source for the `Advect` operator: tangential pull (Pinch) vs drag-vector grab
/// (Move). Packed into `PackedOp.flags` so the POD stays flat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum AdvectSrc {
    Tangential = 0,
    DragVector = 1,
}

/// One operator in the stack. Phase 0 always uses a single entry.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpEntry {
    pub op: Operator,
    /// Per-op strength multiplier (default 1.0).
    pub amount: f32,
    /// Plane offset ×radius (Clay/ClayBuildup = +0.10, the legacy hardcoded value).
    pub plane_offset: f32,
    /// Which side of the contact plane the op acts on.
    pub side: PlaneSide,
    /// How this op's delta combines with prior op output.
    pub blend: OpBlend,
    /// `PlaneFill` shave flag: shave material above the plane (Clay = true, Fill/ClayBuildup = false).
    pub shave: bool,
    /// `Advect` source (Tangential = Pinch, DragVector = Move).
    pub advect: AdvectSrc,
    /// `PlaneFill` PHASE-0 sign mode: when true the invert `sgn` flips WHICH SIDE fills (legacy Fill
    /// semantics, deposit fixed `+w`, strict boundary); when false the `sgn` flips the DEPOSIT sign
    /// (legacy Clay/ClayBuildup, inclusive boundary). See `plan::flags::SGN_ON_SIDE_BIT`.
    pub sgn_on_side: bool,
    /// `PlaneFill` CONVERGING fill (Clay-1, ClayBuildup only): instead of an unbounded additive
    /// deposit, fill the density field TOWARD the offset plane and STOP once it reaches it — so clay
    /// packs evenly and repeated passes do not spike past the surface (Blender Clay Strips behaviour;
    /// see docs/planning/sculpt/SCULPT_FEEL_LOG.md F-Clay). See `plan::flags::CONVERGE_BIT`.
    pub converge: bool,
}

impl Default for OpEntry {
    fn default() -> Self {
        OpEntry {
            op: Operator::Add,
            amount: 1.0,
            plane_offset: 0.0,
            side: PlaneSide::Both,
            blend: OpBlend::Accumulate,
            shave: false,
            advect: AdvectSrc::Tangential,
            sgn_on_side: false,
            converge: false,
        }
    }
}

// ---------------------------------------------------------------------------------------
// SLOT 5 — MODULATORS (pre-dispatch folds; never touch a voxel)
// ---------------------------------------------------------------------------------------

/// How tablet pressure routes into the per-stamp scalars. Phase 0 mirrors the existing global
/// pressure toggles (`pressure_to_strength` / `pressure_to_radius`); the producer already
/// applies `strength * pt.pressure`, so the recipe records the route for the future per-brush UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u32)]
pub enum PressureRoute {
    #[default]
    None = 0,
    Strength = 1,
    Radius = 2,
    Both = 3,
}

/// Pre-dispatch modulators. Phase 0 carries these for losslessness; the fold applies the
/// pressure route, and symmetry/spacing/lazy stay in the producer's existing plumbing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Modulators {
    pub pressure: PressureRoute,
    /// X|Y|Z mirror planes (existing symmetry).
    pub symmetry: [bool; 3],
    /// Stroke resample spacing (×radius).
    pub spacing: f32,
    /// Lazy-mouse rope smoothing 0..1.
    pub lazy: f32,
    /// Ctrl flips the brush sign (the existing `invert`).
    pub allow_invert: bool,
}

impl Default for Modulators {
    fn default() -> Self {
        Modulators {
            pressure: PressureRoute::None,
            symmetry: [false; 3],
            spacing: 0.25,
            lazy: 0.0,
            allow_invert: true,
        }
    }
}

// ---------------------------------------------------------------------------------------
// The recipe
// ---------------------------------------------------------------------------------------

/// A composed brush. Fixed typed slots interpreted by the fixed apply engine. Every shipped
/// `VoxelBrush` is a built-in recipe with a single-entry op stack (`from_builtin`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushRecipe {
    pub uid: BrushUid,
    pub name: String,
    pub builtin: bool,
    pub version: u16,

    // SLOT 1
    pub kernel: Kernel,
    pub kernel_params: KernelParams,
    // SLOT 2
    pub direction: Direction,
    // SLOT 3
    pub channels: ChannelSet,
    // SLOT 4 — Phase 0: fixed array + count, always op_count == 1.
    pub ops: [OpEntry; MAX_OPS],
    pub op_count: u32,
    // SLOT 5
    pub modulators: Modulators,

    // Base scalars (today's two sliders).
    pub base_radius: f32,
    pub base_strength: f32,
}

impl BrushRecipe {
    /// The single op of a Phase-0 (single-op) recipe.
    #[inline]
    pub fn op0(&self) -> &OpEntry { &self.ops[0] }

    /// Build the recipe for a shipped built-in brush, folding in the already-shipped tool state
    /// (the `hardness`/falloff f32, `brush_dir`, `paint_color`/`paint_color_on`, pressure). The
    /// 12 mappings of §2. ALWAYS a single op (`op_count == 1`).
    ///
    /// `brush`         — the active built-in brush.
    /// `radius`,`strength` — today's two sliders (the base scalars).
    /// `hardness`      — the continuous falloff knob (`v.falloff`), → `KernelParams.hardness`.
    /// `brush_dir`     — the deposit-direction tool state, → `Direction`.
    /// `paint_color`   — packed tool paint colour (`pack_voxel_color` format).
    /// `paint_color_on`— whether the COLOR channel is active this stamp.
    /// `pressure_to_strength`,`pressure_to_radius` — the global pressure toggles, → `PressureRoute`.
    /// `symmetry`,`spacing`,`lazy`,`allow_invert` — the remaining modulators (carried, not yet
    ///   consumed by the fold beyond pressure — they stay in the producer's plumbing in Phase 0).
    #[allow(clippy::too_many_arguments)]
    pub fn from_builtin(
        brush: BuiltinBrush,
        radius: f32,
        strength: f32,
        hardness: f32,
        brush_dir: Direction,
        paint_color: u32,
        paint_color_on: bool,
        pressure_to_strength: bool,
        pressure_to_radius: bool,
        symmetry: [bool; 3],
        spacing: f32,
        lazy: f32,
        allow_invert: bool,
    ) -> BrushRecipe {
        // Default Blend (8) preserves hardness-aware F1a for callers that don't pass a preset.
        Self::from_builtin_ex(
            brush, radius, strength, hardness, 8, brush_dir, paint_color, paint_color_on,
            pressure_to_strength, pressure_to_radius, symmetry, spacing, lazy, allow_invert,
        )
    }

    /// Like [`from_builtin`] with an explicit falloff preset id (`FalloffPreset::as_u32`).
    #[allow(clippy::too_many_arguments)]
    pub fn from_builtin_ex(
        brush: BuiltinBrush,
        radius: f32,
        strength: f32,
        hardness: f32,
        falloff_preset: u32,
        brush_dir: Direction,
        paint_color: u32,
        paint_color_on: bool,
        pressure_to_strength: bool,
        pressure_to_radius: bool,
        symmetry: [bool; 3],
        spacing: f32,
        lazy: f32,
        allow_invert: bool,
    ) -> BrushRecipe {
        use BuiltinBrush as B;

        // SLOT 1 — kernel: Crease cubes falloff (Power exp=3). ClayBuildup uses Smooth×hardness
        // (exp=1) so hardness drives a hard ZBrush strip edge (old fall² softened the rim).
        let (kernel, exp) = match brush {
            B::Crease => (Kernel::Power, 3.0),
            B::ClayBuildup => (Kernel::Smooth, 1.0),
            _ => (Kernel::Smooth, 1.0),
        };
        let kernel_params = KernelParams { exp, hardness, falloff_preset };

        // SLOT 4 — the single op + its plane parameters, byte-identical to the legacy match arm.
        let op = match brush {
            B::Draw => OpEntry { op: Operator::Add, ..Default::default() },
            B::Erase => OpEntry { op: Operator::Subtract, ..Default::default() },
            B::Smooth => OpEntry { op: Operator::Relax, ..Default::default() },
            B::Flatten => OpEntry { op: Operator::PlanePush, side: PlaneSide::Both, ..Default::default() },
            // SculptGL clay: plane offset 0.1×radius (proud of contact), fill below, light shave above.
            B::Clay => OpEntry {
                op: Operator::PlaneFill, plane_offset: 0.10, side: PlaneSide::Below, shave: true,
                ..Default::default()
            },
            B::Scrape => OpEntry { op: Operator::PlaneClip, side: PlaneSide::Above, ..Default::default() },
            // Crease = Subtract + Power{3} kernel (kernel set above).
            B::Crease => OpEntry { op: Operator::Subtract, ..Default::default() },
            B::Fill => OpEntry {
                op: Operator::PlaneFill, plane_offset: 0.0, side: PlaneSide::Below, shave: false,
                // Legacy Fill: invert flips WHICH side fills (deposit fixed +w, strict boundary).
                sgn_on_side: true,
                ..Default::default()
            },
            // ClayBuildup = PlaneFill converge; plane_offset = layer height / radius.
            // fold_recipe overwrites plane_offset with strength-scaled layer (see fold).
            B::ClayBuildup => OpEntry {
                op: Operator::PlaneFill, plane_offset: 0.25, side: PlaneSide::Below, shave: false,
                converge: true, // pack layer along normal toward proud plane, then stop
                ..Default::default()
            },
            B::Inflate => OpEntry { op: Operator::Inflate, ..Default::default() },
            B::Pinch => OpEntry { op: Operator::Advect, advect: AdvectSrc::Tangential, ..Default::default() },
            B::Move => OpEntry { op: Operator::Advect, advect: AdvectSrc::DragVector, ..Default::default() },
        };
        let mut ops = [OpEntry::default(); MAX_OPS];
        ops[0] = op;

        // SLOT 3 — channels: DENSITY always (Move/Pinch are displacement ops but still write
        // density on the CPU path — see the legacy arms; the architecture's DISPLACE tag is a
        // Phase-1+ refinement and does NOT change the Phase-0 density math). COLOR added when
        // the paint toggle is on, so the recipe describes the in-flight paint feature.
        let mut mask = ChannelMask::DENSITY;
        if paint_color_on {
            mask = mask | ChannelMask::COLOR;
        }
        let channels = ChannelSet {
            mask,
            color: paint_color,
            roughness: 0.0,
            density_scale: 1.0,
            color_scale: 1.0,
        };

        // SLOT 5 — pressure route from the two global toggles.
        let pressure = match (pressure_to_strength, pressure_to_radius) {
            (true, true) => PressureRoute::Both,
            (true, false) => PressureRoute::Strength,
            (false, true) => PressureRoute::Radius,
            (false, false) => PressureRoute::None,
        };
        let modulators = Modulators { pressure, symmetry, spacing, lazy, allow_invert };

        BrushRecipe {
            uid: BrushUid(brush as u64),
            name: brush.name().to_string(),
            builtin: true,
            version: 1,
            kernel,
            kernel_params,
            direction: brush_dir,
            channels,
            ops,
            op_count: 1,
            modulators,
            base_radius: radius,
            base_strength: strength,
        }
    }
}

/// The shipped built-in brush identity (Phase-0 mirror of `voxel_sculpt::VoxelBrush`). Kept in
/// `core_math::brush` so the recipe module has no dependency on the `voxel_sculpt` brush enum
/// ordering; the app maps `VoxelBrush -> BuiltinBrush` at the single-producer site. Discriminants
/// match `VoxelBrush`'s declaration order (the GPU op-code order) for clarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u64)]
pub enum BuiltinBrush {
    Draw = 0,
    Erase = 1,
    Smooth = 2,
    Flatten = 3,
    Clay = 4,
    Scrape = 5,
    Crease = 6,
    Fill = 7,
    ClayBuildup = 8,
    Inflate = 9,
    Pinch = 10,
    Move = 11,
}

impl BuiltinBrush {
    pub fn name(self) -> &'static str {
        match self {
            BuiltinBrush::Draw => "Draw",
            BuiltinBrush::Erase => "Erase",
            BuiltinBrush::Smooth => "Smooth",
            BuiltinBrush::Flatten => "Flatten",
            BuiltinBrush::Clay => "Clay",
            BuiltinBrush::Scrape => "Scrape",
            BuiltinBrush::Crease => "Crease",
            BuiltinBrush::Fill => "Fill",
            BuiltinBrush::ClayBuildup => "ClayBuildup",
            BuiltinBrush::Inflate => "Inflate",
            BuiltinBrush::Pinch => "Pinch",
            BuiltinBrush::Move => "Move",
        }
    }

    /// All 12 built-ins, in op-code order. For the golden parity test + preset enumeration.
    pub const ALL: [BuiltinBrush; 12] = [
        BuiltinBrush::Draw, BuiltinBrush::Erase, BuiltinBrush::Smooth, BuiltinBrush::Flatten,
        BuiltinBrush::Clay, BuiltinBrush::Scrape, BuiltinBrush::Crease, BuiltinBrush::Fill,
        BuiltinBrush::ClayBuildup, BuiltinBrush::Inflate, BuiltinBrush::Pinch, BuiltinBrush::Move,
    ];

    /// Resolve a concrete deposit direction for one stamp from a `Direction` slot. This is the
    /// recipe-side analogue of `BrushDirection::resolve`; the producer passes the already-chosen
    /// surface/view/first-point vectors. Kept here so the fold has a single resolver.
    #[inline]
    pub fn resolve_direction(dir: Direction, surface: Vec3, view: Vec3, first_point: Option<Vec3>) -> Vec3 {
        match dir {
            Direction::SurfaceNormal => surface,
            Direction::ViewDirection => view,
            Direction::FirstPoint => first_point.unwrap_or(surface),
            Direction::AxisX => Vec3::X,
            Direction::AxisY => Vec3::Y,
            Direction::AxisZ => Vec3::Z,
        }
    }
}
