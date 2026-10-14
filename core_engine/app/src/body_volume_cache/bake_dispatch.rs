//! Bake dispatch — trigger_bake, draft mode, debounce, draft promotion.

use std::time::Instant;
use glam::Vec3;
use core_math::scene_graph::{SceneGraph, NodeId, NodePayload};
use crate::gpu_jobs::{GpuJobDispatcher, GpuJob, GpuJobKind, DeviceTarget};
use crate::bake_utils::*;
use super::{BodyVolumeCache, BodyCacheEntry, BakeResolutionCascade, PrimaryBakeRequest, SparseBakeRequest, BODY_LOCAL_PRIMS};

/// Brick edge length (voxels) of the SparseBricks atlas — the `brick_dim` every sparse call in
/// this file passes (classifier, window dilation, frozen-index check). Matches the GPU contract:
/// `volume_bake.wgsl` mode 2 routes voxels with `bv / 8u`, `SdfBrickAtlasGpu::BRICK_FLOATS`
/// = 8³ = 512 f32 per pool brick.
pub(crate) const SPARSE_BRICK_DIM: u32 = 8;

impl BodyVolumeCache {
    /// True while this body is inside the oversized-window escalation cooldown (an escalation
    /// fired less than `ESCALATED_FULL_MIN_INTERVAL` ago). ONE definition for both the escalation
    /// branch and the top-of-loop fast path, so the two can never disagree about who defers.
    pub(crate) fn escalate_cooling(&self, nid: NodeId) -> bool {
        self.sparse_escalate_last.get(&nid)
            .is_some_and(|t| t.elapsed() < ESCALATED_FULL_MIN_INTERVAL)
    }

    /// Writer-provenance window (2026-07-16 idle escalate-loop diagnosis): true while this body
    /// is escalation-active — cooling OR memo-deferred. The owner's log showed an idle ~3/sec
    /// escalate loop whose driver is a REAL hash-changing prim edit landing every repaint (the
    /// cache is correct; some app-layer writer mutates a hashed field with the panel open). The
    /// writers' field-diff prints (`prim_actions.rs` mirror + transform provenance) widen their
    /// gates on this, so the NEXT repro names the exact prim uid + field + both values in the
    /// owner log instead of requiring another blind diagnostic pass.
    pub(crate) fn escalation_hot(&self, nid: NodeId) -> bool {
        self.escalate_cooling(nid) || self.sparse_escalate_deferred.contains(&nid)
    }

    /// Slice b (settle trim): true when this body has had NO oversized-window escalation within
    /// 2× the cooldown — i.e. no gesture-driven full-pipeline churn is in progress, so a mesh
    /// dispatch may safely shrink transient-peak capacity back toward the settled surface need.
    /// The poll caller feeds this into `SdfBodyMeshSlot.may_trim` (one-shot); the mesh layer
    /// itself stays timing-free and deterministic.
    pub(crate) fn sparse_quiet(&self, nid: NodeId) -> bool {
        self.sparse_escalate_last.get(&nid)
            .map_or(true, |t| t.elapsed() >= ESCALATED_FULL_MIN_INTERVAL * 2)
    }
}

/// Gesture-coalesced escalation: minimum interval between OVERSIZED-WINDOW escalated full
/// classifies per body. One full pipeline (classify + full bake + pool sizing + full re-mesh) at
/// 1024³ costs ~150-300ms wall with 20-70ms frame-thread spikes; during a continuous gesture each
/// landing is immediately stale, so an unbounded rate just burns those spikes back-to-back (the
/// 2026-07-16 owner log: 6 fulls in one scale gesture). 300ms ⇒ at most ~3 mid-gesture refreshes
/// per second; the settle full at gesture end is authoritative. Ticks inside the cooldown defer
/// exactly like the in-flight coalesce (never dropped).
const ESCALATED_FULL_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// P1-2048 (2026-07-17): frame-thread budget for the SYNCHRONOUS frozen-partial walk
/// (`sparse_window_allocate_incompatible` on the trigger thread). The escalation threshold used
/// to be FRACTIONAL only (window > 1/8 of the grid's bricks) but the walk's cost is ABSOLUTE —
/// at 2048³ a window can sit under the fraction yet cost 300ms+ on the frame thread (owner log
/// 2026-07-17: `[SparseWalk] classify=310-363ms` for a ~900³ window ≈ 1.6M bricks = 8.6% of the
/// grid). Any window whose ESTIMATED walk cost exceeds this budget routes to the SAME background
/// windowed-reclassify worker the fractional escalation uses. 8ms keeps the whole trigger inside
/// the 12ms Frame Budget (CLAUDE.md) with headroom for serialize/hash/dispatch.
pub(crate) const SPARSE_WALK_FRAME_BUDGET_MS: f64 = 8.0;

/// Measured walk cost per window brick, used to estimate the frame-thread cost above.
/// Basis (all ≈0.18-0.23 µs/brick — linear in window bricks across 3 independent measurements):
///   - owner 2048 log 2026-07-17: 310-363ms / ~1.6M-brick windows ≈ 0.19-0.23 µs/brick;
///   - owner 1024 log: 5-45ms for the same gesture's proportionally smaller windows;
///   - `w_measure_windowed_walk_1024` (#[ignore]d): 10.0ms / ~57k bricks ≈ 0.18 µs/brick.
/// 200ns/brick ⇒ the budget crosses at ~40k window bricks — an order of magnitude above real
/// 512³ drag windows (hundreds-thousands of bricks), so small-window latency is untouched.
pub(crate) const SPARSE_WALK_NS_PER_BRICK: f64 = 200.0;

/// Windowed escalated reclassify — the decision-site → push-site payload (trigger loop local).
/// Carries everything the background window-walk worker needs beyond the request itself.
struct SparseWindowedSpawn {
    /// The body's frozen index (Arc — cloned into a mutable copy ON the worker, off the frame
    /// thread; ~8MB at 1024³).
    frozen: std::sync::Arc<core_math::chunked_dc::SparseIndexOnly>,
    /// `entry.brick_resident` at trigger time — the Partial's `atlas_generation` identity.
    marker: u64,
    /// The dilated dirty window (voxels) the worker walks + the bake covers.
    dil_min: [u32; 3],
    dil_max: [u32; 3],
    /// A consumed `sparse_scrolled` flag — forces the index_update to ship even when the walk
    /// changes nothing (the GPU atlas still holds the pre-scroll index).
    scrolled_force: bool,
}

impl BodyVolumeCache {
    /// Debounce threshold: bake dispatch waits until at least this many ms
    /// since the last dirty mark. Prevents baking every single slider tick.
    const DEBOUNCE_MS: u64 = 40;

    /// Idle threshold (ms) before draft-baked bodies are promoted to full resolution.
    const DRAFT_PROMOTE_MS: u64 = 200;

    /// Enable or disable draft-resolution baking.
    ///
    /// When `active` is true, subsequent bakes use half the configured resolution
    /// (minimum 32) for fast feedback during slider drag.
    /// When `active` is false, all bodies that were baked at draft resolution are
    /// marked dirty for a full-resolution rebake.
    pub(crate) fn set_draft_mode(&mut self, active: bool) {
        if active {
            let draft = (self.resolution / 2).max(32);
            self.draft_resolution = Some(draft);
        } else {
            self.draft_resolution = None;
            // Promote: mark all draft-baked bodies dirty for full-res rebake
            for body_nid in self.draft_baked_bodies.drain() {
                self.dirty_bodies.insert(body_nid);
            }
            // Clear debounce so the full-res bake dispatches immediately
            self.last_dirty_time = None;
        }
    }

    /// Returns true if draft mode is currently active.
    #[allow(dead_code)] // L4 progressive resolution
    pub(crate) fn is_draft_mode(&self) -> bool {
        self.draft_resolution.is_some()
    }

    /// Returns true if there are dirty bodies waiting for bake dispatch
    /// (e.g., debounced from a previous frame).
    pub(crate) fn has_pending_dirty(&self) -> bool {
        !self.dirty_bodies.is_empty()
    }

    /// B3b: volume-bake pipeline still needs update/poll ticks while the user is idle
    /// (dirty queue, bake in flight, classify queue, or a settle timer already due).
    /// Without this, event-driven idle freezes until mouse wiggle and readbacks never land.
    pub(crate) fn needs_idle_poll(&self) -> bool {
        if !self.dirty_bodies.is_empty() {
            return true;
        }
        if !self.in_flight.is_empty() {
            return true;
        }
        if !self.primary_bake_requests.is_empty() {
            return true;
        }
        if !self.pending_gpu_classify.is_empty() {
            return true;
        }
        if !self.cpu_heal_queue.is_empty() {
            return true;
        }
        // Settle timers past their window: must run this frame (not wait for input).
        if !self.pending_retighten.is_empty() && self.retighten_idle() {
            return true;
        }
        if self.draft_promote_due() {
            return true;
        }
        false
    }

    /// True when draft promotion would fire on the next `check_draft_promotion` call.
    fn draft_promote_due(&self) -> bool {
        if self.draft_resolution.is_none() || self.draft_baked_bodies.is_empty() {
            return false;
        }
        if !self.in_flight.is_empty() {
            return false;
        }
        match self.last_dirty_time {
            Some(t) => t.elapsed().as_millis() as u64 >= Self::DRAFT_PROMOTE_MS,
            None => true,
        }
    }

    /// B3b: soonest wake delay (ms) for settle timers that are armed but not yet due.
    /// Used with `request_repaint_after` so we do not spin 60fps for the whole debounce.
    pub(crate) fn next_settle_wake_ms(&self) -> Option<u64> {
        let mut soonest: Option<u64> = None;
        let consider = |soonest: &mut Option<u64>, rem_ms: u64| {
            let rem = rem_ms.max(1);
            *soonest = Some(soonest.map_or(rem, |a| a.min(rem)));
        };
        if !self.pending_retighten.is_empty() {
            if let Some(t) = self.last_dirty_time {
                let elapsed = t.elapsed().as_millis() as u64;
                if elapsed < Self::RETIGHTEN_IDLE_MS {
                    consider(&mut soonest, Self::RETIGHTEN_IDLE_MS - elapsed);
                }
            }
        }
        if self.draft_resolution.is_some() && !self.draft_baked_bodies.is_empty() {
            if let Some(t) = self.last_dirty_time {
                let elapsed = t.elapsed().as_millis() as u64;
                if elapsed < Self::DRAFT_PROMOTE_MS {
                    consider(&mut soonest, Self::DRAFT_PROMOTE_MS - elapsed);
                }
            }
        }
        soonest
    }

    /// Check if we should promote draft-baked bodies to full resolution.
    ///
    /// Called once per frame. When the user stops dragging (no dirty marks for
    /// DRAFT_PROMOTE_MS), exits draft mode and marks all draft-baked bodies dirty
    /// so the next `trigger_bake` dispatches at full resolution.
    /// Returns true if promotion happened (caller should trigger a bake).
    pub(crate) fn check_draft_promotion(&mut self) -> bool {
        if self.draft_resolution.is_none() { return false; }
        if self.draft_baked_bodies.is_empty() { return false; }
        // Only promote once all in-flight draft bakes have landed
        if !self.in_flight.is_empty() { return false; }

        let idle_enough = match self.last_dirty_time {
            Some(t) => t.elapsed().as_millis() as u64 >= Self::DRAFT_PROMOTE_MS,
            None => true, // No dirty time recorded — promote immediately
        };
        if !idle_enough { return false; }

        // Exit draft mode → marks draft bodies dirty for full-res rebake
        self.set_draft_mode(false);
        true
    }

    /// Idle window (ms) after the last edit before a partially-baked body is
    /// re-tightened. Longer than DEBOUNCE_MS so it only fires once interaction has
    /// clearly stopped — never mid-drag (last_dirty_time keeps bumping during a drag).
    const RETIGHTEN_IDLE_MS: u64 = 150;

    /// True when the settle window has elapsed since the last dirty mark (or none was
    /// ever set) — the same predicate `check_idle_retighten` gates on. Shared by the
    /// `[RetightenArm]` re-arm provenance diagnostics: an arm landing while IDLE is the
    /// feeder of a repeating settle→re-tighten loop (a drag-time arm is by design).
    pub(super) fn retighten_idle(&self) -> bool {
        self.last_dirty_time
            .map_or(true, |t| (t.elapsed().as_millis() as u64) >= Self::RETIGHTEN_IDLE_MS)
    }

