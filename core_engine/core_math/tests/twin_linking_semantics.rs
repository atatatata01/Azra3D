//! Pins the EXACT twin-pairing semantics of `HalfEdgeMesh::from_quads`.
//!
//! Written BEFORE replacing the twin-linking hash map with a direct-addressed scheme, so the
//! replacement has something to be bit-identical against. `from_quads` is reached by every mesh
//! constructor in the codebase (`from_mesh`, the voxel remesh, boolean rebuilds, importers), so a
//! silent change in pairing would surface as non-manifold geometry far from the edit.
//!
//! The manifold case is uninteresting — every directed edge has exactly one opposite. What needs
//! pinning is the NON-MANIFOLD case (3+ half-edges on one undirected edge), where the outcome
//! depends on the ORDER the directed edges are visited, and on the fact that the pending-edge
//! insert is `or_insert` (a second identical directed edge is DROPPED, not overwritten).
//!
//! No test covered this before.

use core_math::half_edge::{HalfEdgeMesh, NO_TWIN};
use glam::Vec3;

/// Eight positions; topology is what matters, not geometry.
fn pts(n: usize) -> Vec<Vec3> {
    (0..n).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect()
}

/// On a fresh mesh, half-edge ids are sequential in (quad, corner) order, so the half-edge for
/// corner `i` of quad `q` is `q * 4 + i`. The pinned expectations below rely on that.
fn he(q: usize, i: usize) -> u32 {
    (q * 4 + i) as u32
}

/// THREE faces share the undirected edge {0,1}, visited as (0,1) then (1,0) then (0,1).
///
/// Expected state machine:
/// * k=0  `(0,1)` — nothing pending for `(1,0)` ⇒ park `(0,1) -> he0`
/// * k=4  `(1,0)` — `(0,1)` is parked ⇒ PAIR he4 with he0, and un-park
/// * k=8  `(0,1)` — nothing parked for `(1,0)` ⇒ park again; never claimed ⇒ stays NO_TWIN
#[test]
fn three_faces_on_one_edge_pair_the_first_two_in_visit_order() {
    let quads = vec![
        [0usize, 1, 2, 3], // corner 0 = (0,1)
        [1usize, 0, 4, 5], // corner 0 = (1,0)
        [0usize, 1, 6, 7], // corner 0 = (0,1) again
    ];
    let m = HalfEdgeMesh::from_quads(&pts(8), &quads);

    assert_eq!(
        m.half_edges[he(0, 0) as usize].twin.0,
        he(1, 0),
        "the first (0,1) must pair with the (1,0) that follows it"
    );
    assert_eq!(
        m.half_edges[he(1, 0) as usize].twin.0,
        he(0, 0),
        "pairing must be symmetric"
    );
    assert_eq!(
        m.half_edges[he(2, 0) as usize].twin,
        NO_TWIN,
        "the THIRD half-edge on the edge has no partner left and must stay NO_TWIN"
    );
}

/// Same three faces, but visited as (0,1), (0,1), (1,0) — which exercises the `or_insert` drop.
///
/// Expected:
/// * k=0 `(0,1)` ⇒ park `(0,1) -> he0`
/// * k=4 `(0,1)` ⇒ nothing parked for `(1,0)`; `(0,1)` is ALREADY parked, so `or_insert` DROPS
///   this one. he4 is never parked and never claimed ⇒ NO_TWIN.
/// * k=8 `(1,0)` ⇒ claims the parked he0 ⇒ PAIR he8 with he0
///
/// So the middle half-edge is the orphan here, whereas in the previous test the last one was.
/// That asymmetry is the whole reason this file exists.
#[test]
fn duplicate_directed_edge_is_dropped_not_overwritten() {
    let quads = vec![
        [0usize, 1, 2, 3], // (0,1)
        [0usize, 1, 4, 5], // (0,1) duplicate
        [1usize, 0, 6, 7], // (1,0)
    ];
    let m = HalfEdgeMesh::from_quads(&pts(8), &quads);

    assert_eq!(
        m.half_edges[he(0, 0) as usize].twin.0,
        he(2, 0),
        "the FIRST parked (0,1) is the one claimed — not the most recent"
    );
    assert_eq!(m.half_edges[he(2, 0) as usize].twin.0, he(0, 0), "symmetric");
    assert_eq!(
        m.half_edges[he(1, 0) as usize].twin,
        NO_TWIN,
        "the duplicate (0,1) was dropped by or_insert and must stay NO_TWIN"
    );
}

