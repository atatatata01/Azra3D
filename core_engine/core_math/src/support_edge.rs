//! Boolean SUPPORT-EDGE relocation ("Smart Clamp") — `docs/planning/poly/BEVEL.md` §6.
//!
//! ## What a support edge is
//!
//! A half-edge mesh cannot represent a face with a hole. When a boolean cuts a cylinder
//! through a flat face, the resulting annulus has to be split into simple faces by
//! **bridges** running from the hole rim out to the face's outer boundary. Those bridges are
//! the *support edges*. They are pure topological bookkeeping — they carry no shape.
//!
//! The kernel places them wherever the cut tessellation happened to land, which on the
//! owner's `test_010` produced lengths `0.4332 0.4644 0.1792 0.3821 0.5680`. Bevel's Clamp
//! Overlap caps the width near half the shortest edge incident to a vertex, so that single
//! `0.1792` bridge throttles the bevel for the entire model.
//!
//! ## Why relocating one is SAFE
//!
//! A support edge lies **IN a planar face**: both of its faces are coplanar (`edge_is_flat`).
//! Moving its outer endpoint along that same plane therefore cannot change the silhouette,
//! the volume, or any surface normal — it only re-partitions a flat region into different
//! flat sub-faces. That is what makes this legitimate where a general edge move would not be.
//!
//! **Do NOT extend this to any edge that is not coplanar with both of its faces.** The
//! eligibility test is deliberately two-part and fails closed: an edge must be BOTH
//! *created by a boolean* (`BoolProvenance` / the persisted `op_created` sidecar — never a
//! heuristic) AND *flat*. Anything we cannot prove is a boolean support edge is left alone.
//!
//! ## The relocation rule (owner-specified)
//!
//! In the face's plane, re-route the bridge **radially**: from the hole centre, through the
//! rim vertex, out to the outer boundary.
//! * If an existing outer-boundary vertex lies within `SNAP_DEG` of that radial direction,
//!   SNAP to it — no new vertex — but only when snapping is *not worse than splitting*
//!   (it must not be shorter than the radial split would be). A naive "snap if within the
//!   threshold" can pull the bridge somewhere worse than where it started.
//! * Otherwise split the outer boundary edge at the radial intersection and use that vertex.
//! * If neither option **improves on the current placement** (strictly longer, and no worse
//!   angular deviation from the radial), LEAVE IT ALONE.
//!
//! ## Non-destructive, and deterministic
//!
//! The caller (the Bevel modifier) runs this on its own working copy, so the base mesh is
//! never touched and toggling the modifier off reverts everything.
//!
//! Every step is order-stable: candidates are keyed by quantized POSITION (ids move as we
//! edit) and sorted; region traversal and boundary-loop construction start from the lowest
//! id and use sorted containers. No `HashSet`/`HashMap` iteration order reaches anything
//! that allocates a vertex or picks a candidate — that class of bug cost this project two
//! days already (boolean determinism, commit 7607c0e).

use crate::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_FACE, NO_TWIN};
use glam::Vec3;
use std::collections::{BTreeMap, BTreeSet};

/// An existing outer-boundary vertex this close (degrees) to the radial direction is a snap
/// candidate instead of splitting an edge.
const SNAP_DEG: f32 = 5.0;
/// Positional quantization for the order-independent edge key (matches the app's sidecar key).
const Q: f32 = 1e5;

type PosKey = (i64, i64, i64);
type EdgeKey = (PosKey, PosKey);

fn pkey(p: Vec3) -> PosKey {
    ((p.x * Q).round() as i64, (p.y * Q).round() as i64, (p.z * Q).round() as i64)
}

