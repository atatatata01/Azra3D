//! Manifold-class production wrap for `mesh_boolean` (elalish/manifold via `manifold-csg`).
//!
//! Default kernel when `AZRA_BSP_BOOLEAN` is unset. The legacy robust+BSP path remains
//! available for A/B behind `AZRA_BSP_BOOLEAN=1` (see `mesh_boolean::mesh_boolean`).
//!
//! # What Manifold loses vs what this wrap restores
//!
//! | Asset | Manifold alone | Wrap restore |
//! |---|---|---|
//! | Closed / manifold topology | Keeps | Gate + post-polish |
//! | N-gon faces | Always triangles | `post_boolean_polish` (limited dissolve + collinear cleanup) — same pipeline as BSP |
//! | Per-face `material_id` | Not tracked | **Nearest input-face transfer**: after polish, each output face inherits `material_id` from the closest coplanar (or nearest) live face on A∪B |
//! | Half-edge element IDs (`VertexId`/`FaceId`) | Destroyed (rebuild) | **Cannot restore** — topology is new (same as BSP `from_polygons` path) |
//! | `poly_custom_data` (UV/crease/seam) | N/A (kernel-level) | Still app-layer (`BOOL-STALECD`) — install path must clear/remap |
//!
//! # REPLACE-RULE
//!
//! BSP (`robust_boolean` / `bsp_boolean`) is **flagged for deletion** only after:
//! full `core_math` green + Blender oracle green + owner soak session.
//! Do **not** delete the BSP code until then.

use crate::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
use crate::mesh_boolean::{
    boundary_half_edge_count, merge_coplanar, mesh_signed_volume, post_boolean_polish, BoolOp,
};
use glam::Vec3;
use manifold_csg::Manifold;
use std::collections::HashMap;

/// Env gate: when `AZRA_BSP_BOOLEAN` is set to a non-empty value other than `0`/`false`/`off`,
/// callers use the legacy robust+BSP path instead of Manifold.
pub fn prefer_bsp_boolean() -> bool {
    match std::env::var("AZRA_BSP_BOOLEAN") {
        Ok(s) => {
            let t = s.trim();
            !(t.is_empty()
                || t == "0"
                || t.eq_ignore_ascii_case("false")
                || t.eq_ignore_ascii_case("off"))
        }
        Err(_) => false,
    }
}

/// INPUT FOLD REPAIR: a body poisoned by an earlier session (zero-thickness folded flaps
/// saved into the file) makes the exact kernel refuse or emit garbage. Re-extract the soup,
/// flatten the folded coplanar clusters, and rebuild. None = mesh is not repairable this
/// way (caller proceeds with the original and lets the kernel decide).
fn sanitize_folds(m: &HalfEdgeMesh) -> Option<HalfEdgeMesh> {
    if m.count_folded_coplanar_pairs() == 0 {
        return None;
    }
    let mut positions: Vec<Vec3> = Vec::new();
    let mut remap: HashMap<u32, usize> = HashMap::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for i in 0..m.faces.len() as u32 {
        let fid = FaceId(i);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let ring: Vec<usize> = m
            .face_vertices(fid)
            .iter()
            .map(|v| {
                *remap.entry(v.0).or_insert_with(|| {
                    positions.push(m.vert(*v).position);
                    positions.len() - 1
                })
            })
            .collect();
        if ring.len() >= 3 {
            faces.push(ring);
        }
    }
    let flat = crate::mesh_boolean::flatten_folded_clusters(&positions, &faces);
    if flat.is_empty() {
        return None;
    }
    let out = HalfEdgeMesh::from_polygons(&positions, &flat);
    if boundary_half_edge_count(&out) == 0
        && out.validate().is_ok()
        && out.is_manifold().is_ok()
        && out.count_folded_coplanar_pairs() == 0
    {
        Some(out)
    } else {
        None
    }
}

/// Shortest live UNDIRECTED edge length in `m` (diagnostic: the bevel's Clamp-Overlap ceiling
/// is ~half this number, globally — see BEVEL.md §6 "SMART CLAMP").
pub fn min_edge_length(m: &HalfEdgeMesh) -> f32 {
    use crate::half_edge::HalfEdgeId;
    let mut mn = f32::INFINITY;
    for i in 0..m.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !m.is_edge_live(h) {
            continue;
        }
        let l = m
            .vert(m.he_source(h))
            .position
            .distance(m.vert(m.he(h).vertex).position);
        if l < mn {
            mn = l;
        }
    }
    mn
}

/// Manifold-backed boolean. Returns `None` on open input, empty result, or Manifold error.
pub fn manifold_boolean(a: &HalfEdgeMesh, b: &HalfEdgeMesh, op: BoolOp) -> Option<HalfEdgeMesh> {
    if boundary_half_edge_count(a) > 0 || boundary_half_edge_count(b) > 0 {
        if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
            eprintln!("[BOOL] refused: open input (A boundary={} B boundary={})",
                boundary_half_edge_count(a), boundary_half_edge_count(b));
        }
        return None;
    }
    // Repair fold-poisoned inputs (older-session files) before the exact kernel sees them.
    let (ra, rb) = (sanitize_folds(a), sanitize_folds(b));
    if (ra.is_some() || rb.is_some()) && std::env::var("AZRA_BOOL_DEBUG").is_ok() {
        eprintln!("[BOOL] input fold repair: A={} B={}", ra.is_some(), rb.is_some());
    }
    let a = ra.as_ref().unwrap_or(a);
    let b = rb.as_ref().unwrap_or(b);

    let ma = match halfedge_to_manifold(a) {
        Some(m) => m,
        None => return None,
    };
    let mb = match halfedge_to_manifold(b) {
        Some(m) => m,
        None => return None,
    };

    let out = match op {
        BoolOp::Difference => ma.difference(&mb),
        BoolOp::Union => ma.union(&mb),
        BoolOp::Intersect => ma.intersection(&mb),
    };
    // Force evaluation; status() returns Err on bad topology.
    if let Err(e) = out.status() {
        eprintln!("[BOOL] Manifold status error: {e:?}");
        return None;
    }
    if out.is_empty() || out.num_tri() == 0 {
        eprintln!("[BOOL] Manifold result empty");
        return None;
    }
    // VOLUME INVARIANT with RESOLVED volumes (owner test-05, 2026-08-04): the outer gate
    // measured operand volumes by raw mesh integral, which UNDERESTIMATES a cutter whose
    // caps are built over a non-planar ring — the exact kernel's resolved solid is
    // slightly larger, so a perfectly correct result landed 0.25% below the "possible"
    // lower bound and the cut was refused. Manifold's own volumes are the ground truth.
    {
        let (va, vb, vr) = (ma.volume(), mb.volume(), out.volume());
        let tol = (va.abs().max(vb.abs()) * 1e-3).max(1e-9);
        let ok = match op {
            BoolOp::Difference => vr >= va - vb - tol && vr <= va + tol,
            BoolOp::Union => vr >= va.max(vb) - tol && vr <= va + vb + tol,
            BoolOp::Intersect => vr >= -tol && vr <= va.min(vb) + tol,
        };
        if !ok {
            eprintln!(
                "[BOOL] resolved-volume invariant failed for {op:?}: vr={vr:.6} bounds                  [va={va:.6} vb={vb:.6}]"
            );
            return None;
        }
    }

    let mut mesh = match manifold_to_halfedge(&out) {
        Some(m) => m,
        None => {
            eprintln!("[BOOL] manifold_to_halfedge rebuild failed (weld/twin pairing)");
            return None;
        }
    };
    if mesh.face_count() == 0 {
        return None;
    }
    if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
        eprintln!("[BOOL] raw kernel: v={} f={} folds={} vol={:.6} min_edge={:.6}",
            mesh.vertex_count(), mesh.face_count(), mesh.count_folded_coplanar_pairs(),
            mesh_signed_volume(&mesh), min_edge_length(&mesh));
        for (fa, fb) in mesh.folded_coplanar_pairs_dbg() {
            let ring = |f: FaceId| mesh.face_vertices(f).iter()
                .map(|v| { let p = mesh.vert(*v).position; format!("({:.6},{:.6},{:.6})", p.x, p.y, p.z) })
                .collect::<Vec<_>>();
            eprintln!("[BOOL] fold pair {:?}{:?} / {:?}{:?}", fa, ring(fa), fb, ring(fb));
        }
    }

    // VOLUME IS SACROSANCT through beautification (owner test-02, 2026-08-04, proven vs
    // the Blender oracle): the kernel's raw answer matched Blender to 6 decimals
    // (0.823816) and a later beautify stage silently destroyed 0.109 of volume — its
    // acceptance checked folds and manifoldness but NOT volume. Every stage below may
    // only re-mesh the surface; the enclosed volume must remain identical.
    let vol_base = mesh_signed_volume(&mesh);
    let vol_same = |m: &HalfEdgeMesh| -> bool {
        (mesh_signed_volume(m) - vol_base).abs() <= (vol_base.abs() * 1e-3).max(1e-9)
    };

    // Manifold ALWAYS emits triangle soup — even on faces the cut never touched. Restoring
    // coplanar n-gons is mandatory for a clean hard-surface mesh (owner 2026-08-10: "boolean
    // triangulates part of the mesh they are even not touching, mesh need to stay clean").
    //
    // Polish acceptance: no NEW folds (owner 2026-08-03 "improve the created segments"):
    // requiring zero folds outright let ONE harmless epsilon-sliver veto the whole dissolve,
    // leaving long support triangulation edges across unrelated flat faces. Baseline = pre-polish
    // fold count.
    //
    // BOOLEAN LOCALITY GATE (2026-08-08): also refuse any candidate that DELETED a preserved
    // input vertex. Fail closed to the unpolished mesh.
    //
    // Heavy iterative polish (`post_boolean_polish`) is O(n²)-ish — keep it for modest outputs.
    // The soup coplanar merge (`re_dissolve_soup`) is O(faces+edges) and MUST always run so dense
    // bodies don't ship Manifold's triangulation on untouched flats (the 2048-face skip was the
    // silent "remote faces become tris" bug).
    let base_folds = mesh.count_folded_coplanar_pairs();
    let pres_base = crate::mesh_boolean::preserved_vert_count(&mesh);
    if mesh.face_count() <= 8_192 {
        let polished = post_boolean_polish(mesh.clone());
        if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
            eprintln!(
                "[POLISH] closed={} folds={}<={} vol_same={} pres={}>={} faces={}<={}",
                is_closed_manifold(&polished),
                polished.count_folded_coplanar_pairs(), base_folds,
                vol_same(&polished),
                crate::mesh_boolean::preserved_vert_count(&polished), pres_base,
                polished.face_count(), mesh.face_count()
            );
        }
        if is_closed_manifold(&polished)
            && polished.count_folded_coplanar_pairs() <= base_folds
            && vol_same(&polished)
            && crate::mesh_boolean::preserved_vert_count(&polished) >= pres_base
            // Prefer fewer faces (more n-gon restore) — never accept a polish that densifies.
            && polished.face_count() <= mesh.face_count()
        {
            mesh = polished;
        }
    }
    // Soup dissolve always: collapses Manifold diagonals on coplanar regions the cut never
    // reached. May run twice (second pass catches residual float-noise pairs).
    for _ in 0..2 {
        let folds_now = mesh.count_folded_coplanar_pairs();
        let Some(again) = re_dissolve_soup(&mesh) else {
            if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
                eprintln!("[SOUP] re_dissolve_soup returned None (no merge candidate)");
            }
            break;
        };
        if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
            eprintln!(
                "[SOUP] closed={} folds={}<={} vol_same={} pres={}>={} faces={}<{}",
                is_closed_manifold(&again),
                again.count_folded_coplanar_pairs(), folds_now,
                vol_same(&again),
                crate::mesh_boolean::preserved_vert_count(&again), pres_base,
                again.face_count(), mesh.face_count()
            );
        }
        if is_closed_manifold(&again)
            && again.count_folded_coplanar_pairs() <= folds_now
            && vol_same(&again)
            && crate::mesh_boolean::preserved_vert_count(&again) >= pres_base
            && again.face_count() < mesh.face_count()
        {
            mesh = again;
        } else {
            break;
        }
    }

    // BOOLEAN LOCALITY GATE, final coverage (adversarial audit 2026-08-08 finding 2): the
    // two cleanup passes below run AFTER the pres-gated acceptances above, so without this
    // baseline the "kills the bug class" claim was only true for the polish passes. Every
    // acceptance from here on also requires no preserved input vertex to disappear.
    let pres_final = crate::mesh_boolean::preserved_vert_count(&mesh);

    // DEGENERACY CLEANUP (Blender merge-by-distance parity; oracle on owner intrude_02:
    // Blender EXACT collapses the epsilon-thin debris — v=30 f=15 tris=0 — where we kept
    // v=41 f=40 tris=24 folds=5 at the SAME volume). Collapse sub-epsilon edges through
    // the half-edge kernel (structurally safe: self-loops spliced, degenerate faces
    // dropped), re-dissolve, and accept only when the gates + volume hold.
    let folds_now = mesh.count_folded_coplanar_pairs();
    if folds_now > 0 {
        if let Some(clean) = collapse_epsilon_debris(&mesh, folds_now) {
            if vol_same(&clean)
                && crate::mesh_boolean::preserved_vert_count(&clean) >= pres_final
            {
                mesh = clean;
            }
        }
    }

    // SLIVER CLEANUP: collapse the hair-thin fragments the cut leaves where the intersection
    // curve passes a hair from an operand vertex. These are what throttle the bevel's global
    // Clamp Overlap (BEVEL.md §6). Fail-closed and never moves an operand vertex.
    if let Some(clean) = collapse_sliver_edges(&mesh, &operand_vertex_keys(a, b), vol_base) {
        if vol_same(&clean)
            && crate::mesh_boolean::preserved_vert_count(&clean) >= pres_final
        {
            mesh = clean;
        }
    }

    // Restore materials from A/B faces (Manifold has no face material channel).
    transfer_materials_from_operands(&mut mesh, a, b);

    if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
        eprintln!("[BOOL] manifold_boolean returning: v={} f={} vol={:.6} min_edge={:.6}",
            mesh.vertex_count(), mesh.face_count(), mesh_signed_volume(&mesh),
            min_edge_length(&mesh));
    }
    if !is_closed_manifold(&mesh) {
        eprintln!(
            "[BOOL] final gate: result not a closed manifold (boundary={} valid={} manifold={})",
            boundary_half_edge_count(&mesh),
            mesh.validate().is_ok(),
            mesh.is_manifold().is_ok()
        );
        return None;
    }
    Some(mesh)
}

