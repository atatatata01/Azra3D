//! Path-tracer acceleration cache (Phase 2 — triangle PT over the DC mesh).
//!
//! Parallel to `MeshCache`, keyed by the same `MeshCacheId` and the same
//! `source_hash`, so a body's BLAS invalidates exactly when its mesh does.
//! BLAS builds run on the rayon pool (the `mesh_cache` background pattern);
//! results drain in `poll()` with a per-body generation counter so stale
//! builds are discarded silently.
//!
//! Vertices stay BODY-LOCAL. The TLAS leaf carries each instance's
//! `model`/`inv_model` matrices (from the raster instance enumeration), and
//! the ray is transformed into body space at the leaf — so moving/rotating a
//! body needs only a TLAS rebuild (sub-ms), never a BLAS rebuild.
//!
//! `trace_scene_cpu` is the golden CPU reference for the WGSL traversal —
//! the same oracle pattern as the voxel-brush CPU/GPU parity gate.

use bytemuck::{Pod, Zeroable};
use core_math::bvh::{build_scene_bvh, build_tri_blas, Aabb, BvhNode, SceneBvhBody};
use core_math::scene_graph::MeshCacheId;
use glam::{Mat3, Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

// ============================================================
// GPU-ready layouts (std430-friendly, uploaded as storage buffers)
// ============================================================

/// PT vertex: body-local position, normal and mesh UV, 48 B.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct PtVertex {
    pub pos: [f32; 3],
    pub _pad0: f32,
    pub normal: [f32; 3],
    pub _pad1: f32,
    pub uv: [f32; 2],
    pub _pad2: [f32; 2],
}

/// PT triangle: three indices into the GLOBAL `pt_verts` array (instance
/// `vert_off` already applied at assembly) + a per-triangle material slot
/// (0 for single-material bodies; the shader falls back to the instance's
/// `material_base` — per-tri materials arrive with multi-material imports).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct PtTri {
    pub v0: u32,
    pub v1: u32,
    pub v2: u32,
    pub material_idx: u32,
}

/// PT instance: one body placement. 192 B.
/// TLAS leaves store an index into the instance array; the shader transforms
/// the ray by `inv_model` and traverses `blas nodes[blas_node_off..]` whose
/// leaves reference `tris[tri_off + k]` (triangle order baked at build time —
/// no indirection buffer on the GPU).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct PtInstanceGpu {
    /// Body-local → world (column-major, matches `InstanceData.model_matrix`).
    pub model: [[f32; 4]; 4],
    /// World → body-local (ray transform).
    pub inv_model: [[f32; 4]; 4],
    /// Local normal → world normal: inverse-transpose upper-3×3 of `model`,
    /// stored as 3 padded columns.
    pub normal_mat: [[f32; 4]; 3],
    pub blas_node_off: u32,
    pub tri_off: u32,
    pub vert_off: u32,
    pub material_base: u32,
}

// ============================================================
// Per-body built BLAS
// ============================================================

/// A body's built BLAS + its GPU-ready arrays (body-local space).
/// `tris` is REORDERED so BLAS leaves reference `tris[first..first+count]`
/// directly — the GPU never sees a `tri_order` indirection.
pub struct BlasBuilt {
    pub source_hash: u64,
    pub verts: Vec<PtVertex>,
    pub tris: Vec<PtTri>,
    pub nodes: Vec<BvhNode>,
    /// Body-local AABB (root node AABB; zero-sized when mesh was empty).
    pub local_aabb: Aabb,
    generation: u64,
    cache_id: MeshCacheId,
}

/// Build a body's BLAS entry from the universal CPU mesh format
/// (`[px,py,pz, nx,ny,nz, u,v]` verts + flat triangle indices — what
/// `Mesh::to_raster_data` and the editable/imported paths all produce).
/// Pure function → directly unit-testable, and the body of the rayon job.
pub fn build_blas_entry(raw_verts: &[[f32; 8]], indices: &[u32]) -> (Vec<PtVertex>, Vec<PtTri>, Vec<BvhNode>, Aabb) {
    build_blas_entry_ex(raw_verts, indices, None)
}

/// Like [`build_blas_entry`], with optional **per-triangle material ids** (index-buffer order,
/// one `u32` per triangle = `indices.len()/3`). Values are scene `MaterialId`s (0 = body
/// default / inherit `material_base` at shade time). BLAS reorder preserves the id onto the
/// reordered `PtTri`. M6 multi-material poly path.
pub fn build_blas_entry_ex(
    raw_verts: &[[f32; 8]],
    indices: &[u32],
    tri_mat_ids: Option<&[u32]>,
) -> (Vec<PtVertex>, Vec<PtTri>, Vec<BvhNode>, Aabb) {
    use rayon::prelude::*;
    // Parallel maps above this size (rayon `collect` preserves order).
    const PAR_MAP_MIN: usize = 32768;

    let positions: Vec<[f32; 3]> = if raw_verts.len() >= PAR_MAP_MIN {
        raw_verts.par_iter().map(|v| [v[0], v[1], v[2]]).collect()
    } else {
        raw_verts.iter().map(|v| [v[0], v[1], v[2]]).collect()
    };
    let blas = build_tri_blas(&positions, indices);

    let mk_vert = |v: &[f32; 8]| PtVertex {
        pos: [v[0], v[1], v[2]],
        _pad0: v[6],
        normal: [v[3], v[4], v[5]],
        _pad1: v[7],
        uv: [v[6], v[7]],
        _pad2: [0.0; 2],
    };
    let verts: Vec<PtVertex> = if raw_verts.len() >= PAR_MAP_MIN {
        raw_verts.par_iter().map(mk_vert).collect()
    } else {
        raw_verts.iter().map(mk_vert).collect()
    };

    let n_tris = indices.len() / 3;
    // Bake tri_order into the triangle array: leaf.first..+count → tris directly.
    let mk_tri = |&t: &u32| {
        let b = t as usize * 3;
        let mat = tri_mat_ids
            .and_then(|m| m.get(t as usize).copied())
            .unwrap_or(0);
        debug_assert!((t as usize) < n_tris || n_tris == 0);
        PtTri {
            v0: indices[b],
            v1: indices[b + 1],
            v2: indices[b + 2],
            material_idx: mat,
        }
    };
    let tris: Vec<PtTri> = if blas.tri_order.len() >= PAR_MAP_MIN {
        blas.tri_order.par_iter().map(mk_tri).collect()
    } else {
        blas.tri_order.iter().map(mk_tri).collect()
    };

    let local_aabb = if blas.nodes.is_empty() {
        Aabb { min: [0.0; 3], max: [0.0; 3] }
    } else {
        Aabb { min: blas.nodes[0].aabb_min, max: blas.nodes[0].aabb_max }
    };
    (verts, tris, blas.nodes, local_aabb)
}

