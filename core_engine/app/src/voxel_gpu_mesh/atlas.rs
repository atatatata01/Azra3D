// Persistent GPU brick atlas + atlas-path GPU undo (relocated from voxel_gpu_mesh god-file).
// Generation-counter comments travel with their owning code.

use eframe::wgpu;

use super::MeshDensitySource;

/// Persistent GPU mirror of a Sparse grid's brick atlas (INDEX tags + POOL arena), kept current
/// per-brick from the CPU dirty-tracking (Phase 4b-2). Lives in `GpuCtx` across frames; the LIVE mesh
/// dispatch reads it for Sparse grids instead of a res³ dense buffer — the path to 1024³ (a 1024³ dense
/// buffer is 4 GB; the atlas is ~the surface shell). Synced just before each dispatch by
/// `FourthLayerApp::voxel_sync_mesh_atlas`: a full upload on (re)create / pool-grow / gc, else the
/// changed INDEX range + each dirty brick's POOL block. `pool_cap` (bricks) may exceed the live arena
/// (growth headroom), so a few new bricks per stroke don't reallocate the buffer.
pub(crate) struct VoxelAtlasGpu {
    pub(crate) index_buf: wgpu::Buffer, // brick_res³ u32 tags (EMPTY/FULL/handle)
    pub(crate) pool_buf: wgpu::Buffer,  // pool_cap × BRICK_VOX f32 arena
    pub(crate) pool_cap: u32,           // capacity in BRICKS (buffer holds pool_cap·4096 f32)
    pub(crate) brick_res: u32,          // bricks per axis (packed into SnParams.misc.w)
    pub(crate) res: u32,                // grid res this atlas mirrors (swap detection)
}

impl VoxelAtlasGpu {
    /// Build the mesh density source pointing at this atlas's persistent buffers.
    pub(crate) fn source(&self) -> MeshDensitySource<'_> {
        MeshDensitySource::Atlas { index: &self.index_buf, pool: &self.pool_buf, brick_res: self.brick_res }
    }
}

// ---------------------------------------------------------------------------------------------
// GPU BRICK-SNAPSHOT UNDO (Phase A.2) — atlas-path undo with NO GPU→CPU readback on stroke OR undo.
//
// During an atlas stroke the brush writes the GPU POOL in place; the CPU grid stays pre-stroke. The
// old undo path read the whole edited box back (the pen-up freeze). Instead we snapshot, ON THE GPU,
// each touched brick's POOL slot BEFORE the stroke's first scatter overwrites it (a GPU→GPU copy into
// a private snapshot buffer). Undo restores those slots (GPU→GPU copy back) + the pre-stroke INDEX
// tags (CPU). Redo snapshots the CURRENT slots first, then restores the undo entry. No readback ever.
// ---------------------------------------------------------------------------------------------
/// Bytes per brick POOL slot (BRICK_VOX f32 = 4096 × 4). Matches `core_math` `BRICK_VOX`.
pub(crate) const BRICK_SLOT_BYTES: u64 = 4096 * 4;

/// One stroke's GPU brick snapshot for atlas-path undo. Slot `i` ↔ `brick_indices[i]` / `tags[i]` /
/// `handles[i]`; `snapshot_buf` holds slot `i`'s 4096 floats at byte offset `i * BRICK_SLOT_BYTES`.
/// `tags` = the INDEX tag to restore (pre-stroke for an undo entry, current-state for a redo entry).
/// `handles` = the (stable — never compacted) POOL handle the snapshot is copied INTO on restore.
pub(crate) struct AtlasUndoEntry {
    pub(crate) brick_indices: Vec<u32>,
    pub(crate) tags: Vec<u32>,
    pub(crate) handles: Vec<u32>,
    pub(crate) snapshot_buf: wgpu::Buffer,
    pub(crate) affected_chunks: Vec<u32>,
    pub(crate) bytes: u64,
}

/// Bounded undo/redo ring of `AtlasUndoEntry`s, evicting oldest when the total snapshot bytes exceed
/// the cap (but never below one entry, so a single huge stroke is never silently lost). Mirrors the
/// CPU `VoxelUndoStack` budget discipline, but in GPU bytes.
#[derive(Default)]
pub(crate) struct AtlasUndoRing {
    pub(crate) undo: std::collections::VecDeque<AtlasUndoEntry>,
    pub(crate) redo: std::collections::VecDeque<AtlasUndoEntry>,
    pub(crate) total_bytes: u64,
}

impl AtlasUndoRing {
    const MAX_BYTES: u64 = 256 * 1024 * 1024; // 256 MB of GPU snapshot buffers

