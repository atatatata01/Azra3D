//! Poly display LOD (PLAN_FOR_GROK_VIEWPORT_PERF P3).
//!
//! **Not** `MeshLod` (that enum means SDF grid 64³/128³/256³). These are display-only triangle
//! reduction levels for object-mode poly solids. Edit mode always uses full detail.
//!
//! Level chooser: reuses `sdf_gpu_mesh::lod_policy::choose_lod_level` on a projected **feature
//! size in pixels** (approx mean edge length on screen), with the same deadband/hysteresis.
//!
//! Coarse meshes: built once per content key via `display_mesh_ops` Fast QEM at 50% / 15%,
//! stored as extra `MeshCacheId`s. Full detail stays on `PolygonalBody.mesh_cache_id`.

use core_math::display_mesh_ops::{
    apply_display_mesh_op, DecimateMode, DecimateParams, DisplayMesh, DisplayMeshOp,
};
use core_math::scene_graph::{MeshCacheId, NodeId, NodePayload};
use glam::Vec3;
use std::time::Instant;

use crate::sdf_gpu_mesh::{choose_lod_level, LOD_MAX_LEVEL, LOD_MIN_SWITCH_INTERVAL};
use crate::AzraApp;

/// Minimum full-detail tris before LOD builds coarse levels (small cages stay full).
const POLY_LOD_MIN_TRIS: u32 = 2_000;
/// Keep ratios of full triangle count for L1 / L2.
const POLY_LOD_RATIO: [f32; 2] = [0.50, 0.15];

/// Shared coarse meshes for one content key (many bodies can reuse these ids).
#[derive(Clone, Default)]
pub(crate) struct PolyLodSharedMeshes {
    pub medium_cid: Option<MeshCacheId>,
    pub far_cid: Option<MeshCacheId>,
    pub full_tris: u32,
    pub medium_tris: u32,
    pub far_tris: u32,
}

/// A finished background coarse-LOD build (see `poly_lod_ensure_coarse`).
pub(crate) struct PolyLodBakeResult {
    pub content_key: (u64, u64),
    pub full_tris: u32,
    /// `(verts, indices, tris)` per level, `None` when that level was already present.
    pub medium: Option<(Vec<[f32; 8]>, Vec<u32>, u32)>,
    pub far: Option<(Vec<[f32; 8]>, Vec<u32>, u32)>,
}

/// Per-body runtime LOD state (never serialized).
#[derive(Clone)]
pub(crate) struct PolyLodBodyState {
    /// Same key as `poly_display_cache` — invalidate when base/stack changes.
    pub content_key: (u64, u64),
    pub level: u32,
    pub last_switch: Instant,
    pub medium_cid: Option<MeshCacheId>,
    pub far_cid: Option<MeshCacheId>,
    pub full_tris: u32,
    pub medium_tris: u32,
    pub far_tris: u32,
}

impl Default for PolyLodBodyState {
    fn default() -> Self {
        Self {
            content_key: (0, 0),
            level: 0,
            last_switch: Instant::now()
                .checked_sub(LOD_MIN_SWITCH_INTERVAL * 2)
                .unwrap_or_else(Instant::now),
            medium_cid: None,
            far_cid: None,
            full_tris: 0,
            medium_tris: 0,
            far_tris: 0,
        }
    }
}

