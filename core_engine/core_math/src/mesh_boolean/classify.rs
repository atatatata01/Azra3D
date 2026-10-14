//! Robust winding-number classifier (mainline legacy path).
//!
//! Pure relocation of the robust boolean path from mesh_boolean.
//! BSP (csg.js invert/clip) remains the fallback; this path classifies
//! fragments by generalized winding number of the other solid.

use glam::Vec3;
use std::collections::BTreeMap;
use crate::half_edge::HalfEdgeMesh;
use super::{
    mesh_to_polygons, polygons_to_mesh, split_polygon, BoolOp, PLANE_EPS,
    Plane, Polygon, SRC_A, SRC_B, COPLANAR, FRONT, BACK, SPANNING, MAX_BSP_POLYS,
};

// ============================================================
// Robust classifier (winding-number) — the mainline path
// ============================================================
//
// The csg.js BSP (below, kept as a fallback) decides which faces survive with an invert/clip dance
// whose coplanar tie-break is order-dependent and non-robust: on sequential / coplanar cuts it emits
// spurious flipped faces FAR from the cut and coincident face+flip pairs (the "2nd boolean adds messy
// segmentation / does nothing" class — proven with the sequential oracle). This path never trusts the
// BSP for classification. It (1) splits every A-face by B's planes and vice-versa so no fragment
// straddles the other surface, then (2) classifies each fragment by the GENERALIZED WINDING NUMBER of
// the OTHER solid — geometric ground truth that cannot produce a spurious or flipped face. A fragment
// that lies exactly ON the other surface (winding ≈ ½, detected by sampling just off both sides along
// the face normal) is the only ambiguous case, resolved by op + normal parity (Blender's coplanar
// table). This is how libigl / Cork / Manifold classify; it is robust where the BSP is not.

/// Generalized (solid-angle) winding number of `p` w.r.t. a closed, outward-wound triangle soup.
/// ≈ +1 strictly inside, ≈ 0 strictly outside, ≈ +½ exactly on the surface. Van Oosterom–Strackee
/// signed-solid-angle per triangle, summed and divided by 4π. Robust near (but not on) the surface.
fn winding_number(tris: &[[Vec3; 3]], p: Vec3) -> f32 {
    let mut omega = 0.0f64;
    for t in tris {
        let a = (t[0] - p).as_dvec3();
        let b = (t[1] - p).as_dvec3();
        let c = (t[2] - p).as_dvec3();
        let (la, lb, lc) = (a.length(), b.length(), c.length());
        if la < 1e-18 || lb < 1e-18 || lc < 1e-18 { continue; }
        let num = a.dot(b.cross(c));
        let den = la * lb * lc + a.dot(b) * lc + b.dot(c) * la + c.dot(a) * lb;
        omega += num.atan2(den); // = ½ × signed solid angle
    }
    // Σ atan2 = ½ Σ Ω ; winding = ΣΩ / 4π = (2·Σatan2)/4π = Σatan2 / 2π.
    (omega / (std::f64::consts::PI)) as f32 * 0.5
}

/// Extract the triangle soup (positions) from a fan-triangulated polygon list.
fn tri_soup(polys: &[Polygon]) -> Vec<[Vec3; 3]> {
    polys.iter().filter(|p| p.verts.len() == 3)
        .map(|p| [p.verts[0], p.verts[1], p.verts[2]]).collect()
}

/// Split every polygon by every plane, keeping ALL fragments (front + back + coplanar), so no fragment
/// straddles any plane. Provenance is preserved (split children inherit; coplanar/front/back pass through).
fn split_by_planes(polys: Vec<Polygon>, planes: &[Plane]) -> Vec<Polygon> {
    let mut cur = polys;
    for plane in planes {
        let mut next: Vec<Polygon> = Vec::with_capacity(cur.len());
        for poly in cur {
            let (mut cf, mut cb, mut fr, mut bk) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            split_polygon(plane, poly, &mut cf, &mut cb, &mut fr, &mut bk);
            next.append(&mut cf); next.append(&mut cb);
            next.append(&mut fr); next.append(&mut bk);
        }
        cur = next;
    }
    cur
}

