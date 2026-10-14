// core_engine/core_math/src/sdf_math.rs
// SDF math primitives — smooth min variants, noise, scene evaluation
use glam::Vec3;
use crate::primitives::{SdfPrimitive, BooleanOp, SdfShape};
use crate::modifiers::BlendProfile;

// ============================================================
// Smooth min / boolean blend functions
// ============================================================

/// Smooth minimum — used for SmoothUnion boolean
pub fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 1e-6 { return a.min(b); }
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    a * h + b * (1.0 - h) - k * h * (1.0 - h)
}

/// Chamfer minimum — creates a 45° bevel between surfaces
pub fn chamfer_min(a: f32, b: f32, k: f32) -> f32 {
    a.min(b).min((a - k + b) * std::f32::consts::FRAC_1_SQRT_2)
}

/// Cubic smooth minimum — wider, smoother transition than polynomial
pub fn smin_cubic(a: f32, b: f32, k: f32) -> f32 {
    if k <= 1e-6 { return a.min(b); }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * h * k * (1.0 / 6.0)
}

/// Exponential smooth minimum — sharp near edges, soft far
pub fn smin_exp(a: f32, b: f32, k: f32) -> f32 {
    if k <= 1e-6 { return a.min(b); }
    let res = (-k * a).exp() + (-k * b).exp();
    -(res.ln()) / k
}

/// Stairs smooth minimum — terraced/stepped blend
pub fn smin_stairs(a: f32, b: f32, k: f32, steps: f32) -> f32 {
    if k <= 1e-6 { return a.min(b); }
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    let quantized = (h * steps).round() / steps;
    a * quantized + b * (1.0 - quantized) - k * quantized * (1.0 - quantized)
}

/// Circular fillet smin (Inigo Quilez). Soft round join between surfaces.
pub fn smin_circular(a: f32, b: f32, k: f32) -> f32 {
    if k <= 1e-6 {
        return a.min(b);
    }
    let k = k / (1.0 - 0.5f32.sqrt());
    let h = ((k - (a - b).abs()) / k).max(0.0);
    a.min(b) - k * 0.5 * (1.0 + h - (1.0 - h * (h - 2.0)).max(0.0).sqrt())
}

/// Apply smooth min for a blend profile. `shape` (0.25..4) scales K for Curve mode only.
pub fn smin_profiled(a: f32, b: f32, k: f32, profile: BlendProfile) -> f32 {
    smin_profiled_shape(a, b, k, profile, 1.0)
}

/// Polynomial smin with mix-weight warped by a multi-point curve (Curve profile only).
pub fn smin_poly_curved(a: f32, b: f32, k: f32, curve: &crate::modifiers::BlendCurve) -> f32 {
    if k <= 1e-6 {
        return a.min(b);
    }
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    let h = curve.eval(h);
    a * h + b * (1.0 - h) - k * h * (1.0 - h)
}

/// Exact preset math. Shape is unused (kept for API parity with GPU uniforms).
/// Curve/Circular → Smooth (curve feature removed; never worked reliably).
pub fn smin_profiled_shape(a: f32, b: f32, k: f32, profile: BlendProfile, _shape: f32) -> f32 {
    match profile {
        BlendProfile::Chamfer => chamfer_min(a, b, k),
        // Polynomial (Smooth), Circular, Curve → classic poly smin.
        BlendProfile::Polynomial
        | BlendProfile::Circular
        | BlendProfile::Curve => smin(a, b, k),
    }
}

/// Full path: Smooth / Chamfer only. Curve LUT is ignored (feature removed).
pub fn smin_profiled_curve(
    a: f32,
    b: f32,
    k: f32,
    profile: BlendProfile,
    shape: f32,
    _curve: &crate::modifiers::BlendCurve,
) -> f32 {
    // shape unused — kept so call sites / GPU shape slot stay in sync.
    let _ = shape;
    smin_profiled_shape(a, b, k, profile, 1.0)
}

/// Apply smooth max (smooth intersection).
pub fn smax_profiled(a: f32, b: f32, k: f32, profile: BlendProfile) -> f32 {
    -smin_profiled(-a, -b, k, profile)
}

pub fn smax_profiled_shape(a: f32, b: f32, k: f32, profile: BlendProfile, shape: f32) -> f32 {
    -smin_profiled_shape(-a, -b, k, profile, shape)
}

pub fn smax_profiled_curve(
    a: f32,
    b: f32,
    k: f32,
    profile: BlendProfile,
    shape: f32,
    curve: &crate::modifiers::BlendCurve,
) -> f32 {
    -smin_profiled_curve(-a, -b, k, profile, shape, curve)
}

// ============================================================
// SEAM boolean — feature line along the A∩B surface intersection curve
// (hg_sdf fOpGroove/fOpTongue/fOpEngrave family, directional form).
// ============================================================

