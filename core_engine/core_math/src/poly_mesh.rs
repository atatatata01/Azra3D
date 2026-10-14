//! `PolyMesh` — the NON-SERIALIZED runtime wrapper around `HalfEdgeMesh`.
//!
//! THE load-bearing serialization-safety principle of the whole poly foundation
//! (`POLY_FOUNDATION_PLAN.md` §0, §4.1): **all new poly state is RUNTIME state on this
//! wrapper.** The serialized substrate — `HalfEdgeMesh` (in-blob `Arc` on
//! `NodePayload::PolygonalBody`) and `Vec<PolyModifier>` — is NEVER extended in place.
//!
//! RED LINES this type exists to enforce:
//! - `CustomData` / `TopoIndex` / `topo_gen` live HERE, never as fields of `HalfEdgeMesh`
//!   (a new `HalfEdgeMesh` field shifts bincode bytes → corrupts every `.fl4`).
//! - `PolyMesh` is NEVER serialized and is NEVER a scene-graph node. It is rebuilt on load
//!   from the `Arc<HalfEdgeMesh>` the body already owns. Therefore it derives NO `Serialize`.
//! - No `GenTags` yet (plan §4.4 — demand-driven, introduced with the first cross-op
//!   consumer, NOT speculatively).
//!
//! Phase P3 fills the `CustomData` SoA sidecar with the live typed columns + the `Interp`
//! propagation contract — **`Constant` and `Copy` ONLY** (plan §4.2 Stage A). `Lerp`/`Average`
//! (weight math) are Phase 4 and are deliberately ABSENT (adding them now is scope creep).
//! `TopoIndex` stays empty until Phase 6.

use std::collections::{HashMap, HashSet};

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_EDGE, NO_FACE, NO_TWIN};

// =====================================================================================
// N-gon triangulation for display/export (ear clipping)
// =====================================================================================
//
// `HalfEdgeMesh::to_raster_data` (frozen) fan-triangulates every face from corner 0. That
// cuts straight ACROSS a concave notch — the "random edges" seen on boolean L-faces and any
// concave n-gon. Blender stores n-gons too but triangulates them properly for display; this
// is that triangulation, living here (poly_mesh is the sanctioned home for poly runtime
// logic — `half_edge.rs` stays frozen). The render path (`poly_sync_mesh_cache`) calls
// `raster_data_ngon` instead of the frozen fan.

/// Ear-clip a planar (possibly CONCAVE) polygon into triangles, given its 3D corner positions
/// in CCW-outward order and the face normal. Returns triples of LOCAL corner indices (0..n).
/// Winding is preserved (triangles keep the input order → correct GPU front faces). Convex
/// polygons come out identical to a fan (so existing convex bakes are byte-unchanged). Falls
/// back to a fan if clipping stalls on degenerate/self-touching input (always emits n-2 tris).
pub fn earclip_polygon(pts: &[glam::Vec3], normal: glam::Vec3) -> Vec<[u32; 3]> {
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    if n == 3 {
        return vec![[0, 1, 2]];
    }
    let fan = || (1..n as u32 - 1).map(|j| [0, j, j + 1]).collect::<Vec<_>>();
    // Face-plane 2D basis (u, v with u×v = normal → right-handed, so a CCW-outward polygon has
    // positive signed area).
    let mut nrm = normal.normalize_or_zero();
    if nrm == glam::Vec3::ZERO {
        let mut nn = glam::Vec3::ZERO;
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            nn += glam::Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
        }
        nrm = nn.normalize_or_zero();
    }
    if nrm == glam::Vec3::ZERO {
        return fan(); // truly degenerate (zero-area) face
    }
    let up = if nrm.x.abs() < 0.9 { glam::Vec3::X } else { glam::Vec3::Y };
    let u = up.cross(nrm).normalize_or_zero();
    let v = nrm.cross(u);
    let p2: Vec<glam::Vec2> = pts.iter().map(|p| glam::Vec2::new(p.dot(u), p.dot(v))).collect();
    let cross2 = |a: glam::Vec2, b: glam::Vec2, c: glam::Vec2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let area2: f32 = (0..n).map(|i| { let (a, b) = (p2[i], p2[(i + 1) % n]); a.x * b.y - b.x * a.y }).sum();
    let sgn = if area2 >= 0.0 { 1.0f32 } else { -1.0 };
    // Strictly-inside test (orientation-agnostic), with a small tolerance so a collinear
    // boundary point doesn't wrongly block an ear.
    let in_tri = |p: glam::Vec2, a: glam::Vec2, b: glam::Vec2, c: glam::Vec2| {
        let (d1, d2, d3) = (cross2(p, a, b), cross2(p, b, c), cross2(p, c, a));
        let neg = d1 < -1e-7 || d2 < -1e-7 || d3 < -1e-7;
        let pos = d1 > 1e-7 || d2 > 1e-7 || d3 > 1e-7;
        !(neg && pos)
    };
    let mut idx: Vec<usize> = (0..n).collect();
    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(n - 2);
    let mut guard = 0usize;
    while idx.len() > 3 {
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (ip, ic, inx) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (p2[ip], p2[ic], p2[inx]);
            if cross2(a, b, c) * sgn <= 1e-9 {
                continue; // reflex / collinear tip — not an ear
            }
            let ear = idx.iter().all(|&j| j == ip || j == ic || j == inx || !in_tri(p2[j], a, b, c));
            if ear {
                tris.push([ip as u32, ic as u32, inx as u32]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        guard += 1;
        if !clipped || guard > 4 * n {
            // Stall (non-simple / degenerate) — fan the remainder rather than drop triangles.
            for k in 1..idx.len().saturating_sub(1) {
                tris.push([idx[0] as u32, idx[k] as u32, idx[k + 1] as u32]);
            }
            return tris;
        }
    }
    tris.push([idx[0] as u32, idx[1] as u32, idx[2] as u32]);
    tris
}

/// Auto Smooth angle: edges whose faces meet within this shade SMOOTH (connected-fan
/// average); sharper stay FLAT. 40° keeps 90° box edges hard while multi-segment bevels
/// (90°/N between strips — often ~30–45°) shade as a continuous fillet. Paired with the
/// connected-fan walk below so strip bands don't get discontinuous "step" shading.
///
/// This is the DEFAULT only — a body may author its own angle
/// ([`CustomData::auto_smooth_deg`]), which is what Blender's Shade Smooth / Shade Flat /
/// Smooth-by-Angle drive.
pub const AUTO_SMOOTH_ANGLE_DEG: f32 = 40.0;

/// Blender-style per-body normal control for the display bake.
///
/// Two independent inputs, exactly like Blender:
/// * `angle_deg` — the Auto Smooth threshold. `<= 0` = **Shade Flat** (every corner keeps its
///   own face normal, no fan walk at all); `>= 180` = **Shade Smooth** (the whole connected
///   fan averages, subject to `sharp`).
/// * `sharp` — authored **Mark Sharp** corners (`CustomData.sharp`). A sharp edge breaks the
///   smooth fan REGARDLESS of angle. Checked on BOTH half-edges of the edge, because the
///   layer is per-corner (directed) and a user may have marked only one side.
///
/// [`NormalConfig::default()`] reproduces the historical hardcoded behaviour bit-for-bit
/// (40°, no sharp set, no custom corners), so every existing scene bakes byte-identically.
///
/// **Custom corner normals** (`custom_corner`): when present, the emitter uses them **instead of**
/// the angle-based Auto Smooth fan (`auto_smooth_corner_normal`). Indexed by `HalfEdgeId.0`.
/// Mark Sharp still applies **inside** the producer that builds those corners (Weighted /
/// Smooth Normal pass Mark Sharp as islands) — the angle fan is fully replaced, not layered.
/// See `poly_normal_state` (app) for stack order: **last enabled** Weighted/Smooth Normal wins.
#[derive(Clone, Copy)]
pub struct NormalConfig<'a> {
    pub angle_deg: f32,
    pub sharp: Option<&'a BitLayer<HalfEdgeId>>,
    /// Authored / modifier per-corner normals (one `Vec3` per half-edge slot). `None` = Auto Smooth.
    pub custom_corner: Option<&'a [glam::Vec3]>,
}

impl Default for NormalConfig<'_> {
    fn default() -> Self {
        Self {
            angle_deg: AUTO_SMOOTH_ANGLE_DEG,
            sharp: None,
            custom_corner: None,
        }
    }
}

impl<'a> NormalConfig<'a> {
    /// `true` when every corner must simply take its own face normal (Shade Flat). The fan walk
    /// is skipped entirely — an angle of 0 can never admit a neighbour, and short-circuiting is
    /// both faster and exact (a `cos_thresh` of 1.0 would still merge float-equal normals).
    #[inline]
    pub fn is_flat(&self) -> bool { self.angle_deg <= 0.0 }

    /// Dihedral cosine threshold the fan walk compares against.
    #[inline]
    pub fn cos_thresh(&self) -> f32 { self.angle_deg.clamp(0.0, 180.0).to_radians().cos() }
}

/// `true` when the edge carrying half-edge `h` is marked sharp on EITHER of its two corners.
/// `sharp` is a CORNER (directed half-edge) layer, so a user marking one side must break the
/// fan from both sides — otherwise the two faces would disagree about whether they may merge.
#[inline]
fn edge_is_sharp(
    mesh: &HalfEdgeMesh,
    sharp: Option<&BitLayer<HalfEdgeId>>,
    h: HalfEdgeId,
) -> bool {
    let Some(s) = sharp else { return false };
    if s.get(h) {
        return true;
    }
    let t = mesh.he(h).twin;
    t != NO_TWIN && s.get(t)
}

/// Per-corner auto-smooth normal via a **connected smooth fan** (Blender-style).
///
/// Walks faces around `vid` starting from `start_face`, crossing only edges whose
/// dihedral is within [`AUTO_SMOOTH_ANGLE_DEG`]. Averages those face normals.
///
/// Why not "every face within angle of start"? That angle-cluster differs per face at the
/// same vertex on a multi-strip bevel (strip₀ sees strip₁ but not strip₂; strip₁ sees both)
/// → discontinuous normals across smooth edges → visible **step shading** on fillets
/// (owner 2026-07-30 bevel corner). Connected-fan walk yields the same island for every
/// face that shares a smooth path, so the fillet shades continuously.
/// `sharp` (Mark Sharp) additionally BLOCKS the walk: a marked edge is never crossed, whatever
/// the dihedral. That is the whole point of Blender's Mark Sharp — an artist-authored hard edge
/// on an otherwise-smooth surface.
pub fn auto_smooth_corner_normal(
    mesh: &HalfEdgeMesh,
    face_normals: &[glam::Vec3],
    vid: VertexId,
    start_face: FaceId,
    cos_thresh: f32,
    sharp: Option<&BitLayer<HalfEdgeId>>,
) -> glam::Vec3 {
    let mut ws = FanWorkspace::default();
    auto_smooth_corner_normal_pre(mesh, face_normals, None, vid, start_face, cos_thresh, sharp, &mut ws)
}

/// Reusable buffers for the corner-normal fan walk.
///
/// The walk is per CORNER, and it allocated four `Vec`s each time (fan, ring, visited, queue) plus
/// one more inside every face-area evaluation. On a 40k-quad body that is millions of allocations
/// for one display bake — which is why the bake, not the modifier, dominated a Smooth slider drag
/// (40k quads: 162 ms bake vs 28 ms modifier, `tests/bench_poly_smooth.rs`).
#[derive(Default)]
pub struct FanWorkspace {
    fan: Vec<HalfEdgeId>,
    ring: Vec<(FaceId, HalfEdgeId)>,
    visited: Vec<bool>,
    queue: Vec<usize>,
    scratch: Vec<VertexId>,
}

/// Fan-walk face area — Newell magnitude / 2, floored. ONE definition, used both by the
/// precomputed table and by the on-demand lookup.
fn fan_face_area(mesh: &HalfEdgeMesh, f: FaceId, ring: &mut Vec<VertexId>) -> f32 {
    mesh.face_vertices_into(f, ring);
    let mut nv = glam::Vec3::ZERO;
    for k in 0..ring.len() {
        let a = mesh.vert(ring[k]).position;
        let b = mesh.vert(ring[(k + 1) % ring.len()]).position;
        nv += glam::Vec3::new(
            (a.y - b.y) * (a.z + b.z),
            (a.z - b.z) * (a.x + b.x),
            (a.x - b.x) * (a.y + b.y),
        );
    }
    (nv.length() * 0.5).max(1e-8)
}

/// Every face's fan-walk area, indexed by `FaceId.0`. Dead slots get the floor value (never read).
///
/// A face's area is identical for every corner that visits it, and a quad mesh visits each face
/// ~16 times per bake — so hoisting the table out of the corner loop removes ~15/16 of the work
/// without changing a single weight.
pub fn fan_face_areas(mesh: &HalfEdgeMesh) -> Vec<f32> {
    use rayon::prelude::*;
    (0..mesh.faces.len())
        .into_par_iter()
        .map_init(Vec::new, |ring, i| {
            let fid = FaceId(i as u32);
            if mesh.face(fid).half_edge == NO_EDGE {
                1e-8
            } else {
                fan_face_area(mesh, fid, ring)
            }
        })
        .collect()
}

#[inline]
fn fan_area_lookup(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    table: Option<&[f32]>,
    scratch: &mut Vec<VertexId>,
) -> f32 {
    match table {
        Some(t) => t.get(f.0 as usize).copied().unwrap_or(1e-8),
        None => fan_face_area(mesh, f, scratch),
    }
}

