//! `mesh_ops` — typed mesh operators (plan §4.7), the Rust-native answer to BMesh's untyped
//! operator slot-dict (`op.outputs['geom.out']`). Every op is compile-time typed via `Params` /
//! `EditResult` + a `PROPAGATES` capability flag — no string lookup, no runtime slot dict.
//!
//! Ops are PURE where rebuild is cheap (`mesh → params → from_polygons` via `WeldMap`, the proven
//! `bevel_op.rs` pattern) and become in-place `*_mut` only where O(1) locality matters (later
//! phases). A thin app glue layer (`mesh_actions.rs`) calls ops inside `transaction()` / episodes;
//! the kernel never owns editing context.
//!
//! Phase P0 scaffold: the trait + shared result/error/delta types compile and are minimal. The ONLY
//! op implementation this phase is bevel (Step E, `bevel.rs`), which delegates to the canonical
//! `crate::bevel_op` so there is ONE bevel code path, never a duplicate.

pub mod bevel;
pub mod local_ops;          // add-only / in-place ops → O(touched) ElementDelta (split, extrude, loop_cut, flip).
pub mod local_rebuild_ops;  // free-list-mutating ops → honest O(body) Snapshot (inset, merge, delete, dissolve_*).
pub mod subdivide;          // whole-mesh ops → honest O(body) Snapshot (subdivide_catmull_clark, subdivide_faces).
pub mod propagate;          // Copy/Constant attribute propagation over the touched delta (plan §4.2 Stage A).

#[cfg(test)]
mod test_spine; // THE TEST SPINE (plan §4.5): mesh_eq oracle, proptest fuzz, bincode sentinel guard, FL4 stubs.

#[cfg(test)]
mod targets_tests; // `PolyEdit::targets()` — the refusal→selection-retention mapping contract.

use serde::Serialize;

use crate::poly_mesh::PolyMesh;

/// Public element-reference currency (plan §4.4). `Handle{id,gen}` is the type ops accept so the
/// signature is stable from day one and no raw element id ever escapes `core_math` into app
/// selection / undo / tool caches.
///
/// Phase P0 NOTE: generation RESOLUTION (`resolve(h) -> Option<Index>` checking `gen` against
/// `GenTags`) is NOT wired yet — `GenTags` are demand-driven and land with their first cross-op
/// consumer in Phase 6 (§4.4). Until then `gen` is carried but not validated; `id` is the live
/// field. The type exists now purely so the `MeshOp::apply` signature never changes when GenTags
/// arrive.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Handle {
    pub id: u32,
    pub gen: u32,
}

impl Handle {
    /// Construct a handle with generation 0 (the only generation that exists pre-GenTags).
    pub fn new(id: u32) -> Self { Self { id, gen: 0 } }
}

/// Why an op refused to run. Ops **fail cleanly** rather than emit garbage (plan §4.3): editing a
/// non-manifold region returns `NonManifoldUnsupported`, never emit-and-warn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpError {
    /// The op touched an edge with non-manifold incidence (≥3 faces). AZRA poly is manifold-first;
    /// non-manifold is opt-in overflow handled only by sanctioned cleanup ops (§4.3).
    NonManifoldUnsupported { edge: u32 },
    /// The selection was empty or referenced elements that don't exist.
    EmptySelection,
    /// The op produced a mesh that failed `validate()` — a guard, never silenced (No-Bandaids).
    InvalidResult { reason: String },
    /// Params out of range (e.g. negative width). Caller-correctable.
    InvalidParams { reason: String },
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::NonManifoldUnsupported { edge } =>
                write!(f, "non-manifold edge {} unsupported by this op", edge),
            OpError::EmptySelection => write!(f, "empty or unresolvable selection"),
            OpError::InvalidResult { reason } => write!(f, "op produced invalid mesh: {}", reason),
            OpError::InvalidParams { reason } => write!(f, "invalid op params: {}", reason),
        }
    }
}

impl std::error::Error for OpError {}

/// Provenance for attribute propagation (plan §4.2 Interp contract). An op that supports
/// propagation returns the parent-map `new_id → source element(s) + weights`; a generic
/// `apply_delta` later propagates each CustomData column by its `Interp` over ONLY this touched
/// parent-map set (Frame Budget: never a full-column sweep).
///
/// Provenance parent-map for attribute propagation (plan §4.2).
///
/// First real population: in-place [`HalfEdgeMesh::loop_cut_multi`] (locality spike) —
/// rail split verts and sub-quad faces. Empty = op did not emit parents (drop layers).
#[derive(Clone, Debug, Default)]
pub struct TopoDelta {
    /// New vertex id → [(parent_vert, weight), ...] (e.g. rail split: (bottom, 1−t), (top, t)).
    pub vert_parents: Vec<(u32, Vec<(u32, f32)>)>,
    /// New face id → parent face id (the strip face that was subdivided).
    pub face_parents: Vec<(u32, u32)>,
}

impl TopoDelta {
    pub fn new() -> Self { Self::default() }
    pub fn is_empty(&self) -> bool {
        self.vert_parents.is_empty() && self.face_parents.is_empty()
    }
}

/// The inverse of an op, for delta undo (plan §4.6). For LOCAL ops it records only the changed
/// elements so undo is O(touched); for WHOLE-MESH ops it is an honest per-BODY snapshot (O(body)).
/// The honest cost split (plan §4.6 / Risk #3) is encoded in the VARIANT — never hidden behind a
/// false "all O(touched)" claim:
///
/// - `ElementDelta` — the O(touched) element delta. REQUIRED for the in-place `*_mut` add-only ops
///   (`split_edge`, `extrude_face`, `loop_cut`, `flip_face`). Stores ONLY the pre-op Vec lengths +
///   the before-image of each pre-existing slot the op overwrote. Inverse = restore those slots and
///   TRUNCATE the appended tail. No whole-mesh clone — the dense single-body local-edit win.
/// - `Snapshot` — the per-BODY before-image (O(body), §4.6). Honest tier for ops that mutate the
///   PRIVATE free-list (`inset_face`, `merge_vertices`, `delete_face`, `dissolve_edge`,
///   `dissolve_face`) — an external delta cannot reconstruct the kernel's private free-list without
///   a `half_edge.rs` edit (red line) — AND for the two whole-mesh ops (`subdivide_catmull_clark`,
///   `subdivide_faces`) that touch ~all elements. Still strictly cheaper than Blender's O(scene).
#[derive(Clone, Debug)]
pub enum OpDelta {
    /// O(touched) element delta for add-only / in-place ops (§4.6, Risk #3). See `ElementDelta`.
    Element(ElementDelta),
    /// Whole-substrate before-image (the per-BODY Arc snapshot tier, §4.6). Correct for free-list-
    /// mutating local ops and the whole-mesh subdivide ops.
    Snapshot(crate::half_edge::HalfEdgeMesh),
}

/// O(touched) inverse for an ADD-ONLY / IN-PLACE op (no free-list mutation), captured by EXTERNAL
/// observation of changed slots (plan §7) — ZERO `half_edge.rs` edits.
///
/// Correctness model (the structural guarantee, not tribal knowledge):
/// 1. The op is applied to a mesh whose three Vecs have lengths `prev_v/prev_e/prev_f`.
/// 2. Add-only ops APPEND new elements at the tail (indices ≥ prev_len) and overwrite a BOUNDED set
///    of pre-existing slots (indices < prev_len). They never push to / pop from the free-list.
/// 3. The inverse restores every overwritten pre-existing slot from its before-image, then truncates
///    each Vec back to `prev_len` — reclaiming the EXACT pre-op state (deterministic free-list reuse
///    is trivial here: the free-lists were empty before and stay empty, so truncation is exact).
///
/// PRECONDITION (asserted, never silenced): the input mesh is DENSE in verts+faces. On a dense closed
/// manifold the add-only ops keep the half-edge Vec dense too, so truncation is byte-exact. A sparse
/// input cannot use this path (the wrapper falls back to `Snapshot`) — declared loudly, not hidden.
#[derive(Clone, Debug)]
pub struct ElementDelta {
    /// Vec lengths BEFORE the op — the truncation target for the appended tail.
    pub prev_v: usize,
    pub prev_e: usize,
    pub prev_f: usize,
    /// Before-images of pre-existing slots the op overwrote: `(slot_index, prior_value)`.
    pub changed_verts: Vec<(u32, crate::half_edge::HVertex)>,
    pub changed_edges: Vec<(u32, crate::half_edge::HalfEdge)>,
    pub changed_faces: Vec<(u32, crate::half_edge::HFace)>,
}

