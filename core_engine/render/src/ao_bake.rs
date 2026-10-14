//! Ambient-occlusion bake cache — per-vertex mesh-AO for the raster deferred path.
//!
//! Substance-style PREBAKED AO (AO_BAKE_PLAN.md): for a body's CPU triangle mesh
//! we cast N cosine-weighted hemisphere rays per vertex against the SAME triangle
//! BLAS the path tracer builds (`build_blas_entry` / `BvhNode`, `render::pt_accel`),
//! and store ONE `f32` occlusion scalar per vertex. The raster deferred shade then
//! composes it into the ambient term (`min()` with SSAO — the darkest occluder
//! wins). The path tracer is UNTOUCHED: it integrates occlusion live in-shader over
//! the same triangles, so this cache is never bound in the PT pass.
//!
//! This mirrors `PtAccelCache` EXACTLY (AO_BAKE_PLAN.md §2): a `rayon::spawn` build
//! job, an `mpsc` result channel, a per-body generation counter, and a `poll()`
//! that drains finished bakes and discards superseded generations. Staleness keys
//! on the mesh cache's `content_stamp` (the one `BodyContentId`-derived identity),
//! so an AO bake invalidates exactly when its mesh does.
//!
//! AO is a CACHE, never serialized (AO_BAKE_PLAN.md §4, owner decision 4): it
//! regenerates on demand, matching `pt_accel`.

use core_math::bvh::BvhNode;
use core_math::scene_graph::MeshCacheId;
use glam::Vec3;
use std::collections::HashMap;
use std::sync::mpsc;

use crate::pt_accel::{build_blas_entry, PtTri, PtVertex};

/// Quality presets (AO_BAKE_PLAN.md, owner decision 3): rays cast per vertex.
/// Default = Medium (128). Higher = slower bake, smoother AO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AoQuality {
    Low,
    Medium,
    High,
}

impl AoQuality {
    pub fn rays(self) -> u32 {
        match self {
            AoQuality::Low => 32,
            AoQuality::Medium => 128,
            AoQuality::High => 512,
        }
    }
}

impl Default for AoQuality {
    fn default() -> Self {
        AoQuality::Medium
    }
}

/// Bake parameters. `radius` is the occlusion `t_max` in WORLD units (large-scale
/// cavity vs contact); default ≈ body-diagonal × 0.5 (AO_BAKE_PLAN.md, owner knob).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AoBakeParams {
    pub quality: AoQuality,
    pub radius: f32,
    /// Deterministic seed (AO_BAKE_PLAN.md D: same mesh + seed → same AO).
    pub seed: u64,
}

impl Default for AoBakeParams {
    fn default() -> Self {
        Self { quality: AoQuality::Medium, radius: 1.0, seed: 0x9E3779B97F4A7C15 }
    }
}

/// Honest per-body ceiling (AO_BAKE_PLAN.md §9 — the PT snapshot ceiling): reject
/// meshes denser than this rather than casting billions of rays / thrashing memory.
pub const AO_BAKE_MAX_TRIS: usize = 1_200_000;

/// A finished per-vertex AO bake for one body.
pub struct AoBaked {
    pub source_hash: u64,
    /// One occlusion scalar per CPU vertex, index-parallel to the body's raster
    /// vertex buffer. 1.0 = fully open (no darkening), 0.0 = fully occluded.
    pub ao: Vec<f32>,
    generation: u64,
    cache_id: MeshCacheId,
}

/// Outcome of a bake request (so callers can toast the honest ceiling).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AoRequest {
    /// Queued a background bake job.
    Queued,
    /// Already current or already in flight for this stamp — nothing to do.
    Noop,
    /// Mesh exceeds `AO_BAKE_MAX_TRIS` — rejected, caller should toast.
    RejectedTooDense,
    /// Mesh was empty — nothing to bake.
    Empty,
}

