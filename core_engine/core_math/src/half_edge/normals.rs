// Normal recomputation (split from half_edge.rs — pure relocation).
//
// Two consumers share the same weighting math:
//   * `recompute_normals_weighted_ex` — writes one normal per **vertex** (`HVertex.normal`)
//   * `weighted_corner_normals` — returns one normal per **half-edge corner** for the
//     display/export bake (`raster_data_ngon_with_normals` via `NormalConfig::custom_corner`)
//
// The per-vertex field alone never reaches the screen: the n-gon emitter builds per-corner
// normals from face geometry. Corner normals are what Weighted/Smooth Normal modifiers need.

use super::*;
use std::collections::HashSet;

/// Scratch buffers reused across the per-vertex / per-corner weighted-normal loops.
///
/// Those loops allocated a `vertex_fan` `Vec` per vertex plus a `face_vertices` `Vec` per
/// incident face — and the corner bake ran the whole thing once per CORNER, so a 40k-quad body
/// paid ~700k allocations for a single normal pass. That allocation traffic, not the traversal,
/// was the cost (40k quads: 12.8 ms of a 25 ms Smooth-modifier evaluation, `bench_poly_smooth`).
/// Reusing one buffer set changes no arithmetic — see `tests/poly_smooth_parity.rs`.
#[derive(Default)]
struct NormalScratch {
    fan: Vec<HalfEdgeId>,
    ring: Vec<VertexId>,
}

/// Free-list `Vec<u32>` → dense `Vec<bool>` mask indexed by id.
///
/// `HalfEdgeMesh::free_*` are unsorted `Vec`s, so every `contains()` is a LINEAR scan; the
/// weighted passes queried liveness once per face, per corner and per half-edge, which turns
/// quadratic on a heavily-edited mesh (boolean / bevel / knife leave long free-lists). Out-of-range
/// ids read as LIVE, matching what `HashSet::contains` returned for them.
fn free_mask(free: &[u32], len: usize) -> Vec<bool> {
    let mut m = vec![false; len];
    for &i in free {
        if (i as usize) < len {
            m[i as usize] = true;
        }
    }
    m
}

#[inline]
fn is_free(mask: &[bool], i: usize) -> bool {
    mask.get(i).copied().unwrap_or(false)
}

/// Face terms at `vid` for weighted-normal accumulation: `(face_id, face_n, weight)`, appended
/// to `terms` (cleared first). Shared by the per-vertex writer, the per-corner producer and the
/// REGION pass (CLAUDE.md reuse) — one definition of the weighting arithmetic, so a region
/// recompute cannot drift from a full one.
///
/// `face_of(face_index)` yields `(newell_normal_normalized, newell_area)` for a live face and
/// `None` for a free / out-of-range one. Whole-mesh callers back it with the dense tables from
/// [`weighted_face_normals_and_areas`] (see [`dense_face_of`]); the region pass backs it with a
/// sparse map covering only the faces its one-ring touches. Same arithmetic either way.
fn weighted_face_terms_at(
    mesh: &HalfEdgeMesh,
    vid: VertexId,
    face_of: impl Fn(usize) -> Option<(Vec3, f32)>,
    mode: u8,
    w_exp: f32,
    scratch: &mut NormalScratch,
    terms: &mut Vec<(u32, Vec3, f32)>,
) {
    terms.clear();
    let NormalScratch { fan, ring } = scratch;
    mesh.vertex_fan_into(vid, fan);
    for &h in fan.iter() {
        let f = mesh.he(h).face;
        if f == NO_FACE {
            continue;
        }
        let Some((face_n, face_area)) = face_of(f.0 as usize) else {
            continue;
        };
        mesh.face_vertices_into(f, ring);
        let Some(pos) = ring.iter().position(|&v| v == vid) else {
            continue;
        };
        let n = ring.len();
        if n < 3 {
            continue;
        }
        let prev = ring[(pos + n - 1) % n];
        let next = ring[(pos + 1) % n];
        let p = mesh.vert(vid).position;
        let e0 = (mesh.vert(prev).position - p).normalize_or_zero();
        let e1 = (mesh.vert(next).position - p).normalize_or_zero();
        let corner = e0.dot(e1).clamp(-1.0, 1.0).acos().max(1e-4);
        let w = match mode {
            0 => corner,                 // Corner Angle
            1 => face_area,              // Face Area
            _ => face_area * corner,     // Face Area & Angle (default)
        };
        // `powf(1.0)` is not contractually identity for f32 — short-circuit weight==50.
        let w = if w_exp == 1.0 { w } else { w.max(0.0).powf(w_exp) };
        terms.push((f.0, face_n, w));
    }
}

/// `face_of` accessor over the dense whole-mesh tables — the whole-mesh half of
/// [`weighted_face_terms_at`]'s face lookup. Out-of-range and free faces read as `None`, which is
/// exactly the `fi >= face_count || is_free(..)` skip the dense loops used to inline.
fn dense_face_of<'a>(
    face_normals: &'a [Vec3],
    face_area: &'a [f32],
    face_free: &'a [bool],
) -> impl Fn(usize) -> Option<(Vec3, f32)> + 'a {
    move |fi| {
        if fi >= face_normals.len() || is_free(face_free, fi) {
            None
        } else {
            Some((face_normals[fi], face_area[fi]))
        }
    }
}

