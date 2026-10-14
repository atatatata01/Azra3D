//! Classification + radius law + planar solve rails (split from nurbs_fillet.rs).

use crate::nurbs::{ControlPoint, FaceOrigin, NurbsFace, NurbsFaceId, NurbsSurface};
use glam::Vec3;
use std::collections::BTreeMap;

use super::*;

/// Edge endpoints of a degree-1 bilinear face (local solid space).
/// Edge index: 0=u0, 1=v1, 2=u1, 3=v0 (matches app pick convention).
pub fn face_edge_endpoints(s: &NurbsSurface, edge: u8) -> Result<(Vec3, Vec3), FilletError> {
    let (rows, cols) = s.grid_size();
    if rows < 2 || cols < 2 {
        return Err(FilletError::DegenerateEdge);
    }
    let p = |r: usize, c: usize| s.control_points[r][c].position;
    Ok(match edge {
        0 => (p(0, 0), p(0, cols - 1)),               // u min row
        1 => (p(0, cols - 1), p(rows - 1, cols - 1)), // v max col
        2 => (p(rows - 1, 0), p(rows - 1, cols - 1)), // u max row
        3 => (p(0, 0), p(rows - 1, 0)),               // v min col
        _ => return Err(FilletError::BadEdgeIndex),
    })
}

/// Outward unit normal of a face at UV midpoint (planar faces: constant).
pub fn face_outward_normal(s: &NurbsSurface) -> Result<Vec3, FilletError> {
    let n = s.normal(0.5, 0.5);
    if n.length_squared() < 1e-10 {
        return Err(FilletError::NotPlanar);
    }
    Ok(n.normalize())
}

/// True if the surface is (within tol) a plane: degree ≤1×1, or sampled normals collinear.
pub fn surface_is_planar(s: &NurbsSurface) -> bool {
    if s.degree_u <= 1 && s.degree_v <= 1 {
        // Bilinear: check corner normals agree
        let samples = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)];
        let mut n0: Option<Vec3> = None;
        for &(u, v) in &samples {
            let n = s.normal(u, v);
            if n.length_squared() < 1e-10 {
                return false;
            }
            let n = n.normalize();
            if let Some(ref nref) = n0 {
                if n.dot(*nref).abs() < 0.999 {
                    return false;
                }
            } else {
                n0 = Some(n);
            }
        }
        return true;
    }
    // Higher degree: sample denser
    let mut n0: Option<Vec3> = None;
    for iu in 0..5 {
        for iv in 0..5 {
            let u = iu as f32 / 4.0;
            let v = iv as f32 / 4.0;
            let n = s.normal(u, v);
            if n.length_squared() < 1e-10 {
                return false;
            }
            let n = n.normalize();
            if let Some(ref nref) = n0 {
                if n.dot(*nref).abs() < 0.995 {
                    return false;
                }
            } else {
                n0 = Some(n);
            }
        }
    }
    true
}

/// Fitted Y-up cylinder from [`NurbsSurface::new_cylinder`] (or equivalent 2-row circular extrude).
#[derive(Clone, Debug)]
pub struct FittedCylinder {
    pub axis_origin: Vec3, // point on axis (mid-height for new_cylinder)
    pub axis_dir: Vec3,    // unit
    pub radius: f32,
    pub half_height: f32, // extent along ±axis from origin
}

/// Try to interpret `s` as a circular cylinder (name "Cylinder" or geometric extrude).
///
/// Fits from **evaluated** rings (not control points — rational circle CPs are off-curve).
pub fn try_fit_cylinder(s: &NurbsSurface) -> Option<FittedCylinder> {
    if surface_is_planar(s) {
        return None;
    }
    let n = 16usize;
    let mut bottom = Vec::with_capacity(n);
    let mut top = Vec::with_capacity(n);
    for i in 0..n {
        let v = i as f32 / n as f32;
        bottom.push(s.evaluate(0.0, v));
        top.push(s.evaluate(1.0, v));
    }
    // Axis ≈ mean of (top − bottom) samples
    let mut axis = Vec3::ZERO;
    for i in 0..n {
        axis += top[i] - bottom[i];
    }
    axis = axis.normalize_or_zero();
    if axis.length_squared() < 0.5 {
        return None;
    }
    let mut c0 = Vec3::ZERO;
    let mut c1 = Vec3::ZERO;
    for i in 0..n {
        c0 += bottom[i];
        c1 += top[i];
    }
    let nf = n as f32;
    c0 /= nf;
    c1 /= nf;
    let mid = (c0 + c1) * 0.5;
    let half_h = (c1 - c0).length() * 0.5;
    let mut r_sum = 0.0f32;
    for p in bottom.iter().chain(top.iter()) {
        let d = *p - mid;
        let radial = d - axis * d.dot(axis);
        r_sum += radial.length();
    }
    let radius = r_sum / (2.0 * nf);
    if radius < 1e-4 || half_h < 1e-4 {
        return None;
    }
    let mut r_var = 0.0f32;
    for p in bottom.iter().chain(top.iter()) {
        let d = *p - mid;
        let radial = d - axis * d.dot(axis);
        r_var += (radial.length() - radius).abs();
    }
    if r_var / (2.0 * nf) > radius * 0.05 {
        return None;
    }
    // Generators roughly parallel to axis
    let mut gen_err = 0.0f32;
    for i in 0..n {
        let g = (top[i] - bottom[i]).normalize_or_zero();
        gen_err += (1.0 - g.dot(axis).abs()).max(0.0);
    }
    if gen_err / nf > 0.05 {
        return None;
    }
    Some(FittedCylinder {
        axis_origin: mid,
        axis_dir: axis,
        radius,
        half_height: half_h,
    })
}

/// Analytic **cylinder + plane rim** fillet (FreeCAD: Toroid between Cylinder and Plane).
///
/// Y-up (or arbitrary-axis) cylinder of radius `cyl.radius`, plane perpendicular to
/// axis at the top (`top=true`) or bottom end. Fillet radius `r` must be `< cyl.radius`.
/// Returns the toroidal blend surface only (caller attaches FaceOrigin / shrinks).
///
/// Geometry: major = R−r, minor = r; tube 90° from radial-out (on cylinder) to
/// axial (on plane). Full 360° major circle.
pub fn make_cylinder_plane_rim_torus(
    cyl: &FittedCylinder,
    r: f32,
    top: bool,
) -> Result<NurbsSurface, FilletError> {
    let r = r.max(1e-5);
    let r_cyl = cyl.radius;
    if r >= r_cyl - 1e-4 {
        return Err(FilletError::RadiusTooLarge {
            max: (r_cyl * 0.95).max(1e-4),
        });
    }
    let ax = cyl.axis_dir.normalize_or_zero();
    if ax.length_squared() < 0.5 {
        return Err(FilletError::DegenerateEdge);
    }
    let major = r_cyl - r;
    let sign = if top { 1.0f32 } else { -1.0f32 };
    // Plane at mid ± half_height; tube centres sit r inset from plane along −sign*ax (into solid)
    let plane_pt = cyl.axis_origin + ax * (sign * cyl.half_height);
    let major_plane_pt = plane_pt - ax * (sign * r); // centers of rolling ball path

    // Full major circle CPs in plane through major_plane_pt ⟂ ax (reuse circle layout).
    // Build orthonormal e0,e1 in major plane.
    let e0 = if ax.cross(Vec3::X).length_squared() > 0.1 {
        ax.cross(Vec3::X).normalize()
    } else {
        ax.cross(Vec3::Z).normalize()
    };
    let e1 = ax.cross(e0).normalize();
    let w = W_MID;
    // 9 CP rational full circle control net (Piegl): axis-aligned CPs on radius `major`,
    // diagonal CPs at major·(±1,±1) with weight W_MID — same layout as `NurbsCurve::new_circle`.
    let circ_xy: [(f32, f32, f32); 9] = [
        (1.0, 0.0, 1.0),
        (1.0, 1.0, w),
        (0.0, 1.0, 1.0),
        (-1.0, 1.0, w),
        (-1.0, 0.0, 1.0),
        (-1.0, -1.0, w),
        (0.0, -1.0, 1.0),
        (1.0, -1.0, w),
        (1.0, 0.0, 1.0),
    ];
    let mut control_points = vec![vec![ControlPoint::new(Vec3::ZERO, 1.0); 9]; 3];
    for (j, &(cx, cy, wj)) in circ_xy.iter().enumerate() {
        let major_c = major_plane_pt + (e0 * cx + e1 * cy) * major;
        // Unit radial for tube frame (diagonal CPs: normalize (1,1))
        let rhat = (e0 * cx + e1 * cy).normalize_or_zero();
        let axial = ax * sign;
        let t0 = major_c + rhat * r;
        let t1 = major_c + (rhat + axial) * r;
        let t2 = major_c + axial * r;
        let tube = [(t0, 1.0f32), (t1, w), (t2, 1.0f32)];
        for (i, &(pos, wi)) in tube.iter().enumerate() {
            control_points[i][j] = ControlPoint::new(pos, wj * wi);
        }
    }
    Ok(NurbsSurface {
        control_points,
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v: vec![
            0.0, 0.0, 0.0, 0.25, 0.25, 0.5, 0.5, 0.75, 0.75, 1.0, 1.0, 1.0,
        ],
        degree_u: 2,
        degree_v: 2,
        name: "Toroid".into(),
        mesh_cache_id: None,
        tess_tolerance: 0.25,
    })
}

/// Fillet the circular rim of a Y-up (or fitted) cylinder against its end plane.
///
/// Builds only the toroidal blend (`name = "Toroid"`). Does not shrink supports —
/// use for oracle / solid assembly by the caller.
pub fn fillet_cylinder_plane_rim(
    cylinder: &NurbsSurface,
    r: f32,
    top: bool,
) -> Result<NurbsSurface, FilletError> {
    let cyl = try_fit_cylinder(cylinder).ok_or(FilletError::CurvedFace {
        reason: "not a fitted cylinder",
    })?;
    make_cylinder_plane_rim_torus(&cyl, r, top)
}

/// Apply cylinder–plane **rim** fillet to a 3-face cylinder solid
/// ([`NurbsSurface::new_cylinder_nurbs_faces`]: wall=0, top=1, bottom=2).
///
/// `wall_edge`: 0 = bottom rim, 2 = top rim (wall UV convention).
/// Shrinks wall half-height and cap circular outer trim; appends Toroid face.
pub fn apply_cylinder_rim_fillet_to_solid(
    faces: &mut Vec<NurbsFace>,
    wall_fi: usize,
    wall_edge: u8,
    radius: f32,
    mut next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    if wall_fi >= faces.len() {
        return Err(FilletError::BadEdgeIndex);
    }
    let top = match wall_edge {
        2 => true,
        0 => false,
        _ => {
            return Err(FilletError::CurvedFace {
                reason: "cylinder rim is wall edge 0 (bottom) or 2 (top)",
            });
        }
    };
    let wall_surf = faces[wall_fi].surface.as_ref().clone();
    let fitted = try_fit_cylinder(&wall_surf).ok_or(FilletError::CurvedFace {
        reason: "not a fitted cylinder",
    })?;
    let r = radius.max(1e-5);
    if r >= fitted.radius - 1e-4 {
        return Err(FilletError::RadiusTooLarge {
            max: (fitted.radius * 0.95).max(1e-4),
        });
    }
    if r >= fitted.half_height - 1e-4 {
        return Err(FilletError::RadiusTooLarge {
            max: (fitted.half_height * 0.95).max(1e-4),
        });
    }
    let toroid = make_cylinder_plane_rim_torus(&fitted, r, top)?;

    // Shrink wall: move the rim ring along axis into the solid by `r`.
    let ax = fitted.axis_dir.normalize_or_zero();
    let sign = if top { 1.0f32 } else { -1.0f32 };
    let mut wall = wall_surf;
    let row = if top {
        wall.control_points.len().saturating_sub(1)
    } else {
        0
    };
    if let Some(ring) = wall.control_points.get_mut(row) {
        for cp in ring.iter_mut() {
            cp.position -= ax * (sign * r);
        }
    }
    faces[wall_fi].surface = std::sync::Arc::new(wall);

    // Cap face: prefer geometric match (plane near rim), fall back to id layout 1/2.
    let plane_pt = fitted.axis_origin + ax * (sign * fitted.half_height);
    let mut cap_fi: Option<usize> = None;
    for (i, f) in faces.iter().enumerate() {
        if i == wall_fi {
            continue;
        }
        if !surface_is_planar(f.surface.as_ref()) {
            continue;
        }
        let p = f.surface.evaluate(0.5, 0.5);
        if (p - plane_pt).length() < fitted.radius * 0.25 + 0.05 {
            cap_fi = Some(i);
            break;
        }
    }
    if cap_fi.is_none() {
        // Layout from new_cylinder_nurbs_faces
        cap_fi = Some(if top { 1 } else { 2 });
    }
    let cap_fi = cap_fi.unwrap();
    if cap_fi >= faces.len() {
        return Err(FilletError::DegenerateEdge);
    }
    // Shrink circular outer trim: UV radius 0.5 → 0.5 * (R−r)/R
    let uv_r = 0.5 * ((fitted.radius - r) / fitted.radius).clamp(0.05, 1.0);
    let c = glam::Vec2::new(0.5, 0.5);
    let pi = std::f32::consts::PI;
    faces[cap_fi].trims = vec![crate::nurbs::TrimLoop {
        is_outer: true,
        curves: vec![
            crate::nurbs::TrimCurve2d::Arc {
                centre: c,
                radius: uv_r,
                t0: 0.0,
                t1: pi,
            },
            crate::nurbs::TrimCurve2d::Arc {
                centre: c,
                radius: uv_r,
                t0: pi,
                t1: 2.0 * pi,
            },
        ],
    }];

    let between = [faces[wall_fi].id, faces[cap_fi].id];
    let mut fillet_face = NurbsFace::new(NurbsFaceId(next_id), toroid).with_origin(FaceOrigin::Fillet {
        radius: r,
        between,
    });
    fillet_face.material = faces[wall_fi].material.clone();
    faces.push(fillet_face);
    next_id += 1;
    let _ = next_id;
    Ok(FilletOutcome {
        faces: faces.clone(),
        faulty_vertices: Vec::new(),
    })
}