/// [`auto_smooth_corner_normal`] with a precomputed area table and caller-owned scratch.
///
/// `face_areas = None` computes each visited face's area on demand (what the standalone entry
/// point does, so its cost stays proportional to the fan rather than the mesh). The bake passes
/// `Some(&fan_face_areas(mesh))`. Both paths run the SAME accumulation in the SAME order.
#[allow(clippy::too_many_arguments)]
pub fn auto_smooth_corner_normal_pre(
    mesh: &HalfEdgeMesh,
    face_normals: &[glam::Vec3],
    face_areas: Option<&[f32]>,
    vid: VertexId,
    start_face: FaceId,
    cos_thresh: f32,
    sharp: Option<&BitLayer<HalfEdgeId>>,
    ws: &mut FanWorkspace,
) -> glam::Vec3 {
    let start_n = if (start_face.0 as usize) < face_normals.len() {
        face_normals[start_face.0 as usize]
    } else {
        return glam::Vec3::Y;
    };
    if start_n.length_squared() < 1e-12 {
        return glam::Vec3::Y;
    }

    let FanWorkspace { fan, ring, visited, queue, scratch } = ws;

    // Ordered face ring around the vertex (one face per outgoing fan HE).
    mesh.vertex_fan_into(vid, fan);
    if fan.is_empty() {
        return start_n;
    }
    // `(face, outgoing half-edge)` kept TOGETHER: the edge separating ring[i] from ring[i+1] is
    // ring[i]'s own half-edge (fan[i+1] = twin(fan[i]).next lives in twin(fan[i])'s face), and the
    // sharp test needs that half-edge. Filtering faces alone would have desynced the two arrays.
    ring.clear();
    ring.extend(
        fan.iter()
            .map(|&h| (mesh.he(h).face, h))
            .filter(|&(f, _)| f != NO_FACE),
    );
    if ring.is_empty() {
        return start_n;
    }

    // Closed fan (manifold interior) when the circulation returned to start without a boundary.
    let closed = {
        let start = mesh.vert(vid).half_edge;
        if start == NO_EDGE || fan.is_empty() {
            false
        } else {
            let last = fan[fan.len() - 1];
            let twin = mesh.he(last).twin;
            twin != NO_TWIN && mesh.he(twin).next == start
        }
    };

    // Smooth adjacency between consecutive ring faces (shared radial edge at vid).
    let n_ring = ring.len();
    let smooth_edge = |a: FaceId, b: FaceId| -> bool {
        let ia = a.0 as usize;
        let ib = b.0 as usize;
        if ia >= face_normals.len() || ib >= face_normals.len() {
            return false;
        }
        let na = face_normals[ia];
        let nb = face_normals[ib];
        na.length_squared() > 1e-12
            && nb.length_squared() > 1e-12
            && na.dot(nb) >= cos_thresh
    };

    // The half-edge separating two ADJACENT ring slots, or None when they are not neighbours.
    // Explicit rather than modular arithmetic: with a 2-face ring `(i+1) % n` is true in both
    // directions and would name the wrong edge.
    let sep_edge = |a: usize, b: usize| -> Option<HalfEdgeId> {
        if b == a + 1 {
            Some(ring[a].1)
        } else if a == b + 1 {
            Some(ring[b].1)
        } else if closed && n_ring > 2 && ((a == n_ring - 1 && b == 0) || (b == n_ring - 1 && a == 0)) {
            Some(ring[n_ring - 1].1)
        } else {
            None
        }
    };

    // BFS over the ring, starting at start_face, only across smooth edges.
    let start_idx = match ring.iter().position(|&(f, _)| f == start_face) {
        Some(i) => i,
        None => return start_n, // face not in fan (shouldn't happen)
    };
    // AREA-WEIGHTED accumulation (Blender Weighted Normal class; owner 2026-08-03 "thin
    // triangulation line"): equal-weight averaging let a sliver bevel strip tilt the corner
    // normal of a huge face, and the tilted corners render as visible interpolation seams
    // along the ear-clip diagonals of big planar n-gons. Weighting by face area keeps large
    // faces' corners at their own normal while small strips still shade smooth.
    visited.clear();
    visited.resize(n_ring, false);
    queue.clear();
    queue.push(start_idx);
    visited[start_idx] = true;
    let mut n_sum =
        face_normals[start_face.0 as usize] * fan_area_lookup(mesh, start_face, face_areas, scratch);
    let mut qh = 0usize;
    while qh < queue.len() {
        let i = queue[qh];
        qh += 1;
        // Neighbours in the circular (or linear boundary) ring, VISITED IN THIS ORDER — the
        // accumulation into `n_sum` is float addition, so reordering would change the result.
        let cand = [
            if i > 0 {
                Some(i - 1)
            } else if closed {
                Some(n_ring - 1)
            } else {
                None
            },
            if i + 1 < n_ring {
                Some(i + 1)
            } else if closed {
                Some(0)
            } else {
                None
            },
        ];
        for j in cand.into_iter().flatten() {
            if j >= n_ring || visited[j] {
                continue;
            }
            // Mark Sharp wins over the angle: a marked edge is never crossed.
            let Some(e) = sep_edge(i, j) else { continue };
            if edge_is_sharp(mesh, sharp, e) {
                continue;
            }
            if smooth_edge(ring[i].0, ring[j].0) {
                visited[j] = true;
                n_sum += face_normals[ring[j].0 .0 as usize]
                    * fan_area_lookup(mesh, ring[j].0, face_areas, scratch);
                queue.push(j);
            }
        }
    }

    let n = n_sum.normalize_or_zero();
    if n.length_squared() < 0.25 {
        start_n
    } else {
        n
    }
}

/// Render/export raster data for a half-edge mesh, ear-clipping each face (concave-safe).
/// Drop-in replacement for `HalfEdgeMesh::to_raster_data` — same per-corner vertex layout
/// (position + normal + [0,0] uv), only the index triangulation differs.
///
/// NORMALS = AUTO-SMOOTH BY CONNECTED FAN (owner 2026-07-23 + bevel step-shading 2026-07-30):
/// each corner averages face normals reachable by walking across sub-threshold edges around
/// the vertex. Hard 90° box edges stay faceted; multi-segment bevel/fillet bands shade round
/// without discontinuous "steps" between strips.
pub fn raster_data_ngon(mesh: &HalfEdgeMesh) -> (Vec<[f32; 8]>, Vec<u32>) {
    let (v, i, _) = raster_data_ngon_with_materials(mesh);
    (v, i)
}

/// Same as [`raster_data_ngon`], but faces are emitted **sorted by `material_id`** and
/// `ranges` lists contiguous index batches `(material_id, first_index, index_count)`.
///
/// Multi-material poly bodies use one GPU mesh + several draw calls (one range each).
/// Single-material meshes return one range (or empty ranges when no faces).
pub fn raster_data_ngon_with_materials(
    mesh: &HalfEdgeMesh,
) -> (Vec<[f32; 8]>, Vec<u32>, Vec<(u32, u32, u32)>) {
    raster_data_ngon_with_normals(mesh, &NormalConfig::default())
}

/// [`raster_data_ngon_with_materials`] with explicit Blender-style normal control
/// ([`NormalConfig`]: authored Auto Smooth angle + Mark Sharp corner set).
///
/// This is the ONE display/normal emitter — `raster_data_ngon_with_materials` and
/// `raster_data_ngon` are thin defaults over it, so a body with no authored normal state bakes
/// byte-identically to before.
pub fn raster_data_ngon_with_normals(
    mesh: &HalfEdgeMesh,
    cfg: &NormalConfig<'_>,
) -> (Vec<[f32; 8]>, Vec<u32>, Vec<(u32, u32, u32)>) {
    let cos_thresh = cfg.cos_thresh();
    let flat = cfg.is_flat();
    // Per-face and pure, so it fans out; `map_init` gives each worker its own scratch pair rather
    // than allocating one per face. `collect` into a Vec is order-preserving, so the table is
    // identical however rayon schedules it.
    let face_normals: Vec<glam::Vec3> = {
        use rayon::prelude::*;
        (0..mesh.faces.len())
            .into_par_iter()
            .map_init(
                || (Vec::new(), Vec::new()),
                |(ring, pts), i| {
                    let fid = FaceId(i as u32);
                    if mesh.face(fid).half_edge == NO_EDGE {
                        glam::Vec3::ZERO
                    } else {
                        mesh.compute_face_normal_into(fid, ring, pts)
                    }
                },
            )
            .collect()
    };

    // Sort live faces by material so index ranges are contiguous (one draw per material).
    let mut face_order: Vec<u32> = (0..mesh.faces.len() as u32)
        .filter(|&i| mesh.face(FaceId(i)).half_edge != NO_EDGE)
        .collect();
    face_order.sort_by_key(|&i| mesh.face(FaceId(i)).material_id);

    // Hoisted out of the per-corner fan walk: the area table is identical for every corner, and
    // the buffers below are rebuilt per face rather than reallocated. Only built for the Auto
    // Smooth branch — Shade Flat and custom corner normals never walk a fan.
    let fan_areas: Option<Vec<f32>> = if !flat && cfg.custom_corner.is_none() {
        Some(fan_face_areas(mesh))
    } else {
        None
    };

    // ── PHASE 1 (serial, cheap): where each face's output lands ──────────────────────────
    //
    // Every face writes a CONTIGUOUS run of corner vertices and `ring - 2` triangles, so a prefix
    // sum hands each face its slot up front. That is what unlocks phase 2: the original emitter
    // was serial only because `base = verts.len()` had to observe every previous face.
    // `ring < 3` faces emit nothing, exactly as the serial emitter's `continue` skipped them.
    let nf = face_order.len();
    let mut ring_len: Vec<u32> = vec![0; nf];
    {
        let mut fv: Vec<VertexId> = Vec::new();
        for (k, &fi) in face_order.iter().enumerate() {
            mesh.face_vertices_into(FaceId(fi), &mut fv);
            ring_len[k] = if fv.len() >= 3 { fv.len() as u32 } else { 0 };
        }
    }
    let mut vert_base: Vec<u32> = vec![0; nf + 1];
    let mut tri_base: Vec<u32> = vec![0; nf + 1];
    {
        let (mut v, mut t) = (0u32, 0u32);
        for k in 0..nf {
            vert_base[k] = v;
            tri_base[k] = t;
            v += ring_len[k];
            t += ring_len[k].saturating_sub(2);
        }
        vert_base[nf] = v;
        tri_base[nf] = t;
    }

    // ── PHASE 2 (parallel): fill the output, one exclusive slice per block ───────────────
    //
    // Blocks rather than per-face tasks so each worker amortises ONE `FanWorkspace` over many
    // faces. Every block owns disjoint sub-slices (`split_at_mut`), so this is ordinary safe
    // Rust — and because each face computes into its own slot from immutable inputs, the output
    // is bit-identical to the serial emitter regardless of how the work is scheduled.
    let mut verts: Vec<[f32; 8]> = vec![[0.0; 8]; vert_base[nf] as usize];
    let mut tri_slab: Vec<u32> = vec![0; tri_base[nf] as usize * 3];
    let bounds = crate::slice_util::block_bounds(nf, 512);
    {
        let voff: Vec<usize> = bounds.iter().map(|&b| vert_base[b] as usize).collect();
        let ioff: Vec<usize> = bounds.iter().map(|&b| tri_base[b] as usize * 3).collect();
        let vblocks = crate::slice_util::split_at_boundaries(&mut verts, &voff);
        let iblocks = crate::slice_util::split_at_boundaries(&mut tri_slab, &ioff);
        let jobs: Vec<(usize, usize, &mut [[f32; 8]], &mut [u32])> = bounds
            .windows(2)
            .zip(vblocks)
            .zip(iblocks)
            .map(|(((w), vb), ib)| (w[0], w[1], vb, ib))
            .collect();

        let fill = |(lo, hi, vb, ib): (usize, usize, &mut [[f32; 8]], &mut [u32])| {
            let mut ws = FanWorkspace::default();
            let mut fv: Vec<VertexId> = Vec::new();
            let mut fhes: Vec<HalfEdgeId> = Vec::new();
            let mut pts: Vec<glam::Vec3> = Vec::new();
            let mut corner_ns: Vec<glam::Vec3> = Vec::new();
            let (v_origin, i_origin) = (vert_base[lo] as usize, tri_base[lo] as usize * 3);
            for k in lo..hi {
                if ring_len[k] == 0 {
                    continue;
                }
                let fi = face_order[k];
                let fid = FaceId(fi);
                mesh.face_vertices_into(fid, &mut fv);
                let face_n = face_normals[fi as usize];
                let base = vert_base[k];
                pts.clear();
                pts.extend(fv.iter().map(|&vid| mesh.vert(vid).position));
                // face_half_edges zip with face_vertices: HE destination == face_vertices[k].
                mesh.face_half_edges_into(fid, &mut fhes);
                // Shade Flat: every corner keeps its own face normal (no fan walk at all).
                // Custom corner (Weighted/Smooth Normal): REPLACE the Auto Smooth fan entirely.
                // Mark Sharp is already encoded in the custom corners (island at produce time).
                corner_ns.clear();
                if flat {
                    let n = if face_n.length_squared() < 1e-12 { glam::Vec3::Y } else { face_n };
                    corner_ns.resize(fv.len(), n);
                } else if let Some(custom) = cfg.custom_corner {
                    corner_ns.extend(fv.iter().enumerate().map(|(c, _)| {
                        let he = fhes.get(c).copied().unwrap_or(NO_EDGE);
                        let n = custom
                            .get(he.0 as usize)
                            .copied()
                            .filter(|n| n.length_squared() > 1e-12)
                            .unwrap_or(face_n);
                        if n.length_squared() < 1e-12 {
                            glam::Vec3::Y
                        } else {
                            n.normalize_or_zero()
                        }
                    }));
                } else {
                    for c in 0..fv.len() {
                        corner_ns.push(auto_smooth_corner_normal_pre(
                            mesh,
                            &face_normals,
                            fan_areas.as_deref(),
                            fv[c],
                            fid,
                            cos_thresh,
                            cfg.sharp,
                            &mut ws,
                        ));
                    }
                }
                let vslot = vert_base[k] as usize - v_origin;
                for (c, p) in pts.iter().enumerate() {
                    let n = corner_ns[c];
                    vb[vslot + c] = [p.x, p.y, p.z, n.x, n.y, n.z, 0.0, 0.0];
                }
                // NOTE (owner 2026-08-04): a CENTROID-FAN triangulation for smooth n-gons was
                // tried here to kill Mach-band diagonals. REVERTED: it made the display
                // triangulation differ from `poly_select_gpu`'s ear-clip, so clicks picked a
                // DIFFERENT face than the one drawn under the cursor, and its radiating edges
                // read as broken geometry. Display, picking and export must share ONE
                // triangulation. The shading seam is handled by area-weighted corner normals.
                //
                // `earclip_polygon` emits exactly `ring - 2` triangles on EVERY path (ear loop,
                // fan fallback, stall fallback) — the slot reserved in phase 1. The assert is the
                // structural guard: if that ever stops holding, this must fail loudly rather than
                // leave zeroed indices, which would render as a silent hole.
                let islot = tri_base[k] as usize * 3 - i_origin;
                let tris = earclip_polygon(&pts, face_n);
                debug_assert_eq!(
                    tris.len(),
                    ring_len[k] as usize - 2,
                    "earclip emitted {} tris for a {}-gon; the index slab is sized for ring-2",
                    tris.len(),
                    ring_len[k]
                );
                for (ti, t) in tris.iter().enumerate() {
                    ib[islot + ti * 3] = base + t[0];
                    ib[islot + ti * 3 + 1] = base + t[1];
                    ib[islot + ti * 3 + 2] = base + t[2];
                }
                // A short emit (only reachable if the invariant above is ever broken in release)
                // would leave zeros; collapse those to a degenerate triangle on this face's own
                // corners so the GPU discards them instead of drawing a stray fin at vertex 0.
                for ti in tris.len()..ring_len[k] as usize - 2 {
                    ib[islot + ti * 3] = base;
                    ib[islot + ti * 3 + 1] = base;
                    ib[islot + ti * 3 + 2] = base;
                }
            }
        };

        if jobs.len() > 1 {
            use rayon::prelude::*;
            jobs.into_par_iter().for_each(fill);
        } else {
            jobs.into_iter().for_each(fill);
        }
    }

    // ── PHASE 3 (serial, cheap): material ranges ─────────────────────────────────────────
    //
    // `face_order` is material-sorted, so ranges are contiguous runs of the index buffer. Walked
    // in face_order sequence with the SAME bookkeeping the serial emitter used — note the
    // material switch is evaluated for every face INCLUDING `ring < 3` ones, which is why the
    // `continue` sits after it here too.
    let indices = tri_slab;
    let mut ranges: Vec<(u32, u32, u32)> = Vec::new();
    let mut cur_mat: Option<u32> = None;
    let mut range_start: u32 = 0;
    for (k, &fi) in face_order.iter().enumerate() {
        let mat = mesh.face(FaceId(fi)).material_id;
        if cur_mat != Some(mat) {
            if let Some(m) = cur_mat {
                let count = tri_base[k] * 3 - range_start;
                if count > 0 {
                    ranges.push((m, range_start, count));
                }
            }
            cur_mat = Some(mat);
            range_start = tri_base[k] * 3;
        }
    }
    if let Some(m) = cur_mat {
        let count = tri_base[nf] * 3 - range_start;
        if count > 0 {
            ranges.push((m, range_start, count));
        }
    }
    (verts, indices, ranges)
}


