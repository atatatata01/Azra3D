// PolySculpt PBVH — PR-9 of ZB_RESPONSIVE_SCULPT_PLAN §8 (P1, first slice).
//
// A vert bounding-volume tree over the WORKING mesh — Blender's PBVH concept, NOT the
// SDF prim SAH in `core_math/src/bvh.rs` (wrong domain: GPU traversal, LEAF_BIT
// encoding). PR-9 ships the QUERY consumer only, behind `AZRA_POLYSCULPT_PBVH=1`;
// `VertGrid` stays the production query until the tree is measured ≤ grid at the same
// seed counts AND also serves pick + draw batches (§8.2 — PR-10).
//
// Build: median split on the longest AABB axis, in-place partition, leaves hold vert-id
// ranges (~LEAF_TARGET verts — §8.1's 250–2000 band). Refit: a stamp EXPANDS the leaf
// AABBs of moved verts and propagates up — a loose AABB is CORRECT (leaves re-filter by
// exact distance; looseness only costs pruning), so refit never misses. A rebuild
// heuristic (escape counter) keeps long strokes from degenerating to a linear scan.

use core_math::half_edge::{HalfEdgeMesh, VertexId};
use glam::Vec3;

const LEAF_TARGET: usize = 1024;

/// Total-ordered f32 for the front-to-back heap (NaN-free by construction).
#[derive(PartialEq)]
struct Ordered(f32);
impl Eq for Ordered {}
impl PartialOrd for Ordered {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ordered {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}
fn ordered(x: f32) -> Ordered {
    Ordered(x)
}
/// Full rebuild after this many vert-escapes-leaf events (huge Grab class — §8.1).
const REBUILD_ESCAPES: u32 = 50_000;

#[derive(Clone, Copy)]
struct Node {
    lo: Vec3,
    hi: Vec3,
    /// `u32::MAX` = leaf; else index of the left child (right = left + 1... children
    /// are stored contiguously).
    left: u32,
    /// Leaf: [start, end) into `verts`.
    start: u32,
    end: u32,
}

pub(crate) struct SculptPbvh {
    nodes: Vec<Node>,
    /// Parent slot per node (u32::MAX = root) — refit walks leaf→root in O(depth).
    parents: Vec<u32>,
    verts: Vec<u32>,
    /// vert id → leaf node index (refit marking). u32::MAX = not in the tree.
    leaf_of: Vec<u32>,
    escapes: u32,
}

impl SculptPbvh {
    pub(crate) fn build(mesh: &HalfEdgeMesh) -> Self {
        let mut ids: Vec<u32> = (0..mesh.vertices.len() as u32)
            .filter(|&i| mesh.is_vert_live(VertexId(i)))
            .collect();
        let mut nodes = Vec::new();
        let mut leaf_of = vec![u32::MAX; mesh.vertices.len()];
        if ids.is_empty() {
            return Self {
                nodes,
                parents: Vec::new(),
                verts: ids,
                leaf_of,
                escapes: 0,
            };
        }
        let mut parents: Vec<u32> = vec![u32::MAX];
        // Recursive build with an explicit stack: (verts range, parent slot).
        struct Task {
            start: usize,
            end: usize,
            slot: usize,
        }
        nodes.push(Node {
            lo: Vec3::ZERO,
            hi: Vec3::ZERO,
            left: u32::MAX,
            start: 0,
            end: 0,
        });
        let mut stack = vec![Task {
            start: 0,
            end: ids.len(),
            slot: 0,
        }];
        while let Some(t) = stack.pop() {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for &i in &ids[t.start..t.end] {
                let p = mesh.vert(VertexId(i)).position;
                lo = lo.min(p);
                hi = hi.max(p);
            }
            if t.end - t.start <= LEAF_TARGET {
                nodes[t.slot] = Node {
                    lo,
                    hi,
                    left: u32::MAX,
                    start: t.start as u32,
                    end: t.end as u32,
                };
                for &i in &ids[t.start..t.end] {
                    leaf_of[i as usize] = t.slot as u32;
                }
                continue;
            }
            let ext = hi - lo;
            let axis = if ext.x >= ext.y && ext.x >= ext.z {
                0
            } else if ext.y >= ext.z {
                1
            } else {
                2
            };
            let mid = (t.start + t.end) / 2;
            ids[t.start..t.end].select_nth_unstable_by(mid - t.start, |&a, &b| {
                let pa = mesh.vert(VertexId(a)).position[axis];
                let pb = mesh.vert(VertexId(b)).position[axis];
                pa.total_cmp(&pb)
            });
            let left = nodes.len();
            nodes.push(Node {
                lo: Vec3::ZERO,
                hi: Vec3::ZERO,
                left: u32::MAX,
                start: 0,
                end: 0,
            });
            nodes.push(Node {
                lo: Vec3::ZERO,
                hi: Vec3::ZERO,
                left: u32::MAX,
                start: 0,
                end: 0,
            });
            parents.push(t.slot as u32);
            parents.push(t.slot as u32);
            nodes[t.slot] = Node {
                lo,
                hi,
                left: left as u32,
                start: 0,
                end: 0,
            };
            stack.push(Task {
                start: t.start,
                end: mid,
                slot: left,
            });
            stack.push(Task {
                start: mid,
                end: t.end,
                slot: left + 1,
            });
        }
        Self {
            nodes,
            parents,
            verts: ids,
            leaf_of,
            escapes: 0,
        }
    }

