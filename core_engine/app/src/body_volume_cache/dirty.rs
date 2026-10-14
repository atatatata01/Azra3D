//! Dirty marking — mark_body_dirty, mark_prim_dirty, dirty region management.

use std::time::Instant;
use glam::Vec3;
use core_math::scene_graph::{SceneGraph, NodeId};
use super::{BodyVolumeCache, BakeDirtyRegion, MAX_DIRTY_REGIONS, BODY_LOCAL_PRIMS};

impl BodyVolumeCache {
    /// Mark a body as needing rebake. Clears TransformSafe state for this body.
    /// This triggers a FULL rebake — dirty_regions for this body are cleared.
    ///
    /// If the body's cache was cloned from a duplicate source (`shared_source` is
    /// set), the shared flag is cleared so the next bake is fully independent.
    pub(crate) fn mark_body_dirty(&mut self, body_id: NodeId) {
        // [HealTrace] settle-scoped dirty-mark provenance: a mark landing inside the heal
        // window resets last_dirty_time and can chain another full bake — name the moment.
        // Parked behind crate::perf_spam_on() after the phantom-edit loop was resolved (2026-07-09).
        if crate::perf_spam_on() && self.heal_tracing() {
            eprintln!("[HealTrace] mark_body_dirty body={:?} (inside heal window)", body_id);
        }
        self.dirty_bodies.insert(body_id);
        self.transform_safe_bodies.remove(&body_id);
        self.poison_rigid(body_id); // structural change — never rigid (P1-#6b)
        // Full bake — discard any pending partial regions
        self.dirty_regions.remove(&body_id);
        // Cancel any active progressive resolution cascade — the new edit
        // will trigger a fresh bake (possibly starting a new cascade).
        self.cancel_cascade(body_id);
        // If this body was using a cloned (shared) cache from a duplicate, clear
        // the flag so the next bake produces an independent grid.
        if let Some(entry) = self.body_grids.get_mut(&body_id) {
            entry.shared_source = None;
        }
        // Stamp dirty time for debounce
        self.last_dirty_time = Some(Instant::now());
    }

    /// Light-dirty Chebyshev neighbour terrain tiles (no cascade cancel).
    fn dirty_terrain_neighbours_light(&mut self, body_id: NodeId, sg: &SceneGraph) {
        if !crate::terrain::is_terrain_bake_body(sg, body_id) {
            return;
        }
        for n in crate::terrain::terrain_neighbour_body_ids(sg, body_id) {
            self.dirty_bodies.insert(n);
            self.transform_safe_bodies.remove(&n);
        }
        self.last_dirty_time = Some(Instant::now());
    }

    /// Mark a body dirty and, for Mesh Terrain tiles, dirty Chebyshev neighbours so
    /// straddling prims re-bake into the seam apron (L2 dual membership).
    pub(crate) fn mark_body_dirty_terrain_fanout(&mut self, body_id: NodeId, sg: &SceneGraph) {
        self.mark_body_dirty(body_id);
        self.dirty_terrain_neighbours_light(body_id, sg);
    }

    /// Mark a single prim as dirty within its body, enabling partial bake.
    ///
    /// Computes the prim's body-local AABB (position ± bounding_radius, offset by
    /// body.origin) and adds it as a dirty region. The next CPU bake will only
    /// re-evaluate voxels within the union of dirty regions (all prims still
    /// participate in SDF evaluation at each point — the region limits WHERE,
    /// not WHAT is evaluated).
    ///
    /// Falls back to full-body dirty if the prim can't be found or the body has
    /// no existing cached grid (partial bake needs a base grid to clone).
    pub(crate) fn mark_prim_dirty(&mut self, body_nid: NodeId, prim_uid: u32, sg: &SceneGraph) {
        // No existing grid to clone from → fall back to full bake
        if !self.body_grids.contains_key(&body_nid) {
            // Throttled: a slider drag on an entry-less body marks EVERY FRAME and a
            // per-frame eprintln floods the console for the whole drag (owner log: thousands
            // of identical lines). 1-in-60 keeps the signal without the flood.
            if cfg!(debug_assertions) {
                use std::cell::Cell;
                thread_local! { static N: Cell<u32> = Cell::new(0); }
                if N.with(|n| { let v = n.get().wrapping_add(1); n.set(v); v % 60 == 1 }) {
                    eprintln!("[DirtyFallback] body={:?} reason=no_grid_in_mark_prim_dirty (1-in-60 shown)", body_nid);
                }
            }
            self.mark_body_dirty_terrain_fanout(body_nid, sg);
            return;
        }

        // Find the prim in the body's scene graph data
        let body_origin = sg.body_origin(body_nid).unwrap_or(Vec3::ZERO);
        let prim = sg.body_primitives(body_nid)
            .and_then(|prims| prims.iter().find(|p| p.prim_uid == prim_uid));

        let prim = match prim {
            Some(p) => p,
            None => {
                // Prim not found directly — may be in a nested child body.
                // Fall back to full bake.
                self.mark_body_dirty_terrain_fanout(body_nid, sg);
                return;
            }
        };

        // Compute body-local AABB using tight per-axis half-extents
        let local_pos = if BODY_LOCAL_PRIMS {
            prim.position // already body-local
        } else {
            prim.position - body_origin
        };
        let he = prim.bounding_aabb_half_extents();

        self.mark_prim_dirty_aabb(body_nid, local_pos, he);
        // L2 seam: neighbour tiles re-bake so straddling stamps appear in both grids.
        self.dirty_terrain_neighbours_light(body_nid, sg);
    }