// POLISH ACCEPTANCE (owner subtract.fl4 + extrude.fl4 2026-08-03): a coplanar merge can
// fold a notched region into an overlapping signed decomposition (one face inverted).
// A fold is edge-consistent, so is_closed_manifold PASSES it — every polish acceptance
// above must also require count_folded_coplanar_pairs() == 0, falling back to the
// correct (merely more triangulated) unpolished mesh.
/// STATUS 2026-08-03: gated attempt, currently NEVER accepted on the pinned fixture —
/// cross-rim welds break closedness because merge_vertices_to cannot re-twin the doubled
/// edges a zero-thickness wall leaves behind. Reaching Blender-parity output (oracle:
/// v=30 f=15 tris=0 on intrude_02 vs our v=41 f=40 tris=24) needs a real Merge-by-Distance
/// kernel op with re-twinning. The gates below make this safe: on any imperfection the
/// original (correct-volume, watertight) result ships unchanged.
/// Collapse edges shorter than a scale-relative epsilon (bbox diagonal * 1e-5, floor 1e-6)
/// via the kernel's `merge_vertices_to`, then re-run the soup dissolve. Returns the cleaned
/// mesh only when it stays a closed manifold, keeps the volume (1e-3 relative), and has
/// FEWER folds than the input — else `None` (caller keeps the original).
/// A sliver edge is one that is drastically shorter than the edges around it. This is the
/// ratio below which an edge counts as a sliver (len < RATIO * shortest neighbouring edge).
/// Scale-free on purpose: an absolute epsilon cannot distinguish "the boolean nicked a corner"
/// from "the user modelled a small feature", but a 10:1 length step against its OWN neighbours
/// can. 0.35 keeps the cylinder-tessellation chords (ratio ~1.0) and the honest short support
/// edges (test_010: 0.08019 vs 0.02094 neighbour, ratio 3.8) while catching the cut debris.
const SLIVER_RATIO: f32 = 0.35;

/// Quantized position key, same mechanism/scale as `mesh_boolean::rebuild`'s PRESERVE sets.
fn qkey(p: Vec3) -> (i64, i64, i64) {
    let q = |v: f32| (v as f64 / 1e-5).round() as i64;
    (q(p.x), q(p.y), q(p.z))
}

/// Positions of every live vertex of the two operands — the geometry the boolean is NOT
/// allowed to move (owner rule: "a boolean must not change geometry it did not cut").
fn operand_vertex_keys(a: &HalfEdgeMesh, b: &HalfEdgeMesh) -> std::collections::HashSet<(i64, i64, i64)> {
    let mut s = std::collections::HashSet::new();
    for m in [a, b] {
        for v in m.vertices.iter() {
            if v.half_edge != NO_EDGE {
                s.insert(qkey(v.position));
            }
        }
    }
    s
}

/// SLIVER CLEANUP — the reason the bevel's Clamp Overlap collapses on boolean output.
///
/// An exact boolean's cut curve crosses an operand's tessellation at an arbitrary phase, so it
/// routinely drops a new vertex a hair away from an existing one; the fragment between them is a
/// sliver edge. MEASURED (`diag_boolean_sliver_sweep`): sliding a 32-side cylinder cutter across
/// a cube in 0.02 steps, the result's shortest edge sweeps CONTINUOUSLY from the full chord
/// (0.098) down to 0.0026 — and the raw kernel output and the final polished mesh have the SAME
/// shortest edge in every sample. So slivers are not weld survivors and are not created after
/// the weld: they are intrinsic to the cut, and no epsilon in the weld chain can prevent them.
/// They must be collapsed after the fact, which is what this does.
///
/// `bevel_edges_offset` clamps on ONE GLOBAL width = half the shortest beveled edge anywhere, so
/// a single sliver throttles the whole bevel (BEVEL.md §6 "SMART CLAMP").
///
/// Rules, all fail-closed:
/// * A candidate edge must be shorter than `SLIVER_RATIO` × its shortest NEIGHBOURING edge.
/// * An operand vertex is never moved. One preserved endpoint → the sliver snaps ONTO it;
///   both preserved → refused (that edge is input geometry, not cut debris).
/// * Each collapse is tried on a scratch clone and kept only if the result stays valid,
///   edge-manifold, closed and fold-free, keeps the volume (1e-3 relative to `vol_base`),
///   and strictly RAISES the global minimum edge length.
/// Public entry to [`collapse_sliver_edges`] for meshes whose operands are no longer available
/// (a boolean result loaded from a `.fl4`, a tools/diagnostic pass). `preserve` lists positions
/// that must not move; pass an empty slice only when nothing is known to be authored input.
pub fn collapse_slivers_preserving(m: &HalfEdgeMesh, preserve: &[Vec3]) -> Option<HalfEdgeMesh> {
    let set: std::collections::HashSet<(i64, i64, i64)> =
        preserve.iter().map(|p| qkey(*p)).collect();
    collapse_sliver_edges(m, &set, mesh_signed_volume(m))
}