/// Groove (CUT polarity) of the seam boolean. `a` = accumulator field, `b` = seam prim
/// field, `w` = seam half-width (in b-units), `d` = seam depth (in a-units),
/// `profile`: 0=V (chamfer walls), 1=U (elliptical round bottom), 2=Square (hg_sdf fOpGroove).
///
/// Construction: carve = `max(a, c)` where `c` is a conservative inside-distance of the
/// groove cutter prism running along B's surface. EVERY profile is clamped by the wall
/// term `w - |b|` — this enforces the FAR-B IDENTITY that makes the op cullable:
///
///   c <= w - |b|, so whenever |b| >= w - a we get c <= a and max(a, c) == a EXACTLY.
///   In particular |b| >= w + |a| suffices for BOTH polarities (raised needs |b| >= w + a).
///   The per-prim bounding-sphere cull uses exactly this: skip when
///   sphere_d > width + |acc| (see fold_prim / volume_bake.wgsl / sdf_scene.wgsl).
///
/// All profile terms have gradient magnitude <= 1 in (a, b) space (V is normalized by its
/// slope, U is a scaled-ellipse bound with |grad| <= 1, Square is two axis-aligned planes),
/// so the result stays a safe lower-bound distance for sphere tracing / DC.
#[inline]
pub fn seam_cut(a: f32, b: f32, w: f32, d: f32, profile: u8) -> f32 {
    // Degenerate widths/depths collapse to "no seam" smoothly; guard the divisions.
    let w = w.max(1e-4);
    let d = d.max(1e-4);
    let s = b.abs();
    let wall = w - s;
    let c = match profile {
        // 1 = U: elliptical bottom. m*(1 - |(a/d, b/w)|) has |grad| <= m*max(1/d,1/w) = 1.
        1 => {
            let m = w.min(d);
            m * (1.0 - ((a / d) * (a / d) + (b / w) * (b / w)).sqrt())
        }
        // 2 = Square: flat bottom at depth d, vertical walls at |b| = w (hg_sdf fOpGroove:
        // max(a, min(a + ra, rb - abs(b))) — the wall min below IS the rb-abs(b) term).
        2 => a + d,
        // 0 = V (default): straight wall from (a=0, |b|=w) to the apex (a=-d, b=0),
        // normalized so the plane term is a true signed distance in (a, b) space.
        _ => {
            let slope = d / w;
            (a + d - slope * s) / (1.0 + slope * slope).sqrt()
        }
    };
    a.max(c.min(wall))
}

/// SEAM boolean combine: groove (cut) or bead (raised) along the curve where the seam
/// prim's surface crosses the accumulated surface. Raised is the exact dual of cut:
/// `-seam_cut(-a, b, ...)` (hg_sdf fOpTongue is the dual of fOpGroove).
/// Far-B identity holds for both polarities (see `seam_cut`).
#[inline]
pub fn seam_op(a: f32, b: f32, width: f32, depth: f32, profile: u8, raised: bool) -> f32 {
    if raised {
        -seam_cut(-a, b, width, depth, profile)
    } else {
        seam_cut(a, b, width, depth, profile)
    }
}

// ============================================================
// EXTRUDE boolean — raise/recess the accumulated surface within B's footprint.
// ============================================================

/// EXTRUDE boolean combine. `a` = accumulator field, `b` = extrude prim field,
/// `width` = falloff band (in b-units), `strength` = signed displacement (in a-units):
/// `strength > 0` raises the surface within B's footprint (emboss/extrude out),
/// `strength < 0` recesses it (deboss/extrude in).
///
/// Construction: `a - strength * mask(b)` with `mask(b) = 1 - smoothstep(0, width, b)`
/// — mask is 1 everywhere inside B (b <= 0), eases to 0 over the [0, width] band
/// outside, so deep inside the footprint the surface moves by EXACTLY `strength`.
///
/// FAR-B IDENTITY (what makes the op cullable): `b >= width` ⇒ the smoothstep clamp
/// saturates ⇒ `mask == 0` EXACTLY ⇒ result `== a` bit-for-bit, INDEPENDENT of `a`.
/// The per-prim bounding-sphere cull therefore skips whenever
/// `dist_to_prim > L * width` — no accumulator term needed (unlike Seam's
/// `width + |acc|`). Pinned by `extrude_far_b_is_identity`.
///
/// NOT a bounded-gradient SDF: |∂/∂b mask| <= 1.5/width, so the result's Lipschitz
/// constant can reach 1 + 1.5*|strength|/width inside the band. The GPU bake samples
/// the field directly (no marching) — safe; the analytical march paths keep the
/// ω-backtrack guard (same non-Lipschitz class as smooth-boolean blend zones).
/// The smoothstep is written out (t*t*(3-2t)) float-for-float identically in
/// volume_bake.wgsl / sdf_prim_eval.wgsl / sdf_functions.glsl for bake parity.
#[inline]
pub fn extrude_op(a: f32, b: f32, width: f32, strength: f32) -> f32 {
    let w = width.max(1e-4);
    let t = (b / w).clamp(0.0, 1.0);
    let mask = 1.0 - t * t * (3.0 - 2.0 * t);
    a - strength * mask
}

// ============================================================
// Noise functions
// ============================================================

/// 3D value noise for displacement modifier (hash-based, no texture lookup)
pub fn mask_noise_3d(p: Vec3) -> f32 {
    let i = Vec3::new(p.x.floor(), p.y.floor(), p.z.floor());
    let f = p - i;
    let u = f * f * (Vec3::splat(3.0) - f * 2.0); // smoothstep
    let hash = |x: f32, y: f32, z: f32| -> f32 {
        let n = (x * 127.1 + y * 311.7 + z * 74.7).sin() * 43758.5453;
        n - n.floor()
    };
    let a = hash(i.x, i.y, i.z);
    let b = hash(i.x + 1.0, i.y, i.z);
    let c = hash(i.x, i.y + 1.0, i.z);
    let d = hash(i.x + 1.0, i.y + 1.0, i.z);
    let e = hash(i.x, i.y, i.z + 1.0);
    let f_v = hash(i.x + 1.0, i.y, i.z + 1.0);
    let g = hash(i.x, i.y + 1.0, i.z + 1.0);
    let h = hash(i.x + 1.0, i.y + 1.0, i.z + 1.0);
    let x1 = a + (b - a) * u.x;
    let x2 = c + (d - c) * u.x;
    let x3 = e + (f_v - e) * u.x;
    let x4 = g + (h - g) * u.x;
    let y1 = x1 + (x2 - x1) * u.y;
    let y2 = x3 + (x4 - x3) * u.y;
    y1 + (y2 - y1) * u.z
}

