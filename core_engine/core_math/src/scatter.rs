//! SCATTER: place instances of one mesh over another mesh's surface.
//!
//! Pure geometry / math (CLAUDE.md: geometry belongs in `core_math`, never the app crate).
//! This module answers ONE question — *where does each instance go?* — and returns a list of
//! placement matrices. It never touches `HalfEdgeMesh` topology and never allocates the
//! instanced geometry; the caller concatenates the copies with the canonical mesh
//! concatenator (`app::poly::boolean::merge_mesh_list`).
//!
//! ## Determinism
//! Every random draw is a PURE FUNCTION of `(seed, site_index, channel)` — there is no RNG
//! state carried between sites. Consequences that are the whole point of doing it this way:
//! * the same seed always gives the same scatter, across runs, machines and save/load;
//! * changing `count` does not reshuffle the sites that were already there — site *i* keeps
//!   its draws, so raising the count *adds* instances instead of scrambling the field.
//!
//! ## Frame budget
//! [`MAX_SCATTER_INSTANCES`] hard-caps the random-points mode. Vertex / face-centre modes are
//! bounded by the target mesh itself and are additionally capped, because a subdivided target
//! can otherwise put a million instances into a synchronous modifier evaluation.

use crate::half_edge::{FaceId, HalfEdgeMesh, VertexId};
use glam::{Mat4, Quat, Vec3};

/// Point source: one instance per live VERTEX of the target mesh.
pub const SCATTER_SRC_VERTICES: u8 = 0;
/// Point source: one instance per live FACE, at its median-weighted centre.
pub const SCATTER_SRC_FACE_CENTERS: u8 = 1;
/// Point source: `count` random points, AREA-WEIGHTED over the faces (a face twice the area
/// receives twice the instances — never a per-face count, which biases toward small faces).
pub const SCATTER_SRC_RANDOM_FACES: u8 = 2;

/// Hard ceiling on instances produced by one Scatter modifier (CLAUDE.md Frame Budget Rule:
/// the modifier stack is evaluated synchronously, so an unbounded count would hang the frame).
pub const MAX_SCATTER_INSTANCES: usize = 4096;

/// A scatter site: a point on the target surface plus the surface normal there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScatterSite {
    pub position: Vec3,
    /// Unit surface normal, or `Vec3::Y` where the mesh is degenerate (never a zero vector —
    /// callers build a rotation frame from this).
    pub normal: Vec3,
}

/// Authored scatter parameters, in the target body's LOCAL mesh space.
///
/// Ranges are `(min, max)`; a `min > max` pair is swapped rather than producing NaN, and a
/// `min == max` pair is exactly "no randomness on this channel".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScatterParams {
    /// One of the `SCATTER_SRC_*` constants. An unknown value falls back to vertices.
    pub source: u8,
    /// Instance count for [`SCATTER_SRC_RANDOM_FACES`] (ignored by the other two sources).
    pub count: u32,
    /// Shared seed for every random channel.
    pub seed: u32,
    /// Rotate each instance so its local **+Y** points along the surface normal.
    /// (+Y is AZRA's up axis — `HalfEdgeMesh::new_cylinder` builds along Y.)
    pub align_to_normal: bool,
    /// Random spin about the alignment axis, in degrees, drawn from `0..spin_deg`.
    /// Applied about the instance's local +Y, i.e. about the surface normal once aligned.
    pub spin_deg: f32,
    /// Random rotation jitter per axis, in degrees, drawn from `-v..v` (local XYZ Euler).
    pub rot_jitter_deg: Vec3,
    /// Random positional offset per axis, in local units, drawn from `-v..v`.
    pub pos_jitter: Vec3,
    /// Uniform scale range, multiplied into every axis.
    pub scale_min: f32,
    pub scale_max: f32,
    /// Per-axis scale range, multiplied on top of the uniform scale.
    pub scale_axis_min: Vec3,
    pub scale_axis_max: Vec3,
}

