// Id-stable polygon-soup rebuild — POLY-STALE-IDS-AFTER-REBUILD (2026-10-13).
//
// A soup rebuild (`from_polygons`) renumbers every vertex / half-edge / face and drops dead
// slots. Every id held OUTSIDE the mesh — the poly selection, hidden set, hover, active vertex,
// the `CustomData` sidecar keyed by half-edge / face id — then silently names a DIFFERENT live
// element (or an out-of-range one: the owner's `vert(301)` crash on a 228-vertex mesh). Pruning
// dead ids stops the crash but not the lie. The cure is for the rebuild itself to keep the ids
// of everything it did not destroy, so no holder ever needs a remap.

use super::*;
use std::collections::HashMap;

impl HalfEdgeMesh {
    /// [`Self::from_polygons_with_materials`] for a soup DERIVED FROM `old` — every element that
    /// survives keeps the id it had in `old`.
    ///
    /// Correspondence is EXACT, supplied by the caller (never guessed from positions):
    /// - `pos_origin[i]` = the `old` vertex that soup position `i` was interned from (`None` =
    ///   a point this op created). Vertex `i` inherits that id.
    /// - `poly_origin[k]` = the `old` face that soup polygon `k` descends from (`None` = new).
    ///   The FIRST polygon in input order naming an old face inherits its id; later ones (the
    ///   other halves of a split) are new.
    /// - A half-edge `a → b` inherits the id of `old`'s live half-edge `origin(a) → origin(b)`.
    ///
    /// Everything without a survivor is appended AFTER `old`'s id range, and every slot in that
    /// range with no survivor (dead before the op, or consumed by it) is retired onto the free
    /// lists. A new element is therefore never handed an id something outside the mesh might
    /// still hold: a stale id names its own element or a dead slot (`is_*_live` → false), never
    /// a stranger.
    ///
    /// A position interned from a slot that is DEAD in `old` is not geometry and is dropped
    /// (it must be unreferenced). Id-indexed soups — the ones that snapshot `old.vertices`
    /// wholesale, see [`Self::id_indexed_origin`] — carry every dead slot's stale position, and
    /// building it would resurrect the slot as a loose vertex.
    pub fn from_polygons_keeping_ids(
        old: &HalfEdgeMesh,
        positions: &[Vec3],
        pos_origin: &[Option<VertexId>],
        faces: &[Vec<usize>],
        poly_origin: &[Option<FaceId>],
        materials: &[u32],
    ) -> Self {
        Self::from_polygons_keeping_ids_mapped(old, positions, pos_origin, faces, poly_origin, materials).0
    }

    /// [`Self::from_polygons_keeping_ids`] that also returns, per soup polygon, the face it
    /// became (`NO_FACE` for a ring shorter than 3, which is skipped). For callers that carry
    /// a per-polygon array (a material plan, a provenance tag) onto the rebuilt mesh.
    pub fn from_polygons_keeping_ids_mapped(
        old: &HalfEdgeMesh,
        positions: &[Vec3],
        pos_origin: &[Option<VertexId>],
        faces: &[Vec<usize>],
        poly_origin: &[Option<FaceId>],
        materials: &[u32],
    ) -> (Self, Vec<FaceId>) {
        debug_assert_eq!(positions.len(), pos_origin.len(), "one origin per soup position");
        debug_assert_eq!(faces.len(), poly_origin.len(), "one origin per soup polygon");
        let dead = |i: usize| matches!(pos_origin[i], Some(v) if !old.is_vert_live(v));
        let compacted;
        let (positions, pos_origin, faces) = if (0..positions.len()).any(dead) {
            let mut referenced = vec![false; positions.len()];
            for &i in faces.iter().flatten() {
                referenced[i] = true;
            }
            let mut remap = vec![usize::MAX; positions.len()];
            let (mut p, mut o) = (Vec::with_capacity(positions.len()), Vec::with_capacity(positions.len()));
            for i in 0..positions.len() {
                if dead(i) {
                    debug_assert!(!referenced[i], "soup ring uses dead vertex slot {:?}", pos_origin[i]);
                    if !referenced[i] {
                        continue;
                    }
                }
                remap[i] = p.len();
                p.push(positions[i]);
                o.push(pos_origin[i]);
            }
            let f: Vec<Vec<usize>> = faces.iter().map(|r| r.iter().map(|&i| remap[i]).collect()).collect();
            compacted = (p, o, f);
            (&compacted.0[..], &compacted.1[..], &compacted.2[..])
        } else {
            (positions, pos_origin, faces)
        };
        let built = Self::from_polygons_with_materials(positions, faces, materials);
        // `built` is fresh (empty free lists), so vertex `i` IS position `i`, and faces were
        // allocated in input order with rings shorter than 3 skipped — the same skip rule as
        // `from_polygons_with_materials`, so the face → polygon index cannot drift.
        let face_origin: Vec<Option<FaceId>> = faces
            .iter()
            .zip(poly_origin)
            .filter(|(ring, _)| ring.len() >= 3)
            .map(|(_, &o)| o)
            .collect();
        debug_assert_eq!(face_origin.len(), built.faces.len());
        let (out, fmap) = built.adopt_ids(old, pos_origin, &face_origin);
        let mut next = 0usize;
        let poly_face = faces
            .iter()
            .map(|ring| {
                if ring.len() < 3 {
                    return NO_FACE;
                }
                next += 1;
                FaceId(fmap[next - 1])
            })
            .collect();
        (out, poly_face)
    }