impl ElementDelta {
    /// Apply this inverse to `m`, restoring the EXACT pre-op state. O(touched): only the recorded
    /// slots are written + the appended tail is truncated. No whole-mesh allocation.
    ///
    /// Order matters: truncate the tail FIRST (drops appended elements), then restore overwritten
    /// pre-existing slots (all indices < prev_len, so they survive the truncation).
    pub fn apply_inverse(&self, m: &mut crate::half_edge::HalfEdgeMesh) {
        m.vertices.truncate(self.prev_v);
        m.half_edges.truncate(self.prev_e);
        m.faces.truncate(self.prev_f);
        for (i, v) in &self.changed_verts {
            m.vertices[*i as usize] = v.clone();
        }
        for (i, e) in &self.changed_edges {
            m.half_edges[*i as usize] = e.clone();
        }
        for (i, f) in &self.changed_faces {
            m.faces[*i as usize] = f.clone();
        }
    }
}

impl OpDelta {
    /// Restore the pre-op mesh from this inverse (dispatch on the honest-cost variant).
    pub fn apply_inverse(&self, m: &mut crate::half_edge::HalfEdgeMesh) {
        match self {
            OpDelta::Element(d) => d.apply_inverse(m),
            OpDelta::Snapshot(before) => *m = before.clone(),
        }
    }
}

/// Capture an `ElementDelta` by comparing a DENSE pre-op mesh against the post-op mesh of an
/// add-only / in-place op (the EXTERNAL-observation path, plan §7). `prev_*` are the pre-op Vec
/// lengths; `before_*` are the pre-op slot Vecs (only the `[0..prev_len)` range is diffed). Returns
/// `None` if the op popped a slot below `prev_len` from the free-list in a way truncation can't undo
/// (it never does for the add-only ops on a dense input) — the wrapper then falls back to Snapshot.
pub(crate) fn capture_element_delta(
    before_verts: &[crate::half_edge::HVertex],
    before_edges: &[crate::half_edge::HalfEdge],
    before_faces: &[crate::half_edge::HFace],
    after: &crate::half_edge::HalfEdgeMesh,
) -> ElementDelta {
    let prev_v = before_verts.len();
    let prev_e = before_edges.len();
    let prev_f = before_faces.len();

    let mut changed_verts = Vec::new();
    for i in 0..prev_v {
        if !hvertex_eq(&before_verts[i], &after.vertices[i]) {
            changed_verts.push((i as u32, before_verts[i].clone()));
        }
    }
    let mut changed_edges = Vec::new();
    for i in 0..prev_e {
        if !halfedge_eq(&before_edges[i], &after.half_edges[i]) {
            changed_edges.push((i as u32, before_edges[i].clone()));
        }
    }
    let mut changed_faces = Vec::new();
    for i in 0..prev_f {
        if !hface_eq(&before_faces[i], &after.faces[i]) {
            changed_faces.push((i as u32, before_faces[i].clone()));
        }
    }
    ElementDelta { prev_v, prev_e, prev_f, changed_verts, changed_edges, changed_faces }
}

#[inline]
fn hvertex_eq(a: &crate::half_edge::HVertex, b: &crate::half_edge::HVertex) -> bool {
    a.position == b.position && a.normal == b.normal && a.half_edge == b.half_edge
}
#[inline]
fn halfedge_eq(a: &crate::half_edge::HalfEdge, b: &crate::half_edge::HalfEdge) -> bool {
    a.vertex == b.vertex && a.twin == b.twin && a.next == b.next && a.prev == b.prev && a.face == b.face
}
#[inline]
fn hface_eq(a: &crate::half_edge::HFace, b: &crate::half_edge::HFace) -> bool {
    a.half_edge == b.half_edge && a.normal == b.normal && a.material_id == b.material_id
}

/// The result of a successful op: the new mesh plus the provenance + inverse needed by the undo and
/// attribute-flow layers (plan §4.7). The wrapper carries the new topology; `parents` drives
/// propagation; `inverse` drives delta undo.
#[derive(Clone, Debug)]
pub struct EditResult {
    /// The post-op runtime mesh (topology + sidecars). `topo_gen` already bumped by the op.
    pub mesh: PolyMesh,
    /// Attribute provenance (empty until an op ships its parent-map, Phase 4+).
    pub parents: TopoDelta,
    /// Inverse for undo (snapshot tier in P0; element deltas in Phase 2+).
    pub inverse: OpDelta,
}

/// A typed mesh operator (plan §4.7). Generalizes `bevel_op.rs`'s offset-rail + `WeldMap` pattern.
pub trait MeshOp {
    /// Parametric inputs, retained for re-edit (true parametric redo, §4.6). Must serialize so a
    /// past op can be re-opened and re-parameterized.
    type Params: Serialize + Clone;

    /// `false` = the op cannot yet propagate attributes and **declares the drop loudly** (UI surface
    /// + `debug_assert`) rather than silently dropping (§4.2). Flips to `true` op-by-op as each op's
    /// parent-map + `layer_flow` test ships (Phase 4, Gate 2c).
    const PROPAGATES: bool;

    /// Apply the op to `m` over selection `sel` with params `p`. Returns the new mesh + provenance +
    /// inverse, or fails cleanly (`OpError`) — never emits garbage.
    fn apply(&self, m: &PolyMesh, sel: &[Handle], p: &Self::Params)
        -> Result<EditResult, OpError>;
}

// =====================================================================================
// IN-PLACE app dispatch (plan §4.6, §4.7) — the SINGLE entry point the app glue
// (`mesh_actions`) uses to run a wrapped Euler op on the body's `Arc::make_mut(mesh)`
// WITHOUT the trait's "return a new PolyMesh" clone, and get back the inverse `OpDelta`
// for the one `OpRecord` the transaction pushes. One code path, parametric for redo.
// =====================================================================================

/// A typed, parametric Euler edit (plan §4.7 — the Rust-native alternative to BMesh's untyped
/// slot-dict). Carries the op + its retained params so undo can store the inverse and REDO can
/// re-run `apply_in_place` deterministically. `selection` lives in the app's `OpRecord`, not here
/// (these ops take their target via params).
#[derive(Clone, Debug)]
pub enum PolyEdit {
    SplitEdge { edge: u32, t: f32 },
    ExtrudeFace { face: u32, offset: [f32; 3] },
    ExtrudeRegion { faces: Vec<u32>, offset: [f32; 3] },
    LoopCut { start_edge: u32 },
    FlipFace { face: u32 },
    InsetFace { face: u32, amount: f32 },
    MergeVertices { a: u32, b: u32 },
    MergeVerticesAt { verts: Vec<u32>, target: [f32; 3] },
    DeleteFace { face: u32 },
    DeleteVertex { vertex: u32 },
    DeleteEdge { edge: u32 },
    DissolveEdge { edge: u32 },
    DissolveVertex { vertex: u32 },
    DissolveFace { face: u32 },
    DissolveFaces { faces: Vec<u32> },
    Solidify { thickness: f32, offset: f32 },
    SubdivideCatmullClark,
    SubdivideFaces { faces: Vec<u32> },
    /// Blender's `I` DEFAULT: inset the selection as ONE region (only its outer boundary moves; shared
    /// interior edges survive). Per-face `InsetFace` is Blender's "Individual" (`I,I`).
    /// Appended at the END (bincode variant-index rule), though PolyEdit is runtime-only.
    InsetRegion { faces: Vec<u32>, thickness: f32, depth: f32 },
    /// Blender multi-edge "Dissolve Edges" (Ctrl+X, `use_verts=True`): collapse the WHOLE selected edge
    /// set as one atomic region (`half_edge::dissolve_edges_region`), replacing the old per-edge
    /// `DissolveEdge` batch that refused on the first shared-geometry slit. Single-edge routes through
    /// here too, so it now matches Blender's 6/9/5 (valence-2 endpoints swept). Appended at the END
    /// (bincode variant-index rule), though `PolyEdit` is runtime-only.
    DissolveEdgesRegion { edges: Vec<u32> },
    /// Blender "Delete > Only Faces" (`delete(type="ONLY_FACE")`): drop the selected FACE records but
    /// KEEP every edge + vertex — loose edges survive as WIRE EDGES (`half_edge::delete_faces_only`).
    /// Distinct from `DeleteFace` (Blender "Delete Faces"), which frees the half-edges so a loose edge
    /// vanishes. Snapshot tier: it only retires a face record (no id re-keying), same as `DeleteFace`.
    /// Appended at the END (bincode variant-index rule), though `PolyEdit` is runtime-only.
    DeleteFacesOnly { faces: Vec<u32> },
    /// Blender "Poke Faces" (`bpy.ops.mesh.poke`): fan each selected face into triangles from a NEW
    /// center vertex at Blender's default MEDIAN_WEIGHTED center, pushed out by `offset` along the face
    /// normal. Snapshot tier (a `from_polygons` rebuild). Appended at the END (bincode variant-index
    /// rule), though `PolyEdit` is runtime-only.
    PokeFaces { faces: Vec<u32>, offset: f32 },
    /// Blender "Connect Vertex Path" (`bpy.ops.mesh.vert_connect_path`, key `J`): connect the ORDERED
    /// vertices with new diagonal edges, splitting each consecutive pair's shared face (ID-preserving
    /// `split_face_between`). A pair sharing no splittable face is skipped; zero connections refuses.
    /// Snapshot tier. Appended at the END (bincode variant-index rule), though `PolyEdit` is runtime-only.
    ConnectVertexPath { verts: Vec<u32> },
    /// Blender "To Sphere" (`bpy.ops.transform.tosphere`): push each selected vertex toward a sphere
    /// (center = mean, radius = mean distance) by `factor` in [0,1]. Pure position edit — IDs preserved.
    /// Snapshot tier. Appended at the END (bincode variant-index rule), though `PolyEdit` is runtime-only.
    ToSphere { verts: Vec<u32>, factor: f32 },
    /// Blender `mesh.extrude_vertices_move`: wire-edge extrude of selected verts. Appended at END.
    ExtrudeVertices { verts: Vec<u32>, offset: [f32; 3] },
    /// Blender `mesh.extrude_edges_move`: boundary-edge extrude → quads. Appended at END.
    ExtrudeEdges { edges: Vec<u32>, offset: [f32; 3] },
    /// Blender "Delete Faces" for a multi-face selection — **one** snapshot clone + validate for the
    /// whole set (not one `DeleteFace` per id). Owner 2026-08-07: A then Del on a UV sphere hung
    /// because each face paid full-body clone/validate. Appended at END.
    DeleteFaces { faces: Vec<u32> },
}

