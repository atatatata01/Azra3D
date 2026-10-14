// DYNTOPO refine kernel — Blender-tech dynamic topology (docs/planning/poly/DYNTOPO_PLAN.md
// Phase A, owner directive 2026-08-19 "DYNTOPO NEEDS TO BE LIKE BLENDER TECH").
//
// Local edge SUBDIVIDE / COLLAPSE inside a brush sphere, composed ENTIRELY from the
// existing ID-preserving Euler ops (`split_edge`, `split_face_between`,
// `merge_vertices_to_ex`) — geometry outside the region keeps its ids, positions, and
// topology bit-for-bit (asserted by the tests below). Blender model:
//   * edges longer than `detail_len` split at their midpoint (long-edge queue, recursive),
//   * edges shorter than `0.4 × detail_len` collapse to their midpoint (short-edge queue),
//   * dyntopo ground is TRIANGLES — a quad the brush touches is fan-triangulated first
//     (topology ripples at most one ring past the brush, exactly like Blender).
//
// Every call is hard-capped by `max_ops` (Frame Budget rule); the mesh is valid at ANY
// stop point because each Euler op leaves it valid. Known Phase-A cost, budgeted not
// hidden: `merge_vertices_to_ex` still redirects via a full half-edge scan (Phase B2
// localizes it) — the op budget bounds the total.

use super::*;
use std::collections::VecDeque;

/// Blender's short-edge ratio: collapse when `len < COLLAPSE_RATIO × detail_len`.
pub const DYNTOPO_COLLAPSE_RATIO: f32 = 0.4;

#[derive(Clone, Copy, Debug)]
pub struct DyntopoParams {
    /// Brush sphere (mesh-local space — the same space the sculpt dabs use).
    pub center: Vec3,
    pub radius: f32,
    /// Target edge length inside the brush (world/local units, > 0).
    pub detail_len: f32,
    /// Hard cap on Euler ops this call (each split / face-split / collapse counts 1).
    pub max_ops: usize,
    /// Run the short-edge collapse pass (false = Blender "Subdivide Edges" refine mode).
    pub collapse: bool,
}

#[derive(Default, Debug)]
pub struct DyntopoResult {
    pub splits: usize,
    pub collapses: usize,
    pub triangulated: usize,
    pub budget_hit: bool,
    /// Verts whose 1-ring changed (new verts + collapse survivors + split endpoints).
    /// The app layer patches its runtime state (VertGrid / CSR / display) from this.
    pub touched: Vec<VertexId>,
}

impl HalfEdgeMesh {
    /// Refine the brush region to `detail_len` (see module header). Returns what changed.
    /// Whole-mesh candidate scan — the stroke path should prefer
    /// [`Self::dyntopo_refine_region_seeded`] with its spatial-index query.
    pub fn dyntopo_refine_region(&mut self, p: &DyntopoParams) -> DyntopoResult {
        self.dyntopo_refine_region_seeded(p, None)
    }

