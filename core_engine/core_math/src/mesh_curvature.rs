//! Discrete **surface** curvature on a [`HalfEdgeMesh`] — the measurement source for material
//! Curvature / Cavity generators on polygonal bodies.
//!
//! Plan: `docs/planning/render/POLY_CURVATURE_PLAN.md` (P1). Prerequisite reading:
//! `docs/planning/render/MESH_CURVATURE_PLAN.md`, which establishes *why* this cannot be the
//! existing field path.
//!
//! ## Why a new kernel and not one of the three that already exist
//!
//! Searched before writing (CLAUDE.md § *No Duplication*). Nothing in `core_math` measures
//! discrete mesh curvature today; the near-misses are all the wrong quantity:
//!
//! | Existing | What it actually measures | Why it cannot serve here |
//! |---|---|---|
//! | `display_mesh_ops::sample_sdf_vertex_curvature` | Laplacian of the **SDF field** at mesh vertex positions | Needs a field. A poly body has none — and near a boolean seam the field's Laplacian is dominated by how the SDF blends, which is the artefact `MESH_CURVATURE_PLAN.md` was written to remove |
//! | `display_mesh_ops::mesh_vertex_importance` | Face-normal **variance**, 0..1 | Unsigned. A cavity and a ridge are indistinguishable, so it cannot drive the Convex / Concave / Signed outputs the reader expects |
//! | `bevel_op::corners` / `nurbs` | Analytic curvature of a specific construction | Not discrete-mesh estimators |
//!
//! ## The estimator
//!
//! For each vertex `v` with geometric normal `n_v`, over its 1-ring neighbours `w`:
//!
//! ```text
//! κ(v) = −mean( dot( normalize(p_w − p_v), n_v ) )
//! ```
//!
//! i.e. how far the neighbourhood sits off the tangent plane. Properties that make it the right
//! choice here, each of which a test below pins:
//!
//! * **Exactly 0 on a flat region** — every neighbour lies in the tangent plane. No epsilon, no
//!   near-zero noise floor to threshold against.
//! * **Signed, with convex positive.** On a convex corner the neighbours fall *below* the tangent
//!   plane, so the raw mean is negative and the leading `−` flips it. This matches the polarity
//!   `shape_curvature_channels` already assumes downstream (`convex = curv.max(0.0)`,
//!   `concave = (−curv).max(0.0)`) with zero change to that reduce step.
//! * **Geometric, never field-derived.** Both `p` and `n_v` come from positions alone (`n_v` is
//!   the Newell area-weighted sum of incident face normals). `MESH_CURVATURE_PLAN.md`'s §"The
//!   trap" records that reaching for a *stored* normal reproduces the field answer and changes
//!   nothing; nothing here reads [`HVertex::normal`].
//! * **Dimensionless.** `normalize()` on the edge vector means a cube edge reads the same whether
//!   the cube is 1 mm or 1 m, so a Radius setting transfers between assets.
//!
//! ## The 1-ring comes from face rings, not `vertex_fan`
//!
//! [`HalfEdgeMesh::vertex_fan`] walks `twin.next` and breaks at `NO_TWIN`, so on a **boundary**
//! vertex it returns only the fan on one side of the boundary — a partial neighbourhood that
//! would bias κ along every open edge. Accumulating from face rings instead is complete for
//! boundary and interior vertices alike, needs no fan walk at all, and falls out of the same
//! single pass that computes the face normals.
//!
//! An interior edge is visited by both of its faces and a boundary edge by one, so interior
//! neighbours carry twice the weight. That is deliberate umbrella weighting (a mild face-incidence
//! proxy for area), not an oversight: it leaves κ unchanged on regular geometry — a cube corner
//! reads `+1/√3` either way, since each of its three neighbours is double-counted uniformly.
//!
//! ## Allocation discipline
//!
//! Per `project_poly_live_feel_bake_dominates`, the cost class on a 40k-quad mesh is per-corner
//! `Vec` allocation, not the arithmetic. Every pass here works over flat preallocated buffers
//! (one CSR adjacency built once and reused by all smoothing iterations); nothing allocates
//! per face, per corner or per vertex.

use crate::half_edge::{newell_normal, FaceId, HalfEdgeMesh, VertexId};
use glam::Vec3;

/// Hard ceiling on smoothing passes. Each pass widens the measured neighbourhood by ~one ring,
/// so this bounds both the cost and how far a Radius can reach before it is just blurring the
/// whole mesh to its mean.
pub const MAX_SMOOTH_ITERS: u32 = 64;

