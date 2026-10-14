// Mesh generators + converters (split from half_edge.rs — pure relocation).

use super::*;

/// Hasher for packed `(src_vertex << 32 | dst_vertex)` twin-link keys in the mesh builders.
/// SipHash dominated `from_quads`/`from_triangles` on remesh-sized meshes (~150k map ops on
/// a 37k-quad Surface-Nets extract); a splitmix64 finalizer gives full avalanche on the
/// sequential-id keys at a fraction of the cost. Construction-time only — never serialized.
#[derive(Default)]
struct EdgeKeyHasher(u64);

impl std::hash::Hasher for EdgeKeyHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _bytes: &[u8]) {
        unreachable!("EdgeKeyHasher is for u64 keys only");
    }
    #[inline]
    fn write_u64(&mut self, k: u64) {
        // splitmix64 finalizer.
        let mut z = k.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        self.0 = z ^ (z >> 31);
    }
}

// (The `EdgeMap` alias that used to live here was the single-twin map for `from_quads` /
// `from_triangles`; both now use `TwinLinker` below. `EdgeKeyHasher` + `edge_key` survive because
// `from_polygons` still keys a MULTI-map — `Vec<HalfEdgeId>` per edge — on the same packed key, and
// that map's INVARIANT is unchanged: the key type must hash via a single `write_u64`, since any
// other key type routes through `Hasher::write` and panics at the first insert.)

#[inline]
fn edge_key(src: u32, dst: u32) -> u64 {
    ((src as u64) << 32) | dst as u64
}

/// Collapse `[a,b,c,a]` (octree T-junction triangle) to a 3-ring; keep a real quad.
fn compact_dc_ring(q: [u32; 4]) -> Option<Vec<usize>> {
    let mut r: Vec<usize> = Vec::with_capacity(4);
    for i in 0..4 {
        let v = q[i] as usize;
        if r.last().copied() != Some(v) {
            r.push(v);
        }
    }
    if r.len() >= 2 && r[0] == r[r.len() - 1] {
        r.pop();
    }
    if r.len() < 3 {
        return None;
    }
    for i in 0..r.len() {
        for j in (i + 1)..r.len() {
            if r[i] == r[j] {
                return None;
            }
        }
    }
    Some(r)
}

/// Sentinel in [`TwinLinker::he`] for a slot whose half-edge has already been twinned. Twinning
/// is what the old map's `remove` modelled, so a consumed slot means "this key is absent".
const TWIN_CONSUMED: u32 = u32::MAX;

/// Direct-addressed replacement for the construction-time `(src,dst) -> HalfEdgeId` hash map.
///
/// The hash map was never bound by hashing — `EdgeKeyHasher` is a single `write_u64` — but by cache
/// misses: at res 256 the remesh feeds 295k half-edges through a map peaking near 147k live entries,
/// and it measured 6.0 ms of `from_mesh`'s 9.9 ms. Here the edges leaving a vertex are contiguous,
/// so a lookup touches one cache line and scans ~valence (≈4) entries.
///
/// **The semantics below are load-bearing and must not be "simplified".** They reproduce the
/// original pair of operations exactly, including the non-manifold behaviour that several callers
/// have baked in:
/// * `map.remove(&edge_key(dst, src))` — consume the OPPOSITE directed edge if one is live.
/// * else `map.entry(edge_key(src, dst)).or_insert(he)` — register this edge only if that key has
///   no LIVE entry. `or_insert` means the FIRST writer wins: when three half-edges share an edge,
///   the first two encountered pair up and the third is re-registered (or dropped untwinned,
///   depending on direction) rather than stealing the pairing.
///
/// A key is "live" iff some slot in the source vertex's bucket holds it un-consumed. `or_insert`
/// guarantees at most one live slot per key, so `remove` scanning forward for the first live match
/// is unambiguous.
struct TwinLinker {
    /// Bucket bounds per vertex, `len() == vertex_count + 1` (prefix sum of out-degree).
    start: Vec<u32>,
    /// Slots written so far in each bucket (consumed ones still occupy their slot).
    len: Vec<u32>,
    dst: Vec<u32>,
    he: Vec<u32>,
}

impl TwinLinker {
    /// `out_degree[v]` must be the EXACT number of directed edges leaving `v` over the whole input,
    /// so every bucket is pre-sized and no push can ever spill into a neighbour's range.
    fn new(out_degree: &[u32]) -> Self {
        let mut start = Vec::with_capacity(out_degree.len() + 1);
        let mut acc = 0u32;
        for &d in out_degree {
            start.push(acc);
            acc += d;
        }
        start.push(acc);
        Self {
            start,
            len: vec![0; out_degree.len()],
            dst: vec![0; acc as usize],
            he: vec![TWIN_CONSUMED; acc as usize],
        }
    }