/// Triangulate EVERY face of `mesh` into triangles, returning a new all-triangle mesh
/// (matches Blender's Triangulate modifier: output has only tris). Each face is ear-clipped
/// via [`earclip_polygon`] (concave-safe — the SAME triangulation the display bake uses),
/// so a valid ngon comes out as `n-2` tris with preserved winding. Per-face `material_id` is
/// carried onto every child triangle. Vertices are shared by position (rebuilt through
/// [`HalfEdgeMesh::from_polygons`], which welds coincident corners) — IDs are NOT preserved,
/// which is correct for a non-destructive DISPLAY modifier (the base mesh is untouched).
///
/// NOTE vs Blender: Blender's default `quad_method=BEAUTY` picks the shorter diagonal per quad;
/// this uses a robust ear-clip, so an individual quad's diagonal MAY differ. The result is an
/// equally-valid triangulation (same vertex set, all-tris) — the beauty/Delaunay flip is a
/// refinement, not a correctness requirement (see task oracle: "accept a different-but-valid
/// diagonal if BEAUTY differs").
pub fn triangulate_mesh(mesh: &HalfEdgeMesh) -> HalfEdgeMesh {
    triangulate_mesh_faces(mesh, None)
}

/// Triangulate the mesh, optionally restricted to a set of face ids.
///
/// * `only_faces = None` — every live face (same as [`triangulate_mesh`]).
/// * `Some(set)` — faces in the set are ear-clipped to tris; faces outside are copied
///   unchanged (quads/n-gons stay). Empty set → clone-equivalent topology (no-op rebuild).
///
/// Used by the Triangulate modifier's Geometry Channel (tris/quads/n-gons/group/material/…).
pub fn triangulate_mesh_faces(
    mesh: &HalfEdgeMesh,
    only_faces: Option<&std::collections::HashSet<u32>>,
) -> HalfEdgeMesh {
    let mut positions: Vec<glam::Vec3> = Vec::new();
    let mut rings: Vec<Vec<usize>> = Vec::new();
    let mut ring_mats: Vec<u32> = Vec::new();
    let mut remap: HashMap<u32, usize> = HashMap::new();
    for i in 0..mesh.faces.len() as u32 {
        let fid = FaceId(i);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        if let Some(sel) = only_faces {
            if !sel.contains(&i) {
                // Keep original polygon intact.
                let fv = mesh.face_vertices(fid);
                if fv.len() < 3 {
                    continue;
                }
                let g: Vec<usize> = fv
                    .iter()
                    .map(|&vid| {
                        *remap.entry(vid.0).or_insert_with(|| {
                            positions.push(mesh.vert(vid).position);
                            positions.len() - 1
                        })
                    })
                    .collect();
                rings.push(g);
                ring_mats.push(mesh.face(fid).material_id);
                continue;
            }
        }
        let fv = mesh.face_vertices(fid);
        if fv.len() < 3 {
            continue;
        }
        // Global indices for this face's corners, welding coincident vertices across faces.
        let g: Vec<usize> = fv
            .iter()
            .map(|&vid| {
                *remap.entry(vid.0).or_insert_with(|| {
                    positions.push(mesh.vert(vid).position);
                    positions.len() - 1
                })
            })
            .collect();
        let mat = mesh.face(fid).material_id;
        // Already a triangle — copy as-is (avoids needless rebuild of diagonal choice).
        if fv.len() == 3 {
            rings.push(g);
            ring_mats.push(mat);
            continue;
        }
        let pts: Vec<glam::Vec3> = fv.iter().map(|&vid| mesh.vert(vid).position).collect();
        let face_n = mesh.compute_face_normal(fid);
        for t in earclip_polygon(&pts, face_n) {
            // earclip returns LOCAL (0..n) corner indices → map back to global vertex ids.
            rings.push(vec![g[t[0] as usize], g[t[1] as usize], g[t[2] as usize]]);
            ring_mats.push(mat);
        }
    }
    let mut out = HalfEdgeMesh::from_polygons(&positions, &rings);
    for (i, &mid) in ring_mats.iter().enumerate() {
        if i < out.faces.len() {
            out.faces[i].material_id = mid;
        }
    }
    out.recompute_normals();
    out
}

// =====================================================================================
// Attribute domain keys (plan §4.2). Typed newtype ids — never raw `usize` — so a column
// can never be indexed by the wrong domain (the BMesh double-indirection bug, §2).
// =====================================================================================

/// Canonical UNDIRECTED edge key: the two endpoint vertex ids, smaller first. An edge and its
/// twin map to the SAME `EdgeKey`, so an edge-domain column (crease) is twin-symmetric BY
/// CONSTRUCTION — no manual mirror, no half-edge-direction ambiguity. Serializable so it rides the
/// FL4 `PolyLayerRecord` directly (plan §6.2 `crease: Vec<(EdgeKey, f32)>`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeKey(pub u32, pub u32);

impl EdgeKey {
    /// Build the canonical (min,max) key from a half-edge's two endpoint vertices.
    #[inline]
    pub fn new(a: VertexId, b: VertexId) -> Self {
        if a.0 <= b.0 { EdgeKey(a.0, b.0) } else { EdgeKey(b.0, a.0) }
    }

    /// The `EdgeKey` of a half-edge `h` in `m` (its source + destination vertex).
    #[inline]
    pub fn of_half_edge(m: &HalfEdgeMesh, h: HalfEdgeId) -> Self {
        let dst = m.he(h).vertex;
        let src = m.he_source(h);
        EdgeKey::new(src, dst)
    }
}

// =====================================================================================
// Interp contract (plan §4.2). THIS PHASE: `Constant` and `Copy` ONLY. `Lerp`/`Average`
// (parent-weight math) are Phase 4 — adding them now is scope creep (PHASE_P3 §9).
// =====================================================================================

/// How a column's value flows onto a NEW element created by an op (plan §4.2 Interp contract).
/// Stage A (this phase) supports only the weight-free interpolations:
/// - `Constant` — a new element gets the column's declared default (no source needed).
/// - `Copy(parent)` — a new element copies its single source element's value verbatim.
///
/// `Lerp`/`Average` (multi-parent weight blends) are Phase 4 and are intentionally NOT variants
/// here — a column declaring them cannot exist this phase, so no op can silently weight-blend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interp {
    /// New element takes the column default (Stage A). No provenance required.
    Constant,
    /// New element copies a single source element's value (Stage A). The op supplies the
    /// `new → source` correspondence; `apply_delta` reads `column[source]` and writes `column[new]`.
    Copy,
}

// =====================================================================================
// Column types (plan §4.2). Sparse typed-key SoA: a `LayerStore` carries only AUTHORED
// entries (most corners have no UV until authored), so it is O(authored) in memory and
// survives free-list renumbering (keyed by stable typed id, never by dense slot index).
// =====================================================================================

/// A sparse value column keyed by a typed domain id (plan §4.2). `Interp` is the column's
/// declared propagation rule; `apply_delta` honours it over ONLY the op's touched set.
#[derive(Clone, Debug)]
pub struct LayerStore<K: std::hash::Hash + Eq, V: Clone> {
    map: HashMap<K, V>,
    interp: Interp,
}

impl<K: std::hash::Hash + Eq + Copy, V: Clone> LayerStore<K, V> {
    /// A new column with the given propagation rule and no authored entries.
    pub fn new(interp: Interp) -> Self {
        Self { map: HashMap::new(), interp }
    }
    #[inline]
    pub fn interp(&self) -> Interp { self.interp }
    #[inline]
    pub fn get(&self, k: K) -> Option<&V> { self.map.get(&k) }
    #[inline]
    pub fn set(&mut self, k: K, v: V) { self.map.insert(k, v); }
    #[inline]
    pub fn remove(&mut self, k: K) -> Option<V> { self.map.remove(&k) }
    #[inline]
    pub fn contains(&self, k: K) -> bool { self.map.contains_key(&k) }
    #[inline]
    pub fn len(&self) -> usize { self.map.len() }
    #[inline]
    pub fn is_empty(&self) -> bool { self.map.is_empty() }
    /// Iterate authored `(key, value)` pairs — the serialization read path (FL4 `PolyLayerRecord`).
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> { self.map.iter() }
}

/// A boolean (bit) column keyed by a typed domain id (plan §4.2 — `seam`/`sharp`). Membership is
/// the bit. `Constant`-interp by nature: a new element is NOT in the set unless explicitly added.
#[derive(Clone, Debug, Default)]
pub struct BitLayer<K: std::hash::Hash + Eq> {
    set: HashSet<K>,
}

impl<K: std::hash::Hash + Eq + Copy> BitLayer<K> {
    pub fn new() -> Self { Self { set: HashSet::new() } }
    #[inline]
    pub fn get(&self, k: K) -> bool { self.set.contains(&k) }
    #[inline]
    pub fn set(&mut self, k: K, on: bool) {
        if on { self.set.insert(k); } else { self.set.remove(&k); }
    }
    #[inline]
    pub fn len(&self) -> usize { self.set.len() }
    #[inline]
    pub fn is_empty(&self) -> bool { self.set.is_empty() }
    /// Iterate the set members — the serialization read path.
    pub fn iter(&self) -> impl Iterator<Item = &K> { self.set.iter() }
}

/// Identity of an `extra` column: its domain + name (plan §4.2 — `(domain,name) -> typed column`).
/// Direct `column[id]`, no layer-name walk (BMesh's double-indirection bug, §2).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LayerKey {
    pub domain: LayerDomain,
    pub name: String,
}

/// The element domain a column keys on (plan §4.2 explicit typed domains).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LayerDomain {
    Corner, // HalfEdgeId
    Edge,   // EdgeKey
    Vertex, // VertexId
    Face,   // FaceId
}

/// A dynamically-typed extra column (plan §4.2 `extra: HashMap<LayerKey, DynLayer>`). Stage A holds
/// only the float-vector shape that covers generic authored scalars/vectors without a new variant
/// per op. Keyed by raw id within its declared `LayerKey.domain`.
#[derive(Clone, Debug)]
pub struct DynLayer {
    pub interp: Interp,
    pub values: HashMap<u32, Vec<f32>>,
}

impl DynLayer {
    pub fn new(interp: Interp) -> Self { Self { interp, values: HashMap::new() } }
    pub fn is_empty(&self) -> bool { self.values.is_empty() }
}