impl AzraApp {
    /// Projected poly feature size in pixels (approx mean edge length on screen).
    /// Uses body world AABB + full-detail triangle count. Degenerate → large (force full).
    pub(crate) fn poly_projected_feature_px(
        &self,
        nid: NodeId,
        full_tris: u32,
        local_min: Vec3,
        local_max: Vec3,
    ) -> f32 {
        let world = self.scene.scene_graph.world_transform(nid);
        let corners = [
            Vec3::new(local_min.x, local_min.y, local_min.z),
            Vec3::new(local_max.x, local_min.y, local_min.z),
            Vec3::new(local_min.x, local_max.y, local_min.z),
            Vec3::new(local_max.x, local_max.y, local_min.z),
            Vec3::new(local_min.x, local_min.y, local_max.z),
            Vec3::new(local_max.x, local_min.y, local_max.z),
            Vec3::new(local_min.x, local_max.y, local_max.z),
            Vec3::new(local_max.x, local_max.y, local_max.z),
        ];
        let mut wmin = Vec3::splat(f32::MAX);
        let mut wmax = Vec3::splat(f32::MIN);
        for c in corners {
            let p = world.transform_point3(c);
            wmin = wmin.min(p);
            wmax = wmax.max(p);
        }
        let diameter = (wmax - wmin).length().max(1e-6);
        let center = (wmin + wmax) * 0.5;
        let cam = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let dist = (center - cam).length().max(1e-4);
        let h = self.viewport.viewport_rect.height().max(1.0);
        let tan_half = if self.render.ortho {
            // Ortho: treat camera.distance as half-height in world units.
            self.viewport.camera.distance.max(1e-4) / (h * 0.5)
        } else {
            (self.render.fov_deg.to_radians() * 0.5).tan().max(1e-6)
        };
        // Mean edge proxy: diameter / sqrt(tri_count).
        let edge = diameter / (full_tris as f32).sqrt().max(1.0);
        if self.render.ortho {
            // projected_px = edge / (world_per_pixel); world_per_pixel ≈ 2*half_h / h
            let world_per_px = (2.0 * self.viewport.camera.distance.max(1e-4)) / h;
            edge / world_per_px.max(1e-8)
        } else {
            edge * (h * 0.5) / (dist * tan_half)
        }
    }

    /// True when this body must stay at full display LOD (edit focus).
    fn poly_lod_force_full(&self, nid: NodeId) -> bool {
        if self.modeling_context == crate::ModelingContext::Polygonal {
            if self.sel.usel.active_node() == Some(nid) {
                return true;
            }
            if self.poly_work_mode != crate::PolyWorkMode::Object {
                // Any poly edit sub-mode: keep all poly solids full (avoid cage/LOD mismatch).
                return true;
            }
        }
        false
    }