    /// Push a stroke's snapshot entry; returns the number of OLDEST undo entries evicted to stay within
    /// the byte budget. The caller MUST prune that many front `UndoKind::Atlas` markers from
    /// `voxel_undo_order` so the shared order stream can't outlive its backing entries (finding #4).
    pub(crate) fn push_undo(&mut self, e: AtlasUndoEntry) -> usize {
        self.clear_redo();
        self.total_bytes += e.bytes;
        self.undo.push_back(e);
        let mut evicted = 0usize;
        while self.total_bytes > Self::MAX_BYTES && self.undo.len() > 1 {
            if let Some(old) = self.undo.pop_front() { self.total_bytes -= old.bytes; evicted += 1; } else { break; }
        }
        evicted
    }
    pub(crate) fn pop_undo(&mut self) -> Option<AtlasUndoEntry> {
        let e = self.undo.pop_back();
        if let Some(ref e) = e { self.total_bytes -= e.bytes; }
        e
    }
    pub(crate) fn push_redo(&mut self, e: AtlasUndoEntry) {
        self.total_bytes += e.bytes;
        self.redo.push_back(e);
    }
    /// Push onto the UNDO stack WITHOUT clearing redo — used when a redo re-arms its inverse undo entry
    /// (the remaining redo branch must survive). `push_undo` (the stroke-commit path) clears redo instead.
    pub(crate) fn push_undo_keep_redo(&mut self, e: AtlasUndoEntry) {
        self.total_bytes += e.bytes;
        self.undo.push_back(e);
    }
    pub(crate) fn pop_redo(&mut self) -> Option<AtlasUndoEntry> {
        let e = self.redo.pop_back();
        if let Some(ref e) = e { self.total_bytes -= e.bytes; }
        e
    }
    pub(crate) fn clear_redo(&mut self) {
        for e in self.redo.drain(..) { self.total_bytes -= e.bytes; }
    }
    /// True while ANY undo or redo entry is live. Box-path `compact_aabb` MUST be skipped while this
    /// holds: compaction frees POOL handles, but a live `AtlasUndoEntry` references stable handles, so a
    /// later atlas Draw could recycle a freed handle for a DIFFERENT brick and an undo would then restore
    /// the old snapshot into the wrong (now-live) pool slot — silent GPU-pool corruption. See the compact
    /// gate in `input_sculpt.rs` stroke release.
    pub(crate) fn has_entries(&self) -> bool { !self.undo.is_empty() || !self.redo.is_empty() }
}

/// Accumulates one stroke's brick snapshots across its frames. On each atlas dispatch, the caller
/// passes the frame's `materialize_box_report` (`(bi, prev_tag, handle)`); the builder snapshots each
/// brick's pre-scatter POOL slot the FIRST time it sees that brick (first-touch dedup via `captured`).
/// Finished on pen-up into an `AtlasUndoEntry`.
#[derive(Default)]
pub(crate) struct AtlasUndoBuilder {
    captured: std::collections::HashMap<u32, u32>, // brick index → slot in this stroke's snapshot
    brick_indices: Vec<u32>,
    tags: Vec<u32>,
    handles: Vec<u32>,
    snap: Option<wgpu::Buffer>,
    snap_cap_bricks: u32, // current capacity of `snap` in brick slots
}

impl AtlasUndoBuilder {
    /// Exact sparse brick set already protected by this stroke's first-touch snapshots. Release uses
    /// it to build an affected-chunk list without expanding a diagonal stroke to its global AABB.
    pub(crate) fn captured_brick_indices(&self) -> &[u32] {
        &self.brick_indices
    }