impl Default for ScatterParams {
    fn default() -> Self {
        Self {
            source: SCATTER_SRC_VERTICES,
            count: 50,
            seed: 0,
            align_to_normal: true,
            spin_deg: 0.0,
            rot_jitter_deg: Vec3::ZERO,
            pos_jitter: Vec3::ZERO,
            scale_min: 1.0,
            scale_max: 1.0,
            scale_axis_min: Vec3::ONE,
            scale_axis_max: Vec3::ONE,
        }
    }
}

// ── deterministic hash RNG ──────────────────────────────────────────────────
// splitmix32. Stateless by design: draw(seed, site, channel) is reproducible forever, which is
// what makes a saved seed mean the same scatter after a reload.

const CH_FACE_PICK: u32 = 0;
const CH_BARY_U: u32 = 1;
const CH_BARY_V: u32 = 2;
const CH_SCALE_U: u32 = 3;
const CH_SCALE_X: u32 = 4;
const CH_SCALE_Y: u32 = 5;
const CH_SCALE_Z: u32 = 6;
const CH_ROT_X: u32 = 7;
const CH_ROT_Y: u32 = 8;
const CH_ROT_Z: u32 = 9;
const CH_POS_X: u32 = 10;
const CH_POS_Y: u32 = 11;
const CH_POS_Z: u32 = 12;
const CH_SPIN: u32 = 13;
const CH_TRI_PICK: u32 = 14;

fn splitmix32(mut x: u32) -> u32 {
    x = x.wrapping_add(0x9E37_79B9);
    let mut z = x;
    z = (z ^ (z >> 16)).wrapping_mul(0x21F0_AAAD);
    z = (z ^ (z >> 15)).wrapping_mul(0x735A_2D97);
    z ^ (z >> 15)
}

/// Uniform in `[0, 1)`, a pure function of `(seed, site, channel)`.
fn u01(seed: u32, site: u32, channel: u32) -> f32 {
    let h = splitmix32(
        seed.wrapping_mul(0x9E37_79B1)
            ^ site.wrapping_mul(0x85EB_CA6B)
            ^ channel.wrapping_mul(0xC2B2_AE35),
    );
    // 24 bits → exact in f32, and never reaches 1.0.
    (h >> 8) as f32 / 16_777_216.0
}

/// Draw in `[lo, hi)`; tolerates a reversed pair and collapses cleanly when `lo == hi`.
fn range(seed: u32, site: u32, channel: u32, lo: f32, hi: f32) -> f32 {
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    if (hi - lo).abs() < f32::EPSILON {
        return lo;
    }
    lo + (hi - lo) * u01(seed, site, channel)
}

// ── sites ───────────────────────────────────────────────────────────────────

/// The scatter sites on `mesh` for one point source.
///
/// Returns an empty vec for an empty / degenerate mesh — the caller's no-op path. Never panics
/// and never returns a zero normal.
pub fn scatter_sites(mesh: &HalfEdgeMesh, source: u8, count: u32, seed: u32) -> Vec<ScatterSite> {
    match source {
        SCATTER_SRC_FACE_CENTERS => face_center_sites(mesh),
        SCATTER_SRC_RANDOM_FACES => random_face_sites(mesh, count, seed),
        _ => vertex_sites(mesh),
    }
}

fn safe_normal(n: Vec3) -> Vec3 {
    let l = n.length();
    if l > 1e-12 && n.is_finite() {
        n / l
    } else {
        Vec3::Y
    }
}

fn vertex_sites(mesh: &HalfEdgeMesh) -> Vec<ScatterSite> {
    let mut out = Vec::new();
    for i in 0..mesh.vertices.len() as u32 {
        if out.len() >= MAX_SCATTER_INSTANCES {
            break;
        }
        let v = VertexId(i);
        if !mesh.is_vert_live(v) {
            continue;
        }
        out.push(ScatterSite {
            position: mesh.vert(v).position,
            normal: safe_normal(mesh.compute_vertex_normal(v)),
        });
    }
    out
}