    /// [`Self::dyntopo_refine_region`] with the candidate gather seeded from `seeds` —
    /// the verts at the brush, from the caller's spatial index (VertGrid / PBVH radius
    /// query). This is Blender's locality model verbatim: the long/short edge queues are
    /// built ONLY from faces of PBVH nodes intersecting the brush sphere
    /// (pbvh_bmesh.cc `long_edge_queue_create` / `short_edge_queue_create`), never from a
    /// whole-mesh scan — which is what makes a steady-state stroke over already-refined
    /// ground actually cost nothing. `None` = scan everything (tests, callers without an
    /// index).
    ///
    /// Seeds are re-validated here (bounds, live, in-region) — a stale index entry is
    /// skipped, never trusted — then sorted ascending so op order (and therefore the
    /// resulting ids) never depends on the caller's index iteration order.
    /// COVERAGE CONTRACT: every in-region vert must be present in `seeds`; an edge/face
    /// qualifies through the fan of an in-region endpoint/corner. A vert the index missed
    /// (created by an earlier call this frame, before the index rebuilt) self-heals on
    /// the next frame's fresh index; phase 1's face fanning creates no verts, so it
    /// cannot widen that gap mid-call.
    pub fn dyntopo_refine_region_seeded(
        &mut self,
        p: &DyntopoParams,
        seeds: Option<&[VertexId]>,
    ) -> DyntopoResult {
        let mut res = DyntopoResult::default();
        let detail = p.detail_len.max(1e-6);
        let min_len = detail * DYNTOPO_COLLAPSE_RATIO;
        let r2 = p.radius.max(0.0).powi(2);
        if r2 <= 0.0 || p.max_ops == 0 {
            res.budget_hit = p.max_ops == 0;
            return res;
        }
        let mut budget = p.max_ops;
        let in_region = |m: &Self, v: VertexId| -> bool {
            (m.vert(v).position - p.center).length_squared() <= r2
        };
        let valid_seeds: Option<Vec<VertexId>> = seeds.map(|sv| {
            let mut out: Vec<VertexId> = sv
                .iter()
                .copied()
                .filter(|s| {
                    (s.0 as usize) < self.vertices.len()
                        && self.is_vert_live(*s)
                        && in_region(self, *s)
                })
                .collect();
            out.sort_unstable_by_key(|v| v.0);
            out.dedup();
            out
        });

        // ── 1. Fan-triangulate region faces (a quad the brush touches becomes tris) ──
        // Collect ids first; `split_face_between` allocates faces, so never iterate the
        // arena while splitting.
        let mut region_faces: Vec<FaceId> = Vec::new();
        match &valid_seeds {
            None => {
                for fi in 0..self.faces.len() as u32 {
                    let f = FaceId(fi);
                    if !self.is_face_live(f) {
                        continue;
                    }
                    let vs = self.face_vertices(f);
                    if vs.len() > 3 && vs.iter().any(|&v| in_region(self, v)) {
                        region_faces.push(f);
                    }
                }
            }
            Some(sv) => {
                // A face with an in-region corner is reachable through that corner's fan
                // (the corner is a seed by the coverage contract).
                let mut seen_f: std::collections::HashSet<u32> = std::collections::HashSet::new();
                let mut fan = Vec::new();
                for &s in sv {
                    self.vertex_fan_into(s, &mut fan);
                    for &h in &fan {
                        if !self.is_edge_live(h) {
                            continue;
                        }
                        let f = self.he(h).face;
                        if f == NO_FACE || !self.is_face_live(f) || !seen_f.insert(f.0) {
                            continue;
                        }
                        if self.face_vertices(f).len() > 3 {
                            region_faces.push(f);
                        }
                    }
                }
            }
        }
        for f in region_faces {
            if !self.triangulate_face_fan(f, &mut budget, &mut res) {
                return res; // budget hit — valid stop point
            }
        }

        // ── 2. Long-edge queue: split > detail ──
        // Queue holds VERTEX PAIRS, resolved to a live half-edge at pop time — half-edge
        // ids churn under splits/collapses, vertex ids only die at collapse (re-validated).
        // Seeding runs AFTER phase 1 on purpose (both arms): the fan diagonals phase 1
        // just created must be candidates too.
        let mut long_q: VecDeque<(VertexId, VertexId)> = VecDeque::new();
        let mut short_q: VecDeque<(VertexId, VertexId)> = VecDeque::new();
        match &valid_seeds {
            None => {
                for hi in 0..self.half_edges.len() as u32 {
                    let he = HalfEdgeId(hi);
                    if !self.is_edge_live(he) {
                        continue;
                    }
                    // Canonical: visit each undirected edge once.
                    let twin = self.he(he).twin;
                    if twin != NO_TWIN && twin.0 < hi {
                        continue;
                    }
                    let a = self.he_source(he);
                    let b = self.he(he).vertex;
                    if !in_region(self, a) && !in_region(self, b) {
                        continue;
                    }
                    let len = (self.vert(a).position - self.vert(b).position).length();
                    if len > detail {
                        long_q.push_back((a, b));
                    } else if p.collapse && len < min_len {
                        short_q.push_back((a, b));
                    }
                }
            }
            Some(sv) => {
                // Every edge with ≥1 in-region endpoint is outgoing from a seed — the
                // same qualification rule as the whole-mesh arm, gathered in O(region).
                let mut seen_e: std::collections::HashSet<(u32, u32)> =
                    std::collections::HashSet::new();
                let mut fan = Vec::new();
                for &s in sv {
                    if !self.is_vert_live(s) {
                        continue; // phase 1 never frees verts, but stay fail-closed
                    }
                    self.vertex_fan_into(s, &mut fan);
                    for &h in &fan {
                        if !self.is_edge_live(h) {
                            continue;
                        }
                        let b = self.he(h).vertex;
                        if !seen_e.insert((s.0.min(b.0), s.0.max(b.0))) {
                            continue;
                        }
                        let len = (self.vert(s).position - self.vert(b).position).length();
                        if len > detail {
                            long_q.push_back((s, b));
                        } else if p.collapse && len < min_len {
                            short_q.push_back((s, b));
                        }
                    }
                }
            }
        }
        while let Some((a, b)) = long_q.pop_front() {
            if budget == 0 {
                res.budget_hit = true;
                return res;
            }
            if !self.is_vert_live(a) || !self.is_vert_live(b) {
                continue;
            }
            let Some(he) = self.find_edge_between(a, b) else { continue };
            let pa = self.vert(a).position;
            let pb = self.vert(b).position;
            let len = (pa - pb).length();
            if len <= detail {
                continue; // shrunk since seeding (an earlier op moved things)
            }
            // A face adjacent to this edge that is NOT a triangle sits outside the
            // triangulated region — triangulate it first so the split cannot leave a
            // 5-gon behind (the ≤1-ring ripple Blender also has).
            for fh in [he, self.he(he).twin] {
                if fh == NO_TWIN {
                    continue;
                }
                let f = self.he(fh).face;
                if f != NO_FACE && self.is_face_live(f) && self.face_vertices(f).len() > 3 {
                    if !self.triangulate_face_fan(f, &mut budget, &mut res) {
                        return res;
                    }
                }
            }
            let Some(he) = self.find_edge_between(a, b) else { continue };
            // Split at the midpoint; the two adjacent tris became quads — re-triangulate
            // each through the new vert so the region stays all-tris.
            let v = self.split_edge(he, 0.5);
            budget -= 1;
            res.splits += 1;
            res.touched.push(v);
            res.touched.push(a);
            res.touched.push(b);
            let mut diag_to: Vec<VertexId> = Vec::new();
            let mut fan = Vec::new();
            self.vertex_fan_into(v, &mut fan);
            let faces: Vec<FaceId> = fan
                .iter()
                .map(|&h| self.he(h).face)
                .filter(|&f| f != NO_FACE && self.is_face_live(f))
                .collect();
            for f in faces {
                let vs = self.face_vertices(f);
                if vs.len() != 4 {
                    continue;
                }
                let Some(vi) = vs.iter().position(|&x| x == v) else { continue };
                let opp = vs[(vi + 2) % 4];
                if budget == 0 {
                    res.budget_hit = true;
                    return res; // valid: a lone quad in the region is legal geometry
                }
                if self.split_face_between(f, v, opp).is_some() {
                    budget -= 1;
                    res.splits += 1;
                    diag_to.push(opp);
                }
            }
            // Recurse: the two halves (len/2) and the new diagonals — but ONLY while they
            // still touch the brush. Requeueing without the region test let refinement
            // ripple outward across the whole mesh (each split spawns near-detail
            // diagonals that spawn more) — Blender's locality IS this gate: an edge is
            // processed only while it overlaps the brush.
            if len * 0.5 > detail {
                if in_region(self, a) || in_region(self, v) {
                    long_q.push_back((a, v));
                }
                if in_region(self, v) || in_region(self, b) {
                    long_q.push_back((v, b));
                }
            }
            let pv = self.vert(v).position;
            for opp in diag_to {
                if !in_region(self, v) && !in_region(self, opp) {
                    continue;
                }
                let dl = (self.vert(opp).position - pv).length();
                if dl > detail {
                    long_q.push_back((v, opp));
                } else if p.collapse && dl < min_len {
                    short_q.push_back((v, opp));
                }
            }
        }

        // ── 3. Short-edge queue: collapse < 0.4·detail ──
        if p.collapse {
            let mut fan_a = Vec::new();
            let mut fan_b = Vec::new();
            while let Some((a, b)) = short_q.pop_front() {
                if budget == 0 {
                    res.budget_hit = true;
                    break;
                }
                if !self.is_vert_live(a) || !self.is_vert_live(b) || a == b {
                    continue;
                }
                let Some(he) = self.find_edge_between(a, b) else { continue };
                let pa = self.vert(a).position;
                let pb = self.vert(b).position;
                if (pa - pb).length() >= min_len {
                    continue;
                }
                // LINK CONDITION (Dey–Edelsbrunner): the endpoints' common neighbors must
                // be exactly the opposite verts of the adjacent faces (≤ 2 interior, ≤ 1 on
                // a boundary edge). More = the collapse would pinch a bowtie — SKIP, never
                // "fix up" after (fail closed; the manifold gate is the contract).
                let boundary = self.he(he).twin == NO_TWIN;
                self.vertex_fan_into(a, &mut fan_a);
                self.vertex_fan_into(b, &mut fan_b);
                let nbrs_a: std::collections::HashSet<u32> =
                    fan_a.iter().map(|&h| self.he(h).vertex.0).collect();
                let common = fan_b
                    .iter()
                    .map(|&h| self.he(h).vertex.0)
                    .filter(|x| nbrs_a.contains(x))
                    .count();
                if common > if boundary { 1 } else { 2 } {
                    continue;
                }
                let mid = (pa + pb) * 0.5;
                // One region-normals pass runs at the end — skip the per-collapse
                // full-mesh recompute (that is what the `_ex(.., false)` is for).
                if self.merge_vertices_to_ex(&[a, b], mid, false).is_some() {
                    budget -= 1;
                    res.collapses += 1;
                    res.touched.push(a);
                }
            }
        }

        // ── 4. ONE normals pass over everything the refine touched ──
        res.touched.retain(|&v| self.is_vert_live(v));
        res.touched.sort_unstable_by_key(|v| v.0);
        res.touched.dedup();
        if !res.touched.is_empty() {
            self.recompute_normals_region(res.touched.iter().copied());
        }
        res
    }