fn poly_centroid(p: &Polygon) -> Vec3 {
    p.verts.iter().copied().sum::<Vec3>() / p.verts.len() as f32
}

/// Winding-probe offset for one fragment: FEATURE-relative, not scene-relative.
/// A scene-diagonal eps (`diag*5e-4`) is larger than a dense bevel strip on a long
/// object (owner sliver bug 2026-07-23, POLY-BOOL-BEVEL): the ± probes then land
/// across NEIGHBOURING strips and whole bands misclassify inside/outside/coplanar.
/// Scale by the fragment's own shortest edge (its thin dimension), clamped to
/// [floor, cap] — cap = the old scene eps, so large flat walls behave exactly as
/// before; only fragments finer than the scene eps get a proportionally finer probe.
fn probe_eps_for(p: &Polygon, floor_eps: f32, cap_eps: f32) -> f32 {
    let n = p.verts.len();
    let mut min_e2 = f32::MAX;
    for i in 0..n {
        let e2 = (p.verts[(i + 1) % n] - p.verts[i]).length_squared();
        if e2 > 0.0 && e2 < min_e2 { min_e2 = e2; }
    }
    if !min_e2.is_finite() { return cap_eps; }
    (0.25 * min_e2.sqrt()).clamp(floor_eps, cap_eps)
}

fn poly_aabb(p: &Polygon) -> (Vec3, Vec3) {
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    for &v in &p.verts { mn = mn.min(v); mx = mx.max(v); }
    (mn, mx)
}

fn aabb_overlap(a: (Vec3, Vec3), b: (Vec3, Vec3), m: f32) -> bool {
    a.0.x <= b.1.x + m && a.1.x >= b.0.x - m
        && a.0.y <= b.1.y + m && a.1.y >= b.0.y - m
        && a.0.z <= b.1.z + m && a.1.z >= b.0.z - m
}

/// Split each polygon in `to_split` ONLY by planes of `cutters` whose AABB overlaps it, AND
/// only insert spanning-edge intersection verts that lie near the finite cutter polygon.
///
/// Infinite plane cuts of a cube top by a cylinder facet that only intersects the TOP INTERIOR
/// used to pin extra verts on the TOP OUTER edges (→ side-wall T-junctions via repair) — Blender
/// Exact does not; that was the cube−cyl 60v vs 40v gap.
fn split_targeted(to_split: &[Polygon], cutters: &[Polygon], margin: f32) -> Vec<Polygon> {
    let caabb: Vec<(Vec3, Vec3)> = cutters.iter().map(poly_aabb).collect();
    let mut out: Vec<Polygon> = Vec::with_capacity(to_split.len());
    for poly in to_split {
        let pab = poly_aabb(poly);
        // Group cutters by quantized plane, keeping EVERY coplanar poly in the group.
        // The finite-support filter must test against the UNION of the group's support:
        // input faces are ear-clipped (mesh_to_polygons), so one logical wall is several
        // triangles on one plane — a single representative (the old `or_insert`) made the
        // support test miss chords near the OTHER triangles, vetoing required splits
        // (POLY-BOOL-BEVEL: the unsplit fragment then overlapped or, with the old
        // per-edge veto, whole walls were chopped without seams and silently dropped).
        let mut plane_cutters: BTreeMap<(i64, i64, i64, i64), (Plane, Vec<usize>)> = BTreeMap::new();
        for (i, cp) in cutters.iter().enumerate() {
            if !aabb_overlap(pab, caabb[i], margin) {
                continue;
            }
            let mut n = cp.plane.normal;
            let mut w = cp.plane.w;
            let lead = if n.x.abs() > 1e-6 { n.x } else if n.y.abs() > 1e-6 { n.y } else { n.z };
            if lead < 0.0 {
                n = -n;
                w = -w;
            }
            let q = |v: f32| (v / 1e-4).round() as i64;
            let key = (q(n.x), q(n.y), q(n.z), q(w));
            plane_cutters
                .entry(key)
                .or_insert_with(|| (Plane { normal: n, w }, Vec::new()))
                .1
                .push(i);
        }
        let mut cur = vec![poly.clone()];
        for (_, (plane, group)) in plane_cutters {
            let group_tris: Vec<[Vec3; 3]> = group
                .iter()
                .flat_map(|&ci| fan_tris(&cutters[ci]))
                .collect();
            let mut next: Vec<Polygon> = Vec::with_capacity(cur.len());
            for p in cur {
                split_polygon_finite(&plane, p, &group_tris, margin, &mut next);
            }
            cur = next;
        }
        out.extend(cur);
    }
    out
}

