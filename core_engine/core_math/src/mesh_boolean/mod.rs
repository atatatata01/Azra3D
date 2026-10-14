//! mesh_boolean.rs — CSG booleans between two CLOSED `HalfEdgeMesh`es.
//!
//! # Kernel selection (production wrap, 2026-08-02)
//!
//! | Mode | When | Implementation |
//! |---|---|---|
//! | **Manifold (default)** | `AZRA_BSP_BOOLEAN` unset / `0` / `false` / `off` | `mesh_boolean_manifold::manifold_boolean` |
//! | **Legacy BSP A/B** | `AZRA_BSP_BOOLEAN=1` (or any other truthy value) | `robust_boolean` then `bsp_boolean` |
//!
//! **REPLACE-RULE:** the robust+BSP path is kept for A/B and soak only. Delete it only after
//! a full green `core_math` suite + Blender oracle green + **one owner session** sign-off.
//! Do not delete in this PR — flag only (see `mesh_boolean_manifold` module docs).
//!
//! Pure module (no GPU). Pattern mirrors `bevel_op.rs`: read the source meshes through the
//! public half-edge API, compute, rebuild via `HalfEdgeMesh::from_polygons`. `half_edge.rs`
//! is NEVER touched (frozen).
//!
//! Legacy BSP algorithm: classic solid-BSP CSG (csg.js formulation) + robust winding-number
//! classifier. Open (boundary-edged) inputs are rejected with `None`.

// DETERMINISM: the coplanar-merge / region-trace / coincident-cancel passes drive the OUTPUT
// topology from map/set ITERATION order. std HashMap/HashSet iterate in a per-process-random order
// (SipHash w/ random seed), so the SAME boolean produced a different mesh — or None — every run
// (the user's "boolean usually breaks on the 2nd cut" was this nondeterminism surfacing). Those
// passes use BTreeMap/BTreeSet so iteration is a fixed key order and the result is reproducible.
// (ToleranceWeld/grid stay HashMap — they are only point-keyed .get() lookups, never iterated.)
use crate::half_edge::{HalfEdgeMesh, FaceId, HalfEdgeId, NO_EDGE, NO_FACE, NO_TWIN};

mod rebuild;
pub(crate) use rebuild::{
    flatten_folded_clusters, is_input_vertex, merge_coplanar, same_operand_vertices, post_boolean_polish,
    preserved_vert_count, repair_t_junctions, dissolve_collinear_valence2,
};
use rebuild::polygons_to_mesh;
pub use rebuild::keep_operand_ids;
mod classify;
pub use classify::point_triangle_dist_sq;
use classify::robust_boolean;
mod bsp;
use bsp::{
    mesh_to_polygons, split_polygon, bsp_boolean, Plane, Polygon,
    SRC_A, SRC_B, COPLANAR, FRONT, BACK, SPANNING,
};
#[cfg(test)]
use bsp::BspNode;


/// Boolean operation between two closed meshes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoolOp {
    Union,
    Difference,
    Intersect,
}

/// Plane-thickness epsilon for point classification (spec: ~1e-5).
const PLANE_EPS: f32 = 1e-5;
/// Position-weld tolerance for the output rebuild. Coarser than PLANE_EPS on purpose:
/// intersection points computed along different BSP paths agree only to interpolation noise.
const WELD_EPS: f32 = 1e-4;
/// Max distance of a face's verts from a neighbour's plane for the two to count as coplanar
/// (the dissolve pass). Loose relative to WELD_EPS — merged flats should absorb seam noise.
/// Slightly looser than 1e-3 so residual boolean float noise on large meshes still dissolves
/// (user "random edges / need only one edge" after boolean).
const COPLANAR_DIST: f32 = 2.5e-3;
/// Normal-agreement threshold (dot) for coplanar faces — ~2.56°. Cube-flat results agree ~1.0.
const COPLANAR_DOT: f32 = 0.999;
/// Collinearity threshold (dot of the two edge directions at a vertex) for dissolving a
/// straight-edge interior vertex — ~1.8° (was ~0.8° — too tight left collinear seam verts).
const COLLINEAR_DOT: f32 = 0.9995;
/// Post-boolean limited-dissolve CANDIDATE angle (degrees) — a pre-filter only. Whether a
/// candidate actually merges is decided by `rebuild::faces_share_plane` (vertex distance to one
/// plane at float precision), so facets of a fine curve (a 32/64-segment bevel steps 2.8°/1.4°)
/// are never dissolved however small their angle (POLY-BOOL-NGON-BEVEL, 2026-10-13: as a bare
/// angle test this merged a rounded cutter's strips into the flat they meet).
const POST_BOOL_DISSOLVE_DEG: f32 = 12.0;
/// Collinearity for post-boolean valence-2 vertex collapse (join_edge_kill_vert).
/// Slightly looser than COLLINEAR_DOT so float noise on long cube edges still dissolves.
const POST_BOOL_COLLINEAR_DOT: f32 = 0.999;
/// Dot product for "two edges form a straight chain through a valence-3 T-junction".
/// Used to snap-remove CSG T-junction pins when two of three edges are collinear.
const T_JUNCTION_COLLINEAR_DOT: f32 = 0.999;
/// BSP recursion depth cap (audit #12). The tree degenerates toward a linked list on dense /
/// self-intersecting input (`build` picks the first polygon as splitter, no balancing), so depth can
/// reach the polygon count → stack overflow. A well-balanced BSP of N polys is only ~log2(N) deep, so
/// this cap NEVER triggers on real meshes (a balanced BSP of N polys is only ~log2(N) deep, ~18 for the
/// 256k ceiling); it is a crash backstop that dumps the tail at the leaf. Kept well under the debug-build
/// overflow threshold (~3000 unoptimized frames blow the 8MB stack) with ~3× margin.
const MAX_BSP_DEPTH: usize = 1000;
/// Input polygon-count ceiling — refuse absurd inputs cleanly (None) instead of an O(n²) BSP blowup.
const MAX_BSP_POLYS: usize = 262_144;

