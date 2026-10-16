//! Pure utility functions for SDF volume baking — grid evaluation, bounds
//! computation, prim collection, and cache invalidation hashing.
//!
//! These are standalone functions with no shared mutable state. Extracted from
//! body_volume_cache.rs for file size management.

use glam::Vec3;
use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp};
use core_math::scene_graph::{SceneGraph, NodeId, NodePayload};

use crate::BODY_LOCAL_PRIMS;


// ============================================================
// Grid AABB padding
// ============================================================

/// Initial tuning values for drag-time stability. Padding on cached grid AABB
/// prevents tiny prim movements from escaping the cached bounds every frame,
/// which would discard dirty regions and force full 128³ rebakes.
/// 15% gives ~10 frames of headroom at 60fps drag speed. 0.5 minimum handles
/// tiny bodies where percentage padding would be meaningless.
pub(crate) const GRID_PAD_FRAC: f32 = 0.15;
pub(crate) const GRID_PAD_MIN: f32 = 0.5;

/// Apply padding to tight AABB bounds for cached grid storage.
/// Returns (padded_min, padded_max). Does NOT modify the input bounds.
pub(crate) fn pad_grid_bounds(tight_min: Vec3, tight_max: Vec3) -> (Vec3, Vec3) {
    let extent = tight_max - tight_min;
    let pad = (extent * GRID_PAD_FRAC).max(Vec3::splat(GRID_PAD_MIN));
    (tight_min - pad, tight_max + pad)
}

// ============================================================
// Body prim collection (recursive through nested bodies)
// ============================================================

/// Collect ALL SdfBody NodeIds from the entire scene graph, regardless of parent type.
pub(crate) fn collect_all_body_nids(sg: &SceneGraph) -> Vec<NodeId> {
    let mut bodies = Vec::new();
    fn walk(sg: &SceneGraph, nid: NodeId, bodies: &mut Vec<NodeId>) {
        if matches!(sg.get(nid).map(|n| &n.payload), Some(NodePayload::SdfBody { .. })) {
            bodies.push(nid);
        }
        for &child in sg.children(nid) {
            walk(sg, child, bodies);
        }
    }
    walk(sg, sg.root(), &mut bodies);
    bodies
}

/// Collect all primitives from a body subtree (body + nested child bodies), every prim expressed in
/// the TOP body's local bake frame.
///
/// Prim positions are body-local — relative to the origin of the body they belong to. A nested
/// sub-body owns its OWN origin (groups get a recalculated centroid origin), so its prims must be
/// shifted by `(sub_origin - top_origin)` to share the one frame the top body's own prims and the
/// grid AABB use. WITHOUT this shift, grouped/nested geometry is collected at the wrong place → the
/// bake grid bounds miss it → the Object-Mode mesh clips or vanishes (the "surface disappears after
/// I group / drag a prim into another" bug). Every prim in the subtree is now treated uniformly — no
/// special base, no per-group exception. (Translation-only: sub-body rotation/scale composition is a
/// separate follow-up; groups are translation-only today.)
///
/// ORDER: an SdfBody's prims are emitted in `resolve_child_order` (the boolean chain), not
/// primitives-Vec order. A same-body reorder does not move the Vec; collecting the Vec baked
/// the pre-reorder field into every consumer of this slice (bounds, CPU flat bake, template hash).
pub(crate) fn collect_body_prims_recursive(sg: &SceneGraph, body_nid: NodeId) -> Vec<SdfPrimitive> {
    let top_origin = match sg.get(body_nid).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { origin, .. }) => *origin,
        _ => Vec3::ZERO,
    };
    let mut prims = Vec::new();
    collect_body_prims_inner(sg, body_nid, top_origin, &mut prims);
    prims
}

fn collect_body_prims_inner(sg: &SceneGraph, body_nid: NodeId, top_origin: Vec3, out: &mut Vec<SdfPrimitive>) {
    // An SdfBody emits in `resolve_child_order` — the SAME sequence `BodyField::from_graph`
    // and the GPU bake serializer fold. Reorder permutes only `child_order`; walking the
    // primitives Vec then `children` kept the pre-reorder chain, so CPU bounds / flat bake /
    // template identity diverged from the field (REORDER: clipped or torn solid).
    // Group / Empty are not bodies: they own no prims, and the walk below descends into them.
    let body_origin = match sg.get(body_nid).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { origin, .. }) => Some(*origin),
        _ => None,
    };
    if let Some(origin) = body_origin {
        let shift = if BODY_LOCAL_PRIMS { origin - top_origin } else { Vec3::ZERO };
        let resolved = sg.resolve_child_order(body_nid);
        let prims = sg.body_primitives(body_nid).unwrap_or(&[]);
        let mut consumed = vec![false; prims.len()];
        let mut seen_child = std::collections::HashSet::new();
        // Owned steps so the `prims` borrow ends before we recurse into children.
        enum Step {
            Prim(SdfPrimitive),
            Child(NodeId),
        }
        let mut steps: Vec<Step> = Vec::with_capacity(resolved.len());
        for entry in resolved {
            match entry {
                Ok(i) => {
                    if let Some(slot) = consumed.get_mut(i) {
                        *slot = true;
                    }
                    if let Some(p) = prims.get(i) {
                        if p.visible {
                            let mut sp = p.clone();
                            sp.position += shift;
                            steps.push(Step::Prim(sp));
                        }
                    }
                }
                Err(nid) => {
                    seen_child.insert(nid);
                    steps.push(Step::Child(nid));
                }
            }
        }
        // A prim resolve didn't emit (stale order mid-mutation) is appended, never dropped.
        for (i, p) in prims.iter().enumerate() {
            if consumed.get(i).copied().unwrap_or(true) || !p.visible {
                continue;
            }
            let mut sp = p.clone();
            sp.position += shift;
            steps.push(Step::Prim(sp));
        }
        // Empty (and any container `direct_order_child_nodes` does not list) is not a
        // child_order slot. Splice it where the children Vec places it relative to the
        // ordered siblings, so a default (prims-then-bodies) order stays the legacy
        // sequence and a nested body under Empty cannot vanish from the set.
        let child_ids = sg.children(body_nid).to_vec();
        let mut extras: Vec<(usize, NodeId)> = Vec::new();
        for (idx, &cid) in child_ids.iter().enumerate() {
            if seen_child.contains(&cid) || !collect_descends(sg, cid) {
                continue;
            }
            extras.push((idx, cid));
        }
        for (idx, cid) in extras {
            let at = steps.iter().position(|s| match s {
                Step::Child(nid) => {
                    child_ids.iter().position(|c| c == nid).unwrap_or(usize::MAX) > idx
                }
                Step::Prim(_) => false,
            }).unwrap_or(steps.len());
            steps.insert(at, Step::Child(cid));
        }
        for step in steps {
            match step {
                Step::Prim(p) => out.push(p),
                Step::Child(nid) => {
                    if collect_descends(sg, nid) {
                        collect_body_prims_inner(sg, nid, top_origin, out);
                    }
                }
            }
        }
        return;
    }
    for &child in sg.children(body_nid) {
        if collect_descends(sg, child) {
            collect_body_prims_inner(sg, child, top_origin, out);
        }
    }
}

/// Containers the recursive collect descends into. SdfObject is NOT one of them — the
/// pre-child_order collect skipped it (the GPU serializer expands it at serialize time).
/// Keeping the same set means a reorder fix cannot shrink or grow which prims are collected.
fn collect_descends(sg: &SceneGraph, nid: NodeId) -> bool {
    matches!(
        sg.get(nid).map(|n| &n.payload),
        Some(NodePayload::SdfBody { .. })
            | Some(NodePayload::Group { .. })
            | Some(NodePayload::Empty { .. })
    )
}

/// Offset prim positions to body-local space by subtracting body.origin.
/// Returns cloned prims with adjusted positions so the volume bake evaluates
/// the SDF in body-local coordinates.
pub(crate) fn offset_prims_to_local(prims: &[SdfPrimitive], body_origin: Vec3) -> Vec<SdfPrimitive> {
    if body_origin.length_squared() < 1e-12 {
        return prims.to_vec(); // origin ≈ zero, no offset needed
    }
    prims.iter().map(|p| {
        let mut local = p.clone();
        local.position -= body_origin;
        local
    }).collect()
}

