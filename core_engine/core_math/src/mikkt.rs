//! MikkTSpace-compatible tangent frames.
//!
//! This is the industry interchange basis (Substance / xNormal / glTF / UE): per-triangle
//! tangent from UV derivatives (Lengyel), accumulated at shared corners, then Gram-Schmidt
//! against the smoothed normal. Bitangent is reconstructed as `sign * N × T`.
//!
//! Full welded-vertex MikkT (Mikkel Gjøl's reference) is the same math on a weld that
//! groups position+normal+UV; callers that pass per-corner UVs already have that split.

use glam::{Vec2, Vec3};

/// Per-triangle tangent and bitangent from positions + UVs.
///
/// `None` when the UV triangle is degenerate (zero area in UV).
pub fn triangle_tangent_bitangent(
    p0: Vec3,
    p1: Vec3,
    p2: Vec3,
    uv0: Vec2,
    uv1: Vec2,
    uv2: Vec2,
) -> Option<(Vec3, Vec3)> {
    let e1 = p1 - p0;
    let e2 = p2 - p0;
    let d1 = uv1 - uv0;
    let d2 = uv2 - uv0;
    let det = d1.x * d2.y - d2.x * d1.y;
    if det.abs() < 1.0e-12 {
        return None;
    }
    let r = 1.0 / det;
    let t = (e1 * d2.y - e2 * d1.y) * r;
    let b = (e2 * d1.x - e1 * d2.x) * r;
    Some((t, b))
}

/// Gram-Schmidt: make `t` orthogonal to `n`. Zero if either is degenerate.
pub fn orthogonalize_tangent(n: Vec3, t: Vec3) -> Vec3 {
    let n = n.normalize_or_zero();
    if n == Vec3::ZERO {
        return Vec3::ZERO;
    }
    (t - n * n.dot(t)).normalize_or_zero()
}

/// Handedness of the (n, t, b) frame: `+1` right-handed, `-1` mirrored (typical UV flip).
pub fn tangent_handedness(n: Vec3, t: Vec3, b: Vec3) -> f32 {
    if n.cross(t).dot(b) < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// Reconstruct bitangent the way engines do: `sign * N × T`.
pub fn reconstruct_bitangent(n: Vec3, t: Vec3, sign: f32) -> Vec3 {
    n.cross(t).normalize_or_zero() * sign
}

/// Express `world_n` in the low-poly tangent frame `(t, b, n)`.
pub fn world_to_tangent(world_n: Vec3, t: Vec3, b: Vec3, n: Vec3) -> Vec3 {
    Vec3::new(world_n.dot(t), world_n.dot(b), world_n.dot(n))
}

/// Inverse of [`world_to_tangent`] — the relight path.
pub fn tangent_to_world(ts: Vec3, t: Vec3, b: Vec3, n: Vec3) -> Vec3 {
    (t * ts.x + b * ts.y + n * ts.z).normalize_or_zero()
}

/// Standard tangent-space normal-map encode: `n * 0.5 + 0.5` (OpenGL +Y).
pub fn encode_tangent_normal(ts: Vec3) -> [u8; 3] {
    let v = ts * 0.5 + Vec3::splat(0.5);
    [
        (v.x.clamp(0.0, 1.0) * 255.0).round() as u8,
        (v.y.clamp(0.0, 1.0) * 255.0).round() as u8,
        (v.z.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// Inverse of [`encode_tangent_normal`].
pub fn decode_tangent_normal(rgb: [u8; 3]) -> Vec3 {
    Vec3::new(
        rgb[0] as f32 / 255.0 * 2.0 - 1.0,
        rgb[1] as f32 / 255.0 * 2.0 - 1.0,
        rgb[2] as f32 / 255.0 * 2.0 - 1.0,
    )
    .normalize_or_zero()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GATE — UV-aligned unit quad on XY: T = +X, B = +Y.
    #[test]
    fn uv_aligned_quad_is_plus_x_plus_y() {
        let (t, b) = triangle_tangent_bitangent(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
        )
        .expect("non-degenerate UV");
        let t = t.normalize();
        let b = b.normalize();
        assert!((t - Vec3::X).length() < 1e-5, "T={t}");
        assert!((b - Vec3::Y).length() < 1e-5, "B={b}");
        assert!(tangent_handedness(Vec3::Z, t, b) > 0.0);
    }

    #[test]
    fn degenerate_uv_returns_none() {
        assert!(triangle_tangent_bitangent(
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            Vec2::ZERO,
            Vec2::ZERO,
            Vec2::ZERO,
        )
        .is_none());
    }

    #[test]
    fn orthogonalize_strips_normal_component() {
        let n = Vec3::Z;
        let t = Vec3::new(1.0, 0.0, 0.5);
        let o = orthogonalize_tangent(n, t);
        assert!(o.dot(n).abs() < 1e-6);
        assert!((o - Vec3::X).length() < 1e-5);
    }

    /// GATE (G3 relight) — encode high n in TBN, decode, get the same world normal.
    #[test]
    fn relight_roundtrip_within_8bit_tolerance() {
        let n = Vec3::Z;
        let t = Vec3::X;
        let b = Vec3::Y;
        // A 30° tilt toward +X — the analytic flat-low / displaced-high case.
        let high = Vec3::new(0.5, 0.0, 0.866_025_4).normalize();
        let ts = world_to_tangent(high, t, b, n);
        let rgb = encode_tangent_normal(ts);
        let ts2 = decode_tangent_normal(rgb);
        let rec = tangent_to_world(ts2, t, b, n);
        assert!(
            rec.dot(high) > 0.995,
            "relight n·high = {} (rec={rec} high={high} rgb={rgb:?})",
            rec.dot(high)
        );
    }

    #[test]
    fn mirrored_uv_is_left_handed() {
        // V flipped: (0,1),(1,1),(1,0) on the same XYZ triangle.
        let (t, b) = triangle_tangent_bitangent(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 0.0),
        )
        .unwrap();
        assert!(tangent_handedness(Vec3::Z, t.normalize(), b.normalize()) < 0.0);
    }
}