/// `(normalized Newell normal, Newell area weight)` for ONE face — the single definition of a
/// face's contribution to weighted normals, shared by the whole-mesh table build below and by
/// [`HalfEdgeMesh::recompute_normals_region`]. A ring shorter than 3 (degenerate face / retired
/// slot) is `(Vec3::Y, 0.0)`, matching what the dense table stored for it; the term loop skips
/// those faces regardless.
///
/// `ring` / `pts` are caller-owned scratch so a per-face loop allocates nothing.
/// The magnitude of the un-normalized Newell vector IS twice the polygon area (see
/// [`newell_normal`]) — that is where the area weight comes from, no separate area routine.
fn face_normal_and_area(
    mesh: &HalfEdgeMesh,
    fid: FaceId,
    ring: &mut Vec<VertexId>,
    pts: &mut Vec<Vec3>,
) -> (Vec3, f32) {
    mesh.face_vertices_into(fid, ring);
    if ring.len() < 3 {
        return (Vec3::Y, 0.0);
    }
    pts.clear();
    pts.extend(ring.iter().map(|&v| mesh.vert(v).position));
    let nn = newell_normal(pts);
    (nn.normalize_or_zero(), nn.length().max(1e-12))
}

/// Precompute Newell face normals + areas (skips free faces). Returns the face free-MASK for
/// callers to reuse.
fn weighted_face_normals_and_areas(mesh: &HalfEdgeMesh) -> (Vec<Vec3>, Vec<f32>, Vec<bool>) {
    let face_count = mesh.faces.len();
    let face_free = free_mask(&mesh.free_faces, face_count);
    let mut face_normals = vec![Vec3::ZERO; face_count];
    let mut face_area = vec![0.0f32; face_count];
    // Per-face and PURE: each face reads only its own ring, so this fans out with no accumulation
    // across faces and the result is bit-identical to the serial loop. `map_init` keeps the
    // `ring`/`pts` scratch reuse the serial version had — now one pair per worker instead of one
    // shared pair. A free face keeps `Vec3::ZERO`/`0.0`, which is what the serial `continue` left.
    // (Remesh at 256³ rebuilds a 74k-face half-edge every drag frame; this loop was serial.)
    {
        use rayon::prelude::*;
        face_normals
            .par_iter_mut()
            .zip(face_area.par_iter_mut())
            .enumerate()
            .for_each_init(
                || (Vec::<VertexId>::new(), Vec::<Vec3>::new()),
                |(ring, pts), (i, (fn_out, fa_out))| {
                    if face_free[i] {
                        return;
                    }
                    let (n, a) = face_normal_and_area(mesh, FaceId(i as u32), ring, pts);
                    *fn_out = n;
                    *fa_out = a;
                },
            );
    }
    (face_normals, face_area, face_free)
}

/// Average weighted terms with keep-sharp + threshold (shared kernel).
fn accumulate_weighted(
    terms: &[(u32, Vec3, f32)],
    keep_cos: f32,
    full_smooth: bool,
    threshold: f32,
) -> Vec3 {
    if terms.is_empty() {
        return Vec3::Y;
    }
    let mut best_i = 0usize;
    for (k, t) in terms.iter().enumerate().skip(1) {
        if t.2 > terms[best_i].2 {
            best_i = k;
        }
    }
    let dom_n = if threshold > 0.0 {
        let max_w = terms[best_i].2;
        let cut = max_w * (1.0 - threshold);
        let mut acc = Vec3::ZERO;
        for (_f, fnm, w) in terms {
            if *w >= cut {
                acc += *fnm * *w;
            }
        }
        let b = acc.normalize_or_zero();
        if b.length_squared() < 1e-12 {
            terms[best_i].1
        } else {
            b
        }
    } else {
        terms[best_i].1
    };
    let mut sum = Vec3::ZERO;
    for (_fid, fnm, w) in terms {
        if !full_smooth && fnm.dot(dom_n) < keep_cos {
            continue;
        }
        sum += *fnm * *w;
    }
    if sum.length_squared() < 1e-12 {
        sum = terms[best_i].1 * terms[best_i].2;
    }
    let n = sum.normalize_or_zero();
    if n.length_squared() < 1e-12 {
        Vec3::Y
    } else {
        n
    }
}

/// `true` when the undirected edge of `h` is in the Mark Sharp set (either half-edge).
#[inline]
fn he_marked_sharp(mesh: &HalfEdgeMesh, sharp: Option<&HashSet<u32>>, h: HalfEdgeId) -> bool {
    let Some(s) = sharp else {
        return false;
    };
    if s.contains(&h.0) {
        return true;
    }
    let t = mesh.he(h).twin;
    t != NO_TWIN && s.contains(&t.0)
}

/// Faces around `vid` reachable from `start_face` without crossing a Mark Sharp edge.
fn sharp_island_faces(
    mesh: &HalfEdgeMesh,
    vid: VertexId,
    start_face: FaceId,
    sharp: Option<&HashSet<u32>>,
) -> HashSet<u32> {
    let fan = mesh.vertex_fan(vid);
    // `(face, outgoing HE)` — same pairing as auto_smooth_corner_normal.
    let ring: Vec<(FaceId, HalfEdgeId)> = fan
        .iter()
        .map(|&h| (mesh.he(h).face, h))
        .filter(|&(f, _)| f != NO_FACE)
        .collect();
    if ring.is_empty() {
        return HashSet::from([start_face.0]);
    }
    let start_i = ring.iter().position(|(f, _)| *f == start_face);
    let Some(start_i) = start_i else {
        return HashSet::from([start_face.0]);
    };
    let mut out = HashSet::new();
    out.insert(start_face.0);
    // Walk both directions around the fan; stop at sharp edges or boundary.
    let n = ring.len();
    for dir in [1i32, -1i32] {
        let mut i = start_i as i32;
        for _ in 0..n {
            let cur = i as usize;
            let next_i = (i + dir).rem_euclid(n as i32) as usize;
            // Edge between ring[cur] and ring[next]: the outgoing HE of the face we leave.
            let edge_he = if dir > 0 {
                ring[cur].1
            } else {
                ring[next_i].1
            };
            if he_marked_sharp(mesh, sharp, edge_he) {
                break;
            }
            // Closed-fan wrap: stop if we return to start without a sharp (full island).
            if next_i == start_i {
                break;
            }
            out.insert(ring[next_i].0 .0);
            i = next_i as i32;
        }
    }
    out
}

