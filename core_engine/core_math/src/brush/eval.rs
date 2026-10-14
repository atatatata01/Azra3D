// core_engine/core_math/src/brush/eval.rs
//
// The CPU fold: `BrushRecipe` + a surface hit -> ONE flat `StampPlan` POD (§4.1).
//
// Runs ONCE per stamp at the single-producer site (`input_sculpt`). It is pure arithmetic on
// ~40 floats — negligible, well inside the 12ms budget. PHASE 0 NOTE: the producer already
// computes the per-stamp `radius`, `strength` (incl. `* pressure`), resolved deposit `normal`
// (Direction resolved against camera/stroke state), and Move `drag_delta`. The fold PACKS those
// already-resolved values into the plan verbatim — it does NOT re-derive them — so the routed
// path is bit-identical to today. (Pressure-route / Direction *selection* lives in the recipe so
// the data model is lossless, but the numeric resolution stays where it is today; moving the
// resolution into the fold is a Phase-1 refactor guarded by the same parity test.)

use glam::Vec3;

use super::plan::{flags, PackedOp, StampPlan, MAX_OPS};
use super::{
    AdvectSrc, BrushRecipe, ChannelMask, OpBlend, OpEntry, Operator, PlaneSide,
};
// OpEntry used by stack-mod construction in fold_recipe.

/// The resolved contact for one stamp — the already-computed per-stamp scalars/vectors the
/// producer hands to the fold. Everything here is in voxel-LOCAL space (the frame `apply_brush`
/// works in), matching today's `apply_brush` arguments exactly.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceHit {
    /// Brush center (voxel-local) — today's `center`.
    pub center: Vec3,
    /// Resolved deposit normal (Direction already resolved) — today's `pt.normal` / `dir`.
    pub normal: Vec3,
    /// Per-stamp radius — today's `radius`.
    pub radius: f32,
    /// Per-stamp strength (already includes `* pressure`) — today's `strength * pt.pressure`.
    pub strength: f32,
}

/// Stroke-level modulation the fold needs that is not part of a single hit. Phase 0: only the
/// invert sign and the Move drag delta affect the numeric plan; the rest is carried on the recipe.
/// **Stack mods** append secondary operators after the base op (up to `MAX_OPS`) so e.g. Flatten
/// can also Gather (Pinch) and Pinch can also Flatten — system knobs, not per-base-op gates.
#[derive(Clone, Copy, Debug)]
pub struct StrokeMods {
    /// Ctrl-invert active this stamp — today's `invert`.
    pub invert: bool,
    /// Move/Grab per-stamp drag delta (voxel-local) — today's `drag_delta`. Zero for every
    /// non-Move brush (the producer passes `Vec3::ZERO`).
    pub drag_delta: Vec3,
    /// Pinch gather-reach multiplier (ZBrush-style Pinch Strength). Default 1.0 = legacy parity.
    /// Packed into `PackedOp.amount` for Tangential Advect only; other ops keep amount = 1.0.
    pub pinch_intensity: f32,
    /// **Layer Height** — proud plane as ×radius (Build/Clay base). Default 0.14.
    pub layer_height: f32,
    /// **Layer×Str / Layer Gain** — strength scales layer: offset += gain × strength. Default 0.22.
    pub layer_gain: f32,
    /// **Plane Offset** — extra plane shift as ×radius (Clay default 0.10 when no layer override).
    pub plane_offset: f32,
    /// **Plane Bias** — additional normal-direction offset as ×radius (positive = further proud).
    pub plane_bias: f32,
    /// **Build Rate** — converge pack speed multiplier (legacy hardcoded 2.0). Default 2.0.
    pub build_rate: f32,
    /// **Height Limit** — max density target for Build layers (0..1). Default 1.0 (full solid).
    pub height_limit: f32,
    /// **Flatten Amount** — PlanePush blend scale (legacy hardcoded 0.5). Default 0.5.
    pub flatten_amount: f32,
    /// **Volume Preservation** — 0..1, Flatten/Inflate softens in solid cores (0 = full effect).
    pub volume_preservation: f32,
    /// **Accumulate** — Draw: false = soft dual approach (rate=wk); true = stronger dual (1.35×).
    /// Both polarities stay dual (no pure ± pile). PlaneFill: Max vs pure add.
    pub accumulate: bool,
    /// **Inflate Speed** — multiplier on Inflate density delta (legacy 1.0). Default 1.0.
    pub inflate_speed: f32,
    /// **Inflate Direction** — Gradient / Surface / Radial (see [`crate::brush::plan::InflateDirection`]).
    pub inflate_direction: u32,
    /// **Thickness Clamp** — max shell density band 0..1 (1 = no clamp). Limits how thick Inflate grows.
    pub thickness_clamp: f32,
    /// **Pinch Strength** — gather reach scale (same as `pinch_intensity`; alias for UI clarity).
    /// Kept as `pinch_intensity` field — this is documentation only in StrokeMods.
    /// **Pinch Width** — scales tangential gather offset (legacy 0.5×radius factor). Default 1.0.
    pub pinch_width: f32,
    /// **Pinch Amount** — blend weight of gather toward sampled density (default 1.0).
    pub pinch_amount: f32,
    /// **Crease Width** — narrows Crease falloff (higher = wider groove). Default 1.0.
    pub crease_width: f32,
    /// **Crease Depth** — multiplies Crease subtract strength. Default 1.0.
    pub crease_depth: f32,
    /// **Trim Plane Angle** — shifts Scrape/Trim cut threshold as ×radius (−1..1). Default 0.
    pub plane_angle: f32,
    /// **Cut Depth** — multiplies Scrape/Trim subtract amount. Default 1.0.
    pub cut_depth: f32,
    /// **Fill Holes** — Scrape/Trim also deposits lightly on the keep side to seal. Default false.
    pub fill_holes: bool,
    /// Append Tangential Advect (Pinch gather) after the base op (if base is not already Pinch).
    pub stack_gather: bool,
    /// Append PlanePush (Flatten) after the base op (if base is not already Flatten/Scrape).
    pub stack_flatten: bool,
    /// Append PlaneFill-below (Fill) after the base op (if base is not already Fill/Clay/Build).
    pub stack_fill: bool,
    /// Append Subtract groove (Crease-style) after the base op (if base is not already Crease/Erase).
    pub stack_crease: bool,
    /// Append Inflate after the base op (if base is not already Inflate).
    pub stack_inflate: bool,
}

