use std::collections::HashMap;
use std::sync::mpsc;

use glam::Vec3;
use core_math::mesh::{DcConfig, DcGridConfig, Mesh, dual_contouring, dual_contouring_from_grid};
use core_math::primitives::SdfPrimitive;
use core_math::scene_graph::MeshCacheId;

use crate::mesh_gpu::GpuMesh;

// ============================================================
// LOD levels
// ============================================================

/// Level of detail for cached meshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshLod {
    /// Fast preview during editing (64³ grid).
    Preview,
    /// Medium quality after brief idle (128³ grid).
    Medium,
    /// High quality for export / final render (256³ grid).
    High,
}

impl MeshLod {
    pub fn resolution(self) -> u32 {
        match self {
            Self::Preview => 64,
            Self::Medium => 128,
            Self::High => 256,
        }
    }
}

// ============================================================
// Cached mesh entry
// ============================================================

/// A single cached mesh for an SDF body.
/// Local-space AABB of a vertex set (A1 Assembly frustum culling). Empty → zero-extent box at origin.
pub fn mesh_local_aabb(verts: &[Vec3]) -> (Vec3, Vec3) {
    if verts.is_empty() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    let mut mn = verts[0];
    let mut mx = verts[0];
    for &v in &verts[1..] {
        mn = mn.min(v);
        mx = mx.max(v);
    }
    (mn, mx)
}

pub struct CachedMesh {
    /// CPU-side mesh data (vertices, normals, quads).
    pub cpu_mesh: Mesh,
    /// GPU-uploaded buffers (created lazily when device is available).
    pub gpu_mesh: Option<GpuMesh>,
    /// LOD level this mesh was extracted at.
    pub lod: MeshLod,
    /// Hash of the source SDF primitives — used for invalidation.
    pub source_hash: u64,
    /// Triangle count (quads × 2).
    pub triangle_count: u32,
    /// Local-space AABB of `cpu_mesh`, computed once at store time (A1 frustum culling).
    pub local_min: Vec3,
    pub local_max: Vec3,
    /// Monotonic change stamp, bumped on EVERY content mutation (insert, editable
    /// update, in-place vert write, recenter shift). Universal invalidation key for
    /// consumers that need "did this mesh change?" across ALL mesh kinds —
    /// `source_hash` only covers DC bodies (editable/imported store 0). The PT
    /// accel cache keys its per-body BLAS on this.
    pub content_stamp: u64,
    /// Per-face material batches for multi-material poly bodies:
    /// `(material_id, first_index, index_count)` contiguous ranges into the index buffer.
    /// `None` / empty = single full-buffer draw (legacy path).
    pub material_ranges: Option<Vec<(u32, u32, u32)>>,
}

// ============================================================
// Background meshing result
// ============================================================

/// Result sent from a background meshing thread.
struct MeshResult {
    cache_id: MeshCacheId,
    mesh: Mesh,
    lod: MeshLod,
    source_hash: u64,
    /// Generation counter at dispatch time — stale results are discarded.
    generation: u64,
}

// ============================================================
// Mesh Cache
// ============================================================

/// Manages cached meshes for SDF bodies.
///
/// When an SDF body changes, the cache detects the hash mismatch and
/// queues a background Dual Contouring job on the rayon thread pool.
/// Results arrive via an mpsc channel and are picked up on the next
/// `poll()` call. A generation counter ensures stale results (from
/// bodies that were edited again before the job completed) are
/// discarded silently.
pub struct MeshCache {
    meshes: HashMap<MeshCacheId, CachedMesh>,
    next_id: u64,
    /// Monotonic source for `CachedMesh::content_stamp` (starts at 1 so 0 can mean "never seen").
    stamp_counter: u64,
    /// Channel for receiving completed meshes from background threads.
    result_rx: mpsc::Receiver<MeshResult>,
    result_tx: mpsc::Sender<MeshResult>,
    /// Track which (cache_id, hash) pairs are currently being meshed
    /// to avoid duplicate jobs.
    in_flight: Vec<(MeshCacheId, u64)>,
    /// Per-body generation counter. Incremented on each new request.
    /// Results with a stale generation are discarded at the single
    /// insertion point in `poll()`.
    generations: HashMap<MeshCacheId, u64>,
    /// Raster data for imported meshes (awaiting GPU upload).
    imported_raster: HashMap<MeshCacheId, (Vec<[f32; 8]>, Vec<u32>)>,
    /// Persistent copy of imported mesh data (survives GPU upload, used for save/load).
    imported_persistent: HashMap<MeshCacheId, (Vec<[f32; 8]>, Vec<u32>, u32)>,
    /// OPTIONAL per-chunk surface-net CELL ID for each vertex of a voxel-sculpt chunk mesh
    /// (parallel to that mesh's vertex array, one `[cx,cy,cz]` per vertex). Lets the zero-remesh
    /// preview paths re-derive a vertex's colour / position from the live density WITHOUT re-running
    /// Surface-Nets quad emission. ONLY voxel chunk meshes carry this; every other mesh kind defaults
    /// to absent (None). Written/cleared ATOMICALLY with the verts (`update_editable_mesh` clears any
    /// stale entry; the voxel path attaches the matching cells via `set_chunk_cells`) so an in-place
    /// preview can never write to the wrong vertices.
    imported_cells: HashMap<MeshCacheId, Vec<[u32; 3]>>,
    /// Ids whose queued raster is a FULL REPLACE (`update_editable_mesh_ex`), so `upload_pending`
    /// must write every vertex instead of diffing against `imported_persistent`.
    ///
    /// WHY: the dirty-run diff treats `imported_persistent` as "what the GPU holds", but a full
    /// replace overwrites that map with the NEW verts before the upload runs. When the replacement
    /// keeps the same index count (a bevel drag at constant segments regenerates identical
    /// topology every frame) the diff then compared the new verts against themselves, found no
    /// changed runs and uploaded NOTHING — the solid froze while the tool HUD kept moving.
    full_replace_pending: std::collections::HashSet<MeshCacheId>,
}