/// Fractal noise (octaved value noise) for richer displacement
pub fn fbm_noise(p: Vec3, octaves: u32) -> f32 {
    let mut val = 0.0f32;
    let mut amp = 0.5;
    let mut freq_p = p;
    for _ in 0..octaves.min(6) {
        val += amp * (mask_noise_3d(freq_p) * 2.0 - 1.0);
        freq_p *= 2.0;
        amp *= 0.5;
    }
    val
}

// ============================================================
// Scene evaluation (free functions for DC / export)
// ============================================================

/// Full k multiplier at `p`: directional blend falloff × protect-walls fade.
/// Matches GPU `compute_local_k` (falloff + lateral wall inset).
#[inline]
pub fn blend_k_multiplier(prim: &SdfPrimitive, p: Vec3) -> f32 {
    let mut m = prim.blend_falloff.evaluate(p, prim.position);
    let margin = prim.blend_falloff.wall_protect;
    if margin > 0.001 {
        m *= wall_protect_multiplier(prim, p, margin);
    }
    m
}

/// Lateral-wall inset factor for Protect Walls. Fades k→0 near **side walls** of
/// the cutter so soft subtracts keep hard vertical planes while the cut interior
/// can still blend.
///
/// Cut depth is the **shortest** half-extent (typical soft cut: wide footprint,
/// shallow depth). Lateral walls are the other two axes. Using the *longest*
/// axis as depth was wrong for flat cutters — side faces never faded K.
///
/// Returns [0,1]: 0 at/outside side walls, 1 when inset ≥ margin.
#[inline]
pub fn wall_protect_multiplier(prim: &SdfPrimitive, p: Vec3, margin: f32) -> f32 {
    use crate::modifiers::BlendFalloff;
    // Local scaled shape space (same frame as shape params / elongation).
    let inv = prim.rotation_quat().inverse();
    let local = inv * (p - prim.position);
    let sc = prim.scale.abs().max(Vec3::splat(1e-4));
    let sp = local / sc;
    // Half-extents of the cutter volume including elongation stretch.
    let he = prim.local_half_extents().max(Vec3::splat(1e-4));
    let axis = prim.blend_falloff.wall_protect_axis;
    let wd = wall_protect_lateral_inset_axis(sp, he, &prim.shape, axis);
    BlendFalloff::wall_protect_factor_curve(
        wd,
        margin,
        prim.blend_falloff.curve_type,
        prim.blend_falloff.curve_power,
    )
}

/// Signed distance (inside +) from `sp` to the nearest **lateral** face of the
/// cutter AABB / radius. Negative = outside past a side wall.
///
/// `axis`: 0=Auto (shortest extent = depth), 1=X walls, 2=Y walls, 3=Z walls.
#[inline]
pub fn wall_protect_lateral_inset(sp: Vec3, he: Vec3, shape: &SdfShape) -> f32 {
    wall_protect_lateral_inset_axis(sp, he, shape, 0)
}

/// Same as [`wall_protect_lateral_inset`] with explicit target wall axis.
#[inline]
pub fn wall_protect_lateral_inset_axis(
    sp: Vec3,
    he: Vec3,
    shape: &SdfShape,
    axis: u32,
) -> f32 {
    match shape {
        // Revolved: side wall is the cylindrical shell (Y = height/depth axis).
        // Axis pick is ignored — radial shell is the only lateral wall.
        SdfShape::Cylinder { .. } | SdfShape::Cone { .. } | SdfShape::Capsule { .. }
        | SdfShape::SpiralStair { .. } => {
            let radial = (sp.x * sp.x + sp.z * sp.z).sqrt();
            he.x - radial
        }
        // Box-like: choose which face pair is "hard wall".
        _ => match axis {
            1 => he.x - sp.x.abs(), // X walls
            2 => he.y - sp.y.abs(), // Y walls
            3 => he.z - sp.z.abs(), // Z walls
            _ => {
                // Auto: cut depth = shortest half-extent; protect the other two faces.
                let (dx, dy, dz) = (he.x, he.y, he.z);
                if dx <= dy && dx <= dz {
                    (he.y - sp.y.abs()).min(he.z - sp.z.abs())
                } else if dy <= dz {
                    (he.x - sp.x.abs()).min(he.z - sp.z.abs())
                } else {
                    (he.x - sp.x.abs()).min(he.y - sp.y.abs())
                }
            }
        },
    }
}

/// Sensible default margin from cutter size: ~35% of the smaller lateral half-extent.
#[inline]
pub fn wall_protect_default_margin(prim: &SdfPrimitive) -> f32 {
    let he = prim.local_half_extents().max(Vec3::splat(1e-4));
    let lateral_min = match &prim.shape {
        SdfShape::Cylinder { .. } | SdfShape::Cone { .. } | SdfShape::Capsule { .. }
        | SdfShape::SpiralStair { .. } => he.x,
        _ => {
            let (dx, dy, dz) = (he.x, he.y, he.z);
            if dx <= dy && dx <= dz {
                he.y.min(he.z)
            } else if dy <= dz {
                he.x.min(he.z)
            } else {
                he.x.min(he.y)
            }
        }
    };
    (lateral_min * 0.35).clamp(0.02, 2.0)
}