    /// Mark a body-local position + radius as dirty for partial bake.
    ///
    /// Use when you know the position/radius explicitly (e.g. capturing old
    /// position before a transform update). Falls back to full-body dirty if
    /// no existing grid is available for partial bake.
    #[deprecated(note = "Use mark_prim_dirty_aabb() instead — provides tighter dirty regions")]
    #[allow(dead_code)]
    pub(crate) fn mark_prim_dirty_at(&mut self, body_nid: NodeId, local_pos: Vec3, radius: f32) {
        self.poison_rigid(body_nid); // generic mark — poison by default (P1-#6b)
        if !self.body_grids.contains_key(&body_nid) {
            if cfg!(debug_assertions) {
                eprintln!("[DirtyFallback] body={:?} reason=no_grid_in_mark_prim_dirty_at", body_nid);
            }
            self.mark_body_dirty(body_nid);
            return;
        }

        let (grid_min, grid_max) = self.body_bounds.get(&body_nid)
            .copied()
            .unwrap_or_else(|| {
                self.body_grids.get(&body_nid)
                    .map(|e| (e.grid_min, e.grid_max))
                    .unwrap_or((Vec3::splat(-self.bounds as f32), Vec3::splat(self.bounds as f32)))
            });
        let grid_extent = grid_max - grid_min;
        let voxel_size = if self.resolution > 1 {
            grid_extent / (self.resolution - 1) as f32
        } else {
            grid_extent
        };
        let voxel_margin = voxel_size * 2.0;

        let region = BakeDirtyRegion {
            local_min: (local_pos - Vec3::splat(radius) - voxel_margin).max(grid_min),
            local_max: (local_pos + Vec3::splat(radius) + voxel_margin).min(grid_max),
        };

        self.add_dirty_region(body_nid, region);

        if crate::perf_spam_on() && self.heal_tracing() {
            eprintln!("[HealTrace] mark_prim_dirty_at body={:?} (inside heal window)", body_nid);
        }
        self.dirty_bodies.insert(body_nid);
        self.transform_safe_bodies.remove(&body_nid);
        self.last_dirty_time = Some(Instant::now());
    }

    /// Mark a tight per-axis AABB region as dirty for partial bake (body-local coordinates).
    ///
    /// Unlike `mark_prim_dirty_at()` which uses a scalar radius (sphere bound),
    /// this takes per-axis half-extents from `bounding_aabb_half_extents()` for
    /// a much tighter dirty region on non-spherical prims.
    pub(crate) fn mark_prim_dirty_aabb(&mut self, body_nid: NodeId, local_pos: Vec3, half_extents: Vec3) {
        // P1-#6b: a generic AABB mark means "the field changed here" for an UNKNOWN reason —
        // poison rigidity by DEFAULT. The transform writer's pure-translation edits route
        // through `mark_prim_transform_dirty` (which notes instead), so a future caller of
        // this generic entry can never silently claim a rigid frame.
        self.poison_rigid(body_nid);
        self.mark_prim_dirty_aabb_inner(body_nid, local_pos, half_extents);
    }

    /// Region-adding core shared by the generic (poisoning) mark above and the
    /// transform-writer mark (`mark_prim_transform_dirty`, which notes-or-poisons itself).
    /// Has NO rigid-accum effect of its own.
    fn mark_prim_dirty_aabb_inner(&mut self, body_nid: NodeId, local_pos: Vec3, half_extents: Vec3) {
        if !self.body_grids.contains_key(&body_nid) {
            if cfg!(debug_assertions) {
                eprintln!(
                    "[DirtyFallback] body={:?} reason=no_grid_in_mark_prim_dirty_aabb pending_regions={}",
                    body_nid,
                    self.dirty_regions.get(&body_nid).map(|v| v.len()).unwrap_or(0)
                );
            }
            self.mark_body_dirty(body_nid);
            return;
        }

        let (grid_min, grid_max) = self.body_bounds.get(&body_nid)
            .copied()
            .unwrap_or_else(|| {
                self.body_grids.get(&body_nid)
                    .map(|e| (e.grid_min, e.grid_max))
                    .unwrap_or((Vec3::splat(-self.bounds as f32), Vec3::splat(self.bounds as f32)))
            });
        let grid_extent = grid_max - grid_min;
        let voxel_size = if self.resolution > 1 {
            grid_extent / (self.resolution - 1) as f32
        } else {
            grid_extent
        };
        let voxel_margin = voxel_size * 2.0;

        let region = BakeDirtyRegion {
            local_min: (local_pos - half_extents - voxel_margin).max(grid_min),
            local_max: (local_pos + half_extents + voxel_margin).min(grid_max),
        };

        self.add_dirty_region(body_nid, region);

        if crate::perf_spam_on() && self.heal_tracing() {
            eprintln!("[HealTrace] mark_prim_dirty_aabb body={:?} (inside heal window)", body_nid);
        }
        self.dirty_bodies.insert(body_nid);
        self.transform_safe_bodies.remove(&body_nid);
        self.last_dirty_time = Some(Instant::now());
    }

