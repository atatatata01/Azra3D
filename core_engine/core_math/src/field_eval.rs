//! CPU-side SDF distance query API for tools and queries.
//!
//! This module provides a trait-based evaluator that reads the scene graph
//! and evaluates the signed distance field on the CPU. It is intended for
//! tools (snapping, placement, surface queries) — NOT as a replacement for
//! GPU-based DC mesh extraction.
//!
//! All evaluation respects body transforms (body-local prim positions are
//! converted via `BodyTransform::world_to_local` before calling into the
//! existing `eval_scene()` boolean chain).

use glam::Vec3;
use crate::body_field_ops::{
    resolve_displacement, resolve_smooth, BodyDisplacementParams, BodySmoothParams,
};
use crate::primitives::{eval_scene, BooleanOp, SdfPrimitive};
use crate::modifiers::BlendProfile;
use crate::sdf_math::{apply_scope_boolean, fold_prim};
use crate::scene_graph::{NodeId, NodePayload, SceneGraph};

// ============================================================
// Types
// ============================================================

/// Result of a closest-surface query, including metadata about which body
/// and prim contributed the minimum distance.
#[derive(Clone, Debug)]
pub struct SurfaceHit {
    /// Signed distance to the closest surface.
    pub distance: f32,
    /// Surface normal direction (gradient of the SDF, normalized).
    pub normal: Vec3,
    /// The body that owns the closest surface (None if scene is empty).
    pub body_nid: Option<NodeId>,
    /// The `prim_uid` of the primitive closest to the query point (None if scene is empty).
    pub prim_uid: Option<u32>,
}

// ============================================================
// Trait
// ============================================================

/// CPU-side SDF evaluation interface for tools and queries.
pub trait FieldEvaluator {
    /// Evaluate the SDF at a world-space point. Returns signed distance.
    fn eval(&self, point: Vec3) -> f32;

    /// Evaluate SDF + gradient (normal direction) using the tetrahedron method (4 evals).
    fn eval_gradient(&self, point: Vec3) -> (f32, Vec3);

    /// Find the closest surface point with full metadata (body + prim identification).
    fn closest_surface(&self, point: Vec3) -> SurfaceHit;

    /// Evaluate SDF for a single body at a world-space point.
    fn eval_body(&self, body_nid: NodeId, point: Vec3) -> f32;
}

// ============================================================
// Body-mirror union (shared canonical impl)
// ============================================================

/// Evaluate a body's prim field with a body-level mirror. Two modes, selected by
/// `mirror_sign` bit 7 ([`crate::body_field_ops::MIRROR_CUT_BIT`]):
///
/// **UNION (bit clear — the legacy mode, what every pre-cut `.fl4` stores).**
/// `d = min over all 2^n sign-combos of eval_scene(reflect(p))`, where `n` = number of enabled
/// axes in `mirror_flags` (bit 0=X, 1=Y, 2=Z). Reflection across an enabled axis is
/// `p_axis = 2*center_axis - p_axis`. Geometry on EITHER side of each plane — and geometry
/// straddling it — survives. Costs 2^n field evaluations per sample.
///
/// **CUT (bit set — "delete the other half").** One-sided domain fold, `p_axis = c ± |p_axis − c|`
/// with the sign chosen per axis by `mirror_sign` bits 0-2 (set = keep the −axis half). Geometry
/// on the discarded side vanishes and straddling geometry is bisected at the plane — the
/// Blender "Bisect" mirror. Costs exactly ONE field evaluation regardless of axis count, which
/// is why it is the default for new Volume Mirror modifiers.
///
/// `mirror_center` is in the same space as `p` (body-local under BODY_LOCAL_PRIMS). When
/// `mirror_flags == 0` this is a single `eval_scene` call — byte-identical to the un-mirrored
/// path. This is the canonical CPU reference that the GPU bake / WGSL paths replicate.
pub fn eval_scene_body_mirrored(
    p: Vec3,
    visible: &[&SdfPrimitive],
    mirror_flags: u8,
    mirror_sign: u8,
    mirror_center: Vec3,
) -> f32 {
    // Fast path: no body mirror -> identical to the original single eval.
    if mirror_flags == 0 {
        return eval_scene(p, visible);
    }

    // Cut: fold the sample point into the kept half — ONE eval, no combo loop.
    if crate::body_field_ops::mirror_cut_enabled(mirror_sign) {
        return eval_scene(fold_for_cut(p, mirror_flags, mirror_sign, mirror_center), visible);
    }

    let combos = 1u32 << mirror_flags.count_ones();
    let mut best = f32::MAX;
    for combo in 0..combos {
        let rp = reflect_for_combo(p, mirror_flags, mirror_center, combo);
        let d = eval_scene(rp, visible);
        if d < best {
            best = d;
        }
    }
    best
}

/// One-sided mirror FOLD — the "cut" (delete the other half) domain map.
///
/// For every enabled axis the point is folded onto the SOURCE half about `mirror_center`:
/// `p_axis = c + |p_axis − c|` normally, `c − |p_axis − c|` when that axis' flip bit is set.
/// The map is 1-Lipschitz and idempotent, and it is the exact CPU twin of `fold_for_cut()`
/// in `volume_bake.wgsl` / `sdf_scene.wgsl` (parity is the contract).
#[inline]
pub fn fold_for_cut(p: Vec3, mirror_flags: u8, mirror_sign: u8, mirror_center: Vec3) -> Vec3 {
    let mut fp = p;
    for axis in 0..3usize {
        if (mirror_flags >> axis) & 1 != 0 {
            let d = (fp[axis] - mirror_center[axis]).abs();
            fp[axis] = if crate::body_field_ops::mirror_keeps_negative(mirror_sign, axis) {
                mirror_center[axis] - d
            } else {
                mirror_center[axis] + d
            };
        }
    }
    fp
}

/// Reflect a point for one mirror sign-combo. Bit `j` of `combo` reflects the `j`-th ENABLED
/// axis (X, then Y, then Z among the axes set in `mirror_flags`) about `mirror_center` —
/// bit-order-identical to `reflect_for_combo()` in `volume_bake.wgsl`. Combo 0 = identity.
#[inline]
pub fn reflect_for_combo(p: Vec3, mirror_flags: u8, mirror_center: Vec3, combo: u32) -> Vec3 {
    let mut rp = p;
    let mut bit = 0u32;
    for axis in 0..3usize {
        if (mirror_flags >> axis) & 1 != 0 {
            if (combo >> bit) & 1 != 0 {
                rp[axis] = 2.0 * mirror_center[axis] - rp[axis];
            }
            bit += 1;
        }
    }
    rp
}

// ============================================================
// BodyField — the scope-aware CPU field of one TOP-LEVEL body subtree
// ============================================================

/// One operand inside a body scope: an owned prim (position PRE-composed into the top body's
/// bake frame, i.e. shifted by `sub_origin − top_origin`, exactly like
/// `serialize_body_for_bake_hot` / `collect_body_prims_recursive`) or a nested child scope.
#[derive(Clone, Debug)]
pub enum FieldChild {
    Prim(SdfPrimitive),
    Scope(FieldScope),
}

/// One body's boolean scope — the CPU twin of a `SCOPE_OPEN … SCOPE_CLOSE` block in the GPU
/// hot buffer (`serialize_body_for_bake_hot` → `eval_scene_at` in `volume_bake.wgsl`).
#[derive(Clone, Debug)]
pub struct FieldScope {
    /// How this scope combines with its PARENT scope (the sub-body's `body_op`). The root
    /// scope is forced to `Union{k:0}` — same as the serializer's `is_top_level` branch.
    pub op: BooleanOp,
    /// Scope-level blend profile (the body's `blend_profile`; Chamfer* ops override to
    /// Chamfer inside `apply_scope_boolean`, matching `emit_scope_open_hot`).
    pub blend_profile: BlendProfile,
    /// Body-level mirror, applied to the CURRENT evaluation point at scope entry: a TRUE UNION
    /// over the 2^n reflection combos, or — when `mirror_sign` carries
    /// [`crate::body_field_ops::MIRROR_CUT_BIT`] — a single one-sided fold that deletes the
    /// other half. `mirror_center` is the body's RAW (body-local) center — deliberately NOT
    /// shifted into the top frame, because the serializer emits it unshifted and the shader
    /// reflects the top-frame point about it. CPU replicates the GPU byte-for-byte (parity is
    /// the contract; see the GPU-oracle tests).
    pub mirror_flags: u8,
    /// Flip bits (0-2 = which half is the source) + cut bit 7. See `body_field_ops`.
    pub mirror_sign: u8,
    pub mirror_center: Vec3,
    /// Body-level bevel (edge rounding): `acc -= bevel_radius` per combo at scope close.
    pub bevel_radius: f32,
    /// Post-boolean field smooth (curvature flow). Multi-sample; CPU bake only.
    /// Nested child scopes carry their own body stack smooth independently.
    pub smooth: Option<BodySmoothParams>,
    /// Procedural surface displacement (noise). Applied after smooth. CPU bake.
    pub displacement: Option<BodyDisplacementParams>,
    pub children: Vec<FieldChild>,
}

