//! Corner caps: spherical octant / toroidal OnSame / ruled loft (split from nurbs_fillet.rs).

use crate::nurbs::{ControlPoint, FaceOrigin, NurbsFace, NurbsFaceId, NurbsSurface};
use glam::Vec3;

use super::*;

/// FreeCAD-style toroidal OnSame corner (equal r, orthogonal, 2+1 concavity).
///
/// Oracle (boss base triad): major = 2r, minor = r, axis along the **odd** edge
/// (unique convexity), centre on that edge's cylinder axis at height r from the
/// majority plane. Rational 3×3 quarter-torus patch.
/// `axis` = unit odd-edge direction; `e0`,`e1` = free-space basis in the major plane.
pub fn make_toroidal_onsame_corner(
    vertex: Vec3,
    axis: Vec3,
    e0: Vec3,
    e1: Vec3,
    r: f32,
) -> Result<NurbsSurface, FilletError> {
    let r = r.max(1e-5);
    let ax = axis.normalize_or_zero();
    let mut u0 = e0.normalize_or_zero();
    let mut u1 = e1.normalize_or_zero();
    if ax.length_squared() < 0.5 || u0.length_squared() < 0.5 || u1.length_squared() < 0.5 {
        return Err(FilletError::DegenerateEdge);
    }
    // Orthonormalize major plane against axis
    u0 = (u0 - ax * u0.dot(ax)).normalize_or_zero();
    u1 = (u1 - ax * u1.dot(ax)).normalize_or_zero();
    if u0.length_squared() < 0.5 {
        u0 = ax.cross(u1).normalize_or_zero();
    }
    if u1.length_squared() < 0.5 {
        u1 = ax.cross(u0).normalize_or_zero();
    }
    // Ensure right-handed u0,u1,ax
    if u0.cross(u1).dot(ax) < 0.0 {
        std::mem::swap(&mut u0, &mut u1);
    }
    let major_r = 2.0 * r;
    let minor_r = r;
    // Torus centre: FreeCAD boss — vertex − r e0 − r e1 + r ax
    let centre = vertex - u0 * r - u1 * r + ax * r;
    let w = W_MID;
    // Rational tensor-product of two 90° circle Beziers (Piegl):
    // Major CPs (not surface samples): C + R e0, C + R(e0+e1), C + R e1
    // Tube CPs in the (radial, axis) frame at each major station.
    let major_cps = [
        (centre + u0 * major_r, u0, 1.0f32),
        (centre + (u0 + u1) * major_r, (u0 + u1).normalize_or_zero(), w),
        (centre + u1 * major_r, u1, 1.0f32),
    ];
    // control_points[u=major][v=tube]
    let mut control_points = vec![vec![ControlPoint::new(Vec3::ZERO, 1.0); 3]; 3];
    for (j, &(mj, rhat, wj)) in major_cps.iter().enumerate() {
        // Tube Bezier 90°: from +rhat (outer equator) to +ax (along axis)
        let t0 = mj + rhat * minor_r;
        let t1 = mj + rhat * minor_r + ax * minor_r;
        let t2 = mj + ax * minor_r;
        let tube = [(t0, 1.0f32), (t1, w), (t2, 1.0f32)];
        for (i, &(pos, wi)) in tube.iter().enumerate() {
            control_points[j][i] = ControlPoint::new(pos, wj * wi);
        }
    }
    Ok(NurbsSurface {
        control_points,
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        degree_u: 2,
        degree_v: 2,
        name: "Toroid".into(),
        mesh_cache_id: None,
        tess_tolerance: 0.25,
    })
}