// =====================================================================================
// CustomData — the runtime SoA attribute sidecar (plan §4.2). Each column declares its
// Interp. ALL of this is on the wrapper — ZERO new `HalfEdgeMesh` fields (the red line).
// =====================================================================================

/// How a polygroup was created (AUTO_POLYGROUPS.md). Used for auto-naming and future reverse ops.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum PolyGroupKind {
    #[default]
    Manual = 0,
    Bevel = 1,
    Extrude = 2,
    Inset = 3,
    LoopCut = 4,
    Bridge = 5,
    Solidify = 6,
    Other = 255,
}

impl PolyGroupKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "Group",
            Self::Bevel => "Bevel",
            Self::Extrude => "Extrude",
            Self::Inset => "Inset",
            Self::LoopCut => "Loop Cut",
            Self::Bridge => "Bridge",
            Self::Solidify => "Shell",
            Self::Other => "Group",
        }
    }
}

/// Optional bevel params stored on a Bevel polygroup for reverse/resize without session history.
/// (Geometry reverse still uses topology guess or `PolyBevelHistory` for exact mesh restore.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolyGroupBevelParams {
    pub width: f32,
    pub segments: u32,
    pub shape: f32,
    pub clamp: bool,
}

impl Default for PolyGroupBevelParams {
    fn default() -> Self {
        Self {
            width: 0.1,
            segments: 1,
            shape: 0.5,
            clamp: true,
        }
    }
}

/// Per-body polygroup metadata (AUTO_POLYGROUPS.md). Not stored per-face — only on the sidecar.
/// `Eq` omitted: optional f32 bevel payload is only `PartialEq`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolyGroupMeta {
    pub id: u16,
    pub name: String,
    /// Display tint RGB 0..255.
    pub color: [u8; 3],
    pub kind: PolyGroupKind,
    /// When `kind == Bevel`, last confirmed bevel tool params (persist across sessions).
    #[serde(default)]
    pub bevel: Option<PolyGroupBevelParams>,
}

impl PolyGroupMeta {
    /// Stable **distinct saturated** colour from group id (golden-angle HSV).
    ///
    /// Used for solid viewport / matcap polygroup paint — not pastel transparent tints.
    /// Adjacent ids land far apart on the hue wheel so neighbouring groups never look alike.
    pub fn color_for_id(id: u16) -> [u8; 3] {
        // Golden conjugate: successive ids ≈ 137.5° apart on the hue circle.
        let h = ((id as f32) * 0.618_033_988).fract();
        let s = 0.78_f32;
        let v = 0.94_f32;
        // HSV → RGB
        let i = (h * 6.0).floor();
        let f = h * 6.0 - i;
        let p = v * (1.0 - s);
        let q = v * (1.0 - f * s);
        let t = v * (1.0 - (1.0 - f) * s);
        let (r, g, b) = match (i as i32).rem_euclid(6) {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        [
            (r * 255.0).round().clamp(0.0, 255.0) as u8,
            (g * 255.0).round().clamp(0.0, 255.0) as u8,
            (b * 255.0).round().clamp(0.0, 255.0) as u8,
        ]
    }
}

/// Runtime SoA attribute sidecar (plan §4.2). Filled in Phase P3 with the live typed columns,
/// `Copy`/`Constant` Interp only. NOT serialized: authored layers persist via an in-blob
/// `#[serde(default)] Vec<PolyLayerRecord>` field on `ProjectFile` (the `voxel_grids` precedent),
/// reattached by `node_id` on load — never from this struct directly.
#[derive(Clone, Debug)]
pub struct CustomData {
    /// CORNER domain — UV differs per face-corner. `Copy` interp (extrude/inset Copy the source
    /// corner's UV onto each new corner; Stage A trivial preserve).
    pub uv: LayerStore<HalfEdgeId, Vec2>,
    /// CORNER domain — UV seam bit. `Constant` by nature (new corners are not seams unless authored).
    pub seam: BitLayer<HalfEdgeId>,
    /// CORNER domain — hard-edge / autosmooth split bit. Drives split-vs-averaged normals in the bake.
    pub sharp: BitLayer<HalfEdgeId>,
    /// EDGE domain — Catmull-Clark crease 0..1. Twin-symmetric by `EdgeKey` construction.
    pub crease: LayerStore<EdgeKey, f32>,
    /// VERTEX domain — vertex color.
    pub vcolor: LayerStore<VertexId, [u8; 4]>,
    /// VERTEX domain — vertex crease 0..1.
    pub vcrease: LayerStore<VertexId, f32>,
    /// CORNER domain — this edge was CREATED by an operation (a bevel's segment edges, a
    /// boolean's cutter seams) rather than authored by the user. Written BY the operation
    /// when it runs and carried across renumbering by `remap_by_position`, so it survives a
    /// destructive edit and later sessions. Bevel Smart Select reads it (owner 2026-08-04:
    /// "those tags should be set when I model, this is not angle base like blender").
    pub op_created: BitLayer<HalfEdgeId>,
    /// FACE domain — auto/manual polygroup id (0 = ungrouped, never stored). `Copy` for Euler ops;
    /// rebuild ops (bevel `from_polygons`) re-tag new faces explicitly (AUTO_POLYGROUPS.md).
    pub groups: LayerStore<FaceId, u16>,
    /// Per-body polygroup names/colours/kinds. Ids are unique within this sidecar (1..).
    pub group_meta: Vec<PolyGroupMeta>,
    /// BODY-domain (like `group_meta`, not a per-element column) — the authored Auto Smooth
    /// angle in degrees. `None` = use [`AUTO_SMOOTH_ANGLE_DEG`], which is what every existing
    /// scene gets, so nothing changes for them. `Some(0)` = Shade Flat, `Some(180)` = Shade
    /// Smooth (Blender's two menu entries are just the ends of this one slider).
    ///
    /// It lives HERE rather than on `NodePayload::PolygonalBody` for the save-format red line:
    /// a new field on the payload would shift bincode bytes and corrupt every `.fl4`. Persistence
    /// rides the self-delimiting `FL4PNORM` tail section (see `project/io.rs`) — zero blob change.
    pub auto_smooth_deg: Option<f32>,
    /// Open-ended typed columns by `(domain, name)` (plan §4.2). Empty unless authored.
    pub extra: HashMap<LayerKey, DynLayer>,
}

impl Default for CustomData {
    fn default() -> Self { Self::new() }
}

impl CustomData {
    /// Carry this sidecar across an operation that RENUMBERED the mesh (a boolean, an
    /// extrude install) — `docs/planning/poly/SIDECAR_SURVIVES_BOOLEAN.md` stage S1.
    ///
    /// Every layer here is keyed by id, and a boolean invalidates every id, which is why the
    /// install currently CLEARS the sidecar (`BOOL-STALECD`) and the user loses UVs, seams,
    /// creases and polygroups on every boolean. Ids do not survive — but POSITIONS do, for
    /// the geometry the boolean did not cut, and that is exactly the geometry whose data is
    /// worth keeping (owner rule: a boolean must not change geometry it did not cut).
    ///
    /// Matching is on QUANTIZED position at the kernel's own tolerance, so this and the
    /// boolean agree by construction rather than by coincidence. Elements with no match are
    /// genuinely new and keep the layer default — a value is NEVER guessed onto new geometry.
    pub fn remap_by_position(&self, old: &HalfEdgeMesh, new: &HalfEdgeMesh) -> CustomData {
        use crate::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE};
        use std::collections::HashMap;
        /// 1e-5, matching `mesh_boolean::rebuild::PRESERVE_Q`.
        const Q: f32 = 1e5;
        type K3 = (i64, i64, i64);
        let qp = |p: glam::Vec3| -> K3 {
            ((p.x * Q).round() as i64, (p.y * Q).round() as i64, (p.z * Q).round() as i64)
        };
        let centroid = |m: &HalfEdgeMesh, f: FaceId| -> glam::Vec3 {
            let vs = m.face_vertices(f);
            if vs.is_empty() {
                return glam::Vec3::ZERO;
            }
            vs.iter().map(|&v| m.vert(v).position).sum::<glam::Vec3>() / vs.len() as f32
        };

        let mut out = CustomData::new();
        out.group_meta = self.group_meta.clone(); // ids are per-body, not per-element
        out.auto_smooth_deg = self.auto_smooth_deg; // BODY domain — no element to remap

        // ---- old lookup tables, keyed by position ----
        let mut old_vert: HashMap<K3, VertexId> = HashMap::new();
        for i in 0..old.vertices.len() as u32 {
            let v = VertexId(i);
            if old.is_vert_live(v) {
                old_vert.insert(qp(old.vert(v).position), v);
            }
        }
        // A directed half-edge is identified by its (source, destination) positions; a face by
        // its centroid.
        let mut old_he: HashMap<(K3, K3), HalfEdgeId> = HashMap::new();
        for i in 0..old.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !old.is_edge_live(h) {
                continue;
            }
            old_he.insert(
                (qp(old.vert(old.he_source(h)).position), qp(old.vert(old.he(h).vertex).position)),
                h,
            );
        }
        let mut old_face: HashMap<K3, FaceId> = HashMap::new();
        for i in 0..old.faces.len() as u32 {
            let f = FaceId(i);
            if old.face(f).half_edge != NO_EDGE {
                old_face.insert(qp(centroid(old, f)), f);
            }
        }

        // ---- VERTEX domain ----
        for i in 0..new.vertices.len() as u32 {
            let nv = VertexId(i);
            if !new.is_vert_live(nv) {
                continue;
            }
            let Some(&ov) = old_vert.get(&qp(new.vert(nv).position)) else { continue };
            if let Some(c) = self.vcolor.get(ov) {
                out.vcolor.set(nv, *c);
            }
            if let Some(c) = self.vcrease.get(ov) {
                out.vcrease.set(nv, *c);
            }
        }

        // ---- CORNER + EDGE domains ----
        for i in 0..new.half_edges.len() as u32 {
            let nh = HalfEdgeId(i);
            if !new.is_edge_live(nh) {
                continue;
            }
            let (sp, dp) = (
                qp(new.vert(new.he_source(nh)).position),
                qp(new.vert(new.he(nh).vertex).position),
            );
            let Some(&oh) = old_he.get(&(sp, dp)) else { continue };
            if let Some(uv) = self.uv.get(oh) {
                out.uv.set(nh, *uv);
            }
            if self.seam.get(oh) {
                out.seam.set(nh, true);
            }
            if self.sharp.get(oh) {
                out.sharp.set(nh, true);
            }
            if self.op_created.get(oh) {
                out.op_created.set(nh, true);
            }
            let (ok, nk) = (
                EdgeKey::new(old.he_source(oh), old.he(oh).vertex),
                EdgeKey::new(new.he_source(nh), new.he(nh).vertex),
            );
            if let Some(cr) = self.crease.get(ok) {
                out.crease.set(nk, *cr);
            }
        }

