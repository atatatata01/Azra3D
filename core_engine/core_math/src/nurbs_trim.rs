//! Trimmed NURBS parameter-space evaluation & tessellation (PROMPT_TRIMMED_SURFACES).
//!
//! Performance:
//! - Empty trims → existing `surface.tessellate()` (untouched)
//! - Sector holes (fillet): shared UV grid + index tris + light arc strip (~O(n²) once)
//! - No per-triangle unique verts, no depth-4 recursion over 48² cells

use crate::nurbs::{NurbsSurface, TrimCurve2d, TrimLoop};
use glam::{Vec2, Vec3};

// ── Curve flatten ────────────────────────────────────────────────────────

pub fn flatten_curve(
    curve: &TrimCurve2d,
    surface: &NurbsSurface,
    chord_tol: f32,
) -> Vec<Vec2> {
    match curve {
        TrimCurve2d::Line { a, b } => vec![*a, *b],
        TrimCurve2d::Arc {
            centre,
            radius,
            t0,
            t1,
        } => {
            let r = radius.max(1e-8);
            let (a0, d) = normalize_arc_span(*t0, *t1);
            let (u0, u1) = surface.domain_u();
            let (v0, v1) = surface.domain_v();
            let cu = centre.x.clamp(u0, u1);
            let cv = centre.y.clamp(v0, v1);
            let eps = ((u1 - u0) + (v1 - v0)) * 0.005;
            let p0 = surface.evaluate(cu, cv);
            let p1 = surface.evaluate((cu + eps).clamp(u0, u1), cv);
            let scale = (p1 - p0).length() / eps.max(1e-8);
            let r3 = (r * scale).max(1e-6);
            let max_ang = (2.0 * (chord_tol / (2.0 * r3)).clamp(0.0, 0.999).asin()).max(0.012);
            let n = ((d.abs() / max_ang).ceil() as usize).clamp(8, 24);
            let mut pts = Vec::with_capacity(n + 1);
            for i in 0..=n {
                let t = a0 + d * (i as f32 / n as f32);
                pts.push(*centre + Vec2::new(t.cos(), t.sin()) * r);
            }
            pts
        }
        TrimCurve2d::Nurbs2d { cps, .. } => cps.iter().map(|p| Vec2::new(p.x, p.y)).collect(),
    }
}

fn normalize_arc_span(t0: f32, t1: f32) -> (f32, f32) {
    let mut d = t1 - t0;
    while d > std::f32::consts::PI {
        d -= 2.0 * std::f32::consts::PI;
    }
    while d < -std::f32::consts::PI {
        d += 2.0 * std::f32::consts::PI;
    }
    (t0, d)
}

pub fn flatten_loop(loop_: &TrimLoop, surface: &NurbsSurface, chord_tol: f32) -> Vec<Vec2> {
    let mut pts = Vec::new();
    for c in &loop_.curves {
        let seg = flatten_curve(c, surface, chord_tol);
        if seg.is_empty() {
            continue;
        }
        if pts.is_empty() {
            pts.extend(seg);
        } else {
            let start = if (seg[0] - *pts.last().unwrap()).length_squared() < 1e-12 {
                1
            } else {
                0
            };
            pts.extend(seg.into_iter().skip(start));
        }
    }
    if pts.len() >= 2 && (pts[0] - *pts.last().unwrap()).length_squared() < 1e-12 {
        pts.pop();
    }
    pts
}

// ── Point classification ─────────────────────────────────────────────────