/// Resolve torus frame at a mixed 3-edge vertex from selected edges.
/// Returns (axis, e0, e1) for [`make_toroidal_onsame_corner`].
pub fn onsame_torus_frame(
    originals: &[NurbsFace],
    edges_at: &[(usize, u8, usize, u8, Vec3, Vec3, bool)],
    vertex: Vec3,
) -> Option<(Vec3, Vec3, Vec3)> {
    if edges_at.len() != 3 {
        return None;
    }
    let n_cx = edges_at.iter().filter(|e| e.6).count();
    if n_cx != 1 && n_cx != 2 {
        return None; // not 2+1 OnSame
    }
    let odd_convex = n_cx == 1;
    let mut odd: Option<&(usize, u8, usize, u8, Vec3, Vec3, bool)> = None;
    let mut maj = Vec::new();
    for e in edges_at {
        if e.6 == odd_convex {
            odd = Some(e);
        } else {
            maj.push(e);
        }
    }
    let odd = odd?;
    if maj.len() != 2 {
        return None;
    }
    // Axis = odd edge direction away from vertex
    let axis = {
        let d = if (odd.4 - vertex).length_squared() < 1e-8 {
            odd.5 - odd.4
        } else {
            odd.4 - odd.5
        };
        d.normalize_or_zero()
    };
    // e0, e1: free-space directions of majority edges (s*(n0+n1) with s for concavity)
    let mut basis = Vec::new();
    for e in &maj {
        let na = face_outward_normal(originals[e.0].surface.as_ref()).ok()?;
        let nb = face_outward_normal(originals[e.2].surface.as_ref()).ok()?;
        let s = if e.6 { -1.0f32 } else { 1.0f32 }; // free dir sign: concave +outward
        let free = (na + nb) * s;
        let f = (free - axis * free.dot(axis)).normalize_or_zero();
        if f.length_squared() > 0.5 {
            basis.push(f);
        }
    }
    if basis.len() < 2 {
        return None;
    }
    Some((axis, basis[0], basis[1]))
}

/// Unique endpoints from resolved edge list.
pub(crate) fn unique_resolved_vertices(
    resolved: &[(usize, u8, usize, u8, Vec3, Vec3, bool)],
) -> Vec<Vec3> {
    const EPS2: f32 = 1e-8;
    let mut out = Vec::new();
    for &(_, _, _, _, a0, a1, _) in resolved {
        for p in [a0, a1] {
            if !out.iter().any(|q: &Vec3| (*q - p).length_squared() < EPS2) {
                out.push(p);
            }
        }
    }
    out
}

/// Drop per-edge radius from resolved 8-tuples for helpers that take 7-tuples.
pub(crate) fn resolved_strip_r(
    resolved: &[(usize, u8, usize, u8, Vec3, Vec3, bool, f32)],
) -> Vec<(usize, u8, usize, u8, Vec3, Vec3, bool)> {
    resolved
        .iter()
        .map(|&(fa, ea, fb, eb, a0, a1, cx, _r)| (fa, ea, fb, eb, a0, a1, cx))
        .collect()
}

/// Radius of a linear law at a vertex that coincides with a0 (r0) or a1 (r1).
pub fn linear_radius_at_vertex(a0: Vec3, a1: Vec3, vertex: Vec3, r0: f32, r1: f32) -> f32 {
    const EPS2: f32 = 1e-8;
    if (vertex - a0).length_squared() < EPS2 {
        r0
    } else if (vertex - a1).length_squared() < EPS2 {
        r1
    } else {
        // Interior: project
        let elen2 = (a1 - a0).length_squared().max(1e-12);
        let t = ((vertex - a0).dot(a1 - a0) / elen2).clamp(0.0, 1.0);
        r0 + (r1 - r0) * t
    }
}

/// Free-end circular-arc CPs + centre at `vertex` for one filleted edge (matches cylinder end).
/// Uses the same α / cos(θ/2) construction as [`fillet_planar_edge_trimmed_ex`].
pub(crate) fn free_end_arc_cps(
    vertex: Vec3,
    na: Vec3,
    nb: Vec3,
    r: f32,
    convex: bool,
) -> ([ControlPoint; 3], Vec3) {
    let cos_n = na.dot(nb).clamp(-1.0, 1.0);
    let alpha = r / (1.0 + cos_n).max(1e-4);
    let s = if convex { -1.0f32 } else { 1.0f32 };
    let centre = vertex + s * alpha * (na + nb);
    let arc_sign = -s;
    let w_mid = ((1.0 + cos_n) * 0.5).max(0.0).sqrt();
    let denom = (1.0 + cos_n).max(1e-4);
    let d0 = na * arc_sign;
    let d1 = nb * arc_sign;
    let cps = [
        ControlPoint::new(centre + d0 * r, 1.0),
        ControlPoint::new(centre + (d0 + d1) * (r / denom), w_mid),
        ControlPoint::new(centre + d1 * r, 1.0),
    ];
    (cps, centre)
}