    /// Snapshot every NOT-yet-captured brick in `report` (`(bi, prev_tag, handle)`): record its
    /// pre-stroke tag + stable handle, grow the snapshot buffer if needed (preserving already-captured
    /// slots), and GPU→GPU copy the brick's CURRENT POOL slot into the snapshot. This runs AFTER the
    /// atlas sync wrote each freshly-materialised brick's implicit fill, but BEFORE the brush scatter, so
    /// the snapshot is the genuine pre-stroke state. All of one frame's copies go in ONE encoder + submit.
    pub(crate) fn capture(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue,
        pool_buf: &wgpu::Buffer, report: &[(u32, u32, u32)],
    ) {
        // New bricks this frame (first touch). Already-captured bricks were snapshotted on an earlier
        // frame in their pre-state — re-copying would overwrite it with mid-stroke data, so skip them.
        let news: Vec<(u32, u32, u32)> = report.iter().copied()
            .filter(|&(bi, _, _)| !self.captured.contains_key(&bi)).collect();
        if news.is_empty() { return; }

        let needed = (self.brick_indices.len() + news.len()) as u32;
        self.ensure_capacity(device, queue, needed);
        let snap = self.snap.as_ref().unwrap();

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("atlas_undo_capture") });
        for (bi, prev_tag, handle) in news {
            let slot = self.brick_indices.len() as u32;
            self.captured.insert(bi, slot);
            self.brick_indices.push(bi);
            self.tags.push(prev_tag);
            self.handles.push(handle);
            enc.copy_buffer_to_buffer(pool_buf, handle as u64 * BRICK_SLOT_BYTES, snap, slot as u64 * BRICK_SLOT_BYTES, BRICK_SLOT_BYTES);
        }
        queue.submit(std::iter::once(enc.finish()));
    }

    /// Grow `snap` to hold at least `need_bricks` slots (1.5×, floor 8), preserving the already-captured
    /// slots via a GPU→GPU copy into the new buffer (submitted on `queue` so it precedes this frame's
    /// capture copies — same queue, ordered). No-op when the current buffer is large enough.
    fn ensure_capacity(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, need_bricks: u32) {
        if self.snap.is_some() && self.snap_cap_bricks >= need_bricks { return; }
        let new_cap = (need_bricks + need_bricks / 2).max(8);
        let new_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas_undo_snapshot"),
            size: new_cap as u64 * BRICK_SLOT_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if let Some(old) = self.snap.take() {
            let copied = self.brick_indices.len() as u64 * BRICK_SLOT_BYTES;
            if copied > 0 {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("atlas_undo_grow") });
                enc.copy_buffer_to_buffer(&old, 0, &new_buf, 0, copied);
                queue.submit(std::iter::once(enc.finish()));
            }
        }
        self.snap = Some(new_buf);
        self.snap_cap_bricks = new_cap;
    }

    /// Finish the stroke: build the `AtlasUndoEntry`, or `None` if no brick was captured (a genuinely
    /// no-op stroke — nothing was materialised/touched). `affected_chunks` = the chunk linear indices the
    /// stroke touched (for re-mesh on restore).
    pub(crate) fn finish(self, affected_chunks: Vec<u32>) -> Option<AtlasUndoEntry> {
        let snap = self.snap?;
        if self.brick_indices.is_empty() { return None; }
        let bytes = self.brick_indices.len() as u64 * BRICK_SLOT_BYTES;
        Some(AtlasUndoEntry {
            brick_indices: self.brick_indices,
            tags: self.tags,
            handles: self.handles,
            snapshot_buf: snap,
            affected_chunks,
            bytes,
        })
    }
}

/// Snapshot the CURRENT POOL slots + current INDEX tags for a set of `(bi, handle)` bricks into a fresh
/// GPU buffer (GPU→GPU copy). Builds the OPPOSITE entry during undo/redo: on undo we snapshot the
/// current (post-stroke) state to make the redo entry; on redo we snapshot the current (post-undo) state
/// to make the undo entry. `get_tag(bi)` reads each brick's live INDEX tag (from the CPU grid).
pub(crate) fn snapshot_current(
    device: &wgpu::Device, queue: &wgpu::Queue, pool_buf: &wgpu::Buffer,
    bricks: &[(u32, u32)], affected_chunks: Vec<u32>, get_tag: impl Fn(u32) -> u32,
) -> AtlasUndoEntry {
    let n = bricks.len() as u64;
    let size = (n * BRICK_SLOT_BYTES).max(BRICK_SLOT_BYTES);
    let snap = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("atlas_undo_snapshot_current"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut brick_indices = Vec::with_capacity(bricks.len());
    let mut tags = Vec::with_capacity(bricks.len());
    let mut handles = Vec::with_capacity(bricks.len());
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("atlas_undo_snap_cur") });
    for (slot, &(bi, handle)) in bricks.iter().enumerate() {
        brick_indices.push(bi);
        tags.push(get_tag(bi));
        handles.push(handle);
        enc.copy_buffer_to_buffer(pool_buf, handle as u64 * BRICK_SLOT_BYTES, &snap, slot as u64 * BRICK_SLOT_BYTES, BRICK_SLOT_BYTES);
    }
    queue.submit(std::iter::once(enc.finish()));
    AtlasUndoEntry { brick_indices, tags, handles, snapshot_buf: snap, affected_chunks, bytes: n * BRICK_SLOT_BYTES }
}