    /// `pos_origin` for an ID-INDEXED soup: its first `self.vertices.len()` positions are
    /// `self`'s vertex slots (soup index = vertex id, dead slots included), and every later
    /// position is new. The layout of every op that starts from
    /// `self.vertices.iter().map(|v| v.position)` and appends what it creates.
    pub fn id_indexed_origin(&self, n_positions: usize) -> Vec<Option<VertexId>> {
        debug_assert!(n_positions >= self.vertices.len());
        (0..n_positions)
            .map(|i| (i < self.vertices.len()).then_some(VertexId(i as u32)))
            .collect()
    }

    /// Renumber a FRESHLY BUILT mesh (`self`, no dead slots) onto `old`'s ids. `vert_origin` /
    /// `face_origin` are indexed by `self`'s ids. Also returns the face map (`self` face → final
    /// id). See [`Self::from_polygons_keeping_ids`].
    fn adopt_ids(
        self,
        old: &HalfEdgeMesh,
        vert_origin: &[Option<VertexId>],
        face_origin: &[Option<FaceId>],
    ) -> (Self, Vec<u32>) {
        // new id → final id. An origin is adopted only if it is LIVE in `old` and not already
        // claimed; everything else is numbered from the end of `old`'s range.
        fn assign(
            n: usize,
            old_len: usize,
            origin: impl Fn(usize) -> Option<u32>,
            live: impl Fn(u32) -> bool,
        ) -> (Vec<u32>, usize) {
            let mut taken = vec![false; old_len];
            let mut map = vec![u32::MAX; n];
            let mut next = old_len as u32;
            for (i, slot) in map.iter_mut().enumerate() {
                match origin(i) {
                    Some(o) if (o as usize) < old_len && live(o) && !taken[o as usize] => {
                        taken[o as usize] = true;
                        *slot = o;
                    }
                    _ => {
                        *slot = next;
                        next += 1;
                    }
                }
            }
            (map, next as usize)
        }

        let (vmap, v_len) = assign(
            self.vertices.len(),
            old.vertices.len(),
            |i| vert_origin.get(i).copied().flatten().map(|v| v.0),
            |o| old.is_vert_live(VertexId(o)),
        );
        let (fmap, f_len) = assign(
            self.faces.len(),
            old.faces.len(),
            |i| face_origin.get(i).copied().flatten().map(|f| f.0),
            |o| old.is_face_live(FaceId(o)),
        );
        // Half-edges: by directed endpoint pair, in `old`'s numbering.
        let mut old_he: HashMap<(u32, u32), u32> = HashMap::with_capacity(old.half_edges.len());
        for h in 0..old.half_edges.len() as u32 {
            let hid = HalfEdgeId(h);
            if old.is_edge_live(hid) {
                old_he.entry((old.he_source(hid).0, old.he(hid).vertex.0)).or_insert(h);
            }
        }
        let he_origin = |i: usize| -> Option<u32> {
            let hid = HalfEdgeId(i as u32);
            let a = vert_origin.get(self.he_source(hid).0 as usize).copied().flatten()?;
            let b = vert_origin.get(self.he(hid).vertex.0 as usize).copied().flatten()?;
            old_he.get(&(a.0, b.0)).copied()
        };
        let (hmap, h_len) = assign(
            self.half_edges.len(),
            old.half_edges.len(),
            he_origin,
            |o| old.is_edge_live(HalfEdgeId(o)),
        );

        // NO_TWIN == NO_EDGE, so one sentinel test covers twin / next / prev / half_edge.
        let map_he = |h: HalfEdgeId| if h == NO_EDGE { NO_EDGE } else { HalfEdgeId(hmap[h.0 as usize]) };
        let map_f = |f: FaceId| if f == NO_FACE { NO_FACE } else { FaceId(fmap[f.0 as usize]) };

        // Placeholders, then every built element moved to its final slot with its links remapped.
        // An unfilled slot keeps `old`'s position (harmless to any consumer that skips the
        // liveness test) and is retired through the allocator chokepoints below.
        let mut out = HalfEdgeMesh::new();
        out.vertices = (0..v_len)
            .map(|i| HVertex {
                position: old.vertices.get(i).map_or(Vec3::ZERO, |v| v.position),
                normal: Vec3::ZERO,
                half_edge: NO_EDGE,
            })
            .collect();
        out.half_edges = vec![
            HalfEdge { vertex: VertexId(0), twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: NO_FACE };
            h_len
        ];
        out.faces = vec![HFace { half_edge: NO_EDGE, normal: Vec3::ZERO, material_id: 0 }; f_len];
        let mut v_filled = vec![false; v_len];
        let mut h_filled = vec![false; h_len];
        let mut f_filled = vec![false; f_len];
        for (i, v) in self.vertices.iter().enumerate() {
            let t = vmap[i] as usize;
            out.vertices[t] = HVertex { position: v.position, normal: v.normal, half_edge: map_he(v.half_edge) };
            v_filled[t] = true;
        }
        for (i, h) in self.half_edges.iter().enumerate() {
            let t = hmap[i] as usize;
            out.half_edges[t] = HalfEdge {
                vertex: VertexId(vmap[h.vertex.0 as usize]),
                twin: map_he(h.twin),
                next: map_he(h.next),
                prev: map_he(h.prev),
                face: map_f(h.face),
            };
            h_filled[t] = true;
        }
        for (i, f) in self.faces.iter().enumerate() {
            let t = fmap[i] as usize;
            out.faces[t] = HFace { half_edge: map_he(f.half_edge), normal: f.normal, material_id: f.material_id };
            f_filled[t] = true;
        }
        // Keep each surviving element's ANCHOR half-edge too (where its ring / disk walk starts),
        // when that half-edge survived on the same element: a rebuilt-but-unchanged face then
        // walks its corners in the SAME order, so an untouched mesh round-trips exactly. A kept
        // id names the same directed edge by construction, so it is the same corner.
        for t in 0..old.faces.len().min(f_len) {
            let oh = old.faces[t].half_edge;
            if f_filled[t] && (oh.0 as usize) < h_len && h_filled[oh.0 as usize] && out.half_edges[oh.0 as usize].face == FaceId(t as u32) {
                out.faces[t].half_edge = oh;
            }
        }
        for t in 0..old.vertices.len().min(v_len) {
            let oh = old.vertices[t].half_edge;
            if !v_filled[t] || (oh.0 as usize) >= h_len || !h_filled[oh.0 as usize] {
                continue;
            }
            let prev = out.half_edges[oh.0 as usize].prev;
            if prev != NO_EDGE && out.half_edges[prev.0 as usize].vertex == VertexId(t as u32) {
                out.vertices[t].half_edge = oh; // still outgoing from this vertex
            }
        }
        // Retire unfilled slots highest-first, so the allocator's LIFO `pop()` hands back the
        // LOWEST id first — deterministic reuse by later in-place ops.
        for i in (0..v_len).rev().filter(|&i| !v_filled[i]) {
            out.free_vert(VertexId(i as u32));
        }
        for i in (0..h_len).rev().filter(|&i| !h_filled[i]) {
            out.free_edge(HalfEdgeId(i as u32));
        }
        for i in (0..f_len).rev().filter(|&i| !f_filled[i]) {
            out.free_face(FaceId(i as u32));
        }
        // Same growth headroom as `from_polygons_with_materials`.
        out.reserve_elements(
            (out.vertices.len() / 16).max(32),
            (out.half_edges.len() / 16).max(128),
            (out.faces.len() / 16).max(32),
        );
        #[cfg(debug_assertions)]
        out.validate_mesh("from_polygons_keeping_ids");
        (out, fmap)
    }
}