/// OnSame two-edge corner (FreeCAD: BSpline join). Equal-r approximation:
/// **circular loft** between the two free-end quarter-circles at the shared vertex.
///
/// Rails are the free-end arcs of each edge cylinder. Mid column CPs sit on the
/// tube of radius `r` about the segment joining the two free-end centres
/// (rolling-ball path A→B) — closer to FreeCAD's OnSame BSpline than a pure
/// ruled (deg-v=1) patch, which left a mid-span hole vs OCCT.
pub fn make_onsame_ruled_corner(
    originals: &[NurbsFace],
    edges_at: &[&(usize, u8, usize, u8, Vec3, Vec3, bool)],
    vertex: Vec3,
    r: f32,
) -> Result<NurbsSurface, FilletError> {
    if edges_at.len() != 2 {
        return Err(FilletError::Unsupported {
            kind: CornerKind::Two,
            state: CornerState::OnSame,
        });
    }
    let r = r.max(1e-5);
    let mut arcs: Vec<([ControlPoint; 3], Vec3)> = Vec::with_capacity(2);
    for &&(fa, _ea, fb, _eb, _, _, convex) in edges_at {
        let na = face_outward_normal(originals[fa].surface.as_ref())?;
        let nb = face_outward_normal(originals[fb].surface.as_ref())?;
        arcs.push(free_end_arc_cps(vertex, na, nb, r, convex));
    }
    let (a, c_a) = (&arcs[0].0, arcs[0].1);
    let (b, c_b) = (&arcs[1].0, arcs[1].1);
    let d_fwd = (a[0].position - b[0].position).length_squared()
        + (a[2].position - b[2].position).length_squared();
    let d_rev = (a[0].position - b[2].position).length_squared()
        + (a[2].position - b[0].position).length_squared();
    let b_ord = if d_rev < d_fwd {
        [b[2].clone(), b[1].clone(), b[0].clone()]
    } else {
        [b[0].clone(), b[1].clone(), b[2].clone()]
    };
    // Mid column: r-tube about centre segment c_a→c_b (not chord lerp of surface points).
    let c_mid = (c_a + c_b) * 0.5;
    let mut mid: Vec<ControlPoint> = Vec::with_capacity(3);
    for i in 0..3 {
        let da = (a[i].position - c_a).normalize_or_zero();
        let db = (b_ord[i].position - c_b).normalize_or_zero();
        let d_sum = da + db;
        let d_mid = if d_sum.length_squared() > 1e-8 {
            d_sum.normalize_or_zero()
        } else {
            // Opposite free-end dirs — fall back to chord mid direction from c_mid.
            let chord = (a[i].position + b_ord[i].position) * 0.5 - c_mid;
            if chord.length_squared() > 1e-8 {
                chord.normalize_or_zero()
            } else {
                da
            }
        };
        // Weight: product of end weights with circle mid factor (rational loft).
        let w = (a[i].weight * b_ord[i].weight).sqrt() * W_MID;
        mid.push(ControlPoint::new(c_mid + d_mid * r, w.max(1e-4)));
    }
    let patch = NurbsSurface {
        control_points: vec![
            vec![a[0].clone(), mid[0].clone(), b_ord[0].clone()],
            vec![a[1].clone(), mid[1].clone(), b_ord[1].clone()],
            vec![a[2].clone(), mid[2].clone(), b_ord[2].clone()],
        ],
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        degree_u: 2,
        degree_v: 2,
        name: "OnSame".into(),
        mesh_cache_id: None,
        tess_tolerance: 0.25,
    };
    let p00 = patch.evaluate(0.0, 0.0);
    let p11 = patch.evaluate(1.0, 1.0);
    if (p00 - p11).length_squared() < 1e-12 {
        return Err(FilletError::DegenerateEdge);
    }
    Ok(patch)
}

