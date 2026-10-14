//! Level 4 — Progressive Resolution Cascade helpers.

use std::time::Instant;
use glam::Vec3;
use core_math::scene_graph::{SceneGraph, NodeId, NodePayload};
use super::{BodyVolumeCache, PrimaryBakeRequest, BODY_LOCAL_PRIMS};
use crate::bake_utils::*;

impl BodyVolumeCache {
    // ============================================================
    // Level 4 — Progressive Resolution Cascade helpers
    // ============================================================

    /// Advance a progressive resolution cascade after a bake result is accepted.
    ///
    /// Called by app_poll after `accept_gpu_result()` succeeds. If a cascade
    /// exists for this body and the generation matches, marks the current level
    /// as completed and pushes a PrimaryBakeRequest for the next level.
    /// When all levels are done (or the body was re-dirtied), removes the entry.
    pub(crate) fn advance_cascade(&mut self, body_nid: NodeId, sg: &SceneGraph) {
        let _cascade_t0 = std::time::Instant::now();
        // Regime ceiling, read BEFORE the cascades borrow (see the cap check below).
        let max_res = self.max_mesh_resolution();
        let cascade = match self.cascades.get_mut(&body_nid) {
            Some(c) => c,
            None => return, // No active cascade for this body
        };

        // Stale check: if body was re-dirtied, the generation has been bumped
        let current_gen = self.generations.get(&body_nid).copied().unwrap_or(0);
        if cascade.generation < current_gen {
            self.cascades.remove(&body_nid);
            return;
        }

        // Mark current level as completed
        let level = cascade.current_level;
        if level < 3 {
            cascade.completed[level] = true;
        }

        // Check if there's a next level to dispatch
        let next_level = level + 1;
        if next_level >= 3 {
            // All 3 levels done — remove cascade
            self.cascades.remove(&body_nid);
            return;
        }

        let next_res = cascade.target_resolutions[next_level];
        let prev_res = cascade.target_resolutions[level];

        // Don't dispatch if next resolution is same or lower than what we just did,
        // or if it exceeds the active regime's ceiling (max_mesh_resolution: dense ≤512,
        // sparse ≤1024 — a 1024 promotion must SURVIVE this check to reach the sparse
        // divert below; TextureOnly's 512 cap equals the dense cap, so Edit Mode is covered).
        // (`max_res` read at the top of this fn, before the cascades borrow.)
        if next_res <= prev_res || next_res > max_res {
            self.cascades.remove(&body_nid);
            return;
        }

        // Update cascade state
        cascade.current_level = next_level;
        let generation = cascade.generation;
        // `cascade` (the &mut self.cascades borrow) is not used past this point.

        // Sparse promotion: the cascade emits DENSE requests (`sparse: None`), so promoting to a
        // sparse-eligible resolution would run a full DENSE 512³ bake + ~512MB readback — the
        // owner-measured 816ms first-512 STALL. A sparse body has no readback at all. So STOP the
        // cascade here and hand the promotion to `trigger_bake`, which classifies + dispatches the
        // sparse full (no readback, and now sphere-prefiltered ~15ms). One-shot: clear debounce so
        // it fires immediately. The lower cascade levels (≤256 draft) still run for fast first
        // feedback; only the ≥512 promotion diverts to sparse.
        if self.sparse_bake_eligible(next_res) {
            self.cascades.remove(&body_nid);
            self.mark_body_dirty(body_nid);
            self.last_dirty_time = None; // bypass the 40ms debounce — one-shot promotion
            return;
        }

        // Collect body data for the next-level bake request
        let (mirror_flags, mirror_sign, mirror_center, bevel_radius, body_origin) = sg.get(body_nid)
            .and_then(|n| match &n.payload {
                NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, bevel_radius, origin, .. } =>
                    Some((*mirror_flags, *mirror_sign, *mirror_center, *bevel_radius, *origin)),
                _ => None,
            })
            .unwrap_or((0, 0, Vec3::ZERO, 0.0, Vec3::ZERO));

        let (grid_min, grid_max) = self.body_bounds.get(&body_nid)
            .copied()
            .unwrap_or((Vec3::splat(-self.bounds), Vec3::splat(self.bounds)));

        let local_mirror_center = if BODY_LOCAL_PRIMS {
            mirror_center // already body-local
        } else {
            mirror_center - body_origin
        };

        let _t_serialize = std::time::Instant::now();
        let mut prim_floats = graphics::serialize_body_for_bake_hot(
            sg, body_nid,
            if BODY_LOCAL_PRIMS { Vec3::ZERO } else { body_origin },
        );
        let clip_curve_floats = graphics::serialize_clip_curves_for_bake(sg, body_nid, &mut prim_floats);
        let mesh_grid_floats = graphics::serialize_mesh_grids_for_bake(sg, body_nid, &mut prim_floats);
        // T3: intermediate cascade levels pack coarse height mips; final level packs full.
        let hm_max = if next_level >= 2 {
            0
        } else {
            core_math::modifiers::DRAFT_HEIGHT_MIP_MAX_SIDE
        };
        let height_map_floats =
            graphics::serialize_height_maps_for_bake(sg, body_nid, &mut prim_floats, hm_max);
        let _serialize_ms = _t_serialize.elapsed().as_secs_f64() * 1000.0;
        let _prim_float_count = prim_floats.len();