pub struct AoBakeCache {
    built: HashMap<MeshCacheId, AoBaked>,
    /// Per-body generation — a result whose generation is stale is discarded at
    /// the single insertion point in `poll()` (the `PtAccelCache` pattern).
    generations: HashMap<MeshCacheId, u64>,
    /// (cache_id → source_hash) of bakes currently on the rayon pool.
    in_flight: HashMap<MeshCacheId, u64>,
    result_rx: mpsc::Receiver<AoBaked>,
    result_tx: mpsc::Sender<AoBaked>,
}

impl Default for AoBakeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AoBakeCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            built: HashMap::new(),
            generations: HashMap::new(),
            in_flight: HashMap::new(),
            result_rx: rx,
            result_tx: tx,
        }
    }

    /// True if this body's AO is baked AND matches `source_hash` (the mesh's
    /// current `content_stamp`). Mesh edit → stamp bumps → this goes false =
    /// STALE (AO_BAKE_PLAN.md §4).
    pub fn is_current(&self, id: MeshCacheId, source_hash: u64) -> bool {
        self.built.get(&id).map_or(false, |b| b.source_hash == source_hash)
    }

    /// Whether ANY AO is baked for this body (even if stale). The panel shows the
    /// stale badge when this is true but `is_current` is false.
    pub fn has_bake(&self, id: MeshCacheId) -> bool {
        self.built.contains_key(&id)
    }

    /// Borrow a body's baked per-vertex AO (for GPU upload), IFF current for
    /// `source_hash`. A stale bake returns None so the shader falls back to AO=1
    /// (no wrong darkening) — the badge tells the user to rebake.
    pub fn current_ao(&self, id: MeshCacheId, source_hash: u64) -> Option<&[f32]> {
        self.built.get(&id).and_then(|b| {
            if b.source_hash == source_hash {
                Some(b.ao.as_slice())
            } else {
                None
            }
        })
    }

    /// Queue a background AO bake for `id` unless already current/in-flight for
    /// the same hash. Takes owned mesh data (moved to the rayon job).
    pub fn request(
        &mut self,
        id: MeshCacheId,
        source_hash: u64,
        raw_verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
        params: AoBakeParams,
    ) -> AoRequest {
        if raw_verts.is_empty() || indices.is_empty() {
            return AoRequest::Empty;
        }
        if indices.len() / 3 > AO_BAKE_MAX_TRIS {
            return AoRequest::RejectedTooDense;
        }
        if self.is_current(id, source_hash) {
            return AoRequest::Noop;
        }
        if self.in_flight.get(&id) == Some(&source_hash) {
            return AoRequest::Noop;
        }
        let generation = {
            let g = self.generations.entry(id).or_insert(0);
            *g += 1;
            *g
        };
        self.in_flight.insert(id, source_hash);
        let tx = self.result_tx.clone();
        rayon::spawn(move || {
            let t0 = std::time::Instant::now();
            let ao = bake_vertex_ao(&raw_verts, &indices, params);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if ms > 5.0 {
                eprintln!(
                    "[AoBake] cid={:?} baked: verts={} rays={} in {:.1}ms (background)",
                    id,
                    ao.len(),
                    params.quality.rays(),
                    ms
                );
            }
            let _ = tx.send(AoBaked { source_hash, ao, generation, cache_id: id });
        });
        AoRequest::Queued
    }

    /// Drain finished bakes. Returns true if any AO changed (caller re-uploads).
    /// Stale generations are discarded silently (the `PtAccelCache` pattern).
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(built) = self.result_rx.try_recv() {
            let current_gen = self.generations.get(&built.cache_id).copied().unwrap_or(0);
            if built.generation != current_gen {
                continue; // superseded by a newer request
            }
            if self.in_flight.get(&built.cache_id) == Some(&built.source_hash) {
                self.in_flight.remove(&built.cache_id);
            }
            self.built.insert(built.cache_id, built);
            changed = true;
        }
        changed
    }

    /// Drop a body's AO bake (body deleted / mesh removed). Invalidates in-flight
    /// results by bumping the generation (a late job would otherwise resurrect it).
    pub fn remove(&mut self, id: MeshCacheId) {
        let generation = self.generations.entry(id).or_insert(0);
        *generation = generation.wrapping_add(1);
        self.built.remove(&id);
        self.in_flight.remove(&id);
    }

    /// Drop cached/in-flight bakes no longer owned by the mesh cache.
    pub fn retain_live<F>(&mut self, mut is_live: F)
    where
        F: FnMut(MeshCacheId) -> bool,
    {
        let stale: Vec<MeshCacheId> = self
            .built
            .keys()
            .chain(self.in_flight.keys().filter(|id| !self.built.contains_key(*id)))
            .copied()
            .filter(|id| !is_live(*id))
            .collect();
        for id in stale {
            self.remove(id);
        }
    }
}