    /// After interaction settles, promote every body that received a PARTIAL
    /// (interactive) bake to ONE full re-tighten bake. During a drag, partial bakes
    /// reuse the cached grid bounds + per-axis resolution to avoid voxel-mapping pops;
    /// an AABB-escape inflates those bounds while the voxel count stays fixed, so the
    /// body's effective resolution collapses — the surface goes coarse, and a freshly
    /// created prim that grows past its initial tiny grid never appears at all. The
    /// only thing that fixed it before was a resolution change (invalidate_all → full
    /// bake). This makes that full re-tighten happen automatically on settle: it
    /// recomputes tight bounds AND adaptive resolution (full-bake path, no dirty_regions).
    /// This is the "final high-quality bake on release" every interactive modeller does.
    ///
    /// Returns true if a bake was queued (caller then calls trigger_body_vol_bake).
    /// Keyed on the `pending_retighten` state set (not timing) for correctness; the
    /// idle window is only a settle debounce, not a correctness crutch.
    pub(crate) fn check_idle_retighten(&mut self, sg: &core_math::scene_graph::SceneGraph) -> bool {
        if self.pending_retighten.is_empty() { return false; }
        // Only after editing has clearly stopped (debounced on the last dirty mark).
        if !self.retighten_idle() { return false; }
        // Never re-tighten on top of an in-flight bake — wait for a clean state.
        if !self.in_flight.is_empty() { return false; }
        // Mid analytical drag (bakes suppressed) — wait for release.
        if self.suppress_bake_dispatch { return false; }
        let bodies: Vec<NodeId> = self.pending_retighten.drain().collect();
        let mut queued = false;
        let mut skipped = 0u32;
        // [RetightenArm] diagnostic: per-body QUEUE reason, printed with the settle line so a
        // repeating settle→re-tighten loop shows WHICH guard keeps failing (loose bounds /
        // clipped grid / stale CPU grid / missing bounds) — not just that it fired.
        let mut queued_why: Vec<String> = Vec::new();
        for nid in bodies {
            if !self.body_grids.contains_key(&nid) { continue; } // body gone
            // S2.4 contract C3: brick-resident (SparseBricks) bodies never re-tighten/heal. There
            // is no dense CPU grid to reclaim resolution for, and the heal readback has no dense
            // bake output to read — the sparse FULL bake already recomputed tight bounds at
            // dispatch (full bakes always do). Keyed on the brick_resident marker, NOT on grid
            // emptiness, so TextureOnly bodies keep their existing behavior.
            if self.body_grids.get(&nid).map_or(false, |e| e.brick_resident.is_some()) { continue; }
            // Skip the (full-res, ~hundreds-of-ms at 512³) re-tighten when the cached grid is already
            // tight enough. For a full-res partial-bake body the re-tighten's ONLY job is to reclaim
            // resolution from loose bounds; a translate / small nudge leaves the extent essentially
            // unchanged (the prim stays contained at full voxel density — re-tighten would only
            // re-center, producing an identical grid that dedups to a pure-overhead fallback mesh).
            // Only re-bake when the bounds have drifted meaningfully looser than tight (a real shrink).
            // Reference the FRESH tight bounds of the CURRENT geometry — NOT the cached
            // `partial_tight_bounds`, which was captured mid-escape/drag (already-inflated extent + 25%
            // pad). Comparing the ballooned grid against that stale, inflated reference made it look
            // "not too loose" against ITSELF, so the settle re-tighten was skipped and the aabb_escape
            // balloon persisted → the grid ran ~1.5–2.5× the true geometry → the DC under-sampled the
            // boxes into ~0.4% of the grid (CPU-measured) → the flat-slab/squish. Recompute tight from
            // the live scene so a ballooned grid is correctly seen as loose and gets shrunk.
            let fresh_padded_tight = {
                let prims = collect_body_prims_recursive(sg, nid);
                if prims.is_empty() { None } else {
                    let (mf, mc, bev) = sg.get(nid).map(|n| match &n.payload {
                        NodePayload::SdfBody { mirror_flags, mirror_center, bevel_radius, .. } =>
                            (*mirror_flags, *mirror_center, *bevel_radius),
                        _ => (0u8, glam::Vec3::ZERO, 0.0f32),
                    }).unwrap_or((0, glam::Vec3::ZERO, 0.0));
                    let (tmin, tmax) = compute_body_bake_bounds(&prims, mf, mc, bev);
                    Some(pad_grid_bounds(tmin, tmax))
                }
            };
            let mut why: Vec<&'static str> = Vec::new();
            if let (Some(&cached), Some(padded_tight)) = (self.body_bounds.get(&nid), fresh_padded_tight) {
                let cached_ext = cached.1 - cached.0;
                let tight_ext = (padded_tight.1 - padded_tight.0).max(glam::Vec3::splat(1e-4));
                const RETIGHTEN_SLACK: f32 = 0.15; // ≤15% looser per axis → keep; else re-tighten to fresh bounds
                let not_too_loose = cached_ext.x <= tight_ext.x * (1.0 + RETIGHTEN_SLACK)
                    && cached_ext.y <= tight_ext.y * (1.0 + RETIGHTEN_SLACK)
                    && cached_ext.z <= tight_ext.z * (1.0 + RETIGHTEN_SLACK);
                // Also require the grid to CONTAIN the fresh padded geometry (a too-SMALL grid clips flat).
                const COVER_EPS: f32 = 1e-3;
                let covers_geometry = cached.0.x <= padded_tight.0.x + COVER_EPS
                    && cached.0.y <= padded_tight.0.y + COVER_EPS
                    && cached.0.z <= padded_tight.0.z + COVER_EPS
                    && cached.1.x >= padded_tight.1.x - COVER_EPS
                    && cached.1.y >= padded_tight.1.y - COVER_EPS
                    && cached.1.z >= padded_tight.1.z - COVER_EPS;
                // Bounds tight + covering:
                //  • fresh CPU grid → skip (classic path)
                //  • cpu_stale only → also SKIP full re-bake (LIVE FIX 2026-07-21)
                //    Live: cpu_stale-only settle fired full 256³ SDF re-eval + MAP_READ
                //    (primary=11s). Display already uses field-direct rv.buffer; a full
                //    heal bake is pure overhead for interactive view. CPU grid stays
                //    marked stale for export/CPU-DC (they force their own refresh).
                if not_too_loose && covers_geometry {
                    skipped += 1;
                    let only_stale = self.cpu_grid_stale.contains(&nid);
                    crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "retighten", || format!(
                        "body={:?} SKIP (tight+covering, cpu_stale={}) cached={:.3?}..{:.3?}",
                        nid, only_stale, cached.0, cached.1));
                    continue;
                }
                if !not_too_loose { why.push("loose"); }
                if !covers_geometry { why.push("clipped"); }
                if self.cpu_grid_stale.contains(&nid) { why.push("cpu_stale"); }
            } else {
                why.push("no_bounds");
            }
            // [visrec] settle re-tighten decision (Part B #7): QUEUE full heal bake.
            // Only when bounds are actually wrong (loose/clipped) — needs full re-eval.
            crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "retighten", || format!(
                "body={:?} QUEUE full re-tighten (cpu_grid_stale={}) — bounds heal",
                nid, self.cpu_grid_stale.contains(&nid)));
            self.dirty_regions.remove(&nid); // force FULL bake: tight bounds + adaptive res
            self.dirty_bodies.insert(nid);
            // Heal mark: THIS settle bake takes the readback path (consumed at dispatch by
            // take_cpu_heal) → body_grids refreshes. All interactive bakes stay GPU-only.
            self.cpu_heal_queue.insert(nid);
            queued = true;
            queued_why.push(format!("{:?}[{}]", nid, why.join("+")));
        }
        if queued {
            self.last_dirty_time = None; // one-shot settle — bypass the 40ms debounce
            self.heal_trace_start = Some(Instant::now()); // open the [HealTrace] window
            eprintln!("[Retighten] settle → full re-tighten bake (restore resolution + bounds): {}",
                queued_why.join(", "));
        } else if skipped > 0 && (crate::perf_spam_on() || cfg!(debug_assertions)) {
            eprintln!("[Retighten] settle → {skipped} body(s) already tight, skipped redundant re-bake");
        }
        queued
    }

    /// Clear the bake-dispatch debounce so the NEXT trigger_bake dispatches immediately,
    /// bypassing the 40ms DEBOUNCE_MS window. For one-shot / structural events where
    /// waiting for "idle" is wrong because the edit is a single discrete action OR a
    /// region-less full-body edit that the debounce would otherwise stall every frame:
    /// scope-leave, prim add/delete, and the body-wide Edge Bevel slider (which marks the
    /// body fully dirty each changed frame, re-arming the debounce so the bake never fires
    /// mid-drag). Mirrors set_draft_mode(false) and check_idle_retighten, which also null
    /// last_dirty_time. NOTE: this does not touch suppress_bake_dispatch — in volume-only
    /// production (force_volume_only) suppress is never set during an episode, so clearing
    /// the debounce is sufficient for live dispatch.
    pub(crate) fn clear_bake_debounce(&mut self) {
        self.last_dirty_time = None;
    }

    /// Spawn background bake jobs for all dirty bodies.
    /// Only the dirty bodies are rebaked — clean bodies retain their cached grids.
    /// When a GPU dispatcher with enabled workers is available, routes bakes to
    /// worker GPUs. Falls back to rayon CPU bake otherwise.
    ///
    /// Returns a list of body NodeIds that were synchronously baked (Level 4
    /// progressive cascade coarse bake). These bodies have fresh grid data and
    /// need DC extraction dispatched immediately by the caller.
    pub(crate) fn trigger_bake(&mut self, sg: &SceneGraph, mut dispatcher: Option<&mut GpuJobDispatcher>) -> Vec<NodeId> {
        if self.dirty_bodies.is_empty() { return Vec::new(); }
        // During interactive drag, suppress dispatch — dirty regions accumulate
        // normally and one combined bake fires when the drag ends.
        if self.suppress_bake_dispatch { return Vec::new(); }
        // Soft-K / bevel edits change the feature floor — keep Detail LO min-res current.
        if self.use_detail_weighted_lo {
            self.refresh_detail_lo_feature_floors(sg);
        }
        let _t_trigger = std::time::Instant::now();
        let _dirty_count = self.dirty_bodies.len();

        // --- Debounce: skip dispatch if the last dirty mark was very recent,
        // unless ALL dirty bodies are NeverBaked (first bake must not be delayed).
        // EXCEPTION: skip debounce entirely when any dirty body has dirty_regions
        // (prim-level partial bake). Partial bakes over small sub-regions are cheap
        // enough to dispatch every frame for real-time cache updates during drag.
        let any_has_regions = self.dirty_bodies.iter()
            .any(|nid| self.dirty_regions.get(nid).map_or(false, |r| !r.is_empty()));
        if !any_has_regions {
            if let Some(t) = self.last_dirty_time {
                let elapsed_ms = t.elapsed().as_millis() as u64;
                if elapsed_ms < Self::DEBOUNCE_MS {
                    let all_first_bake = self.dirty_bodies.iter()
                        .all(|nid| !self.body_grids.contains_key(nid));
                    if !all_first_bake {
                        return Vec::new(); // Wait for more idle time before dispatching
                    }
                }
            }
        }

        // --- Auto-draft: if the last dirty mark was very recent, we're likely in a
        // rapid slider drag — use draft resolution for fast feedback.
        // Draft is only engaged when at least one dirty body has a prior cache entry
        // (i.e., this is an edit, not a first bake).
        // EXCEPTION: skip auto-draft when dirty_regions exist — these come from
        // mark_prim_dirty_at() (prim-level transform/edit). Partial bake at full
        // resolution over a small dirty sub-region (e.g. 20³) is far cheaper than
        // a full bake at draft resolution (64³). Draft mode would cause resolution
        // mismatch (64 vs cached 128) which kills the partial bake path entirely.
        let any_dirty_has_regions = self.dirty_bodies.iter()
            .any(|nid| self.dirty_regions.get(nid).map_or(false, |r| !r.is_empty()));
        if !any_dirty_has_regions {
            if let Some(t) = self.last_dirty_time {
                let elapsed_ms = t.elapsed().as_millis() as u64;
                let has_any_cached = self.dirty_bodies.iter()
                    .any(|nid| self.body_grids.contains_key(nid));
                // S2.5e — never engage the (global) draft flag when EVERY dirty body is
                // sparse-eligible: a sparse body ignores draft (it must stay ≥512, see the
                // per-body `sparse_body` guard), so setting it here would only leave a stale
                // global flag with no consumer. A mixed scene still drafts (the guard protects
                // the sparse body per-body; the dense bodies get the draft speedup).
                let all_dirty_sparse = self.dirty_bodies.iter()
                    .all(|nid| self.sparse_bake_eligible(self.effective_resolution(*nid)));
                if elapsed_ms < 200 && has_any_cached && !all_dirty_sparse
                    && self.draft_resolution.is_none()
                {
                    let draft = (self.resolution / 2).max(32);
                    self.draft_resolution = Some(draft);
                }
            }
        }

        let use_gpu = dispatcher.as_ref().map_or(false, |d| d.has_enabled_workers());
        let all_bodies = collect_all_body_nids(sg);
        // Track bodies that received a synchronous coarse bake (Level 4 cascade).
        let mut sync_baked = Vec::new();
        // Bodies skipped because a bake is already in flight — re-inserted into
        // dirty_bodies after the blanket clear so the next trigger_bake picks them up.
        let mut deferred_bodies = Vec::new();

        for body_nid in &all_bodies {
            let body_nid = *body_nid;
            if !self.dirty_bodies.contains(&body_nid) { continue; }

            // Display Invariant: a NESTED body (parent is another SdfBody) is SUBSUMED into its
            // top-level ancestor's subtree bake — `collect_body_prims_recursive(ancestor)` already
            // pulls its prims in. Baking it independently would produce a second standalone grid/mesh
            // for the SAME geometry (the "nest duplicates the body" render bug). Skip it; the ancestor's
            // bake covers it. Its dirtiness is redirected to the top-level ancestor at the mark site.
            if sg.parent_body(body_nid).is_some() { continue; }

            // Always-cache: active body is baked like all others. Partial bakes
            // (mark_prim_dirty_at) update only the dirty sub-region of the grid.

            // Coalesce: skip dispatch if any bake is already in flight for this body.
            // Dirty regions accumulate until the in-flight bake completes; the next
            // trigger_bake cycle dispatches one combined bake covering all pending regions.
            if self.in_flight.iter().any(|(bid, _)| *bid == body_nid) {
                if crate::perf_spam_on() {
                    eprintln!("[Coalesce] body={:?} skipped — bake in flight, {} pending regions",
                        body_nid, self.dirty_regions.get(&body_nid).map(|v| v.len()).unwrap_or(0));
                }
                deferred_bodies.push(body_nid);
                continue;
            }

            // Escalation-cooldown FAST PATH: this body already deferred an oversized-window
            // escalation and is still cooling, so the decision below is a foregone conclusion —
            // skip HERE, before collecting prims / hashing / arming pending_retighten. Doing that
            // work every frame only to defer at the escalation branch was ~30 wasted trigger walks
            // per cooldown (each printing "[RetightenArm] by=partial_dispatch while IDLE", whose
            // "phantom dirty regions?" premise a deferral legitimately breaks — the regions are
            // deferred, not phantom). Same deferral contract as the in-flight coalesce above:
            // regions stay intact, body re-armed via deferred_bodies.
            if self.sparse_escalate_deferred.contains(&body_nid) {
                if self.escalate_cooling(body_nid) {
                    deferred_bodies.push(body_nid);
                    continue;
                }
                self.sparse_escalate_deferred.remove(&body_nid); // cooldown elapsed — re-evaluate
            }

            // Collect all primitives for this top-level body (including nested children).
            // Mesh Terrain (L2): own prims + straddling neighbour prims; lattice-local frames.
            let is_terrain = crate::terrain::is_terrain_bake_body(sg, body_nid);
            let prims = if is_terrain {
                crate::terrain::collect_terrain_bake_prims(sg, body_nid)
            } else {
                collect_body_prims_recursive(sg, body_nid)
            };
            if prims.is_empty() { continue; }

            // Extract body-level mirror/bevel + origin for body-local bake
            let (mirror_flags, mirror_sign, mirror_center, bevel_radius, body_origin) = sg.get(body_nid)
                .and_then(|n| match &n.payload {
                    NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, bevel_radius, origin, .. } =>
                        Some((*mirror_flags, *mirror_sign, *mirror_center, *bevel_radius, *origin)),
                    _ => None,
                })
                .unwrap_or((0, 0, Vec3::ZERO, 0.0, Vec3::ZERO));
            // Bevel + Body Smooth surface margin for grid AABB (smooth can move the isosurface).
            let field_margin =
                crate::bake_utils::body_field_bounds_margin(sg, body_nid, bevel_radius);

            // Offset prims + mirror_center to body-local space for bake.
            // The resulting volume grid is in body-local coordinates.
            // MOVE, don't clone: under BODY_LOCAL_PRIMS (true in production) the prims are already
            // body-local, so this was a full deep clone of the subtree — every SdfPrimitive owns a
            // name String, a PbrMaterial with its own String, and several Vecs — producing a Vec
            // identical to the one just built, on the per-drag-frame partial-bake path. `prims` has
            // no other reader below, so the legacy branch borrows it and the common branch takes it.
            // Terrain collect already returns body-local (own + neighbour-shifted).
            let local_prims = if is_terrain || BODY_LOCAL_PRIMS {
                prims
            } else {
                offset_prims_to_local(&prims, body_origin)
            };
            let local_mirror_center = if BODY_LOCAL_PRIMS {
                mirror_center // already body-local
            } else {
                mirror_center - body_origin
            };

            // Hash body-local prims + body-level properties + boolean-chain ORDER: rigid
            // body moves don't change this hash (same local positions), but body-level
            // property changes (bevel_radius, mirror), the LAB operand generation, and a
            // `child_order` reorder DO invalidate. ONE implementation shared with cascade
            // + undo invalidation (bake_utils::hash_body_source) — the sites cannot drift.
            let source_hash = crate::bake_utils::hash_body_source(
                sg, body_nid, &local_prims,
                mirror_flags, mirror_sign, local_mirror_center, bevel_radius,
                self.lab_operand_gen,
            );

            // Template hash for instancing — excludes prim_uid so Shift+D clones match
            let template_hash = crate::bake_utils::compute_template_hash(sg, body_nid).unwrap_or(0);

            // Skip if already in-flight with same hash
            if self.in_flight.contains(&(body_nid, source_hash)) { continue; }

            // Per-body resolution: adaptive (based on AABB extent) or global.
            let body_res = self.effective_resolution(body_nid);

            // S2.5e — a sparse-eligible body (SparseBricks: toggle ON + Object Mode + res ≥ 512)
            // is NEVER drafted. Auto-draft is a DENSE-path speed trick — bake at res/2 during a
            // rapid drag so a full dense bake is cheaper — but res/2 = 256 < 512 drops the body
            // BELOW the sparse eligibility threshold, forcing a dense bake that nulls
            // `brick_resident`. That flips the body off the sparse path mid/end-drag → the C3
            // retighten guard (check_idle_retighten) no longer protects it → a full dense
            // re-tighten + 512MB heal readback fires on settle (the ~300-500ms SLOWPOLL), and the
            // next edit pays a fresh full sparse classify because residency was destroyed. Sparse
            // partial bakes are already sub-frame, so a sparse body has nothing to gain from draft
            // and everything to lose. Keep it at its full (≥512) resolution unconditionally.
            let sparse_body = self.sparse_bake_eligible(body_res);

            // Skip if cached grid already matches this hash AND was baked at
            // the target resolution (draft→full promotion changes resolution
            // without changing prim data, so hash alone is not sufficient).
            let target_res = if self.use_brick_bake && self.use_brick_pool {
                // Phase 4: coarse bake for classification only — brick fill handles full resolution.
                // dims_bricks × 2 gives 2 samples per brick per axis (8 samples per brick).
                ((body_res / graphics::brick_pool::BRICK_DIM) * 2).max(16)
            } else if sparse_body {
                body_res // never draft a sparse body (see above)
            } else {
                self.draft_resolution.unwrap_or(body_res)
            };
            if let Some(entry) = self.body_grids.get(&body_nid) {
                // S2.4 contract C5: a brick-resident (SparseBricks) entry only counts as "already
                // baked" while the sparse regime is STILL active for this body (toggle ON + Object
                // Mode + res ≥ 512). Otherwise — toggle turned OFF, or SDF Edit Mode entered
                // (gridbacked_only=false) — the empty-grid entry can serve neither the DC mesh nor
                // the volume raymarch, so an equal hash+res must NOT skip the dense re-bake.
                let entry_regime_ok = entry.brick_resident.is_none()
                    || self.sparse_bake_eligible(target_res);
                if entry.source_hash == source_hash && entry.bake_resolution == target_res
                    && entry_regime_ok
                {
                    // [HealTrace] a heal-marked body dedup-SKIPPED strands the heal mark
                    // (cpu_grid_stale stays set → the settle re-queues forever).
                    // Case closed 2026-07-09 (phantom-edit loop) — verbose trace parked behind
                    // crate::perf_spam_on(); the live tripwires are [RetightenArm] + the settle reason line.
                    if crate::perf_spam_on() && self.heal_tracing() && self.cpu_heal_queue.contains(&body_nid) {
                        eprintln!("[HealTrace] trigger SKIP body={:?} (hash+res match {}³) — heal mark STRANDED",
                            body_nid, target_res);
                    }
                    self.dirty_bodies.remove(&body_nid);
                    // The dedup skip must CONSUME the regions too (2026-07-16): they describe
                    // edits the recorded hash already covers (the rigid-translate arm makes the
                    // identical argument for its own region drop). Stranding them lets a later
                    // tiny edit union with a stale gesture-sized region set → an instant
                    // spurious oversized-window escalation with a "phantom" giant window.
                    self.dirty_regions.remove(&body_nid);
                    continue;
                }
            }
            if crate::perf_spam_on() && self.heal_tracing() {
                let e = self.body_grids.get(&body_nid);
                eprintln!(
                    "[HealTrace] trigger PROCEED body={:?} hash {}→{:016x} res {}→{} heal_mark={} regions={}",
                    body_nid,
                    e.map_or("none".into(), |e| format!("{:016x}", e.source_hash)),
                    source_hash,
                    e.map_or(0, |e| e.bake_resolution), target_res,
                    self.cpu_heal_queue.contains(&body_nid),
                    self.dirty_regions.get(&body_nid).map_or(0, |r| r.len()),
                );
            }

            // ([SparseLoop] DIAGNOSTIC removed 2026-07-14, S2.6 — it existed to attribute the
            //  toggle-era redundant-full-rebake loop; the regime is now structural.)

            // P1-#6b RIGID-TRANSLATE fast path: every visible prim of this brick-resident body
            // accumulated the SAME pure translation since the last dispatch — the whole
            // body-local field is rigid-translated. Move the grid WITH the prims (bounds
            // shift; the atlas content is bit-valid at the shifted bounds by construction)
            // and classify/bake/mesh NOTHING. The drawn mesh re-anchors via the EXPLICIT
            // `entry.mesh_rigid_offset` accumulated below (raster_gbuffer / raster_pick read
            // it; the next mesh at current bounds clears it). Mixed edits (rotate/scale/
            // slider — poisoned), partial-prim moves,
            // and mirror bodies fall through to the normal partial/scroll/full machinery.
            // This is what makes an arbitrarily fast single-prim drag free: no window to
            // outrun, no escape, no teleport case — the bounds simply follow the prim.
            // MUST run BEFORE the generation bump + in_flight insert below: it dispatches no
            // bake, so an in_flight entry here would never complete and the coalesce gate
            // would defer this body forever.
            if let Some(delta) = self.take_rigid_translate(body_nid, &local_prims, mirror_flags, target_res) {
                // Entry bounds + body_bounds (+ CPU chunked verts — empty for resident bodies):
                // the existing body-origin-shift primitive does exactly this bookkeeping.
                self.translate_body_cache(body_nid, delta);
                if let Some(t) = self.partial_tight_bounds.get_mut(&body_nid) {
                    t.0 += delta; t.1 += delta; // keep the escape predictor coherent
                }
                // The regions describe a field change that, expressed in the co-moving frame,
                // never happened; the hash stamp records the moved prims as baked (dedup skip).
                self.dirty_regions.remove(&body_nid);
                if let Some(entry) = self.body_grids.get_mut(&body_nid) {
                    entry.source_hash = source_hash;
                    // EXPLICIT draw re-anchor: the current resident mesh (anchored at the
                    // pre-shift bake) must draw translated by the accumulated rigid delta.
                    // Reset by the next mesh dispatched from a bake at the current bounds
                    // (clear_mesh_rigid_offset_if_current) — NEVER inferred from bounds deltas
                    // (escape/retighten move bounds without moving geometry).
                    entry.mesh_rigid_offset += delta;
                }
                crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "rigid", || format!(
                    "body={:?} delta={:.4?} (zero-bake translate)", body_nid, delta));
                // Owner-visible, drag-rate throttled (1-in-30 — a rigid drag hits this every frame).
                {
                    use std::cell::Cell;
                    thread_local! { static RIGID_N: Cell<u32> = Cell::new(0); }
                    if RIGID_N.with(|n| { let v = n.get().wrapping_add(1); n.set(v); v % 30 == 1 }) {
                        eprintln!("[RigidMove] body={:?} delta={:.4?} — bounds follow the prims, zero bake (1-in-30 shown)",
                            body_nid, delta);
                    }
                }
                continue;
            }

            // Increment generation for this body
            let gen = self.generations.entry(body_nid).or_insert(0);
            *gen += 1;
            let generation = *gen;

            self.in_flight.insert((body_nid, source_hash));
            self.bake_start_times.insert(body_nid, Instant::now());

            // Use draft resolution during slider drag for faster feedback.
            // When brick_bake_active, use target_res (coarse) so the dispatched
            // bake resolution matches what the hash check at line 771 expects.
            // Without this, the bake dispatches at full resolution but the check
            // compares against coarse target_res — causing an infinite rebake loop.
            // Sparse bodies are never drafted (see `sparse_body` above): the dispatched res must
            // stay ≥512 so `sparse_bake_eligible(max_bake_res)` (the sparse-vs-dense fork below)
            // holds, keeping the body on the sparse path and preserving `brick_resident`.
            let is_draft = self.draft_resolution.is_some() && !sparse_body;
            let brick_bake_active = self.use_brick_bake && self.use_brick_pool;
            let res = if brick_bake_active {
                target_res
            } else if sparse_body {
                body_res
            } else {
                self.draft_resolution.unwrap_or(body_res)
            };
            if is_draft {
                self.draft_baked_bodies.insert(body_nid);
            }

            // Compute per-body tight AABB with margin (body-local).
            // Partial bake optimisation: when dirty_regions exist (prim-level edit,
            // not full-body dirty) AND the body already has a cached grid, reuse
            // the cached grid bounds. Recomputing bounds from current prims would
            // shift the AABB (the moved prim changes the tight fit), causing the
            // bounds-match check below to fail and forcing a full bake.
            // The dirty regions from mark_prim_dirty_at() already cover both the
            // old and new prim positions, so the grid AABB doesn't need to change.
            //
            // Edge case: if the prim moved OUTSIDE the cached AABB, the new tight
            // bounds will be larger. Detect this and fall back to full bake with
            // recomputed bounds (the partial region was clamped to the old grid edge).
            //
            // PARTIAL BAKE (dirty_regions exist): Reuse cached bounds so voxel mapping
            // stays stable. Promote to full bake if bounds expanded (prim escaped grid)
            // or shrunk >5% on any axis (accumulated scale-down wastes voxels).
            //
            // FULL BAKE (no dirty_regions): ALWAYS recompute tight bounds from current
            // prims. This ensures bounds never drift — every full bake realigns.
            let has_partial_regions = self.dirty_regions.contains_key(&body_nid);
            // Track interactive (partial) bakes so a full re-tighten fires once editing
            // settles (check_idle_retighten). A full bake here is already tight, so it
            // clears any pending settle for this body.
            if has_partial_regions {
                // P2-2048 (2026-07-17): NEVER arm the settle re-tighten for a brick-resident
                // (sparse) body. Contract C3 says check_idle_retighten SKIPS them (no dense CPU
                // grid to reclaim resolution for), so arming here was a pure no-op RE-ARM: every
                // partial walk dispatch — including a no-op walk (+0 bricks, apply=0) — re-
                // inserted the body into `pending_retighten`, and the "[RetightenArm] …
                // phantom dirty regions?" tripwire fired on state the settle machinery can never
                // consume. Dense partials keep the arm (their settle re-tighten is load-bearing).
                let sparse_resident = self.body_grids.get(&body_nid)
                    .map_or(false, |e| e.brick_resident.is_some());
                if !sparse_resident {
                    // [RetightenArm] diagnostic: a partial dispatch implies a fresh dirty mark, so
                    // arming while IDLE should be impossible — if this prints, something creates
                    // dirty regions without a real edit (phantom regions feeding the settle loop).
                    if self.pending_retighten.insert(body_nid) && self.retighten_idle() {
                        eprintln!("[RetightenArm] body={:?} by=partial_dispatch while IDLE — phantom dirty regions?", body_nid);
                    }
                }
            } else {
                self.pending_retighten.remove(&body_nid);
            }
            // [BAKEPROF] diagnostic: records whether the partial-bake path was promoted to a
            // FULL bake by an aabb_escape this dispatch (the "not live" cost per LIVE_MOVE_PERF.md).
            // Set only inside the `expanded` branch below; read at the emit point after res3.
            let mut escaped = false;
            // P1: escape handled by a brick-aligned window SCROLL this dispatch (stays a
            // partial). Read by the [BAKEPROF] label.
            let mut scrolled_this = false;
            // L2 Mesh Terrain: lattice-locked bounds — never escape-grow / retighten from prims.
            // Adjacent tiles share a ≥1-cell apron so DC samples coincide on the shared face.
            let (grid_min, grid_max) = if is_terrain {
                let lattice = crate::terrain::tile_lattice_bake_bounds_body_local();
                self.partial_tight_bounds.insert(body_nid, lattice);
                self.body_bounds.insert(body_nid, lattice);
                lattice
            } else if has_partial_regions {
                if let Some(&cached) = self.body_bounds.get(&body_nid) {
                    let new_bounds = compute_body_bake_bounds(&local_prims, mirror_flags, local_mirror_center, field_margin);
                    // Record the current tight bounds; the returned PREVIOUS entry feeds the escape
                    // policy's movement prediction (which walls the body is approaching) — see
                    // compute_escape_grid_bounds. (check_idle_retighten does NOT read this map —
                    // it recomputes fresh bounds; see the field doc in mod.rs.)
                    let prev_tight = self.partial_tight_bounds.insert(body_nid, new_bounds);
                    // Expansion: prim moved outside the cached grid (shared predicate — also
                    // drives the capture-replay tests, so they can never model a stale copy).
                    let expanded = tight_escapes_grid(cached, new_bounds);
                    // STABLE VOXEL MAPPING: the grid bounds define the world->voxel
                    // mapping read by the VP shader. ANY change to them remaps the whole
                    // texture (visible "pop" + full re-upload). So during an interaction
                    // we keep the cached grid CONSTANT and react ONLY to a genuine ESCAPE
                    // — never to shrinkage. Shrinkage is reclaimed by the next FULL bake
                    // (drag release / idle), which always retightens. Reacting to shrink
                    // mid-drag caused remap pops for no visual benefit.
                    //
                    // On escape we inflate ONCE to a generous, stable grid (union with the
                    // old grid, never shrink) large enough to contain the ongoing drag, so
                    // the prim does not escape again every frame. As a long drag grows the
                    // grid monotonically, escapes — and thus pops — become progressively
                    // rarer instead of repeating each frame.
                    //
                    // P1 (sparse bodies): a brick-aligned window SCROLL is tried FIRST — it
                    // translates the frozen index (retained bricks keep valid pool content),
                    // keeps the dirty regions, pushes the exposed slab as regions, and the
                    // dispatch stays a frozen-index PARTIAL (slab classify + slab bake) at
                    // CONSTANT extent — no full, no sampling-density dilution, no pop (the
                    // mesh is world-anchored brick data, not a remapped texture). Grow-full
                    // remains the fallback for grow/oscillation/teleport/non-sparse.
                    if expanded {
                        if let Some(scrolled) = self.try_sparse_scroll(
                            body_nid, cached, new_bounds, prev_tight, target_res)
                        {
                            scrolled_this = true;
                            scrolled
                        } else {
                        escaped = true; // [BAKEPROF]: partial edit promoted to a full grid bake this dispatch
                        // Escape policy → compute_escape_grid_bounds (pure fn at the bottom of this
                        // file, unit-tested by replaying the 2026-07-03 [BAKEPROF] drag capture).
                        // Anchored to the STABLE `cached` grid, hard-capped at MAX_GRID_MULT, with
                        // PREDICTIVE multi-axis headroom fed by `prev_tight` (the previous dispatch's
                        // tight bounds). Full design rationale — including the reverted re-centering
                        // dead-end and the balloon/under-sampling trade-off — lives on the fn doc.
                        let padded = compute_escape_grid_bounds(cached, new_bounds, prev_tight);
                        if cfg!(debug_assertions) {
                            eprintln!(
                                "[DirtyFallback] body={:?} reason=aabb_escape(bounded) new_min={:?} new_max={:?} cached_min={:?} cached_max={:?} grid_min={:?} grid_max={:?}",
                                body_nid, new_bounds.0, new_bounds.1, cached.0, cached.1, padded.0, padded.1
                            );
                        }
                        // [visrec] AABB escape → promoted to a FULL bake over grown bounds (Part B #6).
                        crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "escape", || format!(
                            "body={:?} old={:.3?}..{:.3?} new_tight={:.3?}..{:.3?} grid={:.3?}..{:.3?}",
                            body_nid, cached.0, cached.1, new_bounds.0, new_bounds.1, padded.0, padded.1));
                        self.dirty_regions.remove(&body_nid);
                        self.body_bounds.insert(body_nid, padded);
                        padded
                        }
                    } else {
                        cached
                    }
                } else {
                    let tight = compute_body_bake_bounds(&local_prims, mirror_flags, local_mirror_center, field_margin);
                    self.partial_tight_bounds.insert(body_nid, tight);
                    let padded = pad_grid_bounds(tight.0, tight.1);
                    self.body_bounds.insert(body_nid, padded);
                    padded
                }
            } else {
                // FULL BAKE: always recompute tight bounds. This is the reset point
                // that prevents gradual bounds drift from accumulated small edits.
                let tight = compute_body_bake_bounds(&local_prims, mirror_flags, local_mirror_center, field_margin);
                // Keep the movement-prediction reference fresh on EVERY dispatch path. Full bakes
                // (settle, undo/redo, load, structural edits) are exactly the jump events that would
                // otherwise leave a stale entry — a next-drag escape would then read a large,
                // wrong-direction delta and grant headroom on a spurious side (verified bounded but
                // wasteful). Recording here makes stale `prev_tight` structurally impossible: the
                // entry is always at most one dispatch old.
                self.partial_tight_bounds.insert(body_nid, tight);
                let padded = pad_grid_bounds(tight.0, tight.1);
                self.body_bounds.insert(body_nid, padded);
                padded
            };

            // Update adaptive resolution for this body now that we have AABB bounds.
            // Skip during interaction (suppress NOT set = volume-only drag active):
            // locking resolution prevents flip-flop between 96³↔128³ as the AABB
            // shifts during drag, which causes texture realloc artifacts.
            // ALSO lock it during active interaction. `!has_partial_regions` alone doesn't cover a SCALE
            // that grows past the grid: the aabb_escape converts the edit to a FULL bake (regions cleared),
            // so adaptive res would recompute MID-SCALE → resolution change → resident-volume realloc → a
            // visible pop (the "scaling cube visual bug"). Gate on recent edit activity so the resolution is
            // fixed for the whole scale/drag and only re-evaluated on settle (the idle re-tighten, which
            // fires at 150ms — past this 100ms window — and recomputes the body's true adaptive resolution).
            let interacting = self.last_dirty_time.map_or(false, |t| (t.elapsed().as_millis() as u64) < 100);
            // Honour Grid-picker locks — adaptive size must not undo a deliberate res click.
            if self.use_adaptive_resolution
                && !has_partial_regions
                && !interacting
                && !self.body_res_user_locked.contains(&body_nid)
            {
                let new_res = self.compute_resolution_for_body(grid_min, grid_max);
                self.body_target_resolutions.insert(body_nid, new_res);
            }

            // Per-axis asymmetric resolution from the body's AABB aspect ratio.
            // The longest axis gets `res` voxels; short axes are proportionally
            // smaller (minimum 8). For partial bakes, reuse the cached grid's
            // per-axis resolution to keep voxel mapping stable.
            let res3: [u32; 3] = if has_partial_regions {
                if let Some(entry) = self.body_grids.get(&body_nid) {
                    // Infer per-axis resolution from cached grid size + bounds.
                    // If the cached grid was cubic (old path), this falls back
                    // to cubic [res, res, res] since entry size = res^3.
                    let cached_total = entry.grid.len();
                    let cached_cubic_res = (cached_total as f32).cbrt().round() as u32;
                    if cached_total == (cached_cubic_res as usize).pow(3) && cached_cubic_res == res {
                        // Cached grid was cubic at the same scalar resolution —
                        // this is a legacy grid. Preserve cubic to keep voxel mapping stable.
                        [res, res, res]
                    } else {
                        // Cached grid was already asymmetric — use same dimensions.
                        Self::compute_asymmetric_resolution(res, grid_min, grid_max)
                    }
                } else {
                    Self::compute_asymmetric_resolution(res, grid_min, grid_max)
                }
            } else {
                Self::compute_asymmetric_resolution(res, grid_min, grid_max)
            };

            // CUBIC INVARIANT for the DC (raster mesh) path. The chunked DC extractor + its
            // `FlatGridAccessor` index the grid with a SINGLE `nverts` on every axis, and
            // `accept_gpu_result` infers resolution via `cbrt(grid.len())` — both assume grid.len() is a
            // perfect cube. An asymmetric `res3` (rx·ry·rz) from an elongated AABB — e.g. a Shift+D
            // duplicate whose bounds inflate hard on one axis after an aabb_escape — breaks that: the
            // cube the DC walks (cbrt(rx·ry·rz)³) exceeds the packed asymmetric buffer → index out of
            // bounds → CRASH (and silently mis-indexes milder asymmetries). GridBacked bakes feed DC, so
            // force them cubic at the longest axis. TextureOnly (>256³) is sampled by the VP shader in
            // normalized space and keeps the asymmetric (voxel-saving) resolution.
            let res3 = {
                let max_axis = res3[0].max(res3[1]).max(res3[2]);
                // max_mesh_resolution: covers dense GridBacked (≤512) AND sparse 1024 — a sparse
                // 1024 request MUST come out cubic here or the sparse_eligible cubic check below
                // would silently fall it through (and nothing else can take a 1024 request).
                if max_axis <= self.max_mesh_resolution() {
                    [max_axis, max_axis, max_axis]
                } else {
                    res3
                }
            };

            // [BAKEPROF] measurement-only: one line per body per dispatch, classifying the bake
            // path so a real prim drag reveals the partial:full ratio and escape frequency
            // (LIVE_MOVE_PERF.md hypotheses 1/2/4). Emitted here — after bounds + res3 are decided
            // but before the RAM-hit / first-cascade branches (labelled distinctly). Runtime-gated;
            // zero cost when off. NOTE: the dispatch is async, so this reports the PATH chosen on the
            // trigger thread, not the GPU bake wall time (that lands in poll_primary_bake).
            if self.bake_prof {
                let has_grid = self.body_grids.contains_key(&body_nid);
                let region_count = self.dirty_regions.get(&body_nid).map(|r| r.len()).unwrap_or(0);
                let path = if !has_grid { "first(cascade)" }
                    else if !has_partial_regions { "full" }
                    else if escaped { "escape->full" }
                    else if scrolled_this { "scroll->partial" }
                    else { "partial" };
                eprintln!(
                    "[BAKEPROF] body={:?} path={} res={}x{}x{} draft={} regions={} escaped={} prims={}",
                    body_nid, path, res3[0], res3[1], res3[2], is_draft, region_count, escaped, local_prims.len()
                );
            }

            // Check RAM cache for a matching entry before dispatching a bake
            if self.ram_cache_enabled {
                if let Some(ram_entry) = self.ram_cache.get(&body_nid) {
                    if ram_entry.source_hash == source_hash
                        && ram_entry.bake_resolution == target_res
                        && (ram_entry.grid_min - grid_min).length() < 0.01
                        && (ram_entry.grid_max - grid_max).length() < 0.01
                        && ram_entry.invalidation_epoch == self.invalidation_epoch
                    {
                        // RAM cache hit — restore grid to body_grids, skip bake
                        let ram_entry = self.ram_cache.remove(&body_nid).unwrap();
                        self.ram_cache_used_bytes = self.ram_cache_used_bytes.saturating_sub(ram_entry.grid.len() * 4);
                        let ram_bake_res = ram_entry.bake_resolution;
                        self.body_grids.insert(body_nid, BodyCacheEntry {
                            grid: ram_entry.grid,
                            source_hash: ram_entry.source_hash,
                            grid_min: ram_entry.grid_min,
                            grid_max: ram_entry.grid_max,
                            bake_resolution: ram_bake_res,
                            shared_source: None,
                            brick_resident: None, // RAM cache holds dense grids only (C4 guard)
                            sparse_frozen: None,
                            mesh_rigid_offset: Vec3::ZERO,
                        });
                        self.dirty_bodies.remove(&body_nid);
                        // Undo generation increment and in-flight tracking
                        if let Some(gen) = self.generations.get_mut(&body_nid) {
                            *gen = gen.saturating_sub(1);
                        }
                        self.in_flight.remove(&(body_nid, source_hash));
                        self.bake_start_times.remove(&body_nid);
                        // Flag resident upload if this is the active body
                        if self.resident_body_nid == Some(body_nid) {
                            self.resident_upload_needed = true;
                            self.resident_buffer_valid = false; // RAM-restored grid not yet in rv.buffer
                        }
                        // GATE 2: a full grid just landed (RAM restore = accept-equivalent) —
                        // clear the region; absent = full, every consumer's conservative default.
                        self.note_full_bake_accepted(body_nid);
                        sync_baked.push(body_nid);
                        continue;
                    }
                }
            }

            // -------------------------------------------------------
            // Level 4 — Progressive Resolution Cascade (first bake only)
            // When a body has NO existing grid (first creation / topology change),
            // request an ASYNC GPU 64^3 coarse bake for instant feedback, then
            // cascade through higher resolutions asynchronously.
            //
            // The coarse bake used to be a SYNCHRONOUS single-threaded CPU eval
            // (`bake_body_grid`) on the trigger thread — measured 57ms @1 prim →
            // 155ms @7 prims, blocking the input frame. The GPU does the same 64³
            // eval in <1ms. Replaced with an async GPU `PrimaryBakeRequest` at the
            // coarse resolution (cascade level 0). The result is accepted in
            // `poll_primary_bake` → `accept_gpu_result` (sets resident_upload_needed,
            // resident_buffer_valid=false, body_grids entry, rv.bake_bounds) →
            // `dispatch_grid_dc_async` (full DC, since no dirty_mesh_region entry
            // makes is_partial_bake=false) → `advance_cascade` (promotes 64→res).
            // The surface therefore appears ONE FRAME later, correctly, with ZERO CPU
            // eval on the trigger thread. The 64³ grid stays CUBIC (CUBIC INVARIANT).
            //
            // Body-mirror parity: the GPU coarse bake carries mirror_flags/mirror_sign/
            // mirror_center in the request; the bake shader emits the top-level body as a
            // SCOPE_OPEN whose scope-machine UNION reflects the field (volume_bake.wgsl
            // ~811-818), identical to `bake_body_grid`'s `eval_scene_body_mirrored`. A
            // mirrored body's first bake is thus a FULL shape, not a half-slab.
            //
            // Fallback: if no GPU device is available, `dispatch_pending_primary_bakes`
            // routes the queued request through `cpu_fallback_bakes`, so the coarse eval
            // still happens (async, off the trigger thread) even without a GPU.
            // -------------------------------------------------------
            let has_existing_grid = self.body_grids.contains_key(&body_nid);
            if !has_existing_grid && !is_draft && !brick_bake_active {
                let cascade_low_res: u32 = 64.min(res); // never exceed target
                // CUBIC INVARIANT: the coarse grid stays a perfect cube (cascade_low_res³).
                // GridBacked DC + accept_gpu_result's `cbrt(grid.len())` assume a cube; an
                // asymmetric coarse res3 would mis-infer the resolution → OOB on readback.
                let coarse_res3 = [cascade_low_res, cascade_low_res, cascade_low_res];

                // Set up the progressive cascade. Level 0 = this coarse GPU bake (in flight
                // once queued below). advance_cascade fires when the coarse result lands and
                // promotes level 0 → level 1 (target res). Only needed if target > coarse;
                // otherwise the coarse bake IS the final resolution and no promotion runs.
                if res > cascade_low_res {
                    let cascade = BakeResolutionCascade {
                        target_resolutions: [cascade_low_res, res, res],
                        completed: [false, false, false],
                        current_level: 0, // coarse level, dispatched below
                        generation,
                    };
                    self.cascades.insert(body_nid, cascade);
                }

                // Enqueue the async GPU coarse bake (cascade level 0). in_flight was already
                // inserted above (line ~346) with this (body_nid, source_hash) — the coalescer
                // in dispatch_pending_primary_bakes keeps this newest request per body, and
                // poll_primary_bake clears in_flight on accept. Do NOT push to sync_baked:
                // the async acceptance path owns the DC dispatch (dispatch_grid_dc_async in
                // poll_primary_bake), so the caller in dirty.rs must not double-dispatch.
                let mut prim_floats = graphics::serialize_body_for_bake_hot(
                    sg, body_nid,
                    if BODY_LOCAL_PRIMS { Vec3::ZERO } else { body_origin },
                );
                let clip_curve_floats = graphics::serialize_clip_curves_for_bake(sg, body_nid, &mut prim_floats);
                let mesh_grid_floats = graphics::serialize_mesh_grids_for_bake(sg, body_nid, &mut prim_floats);
                // Coarse cascade is always a draft-quality pass — pack height mips small.
                let height_map_floats = graphics::serialize_height_maps_for_bake(
                    sg,
                    body_nid,
                    &mut prim_floats,
                    core_math::modifiers::DRAFT_HEIGHT_MIP_MAX_SIDE,
                );
                self.primary_bake_requests.push(PrimaryBakeRequest {
                    body_nid,
                    prim_floats,
                    resolution: coarse_res3,
                    bounds_min: grid_min,
                    bounds_max: grid_max,
                    bake_min: [0, 0, 0],
                    bake_max: coarse_res3,
                    mirror_flags: mirror_flags as u32,
                    mirror_sign: mirror_sign as u32,
                    mirror_center: local_mirror_center,
                    bevel_radius,
                    source_hash,
                    template_hash,
                    generation,
                    base_grid: None,
                    clip_curve_floats,
                    mesh_grid_floats,
                    height_map_floats,
                    sparse: None, // coarse cascade level is always dense
                    coarse_cull_ok: false, // dense path — never classified
                    force_cpu_mod_stack: crate::bake_utils::body_needs_cpu_full_stack_bake(sg, body_nid),
                });
                continue; // Skip normal dispatch — the async coarse bake + cascade handle this body
            }

            // Check for partial bake opportunity: dirty regions + existing grid with same bounds.
            // Computed BEFORE the GPU/CPU fork so both paths can use it.
            // TextureOnly mode (>256³): always full bake — no CPU grid for base data.
            let max_bake_res = res3[0].max(res3[1]).max(res3[2]);
            // Dense-vs-sparse cap split: >512 is TextureOnly ONLY when the sparse regime is off
            // (max_mesh_resolution = 512). With sparse active the ceiling is 1024 and a >512
            // request stays on the mesh path — the sparse fork below takes it (it can never
            // dense-dispatch: sparse_bake_eligible keys on the SAME regime pair, the cubic
            // invariant was forced above, and no dense 1024³ cached grid can exist — see the
            // binding-size assert at the dispatch's dense buffer sizing).
            let is_texture_only = max_bake_res > self.max_mesh_resolution();
            let partial_regions = if is_texture_only {
                self.dirty_regions.remove(&body_nid); // consume and discard
                None
            } else {
                self.dirty_regions.remove(&body_nid)
            };
            let grid_voxel_count = res3[0] as usize * res3[1] as usize * res3[2] as usize;
            // Does a same-bounds, same-size cached grid exist? → this is a PARTIAL bake over a sub-region.
            // We do NOT clone the (64MB @256³) grid here. Path A bakes the dirty slab in place into
            // rv.buffer and needs no CPU base_grid seed; cloning eagerly EVERY partial bake was the ~18ms
            // cost (the "small edits less responsive than large moves" report — large = full bakes, no
            // clone). The seed is materialized lazily at DISPATCH, only on the rare non-resident path that
            // still uses the throwaway output_buf.
            let has_existing_grid = partial_regions.as_ref().map_or(false, |regions| {
                !regions.is_empty() && self.body_grids.get(&body_nid).map_or(false, |entry| {
                    (entry.grid_min - grid_min).length_squared() < 1e-6
                        && (entry.grid_max - grid_max).length_squared() < 1e-6
                        && entry.grid.len() == grid_voxel_count
                })
            });

            // S2.5: the SparseBricks (mode-2) eligibility for THIS request — S2.4's exact full-bake
            // gate (sparse_bake_eligible = toggle ON + Object Mode + res ≥ 512, not TextureOnly,
            // cubic). `!has_existing_grid` is implied for brick-resident bodies (their grid is
            // empty by design, so the dense-partial predicate above can never be true) but is kept
            // explicit: a DENSE-partial body must never be re-routed sparse mid-drag.
            let sparse_eligible = self.sparse_bake_eligible(max_bake_res)
                && !is_texture_only
                && !has_existing_grid
                && res3[0] == res3[1] && res3[1] == res3[2];
            // ([SparseWhy] DIAGNOSTIC removed 2026-07-14, S2.6 — it existed to name which gate sent
            //  a ≥512 full bake down the dense path when the toggle could silently be OFF (review
            //  finding R1). With the regime structural, the remaining dense ≥512 routes are the
            //  designed ones: SDF Edit Mode (gridbacked_only=false), TextureOnly, an existing dense
            //  grid mid-drag, or a non-cubic request.)
            // S2.5 frozen-index PARTIAL candidate: dirty regions exist (a prim drag/nudge — not a
            // structural mark_body_dirty, which clears them) AND the body's entry is brick-resident
            // at exactly THESE bounds and THIS resolution, with a retained CPU copy of the frozen
            // index. The bounds equality mirrors the dense `has_existing_grid` check; an
            // aabb_escape can't reach here with regions (the escape branch above removed them AND
            // moved body_bounds away from entry bounds — double-keyed), so a bounds-changing edit
            // always takes the FULL classify path.
            let sparse_frozen_partial: Option<(std::sync::Arc<core_math::chunked_dc::SparseIndexOnly>, u64)> =
                if sparse_eligible && partial_regions.as_ref().map_or(false, |r| !r.is_empty()) {
                    self.body_grids.get(&body_nid).and_then(|entry| {
                        let marker = entry.brick_resident?;
                        let frozen = entry.sparse_frozen.as_ref()?;
                        let bounds_match = (entry.grid_min - grid_min).length_squared() < 1e-6
                            && (entry.grid_max - grid_max).length_squared() < 1e-6;
                        let bricks_per_axis = (res3[0] + SPARSE_BRICK_DIM - 1) / SPARSE_BRICK_DIM;
                        let res_match = entry.bake_resolution == res3[0]
                            && frozen.dims_bricks == [bricks_per_axis; 3];
                        (bounds_match && res_match).then(|| (frozen.clone(), marker))
                    })
                } else {
                    None
                };

            // Compute voxel-space bake sub-region from dirty regions (union of all).
            // Used by both GPU and CPU partial bake paths. Per-axis resolution.
            // S2.5: the sparse frozen-index partial derives its window from the SAME code — no
            // second window implementation (`window_from_regions` widens the dense-only gate).
            let window_from_regions = has_existing_grid || sparse_frozen_partial.is_some();
            let (mut voxel_bake_min, mut voxel_bake_max) = match (window_from_regions, partial_regions.as_ref()) {
                (true, Some(regions)) if !regions.is_empty() => {
                    let extent = grid_max - grid_min;
                    let inv_extent = Vec3::new(
                        if extent.x.abs() > 1e-8 { 1.0 / extent.x } else { 0.0 },
                        if extent.y.abs() > 1e-8 { 1.0 / extent.y } else { 0.0 },
                        if extent.z.abs() > 1e-8 { 1.0 / extent.z } else { 0.0 },
                    );
                    let denom_x = if res3[0] > 1 { (res3[0] - 1) as f32 } else { 1.0 };
                    let denom_y = if res3[1] > 1 { (res3[1] - 1) as f32 } else { 1.0 };
                    let denom_z = if res3[2] > 1 { (res3[2] - 1) as f32 } else { 1.0 };
                    // Union of all dirty regions in voxel space
                    let mut v_min = res3;
                    let mut v_max = [0u32, 0u32, 0u32];
                    for region in regions.iter() {
                        // Mirror: the partial dirty region covers only the EDITED prim's half. Expand it to
                        // enclose the mirror reflection(s) so this bake re-evaluates the mirrored voxels too —
                        // otherwise the reflected half keeps a stale field (white-box ghost / thin slab, the
                        // bug that returns only with the mirror modifier on). Reuses the SAME reflect math as
                        // the grid bounds (compute_body_bake_bounds). No-op when mirror_flags == 0.
                        let (reg_min, reg_max) = crate::bake_utils::expand_aabb_for_mirror(
                            region.local_min, region.local_max, mirror_flags, local_mirror_center,
                        );
                        let t_min = (reg_min - grid_min) * inv_extent;
                        let t_max = (reg_max - grid_min) * inv_extent;
                        let ix_min = (t_min.x * denom_x).floor().max(0.0) as u32;
                        let iy_min = (t_min.y * denom_y).floor().max(0.0) as u32;
                        let iz_min = (t_min.z * denom_z).floor().max(0.0) as u32;
                        // +1 because bake_max is exclusive
                        let ix_max = ((t_max.x * denom_x).ceil() as u32 + 1).min(res3[0]);
                        let iy_max = ((t_max.y * denom_y).ceil() as u32 + 1).min(res3[1]);
                        let iz_max = ((t_max.z * denom_z).ceil() as u32 + 1).min(res3[2]);
                        v_min[0] = v_min[0].min(ix_min);
                        v_min[1] = v_min[1].min(iy_min);
                        v_min[2] = v_min[2].min(iz_min);
                        v_max[0] = v_max[0].max(ix_max);
                        v_max[1] = v_max[1].max(iy_max);
                        v_max[2] = v_max[2].max(iz_max);
                    }
                    (v_min, v_max)
                }
                _ => ([0, 0, 0], res3),
            };

            // ---------------------------------------------------------------
            // S2.5 — SparseBricks routing: frozen-index PARTIAL vs FULL classify.
            //
            // For a brick-resident body with dirty regions (a prim drag/nudge):
            //   1. NO re-classify — the atlas index is FROZEN from the last full sparse bake
            //      (CPU copy: `BodyCacheEntry.sparse_frozen`; S2.3's adversarial test pins that
            //      a bake against a frozen index + handled-brick writes are bit-exact).
            //   2. Dilate the dirty voxel window by one brick and check EVERY overlapped brick
            //      against the frozen index — S2.5b TAG COMPATIBILITY, not sentinel absence
            //      (`sparse_window_tags_compatible`): a handle is compatible with anything (the
            //      partial rewrites it); a sentinel is compatible iff the moved field's
            //      mini-classify (the SAME 9-sample predicate the full classifier uses) still
            //      needs that sentinel. This is what makes partials REACHABLE for solids — a
            //      dragged cube's dirty window covers its deep inside, whose INTERIOR sentinels
            //      are correct by design and must NOT force a full re-classify (the S2.5
            //      sentinel-absence rule escalated EVERY drag frame at 512³, owner-measured
            //      85-104ms classify + full bake per frame). Compatible → PARTIAL mode-2 bake
            //      over the dilated window: pool updated IN PLACE, index not re-uploaded.
            //   3. Any INCOMPATIBLE brick (surface entered an unallocated brick, or a sign flip
            //      without a handle) → ESCALATE: full sparse bake exactly as S2.4 (fresh
            //      classify + upload_index + full-box dispatch). The 1-brick dilation is what
            //      makes the check SEE a formerly-INTERIOR/EXTERIOR brick that should now be
            //      surface/apron: the dirty region covers everywhere the field changed (old ∪
            //      new prim AABB + margin), so the moved surface's 1-ring bricks all lie inside
            //      the dilated window — after a passing check the mesher's invariant (surface +
            //      1-ring handled-or-sign-correct) still holds. Sentinel bricks OUTSIDE the
            //      window are untouched by construction (the shader writes only
            //      [bake_min,bake_max), and sentinel-brick voxels are dropped by the mode-2
            //      handle guard) — the mesher's ±far reads for them stay as valid as at
            //      full-bake time.
            // Classify remains ONLY on: first sparse bake (no frozen index), escalation,
            // structural edits (mark_body_dirty cleared the regions → full), and bounds changes
            // (aabb_escape cleared regions + moved bounds → full).
            // ---------------------------------------------------------------
            // P1-#7 ASYNC FULL CLASSIFY: the FULL arms below only RAISE this flag — the
            // 38-45ms `classify_prims_to_sparse_index` (the last frame-thread hitch: select /
            // duplicate / escalation / escape-full at 512³) runs on a background worker. The
            // push site at the bottom of the loop builds the complete request (sparse: None
            // placeholder) and hands it to `spawn_async_classify` instead of pushing; the
            // worker completes the payload and `poll_async_classifies` lands it. The in_flight
            // entry inserted above gates the body for the whole flight (the coalescer defers
            // it), so the spawn-time snapshot stays the newest dispatch until landing.
            let mut sparse_full_async = false;
            // Windowed escalated reclassify (see the WINDOWED RECLASSIFY block below): `Some` =
            // this dispatch's sparse payload is completed by a background window-walk worker
            // (`spawn_async_window_reclassify`) instead of travelling on the request.
            let mut sparse_windowed: Option<SparseWindowedSpawn> = None;
            let sparse: Option<SparseBakeRequest> = if !sparse_eligible {
                None
            } else {
                // Band shared by the FULL classify (async worker), the S2.5b window
                // mini-classify and the frozen-partial walk — ONE formula (`sparse_classify_band`,
                // the slice-1 `2.0 * cell_world` pre-pass convention). Verdicts drift if any
                // consumer diverges.
                let res = res3[0];
                let band = sparse_classify_band(grid_min, grid_max, res);
                // OVERSIZED-WINDOW ESCALATION (the 1024 device-loss fix, 2026-07-13). The
                // frozen-index PARTIAL's design premise is a SMALL window ("dilate the dirty
                // voxel window by one brick" — a drag nudge). A fast-growing / fast-moving prim
                // at 1024³ produces windows covering most of the grid, and then the partial arm
                // is strictly WORSE than the async FULL classify on BOTH axes that killed the
                // owner's session:
                //   (a) the mini-classify walk below runs ON THE FRAME THREAD — measured
                //       ~0.2µs/brick ≈ 200-300ms/frame for a ~1.4M-brick window at 1024
                //       ([TriggerBake] 298ms, AZRA_AUTOREPRO capture 2026-07-13);
                //   (b) incremental allocation APPENDS handles for the whole swept frontier
                //       every frame while reclaim lags (a growing surface vacates nothing), so
                //       the pool balloons past the true surface count — observed 355MB → 1.43GB
                //       in seconds for ONE small cylinder; the next ×1.25 `ensure_capacity` step
                //       crosses `max_storage_buffer_binding_size` (2.1GB) → wgpu validation
                //       error every dispatch → "Command encoder is invalid" cascade → device
                //       loss (the owner's "panicked repeatedly, then crashed").
                // The async FULL classify has neither problem: it runs off the frame thread
                // (P1-#7) and REPLACES the index wholesale, resetting `allocated` to the true
                // surface count (observed 700k → 327k bricks on the very next full). Escalate
                // when the dilated window covers more than 1/8 of the grid's bricks — an
                // order-of-magnitude above real drag windows (2-7% measured in the 1024 e2e
                // drag phases) and an order below the pathological ones (56-90%), and
                // scale-invariant (512 behavior unchanged: its windows are the same fraction).
                // This enforces the S2.5 small-window premise STRUCTURALLY instead of trusting
                // callers to keep dirty regions small.
                if let Some((frozen, marker)) = sparse_frozen_partial {
                    let (dil_min, dil_max) = core_math::chunked_dc::dilate_voxel_window(
                        voxel_bake_min, voxel_bake_max, SPARSE_BRICK_DIM, res3);
                    let (window_bricks, total_bricks) = {
                        let bd = SPARSE_BRICK_DIM;
                        let wb: u64 = (0..3).map(|a| {
                            ((dil_max[a].saturating_sub(dil_min[a]) + bd - 1) / bd) as u64
                        }).product();
                        let total: u64 =
                            (0..3).map(|a| ((res3[a] + bd - 1) / bd) as u64).product();
                        (wb, total)
                    };
                    let window_too_large = window_bricks * 8 > total_bricks;
                    // P1-2048 (2026-07-17): COST decides, the fraction is a secondary guard.
                    // The walk cost is linear in window bricks (SPARSE_WALK_NS_PER_BRICK doc);
                    // a 2048³ window can be under 1/8 of the grid yet cost 300ms+ on the frame
                    // thread. Estimate it and route over-budget windows to the SAME windowed
                    // worker — never a second off-thread path.
                    let est_walk_ms =
                        window_bricks as f64 * SPARSE_WALK_NS_PER_BRICK / 1.0e6;
                    let walk_over_budget = est_walk_ms > SPARSE_WALK_FRAME_BUDGET_MS;
                    // WINDOWED RECLASSIFY (2026-07-16, the escalation residual — Q3 of the O3
                    // measurement gate): an oversized window in (1/8, 1/2] of the grid's bricks
                    // no longer forces a FULL grid reclassify. It runs the EXACT S2.5b/c
                    // frozen-partial machinery (mini-classify + incremental alloc/retag/reclaim,
                    // `sparse_window_allocate_incompatible`) — just on a background WORKER
                    // instead of the frame thread, landing as an ordinary Partial through
                    // `classify_tx`. Both axes that justified the full escalation are addressed
                    // at the root: (a) the walk is off the frame thread; (b) the pool balloon is
                    // bounded by S2.5f freed-handle recycling + full window reclaim + the
                    // dispatch-side pool ceiling self-heal (which reads Partial index_update
                    // allocations too). Bricks OUTSIDE the window keep tags + handles + pool
                    // content byte-untouched by construction — nothing outside the dirty union
                    // changed, so the frozen index there is still valid by definition.
                    // ABOVE 1/2 of the grid the full classify stays cheaper and simpler
                    // (a wholesale index replace also resets the `allocated` watermark), and
                    // every staleness-doubt case (bounds change, res change, structural mark,
                    // scroll fallback, atlas identity mismatch at dispatch) already routes FULL
                    // by construction — this arm is only reachable from a bounds/res-matched
                    // frozen-partial candidate.
                    // NO cooldown here: the in_flight gate serializes flights per body (ticks
                    // during a flight defer via the coalescer), which is the natural rate limit;
                    // the ESCALATED_FULL_MIN_INTERVAL cooldown stays on the full arm only. The
                    // stamp below still records activity so `sparse_quiet` (settle-trim damping)
                    // and the PT gesture heuristics keep seeing the gesture.
                    let windowed_ok = (window_too_large || walk_over_budget)
                        && window_bricks * 2 <= total_bricks;
                    if windowed_ok {
                        self.sparse_escalate_last.insert(body_nid, Instant::now());
                        let why = if window_too_large {
                            format!("window >1/8 of the grid at {res}³")
                        } else {
                            format!("est frame-thread walk {est_walk_ms:.1}ms > \
                                     {SPARSE_WALK_FRAME_BUDGET_MS}ms budget at {res}³")
                        };
                        self.note_sparse_escalation(body_nid, &format!(
                            "mode=windowed {why} \
                             ([{dil_min:?}..{dil_max:?}) = {window_bricks}/{total_bricks} bricks) \
                             — reclassify only the union window on a worker"));
                        // The windowed mode-2 bake covers the dilated window (the worker widens
                        // it further for appended bricks before landing).
                        voxel_bake_min = dil_min;
                        voxel_bake_max = dil_max;
                        // A pending scroll's index MUST still ship — consume the flag and force
                        // the worker's index_update (same contract as the frame-thread partial).
                        let scrolled_force = self.sparse_scrolled.remove(&body_nid);
                        sparse_windowed = Some(SparseWindowedSpawn {
                            frozen: frozen.clone(),
                            marker,
                            dil_min,
                            dil_max,
                            scrolled_force,
                        });
                        None
                    } else if window_too_large {
                        // GESTURE-COALESCED ESCALATION (2026-07-16 live attribution): during a
                        // continuous gesture every landed escalated-full is immediately stale —
                        // the next tick's window is oversized again — so one scale gesture at
                        // 1024³ ran SIX full classify+bake+pool-rebuild+mesh pipelines
                        // back-to-back (owner log: 20-70ms frame-thread spikes each). Bound the
                        // RATE structurally: at most one oversized-window escalation per body per
                        // ESCALATED_FULL_MIN_INTERVAL. Inside the cooldown the tick DEFERS
                        // exactly like the in-flight coalesce above — regions restored, dispatch
                        // state unwound, body re-armed via deferred_bodies — and the drawn mesh
                        // lags the field for at most the cooldown (identical UX to a classify
                        // flight; the settle full at gesture end is authoritative). The Err(inc)
                        // CORRUPTION arm below is NEVER deferred: a corrupt frozen index cannot
                        // serve partials at all.
                        if self.escalate_cooling(body_nid) {
                            // Memo the decision so the next cooling frame skips at the top of the
                            // loop instead of redoing this whole walk.
                            self.sparse_escalate_deferred.insert(body_nid);
                            if let Some(regions) = partial_regions {
                                self.dirty_regions.insert(body_nid, regions);
                            }
                            self.in_flight.remove(&(body_nid, source_hash));
                            self.bake_start_times.remove(&body_nid);
                            // Unwind the generation bump too (the RAM-hit precedent below):
                            // a stranded +1 per deferred tick makes every legitimately
                            // in-flight landing compare stale and get DROPPED — the settle
                            // full then never records and the entry-less settle loop feeds.
                            if let Some(gen) = self.generations.get_mut(&body_nid) {
                                *gen = gen.saturating_sub(1);
                            }
                            deferred_bodies.push(body_nid);
                            crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(),
                                "sparse.escalate_deferred", || format!(
                                    "body={:?} window=[{:?}..{:?}) — cooldown", body_nid, dil_min, dil_max));
                            continue;
                        }
                        self.sparse_escalate_last.insert(body_nid, Instant::now());
                        self.sparse_escalate_deferred.remove(&body_nid); // a real escalation supersedes the memo
                        // ESCALATE to the async FULL classify — same bookkeeping as the
                        // Err(inc) corruption arm below (full box + drop scroll flag).
                        self.note_sparse_escalation(body_nid, &format!(
                            "mode=full window >1/2 of the grid at {res}³ \
                             ([{dil_min:?}..{dil_max:?}) = {window_bricks}/{total_bricks} bricks) — \
                             a wholesale index replace is cheaper and resets the pool watermark"));
                        voxel_bake_min = [0, 0, 0];
                        voxel_bake_max = res3;
                        self.sparse_scrolled.remove(&body_nid);
                        sparse_full_async = true;
                        // fall through with sparse = None (the push site spawns the classify)
                        None
                    } else {
                    // S2.5b: mini-classify the window's sentinel bricks with the MOVED prims
                    // (handles are skipped inside the check — they're compatible by table).
                    // SCOPE-AWARE: attach the body subtree's field program so the mini-classify
                    // samples the SAME nested-boolean field the GPU bake writes (a Subtract
                    // sub-body must not classify as solid union geometry). Flat bodies take
                    // the identical fold_prim path — bit-identical verdicts.
                    let clf = core_math::chunked_dc::BrickClassifier::new(
                        &local_prims, mirror_flags, local_mirror_center,
                        grid_min, grid_max, res, SPARSE_BRICK_DIM, band,
                    ).with_field(core_math::field_eval::BodyField::from_graph(sg, body_nid));
                    // S2.5c — INCREMENTAL ALLOCATION replaces escalation for the common case.
                    // A fast drag outruns the classifier's conservative margin every frame, so
                    // needs-Surface bricks land on EXTERIOR sentinels constantly; a sentinel is
                    // pure compression, so those bricks get APPENDED pool handles (+ the Surface
                    // 26-ring apron) and the windowed bake writes their truth — no full
                    // re-classify, no full-box dispatch. The updated index travels on the
                    // request (installed into `sparse_frozen` only at COMPLETION, so a dropped
                    // bake can't leave the CPU index claiming unbaked handles). Only an
                    // out-of-range index read (corruption) still escalates to a full.
                    // S2.5f: the clone carries `freed` too — reclaimed handles recycle across
                    // frames (each partial pops its predecessor's vacated handles first), so the
                    // pool stops growing during drags instead of leaking one brick per vacated
                    // surface cell (owner-measured 53k→127k bricks in one drag → mid-drag
                    // ensure_capacity growths + atlas generation churn).
                    let mut updated = (*frozen).clone();
                    match core_math::chunked_dc::sparse_window_allocate_incompatible(
                        &mut updated, SPARSE_BRICK_DIM, dil_min, dil_max, &clf)
                    {
                        Ok(alloc) => {
                            // [TriggerBake] attribution: the walk is the partial arm's only
                            // frame-thread hotspot — surface its phase split whenever it is
                            // slow enough to matter, so an owner log can tell walk cost from
                            // the rest of the trigger (serialize / classifier build / field).
                            if alloc.classify_ms + alloc.apply_ms > 2.0 {
                                eprintln!(
                                    "[SparseWalk] body={:?} classify={:.1}ms apply={:.1}ms window=[{:?}..{:?})",
                                    body_nid, alloc.classify_ms, alloc.apply_ms, dil_min, dil_max);
                            }
                            // PARTIAL: bake the (dilated) window — plus every newly allocated
                            // brick (its pool content is garbage until this bake writes it —
                            // recycled handles hold the VACATED brick's stale field; the
                            // Surface apron ring can extend ONE brick beyond the window).
                            // Re-baking a handled brick whose field is unchanged is a
                            // bit-identical write (S2.3), so the widened box is side-effect free.
                            voxel_bake_min = dil_min;
                            voxel_bake_max = dil_max;
                            // Retag-only / reclaim-only frames (no new pool bricks) still ship
                            // the updated index — a stale `+far` tag inside now-solid space
                            // plants a phantom wall, and a reclaimed brick's tag must flip to
                            // its sentinel before the handle can ever be reused.
                            // P1 scroll: a translated index MUST ship even if the window-allocate
                            // changed nothing — the GPU atlas still holds the pre-scroll index
                            // (handles at their old brick coords). Consuming the flag here also
                            // bounds it to this dispatch (set and cleared in the same call).
                            let scrolled_force = self.sparse_scrolled.remove(&body_nid);
                            let index_update = if alloc.appended > 0 || alloc.retagged > 0
                                || alloc.reclaimed > 0 || scrolled_force
                            {
                                if alloc.appended > 0 {
                                    for a in 0..3 {
                                        voxel_bake_min[a] = voxel_bake_min[a]
                                            .min(alloc.new_bricks_min[a] * SPARSE_BRICK_DIM);
                                        voxel_bake_max[a] = voxel_bake_max[a]
                                            .max(((alloc.new_bricks_max[a] + 1) * SPARSE_BRICK_DIM).min(res3[a]));
                                    }
                                }
                                if cfg!(debug_assertions) {
                                    eprintln!(
                                        "[SparseAlloc] body={:?} +{} bricks (reused={}) retag={} reclaim={} (pool → {}) window=[{:?}..{:?})",
                                        body_nid, alloc.appended, alloc.reused, alloc.retagged,
                                        alloc.reclaimed, updated.allocated,
                                        voxel_bake_min, voxel_bake_max);
                                }
                                Some(std::sync::Arc::new(updated))
                            } else {
                                None
                            };
                            Some(SparseBakeRequest::Partial {
                                dims_bricks: frozen.dims_bricks,
                                atlas_generation: marker,
                                index_update,
                            })
                        }
                        Err(inc) => {
                            // ESCALATE (corruption only): an out-of-range index read means the
                            // frozen index cannot be trusted at all → full re-classify + full-box
                            // dispatch. The reason feeds the [SparseEscalate] telemetry.
                            self.note_sparse_escalation(body_nid, &format!(
                                "brick=({},{},{}) needs={:?} frozen_tag={} window=[{:?}..{:?})",
                                inc.brick[0], inc.brick[1], inc.brick[2],
                                inc.needed, inc.tag_str(), dil_min, dil_max));
                            voxel_bake_min = [0, 0, 0];
                            voxel_bake_max = res3;
                            // P1: a same-dispatch scroll flag dies here — the full re-classify
                            // replaces + re-uploads the whole index.
                            self.sparse_scrolled.remove(&body_nid);
                            sparse_full_async = true; // classify on the worker; push site spawns
                            None
                        }
                    }
                    } // end !window_too_large (the frozen-index partial walk)
                } else {
                    // S2.4 FULL sparse bake (first bake / structural / settle / bounds change):
                    // regions (if any) were not partial-eligible; the window is already full-box.
                    debug_assert_eq!((voxel_bake_min, voxel_bake_max), ([0, 0, 0], res3),
                        "sparse FULL bake must cover the whole grid (window comes from regions only \
                         when a frozen index exists)");
                    self.sparse_scrolled.remove(&body_nid); // P1: full classify supersedes a scroll
                    sparse_full_async = true; // classify on the worker; push site spawns
                    None
                }
            };

            // GATE 2 — NO dirty-region stamp at dispatch. The single `DirtyMeshRegion` is
            // produced at bake ACCEPTANCE only (the window travels with the request/pending as
            // `bake_min/max` → `bake_window`), so a superseded/dropped bake never leaves a claim.

            // Store debug region for overlay visualization.
            {
                let dx = if res3[0] > 1 { (res3[0] - 1) as f32 } else { 1.0 };
                let dy = if res3[1] > 1 { (res3[1] - 1) as f32 } else { 1.0 };
                let dz = if res3[2] > 1 { (res3[2] - 1) as f32 } else { 1.0 };
                let extent = grid_max - grid_min;
                let dbg_local_min = grid_min + Vec3::new(
                    voxel_bake_min[0] as f32 / dx * extent.x,
                    voxel_bake_min[1] as f32 / dy * extent.y,
                    voxel_bake_min[2] as f32 / dz * extent.z,
                );
                let dbg_local_max = grid_min + Vec3::new(
                    voxel_bake_max[0] as f32 / dx * extent.x,
                    voxel_bake_max[1] as f32 / dy * extent.y,
                    voxel_bake_max[2] as f32 / dz * extent.z,
                );
                let is_partial = has_existing_grid
                    || matches!(sparse, Some(SparseBakeRequest::Partial { .. }))
                    || sparse_windowed.is_some(); // windowed reclassify lands as a Partial
                self.debug_last_regions.push((body_nid, dbg_local_min, dbg_local_max, is_partial));
                self.debug_region_age = 0;
            }

            // GPU dispatch path: route to worker GPU if available.
            // S2.5: sparse requests NEVER go to a worker. A worker bake is dense GridBacked with
            // its own output_buf — a windowed sparse PARTIAL routed there would bake only the
            // window into a fresh (unseeded — the entry's grid is empty) buffer and accept it as
            // a dense grid = corruption; a sparse FULL routed there would silently drop the
            // regime. (S2.4 already documented workers as excluded from sparse by construction.)
            // P1-#7: `sparse_full_async` bodies carry sparse=None only as a PLACEHOLDER (the
            // worker fills it) — they are sparse bakes and must never route to a worker GPU.
            // Same for `sparse_windowed` (the window-walk worker completes the Partial payload).
            // P3: multi-mod stack cannot ride worker GPU bake (same single-slot shader). Fall
            // through to primary_bake_requests with force_cpu_mod_stack instead.
            let force_cpu_mod = crate::bake_utils::body_needs_cpu_full_stack_bake(sg, body_nid);
            if use_gpu && sparse.is_none() && !sparse_full_async && sparse_windowed.is_none() && !force_cpu_mod {
                let mut prim_floats = graphics::serialize_body_for_bake_hot(
                    sg, body_nid,
                    if BODY_LOCAL_PRIMS { Vec3::ZERO } else { body_origin },
                );
                let clip_curve_floats = graphics::serialize_clip_curves_for_bake(sg, body_nid, &mut prim_floats);
                // Worker GPUs do NOT carry per-prim MeshVolume grids (spike): they bind the
                // disabled dummy at binding 5, so a MeshVolume prim contributes nothing on a
                // worker-baked body. Acceptable — the primary GPU is the production path (no
                // workers on single-GPU machines). Documented in the lab log.
                let job = GpuJob {
                    kind: GpuJobKind::BodyVolumeBake {
                        body_nid,
                        prim_floats,
                        clip_curve_floats,
                        resolution: res3,
                        bounds_min: grid_min,
                        bounds_max: grid_max,
                        mirror_flags,
                        mirror_sign,
                        mirror_center: local_mirror_center,
                        bevel_radius,
                        bake_min: voxel_bake_min,
                        bake_max: voxel_bake_max,
                        // Worker GPUs use the GridBacked output_buf (no path A), so they DO need the seed —
                        // clone it here (only when a worker is actually dispatched, which is rare).
                        base_grid: if has_existing_grid {
                            // base_grid is an owned Vec seed for the worker's GridBacked output_buf, so
                            // deref-clone the inner Vec out of the Arc (a real copy — unchanged from before;
                            // only the rare worker-dispatch path pays it).
                            self.body_grids.get(&body_nid).map(|e| (*e.grid).clone())
                        } else { None },
                    },
                    generation,
                    source_hash,
                    target: DeviceTarget::AnyWorker,
                };
                if let Some(d) = dispatcher.as_deref_mut() {
                    if d.dispatch(job) {
                        continue; // Successfully dispatched to worker GPU
                    }
                }
                // Dispatch failed (no workers) — fall through to rayon
            }

            // Primary GPU bake (Level 2): dispatch on the render device.
            // Falls back to CPU rayon if primary pipeline isn't ready yet.
            let mut prim_floats = graphics::serialize_body_for_bake_hot(
                sg, body_nid,
                if BODY_LOCAL_PRIMS { Vec3::ZERO } else { body_origin },
            );
            let clip_curve_floats = graphics::serialize_clip_curves_for_bake(sg, body_nid, &mut prim_floats);
            let mesh_grid_floats = graphics::serialize_mesh_grids_for_bake(sg, body_nid, &mut prim_floats);
            // T3: draft bake packs coarse height mips; settle/full packs native resolution.
            let hm_max = if is_draft {
                core_math::modifiers::DRAFT_HEIGHT_MIP_MAX_SIDE
            } else {
                0
            };
            let height_map_floats =
                graphics::serialize_height_maps_for_bake(sg, body_nid, &mut prim_floats, hm_max);
            let request = PrimaryBakeRequest {
                body_nid,
                prim_floats,
                resolution: res3,
                bounds_min: grid_min,
                bounds_max: grid_max,
                bake_min: voxel_bake_min,
                bake_max: voxel_bake_max,
                mirror_flags: mirror_flags as u32,
                mirror_sign: mirror_sign as u32,
                mirror_center: local_mirror_center,
                bevel_radius,
                source_hash,
                template_hash,
                generation,
                // Deferred: the primary GPU path A bakes in place (no seed). If the rare non-resident path
                // is taken at dispatch, it materializes the seed from body_grids then (app_poll). This skips
                // the 64MB clone on every partial edit bake.
                base_grid: None,
                clip_curve_floats,
                mesh_grid_floats,
                height_map_floats,
                sparse,
                // O1: the coarse cull is sound only for provably 1-Lipschitz folds — computed
                // from the SAME dispatch-time prim snapshot the classify will run against.
                coarse_cull_ok: core_math::primitives::SdfPrimitive::coarse_cull_safe(&local_prims),
                // REUSE the value computed at :1455 — this used to call
                // `body_needs_cpu_full_stack_bake(sg, body_nid)` a SECOND time with identical
                // arguments in the same frame. That helper clones the whole prim subtree
                // (`collect_body_prims_recursive`) only to read `p.modifier` / `p.modifiers`, and
                // `sg` is not mutated anywhere between the two call sites. On the partial-bake path
                // this runs every drag frame (the 40ms debounce is deliberately bypassed when dirty
                // regions exist), so the duplicate was a per-frame full-subtree deep clone for a
                // value already in scope.
                force_cpu_mod_stack: force_cpu_mod,
            };
            if let Some(w) = sparse_windowed {
                // WINDOWED RECLASSIFY: the S2.5b/c window walk runs on a background worker
                // against THIS request's exact prim snapshot (dispatch-time-provenance, same
                // contract as the full-classify arm below) and lands the completed Partial via
                // `poll_async_classifies`. The in_flight entry inserted above keeps the
                // coalescer deferring this body until the landing.
                let field = core_math::field_eval::BodyField::from_graph(sg, body_nid);
                self.spawn_async_window_reclassify(request, local_prims, field, w);
            } else if sparse_full_async {
                // P1-#7: the FULL classify runs on a background worker against THIS request's
                // exact prim snapshot (dispatch-time-provenance: `local_prims` is the same
                // collection `prim_floats` was serialized from this iteration). The request
                // lands via `poll_async_classifies`; until then the in_flight entry inserted
                // above keeps the coalescer deferring this body.
                // Scope-aware field snapshot travels with the request — the classify worker
                // has no graph access, and the field must match the serialized prim_floats
                // (same dispatch-time provenance as `local_prims`).
                let field = core_math::field_eval::BodyField::from_graph(sg, body_nid);
                self.spawn_async_classify(request, local_prims, field);
            } else {
                self.primary_bake_requests.push(request);
            }
        }

        self.dirty_bodies.clear();
        // Re-insert bodies that were deferred due to in-flight coalescing.
        // Their dirty_regions are intact (not consumed) and will be picked up
        // by the next trigger_bake cycle after the in-flight bake completes.
        for nid in deferred_bodies {
            self.dirty_bodies.insert(nid);
        }
        // NOTE: dirty_regions are consumed per-body by dirty_regions.remove()
        // inside the loop. Do NOT blanket-clear here — bodies that were skipped
        // (in-flight, empty prims) may still have regions that should be picked
        // up on the next trigger_bake call.
        let _trigger_ms = _t_trigger.elapsed().as_secs_f64() * 1000.0;
        if _trigger_ms > 5.0 {
            eprintln!("[TriggerBake] {:.1}ms dirty={} sync_baked={}", _trigger_ms, _dirty_count, sync_baked.len());
        }
        sync_baked
    }
}