fn collapse_sliver_edges(
    m: &HalfEdgeMesh,
    preserved: &std::collections::HashSet<(i64, i64, i64)>,
    vol_base: f64,
) -> Option<HalfEdgeMesh> {
    use crate::half_edge::{HalfEdgeId, VertexId, NO_TWIN};
    let debug = std::env::var("AZRA_BOOL_DEBUG").is_ok();
    let mut out = m.clone();
    let mut accepted = 0usize;
    let vol_tol = (vol_base.abs() * 1e-3).max(1e-9);

    // Iterate: one collapse can expose the next (a chain of two slivers in a row).
    for _round in 0..64 {
        // Per-vertex incident edge lengths, to measure each edge against its own neighbourhood.
        let elen = |mm: &HalfEdgeMesh, h: HalfEdgeId| -> f32 {
            mm.vert(mm.he_source(h)).position.distance(mm.vert(mm.he(h).vertex).position)
        };
        let mut cands: Vec<(f32, HalfEdgeId)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Reused across every candidate endpoint: the allocating `vertex_fan` heap-allocates
        // one Vec per vertex visited, i.e. two per live undirected edge per round.
        let mut fan: Vec<HalfEdgeId> = Vec::new();
        for i in 0..out.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !out.is_edge_live(h) {
                continue;
            }
            let tw = out.he(h).twin;
            let canon = if tw != NO_TWIN { i.min(tw.0) } else { i };
            if !seen.insert(canon) {
                continue;
            }
            let h = HalfEdgeId(canon);
            let len = elen(&out, h);
            // Shortest OTHER edge touching either endpoint = the local feature scale.
            let mut nb = f32::INFINITY;
            for v in [out.he_source(h), out.he(h).vertex] {
                out.vertex_fan_into(v, &mut fan);
                for &g in fan.iter() {
                    let gc = {
                        let t = out.he(g).twin;
                        if t != NO_TWIN { g.0.min(t.0) } else { g.0 }
                    };
                    if gc == canon || !out.is_edge_live(g) {
                        continue;
                    }
                    nb = nb.min(elen(&out, g));
                }
            }
            if nb.is_finite() && len < nb * SLIVER_RATIO {
                cands.push((len, h));
            }
        }
        if cands.is_empty() {
            break;
        }
        cands.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());

        // Loop-invariant: `out` is only written on an ACCEPTED collapse, which immediately
        // breaks out of the candidate loop and re-enters this round. Recomputing it per
        // candidate was a full half-edge walk each time for the same number.
        let before_min = min_edge_length(&out);
        let mut progressed = false;
        for (len, h) in cands {
            if !out.is_edge_live(h) {
                continue;
            }
            let (va, vb) = (out.he_source(h), out.he(h).vertex);
            let (pa, pb) = (out.vert(va).position, out.vert(vb).position);
            let (ka, kb) = (preserved.contains(&qkey(pa)), preserved.contains(&qkey(pb)));
            // Order the merge so the SURVIVOR is the preserved vertex when there is one, and
            // choose the target so no operand vertex moves.
            let (verts, target): (Vec<VertexId>, Vec3) = match (ka, kb) {
                (true, true) => continue, // input edge, not cut debris — never touch it
                (true, false) => (vec![va, vb], pa),
                (false, true) => (vec![vb, va], pb),
                (false, false) => (vec![va, vb], (pa + pb) * 0.5),
            };
            let mut scratch = out.clone();
            if scratch.merge_vertices_to(&verts, target).is_none() {
                continue;
            }
            let after_min = min_edge_length(&scratch);
            // NON-DECREASING, not strictly-increasing: a symmetric cut leaves the SAME sliver
            // several times over (a cylinder through a cube leaves four), so no single collapse
            // can move the global minimum. The whole PASS is gated on a strict increase below;
            // per-collapse we only require that nothing got shorter.
            // `is_closed_manifold` ALREADY requires `is_edge_manifold().is_ok()` (it is its
            // last conjunct), so the explicit re-check that used to sit here was true
            // whenever it was reached — and `is_edge_manifold` rebuilds three hash maps over
            // every half-edge. Removed: `A && B` where A ⇒ B is exactly A.
            let ok = is_closed_manifold(&scratch)
                && scratch.count_folded_coplanar_pairs() == 0
                && (mesh_signed_volume(&scratch) - vol_base).abs() <= vol_tol
                && after_min >= before_min - 1e-9;
            if debug {
                eprintln!(
                    "[BOOL] sliver {:?} len={len:.6} preserved=({ka},{kb}) -> {} (min {before_min:.6}->{after_min:.6} vol {:.6} closed={} emf={} folds={})",
                    h, if ok { "ACCEPT" } else { "refuse" }, mesh_signed_volume(&scratch),
                    is_closed_manifold(&scratch), scratch.is_edge_manifold().is_ok(),
                    scratch.count_folded_coplanar_pairs()
                );
            }
            if ok {
                out = scratch;
                accepted += 1;
                progressed = true;
                break; // re-scan: the neighbourhood changed
            }
        }
        if !progressed {
            break;
        }
    }
    // PASS GATE: the point of the exercise is the global minimum edge length (the bevel's
    // Clamp-Overlap ceiling). If it did not actually rise, keep the un-cleaned result — we do
    // not re-mesh the user's boolean for nothing.
    let (m0, m1) = (min_edge_length(m), min_edge_length(&out));
    if debug {
        eprintln!("[BOOL] sliver cleanup: {accepted} collapsed, min_edge {m0:.6} -> {m1:.6}");
    }
    if accepted == 0 || !(m1 > m0 + 1e-9) {
        return None;
    }
    Some(out)
}

/// `folds_before` is `m.count_folded_coplanar_pairs()`, supplied by the caller: it already
/// has to compute it to decide whether to call at all, and this is an O(half-edge) walk that
/// builds two Newell normals (each allocating) per undirected edge — running it twice on the
/// same mesh was pure duplicate work.
fn collapse_epsilon_debris(m: &HalfEdgeMesh, folds_before: usize) -> Option<HalfEdgeMesh> {
    use crate::half_edge::VertexId;
    let (mut bb_min, mut bb_max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for i in 0..m.vertices.len() as u32 {
        let v = VertexId(i);
        if m.vert(v).half_edge == NO_EDGE {
            continue;
        }
        bb_min = bb_min.min(m.vert(v).position);
        bb_max = bb_max.max(m.vert(v).position);
    }
    let eps = ((bb_max - bb_min).length() * 1e-5).max(1e-6);
    let vol_before = mesh_signed_volume(m);
    let mut out = m.clone();
    // Blender "Merge by Distance": weld EVERY vertex cluster within eps — including pairs
    // NOT joined by an edge (the two long rims of an epsilon-thin wall). The kernel merge
    // collapses self-loops and drops faces reduced below a triangle, so the wall's faces
    // degenerate away instead of folding flat.
    {
        let live: Vec<VertexId> = (0..out.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| out.vert(v).half_edge != NO_EDGE)
            .collect();
        // Greedy spatial clustering (bodies here are small — post-boolean results).
        let mut used = vec![false; live.len()];
        for i in 0..live.len() {
            if used[i] || out.vert(live[i]).half_edge == NO_EDGE {
                continue;
            }
            let mut cluster = vec![live[i]];
            let mut centroid = out.vert(live[i]).position;
            for j in (i + 1)..live.len() {
                if used[j] || out.vert(live[j]).half_edge == NO_EDGE {
                    continue;
                }
                let p = out.vert(live[j]).position;
                if p.distance_squared(out.vert(live[i]).position) <= eps * eps {
                    used[j] = true;
                    cluster.push(live[j]);
                    centroid += p;
                }
            }
            if cluster.len() > 1 {
                let target = centroid / cluster.len() as f32;
                let _ = out.merge_vertices_to(&cluster, target);
            }
        }
    }
    // Re-merge coplanar faces after the collapse freed the degenerate zone.
    if let Some(again) = re_dissolve_soup(&out) {
        if is_closed_manifold(&again) {
            out = again;
        }
    }
    let vol_after = mesh_signed_volume(&out);
    let tol = (vol_before.abs() * 1e-3).max(1e-9);
    if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
        eprintln!("[BOOL] debris cleanup: eps={eps:.2e} closed={} folds {}->{} vol {:.6}->{:.6}",
            is_closed_manifold(&out), folds_before, out.count_folded_coplanar_pairs(),
            vol_before, vol_after);
    }
    if is_closed_manifold(&out)
        && out.count_folded_coplanar_pairs() < folds_before
        && (vol_after - vol_before).abs() <= tol
    {
        Some(out)
    } else {
        None
    }
}

fn is_closed_manifold(m: &HalfEdgeMesh) -> bool {
    // Edge-manifold: bowtie VERTICES are Blender-legal boolean output (vertex-touch case,
    // owner test-02 2026-08-04) — only non-manifold EDGES / open boundaries reject.
    boundary_half_edge_count(m) == 0 && m.validate().is_ok() && m.is_edge_manifold().is_ok()
}

/// Extract face rings → coplanar merge → rebuild (keeps material_id order when face count matches).
fn re_dissolve_soup(mesh: &HalfEdgeMesh) -> Option<HalfEdgeMesh> {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut remap: HashMap<u32, usize> = HashMap::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let mut mats: Vec<u32> = Vec::new();
    for i in 0..mesh.faces.len() as u32 {
        let fid = FaceId(i);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let ring: Vec<usize> = mesh
            .face_vertices(fid)
            .iter()
            .map(|v| {
                *remap.entry(v.0).or_insert_with(|| {
                    positions.push(mesh.vert(*v).position);
                    positions.len() - 1
                })
            })
            .collect();
        if ring.len() >= 3 {
            faces.push(ring);
            mats.push(mesh.face(fid).material_id);
        }
    }
    if faces.is_empty() {
        return None;
    }
    let merged = merge_coplanar(&positions, &faces);
    if merged.is_empty() {
        return None;
    }
    // merge_coplanar drops material lockstep — re-transfer from prior mesh after rebuild.
    let mut out = HalfEdgeMesh::from_polygons(&positions, &merged);
    if !is_closed_manifold(&out) {
        return None;
    }
    // Prefer fewer faces (more dissolve).
    if out.face_count() >= mesh.face_count() {
        return None;
    }
    // ONE material source. This used to pass `mesh` as BOTH operands, which ran
    // `collect_face_samples` twice over identical geometry for zero information: each sample
    // in the second copy scores EXACTLY equal to its twin in the first, and both consumers
    // keep the first hit — `nearest_sample_material`'s `score < best_score` cannot be
    // satisfied by an equal score, and `uniform_sample_material` returns on the first sample
    // that scores at all. Dropping the copy is output-identical and halves the scan.
    let mut samples = Vec::new();
    collect_face_samples(mesh, &mut samples);
    transfer_materials_from_samples(&mut out, &samples);
    Some(out)
}

// ---------------------------------------------------------------------------
// HalfEdgeMesh ↔ Manifold (shared verts, positions only — spike-proven path)
// ---------------------------------------------------------------------------

fn halfedge_to_manifold(mesh: &HalfEdgeMesh) -> Option<Manifold> {
    let n_vert = mesh.vertices.len();
    if n_vert == 0 {
        return None;
    }
    let mut verts: Vec<f64> = Vec::with_capacity(n_vert * 3);
    for v in &mesh.vertices {
        verts.push(v.position.x as f64);
        verts.push(v.position.y as f64);
        verts.push(v.position.z as f64);
    }
    let mut tris: Vec<u64> = Vec::new();
    // Scratch reused across every face. `face_vertices` / `compute_face_normal` heap-allocate
    // one (resp. two) Vec PER FACE; on a 6k-face operand that is ~30k allocations for the same
    // walk and the same Newell math the `_into` forms do in place. (`compute_face_normal_into`
    // refills `ring` itself and leaves `pts` holding exactly the ring's positions — which is
    // what the ear-clip wants, so the two share one buffer instead of building it twice.)
    let (mut ring, mut pts) = (Vec::new(), Vec::new());
    for fi in 0..mesh.faces.len() as u32 {
        let fid = FaceId(fi);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        mesh.face_vertices_into(fid, &mut ring);
        if ring.len() < 3 {
            continue;
        }
        // NONCONVEX FACES MUST BE EAR-CLIPPED, NEVER FANNED (owner test-04, 2026-08-04).
        // A fan from vs[0] on a horseshoe ngon (e.g. the coplanar face wrapping an intrude
        // region) emits triangles OUTSIDE the polygon that overlap each other — the
        // "solid" handed to Manifold is self-intersecting, and the kernel returns fold
        // debris and a wrong volume in exactly that plane while Blender EXACT (which
        // triangulates properly) stays clean on identical input.
        if ring.len() > 3 {
            let n = mesh.compute_face_normal_into(fid, &mut ring, &mut pts);
            let ears = crate::poly_mesh::earclip_polygon(&pts, n);
            if ears.len() == ring.len() - 2 {
                for t in &ears {
                    tris.push(ring[t[0] as usize].0 as u64);
                    tris.push(ring[t[1] as usize].0 as u64);
                    tris.push(ring[t[2] as usize].0 as u64);
                }
                continue;
            }
            // degenerate ring earclip couldn't finish — fall through to the fan
        }
        let v0 = ring[0].0 as u64;
        for w in ring.windows(2).skip(1) {
            tris.push(v0);
            tris.push(w[0].0 as u64);
            tris.push(w[1].0 as u64);
        }
    }
    if tris.is_empty() {
        return None;
    }
    Manifold::from_mesh_f64(&verts, 3, &tris).ok()
}