/// The element domain a `PolyEdit`'s target ids live in.
///
/// Deliberately NOT `poly_mesh::LayerDomain`: that enum's `Edge` means `EdgeKey` and its `Corner`
/// means `HalfEdgeId` (poly_mesh.rs:263-269), while EVERY `PolyEdit` edge field is a `HalfEdgeId`
/// (`dissolve_edge(he_id: HalfEdgeId)`, half_edge.rs:2049) and the app's `poly_sel.selected_edges`
/// stores the same currency (draw_poly.rs:242 iterates it as `heid`). Reusing `LayerDomain` would
/// MISLABEL poly edge selection as EdgeKey — `HalfEdge` keeps the currency honest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElemDomain {
    Vertex,
    HalfEdge,
    Face,
}

impl PolyEdit {
    /// The domain + the exact element ids this op targets — the ids the CALLER handed in, echoed
    /// back unchanged. `None` = the op targets no selectable element (whole-mesh ops).
    ///
    /// This exists so a REFUSED op can be mapped back to the elements that must stay selected, in
    /// ONE place instead of a match over `PolyEdit` at each of the 13 app call sites. The ids stay
    /// valid across a refusal because `apply_in_place` is atomic on `Err` (see its doc comment) —
    /// a refused op leaves the mesh byte-identical, so nothing renumbered.
    ///
    /// Exhaustive ON PURPOSE — no wildcard arm. A new `PolyEdit` variant is a compile error right
    /// here, where the author is already editing. (This codebase has the other outcome too:
    /// `mesh_sync.rs`'s `_ => {}` silently dropping modifiers.)
    pub fn targets(&self) -> Option<(ElemDomain, Vec<u32>)> {
        use ElemDomain::*;
        match self {
            PolyEdit::SplitEdge { edge, .. }        => Some((HalfEdge, vec![*edge])),
            PolyEdit::DeleteEdge { edge }           => Some((HalfEdge, vec![*edge])),
            PolyEdit::DissolveEdge { edge }         => Some((HalfEdge, vec![*edge])),
            PolyEdit::DissolveEdgesRegion { edges }  => Some((HalfEdge, edges.clone())),
            PolyEdit::LoopCut { start_edge }        => Some((HalfEdge, vec![*start_edge])),
            PolyEdit::ExtrudeFace { face, .. }      => Some((Face, vec![*face])),
            PolyEdit::FlipFace { face }             => Some((Face, vec![*face])),
            PolyEdit::InsetFace { face, .. }        => Some((Face, vec![*face])),
            PolyEdit::DeleteFace { face }           => Some((Face, vec![*face])),
            PolyEdit::DeleteFaces { faces }         => Some((Face, faces.clone())),
            PolyEdit::DissolveFace { face }         => Some((Face, vec![*face])),
            PolyEdit::ExtrudeRegion { faces, .. }   => Some((Face, faces.clone())),
            PolyEdit::DissolveFaces { faces }       => Some((Face, faces.clone())),
            PolyEdit::DeleteFacesOnly { faces }     => Some((Face, faces.clone())),
            PolyEdit::SubdivideFaces { faces }      => Some((Face, faces.clone())),
            PolyEdit::InsetRegion { faces, .. }     => Some((Face, faces.clone())),
            PolyEdit::PokeFaces { faces, .. }       => Some((Face, faces.clone())),
            PolyEdit::ConnectVertexPath { verts }   => Some((Vertex, verts.clone())),
            PolyEdit::ToSphere { verts, .. }        => Some((Vertex, verts.clone())),
            PolyEdit::ExtrudeVertices { verts, .. } => Some((Vertex, verts.clone())),
            PolyEdit::DeleteVertex { vertex }       => Some((Vertex, vec![*vertex])),
            PolyEdit::DissolveVertex { vertex }     => Some((Vertex, vec![*vertex])),
            PolyEdit::MergeVertices { a, b }        => Some((Vertex, vec![*a, *b])),
            PolyEdit::MergeVerticesAt { verts, .. } => Some((Vertex, verts.clone())),
            PolyEdit::ExtrudeEdges { edges, .. }    => Some((HalfEdge, edges.clone())),
            // Whole-mesh ops own no selection domain.
            PolyEdit::Solidify { .. } | PolyEdit::SubdivideCatmullClark => None,
        }
    }

    /// `true` if this op gets the O(touched) `ElementDelta` (add-only / in-place — split, extrude,
    /// flip). `false` ops get the honest O(body) `Snapshot`. Drives the app's undo-cost
    /// reporting + the honest-cost gate (no whole-mesh clone for the local ops).
    pub fn is_element_delta(&self) -> bool {
        // NOTE: LoopCut is NO LONGER add-only — it rebuilds the mesh via from_polygons (frees the old
        // faces), so it takes the honest O(body) Snapshot tier below, not the ElementDelta tier.
        matches!(self,
            PolyEdit::SplitEdge { .. } | PolyEdit::ExtrudeFace { .. }
            | PolyEdit::FlipFace { .. })
    }

    /// `true` if this element-delta op is PURELY IN-PLACE: it rewires existing slots and appends
    /// nothing, so `ElementDelta`'s truncate-the-tail inverse is exact regardless of free-list state.
    ///
    /// Why this exists (fixes a real user-facing bug): the element tier used to be gated on the whole
    /// mesh being DENSE. That precondition is real, but only for the ADD-ONLY ops — `SplitEdge` /
    /// `ExtrudeFace` append elements and rely on `truncate(prev_len)` to reclaim them, which is only
    /// byte-exact when the free-lists are empty. `FlipFace` appends nothing (`half_edge::flip_face`
    /// allocates and frees nothing; it reverses the loop of an existing face), so truncation is a
    /// no-op for it and sparseness is irrelevant.
    ///
    /// Gating `FlipFace` on `dense` therefore demoted it to the Snapshot tier the moment a mesh had
    /// ever had anything deleted — and the Snapshot tier ends in an `is_manifold()` check that a
    /// single face flip on a closed mesh CANNOT pass (flipping one face transiently opposes its
    /// neighbours' winding; see the NOTE at the snapshot gate). Net effect: **"Flip Normals" silently
    /// refused on every mesh a user had ever deleted or dissolved from** — a dead menu item on most
    /// real meshes, reported by the 2026-07-30 pillar review and reproduced
    /// (`flip_face_works_on_a_sparse_mesh`).
    fn is_in_place_element_delta(&self) -> bool {
        matches!(self, PolyEdit::FlipFace { .. })
    }

