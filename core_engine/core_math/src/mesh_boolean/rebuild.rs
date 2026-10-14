//! Weld, coplanar dissolve, soup→mesh rebuild, post-boolean polish.
//!
//! Pure relocation of BSP/robust output rebuild helpers from mesh_boolean.
//! Shared by BSP path and Manifold wrap (merge_coplanar, post_boolean_polish).

use glam::Vec3;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use crate::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_EDGE, NO_FACE, NO_TWIN};
use super::{
    boundary_half_edge_count, BoolOp, COLLINEAR_DOT, COPLANAR_DIST, COPLANAR_DOT,
    POST_BOOL_COLLINEAR_DOT, POST_BOOL_DISSOLVE_DEG, SRC_A, T_JUNCTION_COLLINEAR_DOT,
    WELD_EPS, Polygon,
};

// ============================================================
// Tolerance welder (27-cell neighborhood)
// ============================================================

/// Position welder with a true distance tolerance.
///
/// `bevel_op::WeldMap` (the existing welder — found first, per the reuse rule) quantizes to
/// exact 1e-5 grid cells: two points 1e-6 apart straddling a cell border get DIFFERENT
/// indices. That is fine for bevel (coincident points are bit-identical there) but not for
/// BSP output, where matching seam points come from different interpolation paths. This
/// welder hashes to WELD_EPS-sized cells and searches the 27-cell neighborhood for an
/// existing point within WELD_EPS — genuine tolerance welding.
struct ToleranceWeld {
    positions: Vec<Vec3>,
    map: HashMap<(i64, i64, i64), Vec<usize>>,
}

impl ToleranceWeld {
    fn new() -> Self {
        Self { positions: Vec::new(), map: HashMap::new() }
    }

    fn key(p: Vec3) -> (i64, i64, i64) {
        (
            (p.x / WELD_EPS).floor() as i64,
            (p.y / WELD_EPS).floor() as i64,
            (p.z / WELD_EPS).floor() as i64,
        )
    }

    fn push(&mut self, p: Vec3) -> usize {
        let (kx, ky, kz) = Self::key(p);
        for dx in -1..=1i64 {
            for dy in -1..=1i64 {
                for dz in -1..=1i64 {
                    if let Some(bucket) = self.map.get(&(kx + dx, ky + dy, kz + dz)) {
                        for &i in bucket {
                            if self.positions[i].distance_squared(p) <= WELD_EPS * WELD_EPS {
                                return i;
                            }
                        }
                    }
                }
            }
        }
        let i = self.positions.len();
        self.positions.push(p);
        self.map.entry((kx, ky, kz)).or_default().push(i);
        i
    }
}

// ============================================================
// Coplanar dissolve (Blender's "limited dissolve" on boolean output)
// ============================================================
//
// The BSP kernel fan-triangulates every face, so its raw output is triangle soup even on
// faces the intersection never touched (the user-visible "random edges" bug). Blender keeps
// flat regions as single n-gons and only edges the true intersection curve. This pass
// reproduces that: group faces into coplanar CONNECTED regions, replace each simple region
// with its boundary loop, then dissolve straight-edge interior vertices. Euler characteristic
// is invariant (merge: −1 face −1 edge; vertex dissolve: −1 vertex −1 edge), so a watertight
// input stays watertight. A region that isn't a single simple loop (a hole/annulus, or a pinch)
// keeps its original triangles — never worse than the input.

fn uf_find(uf: &mut [usize], mut x: usize) -> usize {
    while uf[x] != x {
        uf[x] = uf[uf[x]];
        x = uf[x];
    }
    x
}

/// Newell sum of a polygon ring, in ring order: (unnormalised normal, |n| = 2·area; centroid).
/// The ONE Newell implementation of this module — soup faces and half-edge faces both feed it.
fn newell_ring(ring: impl Iterator<Item = Vec3>) -> (Vec3, Vec3) {
    let term = |a: Vec3, b: Vec3| {
        Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y))
    };
    let (mut n, mut c, mut k) = (Vec3::ZERO, Vec3::ZERO, 0usize);
    let (mut first, mut prev): (Option<Vec3>, Option<Vec3>) = (None, None);
    for p in ring {
        match prev {
            Some(q) => n += term(q, p),
            None => first = Some(p),
        }
        c += p;
        prev = Some(p);
        k += 1;
    }
    if let (Some(last), Some(first)) = (prev, first) {
        n += term(last, first);
    }
    (n, c / k.max(1) as f32)
}

/// Twice the area of a polygon face (length of its Newell normal) — how well-conditioned its
/// plane is. A CSG sliver's plane is numerical noise; a big face's plane is not.
fn face_area2(positions: &[Vec3], f: &[usize]) -> f32 {
    newell_ring(f.iter().map(|&v| positions[v])).0.length()
}

/// Newell plane (normal, offset w = n·centroid) of a polygon face.
fn face_plane(positions: &[Vec3], f: &[usize]) -> (Vec3, f32) {
    let (n, c) = newell_ring(f.iter().map(|&v| positions[v]));
    let normal = n.normalize_or_zero();
    (normal, normal.dot(c))
}

/// Trace the boundary of a coplanar region into ONE simple loop. Returns None if the region's
/// boundary is not a single simple cycle (hole → 2 loops, or a pinch → bad degrees) → caller
/// keeps the triangles.
fn trace_region_loop(dir: &BTreeSet<(usize, usize)>) -> Option<Vec<usize>> {
    let mut out_adj: BTreeMap<usize, usize> = BTreeMap::new();
    let mut in_deg: BTreeMap<usize, usize> = BTreeMap::new();
    let mut count = 0usize;
    for &(a, b) in dir {
        if dir.contains(&(b, a)) {
            continue; // internal edge (shared by two region faces)
        }
        if out_adj.insert(a, b).is_some() {
            return None; // out-degree > 1 → pinch
        }
        *in_deg.entry(b).or_default() += 1;
        count += 1;
    }
    if count < 3 || out_adj.len() != count || in_deg.values().any(|&d| d != 1) {
        return None;
    }
    let start = *out_adj.keys().next()?;
    let mut loop_v = vec![start];
    let mut cur = start;
    for _ in 0..count {
        let next = *out_adj.get(&cur)?;
        if next == start {
            return if loop_v.len() == count { Some(loop_v) } else { None };
        }
        loop_v.push(next);
        cur = next;
    }
    None
}

/// Remove valence-2 straight-through vertices from every face at once (a point in the interior
/// of a straight edge shared by exactly two regions). Iterated to a fixed point. A vertex with
/// ≥3 distinct neighbours is a genuine junction → kept, so twins never crack.
///
/// Collinearity test is dual: direction-dot OR point-to-segment distance (CSG float noise on
/// long cube edges often fails pure-dot while still lying on the segment — the cube−cyl
/// "extra T-pin verts" class).
// ============================================================
// PRESERVE SET — "a boolean must not change geometry it did not cut"
// ============================================================
//
// Owner 2026-08-04: "it is not normal that after a boolean I lose edges in a plane that
// has no change." The rebuild's merge/dissolve passes ran over the WHOLE result, so they
// happily dissolved support edges and corners that the cut never touched. Every vertex and
// edge that existed on an INPUT mesh is therefore registered here before the kernel runs,
// and the cleanup passes refuse to remove them. Geometry created BY the cut is not in the
// set, so the seam still gets cleaned — cleanup is scoped to where geometry changed.
const PRESERVE_Q: f32 = 1e-5;

fn qkey(p: Vec3) -> (i64, i64, i64) {
    (
        (p.x / PRESERVE_Q).round() as i64,
        (p.y / PRESERVE_Q).round() as i64,
        (p.z / PRESERVE_Q).round() as i64,
    )
}

// ============================================================
// ID-STABLE INSTALL (POLY-STALE-IDS, 2026-10-13)
// ============================================================

/// Renumber `derived` — a mesh a boolean made out of `operand` (its result, or a concatenation
/// of results and untouched parts) — onto `operand`'s element ids, so an id held outside the mesh
/// (poly selection, hidden set, hover, active vertex, the id-keyed sidecar) still names the same
/// geometry after the install. Plain boolean output is numbered from scratch.
///
/// Correspondence is never guessed:
/// - VERTEX: a `derived` vertex IS an `operand` vertex when their positions share one
///   `PRESERVE_Q` key — the quantized match the preserve set already trusts to decide what the
///   cut did not touch — and that key names exactly ONE live `operand` vertex. An ambiguous key
///   matches nothing.
/// - FACE: a `derived` face descends from the `operand` face that owns one of its SURVIVING
///   directed edges (both endpoints matched, that half-edge live on `operand`). Every surviving
///   edge must name the same face; a face with none, or with two candidate parents, is new.
/// - HALF-EDGE: by matched endpoints (`from_polygons_keeping_ids`).
///
/// Returns the renumbered mesh and, indexed by `derived`'s `FaceId`, the face each became
/// (`NO_FACE` for a dead or skipped slot), so a per-face array built on `derived` can follow.
pub fn keep_operand_ids(operand: &HalfEdgeMesh, derived: &HalfEdgeMesh) -> (HalfEdgeMesh, Vec<FaceId>) {
    let mut by_key: HashMap<(i64, i64, i64), Option<VertexId>> = HashMap::with_capacity(operand.vertices.len());
    for v in (0..operand.vertices.len() as u32).map(VertexId).filter(|&v| operand.is_vert_live(v)) {
        by_key
            .entry(qkey(operand.vert(v).position))
            .and_modify(|e| *e = None)
            .or_insert(Some(v));
    }
    let mut operand_edge_face: HashMap<(u32, u32), FaceId> = HashMap::with_capacity(operand.half_edges.len());
    for h in (0..operand.half_edges.len() as u32).map(HalfEdgeId).filter(|&h| operand.is_edge_live(h)) {
        let f = operand.he(h).face;
        if f != NO_FACE {
            operand_edge_face.insert((operand.he_source(h).0, operand.he(h).vertex.0), f);
        }
    }

    let mut positions: Vec<Vec3> = Vec::new();
    let mut pos_origin: Vec<Option<VertexId>> = Vec::new();
    let mut soup_index = vec![usize::MAX; derived.vertices.len()];
    let mut polys: Vec<Vec<usize>> = Vec::new();
    let mut poly_origin: Vec<Option<FaceId>> = Vec::new();
    let mut poly_mats: Vec<u32> = Vec::new();
    let mut poly_src: Vec<FaceId> = Vec::new();
    for f in (0..derived.faces.len() as u32).map(FaceId) {
        if !derived.is_face_live(f) || derived.face(f).half_edge == NO_EDGE {
            continue;
        }
        let ring: Vec<usize> = derived
            .face_vertices(f)
            .into_iter()
            .map(|v| {
                let slot = &mut soup_index[v.0 as usize];
                if *slot == usize::MAX {
                    let p = derived.vert(v).position;
                    *slot = positions.len();
                    positions.push(p);
                    pos_origin.push(by_key.get(&qkey(p)).copied().flatten());
                }
                *slot
            })
            .collect();
        let mut parent: Option<FaceId> = None;
        let mut conflict = false;
        for k in 0..ring.len() {
            let (a, b) = (pos_origin[ring[k]], pos_origin[ring[(k + 1) % ring.len()]]);
            let (Some(a), Some(b)) = (a, b) else { continue };
            if let Some(&pf) = operand_edge_face.get(&(a.0, b.0)) {
                conflict |= parent.is_some_and(|p| p != pf);
                parent = Some(pf);
            }
        }
        polys.push(ring);
        poly_origin.push(if conflict { None } else { parent });
        poly_mats.push(derived.face(f).material_id);
        poly_src.push(f);
    }
    let (out, poly_face) = HalfEdgeMesh::from_polygons_keeping_ids_mapped(
        operand,
        &positions,
        &pos_origin,
        &polys,
        &poly_origin,
        &poly_mats,
    );
    let mut face_map = vec![NO_FACE; derived.faces.len()];
    for (k, f) in poly_src.into_iter().enumerate() {
        face_map[f.0 as usize] = poly_face[k];
    }
    (out, face_map)
}