        // Compute source_hash from current prim data + body-level properties.
        // MUST match the hash computed in trigger_bake (prim + mirror + bevel),
        // otherwise the hash check in trigger_bake will never match the cascade
        // result, causing an infinite rebake loop.
        let _t_collect = std::time::Instant::now();
        let prims = collect_body_prims_recursive(sg, body_nid);
        let local_prims = if BODY_LOCAL_PRIMS {
            prims.clone() // already body-local
        } else {
            offset_prims_to_local(&prims, body_origin)
        };
        let _collect_ms = _t_collect.elapsed().as_secs_f64() * 1000.0;

        let _t_hash = std::time::Instant::now();
        // #4/#12: MUST match trigger_bake's stored source_hash — ONE shared implementation
        // (bake_utils::hash_body_source, incl. the boolean-chain ORDER) so it cannot drift.
        let source_hash = crate::bake_utils::hash_body_source(
            sg, body_nid, &local_prims,
            mirror_flags, mirror_sign, local_mirror_center, bevel_radius,
            self.lab_operand_gen,
        );
        let _hash_ms = _t_hash.elapsed().as_secs_f64() * 1000.0;

        // Mark in-flight so trigger_bake doesn't double-dispatch
        self.in_flight.insert((body_nid, source_hash));
        self.bake_start_times.insert(body_nid, Instant::now());

        // Per-axis asymmetric resolution from AABB aspect ratio.
        let res3 = Self::compute_asymmetric_resolution(next_res, grid_min, grid_max);
        // CUBIC INVARIANT — parity with trigger_bake (bake_dispatch.rs). Cascade feeds the same
        // GridBacked DC path, whose FlatGridAccessor + accept_gpu_result's `cbrt(grid.len())` assume a
        // perfect cube. An asymmetric res3 from an elongated AABB → cbrt mis-infers the resolution → DC
        // indexes past the packed buffer → out-of-bounds crash (and silently mis-indexes milder
        // asymmetries). trigger_bake forces cubic here; cascade had diverged, so an elongated body's
        // draft→refine cascade could dispatch a non-cubic grid and crash on readback. Force cubic at the
        // longest axis for GridBacked (≤512). TextureOnly (>512) is sampled by the VP shader in
        // normalized space, so it keeps the voxel-saving asymmetric resolution.
        let res3 = {
            let max_axis = res3[0].max(res3[1]).max(res3[2]);
            // Parity with trigger_bake's cubic clamp (max_mesh_resolution — dense ≤512 or
            // sparse ≤1024). Unreachable at >512 in practice (the sparse divert above removed
            // the cascade), kept keyed on the same ceiling so the two sites cannot drift.
            if max_axis <= self.max_mesh_resolution() {
                [max_axis, max_axis, max_axis]
            } else {
                res3
            }
        };
        self.primary_bake_requests.push(PrimaryBakeRequest {
            body_nid,
            prim_floats,
            resolution: res3,
            bounds_min: grid_min,
            bounds_max: grid_max,
            bake_min: [0, 0, 0],
            bake_max: res3,
            mirror_flags: mirror_flags as u32,
            mirror_sign: mirror_sign as u32,
            mirror_center: local_mirror_center,
            bevel_radius,
            source_hash,
            template_hash: 0, // cascade bakes don't participate in instancing
            generation,
            base_grid: None,
            clip_curve_floats,
            mesh_grid_floats,
            height_map_floats,
            sparse: None, // cascade (coarse/draft) bakes are always dense
            coarse_cull_ok: false, // dense path — never classified
            force_cpu_mod_stack: crate::bake_utils::body_needs_cpu_full_stack_bake(sg, body_nid),
        });

        let _cascade_total_ms = _cascade_t0.elapsed().as_secs_f64() * 1000.0;
        if _cascade_total_ms > 0.5 {
            eprintln!(
                "[CascadeAdvance] body={:?} level={}->{} res={} total={:.1}ms \
                 (serialize={:.2}ms [{}floats] collect_prims={:.2}ms hash={:.2}ms)",
                body_nid, level, next_level, next_res, _cascade_total_ms,
                _serialize_ms, _prim_float_count, _collect_ms, _hash_ms,
            );
        }
    }

    /// Cancel an active cascade for a body (e.g., body re-dirtied during cascade).
    /// The new edit will trigger a fresh bake/cascade.
    pub(crate) fn cancel_cascade(&mut self, body_nid: NodeId) {
        self.cascades.remove(&body_nid);
    }

    /// Freeze the cascade for a body the user is leaving.
    /// Cancels any pending bake requests and removes the cascade so that
    /// no 256³ bake fires for a body the user just stopped editing.
    /// The body keeps whatever cached volume it has. If the user returns,
    /// cascade resumes from scratch (or uses the existing cached grid).
    pub(crate) fn freeze_cascade_on_leave(&mut self, body_nid: NodeId) {
        self.cascades.remove(&body_nid);
        self.primary_bake_requests.retain(|req| req.body_nid != body_nid);
    }

    /// Clear bake debounce so the next trigger_bake dispatches immediately.
    /// Used on scope-leave (body switch) — a one-shot event, not a rapid-edit stream.
    pub(crate) fn clear_debounce(&mut self) {
        self.last_dirty_time = None;
    }

    /// Human-readable label for diagnostic prints.
    #[allow(dead_code)]
    pub(crate) fn bake_state_label(&self, body_nid: NodeId) -> &'static str {
        let has_cache = self.body_grids.contains_key(&body_nid);
        let is_baking = self.in_flight.iter().any(|(bid, _)| *bid == body_nid);
        let is_dirty = self.dirty_bodies.contains(&body_nid);

        match (has_cache, is_baking, is_dirty) {
            (_, _, true)      => "Dirty",
            (_, true, _)      => "Baking",
            (true, false, _)  => "Clean",
            (false, false, _) => "NeverBaked",
        }
    }
}