/// Classifier band shared by the async FULL classify, the S2.5b window mini-classify and the
/// frozen-partial allocate walk — `2.0 * cell_world`, the slice-1 pre-pass convention. ONE
/// formula: a consumer with a divergent band drifts from the frozen index's own predicate.
pub(crate) fn sparse_classify_band(grid_min: Vec3, grid_max: Vec3, res: u32) -> f32 {
    let cell_world = ((grid_max - grid_min) / (res - 1).max(1) as f32)
        .min_element()
        .max(1e-6);
    2.0 * cell_world
}

impl BodyVolumeCache {
    /// P1-#7 ASYNC FULL CLASSIFY — run the 38-45ms full brick classify on a background thread.
    /// `request` is COMPLETE except `sparse` (None placeholder); the worker classifies the prim
    /// SNAPSHOT it carries (dispatch-time-provenance: the same prims `prim_floats` was
    /// serialized from) and sends the finished request to `classify_rx`. The caller (trigger)
    /// already bumped the generation + inserted `in_flight` exactly like a sync dispatch, so
    /// the coalescer defers this body until the landing pushes (or drops) the request. A
    /// panicked/killed worker never lands — the 10s bake timeout re-dirties the body.
    fn spawn_async_classify(
        &mut self,
        request: PrimaryBakeRequest,
        local_prims: Vec<core_math::primitives::SdfPrimitive>,
        field: core_math::field_eval::BodyField,
    ) {
        debug_assert!(request.sparse.is_none(), "spawn completes the sparse payload itself");
        debug_assert_eq!((request.bake_min, request.bake_max), ([0, 0, 0], request.resolution),
            "async classify is FULL-arm only (full-box window)");
        self.classify_pending += 1;
        // STAGE-2 GPU CLASSIFY: with a GPU present, the verdict pass runs as the
        // `classify_bricks` compute entry instead of the CPU worker — queue the record here;
        // `AzraApp::dispatch_pending_gpu_classifies` (poll_background_tasks, Frame Ordering
        // Contract) turns it into the real dispatch, and the verdict readback finalizes +
        // lands through the SAME `classify_tx` channel. The in_flight/generation semantics
        // are identical in both regimes. No device (headless / cache-only tests) → the CPU
        // worker path below, unchanged.
        if self.gpu_classify_ready {
            self.pending_gpu_classify.push(super::QueuedGpuClassify { request, local_prims, field });
            return;
        }
        self.spawn_classify_worker(request, local_prims, field);
    }