    /// Returns `Some(twin)` when `he` pairs with an already-registered opposite edge, else `None`
    /// after registering `he` (or after finding the key already live, which drops `he` untwinned —
    /// `or_insert` semantics). `src`/`dst` are POSITION indices, not `VertexId`s: the two coincide
    /// on a freshly built mesh, but keying on the input indices keeps that a coincidence rather
    /// than a dependency.
    #[inline]
    fn link(&mut self, src: u32, dst: u32, he: HalfEdgeId) -> Option<HalfEdgeId> {
        debug_assert_ne!(he.0, TWIN_CONSUMED, "a real half-edge id can never be the sentinel");
        // 1. remove(dst -> src)
        let s = self.start[dst as usize] as usize;
        let e = s + self.len[dst as usize] as usize;
        for k in s..e {
            if self.dst[k] == src && self.he[k] != TWIN_CONSUMED {
                let twin = self.he[k];
                self.he[k] = TWIN_CONSUMED;
                return Some(HalfEdgeId(twin));
            }
        }
        // 2. entry(src -> dst).or_insert(he)
        let s2 = self.start[src as usize] as usize;
        let e2 = s2 + self.len[src as usize] as usize;
        for k in s2..e2 {
            if self.dst[k] == dst && self.he[k] != TWIN_CONSUMED {
                return None; // key already live — first writer wins, `he` stays untwinned
            }
        }
        debug_assert!(
            e2 < self.start[src as usize + 1] as usize,
            "bucket overflow: out_degree undercounted vertex {src}"
        );
        self.dst[e2] = dst;
        self.he[e2] = he.0;
        self.len[src as usize] += 1;
        None
    }
}

impl HalfEdgeMesh {
    // ============================================================
    // Mesh generators
    // ============================================================

    /// Create a box with given half-extents.
    pub fn new_box(half_extents: Vec3) -> Self {
        let he = half_extents;
        let positions = [
            Vec3::new(-he.x, -he.y, -he.z), // 0: bottom-left-back
            Vec3::new( he.x, -he.y, -he.z), // 1: bottom-right-back
            Vec3::new( he.x, -he.y,  he.z), // 2: bottom-right-front
            Vec3::new(-he.x, -he.y,  he.z), // 3: bottom-left-front
            Vec3::new(-he.x,  he.y, -he.z), // 4: top-left-back
            Vec3::new( he.x,  he.y, -he.z), // 5: top-right-back
            Vec3::new( he.x,  he.y,  he.z), // 6: top-right-front
            Vec3::new(-he.x,  he.y,  he.z), // 7: top-left-front
        ];

        // 6 quad faces (CCW winding when viewed from outside — outward normals)
        let quads: [[usize; 4]; 6] = [
            [0, 1, 2, 3], // bottom (-Y)
            [7, 6, 5, 4], // top (+Y)
            [4, 5, 1, 0], // back (-Z)
            [6, 7, 3, 2], // front (+Z)
            [7, 4, 0, 3], // left (-X)
            [5, 6, 2, 1], // right (+X)
        ];

        Self::from_quads(&positions, &quads)
    }

    /// Create a plane in XZ with given dimensions and subdivisions.
    pub fn new_plane(width: f32, depth: f32, subdivs_w: u32, subdivs_d: u32) -> Self {
        let sw = subdivs_w.max(1);
        let sd = subdivs_d.max(1);
        let mut positions = Vec::new();
        let hw = width * 0.5;
        let hd = depth * 0.5;

        for iz in 0..=sd {
            for ix in 0..=sw {
                let x = -hw + (ix as f32 / sw as f32) * width;
                let z = -hd + (iz as f32 / sd as f32) * depth;
                positions.push(Vec3::new(x, 0.0, z));
            }
        }

        let mut quads = Vec::new();
        for iz in 0..sd {
            for ix in 0..sw {
                let a = iz * (sw + 1) + ix;
                let b = a + 1;
                let c = a + sw + 2;
                let d = a + sw + 1;
                quads.push([a as usize, d as usize, c as usize, b as usize]);
            }
        }

        Self::from_quads(&positions, &quads)
    }

