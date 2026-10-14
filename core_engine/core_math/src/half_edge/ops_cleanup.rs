//! Blender **Mesh > Clean Up** parity ops.
//!
//! AZRA's Mesh Cleanup grew out of the boolean repair path, so it shipped with the passes a
//! boolean result needs (fold flattening, coplanar re-merge, T-junction repair, limited dissolve,
//! collinear-pin collapse) and none of the passes an IMPORTED or hand-edited mesh needs. This
//! module adds the rest of Blender's menu:
//!
//! | Blender `Mesh > Clean Up`  | here                                          |
//! |----------------------------|-----------------------------------------------|
//! | Merge by Distance          | [`HalfEdgeMesh::merge_by_distance_ex`] (ops_dissolve) |
//! | Degenerate Dissolve        | [`HalfEdgeMesh::dissolve_degenerate_ex`]      |
//! | Delete Loose               | [`HalfEdgeMesh::delete_loose`]                |
//! | Limited Dissolve           | [`HalfEdgeMesh::limited_dissolve`] (ops_dissolve) |
//! | Fill Holes                 | [`HalfEdgeMesh::fill_holes`]                  |
//! | Make Planar Faces          | [`HalfEdgeMesh::make_planar_faces`]           |
//! | Split Non-Planar Faces     | [`HalfEdgeMesh::split_non_planar_faces`]      |
//! | Split Concave Faces        | [`HalfEdgeMesh::split_concave_faces`]         |
//!
//! Plus [`HalfEdgeMesh::recalculate_normals_consistent`] — Blender's Shift+N. AZRA's
//! "Recalculate Normals" only ever recomputed normal VECTORS from the existing winding, so a mesh
//! with inconsistently wound faces came out exactly as wrong as it went in (`UNT_SUITE_REPORT.md`
//! recorded it as a visual no-op). Winding is what Shift+N fixes.
//!
//! **Id stability.** The weld-based ops (`dissolve_degenerate`, and `merge_by_distance` they share
//! their implementation with) rebuild through `from_polygons_keeping_ids` (POLY-STALE-IDS,
//! 2026-10-13): everything the weld does not consume keeps its id, a welded cluster keeps its
//! lowest member's. Every other op here is IN-PLACE and preserves ids (CLAUDE.md: prefer
//! ID-preserving ops over polygon-soup rebuilds).

use super::*;
use std::collections::{HashMap, HashSet};

/// What one [`HalfEdgeMesh::delete_loose`] pass removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LooseCounts {
    pub verts: usize,
    pub edges: usize,
    pub faces: usize,
}

impl LooseCounts {
    pub fn total(&self) -> usize {
        self.verts + self.edges + self.faces
    }
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Below this the Newell area of a ring is treated as zero (a needle / collapsed face).
const AREA_EPS: f32 = 1e-12;

impl HalfEdgeMesh {
    // ================================================================================
    // Delete Loose  (Blender `mesh.delete_loose`)
    // ================================================================================

    /// Remove geometry that is not part of a surface: **faces** sharing no edge with another face,
    /// **wire edges** (an edge with no face on either side — what `delete_face_only` leaves), and
    /// **loose vertices** (not used by any live half-edge).
    ///
    /// Run in that order deliberately: deleting an isolated face makes its edges loose, and
    /// deleting a wire edge makes its endpoints loose, so one pass with all three flags cascades
    /// the way the user expects instead of needing three invocations.
    ///
    /// Wire edges are spliced out of their `next`/`prev` cycle before being freed — a wire loop
    /// left by `delete_face_only` keeps that cycle live (see its doc), so freeing a slot without
    /// unlinking it would dangle the surviving half of the same loop.
    pub fn delete_loose(&mut self, verts: bool, edges: bool, faces: bool) -> LooseCounts {
        let mut out = LooseCounts::default();

        if faces {
            let doomed: Vec<FaceId> = (0..self.faces.len() as u32)
                .map(FaceId)
                .filter(|&f| self.is_face_live(f) && self.face(f).half_edge != NO_EDGE)
                .filter(|&f| {
                    self.face_half_edges(f).iter().all(|&h| {
                        let t = self.he(h).twin;
                        // No neighbouring FACE across this edge (no twin at all, or the twin is a
                        // wire / freed half-edge). Blender: "faces not connected to other faces".
                        t == NO_TWIN || !self.is_edge_live(t) || self.he(t).face == NO_FACE
                    })
                })
                .collect();
            for f in doomed {
                self.delete_face(f);
                out.faces += 1;
            }
        }

        if edges {
            let doomed: Vec<HalfEdgeId> = (0..self.half_edges.len() as u32)
                .map(HalfEdgeId)
                .filter(|&h| self.is_edge_live(h) && self.he(h).face == NO_FACE)
                .filter(|&h| {
                    let t = self.he(h).twin;
                    t == NO_TWIN || !self.is_edge_live(t) || self.he(t).face == NO_FACE
                })
                .collect();
            let mut seen_edges: HashSet<u32> = HashSet::new();
            for h in doomed {
                if !self.is_edge_live(h) {
                    continue;
                }
                let t = self.he(h).twin;
                // Count each GEOMETRIC edge once (both halves are freed below).
                let key = if t != NO_TWIN && t.0 < h.0 { t.0 } else { h.0 };
                if seen_edges.insert(key) {
                    out.edges += 1;
                }
                self.unlink_and_free_half_edge(h);
                if t != NO_TWIN && self.is_edge_live(t) {
                    self.unlink_and_free_half_edge(t);
                }
            }
            self.repair_stale_vertex_disks();
        }

        if verts {
            // A vertex is "used" when some live half-edge points AT it. Every corner of a live ring
            // (face loop or wire loop) is the destination of exactly one of that ring's half-edges,
            // so destinations alone cover the whole used set — and unlike `he_source` this never
            // walks a `prev` pointer, which a freed neighbour may have already invalidated.
            let mut used = vec![false; self.vertices.len()];
            for i in 0..self.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !self.is_edge_live(h) {
                    continue;
                }
                let d = self.he(h).vertex.0 as usize;
                if d < used.len() {
                    used[d] = true;
                }
            }
            for i in 0..self.vertices.len() as u32 {
                let v = VertexId(i);
                if !self.is_vert_live(v) || used[i as usize] {
                    continue;
                }
                if self.free_vert(v) {
                    out.verts += 1;
                }
            }
        }

