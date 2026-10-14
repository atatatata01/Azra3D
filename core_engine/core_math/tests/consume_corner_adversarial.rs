//! ADVERSARIAL PLACEMENT AUDIT — corner-turning scenarios for `consume_fuse_bevel`
//! (bevel_absorb.rs). These are selection shapes the unit tests do NOT cover as live
//! assertions (the in-file corner tests are `#[ignore]`d): L-chains turning a cube
//! corner, the full top-perimeter ring, a U-chain, two simultaneous L-chains, and a
//! width larger than the face.
//!
//! Contract under audit (fail-closed): `consume_fuse_bevel` may REFUSE (None) any of
//! these — that is the accepted fallback path — but it must NEVER return Some(mesh)
//! that fails validate()/is_manifold(), or that contains a vertex which is neither
//! untouched original lattice nor inside the fillet band around a selected edge line
//! (the stray-vertex pattern from `width_sweep_never_destroys_the_mesh`).
//!
//! Observed behaviour is PINNED per scenario so a silent flip (refusal → unsound
//! fuse, or fuse → refusal) fails loudly and gets re-audited.

use core_math::bevel_absorb::consume_fuse_bevel;
use core_math::bevel_op::BevelParams;
use core_math::half_edge::{HalfEdgeMesh, HalfEdgeId, VertexId, NO_TWIN};
use glam::Vec3;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Fixtures / helpers
// ---------------------------------------------------------------------------

/// Cube subdivided n×n on every side, [-1,1]^3, welded lattice, outward winding.
/// (Same construction as the bevel_absorb unit-test fixture.)
fn subdivided_cube(n: usize) -> HalfEdgeMesh {
    let mut map: HashMap<(usize, usize, usize), usize> = HashMap::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let c = |i: usize| -1.0 + 2.0 * i as f32 / n as f32;
    let mut faces: Vec<Vec<usize>> = Vec::new();
    {
        let mut vid = |ijk: (usize, usize, usize)| -> usize {
            *map.entry(ijk).or_insert_with(|| {
                positions.push(Vec3::new(c(ijk.0), c(ijk.1), c(ijk.2)));
                positions.len() - 1
            })
        };
        let sides: [Box<dyn Fn(usize, usize) -> (usize, usize, usize)>; 6] = [
            Box::new(move |u, v| (u, n, v)), // top    y=+1
            Box::new(move |u, v| (v, 0, u)), // bottom y=-1
            Box::new(move |u, v| (u, v, 0)), // front  z=-1
            Box::new(move |u, v| (v, u, n)), // back   z=+1
            Box::new(move |u, v| (0, u, v)), // left   x=-1
            Box::new(move |u, v| (n, v, u)), // right  x=+1
        ];
        for side in &sides {
            for u in 0..n {
                for v in 0..n {
                    faces.push(vec![
                        vid(side(u, v)),
                        vid(side(u, v + 1)),
                        vid(side(u + 1, v + 1)),
                        vid(side(u + 1, v)),
                    ]);
                }
            }
        }
    }
    // Orient outward via signed volume.
    let mut vol6 = 0.0f32;
    for f in &faces {
        for k in 1..f.len() - 1 {
            let (a, b, cc) = (positions[f[0]], positions[f[k]], positions[f[k + 1]]);
            vol6 += a.dot(b.cross(cc));
        }
    }
    if vol6 < 0.0 {
        for f in &mut faces {
            f.reverse();
        }
    }
    let m = HalfEdgeMesh::from_polygons(&positions, &faces);
    m.validate().expect("subdivided cube valid");
    assert!(m.is_manifold().is_ok(), "subdivided cube watertight");
    m
}

