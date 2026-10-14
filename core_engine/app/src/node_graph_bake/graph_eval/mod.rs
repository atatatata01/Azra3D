//! CPU node-graph evaluation + SDF field helpers for bake sampling.
//!
//! Pure relocation from monolithic node_graph_bake — no logic change.
//! Node `eval_node` + projection/noise live in `eval_nodes.rs`.

mod eval_nodes;
use eval_nodes::eval_node;

use super::{ImageMapSources, SampledMat, BAKE_MAX_OCTAVES, MAX_CURV_KEYS_PER_BAKE};
use crate::shader_nodes::{Node, NodeGraph, NodeId, NodeType, SocketType};
use glam::Vec3;
use std::collections::HashMap;


/// Distance returned by [`SdfFieldSources::sdf_at`] when the body owning this bake has **no
/// analytical field at all** (a polygonal body, or a hand-built graph with no body resolved).
///
/// Large, positive and FINITE, chosen so every downstream consumer degrades to "outside / no
/// data" by ordinary arithmetic rather than by a special case:
/// * `project_to_surface` — `d.abs()` never drops under `accept`, so it returns `None` and every
///   seed-based analyzer skips the sample. Fail closed.
/// * `calc_curvature` — the 4 taps and `4.0 * d` cancel exactly, so κ = 0 (never NaN).
/// * `sdf_to_mask` — a huge positive `dd` saturates the softstep to 0.
///
/// Finite matters: `f32::INFINITY` would make those same expressions `inf - inf = NaN` and paint
/// garbage instead of nothing. This constant replaces the old `sn_field_h` analytic fallback,
/// which returned a plausible-looking procedural pattern with no relationship to any geometry —
/// the "false result that looks like a feature" class CLAUDE.md § *No Hacks* forbids.
pub(crate) const NO_FIELD_D: f32 = 1.0e6;

/// Body / prim SDF snapshot for PrimSdfMask + SdfCurvature (body-local space = bake domain).
#[derive(Clone, Default)]
pub(crate) struct SdfFieldSources {
    /// All prims of the body owning this material (for body-field curvature).
    pub body_prims: Vec<core_math::primitives::SdfPrimitive>,
    /// Lookup by `prim_uid` for PrimSdfMask — **scene-wide**, so cross-body picks resolve.
    ///
    /// Because it is scene-wide it must NEVER be read as evidence that *this* body has a field.
    /// [`Self::has_field`] used to be `!body_prims.is_empty() || !by_uid.is_empty()`, so a
    /// POLYGONAL body in a scene that merely contained an SDF body somewhere reported "has
    /// field", then measured — and bounded itself against — *that other body's* geometry. A
    /// poly-only scene and a mixed scene disagreed about what "no field" meant. Same rule in
    /// [`Self::body_aabb`], which no longer falls through to these values.
    pub by_uid: HashMap<u32, core_math::primitives::SdfPrimitive>,
    /// GPU CURVATURE (`GPU_CURVATURE_KERNEL_PLAN.md` P1): `res²` curvature grids precomputed by
    /// the `curvature_eval` kernel in the poll phase, before this snapshot was handed to the
    /// worker — ONE PER DISTINCT knob set the graph's curvature/cavity nodes use (2026-07-27,
    /// was a single `Option<CurvGrid>`: a material with two Generator layers at different radii
    /// has two distinct `CurvKey`s, and the single-grid design fell back the WHOLE bake to the
    /// CPU path the instant it saw a second disagreeing key — every sample in the graph, not just
    /// the second node's — which was the owner's ~45s bug). For each sample, `sdf_curvature_masks`
    /// looks up the grid whose key matches THAT node's own knobs; a node whose key has no
    /// matching grid (still) falls back to the CPU path for that node alone. The worker thread
    /// never touches wgpu; it receives finished data. Bounded by `MAX_CURV_KEYS_PER_BAKE`.
    pub curv_caches: Vec<crate::gpu_curvature::CurvGrid>,
    /// BOTH MODES Phase 3: `res²` grids built by measuring curvature on **surface positions**
    /// (volume-stratified seeds → `project_to_surface` → κ) then **splatting** into the planar
    /// bake domain. Phase 2 used AABB-face seeds only (blind to concavities). Used only by
    /// Vertex-tagged nodes (`sgv`/…). Planar Texture-rung nodes keep reading `curv_caches`
    /// (SEED_PLANAR). Same shaping as [`sdf_curvature_masks`]; different measurement domain.
    pub vertex_curv_caches: Vec<crate::gpu_curvature::CurvGrid>,
    /// POLY CURVATURE (`POLY_CURVATURE_PLAN.md` P5): the active body's post-modifier display mesh,
    /// when that body is **polygonal**. `None` for an SDF body.
    ///
    /// A poly body has no analytical field, so [`Self::has_field`] is false and every field
    /// analyzer fails closed — correctly. Curvature is the one signal it CAN still provide, by
    /// measurement rather than differentiation, and `prepare_poly_curv_caches` turns this snapshot
    /// into ordinary [`Self::curv_caches`] entries. Everything downstream of that is the SDF path,
    /// unchanged and unaware.
    pub poly_curv: Option<crate::poly_curvature::PolyCurvSource>,
}

/// Interior thickness 0..1 (1 = thick/open, 0 = thin shell). Steps along −normal into the body.
pub(crate) fn sdf_thickness(
    sdf: &SdfFieldSources,
    p: [f32; 3],
    n: [f32; 3],
    prim_uid: u32,
    max_d: f32,
) -> f32 {
    if !sdf.has_field() {
        return 1.0;
    }
    let max_d = max_d.max(1e-4);
    // Recover surface normal when bake-domain n is flat [0,0,1].
    let (sp, nn) = if is_planar_bake_domain(p) {
        let (bmin, bmax) = sdf.body_aabb(prim_uid);
        let c = crate::gpu_curvature::CurvConstants::new(bmin, bmax, 0.02, 0.2);
        let ext = (bmax - bmin).max(Vec3::splat(1e-4));
        let u = ((p[0] + 1.0) * 0.5).clamp(0.0, 1.0);
        let v = ((p[1] + 1.0) * 0.5).clamp(0.0, 1.0);
        let center = (bmin + bmax) * 0.5;
        let seeds = [
            [bmin.x + u * ext.x, bmin.y + v * ext.y, center.z],
            [bmin.x + u * ext.x, center.y, bmin.z + v * ext.z],
            [center.x, bmin.y + u * ext.y, bmin.z + v * ext.z],
        ];
        let mut best_sp = None;
        let mut best_n = [0.0, 0.0, 1.0];
        let mut best_d = f32::INFINITY;
        for seed in seeds {
            if let Some(ps) = sdf.project_to_surface(seed, prim_uid, 0.0, c.accept) {
                let d = sdf.sdf_at(ps, prim_uid).abs();
                if d < best_d {
                    best_d = d;
                    let g = sdf.sdf_grad(ps, prim_uid, c.e * 0.5);
                    let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-6);
                    best_n = [g[0] / gl, g[1] / gl, g[2] / gl];
                    best_sp = Some(ps);
                }
            }
        }
        match best_sp {
            Some(ps) => (ps, best_n),
            None => return 1.0,
        }
    } else {
        let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let nn = if nl > 1e-6 {
            [n[0] / nl, n[1] / nl, n[2] / nl]
        } else {
            [0.0, 0.0, 1.0]
        };
        (p, nn)
    };
    // IQ-style: accumulate how much free space is *less* than free-space expectation inward.
    const SAMPLES: u32 = 5;
    let mut th = 0.0f32;
    let mut scale = 1.0f32;
    for i in 1..=SAMPLES {
        let h = max_d * (i as f32) / (SAMPLES as f32);
        let q = [
            sp[0] - nn[0] * h,
            sp[1] - nn[1] * h,
            sp[2] - nn[2] * h,
        ];
        let d = sdf.sdf_at(q, prim_uid).abs();
        th += (h - d).max(0.0) * scale;
        scale *= 0.75;
    }
    (1.0 - th / max_d).clamp(0.0, 1.0)
}