        out
    }

    /// Splice one half-edge out of its `next`/`prev` cycle, detach its twin, then free it.
    /// Keeps a partially-freed wire loop consistent for `validate()`.
    fn unlink_and_free_half_edge(&mut self, h: HalfEdgeId) {
        let (p, n, t) = (self.he(h).prev, self.he(h).next, self.he(h).twin);
        if p != NO_EDGE && p != h && self.is_edge_live(p) {
            self.he_mut(p).next = if n != h { n } else { NO_EDGE };
        }
        if n != NO_EDGE && n != h && self.is_edge_live(n) {
            self.he_mut(n).prev = if p != h { p } else { NO_EDGE };
        }
        if t != NO_TWIN && self.is_edge_live(t) {
            self.he_mut(t).twin = NO_TWIN;
        }
        self.free_edge(h);
    }

    // ================================================================================
    // Degenerate Dissolve  (Blender `mesh.dissolve_degenerate`)
    // ================================================================================

    /// Collapse edges shorter than `threshold` and drop faces whose area collapsed to nothing.
    ///
    /// Distinct from [`merge_by_distance`](Self::merge_by_distance), which welds ANY two vertices
    /// within the threshold whether or not an edge joins them. This one only ever collapses along
    /// an existing edge, so two separate shells that happen to pass close together are left alone —
    /// the same distinction Blender draws between Merge by Distance and Degenerate Dissolve.
    ///
    /// Returns vertices removed + faces removed. Rebuilds — keeping the id of everything it does
    /// not consume — through the weld tail it shares with `merge_by_distance`.
    pub fn dissolve_degenerate(&mut self, threshold: f32) -> usize {
        self.dissolve_degenerate_ex(threshold, &[])
    }

    /// [`Self::dissolve_degenerate`] that refuses to collapse a protected edge.
    /// `protected` is the same undirected endpoint-POSITION currency
    /// `mesh_boolean::CleanupOptions::protected_edges` uses.
    pub fn dissolve_degenerate_ex(&mut self, threshold: f32, protected: &[(Vec3, Vec3)]) -> usize {
        let thr = threshold.max(0.0);
        let (_live, pos, id_to_idx) = super::ops_dissolve::live_vertex_census(self);
        let n = pos.len();
        if n == 0 {
            return 0;
        }
        let mut uf = super::ops_dissolve::WeldUf::new(
            n,
            super::ops_dissolve::protected_index_pairs(&pos, protected),
        );

        let mut any_weld = false;
        if thr > 0.0 {
            let thr2 = thr * thr;
            // Walk FACE RINGS, not the raw half-edge array: that is exactly the set of real edges
            // and it never touches a `prev` pointer.
            for i in 0..self.faces.len() as u32 {
                let f = FaceId(i);
                if !self.is_face_live(f) || self.face(f).half_edge == NO_EDGE {
                    continue;
                }
                let vs = self.face_vertices(f);
                for k in 0..vs.len() {
                    let (a, b) = (vs[k], vs[(k + 1) % vs.len()]);
                    let (Some(&ia), Some(&ib)) = (id_to_idx.get(&a.0), id_to_idx.get(&b.0)) else {
                        continue;
                    };
                    if (pos[ia] - pos[ib]).length_squared() <= thr2 && uf.unite(ia, ib) {
                        any_weld = true;
                    }
                }
            }
        }

        // Blender's degenerate face test is the same threshold applied to area (a face whose two
        // dimensions are both under the threshold is a sliver). thr == 0 still drops EXACTLY zero
        // area, which is the "already collapsed" case.
        let min_area = if thr > 0.0 { thr * thr } else { AREA_EPS };
        let had_zero_area = self.any_zero_area_face(min_area);
        if !any_weld && !had_zero_area {
            return 0;
        }

        let before_v = self.vertex_count();
        let before_f = self.face_count();
        let out = super::ops_dissolve::weld_rebuild(self, &pos, &id_to_idx, &mut uf, min_area);
        if !out.validate().is_ok() {
            return 0;
        }
        if out.vertex_count() >= before_v && out.face_count() >= before_f {
            return 0;
        }
        *self = out;
        before_v.saturating_sub(self.vertex_count()) + before_f.saturating_sub(self.face_count())
    }

    fn any_zero_area_face(&self, min_area: f32) -> bool {
        (0..self.faces.len() as u32).map(FaceId).any(|f| {
            if !self.is_face_live(f) || self.face(f).half_edge == NO_EDGE {
                return false;
            }
            let pts: Vec<Vec3> = self
                .face_vertices(f)
                .iter()
                .map(|&v| self.vert(v).position)
                .collect();
            pts.len() >= 3 && newell_normal(&pts).length() * 0.5 <= min_area
        })
    }