impl Default for StrokeMods {
    fn default() -> Self {
        StrokeMods {
            invert: false,
            drag_delta: Vec3::ZERO,
            pinch_intensity: 1.0,
            layer_height: 0.14,
            layer_gain: 0.22,
            plane_offset: 0.10,
            plane_bias: 0.0,
            build_rate: 2.0,
            height_limit: 1.0,
            flatten_amount: 0.5,
            volume_preservation: 0.0,
            accumulate: true,
            inflate_speed: 1.0,
            inflate_direction: 0,
            thickness_clamp: 1.0,
            pinch_width: 1.0,
            pinch_amount: 1.0,
            crease_width: 1.0,
            crease_depth: 1.0,
            plane_angle: 0.0,
            cut_depth: 1.0,
            fill_holes: false,
            stack_gather: false,
            stack_flatten: false,
            stack_fill: false,
            stack_crease: false,
            stack_inflate: false,
        }
    }
}

/// Pack an `OpEntry` into the flat `PackedOp` (bit-packs the small enums into `flags`).
#[inline]
pub fn pack_op(e: &OpEntry) -> PackedOp {
    let side = match e.side {
        PlaneSide::Both => 0u32,
        PlaneSide::Above => 1,
        PlaneSide::Below => 2,
    };
    let blend = match e.blend {
        OpBlend::Accumulate => 0u32,
        OpBlend::Max => 1,
        OpBlend::Min => 2,
        OpBlend::Replace => 3,
    };
    let advect = match e.advect {
        AdvectSrc::Tangential => 0u32,
        AdvectSrc::DragVector => 1,
    };
    let mut f = (side << flags::SIDE_SHIFT)
        | (blend << flags::BLEND_SHIFT)
        | (advect << flags::ADVECT_SHIFT);
    if e.shave {
        f |= flags::SHAVE_BIT;
    }
    if e.sgn_on_side {
        f |= flags::SGN_ON_SIDE_BIT;
    }
    if e.converge {
        f |= flags::CONVERGE_BIT;
    }
    PackedOp {
        op_code: e.op as u32,
        flags: f,
        amount: e.amount,
        plane_offset: e.plane_offset,
    }
}