/// Body-local position as 0..1 per axis (AABB-normalized). Planar bake domain remaps via triplanar seed.
pub(crate) fn sdf_position_01(
    sdf: &SdfFieldSources,
    p: [f32; 3],
    prim_uid: u32,
) -> (f32, f32, f32) {
    let (bmin, bmax) = sdf.body_aabb(prim_uid);
    let ext = (bmax - bmin).max(Vec3::splat(1e-4));
    let world = if is_planar_bake_domain(p) && sdf.has_field() {
        let c = crate::gpu_curvature::CurvConstants::new(bmin, bmax, 0.02, 0.2);
        let u = ((p[0] + 1.0) * 0.5).clamp(0.0, 1.0);
        let v = ((p[1] + 1.0) * 0.5).clamp(0.0, 1.0);
        let center = (bmin + bmax) * 0.5;
        let seed = [
            bmin.x + u * ext.x,
            bmin.y + v * ext.y,
            center.z,
        ];
        sdf.project_to_surface(seed, prim_uid, 0.0, c.accept)
            .unwrap_or([
                bmin.x + u * ext.x,
                bmin.y + v * ext.y,
                bmin.z + 0.5 * ext.z,
            ])
    } else {
        p
    };
    let x = ((world[0] - bmin.x) / ext.x).clamp(0.0, 1.0);
    let y = ((world[1] - bmin.y) / ext.y).clamp(0.0, 1.0);
    let z = ((world[2] - bmin.z) / ext.z).clamp(0.0, 1.0);
    (x, y, z)
}

impl SdfFieldSources {
    fn sdf_at(&self, p: [f32; 3], prim_uid: u32) -> f32 {
        use core_math::sdf_math::eval_scene;
        use glam::Vec3;
        let pv = Vec3::new(p[0], p[1], p[2]);
        if prim_uid != 0 {
            if let Some(prim) = self.by_uid.get(&prim_uid) {
                return prim.world_distance(pv);
            }
        }
        if !self.has_field() {
            // NO analytical field for this body — fail closed. See `NO_FIELD_D`.
            return NO_FIELD_D;
        }
        let refs: Vec<&core_math::primitives::SdfPrimitive> =
            self.body_prims.iter().collect();
        eval_scene(pv, &refs)
    }

    /// Does **this** body have an analytical SDF field to evaluate?
    ///
    /// `body_prims` is filled ONLY from `SceneGraph::body_primitives`, which returns `Some` for
    /// an `SdfBody` and `None` for a `PolygonalBody` — so a non-empty prim array already means
    /// "this body is an SDF body with a field", and no second discriminator is needed (a second
    /// one would be a second source of truth that every snapshot site could forget to set).
    /// What was wrong before was the `|| !by_uid.is_empty()` clause; see that field's doc.
    pub(super) fn has_field(&self) -> bool {
        !self.body_prims.is_empty()
    }

    /// Is there ANY curvature signal available — an analytical field to differentiate, or a
    /// precomputed κ grid (the GPU `curvature_eval` kernel for SDF bodies, or the half-edge mesh
    /// measurement for polygonal ones)?
    ///
    /// `SdfCurvature` needs no such guard: it calls `sdf_curvature_masks_ex`, which tries the
    /// grid cache first and then degrades to κ = 0 through `NO_FIELD_D`. `Cavity` does, because
    /// it branches on the answer.
    pub(super) fn has_curvature_source(&self) -> bool {
        self.has_field()
            || !self.curv_caches.is_empty()
            || !self.vertex_curv_caches.is_empty()
            || self.poly_curv.as_ref().is_some_and(|p| !p.is_empty())
    }

    /// Axis-aligned bounds of available prims (world space). Fallback unit cube.
    /// pub(crate): the material panel derives the C4 radius-floor caption from
    /// the SAME bounds the curvature bake uses (`material_panel::lower::curvature_radius_floor`).
    pub(crate) fn body_aabb(&self, prim_uid: u32) -> (Vec3, Vec3) {
        let mut min_v = Vec3::splat(f32::INFINITY);
        let mut max_v = Vec3::splat(f32::NEG_INFINITY);
        let mut any = false;
        let consider = |p: &core_math::primitives::SdfPrimitive, min_v: &mut Vec3, max_v: &mut Vec3, any: &mut bool| {
            // Conservative sphere from position + max scale * shape extent.
            let he = p.bounding_aabb_half_extents();
            let sc = p.scale.abs().max(Vec3::splat(1e-4));
            let ext = he * sc + p.elongation.abs();
            let c = p.position;
            *min_v = min_v.min(c - ext);
            *max_v = max_v.max(c + ext);
            *any = true;
        };
        if prim_uid != 0 {
            if let Some(p) = self.by_uid.get(&prim_uid) {
                consider(p, &mut min_v, &mut max_v, &mut any);
            }
        }
        if !any {
            for p in &self.body_prims {
                consider(p, &mut min_v, &mut max_v, &mut any);
            }
        }
        // NO `by_uid` fall-through. `by_uid` holds every prim in the SCENE (so cross-body
        // `PrimSdfMask` picks resolve), so bounding it answers "how big is everything?" when the
        // caller asked "how big is THIS body?". A polygonal body — or an SDF body whose prims are
        // absent — used to silently inherit an unrelated body's AABB, which then set the seed
        // positions, the FD step `e` and the κ display scale for the whole bake. When the caller
        // asked for a prim (`prim_uid != 0`) that prim was already consulted above.
        if !any {
            // Unit cube: a defined, body-independent domain. Every seed projected through it
            // misses (no field to hit), so analyzers return their fail-closed values.
            return (Vec3::splat(-1.0), Vec3::splat(1.0));
        }
        // Pad slightly so projection from outside still hits surface
        let pad = ((max_v - min_v).max_element() * 0.05).max(0.02);
        (min_v - Vec3::splat(pad), max_v + Vec3::splat(pad))
    }