/// A mesh reduced to exactly what a curvature measurement needs: positions plus flattened face
/// rings, where `face_ring[face_start[f] .. face_start[f + 1]]` are face `f`'s vertex indices in
/// winding order.
///
/// This — not [`HalfEdgeMesh`] — is the kernel's real input, for two reasons:
///
/// 1. **One implementation.** [`vertex_curvature`] flattens a half-edge mesh into this form and
///    delegates, so the half-edge path and the flattened-snapshot path cannot drift apart.
/// 2. **It crosses a thread boundary.** The material bake runs on a worker that must not hold a
///    `&HalfEdgeMesh`; the app snapshots this flat form on the frame thread (resolving the
///    corner↔vertex association there, once) and moves it to the worker by value.
///
/// Retired (free-list) and degenerate faces are simply absent — a face with fewer than 3 corners
/// contributes no normal and no adjacency, so callers need no liveness predicate of their own.
#[derive(Clone, Debug, Default)]
pub struct RingMesh {
    pub positions: Vec<Vec3>,
    /// `faces + 1` entries; the last is `face_ring.len()`.
    pub face_start: Vec<u32>,
    pub face_ring: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshCurvatureEdge {
    pub vertices: [u32; 2],
    pub faces: [Option<u32>; 2],
}

pub fn mesh_curvature_edges(rings: &RingMesh) -> Result<Vec<MeshCurvatureEdge>, String> {
    let mut edges = std::collections::BTreeMap::new();
    for face in 0..rings.face_count() {
        let ring = rings.face(face);
        if ring.len() < 3 { continue; }
        for corner in 0..ring.len() {
            let start = ring[corner];
            let end = ring[(corner + 1) % ring.len()];
            if start as usize >= rings.positions.len() || end as usize >= rings.positions.len() {
                return Err(format!("Curvature face {face} references a missing vertex"));
            }
            if start == end { continue; }
            let key = (start.min(end), start.max(end));
            match edges.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(MeshCurvatureEdge {
                        vertices: [start, end], faces: [Some(face as u32), None],
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let edge = entry.get_mut();
                    if edge.faces[0] == Some(face as u32) {
                        return Err(format!("Curvature face {face} repeats edge {key:?}"));
                    }
                    if edge.faces[1].is_some() {
                        return Err(format!("Curvature edge {key:?} is non-manifold"));
                    }
                    if edge.vertices != [end, start] {
                        return Err(format!("Curvature edge {key:?} has inconsistent winding"));
                    }
                    edge.faces[1] = Some(face as u32);
                }
            }
        }
    }
    Ok(edges.into_values().collect())
}

pub fn mesh_curvature_edge_angle(rings: &RingMesh, edge: &MeshCurvatureEdge) -> f32 {
    let [Some(left), Some(right)] = edge.faces else { return 0.0; };
    let normal = |face: u32| {
        let points: Vec<_> = rings.face(face as usize).iter().map(|&vertex| rings.positions[vertex as usize]).collect();
        newell_normal(&points).normalize_or_zero()
    };
    let left_normal = normal(left);
    let right_normal = normal(right);
    if left_normal == Vec3::ZERO || right_normal == Vec3::ZERO { return 0.0; }
    let tangent = (rings.positions[edge.vertices[1] as usize] - rings.positions[edge.vertices[0] as usize]).normalize_or_zero();
    tangent.dot(left_normal.cross(right_normal)).atan2(left_normal.dot(right_normal))
}

impl RingMesh {
    /// Flatten a half-edge mesh's live faces. Vertex indices stay raw `VertexId.0` slots, so
    /// [`Self::positions`] is parallel to `mesh.vertices` and a caller can index results with a
    /// `VertexId` directly.
    pub fn from_half_edge(mesh: &HalfEdgeMesh) -> Self {
        let nf = mesh.faces.len();
        let mut face_start = Vec::with_capacity(nf + 1);
        let mut face_ring: Vec<u32> = Vec::with_capacity(nf * 4);
        let mut scratch: Vec<VertexId> = Vec::with_capacity(8);
        for fi in 0..nf {
            face_start.push(face_ring.len() as u32);
            let f = FaceId(fi as u32);
            if !mesh.is_face_live(f) {
                continue;
            }
            mesh.face_vertices_into(f, &mut scratch);
            if scratch.len() < 3 {
                // Same skip `half_edge::normals::face_normal_and_area` applies.
                continue;
            }
            face_ring.extend(scratch.iter().map(|v| v.0));
        }
        face_start.push(face_ring.len() as u32);
        Self {
            positions: mesh.vertices.iter().map(|v| v.position).collect(),
            face_start,
            face_ring,
        }
    }

    #[inline]
    pub fn face(&self, fi: usize) -> &[u32] {
        &self.face_ring[self.face_start[fi] as usize..self.face_start[fi + 1] as usize]
    }

    #[inline]
    pub fn face_count(&self) -> usize {
        self.face_start.len().saturating_sub(1)
    }

