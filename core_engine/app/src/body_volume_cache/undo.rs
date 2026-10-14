//! Smart cache invalidation for undo/redo.

use std::collections::{HashMap, HashSet};
use std::time::Instant;
use core_math::scene_graph::{SceneGraph, NodeId};
use super::{BodyVolumeCache, RamCacheEntry};
use crate::bake_utils::*;

/// Bump the per-body generation counter for every body that could have a bake in flight
/// at undo/redo time: every body with an existing generation entry (previously dispatched)
/// PLUS every body present in the restored graph (covers a body whose only in-flight bake
/// is its first one). Any pre-undo bake carries the OLD generation and so becomes stale
/// against the new current generation → rejected on landing by the `generation < current_gen`
/// guard in `accept_gpu_result` / `poll`. This does not discard any retained grid; it only
/// gates incoming stale results, so it cannot break the normal (post-undo) bake path, which
/// dispatches with a fresh, higher generation.
fn bump_generations_for_undo(generations: &mut HashMap<NodeId, u64>, sg: &SceneGraph) {
    for nid in collect_all_body_nids(sg) {
        generations.entry(nid).or_insert(0);
    }
    for gen in generations.values_mut() {
        *gen += 1;
    }
}

impl BodyVolumeCache {
    /// Smart cache invalidation for undo/redo.
    ///
    /// Compares each body's SDF source hash (body-local prims + mirror/bevel)
    /// against the cached grid's hash.  Bodies whose SDF data is unchanged
    /// (e.g. transform-only undo of origin/rotation/scale) keep their cached
    /// grids and skip the expensive rebake.  Bodies with changed SDF data are
    /// evicted to RAM cache and marked dirty for rebake.  Structural changes
    /// (body added/removed) trigger full invalidation as a safety fallback.
    pub(crate) fn invalidate_for_undo(&mut self, sg: &SceneGraph) {
        self.invalidation_epoch += 1;

        // Generation guard for undo/redo (closes the transform-safe stale-bake gap).
        //
        // `invalidate_for_undo` clears `self.in_flight`, but that tracker is bookkeeping —
        // it does NOT cancel the actual background/worker GPU bake job already running.
        // Its result still arrives at `accept_gpu_result` / `poll`, where the ONLY guard
        // is `result.generation < current_gen`.
        //
        // Narrow corruption path: edit a body (hash H0→H1, dispatch bumps gen G→G+1, bake
        // in flight) then UNDO back to H0 BEFORE the bake lands. The cached grid still holds
        // H0 (the in-flight H1 result never landed), so `new_hash == cached_hash` → the body
        // is marked transform_safe, NOT dirty, and is NEVER re-dispatched. The late gen-(G+1)
        // bake then lands, passes `G+1 < G+1` == false (not stale by generation), and stamps
        // the UNDONE H1 geometry into a body that never rebakes → wrong cached field.
        //
        // Bumping every body's generation here makes that in-flight pre-undo bake (gen G+1)
        // stale against the new current gen (G+2) → it is rejected on landing. The retained
        // cached grid is untouched; only incoming stale results are gated, so transform_safe
        // bodies keep their correct grid and the normal (post-undo) dispatch — which mints a
        // fresh higher generation — is unaffected.
        bump_generations_for_undo(&mut self.generations, sg);

        // Collect the set of body NodeIds in the restored scene graph.
        let new_nids: HashSet<NodeId> = collect_all_body_nids(sg).into_iter().collect();

        // Structural add/remove handled SELECTIVELY (fix for the owner's multi-body log):
        // the old whole-cache nuke on ANY body add/remove forced a full re-bake of EVERY
        // body per structural undo/redo. Dense bodies restored cheaply from the RAM cache,
        // but a brick-resident (sparse) body's empty-grid entry can NEVER be RAM-cached
        // (contract C4), so each structural event cost it a 40-60ms full re-classify — and
        // during a suppressed-dispatch episode (a slider drag) the missing entry could not
        // heal, producing the endless `no_grid_in_mark_prim_dirty` fallback spam.
        //
        // Soundness of keeping survivors: a body present in BOTH the cache and the restored
        // graph whose source hash still matches provably has the SAME field (the hash covers
        // the recursive prim set + mirror/bevel — the identical trust the dispatch dedup and
        // the non-structural arm below already rely on). Prim add/remove INSIDE a surviving
        // body changes its hash → caught by the per-body compare. Removed bodies drop their
        // entry + per-body state; added bodies have no entry and take the normal first-bake
        // cascade.
        let removed: Vec<NodeId> = self.body_grids.keys()
            .copied().filter(|n| !new_nids.contains(n)).collect();
        for nid in removed {
            self.body_grids.remove(&nid);
            self.body_chunked_meshes.remove(&nid);
            self.dirty_mesh_region.remove(&nid);
            self.dirty_regions.remove(&nid);
            self.dirty_bodies.remove(&nid);
            self.transform_safe_bodies.remove(&nid);
            self.rigid_accum.remove(&nid);
            self.sparse_scrolled.remove(&nid);
            self.cancel_cascade(nid);
            self.brick_pool.deallocate_body(nid);
            self.body_brick_volumes.remove(&nid);
        }
        for nid in new_nids.iter().filter(|n| !self.body_grids.contains_key(n)) {
            self.dirty_bodies.insert(*nid); // added body — normal first-bake path
        }

        // Surviving cached bodies (post-removal).
        let cached_nids: Vec<NodeId> = self.body_grids.keys().copied().collect();

        // Per-body comparison: compute new source hash and compare against cached hash.
        // Bodies whose hash matches keep their cached grid (transform-only change).
        let mut invalidated_nids: Vec<NodeId> = Vec::new();
        for &nid in &cached_nids {
            let new_hash = body_content_id(sg, nid, self.lab_operand_gen).map(|id| id.0);
            let cached_hash = self.body_grids.get(&nid).map(|e| e.source_hash);
            if new_hash == cached_hash {
                // SDF data unchanged — grid is still valid. Mark as transform-safe:
                // the body may need re-serialization (world-space positions changed)
                // but the volume grid itself is correct.
                self.transform_safe_bodies.insert(nid);
            } else {
                invalidated_nids.push(nid);
            }
        }

        // If no bodies were invalidated, we're done — skip the expensive clearing.
        if invalidated_nids.is_empty() {
            // Still cancel stale in-flight bakes and clear draft state, since
            // the undo may have changed transform data that affects serialization.
            self.in_flight.clear();
            self.bake_start_times.clear();
            self.pending_retighten.clear();
        self.cpu_grid_stale.clear();
        self.cpu_heal_queue.clear();
            self.cascades.clear();
            self.draft_resolution = None;
            self.draft_baked_bodies.clear();
            self.last_dirty_time = Some(Instant::now());
            return;
        }

        // Evict only the invalidated bodies' grids to RAM cache, then mark dirty.
        for &nid in &invalidated_nids {
            if let Some(entry) = self.body_grids.remove(&nid) {
                // S2.4 contract C4: never RAM-cache an EMPTY grid (SparseBricks / TextureOnly
                // entry) — a restored empty entry passes the RAM-hit checks without its
                // brick_resident marker and poisons the body (skips its bake with nothing to mesh).
                if self.ram_cache_enabled && !entry.grid.is_empty() {
                    let grid_bytes = entry.grid.len() * 4;
                    // Evict oldest RAM entries if over budget
                    while self.ram_cache_used_bytes + grid_bytes > self.ram_cache_budget_bytes && !self.ram_cache.is_empty() {
                        let oldest_nid = self.ram_cache.iter()
                            .min_by_key(|(_, e)| e.inserted_at)
                            .map(|(nid, _)| *nid);
                        if let Some(old_nid) = oldest_nid {
                            if let Some(old) = self.ram_cache.remove(&old_nid) {
                                self.ram_cache_used_bytes = self.ram_cache_used_bytes.saturating_sub(old.grid.len() * 4);
                            }
                        } else { break; }
                    }
                    if self.ram_cache_used_bytes + grid_bytes <= self.ram_cache_budget_bytes {
                        self.ram_cache_used_bytes += grid_bytes;
                        self.ram_cache.insert(nid, RamCacheEntry {
                            grid: entry.grid,
                            source_hash: entry.source_hash,
                            grid_min: entry.grid_min,
                            grid_max: entry.grid_max,
                            bake_resolution: entry.bake_resolution,
                            invalidation_epoch: self.invalidation_epoch,
                            inserted_at: std::time::Instant::now(),
                        });
                    }
                }
            }
            self.body_chunked_meshes.remove(&nid);
            self.dirty_mesh_region.remove(&nid);
            self.dirty_regions.remove(&nid);
            self.dirty_bodies.insert(nid);
            self.transform_safe_bodies.remove(&nid);
            self.cancel_cascade(nid);
            // Release stale brick data — will be repopulated on rebake
            self.brick_pool.deallocate_body(nid);
            self.body_brick_volumes.remove(&nid);
        }

        // Cancel stale in-flight bakes and draft state
        self.in_flight.clear();
        self.bake_start_times.clear();
        self.pending_retighten.clear();
        self.cpu_grid_stale.clear();
        self.cpu_heal_queue.clear();
        self.primary_bake_requests.clear();
        self.draft_resolution = None;
        self.draft_baked_bodies.clear();
        self.last_dirty_time = Some(Instant::now());
    }


}