    /// WINDOWED ESCALATED RECLASSIFY (2026-07-16) — the oversized-window escalation for windows
    /// in (1/8, 1/2] of the grid's bricks. Runs the EXACT frozen-partial machinery — the S2.5b
    /// mini-classify + S2.5c/f incremental allocate/retag/reclaim walk
    /// (`sparse_window_allocate_incompatible`) — on a background worker instead of the frame
    /// thread, and lands the result as an ordinary `SparseBakeRequest::Partial` through
    /// `classify_tx` (ONE landing point; `poll_async_classifies` applies the same generation +
    /// regime validation as a full classify, and the dispatch-side atlas identity/generation
    /// check still guards the write). Correctness is the S2.5 partial contract, not new math:
    ///   - Bricks OUTSIDE the window keep tags + handles + pool content byte-untouched (the
    ///     walk visits only window bricks; the windowed mode-2 bake writes only the window).
    ///   - Bricks INSIDE the window alloc/retag/reclaim exactly as a frame-thread partial —
    ///     the free list (`freed`, S2.5f) travels inside the cloned index, so the pool
    ///     accounting is ONE code path shared with the nudge partial.
    ///   - The B1 modifier-pad containment contract holds unchanged (same `BrickClassifier`).
    /// Corruption (out-of-range index read — the frozen index cannot be trusted) falls back to
    /// the FULL classify ON THE SAME WORKER (same [SparseFull] contract as
    /// `spawn_classify_worker`). No cooldown: the in_flight gate serializes flights per body.
    fn spawn_async_window_reclassify(
        &mut self,
        mut request: PrimaryBakeRequest,
        local_prims: Vec<core_math::primitives::SdfPrimitive>,
        field: core_math::field_eval::BodyField,
        w: SparseWindowedSpawn,
    ) {
        debug_assert!(request.sparse.is_none(), "the worker completes the sparse payload");
        debug_assert!((0..3).all(|a| w.dil_max[a] <= request.resolution[a]
            && w.dil_min[a] < w.dil_max[a]), "windowed reclassify needs a non-empty sub-window");
        self.classify_pending += 1;
        let tx = self.classify_tx.clone();
        std::thread::spawn(move || {
            let res = request.resolution[0]; // cubic — sparse_eligible enforces it at trigger
            let band = sparse_classify_band(request.bounds_min, request.bounds_max, res);
            // The SAME classifier construction as the frame-thread partial walk (scope-aware:
            // the field program samples nested sub-body boolean trees exactly as the bake).
            let clf = core_math::chunked_dc::BrickClassifier::new(
                &local_prims, request.mirror_flags as u8, request.mirror_center,
                request.bounds_min, request.bounds_max, res, SPARSE_BRICK_DIM, band,
            ).with_field(field);
            let mut updated = (*w.frozen).clone();
            match core_math::chunked_dc::sparse_window_allocate_incompatible(
                &mut updated, SPARSE_BRICK_DIM, w.dil_min, w.dil_max, &clf)
            {
                Ok(alloc) => {
                    // Widen the bake window to cover every appended brick — its pool content is
                    // garbage until the windowed bake writes it (recycled handles hold the
                    // VACATED brick's stale field; the Surface apron ring can extend one brick
                    // beyond the window). Identical to the frame-thread partial arm.
                    let mut bmin = w.dil_min;
                    let mut bmax = w.dil_max;
                    if alloc.appended > 0 {
                        for a in 0..3 {
                            bmin[a] = bmin[a].min(alloc.new_bricks_min[a] * SPARSE_BRICK_DIM);
                            bmax[a] = bmax[a].max(((alloc.new_bricks_max[a] + 1) * SPARSE_BRICK_DIM)
                                .min(request.resolution[a]));
                        }
                    }
                    request.bake_min = bmin;
                    request.bake_max = bmax;
                    let ship_index = alloc.appended > 0 || alloc.retagged > 0
                        || alloc.reclaimed > 0 || w.scrolled_force;
                    // The windowed-vs-full visibility line (sibling of [SparseFull] /
                    // [GpuClassify]): one per windowed escalation, with the walk's phase split.
                    eprintln!(
                        "[SparseWindowed] body={:?} classify={:.1}ms apply={:.1}ms \
                         window=[{:?}..{:?}) +{} (reused={}) retag={} reclaim={} pool={} \
                         index_ship={} (async — off the frame thread)",
                        request.body_nid, alloc.classify_ms, alloc.apply_ms, bmin, bmax,
                        alloc.appended, alloc.reused, alloc.retagged, alloc.reclaimed,
                        updated.allocated, ship_index);
                    request.sparse = Some(SparseBakeRequest::Partial {
                        dims_bricks: w.frozen.dims_bricks,
                        atlas_generation: w.marker,
                        index_update: if ship_index {
                            Some(std::sync::Arc::new(updated))
                        } else {
                            None
                        },
                    });
                    let _ = tx.send(request); // receiver dropped only on cache teardown
                }
                Err(inc) => {
                    // Corruption: the frozen index cannot serve partials at all — full
                    // re-classify on this same worker (the request stays ONE flight).
                    eprintln!(
                        "[SparseWindowed] body={:?} frozen index corrupt at brick=({},{},{}) \
                         needs={:?} tag={} — falling back to a FULL classify on this worker",
                        request.body_nid, inc.brick[0], inc.brick[1], inc.brick[2],
                        inc.needed, inc.tag_str());
                    request.bake_min = [0; 3];
                    request.bake_max = request.resolution;
                    let t = std::time::Instant::now();
                    let si = core_math::chunked_dc::classify_with_classifier(&clf);
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    eprintln!(
                        "[SparseFull] body={:?} classify={ms:.1}ms bricks={}/{} prims={} \
                         (windowed-escalation corruption fallback)",
                        request.body_nid, si.allocated, si.index.len(), local_prims.len());
                    request.sparse = Some(SparseBakeRequest::Full(si));
                    let _ = tx.send(request);
                }
            }
        });
    }