    /// Same CONTRACT as `VertGrid::query`: live verts within `radius` of `center`
    /// (exact distance filter at the leaves — loose AABBs cannot cause false hits).
    pub(crate) fn query(&self, mesh: &HalfEdgeMesh, center: Vec3, radius: f32) -> Vec<u32> {
        let r = radius.max(0.0);
        let r2 = r * r;
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }
        let mut stack = vec![0usize];
        while let Some(ni) = stack.pop() {
            let n = self.nodes[ni];
            // Sphere vs AABB: squared distance from center to the box.
            let c = center.clamp(n.lo, n.hi);
            if (c - center).length_squared() > r2 {
                continue;
            }
            if n.left == u32::MAX {
                for &i in &self.verts[n.start as usize..n.end as usize] {
                    let vid = VertexId(i);
                    if !mesh.is_vert_live(vid) {
                        continue;
                    }
                    if (mesh.vert(vid).position - center).length_squared() <= r2 {
                        out.push(i);
                    }
                }
            } else {
                stack.push(n.left as usize);
                stack.push(n.left as usize + 1);
            }
        }
        out
    }

    /// PR-10: ray pick via node AABBs, front-to-back. Incident faces of leaf verts
    /// come from PR-2's CSR (`vert_off`/`vert_faces`); every face runs the SAME shared
    /// `ray_face_hit` as the full walk and the DDA — one math, three visit orders.
    /// Same contract as the DDA: dedup, hard `budget`, fail CLOSED on exhaustion.
    /// Returns (hit_t, face_tests_spent). Ray is WORLD-space; nodes are LOCAL, so the
    /// traversal transforms the ray once.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pick(
        &self,
        mesh: &HalfEdgeMesh,
        world: glam::Mat4,
        ray_o: Vec3,
        ray_d: Vec3,
        hidden: &std::collections::HashSet<u32>,
        csr: (&[u32], &[u32]),
        mut budget: usize,
    ) -> (Option<f32>, usize) {
        if self.nodes.is_empty() {
            return (None, 0);
        }
        let inv = world.inverse();
        let o = inv.transform_point3(ray_o);
        let d = inv.transform_vector3(ray_d).normalize_or_zero();
        if d == Vec3::ZERO {
            return (None, 0);
        }
        let slab = |n: &Node| -> Option<f32> {
            // Ray vs AABB (branchless slabs); returns entry t ≥ 0, None on miss.
            let inv_d = Vec3::new(
                if d.x.abs() < 1e-12 { f32::INFINITY } else { 1.0 / d.x },
                if d.y.abs() < 1e-12 { f32::INFINITY } else { 1.0 / d.y },
                if d.z.abs() < 1e-12 { f32::INFINITY } else { 1.0 / d.z },
            );
            let t0 = (n.lo - o) * inv_d;
            let t1 = (n.hi - o) * inv_d;
            let tmin = t0.min(t1).max_element().max(0.0);
            let tmax = t0.max(t1).min_element();
            (tmax >= tmin).then_some(tmin)
        };
        // Front-to-back: sorted stack keyed by entry t (pop the NEAREST next).
        let mut heap: std::collections::BinaryHeap<(std::cmp::Reverse<Ordered>, usize)> =
            Default::default();
        let (off, faces) = csr;
        let spent0 = budget;
        let mut best: Option<f32> = None;
        let mut best_local = f32::INFINITY;
        let mut seen = std::collections::HashSet::new();
        let root_t = match slab(&self.nodes[0]) {
            Some(t) => t,
            None => return (None, 0),
        };
        heap.push((std::cmp::Reverse(ordered(root_t)), 0));
        while let Some((std::cmp::Reverse(entry), ni)) = heap.pop() {
            if best.is_some() && entry.0 > best_local {
                break; // every remaining node starts beyond the hit
            }
            let n = self.nodes[ni];
            if n.left == u32::MAX {
                for &vi in &self.verts[n.start as usize..n.end as usize] {
                    if (vi as usize + 1) >= off.len() {
                        continue;
                    }
                    let (a, b) = (off[vi as usize] as usize, off[vi as usize + 1] as usize);
                    for &f in &faces[a..b] {
                        if !seen.insert(f) {
                            continue;
                        }
                        if budget == 0 {
                            return (None, spent0); // fail closed (§7.1)
                        }
                        budget -= 1;
                        if let Some(t) = crate::poly_selection::ray_face_hit(
                            mesh,
                            world,
                            core_math::half_edge::FaceId(f),
                            ray_o,
                            ray_d,
                            hidden,
                        ) {
                            if best.map_or(true, |bt| t < bt) {
                                best = Some(t);
                                let hw = ray_o + ray_d * t;
                                best_local = (inv.transform_point3(hw) - o).dot(d);
                            }
                        }
                    }
                }
            } else {
                for c in [n.left as usize, n.left as usize + 1] {
                    if let Some(t) = slab(&self.nodes[c]) {
                        heap.push((std::cmp::Reverse(ordered(t)), c));
                    }
                }
            }
        }
        (best, spent0 - budget)
    }

    /// Post-stamp refit: EXPAND the leaves of moved verts (and ancestors) to their
    /// current positions. Correct by construction — never re-tightens, never misses.
    /// Returns true when the escape budget says the caller should REBUILD.
    pub(crate) fn refit_moved(&mut self, mesh: &HalfEdgeMesh, moved: &[u32]) -> bool {
        for &i in moved {
            let Some(&leaf) = self.leaf_of.get(i as usize) else { continue };
            if leaf == u32::MAX {
                continue;
            }
            let vid = VertexId(i);
            if !mesh.is_vert_live(vid) {
                continue;
            }
            let p = mesh.vert(vid).position;
            let n = &self.nodes[leaf as usize];
            if p.cmpge(n.lo).all() && p.cmple(n.hi).all() {
                continue;
            }
            self.escapes += 1;
            // Expand leaf → root via parent links, O(depth).
            let mut ni = leaf as usize;
            loop {
                let n = &mut self.nodes[ni];
                n.lo = n.lo.min(p);
                n.hi = n.hi.max(p);
                let up = self.parents[ni];
                if up == u32::MAX {
                    break;
                }
                ni = up as usize;
            }
        }
        self.escapes >= REBUILD_ESCAPES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(n: usize) -> HalfEdgeMesh {
        let mut pos = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                pos.push(Vec3::new(
                    i as f32 * 0.25,
                    ((i * 7 + j * 3) % 5) as f32 * 0.03,
                    j as f32 * 0.25,
                ));
            }
        }
        let at = |i: usize, j: usize| j * (n + 1) + i;
        let mut faces = Vec::new();
        for j in 0..n {
            for i in 0..n {
                faces.push(vec![at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
        let mut m = HalfEdgeMesh::from_polygons(&pos, &faces);
        m.recompute_normals();
        m
    }

    /// PR-9 gate: PBVH query ≡ VertGrid query (the P0 contract), across radii and
    /// centers, before AND after moves + refit.
    #[test]
    fn pbvh_query_matches_grid() {
        let mut mesh = plane(90);
        let grid = super::super::sculpt::VertGrid::rebuild(
            &mesh,
            super::super::sculpt::grid_cell_for(&mesh),
        );
        let mut pbvh = SculptPbvh::build(&mesh);
        let cases = [
            (Vec3::new(11.0, 0.0, 11.0), 2.0),
            (Vec3::new(3.0, 0.1, 20.0), 0.7),
            (Vec3::new(22.4, 0.0, 1.3), 5.0),
            (Vec3::new(-4.0, 0.0, -4.0), 3.0), // fully outside
        ];
        for (c, r) in cases {
            let mut a = grid.query(&mesh, c, r);
            let mut b = pbvh.query(&mesh, c, r);
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "pre-move query mismatch at {c:?} r={r}");
        }
        // Move a patch far outside its leaf, refit, and re-verify — the loose AABB
        // must still find the escaped verts.
        let moved: Vec<u32> = pbvh.query(&mesh, Vec3::new(11.0, 0.0, 11.0), 1.5);
        assert!(!moved.is_empty());
        let mut grid2 = super::super::sculpt::VertGrid::rebuild(
            &mesh,
            super::super::sculpt::grid_cell_for(&mesh),
        );
        for &i in &moved {
            let vid = VertexId(i);
            let old = mesh.vert(vid).position;
            let new = old + Vec3::new(0.0, 4.0, 0.0);
            mesh.vert_mut(vid).position = new;
            grid2.move_vert(i, old, new);
        }
        pbvh.refit_moved(&mesh, &moved);
        for (c, r) in [
            (Vec3::new(11.0, 4.0, 11.0), 2.0), // at the displaced patch
            (Vec3::new(11.0, 0.0, 11.0), 2.0), // at the crater
            (Vec3::new(11.0, 2.0, 11.0), 6.0), // spanning both
        ] {
            let mut a = grid2.query(&mesh, c, r);
            let mut b = pbvh.query(&mesh, c, r);
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "post-refit query mismatch at {c:?} r={r}");
        }
    }
}