/// Expand `(material_id, first_index, index_count)` ranges into one scene MaterialId per triangle.
/// `index_count` is in **indices** (not triangles). Used by mesh_cache → PT BLAS (M6).
pub fn expand_material_ranges_to_tri_ids(
    ranges: &[(u32, u32, u32)],
    index_count: usize,
) -> Vec<u32> {
    let n_tris = index_count / 3;
    let mut out = vec![0u32; n_tris];
    for &(mat_id, first, count) in ranges {
        if count == 0 {
            continue;
        }
        let start = (first / 3) as usize;
        let n = (count / 3) as usize;
        for t in start..start.saturating_add(n).min(n_tris) {
            out[t] = mat_id;
        }
    }
    out
}

// ============================================================
// Cache
// ============================================================

/// One body placement for TLAS assembly (from the raster instance enumeration).
pub struct PtInstanceSource {
    pub cache_id: MeshCacheId,
    pub model: Mat4,
    pub material_base: u32,
    /// M6: when set, `PtTri.material_idx` holds **scene MaterialId** (0 = inherit
    /// `material_base`). At assemble, non-zero ids remap through this map to SSBO slots.
    pub material_id_remap: Option<HashMap<u32, u32>>,
}

/// Matrices and bounds required by GPU traversal. A transform is traceable
/// when the model, its computed inverses, and the resulting world bounds are
/// all finite. Checking the results avoids an arbitrary determinant cutoff
/// that incorrectly rejected valid, very small instances.
fn traceable_transform(model: Mat4, local_aabb: &Aabb) -> Option<(Mat4, Mat3, Aabb)> {
    if !model.to_cols_array().iter().all(|v| v.is_finite()) {
        return None;
    }

    let inv_model = model.inverse();
    let normal_mat = Mat3::from_mat4(model).inverse().transpose();
    if !inv_model.to_cols_array().iter().all(|v| v.is_finite())
        || !normal_mat.to_cols_array().iter().all(|v| v.is_finite())
    {
        return None;
    }

    let world_bounds = world_aabb(local_aabb, &model);
    if !world_bounds
        .min
        .iter()
        .chain(world_bounds.max.iter())
        .all(|v| v.is_finite())
    {
        return None;
    }
    Some((inv_model, normal_mat, world_bounds))
}

/// Assembled scene — concatenated CPU arrays ready for SSBO upload.
#[derive(Default)]
pub struct PtSceneCpu {
    pub tlas: Vec<BvhNode>,
    pub instances: Vec<PtInstanceGpu>,
    pub blas_nodes: Vec<BvhNode>,
    pub tris: Vec<PtTri>,
    pub verts: Vec<PtVertex>,
}

impl PtSceneCpu {
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }
}

pub struct PtAccelCache {
    built: HashMap<MeshCacheId, BlasBuilt>,
    /// Per-body generation — results with a stale generation are discarded
    /// at the single insertion point in `poll()` (the `MeshCache` pattern).
    generations: HashMap<MeshCacheId, u64>,
    /// (cache_id → source_hash) of builds currently on the rayon pool.
    in_flight: HashMap<MeshCacheId, u64>,
    /// (cache_id → source_hash) of bodies deliberately skipped at this stamp
    /// (e.g. over the per-body triangle ceiling). Treated as "current" so the
    /// requester does not re-probe/re-log every poll; a stamp change clears it
    /// naturally (hash mismatch) and `remove` clears it explicitly.
    skipped: HashMap<MeshCacheId, u64>,
    result_rx: mpsc::Receiver<BlasBuilt>,
    result_tx: mpsc::Sender<BlasBuilt>,
}