    // ================================================================================
    // Fill Holes  (Blender `mesh.fill_holes`)
    // ================================================================================

    /// Every boundary loop of the mesh, as ordered runs of boundary half-edges (each run in its
    /// own faces' winding direction).
    ///
    /// Unlike [`region_boundary_loops`](Self::region_boundary_loops) — which refuses the WHOLE
    /// region when any vertex is pinched — an ambiguous component here is simply skipped. A
    /// mesh-wide sweep must not lose twelve good holes because a thirteenth is pinched.
    pub fn boundary_loops(&self) -> Vec<Vec<HalfEdgeId>> {
        let mut bnd: Vec<HalfEdgeId> = Vec::new();
        for i in 0..self.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !self.is_edge_live(h) {
                continue;
            }
            if self.he(h).face == NO_FACE || self.he(h).twin != NO_TWIN {
                continue;
            }
            bnd.push(h);
        }
        if bnd.is_empty() {
            return Vec::new();
        }
        // source vertex -> the boundary half-edge LEAVING it. A duplicate source is a pinch: that
        // vertex has no unambiguous continuation, so any walk touching it is abandoned.
        let mut out_of: HashMap<u32, HalfEdgeId> = HashMap::with_capacity(bnd.len());
        let mut pinched: HashSet<u32> = HashSet::new();
        for &h in &bnd {
            let s = self.he_source(h).0;
            if out_of.insert(s, h).is_some() {
                pinched.insert(s);
            }
        }
        let mut visited: HashSet<u32> = HashSet::new();
        let mut loops: Vec<Vec<HalfEdgeId>> = Vec::new();
        for &start in &bnd {
            if visited.contains(&start.0) {
                continue;
            }
            let mut chain: Vec<HalfEdgeId> = Vec::new();
            let mut local: HashSet<u32> = HashSet::new();
            let mut cur = start;
            let mut closed = false;
            loop {
                if !local.insert(cur.0) {
                    break; // re-entered this walk without closing — not a simple loop
                }
                chain.push(cur);
                let dst = self.he(cur).vertex.0;
                if pinched.contains(&dst) {
                    break;
                }
                match out_of.get(&dst) {
                    Some(&nx) if nx == start => {
                        closed = true;
                        break;
                    }
                    Some(&nx) => cur = nx,
                    None => break,
                }
            }
            // The successor is a pure function of the half-edge, so every edge on a failed walk
            // fails from any start — marking them visited cannot hide a good loop.
            visited.extend(local);
            if closed && chain.len() >= 3 {
                loops.push(chain);
            }
        }
        loops
    }

    /// Cap every boundary loop with at most `max_sides` edges (Blender Fill Holes, default 4).
    /// Returns the number of holes filled.
    ///
    /// The lid is created with the OPPOSITE winding to the loop's own faces, so it twins with the
    /// existing boundary instead of duplicating it, and inherits the material of the face across
    /// the first boundary edge rather than silently reverting to material 0.
    pub fn fill_holes(&mut self, max_sides: usize) -> usize {
        if max_sides < 3 {
            return 0;
        }
        let loops = self.boundary_loops();
        if loops.is_empty() {
            return 0;
        }
        let mut filled = 0usize;
        for chain in loops {
            if chain.len() > max_sides {
                continue;
            }
            if chain.iter().any(|&h| !self.is_edge_live(h) || self.he(h).twin != NO_TWIN) {
                continue; // an earlier fill already closed this edge
            }
            let mut ring: Vec<VertexId> = chain.iter().map(|&h| self.he_source(h)).collect();
            let mut seen: HashSet<u32> = HashSet::new();
            if !ring.iter().all(|v| seen.insert(v.0)) {
                continue; // repeated corner — the lid would be a bowtie
            }
            ring.reverse();
            let mat = self.face(self.he(chain[0]).face).material_id;
            if self.create_face_no_normals_with_material(&ring, mat).is_some() {
                filled += 1;
            }
        }
        if filled > 0 {
            self.repair_unpaired_twins();
            self.recompute_normals();
        }
        filled
    }

    // ================================================================================
    // Make Planar Faces  (Blender `mesh.face_make_planar`)
    // ================================================================================

    /// Iteratively flatten non-planar n-gons onto their own best-fit plane.
    ///
    /// `factor` is the per-iteration relaxation (Blender's default 1.0), `iterations` the repeat
    /// count (Blender's default 1). Shared vertices average the corrections of every face they
    /// belong to, so a face does not get flattened at its neighbours' expense. Triangles are
    /// planar by definition and are skipped. Returns how many distinct faces needed correcting.
    pub fn make_planar_faces(&mut self, factor: f32, iterations: u32) -> usize {
        let factor = factor.clamp(0.0, 1.0);
        if factor <= 0.0 || iterations == 0 {
            return 0;
        }
        let mut touched: HashSet<u32> = HashSet::new();
        for _ in 0..iterations {
            let mut delta = vec![Vec3::ZERO; self.vertices.len()];
            let mut count = vec![0u32; self.vertices.len()];
            let mut any_off = false;
            for i in 0..self.faces.len() as u32 {
                let f = FaceId(i);
                if !self.is_face_live(f) || self.face(f).half_edge == NO_EDGE {
                    continue;
                }
                let vs = self.face_vertices(f);
                if vs.len() < 4 {
                    continue;
                }
                let pts: Vec<Vec3> = vs.iter().map(|&v| self.vert(v).position).collect();
                let nrm = newell_normal(&pts).normalize_or_zero();
                if nrm == Vec3::ZERO {
                    continue;
                }
                let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
                // Scale-relative tolerance: a 1 mm bulge is noise on a 10 m wall and a defect on
                // a 5 mm bolt head, so an absolute epsilon would be wrong at one end or the other.
                let radius = pts.iter().map(|p| (*p - c).length()).fold(0.0f32, f32::max);
                let tol = (radius * 1e-4).max(1e-7);
                for (k, &v) in vs.iter().enumerate() {
                    let d = (pts[k] - c).dot(nrm);
                    if d.abs() > tol {
                        any_off = true;
                        touched.insert(f.0);
                    }
                    delta[v.0 as usize] -= nrm * d * factor;
                    count[v.0 as usize] += 1;
                }
            }
            if !any_off {
                break;
            }
            for i in 0..self.vertices.len() {
                if count[i] == 0 {
                    continue;
                }
                let v = VertexId(i as u32);
                if !self.is_vert_live(v) {
                    continue;
                }
                self.vert_mut(v).position += delta[i] / count[i] as f32;
            }
        }
        if !touched.is_empty() {
            self.recompute_normals();
        }
        touched.len()
    }

    // ================================================================================
    // Split Non-Planar / Split Concave  (Blender `mesh.vert_connect_nonplanar` / `_concave`)
    // ================================================================================

    /// Split every n-gon that twists out of its own plane by more than `angle_deg`.
    ///
    /// **Planarity is measured with |cos|, not signed cos.** The obvious test — the angle between
    /// each corner's triangle normal and the face normal — reads a REFLEX corner of a perfectly
    /// FLAT concave n-gon as 180° off, so a signed test would shred flat concave faces that have
    /// nothing wrong with them. Taking the absolute value keeps the reflex sign out of it while
    /// still catching genuine out-of-plane twist, which is what this op is for (concavity is the
    /// OTHER op, below).
    ///
    /// Returns the number of faces created by the splits. In-place: vertex ids are preserved.
    pub fn split_non_planar_faces(&mut self, angle_deg: f32) -> usize {
        let cos_lim = angle_deg.clamp(0.0, 180.0).to_radians().cos().abs();
        let targets: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.face_ring_is_simple(f, 4))
            .filter(|&f| {
                let pts = self.face_points(f);
                let nrm = newell_normal(&pts).normalize_or_zero();
                if nrm == Vec3::ZERO {
                    return false;
                }
                corner_normals(&pts)
                    .iter()
                    .any(|c| *c != Vec3::ZERO && c.dot(nrm).abs() < cos_lim)
            })
            .collect();
        let mut created = 0usize;
        for f in targets {
            created += self.triangulate_face_in_place(f).len().saturating_sub(1);
        }
        if created > 0 {
            self.recompute_normals();
        }
        created
    }

    /// Split every concave n-gon into convex faces.
    ///
    /// The face is ear-clipped (triangles are convex by construction), then adjacent pairs from
    /// that same split are re-joined while the union stays convex AND planar — so a concave
    /// hexagon comes back as a couple of quads rather than four triangles, which is what Blender's
    /// version produces. The re-join is scoped to faces this call created: it can never dissolve a
    /// pre-existing edge the user authored.
    ///
    /// Returns the net number of faces created. In-place: vertex ids are preserved.
    pub fn split_concave_faces(&mut self) -> usize {
        let targets: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.face_ring_is_simple(f, 4))
            .filter(|&f| self.face_is_concave(f))
            .collect();
        if targets.is_empty() {
            return 0;
        }
        let before = self.face_count();
        for f in targets {
            let produced = self.triangulate_face_in_place(f);
            if produced.len() > 1 {
                let scope: HashSet<u32> = produced.iter().map(|f| f.0).collect();
                self.rejoin_convex_planar_pairs(&scope);
            }
        }
        self.recompute_normals();
        self.face_count().saturating_sub(before)
    }

    /// A live face whose ring is a simple polygon (no repeated corner) of at least `min_sides`.
    /// `split_face_between` resolves its two endpoints with `position()` — first match wins — so a
    /// ring that visits a vertex twice would split along the wrong diagonal.
    fn face_ring_is_simple(&self, f: FaceId, min_sides: usize) -> bool {
        if !self.is_face_live(f) || self.face(f).half_edge == NO_EDGE {
            return false;
        }
        let vs = self.face_vertices(f);
        if vs.len() < min_sides {
            return false;
        }
        let mut seen: HashSet<u32> = HashSet::with_capacity(vs.len());
        vs.iter().all(|v| seen.insert(v.0))
    }

    fn face_points(&self, f: FaceId) -> Vec<Vec3> {
        self.face_vertices(f)
            .iter()
            .map(|&v| self.vert(v).position)
            .collect()
    }

    /// True when some corner of the face's ring is reflex in the face's own best-fit plane.
    fn face_is_concave(&self, f: FaceId) -> bool {
        let pts = self.face_points(f);
        let nrm = newell_normal(&pts).normalize_or_zero();
        if nrm == Vec3::ZERO {
            return false;
        }
        ring_is_concave(&pts, nrm)
    }

    /// Ear-clip one face in place, reusing `f` for the first ear. Returns every face the ring now
    /// occupies (`f` first), so the caller can scope follow-up work to exactly what it created.
    ///
    /// Uses the crate's ONE ear-clipper (`poly_mesh::earclip_polygon`, the same triangulation the
    /// display bake and the Triangulate modifier use) and clips whichever of its triangles is an
    /// ear of the current ring. Every triangulation of a simple polygon has at least two ears, so
    /// the search cannot come up empty on well-formed input; if it does, the loop stops and leaves
    /// the remainder as one n-gon rather than mangling it.
    fn triangulate_face_in_place(&mut self, f: FaceId) -> Vec<FaceId> {
        let mut produced = vec![f];
        let mut cur = f;
        // Each split removes one corner, so a ring of n needs at most n-3 splits.
        for _ in 0..self.faces.len().max(4) {
            if !self.face_ring_is_simple(cur, 4) {
                break;
            }
            let vs = self.face_vertices(cur);
            let n = vs.len();
            let pts = self.face_points(cur);
            let nrm = self.compute_face_normal(cur);
            let tris = crate::poly_mesh::earclip_polygon(&pts, nrm);
            let mut ear: Option<usize> = None;
            'outer: for t in &tris {
                let idx = [t[0] as usize, t[1] as usize, t[2] as usize];
                for r in 0..3 {
                    let (x, y, z) = (idx[r], idx[(r + 1) % 3], idx[(r + 2) % 3]);
                    if (x + 1) % n == y && (y + 1) % n == z {
                        ear = Some(y);
                        break 'outer;
                    }
                }
            }
            let Some(k) = ear else { break };
            let prev = vs[(k + n - 1) % n];
            let next = vs[(k + 1) % n];
            match self.split_face_between(cur, prev, next) {
                Some(rest) => {
                    produced.push(rest);
                    cur = rest;
                }
                None => break,
            }
        }
        produced
    }

    /// Greedily dissolve edges INSIDE `scope` while the merged face stays convex and planar.
    /// `scope` holds face ids; an edge is only considered when both its faces are in it.
    fn rejoin_convex_planar_pairs(&mut self, scope: &HashSet<u32>) -> usize {
        let mut live_scope: HashSet<u32> = scope.clone();
        let mut joined = 0usize;
        loop {
            let mut did = false;
            let candidates: Vec<HalfEdgeId> = (0..self.half_edges.len() as u32)
                .map(HalfEdgeId)
                .filter(|&h| self.is_edge_live(h))
                .filter(|&h| {
                    let t = self.he(h).twin;
                    if t == NO_TWIN || !self.is_edge_live(t) {
                        return false;
                    }
                    let (fa, fb) = (self.he(h).face, self.he(t).face);
                    fa != NO_FACE
                        && fb != NO_FACE
                        && fa != fb
                        && live_scope.contains(&fa.0)
                        && live_scope.contains(&fb.0)
                })
                .collect();
            for h in candidates {
                if !self.is_edge_live(h) {
                    continue;
                }
                let Some(ring) = self.merged_ring(h) else { continue };
                let pts: Vec<Vec3> = ring.iter().map(|&v| self.vert(v).position).collect();
                let nrm = newell_normal(&pts).normalize_or_zero();
                if nrm == Vec3::ZERO || ring_is_concave(&pts, nrm) || !ring_is_planar(&pts, nrm) {
                    continue;
                }
                let (fa, fb) = (self.he(h).face, self.he(self.he(h).twin).face);
                if let Some(kept) = self.dissolve_edge(h) {
                    live_scope.remove(&fa.0);
                    live_scope.remove(&fb.0);
                    live_scope.insert(kept.0);
                    joined += 1;
                    did = true;
                    break; // ids shifted — rebuild the candidate list
                }
            }
            if !did {
                break;
            }
        }
        joined
    }

    /// The ring the two faces across `h` would form if `h` were dissolved — computed WITHOUT
    /// mutating, so a merge can be rejected before it happens (there is no cheap undo for
    /// `dissolve_edge`).
    fn merged_ring(&self, h: HalfEdgeId) -> Option<Vec<VertexId>> {
        let t = self.he(h).twin;
        if t == NO_TWIN || !self.is_edge_live(t) {
            return None;
        }
        let (fa, fb) = (self.he(h).face, self.he(t).face);
        if fa == NO_FACE || fb == NO_FACE || fa == fb {
            return None;
        }
        let mut ring: Vec<VertexId> = Vec::new();
        for &start in &[h, t] {
            let mut cur = self.he(start).next;
            let mut guard = 0usize;
            while cur != start {
                ring.push(self.he(cur).vertex);
                cur = self.he(cur).next;
                guard += 1;
                if guard > self.half_edges.len() {
                    return None; // corrupt ring — refuse rather than loop
                }
            }
        }
        if ring.len() < 3 {
            return None;
        }
        let mut seen: HashSet<u32> = HashSet::with_capacity(ring.len());
        if !ring.iter().all(|v| seen.insert(v.0)) {
            return None; // merging would create a repeated corner
        }
        Some(ring)
    }

    // ================================================================================
    // Recalculate Normals  (Blender Shift+N — WINDING, not just the normal vectors)
    // ================================================================================

    /// Make face winding CONSISTENT across every connected shell, and — for shells that are
    /// closed — orient them `outside` (or inside).
    ///
    /// This is what AZRA's "Recalculate Normals" was missing: `recompute_normals` rewrites each
    /// face's normal FROM its winding, so a face wound the wrong way just gets a correctly
    /// computed wrong-way normal.
    ///
    /// **How an inconsistency looks here.** The kernel twins `a→b` with `b→a` only, so two
    /// neighbouring faces that both traverse a shared edge `a→b` cannot be twinned at all — they
    /// show up as two unpaired half-edges with the SAME direction. That duplicate is the signal
    /// this pass keys on; a twin means "already consistent".
    ///
    /// Orientation is only forced on CLOSED shells, where signed volume answers "outside"
    /// unambiguously. An open shell is made self-consistent and left facing the way its seed face
    /// faced — inventing an outside for a surface that has no inside would be a guess.
    ///
    /// Returns the number of faces whose winding was reversed.
    pub fn recalculate_normals_consistent(&mut self, outside: bool) -> usize {
        let nf = self.faces.len();
        if nf == 0 {
            return 0;
        }
        // Directed edge -> faces traversing it in that direction.
        let mut dir: HashMap<(u32, u32), Vec<FaceId>> = HashMap::new();
        let mut rings: Vec<Option<Vec<VertexId>>> = vec![None; nf];
        for i in 0..nf as u32 {
            let f = FaceId(i);
            if !self.is_face_live(f) || self.face(f).half_edge == NO_EDGE {
                continue;
            }
            let vs = self.face_vertices(f);
            if vs.len() < 3 {
                continue;
            }
            for k in 0..vs.len() {
                let (a, b) = (vs[k].0, vs[(k + 1) % vs.len()].0);
                dir.entry((a, b)).or_default().push(f);
            }
            rings[i as usize] = Some(vs);
        }

        let mut visited = vec![false; nf];
        let mut flip = vec![false; nf];
        let mut flipped = 0usize;

        for seed in 0..nf {
            if visited[seed] || rings[seed].is_none() {
                continue;
            }
            let mut component: Vec<usize> = Vec::new();
            let mut queue: Vec<(usize, bool)> = vec![(seed, false)];
            while let Some((fi, fl)) = queue.pop() {
                if visited[fi] {
                    continue;
                }
                visited[fi] = true;
                flip[fi] = fl;
                component.push(fi);
                let Some(vs) = rings[fi].clone() else { continue };
                for k in 0..vs.len() {
                    let (a, b) = (vs[k].0, vs[(k + 1) % vs.len()].0);
                    // Opposite direction on the neighbour = consistent = same flip state.
                    if let Some(list) = dir.get(&(b, a)) {
                        for &g in list {
                            if !visited[g.0 as usize] {
                                queue.push((g.0 as usize, fl));
                            }
                        }
                    }
                    // Same direction on the neighbour = inconsistent = opposite flip state.
                    if let Some(list) = dir.get(&(a, b)) {
                        for &g in list {
                            if g.0 as usize != fi && !visited[g.0 as usize] {
                                queue.push((g.0 as usize, !fl));
                            }
                        }
                    }
                }
            }
            // Global orientation, closed shells only.
            if self.component_is_closed(&component) {
                let vol: f64 = component
                    .iter()
                    .map(|&fi| {
                        let vs = rings[fi].as_ref().unwrap();
                        let mut pts: Vec<Vec3> =
                            vs.iter().map(|&v| self.vert(v).position).collect();
                        if flip[fi] {
                            pts.reverse();
                        }
                        signed_volume_fan(&pts)
                    })
                    .sum();
                let wants_flip = if outside { vol < 0.0 } else { vol > 0.0 };
                if wants_flip {
                    for &fi in &component {
                        flip[fi] = !flip[fi];
                    }
                }
            }
        }

        for fi in 0..nf {
            if !flip[fi] || rings[fi].is_none() {
                continue;
            }
            self.flip_face_winding(FaceId(fi as u32));
            flipped += 1;
        }
        if flipped > 0 {
            // A flip can make two previously same-direction half-edges opposite — i.e. pairable.
            self.repair_unpaired_twins();
        }
        self.recompute_normals();
        flipped
    }

    /// True when no face in `component` has a boundary (unpaired, face-owning) half-edge, i.e. the
    /// shell is watertight and signed volume is meaningful.
    fn component_is_closed(&self, component: &[usize]) -> bool {
        component.iter().all(|&fi| {
            let f = FaceId(fi as u32);
            self.face_half_edges(f)
                .iter()
                .all(|&h| self.he(h).twin != NO_TWIN)
        })
    }
}