impl MeshCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            meshes: HashMap::new(),
            next_id: 1,
            stamp_counter: 1,
            result_rx: rx,
            result_tx: tx,
            in_flight: Vec::new(),
            generations: HashMap::new(),
            imported_raster: HashMap::new(),
            imported_persistent: HashMap::new(),
            imported_cells: HashMap::new(),
            full_replace_pending: std::collections::HashSet::new(),
        }
    }

    /// Allocate a new cache ID.
    pub fn alloc_id(&mut self) -> MeshCacheId {
        let id = MeshCacheId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Ensure the internal ID counter is at least `min_next` to avoid
    /// collisions with restored IDs from a loaded project file.
    pub fn ensure_next_id_at_least(&mut self, min_next: u64) {
        if self.next_id < min_next {
            self.next_id = min_next;
        }
    }

    /// Get a cached mesh by ID.
    pub fn get(&self, id: MeshCacheId) -> Option<&CachedMesh> {
        self.meshes.get(&id)
    }

    /// Get a mutable cached mesh by ID.
    pub fn get_mut(&mut self, id: MeshCacheId) -> Option<&mut CachedMesh> {
        self.meshes.get_mut(&id)
    }

    /// Remove a cached mesh and all related data (imported raster, persistent copy).
    pub fn remove(&mut self, id: MeshCacheId) {
        // Async-built IDs keep a tombstone generation so a result already on
        // the rayon pool cannot recreate a deleted mesh after its in-flight
        // marker is removed. Imported/editable IDs never have a generation or
        // late result, so avoid creating permanent tombstones for those.
        if let Some(generation) = self.generations.get_mut(&id) {
            *generation = generation.wrapping_add(1);
        }
        self.meshes.remove(&id);
        self.in_flight.retain(|(cid, _)| *cid != id);
        self.imported_raster.remove(&id);
        self.imported_persistent.remove(&id);
        self.imported_cells.remove(&id);
    }

    /// Check if a mesh needs remeshing (hash mismatch or missing).
    pub fn needs_update(&self, id: MeshCacheId, source_hash: u64) -> bool {
        match self.meshes.get(&id) {
            None => true,
            Some(cached) => cached.source_hash != source_hash,
        }
    }

    /// Request a background mesh job for an SDF body.
    ///
    /// `primitives` are cloned and sent to the rayon thread pool for DC extraction.
    /// The result will be available on the next `poll()` call.
    /// A generation counter is assigned — if the body is edited again before this
    /// job completes, the stale result is discarded in `poll()`.
    pub fn request_mesh(
        &mut self,
        cache_id: MeshCacheId,
        primitives: Vec<SdfPrimitive>,
        source_hash: u64,
        lod: MeshLod,
        bounds_min: Vec3,
        bounds_max: Vec3,
    ) {
        // Don't duplicate in-flight jobs for the same (id, hash).
        if self.in_flight.iter().any(|(cid, h)| *cid == cache_id && *h == source_hash) {
            return;
        }
        // Debounce: if this body already has ANY in-flight job, skip.
        // The in-flight job will be discarded as stale (generation check),
        // but we avoid burning CPU on meshes that will be thrown away.
        if self.in_flight.iter().any(|(cid, _)| *cid == cache_id) {
            return;
        }
        // Cap concurrent DC jobs to avoid freezing UI on many dirty bodies.
        // Bodies that don't get a slot stay dirty and retry next frame.
        const MAX_CONCURRENT_DC_JOBS: usize = 4;
        if self.in_flight.len() >= MAX_CONCURRENT_DC_JOBS {
            return;
        }
        self.in_flight.push((cache_id, source_hash));

        // Increment generation for this body — any in-flight result with an
        // older generation will be discarded when it arrives.
        let gen = self.generations.entry(cache_id).or_insert(0);
        *gen += 1;
        let generation = *gen;

        let tx = self.result_tx.clone();
        let resolution = lod.resolution();

        // Use rayon thread pool instead of unbounded std::thread::spawn.
        // This caps parallelism to the number of CPU cores and enables
        // work-stealing across all mesh jobs.
        rayon::spawn(move || {
            let config = DcConfig {
                resolution,
                bounds_min,
                bounds_max,
                sharp_features: true,
                normal_eps: 0.005,
            };
            let mesh = dual_contouring(&primitives, &config);
            let _ = tx.send(MeshResult {
                cache_id,
                mesh,
                lod,
                source_hash,
                generation,
            });
        });
    }

    /// Request a background mesh job from a pre-baked SDF grid (fast path).
    ///
    /// Skips redundant SDF evaluation — runs DC directly on the grid values.
    /// Normals computed via finite differences on the grid.
    pub fn request_mesh_from_grid(
        &mut self,
        cache_id: MeshCacheId,
        grid: Vec<f32>,
        nverts: u32,
        source_hash: u64,
        lod: MeshLod,
        bounds_min: Vec3,
        bounds_max: Vec3,
    ) {
        if self.in_flight.iter().any(|(cid, h)| *cid == cache_id && *h == source_hash) {
            return;
        }
        if self.in_flight.iter().any(|(cid, _)| *cid == cache_id) {
            return;
        }
        const MAX_CONCURRENT_DC_JOBS: usize = 4;
        if self.in_flight.len() >= MAX_CONCURRENT_DC_JOBS {
            return;
        }
        self.in_flight.push((cache_id, source_hash));

        let gen = self.generations.entry(cache_id).or_insert(0);
        *gen += 1;
        let generation = *gen;

        let tx = self.result_tx.clone();
        rayon::spawn(move || {
            let config = DcGridConfig {
                nverts,
                bounds_min,
                bounds_max,
                sharp_features: true,
            };
            let mesh = dual_contouring_from_grid(&grid, &config);
            let _ = tx.send(MeshResult {
                cache_id,
                mesh,
                lod,
                source_hash,
                generation,
            });
        });
    }

    /// Poll for completed background mesh jobs. Returns IDs of meshes that were updated.
    ///
    /// **Generation counter check (HARD RULE):** When a result arrives, its generation
    /// is compared to the current generation for that body. If stale (body was edited
    /// again since dispatch), the result is silently discarded. This is the single
    /// insertion point — no stale result may ever overwrite a newer one.
    pub fn poll(&mut self) -> Vec<MeshCacheId> {
        let mut updated = Vec::new();
        let mut processed = 0;
        while processed < 8 {
            let result = match self.result_rx.try_recv() {
                Ok(r) => r,
                Err(_) => break,
            };
            processed += 1;
            // GENERATION CHECK — discard stale results
            let current_gen = self.generations.get(&result.cache_id).copied().unwrap_or(0);
            if result.generation != current_gen {
                // Stale result — body was edited/deleted since dispatch. Do not
                // touch `in_flight`: any marker now present belongs to the
                // current generation and may legitimately reuse the same hash.
                continue;
            }

            let tri_count = result.mesh.quads.len() as u32 * 2;
            let (local_min, local_max) = mesh_local_aabb(&result.mesh.vertices);
            self.meshes.insert(
                result.cache_id,
                CachedMesh {
                    cpu_mesh: result.mesh,
                    gpu_mesh: None, // uploaded lazily when device is available
                    lod: result.lod,
                    source_hash: result.source_hash,
                    triangle_count: tri_count,
                    local_min,
                    local_max,
                    content_stamp: { self.stamp_counter += 1; self.stamp_counter },
                    material_ranges: None,
                },
            );
            self.in_flight.retain(|(cid, h)| {
                !(*cid == result.cache_id && *h == result.source_hash)
            });
            updated.push(result.cache_id);
        }
        updated
    }

    /// Upload GPU buffers for any cached mesh that doesn't have them yet.
    /// Capped at MAX_MESH_UPLOADS_PER_FRAME to avoid per-frame stalls when
    /// many bodies complete DC extraction simultaneously.
    pub fn upload_pending(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // FRAME-BUDGET drain (was a flat 64/frame). A 1024³ remesh produces ~9000 fresh chunk meshes at
        // once; a flat cap stretched their GPU upload over ~145 frames (a visible tail). A time budget
        // drains as many as fit — collapsing the backlog to a few frames — while still bounding a normal
        // frame (most uploads are cheap `write_buffer` updates; only first-time chunks create a buffer).
        // HARD_CAP is a runaway backstop.
        const UPLOAD_BUDGET_MS: f32 = 5.0;
        const HARD_CAP: usize = 4096;
        let t_start = std::time::Instant::now();
        let over_budget = |n: usize| n >= HARD_CAP || t_start.elapsed().as_secs_f32() * 1000.0 >= UPLOAD_BUDGET_MS;
        let mut upload_count = 0;

        // Re-queue persistent editable rasters that have no live GPU solid (or a zero-index
        // solid). Poly/NURBS `cpu_mesh` stores positions only (empty quads) — the DC fallback
        // below would upload index_count=0 and the whole surface "vanishes" until undo.
        // Prefer `imported_persistent` (survives after imported_raster is consumed).
        {
            let recover: Vec<MeshCacheId> = self
                .meshes
                .iter()
                .filter(|(id, c)| {
                    let dead = c
                        .gpu_mesh
                        .as_ref()
                        .map(|g| g.index_count == 0)
                        .unwrap_or(true);
                    dead
                        && !self.imported_raster.contains_key(id)
                        && self.imported_persistent.contains_key(id)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in recover {
                if let Some((v, i, tc)) = self.imported_persistent.get(&id).cloned() {
                    if i.is_empty() {
                        continue;
                    }
                    self.imported_raster.insert(id, (v, i));
                    if let Some(c) = self.meshes.get_mut(&id) {
                        c.triangle_count = tc;
                    }
                }
            }
        }

        // Collect imported raster IDs to process (avoid borrow conflict)
        let imported_ids: Vec<MeshCacheId> = self.imported_raster.keys().copied().collect();
        for id in imported_ids {
            if over_budget(upload_count) { break; }
            if let Some((verts, indices)) = self.imported_raster.remove(&id) {
                // Snapshot prev verts before borrowing meshes mutably (DirtyMeshRegion runs).
                let prev_verts = self
                    .imported_persistent
                    .get(&id)
                    .map(|(v, _, _)| v.clone());
                let full_replace = self.full_replace_pending.remove(&id);
                if let Some(cached) = self.meshes.get_mut(&id) {
                    let vert_bytes: &[u8] = bytemuck::cast_slice(&verts);
                    let idx_bytes: &[u8] = bytemuck::cast_slice(&indices);
                    cached.triangle_count = (indices.len() / 3) as u32;
                    match &mut cached.gpu_mesh {
                        Some(gm) => {
                            // Position-tier DirtyMeshRegion: same index count → topology stable.
                            // Prefer dirty-run patches vs persistent prev (upload only touched verts).
                            // A full replace never diffs — its baseline was already overwritten.
                            if full_replace {
                                gm.update_raw(device, queue, vert_bytes, idx_bytes, indices.len() as u32);
                            } else if gm.index_count == indices.len() as u32 {
                                if let Some(ref prev) = prev_verts {
                                    if prev.len() == verts.len() {
                                        gm.update_vertices_dirty_runs(
                                            device, queue, prev, &verts,
                                        );
                                    } else {
                                        gm.update_vertices_only(device, queue, vert_bytes);
                                    }
                                } else {
                                    gm.update_vertices_only(device, queue, vert_bytes);
                                }
                            } else {
                                gm.update_raw(
                                    device,
                                    queue,
                                    vert_bytes,
                                    idx_bytes,
                                    indices.len() as u32,
                                );
                            }
                        }
                        None => {
                            cached.gpu_mesh = Some(GpuMesh::upload_raw(
                                device,
                                queue,
                                vert_bytes,
                                idx_bytes,
                                indices.len() as u32,
                            ));
                        }
                    }
                    upload_count += 1;
                    // Baseline := exactly what the GPU now holds. Single writer, so the next
                    // dirty-run diff is against real GPU state (see update_chunk_verts_in_place).
                    let tc = (indices.len() / 3) as u32;
                    self.imported_persistent.insert(id, (verts, indices, tc));
                } else {
                    self.imported_raster.insert(id, (verts, indices));
                }
            }
        }
        // Upload DC-generated meshes ONLY (quad topology). Editable poly/NURBS meshes use
        // empty quads + imported_raster/persistent — never synthesize a 0-index solid here
        // (that was the "full mesh vanishes after Ctrl+X face delete" viewport bug).
        for cached in self.meshes.values_mut() {
            if over_budget(upload_count) { break; }
            if cached.gpu_mesh.is_none()
                && !cached.cpu_mesh.vertices.is_empty()
                && !cached.cpu_mesh.quads.is_empty()
            {
                let (verts, indices) = cached.cpu_mesh.to_raster_data();
                if indices.is_empty() {
                    continue;
                }
                let vert_bytes: &[u8] = bytemuck::cast_slice(&verts);
                let idx_bytes: &[u8] = bytemuck::cast_slice(&indices);
                cached.triangle_count = (indices.len() / 3) as u32;
                cached.gpu_mesh = Some(GpuMesh::upload_raw(
                    device,
                    queue,
                    vert_bytes,
                    idx_bytes,
                    indices.len() as u32,
                ));
                upload_count += 1;
            }
        }
    }

    /// True when the solid draw would skip this id (no buffer or zero indices).
    pub fn solid_undrawable(&self, id: MeshCacheId) -> bool {
        match self.meshes.get(&id) {
            None => true,
            Some(c) => c
                .gpu_mesh
                .as_ref()
                .map(|g| g.index_count == 0)
                .unwrap_or(true),
        }
    }

    /// Insert an imported mesh directly (no DC needed).
    ///
    /// Used for glTF / OBJ imported meshes that already have vertex data.
    /// `raster_data` is `(vertices, indices)` in the same format as `Mesh::to_raster_data()`.
    pub fn insert_imported(
        &mut self,
        cache_id: MeshCacheId,
        raster_data: (Vec<[f32; 8]>, Vec<u32>),
        triangle_count: u32,
    ) {
        let (verts, indices) = raster_data;
        // Build a minimal Mesh (quads empty — imported meshes use triangles directly)
        let positions: Vec<glam::Vec3> = verts.iter().map(|v| glam::Vec3::new(v[0], v[1], v[2])).collect();
        let normals: Vec<glam::Vec3> = verts.iter().map(|v| glam::Vec3::new(v[3], v[4], v[5])).collect();
        let cpu_mesh = Mesh {
            vertices: positions,
            normals,
            quads: Vec::new(), // not used for imported meshes
        };
        let (local_min, local_max) = mesh_local_aabb(&cpu_mesh.vertices);
        // Store the raster data directly for GPU upload
        self.meshes.insert(cache_id, CachedMesh {
            cpu_mesh,
            gpu_mesh: None,
            lod: MeshLod::High,
            source_hash: 0, // imported meshes don't use hash invalidation
            triangle_count,
            local_min,
            local_max,
            content_stamp: { self.stamp_counter += 1; self.stamp_counter },
            material_ranges: None,
        });
        // Stash the raw raster data for upload_pending to use
        self.imported_raster.insert(cache_id, (verts.clone(), indices.clone()));
        // Persistent copy for save/load (not consumed on GPU upload)
        self.imported_persistent.insert(cache_id, (verts, indices, triangle_count));
    }

    /// The retained raster for an imported mesh: `([px,py,pz,nx,ny,nz,u,v] per vertex, indices)`.
    ///
    /// This is the ONLY place an imported mesh's TOPOLOGY and UVs survive on the CPU:
    /// `insert_imported` builds `cpu_mesh` with `quads: Vec::new()` (imported meshes draw as
    /// triangles), and `core_math::mesh::Mesh` has no UV field at all. Anything that needs to
    /// reconstruct an imported mesh — Convert to Poly above all — must read this, not `cpu_mesh`.
    pub fn imported_raster_persistent(
        &self,
        id: MeshCacheId,
    ) -> Option<(&[[f32; 8]], &[u32])> {
        self.imported_persistent
            .get(&id)
            .map(|(v, i, _)| (v.as_slice(), i.as_slice()))
    }

    /// Insert a pre-computed DC mesh directly into the cache (synchronous, no background job).
    ///
    /// Used by chunked DC (Level 3) when dirty chunks have been re-extracted on the
    /// main thread. Clears GPU mesh so it gets re-uploaded on next `upload_pending()`.
    /// Also cancels any in-flight background jobs for this body since they're now stale.
    pub fn insert_mesh_direct(
        &mut self,
        cache_id: MeshCacheId,
        mesh: Mesh,
        source_hash: u64,
        lod: MeshLod,
    ) {
        let tri_count = mesh.quads.len() as u32 * 2;
        let (local_min, local_max) = mesh_local_aabb(&mesh.vertices);
        self.meshes.insert(cache_id, CachedMesh {
            cpu_mesh: mesh,
            gpu_mesh: None, // re-uploaded on next upload_pending()
            lod,
            source_hash,
            triangle_count: tri_count,
            local_min,
            local_max,
            content_stamp: { self.stamp_counter += 1; self.stamp_counter },
            material_ranges: None,
        });
        // Drop any prior editable/mesh-mod/PT imported raster — otherwise upload_pending
        // prefers imported_raster and never re-uploads this DC cpu_mesh (stale solid/wire).
        self.imported_raster.remove(&cache_id);
        self.imported_persistent.remove(&cache_id);
        self.imported_cells.remove(&cache_id);
        // Cancel any in-flight background DC jobs for this body — direct insert supersedes them.
        self.in_flight.retain(|(cid, _)| *cid != cache_id);
        // Bump generation so any still-running background job is discarded on arrival.
        let gen = self.generations.entry(cache_id).or_insert(0);
        *gen += 1;
    }

    /// Translate an existing cached mesh in place by `offset` (body-local units).
    ///
    /// Used when a body recenter (`recalculate_body_origin`) shifts the body-local
    /// frame: the body's instance transform reads the NEW origin immediately, so the
    /// DISPLAYED mesh must move by the same offset THIS frame or existing geometry
    /// visibly jumps by +centroid until an async bake re-lands it. This is a pure
    /// relocation of already-correct geometry — NO Dual Contouring extraction (safe in
    /// volume-only mode, respects the Frame Budget rule).
    ///
    /// Deliberately KEEPS `source_hash` (geometry is unchanged, only moved) so the next
    /// real bake's dedup still fires; drops `gpu_mesh` so the shifted vertices re-upload;
    /// and does NOT bump the generation or cancel in-flight jobs — a subsequent legitimate
    /// DC result with a newer generation must still win. This keeps `mesh_cache` and the
    /// bake-side caches structurally inseparable on a recenter.
    pub fn translate_mesh_in_place(&mut self, id: MeshCacheId, offset: Vec3) {
        self.stamp_counter += 1;
        let stamp = self.stamp_counter;
        if let Some(entry) = self.meshes.get_mut(&id) {
            for v in entry.cpu_mesh.vertices.iter_mut() { *v += offset; }
            entry.local_min += offset; // A1: keep the cached AABB consistent with the shifted verts
            entry.local_max += offset;
            entry.gpu_mesh = None; // force re-upload of the shifted vertices
            entry.content_stamp = stamp; // vertices moved — PT BLAS must rebuild
        }
        // Imported/editable meshes upload from cached raster data, not cpu_mesh — keep it
        // consistent so a re-upload reads the shifted positions (no-op for DC bodies).
        if let Some((verts, _idx)) = self.imported_raster.get_mut(&id) {
            for v in verts.iter_mut() { v[0] += offset.x; v[1] += offset.y; v[2] += offset.z; }
        }
    }

    /// Update (or insert) a poly/NURBS mesh in the cache with new raster data.
    ///
    /// Queues GPU re-upload via `imported_raster`. **Keeps** an existing `gpu_mesh` when present so
    /// the viewport keeps drawing the previous frame until `upload_pending` rewrites the buffers
    /// (bevel/extrude used to set `gpu_mesh = None` every drag frame → solid mesh lagged until
    /// orbit/move forced another upload).
    /// Used after editing a half-edge mesh or retessellating a NURBS surface.
    pub fn update_editable_mesh(
        &mut self,
        cache_id: MeshCacheId,
        raster_data: (Vec<[f32; 8]>, Vec<u32>),
        triangle_count: u32,
    ) {
        self.update_editable_mesh_ex(cache_id, raster_data, triangle_count, None);
    }

    /// Like [`update_editable_mesh`] with optional multi-material index ranges for poly faces.
    pub fn update_editable_mesh_ex(
        &mut self,
        cache_id: MeshCacheId,
        raster_data: (Vec<[f32; 8]>, Vec<u32>),
        triangle_count: u32,
        material_ranges: Option<Vec<(u32, u32, u32)>>,
    ) {
        let (verts, indices) = raster_data;
        // Parallel split for big meshes (1024³ PT snapshots land ~850K verts on the
        // frame thread) — rayon `collect` preserves order, so identical output.
        let (positions, normals): (Vec<glam::Vec3>, Vec<glam::Vec3>) = if verts.len() >= 65536 {
            use rayon::prelude::*;
            rayon::join(
                || verts.par_iter().map(|v| glam::Vec3::new(v[0], v[1], v[2])).collect(),
                || verts.par_iter().map(|v| glam::Vec3::new(v[3], v[4], v[5])).collect(),
            )
        } else {
            (
                verts.iter().map(|v| glam::Vec3::new(v[0], v[1], v[2])).collect(),
                verts.iter().map(|v| glam::Vec3::new(v[3], v[4], v[5])).collect(),
            )
        };
        let cpu_mesh = Mesh {
            vertices: positions,
            normals,
            quads: Vec::new(),
        };
        let (local_min, local_max) = mesh_local_aabb(&cpu_mesh.vertices);
        let stamp = { self.stamp_counter += 1; self.stamp_counter };
        // Preserve live GPU mesh across edits: upload_pending will update_raw (or grow) in place.
        // Dropping it here left PolygonalBody undrawn (gpu_mesh=None) until the next Hybrid upload.
        let prev_gpu = self.meshes.remove(&cache_id).and_then(|c| c.gpu_mesh);
        // Collapse to None when only one range (mono-material) so draw path stays simple.
        let material_ranges = material_ranges.and_then(|r| {
            if r.len() <= 1 {
                None
            } else {
                Some(r)
            }
        });
        self.meshes.insert(cache_id, CachedMesh {
            cpu_mesh,
            gpu_mesh: prev_gpu,
            lod: MeshLod::High,
            source_hash: 0,
            triangle_count,
            local_min,
            local_max,
            content_stamp: stamp,
            material_ranges,
        });
        // Keep persistent so wireframe / PT / tools still see the mesh after GPU upload
        // consumes `imported_raster` (mesh-mod folded DC display depends on this).
        self.imported_persistent
            .insert(cache_id, (verts.clone(), indices.clone(), triangle_count));
        // This write just poisoned the dirty-run baseline (persistent now holds the NEW verts).
        // Mark the id so `upload_pending` writes every vertex rather than diffing against itself.
        self.full_replace_pending.insert(cache_id);
        self.imported_raster.insert(cache_id, (verts, indices));
        // A full mesh replacement invalidates any cached per-chunk cell ids: the new vertex array
        // may have a different count/order, so a stale cells Vec would map preview writes to the
        // WRONG vertices. Drop it; the voxel path re-attaches the matching cells via set_chunk_cells.
        self.imported_cells.remove(&cache_id);
    }

    /// Attach the per-chunk surface-net CELL ID array for a voxel chunk mesh, parallel to the verts
    /// that were just written by `update_editable_mesh`. The caller (the voxel sculpt mesher) MUST
    /// pass exactly one cell id per vertex, in the SAME order the mesh was built — this is the
    /// provenance the zero-remesh recolor / reproject paths read. Atomic with the verts write
    /// (called in the same statement) so the two never desync.
    pub fn set_chunk_cells(&mut self, cache_id: MeshCacheId, cells: Vec<[u32; 3]>) {
        self.imported_cells.insert(cache_id, cells);
    }

    /// The per-chunk cell ids attached to a voxel chunk mesh, or None for any non-voxel mesh.
    pub fn chunk_cells(&self, id: MeshCacheId) -> Option<&Vec<[u32; 3]>> {
        self.imported_cells.get(&id)
    }

    /// Exactly the preconditions [`Self::update_chunk_verts_in_place`] checks before it commits.
    /// Callers that MOVE their vertex Vec into that function cannot recover it on a `false`
    /// return — asking this first lets them skip the defensive clone (5 MB/frame at 323k tris on
    /// the position-edit drag path) and still fall back to the full replace safely.
    pub fn can_update_verts_in_place(&self, id: MeshCacheId, vert_count: usize) -> bool {
        let live = self.imported_raster.get(&id).map(|(v, _)| v.len() == vert_count);
        let persistent = self
            .imported_persistent
            .get(&id)
            .map(|(v, _, _)| v.len() == vert_count);
        live.or(persistent).unwrap_or(false)
    }

    /// ZERO-REMESH PREVIEW upload: rewrite a chunk mesh's VERTICES in place from `new_verts` and
    /// re-queue it for an IN-PLACE GPU vertex write — WITHOUT dropping the existing `gpu_mesh` or
    /// touching the index buffer. This is the displacement / colour preview hot path: the vertex
    /// COUNT is unchanged (same surface topology, only positions+normals or the colour word moved),
    /// so `upload_pending`'s `update_raw` writes the same byte range into the SAME buffer — same
    /// size ⇒ same buffer handle ⇒ no bind-group invalidation, and the indices are never rebuilt.
    ///
    /// Returns false (a no-op) if there is no live chunk mesh for `id` or the new vertex count
    /// differs from the cached one — the caller MUST then fall back to a full `update_editable_mesh`
    /// (a genuine topology change is not a preview). Indices stay exactly as the last full mesh left
    /// them, which is correct precisely because the count (hence the topology) is unchanged.
    /// PR-3 (ZB_RESPONSIVE_SCULPT_PLAN §7.3): scatter-write KNOWN dirty display corners.
    /// Bytes ∝ patches — never a cage clone, never a full-array diff. The protocol exists
    /// because `upload_pending` is budgeted (5 ms) and diffs against `imported_persistent`:
    /// a re-queued full raster array would re-diff and CLOBBER the scatter (or stall).
    ///   1. `cpu_mesh` slots patched at the dirty indices only (pick/save coherence);
    ///   2. `imported_persistent` patched IN PLACE at those indices — it is the
    ///      last-uploaded baseline; after the immediate GPU write it must match the GPU
    ///      or the next dirty-runs diff re-uploads stale rows over the scatter;
    ///   3. GPU write NOW via `GpuMesh::patch_vertex_bytes` (coalesced runs; >64 runs
    ///      still writes runs — NEVER falls back to cloning; >25% of the VB dirty → one
    ///      full-range write sourced from the freshly patched persistent baseline);
    ///   4. `imported_raster` is NOT queued and `full_replace_pending` is NOT set. A
    ///      pre-existing same-count raster entry (another system queued a full upload
    ///      this frame) gets the same rows patched in place so that upload stays
    ///      coherent; a different-count entry means topology changed → `false`.
    /// `queue == None` (headless): cpu_mesh + persistent only.
    /// Returns `false` when there is no same-count persistent baseline — the caller
    /// falls back to the full path (genuine topology change).
    /// PR-8 test/harness seam: seed the last-uploaded baseline directly. Headless
    /// runs have no `upload_pending` (device-only), so `patch_chunk_verts` — which
    /// diffs against this baseline — could otherwise never be exercised without a GPU.
    pub fn seed_persistent_baseline(
        &mut self,
        id: MeshCacheId,
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
        tri_count: u32,
    ) {
        self.imported_persistent.insert(id, (verts, indices, tri_count));
    }

    /// PR-3 diagnostics (harness only).
    pub fn debug_persistent_len(&self, id: MeshCacheId) -> Option<usize> {
        self.imported_persistent.get(&id).map(|(v, _, _)| v.len())
    }
    pub fn debug_raster_len(&self, id: MeshCacheId) -> Option<usize> {
        self.imported_raster.get(&id).map(|(v, _)| v.len())
    }

    pub fn patch_chunk_verts(
        &mut self,
        id: MeshCacheId,
        patches: &[(u32, [f32; 8])],
        queue: Option<&wgpu::Queue>,
    ) -> bool {
        let Some(n) = self.imported_persistent.get(&id).map(|(v, _, _)| v.len()) else {
            return false;
        };
        match self.imported_raster.get_mut(&id) {
            Some((rv, _)) if rv.len() == n => {
                for &(ci, row) in patches {
                    if let Some(s) = rv.get_mut(ci as usize) {
                        *s = row;
                    }
                }
            }
            Some(_) => return false, // queued topology change — not a preview
            None => {}
        }
        if patches.is_empty() {
            return true;
        }
        self.stamp_counter += 1;
        let stamp = self.stamp_counter;
        if let Some(cached) = self.meshes.get_mut(&id) {
            for &(ci, row) in patches {
                let i = ci as usize;
                if let Some(cv) = cached.cpu_mesh.vertices.get_mut(i) {
                    *cv = glam::Vec3::new(row[0], row[1], row[2]);
                }
                if let Some(cn) = cached.cpu_mesh.normals.get_mut(i) {
                    *cn = glam::Vec3::new(row[3], row[4], row[5]);
                }
            }
            cached.content_stamp = stamp;
        }
        if let Some((pv, _, _)) = self.imported_persistent.get_mut(&id) {
            for &(ci, row) in patches {
                if let Some(s) = pv.get_mut(ci as usize) {
                    *s = row;
                }
            }
        }
        let (Some(queue), Some(gm)) = (
            queue,
            self.meshes.get_mut(&id).and_then(|c| c.gpu_mesh.as_mut()),
        ) else {
            return true; // headless: CPU + baseline patched; nothing to upload
        };
        const ROW: usize = std::mem::size_of::<[f32; 8]>();
        if patches.len() * 4 > n {
            // >25% of the KNOWN set dirty: one full-range write from the freshly
            // patched baseline (still never a display-verts clone).
            if let Some((pv, _, _)) = self.imported_persistent.get(&id) {
                let _ = gm.patch_vertex_bytes(queue, 0, bytemuck::cast_slice(pv));
            }
            return true;
        }
        // Coalesce sorted corner indices into contiguous runs; one write per run.
        let mut idx: Vec<usize> = (0..patches.len()).collect();
        idx.sort_unstable_by_key(|&k| patches[k].0);
        let mut run: Vec<[f32; 8]> = Vec::new();
        let mut run_start: u32 = 0;
        let mut prev: Option<u32> = None;
        let mut flush = |run: &mut Vec<[f32; 8]>, run_start: u32| {
            if !run.is_empty() {
                let _ = gm.patch_vertex_bytes(
                    queue,
                    run_start as u64 * ROW as u64,
                    bytemuck::cast_slice(run),
                );
                run.clear();
            }
        };
        for k in idx {
            let (ci, row) = patches[k];
            if ci as usize >= n {
                continue;
            }
            match prev {
                Some(p) if ci == p => {
                    // duplicate corner (multi-stamp frame): last write wins in place
                    if let Some(last) = run.last_mut() {
                        *last = row;
                    }
                    continue;
                }
                Some(p) if ci == p + 1 => run.push(row),
                _ => {
                    flush(&mut run, run_start);
                    run_start = ci;
                    run.push(row);
                }
            }
            prev = Some(ci);
        }
        flush(&mut run, run_start);
        true
    }

    pub fn update_chunk_verts_in_place(&mut self, id: MeshCacheId, new_verts: Vec<[f32; 8]>) -> bool {
        // Prefer a LIVE `imported_raster` entry (a preview write not yet consumed by a GPU upload). If
        // there is none, fall back to the PERSISTENT copy: a SETTLED chunk (already uploaded → its
        // imported_raster entry was removed by `upload_pending`) lives only in `imported_persistent`.
        // Without this fallback a settled chunk's displacement was silently dropped (the move-preview
        // no-op bug) — every frame after the first upload wrote nothing. Re-queueing an `imported_raster`
        // entry (cloning the persistent indices — topology is unchanged) makes `upload_pending` re-upload
        // the displaced verts IN PLACE (same count ⇒ same buffer ⇒ no realloc, no bind-group churn).
        // Count checks up front; the data is MOVED exactly once further down (this function used
        // to clone the whole array once here and once into the raster entry — ~10 MB/frame at
        // 323k tris, measured as `write 11 ms` of the 25 ms position-edit drag path).
        let idx_for_requeue: Option<Vec<u32>> = match self.imported_raster.get(&id) {
            Some((verts, _idx)) if verts.len() == new_verts.len() => None,
            // A live raster entry exists but with a DIFFERENT count → genuine topology change, not a
            // preview. The caller must fall back to a full `update_editable_mesh`.
            Some(_) => return false,
            None => match self.imported_persistent.get(&id) {
                Some((verts, idx, _tc)) if verts.len() == new_verts.len() => Some(idx.clone()),
                _ => return false,
            },
        };
        let verts = new_verts;
        // Write the live preview verts into the raster entry (whether it pre-existed or we just re-queued
        // it from persistent), AND mirror them into the persistent copy so a later settle/read sees the
        // displaced surface too.
        // Mirror cpu_mesh FIRST (from &verts, in parallel — the serial zip loops were the cost:
        // update_editable_mesh_ex splits the same work under rayon), THEN move `verts` into the
        // raster entry. Order matters only for ownership; both write the same data.
        self.stamp_counter += 1;
        let stamp = self.stamp_counter;
        if let Some(cached) = self.meshes.get_mut(&id) {
            if verts.len() >= 65536 {
                use rayon::prelude::*;
                cached
                    .cpu_mesh
                    .vertices
                    .par_iter_mut()
                    .zip(verts.par_iter())
                    .for_each(|(cv, nv)| *cv = glam::Vec3::new(nv[0], nv[1], nv[2]));
                cached
                    .cpu_mesh
                    .normals
                    .par_iter_mut()
                    .zip(verts.par_iter())
                    .for_each(|(cn, nv)| *cn = glam::Vec3::new(nv[3], nv[4], nv[5]));
            } else {
                for (cv, nv) in cached.cpu_mesh.vertices.iter_mut().zip(verts.iter()) {
                    *cv = glam::Vec3::new(nv[0], nv[1], nv[2]);
                }
                for (cn, nv) in cached.cpu_mesh.normals.iter_mut().zip(verts.iter()) {
                    *cn = glam::Vec3::new(nv[3], nv[4], nv[5]);
                }
            }
            cached.content_stamp = stamp; // preview moved verts — PT BLAS is stale
            // Deliberately KEEP cached.gpu_mesh (see comment below on the old mirror site).
        }
        match idx_for_requeue {
            // Settled chunk: re-queue an in-place upload from the persistent indices.
            Some(idx) => {
                self.imported_raster.insert(id, (verts, idx));
            }
            None => {
                if let Some((rv, _idx)) = self.imported_raster.get_mut(&id) { *rv = verts; }
            }
        }
        // Do NOT mirror into `imported_persistent` here. That map is the **last-uploaded**
        // baseline `upload_pending` diffs against for `update_vertices_dirty_runs`; writing
        // the new verts into it first made prev == new → zero dirty runs → the GPU write was
        // a silent no-op and the solid froze while every other stage looked healthy (the
        // "poly surface does not update live" bug; it only refreshed when a topology change
        // took the full-replace path). `upload_pending` writes the baseline after uploading.
        // CPU readers (pick / save) are served by the `cpu_mesh` mirror below.
        // Mirror the positions/normals into the CachedMesh CPU copy so picking / save / a later read see
        // the previewed surface too (parity with update_editable_mesh, which rebuilds cpu_mesh). Cheap
        // (same count) and keeps the representations coherent.
        // (Old serial mirror lived here. KEEP cached.gpu_mesh = Some(...): upload_pending sees the
        // re-queued imported_raster entry and calls update_raw (in-place, same size). Never set
        // None — that would force a buffer realloc + bind-group churn, the freeze path.)
        true
    }

    /// PT accel invalidation key for a mesh: its `content_stamp` (bumped on every
    /// content mutation, all mesh kinds). None if no mesh is cached for `id`.
    pub fn pt_stamp(&self, id: MeshCacheId) -> Option<u64> {
        self.meshes.get(&id).map(|m| m.content_stamp)
    }

    /// Owned triangle source data for the PT accel BLAS build (universal
    /// `[px,py,pz,nx,ny,nz,u,v]` verts + flat triangle indices). Editable /
    /// imported / voxel meshes come from their persistent raster copy; DC bodies
    /// triangulate `cpu_mesh`. ALLOCATES — callers must gate on `pt_stamp`
    /// mismatch before calling (the PT accel cache does).
    pub fn pt_source_data(&self, id: MeshCacheId) -> Option<(Vec<[f32; 8]>, Vec<u32>)> {
        if let Some((v, i)) = self.chunk_mesh_data(id) {
            return Some((v.clone(), i.clone()));
        }
        let cached = self.meshes.get(&id)?;
        if cached.cpu_mesh.vertices.is_empty() || cached.cpu_mesh.quads.is_empty() {
            return None;
        }
        Some(cached.cpu_mesh.to_raster_data())
    }

    /// Read-only access to a chunk mesh's current vertex array (for the preview paths that re-derive
    /// only colour or only positions from it, then write the result back via
    /// `update_chunk_verts_in_place`). None if the id has no imported raster data.
    pub fn chunk_verts(&self, id: MeshCacheId) -> Option<&Vec<[f32; 8]>> {
        self.imported_raster.get(&id).map(|(v, _)| v)
    }

    /// Read-only access to a chunk mesh's vertices + triangle indices. Prefers the live
    /// `imported_raster` (the latest in-place vertex preview, present until consumed by a GPU upload),
    /// falling back to the PERSISTENT copy (always present after the first full mesh; the index buffer
    /// is identical either way since an in-place preview never changes topology). None if the id has no
    /// imported mesh at all. Used by the MOVE tool's Phase-2 whole-object mesh gather (mesh→SDF).
    pub fn chunk_mesh_data(&self, id: MeshCacheId) -> Option<(&Vec<[f32; 8]>, &Vec<u32>)> {
        if let Some((v, i)) = self.imported_raster.get(&id) {
            return Some((v, i));
        }
        self.imported_persistent.get(&id).map(|(v, i, _)| (v, i))
    }

    /// Iterate over all persistent imported mesh data (for project save).
    pub fn imported_data(&self) -> impl Iterator<Item = (MeshCacheId, &Vec<[f32; 8]>, &Vec<u32>, u32)> {
        self.imported_persistent.iter().map(|(&id, (v, i, tc))| (id, v, i, *tc))
    }

    /// Number of cached meshes.
    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }

    /// Number of in-flight background jobs.
    pub fn pending_count(&self) -> usize {
        self.in_flight.len()
    }
}

// ============================================================
// Primitive hashing — MOVED (Gate 1, DC consolidation 2026-07-17)
// ============================================================
// `hash_primitives` (own-prims-only body hash) is DELETED. The ONE body-content
// identity is `app::bake_utils::body_content_id` (RECURSIVE `BodyContentId`);
// this cache only stores the resulting u64 as an opaque `source_hash` stamp.

#[cfg(test)]
mod tests {
    use super::*;

    /// PR-3 (ZB_RESPONSIVE_SCULPT_PLAN §7.3): headless protocol pin. The scatter must
    /// patch cpu_mesh + the persistent baseline AT THE INDICES ONLY, leave every other
    /// slot untouched, patch (never queue) a same-count raster entry, refuse a
    /// count-changed one, and refuse when no baseline exists.
    #[test]
    fn patch_chunk_verts_protocol() {
        let mut cache = MeshCache::new();
        let id = cache.alloc_id();
        // No baseline → refuse (caller takes the full path).
        assert!(!cache.patch_chunk_verts(id, &[(0, [1.0; 8])], None));

        let n = 16usize;
        let mesh = Mesh {
            vertices: vec![glam::Vec3::ZERO; n],
            normals: vec![glam::Vec3::Y; n],
            quads: Vec::new(),
        };
        cache.insert_mesh_direct(id, mesh, 1, MeshLod::High);
        cache
            .imported_persistent
            .insert(id, (vec![[0.0; 8]; n], vec![0, 1, 2], 1));

        let row = [9.0, 8.0, 7.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        assert!(cache.patch_chunk_verts(id, &[(3, row), (4, row)], None));
        let c = cache.meshes.get(&id).unwrap();
        assert_eq!(c.cpu_mesh.vertices[3], glam::Vec3::new(9.0, 8.0, 7.0));
        assert_eq!(c.cpu_mesh.vertices[4], glam::Vec3::new(9.0, 8.0, 7.0));
        assert_eq!(c.cpu_mesh.vertices[2], glam::Vec3::ZERO, "untouched slot stays");
        let (pv, _, _) = cache.imported_persistent.get(&id).unwrap();
        assert_eq!(pv[3], row, "baseline patched in place (clobber-trap rule)");
        assert_eq!(pv[5], [0.0; 8], "baseline untouched elsewhere");
        assert!(
            !cache.imported_raster.contains_key(&id),
            "scatter must NEVER queue a raster entry"
        );
        assert!(
            !cache.full_replace_pending.contains(&id),
            "scatter must never set full_replace_pending"
        );

        // Same-count queued raster: rows patched in place, entry NOT replaced.
        cache
            .imported_raster
            .insert(id, (vec![[0.0; 8]; n], vec![0, 1, 2]));
        assert!(cache.patch_chunk_verts(id, &[(7, row)], None));
        let (rv, _) = cache.imported_raster.get(&id).unwrap();
        assert_eq!(rv[7], row, "pending upload stays coherent with the scatter");
        // Count-changed raster: topology change → refuse.
        cache
            .imported_raster
            .insert(id, (vec![[0.0; 8]; n + 4], vec![0, 1, 2]));
        assert!(!cache.patch_chunk_verts(id, &[(1, row)], None));
    }

    #[test]
    fn cache_basic_lifecycle() {
        let mut cache = MeshCache::new();
        let id = cache.alloc_id();
        assert!(cache.needs_update(id, 42));
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.pending_count(), 0);
    }

    #[test]
    fn remove_rejects_late_background_result() {
        let mut cache = MeshCache::new();
        let id = cache.alloc_id();
        cache.generations.insert(id, 1);
        cache.in_flight.push((id, 0xCAFE));
        cache
            .result_tx
            .send(MeshResult {
                cache_id: id,
                mesh: Mesh { vertices: Vec::new(), normals: Vec::new(), quads: Vec::new() },
                lod: MeshLod::Preview,
                source_hash: 0xCAFE,
                generation: 1,
            })
            .expect("test result channel");

        cache.remove(id);

        assert!(cache.poll().is_empty(), "deleted mesh result must be discarded");
        assert!(cache.get(id).is_none(), "deleted mesh must not be resurrected");
        assert!(cache.get(id).is_none());
    }

    #[test]
    fn stale_result_does_not_clear_current_same_hash_job() {
        let mut cache = MeshCache::new();
        let id = cache.alloc_id();
        let hash = 0xBEEF;
        cache.generations.insert(id, 2);
        cache.in_flight.push((id, hash));
        for generation in [1, 2] {
            cache
                .result_tx
                .send(MeshResult {
                    cache_id: id,
                    mesh: Mesh { vertices: Vec::new(), normals: Vec::new(), quads: Vec::new() },
                    lod: MeshLod::Preview,
                    source_hash: hash,
                    generation,
                })
                .expect("test result channel");
        }

        assert_eq!(cache.poll(), vec![id]);
        assert!(cache.get(id).is_some(), "current generation must still land");
        assert_eq!(cache.pending_count(), 0);
    }

    #[test]
    fn lod_resolutions() {
        assert_eq!(MeshLod::Preview.resolution(), 64);
        assert_eq!(MeshLod::Medium.resolution(), 128);
        assert_eq!(MeshLod::High.resolution(), 256);
    }

    /// The MOVE-preview no-op fix: `update_chunk_verts_in_place` must NOT be a no-op for a SETTLED chunk
    /// that lives only in `imported_persistent` (its `imported_raster` entry was consumed by an upload).
    /// It must update persistent AND re-queue a raster entry so the displaced verts re-upload in place.
    #[test]
    fn in_place_update_works_for_settled_persistent_only_chunk() {
        let mut cache = MeshCache::new();
        let id = cache.alloc_id();
        let v0 = vec![[0.0f32; 8], [1.0; 8], [2.0; 8]];
        let idx = vec![0u32, 1, 2];
        // Seed both raster + persistent (what update_editable_mesh / insert_imported do).
        cache.insert_imported(id, (v0.clone(), idx.clone()), 1);
        // Simulate a GPU upload consuming the raster entry (upload_pending removes it).
        cache.imported_raster.remove(&id);
        assert!(cache.chunk_verts(id).is_none(), "raster entry should be gone (settled)");
        assert!(cache.chunk_mesh_data(id).is_some(), "persistent entry should remain");

        // Displace: same count → must succeed (NOT a no-op) and re-queue a raster entry for re-upload.
        let mut disp = v0.clone();
        for vt in disp.iter_mut() { vt[0] += 5.0; }
        assert!(cache.update_chunk_verts_in_place(id, disp.clone()),
            "settled persistent-only chunk must accept the in-place displacement");
        // Raster re-queued (with the cloned persistent indices) so upload_pending re-uploads in place.
        assert_eq!(cache.chunk_verts(id), Some(&disp), "raster re-queued with displaced verts");
        // Persistent mirrored so a later read/save sees the displaced surface.
        let (pv, pi) = cache.chunk_mesh_data(id).unwrap();
        assert_eq!(pv, &disp, "persistent verts updated");
        assert_eq!(pi, &idx, "indices preserved (topology unchanged)");

        // A genuine topology change (different count) must be REJECTED (caller falls back to full mesh).
        let wrong_count = vec![[9.0f32; 8]; 2];
        assert!(!cache.update_chunk_verts_in_place(id, wrong_count),
            "a different vertex count is a topology change, not a preview → must be rejected");
    }
}