    /// Build / refresh medium+far decimated display meshes for `nid` if missing or stale.
    /// Coarse meshes are shared by `content_key` so Shift+D duplicates pay one decimate.
    fn poly_lod_ensure_coarse(
        &mut self,
        nid: NodeId,
        content_key: (u64, u64),
        full_cid: MeshCacheId,
    ) {
        let Some(cached) = self.cache.mesh_cache.get(full_cid) else {
            return;
        };
        let full_tris = cached.triangle_count;
        if full_tris < POLY_LOD_MIN_TRIS {
            return;
        }

        // Reuse shared pool when content key already has both levels.
        if let Some(shared) = self.poly_lod_shared.get(&content_key) {
            if shared.medium_cid.is_some() && shared.far_cid.is_some() {
                let level = self.poly_lod_cache.get(&nid).map(|s| s.level).unwrap_or(0);
                let last_switch = self
                    .poly_lod_cache
                    .get(&nid)
                    .map(|s| s.last_switch)
                    .unwrap_or_else(Instant::now);
                self.poly_lod_cache.insert(
                    nid,
                    PolyLodBodyState {
                        content_key,
                        level,
                        last_switch,
                        medium_cid: shared.medium_cid,
                        far_cid: shared.far_cid,
                        full_tris: shared.full_tris.max(full_tris),
                        medium_tris: shared.medium_tris,
                        far_tris: shared.far_tris,
                    },
                );
                return;
            }
        }

        let need_medium = self
            .poly_lod_shared
            .get(&content_key)
            .map(|s| s.medium_cid.is_none())
            .unwrap_or(true);
        let need_far = self
            .poly_lod_shared
            .get(&content_key)
            .map(|s| s.far_cid.is_none())
            .unwrap_or(true);
        if !need_medium && !need_far {
            if let Some(shared) = self.poly_lod_shared.get(&content_key).cloned() {
                let level = self.poly_lod_cache.get(&nid).map(|s| s.level).unwrap_or(0);
                let last_switch = self
                    .poly_lod_cache
                    .get(&nid)
                    .map(|s| s.last_switch)
                    .unwrap_or_else(Instant::now);
                self.poly_lod_cache.insert(
                    nid,
                    PolyLodBodyState {
                        content_key,
                        level,
                        last_switch,
                        medium_cid: shared.medium_cid,
                        far_cid: shared.far_cid,
                        full_tris,
                        medium_tris: shared.medium_tris,
                        far_tris: shared.far_tris,
                    },
                );
            }
            return;
        }

        // OFF THE FRAME THREAD (measured 2026-08-12: 628 ms per pass, fast-release, 323k tris —
        // two full decimations run back to back, twice during a single scene load). Two half-second
        // stalls in `poll` for a level the frame does not yet need is a Frame Budget Rule violation;
        // the work is pure CPU over a snapshot, so it belongs on a worker, exactly like
        // `ClusterBakeState`.
        //
        // One job per content key, and the key is checked again on delivery, so an edit landing
        // mid-build discards the result instead of installing a coarse mesh of the OLD geometry.
        // Until a result arrives the body simply draws at level 0 (full detail) — which is what it
        // did before the first build finished anyway, so there is no window where the viewport
        // shows something that is not the real mesh.
        if self.poly_lod_jobs.contains(&content_key) {
            return; // already building
        }
        // ONLY BUILD FOR A SETTLED MESH. Moving the decimation off-thread removed the 628 ms
        // stall but introduced a worse one: during an edit the content key is DIFFERENT every
        // frame, so this spawned a fresh 323k-tri decimation thread per frame, and every landed
        // result reset the LOD latch, which put `poll_poly_lod_policy` (and its
        // `poly_display_cache_key` walk) back into every single frame — measured at 10–13 ms/frame
        // across 160 consecutive frames of a scripted edit, versus once for the whole run before.
        // Async work that is re-issued faster than it completes is a leak, not an optimisation.
        //
        // Two gates, both structural: never while a drag episode is open (a coarse level built
        // from mid-gesture geometry is stale before it lands), and never on the first poll a key
        // is seen — it must survive one pass, which is the same settle latch the select buffer and
        // the geometry audit use.
        if self.pending_episode.is_some() || !self.poly_lod_prev_keys.contains(&content_key) {
            return;
        }
        // Source raster from persistent/imported path.
        let Some((verts, indices)) = self.cache.mesh_cache.pt_source_data(full_cid) else {
            return;
        };
        if indices.is_empty() || verts.is_empty() {
            return;
        }
        self.poly_lod_jobs.insert(content_key);
        let tx = self.poly_lod_tx.clone();
        let ratios = (POLY_LOD_RATIO[0], POLY_LOD_RATIO[1]);
        std::thread::spawn(move || {
            let src = DisplayMesh::from_raster(&verts, &indices);
            let build = |ratio: f32| -> (Vec<[f32; 8]>, Vec<u32>, u32) {
                let mut p = DecimateParams::default();
                p.ratio = ratio;
                p.mode = DecimateMode::Fast;
                let out =
                    apply_display_mesh_op(&DisplayMeshOp::Decimate { params: p }, src.clone());
                let (v, i) = out.to_raster();
                let tris = (i.len() / 3) as u32;
                (v, i, tris)
            };
            let medium = if need_medium { Some(build(ratios.0)) } else { None };
            let far = if need_far { Some(build(ratios.1)) } else { None };
            let _ = tx.send(PolyLodBakeResult { content_key, full_tris, medium, far });
        });
    }