    /// Find the live half-edge running `a → b` (either direction's canonical rep is fine
    /// for the callers above — they only need SOME live half-edge of the undirected edge).
    fn find_edge_between(&self, a: VertexId, b: VertexId) -> Option<HalfEdgeId> {
        let mut fan = Vec::new();
        self.vertex_fan_into(a, &mut fan);
        fan.into_iter().find(|&h| self.is_edge_live(h) && self.he(h).vertex == b)
    }

    /// Fan-triangulate one face in place via repeated `split_face_between` (stable ids —
    /// never a rebuild). Returns false when the budget ran out (mesh stays valid: a
    /// partially fanned face is just a smaller n-gon).
    fn triangulate_face_fan(
        &mut self,
        f: FaceId,
        budget: &mut usize,
        res: &mut DyntopoResult,
    ) -> bool {
        let mut cur = f;
        loop {
            if !self.is_face_live(cur) {
                return true;
            }
            let vs = self.face_vertices(cur);
            if vs.len() <= 3 {
                return true;
            }
            if *budget == 0 {
                res.budget_hit = true;
                return false;
            }
            // Cuts the tri (vs[0], vs[1], vs[2]) off as `cur`; the remainder is returned.
            let Some(rest) = self.split_face_between(cur, vs[0], vs[2]) else {
                return true; // degenerate ring — leave it; validate() owns that verdict
            };
            *budget -= 1;
            res.triangulated += 1;
            for &v in &vs {
                res.touched.push(v);
            }
            cur = rest;
        }
    }
}