    /// Axis-aligned bounds over positions referenced by at least one live face. Falls back to a
    /// unit cube when there is nothing to bound, matching `SdfFieldSources::body_aabb`.
    pub fn aabb(&self) -> (Vec3, Vec3) {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        let mut any = false;
        for &v in &self.face_ring {
            let p = self.positions[v as usize];
            lo = lo.min(p);
            hi = hi.max(p);
            any = true;
        }
        if any {
            (lo, hi)
        } else {
            (Vec3::splat(-1.0), Vec3::splat(1.0))
        }
    }
}

/// Per-vertex geometric data the curvature pass needs, all derived from positions only.
struct Geometry {
    /// Unit vertex normal — Newell area-weighted sum of incident face normals. `Vec3::ZERO` for a
    /// vertex with no live incident face.
    vnormal: Vec<Vec3>,
    /// CSR 1-ring adjacency with multiplicity (see the module header on umbrella weighting).
    adj_start: Vec<u32>,
    adj: Vec<u32>,
    /// Mean length of the ring edges actually walked. `0.0` for an empty mesh.
    avg_edge: f32,
}

fn build_geometry(rings: &RingMesh) -> Geometry {
    let nv = rings.positions.len();
    let mut vnormal = vec![Vec3::ZERO; nv];
    let mut degree = vec![0u32; nv];
    let mut edge_sum = 0.0f64;
    let mut edge_n = 0u64;

    // Pass A — face normals (unnormalized Newell: its magnitude IS twice the polygon area, so it
    // doubles as the area weight; see `newell_normal`'s doc, the single Newell rule for
    // core_math) accumulated onto their corner vertices, plus the per-vertex ring degree.
    let mut pts: Vec<Vec3> = Vec::with_capacity(8);
    for fi in 0..rings.face_count() {
        let ring = rings.face(fi);
        if ring.len() < 3 {
            continue;
        }
        pts.clear();
        pts.extend(ring.iter().map(|&v| rings.positions[v as usize]));
        let nn = newell_normal(&pts);
        for (i, &v) in ring.iter().enumerate() {
            vnormal[v as usize] += nn;
            degree[v as usize] += 2; // prev + next neighbour
            let nxt = pts[(i + 1) % pts.len()];
            edge_sum += (nxt - pts[i]).length() as f64;
            edge_n += 1;
        }
    }
    for n in vnormal.iter_mut() {
        *n = n.normalize_or_zero();
    }

    // Pass B — CSR offsets from the degrees, then fill.
    let mut adj_start = vec![0u32; nv + 1];
    let mut acc = 0u32;
    for v in 0..nv {
        adj_start[v] = acc;
        acc += degree[v];
    }
    adj_start[nv] = acc;
    let mut adj = vec![0u32; acc as usize];
    let mut cursor = adj_start.clone();
    for fi in 0..rings.face_count() {
        let ring = rings.face(fi);
        let k = ring.len();
        if k < 3 {
            continue;
        }
        for i in 0..k {
            let v = ring[i] as usize;
            let prev = ring[(i + k - 1) % k];
            let next = ring[(i + 1) % k];
            adj[cursor[v] as usize] = prev;
            adj[cursor[v] as usize + 1] = next;
            cursor[v] += 2;
        }
    }

    Geometry {
        vnormal,
        adj_start,
        adj,
        avg_edge: if edge_n > 0 {
            (edge_sum / edge_n as f64) as f32
        } else {
            0.0
        },
    }
}

/// Mean ring-edge length of the mesh — the world-space unit a Radius is measured in.
///
/// Returns `0.0` for a mesh with no live face (the caller then has no scale to convert against
/// and should use 0 smoothing iterations rather than guessing one).
pub fn average_edge_length(mesh: &HalfEdgeMesh) -> f32 {
    average_edge_length_rings(&RingMesh::from_half_edge(mesh))
}

/// [`average_edge_length`] over the flattened form.
pub fn average_edge_length_rings(rings: &RingMesh) -> f32 {
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for fi in 0..rings.face_count() {
        let ring = rings.face(fi);
        if ring.len() < 3 {
            continue;
        }
        for i in 0..ring.len() {
            let a = rings.positions[ring[i] as usize];
            let b = rings.positions[ring[(i + 1) % ring.len()] as usize];
            sum += (b - a).length() as f64;
            n += 1;
        }
    }
    if n > 0 {
        (sum / n as f64) as f32
    } else {
        0.0
    }
}

/// Convert the panel's world-space **Radius** into smoothing iterations.
///
/// Each iteration diffuses κ across one more ring of neighbours, so the measured neighbourhood
/// grows by roughly one average edge length per pass. This is the mapping that makes "Radius up →
/// falloff **widens**" true (the `MESH_CURVATURE_PLAN.md` M3 gate) rather than merely saturating
/// the same band differently.
///
/// Radius semantics therefore differ from the SDF path's finite-difference step, and deliberately
/// so — this is a different measurement, not an approximation of that one.
pub fn smooth_iters_for_radius(avg_edge: f32, radius: f32) -> u32 {
    if !(avg_edge > 0.0) || !(radius > 0.0) {
        return 0;
    }
    ((radius / avg_edge).round() as i64).clamp(0, MAX_SMOOTH_ITERS as i64) as u32
}

/// Signed discrete mean curvature per vertex — **convex positive, concave negative**, 0 on flat.
///
/// The returned vector is indexed by raw vertex slot (`VertexId.0`) and has
/// `mesh.vertices.len()` entries, so a caller can index it with a `VertexId` directly. Retired
/// (free-list) slots and isolated vertices read `0.0`.
///
/// `smooth_iters` diffuses the result over the 1-ring that many times — see
/// [`smooth_iters_for_radius`]. 0 gives the raw per-vertex measurement.
pub fn vertex_curvature(mesh: &HalfEdgeMesh, smooth_iters: u32) -> Vec<f32> {
    vertex_curvature_rings(&RingMesh::from_half_edge(mesh), smooth_iters)
}

/// [`vertex_curvature_rings`] plus the per-vertex geometric normals it already computed
/// internally.
///
/// The triplanar writer needs both, and they come from the same `build_geometry` pass — exposing
/// this pair keeps a caller from running that pass twice (or, worse, deriving normals a second
/// way and having them disagree with the ones κ was measured against).
pub fn vertex_curvature_and_normals_rings(
    rings: &RingMesh,
    smooth_iters: u32,
) -> (Vec<f32>, Vec<Vec3>) {
    let normals = if rings.face_ring.is_empty() {
        vec![Vec3::ZERO; rings.positions.len()]
    } else {
        build_geometry(rings).vnormal
    };
    (vertex_curvature_rings(rings, smooth_iters), normals)
}

/// [`vertex_curvature`] over the flattened form — the single implementation both paths run.
pub fn vertex_curvature_rings(rings: &RingMesh, smooth_iters: u32) -> Vec<f32> {
    let nv = rings.positions.len();
    if nv == 0 || rings.face_ring.is_empty() {
        return vec![0.0; nv];
    }
    let geo = build_geometry(rings);
    let mut kappa = vec![0.0f32; nv];

    // Pass C — the estimator itself.
    for v in 0..nv {
        let n_v = geo.vnormal[v];
        if n_v == Vec3::ZERO {
            continue; // no live incident face: nothing to measure against
        }
        let p_v = rings.positions[v];
        let lo = geo.adj_start[v] as usize;
        let hi = geo.adj_start[v + 1] as usize;
        if hi <= lo {
            continue;
        }
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for &w in &geo.adj[lo..hi] {
            let d = rings.positions[w as usize] - p_v;
            // A coincident neighbour has no direction to speak of. Skipping it (rather than
            // normalizing to garbage) keeps a welded-duplicate vertex from injecting noise.
            let len = d.length();
            if len < 1e-9 {
                continue;
            }
            sum += (d / len).dot(n_v);
            count += 1;
        }
        if count > 0 {
            // Leading `−`: neighbours below the tangent plane (convex) must read POSITIVE.
            kappa[v] = -sum / count as f32;
        }
    }

    // Pass D — 1-ring diffusion. Double-buffered so every vertex in a pass reads the SAME
    // generation; smoothing in place would make the result depend on vertex order.
    if smooth_iters > 0 {
        let iters = smooth_iters.min(MAX_SMOOTH_ITERS);
        let mut next = kappa.clone();
        for _ in 0..iters {
            for v in 0..nv {
                let lo = geo.adj_start[v] as usize;
                let hi = geo.adj_start[v + 1] as usize;
                if hi <= lo {
                    next[v] = kappa[v];
                    continue;
                }
                let mut sum = kappa[v];
                for &w in &geo.adj[lo..hi] {
                    sum += kappa[w as usize];
                }
                next[v] = sum / (1 + hi - lo) as f32;
            }
            std::mem::swap(&mut kappa, &mut next);
        }
    }

    kappa
}

/// [`vertex_curvature`] with the Radius→iterations conversion folded in, so callers that hold a
/// world-space radius do not each re-derive the mapping. Returns `(kappa, avg_edge)`.
pub fn vertex_curvature_for_radius(mesh: &HalfEdgeMesh, radius: f32) -> (Vec<f32>, f32) {
    let avg = average_edge_length(mesh);
    let iters = smooth_iters_for_radius(avg, radius);
    (vertex_curvature(mesh, iters), avg)
}

/// Triangulate a [`RingMesh`]'s faces into a flat index list, matching the ear-clip the raster
/// emitters use. Shared by the mesh-map baker (BLAS build) and its tests.
pub fn triangulate_rings(rings: &RingMesh) -> Vec<u32> {
    let mut out = Vec::new();
    let mut pts: Vec<Vec3> = Vec::with_capacity(8);
    for fi in 0..rings.face_count() {
        let ring = rings.face(fi);
        if ring.len() < 3 {
            continue;
        }
        pts.clear();
        pts.extend(ring.iter().map(|&v| rings.positions[v as usize]));
        let n = newell_normal(&pts).normalize_or_zero();
        for t in crate::poly_mesh::earclip_polygon(&pts, n) {
            out.push(ring[t[0] as usize]);
            out.push(ring[t[1] as usize]);
            out.push(ring[t[2] as usize]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::half_edge::HalfEdgeMesh;
    use glam::Vec3;

    /// GATE (AO) — occlusion is real: a point inside a closed box is fully occluded, a point on an
    /// isolated open plane is fully open.
    ///
    /// These are the two ends an AO bake must get right, and both failure modes look plausible: a
    /// query that never hits bakes uniform white, one that always hits bakes uniform black. The
    /// gate also pins the SELF-INTERSECTION bias — without an origin offset along the normal every
    /// texel instantly hits its own triangle and the whole map goes black.
    #[test]
    fn ao_hemisphere_is_open_on_a_plane_and_closed_in_a_box() {
        use crate::bvh::{blas_any_hit, build_tri_blas};

        let trace = |positions: &[[f32; 3]], indices: &[u32], o: Vec3, n: Vec3, len: f32| -> f32 {
            let blas = build_tri_blas(positions, indices);
            let up = if n.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
            let t1 = n.cross(up).normalize_or_zero();
            let t2 = n.cross(t1);
            let rays = 64;
            let bias = 1.0e-3;
            let orig = o + n * bias;
            let mut open = 0;
            for i in 0..rays {
                let t = (i as f32 + 0.5) / rays as f32;
                let sin_t = t.sqrt();
                let cos_t = (1.0 - sin_t * sin_t).max(0.0).sqrt();
                let phi = i as f32 * 2.399_963_2;
                let d = t1 * (sin_t * phi.cos()) + t2 * (sin_t * phi.sin()) + n * cos_t;
                if !blas_any_hit(
                    &blas, positions, indices,
                    [orig.x, orig.y, orig.z], [d.x, d.y, d.z], len,
                ) {
                    open += 1;
                }
            }
            open as f32 / rays as f32
        };

        // Open plane: nothing above it → fully unoccluded.
        let plane_p = vec![
            [-1.0f32, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0],
        ];
        let plane_i: Vec<u32> = vec![0, 1, 2, 0, 2, 3];
        let ao_plane = trace(&plane_p, &plane_i, Vec3::ZERO, Vec3::Y, 10.0);
        assert!(
            ao_plane > 0.99,
            "an isolated plane must bake fully open (got {ao_plane}) — anything lower means the \
             ray origin is not lifted off its own triangle"
        );

        // Inside a closed box: every ray blocked.
        let h = 1.0f32;
        let box_p: Vec<[f32; 3]> = vec![
            [-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h],
            [-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h],
        ];
        let quads = [
            [0usize, 1, 2, 3], [5, 4, 7, 6], [4, 5, 1, 0],
            [3, 2, 6, 7], [1, 5, 6, 2], [4, 0, 3, 7],
        ];
        let mut box_i: Vec<u32> = Vec::new();
        for q in quads {
            box_i.extend_from_slice(&[q[0] as u32, q[1] as u32, q[2] as u32]);
            box_i.extend_from_slice(&[q[0] as u32, q[2] as u32, q[3] as u32]);
        }
        // Floor of the box, normal up — enclosed on all sides.
        let ao_box = trace(&box_p, &box_i, Vec3::new(0.0, -h, 0.0), Vec3::Y, 10.0);
        assert!(
            ao_box < 0.05,
            "a point on the floor of a CLOSED box must bake nearly black (got {ao_box})"
        );
        assert!(
            ao_plane - ao_box > 0.9,
            "open and enclosed must be at opposite ends of the range ({ao_plane} vs {ao_box})"
        );
    }

    #[test]
    fn signed_edge_curvature_separates_cube_creases_from_flat_seams() {
        let rings = RingMesh::from_half_edge(&subdivided_cube(1, 2.0));
        let edges = mesh_curvature_edges(&rings).unwrap();
        assert_eq!(edges.len(), 12);
        for edge in &edges {
            assert!((mesh_curvature_edge_angle(&rings, edge) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        }
        let mut inward = rings.clone();
        for face in 0..inward.face_count() {
            let start = inward.face_start[face] as usize;
            let end = inward.face_start[face + 1] as usize;
            inward.face_ring[start..end].reverse();
        }
        for edge in mesh_curvature_edges(&inward).unwrap() {
            assert!(mesh_curvature_edge_angle(&inward, &edge) < 0.0);
        }
        let plane = HalfEdgeMesh::from_polygons(&[Vec3::ZERO, Vec3::X, Vec3::X + Vec3::Y, Vec3::Y], &[vec![0, 1, 2], vec![0, 2, 3]]);
        let plane = RingMesh::from_half_edge(&plane);
        for edge in mesh_curvature_edges(&plane).unwrap() {
            assert_eq!(mesh_curvature_edge_angle(&plane, &edge), 0.0);
        }
    }

    #[test]
    fn curvature_topology_rejects_non_manifold_input() {
        let rings = RingMesh {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z],
            face_start: vec![0, 3, 6, 9],
            face_ring: vec![0, 1, 2, 1, 0, 3, 0, 1, 4],
        };
        assert!(mesh_curvature_edges(&rings).unwrap_err().contains("non-manifold"));
    }

    /// Axis-aligned box as quads, `n × n` per face (n = 1 is a plain 6-quad cube).
    ///
    /// Built through `from_polygons` because this is a TEST FIXTURE, not a modelling op — the
    /// ID-stability rule that prefers real half-edge ops applies to operations on a user's mesh.
    fn subdivided_cube(n: usize, size: f32) -> HalfEdgeMesh {
        let h = size * 0.5;
        let mut pos: Vec<Vec3> = Vec::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        let mut index = std::collections::HashMap::<(i64, i64, i64), usize>::new();
        // Weld key is quantized RELATIVE to `size`. An absolute key (`x * 1e5`) works at size 1
        // but splits shared corners at size 1000, where f32 rounding makes the same corner
        // computed from two different faces differ in the last ulp — the fixture would then hand
        // the scale-invariance test two meshes of different topology and "prove" a kernel bug
        // that is not there.
        let q = move |x: f32| (x / size * 1.0e4).round() as i64;
        let mut push = |p: Vec3, pos: &mut Vec<Vec3>, index: &mut std::collections::HashMap<(i64, i64, i64), usize>| -> usize {
            let k = (q(p.x), q(p.y), q(p.z));
            *index.entry(k).or_insert_with(|| {
                pos.push(p);
                pos.len() - 1
            })
        };
        // (origin, edge_u, edge_v) per face, wound CCW when viewed from outside.
        let sides = [
            (Vec3::new(-h, -h, h), Vec3::X, Vec3::Y),   // +Z
            (Vec3::new(h, -h, -h), -Vec3::X, Vec3::Y),  // -Z
            (Vec3::new(h, -h, h), -Vec3::Z, Vec3::Y),   // +X
            (Vec3::new(-h, -h, -h), Vec3::Z, Vec3::Y),  // -X
            (Vec3::new(-h, h, h), Vec3::X, -Vec3::Z),   // +Y
            (Vec3::new(-h, -h, -h), Vec3::X, Vec3::Z),  // -Y
        ];
        let s = size / n as f32;
        for (o, du, dv) in sides {
            for i in 0..n {
                for j in 0..n {
                    let a = o + du * (i as f32 * s) + dv * (j as f32 * s);
                    let b = a + du * s;
                    let c = a + du * s + dv * s;
                    let d = a + dv * s;
                    faces.push(vec![
                        push(a, &mut pos, &mut index),
                        push(b, &mut pos, &mut index),
                        push(c, &mut pos, &mut index),
                        push(d, &mut pos, &mut index),
                    ]);
                }
            }
        }
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    /// UV sphere as quads (+ triangle fans at the poles).
    fn uv_sphere(radius: f32, rings: usize, segs: usize) -> HalfEdgeMesh {
        let mut pos = vec![Vec3::new(0.0, radius, 0.0)];
        for r in 1..rings {
            let phi = std::f32::consts::PI * r as f32 / rings as f32;
            for s in 0..segs {
                let th = std::f32::consts::TAU * s as f32 / segs as f32;
                pos.push(Vec3::new(
                    radius * phi.sin() * th.cos(),
                    radius * phi.cos(),
                    radius * phi.sin() * th.sin(),
                ));
            }
        }
        pos.push(Vec3::new(0.0, -radius, 0.0));
        let bottom = pos.len() - 1;
        let ring_at = |r: usize, s: usize| -> usize { 1 + (r - 1) * segs + (s % segs) };
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for s in 0..segs {
            faces.push(vec![0, ring_at(1, s + 1), ring_at(1, s)]);
        }
        for r in 1..rings - 1 {
            for s in 0..segs {
                faces.push(vec![
                    ring_at(r, s),
                    ring_at(r, s + 1),
                    ring_at(r + 1, s + 1),
                    ring_at(r + 1, s),
                ]);
            }
        }
        for s in 0..segs {
            faces.push(vec![bottom, ring_at(rings - 1, s), ring_at(rings - 1, s + 1)]);
        }
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    /// GATE (P1) — flat reads ~0, all 12 edges read high, and the result is a real SPREAD.
    ///
    /// The spread half is the non-vacuity check `MESH_CURVATURE_PLAN.md` M2 specifies: a kernel
    /// that returned one constant for "edge" and another for "flat" would satisfy a naive
    /// edges > flats assertion while being useless as a mask.
    #[test]
    fn p1_cube_flats_are_zero_and_edges_are_high() {
        let mesh = subdivided_cube(4, 2.0);
        let k = vertex_curvature(&mesh, 0);
        let h = 1.0f32;
        let on = |c: f32| (c.abs() - h).abs() < 1e-4;

        let mut flats = Vec::new();
        let mut edges = Vec::new();
        let mut corners = Vec::new();
        for (i, v) in mesh.vertices.iter().enumerate() {
            if !mesh.is_vert_live(VertexId(i as u32)) {
                continue;
            }
            let p = v.position;
            match [on(p.x), on(p.y), on(p.z)].iter().filter(|b| **b).count() {
                1 => flats.push(k[i]),
                2 => edges.push(k[i]),
                3 => corners.push(k[i]),
                _ => {}
            }
        }
        assert!(!flats.is_empty() && !edges.is_empty() && corners.len() == 8);

        for f in &flats {
            assert!(
                f.abs() < 1e-5,
                "a vertex interior to a flat face must read exactly 0 (got {f})"
            );
        }
        let edge_min = edges.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!(
            edge_min > 0.2,
            "every one of the 12 edges must read high and CONVEX-positive (min {edge_min})"
        );
        let corner_min = corners.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!(
            corner_min > edge_min,
            "a 3-face corner is sharper than a 2-face edge ({corner_min} vs {edge_min})"
        );
        // Real spread, not a two-value step: flat/edge/corner are three distinct levels.
        assert!(
            (corner_min - edge_min).abs() > 0.05,
            "edge and corner must be separated, not quantised to one 'sharp' value"
        );
    }

    /// GATE (P1) — concave reads NEGATIVE, and the sign convention is the one the downstream
    /// reduce step (`convex = curv.max(0)`, `concave = (-curv).max(0)`) already assumes.
    ///
    /// An inverted cube (windings reversed) is the same geometry seen from inside, so every edge
    /// that read convex must now read concave with the same magnitude. If someone "simplifies"
    /// away the leading `−` in the estimator, this flips and the Convex/Concave buttons swap.
    #[test]
    fn p1_sign_convention_convex_positive_concave_negative() {
        let outward = subdivided_cube(3, 2.0);
        let mut pos: Vec<Vec3> = Vec::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for (i, v) in outward.vertices.iter().enumerate() {
            assert!(outward.is_vert_live(VertexId(i as u32)));
            pos.push(v.position);
        }
        for fi in 0..outward.faces.len() {
            let f = FaceId(fi as u32);
            if !outward.is_face_live(f) {
                continue;
            }
            let mut ring: Vec<usize> = outward.face_vertices(f).iter().map(|v| v.0 as usize).collect();
            ring.reverse(); // flip winding → normals point inward → every edge is now concave
            faces.push(ring);
        }
        let inward = HalfEdgeMesh::from_polygons(&pos, &faces);

        let ko = vertex_curvature(&outward, 0);
        let ki = vertex_curvature(&inward, 0);
        let mut checked = 0;
        for i in 0..pos.len() {
            if ko[i].abs() < 1e-4 {
                continue; // flat: 0 either way, nothing to compare
            }
            assert!(ko[i] > 0.0, "outward cube edge/corner must be CONVEX (+): {}", ko[i]);
            assert!(ki[i] < 0.0, "inverted cube edge/corner must be CONCAVE (−): {}", ki[i]);
            assert!(
                (ko[i] + ki[i]).abs() < 1e-4,
                "same geometry, opposite orientation → equal magnitude, opposite sign ({} vs {})",
                ko[i],
                ki[i]
            );
            checked += 1;
        }
        assert!(checked >= 20, "expected the cube's edges+corners to be compared (got {checked})");
    }

    /// GATE (P1) — on a sphere κ is near-uniform positive, with variance under a fixed bound.
    ///
    /// This is the second `MESH_CURVATURE_PLAN.md` M2 non-vacuity check: a kernel keyed on
    /// dihedral angle alone drifts across the rings of a UV sphere (pole quads are much smaller
    /// than equator quads), and that drift would paint latitude banding on any sphere.
    #[test]
    fn p1_sphere_curvature_is_uniform_positive() {
        let mesh = uv_sphere(1.0, 16, 24);
        let k = vertex_curvature(&mesh, 0);
        // Equator band only: the pole fans are triangles of a different valence, and comparing
        // them to quads would be measuring tessellation, not the surface.
        let vals: Vec<f32> = mesh
            .vertices
            .iter()
            .enumerate()
            .filter(|(i, v)| mesh.is_vert_live(VertexId(*i as u32)) && v.position.y.abs() < 0.5)
            .map(|(i, _)| k[i])
            .collect();
        assert!(vals.len() > 100, "expected a populated equator band (got {})", vals.len());
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        assert!(mean > 0.0, "a sphere is convex everywhere (mean {mean})");
        let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / vals.len() as f32;
        assert!(
            var / (mean * mean) < 0.05,
            "κ must be near-uniform over the sphere (relative variance {})",
            var / (mean * mean)
        );
    }

    /// GATE (P1) — the estimator is scale-INVARIANT.
    ///
    /// The same cube at 1 unit and at 1000 units must produce identical κ, so a Radius that works
    /// on a bolt still works on a wall. This is what `normalize()` on the edge vector buys, and
    /// it is the property `CurvConstants`' `min_axis` machinery exists to approximate on the SDF
    /// side — here it is exact.
    #[test]
    fn p1_curvature_is_scale_invariant() {
        let small = vertex_curvature(&subdivided_cube(3, 0.001), 0);
        let large = vertex_curvature(&subdivided_cube(3, 1000.0), 0);
        assert_eq!(small.len(), large.len());
        for (a, b) in small.iter().zip(large.iter()) {
            assert!(
                (a - b).abs() < 1e-4,
                "κ must not depend on mesh scale ({a} vs {b})"
            );
        }
    }

    /// GATE (P1 / M3) — Radius WIDENS the falloff rather than only rescaling it.
    ///
    /// Measured as the count of vertices carrying a non-trivial fraction of the peak: smoothing
    /// must spread κ off the edges onto the flats. A kernel where Radius merely multiplied the
    /// result would leave this count unchanged, which is the M3 failure this pins.
    #[test]
    fn p1_radius_widens_the_band() {
        let mesh = subdivided_cube(6, 2.0);
        let width = |iters: u32| -> usize {
            let k = vertex_curvature(&mesh, iters);
            let peak = k.iter().cloned().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(peak > 0.0);
            k.iter().filter(|v| v.abs() > peak * 0.15).count()
        };
        let w0 = width(0);
        let w2 = width(2);
        let w6 = width(6);
        assert!(w2 > w0, "2 iterations must reach more vertices than 0 ({w2} vs {w0})");
        assert!(w6 > w2, "6 iterations must reach more than 2 ({w6} vs {w2})");
    }

    /// GATE (P1) — Radius→iterations is a real world-space mapping, not a magic constant.
    #[test]
    fn p1_radius_maps_through_average_edge_length() {
        let mesh = subdivided_cube(4, 2.0);
        let avg = average_edge_length(&mesh);
        assert!((avg - 0.5).abs() < 1e-5, "4 subdivisions of a size-2 cube → 0.5 (got {avg})");
        assert_eq!(smooth_iters_for_radius(avg, 0.0), 0, "zero radius → no smoothing");
        assert_eq!(smooth_iters_for_radius(avg, 1.0), 2, "one radius = two average edges");
        assert_eq!(smooth_iters_for_radius(avg, 1.0e9), MAX_SMOOTH_ITERS, "clamped, never unbounded");
        assert_eq!(smooth_iters_for_radius(0.0, 1.0), 0, "no scale to measure against → no guess");
    }

    /// GATE (P1) — degenerate inputs return the right LENGTH and no NaN, rather than panicking.
    ///
    /// The bake calls this on whatever the modifier stack produced, which on a bad stack can be
    /// an empty or face-less mesh.
    #[test]
    fn p1_degenerate_meshes_are_finite_and_correctly_sized() {
        let empty = HalfEdgeMesh::from_polygons(&[], &[]);
        assert!(vertex_curvature(&empty, 4).is_empty());

        // Positions but no faces: every vertex is isolated → 0, never NaN from a 0/0 mean.
        let loose = HalfEdgeMesh::from_polygons(&[Vec3::ZERO, Vec3::X, Vec3::Y], &[]);
        let k = vertex_curvature(&loose, 4);
        assert_eq!(k.len(), loose.vertices.len());
        assert!(k.iter().all(|v| *v == 0.0), "isolated vertices read 0");

        // Coincident vertices in a ring: the zero-length-edge guard must hold.
        let degen = HalfEdgeMesh::from_polygons(
            &[Vec3::ZERO, Vec3::ZERO, Vec3::X, Vec3::Y],
            &[vec![0usize, 1, 2], vec![0, 2, 3]],
        );
        assert!(
            vertex_curvature(&degen, 2).iter().all(|v| v.is_finite()),
            "a zero-length ring edge must not produce NaN"
        );
    }
}