/// Per-corner Weighted Normal — **indexed by `HalfEdgeId.0`** (one entry per half-edge slot).
///
/// Mirrors [`HalfEdgeMesh::recompute_normals_weighted_ex`] weighting exactly (mode / weight
/// exponent short-circuit / threshold / keep_sharp). Free / boundary half-edges get `Vec3::Y`.
///
/// `sharp_hes` — optional Mark Sharp half-edge ids. When present, faces past a sharp edge are
/// excluded from each corner's average (Blender Weighted Normal "Keep Sharp" + Mark Sharp).
/// Angle-based Auto Smooth is **not** applied here — the bake uses these as authored corners.
pub fn weighted_corner_normals(
    mesh: &HalfEdgeMesh,
    keep_sharp_deg: f32,
    mode: u8,
    weight: u32,
    threshold: f32,
    sharp_hes: Option<&HashSet<u32>>,
) -> Vec<Vec3> {
    let w_exp = (weight.clamp(1, 100) as f32) / 50.0;
    let threshold = threshold.clamp(0.0, 1.0);
    let (face_normals, face_area, face_free) = weighted_face_normals_and_areas(mesh);
    let nv = mesh.vertices.len();
    let ne = mesh.half_edges.len();
    let vert_free = free_mask(&mesh.free_verts, nv);
    let edge_free = free_mask(&mesh.free_edges, ne);
    let keep_cos = {
        let deg = keep_sharp_deg.clamp(0.0, 180.0);
        deg.to_radians().cos().clamp(-1.0, 1.0)
    };
    let full_smooth = keep_sharp_deg >= 179.5;

    // Each corner's terms depend ONLY on its vertex, but the loop below visits a vertex once per
    // incident corner — so the identical fan walk ran `valence` times per vertex (4x on a quad
    // mesh) and allocated on every one. Build the table once into a flat arena; the corner loop
    // then reads a slice. Same terms, same order, so `accumulate_weighted` sums identically.
    let mut terms_off: Vec<u32> = vec![0; nv + 1];
    let mut terms_arena: Vec<(u32, Vec3, f32)> = Vec::new();
    {
        let mut scratch = NormalScratch::default();
        let mut terms: Vec<(u32, Vec3, f32)> = Vec::new();
        for v in 0..nv {
            terms_off[v] = terms_arena.len() as u32;
            if vert_free[v] {
                continue;
            }
            weighted_face_terms_at(
                mesh,
                VertexId(v as u32),
                dense_face_of(&face_normals, &face_area, &face_free),
                mode,
                w_exp,
                &mut scratch,
                &mut terms,
            );
            terms_arena.extend_from_slice(&terms);
        }
        terms_off[nv] = terms_arena.len() as u32;
    }

    let mut out = vec![Vec3::Y; ne];
    let mut filtered: Vec<(u32, Vec3, f32)> = Vec::new();
    for i in 0..ne as u32 {
        let h = HalfEdgeId(i);
        // `is_edge_live` is a LINEAR scan of the free-list; the mask says the same thing
        // (in-range and not free — `h` is in range by construction here) without the scan.
        if edge_free[i as usize] {
            continue;
        }
        let f = mesh.he(h).face;
        if f == NO_FACE || is_free(&face_free, f.0 as usize) {
            continue;
        }
        // Corner is at the DESTINATION vertex of this half-edge (matches face_vertices order).
        let vid = mesh.he(h).vertex;
        if is_free(&vert_free, vid.0 as usize) {
            continue;
        }
        let (s, e) = (
            terms_off[vid.0 as usize] as usize,
            terms_off[vid.0 as usize + 1] as usize,
        );
        let terms: &[(u32, Vec3, f32)] = if sharp_hes.is_some() {
            let island = sharp_island_faces(mesh, vid, f, sharp_hes);
            filtered.clear();
            filtered.extend(
                terms_arena[s..e]
                    .iter()
                    .copied()
                    .filter(|(fid, _, _)| island.contains(fid)),
            );
            &filtered
        } else {
            &terms_arena[s..e]
        };
        if terms.is_empty() {
            let fi = f.0 as usize;
            out[i as usize] = if fi < face_normals.len() {
                face_normals[fi]
            } else {
                Vec3::Y
            };
            continue;
        }
        out[i as usize] = accumulate_weighted(terms, keep_cos, full_smooth, threshold);
    }
    out
}

/// Fully smooth corner normals (area × corner-angle, no keep-sharp splits).
/// Mark Sharp still islands when `sharp_hes` is provided.
pub fn smooth_corner_normals(
    mesh: &HalfEdgeMesh,
    sharp_hes: Option<&HashSet<u32>>,
) -> Vec<Vec3> {
    // mode 2 / weight 50 / threshold 0 / keep 180 — bit-exact path vs smooth vertex recompute.
    smooth_corner_normals_ex(mesh, 180.0, 2, 50, 0.0, 1.0, sharp_hes)
}