    /// The CPU classify WORKER (P1-#7): classify the prim snapshot on a background thread and
    /// send the completed request through `classify_tx`. Shared by the no-GPU regime of
    /// `spawn_async_classify` and the no-pipeline fallback of the GPU classify dispatch — the
    /// caller has ALREADY counted the flight in `classify_pending` (exactly once per request).
    pub(crate) fn spawn_classify_worker(
        &self,
        mut request: PrimaryBakeRequest,
        local_prims: Vec<core_math::primitives::SdfPrimitive>,
        field: core_math::field_eval::BodyField,
    ) {
        let tx = self.classify_tx.clone();
        std::thread::spawn(move || {
            let res = request.resolution[0]; // cubic — sparse_eligible enforces it at trigger
            let band = sparse_classify_band(request.bounds_min, request.bounds_max, res);
            let t = std::time::Instant::now();
            // SCOPE-AWARE classify: spheres from the flat prim snapshot (bounds), 9-eval from
            // the field program (nested sub-body boolean trees, same field the bake writes).
            let clf = core_math::chunked_dc::BrickClassifier::new(
                &local_prims, request.mirror_flags as u8, request.mirror_center,
                request.bounds_min, request.bounds_max, res, SPARSE_BRICK_DIM, band,
            ).with_field(field);
            let si = core_math::chunked_dc::classify_with_classifier(&clf);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms > 15.0 {
                eprintln!("[SparseFull] body={:?} classify={ms:.1}ms bricks={}/{} prims={} (async — off the frame thread)",
                    request.body_nid, si.allocated, si.index.len(), local_prims.len());
            }
            request.sparse = Some(SparseBakeRequest::Full(si));
            let _ = tx.send(request); // receiver dropped only on cache teardown
        });
    }

    /// Land finished async classifies into `primary_bake_requests`. Called once per frame
    /// (app_poll, immediately before the dispatch drain) so a landed classify bakes the same
    /// frame. Returns the number landed.
    ///
    /// Validation: the spawn-time generation must still be the body's CURRENT generation. The
    /// in_flight gate guarantees trigger dispatched nothing for this body mid-flight, so a
    /// mismatch means undo (bump-all), body removal, or invalidate_all — exactly the events
    /// that make the snapshot's index stale. The sparse-regime re-check covers a mid-flight
    /// toggle / Edit-Mode flip (a sparse request dispatched into a dense regime would skip the
    /// atlas write). Dropped results clear the gate and re-dirty a still-tracked body so the
    /// next trigger re-evaluates with fresh state.
    pub(crate) fn poll_async_classifies(&mut self) -> usize {
        let mut landed = 0;
        while let Ok(req) = self.classify_rx.try_recv() {
            self.classify_pending = self.classify_pending.saturating_sub(1);
            let body = req.body_nid;
            let current = self.generations.get(&body).copied();
            let regime_ok = self.sparse_bake_eligible(req.resolution[0]);
            if current != Some(req.generation) || !regime_ok {
                self.in_flight.retain(|(bid, _)| *bid != body);
                self.bake_start_times.remove(&body);
                if current.is_some() {
                    self.dirty_bodies.insert(body); // still tracked — re-bake from fresh state
                }
                continue;
            }
            self.primary_bake_requests.push(req);
            landed += 1;
        }
        landed
    }

    /// STAGE-2 GPU CLASSIFY: hand a request whose sparse payload was completed by the GPU
    /// verdict readback to the ONE landing point — the same `classify_tx` channel the CPU
    /// worker uses, so `poll_async_classifies` applies identical generation/regime validation
    /// to both regimes.
    pub(crate) fn submit_classified_request(&self, request: PrimaryBakeRequest) {
        debug_assert!(request.sparse.is_some(), "submit carries a COMPLETED sparse payload");
        let _ = self.classify_tx.send(request); // receiver dropped only on cache teardown
    }

    /// STAGE-2 GPU CLASSIFY: sender clone for the GPU-path finalize worker thread (same
    /// channel/contract as the CPU classify worker's own clone in `spawn_classify_worker`).
    pub(crate) fn classify_sender(&self) -> std::sync::mpsc::Sender<PrimaryBakeRequest> {
        self.classify_tx.clone()
    }

    /// STAGE-2 GPU CLASSIFY: a queued/in-flight GPU classify died before it could land (map
    /// error, device gone, corrupt verdict buffer). Un-count the flight and recover exactly
    /// like a stale landing in `poll_async_classifies`: clear the gate (else the coalescer
    /// defers the body forever) and re-dirty a still-tracked body so the next trigger
    /// re-classifies from fresh state (which falls back to the CPU worker if the GPU is gone).
    pub(crate) fn abort_async_classify(&mut self, body: NodeId) {
        self.classify_pending = self.classify_pending.saturating_sub(1);
        self.in_flight.retain(|(bid, _)| *bid != body);
        self.bake_start_times.remove(&body);
        if self.generations.contains_key(&body) {
            self.dirty_bodies.insert(body);
        }
        self.clear_bake_debounce(); // recovery is a one-shot event, not a rapid-edit stream
    }

    /// Test pump: block until every spawned classify has landed (or panic on timeout).
    /// Production never calls this — the per-frame `poll_async_classifies` is non-blocking.
    #[cfg(test)]
    pub(crate) fn wait_async_classifies(&mut self, timeout: std::time::Duration) {
        let t0 = Instant::now();
        while self.classify_pending > 0 && t0.elapsed() < timeout {
            if self.poll_async_classifies() == 0 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        assert_eq!(self.classify_pending, 0, "async classify did not land within {timeout:?}");
    }

    /// P1-#6b rigid-translate fast path — CONSUME the body's `RigidAccum` and return the rigid
    /// delta if this dispatch's accumulated edits are a pure translation of the WHOLE body:
    /// not poisoned, every visible prim present with the SAME summed delta, no body-anchored
    /// field feature (mirror — its plane stays while the prims move, so the reflected half
    /// moves opposite), and the entry is a brick-resident sparse grid at the current res (the
    /// mesh re-anchor offset is wired for the resident slot path only). The accum is removed
    /// unconditionally — a refused burst falls through to the normal partial/scroll/full
    /// machinery THIS dispatch, whose bake covers the same regions.
    fn take_rigid_translate(
        &mut self,
        body_nid: NodeId,
        local_prims: &[core_math::primitives::SdfPrimitive],
        mirror_flags: u8,
        target_res: u32,
    ) -> Option<Vec3> {
        let acc = self.rigid_accum.remove(&body_nid)?;
        if acc.poisoned { return None; }
        if mirror_flags != 0 { return None; }
        if local_prims.is_empty() { return None; }
        // Rigid ⟺ EVERY visible prim moved by the SAME delta (local_prims is the visible set —
        // invisible prims contribute nothing to the field, so they may stay put).
        if acc.deltas.len() != local_prims.len() { return None; }
        let mut delta: Option<Vec3> = None;
        for p in local_prims {
            let d = *acc.deltas.get(&p.prim_uid)?;
            match delta {
                None => delta = Some(d),
                Some(d0) => { if (d - d0).length_squared() > 1e-10 { return None; } }
            }
        }
        let delta = delta?;
        if delta.length_squared() < 1e-12 { return None; }
        let entry = self.body_grids.get(&body_nid)?;
        entry.brick_resident?;
        if entry.sparse_frozen.is_none() || entry.bake_resolution != target_res { return None; }
        if !self.sparse_bake_eligible(target_res) { return None; }
        Some(delta)
    }

    /// P1 scrolling-grid escape — attempt a brick-aligned window TRANSLATE instead of the
    /// grow-full escape. For a brick-resident sparse body whose tight bounds fit inside the
    /// current window extent, moving the window by an INTEGER number of bricks keeps every
    /// retained brick's world-space voxel positions unchanged — its pool content stays valid
    /// (sentinels are per-brick constants; handle data is world-anchored field), so the escape
    /// costs one slab classify + slab partial bake instead of a whole-grid full classify+bake
    /// (owner-measured 57-110ms per escape at 512³). Extent never changes on a scroll, which
    /// also keeps the body's sampling density constant across arbitrarily long drags (the grow
    /// policy monotonically dilutes fixed voxels over a growing extent until the cap).
    ///
    /// Why this does NOT reintroduce the reverted re-centering dead-end
    /// (`compute_escape_grid_bounds` doc): that attempt re-anchored the DENSE grid on the moving
    /// body and paid a FULL bake + whole-texture remap per escape, firing every frame. Here
    /// (a) the anchor moves only when the escape predicate fires AND lands with ≥ one headroom
    /// of slack on every granted side — re-fire cadence is bounded to ~ESCAPE_HEADROOM of body
    /// travel, exactly like the grow policy's grant; (b) a fire costs a slab partial against
    /// retained content, not a full; (c) there is no texture remap — the mesh comes from the DC
    /// over world-anchored brick data.
    ///
    /// Returns the scrolled window bounds after COMMITTING the translate:
    /// - `entry.sparse_frozen` = the translated index, `entry.grid_min/max` = the new window —
    ///   installed ATOMICALLY here (bounds and index are meaningless apart). Pre-installing the
    ///   translate does not violate the S2.5c install-at-completion invariant's purpose: it
    ///   claims only RETAINED (already-baked) content; new handles still travel on the request.
    /// - `body_bounds` moved to the new window.
    /// - the exposed slab(s) pushed as ordinary dirty regions (body-local, same space as every
    ///   `BakeDirtyRegion`), so the existing frozen-index partial arm classifies + allocates +
    ///   bakes them with ZERO scroll-specific machinery downstream.
    /// - the body marked in `sparse_scrolled`, forcing the partial arm to ship an
    ///   `index_update` (the GPU atlas still holds the pre-scroll index).
    ///
    /// Returns None (→ caller falls back to the grow-full policy) when: not sparse-eligible /
    /// not brick-resident / entry disagrees with the cached window / res mismatch / both walls
    /// of one axis granted (oscillation or body ≈ window size) / teleport (no window overlap) /
    /// the snapped window can't contain the tight bounds (the body outgrew the window).
    fn try_sparse_scroll(
        &mut self,
        body_nid: NodeId,
        cached: (Vec3, Vec3),
        tight: (Vec3, Vec3),
        prev_tight: Option<(Vec3, Vec3)>,
        target_res: u32,
    ) -> Option<(Vec3, Vec3)> {
        // ---- Regime gates (read-only; mirrors the frozen-partial gate) ----
        if !self.sparse_bake_eligible(target_res) { return None; }
        let entry = self.body_grids.get(&body_nid)?;
        entry.brick_resident?;
        let frozen = entry.sparse_frozen.as_ref()?;
        if entry.bake_resolution != target_res { return None; }
        if (entry.grid_min - cached.0).length_squared() >= 1e-6
            || (entry.grid_max - cached.1).length_squared() >= 1e-6 { return None; }
        let bricks_per_axis = (target_res + SPARSE_BRICK_DIM - 1) / SPARSE_BRICK_DIM;
        if frozen.dims_bricks != [bricks_per_axis; 3] { return None; }

        // ---- Shift decision (pure geometry — the compute_escape_grid_bounds grant logic,
        //      expressed as translation instead of growth) ----
        let ext = cached.1 - cached.0;
        // Sparse bakes are force-cubed GridBacked (res3 = [target_res;3]), so the per-axis cell
        // uses the same scalar res on every axis — identical to the bake/classify mapping.
        let cell = ext / (target_res - 1).max(1) as f32;
        let brick_w = cell * SPARSE_BRICK_DIM as f32;
        let tight_ext = (tight.1 - tight.0).max(Vec3::splat(GRID_PAD_MIN));
        let headroom = tight_ext * ESCAPE_HEADROOM;
        let esc_lo = (cached.0 - tight.0).max(Vec3::ZERO);
        let esc_hi = (tight.1 - cached.1).max(Vec3::ZERO);
        let (dmin, dmax) = prev_tight
            .map(|(pmin, pmax)| (tight.0 - pmin, tight.1 - pmax))
            .unwrap_or((Vec3::ZERO, Vec3::ZERO));
        let slack_lo = tight.0 - cached.0;
        let slack_hi = cached.1 - tight.1;
        let mut k = [0i32; 3];
        for a in 0..3 {
            let bw = brick_w[a].max(1e-9);
            let grant_lo = esc_lo[a] > 0.0 || (dmin[a] < 0.0 && slack_lo[a] < headroom[a]);
            let grant_hi = esc_hi[a] > 0.0 || (dmax[a] > 0.0 && slack_hi[a] < headroom[a]);
            k[a] = match (grant_lo, grant_hi) {
                // Both walls wanted: translation can't serve both sides — grow policy's case.
                (true, true) => return None,
                // Want new_min ≤ tight.0 − headroom: negative shift, floored to whole bricks
                // (floor of a negative = MORE slack on the granted side).
                (true, false) => (((tight.0[a] - headroom[a]) - cached.0[a]) / bw).floor() as i32,
                // Want new_max ≥ tight.1 + headroom: positive shift, ceiled to whole bricks.
                (false, true) => (((tight.1[a] + headroom[a]) - cached.1[a]) / bw).ceil() as i32,
                (false, false) => 0,
            };
        }
        if k == [0, 0, 0] { return None; }
        // Teleport guard: zero window overlap → nothing retained → a fresh full classify at
        // grown bounds is strictly better than translating an index we'd fully discard.
        if (0..3).any(|a| k[a].unsigned_abs() >= bricks_per_axis) { return None; }

        let delta = Vec3::new(
            k[0] as f32 * brick_w.x,
            k[1] as f32 * brick_w.y,
            k[2] as f32 * brick_w.z,
        );
        let new_min = cached.0 + delta;
        let new_max = cached.1 + delta;
        // CONTAINMENT (non-negotiable, same rule as the grow path's final clamp): the scrolled
        // window must fully hold the live tight bounds. Fails when the body outgrew the window
        // (scale drag) or hugs the trailing wall — the grow policy owns those.
        if (0..3).any(|a| new_min[a] > tight.0[a] || new_max[a] < tight.1[a]) { return None; }

        // ---- Commit: index + entry bounds + body_bounds move ATOMICALLY ----
        let shifted = core_math::chunked_dc::sparse_index_translate(frozen, k);
        let freed_delta = shifted.freed.len() - frozen.freed.len();
        let retained = shifted.index.iter()
            .filter(|&&t| t < core_math::chunked_dc::BRICK_TAG_INTERIOR).count();
        let entry = self.body_grids.get_mut(&body_nid).expect("gated on Some above");
        entry.grid_min = new_min;
        entry.grid_max = new_max;
        entry.sparse_frozen = Some(std::sync::Arc::new(shifted));
        self.body_bounds.insert(body_nid, (new_min, new_max));
        self.sparse_scrolled.insert(body_nid);

        // Exposed slab(s) → ordinary dirty regions (body-local — the same space as grid bounds
        // and every BakeDirtyRegion): per shifted axis, the strip of the NEW window the OLD one
        // did not cover; the other axes span the full new window so corner wedges are covered.
        // The frozen-index partial arm's window (regions ∪ prim stamp, brick-dilated) then
        // classifies/allocates/bakes the slab exactly like any dirty window.
        let regions = self.dirty_regions.entry(body_nid).or_default();
        for a in 0..3 {
            if k[a] == 0 { continue; }
            let mut lo = new_min;
            let mut hi = new_max;
            if k[a] > 0 { lo[a] = cached.1[a].max(new_min[a]); } // window moved + → high strip exposed
            else        { hi[a] = cached.0[a].min(new_max[a]); } // window moved − → low strip exposed
            regions.push(super::BakeDirtyRegion { local_min: lo, local_max: hi });
        }

        // Owner-visibility line — one per scroll (an escape-rate event, ~1 per ESCAPE_HEADROOM
        // of drag travel — never per-frame), the [SparseBake]/[SparseAlloc] convention.
        eprintln!(
            "[SparseScroll] body={:?} k=({},{},{}) bricks retained={} freed+{} window {:.3?}..{:.3?}",
            body_nid, k[0], k[1], k[2], retained, freed_delta, new_min, new_max);
        crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "scroll", || format!(
            "body={:?} k={:?} old={:.3?}..{:.3?} new={:.3?}..{:.3?} tight={:.3?}..{:.3?}",
            body_nid, k, cached.0, cached.1, new_min, new_max, tight.0, tight.1));
        Some((new_min, new_max))
    }
}

