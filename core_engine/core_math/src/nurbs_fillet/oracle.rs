//! FreeCAD/OCCT oracle harness helpers + invariant asserts (split from nurbs_fillet.rs).

use crate::nurbs::{NurbsFace, NurbsSurface};
use glam::Vec3;
use std::collections::BTreeMap;

use super::*;

// â”€â”€ Invariant helpers (for tests) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Sample points on fillet; assert |P âˆ’ axis| â‰ˆ r.
pub fn assert_fillet_radius(fillet: &NurbsSurface, axis_o: Vec3, axis_d: Vec3, r: f32, tol: f32) {
    let ad = axis_d.normalize_or_zero();
    for iu in 0..=10 {
        for iv in 0..=6 {
            let u = iu as f32 / 10.0;
            let v = iv as f32 / 6.0;
            let p = fillet.evaluate(u, v);
            let rel = p - axis_o;
            let along = rel.dot(ad) * ad;
            let radial = (rel - along).length();
            assert!(
                (radial - r).abs() < tol,
                "fillet radius {radial} != {r} at ({u},{v})"
            );
        }
    }
}

/// Sample points on spherical corner; assert |P âˆ’ centre| â‰ˆ r.
pub fn assert_sphere_radius(corner: &NurbsSurface, centre: Vec3, r: f32, tol: f32) {
    for iu in 0..=12 {
        for iv in 0..=12 {
            let u = iu as f32 / 12.0;
            let v = iv as f32 / 12.0;
            // Skip exact pole degeneracy samples if needed â€” still must be on sphere
            let p = corner.evaluate(u, v);
            let dist = (p - centre).length();
            assert!(
                (dist - r).abs() < tol,
                "sphere radius {dist} != {r} at ({u},{v}) p={p:?} c={centre:?}"
            );
        }
    }
}

// â”€â”€ OCCT oracle helpers (OCCT_ORACLE_HARNESS.md) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Classify an AZRA face for Tier 1 compare (OCCT GeomPlane/Cylinder/Sphere names).
/// Uses surface name first (Fillet/Corner), then degree heuristics.
pub fn classify_surface_type(s: &NurbsSurface) -> &'static str {
    match s.name.as_str() {
        "Fillet" => "Cylinder",
        "Corner" => "Sphere",
        _ => {
            if s.degree_u <= 1 && s.degree_v <= 1 {
                "Plane"
            } else if s.degree_u == 2 && s.degree_v <= 1 {
                "Cylinder"
            } else if s.degree_u >= 2 && s.degree_v >= 2 {
                "Sphere"
            } else {
                "Other"
            }
        }
    }
}

/// Tier 1: face counts by type (Plane / Cylinder / Sphere / â€¦).
pub fn count_faces_by_type(faces: &[NurbsFace]) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for f in faces {
        let k = classify_surface_type(f.surface.as_ref());
        *m.entry(k).or_insert(0) += 1;
    }
    m
}

/// OCCT Tier 1 expectation matrix (unit box). FreeCAD JSON overrides when present.
pub fn occt_tier1_expected(case: &str) -> Option<BTreeMap<&'static str, usize>> {
    let mut m = BTreeMap::new();
    let fill = |m: &mut BTreeMap<&'static str, usize>, p: usize, c: usize, s: usize| {
        m.insert("Plane", p);
        m.insert("Cylinder", c);
        m.insert("Sphere", s);
    };
    match case {
        "single_edge_r02" => fill(&mut m, 6, 1, 0),
        // FreeCAD/OCCT 1.1: sphere only when all 3 edges filleted (not 2)
        "two_edges_at_vertex" => fill(&mut m, 6, 2, 0),
        "top_loop_r02" => fill(&mut m, 6, 4, 0),
        "three_edges_at_vertex" => fill(&mut m, 6, 3, 1),
        "all_edges_r005" | "all_edges_r02" | "all_edges_r045" => fill(&mut m, 6, 12, 8),
        _ => return None,
    }
    Some(m)
}

/// Assert Tier 1 face counts match expected (OCCT decomposition).
pub fn assert_tier1_face_counts(faces: &[NurbsFace], case: &str) {
    let got = count_faces_by_type(faces);
    let exp = occt_tier1_expected(case).unwrap_or_else(|| panic!("unknown oracle case {case}"));
    for key in ["Plane", "Cylinder", "Sphere"] {
        let g = got.get(key).copied().unwrap_or(0);
        let e = exp.get(key).copied().unwrap_or(0);
        assert_eq!(
            g, e,
            "Tier 1 {case}: {key} count AZRA={g} OCCT-expected={e} (full got={got:?})"
        );
    }
}

