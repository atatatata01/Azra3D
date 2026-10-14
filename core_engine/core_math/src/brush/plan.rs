// core_engine/core_math/src/brush/plan.rs
//
// The compiled per-stamp POD — `StampPlan` (§1 "The compiled per-stamp POD").
//
// The slot fold (`eval::fold_recipe`) produces ONE flat POD per stamp. This is the ONLY thing
// the apply engine (and, in Phase 3, the GPU uniform) sees. It is a RICHER version of today's
// loose `(op, center, normal, radius, strength, sgn, aabb)` tuple — nothing downstream needs to
// know slots existed.
//
// `#[repr(C)]` + `bytemuck` `Pod`/`Zeroable` so the same struct can be uploaded as a GPU uniform
// in Phase 3 with zero re-layout. Phase 0 only uses it CPU-side, but the layout is fixed NOW so
// the GPU bridge is a no-op later. Field layout is std140/std430-friendly: vec4-aligned blocks
// first, then the scalar tail, then a fixed `PackedOp` array, then pad to a 16-byte multiple.

use bytemuck::{Pod, Zeroable};

/// Hard cap on the in-kernel op-list (§1). Bounds the fixed array in both the recipe and the POD.
pub const MAX_OPS: usize = 4;

/// One operator packed into the flat POD (4×u32 = 16 bytes). `flags` packs the small enums
/// (PlaneSide | OpBlend | AdvectSrc | shave bit) so the struct stays POD with no enum payloads.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct PackedOp {
    /// `Operator` discriminant (the frozen op-code).
    pub op_code: u32,
    /// Bit-packed: see `flags::*` below.
    pub flags: u32,
    /// Per-op strength multiplier.
    pub amount: f32,
    /// Plane offset ×radius.
    pub plane_offset: f32,
}

impl PackedOp {
    /// Zeroed op (op_code 0 == Add, no flags). Used to fill unused op-list slots.
    pub const ZERO: PackedOp = PackedOp { op_code: 0, flags: 0, amount: 0.0, plane_offset: 0.0 };
}

/// Bit layout of `PackedOp.flags`. PlaneSide in bits 0..2, OpBlend in bits 2..4, AdvectSrc in
/// bit 4, shave in bit 5. Decoded by the apply engine (`eval`/`voxel_sculpt`).
pub mod flags {
    /// PlaneSide occupies bits 0..2 (values 0=Both, 1=Above, 2=Below).
    pub const SIDE_SHIFT: u32 = 0;
    pub const SIDE_MASK: u32 = 0b11;
    /// OpBlend occupies bits 2..4 (0=Accumulate, 1=Max, 2=Min, 3=Replace).
    pub const BLEND_SHIFT: u32 = 2;
    pub const BLEND_MASK: u32 = 0b11;
    /// AdvectSrc occupies bit 4 (0=Tangential, 1=DragVector).
    pub const ADVECT_SHIFT: u32 = 4;
    pub const ADVECT_MASK: u32 = 0b1;
    /// Shave flag in bit 5.
    pub const SHAVE_BIT: u32 = 1 << 5;
    /// `PlaneFill` sign mode in bit 6 (PHASE-0 PARITY): the legacy Clay/ClayBuildup arms apply the
    /// invert `sgn` to the DEPOSIT (always fill the `plane<=0` side, the sign flips deposit/erode),
    /// whereas the legacy Fill arm applies `sgn` to the SIDE TEST (always `+w`, the sign flips WHICH
    /// half fills) and uses a STRICT `< 0` boundary. This bit selects the Fill behaviour. It exists
    /// only to reproduce today's two distinct plane-deposit semantics bit-for-bit; unifying them is
    /// a Phase-1 task (promote constants), guarded by the same parity test.
    pub const SGN_ON_SIDE_BIT: u32 = 1 << 6;
    /// `PlaneFill` CONVERGING fill in bit 7 (Clay-1): fill the density toward the offset plane and
    /// STOP, instead of the unbounded additive deposit. Set only for ClayBuildup. The GPU mirror in
    /// `voxel_brush.wgsl` (`CONVERGE_BIT = 128u`) must match this value.
    pub const CONVERGE_BIT: u32 = 1 << 7;
    /// Height Limit (0..1 density) packed as u8 in bits 8..16 — Build max target density.
    pub const HEIGHT_LIMIT_SHIFT: u32 = 8;
    pub const HEIGHT_LIMIT_MASK: u32 = 0xFF;
    /// Volume Preservation (0..1) packed as u8 in bits 16..24 — Flatten/Inflate softens in solid core.
    pub const VOL_PRES_SHIFT: u32 = 16;
    pub const VOL_PRES_MASK: u32 = 0xFF;
    /// Inflate Direction mode in bits 24..26 (0=Gradient, 1=Surface, 2=Radial).
    pub const INFLATE_DIR_SHIFT: u32 = 24;
    pub const INFLATE_DIR_MASK: u32 = 0b11;

