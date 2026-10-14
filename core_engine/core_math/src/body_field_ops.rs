//! Body **field** modifier authoring (volume / bake domain).
//!
//! Runtime bake still reads hardwired `SdfBody` fields (`mirror_flags`, `mirror_sign`,
//! `mirror_center`, `bevel_radius`) via `BodyField` / GPU serialize. This module owns the
//! ordered authoring stack and **syncs into those fields** so eval/GPU stay one path.
//!
//! Body Smooth (curvature / occlusion modes) lives only on the stack + `FieldScope` — it
//! needs multi-sample eval, so bake forces CPU when active (no GPU single-float fold).
//!
//! See `MODIFIER_SYSTEM_PLAN.md` P2. Not the same as `display_mesh_ops` (post-DC).

use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

// ============================================================
// Volume-mirror sign bits (`VolumeMirror::sign_bits` == hardwired `SdfBody::mirror_sign`)
// ============================================================
//
// bits 0-2 : per-axis FLIP — which half is the SOURCE (0 = keep +axis, 1 = keep −axis).
// bit 7    : CUT — "delete the other half". Evaluates the body ONCE through a one-sided
//            domain fold (`p_axis = c ± |p_axis − c|`) instead of the 2ⁿ-combo true union,
//            so geometry on the discarded side disappears and the bake costs 1 scope eval
//            per voxel instead of 2ⁿ.
//
// **Bit 7 clear MUST decode to the historical union.** Every `.fl4` written before this bit
// existed stores 0 there, so old files keep byte-identical geometry. Packing into the spare
// bits (rather than adding a field) also keeps `BodyFieldModKind::VolumeMirror`'s bincode
// shape unchanged — the record round-trips through `ProjectFile::body_field_modifiers`.

/// `mirror_sign` bit 7 — cut mode ("delete the other half"); see the module header.
pub const MIRROR_CUT_BIT: u8 = 0b1000_0000;
/// `mirror_sign` bits 0-2 — the per-axis flip (which half is the source).
pub const MIRROR_FLIP_MASK: u8 = 0b0000_0111;

/// True when the mirror cuts the non-source half (one fold eval) instead of unioning both.
#[inline]
pub fn mirror_cut_enabled(mirror_sign: u8) -> bool {
    mirror_sign & MIRROR_CUT_BIT != 0
}

/// True when `axis` (0=X, 1=Y, 2=Z) keeps the NEGATIVE half as the source (flip bit set).
#[inline]
pub fn mirror_keeps_negative(mirror_sign: u8, axis: usize) -> bool {
    (mirror_sign >> axis) & 1 != 0
}

// ============================================================
// Authoring records
// ============================================================

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BodyFieldModRecord {
    pub id: u32,
    pub enabled: bool,
    pub expanded: bool,
    pub kind: BodyFieldModKind,
}

/// Body-level field smooth mode (mean-curvature flow family).
/// Applied after boolean + bevel on the scope close, near the surface band.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum BodySmoothMode {
    /// Full mean-curvature flow (smooth everything).
    #[default]
    Uniform = 0,
    /// Prefer sharp convex peaks / ridges (high positive curvature).
    Peak = 1,
    /// Only convex regions (bulges, outer corners).
    Convex = 2,
    /// Only concave regions (valleys, inner corners / creases).
    Concave = 3,
    /// Strength scales with |curvature| (feature-aware).
    Curvature = 4,
    /// Prefer occluded / enclosed cavities (local enclosure proxy).
    Occlusion = 5,
}

impl BodySmoothMode {
    pub fn label(self) -> &'static str {
        match self {
            BodySmoothMode::Uniform => "Uniform",
            BodySmoothMode::Peak => "Peak",
            BodySmoothMode::Convex => "Convex",
            BodySmoothMode::Concave => "Concave",
            BodySmoothMode::Curvature => "Curvature",
            BodySmoothMode::Occlusion => "Occlusion",
        }
    }

    pub fn all() -> &'static [BodySmoothMode] {
        &[
            BodySmoothMode::Uniform,
            BodySmoothMode::Peak,
            BodySmoothMode::Convex,
            BodySmoothMode::Concave,
            BodySmoothMode::Curvature,
            BodySmoothMode::Occlusion,
        ]
    }
}

