//! BVH (Bounding Volume Hierarchy) construction for SDF prim acceleration.
//!
//! Two APIs:
//! - `build_bvh()` â€” generic SAH BVH2 builder with multi-prim leaves.
//! - `build_body_bvh()` â€” per-body builder for GPU SDF traversal.
//!   Produces LEAF_BIT-encoded nodes (single-prim leaves in parent pointers).

use glam::Vec3;
use rayon::prelude::*;

/// BVH node â€” 32 bytes, GPU-ready layout.
///
/// For `build_bvh()` (generic SAH):
/// - `count == 0` â†’ internal node. `first` = left child index. Right child = `first + 1`.
/// - `count > 0`  â†’ leaf node. Prims at `prim_indices[first..first+count]`.
///
/// For `build_body_bvh()` (per-body GPU traversal):
/// - `first` = left child ref (`LEAF_BIT | body_local_index` for leaf, node index for internal).
/// - `count` = right child ref (same encoding).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BvhNode {
    pub aabb_min: [f32; 3],
    pub first: u32,
    pub aabb_max: [f32; 3],
    pub count: u32,
}

/// Axis-aligned bounding box.
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Aabb {
    /// An empty AABB that expands to fit the first union operand.
    pub fn empty() -> Self {
        Self { min: [f32::MAX; 3], max: [f32::MIN; 3] }
    }

    /// Return the smallest AABB enclosing both `self` and `other`.
    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb {
            min: [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            max: [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        }
    }

    /// Surface area of this AABB (used as SAH proxy).
    pub fn surface_area(&self) -> f32 {
        let dx = (self.max[0] - self.min[0]).max(0.0);
        let dy = (self.max[1] - self.min[1]).max(0.0);
        let dz = (self.max[2] - self.min[2]).max(0.0);
        2.0 * (dx * dy + dy * dz + dz * dx)
    }

    /// Center point of this AABB.
    pub fn centroid(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }
}

// =====================================================================
// Generic SAH BVH builder (multi-prim leaves)
// =====================================================================

const NUM_BINS: usize = 12;

/// Build a BVH from a list of `(prim_index, aabb)` pairs.
///
/// Returns `(nodes, prim_indices)` ready for GPU upload.
/// - `nodes[0]` is the root.
/// - Internal nodes: `count == 0`, children at `nodes[first]` and `nodes[first + 1]`.
/// - Leaf nodes: `count > 0`, prims at `prim_indices[first..first+count]`.
pub fn build_bvh(prims: &[(u32, Aabb)]) -> (Vec<BvhNode>, Vec<u32>) {
    if prims.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let n = prims.len();
    let mut order: Vec<usize> = (0..n).collect();

    let nodes = if n >= PAR_SUBTREE_MIN {
        // Large input (PT BLAS over a snapshot mesh): fork subtrees onto the
        // rayon pool. Same SAH splits, same node encoding â€” only the node
        // ORDERING differs from the sequential builder (both are valid:
        // internal `first` always points at an adjacent child pair).
        build_parallel(&mut order, prims, 0)
    } else {
        let mut nodes: Vec<BvhNode> = Vec::new();
        // Root placeholder at index 0
        nodes.push(BvhNode { aabb_min: [0.0; 3], first: 0, aabb_max: [0.0; 3], count: 0 });
        build_recursive(&mut nodes, &mut order, prims, 0, 0, n, 0, 0);
        nodes
    };

    let prim_indices: Vec<u32> = if n >= PAR_SUBTREE_MIN {
        order.par_iter().map(|&pos| prims[pos].0).collect()
    } else {
        order.iter().map(|&pos| prims[pos].0).collect()
    };

    (nodes, prim_indices)
}

/// Below this prim count a subtree builds sequentially (fork overhead beats
/// the win); at/above it `build_bvh` takes the parallel path.
const PAR_SUBTREE_MIN: usize = 8192;
/// Above this range length, SAH binning / AABB scans run as parallel reductions.
const PAR_SCAN_MIN: usize = 32768;
/// Hard recursion depth cap. SAH can produce 1-vs-(n-1) chains on coplanar DC
/// meshes â†’ O(n) stack (~8k frames) which overflowed rayon workers
/// (`thread '<unknown>' has overflowed its stack` after 512Â³ PT snaps).
/// Balanced trees stay ~logâ‚‚ n â‰ª this; at the cap we emit a multi-prim leaf.
const MAX_BVH_DEPTH: u32 = 48;

/// AABB over `prims[order[..]]` â€” parallel reduction for large ranges.
/// Union is exact min/max (associative + commutative), so the parallel
/// reduction is bit-identical to the sequential fold.
fn range_aabb(order: &[usize], prims: &[(u32, Aabb)]) -> Aabb {
    if order.len() >= PAR_SCAN_MIN {
        order
            .par_iter()
            .fold(Aabb::empty, |acc, &pos| acc.union(&prims[pos].1))
            .reduce(Aabb::empty, |a, b| a.union(&b))
    } else {
        let mut aabb = Aabb::empty();
        for &pos in order {
            aabb = aabb.union(&prims[pos].1);
        }
        aabb
    }
}

/// Parallel subtree builder. Returns a self-contained node array:
/// - local index 0 is the subtree root;
/// - internal `first` refs are LOCAL indices (rebased when spliced into the
///   parent's array);
/// - leaf `first` refs are GLOBAL positions into the final `order` array
///   (`base` + local offset), exactly like the sequential builder.
/// Layout on a parallel split: `[parent, l_root, r_root, l_rest.., r_rest..]`
/// so the children stay ADJACENT (`first`/`first + 1`), preserving the
/// traversal contract.
fn build_parallel(order: &mut [usize], prims: &[(u32, Aabb)], base: usize) -> Vec<BvhNode> {
    let count = order.len();

    if count < PAR_SUBTREE_MIN {
        // Sequential leaf-task: reuse the one true recursive builder.
        let mut nodes = vec![BvhNode { aabb_min: [0.0; 3], first: 0, aabb_max: [0.0; 3], count: 0 }];
        build_recursive(&mut nodes, order, prims, 0, 0, count, base, 0);
        return nodes;
    }

    let Some(mid) = find_best_split(order, prims, 0, count) else {
        // Degenerate (all centroids coincide) â€” one big leaf, same as sequential.
        let aabb = range_aabb(order, prims);
        return vec![BvhNode { aabb_min: aabb.min, first: base as u32, aabb_max: aabb.max, count: count as u32 }];
    };

    let (lo, ro) = order.split_at_mut(mid);
    let (l, r) = rayon::join(
        || build_parallel(lo, prims, base),
        || build_parallel(ro, prims, base + mid),
    );

    // Parent AABB = union of the child roots (exact â€” children partition the range).
    let l_aabb = Aabb { min: l[0].aabb_min, max: l[0].aabb_max };
    let r_aabb = Aabb { min: r[0].aabb_min, max: r[0].aabb_max };
    let aabb = l_aabb.union(&r_aabb);

    let ll = l.len() as u32;
    // Splice: parent at 0, child roots at 1 and 2 (adjacent), bodies after.
    // Left local j (j â‰¥ 1) lands at global j + 2; right local j at ll + 1 + j.
    let off_l = 2u32;
    let off_r = ll + 1;
    let rebase = |n: &BvhNode, off: u32| {
        let mut n = *n;
        if n.count == 0 {
            n.first += off;
        }
        n
    };
    let mut nodes = Vec::with_capacity(1 + l.len() + r.len());
    nodes.push(BvhNode { aabb_min: aabb.min, first: 1, aabb_max: aabb.max, count: 0 });
    nodes.push(rebase(&l[0], off_l));
    nodes.push(rebase(&r[0], off_r));
    nodes.extend(l[1..].iter().map(|n| rebase(n, off_l)));
    nodes.extend(r[1..].iter().map(|n| rebase(n, off_r)));
    nodes
}

/// `base`: offset of `order[0]` within the FINAL global order array â€” leaves
/// store `base + start` so a subtree built on a sub-slice (parallel path)
/// still references the right `prim_indices` range. Top-level calls pass 0.
/// `depth`: recursion depth (0 at root) â€” caps stack on degenerate SAH splits.
fn build_recursive(
    nodes: &mut Vec<BvhNode>,
    order: &mut [usize],
    prims: &[(u32, Aabb)],
    node_idx: usize,
    start: usize,
    end: usize,
    base: usize,
    depth: u32,
) {
    let count = end - start;

    let mut node_aabb = Aabb::empty();
    for &pos in &order[start..end] {
        node_aabb = node_aabb.union(&prims[pos].1);
    }

    // Single prim â†’ leaf (original contract).
    if count == 1 {
        nodes[node_idx] = BvhNode {
            aabb_min: node_aabb.min,
            first: (base + start) as u32,
            aabb_max: node_aabb.max,
            count: 1,
        };
        return;
    }

    // Depth cap: multi-prim leaf instead of deeper recursion (stack safety).
    if depth >= MAX_BVH_DEPTH {
        nodes[node_idx] = BvhNode {
            aabb_min: node_aabb.min,
            first: (base + start) as u32,
            aabb_max: node_aabb.max,
            count: count as u32,
        };
        return;
    }

    // SAH split. Reject 1-vs-(n-1) chains (stack blow on large PT BLAS) â†’ object median.
    // Degenerate SAH (None) keeps multi-prim leaf like before (overlapping centroids).
    let mid = match find_best_split(order, prims, start, end) {
        Some(m) if m > start + 1 && m < end - 1 => m,
        Some(m) if m > start && m < end => {
            // Extreme 1-vs-(n-1) SAH: rebalance to object median (bounded depth).
            let _ = m;
            let mid = start + count / 2;
            median_partition(order, prims, start, end, mid);
            mid
        }
        _ => {
            // No beneficial split â€” multi-prim leaf (original behaviour).
            nodes[node_idx] = BvhNode {
                aabb_min: node_aabb.min,
                first: (base + start) as u32,
                aabb_max: node_aabb.max,
                count: count as u32,
            };
            return;
        }
    };

    if mid <= start || mid >= end {
        nodes[node_idx] = BvhNode {
            aabb_min: node_aabb.min,
            first: (base + start) as u32,
            aabb_max: node_aabb.max,
            count: count as u32,
        };
        return;
    }

    let left_idx = nodes.len();
    let zero = BvhNode {
        aabb_min: [0.0; 3],
        first: 0,
        aabb_max: [0.0; 3],
        count: 0,
    };
    nodes.push(zero); // left child
    nodes.push(zero); // right child (= left_idx + 1)

    nodes[node_idx] = BvhNode {
        aabb_min: node_aabb.min,
        first: left_idx as u32,
        aabb_max: node_aabb.max,
        count: 0, // internal
    };

    build_recursive(nodes, order, prims, left_idx, start, mid, base, depth + 1);
    build_recursive(nodes, order, prims, left_idx + 1, mid, end, base, depth + 1);
}

/// Partition `order[start..end]` so that the first `mid - start` entries have the
/// smallest centroid on the longest AABB axis (approx. object median).
fn median_partition(
    order: &mut [usize],
    prims: &[(u32, Aabb)],
    start: usize,
    end: usize,
    mid: usize,
) {
    if mid <= start || mid >= end {
        return;
    }
    // Longest centroid extent axis.
    let mut cmin = [f32::MAX; 3];
    let mut cmax = [f32::MIN; 3];
    for &pos in &order[start..end] {
        let c = prims[pos].1.centroid();
        for a in 0..3 {
            cmin[a] = cmin[a].min(c[a]);
            cmax[a] = cmax[a].max(c[a]);
        }
    }
    let mut axis = 0usize;
    let mut best_ext = -1.0f32;
    for a in 0..3 {
        let e = cmax[a] - cmin[a];
        if e > best_ext {
            best_ext = e;
            axis = a;
        }
    }
    let k = mid - start;
    order[start..end].select_nth_unstable_by(k, |&a, &b| {
        let ca = prims[a].1.centroid()[axis];
        let cb = prims[b].1.centroid()[axis];
        ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Find the best SAH split for `order[start..end]`.
/// Partitions `order[start..end]` in-place and returns the split position,
/// or `None` if no valid split exists.
fn find_best_split(
    order: &mut [usize],
    prims: &[(u32, Aabb)],
    start: usize,
    end: usize,
) -> Option<usize> {
    let range = &order[start..end];
    let (centroid_min, centroid_max) = if range.len() >= PAR_SCAN_MIN {
        // Parallel min/max reduction â€” exact (min/max are associative+commutative).
        range
            .par_iter()
            .fold(
                || ([f32::MAX; 3], [f32::MIN; 3]),
                |mut acc, &pos| {
                    let c = prims[pos].1.centroid();
                    for a in 0..3 {
                        acc.0[a] = acc.0[a].min(c[a]);
                        acc.1[a] = acc.1[a].max(c[a]);
                    }
                    acc
                },
            )
            .reduce(
                || ([f32::MAX; 3], [f32::MIN; 3]),
                |mut a, b| {
                    for k in 0..3 {
                        a.0[k] = a.0[k].min(b.0[k]);
                        a.1[k] = a.1[k].max(b.1[k]);
                    }
                    a
                },
            )
    } else {
        let mut centroid_min = [f32::MAX; 3];
        let mut centroid_max = [f32::MIN; 3];
        for &pos in range {
            let c = prims[pos].1.centroid();
            for a in 0..3 {
                centroid_min[a] = centroid_min[a].min(c[a]);
                centroid_max[a] = centroid_max[a].max(c[a]);
            }
        }
        (centroid_min, centroid_max)
    };

    // Bin all 3 axes in ONE pass over the range (parallel fold for large
    // ranges). Bin assignment is deterministic per prim; per-bin count/AABB
    // accumulation is order-independent, so the parallel reduction produces
    // the same bins as the sequential loop.
    let inv_extents = [0usize, 1, 2].map(|a| {
        let extent = centroid_max[a] - centroid_min[a];
        if extent < 1e-6 { 0.0 } else { NUM_BINS as f32 / extent }
    });
    let bin_prim = |bins: &mut [[BinInfo; NUM_BINS]; 3], pos: usize| {
        let p = &prims[pos];
        let c = p.1.centroid();
        for axis in 0..3 {
            if inv_extents[axis] == 0.0 { continue; }
            let bin = (((c[axis] - centroid_min[axis]) * inv_extents[axis]) as usize).min(NUM_BINS - 1);
            bins[axis][bin].count += 1;
            bins[axis][bin].aabb = bins[axis][bin].aabb.union(&p.1);
        }
    };
    let merge_bins = |mut a: [[BinInfo; NUM_BINS]; 3], b: [[BinInfo; NUM_BINS]; 3]| {
        for axis in 0..3 {
            for i in 0..NUM_BINS {
                a[axis][i].count += b[axis][i].count;
                a[axis][i].aabb = a[axis][i].aabb.union(&b[axis][i].aabb);
            }
        }
        a
    };
    let all_bins: [[BinInfo; NUM_BINS]; 3] = if range.len() >= PAR_SCAN_MIN {
        range
            .par_iter()
            .fold(
                || [[BinInfo::default(); NUM_BINS]; 3],
                |mut acc, &pos| {
                    bin_prim(&mut acc, pos);
                    acc
                },
            )
            .reduce(|| [[BinInfo::default(); NUM_BINS]; 3], merge_bins)
    } else {
        let mut acc = [[BinInfo::default(); NUM_BINS]; 3];
        for &pos in range {
            bin_prim(&mut acc, pos);
        }
        acc
    };

    let mut best_cost = f32::MAX;
    let mut best_axis = 0usize;
    let mut best_split_bin = 0usize;
    let mut found = false;

    for axis in 0..3 {
        if inv_extents[axis] == 0.0 { continue; }
        let bins = &all_bins[axis];

        let mut left_area = [0.0f32; NUM_BINS - 1];
        let mut left_count = [0u32; NUM_BINS - 1];
        let mut right_area = [0.0f32; NUM_BINS - 1];
        let mut right_count = [0u32; NUM_BINS - 1];

        {
            let mut running = Aabb::empty();
            let mut cnt = 0u32;
            for i in 0..(NUM_BINS - 1) {
                running = running.union(&bins[i].aabb);
                cnt += bins[i].count;
                left_area[i] = running.surface_area();
                left_count[i] = cnt;
            }
        }
        {
            let mut running = Aabb::empty();
            let mut cnt = 0u32;
            for i in (0..(NUM_BINS - 1)).rev() {
                running = running.union(&bins[i + 1].aabb);
                cnt += bins[i + 1].count;
                right_area[i] = running.surface_area();
                right_count[i] = cnt;
            }
        }

        for i in 0..(NUM_BINS - 1) {
            if left_count[i] == 0 || right_count[i] == 0 { continue; }
            let cost = left_area[i] * left_count[i] as f32
                     + right_area[i] * right_count[i] as f32;
            if cost < best_cost {
                best_cost = cost;
                best_axis = axis;
                best_split_bin = i;
                found = true;
            }
        }
    }

    if !found { return None; }

    let extent = centroid_max[best_axis] - centroid_min[best_axis];
    let inv_extent = NUM_BINS as f32 / extent;

    let slice = &mut order[start..end];
    let mut left = 0usize;
    for i in 0..slice.len() {
        let c = prims[slice[i]].1.centroid();
        let bin = ((c[best_axis] - centroid_min[best_axis]) * inv_extent) as usize;
        let bin = bin.min(NUM_BINS - 1);
        if bin <= best_split_bin {
            slice.swap(left, i);
            left += 1;
        }
    }

    let mid = start + left;
    if mid == start || mid == end {
        return None;
    }
    Some(mid)
}

#[derive(Clone, Copy)]
struct BinInfo {
    aabb: Aabb,
    count: u32,
}

impl Default for BinInfo {
    fn default() -> Self {
        Self { aabb: Aabb::empty(), count: 0 }
    }
}

// =====================================================================
// Scene-level BVH builder (body-level acceleration)
// =====================================================================

/// Input item for scene-level BVH: one top-level body.
pub struct SceneBvhBody {
    pub aabb: Aabb,
    /// First entry index in the combined prim buffer (SCOPE_OPEN entry).
    pub start_entry: u32,
    /// Number of entries in this body's range (SCOPE_OPEN + prims + children + SCOPE_CLOSE).
    pub entry_count: u32,
}

/// Build a BVH over top-level bodies for scene-level acceleration.
///
/// Each leaf contains exactly one body: `first = start_entry`, `count = entry_count`.
/// Internal nodes: `count = 0`, `first = left_child_index` (right = first + 1).
/// Uses median split on the longest axis (simple, not SAH).
pub fn build_scene_bvh(bodies: &[SceneBvhBody]) -> Vec<BvhNode> {
    if bodies.is_empty() {
        return Vec::new();
    }
    if bodies.len() == 1 {
        return vec![BvhNode {
            aabb_min: bodies[0].aabb.min,
            first: bodies[0].start_entry,
            aabb_max: bodies[0].aabb.max,
            count: bodies[0].entry_count,
        }];
    }

    let mut order: Vec<usize> = (0..bodies.len()).collect();
    let mut nodes = Vec::new();
    // Root placeholder
    nodes.push(BvhNode { aabb_min: [0.0; 3], first: 0, aabb_max: [0.0; 3], count: 0 });
    build_scene_recursive(&mut nodes, &mut order, bodies, 0, 0, bodies.len());
    nodes
}

fn build_scene_recursive(
    nodes: &mut Vec<BvhNode>,
    order: &mut [usize],
    bodies: &[SceneBvhBody],
    node_idx: usize,
    start: usize,
    end: usize,
) {
    let count = end - start;

    // Compute AABB for this subtree
    let mut aabb = Aabb::empty();
    for &pos in &order[start..end] {
        aabb = aabb.union(&bodies[pos].aabb);
    }

    // Single body â†’ leaf
    if count == 1 {
        let b = &bodies[order[start]];
        nodes[node_idx] = BvhNode {
            aabb_min: aabb.min,
            first: b.start_entry,
            aabb_max: aabb.max,
            count: b.entry_count,
        };
        return;
    }

    // Find longest axis
    let dx = aabb.max[0] - aabb.min[0];
    let dy = aabb.max[1] - aabb.min[1];
    let dz = aabb.max[2] - aabb.min[2];
    let axis = if dx >= dy && dx >= dz { 0 } else if dy >= dz { 1 } else { 2 };

    // Sort by centroid on longest axis, split at median
    order[start..end].sort_by(|&a, &b| {
        let ca = bodies[a].aabb.centroid()[axis];
        let cb = bodies[b].aabb.centroid()[axis];
        ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mid = start + count / 2;

    let left_idx = nodes.len();
    let zero = BvhNode { aabb_min: [0.0; 3], first: 0, aabb_max: [0.0; 3], count: 0 };
    nodes.push(zero); // left child
    nodes.push(zero); // right child = left_idx + 1

    nodes[node_idx] = BvhNode {
        aabb_min: aabb.min,
        first: left_idx as u32,
        aabb_max: aabb.max,
        count: 0, // internal
    };

    build_scene_recursive(nodes, order, bodies, left_idx, start, mid);
    build_scene_recursive(nodes, order, bodies, left_idx + 1, mid, end);
}

// =====================================================================
// Per-body BVH builder (LEAF_BIT format for GPU shader)
// =====================================================================

/// Leaf marker bit â€” MSB of child pointer.
const LEAF_BIT: u32 = 0x80000000;

/// Input prim for per-body BVH construction.
pub struct BvhPrim {
    pub center: Vec3,
    pub radius: f32,
    pub body_local_index: u32,
    pub op: u32,
    pub blend_k: f32,
}

/// Result of building a per-body BVH.
pub struct BodyBvh {
    pub nodes: Vec<BvhNode>,
    /// Max blend radius `k` across all prims. `f32::MAX` if any prim uses Intersect (op=2).
    pub body_max_k: f32,
}

/// Build a BVH for a single body's direct prims.
///
/// Returns `None` if `prim_count <= threshold` (caller should use linear fallback).
/// Leaf nodes are encoded as `LEAF_BIT | body_local_index` in parent child pointers.
/// Uses SAH (12-bin) for split quality.
pub fn build_body_bvh(prims: &[BvhPrim], threshold: usize) -> Option<BodyBvh> {
    if prims.len() <= threshold || prims.is_empty() {
        return None;
    }
    if prims.len() > 512 {
        return None;
    }

    let has_intersect = prims.iter().any(|p| p.op == 2);
    let body_max_k = if has_intersect {
        f32::MAX
    } else {
        prims.iter().map(|p| p.blend_k).fold(0.0f32, f32::max)
    };

    // Convert to (body_local_index, Aabb) for SAH infrastructure
    let aabb_prims: Vec<(u32, Aabb)> = prims.iter().map(|p| {
        let r = p.radius;
        (p.body_local_index, Aabb {
            min: [p.center.x - r, p.center.y - r, p.center.z - r],
            max: [p.center.x + r, p.center.y + r, p.center.z + r],
        })
    }).collect();

    if prims.len() == 1 {
        let ab = &aabb_prims[0];
        return Some(BodyBvh {
            nodes: vec![BvhNode {
                aabb_min: ab.1.min,
                first: LEAF_BIT | ab.0,
                aabb_max: ab.1.max,
                count: LEAF_BIT | ab.0,
            }],
            body_max_k,
        });
    }

    let mut order: Vec<usize> = (0..prims.len()).collect();
    let mut nodes: Vec<BvhNode> = Vec::new();

    build_body_node(&mut nodes, &mut order, &aabb_prims, 0, prims.len());

    Some(BodyBvh { nodes, body_max_k })
}

/// Recursively build a LEAF_BIT node. Returns a child reference for the parent:
/// - Single prim â†’ `LEAF_BIT | body_local_index` (no node allocated).
/// - Multiple prims â†’ node index in `nodes` array.
fn build_body_node(
    nodes: &mut Vec<BvhNode>,
    order: &mut [usize],
    aabb_prims: &[(u32, Aabb)],
    start: usize,
    end: usize,
) -> u32 {
    let count = end - start;

    if count == 1 {
        return LEAF_BIT | aabb_prims[order[start]].0;
    }

    // Allocate node placeholder
    let node_idx = nodes.len();
    nodes.push(BvhNode { aabb_min: [0.0; 3], first: 0, aabb_max: [0.0; 3], count: 0 });

    // Compute AABB for this subtree
    let mut node_aabb = Aabb::empty();
    for &pos in &order[start..end] {
        node_aabb = node_aabb.union(&aabb_prims[pos].1);
    }

    // Find SAH split, falling back to midpoint for degenerate cases
    let mid = match find_best_split(order, aabb_prims, start, end) {
        Some(m) => m,
        None => {
            // Degenerate â€” all same centroid. Split arbitrarily.
            let m = start + count / 2;
            m.max(start + 1).min(end - 1)
        }
    };

    let left_ref = build_body_node(nodes, order, aabb_prims, start, mid);
    let right_ref = build_body_node(nodes, order, aabb_prims, mid, end);

    nodes[node_idx] = BvhNode {
        aabb_min: node_aabb.min,
        first: left_ref,
        aabb_max: node_aabb.max,
        count: right_ref,
    };

    node_idx as u32
}

// =====================================================================
// Triangle BLAS builder (Phase 2 path tracer)
// =====================================================================
//
// The path tracer traces the SAME triangles the raster pass draws (DC mesh /
// poly / voxel / imported â€” one `RasterVertex` format). Each body gets a
// bottom-level acceleration structure (BLAS) over its triangles, built here on
// the CPU and later uploaded as an SSBO for WGSL traversal. Reuses the generic
// SAH `build_bvh` core â€” a triangle is just a prim whose AABB encloses its 3
// corners, so no new tree-building logic is needed.

/// A bottom-level acceleration structure over one body's triangles.
///
/// Node encoding is identical to `build_bvh`:
/// - `nodes[0]` is the root.
/// - Leaf: `count > 0` â†’ triangles at `tri_order[first..first + count]`.
/// - Internal: `count == 0` â†’ children `nodes[first]` and `nodes[first + 1]`.
///
/// `tri_order[k]` is a triangle index: triangle `t` uses vertices
/// `indices[t*3 .. t*3+3]` from the mesh this BLAS was built from.
#[derive(Clone, Debug)]
pub struct TriBlas {
    pub nodes: Vec<BvhNode>,
    pub tri_order: Vec<u32>,
}

impl TriBlas {
    /// Number of triangles indexed by this BLAS (leaves cover exactly these).
    pub fn triangle_count(&self) -> usize {
        self.tri_order.len()
    }

    /// True when the source mesh had no valid triangles (empty tree).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// MÃ¶llerâ€“Trumbore ray/triangle intersection. Returns the ray parameter `t` of a front-OR-back
/// hit with `t > eps`, else `None`.
///
/// Two-sided on purpose: an occlusion query must not see through a back-facing wall, and a mesh
/// map is baked on whatever the modelled surface is, not on an assumed winding.
///
/// (A private copy lives in this module's tests as an independent brute-force oracle â€” the tests
/// verify the TREE against an exhaustive scan, so sharing this function there would let one bug
/// hide the other.)
pub fn ray_triangle(orig: [f32; 3], dir: [f32; 3], a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<f32> {
    const EPS: f32 = 1.0e-7;
    let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let p = [
        dir[1] * e2[2] - dir[2] * e2[1],
        dir[2] * e2[0] - dir[0] * e2[2],
        dir[0] * e2[1] - dir[1] * e2[0],
    ];
    let det = e1[0] * p[0] + e1[1] * p[1] + e1[2] * p[2];
    if det.abs() < EPS {
        return None; // ray parallel to the triangle plane
    }
    let inv_det = 1.0 / det;
    let t_vec = [orig[0] - a[0], orig[1] - a[1], orig[2] - a[2]];
    let u = (t_vec[0] * p[0] + t_vec[1] * p[1] + t_vec[2] * p[2]) * inv_det;
    if !(-EPS..=1.0 + EPS).contains(&u) {
        return None;
    }
    let q = [
        t_vec[1] * e1[2] - t_vec[2] * e1[1],
        t_vec[2] * e1[0] - t_vec[0] * e1[2],
        t_vec[0] * e1[1] - t_vec[1] * e1[0],
    ];
    let v = (dir[0] * q[0] + dir[1] * q[1] + dir[2] * q[2]) * inv_det;
    if v < -EPS || u + v > 1.0 + EPS {
        return None;
    }
    let t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) * inv_det;
    if t > EPS {
        Some(t)
    } else {
        None
    }
}

/// Slab test: does the ray reach `node`'s box within `t_max`?
fn ray_aabb_slab(orig: [f32; 3], inv_dir: [f32; 3], node: &BvhNode, t_max: f32) -> bool {
    let mut t0 = 0.0f32;
    let mut t1 = t_max;
    for k in 0..3 {
        let a = (node.aabb_min[k] - orig[k]) * inv_dir[k];
        let b = (node.aabb_max[k] - orig[k]) * inv_dir[k];
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        t0 = t0.max(lo);
        t1 = t1.min(hi);
        if t1 < t0 {
            return false;
        }
    }
    true
}

/// **Any-hit occlusion query** against a [`TriBlas`] â€” does anything block `orig â†’ dir` within
/// `t_max`?
///
/// Any-hit, not nearest-hit: ambient occlusion and thickness only ask *whether* the ray was
/// stopped, so the traversal returns on the first intersection instead of sorting for the closest.
///
/// `positions`/`indices` must be the SAME arrays the BLAS was built from â€” `tri_order` holds
/// indices into them, so a mismatched pair reads the wrong triangles (and, with fewer vertices,
/// would panic). Vertices are used in whatever space the caller built them in.
pub fn blas_any_hit(
    blas: &TriBlas,
    positions: &[[f32; 3]],
    indices: &[u32],
    orig: [f32; 3],
    dir: [f32; 3],
    t_max: f32,
) -> bool {
    if blas.is_empty() || t_max <= 0.0 {
        return false;
    }
    // A zero component yields Â±inf here, which the slab test handles correctly (the interval
    // simply never closes on that axis) â€” no branch needed, and no NaN as long as orig is finite.
    let inv_dir = [1.0 / dir[0], 1.0 / dir[1], 1.0 / dir[2]];
    let mut stack: [u32; 64] = [0; 64];
    let mut sp = 1usize;
    stack[0] = 0;
    while sp > 0 {
        sp -= 1;
        let node = &blas.nodes[stack[sp] as usize];
        if !ray_aabb_slab(orig, inv_dir, node, t_max) {
            continue;
        }
        if node.count > 0 {
            for k in 0..node.count {
                let t = blas.tri_order[(node.first + k) as usize] as usize;
                let (i0, i1, i2) = (
                    indices[t * 3] as usize,
                    indices[t * 3 + 1] as usize,
                    indices[t * 3 + 2] as usize,
                );
                if let Some(hit) =
                    ray_triangle(orig, dir, positions[i0], positions[i1], positions[i2])
                {
                    if hit < t_max {
                        return true;
                    }
                }
            }
        } else if sp + 2 <= stack.len() {
            stack[sp] = node.first;
            stack[sp + 1] = node.first + 1;
            sp += 2;
        }
    }
    false
}

/// AABB enclosing a single triangle's three corners.
fn tri_aabb(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Aabb {
    Aabb {
        min: [
            a[0].min(b[0]).min(c[0]),
            a[1].min(b[1]).min(c[1]),
            a[2].min(b[2]).min(c[2]),
        ],
        max: [
            a[0].max(b[0]).max(c[0]),
            a[1].max(b[1]).max(c[1]),
            a[2].max(b[2]).max(c[2]),
        ],
    }
}

/// Build a triangle BLAS from a body-local vertex/index mesh.
///
/// `positions[i]` is vertex `i`; `indices` is a flat list of triangle corners
/// (length must be a multiple of 3 â€” a trailing partial triangle is ignored).
/// Triangles with an out-of-range corner index are skipped (a degenerate mesh
/// can never produce an out-of-bounds vertex fetch in the shader).
///
/// Vertices are consumed in whatever space the caller provides â€” the PT pipeline
/// keeps them BODY-LOCAL (the ray is transformed into body space at the TLAS
/// leaf), so a body's BLAS never rebuilds on move/rotate/scale.
pub fn build_tri_blas(positions: &[[f32; 3]], indices: &[u32]) -> TriBlas {
    let tri_count = indices.len() / 3;
    let make = |t: usize| -> Option<(u32, Aabb)> {
        let i0 = indices[t * 3] as usize;
        let i1 = indices[t * 3 + 1] as usize;
        let i2 = indices[t * 3 + 2] as usize;
        if i0 >= positions.len() || i1 >= positions.len() || i2 >= positions.len() {
            return None; // out-of-range corner â€” skip so the shader never fetches OOB
        }
        Some((t as u32, tri_aabb(positions[i0], positions[i1], positions[i2])))
    };
    // rayon `collect` preserves iteration order, so the parallel path yields
    // the same prim list (same triangle order) as the sequential loop.
    let prims: Vec<(u32, Aabb)> = if tri_count >= PAR_SCAN_MIN {
        (0..tri_count).into_par_iter().filter_map(make).collect()
    } else {
        (0..tri_count).filter_map(make).collect()
    };
    let (nodes, tri_order) = build_bvh(&prims);
    TriBlas { nodes, tri_order }
}


/// **Nearest-hit** query against a [`TriBlas`]. Returns `(t, triangle_index)` of the closest
/// intersection with `0 < t < t_max`, or `None`.
///
/// Used by high-to-low mesh-map projection (front/rear cage rays). Same tree as [`blas_any_hit`];
/// this one keeps walking for the closest `t` instead of returning on the first hit.
pub fn blas_nearest_hit(
    blas: &TriBlas,
    positions: &[[f32; 3]],
    indices: &[u32],
    orig: [f32; 3],
    dir: [f32; 3],
    t_max: f32,
) -> Option<(f32, u32)> {
    if blas.is_empty() || t_max <= 0.0 {
        return None;
    }
    let inv_dir = [1.0 / dir[0], 1.0 / dir[1], 1.0 / dir[2]];
    let mut best: Option<(f32, u32)> = None;
    let mut stack: [u32; 64] = [0; 64];
    let mut sp = 1usize;
    stack[0] = 0;
    while sp > 0 {
        sp -= 1;
        let node = &blas.nodes[stack[sp] as usize];
        let clip = best.map_or(t_max, |(bt, _)| bt.min(t_max));
        if !ray_aabb_slab(orig, inv_dir, node, clip) {
            continue;
        }
        if node.count > 0 {
            for k in 0..node.count {
                let t = blas.tri_order[(node.first + k) as usize] as usize;
                let (i0, i1, i2) = (
                    indices[t * 3] as usize,
                    indices[t * 3 + 1] as usize,
                    indices[t * 3 + 2] as usize,
                );
                if let Some(hit) =
                    ray_triangle(orig, dir, positions[i0], positions[i1], positions[i2])
                {
                    if hit < clip {
                        best = Some((hit, t as u32));
                    }
                }
            }
        } else if sp + 2 <= stack.len() {
            stack[sp] = node.first;
            stack[sp + 1] = node.first + 1;
            sp += 2;
        }
    }
    best
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- Generic build_bvh tests ---

    fn aabb_at(cx: f32, cy: f32, cz: f32, r: f32) -> Aabb {
        Aabb {
            min: [cx - r, cy - r, cz - r],
            max: [cx + r, cy + r, cz + r],
        }
    }

    #[test]
    fn test_empty() {
        let (nodes, indices) = build_bvh(&[]);
        assert!(nodes.is_empty());
        assert!(indices.is_empty());
    }

    #[test]
    fn test_single() {
        let prims = vec![(42u32, aabb_at(1.0, 2.0, 3.0, 0.5))];
        let (nodes, indices) = build_bvh(&prims);
        assert_eq!(nodes.len(), 1);
        assert_eq!(indices.len(), 1);
        assert_eq!(indices[0], 42);
        assert!(nodes[0].count > 0);
        assert_eq!(nodes[0].count, 1);
    }

    #[test]
    fn test_two() {
        let prims = vec![
            (0u32, aabb_at(-5.0, 0.0, 0.0, 0.5)),
            (1u32, aabb_at(5.0, 0.0, 0.0, 0.5)),
        ];
        let (nodes, indices) = build_bvh(&prims);
        assert_eq!(nodes.len(), 3);
        assert_eq!(indices.len(), 2);
        assert_eq!(nodes[0].count, 0);
        let l = nodes[0].first as usize;
        assert!(nodes[l].count > 0);
        assert!(nodes[l + 1].count > 0);
    }

    #[test]
    fn test_ten_scattered() {
        let prims: Vec<(u32, Aabb)> = (0..10).map(|i| {
            let f = i as f32;
            (i as u32, aabb_at(f * 3.0, (f * 7.0) % 5.0, (f * 11.0) % 8.0, 0.5))
        }).collect();
        let (nodes, indices) = build_bvh(&prims);
        assert!(!nodes.is_empty());
        assert_eq!(indices.len(), 10);
        for (i, node) in nodes.iter().enumerate() {
            if node.count == 0 {
                let l = node.first as usize;
                assert!(l < nodes.len(), "left child OOB at node {}", i);
                assert!(l + 1 < nodes.len(), "right child OOB at node {}", i);
            }
        }
    }

    #[test]
    fn test_overlapping() {
        let prims: Vec<(u32, Aabb)> = (0..10).map(|i| {
            (i as u32, aabb_at(0.0, 0.0, 0.0, 1.0))
        }).collect();
        let (nodes, indices) = build_bvh(&prims);
        assert_eq!(nodes.len(), 1, "overlapping prims should produce a single leaf");
        assert_eq!(nodes[0].count, 10);
        assert_eq!(indices.len(), 10);
    }

    #[test]
    fn test_node_count() {
        for n in [1, 2, 3, 5, 8, 13, 20, 50, 100] {
            let prims: Vec<(u32, Aabb)> = (0..n).map(|i| {
                let f = i as f32;
                (i as u32, aabb_at(f * 2.0, f.sin() * 10.0, f.cos() * 10.0, 0.5))
            }).collect();
            let (nodes, _) = build_bvh(&prims);
            assert!(
                nodes.len() <= 2 * n - 1,
                "n={}: {} nodes exceeds 2N-1={}", n, nodes.len(), 2 * n - 1
            );
        }
    }

    #[test]
    fn test_all_prims_referenced() {
        let prims: Vec<(u32, Aabb)> = (0..20).map(|i| {
            let f = i as f32;
            (100 + i as u32, aabb_at(f * 2.0, f.sin() * 5.0, f.cos() * 5.0, 0.5))
        }).collect();
        let (nodes, indices) = build_bvh(&prims);
        assert_eq!(indices.len(), 20);

        let mut sorted = indices.clone();
        sorted.sort();
        let expected: Vec<u32> = (100..120).collect();
        assert_eq!(sorted, expected);

        for node in &nodes {
            if node.count > 0 {
                let s = node.first as usize;
                let e = s + node.count as usize;
                assert!(e <= indices.len(), "leaf range exceeds prim_indices");
            }
        }
    }

    #[test]
    fn test_aabb_containment_generic() {
        let prims: Vec<(u32, Aabb)> = vec![
            (0, aabb_at(-5.0, 0.0, 0.0, 1.0)),
            (1, aabb_at(5.0, 0.0, 0.0, 1.0)),
            (2, aabb_at(0.0, 5.0, 0.0, 1.0)),
            (3, aabb_at(0.0, -5.0, 0.0, 1.0)),
            (4, aabb_at(0.0, 0.0, 5.0, 1.0)),
            (5, aabb_at(0.0, 0.0, -5.0, 1.0)),
            (6, aabb_at(3.0, 3.0, 3.0, 1.0)),
            (7, aabb_at(-3.0, -3.0, -3.0, 1.0)),
        ];
        let (nodes, _) = build_bvh(&prims);

        for node in &nodes {
            if node.count == 0 {
                let l = node.first as usize;
                let child_l = &nodes[l];
                let child_r = &nodes[l + 1];
                for a in 0..3 {
                    assert!(node.aabb_min[a] <= child_l.aabb_min[a] + 1e-6);
                    assert!(node.aabb_max[a] >= child_l.aabb_max[a] - 1e-6);
                    assert!(node.aabb_min[a] <= child_r.aabb_min[a] + 1e-6);
                    assert!(node.aabb_max[a] >= child_r.aabb_max[a] - 1e-6);
                }
            }
        }
    }

    #[test]
    fn test_aabb_methods() {
        let a = Aabb { min: [0.0, 0.0, 0.0], max: [2.0, 3.0, 4.0] };
        let b = Aabb { min: [1.0, -1.0, 2.0], max: [5.0, 2.0, 6.0] };
        let u = a.union(&b);
        assert_eq!(u.min, [0.0, -1.0, 0.0]);
        assert_eq!(u.max, [5.0, 3.0, 6.0]);
        assert!((a.surface_area() - 52.0).abs() < 1e-4);
        let c = a.centroid();
        assert!((c[0] - 1.0).abs() < 1e-6);
        assert!((c[1] - 1.5).abs() < 1e-6);
        assert!((c[2] - 2.0).abs() < 1e-6);
        let e = Aabb::empty();
        let eu = e.union(&a);
        assert_eq!(eu.min, a.min);
        assert_eq!(eu.max, a.max);
        assert_eq!(e.surface_area(), 0.0);
    }

    // --- Per-body build_body_bvh tests ---

    #[test]
    fn test_below_threshold_returns_none() {
        let prims: Vec<BvhPrim> = (0..5).map(|i| BvhPrim {
            center: Vec3::new(i as f32, 0.0, 0.0),
            radius: 0.5,
            body_local_index: i,
            op: 0,
            blend_k: 0.0,
        }).collect();
        assert!(build_body_bvh(&prims, 8).is_none());
    }

    #[test]
    fn test_single_prim_above_threshold() {
        let prims = vec![BvhPrim {
            center: Vec3::new(1.0, 2.0, 3.0),
            radius: 0.5,
            body_local_index: 0,
            op: 0,
            blend_k: 0.1,
        }];
        let bvh = build_body_bvh(&prims, 0).unwrap();
        assert_eq!(bvh.nodes.len(), 1);
        assert_eq!(bvh.body_max_k, 0.1);
        assert_eq!(bvh.nodes[0].first, LEAF_BIT | 0);
        assert_eq!(bvh.nodes[0].count, LEAF_BIT | 0);
    }

    #[test]
    fn test_basic_bvh_structure() {
        let prims: Vec<BvhPrim> = (0..10).map(|i| BvhPrim {
            center: Vec3::new(i as f32 * 2.0, 0.0, 0.0),
            radius: 0.5,
            body_local_index: i,
            op: 0,
            blend_k: 0.05,
        }).collect();
        let bvh = build_body_bvh(&prims, 4).unwrap();

        assert!(!bvh.nodes.is_empty());
        assert!(bvh.nodes.len() <= 9); // At most N-1 internal nodes
        assert_eq!(bvh.body_max_k, 0.05);

        let root = &bvh.nodes[0];
        assert!(root.aabb_min[0] <= 0.0 - 0.5);
        assert!(root.aabb_max[0] >= 18.0 + 0.5);
    }

    #[test]
    fn test_leaf_encoding() {
        let prims: Vec<BvhPrim> = (0..10).map(|i| BvhPrim {
            center: Vec3::new(i as f32, 0.0, 0.0),
            radius: 0.5,
            body_local_index: i,
            op: 0,
            blend_k: 0.0,
        }).collect();
        let bvh = build_body_bvh(&prims, 4).unwrap();

        let mut leaf_bits: Vec<u32> = Vec::new();
        for node in &bvh.nodes {
            if node.first & LEAF_BIT != 0 {
                leaf_bits.push(node.first & !LEAF_BIT);
            }
            if node.count & LEAF_BIT != 0 {
                leaf_bits.push(node.count & !LEAF_BIT);
            }
        }
        leaf_bits.sort();
        leaf_bits.dedup();

        for i in 0..10u32 {
            assert!(leaf_bits.contains(&i), "Missing leaf for prim {}", i);
        }
    }

    #[test]
    fn test_body_aabb_containment() {
        let prims: Vec<BvhPrim> = vec![
            BvhPrim { center: Vec3::new(-5.0, 0.0, 0.0), radius: 1.0, body_local_index: 0, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(5.0, 0.0, 0.0), radius: 1.0, body_local_index: 1, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(0.0, 5.0, 0.0), radius: 1.0, body_local_index: 2, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(0.0, -5.0, 0.0), radius: 1.0, body_local_index: 3, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(0.0, 0.0, 5.0), radius: 1.0, body_local_index: 4, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(0.0, 0.0, -5.0), radius: 1.0, body_local_index: 5, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(3.0, 3.0, 3.0), radius: 1.0, body_local_index: 6, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(-3.0, -3.0, -3.0), radius: 1.0, body_local_index: 7, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(1.0, 1.0, 1.0), radius: 2.0, body_local_index: 8, op: 0, blend_k: 0.0 },
        ];
        let bvh = build_body_bvh(&prims, 4).unwrap();

        for node in &bvh.nodes {
            fn check_child_contained(parent: &BvhNode, child_ref: u32, nodes: &[BvhNode], prims: &[BvhPrim]) {
                if child_ref & LEAF_BIT != 0 {
                    let bit_idx = (child_ref & !LEAF_BIT) as usize;
                    let p = &prims[bit_idx];
                    let r = p.radius;
                    assert!(parent.aabb_min[0] <= p.center.x - r + 1e-6);
                    assert!(parent.aabb_min[1] <= p.center.y - r + 1e-6);
                    assert!(parent.aabb_min[2] <= p.center.z - r + 1e-6);
                    assert!(parent.aabb_max[0] >= p.center.x + r - 1e-6);
                    assert!(parent.aabb_max[1] >= p.center.y + r - 1e-6);
                    assert!(parent.aabb_max[2] >= p.center.z + r - 1e-6);
                } else {
                    let child = &nodes[child_ref as usize];
                    assert!(parent.aabb_min[0] <= child.aabb_min[0] + 1e-6);
                    assert!(parent.aabb_min[1] <= child.aabb_min[1] + 1e-6);
                    assert!(parent.aabb_min[2] <= child.aabb_min[2] + 1e-6);
                    assert!(parent.aabb_max[0] >= child.aabb_max[0] - 1e-6);
                    assert!(parent.aabb_max[1] >= child.aabb_max[1] - 1e-6);
                    assert!(parent.aabb_max[2] >= child.aabb_max[2] - 1e-6);
                }
            }
            check_child_contained(node, node.first, &bvh.nodes, &prims);
            check_child_contained(node, node.count, &bvh.nodes, &prims);
        }
    }

    #[test]
    fn test_intersect_body_max_k() {
        let prims: Vec<BvhPrim> = vec![
            BvhPrim { center: Vec3::ZERO, radius: 1.0, body_local_index: 0, op: 0, blend_k: 0.1 },
            BvhPrim { center: Vec3::X, radius: 1.0, body_local_index: 1, op: 2, blend_k: 0.2 },
            BvhPrim { center: Vec3::Y, radius: 1.0, body_local_index: 2, op: 0, blend_k: 0.3 },
        ];
        let bvh = build_body_bvh(&prims, 0).unwrap();
        assert_eq!(bvh.body_max_k, f32::MAX);
    }

    #[test]
    fn test_no_intersect_body_max_k() {
        let prims: Vec<BvhPrim> = vec![
            BvhPrim { center: Vec3::ZERO, radius: 1.0, body_local_index: 0, op: 0, blend_k: 0.1 },
            BvhPrim { center: Vec3::X, radius: 1.0, body_local_index: 1, op: 1, blend_k: 0.5 },
            BvhPrim { center: Vec3::Y, radius: 1.0, body_local_index: 2, op: 0, blend_k: 0.3 },
        ];
        let bvh = build_body_bvh(&prims, 0).unwrap();
        assert_eq!(bvh.body_max_k, 0.5);
    }

    #[test]
    fn test_two_prims() {
        let prims = vec![
            BvhPrim { center: Vec3::new(-1.0, 0.0, 0.0), radius: 0.5, body_local_index: 0, op: 0, blend_k: 0.0 },
            BvhPrim { center: Vec3::new(1.0, 0.0, 0.0), radius: 0.5, body_local_index: 1, op: 0, blend_k: 0.0 },
        ];
        let bvh = build_body_bvh(&prims, 0).unwrap();
        assert_eq!(bvh.nodes.len(), 1);
        assert_ne!(bvh.nodes[0].first & LEAF_BIT, 0);
        assert_ne!(bvh.nodes[0].count & LEAF_BIT, 0);
        let l = bvh.nodes[0].first & !LEAF_BIT;
        let r = bvh.nodes[0].count & !LEAF_BIT;
        assert_ne!(l, r);
    }

    #[test]
    fn test_empty_prims() {
        assert!(build_body_bvh(&[], 8).is_none());
        assert!(build_body_bvh(&[], 0).is_none());
    }

    // --- Scene-level build_scene_bvh tests ---

    #[test]
    fn test_scene_bvh_empty() {
        let nodes = build_scene_bvh(&[]);
        assert!(nodes.is_empty());
    }

    #[test]
    fn test_scene_bvh_single() {
        let bodies = vec![SceneBvhBody {
            aabb: aabb_at(0.0, 0.0, 0.0, 2.0),
            start_entry: 5,
            entry_count: 12,
        }];
        let nodes = build_scene_bvh(&bodies);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].first, 5);
        assert_eq!(nodes[0].count, 12);
    }

    #[test]
    fn test_scene_bvh_two() {
        let bodies = vec![
            SceneBvhBody { aabb: aabb_at(-5.0, 0.0, 0.0, 1.0), start_entry: 0, entry_count: 10 },
            SceneBvhBody { aabb: aabb_at(5.0, 0.0, 0.0, 1.0), start_entry: 10, entry_count: 8 },
        ];
        let nodes = build_scene_bvh(&bodies);
        assert_eq!(nodes.len(), 3); // 1 root + 2 leaves
        assert_eq!(nodes[0].count, 0); // root is internal
        let l = nodes[0].first as usize;
        assert!(nodes[l].count > 0); // left leaf
        assert!(nodes[l + 1].count > 0); // right leaf
    }

    #[test]
    fn test_scene_bvh_five() {
        let bodies: Vec<SceneBvhBody> = (0..5).map(|i| {
            let f = i as f32;
            SceneBvhBody {
                aabb: aabb_at(f * 4.0, 0.0, 0.0, 1.0),
                start_entry: (i * 10) as u32,
                entry_count: 10,
            }
        }).collect();
        let nodes = build_scene_bvh(&bodies);
        assert!(!nodes.is_empty());
        // All start_entry/entry_count values should appear in leaves
        let mut found_starts: Vec<u32> = Vec::new();
        for n in &nodes {
            if n.count > 0 { // leaf
                found_starts.push(n.first);
            }
        }
        found_starts.sort();
        assert_eq!(found_starts, vec![0, 10, 20, 30, 40]);
    }

    // =================================================================
    // Triangle BLAS tests â€” the ground truth for Phase 2.
    //
    // Strategy: traverse the BLAS with a CPU ray and assert the nearest hit
    // matches a brute-force scan over every triangle. If the tree structure,
    // leaf encoding, or triangle remap were wrong, the two would disagree.
    // =================================================================

    /// MÃ¶llerâ€“Trumbore ray/triangle intersection. Returns `t` (ray-param of hit)
    /// for a front-or-back hit with `t > eps`, else `None`.
    fn ray_tri(orig: [f32; 3], dir: [f32; 3], a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<f32> {
        let sub = |p: [f32; 3], q: [f32; 3]| [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
        let cross = |p: [f32; 3], q: [f32; 3]| {
            [
                p[1] * q[2] - p[2] * q[1],
                p[2] * q[0] - p[0] * q[2],
                p[0] * q[1] - p[1] * q[0],
            ]
        };
        let dot = |p: [f32; 3], q: [f32; 3]| p[0] * q[0] + p[1] * q[1] + p[2] * q[2];
        let eps = 1e-7;
        let e1 = sub(b, a);
        let e2 = sub(c, a);
        let pv = cross(dir, e2);
        let det = dot(e1, pv);
        if det.abs() < eps {
            return None; // ray parallel to triangle plane
        }
        let inv_det = 1.0 / det;
        let tv = sub(orig, a);
        let u = dot(tv, pv) * inv_det;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let qv = cross(tv, e1);
        let v = dot(dir, qv) * inv_det;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = dot(e2, qv) * inv_det;
        if t > 1e-5 { Some(t) } else { None }
    }

    /// Slab test: does the ray reach the AABB within `[0, t_max]`? (Only used to
    /// prune; correctness is checked by comparing against brute force, so a loose
    /// slab test can only make the BVH slower, never wrong.)
    fn ray_aabb(orig: [f32; 3], inv_dir: [f32; 3], node: &BvhNode, t_max: f32) -> bool {
        let mut tmin = 0.0f32;
        let mut tmax = t_max;
        for a in 0..3 {
            let t1 = (node.aabb_min[a] - orig[a]) * inv_dir[a];
            let t2 = (node.aabb_max[a] - orig[a]) * inv_dir[a];
            let (lo, hi) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
            tmin = tmin.max(lo);
            tmax = tmax.min(hi);
        }
        tmin <= tmax
    }

    /// Nearest triangle hit via brute force over all triangles.
    fn brute_hit(
        positions: &[[f32; 3]],
        indices: &[u32],
        orig: [f32; 3],
        dir: [f32; 3],
    ) -> Option<(f32, u32)> {
        let mut best: Option<(f32, u32)> = None;
        for t in 0..indices.len() / 3 {
            let a = positions[indices[t * 3] as usize];
            let b = positions[indices[t * 3 + 1] as usize];
            let c = positions[indices[t * 3 + 2] as usize];
            if let Some(hit_t) = ray_tri(orig, dir, a, b, c) {
                if best.map_or(true, |(bt, _)| hit_t < bt) {
                    best = Some((hit_t, t as u32));
                }
            }
        }
        best
    }

    /// Nearest triangle hit via BLAS traversal (short-stack, mirrors the shader).
    fn blas_hit(
        blas: &TriBlas,
        positions: &[[f32; 3]],
        indices: &[u32],
        orig: [f32; 3],
        dir: [f32; 3],
    ) -> Option<(f32, u32)> {
        if blas.is_empty() {
            return None;
        }
        let inv_dir = [
            1.0 / (if dir[0] == 0.0 { 1e-20 } else { dir[0] }),
            1.0 / (if dir[1] == 0.0 { 1e-20 } else { dir[1] }),
            1.0 / (if dir[2] == 0.0 { 1e-20 } else { dir[2] }),
        ];
        let mut best: Option<(f32, u32)> = None;
        let mut stack: Vec<u32> = vec![0];
        while let Some(node_idx) = stack.pop() {
            let node = &blas.nodes[node_idx as usize];
            let t_max = best.map_or(f32::MAX, |(bt, _)| bt);
            if !ray_aabb(orig, inv_dir, node, t_max) {
                continue;
            }
            if node.count > 0 {
                // Leaf: test each triangle in tri_order[first..first+count].
                for k in node.first..node.first + node.count {
                    let t = blas.tri_order[k as usize];
                    let a = positions[indices[t as usize * 3] as usize];
                    let b = positions[indices[t as usize * 3 + 1] as usize];
                    let c = positions[indices[t as usize * 3 + 2] as usize];
                    if let Some(hit_t) = ray_tri(orig, dir, a, b, c) {
                        if best.map_or(true, |(bt, _)| hit_t < bt) {
                            best = Some((hit_t, t));
                        }
                    }
                }
            } else {
                stack.push(node.first);
                stack.push(node.first + 1);
            }
        }
        best
    }

    /// Unit cube centered at origin (Â±0.5), 8 verts / 12 triangles.
    fn unit_cube() -> (Vec<[f32; 3]>, Vec<u32>) {
        let v = vec![
            [-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [0.5, 0.5, -0.5], [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],  [0.5, -0.5, 0.5],  [0.5, 0.5, 0.5],  [-0.5, 0.5, 0.5],
        ];
        // 12 triangles (CCW outward), 2 per face.
        let i = vec![
            0, 2, 1, 0, 3, 2, // -Z
            4, 5, 6, 4, 6, 7, // +Z
            0, 1, 5, 0, 5, 4, // -Y
            3, 7, 6, 3, 6, 2, // +Y
            0, 4, 7, 0, 7, 3, // -X
            1, 2, 6, 1, 6, 5, // +X
        ];
        (v, i)
    }

    fn approx(a: Option<(f32, u32)>, b: Option<(f32, u32)>) {
        match (a, b) {
            (None, None) => {}
            (Some((ta, _)), Some((tb, _))) => assert!(
                (ta - tb).abs() < 1e-3,
                "hit distance mismatch: bvh={ta} brute={tb}"
            ),
            _ => panic!("hit/miss disagreement: bvh={a:?} brute={b:?}"),
        }
    }

    #[test]
    fn tri_blas_empty_mesh() {
        let blas = build_tri_blas(&[], &[]);
        assert!(blas.is_empty());
        assert_eq!(blas.triangle_count(), 0);
        assert!(blas_hit(&blas, &[], &[], [0.0, 0.0, -5.0], [0.0, 0.0, 1.0]).is_none());
    }

    #[test]
    fn tri_blas_covers_every_triangle_once() {
        let (v, i) = unit_cube();
        let blas = build_tri_blas(&v, &i);
        assert_eq!(blas.triangle_count(), 12);
        let mut seen = blas.tri_order.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0u32..12).collect::<Vec<_>>(), "tri_order must be a permutation of all triangles");
    }

    #[test]
    fn tri_blas_skips_out_of_range_corner() {
        let v = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        // Second "triangle" references vertex 99 (out of range) â€” must be skipped.
        let i = vec![0, 1, 2, 0, 1, 99];
        let blas = build_tri_blas(&v, &i);
        assert_eq!(blas.triangle_count(), 1);
        assert_eq!(blas.tri_order, vec![0]);
    }

    #[test]
    fn tri_blas_node_aabbs_contain_children() {
        let (v, i) = unit_cube();
        let blas = build_tri_blas(&v, &i);
        for node in &blas.nodes {
            if node.count == 0 {
                for child in [node.first, node.first + 1] {
                    let c = &blas.nodes[child as usize];
                    for a in 0..3 {
                        assert!(c.aabb_min[a] >= node.aabb_min[a] - 1e-4);
                        assert!(c.aabb_max[a] <= node.aabb_max[a] + 1e-4);
                    }
                }
            }
        }
    }

    #[test]
    fn tri_blas_matches_brute_force_cube() {
        let (v, i) = unit_cube();
        let blas = build_tri_blas(&v, &i);
        // Rays from every axis, hits and misses, plus a diagonal.
        let rays: [([f32; 3], [f32; 3]); 7] = [
            ([0.0, 0.0, -5.0], [0.0, 0.0, 1.0]),   // -Z â†’ hits front face at t=4.5
            ([0.0, 0.0, 5.0], [0.0, 0.0, -1.0]),   // +Z
            ([5.0, 0.0, 0.0], [-1.0, 0.0, 0.0]),   // +X
            ([0.0, 5.0, 0.0], [0.0, -1.0, 0.0]),   // +Y
            ([2.0, 2.0, 2.0], [-1.0, -1.0, -1.0]), // diagonal through corner region
            ([0.0, 0.0, -5.0], [0.0, 1.0, 0.0]),   // miss (parallel, above)
            ([3.0, 3.0, -5.0], [0.0, 0.0, 1.0]),   // miss (outside footprint)
        ];
        for (orig, dir) in rays {
            let n = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            let d = [dir[0] / n, dir[1] / n, dir[2] / n];
            approx(blas_hit(&blas, &v, &i, orig, d), brute_hit(&v, &i, orig, d));
        }
    }

    #[test]
    fn tri_blas_matches_brute_force_two_bodies() {
        // Two cubes offset along X â€” a single BLAS over both (as if one merged mesh).
        // Nearest-hit must still be exact: the ray should hit the closer cube.
        let (v0, i0) = unit_cube();
        let mut v: Vec<[f32; 3]> = v0.iter().map(|p| [p[0] - 2.0, p[1], p[2]]).collect();
        let base = v.len() as u32;
        v.extend(v0.iter().map(|p| [p[0] + 2.0, p[1], p[2]]));
        let mut i = i0.clone();
        i.extend(i0.iter().map(|&idx| idx + base));
        let blas = build_tri_blas(&v, &i);
        assert_eq!(blas.triangle_count(), 24);

        // A dense grid of rays along +X so both cubes are candidates; the nearest
        // must always be the left cube (xâ‰ˆ-2) when the ray starts far -X.
        for gy in -2..=2 {
            for gz in -2..=2 {
                let y = gy as f32 * 0.2;
                let z = gz as f32 * 0.2;
                let orig = [-10.0, y, z];
                let dir = [1.0, 0.0, 0.0];
                approx(blas_hit(&blas, &v, &i, orig, dir), brute_hit(&v, &i, orig, dir));
                // And from the opposite side.
                let orig2 = [10.0, y, z];
                let dir2 = [-1.0, 0.0, 0.0];
                approx(blas_hit(&blas, &v, &i, orig2, dir2), brute_hit(&v, &i, orig2, dir2));
            }
        }
    }

    /// GATE â€” `blas_any_hit` agrees with an exhaustive scan, and is not trivially true/false.
    ///
    /// Occlusion is the whole basis of the AO and Thickness mesh maps: a query that always
    /// reports "clear" bakes a uniformly white AO map, and one that always reports "blocked"
    /// bakes black. Both look like a plausible result, so the gate asserts BOTH directions on a
    /// closed box â€” a ray from inside must hit, a ray leaving must not â€” and cross-checks every
    /// query against brute force over all triangles.
    #[test]
    fn blas_any_hit_matches_brute_force_and_is_not_vacuous() {
        // Unit box centred at the origin, as 12 triangles.
        let h = 0.5f32;
        let p: Vec<[f32; 3]> = vec![
            [-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h],
            [-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h],
        ];
        let quads = [
            [0usize, 1, 2, 3], [5, 4, 7, 6], [4, 5, 1, 0],
            [3, 2, 6, 7], [1, 5, 6, 2], [4, 0, 3, 7],
        ];
        let mut idx: Vec<u32> = Vec::new();
        for q in quads {
            idx.extend_from_slice(&[q[0] as u32, q[1] as u32, q[2] as u32]);
            idx.extend_from_slice(&[q[0] as u32, q[2] as u32, q[3] as u32]);
        }
        let blas = build_tri_blas(&p, &idx);
        assert_eq!(blas.triangle_count(), 12);

        let brute = |o: [f32; 3], d: [f32; 3], t_max: f32| -> bool {
            (0..idx.len() / 3).any(|t| {
                ray_triangle(o, d, p[idx[t * 3] as usize], p[idx[t * 3 + 1] as usize],
                             p[idx[t * 3 + 2] as usize])
                    .is_some_and(|hit| hit < t_max)
            })
        };

        // Rays from the centre outward must ALL hit a wall (not vacuously false).
        let mut hits = 0;
        for d in [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0],
                  [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0],
                  [0.577, 0.577, 0.577], [-0.577, 0.577, -0.577]] {
            let o = [0.0, 0.0, 0.0];
            let got = blas_any_hit(&blas, &p, &idx, o, d, 10.0);
            assert_eq!(got, brute(o, d, 10.0), "disagrees with brute force for dir {d:?}");
            if got { hits += 1; }
        }
        assert_eq!(hits, 8, "every ray from inside a closed box must be blocked");

        // Rays leaving from outside, pointing away, must ALL miss (not vacuously true).
        let mut misses = 0;
        for d in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let o = [d[0] * 2.0, d[1] * 2.0, d[2] * 2.0];
            let got = blas_any_hit(&blas, &p, &idx, o, d, 10.0);
            assert_eq!(got, brute(o, d, 10.0), "disagrees with brute force for outward {d:?}");
            if !got { misses += 1; }
        }
        assert_eq!(misses, 3, "a ray pointing away from the box must not be blocked");

        // t_max must actually bound the query: the far wall is 0.5 away.
        let o = [0.0, 0.0, 0.0];
        let d = [1.0, 0.0, 0.0];
        assert!(blas_any_hit(&blas, &p, &idx, o, d, 10.0), "unbounded ray hits");
        assert!(!blas_any_hit(&blas, &p, &idx, o, d, 0.25), "t_max shorter than the wall must miss");
    }

    /// GATE (G2) â€” `blas_nearest_hit` matches exhaustive min-t and is not vacuous.
    #[test]
    fn blas_nearest_hit_matches_brute_force_and_respects_t_max() {
        let (p, idx) = unit_cube();
        let blas = build_tri_blas(&p, &idx);

        let brute = |o: [f32; 3], d: [f32; 3], t_max: f32| -> Option<f32> {
            let mut best = None;
            for t in 0..idx.len() / 3 {
                if let Some(hit) = ray_triangle(
                    o, d,
                    p[idx[t * 3] as usize],
                    p[idx[t * 3 + 1] as usize],
                    p[idx[t * 3 + 2] as usize],
                ) {
                    if hit < t_max && best.map_or(true, |b| hit < b) {
                        best = Some(hit);
                    }
                }
            }
            best
        };

        // Outside, pointing at the cube: nearest wall is 9.5 away (origin x=-10, wall at -0.5).
        let o = [-10.0, 0.0, 0.0];
        let d = [1.0, 0.0, 0.0];
        let got = blas_nearest_hit(&blas, &p, &idx, o, d, 100.0);
        let exp = brute(o, d, 100.0);
        assert!(got.is_some() && exp.is_some(), "must hit the cube");
        assert!((got.unwrap().0 - exp.unwrap()).abs() < 1e-4);
        assert!((got.unwrap().0 - 9.5).abs() < 1e-3, "nearest face is x=-0.5, t=9.5");

        // t_max shorter than the wall: miss (negative test).
        assert!(
            blas_nearest_hit(&blas, &p, &idx, o, d, 1.0).is_none(),
            "t_max=1 cannot reach the cube"
        );

        // Two cubes: nearest is the closer one (the existing two-body fixture).
        let (v0, i0) = unit_cube();
        let mut v: Vec<[f32; 3]> = v0.iter().map(|p| [p[0] - 2.0, p[1], p[2]]).collect();
        let base = v.len() as u32;
        v.extend(v0.iter().map(|p| [p[0] + 2.0, p[1], p[2]]));
        let mut i = i0.clone();
        i.extend(i0.iter().map(|&idx| idx + base));
        let blas2 = build_tri_blas(&v, &i);
        let hit = blas_nearest_hit(&blas2, &v, &i, [-10.0, 0.0, 0.0], [1.0, 0.0, 0.0], 100.0);
        assert!(hit.is_some());
        assert!(
            (hit.unwrap().0 - 7.5).abs() < 1e-3,
            "closer cube wall at x=-2.5, t=7.5, got {:?}",
            hit
        );
    }
}