/// Undirected selection of every live edge whose BOTH endpoints satisfy `pred`.
fn select_edges(mesh: &HalfEdgeMesh, pred: impl Fn(Vec3, Vec3) -> bool) -> HashSet<u32> {
    let mut sel = HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !mesh.is_edge_live(he) {
            continue;
        }
        let t = mesh.he(he).twin;
        if t != NO_TWIN && i > t.0 {
            continue;
        }
        let a = mesh.vert(mesh.he_source(he)).position;
        let b = mesh.vert(mesh.he(he).vertex).position;
        if pred(a, b) {
            sel.insert(i);
        }
    }
    sel
}

/// A beveled cube-edge LINE (point + unit direction) for the band distance.
struct EdgeLine {
    p: Vec3,
    d: Vec3,
}

fn dist_to_line(p: Vec3, l: &EdgeLine) -> f32 {
    let v = p - l.p;
    (v - l.d * v.dot(l.d)).length()
}

/// The stray-vertex sanity check, copied from `width_sweep_never_destroys_the_mesh`:
/// every live vertex is either untouched original lattice (all coords on the 2/n grid
/// AND on the cube surface) or inside the fillet band (within ~width of a selected
/// edge line). Plus validate() + is_manifold().
fn check_sane(out: &HalfEdgeMesh, lines: &[EdgeLine], width: f32, n: usize, label: &str) {
    out.validate()
        .unwrap_or_else(|e| panic!("{label}: fused mesh invalid: {e}"));
    assert!(out.is_manifold().is_ok(), "{label}: fused mesh not watertight");
    let step = 2.0 / n as f32;
    let lattice = |x: f32| ((x / step).round() * step - x).abs() < 1e-3;
    let on_surface =
        |p: Vec3| p.x.abs() > 1.0 - 1e-3 || p.y.abs() > 1.0 - 1e-3 || p.z.abs() > 1.0 - 1e-3;
    let mut strays = 0usize;
    for vi in 0..out.vertices.len() as u32 {
        let v = VertexId(vi);
        if !out.is_vert_live(v) {
            continue;
        }
        let p = out.vert(v).position;
        let is_lattice = lattice(p.x) && lattice(p.y) && lattice(p.z) && on_surface(p);
        let band = lines
            .iter()
            .map(|l| dist_to_line(p, l))
            .fold(f32::MAX, f32::min);
        let in_band = band <= width * 1.6 + 1e-3;
        if !is_lattice && !in_band {
            strays += 1;
            if strays <= 8 {
                eprintln!(
                    "{label}: STRAY vert ({:.4},{:.4},{:.4}) — neither lattice nor band (d={band:.4})",
                    p.x, p.y, p.z
                );
            }
        }
    }
    assert_eq!(strays, 0, "{label}: {strays} strayed vertices");
}

fn line(p: Vec3, d: Vec3) -> EdgeLine {
    EdgeLine {
        p,
        d: d.normalize(),
    }
}

fn params(width: f32) -> BevelParams {
    BevelParams {
        width,
        segments: 3,
        shape: 0.5,
        clamp: true,
    }
}