/// Resolved smooth params for `FieldScope` eval (multi-sample curvature flow).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct BodySmoothParams {
    pub mode: BodySmoothMode,
    /// Surface motion scale (world units, order of feature softening).
    pub strength: f32,
    /// Feature / sample radius for finite differences + surface band.
    pub radius: f32,
}

impl Default for BodySmoothParams {
    fn default() -> Self {
        Self {
            mode: BodySmoothMode::Uniform,
            strength: 0.02,
            radius: 0.04,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum BodyFieldModKind {
    /// Volume mirror (same as hardwired body mirror — `field_eval::eval_scene_body_mirrored`).
    ///
    /// `sign_bits` packs BOTH the per-axis flip and the cut mode — see [`MIRROR_CUT_BIT`].
    /// Bit 7 clear = the historical 2ⁿ-combo true union; set = one-sided fold ("delete the
    /// other half"), which is also ~2ⁿ× cheaper to bake.
    VolumeMirror {
        axis_bits: u8,
        sign_bits: u8,
        center: Vec3,
    },
    /// Body edge bevel (`bevel_radius` at scope close).
    BodyBevel { radius: f32 },
    /// Post-boolean field smooth (curvature flow + mode masks). CPU multi-eval only.
    BodySmooth {
        mode: BodySmoothMode,
        strength: f32,
        radius: f32,
    },
    /// Surface displacement: `d' = d + strength * noise(p * frequency + seed)`.
    /// Adds procedural surface detail on the volume (CPU bake; multi-sample when combined
    /// with smooth). Frequency is 1/wavelength in body-local units.
    BodyDisplacement {
        strength: f32,
        frequency: f32,
        seed: u32,
    },
}

impl BodyFieldModRecord {
    pub fn new_volume_mirror() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: BodyFieldModKind::VolumeMirror {
                axis_bits: 1,
                // Cut ON by default (owner-directed): a NEW mirror deletes the other half and
                // costs one fold eval. Legacy stacks keep 0 here = union (see MIRROR_CUT_BIT).
                sign_bits: MIRROR_CUT_BIT,
                center: Vec3::ZERO,
            },
        }
    }

    pub fn new_body_bevel() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: BodyFieldModKind::BodyBevel { radius: 0.02 },
        }
    }

    pub fn new_body_smooth() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: BodyFieldModKind::BodySmooth {
                mode: BodySmoothMode::Uniform,
                strength: 0.02,
                radius: 0.04,
            },
        }
    }

    pub fn new_body_displacement() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: BodyFieldModKind::BodyDisplacement {
                strength: 0.015,
                frequency: 8.0,
                seed: 1,
            },
        }
    }

    pub fn label(&self) -> &'static str {
        match self.kind {
            BodyFieldModKind::VolumeMirror { .. } => "Volume Mirror",
            BodyFieldModKind::BodyBevel { .. } => "Body Bevel",
            BodyFieldModKind::BodySmooth { .. } => "Body Smooth",
            BodyFieldModKind::BodyDisplacement { .. } => "Displacement",
        }
    }
}

// ============================================================
// Hardwired ↔ stack
// ============================================================

/// Resolved hardwired body field state (bake/GPU inputs for mirror + bevel).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BodyFieldHardwired {
    pub mirror_flags: u8,
    pub mirror_sign: u8,
    pub mirror_center: Vec3,
    pub bevel_radius: f32,
}