/// Exact rational biquadratic spherical **octant** corner patch.
///
/// `n0, n1, n2` must be unit, mutually orthogonal **outward** face normals meeting at a vertex.
/// Sphere centre is the solid's inset corner: `vertex - r (n0+n1+n2)`.
/// Gate: every surface point satisfies `|P âˆ’ centre| == r` (to tol).
///
/// Domain layout (degenerate pole at `n2`):
/// - `v=0`: quarter-circle from `n0` â†’ `n1` in the plane of `n0,n1` is NOT used as v=0;
///   instead u spans e0â†’e1 sides and v spans equatorâ†’pole at e2.
pub fn make_spherical_octant(
    centre: Vec3,
    radius: f32,
    n0: Vec3,
    n1: Vec3,
    n2: Vec3,
) -> Result<NurbsSurface, FilletError> {
    let r = radius.max(1e-5);
    let e0 = n0.normalize_or_zero();
    let e1 = n1.normalize_or_zero();
    let e2 = n2.normalize_or_zero();
    if e0.length_squared() < 0.5 || e1.length_squared() < 0.5 || e2.length_squared() < 0.5 {
        return Err(FilletError::DegenerateEdge);
    }
    if e0.dot(e1).abs() > 0.05 || e0.dot(e2).abs() > 0.05 || e1.dot(e2).abs() > 0.05 {
        return Err(FilletError::NotOrthogonal);
    }
    let w = W_MID;
    let w2 = w * w; // 0.5

    let p = |dir: Vec3, weight: f32| ControlPoint::new(centre + dir * r, weight);

    // control_points[u][v] â€” u: e0â†’e1, v: equatorâ†’e2 pole (degenerate v=1)
    // Standard rational sphere octant (Piegl/Tiller style):
    //   u=0: arc e0â†’e2 ; u=1: arc e1â†’e2 ; v=0: arc e0â†’e1 ; v=1: pole e2
    let mut surf = NurbsSurface {
        control_points: vec![
            vec![p(e0, 1.0), p(e0 + e2, w), p(e2, 1.0)],
            vec![p(e0 + e1, w), p(e0 + e1 + e2, w2), p(e2, w)],
            vec![p(e1, 1.0), p(e1 + e2, w), p(e2, 1.0)],
        ],
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        degree_u: 2,
        degree_v: 2,
        name: "Corner".into(),
        mesh_cache_id: None,
        // Dense sphere tess â€” sparse octant reads as clover lobes with dark gaps
        tess_tolerance: 0.25,
    };

    let n_mid = surf.normal(0.5, 0.5);
    let outward = (e0 + e1 + e2).normalize_or_zero();
    if n_mid.dot(outward) < 0.0 {
        let rows = surf.control_points.clone();
        surf.control_points = vec![rows[2].clone(), rows[1].clone(), rows[0].clone()];
    }

    Ok(surf)
}

/// Sphere centre for a cube-style corner at `vertex` with outward normals and equal radius.
pub fn sphere_centre_at_vertex(vertex: Vec3, n0: Vec3, n1: Vec3, n2: Vec3, radius: f32) -> Vec3 {
    vertex - (n0.normalize_or_zero() + n1.normalize_or_zero() + n2.normalize_or_zero()) * radius
}

/// Result of a symmetric 3-edge cube corner fillet (N5).
#[derive(Clone, Debug)]
pub struct SymmetricCornerResult {
    pub radius: f32,
    pub centre: Vec3,
    pub vertex: Vec3,
    pub corner: NurbsSurface,
    /// Three truncated edge fillets (cylinder patches).
    pub edge_fillets: [NurbsSurface; 3],
    /// Updated planar faces for the three meeting faces (order matches `face_indices`).
    pub faces: [NurbsSurface; 3],
    pub face_indices: [usize; 3],
}