    /// P1-#6b — the TRANSFORM WRITER's dirty mark (`write_prim_transform` is the sole caller):
    /// adds the old + new prim AABB regions exactly like two `mark_prim_dirty_aabb` calls, and
    /// records the edit's rigidity verdict: `translate = Some(delta)` (rotation and scale
    /// unchanged) accumulates toward the rigid-translate fast path; `None` (rotate/scale mixed
    /// in) poisons this burst. Every OTHER dirty entry point poisons by default — this is the
    /// only fn that can note.
    pub(crate) fn mark_prim_transform_dirty(
        &mut self,
        body_nid: NodeId,
        prim_uid: u32,
        old_pos: Vec3, old_he: Vec3,
        new_pos: Vec3, new_he: Vec3,
        translate: Option<Vec3>,
    ) {
        match translate {
            Some(d) => self.note_prim_translate(body_nid, prim_uid, d),
            None => self.poison_rigid(body_nid),
        }
        self.mark_prim_dirty_aabb_inner(body_nid, old_pos, old_he);
        self.mark_prim_dirty_aabb_inner(body_nid, new_pos, new_he);
    }

    /// P1-#6b: accumulate a pure-translation prim edit (see `RigidAccum`). A poisoned burst
    /// stays poisoned — the sum is still tracked but the consume will refuse it.
    fn note_prim_translate(&mut self, body_nid: NodeId, prim_uid: u32, delta: Vec3) {
        let acc = self.rigid_accum.entry(body_nid).or_default();
        *acc.deltas.entry(prim_uid).or_insert(Vec3::ZERO) += delta;
    }

    /// P1-#6b: a non-translation field change arrived for this body — rigidity is off the
    /// table until the accumulation is consumed (sticky; see `RigidAccum.poisoned`).
    /// ALWAYS materializes the entry: a poison arriving BEFORE the burst's first translate
    /// note must still dominate (slider tick then prim move in one burst — an if-present
    /// poison would let the later note claim a clean rigid frame over the unbaked slider
    /// change). Every mark_* setter also inserts into `dirty_bodies`, and the consume runs
    /// for every dirty body each trigger, so poisoned entries never accumulate unboundedly.
    pub(super) fn poison_rigid(&mut self, body_nid: NodeId) {
        self.rigid_accum.entry(body_nid).or_default().poisoned = true;
    }

    /// Add a dirty region for a body, merging with overlapping/adjacent regions.
    /// If regions exceed MAX_DIRTY_REGIONS, merge all into one.
    pub(super) fn add_dirty_region(&mut self, body_nid: NodeId, region: BakeDirtyRegion) {
        let regions = self.dirty_regions.entry(body_nid).or_default();

        // Voxel-size margin for adjacency test
        let grid_extent = self.body_bounds.get(&body_nid)
            .map(|(mn, mx)| *mx - *mn)
            .unwrap_or(Vec3::splat(self.bounds as f32 * 2.0));
        let adjacency_margin = if self.resolution > 1 {
            (grid_extent.x / (self.resolution - 1) as f32).max(0.001)
        } else {
            0.01
        };

        // Try to merge with existing overlapping regions
        let mut merged = region;
        let mut i = 0;
        while i < regions.len() {
            if regions[i].overlaps_or_adjacent(&merged, adjacency_margin) {
                merged.merge(&regions[i]);
                regions.swap_remove(i);
            } else {
                i += 1;
            }
        }
        regions.push(merged);

        // Cap: if too many regions, merge all into one
        if regions.len() > MAX_DIRTY_REGIONS {
            let mut combined = regions[0];
            for r in regions.iter().skip(1) {
                combined.merge(r);
            }
            regions.clear();
            regions.push(combined);
        }
    }

    /// Mark a body as transform-safe: the cached mesh/volume is still valid,
    /// only the instance transform (body.origin) changed. No rebake needed.
    /// The raster pipeline reads body.origin from the scene graph each frame
    /// for the instance matrix, so this is effectively a no-op on the cache
    /// side — it just prevents the body from being marked dirty.
    pub(crate) fn mark_transform_safe(&mut self, body_id: NodeId) {
        self.transform_safe_bodies.insert(body_id);
    }
}