/// Max safe radius: approach half the shorter face edge (Plasticity allows nearly this far).
/// Unit cube (side 1) → ~0.495. Kernel still refuses collapse via RadiusTooLarge.
pub fn max_fillet_radius(a: &NurbsSurface, b: &NurbsSurface) -> f32 {
    let extent = |s: &NurbsSurface| {
        let (rows, cols) = s.grid_size();
        if rows < 2 || cols < 2 {
            return 0.0;
        }
        let p00 = s.control_points[0][0].position;
        let p10 = s.control_points[rows - 1][0].position;
        let p01 = s.control_points[0][cols - 1].position;
        let lu = (p10 - p00).length();
        let lv = (p01 - p00).length();
        // Plasticity-scale: allow almost half-edge (was 0.49 / app multi-edge killed it further)
        lu.min(lv) * 0.495
    };
    extent(a).min(extent(b)).max(1e-4)
}

/// Face extent perpendicular to a solid edge (how far the face can set back).
/// For bilinear quads: length of the edges leaving the fillet edge endpoints.
pub fn face_setback_capacity(s: &NurbsSurface, edge: u8) -> f32 {
    let Ok((a0, a1)) = face_edge_endpoints(s, edge) else {
        return 0.0;
    };
    let elen = (a1 - a0).length();
    if elen < 1e-8 {
        return 0.0;
    }
    let (rows, cols) = s.grid_size();
    if rows < 2 || cols < 2 {
        return 0.0;
    }
    // Opposite edge endpoints
    let opp = match edge {
        0 => face_edge_endpoints(s, 2),
        2 => face_edge_endpoints(s, 0),
        1 => face_edge_endpoints(s, 3),
        3 => face_edge_endpoints(s, 1),
        _ => return 0.0,
    };
    let Ok((b0, b1)) = opp else {
        return 0.0;
    };
    // Depth = average distance from fillet edge to opposite edge (for rectangles = side length)
    let d0 = (b0 - a0).length().min((b1 - a0).length());
    let d1 = (b0 - a1).length().min((b1 - a1).length());
    // Also try paired correspondence
    let d_pair = ((b0 - a0).length() + (b1 - a1).length()) * 0.5;
    d0.min(d1).min(d_pair).max(0.0)
}

/// Find the orthogonal neighbour face+edge sharing the same solid edge as `(fa, ea)`.
pub fn find_orthogonal_neighbour(
    faces: &[NurbsFace],
    fa: usize,
    ea: u8,
) -> Result<(usize, u8), FilletError> {
    if fa >= faces.len() {
        return Err(FilletError::BadEdgeIndex);
    }
    let sa = faces[fa].surface.as_ref();
    let (a0, a1) = face_edge_endpoints(sa, ea)?;
    let n0 = face_outward_normal(sa)?;
    const EPS2: f32 = 1e-6;
    let same = |p: Vec3, q: Vec3| (p - q).length_squared() < EPS2;
    for (fb, face) in faces.iter().enumerate() {
        if fb == fa {
            continue;
        }
        let sb = face.surface.as_ref();
        for eb in 0u8..4 {
            if let Ok((b0, b1)) = face_edge_endpoints(sb, eb) {
                if (same(a0, b0) && same(a1, b1)) || (same(a0, b1) && same(a1, b0)) {
                    if let Ok(n1) = face_outward_normal(sb) {
                        // Shared solid edge with a usable dihedral (not parallel/anti-parallel).
                        // Was |n·n|<0.15 (≈90° only); non-ortho acute/obtuse need wider gate.
                        if n0.dot(n1).abs() < 0.98 {
                            return Ok((fb, eb));
                        }
                    }
                }
            }
        }
    }
    Err(FilletError::DegenerateEdge)
}

/// Max safe radius for one solid edge (face extent, setback depth, **and** edge length).
///
/// Caps so a short edge / thin face cannot request r that collapses the spine or
/// zeros a face (the `NoSeedSolution` failure class). For θ≈90°,
/// setback ≈ r, so face depth × 0.495 bounds r the same way.
pub fn max_fillet_radius_for_edge(
    faces: &[NurbsFace],
    fa: usize,
    ea: u8,
) -> Result<f32, FilletError> {
    let (fb, eb) = find_orthogonal_neighbour(faces, fa, ea)?;
    let sa = faces[fa].surface.as_ref();
    let sb = faces[fb].surface.as_ref();
    let face_max = max_fillet_radius(sa, sb);
    let (a0, a1) = face_edge_endpoints(sa, ea)?;
    let elen = (a1 - a0).length();
    let edge_max = (elen * 0.495).max(1e-4);
    // Depth of each face away from the shared edge (setback direction capacity).
    let depth_a = face_setback_capacity(sa, ea);
    let depth_b = face_setback_capacity(sb, eb);
    let depth_max = (depth_a.min(depth_b) * 0.495).max(1e-4);
    Ok(face_max.min(edge_max).min(depth_max))
}

/// True when `radius` would collapse a free (untrimmed) edge spine.
pub fn edge_too_short_for_radius(elen: f32, radius: f32) -> bool {
    elen < 1e-4 || radius > elen * 0.495 + 1e-6
}

/// Deduplicate solid edges by midpoint (one tag face per geometric edge).
pub fn dedupe_solid_edges(faces: &[NurbsFace], edges: &[(usize, u8)]) -> Vec<(usize, u8)> {
    const EPS2: f32 = 1e-8;
    let mut out = Vec::new();
    let mut mids = Vec::new();
    for &(fi, ei) in edges {
        if fi >= faces.len() {
            continue;
        }
        let Ok((a, b)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) else {
            continue;
        };
        let mid = (a + b) * 0.5;
        if mids.iter().any(|m: &Vec3| (*m - mid).length_squared() < EPS2) {
            continue;
        }
        mids.push(mid);
        out.push((fi, ei));
    }
    out
}

/// All solid edges (face tag, edge id) incident on `vertex`.
pub fn edges_at_vertex(faces: &[NurbsFace], vertex: Vec3) -> Vec<(usize, u8)> {
    const EPS2: f32 = 1e-8;
    let mut raw = Vec::new();
    for (fi, face) in faces.iter().enumerate() {
        for ei in 0u8..4 {
            if let Ok((a, b)) = face_edge_endpoints(face.surface.as_ref(), ei) {
                if (a - vertex).length_squared() < EPS2 || (b - vertex).length_squared() < EPS2 {
                    raw.push((fi, ei));
                }
            }
        }
    }
    dedupe_solid_edges(faces, &raw)
}

pub(crate) fn edge_in_set(faces: &[NurbsFace], set: &[(usize, u8)], fa: usize, ea: u8) -> bool {
    const EPS2: f32 = 1e-8;
    let Ok((a0, a1)) = face_edge_endpoints(faces[fa].surface.as_ref(), ea) else {
        return false;
    };
    let mid = (a0 + a1) * 0.5;
    set.iter().any(|&(sf, se)| {
        face_edge_endpoints(faces[sf].surface.as_ref(), se)
            .map(|(p0, p1)| ((p0 + p1) * 0.5 - mid).length_squared() < EPS2)
            .unwrap_or(false)
    })
}

/// If the user already selected **≥2 solid edges that all bound the same planar face**,
/// expand to that face's full outer boundary (Plasticity-style face-loop fillet).
///
/// Example: 2 of 4 top-face edges → all 4 top-loop edges (FreeCAD `top_loop_r02`).
/// Does **not** fire for a single edge (keeps free-end single-edge semantics).
/// Does **not** add verticals (that is [`complete_corners_in_selection`]).
pub fn expand_planar_face_boundary_loop(
    faces: &[NurbsFace],
    edges: &[(usize, u8)],
) -> Vec<(usize, u8)> {
    let selected = dedupe_solid_edges(faces, edges);
    if selected.len() < 2 {
        return selected;
    }
    // Count how often each face appears as an owner of a selected solid edge.
    let mut face_hits: BTreeMap<usize, u32> = BTreeMap::new();
    for &(fa, ea) in &selected {
        *face_hits.entry(fa).or_insert(0) += 1;
        if let Ok((fb, _)) = find_orthogonal_neighbour(faces, fa, ea) {
            *face_hits.entry(fb).or_insert(0) += 1;
        }
    }
    // Prefer the planar face hit by every selected edge (≥ selected.len() hits).
    let mut best: Option<(usize, u32)> = None;
    for (&fi, &hits) in &face_hits {
        if fi >= faces.len() {
            continue;
        }
        if !surface_is_planar(faces[fi].surface.as_ref()) {
            continue;
        }
        if hits >= selected.len() as u32 {
            if best.map(|(_, h)| hits > h).unwrap_or(true) {
                best = Some((fi, hits));
            }
        }
    }
    let Some((face_i, _)) = best else {
        return selected;
    };
    // All 4 UV boundary edges of this face, mapped to solid (deduped) tags.
    let mut loop_edges = Vec::new();
    for ei in 0u8..4 {
        loop_edges.push((face_i, ei));
    }
    let expanded = dedupe_solid_edges(faces, &loop_edges);
    if expanded.len() >= selected.len() {
        expanded
    } else {
        selected
    }
}

/// Complete orthogonal cube corners where the user already selected **≥2 of 3** edges.
///
/// - 1 edge alone → unchanged (single-edge fillet)
/// - 2 edges of a corner → adds the missing third → sphere octant path
/// - Never walks the whole cube from a single edge (that cascade was rejected)
///
/// Fixed-point over vertices; safe for a top-loop (4 edges) → completes 4 verticals.
pub fn complete_corners_in_selection(
    faces: &[NurbsFace],
    edges: &[(usize, u8)],
) -> Vec<(usize, u8)> {
    const EPS2: f32 = 1e-8;
    let mut set = dedupe_solid_edges(faces, edges);
    for _ in 0..16 {
        let mut added = false;
        // Snapshot endpoints of current set
        let mut verts: Vec<Vec3> = Vec::new();
        for &(fi, ei) in &set {
            if fi >= faces.len() {
                continue;
            }
            let Ok((a, b)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) else {
                continue;
            };
            for p in [a, b] {
                if !verts.iter().any(|v| (*v - p).length_squared() < EPS2) {
                    verts.push(p);
                }
            }
        }
        for v in verts {
            let at = edges_at_vertex(faces, v);
            if at.len() != 3 {
                continue;
            }
            // Require orthogonal solid corner (3 unique faces / normals)
            let mut face_ids = Vec::new();
            for &(af, ae) in &at {
                if !face_ids.contains(&af) {
                    face_ids.push(af);
                }
                if let Ok((bf, _)) = find_orthogonal_neighbour(faces, af, ae) {
                    if !face_ids.contains(&bf) {
                        face_ids.push(bf);
                    }
                }
            }
            if face_ids.len() != 3 {
                continue;
            }
            let mut ns = Vec::new();
            let mut ortho = true;
            for &fi in &face_ids {
                let Ok(n) = face_outward_normal(faces[fi].surface.as_ref()) else {
                    ortho = false;
                    break;
                };
                for prev in &ns {
                    if n.dot(*prev).abs() > 0.15 {
                        ortho = false;
                        break;
                    }
                }
                if !ortho {
                    break;
                }
                ns.push(n);
            }
            if !ortho || ns.len() != 3 {
                continue;
            }
            let n_sel = at
                .iter()
                .filter(|&&(fa, ea)| edge_in_set(faces, &set, fa, ea))
                .count();
            if n_sel >= 2 && n_sel < 3 {
                for e in at {
                    if !edge_in_set(faces, &set, e.0, e.1) {
                        set.push(e);
                        added = true;
                    }
                }
            }
        }
        if !added {
            break;
        }
        set = dedupe_solid_edges(faces, &set);
    }
    dedupe_solid_edges(faces, &set)
}

/// Default selection growth: extend the selection along a **tangent contour**.
///
/// At each endpoint of a selected edge, add any other solid edge that:
/// 1. shares that vertex,
/// 2. is **G1-collinear** with the selected edge (`|dir_a · dir_b| ≥ collinear_tol`),
/// 3. is a convex exterior dihedral (same gate as the planar fillet).
///
/// This is **not** corner completion (orthogonal triad → third edge). On a cube,
/// 90° corners never expand — only true smooth/collinear spine continuations do
/// (e.g. multi-segment coplanar hard edge, discretized cylinder rim).
pub fn expand_tangent_edge_chain(
    faces: &[NurbsFace],
    edges: &[(usize, u8)],
) -> Vec<(usize, u8)> {
    expand_tangent_edge_chain_tol(faces, edges, 0.98)
}