    /// Run this edit IN PLACE on `m` and return the inverse `OpDelta` for undo. The add-only / in-
    /// place ops capture an O(touched) `ElementDelta` (on a dense mesh); everything else captures the
    /// honest O(body) `Snapshot`. Validates the result (No-Bandaids: a topology break is a real bug,
    /// never silenced).
    ///
    /// ATOMIC ON FAILURE, UNCONDITIONALLY: if this returns `Err` — for ANY reason, whether a
    /// pre-mutation guard (bad id / param) or a post-mutation integrity refusal (`validate()` /
    /// `is_manifold()`) — `m` is left EXACTLY as it was passed in, byte for byte. This matters because
    /// `m` is the LIVE scene mesh (`Arc::make_mut`, app/src/undo.rs:394 forward, :259 redo): a
    /// mutated-on-`Err` return strands broken geometry on the scene with NO undo record to reach it.
    /// That was a real bug, fixed here — do not reintroduce a mutable path.
    ///
    /// NOTE the callers do NOT use `transaction()` (an earlier draft of this doc claimed they did — they
    /// never have). `poly_op_batch` (app/src/undo.rs:384) builds its own `UndoEntry` and pushes it
    /// directly at :411. A refused op pushes no record because it is SKIPPED at :397 and so never
    /// reaches `applied` — the atomicity above is what makes that skip safe, not a surrounding
    /// transaction. Nothing outside this function will undo a mutation this function leaks.
    ///
    /// The guarantee is STRUCTURAL, not a discipline the next dev has to remember: all fallible work
    /// happens in `compute`, which takes `m` IMMUTABLY and mutates a private working clone. `m` itself
    /// is written at exactly ONE place — the `mem::replace` below, on the `Ok` path only. A future `?`
    /// added anywhere inside `compute` therefore CANNOT escape with `m` mutated; the compiler, not
    /// review, enforces it. (This mirrors `local_rebuild_ops::run_snapshot`, which is likewise
    /// incapable of corrupting its input because it borrows it immutably.)
    ///
    /// This is the ONE place the kernel op runs for the app: exactly one mesh clone, the same one the
    /// inverse needs anyway (the snapshot tier hands its pre-image straight to `OpDelta::Snapshot`).
    pub fn apply_in_place(&self, m: &mut crate::half_edge::HalfEdgeMesh) -> Result<OpDelta, OpError> {
        // Every fallible step is inside `compute`, which cannot touch `m` (immutable borrow).
        let (next, tier) = self.compute(m)?;
        // THE ONLY write to `m`, reachable only after every guard and both classifiers passed.
        // `next` MOVES in (no clone); the displaced pre-image MOVES out and becomes the inverse.
        let before = std::mem::replace(m, next);
        Ok(match tier {
            Tier::Element(d) => OpDelta::Element(d),
            Tier::Snapshot => OpDelta::Snapshot(before),
        })
    }

    /// The fallible half of `apply_in_place`: build the post-op mesh + pick the honest inverse tier,
    /// WITHOUT the ability to touch the caller's mesh (`m` is `&`, so every `?` here escapes with only
    /// the private working clone `w` dropped). This signature IS the atomicity guarantee.
    fn compute(
        &self,
        m: &crate::half_edge::HalfEdgeMesh,
    ) -> Result<(crate::half_edge::HalfEdgeMesh, Tier), OpError> {
        use crate::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE};

        // Tier is decided on the PRISTINE mesh: add-only / in-place ops on a DENSE mesh → O(touched)
        // ElementDelta via external observation; everything else → honest O(body) Snapshot.
        let dense = m.vertices.len() == m.vertex_count() && m.faces.len() == m.face_count();
        // `dense` is required only by the ADD-ONLY element ops, whose inverse reclaims appended slots
        // with `truncate(prev_len)` — exact only when the free-lists are empty. A purely IN-PLACE op
        // appends nothing, so truncation is a no-op and sparseness cannot affect its inverse.
        // (Requiring `dense` here is what made "Flip Normals" a dead menu item on any mesh that had
        // ever had a face deleted — see `is_in_place_element_delta`.)
        let element_tier =
            self.is_element_delta() && (dense || self.is_in_place_element_delta());

        // The ONE clone. The snapshot tier already paid exactly this; the element tier previously
        // cloned the three pub Vecs instead, so the cost is unchanged (the three private free-lists it
        // now also copies are empty on a dense mesh, and an empty Vec clone does not allocate).
        let mut w = m.clone();

        // Heal digons + stale vertex disks BEFORE mutation on every edit path.
        // Owner 2026-08-06: "Ctrl+X should ALWAYS work even if mesh not clean — usually
        // when you start delete face is to clean up bad geo." A free-knife slice can leave
        // digons that poison integrity(); purging them first unblocks delete/dissolve and
        // every other PolyEdit (extrude after a dirty slice, etc.).
        let is_cleanup_op = matches!(
            self,
            PolyEdit::DeleteFace { .. }
                | PolyEdit::DeleteFaces { .. }
                | PolyEdit::DeleteFacesOnly { .. }
                | PolyEdit::DeleteVertex { .. }
                | PolyEdit::DeleteEdge { .. }
                | PolyEdit::DissolveEdge { .. }
                | PolyEdit::DissolveVertex { .. }
                | PolyEdit::DissolveFace { .. }
                | PolyEdit::DissolveEdgesRegion { .. }
                | PolyEdit::DissolveFaces { .. }
        );
        let input_integrity_ok = m.integrity().is_ok();
        let _ = w.heal_for_edit();