impl Default for PtAccelCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PtAccelCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            built: HashMap::new(),
            generations: HashMap::new(),
            in_flight: HashMap::new(),
            skipped: HashMap::new(),
            result_rx: rx,
            result_tx: tx,
        }
    }

    /// True if this body's BLAS is built AND matches `source_hash`.
    pub fn is_current(&self, id: MeshCacheId, source_hash: u64) -> bool {
        self.built.get(&id).map_or(false, |b| b.source_hash == source_hash)
            || self.skipped.get(&id) == Some(&source_hash)
    }

    /// Record that this body was deliberately not built at `source_hash`
    /// (per-body BLAS ceiling). `is_current` then reports it as current, so
    /// retain-only re-requests apply and the skip is logged once per stamp.
    pub fn mark_skipped(&mut self, id: MeshCacheId, source_hash: u64) {
        self.skipped.insert(id, source_hash);
    }

    /// Queue a background BLAS build for `id` unless already built/in-flight
    /// for the same hash. Takes owned mesh data (moved to the rayon job).
    /// Returns true if a job was actually queued.
    pub fn request(
        &mut self,
        id: MeshCacheId,
        source_hash: u64,
        raw_verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
    ) -> bool {
        self.request_ex(id, source_hash, raw_verts, indices, None)
    }

    /// Like [`request`] with optional per-triangle scene MaterialIds (M6 multi-material).
    pub fn request_ex(
        &mut self,
        id: MeshCacheId,
        source_hash: u64,
        raw_verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
        tri_mat_ids: Option<Vec<u32>>,
    ) -> bool {
        if self.is_current(id, source_hash) {
            return false;
        }
        if self.in_flight.get(&id) == Some(&source_hash) {
            return false;
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
            let mats = tri_mat_ids.as_deref();
            let (verts, tris, nodes, local_aabb) =
                build_blas_entry_ex(&raw_verts, &indices, mats);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if ms > 5.0 {
                eprintln!("[PtBlas] cid={:?} built: tris={} nodes={} in {:.1}ms (background)", id, tris.len(), nodes.len(), ms);
            }
            // Send may fail if the cache was dropped — nothing to do then.
            let _ = tx.send(BlasBuilt {
                source_hash,
                verts,
                tris,
                nodes,
                local_aabb,
                generation,
                cache_id: id,
            });
        });
        true
    }

    /// Drain finished builds. Returns true if any BLAS changed (caller should
    /// re-assemble + re-upload). Stale generations are discarded silently.
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

    /// Drop a body's BLAS (body deleted / mesh removed).
    pub fn remove(&mut self, id: MeshCacheId) {
        // Invalidate every result already running for this id. Merely removing
        // `in_flight` is not sufficient: a late job carries the previous
        // generation and would otherwise be accepted by `poll()`, resurrecting
        // deleted geometry in the CPU cache.
        let generation = self.generations.entry(id).or_insert(0);
        *generation = generation.wrapping_add(1);
        self.built.remove(&id);
        self.in_flight.remove(&id);
        self.skipped.remove(&id);
    }

    /// Drop cached/in-flight BLAS entries no longer owned by the mesh cache.
    /// This bounds CPU memory over long edit sessions without churning hidden
    /// meshes, and invalidates late background results through `remove`.
    pub fn retain_live<F>(&mut self, mut is_live: F)
    where
        F: FnMut(MeshCacheId) -> bool,
    {
        let stale: Vec<MeshCacheId> = self
            .built
            .keys()
            .chain(
                self.in_flight
                    .keys()
                    .filter(|id| !self.built.contains_key(*id)),
            )
            .copied()
            .filter(|id| !is_live(*id))
            .collect();
        for id in stale {
            self.remove(id);
        }
        // Skipped-only ids (never built or in flight) are not covered by the
        // chain above — drop dead ones directly.
        self.skipped.retain(|id, _| is_live(*id));
    }

    /// Body-local AABB of a built BLAS (diagnostics / external queries).
    pub fn local_aabb(&self, id: MeshCacheId) -> Option<(Vec3, Vec3)> {
        let b = self.built.get(&id)?;
        Some((
            Vec3::from_array(b.local_aabb.min),
            Vec3::from_array(b.local_aabb.max),
        ))
    }

    /// Ordered unique cache IDs whose geometry assembly will concatenate. The
    /// app stores this geometry-layout signature for the refit gate. Instance
    /// count/order may change safely when this sequence is stable; a new mesh
    /// or a validity transition that reorders unique geometry forces assembly.
    pub fn layout_ids(&self, instances: &[PtInstanceSource]) -> Vec<MeshCacheId> {
        let mut seen = HashSet::new();
        instances
            .iter()
            .filter_map(|src| {
                let b = self.built.get(&src.cache_id)?;
                if b.nodes.is_empty() || b.tris.is_empty() {
                    return None;
                }
                if traceable_transform(src.model, &b.local_aabb).is_none()
                    || !seen.insert(src.cache_id)
                {
                    return None;
                }
                Some(src.cache_id)
            })
            .collect()
    }

    /// Assemble the frame's scene: concatenate every instance whose BLAS is
    /// built into the global arrays + build the TLAS over world AABBs.
    /// Instances whose BLAS isn't ready yet are skipped (they appear once
    /// their build lands — the raster view covers the gap).
    pub fn assemble(&self, instances: &[PtInstanceSource]) -> PtSceneCpu {
        self.assemble_impl(instances, true)
    }

    /// TLAS REFIT fast path (Phase 3 — P3.2): rebuild ONLY the TLAS + instance
    /// table for a transform/material-only change. Valid IFF the caller proved
    /// the instance `cache_id` sequence AND the built-BLAS set are unchanged
    /// since the last full `assemble` — then the geometry concatenation
    /// (verts/tris/blas_nodes) and every per-instance offset reproduce exactly,
    /// so the previously uploaded geometry SSBOs stay valid untouched.
    /// Returns (tlas, instances); geometry arrays in the result are empty.
    pub fn refit(&self, instances: &[PtInstanceSource]) -> PtSceneCpu {
        self.assemble_impl(instances, false)
    }

    /// Single shared walk for `assemble` / `refit` — one filtering + offset
    /// rule, so the two can never diverge. `copy_geo = false` advances the
    /// offsets without copying geometry (the offsets stored in each
    /// `PtInstanceGpu` still must match the concatenated layout byte-for-byte).
    fn assemble_impl(&self, instances: &[PtInstanceSource], copy_geo: bool) -> PtSceneCpu {
        let mut scene = PtSceneCpu::default();
        let mut tlas_bodies: Vec<SceneBvhBody> = Vec::new();
        // Multiple scene instances may place the same mesh. They share one
        // concatenated vertex/triangle/BLAS slice; only PtInstanceGpu and TLAS
        // leaves are per placement. This avoids multiplying VRAM and upload
        // bandwidth for duplicated/instanced bodies.
        let mut geometry_offsets: HashMap<MeshCacheId, (u32, u32, u32)> = HashMap::new();
        // Offset counters used when geometry is not copied.
        let (mut n_verts, mut n_tris, mut n_blas) = (0u32, 0u32, 0u32);

        for src in instances {
            let Some(b) = self.built.get(&src.cache_id) else { continue };
            if b.nodes.is_empty() || b.tris.is_empty() {
                continue; // empty mesh — nothing to trace
            }
            // A zero-scale or non-finite body has no valid world-to-local ray
            // transform. Feeding its inverse into WGSL poisons traversal and
            // progressive accumulation with NaNs; skip it until valid again.
            let Some((inv_model, n3, world_bounds)) =
                traceable_transform(src.model, &b.local_aabb)
            else {
                continue;
            };
            let (vert_off, tri_off, blas_node_off) =
                if let Some(&offsets) = geometry_offsets.get(&src.cache_id) {
                    offsets
                } else {
                    let offsets = (n_verts, n_tris, n_blas);
                    n_verts += b.verts.len() as u32;
                    n_tris += b.tris.len() as u32;
                    n_blas += b.nodes.len() as u32;

                    if copy_geo {
                        scene.verts.extend_from_slice(&b.verts);
                        // Rebase triangle vertex indices into the global vert array.
                        // M6: remap scene MaterialId → SSBO slot when instance provides a map.
                        // 0 stays 0 (shader: inherit material_base).
                        let remap = src.material_id_remap.as_ref();
                        let rebased = |t: &PtTri| {
                            let mid = t.material_idx;
                            let slot = if mid == 0 {
                                0
                            } else {
                                remap
                                    .and_then(|m| m.get(&mid).copied())
                                    .unwrap_or(0)
                            };
                            PtTri {
                                v0: t.v0 + offsets.0,
                                v1: t.v1 + offsets.0,
                                v2: t.v2 + offsets.0,
                                material_idx: slot,
                            }
                        };
                        if b.tris.len() >= 262_144 {
                            // Parallel rebase for snapshot-scale bodies (order-preserving).
                            use rayon::prelude::*;
                            let start = scene.tris.len();
                            scene.tris.resize(start + b.tris.len(), PtTri { v0: 0, v1: 0, v2: 0, material_idx: 0 });
                            scene.tris[start..].par_iter_mut().zip(b.tris.par_iter()).for_each(|(dst, src)| *dst = rebased(src));
                        } else {
                            scene.tris.extend(b.tris.iter().map(rebased));
                        }
                        scene.blas_nodes.extend_from_slice(&b.nodes);
                    }
                    geometry_offsets.insert(src.cache_id, offsets);
                    offsets
                };

            let instance_idx = scene.instances.len() as u32;
            scene.instances.push(PtInstanceGpu {
                model: src.model.to_cols_array_2d(),
                inv_model: inv_model.to_cols_array_2d(),
                normal_mat: [
                    [n3.x_axis.x, n3.x_axis.y, n3.x_axis.z, 0.0],
                    [n3.y_axis.x, n3.y_axis.y, n3.y_axis.z, 0.0],
                    [n3.z_axis.x, n3.z_axis.y, n3.z_axis.z, 0.0],
                ],
                blas_node_off,
                tri_off,
                vert_off,
                material_base: src.material_base,
            });

            tlas_bodies.push(SceneBvhBody {
                aabb: world_bounds,
                start_entry: instance_idx,
                entry_count: 1,
            });
        }

        scene.tlas = build_scene_bvh(&tlas_bodies);
        scene
    }
}