    /// Poll phase: install finished coarse LOD meshes. Mesh-cache writes stay on the main thread
    /// (the worker only produces raster arrays), and a result whose content key is no longer wanted
    /// is dropped rather than installed.
    pub(crate) fn poll_poly_lod_results(&mut self) {
        while let Ok(r) = self.poly_lod_rx.try_recv() {
            self.poly_lod_jobs.remove(&r.content_key);
            let mut shared = self
                .poly_lod_shared
                .get(&r.content_key)
                .cloned()
                .unwrap_or_default();
            shared.full_tris = r.full_tris;
            if let Some((v, i, tris)) = r.medium {
                let cid = shared
                    .medium_cid
                    .unwrap_or_else(|| self.cache.mesh_cache.alloc_id());
                self.cache.mesh_cache.update_editable_mesh(cid, (v, i), tris);
                shared.medium_cid = Some(cid);
                shared.medium_tris = tris;
            }
            if let Some((v, i, tris)) = r.far {
                let cid = shared
                    .far_cid
                    .unwrap_or_else(|| self.cache.mesh_cache.alloc_id());
                self.cache.mesh_cache.update_editable_mesh(cid, (v, i), tris);
                shared.far_cid = Some(cid);
                shared.far_tris = tris;
            }
            self.poly_lod_shared.insert(r.content_key, shared);
            // A new coarse level exists: the LOD policy must reconsider on the next pass.
            self.poly_lod_latch_gen = u64::MAX;
        }
    }

    /// Drop per-body LOD state (content changed). Shared coarse meshes stay until
    /// `poly_lod_gc_shared` or project clear — other bodies may still reference them.
    pub(crate) fn poly_lod_invalidate(&mut self, nid: NodeId) {
        if let Some(s) = self.poly_lod_cache.remove(&nid) {
            // Drop shared entry only if no other body still uses this content key.
            let key = s.content_key;
            let still = self
                .poly_lod_cache
                .values()
                .any(|o| o.content_key == key);
            if !still {
                self.poly_lod_shared.remove(&key);
            }
        }
    }

    /// Frame-end / bound: drop shared coarse meshes not referenced by any body.
    pub(crate) fn poly_lod_gc_shared(&mut self) {
        if self.poly_lod_shared.len() <= 32 {
            return;
        }
        let live: std::collections::HashSet<_> = self
            .poly_lod_cache
            .values()
            .map(|s| s.content_key)
            .collect();
        self.poly_lod_shared.retain(|k, _| live.contains(k));
    }

    /// HUD / P5 gate: `(l0, l1, l2, full_tris, drawn_tris)` over tracked poly LOD bodies.
    pub(crate) fn poly_lod_hud_stats(&self) -> (u32, u32, u32, u32, u32) {
        let mut l0 = 0u32;
        let mut l1 = 0u32;
        let mut l2 = 0u32;
        let mut full = 0u32;
        let mut drawn = 0u32;
        for s in self.poly_lod_cache.values() {
            full = full.saturating_add(s.full_tris);
            match s.level {
                1 => {
                    l1 += 1;
                    drawn = drawn.saturating_add(if s.medium_tris > 0 {
                        s.medium_tris
                    } else {
                        s.full_tris
                    });
                }
                2 => {
                    l2 += 1;
                    drawn = drawn.saturating_add(if s.far_tris > 0 {
                        s.far_tris
                    } else if s.medium_tris > 0 {
                        s.medium_tris
                    } else {
                        s.full_tris
                    });
                }
                _ => {
                    l0 += 1;
                    drawn = drawn.saturating_add(s.full_tris);
                }
            }
        }
        (l0, l1, l2, full, drawn)
    }

