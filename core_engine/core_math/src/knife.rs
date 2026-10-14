//! knife.rs — screen-projected polyline cut (the poly "knife" cut engine).
//!
//! The old app-side knife resolved each *clicked* point to one vertex and connected two only if they
//! happened to share a face — so any cut crossing more than one face silently produced no geometry.
//! This engine walks the projected polyline and splits the mesh at EVERY edge it crosses, so consecutive
//! cut vertices are crossings on adjacent edges of the same face and always connect. Pure module: reads
//! the mesh through the public half-edge API and mutates via `split_edge` + `split_face_between`
//! (`half_edge.rs` stays frozen).
//!
//! Projection-agnostic: the caller passes a `project: Fn(world) -> screen` closure (perspective or ortho).
//! Each edge's 3D split parameter is found by a short binary search so the cut vertex lands under the
//! cursor for ANY projection. `through=true` cuts every crossed edge (front + back); `through=false`
//! restricts to edges with a camera-facing incident face (the visible surface + silhouette).
//!
//! INTERIOR CUT POINTS (Blender parity): a clicked point that lands *inside* a face (not on an edge) is a
//! real cut vertex. When `poly_3d` is supplied, such points route the cut through the face interior — a
//! through-cut (interior points between two boundary crossings) zig-zags the shared edge; a dangling
//! endpoint (cut ends inside a face) becomes a SLIT (a spur edge to an interior vertex, `[.. a, I, a ..]`).
//! Without `poly_3d` the engine keeps the crossing-only behaviour (the flat-grid test path).

use glam::{Vec2, Vec3};
use std::collections::HashSet;
use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, FaceId, VertexId, NO_TWIN, NO_FACE, NO_EDGE};

/// Screen-space segment/segment intersection. Returns `(t, u)` — the parameters along `a0→a1` and
/// `b0→b1` at the crossing — or `None` if they don't cross.
fn seg_intersect(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> Option<(f32, f32)> {
    let r = a1 - a0;
    let s = b1 - b0;
    let rxs = r.x * s.y - r.y * s.x;
    if rxs.abs() < 1e-9 {
        return None; // parallel / degenerate
    }
    let qp = b0 - a0;
    let t = (qp.x * s.y - qp.y * s.x) / rxs;
    let u = (qp.x * r.y - qp.y * r.x) / rxs;
    if (-1e-6..=1.0 + 1e-6).contains(&t) && (-1e-6..=1.0 + 1e-6).contains(&u) {
        Some((t.clamp(0.0, 1.0), u.clamp(0.0, 1.0)))
    } else {
        None
    }
}

/// 3D split parameter `s∈[0,1]` on edge (p0,p1) whose projection lands at screen-parameter `target` along
/// (s0,s1). Binary search — projection is monotonic along a straight edge, so this converges and is
/// exact for both ortho and perspective (ortho: s==target; perspective: perspective-correct).
fn edge_split_param_3d(
    p0: Vec3, p1: Vec3, target: f32, s0: Vec2, s1: Vec2, project: &dyn Fn(Vec3) -> Option<Vec2>,
) -> f32 {
    let d = s1 - s0;
    let l2 = d.length_squared();
    if l2 < 1e-12 {
        return target;
    }
    let param = |c: Vec2| (c - s0).dot(d) / l2;
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        match project(p0.lerp(p1, mid)) {
            Some(pm) => {
                if param(pm) < target { lo = mid; } else { hi = mid; }
            }
            None => break,
        }
    }
    0.5 * (lo + hi)
}

/// Closest parameter `t∈[0,1]` of point `p` onto segment (a,b), plus the distance.
pub(crate) fn closest_on_seg_3d(p: Vec3, a: Vec3, b: Vec3) -> (f32, f32) {
    let ab = b - a;
    let l2 = ab.length_squared();
    if l2 < 1e-20 {
        return (0.0, (p - a).length());
    }
    let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    (t, (p - (a + ab * t)).length())
}

/// Normalized Newell normal of an arbitrary 3D polygon ring (Vec3::ZERO if degenerate).
/// Public: also used to build WORLD-space face planes from transformed rings (app poly_knife).
pub fn newell_normal(pts: &[Vec3]) -> Vec3 {
    let mut n = Vec3::ZERO;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    n.normalize_or_zero()
}

/// Is `p` inside the polygon ring `pts` (plane normal `n`)? Sum-of-signed-angles winding test —
/// works for convex AND concave rings. `p` is assumed near the polygon plane. NOTE: a point exactly
/// ON the boundary reads as OUTSIDE (angle sum ≈ π) — callers needing on-rim awareness must
/// classify three-state (see `merge_coplanar_extrude_walls`).
pub fn point_in_polygon_3d(pts: &[Vec3], n: Vec3, p: Vec3) -> bool {
    if pts.len() < 3 {
        return false;
    }
    let mut angle = 0.0f32;
    for i in 0..pts.len() {
        let a = pts[i] - p;
        let b = pts[(i + 1) % pts.len()] - p;
        let (la, lb) = (a.length(), b.length());
        if la < 1e-6 || lb < 1e-6 {
            return true; // on a vertex
        }
        let cosv = (a.dot(b) / (la * lb)).clamp(-1.0, 1.0);
        let mut ai = cosv.acos();
        if a.cross(b).dot(n) < 0.0 {
            ai = -ai;
        }
        angle += ai;
    }
    angle.abs() > std::f32::consts::PI // winding number ±2π inside, ~0 outside
}

/// Is `p` inside face `f` (projected onto the face plane, winding-agnostic)? Concave-safe.
/// Public: also the coplanar-overlap probe for the extrude stacked-wall merge (ops_dissolve).
pub fn point_in_face_3d(mesh: &HalfEdgeMesh, f: FaceId, p: Vec3) -> bool {
    let vs = mesh.face_vertices(f);
    if vs.len() < 3 {
        return false;
    }
    let pts: Vec<Vec3> = vs.iter().map(|&v| mesh.vert(v).position).collect();
    point_in_polygon_3d(&pts, mesh.compute_face_normal(f), p)
}

/// One resolved node along the cut path.
#[derive(Clone, Copy)]
enum Node {
    /// A real mesh vertex (edge crossing already split, on-vertex snap, or a materialised interior tip).
    Bound(VertexId),
    /// A clicked point interior to a face — becomes a real cut vertex.
    Interior(Vec3),
    /// A clicked point OFF the surface (behind / outside every visible face). It breaks the path: two
    /// crossings on either side of it are NOT connected (the cut left the surface between them). Without
    /// this a sawtooth whose apex dips off-mesh drew a spurious cut across the face — which then re-cut a
    /// child face and corrupted the topology.
    Break,
}

