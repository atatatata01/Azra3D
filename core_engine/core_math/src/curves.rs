// core_engine/core_math/src/curves.rs
// Shared curve system — one curve type used by all tools
// (clip curve, curve extrude shape, future trim/sweep/profile)
use glam::{Vec2, Vec3};
use serde::{Serialize, Deserialize};

/// Maximum polygon vertices for GPU serialization (fixed allocation in HOT buffer).
/// 14 verts * 3 floats = 42 floats, fitting in hot[94..136] after the plane frame.
pub const MAX_CURVE_PROFILE_VERTS: usize = 14;

/// Maximum spine control points for `SdfShape::SpinePipe` (CPU + GPU bake must match).
/// GPU volume bake clamps segment walks to this count; insert/serialize enforce the same.
pub const MAX_SPINE_POINTS: usize = 64;

/// A single vertex of a 2D curve profile.
///
/// **Corner treatment is packed into the SIGN of `bevel_radius`** (bincode-inert — no new field, so
/// every existing `.fl4` stays byte-identical and loads correctly):
///   * `bevel_radius > 0` → ROUND (fillet) of radius `bevel_radius`
///   * `bevel_radius < 0` → CHAMFER (straight cut) of setback `|bevel_radius|`
///   * `bevel_radius == 0` → SHARP
/// Old files only ever stored `bevel_radius >= 0`, so they read back as ROUND/SHARP exactly as
/// before — the negative half of the range is new and unambiguous.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CurveVertex {
    pub pos: Vec2,          // 2D position in profile plane local coords
    pub bevel_radius: f32,  // corner size; SIGN = mode (see struct docs): + round, − chamfer, 0 sharp
}

impl Default for CurveVertex {
    fn default() -> Self { Self { pos: Vec2::ZERO, bevel_radius: 0.0 } }
}

impl CurveVertex {
    /// Corner size (setback / fillet radius), always non-negative.
    #[inline] pub fn corner_radius(&self) -> f32 { self.bevel_radius.abs() }
    #[inline] pub fn is_round(&self) -> bool { self.bevel_radius > 1e-6 }
    #[inline] pub fn is_chamfer(&self) -> bool { self.bevel_radius < -1e-6 }
    #[inline] pub fn is_sharp(&self) -> bool { self.bevel_radius.abs() <= 1e-6 }
}

/// Fillet-arc tessellation. The number of straight segments per ROUND corner scales with the
/// corner's SWEEP ANGLE (≈ one segment per `ARC_SEG_ANGLE`), so a shallow 20° corner and a wide
/// 180° corner both stay visually smooth without over-tessellating the shallow one — the production
/// approach (uniform angular error / constant chord quality). Result is clamped to
/// [`MIN_CORNER_ARC_SEGMENTS`, `MAX_CORNER_ARC_SEGMENTS`]. `boundary_polygon` is the SINGLE source of
/// truth consumed by BOTH the CPU eval and the GPU clip-curve buffer (dynamic length), so raising
/// the resolution here stays automatically parity-safe and never overflows a fixed allocation.
pub const MIN_CORNER_ARC_SEGMENTS: usize = 3;
pub const MAX_CORNER_ARC_SEGMENTS: usize = 24;
/// Target angular resolution of a fillet arc: roughly one straight segment per this many radians
/// (~10°). Smaller ⇒ smoother arcs but more boundary points (more per-sample winding cost at bake).
const ARC_SEG_ANGLE: f32 = 0.18;

/// Number of straight segments to tessellate a fillet arc sweeping `sweep` radians, at uniform
/// angular resolution. Shared by CPU eval and GPU serialization via `boundary_polygon`.
#[inline]
pub fn corner_arc_segments(sweep: f32) -> usize {
    ((sweep.abs() / ARC_SEG_ANGLE).ceil() as usize)
        .clamp(MIN_CORNER_ARC_SEGMENTS, MAX_CORNER_ARC_SEGMENTS)
}

/// The 3D plane on which a curve profile is defined.
/// Captures the workplane state at creation time. Extrusion is along `normal`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurvePlane {
    pub origin: Vec3,     // plane origin (prim-local space for clip, world for shape)
    pub normal: Vec3,     // extrusion direction / plane normal
    pub tangent_u: Vec3,  // local X axis of the 2D profile frame
    pub tangent_v: Vec3,  // local Y axis of the 2D profile frame
}

impl Default for CurvePlane {
    fn default() -> Self {
        Self { origin: Vec3::ZERO, normal: Vec3::Y, tangent_u: Vec3::X, tangent_v: Vec3::Z }
    }
}

/// A closed 2D curve profile with a 3D plane frame.
/// Shared by: SdfShape::CurveExtrude, SdfPrimitive::clip_curve, future trim/sweep.
/// Changing this type's evaluation or serialization updates ALL tools that use curves.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CurveProfile {
    pub verts: Vec<CurveVertex>,
    pub plane: CurvePlane,
}

impl CurveProfile {
    pub fn is_valid(&self) -> bool { self.verts.len() >= 3 }

    /// Signed distance to the profile polygon at a 2D point in the profile's UV frame.
    /// Positive outside, negative inside. Uses winding-number-based sign.
    pub fn sdf_2d(&self, p: Vec2) -> f32 {
        sdf_polygon_2d(p, &self.verts)
    }

    /// 3D signed distance: project `local_p` onto the profile plane, evaluate 2D polygon SDF,
    /// optionally cap extrusion depth along the normal.
    pub fn sdf_3d(&self, local_p: Vec3, half_depth: f32) -> f32 {
        if !self.is_valid() { return f32::MAX; }
        let rel = local_p - self.plane.origin;
        let u = rel.dot(self.plane.tangent_u);
        let v = rel.dot(self.plane.tangent_v);
        let d2d = self.sdf_2d(Vec2::new(u, v));
        if half_depth > 0.001 {
            let axis_dist = rel.dot(self.plane.normal).abs();
            d2d.max(axis_dist - half_depth)
        } else {
            d2d // infinite extrusion
        }
    }
}