// ============================================================
// Boundary census (shared guard: kernel input check, app install
// guard, and unit tests all use this ONE definition)
// ============================================================

/// Number of live, face-owning half-edges with no twin (open-surface boundary edges).
/// 0 on a watertight mesh. Freed half-edges (face == NO_FACE) are ignored.
///
/// NOTE: `poly_drag_tools.rs::poly_bevel_regenerate` has an inline closure with the same
/// logic (predates this fn) — flagged for extraction to here when that file is next touched.
pub fn boundary_half_edge_count(mesh: &HalfEdgeMesh) -> usize {
    (0..mesh.half_edges.len() as u32)
        .filter(|&i| {
            let he = mesh.he(HalfEdgeId(i));
            he.face != NO_FACE && he.twin == NO_TWIN
        })
        .count()
}

// ============================================================
// Public entry point
// ============================================================

/// CSG boolean between two CLOSED half-edge meshes (both in the SAME coordinate space —
/// the caller transforms to world space and back).
///
/// Returns `None` when either input is open (has boundary half-edges), degenerate, or the
/// result is empty (e.g. intersect of disjoint solids). The caller MUST additionally gate
/// installs on `validate().is_ok()` and `boundary_half_edge_count(..) == 0` (mirrors the
/// bevel install guard).
///
/// **Default kernel:** Manifold (`mesh_boolean_manifold`).  
/// **A/B:** set `AZRA_BSP_BOOLEAN=1` to force the legacy robust+BSP path.
///
/// Signature and call sites are unchanged — wrap only.
/// Positive-orientation guard: a closed input wound INSIDE-OUT (negative signed volume)
/// makes every kernel produce empty/garbage (owner subtract.fl4 2026-08-03: an inverted
/// cube shell -> "kernel rejected"). Blender's Exact solver normalizes orientation first;
/// so do we -- rebuild with reversed rings when the signed volume is negative.
fn oriented_positive(m: &HalfEdgeMesh) -> Option<HalfEdgeMesh> {
    if mesh_signed_volume(m) >= 0.0 {
        return None; // already positive (or open/degenerate - leave untouched)
    }
    let mut positions: Vec<glam::Vec3> = Vec::new();
    let mut remap: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut rings: Vec<Vec<usize>> = Vec::new();
    for i in 0..m.faces.len() as u32 {
        let fid = FaceId(i);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let mut ring: Vec<usize> = m
            .face_vertices(fid)
            .iter()
            .map(|v| {
                *remap.entry(v.0).or_insert_with(|| {
                    positions.push(m.vert(*v).position);
                    positions.len() - 1
                })
            })
            .collect();
        ring.reverse();
        rings.push(ring);
    }
    Some(HalfEdgeMesh::from_polygons(&positions, &rings))
}

pub fn mesh_boolean(a: &HalfEdgeMesh, b: &HalfEdgeMesh, op: BoolOp) -> Option<HalfEdgeMesh> {
    // `want_provenance = false`: provenance is a pure ADD-ON (one classification pass over the
    // result), so a caller that discards it must not pay for it. Same pipeline, one flag.
    boolean_impl(a, b, op, false, false).map(|(m, _)| m)
}

/// `mesh_boolean` that ALSO reports where each result face came from
/// (`docs/planning/poly/BOOLEAN_PROVENANCE.md` stage P1).
///
/// Identical geometry to `mesh_boolean` — the provenance is collected from the per-operand
/// PRESERVE sets while they are still armed, and costs one pass over the result faces. Use
/// this wherever the caller wants to know cutter-vs-target (Bevel Smart Select rule 2, the
/// knife live cut, remapping UV/seam/crease through a boolean).
pub fn mesh_boolean_with_provenance(
    a: &HalfEdgeMesh,
    b: &HalfEdgeMesh,
    op: BoolOp,
) -> Option<(HalfEdgeMesh, BoolProvenance)> {
    boolean_impl(a, b, op, true, false)
}

/// [`mesh_boolean_with_provenance`] whose result is numbered on `a`'s ids
/// ([`keep_operand_ids`]): every vertex / edge / face of the target the cut did not consume keeps
/// its id, so a commit that INSTALLS the result over `a` leaves the poly selection, hidden set,
/// hover, active vertex and id-keyed sidecar naming the same geometry (POLY-STALE-IDS). The
/// provenance is collected on the renumbered mesh, so its ids match. Same geometry as
/// [`mesh_boolean`]; costs one extra soup rebuild, so only install paths should ask for it.
pub fn mesh_boolean_keeping_ids(
    a: &HalfEdgeMesh,
    b: &HalfEdgeMesh,
    op: BoolOp,
) -> Option<(HalfEdgeMesh, BoolProvenance)> {
    boolean_impl(a, b, op, true, true)
}