        match self {
            PolyEdit::SplitEdge { edge, t } => {
                if *edge as usize >= w.half_edges.len() { return Err(OpError::EmptySelection); }
                if !(0.0..=1.0).contains(t) { return Err(OpError::InvalidParams { reason: "split t in [0,1]".into() }); }
                w.split_edge(HalfEdgeId(*edge), *t);
            }
            PolyEdit::ExtrudeFace { face, offset } => {
                if *face as usize >= w.faces.len() { return Err(OpError::EmptySelection); }
                w.extrude_face(FaceId(*face), glam::Vec3::from(*offset));
            }
            PolyEdit::ExtrudeRegion { faces, offset } => {
                if faces.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<FaceId> = faces.iter().map(|&f| FaceId(f)).collect();
                w.extrude_region(&set, glam::Vec3::from(*offset));
            }
            PolyEdit::LoopCut { start_edge } => {
                if *start_edge as usize >= w.half_edges.len() { return Err(OpError::EmptySelection); }
                w.loop_cut(HalfEdgeId(*start_edge));
            }
            PolyEdit::FlipFace { face } => {
                if *face as usize >= w.faces.len() { return Err(OpError::EmptySelection); }
                w.flip_face(FaceId(*face));
            }
            PolyEdit::InsetFace { face, amount } => {
                if *face as usize >= w.faces.len() || w.face(FaceId(*face)).half_edge == NO_EDGE {
                    return Err(OpError::EmptySelection);
                }
                w.inset_face(FaceId(*face), *amount);
            }
            PolyEdit::InsetRegion { faces, thickness, depth } => {
                if faces.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<FaceId> = faces.iter()
                    .filter(|&&f| (f as usize) < w.faces.len() && w.face(FaceId(f)).half_edge != NO_EDGE)
                    .map(|&f| FaceId(f)).collect();
                if set.is_empty() { return Err(OpError::EmptySelection); }
                if w.inset_region(&set, *thickness, *depth).faces.is_empty() {
                    return Err(OpError::EmptySelection);
                }
            }
            PolyEdit::MergeVertices { a, b } => {
                if a == b { return Err(OpError::InvalidParams { reason: "merge needs two distinct verts".into() }); }
                if *a as usize >= w.vertices.len() || *b as usize >= w.vertices.len() {
                    return Err(OpError::EmptySelection);
                }
                w.merge_vertices(VertexId(*a), VertexId(*b));
            }
            PolyEdit::MergeVerticesAt { verts, target } => {
                if verts.len() < 2 { return Err(OpError::InvalidParams { reason: "merge needs ≥2 verts".into() }); }
                let vs: Vec<VertexId> = verts.iter().map(|&v| VertexId(v)).collect();
                if w.merge_vertices_to(&vs, glam::Vec3::from(*target)).is_none() {
                    return Err(OpError::EmptySelection);
                }
            }
            PolyEdit::DeleteFace { face } => {
                if *face as usize >= w.faces.len() || w.face(FaceId(*face)).half_edge == NO_EDGE {
                    return Err(OpError::EmptySelection);
                }
                w.delete_face(FaceId(*face));
            }
            PolyEdit::DeleteFaces { faces } => {
                let set: std::collections::HashSet<FaceId> = faces
                    .iter()
                    .filter(|&&f| {
                        (f as usize) < w.faces.len() && w.face(FaceId(f)).half_edge != NO_EDGE
                    })
                    .map(|&f| FaceId(f))
                    .collect();
                if set.is_empty() {
                    return Err(OpError::EmptySelection);
                }
                w.delete_faces(&set);
            }
            PolyEdit::DeleteVertex { vertex } => {
                if *vertex as usize >= w.vertices.len() { return Err(OpError::EmptySelection); }
                w.delete_vertex(VertexId(*vertex));
            }
            PolyEdit::DeleteEdge { edge } => {
                if *edge as usize >= w.half_edges.len() { return Err(OpError::EmptySelection); }
                w.delete_edge(HalfEdgeId(*edge));
            }
            PolyEdit::DissolveEdge { edge } => {
                if *edge as usize >= w.half_edges.len() { return Err(OpError::EmptySelection); }
                if w.dissolve_edge(HalfEdgeId(*edge)).is_none() {
                    return Err(OpError::InvalidParams { reason: "edge boundary / non-dissolvable".into() });
                }
            }
            PolyEdit::DissolveVertex { vertex } => {
                if *vertex as usize >= w.vertices.len() { return Err(OpError::EmptySelection); }
                if w.dissolve_vertex(VertexId(*vertex)).is_none() {
                    return Err(OpError::InvalidParams { reason: "boundary / non-dissolvable vertex".into() });
                }
            }
            PolyEdit::DissolveFace { face } => {
                if *face as usize >= w.faces.len() || w.face(FaceId(*face)).half_edge == NO_EDGE {
                    return Err(OpError::EmptySelection);
                }
                w.dissolve_face(FaceId(*face));
            }
            PolyEdit::DissolveEdgesRegion { edges } => {
                if edges.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<HalfEdgeId> =
                    edges.iter().map(|&e| HalfEdgeId(e)).collect();
                if !w.dissolve_edges_region(&set) {
                    return Err(OpError::InvalidParams { reason: "no dissolvable edge region".into() });
                }
            }
            PolyEdit::DissolveFaces { faces } => {
                if faces.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<FaceId> = faces.iter().map(|&f| FaceId(f)).collect();
                if w.dissolve_faces_region(&set).is_empty() {
                    return Err(OpError::InvalidParams { reason: "no dissolvable face region".into() });
                }
            }
            PolyEdit::DeleteFacesOnly { faces } => {
                // Keep only the faces that name a LIVE face slot — an empty resolved set means the
                // selection referenced nothing droppable, a clean refusal (leaves the mesh untouched).
                let set: std::collections::HashSet<FaceId> = faces.iter()
                    .filter(|&&f| (f as usize) < w.faces.len() && w.face(FaceId(f)).half_edge != NO_EDGE)
                    .map(|&f| FaceId(f)).collect();
                if set.is_empty() { return Err(OpError::EmptySelection); }
                w.delete_faces_only(&set);
            }
            PolyEdit::PokeFaces { faces, offset } => {
                // Keep only live faces with a real ring (≥3 verts). Empty resolved set = clean refusal.
                let set: std::collections::HashSet<u32> = faces.iter()
                    .filter(|&&f| (f as usize) < w.faces.len()
                        && w.face(FaceId(f)).half_edge != NO_EDGE
                        && w.face_vertices(FaceId(f)).len() >= 3)
                    .copied()
                    .collect();
                if set.is_empty() { return Err(OpError::EmptySelection); }
                w.poke_faces(&set, *offset);
            }
            PolyEdit::ConnectVertexPath { verts } => {
                if verts.len() < 2 { return Err(OpError::InvalidParams { reason: "connect needs ≥2 verts".into() }); }
                let ordered: Vec<VertexId> = verts.iter()
                    .filter(|&&v| (v as usize) < w.vertices.len())
                    .map(|&v| VertexId(v))
                    .collect();
                if ordered.len() < 2 { return Err(OpError::EmptySelection); }
                if w.connect_vertices(&ordered) == 0 {
                    return Err(OpError::InvalidParams { reason: "no shared face to connect".into() });
                }
            }
            PolyEdit::ToSphere { verts, factor } => {
                if verts.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<u32> = verts.iter().copied().collect();
                if !w.to_sphere(&set, *factor) {
                    return Err(OpError::EmptySelection);
                }
            }
            PolyEdit::ExtrudeVertices { verts, offset } => {
                if verts.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<u32> = verts.iter().copied().collect();
                if w.extrude_vertices(&set, glam::Vec3::from(*offset)).is_empty() {
                    return Err(OpError::EmptySelection);
                }
            }
            PolyEdit::ExtrudeEdges { edges, offset } => {
                if edges.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<u32> = edges.iter().copied().collect();
                if w.extrude_edges(&set, glam::Vec3::from(*offset)).is_empty() {
                    return Err(OpError::EmptySelection);
                }
            }
            PolyEdit::Solidify { thickness, offset } => {
                if w.face_count() == 0 { return Err(OpError::EmptySelection); }
                if thickness.abs() < 1e-6 { return Err(OpError::InvalidParams { reason: "solidify thickness ~0".into() }); }
                w.solidify(*thickness, *offset);
            }
            PolyEdit::SubdivideCatmullClark => {
                if w.face_count() == 0 { return Err(OpError::EmptySelection); }
                w.subdivide_catmull_clark();
            }
            PolyEdit::SubdivideFaces { faces } => {
                if faces.is_empty() { return Err(OpError::EmptySelection); }
                let set: std::collections::HashSet<u32> = faces.iter().copied().collect();
                w.subdivide_faces(&set);
            }
        }

        // Always heal after mutation: digons / stale disks left by knife densify, slice, or
        // the op itself. Cleanup ops also healed before the mutation (above).
        let _ = w.heal_for_edit();

        w.validate().map_err(|reason| OpError::InvalidResult { reason })?;

        if element_tier {
            // Diffed against the still-pristine `m` — no second clone (the old code cloned three Vecs
            // purely to hold this before-image; the borrow serves it directly now).
            let d = capture_element_delta(&m.vertices, &m.half_edges, &m.faces, &w);
            return Ok((w, Tier::Element(d)));
        }

        // Integrity gate (corruption: digons, stale disks). After heal_for_edit a clean body
        // should pass. Cleanup ops (Ctrl+X / delete / dissolve) ALWAYS ship when validate()
        // is OK — residual integrity issues must not lock the mesh; delete is the cleanup
        // path (owner 2026-08-06). Non-cleanup ops refuse if they would install NEW
        // corruption on a previously clean body; if the input was already dirty beyond
        // heal, still accept so one digon cannot soft-lock every tool.
        //
        // NOTE (deliberate tier asymmetry — do NOT add is_manifold to the element path): a
        // single flip_face fails is_manifold on a closed cube; see prior comments.
        match w.integrity() {
            Ok(()) => {}
            Err(reason) => {
                if is_cleanup_op || !input_integrity_ok {
                    // Cleanup always; or already-dirty input: twin graph valid → accept.
                } else {
                    return Err(OpError::InvalidResult { reason });
                }
            }
        }
        Ok((w, Tier::Snapshot))
    }
}

/// Which inverse `compute` picked. The pre-image itself is NOT carried here: `apply_in_place` gets it
/// for free from the `mem::replace` that installs the result, so the snapshot tier costs no extra clone.
enum Tier {
    Element(ElementDelta),
    Snapshot,
}

#[cfg(test)]
mod tests {
    use super::*;


    /// GATE (pillar review 2026-07-30) — "Flip Normals" must work on a mesh that has had geometry
    /// deleted, which is most real meshes.
    ///
    /// The element tier was gated on the whole mesh being DENSE. One `delete_face` makes it sparse
    /// forever (free-list gaps), which demoted `FlipFace` to the Snapshot tier — and that tier ends in
    /// an `is_manifold()` check a single-face flip on a closed mesh CANNOT pass, because flipping one
    /// face transiently opposes its neighbours' winding. So the op returned
    /// `InvalidResult { "non-manifold: directed edge N->M used by 2 faces (same winding)" }` and the
    /// app's Flip Normals silently did nothing.
    ///
    /// Reproduced before the fix: dense flip Ok, sparse flip Err with exactly that message.
    /// The fix routes purely IN-PLACE element ops (which append nothing, so the truncate-the-tail
    /// inverse is a no-op) through the element tier regardless of density.

    #[test]
    fn flip_face_works_on_a_sparse_mesh() {
        use crate::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
        use glam::Vec3;

        let live_face = |m: &HalfEdgeMesh, skip: Option<u32>| -> Option<u32> {
            m.faces
                .iter()
                .enumerate()
                .find(|(i, f)| Some(*i as u32) != skip && f.half_edge != NO_EDGE)
                .map(|(i, _)| i as u32)
        };

        // Control: on a dense cube the flip already worked. Pin it so the fix cannot regress it.
        let mut dense = HalfEdgeMesh::new_box(Vec3::splat(1.0));
        let f0 = live_face(&dense, None).expect("dense cube has a live face");
        PolyEdit::FlipFace { face: f0 }
            .apply_in_place(&mut dense)
            .expect("flip on a dense mesh must succeed");

        // The regression: delete a face, then flip a different one.
        let mut sparse = HalfEdgeMesh::new_box(Vec3::splat(1.0));
        let victim = live_face(&sparse, None).expect("cube has a live face");
        sparse.delete_face(FaceId(victim));
        assert!(
            sparse.vertices.len() != sparse.vertex_count()
                || sparse.faces.len() != sparse.face_count(),
            "precondition: delete_face must leave the mesh SPARSE, else this gate proves nothing"
        );

        let target = live_face(&sparse, Some(victim)).expect("another live face survives");
        PolyEdit::FlipFace { face: target }
            .apply_in_place(&mut sparse)
            .expect(
                "Flip Normals must work on a mesh that has had a face deleted                  (this is the reported bug: sparse -> Snapshot tier -> is_manifold() refusal)",
            );
    }