thread_local! {
    static PRESERVE_VERTS: std::cell::RefCell<std::collections::HashSet<(i64, i64, i64)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// Per-operand vertex sets: welding two verts from the SAME operand destroys authored
    /// geometry; welding one from each is the legitimate seam coincidence.
    static PRESERVE_A: std::cell::RefCell<std::collections::HashSet<(i64, i64, i64)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    static PRESERVE_B: std::cell::RefCell<std::collections::HashSet<(i64, i64, i64)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    static PRESERVE_EDGES: std::cell::RefCell<
        std::collections::HashSet<((i64, i64, i64), (i64, i64, i64))>,
    > = std::cell::RefCell::new(std::collections::HashSet::new());
    /// BOOLEAN LOCALITY (2026-08-08): input vertices lying ON the OTHER operand's surface.
    /// They are contact/seam geometry — the region a boolean is entitled to rebuild (a flush
    /// union's interface ring, the pins where one box's corner grazes the other's edge). The
    /// LOCALITY gates exempt them; every pre-existing preserve consumer ignores this set, so
    /// merge_coplanar / valence-2 / weld behaviour is unchanged. Verts strictly INSIDE the
    /// other operand never survive the kernel, so on-surface survivors are the only class
    /// that needs the exemption.
    static SEAM_VERTS: std::cell::RefCell<std::collections::HashSet<(i64, i64, i64)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Mark every live vertex of `m` lying within a scale-relative epsilon of `other`'s surface
/// (AABB-prefiltered, fan-triangulated polygon distance) into `out` as quantized keys.
/// Budget-bounded: on a pathological pair (huge mesh against huge mesh) it bails and marks
/// NOTHING — unmarked verts stay fully preserved, which fails toward keeping geometry
/// (denser output near the seam), never toward the wipe.
fn mark_contact_verts(
    m: &HalfEdgeMesh,
    other: &HalfEdgeMesh,
    out: &mut std::collections::HashSet<(i64, i64, i64)>,
) {
    let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut any = false;
    for v in &other.vertices {
        if v.half_edge == NO_EDGE {
            continue;
        }
        mn = mn.min(v.position);
        mx = mx.max(v.position);
        any = true;
    }
    if !any {
        return;
    }
    let eps = ((mx - mn).length() * 1e-5).max(1e-6);
    let eps_sq = eps * eps;
    let (lo, hi) = (mn - Vec3::splat(eps), mx + Vec3::splat(eps));
    let candidates: Vec<Vec3> = (0..m.vertices.len() as u32)
        .filter_map(|i| {
            let v = VertexId(i);
            if m.vert(v).half_edge == NO_EDGE {
                return None;
            }
            let p = m.vert(v).position;
            (p.cmpge(lo).all() && p.cmple(hi).all()).then_some(p)
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    const SEAM_CLASSIFY_BUDGET: usize = 4_000_000;
    if candidates.len().saturating_mul(other.face_count()) > SEAM_CLASSIFY_BUDGET {
        return; // fail toward preservation, never toward the wipe
    }
    // Gather other's faces as fan triangles once.
    let mut tris: Vec<[Vec3; 3]> = Vec::new();
    for i in 0..other.faces.len() as u32 {
        let fid = FaceId(i);
        if other.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs = other.face_vertices(fid);
        if vs.len() < 3 {
            continue;
        }
        let p0 = other.vert(vs[0]).position;
        for k in 1..vs.len() - 1 {
            tris.push([p0, other.vert(vs[k]).position, other.vert(vs[k + 1]).position]);
        }
    }
    for p in candidates {
        let on_surface = tris
            .iter()
            .any(|t| super::point_triangle_dist_sq(p, t[0], t[1], t[2]) <= eps_sq);
        if on_surface {
            out.insert(qkey(p));
        }
    }
}

/// Is `p` a contact/seam vertex (see `SEAM_VERTS`)?
fn vert_is_seam(p: Vec3) -> bool {
    SEAM_VERTS.with(|c| {
        let s = c.borrow();
        !s.is_empty() && s.contains(&qkey(p))
    })
}

/// Register both boolean operands' vertices and edges as untouchable for this thread.
pub(crate) fn set_preserve_from(a: &HalfEdgeMesh, b: &HalfEdgeMesh) {
    let mut vs = std::collections::HashSet::new();
    let mut es = std::collections::HashSet::new();
    let mut side_a = std::collections::HashSet::new();
    let mut side_b = std::collections::HashSet::new();
    for (si, m) in [a, b].into_iter().enumerate() {
        for i in 0..m.vertices.len() as u32 {
            let v = crate::half_edge::VertexId(i);
            if m.vert(v).half_edge == NO_EDGE {
                continue;
            }
            let k = qkey(m.vert(v).position);
            vs.insert(k);
            if si == 0 { side_a.insert(k); } else { side_b.insert(k); }
        }
        for i in 0..m.half_edges.len() as u32 {
            let he = crate::half_edge::HalfEdgeId(i);
            if !m.is_edge_live(he) {
                continue;
            }
            let (p0, p1) = (
                qkey(m.vert(m.he_source(he)).position),
                qkey(m.vert(m.he(he).vertex).position),
            );
            es.insert(if p0 <= p1 { (p0, p1) } else { (p1, p0) });
        }
    }
    PRESERVE_VERTS.with(|c| *c.borrow_mut() = vs);
    PRESERVE_EDGES.with(|c| *c.borrow_mut() = es);
    PRESERVE_A.with(|c| *c.borrow_mut() = side_a);
    PRESERVE_B.with(|c| *c.borrow_mut() = side_b);
    // BOOLEAN LOCALITY (2026-08-08): classify the contact seam — input verts ON the other
    // operand's surface. The LOCALITY gates exempt these so flush/touching contact keeps
    // its oracle-tuned cleanup (a flush union's interface ring must still dissolve to the
    // single clean box); everything else the cutter never reaches is untouchable.
    let mut seam = std::collections::HashSet::new();
    mark_contact_verts(a, b, &mut seam);
    mark_contact_verts(b, a, &mut seam);
    SEAM_VERTS.with(|c| *c.borrow_mut() = seam);
}

/// Register an EXPLICIT list of undirected edges (endpoint position pairs) as untouchable,
/// plus both endpoints of each. Same thread-local sets the boolean seeds via
/// [`set_preserve_from`], so the cleanup passes already honour it — no per-pass plumbing.
///
/// Owner 2026-08-05: "mesh clean up should never touch [tagged edges]". The caller decides
/// WHAT is protected (Mesh Cleanup passes the sidecar's authored edges); this only decides
/// HOW protection is enforced. An empty list leaves the sets empty, i.e. exactly the
/// pre-existing behaviour.
///
/// Endpoints go in too: keeping the edge but dissolving a valence-2 endpoint through it
/// would silently merge it into a longer edge, which is the same loss by another route.
pub(crate) fn set_preserve_explicit(edges: &[(Vec3, Vec3)]) {
    let mut vs = std::collections::HashSet::new();
    let mut es = std::collections::HashSet::new();
    for &(p0, p1) in edges {
        let (k0, k1) = (qkey(p0), qkey(p1));
        vs.insert(k0);
        vs.insert(k1);
        es.insert(if k0 <= k1 { (k0, k1) } else { (k1, k0) });
    }
    PRESERVE_VERTS.with(|c| *c.borrow_mut() = vs);
    PRESERVE_EDGES.with(|c| *c.borrow_mut() = es);
    PRESERVE_A.with(|c| c.borrow_mut().clear());
    PRESERVE_B.with(|c| c.borrow_mut().clear());
    SEAM_VERTS.with(|c| c.borrow_mut().clear());
}

/// Register ONLY vertex positions as untouchable (no edges, no operand sides). Used by the
/// scoped post-cut cleanup: every pre-cut vertex is preserved so cleanup can only reshape
/// geometry the cut itself created.
pub(crate) fn set_preserve_verts_only(verts: &[Vec3]) {
    let vs: std::collections::HashSet<_> = verts.iter().map(|&p| qkey(p)).collect();
    PRESERVE_VERTS.with(|c| *c.borrow_mut() = vs);
    PRESERVE_EDGES.with(|c| c.borrow_mut().clear());
    PRESERVE_A.with(|c| c.borrow_mut().clear());
    PRESERVE_B.with(|c| c.borrow_mut().clear());
    SEAM_VERTS.with(|c| c.borrow_mut().clear());
}

pub(crate) fn clear_preserve() {
    PRESERVE_VERTS.with(|c| c.borrow_mut().clear());
    PRESERVE_EDGES.with(|c| c.borrow_mut().clear());
    PRESERVE_A.with(|c| c.borrow_mut().clear());
    PRESERVE_B.with(|c| c.borrow_mut().clear());
    SEAM_VERTS.with(|c| c.borrow_mut().clear());
}

/// True when `p` and `q` are DISTINCT vertices of the SAME input mesh — welding them would
/// destroy authored geometry away from the cut (owner rule 2026-08-04).
pub(crate) fn same_operand_vertices(p: Vec3, q: Vec3) -> bool {
    let (kp, kq) = (qkey(p), qkey(q));
    if kp == kq {
        return false;
    }
    let both = |c: &std::cell::RefCell<std::collections::HashSet<(i64, i64, i64)>>| {
        let s = c.borrow();
        s.contains(&kp) && s.contains(&kq)
    };
    PRESERVE_A.with(both) || PRESERVE_B.with(both)
}

/// Is `p` a vertex that existed on a boolean INPUT? (used by the weld, see `set_preserve_from`)
pub(crate) fn is_input_vertex(p: Vec3) -> bool {
    vert_is_preserved(p)
}

/// Which INPUT operand(s) `p` belongs to: `(in_a, in_b)`. The provenance collector uses this
/// to classify each RESULT face back to the operand it came from -- the sets are already
/// per-operand (`set_preserve_from`), they simply had no consumer outside the weld. Both true
/// = a vertex the two operands share (a touching corner); both false = created by the cut.
pub(crate) fn vertex_operands(p: Vec3) -> (bool, bool) {
    let k = qkey(p);
    let has = |c: &std::cell::RefCell<std::collections::HashSet<(i64, i64, i64)>>| {
        c.borrow().contains(&k)
    };
    (PRESERVE_A.with(has), PRESERVE_B.with(has))
}

fn vert_is_preserved(p: Vec3) -> bool {
    PRESERVE_VERTS.with(|c| {
        let s = c.borrow();
        !s.is_empty() && s.contains(&qkey(p))
    })
}

/// Did this edge exist on a boolean INPUT? Used by the provenance collector to mark the
/// edges the boolean CREATED -- the flat cross-face support edges it leaves on a face it
/// cut are exactly this set, and they are what the owner needs tagged (2026-08-04).
pub(crate) fn edge_was_input(p0: Vec3, p1: Vec3) -> bool {
    edge_is_preserved(p0, p1)
}

fn edge_is_preserved(p0: Vec3, p1: Vec3) -> bool {
    PRESERVE_EDGES.with(|c| {
        let s = c.borrow();
        if s.is_empty() {
            return false;
        }
        let (a, b) = (qkey(p0), qkey(p1));
        s.contains(&if a <= b { (a, b) } else { (b, a) })
    })
}

/// Number of DISTINCT preserved input positions (armed via [`set_preserve_from`] /
/// [`set_preserve_explicit`]) still occupied by a live vertex of `m`. 0 when no preserve
/// set is armed — callers comparing before/after then see 0 >= 0 and change nothing.
///
/// LOCALITY GATE (2026-08-08, owner "the knife subtract must not delete an applied slice"):
/// every polish acceptance compares this count before/after so that NO cleanup pass — present
/// or future — can ship a mesh that deleted a vertex the boolean was told not to touch.
/// Distinct-position count (not raw vert count) on purpose: a weld that collapses coincident
/// duplicates onto one survivor at the preserved position is not a loss. Contact/seam verts
/// (`SEAM_VERTS`) are exempt — a flush union's interface ring must still dissolve away.
pub(crate) fn preserved_vert_count(m: &HalfEdgeMesh) -> usize {
    PRESERVE_VERTS.with(|c| {
        let s = c.borrow();
        if s.is_empty() {
            return 0;
        }
        let mut seen: std::collections::HashSet<(i64, i64, i64)> =
            std::collections::HashSet::new();
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if m.vert(v).half_edge == NO_EDGE {
                continue;
            }
            let p = m.vert(v).position;
            let k = qkey(p);
            if s.contains(&k) && !vert_is_seam(p) {
                seen.insert(k);
            }
        }
        seen.len()
    })
}

/// Every live half-edge (both directions) that a limited dissolve at `angle_deg` WOULD select
/// but whose two faces do not lie on one plane ([`faces_share_plane`] at `eps`) — a facet edge
/// of a modelled curve. Passed to `limited_dissolve` as protection so the batch dissolve can
/// only merge true flats (see `faces_share_plane` for the owner case this closes).
fn bent_half_edges_of(m: &HalfEdgeMesh, angle_deg: f32, eps: f32) -> std::collections::HashSet<HalfEdgeId> {
    let cos_min = angle_deg.clamp(0.0, 180.0).to_radians().cos();
    let mut out = std::collections::HashSet::new();
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !m.is_edge_live(he) {
            continue;
        }
        let tw = m.he(he).twin;
        if tw == NO_TWIN {
            continue;
        }
        let (fa, fb) = (m.he(he).face, m.he(tw).face);
        if fa == NO_FACE || fb == NO_FACE || fa == fb {
            continue;
        }
        // Same candidate rule as `limited_dissolve`'s scan: only edges it could select need
        // protecting (protection is by face exclusion — never widen it past the candidates).
        let n1 = m.compute_face_normal(fa).normalize_or_zero();
        let n2 = m.compute_face_normal(fb).normalize_or_zero();
        if n1.length_squared() < 0.25 || n2.length_squared() < 0.25 || n1.dot(n2) < cos_min {
            continue;
        }
        if !faces_share_plane(m, &[fa, fb], eps) {
            out.insert(he);
        }
    }
    out
}

/// Resolve the armed thread-local PRESERVE_EDGES set to the live half-edges of `m` — the
/// same by-position re-resolution `mod.rs::protected_half_edges` does for the explicit
/// cleanup list (ids die on every rebuild; positions survive). Empty when nothing is armed.
///
/// Scoped to the edges actually AT RISK from a limited dissolve at `angle_deg`:
/// * only FLAT preserved edges (face normals within the angle) — a 90° input edge is never
///   a dissolve candidate, and protecting its faces would only block legal seam cleanup
///   around it (`limited_dissolve` protection is by face exclusion);
/// * a preserved edge whose BOTH endpoints are contact/seam verts is the flush-contact
///   interface itself — the boolean owns it, so it stays dissolvable.
fn preserved_half_edges_of(m: &HalfEdgeMesh, angle_deg: f32) -> std::collections::HashSet<HalfEdgeId> {
    let mut out = std::collections::HashSet::new();
    if PRESERVE_EDGES.with(|c| c.borrow().is_empty()) {
        return out;
    }
    let cos_min = angle_deg.clamp(0.0, 180.0).to_radians().cos();
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !m.is_edge_live(he) {
            continue;
        }
        let tw = m.he(he).twin;
        if tw == NO_TWIN {
            continue;
        }
        let p0 = m.vert(m.he_source(he)).position;
        let p1 = m.vert(m.he(he).vertex).position;
        if !edge_is_preserved(p0, p1) {
            continue;
        }
        if vert_is_seam(p0) && vert_is_seam(p1) {
            continue;
        }
        let (fa, fb) = (m.he(he).face, m.he(tw).face);
        if fa == NO_FACE || fb == NO_FACE || fa == fb {
            continue;
        }
        // Same normal computation as `limited_dissolve`'s candidate scan, on the same mesh:
        // an edge is protected exactly when it could be selected.
        let n1 = m.compute_face_normal(fa).normalize_or_zero();
        let n2 = m.compute_face_normal(fb).normalize_or_zero();
        if n1.length_squared() < 0.25 || n2.length_squared() < 0.25 {
            continue;
        }
        if n1.dot(n2) >= cos_min {
            out.insert(he);
        }
    }
    out
}

