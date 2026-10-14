// PolySculpt stroke pick — PR-4 of ZB_RESPONSIVE_SCULPT_PLAN §7.1.
//
// The stroke path's hit producer. `ray_pick_mesh_face` is BANNED here (O(F) CPU walk —
// measured 523 ms p50 per stamp on the owner's 161k-face body, 98% of the stroke frame).
// This module provides the bounded CPU path:
//
//   • SMALL meshes (≤ FACE_TEST_BUDGET faces): a full loop over the SAME shared per-face
//     test (`ray_face_hit`) — bounded by construction, correct on giant n-gons (a raw
//     cube's face spans every grid cell; a cell walk that only visits vert-holding cells
//     would miss it).
//   • DENSE meshes: Amanatides–Woo grid-DDA over the existing `VertGrid` cells along the
//     ray. For each crossed cell (plus its 3×3×3 neighborhood — a face's verts can sit
//     one cell over), incident faces of bucket verts via `vertex_fan_into`, deduped, each
//     tested with the SAME `ray_face_hit`. Ray-ordered: once a hit exists, the walk stops
//     at the first cell whose entry lies beyond it. Aborts at FACE_TEST_BUDGET and fails
//     CLOSED (no stamp) — never falls back to the full O(F) walk on a dense mesh.
//
// The 8×8 GPU viewport-crop pick (§7.1 steps 1–8) fronts this as the production path in
// PR-4b; both return `SculptHit` so the swap is invisible to the stroke code.
//
// Known dense-mesh edge (accepted, documented): a face MUCH larger than a grid cell whose
// verts all sit outside every crossed cell's neighborhood is missed → no stamp (fail
// closed). Remesh working meshes (uniform, cell = 2% of extent) cannot produce it; the
// GPU crop covers it when it lands.

use core_math::half_edge::{HalfEdgeMesh, VertexId};
use core_math::scene_graph::NodeId;
use glam::Vec3;
use std::collections::HashSet;

use super::sculpt::VertGrid;

/// Total `ray_face_hit` calls a single pick may spend (§7.1: start 4096, measure).
/// Also the small-mesh threshold: at or below it, the bounded full loop is both correct
/// and cheap (~sub-ms), so the DDA's giant-n-gon blind spot never applies.
pub(crate) const FACE_TEST_BUDGET: usize = 4096;

/// Stroke hit (ZB plan §11.1). `face` stays `None` in P0 — the VertGrid supplies the
/// normal; do NOT rebuild a face-id VB to fill it.
pub(crate) struct SculptHit {
    pub local: Vec3,
    pub world: Vec3,
    pub normal: Vec3,
    pub face: Option<u32>,
    pub pick_ms: f32,
}