/// Fold ONE primitive into a running boolean-chain accumulator — the SINGLE per-prim
/// combine step shared by the flat `eval_scene` and the scope-aware `BodyField` evaluator
/// (`field_eval.rs`). `acc == None` means "scope empty so far": the first prim seeds the
/// accumulator unconditionally (its own op is ignored — the GPU `acc > 998.0` first-prim
/// rule) and is NEVER culled (RAY_CULL first-prim guard).
///
/// With `acc == Some(dist)` this is byte-for-byte the historical `eval_scene` loop body:
/// bounding-sphere cull (Union/Subtract families only; Intersect never far-skips), then the
/// blend-falloff-modulated boolean combine.
#[inline]
pub fn fold_prim(acc: Option<f32>, prim: &SdfPrimitive, p: Vec3) -> f32 {
    let dist = match acc {
        None => return prim.world_distance(p),
        Some(dist) => dist,
    };
    // Per-prim bounding sphere cull: skip if prim's influence sphere can't reach this point.
    // bounding_radius() includes shape extent + scale + blend k + modifier padding.
    let sphere_d = (p - prim.position).length() - prim.bounding_radius();
    if sphere_d > 0.0 {
        let k = match &prim.operation {
            BooleanOp::Union { k } | BooleanOp::Subtract { k }
            | BooleanOp::ChamferUnion { k } | BooleanOp::ChamferSubtract { k }
            | BooleanOp::SmoothIntersect { k } => *k,
            BooleanOp::Intersect => 0.0,
            BooleanOp::Seam { width, .. } => *width,
            BooleanOp::Extrude { width, .. } => *width,
        };
        match &prim.operation {
            BooleanOp::Union { .. } | BooleanOp::ChamferUnion { .. } => {
                if sphere_d > dist + k { return dist; }
            }
            BooleanOp::Subtract { .. } | BooleanOp::ChamferSubtract { .. } => {
                if dist >= 0.0 && sphere_d > k { return dist; }
            }
            // Intersect / SmoothIntersect: NEVER far-skip. dist.max(d) needs a point outside
            // the cutter's bounding sphere to yield d (large positive = empty), never acc.
            // Skipping it retains the un-intersected surface = stale geometry. Matches the
            // analytical paths (sdf_scene.wgsl / glsl skip only Union/ChamferUnion).
            // See SDF_TOOLS_PLAN Item 1.
            BooleanOp::Intersect | BooleanOp::SmoothIntersect { .. } => {}
            // Seam: EXACT far-skip. seam_op(dist, b, w, ..) == dist whenever |b| >= w + |dist|
            // (wall clamp in seam_cut, both polarities — see the seam_cut proof). Here
            // b >= sphere_d > 0, so sphere_d > w + |dist| guarantees the identity.
            // Mirrored in volume_bake.wgsl / sdf_scene.wgsl / sdf_functions.glsl.
            BooleanOp::Seam { .. } => {
                if sphere_d > k + dist.abs() { return dist; }
            }
            // Extrude: EXACT far-skip, independent of acc. extrude_op(dist, b, w, s) == dist
            // whenever b >= w (smoothstep mask saturates to 0). Here b >= sphere_d > 0, so
            // sphere_d > w guarantees the identity. Mirrored in volume_bake.wgsl /
            // sdf_scene.wgsl / sdf_functions.glsl.
            BooleanOp::Extrude { .. } => {
                if sphere_d > k { return dist; }
            }
        }
    }
    let d = prim.world_distance(p);
    let fm = blend_k_multiplier(prim, p);
    let shape = prim.blend_profile_shape;
    let curve = &prim.blend_curve;
    match &prim.operation {
        BooleanOp::Union    { k } => { let kl = *k * fm; if kl > 0.0 { smin_profiled_curve(dist, d, kl, prim.blend_profile, shape, curve) } else { dist.min(d) } }
        BooleanOp::Subtract { k } => { let kl = *k * fm; if kl > 0.0 { -smin_profiled_curve(-dist, d, kl, prim.blend_profile, shape, curve) } else { dist.max(-d) } }
        BooleanOp::Intersect          => dist.max(d),
        BooleanOp::SmoothIntersect { k } => { let kl = *k * fm; if kl > 0.0 { smax_profiled_curve(dist, d, kl, prim.blend_profile, shape, curve) } else { dist.max(d) } }
        BooleanOp::ChamferUnion    { k } => { let kl = *k * fm; if kl > 0.0 { smin_profiled_curve(dist, d, kl, BlendProfile::Chamfer, shape, curve) } else { dist.min(d) } }
        BooleanOp::ChamferSubtract { k } => { let kl = *k * fm; if kl > 0.0 { -smin_profiled_curve(-dist, d, kl, BlendProfile::Chamfer, shape, curve) } else { dist.max(-d) } }
        // Seam: blend falloff modulates the WIDTH (like k elsewhere — GPU compute_local_k
        // scales the k slot, which carries width for op 5/6). Depth is NOT modulated.
        BooleanOp::Seam { width, depth, profile, raised } => {
            seam_op(dist, d, *width * fm, *depth, *profile, *raised)
        }
        // Extrude: blend falloff modulates the WIDTH (like k elsewhere — GPU compute_local_k
        // scales the k slot, which carries width for op 7). Strength is NOT modulated.
        BooleanOp::Extrude { strength, width } => {
            extrude_op(dist, d, *width * fm, *strength)
        }
    }
}