fn dissolve_collinear(positions: &[Vec3], faces: &mut Vec<Vec<usize>>) {
    // SCALE-RELATIVE, TIGHT (owner 2026-08-04: "the cleanup is too strong, it collapses
    // supportive edges"). This used to be an ABSOLUTE 1e-2 — one percent of a unit cube —
    // so any corner sitting within 1% of a straight line was deleted, destroying support
    // edges that hold the shape. A genuine CSG collinear pin is on the line to float
    // noise, not 1%. Tie the tolerance to the model's size instead.
    let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in positions {
        mn = mn.min(*p);
        mx = mx.max(*p);
    }
    let diag = (mx - mn).length().max(1e-6);
    let seg_thr = diag * 1e-4;
    let on_segment = |v: usize, p: usize, q: usize| -> bool {
        let pv = positions[v];
        let a = positions[p];
        let b = positions[q];
        let ab = b - a;
        let len_sq = ab.length_squared();
        if len_sq < 1e-20 {
            return false;
        }
        let t = (pv - a).dot(ab) / len_sq;
        if t <= 1e-4 || t >= 1.0 - 1e-4 {
            return false; // endpoint — not an interior pin
        }
        let proj = a + ab * t;
        // Loose relative to feature size: CSG plane cuts leave pins slightly off the
        // geometric edge (float). 1e-2 catches cube−cyl side-wall pins on unit cubes.
        (proj - pv).length_squared() <= seg_thr * seg_thr
    };
    loop {
        let mut nbr: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        for f in faces.iter() {
            let m = f.len();
            for i in 0..m {
                let (a, b) = (f[i], f[(i + 1) % m]);
                nbr.entry(a).or_default().insert(b);
                nbr.entry(b).or_default().insert(a);
            }
        }
        let mut victim: Option<usize> = None;
        for (&v, ns) in &nbr {
            if ns.len() != 2 {
                continue;
            }
            let mut it = ns.iter();
            let (p, q) = (*it.next().unwrap(), *it.next().unwrap());
            let d1 = (positions[v] - positions[p]).normalize_or_zero();
            let d2 = (positions[q] - positions[v]).normalize_or_zero();
            let colin_dot = d1.dot(d2) > COLLINEAR_DOT;
            if vert_is_preserved(positions[v]) {
                continue; // input vertex — the cut did not create it, do not remove it
            }
            let colin_seg = on_segment(v, p, q);
            if colin_dot || colin_seg {
                victim = Some(v);
                break;
            }
        }
        let v = match victim {
            Some(v) => v,
            None => break,
        };
        for f in faces.iter_mut() {
            if let Some(pos) = f.iter().position(|&x| x == v) {
                f.remove(pos);
            }
        }
        faces.retain(|f| f.len() >= 3);
    }
}

/// Greedily dissolve coplanar INTERNAL edges within one coplanar region by merging any two faces that
/// share an edge into a single simple loop, iterating to a fixed point. Subsumes `trace_region_loop`: a
/// simple region collapses to one n-gon; a NON-simple region (an annulus — a face with a through-hole,
/// e.g. a cube wall a cylinder punched) merges as far as it can into a few big faces instead of staying
/// raw triangles. That matters because those leftover triangles' spurious plane-cuts pin T-junction
/// vertices onto ADJACENT flat walls (the cylinder-through +X-wall 20-gon); dissolving them frees those
/// vertices so `dissolve_collinear` can clean the neighbour back to a quad.
/// [`greedy_merge_region`] that refuses to dissolve edges present on a boolean INPUT mesh
/// (see the PRESERVE SET note) — untouched geometry keeps its edges.
fn greedy_merge_region_preserving(region: Vec<Vec<usize>>, positions: &[Vec3]) -> Vec<Vec<usize>> {
    greedy_merge_region_impl(region, Some(positions))
}

fn greedy_merge_region(region: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    greedy_merge_region_impl(region, None)
}