/// Like `split_polygon`, but spanning-edge intersection points are kept only when they lie
/// within `margin` of the finite `cutter` polygon (projected support). Far plane hits that only
/// exist because the plane is infinite are dropped — the edge stays unsplit (both endpoints
/// classified by their side; if they differ and the hit is far, the edge is assigned to the
/// side of its midpoint).
fn split_polygon_finite(
    plane: &Plane,
    poly: Polygon,
    cutter_tris: &[[Vec3; 3]],
    margin: f32,
    out: &mut Vec<Polygon>,
) {
    let mut polygon_type: u8 = 0;
    let mut types: Vec<u8> = Vec::with_capacity(poly.verts.len());
    for v in &poly.verts {
        let t = plane.normal.dot(*v) - plane.w;
        let ty = if t < -PLANE_EPS {
            BACK
        } else if t > PLANE_EPS {
            FRONT
        } else {
            COPLANAR
        };
        polygon_type |= ty;
        types.push(ty);
    }

    match polygon_type {
        COPLANAR => {
            if plane.normal.dot(poly.plane.normal) > 0.0 {
                out.push(poly);
            } else {
                // Flip-side coplanar: still keep (classification happens later).
                out.push(poly);
            }
        }
        FRONT | BACK => out.push(poly),
        _ => {
            let n = poly.verts.len();
            let m2 = margin * margin;

            // Pass 1: all spanning-edge hit points (edge i → i+1).
            let mut hits: Vec<(usize, Vec3)> = Vec::new();
            for i in 0..n {
                let j = (i + 1) % n;
                if (types[i] | types[j]) == SPANNING {
                    let (vi, vj) = (poly.verts[i], poly.verts[j]);
                    let denom = plane.normal.dot(vj - vi);
                    if denom.abs() > f32::MIN_POSITIVE {
                        let t = (plane.w - plane.normal.dot(vi)) / denom;
                        hits.push((i, vi.lerp(vj, t.clamp(0.0, 1.0))));
                    }
                }
            }

            // Finite support, decided PER POLYGON on the cut CHORD, not per edge hit.
            // The old per-edge veto skipped inserting a "far" hit but still chopped the
            // polygon — when a side ended with <3 verts, Polygon::new returned None and
            // that area was SILENTLY DROPPED (the POLY-BOOL-BEVEL sliver bug: a long
            // wall whose two crossing edges both pass outside the cutter's support lost
            // the ENTIRE wall). The cut matters iff the cutter's body reaches the chord
            // between hit points: then split fully (insert every hit — never an open
            // polygon); otherwise keep the polygon WHOLE (infinite-plane hit only).
            let chord_near = hits.len() >= 2
                && (cutter_tris.is_empty() || {
                    let chord_pairs = hits.len() / 2;
                    (0..chord_pairs).any(|k| {
                        let a = hits[2 * k].1;
                        let b = hits[2 * k + 1].1;
                        cutter_tris.iter().any(|tri| {
                            seg_triangle_dist_sq(a, b, tri[0], tri[1], tri[2]) <= m2
                        })
                    })
                });
            if !chord_near {
                out.push(poly);
                return;
            }

            let mut f: Vec<Vec3> = Vec::with_capacity(n + 1);
            let mut b: Vec<Vec3> = Vec::with_capacity(n + 1);
            for i in 0..n {
                let ti = types[i];
                let vi = poly.verts[i];
                if ti != BACK {
                    f.push(vi);
                }
                if ti != FRONT {
                    b.push(vi);
                }
                if let Some(&(_, v)) = hits.iter().find(|(e, _)| *e == i) {
                    f.push(v);
                    b.push(v);
                }
            }
            if let Some(p) = Polygon::new(f) {
                out.push(p.inherit(&poly));
            }
            if let Some(p) = Polygon::new(b) {
                out.push(p.inherit(&poly));
            }
        }
    }
}