/// Expand a closed profile's per-vertex corner treatment (round / chamfer, encoded in the sign of
/// `bevel_radius`) into an explicit boundary polygon of 2D points:
///   * SHARP corner   → the vertex itself (1 point)
///   * CHAMFER corner → two tangent points on the adjacent edges (an EXACT straight cut)
///   * ROUND corner   → a tessellated fillet arc tangent to both edges (`corner_arc_segments`+1 pts,
///                      count scales with the corner sweep angle)
///
/// Feeding the result to the plain winding-number polygon SDF (`sdf_polygon_2d_pts`) is exact for
/// sharp+chamfer and a smooth tessellation for round. This is the SINGLE source of truth shared by
/// the CPU eval (`SdfShape::CurveExtrude`) and the GPU bake (`serialize_clip_curves_recursive`
/// serialises these very points into the clip-curve buffer, so the WGSL winding SDF needs NO new
/// math — it just consumes more vertices → automatic CPU/GPU parity).
///
/// **Bounding contract:** every emitted point lies on the original edges or inside the corner
/// triangle, so the profile never extends past its authored vertices — the existing
/// `bounding_radius`/AABB (max authored vertex length) still covers it, no outward growth.
pub fn boundary_polygon(verts: &[CurveVertex]) -> Vec<Vec2> {
    let n = verts.len();
    if n < 3 || verts.iter().all(|v| v.is_sharp()) {
        return verts.iter().map(|v| v.pos).collect();
    }
    let mut out: Vec<Vec2> = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v = &verts[i];
        let b = v.pos;
        if v.is_sharp() { out.push(b); continue; }
        let a = verts[(i + n - 1) % n].pos;
        let c = verts[(i + 1) % n].pos;
        let ein = a - b; let eout = c - b;
        let (len_in, len_out) = (ein.length(), eout.length());
        if len_in < 1e-6 || len_out < 1e-6 { out.push(b); continue; }
        let ua = ein / len_in; let uc = eout / len_out;
        // Setback along each edge, clamped to half the shorter neighbour so adjacent beveled corners
        // can never overlap (keeps the expanded polygon simple).
        let s = v.corner_radius().min(len_in * 0.5).min(len_out * 0.5);
        if s < 1e-6 { out.push(b); continue; }
        let p1 = b + ua * s; // toward prev
        let p2 = b + uc * s; // toward next
        if v.is_chamfer() { out.push(p1); out.push(p2); continue; }
        // ROUND: fillet arc tangent to both edges at p1/p2, centre on the interior bisector.
        let half = ua.dot(uc).clamp(-1.0, 1.0).acos() * 0.5; // half the edge-edge angle
        let bis = (ua + uc).normalize_or_zero();
        if half.sin() < 1e-4 || bis == Vec2::ZERO { out.push(p1); out.push(p2); continue; }
        let center = b + bis * (s / half.cos().max(1e-4));
        let rho = (p1 - center).length();
        let ang1 = (p1 - center).y.atan2((p1 - center).x);
        let ang2 = (p2 - center).y.atan2((p2 - center).x);
        // Sweep the SHORT arc from p1 to p2 (the side toward the original corner).
        let mut delta = ang2 - ang1;
        while delta >  std::f32::consts::PI { delta -= std::f32::consts::TAU; }
        while delta < -std::f32::consts::PI { delta += std::f32::consts::TAU; }
        let segs = corner_arc_segments(delta);
        for k in 0..=segs {
            let t = k as f32 / segs as f32;
            let ang = ang1 + delta * t;
            out.push(center + Vec2::new(ang.cos(), ang.sin()) * rho);
        }
    }
    out
}

/// Signed distance to a closed 2D polygon given as an explicit point list (positive outside,
/// negative inside, winding-number sign). The boundary primitive for all corner treatments — see
/// `boundary_polygon`.
pub fn sdf_polygon_2d_pts(p: Vec2, pts: &[Vec2]) -> f32 {
    let n = pts.len();
    if n < 3 { return f32::MAX; }
    let mut d_sq = f32::MAX;
    let mut winding: i32 = 0;
    for i in 0..n {
        let vi = pts[i];
        let vj = pts[(i + 1) % n];
        let e = vj - vi;
        let w = p - vi;
        let t = (w.dot(e) / e.dot(e).max(1e-12)).clamp(0.0, 1.0);
        let closest = vi + e * t;
        d_sq = d_sq.min((p - closest).length_squared());
        if vi.y <= p.y {
            if vj.y > p.y && e.x * w.y - e.y * w.x > 0.0 { winding += 1; }
        } else {
            if vj.y <= p.y && e.x * w.y - e.y * w.x < 0.0 { winding -= 1; }
        }
    }
    d_sq.sqrt() * if winding != 0 { -1.0 } else { 1.0 }
}