    /// Create a UV sphere.
    pub fn new_uv_sphere(radius: f32, segments: u32, rings: u32) -> Self {
        let segs = segments.max(3);
        let rng = rings.max(2);
        let mut positions = Vec::new();

        // Top pole
        positions.push(Vec3::new(0.0, radius, 0.0));

        // Rings (excluding poles)
        for r in 1..rng {
            let phi = std::f32::consts::PI * r as f32 / rng as f32;
            let y = radius * phi.cos();
            let ring_r = radius * phi.sin();
            for s in 0..segs {
                let theta = 2.0 * std::f32::consts::PI * s as f32 / segs as f32;
                positions.push(Vec3::new(ring_r * theta.cos(), y, ring_r * theta.sin()));
            }
        }

        // Bottom pole
        let bottom = positions.len();
        positions.push(Vec3::new(0.0, -radius, 0.0));

        // Blender-parity topology (owner 2026-07-23 "basic sphere has bad topo"): the body is
        // QUADS (loop cut / bevel / subdivision workflows need quad bands), only the two pole
        // caps are triangle fans — exactly Blender's UV Sphere. The old builder triangulated
        // everything (`from_triangles`), splitting every band quad with a diagonal.
        let mut faces: Vec<Vec<usize>> = Vec::new();

        // Top cap triangle fan.
        //
        // WINDING (owner soak FAIL 2026-07-29): both cap fans used to be wound the same way as the
        // quad band traverses its shared ring edge, so adjacent faces wound IDENTICALLY instead of
        // oppositely and `is_manifold` reported "directed edge 167->166 used by 2 faces (same
        // winding)". 20 of 25 tested segment/ring combinations were non-manifold — every case with
        // rings >= 3. rings == 2 has no quad band at all, which is why the bug could hide.
        //
        // The caps are flipped (not the band): the band's winding is the one that produces OUTWARD
        // normals. Flipping the band instead makes the mesh manifold AND inside-out — all 192 faces
        // inward — which is a worse bug that renders as a black sphere. Both properties are gated
        // (`new_uv_sphere_is_manifold_*` + `new_uv_sphere_faces_point_outward`).
        for s in 0..segs {
            let next = (s + 1) % segs;
            faces.push(vec![0, (1 + next) as usize, (1 + s) as usize]);
        }

        // Middle QUAD bands
        for r in 0..(rng - 2) {
            let row0 = 1 + r * segs;
            let row1 = 1 + (r + 1) * segs;
            for s in 0..segs {
                let next = (s + 1) % segs;
                // The quad band is the ORIENTATION REFERENCE: its winding is the one that yields
                // outward-facing normals (verified against face centroids by
                // `new_uv_sphere_faces_point_outward`). The two cap fans below are wound to agree
                // with it.
                faces.push(vec![
                    (row0 + s) as usize,
                    (row0 + next) as usize,
                    (row1 + next) as usize,
                    (row1 + s) as usize,
                ]);
            }
        }

        // Bottom cap triangle fan
        let last_ring = 1 + (rng - 2) * segs;
        for s in 0..segs {
            let next = (s + 1) % segs;
            faces.push(vec![(last_ring + next) as usize, bottom, (last_ring + s) as usize]);
        }

        Self::from_polygons(&positions, &faces)
    }

    /// Create a cylinder.
    /// Capped cylinder (Blender-style): `segments` side quads + a single N-GON cap on each end (Blender's
    /// default cap fill). All faces wound for OUTWARD normals. Built via `from_polygons` (twins by reverse
    /// direction), so the rim edges are shared between the cap n-gon and the wall — a clean, single-n-gon
    /// rim that `edge_select::edge_loop` selects all the way around.
    pub fn new_cylinder(radius: f32, height: f32, segments: u32) -> Self {
        let segs = segments.max(3) as usize;
        let hh = height * 0.5;
        let tau = std::f32::consts::TAU;
        let mut positions: Vec<Vec3> = Vec::with_capacity(segs * 2);
        for s in 0..segs { let t = tau * s as f32 / segs as f32; positions.push(Vec3::new(radius * t.cos(), -hh, radius * t.sin())); } // bottom ring 0..segs-1
        for s in 0..segs { let t = tau * s as f32 / segs as f32; positions.push(Vec3::new(radius * t.cos(),  hh, radius * t.sin())); } // top ring   segs..2*segs-1

        let mut faces: Vec<Vec<usize>> = Vec::with_capacity(segs + 2);
        // Side quads (outward): up the left rail, across the top, down the right rail, back along the bottom.
        for s in 0..segs {
            let n = (s + 1) % segs;
            faces.push(vec![s, segs + s, segs + n, n]);
        }
        // Top cap n-gon (+Y outward = decreasing theta) and bottom cap n-gon (-Y outward = increasing theta);
        // wound opposite to the side quads' rim traversal so the rim edges twin (watertight).
        faces.push((segs..2 * segs).rev().collect());
        faces.push((0..segs).collect());
        Self::from_polygons(&positions, &faces)
    }