/// Smooth Normal with the same settings as Weighted Normal, plus **strength**.
///
/// * `keep_sharp_deg` / `mode` / `weight` / `threshold` — same meaning as
///   [`weighted_corner_normals`].
/// * `strength` — 0..1 blend of face-hard normal → fully weighted smooth (1 = full smooth).
///   Mark Sharp still islands when `sharp_hes` is provided.
pub fn smooth_corner_normals_ex(
    mesh: &HalfEdgeMesh,
    keep_sharp_deg: f32,
    mode: u8,
    weight: u32,
    threshold: f32,
    strength: f32,
    sharp_hes: Option<&HashSet<u32>>,
) -> Vec<Vec3> {
    let mut out = weighted_corner_normals(
        mesh,
        keep_sharp_deg,
        mode,
        weight,
        threshold,
        sharp_hes,
    );
    let s = strength.clamp(0.0, 1.0);
    if s >= 0.999 {
        return out;
    }
    // Blend each corner toward its face normal (hard shading at strength 0).
    let free_e: HashSet<u32> = mesh.free_edges.iter().copied().collect();
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if free_e.contains(&i) || !mesh.is_edge_live(h) {
            continue;
        }
        let f = mesh.he(h).face;
        if f == NO_FACE {
            continue;
        }
        let face_n = mesh.compute_face_normal(f);
        if face_n.length_squared() < 1e-12 {
            continue;
        }
        let blended = (face_n * (1.0 - s) + out[i as usize] * s).normalize_or_zero();
        out[i as usize] = if blended.length_squared() < 1e-12 {
            face_n
        } else {
            blended
        };
    }
    out
}

/// Reusable per-thread scratch for [`HalfEdgeMesh::recompute_normals_region`].
///
/// Face and vertex ids are DENSE indices, so membership and per-face data belong in arrays indexed
/// by id, not in `HashSet`/`HashMap`. The measured problem was never the maths: at 20k moved verts
/// the region pass hash-inserted ~40k face ids and ~40k vertex ids per frame, then probed a
/// `HashMap` ONCE PER CORNER inside the accumulation kernel — ~10 ms of every drag frame.
///
/// A monotonically increasing `stamp` marks "present this call", so nothing is cleared between
/// calls and a steady drag pays zero allocation and zero rehashing after the first frame. Lives in
/// a `thread_local`, never on `HalfEdgeMesh`: that struct's serialized shape is a hard red line and
/// scratch state has no business on disk.
#[derive(Default)]
struct RegionScratch {
    stamp: u32,
    /// PR-7 (ZB_RESPONSIVE_SCULPT_PLAN §7.7): cached free-slot bitmaps. Rebuilding
    /// three HashSets per call dominated small-seed strokes (measured 4.2 ms at 1.3k
    /// seeds — the sets, not the walk). Key = the five lengths below; any alloc/free
    /// changes at least one (alloc-from-free shrinks a free list, arena alloc grows an
    /// arena, free grows a free list), and topology is stable for a whole sculpt
    /// stroke, so the masks are rebuilt once per topology change instead of per stamp.
    free_key: (usize, usize, usize, usize, usize, usize),
    free_f: Vec<bool>,
    free_v: Vec<bool>,
    free_e: Vec<bool>,
    /// `== stamp` means the face is DIRTY (geometry moved; its normal is written back).
    dirty: Vec<u32>,
    /// `== stamp` means `f_normal`/`f_area` hold this call's Newell values (dirty and collar).
    has_data: Vec<u32>,
    f_normal: Vec<Vec3>,
    f_area: Vec<f32>,
    /// `== stamp` means the vertex is in the region.
    seen_vert: Vec<u32>,
    face_list: Vec<u32>,
    vert_list: Vec<u32>,
}

impl RegionScratch {
    fn begin(&mut self, n_faces: usize, n_verts: usize) {
        if self.dirty.len() < n_faces {
            self.dirty.resize(n_faces, 0);
            self.has_data.resize(n_faces, 0);
            self.f_normal.resize(n_faces, Vec3::ZERO);
            self.f_area.resize(n_faces, 0.0);
        }
        if self.seen_vert.len() < n_verts {
            self.seen_vert.resize(n_verts, 0);
        }
        // Stamp 0 means "never marked", so a wrap must reset rather than alias it.
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.dirty.iter_mut().for_each(|x| *x = 0);
            self.has_data.iter_mut().for_each(|x| *x = 0);
            self.seen_vert.iter_mut().for_each(|x| *x = 0);
            self.stamp = 1;
        }
        self.face_list.clear();
        self.vert_list.clear();
    }
}

impl HalfEdgeMesh {
    // ============================================================
    // Normal computation
    // ============================================================

    /// Compute face normal from vertex positions.
    ///
    /// Newell's method (see [`newell_normal`]): area-weighted, sign-correct for ANY non-planar /
    /// non-convex polygon. The naive first-3-vertex cross product `(p1-p0)×(p2-p0)` returns the WRONG
    /// sign whenever the p0→p1→p2 corner is reflex — which flipped the shading normal of non-convex
    /// beveled faces (e.g. the `(false,false)` re-traced faces in bevel_op.rs). Regression-gated by
    /// `bevel_op::tests::bevel_output_faces_all_point_outward`. Sign matches the old method for
    /// convex CCW faces, so existing convex meshes are unchanged.
    pub fn compute_face_normal(&self, f: FaceId) -> Vec3 {
        let (mut ring, mut pts) = (Vec::new(), Vec::new());
        self.compute_face_normal_into(f, &mut ring, &mut pts)
    }