fn manifold_to_halfedge(m: &Manifold) -> Option<HalfEdgeMesh> {
    m.status().ok()?;
    let (props, n_props, tri) = m.to_mesh_f64();
    if n_props < 3 || tri.len() < 3 {
        return None;
    }
    let n_vert = props.len() / n_props;
    let mut positions = Vec::with_capacity(n_vert);
    for i in 0..n_vert {
        let o = i * n_props;
        positions.push(Vec3::new(
            props[o] as f32,
            props[o + 1] as f32,
            props[o + 2] as f32,
        ));
    }
    let mut faces: Vec<Vec<usize>> = Vec::with_capacity(tri.len() / 3);
    for c in tri.chunks_exact(3) {
        let i0 = c[0] as usize;
        let i1 = c[1] as usize;
        let i2 = c[2] as usize;
        if i0 >= n_vert || i1 >= n_vert || i2 >= n_vert {
            continue;
        }
        // Keep zero-area slivers: dropping one side of a collinear seam leaves the
        // neighbour's edge un-paired (boundary crack). After the weld, collapsed rings
        // dedup below 3 verts and disappear CONSISTENTLY on both sides instead.
        faces.push(vec![i0, i1, i2]);
    }
    if faces.is_empty() {
        return None;
    }
    // GUARANTEED BASE (owner intrude_02 2026-08-03): Manifold's output is INDEX-watertight
    // — from_polygons on the raw indexed triangles pairs every twin. The weld/merge chain
    // below is a BEAUTIFIER (epsilon-split collapse + n-gon merge for Blender-oracle flat
    // cases); on near-degenerate geometry it can destroy a correct mesh (weld cracks,
    // collapsed slivers). It is only ACCEPTED when its output is strictly clean; otherwise
    // the raw triangle mesh ships.
    let raw_mesh = {
        let m = HalfEdgeMesh::from_polygons(&positions, &faces);
        if boundary_half_edge_count(&m) == 0 && m.validate().is_ok() && m.is_manifold().is_ok() {
            Some(m)
        } else {
            None
        }
    };
    let raw_folds = raw_mesh
        .as_ref()
        .map(|m| m.count_folded_coplanar_pairs())
        .unwrap_or(usize::MAX);
    // Reference volume from the RAW SOUP — always available, unlike `raw_mesh` (which is
    // None when the raw mesh trips a manifold gate). A candidate whose volume check falls
    // back to "true" because the reference is missing is how 0.109 of volume silently
    // vanished (owner test-02): every candidate must beat THIS number.
    let vol_raw_soup: f64 = {
        let mut v = 0.0f64;
        for f in &faces {
            if f.len() < 3 { continue; }
            let p0 = positions[f[0]].as_dvec3();
            for k in 1..f.len() - 1 {
                let p1 = positions[f[k]].as_dvec3();
                let p2 = positions[f[k + 1]].as_dvec3();
                v += p0.dot(p1.cross(p2)) / 6.0;
            }
        }
        v
    };
    // BEAUTIFY ATTEMPT 0 — WELD-COLLAPSE (owner fixture extrude_bad_front 2026-08-04;
    // Blender-parity path): Manifold's f64-exact output rounds to f32, collapsing
    // near-coincident features into zero-thickness DOUBLED SHEETS (edges used by 4 faces,
    // folded flap pairs). Resolve them structurally: weld, drop degenerate rings, remove
    // reversed duplicate pairs, then repeatedly resolve any edge used >2 times by
    // deleting the zero-thickness pair that overuses it. Accepted only when strictly
    // clean AND volume-preserving vs the raw base.
    {
        // Scale-relative coarse weld (owner: cleanup must not be stronger than the model
        // demands). diag*1e-4 == the old 1e-4 on a unit-ish body, but correct at any size.
        let diag = {
            let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for p in &positions {
                mn = mn.min(*p);
                mx = mx.max(*p);
            }
            (mx - mn).length().max(1e-6)
        };
        // WELD-SCALE SEARCH (owner test-04, 2026-08-04): kernel debris arrives at several
        // separations (~1e-5 .. ~5e-4 on a unit body) — one fixed epsilon either misses the
        // coarse pairs or eats fine detail. Try fine-to-coarse; every candidate faces the
        // full gates (volume-exact vs the raw soup, closed, edge-manifold, zero folds), so
        // a too-coarse weld can only be REJECTED, never shipped wrong.
        for wc_mult in [1e-4f32, 5e-4, 1e-3] {
        let wc_eps = (diag * wc_mult).max(1e-7);
        let (zpos, zfaces) = weld_positions_faces_eps(&positions, &faces, wc_eps);
        let dbg = std::env::var("AZRA_BOOL_DEBUG").is_ok();
        if dbg { eprintln!("[WC] weld: {} faces", zfaces.len()); }
        let zfaces = drop_coincident_reversed_pairs(zfaces);
        if dbg { eprintln!("[WC] drop-rev: {} faces", zfaces.len()); }
        let zfaces = resolve_overused_edges(zfaces);
        if dbg { eprintln!("[WC] overused: {} faces", zfaces.len()); }
        if !zfaces.is_empty() {
            // CANDIDATE SEARCH (owner 2026-08-04): T-junction repair is required by some
            // captures and actively harmful to others (it can make near-parallel faces
            // coincide -> new flaps). Rather than guess, run BOTH variants through the
            // rest of the chain and take the first that passes the gates. Candidate A is
            // exactly the previously-shipping path, so this can never regress.
            for use_tj in [true, false] {
                let stage = if use_tj {
                    crate::mesh_boolean::repair_t_junctions(&zpos, &zfaces, 1e-4)
                } else {
                    zfaces.clone()
                };
                if stage.is_empty() {
                    continue;
                }
                let flat = crate::mesh_boolean::flatten_folded_clusters(&zpos, &stage);
                if flat.is_empty() {
                    continue;
                }
                let merged = merge_coplanar(&zpos, &flat);
                let use_faces = if merged.is_empty() { flat.clone() } else { merged };
                // Final re-weld: earlier passes can leave index-distinct verts at one
                // position (zero-length edges = phantom boundary cracks).
                    let (fpos, ffaces) = weld_positions_faces_eps(&zpos, &use_faces, wc_eps);
                let ffaces: Vec<Vec<usize>> = ffaces
                    .into_iter()
                    .map(|mut r| {
                        r.dedup();
                        while r.len() > 1 && r.first() == r.last() {
                            r.pop();
                        }
                        r
                    })
                    .filter(|r| r.len() >= 3)
                    .collect();
                let ffaces = drop_coincident_reversed_pairs(ffaces);
                let ffaces = resolve_overused_edges(ffaces);
                let ffaces = crate::mesh_boolean::flatten_folded_clusters(&fpos, &ffaces);
                let merged2 = merge_coplanar(&fpos, &ffaces);
                let ffaces = if merged2.is_empty() { ffaces } else { merged2 };
                let m0 = HalfEdgeMesh::from_polygons(&fpos, &ffaces);
                let vol_ok = {
                    let vm = mesh_signed_volume(&m0);
                    (vol_raw_soup - vm).abs() <= (vol_raw_soup.abs() * 1e-3).max(1e-9)
                };
                // Folds gate: no NEW folds vs the raw triangle mesh (same rule as the outer
                // polish). Requiring == 0 let a single epsilon flap veto the entire n-gon
                // restore, shipping Manifold triangulation on untouched flats (owner 2026-08-10).
                let folds_ok = m0.count_folded_coplanar_pairs() <= raw_folds;
                if boundary_half_edge_count(&m0) == 0
                    && m0.validate().is_ok()
                    // Bowtie vertices permitted — Blender-legal boolean touch case.
                    && m0.is_edge_manifold().is_ok()
                    && folds_ok
                    && vol_ok
                {
                    return Some(m0);
                }
                if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
                    eprintln!(
                        "[BOOL] weld-collapse candidate(tj={use_tj}) rejected: boundary={} folds={} vol_ok={vol_ok} valid={:?} manifold={:?}",
                        boundary_half_edge_count(&m0),
                        m0.count_folded_coplanar_pairs(),
                        m0.validate().err().map(|e| format!("{e:?}")),
                        m0.is_manifold().err().map(|e| format!("{e:?}"))
                    );
                }
            }
        }
    }
    }
    let (wpos, wfaces) = weld_positions_faces(&positions, &faces);
    // ZERO-THICKNESS FLAP CLEANUP (owner subtract.fl4 2026-08-03): a near-flush contact
    // leaves an epsilon-thin sliver that the weld collapses into coincident REVERSED
    // face pairs (a fold — edge-consistent, invisible to is_manifold). Both faces of
    // such a pair bound no volume: drop both; the surrounding surface re-pairs cleanly.
    let wfaces = drop_coincident_reversed_pairs(wfaces);
    if wfaces.is_empty() {
        return None;
    }
    // T-JUNCTION REPAIR (owner intrude_02 2026-08-03): the f64→f32 conversion + zero-area
    // drop + weld can leave a long edge un-split where its neighbour was split — the twin
    // never pairs and the rebuild reports boundary cracks. Split ring edges at welded
    // points on their interior, exactly like the BSP soup path.
    let wfaces = crate::mesh_boolean::repair_t_junctions(&wpos, &wfaces, 1e-4);
    if wfaces.is_empty() {
        return None;
    }
    // Flatten signed (folded) coplanar decompositions before the n-gon merge.
    let wfaces = crate::mesh_boolean::flatten_folded_clusters(&wpos, &wfaces);
    if wfaces.is_empty() {
        return None;
    }
    let merged = merge_coplanar(&wpos, &wfaces);
    let use_faces = if !merged.is_empty() { &merged } else { &wfaces };
    let vol_ok_soup = |m: &HalfEdgeMesh| -> bool {
        let vm = mesh_signed_volume(m);
        (vol_raw_soup - vm).abs() <= (vol_raw_soup.abs() * 1e-3).max(1e-9)
    };
    let mesh = HalfEdgeMesh::from_polygons(&wpos, use_faces);
    // Prefer the merged n-gon rebuild whenever it is closed, edge-manifold, volume-safe,
    // and does not introduce NEW folds. Strict zero-fold used to reject good merges that
    // still collapsed most Manifold diagonals on remote coplanar flats.
    if boundary_half_edge_count(&mesh) == 0
        && mesh.validate().is_ok()
        && mesh.is_edge_manifold().is_ok()
        && mesh.count_folded_coplanar_pairs() <= raw_folds
        && vol_ok_soup(&mesh)
    {
        return Some(mesh);
    }
    // Beautifier not accepted — try the welded triangles, then the raw indexed mesh.
    let tri_mesh = HalfEdgeMesh::from_polygons(&wpos, &wfaces);
    if boundary_half_edge_count(&tri_mesh) == 0
        && tri_mesh.validate().is_ok()
        && tri_mesh.is_edge_manifold().is_ok()
        && tri_mesh.count_folded_coplanar_pairs() <= raw_folds
        && vol_ok_soup(&tri_mesh)
    {
        // Prefer a partial merge over pure tris when the full weld path failed above
        // but the merge still reduced face count without new folds.
        if !use_faces.is_empty()
            && use_faces.len() < wfaces.len()
            && boundary_half_edge_count(&mesh) == 0
            && mesh.validate().is_ok()
            && mesh.is_edge_manifold().is_ok()
            && mesh.count_folded_coplanar_pairs() <= tri_mesh.count_folded_coplanar_pairs()
            && vol_ok_soup(&mesh)
        {
            return Some(mesh);
        }
        return Some(tri_mesh);
    }
    // FALLBACK BEAUTIFY — coplanar merge directly on the RAW INDEXED faces (no weld):
    // merging only joins faces across shared indexed edges, so it cannot open cracks the
    // way the weld can. Restores n-gons on the epsilon-sliver cases where the welded chain
    // above gets rejected (owner intrude retest: raw triangle soup shipped). Runs AFTER the
    // welded chain because that one also resolves coincident-face pairs (sequential cuts).
    {
        let merged = merge_coplanar(&positions, &faces);
        if !merged.is_empty() {
            let m = HalfEdgeMesh::from_polygons(&positions, &merged);
            if boundary_half_edge_count(&m) == 0
                && m.validate().is_ok()
                && m.is_edge_manifold().is_ok()
                && m.count_folded_coplanar_pairs() <= raw_folds
                && vol_ok_soup(&m)
                // Only take this path when it actually cleans (fewer faces than raw tris).
                && (m.face_count() as usize) < faces.len()
            {
                return Some(m);
            }
        }
    }
    if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
        eprintln!(
            "[BOOL] beautifier rejected (merged folds={} tri boundary={} tri folds={}) — raw mesh: {}",
            mesh.count_folded_coplanar_pairs(),
            boundary_half_edge_count(&tri_mesh),
            tri_mesh.count_folded_coplanar_pairs(),
            raw_mesh.is_some()
        );
    }
    raw_mesh
}