#[cfg(test)]
mod dyntopo_tests {
    use super::*;

    fn live_vert_count(m: &HalfEdgeMesh) -> usize {
        (0..m.vertices.len() as u32)
            .filter(|&i| m.is_vert_live(VertexId(i)))
            .count()
    }

    /// Max live edge length among edges with BOTH endpoints inside the sphere.
    fn max_region_edge(m: &HalfEdgeMesh, center: Vec3, radius: f32) -> f32 {
        let r2 = radius * radius;
        let mut mx = 0.0f32;
        for hi in 0..m.half_edges.len() as u32 {
            let he = HalfEdgeId(hi);
            if !m.is_edge_live(he) {
                continue;
            }
            let a = m.vert(m.he_source(he)).position;
            let b = m.vert(m.he(he).vertex).position;
            if (a - center).length_squared() <= r2 && (b - center).length_squared() <= r2 {
                mx = mx.max((a - b).length());
            }
        }
        mx
    }

    /// Subdivide refines the brush region to ≤ detail while FAR geometry keeps its exact
    /// ids + positions and far faces stay quads (the Blender local-refinement contract —
    /// the whole reason dyntopo is not a global remesh).
    #[test]
    fn subdivide_refines_region_and_preserves_far_geometry() {
        let mut m = HalfEdgeMesh::new_plane(8.0, 8.0, 8, 8); // 1×1 quads, verts at ±4
        let center = Vec3::ZERO;
        let radius = 1.2;
        // Record every far vert (beyond the ≤1-ring ripple) before refining.
        let far: Vec<(u32, Vec3)> = (0..m.vertices.len() as u32)
            .filter(|&i| m.is_vert_live(VertexId(i)))
            .map(|i| (i, m.vert(VertexId(i)).position))
            .filter(|(_, p)| (*p - center).length() > radius + 2.0)
            .collect();
        assert!(!far.is_empty(), "fixture must have far verts");
        let res = m.dyntopo_refine_region(&DyntopoParams {
            center,
            radius,
            detail_len: 0.3,
            max_ops: 100_000,
            collapse: false,
        });
        assert!(!res.budget_hit, "generous budget must finish");
        assert!(res.splits > 0, "1.0 edges under detail 0.3 must split");
        m.is_manifold().expect("refined plane must stay manifold");
        let mx = max_region_edge(&m, center, radius);
        assert!(
            mx <= 0.3 * 1.001,
            "all fully-in-region edges must be ≤ detail (max {mx})"
        );
        for (i, p) in far {
            let v = VertexId(i);
            assert!(m.is_vert_live(v), "far vert {i} must survive untouched");
            assert_eq!(m.vert(v).position, p, "far vert {i} must not move");
        }
        // A face whose verts are all far must still be a QUAD (no global triangulation).
        let mut far_quads = 0;
        for fi in 0..m.faces.len() as u32 {
            let f = FaceId(fi);
            if !m.is_face_live(f) {
                continue;
            }
            let vs = m.face_vertices(f);
            if vs
                .iter()
                .all(|&v| (m.vert(v).position - center).length() > radius + 2.0)
            {
                assert_eq!(vs.len(), 4, "far face must stay a quad");
                far_quads += 1;
            }
        }
        assert!(far_quads > 0, "fixture must exercise far quads");
    }