fn greedy_merge_region_impl(mut region: Vec<Vec<usize>>, positions: Option<&[Vec3]>) -> Vec<Vec<usize>> {
    // SUPPORT-EDGE PLACEMENT (owner test_014, 2026-08-05): on an annulus (a face with a
    // through-hole) this merge cannot reach one face — the internal edges left at the fixed
    // point ARE the flat "support edges" the boolean ships. Which ones survive used to be an
    // accident of BTreeMap (vertex-index) dissolve order, which routinely left a bridge
    // terminating on a mid-chord T-pin vertex (valence 3 → `dissolve_collinear` cannot remove
    // the pin → "random vertex on the circle segment") at a steep arbitrary angle. Steer the
    // dissolve ORDER instead of relocating afterwards (radial re-routing is REFUTED — BEVEL.md
    // §6: it always shortens the bridge and tightens the bevel clamp):
    //   1. dissolve first any internal edge pinning a collinear, non-preserved endpoint —
    //      survivors then terminate on existing junction vertices and the freed pin is
    //      dissolved by `dissolve_collinear`;
    //   2. among the rest, dissolve the WORST-ANGLED edges first, so the survivor is the
    //      bridge that meets its two junctions most squarely. Owner, decisively: "the length
    //      of segment is not that important, ANGLE is the most important." Two length-based
    //      rules were tried and rejected on sight — preferring LONG survivors routes the
    //      bridge diagonally to a far corner of the face; length is simply not the
    //      criterion. His hand-built set `test_013.fl4` has spans from 0.17 to 0.81, i.e.
    //      length varies freely while every endpoint is a clean junction.
    //   3. tie-break on the (a, b) index pair — deterministic, no hash order.
    // Scale-relative on-segment tolerance for the pin test (same class as `dissolve_collinear`).
    let seg_thr = positions.map(|pos| {
        let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for f in &region {
            for &v in f {
                mn = mn.min(pos[v]);
                mx = mx.max(pos[v]);
            }
        }
        (mx - mn).length().max(1e-6) * 1e-4
    });
    // Scratch buffers, reused across outer iterations. Both structures below are FLAT SORTED
    // VECS rather than BTreeMap/BTreeSet: same ascending-key iteration order (the module's
    // determinism contract — see the DETERMINISM banner in mod.rs), no per-entry node
    // allocation. They are still re-derived every iteration because a merge rewrites the
    // region, and the dissolve ORDER this function computes must keep seeing the CURRENT
    // topology (order is oracle-tuned; see above).
    let mut edge_faces: Vec<((usize, usize), usize)> = Vec::new();
    let mut adj: Vec<(usize, usize)> = Vec::new();
    let mut cands: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let mut keyed: Vec<(u8, f32, ((usize, usize), (usize, usize)))> = Vec::new();
    // SORT-KEY CACHE. A candidate's key (pin count, angle quality) is a pure function of the
    // positions, `seg_thr`, the preserve sets — all fixed for the whole call — and THE
    // ADJACENCY OF ITS TWO ENDPOINTS, nothing else (`pins` and `min_angle` below read
    // `nbr_of(a)` / `nbr_of(b)` and positions, and no other region state).
    //
    // A merge replaces rings i and j by `trace_region_loop`'s traced union, whose edges are
    // exactly the directed edges of i/j whose REVERSE is absent; every other edge of i and j
    // is dropped, and no edge is created. So the region's undirected edge set can only
    // shrink, and only at those dropped edges — the adjacency of every vertex that is not an
    // endpoint of a dropped edge is bit-for-bit what it was. `dirty` collects those endpoints
    // (a superset: the edge may still be used by a third face, in which case we merely
    // recompute an unchanged key), so a cache hit is provably the value a recompute would
    // produce. Kept sorted by edge and merge-joined against the (also edge-sorted) candidate
    // list: no hashing, and no map iteration order can reach the output.
    let mut key_cache: Vec<((usize, usize), (u8, f32))> = Vec::new();
    let mut next_cache: Vec<((usize, usize), (u8, f32))> = Vec::new();
    let mut dirty: Vec<usize> = Vec::new();
    loop {
        // Undirected edge → the region faces that use it. An edge shared by exactly two faces is internal.
        edge_faces.clear();
        for (fi, f) in region.iter().enumerate() {
            let n = f.len();
            for i in 0..n {
                let (a, b) = (f[i], f[(i + 1) % n]);
                edge_faces.push((if a < b { (a, b) } else { (b, a) }, fi));
            }
        }
        // Sorted by (edge, face) — equal-edge runs list their faces in ascending face index,
        // exactly the push order the per-edge Vec used to hold.
        edge_faces.sort_unstable();
        // Candidate internal edges, in dissolve-priority order.
        cands.clear();
        adj.clear();
        let want_adj = seg_thr.is_some();
        let mut s = 0usize;
        while s < edge_faces.len() {
            let (ea, eb) = edge_faces[s].0;
            let mut e = s + 1;
            while e < edge_faces.len() && edge_faces[e].0 == (ea, eb) {
                e += 1;
            }
            let fis = &edge_faces[s..e];
            s = e;
            if want_adj {
                // Vertex adjacency over the CURRENT region (boundary + internal edges), fed
                // from the UNIQUE edge keys — the same set as re-walking every face ring,
                // with each shared edge contributed once instead of once per incident face.
                adj.push((ea, eb));
                adj.push((eb, ea));
            }
            if fis.len() != 2 || fis[0].1 == fis[1].1 { continue; }
            if let Some(pos) = positions {
                if edge_is_preserved(pos[ea], pos[eb]) { continue; }
            }
            cands.push(((ea, eb), (fis[0].1, fis[1].1)));
        }
        if let (Some(pos), Some(thr)) = (positions, seg_thr) {
            adj.sort_unstable();
            adj.dedup(); // only self-edges (a, a) can duplicate once keys are unique
            // Neighbours of `v`, ascending and unique — the same sequence the per-vertex
            // BTreeSet yielded. An empty slice is the old `nbr.get(&v) == None`.
            let nbr_of = |v: usize| -> &[(usize, usize)] {
                let lo = adj.partition_point(|&(x, _)| x < v);
                let hi = adj.partition_point(|&(x, _)| x <= v);
                &adj[lo..hi]
            };
            // Endpoint `v` of a surviving edge is a PIN when, with the edge gone, it would be
            // a degree-2 collinear vertex — i.e. this edge is the only thing keeping a
            // mid-segment T-junction vertex alive on the rim/outer boundary.
            let pins = |ns: &[(usize, usize)], v: usize, opp: usize| -> bool {
                if vert_is_preserved(pos[v]) {
                    return false; // input vertex — a legitimate junction, never a pin
                }
                if ns.is_empty() {
                    return false;
                }
                // Exactly two neighbours other than `opp` (allocation-free — this runs once
                // per candidate endpoint on every outer iteration).
                let mut it = ns.iter().map(|&(_, u)| u).filter(|&x| x != opp);
                let (Some(p), Some(q), None) = (it.next(), it.next(), it.next()) else {
                    return false;
                };
                let d1 = (pos[v] - pos[p]).normalize_or_zero();
                let d2 = (pos[q] - pos[v]).normalize_or_zero();
                if d1.dot(d2) > COLLINEAR_DOT {
                    return true;
                }
                // On-segment fallback (float-noisy T-pins on long straight edges).
                let (a, b) = (pos[p], pos[q]);
                let ab = b - a;
                let len_sq = ab.length_squared();
                if len_sq < 1e-20 {
                    return false;
                }
                let t = (pos[v] - a).dot(ab) / len_sq;
                t > 1e-4 && t < 1.0 - 1e-4 && (pos[v] - (a + ab * t)).length_squared() <= thr * thr
            };
            // ANGLE QUALITY of a surviving bridge (owner 2026-08-05: "the length of segment
            // is not that important, ANGLE is the most important"). Score = the SMALLEST
            // angle the bridge makes with any region edge at either endpoint. A bridge that
            // leaves the rim almost tangentially, or slices into a corner, has a tiny min
            // angle and reads as the "wrong angle" defect; one that meets its junctions
            // squarely scores high. Sorting worst-first means the WELL-ANGLED bridges are
            // the ones left standing.
            let min_angle = |ns: &[(usize, usize)], v: usize, opp: usize| -> f32 {
                if ns.is_empty() {
                    return std::f32::consts::PI;
                }
                let d = (pos[opp] - pos[v]).normalize_or_zero();
                if d == Vec3::ZERO {
                    return 0.0;
                }
                let mut best = std::f32::consts::PI;
                for &(_, o) in ns.iter() {
                    if o == opp {
                        continue;
                    }
                    let e = (pos[o] - pos[v]).normalize_or_zero();
                    if e == Vec3::ZERO {
                        continue;
                    }
                    let ang = d.dot(e).clamp(-1.0, 1.0).acos();
                    if ang < best {
                        best = ang;
                    }
                }
                best
            };
            // PERF: the sort key is computed ONCE per candidate, not inside the comparator.
            // `pins` and the angle quality do adjacency lookups, filtered scans and vector
            // normalisations (an acos per incident edge); evaluating them per COMPARISON ran
            // them O(C log C) times for an O(C) amount of information. The key tuple and the
            // comparison below — pins DESC, then quality ASC by `total_cmp` (NaN semantics
            // included), then the (a, b) index tie-break — reproduce the previous ordering
            // exactly, and the (a, b) keys are unique (one candidate per undirected edge) so
            // the order is total and does not rely on sort stability.
            keyed.clear();
            next_cache.clear();
            let mut cc = 0usize; // merge-join cursor into key_cache (both edge-sorted)
            for &c in cands.iter() {
                let (a, b) = c.0;
                while cc < key_cache.len() && key_cache[cc].0 < (a, b) {
                    cc += 1;
                }
                let hit = cc < key_cache.len()
                    && key_cache[cc].0 == (a, b)
                    && dirty.binary_search(&a).is_err()
                    && dirty.binary_search(&b).is_err();
                let k = if hit {
                    key_cache[cc].1
                } else {
                    let (na, nb) = (nbr_of(a), nbr_of(b));
                    let pin = pins(na, a, b) as u8 + pins(nb, b, a) as u8;
                    // Quality = the smallest angle the bridge makes at either endpoint.
                    (pin, min_angle(na, a, b).min(min_angle(nb, b, a)))
                };
                next_cache.push(((a, b), k));
                keyed.push((k.0, k.1, c));
            }
            std::mem::swap(&mut key_cache, &mut next_cache);
            keyed.sort_by(|x, y| {
                y.0.cmp(&x.0) // more pinned endpoints → dissolve FIRST
                    // WORSE angle → dissolve first ⇒ well-angled survivors.
                    .then_with(|| x.1.total_cmp(&y.1))
                    .then_with(|| x.2 .0.cmp(&y.2 .0))
            });
            cands.clear();
            cands.extend(keyed.iter().map(|k| k.2));
        }
        let mut merged = false;
        for &(_, (i, j)) in &cands {
            // Union both boundaries' directed edges; trace_region_loop drops the shared (reverse-present)
            // edge as internal and returns the merged boundary iff it is a single simple loop.
            let mut dir: BTreeSet<(usize, usize)> = BTreeSet::new();
            for f in [&region[i], &region[j]] {
                let n = f.len();
                for k in 0..n { dir.insert((f[k], f[(k + 1) % n])); }
            }
            if let Some(loop_v) = trace_region_loop(&dir) {
                // Endpoints of the edges this merge DROPS (those present in both directions —
                // exactly the ones `trace_region_loop` excludes from the traced ring). Their
                // adjacency changes, so their cached sort keys must be recomputed next
                // iteration; every other cached key stays valid. See the cache note above.
                dirty.clear();
                for &(u, v) in dir.iter() {
                    if dir.contains(&(v, u)) {
                        dirty.push(u);
                        dirty.push(v);
                    }
                }
                dirty.sort_unstable();
                dirty.dedup();
                let (lo, hi) = if i < j { (i, j) } else { (j, i) };
                region.remove(hi);
                region.remove(lo);
                region.push(loop_v);
                merged = true;
                break;
            }
        }
        if !merged { break; }
    }
    region
}