    fn sdf_grad(&self, p: [f32; 3], prim_uid: u32, e: f32) -> [f32; 3] {
        let e = e.max(1e-5);
        let dx = (self.sdf_at([p[0] + e, p[1], p[2]], prim_uid)
            - self.sdf_at([p[0] - e, p[1], p[2]], prim_uid))
            * (0.5 / e);
        let dy = (self.sdf_at([p[0], p[1] + e, p[2]], prim_uid)
            - self.sdf_at([p[0], p[1] - e, p[2]], prim_uid))
            * (0.5 / e);
        let dz = (self.sdf_at([p[0], p[1], p[2] + e], prim_uid)
            - self.sdf_at([p[0], p[1], p[2] - e], prim_uid))
            * (0.5 / e);
        [dx, dy, dz]
    }

    /// Newton-project `p` onto the isosurface `sdf - grow = 0` (3D surface, not UV plane).
    fn project_to_surface(
        &self,
        mut p: [f32; 3],
        prim_uid: u32,
        grow: f32,
        accept: f32,
    ) -> Option<[f32; 3]> {
        let e = 0.002f32;
        for _ in 0..10 {
            let d = self.sdf_at(p, prim_uid) - grow;
            if d.abs() < 1e-4 {
                return Some(p);
            }
            let g = self.sdf_grad(p, prim_uid, e);
            let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-6);
            // Along unit gradient: reach isosurface in one step for true SDFs
            p[0] -= (g[0] / gl) * d;
            p[1] -= (g[1] / gl) * d;
            p[2] -= (g[2] / gl) * d;
        }
        let d = (self.sdf_at(p, prim_uid) - grow).abs();
        if d < accept.max(1e-3) {
            Some(p)
        } else {
            None
        }
    }

    /// TANGENT-PLANE mean-curvature Laplacian (2026-07-27 fix — replaces the old WORLD-AXIS
    /// 6-neighbour stencil, which second-differenced along fixed X/Y/Z regardless of the local
    /// surface orientation). Mean curvature only lives IN the surface's tangent plane; a tap
    /// along the direction TANGENT to the surface should read ~0 (locally flat there), but the
    /// old stencil always took 2 of its 3 axis-pairs off-tangent-plane-adjacent and, at large `e`
    /// (e.g. the owner's Cavity radius 0.190), that tap escapes the local feature and samples
    /// UNRELATED geometry — a probe on the carved sphere's concave cavity measured the tangent
    /// axis contributing +0.180 against −0.003 on each genuinely-curved axis, flipping the sign
    /// so cavities read convex (`tests_gpu_curvature::zz_probe_grad_norm`, `zz_probe_geometry`).
    ///
    /// Fix: get the surface normal from `sdf_grad`, build an orthonormal tangent basis (t1, t2)
    /// perpendicular to it (Duff/Hughes-Möller construction — branchless, no degenerate axis
    /// pick), and take the 4-tap Laplacian ALONG THE TANGENTS ONLY:
    ///   curv ≈ [f(p+e·t1) + f(p-e·t1) + f(p+e·t2) + f(p-e·t2) − 4f(p)] / e²
    /// This is the standard discrete mean curvature of the level set (no untangented axis can
    /// leak in). Degenerate gradient (∇f ≈ 0, e.g. off the field entirely) falls back to a fixed
    /// world basis rather than propagating NaN.
    ///
    /// SIGN (PROVED empirically, not assumed — `zz_probe_sign_convention` /
    /// `sdf_curvature_sign_matches_known_geometry`, sphere + carved-box probes at the owner's
    /// e=0.19 radius): with this codebase's SDF convention (negative inside), the RAW tangent-
    /// plane Laplacian below already reads **positive on convex, negative on concave** with NO
    /// extra sign flip needed:
    ///   - solid sphere, surface point (unambiguously convex): measured `lap/e² = +3.865`
    ///   - box outer edge (convex corner): measured `lap/e² = +7.443`
    ///   - inside a carved spherical cavity (Subtract, unambiguously concave): `lap/e² = -0.157`
    /// This matches the existing call site's polarity (`convex = curv.max(0.0)`,
    /// `concave = (-curv).max(0.0)` further down in `sdf_curvature_masks`) with ZERO change to
    /// that reduce step — only the stencil orientation changed, exactly as required.
    ///
    /// Mirrors GPU `calc_curvature` (sdf_prim_eval.wgsl) / `curv_laplacian` (volume_bake.wgsl) —
    /// keep IDENTICAL math (same tangent-basis formula, same tap order) in both.
    fn calc_curvature(&self, p: [f32; 3], prim_uid: u32, e: f32) -> f32 {
        let e = e.max(1e-4);
        let d = self.sdf_at(p, prim_uid);
        let g = self.sdf_grad(p, prim_uid, e.min(0.01).max(1e-4));
        let (t1, t2) = tangent_basis(g);
        let lap = self.sdf_at(
            [p[0] + e * t1[0], p[1] + e * t1[1], p[2] + e * t1[2]],
            prim_uid,
        ) + self.sdf_at(
            [p[0] - e * t1[0], p[1] - e * t1[1], p[2] - e * t1[2]],
            prim_uid,
        ) + self.sdf_at(
            [p[0] + e * t2[0], p[1] + e * t2[1], p[2] + e * t2[2]],
            prim_uid,
        ) + self.sdf_at(
            [p[0] - e * t2[0], p[1] - e * t2[1], p[2] - e * t2[2]],
            prim_uid,
        ) - 4.0 * d;
        lap / (e * e)
    }
}

#[cfg(test)]
impl SdfFieldSources {
    /// Test-only window onto the private `sdf_at`, so the P0 fail-closed gate can assert the
    /// sentinel directly instead of inferring it from a mask value.
    pub(crate) fn sdf_at_for_test(&self, p: [f32; 3], prim_uid: u32) -> f32 {
        self.sdf_at(p, prim_uid)
    }
}

/// Build an orthonormal tangent basis (t1, t2) perpendicular to normal `n` — Duff/Hughes-Möller
/// (branchless, no axis-degeneracy special case, matches the WGSL/GPU port bit-for-bit in
/// structure). `n` need not be pre-normalized; degenerate (near-zero) gradients fall back to a
/// fixed world basis instead of propagating NaN/Inf (guard required by the task: never silently
/// return garbage on ∇f ≈ 0).
pub(crate) fn tangent_basis(g: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
    let n = if gl > 1e-6 {
        [g[0] / gl, g[1] / gl, g[2] / gl]
    } else {
        // Degenerate gradient (flat/undefined field) — stable fallback basis, never NaN.
        [0.0, 0.0, 1.0]
    };
    let sign = if n[2] >= 0.0 { 1.0f32 } else { -1.0f32 };
    let a = -1.0 / (sign + n[2]);
    let b = n[0] * n[1] * a;
    let t1 = [1.0 + sign * n[0] * n[0] * a, sign * b, -sign * n[0]];
    let t2 = [b, sign + n[1] * n[1] * a, -n[1]];
    (t1, t2)
}

/// Convert signed distance (+ grow offset) to soft 0–1 mask.
pub(super) fn sdf_to_mask(d: f32, grow: f32, soft: f32, contrast: f32, strength: f32, invert: bool) -> f32 {
    let dd = d - grow; // positive grow expands interior (negative d region)
    let soft = soft.max(1e-5);
    // Inside = negative SDF → mask 1. softstep across the band.
    let mut m = (0.5 - dd / (2.0 * soft)).clamp(0.0, 1.0);
    if invert {
        m = 1.0 - m;
    }
    let c = contrast.max(0.001);
    // Contrast around 0.5
    m = ((m - 0.5) * c + 0.5).clamp(0.0, 1.0);
    (m * strength).clamp(0.0, 1.0)
}