    /// Coarse camera bucket for LOD latch (B2). Quantizes orbit distance + target so small
    /// mouse noise does not re-run the policy; a real dolly/orbit crosses a bucket and re-eval.
    fn poly_lod_camera_bucket(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // ~5% distance bands + quantized look-at (world units / 0.25).
        let dist = self.viewport.camera.distance.max(1e-4);
        let dist_bucket = (dist.log2() * 20.0).round() as i32;
        dist_bucket.hash(&mut h);
        let t = self.viewport.camera.target;
        for c in [t.x, t.y, t.z] {
            ((c * 4.0).round() as i32).hash(&mut h);
        }
        // Orbit direction in coarse octants — enough for level switches without per-frame churn.
        let o = self.viewport.camera.orbit;
        let forward = o * Vec3::new(0.0, 0.0, 1.0);
        for c in [forward.x, forward.y, forward.z] {
            ((c * 4.0).round() as i32).hash(&mut h);
        }
        self.render.ortho.hash(&mut h);
        ((self.render.fov_deg * 2.0).round() as i32).hash(&mut h);
        h.finish()
    }

    /// Poll-phase / pre-draw: choose LOD level per poly body (rate-limited).
    /// Poll-phase / pre-draw: choose LOD level per poly body (rate-limited).
    pub(crate) fn poll_poly_lod_policy(&mut self) {
        if !self.cache.poly_lod_auto {
            return;
        }
        // B2: latch on (camera-distance bucket + poly edit generation). Idle cost ~0 —
        // previously the "rate-limited" body loop still scanned every frame (and paid
        // content_signature via poly_display_cache_key on every large body).
        // NOT MID-DRAG. The latch below is keyed on `poly_edit_generation`, which a live edit
        // bumps every frame, so the pass ran EVERY frame of a drag — and the pass itself is not
        // cheap: it calls `poly_display_cache_key` per body, which walks `content_signature`.
        // Measured at ~10 ms/frame across 174 frames of a 200-frame scripted edit. Gating the
        // BUILDS alone was not enough (that was the 2026-08-12 mistake — the spawn was gated, the
        // pass was not, and the spike count did not move). A level switch mid-gesture is pointless
        // anyway: `poly_lod_ensure_coarse` already refuses to build from mid-gesture geometry, so
        // the pass could only spend time deciding nothing.
        if self.pending_episode.is_some() {
            return;
        }
        let cam_bucket = self.poly_lod_camera_bucket();
        let gen = self.poly_edit_generation;
        if self.poly_lod_latch_gen == gen && self.poly_lod_latch_cam_bucket == cam_bucket {
            return;
        }

        // Collect work list (avoid borrow conflicts).
        // C-scale: check triangle_count BEFORE content_signature — 10k tiny cages were
        // hashing full half-edge graphs every frame (~28 ms) only to skip under MIN_TRIS.
        let mut work: Vec<(NodeId, MeshCacheId, (u64, u64), u32, Vec3, Vec3)> = Vec::new();
        let mut dropped_below: Vec<NodeId> = Vec::new();
        for (nid, node) in self.scene.scene_graph.poly_bodies() {
            if !node.visible {
                continue;
            }
            let NodePayload::PolygonalBody {
                mesh,
                mesh_cache_id: Some(cid),
                modifiers,
                ..
            } = &node.payload
            else {
                continue;
            };
            let Some(cached) = self.cache.mesh_cache.get(*cid) else {
                continue;
            };
            if cached.triangle_count < POLY_LOD_MIN_TRIS {
                // A body that FALLS below the threshold must not keep its old LOD state:
                // `poly_draw_mesh_cache_id` would go on serving the stale coarse cid
                // (level 1/2), which after removing a Decimate modifier looks exactly
                // like the modifier is still applied. Skipping alone leaves that state.
                if self.poly_lod_cache.contains_key(&nid) {
                    dropped_below.push(nid);
                }
                continue;
            }
            let key = self.poly_display_cache_key(nid, mesh, modifiers);
            work.push((
                nid,
                *cid,
                key,
                cached.triangle_count,
                cached.local_min,
                cached.local_max,
            ));
        }

        // Keys seen this pass — next pass uses this as the "survived one poll" settle set.
        let keys_this_pass: std::collections::HashSet<(u64, u64)> =
            work.iter().map(|(_, _, k, _, _, _)| *k).collect();

        for nid in dropped_below {
            self.poly_lod_invalidate(nid);
            self.structural_draw_stale = true; // draw list still references the coarse cid
        }

        let now = Instant::now();
        for (nid, full_cid, key, full_tris, lmin, lmax) in work {
            if self.poly_lod_force_full(nid) {
                if let Some(s) = self.poly_lod_cache.get_mut(&nid) {
                    if s.level != 0 {
                        s.level = 0;
                        s.last_switch = now;
                        self.structural_draw_stale = true;
                    }
                    s.content_key = key;
                    s.full_tris = full_tris;
                }
                continue;
            }

            self.poly_lod_ensure_coarse(nid, key, full_cid);

            let px = self.poly_projected_feature_px(nid, full_tris, lmin, lmax);
            let cur = self.poly_lod_cache.get(&nid).map(|s| s.level).unwrap_or(0);
            let want = choose_lod_level(px, cur, LOD_MAX_LEVEL);
            if want == cur {
                continue;
            }
            let Some(s) = self.poly_lod_cache.get_mut(&nid) else {
                continue;
            };
            if now.duration_since(s.last_switch) < LOD_MIN_SWITCH_INTERVAL {
                continue;
            }
            // Step at most one level per interval (smooth).
            let next = if want > cur { cur + 1 } else { cur.saturating_sub(1) };
            s.level = next;
            s.last_switch = now;
            s.full_tris = full_tris;
            s.content_key = key;
            // LOD swap changes MeshCacheId on the structural draw list — force rebuild.
            // Camera-only frames without a level switch keep the cached InstanceData array.
            self.structural_draw_stale = true;
        }

        // Bound cache size.
        if self.poly_lod_cache.len() > 64 {
            let live: std::collections::HashSet<_> = self
                .scene
                .scene_graph
                .poly_bodies()
                .map(|(id, _)| id)
                .collect();
            self.poly_lod_cache.retain(|k, _| live.contains(k));
        }
        self.poly_lod_gc_shared();

        self.poly_lod_prev_keys = keys_this_pass;
        // Latch only after a full pass so a mid-pass early-return cannot leave stale levels.
        self.poly_lod_latch_gen = gen;
        self.poly_lod_latch_cam_bucket = cam_bucket;
    }