    /// Sweep a circular profile along an arbitrary polyline -- the KNIFE PIPE (owner
    /// 2026-08-04: knife `P` turns the drawn polyline into a tube; drag = radius,
    /// scroll = segments).
    ///
    /// Framing is PARALLEL TRANSPORT: each ring's frame is the previous ring's frame
    /// rotated by the minimal rotation between consecutive tangents. A naive
    /// "up-cross-tangent" frame flips when the path turns near vertical and twists the tube
    /// inside out; transport carries the frame smoothly and degrades gracefully on a
    /// straight path. Corners are mitred by averaging the incoming/outgoing tangents, so a
    /// sharp polyline corner keeps a constant cross-section instead of pinching.
    ///
    /// `closed` welds the last ring to the first (a loop drawn back onto its start);
    /// `caps` closes an OPEN path's two ends with n-gons. Returns an empty mesh when the
    /// path is degenerate (<2 distinct points).
    pub fn new_pipe(path: &[Vec3], radius: f32, segments: u32, closed: bool, caps: bool) -> Self {
        let segs = segments.max(3) as usize;
        // Drop consecutive duplicates -- a zero-length span has no tangent.
        let mut pts: Vec<Vec3> = Vec::with_capacity(path.len());
        for &p in path {
            if pts.last().map_or(true, |l: &Vec3| l.distance(p) > 1e-6) {
                pts.push(p);
            }
        }
        if closed && pts.len() > 2 && pts[0].distance(*pts.last().unwrap()) <= 1e-6 {
            pts.pop(); // the ring closes implicitly; drop the duplicated seam point
        }
        if pts.len() < 2 {
            return Self::from_polygons(&[], &[]);
        }
        let n = pts.len();

        // Per-point tangent: mitre at interior corners, one-sided at open ends.
        let seg_dir = |i: usize| -> Vec3 { (pts[(i + 1) % n] - pts[i]).normalize_or_zero() };
        let mut tangents: Vec<Vec3> = Vec::with_capacity(n);
        for i in 0..n {
            let t = if closed {
                (seg_dir((i + n - 1) % n) + seg_dir(i)).normalize_or_zero()
            } else if i == 0 {
                seg_dir(0)
            } else if i == n - 1 {
                (pts[n - 1] - pts[n - 2]).normalize_or_zero()
            } else {
                ((pts[i] - pts[i - 1]).normalize_or_zero() + seg_dir(i)).normalize_or_zero()
            };
            tangents.push(if t == Vec3::ZERO { Vec3::Z } else { t });
        }

        // Parallel-transport the frame along the path.
        let seed = if tangents[0].dot(Vec3::Y).abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let mut u = seed.cross(tangents[0]).normalize_or_zero();
        if u == Vec3::ZERO {
            u = Vec3::X;
        }
        let mut frames: Vec<(Vec3, Vec3)> = Vec::with_capacity(n);
        for i in 0..n {
            if i > 0 {
                // Rotate u by the minimal rotation taking tangent[i-1] to tangent[i].
                let (a, b) = (tangents[i - 1], tangents[i]);
                let axis = a.cross(b);
                let sin = axis.length();
                if sin > 1e-6 {
                    let angle = sin.min(1.0).asin() * if a.dot(b) < 0.0 { -1.0 } else { 1.0 };
                    let q = glam::Quat::from_axis_angle(axis / sin, angle);
                    u = q * u;
                }
                // Re-orthogonalise against drift.
                u = (u - tangents[i] * u.dot(tangents[i])).normalize_or_zero();
                if u == Vec3::ZERO {
                    u = if tangents[i].dot(Vec3::Y).abs() > 0.9 { Vec3::X } else { Vec3::Y }
                        .cross(tangents[i])
                        .normalize_or_zero();
                }
            }
            let v = tangents[i].cross(u).normalize_or_zero();
            frames.push((u, v));
        }

        let tau = std::f32::consts::TAU;
        let mut positions: Vec<Vec3> = Vec::with_capacity(n * segs);
        for i in 0..n {
            let (u, v) = frames[i];
            for s in 0..segs {
                let a = tau * s as f32 / segs as f32;
                positions.push(pts[i] + (u * a.cos() + v * a.sin()) * radius);
            }
        }

        let mut faces: Vec<Vec<usize>> = Vec::new();
        let rings = if closed { n } else { n - 1 };
        for i in 0..rings {
            let (r0, r1) = (i * segs, ((i + 1) % n) * segs);
            for s in 0..segs {
                let t = (s + 1) % segs;
                // Outward winding, matching new_cylinder's side quads.
                faces.push(vec![r0 + s, r1 + s, r1 + t, r0 + t]);
            }
        }
        if caps && !closed {
            let last = (n - 1) * segs;
            faces.push((0..segs).collect());
            faces.push((last..last + segs).rev().collect());
        }
        Self::from_polygons(&positions, &faces)
    }

    /// Create a torus.
    pub fn new_torus(major_r: f32, minor_r: f32, major_segs: u32, minor_segs: u32) -> Self {
        let ms = major_segs.max(3);
        let ns = minor_segs.max(3);
        let mut positions = Vec::new();
        let mut quads = Vec::new();

        for i in 0..ms {
            let theta = 2.0 * std::f32::consts::PI * i as f32 / ms as f32;
            let ct = theta.cos();
            let st = theta.sin();
            for j in 0..ns {
                let phi = 2.0 * std::f32::consts::PI * j as f32 / ns as f32;
                let cp = phi.cos();
                let sp = phi.sin();
                let x = (major_r + minor_r * cp) * ct;
                let y = minor_r * sp;
                let z = (major_r + minor_r * cp) * st;
                positions.push(Vec3::new(x, y, z));
            }
        }

        for i in 0..ms {
            let ni = (i + 1) % ms;
            for j in 0..ns {
                let nj = (j + 1) % ns;
                let a = (i * ns + j) as usize;
                let b = (ni * ns + j) as usize;
                let c = (ni * ns + nj) as usize;
                let d = (i * ns + nj) as usize;
                // Ring order a→d→c→b (minor φ first, then major θ) so the face normal is
                // ∂φ × ∂θ = OUTWARD. The other order, a→b→c→d, is ∂θ × ∂φ — it points at the
                // major axis and built every full torus inside-out (signed volume −1.19 at the
                // default spec). Vertex indexing is untouched: callers that address the minor
                // ring as `major*minor_segs + minor` still do.
                quads.push([a, d, c, b]);
            }
        }

        Self::from_quads(&positions, &quads)
    }