/// Fold enabled stack ops → hardwired fields. Last enabled of each kind wins.
/// BodySmooth / BodyDisplacement are **not** hardwired (need multi-sample / noise).
pub fn resolve_hardwired(records: &[BodyFieldModRecord]) -> BodyFieldHardwired {
    let mut out = BodyFieldHardwired::default();
    for r in records {
        if !r.enabled {
            continue;
        }
        match &r.kind {
            BodyFieldModKind::VolumeMirror {
                axis_bits,
                sign_bits,
                center,
            } => {
                out.mirror_flags = *axis_bits;
                out.mirror_sign = *sign_bits;
                out.mirror_center = *center;
            }
            BodyFieldModKind::BodyBevel { radius } => {
                out.bevel_radius = (*radius).max(0.0);
            }
            BodyFieldModKind::BodySmooth { .. } | BodyFieldModKind::BodyDisplacement { .. } => {}
        }
    }
    out
}

/// Resolved displacement params for `FieldScope` eval.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct BodyDisplacementParams {
    /// Peak surface offset (world / body-local units). Positive = expand solid (subtract from d).
    pub strength: f32,
    /// Spatial frequency (cycles per unit). Higher = smaller features.
    pub frequency: f32,
    pub seed: u32,
}

impl Default for BodyDisplacementParams {
    fn default() -> Self {
        Self {
            strength: 0.015,
            frequency: 8.0,
            seed: 1,
        }
    }
}

/// Last enabled Body Displacement with |strength| > 0, else None.
pub fn resolve_displacement(records: &[BodyFieldModRecord]) -> Option<BodyDisplacementParams> {
    let mut out: Option<BodyDisplacementParams> = None;
    for r in records {
        if !r.enabled {
            continue;
        }
        if let BodyFieldModKind::BodyDisplacement {
            strength,
            frequency,
            seed,
        } = &r.kind
        {
            let s = *strength;
            if s.abs() > 1e-8 {
                out = Some(BodyDisplacementParams {
                    strength: s,
                    frequency: (*frequency).max(0.01),
                    seed: *seed,
                });
            } else {
                out = None;
            }
        }
    }
    out
}

/// Last enabled Body Smooth with strength > 0, else None.
pub fn resolve_smooth(records: &[BodyFieldModRecord]) -> Option<BodySmoothParams> {
    let mut out: Option<BodySmoothParams> = None;
    for r in records {
        if !r.enabled {
            continue;
        }
        if let BodyFieldModKind::BodySmooth {
            mode,
            strength,
            radius,
        } = &r.kind
        {
            let s = (*strength).max(0.0);
            if s > 1e-8 {
                out = Some(BodySmoothParams {
                    mode: *mode,
                    strength: s,
                    radius: (*radius).max(1e-4),
                });
            } else {
                out = None;
            }
        }
    }
    out
}

/// True when any enabled Body Smooth needs multi-sample CPU field eval.
pub fn body_smooth_needs_cpu(records: &[BodyFieldModRecord]) -> bool {
    resolve_smooth(records).is_some()
}

/// True when field needs CPU path (smooth multi-sample and/or displacement noise).
pub fn body_field_stack_needs_cpu(records: &[BodyFieldModRecord]) -> bool {
    resolve_smooth(records).is_some() || resolve_displacement(records).is_some()
}

/// Extra bake AABB margin from field smooth (surface can move ~strength in the band).
pub fn smooth_bounds_margin(records: &[BodyFieldModRecord]) -> f32 {
    let smooth_m = resolve_smooth(records)
        .map(|p| p.strength + p.radius * 0.5)
        .unwrap_or(0.0);
    let disp_m = resolve_displacement(records)
        .map(|p| p.strength.abs() * 1.25)
        .unwrap_or(0.0);
    smooth_m.max(disp_m)
}