    /// Collapse coarsens a too-dense region and the result stays manifold.
    #[test]
    fn collapse_coarsens_dense_region_manifold() {
        let mut m = HalfEdgeMesh::new_plane(4.0, 4.0, 16, 16); // 0.25 cells
        let before = live_vert_count(&m);
        let res = m.dyntopo_refine_region(&DyntopoParams {
            center: Vec3::ZERO,
            radius: 1.0,
            detail_len: 0.8, // min_len 0.32 > 0.25 cell → collapses fire
            max_ops: 100_000,
            collapse: true,
        });
        assert!(res.collapses > 0, "0.25 edges under min_len 0.32 must collapse");
        assert!(
            live_vert_count(&m) < before,
            "collapse must remove verts ({before} before)"
        );
        m.is_manifold().expect("collapsed plane must stay manifold");
    }

    /// A starved budget stops cleanly: honest `budget_hit`, mesh valid at the stop point.
    #[test]
    fn budget_aborts_at_a_valid_mesh() {
        let mut m = HalfEdgeMesh::new_plane(8.0, 8.0, 8, 8);
        let res = m.dyntopo_refine_region(&DyntopoParams {
            center: Vec3::ZERO,
            radius: 1.2,
            detail_len: 0.3,
            max_ops: 5,
            collapse: true,
        });
        assert!(res.budget_hit, "5 ops cannot finish this region");
        m.is_manifold().expect("budget abort must leave a valid mesh");
    }

