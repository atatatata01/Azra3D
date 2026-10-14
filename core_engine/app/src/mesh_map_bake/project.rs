//! G2 high→low projection.
//!
//! Adds high-poly samples onto [`UvGeoBuffer`] — it does **not** replace the low-poly
//! position/normal (G3 needs that frame). All high geometry is stored in **low-local** space.
//!
//! Curvature stays LOW-geo (`kappa` from the rasterised display mesh). Measuring κ on the high
//! surface at the hit is a later addition; Position / Object-Normal / ID / AO / Thickness read
//! the projected high sample.

use super::raster::UvGeoBuffer;
use crate::bake_diag::BakeNotes;
use core_math::bvh::{blas_nearest_hit, build_tri_blas, TriBlas};
use glam::Vec3;

/// Inflated cage envelope in low-local space. Projection rays along +n stop here
/// instead of the numeric `max_front` (Substance / xNormal mesh-cage).
#[derive(Clone)]
pub(crate) struct CageTarget {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub blas: TriBlas,
}

impl CageTarget {
    pub fn from_soup(positions: Vec<[f32; 3]>, indices: Vec<u32>) -> Option<Self> {
        if indices.len() < 3 {
            return None;
        }
        let blas = build_tri_blas(&positions, &indices);
        if blas.is_empty() {
            return None;
        }
        Some(Self {
            positions,
            indices,
            blas,
        })
    }
}

/// High-poly triangles already transformed into the LOW body's local space.
#[derive(Clone)]
pub(crate) struct HighTarget {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub n0: Vec<Vec3>,
    pub n1: Vec<Vec3>,
    pub n2: Vec<Vec3>,
    pub tri_face: Vec<u32>,
    pub face_group: Vec<u16>,
    pub blas: TriBlas,
}

impl HighTarget {
    /// Build from already-low-local triangle soup.
    pub fn from_soup(
        positions: Vec<[f32; 3]>,
        indices: Vec<u32>,
        n0: Vec<Vec3>,
        n1: Vec<Vec3>,
        n2: Vec<Vec3>,
        tri_face: Vec<u32>,
        face_group: Vec<u16>,
    ) -> Option<Self> {
        if indices.len() < 3 {
            return None;
        }
        let blas = build_tri_blas(&positions, &indices);
        if blas.is_empty() {
            return None;
        }
        Some(Self {
            positions,
            indices,
            n0,
            n1,
            n2,
            tri_face,
            face_group,
            blas,
        })
    }
}

/// Strip `_low` / `_high` (and a few cousins) so `Bolt_low` matches `Bolt_high`.
/// Does **not** strip `_cage` — a cage body must not resolve as a high-poly source.
pub(crate) fn bake_name_stem(name: &str) -> String {
    let n = name.trim();
    let lower = n.to_ascii_lowercase();
    for suf in ["_high", "_low", "_hp", "_lp"] {
        if lower.ends_with(suf) {
            return n[..n.len() - suf.len()].to_string();
        }
    }
    n.to_string()
}

/// `Bolt_cage` (and cousins) — G7 mesh-cage auto-match.
pub(crate) fn is_cage_name(name: &str) -> bool {
    name.trim().to_ascii_lowercase().ends_with("_cage")
}

/// Stem used to pair `Bolt_low` with `Bolt_cage`.
pub(crate) fn cage_name_stem(name: &str) -> String {
    let n = name.trim();
    let lower = n.to_ascii_lowercase();
    if lower.ends_with("_cage") {
        bake_name_stem(&n[..n.len() - 5])
    } else {
        bake_name_stem(n)
    }
}