/// Combine a CHILD SCOPE's field into a parent accumulator via the child body's `body_op` —
/// the CPU mirror of `apply_boolean()` in `volume_bake.wgsl` (scope-level combine: no blend
/// falloff, no blend noise; Chamfer* ops force the Chamfer profile; SmoothIntersect maps to
/// op 2 with its k, exactly the serializer's `emit_scope_open_hot` op table).
#[inline]
pub fn apply_scope_boolean(parent: f32, child: f32, op: &BooleanOp, bp: BlendProfile) -> f32 {
    match op {
        BooleanOp::Union    { k } => if *k > 0.0 { smin_profiled(parent, child, *k, bp) } else { parent.min(child) },
        BooleanOp::Subtract { k } => if *k > 0.0 { -smin_profiled(-parent, child, *k, bp) } else { parent.max(-child) },
        BooleanOp::Intersect          => parent.max(child),
        BooleanOp::SmoothIntersect { k } => if *k > 0.0 { smax_profiled(parent, child, *k, bp) } else { parent.max(child) },
        BooleanOp::ChamferUnion    { k } => if *k > 0.0 { smin_profiled(parent, child, *k, BlendProfile::Chamfer) } else { parent.min(child) },
        BooleanOp::ChamferSubtract { k } => if *k > 0.0 { -smin_profiled(-parent, child, *k, BlendProfile::Chamfer) } else { parent.max(-child) },
        // Seam sub-body: the child scope's field is the seam driver (GPU op 5/6 at SCOPE_CLOSE).
        BooleanOp::Seam { width, depth, profile, raised } =>
            seam_op(parent, child, *width, *depth, *profile, *raised),
        // Extrude sub-body: the child scope's field is the footprint driver (GPU op 7 at SCOPE_CLOSE).
        BooleanOp::Extrude { strength, width } =>
            extrude_op(parent, child, *width, *strength),
    }
}

/// Zero-alloc SDF evaluation over a pre-filtered visible slice (FLAT — one body's own prims).
/// Use this in hot loops (baking, dual contouring) — build `visible` once, call N times.
/// Nested sub-body scopes are handled by `field_eval::BodyField`, which folds prims through
/// the SAME `fold_prim` step — the flat case there is this exact code, not a special case.
#[inline]
pub fn eval_scene(p: Vec3, visible: &[&SdfPrimitive]) -> f32 {
    if visible.is_empty() { return 100.0; }
    let mut dist = fold_prim(None, visible[0], p);
    for prim in visible.iter().skip(1) {
        dist = fold_prim(Some(dist), prim, p);
    }
    dist
}

/// Zero-alloc normal via central differences — use alongside eval_scene in hot loops.
#[inline]
pub fn eval_normal(p: Vec3, visible: &[&SdfPrimitive], eps: f32) -> Vec3 {
    let dx = eval_scene(p + Vec3::new(eps, 0.0, 0.0), visible)
           - eval_scene(p - Vec3::new(eps, 0.0, 0.0), visible);
    let dy = eval_scene(p + Vec3::new(0.0, eps, 0.0), visible)
           - eval_scene(p - Vec3::new(0.0, eps, 0.0), visible);
    let dz = eval_scene(p + Vec3::new(0.0, 0.0, eps), visible)
           - eval_scene(p - Vec3::new(0.0, 0.0, eps), visible);
    Vec3::new(dx, dy, dz).normalize_or_zero()
}

/// Evaluate full scene SDF at a world-space point (all visible primitives + boolean ops).
/// For hot loops, prefer `eval_scene` with a pre-built visible slice to avoid per-call allocation.
pub fn scene_sdf(p: Vec3, primitives: &[SdfPrimitive]) -> f32 {
    let visible: Vec<&SdfPrimitive> = primitives.iter().filter(|pr| pr.visible).collect();
    eval_scene(p, &visible)
}

/// Compute surface normal via central differences on the scene SDF
pub fn scene_normal(p: Vec3, primitives: &[SdfPrimitive], eps: f32) -> Vec3 {
    let visible: Vec<&SdfPrimitive> = primitives.iter().filter(|pr| pr.visible).collect();
    let dx = eval_scene(p + Vec3::new(eps, 0.0, 0.0), &visible)
           - eval_scene(p - Vec3::new(eps, 0.0, 0.0), &visible);
    let dy = eval_scene(p + Vec3::new(0.0, eps, 0.0), &visible)
           - eval_scene(p - Vec3::new(0.0, eps, 0.0), &visible);
    let dz = eval_scene(p + Vec3::new(0.0, 0.0, eps), &visible)
           - eval_scene(p - Vec3::new(0.0, 0.0, eps), &visible);
    Vec3::new(dx, dy, dz).normalize_or_zero()
}

#[cfg(test)]
mod seam_tests {
    use super::*;
    use crate::primitives::{SdfShape, SEAM_PROFILE_V, SEAM_PROFILE_U, SEAM_PROFILE_SQUARE};

    const PROFILES: [u8; 3] = [SEAM_PROFILE_V, SEAM_PROFILE_U, SEAM_PROFILE_SQUARE];
    const W: f32 = 0.1;
    const D: f32 = 0.05;

    /// THE CULL PROOF as a test: far from the seam prim's surface (|b| >= width + |a|)
    /// the seam op returns the accumulator EXACTLY, for every profile and both polarities.
    /// This is what makes the per-prim bounding-sphere far-skip (fold_prim /
    /// volume_bake.wgsl / sdf_scene.wgsl: skip when sphere_d > width + |acc|) exact.
    #[test]
    fn seam_far_b_is_identity() {
        for &prof in &PROFILES {
            for &raised in &[false, true] {
                for &a in &[-2.0f32, -0.5, -0.06, 0.0, 0.03, 0.5, 2.0] {
                    // Margin: the identity region is |b| >= w + |a| in real arithmetic; under
                    // f32 the EXACT boundary can flip by 1 ulp, so the guarantee is stated
                    // with any positive margin. The cull provides this structurally:
                    // sphere_d underestimates |b| by the bounding pad (>= width + depth).
                    for &off in &[1e-3f32, 0.01, 1.0, 100.0] {
                        let bmag = W + a.abs() + off;
                        for &b in &[bmag, -bmag] {
                            let r = seam_op(a, b, W, D, prof, raised);
                            assert_eq!(r, a,
                                "seam must be identity far from B: prof={prof} raised={raised} a={a} b={b}");
                        }
                    }
                }
            }
        }
    }