/// Exact squared distance between segment (a,b) and triangle (t0,t1,t2):
/// 0 when the segment pierces the triangle, else the min over endpoint→triangle
/// and segment→each-triangle-edge distances.
fn seg_triangle_dist_sq(a: Vec3, b: Vec3, t0: Vec3, t1: Vec3, t2: Vec3) -> f32 {
    let n = (t1 - t0).cross(t2 - t0);
    let da = n.dot(a - t0);
    let db = n.dot(b - t0);
    if da * db < 0.0 {
        let t = da / (da - db);
        let p = a + (b - a) * t;
        if point_triangle_dist_sq(p, t0, t1, t2) <= 1e-10 {
            return 0.0;
        }
    }
    let mut d = point_triangle_dist_sq(a, t0, t1, t2).min(point_triangle_dist_sq(b, t0, t1, t2));
    for (ea, eb) in [(t0, t1), (t1, t2), (t2, t0)] {
        d = d.min(seg_seg_dist_sq(a, b, ea, eb));
    }
    d
}

/// Squared distance between segments (p1,q1) and (p2,q2) — standard clamped
/// closest-point formulation (Ericson, Real-Time Collision Detection §5.1.9).
fn seg_seg_dist_sq(p1: Vec3, q1: Vec3, p2: Vec3, q2: Vec3) -> f32 {
    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let a = d1.length_squared();
    let e = d2.length_squared();
    let f = d2.dot(r);
    let (mut s, mut t);
    if a <= 1e-20 && e <= 1e-20 {
        return r.length_squared();
    }
    if a <= 1e-20 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-20 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            s = if denom > 1e-20 { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
            t = (b * s + f) / e;
            if t < 0.0 {
                t = 0.0;
                s = (-c / a).clamp(0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = ((b - c) / a).clamp(0.0, 1.0);
            }
        }
    }
    (p1 + d1 * s - (p2 + d2 * t)).length_squared()
}

fn fan_tris(poly: &Polygon) -> Vec<[Vec3; 3]> {
    let v = &poly.verts;
    if v.len() < 3 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(v.len().saturating_sub(2));
    for i in 1..v.len() - 1 {
        out.push([v[0], v[i], v[i + 1]]);
    }
    out
}

fn point_segment_dist_sq(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq < 1e-20 {
        return (p - a).length_squared();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (a + ab * t - p).length_squared()
}

/// Exact squared point→triangle distance. `pub`: shared by the boolean classifier
/// and the D3 cluster-DAG Hausdorff gate (Single Responsibility + reuse rule —
/// one implementation, promoted rather than duplicated).
pub fn point_triangle_dist_sq(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return ap.length_squared();
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return bp.length_squared();
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return (a + ab * v - p).length_squared();
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return cp.length_squared();
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return (a + ac * w - p).length_squared();
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return (b + (c - b) * w - p).length_squared();
    }
    let denom = va + vb + vc;
    if denom.abs() < 1e-20 {
        return ap.length_squared();
    }
    let inv = 1.0 / denom;
    let v = vb * inv;
    let w = vc * inv;
    (a + ab * v + ac * w - p).length_squared()
}