    /// All live verts inside the sphere — the coverage contract a spatial index supplies.
    fn region_verts(m: &HalfEdgeMesh, center: Vec3, radius: f32) -> Vec<VertexId> {
        let r2 = radius * radius;
        (0..m.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| m.is_vert_live(v))
            .filter(|&v| (m.vert(v).position - center).length_squared() <= r2)
            .collect()
    }

    /// The seeded gather (stroke-path spatial index) must honor the same contract as the
    /// whole-mesh scan: region refined to ≤ detail, far geometry bit-identical, manifold —
    /// and a second seeded pass over refined ground is a no-op.
    #[test]
    fn seeded_gather_matches_contract_and_noops() {
        let center = Vec3::ZERO;
        let radius = 1.2;
        let p = DyntopoParams {
            center,
            radius,
            detail_len: 0.3,
            max_ops: 100_000,
            collapse: false,
        };
        let mut m = HalfEdgeMesh::new_plane(8.0, 8.0, 8, 8);
        let far: Vec<(u32, Vec3)> = (0..m.vertices.len() as u32)
            .filter(|&i| m.is_vert_live(VertexId(i)))
            .map(|i| (i, m.vert(VertexId(i)).position))
            .filter(|(_, p)| (*p - center).length() > radius + 2.0)
            .collect();
        let seeds = region_verts(&m, center, radius);
        assert!(!seeds.is_empty(), "fixture must have region verts");
        let res = m.dyntopo_refine_region_seeded(&p, Some(&seeds));
        assert!(res.splits > 0, "seeded pass must refine the region");
        m.is_manifold().expect("seeded refine must stay manifold");
        let mx = max_region_edge(&m, center, radius);
        assert!(mx <= 0.3 * 1.001, "seeded region edges must be ≤ detail (max {mx})");
        for (i, pos) in far {
            assert_eq!(m.vert(VertexId(i)).position, pos, "far vert {i} must not move");
        }
        // Steady state with a FRESH index (as the stroke loop has next frame): no-op.
        let seeds2 = region_verts(&m, center, radius);
        let again = m.dyntopo_refine_region_seeded(&p, Some(&seeds2));
        assert_eq!(again.splits, 0, "seeded second pass must split nothing");
        assert_eq!(again.triangulated, 0, "seeded second pass must triangulate nothing");
    }

    /// Stale seeds (dead / out-of-range / out-of-region ids) are re-validated, never
    /// trusted — the grid can lag one frame behind a collapse.
    #[test]
    fn seeded_gather_survives_stale_seeds() {
        let mut m = HalfEdgeMesh::new_plane(4.0, 4.0, 16, 16);
        let mut seeds = region_verts(&m, Vec3::ZERO, 1.0);
        seeds.push(VertexId(u32::MAX - 3)); // out of range
        seeds.push(VertexId(0));            // duplicate
        let res = m.dyntopo_refine_region_seeded(
            &DyntopoParams {
                center: Vec3::ZERO,
                radius: 1.0,
                detail_len: 0.8,
                max_ops: 100_000,
                collapse: true,
            },
            Some(&seeds),
        );
        assert!(res.collapses > 0, "0.25 edges under min_len 0.32 must collapse");
        m.is_manifold().expect("stale-seed refine must stay manifold");
    }

    /// Refine twice: the second call over already-refined ground is a NO-OP (steady-state
    /// stroking must cost nothing — the property the stroke loop's perf relies on).
    #[test]
    fn refined_ground_is_a_noop() {
        let mut m = HalfEdgeMesh::new_plane(8.0, 8.0, 8, 8);
        let p = DyntopoParams {
            center: Vec3::ZERO,
            radius: 1.2,
            detail_len: 0.3,
            max_ops: 100_000,
            collapse: false,
        };
        m.dyntopo_refine_region(&p);
        let again = m.dyntopo_refine_region(&p);
        assert_eq!(again.splits, 0, "second pass must split nothing");
        assert_eq!(again.triangulated, 0, "second pass must triangulate nothing");
    }
}
