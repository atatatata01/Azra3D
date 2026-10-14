use super::{
    DisplayMesh, DisplayMeshFoldCtx, DisplayMeshOp,
};
use super::normals::mesh_recompute_normals;
use super::op_impls::{mesh_decimate, mesh_mirror, mesh_smooth_laplacian};

// ============================================================
// Fold
// ============================================================


/// Caches the fold's INTERMEDIATE result after each op, keyed by the ops that produced it.
///
/// The app hashes the WHOLE modifier stack (`mesh_mod_display_stamp_for`), so editing any modifier
/// re-folds every op from scratch. Measured on a 40k-tri sphere
/// (`bench_prefix_cache_upper_bound`): editing the LAST op of a depth-8 stack costs 118 ms today
/// and 43 ms if the untouched prefix is reused — 2.8x, saving 76 ms per edit. At depth 4 it is
/// 4.5x. Shallow stacks gain little (one op is most of the cost), so this specifically fixes the
/// deep-stack case, which is the one that feels broken.
///
/// **Keyed on the op PREFIX, not on an index.** Each entry records the running hash of every op up
/// to and including the one that produced it, so reordering, inserting, deleting or editing an op
/// invalidates that entry and everything after it — while everything BEFORE stays valid. An
/// index-keyed cache would silently serve a stale mesh after a reorder.
///
/// Memory: one `DisplayMesh` per cached step. Bounded by `MAX_CACHED_STEPS` because a deep stack on
/// a large mesh is exactly where this helps AND exactly where clones are expensive — the cache
/// keeps the DEEPEST entries, since those are the ones an edit near the end of the stack reuses.
#[derive(Default)]
pub struct DisplayMeshPrefixCache {
    /// `(running_prefix_hash, mesh_after_that_op)`, ordered by depth.
    /// `(depth, running_prefix_hash, mesh_after_that_op)` — tail entries only, any order.
    steps: Vec<(usize, u64, DisplayMesh)>,
}

/// How many trailing prefixes to store. Each costs a mesh clone, so this trades memory + clone
/// time against how far up the stack an edit can still hit the cache. 3 covers the common case
/// (tweaking the last op or two) at 3 clones instead of one per op.
const CACHE_TAIL_DEPTH: usize = 3;

impl DisplayMeshPrefixCache {
    /// Running hash of `ops[..=i]` — the identity of "the mesh after op i".
    fn prefix_hash(ops: &[DisplayMeshOp], upto: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        upto.hash(&mut h);
        for op in &ops[..=upto] {
            hash_display_mesh_op(op, &mut h);
        }
        h.finish()
    }

    /// Longest cached prefix valid for `ops`: `(ops_already_applied, mesh)`.
    ///
    /// Walks forward and stops at the first mismatch, so a change at op `k` reuses ops `0..k` and
    /// re-runs the rest — which is the whole point.
    fn longest_valid_prefix(&self, ops: &[DisplayMeshOp]) -> Option<(usize, &DisplayMesh)> {
        // Entries are tail-only now, so each carries its own DEPTH rather than being indexed by
        // position in `steps`. Scan for the deepest one whose prefix still matches.
        let mut best: Option<(usize, &DisplayMesh)> = None;
        for (depth, hash, mesh) in self.steps.iter().map(|(d, h, m)| (*d, *h, m)) {
            if depth < ops.len() && Self::prefix_hash(ops, depth) == hash {
                if best.map_or(true, |(b, _)| depth + 1 > b) {
                    best = Some((depth + 1, mesh));
                }
            }
        }
        best
    }

    /// Drop every entry at depth `from` or deeper — those meshes were produced by ops that changed.
    fn truncate_from(&mut self, from: usize) {
        self.steps.retain(|(depth, _, _)| *depth < from);
    }
}