/// The mainline robust boolean: split-by-planes + winding-number classification (see section header).
pub(super) fn robust_boolean(a: &HalfEdgeMesh, b: &HalfEdgeMesh, op: BoolOp) -> Option<HalfEdgeMesh> {
    // Local frame (small coords → split_polygon's PLANE_EPS is meaningful; translation-invariant).
    let (amin, amax) = a.bounding_box();
    let (bmin, bmax) = b.bounding_box();
    let origin = (amin.min(bmin) + amax.max(bmax)) * 0.5;
    let shift = -origin;
    let pa: Vec<Polygon> = mesh_to_polygons(a)?.iter().map(|p| p.translated(shift).with_source(SRC_A)).collect();
    let pb: Vec<Polygon> = mesh_to_polygons(b)?.iter().map(|p| p.translated(shift).with_source(SRC_B)).collect();
    if pa.len() + pb.len() > MAX_BSP_POLYS {
        return None;
    }

    let a_tris = tri_soup(&pa);
    let b_tris = tri_soup(&pb);
    if a_tris.is_empty() || b_tris.is_empty() {
        return None;
    }

    // Winding-probe offsets: capped by the scene size, floored above f32 noise, and
    // per-fragment feature-relative in between (see `probe_eps_for` — the fix for the
    // dense-bevel-on-long-object sliver bug, POLY-BOOL-BEVEL 2026-07-23).
    let diag = (amax.max(bmax) - amin.min(bmin)).length().max(1e-4);
    let eps_cap = diag * 5e-4;
    let eps_floor = (PLANE_EPS * 10.0).min(eps_cap);
    let inside = |w: f32| w > 0.5;

    // Split each solid's faces by the OTHER's faces, gated by AABB overlap (broad-phase) so flat walls
    // far from the intersection are never shattered. After this no fragment straddles the other surface.
    let margin = diag * 1e-3;
    let a_frags = split_targeted(&pa, &pb, margin);
    let b_frags = split_targeted(&pb, &pa, margin);
    let (nfa, nfb) = (a_frags.len(), b_frags.len());
    let mut kept: Vec<Polygon> = Vec::with_capacity(nfa + nfb);

    // A-fragments vs solid B. Probe B's winding just off both sides of the fragment along A's normal.
    for frag in a_frags {
        let c = poly_centroid(&frag);
        let n = frag.plane.normal;
        let eps = probe_eps_for(&frag, eps_floor, eps_cap);
        let wp = winding_number(&b_tris, c + n * eps); // +n side (A's exterior)
        let wm = winding_number(&b_tris, c - n * eps); // −n side (A's interior)
        let out_b = !inside(wp) && !inside(wm);
        let in_b = inside(wp) && inside(wm);
        let copl_same = !inside(wp) && inside(wm);  // B solid on −n side ⇒ B outward ∥ A normal
        let copl_opp = inside(wp) && !inside(wm);   // B solid on +n side ⇒ B outward anti-∥ A normal
        let keep = match op {
            BoolOp::Union => out_b || copl_same,      // shared wall kept once (from A); opposite = interior
            BoolOp::Intersect => in_b || copl_same,
            BoolOp::Difference => out_b || copl_opp,  // coplanar-same seam drops; opposite keeps A's wall
        };
        if keep { kept.push(frag); }
    }

    // B-fragments vs solid A. Coplanar-with-A fragments are ALWAYS dropped here — the A side already
    // decided the shared wall (keeps exactly one copy / cancels), so B must not double it.
    for mut frag in b_frags {
        let c = poly_centroid(&frag);
        let m = frag.plane.normal;
        let eps = probe_eps_for(&frag, eps_floor, eps_cap);
        let wp = winding_number(&a_tris, c + m * eps);
        let wm = winding_number(&a_tris, c - m * eps);
        let coplanar = inside(wp) != inside(wm);
        if coplanar { continue; }
        let in_a = inside(wp) && inside(wm);
        let (keep, flip) = match op {
            BoolOp::Union => (!in_a, false),
            BoolOp::Intersect => (in_a, false),
            BoolOp::Difference => (in_a, true), // subtracted B surface faces into the cavity → flip
        };
        if keep {
            if flip { frag.flip(); }
            kept.push(frag);
        }
    }

    if kept.is_empty() {
        return None;
    }
    let mut mesh = polygons_to_mesh(&kept, op)?;
    if mesh.face_count() == 0 {
        return None;
    }
    for v in mesh.vertices.iter_mut() {
        v.position += origin;
    }
    Some(mesh)
}