    /// Create a cone.
    pub fn new_cone(radius: f32, height: f32, segments: u32) -> Self {
        let segs = segments.max(3);
        let hh = height * 0.5;
        let mut positions = Vec::new();
        let mut tris = Vec::new();

        // Base ring
        for s in 0..segs {
            let theta = 2.0 * std::f32::consts::PI * s as f32 / segs as f32;
            positions.push(Vec3::new(radius * theta.cos(), -hh, radius * theta.sin()));
        }

        // Apex
        let apex = positions.len();
        positions.push(Vec3::new(0.0, hh, 0.0));

        // Base center
        let bc = positions.len();
        positions.push(Vec3::new(0.0, -hh, 0.0));

        // The base ring runs counter-clockwise seen from +Y, so a side wound `ring[s] → next →
        // apex` has its normal pointing INTO the cone, and the base wound `centre → next → s`
        // points +Y where the outward face is −Y. Both were reversed, which built every TriFan
        // cone inside-out (signed volume −2.08 at the default radius/height).

        // Side triangles
        for s in 0..segs {
            let next = (s + 1) % segs;
            tris.push([next as usize, s as usize, apex]);
        }

        // Base triangles
        for s in 0..segs {
            let next = (s + 1) % segs;
            tris.push([bc, s as usize, next as usize]);
        }

        Self::from_triangles(&positions, &tris)
    }

    // ============================================================
    // Construction from indexed data
    // ============================================================

    /// Build from quad faces.
    ///
    /// Every face is allocated with `material_id: 0` — see [`Self::from_quads_with_materials`] when
    /// the quads came from an existing mesh.
    pub fn from_quads(positions: &[Vec3], quads: &[[usize; 4]]) -> Self {
        Self::from_quads_with_materials(positions, quads, &[])
    }

    /// [`Self::from_quads`] that also stamps per-face materials, indexed parallel to `quads`.
    /// A short / empty slice leaves the missing entries at 0, so passing `&[]` is exactly
    /// `from_quads`. Unlike `from_polygons` this constructor skips no input, so the indices cannot
    /// drift — but stamping still happens at allocation time for the same reason.
    pub fn from_quads_with_materials(
        positions: &[Vec3],
        quads: &[[usize; 4]],
        materials: &[u32],
    ) -> Self {
        let mut mesh = Self::new();
        mesh.vertices.reserve(positions.len());
        mesh.half_edges.reserve(quads.len() * 4);
        mesh.faces.reserve(quads.len());

        // Create vertices
        let verts: Vec<VertexId> = positions.iter().map(|&pos| {
            mesh.alloc_vertex(HVertex { position: pos, normal: Vec3::ZERO, half_edge: NO_EDGE })
        }).collect();

        // Twin linking: direct-addressed buckets keyed by source vertex (see `TwinLinker`).
        let mut out_degree = vec![0u32; positions.len()];
        for quad in quads {
            for i in 0..4 {
                out_degree[quad[i]] += 1;
            }
        }
        let mut linker = TwinLinker::new(&out_degree);

        for (quad_idx, quad) in quads.iter().enumerate() {
            let material_id = materials.get(quad_idx).copied().unwrap_or(0);
            let fid = mesh.alloc_face(HFace { half_edge: NO_EDGE, normal: Vec3::ZERO, material_id });
            let mut face_edges = [NO_EDGE; 4];

            for i in 0..4 {
                let (si, di) = (quad[i], quad[(i + 1) % 4]);
                let src = verts[si];
                let dst = verts[di];
                let he = mesh.alloc_half_edge(HalfEdge {
                    vertex: dst,
                    twin: NO_TWIN,
                    next: NO_EDGE,
                    prev: NO_EDGE,
                    face: fid,
                });
                face_edges[i] = he;

                // Set vertex half-edge if not set
                if mesh.vert(src).half_edge == NO_EDGE {
                    mesh.vert_mut(src).half_edge = he;
                }

                // Twin linking
                if let Some(twin) = linker.link(si as u32, di as u32, he) {
                    mesh.he_mut(he).twin = twin;
                    mesh.he_mut(twin).twin = he;
                }
            }

            // Link next/prev within the face
            for i in 0..4 {
                mesh.he_mut(face_edges[i]).next = face_edges[(i + 1) % 4];
                mesh.he_mut(face_edges[i]).prev = face_edges[(i + 3) % 4];
            }
            mesh.face_mut(fid).half_edge = face_edges[0];
        }

        mesh.recompute_normals();
        mesh
    }