// ==================================================================
// Escape grid-bounds policy (pure — capture-replay tested below)
// ==================================================================

/// Headroom = slack added on a granted side so a continuous drag does NOT re-escape
/// every ~0.15 body of travel (each escape clears dirty_regions → a whole-grid FULL
/// bake, the "not live" cost). Raised 0.15→0.30 earlier: the MAX_GRID_MULT cap is the
/// balloon guard — repeated escapes push the grid to the cap either way, so average
/// grid size during a drag is unchanged; headroom just reaches the cap in fewer,
/// larger jumps → fewer escapes → fewer full bakes.
const ESCAPE_HEADROOM: f32 = 0.30;
/// HARD CAP on total grid extent per axis (≤1.35× the current tight geometry).
/// TIGHT caps (was 0.5 / 2.0). A grid ballooned to 2×+ the geometry under-samples the
/// body catastrophically at a fixed res: CPU-measured 6.7% inside at 1× (healthy) →
/// 0.4% at 2.5× (a degenerate slab + detached blobs — the owner's flatten/disappear
/// during a drag). Keeping the grid ≤ ~1.35× the CURRENT tight geometry holds sampling
/// healthy (~3% inside) so moving/spreading cubes stays correctly meshed. The settle
/// re-tighten still shrinks to exact tight bounds when editing stops.
const MAX_GRID_MULT: f32 = 1.35;

/// Grown grid bounds for an AABB escape (body's tight bounds left the cached grid).
///
/// Anchored, BOUNDED escape (replaces the old full-extent headroom + 8x velocity +
/// never-shrink inflation that ran the grid away to 3x-10x the body — the coarse
/// "trailing cube" / "large cube enrobing the geo, size = movement", and a realloc
/// every escape). Three intents:
///   (1) grow from the union of cached grid + tight bounds — the world→voxel mapping
///       changes only at escapes, never mid-partial;
///   (2) headroom on the sides the body needs, so it doesn't re-escape next frame
///       (which would re-fire `expanded` every frame = full-bake spam);
///   (3) a HARD CAP on total grid extent so a fast / multi-prim drag can't balloon it
///       (over-sized grid @ fixed voxel count = under-sampled body). The cap is
///       GUARANTEED on any axis with a grant (granted sides end with exactly one
///       headroom of slack, so overflow trims fit). On a no-grant axis with highly
///       asymmetric union slack, the split-trim + final containment rescue can leave
///       that axis above the cap for one escape — legacy-identical corner (the old
///       inline else-branch had the same math); the settle re-tighten reclaims it.
/// CRUX vs the reverted attempt: the cap is anchored to the STABLE `cached` grid and
/// trims the FAR side — it NEVER re-centres on the MOVING body. Re-centering made the
/// anchor chase the subject, so the prim poked outside every frame → `expanded` true
/// every frame → a full bake every frame. `cached` does not move, so once grown the
/// prim stays inside it and the escape stops re-firing.
///
/// PREDICTIVE MULTI-AXIS HEADROOM (2026-07-03, from the [BAKEPROF] drag capture): the
/// legacy policy granted headroom ONLY on sides that already escaped. On a diagonal
/// drag the tight bounds advance on TWO axes, but each escape re-armed only one — the
/// other axis, left within ~0.04 of the grid wall, fired its own whole-grid FULL bake
/// a few frames later. In the captured one-prim drag, 3 of 5 full bakes were these
/// tiny-overshoot re-escapes (overshoots 0.037 / 0.038 / 0.042 vs headroom 0.30).
/// Fix: ALSO grant headroom on any side the tight bounds are MOVING TOWARD (delta vs
/// `prev_tight`, the previous dispatch's tight bounds) that is within one headroom of
/// the wall. Trailing sides (moving away) and static axes are never granted, so the
/// anchor still never chases the subject; the cap trim prefers non-granted sides.
/// `prev_tight = None` (only before a body's very first dispatch) degrades to the
/// legacy escaped-sides-only behavior. Staleness is structurally excluded: BOTH the
/// partial and full-bake dispatch paths record the entry, so it is always at most one
/// dispatch old (a wrong-direction delta from a stale entry would grant a bounded but
/// wasted headroom side — see the 2026-07-03 verification notes).
/// The escape predicate: did the live tight bounds leave the cached grid on any side?
/// 0.01 world-unit epsilon so f32 recompute jitter can't fire whole-grid rebakes.
/// SINGLE SOURCE — used by both trigger_bake's dispatch decision and the capture-replay
/// tests below; never duplicate it or the tests silently model a stale predicate.
pub(super) fn tight_escapes_grid(grid: (Vec3, Vec3), tight: (Vec3, Vec3)) -> bool {
    tight.0.x < grid.0.x - 0.01 || tight.0.y < grid.0.y - 0.01 || tight.0.z < grid.0.z - 0.01
        || tight.1.x > grid.1.x + 0.01 || tight.1.y > grid.1.y + 0.01 || tight.1.z > grid.1.z + 0.01
}

pub(super) fn compute_escape_grid_bounds(
    cached: (Vec3, Vec3),
    tight: (Vec3, Vec3),
    prev_tight: Option<(Vec3, Vec3)>,
) -> (Vec3, Vec3) {
    let extent = (tight.1 - tight.0).max(Vec3::splat(GRID_PAD_MIN));
    let headroom = extent * ESCAPE_HEADROOM;
    // Grow from the stable anchor (cached) — union of grid + live tight bounds.
    let mut gmin = tight.0.min(cached.0);
    let mut gmax = tight.1.max(cached.1);
    let esc_lo = (cached.0 - tight.0).max(Vec3::ZERO); // >0 ⇒ escaped low
    let esc_hi = (tight.1 - cached.1).max(Vec3::ZERO); // >0 ⇒ escaped high
    // Movement of the tight bounds since the previous dispatch (ZERO ⇒ no prediction).
    let (dmin, dmax) = prev_tight
        .map(|(pmin, pmax)| (tight.0 - pmin, tight.1 - pmax))
        .unwrap_or((Vec3::ZERO, Vec3::ZERO));
    // Remaining slack between the tight bounds and the unioned grid walls (≥ 0).
    let slack_lo = tight.0 - gmin;
    let slack_hi = gmax - tight.1;
    // Grant headroom on: (a) sides that escaped; (b) sides the body is moving toward
    // that are within one headroom of the wall (would re-escape within a few frames).
    let grant_lo = esc_lo.cmpgt(Vec3::ZERO) | (dmin.cmplt(Vec3::ZERO) & slack_lo.cmplt(headroom));
    let grant_hi = esc_hi.cmpgt(Vec3::ZERO) | (dmax.cmpgt(Vec3::ZERO) & slack_hi.cmplt(headroom));
    // Granted side ⇒ at least `headroom` of slack; min/max keep the union (never shrink here).
    gmin = Vec3::select(grant_lo, (tight.0 - headroom).min(gmin), gmin);
    gmax = Vec3::select(grant_hi, (tight.1 + headroom).max(gmax), gmax);
    // HARD CAP: clamp total extent to MAX_GRID_MULT × body per axis, trimming the
    // side(s) NOT granted (keep the slack where the body is headed), never re-centring
    // on the moving body.
    let max_ext = extent * MAX_GRID_MULT;
    let cur_ext = gmax - gmin;
    for a in 0..3 {
        if cur_ext[a] > max_ext[a] {
            let overflow = cur_ext[a] - max_ext[a];
            match (grant_lo.test(a), grant_hi.test(a)) {
                (false, true) => gmin[a] += overflow, // headed high → trim low (far) side
                (true, false) => gmax[a] -= overflow, // headed low → trim high (far) side
                _ => { gmin[a] += overflow * 0.5; gmax[a] -= overflow * 0.5; }
            }
        }
    }
    // Never clip the live tight bounds — the body is always fully contained.
    (gmin.min(tight.0), gmax.max(tight.1))
}

// ==================================================================
// P1-#6b rigid-translate fast path — cache-level tests (CPU only)
// ==================================================================
#[cfg(test)]
pub(crate) mod rigid_translate_tests {
    use super::*;
    use core_math::primitives::{BooleanOp, SdfPrimitive, SdfShape};
    use core_math::scene_graph::NodePayload;
    use std::sync::Arc;

    pub(crate) fn prim(uid: u32, pos: Vec3) -> SdfPrimitive {
        let mut p = SdfPrimitive::new("p".into(), SdfShape::Sphere { radius: 0.2 }, pos);
        p.prim_uid = uid;
        p.operation = BooleanOp::Union { k: 0.0 };
        p
    }

    pub(crate) fn body_sg(prims: Vec<SdfPrimitive>, mirror_flags: u8) -> (SceneGraph, NodeId) {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "Body".into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: core_math::primitives::BlendProfile::default(),
            material: core_math::primitives::PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        (sg, body)
    }

    /// A cache holding a brick-resident sparse 512 entry for `nid` at the given bounds —
    /// the exact regime the fast path requires (Object Mode, ≥512, frozen index).
    pub(crate) fn sparse_cache(nid: NodeId, bounds: (Vec3, Vec3)) -> BodyVolumeCache {
        sparse_cache_at(nid, bounds, 512)
    }

    /// `sparse_cache` at an explicit resolution (512 default; 1024 for the un-gated regime).
    pub(crate) fn sparse_cache_at(nid: NodeId, bounds: (Vec3, Vec3), res: u32) -> BodyVolumeCache {
        let bpa = (res + SPARSE_BRICK_DIM - 1) / SPARSE_BRICK_DIM; // 64
        let mut c = BodyVolumeCache::new(res, 2.0);
        c.gridbacked_only = true; // S2.6: the only sparse gate (with res >= 512)
        c.body_grids.insert(nid, BodyCacheEntry {
            grid: Arc::new(Vec::new()),
            source_hash: 0,
            grid_min: bounds.0,
            grid_max: bounds.1,
            bake_resolution: res,
            shared_source: None,
            brick_resident: Some(1),
            sparse_frozen: Some(Arc::new(core_math::chunked_dc::SparseIndexOnly {
                index: vec![core_math::chunked_dc::BRICK_TAG_EXTERIOR; (bpa * bpa * bpa) as usize],
                dims_bricks: [bpa; 3],
                allocated: 0,
                freed: Vec::new(),
            })),
            mesh_rigid_offset: Vec3::ZERO,
        });
        c.body_bounds.insert(nid, bounds);
        c
    }

    const HE: Vec3 = Vec3::splat(0.25);

    #[test]
    fn rigid_move_shifts_bounds_zero_bake() {
        let d = Vec3::new(0.31, 0.0, -0.07);
        let p_new = Vec3::new(0.1, 0.05, 0.0) + d;
        let (sg, nid) = body_sg(vec![prim(7, p_new)], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache(nid, bounds);
        c.mark_prim_transform_dirty(nid, 7, p_new - d, HE, p_new, HE, Some(d));

        let synced = c.trigger_bake(&sg, None);
        assert!(synced.is_empty());
        assert!(c.primary_bake_requests.is_empty(), "rigid move must dispatch NO bake");
        let (emin, emax) = c.entry_bounds(nid).unwrap();
        assert!((emin - (bounds.0 + d)).length() < 1e-6 && (emax - (bounds.1 + d)).length() < 1e-6,
            "entry bounds must shift by exactly the rigid delta");
        assert_eq!(c.body_bounds.get(&nid).copied().unwrap(), (emin, emax));
        assert!(c.dirty_regions.get(&nid).is_none(), "regions consumed — nothing to bake");
        assert!(c.rigid_accum.is_empty(), "accum consumed");
        assert_ne!(c.body_grids.get(&nid).unwrap().source_hash, 0,
            "hash stamped so the dedup skip recognizes the moved prims as baked");
        assert!(!c.in_flight.iter().any(|(b, _)| *b == nid),
            "no phantom in-flight entry (would deadlock the coalesce gate)");
        // The EXPLICIT draw re-anchor (the "sphere detaches at 128" regression fix): the rigid
        // consume accumulates the offset; a mesh at STALE bounds must NOT clear it; a mesh at
        // the CURRENT bounds must.
        assert!((c.mesh_rigid_offset(nid) - d).length() < 1e-6, "rigid consume accumulates the offset");
        c.clear_mesh_rigid_offset_if_current(nid, bounds.0); // pre-shift anchor — stale, keeps offset
        assert!((c.mesh_rigid_offset(nid) - d).length() < 1e-6,
            "a mesh from an OLD bake must not clear the offset");
        c.clear_mesh_rigid_offset_if_current(nid, bounds.0 + d); // current anchor — clears
        assert_eq!(c.mesh_rigid_offset(nid), Vec3::ZERO,
            "a mesh at the current bounds clears the offset");
    }

    /// Non-rigid bounds moves (escape-grow / settle-retighten semantics: entry bounds change,
    /// geometry does not) must NEVER produce a draw offset — the original derived-from-bounds
    /// offset did exactly that (the "sphere detaches from the gizmo at 128" regression).
    #[test]
    fn non_rigid_bounds_moves_leave_offset_zero() {
        let d = Vec3::new(0.2, 0.0, 0.0);
        let p_new = Vec3::new(0.1, 0.0, 0.0) + d;
        let (sg, nid) = body_sg(vec![prim(7, p_new)], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache(nid, bounds);
        // Poisoned burst → the normal bake path runs (a real request) — offset stays zero even
        // though bounds may later move via escape/retighten machinery.
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, HE); // generic mark — poisons
        c.mark_prim_transform_dirty(nid, 7, p_new - d, HE, p_new, HE, Some(d));
        c.trigger_bake(&sg, None);
        assert!(!c.primary_bake_requests.is_empty());
        assert_eq!(c.mesh_rigid_offset(nid), Vec3::ZERO,
            "non-rigid dispatches must not create a draw offset");
        // Simulate an entry-bounds move WITHOUT rigid content shift (escape/retighten class):
        // translate_body_cache alone (as the body-origin shift path uses it) — offset stays 0.
        c.translate_body_cache(nid, Vec3::new(0.5, 0.0, 0.0));
        assert_eq!(c.mesh_rigid_offset(nid), Vec3::ZERO,
            "bounds moves alone must never imply a draw offset (the 128 regression)");
    }

    #[test]
    fn rigid_accumulates_across_marks() {
        let d1 = Vec3::new(0.1, 0.0, 0.0);
        let d2 = Vec3::new(0.05, 0.02, 0.0);
        let p_new = Vec3::new(0.2, 0.02, 0.0);
        let (sg, nid) = body_sg(vec![prim(3, p_new)], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache(nid, bounds);
        c.mark_prim_transform_dirty(nid, 3, p_new - d1 - d2, HE, p_new - d2, HE, Some(d1));
        c.mark_prim_transform_dirty(nid, 3, p_new - d2, HE, p_new, HE, Some(d2));
        c.trigger_bake(&sg, None);
        assert!(c.primary_bake_requests.is_empty());
        let (emin, _) = c.entry_bounds(nid).unwrap();
        assert!((emin - (bounds.0 + d1 + d2)).length() < 1e-6, "deltas sum across marks");
    }

    #[test]
    fn poisoned_burst_falls_through_to_a_real_bake() {
        let d = Vec3::new(0.2, 0.0, 0.0);
        let p_new = Vec3::new(0.1, 0.0, 0.0) + d;
        let (sg, nid) = body_sg(vec![prim(7, p_new)], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache(nid, bounds);
        // Slider tick BEFORE the move (the ordering hole the sticky poison closes).
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, HE);
        c.mark_prim_transform_dirty(nid, 7, p_new - d, HE, p_new, HE, Some(d));
        c.trigger_bake(&sg, None);
        assert!(!c.primary_bake_requests.is_empty(),
            "poisoned burst must dispatch a real bake covering the regions");
        let (emin, _) = c.entry_bounds(nid).unwrap();
        assert!((emin - bounds.0).length() < 1e-6, "bounds must NOT shift on a poisoned burst");
        assert!(c.rigid_accum.is_empty(), "accum consumed either way");
    }

    #[test]
    fn partial_movers_or_mirror_fall_through() {
        // Two prims, only one moved → not rigid.
        let d = Vec3::new(0.2, 0.0, 0.0);
        let (sg, nid) = body_sg(vec![prim(1, Vec3::new(0.3, 0.0, 0.0) + d), prim(2, Vec3::new(-0.3, 0.0, 0.0))], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache(nid, bounds);
        c.mark_prim_transform_dirty(nid, 1, Vec3::new(0.3, 0.0, 0.0), HE, Vec3::new(0.3, 0.0, 0.0) + d, HE, Some(d));
        c.trigger_bake(&sg, None);
        assert!(!c.primary_bake_requests.is_empty(), "one-of-two movers is not rigid");
        assert!((c.entry_bounds(nid).unwrap().0 - bounds.0).length() < 1e-6);

        // Mirror body: the reflected half moves opposite — never rigid.
        let (sg2, nid2) = body_sg(vec![prim(9, Vec3::new(0.4, 0.0, 0.0) + d)], 1);
        let mut c2 = sparse_cache(nid2, bounds);
        c2.mark_prim_transform_dirty(nid2, 9, Vec3::new(0.4, 0.0, 0.0), HE, Vec3::new(0.4, 0.0, 0.0) + d, HE, Some(d));
        c2.trigger_bake(&sg2, None);
        assert!(!c2.primary_bake_requests.is_empty(), "mirror bodies must never take the rigid path");
        assert!((c2.entry_bounds(nid2).unwrap().0 - bounds.0).length() < 1e-6);
    }
}

// ==================================================================
// P1-#7 async full classify — cache-level tests (CPU only)
// ==================================================================
#[cfg(test)]
mod async_classify_tests {
    use super::*;
    use super::rigid_translate_tests::{prim, body_sg, sparse_cache};
    use std::time::Duration;

    /// The FULL sparse arm (structural mark → no dirty regions → no frozen-partial) must NOT
    /// push a request synchronously: it spawns the classify on a worker, the coalescer gate
    /// (in_flight) holds the body, and the landing pushes a request whose index is
    /// BIT-IDENTICAL to a synchronous classify of the same snapshot (same band helper — the
    /// single-source formula both sides use).
    #[test]
    fn full_classify_lands_async_and_matches_sync() {
        let p_pos = Vec3::new(0.1, 0.0, 0.0);
        let (sg, nid) = body_sg(vec![prim(7, p_pos)], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache(nid, bounds);
        // Structural mark: clears regions → the sparse construction takes the FULL arm.
        // clear_bake_debounce = the production structural pattern (a region-less full-body
        // mark otherwise sits out the 40ms debounce and trigger dispatches nothing).
        c.mark_body_dirty(nid);
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert!(c.primary_bake_requests.is_empty(),
            "FULL classify must not block the trigger — no synchronous push");
        assert_eq!(c.classify_pending, 1, "one classify in flight");
        assert!(c.in_flight.iter().any(|(b, _)| *b == nid),
            "in_flight gate must hold the body during the flight");

        c.wait_async_classifies(Duration::from_secs(30));
        assert_eq!(c.primary_bake_requests.len(), 1, "landed request queued for dispatch");
        let req = &c.primary_bake_requests[0];
        assert_eq!(req.body_nid, nid);
        assert_eq!(c.generations.get(&nid).copied(), Some(req.generation),
            "landed request carries the CURRENT generation (nothing dispatched mid-flight)");
        let Some(SparseBakeRequest::Full(ref si)) = req.sparse else {
            panic!("landed request must carry a FULL sparse payload");
        };
        // Bit-identical to a synchronous classify of the same snapshot — same inputs the
        // request carries (the FULL arm recomputes TIGHT padded bounds, not the entry's box).
        let (bmin, bmax, rres) = (req.bounds_min, req.bounds_max, req.resolution[0]);
        let band = sparse_classify_band(bmin, bmax, rres);
        let sync = core_math::chunked_dc::classify_prims_to_sparse_index(
            &[prim(7, p_pos)], 0, Vec3::ZERO, bmin, bmax, rres, SPARSE_BRICK_DIM, band,
        );
        assert!(si.allocated > 0, "anti-vacuity: the sphere allocates surface bricks");
        assert_eq!(si.allocated, sync.allocated);
        assert_eq!(si.index, sync.index, "async index must be bit-identical to sync");
    }

    /// A generation moved mid-flight (undo bump-all / invalidate) makes the snapshot stale:
    /// the landing must DROP the request, clear the in_flight gate (else the coalescer defers
    /// the body forever), and re-dirty the still-tracked body so trigger re-runs fresh.
    #[test]
    fn stale_async_classify_dropped_and_redirtied() {
        let (sg, nid) = body_sg(vec![prim(7, Vec3::new(0.1, 0.0, 0.0))], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache(nid, bounds);
        c.mark_body_dirty(nid);
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert_eq!(c.classify_pending, 1);
        // Simulate undo's bump-all landing mid-flight.
        *c.generations.get_mut(&nid).unwrap() += 1;
        c.wait_async_classifies(Duration::from_secs(30));
        assert!(c.primary_bake_requests.is_empty(), "stale landing must be dropped");
        assert!(!c.in_flight.iter().any(|(b, _)| *b == nid),
            "gate cleared — a stranded in_flight would defer the body forever");
        assert!(c.dirty_bodies.contains(&nid),
            "still-tracked body re-dirtied so the next trigger re-classifies fresh state");
    }
}

// ==================================================================
// REGRESSION (2026-07-13) — multi-body drag refresh + resolution-change recovery
// (the owner's "cylinder in body A + new body N + drag → breaks; res change no longer
// recovers" report). Cache-level halves of the AZRA_AUTOREPRO=2 sequence; the GPU half
// (dense DC into a 1024-capacity slot must stay bindable) lives in
// sdf_gpu_mesh/tests_dc.rs::sdf_dc_dense_into_1024_capacity_slot_stays_bindable.
// ==================================================================
#[cfg(test)]
mod multibody_recovery_tests {
    use super::*;
    use super::rigid_translate_tests::{prim, body_sg, sparse_cache_at};
    use core_math::primitives::BooleanOp;
    use core_math::scene_graph::NodePayload;

    const HE: Vec3 = Vec3::splat(0.25);

    /// Add a second, EMPTY SdfBody to the graph (the N-shortcut body) — the multi-body context
    /// every assertion below must hold under.
    fn add_empty_body(sg: &mut SceneGraph) -> NodeId {
        let root = sg.root();
        sg.add_child(root, NodePayload::SdfBody {
            name: "Body.N".into(), primitives: Vec::new(), mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: core_math::primitives::BlendProfile::default(),
            material: core_math::primitives::PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),})
    }

    /// Drag leg: a rigid prim drag on body A must keep refreshing A's mesh inputs (bounds follow
    /// the prims, hash stamped, no stranded in-flight) even with the empty N-body in the graph,
    /// at the 1024 sparse regime.
    #[test]
    fn multibody_drag_refreshes_dragged_body_at_1024() {
        let d = Vec3::new(0.31, 0.0, -0.07);
        let p_new = Vec3::new(1.2, 0.0, 0.0) + d;
        let (mut sg, nid) = body_sg(vec![prim(7, p_new)], 0);
        let b = add_empty_body(&mut sg);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(3.0));
        let mut c = sparse_cache_at(nid, bounds, 1024);
        c.mark_prim_transform_dirty(nid, 7, p_new - d, HE, p_new, HE, Some(d));
        let synced = c.trigger_bake(&sg, None);
        assert!(synced.is_empty());
        assert!(c.primary_bake_requests.is_empty(), "rigid drag dispatches no bake");
        let (emin, emax) = c.entry_bounds(nid).unwrap();
        assert!((emin - (bounds.0 + d)).length() < 1e-6 && (emax - (bounds.1 + d)).length() < 1e-6,
            "dragged body's mesh-input bounds must follow the drag in the multi-body scene");
        assert!(!c.in_flight.iter().any(|(bid, _)| *bid == nid), "no stranded in-flight");
        assert!(!c.dirty_bodies.contains(&b) && !c.in_flight.iter().any(|(bid, _)| *bid == b),
            "the empty N-body must not enter the bake pipeline");
    }

    /// Recovery leg: after the drag, a Grid-picker resolution change on the dragged body must
    /// queue a CLEAN dense re-bake (regime 1024-sparse → 256-dense) — the recovery action the
    /// owner reported broken. The hash matches the stamped drag hash, so this also proves the
    /// dedup skip's regime guard (entry_regime_ok) does not strand the recovery.
    #[test]
    fn resolution_change_recovers_dragged_body() {
        let d = Vec3::new(0.31, 0.0, -0.07);
        let p_new = Vec3::new(1.2, 0.0, 0.0) + d;
        let (mut sg, nid) = body_sg(vec![prim(7, p_new)], 0);
        let _b = add_empty_body(&mut sg);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(3.0));
        let mut c = sparse_cache_at(nid, bounds, 1024);
        // The drag (rigid, zero-bake, hash stamped).
        c.mark_prim_transform_dirty(nid, 7, p_new - d, HE, p_new, HE, Some(d));
        c.trigger_bake(&sg, None);
        assert!(c.primary_bake_requests.is_empty());
        // The owner's recovery action (properties_render Grid picker → active body).
        c.set_body_resolution(nid, 256);
        c.trigger_bake(&sg, None);
        let req = c.primary_bake_requests.iter().find(|r| r.body_nid == nid)
            .expect("resolution change must queue a re-bake for the dragged body");
        assert_eq!(req.resolution, [256, 256, 256], "re-bake at the picked resolution");
        assert!(req.sparse.is_none(), "256 is below the sparse threshold — dense regime");
        assert!(!c.dirty_bodies.contains(&nid), "dirty consumed by the dispatch");
    }
}

// ==================================================================
// Capture-replay tests — 2026-07-03 [BAKEPROF] one-prim drag trace
// ==================================================================
#[cfg(test)]
mod escape_policy_tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32) -> Vec3 { Vec3::new(x, y, z) }
    fn approx(a: Vec3, b: Vec3) -> bool { (a - b).abs().max_element() < 1e-3 }