/// Per-corner triangle normals of a ring (unit, `ZERO` for a degenerate corner).
fn corner_normals(pts: &[Vec3]) -> Vec<Vec3> {
    let n = pts.len();
    (0..n)
        .map(|k| {
            let prev = pts[(k + n - 1) % n];
            let cur = pts[k];
            let next = pts[(k + 1) % n];
            (cur - prev).cross(next - cur).normalize_or_zero()
        })
        .collect()
}

/// Does the ring turn the wrong way at any corner, seen from `nrm`?
fn ring_is_concave(pts: &[Vec3], nrm: Vec3) -> bool {
    let n = pts.len();
    if n < 4 {
        return false;
    }
    // Scale the reflex threshold with the ring, so float noise on a nearly-straight corner of a
    // large face does not read as concave.
    let scale = pts
        .iter()
        .map(|p| (*p - pts[0]).length())
        .fold(0.0f32, f32::max)
        .max(1e-6);
    let eps = -(scale * scale) * 1e-6;
    (0..n).any(|k| {
        let prev = pts[(k + n - 1) % n];
        let cur = pts[k];
        let next = pts[(k + 1) % n];
        (cur - prev).cross(next - cur).dot(nrm) < eps
    })
}

/// Is every corner of the ring within a scale-relative tolerance of the plane through its centroid?
fn ring_is_planar(pts: &[Vec3], nrm: Vec3) -> bool {
    if pts.is_empty() {
        return true;
    }
    let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
    let radius = pts.iter().map(|p| (*p - c).length()).fold(0.0f32, f32::max);
    let tol = (radius * 1e-4).max(1e-7);
    pts.iter().all(|p| (*p - c).dot(nrm).abs() <= tol)
}