impl crate::AzraApp {
    /// The ONE stroke-path hit producer (`handle_poly_sculpt_input` + Grab first hit).
    /// `_screen` is reserved for the PR-4b GPU crop (cursor-centred 8×8 viewport tile).
    pub(crate) fn poly_sculpt_pick_hit(
        &mut self,
        nid: NodeId,
        ray_o: Vec3,
        ray_d: Vec3,
        screen: Option<(egui::Pos2, egui::Rect)>,
    ) -> Option<SculptHit> {
        let t0 = std::time::Instant::now();
        self.ensure_poly_sculpt_working(nid);
        let world = self.scene.scene_graph.world_transform(nid);
        // PR-4b: GPU crop pick FIRST (production; O(1) in F). Miss / no resources / no
        // GpuMesh → the bounded CPU DDA below (§7.1's stated fallback chain).
        let gpu_hit = screen.and_then(|(cursor, rect)| self.poly_sculpt_gpu_pick(nid, cursor, rect));
        let w = self.poly_sculpt_working.get(&nid)?;
        let hidden = HashSet::new();
        let hit_w = if let Some(hw) = gpu_hit {
            hw
        } else {
            let csr = (w.vert_off.as_slice(), w.vert_faces.as_slice());
            // PR-10: the PBVH (when the flag built it) is the CPU pick too — front-to-
            // back node traversal beats the cell walk. None → DDA, unchanged.
            let t = pick_bounded_with(
                &w.mesh,
                &w.grid,
                w.pbvh.as_ref(),
                world,
                ray_o,
                ray_d,
                &hidden,
                Some(csr),
            )?;
            ray_o + ray_d * t
        };
        let local = world.inverse().transform_point3(hit_w);
        // NEAREST live vert normal (was: FIRST id in bucket order — arbitrary insertion
        // order, sticky per screen location, with a silent Vec3::Y fallback; that noise
        // became the dab deposit direction and the cursor ring orientation. Blender's
        // cursor/stroke sample the HIT surface — nearest vert is our honest analog).
        let normal = {
            let ids = w.grid.query(&w.mesh, local, w.grid.cell_size() * 2.0);
            ids.iter()
                .min_by(|&&a, &&b| {
                    let da = (w.mesh.vert(VertexId(a)).position - local).length_squared();
                    let db = (w.mesh.vert(VertexId(b)).position - local).length_squared();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|&i| w.mesh.vert(VertexId(i)).normal)
                .unwrap_or(Vec3::Y)
                .normalize_or_zero()
        };
        let pick_ms = t0.elapsed().as_secs_f32() * 1e3;
        self.sculpt.poly_profile.pick_ms = pick_ms;
        Some(SculptHit {
            local,
            world: hit_w,
            normal,
            face: None,
            pick_ms,
        })
    }
}

/// Bounded pick dispatch: full loop for small meshes, grid-DDA for dense ones.
/// World-space ray in, world-space `t` out (same contract as `ray_pick_mesh_face`).
/// `csr`: PR-2's `(vert_off, vert_faces)` from the working mesh — incident faces without
/// a half-edge walk. `None` (bare-mesh tests) falls back to `vertex_fan_into`.
pub(crate) fn pick_bounded(
    mesh: &HalfEdgeMesh,
    grid: &VertGrid,
    world: glam::Mat4,
    ray_o: Vec3,
    ray_d: Vec3,
    hidden: &HashSet<u32>,
    csr: Option<(&[u32], &[u32])>,
) -> Option<f32> {
    pick_bounded_counted(mesh, grid, world, ray_o, ray_d, hidden, csr).0
}

/// PR-10: pick with an optional PBVH — dense meshes traverse nodes front-to-back
/// (needs the CSR); everything else falls to `pick_bounded` (small-mesh loop / DDA).
#[allow(clippy::too_many_arguments)]
pub(crate) fn pick_bounded_with(
    mesh: &HalfEdgeMesh,
    grid: &VertGrid,
    pbvh: Option<&super::sculpt_pbvh::SculptPbvh>,
    world: glam::Mat4,
    ray_o: Vec3,
    ray_d: Vec3,
    hidden: &HashSet<u32>,
    csr: Option<(&[u32], &[u32])>,
) -> Option<f32> {
    if mesh.faces.len() > FACE_TEST_BUDGET {
        if let (Some(p), Some(c)) = (pbvh, csr) {
            return p
                .pick(mesh, world, ray_o, ray_d, hidden, c, FACE_TEST_BUDGET)
                .0;
        }
    }
    pick_bounded(mesh, grid, world, ray_o, ray_d, hidden, csr)
}

/// PR-8 / G1: same pick, plus the number of per-face ray tests it spent. The
/// acceptance gate asserts THIS does not scale with `F` (a structural property —
/// wall-clock gates under suite contention are the recorded flake class).
pub(crate) fn pick_bounded_counted(
    mesh: &HalfEdgeMesh,
    grid: &VertGrid,
    world: glam::Mat4,
    ray_o: Vec3,
    ray_d: Vec3,
    hidden: &HashSet<u32>,
    csr: Option<(&[u32], &[u32])>,
) -> (Option<f32>, usize) {
    if mesh.faces.len() <= FACE_TEST_BUDGET {
        // Small mesh: the full loop IS within budget, and giant n-gons stay correct.
        // Deliberately NOT `ray_pick_mesh_face` (grep-gated on the stroke path) — same
        // shared per-face test, bounded by the mesh-size gate.
        let mut best: Option<f32> = None;
        for i in 0..mesh.faces.len() as u32 {
            if let Some(t) = crate::poly_selection::ray_face_hit(
                mesh,
                world,
                core_math::half_edge::FaceId(i),
                ray_o,
                ray_d,
                hidden,
            ) {
                if best.map_or(true, |bt| t < bt) {
                    best = Some(t);
                }
            }
        }
        return (best, mesh.faces.len());
    }
    dda_pick(mesh, grid, world, ray_o, ray_d, hidden, csr)
}

/// Amanatides–Woo voxel walk over `VertGrid` cells along the LOCAL-space ray.
/// Face tests run in WORLD space via the shared `ray_face_hit` so the returned `t`
/// is directly comparable with the full walk's.
fn dda_pick(
    mesh: &HalfEdgeMesh,
    grid: &VertGrid,
    world: glam::Mat4,
    ray_o: Vec3,
    ray_d: Vec3,
    hidden: &HashSet<u32>,
    csr: Option<(&[u32], &[u32])>,
) -> (Option<f32>, usize) {
    const MAX_CELLS: usize = 4096;
    let inv = world.inverse();
    let o = inv.transform_point3(ray_o);
    let d = inv.transform_vector3(ray_d).normalize_or_zero();
    if d == Vec3::ZERO {
        return (None, 0);
    }
    let cell = grid.cell_size();
    let mut key = grid.key_of(o);
    let step = (
        if d.x >= 0.0 { 1i32 } else { -1 },
        if d.y >= 0.0 { 1i32 } else { -1 },
        if d.z >= 0.0 { 1i32 } else { -1 },
    );
    let safe = |x: f32| if x.abs() < 1e-12 { f32::INFINITY } else { (cell / x).abs() };
    let t_delta = (safe(d.x), safe(d.y), safe(d.z));
    // Distance (in LOCAL ray param) to the first boundary on each axis.
    let bound = |k: i32, o: f32, d: f32| -> f32 {
        if d.abs() < 1e-12 {
            return f32::INFINITY;
        }
        let edge = if d >= 0.0 { (k + 1) as f32 * cell } else { k as f32 * cell };
        ((edge - o) / d).max(0.0)
    };
    let mut t_max = (
        bound(key.0, o.x, d.x),
        bound(key.1, o.y, d.y),
        bound(key.2, o.z, d.z),
    );
    let mut seen_faces: HashSet<u32> = HashSet::new();
    let mut seen_cells: HashSet<(i32, i32, i32)> = HashSet::new();
    let mut fan_scratch: Vec<core_math::half_edge::HalfEdgeId> = Vec::new();
    let mut face_scratch: Vec<core_math::half_edge::FaceId> = Vec::new();
    let mut budget = FACE_TEST_BUDGET;
    let mut best: Option<f32> = None;
    // Local-param position of the hit, for the ray-ordered early-out.
    let mut best_local_t = f32::INFINITY;
    let mut cell_entry = 0.0f32;
    for _ in 0..MAX_CELLS {
        // Ray-ordered early-out: cells are visited front-to-back; a face found in a
        // later cell cannot beat a hit before this cell's entry.
        if best.is_some() && cell_entry > best_local_t {
            break;
        }
        debug_assert!(budget <= FACE_TEST_BUDGET);
        // Visit this cell + its 3×3×3 neighborhood (verts of a ray-crossing face can
        // sit one cell over). Dedup so the streak along the ray stays O(new cells).
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let k = (key.0 + dx, key.1 + dy, key.2 + dz);
                    if !seen_cells.insert(k) {
                        continue;
                    }
                    let Some(bucket) = grid.bucket_at(k) else { continue };
                    for &vi in bucket {
                        let vid = VertexId(vi);
                        if !mesh.is_vert_live(vid) {
                            continue;
                        }
                        // Incident faces: PR-2 CSR when available (no half-edge chase),
                        // else the fan walk (bare-mesh tests).
                        fan_scratch.clear();
                        match csr {
                            Some((off, faces)) if (vi as usize + 1) < off.len() => {
                                let (a, b) =
                                    (off[vi as usize] as usize, off[vi as usize + 1] as usize);
                                for &f in &faces[a..b] {
                                    face_scratch.push(core_math::half_edge::FaceId(f));
                                }
                            }
                            _ => {
                                mesh.vertex_fan_into(vid, &mut fan_scratch);
                                for &he in &fan_scratch {
                                    let fid = mesh.he(he).face;
                                    if fid != core_math::half_edge::NO_FACE {
                                        face_scratch.push(fid);
                                    }
                                }
                            }
                        }
                        for fid in face_scratch.drain(..) {
                            if !seen_faces.insert(fid.0) {
                                continue;
                            }
                            if budget == 0 {
                                // §7.1: abort at the budget — but NEVER throw away a hit
                                // already found. Cells are visited front-to-back, so `best`
                                // is (at worst) a near-front surface point; the old
                                // `return (None, ..)` discarded it, and a dense pinched fold
                                // that exhausted the budget mid-scan turned its whole zone
                                // brush-dead (owner 2026-08-19 "brush no longer has effect").
                                // A genuine no-hit-yet exhaustion stays a MISS (fail closed,
                                // never the full O(F) walk).
                                if best.is_none() && crate::perf_spam_on() {
                                    eprintln!(
                                        "[PICK] dda face budget exhausted with no hit — degenerate/dense zone?"
                                    );
                                }
                                return (best, FACE_TEST_BUDGET);
                            }
                            budget -= 1;
                            if let Some(t) = crate::poly_selection::ray_face_hit(
                                mesh, world, fid, ray_o, ray_d, hidden,
                            ) {
                                if best.map_or(true, |bt| t < bt) {
                                    best = Some(t);
                                    let hw = ray_o + ray_d * t;
                                    best_local_t = (inv.transform_point3(hw) - o).dot(d);
                                }
                            }
                        }
                    }
                }
            }
        }
        // Advance to the next cell boundary (smallest t_max axis).
        if t_max.0 <= t_max.1 && t_max.0 <= t_max.2 {
            cell_entry = t_max.0;
            key.0 += step.0;
            t_max.0 += t_delta.0;
        } else if t_max.1 <= t_max.2 {
            cell_entry = t_max.1;
            key.1 += step.1;
            t_max.1 += t_delta.1;
        } else {
            cell_entry = t_max.2;
            key.2 += step.2;
            t_max.2 += t_delta.2;
        }
    }
    (best, FACE_TEST_BUDGET - budget)
}