    /// Build from triangle faces.
    pub fn from_triangles(positions: &[Vec3], tris: &[[usize; 3]]) -> Self {
        let mut mesh = Self::new();
        mesh.vertices.reserve(positions.len());
        mesh.half_edges.reserve(tris.len() * 3);
        mesh.faces.reserve(tris.len());

        let verts: Vec<VertexId> = positions.iter().map(|&pos| {
            mesh.alloc_vertex(HVertex { position: pos, normal: Vec3::ZERO, half_edge: NO_EDGE })
        }).collect();

        // Same direct-addressed twin linking as `from_quads_with_materials` — ONE implementation,
        // so the two constructors cannot drift apart on non-manifold pairing semantics.
        let mut out_degree = vec![0u32; positions.len()];
        for tri in tris {
            for i in 0..3 {
                out_degree[tri[i]] += 1;
            }
        }
        let mut linker = TwinLinker::new(&out_degree);

        for tri in tris {
            let fid = mesh.alloc_face(HFace { half_edge: NO_EDGE, normal: Vec3::ZERO, material_id: 0 });
            let mut face_edges = [NO_EDGE; 3];

            for i in 0..3 {
                let (si, di) = (tri[i], tri[(i + 1) % 3]);
                let src = verts[si];
                let dst = verts[di];
                let he = mesh.alloc_half_edge(HalfEdge {
                    vertex: dst,
                    twin: NO_TWIN,
                    next: NO_EDGE,
                    prev: NO_EDGE,
                    face: fid,
                });
                face_edges[i] = he;

                if mesh.vert(src).half_edge == NO_EDGE {
                    mesh.vert_mut(src).half_edge = he;
                }

                if let Some(twin) = linker.link(si as u32, di as u32, he) {
                    mesh.he_mut(he).twin = twin;
                    mesh.he_mut(twin).twin = he;
                }
            }

            for i in 0..3 {
                mesh.he_mut(face_edges[i]).next = face_edges[(i + 1) % 3];
                mesh.he_mut(face_edges[i]).prev = face_edges[(i + 2) % 3];
            }
            mesh.face_mut(fid).half_edge = face_edges[0];
        }

        mesh.recompute_normals();
        mesh
    }

    /// Build from existing simple Mesh (DC output or imported).
    pub fn from_mesh(mesh: &crate::mesh::Mesh) -> Self {
        if mesh.quads.is_empty() {
            let mut he_mesh = Self::new();
            for &pos in &mesh.vertices {
                he_mesh.alloc_vertex(HVertex { position: pos, normal: Vec3::Y, half_edge: NO_EDGE });
            }
            return he_mesh;
        }
        let mut mixed = false;
        let mut polys: Vec<Vec<usize>> = Vec::with_capacity(mesh.quads.len());
        let mut quads: Vec<[usize; 4]> = Vec::with_capacity(mesh.quads.len());
        for q in &mesh.quads {
            match compact_dc_ring(*q) {
                Some(r) if r.len() == 4 => {
                    quads.push([r[0], r[1], r[2], r[3]]);
                    polys.push(r);
                }
                Some(r) => {
                    mixed = true;
                    polys.push(r);
                }
                None => {}
            }
        }
        if mixed {
            Self::from_polygons(&mesh.vertices, &polys)
        } else {
            Self::from_quads(&mesh.vertices, &quads)
        }
    }

    /// Drag-time remesh display mesh: same verts/faces as [`from_mesh`], no twin pairing.
    ///
    /// Twin-link is ~60% of `from_mesh` at remesh 256. Display (`to_raster_data`) walks
    /// face rings, not twins, so a live drag can skip pairing. Episode commit must go
    /// through [`from_mesh`] so the stored stack mesh is a real manifold.
    pub fn from_mesh_draft(mesh: &crate::mesh::Mesh) -> Self {
        if mesh.quads.is_empty() {
            return Self::from_mesh(mesh);
        }
        let mut out = Self::new();
        out.vertices.reserve(mesh.vertices.len());
        out.half_edges.reserve(mesh.quads.len() * 4);
        out.faces.reserve(mesh.quads.len());
        let verts: Vec<VertexId> = mesh
            .vertices
            .iter()
            .enumerate()
            .map(|(i, &pos)| {
                let n = mesh.normals.get(i).copied().unwrap_or(Vec3::Y);
                out.alloc_vertex(HVertex {
                    position: pos,
                    normal: n,
                    half_edge: NO_EDGE,
                })
            })
            .collect();
        for quad in &mesh.quads {
            let Some(ring) = compact_dc_ring(*quad) else {
                continue;
            };
            let n = ring.len();
            let fid = out.alloc_face(HFace {
                half_edge: NO_EDGE,
                normal: Vec3::ZERO,
                material_id: 0,
            });
            let mut face_edges = vec![NO_EDGE; n];
            for i in 0..n {
                let src = verts[ring[i]];
                let dst = verts[ring[(i + 1) % n]];
                let he = out.alloc_half_edge(HalfEdge {
                    vertex: dst,
                    twin: NO_TWIN,
                    next: NO_EDGE,
                    prev: NO_EDGE,
                    face: fid,
                });
                face_edges[i] = he;
                if out.vert(src).half_edge == NO_EDGE {
                    out.vert_mut(src).half_edge = he;
                }
            }
            for i in 0..n {
                out.he_mut(face_edges[i]).next = face_edges[(i + 1) % n];
                out.he_mut(face_edges[i]).prev = face_edges[(i + n - 1) % n];
            }
            out.face_mut(fid).half_edge = face_edges[0];
        }
        // No TwinLinker, no recompute_normals. `to_raster_data` recomputes face
        // normals from the ring; vertex normals are already stamped from extract.
        out
    }

    /// Build from raw triangle soup (positions + normals + triangle indices).
    pub fn from_triangle_soup(positions: &[Vec3], _normals: &[Vec3], indices: &[[u32; 3]]) -> Self {
        let tris: Vec<[usize; 3]> = indices.iter()
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect();
        Self::from_triangles(positions, &tris)
    }

    // ============================================================
    // Conversion to raster data
    // ============================================================