/// True when `p` looks like the planar bake domain (u,v)→[-1,1]², z≈0.
pub(super) fn is_planar_bake_domain(p: [f32; 3]) -> bool {
    p[0].abs() <= 1.05 && p[1].abs() <= 1.05 && p[2].abs() <= 0.08
}

/// DIRECTION mask: how much the surface faces a chosen world axis.
///
/// Returns `(mask, raw_dot)` where `raw_dot = dot(surface_normal, axis)` in −1..1 and `mask` is
/// that dot remapped to 0..1 and shaped by `contrast`/`strength`. Axis +Y therefore gives 1 on
/// up-facing surfaces, 0.5 on vertical walls, 0 on down-facing — the "snow on top" mask.
///
/// **Why this is not just `dot(n, axis)` on the interpolated normal.** On the planar bake domain
/// the evaluator's `n` argument is the CONSTANT `[0,0,1]` (every pixel of the flat bake quad —
/// see the bake loop), so a node reading that socket bakes a uniform gray card. That is the
/// documented `Slope` gap. This function instead recovers the REAL surface normal the same way
/// [`sdf_curvature_masks`] does: project triplanar seeds through the body AABB onto the
/// isosurface with `project_to_surface`, then take the normalized `sdf_grad` there. The seed
/// set, weights, `CurvConstants` and accept-band are deliberately IDENTICAL to the curvature
/// function so a Direction layer and a Curvature layer at the same radius sample the same
/// surface points — divergence between the two would be a bug, not a feature.
///
/// Off the planar domain (an explicit world `Position` wired into input 0) a single seed at `p`
/// is used, matching the curvature function's `else` branch.
///
/// Returns `(0.0, 0.0)` when no seed projects onto a surface (empty field / seed outside the
/// band) — fail-closed, the same as curvature, never a plausible-looking constant.
/// Ambient occlusion from the body SDF — same math as `calc_ao` in
/// `graphics/src/sdf_material.wgsl` (IQ-style: sample along the outward normal,
/// accumulate how much nearer geometry is than free-space distance).
///
/// On the planar bake domain the evaluator's `n` is constant, so we recover the
/// real surface normal via the same seed projection + `sdf_grad` path as
/// [`sdf_direction_mask`]. No field → 1.0 (fully unoccluded, fail-open so a
/// hand-built graph without a body does not paint fake dark cavities).
pub(crate) fn sdf_ambient_occlusion(
    sdf: &SdfFieldSources,
    p: [f32; 3],
    eps: f32,
    prim_uid: u32,
    grow: f32,
    band: f32,
) -> f32 {
    if !sdf.has_field() {
        return 1.0;
    }
    let (bmin, bmax) = sdf.body_aabb(prim_uid);
    let c = crate::gpu_curvature::CurvConstants::new(bmin, bmax, eps, band);
    let ext = (bmax - bmin).max(Vec3::splat(1e-4));
    let e = c.e;
    let accept = c.accept;
    let diag = c.diag;

    // Lockstep seed set with `sdf_direction_mask` / `sdf_curvature_masks`.
    let seeds: Vec<([f32; 3], usize)> = if is_planar_bake_domain(p) {
        let u = ((p[0] + 1.0) * 0.5).clamp(0.0, 1.0);
        let v = ((p[1] + 1.0) * 0.5).clamp(0.0, 1.0);
        let center = (bmin + bmax) * 0.5;
        let p_xy = [bmin.x + u * ext.x, bmin.y + v * ext.y, center.z];
        let p_xz = [bmin.x + u * ext.x, center.y, bmin.z + v * ext.z];
        let p_yz = [center.x, bmin.y + u * ext.y, bmin.z + v * ext.z];
        let out = diag * 0.55;
        vec![
            (p_xy, 2),
            (p_xz, 1),
            (p_yz, 0),
            ([p_xy[0], p_xy[1], bmax.z + out], 2),
            ([p_xz[0], bmax.y + out, p_xz[2]], 1),
            ([bmax.x + out, p_yz[1], p_yz[2]], 0),
        ]
    } else {
        vec![(p, 0)]
    };

    let mut acc = 0.0f32;
    let mut wsum = 0.0f32;
    for (seed, sa) in seeds {
        let Some(sp) = sdf.project_to_surface(seed, prim_uid, grow, accept) else {
            continue;
        };
        let g = sdf.sdf_grad(sp, prim_uid, e * 0.5);
        let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
        if gl < 1e-6 {
            continue;
        }
        // Outward normal (SDF positive outside → gradient points out).
        let n = [g[0] / gl, g[1] / gl, g[2] / gl];
        let w = if is_planar_bake_domain(p) {
            let a = n[sa].abs();
            (a * a).max(0.05)
        } else {
            1.0
        };
        let ao = calc_ao_samples(sdf, sp, n, prim_uid);
        acc += ao * w;
        wsum += w;
    }
    if wsum < 1e-6 {
        1.0
    } else {
        (acc / wsum).clamp(0.0, 1.0)
    }
}

/// IQ-style AO sample loop — bit-for-bit structure with `calc_ao` (5 samples =
/// medium `render_quality` default). `p` is a surface point, `n` unit outward.
pub(super) fn calc_ao_samples(
    sdf: &SdfFieldSources,
    p: [f32; 3],
    n: [f32; 3],
    prim_uid: u32,
) -> f32 {
    const SAMPLES: u32 = 5;
    let mut occ = 0.0f32;
    let mut scale = 1.0f32;
    for i in 1..=SAMPLES {
        let h = 0.01 + 0.12 * (i as f32) / (SAMPLES as f32);
        let q = [p[0] + n[0] * h, p[1] + n[1] * h, p[2] + n[2] * h];
        let d = sdf.sdf_at(q, prim_uid);
        occ += (h - d) * scale;
        scale *= 0.75;
    }
    (1.0 - 2.5 * occ).clamp(0.0, 1.0)
}