/// FOLD FLATTENER (owner subtract.fl4 / extrude.fl4 2026-08-03): a near-flush contact can
/// leave a coplanar cluster tessellated as a SIGNED decomposition — one face inverted,
/// overlapping its neighbours as a zero-thickness flap. Volume and edge-pairing stay
/// consistent, so every manifold gate passes it, but it renders as broken diagonals.
/// Repair per folded cluster: orient every face along the cluster's reference normal,
/// collect directed boundary edges, cancel opposite pairs (the fold cancels itself), and
/// retrace the NET outline into one simple loop. Clusters whose net boundary is not a
/// single simple loop (hole / pinch) are left untouched.
pub(crate) fn flatten_folded_clusters(positions: &[Vec3], faces: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let nf = faces.len();
    if nf == 0 {
        return Vec::new();
    }
    let planes: Vec<(Vec3, f32)> = faces.iter().map(|f| face_plane(positions, f)).collect();
    // Union coplanar faces sharing an edge, IGNORING orientation (|dot| — folds must land
    // in the same cluster as the faces they overlap).
    let mut edge_faces: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
    for (fi, f) in faces.iter().enumerate() {
        let m = f.len();
        for i in 0..m {
            let (a, b) = (f[i], f[(i + 1) % m]);
            edge_faces.entry(if a < b { (a, b) } else { (b, a) }).or_default().push(fi);
        }
    }
    let mut uf: Vec<usize> = (0..nf).collect();
    for fs in edge_faces.values() {
        if fs.len() != 2 {
            continue;
        }
        let (i, j) = (fs[0], fs[1]);
        let (ri, rj) = (uf_find(&mut uf, i), uf_find(&mut uf, j));
        if ri == rj {
            continue;
        }
        let (nri, wri) = planes[ri];
        let nj = planes[j].0;
        if nri.dot(nj).abs() > COPLANAR_DOT
            && faces[j]
                .iter()
                .all(|&v| (nri.dot(positions[v]) - wri).abs() < COPLANAR_DIST)
        {
            uf[rj] = ri;
        }
    }
    let mut regions: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for fi in 0..nf {
        let r = uf_find(&mut uf, fi);
        regions.entry(r).or_default().push(fi);
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (&root, members) in &regions {
        // Folded iff the cluster mixes orientations vs its root plane.
        let nref = planes[root].0;
        let has_fold = members.len() > 1 && members.iter().any(|&fi| planes[fi].0.dot(nref) < 0.0);
        if !has_fold {
            for &fi in members {
                out.push(faces[fi].clone());
            }
            continue;
        }
        // Cluster vertex set — needed to split T-junction edges (a long edge in one face
        // spanning two shorter edges of another never cancels on exact endpoint pairs).
        let mut cverts: BTreeSet<usize> = BTreeSet::new();
        for &fi in members {
            cverts.extend(faces[fi].iter().copied());
        }
        // Subdivide (a,b) at every cluster vertex lying strictly inside the segment.
        let split_edge = |a: usize, b: usize| -> Vec<(usize, usize)> {
            let (pa, pb) = (positions[a], positions[b]);
            let ab = pb - pa;
            let len2 = ab.length_squared();
            if len2 < 1e-20 {
                return vec![(a, b)];
            }
            let mut mids: Vec<(f32, usize)> = Vec::new();
            for &v in &cverts {
                if v == a || v == b {
                    continue;
                }
                let t = (positions[v] - pa).dot(ab) / len2;
                if t <= 1e-5 || t >= 1.0 - 1e-5 {
                    continue;
                }
                let d = (positions[v] - (pa + ab * t)).length_squared();
                if d < 1e-10 {
                    mids.push((t, v));
                }
            }
            mids.sort_by(|x, y| x.0.total_cmp(&y.0));
            let mut chain = Vec::with_capacity(mids.len() + 1);
            let mut prev = a;
            for (_, v) in mids {
                chain.push((prev, v));
                prev = v;
            }
            chain.push((prev, b));
            chain
        };
        // Directed boundary edges with fold-cancellation: multiset counts so an edge used
        // by an inverted face erases the same edge from an upright face. Windings stay AS
        // AUTHORED — the inverted flap's reversed edges are exactly what cancels the
        // over-covered region in the signed sum.
        let mut dir_count: BTreeMap<(usize, usize), i64> = BTreeMap::new();
        for &fi in members {
            let f = &faces[fi];
            let m = f.len();
            for i in 0..m {
                for (a, b) in split_edge(f[i], f[(i + 1) % m]) {
                    *dir_count.entry((a, b)).or_default() += 1;
                }
            }
        }
        // Net = forward count minus reverse count; keep edges with net +1 exactly.
        let mut net: BTreeSet<(usize, usize)> = BTreeSet::new();
        let mut bad = false;
        let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
        let keys: Vec<(usize, usize)> = dir_count.keys().copied().collect();
        for &(ka, kb) in &keys {
            let (a, b) = if ka < kb { (ka, kb) } else { (kb, ka) };
            if !seen.insert((a, b)) {
                continue; // each undirected edge once
            }
            let fwd = *dir_count.get(&(a, b)).unwrap_or(&0);
            let rev = *dir_count.get(&(b, a)).unwrap_or(&0);
            match fwd - rev {
                0 => {}
                1 => {
                    net.insert((a, b));
                }
                -1 => {
                    net.insert((b, a));
                }
                _ => {
                    bad = true; // winding number > 1 somewhere — not a simple fold
                    break;
                }
            }
        }
        let traced = trace_region_loop(&net);
        if std::env::var("AZRA_BOOL_DEBUG").is_ok() {
            eprintln!("[FLATTEN] cluster {} faces, bad={} net_edges={} traced={:?}",
                members.len(), bad, net.len(), traced.as_ref().map(|l| l.len()));
            for &fi in members {
                eprintln!("[FLATTEN]   face{} dot={:.2} ring={:?}", fi, planes[fi].0.dot(nref),
                    faces[fi].iter().map(|&v| format!("{:?}", positions[v])).collect::<Vec<_>>());
            }
            eprintln!("[FLATTEN]   net={:?}", net.iter().map(|&(a,b)| (format!("{:?}",positions[a]),format!("{:?}",positions[b]))).collect::<Vec<_>>());
        }
        if !bad && net.is_empty() {
            // The whole cluster cancels out (a zero-thickness flap and its mirror):
            // it bounds no area — emit nothing.
            continue;
        }
        match (bad, traced) {
            (false, Some(loop_v)) if loop_v.len() >= 3 => out.push(loop_v),
            _ => {
                // Cannot flatten — keep the cluster as-is (no worse than before).
                for &fi in members {
                    out.push(faces[fi].clone());
                }
            }
        }
    }
    out
}

/// Merge coplanar connected regions into n-gons + dissolve straight-through vertices.
/// Merge coplanar adjacent faces into n-gons (Blender limited-dissolve class).
/// Shared by BSP rebuild and Manifold wrap so flat oracle cases get 0 residual tris.
pub(crate) fn merge_coplanar(positions: &[Vec3], faces: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let nf = faces.len();
    if nf == 0 {
        return Vec::new();
    }
    let planes: Vec<(Vec3, f32)> = faces.iter().map(|f| face_plane(positions, f)).collect();
    let area2: Vec<f32> = faces.iter().map(|f| face_area2(positions, f)).collect();
    let eps = {
        let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for f in faces {
            for &v in f {
                mn = mn.min(positions[v]);
                mx = mx.max(positions[v]);
            }
        }
        surface_eps_of_diag((mx - mn).length())
    };

    // Undirected edge → faces sharing it. (BTreeMap: deterministic union order → reproducible regions.)
    let mut edge_faces: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
    for (fi, f) in faces.iter().enumerate() {
        let m = f.len();
        for i in 0..m {
            let (a, b) = (f[i], f[(i + 1) % m]);
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(fi);
        }
    }

    // Union coplanar faces that share an edge.
    let mut uf: Vec<usize> = (0..nf).collect();
    for (&(ea, eb), fs) in edge_faces.iter() {
        if fs.len() != 2 {
            continue; // non-manifold edge — never merge across it
        }
        if edge_is_preserved(positions[ea], positions[eb]) {
            continue; // input edge — the cut did not create it, keep it (support edge)
        }
        let (i, j) = (fs[0], fs[1]);
        let (ri, rj) = (uf_find(&mut uf, i), uf_find(&mut uf, j));
        if ri == rj {
            continue;
        }
        // Test the candidate against its would-be REGION's reference plane (the root
        // face's plane) — NOT just its pairwise neighbour. Pairwise chaining let a
        // smooth fillet chain-merge into one curved "coplanar" region: every 2.6°
        // step passed while the whole arc was wildly non-planar, and tessellating
        // that ring produced phantom walls (owner 2026-08-03: knife subtract breaks
        // the filleted groove far from the cut).
        //
        // And the test is DISTANCE at float precision (`surface_eps`), not a 2.56° normal
        // angle + 2.5e-3 slab (POLY-BOOL-NGON-BEVEL, owner `bollean.fl4` 2026-10-13): a
        // bevel step under 2.56° (the owner's cutter has 1.4° strips) was merged into the
        // flat it meets, bending the result. The reference is the LARGER region root (a
        // sliver's own plane is noise); the candidate pair AND the smaller root must sit on
        // it, so each region stays within ~2·eps of one plane.
        let (big, small) = if area2[ri] >= area2[rj] { (ri, rj) } else { (rj, ri) };
        let (nb, wb) = planes[big];
        let on_plane = |f: &Vec<usize>| f.iter().all(|&v| (nb.dot(positions[v]) - wb).abs() <= eps);
        if nb != Vec3::ZERO && on_plane(&faces[i]) && on_plane(&faces[j]) && on_plane(&faces[small]) {
            uf[small] = big; // absorb the smaller region; the larger root's plane stays the reference
        }
    }

    // Group faces by region root. (BTreeMap: deterministic region iteration → reproducible output order.)
    let mut regions: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for fi in 0..nf {
        let r = uf_find(&mut uf, fi);
        regions.entry(r).or_default().push(fi);
    }

    let mut out: Vec<Vec<usize>> = Vec::new();
    for members in regions.values() {
        if members.len() == 1 {
            out.push(faces[members[0]].clone());
            continue;
        }
        // Greedily dissolve internal coplanar edges: a simple region collapses to one n-gon; an annulus
        // (a wall with a through-hole) merges as far as it can, freeing T-junctions pinned on neighbours.
        let region_faces: Vec<Vec<usize>> = members.iter().map(|&fi| faces[fi].clone()).collect();
        out.extend(greedy_merge_region_preserving(region_faces, positions));
    }

    dissolve_collinear(positions, &mut out);
    out
}

// ============================================================
// Soup → HalfEdgeMesh (weld + T-junction repair + rebuild)
// ============================================================

/// Standalone T-junction repair on an already-welded soup: split every ring edge at welded
/// points lying on its interior (within `eps`). Same algorithm as the `polygons_to_mesh`
/// pass below, without the BSP provenance bookkeeping — used by the Manifold wrap, whose
/// f64→f32 rounding + degenerate-tri drop + weld can leave un-paired seam edges (owner
/// intrude_02 2026-08-03: "manifold_to_halfedge rebuild failed", 9 boundary half-edges).
/// TODO(consolidation): fold `polygons_to_mesh`'s inline pass onto this helper.
pub(crate) fn repair_t_junctions(
    positions: &[Vec3],
    faces: &[Vec<usize>],
    eps: f32,
) -> Vec<Vec<usize>> {
    if faces.is_empty() {
        return Vec::new();
    }
    let (mut bb_min, mut bb_max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in positions {
        bb_min = bb_min.min(*p);
        bb_max = bb_max.max(*p);
    }
    let cell = ((bb_max - bb_min).length() / 64.0).max(eps * 8.0);
    let cell_key = |p: Vec3| -> (i64, i64, i64) {
        (
            (p.x / cell).floor() as i64,
            (p.y / cell).floor() as i64,
            (p.z / cell).floor() as i64,
        )
    };
    let mut grid: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for (i, p) in positions.iter().enumerate() {
        grid.entry(cell_key(*p)).or_default().push(i);
    }
    let mut out_faces: Vec<Vec<usize>> = Vec::with_capacity(faces.len());
    for ring in faces {
        let n = ring.len();
        let mut out: Vec<usize> = Vec::with_capacity(n);
        for i in 0..n {
            let ia = ring[i];
            let ib = ring[(i + 1) % n];
            let a = positions[ia];
            let b = positions[ib];
            out.push(ia);
            let ab = b - a;
            let len_sq = ab.length_squared();
            if len_sq <= f32::MIN_POSITIVE {
                continue;
            }
            let seg_min = a.min(b) - Vec3::splat(eps * 2.0);
            let seg_max = a.max(b) + Vec3::splat(eps * 2.0);
            let (k0x, k0y, k0z) = cell_key(seg_min);
            let (k1x, k1y, k1z) = cell_key(seg_max);
            let mut on_edge: Vec<(f32, usize)> = Vec::new();
            for kx in k0x..=k1x {
                for ky in k0y..=k1y {
                    for kz in k0z..=k1z {
                        let Some(bucket) = grid.get(&(kx, ky, kz)) else { continue };
                        for &ci in bucket {
                            if ci == ia || ci == ib {
                                continue;
                            }
                            let p = positions[ci];
                            let t = ab.dot(p - a) / len_sq;
                            if t <= 1e-4 || t >= 1.0 - 1e-4 {
                                continue;
                            }
                            if (a + ab * t).distance_squared(p) <= eps * eps {
                                on_edge.push((t, ci));
                            }
                        }
                    }
                }
            }
            on_edge.sort_by(|x, y| x.0.total_cmp(&y.0));
            on_edge.dedup_by_key(|e| e.1);
            for (_, ci) in on_edge {
                if out.last() != Some(&ci) && ring[(i + 1) % n] != ci {
                    out.push(ci);
                }
            }
        }
        out.dedup();
        while out.len() > 1 && out.first() == out.last() {
            out.pop();
        }
        if out.len() >= 3 {
            out_faces.push(out);
        }
    }
    out_faces
}

/// Weld the clipped soup, repair T-junctions, rebuild a half-edge mesh.
///
/// T-junction repair: BSP clipping can split a polygon on one side of a seam without
/// splitting its geometric neighbor on the other side. The un-split neighbor's edge then
/// has a welded vertex sitting in its interior → `from_polygons` cannot pair the twins →
/// boundary crack. Fix: for every ring edge, insert (in order) every welded vertex that
/// lies on the open segment within WELD_EPS.
/// Do rings `f` and `g` (same vertex SET) traverse it the SAME way (all directed edges match →
/// Some(true)), REVERSED (Some(false)), or neither (None — not a genuine coincident pair)?
fn ring_winding_same(f: &[usize], g: &[usize]) -> Option<bool> {
    let n = f.len();
    if g.len() != n { return None; }
    let fset: std::collections::HashSet<(usize, usize)> =
        (0..n).map(|i| (f[i], f[(i + 1) % n])).collect();
    let (mut same, mut rev) = (0usize, 0usize);
    for i in 0..n {
        let e = (g[i], g[(i + 1) % n]);
        if fset.contains(&e) { same += 1; }
        if fset.contains(&(e.1, e.0)) { rev += 1; }
    }
    if same == n { Some(true) } else if rev == n { Some(false) } else { None }
}

/// Resolve coincident faces (identical welded vertex SET) with PROVENANCE + OP awareness — the
/// coplanar-seam fix for sequential / coplanar booleans (the user's "2nd boolean on the same island
/// breaks / messy segmentation" class).
///
/// Wherever a cut plane coincides with an existing face, the BSP emits a face on BOTH solids. Whether
/// that coincident pair survives depends on the OP and the two faces' ORIGINAL outward normals
/// (`orig_n`, captured at input and immune to the csg.js inverts — the *stored* winding is unreliable
/// here because difference/intersect flip B). Rule for a CROSS-source (one A, one B) coincident pair:
///
///   | op                | original normals AGREE (dot>0) | original normals OPPOSE (dot<0) |
///   | Union / Intersect | keep one                       | drop BOTH (interior wall)       |
///   | Difference        | DROP BOTH (both sides void)    | keep A, drop B                  |
///
/// The failing x=0 seam is exactly Difference+AGREE: A's first-cut wall and B's cutter face point the
/// same way and subtracting B empties both sides → the shared face must vanish. The old provenance-blind
/// code "kept one" there → a same-winding non-manifold edge → is_manifold() rejected → boolean did
/// nothing. A SAME-source coincident pair is a pure BSP artifact of one solid: same winding → drop one,
/// opposite → drop both (a clean solid never has two coincident faces).
fn resolve_coincident_faces(
    faces: Vec<Vec<usize>>,
    src: &[u8],
    orig_n: &[Vec3],
    op: BoolOp,
) -> Vec<Vec<usize>> {
    let mut groups: BTreeMap<Vec<usize>, Vec<usize>> = BTreeMap::new();
    for (i, f) in faces.iter().enumerate() {
        let mut key = f.clone();
        key.sort_unstable();
        key.dedup();
        groups.entry(key).or_default().push(i);
    }
    let mut drop = vec![false; faces.len()];
    for (_, idxs) in &groups {
        if idxs.len() < 2 { continue; }
        let mut used = vec![false; idxs.len()];
        for a in 0..idxs.len() {
            if used[a] { continue; }
            for b in (a + 1)..idxs.len() {
                if used[b] { continue; }
                let (ia, ib) = (idxs[a], idxs[b]);
                // Confirm a genuine coincident pair (same ring up to direction) before acting.
                if ring_winding_same(&faces[ia], &faces[ib]).is_none() { continue; }
                let (drop_a, drop_b) = if src[ia] != src[ib] {
                    // CROSS-source A/B pair → op × original-normal-parity table.
                    let agree = orig_n[ia].dot(orig_n[ib]) > 0.0;
                    match (op, agree) {
                        (BoolOp::Difference, true) => (true, true),   // coplanar seam → both sides void
                        (BoolOp::Difference, false) => {              // keep the A face (correct winding), drop B
                            if src[ia] == SRC_A { (false, true) } else { (true, false) }
                        }
                        (_, true) => (false, true),                   // Union/Intersect agree → keep one copy
                        (_, false) => (true, true),                   // Union/Intersect oppose → interior, drop both
                    }
                } else {
                    // SAME-source BSP artifact: resolve by stored winding.
                    match ring_winding_same(&faces[ia], &faces[ib]) {
                        Some(true) => (false, true),   // duplicate → drop one
                        Some(false) => (true, true),   // face + its flip → cancel both
                        None => (false, false),
                    }
                };
                if drop_a { drop[ia] = true; used[a] = true; }
                if drop_b { drop[ib] = true; used[b] = true; }
                if drop_a { break; } // `a` consumed — stop pairing it
            }
        }
    }
    faces.into_iter().enumerate().filter(|(i, _)| !drop[*i]).map(|(_, f)| f).collect()
}

pub(super) fn polygons_to_mesh(polys: &[Polygon], op: BoolOp) -> Option<HalfEdgeMesh> {
    // 1) Weld all corners by tolerance. Carry each ring's PROVENANCE (source solid + original
    //    outward normal) in lockstep — the coincident-face resolution below needs it.
    let mut weld = ToleranceWeld::new();
    let mut rings: Vec<Vec<usize>> = Vec::with_capacity(polys.len());
    let mut ring_src: Vec<u8> = Vec::with_capacity(polys.len());
    let mut ring_orig: Vec<Vec3> = Vec::with_capacity(polys.len());
    for poly in polys {
        let mut ring: Vec<usize> = poly.verts.iter().map(|&v| weld.push(v)).collect();
        // Drop consecutive duplicates produced by welding (incl. wraparound).
        ring.dedup();
        while ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() < 3 {
            continue; // collapsed sliver — neighbors are healed by the T-junction pass
        }
        rings.push(ring);
        ring_src.push(poly.source);
        ring_orig.push(poly.orig_normal);
    }
    if rings.is_empty() {
        return None;
    }

    // 2) Spatial grid over welded positions for the T-junction pass.
    let (mut bb_min, mut bb_max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in &weld.positions {
        bb_min = bb_min.min(*p);
        bb_max = bb_max.max(*p);
    }
    let cell = ((bb_max - bb_min).length() / 64.0).max(WELD_EPS * 8.0);
    let cell_key = |p: Vec3| -> (i64, i64, i64) {
        (
            (p.x / cell).floor() as i64,
            (p.y / cell).floor() as i64,
            (p.z / cell).floor() as i64,
        )
    };
    let mut grid: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for (i, p) in weld.positions.iter().enumerate() {
        grid.entry(cell_key(*p)).or_default().push(i);
    }

    // 3) T-junction repair: split every ring edge at welded points lying on its interior.
    //    ONLY insert a point that has a real branch (a ring-neighbour off the edge line).
    //    Collinear pins (point only exists because an infinite plane cut another face) are
    //    skipped — they were the cube−cyl left-wall extras (Blender 40v vs 44v with 4 pins).
    let positions = &weld.positions;
    let mut ring_nbr: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); positions.len()];
    for ring in &rings {
        let n = ring.len();
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            ring_nbr[a].insert(b);
            ring_nbr[b].insert(a);
        }
    }
    let has_branch_off_edge = |ci: usize, a: Vec3, b: Vec3| -> bool {
        let ab = b - a;
        let len_sq = ab.length_squared();
        if len_sq < 1e-20 {
            return true;
        }
        for &nb in &ring_nbr[ci] {
            let p = positions[nb];
            let t = (p - a).dot(ab) / len_sq;
            let proj = a + ab * t.clamp(0.0, 1.0);
            if (p - proj).length_squared() > WELD_EPS * WELD_EPS * 4.0 {
                return true; // neighbour off the edge line → real T-junction
            }
        }
        false
    };
    let mut faces: Vec<Vec<usize>> = Vec::with_capacity(rings.len());
    let mut face_src: Vec<u8> = Vec::with_capacity(rings.len());
    let mut face_orig: Vec<Vec3> = Vec::with_capacity(rings.len());
    for (ri, ring) in rings.iter().enumerate() {
        let n = ring.len();
        let mut out: Vec<usize> = Vec::with_capacity(n);
        for i in 0..n {
            let ia = ring[i];
            let ib = ring[(i + 1) % n];
            let a = positions[ia];
            let b = positions[ib];
            out.push(ia);

            // Gather candidate points from grid cells overlapping the segment AABB.
            let seg_min = a.min(b) - Vec3::splat(WELD_EPS * 2.0);
            let seg_max = a.max(b) + Vec3::splat(WELD_EPS * 2.0);
            let (k0x, k0y, k0z) = cell_key(seg_min);
            let (k1x, k1y, k1z) = cell_key(seg_max);
            let ab = b - a;
            let len_sq = ab.length_squared();
            if len_sq <= f32::MIN_POSITIVE {
                continue;
            }
            let mut on_edge: Vec<(f32, usize)> = Vec::new();
            for kx in k0x..=k1x {
                for ky in k0y..=k1y {
                    for kz in k0z..=k1z {
                        let Some(bucket) = grid.get(&(kx, ky, kz)) else { continue };
                        for &ci in bucket {
                            if ci == ia || ci == ib {
                                continue;
                            }
                            let p = positions[ci];
                            let t = ab.dot(p - a) / len_sq;
                            if t <= 1e-4 || t >= 1.0 - 1e-4 {
                                continue;
                            }
                            let dist_sq = (a + ab * t).distance_squared(p);
                            if dist_sq <= WELD_EPS * WELD_EPS && has_branch_off_edge(ci, a, b) {
                                on_edge.push((t, ci));
                            }
                        }
                    }
                }
            }
            on_edge.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
            on_edge.dedup_by_key(|e| e.1);
            for (_, ci) in on_edge {
                if out.last() != Some(&ci) {
                    out.push(ci);
                }
            }
        }
        // Final ring hygiene after insertions.
        out.dedup();
        while out.len() > 1 && out.first() == out.last() {
            out.pop();
        }
        if out.len() >= 3 {
            faces.push(out);
            face_src.push(ring_src[ri]);
            face_orig.push(ring_orig[ri]);
        }
    }
    if faces.is_empty() {
        return None;
    }

    // 3b) Resolve coincident faces with PROVENANCE + OP awareness (the sequential/coplanar-boolean fix):
    // where a cut plane coincides with an existing face, the BSP emits a face on BOTH solids. The op and
    // the two faces' ORIGINAL outward normals decide keep-one / drop-both / keep-A — see
    // resolve_coincident_faces. This is what makes a 2nd boolean over a coplanar seam stay manifold.
    let faces = resolve_coincident_faces(faces, &face_src, &face_orig, op);
    if faces.is_empty() {
        return None;
    }

    // Rebuild from a face list, compacting to only referenced vertices (dissolve orphans some
    // welded points; an orphan vertex would inflate vertex_count and break the Euler invariant).
    let build = |flist: &[Vec<usize>]| -> HalfEdgeMesh {
        let mut remap = vec![usize::MAX; positions.len()];
        let mut cpos: Vec<Vec3> = Vec::new();
        let mut cfaces: Vec<Vec<usize>> = Vec::with_capacity(flist.len());
        for f in flist {
            let nf: Vec<usize> = f.iter().map(|&v| {
                if remap[v] == usize::MAX {
                    remap[v] = cpos.len();
                    cpos.push(positions[v]);
                }
                remap[v]
            }).collect();
            cfaces.push(nf);
        }
        HalfEdgeMesh::from_polygons(&cpos, &cfaces)
    };

    // 4) Coplanar dissolve → clean n-gons (Blender parity). GUARDED on is_manifold (audit #2): the
    // dissolve (or a coincident/coplanar face pair from the BSP) can leave an even-multiplicity
    // non-manifold edge that twins cleanly and passes validate()+boundary==0 — is_manifold() is what
    // rejects it. Fall back to the raw triangulated build, ITSELF validated. If neither is a clean
    // closed manifold, return None so the boolean fails honestly instead of emitting bad geometry
    // (was: `Some(build(&faces))` returned UNVALIDATED).
    //
    // 5) Post-boolean polish (Blender Exact parity) — shared with Manifold wrap.
    let polish = post_boolean_polish;
    let manifold_ok = |m: &HalfEdgeMesh| {
        boundary_half_edge_count(m) == 0 && m.validate().is_ok() && m.is_manifold().is_ok()
    };
    let merged = merge_coplanar(positions, &faces);
    if !merged.is_empty() {
        let m = build(&merged);
        if manifold_ok(&m) {
            return Some(polish(m));
        }
    }
    let tri = build(&faces);
    if manifold_ok(&tri) {
        // Triangulated fallback: still try limited dissolve so flat pairs collapse to one edge.
        return Some(polish(tri));
    }
    None
}