/// Run one scenario: if the fuse returns Some, it must be sane; either way report.
/// Returns whether it fused.
fn run_scenario(
    mesh: &HalfEdgeMesh,
    sel: &HashSet<u32>,
    width: f32,
    lines: &[EdgeLine],
    n: usize,
    label: &str,
) -> bool {
    match consume_fuse_bevel(mesh, sel, &params(width)) {
        Some(out) => {
            check_sane(&out, lines, width, n, label);
            eprintln!(
                "{label}: FUSED v={} f={} (orig v={})",
                out.vertex_count(),
                out.face_count(),
                mesh.vertex_count()
            );
            true
        }
        None => {
            eprintln!("{label}: REFUSED (fail-closed fallback)");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// (a) L-chain on the n=6 lattice (rows of 1/3): top-front + top-right edges
//     turning at cube corner (1,1,-1). Narrow (0.15, inside the first row) and
//     wide (0.9, eats rows).
// ---------------------------------------------------------------------------
#[test]
fn a_l_chain_n6_narrow_and_wide() {
    let n = 6usize;
    let mesh = subdivided_cube(n);
    let on = |p: Vec3, q: Vec3| {
        let top = (p.y - 1.0).abs() < 1e-4 && (q.y - 1.0).abs() < 1e-4;
        let front = (p.z + 1.0).abs() < 1e-4 && (q.z + 1.0).abs() < 1e-4;
        let right = (p.x - 1.0).abs() < 1e-4 && (q.x - 1.0).abs() < 1e-4;
        top && (front || right)
    };
    let sel = select_edges(&mesh, on);
    assert_eq!(sel.len(), 2 * n, "L-chain of two full top edges");
    let lines = [
        line(Vec3::new(0.0, 1.0, -1.0), Vec3::X), // top-front
        line(Vec3::new(1.0, 1.0, 0.0), Vec3::Z),  // top-right
    ];
    let fused_narrow = run_scenario(&mesh, &sel, 0.15, &lines, n, "a/L-chain n6 w=0.15");
    let fused_wide = run_scenario(&mesh, &sel, 0.9, &lines, n, "a/L-chain n6 w=0.9");
    // PINNED (audit 2026-08-08): the fuse gate refuses L-chains — the corner quad
    // borders both selected edges (classify_side's is_selected(next/prev) / repeated
    // ring-face gates). Corner-turning is NOT implemented in consume_fuse_bevel;
    // refusal is the documented fail-closed fallback (absorb+bevel takes over).
    // If either flips to fused, the sanity checks above already ran — re-audit
    // placement and update this pin deliberately.
    assert!(!fused_narrow, "a: narrow L-chain unexpectedly fused — re-audit corner placement");
    assert!(!fused_wide, "a: wide L-chain unexpectedly fused — re-audit corner placement");
}

// ---------------------------------------------------------------------------
// (b) FULL top-perimeter ring on n=8: closed loop turning at all four top
//     corners; band around all four edges, corners mitered. w=0.3 and w=1.0.
// ---------------------------------------------------------------------------
#[test]
fn b_full_top_perimeter_ring_n8() {
    let n = 8usize;
    let mesh = subdivided_cube(n);
    let on_border = |p: Vec3| {
        (p.y - 1.0).abs() < 1e-4
            && ((p.x.abs() - 1.0).abs() < 1e-4 || (p.z.abs() - 1.0).abs() < 1e-4)
    };
    let sel = select_edges(&mesh, |a, b| on_border(a) && on_border(b));
    assert_eq!(sel.len(), 4 * n, "full perimeter ring selected");
    let lines = [
        line(Vec3::new(0.0, 1.0, -1.0), Vec3::X),
        line(Vec3::new(0.0, 1.0, 1.0), Vec3::X),
        line(Vec3::new(-1.0, 1.0, 0.0), Vec3::Z),
        line(Vec3::new(1.0, 1.0, 0.0), Vec3::Z),
    ];
    let fused_03 = run_scenario(&mesh, &sel, 0.3, &lines, n, "b/perimeter n8 w=0.3");
    let fused_10 = run_scenario(&mesh, &sel, 1.0, &lines, n, "b/perimeter n8 w=1.0");
    // PINNED: refused — side A (the top face) is an n×n quad grid, not one cap n-gon,
    // and each corner quad repeats along the ring (seen-face gate). Fail-closed.
    // PIN FLIPPED 2026-08-08 (corner turning landed): w=1.0 is lattice-aligned and FUSES
    // (check_sane above already vetted the mesh); w=0.3 is unaligned — v1 scope refuses it
    // (falls back to absorb+bevel).
    assert!(!fused_03, "b: unaligned w=0.3 must refuse (v1 lattice-aligned corner scope)");
    assert!(fused_10, "b: aligned w=1.0 perimeter ring must fuse (corner turning landed)");
}

// ---------------------------------------------------------------------------
// (c) U-chain: three top edges (front, right, back) — an open chain turning at
//     TWO corners. n=8, w=0.5.
// ---------------------------------------------------------------------------
#[test]
fn c_u_chain_three_top_edges_n8() {
    let n = 8usize;
    let mesh = subdivided_cube(n);
    let on = |p: Vec3, q: Vec3| {
        let top = (p.y - 1.0).abs() < 1e-4 && (q.y - 1.0).abs() < 1e-4;
        let front = (p.z + 1.0).abs() < 1e-4 && (q.z + 1.0).abs() < 1e-4;
        let right = (p.x - 1.0).abs() < 1e-4 && (q.x - 1.0).abs() < 1e-4;
        let back = (p.z - 1.0).abs() < 1e-4 && (q.z - 1.0).abs() < 1e-4;
        top && (front || right || back)
    };
    let sel = select_edges(&mesh, on);
    assert_eq!(sel.len(), 3 * n, "U-chain of three full top edges");
    let lines = [
        line(Vec3::new(0.0, 1.0, -1.0), Vec3::X),
        line(Vec3::new(1.0, 1.0, 0.0), Vec3::Z),
        line(Vec3::new(0.0, 1.0, 1.0), Vec3::X),
    ];
    let fused = run_scenario(&mesh, &sel, 0.5, &lines, n, "c/U-chain n8 w=0.5");
    // PINNED: refused — both corner quads border two selected edges each. Fail-closed.
    // PIN FLIPPED 2026-08-08 (corner turning landed): w=0.5 is lattice-aligned and FUSES.
    assert!(fused, "c: aligned U-chain must fuse (corner turning landed)");
}

// ---------------------------------------------------------------------------
// (d) TWO L-chains on OPPOSITE top corners simultaneously. To keep them
//     disjoint (full edges would close into the perimeter ring), each chain is
//     the HALF of two adjacent top edges nearest its corner:
//       L1 around (1,1,-1):  x∈[0,1] on (y=1,z=-1)  +  z∈[-1,0] on (y=1,x=1)
//       L2 around (-1,1,1):  x∈[-1,0] on (y=1,z=1)  +  z∈[0,1]  on (y=1,x=-1)
//     n=8, w=0.4. Chain ends sit mid-edge (lattice midpoints).
// ---------------------------------------------------------------------------
#[test]
fn d_two_opposite_l_chains_n8() {
    let n = 8usize;
    let mesh = subdivided_cube(n);
    let on = |p: Vec3, q: Vec3| {
        let top = (p.y - 1.0).abs() < 1e-4 && (q.y - 1.0).abs() < 1e-4;
        if !top {
            return false;
        }
        let seg = |lo: f32, hi: f32, a: f32, b: f32| a >= lo - 1e-4 && a <= hi + 1e-4 && b >= lo - 1e-4 && b <= hi + 1e-4;
        // L1
        let l1a = (p.z + 1.0).abs() < 1e-4 && (q.z + 1.0).abs() < 1e-4 && seg(0.0, 1.0, p.x, q.x);
        let l1b = (p.x - 1.0).abs() < 1e-4 && (q.x - 1.0).abs() < 1e-4 && seg(-1.0, 0.0, p.z, q.z);
        // L2
        let l2a = (p.z - 1.0).abs() < 1e-4 && (q.z - 1.0).abs() < 1e-4 && seg(-1.0, 0.0, p.x, q.x);
        let l2b = (p.x + 1.0).abs() < 1e-4 && (q.x + 1.0).abs() < 1e-4 && seg(0.0, 1.0, p.z, q.z);
        l1a || l1b || l2a || l2b
    };
    let sel = select_edges(&mesh, on);
    assert_eq!(sel.len(), 2 * n, "two half-edge L-chains (n/2 segments × 4 legs)");
    let lines = [
        line(Vec3::new(0.0, 1.0, -1.0), Vec3::X),
        line(Vec3::new(1.0, 1.0, 0.0), Vec3::Z),
        line(Vec3::new(0.0, 1.0, 1.0), Vec3::X),
        line(Vec3::new(-1.0, 1.0, 0.0), Vec3::Z),
    ];
    let fused = run_scenario(&mesh, &sel, 0.4, &lines, n, "d/two L-chains n8 w=0.4");
    // PINNED: refused — same corner-quad class as (a), twice over; additionally the
    // mid-face chain ends border >1 unremoved face (end-vertex isolation gate).
    assert!(!fused, "d: two L-chains unexpectedly fused — re-audit corner placement");
}

// ---------------------------------------------------------------------------
// (e) Degenerate: width larger than the face (w=1.9 on a 2-unit cube), single
//     straight top-front chain on n=8 — the carve would have to consume nearly
//     the entire top and front faces, colliding with the opposite cube edges.
//     None is fine; Some MUST be sane.
// ---------------------------------------------------------------------------
#[test]
fn e_width_larger_than_face() {
    let n = 8usize;
    let mesh = subdivided_cube(n);
    let on = |p: Vec3, q: Vec3| {
        (p.y - 1.0).abs() < 1e-4
            && (q.y - 1.0).abs() < 1e-4
            && (p.z + 1.0).abs() < 1e-4
            && (q.z + 1.0).abs() < 1e-4
    };
    let sel = select_edges(&mesh, on);
    assert_eq!(sel.len(), n, "single top-front chain");
    let width = 1.9f32;
    let lines = [line(Vec3::new(0.0, 1.0, -1.0), Vec3::X)];
    // PINNED (audit 2026-08-08): this FUSES — the carve eats 7 of 8 rows on both the
    // top and front faces and cuts the boundary inside the last row (285v/268f of
    // 386v). NOTE: at w=1.9 the band check in check_sane is vacuous (1.6·w spans the
    // whole cube), so placement is pinned STRUCTURALLY below instead.
    let out = consume_fuse_bevel(&mesh, &sel, &params(width))
        .expect("e: w=1.9 refused — pin flipped, re-audit degenerate-width behaviour");
    check_sane(&out, &lines, width, n, "e/degenerate n8 w=1.9");
    eprintln!(
        "e/degenerate n8 w=1.9: FUSED v={} f={} (orig v={})",
        out.vertex_count(),
        out.face_count(),
        mesh.vertex_count()
    );
    let live_pos = || {
        (0..out.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| out.is_vert_live(v))
            .map(|v| out.vert(v).position)
    };
    // (1) Nothing leaves the original cube volume.
    for p in live_pos() {
        assert!(
            p.x.abs() <= 1.0 + 1e-3 && p.y.abs() <= 1.0 + 1e-3 && p.z.abs() <= 1.0 + 1e-3,
            "e: vertex escaped the cube: ({:.4},{:.4},{:.4})",
            p.x,
            p.y,
            p.z
        );
    }
    // (2) Strip boundaries: ONE clean loop per side at exactly `width` from the edge —
    //     n+1 columns on the top face (z = -1+w) and on the front face (y = 1-w).
    let top = live_pos()
        .filter(|p| (p.y - 1.0).abs() < 1e-3 && (p.z - (-1.0 + width)).abs() < 2e-3)
        .count();
    let wall = live_pos()
        .filter(|p| (p.z + 1.0).abs() < 1e-3 && (p.y - (1.0 - width)).abs() < 2e-3)
        .count();
    assert_eq!(top, n + 1, "e: top boundary loop malformed ({top} verts)");
    assert_eq!(wall, n + 1, "e: wall boundary loop malformed ({wall} verts)");
    // (3) The untouched faces keep their full lattice: bottom face (y=-1) = (n+1)².
    let bottom = live_pos().filter(|p| (p.y + 1.0).abs() < 1e-3).count();
    assert_eq!(bottom, (n + 1) * (n + 1), "e: bottom face lattice damaged");
    // (4) The sharp edge line is fully consumed.
    let on_edge = live_pos()
        .filter(|p| (p.y - 1.0).abs() < 1e-3 && (p.z + 1.0).abs() < 1e-3)
        .count();
    assert_eq!(on_edge, 0, "e: sharp edge not fully consumed");
}