pub(crate) fn sdf_direction_mask(
    sdf: &SdfFieldSources,
    p: [f32; 3],
    eps: f32,
    prim_uid: u32,
    axis: [f32; 3],
    grow: f32,
    band: f32,
    contrast: f32,
    strength: f32,
) -> (f32, f32) {
    let (bmin, bmax) = sdf.body_aabb(prim_uid);
    // Lockstep with `sdf_curvature_masks` — same constants, same seeds, same weights.
    let c = crate::gpu_curvature::CurvConstants::new(bmin, bmax, eps, band);
    let ext = (bmax - bmin).max(Vec3::splat(1e-4));
    let e = c.e;
    let accept = c.accept;
    let diag = c.diag;

    let seeds: Vec<([f32; 3], usize)> = if is_planar_bake_domain(p) {
        let u = ((p[0] + 1.0) * 0.5).clamp(0.0, 1.0);
        let v = ((p[1] + 1.0) * 0.5).clamp(0.0, 1.0);
        let center = (bmin + bmax) * 0.5;
        let p_xy = [bmin.x + u * ext.x, bmin.y + v * ext.y, center.z];
        let p_xz = [bmin.x + u * ext.x, center.y, bmin.z + v * ext.z];
        let p_yz = [center.x, bmin.y + u * ext.y, bmin.z + v * ext.z];
        let out = diag * 0.55;
        vec![
            (p_xy, 2),
            (p_xz, 1),
            (p_yz, 0),
            ([p_xy[0], p_xy[1], bmax.z + out], 2),
            ([p_xz[0], bmax.y + out, p_xz[2]], 1),
            ([bmax.x + out, p_yz[1], p_yz[2]], 0),
        ]
    } else {
        vec![(p, 0)]
    };

    // Normalize the axis once; a zero axis (never authored, but cheap to guard) falls back to +Y
    // rather than producing NaN.
    let al = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    let ax = if al < 1e-6 {
        [0.0, 1.0, 0.0]
    } else {
        [axis[0] / al, axis[1] / al, axis[2] / al]
    };

    let mut sum_d = 0.0f32;
    let mut sum_w = 0.0f32;
    for (seed, sa) in seeds {
        let Some(ps) = sdf.project_to_surface(seed, prim_uid, grow, accept) else {
            continue;
        };
        let g = sdf.sdf_grad(ps, prim_uid, e * 0.5);
        let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-6);
        // SDF convention here is NEGATIVE INSIDE, so +∇f already points OUT of the surface —
        // this is the outward normal with no sign flip (same as `sdf_curvature_masks`).
        let n = [g[0] / gl, g[1] / gl, g[2] / gl];
        let w = if is_planar_bake_domain(p) {
            let a = n[sa].abs();
            (a * a).max(0.05)
        } else {
            1.0
        };
        sum_d += (n[0] * ax[0] + n[1] * ax[1] + n[2] * ax[2]) * w;
        sum_w += w;
    }
    if sum_w < 1e-6 {
        return (0.0, 0.0);
    }
    let dot = (sum_d / sum_w).clamp(-1.0, 1.0);
    // −1..1 → 0..1, then contrast around the 0.5 midpoint (vertical walls stay put as contrast
    // rises — the intuitive behaviour for "tighten the snow to the flattest tops").
    let t = dot * 0.5 + 0.5;
    let c_k = contrast.max(0.001);
    let mask = (((t - 0.5) * c_k + 0.5).clamp(0.0, 1.0) * strength).clamp(0.0, 1.0);
    (mask, dot)
}

/// Surface curvature masks for edge wear / line art / cavity dirt.
///
/// Uses the **same** signal as GPU edge-wear protection (`calc_curvature` in sdf_prim_eval.wgsl):
/// project sample seeds onto the isosurface, then 6-neighbor Laplacian / e².
/// UV bake domain uses triplanar seeds through the body AABB (not a flat silhouette).
///
/// region: 0=Both (|κ|), 1=Convex/edges (κ>0), 2=Concave/cavities (κ<0).
/// Returns (mask, signed_curv −1..1, convex_mask, concave_mask).
/// `pub(crate)` so the GPU golden-parity gate (`sdf_gpu_mesh::tests_gpu_curvature`) calls THIS
/// function — the production CPU path — as its oracle, never a copy of it.
/// Shape raw mean curvature into the four channels the node graph exposes.
/// Shared by planar CPU, planar GPU grid, and vertex-splat grids so a mode switch
/// never redefines "what 0.7 means".
pub(crate) fn shape_curvature_channels(
    k_raw: f32,
    feature_display_scale: f32,
    region: u32,
    contrast: f32,
    strength: f32,
) -> (f32, f32, f32, f32) {
    let curv = {
        let x = k_raw * feature_display_scale;
        x / (1.0 + x.abs())
    };
    let convex = curv.max(0.0);
    let concave = (-curv).max(0.0);
    let c = contrast.max(0.001);
    let shape = |x: f32| {
        let t = (x * c * 4.0).clamp(0.0, 1.0);
        let s = t * t * (3.0 - 2.0 * t);
        (s * strength).clamp(0.0, 1.0)
    };
    let mask = match region {
        1 => shape(convex),
        2 => shape(concave),
        _ => shape(curv.abs()),
    };
    (mask, curv, shape(convex), shape(concave))
}