/// Precision scale below which two positions of a boolean result are "the same place" —
/// `bbox diagonal · 1e-5`, the scale `mark_contact_verts` already uses to decide that a vertex
/// lies ON the other operand's surface. It is a float-precision scale, not a shape tolerance:
/// f32 rounding of the exact kernel's f64 output sits orders of magnitude below it, while any
/// real facet of a modelled curve (a bevel strip, a cylinder wall) sits orders above it.
fn surface_eps(m: &HalfEdgeMesh) -> f32 {
    let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for v in &m.vertices {
        if v.half_edge == NO_EDGE {
            continue;
        }
        mn = mn.min(v.position);
        mx = mx.max(v.position);
    }
    if mn.x > mx.x {
        return 1e-6;
    }
    surface_eps_of_diag((mx - mn).length())
}

/// [`surface_eps`] from a bounding-box diagonal (soup callers have no `HalfEdgeMesh`).
fn surface_eps_of_diag(diag: f32) -> f32 {
    (diag * 1e-5).max(1e-6)
}

/// Would merging `faces` into one n-gon leave the SURFACE exactly where it is?
///
/// True iff every vertex of every face lies within `eps` (see [`surface_eps`]) of ONE plane —
/// the plane of the largest face (Newell: |n| = 2·area). Anchoring on the largest face matters:
/// a CSG sliver's own normal is numerical noise, but its vertices still sit on the plane of
/// the big face it was cut from, so float-noise debris keeps merging away.
///
/// This replaced a 12° face-normal ANGLE test (POLY-BOOL-NGON-BEVEL, owner `bollean.fl4`
/// 2026-10-13). An angle test is not a coplanarity test: a 32-segment round bevel steps 2.8°
/// per strip, so the polish dissolved the cutter's rounded channel strip by strip — and
/// because every merge re-averages the merged face's normal, the chain kept going until the
/// channel had been absorbed into the flat floor (a 27-gon bent by 0.011, the "partial
/// rounded channel"), and grazing cutter strips were absorbed into the target's own facets
/// across the intersection curve (five bent 18-gons). The solid changed by 0.0045 (0.8 % of
/// the carved volume) — invisible to the whole-mesh 1e-3 volume gate — and the bent n-gons
/// re-triangulated for display as the owner's slanted slivers and stray edges.
fn faces_share_plane(m: &HalfEdgeMesh, faces: &[FaceId], eps: f32) -> bool {
    let newell = |f: FaceId| newell_ring(m.face_vertices(f).into_iter().map(|v| m.vert(v).position));
    let Some((n, c)) = faces
        .iter()
        .map(|&f| newell(f))
        .max_by(|x, y| x.0.length_squared().total_cmp(&y.0.length_squared()))
    else {
        return false;
    };
    let n = n.normalize_or_zero();
    if n == Vec3::ZERO {
        return false; // every face degenerate: no plane to keep, merge nothing
    }
    faces.iter().all(|&f| {
        m.face_vertices(f)
            .iter()
            .all(|&v| (m.vert(v).position - c).dot(n).abs() <= eps)
    })
}