    #[test]
    fn handle_defaults_to_gen_zero() {
        let h = Handle::new(7);
        assert_eq!(h.id, 7);
        assert_eq!(h.gen, 0, "pre-GenTags handles are generation 0");
    }

    #[test]
    fn op_error_displays() {
        let e = OpError::NonManifoldUnsupported { edge: 3 };
        assert!(format!("{}", e).contains("non-manifold edge 3"));
    }

    #[test]
    fn topo_delta_empty_in_p0() {
        assert!(TopoDelta::new().is_empty());
    }

    /// topo_gen no-op-replay guard (plan §4.6): every mutating MeshOp bumps `topo_gen` via
    /// `set_topo`, so a stale (no-op) replay is detectable by an unchanged generation. Two distinct
    /// applies advance it twice; applying to the SAME input twice yields the SAME post-gen (1) —
    /// the property the app's lazy re-eval keys on.
    #[test]
    fn topo_gen_bumps_per_mutating_op() {
        use crate::half_edge::{HalfEdgeMesh, FaceId};
        use crate::mesh_ops::local_ops::{ExtrudeFaceOp, ExtrudeFaceParams};
        use glam::Vec3;
        let _ = FaceId(0);

        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        assert_eq!(pm.topo_gen, 0, "fresh wrapper at gen 0");

        let r1 = ExtrudeFaceOp.apply(&pm, &[], &ExtrudeFaceParams { face: 0, offset: [0.0, 0.3, 0.0] }).unwrap();
        assert_eq!(r1.mesh.topo_gen, 1, "one mutating op bumps topo_gen to 1");

        // Re-applying the op to the FRESH input again yields the same post-gen (1) — deterministic,
        // so a no-op replay (gen unchanged from a cached value) is detectable by comparison.
        let r2 = ExtrudeFaceOp.apply(&pm, &[], &ExtrudeFaceParams { face: 0, offset: [0.0, 0.3, 0.0] }).unwrap();
        assert_eq!(r2.mesh.topo_gen, 1, "same input → same post-gen (replay detectable)");

        // Chaining a second op on r1 advances the generation again.
        let r3 = ExtrudeFaceOp.apply(&r1.mesh, &[], &ExtrudeFaceParams { face: 0, offset: [0.0, 0.3, 0.0] }).unwrap();
        assert_eq!(r3.mesh.topo_gen, 2, "a chained op advances topo_gen to 2");
    }

    /// The in-place app dispatch (PolyEdit::apply_in_place) round-trips: apply → inverse restores the
    /// EXACT pre-op mesh, and an element-delta op (split) uses the O(touched) tier while a snapshot
    /// op (delete) uses the O(body) tier. This is the path the app's OpRecord undo drives.
    #[test]
    fn poly_edit_in_place_round_trips_and_picks_honest_tier() {
        use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, NO_TWIN, NO_FACE};
        use glam::Vec3;

        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let e = (0..base.half_edges.len() as u32).find(|&i| {
            let he = base.he(HalfEdgeId(i));
            he.twin != NO_TWIN && he.face != NO_FACE && base.he(he.twin).face != NO_FACE
        }).unwrap();

        // split → ElementDelta, restores exactly.
        let mut m = base.clone();
        let inv = PolyEdit::SplitEdge { edge: e, t: 0.5 }.apply_in_place(&mut m).unwrap();
        assert!(matches!(inv, OpDelta::Element(_)), "split (dense) → O(touched) element delta");
        assert_eq!(m.vertex_count(), 9);
        inv.apply_inverse(&mut m);
        assert_eq!(m.vertex_count(), 8, "inverse restores the pre-op vertex count");
        assert!(m.validate().is_ok());