/// Signed volume contribution of one face, fanned from its first corner (÷6 of the scalar triple).
fn signed_volume_fan(pts: &[Vec3]) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    let p0 = pts[0].as_dvec3();
    let mut acc = 0.0f64;
    for k in 1..pts.len() - 1 {
        let a = pts[k].as_dvec3();
        let b = pts[k + 1].as_dvec3();
        acc += p0.dot(a.cross(b));
    }
    acc / 6.0
}

// ================================================================================
// Tests
// ================================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_boolean::boundary_half_edge_count;

    /// Cube corner positions + CCW-outward face rings, as plain soup so a test can corrupt one
    /// ring's winding on purpose.
    fn cube_soup() -> (Vec<Vec3>, Vec<Vec<usize>>) {
        let p = vec![
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let f = vec![
            vec![0, 3, 2, 1], // -Z
            vec![4, 5, 6, 7], // +Z
            vec![0, 1, 5, 4], // -Y
            vec![2, 3, 7, 6], // +Y
            vec![0, 4, 7, 3], // -X
            vec![1, 2, 6, 5], // +X
        ];
        (p, f)
    }

    fn adjacent_face_of(m: &HalfEdgeMesh, f: FaceId) -> FaceId {
        for h in m.face_half_edges(f) {
            let t = m.he(h).twin;
            if t != NO_TWIN {
                return m.he(t).face;
            }
        }
        panic!("no neighbour");
    }

    // ---------------------------------------------------------------- Delete Loose

    #[test]
    fn delete_loose_frees_an_unreferenced_vertex() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let before = m.vertex_count();
        m.alloc_vertex(HVertex {
            position: Vec3::new(9.0, 9.0, 9.0),
            normal: Vec3::ZERO,
            half_edge: NO_EDGE,
        });
        assert_eq!(m.vertex_count(), before + 1);
        let r = m.delete_loose(true, false, false);
        assert_eq!(r.verts, 1, "the orphan must be the only casualty");
        assert_eq!(m.vertex_count(), before);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn delete_loose_frees_a_wire_edge_left_by_delete_face_only() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let f0 = FaceId(0);
        let f1 = adjacent_face_of(&m, f0);
        let mut set = HashSet::new();
        set.insert(f0);
        set.insert(f1);
        m.delete_faces_only(&set);
        let wires = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).face == NO_FACE)
            .filter(|&h| {
                let t = m.he(h).twin;
                t == NO_TWIN || !m.is_edge_live(t) || m.he(t).face == NO_FACE
            })
            .count();
        assert!(wires >= 2, "expected a wire edge (both halves faceless), got {wires}");
        let r = m.delete_loose(false, true, false);
        assert!(r.edges >= 1, "wire edge not collected");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn delete_loose_removes_a_face_that_touches_nothing() {
        // One quad, entirely on its own: every edge is a boundary, so it is "loose" by Blender's
        // definition (no edge shared with another face).
        let mut m = HalfEdgeMesh::from_polygons(
            &[Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y],
            &[vec![0, 1, 2, 3]],
        );
        let r = m.delete_loose(true, true, true);
        assert_eq!(r.faces, 1);
        assert_eq!(m.face_count(), 0);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn delete_loose_leaves_a_healthy_cube_untouched() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let (v, e, f) = (m.vertex_count(), m.edge_count(), m.face_count());
        let r = m.delete_loose(true, true, true);
        assert!(r.is_empty(), "clean cube reported loose geometry: {r:?}");
        assert_eq!((m.vertex_count(), m.edge_count(), m.face_count()), (v, e, f));
    }

    // ---------------------------------------------------------------- Degenerate Dissolve

    /// A quad whose last two corners sit 1e-5 apart — one micro edge, nothing else wrong.
    fn micro_edge_quad() -> (Vec<Vec3>, HalfEdgeMesh) {
        let pts = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(1.0 + 1e-5, 1.0, 0.0),
        ];
        let m = HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3]]);
        (pts, m)
    }

    #[test]
    fn dissolve_degenerate_collapses_a_micro_edge() {
        let (_pts, mut m) = micro_edge_quad();
        assert_eq!(m.vertex_count(), 4);
        let removed = m.dissolve_degenerate(1e-3);
        assert!(removed > 0, "micro edge survived");
        assert_eq!(m.vertex_count(), 3, "quad should collapse to a triangle");
        assert_eq!(m.face_count(), 1);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn dissolve_degenerate_is_a_no_op_on_a_clean_cube() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let sig = m.content_signature();
        assert_eq!(m.dissolve_degenerate(1e-3), 0);
        assert_eq!(m.content_signature(), sig, "no-op must not rebuild");
    }

    #[test]
    fn dissolve_degenerate_respects_a_protected_edge() {
        let (pts, mut m) = micro_edge_quad();
        let protect = vec![(pts[2], pts[3])];
        assert_eq!(
            m.dissolve_degenerate_ex(1e-3, &protect),
            0,
            "a protected edge must never be collapsed"
        );
        assert_eq!(m.vertex_count(), 4);
    }

    // ---------------------------------------------------------------- Merge by Distance

    #[test]
    fn merge_by_distance_refuses_to_fuse_a_protected_edge() {
        let (pts, mut free) = micro_edge_quad();
        assert_eq!(free.merge_by_distance(1e-3), 1, "unprotected weld should fire");

        let (_p2, mut prot) = micro_edge_quad();
        assert_eq!(prot.merge_by_distance_ex(1e-3, &[(pts[2], pts[3])]), 0);
        assert_eq!(prot.vertex_count(), 4);
    }

    // ---------------------------------------------------------------- Fill Holes

    #[test]
    fn fill_holes_caps_a_deleted_cube_face() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        m.delete_face(FaceId(0));
        assert_eq!(m.face_count(), 5);
        assert!(boundary_half_edge_count(&m) > 0, "expected an open rim");
        assert_eq!(m.boundary_loops().len(), 1);
        assert_eq!(m.fill_holes(4), 1);
        assert_eq!(m.face_count(), 6);
        assert_eq!(boundary_half_edge_count(&m), 0, "lid did not twin with the rim");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "{:?}", m.is_manifold());
    }

    #[test]
    fn fill_holes_honours_the_sides_limit() {
        let mut m = HalfEdgeMesh::new_cylinder(1.0, 2.0, 6);
        let cap = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| m.is_face_live(f) && m.face_vertices(f).len() == 6)
            .expect("cylinder cap");
        m.delete_face(cap);
        assert_eq!(m.fill_holes(4), 0, "6-sided hole must be refused at limit 4");
        assert_eq!(m.fill_holes(6), 1);
        assert_eq!(boundary_half_edge_count(&m), 0);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn fill_holes_is_a_no_op_on_a_closed_mesh() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(m.fill_holes(4), 0);
        assert_eq!(m.face_count(), 6);
    }

    // ---------------------------------------------------------------- Make Planar

    #[test]
    fn make_planar_faces_flattens_a_bowed_quad() {
        let pts = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.4),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3]]);
        assert_eq!(m.make_planar_faces(1.0, 30), 1);
        let out = m.face_points(FaceId(0));
        let n = newell_normal(&out).normalize_or_zero();
        let c = out.iter().copied().sum::<Vec3>() / out.len() as f32;
        let worst = out.iter().map(|p| (*p - c).dot(n).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "still bowed: {worst}");
        assert!(m.validate().is_ok());
    }

    #[test]
    fn make_planar_faces_leaves_a_cube_alone() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(m.make_planar_faces(1.0, 10), 0);
    }

    // ---------------------------------------------------------------- Split Non-Planar / Concave

    /// Flat L-shaped hexagon: CONCAVE but perfectly planar.
    fn flat_l_ngon() -> HalfEdgeMesh {
        let pts = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(1.0, 2.0, 0.0),
            Vec3::new(0.0, 2.0, 0.0),
        ];
        HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3, 4, 5]])
    }

    #[test]
    fn split_non_planar_does_not_shred_a_flat_concave_ngon() {
        // The regression the |cos| test exists for: a reflex corner is not out-of-plane twist.
        let mut m = flat_l_ngon();
        assert_eq!(m.split_non_planar_faces(5.0), 0);
        assert_eq!(m.face_count(), 1);
    }

    #[test]
    fn split_non_planar_splits_a_twisted_quad() {
        let pts = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.8),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3]]);
        assert!(m.split_non_planar_faces(5.0) >= 1, "twisted quad not split");
        assert_eq!(m.face_count(), 2);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn split_concave_leaves_only_convex_faces() {
        let mut m = flat_l_ngon();
        assert!(m.split_concave_faces() >= 1, "concave n-gon not split");
        for i in 0..m.faces.len() as u32 {
            let f = FaceId(i);
            if !m.is_face_live(f) || m.face(f).half_edge == NO_EDGE {
                continue;
            }
            assert!(!m.face_is_concave(f), "face {i} is still concave");
        }
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn split_concave_leaves_a_convex_quad_alone() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(m.split_concave_faces(), 0);
        assert_eq!(m.face_count(), 6);
    }

    // ---------------------------------------------------------------- Recalculate Normals

    #[test]
    fn recalculate_normals_repairs_one_reversed_face() {
        let (p, mut f) = cube_soup();
        f[2].reverse(); // -Y face wound inward
        let mut m = HalfEdgeMesh::from_polygons(&p, &f);
        assert!(
            boundary_half_edge_count(&m) > 0,
            "a reversed face cannot twin — expected unpaired half-edges"
        );
        let flipped = m.recalculate_normals_consistent(true);
        assert!(flipped > 0, "nothing was re-wound");
        assert_eq!(boundary_half_edge_count(&m), 0, "shell still open after re-winding");
        assert!(m.is_manifold().is_ok(), "{:?}", m.is_manifold());
        for i in 0..m.faces.len() as u32 {
            let fid = FaceId(i);
            if !m.is_face_live(fid) || m.face(fid).half_edge == NO_EDGE {
                continue;
            }
            let pts = m.face_points(fid);
            let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
            let n = newell_normal(&pts).normalize_or_zero();
            assert!(n.dot(c) > 0.0, "face {i} points inward (n={n:?} c={c:?})");
        }
    }

    #[test]
    fn recalculate_normals_flips_a_fully_inverted_cube_outward() {
        let (p, f) = cube_soup();
        let inverted: Vec<Vec<usize>> =
            f.iter().map(|r| r.iter().rev().copied().collect()).collect();
        let mut m = HalfEdgeMesh::from_polygons(&p, &inverted);
        assert_eq!(boundary_half_edge_count(&m), 0, "inverted cube is still closed");
        assert_eq!(m.recalculate_normals_consistent(true), 6);
        for i in 0..m.faces.len() as u32 {
            let fid = FaceId(i);
            let pts = m.face_points(fid);
            let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
            assert!(newell_normal(&pts).normalize_or_zero().dot(c) > 0.0);
        }
    }

    #[test]
    fn recalculate_normals_leaves_a_good_cube_alone() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(m.recalculate_normals_consistent(true), 0);
    }
}