fn edge_key(mesh: &HalfEdgeMesh, he: HalfEdgeId) -> EdgeKey {
    let a = pkey(mesh.vert(mesh.he_source(he)).position);
    let b = pkey(mesh.vert(mesh.he(he).vertex).position);
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Lowest live half-edge id whose endpoints match `key`, or `None`. Deterministic by
/// construction (ascending scan), and stale-proof: ids move under `dissolve_edge` /
/// `split_edge`, positions do not.
fn find_edge_by_key(mesh: &HalfEdgeMesh, key: EdgeKey) -> Option<HalfEdgeId> {
    (0..mesh.half_edges.len() as u32)
        .map(HalfEdgeId)
        .filter(|&h| mesh.is_edge_live(h))
        .find(|&h| edge_key(mesh, h) == key)
}

/// Outcome of relocating one support edge, for the caller's bookkeeping (e.g. Bevel Smart
/// Select still has to exclude the edge under its NEW endpoints).
#[derive(Clone, Copy, Debug)]
pub struct Relocated {
    pub from: (Vec3, Vec3),
    pub to: (Vec3, Vec3),
}

/// Relocate every eligible boolean support edge in `mesh`, in place.
///
/// `eligible` = canonical half-edge ids that the CALLER has proven are boolean-created
/// (`BoolProvenance::new_flat_edges`, or the persisted `op_created` tags intersected with
/// `edge_is_flat`). This function re-verifies flatness and every structural precondition and
/// refuses anything it cannot prove; an empty or unproven input relocates nothing.
///
/// Each relocation is attempted on a scratch clone and kept only if the result still
/// validates, is manifold, is fold-free, and has the SAME signed volume — a coplanar bridge
/// move that changed the volume would mean we moved real geometry, and is rejected.
pub fn relocate_support_edges(mesh: &mut HalfEdgeMesh, eligible: &[HalfEdgeId]) -> Vec<Relocated> {
    // Deterministic candidate order: quantized position keys, sorted + deduped.
    let mut keys: Vec<EdgeKey> = eligible
        .iter()
        .copied()
        .filter(|&h| mesh.is_edge_live(h) && mesh.edge_is_flat(h, 0.0))
        .map(|h| edge_key(mesh, h))
        .collect();
    keys.sort_unstable();
    keys.dedup();

    let mut out = Vec::new();
    for key in keys {
        let Some(he) = find_edge_by_key(mesh, key) else { continue };
        if !mesh.edge_is_flat(he, 0.0) {
            continue; // an earlier relocation changed this edge's neighbourhood
        }
        let before_vol = crate::mesh_boolean::mesh_signed_volume(mesh);
        let before_min = min_edge_length(mesh);
        let mut scratch = mesh.clone();
        let Some(rel) = try_relocate(&mut scratch, he) else { continue };
        let after_vol = crate::mesh_boolean::mesh_signed_volume(&scratch);
        let tol = (before_vol.abs() * 1e-4).max(1e-6);
        // COOPERATE WITH CLAMP OVERLAP, NEVER FIGHT IT. Clamp caps the width near half the
        // shortest edge at a vertex, so a relocation that CREATES a shorter edge than the one
        // it fixed (a split landing close to an existing boundary vertex) would throttle the
        // bevel just as hard somewhere else. Refuse any move that lowers the mesh's minimum
        // edge length: relocate what can be relocated, then let clamp handle what remains.
        if scratch.validate().is_ok()
            && scratch.is_manifold().is_ok()
            && scratch.count_folded_coplanar_pairs() == 0
            && (after_vol - before_vol).abs() <= tol
            && min_edge_length(&scratch) >= before_min - 1e-6
        {
            *mesh = scratch;
            out.push(rel);
        }
        // else: fail closed — keep the mesh exactly as it was.
    }
    out
}

/// Where a relocated bridge should land.
#[derive(Clone, Copy)]
enum Target {
    /// An existing outer-boundary vertex (snap: no new vertex).
    Vertex(VertexId),
    /// Split the outer boundary edge `(a, b)` at parameter `t` measured from `a`.
    Split(VertexId, VertexId, f32),
}

macro_rules! bail {
    ($why:expr) => {{
        if std::env::var("AZRA_SMARTCLAMP_DEBUG").is_ok() {
            eprintln!("[SMARTCLAMP] refuse: {}", $why);
        }
        return None;
    }};
}

fn try_relocate(mesh: &mut HalfEdgeMesh, he: HalfEdgeId) -> Option<Relocated> {
    let e = mesh.he(he).clone();
    if e.twin == NO_TWIN || e.face == NO_FACE {
        return None;
    }
    let twin_face = mesh.he(e.twin).face;
    if twin_face == NO_FACE || twin_face == e.face {
        return None; // slit edge: not a bridge between two faces
    }
    let n = mesh.compute_face_normal(e.face).normalize_or_zero();
    if n.length_squared() < 0.5 {
        return None;
    }

    // The coplanar region this bridge lives in, and its boundary loops.
    let region = coplanar_region(mesh, e.face);
    let loops = region_boundary_loops(mesh, &region);
    if loops.len() < 2 {
        bail!(format!("only {} boundary loop(s) - no hole", loops.len()));
    }

    // Which loop is the outer boundary: the one enclosing the largest area in-plane.
    let (u_ax, v_ax) = plane_basis(n);
    let proj = |p: Vec3| (p.dot(u_ax), p.dot(v_ax));
    let area = |lp: &Vec<VertexId>| -> f32 {
        let mut a = 0.0;
        for i in 0..lp.len() {
            let (x0, y0) = proj(mesh.vert(lp[i]).position);
            let (x1, y1) = proj(mesh.vert(lp[(i + 1) % lp.len()]).position);
            a += x0 * y1 - x1 * y0;
        }
        (a * 0.5).abs()
    };
    if std::env::var("AZRA_SMARTCLAMP_DEBUG").is_ok() {
        for (i, lp) in loops.iter().enumerate() {
            eprintln!(
                "[SMARTCLAMP]   loop{i}: n={} area={:.4} first={:?}",
                lp.len(),
                area(lp),
                mesh.vert(lp[0]).position
            );
        }
    }
    let outer_idx = (0..loops.len()).max_by(|&a, &b| {
        area(&loops[a]).partial_cmp(&area(&loops[b])).unwrap_or(std::cmp::Ordering::Equal)
    })?;

    // Vertex -> loop index (a planar region's boundary loops are vertex-disjoint).
    let mut loop_of: BTreeMap<u32, usize> = BTreeMap::new();
    for (i, lp) in loops.iter().enumerate() {
        for v in lp {
            loop_of.entry(v.0).or_insert(i);
        }
    }

    // Orient: rim endpoint on an INNER loop, outer endpoint on the OUTER loop. Anything else
    // (both on the same loop, either endpoint unclassified, rim-to-rim between two holes) is
    // not a case we can prove, so we refuse it.
    let (a_v, b_v) = (mesh.he_source(he), e.vertex);
    let (la, lb) = (*loop_of.get(&a_v.0)?, *loop_of.get(&b_v.0)?);
    let (rim_v, out_v, rim_loop) = if lb == outer_idx && la != outer_idx {
        (a_v, b_v, la)
    } else if la == outer_idx && lb != outer_idx {
        (b_v, a_v, lb)
    } else {
        bail!(format!("endpoints not rim->outer (la={la} lb={lb} outer={outer_idx})"));
    };

    // Radial direction: from the hole centre, through the rim vertex, in the face's plane.
    let rim_pts: Vec<Vec3> = loops[rim_loop].iter().map(|&v| mesh.vert(v).position).collect();
    let centre = rim_pts.iter().copied().sum::<Vec3>() / rim_pts.len() as f32;
    let r_pos = mesh.vert(rim_v).position;
    let radial = {
        let d = r_pos - centre;
        (d - n * d.dot(n)).normalize_or_zero()
    };
    if radial.length_squared() < 0.5 {
        return None; // rim vertex sits on the hole centre — no meaningful radial
    }

    let outer_loop = &loops[outer_idx];
    let ang_to = |p: Vec3| -> f32 {
        let d = p - r_pos;
        let d = d - n * d.dot(n);
        if d.length_squared() < 1e-12 {
            return 180.0;
        }
        d.normalize().dot(radial).clamp(-1.0, 1.0).acos().to_degrees()
    };

    // --- candidate SPLIT: first outer-boundary segment the radial ray crosses -------------
    let ro = proj(r_pos);
    let rd = (radial.dot(u_ax), radial.dot(v_ax));
    let mut split: Option<(f32, VertexId, VertexId, f32)> = None; // (t, a, b, s)
    for i in 0..outer_loop.len() {
        let (va, vb) = (outer_loop[i], outer_loop[(i + 1) % outer_loop.len()]);
        if va == rim_v || vb == rim_v {
            continue;
        }
        let (pa, pb) = (proj(mesh.vert(va).position), proj(mesh.vert(vb).position));
        let seg = (pb.0 - pa.0, pb.1 - pa.1);
        let den = rd.0 * seg.1 - rd.1 * seg.0;
        if den.abs() < 1e-9 {
            continue; // parallel
        }
        let dx = (pa.0 - ro.0, pa.1 - ro.1);
        let t = (dx.0 * seg.1 - dx.1 * seg.0) / den;
        let s = (dx.0 * rd.1 - dx.1 * rd.0) / den;
        if t > 1e-6 && (-1e-6..=1.0 + 1e-6).contains(&s) {
            if split.map_or(true, |(bt, _, _, _)| t < bt) {
                split = Some((t, va, vb, s.clamp(0.0, 1.0)));
            }
        }
    }
    let Some((split_len, split_a, split_b, split_s)) = split else {
        bail!("radial ray hits no outer boundary segment");
    };

    // --- candidate SNAP: closest-to-radial existing outer vertex within SNAP_DEG ----------
    // Sorted scan (loop order is deterministic), strict `<` so ties keep the first.
    let mut snap: Option<(VertexId, f32, f32)> = None; // (v, angle, length)
    for &v in outer_loop.iter() {
        if v == rim_v {
            continue;
        }
        let p = mesh.vert(v).position;
        let (a, l) = (ang_to(p), p.distance(r_pos));
        if a <= SNAP_DEG && snap.map_or(true, |(_, ba, _)| a < ba) {
            snap = Some((v, a, l));
        }
    }

    // OWNER-FLAGGED RISK: snapping "because it is within the threshold" can pull the bridge
    // to a WORSE place than it started. Snap only when it is also not shorter than the
    // radial split would be; otherwise split.
    let target = match snap {
        Some((v, _, l)) if l >= split_len - 1e-6 => Target::Vertex(v),
        _ => Target::Split(split_a, split_b, split_s),
    };
    let (new_pos, new_len, new_ang) = match target {
        Target::Vertex(v) => {
            let p = mesh.vert(v).position;
            (p, p.distance(r_pos), ang_to(p))
        }
        Target::Split(a, b, s) => {
            let p = mesh.vert(a).position.lerp(mesh.vert(b).position, s);
            (p, p.distance(r_pos), ang_to(p))
        }
    };

    // --- the improvement gate ------------------------------------------------------------
    // Must be STRICTLY longer (this is the whole point: stop throttling Clamp Overlap) and no
    // worse in angular deviation from the radial. Otherwise LEAVE IT ALONE.
    let cur_pos = mesh.vert(out_v).position;
    let (cur_len, cur_ang) = (cur_pos.distance(r_pos), ang_to(cur_pos));
    if let Target::Vertex(v) = target {
        if v == out_v {
            return None; // already where we would put it
        }
    }
    if new_len <= cur_len * (1.0 + 1e-4) || new_ang > cur_ang + 1e-3 {
        bail!(format!(
            "no improvement: len {cur_len:.4}->{new_len:.4} ang {cur_ang:.2}->{new_ang:.2}"
        ));
    }

    // --- topological edit: dissolve the bridge, re-split the merged face radially ---------
    // Both faces are coplanar, so the merge and the re-split are shape-preserving; the split
    // vertex lands ON the existing outer edge (collinear), which is why the volume is
    // unchanged and the caller's volume gate holds.
    let Some(f) = mesh.dissolve_edge(he) else { bail!("dissolve_edge refused") };
    let ring = mesh.face_vertices(f);
    {
        // A merged face that touches itself (duplicate vertex in the ring) cannot be split
        // reliably — `split_face_between` resolves vertices by first occurrence. Refuse.
        let mut seen = BTreeSet::new();
        if !ring.iter().all(|v| seen.insert(v.0)) {
            bail!("merged face touches itself");
        }
    }
    let t_vert = match target {
        Target::Vertex(v) => v,
        Target::Split(a, b, s) => {
            let hes = mesh.face_half_edges(f);
            let Some(h) = hes.into_iter().find(|&h| {
                let (s0, d0) = (mesh.he_source(h), mesh.he(h).vertex);
                (s0 == a && d0 == b) || (s0 == b && d0 == a)
            }) else {
                bail!("target segment is not on the merged face");
            };
            // The target segment must belong to the MERGED face, else the split is not on a
            // boundary we are allowed to re-thread (it is past a neighbouring bridge).
            let t = if mesh.he_source(h) == a { s } else { 1.0 - s };
            mesh.split_edge(h, t)
        }
    };
    if mesh.split_face_between(f, rim_v, t_vert).is_none() {
        bail!("split_face_between refused");
    }
    Some(Relocated { from: (r_pos, cur_pos), to: (r_pos, new_pos) })
}

/// Shortest live edge in the mesh — the quantity Clamp Overlap is governed by.
fn min_edge_length(mesh: &HalfEdgeMesh) -> f32 {
    let mut m = f32::INFINITY;
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        let l = mesh
            .vert(mesh.he_source(h))
            .position
            .distance(mesh.vert(mesh.he(h).vertex).position);
        if l < m {
            m = l;
        }
    }
    m
}