/// World-space AABB of a body-local AABB under `model` (all 8 corners).
fn world_aabb(local: &Aabb, model: &Mat4) -> Aabb {
    let mut out = Aabb::empty();
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { local.min[0] } else { local.max[0] },
            if i & 2 == 0 { local.min[1] } else { local.max[1] },
            if i & 4 == 0 { local.min[2] } else { local.max[2] },
        );
        let w = model.transform_point3(c);
        out = out.union(&Aabb { min: [w.x, w.y, w.z], max: [w.x, w.y, w.z] });
    }
    out
}

// ============================================================
// CPU reference tracer — the golden oracle for the WGSL traversal.
// Mirrors the shader structure 1:1 (TLAS short-stack → ray into body space
// via inv_model → BLAS short-stack → Möller–Trumbore, nearest hit wins).
// The direction is NOT renormalized after the instance transform, so `t` is
// world-consistent across instances with rotation/scale.
// ============================================================

/// A CPU-traced hit (world-consistent `t`, instance + triangle identity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PtHitCpu {
    pub t: f32,
    pub instance: u32,
    pub tri: u32,
}

/// Trace one ray through the assembled scene. Reference implementation for
/// the GPU shader — keep the two in lockstep (parity tests compare them).
pub fn trace_scene_cpu(scene: &PtSceneCpu, orig: Vec3, dir: Vec3) -> Option<PtHitCpu> {
    if scene.tlas.is_empty() {
        return None;
    }
    let mut best: Option<PtHitCpu> = None;
    let mut stack: Vec<u32> = vec![0];
    while let Some(node_idx) = stack.pop() {
        let node = &scene.tlas[node_idx as usize];
        let t_max = best.map_or(f32::MAX, |h| h.t);
        if !ray_aabb_hit(orig, dir, node.aabb_min, node.aabb_max, t_max) {
            continue;
        }
        if node.count > 0 {
            let inst_idx = node.first;
            trace_instance(scene, inst_idx, orig, dir, &mut best);
        } else {
            stack.push(node.first);
            stack.push(node.first + 1);
        }
    }
    best
}

fn trace_instance(scene: &PtSceneCpu, inst_idx: u32, orig: Vec3, dir: Vec3, best: &mut Option<PtHitCpu>) {
    let inst = &scene.instances[inst_idx as usize];
    let inv = Mat4::from_cols_array_2d(&inst.inv_model);
    // Transform into body space. Direction deliberately NOT renormalized:
    // keeps `t` in world units so nearest-hit compares across instances.
    let lo = inv.transform_point3(orig);
    let ld = inv.transform_vector3(dir);

    let mut stack: Vec<u32> = vec![inst.blas_node_off];
    while let Some(node_idx) = stack.pop() {
        let node = &scene.blas_nodes[node_idx as usize];
        let t_max = best.map_or(f32::MAX, |h| h.t);
        if !ray_aabb_hit(lo, ld, node.aabb_min, node.aabb_max, t_max) {
            continue;
        }
        if node.count > 0 {
            for k in node.first..node.first + node.count {
                let tri_idx = inst.tri_off + k;
                let tri = &scene.tris[tri_idx as usize];
                let a = Vec3::from(scene.verts[tri.v0 as usize].pos);
                let b = Vec3::from(scene.verts[tri.v1 as usize].pos);
                let c = Vec3::from(scene.verts[tri.v2 as usize].pos);
                if let Some(t) = ray_tri_mt(lo, ld, a, b, c) {
                    if best.map_or(true, |h| t < h.t) {
                        *best = Some(PtHitCpu { t, instance: inst_idx, tri: tri_idx });
                    }
                }
            }
        } else {
            // Child indices are relative to THIS body's BLAS slice at build
            // time — but build_bvh child refs are absolute within the body's
            // node array, so rebase by blas_node_off.
            stack.push(inst.blas_node_off + node.first);
            stack.push(inst.blas_node_off + node.first + 1);
        }
    }
}