/// Hash one op. Mirrors `hash_kind`'s discipline: every field that changes the OUTPUT must be
/// hashed, or the cache serves a stale mesh after an edit that it failed to notice.
fn hash_display_mesh_op(op: &DisplayMeshOp, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match op {
        DisplayMeshOp::Mirror { axis_bits, center, weld, weld_threshold } => {
            0u8.hash(h);
            axis_bits.hash(h);
            for c in [center.x, center.y, center.z] {
                c.to_bits().hash(h);
            }
            weld.hash(h);
            weld_threshold.to_bits().hash(h);
        }
        DisplayMeshOp::SmoothGeometry { iterations, factor } => {
            1u8.hash(h);
            iterations.hash(h);
            factor.to_bits().hash(h);
        }
        DisplayMeshOp::SmoothNormals => 2u8.hash(h),
        DisplayMeshOp::Decimate { params } => {
            3u8.hash(h);
            params.clamped().hash_into(h);
        }
    }
}

/// Fold reusing `cache`'s valid prefix, then extending it.
///
/// Semantically identical to [`fold_display_mesh_with_ctx`] — gated by
/// `prefix_cache_matches_full_fold`, which compares the two across stacks that edit the first,
/// middle and last op. A cache that changes the result is worse than no cache.
pub fn fold_display_mesh_cached(
    ops: &[DisplayMeshOp],
    base: DisplayMesh,
    ctx: &DisplayMeshFoldCtx<'_>,
    cache: &mut DisplayMeshPrefixCache,
) -> DisplayMesh {
    let (start, mut mesh) = match cache.longest_valid_prefix(ops) {
        Some((n, m)) => (n, m.clone()),
        None => (0, base),
    };
    cache.truncate_from(start);
    for (i, op) in ops.iter().enumerate().skip(start) {
        mesh = apply_display_mesh_op_ctx(op, mesh, ctx);
        // Store only prefixes an edit is LIKELY to reuse — the last few ops. Every stored step
        // costs a full mesh clone (~0.18 ms at 40k tris), so caching all of them spent more on
        // clones than the reuse saved: measured 1.2x at depth 12 versus the 2.8x the precomputed
        // upper bound predicted. Caching only the tail keeps the reuse and drops most of the tax.
        //
        // `CACHE_TAIL_DEPTH` ops back from the end: an edit to the last op reuses depth-1, an edit
        // two from the end reuses depth-2, and so on. Edits further up re-fold, which is correct —
        // they invalidate that much of the stack anyway.
        if i + CACHE_TAIL_DEPTH >= ops.len() {
            cache.steps.push((i, DisplayMeshPrefixCache::prefix_hash(ops, i), mesh.clone()));
        }
    }
    mesh
}

pub fn fold_display_mesh(ops: &[DisplayMeshOp], mesh: DisplayMesh) -> DisplayMesh {
    fold_display_mesh_with_ctx(ops, mesh, &DisplayMeshFoldCtx::default())
}

/// Fold with optional body field (enables Field-mode Decimate curvature weights).
pub fn fold_display_mesh_with_ctx(
    ops: &[DisplayMeshOp],
    mut mesh: DisplayMesh,
    ctx: &DisplayMeshFoldCtx<'_>,
) -> DisplayMesh {
    for op in ops {
        mesh = apply_display_mesh_op_ctx(op, mesh, ctx);
    }
    mesh
}

pub fn apply_display_mesh_op(op: &DisplayMeshOp, mesh: DisplayMesh) -> DisplayMesh {
    apply_display_mesh_op_ctx(op, mesh, &DisplayMeshFoldCtx::default())
}

pub fn apply_display_mesh_op_ctx(
    op: &DisplayMeshOp,
    mesh: DisplayMesh,
    ctx: &DisplayMeshFoldCtx<'_>,
) -> DisplayMesh {
    match op {
        DisplayMeshOp::Mirror {
            axis_bits,
            center,
            weld,
            weld_threshold,
        } => mesh_mirror(mesh, *axis_bits, *center, *weld, *weld_threshold),
        DisplayMeshOp::SmoothGeometry {
            iterations,
            factor,
        } => mesh_smooth_laplacian(mesh, *iterations, *factor),
        DisplayMeshOp::SmoothNormals => mesh_recompute_normals(mesh),
        DisplayMeshOp::Decimate { params } => mesh_decimate(mesh, *params, ctx.field),
    }
}