/// Post-boolean polish shared by BSP rebuild and Manifold wrap (Blender Exact parity):
/// limited dissolve, coplanar edge dissolve, valence-2/3 collinear cleanup, soup re-dissolve.
/// Only keeps transforms that stay closed + manifold; prefers fewer verts.
///
/// Every face merge in here is gated on [`faces_share_plane`]: the polish may re-mesh a plane,
/// never bend the surface.
pub(crate) fn post_boolean_polish(m: HalfEdgeMesh) -> HalfEdgeMesh {
    let manifold_ok = |mesh: &HalfEdgeMesh| {
        boundary_half_edge_count(mesh) == 0 && mesh.validate().is_ok() && mesh.is_manifold().is_ok()
    };
    if !manifold_ok(&m) {
        return m;
    }
    // BOOLEAN LOCALITY (2026-08-08, owner "the knife subtract must not delete an applied
    // slice" / "this needs to be localised"): the polish now honours the thread-local
    // preserve set like every other rebuild pass — this WAS the one admitted inconsistency
    // (step A ran `limited_dissolve(…, None, None)` and its region merge + valence sweep
    // wiped whole untouched lattices: 386v grid cube → 10v on an edge-clipping wedge cut).
    // Cut-created debris is never in the preserve set, so seam cleanup stays oracle-legal.
    // Belt-and-suspenders: every step's acceptance ALSO requires that no preserved input
    // vertex present in the polish input was deleted — a future preserve-blind pass fails
    // closed to the unpolished mesh instead of shipping the wipe.
    let pres_base = preserved_vert_count(&m);
    let pres_ok = |mesh: &HalfEdgeMesh| preserved_vert_count(mesh) >= pres_base;
    let eps = surface_eps(&m);
    let mut cur = m.clone();

    // A) Batch limited dissolve (Blender Limited Dissolve), protecting preserved flat edges
    // AND every candidate edge whose two faces do not share a plane (a facet of a curve).
    // Protection is by FACE exclusion (see `limited_dissolve`'s own note: dropping the edge
    // from the candidate list alone lets the region merge swallow it through another path).
    {
        let mut t = cur.clone();
        let mut protect = preserved_half_edges_of(&t, POST_BOOL_DISSOLVE_DEG);
        protect.extend(bent_half_edges_of(&t, POST_BOOL_DISSOLVE_DEG, eps));
        let _ = t.limited_dissolve(
            POST_BOOL_DISSOLVE_DEG,
            None,
            if protect.is_empty() { None } else { Some(&protect) },
        );
        if manifold_ok(&t) && pres_ok(&t) {
            cur = t;
        }
    }

    // B) Iterative single-edge coplanar dissolve (skips preserved edges itself).
    dissolve_coplanar_edges_iter(&mut cur, eps);
    if !manifold_ok(&cur) || !pres_ok(&cur) {
        cur = m.clone();
    }

    // C) Valence-2 collinear collapse.
    {
        let mut t = cur.clone();
        dissolve_collinear_valence2(&mut t, POST_BOOL_COLLINEAR_DOT);
        if manifold_ok(&t) && pres_ok(&t) {
            cur = t;
        }
    }

    // D) Valence-3 collinear T-junction pins.
    {
        let mut t = cur.clone();
        dissolve_valence3_collinear_tjunc(&mut t, eps);
        if manifold_ok(&t) && pres_ok(&t) {
            cur = t;
        }
    }

    // E) Soup re-dissolve.
    {
        let mut t = cur.clone();
        if let Some(simplified) = resimplify_collinear_rings(&t) {
            if manifold_ok(&simplified) && pres_ok(&simplified) {
                t = simplified;
            }
        }
        dissolve_collinear_valence2(&mut t, POST_BOOL_COLLINEAR_DOT);
        if manifold_ok(&t) && pres_ok(&t) {
            cur = t;
        }
    }

    // Final pick: fewest verts among candidates that kept every preserved vertex — the old
    // pure vertex-count tie-break only protected preserved geometry BY ACCIDENT (ties went
    // to the unpolished first element). `m` always qualifies (pres_base >= pres_base).
    [m, cur]
        .into_iter()
        .filter(|x| manifold_ok(x) && pres_ok(x))
        .min_by_key(|x| x.vertex_count())
        .unwrap()
}