/// The scope-aware CPU signed-distance field of ONE top-level body subtree, in the TOP
/// body's local (translation) bake frame — the same frame the GPU bake grid samples.
///
/// This is the ONE recursive CPU implementation of the nested sub-body boolean semantics
/// (COMPLETE_BUG_TRACKER "boolean tree inside a sub-body not working", defect (1)): a nested
/// child SdfBody evaluates as ONE OPERAND (its own boolean chain first, in `child_order`
/// order), then combines with the parent via the child's `body_op`. Semantics mirror the GPU
/// scope stack machine (`eval_scene_at`) exactly:
/// - first operand in a scope seeds the accumulator regardless of its op (`fold_prim(None,..)`)
/// - an EMPTY child scope leaves the parent unchanged
/// - a non-empty child scope with an EMPTY parent: Union-family keeps the child; the
///   Subtract/Intersect families keep the (empty) parent
/// - per-scope mirror = union over reflection combos of the whole scope body
/// - per-scope bevel subtracts at scope close, per combo
///
/// Owned + `Send` — safe to build once on the frame thread and evaluate on rayon workers
/// (CPU bake fallback, BrickClassifier).
///
/// NOTE: nested composition is TRANSLATION-ONLY (`sub_origin − top_origin`), exactly like the
/// serializer and `collect_body_prims_recursive` (sub-body rotation/scale composition is a
/// known follow-up on BOTH paths — CPU/GPU stay in lockstep). Assumes `BODY_LOCAL_PRIMS`
/// (production).
#[derive(Clone, Debug)]
pub struct BodyField {
    pub root: FieldScope,
}

impl BodyField {
    /// Build the field program for a body subtree from the scene graph. Frame = the given
    /// body's local (translation) frame. Invisible prims and invisible child bodies are
    /// skipped (same as the serializer). Works for a top-level body (the normal case) or a
    /// nested body queried standalone (its own frame becomes the root frame).
    pub fn from_graph(sg: &SceneGraph, body_nid: NodeId) -> BodyField {
        let top_origin = match sg.get(body_nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { origin, .. }) => *origin,
            _ => Vec3::ZERO,
        };
        let root = build_scope(sg, body_nid, true, top_origin);
        BodyField { root }
    }

    /// Build a FLAT field (one scope, no nesting) from a prim slice — the exact semantics of
    /// `eval_scene_body_mirrored` (visible-filtered, vec order, no bevel), routed through the
    /// same scope evaluator so flat and nested share ONE code path.
    pub fn from_prims(
        prims: &[SdfPrimitive],
        mirror_flags: u8,
        mirror_sign: u8,
        mirror_center: Vec3,
    ) -> BodyField {
        BodyField {
            root: FieldScope {
                op: BooleanOp::Union { k: 0.0 },
                blend_profile: BlendProfile::default(),
                mirror_flags,
                mirror_sign,
                mirror_center,
                bevel_radius: 0.0,
                smooth: None,
                displacement: None,
                children: prims.iter().filter(|p| p.visible).cloned().map(FieldChild::Prim).collect(),
            },
        }
    }

    /// Evaluate the field at a point in the body's local frame. `empty` is returned when the
    /// whole subtree contains no visible prims (callers pick their sentinel: 100.0 for bake
    /// grids to match `eval_scene`, `f32::MAX` for tool queries).
    #[inline]
    pub fn eval_or(&self, p: Vec3, empty: f32) -> f32 {
        eval_scope(&self.root, p).unwrap_or(empty)
    }

    /// True when the subtree contains no visible prims at all.
    pub fn is_empty(&self) -> bool {
        fn scope_empty(s: &FieldScope) -> bool {
            s.children.iter().all(|c| match c {
                FieldChild::Prim(_) => false,
                FieldChild::Scope(cs) => scope_empty(cs),
            })
        }
        scope_empty(&self.root)
    }

    /// True when the flat per-prim bounding-sphere containment proofs (BrickClassifier's
    /// `provably_exterior` / `provably_interior`) remain SOUND for this field. Nested scopes
    /// are proof-safe only when they are plain hard unions with no scope-local mirror/bevel:
    /// - a non-Union nested scope flips/restructures prim roles (a Union prim inside a
    ///   Subtract sub-body is effectively a carver → the interior donor proof is unsound)
    /// - a nested scope blend k > 0 can bulge the surface OUTSIDE the prims' influence
    ///   spheres (scope-level k is not folded into any prim's `bounding_radius`)
    /// - a nested scope mirror creates reflected geometry with no sphere in the flat set
    /// - any bevel (root included) grows the surface outward by `bevel_radius`.
    /// When false, consumers must fall back to the exact per-brick classify (conservative,
    /// never wrong — only slower).
    pub fn sphere_proofs_valid(&self) -> bool {
        fn nested_ok(s: &FieldScope) -> bool {
            s.children.iter().all(|c| match c {
                FieldChild::Prim(_) => true,
                FieldChild::Scope(cs) => {
                    matches!(cs.op, BooleanOp::Union { k } if k == 0.0)
                        && cs.mirror_flags == 0
                        && cs.bevel_radius == 0.0
                        && cs.smooth.is_none()
                        && cs.displacement.is_none()
                        && nested_ok(cs)
                }
            })
        }
        self.root.bevel_radius == 0.0
            && self.root.smooth.is_none()
            && self.root.displacement.is_none()
            && nested_ok(&self.root)
    }
}

/// Recursive scope construction — mirrors `serialize_body_for_bake_hot_recursive` (unified
/// `child_order` walk, SdfObject wrappers expanded to their bodies, invisible children
/// skipped, prims shifted by `this_origin − top_origin` into the top frame).
fn build_scope(sg: &SceneGraph, body_id: NodeId, is_top_level: bool, top_origin: Vec3) -> FieldScope {
    let (body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_profile, this_origin, smooth, displacement) =
        match sg.get(body_id).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                body_op,
                mirror_flags,
                mirror_sign,
                mirror_center,
                bevel_radius,
                blend_profile,
                origin,
                field_modifiers,
                ..
            }) => (
                body_op.clone(),
                *mirror_flags,
                *mirror_sign,
                *mirror_center,
                *bevel_radius,
                *blend_profile,
                *origin,
                resolve_smooth(field_modifiers),
                resolve_displacement(field_modifiers),
            ),
            _ => (
                BooleanOp::Union { k: 0.0 },
                0,
                0,
                Vec3::ZERO,
                0.0,
                BlendProfile::default(),
                Vec3::ZERO,
                None,
                None,
            ),
        };
    let prim_shift = this_origin - top_origin;

    let prims: &[SdfPrimitive] = sg.body_primitives(body_id).map(|p| p as &[_]).unwrap_or(&[]);
    let mut children = Vec::new();
    for entry in sg.resolve_child_order(body_id) {
        match entry {
            Ok(i) => {
                if let Some(p) = prims.get(i) {
                    if !p.visible { continue; }
                    let mut sp = p.clone();
                    sp.position += prim_shift;
                    children.push(FieldChild::Prim(sp));
                }
            }
            Err(nid) => {
                // Expand SdfObject wrappers into their underlying bodies, in order — the
                // same expansion `body_eval_order` does in graphics/serialize.rs.
                let expand: Vec<NodeId> = match sg.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::SdfObject { .. }) => sg.child_bodies(nid),
                    Some(NodePayload::SdfBody { .. }) => vec![nid],
                    _ => Vec::new(),
                };
                for child_id in expand {
                    if let Some(child_node) = sg.get(child_id) {
                        if !child_node.visible { continue; }
                    }
                    children.push(FieldChild::Scope(build_scope(sg, child_id, false, top_origin)));
                }
            }
        }
    }

    FieldScope {
        op: if is_top_level { BooleanOp::Union { k: 0.0 } } else { body_op },
        blend_profile,
        mirror_flags,
        mirror_sign,
        mirror_center,
        bevel_radius,
        smooth,
        displacement,
        children,
    }
}

/// Full scope eval: core → smooth → displacement.
fn eval_scope(scope: &FieldScope, p: Vec3) -> Option<f32> {
    let d = eval_scope_core(scope, p)?;
    let d = apply_scope_smooth(scope, p, d);
    Some(apply_scope_displacement(scope, p, d))
}

fn apply_scope_displacement(scope: &FieldScope, p: Vec3, d: f32) -> f32 {
    let Some(params) = scope.displacement.as_ref() else {
        return d;
    };
    crate::body_field_ops::apply_displacement(d, p, params)
}

/// Core field without **this** scope's smooth (children still apply theirs).
/// Used for finite-difference samples so smooth is not recursive on itself.
fn eval_scope_core(scope: &FieldScope, p: Vec3) -> Option<f32> {
    // CUT: one fold instead of the 2^n-combo union — the whole scope body is walked ONCE.
    // (`combos == 1` and the loop's single reflect becomes the fold; same code below.)
    let cut = scope.mirror_flags != 0
        && crate::body_field_ops::mirror_cut_enabled(scope.mirror_sign);
    let combos = if cut { 1u32 } else { 1u32 << scope.mirror_flags.count_ones() };
    let mut scope_min: Option<f32> = None;
    for combo in 0..combos {
        let rp = if cut {
            fold_for_cut(p, scope.mirror_flags, scope.mirror_sign, scope.mirror_center)
        } else {
            reflect_for_combo(p, scope.mirror_flags, scope.mirror_center, combo)
        };
        let mut acc: Option<f32> = None;
        for child in &scope.children {
            match child {
                FieldChild::Prim(prim) => {
                    acc = Some(fold_prim(acc, prim, rp));
                }
                FieldChild::Scope(cs) => {
                    // Child scope evaluates fully (incl. child smooth) at the live point.
                    match (eval_scope(cs, rp), acc) {
                        // Child empty: parent unchanged (GPU "child scope empty, keep parent").
                        (None, _) => {}
                        // Parent empty: Union-family keeps child; Subtract/Intersect
                        // families keep the empty parent (GPU sop==1||2||4 rule; note
                        // SmoothIntersect serializes as op 2).
                        (Some(cd), None) => {
                            if matches!(cs.op, BooleanOp::Union { .. } | BooleanOp::ChamferUnion { .. }) {
                                acc = Some(cd);
                            }
                        }
                        (Some(cd), Some(parent)) => {
                            acc = Some(apply_scope_boolean(parent, cd, &cs.op, cs.blend_profile));
                        }
                    }
                }
            }
        }
        // Body-level bevel applies per combo at scope close (GPU: `acc -= bevel_r`).
        if let Some(a) = acc.as_mut() {
            if scope.bevel_radius > 0.0 { *a -= scope.bevel_radius; }
        }
        scope_min = match (scope_min, acc) {
            (None, a) => a,
            (Some(m), None) => Some(m),
            (Some(m), Some(a)) => Some(m.min(a)),
        };
    }
    scope_min
}