/// THE boolean pipeline — every public entry point routes through here (never two copies).
///
/// `want_provenance` gates ONLY the trailing [`collect_provenance`] classification pass; every
/// geometric step above it is unconditional, so the returned mesh is identical either way.
/// When `false` the returned [`BoolProvenance`] is `Default` (empty), which every consumer
/// already treats as "unknown → fail closed". `keep_a_ids` renumbers the result onto the
/// CALLER's `a` (not its re-oriented copy, whose ids are fresh) before provenance is read.
fn boolean_impl(
    a_in: &HalfEdgeMesh,
    b: &HalfEdgeMesh,
    op: BoolOp,
    want_provenance: bool,
    keep_a_ids: bool,
) -> Option<(HalfEdgeMesh, BoolProvenance)> {
    // Normalize inside-out closed inputs before any kernel sees them.
    let (fa, fb) = (oriented_positive(a_in), oriented_positive(b));
    let a = fa.as_ref().unwrap_or(a_in);
    let b = fb.as_ref().unwrap_or(b);
    // Register both operands' verts/edges as untouchable, so cleanup only reshapes
    // geometry the cut actually created (owner rule 2026-08-04).
    rebuild::set_preserve_from(a, b);
    let _guard = scopeguard_clear();
    let r = if crate::mesh_boolean_manifold::prefer_bsp_boolean() {
        // Legacy A/B path (REPLACE-RULE: delete only after owner soak + full green).
        robust_boolean(a, b, op).or_else(|| bsp_boolean(a, b, op))?
    } else {
        match crate::mesh_boolean_manifold::manifold_boolean(a, b, op) {
            Some(r) => r,
            None => {
                eprintln!("[BOOL] manifold kernel returned None for {:?}", op);
                return None;
            }
        }
    };
    // VOLUME INVARIANT GATE (POLY-BOOL-BEVEL): the manifold gates are purely topological —
    // a tiny closed sliver left over by a misclassification is a perfectly valid manifold
    // and would INSTALL as the "result". Enforce the mathematical volume bounds of each op
    // so such a result fails honestly (caller keeps the old mesh + toasts) instead:
    //   A−B ∈ [vol(A) − vol(B), vol(A)]     A∪B ∈ [max, vol(A)+vol(B)]     A∩B ∈ [0, min]
    // Only enforced when both inputs are positively wound (an inside-out input has no
    // meaningful volume semantics — the boolean itself is already undefined there).
    // Raw-integral volume gate: LEGACY BSP PATH ONLY. The manifold path enforces the
    // invariant internally with Manifold's RESOLVED volumes — the raw integral of a
    // non-planar-capped operand underestimates its solid and wrongly rejects correct
    // results (owner test-05, 2026-08-04).
    let (va, vb) = (mesh_signed_volume(a), mesh_signed_volume(b));
    if crate::mesh_boolean_manifold::prefer_bsp_boolean() && va > 0.0 && vb > 0.0 {
        let vr = mesh_signed_volume(&r);
        let tol = 1e-3 * va.max(vb) + 1e-6;
        let ok = match op {
            BoolOp::Difference => vr >= va - vb - tol && vr <= va + tol,
            BoolOp::Union => vr >= va.max(vb) - tol && vr <= va + vb + tol,
            BoolOp::Intersect => vr >= -tol && vr <= va.min(vb) + tol,
        };
        if !ok && std::env::var("AZRA_NO_VOLGATE").is_err() {
            eprintln!(
                "[BOOL] volume gate rejected {:?}: result vol={vr:.6} outside bounds (va={va:.6} vb={vb:.6})",
                op
            );
            return None;
        }
    }
    let r = if keep_a_ids { keep_operand_ids(a_in, &r).0 } else { r };
    // MUST run before `_guard` drops — the drop clears the per-operand sets this reads.
    let prov = if want_provenance {
        collect_provenance(&r, a, b)
    } else {
        BoolProvenance::default()
    };
    Some((r, prov))
}

/// MESH CLEANUP (owner 2026-08-03 — repair bodies poisoned by older builds): re-extract
/// the face soup, flatten folded coplanar clusters (zero-thickness flaps), drop collapsed
/// rings, and merge coplanar faces back into n-gons. Returns `None` when the mesh is
/// already clean AND fully merged, or when the repaired rebuild fails its gates (open /
/// invalid / MORE folds than before) — the caller keeps the original in both cases.
pub struct CleanupOptions {
    /// Flatten zero-thickness folded coplanar flaps (signed decompositions).
    pub flatten_folds: bool,
    /// Merge coplanar faces back into n-gons.
    pub merge_coplanar: bool,
    /// Blender Limited Dissolve on the result: dissolve edges whose faces meet within
    /// this angle (degrees). 0 = off.
    pub dissolve_angle_deg: f32,
    /// Edges cleanup MUST NOT remove, as undirected endpoint-position pairs (positions, not
    /// ids — cleanup rebuilds the mesh, so every id dies but the geometry survives).
    ///
    /// Owner 2026-08-05: "mesh clean up should never touch [tagged edges]" — the app fills
    /// this from the sidecar's authored layers (Smart Select `op_created`, Mark Sharp, UV
    /// seam, crease). Empty = the historical behaviour, cleanup merges/dissolves freely.
    pub protected_edges: Vec<(glam::Vec3, glam::Vec3)>,