fn face_center_sites(mesh: &HalfEdgeMesh) -> Vec<ScatterSite> {
    let mut out = Vec::new();
    for i in 0..mesh.faces.len() as u32 {
        if out.len() >= MAX_SCATTER_INSTANCES {
            break;
        }
        let f = FaceId(i);
        if !mesh.is_face_live(f) {
            continue;
        }
        let pts = face_points(mesh, f);
        if pts.len() < 3 {
            continue;
        }
        out.push(ScatterSite {
            position: crate::half_edge::face_center_median_weighted(&pts),
            normal: safe_normal(mesh.compute_face_normal(f)),
        });
    }
    out
}

fn face_points(mesh: &HalfEdgeMesh, f: FaceId) -> Vec<Vec3> {
    mesh.face_vertices(f).iter().map(|v| mesh.vert(*v).position).collect()
}

/// Area-weighted random points. Two nested weighted picks — face by area, then fan triangle by
/// area — so an n-gon is sampled uniformly over its actual surface, not per-triangle.
fn random_face_sites(mesh: &HalfEdgeMesh, count: u32, seed: u32) -> Vec<ScatterSite> {
    // Per-face: ring points, unit normal, fan-triangle cumulative areas, total area.
    struct FaceEntry {
        pts: Vec<Vec3>,
        normal: Vec3,
        tri_cum: Vec<f32>, // cumulative area of fan triangles (0, i+1, i+2)
        area: f32,
    }
    let mut faces: Vec<FaceEntry> = Vec::new();
    let mut cum: Vec<f32> = Vec::new();
    let mut total = 0.0f32;
    for i in 0..mesh.faces.len() as u32 {
        let f = FaceId(i);
        if !mesh.is_face_live(f) {
            continue;
        }
        let pts = face_points(mesh, f);
        if pts.len() < 3 {
            continue;
        }
        let mut tri_cum = Vec::with_capacity(pts.len() - 2);
        let mut acc = 0.0f32;
        for t in 0..pts.len() - 2 {
            let a = (pts[t + 1] - pts[0]).cross(pts[t + 2] - pts[0]).length() * 0.5;
            acc += a;
            tri_cum.push(acc);
        }
        if !(acc > 0.0) || !acc.is_finite() {
            continue;
        }
        total += acc;
        cum.push(total);
        faces.push(FaceEntry {
            normal: safe_normal(mesh.compute_face_normal(f)),
            pts,
            tri_cum,
            area: acc,
        });
    }
    if faces.is_empty() || !(total > 0.0) {
        return Vec::new();
    }
    let n = (count as usize).min(MAX_SCATTER_INSTANCES);
    let mut out = Vec::with_capacity(n);
    for s in 0..n as u32 {
        let target = u01(seed, s, CH_FACE_PICK) * total;
        let fi = cum.partition_point(|&c| c < target).min(faces.len() - 1);
        let fe = &faces[fi];
        // Fan triangle, area-weighted within the face.
        let tt = u01(seed, s, CH_TRI_PICK) * fe.area;
        let ti = fe.tri_cum.partition_point(|&c| c < tt).min(fe.tri_cum.len() - 1);
        let (a, b, c) = (fe.pts[0], fe.pts[ti + 1], fe.pts[ti + 2]);
        // Uniform barycentric in a triangle (sqrt map — the naive (u,v) pair clusters at a corner).
        let u = u01(seed, s, CH_BARY_U).sqrt();
        let v = u01(seed, s, CH_BARY_V);
        let p = a + (b - a) * (u * (1.0 - v)) + (c - a) * (u * v);
        out.push(ScatterSite { position: p, normal: fe.normal });
    }
    out
}

// ── transforms ──────────────────────────────────────────────────────────────

