//! Copy/Constant attribute propagation over the TOUCHED delta (plan §4.2 Stage A, §6.3 Frame
//! Budget). THIS PHASE: `Copy` and `Constant` ONLY — no weight math (`Lerp`/`Average` are Phase 4).
//!
//! The propagation contract (plan §4.2): an op that CAN preserve a column supplies the
//! correspondence between the elements it CREATED and their SOURCE elements; `apply_delta`
//! propagates each column by its `Interp` over ONLY that touched set — NEVER a full-column sweep
//! (Frame Budget). An op that CANNOT supply a correct correspondence stays `PROPAGATES = false` and
//! declares the drop loudly (its wrapper does not call into here; the column is left untouched and
//! the op's `PROPAGATES` flag tells the UI the layer did not flow).
//!
//! Why a CORNER-level Copy keyed by SOURCE VERTEX is correct for extrude/inset (Stage A): both ops
//! create new corners whose destination vertex is either a SOURCE-face vertex or a positional COPY
//! of one. A per-face-corner UV that the artist authored on the source face is, by construction, the
//! value the trivially-preserving extrude/inset must carry onto the matching new corner. We read the
//! source face's authored `vertex → uv` once (O(source-corners)) and write it onto exactly the new
//! corners the op reports (O(touched)). Pre-existing corners that the op LEFT IN PLACE keep their UV
//! automatically — the column is keyed by `HalfEdgeId`, so an unchanged slot's entry is still valid.

use std::collections::HashMap;

use glam::Vec2;

use crate::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId};
use crate::poly_mesh::{CustomData, Interp};

/// The provenance an op hands to `copy_propagate_corner_uv`: for each NEW corner (a `HalfEdgeId`
/// that did not carry an authored value before the op), the SOURCE vertex whose authored UV it
/// inherits. The op computes this from its OWN deterministic construction (never guessed externally
/// — that is the error-prone post-hoc reconstruction the plan §4.2 forbids).
///
/// `new_corner` is a half-edge id in the POST-op mesh; `source_vertex` is a vertex id in the SOURCE
/// face's pre-op ring (whose UV, if authored, is copied).
#[derive(Clone, Debug, Default)]
pub struct CornerParentMap {
    pub pairs: Vec<(HalfEdgeId, VertexId)>,
}

impl CornerParentMap {
    pub fn new() -> Self { Self { pairs: Vec::new() } }
    pub fn push(&mut self, new_corner: HalfEdgeId, source_vertex: VertexId) {
        self.pairs.push((new_corner, source_vertex));
    }
    pub fn is_empty(&self) -> bool { self.pairs.is_empty() }
}

/// Build the `new_corner → source_vertex` parent-map for a TRIVIALLY-PRESERVING op that
/// (1) appended its new vertices at the tail in source order (`new_vert[i]` is a copy of
/// `src_ring[i]`) and (2) appended its new half-edges at the tail (`HalfEdgeId >= prev_e`). Extrude
/// and inset both have exactly this structure (verified `half_edge.rs` extrude_face/inset_face:
/// `alloc_vertex` for each ring vertex in order, then `alloc_half_edge`/`create_face` for the new
/// faces). This is the op emitting its OWN parent-map from its deterministic construction (plan
/// §4.2) — NOT an external post-hoc guess.
///
/// For each new corner (index `>= prev_e`), its source vertex is:
/// - the corner's destination vertex itself, if that vertex is in the source ring (a reused source
///   vertex, e.g. extrude's side-wall corners that point back at `old_v0/old_v1`), or
/// - the source-ring vertex the new vertex is a copy of (`new_vert[i] → src_ring[i]`), if the
///   destination is one of the appended vertices (index `>= prev_v`).
///
/// Corners whose destination is neither (should not occur for extrude/inset) are skipped — Copy of
/// an unknown source is undefined, so we leave the new corner default rather than guess (No-Bandaids).
pub fn append_tail_parent_map(
    after: &HalfEdgeMesh,
    prev_v: usize,
    prev_e: usize,
    src_ring: &[VertexId],
) -> CornerParentMap {
    // new appended vertex slot (prev_v + i) → its source ring vertex src_ring[i].
    let mut new_vert_src: HashMap<u32, VertexId> = HashMap::new();
    for (i, &sv) in src_ring.iter().enumerate() {
        new_vert_src.insert((prev_v + i) as u32, sv);
    }
    let src_set: std::collections::HashSet<u32> = src_ring.iter().map(|v| v.0).collect();

    let mut pm = CornerParentMap::new();
    for e in prev_e..after.half_edges.len() {
        let corner = HalfEdgeId(e as u32);
        let dst = after.he(corner).vertex;
        if dst.0 == u32::MAX { continue; } // detached/sentinel — never a live corner
        if src_set.contains(&dst.0) {
            pm.push(corner, dst); // points at a reused source-ring vertex
        } else if let Some(&sv) = new_vert_src.get(&dst.0) {
            pm.push(corner, sv); // points at an appended copy of a source-ring vertex
        }
        // else: not part of this op's footprint → skip (no source).
    }
    pm
}