/// BOTH MODES Phase 3 — measure κ on **surface positions** (volume-stratified seeds projected
/// onto the isosurface), splat into the planar `res×res` bake domain.
///
/// Phase 2 seeded from six AABB faces only — rays can only hit exterior-visible surface, so
/// concave pockets / boolean interiors stayed at 0 (measured 22.8 % fill vs planar 81.8 %).
/// Density sweeps proved the ceiling was structural (~35 % max), not under-sampling.
///
/// Phase 3 seeds a 3D lattice **through the AABB** (interior + exterior of the solid).
/// Newton projection lands on the nearest isosurface, including cavity walls — the same
/// coverage property SEED_POSITIONS / DC mesh verts give (any surface point is reachable).
/// Storage stays planar so PT and Hybrid sample the bake (Decision 1 compositor).
pub(crate) fn build_vertex_splat_curv_grid(
    sdf: &SdfFieldSources,
    key: &crate::gpu_curvature::CurvKey,
    res: u32,
) -> crate::gpu_curvature::CurvGrid {
    use crate::gpu_curvature::CurvGrid;
    let res = res.max(8);
    let n = (res * res) as usize;
    let mut acc = vec![0.0f32; n * 4];
    let mut wsum = vec![0.0f32; n];

    if !sdf.has_field() {
        return CurvGrid {
            res,
            data: acc,
            region: key.region,
            key: *key,
        };
    }

    let (bmin, bmax) = sdf.body_aabb(key.prim_uid);
    let c = crate::gpu_curvature::CurvConstants::new(bmin, bmax, key.eps, key.band);
    let ext = (bmax - bmin).max(Vec3::splat(1e-4));
    let e = c.e;
    let accept = c.accept;
    let feat = c.feature_display_scale();
    // 3D lattice axis count. Scales with bake res; capped so Final 1K stays interactive.
    // At res=64 → s=32 → 32k seeds (order of Phase-2 max face seeds, without the blind spot).
    let s = ((res / 2).clamp(16, 48)) as i32;

    let splat_bilinear = |u: f32, v: f32, w: f32, vals: [f32; 4], acc: &mut [f32], wsum: &mut [f32]| {
        if w < 1e-8 {
            return;
        }
        let fx = u * (res as f32) - 0.5;
        let fy = v * (res as f32) - 0.5;
        let x0 = fx.floor() as i32;
        let y0 = fy.floor() as i32;
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        for (dx, dy, bw) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            let x = x0 + dx;
            let y = y0 + dy;
            if x < 0 || y < 0 || x >= res as i32 || y >= res as i32 {
                continue;
            }
            let wi = w * bw;
            if wi < 1e-10 {
                continue;
            }
            let i = (y as u32 * res + x as u32) as usize;
            wsum[i] += wi;
            let base = i * 4;
            for c in 0..4 {
                acc[base + c] += vals[c] * wi;
            }
        }
    };

    let measure_and_splat = |seed: [f32; 3], acc: &mut [f32], wsum: &mut [f32]| {
        let Some(ps) = sdf.project_to_surface(seed, key.prim_uid, key.grow, accept) else {
            return;
        };
        let g = sdf.sdf_grad(ps, key.prim_uid, e * 0.5);
        let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
        if gl < 1e-6 {
            return;
        }
        let nrm = [g[0] / gl, g[1] / gl, g[2] / gl];
        let k_raw = sdf.calc_curvature(ps, key.prim_uid, e);
        let (mask, curv, conv, conc) =
            shape_curvature_channels(k_raw, feat, key.region, key.contrast, key.strength);
        let vals = [mask, curv, conv, conc];
        // Normal-weighted box projection → bake UV (same three planes planar seeds use).
        let b0 = [bmin.x, bmin.y, bmin.z];
        let e0 = [ext.x, ext.y, ext.z];
        let comps = [
            (nrm[0].abs().powi(2), 1usize, 2usize), // YZ → UV (y,z)
            (nrm[1].abs().powi(2), 0usize, 2usize), // XZ
            (nrm[2].abs().powi(2), 0usize, 1usize), // XY
        ];
        for (nw, ui, vi) in comps {
            if nw < 1e-6 {
                continue;
            }
            let w = nw.max(0.05);
            let u = ((ps[ui] - b0[ui]) / e0[ui]).clamp(0.0, 1.0);
            let v = ((ps[vi] - b0[vi]) / e0[vi]).clamp(0.0, 1.0);
            splat_bilinear(u, v, w, vals, acc, wsum);
        }
    };

    // ── Pass 1: volume lattice → surface measure → splat (Phase 3 core) ─────────────────
    // Interior samples reach cavity walls; exterior samples hit the outer shell.
    for iz in 0..s {
        for iy in 0..s {
            for ix in 0..s {
                let seed = [
                    bmin.x + (ix as f32 + 0.5) / s as f32 * ext.x,
                    bmin.y + (iy as f32 + 0.5) / s as f32 * ext.y,
                    bmin.z + (iz as f32 + 0.5) / s as f32 * ext.z,
                ];
                measure_and_splat(seed, &mut acc, &mut wsum);
            }
        }
    }

    // Normalize splat accumulators before hole-fill.
    for i in 0..n {
        if wsum[i] > 1e-6 {
            let inv = 1.0 / wsum[i];
            for c in 0..4 {
                acc[i * 4 + c] *= inv;
            }
        }
    }

    // ── Pass 2: hole-fill from planar multi-seed (coverage without losing cavity hits) ───
    // Splat alone under-covers the UV domain (Phase-2 symptom: 22–36 % fill). Any texel
    // still empty is filled with the same multi-seed surface measure planar uses, so the
    // map is not mostly black. Where volume splat already wrote, we keep that value —
    // those are the surface-position samples (cavities/overhangs) planar gift-wraps poorly.
    for y in 0..res {
        for x in 0..res {
            let ti = (y * res + x) as usize;
            if wsum[ti] > 1e-6 {
                continue; // already has a surface-position sample
            }
            let u = (x as f32 + 0.5) / res as f32;
            let v = (y as f32 + 0.5) / res as f32;
            let p = [u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0];
            let (mask, curv, conv, conc) = sdf_curvature_masks(
                sdf,
                p,
                key.eps,
                key.prim_uid,
                key.region,
                key.grow,
                key.band,
                key.contrast,
                key.strength,
            );
            if mask > 1e-6 || curv.abs() > 1e-6 {
                let base = ti * 4;
                acc[base] = mask;
                acc[base + 1] = curv;
                acc[base + 2] = conv;
                acc[base + 3] = conc;
                wsum[ti] = 1.0;
            }
        }
    }

    CurvGrid {
        res,
        data: acc,
        region: key.region,
        key: *key,
    }
}

/// Ensure `sdf.vertex_curv_caches` has a splat grid for every Vertex-tagged curv key at `res`.
pub(crate) fn prepare_vertex_splat_caches(
    graph: &NodeGraph,
    sdf: &mut SdfFieldSources,
    res: u32,
) {
    if !sdf.has_field() {
        return;
    }
    let vkeys = super::vertex_curv_keys_for_graph(graph);
    if vkeys.is_empty() {
        return;
    }
    for (key, _) in vkeys {
        if sdf
            .vertex_curv_caches
            .iter()
            .any(|g| g.key.matches(&key) && g.res == res && !g.data.is_empty())
        {
            continue;
        }
        // Drop stale same-key wrong-res entry if any.
        sdf.vertex_curv_caches
            .retain(|g| !(g.key.matches(&key) && g.res != res));
        sdf.vertex_curv_caches
            .push(build_vertex_splat_curv_grid(sdf, &key, res));
    }
}

pub(crate) fn sdf_curvature_masks(
    sdf: &SdfFieldSources,
    p: [f32; 3],
    eps: f32,
    prim_uid: u32,
    region: u32,
    grow: f32,
    band: f32,
    contrast: f32,
    strength: f32,
) -> (f32, f32, f32, f32) {
    sdf_curvature_masks_ex(
        sdf, p, eps, prim_uid, region, grow, band, contrast, strength, false,
    )
}

