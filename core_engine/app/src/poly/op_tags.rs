//! Bevel **Smart Select** provenance tags — the ONE place the destructive poly ops decide
//! *which* of the geometry they just built is recorded as "created by an operation".
//!
//! Tags live in the runtime sidecar `CustomData::op_created` (`core_math/src/poly_mesh.rs`),
//! keyed per body in `cache.poly_custom_data`, and are consumed by the Bevel modifier's Smart
//! Select in `poly/mesh_sync.rs`. The modifier runs a GEOMETRY rule FIRST (skip edges whose
//! dihedral is tangent-continuous), then tags ADD exclusions on top — so an op should tag ONLY
//! what geometry cannot already infer. See `docs/planning/poly/BEVEL.md` §6 and
//! `docs/planning/poly/SIDECAR_SURVIVES_BOOLEAN.md`.
//!
//! Everything here is a pure function over a `HalfEdgeMesh` so the install sites stay thin and
//! the rules are unit-testable without an `AzraApp`/GPU.

use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE};
use core_math::poly_mesh::CustomData;

/// Carry a sidecar across ONE destructive-op install — the whole `CustomData` policy, in one
/// pure function.
///
/// `base` is the sidecar as it stood BEFORE the op (keyed onto `old`); `old`/`new` are the
/// meshes either side of the op; `created` are the half-edges this op just built, in `new`'s
/// numbering.
///
/// An op that REBUILDS the mesh renumbers every id, so an earlier operation's `op_created` bits
/// would point at unrelated edges of the new mesh: the old tags are lost AND fresh edges are
/// mis-tagged (the owner's "bevel another edge first and the tags break"). Positions survive
/// renumbering, so the base is re-keyed with `remap_by_position` FIRST — the same contract as
/// the boolean install — and only then are this op's own created edges stamped on top. `None`
/// means "no sidecar and nothing created": the caller removes the entry rather than leaving
/// stale keys behind.
///
/// **Do NOT use this for an op that only ALLOCATES** (extrude): remapping by position there
/// would DROP the tags on every vertex the op moved, which is strictly worse than the identity
/// the ids already give you. Stamp directly in that case.
pub(crate) fn carry_op_tags(
    base: Option<&CustomData>,
    old: &HalfEdgeMesh,
    new: &HalfEdgeMesh,
    created: &[HalfEdgeId],
) -> Option<CustomData> {
    let mut out = base.map(|cd| cd.remap_by_position(old, new));
    if !created.is_empty() {
        let cd = out.get_or_insert_with(Default::default);
        for &he in created {
            cd.op_created.set(he, true);
        }
    }
    out
}

/// Face ids live in `new` but NOT live in `old`.
///
/// EXACT for an operation that only ALLOCATES faces (`extrude_region` / `extrude_region_volume`
/// never call `delete_face`): a recycled free slot was not live before, so it is still correctly
/// reported as created. Do NOT use it after an op that deletes and re-creates faces — there the
/// recycled ids are indistinguishable, and the op must report its own created set instead.
pub(crate) fn faces_created(old: &HalfEdgeMesh, new: &HalfEdgeMesh) -> Vec<u32> {
    let live = |m: &HalfEdgeMesh, f: u32| -> bool {
        (f as usize) < m.faces.len() && m.face(FaceId(f)).half_edge != NO_EDGE
    };
    (0..new.faces.len() as u32)
        .filter(|&f| live(new, f) && !live(old, f))
        .collect()
}

/// Every live half-edge belonging to `faces`.
pub(crate) fn edges_of_faces(mesh: &HalfEdgeMesh, faces: &[u32]) -> Vec<HalfEdgeId> {
    let mut out = Vec::new();
    for &f in faces {
        let fid = FaceId(f);
        if (f as usize) >= mesh.faces.len() || mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        for he in mesh.face_half_edges(fid) {
            if mesh.is_edge_live(he) {
                out.push(he);
            }
        }
    }
    out.sort_unstable_by_key(|h| h.0);
    out.dedup();
    out
}

/// Every live half-edge with at least ONE endpoint among `verts`.
///
/// This is how an INSET reports what it created: `inset_region` allocates exactly one new vertex
/// per region-boundary vertex, so "incident to a new vertex" is precisely the inner ring plus the
/// radial rim edges — the geometry that did not exist before. The region's OUTER boundary edge
/// (both endpoints original) is deliberately NOT matched: it is the same edge, in the same place,
/// that the user already had, and it is usually a real crease (the cube edge an inset top face
/// still sits inside). Same lesson as the bevel's end arcs — tag what geometry cannot infer, not
/// everything the op's faces happen to touch.
pub(crate) fn edges_incident_to_verts(mesh: &HalfEdgeMesh, verts: &[u32]) -> Vec<HalfEdgeId> {
    use std::collections::HashSet;
    let set: HashSet<u32> = verts.iter().copied().collect();
    if set.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        let (a, b) = (mesh.he_source(h).0, mesh.he(h).vertex.0);
        if set.contains(&a) || set.contains(&b) {
            out.push(h);
        }
    }
    out
}

