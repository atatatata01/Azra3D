//! Shared UV-space geometry buffer. One raster per bake; every map reads it.

use super::source::PolyCurvSource;
use glam::{Vec2, Vec3};

/// Per-texel surface sample in UV space — the shared basis for EVERY mesh map.
pub(crate) struct UvGeoBuffer {
    pub res: u32,
    pub position: Vec<Vec3>,
    pub normal: Vec<Vec3>,
    /// Raw signed curvature per texel (pre-shaping). G0: curvature encodes this, it does not
    /// re-raster.
    pub kappa: Vec<f32>,
    pub coverage: Vec<f32>,
    /// Source FACE index per texel (`u32::MAX` = uncovered).
    pub face: Vec<u32>,
    pub bounds: (Vec3, Vec3),
    /// Projected high-poly sample (D7). Self-bake: copies of the low fields.
    pub high_position: Vec<Vec3>,
    pub high_normal: Vec<Vec3>,
    pub high_face: Vec<u32>,
    pub high_group: Vec<u16>,
    /// 1 = this covered texel missed the high-poly (fallback is the low sample).
    pub missed: Vec<u8>,
    pub miss_count: u32,
    /// Low-poly tangent frame (G3). Zero until MikkT lands.
    pub low_tangent: Vec<Vec3>,
    pub low_bitangent: Vec<Vec3>,
}

impl UvGeoBuffer {
    #[inline]
    pub fn covered(&self, i: usize) -> bool {
        self.coverage[i] > 1.0e-8
    }
}

/// Rasterise position, normal and κ per texel in UV space.
///
/// 4×4 coverage supersample — not G6 output-SS. Same triangulation as the drawn mesh
/// (`earclip_polygon` on the 3D ring).
pub(crate) fn rasterize_uv_geo(src: &PolyCurvSource, res: u32, curv_radius: f32) -> UvGeoBuffer {
    rasterize_uv_geo_in(src, res, curv_radius, Vec2::ZERO, true)
}