/// Like [`sdf_curvature_masks`]; `vertex_mode` selects the Phase-2 splat grid
/// (`vertex_curv_caches`) over the planar SEED_PLANAR grid (`curv_caches`).
pub(crate) fn sdf_curvature_masks_ex(
    sdf: &SdfFieldSources,
    p: [f32; 3],
    eps: f32,
    prim_uid: u32,
    region: u32,
    grow: f32,
    band: f32,
    contrast: f32,
    strength: f32,
    vertex_mode: bool,
) -> (f32, f32, f32, f32) {
    // ── FAST PATH: precomputed res² grid (planar GPU or vertex splat) ───────────────────
    if is_planar_bake_domain(p) {
        let key = crate::gpu_curvature::CurvKey {
            eps, prim_uid, region, grow, band, contrast, strength,
        };
        let cache = if vertex_mode {
            sdf.vertex_curv_caches.as_slice()
        } else {
            sdf.curv_caches.as_slice()
        };
        if let Some(g) = cache.iter().find(|g| g.key.matches(&key)) {
            if g.res > 0 && !g.data.is_empty() {
                let res_f = g.res as f32;
                let fx = ((p[0] + 1.0) * 0.5) * res_f - 0.5;
                let fy = ((p[1] + 1.0) * 0.5) * res_f - 0.5;
                let x = fx.round().clamp(0.0, res_f - 1.0) as u32;
                let y = fy.round().clamp(0.0, res_f - 1.0) as u32;
                return g.get(x, y);
            }
        }
    }

    let (bmin, bmax) = sdf.body_aabb(prim_uid);
    // Lockstep with `CurvConstants::new` / GPU `curvature_eval` (elongate + multi-prim safe).
    let c = crate::gpu_curvature::CurvConstants::new(bmin, bmax, eps, band);
    let ext = (bmax - bmin).max(Vec3::splat(1e-4));
    let e = c.e;
    let accept = c.accept;
    let diag = c.diag;

    // Build surface sample seeds.
    // - Bake UV domain: three orthographic seeds (triplanar) covering the body.
    // - Explicit world Position: single seed at p (still projected onto the surface).
    let seeds: Vec<([f32; 3], usize)> = if is_planar_bake_domain(p) {
        let u = ((p[0] + 1.0) * 0.5).clamp(0.0, 1.0);
        let v = ((p[1] + 1.0) * 0.5).clamp(0.0, 1.0);
        let center = (bmin + bmax) * 0.5;
        let p_xy = [bmin.x + u * ext.x, bmin.y + v * ext.y, center.z];
        let p_xz = [bmin.x + u * ext.x, center.y, bmin.z + v * ext.z];
        let p_yz = [center.x, bmin.y + u * ext.y, bmin.z + v * ext.z];
        // Seed push uses full body diag (not feature) so long AABBs still cover end faces.
        let out = diag * 0.55;
        vec![
            (p_xy, 2),
            (p_xz, 1),
            (p_yz, 0),
            ([p_xy[0], p_xy[1], bmax.z + out], 2),
            ([p_xz[0], bmax.y + out, p_xz[2]], 1),
            ([bmax.x + out, p_yz[1], p_yz[2]], 0),
        ]
    } else {
        vec![(p, 0)]
    };

    let mut sum_k = 0.0f32;
    let mut sum_w = 0.0f32;
    for (seed, axis) in seeds {
        let Some(ps) = sdf.project_to_surface(seed, prim_uid, grow, accept) else {
            continue;
        };
        let g = sdf.sdf_grad(ps, prim_uid, e * 0.5);
        let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-6);
        let n = [g[0] / gl, g[1] / gl, g[2] / gl];
        let w = if is_planar_bake_domain(p) {
            let a = n[axis].abs();
            (a * a).max(0.05)
        } else {
            1.0
        };
        // GPU-identical protection / edge-wear curvature
        let k = sdf.calc_curvature(ps, prim_uid, e);
        sum_k += k * w;
        sum_w += w;
    }

    if sum_w < 1e-6 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let k_raw = sum_k / sum_w;
    // Compressive (Reinhard) display scale — keep identical to GPU `curvature_eval`.
    shape_curvature_channels(
        k_raw,
        c.feature_display_scale(),
        region,
        contrast,
        strength,
    )
}

// ---------------------------------------------------------------------------
// CPU evaluator
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(super) enum Val {
    F(f32),
    V([f32; 3]),
}

impl Val {
    /// Scalar view. Vec3/Color → Rec.709 luminance (matches WGSL `input_var_name` +
    /// `SocketType::compatible` "luminance" promise — LOG_TRIAGE BUG 3).
    pub(super) fn as_f(self) -> f32 {
        match self {
            Val::F(f) => f,
            Val::V(v) => 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2],
        }
    }
    pub(super) fn as_v(self) -> [f32; 3] {
        match self {
            Val::F(f) => [f, f, f],
            Val::V(v) => v,
        }
    }
}

/// Node types that never sample `p`/`n`/`hit_prim` — candidates for T3 hoist when all
/// *wired* inputs also come from invariant nodes.
pub(super) fn node_type_can_be_invariant(nt: &NodeType) -> bool {
    use crate::shader_nodes::NodeType as NT;
    matches!(
        nt,
        NT::ConstantValue
            | NT::ConstantColor
            | NT::MathBinary
            | NT::MathUnary
            | NT::VectorMath
            | NT::Clamp
            | NT::MixFloat
            | NT::MixRgb
            | NT::Smoothstep
            | NT::Remap
            | NT::Invert
            | NT::ColorCorrect
            | NT::BrightnessContrast
            | NT::ColorRamp
            | NT::SeparateXyz
            | NT::CombineXyz
            | NT::ObjectInfo // bake always passes hit_prim=0
            | NT::MaterialOutput
    )
}