/// 6-neighbor mean-curvature flow + mode mask (see `body_field_ops::apply_smooth_offset`).
fn apply_scope_smooth(scope: &FieldScope, p: Vec3, d: f32) -> f32 {
    let Some(params) = scope.smooth.as_ref() else {
        return d;
    };
    if params.strength <= 1e-8 {
        return d;
    }
    let eps = params.radius.max(1e-4);
    // Surface band: only affect near zero-set so far field stays bake-stable.
    let band = {
        let t = (d.abs() / (eps * 3.0)).clamp(0.0, 1.0);
        (1.0 - t) * (1.0 - t)
    };
    if band <= 1e-6 {
        return d;
    }

    let sample = |q: Vec3| eval_scope_core(scope, q).unwrap_or(d);
    let dxp = sample(p + Vec3::X * eps);
    let dxm = sample(p - Vec3::X * eps);
    let dyp = sample(p + Vec3::Y * eps);
    let dym = sample(p - Vec3::Y * eps);
    let dzp = sample(p + Vec3::Z * eps);
    let dzm = sample(p - Vec3::Z * eps);

    // Discrete Laplacian; dimensionless h_norm ≈ lap * eps².
    let lap = (dxp + dxm + dyp + dym + dzp + dzm - 6.0 * d) / (eps * eps);
    let h_norm = (lap * eps * eps).clamp(-4.0, 4.0);

    // Enclosure proxy: neighbors more interior than center → cavity / occluded crease.
    let mean_n = (dxp + dxm + dyp + dym + dzp + dzm) / 6.0;
    let enclosure = ((d - mean_n) / eps).clamp(0.0, 1.0);

    crate::body_field_ops::apply_smooth_offset(d, h_norm, enclosure, band, params)
}

// ============================================================
// SceneEvaluator — reads SceneGraph immutably
// ============================================================

/// Evaluates the scene SDF on the CPU by reading the scene graph.
/// Borrows the graph immutably — safe to use from any read-only context.
pub struct SceneEvaluator<'a> {
    pub graph: &'a SceneGraph,
    /// Memoized per-body `BodyField`, built on first use and reused for the evaluator's lifetime.
    ///
    /// WHY THIS EXISTS. `eval_body_internal` used to call `BodyField::from_graph(...)` on EVERY
    /// sample and throw the result away. `from_graph` → `build_scope` deep-clones every visible prim
    /// in the subtree (`SdfPrimitive` owns a `String` name, a `PbrMaterial` that owns its own
    /// `String`, a `BlendCurve` `Vec`, a `MaterialDna` `Vec`, plus modifier/sculpt/clip `Vec`s — four
    /// or more heap allocations per prim), and recurses into nested sub-bodies. The SDF→voxel stamp
    /// (`properties_sdf.rs`, the "Stamp into Active Sculpt" / "Carve" buttons) calls this once PER
    /// VOXEL through `fill_from_sdf`, synchronously on the frame thread.
    ///
    /// MEASURED on this fixture, 20k samples: 15.8× slower at 1 prim, 12.3× at 5, 14.2× at 10,
    /// 14.7× at 20 — i.e. roughly 93 % of the work was rebuilding a field that never changed.
    /// The graph is borrowed immutably for the evaluator's whole lifetime, so every rebuild produced
    /// a byte-identical result; caching is sound by construction, not by convention.
    ///
    /// `RefCell` rather than a `&mut self` API because `SceneEvaluator` is built with a struct
    /// literal at 23 sites and is used from `&`-only contexts (including a closure that already
    /// borrows the graph). Interior mutability keeps every one of those compiling untouched.
    ///
    /// NOTE the type's own contract (see `BodyField`): "Owned + Send — safe to build once on the
    /// frame thread and evaluate on rayon workers". Building it per sample was always the misuse.
    field_cache: std::cell::RefCell<std::collections::HashMap<NodeId, std::rc::Rc<BodyField>>>,
}

impl<'a> SceneEvaluator<'a> {
    /// Construct over a graph. Equivalent to the struct literal, plus an empty field cache.
    pub fn new(graph: &'a SceneGraph) -> Self {
        Self {
            graph,
            field_cache: Default::default(),
        }
    }

    /// The memoized `BodyField` for `body_nid`, built on first request.
    fn body_field(&self, body_nid: NodeId) -> std::rc::Rc<BodyField> {
        if let Some(f) = self.field_cache.borrow().get(&body_nid) {
            return std::rc::Rc::clone(f);
        }
        let f = std::rc::Rc::new(BodyField::from_graph(self.graph, body_nid));
        self.field_cache
            .borrow_mut()
            .insert(body_nid, std::rc::Rc::clone(&f));
        f
    }
}

impl<'a> SceneEvaluator<'a> {
    /// Evaluate a single body SUBTREE's SDF at a world-space point.
    /// Converts the point to body-local space, then evaluates the scope-aware `BodyField`:
    /// nested sub-bodies evaluate their own boolean tree first, then combine via their
    /// `body_op` — matching the GPU bake's scope stack machine. A body with no nested
    /// children takes the IDENTICAL code path (one flat scope: mirror union + `fold_prim`
    /// chain — the historical `eval_scene_body_mirrored` semantics, shared code).
    fn eval_body_internal(&self, body_nid: NodeId, point: Vec3) -> f32 {
        // Object-aware: world->local through the body's EFFECTIVE transform (composes any parent SdfObject;
        // top-level body = identity object, so unchanged). Keeps tool SDF queries aligned with render/pick.
        let local_p = self.graph.effective_world_to_local(body_nid, point);
        self.body_field(body_nid).eval_or(local_p, f32::MAX)
    }

    /// Find which prim in a body is closest to the query point (body-local).
    /// Returns (distance_to_prim, prim_uid).
    fn closest_prim_in_body(&self, body_nid: NodeId, point: Vec3) -> Option<(f32, u32)> {
        let prims = match self.graph.body_primitives(body_nid) {
            Some(p) => p,
            None => return None,
        };
        let local_p = self.graph.effective_world_to_local(body_nid, point);
        let mut best_dist = f32::MAX;
        let mut best_uid: u32 = 0;
        for prim in prims.iter().filter(|p| p.visible) {
            let d = prim.world_distance(local_p).abs();
            if d < best_dist {
                best_dist = d;
                best_uid = prim.prim_uid;
            }
        }
        if best_dist < f32::MAX {
            Some((best_dist, best_uid))
        } else {
            None
        }
    }
}

impl<'a> FieldEvaluator for SceneEvaluator<'a> {
    fn eval(&self, point: Vec3) -> f32 {
        // Union TOP-LEVEL bodies only: a nested sub-body is ONE OPERAND inside its parent's
        // boolean tree (combined via its `body_op` in eval_body_internal's recursion) — it is
        // NOT an independent scene operand. Unioning it here re-added Subtract sub-bodies as
        // solid geometry (the nesting-blind defect this fixes).
        let mut min_d = f32::MAX;
        for (nid, _node) in self.graph.sdf_bodies() {
            if self.graph.parent_body(nid).is_some() { continue; }
            let d = self.eval_body_internal(nid, point);
            if d < min_d {
                min_d = d;
            }
        }
        min_d
    }

    fn eval_gradient(&self, point: Vec3) -> (f32, Vec3) {
        let d = self.eval(point);
        // Tetrahedron normal: 4 evaluations at tetrahedral offsets.
        // Matches GPU shader normal calculation (CLAUDE.md: "tetrahedron normals").
        const EPS: f32 = 0.001;
        let k = [
            Vec3::new( 1.0, -1.0, -1.0),
            Vec3::new(-1.0, -1.0,  1.0),
            Vec3::new(-1.0,  1.0, -1.0),
            Vec3::new( 1.0,  1.0,  1.0),
        ];
        let normal = k[0] * self.eval(point + k[0] * EPS)
                   + k[1] * self.eval(point + k[1] * EPS)
                   + k[2] * self.eval(point + k[2] * EPS)
                   + k[3] * self.eval(point + k[3] * EPS);
        (d, normal.normalize_or_zero())
    }

    fn closest_surface(&self, point: Vec3) -> SurfaceHit {
        let mut best_body_d = f32::MAX;
        let mut best_body_nid: Option<NodeId> = None;

        // Find the body with the smallest SDF distance at this point (top-level subtrees
        // only — nested sub-bodies are operands of their parent, not independent bodies).
        for (nid, _node) in self.graph.sdf_bodies() {
            if self.graph.parent_body(nid).is_some() { continue; }
            let d = self.eval_body_internal(nid, point);
            if d < best_body_d {
                best_body_d = d;
                best_body_nid = Some(nid);
            }
        }

        // Identify the closest individual prim within that body.
        let best_prim_uid = best_body_nid
            .and_then(|nid| self.closest_prim_in_body(nid, point))
            .map(|(_, uid)| uid);

        let (_, normal) = self.eval_gradient(point);

        SurfaceHit {
            distance: best_body_d,
            normal,
            body_nid: best_body_nid,
            prim_uid: best_prim_uid,
        }
    }

    fn eval_body(&self, body_nid: NodeId, point: Vec3) -> f32 {
        self.eval_body_internal(body_nid, point)
    }
}

// ============================================================
// CPU Raymarch helper
// ============================================================