/// Same as [`expand_tangent_edge_chain`] with an explicit collinearity threshold on
/// `|edge_dir · other_dir|` (1 = perfect collinear).
pub fn expand_tangent_edge_chain_tol(
    faces: &[NurbsFace],
    edges: &[(usize, u8)],
    collinear_tol: f32,
) -> Vec<(usize, u8)> {
    let col = collinear_tol.clamp(0.5, 1.0);
    let mut set = dedupe_solid_edges(faces, edges);
    for _ in 0..64 {
        let mut added = false;
        let snapshot = set.clone();
        for &(fi, ei) in &snapshot {
            if fi >= faces.len() {
                continue;
            }
            let Ok((a0, a1)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) else {
                continue;
            };
            let d_sel = (a1 - a0).normalize_or_zero();
            if d_sel.length_squared() < 0.5 {
                continue;
            }
            // Only continue if this edge is filletable (convex exterior pair).
            let Ok((fb, _)) = find_orthogonal_neighbour(faces, fi, ei) else {
                continue;
            };
            if !edge_is_convex_exterior(
                faces[fi].surface.as_ref(),
                faces[fb].surface.as_ref(),
            ) {
                continue;
            }
            for p in [a0, a1] {
                for &(of, oe) in &edges_at_vertex(faces, p) {
                    if edge_in_set(faces, &set, of, oe) {
                        continue;
                    }
                    let Ok((b0, b1)) = face_edge_endpoints(faces[of].surface.as_ref(), oe) else {
                        continue;
                    };
                    let d_o = (b1 - b0).normalize_or_zero();
                    if d_o.length_squared() < 0.5 {
                        continue;
                    }
                    if d_sel.dot(d_o).abs() < col {
                        continue; // not collinear — cube corners stay out
                    }
                    let Ok((ob, _)) = find_orthogonal_neighbour(faces, of, oe) else {
                        continue;
                    };
                    if !edge_is_convex_exterior(
                        faces[of].surface.as_ref(),
                        faces[ob].surface.as_ref(),
                    ) {
                        continue;
                    }
                    set.push((of, oe));
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
        set = dedupe_solid_edges(faces, &set);
    }
    dedupe_solid_edges(faces, &set)
}

/// Apply fillets with a [`Radius`] law.
pub fn apply_edges_fillet_batch_radius(
    faces: &mut Vec<NurbsFace>,
    edges: &[(usize, u8)],
    radius: &Radius,
    next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    match radius {
        Radius::Const(r) => apply_edges_fillet_batch_outcome(faces, edges, *r, next_id),
        Radius::Linear { r0, r1 } => {
            apply_edges_fillet_batch_linear(faces, edges, *r0, *r1, next_id)
        }
        Radius::Law(samples) => apply_edges_fillet_batch_law(faces, edges, samples, next_id),
    }
}

/// Per-edge constant radii (several contours, one radius each).
///
/// `edge_radii` is `(face, edge, radius)` — duplicate solid edges are deduped (first wins).
/// At an Uniform 3-edge vertex: **sphere only if all three radii match**; unequal → free-end
/// cylinders + soft [`FilletError::UnequalRadiusCorner`].
pub fn apply_edges_fillet_batch_edge_radii(
    faces: &mut Vec<NurbsFace>,
    edge_radii: &[(usize, u8, f32)],
    next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    if edge_radii.is_empty() {
        return Err(FilletError::DegenerateEdge);
    }
    // Dedupe solid edges, keep first radius.
    let tags: Vec<(usize, u8)> = edge_radii.iter().map(|&(f, e, _)| (f, e)).collect();
    let selected = dedupe_solid_edges(faces, &tags);
    if selected.is_empty() {
        return Err(FilletError::DegenerateEdge);
    }
    let mut radii = Vec::with_capacity(selected.len());
    for &(fa, ea) in &selected {
        let r = edge_radii
            .iter()
            .find(|&&(f, e, _)| edge_in_set(faces, &[(fa, ea)], f, e))
            .map(|t| t.2)
            .unwrap_or(1e-5)
            .max(1e-5);
        radii.push(r);
    }
    apply_edges_fillet_batch_with_radii(faces, &selected, &radii, next_id)
}

/// Linear variable-radius fillet (radius varies linearly from `r0` to `r1`) for planar dihedrals.
///
/// Each edge gets a loft of two circular arcs (r0 at start endpoint of face-A edge,
/// r1 at end). Multi-edge: same (r0,r1) per edge along that edge's orientation.
///
/// **Corners (Tier 1.10):** Uniform triad with **equal** endpoint radius at the vertex
/// → spherical octant (FreeCAD still emits Sphere when r matches at the join). Unequal
/// endpoint radii → free ends + soft [`FilletError::UnequalRadiusCorner`].
pub fn apply_edges_fillet_batch_linear(
    faces: &mut Vec<NurbsFace>,
    edges: &[(usize, u8)],
    r0: f32,
    r1: f32,
    mut next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    let r0 = r0.max(1e-5);
    let r1 = r1.max(1e-5);
    let r_max = r0.max(r1);
    let selected = dedupe_solid_edges(faces, edges);
    if selected.is_empty() {
        return Err(FilletError::DegenerateEdge);
    }
    let mut max_ok = f32::MAX;
    for &(fa, ea) in &selected {
        max_ok = max_ok.min(max_fillet_radius_for_edge(faces, fa, ea)?);
    }
    if r_max > max_ok + 1e-6 {
        return Err(FilletError::RadiusTooLarge { max: max_ok });
    }

    let originals: Vec<NurbsFace> = faces.clone();
    let mut working: Vec<Option<NurbsSurface>> = originals
        .iter()
        .map(|f| Some(f.surface.as_ref().clone()))
        .collect();
    let mut faces_trims: Vec<Vec<crate::nurbs::TrimLoop>> =
        originals.iter().map(|f| f.trims.clone()).collect();
    let mat0 = faces
        .first()
        .map(|f| f.material.clone())
        .unwrap_or_default();
    let mut added = 0usize;
    let mut faulty_vertices = Vec::new();
    const EPS2: f32 = 1e-8;
    // (fa,ea,fb,eb,a0,a1,convex)
    let mut resolved: Vec<(usize, u8, usize, u8, Vec3, Vec3, bool)> = Vec::new();

    for &(fa, ea) in &selected {
        let (fb, eb) = find_orthogonal_neighbour(&originals, fa, ea)?;
        let sa = originals[fa].surface.as_ref();
        let sb = originals[fb].surface.as_ref();
        let convex = match edge_concavity(&originals, fa, ea) {
            EdgeConcavity::Convex => true,
            EdgeConcavity::Concave => false,
            EdgeConcavity::Invalid => {
                return Err(unsupported_for_non_convex_edge(&originals, fa, ea));
            }
        };
        let res = fillet_planar_edge_linear(sa, ea, sb, eb, r0, r1, convex)?;
        let na = face_outward_normal(sa)?;
        let nb = face_outward_normal(sb)?;
        let cos_n = na.dot(nb).clamp(-1.0, 1.0);
        let tan_half = (cos_n.acos() * 0.5).max(1e-3).tan();
        let s0 = r0 / tan_half;
        let s1 = r1 / tan_half;
        let s = if convex { -1.0f32 } else { 1.0f32 };
        if let Some(ref mut wa) = working[fa] {
            shrink_face_edge_tapered(wa, ea, s * nb * s0, s * nb * s1)?;
        }
        let (a0, a1) = face_edge_endpoints(sa, ea)?;
        let (b0, _b1) = face_edge_endpoints(sb, eb)?;
        let (sb0, sb1) = if (b0 - a0).length_squared() <= (b0 - a1).length_squared() {
            (s0, s1)
        } else {
            (s1, s0)
        };
        if let Some(ref mut wb) = working[fb] {
            shrink_face_edge_tapered(wb, eb, s * na * sb0, s * na * sb1)?;
        }
        resolved.push((fa, ea, fb, eb, a0, a1, convex));
        let between = [originals[fa].id, originals[fb].id];
        let mut fillet_face = NurbsFace::new(NurbsFaceId(next_id), res.fillet).with_origin(
            FaceOrigin::FilletLinear { r0, r1, between },
        );
        fillet_face.material = faces
            .get(fa)
            .map(|f| f.material.clone())
            .unwrap_or(mat0.clone());
        faces.push(fillet_face);
        next_id += 1;
        added += 1;
    }

    // Uniform triad: sphere if linear law gives equal r at the vertex on all three edges.
    let mut sphere_verts: Vec<(Vec3, f32)> = Vec::new();
    for p in unique_resolved_vertices(&resolved) {
        let at: Vec<_> = resolved
            .iter()
            .filter(|e| (e.4 - p).length_squared() < EPS2 || (e.5 - p).length_squared() < EPS2)
            .collect();
        if at.len() != 3 || !at.iter().all(|e| e.6) {
            continue; // need 3 convex selected
        }
        let rs = [
            linear_radius_at_vertex(at[0].4, at[0].5, p, r0, r1),
            linear_radius_at_vertex(at[1].4, at[1].5, p, r0, r1),
            linear_radius_at_vertex(at[2].4, at[2].5, p, r0, r1),
        ];
        let rmin = rs[0].min(rs[1]).min(rs[2]);
        let rmax = rs[0].max(rs[1]).max(rs[2]);
        if rmax - rmin > 1e-4 {
            faulty_vertices.push((p, FilletError::UnequalRadiusCorner { radii: rs }));
            continue;
        }
        // Ortho solid triad only
        if solid_orthogonal_triad_at(&originals, p).is_some()
            || orthogonal_triad_at_sel(&originals, &resolved, p).is_some()
        {
            sphere_verts.push((p, rmin));
        }
    }
    let is_sphere = |p: Vec3| {
        sphere_verts
            .iter()
            .any(|(c, _)| (*c - p).length_squared() < EPS2)
    };
    // Free-end notches only where not a sphere join
    for &(fa, _ea, fb, _eb, a0, a1, convex) in &resolved {
        let sa = originals[fa].surface.as_ref();
        let sb = originals[fb].surface.as_ref();
        let Ok(na) = face_outward_normal(sa) else {
            continue;
        };
        let Ok(nb) = face_outward_normal(sb) else {
            continue;
        };
        for (endpoint, r_end) in [(a0, r0), (a1, r1)] {
            if is_sphere(endpoint) {
                continue;
            }
            let n_sel = resolved
                .iter()
                .filter(|e| {
                    (e.4 - endpoint).length_squared() < EPS2
                        || (e.5 - endpoint).length_squared() < EPS2
                })
                .count();
            // Same rule as const batch: free-end notches for n_sel 1 or pure TwoCorner (2).
            if n_sel == 0 || n_sel >= 3 {
                continue;
            }
            add_side_face_notch_trim(
                &mut working,
                &mut faces_trims,
                &originals,
                fa,
                fb,
                endpoint,
                na,
                nb,
                r_end,
                convex,
            );
        }
    }
    for (vertex, cr) in sphere_verts {
        let (face_ids, normals) = match solid_orthogonal_triad_at(&originals, vertex) {
            Some(t) => t,
            None => match orthogonal_triad_at_sel(&originals, &resolved, vertex) {
                Some(t) => t,
                None => continue,
            },
        };
        let centre =
            sphere_centre_at_vertex(vertex, normals[0], normals[1], normals[2], cr);
        let Ok(surf) = make_spherical_octant(centre, cr, normals[0], normals[1], normals[2])
        else {
            continue;
        };
        let corner_faces = [
            originals[face_ids[0]].id,
            originals[face_ids[1]].id,
            originals[face_ids[2]].id,
        ];
        let mut cf =
            NurbsFace::new(NurbsFaceId(next_id), surf).with_origin(FaceOrigin::Corner {
                radius: cr,
                faces: corner_faces,
            });
        cf.material = mat0.clone();
        faces.push(cf);
        next_id += 1;
        added += 1;
    }

    for (i, w) in working.into_iter().enumerate() {
        if let Some(surf) = w {
            faces[i].surface = std::sync::Arc::new(surf);
        }
        if i < faces_trims.len() {
            faces[i].trims = faces_trims[i].clone();
        }
    }
    if added == 0 {
        return Err(FilletError::DegenerateEdge);
    }
    Ok(FilletOutcome {
        faces: faces.clone(),
        faulty_vertices,
    })
}

/// Apply fillets for a set of solid edges from **original** geometry (no sequential mutate).
///
/// **Only the edges you pass in** — never adds unselected edges (owner: 2 top edges must
/// not pull in a vertical). For a full sphere corner, select all 3 concurrent edges yourself.
///
/// - **1 edge** â€” cylinder + side-face Arc trims at free ends
/// - **2 edges at a vertex** â€” 2 cylinders only (a sphere appears only once all 3 are selected; matches the FreeCAD oracle)
/// - **3 concurrent selected edges** â€” spherical octant + cylinders trimmed at that vertex
/// - **N free ends** â€” Arc notch per free end (e.g. 4 verticals â†’ 4 notches on top/bottom)
///
/// Not yet: curved faces (cylinder body), unequal radii, non-90Â°, `JoinPatch`/`LoneEdge`.
///
/// Returns number of new faces appended (edge fillets + corners).

/// Piecewise-linear radius law along each edge (samples of `(u, radius)`).
/// Samples `(t, r)` with t∈[0,1] along the edge; lofts circular arcs at each station.
/// True if radius samples (any t-order) are **monotone in r** after sorting by t:
/// non-decreasing or non-increasing (constant allowed). Interior peaks/valleys → false.
pub fn radius_law_is_monotone(samples: &[(f32, f32)]) -> bool {
    if samples.len() < 2 {
        return samples.len() == 1;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut non_dec = true;
    let mut non_inc = true;
    for w in sorted.windows(2) {
        let dr = w[1].1 - w[0].1;
        if dr < -1e-6 {
            non_dec = false;
        }
        if dr > 1e-6 {
            non_inc = false;
        }
    }
    non_dec || non_inc
}

pub fn apply_edges_fillet_batch_law(
    faces: &mut Vec<NurbsFace>,
    edges: &[(usize, u8)],
    samples: &[(f32, f32)],
    mut next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    if samples.len() < 2 {
        return Err(FilletError::NoSeedSolution);
    }
    // Non-monotone OK: per-station bent rail via shrink_face_edge_law.
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let r_max = sorted
        .iter()
        .map(|(_, r)| *r)
        .fold(0.0f32, f32::max)
        .max(1e-5);
    let r_start = sorted.first().map(|p| p.1).unwrap_or(r_max).max(1e-5);
    let r_end = sorted.last().map(|p| p.1).unwrap_or(r_max).max(1e-5);
    let selected = dedupe_solid_edges(faces, edges);
    if selected.is_empty() {
        return Err(FilletError::DegenerateEdge);
    }
    let mut max_ok = f32::MAX;
    for &(fa, ea) in &selected {
        max_ok = max_ok.min(max_fillet_radius_for_edge(faces, fa, ea)?);
    }
    if r_max > max_ok + 1e-6 {
        return Err(FilletError::RadiusTooLarge { max: max_ok });
    }

    let originals: Vec<NurbsFace> = faces.clone();
    let mut working: Vec<Option<NurbsSurface>> = originals
        .iter()
        .map(|f| Some(f.surface.as_ref().clone()))
        .collect();
    let mut faces_trims: Vec<Vec<crate::nurbs::TrimLoop>> =
        originals.iter().map(|f| f.trims.clone()).collect();
    let mat0 = faces
        .first()
        .map(|f| f.material.clone())
        .unwrap_or_default();
    let mut added = 0usize;

    for &(fa, ea) in &selected {
        let (fb, eb) = find_orthogonal_neighbour(&originals, fa, ea)?;
        let sa = originals[fa].surface.as_ref();
        let sb = originals[fb].surface.as_ref();
        let convex = match edge_concavity(&originals, fa, ea) {
            EdgeConcavity::Convex => true,
            EdgeConcavity::Concave => false,
            EdgeConcavity::Invalid => {
                return Err(unsupported_for_non_convex_edge(&originals, fa, ea));
            }
        };
        let res = fillet_planar_edge_law(sa, ea, sb, eb, &sorted, convex)?;
        let na = face_outward_normal(sa)?;
        let nb = face_outward_normal(sb)?;
        let cos_n = na.dot(nb).clamp(-1.0, 1.0);
        let tan_half = (cos_n.acos() * 0.5).max(1e-3).tan();
        let s = if convex { -1.0f32 } else { 1.0f32 };
        let (a0, a1) = face_edge_endpoints(sa, ea)?;
        // Bent multi-station rail on working faces (multi-edge safe; matches res.face_*).
        if let Some(ref mut wa) = working[fa] {
            shrink_face_edge_law(wa, ea, s * nb, a0, a1, &sorted, tan_half)?;
        }
        if let Some(ref mut wb) = working[fb] {
            shrink_face_edge_law(wb, eb, s * na, a0, a1, &sorted, tan_half)?;
        }
        for (endpoint, r_end) in [(a0, r_start), (a1, r_end)] {
            add_side_face_notch_trim(
                &mut working,
                &mut faces_trims,
                &originals,
                fa,
                fb,
                endpoint,
                na,
                nb,
                r_end.max(1e-5),
                convex,
            );
        }
        let between = [originals[fa].id, originals[fb].id];
        let mut fillet_face = NurbsFace::new(NurbsFaceId(next_id), res.fillet).with_origin(
            FaceOrigin::FilletLaw {
                samples: sorted.clone(),
                between,
            },
        );
        fillet_face.material = faces
            .get(fa)
            .map(|f| f.material.clone())
            .unwrap_or(mat0.clone());
        faces.push(fillet_face);
        next_id += 1;
        added += 1;
    }

    for (i, w) in working.into_iter().enumerate() {
        if let Some(surf) = w {
            faces[i].surface = std::sync::Arc::new(surf);
        }
        if i < faces_trims.len() {
            faces[i].trims = faces_trims[i].clone();
        }
    }
    if added == 0 {
        return Err(FilletError::DegenerateEdge);
    }
    Ok(FilletOutcome {
        faces: faces.clone(),
        faulty_vertices: Vec::new(),
    })
}

pub fn apply_edges_fillet_batch(
    faces: &mut Vec<NurbsFace>,
    edges: &[(usize, u8)],
    radius: f32,
    next_id: u64,
) -> Result<usize, FilletError> {
    let n0 = faces.len();
    let out = apply_edges_fillet_batch_outcome(faces, edges, radius, next_id)?;
    if !out.faulty_vertices.is_empty() {
        return Err(out.faulty_vertices[0].1.clone());
    }
    Ok(faces.len().saturating_sub(n0))
}

/// Same as [`apply_edges_fillet_batch`] but returns [`FilletOutcome`] (partial results + faults).
///
/// On hard failure before any geometry is written, returns `Err`. On soft/per-vertex failure,
/// returns `Ok(FilletOutcome { faulty_vertices: ... })` with whatever faces were built.
pub fn apply_edges_fillet_batch_outcome(
    faces: &mut Vec<NurbsFace>,
    edges: &[(usize, u8)],
    radius: f32,
    next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    let selected = dedupe_solid_edges(faces, edges);
    if selected.is_empty() {
        return Err(FilletError::DegenerateEdge);
    }
    let r = radius.max(1e-5);
    let radii = vec![r; selected.len()];
    apply_edges_fillet_batch_with_radii(faces, &selected, &radii, next_id)
}

/// Equal- or per-edge constant radii on already-deduped solid edges.
///
/// Parallel `radii[i]` applies to `selected[i]`. Uniform triad → sphere only when the
/// three edge radii match; otherwise free ends + soft [`FilletError::UnequalRadiusCorner`].
pub fn apply_edges_fillet_batch_with_radii(
    faces: &mut Vec<NurbsFace>,
    selected: &[(usize, u8)],
    radii: &[f32],
    mut next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    let mut faulty_vertices: Vec<(Vec3, FilletError)> = Vec::new();
    if selected.is_empty() || radii.len() != selected.len() {
        return Err(FilletError::DegenerateEdge);
    }
    let mut max_ok = f32::MAX;
    for &(fa, ea) in selected {
        max_ok = max_ok.min(max_fillet_radius_for_edge(faces, fa, ea)?);
    }
    for &r in radii {
        if r.max(1e-5) > max_ok + 1e-6 {
            return Err(FilletError::RadiusTooLarge { max: max_ok });
        }
    }

    let originals: Vec<NurbsFace> = faces.clone();
    const EPS2: f32 = 1e-8;

    // Resolve each solid edge -> (fa, ea, fb, eb, a0, a1, convex, r)
    let mut resolved: Vec<(usize, u8, usize, u8, Vec3, Vec3, bool, f32)> = Vec::new();
    for (i, &(fa, ea)) in selected.iter().enumerate() {
        let r = radii[i].max(1e-5);
        let (fb, eb) = find_orthogonal_neighbour(&originals, fa, ea)?;
        let sa = originals[fa].surface.as_ref();
        let sb = originals[fb].surface.as_ref();
        let conc = edge_concavity(&originals, fa, ea);
        let convex = match conc {
            EdgeConcavity::Convex => true,
            EdgeConcavity::Concave => false, // L-bracket re-entrant cylinder (FreeCAD)
            EdgeConcavity::Invalid => {
                return Err(unsupported_for_non_convex_edge(&originals, fa, ea));
            }
        };
        let (a0, a1) = face_edge_endpoints(sa, ea)?;
        resolved.push((fa, ea, fb, eb, a0, a1, convex, r));
    }

    // ── Corner patch vertices (sphere / toroid) ────────────────────────────
    // Sphere ONLY when all 3 edges at a vertex are filleted **and** radii match.
    // Unequal radii → free ends + soft UnequalRadiusCorner (Tier 1.10).
    // corner: (vertex, kind, corner_r) — kind 0=convex sph, 1=concave sph, 2=onsame toroid
    let mut corner_verts: Vec<(Vec3, u8, f32)> = Vec::new();
    for &(_, _, _, _, a0, a1, _, _) in &resolved {
        for p in [a0, a1] {
            if corner_verts
                .iter()
                .any(|(c, _k, _)| (*c - p).length_squared() < EPS2)
            {
                continue;
            }
            let at_sel: Vec<_> = resolved
                .iter()
                .filter(|e| {
                    (e.4 - p).length_squared() < EPS2 || (e.5 - p).length_squared() < EPS2
                })
                .collect();
            if at_sel.len() != 3 {
                continue;
            }
            let n_convex = at_sel.iter().filter(|e| e.6).count();
            let all_convex = n_convex == 3;
            let all_concave = n_convex == 0;
            let onsame_3 = n_convex == 1 || n_convex == 2;
            if !all_convex && !all_concave && !onsame_3 {
                continue;
            }
            let rs = [at_sel[0].7, at_sel[1].7, at_sel[2].7];
            let r_min = rs[0].min(rs[1]).min(rs[2]);
            let r_max_v = rs[0].max(rs[1]).max(rs[2]);
            let radii_equal = (r_max_v - r_min) < 1e-4;
            if !radii_equal {
                // Soft fault: still build cylinders, no sphere/toroid.
                if !faulty_vertices
                    .iter()
                    .any(|(q, _)| (*q - p).length_squared() < EPS2)
                {
                    faulty_vertices.push((
                        p,
                        FilletError::UnequalRadiusCorner { radii: rs },
                    ));
                }
                continue;
            }
            let solid_at = edges_at_vertex(&originals, p);
            let solid_triad = solid_at.len() == 3 && {
                let mut ok = true;
                let mut faces_found = Vec::new();
                for &(af, ae) in &solid_at {
                    if !faces_found.contains(&af) {
                        faces_found.push(af);
                    }
                    if let Ok((bf, _)) = find_orthogonal_neighbour(&originals, af, ae) {
                        if !faces_found.contains(&bf) {
                            faces_found.push(bf);
                        }
                    } else {
                        ok = false;
                        break;
                    }
                }
                ok && faces_found.len() == 3 && {
                    let mut ns = Vec::new();
                    let mut ortho = true;
                    for &fi in &faces_found {
                        let Ok(n) = face_outward_normal(originals[fi].surface.as_ref()) else {
                            ortho = false;
                            break;
                        };
                        for prev in &ns {
                            if n.dot(*prev).abs() > 0.15 {
                                ortho = false;
                                break;
                            }
                        }
                        if !ortho {
                            break;
                        }
                        ns.push(n);
                    }
                    ortho && ns.len() == 3
                }
            };
            if solid_triad {
                let kind = if all_convex {
                    0u8
                } else if all_concave {
                    1u8
                } else {
                    2u8 // JoinPatch 2+1 → toroid
                };
                corner_verts.push((p, kind, r_min));
            }
        }
    }
    let is_patch_vertex = |p: Vec3| {
        corner_verts
            .iter()
            .any(|(c, _, _)| (*c - p).length_squared() < EPS2)
    };
    let corner_r_at = |p: Vec3| -> f32 {
        corner_verts
            .iter()
            .find(|(c, _, _)| (*c - p).length_squared() < EPS2)
            .map(|(_, _, cr)| *cr)
            .unwrap_or(0.0)
    };

    // Clone planar faces we will shrink; trims accumulate per face index
    let mut working: Vec<Option<NurbsSurface>> = originals
        .iter()
        .map(|f| Some(f.surface.as_ref().clone()))
        .collect();
    let mut faces_trims: Vec<Vec<crate::nurbs::TrimLoop>> =
        originals.iter().map(|f| f.trims.clone()).collect();

    let mut added = 0usize;
    let mat0 = faces
        .first()
        .map(|f| f.material.clone())
        .unwrap_or_default();

    // JoinPatch vertices: exactly 2 selected edges with **mixed** convexity
    // (FreeCAD L-bracket: BSpline join). Pure-concave boss base = 2 cyl only, no patch.
    let is_onsame_vertex = |p: Vec3| {
        let at: Vec<_> = resolved
            .iter()
            .filter(|e| (e.4 - p).length_squared() < EPS2 || (e.5 - p).length_squared() < EPS2)
            .collect();
        at.len() == 2 && at.iter().any(|e| e.6) && at.iter().any(|e| !e.6)
    };

    // One cylinder per edge — trim at sphere triad ends and JoinPatch join ends.
    for &(fa, ea, fb, eb, a0, a1, convex, r) in &resolved {
        let ts = if (convex && is_patch_vertex(a0)) || is_onsame_vertex(a0) {
            if is_patch_vertex(a0) {
                corner_r_at(a0)
            } else {
                r
            }
        } else {
            0.0
        };
        let te = if (convex && is_patch_vertex(a1)) || is_onsame_vertex(a1) {
            if is_patch_vertex(a1) {
                corner_r_at(a1)
            } else {
                r
            }
        } else {
            0.0
        };

        let sa0 = originals[fa].surface.as_ref();
        let sb0 = originals[fb].surface.as_ref();
        let res = fillet_planar_edge_trimmed_ex(sa0, ea, sb0, eb, r, ts, te, convex)?;
        let na = face_outward_normal(sa0)?;
        let nb = face_outward_normal(sb0)?;
        let cos_n = na.dot(nb).clamp(-1.0, 1.0);
        let setback = r / (cos_n.acos() * 0.5).max(1e-3).tan();
        let s = if convex { -1.0f32 } else { 1.0f32 };
        if let Some(ref mut wa) = working[fa] {
            shrink_face_edge(wa, ea, s * nb * setback)?;
        }
        if let Some(ref mut wb) = working[fb] {
            shrink_face_edge(wb, eb, s * na * setback)?;
        }
        // T3 free-end Arc notches on side faces:
        // - n_sel==1: classic single-edge free end (always)
        // - n_sel==2 pure-convex TwoCorner (not JoinPatch, not triad): FreeCAD keeps
        //   0 spheres / 2 cylinders — both free ends STILL need side-face Arc notches
        //   or the third face keeps a sharp corner → gap/gouge (owner Plasticity shot).
        // Skip triad sphere ends and JoinPatch loft ends (their own join geometry).
        for endpoint in [a0, a1] {
            if is_patch_vertex(endpoint) || is_onsame_vertex(endpoint) {
                continue;
            }
            let n_sel_here = resolved
                .iter()
                .filter(|e| {
                    (e.4 - endpoint).length_squared() < EPS2
                        || (e.5 - endpoint).length_squared() < EPS2
                })
                .count();
            if n_sel_here == 0 || n_sel_here >= 3 {
                continue;
            }
            add_side_face_notch_trim(
                &mut working,
                &mut faces_trims,
                &originals,
                fa,
                fb,
                endpoint,
                na,
                nb,
                r,
                convex,
            );
        }
        let between = [
            originals[fa].id,
            originals[fb].id,
        ];
        let mut fillet_face = NurbsFace::new(NurbsFaceId(next_id), res.fillet).with_origin(
            FaceOrigin::Fillet {
                radius: r,
                between,
            },
        );
        fillet_face.material = faces.get(fa).map(|f| f.material.clone()).unwrap_or(mat0.clone());
        faces.push(fillet_face);
        next_id += 1;
        added += 1;
    }

    // Corners: Uniform sphere (convex/concave) or JoinPatch toroid (2+1 concavity).
    for &(vertex, kind, cr) in &corner_verts {
        let resolved7 = resolved_strip_r(&resolved);
        let (face_ids, normals) = match solid_orthogonal_triad_at(&originals, vertex) {
            Some(t) => t,
            None => match orthogonal_triad_at_sel(&originals, &resolved7, vertex) {
                Some(t) => t,
                None => continue,
            },
        };
        let corner_faces = [
            originals[face_ids[0]].id,
            originals[face_ids[1]].id,
            originals[face_ids[2]].id,
        ];
        let corner_surf = if kind == 2 {
            // JoinPatch toroid — FreeCAD major=2r minor=r
            let at3: Vec<_> = resolved7
                .iter()
                .filter(|e| {
                    (e.4 - vertex).length_squared() < EPS2 || (e.5 - vertex).length_squared() < EPS2
                })
                .cloned()
                .collect();
            if at3.len() != 3 {
                continue;
            }
            let Some((axis, e0, e1)) = onsame_torus_frame(&originals, &at3, vertex) else {
                faulty_vertices.push((
                    vertex,
                    FilletError::Unsupported {
                        kind: EdgeValence::Triple,
                        state: CornerMix::Majority,
                    },
                ));
                continue;
            };
            match make_toroidal_onsame_corner(vertex, axis, e0, e1, cr) {
                Ok(s) => s,
                Err(_) => {
                    faulty_vertices.push((
                        vertex,
                        FilletError::Unsupported {
                            kind: EdgeValence::Triple,
                            state: CornerMix::Majority,
                        },
                    ));
                    continue;
                }
            }
        } else if kind == 0 {
            let centre =
                sphere_centre_at_vertex(vertex, normals[0], normals[1], normals[2], cr);
            match make_spherical_octant(centre, cr, normals[0], normals[1], normals[2]) {
                Ok(s) => s,
                Err(_) => continue,
            }
        } else {
            let centre = vertex
                + (normals[0].normalize_or_zero()
                    + normals[1].normalize_or_zero()
                    + normals[2].normalize_or_zero())
                    * cr;
            match make_spherical_octant(centre, cr, -normals[0], -normals[1], -normals[2]) {
                Ok(s) => s,
                Err(_) => continue,
            }
        };
        let mut cf =
            NurbsFace::new(NurbsFaceId(next_id), corner_surf).with_origin(FaceOrigin::Corner {
                radius: cr,
                faces: corner_faces,
            });
        cf.material = faces
            .get(face_ids[0])
            .map(|f| f.material.clone())
            .unwrap_or(mat0.clone());
        faces.push(cf);
        next_id += 1;
        added += 1;
    }

    // JoinPatch mixed-convexity two-edge vertices only (FreeCAD: +BSpline).
    // Pure-convex TwoCorner → Steinmetz (0.5). Pure-concave boss base → 2 cyl only.
    // Join uses max of the two edge radii when equal; unequal → soft fault, no patch.
    let resolved7 = resolved_strip_r(&resolved);
    for p in unique_resolved_vertices(&resolved7) {
        let at: Vec<_> = resolved
            .iter()
            .filter(|e| (e.4 - p).length_squared() < EPS2 || (e.5 - p).length_squared() < EPS2)
            .collect();
        if at.len() != 2 {
            continue;
        }
        let any_cx = at.iter().any(|e| e.6);
        let any_cc = at.iter().any(|e| !e.6);
        if !(any_cx && any_cc) {
            continue;
        }
        let r_join = {
            let ra = at[0].7;
            let rb = at[1].7;
            if (ra - rb).abs() > 1e-4 {
                if !faulty_vertices
                    .iter()
                    .any(|(q, _)| (*q - p).length_squared() < EPS2)
                {
                    faulty_vertices.push((
                        p,
                        FilletError::UnequalRadiusCorner {
                            radii: [ra, rb, 0.0],
                        },
                    ));
                }
                continue;
            }
            ra
        };
        let at7: Vec<_> = at
            .iter()
            .map(|e| (e.0, e.1, e.2, e.3, e.4, e.5, e.6))
            .collect();
        let at_refs: Vec<_> = at7.iter().collect();
        match make_onsame_ruled_corner(&originals, &at_refs, p, r_join) {
            Ok(patch) => {
                let between = [
                    originals[at[0].0].id,
                    originals[at[1].0].id,
                ];
                // Reuse Fillet origin (2-face attribution); name marks the JoinPatch join.
                let mut cf = NurbsFace::new(NurbsFaceId(next_id), patch).with_origin(
                    FaceOrigin::Fillet {
                        radius: r_join,
                        between,
                    },
                );
                cf.material = mat0.clone();
                faces.push(cf);
                next_id += 1;
                added += 1;
            }
            Err(e) => {
                faulty_vertices.push((
                    p,
                    FilletError::Unsupported {
                        kind: EdgeValence::Pair,
                        state: CornerMix::Majority,
                    },
                ));
                let _ = e;
            }
        }
    }

    // Write shrunk planar faces + trims back. Finer tess on notched faces so free-end
    // Arc holes shade cleanly (dense sector poly), not a dark chord wedge.
    for (i, w) in working.into_iter().enumerate() {
        if let Some(mut surf) = w {
            if i < faces_trims.len() && !faces_trims[i].is_empty() {
                surf.tess_tolerance = surf.tess_tolerance.min(0.2);
            }
            faces[i].surface = std::sync::Arc::new(surf);
        }
        if i < faces_trims.len() {
            faces[i].trims = faces_trims[i].clone();
        }
    }

    if added == 0 {
        return Err(FilletError::DegenerateEdge);
    }
    Ok(FilletOutcome {
        faces: faces.clone(),
        faulty_vertices,
    })
}

/// True if the shared edge is a **convex exterior** edge (solid behind both outward normals).
/// False for concave re-entrant edges (L-bracket inner) where FreeCAD uses BSpline JoinPatch blends.
pub fn edge_is_convex_exterior(sa: &NurbsSurface, sb: &NurbsSurface) -> bool {
    let Ok(na) = face_outward_normal(sa) else {
        return false;
    };
    let Ok(nb) = face_outward_normal(sb) else {
        return false;
    };
    // Planar 90° path only — parallel/opposite faces are not a dihedral edge.
    if na.dot(nb).abs() > 0.15 {
        return false;
    }
    let ca = sa.evaluate(0.5, 0.5);
    let cb = sb.evaluate(0.5, 0.5);
    // Each face centre should lie in the solid half-space of the other (outward n).
    na.dot(cb - ca) < 0.15 && nb.dot(ca - cb) < 0.15
}

/// Concavity of one solid edge (two faces). Used for `CornerMix` dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeConcavity {
    /// Exterior convex (cube outer edge) — planar Uniform path.
    Convex,
    /// Re-entrant concave (L-bracket inner) — needs JoinPatch/LoneEdge blend.
    Concave,
    /// Parallel / non-dihedral / non-planar pair.
    Invalid,
}

/// Classify the dihedral at `(fa, ea)` against its orthogonal neighbour.
pub fn edge_concavity(faces: &[NurbsFace], fa: usize, ea: u8) -> EdgeConcavity {
    let Ok((fb, _)) = find_orthogonal_neighbour(faces, fa, ea) else {
        return EdgeConcavity::Invalid;
    };
    if fa >= faces.len() || fb >= faces.len() {
        return EdgeConcavity::Invalid;
    }
    let sa = faces[fa].surface.as_ref();
    let sb = faces[fb].surface.as_ref();
    if edge_is_convex_exterior(sa, sb) {
        EdgeConcavity::Convex
    } else {
        // Shared edge exists but half-space test failed → concave (or near-tangent invalid).
        let Ok(na) = face_outward_normal(sa) else {
            return EdgeConcavity::Invalid;
        };
        let Ok(nb) = face_outward_normal(sb) else {
            return EdgeConcavity::Invalid;
        };
        if na.dot(nb).abs() > 0.15 {
            EdgeConcavity::Invalid
        } else {
            EdgeConcavity::Concave
        }
    }
}

/// How many valid (convex or concave) solid edges meet at `vertex`, split by sign.
/// Returns `(convex, concave)`; edges whose dihedral is invalid are ignored.
fn vertex_sign_counts(faces: &[NurbsFace], vertex: Vec3) -> (usize, usize) {
    let mut convex = 0;
    let mut concave = 0;
    for (fa, ea) in edges_at_vertex(faces, vertex) {
        match edge_concavity(faces, fa, ea) {
            EdgeConcavity::Convex => convex += 1,
            EdgeConcavity::Concave => concave += 1,
            EdgeConcavity::Invalid => {}
        }
    }
    (convex, concave)
}

/// Vertex-level sign mix of the solid edges meeting at `vertex` (independent of any chosen edge).
///
/// * fewer than three valid edges -> [`CornerMix::Open`];
/// * every valid edge has the same sign (all convex like a cube corner, or all concave) ->
///   [`CornerMix::Uniform`];
/// * both signs present -> [`CornerMix::Majority`] (the vertex has a dominant sign group; use
///   [`classify_fillet_end_state`] to learn whether a particular edge is in it or is the odd one).
pub fn classify_corner_state_at_vertex(faces: &[NurbsFace], vertex: Vec3) -> CornerMix {
    let (convex, concave) = vertex_sign_counts(faces, vertex);
    if convex + concave < 3 {
        CornerMix::Open
    } else if convex == 0 || concave == 0 {
        CornerMix::Uniform
    } else {
        CornerMix::Majority
    }
}

/// Sign mix at `vertex` as seen from one filleted edge `(fa, ea)`, comparing that edge's sign with
/// the other valid edges meeting there:
///
/// * the edge itself has no valid dihedral -> [`CornerMix::Minority`] (it cannot join anything);
/// * fewer than two other valid edges -> [`CornerMix::Open`];
/// * every other edge shares its sign -> [`CornerMix::Uniform`];
/// * no other edge shares its sign (it is the lone odd edge) -> [`CornerMix::Minority`];
/// * otherwise (it shares its sign with some, not all, of the rest) -> [`CornerMix::Majority`].
pub fn classify_fillet_end_state(
    faces: &[NurbsFace],
    vertex: Vec3,
    fa: usize,
    ea: u8,
) -> CornerMix {
    let own = edge_concavity(faces, fa, ea);
    if own == EdgeConcavity::Invalid {
        return CornerMix::Minority;
    }
    let own_cx = own == EdgeConcavity::Convex;
    let self_mid = face_edge_endpoints(faces[fa].surface.as_ref(), ea)
        .map(|(a, b)| (a + b) * 0.5)
        .ok();
    let mut same = 0usize;
    let mut differ = 0usize;
    for (oa, oe) in edges_at_vertex(faces, vertex) {
        // Skip the filleted edge itself (it is deduplicated by midpoint, so match on that).
        if let (Some(m), Ok((p, q))) = (self_mid, face_edge_endpoints(faces[oa].surface.as_ref(), oe)) {
            if (((p + q) * 0.5) - m).length_squared() < 1e-8 {
                continue;
            }
        }
        match edge_concavity(faces, oa, oe) {
            EdgeConcavity::Convex => {
                if own_cx { same += 1 } else { differ += 1 }
            }
            EdgeConcavity::Concave => {
                if own_cx { differ += 1 } else { same += 1 }
            }
            EdgeConcavity::Invalid => {}
        }
    }
    if same + differ < 2 {
        CornerMix::Open
    } else if differ == 0 {
        CornerMix::Uniform
    } else if same == 0 {
        CornerMix::Minority
    } else {
        CornerMix::Majority
    }
}

/// Edge-count class from the number of solid edges meeting at a vertex.
fn edge_valence_class(n: usize) -> EdgeValence {
    match n {
        0 => EdgeValence::Degenerate,
        1 => EdgeValence::Single,
        2 => EdgeValence::Pair,
        3 => EdgeValence::Triple,
        n => EdgeValence::Many(n.min(u8::MAX as usize) as u8),
    }
}

/// Map a rejected non-convex edge into the right `Unsupported` leaf (never invent a surface).
///
/// Looks at both ends of the edge; the first end where the edge meets a mixed-sign neighbourhood
/// supplies the edge-count class and the sign mix. If neither end is mixed, the edge itself is the
/// problem and the leaf reports a single edge with an undetermined mix.
pub fn unsupported_for_non_convex_edge(
    faces: &[NurbsFace],
    fa: usize,
    ea: u8,
) -> FilletError {
    let ends = faces
        .get(fa)
        .and_then(|f| face_edge_endpoints(f.surface.as_ref(), ea).ok());
    if let Some((p0, p1)) = ends {
        for v in [p0, p1] {
            let mix = classify_fillet_end_state(faces, v, fa, ea);
            if matches!(mix, CornerMix::Majority | CornerMix::Minority) {
                return FilletError::Unsupported {
                    kind: edge_valence_class(edges_at_vertex(faces, v).len()),
                    state: mix,
                };
            }
        }
    }
    FilletError::Unsupported {
        kind: EdgeValence::Single,
        state: CornerMix::Undetermined,
    }
}

/// Emit a CW hole trim on the side face at a free cylinder end (PROMPT T3).
pub(crate) fn add_side_face_notch_trim(
    working: &mut [Option<NurbsSurface>],
    faces_trims: &mut [Vec<crate::nurbs::TrimLoop>],
    originals: &[NurbsFace],
    fa: usize,
    fb: usize,
    vertex: Vec3,
    na: Vec3,
    nb: Vec3,
    r: f32,
    convex: bool,
) {
    const EPS2: f32 = 1e-8;
    // Convex: centre inset; concave: centre in free space (match cylinder axis sign).
    let sign = if convex { 1.0f32 } else { -1.0f32 };
    let arc_centre = vertex - sign * (na * r + nb * r);
    let arc_start = vertex - sign * nb * r;
    let arc_end = vertex - sign * na * r;

    for (fi, slot) in working.iter_mut().enumerate() {
        if fi == fa || fi == fb {
            continue;
        }
        let Some(ref _surf) = slot else {
            continue;
        };
        // Side face must own the original vertex
        let had = originals[fi].surface.control_points.iter().any(|row| {
            row.iter()
                .any(|cp| (cp.position - vertex).length_squared() < EPS2)
        });
        if !had {
            continue;
        }
        // UV from **original** surface — working may already be edge-shrunk (order-dependent),
        // which desyncs world_to_uv from free-end arc geometry.
        // Do NOT move corner CPs: that warps the planar face while the Arc hole UV
        // stays on the original plane → free-end HOLE (owner regression on simple bevel).
        let orig_surf = originals[fi].surface.as_ref();
        if let Some(hole) = crate::nurbs_trim::corner_notch_hole(
            orig_surf,
            vertex,
            arc_start,
            arc_end,
            arc_centre,
        ) {
            if fi < faces_trims.len() {
                faces_trims[fi].push(hole);
            }
        }
    }
}

/// Exact rational quarter-cylinder fillet between two **orthogonal planar** faces.
/// Non-planar supports → [`FilletError::CurvedFace`] (use [`fillet_cylinder_plane_rim`]).
///
/// `edge_a` / `edge_b` must be the shared solid edge (same geometry, possibly reversed).
/// Faces are shrunk by setback `r` (Î¸=90Â°) and a new fillet face is produced.
pub fn fillet_orthogonal_planar_edge(
    face_a: &NurbsSurface,
    edge_a: u8,
    face_b: &NurbsSurface,
    edge_b: u8,
    radius: f32,
) -> Result<PlanarFilletResult, FilletError> {
    fillet_orthogonal_planar_edge_trimmed(face_a, edge_a, face_b, edge_b, radius, 0.0, 0.0)
}

/// Same as [`fillet_orthogonal_planar_edge`] but trims `trim_start` / `trim_end` along the edge
/// (from endpoints `a0` / `a1` of face A's edge). Used so cylinders stop at spherical corners (N5).
///
/// Auto-detects **convex exterior** vs **concave re-entrant** (L-bracket inner edge):
/// FreeCAD single re-entrant → 1 Cylinder with axis in free space (`emid + r(n0+n1)`).
pub fn fillet_orthogonal_planar_edge_trimmed(
    face_a: &NurbsSurface,
    edge_a: u8,
    face_b: &NurbsSurface,
    edge_b: u8,
    radius: f32,
    trim_start: f32,
    trim_end: f32,
) -> Result<PlanarFilletResult, FilletError> {
    let convex = edge_is_convex_exterior(face_a, face_b);
    fillet_planar_edge_trimmed_ex(
        face_a, edge_a, face_b, edge_b, radius, trim_start, trim_end, convex,
    )
}

/// Planar dihedral fillet with explicit convex/concave sign and general setback.
///
/// - **Convex**: axis `emid − α(n0+n1)`, α=r/(1+cosθ); setback into solid.
/// - **Concave**: axis `emid + α(n0+n1)`; setback into free notch (L-bracket).
/// - Dihedral θ = acos(n0·n1); setback s = r/tan(θ/2); rational arc weight cos(θ/2).
///   θ=90° recovers classic quarter-cylinder (α=r, s=r, w=√2/2).
pub fn fillet_planar_edge_trimmed_ex(
    face_a: &NurbsSurface,
    edge_a: u8,
    face_b: &NurbsSurface,
    edge_b: u8,
    radius: f32,
    trim_start: f32,
    trim_end: f32,
    convex: bool,
) -> Result<PlanarFilletResult, FilletError> {
    if !surface_is_planar(face_a) || !surface_is_planar(face_b) {
        return Err(FilletError::CurvedFace {
            reason: "planar solver only — use cylinder–plane rim for analytic toroid",
        });
    }
    let r = radius.max(1e-5);
    let max_r = max_fillet_radius(face_a, face_b);
    if r > max_r + 1e-6 {
        return Err(FilletError::RadiusTooLarge { max: max_r });
    }

    let n0 = face_outward_normal(face_a)?;
    let n1 = face_outward_normal(face_b)?;
    let cos_n = n0.dot(n1).clamp(-1.0, 1.0);
    // Refuse near-parallel (tangent, R.2) or anti-parallel (folded, R.3).
    if cos_n.abs() > 0.98 {
        return Err(FilletError::NotOrthogonal);
    }
    let theta = cos_n.acos(); // dihedral between outward normals ∈ (0, π)
    // Face setback along each face from the edge: s = r / tan(θ/2). θ=90° → s=r.
    let half = (theta * 0.5).max(1e-3);
    let setback = r / half.tan();
    if !setback.is_finite() || setback < 1e-6 {
        return Err(FilletError::DegenerateEdge);
    }
    // Axis offset scale: α = r / (1 + cos θ). θ=90° → α=r.
    // Convex: c = emid − α(n0+n1); concave: c = emid + α(n0+n1).
    let alpha = r / (1.0 + cos_n).max(1e-4);

    let (a0, a1) = face_edge_endpoints(face_a, edge_a)?;
    let edge_dir = a1 - a0;
    let elen = edge_dir.length();
    if elen < 1e-6 {
        return Err(FilletError::DegenerateEdge);
    }
    let e = edge_dir / elen;
    let mut ts = trim_start.clamp(0.0, elen * 0.499);
    let mut te = trim_end.clamp(0.0, elen * 0.499);
    if ts + te >= elen - 1e-4 {
        let half_e = (elen * 0.5 - 5e-4).max(0.0);
        if ts > 0.0 && te > 0.0 {
            ts = half_e;
            te = half_e;
        } else if ts > 0.0 {
            ts = (elen - 1e-3).max(0.0);
            te = 0.0;
        } else {
            te = (elen - 1e-3).max(0.0);
            ts = 0.0;
        }
    }
    if elen < 1e-4 {
        return Err(FilletError::DegenerateEdge);
    }
    let a0t = a0 + e * ts;
    let a1t = a1 - e * te;

    let s = if convex { -1.0f32 } else { 1.0f32 };
    let emid = (a0t + a1t) * 0.5;
    let axis_origin = emid + s * alpha * (n0 + n1);
    let axis_dir = e;
    let axis_a = a0t + s * alpha * (n0 + n1);
    let axis_b = a1t + s * alpha * (n0 + n1);

    // Rational circular arc of angle θ (Piegl/Tiller):
    //   P0 = c + r d0, P2 = c + r d1, P1 = c + r(d0+d1)/(1+d0·d1), w1 = cos(θ/2)
    // Convex: d = +n (from inset axis toward faces). Concave: d = −n.
    let arc_sign = -s;
    let w_mid = ((1.0 + cos_n) * 0.5).max(0.0).sqrt(); // cos(θ/2)
    let denom = (1.0 + cos_n).max(1e-4);
    let mk_arc = |c: Vec3| -> [ControlPoint; 3] {
        let d0 = n0 * arc_sign;
        let d1 = n1 * arc_sign;
        [
            ControlPoint::new(c + d0 * r, 1.0),
            ControlPoint::new(c + (d0 + d1) * (r / denom), w_mid),
            ControlPoint::new(c + d1 * r, 1.0),
        ]
    };
    let arc_a = mk_arc(axis_a);
    let arc_b = mk_arc(axis_b);

    let mut fillet = NurbsSurface {
        control_points: vec![
            vec![arc_a[0].clone(), arc_b[0].clone()],
            vec![arc_a[1].clone(), arc_b[1].clone()],
            vec![arc_a[2].clone(), arc_b[2].clone()],
        ],
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v: vec![0.0, 0.0, 1.0, 1.0],
        degree_u: 2,
        degree_v: 1,
        name: "Fillet".into(),
        mesh_cache_id: None,
        tess_tolerance: 0.25,
    };

    let free_dir = if convex { n0 + n1 } else { -(n0 + n1) };
    let fn_mid = fillet.normal(0.5, 0.5);
    if fn_mid.dot(free_dir) < 0.0 {
        fillet.control_points = vec![
            vec![arc_a[2].clone(), arc_b[2].clone()],
            vec![arc_a[1].clone(), arc_b[1].clone()],
            vec![arc_a[0].clone(), arc_b[0].clone()],
        ];
    }

    let mut new_a = face_a.clone();
    shrink_face_edge(&mut new_a, edge_a, s * n1 * setback)?;
    let mut new_b = face_b.clone();
    shrink_face_edge(&mut new_b, edge_b, s * n0 * setback)?;

    Ok(PlanarFilletResult {
        face_a: new_a,
        face_b: new_b,
        fillet,
        radius: r,
        axis_origin,
        axis_dir,
    })
}

// â”€â”€ N5: spherical octant corner (symmetric equal-r, mutually perpendicular) â”€â”€



/// Linear variable-radius planar fillet: circular arc of r0 at edge start, r1 at end.
///
/// **Slanted tangency rail:** each face is set back along a straight line from
/// `s0 = r0/tan(θ/2)` at the start endpoint to `s1 = r1/tan(θ/2)` at the end
/// (not a uniform max-r setback — that leaves a G0 crack of size
/// `(r_max−r_min)/tan(θ/2)` at the small-r free end).
pub fn fillet_planar_edge_linear(
    face_a: &NurbsSurface,
    edge_a: u8,
    face_b: &NurbsSurface,
    edge_b: u8,
    r0: f32,
    r1: f32,
    convex: bool,
) -> Result<PlanarFilletResult, FilletError> {
    let r0 = r0.max(1e-5);
    let r1 = r1.max(1e-5);
    let r_max = r0.max(r1);
    let max_r = max_fillet_radius(face_a, face_b);
    if r_max > max_r + 1e-6 {
        return Err(FilletError::RadiusTooLarge { max: max_r });
    }
    let n0 = face_outward_normal(face_a)?;
    let n1 = face_outward_normal(face_b)?;
    let cos_n = n0.dot(n1).clamp(-1.0, 1.0);
    if cos_n.abs() > 0.98 {
        return Err(FilletError::NotOrthogonal);
    }
    let theta = cos_n.acos();
    let tan_half = (theta * 0.5).max(1e-3).tan();
    let s0 = r0 / tan_half;
    let s1 = r1 / tan_half;
    let (a0, a1) = face_edge_endpoints(face_a, edge_a)?;
    let edge_dir = a1 - a0;
    let elen = edge_dir.length();
    if elen < 1e-4 {
        return Err(FilletError::DegenerateEdge);
    }
    let e = edge_dir / elen;
    let s = if convex { -1.0f32 } else { 1.0f32 };
    let arc_sign = -s;
    let w_mid = ((1.0 + cos_n) * 0.5).max(0.0).sqrt();
    let denom = (1.0 + cos_n).max(1e-4);
    let mk_arc = |c: Vec3, r: f32| -> [ControlPoint; 3] {
        let d0 = n0 * arc_sign;
        let d1 = n1 * arc_sign;
        [
            ControlPoint::new(c + d0 * r, 1.0),
            ControlPoint::new(c + (d0 + d1) * (r / denom), w_mid),
            ControlPoint::new(c + d1 * r, 1.0),
        ]
    };
    let alpha0 = r0 / denom;
    let alpha1 = r1 / denom;
    let axis_a = a0 + s * alpha0 * (n0 + n1);
    let axis_b = a1 + s * alpha1 * (n0 + n1);
    let arc_a = mk_arc(axis_a, r0);
    let arc_b = mk_arc(axis_b, r1);
    let mut fillet = NurbsSurface {
        control_points: vec![
            vec![arc_a[0].clone(), arc_b[0].clone()],
            vec![arc_a[1].clone(), arc_b[1].clone()],
            vec![arc_a[2].clone(), arc_b[2].clone()],
        ],
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v: vec![0.0, 0.0, 1.0, 1.0],
        degree_u: 2,
        degree_v: 1,
        name: "Fillet".into(),
        mesh_cache_id: None,
        tess_tolerance: 0.25,
    };
    let free_dir = if convex { n0 + n1 } else { -(n0 + n1) };
    if fillet.normal(0.5, 0.5).dot(free_dir) < 0.0 {
        fillet.control_points = vec![
            vec![arc_a[2].clone(), arc_b[2].clone()],
            vec![arc_a[1].clone(), arc_b[1].clone()],
            vec![arc_a[0].clone(), arc_b[0].clone()],
        ];
    }
    // Slanted rail: face A set back along −n1 (convex) by s0 at a0, s1 at a1.
    // face_edge_endpoints order matches edge CP parametrization in shrink_face_edge_tapered.
    let mut new_a = face_a.clone();
    shrink_face_edge_tapered(&mut new_a, edge_a, s * n1 * s0, s * n1 * s1)?;
    // Face B: edge may be reversed relative to A's a0→a1 — match by endpoint proximity.
    let (b0, b1) = face_edge_endpoints(face_b, edge_b)?;
    let (sb0, sb1) = if (b0 - a0).length_squared() <= (b0 - a1).length_squared() {
        (s0, s1) // b0 near a0
    } else {
        (s1, s0) // b0 near a1 — reversed
    };
    let mut new_b = face_b.clone();
    shrink_face_edge_tapered(&mut new_b, edge_b, s * n0 * sb0, s * n0 * sb1)?;
    let emid = (a0 + a1) * 0.5;
    let r_mid = 0.5 * (r0 + r1);
    let axis_origin = emid + s * (r_mid / denom) * (n0 + n1);
    Ok(PlanarFilletResult {
        face_a: new_a,
        face_b: new_b,
        fillet,
        radius: r_mid,
        axis_origin,
        axis_dir: e,
    })
}


/// Multi-station planar fillet from sorted `(t,r)` samples (t∈[0,1] along face-A edge).
///
/// Non-monotone laws (interior peak/valley) are supported: face rails use
/// [`shrink_face_edge_law`] (refine edge CPs + per-station setback
/// `s(t)=r(t)/tan(θ/2)`), not a first→last linear taper.
pub fn fillet_planar_edge_law(
    face_a: &NurbsSurface,
    edge_a: u8,
    face_b: &NurbsSurface,
    edge_b: u8,
    samples: &[(f32, f32)],
    convex: bool,
) -> Result<PlanarFilletResult, FilletError> {
    if samples.len() < 2 {
        return Err(FilletError::DegenerateEdge);
    }
    let r_max = samples.iter().map(|p| p.1).fold(0.0f32, f32::max).max(1e-5);
    let max_r = max_fillet_radius(face_a, face_b);
    if r_max > max_r + 1e-6 {
        return Err(FilletError::RadiusTooLarge { max: max_r });
    }
    let n0 = face_outward_normal(face_a)?;
    let n1 = face_outward_normal(face_b)?;
    let cos_n = n0.dot(n1).clamp(-1.0, 1.0);
    if cos_n.abs() > 0.98 {
        return Err(FilletError::NotOrthogonal);
    }
    let theta = cos_n.acos();
    let tan_half = (theta * 0.5).max(1e-3).tan();
    let (a0, a1) = face_edge_endpoints(face_a, edge_a)?;
    let edge_dir = a1 - a0;
    let elen = edge_dir.length();
    if elen < 1e-4 {
        return Err(FilletError::DegenerateEdge);
    }
    let e = edge_dir / elen;
    let s = if convex { -1.0f32 } else { 1.0f32 };
    let arc_sign = -s;
    let w_mid = ((1.0 + cos_n) * 0.5).max(0.0).sqrt();
    let denom = (1.0 + cos_n).max(1e-4);
    let n = samples.len();
    // control_points[u_arc][v_station]
    let mut cols: Vec<[ControlPoint; 3]> = Vec::with_capacity(n);
    for &(t, r) in samples {
        let t = t.clamp(0.0, 1.0);
        let r = r.max(1e-5);
        let alpha = r / denom;
        let pos = a0 + e * (t * elen);
        let c = pos + s * alpha * (n0 + n1);
        let d0 = n0 * arc_sign;
        let d1 = n1 * arc_sign;
        cols.push([
            ControlPoint::new(c + d0 * r, 1.0),
            ControlPoint::new(c + (d0 + d1) * (r / denom), w_mid),
            ControlPoint::new(c + d1 * r, 1.0),
        ]);
    }
    let mut control_points = vec![Vec::with_capacity(n); 3];
    for col in &cols {
        for i in 0..3 {
            control_points[i].push(col[i].clone());
        }
    }
    // Degree-1 in V: knots 0,0, t1, t2, ..., 1,1 (clamped)
    let knots_v = degree1_clamped_knots(n);
    let mut fillet = NurbsSurface {
        control_points,
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v,
        degree_u: 2,
        degree_v: 1,
        name: "Fillet".into(),
        mesh_cache_id: None,
        tess_tolerance: 0.25,
    };
    let free_dir = if convex { n0 + n1 } else { -(n0 + n1) };
    if fillet.normal(0.5, 0.5).dot(free_dir) < 0.0 {
        control_points = vec![
            fillet.control_points[2].clone(),
            fillet.control_points[1].clone(),
            fillet.control_points[0].clone(),
        ];
        fillet.control_points = control_points;
    }
    // Bent multi-station rail: s(t)=r(t)/tan(θ/2) at each sample (not first→last only).
    let mut new_a = face_a.clone();
    shrink_face_edge_law(&mut new_a, edge_a, s * n1, a0, a1, samples, tan_half)?;
    let mut new_b = face_b.clone();
    shrink_face_edge_law(&mut new_b, edge_b, s * n0, a0, a1, samples, tan_half)?;
    let r_mid = samples[samples.len() / 2].1.max(1e-5);
    let emid = (a0 + a1) * 0.5;
    Ok(PlanarFilletResult {
        face_a: new_a,
        face_b: new_b,
        fillet,
        radius: r_mid,
        axis_origin: emid + s * (r_mid / denom) * (n0 + n1),
        axis_dir: e,
    })
}

/// Apply fillet to a solid face list: replace face A/B, append fillet face with new id.
/// Prefer [`apply_edges_fillet_batch`] (also emits side-face Arc trims).
pub fn apply_fillet_to_faces(
    faces: &mut Vec<NurbsFace>,
    face_a_idx: usize,
    edge_a: u8,
    face_b_idx: usize,
    edge_b: u8,
    radius: f32,
    next_id: u64,
) -> Result<PlanarFilletResult, FilletError> {
    if face_a_idx >= faces.len() || face_b_idx >= faces.len() {
        return Err(FilletError::BadEdgeIndex);
    }
    let sa = faces[face_a_idx].surface.as_ref().clone();
    let sb = faces[face_b_idx].surface.as_ref().clone();
    let convex = edge_is_convex_exterior(&sa, &sb);
    let result = fillet_planar_edge_trimmed_ex(
        &sa, edge_a, &sb, edge_b, radius, 0.0, 0.0, convex,
    )?;
    faces[face_a_idx].surface = std::sync::Arc::new(result.face_a.clone());
    faces[face_b_idx].surface = std::sync::Arc::new(result.face_b.clone());
    // Side-face Arc holes at free ends
    let na = face_outward_normal(&sa)?;
    let nb = face_outward_normal(&sb)?;
    let (a0, a1) = face_edge_endpoints(&sa, edge_a)?;
    let mut working: Vec<Option<NurbsSurface>> = faces
        .iter()
        .map(|f| Some(f.surface.as_ref().clone()))
        .collect();
    let mut faces_trims: Vec<Vec<crate::nurbs::TrimLoop>> =
        faces.iter().map(|f| f.trims.clone()).collect();
    let originals = faces.clone();
    for endpoint in [a0, a1] {
        add_side_face_notch_trim(
            &mut working,
            &mut faces_trims,
            &originals,
            face_a_idx,
            face_b_idx,
            endpoint,
            na,
            nb,
            radius,
            convex,
        );
    }
    for (i, t) in faces_trims.into_iter().enumerate() {
        if i < faces.len() {
            faces[i].trims = t;
        }
    }
    let between = [faces[face_a_idx].id, faces[face_b_idx].id];
    let mut fillet_face = NurbsFace::new(NurbsFaceId(next_id), result.fillet.clone()).with_origin(
        FaceOrigin::Fillet {
            radius,
            between,
        },
    );
    fillet_face.material = faces[face_a_idx].material.clone();
    faces.push(fillet_face);
    Ok(result)
}

// ── FaceOrigin re-solve (PLAN Decision 5 / §2b) ─────────────────────────────

/// One editable fillet op recovered from [`FaceOrigin`] tags (or built by the tool).
#[derive(Clone, Debug)]
pub struct FilletOp {
    pub edges: Vec<(usize, u8)>,
    /// Full radius law (Const / Linear / Law) — not just peak scalar.
    pub radius: Radius,
}

/// Re-solve fillets from a **pre-fillet base solid** + edge list + const radius.
///
/// Prefer [`resolve_edges_fillet_from_base_radius`] when the origin carries Linear/Law.
pub fn resolve_edges_fillet_from_base(
    base_faces: &[NurbsFace],
    edges: &[(usize, u8)],
    radius: f32,
    next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    resolve_edges_fillet_from_base_radius(
        base_faces,
        edges,
        &Radius::Const(radius),
        next_id,
    )
}

/// Re-solve with a full [`Radius`] law (Const / Linear / Law).
pub fn resolve_edges_fillet_from_base_radius(
    base_faces: &[NurbsFace],
    edges: &[(usize, u8)],
    radius: &Radius,
    next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    let mut faces = base_faces.to_vec();
    for f in &mut faces {
        f.origin = FaceOrigin::Base;
        f.trims.clear();
    }
    apply_edges_fillet_batch_radius(&mut faces, edges, radius, next_id)
}

/// Recover solid-edge tags from attributed edge-fillet origins
/// (`Fillet` / `FilletLinear` / `FilletLaw`) by finding the shared orthogonal edge
/// of each `between` pair on **pre-fillet** `base_faces`.
pub fn edges_from_fillet_origins(
    base_faces: &[NurbsFace],
    attributed: &[NurbsFace],
) -> Vec<(usize, u8)> {
    const EPS2: f32 = 1e-8;
    let id_to_idx: BTreeMap<u64, usize> = base_faces
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.0, i))
        .collect();
    let mut raw: Vec<(usize, u8)> = Vec::new();
    for f in attributed {
        let Some(between) = f.origin.fillet_between() else {
            continue;
        };
        let Some(&ia) = id_to_idx.get(&between[0].0) else {
            continue;
        };
        let Some(&ib) = id_to_idx.get(&between[1].0) else {
            continue;
        };
        let sa = base_faces[ia].surface.as_ref();
        for ea in 0u8..4 {
            let Ok((a0, a1)) = face_edge_endpoints(sa, ea) else {
                continue;
            };
            let sb = base_faces[ib].surface.as_ref();
            for eb in 0u8..4 {
                let Ok((b0, b1)) = face_edge_endpoints(sb, eb) else {
                    continue;
                };
                let same = |p: Vec3, q: Vec3| (p - q).length_squared() < EPS2;
                if (same(a0, b0) && same(a1, b1)) || (same(a0, b1) && same(a1, b0)) {
                    raw.push((ia, ea));
                }
            }
        }
    }
    dedupe_solid_edges(base_faces, &raw)
}

/// Collect a [`FilletOp`] from attributed faces (first edge-fillet origin supplies the law).
pub fn fillet_op_from_origins(
    base_faces: &[NurbsFace],
    attributed: &[NurbsFace],
) -> Option<FilletOp> {
    let edges = edges_from_fillet_origins(base_faces, attributed);
    if edges.is_empty() {
        return None;
    }
    let radius = attributed.iter().find_map(|f| f.origin.fillet_radius_law())?;
    Some(FilletOp { edges, radius })
}

/// Re-solve from base using the law recovered from attributed fillet faces.
pub fn resolve_fillet_op_from_origins(
    base_faces: &[NurbsFace],
    attributed: &[NurbsFace],
    next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    let op = fillet_op_from_origins(base_faces, attributed).ok_or(FilletError::DegenerateEdge)?;
    resolve_edges_fillet_from_base_radius(base_faces, &op.edges, &op.radius, next_id)
}

/// Un-shrink planar support faces for **all** edge-fillet origins and delete
/// fillet / corner faces + free-end notch trims. Restores sharp solid edges.
///
/// Returns recovered solid edges with peak radius (for re-batch after sequential).
/// Curved supports → [`FilletError::CurvedFace`].
///
/// Used by sequential fillet (P3) and Remove Fillet (P4).
pub fn remove_all_planar_edge_fillets(
    faces: &mut Vec<NurbsFace>,
) -> Result<Vec<(usize, u8, f32)>, FilletError> {
    // Collect ops first (indices into current list).
    let mut ops: Vec<(usize, f32, [NurbsFaceId; 2])> = Vec::new();
    for (i, f) in faces.iter().enumerate() {
        match &f.origin {
            FaceOrigin::Fillet { radius, between } => {
                ops.push((i, *radius, *between));
            }
            FaceOrigin::FilletLinear { r0, r1, between } => {
                ops.push((i, r0.max(*r1), *between));
            }
            FaceOrigin::FilletLaw { samples, between } => {
                let peak = samples.iter().map(|p| p.1).fold(0.0f32, f32::max);
                ops.push((i, peak, *between));
            }
            _ => {}
        }
    }
    if ops.is_empty() {
        return Ok(Vec::new());
    }

    let id_to_idx: BTreeMap<u64, usize> = faces
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.0, i))
        .collect();

    let mut recovered: Vec<(usize, u8, f32)> = Vec::new();
    let mut delete_ids: Vec<u64> = Vec::new();

    for &(_fi, r, between) in &ops {
        let Some(&ia) = id_to_idx.get(&between[0].0) else {
            continue;
        };
        let Some(&ib) = id_to_idx.get(&between[1].0) else {
            continue;
        };
        let sa = faces[ia].surface.as_ref().clone();
        let sb = faces[ib].surface.as_ref().clone();
        if !surface_is_planar(&sa) || !surface_is_planar(&sb) {
            return Err(FilletError::CurvedFace {
                reason: "remove fillet: curved support (planar only)",
            });
        }
        let na = face_outward_normal(&sa)?;
        let nb = face_outward_normal(&sb)?;
        let cos_n = na.dot(nb).clamp(-1.0, 1.0);
        let half = (cos_n.acos() * 0.5).max(1e-3);
        let setback = r / half.tan();
        // Shrink used s=-1 for convex (into solid along neighbour normal).
        // Un-shrink = reverse. Prefer convex (cube edges).
        let convex = edge_concavity_from_normals(na, nb);
        let s = if convex { -1.0f32 } else { 1.0f32 };
        // Reverse: A was moved by s*nb*setback along its edge → move by -s*nb*setback
        let ea = closest_edge_to_plane_intersection(&sa, &sb, na, nb)?;
        let eb = closest_edge_to_plane_intersection(&sb, &sa, nb, na)?;
        let mut wa = sa;
        shrink_face_edge(&mut wa, ea, -s * nb * setback)?;
        faces[ia].surface = std::sync::Arc::new(wa);
        faces[ia].trims.clear();
        let mut wb = sb;
        shrink_face_edge(&mut wb, eb, -s * na * setback)?;
        faces[ib].surface = std::sync::Arc::new(wb);
        faces[ib].trims.clear();
        recovered.push((ia, ea, r));
        // Also record neighbour tag for dedupe later
        recovered.push((ib, eb, r));
    }

    // Delete fillet faces + corner patches
    for f in faces.iter() {
        match f.origin {
            FaceOrigin::Fillet { .. }
            | FaceOrigin::FilletLinear { .. }
            | FaceOrigin::FilletLaw { .. }
            | FaceOrigin::Corner { .. } => delete_ids.push(f.id.0),
            _ => {}
        }
    }
    faces.retain(|f| !delete_ids.contains(&f.id.0));
    // Clear residual free-end notch trims on every remaining face
    for f in faces.iter_mut() {
        f.trims.clear();
        if matches!(
            f.origin,
            FaceOrigin::Fillet { .. }
                | FaceOrigin::FilletLinear { .. }
                | FaceOrigin::FilletLaw { .. }
                | FaceOrigin::Corner { .. }
        ) {
            f.origin = FaceOrigin::Base;
        }
    }

    // Dedupe solid edges (prefer first radius)
    let tags: Vec<(usize, u8)> = recovered.iter().map(|&(f, e, _)| (f, e)).collect();
    let uniq = dedupe_solid_edges(faces, &tags);
    let mut out = Vec::new();
    for &(fa, ea) in &uniq {
        let r = recovered
            .iter()
            .find(|&&(f, e, _)| edge_in_set(faces, &[(fa, ea)], f, e))
            .map(|t| t.2)
            .unwrap_or(0.1);
        out.push((fa, ea, r));
    }
    Ok(out)
}