    /// Convert to flat triangle data for GPU upload.
    /// Format: `[px, py, pz, nx, ny, nz, u, v]` per vertex + triangle indices.
    ///
    /// Emits per-face split vertices so each face gets its own flat normal.
    /// A box produces 24 vertices (4 per face × 6 faces) with hard edges.
    pub fn to_raster_data(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        // Allocation discipline (2026-08-08): this ran three heap allocations PER FACE
        // (face ring, points, ear-clip result) and grew both outputs by reallocation — on a
        // 150k-quad remesh extract that was ~450k allocations plus repeated copies of a
        // ~19 MB vertex buffer. Reserve once from the live face count and reuse the two
        // per-face buffers. Pure allocation removal: identical vertices, identical indices.
        let live_faces = self.faces.len().saturating_sub(self.free_faces.len());
        let mut verts = Vec::with_capacity(live_faces * 4);
        let mut indices = Vec::with_capacity(live_faces * 6);
        let mut face_verts: Vec<VertexId> = Vec::with_capacity(8);
        let mut pts: Vec<glam::Vec3> = Vec::with_capacity(8);

        for (i, _f) in self.faces.iter().enumerate() {
            if self.free_faces.contains(&(i as u32)) { continue; }
            let fid = FaceId(i as u32);
            self.face_vertices_into(fid, &mut face_verts);
            if face_verts.len() < 3 { continue; }

            let face_n = self.compute_face_normal(fid);
            let base = verts.len() as u32;

            // Emit one vertex per face-corner with the face normal
            pts.clear();
            for &vid in &face_verts {
                let p = self.vert(vid).position;
                pts.push(p);
                verts.push([p.x, p.y, p.z,
                            face_n.x, face_n.y, face_n.z,
                            0.0, 0.0]);
            }

            // Ear-clip triangulation (concave-safe), CCW when viewed from outside (matches
            // wgpu FrontFace::Ccw). The old corner-0 FAN emitted phantom triangles OUTSIDE
            // any concave n-gon plus overlapping slivers — poisoning every consumer of this
            // fn: OBJ/glTF export, Object-Mode bake input, and the mesh→SDF convert (owner
            // 2026-08-03: "mesh to sdf creates bad results when the mesh has n-gons" — the
            // sampler read the phantom fan surface as real). Convex faces triangulate to the
            // same count (diagonal choice may differ). Zero-area slivers are culled so
            // degenerate rings can't reach exporters or SDF sign classification.
            for t in crate::poly_mesh::earclip_polygon(&pts, face_n) {
                let (a, b, c) = (
                    pts[t[0] as usize],
                    pts[t[1] as usize],
                    pts[t[2] as usize],
                );
                if (b - a).cross(c - a).length_squared() <= 1e-20 {
                    continue;
                }
                indices.push(base + t[0]);
                indices.push(base + t[1]);
                indices.push(base + t[2]);
            }
        }

        (verts, indices)
    }

    /// Convert back to simple Mesh for export.
    pub fn to_mesh(&self) -> crate::mesh::Mesh {
        let mut vertices = Vec::new();
        let mut normals = Vec::new();
        let mut quads = Vec::new();
        let mut id_map = vec![u32::MAX; self.vertices.len()];

        for (i, v) in self.vertices.iter().enumerate() {
            if !self.is_vert_live(VertexId(i as u32)) { continue; }
            id_map[i] = vertices.len() as u32;
            vertices.push(v.position);
            normals.push(v.normal);
        }

        for (i, _f) in self.faces.iter().enumerate() {
            if self.free_faces.contains(&(i as u32)) { continue; }
            let face_verts = self.face_vertices(FaceId(i as u32));
            if face_verts.len() == 4 {
                quads.push([
                    id_map[face_verts[0].0 as usize],
                    id_map[face_verts[1].0 as usize],
                    id_map[face_verts[2].0 as usize],
                    id_map[face_verts[3].0 as usize],
                ]);
            } else if face_verts.len() == 3 {
                // Store as degenerate quad (v3 == v2)
                quads.push([
                    id_map[face_verts[0].0 as usize],
                    id_map[face_verts[1].0 as usize],
                    id_map[face_verts[2].0 as usize],
                    id_map[face_verts[2].0 as usize],
                ]);
            }
        }

        crate::mesh::Mesh { vertices, normals, quads }
    }

    // ============================================================
    // ============================================================
    // Polygon constructor + Bevel
    // ============================================================