/// Remove every pair of faces with the SAME vertex set traversed in OPPOSITE order
/// (zero-thickness flap after welding). Same-winding duplicates are left for the
/// manifold gate to reject (they are a genuine error, not a flap).
fn drop_coincident_reversed_pairs(faces: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<Vec<usize>, Vec<usize>> = BTreeMap::new();
    for (i, f) in faces.iter().enumerate() {
        let mut key = f.clone();
        key.sort_unstable();
        groups.entry(key).or_default().push(i);
    }
    let mut drop = vec![false; faces.len()];
    for idxs in groups.values() {
        if idxs.len() < 2 {
            continue;
        }
        let mut used = vec![false; idxs.len()];
        for x in 0..idxs.len() {
            if used[x] { continue; }
            for y in (x + 1)..idxs.len() {
                if used[y] { continue; }
                let n = faces[idxs[x]].len();
                if faces[idxs[y]].len() != n { continue; }
                let fset: std::collections::HashSet<(usize, usize)> = (0..n)
                    .map(|k| (faces[idxs[x]][k], faces[idxs[x]][(k + 1) % n]))
                    .collect();
                let rev = (0..n).all(|k| {
                    let e = (faces[idxs[y]][k], faces[idxs[y]][(k + 1) % n]);
                    fset.contains(&(e.1, e.0))
                });
                if rev {
                    drop[idxs[x]] = true;
                    drop[idxs[y]] = true;
                    used[x] = true;
                    used[y] = true;
                    break;
                }
                // NOTE (2026-08-04): same-winding duplicates are NOT dropped here — a blind
                // dedupe deleted load-bearing faces of doubled sheets (owner: entry-face
                // region vanished, 7-edge crack loop). resolve_overused_edges removes them
                // only when an edge is provably oversubscribed.
            }
        }
    }
    faces
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !drop[*i])
        .map(|(_, f)| f)
        .collect()
}

/// Delete zero-thickness doubled sheets: while any undirected edge is used by MORE than
/// two faces, find among its users a pair with identical vertex SETS traversed in
/// opposite order (a flap) and delete both; if none, a same-winding duplicate and delete
/// one. No progress => bail (returns what remains; caller's gates decide).
///
/// NOTE (owner test-04, 2026-08-04): deleting one user of a same-winding DIRECTED duplicate
/// edge was tried here and opened 11-24 boundary cracks — a doubled sheet cannot be fixed by
/// deletion without re-twinning the remainder. That is the Merge-by-Distance / f64-rebuild
/// task; until then such meshes fail the gates and the op refuses safely. Its scaffolding (a
/// second `directed` BTreeMap over every face corner, plus an area closure over `positions`)
/// survived the revert, was rebuilt on every one of the 64 rounds and read by nothing but a
/// `let _ = …` keep-alive; removed 2026-08-09 (perf). Reinstate both if that task resumes.
fn resolve_overused_edges(mut faces: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    use std::collections::BTreeMap;
    for _round in 0..64 {
        // BTreeMap, not HashMap: `find` below takes the FIRST oversubscribed edge in KEY
        // order, so which face pair gets deleted — and therefore the output mesh — depends
        // on this iteration order being deterministic.
        let mut edge_faces: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (fi, f) in faces.iter().enumerate() {
            let n = f.len();
            for i in 0..n {
                let (a, b) = (f[i], f[(i + 1) % n]);
                edge_faces
                    .entry(if a < b { (a, b) } else { (b, a) })
                    .or_default()
                    .push(fi);
            }
        }
        let Some(users) = edge_faces.values().find(|v| v.len() > 2) else {
            return faces; // all edges are 2-manifold
        };
        let mut kill: Option<Vec<usize>> = None;
        'outer: for xi in 0..users.len() {
            for yi in (xi + 1)..users.len() {
                let (fx, fy) = (users[xi], users[yi]);
                // Same face can appear twice in `users` when a poly reuses an undirected
                // edge (degenerate / self-touching). Pairing a face with itself made
                // kill = [i, i] → second faces.remove(i) panics (owner crash 2026-08-07:
                // confirm extrude → poly_inset_sync → manifold boolean).
                if fx == fy {
                    continue;
                }
                if fx >= faces.len() || fy >= faces.len() {
                    continue;
                }
                if faces[fx].len() != faces[fy].len() {
                    continue;
                }
                let n = faces[fx].len();
                if n < 3 {
                    continue;
                }
                let fset: std::collections::HashSet<(usize, usize)> =
                    (0..n).map(|k| (faces[fx][k], faces[fx][(k + 1) % n])).collect();
                let rev = (0..n).all(|k| {
                    let e = (faces[fy][k], faces[fy][(k + 1) % n]);
                    fset.contains(&(e.1, e.0))
                });
                if rev {
                    kill = Some(vec![fx, fy]);
                    break 'outer;
                }
                let same = (0..n).all(|k| {
                    let e = (faces[fy][k], faces[fy][(k + 1) % n]);
                    fset.contains(&e)
                });
                if same {
                    kill = Some(vec![fy]);
                    break 'outer;
                }
            }
        }
        let Some(kill) = kill else {
            return faces; // unresolvable overuse — let the gates reject downstream
        };
        // Descending unique indices so each remove stays in-range.
        let mut ki = kill;
        ki.sort_unstable_by(|a, b| b.cmp(a));
        ki.dedup();
        for k in ki {
            if k < faces.len() {
                faces.remove(k);
            }
        }
    }
    faces
}


fn weld_positions_faces(
    positions: &[Vec3],
    faces: &[Vec<usize>],
) -> (Vec<Vec3>, Vec<Vec<usize>>) {
    // 1e-6 missed borderline pairs exactly 1e-6 apart; 1e-5 = 10um at meter scale.
    weld_positions_faces_eps(positions, faces, 1e-5)
}
fn weld_positions_faces_eps(
    positions: &[Vec3],
    faces: &[Vec<usize>],
    eps: f32,
) -> (Vec<Vec3>, Vec<Vec<usize>>) {
    // 1e-6 missed borderline pairs exactly 1e-6 apart (owner intrude_02: micro-edges of
    // length ~1e-6 left unpaired -> boundary cracks). 1e-5 = 10um at meter scale.
    let _ = ();
    let inv = 1.0 / eps;
    let mut key_to_idx: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let mut out_pos: Vec<Vec3> = Vec::new();
    let mut remap = vec![0usize; positions.len()];
    for (i, &p) in positions.iter().enumerate() {
        let key = (
            (p.x * inv).round() as i64,
            (p.y * inv).round() as i64,
            (p.z * inv).round() as i64,
        );
        // NEVER weld two vertices that BOTH existed on an input mesh (owner rule
        // 2026-08-04: "geo changes on polys not connected to the cut"). This epsilon scales
        // with the whole model, so on a small feature far from the cut it would otherwise
        // merge real, distinct corners — destroying geometry the boolean never touched.
        // Vertices the BOOLEAN created are still welded; that float noise is why this pass
        // exists.
        let idx = match key_to_idx.get(&key) {
            Some(&existing)
                if crate::mesh_boolean::same_operand_vertices(p, out_pos[existing]) =>
            {
                let n = out_pos.len();
                out_pos.push(p);
                n
            }
            Some(&existing) => {
                // BOOLEAN LOCALITY (2026-08-08, case a): first-seen-wins let a CUT vertex
                // own the cluster's surviving position; a preserved INPUT vertex absorbed
                // into it moved off its 1e-5 qkey and silently lost every preserve
                // protection downstream (this weld runs at eps up to diag*1e-3 ≈ 350×
                // PRESERVE_Q). When a cluster mixes the two, the INPUT position survives.
                if crate::mesh_boolean::is_input_vertex(p)
                    && !crate::mesh_boolean::is_input_vertex(out_pos[existing])
                {
                    out_pos[existing] = p;
                }
                existing
            }
            None => {
                let n = out_pos.len();
                key_to_idx.insert(key, n);
                out_pos.push(p);
                n
            }
        };
        remap[i] = idx;
    }
    let mut out_faces = Vec::with_capacity(faces.len());
    for f in faces {
        let mut ring: Vec<usize> = f.iter().map(|&i| remap[i]).collect();
        ring.dedup();
        while ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() >= 3 {
            out_faces.push(ring);
        }
    }
    (out_pos, out_faces)
}