/// Read the source face's authored per-corner UV as a `source_vertex → uv` map (the value each
/// preserved/copied corner of that face should carry). O(source-face corners). A corner with no
/// authored UV contributes nothing (Copy of "unauthored" is "unauthored" — the new corner stays
/// absent from the column, which the bake renders as the default `[0,0]`, identical to the old path).
fn source_face_vertex_uv(
    before: &HalfEdgeMesh,
    cd: &CustomData,
    face: FaceId,
) -> HashMap<VertexId, Vec2> {
    let mut map = HashMap::new();
    for he in before.face_half_edges(face) {
        if let Some(uv) = cd.uv.get(he) {
            // The corner's destination vertex is the natural per-vertex anchor of a per-corner UV
            // on a single face (each face visits each of its vertices exactly once).
            let dst = before.he(he).vertex;
            map.insert(dst, *uv);
        }
    }
    map
}

/// Copy-propagate the per-corner `uv` column onto the op's NEW corners (plan §4.2 Stage A).
///
/// - `before` / `after`: the pre/post-op meshes (the op already ran; `after` carries the new corners).
/// - `src_face`: the face the op consumed (extrude/inset's target) — the UV source.
/// - `parents`: the op's `new_corner → source_vertex` correspondence (O(touched)).
///
/// Mutates `cd` (the post-op sidecar): for each `(new_corner, source_vertex)`, if the source face
/// authored a UV at `source_vertex`, write it at `new_corner`. Bounded to `parents` — NEVER a
/// full-column sweep (Frame Budget §6.3). Pre-existing corners are untouched (their column entries,
/// keyed by stable `HalfEdgeId`, remain valid).
///
/// `debug_assert`s the column's Interp is `Copy` (No-Bandaids: a Constant/absent column must not be
/// silently Copy-propagated; the wrapper only calls this for the `Copy` uv column).
pub fn copy_propagate_corner_uv(
    cd: &mut CustomData,
    before: &HalfEdgeMesh,
    after: &HalfEdgeMesh,
    src_face: FaceId,
    parents: &CornerParentMap,
) {
    debug_assert_eq!(
        cd.uv.interp(),
        Interp::Copy,
        "copy_propagate_corner_uv must only run on a Copy-interp uv column"
    );
    if cd.uv.is_empty() || parents.is_empty() {
        return; // nothing authored, or no new corners → nothing to copy (the trivial fast path).
    }
    let src = source_face_vertex_uv(before, cd, src_face);
    if src.is_empty() {
        return; // the source face carried no authored UV → new corners stay default (old behaviour).
    }
    for &(new_corner, source_vertex) in &parents.pairs {
        // Guard: the new corner must be a live slot in the post-op mesh (No-Bandaids — never write
        // a stale id). A `debug_assert` here would fire on a wrong op-supplied parent-map.
        debug_assert!(
            (new_corner.0 as usize) < after.half_edges.len(),
            "parent-map references a non-existent post-op corner {:?}",
            new_corner
        );
        if let Some(uv) = src.get(&source_vertex) {
            cd.uv.set(new_corner, *uv);
        }
    }
}