    /// Build a NEW mesh from a subset of this mesh's faces (Blender Separate by Selection geometry).
    ///
    /// Vertices are welded by source `VertexId` (adjacent selected faces stay connected). Does not
    /// mutate `self`. Returns `None` if `faces` is empty or yields no valid polygon (≥3 verts).
    /// The result is re-normalized; caller should check `is_manifold` if installing as a body.
    pub fn faces_as_new_mesh(&self, faces: &[FaceId]) -> Option<HalfEdgeMesh> {
        if faces.is_empty() {
            return None;
        }
        use std::collections::HashMap;
        let mut old_to_new: HashMap<u32, usize> = HashMap::new();
        let mut positions: Vec<Vec3> = Vec::new();
        let mut face_lists: Vec<Vec<usize>> = Vec::new();
        // Parallel to `face_lists` — separated geometry keeps the materials it was authored with.
        let mut face_mats: Vec<u32> = Vec::new();
        for &fid in faces {
            if (fid.0 as usize) >= self.faces.len() || self.free_faces.contains(&fid.0) {
                continue;
            }
            let ring = self.face_vertices(fid);
            if ring.len() < 3 {
                continue;
            }
            let mut poly = Vec::with_capacity(ring.len());
            for vid in ring {
                let idx = *old_to_new.entry(vid.0).or_insert_with(|| {
                    let i = positions.len();
                    positions.push(self.vert(vid).position);
                    i
                });
                poly.push(idx);
            }
            face_lists.push(poly);
            face_mats.push(self.face(fid).material_id);
        }
        if face_lists.is_empty() || positions.is_empty() {
            return None;
        }
        Some(Self::from_polygons_with_materials(&positions, &face_lists, &face_mats))
    }

    /// Build from arbitrary polygon faces (generalized from_quads/from_triangles).
    ///
    /// Every face is allocated with `material_id: 0`. If the polygon soup came from an existing
    /// mesh, use [`Self::from_polygons_with_materials`] instead — otherwise the rebuild silently
    /// repaints the whole body with the default material (the Ctrl+X / Alt+X material-loss class).
    pub fn from_polygons(positions: &[Vec3], faces: &[Vec<usize>]) -> Self {
        Self::from_polygons_with_materials(positions, faces, &[])
    }

    /// [`Self::from_polygons`] that also stamps per-face materials.
    ///
    /// `materials` is indexed by the INPUT face index (parallel to `faces`), NOT by the resulting
    /// `FaceId`. That distinction is the whole point: `from_polygons` SKIPS rings shorter than 3,
    /// so output face ids drift from input indices as soon as one degenerate ring is present, and
    /// a naive post-pass would paint faces with a neighbour's material — worse than the default it
    /// replaces. This constructor stamps at allocation time, so it cannot drift.
    ///
    /// A short / empty `materials` slice leaves the missing entries at 0, so
    /// `from_polygons_with_materials(p, f, &[])` is exactly `from_polygons(p, f)`.
    pub fn from_polygons_with_materials(
        positions: &[Vec3],
        faces: &[Vec<usize>],
        materials: &[u32],
    ) -> Self {
        let mut mesh = Self::new();
        mesh.vertices.reserve(positions.len());
        mesh.faces.reserve(faces.len());

        let verts: Vec<VertexId> = positions.iter().map(|&pos| {
            mesh.alloc_vertex(HVertex { position: pos, normal: Vec3::ZERO, half_edge: NO_EDGE })
        }).collect();

        // Same packed-key EdgeMap as from_quads/from_triangles (one edge-key convention per
        // file); Vec values keep the non-manifold polygon-soup tolerance.
        let mut edge_map: std::collections::HashMap<
            u64,
            Vec<HalfEdgeId>,
            std::hash::BuildHasherDefault<EdgeKeyHasher>,
        > = Default::default();

        for (poly_idx, face) in faces.iter().enumerate() {
            let n = face.len();
            if n < 3 { continue; }
            let material_id = materials.get(poly_idx).copied().unwrap_or(0);
            let fid = mesh.alloc_face(HFace { half_edge: NO_EDGE, normal: Vec3::ZERO, material_id });
            let mut face_edges = Vec::with_capacity(n);

            for i in 0..n {
                let src = verts[face[i]];
                let dst = verts[face[(i + 1) % n]];
                let he = mesh.alloc_half_edge(HalfEdge {
                    vertex: dst, twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: fid,
                });
                face_edges.push(he);

                if mesh.vert(src).half_edge == NO_EDGE {
                    mesh.vert_mut(src).half_edge = he;
                }

                // Twin linking: match reverse-direction half-edges.
                // Use Vec to handle non-manifold polygon soups (multiple
                // half-edges with the same directed edge).
                let twins = edge_map.entry(edge_key(dst.0, src.0)).or_default();
                if let Some(pos) = twins.iter().position(|&t| mesh.he(t).twin == NO_TWIN) {
                    let twin = twins.remove(pos);
                    mesh.he_mut(he).twin = twin;
                    mesh.he_mut(twin).twin = he;
                } else {
                    edge_map.entry(edge_key(src.0, dst.0)).or_default().push(he);
                }
            }

            for i in 0..n {
                mesh.he_mut(face_edges[i]).next = face_edges[(i + 1) % n];
                mesh.he_mut(face_edges[i]).prev = face_edges[(i + n - 1) % n];
            }
            mesh.face_mut(fid).half_edge = face_edges[0];
        }

        mesh.recompute_normals();
        // Locality: leave ~6% growth headroom so the first in-place edit after build
        // does not pay an O(mesh) Vec reallocation (exact-capacity packs).
        let hv = (mesh.vertices.len() / 16).max(32);
        let he = (mesh.half_edges.len() / 16).max(128);
        let hf = (mesh.faces.len() / 16).max(32);
        mesh.reserve_elements(hv, he, hf);
        #[cfg(debug_assertions)]
        mesh.validate_mesh("from_polygons");
        mesh
    }

}