    /// Cross-section sanity along the seam center plane (b = 0):
    /// the groove floor sits at depth D (field 0 there), the old surface point is carved
    /// into air (field > 0), and below the floor stays solid.
    #[test]
    fn seam_cut_cross_section_depth() {
        for &prof in &PROFILES {
            // Old surface point (a=0) at seam center is carved away.
            assert!(seam_op(0.0, 0.0, W, D, prof, false) > 0.0, "prof={prof} surface not carved");
            // Halfway down is still air.
            assert!(seam_op(-D * 0.5, 0.0, W, D, prof, false) > 0.0, "prof={prof} groove too shallow");
            // Groove floor: field ~0 at depth D.
            let floor = seam_op(-D, 0.0, W, D, prof, false);
            assert!(floor.abs() < 1e-4, "prof={prof} floor at wrong depth: {floor}");
            // Below the floor: solid again.
            assert!(seam_op(-D - 0.02, 0.0, W, D, prof, false) < 0.0, "prof={prof} cut too deep");
            // Outside the half-width (at the surface): untouched.
            assert_eq!(seam_op(0.0, W + 1e-3, W, D, prof, false), 0.0, "prof={prof} groove leaks past width");
        }
    }

    /// Raised polarity: adds a bead of height D on the surface — exact dual of the cut.
    /// Also: cut only ever REMOVES material (result >= a), raised only ever ADDS (result <= a).
    #[test]
    fn seam_raised_adds_material() {
        for &prof in &PROFILES {
            // Inside the bead (above the old surface at seam center).
            assert!(seam_op(D * 0.5, 0.0, W, D, prof, true) < 0.0, "prof={prof} bead missing");
            // Bead crest: field ~0 at height D.
            let crest = seam_op(D, 0.0, W, D, prof, true);
            assert!(crest.abs() < 1e-4, "prof={prof} crest at wrong height: {crest}");
            // Above the crest: air.
            assert!(seam_op(D + 0.02, 0.0, W, D, prof, true) > 0.0, "prof={prof} bead too tall");
            // Monotonicity of the polarities over an (a, b) grid.
            for ai in -10..=10 {
                for bi in -10..=10 {
                    let a = ai as f32 * 0.03;
                    let b = bi as f32 * 0.03;
                    assert!(seam_op(a, b, W, D, prof, false) >= a, "cut must not add material");
                    assert!(seam_op(a, b, W, D, prof, true) <= a, "raised must not remove material");
                }
            }
        }
    }

    /// The scope-level combine (sub-body with a Seam body_op) dispatches the same math.
    #[test]
    fn seam_scope_combine_matches_prim_combine() {
        let op = BooleanOp::Seam { width: W, depth: D, profile: SEAM_PROFILE_U, raised: false };
        for &(a, b) in &[(0.0f32, 0.0f32), (-0.03, 0.02), (0.5, 0.2), (-1.0, 5.0)] {
            let via_scope = apply_scope_boolean(a, b, &op, BlendProfile::Polynomial);
            let direct = seam_op(a, b, W, D, SEAM_PROFILE_U, false);
            assert_eq!(via_scope, direct);
        }
    }

    /// fold_prim's bounding-sphere far-skip for Seam is EXACT: culled and forced-eval
    /// paths agree bit-for-bit when the prim is beyond width + |acc| of the point.
    #[test]
    fn fold_prim_seam_cull_is_exact() {
        let mut prim = SdfPrimitive::new("seam".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::new(10.0, 0.0, 0.0));
        prim.operation = BooleanOp::Seam { width: W, depth: D, profile: SEAM_PROFILE_V, raised: false };
        for &acc in &[-1.5f32, -0.2, 0.0, 0.3, 2.0] {
            let p = Vec3::ZERO;
            let folded = fold_prim(Some(acc), &prim, p);
            // The skip must return the accumulator unchanged...
            assert_eq!(folded, acc, "cull returned a changed accumulator");
            // ...and that IS the exact seam result at this distance (identity region).
            let direct = seam_op(acc, prim.world_distance(p), W, D, SEAM_PROFILE_V, false);
            assert_eq!(direct, acc, "identity region violated — cull would be lossy");
        }
    }
}

#[cfg(test)]
mod wall_protect_tests {
    use super::*;
    use crate::modifiers::BlendFalloff;
    use crate::primitives::SdfShape;
    use crate::sdf_shapes::BoxBevel;

    fn box_cutter(he: Vec3) -> SdfPrimitive {
        SdfPrimitive::new(
            "cut".into(),
            SdfShape::Box {
                half_extents: he,
                bevel: BoxBevel::default(),
            },
            Vec3::ZERO,
        )
    }