pub fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        if (pi.y > p.y) != (pj.y > p.y) {
            let x_int = (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y + 1e-20) + pi.x;
            if p.x < x_int {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Fast exact test for fillet sector holes (Arc + two Lines to corner).
fn point_in_sector_hole(uv: Vec2, hole: &TrimLoop) -> Option<bool> {
    let mut centre = None;
    let mut radius = None;
    let mut corner = None;
    for c in &hole.curves {
        match c {
            TrimCurve2d::Arc {
                centre: ctr,
                radius: r,
                ..
            } => {
                centre = Some(*ctr);
                radius = Some(*r);
            }
            TrimCurve2d::Line { a, b } => {
                // Corner is the endpoint shared by both lines
                if corner.is_none() {
                    corner = Some(*a);
                }
                // Prefer vertex that appears twice
                let _ = b;
            }
            _ => {}
        }
    }
    // Find corner as shared point of the two lines
    let lines: Vec<(Vec2, Vec2)> = hole
        .curves
        .iter()
        .filter_map(|c| match c {
            TrimCurve2d::Line { a, b } => Some((*a, *b)),
            _ => None,
        })
        .collect();
    if lines.len() >= 2 {
        let (a0, b0) = lines[0];
        let (a1, b1) = lines[1];
        const E: f32 = 1e-5;
        if (a0 - a1).length_squared() < E || (a0 - b1).length_squared() < E {
            corner = Some(a0);
        } else if (b0 - a1).length_squared() < E || (b0 - b1).length_squared() < E {
            corner = Some(b0);
        }
    }
    let (ctr, r, cnr) = match (centre, radius, corner) {
        (Some(c), Some(r), Some(k)) => (c, r, k),
        _ => return None,
    };
    // Fillet corner notch:
    //   Arc |P-C|=r is the KEEP boundary. REMOVED = corner quadrant AND outside circle.
    //   (Previously inverted — that kept the solid corner and cut the wrong region.)
    let in_quadrant = ((uv.x - ctr.x) * (cnr.x - ctr.x) >= -1e-6)
        && ((uv.y - ctr.y) * (cnr.y - ctr.y) >= -1e-6);
    if !in_quadrant {
        return Some(false);
    }
    // Strictly outside the arc circle → removed. On/inside arc → kept.
    let d = (uv - ctr).length();
    Some(d > r + 1e-5)
}

/// Precomputed hole polylines for PIP.
struct TrimClass {
    outers: Vec<Vec<Vec2>>,
    holes_poly: Vec<Vec<Vec2>>,
    holes_raw: Vec<TrimLoop>,
}

impl TrimClass {
    fn from_trims(trims: &[TrimLoop], surface: &NurbsSurface) -> Self {
        let holes_raw: Vec<TrimLoop> = trims.iter().filter(|t| !t.is_outer).cloned().collect();
        // Sector holes use O(1) analytic test — skip expensive polyline flatten
        let all_sector = holes_raw.iter().all(|h| point_in_sector_hole(Vec2::ZERO, h).is_some());
        let chord = 0.01;
        let outers: Vec<Vec<Vec2>> = trims
            .iter()
            .filter(|t| t.is_outer)
            .map(|t| flatten_loop(t, surface, chord))
            .filter(|p| p.len() >= 3)
            .collect();
        let holes_poly = if all_sector && outers.is_empty() {
            Vec::new()
        } else {
            holes_raw
                .iter()
                .map(|t| flatten_loop(t, surface, chord))
                .filter(|p| p.len() >= 3)
                .collect()
        };
        Self {
            outers,
            holes_poly,
            holes_raw,
        }
    }

    #[inline]
    fn inside(&self, uv: Vec2) -> bool {
        if !self.outers.is_empty() && !self.outers.iter().any(|o| point_in_polygon(uv, o)) {
            return false;
        }
        for (i, h) in self.holes_raw.iter().enumerate() {
            if let Some(inh) = point_in_sector_hole(uv, h) {
                if inh {
                    return false;
                }
                continue;
            }
            if i < self.holes_poly.len() && point_in_polygon(uv, &self.holes_poly[i]) {
                return false;
            }
        }
        true
    }
}

pub fn point_in_trims(uv: Vec2, trims: &[TrimLoop]) -> bool {
    if trims.is_empty() {
        return true;
    }
    // Use a unit plane for pure-UV tests in unit tests without a real face
    let dummy = NurbsSurface::new_plane(2.0, 2.0);
    let cls = TrimClass::from_trims(trims, &dummy);
    cls.inside(uv)
}

// ── Tessellation: closed polygon (domain − sector arcs) + ear-clip ───────

#[derive(Clone, Debug)]
struct SectorUv {
    corner: Vec2,
    centre: Vec2,
    radius: f32,
    t0: f32,
    t1: f32,
}

fn parse_sector_uv(hole: &TrimLoop) -> Option<SectorUv> {
    let mut centre = None;
    let mut radius = None;
    let mut t0 = 0.0;
    let mut t1 = 0.0;
    let mut lines: Vec<(Vec2, Vec2)> = Vec::new();
    for c in &hole.curves {
        match c {
            TrimCurve2d::Arc {
                centre: ctr,
                radius: r,
                t0: a,
                t1: b,
            } => {
                centre = Some(*ctr);
                radius = Some(*r);
                t0 = *a;
                t1 = *b;
            }
            TrimCurve2d::Line { a, b } => lines.push((*a, *b)),
            _ => {}
        }
    }
    let (ctr, r) = (centre?, radius?);
    let mut corner = None;
    if lines.len() >= 2 {
        let (a0, b0) = lines[0];
        let (a1, b1) = lines[1];
        const E: f32 = 1e-5;
        if (a0 - a1).length_squared() < E || (a0 - b1).length_squared() < E {
            corner = Some(a0);
        } else if (b0 - a1).length_squared() < E || (b0 - b1).length_squared() < E {
            corner = Some(b0);
        }
    }
    let corner = corner?;
    Some(SectorUv {
        corner,
        centre: ctr,
        radius: r,
        t0,
        t1,
    })
}

fn sample_sector_arc(sec: &SectorUv, n: usize) -> Vec<Vec2> {
    let (a0, d) = normalize_arc_span(sec.t0, sec.t1);
    let n = n.max(4);
    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = a0 + d * (i as f32 / n as f32);
        pts.push(sec.centre + Vec2::new(t.cos(), t.sin()) * sec.radius);
    }
    pts
}

/// Build single closed UV polygon: domain rectangle with sector corners replaced by arcs.
fn build_kept_polygon_sectors(
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
    sectors: &[SectorUv],
) -> Vec<Vec2> {
    let domain = [
        Vec2::new(u0, v0),
        Vec2::new(u1, v0),
        Vec2::new(u1, v1),
        Vec2::new(u0, v1),
    ];
    let mut poly = Vec::new();
    for i in 0..4 {
        let c = domain[i];
        let prev = domain[(i + 3) % 4];
        let next = domain[(i + 1) % 4];
        if let Some(sec) = sectors
            .iter()
            .find(|s| (s.corner - c).length() < 1e-3)
        {
            // Dense free-end arcs kill dark-wedge shading at cylinder free ends.
            let mut arc = sample_sector_arc(sec, 32);
            // Order so we enter from prev-edge and leave toward next-edge
            let mid_prev = (prev + c) * 0.5;
            if arc.first().map(|p| p.distance(mid_prev)).unwrap_or(0.0)
                > arc.last().map(|p| p.distance(mid_prev)).unwrap_or(0.0)
            {
                arc.reverse();
            }
            // Drop samples that jump outside domain slightly
            for p in arc {
                let q = Vec2::new(p.x.clamp(u0, u1), p.y.clamp(v0, v1));
                if poly
                    .last()
                    .map(|l: &Vec2| (*l - q).length_squared() > 1e-12)
                    .unwrap_or(true)
                {
                    poly.push(q);
                }
            }
            let _ = next;
        } else {
            poly.push(c);
        }
    }
    // Densify long straight spans for smoother shading (tighter = fewer flat fans)
    let max_e = ((u1 - u0) + (v1 - v0)) * 0.035;
    densify_poly_edges(&mut poly, max_e);
    // Ensure CCW
    if signed_area_uv(&poly) < 0.0 {
        poly.reverse();
    }
    poly
}

fn densify_poly_edges(poly: &mut Vec<Vec2>, max_e: f32) {
    if poly.len() < 2 || max_e < 1e-6 {
        return;
    }
    let n = poly.len();
    let mut out = Vec::new();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        out.push(a);
        let d = a.distance(b);
        if d > max_e {
            let steps = (d / max_e).ceil() as usize;
            for s in 1..steps {
                out.push(a.lerp(b, s as f32 / steps as f32));
            }
        }
    }
    *poly = out;
}