/// Slab test with unnormalized direction support.
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

/// Möller–Trumbore, front+back faces, `t > 1e-5`.
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

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit cube (±0.5) in the universal `[f32;8]` format, normals = +Y dummy.
    fn cube_raw() -> (Vec<[f32; 8]>, Vec<u32>) {
        let p = [
            [-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [0.5, 0.5, -0.5], [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],  [0.5, -0.5, 0.5],  [0.5, 0.5, 0.5],  [-0.5, 0.5, 0.5],
        ];
        let verts = p.iter().map(|v| [v[0], v[1], v[2], 0.0, 1.0, 0.0, 0.0, 0.0]).collect();
        let indices = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4,
            3, 7, 6, 3, 6, 2, 0, 4, 7, 0, 7, 3, 1, 2, 6, 1, 6, 5,
        ];
        (verts, indices)
    }

    /// Brute-force nearest hit over WORLD-transformed triangles of all instances.
    fn brute_world(
        bodies: &[(&[[f32; 8]], &[u32], Mat4)],
        orig: Vec3,
        dir: Vec3,
    ) -> Option<f32> {
        let mut best: Option<f32> = None;
        for (verts, indices, model) in bodies {
            for t in 0..indices.len() / 3 {
                let f = |k: usize| {
                    let v = verts[indices[t * 3 + k] as usize];
                    model.transform_point3(Vec3::new(v[0], v[1], v[2]))
                };
                if let Some(hit) = ray_tri_mt(orig, dir, f(0), f(1), f(2)) {
                    if best.map_or(true, |b| hit < b) {
                        best = Some(hit);
                    }
                }
            }
        }
        best
    }

    fn approx_opt(a: Option<f32>, b: Option<f32>) {
        match (a, b) {
            (None, None) => {}
            (Some(x), Some(y)) => assert!((x - y).abs() < 1e-3, "t mismatch: scene={x} brute={y}"),
            _ => panic!("hit/miss disagreement: scene={a:?} brute={b:?}"),
        }
    }

    #[test]
    fn blas_entry_bakes_tri_order() {
        let (mut v, i) = cube_raw();
        for (index, vertex) in v.iter_mut().enumerate() {
            vertex[6] = index as f32 * 0.125;
            vertex[7] = 1.0 - vertex[6];
        }
        let (verts, tris, nodes, aabb) = build_blas_entry(&v, &i);
        assert_eq!(verts.len(), 8);
        assert_eq!(std::mem::size_of::<PtVertex>(), 48);
        for (vertex, raw) in verts.iter().zip(&v) {
            assert_eq!(vertex.pos, [raw[0], raw[1], raw[2]]);
            assert_eq!(vertex.normal, [raw[3], raw[4], raw[5]]);
            assert_eq!(vertex.uv, [raw[6], raw[7]]);
            assert_eq!(vertex._pad0.to_bits(), raw[6].to_bits());
            assert_eq!(vertex._pad1.to_bits(), raw[7].to_bits());
        }
        let mut painted = v.clone();
        painted[0][6] = f32::from_bits(0xff12_3456);
        painted[0][7] = 1.0;
        let (painted_vertices, _, _, _) = build_blas_entry(&painted, &i);
        assert_eq!(painted_vertices[0]._pad0.to_bits(), 0xff12_3456);
        assert_eq!(painted_vertices[0]._pad1, 1.0);
        assert_eq!(painted_vertices[0].uv[0].to_bits(), 0xff12_3456);
        assert_eq!(tris.len(), 12);
        assert!(!nodes.is_empty());
        assert!((aabb.min[0] + 0.5).abs() < 1e-6 && (aabb.max[2] - 0.5).abs() < 1e-6);
        // Every leaf range must lie inside the baked tris array.
        for n in &nodes {
            if n.count > 0 {
                assert!((n.first + n.count) as usize <= tris.len());
            }
        }
    }

    #[test]
    fn assemble_empty_scene() {
        let cache = PtAccelCache::new();
        let scene = cache.assemble(&[]);
        assert!(scene.is_empty());
        assert!(trace_scene_cpu(&scene, Vec3::new(0.0, 0.0, -5.0), Vec3::Z).is_none());
    }

    #[test]
    fn remove_rejects_late_background_result() {
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(77);
        cache.generations.insert(id, 1);
        cache.in_flight.insert(id, 0xCAFE);
        cache
            .result_tx
            .send(BlasBuilt {
                source_hash: 0xCAFE,
                verts: Vec::new(),
                tris: Vec::new(),
                nodes: Vec::new(),
                local_aabb: Aabb { min: [0.0; 3], max: [0.0; 3] },
                generation: 1,
                cache_id: id,
            })
            .expect("test result channel");

        cache.remove(id);
        assert!(!cache.poll(), "removed geometry must not be resurrected");
        assert!(!cache.built.contains_key(&id));
        assert!(!cache.in_flight.contains_key(&id));
    }

    #[test]
    fn singular_instance_is_excluded_from_tlas() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(78);
        cache.request(id, 1, v, i);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }

        let scene = cache.assemble(&[PtInstanceSource {
            cache_id: id,
            model: Mat4::from_scale(Vec3::new(1.0, 0.0, 1.0)),
            material_base: 0,
            material_id_remap: None,
        }]);
        assert!(scene.instances.is_empty());
        assert!(scene.tlas.is_empty());
        assert!(scene.verts.is_empty());
        assert!(scene.tris.is_empty());

        // Small but invertible transforms are valid. The old absolute
        // determinant cutoff rejected this scale even though both inverse
        // matrices are finite.
        let tiny = cache.assemble(&[PtInstanceSource {
            cache_id: id,
            model: Mat4::from_scale(Vec3::splat(1e-5)),
            material_base: 0,
            material_id_remap: None,
        }]);
        assert_eq!(tiny.instances.len(), 1);
        assert_eq!(tiny.tris.len(), 12);
    }

    #[test]
    fn layout_signature_tracks_transform_validity() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let a = MeshCacheId(79);
        let b = MeshCacheId(80);
        cache.request(a, 1, v.clone(), i.clone());
        cache.request(b, 1, v, i);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.is_current(a, 1) || !cache.is_current(b, 1) {
            cache.poll();
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }

        let singular = Mat4::from_scale(Vec3::new(1.0, 0.0, 1.0));
        let valid = Mat4::IDENTITY;
        let before = [
            PtInstanceSource { cache_id: a, model: singular, material_base: 0, material_id_remap: None },
            PtInstanceSource { cache_id: b, model: valid, material_base: 0, material_id_remap: None },
        ];
        let swapped = [
            PtInstanceSource { cache_id: a, model: valid, material_base: 0, material_id_remap: None },
            PtInstanceSource { cache_id: b, model: singular, material_base: 0, material_id_remap: None },
        ];
        let both_valid = [
            PtInstanceSource { cache_id: a, model: valid, material_base: 0, material_id_remap: None },
            PtInstanceSource { cache_id: b, model: valid, material_base: 0, material_id_remap: None },
        ];
        let duplicated = [
            PtInstanceSource { cache_id: a, model: valid, material_base: 0, material_id_remap: None },
            PtInstanceSource { cache_id: a, model: valid, material_base: 1, material_id_remap: None },
            PtInstanceSource { cache_id: b, model: valid, material_base: 0, material_id_remap: None },
        ];

        assert_eq!(cache.layout_ids(&before), vec![b]);
        assert_eq!(cache.layout_ids(&swapped), vec![a]);
        assert_eq!(cache.layout_ids(&both_valid), vec![a, b]);
        assert_eq!(cache.layout_ids(&duplicated), vec![a, b]);
        assert_ne!(cache.layout_ids(&before), cache.layout_ids(&swapped));
        assert_ne!(cache.layout_ids(&before), cache.layout_ids(&both_valid));
    }

    #[test]
    fn retain_live_drops_only_unowned_blas() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let keep = MeshCacheId(81);
        let drop = MeshCacheId(82);
        cache.request(keep, 1, v.clone(), i.clone());
        cache.request(drop, 1, v, i);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.is_current(keep, 1) || !cache.is_current(drop, 1) {
            cache.poll();
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }
        let old_generation = cache.generations[&drop];

        cache.retain_live(|id| id == keep);

        assert!(cache.built.contains_key(&keep));
        assert!(!cache.built.contains_key(&drop));
        assert!(cache.generations[&drop] != old_generation);
    }

    /// The Milestone B ground truth: two instances of one BLAS under different
    /// transforms (translation + rotation + non-uniform placement), traced via
    /// TLAS→inv_model→BLAS, must match brute force over world triangles —
    /// proving the offsets, index rebasing, and instance transforms are exact.
    #[test]
    fn assembled_scene_matches_brute_force() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(7);
        cache.request(id, 0xABC, v.clone(), i.clone());
        // Background build — wait for the rayon job (fast; bounded spin).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }
        assert!(cache.is_current(id, 0xABC));

        let m1 = Mat4::from_translation(Vec3::new(-2.0, 0.0, 0.0));
        let m2 = Mat4::from_translation(Vec3::new(2.0, 0.5, 0.0))
            * Mat4::from_rotation_y(0.7)
            * Mat4::from_scale(Vec3::new(1.5, 1.0, 0.8));
        let scene = cache.assemble(&[
            PtInstanceSource { cache_id: id, model: m1, material_base: 0, material_id_remap: None },
            PtInstanceSource { cache_id: id, model: m2, material_base: 0, material_id_remap: None },
        ]);
        assert_eq!(scene.instances.len(), 2);
        assert_eq!(scene.tris.len(), 12, "instances must share triangle storage");
        assert_eq!(scene.verts.len(), 8, "instances must share vertex storage");
        assert_eq!(scene.instances[0].tri_off, scene.instances[1].tri_off);
        assert_eq!(scene.instances[0].vert_off, scene.instances[1].vert_off);
        assert_eq!(scene.instances[0].blas_node_off, scene.instances[1].blas_node_off);

        let bodies: [(&[[f32; 8]], &[u32], Mat4); 2] = [(&v, &i, m1), (&v, &i, m2)];
        // Ray grid from -Z and +X through both bodies, plus misses.
        for gy in -3..=3 {
            for gx in -3..=3 {
                let (x, y) = (gx as f32 * 0.8, gy as f32 * 0.4);
                for (orig, dir) in [
                    (Vec3::new(x, y, -8.0), Vec3::Z),
                    (Vec3::new(-8.0, y, x * 0.5), Vec3::X),
                    (Vec3::new(8.0, y, x * 0.5), Vec3::NEG_X),
                ] {
                    let got = trace_scene_cpu(&scene, orig, dir).map(|h| h.t);
                    approx_opt(got, brute_world(&bodies, orig, dir));
                }
            }
        }
    }

    /// Nearest-instance identity: a ray down +X from far -X must report
    /// instance 0 (the cube at x=-2), not just the right distance.
    #[test]
    fn nearest_hit_reports_correct_instance() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(1);
        cache.request(id, 1, v, i);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }
        let scene = cache.assemble(&[
            PtInstanceSource { cache_id: id, model: Mat4::from_translation(Vec3::new(-2.0, 0.0, 0.0)), material_base: 0, material_id_remap: None },
            PtInstanceSource { cache_id: id, model: Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0)), material_base: 0, material_id_remap: None },
        ]);
        let hit = trace_scene_cpu(&scene, Vec3::new(-10.0, 0.0, 0.0), Vec3::X).expect("must hit");
        assert_eq!(hit.instance, 0);
        assert!((hit.t - 7.5).abs() < 1e-3, "expected first face of left cube at t=7.5, got {}", hit.t);
        let hit2 = trace_scene_cpu(&scene, Vec3::new(10.0, 0.0, 0.0), Vec3::NEG_X).expect("must hit");
        assert_eq!(hit2.instance, 1);
    }

    /// Micro-bench (always-on): 20 instances of one mesh must share geometry
    /// storage (Codex dedup). Stand-in for multi-placement / array instances;
    /// a single SDF body with 20 prims is still *one* mesh after DC, so the
    /// win is mainly multi-body / array cases — but the path is exercised here.
    #[test]
    fn assemble_20_shared_instances_is_fast() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(20);
        cache.request(id, 1, v, i);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }
        let srcs: Vec<_> = (0..20)
            .map(|k| PtInstanceSource {
                cache_id: id,
                model: Mat4::from_translation(Vec3::new(k as f32 * 2.5, 0.0, 0.0)),
                material_base: 0,
                material_id_remap: None,
            })
            .collect();
        let t0 = std::time::Instant::now();
        let scene = cache.assemble(&srcs);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(scene.instances.len(), 20);
        assert_eq!(scene.tris.len(), 12, "shared geo: one cube of tris");
        assert_eq!(scene.verts.len(), 8, "shared geo: one cube of verts");
        // Without dedup this would be 20× verts/tris and ~linear upload cost.
        eprintln!(
            "[bench] assemble 20 shared instances: {:.3}ms  verts={} tris={} tlas_nodes={}",
            ms,
            scene.verts.len(),
            scene.tris.len(),
            scene.tlas.len()
        );
        let layout = cache.layout_ids(&srcs);
        assert_eq!(layout, vec![id]);
        let t1 = std::time::Instant::now();
        let refit = cache.refit(&srcs);
        let refit_ms = t1.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(refit.instances.len(), 20);
        assert!(refit.verts.is_empty());
        eprintln!("[bench] refit 20 shared instances: {:.3}ms", refit_ms);
    }

    /// Refit parity: for the same instance set, `refit` must reproduce the
    /// exact TLAS + instance table of a full `assemble` (offsets included) —
    /// the invariant that lets the geometry SSBOs stay untouched on a
    /// transform-only change.
    #[test]
    fn refit_matches_full_assemble() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(9);
        cache.request(id, 42, v, i);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }
        let srcs = [
            PtInstanceSource { cache_id: id, model: Mat4::from_translation(Vec3::new(-2.0, 0.0, 0.0)), material_base: 1, material_id_remap: None },
            PtInstanceSource { cache_id: id, model: Mat4::from_rotation_y(0.4) * Mat4::from_scale(Vec3::splat(2.0)), material_base: 2, material_id_remap: None },
        ];
        let full = cache.assemble(&srcs);
        let refit = cache.refit(&srcs);
        assert_eq!(refit.instances.len(), full.instances.len());
        assert_eq!(refit.tlas.len(), full.tlas.len());
        assert!(refit.verts.is_empty() && refit.tris.is_empty() && refit.blas_nodes.is_empty());
        for (a, b) in refit.instances.iter().zip(&full.instances) {
            assert_eq!(a.blas_node_off, b.blas_node_off);
            assert_eq!(a.tri_off, b.tri_off);
            assert_eq!(a.vert_off, b.vert_off);
            assert_eq!(a.material_base, b.material_base);
            assert_eq!(a.model, b.model);
        }
        for (a, b) in refit.tlas.iter().zip(&full.tlas) {
            assert_eq!((a.first, a.count, a.aabb_min, a.aabb_max), (b.first, b.count, b.aabb_min, b.aabb_max));
        }
    }

    /// UV-sphere in the universal `[f32;8]` format — `stacks*slices*2` tris.
    /// Deterministic synthetic stand-in for the owner's 1024³ sparse-SDF snapshot
    /// (850K verts / 1.7M tris) so the BLAS build cost is measurable headlessly.
    fn uv_sphere_raw(stacks: u32, slices: u32) -> (Vec<[f32; 8]>, Vec<u32>) {
        let mut verts = Vec::with_capacity(((stacks + 1) * (slices + 1)) as usize);
        for st in 0..=stacks {
            let phi = std::f32::consts::PI * st as f32 / stacks as f32;
            for sl in 0..=slices {
                let theta = 2.0 * std::f32::consts::PI * sl as f32 / slices as f32;
                let (sp, cp) = phi.sin_cos();
                let (stt, ct) = theta.sin_cos();
                let n = [sp * ct, cp, sp * stt];
                verts.push([n[0], n[1], n[2], n[0], n[1], n[2], 0.0, 0.0]);
            }
        }
        let mut indices = Vec::with_capacity((stacks * slices * 6) as usize);
        let row = slices + 1;
        for st in 0..stacks {
            for sl in 0..slices {
                let a = st * row + sl;
                let b = a + row;
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        (verts, indices)
    }

    /// Latency benchmark for the owner's 1.7M-tri case. Run explicitly:
    /// `cargo test -p render --release -- --ignored bench_blas_build_1_7m --nocapture`
    #[test]
    #[ignore = "benchmark — run with --release --ignored --nocapture"]
    fn bench_blas_build_1_7m() {
        let (v, i) = uv_sphere_raw(925, 925);
        eprintln!("[bench] mesh: verts={} tris={}", v.len(), i.len() / 3);
        // Warm-up (rayon pool spin-up) on a smaller mesh.
        let (wv, wi) = uv_sphere_raw(100, 100);
        let _ = build_blas_entry(&wv, &wi);
        for run in 0..3 {
            let t0 = std::time::Instant::now();
            let (verts, tris, nodes, _aabb) = build_blas_entry(&v, &i);
            eprintln!(
                "[bench] run {run}: build_blas_entry {:.1}ms (verts={} tris={} nodes={})",
                t0.elapsed().as_secs_f64() * 1000.0, verts.len(), tris.len(), nodes.len()
            );
        }
        // Frame-thread stages: source clone (request) + full assemble.
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(99);
        let t0 = std::time::Instant::now();
        let (vc, ic) = (v.clone(), i.clone());
        eprintln!("[bench] source clone {:.1}ms", t0.elapsed().as_secs_f64() * 1000.0);
        cache.request(id, 1, vc, ic);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        for run in 0..3 {
            let t0 = std::time::Instant::now();
            let scene = cache.assemble(&[PtInstanceSource { cache_id: id, model: Mat4::IDENTITY, material_base: 0, material_id_remap: None }]);
            eprintln!("[bench] run {run}: assemble {:.1}ms (tris={} nodes={})",
                t0.elapsed().as_secs_f64() * 1000.0, scene.tris.len(), scene.blas_nodes.len());
        }
    }

    /// Parity gate for the parallel BLAS builder: on a mesh large enough to take
    /// the parallel path, traversal must still match brute force exactly, every
    /// triangle must appear exactly once, and node AABBs must contain children.
    #[test]
    fn parallel_blas_matches_brute_force() {
        let (v, i) = uv_sphere_raw(96, 96); // 18,432 tris — well past the parallel threshold
        let (_verts, tris, nodes, _aabb) = build_blas_entry(&v, &i);
        assert_eq!(tris.len(), i.len() / 3);
        // Node invariants: internal children in-bounds + AABB containment; leaves in-bounds.
        for n in &nodes {
            if n.count == 0 {
                let l = n.first as usize;
                assert!(l + 1 < nodes.len(), "child OOB");
                for c in [l, l + 1] {
                    for a in 0..3 {
                        assert!(nodes[c].aabb_min[a] >= n.aabb_min[a] - 1e-4);
                        assert!(nodes[c].aabb_max[a] <= n.aabb_max[a] + 1e-4);
                    }
                }
            } else {
                assert!((n.first + n.count) as usize <= tris.len());
            }
        }
        // Every leaf range must be disjoint and cover all tris exactly once.
        let mut covered = vec![false; tris.len()];
        for n in &nodes {
            if n.count > 0 {
                for k in n.first..n.first + n.count {
                    assert!(!covered[k as usize], "tri covered twice");
                    covered[k as usize] = true;
                }
            }
        }
        assert!(covered.iter().all(|&c| c), "every tri must be reachable from a leaf");
        // Traversal parity vs brute force through the assembled-scene tracer.
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(21);
        cache.request(id, 7, v.clone(), i.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }
        let scene = cache.assemble(&[PtInstanceSource { cache_id: id, model: Mat4::IDENTITY, material_base: 0, material_id_remap: None }]);
        let bodies: [(&[[f32; 8]], &[u32], Mat4); 1] = [(&v, &i, Mat4::IDENTITY)];
        for gy in -4..=4 {
            for gx in -4..=4 {
                let (x, y) = (gx as f32 * 0.22, gy as f32 * 0.22);
                for (orig, dir) in [
                    (Vec3::new(x, y, -5.0), Vec3::Z),
                    (Vec3::new(-5.0, y, x), Vec3::X),
                    (Vec3::new(x, 5.0, y), Vec3::NEG_Y),
                ] {
                    let got = trace_scene_cpu(&scene, orig, dir).map(|h| h.t);
                    approx_opt(got, brute_world(&bodies, orig, dir));
                }
            }
        }
    }

    /// M6 — expand ranges → per-tri MaterialIds; BLAS preserves them through reorder.
    #[test]
    fn m6_tri_material_ids_survive_blas_reorder() {
        // Two tris, different mats. Range format: (mat_id, first_index, index_count).
        let ranges = [(1u32, 0u32, 3u32), (2u32, 3u32, 3u32)];
        let expanded = expand_material_ranges_to_tri_ids(&ranges, 6);
        assert_eq!(expanded, vec![1, 2]);

        // Minimal 2-tri mesh (4 verts, 2 tris).
        let v = vec![
            [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        ];
        let i = vec![0u32, 1, 2, 1, 3, 2];
        let (_verts, tris, _nodes, _aabb) =
            build_blas_entry_ex(&v, &i, Some(&expanded));
        assert_eq!(tris.len(), 2);
        let mats: std::collections::HashSet<u32> =
            tris.iter().map(|t| t.material_idx).collect();
        assert!(
            mats.contains(&1) && mats.contains(&2),
            "both material ids must appear after BLAS reorder: {mats:?}"
        );

        // Assemble remap: scene id 1→slot 3, id 2→slot 7.
        let mut remap = HashMap::new();
        remap.insert(1, 3);
        remap.insert(2, 7);
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(9001);
        assert!(cache.request_ex(id, 1, v, i, Some(expanded)));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS timed out");
            std::thread::yield_now();
        }
        let scene = cache.assemble(&[PtInstanceSource {
            cache_id: id,
            model: Mat4::IDENTITY,
            material_base: 0,
            material_id_remap: Some(remap),
        }]);
        let slots: std::collections::HashSet<u32> =
            scene.tris.iter().map(|t| t.material_idx).collect();
        assert!(
            slots.contains(&3) && slots.contains(&7),
            "assemble must remap scene MaterialIds to SSBO slots: {slots:?}"
        );
    }

    /// A re-request with a new hash supersedes the old BLAS (generation guard).
    #[test]
    fn rebuild_supersedes_old_blas() {
        let (v, i) = cube_raw();
        let mut cache = PtAccelCache::new();
        let id = MeshCacheId(3);
        cache.request(id, 100, v.clone(), i.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.poll() {
            assert!(std::time::Instant::now() < deadline, "BLAS build timed out");
            std::thread::yield_now();
        }
        assert!(cache.is_current(id, 100));

        // Same mesh, new hash → re-queue accepted; old entry replaced on poll.
        let queued = cache.request(id, 200, v, i);
        assert!(queued);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cache.is_current(id, 200) {
            cache.poll();
            assert!(std::time::Instant::now() < deadline, "rebuild timed out");
            std::thread::yield_now();
        }
        // Duplicate request for the current hash is a no-op.
        let (v2, i2) = cube_raw();
        assert!(!cache.request(id, 200, v2, i2));
    }
}