    #[inline]
    pub fn pack_height_limit(h: f32) -> u32 {
        let u = (h.clamp(0.0, 1.0) * 255.0).round() as u32;
        (u & HEIGHT_LIMIT_MASK) << HEIGHT_LIMIT_SHIFT
    }
    #[inline]
    pub fn unpack_height_limit(flags: u32) -> f32 {
        ((flags >> HEIGHT_LIMIT_SHIFT) & HEIGHT_LIMIT_MASK) as f32 / 255.0
    }

    /// Map UI **Height Limit** (0..1, **1 = off**) → absolute density ceiling.
    ///
    /// Absolute density cap (same as original knob), but **never at/below iso** so a mid
    /// slider (0.5) cannot collapse the surface. Packed 0 / UI ≥ 0.999 → no cap (`None`).
    ///
    /// IMPORTANT: callers must only use the cap to **limit growth**, never to pull existing
    /// solid density *down* to the ceiling (that made every brush “do nothing” / erase clay).
    #[inline]
    pub fn density_cap_from_height_limit(h_ui: f32, iso: f32) -> Option<f32> {
        if !(h_ui.is_finite()) || h_ui < 1e-4 || h_ui >= 0.999 {
            return None; // off / unset / NaN
        }
        let iso = if iso.is_finite() { iso.clamp(0.0, 0.95) } else { 0.5 };
        // Absolute density, floored just above iso so 0.5 ≠ “kill the surface”.
        let cap = h_ui.clamp(0.0, 1.0).max(iso + 0.02);
        Some(cap.min(1.0))
    }

    /// Apply density ceiling to a candidate write: limits **upward** deposit only.
    /// If density is already above the cap, leave it (do not dig solid down to the ceiling).
    #[inline]
    pub fn apply_density_cap(cur: f32, nxt: f32, cap: Option<f32>) -> f32 {
        let Some(c) = cap else { return nxt; };
        if nxt > cur {
            // Growing: allow growth up to cap; if already above cap, stay put.
            if cur >= c {
                cur
            } else {
                nxt.min(c)
            }
        } else {
            // Carving / lower: height limit does not apply.
            nxt
        }
    }
    #[inline]
    pub fn pack_vol_pres(v: f32) -> u32 {
        let u = (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        (u & VOL_PRES_MASK) << VOL_PRES_SHIFT
    }
    #[inline]
    pub fn unpack_vol_pres(flags: u32) -> f32 {
        ((flags >> VOL_PRES_SHIFT) & VOL_PRES_MASK) as f32 / 255.0
    }
    #[inline]
    pub fn pack_inflate_dir(mode: u32) -> u32 {
        (mode & INFLATE_DIR_MASK) << INFLATE_DIR_SHIFT
    }
    #[inline]
    pub fn unpack_inflate_dir(flags: u32) -> u32 {
        (flags >> INFLATE_DIR_SHIFT) & INFLATE_DIR_MASK
    }
}

/// How Inflate expands density near the surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
#[repr(u32)]
pub enum InflateDirection {
    /// Density-gradient magnitude (legacy shell inflate). Default.
    #[default]
    Gradient = 0,
    /// Prefer expansion aligned with the stamp / deposit normal (Surface Direction).
    Surface = 1,
    /// Stronger near the brush centre (radial falloff of the effect itself).
    Radial = 2,
}

impl InflateDirection {
    pub const ALL: [InflateDirection; 3] = [
        InflateDirection::Gradient,
        InflateDirection::Surface,
        InflateDirection::Radial,
    ];
    #[inline]
    pub fn as_u32(self) -> u32 { self as u32 }
    #[inline]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => InflateDirection::Surface,
            2 => InflateDirection::Radial,
            _ => InflateDirection::Gradient,
        }
    }
    #[inline]
    pub fn label(self) -> &'static str {
        match self {
            InflateDirection::Gradient => "Gradient",
            InflateDirection::Surface => "Surface",
            InflateDirection::Radial => "Radial",
        }
    }
    #[inline]
    pub fn tip(self) -> &'static str {
        match self {
            InflateDirection::Gradient => "Inflate along density-gradient magnitude (classic shell puff).",
            InflateDirection::Surface => "Weight inflate by alignment with deposit / surface normal.",
            InflateDirection::Radial => "Stronger inflate near the brush centre (radial).",
        }
    }
}