/// Keep only the edges whose faces deviate by LESS than `max_deg` — i.e. the nearly tangent
/// (flush / coplanar) ones.
///
/// `edge_dihedral_cos` returns `dot(n1, n2)`, so `cos ≈ 1` is coplanar and `cos ≈ 0` is a 90°
/// crease; "below `max_deg`" therefore means `cos > cos(max_deg)`. Edges with no dihedral
/// (boundary, no twin) are DROPPED — fail closed: an unknown angle never earns an exclusion.
///
/// **Exactly-coplanar edges ARE kept here, on purpose.** The Bevel modifier's geometry rule
/// deliberately does NOT skip coplanar edges (a knife cut is coplanar and must still bevel), so
/// coplanar seams can only be excluded by PROVENANCE. An extrude-created edge at ~0° is exactly
/// such a case and is what this filter is for — do not "fix" it to match the modifier's rule.
pub(crate) fn retain_near_tangent(
    mesh: &HalfEdgeMesh,
    edges: &[HalfEdgeId],
    max_deg: f32,
) -> Vec<HalfEdgeId> {
    let cos_t = max_deg.to_radians().cos();
    edges
        .iter()
        .copied()
        .filter(|&h| matches!(mesh.edge_dihedral_cos(h), Some(c) if c > cos_t))
        .collect()
}

/// Every live half-edge with BOTH endpoints on the plane `{origin, normal}` (within `eps`).
///
/// This is the Symmetrize / Cut-Half WELD SEAM: the ring where the mirrored half meets the
/// original (owner 2026-08-05: "symmetrize middle line should be tag to no bevel"). It is a
/// manufacturing artifact of the mirror, not a corner the user modelled, and it is usually
/// exactly COPLANAR — geometrically indistinguishable from a knife cut, so only provenance can
/// exclude it. Same argument as [`retain_near_tangent`], different operation.
///
/// BOTH endpoints must be on the plane. An edge with only one endpoint on it merely *touches*
/// the seam while running away into the surface — beveling that one is correct.
pub(crate) fn edges_on_plane(
    mesh: &HalfEdgeMesh,
    origin: glam::Vec3,
    normal: glam::Vec3,
    eps: f32,
) -> Vec<HalfEdgeId> {
    let n = normal.normalize_or_zero();
    if n.length_squared() < 0.5 {
        return Vec::new(); // degenerate plane — tag nothing rather than tag everything
    }
    let on = |p: glam::Vec3| (p - origin).dot(n).abs() <= eps;
    (0..mesh.half_edges.len() as u32)
        .map(HalfEdgeId)
        .filter(|&h| mesh.is_edge_live(h))
        .filter(|&h| on(mesh.vert(mesh.he_source(h)).position) && on(mesh.vert(mesh.he(h).vertex).position))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn cube() -> HalfEdgeMesh {
        HalfEdgeMesh::new_box(Vec3::splat(2.0))
    }

    /// A 90° cube edge is a CREASE, not a tangent seam — the 20° filter must reject it, or an
    /// ordinary extrude would tag its own walls and stop them beveling.
    #[test]
    fn near_tangent_rejects_a_ninety_degree_crease() {
        let m = cube();
        let all: Vec<HalfEdgeId> = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h))
            .collect();
        assert!(!all.is_empty());
        assert!(
            retain_near_tangent(&m, &all, 20.0).is_empty(),
            "no cube edge is within 20° of coplanar"
        );
    }

    /// A knife-style coplanar split gives cos ≈ 1, which the filter MUST keep (provenance, not
    /// geometry, is what separates it from a knife cut — see the doc comment).
    #[test]
    fn near_tangent_keeps_a_coplanar_seam() {
        // Two coplanar quads sharing an edge.
        let p = vec![
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(-1.0, 0.0, 1.0),
        ];
        let f: Vec<Vec<usize>> = vec![vec![0, 1, 4, 5], vec![1, 2, 3, 4]];
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        let all: Vec<HalfEdgeId> = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h))
            .collect();
        let kept = retain_near_tangent(&m, &all, 20.0);
        assert_eq!(kept.len(), 2, "exactly the shared edge's two half-edges");
    }
}