/// Fillet a **symmetric cube vertex**: three mutually perpendicular planar faces, equal radius.
/// Builds three edge cylinders (trimmed by `r` at the corner) + one spherical octant.
///
/// All cylinders and shrinks are computed from the **original** faces (not sequential mutate),
/// so neighbour matching stays valid for all three edges.
///
/// `face_edges[i] = (face_idx, edge_id)` for each of the three solid edges around the vertex.
pub fn fillet_symmetric_cube_vertex(
    faces: &[NurbsFace],
    face_edges: [(usize, u8); 3],
    vertex: Vec3,
    radius: f32,
) -> Result<SymmetricCornerResult, FilletError> {
    let r = radius.max(1e-5);
    for &(fi, _) in &face_edges {
        if fi >= faces.len() {
            return Err(FilletError::BadEdgeIndex);
        }
    }

    // Resolve all three solid-edge pairs on ORIGINAL geometry.
    // Neighbour search scans ALL solid faces so we discover the third face even when
    // `face_edges` only tags one side of each edge.
    const EPS2: f32 = 1e-6;
    let same = |p: Vec3, q: Vec3| (p - q).length_squared() < EPS2;
    // (fa, ea, fb, eb, trim_start, trim_end)
    let mut pairs: Vec<(usize, u8, usize, u8, f32, f32)> = Vec::new();
    for &(fa, ea) in &face_edges {
        let sa = faces[fa].surface.as_ref();
        let (a0, a1) = face_edge_endpoints(sa, ea)?;
        let d0 = (a0 - vertex).length_squared();
        let d1 = (a1 - vertex).length_squared();
        if d0 > 1e-6 && d1 > 1e-6 {
            // Edge does not touch the vertex â€” skip
            continue;
        }
        let trim_start = if d0 < d1 { r } else { 0.0 };
        let trim_end = if d1 <= d0 { r } else { 0.0 };
        let na = face_outward_normal(sa)?;
        let mut found: Option<(usize, u8)> = None;
        for (fb, face) in faces.iter().enumerate() {
            if fb == fa {
                continue;
            }
            let sb = face.surface.as_ref();
            for eb in 0u8..4 {
                if let Ok((b0, b1)) = face_edge_endpoints(sb, eb) {
                    if (same(a0, b0) && same(a1, b1)) || (same(a0, b1) && same(a1, b0)) {
                        if let Ok(nb) = face_outward_normal(sb) {
                            if na.dot(nb).abs() < 0.15 {
                                found = Some((fb, eb));
                                break;
                            }
                        }
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (fb, eb) = found.ok_or(FilletError::DegenerateEdge)?;
        // Dedup geometric edges by midpoint
        let mid = (a0 + a1) * 0.5;
        if pairs.iter().any(|&(fa2, ea2, ..)| {
            if let Ok((p0, p1)) = face_edge_endpoints(faces[fa2].surface.as_ref(), ea2) {
                ((p0 + p1) * 0.5 - mid).length_squared() < 1e-8
            } else {
                false
            }
        }) {
            continue;
        }
        pairs.push((fa, ea, fb, eb, trim_start, trim_end));
    }
    if pairs.len() != 3 {
        return Err(FilletError::DegenerateEdge);
    }

    // Collect the three unique faces from resolved pairs
    let mut face_idxs = Vec::new();
    for &(fa, _, fb, ..) in &pairs {
        if !face_idxs.contains(&fa) {
            face_idxs.push(fa);
        }
        if !face_idxs.contains(&fb) {
            face_idxs.push(fb);
        }
    }
    if face_idxs.len() != 3 {
        return Err(FilletError::DegenerateEdge);
    }

    let n = |i: usize| face_outward_normal(faces[i].surface.as_ref());
    let n0 = n(face_idxs[0])?;
    let n1 = n(face_idxs[1])?;
    let n2 = n(face_idxs[2])?;
    if n0.dot(n1).abs() > 0.05 || n0.dot(n2).abs() > 0.05 || n1.dot(n2).abs() > 0.05 {
        return Err(FilletError::NotOrthogonal);
    }
    for &fi in &face_idxs {
        let max_r = {
            let s = faces[fi].surface.as_ref();
            let (rows, cols) = s.grid_size();
            if rows < 2 || cols < 2 {
                0.0
            } else {
                let p00 = s.control_points[0][0].position;
                let p10 = s.control_points[rows - 1][0].position;
                let p01 = s.control_points[0][cols - 1].position;
                (p10 - p00).length().min((p01 - p00).length()) * 0.5
            }
        };
        if r > max_r + 1e-6 {
            return Err(FilletError::RadiusTooLarge { max: max_r });
        }
    }

    let centre = sphere_centre_at_vertex(vertex, n0, n1, n2, r);
    let corner = make_spherical_octant(centre, r, n0, n1, n2)?;

    // Build cylinders from ORIGINAL faces; apply shrinks onto clones
    let mut updated: [NurbsSurface; 3] = [
        faces[face_idxs[0]].surface.as_ref().clone(),
        faces[face_idxs[1]].surface.as_ref().clone(),
        faces[face_idxs[2]].surface.as_ref().clone(),
    ];
    let mut edge_fillets = Vec::with_capacity(3);
    for &(fa, ea, fb, eb, ts, te) in &pairs {
        let sa = faces[fa].surface.as_ref();
        let sb = faces[fb].surface.as_ref();
        let res = fillet_orthogonal_planar_edge_trimmed(sa, ea, sb, eb, r, ts, te)?;
        edge_fillets.push(res.fillet);
        let ka = face_idxs.iter().position(|&x| x == fa).unwrap();
        let kb = face_idxs.iter().position(|&x| x == fb).unwrap();
        let na = face_outward_normal(sa)?;
        let nb = face_outward_normal(sb)?;
        shrink_face_edge(&mut updated[ka], ea, -nb * r)?;
        shrink_face_edge(&mut updated[kb], eb, -na * r)?;
    }

    Ok(SymmetricCornerResult {
        radius: r,
        centre,
        vertex,
        corner,
        edge_fillets: [
            edge_fillets[0].clone(),
            edge_fillets[1].clone(),
            edge_fillets[2].clone(),
        ],
        faces: [updated[0].clone(), updated[1].clone(), updated[2].clone()],
        face_indices: [face_idxs[0], face_idxs[1], face_idxs[2]],
    })
}

/// Apply symmetric corner result into a face list (replace 3 planar faces, append 3 edges + 1 corner).
pub fn apply_symmetric_corner_to_faces(
    faces: &mut Vec<NurbsFace>,
    result: &SymmetricCornerResult,
    mut next_id: u64,
) {
    for (i, &fi) in result.face_indices.iter().enumerate() {
        if fi < faces.len() {
            faces[fi].surface = std::sync::Arc::new(result.faces[i].clone());
        }
    }
    let mat = result
        .face_indices
        .first()
        .and_then(|&fi| faces.get(fi).map(|f| f.material.clone()))
        .unwrap_or_default();
    let between_pairs = [
        [faces[result.face_indices[0]].id, faces[result.face_indices[1]].id],
        [faces[result.face_indices[0]].id, faces[result.face_indices[2]].id],
        [faces[result.face_indices[1]].id, faces[result.face_indices[2]].id],
    ];
    for (i, ef) in result.edge_fillets.iter().enumerate() {
        let mut f = NurbsFace::new(NurbsFaceId(next_id), ef.clone()).with_origin(
            FaceOrigin::Fillet {
                radius: result.radius,
                between: between_pairs[i.min(2)],
            },
        );
        f.material = mat.clone();
        faces.push(f);
        next_id += 1;
    }
    let corner_ids = [
        faces[result.face_indices[0]].id,
        faces[result.face_indices[1]].id,
        faces[result.face_indices[2]].id,
    ];
    let mut cf = NurbsFace::new(NurbsFaceId(next_id), result.corner.clone()).with_origin(
        FaceOrigin::Corner {
            radius: result.radius,
            faces: corner_ids,
        },
    );
    cf.material = mat;
    faces.push(cf);
}

/// Translate all CPs on a given edge by `delta` (planar face shrink, uniform).
pub(crate) fn shrink_face_edge(s: &mut NurbsSurface, edge: u8, delta: Vec3) -> Result<(), FilletError> {
    shrink_face_edge_tapered(s, edge, delta, delta)
}

/// Translate CPs on a face edge with **linearly interpolated** displacement from
/// `delta_start` (first endpoint of [`face_edge_endpoints`]) to `delta_end` (second).
///
/// For linear variable radius the tangency rail is a straight slanted line:
/// setback = r(t)/tan(θ/2) at each end — uniform max-r setback leaves a G0 crack
/// of size (r_max−r_min)/tan(θ/2) at the small-r free end.
pub(crate) fn shrink_face_edge_tapered(
    s: &mut NurbsSurface,
    edge: u8,
    delta_start: Vec3,
    delta_end: Vec3,
) -> Result<(), FilletError> {
    let (rows, cols) = s.grid_size();
    if rows < 2 || cols < 2 {
        return Err(FilletError::DegenerateEdge);
    }
    let blend = |t: f32| delta_start * (1.0 - t) + delta_end * t;
    match edge {
        0 => {
            // endpoints: [0][0] → [0][cols-1]
            let n = (cols - 1).max(1) as f32;
            for c in 0..cols {
                s.control_points[0][c].position += blend(c as f32 / n);
            }
        }
        1 => {
            // [0][cols-1] → [rows-1][cols-1]
            let n = (rows - 1).max(1) as f32;
            for r in 0..rows {
                s.control_points[r][cols - 1].position += blend(r as f32 / n);
            }
        }
        2 => {
            // [rows-1][0] → [rows-1][cols-1]
            let n = (cols - 1).max(1) as f32;
            for c in 0..cols {
                s.control_points[rows - 1][c].position += blend(c as f32 / n);
            }
        }
        3 => {
            // [0][0] → [rows-1][0]
            let n = (rows - 1).max(1) as f32;
            for r in 0..rows {
                s.control_points[r][0].position += blend(r as f32 / n);
            }
        }
        _ => return Err(FilletError::BadEdgeIndex),
    }
    Ok(())
}

/// Piecewise-linear radius lookup (sorted or unsorted `(t,r)` samples).
pub(crate) fn law_radius_at(samples: &[(f32, f32)], t: f32) -> f32 {
    Radius::Law(samples.to_vec()).at(t)
}

/// Clamped degree-1 knot vector for `n` control points.
pub(crate) fn degree1_clamped_knots(n: usize) -> Vec<f32> {
    let n = n.max(2);
    let mut k = vec![0.0, 0.0];
    for i in 1..n - 1 {
        k.push(i as f32 / (n - 1) as f32);
    }
    k.push(1.0);
    k.push(1.0);
    k
}

/// Resample a CP polyline to `n` points (linear in arc parameter).
pub(crate) fn resample_cp_polyline(cps: &[ControlPoint], n: usize) -> Vec<ControlPoint> {
    let n = n.max(2);
    if cps.len() == n {
        return cps.to_vec();
    }
    if cps.is_empty() {
        return vec![ControlPoint::new(Vec3::ZERO, 1.0); n];
    }
    if cps.len() == 1 {
        return vec![cps[0].clone(); n];
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let f = t * (cps.len() - 1) as f32;
        let j = (f.floor() as usize).min(cps.len() - 2);
        let u = (f - j as f32).clamp(0.0, 1.0);
        let p = cps[j].position.lerp(cps[j + 1].position, u);
        let w = cps[j].weight * (1.0 - u) + cps[j + 1].weight * u;
        out.push(ControlPoint::new(p, w));
    }
    out
}

/// Elevate the face grid so the given edge has at least `n_min` control points.
/// Planar bilinear faces need this so a multi-station law can bend the rail
/// (2 edge CPs can only represent a linear first→last taper).
///
/// After refine, the elevated direction is degree-1 with clamped knots so the
/// rail is an exact piecewise-linear polyline through the stations.
pub(crate) fn refine_face_edge_stations(s: &mut NurbsSurface, edge: u8, n_min: usize) -> Result<(), FilletError> {
    let n_min = n_min.max(2);
    let (rows, cols) = s.grid_size();
    if rows < 2 || cols < 2 {
        return Err(FilletError::DegenerateEdge);
    }
    match edge {
        0 | 2 => {
            // Edge runs along V (columns)
            if cols >= n_min {
                return Ok(());
            }
            let mut new_grid = Vec::with_capacity(rows);
            for r in 0..rows {
                new_grid.push(resample_cp_polyline(&s.control_points[r], n_min));
            }
            s.control_points = new_grid;
            s.degree_v = 1;
            s.knots_v = degree1_clamped_knots(n_min);
        }
        1 | 3 => {
            // Edge runs along U (rows)
            if rows >= n_min {
                return Ok(());
            }
            let cols_data: Vec<Vec<ControlPoint>> = (0..cols)
                .map(|c| {
                    let col: Vec<_> = (0..rows).map(|r| s.control_points[r][c].clone()).collect();
                    resample_cp_polyline(&col, n_min)
                })
                .collect();
            // cols_data[c][r_new] → transpose to control_points[r_new][c]
            let mut new_grid = vec![vec![ControlPoint::new(Vec3::ZERO, 1.0); cols]; n_min];
            for c in 0..cols {
                for r in 0..n_min {
                    new_grid[r][c] = cols_data[c][r].clone();
                }
            }
            s.control_points = new_grid;
            s.degree_u = 1;
            s.knots_u = degree1_clamped_knots(n_min);
        }
        _ => return Err(FilletError::BadEdgeIndex),
    }
    Ok(())
}

/// Multi-station (possibly non-monotone) setback rail: at each edge CP with
/// geometric parameter `t` along `edge_a0→edge_a1`, displace by
/// `setback_dir * (r(t)/tan_half)` where `r` is the law.
///
/// Refines the face edge to at least `samples.len()` CPs so an interior r peak
/// can bend the rail (bilinear 2-CP edges cannot).
pub(crate) fn shrink_face_edge_law(
    s: &mut NurbsSurface,
    edge: u8,
    setback_dir: Vec3,
    edge_a0: Vec3,
    edge_a1: Vec3,
    samples: &[(f32, f32)],
    tan_half: f32,
) -> Result<(), FilletError> {
    if samples.len() < 2 {
        return Err(FilletError::DegenerateEdge);
    }
    let tan_half = tan_half.max(1e-4);
    let n_min = samples.len().max(2);
    refine_face_edge_stations(s, edge, n_min)?;
    let (rows, cols) = s.grid_size();
    let elen2 = (edge_a1 - edge_a0).length_squared().max(1e-12);
    let t_of = |p: Vec3| ((p - edge_a0).dot(edge_a1 - edge_a0) / elen2).clamp(0.0, 1.0);
    let apply = |p: &mut Vec3| {
        let t = t_of(*p);
        let r = law_radius_at(samples, t).max(0.0);
        *p += setback_dir * (r / tan_half);
    };
    match edge {
        0 => {
            for c in 0..cols {
                apply(&mut s.control_points[0][c].position);
            }
        }
        1 => {
            for r in 0..rows {
                apply(&mut s.control_points[r][cols - 1].position);
            }
        }
        2 => {
            for c in 0..cols {
                apply(&mut s.control_points[rows - 1][c].position);
            }
        }
        3 => {
            for r in 0..rows {
                apply(&mut s.control_points[r][0].position);
            }
        }
        _ => return Err(FilletError::BadEdgeIndex),
    }
    Ok(())
}

/// True if selected edges at `vertex` involve 3 mutually orthogonal face normals.
#[allow(dead_code)]
pub(crate) fn vertex_has_orthogonal_triad(
    originals: &[NurbsFace],
    resolved: &[(usize, u8, usize, u8, Vec3, Vec3, bool)],
    vertex: Vec3,
) -> bool {
    orthogonal_triad_at_sel(originals, resolved, vertex).is_some()
}

/// Three mutually orthogonal faces at a solid vertex (from all solid edges).
pub(crate) fn solid_orthogonal_triad_at(
    originals: &[NurbsFace],
    vertex: Vec3,
) -> Option<([usize; 3], [Vec3; 3])> {
    let at = edges_at_vertex(originals, vertex);
    if at.len() != 3 {
        return None;
    }
    let mut face_ids = Vec::new();
    for &(af, ae) in &at {
        if !face_ids.contains(&af) {
            face_ids.push(af);
        }
        if let Ok((bf, _)) = find_orthogonal_neighbour(originals, af, ae) {
            if !face_ids.contains(&bf) {
                face_ids.push(bf);
            }
        }
    }
    if face_ids.len() != 3 {
        return None;
    }
    let mut normals = Vec::new();
    for &fi in &face_ids {
        let n = face_outward_normal(originals[fi].surface.as_ref()).ok()?;
        for prev in &normals {
            if n.dot(*prev).abs() > 0.15 {
                return None;
            }
        }
        normals.push(n);
    }
    if normals.len() != 3 {
        return None;
    }
    Some((
        [face_ids[0], face_ids[1], face_ids[2]],
        [normals[0], normals[1], normals[2]],
    ))
}

/// Collect up to 3 unique faces (and normals) from selected edges touching `vertex`.
pub(crate) fn orthogonal_triad_at_sel(
    originals: &[NurbsFace],
    resolved: &[(usize, u8, usize, u8, Vec3, Vec3, bool)],
    vertex: Vec3,
) -> Option<([usize; 3], [Vec3; 3])> {
    const EPS2: f32 = 1e-8;
    let mut face_ids = Vec::new();
    let mut normals = Vec::new();
    for &(fa, _ea, fb, _eb, a0, a1, _) in resolved {
        if (a0 - vertex).length_squared() >= EPS2 && (a1 - vertex).length_squared() >= EPS2 {
            continue;
        }
        for fi in [fa, fb] {
            if face_ids.contains(&fi) {
                continue;
            }
            let n = face_outward_normal(originals[fi].surface.as_ref()).ok()?;
            for prev in &normals {
                if n.dot(*prev).abs() > 0.15 {
                    return None;
                }
            }
            face_ids.push(fi);
            normals.push(n);
        }
    }
    if face_ids.len() != 3 || normals.len() != 3 {
        return None;
    }
    Some((
        [face_ids[0], face_ids[1], face_ids[2]],
        [normals[0], normals[1], normals[2]],
    ))
}