/// Placement matrices for every instance, in the target body's local mesh space.
///
/// Composition (right to left): scale → Euler rotation jitter → spin about local +Y →
/// align +Y to the surface normal → translate to the site plus positional jitter. Spin is
/// applied *before* alignment about +Y, which is exactly "spin about the surface normal"
/// afterwards — and stays meaningful when alignment is off.
pub fn scatter_transforms(mesh: &HalfEdgeMesh, p: &ScatterParams) -> Vec<Mat4> {
    let sites = scatter_sites(mesh, p.source, p.count, p.seed);
    let mut out = Vec::with_capacity(sites.len());
    for (i, site) in sites.iter().enumerate() {
        let s = i as u32;
        let su = range(p.seed, s, CH_SCALE_U, p.scale_min, p.scale_max);
        let scale = Vec3::new(
            su * range(p.seed, s, CH_SCALE_X, p.scale_axis_min.x, p.scale_axis_max.x),
            su * range(p.seed, s, CH_SCALE_Y, p.scale_axis_min.y, p.scale_axis_max.y),
            su * range(p.seed, s, CH_SCALE_Z, p.scale_axis_min.z, p.scale_axis_max.z),
        );
        // A zero/negative scale would produce degenerate or inside-out geometry that the
        // install guards then reject wholesale — clamp to a tiny positive instead.
        let scale = Vec3::new(scale.x.max(1e-5), scale.y.max(1e-5), scale.z.max(1e-5));
        let jitter = Quat::from_euler(
            glam::EulerRot::XYZ,
            range(p.seed, s, CH_ROT_X, -p.rot_jitter_deg.x, p.rot_jitter_deg.x).to_radians(),
            range(p.seed, s, CH_ROT_Y, -p.rot_jitter_deg.y, p.rot_jitter_deg.y).to_radians(),
            range(p.seed, s, CH_ROT_Z, -p.rot_jitter_deg.z, p.rot_jitter_deg.z).to_radians(),
        );
        let spin = Quat::from_rotation_y(range(p.seed, s, CH_SPIN, 0.0, p.spin_deg).to_radians());
        let align = if p.align_to_normal {
            Quat::from_rotation_arc(Vec3::Y, site.normal)
        } else {
            Quat::IDENTITY
        };
        let offset = Vec3::new(
            range(p.seed, s, CH_POS_X, -p.pos_jitter.x, p.pos_jitter.x),
            range(p.seed, s, CH_POS_Y, -p.pos_jitter.y, p.pos_jitter.y),
            range(p.seed, s, CH_POS_Z, -p.pos_jitter.z, p.pos_jitter.z),
        );
        let rot = align * spin * jitter;
        let m = Mat4::from_scale_rotation_translation(scale, rot, site.position + offset);
        if m.is_finite() {
            out.push(m);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half-extent 1 — every surface point has `max|component| == 1`.
    fn cube() -> HalfEdgeMesh {
        HalfEdgeMesh::new_box(Vec3::ONE)
    }

    #[test]
    fn vertex_source_gives_one_site_per_vertex() {
        let m = cube();
        let s = scatter_sites(&m, SCATTER_SRC_VERTICES, 0, 0);
        assert_eq!(s.len(), 8);
        for site in &s {
            assert!((site.normal.length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn face_center_source_gives_one_site_per_face() {
        let m = cube();
        let s = scatter_sites(&m, SCATTER_SRC_FACE_CENTERS, 0, 0);
        assert_eq!(s.len(), 6);
        // A unit-ish cube's face centres sit on the axis planes at ±1.
        for site in &s {
            assert!(site.position.abs().max_element() > 0.9, "{:?}", site.position);
        }
    }

    #[test]
    fn random_source_respects_count_and_cap() {
        let m = cube();
        assert_eq!(scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 137, 7).len(), 137);
        assert_eq!(
            scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 1_000_000, 7).len(),
            MAX_SCATTER_INSTANCES
        );
    }

    #[test]
    fn same_seed_is_reproducible_and_a_new_seed_differs() {
        let m = cube();
        let a = scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 64, 42);
        let b = scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 64, 42);
        let c = scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 64, 43);
        assert_eq!(a, b, "same seed must reproduce exactly");
        assert_ne!(a, c, "a different seed must produce a different field");
    }

    #[test]
    fn raising_the_count_keeps_existing_sites_in_place() {
        // The stateless-draw promise: more density must ADD points, not reshuffle the field.
        let m = cube();
        let a = scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 20, 5);
        let b = scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 60, 5);
        assert_eq!(&a[..], &b[..20]);
    }

    /// Area weighting: a box that is 10x longer in X must land ~10x more points on its long
    /// faces than a per-face (unweighted) pick would. Per-face picking would give 1/6 each.
    #[test]
    fn random_points_are_area_weighted_not_face_weighted() {
        let m = HalfEdgeMesh::new_box(Vec3::new(10.0, 1.0, 1.0));
        let s = scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 2000, 11);
        // The two end caps (|x| extreme) have area 2x2=4 each; the four long faces 20x2=40
        // each. End caps = 8 / 168 ≈ 4.8% of the area.
        let caps = s.iter().filter(|t| t.position.x.abs() > 9.99).count() as f32 / s.len() as f32;
        assert!(caps < 0.12, "end caps took {caps:.3} of the samples — not area weighted");
        assert!(caps > 0.01, "end caps took {caps:.3} — they must still get some");
    }

    #[test]
    fn all_random_points_lie_on_the_surface_of_the_box() {
        let m = cube();
        for s in scatter_sites(&m, SCATTER_SRC_RANDOM_FACES, 500, 3) {
            // A box of half-extent 1: every surface point has max |component| == 1.
            assert!(
                (s.position.abs().max_element() - 1.0).abs() < 1e-3,
                "off-surface sample {:?}",
                s.position
            );
        }
    }

    #[test]
    fn empty_mesh_scatters_nothing() {
        let m = HalfEdgeMesh::new();
        for src in [SCATTER_SRC_VERTICES, SCATTER_SRC_FACE_CENTERS, SCATTER_SRC_RANDOM_FACES] {
            assert!(scatter_sites(&m, src, 100, 1).is_empty());
        }
        assert!(scatter_transforms(&m, &ScatterParams::default()).is_empty());
    }

    #[test]
    fn default_params_place_unrotated_unit_scaled_copies_at_the_sites() {
        let m = cube();
        let p = ScatterParams { align_to_normal: false, ..Default::default() };
        let ts = scatter_transforms(&m, &p);
        let sites = scatter_sites(&m, p.source, p.count, p.seed);
        assert_eq!(ts.len(), sites.len());
        for (t, s) in ts.iter().zip(&sites) {
            let (sc, rot, tr) = t.to_scale_rotation_translation();
            assert!((sc - Vec3::ONE).length() < 1e-5);
            assert!(rot.angle_between(Quat::IDENTITY) < 1e-4);
            assert!((tr - s.position).length() < 1e-5);
        }
    }

    #[test]
    fn align_to_normal_points_local_y_along_the_surface_normal() {
        let m = cube();
        let p = ScatterParams {
            source: SCATTER_SRC_FACE_CENTERS,
            align_to_normal: true,
            ..Default::default()
        };
        let ts = scatter_transforms(&m, &p);
        let sites = scatter_sites(&m, p.source, p.count, p.seed);
        for (t, s) in ts.iter().zip(&sites) {
            let up = t.transform_vector3(Vec3::Y).normalize();
            assert!(up.dot(s.normal) > 0.999, "up {up:?} vs normal {:?}", s.normal);
        }
    }

    #[test]
    fn scale_range_is_respected_and_never_collapses() {
        let m = cube();
        let p = ScatterParams { scale_min: 0.25, scale_max: 2.0, seed: 9, ..Default::default() };
        for t in scatter_transforms(&m, &p) {
            let (sc, _, _) = t.to_scale_rotation_translation();
            assert!(sc.min_element() >= 0.25 - 1e-4 && sc.max_element() <= 2.0 + 1e-4, "{sc:?}");
        }
    }

    #[test]
    fn reversed_ranges_do_not_produce_nan() {
        let m = cube();
        let p = ScatterParams {
            scale_min: 2.0,
            scale_max: 0.5,
            rot_jitter_deg: Vec3::splat(-30.0),
            pos_jitter: Vec3::splat(-0.1),
            seed: 4,
            ..Default::default()
        };
        for t in scatter_transforms(&m, &p) {
            assert!(t.is_finite());
        }
    }
}