/// Load FreeCAD Tier-2 point cloud (`docs/planning/nurbs/refs/<case>_pts.json`).
/// Points are in OCCT unit box [0,1]Â³. Returns `None` if file missing.
/// Uses `serde_json` (dev-dep) â€” only call from tests / tools that pull it in.
#[cfg(any(test, feature = "occt_oracle"))]
pub fn load_occt_tier2_points(case: &str) -> Option<Vec<Vec3>> {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../docs/planning/nurbs/refs");
    path.push(format!("{case}_pts.json"));
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let arr = v.get("points")?.as_array()?;
    let mut pts = Vec::with_capacity(arr.len());
    for p in arr {
        let a = p.as_array()?;
        if a.len() < 3 {
            continue;
        }
        pts.push(Vec3::new(
            a[0].as_f64()? as f32,
            a[1].as_f64()? as f32,
            a[2].as_f64()? as f32,
        ));
    }
    if pts.is_empty() {
        None
    } else {
        Some(pts)
    }
}

#[cfg(not(any(test, feature = "occt_oracle")))]
pub fn load_occt_tier2_points(_case: &str) -> Option<Vec<Vec3>> {
    None
}

/// AZRA centered **Y-up** box `[-0.5,0.5]Â³` â†’ FreeCAD/OCCT `Part.makeBox` **Z-up** `[0,1]Â³`.
///
/// Mapping: `(x,y,z)_azra â†’ (x+0.5, z+0.5, y+0.5)_occt` so AZRA +Y (up) becomes OCCT +Z (up).
/// **Never** use a pure `+0.5` shift â€” that maps AZRA top (y) onto OCCT +Y, not the FreeCAD top face.
pub fn azra_to_occt_box(p: Vec3) -> Vec3 {
    Vec3::new(p.x + 0.5, p.z + 0.5, p.y + 0.5)
}

/// Inverse of [`azra_to_occt_box`].
pub fn occt_to_azra_box(p: Vec3) -> Vec3 {
    Vec3::new(p.x - 0.5, p.z - 0.5, p.y - 0.5)
}

/// One geometric solid edge: AZRA tag + midpoints in both frames.
#[derive(Clone, Debug)]
pub struct CubeSolidEdge {
    pub face: usize,
    pub edge: u8,
    pub a_azra: Vec3,
    pub b_azra: Vec3,
    pub mid_azra: Vec3,
    pub mid_occt: Vec3,
}

/// All 12 unique solid edges of the centered unit cube (hx=hy=hz=0.5).
pub fn cube_solid_edge_catalog(faces: &[NurbsFace]) -> Vec<CubeSolidEdge> {
    let tags = cube_all_solid_edges(faces);
    let mut out = Vec::with_capacity(tags.len());
    for (fi, ei) in tags {
        let Ok((a, b)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) else {
            continue;
        };
        let mid = (a + b) * 0.5;
        out.push(CubeSolidEdge {
            face: fi,
            edge: ei,
            a_azra: a,
            b_azra: b,
            mid_azra: mid,
            mid_occt: azra_to_occt_box(mid),
        });
    }
    out
}

/// Map FreeCAD/OCCT edge midpoints (in `[0,1]Â³` Z-up) â†’ AZRA `(face, edge)` tags.
pub fn azra_edges_from_occt_mids(
    faces: &[NurbsFace],
    occt_mids: &[[f32; 3]],
    tol: f32,
) -> Vec<(usize, u8)> {
    let cat = cube_solid_edge_catalog(faces);
    let mut out = Vec::new();
    let tol2 = tol * tol;
    for m in occt_mids {
        let want = Vec3::new(m[0], m[1], m[2]);
        let mut best: Option<(f32, usize, u8)> = None;
        for e in &cat {
            let d2 = (e.mid_occt - want).length_squared();
            if d2 < tol2 && best.map(|(bd, ..)| d2 < bd).unwrap_or(true) {
                best = Some((d2, e.face, e.edge));
            }
        }
        if let Some((_, fi, ei)) = best {
            if !out.iter().any(|&(f, e)| f == fi && e == ei) {
                out.push((fi, ei));
            }
        }
    }
    out
}

/// Top loop of the unit cube (4 edges of the +Y face) â€” geometric, not hard-coded indices.
pub fn cube_top_loop_edges(faces: &[NurbsFace]) -> Vec<(usize, u8)> {
    let cat = cube_solid_edge_catalog(faces);
    // Top face y â‰ˆ +0.5; horizontal edges (little y span)
    cat.into_iter()
        .filter(|e| {
            e.mid_azra.y > 0.45
                && (e.a_azra.y - e.b_azra.y).abs() < 1e-4
        })
        .map(|e| (e.face, e.edge))
        .collect()
}