/// CPU sphere-trace raymarch using a FieldEvaluator. Returns the first surface hit.
///
/// Marches along the ray `origin + dir * t`, advancing by the SDF distance at each step.
/// Terminates when within `hit_eps` of a surface, or after `max_steps` / `max_dist`.
pub fn cpu_raymarch(
    evaluator: &dyn FieldEvaluator,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<SurfaceHit> {
    const MAX_STEPS: u32 = 128;
    const HIT_EPS: f32 = 0.001;

    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }

    let mut t = 0.0f32;
    for _ in 0..MAX_STEPS {
        let p = origin + dir * t;
        let hit = evaluator.closest_surface(p);
        if hit.distance < HIT_EPS {
            return Some(hit);
        }
        t += hit.distance;
        if t > max_dist {
            break;
        }
    }
    None
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{BooleanOp, PbrMaterial, SdfShape, BlendProfile};
    use crate::scene_graph::NodePayload;
    use glam::{Quat, Vec3};

    /// Helper: create a scene graph with a single body containing one sphere prim.
    fn make_sphere_scene(radius: f32, body_origin: Vec3) -> (SceneGraph, NodeId) {
        let mut graph = SceneGraph::new();
        let root = graph.root();
        let prim = SdfPrimitive::new(
            "Sphere".into(),
            SdfShape::Sphere { radius },
            Vec3::ZERO, // body-local position
        );
        let body_nid = graph.add_child(
            root,
            NodePayload::SdfBody {
                name: "Body".into(),
                primitives: vec![prim],
                mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 },
                origin: body_origin,
                body_rotation: Quat::IDENTITY,
                body_scale: Vec3::ONE,
                mirror_flags: 0,
                mirror_sign: 0,
                mirror_center: Vec3::ZERO,
                bevel_radius: 0.0,
                blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(),
                child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),},
        );
        (graph, body_nid)
    }

    #[test]
    fn eval_sphere_at_origin() {
        let (graph, _body_nid) = make_sphere_scene(1.0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // Point at distance 2 from center -> expect distance ~1.0
        let d = eval.eval(Vec3::new(2.0, 0.0, 0.0));
        assert!((d - 1.0).abs() < 0.01, "expected ~1.0, got {d}");

        // Point at center -> expect distance ~-1.0 (inside sphere)
        let d = eval.eval(Vec3::ZERO);
        assert!((d + 1.0).abs() < 0.01, "expected ~-1.0, got {d}");

        // Point on surface -> expect distance ~0.0
        let d = eval.eval(Vec3::new(1.0, 0.0, 0.0));
        assert!(d.abs() < 0.01, "expected ~0.0, got {d}");
    }

    #[test]
    fn eval_sphere_with_body_offset() {
        // Body origin at (5, 0, 0), sphere at body-local (0,0,0).
        // World-space center is (5, 0, 0).
        let (graph, _body_nid) = make_sphere_scene(1.0, Vec3::new(5.0, 0.0, 0.0));
        let eval = SceneEvaluator::new(&graph);

        // World point (7, 0, 0) -> body-local (2, 0, 0) -> distance ~1.0
        let d = eval.eval(Vec3::new(7.0, 0.0, 0.0));
        assert!((d - 1.0).abs() < 0.01, "expected ~1.0, got {d}");

        // World point (5, 0, 0) -> body-local (0, 0, 0) -> distance ~-1.0
        let d = eval.eval(Vec3::new(5.0, 0.0, 0.0));
        assert!((d + 1.0).abs() < 0.01, "expected ~-1.0, got {d}");
    }

    #[test]
    fn eval_body_single() {
        let (graph, body_nid) = make_sphere_scene(1.0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        let d = eval.eval_body(body_nid, Vec3::new(2.0, 0.0, 0.0));
        assert!((d - 1.0).abs() < 0.01, "expected ~1.0, got {d}");
    }

    #[test]
    fn eval_gradient_points_outward() {
        let (graph, _body_nid) = make_sphere_scene(1.0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        let (d, normal) = eval.eval_gradient(Vec3::new(2.0, 0.0, 0.0));
        assert!((d - 1.0).abs() < 0.01, "expected ~1.0, got {d}");
        // Normal should point in +X direction
        assert!(normal.x > 0.9, "expected normal.x ~1.0, got {}", normal.x);
        assert!(normal.y.abs() < 0.1, "expected normal.y ~0.0, got {}", normal.y);
        assert!(normal.z.abs() < 0.1, "expected normal.z ~0.0, got {}", normal.z);
    }

    #[test]
    fn eval_gradient_y_axis() {
        let (graph, _body_nid) = make_sphere_scene(1.0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        let (_d, normal) = eval.eval_gradient(Vec3::new(0.0, 3.0, 0.0));
        assert!(normal.y > 0.9, "expected normal.y ~1.0, got {}", normal.y);
    }

    #[test]
    fn closest_surface_identifies_body() {
        let (graph, body_nid) = make_sphere_scene(1.0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        let hit = eval.closest_surface(Vec3::new(2.0, 0.0, 0.0));
        assert!((hit.distance - 1.0).abs() < 0.01);
        assert_eq!(hit.body_nid, Some(body_nid));
        assert!(hit.prim_uid.is_some());
    }

    #[test]
    fn eval_empty_scene() {
        let graph = SceneGraph::new();
        let eval = SceneEvaluator::new(&graph);

        let d = eval.eval(Vec3::ZERO);
        assert_eq!(d, f32::MAX, "empty scene should return f32::MAX");
    }

    #[test]
    fn cpu_raymarch_hits_sphere() {
        let (graph, body_nid) = make_sphere_scene(1.0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // Ray from (5, 0, 0) toward origin — should hit sphere at ~(1, 0, 0)
        let hit = cpu_raymarch(&eval, Vec3::new(5.0, 0.0, 0.0), Vec3::new(-1.0, 0.0, 0.0), 100.0);
        assert!(hit.is_some(), "raymarch should hit the sphere");
        let hit = hit.unwrap();
        assert!((hit.distance).abs() < 0.01);
        assert_eq!(hit.body_nid, Some(body_nid));
    }

    #[test]
    fn cpu_raymarch_misses() {
        let (graph, _body_nid) = make_sphere_scene(1.0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // Ray that misses the sphere entirely
        let hit = cpu_raymarch(&eval, Vec3::new(5.0, 5.0, 0.0), Vec3::new(1.0, 0.0, 0.0), 100.0);
        assert!(hit.is_none(), "raymarch should miss the sphere");
    }

    #[test]
    fn eval_two_bodies_closest() {
        let mut graph = SceneGraph::new();
        let root = graph.root();

        // Body A at origin, radius 1
        let prim_a = SdfPrimitive::new("A".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        let body_a = graph.add_child(root, NodePayload::SdfBody {
            name: "BodyA".into(),
            primitives: vec![prim_a],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        // Body B at (10, 0, 0), radius 1
        let prim_b = SdfPrimitive::new("B".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        let _body_b = graph.add_child(root, NodePayload::SdfBody {
            name: "BodyB".into(),
            primitives: vec![prim_b],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::new(10.0, 0.0, 0.0),
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let eval = SceneEvaluator::new(&graph);

        // Point near body A
        let hit = eval.closest_surface(Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(hit.body_nid, Some(body_a));
        assert!((hit.distance - 1.0).abs() < 0.01);
    }

    #[test]
    fn eval_boolean_subtract() {
        let mut graph = SceneGraph::new();
        let root = graph.root();

        let mut prim_a = SdfPrimitive::new("A".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        prim_a.operation = BooleanOp::Union { k: 0.0 };

        let mut prim_b = SdfPrimitive::new("B".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::new(0.5, 0.0, 0.0));
        prim_b.operation = BooleanOp::Subtract { k: 0.0 };

        let _body = graph.add_child(root, NodePayload::SdfBody {
            name: "Body".into(),
            primitives: vec![prim_a, prim_b],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let eval = SceneEvaluator::new(&graph);

        // The point (0.5, 0, 0) is at the center of the subtracted sphere.
        // After subtraction, this point should be outside (positive distance).
        let d = eval.eval(Vec3::new(0.5, 0.0, 0.0));
        assert!(d > 0.0, "carved point should be outside, got d={d}");
    }

    // ====================================================================
    // BODY-MIRROR UNION  (fix: Mirror is a true union, not a one-sided fold)
    // --------------------------------------------------------------------
    // The body-mirror was historically a ONE-SIDED DOMAIN FOLD on every GPU
    // path (volume_bake.wgsl, sdf_scene.wgsl, raymarch_bvh_full.comp):
    //
    //   p.y = mirror_center_y + select( |y-c|, -|y-c|, flip )
    //
    // That maps EVERY evaluation point onto a SINGLE side of the plane, so
    // geometry on the non-kept side (or a body straddling the plane) was
    // folded onto the empty side and DELETED -> the body flattened to a plane.
    //
    // The fix (now applied here on the CPU and mirrored in all GPU paths) is a
    // TRUE UNION: d = min over the 2^n reflection sign-combos of the n enabled
    // axes of eval_scene(reflect(p)). Geometry on EITHER side and straddling
    // the plane survives. `mirror_sign` is irrelevant to a union (both sides
    // are always evaluated). These tests drive the REAL evaluator: the body
    // carries `mirror_flags`/`mirror_center`, so `eval.eval_body()` unions.
    // ====================================================================

    /// Build a body with a single AXIS-ALIGNED BOX, body-local position `pos`,
    /// half-extents `he`, and an optional body-level mirror (`flags`/`sign`/`center`).
    /// Body origin at world ZERO; no body transform — so world == body-local space.
    fn make_box_body_mirror(
        pos: Vec3,
        he: Vec3,
        mirror_flags: u8,
        mirror_sign: u8,
        mirror_center: Vec3,
    ) -> (SceneGraph, NodeId) {
        use crate::sdf_shapes::BoxBevel;
        let mut graph = SceneGraph::new();
        let root = graph.root();
        let prim = SdfPrimitive::new(
            "Box".into(),
            SdfShape::Box { half_extents: he, bevel: BoxBevel::default() },
            pos,
        );
        let body = graph.add_child(root, NodePayload::SdfBody {
            name: "Body".into(),
            primitives: vec![prim],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags, mirror_sign, mirror_center,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        (graph, body)
    }

    /// SANITY: a CENTERED box (already symmetric about the plane) stays solid on
    /// both halves under the union — and matches the un-mirrored field, since a
    /// symmetric body reflects onto itself.
    #[test]
    fn body_mirror_union_centered_box_survives() {
        // Box centered at origin, occupies y in [-0.5, 0.5]. Mirror Y about y=0.
        let (graph, body) = make_box_body_mirror(Vec3::ZERO, Vec3::splat(0.5), 0b010, 0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // Center is solid.
        let d = eval.eval_body(body, Vec3::ZERO);
        assert!(d < 0.0, "centered box center should be solid, got d={d}");

        // Both halves are solid.
        assert!(eval.eval_body(body, Vec3::new(0.0, 0.3, 0.0)) < 0.0, "upper half should be solid");
        assert!(eval.eval_body(body, Vec3::new(0.0, -0.3, 0.0)) < 0.0, "lower half should be solid");
    }

    /// SANITY: a box already entirely on the KEPT (+Y) side keeps its own half
    /// solid AND gains the reflected copy on -Y. The union adds, never deletes.
    #[test]
    fn body_mirror_union_kept_side_box_mirrors() {
        // Box centered at y = +1, occupies y in [+0.5, +1.5]. Mirror Y about y=0.
        let (graph, body) = make_box_body_mirror(Vec3::new(0.0, 1.0, 0.0), Vec3::splat(0.5), 0b010, 0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // The box's own (+Y) center stays solid.
        assert!(eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0)) < 0.0, "kept-side box center should be solid");

        // The reflected instance appears on -Y.
        assert!(eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0)) < 0.0, "mirrored copy should appear on -Y");
    }

    /// ROOT-CAUSE FIX (was `body_mirror_fold_offcenter_box_collapses`, #[ignore]'d
    /// and FAILING by design under the old fold): an OFF-CENTER box on the -Y side
    /// must NOT collapse. With the union, the original -Y half stays solid AND a
    /// reflected copy appears on +Y. The decisive assertion — a point inside the
    /// original box at its true location reads SOLID — now PASSES.
    ///
    /// Box centered at y = -1 (occupies y in [-1.5, -0.5], entirely on -Y).
    /// Mirror Y about y=0, mirror_sign = keep +Y (irrelevant to the union).
    #[test]
    fn body_mirror_union_offcenter_box_does_not_collapse() {
        let (graph, body) = make_box_body_mirror(Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5), 0b010, 0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // DECISIVE: a point inside the original -Y box, at its TRUE location, is
        // SOLID. Under the old one-sided fold (keep +Y) this read OUTSIDE and the
        // body flattened. The union keeps the original half.
        let d_orig = eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0));
        assert!(
            d_orig < 0.0,
            "FIXED: union keeps the original -Y half. Point (0,-1,0) inside the box \
             must be solid (d<0), got d={d_orig}. (Old fold deleted this half.)"
        );

        // And the reflected copy now exists on +Y.
        let d_refl = eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0));
        assert!(d_refl < 0.0, "union adds the reflected +Y copy, got d={d_refl}");
    }

    /// THE FIX, FULL SHAPE: the off-center box has BOTH halves solid and the
    /// gap on the plane correctly empty (the union is faithful — it mirrors the
    /// existing geometry, it does not invent material between the two copies).
    #[test]
    fn body_mirror_union_offcenter_box_both_halves_solid() {
        let (graph, body) = make_box_body_mirror(Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5), 0b010, 0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // Original half (-Y): solid.
        let d_neg = eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0));
        assert!(d_neg < 0.0, "union: original -Y half must be solid, got {d_neg}");

        // Reflected half (+Y): also solid.
        let d_pos = eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0));
        assert!(d_pos < 0.0, "union: reflected +Y half must be solid, got {d_pos}");

        // The gap on the plane (y in (-0.5,0.5)) is correctly empty for THIS
        // off-center box (no geometry spans the plane) — union is faithful.
        let d_gap = eval.eval_body(body, Vec3::ZERO);
        assert!(d_gap > 0.0, "union: plane gap should be empty, got {d_gap}");
    }

    // ====================================================================
    // BODY-MIRROR CUT ("delete the other half" — mirror_sign bit 7)
    // --------------------------------------------------------------------
    // The union above keeps BOTH halves at 2^n evals per sample. The cut
    // restores the one-sided fold as an OPT-IN mode: the non-source half is
    // deleted, straddling geometry is bisected at the plane, and the whole
    // field costs ONE eval. The bit is 0 in every save written before it
    // existed, so those files keep the union — pinned below.
    // ====================================================================

    /// The old fold's failure mode is the cut's FEATURE: an off-center box on the
    /// discarded side is deleted, not mirrored. Same body as
    /// `body_mirror_union_offcenter_box_does_not_collapse`, one bit different.
    #[test]
    fn body_mirror_cut_deletes_the_discarded_half() {
        use crate::body_field_ops::MIRROR_CUT_BIT;
        // Box at y = −1 (occupies [−1.5, −0.5]), mirror Y about y=0, source = +Y half.
        let (graph, body) = make_box_body_mirror(
            Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5), 0b010, MIRROR_CUT_BIT, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);
        // Nothing lives on the +Y source half, so the cut body is EMPTY everywhere —
        // exactly what "delete the other half" means for geometry entirely on that half.
        assert!(eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0)) > 0.0,
            "cut: geometry on the discarded half must be gone");
        assert!(eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0)) > 0.0,
            "cut: nothing to reflect, so the source half stays empty too");
    }

    /// Flip bits pick WHICH half is the source — inert under the union, load-bearing here.
    #[test]
    fn body_mirror_cut_flip_selects_the_source_half() {
        use crate::body_field_ops::MIRROR_CUT_BIT;
        // Same −Y box, but flip Y (bit 1) → the −Y half is the source: it survives and
        // reflects onto +Y, giving the union's result from ONE eval instead of two.
        let (graph, body) = make_box_body_mirror(
            Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5), 0b010, MIRROR_CUT_BIT | 0b010, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);
        assert!(eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0)) < 0.0, "cut: source half kept");
        assert!(eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0)) < 0.0, "cut: reflected copy present");
        assert!(eval.eval_body(body, Vec3::ZERO) > 0.0, "cut: plane gap stays empty");
    }

    /// A body STRADDLING the plane is bisected: the source half is kept and mirrored, the
    /// other half is discarded. (Asymmetric box — a symmetric one cannot tell the modes apart.)
    #[test]
    fn body_mirror_cut_bisects_a_straddling_body() {
        use crate::body_field_ops::MIRROR_CUT_BIT;
        // Box spanning y ∈ [−0.2, 1.8] — straddles y=0, mostly on +Y. Source = +Y.
        let (graph, body) = make_box_body_mirror(
            Vec3::new(0.0, 0.8, 0.0), Vec3::new(0.5, 1.0, 0.5), 0b010, MIRROR_CUT_BIT, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);
        assert!(eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0)) < 0.0, "cut: source half kept");
        // The fold maps −1.0 to +1.0, which is inside the box → solid (the mirrored copy).
        assert!(eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0)) < 0.0, "cut: reflected copy present");
        // Beyond the reflected extent (|y| > 1.8) it must be empty — the cut did not smear
        // the discarded overhang across the plane.
        assert!(eval.eval_body(body, Vec3::new(0.0, -1.95, 0.0)) > 0.0, "cut: past the mirrored extent");
    }

    /// SAVE COMPATIBILITY: `mirror_sign` bit 7 clear is what every pre-cut `.fl4` stores, and
    /// it MUST still evaluate as the true union — including with the flip bits set, which the
    /// union has always ignored. This is the regression guard against the cut silently
    /// changing the geometry of existing projects.
    #[test]
    fn body_mirror_without_cut_bit_is_still_the_union() {
        for sign in [0u8, 0b010, 0b111] {
            let (graph, body) = make_box_body_mirror(
                Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5), 0b010, sign, Vec3::ZERO);
            let eval = SceneEvaluator::new(&graph);
            assert!(eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0)) < 0.0,
                "sign={sign:#b}: union must keep the original half (no cut bit set)");
            assert!(eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0)) < 0.0,
                "sign={sign:#b}: union must add the reflected half");
        }
    }

    /// The cut is exactly `eval_scene(fold(p))` — pins the flat helper, the scope evaluator and
    /// `fold_for_cut` to ONE definition (the GPU shaders replicate this same expression).
    #[test]
    fn body_mirror_cut_equals_eval_at_the_folded_point() {
        use crate::body_field_ops::MIRROR_CUT_BIT;
        let prims = vec![
            spherep("A", Vec3::new(0.3, 0.9, 0.0), 0.6, BooleanOp::Union { k: 0.0 }),
            spherep("B", Vec3::new(-0.4, 1.2, 0.2), 0.4, BooleanOp::Subtract { k: 0.05 }),
        ];
        let visible: Vec<&SdfPrimitive> = prims.iter().collect();
        let (flags, sign, c) = (0b011u8, MIRROR_CUT_BIT | 0b001, Vec3::new(0.1, 0.0, 0.0));
        let field = BodyField::from_prims(&prims, flags, sign, c);
        for i in 0..64 {
            let p = Vec3::new((i % 4) as f32, ((i / 4) % 4) as f32, (i / 16) as f32) * 0.6 - Vec3::splat(0.9);
            let folded = eval_scene(fold_for_cut(p, flags, sign, c), &visible);
            let flat = eval_scene_body_mirrored(p, &visible, flags, sign, c);
            let scoped = field.eval_or(p, 100.0);
            assert!(flat == folded && scoped == folded,
                "cut must be bit-identical to eval@fold: flat={flat} scoped={scoped} folded={folded} at {p:?}");
        }
    }

    // ====================================================================
    // NESTED SUB-BODY BOOLEAN TREES (COMPLETE_BUG_TRACKER "boolean tree
    // inside a sub-body not working", defect (1)): the CPU evaluator must
    // match the GPU scope stack machine — a nested child SdfBody evaluates
    // its OWN boolean chain first, then combines via its `body_op`.
    // ====================================================================

    fn body_payload(name: &str, op: BooleanOp, origin: Vec3, prims: Vec<SdfPrimitive>) -> NodePayload {
        NodePayload::SdfBody {
            name: name.into(), primitives: prims, mesh_cache_id: None,
            body_op: op, origin,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),}
    }

    fn boxp(name: &str, pos: Vec3, he: Vec3, op: BooleanOp) -> SdfPrimitive {
        use crate::sdf_shapes::BoxBevel;
        let mut p = SdfPrimitive::new(name.into(),
            SdfShape::Box { half_extents: he, bevel: BoxBevel::default() }, pos);
        p.operation = op;
        p
    }

    fn spherep(name: &str, pos: Vec3, r: f32, op: BooleanOp) -> SdfPrimitive {
        let mut p = SdfPrimitive::new(name.into(), SdfShape::Sphere { radius: r }, pos);
        p.operation = op;
        p
    }

    /// THE PINNED DISCRIMINATOR (tracker entry, commit 1046378's GPU-oracle scene):
    /// top T { cube P Union } + nested sub-body S (body_op = Subtract{k:0}) { cube A Union,
    /// cube B Subtract{k:0} }, with a NON-ZERO sub-body origin (app-realistic grouped
    /// centroid). Probe in P∩A\B: true field +0.300 (carved) — the nesting-blind evaluator
    /// returned −0.500 (S union'd in, its internal Subtract ignored).
    #[test]
    fn nested_subbody_boolean_tree_discriminator() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let top = sg.add_child(root, body_payload("Top", BooleanOp::Union { k: 0.0 }, Vec3::ZERO,
            vec![boxp("P", Vec3::ZERO, Vec3::splat(1.0), BooleanOp::Union { k: 0.0 })]));
        let sub_origin = Vec3::new(0.8, 0.25, 0.0);
        let _sub = sg.add_child(top, body_payload("Sub", BooleanOp::Subtract { k: 0.0 }, sub_origin, vec![
            boxp("A", Vec3::new(0.8, 0.0, 0.0) - sub_origin, Vec3::splat(0.6), BooleanOp::Union { k: 0.0 }),
            boxp("B", Vec3::new(0.8, 0.5, 0.0) - sub_origin, Vec3::splat(0.4), BooleanOp::Subtract { k: 0.0 }),
        ]));
        let eval = SceneEvaluator::new(&sg);

        // THE pinned probe: in P∩A, outside B → carved, exactly +0.300.
        let d = eval.eval(Vec3::new(0.5, -0.3, 0.0));
        assert!((d - 0.3).abs() < 1e-3,
            "P∩A\\B probe must read +0.300 (carved by the Subtract sub-body), got {d:+.4} \
             (nesting-blind evaluator returned −0.500)");

        // The other three hand-computed probes (signs + values):
        // a: in P, outside S → P's own field −0.5.
        let d = eval.eval(Vec3::new(-0.5, 0.0, 0.0));
        assert!((d + 0.5).abs() < 1e-3, "probe a: expected −0.5, got {d:+.4}");
        // c: in P∩A∩B — B un-carves inside S → S empty there → P survives at −0.2.
        let d = eval.eval(Vec3::new(0.8, 0.5, 0.0));
        assert!((d + 0.2).abs() < 1e-3, "probe c: expected −0.2 (B un-carves), got {d:+.4}");
        // d: in A\B but OUTSIDE P → empty +0.1 (sub-body alone adds no scene geometry).
        let d = eval.eval(Vec3::new(1.1, 0.5, 0.0));
        assert!((d - 0.1).abs() < 1e-3, "probe d: expected +0.1 (outside P), got {d:+.4}");

        // eval_body on the TOP body gives the same subtree field.
        let d = eval.eval_body(top, Vec3::new(0.5, -0.3, 0.0));
        assert!((d - 0.3).abs() < 1e-3, "eval_body(top) must match: got {d:+.4}");
    }

    /// TWO-LEVEL nesting (sub-sub-body), spheres for exact hand-computed values, with
    /// non-zero origins at BOTH nesting levels (translation-only composition, matching
    /// the serializer / collect_body_prims_recursive):
    /// T { P: sphere r1 @0 } − S { A: sphere r0.6 @(0.8,0,0) − SS { C: sphere r0.3 @(0.8,0,0) } }.
    #[test]
    fn nested_two_level_subtract_chain() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let top = sg.add_child(root, body_payload("Top", BooleanOp::Union { k: 0.0 }, Vec3::ZERO,
            vec![spherep("P", Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 })]));
        let s_origin = Vec3::new(2.0, 0.0, 0.0);
        let sub = sg.add_child(top, body_payload("S", BooleanOp::Subtract { k: 0.0 }, s_origin,
            vec![spherep("A", Vec3::new(0.8, 0.0, 0.0) - s_origin, 0.6, BooleanOp::Union { k: 0.0 })]));
        let ss_origin = Vec3::new(1.0, 1.0, 0.0);
        let _subsub = sg.add_child(sub, body_payload("SS", BooleanOp::Subtract { k: 0.0 }, ss_origin,
            vec![spherep("C", Vec3::new(0.8, 0.0, 0.0) - ss_origin, 0.3, BooleanOp::Union { k: 0.0 })]));
        let eval = SceneEvaluator::new(&sg);

        // At (0.8,0,0): C un-carves inside S (S = max(−0.6, +0.3) = +0.3 → hole in the
        // carve) → T = max(P −0.2, −S) = −0.2 SOLID.
        let d = eval.eval(Vec3::new(0.8, 0.0, 0.0));
        assert!((d + 0.2).abs() < 1e-4, "sub-sub un-carve center: expected −0.2, got {d:+.5}");

        // At (0.8,0.45,0): outside C (+0.15), inside A (−0.15) → S = −0.15 solid → T =
        // max(P, +0.15) = +0.15 CARVED.
        let d = eval.eval(Vec3::new(0.8, 0.45, 0.0));
        assert!((d - 0.15).abs() < 1e-4, "carved ring: expected +0.15, got {d:+.5}");

        // Far side of P untouched: (−0.5,0,0) → −0.5.
        let d = eval.eval(Vec3::new(-0.5, 0.0, 0.0));
        assert!((d + 0.5).abs() < 1e-4, "untouched half: expected −0.5, got {d:+.5}");
    }

    /// FLAT BIT-IDENTITY: for a body with NO nested children the scope-aware path is the
    /// SAME code as the historical flat eval — assert exact (==) equality against

    /// GATE (pillar review 2026-07-30) — `SceneEvaluator` memoizes `BodyField` per body, and the memo
    /// is BIT-IDENTICAL to rebuilding it every sample.
    ///
    /// `eval_body_internal` used to call `BodyField::from_graph` on EVERY sample and discard it.
    /// `from_graph` deep-clones every visible prim in the subtree (each `SdfPrimitive` owns a name
    /// `String`, a `PbrMaterial` with its own `String`, and several `Vec`s) and recurses into nested
    /// sub-bodies. The SDF→voxel stamp calls this ONCE PER VOXEL, on the frame thread.
    ///
    /// This gate pins the property that makes caching sound: the graph is borrowed immutably for the
    /// evaluator's lifetime, so every rebuild produced the same field. If a future change lets the
    /// graph mutate mid-evaluation, this assert is what catches it — not a comment.
    ///
    /// Deliberately asserts CORRECTNESS, not a speed ratio: a timing threshold would be flaky on a
    /// loaded machine (this project already has one such flaky bench). The speedup was measured
    /// separately at 12-16x over 1/5/10/20-prim bodies.
    #[test]
    fn evaluator_memo_is_bit_identical_to_rebuilding() {
        let prims: Vec<SdfPrimitive> = vec![
            spherep("A", Vec3::new(0.2, 0.0, 0.0), 0.8, BooleanOp::Union { k: 0.0 }),
            boxp("B", Vec3::new(0.5, 0.3, 0.0), Vec3::splat(0.4), BooleanOp::Subtract { k: 0.15 }),
            spherep("C", Vec3::new(-0.4, 0.2, 0.1), 0.5, BooleanOp::Union { k: 0.25 }),
        ];
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let body = sg.add_child(
            root,
            body_payload("Memo", BooleanOp::Union { k: 0.0 }, Vec3::new(1.0, -2.0, 0.5), prims),
        );

        let eval = SceneEvaluator::new(&sg);
        // Sweep a dense grid so repeated hits on the SAME body exercise the cache path.
        for iz in 0..9 {
            for iy in 0..9 {
                for ix in 0..9 {
                    let p = Vec3::new(ix as f32, iy as f32, iz as f32) / 4.0 - Vec3::ONE;
                    let memo = eval.eval_body(body, p);
                    let local = sg.effective_world_to_local(body, p);
                    let raw = BodyField::from_graph(&sg, body).eval_or(local, f32::MAX);
                    assert_eq!(
                        memo, raw,
                        "memoized eval must be bit-identical to a fresh rebuild at {p:?}"
                    );
                }
            }
        }
    }

    /// `eval_scene_body_mirrored` over a dense probe grid, mixed ops + body mirror.
    #[test]
    fn flat_body_bit_identical_to_legacy_eval() {
        let prims = vec![
            spherep("A", Vec3::new(0.2, 0.0, 0.0), 0.8, BooleanOp::Union { k: 0.0 }),
            boxp("B", Vec3::new(0.5, 0.3, 0.0), Vec3::splat(0.4), BooleanOp::Subtract { k: 0.15 }),
            spherep("C", Vec3::new(-0.4, 0.2, 0.1), 0.5, BooleanOp::Union { k: 0.25 }),
        ];
        for &(mflags, mc) in &[(0u8, Vec3::ZERO), (0b010u8, Vec3::new(0.0, 0.1, 0.0))] {
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let mut payload = body_payload("Flat", BooleanOp::Union { k: 0.0 }, Vec3::new(1.0, -2.0, 0.5), prims.clone());
            if let NodePayload::SdfBody { mirror_flags, mirror_center, .. } = &mut payload {
                *mirror_flags = mflags; *mirror_center = mc;
            }
            let body = sg.add_child(root, payload);
            let visible: Vec<&SdfPrimitive> = prims.iter().filter(|p| p.visible).collect();

            // Compare in BODY-LOCAL space (the shared frame): the world→local entry
            // transform (effective_world_to_local) is unchanged by this fix and is
            // matrix-based (float-lossy vs exact subtraction), so bit-identity is asserted
            // on the EVALUATOR itself.
            let field = BodyField::from_graph(&sg, body);
            for iz in 0..7 { for iy in 0..7 { for ix in 0..7 {
                let local = Vec3::new(ix as f32, iy as f32, iz as f32) / 3.0 - Vec3::ONE;
                let new_d = field.eval_or(local, f32::MAX);
                let old_d = eval_scene_body_mirrored(local, &visible, mflags, 0, mc);
                assert!(new_d == old_d,
                    "flat body must be BIT-IDENTICAL: new={new_d} old={old_d} at {local:?} mflags={mflags}");
            }}}
        }
    }

    /// Scene-level eval unions only TOP-LEVEL bodies: a Subtract sub-body must not be
    /// re-added as an independent solid operand.
    #[test]
    fn scene_eval_skips_nested_bodies_as_operands() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let top = sg.add_child(root, body_payload("Top", BooleanOp::Union { k: 0.0 }, Vec3::ZERO,
            vec![spherep("P", Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 })]));
        let _sub = sg.add_child(top, body_payload("S", BooleanOp::Subtract { k: 0.0 }, Vec3::ZERO,
            vec![spherep("A", Vec3::new(0.8, 0.0, 0.0), 0.5, BooleanOp::Union { k: 0.0 })]));
        let eval = SceneEvaluator::new(&sg);
        // (0.8,0,0) is inside A: the sub-body carves here → EMPTY. The old evaluator
        // union'd the sub-body's own field (−0.5) into the scene → falsely solid.
        let d = eval.eval(Vec3::new(0.8, 0.0, 0.0));
        assert!(d > 0.0, "Subtract sub-body must carve, not union: got {d:+.4}");
        // closest_surface must also not attribute geometry to the nested body.
        let hit = eval.closest_surface(Vec3::new(0.8, 0.0, 0.0));
        assert_eq!(hit.body_nid, Some(top), "closest body must be the top-level subtree");
    }

    /// BodyField::from_prims routes flat slices through the same scope evaluator —
    /// exact equality with eval_scene_body_mirrored (the classifier/bake oracle contract).
    #[test]
    fn body_field_from_prims_matches_flat_eval() {
        let prims = vec![
            spherep("A", Vec3::new(0.1, -0.2, 0.0), 0.7, BooleanOp::Union { k: 0.0 }),
            spherep("B", Vec3::new(0.5, 0.0, 0.0), 0.4, BooleanOp::Subtract { k: 0.1 }),
        ];
        let visible: Vec<&SdfPrimitive> = prims.iter().collect();
        let field = BodyField::from_prims(&prims, 0b001, 0, Vec3::new(0.05, 0.0, 0.0));
        for i in 0..64 {
            let p = Vec3::new((i % 4) as f32, ((i / 4) % 4) as f32, (i / 16) as f32) * 0.5 - Vec3::splat(0.75);
            let a = field.eval_or(p, 100.0);
            let b = eval_scene_body_mirrored(p, &visible, 0b001, 0, Vec3::new(0.05, 0.0, 0.0));
            assert!(a == b, "from_prims must be bit-identical to flat eval: {a} vs {b} at {p:?}");
        }
    }

    /// sphere_proofs_valid: plain Union{0} nested scopes keep the classifier proofs; any
    /// non-Union / blended / mirrored / beveled nested scope disables them.
    #[test]
    fn sphere_proofs_predicate() {
        let mk = |op: BooleanOp| {
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let top = sg.add_child(root, body_payload("T", BooleanOp::Union { k: 0.0 }, Vec3::ZERO,
                vec![spherep("P", Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 })]));
            let _s = sg.add_child(top, body_payload("S", op, Vec3::ZERO,
                vec![spherep("A", Vec3::new(0.5, 0.0, 0.0), 0.3, BooleanOp::Union { k: 0.0 })]));
            BodyField::from_graph(&sg, top)
        };
        assert!(mk(BooleanOp::Union { k: 0.0 }).sphere_proofs_valid(), "plain union group keeps proofs");
        assert!(!mk(BooleanOp::Subtract { k: 0.0 }).sphere_proofs_valid(), "subtract scope disables proofs");
        assert!(!mk(BooleanOp::Union { k: 0.5 }).sphere_proofs_valid(), "scope blend k disables proofs");
        assert!(!mk(BooleanOp::Intersect).sphere_proofs_valid(), "intersect scope disables proofs");
    }

    // ====================================================================
    // SUB-BODY EVAL-ORDER SEMANTICS (COMPLETE_BUG_TRACKER root (B) of the
    // "visual notch" bug, diagnosed 2026-07-17). AZRA evaluates a body as a
    // LEFT FOLD in child_order; a sub-body is its OWN fold combined via its
    // `body_op` — i.e. GROUPING IS PARENTHESIZATION. These tests pin exactly
    // WHICH operations are field-invariant (must never drift — a regression
    // here is a real defect) and WHICH legitimately change the field
    // (characterized with exact values so any future change is deliberate).
    // ====================================================================

    /// Max |delta| + surface sign flips between two BodyFields over a dense local grid.
    fn field_diff(fa: &BodyField, fb: &BodyField, lo: Vec3, hi: Vec3, n: u32) -> (f32, usize) {
        let s = (n - 1) as f32;
        let (mut max_d, mut flips) = (0.0f32, 0usize);
        for iz in 0..n { for iy in 0..n { for ix in 0..n {
            let p = lo + Vec3::new(ix as f32, iy as f32, iz as f32) / s * (hi - lo);
            let a = fa.eval_or(p, 100.0);
            let b = fb.eval_or(p, 100.0);
            let d = (a - b).abs();
            if d > max_d { max_d = d; }
            if (a < 0.0) != (b < 0.0) { flips += 1; }
        }}}
        (max_d, flips)
    }

    fn flat_body(prims: Vec<SdfPrimitive>) -> BodyField {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let body = sg.add_child(root, body_payload("T", BooleanOp::Union { k: 0.0 }, Vec3::ZERO, prims));
        BodyField::from_graph(&sg, body)
    }

    /// Top body with `top_prims`, plus one sub-body (op, origin) holding `sub_prims`
    /// given in TOP-frame positions (shifted to sub-local exactly like the app's group).
    fn grouped_body(top_prims: Vec<SdfPrimitive>, sub_op: BooleanOp, sub_origin: Vec3,
                    sub_prims: Vec<SdfPrimitive>) -> BodyField {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let top = sg.add_child(root, body_payload("T", BooleanOp::Union { k: 0.0 }, Vec3::ZERO, top_prims));
        let local: Vec<SdfPrimitive> = sub_prims.into_iter().map(|mut p| { p.position -= sub_origin; p }).collect();
        let _sub = sg.add_child(top, body_payload("S", sub_op, sub_origin, local));
        BodyField::from_graph(&sg, top)
    }

    fn abc(k: f32) -> [SdfPrimitive; 3] {
        [
            spherep("A", Vec3::new(-0.4, 0.0, 0.0), 0.8, BooleanOp::Union { k }),
            spherep("B", Vec3::new(0.4, 0.1, 0.0), 0.7, BooleanOp::Union { k }),
            spherep("C", Vec3::new(0.0, 0.5, 0.2), 0.6, BooleanOp::Union { k }),
        ]
    }

    const GRID_LO: Vec3 = Vec3::new(-1.5, -1.5, -1.5);
    const GRID_HI: Vec3 = Vec3::new(1.5, 1.5, 1.5);

    /// INVARIANT (defect if it ever breaks): reordering prims within a HARD-union run
    /// (k = 0) is field-invariant — min is exactly associative + commutative, and the
    /// bounding-sphere cull returns the identical value. Bit-exact over the grid.
    #[test]
    fn reorder_hard_union_run_is_field_invariant() {
        let [a, b, c] = abc(0.0);
        let f1 = flat_body(vec![a.clone(), b.clone(), c.clone()]);
        let f2 = flat_body(vec![c, a, b]);
        let (max_d, flips) = field_diff(&f1, &f2, GRID_LO, GRID_HI, 17);
        assert!(max_d == 0.0 && flips == 0,
            "hard-union reorder must be exactly field-invariant: max|d|={max_d} flips={flips}");
    }

    /// INVARIANT: grouping a HARD-union run into a Union{{k:0}} sub-body (with a non-zero
    /// group origin, like the app's recalculate_body_origin) is field-invariant up to the
    /// float error of the position shift (sub_origin round-trip) — no visible notch.
    #[test]
    fn group_hard_union_run_is_field_invariant() {
        let [a, b, c] = abc(0.0);
        let flat = flat_body(vec![a.clone(), b.clone(), c.clone()]);
        let grouped = grouped_body(vec![a], BooleanOp::Union { k: 0.0 },
            Vec3::new(0.2, 0.3, 0.1), vec![b, c]);
        let (max_d, flips) = field_diff(&flat, &grouped, GRID_LO, GRID_HI, 17);
        eprintln!("[EvalOrder] group hard-union: max|d|={max_d:e} flips={flips}");
        assert!(max_d < 1e-6 && flips == 0,
            "grouping a hard-union run must not change the field: max|d|={max_d} flips={flips}");
    }

    /// INVARIANT: flattening a nested Union{{k:0}} chain (sub-sub-body) is field-invariant.
    #[test]
    fn nested_hard_union_chain_matches_flat() {
        let [a, b, c] = abc(0.0);
        let flat = flat_body(vec![a.clone(), b.clone(), c.clone()]);
        // T { A } ∪ S { B } ∪ SS-in-S { C } — build two-level nesting by hand.
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let top = sg.add_child(root, body_payload("T", BooleanOp::Union { k: 0.0 }, Vec3::ZERO, vec![a]));
        let s_origin = Vec3::new(0.3, 0.0, 0.0);
        let mut bl = b; bl.position -= s_origin;
        let sub = sg.add_child(top, body_payload("S", BooleanOp::Union { k: 0.0 }, s_origin, vec![bl]));
        let ss_origin = Vec3::new(0.1, 0.4, 0.0);
        let mut cl = c; cl.position -= ss_origin;
        let _ss = sg.add_child(sub, body_payload("SS", BooleanOp::Union { k: 0.0 }, ss_origin, vec![cl]));
        let nested = BodyField::from_graph(&sg, top);
        let (max_d, flips) = field_diff(&flat, &nested, GRID_LO, GRID_HI, 17);
        eprintln!("[EvalOrder] nested hard-union chain: max|d|={max_d:e} flips={flips}");
        assert!(max_d < 1e-6 && flips == 0,
            "nested hard-union chain must equal the flat fold: max|d|={max_d} flips={flips}");
    }

    /// CHARACTERIZED (intentional math, NOT a defect): reordering prims within a run of
    /// SMOOTH unions (equal k > 0) changes the field, because pairwise smin is
    /// NON-ASSOCIATIVE: smin(smin(a,b),c) ≠ smin(a,smin(b,c)). Every SDF DCC that folds
    /// pairwise (this one, and the GPU paths byte-identically) has this property. The
    /// delta is bounded by k. If this test starts reporting ZERO delta, the fold was
    /// changed to an order-canonical form — that ALTERS every existing scene's look and
    /// must be an owner-approved semantic decision, not a silent fix.
    #[test]
    fn reorder_smooth_union_run_changes_field_bounded_by_k() {
        let k = 0.25;
        let [a, b, c] = abc(k);
        let f1 = flat_body(vec![a.clone(), b.clone(), c.clone()]);
        let f2 = flat_body(vec![c, b, a]);
        let (max_d, flips) = field_diff(&f1, &f2, GRID_LO, GRID_HI, 17);
        eprintln!("[EvalOrder] smooth-union reorder (k={k}): max|d|={max_d:.5} flips={flips}");
        assert!(max_d > 1e-4,
            "pairwise smin is non-associative — a zero delta means the fold semantics changed \
             (order-canonical fold?). That is a scene-look-altering decision: escalate, don't ship.");
        assert!(max_d < k, "smooth-union reorder delta must stay bounded by k: {max_d}");
    }

    /// CHARACTERIZED (intentional parenthesization): grouping [A Union, B Subtract] into a
    /// Union{{k:0}} sub-body SCOPES the Subtract to the group. Flat: B carves BOTH P and A
    /// (left fold max(min(P,A),−B)). Grouped: B carves only A (min(P, max(A,−B))) — P's
    /// surface inside B reappears. THE owner-visible "notch": same prims, different shape,
    /// because grouping changed the parse tree. Pinned with exact hand-computed values.
    #[test]
    fn group_scopes_subtract_to_group_parenthesization() {
        let p = spherep("P", Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 });
        let a = spherep("A", Vec3::new(1.2, 0.0, 0.0), 0.5, BooleanOp::Union { k: 0.0 });
        let b = spherep("B", Vec3::new(0.0, 0.9, 0.0), 0.4, BooleanOp::Subtract { k: 0.0 });
        let flat = flat_body(vec![p.clone(), a.clone(), b.clone()]);
        let grouped = grouped_body(vec![p], BooleanOp::Union { k: 0.0 },
            Vec3::new(0.6, 0.45, 0.0), vec![a, b]);
        // Probe inside P ∩ B, far from A: (0, 0.9, 0) = B's center, on P's surface region.
        let probe = Vec3::new(0.0, 0.8, 0.0);
        let d_flat = flat.eval_or(probe, 100.0);
        let d_grp = grouped.eval_or(probe, 100.0);
        eprintln!("[EvalOrder] subtract scoping probe: flat={d_flat:+.4} grouped={d_grp:+.4}");
        // Flat: B carves P here → exterior, d = 0.4 − |p−B| = 0.4−0.1 → +0.3.
        assert!((d_flat - 0.3).abs() < 1e-4, "flat: B must carve P (expected +0.300), got {d_flat:+.4}");
        // Grouped: B only carves A; P intact → interior, d = |p|−1 = −0.2.
        assert!((d_grp + 0.2).abs() < 1e-4, "grouped: B must NOT carve P (expected −0.200), got {d_grp:+.4}");
    }

    /// CHARACTERIZED (intentional, and the most likely owner-visible symptom): grouping two
    /// SMOOTHLY-blended prims into a default Union{{k:0}} sub-body replaces the smooth blend
    /// BETWEEN group and outside geometry with a HARD min — the fillet between A (outside)
    /// and B (inside the group) becomes a crease/notch. The blend between B and C (both
    /// inside) survives. Group k is the sub-body's `body_op` k, not the prims'.
    #[test]
    fn group_hardens_blend_across_group_boundary() {
        let k = 0.25;
        let [a, b, c] = abc(k);
        let flat = flat_body(vec![a.clone(), b.clone(), c.clone()]);
        let grouped = grouped_body(vec![a], BooleanOp::Union { k: 0.0 },
            Vec3::new(0.2, 0.3, 0.1), vec![b, c]);
        let (max_d, flips) = field_diff(&flat, &grouped, GRID_LO, GRID_HI, 17);
        eprintln!("[EvalOrder] group smooth-union under hard group op: max|d|={max_d:.5} flips={flips}");
        assert!(max_d > 1e-3,
            "grouping smooth-blended prims under a hard Union body_op must change the field \
             (blend across the group boundary hardens) — zero delta means semantics changed");
        // Setting the sub-body's op to Union{k} restores a blend at the boundary (not
        // bit-identical to flat — parenthesization — but far closer than the hard group).
        let grouped_k = grouped_body(vec![abc(k)[0].clone()], BooleanOp::Union { k },
            Vec3::new(0.2, 0.3, 0.1), vec![abc(k)[1].clone(), abc(k)[2].clone()]);
        let (max_dk, _) = field_diff(&flat, &grouped_k, GRID_LO, GRID_HI, 17);
        eprintln!("[EvalOrder] group smooth-union with group k={k}: max|d|={max_dk:.5}");
        assert!(max_dk < max_d, "group op k={k} must reduce the boundary hardening delta");
    }

    /// CHARACTERIZED (intentional first-operand rule, all three paths): the FIRST operand
    /// in a scope seeds the accumulator regardless of its own op. Reordering a Subtract
    /// prim into slot 0 turns the carver into the seed solid — a large, expected change.
    #[test]
    fn reorder_subtract_to_first_slot_seeds_solid() {
        let p = spherep("P", Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 });
        let b = spherep("B", Vec3::new(0.0, 0.9, 0.0), 0.4, BooleanOp::Subtract { k: 0.0 });
        let normal = flat_body(vec![p.clone(), b.clone()]);
        let swapped = flat_body(vec![b, p]);
        // At B's center: normal order → carved (+0.4); swapped → B seeds solid then P unions → −0.4.
        let probe = Vec3::new(0.0, 0.9, 0.0);
        let d_n = normal.eval_or(probe, 100.0);
        let d_s = swapped.eval_or(probe, 100.0);
        eprintln!("[EvalOrder] subtract-first probe: normal={d_n:+.4} swapped={d_s:+.4}");
        assert!((d_n - 0.4).abs() < 1e-4, "normal order: carved, expected +0.400, got {d_n:+.4}");
        assert!((d_s + 0.4).abs() < 1e-4, "subtract-first seeds solid, expected −0.400, got {d_s:+.4}");
    }

    /// SANITY: `mirror_flags == 0` is byte-identical to the un-mirrored eval —
    /// the common (non-mirrored) body path is unchanged.
    #[test]
    fn body_mirror_none_matches_unmirrored() {
        let (graph, body) = make_box_body_mirror(Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5), 0, 0, Vec3::ZERO);
        let eval = SceneEvaluator::new(&graph);

        // Original box solid, mirror plane region empty (no mirror applied).
        assert!(eval.eval_body(body, Vec3::new(0.0, -1.0, 0.0)) < 0.0, "box solid at its location");
        assert!(eval.eval_body(body, Vec3::new(0.0, 1.0, 0.0)) > 0.0, "no mirror -> +Y is empty");
    }
}