// ============================================================================
// PR-4 gates: DDA parity vs the full walk (same shared face test, so any drift
// is a TRAVERSAL bug), budget fail-closed, and the stroke-path grep gate.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use core_math::half_edge::HalfEdgeMesh;

    /// N×N unit-quad grid plane on Y=0 (dense enough to force the DDA path at 80×80).
    fn plane(n: usize) -> HalfEdgeMesh {
        let mut pos = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                pos.push(Vec3::new(i as f32, ((i * 7 + j * 3) % 5) as f32 * 0.02, j as f32));
            }
        }
        let at = |i: usize, j: usize| j * (n + 1) + i;
        let mut faces = Vec::new();
        for j in 0..n {
            for i in 0..n {
                faces.push(vec![at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
        let mut m = HalfEdgeMesh::from_polygons(&pos, &faces);
        m.recompute_normals();
        m
    }

    #[test]
    fn dda_matches_the_full_walk() {
        let mesh = plane(80); // 6400 faces > FACE_TEST_BUDGET → DDA path
        assert!(mesh.faces.len() > FACE_TEST_BUDGET);
        let grid = VertGrid::rebuild(&mesh, super::super::sculpt::grid_cell_for(&mesh));
        let world = glam::Mat4::from_translation(Vec3::new(0.5, 0.0, -0.3));
        let hidden = HashSet::new();
        let mut hits = 0;
        for k in 0..25 {
            let x = 4.0 + (k as f32) * 3.0;
            let z = 6.0 + ((k * 13) % 60) as f32;
            let ro = world.transform_point3(Vec3::new(x, 8.0, z)) + Vec3::new(0.3, 0.0, 0.1);
            let rd = (world.transform_point3(Vec3::new(x, 0.0, z)) - ro).normalize();
            let full = crate::poly_selection::ray_pick_mesh_face(&mesh, world, ro, rd, &hidden);
            let dda = pick_bounded(&mesh, &grid, world, ro, rd, &hidden, None);
            let csr_hit = {
                let (_, cv, cf) = super::super::sculpt::raster_with_corner_map(&mesh);
                let (off, _vc, vf) =
                    super::super::sculpt::build_vert_csr(mesh.vertices.len(), &cv, &cf);
                pick_bounded(&mesh, &grid, world, ro, rd, &hidden, Some((&off, &vf)))
            };
            assert_eq!(
                dda.map(|t| (t * 1e4) as i64),
                csr_hit.map(|t| (t * 1e4) as i64),
                "ray {k}: CSR incident-face path must match the fan path"
            );
            match (full, dda) {
                (Some((tf, _)), Some(td)) => {
                    assert!(
                        (tf - td).abs() < 1e-4,
                        "ray {k}: full t={tf} vs dda t={td}"
                    );
                    hits += 1;
                }
                (None, None) => {}
                (f, d) => panic!("ray {k}: full={f:?} dda={d:?} — traversal missed a face"),
            }
        }
        assert!(hits >= 20, "fixture must actually hit ({hits}/25) or the parity is vacuous");
    }

    #[test]
    fn small_mesh_uses_the_bounded_full_loop_and_hits_giant_ngons() {
        // A raw cube: 6 giant faces whose verts all sit in corner cells — the DDA's
        // documented blind spot. The small-mesh gate must make this correct.
        let pos = vec![
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let faces = vec![
            vec![0usize, 3, 2, 1],
            vec![4, 5, 6, 7],
            vec![0, 1, 5, 4],
            vec![2, 3, 7, 6],
            vec![1, 2, 6, 5],
            vec![3, 0, 4, 7],
        ];
        let mut mesh = HalfEdgeMesh::from_polygons(&pos, &faces);
        mesh.recompute_normals();
        let grid = VertGrid::rebuild(&mesh, super::super::sculpt::grid_cell_for(&mesh));
        let hidden = HashSet::new();
        // Ray through the +Y face CENTER — far from every vertex cell.
        let ro = Vec3::new(0.0, 5.0, 0.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let t = pick_bounded(&mesh, &grid, glam::Mat4::IDENTITY, ro, rd, &hidden, None)
            .expect("center of a giant n-gon must hit");
        assert!((t - 4.0).abs() < 1e-4, "top face is at y=1 → t=4, got {t}");
    }

    #[test]
    fn miss_is_bounded_and_fails_closed() {
        let mesh = plane(80);
        let grid = VertGrid::rebuild(&mesh, super::super::sculpt::grid_cell_for(&mesh));
        let hidden = HashSet::new();
        // Parallel ray far above the plane: crosses thousands of cells, hits nothing.
        // MIN of 3 runs: wall-clock asserts under full-suite contention are the recorded
        // flake class — the minimum is contention-resistant while still failing an O(F)
        // scan (which cannot go fast on ANY run).
        let mut best = u128::MAX;
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            let r = pick_bounded(
                &mesh,
                &grid,
                glam::Mat4::IDENTITY,
                Vec3::new(-50.0, 30.0, 40.0),
                Vec3::new(1.0, 0.0, 0.0),
                &hidden,
                None,
            );
            assert!(r.is_none());
            best = best.min(t0.elapsed().as_millis());
        }
        assert!(
            best < 100,
            "a miss must be bounded (MAX_CELLS + budget), not an O(F) scan — best {best}ms"
        );
    }

    /// PR-10 gate: the PBVH pick ≡ the full walk (same shared face test — parity is
    /// the traversal's burden), on the dense fixture, budget-bounded.
    #[test]
    fn pbvh_pick_matches_the_full_walk() {
        let mesh = plane(80);
        assert!(mesh.faces.len() > FACE_TEST_BUDGET);
        let (_, cv, cf) = super::super::sculpt::raster_with_corner_map(&mesh);
        let (off, _vc, vf) =
            super::super::sculpt::build_vert_csr(mesh.vertices.len(), &cv, &cf);
        let pbvh = super::super::sculpt_pbvh::SculptPbvh::build(&mesh);
        let world = glam::Mat4::from_translation(Vec3::new(0.5, 0.0, -0.3));
        let hidden = HashSet::new();
        let mut hits = 0;
        for k in 0..25 {
            let x = 4.0 + (k as f32) * 3.0;
            let z = 6.0 + ((k * 13) % 60) as f32;
            let ro = world.transform_point3(Vec3::new(x, 8.0, z)) + Vec3::new(0.3, 0.0, 0.1);
            let rd = (world.transform_point3(Vec3::new(x, 0.0, z)) - ro).normalize();
            let full = crate::poly_selection::ray_pick_mesh_face(&mesh, world, ro, rd, &hidden);
            let (tree, spent) =
                pbvh.pick(&mesh, world, ro, rd, &hidden, (&off, &vf), FACE_TEST_BUDGET);
            assert!(spent <= FACE_TEST_BUDGET);
            match (full, tree) {
                (Some((tf, _)), Some(tt)) => {
                    assert!((tf - tt).abs() < 1e-4, "ray {k}: full {tf} vs pbvh {tt}");
                    hits += 1;
                }
                (None, None) => {}
                (f, t) => panic!("ray {k}: full={f:?} pbvh={t:?} — traversal missed"),
            }
        }
        assert!(hits >= 20, "fixture must hit ({hits}/25) or parity is vacuous");
    }

    /// PR-8 / G1 (§20): pick work must NOT scale with F. Two planes, SAME world extent,
    /// 10× the face count — the DDA crosses the same cells, so its FACE-TEST COUNT (a
    /// structural property, immune to machine load) must stay within 2×.
    #[test]
    fn g1_pick_face_tests_do_not_scale_with_f() {
        let hidden = HashSet::new();
        let mk = |n: usize| {
            // Unit extent regardless of n: spacing = 1/n.
            let mut pos = Vec::new();
            for j in 0..=n {
                for i in 0..=n {
                    pos.push(Vec3::new(
                        i as f32 * 40.0 / n as f32,
                        ((i * 7 + j * 3) % 5) as f32 * 0.01,
                        j as f32 * 40.0 / n as f32,
                    ));
                }
            }
            let at = |i: usize, j: usize| j * (n + 1) + i;
            let mut faces = Vec::new();
            for j in 0..n {
                for i in 0..n {
                    faces.push(vec![at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
            let mut m = HalfEdgeMesh::from_polygons(&pos, &faces);
            m.recompute_normals();
            let g = VertGrid::rebuild(&m, super::super::sculpt::grid_cell_for(&m));
            (m, g)
        };
        let (m1, g1) = mk(80); // 6,400 faces  — DDA path
        let (m2, g2) = mk(250); // 62,500 faces — ~10× F, same 40-unit extent
        assert!(m1.faces.len() > FACE_TEST_BUDGET && m2.faces.len() > 9 * m1.faces.len());
        // The CPU fallback's CONTRACT (§7.1): tests ∝ verts in the crossed-cell
        // corridor (LOCAL density), hard-capped at FACE_TEST_BUDGET, and always ≪ F —
        // never the O(F) walk. Strict F-INDEPENDENCE (<2× across densities) is the GPU
        // crop's G1 gate (PR-4b); asserting it here mis-states the plan — a denser
        // mesh legitimately has more incident faces per crossed cell.
        let mut hits = 0;
        for k in 0..10 {
            let x = 4.0 + k as f32 * 3.5;
            let ro = Vec3::new(x, 9.0, 6.0 + k as f32 * 3.0);
            let rd = (Vec3::new(x, 0.0, 6.0 + k as f32 * 3.0) - ro).normalize();
            let (h1, c1) = pick_bounded_counted(&m1, &g1, glam::Mat4::IDENTITY, ro, rd, &hidden, None);
            let (h2, c2) = pick_bounded_counted(&m2, &g2, glam::Mat4::IDENTITY, ro, rd, &hidden, None);
            if h1.is_none() || h2.is_none() {
                continue;
            }
            hits += 1;
            assert!(c1 <= FACE_TEST_BUDGET && c2 <= FACE_TEST_BUDGET, "budget cap");
            assert!(
                c1 * 10 <= m1.faces.len() && c2 * 10 <= m2.faces.len(),
                "ray {k}: tests must be ≪ F (c1={c1}/{}, c2={c2}/{}) — an O(F) walk crept in",
                m1.faces.len(),
                m2.faces.len(),
            );
        }
        assert!(hits >= 8, "fixture must hit ({hits}/10) or the gate is vacuous");
    }

    /// G2 negative (§7.2): the per-stamp full corner scan stays deleted — ONE display
    /// writer. Any `for (ci, &vid) in w.corner_vert` fork fails here, not in review.
    #[test]
    fn no_second_corner_scan_in_sculpt() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/poly/sculpt.rs");
        let src = std::fs::read_to_string(&path).unwrap();
        for (ln, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(
                !code.contains(".corner_vert.iter().enumerate()"),
                "sculpt.rs:{}: a second corner scan — write_display_corners is the ONE writer",
                ln + 1
            );
        }
    }

    /// Grep gate (§7.1 negative): the stroke path must never call `ray_pick_mesh_face`.
    /// Source-scan of the stroke files — a re-introduction fails this test, not a review.
    #[test]
    fn stroke_path_never_calls_ray_pick_mesh_face() {
        for f in ["src/poly/sculpt.rs", "src/input_sculpt/stroke.rs"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(f);
            let src = std::fs::read_to_string(&path).unwrap();
            for (ln, line) in src.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                assert!(
                    !code.contains("ray_pick_mesh_face("),
                    "{f}:{} calls ray_pick_mesh_face — BANNED on the stroke path (PR-4)",
                    ln + 1
                );
            }
        }
    }
}