/// Seed high_* from the low raster (self-bake), then project if a high target is present.
///
/// Returns the miss count among covered texels.
pub(crate) fn apply_projection(
    buf: &mut UvGeoBuffer,
    high: Option<&HighTarget>,
    max_front: f32,
    max_rear: f32,
    notes: &mut BakeNotes,
    cage: Option<&CageTarget>,
) -> u32 {
    let n = (buf.res * buf.res) as usize;
    buf.high_position = buf.position.clone();
    buf.high_normal = buf.normal.clone();
    buf.high_face = buf.face.clone();
    buf.high_group = vec![0; n];
    buf.missed = vec![0; n];

    let Some(high) = high else {
        return 0;
    };

    let numeric_front = max_front.max(0.0);
    let rear = max_rear.max(0.0);
    let diag = (buf.bounds.1 - buf.bounds.0).length().max(1.0e-4);
    let cage_search = (diag * 4.0).max(numeric_front).max(1.0e-3);
    let mut misses = 0u32;
    let mut cage_hits = 0u32;
    let mut cage_misses = 0u32;
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let p = buf.position[i];
        let nrm = buf.normal[i];
        if nrm.length_squared() < 1.0e-12 {
            buf.missed[i] = 1;
            misses += 1;
            continue;
        }
        let nrm = nrm.normalize();
        let bias = 1.0e-4;
        let orig = [p.x, p.y, p.z];
        let fd = [nrm.x, nrm.y, nrm.z];
        let rd = [-nrm.x, -nrm.y, -nrm.z];
        let front_o = [orig[0] + fd[0] * bias, orig[1] + fd[1] * bias, orig[2] + fd[2] * bias];
        let rear_o = [orig[0] + rd[0] * bias, orig[1] + rd[1] * bias, orig[2] + rd[2] * bias];

        // Mesh cage: per-texel front is the hit on the envelope. Miss → numeric Max Front.
        let front = if let Some(cg) = cage {
            match blas_nearest_hit(&cg.blas, &cg.positions, &cg.indices, front_o, fd, cage_search)
            {
                Some((t, _)) => {
                    cage_hits += 1;
                    t.max(0.0)
                }
                None => {
                    cage_misses += 1;
                    numeric_front
                }
            }
        } else {
            numeric_front
        };

        let fh = if front > 0.0 {
            blas_nearest_hit(&high.blas, &high.positions, &high.indices, front_o, fd, front)
        } else {
            None
        };
        let rh = if rear > 0.0 {
            blas_nearest_hit(&high.blas, &high.positions, &high.indices, rear_o, rd, rear)
        } else {
            None
        };
        let pick = match (fh, rh) {
            (Some(f), Some(r)) => Some(if f.0 <= r.0 { (f, 1.0) } else { (r, -1.0) }),
            (Some(f), None) => Some((f, 1.0)),
            (None, Some(r)) => Some((r, -1.0)),
            (None, None) => None,
        };
        let Some(((t, tri), sign)) = pick else {
            buf.missed[i] = 1;
            misses += 1;
            continue;
        };
        let dir = nrm * sign;
        let hit_p = p + dir * (bias + t);
        let ti = tri as usize;
        let (a, b, c) = (
            Vec3::from_array(high.positions[high.indices[ti * 3] as usize]),
            Vec3::from_array(high.positions[high.indices[ti * 3 + 1] as usize]),
            Vec3::from_array(high.positions[high.indices[ti * 3 + 2] as usize]),
        );
        let (w0, w1, w2) = barycentric(hit_p, a, b, c);
        let hn = (high.n0[ti] * w0 + high.n1[ti] * w1 + high.n2[ti] * w2).normalize_or_zero();
        buf.high_position[i] = hit_p;
        buf.high_normal[i] = if hn == Vec3::ZERO { nrm } else { hn };
        buf.high_face[i] = high.tri_face.get(ti).copied().unwrap_or(u32::MAX);
        let fi = buf.high_face[i];
        buf.high_group[i] = high.face_group.get(fi as usize).copied().unwrap_or(0);
    }

    if misses > 0 {
        notes.push(format!(
            "Bake Mesh Maps: {misses} texels missed the high-poly (rear distance too small?)"
        ));
    }
    if cage.is_some() && cage_misses > 0 {
        notes.push(format!(
            "Bake Mesh Maps: {cage_misses} texels missed the cage — used Max Front"
        ));
    }
    if cage.is_some() && cage_hits > 0 {
        notes.push(format!(
            "Bake Mesh Maps: cage envelope hit {cage_hits} texels"
        ));
    }

    // Position map is AABB-normalised — include the high surface so a bevelled high does
    // not clamp outside 0..1.
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let mut any = false;
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        lo = lo.min(buf.high_position[i]);
        hi = hi.max(buf.high_position[i]);
        any = true;
    }
    if any {
        buf.bounds = (lo, hi);
    }
    buf.miss_count = misses;
    misses
}