pub(crate) fn edge_concavity_from_normals(na: Vec3, nb: Vec3) -> bool {
    // Convex dihedral for solid: outward normals diverge (n_a · n_b ~ 0 for cube).
    // Match edge_concavity default for planar orthogonal: convex.
    na.dot(nb) < 0.5
}

/// Edge of `s` whose mid is closest to the plane–plane intersection with neighbour.
pub(crate) fn closest_edge_to_plane_intersection(
    s: &NurbsSurface,
    neighbour: &NurbsSurface,
    n_s: Vec3,
    n_n: Vec3,
) -> Result<u8, FilletError> {
    let p_s = s.evaluate(0.5, 0.5);
    let p_n = neighbour.evaluate(0.5, 0.5);
    // Intersection direction
    let dir = n_s.cross(n_n);
    if dir.length_squared() < 1e-10 {
        return Err(FilletError::DegenerateEdge);
    }
    let dir = dir.normalize();
    // A point on the intersection: solve approx from two planes.
    // Plane S: n_s · (x - p_s) = 0; Plane N: n_n · (x - p_n) = 0.
    // Pick point on S closest to p_n projected.
    let mut mid = p_s;
    for _ in 0..4 {
        mid -= n_s * n_s.dot(mid - p_s);
        mid -= n_n * n_n.dot(mid - p_n);
    }
    let mut best = (f32::MAX, 0u8);
    for ei in 0u8..4 {
        let Ok((a, b)) = face_edge_endpoints(s, ei) else {
            continue;
        };
        let emid = (a + b) * 0.5;
        // Distance to infinite line (mid, dir)
        let t = (emid - mid).dot(dir);
        let closest = mid + dir * t;
        let d = (emid - closest).length();
        // Prefer edges parallel to dir
        let ed = (b - a).normalize_or_zero();
        let parallel = ed.dot(dir).abs();
        let score = d - parallel * 0.01;
        if score < best.0 {
            best = (score, ei);
        }
    }
    if best.0 > 1.0 {
        return Err(FilletError::DegenerateEdge);
    }
    Ok(best.1)
}