/// T3: precompute nodes whose outputs do not depend on pixel position/normal.
pub(super) fn compute_invariant_outs(
    graph: &NodeGraph,
    order: &[NodeId],
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    max_noise_octaves: u32,
) -> (HashMap<(NodeId, usize), Val>, Vec<NodeId>) {
    use std::collections::HashSet;
    let mut inv: HashSet<NodeId> = HashSet::new();
    // Fixed-point: a node is invariant if its type allows it and every *wired* source is invariant.
    for &nid in order {
        let Some(node) = graph.find_node(nid) else {
            continue;
        };
        if !node_type_can_be_invariant(&node.node_type) {
            continue;
        }
        let n_in = Node::input_defs(&node.node_type).len();
        let mut ok = true;
        for i in 0..n_in {
            if let Some(conn) = graph
                .connections
                .iter()
                .find(|c| c.to_node == nid && c.to_input == i)
            {
                if !inv.contains(&conn.from_node) {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            inv.insert(nid);
        }
    }
    let mut outs: HashMap<(NodeId, usize), Val> = HashMap::new();
    let dummy_p = [0.0f32; 3];
    let dummy_n = [0.0f32, 0.0, 1.0];
    for &nid in order {
        if !inv.contains(&nid) {
            continue;
        }
        let Some(node) = graph.find_node(nid) else {
            continue;
        };
        eval_node(
            graph,
            node,
            dummy_p,
            dummy_n,
            0.0,
            images,
            sdf,
            max_noise_octaves,
            &mut outs,
        );
    }
    let varying: Vec<NodeId> = order
        .iter()
        .copied()
        .filter(|id| !inv.contains(id))
        .collect();
    (outs, varying)
}

pub(super) fn eval_graph_sample(
    graph: &NodeGraph,
    order: &[NodeId],
    output_id: NodeId,
    p: [f32; 3],
    n: [f32; 3],
    hit_prim: f32,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    max_noise_octaves: u32,
) -> SampledMat {
    eval_graph_sample_cached(
        graph,
        order,
        None,
        output_id,
        p,
        n,
        hit_prim,
        images,
        sdf,
        max_noise_octaves,
    )
}

/// Like [`eval_graph_sample`]; when `invariant` is set, seed `outs` from the precomputed map
/// and only re-eval `varying` node ids (T3).
pub(super) fn eval_graph_sample_cached(
    graph: &NodeGraph,
    order: &[NodeId],
    invariant: Option<(&HashMap<(NodeId, usize), Val>, &[NodeId])>,
    output_id: NodeId,
    p: [f32; 3],
    n: [f32; 3],
    hit_prim: f32,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    max_noise_octaves: u32,
) -> SampledMat {
    let mut outs: HashMap<(NodeId, usize), Val> = match invariant {
        Some((base, _)) => base.clone(),
        None => HashMap::new(),
    };
    let eval_order: &[NodeId] = match invariant {
        Some((_, varying)) => varying,
        None => order,
    };
    for &nid in eval_order {
        let Some(node) = graph.find_node(nid) else {
            continue;
        };
        eval_node(
            graph,
            node,
            p,
            n,
            hit_prim,
            images,
            sdf,
            max_noise_octaves,
            &mut outs,
        );
    }
    use crate::shader_nodes::NodeGraph as NG;
    let albedo = input_val(
        graph,
        output_id,
        NG::MO_ALBEDO,
        Val::V([0.66, 0.585, 0.53]),
        &outs,
    )
    .as_v();
    let roughness = input_val(graph, output_id, NG::MO_ROUGHNESS, Val::F(0.65), &outs).as_f();
    let metallic = input_val(graph, output_id, NG::MO_METALLIC, Val::F(0.0), &outs).as_f();
    let em_col = input_val(
        graph,
        output_id,
        NG::MO_EMISSION,
        Val::V([0.0, 0.0, 0.0]),
        &outs,
    )
    .as_v();
    let em_int =
        input_val(graph, output_id, NG::MO_EMISSION_INTENSITY, Val::F(0.0), &outs).as_f();
    let ao = input_val(graph, output_id, NG::MO_AO, Val::F(1.0), &outs)
        .as_f()
        .clamp(0.0, 1.0);
    let height = input_val(graph, output_id, NG::MO_HEIGHT, Val::F(0.0), &outs).as_f();
    let mut normal = input_val(
        graph,
        output_id,
        NG::MO_NORMAL,
        Val::V([0.0, 0.0, 1.0]),
        &outs,
    )
    .as_v();
    if graph.spatial_bump_strength() {
        let strength = input_val(graph, output_id, NG::MO_BUMP_STRENGTH, Val::F(0.05), &outs).as_f().clamp(0.0, 4.0);
        normal = eval_nodes::sample_height_normal(graph, graph.find_node(output_id).unwrap(), NG::MO_HEIGHT,
            p, n, hit_prim, images, sdf, max_noise_octaves, normal, strength, 2.0 / graph.bake_res.max(16) as f32);
    }
    let displacement =
        input_val(graph, output_id, NG::MO_DISPLACEMENT, Val::F(0.0), &outs).as_f();
    // Default matches `default_sss_color` (core_math) — only read when the sss channel
    // is in the bake mask, i.e. MO_SSS_COLOR is actually wired.
    let sss_color = input_val(
        graph,
        output_id,
        NG::MO_SSS_COLOR,
        Val::V([1.0, 0.2, 0.1]),
        &outs,
    )
    .as_v();
    SampledMat {
        albedo,
        roughness: roughness.clamp(0.0, 1.0),
        metallic: metallic.clamp(0.0, 1.0),
        emission: [
            (em_col[0] * em_int).max(0.0),
            (em_col[1] * em_int).max(0.0),
            (em_col[2] * em_int).max(0.0),
        ],
        height,
        normal,
        ao,
        displacement,
        sss_color,
    }
}

/// Render the primary output of a procedural noise node through the same CPU evaluator used by
/// material-map baking. The preview follows connected upstream inputs and never duplicates noise
/// formulas in the UI layer.
pub(crate) fn render_noise_preview_rgba(
    graph: &NodeGraph,
    node_id: NodeId,
    size: usize,
) -> Option<Vec<u8>> {
    let target = graph.find_node(node_id)?;
    if !matches!(
        target.node_type,
        NodeType::Noise | NodeType::Voronoi | NodeType::MusgraveTexture
    ) || size == 0
    {
        return None;
    }

    let order = graph.topo_sort(node_id)?;
    let images = ImageMapSources::default();
    let sdf = SdfFieldSources::default();
    let mut outs = HashMap::new();
    let mut rgba = Vec::with_capacity(size * size * 4);
    let denom = size as f32;

    for y in 0..size {
        for x in 0..size {
            outs.clear();
            let p = [
                ((x as f32 + 0.5) / denom) * 2.0 - 1.0,
                ((y as f32 + 0.5) / denom) * 2.0 - 1.0,
                0.0,
            ];
            for &nid in &order {
                let node = graph.find_node(nid)?;
                eval_node(
                    graph,
                    node,
                    p,
                    [0.0, 0.0, 1.0],
                    0.0,
                    &images,
                    &sdf,
                    BAKE_MAX_OCTAVES,
                    &mut outs,
                );
            }
            let value = outs.get(&(node_id, 0)).copied()?.as_f();
            let value = if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.0
            };
            let gray = (value * 255.0).round() as u8;
            rgba.extend_from_slice(&[gray, gray, gray, 255]);
        }
    }
    Some(rgba)
}

/// Analytic height field used by surface analyzers (bake domain).
pub(super) fn sn_field_h(p: [f32; 3]) -> f32 {
    let (x, y, z) = (p[0], p[1], p[2]);
    (x * 2.1).sin() * 0.5
        + (y * 1.7).cos() * 0.35
        + ((x + y) * 3.3).sin() * 0.15
        + (z * 2.4 + x * 0.5).sin() * 0.1
}

pub(super) fn is_input_connected(graph: &NodeGraph, node_id: NodeId, input_idx: usize) -> bool {
    graph
        .connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_input == input_idx)
}

pub(super) fn input_val(
    graph: &NodeGraph,
    node_id: NodeId,
    input_idx: usize,
    default: Val,
    outs: &HashMap<(NodeId, usize), Val>,
) -> Val {
    if let Some(conn) = graph
        .connections
        .iter()
        .find(|c| c.to_node == node_id && c.to_input == input_idx)
    {
        if let Some(v) = outs.get(&(conn.from_node, conn.from_output)) {
            return *v;
        }
    }
    // Unconnected: use node.input_defaults if present, else provided default.
    if let Some(node) = graph.find_node(node_id) {
        if let Some(d) = node.input_defaults.get(input_idx) {
            let defs = Node::input_defs(&node.node_type);
            if let Some(sock) = defs.get(input_idx) {
                return match sock.socket_type {
                    SocketType::Float => Val::F(d.value_f32),
                    SocketType::Vec3 | SocketType::Color => Val::V(d.value_vec3),
                };
            }
        }
    }
    default
}

/// Position/UV for generators with **built-in projection**.
/// - Wired: use the connected value.
/// - Unconnected: use surface sample `p` (+ optional offset from the socket's manual XYZ).
///   Critical: do NOT use bare default [0,0,0] — that made triplanar a constant at origin
///   so users thought they always had to plug TexCoord/Projection.
pub(super) fn generator_sample_pos(
    graph: &NodeGraph,
    node: &Node,
    pos_idx: usize,
    p: [f32; 3],
    outs: &HashMap<(NodeId, usize), Val>,
) -> [f32; 3] {
    if is_input_connected(graph, node.id, pos_idx) {
        input_val(
            graph,
            node.id,
            pos_idx,
            Val::V(p),
            outs,
        )
        .as_v()
    } else {
        let off = def_v(node, pos_idx, [0.0, 0.0, 0.0]);
        [p[0] + off[0], p[1] + off[1], p[2] + off[2]]
    }
}

pub(super) fn def_f(node: &Node, idx: usize, fallback: f32) -> f32 {
    node.input_defaults
        .get(idx)
        .map(|d| d.value_f32)
        .unwrap_or(fallback)
}

pub(super) fn def_v(node: &Node, idx: usize, fallback: [f32; 3]) -> [f32; 3] {
    node.input_defaults
        .get(idx)
        .map(|d| d.value_vec3)
        .unwrap_or(fallback)
}


pub(super) use eval_nodes::eval_noise;