/// Cut `mesh` along the screen-space polyline `poly` (≥2 points). `project` maps world→screen (None when
/// behind the camera / clipped). `poly_3d`, when supplied, is the 3D (mesh-local) position of each polyline
/// point and enables interior cut points (points inside a face become real cut vertices / slits). `closed`
/// connects the last point back to the first. `through` cuts every crossed edge; else only edges with a
/// camera-facing incident face (`view_dir` = camera forward, world).
///
/// `restrict_faces`: when `Some`, only edges incident to those faces are crossed and only interior clicks
/// on those faces become cut vertices — used by free-knife "selected poly only" splice mode.
/// Returns the number of new cut edges.
pub fn knife_cut_screen(
    mesh: &mut HalfEdgeMesh,
    project: &dyn Fn(Vec3) -> Option<Vec2>,
    poly: &[Vec2],
    poly_3d: Option<&[Vec3]>,
    closed: bool,
    through: bool,
    view_dir: Vec3,
    cam_local: Option<Vec3>,
    restrict_faces: Option<&HashSet<FaceId>>,
) -> usize {
    if poly.len() < 2 {
        return 0;
    }
    let face_ok = |f: FaceId| -> bool {
        if f == NO_FACE { return false; }
        match restrict_faces {
            None => true,
            Some(set) => set.contains(&f),
        }
    };
    let edge_touches_restrict = |mesh: &HalfEdgeMesh, he: HalfEdgeId| -> bool {
        let Some(set) = restrict_faces else { return true };
        let f1 = mesh.he(he).face;
        let f2 = if mesh.he(he).twin != NO_TWIN { mesh.he(mesh.he(he).twin).face } else { NO_FACE };
        (f1 != NO_FACE && set.contains(&f1)) || (f2 != NO_FACE && set.contains(&f2))
    };
    // Segments as (screen_a, screen_b, click_index_a) — the click index lets interior clicked points slot
    // into path order between the crossings of the segments around them.
    let mut segs: Vec<(Vec2, Vec2, usize)> = poly.windows(2).enumerate().map(|(i, w)| (w[0], w[1], i)).collect();
    if closed && poly.len() >= 3 {
        segs.push((*poly.last().unwrap(), poly[0], poly.len() - 1));
    }

    // Camera-facing test for a face (normal points toward the camera when dot(normal, view_dir) < 0).
    // Strict `< 0.0` is correct: a face at dot == 0 is exactly edge-on (an axis-aligned wall under a
    // diagonal view) and contributes nothing visible — admitting it over-cuts. A face's edge stays
    // reachable when its OTHER incident face is front-facing (the `front(f1) || front(f2)` OR below), so a
    // true silhouette edge is never dropped by this test.
    let front = |mesh: &HalfEdgeMesh, f: FaceId| -> bool {
        f != NO_FACE && mesh.compute_face_normal(f).dot(view_dir) < 0.0
    };
    // TRUE OCCLUSION for visible-only cuts (through=false). The facing test above is NOT occlusion:
    // a tilted bottom/back face can face the camera yet be hidden BEHIND the surface the user sees —
    // its edges then produced PHANTOM crossings that split the anchor chain and mis-cut hidden faces
    // (owner: "multi-segment cut often fails"; diag: an anchor on the bottom face amid a stroke that
    // lies entirely on the side face). A crossing point survives only if the ray from it toward the
    // camera (mesh-LOCAL position — affine transforms preserve the ordering along a sight line) hits
    // no nearer triangle. `None` (tests / callers without a camera) keeps the facing-only behavior.
    let mesh_diag = {
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in 0..mesh.vertices.len() as u32 {
            if mesh.vert(VertexId(v)).half_edge == NO_EDGE { continue; }
            let pos = mesh.vert(VertexId(v)).position;
            lo = lo.min(pos); hi = hi.max(pos);
        }
        if hi.x >= lo.x { (hi - lo).length().max(1e-6) } else { 1.0 }
    };
    let occluded = |mesh: &HalfEdgeMesh, wp: Vec3| -> bool {
        let Some(cam) = cam_local else { return false };
        let to_cam = cam - wp;
        let dist = to_cam.length();
        if dist < 1e-9 { return false; }
        let dir = to_cam / dist;
        let eps = 1e-4 * mesh_diag;
        let o = wp + dir * eps;
        for f in 0..mesh.faces.len() as u32 {
            let fid = FaceId(f);
            if mesh.face(fid).half_edge == NO_EDGE { continue; }
            let verts = mesh.face_vertices(fid);
            if verts.len() < 3 { continue; }
            // Ray ∩ face PLANE + winding-number containment — NOT a fan of triangles. A fan on
            // a CONCAVE n-gon claims phantom area over the notch (false occlusion drops legit
            // crossings) and MISSES real area inside it (hidden back-side edges deemed visible
            // → spurious far-side cuts: "the second cut broke the first cut, one side only").
            let n = mesh.compute_face_normal(fid);
            let denom = dir.dot(n);
            if denom.abs() < 1e-9 { continue; } // edge-on: no occlusion area
            let t = (mesh.vert(verts[0]).position - o).dot(n) / denom;
            if t <= 0.0 || t >= dist - 2.0 * eps { continue; }
            if point_in_face_3d(mesh, fid, o + dir * t) { return true; }
        }
        false
    };

    // 1) Collect every edge crossing along the path, in path order. path_key = seg_index + u (so crossings
    //    sort along the whole polyline). Read-only pass: the actual splits happen in step 2 so half-edge
    //    ids stay valid here. (path_key, primary_he, screen_t_on_edge)
    let mut crossings: Vec<(f32, u32, f32)> = Vec::new();
    let n_he = mesh.half_edges.len() as u32;
    for &(a, b, si) in &segs {
        for i in 0..n_he {
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE {
                continue;
            }
            if he.twin != NO_TWIN && i >= he.twin.0 {
                continue; // primary (undirected) edge only
            }
            if !edge_touches_restrict(mesh, HalfEdgeId(i)) {
                continue; // selected-face-only splice: ignore edges outside the selection
            }
            if !through {
                let f2 = if he.twin != NO_TWIN { mesh.he(he.twin).face } else { NO_FACE };
                if !front(mesh, he.face) && !front(mesh, f2) {
                    continue; // both incident faces face away → not the visible surface
                }
            }
            let p0 = mesh.vert(mesh.he_source(HalfEdgeId(i))).position;
            let p1 = mesh.vert(he.vertex).position;
            let (s0, s1) = match (project(p0), project(p1)) {
                (Some(x), Some(y)) => (x, y),
                _ => continue,
            };
            if let Some((t_edge, u_cut)) = seg_intersect(s0, s1, a, b) {
                // Visible-only: drop a crossing whose surface point is hidden behind nearer geometry.
                if !through && occluded(mesh, p0.lerp(p1, t_edge)) { continue; }
                crossings.push((si as f32 + u_cut, i, t_edge));
            }
        }
    }

    // 1b) Classify each clicked / densify path point (only with poly_3d).
    //     - On vertex → skip (crossing / endpoint handles it)
    //     - Near edge → SNAP to that edge as a Bound (split), NOT a free Interior
    //       Owner 23.fl4 (2026-08-05): densify samples ~2% off an edge became free mid-face
    //       verts (e.g. x=-0.98 next to x=-1), producing a "weird vertex on the edge" that
    //       breaks Extrude Along Normals. A larger edge-snap tolerance + Bound materialization
    //       keeps cut segments clean.
    //     - Genuine face interior → Interior node
    //     - Off surface → Break
    let mut interior_clicks: Vec<(f32, Vec3)> = Vec::new();
    let mut break_clicks: Vec<f32> = Vec::new();
    // (path_key, primary_he, 3d_t on edge) — densify/near-edge snaps merged with screen crossings.
    let mut edge_snaps: Vec<(f32, u32, f32)> = Vec::new();
    if let Some(p3d) = poly_3d {
        // Scale the on-vertex / on-edge tolerance to the mesh size. A fixed 1e-3 world-space threshold is
        // fine on a unit cube but far TIGHTER than the screen-space hover snap (≈8px) on a large mesh — a
        // point the user snapped ONTO an edge then lands >1e-3 from it in world space, gets misclassified
        // as face-interior, and routes through the interior path instead of splitting the edge ("crossing
        // a segment is not always working"). Tolerance = max(1e-3, 1e-3 · mesh diagonal) tracks the mesh.
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in 0..mesh.vertices.len() as u32 {
            if mesh.vert(VertexId(v)).half_edge == NO_EDGE { continue; }
            let pos = mesh.vert(VertexId(v)).position;
            lo = lo.min(pos);
            hi = hi.max(pos);
        }
        let diag = if hi.x >= lo.x { (hi - lo).length() } else { 1.0 };
        let vtol = (1e-3 * diag).max(1e-3);
        // Average live undirected edge length — local scale for densify near-edge snaps.
        // Owner 2026-08-07: plane subdivided 4× then knife L @ 45° failed to stay straight.
        // Tol was only clamped by mesh *diagonal* (2.5–5% of diag). On a dense grid that is
        // LARGER than a cell, so densify samples on a true straight segment snap to the wrong
        // grid edge and the cut zigzags. Cap by ~15% of average edge so mid-cell samples stay
        // free Interiors (or pure crossings) while still catching the 23.fl4 "2% off edge" class.
        let (mut elen_sum, mut elen_n) = (0.0f32, 0u32);
        for i in 0..mesh.half_edges.len() as u32 {
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE || (he.twin != NO_TWIN && i >= he.twin.0) {
                continue;
            }
            let a = mesh.vert(mesh.he_source(HalfEdgeId(i))).position;
            let b = mesh.vert(he.vertex).position;
            elen_sum += (b - a).length();
            elen_n += 1;
        }
        let avg_edge = if elen_n > 0 {
            elen_sum / elen_n as f32
        } else {
            diag * 0.25
        };
        // Generous snap band for densify samples that land near an edge but not exactly on it.
        // Diag-based floor for coarse meshes; avg-edge cap for dense subdivided surfaces.
        let edge_snap_tol = ((0.025 * diag).max(vtol * 10.0))
            .min(0.05 * diag)
            .min((avg_edge * 0.15).max(vtol))
            .max(vtol);
        for (k, &p) in p3d.iter().enumerate() {
            // near a vertex? → boundary, skip (a crossing / near-endpoint snap handles it).
            let near_vert = (0..mesh.vertices.len() as u32).any(|v| {
                mesh.vert(VertexId(v)).half_edge != NO_EDGE
                    && (mesh.vert(VertexId(v)).position - p).length() < vtol
            });
            if near_vert {
                continue;
            }
            // nearest live undirected edge (respect face restriction).
            let mut best: Option<(f32, u32, f32)> = None; // (dist, he, t)
            for i in 0..mesh.half_edges.len() as u32 {
                let he = mesh.he(HalfEdgeId(i));
                if he.face == NO_FACE || (he.twin != NO_TWIN && i >= he.twin.0) {
                    continue;
                }
                if !edge_touches_restrict(mesh, HalfEdgeId(i)) {
                    continue;
                }
                let a = mesh.vert(mesh.he_source(HalfEdgeId(i))).position;
                let b = mesh.vert(he.vertex).position;
                let (t, d) = closest_on_seg_3d(p, a, b);
                if best.map_or(true, |(bd, _, _)| d < bd) {
                    best = Some((d, i, t));
                }
            }
            if let Some((d, he_i, t)) = best {
                if d < edge_snap_tol {
                    // Snap onto the edge → Bound materialization (same progressive split as crossings).
                    edge_snaps.push((k as f32, he_i, t.clamp(0.0, 1.0)));
                    continue;
                }
            }
            // interior to some face → a real cut vertex; else OFF the surface → a path break.
            // When restrict_faces is set, only interiors on selected faces count (else Break).
            //
            // THE PLANE GATE IS LOAD-BEARING. `point_in_polygon_3d` is a sum-of-signed-angles
            // winding test and its doc states "p is assumed near the polygon plane" — that
            // precondition was assumed here, never enforced. It reads as INSIDE anywhere in the
            // face's infinite PRISM, at any height: a point floating in space above a cube's top
            // face is "interior" to it. Such a point then became a real cut vertex OUT IN SPACE
            // and the edges to it rendered as flaps sticking out of the mesh (owner 2026-08-05,
            // free-knife slice on a cube). Callers that hand over genuinely off-surface path
            // points — the free knife draws on a frozen plane, not on the surface — depend on
            // those points classifying as Break, so the precondition is now structural.
            let on_face_plane = |fid: FaceId| -> bool {
                let vs = mesh.face_vertices(fid);
                if vs.is_empty() {
                    return false;
                }
                let n = mesh.compute_face_normal(fid).normalize_or_zero();
                if n == Vec3::ZERO {
                    return false;
                }
                (p - mesh.vert(vs[0]).position).dot(n).abs() < vtol
            };
            if (0..mesh.faces.len() as u32).any(|f| {
                let fid = FaceId(f);
                mesh.face(fid).half_edge != NO_EDGE
                    && face_ok(fid)
                    && on_face_plane(fid)
                    && point_in_face_3d(mesh, fid, p)
            }) {
                interior_clicks.push((k as f32, p));
            } else {
                break_clicks.push(k as f32);
            }
        }
    }

    if crossings.len() < 2 && interior_clicks.is_empty() && edge_snaps.is_empty() {
        return 0;
    }
    crossings.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));

    // 2) Materialise crossings + near-edge densify snaps into boundary vertices. An edge may be
    //    crossed MORE THAN ONCE (a cut that dips into a face and returns on the SAME edge — the
    //    U-turn); each crossing must become its own vertex. So: compute every crossing's 3D split
    //    param first (before any split invalidates endpoints), group by edge, and split each edge
    //    PROGRESSIVELY at its crossings in ascending edge-order. A crossing near an endpoint reuses
    //    that vertex; near a previous split on the same edge, reuses it.
    const VEND: f32 = 1e-3;
    // 3D merge tolerance for the per-edge vertex snap below: ~1% of the mesh diagonal —
    // wide enough to absorb the densify chord error and cross-pass re-crossing noise
    // (both ~0.3-1% of diag in practice), narrow enough that two INTENTIONAL crossings
    // (a U-turn thinner than 1% of the object is visually zero-width) never merge.
    let merge_tol = {
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for i in 0..mesh.vertices.len() as u32 {
            let v = VertexId(i);
            if mesh.vert(v).half_edge == NO_EDGE {
                continue;
            }
            lo = lo.min(mesh.vert(v).position);
            hi = hi.max(mesh.vert(v).position);
        }
        if hi.x >= lo.x { (hi - lo).length() * 0.01 } else { 1e-3 }
    };
    // (path_key, primary_he, 3d_split_param) — s3 computed against the ORIGINAL (unsplit) edge.
    let mut cx: Vec<(f32, u32, f32)> = Vec::with_capacity(crossings.len() + edge_snaps.len());
    for &(pk, he, t_edge) in &crossings {
        let src = mesh.he_source(HalfEdgeId(he));
        let dst = mesh.he(HalfEdgeId(he)).vertex;
        let (p0, p1) = (mesh.vert(src).position, mesh.vert(dst).position);
        let s3 = match (project(p0), project(p1)) {
            (Some(s0), Some(s1)) => edge_split_param_3d(p0, p1, t_edge, s0, s1, project),
            _ => t_edge,
        };
        cx.push((pk, he, s3));
    }
    // Densify / free-path points that landed near an edge: already have a 3D param on that edge.
    for &(pk, he, s3) in &edge_snaps {
        cx.push((pk, he, s3));
    }
    // group by edge, then by ascending split param within the edge
    cx.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)));
    let mut pk_vert: Vec<(f32, VertexId)> = Vec::with_capacity(cx.len());
    let mut gi = 0usize;
    while gi < cx.len() {
        let he = cx[gi].1;
        let mut gj = gi;
        while gj < cx.len() && cx[gj].1 == he {
            gj += 1;
        }
        let src = mesh.he_source(HalfEdgeId(he));
        let dst = mesh.he(HalfEdgeId(he)).vertex;
        let (ep0, ep1) = (mesh.vert(src).position, mesh.vert(dst).position);
        // Per-edge cap (owner ortho-cylinder feedback 2026-08-07 "a lot better but not
        // perfect"): 1% of the MESH diagonal can rival a short facet edge on a long
        // dense body — an uncapped merge then snaps legitimate crossings to endpoints
        // and the cut line degrades. Never merge across more than 10% of the edge
        // being split.
        let merge_tol = merge_tol.min((ep1 - ep0).length() * 0.1);
        // 3D vertex snap: a crossing that lands within ~1% of the mesh diagonal of an
        // existing vertex on this edge (an endpoint — which after an earlier pass of a
        // multi-layer slice IS that pass's split vert — or the previous split, or a
        // near-edge densify snap) reuses that vertex instead of splitting again.
        // Edge-PARAM epsilons (VEND) cannot do this job: they don't compose across
        // passes (the same mathematical crossing, re-expressed on an already-shortened
        // segment, sits >VEND away in param while being visually coincident), and the
        // densify chord error scales with the sample step, not the edge length.
        // Duplicate-ring-vert class, owner 2026-08-07 "the slice always fails to do a
        // proper cut": a straight box slice left paired verts 0.01 apart on every
        // silhouette edge.
        let near_3d = |a: Vec3, b: Vec3| -> bool { (a - b).length() < merge_tol };
        // progressive split: `cur_he` is the remaining [prev→dst] segment; `base` = edge-param already
        // consumed, so the local param on the remaining segment is (s3-base)/(1-base).
        let mut cur_he = HalfEdgeId(he);
        let mut base = 0.0f32;
        let mut prev_v = src;
        let mut prev_s = 0.0f32;
        for k in gi..gj {
            let (pk, _, s3) = cx[k];
            let pos3 = ep0.lerp(ep1, s3.clamp(0.0, 1.0));
            let v = if s3 < VEND || (prev_v == src && near_3d(pos3, ep0)) {
                src
            } else if s3 > 1.0 - VEND || near_3d(pos3, ep1) {
                dst
            } else if prev_v != src
                && ((s3 - prev_s) < VEND || near_3d(pos3, mesh.vert(prev_v).position))
            {
                prev_v // coincident with the previous split on this edge → merge
            } else {
                let local_t = ((s3 - base) / (1.0 - base)).clamp(1e-3, 1.0 - 1e-3);
                let nv = mesh.split_edge(cur_he, local_t);
                cur_he = mesh.vert(nv).half_edge; // the fresh [nv→dst] segment
                base = s3;
                prev_v = nv;
                prev_s = s3;
                nv
            };
            pk_vert.push((pk, v));
        }
        gi = gj;
    }

    // Build the path node list in path order: Bound(vertex) for crossings, Interior(pos) for face-interior
    // clicks. Dedup consecutive equal Bounds (a cut through a shared vertex registers on both its edges).
    let mut stream: Vec<(f32, Node)> = Vec::with_capacity(pk_vert.len() + interior_clicks.len() + break_clicks.len());
    for &(pk, v) in &pk_vert {
        stream.push((pk, Node::Bound(v)));
    }
    for &(pk, p) in &interior_clicks {
        stream.push((pk, Node::Interior(p)));
    }
    for &pk in &break_clicks {
        stream.push((pk, Node::Break));
    }
    stream.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut nodes: Vec<Node> = Vec::new();
    for (_pk, node) in stream {
        match node {
            // dedup consecutive equal Bounds (a cut through a shared vertex registers on both its edges)
            Node::Bound(v) if matches!(nodes.last(), Some(Node::Bound(pv)) if *pv == v) => {}
            // dedup coincident consecutive Interiors (a duplicated click) — else a single interior
            // point encoded as [p, p] fabricates a degenerate zero-length slit
            Node::Interior(p) if matches!(nodes.last(), Some(Node::Interior(pv)) if (*pv - p).length() < 1e-5) => {}
            // collapse consecutive Breaks
            Node::Break if matches!(nodes.last(), Some(Node::Break)) => {}
            _ => nodes.push(node),
        }
    }
    // CLOSED PATHS ARE CYCLIC — rotate the stream so no run straddles the wrap.
    //
    // Owner "KNIFE FAIL.fl4" 2026-08-07: a rect slice whose top side leaves the silhouette
    // classifies as `I… B B… BRK B B… I…` — the OFF-surface top side is a Break sitting
    // BETWEEN the two edge crossings, and the real cut chain wraps around index 0. The run
    // walk below scans a LINEAR array, so it saw two runs (`I…B` and `B…I`), each holding a
    // single anchor, and degraded the through-cut into TWO DANGLING SLITS. Worse, the closed
    // polyline repeats its first point as its last, so each half materialised its own vertex
    // at the same corner → the coincident duplicate pair in the owner's file.
    //
    // Rotating left past the LAST Break makes the wrap-around run contiguous, after which the
    // linear walk is exactly correct. Then re-dedup: rotation can bring the closed path's
    // first==last pair together, and only now are they consecutive.
    if closed && nodes.len() >= 2 {
        if let Some(last_brk) = nodes.iter().rposition(|n| matches!(n, Node::Break)) {
            if last_brk + 1 < nodes.len() {
                nodes.rotate_left(last_brk + 1);
                let mut dedup: Vec<Node> = Vec::with_capacity(nodes.len());
                for node in nodes.drain(..) {
                    match node {
                        Node::Bound(v) if matches!(dedup.last(), Some(Node::Bound(pv)) if *pv == v) => {}
                        Node::Interior(p) if matches!(dedup.last(), Some(Node::Interior(pv)) if (*pv - p).length() < 1e-5) => {}
                        Node::Break if matches!(dedup.last(), Some(Node::Break)) => {}
                        _ => dedup.push(node),
                    }
                }
                nodes = dedup;
            }
        }
    }
    // Diagnostic node-stream dump (env AZRA_KNIFE_DIAG=1): shows where a missing segment fell out
    // (crossing not detected / merged / broken). The through-FAIL reasons below stay unconditional —
    // they are rare and were the decisive data for two shipped bugs.
    let knife_diag = std::env::var_os("AZRA_KNIFE_DIAG").is_some();
    if knife_diag {
        let dump: Vec<String> = nodes.iter().map(|n| match n {
            Node::Bound(v) => format!("B{}", v.0),
            Node::Interior(p) => format!("I({:.2},{:.2},{:.2})", p.x, p.y, p.z),
            Node::Break => "BRK".to_string(),
        }).collect();
        eprintln!("[KNIFE-DIAG] nodes({}): {}", nodes.len(), dump.join(" "));
    }
    if nodes.len() < 2 {
        if knife_diag { eprintln!("[KNIFE-DIAG] <2 nodes -> NO CUT"); }
        return 0;
    }

    // 3) Split faces ROBUSTLY. Incremental delete_face/create_face does NOT compose across multiple face
    //    splits in one stroke: re-cutting a face the same stroke just created makes create_face's twin
    //    re-pairing (a boundary-terminating vertex fan) mis-link → a non-manifold mesh (twin.twin != self).
    //    Instead, snapshot the (edge-split) mesh as a polygon soup of vertex INDICES, split those polygons
    //    by the cut chains as pure index math (no half-edge surgery), and rebuild ONCE via from_polygons —
    //    which re-pairs every twin globally. Breaks segment the path so an off-surface dip draws no cut.

    // position pool + snapshot every live face as an index list (twins are intact here, post edge-split).
    let mut positions: Vec<Vec3> = Vec::new();
    let mut vid_idx: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut polys: Vec<Vec<usize>> = Vec::new();
    // PER-POLYGON PARENT FACE (the pre-cut FaceId it descends from), parallel to `polys`. The
    // soup split is pure index math, so each child simply copies its parent's entry. It drives
    // the two things the rebuild must not lose:
    // - MATERIAL (owner 2026-08-07: "the knife tool should reassign the proper material") —
    //   `from_polygons` allocates every face with `material_id: 0`, so a cut used to repaint
    //   the body with the default material.
    // - IDS (POLY-STALE-IDS-AFTER-REBUILD, 2026-10-13) — the first polygon descending from a
    //   face keeps that FaceId, so the rebuild below is id-stable.
    let mut poly_parent: Vec<u32> = Vec::new();
    for fi in 0..mesh.faces.len() as u32 {
        let fid = FaceId(fi);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let poly: Vec<usize> = mesh
            .face_vertices(fid)
            .iter()
            .map(|&v| poly_index_of(v, mesh, &mut positions, &mut vid_idx))
            .collect();
        polys.push(poly);
        poly_parent.push(fi);
    }

    // resolve path nodes to position indices: Bound → existing vertex, Interior → a new position, Break kept.
    #[derive(Clone, Copy)]
    enum PE {
        Bound(usize),
        Interior(usize),
        Break,
    }
    let mut pe: Vec<PE> = Vec::with_capacity(nodes.len());
    for n in &nodes {
        match n {
            Node::Bound(v) => pe.push(PE::Bound(poly_index_of(*v, mesh, &mut positions, &mut vid_idx))),
            Node::Interior(p) => {
                let i = positions.len();
                positions.push(*p);
                pe.push(PE::Interior(i));
            }
            Node::Break => pe.push(PE::Break),
        }
    }

    // Walk maximal runs between Breaks. In each run: leading interiors before the first anchor → slit,
    // each consecutive anchor pair (threading its interior span) → through-split, trailing interiors → slit.
    let mut cut_edges = 0usize;
    let mut ci = 0usize;
    while ci < pe.len() {
        if matches!(pe[ci], PE::Break) {
            ci += 1;
            continue;
        }
        let mut re = ci;
        while re < pe.len() && !matches!(pe[re], PE::Break) {
            re += 1;
        }
        // Face-connectivity chain building (owner classic-S face-deletion 2026-08-07):
        // with `through=true`, HIDDEN-edge crossings land in the same path-ordered stream
        // as the visible surface chain — screen order interleaves DEPTH SHEETS. Pairing
        // nodes purely by stream order then anchors slits/throughs across unrelated faces
        // (§5.4's cross-face class one level up): the classic S on a box stitched a spur
        // from a hidden back-edge crossing to a front-face interior point, and the
        // rebuild ate the two side faces. Nodes therefore chain by FACE CONNECTIVITY: a
        // node joins the first chain whose LAST node shares a soup face with it;
        // otherwise it seeds a side chain (its own depth sheet). Each chain connects
        // independently with the same head-slit/through/tail-slit walk — a stream whose
        // consecutive nodes all share faces (every pre-existing case) chains identically
        // to the old sequential pairing.
        let chains: Vec<Vec<usize>> = {
            let node_in_poly = |poly: &[usize], e: &PE| -> bool {
                match *e {
                    PE::Bound(p) => poly.contains(&p),
                    PE::Interior(p) => {
                        // Plane-gated footprint (§5.4): winding alone reads the whole
                        // infinite prism as "inside".
                        let pos = positions[p];
                        let mut diag = 0.0f32;
                        for k in 1..poly.len() {
                            diag = diag.max((positions[poly[k]] - positions[poly[0]]).length());
                        }
                        plane_dist(&positions, poly, pos) <= (diag * 0.02).max(1e-4)
                            && poly_contains(&positions, poly, pos)
                    }
                    PE::Break => false,
                }
            };
            let mut chains: Vec<Vec<usize>> = Vec::new();
            for li in ci..re {
                if matches!(pe[li], PE::Break) {
                    continue;
                }
                let mut placed = false;
                for ch in chains.iter_mut() {
                    let last = *ch.last().unwrap();
                    if polys
                        .iter()
                        .any(|poly| node_in_poly(poly, &pe[last]) && node_in_poly(poly, &pe[li]))
                    {
                        ch.push(li);
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    chains.push(vec![li]);
                }
            }
            chains
        };
        if knife_diag && chains.len() > 1 {
            eprintln!("[KNIFE-DIAG] run split into {} face-connected chains", chains.len());
        }
        for chain in &chains {
        let bidx: Vec<(usize, usize)> = chain
            .iter()
            .enumerate()
            .filter_map(|(cp, &li)| if let PE::Bound(p) = pe[li] { Some((cp, p)) } else { None })
            .collect();
        let interior_span = |lo: usize, hi: usize| -> Vec<usize> {
            (lo..hi)
                .filter_map(|cp| if let PE::Interior(p) = pe[chain[cp]] { Some(p) } else { None })
                .collect()
        };
        if let Some(&(first_cp, first_p)) = bidx.first() {
            if first_cp > 0 {
                let mut r = interior_span(0, first_cp);
                r.reverse(); // grow the spur outward from the anchor
                let ok = split_poly_slit(&mut polys, &positions, first_p, &r);
                if knife_diag { eprintln!("[KNIFE-DIAG] head-slit anchor={} span={} -> {}", first_p, r.len(), if ok {"CUT"} else {"FAIL"}); }
                if ok { cut_edges += 1; }
            }
            for w in bidx.windows(2) {
                let ((ca, a), (cb, b)) = (w[0], w[1]);
                let inter = interior_span(ca + 1, cb);
                let ok = split_poly_through(&mut polys, &mut poly_parent, &positions, a, b, &inter);
                if knife_diag { eprintln!("[KNIFE-DIAG] through {}->{} span={} -> {}", a, b, inter.len(), if ok {"CUT"} else {"FAIL"}); }
                if ok { cut_edges += 1; }
            }
            let &(last_cp, last_p) = bidx.last().unwrap();
            if last_cp + 1 < chain.len() {
                let r = interior_span(last_cp + 1, chain.len());
                let ok = split_poly_slit(&mut polys, &positions, last_p, &r);
                if knife_diag { eprintln!("[KNIFE-DIAG] tail-slit anchor={} span={} -> {}", last_p, r.len(), if ok {"CUT"} else {"FAIL"}); }
                if ok { cut_edges += 1; }
            }
        } else {
            // Anchor-less chain: EVERY node is interior to a face (no edge crossing, no vertex snap) —
            // the basic "draw a line in the middle of a face" cut. Blender parity: it connects EACH free
            // endpoint of the interior chain to its OWN nearest boundary vertex, forming a boundary-to-
            // boundary chord that SPLITS the face into an n-gon (`BM_face_split_edgenet` island-connect) —
            // NOT a dangling one-vertex slit spur (a spur doesn't divide the face, and a wire/slit edge
            // then breaks the bevel — BEVEL.md §11 2026-07-18(c)). We route the chain through the same
            // `split_poly_through` used by the crossing-anchored case, with the two nearest ring vertices
            // as the a/b anchors and the interior points as the threaded span.
            let inter = interior_span(0, chain.len());
            if !inter.is_empty() {
                let probe = positions[inter[0]];
                // The face carrying the chain: probe inside AND nearest plane (disambiguates a shared-edge
                // footprint false-positive, same rule as find_soup_poly / split_poly_slit).
                let mut best: Option<(usize, f32)> = None;
                for (i, poly) in polys.iter().enumerate() {
                    if poly_contains(&positions, poly, probe) {
                        let d = plane_dist(&positions, poly, probe);
                        if best.map_or(true, |(_, bd)| d < bd) {
                            best = Some((i, d));
                        }
                    }
                }
                if let Some((pi, _)) = best {
                    // Every chain point must sit in the SAME face — a chain spanning faces with no crossing
                    // means the connecting edges were culled (back-face); don't guess a cross-face chord.
                    if knife_diag && !inter.iter().all(|&p| poly_contains(&positions, &polys[pi], positions[p])) {
                        eprintln!("[KNIFE-DIAG] anchorless run: chain spans faces -> NO CUT");
                    }
                    if inter.iter().all(|&p| poly_contains(&positions, &polys[pi], positions[p])) {
                        // CLOSED interior loop (the C-shape knife drawn wholly inside one face)
                        // divides into disc + annulus — never a slit spur. Without this, both free
                        // ends resolve to the SAME nearest ring vertex and the `(Some(a), _)` arm
                        // below emits the spur the owner reported as "GOT SEGMENT AFTER CUT"
                        // (KNIFE.md §5.5).
                        let gap = (positions[inter[0]] - positions[*inter.last().unwrap()]).length();
                        let step = (positions[inter[0]] - positions[inter.get(1).copied().unwrap_or(inter[0])]).length();
                        // The shape rebuild repeats the first sample so `knife_free_is_closed()`
                        // reads closure (§5.6) — but a node-level dedup may already have eaten it.
                        // Accept both: gap ≈ 0 (duplicate present, drop it) or gap ≈ one step
                        // (already deduped, keep every point).
                        let dup_close = gap <= step * 0.5;
                        let is_loop = closed && inter.len() >= 4 && gap <= step * 1.5;
                        if is_loop {
                            let mut lp = inter.clone();
                            if dup_close {
                                lp.pop(); // the duplicated closing sample (see §5.6)
                            }
                            if split_poly_closed_loop(&mut polys, &mut poly_parent, &positions, &lp) {
                                if knife_diag {
                                    eprintln!("[KNIFE-DIAG] closed interior loop k={} -> disc + 2 annulus halves", lp.len());
                                }
                                cut_edges += 1;
                                continue; // `for chain in &chains`
                            }
                        }
                        let head = positions[inter[0]];
                        let tail = positions[*inter.last().unwrap()];
                        // Nearest ring vertex to EACH free end (its own anchor) — restricted to
                        // vertices whose connecting segment stays INSIDE the face (midpoint
                        // containment). On a CONCAVE ring the unfiltered nearest vertex can sit
                        // across the notch; that chord leaves the polygon and tangles the ring.
                        // No visible candidate → no cut (fail closed).
                        let nearest = |p: Vec3| -> Option<usize> {
                            polys[pi].iter().copied()
                                .filter(|&x| poly_contains(&positions, &polys[pi], (positions[x] + p) * 0.5))
                                .min_by(|&x, &y| {
                                    (positions[x] - p).length().partial_cmp(&(positions[y] - p).length())
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                })
                        };
                        match (nearest(head), nearest(tail)) {
                            (Some(a_head), Some(a_tail)) if a_head != a_tail => {
                                // Boundary-to-boundary chord: a_head → chain → a_tail. split_poly_through
                                // builds the two rings and shares the interior chain as the cut edge.
                                let ok = split_poly_through(&mut polys, &mut poly_parent, &positions, a_head, a_tail, &inter);
                                if knife_diag { eprintln!("[KNIFE-DIAG] anchorless chord {}->{} span={} -> {}", a_head, a_tail, inter.len(), if ok {"CUT"} else {"FAIL"}); }
                                if ok { cut_edges += 1; }
                            }
                            (Some(a), _) => {
                                // Both ends resolve to the SAME ring vertex (a tiny chain hugging one
                                // corner) — a through-split would be a degenerate 2-gon. Fall back to a
                                // slit spur off that vertex (still embeds both clicked points).
                                if split_poly_slit(&mut polys, &positions, a, &inter) {
                                    cut_edges += 1;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        } // for chain
        ci = re;
    }

    if cut_edges == 0 {
        return 0;
    }
    // Rebuild ONCE. from_polygons re-pairs every twin by matching directed edges globally — robust to the
    // overlapping splits (child-of-child faces, shared fold-edge crossings) that broke incremental surgery.
    //
    // ID-STABLE (POLY-STALE-IDS-AFTER-REBUILD, 2026-10-13): a plain `from_polygons` renumbered every
    // vertex/edge/face and compacted dead slots, so the selection, hidden set, hover, active vertex and
    // the id-keyed CustomData sidecar silently pointed at DIFFERENT geometry after every cut (and past
    // the end of the mesh after a delete — the owner's `vert(301)` crash). The soup's own interning map
    // and parent faces give the exact correspondence, so every vertex, untouched edge and (first-child)
    // face keeps its id; only what the cut created is new.
    let mut pos_origin: Vec<Option<VertexId>> = vec![None; positions.len()];
    for (&v, &i) in &vid_idx {
        pos_origin[i] = Some(VertexId(v));
    }
    let poly_origin: Vec<Option<FaceId>> = poly_parent.iter().map(|&f| Some(FaceId(f))).collect();
    // Materials stamped at allocation, by input index — cannot drift past a skipped short ring.
    let materials: Vec<u32> = poly_parent.iter().map(|&f| mesh.face(FaceId(f)).material_id).collect();
    *mesh = HalfEdgeMesh::from_polygons_keeping_ids(mesh, &positions, &pos_origin, &polys, &poly_origin, &materials);
    mesh.recompute_normals();
    cut_edges
}

/// Index of vertex `v` in the position pool (interning by VertexId).
fn poly_index_of(
    v: VertexId,
    mesh: &HalfEdgeMesh,
    positions: &mut Vec<Vec3>,
    map: &mut std::collections::HashMap<u32, usize>,
) -> usize {
    if let Some(&i) = map.get(&v.0) {
        return i;
    }
    let i = positions.len();
    positions.push(mesh.vert(v).position);
    map.insert(v.0, i);
    i
}

/// Angle-sum point-in-polygon on an index list (winding-agnostic; convex AND concave). `p` is assumed near
/// the polygon plane (surface-snapped). Newell normal so it works for non-planar n-gons too.
fn poly_contains(positions: &[Vec3], poly: &[usize], p: Vec3) -> bool {
    let m = poly.len();
    if m < 3 {
        return false;
    }
    let mut nrm = Vec3::ZERO;
    for i in 0..m {
        let a = positions[poly[i]];
        let b = positions[poly[(i + 1) % m]];
        nrm += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    let n = nrm.normalize_or_zero();
    if n == Vec3::ZERO {
        return false;
    }
    let mut angle = 0.0f32;
    for i in 0..m {
        let a = positions[poly[i]] - p;
        let b = positions[poly[(i + 1) % m]] - p;
        let (la, lb) = (a.length(), b.length());
        if la < 1e-6 || lb < 1e-6 {
            return true;
        }
        let c = (a.dot(b) / (la * lb)).clamp(-1.0, 1.0);
        let mut ai = c.acos();
        if a.cross(b).dot(n) < 0.0 {
            ai = -ai;
        }
        angle += ai;
    }
    angle.abs() > std::f32::consts::PI
}

/// Distance from `p` to the polygon's plane (Newell normal through poly[0]). Disambiguates two faces that
/// share an edge but lie in different planes: the angle-sum footprint test alone gives a FALSE positive for
/// a point off the plane but within the footprint (e.g. a front-face cut point sits under the top face's
/// footprint), which mis-routed the cut to the wrong face and merged them. The correct face is the one whose
/// plane the surface-snapped cut point actually lies on → smallest plane distance.
fn plane_dist(positions: &[Vec3], poly: &[usize], p: Vec3) -> f32 {
    let m = poly.len();
    if m < 3 {
        return f32::MAX;
    }
    let mut nrm = Vec3::ZERO;
    for i in 0..m {
        let a = positions[poly[i]];
        let b = positions[poly[(i + 1) % m]];
        nrm += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    let n = nrm.normalize_or_zero();
    if n == Vec3::ZERO {
        return f32::MAX;
    }
    (p - positions[poly[0]]).dot(n).abs()
}

/// The polygon (in the growable soup) carrying both `a` and `b` with `probe` inside — and, among those, the
/// one whose PLANE the probe lies on (min plane distance), so a shared-edge anchor routes to the right face.
/// When NO candidate contains the probe, a carrier is accepted only if the probe lies ON its boundary
/// (the angle-sum containment test reads an exactly-on-edge point as outside — the legitimate fallback
/// case). A probe genuinely OUTSIDE every carrier — the midpoint of an anchor pair spanning a CONCAVE
/// face's notch — returns None. The old unconditional fallback emitted a chord straight across the notch:
/// self-intersecting ring → ear-clip stall-fan slivers ("cutting one part breaks the other part").
fn find_soup_poly(polys: &[Vec<usize>], positions: &[Vec3], a: usize, b: usize, probe: Vec3) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    let mut carriers: Vec<usize> = Vec::new();
    for (i, poly) in polys.iter().enumerate() {
        if poly.contains(&a) && poly.contains(&b) {
            if poly_contains(positions, poly, probe) {
                let d = plane_dist(positions, poly, probe);
                if best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            carriers.push(i);
        }
    }
    if let Some((i, _)) = best {
        return Some(i);
    }
    carriers.into_iter().find(|&i| {
        let poly = &polys[i];
        let m = poly.len();
        // On-boundary tolerance scaled to the polygon (crossing points come from split_edge lerps).
        let mut diag = 0.0f32;
        for k in 1..m {
            diag = diag.max((positions[poly[k]] - positions[poly[0]]).length());
        }
        let eps = (diag * 1e-4).max(1e-5);
        (0..m).any(|k| closest_on_seg_3d(probe, positions[poly[k]], positions[poly[(k + 1) % m]]).1 <= eps)
    })
}

/// Through-split the polygon carrying `a → interior → b` into two, by direct ring construction: boundary
/// arc(a→b) + reversed interior chain, and arc(b→a) + interior chain (the chain becomes the shared cut).
/// Replaces the poly in place and appends the second half. Handles a U-turn (a,b adjacent). Returns success.
fn split_poly_through(
    polys: &mut Vec<Vec<usize>>,
    parents: &mut Vec<u32>,
    positions: &[Vec3],
    a: usize,
    b: usize,
    interior: &[usize],
) -> bool {
    if a == b {
        // chain leaves `a` and returns to it → a spur, not a through-cut.
        // A slit rewrites its face in place, so no parent bookkeeping is needed.
        return split_poly_slit(polys, positions, a, interior);
    }
    let probe = interior
        .first()
        .map(|&i| positions[i])
        .unwrap_or_else(|| (positions[a] + positions[b]) * 0.5);
    let pi = match find_soup_poly(polys, positions, a, b, probe) {
        Some(i) => i,
        None => {
            eprintln!("[KNIFE-DIAG]   through-FAIL no-poly: a={} {:?} b={} {:?} probe={:?}",
                a, positions[a], b, positions[b], probe);
            return false;
        }
    };
    let ring = polys[pi].clone();
    let n = ring.len();
    let (ia, ib) = match (ring.iter().position(|&x| x == a), ring.iter().position(|&x| x == b)) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            eprintln!("[KNIFE-DIAG]   through-FAIL anchor-not-in-ring: a={} b={} ring={:?}", a, b, ring);
            return false;
        }
    };
    if ia == ib {
        eprintln!("[KNIFE-DIAG]   through-FAIL same-ring-slot: a={} b={}", a, b);
        return false;
    }
    let mut arc1 = Vec::new();
    let mut i = ia;
    loop {
        arc1.push(ring[i]);
        if i == ib { break; }
        i = (i + 1) % n;
    }
    let mut arc2 = Vec::new();
    let mut i = ib;
    loop {
        arc2.push(ring[i]);
        if i == ia { break; }
        i = (i + 1) % n;
    }
    let mut r1 = arc1;
    r1.extend(interior.iter().rev().copied());
    let mut r2 = arc2;
    r2.extend(interior.iter().copied());
    if r1.len() < 3 || r2.len() < 3 {
        // Adjacent endpoints with no interior chain: the ring edge a—b IS the cut already —
        // that's a Blender no-op, not a failure. Aborting here killed entire slices on dense
        // curved meshes (owner 2026-08-08 slice battery, torus): tube quads are small enough
        // that consecutive path anchors snap onto adjacent verts, which is a legal
        // already-cut state.
        if interior.is_empty() && ((ia + 1) % n == ib || (ib + 1) % n == ia) {
            return true;
        }
        eprintln!("[KNIFE-DIAG]   through-FAIL adjacent-2gon: a={} {:?} b={} {:?}", a, positions[a], b, positions[b]);
        return false; // degenerate arc with interior → refuse, never emit a 2-gon
    }
    polys[pi] = r1;
    polys.push(r2);
    // The second half is the SAME surface as the face we just divided — it descends from the
    // same pre-cut face, so it inherits that face's material (else it falls back to id 0 and a
    // cut repaints part of a textured body with the default material). `polys[pi]` keeps the
    // parent's slot, so it — not this half — inherits the parent's FaceId.
    let parent = parents[pi];
    parents.push(parent);
    debug_assert_eq!(polys.len(), parents.len(), "poly/parent arrays must stay parallel");
    true
}

/// Split the face carrying a CLOSED interior loop into inner disc + two annulus halves
/// (Blender-EXACT parity — `docs/planning/poly/circle_knife_oracle.py`: pinched_faces = 0, the
/// annulus arrives as two SIMPLE polygons). Never a keyhole ring, never a slit spur: a spur does
/// not divide the face, which is the owner's "GOT SEGMENT AFTER CUT" (KNIFE.md §5.5).
/// `lp` = the loop's position indices in traversal order, WITHOUT a repeated closing sample.
/// Fails closed (false) if there is no carrier face, if the loop is not wholly inside it, or if
/// two non-crossing bridges cannot be found.
fn split_poly_closed_loop(
    polys: &mut Vec<Vec<usize>>,
    parents: &mut Vec<u32>,
    positions: &[Vec3],
    lp: &[usize],
) -> bool {
    if lp.len() < 3 {
        return false;
    }
    // Carrier: contains EVERY loop point AND is plane-GATED (>2% of face diameter = reject).
    // Winding containment alone reads the whole infinite prism as inside (§3).
    let probe = positions[lp[0]];
    let mut best: Option<(usize, f32)> = None;
    for (i, poly) in polys.iter().enumerate() {
        if poly.len() < 3 {
            continue;
        }
        if !lp.iter().all(|&x| poly_contains(positions, poly, positions[x])) {
            continue;
        }
        let mut diag = 0.0f32;
        for k in 1..poly.len() {
            diag = diag.max((positions[poly[k]] - positions[poly[0]]).length());
        }
        let d = plane_dist(positions, poly, probe);
        if d > (diag * 0.02).max(1e-6) {
            continue;
        }
        if best.map_or(true, |(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    let pi = match best {
        Some((i, _)) => i,
        None => return false,
    };
    let ring = polys[pi].clone();
    let (n, k) = (ring.len(), lp.len());

    // Orient the loop to MATCH the parent ring, so the disc keeps the parent's normal and each
    // annulus half walks the loop backwards (= a hole).
    let newell = |ids: &[usize]| -> Vec3 {
        let mut s = Vec3::ZERO;
        for i in 0..ids.len() {
            s += positions[ids[i]].cross(positions[ids[(i + 1) % ids.len()]]);
        }
        s
    };
    let mut lp: Vec<usize> = lp.to_vec();
    if newell(&lp).dot(newell(&ring)) < 0.0 {
        lp.reverse();
    }

    // Two bridges that cannot cross: the nearest visible (ring, loop) pair, then the pair closest
    // to diametrically opposite on BOTH cycles. Midpoint containment is the same visibility filter
    // the anchor-less chord uses on a concave ring (fail closed).
    let visible = |ri: usize, li: usize, lp: &[usize]| -> bool {
        poly_contains(positions, &ring, (positions[ring[ri]] + positions[lp[li]]) * 0.5)
    };
    let d2 = |ri: usize, li: usize, lp: &[usize]| {
        (positions[ring[ri]] - positions[lp[li]]).length_squared()
    };
    let mut b0: Option<(usize, usize)> = None;
    for ri in 0..n {
        for li in 0..k {
            if !visible(ri, li, &lp) {
                continue;
            }
            if b0.map_or(true, |(r, l)| d2(ri, li, &lp) < d2(r, l, &lp)) {
                b0 = Some((ri, li));
            }
        }
    }
    let (r0, l0) = match b0 {
        Some(x) => x,
        None => return false,
    };
    let opp = |ri: usize, li: usize| -> f32 {
        let ra = (ri + n - r0) % n;
        let la = (li + k - l0) % k;
        (ra as f32 / n as f32 - 0.5).abs() + (la as f32 / k as f32 - 0.5).abs()
    };
    let mut b1: Option<(usize, usize)> = None;
    for ri in 0..n {
        for li in 0..k {
            if ri == r0 || li == l0 {
                continue;
            }
            if !visible(ri, li, &lp) {
                continue;
            }
            if b1.map_or(true, |(pr, pl)| opp(ri, li) < opp(pr, pl)) {
                b1 = Some((ri, li));
            }
        }
    }
    let (r1, l1) = match b1 {
        Some(x) => x,
        None => return false,
    };

    let arc = |cyc: &[usize], from: usize, to: usize| -> Vec<usize> {
        let m = cyc.len();
        let (mut out, mut i) = (Vec::new(), from);
        loop {
            out.push(cyc[i]);
            if i == to {
                break;
            }
            i = (i + 1) % m;
        }
        out
    };
    // Half A: ring arc r0→r1, bridge, loop BACKWARDS l1→l0, bridge home.
    let mut half_a = arc(&ring, r0, r1);
    let mut i = l1;
    loop {
        half_a.push(lp[i]);
        if i == l0 {
            break;
        }
        i = (i + k - 1) % k;
    }
    // Half B: ring arc r1→r0, bridge, loop BACKWARDS l0→l1, bridge home.
    let mut half_b = arc(&ring, r1, r0);
    let mut i = l0;
    loop {
        half_b.push(lp[i]);
        if i == l1 {
            break;
        }
        i = (i + k - 1) % k;
    }
    if half_a.len() < 3 || half_b.len() < 3 {
        return false;
    }

    let parent = parents[pi];
    polys[pi] = half_a; // keeps the parent's slot → keeps its FaceId
    polys.push(half_b);
    parents.push(parent);
    polys.push(lp); // the inner disc
    parents.push(parent);
    debug_assert_eq!(polys.len(), parents.len(), "poly/parent arrays must stay parallel");
    true
}

/// Slit: a dangling spur from boundary vertex `a` through `interior` (tip = last), ending inside the face.
/// Rewrites the polygon with a spur ring `[.. a, i0..ik, i_{k-1}..i0, a ..]`. Returns success.
fn split_poly_slit(polys: &mut Vec<Vec<usize>>, positions: &[Vec3], a: usize, interior: &[usize]) -> bool {
    if interior.is_empty() {
        return false;
    }
    let probe = positions[interior[0]];
    let pi = {
        // Among faces with `a` on the boundary AND the probe inside, pick the one whose PLANE the probe lies
        // on (min distance) — else a spur anchored on a shared edge binds to the wrong incident face.
        //
        // The plane test is a GATE, not only a tie-break (§5.4, owner classic-S face-deletion
        // 2026-08-07): winding containment alone reads the face's whole infinite PRISM as
        // "inside", so a spur whose tip sits far off every candidate's plane still bound to
        // one — the ring then self-intersects in 3D and the from_polygons rebuild eats whole
        // faces. A genuine slit tip is surface-resolved (distance ≈ 0); reject anything
        // beyond 2% of the candidate's diameter.
        let mut best: Option<(usize, f32)> = None;
        for (i, poly) in polys.iter().enumerate() {
            if !poly.contains(&a) {
                continue;
            }
            let mut diag = 0.0f32;
            for k in 1..poly.len() {
                diag = diag.max((positions[poly[k]] - positions[poly[0]]).length());
            }
            let d = plane_dist(positions, poly, probe);
            if d > (diag * 0.02).max(1e-4) {
                continue;
            }
            if poly_contains(positions, poly, probe) {
                if best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
        }
        match best {
            Some((i, _)) => i,
            None => return false,
        }
    };
    let ring = polys[pi].clone();
    let ai = match ring.iter().position(|&x| x == a) {
        Some(x) => x,
        None => return false,
    };
    let mut spur: Vec<usize> = Vec::new();
    spur.extend_from_slice(interior);
    for &v in interior.iter().rev().skip(1) {
        spur.push(v);
    }
    spur.push(a);
    let mut new_ring: Vec<usize> = Vec::new();
    new_ring.extend_from_slice(&ring[..=ai]);
    new_ring.extend_from_slice(&spur);
    new_ring.extend_from_slice(&ring[ai + 1..]);
    polys[pi] = new_ring;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat N×N grid plane (in the XY plane at z=0), for testing multi-face cuts. Open mesh.
    fn grid(n: usize) -> HalfEdgeMesh {
        let mut p = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                p.push(Vec3::new(i as f32 / n as f32 - 0.5, j as f32 / n as f32 - 0.5, 0.0));
            }
        }
        let idx = |i: usize, j: usize| j * (n + 1) + i;
        let mut f = Vec::new();
        for j in 0..n {
            for i in 0..n {
                f.push(vec![idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1)]);
            }
        }
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// Orthographic top-down projection (world XY → screen), so screen == world XY.
    fn ortho_xy(p: Vec3) -> Option<Vec2> {
        Some(Vec2::new(p.x, p.y))
    }

    /// A path point that is NOT on the surface must never become a cut vertex, however neatly it
    /// lines up with a face. `point_in_polygon_3d` is a winding test and reads as INSIDE anywhere
    /// in a face's infinite prism, at any height — so a point floating a unit above a cube's top
    /// face used to classify as interior to it, materialise as a real vertex out in space, and
    /// render as a flap hanging off the mesh (owner 2026-08-05: "slice produce geo outside of the
    /// target mesh to slice"). The free knife hands over exactly such points: it draws on a plane
    /// captured through the body centre, so anything past the silhouette sits in some face's prism.
    /// A through-cut must never leave an inverted face. Written while chasing the owner's
    /// "back-face anchor pairing - this is for the broken normal": `bidx.windows(2)` pairs
    /// anchors by PATH ORDER with nothing constraining them to one surface, and on a
    /// `through=true` cut both the front and back faces contribute crossings that project to
    /// nearly the same screen positions, so they interleave. This locks the simple case; it does
    /// NOT yet reproduce the owner's, so the pairing question stays open.
    #[test]
    fn through_cut_face_normals_stay_outward() {
        let mut m = HalfEdgeMesh::new_box(Vec3::splat(0.5)); // [-0.5,0.5]^3, like the owner's log
        let poly = [Vec2::new(0.05, -1.5), Vec2::new(0.05, 1.5)];
        let poly_3d = [Vec3::new(0.05, -1.5, 0.5), Vec3::new(0.05, 1.5, 0.5)];
        let n = knife_cut_screen(
            &mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None,
        );
        eprintln!("[DIAG] cuts={n} faces={} verts={}", m.face_count(), m.vertex_count());
        eprintln!("[DIAG] validate={:?} manifold={:?}", m.validate().is_ok(), m.is_manifold().is_ok());
        let mut bad = 0;
        for f in 0..m.faces.len() as u32 {
            let fid = FaceId(f);
            if m.face(fid).half_edge == NO_EDGE { continue; }
            let vs = m.face_vertices(fid);
            if vs.len() < 3 { continue; }
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            let nrm = m.compute_face_normal(fid).normalize_or_zero();
            // Convex box centred at origin: every outward normal agrees with its centroid direction.
            if nrm.dot(c.normalize_or_zero()) <= 0.0 {
                bad += 1;
                eprintln!("[DIAG] INVERTED face {f}: n={nrm:?} centroid={c:?} nverts={}", vs.len());
            }
        }
        eprintln!("[DIAG] inverted faces = {bad}");
        assert_eq!(bad, 0, "through-cut produced {bad} face(s) with a broken normal");
    }

    #[test]
    fn knife_off_surface_point_never_becomes_a_vertex() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE); // [-1,1]^3
        // Drawn on the z = 0 plane through the body centre: enter above the cube, cross the front
        // face with a bend, exit below it. Points 0 and 3 are past the silhouette but sit squarely
        // inside the top / bottom face prisms — the trap.
        let poly = [
            Vec2::new(0.2, 2.0), Vec2::new(0.2, 0.0),
            Vec2::new(-0.3, -0.5), Vec2::new(-0.3, -2.0),
        ];
        // What the app now supplies: surface-resolved positions where the view ray hits, the raw
        // drawing-plane point where it misses.
        let poly_3d = [
            Vec3::new(0.2, 2.0, 0.0),
            Vec3::new(0.2, 0.0, 1.0),
            Vec3::new(-0.3, -0.5, 1.0),
            Vec3::new(-0.3, -2.0, 0.0),
        ];
        let cuts = knife_cut_screen(
            &mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None,
        );
        assert!(cuts >= 1, "the front face must still be cut, got {cuts}");
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if m.vert(v).half_edge == NO_EDGE {
                continue;
            }
            let p = m.vert(v).position;
            assert!(
                p.x.abs() <= 1.0 + 1e-3 && p.y.abs() <= 1.0 + 1e-3 && p.z.abs() <= 1.0 + 1e-3,
                "cut produced a vertex OUTSIDE the cube at {p:?}"
            );
        }
        assert!(m.validate().is_ok(), "valid after the cut: {:?}", m.validate());
    }

    /// POLY-STALE-IDS-AFTER-REBUILD (owner 2026-10-13: delete a vertex, then knife). A cut must
    /// not renumber geometry it did not touch: a vertex selected before the cut is the SAME
    /// geometric vertex after it. The old commit rebuilt through a plain `from_polygons`, which
    /// interned vertices in face-traversal order and compacted the deleted slots — so the
    /// selected id survived "in range" while naming a different vertex.
    #[test]
    fn knife_cut_keeps_ids_of_untouched_geometry() {
        let mut m = grid(4); // 25 verts, ids j*5+i, y = j/4 - 0.5
        m.delete_vertex(VertexId(21)); // top row: makes the mesh sparse, far from the cut
        let pre = m.clone();
        let selected = VertexId(3); // bottom row (y = -0.5), untouched by a cut at y = 0.1
        let sel_pos = m.vert(selected).position;
        let untouched_face = FaceId(0); // bottom-left cell, y ∈ [-0.5, -0.25]
        let untouched_he = m.face_half_edges(untouched_face)[0];

        let poly = [Vec2::new(-0.6, 0.1), Vec2::new(0.6, 0.1)];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 4, "the cut must happen, got {cuts}");
        assert!(m.validate().is_ok(), "{:?}", m.validate());

        assert!(m.is_vert_live(selected));
        assert_eq!(m.vert(selected).position, sel_pos, "selected id must name the same vertex");
        // The deleted slot may be RECYCLED — by the in-place `split_edge` before the rebuild
        // (`alloc_vertex` pops the free list), as every in-place op does — but only ever for a
        // vertex the cut created, never renumbered onto pre-cut geometry.
        if m.is_vert_live(VertexId(21)) {
            assert!((m.vert(VertexId(21)).position.y - 0.1).abs() < 1e-4, "slot 21 reused by old geometry");
        }
        // Every pre-cut vertex ON A FACE keeps its id. (Vertex 20 went LOOSE with the delete —
        // its only face died — and a soup rebuild holds faces, so loose points are not carried.)
        let on_face: HashSet<VertexId> = (0..pre.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| pre.is_face_live(f))
            .flat_map(|f| pre.face_vertices(f))
            .collect();
        for v in 0..pre.vertices.len() as u32 {
            let v = VertexId(v);
            if on_face.contains(&v) {
                assert!(m.is_vert_live(v), "{v:?} died");
                assert_eq!(m.vert(v).position, pre.vert(v).position, "{v:?} renumbered");
            }
        }
        for v in pre.vertices.len() as u32..m.vertices.len() as u32 {
            assert!((m.vert(VertexId(v)).position.y - 0.1).abs() < 1e-4, "new vertices are the cut's, appended");
        }
        assert_eq!(m.face_vertices(untouched_face), pre.face_vertices(untouched_face), "untouched face keeps its id");
        assert_eq!(
            (m.he_source(untouched_he), m.he(untouched_he).vertex),
            (pre.he_source(untouched_he), pre.he(untouched_he).vertex),
            "untouched edge keeps its id"
        );
        // Every face the cut did not cross is the same face under the same id.
        for f in 0..pre.faces.len() as u32 {
            let f = FaceId(f);
            if !pre.is_face_live(f) {
                assert!(!m.is_face_live(f), "dead face slot {f:?} must stay dead");
                continue;
            }
            let ys: Vec<f32> = pre.face_vertices(f).iter().map(|&v| pre.vert(v).position.y).collect();
            let crossed = ys.iter().any(|&y| y < 0.1) && ys.iter().any(|&y| y > 0.1);
            if !crossed {
                assert_eq!(m.face_vertices(f), pre.face_vertices(f), "uncut face {f:?} renumbered");
            }
        }
    }

    /// A split face's FIRST child keeps the parent id (the other half is new), and both halves
    /// keep the parent's material — the id and the material ride the same parent bookkeeping.
    #[test]
    fn knife_split_face_child_inherits_parent_id_and_material() {
        let mut m = grid(2);
        for f in 0..m.faces.len() {
            m.faces[f].material_id = 10 + f as u32;
        }
        let pre = m.clone();
        let faces_before = m.faces.len();
        // Vertical cut through the left column only (x = -0.2), crossing faces 0 and 2.
        let poly = [Vec2::new(-0.2, -0.6), Vec2::new(-0.2, 0.6)];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 2, "got {cuts}");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        for f in 0..faces_before as u32 {
            assert!(m.is_face_live(FaceId(f)), "face {f} id must survive (first child inherits)");
            assert_eq!(m.face(FaceId(f)).material_id, pre.face(FaceId(f)).material_id);
        }
        let mut new_mats: Vec<u32> = (faces_before as u32..m.faces.len() as u32)
            .filter(|&f| m.is_face_live(FaceId(f)))
            .map(|f| m.face(FaceId(f)).material_id)
            .collect();
        new_mats.sort_unstable();
        assert_eq!(new_mats, vec![10, 12], "the appended halves inherit their parents' materials");
    }

    #[test]
    fn knife_straight_cut_crosses_all_faces() {
        // 4×4 grid; a horizontal line at y=0.1 crosses all 4 columns → must split every crossed edge and
        // connect within each of the 4 row-faces it passes through.
        let mut m = grid(4);
        let faces_before = m.face_count();
        let v_before = m.vertex_count();
        let poly = [Vec2::new(-0.6, 0.1), Vec2::new(0.6, 0.1)];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 4, "a line across 4 columns must make >=4 cut edges, got {cuts}");
        assert!(m.validate().is_ok(), "mesh valid after cut");
        assert!(m.vertex_count() > v_before, "edge splits added vertices ({} -> {})", v_before, m.vertex_count());
        assert!(m.face_count() > faces_before, "cut split faces");
    }

    #[test]
    fn knife_diagonal_through_vertices_valid() {
        // Full diagonal of a 3×3 grid — passes exactly through the interior grid VERTICES (the degenerate
        // case): must snap to them (not split) and connect the cell diagonals. Spans outside → 3 cells cut.
        let mut m = grid(3);
        let v_before = m.vertex_count();
        let poly = [Vec2::new(-0.6, -0.6), Vec2::new(0.6, 0.6)];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 3, "diagonal through 3 cells must make >=3 cut edges, got {cuts}");
        assert!(m.validate().is_ok(), "valid after diagonal cut");
        assert_eq!(m.vertex_count(), v_before, "cut through vertices must NOT add vertices");
    }

    #[test]
    fn knife_miss_is_noop() {
        // A line entirely outside the grid crosses nothing.
        let mut m = grid(3);
        let v = m.vertex_count();
        let poly = [Vec2::new(-2.0, -2.0), Vec2::new(-1.5, -2.0)];
        assert_eq!(knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None), 0);
        assert_eq!(m.vertex_count(), v, "miss must not change the mesh");
    }

    #[test]
    fn knife_closed_loop_cut() {
        // A closed square loop in the middle of a 6×6 grid → cuts a rectangular region boundary.
        let mut m = grid(6);
        let poly = [
            Vec2::new(-0.2, -0.2), Vec2::new(0.2, -0.2), Vec2::new(0.2, 0.2), Vec2::new(-0.2, 0.2),
        ];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, true, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 4, "closed loop must cut its 4 sides, got {cuts}");
        assert!(m.validate().is_ok(), "valid after closed cut");
    }

    #[test]
    fn knife_dangling_into_ngon_slits() {
        // Vertex-bevel a cube corner → the top (+Y) face is a PENTAGON. A cut from OUTSIDE into the pentagon
        // interior (dangling endpoint) must slit the n-gon (this is the reported "won't cut the n-gon").
        use crate::bevel_op::{bevel_vertices_offset, BevelParams};
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let vi = cube.vertices.iter().position(|v| (v.position - Vec3::ONE).length() < 1e-4).unwrap() as u32;
        let mut sel = HashSet::new();
        sel.insert(vi);
        let mut m = bevel_vertices_offset(&cube, &sel, &BevelParams { width: 0.4, segments: 1, shape: 0.5, clamp: true });
        let f_before = m.face_count();
        let v_before = m.vertex_count();
        let project = |q: Vec3| -> Option<Vec2> { Some(Vec2::new(q.x, q.z)) }; // top-down along -Y
        // screen path: from outside (-1.5,0) to an interior point of the top pentagon (0.3, 0.3)
        let poly = [Vec2::new(-1.5, 0.0), Vec2::new(0.3, 0.3)];
        let poly_3d = [Vec3::new(-1.5, 1.0, 0.0), Vec3::new(0.3, 1.0, 0.3)]; // 2nd point is on the top face
        let cuts = knife_cut_screen(&mut m, &project, &poly, Some(&poly_3d), false, false, Vec3::NEG_Y, None, None);
        assert!(cuts >= 1, "dangling cut into the n-gon must create geometry, got {cuts}");
        assert!(m.validate().is_ok(), "valid after slit cut");
        assert!(m.vertex_count() > v_before, "slit added an interior vertex");
        // slit keeps the same face count around that face (one polygon with a spur), but the entry edge
        // split can add a face elsewhere; just require the mesh stayed valid & grew a vertex.
        let _ = f_before;
        // Render path: the slit face (a degenerate spur `[.. a, I, a ..]`) MUST tessellate without panic /
        // hang — the ear-clipper's stall guard fans the remainder; the zero-area spur renders as nothing.
        let (verts, tris) = crate::poly_mesh::raster_data_ngon(&m);
        assert!(!verts.is_empty() && !tris.is_empty(), "slit mesh still tessellates for display");
        assert_eq!(tris.len() % 3, 0, "triangle index list well-formed");
    }

    /// No face may carry a repeated vertex (the tell-tale of a self-intersecting tangle the ear-clipper
    /// fans into a triangle spray). Slits are excluded — they legitimately repeat their anchor.
    fn no_tangled_faces(m: &HalfEdgeMesh) -> bool {
        for f in 0..m.faces.len() as u32 {
            let fid = FaceId(f);
            if m.face(fid).half_edge == NO_EDGE {
                continue;
            }
            let vs = m.face_vertices(fid);
            let mut seen = std::collections::HashSet::new();
            if vs.iter().any(|v| !seen.insert(v.0)) {
                return false;
            }
        }
        true
    }

    #[test]
    fn knife_concave_face_cut_never_chords_the_notch() {
        // A rectangle with a notch cut into its top edge — a CONCAVE n-gon (exactly what the SketchUp-
        // style pushed-in extrude leaves after the stacked-plane merge). A horizontal cut at y=0.5
        // crosses the boundary 4×: left edge, both notch walls, right edge. The consecutive-anchor
        // pair spanning the NOTCH (midpoint outside the polygon) must be REFUSED — the old
        // find_soup_poly fallback emitted a chord straight across the notch: self-intersecting ring,
        // ear-clip stall-fan slivers, "cutting one part breaks the other part". Both arm spans must
        // still cut.
        let positions = vec![
            Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(0.5, 1.0, 0.0), Vec3::new(0.5, 0.0, 0.0), Vec3::new(-0.5, 0.0, 0.0),
            Vec3::new(-0.5, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&positions, &[vec![0, 1, 2, 3, 4, 5, 6, 7]]);
        let f0 = m.face_count();
        let poly = [Vec2::new(-1.5, 0.5), Vec2::new(1.5, 0.5)];
        let poly_3d = [Vec3::new(-1.5, 0.5, 0.0), Vec3::new(1.5, 0.5, 0.0)]; // both ends OFF the surface
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 2, "both arms must cut, got {cuts}");
        assert!(m.validate().is_ok(), "valid after concave cut");
        assert!(no_tangled_faces(&m), "no self-intersecting (dup-vertex) face after concave cut");
        assert_eq!(m.face_count(), f0 + 2, "left arm + right arm each split once → +2 faces");
        // The two notch-wall crossings must NOT be joined by an edge across the notch.
        let at = |p: Vec3| (0..m.vertices.len() as u32).map(VertexId)
            .find(|&v| m.vert(v).half_edge != NO_EDGE && (m.vert(v).position - p).length() < 1e-3);
        if let (Some(va), Some(vb)) = (at(Vec3::new(-0.5, 0.5, 0.0)), at(Vec3::new(0.5, 0.5, 0.0))) {
            let joined = (0..m.half_edges.len() as u32).map(HalfEdgeId).any(|h| {
                m.is_edge_live(h) && {
                    let (s, d) = (m.he_source(h), m.he(h).vertex);
                    (s == va && d == vb) || (s == vb && d == va)
                }
            });
            assert!(!joined, "chord across the notch — the concave-fallback corruption is back");
        }
    }

    #[test]
    fn knife_visible_only_occlusion_sees_through_concave_notch() {
        // OWNER REPRO 2026-08-03 ("second small cut broke the first cut's geo, one side only").
        // Front sheet: notched CONCAVE octagon (z=0.5), ring deliberately STARTED at the notch's
        // inner corner so the old FAN-triangulated occlusion covered the hole with phantom
        // triangles. Back sheet: a quad entirely behind the HOLE (z=-0.5). A visible-only cut
        // straight down x=0 must split BOTH faces — the back quad is visible THROUGH the notch.
        // The fan claimed the hole was solid → a back crossing was dropped as "occluded" → the
        // hidden side was half-cut/mangled. Plane + winding containment sees the hole.
        let p = vec![
            Vec3::new(0.5, 0.0, 0.5), Vec3::new(-0.5, 0.0, 0.5), Vec3::new(-0.5, 1.0, 0.5),
            Vec3::new(-1.0, 1.0, 0.5), Vec3::new(-1.0, -1.0, 0.5), Vec3::new(1.0, -1.0, 0.5),
            Vec3::new(1.0, 1.0, 0.5), Vec3::new(0.5, 1.0, 0.5),
            Vec3::new(-0.4, 0.2, -0.5), Vec3::new(0.4, 0.2, -0.5),
            Vec3::new(0.4, 0.9, -0.5), Vec3::new(-0.4, 0.9, -0.5),
        ];
        let faces = vec![vec![0, 1, 2, 3, 4, 5, 6, 7], vec![8, 9, 10, 11]];
        let mut m = HalfEdgeMesh::from_polygons(&p, &faces);
        let f0 = m.face_count();
        let poly = [Vec2::new(0.0, -2.0), Vec2::new(0.0, 2.0)];
        let poly_3d = [Vec3::new(0.0, -2.0, 0.5), Vec3::new(0.0, 2.0, 0.5)]; // both ends OFF the surface
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, false,
            Vec3::NEG_Z, Some(Vec3::new(0.0, 0.0, 10.0)), None);
        assert!(m.validate().is_ok(), "valid after see-through cut");
        assert!(no_tangled_faces(&m), "no tangled faces after see-through cut");
        assert!(cuts >= 2, "front arm AND back quad must both cut, got {cuts}");
        assert_eq!(m.face_count(), f0 + 2, "front splits once, back quad splits once");
    }

    #[test]
    fn knife_u_cut_same_edge_splits_clean() {
        // Dip INTO a face and return on the SAME edge (the U-turn from the wrapping-cut report). Old engine
        // dropped the 2nd same-edge crossing → the run degraded to a slit → a self-intersecting 11-gon that
        // the ear-clipper fanned. Now: two same-edge crossings each split, direct-ring build → 2 clean faces.
        let mut m = grid(1); // quad [-0.5,0.5]^2
        let f0 = m.face_count();
        let poly = [
            Vec2::new(-0.3, 0.7), Vec2::new(-0.3, 0.2), Vec2::new(0.0, -0.2),
            Vec2::new(0.3, 0.2), Vec2::new(0.3, 0.7),
        ];
        let poly_3d = [
            Vec3::new(-0.3, 0.7, 0.0), Vec3::new(-0.3, 0.2, 0.0), Vec3::new(0.0, -0.2, 0.0),
            Vec3::new(0.3, 0.2, 0.0), Vec3::new(0.3, 0.7, 0.0),
        ];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 1, "U-cut must split the face, got {cuts}");
        assert!(m.validate().is_ok(), "valid after U-cut");
        assert_eq!(m.face_count(), f0 + 1, "U-cut: quad → 2 faces (dip cap + remainder)");
        assert!(no_tangled_faces(&m), "no self-intersecting (dup-vertex) face — no ear-clip fan");
        // the 3 interior dip points became real vertices
        for p in [Vec3::new(-0.3, 0.2, 0.0), Vec3::new(0.0, -0.2, 0.0), Vec3::new(0.3, 0.2, 0.0)] {
            assert!(
                (0..m.vertices.len() as u32).any(|v| m.vert(VertexId(v)).half_edge != NO_EDGE
                    && (m.vert(VertexId(v)).position - p).length() < 1e-3),
                "dip point {p:?} became a real vertex"
            );
        }
    }

    #[test]
    fn knife_multiface_interior_watertight() {
        // A cut spanning TWO adjacent quads over their shared edge, with an interior point in EACH (the
        // wrapping cut, flattened). Must split both faces and stay watertight (valid + no tangled face).
        let mut m = grid(2); // 2×2 quads
        let f0 = m.face_count();
        let poly = [
            Vec2::new(-0.7, 0.25), Vec2::new(-0.25, 0.30), Vec2::new(0.25, 0.30), Vec2::new(0.7, 0.25),
        ];
        let poly_3d = [
            Vec3::new(-0.7, 0.25, 0.0), Vec3::new(-0.25, 0.30, 0.0),
            Vec3::new(0.25, 0.30, 0.0), Vec3::new(0.7, 0.25, 0.0),
        ];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 2, "cut must split both top quads, got {cuts}");
        assert!(m.validate().is_ok(), "valid after multi-face interior cut");
        assert!(no_tangled_faces(&m), "no self-intersecting face across the span");
        assert_eq!(m.face_count(), f0 + 2, "both top quads split → +2 faces");
    }

    /// Owner 23.fl4 (2026-08-05): densify sample ~2cm off a cube edge (x=-0.98 next to x=-1)
    /// used to become a free Interior vertex → "weird vertex on the edge" that broke Extrude
    /// Along Normals. Near-edge samples must SNAP onto the edge (Bound), never free-float.
    #[test]
    fn knife_near_edge_densify_snaps_to_edge_not_free_interior() {
        // Unit-ish quad [-1,1]^2 so edge_snap_tol ≈ 0.05 catches a 0.02 offset.
        let p = [
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
        ];
        let faces = [vec![0usize, 1, 2, 3]];
        let mut m = HalfEdgeMesh::from_polygons(&p, &faces);
        let v_before = m.vertex_count();
        // Horizontal cut: left sample is 0.02 inside from the left edge (the densify-near-edge class),
        // right sample is a clean mid-face point so the path still cuts.
        let near_edge = Vec3::new(-0.98, 0.0, 0.0);
        let mid = Vec3::new(0.0, 0.0, 0.0);
        let poly = [Vec2::new(-0.98, 0.0), Vec2::new(0.0, 0.0)];
        let poly_3d = [near_edge, mid];
        let cuts = knife_cut_screen(
            &mut m,
            &ortho_xy,
            &poly,
            Some(&poly_3d),
            false,
            true,
            Vec3::NEG_Z,
            None,
            None,
        );
        assert!(cuts >= 1, "near-edge path must still cut, got {cuts}");
        assert!(m.validate().is_ok(), "valid: {:?}", m.validate());
        // No free vertex stuck at the off-edge densify sample.
        let free_near = (0..m.vertices.len() as u32).any(|v| {
            m.vert(VertexId(v)).half_edge != NO_EDGE
                && (m.vert(VertexId(v)).position - near_edge).length() < 1e-3
        });
        assert!(
            !free_near,
            "densify sample at {near_edge:?} must NOT stay as a free interior vert"
        );
        // Must have a real split ON the left edge (x≈-1).
        let on_left_edge = (0..m.vertices.len() as u32).any(|v| {
            if m.vert(VertexId(v)).half_edge == NO_EDGE {
                return false;
            }
            let q = m.vert(VertexId(v)).position;
            (q.x + 1.0).abs() < 1e-3 && q.y.abs() < 0.05
        });
        assert!(
            on_left_edge,
            "near-edge sample must snap onto left edge (x=-1); verts after: {}",
            m.vertex_count() - v_before
        );
    }

    #[test]
    fn knife_interior_only_cut_makes_geometry() {
        // THE basic gesture: two clicks in the MIDDLE of one face, crossing no edge. Blender parity: each
        // free endpoint connects to ITS OWN nearest boundary vertex → a boundary-to-boundary chord that
        // SPLITS the quad into two n-gons (NOT a dangling slit — a slit doesn't divide the face and its
        // wire edge later breaks the bevel, BEVEL.md §11 2026-07-18(c)). One quad → 2 faces, +2 vertices.
        let mut m = grid(1); // single quad [-0.5,0.5]^2
        let v_before = m.vertex_count();
        let f_before = m.face_count();
        let poly = [Vec2::new(-0.2, 0.05), Vec2::new(0.2, 0.05)];
        let poly_3d = [Vec3::new(-0.2, 0.05, 0.0), Vec3::new(0.2, 0.05, 0.0)];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 1, "interior-only cut must create geometry, got {cuts}");
        assert!(m.validate().is_ok(), "valid after interior-only cut");
        assert_eq!(m.face_count(), f_before + 1, "n-gon split: one quad → two faces");
        assert!(no_tangled_faces(&m), "n-gon split leaves no self-intersecting face — no slit spur");
        assert_eq!(m.vertex_count(), v_before + 2, "both clicked points became real vertices");
        for p in [Vec3::new(-0.2, 0.05, 0.0), Vec3::new(0.2, 0.05, 0.0)] {
            assert!(
                (0..m.vertices.len() as u32).any(|v| m.vert(VertexId(v)).half_edge != NO_EDGE
                    && (m.vert(VertexId(v)).position - p).length() < 1e-3),
                "clicked point {p:?} became a real vertex"
            );
        }
        let (verts, tris) = crate::poly_mesh::raster_data_ngon(&m);
        assert!(!verts.is_empty() && !tris.is_empty(), "split mesh still tessellates");
        assert_eq!(tris.len() % 3, 0, "triangle index list well-formed");
    }

    #[test]
    fn knife_through_interior_point_zigzags() {
        // A single grid quad, cut edge-to-edge but routed through an off-line interior point → the shared
        // boundary must bend through that point (through-cut zig-zag). 1 quad → 2 faces, +1 interior vertex.
        let mut m = grid(1); // single quad [-0.5,0.5]^2
        let v_before = m.vertex_count();
        let f_before = m.face_count();
        // path: left edge (-0.5,0) → interior (0.1,0.2) → right edge (0.5,0)
        let poly = [Vec2::new(-0.6, 0.0), Vec2::new(0.1, 0.2), Vec2::new(0.6, 0.0)];
        let poly_3d = [Vec3::new(-0.5, 0.0, 0.0), Vec3::new(0.1, 0.2, 0.0), Vec3::new(0.5, 0.0, 0.0)];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
        assert!(cuts >= 1, "through-interior cut must split the quad, got {cuts}");
        assert!(m.validate().is_ok(), "valid after zig-zag cut");
        assert_eq!(m.face_count(), f_before + 1, "one quad → two faces");
        // the interior vertex exists at the click position
        assert!(
            (0..m.vertices.len() as u32).any(|v| m.vert(VertexId(v)).half_edge != NO_EDGE
                && (m.vert(VertexId(v)).position - Vec3::new(0.1, 0.2, 0.0)).length() < 1e-3),
            "interior click became a real vertex on the cut"
        );
        let _ = v_before;
    }

}