    /// The PRODUCTION escape predicate — shared with the dispatch site by construction.
    fn escapes(grid: (Vec3, Vec3), tight: (Vec3, Vec3)) -> bool {
        tight_escapes_grid(grid, tight)
    }

    fn initial_grid() -> (Vec3, Vec3) { (v(-1.0, -1.0, -1.0), v(1.0, 1.0, 1.0)) }

    /// Tight bounds from the captured [DirtyFallback] lines: a 1-unit prim dragged
    /// up-left (-x, +y). E1..E5 = the five legacy escape→full events, in order.
    fn capture() -> Vec<(Vec3, Vec3)> {
        vec![
            (v(-1.2154012, 0.1330924, -0.5), v(-0.21540117, 1.1330924, 0.5)), // E1
            (v(-1.604897, 0.42137673, -0.5), v(-0.604897, 1.4213767, 0.5)),   // E2
            (v(-1.6961174, 0.47070745, -0.5), v(-0.6961174, 1.4707074, 0.5)), // E3 (y +0.037)
            (v(-1.9434798, 0.6529244, -0.5), v(-0.9434798, 1.6529244, 0.5)),  // E4 (x +0.038)
            (v(-2.1121225, 0.8126625, -0.5), v(-1.1121225, 1.8126625, 0.5)),  // E5 (y +0.042)
        ]
    }

    /// Without movement info the policy must be bit-equivalent to the legacy
    /// escaped-sides-only behavior — asserted against the grids the OLD code
    /// actually logged during the capture.
    #[test]
    fn legacy_parity_without_prediction() {
        let e = capture();
        let g1 = compute_escape_grid_bounds(initial_grid(), e[0], None);
        assert!(approx(g1.0, v(-1.5154011, 0.08309233, -0.675)), "E1 min {:?}", g1.0);
        assert!(approx(g1.1, v(-0.1654011, 1.4330924, 0.675)), "E1 max {:?}", g1.1);
        let g2 = compute_escape_grid_bounds(g1, e[1], None);
        assert!(approx(g2.0, v(-1.904897, 0.08309233, -0.675)), "E2 min {:?}", g2.0);
        assert!(approx(g2.1, v(-0.55489695, 1.4330924, 0.675)), "E2 max {:?}", g2.1);
    }

    /// Replay the captured drag WITH prediction (prev = previous event's tight
    /// bounds — same drag direction the intermediate partial dispatches recorded).
    /// Legacy fired all 5 escapes (5 whole-grid FULL bakes); the predictive grant
    /// absorbs the tiny-overshoot re-escapes E3 and E5 → 3 full bakes.
    #[test]
    fn predictive_headroom_absorbs_tiny_overshoot_reescapes() {
        let e = capture();
        let mut grid = initial_grid();
        let mut prev: Option<(Vec3, Vec3)> = None;
        let mut fired = Vec::new();
        for (i, tight) in e.into_iter().enumerate() {
            if escapes(grid, tight) {
                fired.push(i + 1);
                grid = compute_escape_grid_bounds(grid, tight, prev);
            }
            prev = Some(tight);
        }
        assert_eq!(fired, vec![1, 2, 4], "E3 + E5 must be absorbed by prediction");
    }

    /// Structural invariants across the replay: the output always contains the live
    /// tight bounds (body never clipped), and the per-axis extent respects the
    /// MAX_GRID_MULT sampling-health cap even with the extra predictive grants.
    #[test]
    fn contains_tight_and_respects_cap() {
        let e = capture();
        let mut grid = initial_grid();
        let mut prev: Option<(Vec3, Vec3)> = None;
        for tight in e {
            if escapes(grid, tight) {
                grid = compute_escape_grid_bounds(grid, tight, prev);
                assert!(grid.0.cmple(tight.0).all() && grid.1.cmpge(tight.1).all(),
                    "grid must contain tight: {:?} vs {:?}", grid, tight);
                let text = (tight.1 - tight.0).max(Vec3::splat(GRID_PAD_MIN));
                let gext = grid.1 - grid.0;
                for a in 0..3 {
                    assert!(gext[a] <= text[a] * MAX_GRID_MULT + 1e-4,
                        "axis {a}: extent {} exceeds cap {}", gext[a], text[a] * MAX_GRID_MULT);
                }
            }
            prev = Some(tight);
        }
    }

    /// Trailing walls (the sides the body moves AWAY from) and static axes must
    /// never receive headroom — granting them would re-centre the grid on the
    /// moving body, the reverted dead-end that escaped every frame.
    #[test]
    fn trailing_sides_never_granted() {
        let e = capture();
        let g1 = compute_escape_grid_bounds(initial_grid(), e[0], None);
        let g2 = compute_escape_grid_bounds(g1, e[1], Some(e[0]));
        // z (no movement): identical to the no-prediction outcome — cap-split only.
        assert!((g2.0.z - -0.675).abs() < 1e-3 && (g2.1.z - 0.675).abs() < 1e-3, "z {:?}", g2);
        // x-max (trailing on a -x drag) took the cap trim, not headroom.
        assert!(g2.1.x < e[1].1.x + ESCAPE_HEADROOM - 1e-3, "x-max granted headroom: {}", g2.1.x);
    }

    /// The 1024 device-loss fix (2026-07-13): a dirty window covering more than 1/8 of the
    /// grid's bricks must ESCALATE the frozen-index partial to the async FULL classify —
    /// never run the frame-thread mini-classify walk (~200-300ms at 1024) nor the incremental
    /// allocation that balloons the pool past the storage-binding ceiling (the owner's
    /// "panicked repeatedly, then crashed": wgpu validation error cascade → device loss).
    /// A nudge-sized window stays a frozen-index PARTIAL (the S2.5 design case).
    #[test]
    fn sparse_partial_oversized_window_escalates_to_full() {
        use super::rigid_translate_tests::{body_sg, prim, sparse_cache};
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));

        // Pathological window (a create-drag growing a prim): covers most of the grid.
        let (sg, nid) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        let mut c = sparse_cache(nid, bounds);
        c.gpu_classify_ready = true; // queue the classify record (no worker thread in tests)
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.95));
        let before = c.sparse_escalate_count;
        c.clear_bake_debounce();
        let synced = c.trigger_bake(&sg, None);
        assert!(synced.is_empty());
        assert!(c.primary_bake_requests.is_empty(),
            "oversized window must NOT dispatch a frozen-index partial");
        assert_eq!(c.pending_gpu_classify.len(), 1,
            "oversized window must escalate to the async FULL classify");
        let q = &c.pending_gpu_classify[0];
        assert_eq!((q.request.bake_min, q.request.bake_max), ([0; 3], [512; 3]),
            "escalated FULL must cover the whole grid");
        assert_eq!(c.sparse_escalate_count, before + 1, "escalation must be counted (telemetry)");

        // CONTROL — nudge-sized window: stays a frozen-index PARTIAL, no escalation.
        let (sg2, nid2) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        let mut c2 = sparse_cache(nid2, bounds);
        c2.gpu_classify_ready = true;
        c2.mark_prim_dirty_aabb(nid2, Vec3::ZERO, Vec3::splat(0.05));
        c2.clear_bake_debounce();
        let synced = c2.trigger_bake(&sg2, None);
        assert!(synced.is_empty());
        assert_eq!(c2.pending_gpu_classify.len(), 0,
            "a nudge window must not escalate to a full classify");
        assert_eq!(c2.primary_bake_requests.len(), 1, "nudge window dispatches ONE partial");
        assert!(matches!(c2.primary_bake_requests[0].sparse,
            Some(SparseBakeRequest::Partial { .. })),
            "nudge window stays on the frozen-index PARTIAL arm");
    }

    /// WINDOWED RECLASSIFY routing (2026-07-16): an oversized window in (1/8, 1/2] of the
    /// grid's bricks must take the WINDOWED escalation — a background window-walk worker that
    /// lands an ordinary `SparseBakeRequest::Partial` (sub-grid bake window, frozen index
    /// updated incrementally) — NEVER the full-grid classify, and NEVER the frame-thread walk
    /// as a synchronous partial. The windowed arm has NO cooldown: a second midsize tick after
    /// the first flight lands spawns immediately (the in_flight gate is the rate limit).
    #[test]
    fn sparse_oversized_midsize_window_takes_windowed_reclassify() {
        use super::rigid_translate_tests::{body_sg, prim, sparse_cache};
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let (sg, nid) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        let mut c = sparse_cache(nid, bounds);
        c.gpu_classify_ready = true; // the windowed arm must NOT queue a GPU full classify

        // ±0.6 of ±1.0 bounds ≈ 60% per axis ≈ 26% of bricks after dilation — in (1/8, 1/2].
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.6));
        c.clear_bake_debounce();
        let before = c.sparse_escalate_count;
        let synced = c.trigger_bake(&sg, None);
        assert!(synced.is_empty());
        assert!(c.pending_gpu_classify.is_empty(),
            "a midsize window must NOT escalate to the full GPU classify");
        assert!(c.primary_bake_requests.is_empty(),
            "the windowed payload completes on a worker, not synchronously");
        assert_eq!(c.classify_pending, 1, "windowed reclassify is ONE async flight");
        assert_eq!(c.sparse_escalate_count, before + 1,
            "windowed escalation is counted (telemetry, mode=windowed)");
        assert!(c.sparse_escalate_last.contains_key(&nid),
            "windowed escalation stamps activity (sparse_quiet / settle-trim damping)");
        assert!(!c.sparse_escalate_deferred.contains(&nid),
            "the windowed arm never defers via the full-arm cooldown memo");

        // The flight lands as a Partial whose bake window is a strict SUB-window of the grid.
        c.wait_async_classifies(std::time::Duration::from_secs(30));
        assert_eq!(c.primary_bake_requests.len(), 1, "landed windowed request");
        let req = &c.primary_bake_requests[0];
        match &req.sparse {
            Some(SparseBakeRequest::Partial { index_update, .. }) => {
                // All-EXTERIOR frozen + a real sphere in the window ⇒ the walk allocated
                // handles, so the updated index MUST ship.
                let updated = index_update.as_ref()
                    .expect("walk allocated bricks — index_update must ship");
                assert!(updated.allocated > 0, "surface bricks allocated inside the window");
            }
            other => panic!("windowed escalation must land as Partial, got {:?}",
                other.as_ref().map(|_| "non-Partial")),
        }
        assert!(req.bake_min != [0, 0, 0] || req.bake_max != [512; 3],
            "windowed bake covers the union window, never the whole grid \
             (got [{:?}..{:?}))", req.bake_min, req.bake_max);
        assert!((0..3).all(|a| req.bake_max[a] <= 512 && req.bake_min[a] < req.bake_max[a]));

        // Simulate the bake landing, then a SECOND midsize tick INSIDE 300ms: the windowed
        // arm spawns again immediately — no cooldown deferral (that is full-arm-only).
        c.primary_bake_requests.clear();
        c.in_flight.clear();
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.6));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert_eq!(c.sparse_escalate_count, before + 2,
            "second midsize tick escalates windowed immediately (no cooldown)");
        assert_eq!(c.classify_pending, 1, "second windowed flight spawned");
        assert!(c.pending_gpu_classify.is_empty() && c.sparse_escalate_deferred.is_empty());
        c.wait_async_classifies(std::time::Duration::from_secs(30));
        assert_eq!(c.primary_bake_requests.len(), 1, "second flight lands too");
    }

    /// P1-2048 COST-based routing (2026-07-17): a window UNDER the 1/8 grid fraction but whose
    /// estimated frame-thread walk cost exceeds SPARSE_WALK_FRAME_BUDGET_MS must route to the
    /// SAME windowed-reclassify worker the fractional escalation uses — never run synchronously
    /// on the frame thread (the owner's 2048 session: 310-363ms `[SparseWalk]` PER TICK for a
    /// ~1.6M-brick window at 8.6% of the grid — under the fraction, over any budget).
    #[test]
    fn sparse_window_over_cost_budget_routes_to_worker() {
        use super::rigid_translate_tests::{body_sg, prim, sparse_cache_at};
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let (sg, nid) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        // 1024³: total = 128³ = 2,097,152 bricks; 1/8 = 262,144; the cost budget crosses at
        // ~40k bricks — so a ±0.27 window (~55k bricks ≈ 2.6% of the grid) is UNDER the
        // fraction but OVER the budget: cost must decide.
        let mut c = sparse_cache_at(nid, bounds, 1024);
        c.gpu_classify_ready = true; // must NOT be used — the windowed worker is the route
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.27));
        c.clear_bake_debounce();
        let before = c.sparse_escalate_count;
        let synced = c.trigger_bake(&sg, None);
        assert!(synced.is_empty());
        assert!(c.primary_bake_requests.is_empty(),
            "an over-budget window must NOT run the synchronous frame-thread walk");
        assert!(c.pending_gpu_classify.is_empty(),
            "an under-fraction window must NOT escalate to the full classify");
        assert_eq!(c.classify_pending, 1, "over-budget window is ONE windowed worker flight");
        assert_eq!(c.sparse_escalate_count, before + 1,
            "cost routing is counted as a windowed escalation (telemetry)");

        // The flight lands as an ordinary Partial over a sub-grid window whose brick count is
        // over the budget crossing (~40k) yet under the 1/8 fraction — pinning that COST, not
        // the fraction, made the routing decision.
        c.wait_async_classifies(std::time::Duration::from_secs(60));
        assert_eq!(c.primary_bake_requests.len(), 1, "windowed flight lands");
        let req = &c.primary_bake_requests[0];
        assert!(matches!(req.sparse, Some(SparseBakeRequest::Partial { .. })),
            "cost-routed window lands as a Partial");
        let wb: u64 = (0..3).map(|a| {
            ((req.bake_max[a].saturating_sub(req.bake_min[a]) + SPARSE_BRICK_DIM - 1)
                / SPARSE_BRICK_DIM) as u64
        }).product();
        let budget_bricks =
            (SPARSE_WALK_FRAME_BUDGET_MS / SPARSE_WALK_NS_PER_BRICK * 1.0e6) as u64;
        let total: u64 = 128 * 128 * 128;
        assert!(wb > budget_bricks,
            "test window must exceed the cost-budget crossing (wb={wb} <= {budget_bricks})");
        assert!(wb * 8 <= total + total / 10, // small slack: worker may widen for appends
            "test window must stay under the 1/8 fraction (wb={wb}) — else this only re-tests \
             the fractional escalation");
        // Anti-regression control: a genuinely small window still walks SYNCHRONOUSLY
        // (latency wins for real drag nudges).
        c.primary_bake_requests.clear();
        c.in_flight.clear();
        c.mark_prim_dirty_aabb(nid, Vec3::new(0.7, 0.7, 0.7), Vec3::splat(0.05));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert_eq!(c.classify_pending, 0, "small window must not spawn a worker");
        assert_eq!(c.primary_bake_requests.len(), 1, "small window stays a sync partial");
    }

    /// P2-2048 (2026-07-17) — the settle-loop state machine, pinned end-to-end for a sparse
    /// body: (1) a dispatched no-op walk CONSUMES its dirty input (no re-walk next tick without
    /// a new edit); (2) after the flight lands, an IDENTICAL re-mark (the phantom class) is
    /// dedup-consumed, never re-walked; (3) the anti-hack guard: a REAL content edit after the
    /// no-op walk still triggers a walk — 'no content change' is distinguished from 'same
    /// window, new content' by the source hash, never by window identity.
    /// Also pins the P2 re-arm fix: a sparse (brick-resident) partial dispatch must NOT arm
    /// `pending_retighten` — C3 says the settle re-tighten can never consume it, so arming it
    /// was the "[RetightenArm] … phantom dirty regions?" feeder.
    #[test]
    fn noop_walk_consumes_dirty_state_and_never_rearms() {
        use super::rigid_translate_tests::{body_sg, prim, sparse_cache_at};
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let (mut sg, nid) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        let mut c = sparse_cache_at(nid, bounds, 1024);

        // A small window far from the sphere: all-EXTERIOR stays all-EXTERIOR — a true no-op
        // walk (+0 appended, 0 retag, 0 reclaim → index_update=None).
        let far = Vec3::new(0.7, 0.7, 0.7);
        c.mark_prim_dirty_aabb(nid, far, Vec3::splat(0.05));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert_eq!(c.primary_bake_requests.len(), 1, "no-op walk still dispatches its bake \
            (content inside handled bricks could have changed — the walk can't prove otherwise)");
        let req = c.primary_bake_requests.remove(0);
        match &req.sparse {
            Some(SparseBakeRequest::Partial { index_update, .. }) =>
                assert!(index_update.is_none(), "a no-op walk ships NO index update"),
            other => panic!("expected Partial, got {:?}", other.as_ref().map(|_| "non-Partial")),
        }
        // (1) dirty input consumed at dispatch: no re-walk next tick without a new edit.
        assert!(!c.dirty_bodies.contains(&nid), "dirty mark consumed at dispatch");
        assert!(c.dirty_regions.get(&nid).map_or(true, |r| r.is_empty()),
            "dirty regions consumed at dispatch");
        // P2 re-arm fix: the sparse partial dispatch must NOT arm the settle re-tighten.
        assert!(!c.pending_retighten.contains(&nid),
            "brick-resident partial dispatch must never arm pending_retighten (C3 skips it — \
             arming is the [RetightenArm] phantom-dirty-regions feeder)");
        c.trigger_bake(&sg, None);
        assert!(c.primary_bake_requests.is_empty(), "no re-walk without a new edit");

        // Flight lands: hash stamped on the entry (the dedup key).
        assert!(c.record_sparse_partial_bake(nid, req.source_hash, req.generation, 1, None),
            "no-op completion must record (hash stamp is what stops the loop)");

        // (2) phantom identical re-mark → dedup consumes, never re-walks.
        c.mark_prim_dirty_aabb(nid, far, Vec3::splat(0.05));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert!(c.primary_bake_requests.is_empty(),
            "identical re-mark after a recorded no-op walk must dedup-skip (the settle loop)");
        assert!(!c.dirty_bodies.contains(&nid), "dedup skip consumes the dirty mark");
        assert!(c.dirty_regions.get(&nid).map_or(true, |r| r.is_empty()),
            "dedup skip consumes the regions");

        // (3) anti-hack guard: a REAL edit (hash-changing prim move) at the SAME window must
        // still walk — the loop fix must key on content, never on window identity.
        if let Some(prims) = sg.body_primitives_mut(nid) {
            prims[0].position = Vec3::new(0.001, 0.0, 0.0);
        }
        c.mark_prim_dirty_aabb(nid, far, Vec3::splat(0.05));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert_eq!(c.primary_bake_requests.len(), 1,
            "a real content edit at the same window must still dispatch a walk");
    }

    /// Gesture-coalesced escalation (2026-07-16): a second oversized-window tick INSIDE the
    /// `ESCALATED_FULL_MIN_INTERVAL` cooldown must DEFER (regions restored, no classify spawned,
    /// body re-armed) instead of running another full pipeline — the owner's scale gesture at
    /// 1024³ ran six full classify+bake+mesh cycles back-to-back. Once the cooldown elapses the
    /// escalation proceeds. The deferral must be a complete unwind: no in_flight residue (a
    /// leaked entry would coalesce-defer the body forever) and dirty regions intact.
    #[test]
    fn sparse_escalation_cooldown_defers_then_reescalates() {
        use super::rigid_translate_tests::{body_sg, prim, sparse_cache};
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let (sg, nid) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        let mut c = sparse_cache(nid, bounds);
        c.gpu_classify_ready = true;

        // Tick 1 — oversized window escalates (fresh body, no cooldown stamp).
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.95));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert_eq!(c.pending_gpu_classify.len(), 1, "first oversized tick escalates");
        assert_eq!(c.sparse_escalate_count, 1);
        assert!(c.sparse_escalate_last.contains_key(&nid), "escalation must stamp the cooldown");

        // Simulate the classify landing: flight cleared, drop the spawned record.
        c.pending_gpu_classify.clear();
        c.in_flight.clear();

        // Tick 2 — still inside the cooldown: must DEFER, not escalate.
        let gen_before_defer = c.generations.get(&nid).copied().unwrap_or(0);
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.95));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        // Settle-loop fix (2026-07-16): the deferral must unwind its generation bump — a
        // stranded +1 per deferred tick makes every in-flight landing compare stale and get
        // DROPPED, so the settle full never records (the entry-less settle-loop feeder).
        assert_eq!(c.generations.get(&nid).copied().unwrap_or(0), gen_before_defer,
            "deferral must unwind the generation bump (stranded gens drop in-flight landings)");
        assert_eq!(c.pending_gpu_classify.len(), 0, "cooldown tick must not spawn a classify");
        assert_eq!(c.primary_bake_requests.len(), 0, "cooldown tick must not dispatch anything");
        assert_eq!(c.sparse_escalate_count, 1, "deferral is not an escalation");
        assert!(c.dirty_bodies.contains(&nid), "deferred body stays dirty (re-armed)");
        assert!(c.dirty_regions.get(&nid).is_some_and(|r| !r.is_empty()),
            "deferred body keeps its dirty regions");
        assert!(!c.in_flight.iter().any(|(b, _)| *b == nid),
            "deferral must leave NO in_flight residue (would coalesce-defer forever)");
        // The deferral must MEMO itself so subsequent cooling frames skip at the top of the loop
        // (owner log: 30 full trigger walks + "[RetightenArm] partial_dispatch while IDLE" prints
        // per cooldown, one per frame, all reaching the same foregone conclusion).
        assert!(c.sparse_escalate_deferred.contains(&nid), "defer must memo the fast path");

        // Tick 3 — still cooling: the memo must make this a TOP-OF-LOOP skip. Proven by the
        // pending_retighten arm (which lives AFTER the skip point) never firing: clear it first,
        // and it must stay clear because the walk never reaches the arm.
        c.pending_retighten.remove(&nid);
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert!(!c.pending_retighten.contains(&nid),
            "a cooling tick must skip BEFORE the pending_retighten arm (no wasted trigger walk)");
        assert_eq!(c.pending_gpu_classify.len(), 0, "memoized cooling tick dispatches nothing");
        assert!(c.dirty_bodies.contains(&nid), "memoized skip still re-arms the body");

        // Cooldown elapsed (backdate the stamp) — the SAME dirty state now escalates.
        c.sparse_escalate_last.insert(nid,
            Instant::now() - ESCALATED_FULL_MIN_INTERVAL - std::time::Duration::from_millis(50));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert_eq!(c.pending_gpu_classify.len(), 1, "post-cooldown tick escalates again");
        assert_eq!(c.sparse_escalate_count, 2);
        let q = &c.pending_gpu_classify[0];
        assert_eq!((q.request.bake_min, q.request.bake_max), ([0; 3], [512; 3]),
            "re-escalated FULL covers the whole grid");
    }

    /// Dedup-skip region consumption (2026-07-16 idle escalate-loop diagnosis): a body whose
    /// hash+res match the recorded entry is dedup-skipped — and that skip must CONSUME the dirty
    /// regions too. Stranded regions union with the next real (tiny) edit into a stale
    /// gesture-sized window → an instant spurious oversized-window escalation.
    #[test]
    fn dedup_skip_consumes_stale_regions() {
        use super::rigid_translate_tests::{body_sg, prim, sparse_cache};
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let (sg, nid) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        let mut c = sparse_cache(nid, bounds);
        c.gpu_classify_ready = true;

        // Land a full sparse state: first trigger escalates (fresh body, huge window) and the
        // stamped entry hash comes from recording the spawned request's own hash.
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.95));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        let q = c.pending_gpu_classify.pop().expect("first trigger escalates to a full classify");
        let hash = q.request.source_hash;
        // Stamp the entry as baked at this hash+res (the record_sparse_bake effect the test
        // needs: dedup compares entry.source_hash + bake_resolution).
        if let Some(e) = c.body_grids.get_mut(&nid) {
            e.source_hash = hash;
            e.bake_resolution = 512;
        }
        c.in_flight.clear();

        // Idle-loop model: a mark with UNCHANGED data — regions exist, hash matches the entry.
        c.mark_prim_dirty_aabb(nid, Vec3::ZERO, Vec3::splat(0.95));
        assert!(c.dirty_regions.get(&nid).is_some_and(|r| !r.is_empty()));
        c.clear_bake_debounce();
        c.trigger_bake(&sg, None);
        assert!(c.pending_gpu_classify.is_empty() && c.primary_bake_requests.is_empty(),
            "hash+res match must dedup-skip (no dispatch)");
        assert!(!c.dirty_bodies.contains(&nid), "dedup skip consumes the dirty mark");
        assert!(c.dirty_regions.get(&nid).map_or(true, |r| r.is_empty()),
            "dedup skip must consume regions — stranded regions resurrect as oversized windows");
    }

    /// Settle-loop guard (2026-07-16 owner log): a no-copy bake completion for an ENTRY-LESS
    /// body must NOT arm the heal/settle machinery — there is no CPU grid to be stale relative
    /// to. Before the guard, this armed `pending_retighten` + `cpu_grid_stale` in an infinite
    /// 64³-coarse → no-copy → re-arm cycle (the endless "[RetightenArm] by=no_copy_completion
    /// while IDLE" spam). The dispatch gate (app_poll_bake.rs `resident_full_direct` requires
    /// `has_cached_grid`) prevents the class; this pins the cache-level guard.
    #[test]
    fn no_copy_completion_never_arms_settle_for_entryless_body() {
        use super::rigid_translate_tests::{body_sg, prim, sparse_cache};
        let (_sg, nid) = body_sg(vec![prim(7, Vec3::ZERO)], 0);
        let mut c = sparse_cache(nid, (Vec3::splat(-1.0), Vec3::splat(1.0)));
        // Simulate the loop's seed: the body's entry is gone (undo/invalidate/regime flip).
        c.body_grids.remove(&nid);
        assert!(c.settle_quiescent(), "clean start");
        // The PATH-A no-copy completion bookkeeping fires for the entry-less body…
        c.note_cpu_grid_stale(nid);
        // …and must refuse to arm anything: quiescence holds, no phantom settle feeder.
        assert!(c.settle_quiescent(),
            "an entry-less no-copy completion must not arm pending_retighten/cpu_grid_stale \
             (the infinite idle settle-loop feeder)");
    }
}