    /// [`Self::compute_face_normal`] with caller-owned scratch — identical math, no allocation.
    /// The allocating form wraps this one, so there is ONE Newell definition. Callers that
    /// normal every face of a mesh (the display bake) reused two `Vec`s per face otherwise.
    pub fn compute_face_normal_into(
        &self,
        f: FaceId,
        ring: &mut Vec<VertexId>,
        pts: &mut Vec<Vec3>,
    ) -> Vec3 {
        self.face_vertices_into(f, ring);
        if ring.len() < 3 { return Vec3::Y; }
        pts.clear();
        pts.extend(ring.iter().map(|&v| self.vert(v).position));
        newell_normal(pts).normalize_or_zero()
    }

    /// Compute vertex normal as average of adjacent face normals.
    pub fn compute_vertex_normal(&self, v: VertexId) -> Vec3 {
        let fan = self.vertex_fan(v);
        let mut sum = Vec3::ZERO;
        for &h in &fan {
            let f = self.he(h).face;
            if f != NO_FACE {
                sum += self.face(f).normal;
            }
        }
        sum.normalize_or_zero()
    }

    /// Recompute all face normals, then all vertex normals.
    pub fn recompute_normals(&mut self) {
        self.recompute_normals_weighted(180.0);
    }

    /// Fully smooth shading: area × corner-angle weighted normals, no sharp splits.
    pub fn recompute_normals_smooth(&mut self) {
        self.recompute_normals_smooth_ex(180.0, 2, 50, 0.0, 1.0);
    }