// ============================================================
// Per-body SDF grid bake (runs on rayon thread pool)
// ============================================================

/// Evaluate a body's primitives into a flat f32 grid using per-body bounds.
/// Layout: index = x + y*res + z*res*res
/// World sampling: endpoint-inclusive interpolation from grid_min to grid_max.
///
/// Applies the body-level mirror via the canonical `BodyField` — the SAME spec the GPU
/// bake / WGSL paths use, in whichever mode `mirror_sign` selects (union, or the CUT fold
/// of `MIRROR_CUT_BIT`). `mirror_flags == 0` routes to a single `eval_scene` (byte-identical
/// to the non-mirrored path). Omitting the mirror here baked only the original half into a
/// grid whose bounds were already expanded for both halves → flat slab + scattered
/// DC fragments the instant Body-Mirror is enabled (the CPU cascade/fallback bake
/// was the missing 5th eval path of commit 90522b5) — which is why `mirror_sign` is a real
/// parameter here and not a hardcoded 0: a CPU grid baked in union mode under a body the GPU
/// cuts is that same divergence with a new name.
pub(crate) fn bake_body_grid(
    primitives: &[SdfPrimitive],
    res: u32,
    grid_min: Vec3,
    grid_max: Vec3,
    mirror_flags: u8,
    mirror_sign: u8,
    mirror_center: Vec3,
) -> Vec<f32> {
    let field = core_math::field_eval::BodyField::from_prims(
        primitives,
        mirror_flags,
        mirror_sign,
        mirror_center,
    );
    bake_body_grid_field(&field, res, grid_min, grid_max)
}

/// SCOPE-AWARE full-grid CPU bake: evaluates a `BodyField` program (nested sub-body boolean
/// trees included — the CPU twin of the GPU scope stack machine). `bake_body_grid` (flat prim
/// slice, oracle for the GPU parity tests) routes through here via `BodyField::from_prims`,
/// so flat and nested share ONE grid loop and ONE eval implementation.
/// True when any prim in the body (recursive collect) needs full modifier-stack CPU bake
/// because the GPU volume bake only has one non-mirror modifier slot (P3 honesty).
pub(crate) fn body_needs_cpu_full_stack_bake(
    sg: &SceneGraph,
    body_nid: NodeId,
) -> bool {
    // Prim multi-mod stack (GPU has one non-mirror slot) OR body field Smooth
    // (multi-sample curvature flow — not in volume_bake.wgsl yet).
    if collect_body_prims_recursive(sg, body_nid)
        .iter()
        .any(|p| p.needs_cpu_full_stack_bake())
    {
        return true;
    }
    body_or_nested_has_field_stack_cpu(sg, body_nid)
}