        // ---- FACE domain ----
        // A face the boolean SPLIT has a different centroid and so gets no group: a split
        // face is genuinely new geometry, and inheriting a group would be a guess.
        for i in 0..new.faces.len() as u32 {
            let nf = FaceId(i);
            if new.face(nf).half_edge == NO_EDGE {
                continue;
            }
            let Some(&of) = old_face.get(&qp(centroid(new, nf))) else { continue };
            if let Some(g) = self.groups.get(of) {
                out.groups.set(nf, *g);
            }
        }
        out
    }

    /// Stamp `op_created` for edges known BY ID on `donor` into this sidecar, whose keys
    /// belong to `target`. Each donor half-edge is matched into `target` by its directed
    /// (source, destination) QUANTIZED position pair — the same 1e-5 quantization as
    /// `remap_by_position`, so the two agree by construction. Needed when the mesh that the
    /// operation produced (where the created-edge ids live) is subsequently REBUILT before
    /// install (e.g. the island boolean's keep-cutter `merge_meshes` re-runs `from_polygons`
    /// and renumbers every id). An unmatched donor edge is dropped — fail closed, a tag is
    /// never guessed onto the wrong edge.
    pub fn stamp_op_created_by_position(
        &mut self,
        donor: &HalfEdgeMesh,
        donor_edges: &[HalfEdgeId],
        target: &HalfEdgeMesh,
    ) {
        use crate::half_edge::HalfEdgeId;
        use std::collections::HashMap;
        /// 1e-5, matching `remap_by_position` / `mesh_boolean::rebuild::PRESERVE_Q`.
        const Q: f32 = 1e5;
        type K3 = (i64, i64, i64);
        let qp = |p: glam::Vec3| -> K3 {
            ((p.x * Q).round() as i64, (p.y * Q).round() as i64, (p.z * Q).round() as i64)
        };
        let mut tgt: HashMap<(K3, K3), HalfEdgeId> = HashMap::new();
        for i in 0..target.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !target.is_edge_live(h) {
                continue;
            }
            tgt.insert(
                (
                    qp(target.vert(target.he_source(h)).position),
                    qp(target.vert(target.he(h).vertex).position),
                ),
                h,
            );
        }
        for &dh in donor_edges {
            if (dh.0 as usize) >= donor.half_edges.len() || !donor.is_edge_live(dh) {
                continue;
            }
            let key = (
                qp(donor.vert(donor.he_source(dh)).position),
                qp(donor.vert(donor.he(dh).vertex).position),
            );
            if let Some(&th) = tgt.get(&key) {
                self.op_created.set(th, true);
            }
        }
    }

    /// A fresh sidecar with every column present but empty, each carrying its declared Interp.
    pub fn new() -> Self {
        Self {
            uv: LayerStore::new(Interp::Copy),
            seam: BitLayer::new(),
            sharp: BitLayer::new(),
            op_created: BitLayer::new(),
            crease: LayerStore::new(Interp::Copy),
            vcolor: LayerStore::new(Interp::Copy),
            vcrease: LayerStore::new(Interp::Copy),
            groups: LayerStore::new(Interp::Copy),
            group_meta: Vec::new(),
            auto_smooth_deg: None,
            extra: HashMap::new(),
        }
    }

    /// The effective Auto Smooth angle for this body (authored, else the global default).
    #[inline]
    pub fn auto_smooth_angle(&self) -> f32 {
        self.auto_smooth_deg.unwrap_or(AUTO_SMOOTH_ANGLE_DEG)
    }

    /// The normal-authoring config this sidecar describes: authored angle + the `sharp` set
    /// (omitted when empty so the walk pays nothing). The ONE place app code turns a sidecar
    /// into a [`NormalConfig`].
    pub fn normal_config(&self) -> NormalConfig<'_> {
        NormalConfig {
            angle_deg: self.auto_smooth_angle(),
            sharp: if self.sharp.is_empty() { None } else { Some(&self.sharp) },
            custom_corner: None, // modifier corners come from poly_normal_state only
        }
    }

    /// `true` when this body carries authored NORMAL state (Mark Sharp corners or a non-default
    /// Auto Smooth angle) — i.e. when the display bake must be told about it and the save must
    /// emit a `FL4PNORM` record.
    pub fn has_authored_normals(&self) -> bool {
        self.auto_smooth_deg.is_some() || !self.sharp.is_empty()
    }

    /// `true` when NO column carries any authored entry. Lets `mesh_eq` and the FL4 round-trip
    /// suite take the empty fast-path (an empty sidecar must bake byte-identically to the old
    /// hardcoded-UV `to_raster_data`, Acceptance) without special-casing.
    pub fn is_empty(&self) -> bool {
        self.uv.is_empty()
            && self.seam.is_empty()
            && self.sharp.is_empty()
            && self.crease.is_empty()
            && self.vcolor.is_empty()
            && self.vcrease.is_empty()
            && self.groups.is_empty()
            && self.group_meta.is_empty()
            && self.auto_smooth_deg.is_none()
            && self.extra.values().all(|d| d.is_empty())
    }

    /// Next free polygroup id (1..). 0 is reserved for ungrouped.
    pub fn next_group_id(&self) -> u16 {
        let max_meta = self.group_meta.iter().map(|m| m.id).max().unwrap_or(0);
        let max_used = self.groups.iter().map(|(_, &g)| g).max().unwrap_or(0);
        max_meta.max(max_used).saturating_add(1).max(1)
    }

    /// Create a new group meta entry; returns its id. Does not assign any faces.
    pub fn create_group(&mut self, kind: PolyGroupKind, name: Option<String>) -> u16 {
        self.create_group_ex(kind, name, None)
    }

    /// Create a group with optional bevel reverse params.
    pub fn create_group_ex(
        &mut self,
        kind: PolyGroupKind,
        name: Option<String>,
        bevel: Option<PolyGroupBevelParams>,
    ) -> u16 {
        let id = self.next_group_id();
        let name = name.unwrap_or_else(|| {
            let n = self.group_meta.iter().filter(|m| m.kind == kind).count() + 1;
            format!("{} {}", kind.label(), n)
        });
        self.group_meta.push(PolyGroupMeta {
            id,
            name,
            color: PolyGroupMeta::color_for_id(id),
            kind,
            bevel,
        });
        id
    }

    /// Rename group `gid`. Returns false if missing or name empty after trim.
    pub fn rename_group(&mut self, gid: u16, name: String) -> bool {
        let name = name.trim().to_string();
        if name.is_empty() {
            return false;
        }
        if let Some(m) = self.group_meta.iter_mut().find(|m| m.id == gid) {
            m.name = name;
            true
        } else {
            false
        }
    }

    /// Assign `gid` to every face in `faces` (0 clears). Sparse store: 0 removes the entry.
    pub fn assign_faces_to_group(&mut self, faces: impl IntoIterator<Item = FaceId>, gid: u16) {
        for fid in faces {
            if gid == 0 {
                self.groups.remove(fid);
            } else {
                self.groups.set(fid, gid);
            }
        }
    }

    /// All live face ids that carry `gid`.
    pub fn faces_in_group(&self, gid: u16) -> Vec<FaceId> {
        if gid == 0 {
            return Vec::new();
        }
        self.groups
            .iter()
            .filter(|(_, &g)| g == gid)
            .map(|(&f, _)| f)
            .collect()
    }

    /// Drop face→group entries whose faces are free/dead, and drop meta for empty groups.
    /// Call after topology rebuilds and before showing the group list.
    pub fn purge_empty_groups(&mut self, mesh: &HalfEdgeMesh) {
        // `free_faces` is private (allocator detail — `alloc_face`'s LIFO reuse depends on its
        // order), so liveness goes through the public `is_face_live` predicate instead of an
        // open-coded free-list scan. Also O(1) per face rather than building a HashSet per call.
        let stale: Vec<FaceId> = self
            .groups
            .iter()
            .filter_map(|(&fid, _)| {
                if !mesh.is_face_live(fid) || mesh.face(fid).half_edge == NO_EDGE {
                    Some(fid)
                } else {
                    None
                }
            })
            .collect();
        for f in stale {
            self.groups.remove(f);
        }
        let live: HashSet<u16> = self.groups.iter().map(|(_, &g)| g).collect();
        self.group_meta.retain(|m| live.contains(&m.id));
    }

    /// `true` when any polygroup meta or face assignment exists.
    pub fn has_polygroups(&self) -> bool {
        !self.group_meta.is_empty() || !self.groups.is_empty()
    }

    /// Serialize the AUTHORED layers to the plain tuple vectors the FL4 `PolyLayerRecord` carries
    /// (plan §6.2). Deterministically ORDERED (sorted by key) so a save→load→save round-trip is
    /// byte-reproducible — `HashMap` iteration order is otherwise unstable.
    #[allow(clippy::type_complexity)]
    pub fn to_layer_tuples(&self) -> (
        Vec<(u32, Vec2)>,      // uv: (HalfEdgeId.0, uv)
        Vec<(EdgeKey, f32)>,   // crease: (EdgeKey, weight)
        Vec<u32>,              // seam: HalfEdgeId.0
        Vec<u32>,              // sharp: HalfEdgeId.0
        Vec<(u32, u16)>,       // groups: (FaceId.0, group_id)
        Vec<PolyGroupMeta>,    // group_meta
    ) {
        let mut uv: Vec<(u32, Vec2)> = self.uv.iter().map(|(k, v)| (k.0, *v)).collect();
        uv.sort_by_key(|(k, _)| *k);
        let mut crease: Vec<(EdgeKey, f32)> = self.crease.iter().map(|(k, v)| (*k, *v)).collect();
        crease.sort_by_key(|(k, _)| *k);
        let mut seam: Vec<u32> = self.seam.iter().map(|k| k.0).collect();
        seam.sort_unstable();
        let mut sharp: Vec<u32> = self.sharp.iter().map(|k| k.0).collect();
        sharp.sort_unstable();
        let mut groups: Vec<(u32, u16)> = self
            .groups
            .iter()
            .filter(|(_, &g)| g != 0)
            .map(|(k, v)| (k.0, *v))
            .collect();
        groups.sort_by_key(|(k, _)| *k);
        let mut group_meta = self.group_meta.clone();
        group_meta.sort_by_key(|m| m.id);
        (uv, crease, seam, sharp, groups, group_meta)
    }

    /// Rebuild a `CustomData` from the FL4 `PolyLayerRecord` tuple vectors (the load path, plan §6.2).
    /// The inverse of `to_layer_tuples`. Columns keep their declared Interp.
    /// `groups` / `group_meta` may be empty (pre-polygroup files).
    pub fn from_layer_tuples(
        uv: &[(u32, Vec2)],
        crease: &[(EdgeKey, f32)],
        seam: &[u32],
        sharp: &[u32],
        groups: &[(u32, u16)],
        group_meta: &[PolyGroupMeta],
    ) -> Self {
        let mut cd = CustomData::new();
        for &(k, v) in uv { cd.uv.set(HalfEdgeId(k), v); }
        for &(k, v) in crease { cd.crease.set(k, v); }
        for &k in seam { cd.seam.set(HalfEdgeId(k), true); }
        for &k in sharp { cd.sharp.set(HalfEdgeId(k), true); }
        for &(fid, gid) in groups {
            if gid != 0 {
                cd.groups.set(FaceId(fid), gid);
            }
        }
        cd.group_meta = group_meta.to_vec();
        cd
    }

    /// Byte-equality of every column (plan §4.5 `mesh_eq` deep oracle — strengthened for live
    /// columns this phase). Order-independent (sparse maps): compares authored entry sets. Returns
    /// `Err(reason)` with the first mismatching column for a readable proptest shrink message.
    pub fn columns_eq(&self, other: &CustomData) -> Result<(), String> {
        if self.uv.len() != other.uv.len() {
            return Err(format!("uv column len {} != {}", self.uv.len(), other.uv.len()));
        }
        for (k, v) in self.uv.iter() {
            match other.uv.get(*k) {
                Some(o) if o == v => {}
                _ => return Err(format!("uv corner {:?} differs", k)),
            }
        }
        if self.crease.len() != other.crease.len() {
            return Err(format!("crease column len {} != {}", self.crease.len(), other.crease.len()));
        }
        for (k, v) in self.crease.iter() {
            match other.crease.get(*k) {
                Some(o) if o == v => {}
                _ => return Err(format!("crease edge {:?} differs", k)),
            }
        }
        if self.vcolor.len() != other.vcolor.len() {
            return Err(format!("vcolor column len {} != {}", self.vcolor.len(), other.vcolor.len()));
        }
        for (k, v) in self.vcolor.iter() {
            match other.vcolor.get(*k) {
                Some(o) if o == v => {}
                _ => return Err(format!("vcolor vertex {:?} differs", k)),
            }
        }
        if self.vcrease.len() != other.vcrease.len() {
            return Err(format!("vcrease column len {} != {}", self.vcrease.len(), other.vcrease.len()));
        }
        for (k, v) in self.vcrease.iter() {
            match other.vcrease.get(*k) {
                Some(o) if o == v => {}
                _ => return Err(format!("vcrease vertex {:?} differs", k)),
            }
        }
        // seam / sharp: set equality.
        if self.seam.len() != other.seam.len() {
            return Err(format!("seam set len {} != {}", self.seam.len(), other.seam.len()));
        }
        for k in self.seam.iter() {
            if !other.seam.get(*k) { return Err(format!("seam corner {:?} differs", k)); }
        }
        if self.sharp.len() != other.sharp.len() {
            return Err(format!("sharp set len {} != {}", self.sharp.len(), other.sharp.len()));
        }
        for k in self.sharp.iter() {
            if !other.sharp.get(*k) { return Err(format!("sharp corner {:?} differs", k)); }
        }
        if self.groups.len() != other.groups.len() {
            return Err(format!("groups column len {} != {}", self.groups.len(), other.groups.len()));
        }
        for (k, v) in self.groups.iter() {
            match other.groups.get(*k) {
                Some(o) if o == v => {}
                _ => return Err(format!("groups face {:?} differs", k)),
            }
        }
        if self.group_meta.len() != other.group_meta.len() {
            return Err(format!(
                "group_meta len {} != {}",
                self.group_meta.len(),
                other.group_meta.len()
            ));
        }
        Ok(())
    }
}

/// Derived adjacency index (plan §4.3): deterministic angle-sorted disk + radial cycles, patched
/// incrementally on `*_mut` ops. EMPTY until Phase 6. Distinct type so the radial/non-manifold
/// overflow lands here (NOT as a new `HalfEdge` field — red line).
#[derive(Clone, Debug, Default)]
pub struct TopoIndex {
    // Phase 6: disk: Vec<Vec<HalfEdgeId>> (angle-sorted), radial overflow for N>=3 edges.
    // Intentionally empty in P3.
}

impl TopoIndex {
    pub fn new() -> Self { Self::default() }

    /// `true` while no adjacency cycles are built.
    pub fn is_empty(&self) -> bool { true }
}

/// Runtime wrapper: the serialized `HalfEdgeMesh` substrate + all NON-serialized derived/authored
/// runtime state. Lives in app/cache state, rebuilt on load. Carrying state here is the ONLY way
/// new poly state is added without risking the FL4 save format.
///
/// `GenTags` (plan §4.4) are deliberately ABSENT this phase — they arrive demand-driven with the
/// first cross-op consumer (persistent element selection / cross-frame falloff cache).
#[derive(Clone, Debug)]
pub struct PolyMesh {
    /// The serialized substrate (an `Arc<HalfEdgeMesh>` in the scene node; owned here for editing).
    pub topo: HalfEdgeMesh,
    /// §4.2 runtime SoA attribute sidecar. Live columns this phase (Copy/Constant Interp).
    pub cd: CustomData,
    /// §4.3 derived disk + radial cycles. Empty until Phase 6.
    pub topo_index: TopoIndex,
    /// Bumps on every topology change → drives lazy re-eval + incremental index patch (§4.1).
    pub topo_gen: u64,
}

impl PolyMesh {
    /// Wrap an existing `HalfEdgeMesh` with empty runtime sidecars at generation 0.
    pub fn new(topo: HalfEdgeMesh) -> Self {
        Self { topo, cd: CustomData::new(), topo_index: TopoIndex::new(), topo_gen: 0 }
    }

    /// Replace the topology after a topology-changing op and bump `topo_gen`. Used by the rebuild-
    /// path ops (the `from_polygons` pattern). Sidecars carry over as-is; `apply_delta` (the op
    /// wrapper) is responsible for propagating columns onto new elements per their Interp.
    pub fn set_topo(&mut self, topo: HalfEdgeMesh) {
        self.topo = topo;
        self.topo_gen = self.topo_gen.wrapping_add(1);
    }

    /// Read access to the substrate (the public half-edge API ops read through).
    pub fn topo(&self) -> &HalfEdgeMesh { &self.topo }

