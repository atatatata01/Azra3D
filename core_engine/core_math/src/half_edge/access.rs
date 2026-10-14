// Element accessors + he_source + bounding box (split from half_edge.rs — pure relocation).

use super::*;

impl HalfEdgeMesh {
    // ============================================================
    // Accessors
    // ============================================================

    #[inline]
    pub fn vert(&self, v: VertexId) -> &HVertex { &self.vertices[v.0 as usize] }
    #[inline]
    pub fn vert_mut(&mut self, v: VertexId) -> &mut HVertex { &mut self.vertices[v.0 as usize] }
    #[inline]
    pub fn he(&self, h: HalfEdgeId) -> &HalfEdge { &self.half_edges[h.0 as usize] }
    #[inline]
    pub fn he_mut(&mut self, h: HalfEdgeId) -> &mut HalfEdge { &mut self.half_edges[h.0 as usize] }
    #[inline]
    pub fn face(&self, f: FaceId) -> &HFace { &self.faces[f.0 as usize] }
    #[inline]
    pub fn face_mut(&mut self, f: FaceId) -> &mut HFace { &mut self.faces[f.0 as usize] }

    /// Source vertex of a half-edge (the vertex it comes FROM).
    /// Cosine of the angle between the two faces sharing this edge, or `None` when the edge
    /// has no dihedral to speak of: it is dead, has no twin, is a boundary, both sides are
    /// the same face, or a face normal is degenerate.
    ///
    /// `1.0` = perfectly flat (coplanar, same winding), `0.0` = a right-angle crease,
    /// `-1.0` = a fold. ONE implementation for the whole codebase: the same test was written
    /// separately in the boolean's coplanar dissolve, the weighted-normal keep-sharp, and the
    /// display-mesh crease projection. New callers (Bevel Smart Select rule 3 -- "all edges
    /// created inside a face with no plane change should be ignored") use this, not a fourth
    /// copy.
    pub fn edge_dihedral_cos(&self, h: HalfEdgeId) -> Option<f32> {
        use crate::half_edge::{NO_FACE, NO_TWIN};
        if !self.is_edge_live(h) {
            return None;
        }
        let e = self.he(h);
        if e.twin == NO_TWIN || e.face == NO_FACE {
            return None;
        }
        let tf = self.he(e.twin).face;
        if tf == NO_FACE || tf == e.face {
            return None;
        }
        let n1 = self.compute_face_normal(e.face).normalize_or_zero();
        let n2 = self.compute_face_normal(tf).normalize_or_zero();
        if n1.length_squared() < 0.25 || n2.length_squared() < 0.25 {
            return None; // degenerate face -- no meaningful angle
        }
        Some(n1.dot(n2).clamp(-1.0, 1.0))
    }

    /// Is this edge FLAT -- its two faces lying in the same plane within `tol_deg`?
    ///
    /// Rule 3 of Bevel Smart Select uses `tol_deg = 0` semantics (exactly coplanar within
    /// float tolerance), which the owner chose over an angle threshold because it can never
    /// skip an edge the user wanted beveled. An edge with no dihedral (boundary, no twin) is
    /// NOT flat -- it is not a candidate for exclusion, and returning true would silently
    /// drop real geometry.
    pub fn edge_is_flat(&self, h: HalfEdgeId, tol_deg: f32) -> bool {
        match self.edge_dihedral_cos(h) {
            Some(c) => c >= tol_deg.max(0.0).to_radians().cos() - 1e-7,
            None => false,
        }
    }

    pub fn he_source(&self, h: HalfEdgeId) -> VertexId {
        // A dead / out-of-range slot has no source. This is NOT corruption — it is a freed slot that a
        // stale consumer id still names (the viewport wire overlay iterating `0..len`, or a `hovered_edge`
        // / `selected_edges` id left over from before a dissolve/delete). Recognize it and stay SILENT.
        // Every freed slot is in the canonical dead state (`free_edge`), so the warning below is now
        // reserved for a genuine LIVE orphan (a relink bug) and means something again instead of crying
        // wolf on every freed slot every frame. This is the architectural root of the owner's
        // `[WARN] he_source: no valid prev or twin` spam.
        if !self.is_edge_live(h) { return VertexId(0); }
        let prev = self.he(h).prev;
        if prev == NO_EDGE || prev.0 as usize >= self.half_edges.len() {
            // Try twin's vertex as fallback (twin goes opposite direction)
            let twin = self.he(h).twin;
            if twin != NO_TWIN && (twin.0 as usize) < self.half_edges.len() {
                return self.he(twin).vertex;
            }
            eprintln!("[WARN] he_source: LIVE orphan half-edge {:?} (no valid prev or twin) — relink bug", h);
            // Same sentinel as the freed-slot path above: a live orphan has NO knowable source, and
            // returning `self.he(h).vertex` (the DESTINATION) handed callers a known-wrong vertex they
            // could not distinguish from a valid answer (findings §1). The warn stays — a live orphan
            // is a relink bug worth hearing about; the freed-slot path stays byte-identical (silent).
            return VertexId(0);
        }
        self.he(prev).vertex
    }

    /// Axis-aligned bounding box.
    pub fn bounding_box(&self) -> (Vec3, Vec3) {
        // Linear `Vec::contains` per vertex → O(V x free-list). Hoisted; see `recompute_normals`.
        // Skipped entirely when nothing has been freed, which is the common case.
        let free_v: std::collections::HashSet<u32> = self.free_verts.iter().copied().collect();
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for (i, v) in self.vertices.iter().enumerate() {
            if !free_v.is_empty() && free_v.contains(&(i as u32)) { continue; }
            min = min.min(v.position);
            max = max.max(v.position);
        }
        (min, max)
    }

}