/// Rasterise one UV window. `origin` is subtracted from corner UVs (UDIM tile).
/// `wrap`: current 0–1 behaviour (`rem_euclid`). Off: clip to this tile only.
pub(crate) fn rasterize_uv_geo_in(
    src: &PolyCurvSource,
    res: u32,
    curv_radius: f32,
    origin: Vec2,
    wrap: bool,
) -> UvGeoBuffer {
    use core_math::mesh_curvature::{
        average_edge_length_rings, smooth_iters_for_radius, vertex_curvature_and_normals_rings,
    };
    let res = res.max(8);
    let n = (res * res) as usize;
    let avg_edge = average_edge_length_rings(&src.rings);
    let iters = smooth_iters_for_radius(avg_edge, curv_radius);
    let (kappa_v, normal_v) = vertex_curvature_and_normals_rings(&src.rings, iters);
    let (tan_c, bit_c) = corner_tangents(src);

    let mut buf = UvGeoBuffer {
        res,
        position: vec![Vec3::ZERO; n],
        normal: vec![Vec3::ZERO; n],
        kappa: vec![0.0; n],
        coverage: vec![0.0; n],
        face: vec![u32::MAX; n],
        bounds: src.rings.aabb(),
        high_position: vec![Vec3::ZERO; n],
        high_normal: vec![Vec3::ZERO; n],
        high_face: vec![u32::MAX; n],
        high_group: vec![0; n],
        missed: vec![0; n],
        miss_count: 0,
        low_tangent: vec![Vec3::ZERO; n],
        low_bitangent: vec![Vec3::ZERO; n],
    };

    const SS: i32 = 4;
    let r = res as f32;
    let mut pts: Vec<Vec3> = Vec::with_capacity(8);
    for fi in 0..src.rings.face_count() {
        let lo = src.rings.face_start[fi] as usize;
        let hi = src.rings.face_start[fi + 1] as usize;
        let ring = &src.rings.face_ring[lo..hi];
        if ring.len() < 3 {
            continue;
        }
        pts.clear();
        pts.extend(ring.iter().map(|&v| src.rings.positions[v as usize]));
        let fnorm = core_math::half_edge::newell_normal(&pts).normalize_or_zero();
        for tri in core_math::poly_mesh::earclip_polygon(&pts, fnorm) {
            let c = |k: u32| -> (glam::Vec2, Vec3, Vec3, f32, Vec3, Vec3) {
                let l = k as usize;
                let v = ring[l] as usize;
                (
                    src.corner_uv[lo + l],
                    pts[l],
                    normal_v[v],
                    kappa_v[v],
                    tan_c[lo + l],
                    bit_c[lo + l],
                )
            };
            let (uv0, p0, n0, k0, t0, b0) = c(tri[0]);
            let (uv1, p1, n1, k1, t1, b1) = c(tri[1]);
            let (uv2, p2, n2, k2, t2, b2) = c(tri[2]);
            let a = glam::Vec2::new((uv0.x - origin.x) * r - 0.5, (uv0.y - origin.y) * r - 0.5);
            let b = glam::Vec2::new((uv1.x - origin.x) * r - 0.5, (uv1.y - origin.y) * r - 0.5);
            let cc = glam::Vec2::new((uv2.x - origin.x) * r - 0.5, (uv2.y - origin.y) * r - 0.5);
            let den = (b.x - a.x) * (cc.y - a.y) - (b.y - a.y) * (cc.x - a.x);
            if den.abs() < 1.0e-12 {
                continue;
            }
            let inv = 1.0 / den;
            let min_x = a.x.min(b.x).min(cc.x).floor() as i64 - 1;
            let max_x = a.x.max(b.x).max(cc.x).ceil() as i64 + 1;
            let min_y = a.y.min(b.y).min(cc.y).floor() as i64 - 1;
            let max_y = a.y.max(b.y).max(cc.y).ceil() as i64 + 1;
            if (max_x - min_x).max(max_y - min_y) > res as i64 * 4 {
                continue;
            }
            if !wrap
                && (max_x < 0 || max_y < 0 || min_x >= res as i64 || min_y >= res as i64)
            {
                continue;
            }
            let step = 1.0 / SS as f32;
            for py in min_y..=max_y {
                for px in min_x..=max_x {
                    let mut acc = (Vec3::ZERO, Vec3::ZERO, 0.0f32, Vec3::ZERO, Vec3::ZERO);
                    let mut hits = 0i32;
                    for sy in 0..SS {
                        for sx in 0..SS {
                            let s = glam::Vec2::new(
                                px as f32 - 0.5 + (sx as f32 + 0.5) * step,
                                py as f32 - 0.5 + (sy as f32 + 0.5) * step,
                            );
                            let w0 = ((b.x - s.x) * (cc.y - s.y) - (b.y - s.y) * (cc.x - s.x)) * inv;
                            let w1 = ((cc.x - s.x) * (a.y - s.y) - (cc.y - s.y) * (a.x - s.x)) * inv;
                            let w2 = 1.0 - w0 - w1;
                            const TOL: f32 = 1.0e-6;
                            if w0 < -TOL || w1 < -TOL || w2 < -TOL {
                                continue;
                            }
                            acc.0 += p0 * w0 + p1 * w1 + p2 * w2;
                            acc.1 += n0 * w0 + n1 * w1 + n2 * w2;
                            acc.2 += k0 * w0 + k1 * w1 + k2 * w2;
                            acc.3 += t0 * w0 + t1 * w1 + t2 * w2;
                            acc.4 += b0 * w0 + b1 * w1 + b2 * w2;
                            hits += 1;
                        }
                    }
                    if hits == 0 {
                        continue;
                    }
                    let (x, y) = if wrap {
                        (
                            (px.rem_euclid(res as i64)) as usize,
                            (py.rem_euclid(res as i64)) as usize,
                        )
                    } else {
                        if px < 0 || py < 0 || px >= res as i64 || py >= res as i64 {
                            continue;
                        }
                        (px as usize, py as usize)
                    };
                    let i = y * res as usize + x;
                    let cov = hits as f32 / (SS * SS) as f32;
                    let inv_h = 1.0 / hits as f32;
                    buf.position[i] += acc.0 * inv_h * cov;
                    buf.normal[i] += acc.1 * inv_h * cov;
                    buf.kappa[i] += acc.2 * inv_h * cov;
                    buf.low_tangent[i] += acc.3 * inv_h * cov;
                    buf.low_bitangent[i] += acc.4 * inv_h * cov;
                    buf.coverage[i] += cov;
                    buf.face[i] = fi as u32;
                }
            }
        }
    }
    for i in 0..n {
        if buf.coverage[i] > 1.0e-8 {
            let inv = 1.0 / buf.coverage[i];
            buf.position[i] *= inv;
            buf.normal[i] = (buf.normal[i] * inv).normalize_or_zero();
            buf.kappa[i] *= inv;
            let t = buf.low_tangent[i] * inv;
            let b = buf.low_bitangent[i] * inv;
            let tan = core_math::mikkt::orthogonalize_tangent(buf.normal[i], t);
            let sign = core_math::mikkt::tangent_handedness(buf.normal[i], tan, b);
            buf.low_tangent[i] = tan;
            buf.low_bitangent[i] =
                core_math::mikkt::reconstruct_bitangent(buf.normal[i], tan, sign);
        }
    }
    buf
}

/// Per-CORNER tangent/bitangent, parallel to `src.corner_uv` / `face_ring`.
fn corner_tangents(src: &PolyCurvSource) -> (Vec<Vec3>, Vec<Vec3>) {
    let n = src.rings.face_ring.len();
    let mut tan = vec![Vec3::ZERO; n];
    let mut bit = vec![Vec3::ZERO; n];
    let mut pts: Vec<Vec3> = Vec::with_capacity(8);
    for fi in 0..src.rings.face_count() {
        let lo = src.rings.face_start[fi] as usize;
        let hi = src.rings.face_start[fi + 1] as usize;
        let ring = &src.rings.face_ring[lo..hi];
        if ring.len() < 3 {
            continue;
        }
        pts.clear();
        pts.extend(ring.iter().map(|&v| src.rings.positions[v as usize]));
        let fnorm = core_math::half_edge::newell_normal(&pts).normalize_or_zero();
        for tri in core_math::poly_mesh::earclip_polygon(&pts, fnorm) {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;
            let Some((t, b)) = core_math::mikkt::triangle_tangent_bitangent(
                pts[i0],
                pts[i1],
                pts[i2],
                src.corner_uv[lo + i0],
                src.corner_uv[lo + i1],
                src.corner_uv[lo + i2],
            ) else {
                continue;
            };
            tan[lo + i0] += t;
            tan[lo + i1] += t;
            tan[lo + i2] += t;
            bit[lo + i0] += b;
            bit[lo + i1] += b;
            bit[lo + i2] += b;
        }
    }
    (tan, bit)
}