/// Shared oracle for every id-keeping rebuild's test (dice, subdivide, poke, weld, boolean …).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// n×n quad grid in XY, vertex id `j*(n+1)+i` at `(i, j, 0)`, face id `j*n+i`.
    pub(crate) fn grid(n: usize) -> HalfEdgeMesh {
        let mut p = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                p.push(Vec3::new(i as f32, j as f32, 0.0));
            }
        }
        let idx = |i: usize, j: usize| j * (n + 1) + i;
        let mut f = Vec::new();
        for j in 0..n {
            for i in 0..n {
                f.push(vec![idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1)]);
            }
        }
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// The ids an outside holder kept for elements the op did NOT touch (a selected vertex, a
    /// selected edge, a selected face) name the SAME geometry after the op: same position, same
    /// endpoint positions, same corner ring.
    pub(crate) fn assert_ids_name_same_geometry(
        before: &HalfEdgeMesh,
        after: &HalfEdgeMesh,
        verts: &[VertexId],
        edges: &[HalfEdgeId],
        faces: &[FaceId],
    ) {
        let ends = |m: &HalfEdgeMesh, h: HalfEdgeId| (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
        let ring = |m: &HalfEdgeMesh, f: FaceId| m.face_vertices(f).iter().map(|&v| m.vert(v).position).collect::<Vec<_>>();
        for &v in verts {
            assert!(before.is_vert_live(v), "fixture: {v:?} must be live before the op");
            assert!(after.is_vert_live(v), "vertex {v:?} died");
            assert_eq!(before.vert(v).position, after.vert(v).position, "vertex {v:?} names a different point");
        }
        for &h in edges {
            assert!(before.is_edge_live(h), "fixture: {h:?} must be live before the op");
            assert!(after.is_edge_live(h), "half-edge {h:?} died");
            assert_eq!(ends(before, h), ends(after, h), "half-edge {h:?} names a different edge");
        }
        for &f in faces {
            assert!(before.is_face_live(f), "fixture: {f:?} must be live before the op");
            assert!(after.is_face_live(f), "face {f:?} died");
            assert_eq!(ring(before, f), ring(after, f), "face {f:?} names a different polygon");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_support::grid;

    /// The soup snapshot a rebuild op takes: live faces walked in REVERSE id order, vertices
    /// interned in traversal order — so soup indices differ from ids, which is the point.
    #[allow(clippy::type_complexity)]
    fn soup(m: &HalfEdgeMesh) -> (Vec<Vec3>, Vec<Option<VertexId>>, Vec<Vec<usize>>, Vec<Option<FaceId>>) {
        let mut pos = Vec::new();
        let mut origin = Vec::new();
        let mut index: HashMap<u32, usize> = HashMap::new();
        let mut polys = Vec::new();
        let mut porigin = Vec::new();
        for f in (0..m.faces.len() as u32).rev() {
            let fid = FaceId(f);
            if !m.is_face_live(fid) {
                continue;
            }
            let ring = m
                .face_vertices(fid)
                .into_iter()
                .map(|v| {
                    *index.entry(v.0).or_insert_with(|| {
                        pos.push(m.vert(v).position);
                        origin.push(Some(v));
                        pos.len() - 1
                    })
                })
                .collect();
            polys.push(ring);
            porigin.push(Some(fid));
        }
        (pos, origin, polys, porigin)
    }

    #[test]
    fn unchanged_soup_round_trips_every_id() {
        let mut m = grid(3);
        m.delete_vertex(VertexId(5)); // sparse: dead slots must stay dead, not be compacted away
        let (pos, origin, polys, porigin) = soup(&m);
        let r = HalfEdgeMesh::from_polygons_keeping_ids(&m, &pos, &origin, &polys, &porigin, &[]);
        assert!(r.validate().is_ok(), "{:?}", r.validate());
        assert!(!r.is_vert_live(VertexId(5)), "a dead slot must stay dead");
        // Vertex 0 is left LOOSE by the delete (its only face died) — a soup holds faces, so a
        // loose vertex is not in it and is retired, never renumbered onto something else.
        assert!(!r.is_vert_live(VertexId(0)), "a loose vertex is retired");
        let on_face: std::collections::HashSet<VertexId> = origin.iter().flatten().copied().collect();
        for v in 0..m.vertices.len() as u32 {
            let v = VertexId(v);
            assert_eq!(on_face.contains(&v), r.is_vert_live(v), "vertex {v:?} liveness");
            if on_face.contains(&v) {
                assert_eq!(m.vert(v).position, r.vert(v).position, "vertex {v:?} moved");
            }
        }
        for h in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(h);
            if m.is_edge_live(h) {
                assert!(r.is_edge_live(h), "half-edge {h:?} died");
                assert_eq!((m.he_source(h), m.he(h).vertex), (r.he_source(h), r.he(h).vertex), "half-edge {h:?}");
            }
        }
        for fi in 0..m.faces.len() as u32 {
            let fid = FaceId(fi);
            assert_eq!(m.is_face_live(fid), r.is_face_live(fid), "face {fid:?} liveness");
            if m.is_face_live(fid) {
                assert_eq!(m.face_vertices(fid), r.face_vertices(fid), "face {fid:?} ring");
            }
        }
    }

    /// The dice / subdivide / poke soup layout: `old.vertices` snapshotted wholesale, so the soup
    /// carries a dead slot's stale position. It must stay dead — not come back as a loose vertex
    /// (the plain `from_polygons` did exactly that), and not be appended under a new id either.
    #[test]
    fn id_indexed_soup_keeps_dead_slots_dead() {
        let mut m = grid(3);
        m.delete_vertex(VertexId(5));
        let pos: Vec<Vec3> = m.vertices.iter().map(|v| v.position).collect();
        let origin = m.id_indexed_origin(pos.len());
        let (mut polys, mut porigin) = (Vec::new(), Vec::new());
        for f in (0..m.faces.len() as u32).map(FaceId).filter(|&f| m.is_face_live(f)) {
            polys.push(m.face_vertices(f).iter().map(|v| v.0 as usize).collect::<Vec<_>>());
            porigin.push(Some(f));
        }
        let (r, poly_face) =
            HalfEdgeMesh::from_polygons_keeping_ids_mapped(&m, &pos, &origin, &polys, &porigin, &[]);
        assert!(r.validate().is_ok(), "{:?}", r.validate());
        assert!(!r.is_vert_live(VertexId(5)), "a dead slot must stay dead");
        assert_eq!(r.vertices.len(), m.vertices.len(), "nothing appended");
        assert_eq!(poly_face, porigin.iter().map(|o| o.unwrap()).collect::<Vec<_>>(), "unchanged faces map to themselves");
        for v in (0..m.vertices.len() as u32).map(VertexId).filter(|&v| m.is_vert_live(v)) {
            assert!(r.is_vert_live(v));
            assert_eq!(m.vert(v).position, r.vert(v).position, "vertex {v:?} moved");
        }
    }

    #[test]
    fn split_face_keeps_parent_id_and_appends_the_rest() {
        let m = grid(2);
        let (mut pos, mut origin, mut polys, mut porigin) = soup(&m);
        // Split face 0 through a NEW midpoint: [a,b,c,mid] keeps the slot, [c,d,a,mid] appends.
        let k = porigin.iter().position(|&o| o == Some(FaceId(0))).unwrap();
        let ring = polys[k].clone();
        let mid = pos.len();
        pos.push((pos[ring[0]] + pos[ring[2]]) * 0.5);
        origin.push(None);
        polys[k] = vec![ring[0], ring[1], ring[2], mid];
        polys.push(vec![ring[2], ring[3], ring[0], mid]);
        porigin.push(Some(FaceId(0)));
        let r = HalfEdgeMesh::from_polygons_keeping_ids(&m, &pos, &origin, &polys, &porigin, &[]);
        assert!(r.validate().is_ok(), "{:?}", r.validate());
        assert!(r.face_vertices(FaceId(0)).contains(&m.face_vertices(FaceId(0))[1]), "first child inherits the parent id");
        assert_eq!(r.faces.len(), m.faces.len() + 1, "the second child is appended");
        assert_eq!(r.vertices.len(), m.vertices.len() + 1, "the new point is appended");
        let fresh = VertexId(m.vertices.len() as u32);
        assert!((r.vert(fresh).position - pos[mid]).length() < 1e-6);
        for v in 0..m.vertices.len() as u32 {
            assert_eq!(m.vert(VertexId(v)).position, r.vert(VertexId(v)).position);
        }
        // Every boundary edge of the split face survives with its id.
        for h in m.face_half_edges(FaceId(0)) {
            assert!(r.is_edge_live(h));
            assert_eq!((m.he_source(h), m.he(h).vertex), (r.he_source(h), r.he(h).vertex));
        }
    }
}