/// Orthonormal in-plane basis for a unit normal.
fn plane_basis(n: Vec3) -> (Vec3, Vec3) {
    let a = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let u = (a - n * a.dot(n)).normalize();
    (u, n.cross(u))
}

/// Faces reachable from `seed` across FLAT edges — i.e. the maximal coplanar face region the
/// support edge lives in. `BTreeSet` + sorted frontier keeps traversal order fixed.
fn coplanar_region(mesh: &HalfEdgeMesh, seed: FaceId) -> BTreeSet<u32> {
    let mut seen = BTreeSet::new();
    seen.insert(seed.0);
    let mut stack = vec![seed];
    while let Some(f) = stack.pop() {
        let mut nbr: Vec<FaceId> = Vec::new();
        for h in mesh.face_half_edges(f) {
            if !mesh.edge_is_flat(h, 0.0) {
                continue;
            }
            let t = mesh.he(h).twin;
            if t == NO_TWIN {
                continue;
            }
            let tf = mesh.he(t).face;
            if tf != NO_FACE && !seen.contains(&tf.0) {
                nbr.push(tf);
            }
        }
        nbr.sort_unstable_by_key(|f| f.0);
        for f in nbr {
            if seen.insert(f.0) {
                stack.push(f);
            }
        }
        if seen.len() > 100_000 {
            break; // frame-budget guard; a runaway region is not a planar cut face
        }
    }
    seen
}