    // --------------------------------------------------------------------------------
    // Blender `Mesh > Clean Up` parity (added 2026-08-11). ALL default to OFF, so
    // `cleanup_mesh()` — i.e. the boolean repair path — behaves exactly as before.
    // --------------------------------------------------------------------------------
    /// Blender **Merge by Distance**: weld vertices closer than this. 0 = off.
    pub merge_distance: f32,
    /// Blender **Degenerate Dissolve**: collapse edges shorter than this and drop the
    /// zero-area faces that result. 0 = off.
    pub degenerate_threshold: f32,
    /// Blender **Delete Loose**: unreferenced vertices.
    pub delete_loose_verts: bool,
    /// Blender **Delete Loose**: wire edges (no face on either side).
    pub delete_loose_edges: bool,
    /// Blender **Delete Loose**: faces sharing no edge with any other face.
    pub delete_loose_faces: bool,
    /// Blender **Fill Holes**: cap boundary loops with at most this many sides. 0 = off.
    pub fill_holes_sides: usize,
    /// Blender **Make Planar Faces**: relaxation iterations at factor 1.0. 0 = off.
    pub make_planar_iterations: u32,
    /// Blender **Split Non-Planar Faces**: split n-gons twisting past this angle. 0 = off.
    pub split_non_planar_deg: f32,
    /// Blender **Split Concave Faces**.
    pub split_concave: bool,
    /// Blender **Recalculate Normals** (Shift+N): consistent winding, outward on closed shells.
    pub recalc_normals: bool,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            flatten_folds: true,
            merge_coplanar: true,
            dissolve_angle_deg: 0.0,
            protected_edges: Vec::new(),
            merge_distance: 0.0,
            degenerate_threshold: 0.0,
            delete_loose_verts: false,
            delete_loose_edges: false,
            delete_loose_faces: false,
            fill_holes_sides: 0,
            make_planar_iterations: 0,
            split_non_planar_deg: 0.0,
            split_concave: false,
            recalc_normals: false,
        }
    }
}

/// Per-pass tally of one [`cleanup_mesh_report`] run — drives the app's result popup so the user
/// sees WHICH repair fired, not just a net vertex delta (a run that fills a hole and welds a
/// double nets out to "nothing changed" on counts alone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanupStats {
    pub t_junctions_fixed: bool,
    pub verts_welded: usize,
    pub degenerate_removed: usize,
    pub loose: crate::half_edge::LooseCounts,
    pub holes_filled: usize,
    pub planar_faces_fixed: usize,
    pub non_planar_split: usize,
    pub concave_split: usize,
    pub normals_flipped: usize,
}

impl CleanupStats {
    /// True when at least one pass reported work. Used as the no-op gate's escape hatch: several
    /// passes are INVISIBLE to the vert/face/fold counters (a T-junction repair changes none of
    /// them; a hole fill and a concave split RAISE face count) and would otherwise be computed
    /// and then thrown away.
    pub fn did_something(&self) -> bool {
        self.t_junctions_fixed
            || self.verts_welded > 0
            || self.degenerate_removed > 0
            || !self.loose.is_empty()
            || self.holes_filled > 0
            || self.planar_faces_fixed > 0
            || self.non_planar_split > 0
            || self.concave_split > 0
            || self.normals_flipped > 0
    }
}

pub fn cleanup_mesh(m: &HalfEdgeMesh) -> Option<HalfEdgeMesh> {
    cleanup_mesh_with(m, &CleanupOptions::default())
}

/// Parameterized [`cleanup_mesh`] — same gates, caller-chosen passes.
pub fn cleanup_mesh_with(m: &HalfEdgeMesh, opts: &CleanupOptions) -> Option<HalfEdgeMesh> {
    cleanup_mesh_report(m, opts).map(|(mesh, _)| mesh)
}