/// The flat per-stamp plan — the bridge between the recipe slots and the apply engine.
///
/// Layout (vec4-aligned blocks first for std140 friendliness):
///   center   : [f32;4]  — .xyz center, .w = radius
///   plane_n  : [f32;4]  — .xyz resolved deposit normal, .w = strength
///   color    : [f32;4]  — RGBA when COLOR set (Phase 0: carried, not consumed)
///   drag     : [f32;4]  — .xyz Move drag delta (voxel-local), .w unused
///   then scalars: kernel_code, kernel_exp, kernel_hardness, chan_mask, sgn, op_count, _pad0, _pad1
///   then op_list: [PackedOp; MAX_OPS]
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct StampPlan {
    /// .xyz = brush center (voxel-local), .w = radius.
    pub center: [f32; 4],
    /// .xyz = resolved deposit normal (Direction already resolved CPU-side), .w = strength.
    pub plane_n: [f32; 4],
    /// RGBA paint colour when COLOR is set. Phase 0 carries it; the apply engine does not write
    /// colour (paint keeps its `deposit_color_sphere` plumbing).
    pub color: [f32; 4],
    /// .xyz = Move/Grab drag delta (voxel-local space). Zero for every non-Advect-DragVector op.
    /// Carried in the POD so the apply engine reproduces the Move arm without a side channel.
    pub drag: [f32; 4],

    /// `Kernel` discriminant.
    pub kernel_code: u32,
    /// `Power` exponent (1.0 for non-Power).
    pub kernel_exp: f32,
    /// Continuous radial hardness (feeds `falloff_weight`).
    pub kernel_hardness: f32,
    /// `ChannelMask::bits()`.
    pub chan_mask: u32,

    /// Brush sign: +1.0 normally, -1.0 when inverted (Ctrl).
    pub sgn: f32,
    /// Number of active ops in `op_list` (Phase 0: always 1).
    pub op_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,

    /// The in-kernel op sequence (single dispatch consumes it). Phase 0 fills only `[0]`.
    pub op_list: [PackedOp; MAX_OPS],
}

impl StampPlan {
    /// A zeroed plan with `op_count = 0`. The fold seeds from this and fills the slots.
    pub fn zeroed() -> Self {
        StampPlan {
            center: [0.0; 4],
            plane_n: [0.0; 4],
            color: [0.0; 4],
            drag: [0.0; 4],
            kernel_code: 0,
            kernel_exp: 1.0,
            kernel_hardness: 0.0,
            chan_mask: 0,
            sgn: 1.0,
            op_count: 0,
            _pad0: 0,
            _pad1: 0,
            op_list: [PackedOp::ZERO; MAX_OPS],
        }
    }

    /// Brush radius (`center.w`).
    #[inline]
    pub fn radius(&self) -> f32 { self.center[3] }
    /// Per-stamp strength (`plane_n.w`).
    #[inline]
    pub fn strength(&self) -> f32 { self.plane_n[3] }
}