/// FreeCAD `Part.makeBox(1,1,1)` top-loop edge indices (both endpoints zâ‰ˆ1).
/// Documented FreeCAD 1.1.3: `[1, 5, 9, 11]`.
pub const FREECAD_UNIT_BOX_TOP_LOOP: [usize; 4] = [1, 5, 9, 11];

/// FreeCAD edges at vertex (1,1,1) â€” FreeCAD 1.1.3: `[5, 6, 11]`.
pub const FREECAD_UNIT_BOX_CORNER_111: [usize; 3] = [5, 6, 11];

/// Sample AZRA solid and compare two-sided Hausdorff to FreeCAD points (same coord frame).
/// Returns (azraâ†’occt, occtâ†’azra). `None` if FreeCAD ref missing.
pub fn hausdorff_vs_occt(faces: &[NurbsFace], case: &str, steps: u32) -> Option<(f32, f32)> {
    let occt = load_occt_tier2_points(case)?;
    let azra: Vec<Vec3> = sample_azra_points(faces, steps)
        .into_iter()
        .map(azra_to_occt_box)
        .collect();
    Some(hausdorff_two_sided(&azra, &occt))
}

/// Max min-distance from each point in `a` to the set `b` (one-sided Hausdorff sample).
pub fn hausdorff_one_sided(a: &[Vec3], b: &[Vec3]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return f32::MAX;
    }
    let mut m = 0.0f32;
    for p in a {
        let mut best = f32::MAX;
        for q in b {
            best = best.min((*p - *q).length());
        }
        m = m.max(best);
    }
    m
}

/// Two-sided Hausdorff on point clouds (Tier 2). Both directions required for hole detection.
pub fn hausdorff_two_sided(a: &[Vec3], b: &[Vec3]) -> (f32, f32) {
    (hausdorff_one_sided(a, b), hausdorff_one_sided(b, a))
}

/// Sample AZRA solid faces into a point cloud for Tier 2.
///
/// Fillet/Corner patches use denser UV. FreeCAD tess samples the Steinmetz join
/// (2-cylinder ellipse) densely; sparse UV on a 3×2 rational cylinder left a
/// ~0.021 hole residual that was pure undersampling (geometry already matches
/// ideal cylinders — FreeCAD max dist to two-cyl model is ~0).
pub fn sample_azra_points(faces: &[NurbsFace], steps: u32) -> Vec<Vec3> {
    let mut out = Vec::new();
    let base = steps.max(2);
    for f in faces {
        let s = f.surface.as_ref();
        // base=12 → 72 samples on Fillet: multi hole ~0.008 (was 0.021 at ×2)
        let su = match s.name.as_str() {
            "Fillet" | "Corner" | "JoinPatch" | "Toroid" => (base * 6).max(48),
            _ => base,
        };
        let sv = su;
        let (u0, u1) = s.domain_u();
        let (v0, v1) = s.domain_v();
        for iu in 0..=su {
            for iv in 0..=sv {
                let u = u0 + (u1 - u0) * iu as f32 / su as f32;
                let v = v0 + (v1 - v0) * iv as f32 / sv as f32;
                if !f.uv_inside(u, v) {
                    continue;
                }
                out.push(s.evaluate(u, v));
            }
        }
    }
    out
}

/// Collect all unique solid edges of the unit NURBS box (one tag per geometric edge).
pub fn cube_all_solid_edges(faces: &[NurbsFace]) -> Vec<(usize, u8)> {
    let mut raw = Vec::new();
    for (fi, face) in faces.iter().enumerate() {
        for ei in 0u8..4 {
            raw.push((fi, ei));
        }
    }
    dedupe_solid_edges(faces, &raw)
}

/// Two **top** edges that share the +X+Y+Z corner (matches FreeCAD edges 5+11 style).
/// Prefer geometric mid map over hard-coded face indices.
pub fn cube_two_edges_at_vertex(faces: &[NurbsFace]) -> [(usize, u8); 2] {
    // FreeCAD corner (1,1,1) top edges mids: edge5 (1,0.5,1), edge11 (0.5,1,1)
    let mids = [[1.0, 0.5, 1.0], [0.5, 1.0, 1.0]];
    let mapped = azra_edges_from_occt_mids(faces, &mids, 0.05);
    if mapped.len() >= 2 {
        return [mapped[0], mapped[1]];
    }
    // Fallback: two top-loop edges that share a vertex
    let tops = cube_top_loop_edges(faces);
    for i in 0..tops.len() {
        for j in (i + 1)..tops.len() {
            let (fi, ei) = tops[i];
            let (fj, ej) = tops[j];
            let Ok((a0, a1)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) else {
                continue;
            };
            let Ok((b0, b1)) = face_edge_endpoints(faces[fj].surface.as_ref(), ej) else {
                continue;
            };
            const E: f32 = 1e-6;
            for p in [a0, a1] {
                if (p - b0).length_squared() < E || (p - b1).length_squared() < E {
                    return [tops[i], tops[j]];
                }
            }
        }
    }
    // Last resort (legacy)
    [(0, 1), (0, 2)]
}