    /// MeshCacheId to draw for a poly body under current LOD policy (full if disabled/forced).
    pub(crate) fn poly_draw_mesh_cache_id(
        &self,
        nid: NodeId,
        full_cid: MeshCacheId,
    ) -> MeshCacheId {
        if !self.cache.poly_lod_auto || self.poly_lod_force_full(nid) {
            return full_cid;
        }
        let Some(s) = self.poly_lod_cache.get(&nid) else {
            return full_cid;
        };
        match s.level {
            1 => s.medium_cid.unwrap_or(full_cid),
            2 => s.far_cid.or(s.medium_cid).unwrap_or(full_cid),
            _ => full_cid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdf_gpu_mesh::choose_lod_level;

    #[test]
    fn poly_lod_reuses_sdf_chooser_stability() {
        // Sanity: our levels map to the same pure function as SDF O2b.
        for i in 0..500 {
            let px = i as f32 * 0.01;
            let a = choose_lod_level(px, 0, 2);
            assert_eq!(choose_lod_level(px, a, 2), a);
        }
    }

    #[test]
    fn poly_lod_ratios_reduce() {
        // 50% and 15% ratios must be strictly decreasing when both applied.
        assert!(POLY_LOD_RATIO[0] > POLY_LOD_RATIO[1]);
        assert!(POLY_LOD_RATIO[1] > 0.0);
    }
}