/// [`cleanup_mesh_with`] that also reports which passes fired.
pub fn cleanup_mesh_report(
    m: &HalfEdgeMesh,
    opts: &CleanupOptions,
) -> Option<(HalfEdgeMesh, CleanupStats)> {
    // Protection rides the SAME thread-local preserve set the boolean seeds: `merge_coplanar`
    // and the valence-2 sweep already consult it, so nothing below needs a protection branch.
    // The guard clears it on every exit path, including the `return None` refusals.
    rebuild::set_preserve_explicit(&opts.protected_edges);
    let _guard = scopeguard_clear();
    let mut stats = CleanupStats::default();

    // WELD FIRST (Blender Merge by Distance + Degenerate Dissolve). These run BEFORE the soup is
    // extracted rather than after the rebuild, so the fold flattener and the coplanar merge below
    // see already-welded geometry — two coplanar faces separated only by a seam of doubles are
    // invisible to `merge_coplanar` until the doubles are gone. Both refuse to collapse a
    // protected edge, so an authored tag still survives the pass (owner 2026-08-05).
    let welded_src;
    let src: &HalfEdgeMesh = if opts.merge_distance > 0.0 || opts.degenerate_threshold > 0.0 {
        let mut w = m.clone();
        if opts.merge_distance > 0.0 {
            stats.verts_welded = w.merge_by_distance_ex(opts.merge_distance, &opts.protected_edges);
        }
        if opts.degenerate_threshold > 0.0 {
            stats.degenerate_removed =
                w.dissolve_degenerate_ex(opts.degenerate_threshold, &opts.protected_edges);
        }
        welded_src = w;
        &welded_src
    } else {
        m
    };

    let mut positions: Vec<glam::Vec3> = Vec::new();
    let mut remap: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for i in 0..src.faces.len() as u32 {
        let fid = FaceId(i);
        if src.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let mut ring: Vec<usize> = src
            .face_vertices(fid)
            .iter()
            .map(|v| {
                *remap.entry(v.0).or_insert_with(|| {
                    positions.push(src.vert(*v).position);
                    positions.len() - 1
                })
            })
            .collect();
        ring.dedup();
        while ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() >= 3 {
            faces.push(ring);
        }
    }
    if faces.is_empty() {
        return None;
    }
    // T-JUNCTION REPAIR (owner 2026-08-06, "EDGE CASE INSET.fl4": intrude + clean mesh gives a
    // result that is "visually perfect but some vertex was not merged properly to segment").
    //
    // A T-junction is a vertex sitting on ANOTHER edge's interior without being part of it. It is
    // topologically legal - `validate()` and `is_manifold()` both pass - and it renders perfectly,
    // which is exactly why it survives a visual check and then breaks the next bevel or boolean.
    // The owner's file carried two, each only 7.16e-7 off the edge it lands on.
    //
    // `repair_t_junctions` already existed and was wired ONLY into the boolean. Mesh Cleanup - the
    // operation whose entire purpose is repairing poisoned bodies - never ran it, so a T-junction
    // was the one defect Cleanup could not clean. Same 1e-4 epsilon the boolean uses.
    //
    // This SPLITS an edge at a point already lying on it; it never removes one, so it cannot
    // violate the protected-edge rule above (a protected edge survives, subdivided at a point that
    // was geometrically on it anyway).
    let (faces, fixed_t_junctions) = {
        let repaired = repair_t_junctions(&positions, &faces, 1e-4);
        if repaired.is_empty() {
            (faces, false)
        } else {
            let changed = repaired != faces;
            (repaired, changed)
        }
    };
    let flat = if opts.flatten_folds {
        let f = flatten_folded_clusters(&positions, &faces);
        if f.is_empty() {
            return None;
        }
        f
    } else {
        faces
    };
    let merged = if opts.merge_coplanar {
        merge_coplanar(&positions, &flat)
    } else {
        Vec::new()
    };
    let use_faces = if merged.is_empty() { &flat } else { &merged };
    // Compact to positions still referenced after soup dissolve_collinear. `from_polygons`
    // allocates one vert per entry in `positions`, so unreferenced mid-edge pins that the
    // soup pass already dropped would otherwise become NO_EDGE orphans that still inflate
    // `vertex_count` and make the no-op gate think nothing improved (owner test-019).
    let mut compact_pos: Vec<glam::Vec3> = Vec::new();
    let mut compact_remap: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let compact_faces: Vec<Vec<usize>> = use_faces
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|&i| {
                    *compact_remap.entry(i).or_insert_with(|| {
                        compact_pos.push(positions[i]);
                        compact_pos.len() - 1
                    })
                })
                .collect()
        })
        .collect();
    let mut out = HalfEdgeMesh::from_polygons(&compact_pos, &compact_faces);
    if opts.dissolve_angle_deg > 0.01 {
        // `limited_dissolve` works on live ids, and `out` was just rebuilt — so the protected
        // set has to be re-resolved by position here rather than carried as ids.
        let protect = protected_half_edges(&out, &opts.protected_edges);
        let _ = out.limited_dissolve(
            opts.dissolve_angle_deg,
            None,
            if protect.is_empty() { None } else { Some(&protect) },
        );
    }
    // Owner test-019 (2026-08-06): free-knife densify + boolean pins left valence-2 collinear
    // verts on face rings. Merge alone dropped face count but kept every pin, so selecting a
    // trapezoid painted a crazy multi-vert outline. Soup dissolve_collinear can miss pins that
    // only become valence-2 after the half-edge rebuild; the live-mesh pass (same as
    // post_boolean_polish) finishes the job. Protected endpoints stay (vert_is_preserved).
    dissolve_collinear_valence2(&mut out, POST_BOOL_COLLINEAR_DOT);

    // --------------------------------------------------------------------------------
    // Blender `Mesh > Clean Up` tail. Ordered the way a repair actually has to run:
    // remove what is not surface (loose) → close what is open (holes) → fix face SHAPE
    // (planar / non-planar / concave) → fix face DIRECTION (winding) last, so it sees the
    // final face set.
    // --------------------------------------------------------------------------------
    stats.t_junctions_fixed = fixed_t_junctions;
    if opts.delete_loose_verts || opts.delete_loose_edges || opts.delete_loose_faces {
        stats.loose = out.delete_loose(
            opts.delete_loose_verts,
            opts.delete_loose_edges,
            opts.delete_loose_faces,
        );
    }
    if opts.fill_holes_sides >= 3 {
        stats.holes_filled = out.fill_holes(opts.fill_holes_sides);
    }
    if opts.make_planar_iterations > 0 {
        stats.planar_faces_fixed = out.make_planar_faces(1.0, opts.make_planar_iterations);
    }
    if opts.split_non_planar_deg > 0.0 {
        stats.non_planar_split = out.split_non_planar_faces(opts.split_non_planar_deg);
    }
    if opts.split_concave {
        stats.concave_split = out.split_concave_faces();
    }
    if opts.recalc_normals {
        stats.normals_flipped = out.recalculate_normals_consistent(true);
    }

    // GATES. The closedness bar used to be `boundary_half_edge_count(&out) == 0` — inherited from
    // the boolean repair path this code grew out of, where both operands are solids by contract.
    // As a bar for a general Mesh Cleanup it was a silent no-op switch: ANY mesh with a rim (a
    // plane, a sheet, an open shell, a scanned import — most of what a cleanup is aimed at) failed
    // it, so cleanup returned None and reported "mesh is already clean". The correct invariant is
    // that cleanup must not TEAR the mesh: the boundary may shrink (hole fill, loose-face removal,
    // coplanar merge) but never grow. A closed input still has to come back closed, because 0 is
    // the only count that is `<= 0`.
    let boundary_ok = boundary_half_edge_count(&out) <= boundary_half_edge_count(m);
    // Full manifoldness is likewise only demanded of inputs that HAD it — an owner directive
    // predating this file (`integrity()`, 2026-08-03: "all tools must work even on open /
    // non-manifold shapes"). `integrity()` — the edit-op bar — is required unconditionally.
    let manifold_ok = m.is_manifold().is_err() || out.is_manifold().is_ok();
    // A cleanup that empties the body is a bug, not a repair. Reachable: `delete_loose_faces` on a
    // decal/sheet body whose faces genuinely share no edges would remove every one of them, and
    // every other gate here is satisfied by an empty mesh (0 boundary, 0 folds, valid).
    if out.face_count() == 0 && m.face_count() > 0 {
        return None;
    }
    let clean = boundary_ok
        && out.validate().is_ok()
        && out.integrity().is_ok()
        && manifold_ok
        && out.count_folded_coplanar_pairs() <= m.count_folded_coplanar_pairs();
    if !clean {
        return None;
    }
    // No-op detection: faces, folds, AND verts — collinear-only cleanup is a real improvement
    // even when face count is already minimal (owner test-019: 36v → clean with same faces).
    let no_face_gain = out.face_count() >= m.face_count();
    let no_fold_gain = out.count_folded_coplanar_pairs() >= m.count_folded_coplanar_pairs();
    let no_vert_gain = out.vertex_count() >= m.vertex_count();
    // A T-JUNCTION REPAIR IS INVISIBLE TO ALL THREE of those counters: it inserts an ALREADY
    // EXISTING welded point into a ring that was skipping it, so faces, folds and verts are all
    // unchanged while the topology genuinely improved. Without this the repair below is computed
    // and then thrown away by the no-op gate - which is what happened on the first attempt.
    // `did_something` generalises that escape hatch to every Clean Up pass with the same problem:
    // filling a hole and splitting a concave face RAISE face count, and re-winding a face or
    // flattening an n-gon changes no counter at all.
    if !stats.did_something() && no_face_gain && no_fold_gain && no_vert_gain {
        return None;
    }
    Some((out, stats))
}