    /// Object-Mode bake WITH CustomData overlay (plan §6.1 — the wrapper bake boundary). Produces
    /// the SAME `[px,py,pz,nx,ny,nz,u,v]` + index layout as `HalfEdgeMesh::to_raster_data`
    /// (`half_edge.rs:1376`), but OVERLAYS the authored per-corner `uv` into the hardcoded `[0,0]`
    /// slot and chooses split (per-face flat) vs averaged (smooth) normals from the `sharp` column.
    ///
    /// RED LINE: this lives on the wrapper, NOT in `half_edge.rs` (frozen). It reuses the kernel's
    /// public winding/triangulation accessors (`face_half_edges` / `face_vertices` /
    /// `compute_face_normal`) — it does NOT re-implement triangulation.
    ///
    /// BYTE-IDENTITY GUARANTEE (Acceptance): when `cd` is empty the output equals the old
    /// `to_raster_data` byte-for-byte. We enforce this STRUCTURALLY by delegating to the kernel's
    /// `to_raster_data` on the empty fast-path — the overlay path runs ONLY when a column is authored.
    pub fn to_raster_data_with_cd(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        // Empty sidecar → the old path verbatim (the byte-identity gate, enforced by delegation).
        if self.cd.is_empty() {
            return self.topo.to_raster_data();
        }

        let m = &self.topo;
        // Smooth normals are computed only when `sharp` is authored (otherwise the bake stays
        // all-hard = face-normal-per-corner, matching the old output for a uv-only sidecar).
        let use_smooth = !self.cd.sharp.is_empty();

        let mut verts: Vec<[f32; 8]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();

        for (i, _f) in m.faces.iter().enumerate() {
            let fid = FaceId(i as u32);
            if m.face(fid).half_edge == NO_EDGE { continue; }
            let face_hes = m.face_half_edges(fid);
            if face_hes.len() < 3 { continue; }

            let face_n = m.compute_face_normal(fid);
            let base = verts.len() as u32;

            // Emit one vertex per face-corner (matching the kernel's per-face split-vertex layout).
            // A corner is the half-edge whose DESTINATION vertex is that corner (the same association
            // `to_raster_data` uses via `face_vertices`, which returns each `he.vertex` in order).
            let mut pts: Vec<glam::Vec3> = Vec::with_capacity(face_hes.len());
            for &he in &face_hes {
                let vid = m.he(he).vertex;
                let p = m.vert(vid).position;

                // Normal: per-face (hard) unless smoothing is on AND this corner's incident edges are
                // not sharp — then average across the corner's smooth fan.
                let n = if use_smooth {
                    self.smooth_corner_normal(vid, fid, face_n)
                } else {
                    face_n
                };

                // UV overlay: authored per-corner uv, else the old default [0,0].
                let uv = self.cd.uv.get(he).copied().unwrap_or(Vec2::ZERO);

                verts.push([p.x, p.y, p.z, n.x, n.y, n.z, uv.x, uv.y]);
                pts.push(p);
            }

            // Ear-clip (concave-safe; convex faces come out identical to the old fan).
            for t in earclip_polygon(&pts, face_n) {
                indices.push(base + t[0]);
                indices.push(base + t[1]);
                indices.push(base + t[2]);
            }
        }

        (verts, indices)
    }

    /// Averaged (smooth) normal for the corner `(vid, fid)`: the normalized sum of the face normals
    /// of every face in `vid`'s SMOOTH FAN — the faces around `vid` reachable from `fid` without
    /// crossing a `sharp` corner. A fully sharp-ringed vertex falls back to the single `face_n`
    /// (hard), so a `sharp` cage reproduces hard edges exactly. No `normalize()` without the
    /// zero-guard (Half-Edge rule).
    fn smooth_corner_normal(&self, vid: VertexId, fid: FaceId, face_n: glam::Vec3) -> glam::Vec3 {
        use crate::half_edge::NO_FACE;
        let m = &self.topo;
        // The faces incident to vid (via the outgoing fan). For each, include its normal iff the
        // corner of vid on that face is NOT separated from `fid` by a sharp edge. Stage A treats the
        // smooth fan conservatively: a face contributes when NEITHER of vid's two incident edges on
        // that face is marked sharp (a sharp edge breaks the smoothing across it).
        let mut sum = face_n;
        let mut count = 1u32;
        for &out_he in &m.vertex_fan(vid) {
            let f = m.he(out_he).face;
            if f == NO_FACE || f == fid { continue; }
            // vid's two incident half-edges on face f: the outgoing `out_he` and the one pointing TO
            // vid (its prev). If either is sharp, the edge breaks smoothing → skip this face.
            let in_he = m.he(out_he).prev;
            let out_sharp = self.cd.sharp.get(out_he)
                || m.he(out_he).twin != crate::half_edge::NO_TWIN && self.cd.sharp.get(m.he(out_he).twin);
            let in_sharp = in_he != NO_EDGE && (self.cd.sharp.get(in_he)
                || m.he(in_he).twin != crate::half_edge::NO_TWIN && self.cd.sharp.get(m.he(in_he).twin));
            if out_sharp || in_sharp { continue; }
            sum += m.compute_face_normal(f);
            count += 1;
        }
        let _ = count;
        sum.normalize_or_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    /// Signed area of a triangle projected onto `normal`.
    fn tri_area_signed(a: Vec3, b: Vec3, c: Vec3, normal: Vec3) -> f32 {
        (b - a).cross(c - a).dot(normal) * 0.5
    }

    /// A unit cube (6 quads, 8 shared verts) — the Triangulate oracle base.
    fn cube() -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(-1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,1.0),
            Vec3::new(-1.0,-1.0,1.0), Vec3::new(-1.0,1.0,-1.0), Vec3::new(1.0,1.0,-1.0),
            Vec3::new(1.0,1.0,1.0), Vec3::new(-1.0,1.0,1.0),
        ];
        let f: Vec<Vec<usize>> = vec![
            vec![0,1,2,3], vec![7,6,5,4], vec![4,5,1,0],
            vec![6,7,3,2], vec![7,4,0,3], vec![5,6,2,1],
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// M6 — face material_id participates in content_signature (assign invalidates display cache).
    #[test]
    fn m6_face_material_changes_content_signature() {
        let mut mesh = cube();
        let a = mesh.content_signature();
        mesh.faces[0].material_id = 42;
        let b = mesh.content_signature();
        assert_ne!(
            a, b,
            "changing HFace.material_id must change content_signature"
        );
        mesh.faces[0].material_id = 0;
        assert_eq!(
            mesh.content_signature(),
            a,
            "restoring material_id restores signature"
        );
    }

    /// M6 gate — red/blue face sets produce two material ranges with correct index batches.
    #[test]
    fn m6_raster_multi_material_ranges_red_blue() {
        let mut mesh = cube();
        // Assign half the faces mat 10, half mat 20.
        let n = mesh.faces.len();
        for i in 0..n {
            mesh.faces[i].material_id = if i < n / 2 { 10 } else { 20 };
        }
        let (_v, indices, ranges) = raster_data_ngon_with_materials(&mesh);
        assert!(
            ranges.len() >= 2,
            "two materials must yield ≥2 ranges, got {ranges:?}"
        );
        let mats: Vec<u32> = ranges.iter().map(|r| r.0).collect();
        assert!(mats.contains(&10) && mats.contains(&20), "ranges mats={mats:?}");
        // Ranges cover every index exactly once.
        let mut covered = 0u32;
        for &(_, first, count) in &ranges {
            assert_eq!(first % 3, 0, "first_index must be tri-aligned");
            assert_eq!(count % 3, 0, "count must be tri-aligned");
            covered += count;
        }
        assert_eq!(covered, indices.len() as u32);
        // Sorted by material_id.
        for w in ranges.windows(2) {
            assert!(w[0].0 <= w[1].0, "ranges must be sorted by material_id");
        }
    }

    /// Multi-segment bevel fillet must shade continuously (no "step" banding between strips).
    /// Regression for angle-cluster auto-smooth: adjacent strip faces at the same vertex used
    /// different normal averages → discontinuous lighting on the round corner.
    #[test]
    fn bevel_fillet_raster_normals_are_continuous_across_strips() {
        use crate::bevel_op::{bevel_edges_offset, BevelParams};
        use std::collections::HashSet;

        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        // One top edge (both verts at y ≈ +1).
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(crate::half_edge::HalfEdgeId(i));
            if he.twin == crate::half_edge::NO_TWIN || i > he.twin.0 {
                continue;
            }
            let a = cube.he_source(crate::half_edge::HalfEdgeId(i)).0;
            let b = he.vertex.0;
            let ya = cube.vert(VertexId(a)).position.y;
            let yb = cube.vert(VertexId(b)).position.y;
            if (ya - 1.0).abs() < 1e-4 && (yb - 1.0).abs() < 1e-4 {
                sel.insert(i);
                break;
            }
        }
        assert!(!sel.is_empty(), "need a selectable top edge");
        let out = bevel_edges_offset(
            &cube,
            &sel,
            &BevelParams {
                width: 0.25,
                segments: 3,
                shape: 0.5,
                clamp: true,
            },
        );
        let (verts, indices) = raster_data_ngon(&out);
        assert!(indices.len() >= 9, "beveled cube must produce triangles");

        // Bucket raster corners by quantized position; at each shared position, normals from
        // different incident faces should agree closely when the edge is smooth (fillet band).
        // A large max angle at the SAME position means discontinuous step shading.
        use std::collections::HashMap;
        let q = |x: f32| (x * 200.0).round() as i32;
        let mut by_pos: HashMap<[i32; 3], Vec<Vec3>> = HashMap::new();
        for tri in indices.chunks_exact(3) {
            for &vi in tri {
                let v = &verts[vi as usize];
                let key = [q(v[0]), q(v[1]), q(v[2])];
                by_pos
                    .entry(key)
                    .or_default()
                    .push(Vec3::new(v[3], v[4], v[5]).normalize_or_zero());
            }
        }
        let mut worst = 0.0f32;
        for ns in by_pos.values() {
            if ns.len() < 2 {
                continue;
            }
            for i in 0..ns.len() {
                for j in i + 1..ns.len() {
                    let d = ns[i].dot(ns[j]).clamp(-1.0, 1.0).acos().to_degrees();
                    worst = worst.max(d);
                }
            }
        }
        // With connected-fan smooth, fillet-band corners at one vertex stay within a few degrees.
        // Angle-cluster left 15–40° steps across strip seams. Allow a little for hard cube edges
        // (90°) that also meet at vertices — those SHOULD disagree. Filter: only check positions
        // that are NOT original cube corners (±1,±1,±1).
        // On the fillet BAND (verts with |y| clearly below the uncut top face, not cube corners):
        // multi-strip normals at one position must agree. Hard cube corners still split at 90°.
        let mut worst_fillet = 0.0f32;
        let mut sample_pos = [0.0f32; 3];
        for (key, ns) in &by_pos {
            if ns.len() < 2 {
                continue;
            }
            let px = key[0] as f32 / 200.0;
            let py = key[1] as f32 / 200.0;
            let pz = key[2] as f32 / 200.0;
            // Fillet interior: top was y=1, width 0.25 → rails near y≈0.75; skip pure cube corners.
            let on_top_rail = py > 0.6 && py < 0.95;
            let not_corner = px.abs() < 0.95 && pz.abs() < 0.95
                || (px.abs() - 1.0).abs() > 0.08 && (pz.abs() - 1.0).abs() > 0.08;
            // Mid-band of a single edge bevel: near y=1 plane, not at ±1,±1 corners.
            let mid_band = py > 0.5
                && (px.abs() - 1.0).abs() > 0.1
                && (pz.abs() - 1.0).abs() > 0.1;
            if !mid_band && !(on_top_rail && not_corner) {
                continue;
            }
            for i in 0..ns.len() {
                for j in i + 1..ns.len() {
                    let d = ns[i].dot(ns[j]).clamp(-1.0, 1.0).acos().to_degrees();
                    if d > worst_fillet {
                        worst_fillet = d;
                        sample_pos = [px, py, pz];
                    }
                }
            }
        }
        assert!(
            worst_fillet < 8.0,
            "bevel fillet strip normals must be continuous at shared verts \
             (worst divergence {worst_fillet:.1}° at {sample_pos:?}; step shading if large). \
             global worst={worst:.1}°"
        );
    }

    /// Triangulate modifier — Blender oracle IDENTITY. Blender's Triangulate modifier (BEAUTY) on
    /// the size-2 cube yields: 8 verts, 12 faces, all-tris, vert set = the 8 corners (captured
    /// 2026-07-17: `TRI_RESULT={"nverts":8,"nfaces":12,"all_tris":true, corners}`). AZRA must match
    /// vert MULTISET + face count + all-tris (diagonal choice may differ — BEAUTY vs ear-clip — which
    /// is an accepted valid variation, task oracle).
    #[test]
    fn triangulate_mesh_matches_blender_cube_oracle() {
        let out = triangulate_mesh(&cube());
        let live_faces: Vec<FaceId> = (0..out.faces.len() as u32).map(FaceId)
            .filter(|&f| out.face(f).half_edge != NO_EDGE).collect();
        assert_eq!(live_faces.len(), 12, "cube triangulate → 12 tris (Blender oracle)");
        for &f in &live_faces {
            assert_eq!(out.face_vertices(f).len(), 3, "every face must be a triangle");
        }
        // Vert set identity (8 corners, welded — no duplication).
        let mut got: Vec<[i32;3]> = out.vertices.iter()
            .map(|v| [(v.position.x*100.0) as i32,(v.position.y*100.0) as i32,(v.position.z*100.0) as i32])
            .collect();
        got.sort(); got.dedup();
        assert_eq!(got.len(), 8, "8 distinct corner positions (Blender oracle: nverts=8)");
        assert!(out.validate().is_ok(), "triangulated mesh must be topologically valid");
        assert!(out.is_manifold().is_ok(), "triangulated cube must be manifold");
    }