/// Copy-propagate UV for `inset_face` (plan §4.2 Stage A). Inset is the FREE-LIST case: it
/// `delete_face`s the source face (freeing its half-edge slots onto the private free-list) and the
/// new ring/inner faces REUSE those freed slots — so the source face's old per-corner UV entries
/// become STALE (their `HalfEdgeId` now belongs to a different corner) and MUST be cleared, not left
/// to silently corrupt (No-Bandaids: this is the exact "silent attribute loss" failure the plan
/// §4.2 / §2 warns about). After clearing, the inset region's corners are rebuilt from the source
/// face's authored `vertex → uv`: a corner inherits the UV of the source-ring vertex it (or its
/// inset copy `new_vert[i]`) corresponds to.
///
/// `src_face_corners` are the source face's PRE-op half-edge ids (the entries to clear).
/// `inset_corners` are the POST-op half-edge ids of the new ring + inner faces (the touched set —
/// the op supplies them from its own construction; bounded, never a full-column sweep).
pub fn copy_propagate_inset_uv(
    cd: &mut CustomData,
    before: &HalfEdgeMesh,
    after: &HalfEdgeMesh,
    src_face: FaceId,
    src_ring: &[VertexId],
    prev_v: usize,
    src_face_corners: &[HalfEdgeId],
    inset_corners: &[HalfEdgeId],
) {
    debug_assert_eq!(
        cd.uv.interp(),
        Interp::Copy,
        "copy_propagate_inset_uv must only run on a Copy-interp uv column"
    );
    // Read the source face's authored UV BEFORE clearing (the values to redistribute).
    let src = source_face_vertex_uv(before, cd, src_face);

    // Clear the source face's stale corner entries (their slots are reused by the inset geometry).
    for &he in src_face_corners {
        cd.uv.remove(he);
    }
    if src.is_empty() {
        return; // source face had no authored UV → nothing to redistribute (old default behaviour).
    }

    // new appended inset vertex (prev_v + i) → its source-ring vertex.
    let mut new_vert_src: HashMap<u32, VertexId> = HashMap::new();
    for (i, &sv) in src_ring.iter().enumerate() {
        new_vert_src.insert((prev_v + i) as u32, sv);
    }
    let src_set: std::collections::HashSet<u32> = src_ring.iter().map(|v| v.0).collect();

    for &corner in inset_corners {
        debug_assert!(
            (corner.0 as usize) < after.half_edges.len(),
            "inset parent-map references a non-existent post-op corner {:?}",
            corner
        );
        let dst = after.he(corner).vertex;
        let source_vertex = if src_set.contains(&dst.0) {
            Some(dst)
        } else {
            new_vert_src.get(&dst.0).copied()
        };
        if let Some(sv) = source_vertex {
            if let Some(uv) = src.get(&sv) {
                cd.uv.set(corner, *uv);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use crate::half_edge::HalfEdgeMesh;

    #[test]
    fn copy_propagate_writes_only_touched_new_corners() {
        let before = HalfEdgeMesh::new_box(Vec3::ONE);
        let after = before.clone(); // same topology — we only check the column write logic here.
        let mut cd = CustomData::new();

        // Author UV on face 0's corners (keyed by their destination vertex).
        let f0_edges = before.face_half_edges(FaceId(0));
        for (i, &he) in f0_edges.iter().enumerate() {
            cd.uv.set(he, Vec2::new(i as f32 * 0.1, 0.0));
        }
        let authored_before = cd.uv.len();

        // A parent-map naming ONE new corner inheriting face-0 vertex 0's UV.
        let v0 = before.he(f0_edges[0]).vertex;
        let want = *cd.uv.get(f0_edges[0]).unwrap();
        let new_corner = HalfEdgeId((after.half_edges.len() - 1) as u32);
        let mut parents = CornerParentMap::new();
        parents.push(new_corner, v0);

        copy_propagate_corner_uv(&mut cd, &before, &after, FaceId(0), &parents);

        // Exactly one new entry, equal to the source corner's UV; the originals are untouched.
        assert_eq!(cd.uv.len(), authored_before + 1, "only the touched new corner gains a UV");
        assert_eq!(*cd.uv.get(new_corner).unwrap(), want, "new corner copies its source vertex UV");
    }

    #[test]
    fn copy_propagate_noop_when_source_unauthored() {
        let before = HalfEdgeMesh::new_box(Vec3::ONE);
        let after = before.clone();
        let mut cd = CustomData::new();
        // Author UV on face 1 ONLY; propagate from face 0 (unauthored) → no writes.
        let f1 = before.face_half_edges(FaceId(1));
        cd.uv.set(f1[0], Vec2::new(0.5, 0.5));
        let before_len = cd.uv.len();

        let mut parents = CornerParentMap::new();
        parents.push(HalfEdgeId((after.half_edges.len() - 1) as u32), before.he(before.face_half_edges(FaceId(0))[0]).vertex);
        copy_propagate_corner_uv(&mut cd, &before, &after, FaceId(0), &parents);
        assert_eq!(cd.uv.len(), before_len, "copying from an unauthored source face writes nothing");
    }
}