/// The AO integrator (the body of the rayon job) — a PURE function so it is
/// directly unit-testable and the golden CPU reference.
///
/// For each vertex, cast `params.quality.rays()` cosine-weighted hemisphere rays
/// about the vertex normal against the body's OWN BLAS (self-occlusion), out to
/// `params.radius` world units. AO = fraction of rays that reach `t_max` unblocked
/// (1.0 = fully open). Cosine weighting is achieved by cosine-distributing the ray
/// directions (Malley's method), so no per-ray weight term is needed — the
/// unweighted hit fraction already equals the cosine-weighted occlusion integral.
pub fn bake_vertex_ao(raw_verts: &[[f32; 8]], indices: &[u32], params: AoBakeParams) -> Vec<f32> {
    use rayon::prelude::*;

    let (verts, tris, nodes, _aabb) = build_blas_entry(raw_verts, indices);
    if nodes.is_empty() || tris.is_empty() {
        return vec![1.0; raw_verts.len()];
    }
    let n_rays = params.quality.rays().max(1);
    let t_max = params.radius.max(1e-4);
    // Small offset along the normal so a vertex does not self-hit its own faces.
    let bias = t_max * 1e-3 + 1e-5;

    let bake_one = |vi: usize| -> f32 {
        let v = &verts[vi];
        let p = Vec3::from(v.pos);
        let mut n = Vec3::from(v.normal);
        if !n.is_finite() || n.length_squared() < 1e-12 {
            return 1.0; // degenerate normal — treat as open
        }
        n = n.normalize();
        let orig = p + n * bias;
        // Orthonormal basis around the normal.
        let (tangent, bitangent) = ortho_basis(n);
        // Deterministic per-vertex RNG (seed + vertex index) — same seed → same AO.
        let mut rng = SplitMix64::new(params.seed ^ (vi as u64).wrapping_mul(0x2545F4914F6CDD1D));
        let mut unblocked = 0u32;
        for _ in 0..n_rays {
            let u1 = rng.next_f32();
            let u2 = rng.next_f32();
            // Malley: cosine-weighted hemisphere sample in tangent space.
            let r = u1.sqrt();
            let phi = std::f32::consts::TAU * u2;
            let (sp, cp) = phi.sin_cos();
            let x = r * cp;
            let y = r * sp;
            let z = (1.0 - u1).max(0.0).sqrt();
            let dir = (tangent * x + bitangent * y + n * z).normalize_or_zero();
            if dir.length_squared() < 1e-12 {
                unblocked += 1;
                continue;
            }
            if !occludes(&verts, &tris, &nodes, orig, dir, t_max) {
                unblocked += 1;
            }
        }
        (unblocked as f32) / (n_rays as f32)
    };

    // Parallel over vertices above a threshold (bake is background + explicit).
    const PAR_MIN: usize = 4096;
    if verts.len() >= PAR_MIN {
        (0..verts.len()).into_par_iter().map(bake_one).collect()
    } else {
        (0..verts.len()).map(bake_one).collect()
    }
}