/// Fold a recipe + a resolved hit into one flat `StampPlan`. §4.1.
pub fn fold_recipe(r: &BrushRecipe, hit: &SurfaceHit, mods: &StrokeMods) -> StampPlan {
    let mut p = StampPlan::zeroed();

    // Base scalars / resolved vectors (the producer already applied pressure → strength, and
    // resolved Direction → normal). Pack verbatim for bit-identity.
    p.center = [hit.center.x, hit.center.y, hit.center.z, hit.radius];
    p.plane_n = [hit.normal.x, hit.normal.y, hit.normal.z, hit.strength];
    p.drag = [mods.drag_delta.x, mods.drag_delta.y, mods.drag_delta.z, 0.0];
    p.sgn = if mods.invert { -1.0 } else { 1.0 };

    // SLOT 1 — kernel.
    p.kernel_code = r.kernel as u32;
    p.kernel_exp = r.kernel_params.exp;
    p.kernel_hardness = r.kernel_params.hardness;
    // Falloff preset id rides `_pad0` (POD free slot) → CPU apply + GPU `box_dim.w`.
    p._pad0 = r.kernel_params.falloff_preset;

    // SLOT 3 — channels (carried; the apply engine writes only DENSITY in Phase 0).
    p.chan_mask = r.channels.mask.bits();
    let c = r.channels.color;
    p.color = [
        ((c >> 16) & 0xFF) as f32 / 255.0,
        ((c >> 8) & 0xFF) as f32 / 255.0,
        (c & 0xFF) as f32 / 255.0,
        ((c >> 24) & 0xFF) as f32 / 255.0,
    ];

    // SLOT 4 — operator stack → flat op_list. Base recipe ops first, then optional stack mods
    // (Gather / Flatten / Fill) so any base brush can layer Pinch-on-Flatten etc.
    let n_base = (r.op_count as usize).clamp(1, MAX_OPS);
    let mut n = 0usize;
    for i in 0..n_base {
        if n >= MAX_OPS { break; }
        let mut packed = pack_op(&r.ops[i]);
        refine_packed_op(&mut packed, mods, hit.strength);
        p.op_list[n] = packed;
        n += 1;
    }
    // Detect what the base already is so we don't double-stack the same op.
    let base0 = p.op_list[0];
    let base_is_gather = base0.op_code == Operator::Advect as u32
        && unpack_advect(base0.flags) == AdvectSrc::Tangential;
    let base_is_flatten = base0.op_code == Operator::PlanePush as u32;
    let base_is_fill_family = base0.op_code == Operator::PlaneFill as u32
        || base0.op_code == Operator::PlaneClip as u32;
    let base_is_move = base0.op_code == Operator::Advect as u32
        && unpack_advect(base0.flags) == AdvectSrc::DragVector;
    let base_is_crease = base0.op_code == Operator::Subtract as u32
        && p.kernel_exp >= 2.5; // Crease uses Power exp=3; Erase uses Smooth exp=1
    let base_is_erase = base0.op_code == Operator::Subtract as u32 && !base_is_crease;
    let base_is_inflate = base0.op_code == Operator::Inflate as u32;

    // Stack mods are ignored for pure Move (drag advection is its own path).
    // User toggles these ON to layer behaviors onto any base op (e.g. Draw + Gather).
    if !base_is_move {
        if mods.stack_gather && !base_is_gather && n < MAX_OPS {
            let e = OpEntry {
                op: Operator::Advect,
                advect: AdvectSrc::Tangential,
                amount: 1.0,
                ..Default::default()
            };
            let mut packed = pack_op(&e);
            refine_packed_op(&mut packed, mods, hit.strength);
            p.op_list[n] = packed;
            n += 1;
        }
        if mods.stack_flatten && !base_is_flatten && n < MAX_OPS {
            let e = OpEntry {
                op: Operator::PlanePush,
                side: PlaneSide::Both,
                ..Default::default()
            };
            let mut packed = pack_op(&e);
            refine_packed_op(&mut packed, mods, hit.strength);
            p.op_list[n] = packed;
            n += 1;
        }
        if mods.stack_fill && !base_is_fill_family && n < MAX_OPS {
            let e = OpEntry {
                op: Operator::PlaneFill,
                plane_offset: 0.0,
                side: PlaneSide::Below,
                shave: false,
                sgn_on_side: true, // Fill semantics
                ..Default::default()
            };
            let mut packed = pack_op(&e);
            refine_packed_op(&mut packed, mods, hit.strength);
            p.op_list[n] = packed;
            n += 1;
        }
        if mods.stack_crease && !base_is_crease && !base_is_erase && n < MAX_OPS {
            // Mild groove after base (Draw+Crease, Flatten+Crease, …). Uses crease knobs.
            let e = OpEntry {
                op: Operator::Subtract,
                ..Default::default()
            };
            let mut packed = pack_op(&e);
            // Force crease knobs onto this stacked subtract.
            packed.amount = if mods.crease_depth > 1e-6 {
                mods.crease_depth.clamp(0.0, 4.0)
            } else {
                1.0
            };
            packed.plane_offset = if mods.crease_width > 1e-6 {
                mods.crease_width.clamp(0.1, 4.0)
            } else {
                1.0
            };
            p.op_list[n] = packed;
            n += 1;
        }
        if mods.stack_inflate && !base_is_inflate && n < MAX_OPS {
            let e = OpEntry {
                op: Operator::Inflate,
                ..Default::default()
            };
            let mut packed = pack_op(&e);
            refine_packed_op(&mut packed, mods, hit.strength);
            p.op_list[n] = packed;
            n += 1;
        }
    }
    p.op_count = n.max(1) as u32;
    p
}