/// Collapse valence-2 collinear mid-edge vertices in place (Blender `use_verts`).
///
/// Used after knife densify cuts so straight segments keep only their real endpoints, and by
/// Mesh Cleanup for bodies already poisoned with pins. Honour any thread-local preserve set
/// currently installed; call with an empty preserve (default) for a free collapse.
pub fn collapse_collinear_pins(mesh: &mut HalfEdgeMesh) {
    dissolve_collinear_valence2(mesh, POST_BOOL_COLLINEAR_DOT);
}

/// Scoped post-cut cleanup: collapse collinear valence-2 pins the CUT created while never
/// dissolving a vertex that existed before it. `keep_verts` = the pre-cut live vertex
/// positions. Owner 2026-08-08 "mesh cleanup after a cut cleaned all my geo": the unscoped
/// collapse ran over the WHOLE mesh and ate authored tessellation (subdivided walls have
/// legitimate collinear valence-2 verts on straight rims). Densify pins are by definition
/// NEW verts, so scoping to post-cut verts loses nothing the cleanup was added for.
pub fn collapse_collinear_pins_scoped(mesh: &mut HalfEdgeMesh, keep_verts: &[glam::Vec3]) {
    rebuild::set_preserve_verts_only(keep_verts);
    dissolve_collinear_valence2(mesh, POST_BOOL_COLLINEAR_DOT);
    rebuild::clear_preserve();
}

/// Resolve undirected endpoint-position pairs to the live half-edges of `m` (BOTH twins),
/// quantized at the same 1e-5 as `rebuild::PRESERVE_Q` so this and the preserve set agree by
/// construction. A pair with no match is dropped — fail closed, never protect a stranger.
fn protected_half_edges(
    m: &HalfEdgeMesh,
    protected: &[(glam::Vec3, glam::Vec3)],
) -> std::collections::HashSet<crate::half_edge::HalfEdgeId> {
    use crate::half_edge::HalfEdgeId;
    let mut out = std::collections::HashSet::new();
    if protected.is_empty() {
        return out;
    }
    let q = |p: glam::Vec3| -> (i64, i64, i64) {
        ((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64)
    };
    let want: std::collections::HashSet<((i64, i64, i64), (i64, i64, i64))> = protected
        .iter()
        .map(|&(a, b)| {
            let (ka, kb) = (q(a), q(b));
            if ka <= kb { (ka, kb) } else { (kb, ka) }
        })
        .collect();
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !m.is_edge_live(he) {
            continue;
        }
        let (ka, kb) = (
            q(m.vert(m.he_source(he)).position),
            q(m.vert(m.he(he).vertex).position),
        );
        let key = if ka <= kb { (ka, kb) } else { (kb, ka) };
        if want.contains(&key) {
            out.insert(he);
        }
    }
    out
}

/// RAII: clear the thread-local preserve set on every exit path of `mesh_boolean`.
struct PreserveGuard;
impl Drop for PreserveGuard {
    fn drop(&mut self) {
        rebuild::clear_preserve();
    }
}
fn scopeguard_clear() -> PreserveGuard {
    PreserveGuard
}

/// Signed mesh volume via the divergence theorem (fan-triangulated faces).
/// Positive for outward-wound closed meshes; f64 accumulation for long thin geometry.
/// Where a RESULT face came from. Stage P1 of `docs/planning/poly/BOOLEAN_PROVENANCE.md`.
///
/// Result indices are new, so provenance cannot be an index remap; faces are classified by
/// QUANTIZED POSITION against the per-operand input sets the boolean already registers
/// (`rebuild::set_preserve_from`, `PRESERVE_Q = 1e-5`) -- the same mechanism the weld has
/// been trusting on this exact geometry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaceOrigin {
    /// Every vertex came from operand A (the target) -- untouched original surface.
    OperandA,
    /// Every vertex came from operand B (the cutter) -- e.g. the barrel of a subtracted
    /// cylinder. This is what Bevel Smart Select rule 2 must ignore.
    OperandB,
    /// Created by the intersection, or spanning both operands -- e.g. the rim ring where a
    /// cylindrical hole meets the flat face. This is what rule 2 must KEEP.
    Cut,
}