/// Apply new edges on a solid that may already have planar fillets (sequential path).
/// Un-shrinks prior planar fillets, merges recovered edges with `new_edges`, re-batches.
pub fn apply_edges_fillet_sequential_const(
    faces: &mut Vec<NurbsFace>,
    new_edges: &[(usize, u8)],
    radius: f32,
    next_id: u64,
) -> Result<FilletOutcome, FilletError> {
    let r = radius.max(1e-5);
    let has_prior = faces.iter().any(|f| f.origin.fillet_between().is_some());
    let mut edge_radii: Vec<(usize, u8, f32)> = Vec::new();
    if has_prior {
        let prior = remove_all_planar_edge_fillets(faces)?;
        edge_radii.extend(prior);
    }
    for &(fi, ei) in new_edges {
        // Remap: after remove, face indices of Base faces are stable (only fillet
        // faces deleted, which were appended). Base indices unchanged.
        if !edge_radii
            .iter()
            .any(|&(f, e, _)| edge_in_set(faces, &[(fi, ei)], f, e))
        {
            edge_radii.push((fi, ei, r));
        } else {
            // Upgrade radius of recovered edge if re-selected
            for t in edge_radii.iter_mut() {
                if edge_in_set(faces, &[(fi, ei)], t.0, t.1) {
                    t.2 = r;
                }
            }
        }
    }
    if edge_radii.is_empty() {
        return Err(FilletError::DegenerateEdge);
    }
    // next_id after remove: max remaining + 1
    let next = faces.iter().map(|f| f.id.0).max().unwrap_or(0).max(next_id - 1) + 1;
    apply_edges_fillet_batch_edge_radii(faces, &edge_radii, next)
}