// ==================================================================
// Boolean-chain ORDER in the source hash + resolution-regime stability
// (owner session 2026-07-13: undo-of-reorder never re-baked; selection/
// work-mode changes churned 1024->512->256->1024 re-bakes)
// ==================================================================
#[cfg(test)]
mod order_hash_and_regime_tests {
    use super::*;
    use super::rigid_translate_tests::{body_sg, prim, sparse_cache_at};
    use core_math::scene_graph::BodyChildRef;

    /// Reordering `child_order` (the boolean chain — order IS the field the serializer
    /// emits; the prims Vec is NOT permuted by `reorder_child_in_body`) must change the
    /// body source hash. Restoring the ORIGINAL order must reproduce the ORIGINAL hash —
    /// that is correct dedup: an entry stamped pre-reorder is legitimately reusable when
    /// undo lands back on the pre-reorder state.
    #[test]
    fn source_hash_covers_child_order() {
        let (mut sg, nid) = body_sg(vec![prim(1, Vec3::ZERO), prim(2, Vec3::new(0.5, 0.0, 0.0))], 0);
        sg.ensure_body_child_order(nid);
        let h_orig = crate::bake_utils::body_content_id(&sg, nid, 0).unwrap().0;

        // Move prim 1 to the end (owner repro: first prim of the stack becomes last).
        sg.reorder_child_order(nid, BodyChildRef::Prim(1), 1);
        let h_reordered = crate::bake_utils::body_content_id(&sg, nid, 0).unwrap().0;
        assert_ne!(h_orig, h_reordered,
            "FIXED: the source hash must see a child_order reorder (it was blind — \
             a redundant full re-bake: hash X==X across different boolean chains)");

        // Restore the original order (what an undo snapshot restore produces — same
        // uids/NodeIds, same order): hash must equal the ORIGINAL, not the reordered one.
        sg.reorder_child_order(nid, BodyChildRef::Prim(1), 0);
        let h_restored = crate::bake_utils::body_content_id(&sg, nid, 0).unwrap().0;
        assert_eq!(h_orig, h_restored, "same order = same hash (stable across undo restore)");
    }

    /// A nested sub-body's `body_op` (how its evaluated result folds into the parent
    /// scope) is an eval input outside the prim slice — same blindness class as order.
    #[test]
    fn source_hash_covers_subbody_order_and_op() {
        use core_math::primitives::{BlendProfile, BooleanOp, PbrMaterial};
        let (mut sg, top) = body_sg(vec![prim(1, Vec3::ZERO)], 0);
        let sub = sg.add_child(top, NodePayload::SdfBody {
            name: "Sub".into(), primitives: vec![prim(2, Vec3::new(0.4, 0.0, 0.0))],
            mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        sg.ensure_body_child_order(top);
        sg.ensure_body_child_order(sub);
        let h0 = crate::bake_utils::body_content_id(&sg, top, 0).unwrap().0;

        // Reorder the sub-body before the loose prim: chain changes, hash must change.
        sg.reorder_child_order(top, BodyChildRef::Body(sub), 0);
        let h1 = crate::bake_utils::body_content_id(&sg, top, 0).unwrap().0;
        assert_ne!(h0, h1, "sub-body position in the chain must be hashed");

        // Flip the sub-body's boolean op: hash must change too.
        if let Some(NodePayload::SdfBody { body_op, .. }) = sg.get_mut(sub).map(|n| &mut n.payload) {
            *body_op = BooleanOp::Subtract { k: 0.0 };
        }
        let h2 = crate::bake_utils::body_content_id(&sg, top, 0).unwrap().0;
        assert_ne!(h1, h2, "sub-body body_op must be hashed (Union to Subtract is a field change)");
    }

    /// GATE 1 GO-gate (DC_CONSOLIDATION_GATED_PLAN.md): editing a prim INSIDE a nested
    /// sub-body must bump the parent body's unified `BodyContentId` (the recursive
    /// semantic every consumer — bake, atlas, DC dispatch, mesh_cache, PT — now shares),
    /// while an untouched SIBLING body's ID stays identical (no cross-body thrash).
    #[test]
    fn nested_child_edit_bumps_content_id() {
        use core_math::primitives::{BlendProfile, BooleanOp, PbrMaterial};
        let (mut sg, top) = body_sg(vec![prim(1, Vec3::ZERO)], 0);
        let sub = sg.add_child(top, NodePayload::SdfBody {
            name: "Sub".into(), primitives: vec![prim(2, Vec3::new(0.4, 0.0, 0.0))],
            mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        // Independent sibling body under the root — its ID must be inert to the edit.
        let sibling = sg.add_child(sg.root(), NodePayload::SdfBody {
            name: "Sibling".into(), primitives: vec![prim(3, Vec3::new(3.0, 0.0, 0.0))],
            mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        sg.ensure_body_child_order(top);
        sg.ensure_body_child_order(sub);
        sg.ensure_body_child_order(sibling);

        let top_before = crate::bake_utils::body_content_id(&sg, top, 0).unwrap();
        let sib_before = crate::bake_utils::body_content_id(&sg, sibling, 0).unwrap();

        // Edit a prim INSIDE the nested sub-body (content change, not a transform of `top`).
        if let Some(NodePayload::SdfBody { primitives, .. }) = sg.get_mut(sub).map(|n| &mut n.payload) {
            primitives[0].position = Vec3::new(0.6, 0.1, 0.0);
        }

        let top_after = crate::bake_utils::body_content_id(&sg, top, 0).unwrap();
        let sib_after = crate::bake_utils::body_content_id(&sg, sibling, 0).unwrap();
        assert_ne!(top_before, top_after,
            "a nested-child prim edit MUST bump the parent's BodyContentId (rebake fires)");
        assert_eq!(sib_before, sib_after,
            "an untouched sibling body's BodyContentId must be stable (no thrash)");
    }

    /// END-TO-END undo scenario at the cache level (the owner repro): reorder, bake,
    /// entry stamped with the REORDERED hash, undo restores the original order —
    /// `invalidate_for_undo` must detect the mismatch (mark dirty, NOT transform_safe)
    /// and `trigger_bake` must NOT hash-skip — a re-bake fires.
    #[test]
    fn undo_of_reorder_rebakes() {
        let (mut sg, nid) = body_sg(vec![prim(1, Vec3::ZERO), prim(2, Vec3::new(0.5, 0.0, 0.0))], 0);
        sg.ensure_body_child_order(nid);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));

        // User reorders (prim 1 to last), the bake lands and stamps the reordered hash.
        sg.reorder_child_order(nid, BodyChildRef::Prim(1), 1);
        let h_reordered = crate::bake_utils::body_content_id(&sg, nid, 0).unwrap().0;
        let mut c = sparse_cache_at(nid, bounds, 512);
        c.body_grids.get_mut(&nid).unwrap().source_hash = h_reordered;

        // Undo: snapshot restore = the original order (same uids/NodeIds).
        sg.reorder_child_order(nid, BodyChildRef::Prim(1), 0);

        // Selective undo invalidation must see the change...
        c.invalidate_for_undo(&sg);
        assert!(c.dirty_bodies.contains(&nid),
            "FIXED: undo restoring a different boolean-chain order must mark the body dirty \
             (was: hash falsely equal, transform_safe, surface never updated back)");
        assert!(!c.transform_safe_bodies.contains(&nid),
            "an order change is NOT transform-safe");

        // ...and trigger_bake must NOT dedup-skip: a bake (classify) must actually dispatch.
        c.gpu_classify_ready = true;
        c.clear_bake_debounce();
        let _ = c.trigger_bake(&sg, None);
        assert!(!c.pending_gpu_classify.is_empty() || !c.primary_bake_requests.is_empty(),
            "trigger must re-bake after undo-of-reorder (entry hash = reordered, not restored)");
    }

    /// Regime stability (owner bug 2): a sparse-eligible body's bake resolution must NOT
    /// collapse to the draft. Draft is a dense-path trick; drafting a 1024 sparse body to
    /// 512/256 destroyed brick residency (regime_ok=false) and produced the
    /// 1024->512->256->1024 re-bake churn on mere selection / work-mode changes.
    /// `effective_resolution` (the single clamp policy) now decides on the UNDRAFTED base.
    #[test]
    fn sparse_body_resolution_stable_under_draft() {
        let (_sg, nid) = body_sg(vec![prim(1, Vec3::ZERO)], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let mut c = sparse_cache_at(nid, bounds, 1024);
        c.set_body_resolution(nid, 1024);
        c.dirty_bodies.clear(); // set_body_resolution marks dirty; not under test here

        c.draft_resolution = Some(256);
        assert_eq!(c.effective_resolution(nid), 1024,
            "FIXED: a sparse-eligible (>=512, Object Mode) body must ignore the draft");

        // A dense body still drafts — the speed trick is preserved. S2.6: with the toggle gone,
        // eligibility is `gridbacked_only && res >= 512`, and `gridbacked_only` is ALWAYS true in
        // production (dirty.rs sets it unconditionally in trigger_body_vol_bake; the legacy
        // `force_volume_only` that could clear it was deleted 2026-07-13). So the ONLY
        // production-reachable dense regime is a body BELOW the sparse threshold — that is what
        // this arm must pin. (Do NOT use `gridbacked_only = false`: it is dead in production, and
        // asserting through it would guard a branch no user can reach.)
        // base 256 (<512 → not sparse-eligible) + draft 128 → draft.min(base) = 128; the values
        // differ so the assert genuinely proves the draft was applied.
        c.set_body_resolution(nid, 256);
        c.dirty_bodies.clear(); // set_body_resolution marks dirty; not under test here
        c.draft_resolution = Some(128);
        assert_eq!(c.effective_resolution(nid), 128,
            "dense (<512) regime keeps the draft speedup (base 256 drafted to 128)");
    }

    /// OWNER REPRO 2026-07-14 (root #1 of "group into sub-body → surface lost + [RetightenArm]
    /// spam"): `update_lod_resolutions` computed the body AABB from OWN prims only. A top-level
    /// body whose prims ALL moved into a nested sub-body (the outliner Group gesture) has an
    /// empty own list → the LOD fell back to a degenerate (ZERO,ONE) box → tiny screen fraction
    /// → a 1024 sparse body was dropped below 512 → sparse regime destroyed → dense re-bake
    /// churn ping-ponging against the full-bake adaptive recompute (which uses the REAL
    /// recursive grid bounds). The LOD must use the SAME recursive prim set as the bake.
    #[test]
    fn lod_uses_recursive_prims_for_grouped_body() {
        use core_math::primitives::{BlendProfile, BooleanOp, PbrMaterial};
        // Top body with ZERO own prims; ALL geometry lives in a nested sub-body, spread wide
        // (recursive AABB ≈ ±4). The degenerate (0,1) fallback box would read a tiny screen
        // fraction at this camera and drop the tier; the real bounds read > 0.15 → full res.
        let (mut sg, top) = body_sg(Vec::new(), 0);
        let _sub = sg.add_child(top, NodePayload::SdfBody {
            name: "Group.001".into(),
            primitives: vec![prim(5, Vec3::new(-4.0, 0.0, 0.0)), prim(6, Vec3::new(4.0, 0.0, 0.0))],
            mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        sg.ensure_body_child_order(top);

        let bounds = (Vec3::splat(-4.5), Vec3::splat(4.5));
        let mut c = sparse_cache_at(top, bounds, 1024);
        c.set_body_resolution(top, 1024);
        c.dirty_bodies.clear(); // set_body_resolution marks dirty; not under test
        c.use_adaptive_resolution = true;

        // Camera at 20 units, fov_scale 0.5: real radius ≈ 7.3 → fraction ≈ 0.73 (full res);
        // the old degenerate box (radius ≈ 0.87) read ≈ 0.089 → half-res tier → 512 + dirty.
        c.update_lod_resolutions(&sg, Vec3::new(0.0, 0.0, 20.0), 0.5, 1000.0);
        assert_eq!(c.body_resolution(top), Some(1024),
            "FIXED: LOD must size a grouped body from its RECURSIVE prim set — dropping a 1024 \
             sparse body below 512 destroys the sparse regime (the RetightenArm churn loop)");
        assert!(c.dirty_bodies.is_empty(),
            "LOD must not churn dirty_bodies for a grouped body whose real bounds are unchanged");

        // Idle-frame stability: repeated LOD passes (poll_warming / poll pumps run this every
        // trigger) must stay quiescent — the churn loop was exactly a per-frame re-dirty here.
        for _ in 0..3 {
            c.update_lod_resolutions(&sg, Vec3::new(0.0, 0.0, 20.0), 0.5, 1000.0);
        }
        assert!(c.dirty_bodies.is_empty(), "repeated LOD passes must not re-dirty (settle loop)");

        // A truly empty body (no prims anywhere) gets NO tier at all — never the degenerate box.
        let (sg2, empty) = body_sg(Vec::new(), 0);
        let mut c2 = sparse_cache_at(empty, bounds, 1024);
        c2.body_grids.clear();
        c2.body_target_resolutions.clear();
        c2.dirty_bodies.clear();
        c2.use_adaptive_resolution = true;
        c2.update_lod_resolutions(&sg2, Vec3::new(0.0, 0.0, 20.0), 0.5, 1000.0);
        assert_eq!(c2.body_resolution(empty), None,
            "an empty subtree must not be assigned an LOD tier from the fallback box");
        assert!(c2.dirty_bodies.is_empty(), "an empty subtree must not be dirtied by LOD");
    }

    /// Work-mode / scope switches mark ONLY stale bodies dirty: a body with a valid
    /// full-resolution grid must not re-bake on a Vertex/Object toggle.
    #[test]
    fn mode_switch_marks_only_stale_bodies() {
        let (sg, nid) = body_sg(vec![prim(1, Vec3::ZERO)], 0);
        let bounds = (Vec3::splat(-1.0), Vec3::splat(1.0));

        // Valid full-res sparse entry at its target resolution: NOT marked.
        let mut c = sparse_cache_at(nid, bounds, 512);
        c.mark_stale_bodies_dirty(&sg);
        assert!(!c.dirty_bodies.contains(&nid),
            "a body with a valid full-resolution grid must NOT re-bake on a mode switch");

        // Missing grid: marked (first-bake path preserved).
        let mut c2 = sparse_cache_at(nid, bounds, 512);
        c2.body_grids.clear();
        c2.mark_stale_bodies_dirty(&sg);
        assert!(c2.dirty_bodies.contains(&nid), "a body with no grid must be marked");
    }
}