/// Per-result-face provenance. ADVISORY: every consumer must behave correctly when this is
/// absent (old saves, the BSP path, a refused boolean) and must fail CLOSED -- e.g. bevel
/// everything rather than silently skipping edges on missing data.
#[derive(Clone, Debug, Default)]
pub struct BoolProvenance {
    /// Indexed by result `FaceId.0`. Empty when provenance could not be collected.
    pub face_origin: Vec<FaceOrigin>,
    /// Indexed by result `HalfEdgeId.0`: true when this edge did NOT exist on either input,
    /// i.e. the boolean CREATED it. The flat cross-face support edges a cut leaves behind
    /// (owner screenshot, two marked with a red X) are exactly these. Empty when not collected.
    pub edge_is_new: Vec<bool>,
}

impl BoolProvenance {
    pub fn is_empty(&self) -> bool {
        self.face_origin.is_empty()
    }

    /// Edges the boolean CREATED that lie flat (no plane change) -- the artifacts the owner
    /// wants gone from a cut face. Returned as canonical `min(he, twin)` ids.
    ///
    /// These must be DISSOLVED, never merely excluded from a bevel selection: §6 of
    /// `docs/planning/poly/BEVEL.md` records that excluding a coplanar edge drives the
    /// surrounding bevel rails into it and produces folds. An edge that ALREADY existed on an
    /// input is never returned, however flat -- the boolean must not change geometry it did
    /// not cut (owner rule), and a user-authored flat edge is authored intent.
    pub fn new_flat_edges(&self, mesh: &HalfEdgeMesh) -> Vec<u32> {
        use crate::half_edge::{HalfEdgeId, NO_TWIN};
        if self.edge_is_new.is_empty() {
            return Vec::new(); // fail closed: unknown provenance dissolves nothing
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            if !self.edge_is_new.get(i as usize).copied().unwrap_or(false) {
                continue;
            }
            if !mesh.edge_is_flat(he, 0.0) {
                continue;
            }
            let twin = mesh.he(he).twin;
            let canon = if twin != NO_TWIN { i.min(twin.0) } else { i };
            if seen.insert(canon) {
                out.push(canon);
            }
        }
        out.sort_unstable();
        out
    }
    /// Origin of a result face, or `None` when unknown (index out of range / not collected).
    pub fn face(&self, f: crate::half_edge::FaceId) -> Option<FaceOrigin> {
        self.face_origin.get(f.0 as usize).copied()
    }
    /// True when BOTH faces of this edge came from the cutter -- the cutter's own
    /// tessellation, e.g. the vertical seams of a subtracted cylinder. An edge with one face
    /// from each operand is the RIM and is deliberately not matched here.
    pub fn edge_is_cutter_interior(
        &self,
        mesh: &HalfEdgeMesh,
        he: crate::half_edge::HalfEdgeId,
    ) -> bool {
        use crate::half_edge::{NO_FACE, NO_TWIN};
        if self.is_empty() {
            return false; // fail closed: unknown provenance never excludes an edge
        }
        let e = mesh.he(he);
        if e.face == NO_FACE || e.twin == NO_TWIN {
            return false;
        }
        let tf = mesh.he(e.twin).face;
        if tf == NO_FACE {
            return false;
        }
        self.face(e.face) == Some(FaceOrigin::OperandB)
            && self.face(tf) == Some(FaceOrigin::OperandB)
    }
}

/// The BVH prune bound is widened by this relative slack so f32 rounding in the point→AABB
/// distance can never discard a triangle the exact test would have accepted. Widening only ADDS
/// candidates; every survivor still runs the exact `point_triangle_dist_sq`, so the
/// classification matches a linear scan.
const PRUNE_SLACK: f32 = 1.0 + 1e-5;

/// Spatial index over ONE operand's fan-triangles, for "is any triangle within eps of p?".
///
/// Reuses the crate's SAH BVH2 (`crate::bvh::build_bvh`) rather than a bespoke tree — the same
/// builder the PT triangle BLAS uses. It is a CANDIDATE FILTER only: a node is pruned solely
/// when its AABB (which encloses every triangle beneath it) is farther than the tolerance, so
/// no triangle that could pass the exact test is ever skipped.
struct TriIndex {
    tris: Vec<[glam::Vec3; 3]>,
    nodes: Vec<crate::bvh::BvhNode>,
    /// `nodes` leaf ranges index this; entries are indices into `tris`.
    order: Vec<u32>,
}

impl TriIndex {
    fn build(tris: Vec<[glam::Vec3; 3]>) -> Self {
        let prims: Vec<(u32, crate::bvh::Aabb)> = tris
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let lo = t[0].min(t[1]).min(t[2]);
                let hi = t[0].max(t[1]).max(t[2]);
                (i as u32, crate::bvh::Aabb { min: lo.to_array(), max: hi.to_array() })
            })
            .collect();
        let (nodes, order) = crate::bvh::build_bvh(&prims);
        Self { tris, nodes, order }
    }

    /// Squared distance from `p` to an axis-aligned box — 0 inside, and always ≤ the squared
    /// distance to any triangle the box encloses, which is what makes the prune exact.
    fn aabb_dist_sq(p: glam::Vec3, n: &crate::bvh::BvhNode) -> f32 {
        let lo = glam::Vec3::from(n.aabb_min);
        let hi = glam::Vec3::from(n.aabb_max);
        let d = (lo - p).max(p - hi).max(glam::Vec3::ZERO);
        d.length_squared()
    }

    /// True when ANY indexed triangle is within `sqrt(eps_sq)` of `p`. Early-exits on the first
    /// hit. `stack` is caller-owned scratch so a per-face query allocates nothing.
    fn any_within(&self, p: glam::Vec3, eps_sq: f32, stack: &mut Vec<u32>) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let prune_sq = eps_sq * PRUNE_SLACK;
        stack.clear();
        stack.push(0);
        while let Some(ni) = stack.pop() {
            let n = &self.nodes[ni as usize];
            if Self::aabb_dist_sq(p, n) > prune_sq {
                continue;
            }
            if n.count == 0 {
                stack.push(n.first);
                stack.push(n.first + 1);
                continue;
            }
            for k in n.first..n.first + n.count {
                let t = &self.tris[self.order[k as usize] as usize];
                if point_triangle_dist_sq(p, t[0], t[1], t[2]) <= eps_sq {
                    return true;
                }
            }
        }
        false
    }
}