    /// EAR-CLIP concave n-gon: an L-shaped hexagon (like a boolean side face). Every triangle must
    /// lie INSIDE the polygon — i.e. all triangles wind the SAME way as the polygon (a fan-from-0
    /// would emit a triangle spanning the concave notch with the opposite/zero winding), and the
    /// triangle areas sum to the polygon area (full cover, no overlap/gap).
    #[test]
    fn earclip_concave_l_hexagon() {
        // L in the XY plane (normal +Z), CCW-outward. Concave corner at (1,1).
        let pts = [
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0), Vec3::new(1.0, 2.0, 0.0), Vec3::new(0.0, 2.0, 0.0),
        ];
        let nrm = Vec3::Z;
        let tris = earclip_polygon(&pts, nrm);
        assert_eq!(tris.len(), 4, "hexagon → n-2 = 4 triangles");
        let mut area = 0.0f32;
        for t in &tris {
            let (a, b, c) = (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
            let s = tri_area_signed(a, b, c, nrm);
            assert!(s > 1e-6, "every ear triangle must wind outward (a fan would flip one across the notch): {s}");
            area += s;
        }
        // L area = 2×2 square minus the 1×1 missing corner = 3.0.
        assert!((area - 3.0).abs() < 1e-4, "triangles must exactly cover the L (area {area}, want 3.0)");
    }

    /// END-TO-END render path: an L-prism (two CONCAVE hexagon caps + side quads, like a boolean
    /// chair side) through `raster_data_ngon`. Every emitted triangle must wind outward (dot with
    /// its face normal > 0) — a fan-from-0 on the concave caps would emit a notch-spanning triangle
    /// with the WRONG normal (the "random edges" bug). This is the exact defect the user reported.
    #[test]
    fn raster_ngon_concave_caps_all_outward() {
        // L cross-section in XY (concave at (1,1)), extruded along Z from 0 to 1.
        let l = [[0.0,0.0],[2.0,0.0],[2.0,1.0],[1.0,1.0],[1.0,2.0],[0.0,2.0]];
        let n = l.len();
        let mut pos: Vec<Vec3> = Vec::new();
        for &[x,y] in &l { pos.push(Vec3::new(x,y,0.0)); }          // bottom cap 0..n
        for &[x,y] in &l { pos.push(Vec3::new(x,y,1.0)); }          // top cap n..2n
        let mut faces: Vec<Vec<usize>> = Vec::new();
        faces.push((0..n).rev().collect());                        // bottom (−Z, outward = reversed)
        faces.push((n..2*n).collect());                            // top (+Z)
        for i in 0..n { let j=(i+1)%n; faces.push(vec![i, j, n+j, n+i]); } // side walls
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&i| { let he = m.he(HalfEdgeId(i)); he.face != crate::half_edge::NO_FACE && he.twin == crate::half_edge::NO_TWIN }).count();
        assert_eq!(bnd, 0, "L-prism must be closed");
        let (verts, indices) = raster_data_ngon(&m);
        assert_eq!(indices.len() % 3, 0);
        for t in indices.chunks(3) {
            let p = |k: u32| Vec3::new(verts[k as usize][0], verts[k as usize][1], verts[k as usize][2]);
            let nrm = Vec3::new(verts[t[0] as usize][3], verts[t[0] as usize][4], verts[t[0] as usize][5]);
            let (a,b,c) = (p(t[0]), p(t[1]), p(t[2]));
            let tn = (b-a).cross(c-a);
            assert!(tn.dot(nrm) > 1e-6,
                "every render triangle must wind outward with its face normal (fan-across-notch bug); dot={}", tn.dot(nrm));
        }
    }

    /// Ear-clip a convex quad → 2 valid outward triangles covering the unit square (a valid
    /// diagonal; not necessarily the fan's 0-2, but every triangle winds correctly).
    #[test]
    fn earclip_convex_quad_valid() {
        let pts = [Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y];
        let tris = earclip_polygon(&pts, Vec3::Z);
        assert_eq!(tris.len(), 2, "quad → 2 triangles");
        let mut area = 0.0f32;
        for t in &tris {
            let s = tri_area_signed(pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize], Vec3::Z);
            assert!(s > 1e-6, "outward winding");
            area += s;
        }
        assert!((area - 1.0).abs() < 1e-4, "covers the unit square (area {area})");
    }

    #[test]
    fn wrapper_carries_empty_sidecars_at_gen_zero() {
        let m = HalfEdgeMesh::from_polygons(
            &[Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y],
            &[vec![0, 1, 2, 3]],
        );
        let pm = PolyMesh::new(m);
        assert_eq!(pm.topo_gen, 0);
        assert!(pm.cd.is_empty(), "fresh CustomData must be empty");
        assert!(pm.topo_index.is_empty(), "P3 TopoIndex must be empty");
        assert_eq!(pm.topo().face_count(), 1);
    }

    #[test]
    fn set_topo_bumps_generation() {
        let m = HalfEdgeMesh::from_polygons(
            &[Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y],
            &[vec![0, 1, 2, 3]],
        );
        let mut pm = PolyMesh::new(m.clone());
        assert_eq!(pm.topo_gen, 0);
        pm.set_topo(m);
        assert_eq!(pm.topo_gen, 1, "topology replacement must bump topo_gen");
    }

    #[test]
    fn customdata_columns_author_and_compare() {
        let mut cd = CustomData::new();
        assert!(cd.is_empty());
        cd.uv.set(HalfEdgeId(3), Vec2::new(0.25, 0.75));
        cd.sharp.set(HalfEdgeId(5), true);
        cd.crease.set(EdgeKey(0, 1), 0.5);
        assert!(!cd.is_empty(), "authoring a column makes the sidecar non-empty");

        let mut cd2 = CustomData::new();
        cd2.uv.set(HalfEdgeId(3), Vec2::new(0.25, 0.75));
        cd2.sharp.set(HalfEdgeId(5), true);
        cd2.crease.set(EdgeKey(0, 1), 0.5);
        assert!(cd.columns_eq(&cd2).is_ok(), "identical columns must compare equal");

        cd2.uv.set(HalfEdgeId(3), Vec2::new(0.9, 0.1));
        assert!(cd.columns_eq(&cd2).is_err(), "a differing uv must break columns_eq");
    }

    #[test]
    fn edge_key_is_canonical_undirected() {
        assert_eq!(EdgeKey::new(VertexId(5), VertexId(2)), EdgeKey(2, 5));
        assert_eq!(EdgeKey::new(VertexId(2), VertexId(5)), EdgeKey(2, 5));
        assert_eq!(
            EdgeKey::new(VertexId(2), VertexId(5)),
            EdgeKey::new(VertexId(5), VertexId(2)),
            "an edge and its twin map to the same canonical key"
        );
    }

    // =================================================================================
    // GOLDEN-TOPOLOGY BAKE GATE (PHASE_P3 Acceptance — Step C). Empty CustomData bakes
    // byte-identical to the old `to_raster_data`; an authored UV + sharp edge overlays
    // the expected corner UVs and split-vs-averaged normals.
    // =================================================================================

    use crate::half_edge::HalfEdgeMesh;

    #[test]
    fn empty_customdata_bake_is_byte_identical_to_old() {
        // Every primitive shape — the empty-sidecar bake MUST equal the kernel's `to_raster_data`
        // byte-for-byte (the wrapper delegates on the empty fast-path).
        for m in [
            HalfEdgeMesh::new_box(Vec3::ONE),
            HalfEdgeMesh::new_uv_sphere(0.8, 12, 8),
            HalfEdgeMesh::new_cylinder(0.6, 2.0, 12),
        ] {
            let old = m.to_raster_data();
            let pm = PolyMesh::new(m);
            let new = pm.to_raster_data_with_cd();
            assert_eq!(new.0, old.0, "empty-CustomData vert array must be byte-identical to old");
            assert_eq!(new.1, old.1, "empty-CustomData index array must be byte-identical to old");
        }
    }

    #[test]
    fn authored_uv_overlays_into_the_slot() {
        let mut pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        // Author a distinct UV on each corner of face 0.
        let f0 = pm.topo.face_half_edges(FaceId(0));
        for (i, &he) in f0.iter().enumerate() {
            pm.cd.uv.set(he, Vec2::new(0.1 * (i as f32 + 1.0), 0.2 * (i as f32 + 1.0)));
        }
        let (verts, _idx) = pm.to_raster_data_with_cd();
        // Face 0 is emitted first (4 corners); their uv slots must equal the authored values in order.
        for (i, &he) in f0.iter().enumerate() {
            let want = *pm.cd.uv.get(he).unwrap();
            let (u, v) = (verts[i][6], verts[i][7]);
            assert!((u - want.x).abs() < 1e-6 && (v - want.y).abs() < 1e-6,
                "corner {i}: baked uv ({u},{v}) != authored ({},{})", want.x, want.y);
        }
        // Unauthored corners (later faces) keep the default [0,0].
        let n_f0 = f0.len();
        assert_eq!((verts[n_f0][6], verts[n_f0][7]), (0.0, 0.0), "unauthored corner stays [0,0]");
    }

    #[test]
    fn sharp_edge_drives_split_vs_averaged_normals() {
        // With NO sharp authored but a uv authored, normals stay per-face (hard) — same as old.
        let mut pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        pm.cd.uv.set(pm.topo.face_half_edges(FaceId(0))[0], Vec2::new(0.5, 0.5));
        let hard = pm.to_raster_data_with_cd();
        let old = pm.topo.to_raster_data();
        // Normals (slots 3,4,5) must match the per-face hard normals when no sharp is set.
        for (h, o) in hard.0.iter().zip(old.0.iter()) {
            assert!((h[3] - o[3]).abs() < 1e-6 && (h[4] - o[4]).abs() < 1e-6 && (h[5] - o[5]).abs() < 1e-6,
                "uv-only sidecar must keep per-face hard normals");
        }

        // Now author sharp on ALL edges → every vertex is fully sharp-ringed → still hard (a sharp
        // cage reproduces hard edges exactly), proving the split path is reachable + correct.
        let mut pm2 = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        for i in 0..pm2.topo.half_edges.len() as u32 {
            pm2.cd.sharp.set(HalfEdgeId(i), true);
        }
        let all_sharp = pm2.to_raster_data_with_cd();
        for (s, o) in all_sharp.0.iter().zip(old.0.iter()) {
            assert!((s[3] - o[3]).abs() < 1e-5 && (s[4] - o[4]).abs() < 1e-5 && (s[5] - o[5]).abs() < 1e-5,
                "an all-sharp cage must reproduce the hard per-face normals");
        }

        // With sharp authored on only SOME edges, at least one corner's normal differs from the hard
        // face normal (smoothing kicked in across the non-sharp edges) — the split-vs-averaged choice.
        let mut pm3 = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        // Mark a single edge sharp → the rest of the surface smooths.
        pm3.cd.sharp.set(HalfEdgeId(0), true);
        let some = pm3.to_raster_data_with_cd();
        let any_smoothed = some.0.iter().zip(old.0.iter()).any(|(s, o)| {
            (s[3] - o[3]).abs() > 1e-4 || (s[4] - o[4]).abs() > 1e-4 || (s[5] - o[5]).abs() > 1e-4
        });
        assert!(any_smoothed, "a partially-sharp mesh must average (smooth) some corner normals");
        // Same vertex count + winding as the hard bake (only normals change, not topology).
        assert_eq!(some.0.len(), hard.0.len(), "smoothing must not change the vertex count");
        assert_eq!(some.1, hard.1, "smoothing must not change the triangulation");
    }

    // =================================================================================
    // Blender-style normal control on the VIEWPORT path
    // (`raster_data_ngon_with_normals` — Mark Sharp / Auto Smooth angle / Shade Flat|Smooth)
    // =================================================================================

    /// A 3×3 quad grid bent into a gentle parabola along X (y = 0.15·x²). Every adjacent-face
    /// dihedral is ~16.7° — comfortably INSIDE the 40° default, so the default bake smooths the
    /// whole surface. The four INTERIOR vertices have a CLOSED 4-face fan, which is what makes
    /// this the right Mark Sharp probe: a split can only come from the mark, not from the angle,
    /// and not from a truncated boundary fan.
    fn curved_grid() -> HalfEdgeMesh {
        let idx = |i: usize, j: usize| i * 4 + j;
        let mut p = Vec::with_capacity(16);
        for i in 0..4usize {
            for j in 0..4usize {
                let x = i as f32 - 1.5;
                p.push(Vec3::new(x, 0.15 * x * x, j as f32 - 1.5));
            }
        }
        let mut f: Vec<Vec<usize>> = Vec::with_capacity(9);
        for i in 0..3usize {
            for j in 0..3usize {
                f.push(vec![idx(i, j), idx(i, j + 1), idx(i + 1, j + 1), idx(i + 1, j)]);
            }
        }
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// Vertex id of grid node `(i, j)` in [`curved_grid`] (positions are unique, so match on them).
    fn grid_vert(m: &HalfEdgeMesh, i: usize, j: usize) -> VertexId {
        let x = i as f32 - 1.5;
        let want = Vec3::new(x, 0.15 * x * x, j as f32 - 1.5);
        for k in 0..m.vertices.len() as u32 {
            let v = VertexId(k);
            if m.is_vert_live(v) && (m.vert(v).position - want).length() < 1e-5 {
                return v;
            }
        }
        panic!("grid vertex ({i},{j}) not found");
    }

    /// The straight edge LINE at x-index `i`, running along Z — what a Blender user marks when
    /// they mark an edge loop. Marking a lone edge would NOT split a closed fan (the walk simply
    /// goes round the other way, exactly as Blender does); cutting the ring takes a line.
    fn grid_edge_line(m: &HalfEdgeMesh, i: usize) -> Vec<HalfEdgeId> {
        (0..3)
            .map(|j| he_between(m, grid_vert(m, i, j), grid_vert(m, i, j + 1)))
            .collect()
    }

    /// Every emitted corner normal at the given position (one per incident face-corner).
    fn normals_at(verts: &[[f32; 8]], pos: Vec3) -> Vec<Vec3> {
        verts
            .iter()
            .filter(|v| (Vec3::new(v[0], v[1], v[2]) - pos).length() < 1e-5)
            .map(|v| Vec3::new(v[3], v[4], v[5]).normalize_or_zero())
            .collect()
    }

    /// Any live half-edge of the undirected edge `a–b`.
    fn he_between(m: &HalfEdgeMesh, a: VertexId, b: VertexId) -> HalfEdgeId {
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) {
                continue;
            }
            let (s, d) = (m.he_source(h), m.he(h).vertex);
            if (s == a && d == b) || (s == b && d == a) {
                return h;
            }
        }
        panic!("no half-edge between {a:?} and {b:?}");
    }

    /// GATE 1 — a MARKED SHARP edge gives its two sides DIFFERENT corner normals at the shared
    /// vertex, and clearing the mark re-merges them. This is Blender's Mark Sharp.
    #[test]
    fn mark_sharp_splits_corner_normals_and_clearing_remerges() {
        let m = curved_grid();
        // The probe vertex: interior node (2,1) — a CLOSED 4-face fan, two faces on each side of
        // the x-index-2 edge line.
        let probe = m.vert(grid_vert(&m, 2, 1)).position;

        // --- unmarked: all four incident corners share ONE smoothed normal ---
        let (v0, i0, _) = raster_data_ngon_with_normals(&m, &NormalConfig::default());
        let merged = normals_at(&v0, probe);
        assert_eq!(merged.len(), 4, "the interior vertex has one corner per incident face");
        for n in &merged {
            assert!(
                n.dot(merged[0]) > 0.99999,
                "without Mark Sharp every side must share ONE normal (got {n:?} vs {:?})",
                merged[0]
            );
        }

        // --- mark the x-index-2 edge line sharp (Blender's "Mark Sharp" on an edge loop) ---
        // Only ONE of the two half-edges per edge is marked, proving the reader checks the twin.
        let mut sharp = BitLayer::<HalfEdgeId>::new();
        for h in grid_edge_line(&m, 2) {
            sharp.set(h, true);
        }
        let cfg = NormalConfig {
            angle_deg: AUTO_SMOOTH_ANGLE_DEG,
            sharp: Some(&sharp),
            custom_corner: None,
        };
        let (v1, i1, _) = raster_data_ngon_with_normals(&m, &cfg);
        let split = normals_at(&v1, probe);
        assert_eq!(split.len(), 4, "marking sharp must not change the corner count");
        // Exactly TWO distinct normals now — one per side of the marked line.
        let mut distinct: Vec<Vec3> = Vec::new();
        for n in &split {
            if !distinct.iter().any(|d| d.dot(*n) > 0.9999) {
                distinct.push(*n);
            }
        }
        assert_eq!(
            distinct.len(),
            2,
            "a marked sharp edge must give its two sides DIFFERENT corner normals, got {split:?}"
        );
        let d_split = distinct[0].dot(distinct[1]).clamp(-1.0, 1.0).acos().to_degrees();
        assert!(
            d_split > 5.0,
            "the two sides must differ by the real dihedral, got {d_split:.2}°"
        );
        // Each side's normal IS that side's face normal (both faces on a side are parallel here).
        for n in &distinct {
            let matches_a_face = (0..m.faces.len() as u32)
                .map(FaceId)
                .filter(|&f| m.face(f).half_edge != NO_EDGE)
                .any(|f| m.compute_face_normal(f).normalize_or_zero().dot(*n) > 0.9999);
            assert!(matches_a_face, "each side of a sharp edge keeps its own face normal: {n:?}");
        }

        // --- topology untouched, and clearing the mark restores the merged bake EXACTLY ---
        assert_eq!(i1, i0, "Mark Sharp must not change the triangulation");
        let cleared = NormalConfig {
            angle_deg: AUTO_SMOOTH_ANGLE_DEG,
            sharp: Some(&BitLayer::new()),
            custom_corner: None,
        };
        let (v2, _, _) = raster_data_ngon_with_normals(&m, &cleared);
        assert_eq!(
            bytes_of(&v2),
            bytes_of(&v0),
            "Clear Sharp must restore the exact unmarked bake"
        );
    }

    fn bytes_of(v: &[[f32; 8]]) -> Vec<u32> {
        v.iter().flat_map(|r| r.iter().map(|f| f.to_bits())).collect()
    }

    /// GATE 2 — the DEFAULT (40°, no sharp) is byte-identical to what an untouched mesh baked
    /// before this feature: `raster_data_ngon_with_materials` still routes through the default
    /// config, an EMPTY sharp layer is inert, and a box still emits pure per-face normals.
    #[test]
    fn default_auto_smooth_is_byte_identical_for_an_untouched_mesh() {
        for m in [HalfEdgeMesh::new_box(Vec3::ONE), curved_grid(), cube()] {
            let (va, ia, ra) = raster_data_ngon_with_materials(&m);
            let (vb, ib, rb) = raster_data_ngon_with_normals(&m, &NormalConfig::default());
            assert_eq!(bytes_of(&va), bytes_of(&vb), "default config must be the legacy path");
            assert_eq!(ia, ib);
            assert_eq!(ra, rb);
            // An EMPTY authored sharp layer must be exactly as inert as `None`.
            let empty = BitLayer::<HalfEdgeId>::new();
            let (vc, ic, rc) = raster_data_ngon_with_normals(
                &m,
                &NormalConfig {
                    angle_deg: AUTO_SMOOTH_ANGLE_DEG,
                    sharp: Some(&empty),
                    custom_corner: None,
                },
            );
            assert_eq!(bytes_of(&va), bytes_of(&vc), "an empty sharp layer must change nothing");
            assert_eq!(ia, ic);
            assert_eq!(ra, rc);
        }

        // Behavioural pin for the default on a BOX: 90° edges exceed 40°, so every corner keeps
        // its own face normal — 24 split corners, 36 indices, axis-aligned unit normals.
        let box_m = HalfEdgeMesh::new_box(Vec3::ONE);
        let (v, i, _) = raster_data_ngon_with_materials(&box_m);
        assert_eq!(v.len(), 24, "box bakes 4 split corners per face");
        assert_eq!(i.len(), 36);
        for r in &v {
            let n = Vec3::new(r[3], r[4], r[5]);
            let axis_aligned = (n.x.abs() + n.y.abs() + n.z.abs() - 1.0).abs() < 1e-5;
            assert!(axis_aligned, "default 40° must keep box edges HARD (got {n:?})");
        }
    }

    /// GATE 3 — Shade Flat gives every corner its own face normal; Shade Smooth averages the
    /// whole fan. These are the two ends of the one Auto Smooth angle (0° / 180°).
    #[test]
    fn shade_flat_is_per_face_and_shade_smooth_averages_the_full_fan() {
        // --- Shade Flat (angle 0) on a mesh the default DOES smooth ---
        let grid = curved_grid();
        let flat = NormalConfig {
            angle_deg: 0.0,
            sharp: None,
            custom_corner: None,
        };
        assert!(flat.is_flat());
        let (vf, i_flat, _) = raster_data_ngon_with_normals(&grid, &flat);
        let (vd, i_def, _) = raster_data_ngon_with_materials(&grid);
        assert_eq!(i_flat, i_def, "Shade Flat must not change the triangulation");
        assert_ne!(bytes_of(&vf), bytes_of(&vd), "Shade Flat must differ from the smoothed default");
        // EVERY emitted corner normal must be one of the mesh's face normals — and specifically
        // the normal of the face that corner belongs to (faces are emitted contiguously).
        let face_ns: Vec<Vec3> = (0..grid.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| grid.face(f).half_edge != NO_EDGE)
            .map(|f| grid.compute_face_normal(f).normalize_or_zero())
            .collect();
        assert_eq!(vf.len(), face_ns.len() * 4, "9 quads → 4 split corners each");
        for (k, r) in vf.iter().enumerate() {
            let n = Vec3::new(r[3], r[4], r[5]).normalize_or_zero();
            assert!(
                n.dot(face_ns[k / 4]) > 0.99999,
                "Shade Flat: corner {k} must carry ITS face's normal ({n:?} vs {:?})",
                face_ns[k / 4]
            );
        }
        // The interior vertex that the default merged into ONE normal now shows all four.
        let probe = grid.vert(grid_vert(&grid, 2, 1)).position;
        let ns = normals_at(&vf, probe);
        assert!(
            ns.iter().any(|n| n.dot(ns[0]) < 0.9999),
            "Shade Flat must NOT merge across a smooth edge"
        );

        // --- Shade Smooth (angle 180) on a BOX: the 3 faces at a corner all average ---
        let box_m = HalfEdgeMesh::new_box(Vec3::ONE);
        let smooth = NormalConfig {
            angle_deg: 180.0,
            sharp: None,
            custom_corner: None,
        };
        let (vs, _, _) = raster_data_ngon_with_normals(&box_m, &smooth);
        let corner = Vec3::new(1.0, 1.0, 1.0);
        let ns = normals_at(&vs, corner);
        assert_eq!(ns.len(), 3, "a box corner has 3 face-corners");
        let want = Vec3::splat(1.0).normalize();
        for n in &ns {
            assert!(
                n.dot(want) > 0.999,
                "Shade Smooth must average the FULL fan at a box corner (got {n:?}, want {want:?})"
            );
        }
        // …and Mark Sharp still OVERRIDES Shade Smooth (Blender parity: an authored hard edge
        // survives "smooth everything"). Cut the grid's fan with the x-index-2 edge line at 180°.
        let (vs180, _, _) = raster_data_ngon_with_normals(&grid, &smooth);
        let mut sharp = BitLayer::<HalfEdgeId>::new();
        for h in grid_edge_line(&grid, 2) {
            sharp.set(h, true);
        }
        let (vss, _, _) = raster_data_ngon_with_normals(
            &grid,
            &NormalConfig {
                angle_deg: 180.0,
                sharp: Some(&sharp),
                custom_corner: None,
            },
        );
        assert_ne!(
            bytes_of(&vss),
            bytes_of(&vs180),
            "a sharp edge must break the fan even at Shade Smooth (180°)"
        );
        let split = normals_at(&vss, probe);
        let mut distinct: Vec<Vec3> = Vec::new();
        for n in &split {
            if !distinct.iter().any(|d| d.dot(*n) > 0.9999) {
                distinct.push(*n);
            }
        }
        assert_eq!(distinct.len(), 2, "Shade Smooth + Mark Sharp = split at the marked line");
    }

    /// The authored angle round-trips through `CustomData` and drives `normal_config()`.
    #[test]
    fn customdata_normal_config_carries_angle_and_sharp() {
        let mut cd = CustomData::new();
        assert_eq!(cd.auto_smooth_angle(), AUTO_SMOOTH_ANGLE_DEG, "unauthored = the default");
        assert!(!cd.has_authored_normals());
        assert!(cd.normal_config().sharp.is_none(), "an empty sharp column stays inert");

        cd.auto_smooth_deg = Some(0.0);
        assert!(cd.has_authored_normals());
        assert!(cd.normal_config().is_flat(), "0° = Shade Flat");
        cd.auto_smooth_deg = Some(180.0);
        assert!(!cd.normal_config().is_flat());
        assert!(cd.normal_config().cos_thresh() <= -0.999, "180° admits the whole fan");

        cd.sharp.set(HalfEdgeId(3), true);
        assert!(cd.normal_config().sharp.is_some());
        // BODY-domain state survives a renumbering remap (it keys on nothing).
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let out = cd.remap_by_position(&m, &m);
        assert_eq!(out.auto_smooth_deg, Some(180.0), "the angle survives a remap");
    }
}