/// Any-hit occlusion query against one body's BLAS (body-local space). Returns
/// true if a triangle is hit within `(0, t_max)`. Mirrors the traversal structure
/// of `pt_accel::trace_instance` but any-hit (early-out) and single-BLAS.
fn occludes(
    verts: &[PtVertex],
    tris: &[PtTri],
    nodes: &[BvhNode],
    orig: Vec3,
    dir: Vec3,
    t_max: f32,
) -> bool {
    let mut stack: [u32; 64] = [0; 64];
    let mut sp = 1usize;
    stack[0] = 0;
    while sp > 0 {
        sp -= 1;
        let node = &nodes[stack[sp] as usize];
        if !ray_aabb_hit(orig, dir, node.aabb_min, node.aabb_max, t_max) {
            continue;
        }
        if node.count > 0 {
            for k in node.first..node.first + node.count {
                let tri = &tris[k as usize];
                let a = Vec3::from(verts[tri.v0 as usize].pos);
                let b = Vec3::from(verts[tri.v1 as usize].pos);
                let c = Vec3::from(verts[tri.v2 as usize].pos);
                if let Some(t) = ray_tri_mt(orig, dir, a, b, c) {
                    if t > 1e-5 && t < t_max {
                        return true;
                    }
                }
            }
        } else {
            // Internal node — push both children (BLAS child refs are absolute
            // within the node array). Guard the fixed stack against pathological depth.
            if sp + 2 <= stack.len() {
                stack[sp] = node.first;
                stack[sp + 1] = node.first + 1;
                sp += 2;
            }
        }
    }
    false
}

/// A stable orthonormal basis (tangent, bitangent) for a unit normal.
fn ortho_basis(n: Vec3) -> (Vec3, Vec3) {
    let a = if n.x.abs() > 0.9 { Vec3::Y } else { Vec3::X };
    let t = a.cross(n).normalize_or_zero();
    let t = if t.length_squared() < 1e-12 { Vec3::X } else { t };
    let b = n.cross(t);
    (t, b)
}

/// Slab test with unnormalized-direction support (matches `pt_accel`).
fn ray_aabb_hit(orig: Vec3, dir: Vec3, bmin: [f32; 3], bmax: [f32; 3], t_max: f32) -> bool {
    let inv = Vec3::new(
        1.0 / (if dir.x == 0.0 { 1e-30 } else { dir.x }),
        1.0 / (if dir.y == 0.0 { 1e-30 } else { dir.y }),
        1.0 / (if dir.z == 0.0 { 1e-30 } else { dir.z }),
    );
    let t1 = (Vec3::from(bmin) - orig) * inv;
    let t2 = (Vec3::from(bmax) - orig) * inv;
    let tmin = t1.min(t2).max_element().max(0.0);
    let tmax = t1.max(t2).min_element().min(t_max);
    tmin <= tmax
}

/// Möller–Trumbore, front+back faces (matches `pt_accel::ray_tri_mt`).
fn ray_tri_mt(orig: Vec3, dir: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let pv = dir.cross(e2);
    let det = e1.dot(pv);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv_det = 1.0 / det;
    let tv = orig - a;
    let u = tv.dot(pv) * inv_det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qv = tv.cross(e1);
    let v = dir.dot(qv) * inv_det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(qv) * inv_det;
    if t > 1e-5 {
        Some(t)
    } else {
        None
    }
}

/// SplitMix64 — a tiny, deterministic PRNG. Same seed → same stream, so an AO
/// bake is reproducible (AO_BAKE_PLAN.md D determinism test).
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    /// A float in [0, 1).
    fn next_f32(&mut self) -> f32 {
        // Top 24 bits → [0,1) with full mantissa precision.
        ((self.next_u64() >> 40) as f32) / ((1u32 << 24) as f32)
    }
}