fn signed_area_uv(poly: &[Vec2]) -> f32 {
    let n = poly.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = poly[i];
        let q = poly[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

/// Ear-clip simple CCW polygon → triangle corner indices into `poly`.
/// Ear-clip a simple 2D polygon (either winding) into triangles. `pub`: also the cap
/// triangulator for the knife free-polyline cutter prism (poly_knife.rs) — one ear
/// clipper in the codebase, not two.
pub fn ear_clip_uv(poly: &[Vec2]) -> Vec<[usize; 3]> {
    let n0 = poly.len();
    if n0 < 3 {
        return Vec::new();
    }
    let mut idx: Vec<usize> = (0..n0).collect();
    if signed_area_uv(poly) < 0.0 {
        idx.reverse();
    }
    let mut tris = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < n0 * n0 {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let i0 = idx[(i + m - 1) % m];
            let i1 = idx[i];
            let i2 = idx[(i + 1) % m];
            let a = poly[i0];
            let b = poly[i1];
            let c = poly[i2];
            let cross = (b - a).perp_dot(c - b);
            if cross <= 1e-12 {
                continue;
            }
            let mut empty = true;
            for &j in &idx {
                if j == i0 || j == i1 || j == i2 {
                    continue;
                }
                if point_in_tri_uv(poly[j], a, b, c) {
                    empty = false;
                    break;
                }
            }
            if !empty {
                continue;
            }
            tris.push([i0, i1, i2]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            let i0 = idx[0];
            for k in 1..idx.len() - 1 {
                tris.push([i0, idx[k], idx[k + 1]]);
            }
            break;
        }
    }
    if idx.len() == 3 {
        tris.push([idx[0], idx[1], idx[2]]);
    }
    tris
}

fn point_in_tri_uv(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let d1 = (p.x - b.x) * (a.y - b.y) - (a.x - b.x) * (p.y - b.y);
    let d2 = (p.x - c.x) * (b.y - c.y) - (b.x - c.x) * (p.y - c.y);
    let d3 = (p.x - a.x) * (c.y - a.y) - (c.x - a.x) * (p.y - a.y);
    let has_neg = (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0);
    let has_pos = (d1 > 0.0) || (d2 > 0.0) || (d3 > 0.0);
    !(has_neg && has_pos)
}

/// Tessellate with trims. Sector holes → closed polygon ear-clip (no grid holes).
pub fn tessellate_trimmed(
    surface: &NurbsSurface,
    trims: &[TrimLoop],
) -> (Vec<[f32; 8]>, Vec<u32>) {
    if trims.is_empty() {
        return surface.tessellate();
    }

    let (u0, u1) = surface.domain_u();
    let (v0, v1) = surface.domain_v();
    let planar = surface.degree_u <= 1 && surface.degree_v <= 1;
    let n_const = if planar {
        surface.normal(0.5, 0.5)
    } else {
        Vec3::ZERO
    };

    let holes: Vec<&TrimLoop> = trims.iter().filter(|t| !t.is_outer).collect();
    let sectors: Vec<SectorUv> = holes.iter().filter_map(|h| parse_sector_uv(h)).collect();

    // Fast closed path when every hole is a fillet sector (typical multi-edge case)
    if !sectors.is_empty()
        && sectors.len() == holes.len()
        && trims.iter().all(|t| !t.is_outer || t.curves.is_empty())
    {
        let poly = build_kept_polygon_sectors(u0, u1, v0, v1, &sectors);
        if poly.len() >= 3 {
            let tris = ear_clip_uv(&poly);
            if !tris.is_empty() {
                let mut verts = Vec::with_capacity(poly.len());
                let mut indices = Vec::with_capacity(tris.len() * 3);
                for &uv in &poly {
                    let p = surface.evaluate(uv.x, uv.y);
                    let n = if planar {
                        n_const
                    } else {
                        surface.normal(uv.x, uv.y)
                    };
                    let uu = (uv.x - u0) / (u1 - u0).max(1e-8);
                    let vv = (uv.y - v0) / (v1 - v0).max(1e-8);
                    verts.push([p.x, p.y, p.z, n.x, n.y, n.z, uu, vv]);
                }
                for t in tris {
                    let mid = (poly[t[0]] + poly[t[1]] + poly[t[2]]) / 3.0;
                    // Safety: only emit if centroid is kept
                    if point_in_trims(mid, trims) {
                        indices.extend_from_slice(&[t[0] as u32, t[1] as u32, t[2] as u32]);
                    }
                }
                if !indices.is_empty() {
                    return (verts, indices);
                }
            }
        }
    }

    // Fallback: denser shared grid (still better than empty)
    tessellate_trimmed_grid_fallback(surface, trims, n_const, planar)
}

fn tessellate_trimmed_grid_fallback(
    surface: &NurbsSurface,
    trims: &[TrimLoop],
    n_const: Vec3,
    planar: bool,
) -> (Vec<[f32; 8]>, Vec<u32>) {
    let cls = TrimClass::from_trims(trims, surface);
    let (u0, u1) = surface.domain_u();
    let (v0, v1) = surface.domain_v();
    let su = 28u32;
    let sv = 28u32;
    let nu = su as usize + 1;
    let nv = sv as usize + 1;
    let mut keep = vec![false; nu * nv];
    let mut uvs = Vec::with_capacity(nu * nv);
    for iu in 0..nu {
        let u = u0 + (u1 - u0) * iu as f32 / su as f32;
        for iv in 0..nv {
            let v = v0 + (v1 - v0) * iv as f32 / sv as f32;
            let uv = Vec2::new(u, v);
            keep[iu * nv + iv] = cls.inside(uv);
            uvs.push(uv);
        }
    }
    let mut vert_of = vec![u32::MAX; nu * nv];
    let mut verts: Vec<[f32; 8]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut push = |uv: Vec2| -> u32 {
        let p = surface.evaluate(uv.x, uv.y);
        let n = if planar {
            n_const
        } else {
            surface.normal(uv.x, uv.y)
        };
        let i = verts.len() as u32;
        verts.push([
            p.x,
            p.y,
            p.z,
            n.x,
            n.y,
            n.z,
            (uv.x - u0) / (u1 - u0).max(1e-8),
            (uv.y - v0) / (v1 - v0).max(1e-8),
        ]);
        i
    };
    for i in 0..uvs.len() {
        if keep[i] {
            vert_of[i] = push(uvs[i]);
        }
    }
    for iu in 0..su as usize {
        for iv in 0..sv as usize {
            let i00 = iu * nv + iv;
            let i10 = (iu + 1) * nv + iv;
            let i11 = (iu + 1) * nv + iv + 1;
            let i01 = iu * nv + iv + 1;
            let ks = [keep[i00], keep[i10], keep[i11], keep[i01]];
            let n = ks.iter().filter(|&&x| x).count();
            if n == 4 {
                indices.extend_from_slice(&[
                    vert_of[i00],
                    vert_of[i10],
                    vert_of[i11],
                    vert_of[i00],
                    vert_of[i11],
                    vert_of[i01],
                ]);
            } else if n == 3 {
                let mut ids = Vec::new();
                for (k, id) in [
                    (ks[0], vert_of[i00]),
                    (ks[1], vert_of[i10]),
                    (ks[2], vert_of[i11]),
                    (ks[3], vert_of[i01]),
                ] {
                    if k {
                        ids.push(id);
                    }
                }
                if ids.len() == 3 {
                    indices.extend_from_slice(&ids);
                }
            }
        }
    }
    // Arc boundary strip to seal silhouette
    for hole in &cls.holes_raw {
        if let Some(sec) = parse_sector_uv(hole) {
            let pts = sample_sector_arc(&sec, 20);
            let inward = (sec.centre - sec.corner).normalize_or_zero() * (sec.radius * 0.15).max(0.008);
            let mut ids = Vec::new();
            for &p in &pts {
                let q = if cls.inside(p) {
                    p
                } else {
                    p + (sec.centre - sec.corner).normalize_or_zero() * 1e-3
                };
                if cls.inside(q) {
                    ids.push(Some(push(q)));
                } else {
                    ids.push(None);
                }
            }
            for w in 0..pts.len().saturating_sub(1) {
                let (Some(ia), Some(ib)) = (ids[w], ids[w + 1]) else {
                    continue;
                };
                let mid = (pts[w] + pts[w + 1]) * 0.5 + inward;
                if cls.inside(mid) {
                    let ic = push(mid);
                    indices.extend_from_slice(&[ia, ib, ic]);
                }
            }
        }
    }
    if indices.is_empty() {
        return surface.tessellate();
    }
    (verts, indices)
}

// ── UV mapping + hole construction ───────────────────────────────────────

pub fn world_to_uv(surface: &NurbsSurface, p: Vec3) -> Option<Vec2> {
    let (u0, u1) = surface.domain_u();
    let (v0, v1) = surface.domain_v();
    let p00 = surface.evaluate(u0, v0);
    let p10 = surface.evaluate(u1, v0);
    let p01 = surface.evaluate(u0, v1);
    let du = p10 - p00;
    let dv = p01 - p00;
    if du.cross(dv).length_squared() < 1e-12 {
        return None;
    }
    let w = p - p00;
    let dudu = du.dot(du);
    let dvdv = dv.dot(dv);
    let dudv = du.dot(dv);
    let wdu = w.dot(du);
    let wdv = w.dot(dv);
    let det = dudu * dvdv - dudv * dudv;
    if det.abs() < 1e-12 {
        return None;
    }
    let s = (wdu * dvdv - wdv * dudv) / det;
    let t = (dudu * wdv - dudv * wdu) / det;
    Some(Vec2::new(u0 + s * (u1 - u0), v0 + t * (v1 - v0)))
}

pub fn corner_notch_hole(
    surface: &NurbsSurface,
    corner: Vec3,
    arc_start: Vec3,
    arc_end: Vec3,
    arc_centre: Vec3,
) -> Option<TrimLoop> {
    let c_uv = world_to_uv(surface, corner)?;
    let s_uv = world_to_uv(surface, arc_start)?;
    let e_uv = world_to_uv(surface, arc_end)?;
    let o_uv = world_to_uv(surface, arc_centre)?;
    let r = (s_uv - o_uv).length().max(1e-6);
    let t0 = (s_uv - o_uv).y.atan2((s_uv - o_uv).x);
    let t1 = (e_uv - o_uv).y.atan2((e_uv - o_uv).x);
    // Arc mid should be farther from corner than centre (keep-side arc)
    let (a0, d) = normalize_arc_span(t0, t1);
    let mid_ang = a0 + d * 0.5;
    let mid_uv = o_uv + Vec2::new(mid_ang.cos(), mid_ang.sin()) * r;
    let (t0, t1) = if mid_uv.distance(c_uv) + 1e-5 < o_uv.distance(c_uv) {
        (t1, t0)
    } else {
        (t0, t1)
    };
    Some(TrimLoop {
        is_outer: false,
        curves: vec![
            TrimCurve2d::Line { a: c_uv, b: s_uv },
            TrimCurve2d::Arc {
                centre: o_uv,
                radius: r,
                t0,
                t1,
            },
            TrimCurve2d::Line { a: e_uv, b: c_uv },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurbs::{NurbsFace, NurbsFaceId, NurbsSurface, TrimCurve2d, TrimLoop};
    use crate::nurbs_fillet::apply_edges_fillet_batch;
    use glam::Vec2;

    #[test]
    fn t2_untrimmed_matches_surface_tessellate() {
        let s = NurbsSurface::new_plane(2.0, 2.0);
        let face = NurbsFace::new(NurbsFaceId(0), s.clone());
        let (v0, i0) = face.tessellate();
        let (v1, i1) = s.tessellate();
        assert_eq!(v0.len(), v1.len());
        assert_eq!(i0, i1);
    }

    #[test]
    fn sector_hole_excludes_corner() {
        let s = NurbsSurface::new_plane(2.0, 2.0);
        let (u0, u1) = s.domain_u();
        let (v0, v1) = s.domain_v();
        let r = 0.4_f32;
        let corner = Vec2::new(u1, v1);
        let centre = Vec2::new(u1 - r, v1 - r);
        let hole = TrimLoop {
            is_outer: false,
            curves: vec![
                TrimCurve2d::Line {
                    a: corner,
                    b: Vec2::new(u1, v1 - r),
                },
                TrimCurve2d::Arc {
                    centre,
                    radius: r,
                    t0: 0.0,
                    t1: std::f32::consts::FRAC_PI_2,
                },
                TrimCurve2d::Line {
                    a: Vec2::new(u1 - r, v1),
                    b: corner,
                },
            ],
        };
        // Corner must be outside kept region
        assert!(!point_in_trims(corner, &[hole.clone()]));
        // Face centre must be kept
        assert!(point_in_trims(
            Vec2::new((u0 + u1) * 0.5, (v0 + v1) * 0.5),
            &[hole.clone()]
        ));
        // Point near corner inside sector removed
        assert!(!point_in_trims(
            Vec2::new(u1 - 0.05, v1 - 0.05),
            &[hole.clone()]
        ));
    }

    #[test]
    fn t2_trimmed_mesh_has_no_corner_vertex() {
        let s = NurbsSurface::new_plane(2.0, 2.0);
        let (u0, u1) = s.domain_u();
        let (_v0, v1) = s.domain_v();
        let r = 0.35_f32;
        let corner = Vec2::new(u1, v1);
        let centre = Vec2::new(u1 - r, v1 - r);
        let hole = TrimLoop {
            is_outer: false,
            curves: vec![
                TrimCurve2d::Line {
                    a: corner,
                    b: Vec2::new(u1, v1 - r),
                },
                TrimCurve2d::Arc {
                    centre,
                    radius: r,
                    t0: 0.0,
                    t1: std::f32::consts::FRAC_PI_2,
                },
                TrimCurve2d::Line {
                    a: Vec2::new(u1 - r, v1),
                    b: corner,
                },
            ],
        };
        let (verts, idx) = tessellate_trimmed(&s, &[hole]);
        assert!(!idx.is_empty(), "must produce triangles");
        // No tess vertex should sit near the removed corner in UV
        for v in &verts {
            let u = u0 + v[6] * (u1 - u0);
            let vv = s.domain_v().0 + v[7] * (s.domain_v().1 - s.domain_v().0);
            let d = (Vec2::new(u, vv) - corner).length();
            assert!(
                d > r * 0.25,
                "tess vertex too close to removed corner d={d} r={r}"
            );
        }
    }

    #[test]
    fn fillet_side_face_mesh_drops_solid_corner() {
        // End-to-end: after single-edge fillet, notched corner UV is outside kept region
        // and tessellation has no vertex near that 3D corner.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let r = 0.15_f32;
        apply_edges_fillet_batch(&mut faces, &[(0, 2)], r, 100).expect("fillet");

        let mut checked = 0u32;
        for (fi, face) in faces.iter().enumerate().take(6) {
            if face.trims.is_empty() {
                continue;
            }
            checked += 1;
            // Find hole corner UV from first Line of first hole
            let hole = face.trims.iter().find(|t| !t.is_outer).expect("hole");
            let corner_uv = match &hole.curves[0] {
                TrimCurve2d::Line { a, .. } => *a,
                _ => panic!("expected Line"),
            };
            assert!(
                !face.uv_inside(corner_uv.x, corner_uv.y),
                "face {fi}: hole corner UV must be outside kept region"
            );
            let corner_3d = face.surface.evaluate(corner_uv.x, corner_uv.y);
            let (verts, idx) = face.tessellate();
            assert!(!idx.is_empty(), "face {fi} must produce trimmed mesh");
            let mut min_d = f32::MAX;
            for v in &verts {
                let p = Vec3::new(v[0], v[1], v[2]);
                min_d = min_d.min(p.distance(corner_3d));
            }
            assert!(
                min_d > r * 0.2,
                "face {fi}: mesh still reaches solid corner min_d={min_d} r={r}"
            );
        }
        assert!(checked >= 1, "expected at least one trimmed side face");
    }
}