/// Four half-edges on one edge, alternating — both pairs must form, in visit order.
#[test]
fn four_half_edges_on_one_edge_form_two_pairs_in_order() {
    let quads = vec![
        [0usize, 1, 2, 3], // (0,1)
        [1usize, 0, 4, 5], // (1,0)
        [0usize, 1, 6, 7], // (0,1)
        [1usize, 0, 8, 9], // (1,0)
    ];
    let m = HalfEdgeMesh::from_quads(&pts(10), &quads);

    assert_eq!(m.half_edges[he(0, 0) as usize].twin.0, he(1, 0), "first pair");
    assert_eq!(m.half_edges[he(1, 0) as usize].twin.0, he(0, 0), "first pair symmetric");
    assert_eq!(m.half_edges[he(2, 0) as usize].twin.0, he(3, 0), "second pair");
    assert_eq!(m.half_edges[he(3, 0) as usize].twin.0, he(2, 0), "second pair symmetric");
}

/// The ordinary manifold case, plus the boundary rule: a closed box has every half-edge twinned;
/// a single quad has none.
#[test]
fn manifold_pairs_completely_and_a_lone_quad_is_all_boundary() {
    let closed = HalfEdgeMesh::new_box(Vec3::ONE);
    assert!(
        closed.half_edges.iter().enumerate().all(|(i, h)| {
            closed.is_edge_live(core_math::half_edge::HalfEdgeId(i as u32)) == false
                || h.twin != NO_TWIN
        }),
        "a closed box must have no boundary half-edges"
    );

    let lone = HalfEdgeMesh::from_quads(&pts(4), &[[0usize, 1, 2, 3]]);
    assert!(
        lone.half_edges.iter().all(|h| h.twin == NO_TWIN),
        "a single quad has four boundary edges and no twins"
    );
}

/// INTERLEAVING decides, not multiplicity. Contrast with the alternating four-edge case above:
/// the same four directed edges, reordered, produce ONE pair and two orphans with *different*
/// causes — one dropped by `or_insert`, one parked and never claimed.
#[test]
fn two_forward_then_two_reverse_pairs_once_and_orphans_two() {
    let quads = vec![
        [0usize, 1, 2, 3], // (0,1) parks
        [0usize, 1, 4, 5], // (0,1) or_insert on an occupied slot: DROPPED
        [1usize, 0, 6, 7], // (1,0) claims the first
        [1usize, 0, 8, 9], // (1,0) parks, never claimed
    ];
    let m = HalfEdgeMesh::from_quads(&pts(10), &quads);
    assert_eq!(m.half_edges[he(0, 0) as usize].twin.0, he(2, 0));
    assert_eq!(m.half_edges[he(2, 0) as usize].twin.0, he(0, 0), "symmetric");
    assert_eq!(m.half_edges[he(1, 0) as usize].twin, NO_TWIN, "dropped by or_insert");
    assert_eq!(m.half_edges[he(3, 0) as usize].twin, NO_TWIN, "parked and abandoned");
}

/// SELF-LOOP — and this one is reachable in production, not a synthetic curiosity: `to_mesh`
/// stores a triangle as the degenerate quad `[a,b,c,c]`, and `from_mesh` feeds it straight back,
/// so corner 2 is the edge `(c,c)`.
///
/// Its forward and reverse keys are IDENTICAL, so the reverse-claim arm fires and two unrelated
/// self-loops twin *each other*. Any replacement that buckets on `{min,max}` and decides
/// pair-vs-drop by comparing directions reads these as same-direction and DROPS them instead —
/// which is exactly the trap this test exists to catch.
#[test]
fn degenerate_quad_self_loops_pair_with_each_other() {
    let quads = vec![[0usize, 1, 2, 2], [3usize, 4, 2, 2]];
    let m = HalfEdgeMesh::from_quads(&pts(5), &quads);
    assert_eq!(m.half_edges[he(0, 2) as usize].twin.0, he(1, 2));
    assert_eq!(m.half_edges[he(1, 2) as usize].twin.0, he(0, 2), "symmetric");
}

/// `vert.half_edge` must be the FIRST outgoing half-edge in visit order, not the last.
#[test]
fn vertex_half_edge_is_the_first_outgoing_in_visit_order() {
    let quads = vec![[0usize, 1, 2, 3], [0usize, 4, 5, 6]];
    let m = HalfEdgeMesh::from_quads(&pts(7), &quads);
    assert_eq!(
        m.vertices[0].half_edge.0,
        he(0, 0),
        "vertex 0 first appears as the source of quad 0 corner 0"
    );
}