/// Remove **one** edge fillet (by fillet face id): un-shrink all, re-apply others.
/// Plasticity-style delete anytime (P4).
pub fn remove_one_planar_fillet(
    faces: &mut Vec<NurbsFace>,
    fillet_face_id: NurbsFaceId,
) -> Result<(), FilletError> {
    let between = faces
        .iter()
        .find(|f| f.id == fillet_face_id)
        .and_then(|f| f.origin.fillet_between())
        .ok_or(FilletError::DegenerateEdge)?;
    let prior = remove_all_planar_edge_fillets(faces)?;
    // Drop edges that belonged to the removed fillet (match via support face ids).
    let id_to_idx: BTreeMap<u64, usize> = faces
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.0, i))
        .collect();
    let ia = id_to_idx.get(&between[0].0).copied();
    let ib = id_to_idx.get(&between[1].0).copied();
    let keep: Vec<(usize, u8, f32)> = prior
        .into_iter()
        .filter(|&(f, e, _)| {
            // Keep if this solid edge is NOT the shared edge of `between`.
            let Some(ia) = ia else {
                return true;
            };
            let Some(ib) = ib else {
                return true;
            };
            // Drop if edge is on ia or ib and geometrically the between-pair edge.
            if f == ia || f == ib {
                // If either support's recovered edge matches, skip when both endpoints
                // lie on both faces (the shared solid edge).
                if let Ok((a0, a1)) = face_edge_endpoints(faces[f].surface.as_ref(), e) {
                    let other = if f == ia { ib } else { ia };
                    for eo in 0u8..4 {
                        if let Ok((b0, b1)) =
                            face_edge_endpoints(faces[other].surface.as_ref(), eo)
                        {
                            let same = |p: Vec3, q: Vec3| (p - q).length_squared() < 1e-6;
                            if (same(a0, b0) && same(a1, b1)) || (same(a0, b1) && same(a1, b0))
                            {
                                return false; // this is the removed edge
                            }
                        }
                    }
                }
            }
            true
        })
        .collect();
    if keep.is_empty() {
        return Ok(()); // solid is fully sharp
    }
    let next = faces.iter().map(|f| f.id.0).max().unwrap_or(0) + 1;
    apply_edges_fillet_batch_edge_radii(faces, &keep, next)?;
    Ok(())
}