fn barycentric(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> (f32, f32, f32) {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);
    let den = d00 * d11 - d01 * d01;
    if den.abs() < 1.0e-20 {
        return (1.0, 0.0, 0.0);
    }
    let v = (d11 * d20 - d01 * d21) / den;
    let w = (d00 * d21 - d01 * d20) / den;
    let u = 1.0 - v - w;
    (u, v, w)
}

/// Flatten a ring mesh to a triangle soup in the same space as `positions`.
pub(crate) fn triangulate_rings(
    rings: &core_math::mesh_curvature::RingMesh,
    normals: &[Vec3],
) -> (Vec<[f32; 3]>, Vec<u32>, Vec<Vec3>, Vec<Vec3>, Vec<Vec3>, Vec<u32>) {
    let positions: Vec<[f32; 3]> = rings.positions.iter().map(|p| [p.x, p.y, p.z]).collect();
    let mut indices = Vec::new();
    let mut n0 = Vec::new();
    let mut n1 = Vec::new();
    let mut n2 = Vec::new();
    let mut tri_face = Vec::new();
    let mut pts: Vec<Vec3> = Vec::with_capacity(8);
    for fi in 0..rings.face_count() {
        let lo = rings.face_start[fi] as usize;
        let hi = rings.face_start[fi + 1] as usize;
        let ring = &rings.face_ring[lo..hi];
        if ring.len() < 3 {
            continue;
        }
        pts.clear();
        pts.extend(ring.iter().map(|&v| rings.positions[v as usize]));
        let fnorm = core_math::half_edge::newell_normal(&pts).normalize_or_zero();
        for t in core_math::poly_mesh::earclip_polygon(&pts, fnorm) {
            let ia = ring[t[0] as usize];
            let ib = ring[t[1] as usize];
            let ic = ring[t[2] as usize];
            indices.extend_from_slice(&[ia, ib, ic]);
            n0.push(normals.get(ia as usize).copied().unwrap_or(fnorm));
            n1.push(normals.get(ib as usize).copied().unwrap_or(fnorm));
            n2.push(normals.get(ic as usize).copied().unwrap_or(fnorm));
            tri_face.push(fi as u32);
        }
    }
    (positions, indices, n0, n1, n2, tri_face)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_map_bake::raster::rasterize_uv_geo;
    use crate::mesh_map_bake::source::{PolyCurvSource, PolySampleSpace};
    use crate::mesh_map_bake::maps::bake_vector_pixels;
    use crate::mesh_map_bake::MeshMapKind;
    use core_math::mesh_curvature::RingMesh;
    use glam::Vec2;

    fn quad(z: f32) -> RingMesh {
        RingMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, z),
                Vec3::new(1.0, 0.0, z),
                Vec3::new(1.0, 1.0, z),
                Vec3::new(0.0, 1.0, z),
            ],
            face_start: vec![0, 4],
            face_ring: vec![0, 1, 2, 3],
        }
    }

    fn src_at(z: f32) -> PolyCurvSource {
        PolyCurvSource {
            rings: quad(z),
            corner_uv: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ],
            space: PolySampleSpace::default(),
        }
    }

    fn high_from_rings(rings: &RingMesh) -> HighTarget {
        let nrm = vec![Vec3::Z; rings.positions.len()];
        let (p, i, a, b, c, f) = triangulate_rings(rings, &nrm);
        HighTarget::from_soup(p, i, a, b, c, f, vec![0]).expect("high soup")
    }

    #[test]
    fn bake_name_stem_strips_low_high() {
        assert_eq!(bake_name_stem("Bolt_low"), "Bolt");
        assert_eq!(bake_name_stem("Bolt_HIGH"), "Bolt");
        assert_eq!(bake_name_stem("Hull"), "Hull");
        assert_eq!(bake_name_stem("part_lp"), "part");
        assert_eq!(bake_name_stem("Bolt_cage"), "Bolt_cage");
        assert!(is_cage_name("Bolt_cage"));
        assert!(is_cage_name("hull_CAGE"));
        assert!(!is_cage_name("Bolt_high"));
        assert_eq!(cage_name_stem("Bolt_cage"), "Bolt");
        assert_eq!(cage_name_stem("Bolt_low"), "Bolt");
    }

    /// GATE (G2) — a plane lifted 0.25 along Z is sampled, not the low z=0 surface.
    #[test]
    fn project_displaced_plane_samples_high() {
        let src = src_at(0.0);
        let mut geo = rasterize_uv_geo(&src, 32, 0.0);
        let high = high_from_rings(&quad(0.25));
        let mut notes = BakeNotes::default();
        let misses = apply_projection(&mut geo, Some(&high), 1.0, 1.0, &mut notes, None);
        assert_eq!(misses, 0, "sane cage must hit the lifted plane");
        let mut seen = 0u32;
        for i in 0..geo.position.len() {
            if !geo.covered(i) {
                continue;
            }
            assert!(
                (geo.high_position[i].z - 0.25).abs() < 0.02,
                "high z={}, low z={}",
                geo.high_position[i].z,
                geo.position[i].z
            );
            assert!(
                geo.position[i].z.abs() < 0.02,
                "low frame must stay at z=0, got {}",
                geo.position[i].z
            );
            seen += 1;
        }
        assert!(seen > 16);
        let px = bake_vector_pixels(&geo, MeshMapKind::Position).unwrap();
        // Flat Z after AABB-normalise is 0 (lo.z == hi.z). X still spans the quad.
        let mut x_hi = 0u32;
        for i in 0..geo.position.len() {
            if geo.covered(i) {
                x_hi = x_hi.max(px[i * 4] as u32);
            }
        }
        assert!(x_hi > 200, "position map must write the high XY, peak x byte={x_hi}");
    }

    /// GATE (G2 negative) — rear=0.001 cannot reach a plane 0.1 behind the low surface.
    #[test]
    fn project_misses_when_rear_too_small() {
        let src = src_at(0.0);
        let mut geo = rasterize_uv_geo(&src, 16, 0.0);
        let high = high_from_rings(&quad(-0.1));
        let mut notes = BakeNotes::default();
        let misses = apply_projection(&mut geo, Some(&high), 0.001, 0.001, &mut notes, None);
        assert!(misses > 0, "tiny cage must miss a plane 0.1 behind");
        assert!(
            notes.summary().contains("missed"),
            "log must mention misses: {}",
            notes.summary()
        );
        // Fallback is self-bake: high position stays at low z=0.
        for i in 0..geo.position.len() {
            if geo.covered(i) && geo.missed[i] == 1 {
                assert!(geo.high_position[i].z.abs() < 0.02);
            }
        }
    }

    /// GATE (G2) — high verts authored at z=0, then translated +0.3 in low-local, are hit.
    #[test]
    fn project_respects_high_transform() {
        let src = src_at(0.0);
        let mut geo = rasterize_uv_geo(&src, 16, 0.0);
        let mut high = high_from_rings(&quad(0.0));
        for p in &mut high.positions {
            p[2] += 0.3;
        }
        high.blas = build_tri_blas(&high.positions, &high.indices);
        let mut notes = BakeNotes::default();
        let misses = apply_projection(&mut geo, Some(&high), 1.0, 0.01, &mut notes, None);
        assert_eq!(misses, 0);
        for i in 0..geo.position.len() {
            if geo.covered(i) {
                assert!(
                    (geo.high_position[i].z - 0.3).abs() < 0.02,
                    "transformed high z={}",
                    geo.high_position[i].z
                );
            }
        }
    }

    /// GATE (G3) — UV-aligned low quad has T=+X, B=+Y. A 30° high tilt encodes and relights.
    #[test]
    fn tangent_normal_flat_low_tilted_high_relights() {
        use crate::mesh_map_bake::maps::bake_tangent_normal_pixels;
        use core_math::mikkt::{decode_tangent_normal, tangent_to_world};

        let src = src_at(0.0);
        let mut geo = rasterize_uv_geo(&src, 32, 0.0);
        apply_projection(&mut geo, None, 0.0, 0.0, &mut BakeNotes::default(), None);
        let tilt = Vec3::new(0.5, 0.0, 0.866_025_4).normalize();
        for i in 0..geo.position.len() {
            if geo.covered(i) {
                geo.high_normal[i] = tilt;
            }
        }
        let px = bake_tangent_normal_pixels(&geo).unwrap();
        let mut checked = 0u32;
        for i in 0..geo.position.len() {
            if !geo.covered(i) || px[i * 4 + 3] == 0 {
                continue;
            }
            assert!(
                (geo.low_tangent[i] - Vec3::X).length() < 0.05,
                "T={}",
                geo.low_tangent[i]
            );
            assert!(
                (geo.low_bitangent[i] - Vec3::Y).length() < 0.05,
                "B={}",
                geo.low_bitangent[i]
            );
            let ts = decode_tangent_normal([px[i * 4], px[i * 4 + 1], px[i * 4 + 2]]);
            let rec = tangent_to_world(ts, geo.low_tangent[i], geo.low_bitangent[i], geo.normal[i]);
            assert!(
                rec.dot(tilt) > 0.99,
                "relight n·high={} rec={rec}",
                rec.dot(tilt)
            );
            checked += 1;
        }
        assert!(checked > 16);
    }

    fn cage_from_rings(rings: &RingMesh) -> CageTarget {
        let nrm = vec![Vec3::Z; rings.positions.len()];
        let (p, i, _, _, _, _) = triangulate_rings(rings, &nrm);
        CageTarget::from_soup(p, i).expect("cage soup")
    }

    /// GATE (G7 cage) — high beyond the cage envelope is clipped even if Max Front is huge.
    #[test]
    fn cage_clips_high_beyond_envelope() {
        let src = src_at(0.0);
        let mut geo = rasterize_uv_geo(&src, 32, 0.0);
        let high = high_from_rings(&quad(0.5));
        let cage = cage_from_rings(&quad(0.1));
        let mut notes = BakeNotes::default();
        let misses = apply_projection(&mut geo, Some(&high), 10.0, 0.0, &mut notes, Some(&cage));
        assert!(misses > 0, "high past the cage must miss");
        for i in 0..geo.position.len() {
            if geo.covered(i) && geo.missed[i] == 1 {
                assert!(geo.high_position[i].z.abs() < 0.02, "fallback stays on low");
            }
        }
    }

    /// GATE (G7 cage) — cage extends the ray past a too-small numeric Max Front.
    #[test]
    fn cage_extends_past_numeric_front() {
        let src = src_at(0.0);
        let mut geo = rasterize_uv_geo(&src, 32, 0.0);
        let high = high_from_rings(&quad(0.25));
        let cage = cage_from_rings(&quad(0.5));
        let mut notes = BakeNotes::default();
        let misses = apply_projection(&mut geo, Some(&high), 0.01, 0.0, &mut notes, Some(&cage));
        assert_eq!(misses, 0, "cage must reach the high at z=0.25, notes={}", notes.summary());
        let mut seen = 0u32;
        for i in 0..geo.position.len() {
            if !geo.covered(i) {
                continue;
            }
            assert!(
                (geo.high_position[i].z - 0.25).abs() < 0.02,
                "high z={}",
                geo.high_position[i].z
            );
            seen += 1;
        }
        assert!(seen > 16);
    }
}