        // delete → Snapshot, restores exactly.
        let mut m2 = base.clone();
        let inv2 = PolyEdit::DeleteFace { face: 0 }.apply_in_place(&mut m2).unwrap();
        assert!(matches!(inv2, OpDelta::Snapshot(_)), "delete (free-list) → O(body) snapshot");
        assert_eq!(m2.face_count(), 5);
        inv2.apply_inverse(&mut m2);
        assert_eq!(m2.face_count(), 6, "inverse restores the deleted face");
        assert!(m2.validate().is_ok());
    }

    /// The app-facing op path for multi-edge Dissolve Edges (Ctrl+X): PolyEdit::DissolveEdgesRegion runs
    /// through `apply_in_place` — proving the compute() arm's validate()+is_manifold() gate ACCEPTS the
    /// region collapse (not just the raw kernel) — and its Snapshot inverse restores the pristine cube.
    /// Exercised on the HARD 4-top-edges case (cap collapse to a single quad, 4v/4e/1f).
    #[test]
    fn dissolve_edges_region_op_applies_and_restores() {
        use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, VertexId};
        use glam::Vec3;

        let base = HalfEdgeMesh::new_box(Vec3::ONE); // cube spanning ±1
        let sign = |m: &HalfEdgeMesh, v: VertexId| -> [i32; 3] {
            let p = m.vert(v).position;
            [p.x.signum() as i32, p.y.signum() as i32, p.z.signum() as i32]
        };
        // The 4 perimeter edges of the +Y face, picked by endpoint sign.
        let top = [
            ([-1, 1, -1], [1, 1, -1]), ([1, 1, -1], [1, 1, 1]),
            ([1, 1, 1], [-1, 1, 1]), ([-1, 1, 1], [-1, 1, -1]),
        ];
        let mut edges: Vec<u32> = Vec::new();
        for &(a, b) in &top {
            let h = (0..base.half_edges.len() as u32).map(HalfEdgeId)
                .find(|&h| {
                    if !base.is_edge_live(h) { return false; }
                    let (s, d) = (sign(&base, base.he_source(h)), sign(&base, base.he(h).vertex));
                    (s == a && d == b) || (s == b && d == a)
                }).expect("top edge exists");
            edges.push(h.0);
        }

        let mut m = base.clone();
        let inv = PolyEdit::DissolveEdgesRegion { edges }.apply_in_place(&mut m).unwrap();
        assert!(matches!(inv, OpDelta::Snapshot(_)), "region dissolve → O(body) snapshot");
        assert_eq!(m.vertex_count(), 4, "4-top cap collapses to a single quad (nv=4)");
        assert_eq!(m.face_count(), 1, "one quad remains (nf=1)");
        assert!(m.is_manifold().is_ok(), "compute() gate accepted a manifold result");
        inv.apply_inverse(&mut m);
        assert_eq!(m.vertex_count(), 8, "inverse restores the pristine cube (nv=8)");
        assert_eq!(m.face_count(), 6, "inverse restores all 6 faces");
        assert!(m.validate().is_ok());
    }

    /// The app-facing op path for Delete > Only Faces: PolyEdit::DeleteFacesOnly runs through
    /// `apply_in_place` — proving the compute() arm's validate()+is_manifold() gate ACCEPTS a WIRE-EDGE
    /// result (the whole point: is_manifold already tolerates faceless half-edges, no loosening) — and
    /// its Snapshot inverse restores the pristine mesh. Exercised on an isolated quad (worst case: the
    /// result is ALL wire edges, zero faces) and on a cube face (open box, mixed boundary result).
    #[test]
    fn delete_faces_only_op_applies_through_gate_and_restores() {
        use crate::half_edge::{HalfEdgeMesh, FaceId, NO_FACE};
        use glam::Vec3;

        // Isolated quad -> all-wire result must pass the gate.
        let v = vec![
            Vec3::new(-0.5, -0.5, 0.0), Vec3::new(0.5, -0.5, 0.0),
            Vec3::new(0.5, 0.5, 0.0), Vec3::new(-0.5, 0.5, 0.0),
        ];
        let quad = HalfEdgeMesh::from_polygons(&v, &[vec![0, 1, 2, 3]]);
        let mut m = quad.clone();
        let inv = PolyEdit::DeleteFacesOnly { faces: vec![0] }
            .apply_in_place(&mut m)
            .expect("all-wire ONLY_FACE result must pass validate()+is_manifold() gate");
        assert!(matches!(inv, OpDelta::Snapshot(_)), "delete-only (free-list) -> O(body) snapshot");
        assert_eq!(m.face_count(), 0, "quad face dropped");
        assert!(m.is_manifold().is_ok(), "gate accepted the wire result");
        assert!((0..m.half_edges.len() as u32).filter(|i| m.is_edge_live(crate::half_edge::HalfEdgeId(*i))).count() > 0
            && !(0..m.half_edges.len() as u32).any(|i| {
                let h = crate::half_edge::HalfEdgeId(i);
                m.is_edge_live(h) && m.he(h).face != NO_FACE
            }), "wire edges present and all faceless");
        inv.apply_inverse(&mut m);
        assert_eq!(m.face_count(), 1, "inverse restores the quad face");
        assert!(m.validate().is_ok());

        // Cube: ONLY_FACE the +Y top -> open box (mixed wire/boundary), also passes the gate.
        let mut cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let top = (0..cube.faces.len() as u32).map(FaceId).find(|&f| {
            let vs = cube.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&vv| cube.vert(vv).position).sum::<Vec3>() / vs.len() as f32;
            (c - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-4
        }).unwrap();
        let inv2 = PolyEdit::DeleteFacesOnly { faces: vec![top.0] }
            .apply_in_place(&mut cube)
            .expect("open-box ONLY_FACE result must pass the gate");
        assert_eq!(cube.face_count(), 5, "cube -> 5 faces (open box)");
        inv2.apply_inverse(&mut cube);
        assert_eq!(cube.face_count(), 6, "inverse restores the cube");
    }

    /// Batch DeleteFaces on a UV sphere must be ONE clone/validate — not N (owner Del hang).
    #[test]
    fn delete_faces_batch_on_sphere_is_fast_and_empties() {
        use crate::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
        use std::time::Instant;

        let mut m = HalfEdgeMesh::new_uv_sphere(1.0, 32, 16);
        let f0 = m.face_count();
        assert!(f0 > 200, "need a dense-ish sphere, got {f0} faces");
        let all: Vec<u32> = (0..m.faces.len() as u32)
            .filter(|&f| m.face(FaceId(f)).half_edge != NO_EDGE)
            .collect();
        let t0 = Instant::now();
        let inv = PolyEdit::DeleteFaces { faces: all }
            .apply_in_place(&mut m)
            .expect("batch delete all faces");
        let dt = t0.elapsed();
        assert!(
            matches!(inv, OpDelta::Snapshot(_)),
            "delete faces → snapshot undo"
        );
        assert_eq!(m.face_count(), 0, "all faces deleted");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        // One full-body clone path. Debug builds are slow; gate is "not the per-face hang"
        // (per-face was ~N× this cost — minutes on a dense sphere).
        assert!(
            dt.as_secs() < 5,
            "batch delete hung: {dt:?} for {f0} faces"
        );
        // Prove the old per-face path is far worse on a smaller subset (same op machinery).
        let mut m2 = HalfEdgeMesh::new_uv_sphere(1.0, 16, 8);
        let subset: Vec<u32> = (0..m2.faces.len() as u32)
            .filter(|&f| m2.face(FaceId(f)).half_edge != NO_EDGE)
            .take(40)
            .collect();
        let t_batch = Instant::now();
        PolyEdit::DeleteFaces {
            faces: subset.clone(),
        }
        .apply_in_place(&mut m2)
        .expect("batch");
        let batch_ms = t_batch.elapsed().as_secs_f64();
        let mut m3 = HalfEdgeMesh::new_uv_sphere(1.0, 16, 8);
        let t_each = Instant::now();
        for &f in &subset {
            // May skip already-freed after prior deletes; still pays clone each time when live.
            let _ = PolyEdit::DeleteFace { face: f }.apply_in_place(&mut m3);
        }
        let each_ms = t_each.elapsed().as_secs_f64();
        assert!(
            each_ms > batch_ms * 2.0,
            "per-face path should be much slower than batch: each={each_ms:.3}s batch={batch_ms:.3}s"
        );
    }

    /// A DeleteFacesOnly with no resolvable live face refuses cleanly (EmptySelection) and leaves the mesh
    /// byte-identical — the atomic-on-Err contract, exercised on the new op.
    #[test]
    fn delete_faces_only_empty_selection_refuses_atomically() {
        use crate::half_edge::HalfEdgeMesh;
        use crate::mesh_ops::test_spine::topo_identical;
        use glam::Vec3;
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut m = base.clone();
        let err = PolyEdit::DeleteFacesOnly { faces: vec![9999] }
            .apply_in_place(&mut m).expect_err("bad face id must refuse");
        assert!(matches!(err, OpError::EmptySelection), "got {err:?}");
        topo_identical(&base, &m).expect("refused op must leave the mesh byte-identical");
    }

    /// ATOMICITY (the contract at `apply_in_place`'s doc): a REFUSED op must leave `m` untouched.
    ///
    /// This pins the VALIDATION-refusal class, which the id/param guards hid: those return BEFORE any
    /// mutation, so the contract read as true while `validate()` / `is_manifold()` — which run AFTER
    /// the match mutated `m` — escaped via `?` with `m` mutated and the before-image dropped.
    ///
    /// Fixture: `DissolveEdge{1}` after `DissolveEdge{0}` merges two faces that ALREADY share more
    /// than one edge, so the survivor is a slit (the mechanism documented at half_edge.rs:2073-2075).
    /// `validate()` is BLIND to it (it only checks next/prev/twin round-trips); `is_manifold()`
    /// correctly refuses. `m` is the LIVE scene mesh in the app (`Arc::make_mut`, app/src/undo.rs:394
    /// forward and :259 redo), and the refusal pushes NO undo record — so a non-atomic refusal ships
    /// broken geometry that Ctrl+Z cannot reach.
    #[test]
    fn apply_in_place_refusal_leaves_mesh_untouched() {
        use crate::half_edge::HalfEdgeMesh;
        use crate::mesh_ops::test_spine::topo_identical;
        use glam::Vec3;

        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        PolyEdit::DissolveEdge { edge: 0 }.apply_in_place(&mut m).expect("op1 is a legitimate dissolve");
        assert_eq!(m.face_count(), 5, "fixture: op1 really merged two faces (guards vacuity)");

        // The state the contract promises to preserve on refusal.
        let before = m.clone();

        let err = PolyEdit::DissolveEdge { edge: 1 }
            .apply_in_place(&mut m)
            .expect_err("op2 makes a slit; is_manifold() must refuse it");
        assert!(
            matches!(err, OpError::InvalidResult { .. }),
            "must be the post-mutation validation refusal, not a pre-mutation id guard; got {err:?}"
        );

        topo_identical(&before, &m).expect("a REFUSED op must leave the mesh byte-identical");
    }

    /// Owner directive 2026-08-03: "all tools must work even on open / non-manifold shapes —
    /// I merged vertices in an open shape and got a manifold error." Merging two boundary
    /// corners of an open sheet creates a BOWTIE vertex — vertex-non-manifold, but a
    /// perfectly legal, Blender-representable state. The old RELATIVE gate (manifold input
    /// ⇒ manifold output required) refused exactly this; edit ops now gate on `integrity()`
    /// (corruption: degenerate faces, stale disk pointers) only, and `is_manifold()` stays
    /// the bar solely where solids are required (boolean install).
    #[test]
    fn merge_boundary_verts_on_open_sheet_is_allowed() {
        use crate::half_edge::HalfEdgeMesh;
        use glam::Vec3;
        // Two disjoint quads with a gap: merging one gap-side corner from each makes a bowtie.
        let positions = vec![
            Vec3::new(0., 0., 0.), Vec3::new(1., 0., 0.), Vec3::new(1., 1., 0.), Vec3::new(0., 1., 0.),
            Vec3::new(2., 0., 0.), Vec3::new(3., 0., 0.), Vec3::new(3., 1., 0.), Vec3::new(2., 1., 0.),
        ];
        let faces: Vec<Vec<usize>> = vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]];
        let mut m = HalfEdgeMesh::from_polygons(&positions, &faces);
        assert!(m.is_manifold().is_ok(), "fixture: open two-quad sheet is manifold-with-boundary");

        PolyEdit::MergeVerticesAt { verts: vec![1, 4], target: [1.5, 0.0, 0.0] }
            .apply_in_place(&mut m)
            .expect("merging boundary verts on an open sheet must SUCCEED (Blender parity)");

        m.validate().expect("result structurally valid");
        m.integrity().expect("result corruption-free");
        assert!(m.is_manifold().is_err(), "merged vertex is a bowtie — a visible state, not an error");
        assert_eq!(m.face_count(), 2, "both quads survive");
    }

    /// Ctrl+X / delete / dissolve must work on an ALREADY non-manifold mesh (owner: "ctrl x does not
    /// work on non manifold shape"). The absolute `is_manifold(result)` gate used to refuse every
    /// Snapshot-tier edit on a T-fin mesh even when the op itself was clean, because the fin defect
    /// survives and the result stays non-manifold. Relative policy: only refuse when the INPUT was
    /// manifold and the OUTPUT is not.
    /// Digon-poison case lives in `half_edge::tests::purge_digon_allows_delete_of_real_face`
    /// (needs private alloc for digon inject).
    #[test]
    fn ctrl_x_delete_face_works_on_already_non_manifold_mesh() {
        use crate::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
        use glam::Vec3;

        // T-junction fin: three faces share undirected edge 0-1 → non-manifold.
        let fin_pos = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let mut m = HalfEdgeMesh::from_polygons(
            &fin_pos,
            &[
                vec![0, 1, 2, 3], // A
                vec![1, 0, 4, 5], // B
                vec![0, 1, 6, 7], // fin C
            ],
        );
        assert!(
            m.is_manifold().is_err(),
            "precondition: T-fin is non-manifold: {:?}",
            m.is_manifold()
        );
        let f_before = m.face_count();
        // Pick any live face that is NOT the only remaining one.
        let victim = (0..m.faces.len() as u32)
            .find(|&i| m.face(FaceId(i)).half_edge != NO_EDGE)
            .expect("has a live face");
        PolyEdit::DeleteFace { face: victim }
            .apply_in_place(&mut m)
            .expect("DeleteFace (Ctrl+X face path) must apply on already-broken mesh");
        assert_eq!(m.face_count(), f_before - 1, "face must actually be deleted");
        assert!(m.validate().is_ok(), "twin/cycle still valid: {:?}", m.validate());
    }

    /// Clean meshes still refuse an op that *creates* a non-manifold tear (relative gate still protects).
    #[test]
    fn clean_mesh_still_refuses_slit_creating_dissolve() {
        use crate::half_edge::HalfEdgeMesh;
        use glam::Vec3;

        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        PolyEdit::DissolveEdge { edge: 0 }
            .apply_in_place(&mut m)
            .expect("first dissolve ok");
        assert!(m.is_manifold().is_ok(), "after first dissolve still manifold");
        let err = PolyEdit::DissolveEdge { edge: 1 }
            .apply_in_place(&mut m)
            .expect_err("slit-creating dissolve must still refuse on clean-input path");
        assert!(matches!(err, OpError::InvalidResult { .. }), "got {err:?}");
    }

    /// The pre-mutation refusals (bad id / param) must ALSO stay atomic — they are clean today, and
    /// this locks that the fix does not regress them (e.g. by restoring a wrong image).
    #[test]
    fn apply_in_place_guard_refusals_leave_mesh_untouched() {
        use crate::half_edge::HalfEdgeMesh;
        use crate::mesh_ops::test_spine::topo_identical;
        use glam::Vec3;

        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        // Snapshot tier (bad id), snapshot tier (bad param), element tier (bad id + bad param).
        let cases: Vec<PolyEdit> = vec![
            PolyEdit::DissolveEdge { edge: 9999 },
            PolyEdit::MergeVertices { a: 3, b: 3 },
            PolyEdit::ExtrudeFace { face: 9999, offset: [0.0, 1.0, 0.0] },
            PolyEdit::SplitEdge { edge: 0, t: 1.5 },
        ];
        for op in cases {
            let mut m = base.clone();
            let err = op.apply_in_place(&mut m).expect_err("guard must refuse");
            assert!(
                matches!(err, OpError::EmptySelection | OpError::InvalidParams { .. }),
                "{op:?} should refuse at a guard, got {err:?}"
            );
            topo_identical(&base, &m).unwrap_or_else(|e| panic!("{op:?} mutated on a guard refusal: {e}"));
        }
    }

    /// The app-facing op paths for the three new ops run through `apply_in_place` — proving each
    /// compute() arm's validate()+is_manifold() gate ACCEPTS the result (not just the raw kernel) and its
    /// Snapshot inverse restores the pristine mesh. Poke (cube, all faces), Connect (quad diagonal), and
    /// To Sphere (flat grid) each exercised.
    #[test]
    fn poke_connect_tosphere_apply_in_place_gate_and_restore() {
        use crate::half_edge::{HalfEdgeMesh, VertexId};
        use glam::Vec3;

        // POKE — cube, all 6 faces → 14v/24f, Snapshot inverse restores the cube.
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut m = base.clone();
        let inv = PolyEdit::PokeFaces { faces: (0..6).collect(), offset: 0.0 }
            .apply_in_place(&mut m).expect("poke must pass the gate");
        assert!(matches!(inv, OpDelta::Snapshot(_)), "poke → O(body) snapshot");
        assert_eq!(m.vertex_count(), 14, "cube poke nv");
        assert_eq!(m.face_count(), 24, "cube poke nf");
        assert!(m.is_manifold().is_ok(), "gate accepted the poked cube");
        inv.apply_inverse(&mut m);
        assert_eq!(m.vertex_count(), 8, "inverse restores the cube");
        assert_eq!(m.face_count(), 6);

        // CONNECT — quad diagonal → 2 triangles, restores.
        let quad = HalfEdgeMesh::from_polygons(
            &[Vec3::new(-1.,0.,-1.), Vec3::new(1.,0.,-1.), Vec3::new(1.,0.,1.), Vec3::new(-1.,0.,1.)],
            &[vec![0,1,2,3]]);
        let mut q = quad.clone();
        let inv = PolyEdit::ConnectVertexPath { verts: vec![0, 2] }
            .apply_in_place(&mut q).expect("connect must pass the gate");
        assert_eq!(q.face_count(), 2, "quad → 2 faces");
        inv.apply_inverse(&mut q);
        assert_eq!(q.face_count(), 1, "inverse restores the quad");

        // CONNECT refusal — two unshared verts refuse cleanly, mesh untouched.
        let mut q2 = quad.clone();
        let err = PolyEdit::ConnectVertexPath { verts: vec![0, 1] } // adjacent → no diagonal
            .apply_in_place(&mut q2).expect_err("adjacent pair connects nothing → refuse");
        assert!(matches!(err, OpError::InvalidParams { .. }), "got {err:?}");
        assert_eq!(q2.face_count(), 1, "refused connect left the quad untouched");

        // TO SPHERE — flat grid factor 1.0 moves verts but keeps topology; inverse restores positions.
        let mut g = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
        let before_v4 = g.vert(VertexId(0)).position;
        let inv = PolyEdit::ToSphere { verts: (0..9).collect(), factor: 1.0 }
            .apply_in_place(&mut g).expect("to_sphere must pass the gate");
        assert!(matches!(inv, OpDelta::Snapshot(_)), "to_sphere → snapshot");
        assert!((g.vert(VertexId(0)).position - before_v4).length() > 0.1, "corner vertex moved");
        inv.apply_inverse(&mut g);
        assert!((g.vert(VertexId(0)).position - before_v4).length() < 1e-6, "inverse restored the position");
    }

    /// Simulate the app's BATCH undo/redo (the `UndoPayload::Poly` arm in `undo.rs`) at the kernel
    /// level: apply N ops forward capturing inverses, UNDO = apply inverses in REVERSE order, REDO =
    /// re-run the ops forward. The pre-op mesh must be restored exactly, and one undo→redo cycle must
    /// land back on the post-op mesh — proving the batch ordering in the app glue is correct.
    #[test]
    fn poly_edit_batch_undo_redo_cycle() {
        use crate::half_edge::{HalfEdgeMesh, FaceId};
        use glam::Vec3;

        let pre = HalfEdgeMesh::new_box(Vec3::ONE);
        // A multi-op batch: delete two faces (each free-list snapshot).
        let batch = vec![PolyEdit::DeleteFace { face: 2 }, PolyEdit::DeleteFace { face: 0 }];

        // Forward apply, capturing inverses (what poly_op_batch does).
        let mut m = pre.clone();
        let mut inverses = Vec::new();
        for op in &batch {
            inverses.push((op.clone(), op.apply_in_place(&mut m).unwrap()));
        }
        let post_faces = m.face_count();
        assert_eq!(post_faces, 4, "two faces deleted from a cube");
        // Capture the post-op active geometry for the redo comparison.
        let post_active: Vec<_> = (0..m.faces.len() as u32)
            .filter(|&i| m.face(FaceId(i)).half_edge != crate::half_edge::NO_EDGE)
            .map(|i| m.face_vertices(FaceId(i)).iter().map(|v| v.0).collect::<Vec<_>>())
            .collect();

        // UNDO: inverses in REVERSE order.
        for (_op, inv) in inverses.iter().rev() {
            inv.apply_inverse(&mut m);
        }
        assert_eq!(m.face_count(), 6, "undo restores all faces");
        assert!(m.validate().is_ok());

        // REDO: re-run the ops forward (parametric redo).
        let mut redo_inverses = Vec::new();
        for (op, _) in &inverses {
            redo_inverses.push(op.apply_in_place(&mut m).unwrap());
        }
        assert_eq!(m.face_count(), post_faces, "redo reproduces the post-op face count");
        let redo_active: Vec<_> = (0..m.faces.len() as u32)
            .filter(|&i| m.face(FaceId(i)).half_edge != crate::half_edge::NO_EDGE)
            .map(|i| m.face_vertices(FaceId(i)).iter().map(|v| v.0).collect::<Vec<_>>())
            .collect();
        assert_eq!(redo_active, post_active, "redo reproduces the EXACT post-op topology");
    }
}
