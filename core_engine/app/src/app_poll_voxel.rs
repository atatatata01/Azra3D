// core_engine/app/src/app_poll_voxel.rs
//
// Slice (app_poll de-god, PURE RELOCATION) — byte-for-byte method bodies moved out of app_poll.rs;
// no logic change. poll_background_tasks (orchestrator) stays in app_poll.rs and calls these.

use crate::*;

impl AzraApp {
    /// Phase A.2 (GPU brick-snapshot undo): apply a queued atlas undo/redo restore. Three steps:
    /// (a) GPU: copy each captured brick's snapshot POOL slot back into the live POOL at its (stable)
    ///     handle — but ONLY for bricks whose restored tag is an allocated handle. Bricks restored to an
    ///     implicit EMPTY/FULL tag are read from the INDEX alone, so their pool slot is a don't-care.
    /// (b) CPU index: `restore_atlas_undo` sets the INDEX tags back + marks the INDEX dirty (so the next
    ///     `voxel_sync_mesh_atlas` re-uploads ONLY the index — the POOL is NOT re-uploaded, so the GPU
    ///     pool restore from (a) survives).
    /// (c) re-mesh: mark the entry's affected chunks dirty.
    /// The CPU grid stays `cpu_grid_stale` (the atlas changed; the lazy materialise picks it up later).
    /// All GPU work is GPU→GPU copy — NO readback.
    pub(crate) fn voxel_apply_atlas_restore(&mut self) {
        // Drain ALL queued atlas restores this poll. A single Option slot SILENTLY DROPPED a restore when
        // two AppCommand::Undo landed in one frame (macro replay / command palette enqueue several at once;
        // process_commands drains them with no intervening poll), leaving the surface desynced from the
        // undo stack. The VecDeque + full drain removes that class. GPU buffer mutation still happens only
        // in poll (Frame Ordering Contract). MAX_PER_POLL is a backstop — the queue is normally tiny
        // (bounded by undos-per-frame) and poll runs every frame, so leftovers apply next poll.
        const MAX_PER_POLL: usize = 64;
        let mut applied = 0usize;
        while applied < MAX_PER_POLL {
            let Some(entry) = self.sculpt.pending_atlas_restore.pop_front() else { break; };
            self.apply_one_atlas_restore(entry);
            applied += 1;
        }
    }

    /// Apply ONE queued atlas-undo restore: GPU POOL copy-back (GPU→GPU, no readback) + CPU INDEX restore
    /// + mark the affected chunks for re-mesh. Drained by `voxel_apply_atlas_restore`.
    pub(crate) fn apply_one_atlas_restore(&mut self, entry: crate::voxel_gpu_mesh::AtlasUndoEntry) {
        use core_math::voxel_sculpt::DensityStorage;

        // (a) GPU POOL restore (one encoder, GPU→GPU copies for allocated-tag bricks).
        {
            let Some(gpu) = self.gpu.as_ref() else { return; };
            let Some(atlas) = gpu.voxel_mesh_atlas.as_ref() else { return; };
            let slot = crate::voxel_gpu_mesh::BRICK_SLOT_BYTES;
            let mut enc = gpu.device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor { label: Some("atlas_undo_restore") });
            let mut copies = 0u32;
            for i in 0..entry.brick_indices.len() {
                if DensityStorage::tag_is_allocated(entry.tags[i]) {
                    enc.copy_buffer_to_buffer(&entry.snapshot_buf, i as u64 * slot, &atlas.pool_buf, entry.handles[i] as u64 * slot, slot);
                    copies += 1;
                }
            }
            if copies > 0 { gpu.queue.submit(std::iter::once(enc.finish())); }
            // (Encoder with zero copies is just dropped — nothing to submit.)
        }

        // (b) CPU INDEX restore + (c) mark affected chunks for re-mesh.
        let restore: Vec<(u32, u32)> = entry.brick_indices.iter().copied()
            .zip(entry.tags.iter().copied()).collect();
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.grid.density.restore_atlas_undo(&restore);
            let chunk_dim = v.chunk_dim;
            for &ci in &entry.affected_chunks {
                if (ci as usize) < v.chunks.len().max((chunk_dim * chunk_dim * chunk_dim) as usize) {
                    v.dirty_chunks.insert(ci);
                }
            }
            // The atlas now differs from the CPU grid → keep it flagged so a later resample/save/
            // duplicate triggers the one-shot lazy materialise.
            v.cpu_grid_stale = true;
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }
}