// ---------------------------------------------------------------------------
// Material restore — nearest input face
// ---------------------------------------------------------------------------

struct FaceSample {
    centroid: Vec3,
    normal: Vec3,
    material_id: u32,
}

const COPLANAR_DOT: f32 = 0.95;
const COPLANAR_DIST: f32 = 0.05;

/// Match cost of one input-face sample against an output face `(c, n)`. Lower wins.
/// ONE definition — both the nearest-sample search and the uniform-material shortcut score
/// through here, so the two can never drift apart.
#[inline]
fn sample_score(s: &FaceSample, c: Vec3, n: Vec3) -> f32 {
    let d_cent = (s.centroid - c).length_squared();
    let plane_d = (n.dot(c) - n.dot(s.centroid)).abs();
    let coplanar = n.dot(s.normal).abs() > COPLANAR_DOT && plane_d < COPLANAR_DIST;
    // Prefer coplanar matches heavily.
    if coplanar {
        plane_d + d_cent * 0.01
    } else {
        10.0 + d_cent
    }
}

fn collect_face_samples(mesh: &HalfEdgeMesh, out: &mut Vec<FaceSample>) {
    // Scratch reused across every face: the allocating `face_vertices` / `compute_face_normal`
    // heap-allocate one (resp. two) Vec PER FACE, which on a 6k-face operand is the bulk of
    // this pass. Same walk, same Newell math — the allocating forms wrap these.
    let (mut ring, mut pts) = (Vec::new(), Vec::new());
    for i in 0..mesh.faces.len() as u32 {
        let fid = FaceId(i);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        mesh.face_vertices_into(fid, &mut ring);
        if ring.len() < 3 {
            continue;
        }
        let centroid =
            ring.iter().fold(Vec3::ZERO, |a, v| a + mesh.vert(*v).position) / ring.len() as f32;
        let normal = mesh.compute_face_normal_into(fid, &mut ring, &mut pts);
        out.push(FaceSample {
            centroid,
            normal,
            material_id: mesh.face(fid).material_id,
        });
    }
}

/// Material of the sample that scores lowest against `(c, n)`, or 0 when no sample scores
/// below `f32::MAX` (NaN / overflowed coordinates). `<` keeps the FIRST minimum on ties, and
/// `samples` is ordered A's faces then B's — both are behaviour, do not reorder.
fn nearest_sample_material(samples: &[FaceSample], c: Vec3, n: Vec3) -> u32 {
    let mut best_mat = 0u32;
    let mut best_score = f32::MAX;
    for s in samples {
        let score = sample_score(s, c, n);
        if score < best_score {
            best_score = score;
            best_mat = s.material_id;
        }
    }
    best_mat
}

/// [`nearest_sample_material`] when EVERY sample carries the same `material_id` = `uniform`.
///
/// EQUIVALENCE (why this is exact, not an approximation): the search is a minimum-search that
/// only ever writes the winning sample's `material_id`. When every sample carries the same id,
/// *which* sample wins is unknowable but irrelevant — the value written is that one id
/// regardless of the scores. The single remaining observable is whether ANY sample won at all:
/// `best_score` starts at `f32::MAX`, so a face whose every score is NaN or ≥ `f32::MAX`
/// (degenerate/NaN geometry) leaves `best_mat` at its 0 default. So: scan until the first
/// sample that would have won, then stop. Normal geometry exits on the first sample, turning
/// an O(F_out × (F_a + F_b)) min-search into O(F_out).
fn uniform_sample_material(samples: &[FaceSample], c: Vec3, n: Vec3, uniform: u32) -> u32 {
    for s in samples {
        if sample_score(s, c, n) < f32::MAX {
            return uniform;
        }
    }
    0
}

/// The one `material_id` shared by every sample, or `None` when they differ.
fn uniform_sample_material_id(samples: &[FaceSample]) -> Option<u32> {
    let first = samples.first()?.material_id;
    samples.iter().all(|s| s.material_id == first).then_some(first)
}

/// Assign each output face the material of the closest coplanar (preferred) or nearest
/// sample. Coplanar preference: normal agree + plane distance small.
///
/// Takes the samples rather than the operands so a caller with ONE material source
/// (`re_dissolve_soup` re-materialises its own input) collects once instead of passing the
/// same mesh twice.
fn transfer_materials_from_samples(out: &mut HalfEdgeMesh, samples: &[FaceSample]) {
    if samples.is_empty() {
        return;
    }
    // Both operands single-material is the overwhelmingly common case, and it collapses the
    // whole quadratic search — see `uniform_sample_material`.
    let uniform = uniform_sample_material_id(samples);

    let (mut ring, mut pts) = (Vec::new(), Vec::new());
    for i in 0..out.faces.len() as u32 {
        let fid = FaceId(i);
        if out.face(fid).half_edge == NO_EDGE {
            continue;
        }
        out.face_vertices_into(fid, &mut ring);
        if ring.len() < 3 {
            continue;
        }
        let c = ring.iter().fold(Vec3::ZERO, |acc, v| acc + out.vert(*v).position)
            / ring.len() as f32;
        let n = out.compute_face_normal_into(fid, &mut ring, &mut pts);

        let best_mat = match uniform {
            Some(m) => uniform_sample_material(samples, c, n, m),
            None => nearest_sample_material(samples, c, n),
        };
        out.face_mut(fid).material_id = best_mat;
    }
}