/// Extract live face rings, dissolve collinear verts **per ring** (prev–v–next on each face),
/// then global `dissolve_collinear`, rebuild. Per-ring pass removes CSG pins that sit mid-edge
/// even when global valence > 2 (shared by two faces that both list the same collinear triple).
fn resimplify_collinear_rings(mesh: &HalfEdgeMesh) -> Option<HalfEdgeMesh> {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut remap: HashMap<u32, usize> = HashMap::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for i in 0..mesh.faces.len() as u32 {
        let fid = FaceId(i);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let ring: Vec<usize> = mesh
            .face_vertices(fid)
            .iter()
            .map(|v| {
                *remap.entry(v.0).or_insert_with(|| {
                    positions.push(mesh.vert(*v).position);
                    positions.len() - 1
                })
            })
            .collect();
        if ring.len() >= 3 {
            faces.push(ring);
        }
    }
    if faces.is_empty() {
        return None;
    }
    dissolve_collinear_per_ring(&positions, &mut faces);
    dissolve_collinear(&positions, &mut faces);
    if faces.is_empty() {
        return None;
    }
    // Compact to referenced verts.
    let mut used = vec![false; positions.len()];
    for f in &faces {
        for &v in f {
            used[v] = true;
        }
    }
    let mut new_pos = Vec::new();
    let mut ren = vec![usize::MAX; positions.len()];
    for (i, u) in used.iter().enumerate() {
        if *u {
            ren[i] = new_pos.len();
            new_pos.push(positions[i]);
        }
    }
    let new_faces: Vec<Vec<usize>> = faces
        .iter()
        .map(|f| f.iter().map(|&v| ren[v]).filter(|&v| v != usize::MAX).collect())
        .filter(|f: &Vec<usize>| f.len() >= 3)
        .collect();
    if new_faces.is_empty() {
        return None;
    }
    Some(HalfEdgeMesh::from_polygons(&new_pos, &new_faces))
}

/// On each face ring, drop verts that lie on the segment between their ring-neighbours.
/// Iterated to a fixed point per face. Does not require global valence-2.
fn dissolve_collinear_per_ring(positions: &[Vec3], faces: &mut Vec<Vec<usize>>) {
    let thr = COPLANAR_DIST.max(1e-2);
    let thr2 = thr * thr;
    for f in faces.iter_mut() {
        loop {
            if f.len() < 4 {
                break;
            }
            let n = f.len();
            let mut kill: Option<usize> = None;
            for i in 0..n {
                let p = f[(i + n - 1) % n];
                let v = f[i];
                let q = f[(i + 1) % n];
                let pv = positions[v];
                let a = positions[p];
                let b = positions[q];
                let ab = b - a;
                let len_sq = ab.length_squared();
                if len_sq < 1e-20 {
                    continue;
                }
                let t = (pv - a).dot(ab) / len_sq;
                if t <= 1e-4 || t >= 1.0 - 1e-4 {
                    continue;
                }
                let proj = a + ab * t;
                if (proj - pv).length_squared() <= thr2 {
                    kill = Some(i);
                    break;
                }
            }
            match kill {
                Some(i) => {
                    f.remove(i);
                }
                None => break,
            }
        }
    }
    faces.retain(|f| f.len() >= 3);
}

/// Repeatedly dissolve ONE interior edge whose two faces lie on one plane
/// ([`faces_share_plane`] at `eps`). Safer than a single batch limited-dissolve when residual
/// CSG faces form awkward regions — each successful merge frees T-junction pins on
/// neighbours. Bounded by edge count.
fn dissolve_coplanar_edges_iter(mesh: &mut HalfEdgeMesh, eps: f32) {
    let budget = mesh.half_edges.len().saturating_mul(2).max(8);
    // Undirected edge keys that `dissolve_edge` refused (slit / non-disk) — never retry.
    let mut refused: BTreeSet<(u32, u32)> = BTreeSet::new();
    for _ in 0..budget {
        let mut victim: Option<HalfEdgeId> = None;
        let he_n = mesh.half_edges.len();
        for i in 0..he_n {
            let he = HalfEdgeId(i as u32);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let tw = mesh.he(he).twin;
            // Process each undirected edge once (lower id).
            if tw == NO_TWIN || tw.0 < i as u32 {
                continue;
            }
            let a = mesh.he_source(he).0;
            let b = mesh.he(he).vertex.0;
            let key = if a < b { (a, b) } else { (b, a) };
            if refused.contains(&key) {
                continue;
            }
            let fa = mesh.he(he).face;
            let fb = mesh.he(tw).face;
            if fa == NO_FACE || fb == NO_FACE || fa == fb {
                continue;
            }
            // BOOLEAN LOCALITY (2026-08-08): an edge that existed on a boolean INPUT is
            // authored geometry, never polish debris — same contract as merge_coplanar.
            // Exception: both endpoints on the contact seam = the flush-contact interface
            // itself, which the boolean owns and may dissolve.
            let (pa, pb) = (
                mesh.vert(mesh.he_source(he)).position,
                mesh.vert(mesh.he(he).vertex).position,
            );
            if edge_is_preserved(pa, pb) && !(vert_is_seam(pa) && vert_is_seam(pb)) {
                continue;
            }
            if faces_share_plane(mesh, &[fa, fb], eps) {
                victim = Some(he);
                break;
            }
        }
        let Some(he) = victim else { break; };
        let a = mesh.he_source(he).0;
        let b = mesh.he(he).vertex.0;
        let key = if a < b { (a, b) } else { (b, a) };
        if mesh.dissolve_edge(he).is_none() {
            refused.insert(key);
            continue;
        }
        // After a successful merge, collapse any new valence-2 collinear verts immediately.
        dissolve_collinear_valence2(mesh, POST_BOOL_COLLINEAR_DOT);
    }
}

/// Remove valence-3 verts where two edges are collinear (CSG T-junction pin on a straight
/// edge). Strategy: dissolve the spur edge (the non-collinear one) when its two adjacent
/// faces share a plane ([`faces_share_plane`]); then valence-2 collinear collapse
/// removes the pin. If the spur cannot dissolve, try `dissolve_edge` on either collinear
/// half only when faces allow — otherwise leave it. Both merges are gated on
/// [`faces_share_plane`]: a T-pin is only debris on a FLAT.
fn dissolve_valence3_collinear_tjunc(mesh: &mut HalfEdgeMesh, eps: f32) {
    let budget = mesh.vertices.len().saturating_mul(4).max(16);
    for _ in 0..budget {
        let mut progress = false;
        let n = mesh.vertices.len();
        for i in 0..n {
            let v = VertexId(i as u32);
            if mesh.vert(v).half_edge == NO_EDGE {
                continue;
            }
            let fan = mesh.vertex_fan(v);
            if fan.len() != 3 {
                continue;
            }
            let pv = mesh.vert(v).position;
            // BOOLEAN LOCALITY (2026-08-08): a preserved input vertex is authored geometry,
            // never a CSG T-junction pin — leave it and everything hanging off it alone.
            // Contact/seam verts stay removable (a flush contact's grazing pin IS a pin).
            if vert_is_preserved(pv) && !vert_is_seam(pv) {
                continue;
            }
            // Pair of fan edges that are collinear through v.
            let mut colin: Option<(usize, usize)> = None;
            for a in 0..3 {
                for b in (a + 1)..3 {
                    let pa = mesh.vert(mesh.he(fan[a]).vertex).position;
                    let pb = mesh.vert(mesh.he(fan[b]).vertex).position;
                    let d1 = (pv - pa).normalize_or_zero();
                    let d2 = (pb - pv).normalize_or_zero();
                    if d1.length_squared() > 0.25
                        && d2.length_squared() > 0.25
                        && d1.dot(d2) >= T_JUNCTION_COLLINEAR_DOT
                    {
                        colin = Some((a, b));
                        break;
                    }
                }
                if colin.is_some() {
                    break;
                }
            }
            let Some((ia, _ib)) = colin else { continue; };
            // Spur = the fan edge that is NOT part of the collinear pair.
            let spur_i = (0..3).find(|&k| k != ia && k != colin.unwrap().1).unwrap();
            let spur = fan[spur_i];
            // 1) Prefer dissolving the spur edge when faces across it are coplanar.
            let tw = mesh.he(spur).twin;
            let mut did = false;
            if tw != NO_TWIN {
                let fa = mesh.he(spur).face;
                let fb = mesh.he(tw).face;
                if fa != NO_FACE && fb != NO_FACE && faces_share_plane(mesh, &[fa, fb], eps) {
                    if mesh.dissolve_edge(spur).is_some() {
                        did = true;
                    }
                }
            }
            // 2) Else dissolve the vertex itself (merge its 3-face fan into one n-gon) —
            //    Blender dissolve-verts on a T-pin on a FLAT: only when the whole fan shares
            //    one plane. Only keep if mesh stays manifold.
            let flat_fan = !did && {
                let fan_faces: Vec<FaceId> = fan
                    .iter()
                    .map(|&h| mesh.he(h).face)
                    .filter(|&f| f != NO_FACE)
                    .collect();
                fan_faces.len() == fan.len() && faces_share_plane(mesh, &fan_faces, eps)
            };
            if flat_fan {
                let before = mesh.vertex_count();
                // Snapshot via clone is expensive; try and rely on caller manifold_ok.
                if mesh.dissolve_vertex(v).is_some() && mesh.vertex_count() < before {
                    did = true;
                }
            }
            if did {
                dissolve_collinear_valence2(mesh, POST_BOOL_COLLINEAR_DOT);
                progress = true;
                break; // restart scan after topology change
            }
        }
        if !progress {
            break;
        }
    }
}

/// Collapse every INTERIOR valence-2 collinear vertex via `join_edge_kill_vert` (Blender
/// `use_verts` / `bmesh_kernel_join_edge_kill_vert`). Iterated to a fixed point. This is what
/// turns a boolean output with extra T-junctions on straight cube edges into Blender's clean
/// n-gon counts (cube−cyl16: 60v → 40v).
///
/// Honour the thread-local preserve set: a vertex that is an endpoint of a protected /
/// tagged edge (Mesh Cleanup Smart Select tags, boolean input preserve) is never dissolved.
/// Owner 2026-08-05 "never touch tagged edges" and 2026-08-06 test-019: collinear densify
/// pins must still go — those are not preserved.
pub(crate) fn dissolve_collinear_valence2(mesh: &mut HalfEdgeMesh, cos_min: f32) {
    for _ in 0..mesh.vertices.len().saturating_add(8) {
        let mut changed = false;
        let n = mesh.vertices.len();
        for i in 0..n {
            let v = VertexId(i as u32);
            // Freed / loose verts carry NO_EDGE (free_verts is private).
            if mesh.vert(v).half_edge == NO_EDGE {
                continue;
            }
            let pv = mesh.vert(v).position;
            if vert_is_preserved(pv) {
                continue;
            }
            let fan = mesh.vertex_fan(v);
            if fan.len() != 2 {
                continue;
            }
            let a = mesh.he(fan[0]).vertex;
            let b = mesh.he(fan[1]).vertex;
            if a == b {
                continue;
            }
            let d1 = (pv - mesh.vert(a).position).normalize_or_zero();
            let d2 = (mesh.vert(b).position - pv).normalize_or_zero();
            if d1.length_squared() < 0.25 || d2.length_squared() < 0.25 {
                continue;
            }
            // a→v→b collinear when the two unit segments agree (dot ≈ +1).
            if d1.dot(d2) < cos_min {
                continue;
            }
            if mesh.join_edge_kill_vert(v) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