/// Cheap 3D value noise in [-1, 1] for body displacement (no external deps).
#[inline]
pub fn displacement_noise3(p: Vec3, seed: u32) -> f32 {
    let i = p.floor();
    let f = p - i;
    let u = f * f * (Vec3::splat(3.0) - 2.0 * f); // smoothstep
    let n = |ix: i32, iy: i32, iz: i32| -> f32 {
        let mut h = seed
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add(ix as u32)
            .wrapping_mul(0x85EB_CA6B)
            .wrapping_add(iy as u32)
            .wrapping_mul(0xC2B2_AE35)
            .wrapping_add(iz as u32);
        h ^= h >> 16;
        h = h.wrapping_mul(0x7FEB_352D);
        h ^= h >> 15;
        h = h.wrapping_mul(0x846C_A68B);
        h ^= h >> 16;
        (h as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let x0 = i.x as i32;
    let y0 = i.y as i32;
    let z0 = i.z as i32;
    let c000 = n(x0, y0, z0);
    let c100 = n(x0 + 1, y0, z0);
    let c010 = n(x0, y0 + 1, z0);
    let c110 = n(x0 + 1, y0 + 1, z0);
    let c001 = n(x0, y0, z0 + 1);
    let c101 = n(x0 + 1, y0, z0 + 1);
    let c011 = n(x0, y0 + 1, z0 + 1);
    let c111 = n(x0 + 1, y0 + 1, z0 + 1);
    let x00 = c000 * (1.0 - u.x) + c100 * u.x;
    let x10 = c010 * (1.0 - u.x) + c110 * u.x;
    let x01 = c001 * (1.0 - u.x) + c101 * u.x;
    let x11 = c011 * (1.0 - u.x) + c111 * u.x;
    let y0v = x00 * (1.0 - u.y) + x10 * u.y;
    let y1v = x01 * (1.0 - u.y) + x11 * u.y;
    y0v * (1.0 - u.z) + y1v * u.z
}

/// Apply displacement to a signed distance (positive strength expands solid when noise > 0).
#[inline]
pub fn apply_displacement(d: f32, p: Vec3, params: &BodyDisplacementParams) -> f32 {
    if params.strength.abs() <= 1e-8 {
        return d;
    }
    let n = displacement_noise3(p * params.frequency, params.seed);
    // Standard SDF displacement: subtract so +noise grows the solid.
    d - params.strength * n
}

/// Seed stack from legacy hardwired fields when the stack is empty but hardwired is active.
pub fn seed_from_hardwired(hw: BodyFieldHardwired) -> Vec<BodyFieldModRecord> {
    let mut out = Vec::new();
    let mut next_id = 1u32;
    if hw.mirror_flags != 0 {
        out.push(BodyFieldModRecord {
            id: next_id,
            enabled: true,
            expanded: true,
            kind: BodyFieldModKind::VolumeMirror {
                axis_bits: hw.mirror_flags,
                sign_bits: hw.mirror_sign,
                center: hw.mirror_center,
            },
        });
        next_id += 1;
    }
    if hw.bevel_radius > 1e-6 {
        out.push(BodyFieldModRecord {
            id: next_id,
            enabled: true,
            expanded: true,
            kind: BodyFieldModKind::BodyBevel {
                radius: hw.bevel_radius,
            },
        });
    }
    out
}

pub fn hardwired_is_active(hw: BodyFieldHardwired) -> bool {
    hw.mirror_flags != 0 || hw.bevel_radius > 1e-6
}

/// Hash authoring stack (bake identity when smooth/stack order matters beyond hardwired).
pub fn hash_body_field_stack(records: &[BodyFieldModRecord]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    records.len().hash(&mut h);
    for r in records {
        r.id.hash(&mut h);
        r.enabled.hash(&mut h);
        match &r.kind {
            BodyFieldModKind::VolumeMirror {
                axis_bits,
                sign_bits,
                center,
            } => {
                0u32.hash(&mut h);
                axis_bits.hash(&mut h);
                sign_bits.hash(&mut h);
                center.x.to_bits().hash(&mut h);
                center.y.to_bits().hash(&mut h);
                center.z.to_bits().hash(&mut h);
            }
            BodyFieldModKind::BodyBevel { radius } => {
                1u32.hash(&mut h);
                radius.to_bits().hash(&mut h);
            }
            BodyFieldModKind::BodySmooth {
                mode,
                strength,
                radius,
            } => {
                2u32.hash(&mut h);
                (*mode as u8).hash(&mut h);
                strength.to_bits().hash(&mut h);
                radius.to_bits().hash(&mut h);
            }
            BodyFieldModKind::BodyDisplacement {
                strength,
                frequency,
                seed,
            } => {
                3u32.hash(&mut h);
                strength.to_bits().hash(&mut h);
                frequency.to_bits().hash(&mut h);
                seed.hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Apply mean-curvature-style field smooth given precomputed laplacian / enclosure.
///
/// `h_norm` = dimensionless curvature proxy (laplacian × radius²), typically mild in [-4, 4].
/// `enclosure` in [0, 1] — high when locally occluded / cavitary.
/// `band` in [0, 1] — near-surface weight so far-field stays stable.
#[inline]
pub fn apply_smooth_offset(
    d: f32,
    h_norm: f32,
    enclosure: f32,
    band: f32,
    params: &BodySmoothParams,
) -> f32 {
    if params.strength <= 1e-8 || band <= 1e-6 {
        return d;
    }
    let h = h_norm.clamp(-4.0, 4.0);
    let enc = enclosure.clamp(0.0, 1.0);
    let w = match params.mode {
        BodySmoothMode::Uniform => 1.0,
        BodySmoothMode::Peak => {
            // Convex high-curvature peaks only.
            let convex = h.max(0.0);
            (convex * 0.6).clamp(0.0, 1.0)
        }
        BodySmoothMode::Convex => {
            if h > 1e-4 {
                1.0
            } else {
                0.0
            }
        }
        BodySmoothMode::Concave => {
            if h < -1e-4 {
                1.0
            } else {
                0.0
            }
        }
        BodySmoothMode::Curvature => h.abs().clamp(0.0, 1.0),
        BodySmoothMode::Occlusion => enc,
    };
    if w <= 1e-6 {
        return d;
    }
    // Mean curvature flow on SDF (φ_t = −H): rounds peaks and fills valleys.
    // For Occlusion, still use H so cavities soften rather than pure inflate.
    let flow = match params.mode {
        BodySmoothMode::Occlusion => h * w,
        BodySmoothMode::Peak | BodySmoothMode::Curvature => h * w,
        BodySmoothMode::Convex | BodySmoothMode::Concave | BodySmoothMode::Uniform => h * w,
    };
    d - params.strength * flow * band
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_last_wins() {
        let mut a = BodyFieldModRecord::new_volume_mirror();
        a.kind = BodyFieldModKind::VolumeMirror {
            axis_bits: 1,
            sign_bits: 0,
            center: Vec3::ZERO,
        };
        let mut b = BodyFieldModRecord::new_volume_mirror();
        b.id = 2;
        b.kind = BodyFieldModKind::VolumeMirror {
            axis_bits: 2,
            sign_bits: 1,
            center: Vec3::new(1.0, 0.0, 0.0),
        };
        let hw = resolve_hardwired(&[a, b]);
        assert_eq!(hw.mirror_flags, 2);
        assert_eq!(hw.mirror_sign, 1);
        assert!((hw.mirror_center.x - 1.0).abs() < 1e-5);
    }

    #[test]
    fn disabled_skipped() {
        let mut m = BodyFieldModRecord::new_body_bevel();
        m.enabled = false;
        let hw = resolve_hardwired(&[m]);
        assert_eq!(hw.bevel_radius, 0.0);
    }

    #[test]
    fn seed_from_hardwired_roundtrip() {
        let hw = BodyFieldHardwired {
            mirror_flags: 1 | 4,
            mirror_sign: 1,
            mirror_center: Vec3::new(0.5, 0.0, 0.0),
            bevel_radius: 0.03,
        };
        let stack = seed_from_hardwired(hw);
        assert_eq!(stack.len(), 2);
        let back = resolve_hardwired(&stack);
        assert_eq!(back.mirror_flags, hw.mirror_flags);
        assert_eq!(back.mirror_sign, hw.mirror_sign);
        assert!((back.bevel_radius - 0.03).abs() < 1e-6);
    }

    #[test]
    fn resolve_smooth_last_wins() {
        let mut a = BodyFieldModRecord::new_body_smooth();
        a.kind = BodyFieldModKind::BodySmooth {
            mode: BodySmoothMode::Uniform,
            strength: 0.01,
            radius: 0.02,
        };
        let mut b = BodyFieldModRecord::new_body_smooth();
        b.id = 2;
        b.kind = BodyFieldModKind::BodySmooth {
            mode: BodySmoothMode::Peak,
            strength: 0.05,
            radius: 0.08,
        };
        let s = resolve_smooth(&[a, b]).expect("smooth");
        assert_eq!(s.mode, BodySmoothMode::Peak);
        assert!((s.strength - 0.05).abs() < 1e-6);
        assert!((s.radius - 0.08).abs() < 1e-6);
    }

    #[test]
    fn smooth_zero_strength_clears() {
        let mut a = BodyFieldModRecord::new_body_smooth();
        let mut b = BodyFieldModRecord::new_body_smooth();
        b.id = 2;
        b.kind = BodyFieldModKind::BodySmooth {
            mode: BodySmoothMode::Convex,
            strength: 0.0,
            radius: 0.1,
        };
        assert!(resolve_smooth(&[a.clone(), b]).is_none());
        a.kind = BodyFieldModKind::BodySmooth {
            mode: BodySmoothMode::Convex,
            strength: 0.02,
            radius: 0.1,
        };
        assert!(resolve_smooth(&[a]).is_some());
    }

    #[test]
    fn apply_smooth_uniform_reduces_positive_curvature() {
        let p = BodySmoothParams {
            mode: BodySmoothMode::Uniform,
            strength: 0.1,
            radius: 0.05,
        };
        // Convex peak (h>0): flow lowers d → expands solid / rounds tip (AZRA d-=).
        let d0 = 0.0;
        let d1 = apply_smooth_offset(d0, 1.0, 0.0, 1.0, &p);
        assert!(d1 < d0);
    }

    #[test]
    fn apply_smooth_concave_only_negative_h() {
        let p = BodySmoothParams {
            mode: BodySmoothMode::Concave,
            strength: 0.1,
            radius: 0.05,
        };
        let d_convex = apply_smooth_offset(0.0, 1.0, 0.0, 1.0, &p);
        let d_concave = apply_smooth_offset(0.0, -1.0, 0.0, 1.0, &p);
        assert!((d_convex - 0.0).abs() < 1e-6);
        assert!(d_concave > 0.0); // −strength * (−1) * band → + fills valley
    }

    #[test]
    fn displacement_noise_bounded() {
        for i in 0..32 {
            let p = Vec3::new(i as f32 * 0.37, i as f32 * 0.11, -i as f32 * 0.19);
            let n = displacement_noise3(p, 7);
            assert!(n >= -1.01 && n <= 1.01, "n={n}");
        }
    }

    #[test]
    fn resolve_displacement_last_wins() {
        let mut a = BodyFieldModRecord::new_body_displacement();
        a.kind = BodyFieldModKind::BodyDisplacement {
            strength: 0.01,
            frequency: 4.0,
            seed: 1,
        };
        let mut b = BodyFieldModRecord::new_body_displacement();
        b.id = 2;
        b.kind = BodyFieldModKind::BodyDisplacement {
            strength: 0.05,
            frequency: 12.0,
            seed: 9,
        };
        let d = resolve_displacement(&[a, b]).expect("disp");
        assert!((d.strength - 0.05).abs() < 1e-6);
        assert!((d.frequency - 12.0).abs() < 1e-6);
        assert_eq!(d.seed, 9);
    }
}