/// Classify every live face of `result` by SURFACE MEMBERSHIP against the two inputs.
///
/// The obvious test -- "all this face's vertices were input vertices" -- is WRONG and the
/// cylinder test caught it: on a through-cut, every corner of the hole's barrel quads is
/// created by the intersection, so not one original cutter vertex survives on them and the
/// whole barrel would be misfiled as `Cut`. What actually identifies a face is the SURFACE
/// IT LIES ON: a barrel quad lies on the cutter's surface whether or not its corners are
/// original. So each result face is tested by its centroid's distance to each operand's
/// triangles.
///
/// `eps` is scale-relative to the result's bbox; a face that lies on BOTH surfaces
/// (coincident/touching input faces) is reported as `Cut`, because guessing in favour of
/// excluding geometry is the failure a bevel user cannot see.
///
/// The membership test goes through [`TriIndex`]. It used to be a linear `.any()` over EVERY
/// fan-triangle of BOTH operands — O(F_out × (T_a + T_b)), which put ~166 ms of a 410 ms
/// 6k-face boolean in here and grew quadratically. The index only REDUCES the candidate set;
/// the surviving candidates take the same exact distance test, so the classification is
/// unchanged.
pub(crate) fn collect_provenance(
    result: &HalfEdgeMesh,
    a_in: &HalfEdgeMesh,
    b_in: &HalfEdgeMesh,
) -> BoolProvenance {
    use crate::half_edge::{FaceId, NO_EDGE};
    use glam::Vec3;
    let mut face_origin = vec![FaceOrigin::Cut; result.faces.len()];

    // Flatten each operand to triangles once.
    let tris_of = |m: &HalfEdgeMesh| -> Vec<[Vec3; 3]> {
        let mut t = Vec::new();
        for i in 0..m.faces.len() as u32 {
            let f = FaceId(i);
            if m.face(f).half_edge == NO_EDGE {
                continue;
            }
            let vs = m.face_vertices(f);
            if vs.len() < 3 {
                continue;
            }
            let p0 = m.vert(vs[0]).position;
            for w in vs.windows(2).skip(1) {
                t.push([p0, m.vert(w[0]).position, m.vert(w[1]).position]);
            }
        }
        t
    };
    let (ia, ib) = (TriIndex::build(tris_of(a_in)), TriIndex::build(tris_of(b_in)));

    // Scale-relative tolerance from the result's bounding box.
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for v in &result.vertices {
        lo = lo.min(v.position);
        hi = hi.max(v.position);
    }
    let eps = ((hi - lo).length().max(1e-4) * 1e-4).max(1e-6);
    let eps_sq = eps * eps;
    let mut stack: Vec<u32> = Vec::new();

    for i in 0..result.faces.len() as u32 {
        let f = FaceId(i);
        if result.face(f).half_edge == NO_EDGE {
            continue;
        }
        let vs = result.face_vertices(f);
        if vs.is_empty() {
            continue;
        }
        let c: Vec3 =
            vs.iter().map(|&v| result.vert(v).position).sum::<Vec3>() / vs.len() as f32;
        face_origin[i as usize] = match (
            ia.any_within(c, eps_sq, &mut stack),
            ib.any_within(c, eps_sq, &mut stack),
        ) {
            (true, false) => FaceOrigin::OperandA,
            (false, true) => FaceOrigin::OperandB,
            // On both (coincident faces) or on neither: ambiguous -> Cut, so consumers keep
            // the geometry. Never guess in favour of excluding.
            _ => FaceOrigin::Cut,
        };
    }
    // Which edges did the boolean CREATE? Read while the PRESERVE sets are still armed.
    let mut edge_is_new = vec![false; result.half_edges.len()];
    for i in 0..result.half_edges.len() as u32 {
        let he = crate::half_edge::HalfEdgeId(i);
        if !result.is_edge_live(he) {
            continue;
        }
        let p0 = result.vert(result.he_source(he)).position;
        let p1 = result.vert(result.he(he).vertex).position;
        edge_is_new[i as usize] = !rebuild::edge_was_input(p0, p1);
    }
    BoolProvenance { face_origin, edge_is_new }
}

pub fn mesh_signed_volume(m: &HalfEdgeMesh) -> f64 {
    let mut vol = 0.0f64;
    for i in 0..m.faces.len() {
        let fid = FaceId(i as u32);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs = m.face_vertices(fid);
        if vs.len() < 3 {
            continue;
        }
        let p0 = m.vert(vs[0]).position.as_dvec3();
        for k in 1..vs.len() - 1 {
            let p1 = m.vert(vs[k]).position.as_dvec3();
            let p2 = m.vert(vs[k + 1]).position.as_dvec3();
            vol += p0.dot(p1.cross(p2)) / 6.0;
        }
    }
    vol
}

#[cfg(test)]
mod tests;