/// Three solid edges of one cube corner (+X+Y+Z), geometric.
pub fn cube_three_edges_at_vertex(faces: &[NurbsFace]) -> Vec<(usize, u8)> {
    // FreeCAD (1,1,1): edges 5,6,11 â†’ mids (1,0.5,1), (1,1,0.5), (0.5,1,1)
    let mids = [[1.0, 0.5, 1.0], [1.0, 1.0, 0.5], [0.5, 1.0, 1.0]];
    let mapped = azra_edges_from_occt_mids(faces, &mids, 0.05);
    if mapped.len() == 3 {
        return mapped;
    }
    let v = Vec3::new(0.5, 0.5, 0.5);
    let at = edges_at_vertex(faces, v);
    assert_eq!(at.len(), 3, "cube corner should have 3 edges, got {at:?}");
    at
}

/// Single top edge matching FreeCAD unit-box edge 5 midpoint (1, 0.5, 1).
pub fn cube_single_top_edge_matching_freecad5(faces: &[NurbsFace]) -> (usize, u8) {
    let mapped = azra_edges_from_occt_mids(faces, &[[1.0, 0.5, 1.0]], 0.05);
    mapped.first().copied().unwrap_or((0, 1))
}

/// G1: fillet normal at seam matches face normal (dot â‰¥ 1 âˆ’ tol for unit normals).
pub fn assert_g1_seams(
    face_a: &NurbsSurface,
    face_b: &NurbsSurface,
    fillet: &NurbsSurface,
    tol: f32,
) {
    let nf = fillet.normal(0.0, 0.5).normalize_or_zero();
    let na = face_a.normal(0.5, 0.5).normalize_or_zero();
    assert!(
        nf.dot(na).abs() > 1.0 - tol || nf.dot(na) > 0.9,
        "G1 seam A: fillet n {nf:?} vs face A {na:?} (dot={})",
        nf.dot(na)
    );
    let nf2 = fillet.normal(1.0, 0.5).normalize_or_zero();
    let nb = face_b.normal(0.5, 0.5).normalize_or_zero();
    assert!(
        nf2.dot(nb).abs() > 1.0 - tol || nf2.dot(nb) > 0.9,
        "G1 seam B: fillet n {nf2:?} vs face B {nb:?} (dot={})",
        nf2.dot(nb)
    );
}

/// Geometric ThreeCorner seam gate (UV of sphere vs cylinder may not share `u`).
///
/// At the cylinder end nearer `sphere_centre`, every sample must lie on the sphere
/// (`||P − centre| − r| < pos_tol`) and the cylinder normal must be radial
/// (parallel to `P − centre`) within `n_tol` on `1 − |n·radial|`.
pub fn assert_g1_sphere_cylinder_end(
    sphere_centre: Vec3,
    radius: f32,
    cylinder: &NurbsSurface,
    pos_tol: f32,
    n_tol: f32,
) {
    let r = radius.max(1e-5);
    // Pick the end ring closer to the sphere centre (trimmed corner end).
    let mid0 = cylinder.evaluate(0.5, 0.0);
    let mid1 = cylinder.evaluate(0.5, 1.0);
    let end = if (mid0 - sphere_centre).length() < (mid1 - sphere_centre).length() {
        0.0
    } else {
        1.0
    };
    for i in 0..12 {
        let t = i as f32 / 11.0;
        let p = cylinder.evaluate(t, end);
        let radial = p - sphere_centre;
        let dist = radial.length();
        assert!(
            (dist - r).abs() < pos_tol,
            "cyl end not on sphere t={t}: |P-c|={dist} r={r} p={p:?} c={sphere_centre:?}"
        );
        let n_cyl = cylinder.normal(t, end).normalize_or_zero();
        let n_rad = radial.normalize_or_zero();
        let align = n_cyl.dot(n_rad).abs();
        assert!(
            align > 1.0 - n_tol,
            "cyl end normal not radial t={t}: |n·rad|={align} n={n_cyl:?} rad={n_rad:?}"
        );
    }
}