/// Adjust radius of one edge fillet (by fillet face id): un-shrink all, re-apply
/// with that edge's radius replaced. Plasticity-style edit anytime (P4).
pub fn adjust_one_planar_fillet_radius(
    faces: &mut Vec<NurbsFace>,
    fillet_face_id: NurbsFaceId,
    new_radius: f32,
) -> Result<FilletOutcome, FilletError> {
    let r_new = new_radius.max(1e-5);
    let between = faces
        .iter()
        .find(|f| f.id == fillet_face_id)
        .and_then(|f| f.origin.fillet_between())
        .ok_or(FilletError::DegenerateEdge)?;
    let prior = remove_all_planar_edge_fillets(faces)?;
    let id_to_idx: BTreeMap<u64, usize> = faces
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.0, i))
        .collect();
    let ia = id_to_idx.get(&between[0].0).copied();
    let ib = id_to_idx.get(&between[1].0).copied();
    let mut edge_radii = prior;
    for t in edge_radii.iter_mut() {
        let Some(ia) = ia else { continue };
        let Some(ib) = ib else { continue };
        if t.0 != ia && t.0 != ib {
            continue;
        }
        let other = if t.0 == ia { ib } else { ia };
        if let Ok((a0, a1)) = face_edge_endpoints(faces[t.0].surface.as_ref(), t.1) {
            for eo in 0u8..4 {
                if let Ok((b0, b1)) = face_edge_endpoints(faces[other].surface.as_ref(), eo) {
                    let same = |p: Vec3, q: Vec3| (p - q).length_squared() < 1e-6;
                    if (same(a0, b0) && same(a1, b1)) || (same(a0, b1) && same(a1, b0)) {
                        t.2 = r_new;
                    }
                }
            }
        }
    }
    let next = faces.iter().map(|f| f.id.0).max().unwrap_or(0) + 1;
    apply_edges_fillet_batch_edge_radii(faces, &edge_radii, next)
}

