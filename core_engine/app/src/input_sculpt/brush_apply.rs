//! Brush stamp apply: GPU/CPU density writes, chunk mesh pipeline, atlas collect/materialize.
use super::*;

impl AzraApp {
    /// Queue a full rebuild of EVERY chunk (create / clear / Ctrl+R resolution change). Allocates
    /// chunk ids when the chunk count changes, then marks all chunks dirty and returns — the actual
    /// meshing is drained at a capped rate by `voxel_flush_dirty_chunks` over the next frames. This
    /// is the fix for the **resolution-change freeze**: meshing all ~1728 chunks synchronously here
    /// blocked the UI for ~2.4s; now the surface fills in progressively while the app stays
    /// responsive (Frame Budget Rule — never unbounded per-frame work).
    pub(crate) fn voxel_build_all_chunks(&mut self) {
        let chunk_dim = match self.sculpt.voxel.as_ref() {
            Some(v) => voxel_chunk_dim(v.grid.res),
            None => return,
        };
        let n = (chunk_dim * chunk_dim * chunk_dim) as usize;
        if self.sculpt.voxel.as_ref().map_or(true, |v| v.chunks.len() != n) {
            let mut ids = Vec::with_capacity(n);
            for _ in 0..n { ids.push(self.cache.mesh_cache.alloc_id()); }
            if let Some(v) = self.sculpt.voxel.as_mut() { v.chunk_dim = chunk_dim; v.chunks = ids; v.chunk_tris = vec![0; n]; }
        }
        // One cheap pass flags which chunks actually contain surface, so the drain skips the many
        // empty interior/exterior chunks (the bulk of a sphere) instead of scanning all of them.
        let mask = self.sculpt.voxel.as_ref().map(|v| v.grid.surface_chunk_mask(VOXEL_CHUNK));
        if let Some(v) = self.sculpt.voxel.as_mut() {
            // Clear old geometry/tris and queue the surface chunks for the time-budgeted drain.
            v.chunk_tris.iter_mut().for_each(|t| *t = 0);
            v.tris = 0;
            v.dirty_chunks.clear();
            match mask {
                Some(m) => {
                    for (ci, &has) in m.iter().enumerate() {
                        if has { v.dirty_chunks.insert(ci as u32); }
                    }
                    v.surface_chunk_count = v.dirty_chunks.len() as u32;
                }
                None => {
                    v.dirty_chunks.extend(0..n as u32);
                    v.surface_chunk_count = n as u32;
                }
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// DEBUG (GPU sculpt Phase 1b): mesh the whole voxel grid on the GPU (`voxel_mesh.wgsl`) and
    /// compare to the CPU `mesh_region` — vertex/index COUNTS + the rounded vertex-position SET
    /// (emission order differs, so we compare sets). Toasts OK/MISMATCH. Touches no render state;
    /// this validates the GPU Surface-Nets shader before it's wired into the live path (Phase 1c).
    pub(crate) fn voxel_gpu_parity_check(&mut self) {
        let res = match self.sculpt.voxel.as_ref() {
            Some(v) => v.grid.res,
            None => { self.toast("Parity: no voxel object"); return; }
        };
        if self.gpu.is_none() { self.toast("Parity: no GPU context"); return; }
        let chunk_dim = voxel_chunk_dim(res);
        let vs = self.sculpt.voxel.as_ref().unwrap().grid.voxel_size();
        // Mesh PER-CHUNK on both sides (the live path), NOT the whole grid as one dispatch: a 512³
        // single dispatch is 510³≈133M cells → multi-GB output buffers that hit the storage-binding
        // limit (the old MISMATCH). Per-chunk uses bounded buffers and tests the exact production path;
        // identical chunk boundaries on both sides mean any difference is a real shader discrepancy.
        let ranges: Vec<([u32; 3], [u32; 3])> = {
            let v = self.sculpt.voxel.as_ref().unwrap();
            v.grid.surface_chunk_mask(VOXEL_CHUNK).iter().enumerate()
                .filter_map(|(ci, &h)| if h { Some(voxel_chunk_anchor(ci as u32, chunk_dim, res)) } else { None })
                .collect()
        };
        if ranges.is_empty() { self.toast("Parity: empty surface (nothing to mesh)"); return; }
        // Lazily compile the persistent GPU mesher (owned by GpuCtx — compiled once per session).
        if self.gpu.as_ref().map_or(false, |g| g.voxel_mesh.is_none()) {
            let device = self.gpu.as_ref().unwrap().device.clone();
            let m = crate::voxel_gpu_mesh::VoxelMeshGpu::new(&device);
            self.gpu.as_mut().unwrap().voxel_mesh = Some(m);
        }
        let t = std::time::Instant::now();
        // CPU reference: the live CPU per-chunk mesher, same chunk anchors as the GPU side.
        let mut cpu_v: Vec<[f32; 8]> = Vec::new();
        let mut cpu_i_count = 0usize;
        let mut cpu_counts: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        {
            let grid = &self.sculpt.voxel.as_ref().unwrap().grid;
            for &(a0, a1) in &ranges {
                let (v, i) = grid.mesh_region(a0, a1);
                cpu_counts.push((v.len(), i.len()));
                cpu_i_count += i.len();
                cpu_v.extend(v);
            }
        }
        // GPU: mesh the SAME chunks, in bounded batches (each chunk gets its own small output buffers —
        // no whole-grid allocation). Blocking readback (debug only). Results are in input order.
        let device = self.gpu.as_ref().unwrap().device.clone();
        let queue = self.gpu.as_ref().unwrap().queue.clone();
        const PARITY_BATCH: usize = 128;
        let mut gpu_v: Vec<[f32; 8]> = Vec::new();
        let mut gpu_i_count = 0usize;
        let mut mismatch_chunks = 0usize;
        let mut idx = 0usize;
        for batch in ranges.chunks(PARITY_BATCH) {
            let results = {
                let grid = &self.sculpt.voxel.as_ref().unwrap().grid;
                self.gpu.as_ref().unwrap().voxel_mesh.as_ref().unwrap()
                    .mesh_chunks(&device, &queue, grid, batch)
            };
            for (v, i) in results {
                if idx < cpu_counts.len() && (v.len() != cpu_counts[idx].0 || i.len() != cpu_counts[idx].1) {
                    mismatch_chunks += 1;
                }
                idx += 1;
                gpu_i_count += i.len();
                gpu_v.extend(v);
            }
        }
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        let count_match = cpu_v.len() == gpu_v.len() && cpu_i_count == gpu_i_count && mismatch_chunks == 0;
        // Positions: max nearest-neighbour distance (robust to FP rounding + emission order).
        // ~1e-5 = the meshes agree (float noise); OK if within a tiny fraction of a voxel.
        let max_d = crate::voxel_gpu_mesh::max_nn_dist(&cpu_v, &gpu_v, vs);
        let ok = count_match && max_d < vs * 0.05;
        let msg = format!(
            "GPU parity {}: {} chunks, CPU {}v/{}i vs GPU {}v/{}i, {} chunk-mismatch, maxdpos {:.1e} ({:.0}ms)",
            if ok { "OK" } else { "MISMATCH" },
            ranges.len(), cpu_v.len(), cpu_i_count, gpu_v.len(), gpu_i_count, mismatch_chunks, max_d, ms,
        );
        eprintln!("[VXGPU] {msg}");
        self.toast(&msg);
        // Persist it so the Sculpt panel can show it until the next run (the toast is too brief).
        if let Some(v) = self.sculpt.voxel.as_mut() { v.parity_msg = msg; }
    }

    /// DEBUG (GPU brush Phase 2): apply each of the 11 brush ops via the GPU shader AND the CPU
    /// `apply_brush`, on a clone of the current grid, and report the worst per-voxel |Δdensity|
    /// across all ops (~1e-5 = the GPU brush matches the CPU bit-for-bit; large = a shader op is
    /// wrong). Validates `voxel_brush.wgsl` before it drives the live sculpt path.
    pub(crate) fn voxel_gpu_brush_parity(&mut self) {
        if self.gpu.is_none() || self.sculpt.voxel.is_none() { self.toast("Brush parity: no voxel/GPU"); return; }
        if self.gpu.as_ref().map_or(false, |g| g.voxel_brush.is_none()) {
            let device = self.gpu.as_ref().unwrap().device.clone();
            let b = crate::voxel_gpu_mesh::VoxelBrushGpu::new(&device);
            self.gpu.as_mut().unwrap().voxel_brush = Some(b);
        }
        use core_math::voxel_sculpt::VoxelBrush;
        let ops: [(VoxelBrush, u32, &str); 11] = [
            (VoxelBrush::Draw, 0, "Draw"), (VoxelBrush::Erase, 1, "Erase"), (VoxelBrush::Smooth, 2, "Smooth"),
            (VoxelBrush::Flatten, 3, "Flatten"), (VoxelBrush::Clay, 4, "Clay"), (VoxelBrush::Scrape, 5, "Scrape"),
            (VoxelBrush::Crease, 6, "Crease"), (VoxelBrush::Fill, 7, "Fill"), (VoxelBrush::ClayBuildup, 8, "Build"),
            (VoxelBrush::Inflate, 9, "Inflate"), (VoxelBrush::Pinch, 10, "Pinch"),
        ];
        let center = glam::Vec3::new(0.0, 0.0, 0.45);
        let normal = glam::Vec3::Z;
        let (radius, strength) = (0.3f32, 0.6f32);
        // All 11 ops run on the GPU over the brush AABB BOX. `apply_stamp` returns the box; compare it
        // to the CPU result over the SAME box (`subvolume`). Box = the AABB, so `snap_g`'s box clamp now
        // matches the CPU's AABB-snapshot clamp → Smooth/Inflate should agree bit-closely (not just to
        // the falloff ring). Pinch's gather is on GPU; its redistancing relax is a CPU post-pass that
        // `apply_stamp` omits, so Pinch shows the pre-relax delta.
        let mut worst_gpu = (0.0f32, "");
        let mut pinch_d = 0.0f32;
        let aabb = self.sculpt.voxel.as_ref().unwrap().grid.brush_aabb(center, radius);
        for (vb, code, name) in ops {
            let Some(aabb) = aabb else { continue };
            let mut cpu = self.sculpt.voxel.as_ref().unwrap().grid.clone();
            // Hardness 0.0 matches `apply_stamp`'s packed `box_min.w = 0` (bitcast 0u32 -> 0.0_f32).
            cpu.apply_brush(vb, center, normal, radius, strength, false, 0.0, glam::Vec3::ZERO, None);
            let cpu_box = cpu.density.subvolume(aabb);
            let gpu_box = {
                let gpu = self.gpu.as_ref().unwrap();
                let grid = &self.sculpt.voxel.as_ref().unwrap().grid;
                gpu.voxel_brush.as_ref().unwrap().apply_stamp(&gpu.device, &gpu.queue, grid, code, center, normal, radius, strength, 1.0)
            };
            let md = cpu_box.iter().zip(gpu_box.iter()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            eprintln!("[VXGPU] brush op {name}: |Δ| {md:.2e}");
            if code == 10 { pinch_d = md; }
            else if md > worst_gpu.0 { worst_gpu = (md, name); }
        }
        let msg = format!("Brush parity: GPU ops |Δ|{:.1e} ({}); Pinch {:.1e} (pre-relax)", worst_gpu.0, worst_gpu.1, pinch_d);
        eprintln!("[VXGPU] {msg}");
        self.toast(&msg);
        if let Some(v) = self.sculpt.voxel.as_mut() { v.parity_msg = msg; }
    }

    /// Re-mesh ONLY the chunks touched by a VOXEL cell region (the brush) + re-upload them.
    /// This is the per-frame sculpt hot path — cost scales with the brush, not the whole surface.
    /// Queue every chunk overlapping a VOXEL cell `region` for re-mesh. Used per brush stamp and on
    /// undo — marking only the THIN set the edit actually touches. (Marking the bounding box of a
    /// whole frame's stamps is what blew up to the entire grid on a fast flick → the 1.8s freeze.)
    /// The queue is drained, capped, by `voxel_flush_dirty_chunks`.
    pub(crate) fn voxel_mark_chunks_in_region(&mut self, region: [u32; 6]) {
        let (res, chunk_dim) = match self.sculpt.voxel.as_ref() {
            Some(v) if !v.chunks.is_empty() => (v.grid.res, v.chunk_dim),
            _ => return,
        };
        let cr = voxel_chunks_for_region(region, chunk_dim, res);
        if let Some(v) = self.sculpt.voxel.as_mut() {
            for cz in cr[2]..=cr[5] { for cy in cr[1]..=cr[4] { for cx in cr[0]..=cr[3] {
                v.dirty_chunks.insert(cx + cy * chunk_dim + cz * chunk_dim * chunk_dim);
            }}}
        }
    }

    /// Linear chunk indices a voxel region touches (same halo + linear encoding as
    /// `voxel_mark_chunks_in_region`, but returned instead of marked). Phase A.2: an atlas stroke's
    /// `AtlasUndoEntry` carries these so undo/redo can re-mark exactly the same chunks for re-mesh.
    pub(crate) fn voxel_chunks_for_region_linear(&self, region: [u32; 6]) -> Vec<u32> {
        let (res, chunk_dim) = match self.sculpt.voxel.as_ref() {
            Some(v) if !v.chunks.is_empty() => (v.grid.res, v.chunk_dim),
            _ => return Vec::new(),
        };
        let cr = voxel_chunks_for_region(region, chunk_dim, res);
        let mut out = Vec::new();
        for cz in cr[2]..=cr[5] { for cy in cr[1]..=cr[4] { for cx in cr[0]..=cr[3] {
            out.push(cx + cy * chunk_dim + cz * chunk_dim * chunk_dim);
        }}}
        out
    }

    /// Mark dirty ONLY the chunks the brush SPHERE overlaps — not the full stamp AABB box. A large
    /// brush's AABB includes corner chunks the inscribed sphere never reaches (~48% of the box), and
    /// marking those floods the re-mesh budget with chunks that don't change → the trailing "slivers"
    /// on a fast large-brush stroke. The cull is CONSERVATIVE: the brush footprint is bounded by
    /// `radius + one voxel` (see `VoxelGrid::brush_aabb`), so any chunk whose nearest voxel is farther
    /// than that from the sphere centre provably has no edited voxel and is safe to skip. (Solid-
    /// interior chunks INSIDE the sphere are still marked — they may flip surface↔solid; only the
    /// outside-sphere corners are dropped.) Used by the GPU-resident/atlas paths where chunk count,
    /// not per-voxel cost, is the bottleneck.
    pub(crate) fn voxel_mark_chunks_for_brush(&mut self, center: glam::Vec3, radius: f32) {
        let (res, chunk_dim, cvox, rvox) = match self.sculpt.voxel.as_ref() {
            Some(v) if !v.chunks.is_empty() => {
                let vs = v.grid.voxel_size().max(1e-6);
                (v.grid.res, v.chunk_dim, v.grid.world_to_grid(center), radius / vs + 1.0)
            }
            _ => return,
        };
        // Search bound = the brush's clamped voxel AABB → its chunk-AABB (same bound the box-mark uses).
        let aabb = match self.sculpt.voxel.as_ref().and_then(|v| v.grid.brush_aabb(center, radius)) {
            Some(a) => a,
            None => return,
        };
        let cr = voxel_chunks_for_region(aabb, chunk_dim, res);
        let r2 = rvox * rvox;
        let last = res.saturating_sub(1) as f32;
        if let Some(v) = self.sculpt.voxel.as_mut() {
            for cz in cr[2]..=cr[5] { for cy in cr[1]..=cr[4] { for cx in cr[0]..=cr[3] {
                // Chunk (cx,cy,cz) spans voxels [c·VOXEL_CHUNK, (c+1)·VOXEL_CHUNK] (inclusive, clamped):
                // distance from the sphere centre to that box, marked iff within the brush radius.
                // Expand the LOW corner by the 1-voxel Surface-Nets apron each chunk reads ([a0-1 .. a1+1]):
                // a chunk whose only overlap with the brush is via that -1 low apron must still be marked, or
                // its seam vertices don't match the edited chunk's → a crack along the chunk boundary (~21%
                // of stamps left a neighbour unmarked before this). The HI corner already covers the +1 read
                // via (c+1)·VOXEL_CHUNK. Sphere corner-cull is otherwise preserved (~+1 chunk/stamp).
                let lo = [(cx * VOXEL_CHUNK) as f32 - 1.0, (cy * VOXEL_CHUNK) as f32 - 1.0, (cz * VOXEL_CHUNK) as f32 - 1.0];
                let hi = [(((cx + 1) * VOXEL_CHUNK) as f32).min(last), (((cy + 1) * VOXEL_CHUNK) as f32).min(last), (((cz + 1) * VOXEL_CHUNK) as f32).min(last)];
                let dx = cvox.x - cvox.x.clamp(lo[0], hi[0]);
                let dy = cvox.y - cvox.y.clamp(lo[1], hi[1]);
                let dz = cvox.z - cvox.z.clamp(lo[2], hi[2]);
                if dx * dx + dy * dy + dz * dz <= r2 {
                    v.dirty_chunks.insert(cx + cy * chunk_dim + cz * chunk_dim * chunk_dim);
                }
            }}}
        }
    }

    /// Union a voxel-AABB into the current stroke's edited region (compacted on release).
    pub(crate) fn voxel_expand_stroke_aabb(&mut self, r: [u32; 6]) {
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.stroke_aabb = Some(match v.stroke_aabb {
                Some(a) => [a[0].min(r[0]), a[1].min(r[1]), a[2].min(r[2]), a[3].max(r[3]), a[4].max(r[4]), a[5].max(r[5])],
                None => r,
            });
        }
    }

    /// Gather the live sculpt display mesh from `mesh_cache` chunk data (already meshed — O(verts)
    /// copy). Used by Move bake so we do NOT re-run Surface-Nets over the whole grid every release
    /// (~550ms in owner logs). 3D-Coat keeps a live surface mesh; we reuse the same idea.
    /// Returns empty if no chunk meshes are resident yet (caller may fall back to density remesh).
    pub(super) fn gather_voxel_display_mesh_from_cache(&self) -> (Vec<glam::Vec3>, Vec<u32>) {
        let Some(v) = self.sculpt.voxel.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let mut verts: Vec<glam::Vec3> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for (i, &cid) in v.chunks.iter().enumerate() {
            if v.chunk_tris.get(i).copied().unwrap_or(0) == 0 { continue; }
            let Some((cv, ci)) = self.cache.mesh_cache.chunk_mesh_data(cid) else { continue; };
            if cv.is_empty() || ci.is_empty() { continue; }
            let base = verts.len() as u32;
            verts.reserve(cv.len());
            for vt in cv.iter() {
                verts.push(glam::Vec3::new(vt[0], vt[1], vt[2]));
            }
            indices.reserve(ci.len());
            for &idx in ci.iter() {
                indices.push(base + idx);
            }
        }
        (verts, indices)
    }

    /// Re-mesh + re-upload up to `MAX_CHUNK_REMESH_PER_FRAME` queued dirty chunks. Frame Budget Rule:
    /// bounds per-frame work so a stroke that dirties many chunks (fast flick, big brush) spreads
    /// over a few frames instead of freezing. Call once per frame — a cheap no-op when idle.
    pub(crate) fn voxel_flush_dirty_chunks(&mut self) {
        // Land a finished ASYNC Move bake first (no-op unless one is in flight AND done) — this is
        // the per-frame poll site (update() calls this every frame). Re-entrancy safe: the land's
        // own drain loop re-enters here with the bake already taken → the poll is a no-op.
        self.voxel_poll_move_bake();
        // Mesh queued chunks until ~this much is spent this frame, then continue next frame. Kept
        // well under a full frame so a rebuild doesn't peg a core at 100% (sculpt edits touch only
        // a handful of chunks → still drain fully in one frame, no latency).
        // P2b: tighter budget mid-stroke keeps the brush UI-thread responsive (backlog drains on release).
        // PERF (log-driven): after large Move @256/512³ the dirty backlog caused continuous ~42ms
        // SPIKE frames. Scale budget down with res + backlog so the UI stays interactive while catch-up
        // spreads over more frames (nearest-first still prioritises under-cursor).
        let (dirty_n, grid_res, live_rm) = self.sculpt.voxel.as_ref()
            .map(|v| (v.dirty_chunks.len(), v.grid.res, v.use_sculpt_raymarch_overlay()))
            .unwrap_or((0, 64, false));
        // Fluid LIVE sculpt: mid-stroke MUST remesh under the brush (Coat/Blender clay feel).
        // Only when sparse raymarch overlay is the live surface do we skip mesh mid-stroke.
        // Dense mesh path: real remesh budget so the surface follows the tip every frame.
        let prev_wall = self.prev_frame_wall_ms;
        let mut frame_budget_ms: f32 = if live_rm {
            if self.sculpt.stroke_active {
                0.0 // raymarch reads atlas/density live — mesh catch-up on release/idle
            } else if grid_res >= 896 {
                // 1024³: idle drain after stroke is the catch-up — free frames go hard.
                if dirty_n > 8000 { 8.0 } else if dirty_n > 3000 { 12.0 } else { 16.0 }
            } else if dirty_n > 2000 {
                6.0
            } else {
                10.0
            }
        } else if self.sculpt.stroke_active {
            // LIVE mesh under the tip (Coat: surface follows density continuously).
            if grid_res >= 896 { 10.0 } else if grid_res >= 512 { 14.0 } else if grid_res >= 256 { 16.0 } else { 18.0 }
        } else if dirty_n > 400 || grid_res >= 512 {
            12.0
        } else if dirty_n > 80 || grid_res >= 256 {
            14.0
        } else {
            18.0
        };
        // Adaptive: free frame → spend more on remesh (Coat drains after stroke quickly).
        // Heavy frame → clamp so the UI never stalls — keep a floor mid-stroke for live look.
        if prev_wall > 40.0 {
            frame_budget_ms = if self.sculpt.stroke_active && !live_rm {
                frame_budget_ms.min(8.0).max(5.0)
            } else {
                frame_budget_ms.min(3.0)
            };
        } else if prev_wall > 22.0 {
            frame_budget_ms = if self.sculpt.stroke_active && !live_rm {
                frame_budget_ms.min(10.0).max(6.0)
            } else {
                frame_budget_ms.min(5.0)
            };
        } else if prev_wall < 12.0 && dirty_n > 0 {
            let boost = if self.sculpt.stroke_active { 1.35 } else { 1.65 };
            let cap = if grid_res >= 896 {
                if self.sculpt.stroke_active { 14.0 } else { 24.0 }
            } else if self.sculpt.stroke_active { 18.0 } else { 20.0 };
            frame_budget_ms = (frame_budget_ms * boost).min(cap);
        }
        const HARD_CAP: usize = 512;
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.pending_rebuild.is_some()) {
            self.voxel_flush_pending_rebuild();
            return;
        }
        // Sparse raymarch live look: skip mesh mid-stroke (prevents tearing + SPIKE storm).
        // Density edits still apply via atlas/GPU; mesh catch-up runs idle / on leave.
        // If raymarch is NOT the live surface (mask/paint/mesh preferred), remesh continues.
        if self.sculpt.stroke_active
            && self.sculpt.voxel.as_ref().map_or(false, |v| v.use_sculpt_raymarch_overlay())
        {
            return;
        }


        // GPU live meshing (toggle): async — its own method consumes the in-flight batch then
        // dispatches the next. Runs even when dirty_chunks is empty (to collect a batch still in
        // flight from a previous frame). CPU path below is the safe fallback.
        //
        // DECISION B (STEP 9): a deposit/carve-while-painting stroke is CPU-FORCED only because COLOUR
        // is involved — but it changes density+topology, so a CPU per-frame Surface-Nets re-mesh is the
        // freeze. Route its DENSITY+TOPOLOGY through the GPU mesher anyway (zero-readback resident OR
        // the non-resident readback path) and apply the per-voxel paint colour as a SEPARATE in-place
        // vertex-colour write AFTER the dispatch lands (recolor_vertices_by_position, in
        // voxel_flush_gpu_async). `gpu_mesh_paint_topology()` is the gate; it EXCLUDES Mask (colour-only
        // → STEP 5 CPU recolor) and Move/Smudge (displacement → STEP 7 CPU reproject), which keep their
        // own zero-remesh CPU paths. So the only strokes that still fall through to the CPU mesh loop
        // below are those zero-remesh CPU classes (Mask / Move / Smudge) — never a CPU topology re-mesh.
        // SURFACE MODE: the CPU chunk mesh (mesh_cache) is the stroke-editable AUTHORITY, so any
        // chunk refresh must land in mesh_cache via the CPU mesher below, never the zero-readback
        // resident dispatch (whose results the surface stroke could neither see nor edit). Display
        // is the resident POOL (Gate 1): the CPU mesher hook below re-uploads the remeshed pair
        // into the chunk's slot, and every stroke write mirrors via voxel_surface_pool_upload.
        let route_to_gpu_mesher = self.sculpt.voxel.as_ref().map_or(false, |v|
            v.gpu_mesh && !v.chunks.is_empty() && !v.surface_mode
                && (!v.cpu_paint_path() || v.gpu_mesh_paint_topology()));
        if route_to_gpu_mesher && self.gpu.is_some() {
            self.voxel_flush_gpu_async();
            return;
        }

        let (res, chunk_dim) = match self.sculpt.voxel.as_ref() {
            Some(v) if !v.chunks.is_empty() && !v.dirty_chunks.is_empty() => (v.grid.res, v.chunk_dim),
            _ => return,
        };
        // ZERO-REMESH DISPLACEMENT PREVIEW gate (STEP 7). Move/Smudge advect material coherently — the
        // surface SHIFTS but (away from a genuine topology flip) the set of crossing cells is stable.
        // So re-derive only the existing vertices' positions+normals from the live density in place
        // (reproject_chunk_positions) and upload vertex bytes only (indices untouched). On a real
        // topology drift reproject returns false and THAT chunk falls back to a full mesh_region — the
        // mandatory honest fallback (a reprojected-only chunk would show stale topology). Dense-only;
        // sparse Move/Smudge never reach here (the early sparse-stroke return raymarches).
        use core_math::voxel_sculpt::VoxelBrush;
        let displacement_stroke = self.sculpt.stroke_active
            && self.sculpt.voxel.as_ref().map_or(false, |v| !v.grid.density.is_sparse()
                && matches!(v.brush, VoxelBrush::Move | VoxelBrush::Smudge));
        let t_start = std::time::Instant::now(); // also the MESH-phase profiler timer
        let mut meshed = 0usize;
        while meshed < HARD_CAP {
            // Pop one queued chunk (a small sculpt edit drains fully under budget → zero latency;
            // a full rebuild drains as many as fit in FRAME_BUDGET_MS, then continues next frame).
            let ci_u = {
                let v = self.sculpt.voxel.as_mut().unwrap();
                match v.dirty_chunks.iter().next().copied() {
                    Some(ci) => { v.dirty_chunks.remove(&ci); ci }
                    None => break,
                }
            };
            let ci = ci_u as usize;
            let cid = match self.sculpt.voxel.as_ref().and_then(|v| v.chunks.get(ci).copied()) {
                Some(cid) => cid,
                None => { meshed += 1; continue; }
            };
            // SURFACE-MODE STALE-DENSITY GUARD (residual-spike class): surface strokes displace
            // the chunk MESHES only — the density grid stays pre-stroke until revoxelize. A chunk
            // remesh from that stale density would snap JUST this chunk back to pre-stroke
            // geometry while its neighbours keep their displaced halo copies → needle spikes
            // along the whole chunk border. Any dirty mark that reaches a surface-edited chunk
            // mid-session (leftover queue entries, density-undo interleave, partial revoxelize
            // races) is therefore FORBIDDEN to remesh: fail closed — drop the mark, log. The
            // `surface_moved_aabbs` registry is exactly the set of surface-edited chunks (every
            // write lands through voxel_surface_write_chunk_verts; cleared on revoxelize/entry).
            if self.sculpt.voxel.as_ref().map_or(false, |v|
                v.surface_mode && v.surface_moved_aabbs.contains_key(&ci_u))
            {
                eprintln!("[SURFACE] chunk {ci_u} remesh-from-density skipped (un-voxelized surface edits)");
                meshed += 1;
                continue;
            }
            let (a0, a1) = voxel_chunk_anchor(ci_u, chunk_dim, res);
            // ZERO-REMESH PREVIEW fast path (COLOUR recolor OR DISPLACEMENT reproject). Both require the
            // chunk to already carry its cell-id provenance AND verts whose count still matches (a fresh
            // chunk / a topology change has none / a stale array); both leave the index buffer untouched
            // and do an in-place vertex upload (no buffer realloc, no bind-group churn). Reproject also
            // self-checks topology drift and returns false → fall back to the authoritative full mesh.
            // A chunk that fails either path falls through to mesh_region_with_cells below, which
            // re-attaches matching provenance for the next preview frame.
            let previewed = if displacement_stroke {
                let cells = self.cache.mesh_cache.chunk_cells(cid).cloned();
                match (cells, self.cache.mesh_cache.chunk_verts(cid).cloned()) {
                    (Some(cells), Some(mut verts)) if verts.len() == cells.len() && !verts.is_empty() => {
                        // Reproject positions+normals; false on drift (don't upload — full-mesh below).
                        let ok = self.sculpt.voxel.as_ref().map_or(false, |v| v.grid.reproject_chunk_positions(&mut verts, &cells, a0, a1));
                        ok && self.cache.mesh_cache.update_chunk_verts_in_place(cid, verts)
                    }
                    _ => false,
                }
            } else { false };
            if !previewed {
                // Authoritative full mesh of this chunk + attach the matching cell-id provenance so the
                // NEXT preview frame can recolor/reproject in place.
                let data = self.sculpt.voxel.as_ref().map(|v| v.grid.mesh_region_with_cells(a0, a1));
                if let Some((verts, idx, cells)) = data {
                    let tris = (idx.len() / 3) as u32;
                    self.cache.mesh_cache.update_editable_mesh(cid, (verts, idx), tris);
                    self.cache.mesh_cache.set_chunk_cells(cid, cells);
                    if let Some(v) = self.sculpt.voxel.as_mut() { if ci < v.chunk_tris.len() { v.chunk_tris[ci] = tris; } }
                    // GATE 1 (surface mode): the pool multi-draw is the display path, so a full CPU
                    // remesh of a chunk must land in its pool slot too (topology re-upload) — the
                    // slot would otherwise keep showing the pre-remesh pair. Queue::write_buffer
                    // only; a slotless chunk keeps the per-chunk CPU-draw fallback.
                    if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_mode) {
                        self.voxel_surface_pool_upload(ci_u, cid, true);
                    }
                }
            }
            meshed += 1;
            if t_start.elapsed().as_secs_f32() * 1000.0 >= frame_budget_ms { break; }
        }
        let mesh_ms = t_start.elapsed().as_secs_f32() * 1000.0;
        let total: u32 = self.sculpt.voxel.as_ref().map_or(0, |v| v.chunk_tris.iter().sum());
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.tris = total;
            // Profiler: record the MESH phase (Surface-Nets re-mesh of the drained chunks).
            v.profile.mesh_ms = mesh_ms;
            v.profile.chunks = meshed as u32;
            if mesh_ms > v.profile.mesh_peak { v.profile.mesh_peak = mesh_ms; }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// GPU live-mesh tick (toggle on): ASYNC. Advance the in-flight batch's maps non-blockingly and
    /// consume it if ready; then dispatch the next batch of dirty chunks. The CPU never blocks on the
    /// GPU — a batch dispatched one frame is collected the next. One batch in flight at a time.
    /// Bring the persistent GPU brick atlas (INDEX + POOL) in sync with the authoritative CPU grid
    /// (Sparse only — Dense uses `voxel_mesh_density`). The SPARSE counterpart of the dense buffer's
    /// fill / Z-band patch, called just before the mesh dispatch in `voxel_flush_gpu_async`.
    ///
    /// A FULL upload (recreate both buffers + write the whole atlas) happens on: first sync, a grid
    /// swap (res / brick_res mismatch), the POOL outgrowing its capacity, or `gc_pending` (a fresh fill
    /// or an arena GC remapped every handle). Otherwise an INCREMENTAL patch: the changed INDEX brick
    /// range + each POOL-dirty brick's 16³ block — bounded by what the stroke touched, never res³ (the
    /// 1024³ enabler). Either way the CPU sync flags are cleared afterward so the next frame starts fresh.
    ///
    /// Safe to write the persistent buffers here: Section B only runs with NO mesh in flight, so the
    /// previous dispatch's atlas reads have been submitted before these queue writes (wgpu orders them).
    pub(super) fn voxel_sync_mesh_atlas(&mut self) {
        if self.gpu.is_none() { return; }
        let Some(v) = self.sculpt.voxel.as_ref() else { return };
        if !v.grid.density.is_sparse() { return; }
        let res = v.grid.res;
        let brick_res = v.grid.density.brick_res();
        let index = v.grid.density.index_slice().unwrap();
        let pool = v.grid.density.bricks_slice().unwrap();
        let arena_bricks = (pool.len() / 4096) as u32;
        let gc = v.grid.density.gc_pending();
        // GPU pool is the live sculpt truth while an atlas stroke is in progress (or after scatter
        // before release materialize). Re-uploading dirty POOL bricks from the still-stale CPU would
        // wipe GPU scatter (stroke vanishes). INDEX tags stay CPU-authoritative always.
        let gpu_pool_authority = v.cpu_grid_stale || v.atlas_stroke;

        let device = self.gpu.as_ref().unwrap().device.clone();
        let queue = self.gpu.as_ref().unwrap().queue.clone();

        // A pure GROW (existing atlas, same grid, not gc, just outgrew its POOL capacity) is the ONLY
        // need_full case where the GPU POOL may hold data the CPU arena does NOT: during a GPU-resident
        // (atlas) stroke the brush writes the GPU POOL in place and the CPU grid stays pre-stroke until
        // release, so the GPU POOL is the authoritative accumulator. A grow must PRESERVE it (GPU→GPU
        // copy) — re-uploading the stale CPU arena would wipe every already-scattered brick (the stroke
        // "erasing itself" behind the cursor). Fresh / gc-remap / grid-swap full uploads come from the
        // CPU (which IS authoritative then). The copy path is also correct + faster for the box path.
        let is_grow = match self.gpu.as_ref().unwrap().voxel_mesh_atlas.as_ref() {
            Some(a) => !gc && a.res == res && a.brick_res == brick_res && a.pool_cap < arena_bricks,
            None => false,
        };
        // When GPU is authority and we would have done a non-grow full recreate (e.g. spurious path),
        // force the preserve path if an atlas already exists with matching layout.
        let preserve_gpu_pool = is_grow || (gpu_pool_authority
            && match self.gpu.as_ref().unwrap().voxel_mesh_atlas.as_ref() {
                Some(a) => !gc && a.res == res && a.brick_res == brick_res,
                None => false,
            });
        let need_full = is_grow || match self.gpu.as_ref().unwrap().voxel_mesh_atlas.as_ref() {
            None => true,
            Some(a) => gc || a.res != res || a.brick_res != brick_res,
        };
        // Mid-stroke with existing atlas + GPU authority: never need_full from CPU (preserve in place).
        let need_full = need_full && !(gpu_pool_authority && !is_grow && !gc
            && self.gpu.as_ref().unwrap().voxel_mesh_atlas.as_ref().map_or(false, |a| a.res == res && a.brick_res == brick_res));

        if need_full {
            // POOL capacity with 1.5× headroom (floor 64 bricks ≈ 1 MB) so a few new bricks per stroke
            // don't reallocate the buffer (a realloc forces a full re-upload — the slow path).
            let pool_cap = (arena_bricks + arena_bricks / 2).max(64);
            let index_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                label: Some("voxel_atlas_index"), size: ((index.len() as u64) * 4).max(4),
                usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false });
            let pool_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                label: Some("voxel_atlas_pool"), size: ((pool_cap as u64) * 4096 * 4).max(4),
                // COPY_SRC so a FUTURE grow can preserve this buffer's live contents via copy_buffer_to_buffer.
                usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST | eframe::wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false });
            // INDEX is always CPU-authoritative (tags are CPU bookkeeping, current even mid-stroke).
            queue.write_buffer(&index_buf, 0, bytemuck::cast_slice(index));
            if preserve_gpu_pool || is_grow {
                // Preserve the live GPU accumulator: copy the old POOL into the larger/new buffer, then
                // write ONLY newly-materialised dirty bricks from the CPU (implicit EMPTY/FULL fills).
                // Never re-upload the whole CPU pool (stale for already-scattered bricks mid-stroke).
                let old_pool = &self.gpu.as_ref().unwrap().voxel_mesh_atlas.as_ref().unwrap().pool_buf;
                let copy_bytes = old_pool.size().min((pool_cap as u64) * 4096 * 4);
                let mut enc = device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor { label: Some("voxel_atlas_grow_copy") });
                enc.copy_buffer_to_buffer(old_pool, 0, &pool_buf, 0, copy_bytes);
                queue.submit(std::iter::once(enc.finish()));
                if let Some(dirty) = v.grid.density.dirty_bricks() {
                    for &bi in dirty {
                        if let Some((h, block)) = v.grid.density.brick_pool_block(bi) {
                            queue.write_buffer(&pool_buf, (h as u64) * (block.len() as u64) * 4, bytemuck::cast_slice(block));
                        }
                    }
                }
            } else if !pool.is_empty() {
                queue.write_buffer(&pool_buf, 0, bytemuck::cast_slice(pool)); // fresh / gc / grid-swap: CPU is authoritative
            }
            let g = self.gpu.as_mut().unwrap();
            g.voxel_mesh_atlas = Some(crate::voxel_gpu_mesh::VoxelAtlasGpu {
                index_buf, pool_buf, pool_cap, brick_res, res });
            // BOTH atlas buffers (index + pool) were RECREATED → invalidate every cached resident BG (it
            // binds index/pool at bindings 6/7). The `else` branch writes the SAME buffers in place (per-
            // brick / index-range uploads) → no realloc, no bump. A pure GROW also lands here (is_grow ⇒
            // need_full) and DOES realloc the pool buffer, so it correctly bumps too.
            g.voxel_mesh_src_gen = g.voxel_mesh_src_gen.wrapping_add(1);
        } else {
            let atlas = self.gpu.as_ref().unwrap().voxel_mesh_atlas.as_ref().unwrap();
            // INDEX: upload only the changed contiguous brick range (materialise / demote).
            if let Some((lo, hi)) = v.grid.density.index_dirty_range() {
                queue.write_buffer(&atlas.index_buf, (lo as u64) * 4, bytemuck::cast_slice(&index[lo as usize..=hi as usize]));
            }
            // POOL: upload each dirty brick's 16³ block — but NOT when GPU pool is authoritative.
            // Mid-atlas-stroke dirty bricks are almost always freshly materialised (CPU fill of EMPTY/FULL
            // before scatter); those still need upload. Bricks dirtied by CPU edits while stale (Auto
            // Smooth on pre-stroke field) must never clobber GPU scatter.
            // Heuristic: only upload dirty bricks that also sit in the INDEX dirty range (materialise
            // marks both; set_idx/relax on an existing handle marks pool dirty only).
            if let Some(dirty) = v.grid.density.dirty_bricks() {
                let idx_range = v.grid.density.index_dirty_range();
                for &bi in dirty {
                    let allow = if gpu_pool_authority {
                        idx_range.map_or(false, |(lo, hi)| bi >= lo && bi <= hi)
                    } else {
                        true
                    };
                    if !allow { continue; }
                    if let Some((h, block)) = v.grid.density.brick_pool_block(bi) {
                        queue.write_buffer(&atlas.pool_buf, (h as u64) * (block.len() as u64) * 4, bytemuck::cast_slice(block));
                    }
                }
            }
        }
        // Sync bookkeeping cleared even when some dirty pool uploads were skipped under GPU authority
        // (those CPU dirties are discarded; GPU remains truth until materialize on release).
        self.sculpt.voxel.as_mut().unwrap().grid.density.clear_sync_dirty();
    }

    /// Ensure the shared vertex/index/indirect POOL is allocated and large enough for the current
    /// resident voxel object (the 1024³ draw-bound fix). POLL-PHASE ONLY (the pool is multi-GB — the
    /// Frame Ordering Contract bans buffer creates outside `poll_background_tasks`). Sized to the grid's
    /// SURFACE chunk count (NOT cd3 — at 1024³ cd3=64³=262k slots ≈ 77GB; the surface shell is ~12k),
    /// plus headroom so mid-stroke growth is rare; clamped to the device `max_buffer_size` inside
    /// `ensure_pool`. Honors a deferred grow request (`voxel_pool_grow_to`) set by the dispatch path when
    /// `alloc_slot` ran dry. No-op when resident meshing is off or no voxel object exists.
    pub(crate) fn voxel_ensure_pool(&mut self) {
        let resident = self.sculpt.voxel.as_ref().map_or(false, |v| v.gpu_mesh && v.gpu_resident_mesh && !v.chunks.is_empty());
        let grow_req = self.gpu.as_ref().map_or(0, |g| g.voxel_pool_grow_to);
        if !resident && grow_req == 0 { return; }
        let Some(g) = self.gpu.as_ref() else { return; };
        if g.voxel_mesh.is_none() { return; } // mesher compiles lazily in voxel_flush_gpu_async
        let cur_cap = self.gpu.as_ref().and_then(|g| g.voxel_mesh.as_ref()).map_or(0, |m| m.pool_cap_slots());
        // FAST PATH (Frame Budget Rule): once the pool is allocated and no grow is flagged, return early.
        if cur_cap > 0 && grow_req == 0 { return; }
        // Surface-chunk count + 1/4 headroom (a stroke grows the shell). Prefer `surface_chunk_count`
        // from remesh worker / build_all (never `chunks.len()` = chunk_dim³ ≈ 262k @1024³ → multi-GB pool).
        // Re-running O(res³) surface_chunk_mask on the main thread cost ~1.1s at 1024³ — forbidden.
        // Cap at ~16k (POOL_MAX ≈ 1GiB / 64KB per slot) so an unknown count cannot explode VRAM.
        const POOL_SURFACE_CAP: u32 = 16_384;
        let need = {
            let surface = self.sculpt.voxel.as_ref().map_or(0u32, |v| {
                if !(v.gpu_mesh && v.gpu_resident_mesh) { return 0; }
                if v.surface_chunk_count > 0 {
                    v.surface_chunk_count.min(POOL_SURFACE_CAP)
                } else {
                    // Unknown: conservative shell estimate from res (sphere ~ O(res² / chunk²)).
                    let cd = v.chunk_dim.max(1);
                    let est = ((cd as f32) * (cd as f32) * 0.35).ceil() as u32; // ~35% of mid-plane shell
                    est.clamp(256, POOL_SURFACE_CAP)
                }
            });
            let with_headroom = surface.saturating_add(surface / 4).max(256);
            with_headroom.max(grow_req).min(POOL_SURFACE_CAP)
        };
        if need <= cur_cap && cur_cap > 0 {
            // Already big enough — just clear the stale grow request (no realloc, no chunk reset).
            if let Some(g) = self.gpu.as_mut() { g.voxel_pool_grow_to = 0; }
            return;
        }
        let device = self.gpu.as_ref().unwrap().device.clone();
        let queue = self.gpu.as_ref().unwrap().queue.clone();
        if let Some(m) = self.gpu.as_mut().and_then(|g| g.voxel_mesh.as_mut()) {
            m.ensure_pool(&device, &queue, need);
        }
        // A pool realloc bumps pool_gen (folded into each chunk's bg_res_gen) AND reset the slot allocator
        // → every existing chunk's pool_slot is now stale (points into the old, freed pool). Clear the
        // resident chunk set so they re-mesh from scratch (re-alloc a slot in the new pool); the not-yet-
        // resident CPU mesh fallback covers them meanwhile. Same invalidation as a chunk_dim swap.
        if let Some(g) = self.gpu.as_mut() {
            g.voxel_chunk_gpu.clear();
            g.voxel_pool_grow_to = 0;
        }
        // Gate 1: the CPU-synced marks described slots in the FREED pool — clear them alongside
        // the chunk records (slot_was_none re-syncs topology on the next surface write anyway;
        // this keeps the two invalidation states in lockstep by construction, no reliance on it).
        if let Some(v) = self.sculpt.voxel.as_mut() { v.surface_pool_synced.clear(); }
    }

    pub(super) fn voxel_flush_gpu_async(&mut self) {
        // Per-frame re-mesh budget (chunks drained from dirty_chunks, NEAREST the brush first).
        // NON-resident path: readback-bound (~per-chunk staging cost) — a bigger batch trades framerate
        // for little chunks/sec, so keep it moderate. RESIDENT path (Phase 6): NO readback/inflight —
        // it dispatches straight into persistent per-chunk buffers, so throughput is GPU-dispatch-bound
        // and a bigger batch genuinely drains the backlog faster. A large brush at 512³ dirties ~1000
        // chunks/stamp; the higher resident cap keeps the surface from lagging the cursor (the stale-
        // patch "tearing"). Bounded so a virgin-region stamp (new buffers each) can't hitch the frame.
        const MAX_GPU_CHUNKS_PER_FRAME: usize = 96;
        let t = std::time::Instant::now();

        // Lazily compile the persistent mesher (once per session).
        if self.gpu.as_ref().map_or(false, |g| g.voxel_mesh.is_none()) {
            let device = self.gpu.as_ref().unwrap().device.clone();
            let m = crate::voxel_gpu_mesh::VoxelMeshGpu::new(&device);
            self.gpu.as_mut().unwrap().voxel_mesh = Some(m);
        }

        // A) Advance the in-flight maps without blocking and consume the batch once ready. Callback
        //    failures are explicit and re-queue the affected chunks; Pending is never converted into
        //    a UI-thread Wait merely because a large GPU batch took several frames.
        let mut consumed = 0usize;
        let mut requeued = false;
        let map_state = match self.gpu.as_ref().and_then(|g| g.voxel_mesh_inflight.as_ref()) {
            Some((_, infl)) => {
                self.gpu.as_ref().unwrap().device.poll(eframe::wgpu::PollType::Poll).expect("GPU device polling failed");
                Some(infl.map_state())
            }
            None => None,
        };
        match map_state {
            Some(crate::voxel_gpu_mesh::VoxelMeshMapState::Ready) => {
                let (chunk_ids, infl) = self.gpu.as_mut().unwrap().voxel_mesh_inflight.take().unwrap();
                let results = infl.collect();
                consumed = chunk_ids.len();
                // DECISION B: the GPU mesher emits no colour. If per-voxel paint colour is in play, pack
                // it into each landed chunk's CPU verts IN PLACE (recolor_vertices_by_position — derives
                // each vertex's anchor cell from its position, same chunk_vertex_color helper as the CPU
                // mesher → byte-identical, no release pop). The colour write lands AFTER the dispatch's
                // verts arrive here (the collect-before-use barrier), so density/topology come from the
                // GPU and only the colour is a cheap CPU word rewrite — never a CPU per-frame re-mesh.
                let recolor = self.sculpt.voxel.as_ref().map_or(false, |v| v.needs_gpu_vertex_recolor());
                for ((ci, cid), (mut verts, idx)) in chunk_ids.into_iter().zip(results.into_iter()) {
                    let tris = (idx.len() / 3) as u32;
                    if recolor {
                        if let Some(v) = self.sculpt.voxel.as_ref() { v.grid.recolor_vertices_by_position(&mut verts); }
                    }
                    self.cache.mesh_cache.update_editable_mesh(cid, (verts, idx), tris);
                    if let Some(v) = self.sculpt.voxel.as_mut() { if ci < v.chunk_tris.len() { v.chunk_tris[ci] = tris; } }
                }
            }
            Some(crate::voxel_gpu_mesh::VoxelMeshMapState::Failed) => {
                let (chunk_ids, _) = self.gpu.as_mut().unwrap().voxel_mesh_inflight.take().unwrap();
                let had_chunks = !chunk_ids.is_empty();
                eprintln!("[VXGPU] failed mesh batch re-queued ({} chunks)", chunk_ids.len());
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    for (ci, _) in chunk_ids { v.dirty_chunks.insert(ci as u32); }
                }
                requeued |= had_chunks;
            }
            Some(crate::voxel_gpu_mesh::VoxelMeshMapState::Pending) => {}
            None => {}
        }

        // B) If nothing is in flight now, dispatch the next capped batch of dirty chunks.
        if self.gpu.as_ref().map_or(true, |g| g.voxel_mesh_inflight.is_none()) {
            let (res, chunk_dim) = match self.sculpt.voxel.as_ref() {
                Some(v) => (v.grid.res, v.chunk_dim),
                None => return,
            };
            // Capture before mut-borrow of voxel (disjoint fields, but rustc needs the order).
            let prev_wall = self.prev_frame_wall_ms;
            let stroke_active = self.sculpt.stroke_active;
            let picked: Vec<(usize, core_math::scene_graph::MeshCacheId, ([u32; 3], [u32; 3]))> = {
                let v = self.sculpt.voxel.as_mut().unwrap();
                // Cap first (before pick) — 1024³ can hold 10k+ dirty; never full-sort that set.
                // Focus floor: always remesh enough under the tip mid-stroke (Coat continuous surface).
                let res_floor: usize = if v.grid.res >= 896 {
                    if stroke_active { 10 } else { 6 }
                } else if v.grid.res >= 768 {
                    if stroke_active { 12 } else { 6 }
                } else if v.grid.res >= 512 {
                    if stroke_active { 14 } else { 6 }
                } else if v.grid.res >= 256 {
                    if stroke_active { 18 } else { 10 }
                } else {
                    if stroke_active { 28 } else { 14 }
                };
                let mut cap = if v.gpu_resident_mesh {
                    (v.resident_cap as usize).max(res_floor).min(if v.grid.res >= 896 {
                        if stroke_active { 36 } else { 20 }
                    } else if v.grid.res >= 512 {
                        if stroke_active { 40 } else { 20 }
                    } else {
                        2048
                    })
                } else {
                    MAX_GPU_CHUNKS_PER_FRAME.min(if v.grid.res >= 896 {
                        if stroke_active { 28 } else { 18 }
                    } else if v.grid.res >= 512 {
                        if stroke_active { 32 } else { 18 }
                    } else {
                        96
                    })
                };
                // LIVE stroke: force a strong near-brush batch so the tip is never starved by backlog.
                if stroke_active {
                    cap = cap.max(res_floor).min(if v.grid.res >= 512 { 44 } else { 64 });
                }
                if prev_wall > 40.0 {
                    // Still keep focus floor mid-stroke — never collapse to 2 under the tip.
                    cap = if stroke_active { cap.min(res_floor.max(6)) } else { cap.min(2) };
                } else if prev_wall > 22.0 || v.dirty_chunks.len() > 800 {
                    cap = if stroke_active {
                        cap.min(res_floor.max(12))
                    } else {
                        cap.min(res_floor.max(4))
                    };
                } else if prev_wall < 12.0 {
                    // Free frame: drain faster — mid-stroke too.
                    cap = (cap + cap / 2).min(if v.grid.res >= 896 {
                        if stroke_active { 36 } else { 28 }
                    } else if stroke_active {
                        56
                    } else {
                        48
                    });
                }
                // Backlog caps: protect FPS, but do not starve the focus floor mid-stroke.
                if v.dirty_chunks.len() > 8000 {
                    cap = if stroke_active { cap.min(res_floor.max(6)) } else { cap.min(3) };
                } else if v.dirty_chunks.len() > 3000 {
                    cap = if stroke_active {
                        cap.min(res_floor.max(8))
                    } else {
                        cap.min(if v.grid.res >= 896 { 4 } else { 3 })
                    };
                }
                let mut cand: Vec<u32> = v.dirty_chunks.iter().copied().collect();
                let n = cand.len();
                // Nearest-to-brush + anti-starvation far quarter. Full O(n log n) sort of 10k+ dirty
                // chunks was a measured hitch @1024³ — use select_nth (avg O(n)) instead.
                let taken: Vec<u32> = if n <= cap {
                    cand
                } else if let Some([fx, fy, fz]) = v.focus_chunk {
                    let cd = chunk_dim.max(1) as u64;
                    let dist_key = |ci: &u32| -> u64 {
                        let c = *ci as u64;
                        let cx = c % cd;
                        let cy = (c / cd) % cd;
                        let cz = c / (cd * cd);
                        let dx = cx as i64 - fx as i64;
                        let dy = cy as i64 - fy as i64;
                        let dz = cz as i64 - fz as i64;
                        (dx * dx + dy * dy + dz * dz) as u64
                    };
                    let near = (cap * 3 / 4).max(1);
                    let far = (cap - near).max(1);
                    // Farthest `far` → end of array
                    cand.select_nth_unstable_by_key(n - far, dist_key);
                    let far_part: Vec<u32> = cand[n - far..].to_vec();
                    // Nearest `near` from the rest
                    let mut head = cand[..n - far].to_vec();
                    if head.len() > near {
                        head.select_nth_unstable_by_key(near - 1, dist_key);
                        head.truncate(near);
                    }
                    head.extend(far_part);
                    head
                } else {
                    // No focus: take arbitrary cap (still drains; no sort cost).
                    cand.truncate(cap);
                    cand
                };
                for &ci in &taken { v.dirty_chunks.remove(&ci); }
                taken.into_iter().filter_map(|ci_u| {
                    let ci = ci_u as usize;
                    v.chunks.get(ci).map(|&cid| (ci, cid, voxel_chunk_anchor(ci_u, chunk_dim, res)))
                }).collect()
            };
            if !picked.is_empty() {
                let ranges: Vec<([u32; 3], [u32; 3])> = picked.iter().map(|p| p.2).collect();
                let chunk_ids: Vec<(usize, core_math::scene_graph::MeshCacheId)> = picked.iter().map(|p| (p.0, p.1)).collect();
                // Keep the GPU density source current for the picked chunks. SPARSE (res ≥ 256) syncs the
                // persistent brick ATLAS per-brick from the CPU dirty-tracking (Phase 4b-2 — the 1024³
                // path; no res³ buffer). DENSE (< 256) keeps a persistent res³ buffer filled once on
                // (re)creation, then patched by the dirty Z-band covering the picked chunks ± the 1-voxel
                // mesh halo (full XY, one contiguous write) — bounded by the edit's Z-extent (Phase 3c).
                let is_sparse = self.sculpt.voxel.as_ref().map_or(false, |v| v.grid.density.is_sparse());
                if is_sparse {
                    self.voxel_sync_mesh_atlas();
                } else {
                    let need_fill = self.gpu.as_ref().map_or(true, |g| g.voxel_mesh_density.is_none() || g.voxel_mesh_density_res != res);
                    if need_fill {
                        let device = self.gpu.as_ref().unwrap().device.clone();
                        let n = (res as u64) * (res as u64) * (res as u64) * 4;
                        let buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                            label: Some("voxel_mesh_density"), size: n.max(4),
                            usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false });
                        let dense = self.sculpt.voxel.as_ref().unwrap().grid.density.to_dense();
                        self.gpu.as_ref().unwrap().queue.write_buffer(&buf, 0, bytemuck::cast_slice(&dense));
                        let g = self.gpu.as_mut().unwrap();
                        g.voxel_mesh_density = Some(buf);
                        g.voxel_mesh_density_res = res;
                        // Density buffer RECREATED → invalidate every cached resident BG (it binds this at
                        // binding 0). A Z-band patch (else branch) writes the SAME buffer → no bump needed.
                        g.voxel_mesh_src_gen = g.voxel_mesh_src_gen.wrapping_add(1);
                    } else {
                        let (mut z_lo, mut z_hi) = (u32::MAX, 0u32);
                        for &(a0, a1) in &ranges { z_lo = z_lo.min(a0[2]); z_hi = z_hi.max(a1[2]); }
                        let z0 = z_lo.saturating_sub(1);             // mesh vertex halo reads az0-1
                        let z1 = (z_hi + 1).min(res - 1);            // corner read reaches az1+1
                        let band = self.sculpt.voxel.as_ref().unwrap().grid.density.z_slice(z0, z1);
                        let off = (z0 as u64) * (res as u64) * (res as u64) * 4;
                        let gpu = self.gpu.as_ref().unwrap();
                        gpu.queue.write_buffer(gpu.voxel_mesh_density.as_ref().unwrap(), off, bytemuck::cast_slice(&band));
                    }
                }
                // DECISION B: when per-voxel paint colour must be packed into the meshed vertices, force
                // the NON-RESIDENT path. The resident path writes straight into the GPU pool (no CPU
                // readback), so there is no CPU vertex array to apply the in-place paint colour to; the
                // GPU mesher emits no colour, so resident-meshed painted chunks would render uncoloured.
                // The non-resident readback yields CPU verts → recolor_vertices_by_position then packs
                // the paint colour (in the readback handler below) before the mesh reaches the G-buffer.
                let needs_recolor = self.sculpt.voxel.as_ref().map_or(false, |v| v.needs_gpu_vertex_recolor());
                let is_resident = self.sculpt.voxel.as_ref().map_or(false, |v| v.gpu_resident_mesh) && !needs_recolor;
                if is_resident {
                    // GPU-RESIDENT (Phase 6 + POOL): mesh straight into a SUB-RANGE of the shared vertex/
                    // index pool, draw ALL chunks in ONE multi_draw_indexed_indirect (raster_render). NO
                    // inflight, NO readback, NO per-chunk buffer create. The pool is sized in poll
                    // (`voxel_ensure_pool`, Frame Ordering Contract); here we only assign slots + create the
                    // two tiny per-chunk buffers (counters 8B + params 256B).
                    let cd3 = (chunk_dim as usize).pow(3);
                    let device = self.gpu.as_ref().unwrap().device.clone();
                    let queue = self.gpu.as_ref().unwrap().queue.clone();
                    // (1) Assign a pool slot + ensure per-chunk buffers for each picked chunk. A chunk whose
                    // slot allocation fails (pool exhausted this frame) is SKIPPED here, re-queued dirty, and
                    // a deferred grow is flagged for poll — never silently dropped, never OOB.
                    let mut ok_idx: Vec<usize> = Vec::with_capacity(picked.len()); // positions in picked/ranges that got a slot
                    let mut pool_exhausted = false;
                    {
                        // Split GpuCtx so voxel_mesh (the mesher, &mut for alloc_slot) and voxel_chunk_gpu
                        // (the slot Vec) are disjoint borrows. ensure_chunk_gpu is now a mesher method.
                        let crate::GpuCtx { voxel_mesh, voxel_chunk_gpu, .. } = self.gpu.as_mut().unwrap();
                        if voxel_chunk_gpu.len() < cd3 { voxel_chunk_gpu.resize_with(cd3, || None); }
                        let mesher = voxel_mesh.as_mut().unwrap();
                        for (k, (&ci, &(a0, a1))) in picked.iter().map(|p| &p.0).zip(ranges.iter()).enumerate() {
                            // cap = the chunk's OWN worst case (vspan³ verts, acells·18 idx); ensure_chunk_gpu
                            // clamps it to the slot stride. The shader's arrayLength guard (bound by the slot
                            // sub-range) is the independent hard bounds-safety against an OOB indirect draw.
                            let vspan = [a1[0]-a0[0].saturating_sub(1)+1, a1[1]-a0[1].saturating_sub(1)+1, a1[2]-a0[2].saturating_sub(1)+1];
                            let cap_verts = vspan[0] * vspan[1] * vspan[2];
                            let cap_idx = (a1[0]-a0[0]+1) * (a1[1]-a0[1]+1) * (a1[2]-a0[2]+1) * crate::voxel_gpu_mesh::SN_MAX_IDX_PER_ANCHOR_CELL;
                            if mesher.ensure_chunk_gpu(&mut voxel_chunk_gpu[ci], &device, &queue, cap_verts, cap_idx) {
                                ok_idx.push(k);
                            } else {
                                pool_exhausted = true;
                            }
                        }
                    }
                    if pool_exhausted {
                        // Re-queue the chunks that couldn't get a slot + flag a pool grow for poll. They
                        // re-mesh next frame against the grown pool. Grow target = current chunk grid (cd3)
                        // so a stroke that touches the whole surface eventually gets every chunk resident.
                        if let Some(v) = self.sculpt.voxel.as_mut() {
                            let ok_set: std::collections::HashSet<usize> = ok_idx.iter().copied().collect();
                            for (k, p) in picked.iter().enumerate() {
                                if !ok_set.contains(&k) { v.dirty_chunks.insert(p.0 as u32); }
                            }
                        }
                        if let Some(g) = self.gpu.as_mut() { g.voxel_pool_grow_to = g.voxel_pool_grow_to.max(cd3 as u32); }
                        requeued = true;
                    }
                    if !ok_idx.is_empty() {
                        // Only the chunks that got a slot dispatch this frame.
                        let ranges_ok: Vec<([u32; 3], [u32; 3])> = ok_idx.iter().map(|&k| ranges[k]).collect();
                        // dispatch_chunks_resident is &mut (it owns persistent SCRATCH it grows lazily AND
                        // mutates each chunk's CACHED bind group). Split GpuCtx into DISJOINT field borrows:
                        // the mesher (&mut voxel_mesh), the src buffer (& atlas / density), and the per-chunk
                        // slot Vec (&mut voxel_chunk_gpu). device/queue come from earlier clones.
                        let gpu = self.gpu.as_mut().unwrap();
                        let grid = &self.sculpt.voxel.as_ref().unwrap().grid;
                        let src_gen = gpu.voxel_mesh_src_gen;
                        let crate::GpuCtx { voxel_mesh, voxel_mesh_atlas, voxel_mesh_density, voxel_chunk_gpu, .. } = gpu;
                        let src = if is_sparse {
                            voxel_mesh_atlas.as_ref().unwrap().source()
                        } else {
                            crate::voxel_gpu_mesh::MeshDensitySource::Dense(voxel_mesh_density.as_ref().unwrap())
                        };
                        // chunk_refs must be &mut (the dispatch caches a BG per chunk). picked indices are
                        // UNIQUE (drained from dirty_chunks), so distinct slots → no aliasing. Build them in
                        // ranges_ok order via a chunk-index→position map over iter_mut (the borrow-checker-
                        // safe way to get many disjoint &mut out of a Vec by arbitrary index).
                        let mut pos_of: std::collections::HashMap<usize, usize> = std::collections::HashMap::with_capacity(ok_idx.len());
                        for (pos, &k) in ok_idx.iter().enumerate() { pos_of.insert(picked[k].0, pos); }
                        let mut ordered: Vec<Option<&mut crate::voxel_gpu_mesh::VoxelChunkGpu>> =
                            (0..ok_idx.len()).map(|_| None).collect();
                        for (ci, slot) in voxel_chunk_gpu.iter_mut().enumerate() {
                            if let Some(&pos) = pos_of.get(&ci) { ordered[pos] = Some(slot.as_mut().unwrap()); }
                        }
                        let mut chunk_refs: Vec<&mut crate::voxel_gpu_mesh::VoxelChunkGpu> =
                            ordered.into_iter().map(|o| o.unwrap()).collect();
                        voxel_mesh.as_mut().unwrap().dispatch_chunks_resident(&device, &queue, grid, src, &ranges_ok, &mut chunk_refs, src_gen);
                    }
                    consumed = ok_idx.len(); // diagnostic: chunks meshed this frame (no collect for resident)
                } else {
                    let infl = {
                        let gpu = self.gpu.as_ref().unwrap();
                        let grid = &self.sculpt.voxel.as_ref().unwrap().grid;
                        let src = if is_sparse {
                            gpu.voxel_mesh_atlas.as_ref().unwrap().source()
                        } else {
                            crate::voxel_gpu_mesh::MeshDensitySource::Dense(gpu.voxel_mesh_density.as_ref().unwrap())
                        };
                        gpu.voxel_mesh.as_ref().unwrap().dispatch_chunks(&gpu.device, &gpu.queue, grid, src, &ranges)
                    };
                    self.gpu.as_mut().unwrap().voxel_mesh_inflight = Some((chunk_ids, infl));
                }
            }
        }

        let mesh_ms = t.elapsed().as_secs_f32() * 1000.0;
        // Whole-frame wall time (GPU-inclusive). `mesh_ms` is only the CPU command-recording cost; since
        // batching slashed that, mesh_ms alone can't see when the GPU is the bottleneck (a too-big batch's
        // mesh shows up as a long FRAME, not long mesh_ms) — so the cap below also watches this.
        let prev_frame_wall = self.prev_frame_wall_ms;
        // [VXMESH-GPU] last measured GPU time of the mesh passes (from the mesher's own query set).
        let mesh_gpu_ms = self.gpu.as_ref().and_then(|g| g.voxel_mesh.as_ref()).map_or(0.0, |m| m.last_mesh_gpu_ms);
        let total: u32 = self.sculpt.voxel.as_ref().map_or(0, |v| v.chunk_tris.iter().sum());
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.tris = total;
            v.profile.mesh_ms = mesh_ms;
            v.profile.mesh_gpu_ms = mesh_gpu_ms;
            v.profile.frame_ms = prev_frame_wall;
            v.profile.chunks = consumed as u32; // chunks UPLOADED this frame (collected)
            v.profile.dirty_backlog = v.dirty_chunks.len() as u32; // diagnostic: chunks still waiting
            if mesh_ms > v.profile.mesh_peak { v.profile.mesh_peak = mesh_ms; }
            // Adaptive resident cap (keeps 1024³ smooth): aim the mesh phase at ~TARGET_MS. Back off hard
            // when it runs long; grow only when the cap was the limiter (drained `cap` chunks) AND meshing
            // was cheap AND backlog remains — i.e. there's more work and budget to spend on it. Bounded.
            if v.gpu_resident_mesh && consumed > 0 {
                const TARGET_MS: f32 = 10.0;
                // Frame ceiling: above this the GPU (not the CPU recorder) is the limiter — meshing a
                // too-big batch lands here, never in mesh_ms. Back off on it, and only grow when BOTH
                // the recorder AND the whole frame are comfortably under budget. This stops the cap from
                // ramping to thousands on cheap CPU encode and then stalling the GPU for seconds at 1024³.
                // PERF: floor was 32 which still cost ~42ms/frame after big Move @512³ (log SPIKE
                // voxel_flush_dirty_chunks). Floor 4 lets the governor actually recover FPS.
                const FRAME_CEIL_MS: f32 = 18.0;
                let hit_cap = consumed as u32 >= v.resident_cap;
                let floor = if v.grid.res >= 896 {
                    3
                } else if v.grid.res >= 768 {
                    2
                } else if v.grid.res >= 512 {
                    3
                } else if v.grid.res >= 256 {
                    6
                } else {
                    8
                };
                // 1024³: allow higher resident ceiling when free (Coat drains mesh after stroke).
                let res_cap_max = if v.grid.res >= 896 { 48 } else if v.grid.res >= 512 { 96 } else { 1024 };
                if prev_frame_wall > FRAME_CEIL_MS {
                    v.resident_cap = (v.resident_cap / 2).max(floor);
                } else if mesh_ms > TARGET_MS * 1.3 {
                    v.resident_cap = (v.resident_cap * 3 / 4).max(floor);
                } else if mesh_ms < TARGET_MS * 0.6
                    && prev_frame_wall < FRAME_CEIL_MS * 0.6
                    && hit_cap
                    && !v.dirty_chunks.is_empty()
                {
                    v.resident_cap = (v.resident_cap + v.resident_cap / 4 + 4).min(res_cap_max);
                }
            }
        }
        let poll_pending = self.gpu.as_ref().is_some_and(|g| g.voxel_mesh_inflight.is_some());
        if voxel_gpu_flush_needs_render(consumed, requeued, poll_pending) {
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    /// Consume the in-flight async GPU brush batch (if any): read the edited slice back, write it into
    /// the authoritative CPU grid + record undo, run Pinch's relax, and mark the edited chunks dirty.
    /// Called every frame from `poll_background_tasks` (BEFORE input, so the next raycast sees current
    /// density — identical freshness to the old blocking path), and as a safety net before each new
    /// dispatch and at stroke release. Pending work remains in flight; an explicit map failure releases
    /// the batch without reading an invalid mapped range, so later dabs are never blocked forever.
    pub(crate) fn voxel_brush_collect(&mut self) {
        self.voxel_brush_collect_mode(VoxelBrushCollectMode::PollOnly);
    }

    pub(super) fn voxel_brush_drain(&mut self) {
        self.voxel_brush_collect_mode(VoxelBrushCollectMode::Drain);
    }

    pub(super) fn voxel_brush_collect_mode(&mut self, mode: VoxelBrushCollectMode) {
        if self.gpu.as_ref().map_or(true, |g| g.voxel_brush_inflight.is_none()) { return; }
        // Make the readback host-visible: non-blocking first. Mid-stroke: NEVER Wait (Coat continuous
        // clay) — leave the batch in flight and try again next poll; the next dispatch also calls
        // collect. On release / idle: Wait so undo commit is complete.
        let mut map_state = {
            let g = self.gpu.as_ref().unwrap();
            g.device.poll(eframe::wgpu::PollType::Poll).expect("GPU device polling failed");
            g.voxel_brush_inflight.as_ref().unwrap().map_state()
        };
        if voxel_brush_collect_should_wait(mode, map_state) {
            self.gpu.as_ref().unwrap().device.poll(eframe::wgpu::PollType::Wait).expect("GPU device polling failed");
            map_state = self.gpu.as_ref().unwrap().voxel_brush_inflight.as_ref().unwrap().map_state();
        }
        match map_state {
            crate::voxel_gpu_mesh::VoxelBrushMapState::Pending => return,
            crate::voxel_gpu_mesh::VoxelBrushMapState::Failed => {
                self.gpu.as_mut().unwrap().voxel_brush_inflight = None;
                eprintln!("[VXGPU] brush readback map failed — batch released; later dabs may retry");
                return;
            }
            crate::voxel_gpu_mesh::VoxelBrushMapState::Ready => {}
        }
        let mut infl = self.gpu.as_mut().unwrap().voxel_brush_inflight.take().unwrap();
        let (res, union, is_pinch) = (infl.res, infl.union, infl.is_pinch);
        let stamp_aabbs = std::mem::take(&mut infl.stamp_aabbs); // mark per-stamp (thin tube), not union
        let (newbox, prev_box) = infl.collect(); // box-local (union) layout: GPU result + pre-edit values
        let r = res as usize;
        let [ux0, uy0, uz0, ux1, uy1, uz1] = union;
        let (bdx, bdy) = ((ux1 - ux0 + 1) as usize, (uy1 - uy0 + 1) as usize);
        // Guard: if a resolution rebuild swapped the grid since dispatch, the box indexing no longer
        // matches this grid — discard (those edits belonged to the now-replaced grid). The normal
        // res-change path drains the batch BEFORE the swap, so this only fires on an unexpected race.
        let cur_res = self.sculpt.voxel.as_ref().map_or(0, |v| v.grid.res);
        if cur_res != res || newbox.len() != bdx * bdy * ((uz1 - uz0 + 1) as usize) || prev_box.len() != newbox.len() { return; }
        {
            let rec = &mut self.sculpt.voxel_stroke; // disjoint field borrow from self.sculpt.voxel below
            if let Some(v) = self.sculpt.voxel.as_mut() {
                // Write back ONLY the changed voxels. `prev_box[sidx]` is the pre-dispatch grid value
                // (the grid in this box is untouched between dispatch and collect — one batch in flight),
                // so it IS the correct pre-edit value for undo AND lets us diff WITHOUT re-decoding the
                // sparse grid per voxel (the old `get_idx`×2/voxel was the large-brush writeback cost).
                // `sidx` is BOX-LOCAL (union layout, matches dispatch_stamps_region).
                //
                // Iterate the per-stamp tube AABBs, NOT the full union box: for a fast diagonal flick the
                // union is the whole-stroke bounding box (~1M voxels) while the swept brush tube is ~20×
                // smaller. Each stamp AABB (`grid.brush_aabb`, center ± radius) bounds the GPU brush
                // footprint, so their union covers every voxel the GPU could have changed — the diff gate
                // (`nv != ov`) drops the rest. Overlapping stamps re-touch a voxel harmlessly: `rec.record`
                // first-touch-dedups the undo value, `set_idx(nv)` is idempotent. Fall back to the union
                // when no per-stamp AABBs were recorded (mirrors the chunk-marking fallback below).
                let boxes: &[[u32; 6]] = if stamp_aabbs.is_empty() { std::slice::from_ref(&union) } else { &stamp_aabbs };
                for &[bx0, by0, bz0, bx1, by1, bz1] in boxes {
                    for z in bz0..=bz1 { for y in by0..=by1 { for x in bx0..=bx1 {
                        let sidx = (x - ux0) as usize + (y - uy0) as usize * bdx + (z - uz0) as usize * bdx * bdy;
                        let (nv, ov) = (newbox[sidx], prev_box[sidx]);
                        if (nv - ov).abs() > 1e-7 {
                            let gidx = x as usize + y as usize * r + z as usize * r * r;
                            rec.record(gidx as u32, ov);
                            v.grid.density.set_idx(gidx, nv);
                        }
                    }}}
                }
                // Pinch redistancing: defer while stroke is active (union relax over a large box is the
                // main Pinch hitch @512/1024). Release path runs a mild relax after density lands.
                if is_pinch && !self.sculpt.stroke_active {
                    // Mild relax only — 0.3 was erasing pinch so intensity changes looked dead.
                    // Prefer per-stamp tubes when available (much smaller than union AABB).
                    let boxes: &[[u32; 6]] = if stamp_aabbs.is_empty() {
                        std::slice::from_ref(&union)
                    } else {
                        &stamp_aabbs
                    };
                    for &aabb in boxes {
                        v.grid.relax_region(aabb, 1, 0.12, true, Some(rec));
                    }
                }
                // Auto Smooth: mild post-stamp relax. At high res mid-stroke this is deferred to
                // release (see handle_sculpt_input) so continuous clay is not taxed every frame —
                // Coat keeps volume edits free and smooths as a light finish, not a hitch.
                let defer_as = self.sculpt.stroke_active && v.grid.res >= 512;
                if v.auto_smooth
                    && !defer_as
                    && !matches!(
                        v.brush,
                        core_math::voxel_sculpt::VoxelBrush::Smooth
                            | core_math::voxel_sculpt::VoxelBrush::Move
                            | core_math::voxel_sculpt::VoxelBrush::Smudge
                    )
                {
                    let amt = (v.auto_smooth_intensity * 0.45).clamp(0.05, 0.5);
                    let boxes: &[[u32; 6]] = if stamp_aabbs.is_empty() {
                        std::slice::from_ref(&union)
                    } else {
                        &stamp_aabbs
                    };
                    for &aabb in boxes {
                        v.grid.relax_region(aabb, 1, amt, true, Some(rec));
                    }
                }
            }
        }
        // Re-mesh the edited chunks (now that the density has landed). Mesh flush runs later this frame.
        // Mark ONLY the per-stamp tube (NOT the union AABB — over-marks ~20× for a fast diagonal flick
        // and floods the mesher → the fast-stroke tearing). Falls back to the union if no stamps recorded.
        if stamp_aabbs.is_empty() { self.voxel_mark_chunks_in_region(union); }
        else { for r in &stamp_aabbs { self.voxel_mark_chunks_in_region(*r); } }
        self.voxel_expand_stroke_aabb(union); // for sparse compaction on release
        // Density landed this frame → force a redraw so remesh/render cannot early-out as clean.
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Prefer stroke-region materialise when `region` is set (1024^3 release); else full arena.
    pub(crate) fn materialize_cpu_from_atlas_prefer(&mut self, region: Option<[u32; 6]>) {
        if let Some(aabb) = region {
            if self.materialize_cpu_from_atlas_region(aabb) {
                return;
            }
        }
        self.materialize_cpu_from_atlas();
    }

    /// Phase B: full sparse POOL GPU->CPU. Remesh/save/duplicate + fallback after region path.
    /// NEVER on the mid-stroke hot path. Reuses `voxel_pool_rb_staging`.
    pub(crate) fn materialize_cpu_from_atlas(&mut self) {
        let stale = self.sculpt.voxel.as_ref().map_or(false, |v| v.cpu_grid_stale && v.grid.density.is_sparse());
        if !stale { return; }
        let arena_floats = match self.sculpt.voxel.as_ref() {
            Some(v) => v.grid.density.arena_floats(),
            None => return,
        };
        if arena_floats == 0 {
            if let Some(v) = self.sculpt.voxel.as_mut() { v.cpu_grid_stale = false; }
            return;
        }
        let nbytes = (arena_floats as u64) * 4;
        let pool: Vec<f32> = {
            let gpu = match self.gpu.as_mut() { Some(g) => g, None => return };
            let atlas = match gpu.voxel_mesh_atlas.as_ref() { Some(a) => a, None => return };
            if nbytes > atlas.pool_buf.size() { return; }
            let need_new = gpu.voxel_pool_rb_staging.as_ref().map_or(true, |(_, cap)| *cap < nbytes);
            if need_new {
                let staging = gpu.device.create_buffer(&eframe::wgpu::BufferDescriptor {
                    label: Some("vbr_pool_rb"), size: nbytes,
                    usage: eframe::wgpu::BufferUsages::COPY_DST | eframe::wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false });
                gpu.voxel_pool_rb_staging = Some((staging, nbytes));
            }
            let (staging, _) = gpu.voxel_pool_rb_staging.as_ref().unwrap();
            let pool_buf = &atlas.pool_buf;
            let mut enc = gpu.device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor { label: Some("vbr_pool_rb_enc") });
            enc.copy_buffer_to_buffer(pool_buf, 0, staging, 0, nbytes);
            gpu.queue.submit(std::iter::once(enc.finish()));
            let (tx, rx) = std::sync::mpsc::channel();
            staging.slice(0..nbytes).map_async(eframe::wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
            gpu.device.poll(eframe::wgpu::PollType::Wait).expect("GPU device polling failed");
            match rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    eprintln!("[VXGPU] pool readback map_async failed: {e:?}");
                    return;
                }
                Err(_) => {
                    eprintln!("[VXGPU] pool readback channel closed");
                    return;
                }
            }
            let out = bytemuck::cast_slice::<u8, f32>(&staging.slice(0..nbytes).get_mapped_range()).to_vec();
            staging.unmap();
            out
        };
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.grid.density.refresh_pool_arena(&pool);
            v.cpu_grid_stale = false;
        }
    }

    /// Partial materialise: only pool slots for bricks overlapping `aabb` (+1 brick halo).
    /// Returns true if completed; false if caller should full-materialise.
    pub(super) fn materialize_cpu_from_atlas_region(&mut self, aabb: [u32; 6]) -> bool {
        let stale = self.sculpt.voxel.as_ref().map_or(false, |v| v.cpu_grid_stale && v.grid.density.is_sparse());
        if !stale { return true; }
        let res = self.sculpt.voxel.as_ref().map(|v| v.grid.res).unwrap_or(0);
        let exp = |v: u32, lo: bool| -> u32 {
            if lo { v.saturating_sub(16) } else { (v + 16).min(res.saturating_sub(1)) }
        };
        let region = [
            exp(aabb[0], true), exp(aabb[1], true), exp(aabb[2], true),
            exp(aabb[3], false), exp(aabb[4], false), exp(aabb[5], false),
        ];
        let handles = match self.sculpt.voxel.as_ref() {
            Some(v) => v.grid.density.allocated_handles_in_aabb(region),
            None => return true,
        };
        self.materialize_cpu_from_atlas_handles(handles)
    }

    /// Exact sparse release readback. The atlas undo builder already owns the complete first-touch
    /// brick set, so reading those handles avoids copying unrelated surface bricks inside a fast
    /// diagonal stroke's global AABB.
    pub(super) fn materialize_cpu_from_atlas_bricks(&mut self, bricks: &[u32]) -> bool {
        let stale = self.sculpt.voxel.as_ref().map_or(false, |v| {
            v.cpu_grid_stale && v.grid.density.is_sparse()
        });
        if !stale { return true; }
        let handles = match self.sculpt.voxel.as_ref() {
            Some(v) => bricks.iter().filter_map(|&bi| {
                let tag = v.grid.density.index_tag(bi);
                core_math::voxel_sculpt::DensityStorage::tag_is_allocated(tag).then_some(tag)
            }).collect(),
            None => return true,
        };
        self.materialize_cpu_from_atlas_handles(handles)
    }

    pub(super) fn materialize_cpu_from_atlas_handles(&mut self, mut handles: Vec<u32>) -> bool {
        handles.sort_unstable();
        handles.dedup();
        if handles.is_empty() {
            if let Some(v) = self.sculpt.voxel.as_mut() { v.cpu_grid_stale = false; }
            return true;
        }
        let arena_floats = self.sculpt.voxel.as_ref().map(|v| v.grid.density.arena_floats()).unwrap_or(0);
        const SLOT: u64 = 4096 * 4;
        if arena_floats > 0 && (handles.len() as u64) * 4096 > (arena_floats as u64) / 2 {
            return false;
        }
        let n = handles.len() as u64;
        let nbytes = n * SLOT;
        let data: Vec<f32> = {
            let gpu = match self.gpu.as_mut() { Some(g) => g, None => return false };
            let atlas = match gpu.voxel_mesh_atlas.as_ref() { Some(a) => a, None => return false };
            let min_cap = nbytes.max(SLOT * 64);
            let need_new = gpu.voxel_pool_rb_staging.as_ref().map_or(true, |(_, cap)| *cap < nbytes);
            if need_new {
                let staging = gpu.device.create_buffer(&eframe::wgpu::BufferDescriptor {
                    label: Some("vbr_pool_rb"), size: min_cap,
                    usage: eframe::wgpu::BufferUsages::COPY_DST | eframe::wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false });
                gpu.voxel_pool_rb_staging = Some((staging, min_cap));
            }
            let (staging, _) = gpu.voxel_pool_rb_staging.as_ref().unwrap();
            let pool_buf = &atlas.pool_buf;
            let mut enc = gpu.device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor {
                label: Some("vbr_pool_rb_region"),
            });
            for (i, &h) in handles.iter().enumerate() {
                let src = (h as u64) * SLOT;
                if src + SLOT > pool_buf.size() { continue; }
                enc.copy_buffer_to_buffer(pool_buf, src, staging, (i as u64) * SLOT, SLOT);
            }
            gpu.queue.submit(std::iter::once(enc.finish()));
            let (tx, rx) = std::sync::mpsc::channel();
            staging.slice(0..nbytes).map_async(eframe::wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
            gpu.device.poll(eframe::wgpu::PollType::Wait).expect("GPU device polling failed");
            match rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    eprintln!("[VXGPU] region pool readback map_async failed: {e:?}");
                    return false;
                }
                Err(_) => {
                    eprintln!("[VXGPU] region pool readback channel closed");
                    return false;
                }
            }
            let out = bytemuck::cast_slice::<u8, f32>(&staging.slice(0..nbytes).get_mapped_range()).to_vec();
            staging.unmap();
            out
        };
        if let Some(v) = self.sculpt.voxel.as_mut() {
            for (i, &h) in handles.iter().enumerate() {
                let o = i * 4096;
                if o + 4096 <= data.len() {
                    v.grid.density.refresh_pool_handle(h, &data[o..o + 4096]);
                }
            }
            v.cpu_grid_stale = false;
        }
        true
    }

    /// Phase C (depth-raycast): copy the cursor pixel of the rendered `gbuf_normal` (world-space normal
    /// in .xyz, world_dist in .w) so the voxel sculpt brush position comes from the VISIBLE surface, not
    /// a CPU `grid.raycast`. One-frame-late, non-blocking (mirrors the picking single-pixel readback).
    /// Runs after `dispatch_gpu_render` (gbuf is written) each frame while sculpting a voxel with the
    /// cursor over the viewport. The world ray is captured now and reconstructs the hit on collect.
    pub(crate) fn voxel_surface_pick(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        if self.sculpt.voxel.is_none() || self.tool.work_mode != WorkMode::Sculpt { return; }
        if self.pending_surface_pick.is_some() { return; } // one readback in flight at a time
        if rect.width() < 1.0 || rect.height() < 1.0 { return; }
        // During an active stroke, pick at the SAME lazy-mouse-smoothed cursor the brush uses (v.lazy_pos,
        // updated this frame in handle_voxel_sculpt_input), not the raw pointer. last_surface_hit is the
        // brush's preferred hit source in every config, so reading the raw cursor here made the Lazy slider
        // a no-op (the smoothing never reached stamp placement). Outside a stroke (hover), use the raw
        // pointer. No new smoothing state — just consume the position the brush already computed.
        let mp = if self.sculpt.stroke_active {
            self.sculpt.voxel.as_ref().and_then(|v| v.lazy_pos).map(|p| egui::pos2(p.x, p.y))
                .or_else(|| ctx.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos())))
        } else {
            ctx.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()))
        };
        let Some(mp) = mp else { return; };
        if !rect.contains(mp) { return; }
        let (ro, rd) = self.cam_ray(mp, rect);
        let ready = std::sync::Arc::new(crate::map_state::MapState::new());
        {
            let gpu = match self.gpu.as_mut() { Some(g) => g, None => return };
            let (gw, gh) = match gpu.gbuf_normal.as_ref() { Some(t) => (t.width(), t.height()), None => return };
            // Cursor → gbuf pixel (gbuf is at render resolution, which may differ from the egui rect).
            let ux = ((mp.x - rect.min.x) / rect.width()).clamp(0.0, 0.99999);
            let uy = ((mp.y - rect.min.y) / rect.height()).clamp(0.0, 0.99999);
            let px = ((ux * gw as f32) as u32).min(gw - 1);
            let py = ((uy * gh as f32) as u32).min(gh - 1);
            if gpu.surface_pick_staging.is_none() {
                gpu.surface_pick_staging = Some(gpu.device.create_buffer(&eframe::wgpu::BufferDescriptor {
                    label: Some("surface_pick_staging"), size: 256,
                    usage: eframe::wgpu::BufferUsages::MAP_READ | eframe::wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false }));
            }
            let staging = gpu.surface_pick_staging.as_ref().unwrap();
            let gn = gpu.gbuf_normal.as_ref().unwrap();
            let mut enc = gpu.device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor { label: Some("surface_pick_enc") });
            enc.copy_texture_to_buffer(
                eframe::wgpu::ImageCopyTexture { texture: gn, mip_level: 0, origin: eframe::wgpu::Origin3d { x: px, y: py, z: 0 }, aspect: eframe::wgpu::TextureAspect::All },
                eframe::wgpu::ImageCopyBuffer { buffer: staging, layout: eframe::wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: None } },
                eframe::wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
            gpu.queue.submit(std::iter::once(enc.finish()));
            let r2 = ready.clone();
            staging.slice(..8).map_async(eframe::wgpu::MapMode::Read, move |result| {
                r2.store_result(&result);
            });
        }
        self.pending_surface_pick = Some(crate::PendingSurfacePick { map_ready: ready, ro, rd });
    }

    /// Collect the surface-pick readback (Phase C): when ready, decode the cursor pixel (4× f16:
    /// normal.xyz + world_dist), reconstruct `hit = ro + rd·world_dist` from the ray captured at copy
    /// time, transform into voxel-LOCAL space, and store it as `last_surface_hit` for the brush. A
    /// world_dist of ~0 means the cursor is over empty space (cleared gbuf) → `None`. Non-blocking;
    /// called each frame from `poll_background_tasks`.
    pub(crate) fn poll_pending_surface_pick(&mut self) {
        if self.pending_surface_pick.is_none() { return; }
        let map_state = {
            let g = match self.gpu.as_ref() { Some(g) => g, None => { self.pending_surface_pick = None; return; } };
            g.device.poll(eframe::wgpu::PollType::Poll).expect("GPU device polling failed");
            self.pending_surface_pick.as_ref().unwrap().map_ready.peek()
        };
        if map_state == crate::map_state::PENDING { return; }
        if map_state == crate::map_state::ERR {
            self.pending_surface_pick = None;
            if let Some(gpu) = self.gpu.as_mut() {
                // Recreate on the next frame instead of trusting a buffer whose map failed.
                gpu.surface_pick_staging = None;
            }
            if let Some(v) = self.sculpt.voxel.as_mut() { v.last_surface_hit = None; }
            eprintln!("[VXGPU] surface-pick readback map failed — staging released for retry");
            return;
        }
        let (ro, rd) = { let p = self.pending_surface_pick.as_ref().unwrap(); (p.ro, p.rd) };
        let texel = match self.gpu.as_ref().and_then(|g| g.surface_pick_staging.as_ref()) {
            Some(staging) => {
                let data = staging.slice(..8).get_mapped_range();
                let h = |i: usize| half_to_f32(u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]));
                let t = [h(0), h(1), h(2), h(3)];
                drop(data);
                staging.unmap();
                t
            }
            None => { self.pending_surface_pick = None; return; }
        };
        self.pending_surface_pick = None;
        let wdist = texel[3];
        if !(wdist > 1e-4) { // cleared gbuf (.w == 0) → cursor over empty space, no hit
            if let Some(v) = self.sculpt.voxel.as_mut() { v.last_surface_hit = None; }
            return;
        }
        let hit_world = ro + rd * wdist;
        let n_world = glam::Vec3::new(texel[0], texel[1], texel[2]);
        // Transform into the voxel node's LOCAL space — the same space the CPU raycast/brush operate in.
        let vnode = self.sculpt.voxel.as_ref().map(|v| v.node);
        let (hit_local, n_local) = match vnode.map(|n| self.scene.scene_graph.world_transform(n)) {
            Some(w) => { let inv = w.inverse(); (inv.transform_point3(hit_world), inv.transform_vector3(n_world).normalize_or_zero()) }
            None => (hit_world, n_world.normalize_or_zero()),
        };
        // `gbuf_normal` carries ALL rendered geometry (floor grid, other objects), but the CPU raycast
        // only hit the voxel surface. Reject hits outside the grid's local bounds so the cursor over the
        // floor/background doesn't phantom-sculpt — the voxel grid spans local [-bounds, bounds].
        let bounds = self.sculpt.voxel.as_ref().map_or(0.0, |v| v.grid.bounds) * 1.1;
        let inside = bounds > 0.0 && hit_local.x.abs() <= bounds && hit_local.y.abs() <= bounds && hit_local.z.abs() <= bounds;
        if let Some(v) = self.sculpt.voxel.as_mut() { v.last_surface_hit = if inside { Some((hit_local, n_local)) } else { None }; }
    }

    /// Restore a voxel density delta and re-mesh; returns the inverse delta (for redo).
    pub(crate) fn voxel_restore_delta(&mut self, delta: &[(u32, f32)]) -> Vec<(u32, f32)> {
        let (inverse, region) = match self.sculpt.voxel.as_mut() {
            Some(v) => {
                // restore_delta marks the changed AABB dirty → re-mesh only those chunks.
                let inverse = v.grid.restore_delta(delta);
                let region = v.grid.take_dirty().map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                (inverse, region)
            }
            None => return Vec::new(),
        };
        if let Some(region) = region { self.voxel_mark_chunks_in_region(region); }
        inverse
    }

    /// Restore a voxel COLOUR delta (Fill All / vertex-paint) and re-mesh; returns the inverse delta (for
    /// redo). Mirrors `voxel_restore_delta` but on the parallel colour buffer — `restore_color_delta` marks
    /// only the changed AABB dirty so just those chunks re-mesh. RENDER, not SCENE: colour is a view-level
    /// surface attribute, not a structural change.
    pub(crate) fn voxel_restore_color_delta(&mut self, delta: &[(u32, u32)]) -> Vec<(u32, u32)> {
        let (inverse, region) = match self.sculpt.voxel.as_mut() {
            Some(v) => {
                let inverse = v.grid.restore_color_delta(delta);
                let region = v.grid.take_dirty().map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                (inverse, region)
            }
            None => return Vec::new(),
        };
        if let Some(region) = region { self.voxel_mark_chunks_in_region(region); }
        self.mark_dirty(DirtyFlags::RENDER); // live re-dispatch this frame (sculpt is GPU-authority)
        inverse
    }

    /// "Fill All" action: paint EVERY solid voxel of the active sculpt with the current BRUSH paint colour
    /// in ONE undoable step. The colour delta is registered on the sculpt colour-undo path (its OWN path —
    /// NOT `transaction()`), so Ctrl+Z reverts the whole fill at once. DENSE CPU path only; on a sparse
    /// (>256³) grid the per-voxel colour buffer isn't materialised yet (atlas colour scatter deferred), so
    /// the fill no-ops with a toast. Marks RENDER for the live re-mesh.
    pub(crate) fn voxel_fill_all_color(&mut self) {
        // Recolors density voxels + remeshes — would wipe un-voxelized surface-mode mesh edits.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some()) {
            self.toast("Surface edits pending — switch to Voxel mode first");
            return;
        }
        let color = match self.sculpt.voxel.as_ref() {
            Some(v) => core_math::voxel_sculpt::pack_voxel_color(v.paint_color),
            None => return,
        };
        // Sparse grids defer per-voxel colour → fill_all_color returns empty there; surface that to the user.
        if self.sculpt.voxel.as_ref().map_or(true, |v| v.grid.density.is_sparse()) {
            self.toast("Fill All: per-voxel paint is dense-only (>256\u{B3} deferred)");
            return;
        }
        let (delta, region) = match self.sculpt.voxel.as_mut() {
            Some(v) => {
                let delta = v.grid.fill_all_color(color);
                let region = v.grid.take_dirty().map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                (delta, region)
            }
            None => return,
        };
        if delta.is_empty() {
            self.toast("Fill All: nothing to paint (no solid voxels)");
            return;
        }
        if let Some(region) = region { self.voxel_mark_chunks_in_region(region); }
        // ONE colour-undo entry for the whole fill (sculpt undo is its own path). Redo re-arms on undo;
        // a NEW fill invalidates any pending redo, so clear the redo side here.
        let evicted = self.sculpt.voxel_color_undo_stack.push(delta);
        self.sculpt.voxel_undo_order.push_back(crate::UndoKind::CpuColor);
        prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, crate::UndoKind::CpuColor, evicted);
        self.sculpt.voxel_color_redo_stack.clear();
        self.sculpt.voxel_redo_order.retain(|&k| k != crate::UndoKind::CpuColor);
        self.mark_dirty(DirtyFlags::RENDER); // live re-dispatch this frame
        self.toast("Fill All: painted every solid voxel");
    }

    /// "Smooth All" action: run `iters` global Laplacian relax passes over the ENTIRE active sculpt (a
    /// global density smooth — the Smooth brush applied everywhere at once). Recorded as ONE step on the
    /// sculpt density-undo path (its OWN path — NOT `transaction()`), so Ctrl+Z reverts the whole smooth.
    /// Marks RENDER + re-meshes the whole surface live. Reuses `VoxelGrid::smooth_all` (the shared Smooth
    /// stencil) — no new smoothing math here.
    pub(crate) fn voxel_smooth_all(&mut self, iters: u32) {
        if self.sculpt.voxel.is_none() { return; }
        // Reads+writes density and remeshes — would wipe un-voxelized surface-mode mesh edits.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some()) {
            self.toast("Surface edits pending — switch to Voxel mode first");
            return;
        }
        // An ASYNC Move bake writes the grid on land — BLOCK-land it first (no-op when none).
        self.voxel_land_move_bake(true);
        // The smooth reads + writes the CPU grid, so it must be authoritative first. After an atlas
        // (sparse) stroke the CPU grid is BEHIND the GPU pool — materialise it (no-op for a dense grid,
        // or when not stale). One blocking surface-arena readback; this is a deliberate one-shot button.
        self.materialize_cpu_from_atlas();
        // Reset the stroke recorder's dedup mask to THIS grid, then run the smooth recording each changed
        // voxel's pre-smooth value (first-touch dedup) so the whole op is ONE reversible delta.
        let res = self.sculpt.voxel.as_ref().map_or(0, |v| v.grid.res);
        if res == 0 { return; }
        self.sculpt.voxel_stroke.begin(res);
        const SMOOTH_ALL_AMOUNT: f32 = 0.5; // per-pass blend toward the 6-neighbour average (matches the mesh relax rate)
        let (changed, region) = match self.sculpt.voxel.as_mut() {
            Some(v) => {
                let rec = &mut self.sculpt.voxel_stroke; // disjoint borrow from v
                let changed = v.grid.smooth_all(iters, SMOOTH_ALL_AMOUNT, Some(rec));
                let region = v.grid.take_dirty().map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                (changed, region)
            }
            None => return,
        };
        if changed == 0 {
            self.sculpt.voxel_stroke.clear();
            self.toast("Smooth All: nothing to smooth");
            return;
        }
        // Re-mesh the chunks the smooth touched (the whole grid region) — the CPU grid is now authoritative,
        // so the chunked re-mesh picks up the smoothed field this frame (budgeted drain, Frame Budget Rule).
        if let Some(region) = region { self.voxel_mark_chunks_in_region(region); }
        // ONE density-undo entry for the whole smooth (sculpt undo is its OWN path — NOT transaction()).
        // A new op truncates redo. Same Cpu undo lane the box/dense brush release uses.
        let delta = self.sculpt.voxel_stroke.take();
        let evicted = self.sculpt.voxel_undo_stack.push(delta);
        self.sculpt.voxel_undo_order.push_back(crate::UndoKind::Cpu);
        prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, crate::UndoKind::Cpu, evicted);
        self.sculpt.voxel_redo_stack.clear();
        self.sculpt.voxel_redo_order.clear();
        self.mark_dirty(DirtyFlags::RENDER); // live re-dispatch this frame
        self.toast("Smooth All");
    }

    /// One-shot SYMMETRIZE across a local axis (0=X, 1=Y, 2=Z): mirror the +axis half of the sculpt
    /// onto the −axis half (`positive_to_negative = true`), making the model symmetric NOW. Distinct
    /// from the live per-stroke Symmetry toggles. ONE sculpt-undo step (its OWN density-undo path, NOT
    /// `transaction()`) — Ctrl+Z reverts the whole mirror. Reuses `VoxelGrid::symmetrize`; re-meshes the
    /// touched region live. Density only (paint colours are not mirrored yet — see `VoxelGrid::symmetrize`).
    pub(crate) fn voxel_symmetrize(&mut self, axis: usize, positive_to_negative: bool) {
        if self.sculpt.voxel.is_none() || axis > 2 { return; }
        // Reads+writes density and remeshes — would wipe un-voxelized surface-mode mesh edits.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some()) {
            self.toast("Surface edits pending — switch to Voxel mode first");
            return;
        }
        // An ASYNC Move bake writes the grid on land — BLOCK-land it first (no-op when none).
        self.voxel_land_move_bake(true);
        // Reads + writes the CPU grid, so it must be authoritative first (materialise after a sparse
        // atlas stroke; no-op for a dense grid / when not stale). One blocking readback — a one-shot button.
        self.materialize_cpu_from_atlas();
        let res = self.sculpt.voxel.as_ref().map_or(0, |v| v.grid.res);
        if res == 0 { return; }
        self.sculpt.voxel_stroke.begin(res);
        let (changed, region) = match self.sculpt.voxel.as_mut() {
            Some(v) => {
                let rec = &mut self.sculpt.voxel_stroke; // disjoint borrow from v
                let changed = v.grid.symmetrize(axis, positive_to_negative, Some(rec));
                let region = v.grid.take_dirty().map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                (changed, region)
            }
            None => return,
        };
        if changed == 0 {
            self.sculpt.voxel_stroke.clear();
            self.toast("Symmetrize: already symmetric");
            return;
        }
        // Re-mesh the region the mirror touched (CPU grid is now authoritative — Frame Budget Rule drain).
        if let Some(region) = region { self.voxel_mark_chunks_in_region(region); }
        // ONE density-undo entry for the whole mirror (sculpt undo is its OWN path — NOT transaction()).
        let delta = self.sculpt.voxel_stroke.take();
        let evicted = self.sculpt.voxel_undo_stack.push(delta);
        self.sculpt.voxel_undo_order.push_back(crate::UndoKind::Cpu);
        prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, crate::UndoKind::Cpu, evicted);
        self.sculpt.voxel_redo_stack.clear();
        self.sculpt.voxel_redo_order.clear();
        self.mark_dirty(DirtyFlags::RENDER); // live re-dispatch this frame
        let msg = match axis { 0 => "Symmetrize X (shape only)", 1 => "Symmetrize Y (shape only)", _ => "Symmetrize Z (shape only)" };
        self.toast(msg);
    }
}