    /// Wide shallow cut: depth is Y (shortest). Center must keep full K; near
    /// X/Z side wall K must die. Old "longest-axis = depth" failed this case.
    #[test]
    fn shallow_box_cut_protects_xz_sides() {
        let mut prim = box_cutter(Vec3::new(1.0, 0.12, 0.8));
        prim.blend_falloff.wall_protect = 0.2;
        prim.operation = BooleanOp::Subtract { k: 0.25 };

        // Center of cutter — deep inset from X and Z walls
        let m_center = wall_protect_multiplier(&prim, Vec3::ZERO, 0.2);
        assert!(
            m_center > 0.95,
            "center must keep full blend, got {m_center}"
        );

        // Near +X side wall (inside)
        let m_wall = wall_protect_multiplier(&prim, Vec3::new(0.95, 0.0, 0.0), 0.2);
        assert!(
            m_wall < 0.35,
            "near side wall must fade K, got {m_wall}"
        );

        // Outside past +X wall — hard
        let m_out = wall_protect_multiplier(&prim, Vec3::new(1.2, 0.0, 0.0), 0.2);
        assert!(
            m_out < 0.01,
            "outside side wall must kill blend, got {m_out}"
        );

        // Lateral inset uses shortest (Y) as depth — X/Z are walls
        let he = Vec3::new(1.0, 0.12, 0.8);
        let inset_side = wall_protect_lateral_inset(
            Vec3::new(0.95, 0.0, 0.0),
            he,
            &prim.shape,
        );
        assert!((inset_side - 0.05).abs() < 1e-5, "inset={inset_side}");
    }

    #[test]
    fn blend_k_multiplier_stacks_with_protect() {
        let mut prim = box_cutter(Vec3::new(0.5, 0.1, 0.5));
        prim.operation = BooleanOp::Subtract { k: 0.3 };
        prim.blend_falloff.wall_protect = 0.15;
        // No directional falloff
        assert!((blend_k_multiplier(&prim, Vec3::ZERO) - 1.0).abs() < 0.05);
        let near = blend_k_multiplier(&prim, Vec3::new(0.48, 0.0, 0.0));
        assert!(near < 0.5, "near wall k mult={near}");
    }

    #[test]
    fn wall_protect_axis_selects_face() {
        // Tall thin box: Auto protects X/Z (depth=Y). Force Z walls only.
        let he = Vec3::new(1.0, 0.2, 0.5);
        let sp_z = Vec3::new(0.0, 0.0, 0.48);
        let sp_x = Vec3::new(0.95, 0.0, 0.0);
        let inz = wall_protect_lateral_inset_axis(sp_z, he, &SdfShape::Box {
            half_extents: he,
            bevel: Default::default(),
        }, 3);
        let inx = wall_protect_lateral_inset_axis(sp_x, he, &SdfShape::Box {
            half_extents: he,
            bevel: Default::default(),
        }, 3);
        // Near Z wall → small inset; near X wall with Z-only protect → large inset (safe)
        assert!(inz < 0.05, "Z wall inset={inz}");
        // On X face but measuring Z-wall inset → full he.z (=0.5) remaining
        assert!((inx - 0.5).abs() < 0.01, "X sample under Z-only protect inset={inx}");
    }

    #[test]
    fn default_margin_scales_with_cutter() {
        let small = box_cutter(Vec3::splat(0.1));
        let big = box_cutter(Vec3::new(2.0, 0.3, 2.0));
        let ms = wall_protect_default_margin(&small);
        let mb = wall_protect_default_margin(&big);
        assert!(ms < mb, "larger cutter → larger default margin ({ms} vs {mb})");
        assert!(ms >= 0.02 && mb <= 2.0);
    }

    #[test]
    fn wall_protect_factor_smoothstep_bounds() {
        assert_eq!(BlendFalloff::wall_protect_factor(-1.0, 0.2), 0.0);
        assert_eq!(BlendFalloff::wall_protect_factor(0.0, 0.2), 0.0);
        assert!((BlendFalloff::wall_protect_factor(0.2, 0.2) - 1.0).abs() < 1e-5);
        assert!((BlendFalloff::wall_protect_factor(1.0, 0.2) - 1.0).abs() < 1e-5);
        let mid = BlendFalloff::wall_protect_factor(0.1, 0.2);
        assert!(mid > 0.2 && mid < 0.8, "mid={mid}");
    }
}

#[cfg(test)]
mod extrude_tests {
    use super::*;
    use crate::primitives::SdfShape;

    const W: f32 = 0.1;
    const S: f32 = 0.06;

    /// THE CULL PROOF as a test: beyond `width` of the extrude prim's surface (b >= width)
    /// the op returns the accumulator EXACTLY, for both signs, INDEPENDENT of the
    /// accumulator value. This is what makes the per-prim far-skip
    /// (fold_prim / volume_bake.wgsl / sdf_scene.wgsl: skip when dist_to_prim > L*width) exact.
    #[test]
    fn extrude_far_b_is_identity() {
        for &s in &[S, -S, 0.4, -0.4] {
            for &a in &[-2.0f32, -0.5, -0.06, 0.0, 0.03, 0.5, 2.0] {
                for &off in &[0.0f32, 1e-3, 0.01, 1.0, 100.0] {
                    let b = W + off; // at and beyond the band edge — clamp saturates exactly
                    let r = extrude_op(a, b, W, s);
                    assert_eq!(r, a, "extrude must be identity beyond width: s={s} a={a} b={b}");
                }
            }
        }
    }