/// Apply system knobs (layer/plane/flatten/build/gather) onto a packed op in place.
#[inline]
fn refine_packed_op(packed: &mut PackedOp, mods: &StrokeMods, strength: f32) {
    use super::plan::flags;
    let bias = mods.plane_bias;
    let off0 = mods.plane_offset; // may be 0 — valid
    // Height limit + volume preservation ride flag upper bytes (CPU + GPU decode).
    packed.flags = (packed.flags
        & !((flags::HEIGHT_LIMIT_MASK << flags::HEIGHT_LIMIT_SHIFT)
            | (flags::VOL_PRES_MASK << flags::VOL_PRES_SHIFT)))
        | flags::pack_height_limit(mods.height_limit)
        | flags::pack_vol_pres(mods.volume_preservation);
    // Accumulate vs Max for plane fills AND Draw/Add (basic brush presets).
    if packed.op_code == Operator::PlaneFill as u32 || packed.op_code == Operator::Add as u32 {
        let blend_bits = if mods.accumulate {
            0u32 // Accumulate
        } else {
            1u32 // Max
        };
        packed.flags = (packed.flags & !(flags::BLEND_MASK << flags::BLEND_SHIFT))
            | (blend_bits << flags::BLEND_SHIFT);
    }
    if packed.op_code == Operator::PlaneFill as u32 {
        let base = mods.layer_height.max(0.0);
        let gain = mods.layer_gain.max(0.0);
        if unpack_converge(packed.flags) {
            // Build only: Layer height + Layer×Str×strength + bias (Clay BuildUp).
            packed.plane_offset = base + gain * strength.clamp(0.0, 1.0) + bias;
            packed.amount = mods.build_rate.max(0.0);
            if packed.amount < 1e-6 {
                packed.amount = 2.0; // legacy default
            }
        } else {
            // Clay / Fill: live Plane Offset (0 is valid — do NOT fall back to recipe when 0).
            // Recipe default only used when plane_offset was never authored (neg sentinel unused;
            // app always passes the live knob).
            packed.plane_offset = off0 + bias;
            if packed.amount < 1e-6 {
                packed.amount = 1.0;
            }
        }
    }
    if packed.op_code == Operator::PlanePush as u32 {
        // Flatten Amount (legacy 0.5)
        packed.amount = if mods.flatten_amount > 1e-6 {
            mods.flatten_amount.clamp(0.0, 2.0)
        } else {
            0.5
        };
        // Plane Offset + Plane Bias both shift the flatten plane (×radius).
        // Was bias-only → Plane Offset slider did nothing on Flatten.
        packed.plane_offset = off0 + bias;
    }
    if packed.op_code == Operator::PlaneClip as u32 {
        // Trim: plane_offset stores plane_angle (+ bias); amount = cut_depth; fill_holes → CONVERGE bit.
        packed.plane_offset = mods.plane_angle + bias;
        packed.amount = if mods.cut_depth > 1e-6 {
            mods.cut_depth.clamp(0.0, 4.0)
        } else {
            1.0
        };
        if mods.fill_holes {
            packed.flags |= flags::CONVERGE_BIT;
        } else {
            packed.flags &= !flags::CONVERGE_BIT;
        }
    }
    if packed.op_code == Operator::Advect as u32
        && unpack_advect(packed.flags) == AdvectSrc::Tangential
    {
        // Pinch Strength (reach) in amount; Width in plane_offset; Amount in height_limit byte.
        packed.amount = mods.pinch_intensity.max(0.0);
        packed.plane_offset = if mods.pinch_width > 1e-6 {
            mods.pinch_width.clamp(0.05, 4.0)
        } else {
            1.0
        };
        // Re-pack height_limit slot as pinch_amount (0..1 maps to 0..2 via *2 in apply when pinch).
        // Default amount 1.0 → pack 0 so unpack treats as identity (avoids 8-bit quantize drift vs legacy).
        let pa = if (mods.pinch_amount - 1.0).abs() < 1e-3 {
            0.0
        } else {
            mods.pinch_amount.clamp(0.0, 2.0) * 0.5 // store 0..1
        };
        packed.flags = (packed.flags & !(flags::HEIGHT_LIMIT_MASK << flags::HEIGHT_LIMIT_SHIFT))
            | flags::pack_height_limit(pa)
            | flags::pack_vol_pres(mods.volume_preservation);
    }
    if packed.op_code == Operator::Subtract as u32 {
        // Crease knobs when +Crease stack is on, or knobs differ from identity (Crease base factory
        // sets width/depth ≠ 1). Erase keeps identity → amount=1 / width=1. UI only enables crease
        // sliders when base is Crease or +Crease is toggled ON.
        let apply_crease = mods.stack_crease
            || (mods.crease_depth - 1.0).abs() > 1e-4
            || (mods.crease_width - 1.0).abs() > 1e-4;
        if apply_crease {
            packed.amount = if mods.crease_depth > 1e-6 {
                mods.crease_depth.clamp(0.0, 4.0)
            } else {
                1.0
            };
            packed.plane_offset = if mods.crease_width > 1e-6 {
                mods.crease_width.clamp(0.1, 4.0)
            } else {
                1.0
            };
        } else {
            packed.amount = 1.0;
            packed.plane_offset = 1.0;
        }
    }
    if packed.op_code == Operator::Inflate as u32 {
        // amount = Inflate Speed; plane_offset = Thickness Clamp (0..1 density band).
        packed.amount = if mods.inflate_speed > 1e-6 {
            mods.inflate_speed.clamp(0.0, 4.0)
        } else {
            1.0
        };
        packed.plane_offset = mods.thickness_clamp.clamp(0.05, 1.0);
        packed.flags = (packed.flags & !(flags::INFLATE_DIR_MASK << flags::INFLATE_DIR_SHIFT))
            | flags::pack_inflate_dir(mods.inflate_direction);
    }
}