/// Composition helper (AO_BAKE_PLAN.md, owner decision 2): baked mesh-AO composes
/// with SSAO via `min()` — the darkest (strongest) occluder wins, avoiding the
/// double-darkening of a multiply. Both are ambient-only factors in [0,1]. The
/// WGSL deferred shade does the same `min()` at the ambient multiply; this mirror
/// keeps the CPU and GPU composition in lockstep (and is unit-tested).
#[inline]
pub fn compose_ao_min(baked_ao: f32, ssao: f32) -> f32 {
    baked_ao.min(ssao)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit cube (±0.5) with outward-ish normals (per-corner, from center).
    fn cube_raw() -> (Vec<[f32; 8]>, Vec<u32>) {
        let p = [
            [-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [0.5, 0.5, -0.5], [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],  [0.5, -0.5, 0.5],  [0.5, 0.5, 0.5],  [-0.5, 0.5, 0.5],
        ];
        let verts = p
            .iter()
            .map(|v| {
                let n = Vec3::new(v[0], v[1], v[2]).normalize();
                [v[0], v[1], v[2], n.x, n.y, n.z, 0.0, 0.0]
            })
            .collect();
        let indices = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4,
            3, 7, 6, 3, 6, 2, 0, 4, 7, 0, 7, 3, 1, 2, 6, 1, 6, 5,
        ];
        (verts, indices)
    }

    #[test]
    fn ao_bake_is_deterministic() {
        let (v, i) = cube_raw();
        let params = AoBakeParams { quality: AoQuality::Medium, radius: 1.0, seed: 12345 };
        let a = bake_vertex_ao(&v, &i, params);
        let b = bake_vertex_ao(&v, &i, params);
        assert_eq!(a.len(), 8);
        assert_eq!(a, b, "same mesh + seed must produce identical AO");
    }

    #[test]
    fn ao_scalars_in_unit_range() {
        let (v, i) = cube_raw();
        let ao = bake_vertex_ao(&v, &i, AoBakeParams { radius: 2.0, ..Default::default() });
        for a in &ao {
            assert!((0.0..=1.0).contains(a), "AO {a} out of [0,1]");
        }
    }

    /// A concave shape: a vertex sitting inside a deep groove should be MORE
    /// occluded than an exposed convex corner. We build an open box (missing the
    /// +Z face) so interior-facing vertices see more geometry.
    #[test]
    fn concave_vertex_is_more_occluded_than_open() {
        // Two parallel walls forming a slot; a probe vertex between them faces up.
        // Wall geometry: quads at x=-0.5 and x=+0.5 spanning y,z. Probe vertex at
        // origin facing +Y between the walls.
        let mut verts: Vec<[f32; 8]> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        let push_quad = |verts: &mut Vec<[f32; 8]>, idx: &mut Vec<u32>, q: [[f32; 3]; 4], n: [f32; 3]| {
            let base = verts.len() as u32;
            for c in q.iter() {
                verts.push([c[0], c[1], c[2], n[0], n[1], n[2], 0.0, 0.0]);
            }
            idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        };
        // Left wall (x=-0.5), normal +X (faces inward toward probe).
        push_quad(&mut verts, &mut idx,
            [[-0.5, -1.0, -1.0], [-0.5, -1.0, 1.0], [-0.5, 1.0, 1.0], [-0.5, 1.0, -1.0]], [1.0, 0.0, 0.0]);
        // Right wall (x=+0.5), normal -X (faces inward).
        push_quad(&mut verts, &mut idx,
            [[0.5, -1.0, -1.0], [0.5, 1.0, -1.0], [0.5, 1.0, 1.0], [0.5, -1.0, 1.0]], [-1.0, 0.0, 0.0]);
        // Probe vertex 0 will be the interior one; append an isolated probe vertex
        // that faces +Y from between the walls, plus a lone reference vertex far away
        // facing +Y in the open with no nearby geometry.
        let interior_idx = verts.len();
        verts.push([0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]); // between the walls
        let open_idx = verts.len();
        verts.push([0.0, 0.0, 20.0, 0.0, 1.0, 0.0, 0.0, 0.0]); // far from any wall

        let params = AoBakeParams { quality: AoQuality::High, radius: 4.0, seed: 7 };
        let ao = bake_vertex_ao(&verts, &idx, params);
        let interior = ao[interior_idx];
        let open = ao[open_idx];
        assert!(open > 0.9, "open vertex should be nearly unoccluded, got {open}");
        assert!(
            interior < open - 0.1,
            "interior (between walls) AO {interior} must be notably more occluded than open {open}"
        );
    }

    #[test]
    fn reject_over_ceiling() {
        let mut cache = AoBakeCache::new();
        // Fabricate an index array that reports > AO_BAKE_MAX_TRIS triangles with a
        // matching (tiny) vertex list — request must reject WITHOUT spawning a job.
        let verts = vec![[0.0f32; 8]; 3];
        let indices = vec![0u32; (AO_BAKE_MAX_TRIS + 1) * 3];
        let r = cache.request(MeshCacheId(1), 1, verts, indices, AoBakeParams::default());
        assert_eq!(r, AoRequest::RejectedTooDense);
        assert!(!cache.has_bake(MeshCacheId(1)));
    }

    #[test]
    fn empty_mesh_is_noop() {
        let mut cache = AoBakeCache::new();
        let r = cache.request(MeshCacheId(2), 1, Vec::new(), Vec::new(), AoBakeParams::default());
        assert_eq!(r, AoRequest::Empty);
    }

    #[test]
    fn staleness_invalidates_on_stamp_change() {
        let (v, i) = cube_raw();
        let mut cache = AoBakeCache::new();
        let id = MeshCacheId(3);
        let r = cache.request(id, 100, v.clone(), i.clone(), AoBakeParams::default());
        assert_eq!(r, AoRequest::Queued);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "AO bake timed out");
            std::thread::yield_now();
        }
        // Current for stamp 100, STALE for a new stamp 200 (mesh edited).
        assert!(cache.is_current(id, 100));
        assert!(cache.current_ao(id, 100).is_some());
        assert!(!cache.is_current(id, 200), "a changed stamp must read as stale");
        assert!(cache.current_ao(id, 200).is_none(), "stale bake must not be served");
        assert!(cache.has_bake(id), "the stale bake is still present (badge shows stale)");
    }

    #[test]
    fn rebake_supersedes_old_generation() {
        let (v, i) = cube_raw();
        let mut cache = AoBakeCache::new();
        let id = MeshCacheId(4);
        assert_eq!(cache.request(id, 1, v.clone(), i.clone(), AoBakeParams::default()), AoRequest::Queued);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cache.is_current(id, 1) {
            cache.poll();
            assert!(std::time::Instant::now() < deadline, "bake timed out");
            std::thread::yield_now();
        }
        // Re-request with a new stamp; old entry replaced on poll.
        assert_eq!(cache.request(id, 2, v.clone(), i.clone(), AoBakeParams::default()), AoRequest::Queued);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cache.is_current(id, 2) {
            cache.poll();
            assert!(std::time::Instant::now() < deadline, "rebake timed out");
            std::thread::yield_now();
        }
        // Duplicate request for the current stamp is a no-op.
        assert_eq!(cache.request(id, 2, v, i, AoBakeParams::default()), AoRequest::Noop);
    }

    #[test]
    fn compose_min_takes_darkest() {
        assert_eq!(compose_ao_min(0.3, 0.8), 0.3);
        assert_eq!(compose_ao_min(0.9, 0.5), 0.5);
        assert_eq!(compose_ao_min(1.0, 1.0), 1.0);
        // No double-darkening: min(0.5,0.5)=0.5, NOT 0.25 (a multiply).
        assert_eq!(compose_ao_min(0.5, 0.5), 0.5);
    }

    #[test]
    fn remove_rejects_late_background_result() {
        let mut cache = AoBakeCache::new();
        let id = MeshCacheId(5);
        cache.generations.insert(id, 1);
        cache.in_flight.insert(id, 0xCAFE);
        cache
            .result_tx
            .send(AoBaked { source_hash: 0xCAFE, ao: vec![1.0; 3], generation: 1, cache_id: id })
            .expect("test channel");
        cache.remove(id);
        assert!(!cache.poll(), "removed bake must not be resurrected");
        assert!(!cache.has_bake(id));
    }
}