    /// Smooth Normal with Weighted-parity settings + **strength** (0 = face-hard, 1 = full smooth).
    pub fn recompute_normals_smooth_ex(
        &mut self,
        keep_sharp_angle_deg: f32,
        mode: u8,
        weight: u32,
        threshold: f32,
        strength: f32,
    ) {
        // Capture face normals before the weighted pass overwrites vertex normals.
        let free_f: HashSet<u32> = self.free_faces.iter().copied().collect();
        let free_v: HashSet<u32> = self.free_verts.iter().copied().collect();
        let mut face_n_cache = vec![Vec3::Y; self.faces.len()];
        for i in 0..self.faces.len() {
            if free_f.contains(&(i as u32)) {
                continue;
            }
            face_n_cache[i] = self.compute_face_normal(FaceId(i as u32));
        }
        self.recompute_normals_weighted_ex(keep_sharp_angle_deg, mode, weight, threshold);
        let s = strength.clamp(0.0, 1.0);
        if s >= 0.999 {
            return;
        }
        // Per-vertex: blend face-average (hard-ish) toward the weighted smooth result.
        for i in 0..self.vertices.len() {
            if free_v.contains(&(i as u32)) {
                continue;
            }
            let vid = VertexId(i as u32);
            let fan = self.vertex_fan(vid);
            let mut hard = Vec3::ZERO;
            let mut n = 0u32;
            for &h in &fan {
                let f = self.he(h).face;
                if f == NO_FACE || free_f.contains(&f.0) {
                    continue;
                }
                let fi = f.0 as usize;
                if fi < face_n_cache.len() {
                    hard += face_n_cache[fi];
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let hard = hard.normalize_or_zero();
            if hard.length_squared() < 1e-12 {
                continue;
            }
            let smooth = self.vertices[i].normal;
            let blended = (hard * (1.0 - s) + smooth * s).normalize_or_zero();
            self.vertices[i].normal = if blended.length_squared() < 1e-12 {
                smooth
            } else {
                blended
            };
        }
    }

    /// Region-bounded normal recompute: only faces **incident to** `seed_verts`, then only
    /// vertices of those faces. O(region) — the locality-spike fix for extrude / grab / loop cut
    /// tails that previously paid full-mesh `recompute_normals` (op_bench C: extrude XL 207 ms).
    ///
    /// **CONTRACT: every vertex this writes gets exactly the normal a full
    /// [`recompute_normals`](Self::recompute_normals) would have written.** That is the only
    /// reason the eleven callers may substitute it for a full pass. It holds by construction:
    /// the per-vertex terms come from the SAME [`weighted_face_terms_at`] the full pass uses, fed
    /// the SAME [`face_normal_and_area`] Newell values, and are summed by the SAME
    /// [`accumulate_weighted`] with the settings `recompute_normals` resolves to. Only the FACE
    /// table is scoped down: instead of every face in the mesh it covers exactly the faces the
    /// region's vertices touch (dirty faces + their one-ring collar), computed on the spot.
    ///
    /// It used to weight every collar face — every fan face outside the dirty set — by a hardcoded
    /// area of `1.0` instead of that face's real Newell area, under a comment describing an area
    /// derivation that was never written. Wherever the mesh was not locally coplanar that produced
    /// a vertex normal the full pass disagrees with (measured before the fix: up to 0.76 of
    /// normal-vector length on a box corner, ~50 deg). `HVertex.normal` reaches `to_mesh` ->
    /// `export_node_obj`'s OBJ `vn` lines and is inherited by vertices created later
    /// (`split_edge` lerps it, extrude / inset copy it), so it was a wrong authored value, not a
    /// viewport artifact — the n-gon emitter builds its own per-corner normals from face geometry.
    ///
    /// Faces OUTSIDE the region keep their stored `HFace.normal` (this pass only stamps the dirty
    /// ones); the collar's Newell values are computed locally for weighting and not written back.
    pub fn recompute_normals_region(&mut self, seed_verts: impl IntoIterator<Item = VertexId>) {
        use std::collections::{HashMap, HashSet};
        // Settings `recompute_normals` resolves to: `recompute_normals_weighted(180)` ->
        // `recompute_normals_weighted_ex(180, mode 2 = area x angle, weight 50, threshold 0)`.
        // weight 50 => exponent 1 (the `w_exp == 1.0` short-circuit); 180 deg => full smooth, so
        // `keep_cos` is computed the same way but never filters.
        const MODE: u8 = 2;
        const W_EXP: f32 = 1.0;
        const THRESHOLD: f32 = 0.0;
        let keep_cos = 180.0f32.clamp(0.0, 180.0).to_radians().cos().clamp(-1.0, 1.0);
        let full_smooth = true;

        // PR-7: free-slot BITMAPS, cached on the thread-local scratch (see RegionScratch).
        // The HashSet builds this replaced were O(free) allocs on EVERY stamp.

        // A REGION THAT IS THE WHOLE MESH IS NOT A REGION. Seeding every vertex (select-all grab
        // or rotate) makes every face dirty, so the collar expansion re-walks every fan for nothing
        // and the stamp bookkeeping is pure overhead on top of a full pass. Measured on a 323k mesh:
        // 138 ms for a 161k-vertex grab versus 4.3 ms for 20k — 8x the verts, 32x the time, i.e.
        // superlinear precisely where it should be linear (owner: "rotating with the full mesh
        // selected I have 1 fps").
        //
        // `recompute_normals` is `recompute_normals_weighted(180)`, which is what this function's
        // own header documents the region pass as mirroring — same MODE/W_EXP/THRESHOLD, same
        // keep_cos — so this is the same answer by a path built for the whole-mesh case.
        let seed: Vec<VertexId> = seed_verts.into_iter().collect();
        if seed.len() * 2 >= self.vertices.len() {
            self.recompute_normals();
            return;
        }
        let seed_verts = seed;

        thread_local! {
            static SCRATCH: std::cell::RefCell<RegionScratch> =
                std::cell::RefCell::new(RegionScratch::default());
        }
        // Taken OUT of the cell for the duration: the region walk calls `&mut self` methods, and
        // holding a `RefCell` borrow across those is how a re-entrant call would panic instead of
        // simply being slow. Put back on every exit path.
        let mut sc_cell = SCRATCH.with(|c| c.replace(RegionScratch::default()));
        let sc = &mut sc_cell;
        sc.begin(self.faces.len(), self.vertices.len());
        let key = (
            self.faces.len(),
            self.vertices.len(),
            self.half_edges.len(),
            self.free_faces.len(),
            self.free_verts.len(),
            self.free_edges.len(),
        );
        if sc.free_key != key {
            sc.free_f = free_mask(&self.free_faces, self.faces.len());
            sc.free_v = free_mask(&self.free_verts, self.vertices.len());
            sc.free_e = free_mask(&self.free_edges, self.half_edges.len());
            sc.free_key = key;
        }
        let stamp = sc.stamp;
        // `vertex_fan_into` (scratch buffer), NOT `vertex_fan` (fresh Vec per call). This loop runs
        // once per SEED vertex, and a grab seeds every moved vertex: at 20k moved verts the
        // allocating variant made 20k heap allocations per frame, measured as `normals 18.5–31.4 ms`
        // against `writes 0.3 ms` in the same drag — i.e. essentially the entire `input` bucket the
        // owner reported as 50 ms. Step (3) below was already written this way; step (1) was not.
        // Same cost class as the 2026-08-08 display-bake fix (per-corner Vec allocs, 21x).
        let mut seed_fan: Vec<HalfEdgeId> = Vec::new();
        for vid in seed_verts {
            if (vid.0 as usize) >= self.vertices.len() || sc.free_v.get(vid.0 as usize).copied().unwrap_or(true) {
                continue;
            }
            self.vertex_fan_into(vid, &mut seed_fan);
            for k in 0..seed_fan.len() {
                let h = seed_fan[k];
                if sc.free_e.get(h.0 as usize).copied().unwrap_or(true) {
                    continue;
                }
                let f = self.he(h).face;
                if f != NO_FACE
                    && !sc.free_f.get(f.0 as usize).copied().unwrap_or(true)
                    && (f.0 as usize) < sc.dirty.len()
                    && sc.dirty[f.0 as usize] != stamp
                {
                    sc.dirty[f.0 as usize] = stamp;
                    sc.face_list.push(f.0);
                }
            }
        }
        if sc.face_list.is_empty() {
            SCRATCH.with(|c| c.replace(sc_cell));
            return;
        }

        // (1) Dirty faces: Newell normal + area, stamped into the mesh (their geometry moved).
        let (mut ring, mut pts) = (Vec::new(), Vec::new());
        for k in 0..sc.face_list.len() {
            let fi = sc.face_list[k];
            let (n, a) = face_normal_and_area(self, FaceId(fi), &mut ring, &mut pts);
            self.faces[fi as usize].normal = n;
            sc.has_data[fi as usize] = stamp;
            sc.f_normal[fi as usize] = n;
            sc.f_area[fi as usize] = a;
        }

        // (2) Vertex set = all corners of dirty faces (one-ring expansion for smooth shading).
        for k in 0..sc.face_list.len() {
            let fi = sc.face_list[k];
            self.face_vertices_into(FaceId(fi), &mut ring);
            for v in ring.iter() {
                if !sc.free_v.get(v.0 as usize).copied().unwrap_or(true)
                    && (v.0 as usize) < sc.seen_vert.len()
                    && sc.seen_vert[v.0 as usize] != stamp
                {
                    sc.seen_vert[v.0 as usize] = stamp;
                    sc.vert_list.push(v.0);
                }
            }
        }

        // (3) Collar: the remaining faces those vertices' fans reach. They are NOT dirty, so their
        // stored normal stands — but their real area is needed to weight them, and the Newell
        // vector carries both, so derive both here rather than trusting a possibly stale
        // `HFace.normal`. Still O(region): the collar is one face-ring past the dirty set.
        let mut fan: Vec<HalfEdgeId> = Vec::new();
        for vk in 0..sc.vert_list.len() {
            let vi = sc.vert_list[vk];
            self.vertex_fan_into(VertexId(vi), &mut fan);
            for k in 0..fan.len() {
                let f = self.he(fan[k]).face;
                if f == NO_FACE || sc.free_f.get(f.0 as usize).copied().unwrap_or(true) || (f.0 as usize) >= self.faces.len() {
                    continue;
                }
                if sc.has_data[f.0 as usize] == stamp {
                    continue;
                }
                let (n, a) = face_normal_and_area(self, f, &mut ring, &mut pts);
                sc.has_data[f.0 as usize] = stamp;
                sc.f_normal[f.0 as usize] = n;
                sc.f_area[f.0 as usize] = a;
            }
        }

        // (4) Per-vertex accumulation — the full pass's kernel, over the region's face table.
        // The face lookup is now an array index behind a stamp compare rather than a hash probe.
        // It runs once per CORNER, which is exactly why it dominated the profile.
        let mut scratch = NormalScratch::default();
        let mut terms: Vec<(u32, Vec3, f32)> = Vec::new();
        let has_data = &sc.has_data;
        let f_normal = &sc.f_normal;
        let f_area = &sc.f_area;
        let vert_list = &sc.vert_list;
        for vk in 0..vert_list.len() {
            let vi = vert_list[vk];
            weighted_face_terms_at(
                self,
                VertexId(vi),
                |fi| {
                    let i = fi as usize;
                    if i < has_data.len() && has_data[i] == stamp {
                        Some((f_normal[i], f_area[i]))
                    } else {
                        None
                    }
                },
                MODE,
                W_EXP,
                &mut scratch,
                &mut terms,
            );
            self.vertices[vi as usize].normal =
                accumulate_weighted(&terms, keep_cos, full_smooth, THRESHOLD);
        }
        SCRATCH.with(|c| c.replace(sc_cell));
    }

    /// Blender-style **Weighted Normal**:
    /// - Face normals via Newell
    /// - Vertex normal = Σ (face_n × face_area × corner_angle)
    /// - Faces whose dihedral with the vertex's strongest face exceeds
    ///   `keep_sharp_angle_deg` are excluded (sharp edge keep). Use **180** for full smooth.
    pub fn recompute_normals_weighted(&mut self, keep_sharp_angle_deg: f32) {
        // Legacy entry point: mode 2 (area x angle), weight 50 (exponent 1), threshold 0
        // (strict first-max tie-break) == the behaviour that shipped before the Blender
        // parity settings existed. Bit-exact by construction (see the `w_exp == 1.0` and
        // `threshold <= 0` early-outs in the `_ex` body).
        self.recompute_normals_weighted_ex(keep_sharp_angle_deg, 2, 50, 0.0);
    }

    /// Weighted Normal with Blender-parity settings.
    ///
    /// * `mode` — 0 = Corner Angle, 1 = Face Area, 2 = Face Area & Angle.
    /// * `weight` — Blender's 1..=100 "Weight" slider. **APPROXIMATION, not Blender-exact:**
    ///   Blender's internal bias is undocumented, so here each face weight is raised to
    ///   `weight / 50` before accumulation. 50 => exponent 1 (no bias, and short-circuited so
    ///   it is bit-exact vs. the pre-settings code), 100 => exponent 2 (the dominant face
    ///   wins harder), 1 => exponent 0.02 (weights collapse toward an even average).
    /// * `threshold` — relative tolerance for "tied with the strongest face". Faces whose
    ///   weight is >= `max * (1 - threshold)` are treated as jointly dominant and their
    ///   normals are blended to form the reference normal for the keep-sharp test, instead of
    ///   picking one arbitrarily (which flip-flops on near-equal faces). `0` = strict
    ///   first-max, i.e. the legacy tie-break.
    pub fn recompute_normals_weighted_ex(
        &mut self,
        keep_sharp_angle_deg: f32,
        mode: u8,
        weight: u32,
        threshold: f32,
    ) {
        let w_exp = (weight.clamp(1, 100) as f32) / 50.0;
        let threshold = threshold.clamp(0.0, 1.0);
        let (face_normals, face_area, face_free) = weighted_face_normals_and_areas(self);
        let vert_free = free_mask(&self.free_verts, self.vertices.len());
        // Write face normals back (Newell) — same as pre-shared-helper path.
        for i in 0..self.faces.len() {
            if face_free[i] {
                continue;
            }
            self.faces[i].normal = face_normals[i];
        }

        let keep_cos = {
            let deg = keep_sharp_angle_deg.clamp(0.0, 180.0);
            deg.to_radians().cos().clamp(-1.0, 1.0)
        };
        let full_smooth = keep_sharp_angle_deg >= 179.5;

        // Per-vertex, pure, disjoint writes — so it fans out. Computed into a side buffer first
        // because the walk needs `&self` while the write needs `&mut self`; the immutable borrow
        // ends with the block. Each vertex accumulates its OWN terms in fan order either way, so
        // the result is bit-identical to the serial loop (gated in `poly_smooth_parity.rs`).
        // A free vertex keeps its existing normal, which is what the serial `continue` did.
        let vert_count = self.vertices.len();
        let normals: Vec<Vec3> = {
            use rayon::prelude::*;
            let me: &HalfEdgeMesh = self;
            (0..vert_count)
                .into_par_iter()
                .map_init(
                    || (NormalScratch::default(), Vec::new()),
                    |(scratch, terms), i| {
                        if vert_free[i] {
                            return me.vertices[i].normal;
                        }
                        weighted_face_terms_at(
                            me,
                            VertexId(i as u32),
                            dense_face_of(&face_normals, &face_area, &face_free),
                            mode,
                            w_exp,
                            scratch,
                            terms,
                        );
                        if terms.is_empty() {
                            Vec3::Y
                        } else {
                            accumulate_weighted(terms, keep_cos, full_smooth, threshold)
                        }
                    },
                )
                .collect()
        };
        for (i, n) in normals.into_iter().enumerate() {
            self.vertices[i].normal = n;
        }
    }

}

#[cfg(test)]
mod weighted_normal_tests {
    use super::*;
    use crate::half_edge::HalfEdgeMesh;

    /// An open fan around v0 whose triangles deliberately disagree on area vs. corner angle:
    /// one big-area / small-angle sliver, one small-area / large-angle wedge, all differently
    /// tilted so the three weighting modes must land on different vertex normals.
    fn disagreeing_fan() -> HalfEdgeMesh {
        let pos = vec![
            Vec3::new(0.0, 0.0, 0.0),   // 0 — the shared vertex
            Vec3::new(1.0, 0.0, 0.0),   // 1
            Vec3::new(0.0, 1.0, 0.0),   // 2
            Vec3::new(-0.15, 3.0, 0.9), // 3 — long sliver: big area, small angle
            Vec3::new(-0.4, 0.05, 0.2), // 4 — stubby wedge: small area, large angle
        ];
        // Closed fan (v0 is interior, boundary is the 1-2-3-4 ring) — a boundary vertex
        // would give an incomplete `vertex_fan` and hide the weighting differences.
        let faces = vec![vec![0, 1, 2], vec![0, 2, 3], vec![0, 3, 4], vec![0, 4, 1]];
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    fn n0(mode: u8, weight: u32, threshold: f32) -> Vec3 {
        let mut m = disagreeing_fan();
        m.recompute_normals_weighted_ex(180.0, mode, weight, threshold);
        m.vert(VertexId(0)).normal
    }

    #[test]
    fn weighting_modes_produce_different_normals() {
        let angle = n0(0, 50, 0.0);
        let area = n0(1, 50, 0.0);
        let both = n0(2, 50, 0.0);
        for (a, b, what) in [
            (angle, area, "angle vs area"),
            (angle, both, "angle vs area&angle"),
            (area, both, "area vs area&angle"),
        ] {
            assert!(
                (a - b).length() > 1e-4,
                "{what} should differ, got {a:?} vs {b:?}"
            );
        }
    }

    /// Regression guard: mode 2 / weight 50 / threshold 0 is BIT-EXACT vs. the pre-settings
    /// kernel (which is what `recompute_normals_weighted` still calls).
    #[test]
    fn mode2_weight50_reproduces_legacy_exactly() {
        for keep_sharp in [0.0f32, 30.0, 75.0, 180.0] {
            let mut legacy = disagreeing_fan();
            legacy.recompute_normals_weighted(keep_sharp);
            let mut new = disagreeing_fan();
            new.recompute_normals_weighted_ex(keep_sharp, 2, 50, 0.0);
            for i in 0..legacy.vertices.len() {
                assert_eq!(
                    legacy.vertices[i].normal, new.vertices[i].normal,
                    "vertex {i} drifted at keep_sharp={keep_sharp}"
                );
            }
        }
    }

    #[test]
    fn weight_slider_biases_toward_the_dominant_face() {
        let even = n0(2, 1, 0.0);
        let neutral = n0(2, 50, 0.0);
        let peaked = n0(2, 100, 0.0);
        assert!((even - neutral).length() > 1e-4, "weight=1 should flatten weights");
        assert!((peaked - neutral).length() > 1e-4, "weight=100 should sharpen weights");
    }

    #[test]
    fn threshold_ties_change_the_keep_sharp_reference() {
        // A cube corner: three faces of exactly equal weight. threshold=0 picks one
        // arbitrarily; a non-zero threshold blends all three tied faces instead.
        let pos = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let faces = vec![vec![0, 1, 2], vec![0, 2, 3], vec![0, 3, 1]];
        let sample = |t: f32| {
            let mut m = HalfEdgeMesh::from_polygons(&pos, &faces);
            // keep_sharp=60 deg: the blended reference (the corner diagonal) sits 54.7 deg
            // from each of the three tied faces, so all three survive the filter; the strict
            // first-max reference keeps only its own face (the others are 70.5 deg away).
            m.recompute_normals_weighted_ex(60.0, 2, 50, t);
            m.vert(VertexId(0)).normal
        };
        assert!(
            (sample(0.0) - sample(0.01)).length() > 1e-4,
            "tie blending should change the corner normal"
        );
    }
}