/// Decode OpBlend from PackedOp.flags.
#[inline]
pub fn unpack_blend(flags_word: u32) -> OpBlend {
    match (flags_word >> flags::BLEND_SHIFT) & flags::BLEND_MASK {
        1 => OpBlend::Max,
        2 => OpBlend::Min,
        3 => OpBlend::Replace,
        _ => OpBlend::Accumulate,
    }
}

/// Decode `PackedOp.flags` PlaneSide. Used by the apply engine.
#[inline]
pub fn unpack_side(flags_word: u32) -> PlaneSide {
    match (flags_word >> flags::SIDE_SHIFT) & flags::SIDE_MASK {
        1 => PlaneSide::Above,
        2 => PlaneSide::Below,
        _ => PlaneSide::Both,
    }
}

/// Decode `PackedOp.flags` AdvectSrc.
#[inline]
pub fn unpack_advect(flags_word: u32) -> AdvectSrc {
    if (flags_word >> flags::ADVECT_SHIFT) & flags::ADVECT_MASK == 1 {
        AdvectSrc::DragVector
    } else {
        AdvectSrc::Tangential
    }
}

/// Decode `PackedOp.flags` shave bit.
#[inline]
pub fn unpack_shave(flags_word: u32) -> bool {
    flags_word & flags::SHAVE_BIT != 0
}

/// Decode `PackedOp.flags` `PlaneFill` sign-on-side bit (legacy Fill semantics).
#[inline]
pub fn unpack_sgn_on_side(flags_word: u32) -> bool {
    flags_word & flags::SGN_ON_SIDE_BIT != 0
}

/// Decode `PackedOp.flags` `PlaneFill` converge bit (Clay-1 converging fill, ClayBuildup only).
#[inline]
pub fn unpack_converge(flags_word: u32) -> bool {
    flags_word & flags::CONVERGE_BIT != 0
}

/// Decode the op-code back to an `Operator` (the apply engine matches on this). Phase 0's
/// 8-arm family; an unknown code falls back to `Add` (a defined no-op-ish default, never reached
/// because the fold only ever writes valid discriminants).
#[inline]
pub fn op_from_code(code: u32) -> Operator {
    match code {
        0 => Operator::Add,
        1 => Operator::Subtract,
        2 => Operator::Relax,
        3 => Operator::PlanePush,
        4 => Operator::PlaneFill,
        5 => Operator::PlaneClip,
        6 => Operator::Inflate,
        7 => Operator::Advect,
        _ => Operator::Add,
    }
}

/// Whether a channel bit is set in a packed mask. Convenience for the apply engine.
#[inline]
pub fn mask_has(chan_mask: u32, bit: ChannelMask) -> bool {
    chan_mask & bit.bits() == bit.bits()
}