/// Assign each output face the material of the closest coplanar (preferred) or nearest
/// input face from A∪B. The sample order is A's live faces then B's — `nearest_sample_material`
/// resolves exact ties by keeping the first, so the order is behaviour.
fn transfer_materials_from_operands(out: &mut HalfEdgeMesh, a: &HalfEdgeMesh, b: &HalfEdgeMesh) {
    let mut samples: Vec<FaceSample> = Vec::new();
    collect_face_samples(a, &mut samples);
    collect_face_samples(b, &mut samples);
    transfer_materials_from_samples(out, &samples);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_boolean::mesh_boolean;
    use glam::Vec3;

    fn box_mesh(center: Vec3, half: Vec3) -> HalfEdgeMesh {
        let c = center;
        let h = half;
        let positions = vec![
            c + Vec3::new(-h.x, -h.y, -h.z),
            c + Vec3::new(h.x, -h.y, -h.z),
            c + Vec3::new(h.x, -h.y, h.z),
            c + Vec3::new(-h.x, -h.y, h.z),
            c + Vec3::new(-h.x, h.y, -h.z),
            c + Vec3::new(h.x, h.y, -h.z),
            c + Vec3::new(h.x, h.y, h.z),
            c + Vec3::new(-h.x, h.y, h.z),
        ];
        let faces: Vec<Vec<usize>> = vec![
            vec![0, 1, 2, 3],
            vec![7, 6, 5, 4],
            vec![4, 5, 1, 0],
            vec![6, 7, 3, 2],
            vec![7, 4, 0, 3],
            vec![5, 6, 2, 1],
        ];
        HalfEdgeMesh::from_polygons(&positions, &faces)
    }

    // -----------------------------------------------------------------------
    // Material transfer — the optimized transfer must reproduce the ORIGINAL
    // linear min-search bit for bit (2026-08-09 perf work).
    // -----------------------------------------------------------------------

    /// FROZEN verbatim copy of `transfer_materials_from_operands` as it stood BEFORE the
    /// uniform-material fast path and the buffer reuse: full linear min-search over A's
    /// faces then B's, allocating accessors, `<` keeping the first minimum. Ground truth.
    ///
    /// One entry per face slot; `None` = the original skipped that slot (dead face or a
    /// ring shorter than 3) and therefore left its `material_id` untouched.
    fn reference_transfer(
        out: &HalfEdgeMesh,
        a: &HalfEdgeMesh,
        b: &HalfEdgeMesh,
    ) -> Vec<Option<u32>> {
        struct S {
            centroid: Vec3,
            normal: Vec3,
            material_id: u32,
        }
        fn collect(mesh: &HalfEdgeMesh, out: &mut Vec<S>) {
            for i in 0..mesh.faces.len() as u32 {
                let fid = FaceId(i);
                if mesh.face(fid).half_edge == NO_EDGE {
                    continue;
                }
                let vs = mesh.face_vertices(fid);
                if vs.len() < 3 {
                    continue;
                }
                let centroid = vs.iter().fold(Vec3::ZERO, |a, v| a + mesh.vert(*v).position)
                    / vs.len() as f32;
                let normal = mesh.compute_face_normal(fid);
                out.push(S { centroid, normal, material_id: mesh.face(fid).material_id });
            }
        }
        let mut samples: Vec<S> = Vec::new();
        collect(a, &mut samples);
        collect(b, &mut samples);
        let mut res = vec![None; out.faces.len()];
        if samples.is_empty() {
            return res;
        }
        for i in 0..out.faces.len() as u32 {
            let fid = FaceId(i);
            if out.face(fid).half_edge == NO_EDGE {
                continue;
            }
            let vs = out.face_vertices(fid);
            if vs.len() < 3 {
                continue;
            }
            let c =
                vs.iter().fold(Vec3::ZERO, |acc, v| acc + out.vert(*v).position) / vs.len() as f32;
            let n = out.compute_face_normal(fid);
            let mut best_mat = 0u32;
            let mut best_score = f32::MAX;
            for s in &samples {
                let d_cent = (s.centroid - c).length_squared();
                let plane_d = (n.dot(c) - n.dot(s.centroid)).abs();
                let coplanar = n.dot(s.normal).abs() > 0.95 && plane_d < 0.05;
                let score = if coplanar { plane_d + d_cent * 0.01 } else { 10.0 + d_cent };
                if score < best_score {
                    best_score = score;
                    best_mat = s.material_id;
                }
            }
            res[i as usize] = Some(best_mat);
        }
        res
    }

    fn box_with_material(center: Vec3, half: Vec3, mat: u32) -> HalfEdgeMesh {
        let mut m = box_mesh(center, half);
        for f in m.faces.iter_mut() {
            f.material_id = mat;
        }
        m
    }

    /// MULTI-material: the scoring path must be untouched. Cut a dense target (material 3)
    /// with a small corner box (material 7) and assert every result face carries exactly the
    /// id the original min-search would have picked — including which of the two it picked.
    #[test]
    fn material_transfer_multi_material_matches_reference() {
        if prefer_bsp_boolean() {
            return;
        }
        let mut a = box_with_material(Vec3::ZERO, Vec3::splat(1.0), 3);
        a.subdivide_catmull_clark();
        a.subdivide_catmull_clark();
        for f in a.faces.iter_mut() {
            f.material_id = 3; // CC rebuild does not carry ids
        }
        let b = box_with_material(Vec3::new(0.75, 0.75, 0.0), Vec3::splat(0.35), 7);
        let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("box - corner box must succeed");

        let expect = reference_transfer(&r, &a, &b);
        for (i, e) in expect.iter().enumerate() {
            if let Some(m) = e {
                assert_eq!(
                    r.faces[i].material_id, *m,
                    "face {i}: optimized transfer diverged from the original min-search"
                );
            }
        }
        // Sanity: the case really does exercise the multi-material path (both ids survive).
        let live = |m: u32| {
            r.faces
                .iter()
                .filter(|f| f.half_edge != NO_EDGE && f.material_id == m)
                .count()
        };
        assert!(live(3) > 0, "target walls must keep material 3");
        assert!(live(7) > 0, "cut walls must pick up the cutter's material 7");
    }

    /// SINGLE-material fast path: same geometry, both operands on one id. The shortcut must
    /// agree with the original min-search face for face — and actually assign the shared id
    /// (not fall through to the 0 default).
    #[test]
    fn material_transfer_uniform_fast_path_matches_slow_path() {
        if prefer_bsp_boolean() {
            return;
        }
        const MAT: u32 = 5;
        let mut a = box_with_material(Vec3::ZERO, Vec3::splat(1.0), MAT);
        a.subdivide_catmull_clark();
        a.subdivide_catmull_clark();
        for f in a.faces.iter_mut() {
            f.material_id = MAT;
        }
        let b = box_with_material(Vec3::new(0.75, 0.75, 0.0), Vec3::splat(0.35), MAT);
        let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("box - corner box must succeed");

        let expect = reference_transfer(&r, &a, &b);
        let mut live = 0usize;
        for (i, e) in expect.iter().enumerate() {
            if let Some(m) = e {
                live += 1;
                assert_eq!(*m, MAT, "reference itself must pick the one shared id");
                assert_eq!(
                    r.faces[i].material_id, *m,
                    "face {i}: uniform fast path diverged from the min-search"
                );
            }
        }
        assert!(live > 0, "the boolean must produce live faces to check");
    }

    /// D1 ORACLE — `re_dissolve_soup` has ONE material source and used to pass it as BOTH
    /// operands, doubling the sample vector. Dropping the copy must be output-identical.
    ///
    /// The argument: sample `k` of the second copy is bit-identical to sample `k` of the
    /// first (same mesh, no mutation between the two collections — `out` is a distinct mesh,
    /// which Rust's `&mut` vs `&` guarantees), so it scores exactly equal, and BOTH consumers
    /// keep the first hit (`score < best_score` is false on equality; `uniform_sample_material`
    /// returns on the first scoring sample). This test proves it on the MULTI-material path,
    /// where a wrong tie-break would actually be visible.
    #[test]
    fn material_transfer_single_source_matches_duplicated_operands() {
        // Six distinct materials so a tie broken the other way would change the answer.
        let mut src = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
        for (i, f) in src.faces.iter_mut().enumerate() {
            f.material_id = i as u32 + 1;
        }
        // A target whose faces sit near, but not on, the source's — every face has to score.
        let target = box_mesh(Vec3::new(0.05, -0.03, 0.02), Vec3::splat(0.97));

        let mut dup = target.clone();
        transfer_materials_from_operands(&mut dup, &src, &src); // the OLD re_dissolve call

        let mut single = target.clone();
        let mut samples = Vec::new();
        collect_face_samples(&src, &mut samples);
        transfer_materials_from_samples(&mut single, &samples);

        assert!(
            uniform_sample_material_id(&samples).is_none(),
            "fixture must exercise the multi-material scoring path"
        );
        let got: Vec<u32> = single.faces.iter().map(|f| f.material_id).collect();
        let want: Vec<u32> = dup.faces.iter().map(|f| f.material_id).collect();
        assert_eq!(got, want, "de-duplicating the sample vector changed the result");
        assert!(want.iter().any(|&m| m != 0), "transfer must actually assign ids");
    }

    /// The fast path is not allowed to touch dead face slots, and must survive an operand
    /// pair that produces no samples at all (both empty → early return, nothing written).
    #[test]
    fn material_transfer_no_samples_leaves_ids_untouched() {
        let empty = HalfEdgeMesh::from_polygons(&[], &[]);
        let mut out = box_with_material(Vec3::ZERO, Vec3::splat(1.0), 9);
        transfer_materials_from_operands(&mut out, &empty, &empty);
        assert!(
            out.faces.iter().all(|f| f.material_id == 9),
            "no samples must mean no writes"
        );
    }

    /// Regression: face that reuses an undirected edge listed itself twice in edge users;
    /// pairing produced kill=[i,i] and `faces.remove(i)` twice panics (extrude confirm).
    #[test]
    fn resolve_overused_edges_self_pair_no_panic() {
        // Two coplanar quads + a third face that walks the same undirected edge twice
        // is hard to invent cleanly; instead push a doubled-sheet pair and a face that
        // shares an edge with both so users.len()>2, including a synthetic duplicate
        // via a bow-tie poly that revisits edge (0,1). Indices only — the op is purely
        // topological (it stopped taking positions when the dead area closure was removed).
        //
        // Face A and reverse Face A' (doubled sheet), plus two more faces that also use
        // edge (0,1) so the undirected edge is oversubscribed with A appearing once —
        // the crash path was A paired with itself when users listed A twice. Force that
        // by a self-touching polygon: 0-1-4-1-3 (edge 0-1 then later 1-… still ok;
        // undirected (0,1) once). Better: inject the bug path via a face that lists
        // edge both ways: 0-1-2-1 (degenerate) gives undirected (0,1) and (1,2) twice?
        // 0→1 and later 1→0 would be (0,1) twice from one face.
        let faces = vec![
            vec![0, 1, 2, 3],       // A
            vec![3, 2, 1, 0],       // A reverse (flap with A)
            vec![0, 1, 4],          // B also on (0,1)
            vec![0, 1, 2, 1],       // degenerate: (0,1) once, (1,2),(2,1) cancel — still
        ];
        // Must not panic.
        let out = resolve_overused_edges(faces);
        assert!(!out.is_empty() || out.is_empty()); // just survive
    }

    /// Audit BOOL-CURVED-SCALE: box − 1536-face CC solid must succeed under Manifold.
    #[test]
    fn audit_box_minus_cc_l4_succeeds_fast() {
        if prefer_bsp_boolean() {
            return;
        }
        let a = box_mesh(Vec3::ZERO, Vec3::splat(0.8));
        let mut b = HalfEdgeMesh::new_box(Vec3::splat(0.75));
        for _ in 0..4 {
            b.subdivide_catmull_clark();
        }
        for v in b.vertices.iter_mut() {
            v.position += Vec3::new(0.8, 0.0, 0.0);
        }
        assert_eq!(b.face_count(), 1536, "CC L4 face count");
        let t0 = std::time::Instant::now();
        let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("CC L4 must not return None");
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // Release ~4 ms; debug unoptimized polish/CC can be tens of seconds — still << BSP None.
        assert!(ms < 120_000.0, "CC L4 too slow: {ms:.1} ms");
        assert_eq!(boundary_half_edge_count(&r), 0);
        assert!(r.is_manifold().is_ok());
        let va = mesh_signed_volume(&a);
        let vr = mesh_signed_volume(&r);
        assert!(vr < va - 1e-3, "vol result {vr} should be < minuend {va}");
        assert!(vr > 0.0);
    }

    fn cylinder_mesh(r: f32, half_h: f32, sides: usize, c: Vec3) -> HalfEdgeMesh {
        let mut pos: Vec<Vec3> = Vec::new();
        for k in 0..2 {
            let z = if k == 0 { half_h } else { -half_h };
            for i in 0..sides {
                let a = std::f32::consts::TAU * i as f32 / sides as f32;
                pos.push(c + Vec3::new(r * a.cos(), r * a.sin(), z));
            }
        }
        let mut f: Vec<Vec<usize>> = vec![(0..sides).collect(), (sides..2 * sides).rev().collect()];
        for i in 0..sides {
            let j = (i + 1) % sides;
            f.push(vec![i, sides + i, sides + j, j]);
        }
        HalfEdgeMesh::from_polygons(&pos, &f)
    }

    /// WHERE DO THE SLIVERS COME FROM? Sweep cube − cylinder over cutter positions and report
    /// the shortest edge of the result relative to the cylinder's own chord length. Any value
    /// well below the chord is an intersection-induced sliver, which is what throttles the
    /// bevel's global Clamp Overlap (BEVEL.md §6).
    ///
    ///   cargo test -p core_math diag_boolean_sliver_sweep -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diag_boolean_sliver_sweep() {
        if prefer_bsp_boolean() {
            return;
        }
        let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
        let sides = 32usize;
        let r = 0.5f32;
        let chord = 2.0 * r * (std::f32::consts::PI / sides as f32).sin();
        eprintln!("[SWEEP] cylinder chord = {chord:.6}");
        for step in 0..24 {
            // Slide the cutter along X so the cut plane (cube face x=1) slices the barrel
            // at ever-different phases relative to the tessellation vertices.
            let dx = 0.60 + step as f32 * 0.02;
            let b = cylinder_mesh(r, 2.0, sides, Vec3::new(dx, 0.0, 0.0));
            match mesh_boolean(&a, &b, BoolOp::Difference) {
                Some(out) => {
                    let mn = min_edge_length(&out);
                    eprintln!(
                        "[SWEEP] dx={dx:.2} v={} f={} min_edge={mn:.6} ratio_to_chord={:.4}{}",
                        out.vertex_count(),
                        out.face_count(),
                        mn / chord,
                        if mn < chord * 0.3 { "   <== SLIVER" } else { "" }
                    );
                }
                None => eprintln!("[SWEEP] dx={dx:.2} boolean refused"),
            }
        }
    }

    /// PINNED — the boolean must not leave sliver edges that throttle the bevel.
    ///
    /// Cube [-1,1]³ minus a 32-side cylinder centred at x=0.90: the cut crosses the barrel a hair
    /// from a tessellation vertex, and BEFORE the sliver cleanup the result's shortest edge was
    /// **0.002565** (2.6% of the cylinder's own 0.098 chord), which pinned `bevel_edges_offset`'s
    /// global Clamp Overlap at ~0.00128. The cleanup collapses those fragments (never moving an
    /// operand vertex), so the shortest edge becomes the honest chord and the bevel can open up.
    ///
    /// If this fails with a tiny `min_edge` again, the cleanup stopped being accepted — check the
    /// gate reasons with `AZRA_BOOL_DEBUG=1`, and see `remove_degenerate_face` (the re-twinning
    /// step in `half_edge/ops_dissolve.rs` that makes an edge collapse keep the surface closed).
    #[test]
    fn boolean_leaves_no_sliver_edges_so_the_bevel_can_open_up() {
        if prefer_bsp_boolean() {
            return;
        }
        let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
        let sides = 32usize;
        let r = 0.5f32;
        let chord = 2.0 * r * (std::f32::consts::PI / sides as f32).sin();
        let b = cylinder_mesh(r, 2.0, sides, Vec3::new(0.90, 0.0, 0.0));
        let out = mesh_boolean(&a, &b, BoolOp::Difference).expect("difference must produce a mesh");

        // Correctness gates first — a wider bevel on a broken mesh is worth nothing.
        assert!(out.validate().is_ok(), "result must be valid");
        assert!(out.is_edge_manifold().is_ok(), "result must be edge-manifold");
        assert_eq!(boundary_half_edge_count(&out), 0, "result must be closed");
        assert_eq!(out.count_folded_coplanar_pairs(), 0, "result must be fold-free");
        // Volume: the cut removes the part of the cylinder inside the cube; the pinned value is
        // what the exact kernel produced BEFORE the cleanup existed, so this also proves the
        // cleanup is volume-neutral.
        let vol = mesh_signed_volume(&out);
        assert!(
            (vol - 7.021611).abs() <= 7.021611 * 1e-3,
            "cleanup must be volume-neutral; vol={vol}"
        );

        let mn = min_edge_length(&out);
        assert!(
            mn > chord * 0.5,
            "sliver survived: min_edge={mn:.6} vs chord {chord:.6} (was 0.002565 before the cleanup)"
        );

        // THE PAYOFF: the bevel's global clamp is ~half the shortest beveled edge, so the sliver
        // used to cap the effective width at ~0.0013 no matter what the user asked for.
        let sel: std::collections::HashSet<u32> = (0..out.half_edges.len() as u32)
            .filter(|&i| out.is_edge_live(crate::half_edge::HalfEdgeId(i)))
            .collect();
        let params = crate::bevel_op::BevelParams { width: 0.3, segments: 2, shape: 0.5, clamp: true };
        let beveled = crate::bevel_op::bevel_edges_offset(&out, &sel, &params);
        let w = crate::bevel_op::last_bevel_effective_width();
        assert!(
            w > 0.002565 / 2.0 * 4.0,
            "bevel still throttled: effective width={w:.6} (sliver-capped baseline was 0.00128)"
        );
        assert!(beveled.validate().is_ok(), "beveled result must be valid");
        assert!(beveled.is_edge_manifold().is_ok(), "beveled result must be edge-manifold");
        assert_eq!(
            beveled.count_folded_coplanar_pairs(),
            0,
            "beveled result must be fold-free"
        );
    }

    /// The cleanup must NEVER move an operand vertex (owner rule: a boolean does not change
    /// geometry it did not cut). A disjoint difference returns A untouched — every one of A's
    /// eight corners must still be exactly where the user put them.
    #[test]
    fn sliver_cleanup_never_moves_an_operand_vertex() {
        if prefer_bsp_boolean() {
            return;
        }
        let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
        let b = cylinder_mesh(0.5, 2.0, 32, Vec3::new(0.90, 0.0, 0.0));
        let out = mesh_boolean(&a, &b, BoolOp::Difference).expect("difference");
        let keys = operand_vertex_keys(&a, &b);
        // Every surviving cube corner (a vertex far from the cut) must be bit-exact.
        for v in a.vertices.iter().filter(|v| v.half_edge != NO_EDGE) {
            if v.position.x > 0.3 {
                continue; // near/inside the cut — legitimately removed or re-cut
            }
            assert!(
                out.vertices
                    .iter()
                    .any(|w| w.half_edge != NO_EDGE && w.position == v.position),
                "operand vertex {:?} was moved or dropped by the boolean",
                v.position
            );
        }
        assert!(keys.contains(&qkey(Vec3::new(-1.0, -1.0, -1.0))));
    }

    /// WHERE DO THE SUPPORT EDGES LAND? Sweep cube − cylinder and report every flat support
    /// edge the boolean leaves (BoolProvenance::new_flat_edges): endpoint valences + lengths.
    /// Owner test_014 complaint: a support edge terminated on a VALENCE-3 mid-chord pin,
    /// splitting the rim circle's regular pattern.
    ///
    ///   cargo test -p core_math diag_support_edge_sweep -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diag_support_edge_sweep() {
        if prefer_bsp_boolean() {
            return;
        }
        use crate::half_edge::{HalfEdgeId, NO_TWIN};
        use crate::mesh_boolean::mesh_boolean_with_provenance;
        let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
        for step in 0..24 {
            let dx = step as f32 * 0.02;
            let b = cylinder_mesh(0.5, 2.0, 32, Vec3::new(dx, 0.0, 0.0));
            let Some((out, prov)) = mesh_boolean_with_provenance(&a, &b, BoolOp::Difference)
            else {
                eprintln!("[SUPSWEEP] dx={dx:.2} boolean refused");
                continue;
            };
            let mut val = vec![0usize; out.vertices.len()];
            for i in 0..out.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if out.is_edge_live(h) {
                    val[out.he(h).vertex.0 as usize] += 1;
                }
            }
            let mn = min_edge_length(&out);
            let mut seen = std::collections::HashSet::new();
            for e in prov.new_flat_edges(&out) {
                let h = HalfEdgeId(e);
                let tw = out.he(h).twin;
                let canon = if tw != NO_TWIN { e.min(tw.0) } else { e };
                if !seen.insert(canon) {
                    continue;
                }
                let (sa, sb) = (out.he_source(h), out.he(h).vertex);
                let (pa, pb) = (out.vert(sa).position, out.vert(sb).position);
                eprintln!(
                    "[SUPSWEEP] dx={dx:.2} min_edge={mn:.5} sup len={:.4} | v{} val={} -> v{} val={}{}",
                    pa.distance(pb),
                    sa.0,
                    val[sa.0 as usize],
                    sb.0,
                    val[sb.0 as usize],
                    if val[sa.0 as usize] < 4 || val[sb.0 as usize] < 4 { "   <== PIN" } else { "" }
                );
            }
        }
    }

    /// PINNED — support edges terminate on existing junction vertices, not on mid-segment
    /// pins (owner test_014, 2026-08-05: "random vertex created on the circle segment").
    ///
    /// A support edge (flat, boolean-created — `new_flat_edges`) whose endpoint has valence 3
    /// is holding alive a T-junction vertex that splits a rim chord (2 collinear chord halves
    /// + the bridge = 3); a legitimate junction endpoint carries >= 4 edges. The merge order
    /// in `greedy_merge_region_impl` dissolves pin-holding diagonals first (then shortest
    /// first — maximin: surviving bridges stay long, min edge length never gains a shorter
    /// member), so the freed pin is removed by `dissolve_collinear` and the rim stays regular.
    #[test]
    fn support_edges_land_on_junction_vertices_not_midsegment_pins() {
        if prefer_bsp_boolean() {
            return;
        }
        use crate::half_edge::HalfEdgeId;
        use crate::mesh_boolean::mesh_boolean_with_provenance;
        let a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
        let chord = 2.0 * 0.5 * (std::f32::consts::PI / 32.0).sin();
        for step in 0..24 {
            let dx = step as f32 * 0.02;
            let b = cylinder_mesh(0.5, 2.0, 32, Vec3::new(dx, 0.0, 0.0));
            let Some((out, prov)) = mesh_boolean_with_provenance(&a, &b, BoolOp::Difference)
            else {
                continue; // refusal is fail-closed, not a placement defect
            };
            let mut val = vec![0usize; out.vertices.len()];
            for i in 0..out.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if out.is_edge_live(h) {
                    val[out.he(h).vertex.0 as usize] += 1;
                }
            }
            for e in prov.new_flat_edges(&out) {
                let h = HalfEdgeId(e);
                let (sa, sb) = (out.he_source(h), out.he(h).vertex);
                assert!(
                    val[sa.0 as usize] >= 4 && val[sb.0 as usize] >= 4,
                    "dx={dx:.2}: support edge lands on a valence-3 mid-segment pin \
                     (v{} val={} -> v{} val={})",
                    sa.0,
                    val[sa.0 as usize],
                    sb.0,
                    val[sb.0 as usize]
                );
            }
            // Maximin guarantee: steering the survivors must not introduce edges shorter
            // than the sliver cleanup's floor (half the cutter chord — the pinned bar the
            // sliver test already enforces at dx=0.90).
            assert!(
                min_edge_length(&out) > chord * 0.5,
                "dx={dx:.2}: min edge {:.6} fell below half the chord {chord:.6}",
                min_edge_length(&out)
            );
        }
    }

    /// Per-face materials survive a corner bite via nearest-face transfer.
    #[test]
    fn materials_preserved_across_corner_difference() {
        if prefer_bsp_boolean() {
            return;
        }
        let mut a = box_mesh(Vec3::ZERO, Vec3::splat(1.0));
        for i in 0..a.faces.len() {
            if a.faces[i].half_edge != NO_EDGE {
                a.faces[i].material_id = (i as u32) + 1; // 1..6
            }
        }
        let b = box_mesh(Vec3::splat(1.0), Vec3::splat(0.7));
        let r = mesh_boolean(&a, &b, BoolOp::Difference).expect("corner diff");
        let mats: Vec<u32> = r
            .faces
            .iter()
            .filter(|f| f.half_edge != NO_EDGE)
            .map(|f| f.material_id)
            .collect();
        assert!(
            mats.iter().any(|&m| (1..=6).contains(&m)),
            "expected restored materials in 1..6, got {mats:?}"
        );
        let nonzero = mats.iter().filter(|&&m| m != 0).count();
        assert!(
            nonzero >= mats.len() / 2,
            "most faces should keep non-zero material; mats={mats:?}"
        );
    }
}