/// Boundary loops of a coplanar face region, as vertex rings in winding order.
///
/// A boundary half-edge is one whose face is in the region and whose twin's face is not (or
/// whose edge is not flat). Its successor is found by walking `next` and hopping across
/// interior edges — the standard region-border walk. Loops are emitted starting from the
/// lowest half-edge id so the output is reproducible.
fn region_boundary_loops(mesh: &HalfEdgeMesh, region: &BTreeSet<u32>) -> Vec<Vec<VertexId>> {
    let is_border = |h: HalfEdgeId| -> bool {
        if !mesh.is_edge_live(h) {
            return false;
        }
        let e = mesh.he(h);
        if e.face == NO_FACE || !region.contains(&e.face.0) {
            return false;
        }
        if e.twin == NO_TWIN {
            return true;
        }
        let tf = mesh.he(e.twin).face;
        tf == NO_FACE || !region.contains(&tf.0) || !mesh.edge_is_flat(h, 0.0)
    };

    let mut borders: Vec<HalfEdgeId> = Vec::new();
    for &f in region {
        for h in mesh.face_half_edges(FaceId(f)) {
            if is_border(h) {
                borders.push(h);
            }
        }
    }
    borders.sort_unstable_by_key(|h| h.0);
    borders.dedup();

    let mut used: BTreeSet<u32> = BTreeSet::new();
    let mut loops = Vec::new();
    for &start in &borders {
        if used.contains(&start.0) {
            continue;
        }
        let mut ring = Vec::new();
        let mut h = start;
        let mut guard = 0;
        loop {
            used.insert(h.0);
            ring.push(mesh.he(h).vertex);
            // Advance around the region border: from h's destination, take the next border
            // half-edge, hopping over interior (flat, in-region) edges.
            let mut nx = mesh.he(h).next;
            let mut hop = 0;
            while !is_border(nx) {
                let t = mesh.he(nx).twin;
                if t == NO_TWIN {
                    break;
                }
                nx = mesh.he(t).next;
                hop += 1;
                if hop > 1000 {
                    break;
                }
            }
            if !is_border(nx) || nx == start {
                break;
            }
            h = nx;
            guard += 1;
            if guard > 100_000 {
                break;
            }
        }
        if ring.len() >= 3 {
            loops.push(ring);
        }
    }
    loops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bevel_op::{bevel_edges_offset, last_bevel_effective_width, BevelParams};
    use crate::mesh_boolean::{mesh_boolean_with_provenance, mesh_signed_volume, BoolOp};

    /// Real cube-minus-cylinder (NOT a synthetic fixture), then a second cut that puts extra
    /// vertices on the cut face's OUTER boundary near the hole — which is what makes the
    /// kernel bridge badly. Measured support-edge lengths: `0.3761` and `1.6553`, i.e. the
    /// same pathology as the owner's `test_010` (`0.1792` among five). A lone cylinder in a
    /// bare cube bridges straight to the cube corners and has nothing to improve, which is
    /// exactly why that shape is NOT the fixture here.
    fn cut_cube() -> (HalfEdgeMesh, Vec<HalfEdgeId>) {
        let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
        let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
        let (r1, _) =
            mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference).expect("boolean");
        let mut notch = HalfEdgeMesh::new_box(Vec3::splat(0.3));
        for v in notch.vertices.iter_mut() {
            v.position += Vec3::new(1.0, 2.0, 0.2);
        }
        let (res, prov) =
            mesh_boolean_with_provenance(&r1, &notch, BoolOp::Difference).expect("boolean 2");
        let sup = prov.new_flat_edges(&res).into_iter().map(HalfEdgeId).collect();
        (res, sup)
    }

    fn lengths(mesh: &HalfEdgeMesh, hes: &[HalfEdgeId]) -> Vec<f32> {
        let mut l: Vec<f32> = hes
            .iter()
            .filter(|&&h| mesh.is_edge_live(h))
            .map(|&h| {
                mesh.vert(mesh.he_source(h)).position.distance(mesh.vert(mesh.he(h).vertex).position)
            })
            .collect();
        l.sort_by(|a, b| a.partial_cmp(b).unwrap());
        l
    }

    /// MEASURED, NOT ASSUMED (2026-08-05) — on a real cube-minus-cylinder the radial route
    /// is SHORTER than the bridge the boolean already placed, for EVERY support edge.
    ///
    /// This is geometry, not a bug: the radial exit from a hole in a star-shaped face is the
    /// NEAREST exit, so re-routing radially can only shorten. Confirmed on 20+ real boolean
    /// configurations (plain / off-centre / multi-cut / L-shaped concave faces, 8..32 segments)
    /// and on the owner's `test_010`, where the four classifiable support edges would go
    /// 0.4332->0.1045, 0.4644->0.0617, 0.5680->0.3918, 0.3821->0.1946.
    ///
    /// So Smart Clamp's improvement gate REFUSES all of them, and this test pins that. If a
    /// future change makes the rule relocate here, it is lengthening nothing and this test
    /// must be re-derived against Blender first (BEVEL.md section 0 rule 1).
    #[test]
    fn radial_reroute_is_shorter_than_the_existing_bridge_so_nothing_is_relocated() {
        let (mesh, sup) = cut_cube();
        assert!(!sup.is_empty(), "the cut face must carry support edges");
        let before = lengths(&mesh, &sup);
        let mut relocated = mesh.clone();
        let moves = relocate_support_edges(&mut relocated, &sup);
        eprintln!("[SMARTCLAMP] support lengths={before:?} relocated={}", moves.len());
        assert!(moves.is_empty(), "radial re-route claimed an improvement it cannot have");
        // ...and refusing leaves the mesh byte-identical: the gate is fail-closed.
        assert_eq!(relocated.vertices.len(), mesh.vertices.len());
        assert_eq!(relocated.face_count(), mesh.face_count());
    }

    /// THE SAFETY CONTRACT, asserted on whatever the rule does decide to move: the mesh stays
    /// valid + manifold + fold-free, the SOLID is unchanged (a coplanar bridge move must not
    /// alter the volume), and the mesh's minimum edge length never DROPS -- because that
    /// minimum is precisely what Clamp Overlap is governed by, so lowering it would fight the
    /// clamp instead of cooperating with it.
    #[test]
    fn every_relocation_preserves_the_solid_and_never_lowers_the_clamp_limit() {
        let (mesh, sup) = cut_cube();
        let vol_before = mesh_signed_volume(&mesh);
        let min_before = min_edge_length(&mesh);
        let mut relocated = mesh.clone();
        let _ = relocate_support_edges(&mut relocated, &sup);
        assert!(relocated.validate().is_ok(), "invalid: {:?}", relocated.validate());
        assert!(relocated.is_manifold().is_ok(), "non-manifold: {:?}", relocated.is_manifold());
        assert_eq!(relocated.count_folded_coplanar_pairs(), 0, "folds introduced");
        let vol_after = mesh_signed_volume(&relocated);
        assert!(
            (vol_after - vol_before).abs() < vol_before.abs() * 1e-4,
            "volume changed: {vol_before} -> {vol_after}"
        );
        assert!(
            min_edge_length(&relocated) >= min_before - 1e-6,
            "relocation lowered the clamp limit"
        );
    }

    /// CLAMP OVERLAP MUST NOT REGRESS. Whatever the rule does, the width the bevel can
    /// actually reach with clamp ON is never smaller than without Smart Clamp.
    #[test]
    fn smart_clamp_never_lowers_the_clamped_bevel_width() {
        let (mesh, sup) = cut_cube();
        let params = BevelParams { width: 0.4, segments: 2, shape: 0.5, clamp: true };
        let sel: std::collections::HashSet<u32> = (0..mesh.half_edges.len() as u32)
            .filter(|&i| mesh.is_edge_live(HalfEdgeId(i)))
            .collect();
        let _ = bevel_edges_offset(&mesh, &sel, &params);
        let w_off = last_bevel_effective_width();

        let mut relocated = mesh.clone();
        let _ = relocate_support_edges(&mut relocated, &sup);
        let sel2: std::collections::HashSet<u32> = (0..relocated.half_edges.len() as u32)
            .filter(|&i| relocated.is_edge_live(HalfEdgeId(i)))
            .collect();
        let _ = bevel_edges_offset(&relocated, &sel2, &params);
        let w_on = last_bevel_effective_width();
        eprintln!("[SMARTCLAMP] effective width off={w_off:.5} on={w_on:.5}");
        assert!(w_on >= w_off - 1e-6, "smart clamp LOWERED the clamped width");
    }

    /// FAIL CLOSED on provenance: an edge that is flat but was NOT created by a boolean is
    /// never touched, however badly placed. Only the caller's proven tag set is eligible.
    #[test]
    fn a_flat_edge_without_boolean_provenance_is_never_relocated() {
        let (mesh, _) = cut_cube();
        let before = mesh.clone();
        let mut m = mesh.clone();
        // Deliberately pass NOTHING: this is what `BoolProvenance::default()` yields.
        assert!(relocate_support_edges(&mut m, &[]).is_empty());
        assert_eq!(m.vertices.len(), before.vertices.len());
        assert_eq!(m.face_count(), before.face_count());
    }

    /// DETERMINISM: the same input must produce the same relocation every time, or the
    /// viewport flickers while dragging the width.
    #[test]
    fn relocation_is_deterministic() {
        let (mesh, sup) = cut_cube();
        let run = || {
            let mut m = mesh.clone();
            relocate_support_edges(&mut m, &sup);
            m.vertices.iter().map(|v| pkey(v.position)).collect::<Vec<_>>()
        };
        let a = run();
        for _ in 0..4 {
            assert_eq!(a, run(), "relocation is not reproducible");
        }
    }

    /// FAIL CLOSED: an empty eligibility set (no boolean provenance, no tags) relocates
    /// nothing -- we never guess which edges are support edges.
    #[test]
    fn no_eligible_edges_changes_nothing() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let before = m.clone();
        assert!(relocate_support_edges(&mut m, &[]).is_empty());
        assert_eq!(m.vertices.len(), before.vertices.len());
        assert_eq!(m.face_count(), before.face_count());
    }
}