#[cfg(test)]
mod normal_black_shading_tests {
    use super::*;
    use crate::half_edge::HalfEdgeMesh;
    use glam::Vec3;

    /// OWNER 2026-08-05: "BPR black shading even when I move the light, the black mass stays
    /// there." Screenshot: a bevelled cube whose BEVEL STRIPS are lit correctly while the
    /// original flat faces are pitch black. Same mesh, same material, same draw - so the
    /// difference has to be per-vertex, i.e. the normal. Owner's exact bevel params from the log.
    #[test]
    fn bevelled_cube_emits_no_black_normals() {
        use crate::bevel_op::{bevel_edges_offset, BevelParams};
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        // Two edges, like the log line "[BEVEL] edges=2 width=0.010 segments=3 shape=0.50".
        let mut sel: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            if cube.is_edge_live(crate::half_edge::HalfEdgeId(i)) {
                sel.insert(i);
                if sel.len() == 2 { break; }
            }
        }
        let m = bevel_edges_offset(
            &cube, &sel,
            &BevelParams { width: 0.01, segments: 3, shape: 0.5, clamp: true },
        );
        let (verts, _idx, _r) = raster_data_ngon_with_normals(&m, &NormalConfig::default());
        let mut zero = 0usize;
        let mut inward = 0usize;
        for v in &verts {
            let n = Vec3::new(v[3], v[4], v[5]);
            let p = Vec3::new(v[0], v[1], v[2]);
            if n.length() < 0.5 { zero += 1; continue; }
            // Convex box centred on origin: an outward normal agrees with its own position.
            if n.normalize().dot(p.normalize_or_zero()) <= -0.01 { inward += 1; }
        }
        eprintln!("[DIAG] verts={} zero-length-normals={zero} inward-facing={inward}", verts.len());
        assert_eq!(zero, 0, "{zero} vertices carry a ZERO normal - those shade black");
        assert_eq!(inward, 0, "{inward} vertices carry an INWARD normal - those shade black");
    }
}