/// Unsigned distance from point `p` to segment `a→b` in 3D.
#[inline]
pub fn dist_point_segment_3d(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let ab2 = ab.dot(ab).max(1e-20);
    let t = ((p - a).dot(ab) / ab2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Unsigned distance from `p` to a 3D polyline (segments `points[i]→points[i+1]`).
/// When `closed` and `points.len() >= 3`, also includes the wrap segment last→first.
pub fn dist_to_polyline_3d(p: Vec3, points: &[Vec3], closed: bool) -> f32 {
    let n = points.len();
    if n < 2 {
        return if n == 1 { (p - points[0]).length() } else { f32::MAX };
    }
    let mut d = f32::MAX;
    for i in 0..n - 1 {
        d = d.min(dist_point_segment_3d(p, points[i], points[i + 1]));
    }
    if closed && n >= 3 {
        d = d.min(dist_point_segment_3d(p, points[n - 1], points[0]));
    }
    d
}

/// Signed distance to a spine tube with **per-point radius** =
/// `base_radius * point_scale[i]` (missing scale → 1.0). Radius is lerped along each segment.
///
/// Call sites that store join metadata should pass pre-expanded polylines from
/// [`expand_spine_polyline`] so Sharp/Round/Chamfer affect the centerline.
pub fn dist_to_spine_pipe_3d(
    p: Vec3,
    points: &[Vec3],
    base_radius: f32,
    point_scale: &[f32],
    closed: bool,
) -> f32 {
    let n = points.len();
    let r_at = |i: usize| -> f32 {
        let s = point_scale.get(i).copied().unwrap_or(1.0).max(0.001);
        (base_radius * s).max(0.001)
    };
    if n == 0 {
        return f32::MAX;
    }
    if n == 1 {
        return (p - points[0]).length() - r_at(0);
    }
    let mut best = f32::MAX;
    let segs = if closed && n >= 3 { n } else { n - 1 };
    for i in 0..segs {
        let j = (i + 1) % n;
        let a = points[i];
        let b = points[j];
        let ab = b - a;
        let ab2 = ab.dot(ab).max(1e-20);
        let t = ((p - a).dot(ab) / ab2).clamp(0.0, 1.0);
        let closest = a + ab * t;
        let r = r_at(i) * (1.0 - t) + r_at(j) * t;
        best = best.min((p - closest).length() - r);
    }
    best
}

/// Cubic Bezier evaluation: (1−t)³P0 + 3(1−t)²t P1 + 3(1−t)t² P2 + t³ P3.
#[inline]
pub fn cubic_bezier_3d(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    let u = 1.0 - t;
    let uu = u * u;
    let tt = t * t;
    p0 * (uu * u) + p1 * (3.0 * uu * t) + p2 * (3.0 * u * tt) + p3 * (tt * t)
}

/// Auto-aligned Bezier handles at control point `i` (Blender-style 1/3 edges).
/// Returns `(handle_in, handle_out)` as offsets from `points[i]`.
pub fn spine_auto_handles(points: &[Vec3], i: usize, closed: bool) -> (Vec3, Vec3) {
    let n = points.len();
    if n < 2 || i >= n {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    let p = points[i];
    let prev = if i == 0 {
        if closed && n >= 3 { points[n - 1] } else { p }
    } else {
        points[i - 1]
    };
    let next = if i + 1 >= n {
        if closed && n >= 3 { points[0] } else { p }
    } else {
        points[i + 1]
    };
    let len_in = (p - prev).length() / 3.0;
    let len_out = (next - p).length() / 3.0;
    let dir = (next - prev).normalize_or_zero();
    if dir.length_squared() < 0.25 {
        let d_in = (prev - p).normalize_or_zero() * len_in;
        let d_out = (next - p).normalize_or_zero() * len_out;
        return (d_in, d_out);
    }
    (-dir * len_in, dir * len_out)
}

/// Resolve outgoing handle offset at point `i`.
/// Explicit non-zero storage wins; else auto when join is Bezier (`>0`); else zero (linear).
pub fn spine_resolve_handle_out(
    points: &[Vec3],
    joins: &[f32],
    handles_out: &[Vec3],
    i: usize,
    closed: bool,
) -> Vec3 {
    if let Some(h) = handles_out.get(i) {
        if h.length_squared() > 1e-12 {
            return *h;
        }
    }
    if joins.get(i).copied().unwrap_or(0.0) > 1e-6 {
        return spine_auto_handles(points, i, closed).1;
    }
    Vec3::ZERO
}

/// Resolve incoming handle offset at point `i`.
pub fn spine_resolve_handle_in(
    points: &[Vec3],
    joins: &[f32],
    handles_in: &[Vec3],
    i: usize,
    closed: bool,
) -> Vec3 {
    if let Some(h) = handles_in.get(i) {
        if h.length_squared() > 1e-12 {
            return *h;
        }
    }
    if joins.get(i).copied().unwrap_or(0.0) > 1e-6 {
        return spine_auto_handles(points, i, closed).0;
    }
    Vec3::ZERO
}

/// Expand spine control points by join type + Bezier handles into a denser centerline.
///
/// Join packing (`SpinePipe.joins` / `CurveVertex::bevel_radius`):
/// - `0` → Sharp (linear segments)
/// - `>0` → **Bezier** (cubic segments using `handles_in` / `handles_out`; auto if zero)
/// - `<0` → Chamfer (straight cut of setback `|j|`)
///
/// Open paths never chamfer the first/last vertex. Output ≤ [`MAX_SPINE_POINTS`].
pub fn expand_spine_polyline(
    points: &[Vec3],
    joins: &[f32],
    point_scale: &[f32],
    closed: bool,
    handles_in: &[Vec3],
    handles_out: &[Vec3],
) -> (Vec<Vec3>, Vec<f32>) {
    let n = points.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let scale_at = |i: usize| point_scale.get(i).copied().unwrap_or(1.0).max(0.001);
    let join_at = |i: usize| joins.get(i).copied().unwrap_or(0.0);

    let any_handle = handles_in.iter().any(|h| h.length_squared() > 1e-12)
        || handles_out.iter().any(|h| h.length_squared() > 1e-12);
    let any_bezier = (0..n).any(|i| join_at(i) > 1e-6) || any_handle;
    let any_chamfer = (0..n).any(|i| {
        let endpoint = !closed && (i == 0 || i + 1 == n);
        !endpoint && join_at(i) < -1e-6
    });

    if !any_bezier && !any_chamfer {
        let sc: Vec<f32> = (0..n).map(scale_at).collect();
        return (points.to_vec(), sc);
    }

    // ── Chamfer-only (no Bezier): cut corners like 2D boundary_polygon ─────
    if any_chamfer && !any_bezier {
        return expand_spine_chamfer_only(points, joins, point_scale, closed);
    }

    // ── Bezier path (cubic segments). Chamfer verts use zero handles. ─────
    expand_spine_bezier(points, joins, point_scale, closed, handles_in, handles_out)
}

fn expand_spine_chamfer_only(
    points: &[Vec3],
    joins: &[f32],
    point_scale: &[f32],
    closed: bool,
) -> (Vec<Vec3>, Vec<f32>) {
    let n = points.len();
    let scale_at = |i: usize| point_scale.get(i).copied().unwrap_or(1.0).max(0.001);
    let join_at = |i: usize| joins.get(i).copied().unwrap_or(0.0);
    let mut out_pts: Vec<Vec3> = Vec::with_capacity(n * 2);
    let mut out_sc: Vec<f32> = Vec::with_capacity(n * 2);
    let push = |pts: &mut Vec<Vec3>, scs: &mut Vec<f32>, p: Vec3, s: f32| -> bool {
        if pts.len() >= MAX_SPINE_POINTS {
            return false;
        }
        pts.push(p);
        scs.push(s);
        true
    };
    for i in 0..n {
        let b = points[i];
        let sc = scale_at(i);
        let endpoint = !closed && (i == 0 || i + 1 == n);
        let jv = join_at(i);
        if endpoint || jv >= -1e-6 {
            if !push(&mut out_pts, &mut out_sc, b, sc) {
                break;
            }
            continue;
        }
        let ip = if i == 0 { n - 1 } else { i - 1 };
        let inext = (i + 1) % n;
        let a = points[ip];
        let c = points[inext];
        let ein = a - b;
        let eout = c - b;
        let (len_in, len_out) = (ein.length(), eout.length());
        if len_in < 1e-6 || len_out < 1e-6 {
            if !push(&mut out_pts, &mut out_sc, b, sc) {
                break;
            }
            continue;
        }
        let s = jv.abs().min(len_in * 0.5).min(len_out * 0.5);
        if s < 1e-6 {
            if !push(&mut out_pts, &mut out_sc, b, sc) {
                break;
            }
            continue;
        }
        let p1 = b + (ein / len_in) * s;
        let p2 = b + (eout / len_out) * s;
        if !push(&mut out_pts, &mut out_sc, p1, sc) {
            break;
        }
        if !push(&mut out_pts, &mut out_sc, p2, sc) {
            break;
        }
    }
    if out_pts.len() < 2 {
        return (points.to_vec(), (0..n).map(scale_at).collect());
    }
    (out_pts, out_sc)
}

fn expand_spine_bezier(
    points: &[Vec3],
    joins: &[f32],
    point_scale: &[f32],
    closed: bool,
    handles_in: &[Vec3],
    handles_out: &[Vec3],
) -> (Vec<Vec3>, Vec<f32>) {
    let n = points.len();
    let scale_at = |i: usize| point_scale.get(i).copied().unwrap_or(1.0).max(0.001);
    if n < 2 {
        return (points.to_vec(), (0..n).map(scale_at).collect());
    }

    let segs = if closed && n >= 3 { n } else { n - 1 };
    // Budget samples evenly across curved segments.
    let mut curved = 0usize;
    for s in 0..segs {
        let i = s;
        let j = (s + 1) % n;
        let ho = spine_resolve_handle_out(points, joins, handles_out, i, closed);
        let hi = spine_resolve_handle_in(points, joins, handles_in, j, closed);
        if ho.length_squared() > 1e-12 || hi.length_squared() > 1e-12 {
            curved += 1;
        }
    }
    // Samples per curved segment (including both ends counted once overall).
    let room = MAX_SPINE_POINTS.saturating_sub(n.max(2));
    let per_curve = if curved == 0 {
        0
    } else {
        ((room / curved) + 1).clamp(MIN_CORNER_ARC_SEGMENTS, MAX_CORNER_ARC_SEGMENTS)
    };

    let mut out_pts: Vec<Vec3> = Vec::with_capacity(MAX_SPINE_POINTS);
    let mut out_sc: Vec<f32> = Vec::with_capacity(MAX_SPINE_POINTS);

    // Always emit first anchor.
    out_pts.push(points[0]);
    out_sc.push(scale_at(0));

    for s in 0..segs {
        if out_pts.len() >= MAX_SPINE_POINTS {
            break;
        }
        let i = s;
        let j = (s + 1) % n;
        let p0 = points[i];
        let p3 = points[j];
        let ho = spine_resolve_handle_out(points, joins, handles_out, i, closed);
        let hi = spine_resolve_handle_in(points, joins, handles_in, j, closed);
        let sc0 = scale_at(i);
        let sc1 = scale_at(j);

        if ho.length_squared() <= 1e-12 && hi.length_squared() <= 1e-12 {
            // Linear segment — only emit end (start already present).
            if out_pts.len() < MAX_SPINE_POINTS {
                out_pts.push(p3);
                out_sc.push(sc1);
            }
            continue;
        }

        let p1 = p0 + ho;
        let p2 = p3 + hi;
        let steps = per_curve.max(2);
        // t=0 is previous end; emit t=1/steps .. t=1
        for k in 1..=steps {
            if out_pts.len() >= MAX_SPINE_POINTS {
                break;
            }
            let t = k as f32 / steps as f32;
            let pt = cubic_bezier_3d(p0, p1, p2, p3, t);
            let sc = sc0 * (1.0 - t) + sc1 * t;
            out_pts.push(pt);
            out_sc.push(sc);
        }
    }

    if out_pts.len() < 2 {
        return (points.to_vec(), (0..n).map(scale_at).collect());
    }
    (out_pts, out_sc)
}

/// Spine tube distance with join + handle expansion (Phase B/E).
pub fn dist_to_spine_pipe_3d_joined(
    p: Vec3,
    points: &[Vec3],
    base_radius: f32,
    point_scale: &[f32],
    closed: bool,
    joins: &[f32],
    handles_in: &[Vec3],
    handles_out: &[Vec3],
) -> f32 {
    let any_join = joins.iter().any(|j| j.abs() > 1e-6);
    let any_handle = handles_in.iter().any(|h| h.length_squared() > 1e-12)
        || handles_out.iter().any(|h| h.length_squared() > 1e-12);
    if (!any_join && !any_handle) || points.len() < 2 {
        return dist_to_spine_pipe_3d(p, points, base_radius, point_scale, closed);
    }
    let (pts, sc) = expand_spine_polyline(
        points, joins, point_scale, closed, handles_in, handles_out,
    );
    dist_to_spine_pipe_3d(p, &pts, base_radius, &sc, closed)
}

/// Unsigned distance from `p` to an OPEN polyline (segments `verts[i]→verts[i+1]`, NO wrap-around
/// closing segment). The P2 primitive for open-profile CurveExtrude: offsetting this by −thickness/2
/// gives a flat ribbon (Minkowski sum with a disk → round convex corners + round end-caps for free);
/// combining with the out-of-plane axis gives a round tube. Corner `bevel_radius` is ignored in open
/// mode (the offset already rounds convex corners). Matches `dist_to_open_polyline_gpu` in
/// `volume_bake.wgsl` for CPU/GPU parity.
pub fn dist_to_open_polyline_2d(p: Vec2, verts: &[CurveVertex]) -> f32 {
    let n = verts.len();
    if n < 2 { return f32::MAX; }
    let mut d = f32::MAX;
    for i in 0..n - 1 {
        let a = verts[i].pos;
        let b = verts[i + 1].pos;
        let e = b - a;
        let w = p - a;
        let t = (w.dot(e) / e.dot(e).max(1e-12)).clamp(0.0, 1.0);
        let closest = a + e * t;
        d = d.min((p - closest).length());
    }
    d
}

/// Signed distance to a closed 2D polygon with per-vertex corner treatment (sharp/round/chamfer,
/// packed in the sign of `bevel_radius`). Positive outside, negative inside.
pub fn sdf_polygon_2d(p: Vec2, verts: &[CurveVertex]) -> f32 {
    let n = verts.len();
    if n < 3 { return f32::MAX; }
    // Beveled path: expand corners then evaluate the explicit boundary polygon.
    if verts.iter().any(|v| !v.is_sharp()) {
        return sdf_polygon_2d_pts(p, &boundary_polygon(verts));
    }
    // Sharp fast path (no allocation).
    let mut d_sq = f32::MAX;
    let mut winding: i32 = 0;
    for i in 0..n {
        let vi = verts[i].pos;
        let vj = verts[(i + 1) % n].pos;
        let e = vj - vi;
        let w = p - vi;
        let t = (w.dot(e) / e.dot(e).max(1e-12)).clamp(0.0, 1.0);
        let closest = vi + e * t;
        d_sq = d_sq.min((p - closest).length_squared());
        if vi.y <= p.y {
            if vj.y > p.y && e.x * w.y - e.y * w.x > 0.0 { winding += 1; }
        } else {
            if vj.y <= p.y && e.x * w.y - e.y * w.x < 0.0 { winding -= 1; }
        }
    }
    d_sq.sqrt() * if winding != 0 { -1.0 } else { 1.0 }
}

/// 2D corner bevel between two half-plane distances `a` and `b` (both positive outside), matching
/// the WGSL `bevel_2d_profiled` in `volume_bake.wgsl` bit-for-bit. `r`=corner radius, `profile` in
/// −1..+1 (0 = straight chamfer, +1 = round fillet, −1 = concave scoop), `chamfer_angle_deg` biases
/// the straight cut. Reused for the CurveExtrude RIM (profile↔depth cap join). `r <= 0` ⇒ hard `max`.
pub fn bevel_2d_profiled(a: f32, b: f32, r: f32, profile: f32, chamfer_angle_deg: f32) -> f32 {
    if r <= 0.0 { return a.max(b); }
    let theta = chamfer_angle_deg.to_radians();
    let (st, ct) = (theta.sin(), theta.cos());
    let d_chamfer = a.max(b).max(st * (a + r) + ct * b);
    if profile >= 0.0 {
        let q = Vec2::new((a + r).max(0.0), (b + r).max(0.0));
        let d_round = q.length() + (a + r).max(b + r).min(0.0) - r;
        d_chamfer + (d_round - d_chamfer) * profile
    } else {
        let u = Vec2::new((r + a).max(0.0), (r + b).max(0.0));
        let d_concave = a.max(b) - (r - u.length());
        d_chamfer + (d_concave - d_chamfer) * (-profile)
    }
}

/// A single clip curve operation: a CurveProfile used as a boolean cutter on a prim.
/// Multiple ClipCurves can be stacked on one prim (each evaluated sequentially).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipCurve {
    pub profile: CurveProfile,
    pub mode: u32,       // 0=Subtract, 1=KeepInside
    pub depth: f32,      // half-depth along extrusion axis (0.0 = infinite)
    pub enabled: bool,
}

impl Default for ClipCurve {
    fn default() -> Self {
        Self { profile: CurveProfile::default(), mode: 0, depth: 0.0, enabled: true }
    }
}

impl ClipCurve {
    pub fn is_valid(&self) -> bool { self.enabled && self.profile.is_valid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::SdfShape;
    use glam::Vec3;

    // Unit square profile centred at origin, side 2 (verts at ±1).
    fn square(radius: f32) -> Vec<CurveVertex> {
        [Vec2::new(-1.0, -1.0), Vec2::new(1.0, -1.0), Vec2::new(1.0, 1.0), Vec2::new(-1.0, 1.0)]
            .iter().map(|&pos| CurveVertex { pos, bevel_radius: radius }).collect()
    }

    #[test]
    fn sharp_square_sdf_basic() {
        let v = square(0.0);
        assert!(sdf_polygon_2d(Vec2::ZERO, &v) < 0.0, "centre is inside");
        assert!((sdf_polygon_2d(Vec2::new(2.0, 0.0), &v) - 1.0).abs() < 1e-4, "1 unit right of edge");
        // Sharp corner: distance to (1,1) corner from (2,2) is sqrt(2).
        assert!((sdf_polygon_2d(Vec2::new(2.0, 2.0), &v) - 2f32.sqrt()).abs() < 1e-4);
    }

    #[test]
    fn chamfer_corner_cuts_the_tip() {
        // Chamfer of setback 0.5 → the corner (1,1) is cut back by a straight bevel line.
        let sharp = square(0.0);
        let cham = square(-0.5); // negative = chamfer
        // The exact former corner point (1,1) must now be OUTSIDE (positive), since the tip was cut.
        assert!(sdf_polygon_2d(Vec2::new(1.0, 1.0), &sharp) <= 1e-4, "sharp: corner on boundary");
        assert!(sdf_polygon_2d(Vec2::new(1.0, 1.0), &cham) > 1e-3, "chamfer removes the corner tip");
        // The chamfer boundary passes through the two tangent points (1,0.5) and (0.5,1) → ~on surface.
        assert!(sdf_polygon_2d(Vec2::new(1.0, 0.5), &cham).abs() < 1e-3, "tangent point on chamfer edge");
        // Boundary expansion for one chamfered corner adds exactly one extra vertex (1 → 2).
        assert_eq!(boundary_polygon(&cham).len(), 8, "4 chamfered corners → 8 boundary points");
    }

    #[test]
    fn round_corner_rounds_the_tip() {
        let round = square(0.5); // positive = round
        // Former corner (1,1) is cut inward (positive) but by LESS than the chamfer would.
        let d_corner = sdf_polygon_2d(Vec2::new(1.0, 1.0), &round);
        assert!(d_corner > 1e-3, "round removes the sharp corner tip");
        // A point along the diagonal just outside the fillet is positive; deep centre negative.
        assert!(sdf_polygon_2d(Vec2::ZERO, &round) < 0.0);
        // Tangent point (1,0.5) sits on the fillet boundary.
        assert!(sdf_polygon_2d(Vec2::new(1.0, 0.5), &round).abs() < 1e-3);
        // Arc tessellation: each rounded corner emits corner_arc_segments(sweep)+1 points. The
        // square's 90° corners each sweep π/2; the adaptive count keeps them smooth.
        let segs = corner_arc_segments(std::f32::consts::FRAC_PI_2);
        assert_eq!(boundary_polygon(&round).len(), 4 * (segs + 1));
        assert!(segs >= 6, "a 90° fillet should tessellate to a smooth arc, got {segs} segments");
    }

    #[test]
    fn corner_bevel_never_grows_outward() {
        // Bounding probe: no corner treatment may push the surface beyond the authored max radius.
        // Original max vertex length = sqrt(2). Sample a ring beyond it; must stay OUTSIDE (>0).
        for verts in &[square(0.0), square(0.5), square(-0.5)] {
            let r_bound = 2f32.sqrt() + 1e-3;
            for k in 0..16 {
                let a = k as f32 / 16.0 * std::f32::consts::TAU;
                let p = Vec2::new(a.cos(), a.sin()) * (r_bound + 0.5);
                assert!(sdf_polygon_2d(p, &verts) > 0.0, "point beyond bound must be exterior");
            }
        }
    }

    #[test]
    fn shell_hollows_and_preserves_outer_surface() {
        // Solid extruded square vs shelled: the shell must keep the OUTER surface (d≈0 at the face)
        // and make the deep interior POSITIVE (hollow).
        let profile = CurveProfile { verts: square(0.0).into_iter().collect(), plane: CurvePlane::default() };
        let solid = SdfShape::CurveExtrude { profile: profile.clone(), half_depth: 1.0, wall_t: 0.0, rim: 0.0, open: false, thickness: 0.0, tube: false };
        let shell = SdfShape::CurveExtrude { profile, half_depth: 1.0, wall_t: 0.3, rim: 0.0, open: false, thickness: 0.0, tube: false };
        // Deep interior (origin): solid strongly negative; shell hollow → positive.
        assert!(solid.distance(Vec3::ZERO) < -0.5);
        assert!(shell.distance(Vec3::ZERO) > 0.0, "shell hollows the core");
        // Outer face point (1,0,0): both ~on the surface (shell preserves outer surface, no growth).
        assert!(solid.distance(Vec3::new(1.0, 0.0, 0.0)).abs() < 1e-3);
        assert!(shell.distance(Vec3::new(1.0, 0.0, 0.0)).abs() < 1e-3, "shell keeps outer surface");
        // Inside the wall (0.85 from centre, wall 0.3 thick) → negative (solid wall material).
        assert!(shell.distance(Vec3::new(0.85, 0.0, 0.0)) < 0.0);
    }

    #[test]
    fn rim_rounds_the_cap_edge() {
        let profile = CurveProfile { verts: square(0.0).into_iter().collect(), plane: CurvePlane::default() };
        let hard = SdfShape::CurveExtrude { profile: profile.clone(), half_depth: 1.0, wall_t: 0.0, rim: 0.0, open: false, thickness: 0.0, tube: false };
        let round = SdfShape::CurveExtrude { profile, half_depth: 1.0, wall_t: 0.0, rim: 0.4, open: false, thickness: 0.0, tube: false };
        // The top rim edge point (1,1,0) [face corner where profile edge meets the cap]:
        // hard cap keeps it ON the surface (max(0,0)=0); rounded rim cuts it inward → positive.
        let p = Vec3::new(1.0, 1.0, 0.0);
        assert!(hard.distance(p).abs() < 1e-3, "hard cap: rim corner on surface");
        assert!(round.distance(p) > 1e-3, "round rim cuts the cap edge inward");
        // Rim must not grow outward: a point 0.5 beyond the top corner stays exterior for both.
        assert!(round.distance(Vec3::new(1.0, 1.5, 0.0)) > 0.0);
    }

    // An open 3-point polyline along X at y=0 (in the profile's XZ plane): (-1,0)→(0,0)→(1,0).
    fn open_line() -> CurveProfile {
        CurveProfile {
            verts: vec![
                CurveVertex { pos: Vec2::new(-1.0, 0.0), bevel_radius: 0.0 },
                CurveVertex { pos: Vec2::new( 0.0, 0.0), bevel_radius: 0.0 },
                CurveVertex { pos: Vec2::new( 1.0, 0.0), bevel_radius: 0.0 },
            ],
            plane: CurvePlane::default(),
        }
    }

    #[test]
    fn polyline_3d_segment_and_tube() {
        // Straight segment along X: (-1,0,0)→(1,0,0). Midpoint distance 0; offset Y = |y|.
        let pts = [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)];
        assert!(dist_to_polyline_3d(Vec3::ZERO, &pts, false).abs() < 1e-5);
        assert!((dist_to_polyline_3d(Vec3::new(0.0, 0.3, 0.0), &pts, false) - 0.3).abs() < 1e-5);
        // End cap hemisphere: past the tip by 0.2 along X.
        assert!((dist_to_polyline_3d(Vec3::new(1.2, 0.0, 0.0), &pts, false) - 0.2).abs() < 1e-5);
        // SpinePipe: radius 0.1 → interior on axis, surface at 0.1 offset.
        let pipe = SdfShape::SpinePipe {
            points: pts.to_vec(),
            radius: 0.1,
            closed: false,
            joins: vec![],
            point_scale: vec![],
            handles_in: vec![],
            handles_out: vec![],
        };
        assert!(pipe.distance(Vec3::ZERO) < 0.0);
        assert!(pipe.distance(Vec3::new(0.0, 0.1, 0.0)).abs() < 1e-4);
        assert!(pipe.distance(Vec3::new(0.0, 0.25, 0.0)) > 0.0);
        // Closed triangle: wrap segment contributes.
        let tri = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.5, 0.0, 1.0),
        ];
        let d_open = dist_to_polyline_3d(Vec3::new(0.25, 0.0, 0.5), &tri, false);
        let d_closed = dist_to_polyline_3d(Vec3::new(0.25, 0.0, 0.5), &tri, true);
        assert!(d_closed < d_open + 1e-4, "closed wrap must not be longer than open");
    }

    /// Regression: CPU + GPU share one budget; insert/serialize must not exceed this.
    #[test]
    fn spine_max_points_budget_matches_gpu_clamp() {
        assert_eq!(MAX_SPINE_POINTS, 64, "volume_bake.wgsl st==12 clamps pc to 64u");
        // Distance still well-defined at the budget edge (straight polyline of MAX pts).
        let pts: Vec<Vec3> = (0..MAX_SPINE_POINTS)
            .map(|i| Vec3::new(i as f32 * 0.1, 0.0, 0.0))
            .collect();
        let d = dist_to_polyline_3d(Vec3::new(3.15, 0.0, 0.0), &pts, false);
        assert!(d < 0.01, "mid-span sample on long spine must hit a segment");
        let pipe = SdfShape::SpinePipe {
            points: pts,
            radius: 0.05,
            closed: false,
            joins: vec![],
            point_scale: vec![],
            handles_in: vec![],
            handles_out: vec![],
        };
        assert!(pipe.distance(Vec3::new(3.15, 0.0, 0.0)) < 0.0);
    }

    /// Phase B: chamfer cuts the corner; Phase E: Bezier samples a smooth curve past the tip.
    #[test]
    fn spine_join_chamfer_and_bezier_shorten_corner() {
        // Right angle: (0,0,0)→(1,0,0)→(1,1,0). Corner tip = (1,0,0).
        let pts = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
        ];
        let scales = vec![1.0, 1.0, 1.0];
        let empty_h: Vec<Vec3> = vec![];
        let tip = Vec3::new(1.0, 0.0, 0.0);

        let (sharp_pts, _) =
            expand_spine_polyline(&pts, &[0.0, 0.0, 0.0], &scales, false, &empty_h, &empty_h);
        assert_eq!(sharp_pts.len(), 3, "sharp path unchanged");

        let (cham_pts, _) =
            expand_spine_polyline(&pts, &[0.0, -0.3, 0.0], &scales, false, &empty_h, &empty_h);
        assert!(cham_pts.len() >= 4, "chamfer replaces 1 corner with 2 samples");
        let d_tip = dist_to_polyline_3d(tip, &cham_pts, false);
        assert!(d_tip > 0.05, "chamfer centerline leaves the sharp tip behind, d={d_tip}");

        // Bezier join at middle: auto handles make cubic segments (anchors still on path).
        let (bez_pts, _) =
            expand_spine_polyline(&pts, &[0.0, 0.3, 0.0], &scales, false, &empty_h, &empty_h);
        assert!(bez_pts.len() > 3, "bezier tessellates the path");
        // Some sample should leave the pure polyline edges (off the L-shaped AB∪BC).
        let off = bez_pts.iter().any(|p| {
            let on_ab = p.y.abs() < 1e-3 && p.x >= -1e-3 && p.x <= 1.0 + 1e-3;
            let on_bc = p.x > 1.0 - 1e-3 && p.y >= -1e-3 && p.y <= 1.0 + 1e-3;
            !on_ab && !on_bc
        });
        assert!(off, "bezier samples must leave the sharp polyline edges");

        // Explicit handles: out from p0 and in to p2 make a known curve.
        let ho = vec![Vec3::new(0.4, 0.0, 0.0), Vec3::new(0.0, 0.4, 0.0), Vec3::ZERO];
        let hi = vec![Vec3::ZERO, Vec3::new(0.0, -0.4, 0.0), Vec3::new(0.0, -0.4, 0.0)];
        let (exp, _) = expand_spine_polyline(
            &pts,
            &[0.3, 0.3, 0.3],
            &scales,
            false,
            &hi,
            &ho,
        );
        assert!(exp.len() <= MAX_SPINE_POINTS);
        assert!(exp.len() > 3);

        let r = 0.05;
        let sharp = SdfShape::SpinePipe {
            points: pts.clone(),
            radius: r,
            closed: false,
            joins: vec![0.0, 0.0, 0.0],
            point_scale: scales.clone(),
            handles_in: vec![],
            handles_out: vec![],
        };
        let cham = SdfShape::SpinePipe {
            points: pts.clone(),
            radius: r,
            closed: false,
            joins: vec![0.0, -0.3, 0.0],
            point_scale: scales.clone(),
            handles_in: vec![],
            handles_out: vec![],
        };
        let sample = Vec3::new(1.0 + r * 0.5, 0.0 - r * 0.5, 0.0);
        let d_s = sharp.distance(sample);
        let d_c = cham.distance(sample);
        assert!(
            d_c > d_s + 1e-3,
            "chamfer should pull mass off the corner (d_s={d_s}, d_c={d_c})"
        );
    }

    #[test]
    fn spine_expand_respects_max_budget() {
        // Many Bezier corners must not exceed MAX_SPINE_POINTS.
        let n = 20;
        let pts: Vec<Vec3> = (0..n)
            .map(|i| {
                let a = i as f32 * 0.4;
                Vec3::new(a.cos(), a.sin(), (i as f32) * 0.05)
            })
            .collect();
        let joins = vec![0.15f32; n];
        let scales = vec![1.0f32; n];
        let empty: Vec<Vec3> = vec![];
        let (exp, sc) = expand_spine_polyline(&pts, &joins, &scales, true, &empty, &empty);
        assert!(exp.len() <= MAX_SPINE_POINTS);
        assert_eq!(exp.len(), sc.len());
        assert!(exp.len() >= n, "expansion should not drop below control count");
    }

    #[test]
    fn spine_auto_handles_are_aligned() {
        let pts = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
        ];
        let (hin, hout) = spine_auto_handles(&pts, 1, false);
        // Aligned: out and -in share a direction (or both zero).
        if hin.length_squared() > 1e-8 && hout.length_squared() > 1e-8 {
            let a = hin.normalize();
            let b = hout.normalize();
            assert!(
                a.dot(b) < -0.9,
                "auto handles should be anti-parallel (aligned), hin={hin:?} hout={hout:?}"
            );
        }
    }

    #[test]
    fn open_wall_is_a_thick_ribbon() {
        // Open ribbon, thickness 0.4 (band ±0.2 around the path in XZ), extruded ±half_depth 0.5 in Y.
        let wall = SdfShape::CurveExtrude {
            profile: open_line(), half_depth: 0.5, wall_t: 0.0, rim: 0.0,
            open: true, thickness: 0.4, tube: false,
        };
        // On the path (origin), inside the band and inside the extrude → negative.
        assert!(wall.distance(Vec3::ZERO) < 0.0, "on-path point is inside the wall");
        // In the profile plane, 0.2 off the path (Z) = band edge → ~surface.
        assert!(wall.distance(Vec3::new(0.0, 0.0, 0.2)).abs() < 1e-4, "band half-width = thickness/2");
        // 0.5 off the path (Z) is outside the band → exterior (proves NO winding fill of the interior).
        assert!(wall.distance(Vec3::new(0.0, 0.0, 0.5)) > 0.0, "open profile does not fill");
        // Extrude cap at |y| = half_depth.
        assert!(wall.distance(Vec3::new(0.0, 0.5, 0.0)).abs() < 1e-4, "wall cap at half_depth");
        // Endpoint is rounded: 0.2 straight past the end tip (x=1.2) sits on the round cap.
        assert!(wall.distance(Vec3::new(1.2, 0.0, 0.0)).abs() < 1e-4, "ribbon end is rounded by offset");
    }

    #[test]
    fn open_tube_is_a_round_swept_section() {
        // Round tube of radius 0.25 (thickness 0.5) swept along the path; half_depth ignored.
        let tube = SdfShape::CurveExtrude {
            profile: open_line(), half_depth: 0.5, wall_t: 0.0, rim: 0.0,
            open: true, thickness: 0.5, tube: true,
        };
        // Cross-section is round: 0.25 away from the path in ANY perpendicular direction = surface.
        assert!(tube.distance(Vec3::new(0.0, 0.0, 0.25)).abs() < 1e-4, "tube radius in-plane");
        assert!(tube.distance(Vec3::new(0.0, 0.25, 0.0)).abs() < 1e-4, "tube radius out-of-plane (round!)");
        // Inside the tube (on the path) is negative; well outside is positive.
        assert!(tube.distance(Vec3::ZERO) < 0.0);
        assert!(tube.distance(Vec3::new(0.0, 0.5, 0.0)) > 0.0);
        // Round end-cap: 0.25 straight past the end tip sits on the hemisphere.
        assert!(tube.distance(Vec3::new(1.25, 0.0, 0.0)).abs() < 1e-4, "tube has round end-caps");
    }
}