    /// Cross-section probes, both signs. Deep inside the footprint (b <= 0) the field is
    /// displaced by EXACTLY strength: raise (s>0) ADDS material (old surface point becomes
    /// interior; new surface sits `s` above it), recess (s<0) REMOVES material (old surface
    /// point becomes exterior at distance |s|; new surface sits |s| below).
    #[test]
    fn extrude_raise_and_recess_cross_section() {
        // -- Raise (s = +S): emboss out --
        // Old surface point (a=0) deep inside footprint → interior by exactly S.
        assert_eq!(extrude_op(0.0, -0.5, W, S), -S, "raise must lower the field by strength");
        // New surface: old exterior point at height a=S → exactly on the new surface.
        assert!(extrude_op(S, -0.5, W, S).abs() < 1e-6, "raised surface must sit at a=+strength");
        // Above the new surface: still air.
        assert!(extrude_op(S + 0.02, -0.5, W, S) > 0.0, "raise too tall");
        // -- Recess (s = -S): deboss in --
        assert_eq!(extrude_op(0.0, -0.5, W, -S), S, "recess must raise the field by |strength|");
        // New surface: old interior point at depth a=-S → exactly on the new surface.
        assert!(extrude_op(-S, -0.5, W, -S).abs() < 1e-6, "recessed surface must sit at a=-|strength|");
        // Below the new floor: still solid.
        assert!(extrude_op(-S - 0.02, -0.5, W, -S) < 0.0, "recess too deep");
        // Monotonicity over an (a, b) grid: raise only ever ADDS material (result <= a),
        // recess only ever REMOVES (result >= a); displacement magnitude <= |strength|.
        for ai in -10..=10 {
            for bi in -10..=10 {
                let a = ai as f32 * 0.03;
                let b = bi as f32 * 0.03;
                let up = extrude_op(a, b, W, S);
                let dn = extrude_op(a, b, W, -S);
                assert!(up <= a && a - up <= S + 1e-6, "raise out of bounds at a={a} b={b}");
                assert!(dn >= a && dn - a <= S + 1e-6, "recess out of bounds at a={a} b={b}");
            }
        }
    }

    /// The scope-level combine (sub-body with an Extrude body_op) dispatches the same math.
    #[test]
    fn extrude_scope_combine_matches_prim_combine() {
        for &s in &[S, -S] {
            let op = BooleanOp::Extrude { strength: s, width: W };
            for &(a, b) in &[(0.0f32, 0.0f32), (-0.03, 0.02), (0.5, 0.2), (-1.0, 5.0), (0.1, -0.3)] {
                let via_scope = apply_scope_boolean(a, b, &op, BlendProfile::Polynomial);
                let direct = extrude_op(a, b, W, s);
                assert_eq!(via_scope, direct);
            }
        }
    }

    /// fold_prim's bounding-sphere far-skip for Extrude is EXACT: culled and forced-eval
    /// paths agree bit-for-bit when the prim is beyond `width` of the point — for ANY
    /// accumulator value (the identity is independent of acc, unlike Seam's).
    #[test]
    fn fold_prim_extrude_cull_is_exact() {
        for &s in &[S, -S] {
            let mut prim = SdfPrimitive::new("ext".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::new(10.0, 0.0, 0.0));
            prim.operation = BooleanOp::Extrude { strength: s, width: W };
            for &acc in &[-1.5f32, -0.2, 0.0, 0.3, 2.0] {
                let p = Vec3::ZERO;
                let folded = fold_prim(Some(acc), &prim, p);
                assert_eq!(folded, acc, "cull returned a changed accumulator (s={s})");
                let direct = extrude_op(acc, prim.world_distance(p), W, s);
                assert_eq!(direct, acc, "identity region violated — cull would be lossy (s={s})");
            }
        }
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    use crate::modifiers::{BlendCurve, BlendCurveKnot, BlendProfile};

    #[test]
    fn smooth_ignores_curve_matches_plain_smin() {
        let a = 0.1f32;
        let b = -0.05f32;
        let k = 0.2f32;
        // Warped curve would change result if applied
        let mut warped = BlendCurve::default();
        warped.points = vec![
            BlendCurveKnot::soft_xy(0.0, 0.0),
            BlendCurveKnot::soft_xy(0.5, 0.9),
            BlendCurveKnot::soft_xy(1.0, 1.0),
        ];
        let plain = smin(a, b, k);
        let via = smin_profiled_curve(a, b, k, BlendProfile::Polynomial, 2.0, &warped);
        assert!((plain - via).abs() < 1e-6, "Smooth must be exact smin, got {via} vs {plain}");
    }

    #[test]
    fn chamfer_ignores_curve() {
        let a = 0.1f32;
        let b = -0.05f32;
        let k = 0.2f32;
        let mut warped = BlendCurve::default();
        warped.points = vec![
            BlendCurveKnot::soft_xy(0.0, 0.0),
            BlendCurveKnot::soft_xy(0.5, 0.1),
            BlendCurveKnot::soft_xy(1.0, 1.0),
        ];
        let plain = chamfer_min(a, b, k);
        let via = smin_profiled_curve(a, b, k, BlendProfile::Chamfer, 3.0, &warped);
        assert!((plain - via).abs() < 1e-6, "Chamfer must be exact, got {via} vs {plain}");
    }

    #[test]
    fn legacy_curve_tag_maps_to_smooth() {
        // Curve feature removed: saved Curve/Circular tags must evaluate as plain smin.
        let a = 0.1f32;
        let b = -0.05f32;
        let k = 0.2f32;
        let mut warped = BlendCurve::default();
        warped.points = vec![
            BlendCurveKnot::soft_xy(0.0, 0.0),
            BlendCurveKnot::soft_xy(0.5, 0.9),
            BlendCurveKnot::soft_xy(1.0, 1.0),
        ];
        let plain = smin(a, b, k);
        for p in [BlendProfile::Curve, BlendProfile::Circular] {
            let via = smin_profiled_curve(a, b, k, p, 2.0, &warped);
            assert!(
                (plain - via).abs() < 1e-6,
                "{p:?} must map to plain smin, got {via} vs {plain}"
            );
        }
    }
}