/// Any Body Smooth / Displacement on this body or nested child scopes → force CPU bake.
fn body_or_nested_has_field_stack_cpu(sg: &SceneGraph, body_nid: NodeId) -> bool {
    use core_math::body_field_ops::body_field_stack_needs_cpu;
    let Some(node) = sg.get(body_nid) else {
        return false;
    };
    match &node.payload {
        NodePayload::SdfBody { field_modifiers, .. } => {
            if body_field_stack_needs_cpu(field_modifiers) {
                return true;
            }
        }
        _ => return false,
    }
    for slot in sg.resolve_child_order(body_nid) {
        if let Err(child_nid) = slot {
            let expand: Vec<NodeId> = match sg.get(child_nid).map(|n| &n.payload) {
                Some(NodePayload::SdfObject { .. }) => sg.child_bodies(child_nid),
                Some(NodePayload::SdfBody { .. }) => vec![child_nid],
                _ => Vec::new(),
            };
            for cid in expand {
                if body_or_nested_has_field_stack_cpu(sg, cid) {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn bake_body_grid_field(
    field: &core_math::field_eval::BodyField,
    res: u32,
    grid_min: Vec3,
    grid_max: Vec3,
) -> Vec<f32> {
    let n = (res as usize).pow(3);
    let mut grid = vec![100.0f32; n];
    let denom = if res > 1 { (res - 1) as f32 } else { 1.0 };
    let extent = grid_max - grid_min;
    for iz in 0..res {
        for iy in 0..res {
            for ix in 0..res {
                let t = Vec3::new(ix as f32 / denom, iy as f32 / denom, iz as f32 / denom);
                let p = grid_min + t * extent;
                grid[(ix + iy * res + iz * res * res) as usize] = field.eval_or(p, 100.0);
            }
        }
    }
    grid
}

/// Partial bake using voxel-space sub-region. Re-evaluates only voxels in
/// [bake_min, bake_max) and leaves the rest of the grid untouched.
/// Used by CPU fallback when a base_grid + dirty region are available.
/// Matches the GPU shader semantics (bake_min inclusive, bake_max exclusive).
///
/// Applies the body-level mirror in the SAME mode as the full-grid bake (union or CUT fold,
/// per `mirror_sign`) — otherwise a partial re-bake on a mirror body would punch a
/// non-mirrored hole into an otherwise-mirrored grid.
pub(crate) fn bake_body_grid_region(
    primitives: &[SdfPrimitive],
    res: u32,
    grid_min: Vec3,
    grid_max: Vec3,
    grid: Vec<f32>,
    bake_min: [u32; 3],
    bake_max: [u32; 3],
    mirror_flags: u8,
    mirror_sign: u8,
    mirror_center: Vec3,
) -> Vec<f32> {
    let field = core_math::field_eval::BodyField::from_prims(
        primitives,
        mirror_flags,
        mirror_sign,
        mirror_center,
    );
    bake_body_grid_region_field(&field, res, grid_min, grid_max, grid, bake_min, bake_max)
}

/// SCOPE-AWARE partial re-bake — the `BodyField` twin of [`bake_body_grid_region`], which
/// routes through here (one loop, one eval).
pub(crate) fn bake_body_grid_region_field(
    field: &core_math::field_eval::BodyField,
    res: u32,
    grid_min: Vec3,
    grid_max: Vec3,
    mut grid: Vec<f32>,
    bake_min: [u32; 3],
    bake_max: [u32; 3],
) -> Vec<f32> {
    let denom = if res > 1 { (res - 1) as f32 } else { 1.0 };
    let extent = grid_max - grid_min;
    for iz in bake_min[2]..bake_max[2].min(res) {
        for iy in bake_min[1]..bake_max[1].min(res) {
            for ix in bake_min[0]..bake_max[0].min(res) {
                let t = Vec3::new(ix as f32 / denom, iy as f32 / denom, iz as f32 / denom);
                let p = grid_min + t * extent;
                grid[(ix + iy * res + iz * res * res) as usize] = field.eval_or(p, 100.0);
            }
        }
    }
    grid
}

/// Compute per-body AABB with margins for baking.
/// Includes: prim bounding half-extents (which already account for blend_k + modifiers),
/// body-level bevel_radius, mirror expansion, and an absolute minimum margin.
/// Expand an AABB to enclose its mirror reflection across each enabled mirror axis, matching the body
/// mirror fold in `volume_bake.wgsl`. SINGLE SOURCE for the reflect math — used by
/// `compute_body_bake_bounds` (so a mirror body's grid spans both halves) AND the partial-bake
/// dirty-region union (`bake_dispatch.rs`) so a live prim edit on a mirror body re-bakes the mirrored
/// half too. Without the latter, the partial bake covers only the edited prim's half and the reflection
/// keeps the prior frame's field → the white-box ghost / thin-slab residual that returns only with the
/// mirror modifier active. Per-axis expansion yields the enclosing AABB of the box and its reflections —
/// correctness-safe (it over-covers the span between halves slightly; cheap). No-op when mirror_flags==0.
///
/// CUT mirror (`mirror_sign` bit 7, "delete the other half") is DELIBERATELY not tightened here.
/// Its result — `fold(box)` ∪ `reflect(fold(box))` — is symmetric about the plane and always a
/// SUBSET of this union box, so this stays a valid enclosure in both modes. Tightening would have
/// to thread `mirror_sign` through all ~20 `compute_body_bake_bounds` call sites in lockstep: two
/// sites disagreeing about one body's bounds means two different grids for it (the dispatch
/// records one box, a consumer recomputes another) — a cache-coherence bug far worse than the
/// tightness it would buy. The two boxes are IDENTICAL anyway whenever the body sits entirely on
/// the source side, which is the normal way a mirror is used; they differ only for geometry that
/// straddles the plane, where the cut discards material the box merely over-covers.
pub(crate) fn expand_aabb_for_mirror(
    mut aabb_min: Vec3,
    mut aabb_max: Vec3,
    mirror_flags: u8,
    mirror_center: Vec3,
) -> (Vec3, Vec3) {
    if mirror_flags != 0 {
        for axis in 0..3 {
            if (mirror_flags >> axis) & 1 != 0 {
                let c = mirror_center[axis];
                let reflected_min = 2.0 * c - aabb_max[axis];
                let reflected_max = 2.0 * c - aabb_min[axis];
                aabb_min[axis] = aabb_min[axis].min(reflected_min);
                aabb_max[axis] = aabb_max[axis].max(reflected_max);
            }
        }
    }
    (aabb_min, aabb_max)
}

/// Effective body field margin (bevel + optional Body Smooth surface motion).
pub(crate) fn body_field_bounds_margin(sg: &SceneGraph, body_nid: NodeId, bevel_radius: f32) -> f32 {
    let smooth = match sg.get(body_nid).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { field_modifiers, .. }) => {
            core_math::body_field_ops::smooth_bounds_margin(field_modifiers)
        }
        _ => 0.0,
    };
    bevel_radius.max(0.0) + smooth
}

/// Soft-feature thickness for Detail Weighted LO (PoC) — pure op params, not analytical mesh.
///
/// Takes the max of body bevel, body-op soft-K, and recursive prim blend radii (Union/Sub k,
/// Seam width, Extrude width). Used to demand enough DC cells across soft walls/fillets so
/// low-res extract still reads as curved after importance decimate.
pub(crate) fn body_soft_feature_size(sg: &SceneGraph, body_nid: NodeId) -> f32 {
    let mut feat = 0.0f32;
    if let Some(NodePayload::SdfBody {
        bevel_radius,
        body_op,
        ..
    }) = sg.get(body_nid).map(|n| &n.payload)
    {
        feat = feat.max(*bevel_radius);
        feat = feat.max(body_op.blend_k());
    }
    for p in collect_body_prims_recursive(sg, body_nid) {
        if !p.visible {
            continue;
        }
        feat = feat.max(p.operation.blend_k());
    }
    feat.max(0.0)
}

pub(crate) fn compute_body_bake_bounds(
    prims: &[SdfPrimitive],
    mirror_flags: u8,
    mirror_center: Vec3,
    bevel_radius: f32,
) -> (Vec3, Vec3) {
    if prims.is_empty() {
        return (Vec3::splat(-0.5), Vec3::splat(0.5));
    }

    // Tight AABB from prim positions ± per-axis half-extents.
    //
    // The slice is the boolean chain (`collect_body_prims_recursive` walks `resolve_child_order`).
    // The FIRST visible prim SEEDS the fold regardless of its stored op (`field_eval` `build_scope`:
    // `fold_prim(None, ..)`). A Subtract/Intersect moved to the base slot becomes the solid — its
    // AABB must be inside the grid or the rebake clips that solid (REORDER: the mesh no longer
    // matches the chain until something forces a clean grid that happens to still cover it).
    // Every LATER non-additive prim only removes or restricts material, so it must NOT expand the
    // grid: a cutter dragged outside the body would `aabb_escape` every frame and balloon the
    // volume. A smooth-cut fillet rounds inward, covered by the margin below.
    //
    // Two hard-unions swapped stay the same box (both additive). A cutter that stays a cutter
    // (not the seed) stays out of the box. Only a chain-base change moves the box, and that
    // change is the field.
    let mut aabb_min = Vec3::splat(f32::MAX);
    let mut aabb_max = Vec3::splat(f32::MIN);
    let mut seeded = false;
    for p in prims {
        if !p.visible { continue; }
        let counts = !seeded || p.operation.is_additive();
        seeded = true;
        if !counts { continue; }
        let he = p.bounding_aabb_half_extents();
        aabb_min = aabb_min.min(p.position - he);
        aabb_max = aabb_max.max(p.position + he);
    }
    // No additive prim (degenerate / transient all-cutter body): fall back to the first visible prim's
    // AABB so the grid is non-empty — same non-empty guarantee the old base-first-prim path gave, and
    // still order-invariant enough for this pathological case (there's no additive silhouette to move).
    if aabb_min.x > aabb_max.x {
        if let Some(p) = prims.iter().find(|p| p.visible) {
            let he = p.bounding_aabb_half_extents();
            aabb_min = p.position - he;
            aabb_max = p.position + he;
        }
    }

    // Degenerate fallback (no visible prims at all)
    if aabb_min.x > aabb_max.x {
        return (Vec3::splat(-0.5), Vec3::splat(0.5));
    }

    // Expand for mirror (enclose the reflection across each enabled axis) — shared helper, same math
    // the partial-bake dirty-region union uses so the grid and the dirty region agree on a mirror body.
    let (aabb_min, aabb_max) = expand_aabb_for_mirror(aabb_min, aabb_max, mirror_flags, mirror_center);

    // Margin: body bevel + 25% of half-extent + absolute minimum.
    // Also cover any smooth-cut fillet: a Subtract/Intersect with blend_k>0 can round the surface
    // outward by ~k near the blend zone. Cutters no longer move the bounds (above), so widen the
    // margin by the largest cutter blend radius instead — a CONSTANT term (max over cutters,
    // independent of where the cutter sits), so it never chases the cutter and never triggers an escape.
    // Seam: blend_k() carries the WIDTH; a RAISED seam additionally lifts a bead up to `depth`
    // OUTWARD from the additive surface, so cover width + depth (constant term, same rationale).
    // Extrude: same shape of argument — a RAISE (strength>0) lifts the additive surface by up
    // to |strength| within the width band, so cover width + |strength| (both signs via abs).
    let max_cut_k = prims.iter()
        .filter(|p| p.visible && !p.operation.is_additive())
        .map(|p| match &p.operation {
            core_math::primitives::BooleanOp::Seam { width, depth, .. } => width + depth,
            core_math::primitives::BooleanOp::Extrude { strength, width } => width + strength.abs(),
            op => op.blend_k(),
        })
        .fold(0.0_f32, f32::max);
    let half_extent = (aabb_max - aabb_min) * 0.5;
    let pct_margin = half_extent * 0.25;
    let abs_min = Vec3::splat(0.1_f32.max(bevel_radius).max(max_cut_k));
    let margin = pct_margin.max(abs_min);

    (aabb_min - margin, aabb_max + margin)
}

// ============================================================
// Primitive hashing (for cache invalidation)
// ============================================================

/// THE body-content identity (Gate 1, DC_CONSOLIDATION_GATED_PLAN.md): a stamp that changes
/// iff the body's *evaluated SDF content* changes — RECURSIVE over nested sub-bodies (a nested
/// child edit bumps the parent's ID). Consumed as the `source_hash` u64 by bake dispatch dedup,
/// atlas/entry stamps, DC dispatch → mesh_cache stamps, and (transitively, via
/// `entry.source_hash`) the PT sparse-SDF snapshot keying. The wrapped u64 is an opaque
/// equality stamp — never persisted, never ordered.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct BodyContentId(pub u64);

/// Compute the unified `BodyContentId` for a body in the scene graph — the ONE graph-level
/// entry point (replaces the pre-Gate-1 pair `compute_body_source_hash` (recursive) /
/// `render::mesh_cache::hash_primitives` (own-prims-only, DELETED)).
/// MUST fold the SAME inputs as `trigger_bake`'s stored `source_hash` (bake_dispatch.rs), INCLUDING
/// `lab_operand_gen` — omitting it (review #4) made the transform-safe undo fast-path compare a hash
/// that could never equal the stored one, so every transform undo/redo re-baked ALL bodies. Keep the
/// field set identical to trigger_bake + advance_cascade (the three must never drift again — #12).
pub(crate) fn body_content_id(sg: &SceneGraph, body_nid: NodeId, lab_operand_gen: u64) -> Option<BodyContentId> {
    let node = sg.get(body_nid)?;
    let (mirror_flags, mirror_sign, mirror_center, bevel_radius, body_origin) = match &node.payload {
        NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, bevel_radius, origin, .. } =>
            (*mirror_flags, *mirror_sign, *mirror_center, *bevel_radius, *origin),
        _ => return None,
    };

    let prims = collect_body_prims_recursive(sg, body_nid);
    if prims.is_empty() { return None; }

    let local_prims = if BODY_LOCAL_PRIMS {
        prims
    } else {
        offset_prims_to_local(&prims, body_origin)
    };
    let local_mirror_center = if BODY_LOCAL_PRIMS {
        mirror_center
    } else {
        mirror_center - body_origin
    };

    Some(BodyContentId(hash_body_source(
        sg, body_nid, &local_prims,
        mirror_flags, mirror_sign, local_mirror_center, bevel_radius, lab_operand_gen,
    )))
}

/// THE body bake-identity hash — the ONE implementation behind every `source_hash`.
/// Folds: prim content (`hash_prim_slice`) + body mirror/bevel + `lab_operand_gen`
/// + the resolved BOOLEAN-CHAIN ORDER (`hash_body_child_order`).
///
/// The three sites that used to inline this fold (`trigger_bake` in bake_dispatch.rs,
/// `advance_cascade` in cascade.rs, and `body_content_id` — the selective
/// undo invalidation) MUST produce the identical value or the dispatch dedup / cascade
/// completion / undo transform-safe fast-path compare hashes that can never match
/// (review #4/#12, cascade.rs's "MUST match trigger_bake" warning). They now all call
/// this function, so the field set structurally cannot drift.
///
/// ORDER (owner repro 2026-07-13): the boolean chain the bake serializer emits follows
/// `resolve_child_order`, NOT the prims-Vec order — `reorder_child_in_body` permutes ONLY
/// `child_order`. A hash blind to it made undo-of-reorder land on a graph whose hash
/// falsely equaled the reordered entry's stamp → `invalidate_for_undo` marked the body
/// transform_safe → the surface NEVER re-baked back. Same fix pattern as the elongation
/// hash (see `hash_prim_slice`'s elongation comment).
#[allow(clippy::too_many_arguments)]
pub(crate) fn hash_body_source(
    sg: &SceneGraph,
    body_nid: NodeId,
    local_prims: &[SdfPrimitive],
    mirror_flags: u8,
    mirror_sign: u8,
    local_mirror_center: Vec3,
    bevel_radius: f32,
    lab_operand_gen: u64,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let base = hash_prim_slice(local_prims);
    base.hash(&mut h);
    mirror_flags.hash(&mut h);
    mirror_sign.hash(&mut h);
    hash_vec3(&local_mirror_center, &mut h);
    hash_f32(bevel_radius, &mut h);
    lab_operand_gen.hash(&mut h); // #4/#12: MUST match trigger_bake's stored source_hash
    hash_body_child_order(sg, body_nid, &mut h);
    // Body field stack (esp. Body Smooth) is not fully hardwired — fold into identity.
    hash_body_field_stack_recursive(sg, body_nid, &mut h);
    h.finish()
}

fn hash_body_field_stack_recursive<H: std::hash::Hasher>(
    sg: &SceneGraph,
    body_nid: NodeId,
    h: &mut H,
) {
    use std::hash::Hash;
    if let Some(NodePayload::SdfBody { field_modifiers, .. }) =
        sg.get(body_nid).map(|n| &n.payload)
    {
        core_math::body_field_ops::hash_body_field_stack(field_modifiers).hash(h);
    } else {
        0u64.hash(h);
    }
    for slot in sg.resolve_child_order(body_nid) {
        if let Err(child_nid) = slot {
            let expand: Vec<NodeId> = match sg.get(child_nid).map(|n| &n.payload) {
                Some(NodePayload::SdfObject { .. }) => sg.child_bodies(child_nid),
                Some(NodePayload::SdfBody { .. }) => vec![child_nid],
                _ => Vec::new(),
            };
            for cid in expand {
                hash_body_field_stack_recursive(sg, cid, h);
            }
        }
    }
}

/// Fold the body's resolved eval/boolean-chain ORDER into `h`, recursively (nested
/// sub-bodies are subsumed into the top-level bake, and the serializer walks THEIR
/// `resolve_child_order` too — see `serialize_body_for_bake_hot`). Slots:
/// - prim → tag 0 + `prim_uid` (stable across undo restore: snapshots clone the graph)
/// - sub-body → tag 1 + `NodeId` (session-stable, preserved by undo snapshot restore)
///   + the sub-body's `body_op` (how its evaluated result folds into the parent scope —
///   an eval input living OUTSIDE the prim slice, same blindness class as order).
/// Uses `resolve_child_order` — the SINGLE source of truth the bake serializer walks —
/// so the hash follows exactly the sequence the field is built from.
pub(crate) fn hash_body_child_order<H: std::hash::Hasher>(
    sg: &SceneGraph,
    body_nid: NodeId,
    h: &mut H,
) {
    use std::hash::Hash;
    let prims = sg.body_primitives(body_nid).unwrap_or(&[]);
    let resolved = sg.resolve_child_order(body_nid);
    resolved.len().hash(h);
    for slot in resolved {
        match slot {
            Ok(local_idx) => {
                0u8.hash(h);
                if let Some(p) = prims.get(local_idx) {
                    p.prim_uid.hash(h);
                }
            }
            Err(child_nid) => {
                1u8.hash(h);
                child_nid.0.hash(h);
                if let Some(NodePayload::SdfBody { body_op, .. }) =
                    sg.get(child_nid).map(|n| &n.payload)
                {
                    hash_boolean_op(body_op, h);
                }
                hash_body_child_order(sg, child_nid, h);
            }
        }
    }
}

/// Compute the TEMPLATE hash for instancing. Two bodies with identical
/// prim content (shapes, operations, modifiers, relative positions) produce
/// the same template hash — even if prim_uids differ (Shift+D clones get new UIDs).
/// This is separate from source_hash which includes UIDs for cache invalidation.
pub(crate) fn compute_template_hash(sg: &SceneGraph, body_nid: NodeId) -> Option<u64> {
    let node = sg.get(body_nid)?;
    let (mirror_flags, mirror_sign, mirror_center, bevel_radius, body_origin) = match &node.payload {
        NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, bevel_radius, origin, .. } =>
            (*mirror_flags, *mirror_sign, *mirror_center, *bevel_radius, *origin),
        _ => return None,
    };

    let prims = collect_body_prims_recursive(sg, body_nid);
    if prims.is_empty() { return None; }

    let local_prims = if BODY_LOCAL_PRIMS { prims } else { offset_prims_to_local(&prims, body_origin) };
    let local_mirror_center = if BODY_LOCAL_PRIMS { mirror_center } else { mirror_center - body_origin };

    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // Hash prim content WITHOUT uid (enables instancing across clones)
    let base = hash_prim_slice_no_uid(&local_prims);
    base.hash(&mut h);
    mirror_flags.hash(&mut h);
    mirror_sign.hash(&mut h);
    hash_vec3(&local_mirror_center, &mut h);
    hash_f32(bevel_radius, &mut h);
    Some(h.finish())
}

/// Hash prim content for instancing — same as hash_prim_slice but WITHOUT prim_uid.
/// Two identical cubes with different UIDs produce the same hash.
fn hash_prim_slice_no_uid(primitives: &[SdfPrimitive]) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    primitives.len().hash(&mut hasher);
    for p in primitives {
        // NO prim_uid — that's the whole point
        hash_vec3(&p.position, &mut hasher);
        hash_vec3(&p.rotation_deg, &mut hasher);
        hash_vec3(&p.scale, &mut hasher);
        hash_vec3(&p.elongation, &mut hasher); // field-relevant domain warp (was missing)
        p.visible.hash(&mut hasher);
        hash_shape(&p.shape, &mut hasher);
        hash_boolean_op(&p.operation, &mut hasher);
        p.mirror_flags.hash(&mut hasher);
        hash_vec3(&p.mirror_center, &mut hasher);
        hash_modifier(&p.modifier, &mut hasher);
        hash_blend_falloff(&p.blend_falloff, &mut hasher);
        p.subdivision.hash(&mut hasher);
        hash_blend_profile(&p.blend_profile, &mut hasher);
        hash_f32(p.blend_profile_shape, &mut hasher);
        hash_blend_curve(&p.blend_curve, &mut hasher);
        hash_f32(p.bevel_profile_top, &mut hasher);
        hash_f32(p.bevel_profile_bot, &mut hasher);
        hash_f32(p.bevel_profile_vert, &mut hasher);
        hash_f32(p.chamfer_angle_top, &mut hasher);
        hash_f32(p.chamfer_angle_bot, &mut hasher);
        hash_f32(p.chamfer_angle_vert, &mut hasher);
        hash_cylinder_bevel(&p.cyl_bevel, &mut hasher);
        // Skip instance_master_id — instancing-specific, not content
        p.modifiers.len().hash(&mut hasher);
        for m in &p.modifiers { hash_modifier(m, &mut hasher); }
        p.sculpt_layers.len().hash(&mut hasher);
        for sl in &p.sculpt_layers { hash_sculpt_layer(sl, &mut hasher); }
        p.clip_curves.len().hash(&mut hasher);
        for clip in &p.clip_curves {
            clip.enabled.hash(&mut hasher);
            clip.mode.hash(&mut hasher);
            hash_f32(clip.depth, &mut hasher);
            clip.profile.verts.len().hash(&mut hasher);
            for cv in &clip.profile.verts {
                hash_f32(cv.pos.x, &mut hasher);
                hash_f32(cv.pos.y, &mut hasher);
                hash_f32(cv.bevel_radius, &mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Hash a slice of SDF primitives for cache invalidation.
pub(crate) fn hash_prim_slice(primitives: &[SdfPrimitive]) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;

    let mut hasher = DefaultHasher::new();
    primitives.len().hash(&mut hasher);
    for p in primitives {
        p.prim_uid.hash(&mut hasher);
        hash_vec3(&p.position, &mut hasher);
        hash_vec3(&p.rotation_deg, &mut hasher);
        hash_vec3(&p.scale, &mut hasher);
        // Elongation is a domain warp (`p.axis -= clamp(p.axis, -h, h)`) — field-relevant in
        // every eval path incl. the bake. It was MISSING since the elongate tool shipped: the
        // dispatch dedup (entry.source_hash == source_hash skip) was blind to elongation-only
        // edits, and the mirror no-op guard (prim_actions.rs) reuses this hash as its
        // bake-identity predicate — without this line it would skip elongation bakes.
        hash_vec3(&p.elongation, &mut hasher);
        p.visible.hash(&mut hasher);
        hash_shape(&p.shape, &mut hasher);
        hash_boolean_op(&p.operation, &mut hasher);
        p.mirror_flags.hash(&mut hasher);
        hash_vec3(&p.mirror_center, &mut hasher);
        hash_modifier(&p.modifier, &mut hasher);
        hash_blend_falloff(&p.blend_falloff, &mut hasher);
        p.subdivision.hash(&mut hasher);
        hash_blend_profile(&p.blend_profile, &mut hasher);
        hash_f32(p.blend_profile_shape, &mut hasher);
        hash_blend_curve(&p.blend_curve, &mut hasher);
        hash_f32(p.bevel_profile_top, &mut hasher);
        hash_f32(p.bevel_profile_bot, &mut hasher);
        hash_f32(p.bevel_profile_vert, &mut hasher);
        hash_f32(p.chamfer_angle_top, &mut hasher);
        hash_f32(p.chamfer_angle_bot, &mut hasher);
        hash_f32(p.chamfer_angle_vert, &mut hasher);
        hash_cylinder_bevel(&p.cyl_bevel, &mut hasher);
        p.instance_master_id.hash(&mut hasher);
        p.modifiers.len().hash(&mut hasher);
        for m in &p.modifiers {
            hash_modifier(m, &mut hasher);
        }
        p.sculpt_layers.len().hash(&mut hasher);
        for sl in &p.sculpt_layers {
            hash_sculpt_layer(sl, &mut hasher);
        }
        // Clip curves (stacked boolean cutters)
        p.clip_curves.len().hash(&mut hasher);
        for clip in &p.clip_curves {
            clip.enabled.hash(&mut hasher);
            clip.mode.hash(&mut hasher);
            hash_f32(clip.depth, &mut hasher);
            clip.profile.verts.len().hash(&mut hasher);
            for cv in &clip.profile.verts {
                hash_f32(cv.pos.x, &mut hasher);
                hash_f32(cv.pos.y, &mut hasher);
                hash_f32(cv.bevel_radius, &mut hasher);
            }
            hash_f32(clip.profile.plane.normal.x, &mut hasher);
            hash_f32(clip.profile.plane.normal.y, &mut hasher);
            hash_f32(clip.profile.plane.normal.z, &mut hasher);
            hash_f32(clip.profile.plane.tangent_u.x, &mut hasher);
            hash_f32(clip.profile.plane.tangent_u.y, &mut hasher);
            hash_f32(clip.profile.plane.tangent_u.z, &mut hasher);
        }
        // Legacy circle fallback
        p.clip_curve_enabled.hash(&mut hasher);
        hash_f32(p.clip_curve_radius, &mut hasher);
        p.clip_curve_extrude_axis.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_bevel_mode<H: std::hash::Hasher>(bm: &core_math::primitives::BevelMode, h: &mut H) {
    use std::hash::Hash;
    use core_math::primitives::BevelMode;
    let tag: u32 = match bm { BevelMode::None => 0, BevelMode::Round => 1, BevelMode::Chamfer => 2 };
    tag.hash(h);
}

fn hash_shape<H: std::hash::Hasher>(shape: &SdfShape, h: &mut H) {
    use std::hash::Hash;
    match shape {
        SdfShape::Sphere { radius } => { 0u32.hash(h); hash_f32(*radius, h); }
        SdfShape::Box { half_extents, bevel } => {
            1u32.hash(h); hash_vec3(half_extents, h);
            for &r in &bevel.radii { hash_f32(r, h); }
            for m in &bevel.modes { hash_bevel_mode(m, h); }
            bevel.up_axis.hash(h);
            // Uniform vs Per-Edge changes the field — must invalidate bake.
            (bevel.algo.as_u8() as u32).hash(h);
        }
        SdfShape::Cylinder { radius, half_height } => { 2u32.hash(h); hash_f32(*radius, h); hash_f32(*half_height, h); }
        SdfShape::Torus { major_radius, minor_radius } => { 3u32.hash(h); hash_f32(*major_radius, h); hash_f32(*minor_radius, h); }
        SdfShape::Cone { radius, half_height } => { 4u32.hash(h); hash_f32(*radius, h); hash_f32(*half_height, h); }
        SdfShape::Capsule { radius, half_height } => { 5u32.hash(h); hash_f32(*radius, h); hash_f32(*half_height, h); }
        SdfShape::Stair { half_extents, num_steps } => { 6u32.hash(h); hash_vec3(half_extents, h); num_steps.hash(h); }
        SdfShape::SpiralStair { radius, half_height, num_steps, step_width } => {
            7u32.hash(h); hash_f32(*radius, h); hash_f32(*half_height, h); num_steps.hash(h); hash_f32(*step_width, h);
        }
        SdfShape::Billboard { width, height, face_camera } => { 8u32.hash(h); hash_f32(*width, h); hash_f32(*height, h); face_camera.hash(h); }
        SdfShape::CurveExtrude { profile, half_depth, wall_t, rim, open, thickness, tube } => {
            9u32.hash(h);
            profile.verts.len().hash(h);
            for v in &profile.verts { hash_f32(v.pos.x, h); hash_f32(v.pos.y, h); hash_f32(v.bevel_radius, h); }
            hash_f32(*half_depth, h);
            hash_f32(*wall_t, h);
            hash_f32(*rim, h);
            open.hash(h);
            hash_f32(*thickness, h);
            tube.hash(h);
        }
        SdfShape::MeshVolume { grid, .. } => { 10u32.hash(h); grid.res.hash(h); hash_vec3(&grid.bounds_min, h); hash_f32(grid.cell, h); grid.dist.len().hash(h); let mut j = 0usize; while j < grid.dist.len() { hash_f32(grid.dist[j], h); j += 1013; } }
        SdfShape::HalfSpace => { 11u32.hash(h); } // infinite plane, no shape params (transform is the plane)
        SdfShape::SpinePipe {
            points, radius, closed, joins, point_scale, handles_in, handles_out,
        } => {
            12u32.hash(h);
            points.len().hash(h);
            for p in points { hash_vec3(p, h); }
            hash_f32(*radius, h);
            closed.hash(h);
            joins.len().hash(h);
            for j in joins { hash_f32(*j, h); }
            point_scale.len().hash(h);
            for s in point_scale { hash_f32(*s, h); }
            handles_in.len().hash(h);
            for v in handles_in { hash_vec3(v, h); }
            handles_out.len().hash(h);
            for v in handles_out { hash_vec3(v, h); }
        }
    }
}

fn hash_boolean_op<H: std::hash::Hasher>(op: &BooleanOp, h: &mut H) {
    use std::hash::Hash;
    match op {
        BooleanOp::Union { k }           => { 0u32.hash(h); hash_f32(*k, h); }
        BooleanOp::Subtract { k }        => { 1u32.hash(h); hash_f32(*k, h); }
        BooleanOp::Intersect             => { 2u32.hash(h); }
        BooleanOp::ChamferUnion { k }    => { 3u32.hash(h); hash_f32(*k, h); }
        BooleanOp::ChamferSubtract { k } => { 4u32.hash(h); hash_f32(*k, h); }
        BooleanOp::SmoothIntersect { k } => { 5u32.hash(h); hash_f32(*k, h); }
        // ALL seam params are serializer-read → ALL must be in the bake identity hash.
        BooleanOp::Seam { width, depth, profile, raised } => {
            6u32.hash(h); hash_f32(*width, h); hash_f32(*depth, h);
            profile.hash(h); raised.hash(h);
        }
        // ALL extrude params are serializer-read → both in the bake identity hash.
        BooleanOp::Extrude { strength, width } => {
            7u32.hash(h); hash_f32(*strength, h); hash_f32(*width, h);
        }
    }
}

fn hash_blend_profile<H: std::hash::Hasher>(bp: &core_math::primitives::BlendProfile, h: &mut H) {
    use std::hash::Hash;
    use core_math::primitives::BlendProfile;
    let tag: u32 = match bp {
        BlendProfile::Polynomial => 0,
        BlendProfile::Chamfer => 1,
        BlendProfile::Circular => 2,
        BlendProfile::Curve => 3,
    };
    tag.hash(h);
}

fn hash_blend_curve<H: std::hash::Hasher>(c: &core_math::primitives::BlendCurve, h: &mut H) {
    use std::hash::Hash;
    c.points.len().hash(h);
    for p in &c.points {
        hash_f32(p.x, h);
        hash_f32(p.y, h);
        p.soft.hash(h);
    }
}

fn hash_modifier<H: std::hash::Hasher>(m: &core_math::primitives::Modifier, h: &mut H) {
    use std::hash::Hash;
    hash_modifier_type(&m.mod_type, h);
    hash_f32(m.strength, h);
    m.axis.hash(h);
    hash_f32(m.param2, h);
    hash_volume_mask(&m.mask, h);
    // Image Displace: content identity (size + sampled pixels) — path alone is not enough.
    match &m.height_map {
        None => 0u8.hash(h),
        Some(hm) => {
            1u8.hash(h);
            hm.w.hash(h);
            hm.h.hash(h);
            hm.pixels.len().hash(h);
            // Sample a few bytes so hash is cheap but changes with image content.
            if !hm.pixels.is_empty() {
                hm.pixels[0].hash(h);
                hm.pixels[hm.pixels.len() / 2].hash(h);
                hm.pixels[hm.pixels.len() - 1].hash(h);
                // Fold a stride sample for better collision resistance.
                let step = (hm.pixels.len() / 17).max(1);
                let mut i = 0;
                while i < hm.pixels.len() {
                    hm.pixels[i].hash(h);
                    i += step;
                }
            }
        }
    }
}

fn hash_modifier_type<H: std::hash::Hasher>(mt: &core_math::primitives::ModifierType, h: &mut H) {
    use std::hash::Hash;
    use core_math::primitives::ModifierType;
    let tag: u32 = match mt {
        ModifierType::None => 0, ModifierType::Twist => 1, ModifierType::Taper => 2,
        ModifierType::Smooth => 3, ModifierType::Displace => 4, ModifierType::Mirror => 5,
        ModifierType::Shear => 6, ModifierType::Sculpt => 7,
        ModifierType::Spherize => 8, ModifierType::Shell => 9,
        ModifierType::Emboss => 10, ModifierType::Deboss => 11,
        ModifierType::Array => 12,
    };
    tag.hash(h);
}

fn hash_volume_mask<H: std::hash::Hasher>(mask: &core_math::primitives::VolumeMask, h: &mut H) {
    use std::hash::Hash;
    hash_mask_shape(&mask.shape, h);
    hash_vec3(&mask.center, h); hash_vec3(&mask.extents, h);
    hash_f32(mask.feather, h); mask.invert.hash(h);
    hash_f32(mask.noise_frequency, h); mask.noise_octaves.hash(h);
    hash_f32(mask.noise_lacunarity, h); hash_f32(mask.noise_gain, h);
    hash_f32(mask.noise_threshold, h); hash_vec3(&mask.noise_offset, h);
}

fn hash_mask_shape<H: std::hash::Hasher>(ms: &core_math::primitives::MaskShape, h: &mut H) {
    use std::hash::Hash;
    use core_math::primitives::MaskShape;
    let tag: u32 = match ms { MaskShape::None => 0, MaskShape::Sphere => 1, MaskShape::Box => 2, MaskShape::Gradient => 3, MaskShape::Noise3D => 4 };
    tag.hash(h);
}

fn hash_blend_falloff<H: std::hash::Hasher>(bf: &core_math::primitives::BlendFalloff, h: &mut H) {
    use std::hash::Hash;
    hash_vec3(&bf.direction, h);
    hash_f32(bf.origin_offset, h);
    hash_f32(bf.length, h);
    hash_f32(bf.wall_protect, h);
    bf.wall_protect_axis.hash(h);
    (bf.curve_type as u32).hash(h);
    hash_f32(bf.curve_power, h);
}

fn hash_cylinder_bevel<H: std::hash::Hasher>(cb: &core_math::primitives::CylinderBevel, h: &mut H) {
    hash_f32(cb.top, h); hash_f32(cb.bot, h);
    hash_bevel_mode(&cb.top_mode, h); hash_bevel_mode(&cb.bot_mode, h);
}

fn hash_sculpt_layer<H: std::hash::Hasher>(sl: &core_math::sculpt_layer::SculptLayer, h: &mut H) {
    use std::hash::Hash;
    sl.id.hash(h);
    hash_f32(sl.opacity, h);
    sl.visible.hash(h);
    hash_layer_blend_mode(&sl.blend_mode, h);
    hash_f32(sl.delta_grid.cell_size, h);
    sl.delta_grid.cells.len().hash(h);
    let mut keys: Vec<_> = sl.delta_grid.cells.keys().copied().collect();
    keys.sort_unstable();
    for k in &keys {
        k.hash(h);
        hash_f32(sl.delta_grid.cells[k], h);
    }
}

fn hash_layer_blend_mode<H: std::hash::Hasher>(m: &core_math::sculpt_layer::LayerBlendMode, h: &mut H) {
    use std::hash::Hash;
    use core_math::sculpt_layer::LayerBlendMode;
    let tag: u32 = match m { LayerBlendMode::Add => 0, LayerBlendMode::Subtract => 1, LayerBlendMode::Max => 2, LayerBlendMode::Min => 3 };
    tag.hash(h);
}

/// Hash a Vec3 by hashing each component's bits.
#[inline]
pub(crate) fn hash_vec3<H: std::hash::Hasher>(v: &Vec3, h: &mut H) {
    hash_f32(v.x, h); hash_f32(v.y, h); hash_f32(v.z, h);
}

#[inline]
pub(crate) fn hash_f32<H: std::hash::Hasher>(v: f32, hasher: &mut H) {
    use std::hash::Hash;
    v.to_bits().hash(hasher);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::primitives::SdfPrimitive;
    use core_math::sdf_shapes::BoxBevel;

    /// Sample the baked grid at the nearest voxel to a world point.
    fn at(grid: &[f32], res: u32, gmin: Vec3, gmax: Vec3, p: Vec3) -> f32 {
        let t = (p - gmin) / (gmax - gmin);
        let v = (t * (res - 1) as f32).round();
        let (x, y, z) = (
            (v.x as i32).clamp(0, res as i32 - 1) as u32,
            (v.y as i32).clamp(0, res as i32 - 1) as u32,
            (v.z as i32).clamp(0, res as i32 - 1) as u32,
        );
        grid[(x + y * res + z * res * res) as usize]
    }

    fn box_prim(pos: Vec3, he: Vec3) -> SdfPrimitive {
        SdfPrimitive::new("Box".into(), SdfShape::Box { half_extents: he, bevel: BoxBevel::default() }, pos)
    }

    /// REGRESSION (mesh explosion on symmetry): the CPU cascade / fallback grid bake
    /// (`bake_body_grid`) MUST apply the body-level mirror as a TRUE UNION. Before the fix it
    /// called bare `eval_scene`, baking only the original half into a mirror-expanded grid →
    /// the reflected half was empty (flat slab) and the under-sampled grid produced scattered
    /// DC fragments. This is the missing 5th eval path of commit 90522b5.
    #[test]
    fn bake_body_grid_applies_mirror_union() {
        let res = 48u32;
        let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
        // Off-center box entirely on -Y; mirror Y about y=0.
        let prims = vec![box_prim(Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5))];

        let g = bake_body_grid(&prims, res, gmin, gmax, 0b010, 0, Vec3::ZERO);
        // Original -Y half solid.
        assert!(at(&g, res, gmin, gmax, Vec3::new(0.0, -1.0, 0.0)) < 0.0, "original -Y half must be solid");
        // Reflected +Y half ALSO solid (the union — was empty under the old bare eval_scene).
        assert!(at(&g, res, gmin, gmax, Vec3::new(0.0, 1.0, 0.0)) < 0.0,
            "FIXED: mirrored +Y half must be solid in the CPU bake (was the flat-slab/explosion bug)");

        // mirror_flags == 0 stays one-sided (the common path is unchanged).
        let g0 = bake_body_grid(&prims, res, gmin, gmax, 0, 0, Vec3::ZERO);
        assert!(at(&g0, res, gmin, gmax, Vec3::new(0.0, -1.0, 0.0)) < 0.0, "no-mirror -Y solid");
        assert!(at(&g0, res, gmin, gmax, Vec3::new(0.0, 1.0, 0.0)) > 0.0, "no-mirror +Y empty");
    }

    /// CHAIN BASE: the slice is eval order. The first visible prim seeds the solid even when its
    /// stored op is Subtract — that is the fold (`fold_prim(None, …)`). A cutter that is NOT the
    /// seed must not balloon the grid. Two additive prims swapped leave the box unchanged.
    #[test]
    fn bake_bounds_follow_chain_base() {
        use core_math::primitives::BooleanOp;
        let mut base = box_prim(Vec3::ZERO, Vec3::splat(0.5));
        base.operation = BooleanOp::Union { k: 0.0 };
        let mut cutter = box_prim(Vec3::new(5.0, 0.0, 0.0), Vec3::splat(0.5));
        cutter.operation = BooleanOp::Subtract { k: 0.0 };

        let seed_then_cut = compute_body_bake_bounds(&[base.clone(), cutter.clone()], 0, Vec3::ZERO, 0.0);
        assert!(seed_then_cut.1.x < 2.0, "a cutter that stays a cutter must not balloon, got max.x={}", seed_then_cut.1.x);

        // The subtract is now the chain seed: it IS the solid, and the union adds beside it.
        let cut_is_seed = compute_body_bake_bounds(&[cutter, base.clone()], 0, Vec3::ZERO, 0.0);
        assert!(cut_is_seed.1.x > 4.0, "chain-base subtract must be inside the grid, got {cut_is_seed:?}");
        assert!(cut_is_seed.0.x < 1.0, "the later union must still contribute, got {cut_is_seed:?}");

        let mut other = box_prim(Vec3::new(3.0, 0.0, 0.0), Vec3::splat(0.4));
        other.operation = BooleanOp::Union { k: 0.0 };
        let u1 = compute_body_bake_bounds(&[base.clone(), other.clone()], 0, Vec3::ZERO, 0.0);
        let u2 = compute_body_bake_bounds(&[other, base], 0, Vec3::ZERO, 0.0);
        assert!(
            (u1.0 - u2.0).length() < 1e-5 && (u1.1 - u2.1).length() < 1e-5,
            "swapping two unions must not move bounds: {u1:?} vs {u2:?}"
        );
    }

    /// NESTED-ORIGIN COMPOSITION (the "grouped/nested geometry vanishes" root cause):
    /// prim positions are body-local — relative to the origin of the body they belong to. A nested
    /// sub-body owns its OWN (usually centroid-recalculated) origin, so `collect_body_prims_recursive`
    /// MUST shift each sub-body prim by `(sub_origin - top_origin)` into the top body's single bake
    /// frame. Before the fix it collected sub-body prims at their RAW positions, so the grid bounds
    /// (and the bake) placed nested geometry in the wrong frame → outside the grid → clipped/vanished
    /// after grouping or dragging one prim into another. Every prim is now treated identically — no
    /// special base, no per-group exception. (Mirrors the same shift in serialize_body_for_bake_hot.)
    #[test]
    fn nested_subbody_prims_compose_into_top_frame() {
        use core_math::primitives::{BlendProfile, PbrMaterial, BooleanOp};
        use core_math::scene_graph::SceneGraph;
        use glam::Quat;

        let body_payload = |name: &str, origin: Vec3| NodePayload::SdfBody {
            name: name.into(), primitives: Vec::new(), mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),};

        let mut sg = SceneGraph::new();
        let root = sg.root();
        // Top body at origin (1,0,0); nested sub-body at origin (3,0,0).
        let top = sg.add_child(root, body_payload("Top", Vec3::new(1.0, 0.0, 0.0)));
        let sub = sg.add_child(top, body_payload("Sub", Vec3::new(3.0, 0.0, 0.0)));
        // Top's OWN prim at top-local (0.5,0,0) — shift 0, must stay put.
        sg.body_primitives_mut(top).unwrap().push(box_prim(Vec3::new(0.5, 0.0, 0.0), Vec3::splat(0.5)));
        // Sub's prim at sub-local (0,0,0) — true top-frame position = 0 + (3 - 1) = (2,0,0).
        sg.body_primitives_mut(sub).unwrap().push(box_prim(Vec3::ZERO, Vec3::splat(0.5)));

        let prims = collect_body_prims_recursive(&sg, top);
        assert_eq!(prims.len(), 2, "collect must gather top + nested prims");
        let top_prim = prims.iter().find(|p| (p.position - Vec3::new(0.5, 0.0, 0.0)).length() < 1e-4);
        let sub_prim = prims.iter().find(|p| (p.position - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4);
        assert!(top_prim.is_some(), "top body's own prim must be UNCHANGED at (0.5,0,0): {:?}",
            prims.iter().map(|p| p.position).collect::<Vec<_>>());
        assert!(sub_prim.is_some(),
            "FIXED: nested sub-body prim must be composed to top-frame (2,0,0) = raw(0,0,0)+(sub_origin 3 - top_origin 1); \
             was raw (0,0,0) → grid missed it → geometry vanished. got: {:?}",
            prims.iter().map(|p| p.position).collect::<Vec<_>>());

        // And the grid bounds must now reach the nested box (centered at x=2, he=0.5 → max.x ≥ 2.5).
        let (_bmin, bmax) = compute_body_bake_bounds(&prims, 0, Vec3::ZERO, 0.0);
        assert!(bmax.x >= 2.5, "grid bounds must cover the composed nested box, got max.x={}", bmax.x);
    }

    /// Partial re-bake must also mirror — otherwise a prim edit on a mirror body punches a
    /// non-mirrored hole into the mirrored grid.
    #[test]
    fn bake_body_grid_region_applies_mirror_union() {
        let res = 48u32;
        let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
        let prims = vec![box_prim(Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5))];
        // Seed the whole grid via the (now-mirrored) full bake, then re-bake the whole region.
        let base = bake_body_grid(&prims, res, gmin, gmax, 0b010, 0, Vec3::ZERO);
        let g = bake_body_grid_region(&prims, res, gmin, gmax, base, [0, 0, 0], [res, res, res], 0b010, 0, Vec3::ZERO);
        assert!(at(&g, res, gmin, gmax, Vec3::new(0.0, 1.0, 0.0)) < 0.0,
            "partial re-bake must keep the mirrored +Y half solid");
    }

    fn sphere(uid: u32, pos: Vec3, radius: f32, op: core_math::primitives::BooleanOp) -> SdfPrimitive {
        let mut p = SdfPrimitive::new(format!("S{uid}"), SdfShape::Sphere { radius }, pos);
        p.prim_uid = uid;
        p.operation = op;
        p
    }

    fn chain_body(prims: Vec<SdfPrimitive>) -> (core_math::scene_graph::SceneGraph, core_math::scene_graph::NodeId) {
        use core_math::primitives::{BlendProfile, PbrMaterial, BooleanOp};
        use core_math::scene_graph::{SceneGraph, NodePayload};
        use glam::Quat;
        let mut sg = SceneGraph::new();
        let body = sg.add_child(sg.root(), NodePayload::SdfBody {
            name: "Body".into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
            mesh_modifiers: Vec::new(), field_modifiers: Vec::new(),
        });
        sg.ensure_body_child_order(body);
        (sg, body)
    }

    fn prim_uids(sg: &core_math::scene_graph::SceneGraph, body: core_math::scene_graph::NodeId) -> Vec<u32> {
        collect_body_prims_recursive(sg, body).iter().map(|p| p.prim_uid).collect()
    }

    /// REORDER: moving a Subtract to the chain base changes the field (the seed is solid
    /// regardless of its stored op). The collector and the flat CPU bake must follow that
    /// chain, and the grid bounds must cover the new solid. Before the fix, collect walked
    /// the primitives Vec, so the CPU bake stayed on the pre-reorder field and the bounds
    /// skipped the Subtract — the baked solid was clipped.
    #[test]
    fn reorder_subtract_to_base_matches_field_and_bounds() {
        use core_math::field_eval::BodyField;
        use core_math::primitives::BooleanOp;
        use core_math::scene_graph::BodyChildRef;

        let (mut sg, body) = chain_body(vec![
            sphere(100, Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 }),
            sphere(101, Vec3::new(2.0, 0.0, 0.0), 0.8, BooleanOp::Subtract { k: 0.0 }),
        ]);
        let probe = Vec3::new(2.0, 0.0, 0.0);
        let before = BodyField::from_graph(&sg, body).eval_or(probe, 100.0);
        assert!(before > 0.0, "cutter is not solid while it is not the seed, got {before}");
        assert_eq!(prim_uids(&sg, body), vec![100, 101]);

        sg.reorder_child_order(body, BodyChildRef::Prim(101), 0);
        let field = BodyField::from_graph(&sg, body);
        let after = field.eval_or(probe, 100.0);
        assert!(after < 0.0, "subtract moved to the base slot must be solid at its center, got {after}");
        assert_eq!(prim_uids(&sg, body), vec![101, 100], "collect must follow child_order, not the Vec");

        let collected = collect_body_prims_recursive(&sg, body);
        let (bmin, bmax) = compute_body_bake_bounds(&collected, 0, Vec3::ZERO, 0.0);
        assert!(bmin.x < 1.3 && bmax.x > 2.7, "grid must cover the new seed at x=2, got {bmin:?}..{bmax:?}");

        let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::new(4.0, 2.0, 2.0));
        let grid = bake_body_grid(&collected, 24, gmin, gmax, 0, 0, Vec3::ZERO);
        let baked = at(&grid, 24, gmin, gmax, probe);
        assert!(baked < 0.0, "CPU bake of the collected slice must be the reordered field, got {baked}");
        let direct = field.eval_or(probe, 100.0);
        assert!((baked - direct).abs() < 0.35, "flat bake {baked} vs field {direct}");

        let h_reordered = compute_template_hash(&sg, body).unwrap();
        sg.reorder_child_order(body, BodyChildRef::Prim(101), 1);
        let h_restored = compute_template_hash(&sg, body).unwrap();
        assert_ne!(h_reordered, h_restored, "template identity must see the boolean chain");
    }

    /// A body nested under Empty is not a child_order slot. Collect must still emit it
    /// (the set cannot shrink) and a default order stays prims-then-nested.
    #[test]
    fn collect_keeps_body_nested_under_empty() {
        use core_math::primitives::{BlendProfile, PbrMaterial, BooleanOp};
        use core_math::scene_graph::{SceneGraph, NodePayload};
        use glam::Quat;
        let body = |name: &str, prims: Vec<SdfPrimitive>| NodePayload::SdfBody {
            name: name.into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
            mesh_modifiers: Vec::new(), field_modifiers: Vec::new(),
        };
        let mut sg = SceneGraph::new();
        let top = sg.add_child(sg.root(), body("Top", vec![
            sphere(10, Vec3::ZERO, 0.5, BooleanOp::Union { k: 0.0 }),
        ]));
        let empty = sg.add_child(top, NodePayload::Empty { name: "Slot".into() });
        let _nested = sg.add_child(empty, body("Nested", vec![
            sphere(20, Vec3::X, 0.4, BooleanOp::Union { k: 0.0 }),
        ]));
        sg.ensure_body_child_order(top);
        assert_eq!(prim_uids(&sg, top), vec![10, 20]);
    }

    /// Undo/redo of a reorder is the graph snapshot `transaction()` already takes.
    /// Restoring it must put the chain, the field, and the collected slice back.
    #[test]
    fn reorder_undo_redo_restores_field() {
        use core_math::field_eval::BodyField;
        use core_math::primitives::BooleanOp;
        use core_math::scene_graph::BodyChildRef;

        let mut app = crate::FourthLayerApp::new_headless();
        let (sg, body) = chain_body(vec![
            sphere(100, Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 }),
            sphere(101, Vec3::new(2.0, 0.0, 0.0), 0.8, BooleanOp::Subtract { k: 0.0 }),
        ]);
        app.scene.scene_graph = sg;
        app.sync_flat_cache();
        let probe = Vec3::new(2.0, 0.0, 0.0);
        let eval = |app: &crate::FourthLayerApp| {
            BodyField::from_graph(&app.scene.scene_graph, body).eval_or(probe, 100.0)
        };
        let before = eval(&app);
        let before_uids = prim_uids(&app.scene.scene_graph, body);
        app.transaction("Reorder prim", |s| {
            s.reorder_child_in_body(body, BodyChildRef::Prim(101), 0);
        });
        let reordered = eval(&app);
        assert!(before > 0.0 && reordered < 0.0, "reorder must change the field ({before} -> {reordered})");
        assert_eq!(prim_uids(&app.scene.scene_graph, body), vec![101, 100]);
        app.undo();
        assert!((eval(&app) - before).abs() < 1e-4, "undo must restore the pre-reorder field");
        assert_eq!(prim_uids(&app.scene.scene_graph, body), before_uids);
        app.redo();
        assert!((eval(&app) - reordered).abs() < 1e-4, "redo must restore the reordered field");
        assert_eq!(prim_uids(&app.scene.scene_graph, body), vec![101, 100]);
    }
}
