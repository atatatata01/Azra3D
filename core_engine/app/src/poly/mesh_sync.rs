use core_math::half_edge::HalfEdgeMesh;
use core_math::scene_graph::{NodeId, NodePayload, PolyModifier};
use std::sync::Arc;

use crate::AzraApp;

// ============================================================================
// Bevel SMART SELECT — the two angle thresholds, side by side so they stay
// visibly related and tunable. Both measure the DEVIATION between the two face
// normals (`edge_dihedral_cos` returns `dot(n1, n2)`: 1.0 = coplanar, 0.0 = 90°).
// ============================================================================

/// READ side (the Bevel modifier): an edge whose faces deviate by LESS than this is inside a
/// ROUND, never a corner, so Smart Select skips it whatever its provenance says. Exactly-coplanar
/// edges are deliberately EXEMPT — a knife cut is coplanar and must keep beveling (owner
/// 2026-08-05), so flat seams are left entirely to tags.
pub(crate) const SMART_SELECT_TANGENT_DEG: f32 = 30.0;

/// WRITE side (the destructive Extrude): an extrude-created edge whose faces deviate by LESS
/// than this is a flush / shallow seam and gets TAGGED out of the bevel; steeper ones are a
/// normal extrude's wall and cap creases and stay beveled. **Owner-chosen, 2026-08-05: "extrude:
/// we should use angle based to tag or not, maybe all 20 degree under are tagged out of bevel."**
/// Deliberately BELOW [`SMART_SELECT_TANGENT_DEG`]: the geometry rule already covers 0°..30° for
/// rounds, and this narrower band only marks the near-coplanar extrude seams that geometry alone
/// cannot separate from a knife cut.
pub(crate) const EXTRUDE_TAG_TANGENT_DEG: f32 = 20.0;

/// Runtime-only display mesh entry (POLY_VIEWPORT_PERF P1a/P1b).
/// Key = `(base content_signature, stack_key)` — same identity as the old bug-#6 skip tuple.
/// Never serialized into FL4.
#[derive(Clone)]
pub(crate) struct PolyDisplayMeshCache {
    pub key: (u64, u64),
    pub mesh: Arc<HalfEdgeMesh>,
    /// Arc identity of the *base* body mesh when this entry was built (O(1) idle skip).
    pub arc_ptr: Option<usize>,
    /// `face_count` of that base mesh (cheap secondary guard).
    pub face_count: Option<usize>,
}

/// One prefix slot: mesh after processing `modifiers[0..=index]` (P2a).
#[derive(Clone)]
pub(crate) struct PolyPrefixEntry {
    pub prefix_hash: u64,
    pub mesh: Arc<HalfEdgeMesh>,
    pub approx_bytes: usize,
}

/// Per-body prefix ladder (P2a). `entries[i]` = result after first `i+1` stack slots.
/// Never serialized into FL4.
#[derive(Clone, Default)]
pub(crate) struct PolyPrefixBodyCache {
    pub base_sig: u64,
    pub entries: Vec<Option<PolyPrefixEntry>>,
    pub last_used: u64,
    pub total_bytes: usize,
}

/// Soft host-memory budget for all prefix meshes combined (~256 MiB).
const POLY_PREFIX_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// Max bodies that may hold a prefix ladder at once.
const POLY_PREFIX_MAX_BODIES: usize = 16;

/// Floor for the interactive poly solid's staleness ceiling (~2 frames at 60 fps).
const POLY_SYNC_MIN_STALE_MS: f32 = 33.0;
/// Ceiling for it. Past this the surface genuinely reads as lagging, so a mesh heavy enough to
/// need more than this is telling you the sync belongs on a worker thread, not that the ceiling
/// should keep rising.
const POLY_SYNC_MAX_STALE_MS: f32 = 200.0;

/// How stale the solid may get, ADAPTED to what a sync actually costs on this mesh.
///
/// A fixed 33 ms ceiling is self-defeating once the sync itself costs more than that: forcing a
/// 40 ms sync every 33 ms guarantees a stalled frame roughly half the time, which is exactly the
/// ~20 fps floor measured on the 323k body (48 frames at 48–54 ms, `mesh=35–40`, behind a healthy
/// median). Scaling the interval to ~4x the measured cost keeps the frame thread mostly free
/// regardless of mesh size, and a light mesh still lands on the 33 ms floor, i.e. unchanged.
#[inline]
fn poly_sync_stale_limit_ms(cost_ms: f32) -> f32 {
    (cost_ms * 4.0).clamp(POLY_SYNC_MIN_STALE_MS, POLY_SYNC_MAX_STALE_MS)
}

impl AzraApp {
    /// AZRA_MASTER_ROADMAP Phase B.4: how many nodes reference this display MeshCacheId.
    pub(crate) fn mesh_cache_user_count(
        &self,
        cid: core_math::scene_graph::MeshCacheId,
    ) -> usize {
        let mut n = 0usize;
        for (_, node) in self.scene.scene_graph.all_nodes() {
            let hit = match &node.payload {
                NodePayload::PolygonalBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::NurbsBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::SdfBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::VoxelBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::SdfObject {
                    mesh_cache_id: Some(c),
                    ..
                } => *c == cid,
                NodePayload::ImportedMesh { mesh_cache_id, .. } => *mesh_cache_id == cid,
                _ => false,
            };
            if hit {
                n += 1;
            }
        }
        n
    }

    /// Before writing GPU mesh bytes for `nid`, break a shared MeshCacheId so siblings keep
    /// their display (COW). No-op when this body is the sole user.
    /// Call only when content actually diverges — identical Shift+D siblings skip write via
    /// `poly_mesh_gpu_key` and must not COW (B.4 / C.1).
    pub(crate) fn cow_mesh_cache_id_before_write(&mut self, nid: NodeId) {
        let cid = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody {
                mesh_cache_id: Some(c),
                ..
            })
            | Some(NodePayload::NurbsBody {
                mesh_cache_id: Some(c),
                ..
            })
            | Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            })
            | Some(NodePayload::VoxelBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            Some(NodePayload::ImportedMesh { mesh_cache_id, .. }) => *mesh_cache_id,
            _ => return,
        };
        if self.mesh_cache_user_count(cid) <= 1 {
            return;
        }
        let new_cid = self.cache.mesh_cache.alloc_id();
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            match &mut node.payload {
                NodePayload::PolygonalBody { mesh_cache_id, .. }
                | NodePayload::NurbsBody { mesh_cache_id, .. }
                | NodePayload::SdfBody { mesh_cache_id, .. }
                | NodePayload::VoxelBody { mesh_cache_id, .. } => {
                    *mesh_cache_id = Some(new_cid);
                }
                NodePayload::ImportedMesh { mesh_cache_id, .. } => {
                    *mesh_cache_id = new_cid;
                }
                _ => {}
            }
        }
    }

    /// Count MeshCacheIds used by ≥2 bodies (B.4 / instancing gate metric).
    pub(crate) fn shared_mesh_cache_id_count(&self) -> u32 {
        use std::collections::HashMap;
        let mut counts: HashMap<core_math::scene_graph::MeshCacheId, u32> = HashMap::new();
        for (_, node) in self.scene.scene_graph.all_nodes() {
            let cid = match &node.payload {
                NodePayload::PolygonalBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::NurbsBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::SdfBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::VoxelBody {
                    mesh_cache_id: Some(c),
                    ..
                }
                | NodePayload::SdfObject {
                    mesh_cache_id: Some(c),
                    ..
                } => Some(*c),
                NodePayload::ImportedMesh { mesh_cache_id, .. } => Some(*mesh_cache_id),
                _ => None,
            };
            if let Some(c) = cid {
                *counts.entry(c).or_insert(0) += 1;
            }
        }
        counts.values().filter(|&&n| n >= 2).count() as u32
    }

    /// Flush deferred mesh cache sync (called on confirm/cancel of drag operations).
    pub(crate) fn poly_flush_mesh_dirty(&mut self) {
        if let Some(nid) = self.poly_mesh_dirty.take() {
            self.poly_display_dirty.insert(nid);
        }
        self.poly_drain_display_dirty();
    }

    /// PLAN_FOR_GROK_VIEWPORT_PERF P1: mark body for display sync; drained once per frame.
    /// Prefer this over `poly_sync_mesh_cache` for topology ops that do not need same-line solid.
    pub(crate) fn poly_request_display_sync(&mut self, nid: NodeId) {
        self.poly_sync_marks_this_frame =
            self.poly_sync_marks_this_frame.saturating_add(1);
        self.poly_display_dirty.insert(nid);
        self.bump_poly_edit_generation();
    }

    /// Drain coalesced display dirty set — at most one full sync per body.
    /// Call once after input / before Hybrid mesh upload (see `app_update`).
    ///
    /// R6a: multi-body / recovery backlog → **exactly one body per frame**.
    /// `ensure_poly_meshes` only enqueues here; never `poly_sync_mesh_cache` inline.
    /// Single-body tool edits still same-frame.
    pub(crate) fn poly_drain_display_dirty(&mut self) {
        if self.poly_display_dirty.is_empty() {
            return;
        }
        let nids: Vec<NodeId> = self.poly_display_dirty.drain().collect();
        // Interactive single-body edit.
        if nids.len() == 1 {
            // BUDGETED DURING A DRAG (VIEWPORT_PERF 2026-08-12, the architectural change).
            //
            // This path used to be unconditional "same-frame solid", which is the whole edit
            // frame: a vertex grab changes the base mesh every frame, so the prefix ladder always
            // misses, the stack re-evaluates from a full base clone, the display re-triangulates
            // and the buffer is rewritten — measured at 33–65 ms per frame at 323k tris, every
            // frame, with nothing cacheable about it because the input genuinely changed.
            //
            // The display does not have to refresh at INPUT rate. `FrameScheduler` already owns
            // this decision for every other subsystem (bake readback, DC results, mesh uploads)
            // and this was the one interactive path opting out of it. Now it asks, using its own
            // measured cost as the hint, so the sync runs as often as the frame budget allows
            // instead of as often as the pointer moves.
            //
            // Two guarantees keep this honest rather than a dropped update:
            //  * a hard staleness ceiling — the solid can never lag more than
            //    `POLY_SYNC_MAX_STALE_MS`, so it stays visibly live during the gesture;
            //  * the body goes back on `poly_display_dirty`, and `app_update` requests a repaint
            //    while that queue is non-empty, so a skipped frame is always followed by a real
            //    one. Once the episode commits, `pending_episode` is None and the very next drain
            //    takes the unbudgeted branch below — so the SETTLED mesh is always synced in full,
            //    and a draft can never outlive the gesture.
            // Outside an episode (a one-shot edit) nothing is budgeted: it syncs immediately.
            let nid = nids[0];
            // Budgeted ONLY while a drag episode is open. The "always budgeted" version had a
            // structural hole: `can_run(Medium)` requires `remaining > cost_hint`, and a learned
            // cost of ~45 ms against the 12 ms frame budget is PERMANENTLY false — so every
            // one-shot sync (undo/redo above all) rode on nothing but the staleness ceiling, up to
            // 200 ms late and fragile (owner 2026-08-13: "ctrl z does not update the surface
            // back"). An undo must show its result THIS frame; only a gesture that will re-mark
            // the body next frame anyway is worth deferring.
            if self.pending_episode.is_none() {
                self.poly_sync_mesh_cache(nid);
                self.poly_sync_last_at = std::time::Instant::now();
                return;
            }
            // ALWAYS BUDGETED — not only inside a drag episode.
            //
            // The first version gated this on `pending_episode.is_some()`, which assumed every
            // repeated edit is a pointer drag. It is not: a modifier slider, a live parameter
            // scrub, or any tool that re-marks the body without opening an undo episode went
            // straight down the unbudgeted path and paid the full sync EVERY frame — owner
            // 2026-08-12, "still 17 fps when I edit", ~59 ms/frame, with my own harness showing 42
            // frames at 48–52 ms (`mesh=35–39`) hiding behind a 7.6 ms median.
            //
            // The staleness ceiling is what makes the episode test unnecessary. A ONE-SHOT edit is
            // stale by definition — nothing synced this body recently, so `stale_ms` is far past
            // the ceiling and it runs same-frame, exactly as before. Only a body being re-marked
            // FASTER than the frame budget allows is ever deferred, which is precisely the case
            // worth deferring, however the user is driving it.
            {
                let ticket = crate::frame_scheduler::WorkTicket {
                    class: crate::frame_scheduler::WorkClass::Medium,
                    cost_hint_ms: self.poly_sync_cost_ms.max(1.0),
                    name: "poly_display_sync_interactive",
                };
                let stale_ms = self.poly_sync_last_at.elapsed().as_secs_f32() * 1000.0;
                if !self.scheduler.can_run(&ticket)
                    && stale_ms < poly_sync_stale_limit_ms(self.poly_sync_cost_ms)
                {
                    self.scheduler.record_skipped(&ticket);
                    self.poly_display_dirty.insert(nid);
                    return;
                }
                let t = std::time::Instant::now();
                self.poly_sync_mesh_cache(nid);
                let ms = t.elapsed().as_secs_f32() * 1000.0;
                // EWMA so one anomalous frame cannot latch the hint high (or low) for the drag.
                self.poly_sync_cost_ms = self.poly_sync_cost_ms * 0.75 + ms * 0.25;
                self.poly_sync_last_at = std::time::Instant::now();
                self.scheduler.record(&ticket, ms);
                return;
            }
        }
        // Multi-body: one body this frame, re-queue the rest (progressive solids).
        let ticket = crate::frame_scheduler::WorkTicket {
            class: crate::frame_scheduler::WorkClass::Medium,
            cost_hint_ms: 16.0,
            name: "poly_display_sync",
        };
        let t_body = std::time::Instant::now();
        self.poly_sync_mesh_cache(nids[0]);
        let body_ms = t_body.elapsed().as_secs_f32() * 1000.0;
        self.scheduler.record(&ticket, body_ms);
        if body_ms > 100.0 {
            eprintln!(
                "[R6a] poly_display_sync body took {:.0}ms (one body/frame; progressive load)",
                body_ms
            );
        }
        for nid in nids.into_iter().skip(1) {
            self.poly_display_dirty.insert(nid);
        }
    }

    /// Schedule a re-sync of all poly mesh caches (undo/redo, process_dirty MESH,
    /// outliner bulk ops). **Never** runs `poly_sync_mesh_cache` inline — that was the
    /// R6a spawn-frame 10.9s `process_dirty` (MESH → poly_sync_all × 8 dense bodies).
    /// Bodies enqueue into `poly_display_dirty`; budgeted drain does one body/frame.
    pub(crate) fn poly_sync_all_meshes(&mut self) {
        self.bump_poly_edit_generation();
        let nids: Vec<NodeId> = self.scene.scene_graph.poly_bodies().map(|(nid, _)| nid).collect();
        for nid in nids {
            self.poly_display_dirty.insert(nid);
        }
    }

    /// Bump the poly edit generation and drop memoized `(csig, stack_key)` entries.
    /// Call from every mutation choke point — tools already route through these APIs.
    /// One counter invalidates pick mesh keys, wire stamps (via display key), and LOD latches.
    /// `#[track_caller]` so the churn detector below can name the CALL SITE, not this function.
    /// Costs nothing at runtime — `Location` is a compile-time `&'static`.
    #[inline]
    #[track_caller]
    pub(crate) fn bump_poly_edit_generation(&mut self) {
        self.poly_edit_generation = self.poly_edit_generation.wrapping_add(1);
        self.poly_display_key_memo.borrow_mut().clear();
        // Force LOD policy to re-evaluate once after the next camera/gen latch miss.
        self.poly_lod_latch_gen = u64::MAX;
        Self::note_gen_bump(std::panic::Location::caller());
    }

    /// GENERATION CHURN DETECTOR (VIEWPORT_PERF_2026-08-12).
    ///
    /// This counter is the app's mutation choke point, so every generation-keyed cache — display
    /// keys, pick mesh keys, wire stamps, LOD latches, the geometry-issue audit — degrades to "no
    /// cache" at exactly the rate it moves. The owner's 2026-08-12 capture shows it moving with
    /// `tool=Idle` and nothing being edited (`[ClusterBake]` stamps 47→61 in 1.6 s, each dropping
    /// the previous build as "stale (edited while building)"), which means several caches were
    /// paying full price at rest. Nothing should mutate a mesh when the user is not editing.
    ///
    /// Reports at most once a second, and only ABOVE a rate that no genuine interaction produces
    /// (a drag bumps once per frame; this needs sustained bumping with nothing to show for it), so
    /// a healthy session prints nothing and pays two integer compares per bump.
    fn note_gen_bump(loc: &'static std::panic::Location<'static>) {
        use std::cell::RefCell;
        /// A 60 Hz drag legitimately bumps ~60×/s. Above this, something is bumping without a user.
        const CHURN_PER_SEC: usize = 90;
        thread_local! {
            static WINDOW: RefCell<(std::time::Instant, Vec<(&'static std::panic::Location<'static>, usize)>)> =
                RefCell::new((std::time::Instant::now(), Vec::new()));
        }
        WINDOW.with(|w| {
            let Ok(mut w) = w.try_borrow_mut() else { return };
            let (start, sites) = &mut *w;
            match sites.iter_mut().find(|(l, _)| std::ptr::eq(*l, loc)) {
                Some((_, n)) => *n += 1,
                None => sites.push((loc, 1)),
            }
            if start.elapsed() < std::time::Duration::from_secs(1) {
                return;
            }
            let total: usize = sites.iter().map(|(_, n)| *n).sum();
            if total > CHURN_PER_SEC || crate::perf_spam_on() {
                sites.sort_by(|a, b| b.1.cmp(&a.1));
                let top: Vec<String> = sites
                    .iter()
                    .take(4)
                    .map(|(l, n)| format!("{}:{} x{}", l.file(), l.line(), n))
                    .collect();
                eprintln!("[GenChurn] {total} bumps/s | {}", top.join(" | "));
            }
            *start = std::time::Instant::now();
            sites.clear();
        });
    }

    /// Invalidate the screen-space pick cache (call after mesh or selection changes).
    pub(crate) fn poly_invalidate_pick_cache(&mut self) {
        self.poly_pick_cache.cam_key = 0;
    }

    /// Force a full solid-mesh rebuild for `nid` after topology ops (dissolve / face delete).
    ///
    /// IMPORTANT: do **not** set `gpu_mesh = None` before re-upload. That left PolygonalBody
    /// undrawn (full mesh "vanishes") whenever the same-frame upload was skipped or failed.
    /// `update_editable_mesh` preserves the GPU buffers and `upload_pending` rewrites them
    /// in place (or creates them if missing).
    pub(crate) fn poly_force_display_resync(&mut self, nid: NodeId) {
        self.bump_poly_edit_generation();
        self.poly_display_cache.remove(&nid);
        self.poly_wire_local_cache.remove(&nid);
        self.poly_prefix_cache_remove(nid);
        self.poly_lod_invalidate(nid);
        self.poly_sync_mesh_cache(nid);
    }

    /// Drop one body's prefix ladder and update the global byte counter.
    fn poly_prefix_cache_remove(&mut self, nid: NodeId) {
        if let Some(body) = self.poly_prefix_cache.remove(&nid) {
            self.poly_prefix_total_bytes = self
                .poly_prefix_total_bytes
                .saturating_sub(body.total_bytes);
        }
        // C1 OPEN-STROKE PIN (§7.4): the ACTIVE edit target's working mesh survives
        // prefix eviction / display resync — dropping it mid-stroke rebuilds from
        // pre-stroke deltas and wipes the open dabs.
        if !self.poly_sculpt_stroke_pinned(nid) {
            self.poly_sculpt_drop(nid);
        }
    }

    /// Evict LRU prefix bodies until under budget / body-count caps.
    fn poly_prefix_evict_if_needed(&mut self) {
        while self.poly_prefix_cache.len() > POLY_PREFIX_MAX_BODIES
            || self.poly_prefix_total_bytes > POLY_PREFIX_BUDGET_BYTES
        {
            let victim = self
                .poly_prefix_cache
                .iter()
                .min_by_key(|(_, b)| b.last_used)
                .map(|(id, _)| *id);
            let Some(vid) = victim else {
                break;
            };
            self.poly_prefix_cache_remove(vid);
        }
    }

    fn poly_mesh_approx_bytes(mesh: &HalfEdgeMesh) -> usize {
        // Conservative: positions+normals + HE + faces.
        mesh.vertices.len() * 48
            + mesh.half_edges.len() * 24
            + mesh.faces.len() * 16
    }

    /// Hash of `modifiers[0..end]` (same field layout as full stack hash) with boolean
    /// cutter **content** folded in — so editing a cutter invalidates prefixes past it.
    pub(crate) fn poly_stack_prefix_hash(
        &self,
        mods: &[PolyModifier],
        end: usize,
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let end = end.min(mods.len());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        Self::hash_modifier_stack_into(&mods[..end], &mut h);
        for m in &mods[..end] {
            if m.enabled
                && m.mod_type == core_math::scene_graph::PolyModifierType::Boolean
                && m.bool_cutter_nid != 0
            {
                m.bool_cutter_nid.hash(&mut h);
                if let Some(cn) = self.scene.scene_graph.get(NodeId(m.bool_cutter_nid)) {
                    if let NodePayload::PolygonalBody { mesh: cm, .. } = &cn.payload {
                        cm.content_signature().hash(&mut h);
                    }
                }
            }
        }
        h.finish()
    }

    /// True when this stack must be evaluated whole via the tagged path — no prefix ladder:
    /// Smart-Select Bevel needs provenance, Mesh Cleanup needs protected-edge tags (both
    /// exist only inside ONE evaluation pass). Single source for `apply_poly_modifiers_cached`
    /// and the Modifiers-panel stats fallback.
    pub(crate) fn poly_stack_needs_tagged_eval(mods: &[PolyModifier]) -> bool {
        mods.iter().any(|m| {
            m.enabled
                && ((m.mod_type == core_math::scene_graph::PolyModifierType::Bevel
                    && m.bevel_smart_select)
                    || m.mod_type == core_math::scene_graph::PolyModifierType::MeshCleanup
                    // An AUTHORED channel (polygroup / edge weight) is position-keyed off the
                    // BASE mesh, exactly like the Smart Select tags above — a reused prefix
                    // would hand the modifier an empty table and it would silently mask
                    // nothing. Same rule, same reason: evaluate such a stack whole.
                    || m.geo_channel.kind.needs_authored()
                    // Bevel reads painted W as a % of Width with channel left at All.
                    // Without this, the prefix ladder applies Bevel with empty auth and
                    // weights look dead (owner: "Weights (W) are always a % of Width" no-op).
                    || m.reads_edge_weight())
        })
    }

    /// Stamp the Modifiers panel's per-slot stats memo from the after-counts collected during
    /// an `apply_poly_modifiers_cached` walk. Counts (~16 bytes/slot) are captured from the
    /// intermediate meshes WHILE the walk holds them, so the stats survive even when
    /// `poly_prefix_evict_if_needed` immediately evicts the ladder that produced them — the
    /// self-eviction case where a ladder re-read would silently force the panel back into a
    /// full uncached stack walk per slider tick.
    fn stamp_poly_mod_stats(
        &mut self,
        nid: NodeId,
        base: &HalfEdgeMesh,
        base_sig: u64,
        modifiers: &[PolyModifier],
        slot_counts: &[(u32, u32)],
    ) {
        let mut stats = Vec::with_capacity(slot_counts.len());
        let mut prev = (base.vertex_count() as u32, base.face_count() as u32);
        for &cur in slot_counts {
            stats.push((prev.0, prev.1, cur.0, cur.1));
            prev = cur;
        }
        self.ui.poly_mod_prefix_stats =
            Some(((nid.0, base_sig, Self::poly_modifier_stack_hash(modifiers)), stats));
    }

    /// Uncached per-slot stats: one manual walk of the stack. Last-resort fallback for
    /// stacks the ladder cannot serve (tagged Smart-Bevel / Mesh Cleanup stacks). NOTE:
    /// evaluates WITHOUT tags — per-slot counts for tagged stacks are approximate where a
    /// tag-driven op would diverge (pre-existing panel behavior, kept).
    pub(crate) fn poly_mod_stats_uncached(
        &self,
        nid: NodeId,
        base: &HalfEdgeMesh,
        mods: &[PolyModifier],
    ) -> Vec<(u32, u32, u32, u32)> {
        let cutters = self.poly_ref_meshes_for(nid, mods);
        let centers = self.poly_mirror_centers_for(nid, mods);
        let mut running = base.clone();
        let mut stats = Vec::with_capacity(mods.len());
        let mut prov = core_math::mesh_boolean::BoolProvenance::default();
        // Authored channels ARE available here (we hold `self` + `nid` + the base), so pass the
        // real tables — an authored-channel modifier that changes counts must be counted with the
        // mask it will actually run with, not an empty one.
        let auth = self.poly_authored_channels(nid, base);
        for m in mods {
            let bv = running.vertex_count() as u32;
            let bf = running.face_count() as u32;
            if m.enabled {
                Self::apply_one_poly_modifier(
                    &mut running, m, &cutters, &mut prov, &Default::default(), &auth, &centers,
                );
            }
            let av = running.vertex_count() as u32;
            let af = running.face_count() as u32;
            stats.push((bv, bf, av, af));
        }
        stats
    }

    /// Apply stack with **prefix reuse** (P2a). On a param change to Mi, rebuilds from the
    /// longest matching prefix (typically i-1) so Subdivide is not re-run when scrubbing Array.
    /// `base_sig_hint`: a `content_signature()` of **this exact `base`**, already computed and
    /// known FRESH (not memo-served) by the caller. Purely an optimisation — `None` is always
    /// correct and simply recomputes. Callers get it from
    /// [`Self::poly_display_cache_key_fresh`], and must pass `None` whenever that reported the
    /// value came from the generation-keyed memo, because this ladder's whole safety argument is
    /// that `base_sig` describes actual mesh CONTENT.
    pub(crate) fn apply_poly_modifiers_cached(
        &mut self,
        nid: NodeId,
        base: &HalfEdgeMesh,
        modifiers: &[PolyModifier],
        cutters: &std::collections::HashMap<u64, HalfEdgeMesh>,
        base_sig_hint: Option<u64>,
    ) -> Arc<HalfEdgeMesh> {
        // Smart Select needs provenance, which exists only inside ONE evaluation pass. A
        // reused prefix would hand the Bevel empty tags, so the rule would switch itself off
        // depending on cache state -- two paths disagreeing about one operation, which
        // CLAUDE.md forbids. Evaluate such a stack whole; costs prefix reuse only when opted in.
        // Full re-eval when Smart Select Bevel needs provenance OR Mesh Cleanup needs
        // protected-edge tags (both keyed by position; prefix reuse would lose them).
        if Self::poly_stack_needs_tagged_eval(modifiers) {
            // Read the PERSISTED tags off this body's sidecar and key them by position.
            let tags = self.poly_op_tag_keys(nid, base);
            let auth = self.poly_authored_channels(nid, base);
            let centers = self.poly_mirror_centers_for(nid, modifiers);
            return Arc::new(Self::apply_poly_modifiers_tagged(
                base, modifiers, cutters, &tags, &auth, &centers,
            ));
        }
        // The caller usually just walked this same mesh to build the display cache key; taking
        // its result avoids a second full O(verts+edges+faces) walk in the same frame. Only ever
        // accepted when the caller proved it fresh — see the param doc.
        let base_sig = base_sig_hint.unwrap_or_else(|| base.content_signature());
        debug_assert_eq!(
            base_sig,
            base.content_signature(),
            "base_sig_hint must be the content_signature of THIS base mesh"
        );
        self.poly_prefix_clock = self.poly_prefix_clock.wrapping_add(1);
        let clock = self.poly_prefix_clock;

        // Longest matching prefix depth (number of stack slots already applied).
        // Per-slot after-counts ride along for the Modifiers panel stats memo (stamped at
        // both exits below) — the panel must NEVER need a second stack walk of its own.
        let mut start_depth = 0usize;
        // LAZY base clone. This used to be an unconditional `Arc::new(base.clone())`, which is a
        // full deep copy of every vertex, half-edge and face — 1.55 ms at 40k quads, 6.77 ms at
        // 160k. It is only ever USED when the ladder walk below matches nothing (start_depth 0);
        // any prefix hit overwrote it, and a FULL hit returns the cached Arc without reading it at
        // all. So on every warm walk and every full-hit walk it was a pure-waste copy — including
        // the Modifiers-panel stats path and the idle outline/overlay syncs that hit the ladder
        // every frame. `None` here means "not needed yet"; it is materialised at exactly one place.
        let mut mesh_arc: Option<Arc<HalfEdgeMesh>> = None;
        let mut slot_counts: Vec<(u32, u32)> = Vec::with_capacity(modifiers.len());

        if let Some(body) = self.poly_prefix_cache.get(&nid) {
            if body.base_sig == base_sig {
                for depth in 1..=modifiers.len() {
                    let ph = self.poly_stack_prefix_hash(modifiers, depth);
                    match body.entries.get(depth - 1) {
                        Some(Some(e)) if e.prefix_hash == ph => {
                            start_depth = depth;
                            mesh_arc = Some(e.mesh.clone());
                            slot_counts
                                .push((e.mesh.vertex_count() as u32, e.mesh.face_count() as u32));
                        }
                        _ => break,
                    }
                }
            }
        }

        // Full ladder hit — no further apply. An EMPTY stack also lands here with nothing latched
        // (`start_depth == modifiers.len() == 0`), which is the one case that still has to
        // materialise the base: the contract is that this returns the fully-evaluated mesh, and
        // for a stack with no modifiers that IS the base.
        if start_depth == modifiers.len() {
            if let Some(body) = self.poly_prefix_cache.get_mut(&nid) {
                body.last_used = clock;
            }
            self.stamp_poly_mod_stats(nid, base, base_sig, modifiers, &slot_counts);
            return mesh_arc.unwrap_or_else(|| Arc::new(base.clone()));
        }
        // Past here the walk really does need an owned starting mesh.
        let mut mesh_arc: Arc<HalfEdgeMesh> = mesh_arc.unwrap_or_else(|| Arc::new(base.clone()));

        // Truncate / reset body cache for this base (bytes adjusted after map borrow ends).
        let mut freed_bytes = 0usize;
        let mut added_bytes = 0usize;
        {
            let body = self
                .poly_prefix_cache
                .entry(nid)
                .or_insert_with(PolyPrefixBodyCache::default);
            if body.base_sig != base_sig {
                freed_bytes = freed_bytes.saturating_add(body.total_bytes);
                body.base_sig = base_sig;
                body.entries.clear();
                body.total_bytes = 0;
            }
            // Drop invalidated slots from start_depth onward.
            if body.entries.len() > start_depth {
                for slot in body.entries.drain(start_depth..) {
                    if let Some(e) = slot {
                        body.total_bytes = body.total_bytes.saturating_sub(e.approx_bytes);
                        freed_bytes = freed_bytes.saturating_add(e.approx_bytes);
                    }
                }
            }
            body.entries.resize_with(modifiers.len(), || None);
            body.last_used = clock;
        }

        // Mirror object origins → target-local (once per walk; null moves bust stack key above).
        let mirror_centers = self.poly_mirror_centers_for(nid, modifiers);

        // Apply remaining slots; store a prefix after each.
        for i in start_depth..modifiers.len() {
            let prefix_arc = Arc::clone(&mesh_arc);
            let prefix_hash_sculpt = self.poly_stack_prefix_hash(modifiers, i);
            let mut owned = Arc::try_unwrap(mesh_arc).unwrap_or_else(|a| (*a).clone());
            // V1 INCREMENTAL REMESH: the display walk (the only caller with app state)
            // routes enabled Remesh slots through the retained-grid path — a live edit
            // under an active Remesh resamples only the dirty region instead of the whole
            // lattice. Same Option contract as apply_one's Remesh arm (None keeps the
            // running mesh); disabled slots fall through to apply_one's no-op. Stateless
            // callers (tests / tagged walk / panel fallback) keep the stateless path —
            // identical output whenever the lattice fit matches (test-gated), and lattice
            // divergence is episode-scoped + reconciled at commit.
            if modifiers[i].enabled
                && modifiers[i].mod_type == core_math::scene_graph::PolyModifierType::Remesh
            {
                let episode_active = self.has_active_episode();
                // Draft res only while the episode is live. `commit_episode` takes the
                // episode first, then `poly_force_display_resync` (prefix cleared) remeshes
                // at the authored slider.
                let authored_res = modifiers[i].remesh_resolution;
                let drop_res = episode_active && modifiers[i].remesh_drop_res_drag;
                let eval_res = if drop_res {
                    authored_res.min(crate::poly::remesh::REMESH_LIVE_SCRUB_MAX)
                } else {
                    authored_res
                };
                let in_v = owned.vertex_count();
                let t_remesh = std::time::Instant::now();
                if let Some(out) = crate::poly::remesh::voxel_remesh_mesh_incremental_ex(
                    &owned,
                    eval_res,
                    modifiers[i].remesh_smooth,
                    modifiers[i].remesh_padding,
                    modifiers[i].remesh_project,
                    modifiers[i].remesh_union_islands,
                    modifiers[i].remesh_collapse,
                    nid.0,
                    i,
                    episode_active,
                    modifiers[i].remesh_skip_relax_drag,
                    modifiers[i].remesh_skip_seal_drag,
                    &mut self.poly_remesh_incr,
                ) {
                    let ms = t_remesh.elapsed().as_secs_f64() * 1e3;
                    let (incr, div, slabs_r, slabs_n, res) = self
                        .poly_remesh_incr
                        .as_ref()
                        .map(|c| {
                            (
                                c.last_eval_incremental,
                                c.diverged,
                                c.extract.recomputed_slabs,
                                c.extract.slab_vert_counts().len(),
                                c.res,
                            )
                        })
                        .unwrap_or((false, false, 0, 0, eval_res));
                    crate::poly::remesh::remesh_console_log(&format!(
                        "[REMESH] {ms:.1}ms incr={} ep={} res={res} nid={} slot={i} inV={in_v} outV={} outF={} slabs={slabs_r}/{slabs_n} div={} clps={} norelax={} noseal={} draft={} skip={}",
                        u8::from(incr),
                        u8::from(episode_active),
                        nid.0,
                        out.vertex_count(),
                        out.face_count(),
                        u8::from(div),
                        u8::from(modifiers[i].remesh_collapse),
                        u8::from(modifiers[i].remesh_skip_relax_drag && episode_active),
                        u8::from(modifiers[i].remesh_skip_seal_drag && episode_active),
                        u8::from(drop_res && eval_res < authored_res),
                        crate::poly::remesh::last_incr_skip(),
                    ));
                    owned = out;
                    // B: drag extract is from_mesh_draft (no twins). Mark the body so
                    // commit re-evals with episode_active=false and rebuilds a manifold.
                    if episode_active {
                        self.poly_remesh_diverged.insert(nid);
                    }
                }
                // Record divergence PER BODY, not just on the surviving cache slot — the
                // single slot is overwritten as soon as another body's stack is walked
                // mid-episode, and the commit reconcile must still resync this one.
                if self.poly_remesh_incr.as_ref().map_or(false, |c| c.diverged) {
                    self.poly_remesh_diverged.insert(nid);
                }
            } else if modifiers[i].enabled
                && modifiers[i].mod_type == core_math::scene_graph::PolyModifierType::Sculpt
            {
                owned = self.poly_sculpt_eval_working(
                    nid,
                    &prefix_arc,
                    prefix_hash_sculpt,
                    &modifiers[i],
                );
            } else {
                // Authored tables: tagged stacks (Smart Select / Mesh Cleanup / Bevel weights /
                // authored channels) never reach this ladder. Empty is correct for the rest.
                Self::apply_one_poly_modifier(
                    &mut owned, &modifiers[i], cutters,
                    &mut core_math::mesh_boolean::BoolProvenance::default(),
                    &Default::default(), &Default::default(), &mirror_centers,
                );
            }
            mesh_arc = Arc::new(owned);
            slot_counts.push((mesh_arc.vertex_count() as u32, mesh_arc.face_count() as u32));
            // C1: a PolySculpt slot is never a full prefix hit. Strokes mutate sidecar
            // deltas; hashing only modifier fields would serve a stale last-slot Arc.
            // Keep the UPSTREAM prefix (Remesh/Smooth) and re-apply Sculpt every walk.
            let skip_sculpt_prefix = modifiers[i].enabled
                && modifiers[i].mod_type == core_math::scene_graph::PolyModifierType::Sculpt;
            if skip_sculpt_prefix {
                continue;
            }
            let ph = self.poly_stack_prefix_hash(modifiers, i + 1);
            let approx = Self::poly_mesh_approx_bytes(&mesh_arc);
            let entry = PolyPrefixEntry {
                prefix_hash: ph,
                mesh: mesh_arc.clone(),
                approx_bytes: approx,
            };
            if let Some(body) = self.poly_prefix_cache.get_mut(&nid) {
                if i >= body.entries.len() {
                    body.entries.resize_with(i + 1, || None);
                }
                if let Some(old) = body.entries[i].take() {
                    body.total_bytes = body.total_bytes.saturating_sub(old.approx_bytes);
                    freed_bytes = freed_bytes.saturating_add(old.approx_bytes);
                }
                body.entries[i] = Some(entry);
                body.total_bytes = body.total_bytes.saturating_add(approx);
                added_bytes = added_bytes.saturating_add(approx);
                body.last_used = clock;
            }
        }
        self.poly_prefix_total_bytes = self
            .poly_prefix_total_bytes
            .saturating_sub(freed_bytes)
            .saturating_add(added_bytes);

        self.stamp_poly_mod_stats(nid, base, base_sig, modifiers, &slot_counts);
        self.poly_prefix_evict_if_needed();
        mesh_arc
    }

    /// Cache key for the display mesh of a poly body: `(content_signature, stack_key)`.
    /// `stack_key` folds boolean cutter content so editing a cutter invalidates the target.
    ///
    /// B1: when `poly_edit_generation` is unchanged, returns a per-nid memo of `(csig, stack_key)`
    /// so idle pick/LOD polls do **not** re-walk the half-edge graph every frame. Memo is
    /// cleared only via [`bump_poly_edit_generation`] at mutation choke points (not Arc-ptr —
    /// in-place `Arc::make_mut` keeps the pointer).
    pub(crate) fn poly_display_cache_key(
        &self,
        nid: NodeId,
        mesh: &HalfEdgeMesh,
        modifiers: &[PolyModifier],
    ) -> (u64, u64) {
        let (csig, sk, _fresh) = self.poly_display_cache_key_fresh(nid, mesh, modifiers);
        (csig, sk)
    }

    /// As [`Self::poly_display_cache_key`], but also reports whether `csig` was **computed from
    /// mesh content on this call** (`true`) or served from the generation-keyed memo (`false`).
    ///
    /// This distinction is the whole point. `apply_poly_modifiers_cached` validates its prefix
    /// ladder against a CONTENT signature; handing it a memo value would silently swap that for
    /// a GENERATION-based trust model — the bug class documented at the top of
    /// `poly_sync_mesh_cache`. So a caller may forward the signature to the ladder ONLY when this
    /// returns `true`, and only for the very mesh it was computed from. On a real edit the
    /// generation is bumped, the memo misses, and the signature is fresh — i.e. it is available
    /// exactly on the hot path where the duplicate walk hurt (1.24 ms at 40k quads, 5.97 ms at
    /// 160k, per call).
    fn poly_display_cache_key_fresh(
        &self,
        nid: NodeId,
        mesh: &HalfEdgeMesh,
        modifiers: &[PolyModifier],
    ) -> (u64, u64, bool) {
        let gen = self.poly_edit_generation;
        if let Ok(memo) = self.poly_display_key_memo.try_borrow() {
            if let Some(&(m_gen, csig, sk)) = memo.get(&nid) {
                if m_gen == gen {
                    return (csig, sk, false);
                }
            }
        }
        let mut stack_key = Self::poly_modifier_stack_hash(modifiers);
        for m in modifiers.iter() {
            if m.enabled
                && m.mod_type == core_math::scene_graph::PolyModifierType::Boolean
                && m.bool_cutter_nid != 0
            {
                use std::hash::{Hash, Hasher};
                let mut hh = std::collections::hash_map::DefaultHasher::new();
                stack_key.hash(&mut hh);
                m.bool_cutter_nid.hash(&mut hh);
                if let Some(cn) = self.scene.scene_graph.get(NodeId(m.bool_cutter_nid)) {
                    if let NodePayload::PolygonalBody { mesh: cm, .. } = &cn.payload {
                        cm.content_signature().hash(&mut hh);
                    }
                }
                stack_key = hh.finish();
            }
            // Mirror center object: transform must bust the display key when the null moves.
            if m.enabled
                && m.mod_type == core_math::scene_graph::PolyModifierType::Mirror
                && m.mirror_object_nid != 0
            {
                use std::hash::{Hash, Hasher};
                let mut hh = std::collections::hash_map::DefaultHasher::new();
                stack_key.hash(&mut hh);
                m.mirror_object_nid.hash(&mut hh);
                let world = self
                    .scene
                    .scene_graph
                    .world_transform(NodeId(m.mirror_object_nid));
                for col in world.to_cols_array() {
                    col.to_bits().hash(&mut hh);
                }
                stack_key = hh.finish();
            }
        }
        // SMART SELECT TAGS ARE PART OF THE KEY (owner 2026-08-05: "it still does not update
        // live when I am modeling"). The key was (mesh signature, modifier hash) and the
        // op_created sidecar is in NEITHER, so beveling an edge - which writes tags but does
        // not change the base mesh or any modifier parameter - left the key identical and the
        // STALE display mesh was reused. The modifier only appeared to work after something
        // else forced a rebuild, which is exactly the "needs a reset to see changes" report.
        // Only folded in when a Smart Select bevel is present, so no other stack pays for it.
        if modifiers.iter().any(|m| {
            m.enabled
                && m.mod_type == core_math::scene_graph::PolyModifierType::Bevel
                && m.bevel_smart_select
        }) {
            use std::hash::{Hash, Hasher};
            let mut hh = std::collections::hash_map::DefaultHasher::new();
            stack_key.hash(&mut hh);
            if let Some(cd) = self.cache.poly_custom_data.get(&nid) {
                // Count + the tagged ids: cheap, and any tag change moves the digest.
                let mut ids: Vec<u32> = (0..mesh.half_edges.len() as u32)
                    .filter(|&i| {
                        let he = core_math::half_edge::HalfEdgeId(i);
                        mesh.is_edge_live(he) && cd.op_created.get(he)
                    })
                    .collect();
                ids.sort_unstable();
                ids.hash(&mut hh);
            }
            stack_key = hh.finish();
        }
        // AUTHORED CHANNEL DATA IS PART OF THE KEY — the same trap as the Smart Select tags
        // above, one layer out: assigning a polygroup or setting an edge weight changes neither
        // the mesh signature nor any modifier parameter, so a masked Smooth would keep showing
        // the mask it was built with until something unrelated forced a rebuild. Folded in only
        // when a modifier actually reads authored data, so no other stack pays for it.
        // A Bevel driving its width from AUTHORED EDGE WEIGHTS reads the same sidecar without
        // needing a geometry channel at all (the channel can stay `All`), so the channel predicate
        // alone would miss it and editing a weight would not refresh the display. That case is
        // `PolyModifier::reads_edge_weight`, shared with the code that decides whether authoring a
        // weight re-evaluates the stack — the two used to spell it out separately and disagreed.
        if modifiers
            .iter()
            .any(|m| (m.enabled && m.geo_channel.kind.needs_authored()) || m.reads_edge_weight())
        {
            use std::hash::{Hash, Hasher};
            let mut hh = std::collections::hash_map::DefaultHasher::new();
            stack_key.hash(&mut hh);
            if let Some(cd) = self.cache.poly_custom_data.get(&nid) {
                let mut groups: Vec<(u32, u16)> =
                    cd.groups.iter().map(|(k, v)| (k.0, *v)).collect();
                groups.sort_unstable();
                groups.hash(&mut hh);
                // f32 has no Hash — key on the bit pattern, which is what every other float in
                // this file's digests does.
                let mut weights: Vec<(u32, u32, u32)> =
                    cd.crease.iter().map(|(k, v)| (k.0, k.1, v.to_bits())).collect();
                weights.sort_unstable();
                weights.hash(&mut hh);
            }
            stack_key = hh.finish();
        }
        // AUTHORED NORMAL STATE IS PART OF THE KEY — same reasoning as the Smart Select tags
        // above: Mark Sharp / Shade Flat / an Auto Smooth angle change none of the mesh signature
        // nor any modifier parameter, so without this the STALE display mesh would be reused and
        // the shading would only update after some unrelated edit forced a rebuild.
        if let Some(nk) = self.poly_normal_key(nid, mesh) {
            use std::hash::{Hash, Hasher};
            let mut hh = std::collections::hash_map::DefaultHasher::new();
            stack_key.hash(&mut hh);
            nk.hash(&mut hh);
            stack_key = hh.finish();
        }
        // AUTHORED UV STATE — unwrap only touches CustomData.uv; without this the GPU mesh
        // keeps zeroed UVs until an unrelated geometry edit forces a rebuild.
        if let Some(uk) = self.poly_uv_key(nid) {
            use std::hash::{Hash, Hasher};
            let mut hh = std::collections::hash_map::DefaultHasher::new();
            stack_key.hash(&mut hh);
            uk.hash(&mut hh);
            stack_key = hh.finish();
        }
        let csig = mesh.content_signature();
        if let Ok(mut memo) = self.poly_display_key_memo.try_borrow_mut() {
            memo.insert(nid, (gen, csig, stack_key));
        }
        (csig, stack_key, true)
    }

    /// Return the **display** mesh (base + enabled modifiers), sharing one evaluation across
    /// solid sync / selection outline / overlay wires (POLY_VIEWPORT_PERF P1a).
    ///
    /// Cache hit → `poly_modifier_evals_this_frame` unchanged.
    /// Miss → one `apply_poly_modifiers` + store; increments the eval counter.
    ///
    /// Does **not** upload the GPU solid — `poly_sync_mesh_cache` remains the GPU owner.
    /// Raster data for EXPORT — deliberately the SAME geometry and the SAME normals the
    /// viewport shows, produced by the same two calls in the same order as the display bake.
    ///
    /// Owner 2026-08-05: "export doesn't honour authored normals — display and export disagree
    /// for a body with Mark Sharp." It was worse than that. Both exporters called
    /// `to_raster_data()` on the BASE mesh, so they lost:
    ///   * every authored normal (Mark Sharp, Auto Smooth angle) — the reported bug, and
    ///   * THE ENTIRE MODIFIER STACK. A bevelled, subdivided, mirrored or scattered body
    ///     exported as its unmodified base.
    /// Display and export were simply two different code paths that happened to agree on a body
    /// with neither modifiers nor authored normals. This is the one choke point, so they now
    /// agree by construction rather than by coincidence.
    pub(crate) fn poly_export_raster(
        &mut self,
        nid: NodeId,
    ) -> Option<(Vec<[f32; 8]>, Vec<u32>)> {
        let display = self.poly_cached_display_mesh(nid)?;
        let nrm = self.poly_normal_state(nid, &display);
        let (verts, indices, _mat_ranges) =
            core_math::poly_mesh::raster_data_ngon_with_normals(&display, &nrm.cfg());
        Some((verts, indices))
    }

    pub(crate) fn poly_cached_display_mesh(
        &mut self,
        nid: NodeId,
    ) -> Option<Arc<HalfEdgeMesh>> {
        let (base, mods, key, sig_fresh) = {
            let node = self.scene.scene_graph.get(nid)?;
            match &node.payload {
                NodePayload::PolygonalBody {
                    mesh, modifiers, ..
                } => {
                    let (csig, sk, fresh) = self.poly_display_cache_key_fresh(nid, mesh, modifiers);
                    (mesh.clone(), modifiers.clone(), (csig, sk), fresh)
                }
                _ => return None,
            }
        };
        if let Some(entry) = self.poly_display_cache.get(&nid) {
            if entry.key == key {
                return Some(entry.mesh.clone());
            }
        }
        let cutters = self.poly_ref_meshes_for(nid, &mods);
        // `key.0` is this exact `base`'s content signature, and nothing between here and the
        // read above mutates the graph — so forward it when it was freshly computed.
        let display = self.apply_poly_modifiers_cached(
            nid,
            &base,
            &mods,
            &cutters,
            if sig_fresh { Some(key.0) } else { None },
        );
        self.poly_modifier_evals_this_frame =
            self.poly_modifier_evals_this_frame.saturating_add(1);
        self.poly_display_cache.insert(
            nid,
            PolyDisplayMeshCache {
                key,
                mesh: display.clone(),
                arc_ptr: None,
                face_count: None,
            },
        );
        Some(display)
    }

    /// True when an enabled modifier changes **geometry** (not just shading normals).
    /// Position-only edits (grab) must fully re-run these; otherwise the display cage can
    /// re-raster without Catmull-Clark / Array / Boolean (POLY_VIEWPORT_PERF P3).
    fn poly_mods_need_geometry_reapply(mods: &[PolyModifier]) -> bool {
        use core_math::scene_graph::PolyModifierType;
        mods.iter().any(|m| {
            if !m.enabled {
                return false;
            }
            !matches!(
                m.mod_type,
                PolyModifierType::WeightedNormal | PolyModifierType::SmoothNormal
            )
        })
    }

    /// Drop display/prefix Arcs for `nid` so a subsequent `Arc::make_mut` on the base mesh
    /// does not force a full half-edge clone (P3: grab/slide every frame).
    pub(crate) fn poly_release_shared_display_arcs(&mut self, nid: NodeId) {
        self.bump_poly_edit_generation();
        self.poly_display_cache.remove(&nid);
        self.poly_prefix_cache_remove(nid);
    }

    /// **Single chokepoint** for in-place base-mesh mutation (drag appliers + cancel).
    ///
    /// Always releases shared display/prefix Arcs **before** `Arc::make_mut`. Without the
    /// release, a body with a geometry modifier keeps a unique base Arc whose
    /// (arc_ptr, face_count, stack) still matches the display-cache key → `poly_sync`
    /// early-outs and the viewport keeps the dragged shape after Escape (or shows stale
    /// display while the base is restored).
    ///
    /// Returns `None` when `nid` is missing or not a `PolygonalBody`.
    /// Mesh > Cleanup: open the settings dialog for the active poly body (owner
    /// 2026-08-03: standalone command WITH settings). Apply runs `poly_mesh_cleanup_now`.
    /// Also arms left-panel Tool Parameters ("Mesh Cleanup").
    pub(crate) fn poly_mesh_cleanup(&mut self) {
        let Some(nid) = self.poly_cleanup_target() else {
            self.toast("Cleanup: select a polygonal body first");
            return;
        };
        let Some(mesh) = self.poly_node_mesh(nid) else { return };
        let name = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                core_math::scene_graph::NodePayload::PolygonalBody { name, .. } => {
                    Some(name.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        self.arm_mesh_cleanup_tool_params();
        self.mesh_cleanup_begin_session(nid);
        if self.ui.mesh_cleanup_live {
            self.mesh_cleanup_live_regenerate();
        }
        let faces = self
            .poly_node_mesh(nid)
            .map(|m| m.face_count())
            .unwrap_or(mesh.face_count());
        let folds = self
            .poly_node_mesh(nid)
            .map(|m| m.count_folded_coplanar_pairs())
            .unwrap_or_else(|| mesh.count_folded_coplanar_pairs());
        self.ui.mesh_cleanup_dialog = Some((nid, name, faces, folds));
    }

    /// Snapshot the body's base mesh for live cleanup preview. Idempotent for the same nid
    /// while a session is already open (does not re-snapshot over a live preview mesh).
    pub(crate) fn mesh_cleanup_begin_session(&mut self, nid: core_math::scene_graph::NodeId) {
        if self
            .mesh_cleanup_base
            .as_ref()
            .map(|(id, _)| *id == nid)
            .unwrap_or(false)
        {
            return;
        }
        // Leaving another body mid-live: restore that one first.
        if self.mesh_cleanup_base.is_some() {
            self.mesh_cleanup_end_session(true);
        }
        let Some(mesh) = self.poly_node_mesh(nid) else {
            return;
        };
        self.mesh_cleanup_base = Some((nid, mesh));
    }

    /// End live session. `restore` puts the base mesh back on the body (Cancel / Live off /
    /// leave tool). `false` keeps whatever is currently on the body (after Apply committed).
    pub(crate) fn mesh_cleanup_end_session(&mut self, restore: bool) {
        let Some((nid, base)) = self.mesh_cleanup_base.take() else {
            return;
        };
        if restore {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } =
                    &mut node.payload
                {
                    *mesh = base;
                }
            }
            self.poly_request_display_sync(nid);
            self.poly_flush_mesh_dirty();
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    /// Re-run cleanup from the base snapshot and install the preview (no undo). No-op when
    /// Live is off or there is no session.
    pub(crate) fn mesh_cleanup_live_regenerate(&mut self) {
        if !self.ui.mesh_cleanup_live {
            return;
        }
        let Some((nid, base)) = self.mesh_cleanup_base.clone() else {
            return;
        };
        let protected = self.poly_protected_edge_positions(nid, &base);
        let opts = self.mesh_cleanup_options(protected);
        let preview = match core_math::mesh_boolean::cleanup_mesh_report(&base, &opts) {
            Some((cleaned, stats)) => {
                let folds_after = cleaned.count_folded_coplanar_pairs();
                let rep = crate::MeshCleanupReport {
                    body_name: String::new(),
                    verts_before: base.vertex_count(),
                    verts_after: cleaned.vertex_count(),
                    edges_before: base.edge_count(),
                    edges_after: cleaned.edge_count(),
                    faces_before: base.face_count(),
                    faces_after: cleaned.face_count(),
                    folds_before: base.count_folded_coplanar_pairs(),
                    folds_after,
                    dissolve_deg: opts.dissolve_angle_deg,
                    protected_kept: opts.protected_edges.len(),
                    applied: true,
                    stats,
                };
                self.ui.mesh_cleanup_result = Some(rep);
                cleaned
            }
            None => {
                // Already clean under these knobs — show the base mesh.
                self.ui.mesh_cleanup_result = Some(crate::MeshCleanupReport {
                    body_name: String::new(),
                    verts_before: base.vertex_count(),
                    verts_after: base.vertex_count(),
                    edges_before: base.edge_count(),
                    edges_after: base.edge_count(),
                    faces_before: base.face_count(),
                    faces_after: base.face_count(),
                    folds_before: base.count_folded_coplanar_pairs(),
                    folds_after: base.count_folded_coplanar_pairs(),
                    dissolve_deg: opts.dissolve_angle_deg,
                    protected_kept: opts.protected_edges.len(),
                    applied: false,
                    stats: Default::default(),
                });
                (*base).clone()
            }
        };
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } =
                &mut node.payload
            {
                *mesh = std::sync::Arc::new(preview);
            }
        }
        self.poly_request_display_sync(nid);
        self.poly_flush_mesh_dirty();
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// If the user left Mesh Cleanup without Apply/Cancel, restore the base mesh.
    pub(crate) fn mesh_cleanup_poll_session(&mut self) {
        if self.mesh_cleanup_base.is_none() {
            return;
        }
        let armed = self.ui2.active_tool == "Mesh Cleanup" || self.ui.mesh_cleanup_dialog.is_some();
        if !armed {
            self.mesh_cleanup_end_session(true);
        }
    }

    /// Ctrl+X in poly **edit** mode with nothing selected: run Mesh Cleanup immediately
    /// with limited dissolve at **4.5°**, arm Tool Parameters, select-all, show result popup.
    pub(crate) fn poly_ctrl_x_mesh_cleanup(&mut self) {
        let Some(nid) = self.poly_cleanup_target() else {
            self.toast("Cleanup: no polygonal body active");
            return;
        };
        // Ensure the body is the active node so select-all targets it.
        self.sel.usel.active = Some(crate::SceneItemId::Node(nid));
        self.sel.usel.selected.insert(crate::SceneItemId::Node(nid));

        // Ctrl+X is a fixed "repair it" preset, not the dialog's last state — every knob is set
        // here so the shortcut does the same thing whatever the user left in the panel. The three
        // REBUILD passes (fill holes / make planar / splits) are deliberately zeroed: they add or
        // move geometry, which is a modelling decision, not a repair.
        self.ui.mesh_cleanup_flatten = true;
        self.ui.mesh_cleanup_merge = true;
        self.ui.mesh_cleanup_dissolve_deg = 4.5;
        self.ui.mesh_cleanup_merge_dist = 1e-4;
        self.ui.mesh_cleanup_degenerate = 1e-4;
        self.ui.mesh_cleanup_loose_verts = true;
        self.ui.mesh_cleanup_loose_edges = true;
        self.ui.mesh_cleanup_loose_faces = false;
        self.ui.mesh_cleanup_fill_holes = 0;
        self.ui.mesh_cleanup_planar_iters = 0;
        self.ui.mesh_cleanup_split_nonplanar_deg = 0.0;
        self.ui.mesh_cleanup_split_concave = false;
        // Not re-winding either: on a model with an intentionally inverted interior shell,
        // "orient outward" turns the cavity inside out. Shift+N is the explicit command for it.
        self.ui.mesh_cleanup_recalc_normals = false;
        // Drop any live session WITHOUT restoring the snapshot. Restoring used to wipe
        // modelling done after the session started (owner: Ctrl+X after Symmetrize undid the
        // mirror — live base was still the pre-symmetrize mesh). Keep the mesh that is on the
        // body now; cleanup runs on that.
        self.mesh_cleanup_end_session(false);
        // Apply first from the current mesh (no session base → now_ex takes the live body).
        self.poly_mesh_cleanup_now_ex(nid, true);
        // Then arm Tool Parameters + a fresh live base from the cleaned result.
        self.arm_mesh_cleanup_tool_params();
    }

    /// ONE place the `ui.mesh_cleanup_*` widgets become [`CleanupOptions`] — the standalone
    /// command, Ctrl+X and the Tool Parameters "Apply" all route through here, so a knob can
    /// never mean one thing in the dialog and another on the shortcut.
    pub(crate) fn mesh_cleanup_options(
        &self,
        protected: Vec<(glam::Vec3, glam::Vec3)>,
    ) -> core_math::mesh_boolean::CleanupOptions {
        core_math::mesh_boolean::CleanupOptions {
            flatten_folds: self.ui.mesh_cleanup_flatten,
            merge_coplanar: self.ui.mesh_cleanup_merge,
            dissolve_angle_deg: self.ui.mesh_cleanup_dissolve_deg,
            protected_edges: protected,
            merge_distance: self.ui.mesh_cleanup_merge_dist.max(0.0),
            degenerate_threshold: self.ui.mesh_cleanup_degenerate.max(0.0),
            delete_loose_verts: self.ui.mesh_cleanup_loose_verts,
            delete_loose_edges: self.ui.mesh_cleanup_loose_edges,
            delete_loose_faces: self.ui.mesh_cleanup_loose_faces,
            fill_holes_sides: self.ui.mesh_cleanup_fill_holes,
            make_planar_iterations: self.ui.mesh_cleanup_planar_iters,
            split_non_planar_deg: self.ui.mesh_cleanup_split_nonplanar_deg.max(0.0),
            split_concave: self.ui.mesh_cleanup_split_concave,
            recalc_normals: self.ui.mesh_cleanup_recalc_normals,
        }
    }

    /// The Clean Up checkbox/slider block — ONE widget layout drawn by both the modal dialog and
    /// the left Tool Parameters panel (they used to carry near-duplicate copies of the three
    /// original knobs; ten knobs would have made that drift certain).
    ///
    /// Returns `true` when any knob changed (callers may re-run live preview).
    pub(crate) fn draw_mesh_cleanup_controls(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        // Live toggle first — default ON so activating the tool already shows results as you edit.
        let live_before = self.ui.mesh_cleanup_live;
        let live_resp = ui
            .checkbox(&mut self.ui.mesh_cleanup_live, "Live")
            .on_hover_text(
                "On (default): changing any setting re-runs cleanup from the base mesh and \
                 updates the viewport immediately. Apply commits (undoable).\n\
                 Off: viewport shows the base mesh; Apply runs once.",
            );
        if live_resp.changed() {
            changed = true;
            if self.ui.mesh_cleanup_live && !live_before {
                // Turning Live ON → preview now.
                if let Some(nid) = self.poly_cleanup_target() {
                    self.mesh_cleanup_begin_session(nid);
                }
                self.mesh_cleanup_live_regenerate();
            } else if !self.ui.mesh_cleanup_live && live_before {
                // Turning Live OFF → restore base so the user sees the uncleaned mesh.
                if let Some((nid, base)) = self.mesh_cleanup_base.clone() {
                    if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                        if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } =
                            &mut node.payload
                        {
                            *mesh = base;
                        }
                    }
                    self.poly_request_display_sync(nid);
                    self.poly_flush_mesh_dirty();
                    self.mark_dirty(crate::DirtyFlags::RENDER);
                }
            }
        }

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Repair").strong());
        changed |= ui
            .checkbox(
                &mut self.ui.mesh_cleanup_flatten,
                "Flatten folded flaps (zero-thickness)",
            )
            .changed();
        changed |= ui
            .checkbox(&mut self.ui.mesh_cleanup_merge, "Merge coplanar faces into n-gons")
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.ui.mesh_cleanup_merge_dist, 0.0..=0.01)
                    .logarithmic(true)
                    .text("merge by distance"),
            )
            .on_hover_text("Blender Merge by Distance — weld doubles. 0 = off.")
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.ui.mesh_cleanup_degenerate, 0.0..=0.01)
                    .logarithmic(true)
                    .text("degenerate dissolve"),
            )
            .on_hover_text(
                "Blender Degenerate Dissolve — collapse edges shorter than this and drop the \
                 zero-area faces they leave. 0 = off.",
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.ui.mesh_cleanup_dissolve_deg, 0.0..=15.0)
                    .text("limited dissolve °"),
            )
            .on_hover_text(
                "Blender Limited Dissolve — dissolve edges whose faces meet within this angle. 0 = off.",
            )
            .changed();

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Delete loose").strong());
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut self.ui.mesh_cleanup_loose_verts, "verts")
                .changed();
            changed |= ui
                .checkbox(&mut self.ui.mesh_cleanup_loose_edges, "edges")
                .changed();
            changed |= ui
                .checkbox(&mut self.ui.mesh_cleanup_loose_faces, "faces")
                .changed();
        });

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Rebuild (opt-in — these ADD or MOVE geometry)").strong());
        let mut sides = self.ui.mesh_cleanup_fill_holes as u32;
        if ui
            .add(egui::Slider::new(&mut sides, 0..=12).text("fill holes ≤ sides"))
            .on_hover_text("Blender Fill Holes — cap boundary loops up to this many sides. 0 = off.")
            .changed()
        {
            self.ui.mesh_cleanup_fill_holes = sides as usize;
            changed = true;
        }
        changed |= ui
            .add(
                egui::Slider::new(&mut self.ui.mesh_cleanup_planar_iters, 0..=20)
                    .text("make planar iters"),
            )
            .on_hover_text(
                "Blender Make Planar Faces — flatten n-gons onto their best-fit plane. 0 = off.",
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.ui.mesh_cleanup_split_nonplanar_deg, 0.0..=45.0)
                    .text("split non-planar °"),
            )
            .on_hover_text(
                "Blender Split Non-Planar Faces — split n-gons twisting past this angle. 0 = off.",
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.ui.mesh_cleanup_split_concave,
                "Split concave faces into convex",
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.ui.mesh_cleanup_recalc_normals,
                "Recalculate normals (consistent winding, outward)",
            )
            .changed();
        changed
    }

    /// Point left Tool Parameters at Mesh Cleanup (poly pillar).
    fn arm_mesh_cleanup_tool_params(&mut self) {
        self.ui2.active_pillar = "poly".into();
        self.ui2.active_tool = "Mesh Cleanup".into();
        self.ui.show_tool_panel = true;
        if let Some(nid) = self.poly_cleanup_target() {
            self.mesh_cleanup_begin_session(nid);
        }
    }

    pub(crate) fn poly_cleanup_target(&self) -> Option<core_math::scene_graph::NodeId> {
        self.poly_active_body().or_else(|| {
            self.sel.usel.active_node().filter(|&n| {
                matches!(
                    self.scene.scene_graph.get(n).map(|nd| &nd.payload),
                    Some(core_math::scene_graph::NodePayload::PolygonalBody { .. })
                )
            })
        })
    }

    /// Settings dialog (drawn every frame from app_update; no-op while None).
    pub(crate) fn draw_mesh_cleanup_dialog(&mut self, ctx: &egui::Context) {
        let Some((nid, name, faces, folds)) = self.ui.mesh_cleanup_dialog.clone() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut knobs_changed = false;
        egui::Window::new("Mesh Cleanup")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.label(format!("{name} — {faces} faces, {folds} folded flap(s)"));
                ui.separator();
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    knobs_changed = self.draw_mesh_cleanup_controls(ui);
                });
                ui.separator();
                ui.weak("Never installs a broken result: if the repair fails its gates");
                ui.weak("(invalid, or MORE open / MORE folded than before), nothing changes.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(if self.ui.mesh_cleanup_live {
                            "Apply (commit live)"
                        } else {
                            "Apply"
                        })
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if knobs_changed && self.ui.mesh_cleanup_live {
            self.mesh_cleanup_begin_session(nid);
            self.mesh_cleanup_live_regenerate();
            // Refresh face/fold counts in the dialog title.
            if let Some(m) = self.poly_node_mesh(nid) {
                if let Some(d) = self.ui.mesh_cleanup_dialog.as_mut() {
                    d.2 = m.face_count();
                    d.3 = m.count_folded_coplanar_pairs();
                }
            }
        }
        if !open || cancel {
            self.ui.mesh_cleanup_dialog = None;
            // Cancel / close without Apply → restore base mesh.
            self.mesh_cleanup_end_session(true);
            if self.ui2.active_tool == "Mesh Cleanup" {
                self.ui2.active_tool.clear();
            }
        }
        if apply {
            self.ui.mesh_cleanup_dialog = None;
            // Commit from the base snapshot (live may already show the preview).
            self.poly_mesh_cleanup_commit_session(nid, false);
        }
    }

    /// Result popup after Ctrl+X cleanup / Apply — verts merged, edges dissolved, faces, folds.
    pub(crate) fn draw_mesh_cleanup_result_dialog(&mut self, ctx: &egui::Context) {
        let Some(rep) = self.ui.mesh_cleanup_result.clone() else {
            return;
        };
        let mut open = true;
        let mut dismiss = false;
        egui::Window::new(if rep.applied {
            "Mesh Cleanup — done"
        } else {
            "Mesh Cleanup — nothing changed"
        })
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0))
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(if rep.body_name.is_empty() {
                    "Active mesh".into()
                } else {
                    rep.body_name.clone()
                })
                .strong(),
            );
            ui.separator();
            if rep.applied {
                ui.label("What changed:");
                let vm = rep.verts_merged();
                let ed = rep.edges_dissolved();
                let fr = rep.faces_removed();
                let ff = rep.folds_fixed();
                ui.monospace(format!(
                    "  Vertices merged / removed:  {vm}  ({} → {})",
                    rep.verts_before, rep.verts_after
                ));
                ui.monospace(format!(
                    "  Edges dissolved / removed:  {ed}  ({} → {})",
                    rep.edges_before, rep.edges_after
                ));
                ui.monospace(format!(
                    "  Faces removed / merged:     {fr}  ({} → {})",
                    rep.faces_before, rep.faces_after
                ));
                ui.monospace(format!(
                    "  Folded flaps fixed:         {ff}  ({} → {})",
                    rep.folds_before, rep.folds_after
                ));
                if rep.dissolve_deg > 0.01 {
                    ui.label(format!(
                        "Limited dissolve: {:.2}°",
                        rep.dissolve_deg
                    ));
                }
                // Which pass fired. Net counts cannot say — a run that welds a double AND fills a
                // hole comes out even, and a re-wind moves no counter at all.
                let s = &rep.stats;
                let mut lines: Vec<String> = Vec::new();
                if s.verts_welded > 0 {
                    lines.push(format!("merged by distance: {} vert(s)", s.verts_welded));
                }
                if s.degenerate_removed > 0 {
                    lines.push(format!("degenerate dissolved: {}", s.degenerate_removed));
                }
                if !s.loose.is_empty() {
                    lines.push(format!(
                        "loose removed: {}v {}e {}f",
                        s.loose.verts, s.loose.edges, s.loose.faces
                    ));
                }
                if s.holes_filled > 0 {
                    lines.push(format!("holes filled: {}", s.holes_filled));
                }
                if s.planar_faces_fixed > 0 {
                    lines.push(format!("faces made planar: {}", s.planar_faces_fixed));
                }
                if s.non_planar_split > 0 {
                    lines.push(format!("non-planar split: +{} face(s)", s.non_planar_split));
                }
                if s.concave_split > 0 {
                    lines.push(format!("concave split: +{} face(s)", s.concave_split));
                }
                if s.normals_flipped > 0 {
                    lines.push(format!("normals re-wound: {} face(s)", s.normals_flipped));
                }
                if s.t_junctions_fixed {
                    lines.push("T-junctions repaired".into());
                }
                if !lines.is_empty() {
                    ui.add_space(4.0);
                    ui.label("Passes that fired:");
                    for l in lines {
                        ui.monospace(format!("  {l}"));
                    }
                }
                if rep.protected_kept > 0 {
                    ui.weak(format!(
                        "{} tagged / sharp / seam edge(s) protected",
                        rep.protected_kept
                    ));
                }
            } else {
                ui.label("Mesh is already clean — nothing to repair.");
                ui.weak(format!(
                    "{} verts · {} edges · {} faces · {} folds",
                    rep.verts_after, rep.edges_after, rep.faces_after, rep.folds_after
                ));
            }
            ui.add_space(8.0);
            if ui.button("OK").clicked() {
                dismiss = true;
            }
        });
        if !open || dismiss {
            self.ui.mesh_cleanup_result = None;
        }
    }

    /// Apply the cleanup with the dialog's options. One transaction (undoable).
    pub(crate) fn poly_mesh_cleanup_now(&mut self, nid: core_math::scene_graph::NodeId) {
        self.poly_mesh_cleanup_now_ex(nid, false);
    }

    /// Apply cleanup. When `select_all_after`, force select-all in the current edit mode
    /// and open the result popup (Ctrl+X empty-edit path).
    ///
    /// Authored edges are PROTECTED from the merge/dissolve passes and the sidecar is CARRIED
    /// through the rebuild by position — it used to be dropped wholesale here, which erased
    /// every Smart Select tag, sharp mark, UV, seam, crease and polygroup on the body and is
    /// exactly the "mesh clean up breaks the tag systeme" the owner reported (2026-08-05).
    /// Fold repair is unaffected by protection, so cleanup still fixes what it exists to fix.
    pub(crate) fn poly_mesh_cleanup_now_ex(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        select_all_after: bool,
    ) {
        // Prefer committing from the live session base so we never clean an already-previewed mesh.
        if self
            .mesh_cleanup_base
            .as_ref()
            .map(|(id, _)| *id == nid)
            .unwrap_or(false)
        {
            self.poly_mesh_cleanup_commit_session(nid, select_all_after);
            return;
        }
        let Some(mesh) = self.poly_node_mesh(nid) else { return };
        let protected = self.poly_protected_edge_positions(nid, &mesh);
        let opts = self.mesh_cleanup_options(protected);
        self.poly_mesh_cleanup_apply(nid, opts, select_all_after);
    }

    /// Commit live (or non-live) cleanup from the base snapshot → undoable install, then
    /// re-snapshot so further Live tweaks start from the committed mesh.
    pub(crate) fn poly_mesh_cleanup_commit_session(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        select_all_after: bool,
    ) {
        // Restore base first so apply runs on the original, not a previous live preview.
        if let Some((sid, base)) = self.mesh_cleanup_base.take() {
            if let Some(node) = self.scene.scene_graph.get_mut(sid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } =
                    &mut node.payload
                {
                    *mesh = base;
                }
            }
            if sid != nid {
                self.poly_request_display_sync(sid);
            }
        }
        let Some(mesh) = self.poly_node_mesh(nid) else {
            return;
        };
        let protected = self.poly_protected_edge_positions(nid, &mesh);
        let opts = self.mesh_cleanup_options(protected);
        self.poly_mesh_cleanup_apply(nid, opts, select_all_after);
        // After a successful commit, re-arm the base so Live can continue from the new mesh.
        if self.ui2.active_tool == "Mesh Cleanup" || self.ui.mesh_cleanup_dialog.is_some() {
            self.mesh_cleanup_begin_session(nid);
        }
    }

    /// THE install path for every Mesh Cleanup run — dialog Apply, Ctrl+X, Tool Parameters, and
    /// the single-pass `Mesh > Clean Up` commands. Taking the options as an argument (instead of
    /// re-reading `self.ui`) is what lets a single-pass command reuse this whole path — gates,
    /// transaction, sidecar carry, report — rather than growing a second one (CLAUDE.md: two code
    /// paths for the same operation MUST share one path).
    ///
    /// The caller is responsible for filling `opts.protected_edges`
    /// ([`Self::poly_protected_edge_positions`]).
    pub(crate) fn poly_mesh_cleanup_apply(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        opts: core_math::mesh_boolean::CleanupOptions,
        select_all_after: bool,
    ) {
        let Some(mesh) = self.poly_node_mesh(nid) else {
            return;
        };
        let body_name = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                core_math::scene_graph::NodePayload::PolygonalBody { name, .. } => {
                    Some(name.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        let n_protected = opts.protected_edges.len();
        let dissolve_deg = opts.dissolve_angle_deg;
        let verts_before = mesh.vertex_count();
        let edges_before = mesh.edge_count();
        let faces_before = mesh.face_count();
        let folds_before = mesh.count_folded_coplanar_pairs();

        let Some((cleaned, stats)) = core_math::mesh_boolean::cleanup_mesh_report(&mesh, &opts)
        else {
            let rep = crate::MeshCleanupReport {
                body_name,
                verts_before,
                verts_after: verts_before,
                edges_before,
                edges_after: edges_before,
                faces_before,
                faces_after: faces_before,
                folds_before,
                folds_after: folds_before,
                dissolve_deg,
                protected_kept: n_protected,
                applied: false,
                stats: Default::default(),
            };
            self.ui.mesh_cleanup_result = Some(rep);
            self.toast("Cleanup: mesh is already clean (nothing to repair)");
            if select_all_after {
                self.poly_select_all();
            }
            return;
        };
        let verts_after = cleaned.vertex_count();
        let edges_after = cleaned.edge_count();
        let faces_after = cleaned.face_count();
        let folds_after = cleaned.count_folded_coplanar_pairs();
        // Positions of uncut geometry survive the rebuild even though every id dies — the same
        // carry every other rebuild op uses (boolean, bevel, inset).
        let carried = self
            .cache
            .poly_custom_data
            .get(&nid)
            .map(|cd| cd.remap_by_position(&mesh, &cleaned));
        self.transaction("Mesh Cleanup", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } =
                    &mut node.payload
                {
                    *mesh = std::sync::Arc::new(cleaned);
                }
            }
            match carried {
                Some(cd) => {
                    s.cache.poly_custom_data.insert(nid, cd);
                }
                None => {
                    s.cache.poly_custom_data.remove(&nid);
                }
            }
            s.poly_request_display_sync(nid);
        });
        self.poly_flush_mesh_dirty();
        self.mark_dirty(crate::DirtyFlags::RENDER);

        let rep = crate::MeshCleanupReport {
            body_name,
            verts_before,
            verts_after,
            edges_before,
            edges_after,
            faces_before,
            faces_after,
            folds_before,
            folds_after,
            dissolve_deg,
            protected_kept: n_protected,
            applied: true,
            stats,
        };
        let toast = format!(
            "Cleanup: verts {}→{} (Δ{}), edges {}→{} (Δ{}), faces {}→{} (Δ{}), folds {}→{}",
            verts_before,
            verts_after,
            rep.verts_merged(),
            edges_before,
            edges_after,
            rep.edges_dissolved(),
            faces_before,
            faces_after,
            rep.faces_removed(),
            folds_before,
            folds_after,
        );
        self.toast(&toast);
        self.ui.mesh_cleanup_result = Some(rep);

        if select_all_after {
            // After rebuild, ids are new — select everything in the current edit mode.
            self.poly_select_all();
        }
    }

    pub(crate) fn poly_with_base_mesh_mut<R>(
        &mut self,
        nid: NodeId,
        f: impl FnOnce(&mut HalfEdgeMesh) -> R,
    ) -> Option<R> {
        self.poly_release_shared_display_arcs(nid);
        let node = self.scene.scene_graph.get_mut(nid)?;
        match &mut node.payload {
            NodePayload::PolygonalBody { mesh, .. } => {
                let m = Arc::make_mut(mesh);
                Some(f(m))
            }
            _ => None,
        }
    }

    /// POLY_VIEWPORT_PERF P3: after a **topology-stable** position edit (grab, scale, rotate
    /// verts, shrink/fatten). Skips full modifier re-eval when the stack has no geometry
    /// modifiers; otherwise falls back to [`poly_sync_mesh_cache`].
    pub(crate) fn poly_sync_after_position_edit(&mut self, nid: NodeId) {
        self.bump_poly_edit_generation();
        let needs_geo = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            match &node.payload {
                NodePayload::PolygonalBody { modifiers, .. } => {
                    Self::poly_mods_need_geometry_reapply(modifiers)
                }
                _ => return,
            }
        };
        if needs_geo {
            // Base positions moved → all prefixes invalid; full stack (Subdivide etc.).
            self.poly_prefix_cache_remove(nid);
            self.poly_sync_mesh_cache(nid);
            return;
        }

        // Fast path: identity or normal-only stack — re-raster the base cage only.
        let _t0 = std::time::Instant::now();
        self.poly_invalidate_pick_cache();
        self.poly_wire_local_cache.remove(&nid);
        self.poly_prefix_cache_remove(nid);

        let (cache_id, mesh_arc, mods, key, live_faces) = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            match &node.payload {
                NodePayload::PolygonalBody {
                    mesh,
                    mesh_cache_id,
                    modifiers,
                    ..
                } => {
                    // Generation-based key, NOT `poly_display_cache_key` — that walks
                    // `content_signature` (O(mesh), ~13 ms of this path's measured 38 ms at 323k)
                    // and can never hit its memo here because the first line of this function just
                    // bumped the generation. Both consumers of this key (`poly_display_cache`,
                    // `poly_mesh_gpu_key`) treat a MISMATCH as "do a full sync": a generation key
                    // therefore costs exactly one extra full sync at drag end (through the budgeted
                    // queue) and can never serve a stale display. Same move, same reasoning, as the
                    // wire-overlay stamp (overlay_lines.rs, 2026-08-13).
                    let key = (
                        self.poly_edit_generation ^ 0xD15B_0000_0000_0000,
                        Self::poly_modifier_stack_hash(modifiers),
                    );
                    let key = (key.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (nid.0 as u64), key.1);
                    (
                        *mesh_cache_id,
                        mesh.clone(),
                        modifiers.clone(),
                        key,
                        mesh.face_count(),
                    )
                }
                _ => return,
            }
        };

        // Apply normal-only mods into an owned mesh when needed; else share the base Arc
        // (safe: we released display cache before make_mut, or rebuild a fresh Arc here).
        let display_mesh = {
            let need_normal_pass = mods.iter().any(|m| {
                m.enabled
                    && matches!(
                        m.mod_type,
                        core_math::scene_graph::PolyModifierType::WeightedNormal
                            | core_math::scene_graph::PolyModifierType::SmoothNormal
                    )
            });
            if need_normal_pass {
                let mut owned = (*mesh_arc).clone();
                let empty = std::collections::HashMap::new();
                for m in &mods {
                    if m.enabled {
                        // Normal-only pass (WeightedNormal / SmoothNormal); neither reads a
                        // geometry channel, so empty authored tables cannot change the result.
                        // Normal-only pass (WeightedNormal / SmoothNormal); neither reads a
                        // geometry channel, so empty authored tables cannot change the result.
                        let empty_c = std::collections::HashMap::new();
                        Self::apply_one_poly_modifier(
                            &mut owned, m, &empty,
                            &mut core_math::mesh_boolean::BoolProvenance::default(),
                            &Default::default(), &Default::default(), &empty_c,
                        );
                    }
                }
                Arc::new(owned)
            } else {
                mesh_arc
            }
        };

        self.poly_display_cache.insert(
            nid,
            PolyDisplayMeshCache {
                key,
                mesh: display_mesh.clone(),
                arc_ptr: None,
                face_count: None,
            },
        );
        self.poly_lod_invalidate(nid);

        let nrm = self.poly_normal_state(nid, &display_mesh);
        let _ms_key = _t0.elapsed().as_secs_f32() * 1000.0;
        let _t1 = std::time::Instant::now();
        let (mut verts, indices, mat_ranges) =
            core_math::poly_mesh::raster_data_ngon_with_normals(&display_mesh, &nrm.cfg());
        let _ms_raster = _t1.elapsed().as_secs_f32() * 1000.0;
        let _t2 = std::time::Instant::now();
        let tri_count = indices.len() as u32 / 3;
        let ranges = if mat_ranges.len() > 1 {
            Some(mat_ranges)
        } else {
            None
        };

        // B.4: position edit diverges content — COW so Shift+D siblings keep the old surface.
        let cid_before_cow = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody {
                mesh_cache_id: Some(c),
                ..
            } => Some(*c),
            _ => None,
        });
        self.cow_mesh_cache_id_before_write(nid);
        let cache_id = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody {
                mesh_cache_id: Some(c),
                ..
            } => Some(*c),
            _ => None,
        });

        // Locality spike — position-tier DirtyMeshRegion path: topology-stable edits
        // (G/R/S, extrude drag, shrink/fatten) keep index buffer + draw ranges; only the
        // vertex buffer is rewritten in place (`update_chunk_verts_in_place` → verts-only
        // GPU upload). Full replace only when vert count / topology diverges.
        // Structural mesh change (COW'd/new cid, or topology full-replace) must mark MESH:
        // the cached structural draw list binds (cid, ranges, tri_count) and dirty.rs is
        // the ONE place that rebuilds it + busts the instance plane key. RENDER alone
        // repaints the old draw list — the "extrude solid frozen while wires update" bug.
        // Topology-stable in-place vert uploads keep RENDER (no per-frame rebuild in drags).
        let mut structural_change = cache_id != cid_before_cow;
        if let Some(cid) = cache_id {
            // Ask the precondition FIRST so the vertex Vec can be MOVED, not cloned: the defensive
            // `verts.clone()` was ~5 MB per drag frame at 323k tris, paid only to keep a fallback
            // alive for the rare topology-diverged case.
            let can = self.cache.mesh_cache.can_update_verts_in_place(cid, verts.len());
            let n_verts = verts.len();
            let in_place = if can {
                let ok = self.cache.mesh_cache.update_chunk_verts_in_place(cid, verts);
                debug_assert!(ok, "in-place vert write refused after its precondition passed");
                verts = Vec::new(); // moved; fallback below is unreachable when `can`
                ok
            } else {
                false
            };
            Self::poly_trace(&format!(
                "pos-edit nid={nid:?} cid={cid:?} in_place={in_place} cow={} verts={} tris={tri_count}",
                cache_id != cid_before_cow,
                n_verts
            ));
            if !in_place {
                self.cache.mesh_cache.update_editable_mesh_ex(
                    cid,
                    (verts, indices),
                    tri_count,
                    ranges,
                );
                structural_change = true;
            }
            self.cache.poly_mesh_gpu_key.insert(cid, key);
        } else {
            let cid = self.cache.mesh_cache.alloc_id();
            self.cache.mesh_cache.update_editable_mesh_ex(
                cid,
                (verts, indices),
                tri_count,
                ranges,
            );
            structural_change = true;
            self.cache.poly_mesh_gpu_key.insert(cid, key);
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh_cache_id, .. } = &mut node.payload {
                    *mesh_cache_id = Some(cid);
                }
            }
        }

        let _ms_write = _t2.elapsed().as_secs_f32() * 1000.0;
        let _t3 = std::time::Instant::now();
        // Same-frame GPU upload (keep solid drawable during drag).
        if let Some(gpu) = self.gpu.as_ref() {
            self.cache
                .mesh_cache
                .upload_pending(&gpu.device, &gpu.queue);
            let cid_now = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody {
                    mesh_cache_id: Some(c),
                    ..
                } => Some(*c),
                _ => None,
            });
            let still_missing = cid_now
                .map(|c| {
                    self.cache.mesh_cache.solid_undrawable(c) && live_faces > 0 && tri_count > 0
                })
                .unwrap_or(false);
            if still_missing {
                self.cache
                    .mesh_cache
                    .upload_pending(&gpu.device, &gpu.queue);
            }
        }

        let _ms_gpu = _t3.elapsed().as_secs_f32() * 1000.0;
        if _ms_key + _ms_raster + _ms_write + _ms_gpu > 8.0 || crate::perf_spam_on() {
            eprintln!(
                "[PosEdit] {:.1}ms = key+caches {:.1} + raster {:.1} + write {:.1} + gpu {:.1}",
                _ms_key + _ms_raster + _ms_write + _ms_gpu, _ms_key, _ms_raster, _ms_write, _ms_gpu
            );
        }
        self.mark_dirty(if structural_change {
            crate::DirtyFlags::MESH
        } else {
            crate::DirtyFlags::RENDER
        });
        self.lab_notify_poly_mesh_changed(nid);
        let _ = live_faces;
        self.poly_sync_calls_this_frame =
            self.poly_sync_calls_this_frame.saturating_add(1);
    }

    /// After any topology/position edit, retessellate and update the mesh cache.
    /// Applies enabled modifiers (Mirror, etc.) non-destructively before rasterisation.
    ///
    /// Prefer [`poly_request_display_sync`] + [`poly_drain_display_dirty`] when many ops may
    /// touch the same body in one frame. Live drag may still call this directly.
    ///
    /// On the real-work path (not early-out), bumps [`poly_edit_generation`] so B1 memos and
    /// B2 LOD latches cannot retain pre-edit keys after direct callers skip the mark APIs.
    pub(crate) fn poly_sync_mesh_cache(&mut self, nid: NodeId) {
        // Generation-keyed skip (bug #6 — "the calcul take way too long like 5 seconde plus").
        // The display mesh is a PURE function of (base mesh content, modifier stack). Re-running
        // the modifier stack — up to 4 Catmull-Clark levels, a 256x face explosion — on EVERY sync
        // (each drag frame, undo's poly_sync_all_meshes over unchanged bodies, a redo panel's
        // redundant release+close syncs) was the measured hotspot. When neither the base content
        // signature nor the stack hash moved AND the GPU mesh is already resident, the retessellation
        // is pure waste: the buffer still holds the right triangles. `content_signature` covers
        // in-place (`Arc::make_mut`) edits too, so this can never serve a stale display.
        //
        // PLAN_FOR_GROK_VIEWPORT_PERF: do NOT invalidate pick / log on the early-out path —
        // that made "redundant syncs" still expensive even when the mesh was unchanged.
        // A sync call is BY CONTRACT "this body may have just mutated" (drain after marks,
        // or a drag tool calling directly each frame WITHOUT bumping poly_edit_generation).
        // The B1 key memo is generation-keyed, so trusting it here serves the PRE-edit
        // signature → the "fully hot" early-out below matches → solid frozen until some
        // unrelated action bumps the generation (owner repro: extrude solid updated only
        // "after another action"; AZRA_CTRLX log showed zero syncs during the edit).
        // Drop this body's memo so `poly_display_cache_key` re-walks real content; idle
        // pick/LOD polls (the memo's purpose) never come through this function.
        if let Ok(mut memo) = self.poly_display_key_memo.try_borrow_mut() {
            memo.remove(&nid);
        }
        let (sig, cid, live_faces, gpu_missing, sig_fresh) = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            match &node.payload {
                NodePayload::PolygonalBody {
                    mesh,
                    mesh_cache_id,
                    modifiers,
                    ..
                } => {
                    let cid = *mesh_cache_id;
                    let live = mesh.face_count();
                    // Drawable solid = gpu_mesh with index_count > 0. triangle_count alone is not
                    // enough: a dead DC fallback used to leave gpu_mesh=Some(index_count=0) while
                    // triangle_count stayed stale non-zero → skip forever → full mesh "vanishes".
                    let gpu_missing = cid
                        .map(|c| {
                            self.cache.mesh_cache.solid_undrawable(c)
                                || (live > 0
                                    && self
                                        .cache
                                        .mesh_cache
                                        .get(c)
                                        .map(|m| m.triangle_count == 0)
                                        .unwrap_or(true))
                        })
                        .unwrap_or(true);
                    // D0 / dense: full content_signature is O(verts+edges+faces) — multi-ms at
                    // multi-M tris. When Arc identity + face count + stack hash still match the
                    // last display entry, the mesh content is the same (immutable Arc). Skip the
                    // full walk. In-place unique-Arc edits must call poly_request_display_sync
                    // after mutation (already the case for tools); that still recomputes sig.
                    let stack_key = {
                        let mut sk = Self::poly_modifier_stack_hash(modifiers);
                        for m in modifiers.iter() {
                            if m.enabled
                                && m.mod_type == core_math::scene_graph::PolyModifierType::Boolean
                                && m.bool_cutter_nid != 0
                            {
                                use std::hash::{Hash, Hasher};
                                let mut hh = std::collections::hash_map::DefaultHasher::new();
                                sk.hash(&mut hh);
                                m.bool_cutter_nid.hash(&mut hh);
                                if let Some(cn) = self.scene.scene_graph.get(NodeId(m.bool_cutter_nid))
                                {
                                    if let NodePayload::PolygonalBody { mesh: cm, .. } = &cn.payload {
                                        // Cutter: Arc ptr + face count only (cheap); full sig on miss below.
                                        (std::sync::Arc::as_ptr(cm) as usize).hash(&mut hh);
                                        cm.face_count().hash(&mut hh);
                                    }
                                }
                                sk = hh.finish();
                            }
                        }
                        sk
                    };
                    let arc_ptr = std::sync::Arc::as_ptr(mesh) as usize;
                    // Safe skip only when the Arc is **shared** (immutable content). A unique
                    // Arc can be mutated in place via make_mut with the same pointer + face
                    // count (G/R/S verts) — matching arc_ptr alone would leave the GPU solid
                    // stale while HE overlays already show the new shape.
                    let arc_shared = std::sync::Arc::strong_count(mesh) > 1;
                    // GPU-TRUTH GATE: a display-cache key match alone does NOT prove the GPU
                    // buffer holds that content — `poly_cached_display_mesh` (overlays, export)
                    // stamps the key WITHOUT uploading. `poly_mesh_gpu_key` is written only
                    // alongside a real upload, so it is the only structure that records what
                    // the GPU actually holds (same predicate the B.4 sibling-seed path uses).
                    // Without it: delete a modifier via any DEFERRED sync (undo, pipe editor,
                    // request+drain) → overlay stamps the new key between queue and drain →
                    // this skip fires → the decimated solid stays on screen forever
                    // (owner 2026-08-07: "delete it and now all the geo was decimated").
                    if cid.is_some()
                        && !gpu_missing
                        && arc_shared
                        && self.poly_display_cache.get(&nid).map_or(false, |e| {
                            e.key.1 == stack_key
                                && e.arc_ptr == Some(arc_ptr)
                                && e.face_count == Some(live)
                                && cid.map_or(false, |c| {
                                    self.cache.poly_mesh_gpu_key.get(&c) == Some(&e.key)
                                })
                        })
                    {
                        Self::poly_trace(&format!("sync nid={nid:?} EARLY-OUT arc (faces={live})"));
                        return;
                    }
                    // Fresh by construction — the memo for this nid was dropped above — but read
                    // the flag rather than assume it, so removing that drop can never silently
                    // start feeding the prefix ladder a generation-keyed signature.
                    let (csig, sk, fresh) = self.poly_display_cache_key_fresh(nid, mesh, modifiers);
                    ((csig, sk), cid, live, gpu_missing, fresh)
                }
                _ => return,
            }
        };
        let resident = cid.is_some();
        // Fully hot: key match + drawable solid + GPU-truth — skip everything (including
        // pick invalidate). The third clause is load-bearing, see GPU-TRUTH GATE above.
        if resident
            && !gpu_missing
            && self
                .poly_display_cache
                .get(&nid)
                .map(|e| e.key == sig)
                .unwrap_or(false)
            && cid.map_or(false, |c| self.cache.poly_mesh_gpu_key.get(&c) == Some(&sig))
        {
            Self::poly_trace(&format!("sync nid={nid:?} EARLY-OUT hot (sig match)"));
            return;
        }

        // B.4 / C.1: Shift+D sibling — this MeshCacheId already holds this exact display
        // content (`poly_mesh_gpu_key`). Seed display only; do not COW+reupload (N dups →
        // one mesh). Works even when GPU is still cold (CPU entry already written).
        if resident {
            let cid = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody {
                    mesh_cache_id: Some(c),
                    ..
                } => Some(*c),
                _ => None,
            });
            if let Some(cid) = cid {
                if self.cache.poly_mesh_gpu_key.get(&cid) == Some(&sig) {
                    if self
                        .poly_display_cache
                        .get(&nid)
                        .map(|e| e.key != sig)
                        .unwrap_or(true)
                    {
                        let (mesh_arc, mods) = {
                            let node = match self.scene.scene_graph.get(nid) {
                                Some(n) => n,
                                None => return,
                            };
                            match &node.payload {
                                NodePayload::PolygonalBody {
                                    mesh, modifiers, ..
                                } => (mesh.clone(), modifiers.clone()),
                                _ => return,
                            }
                        };
                        let cutters = self.poly_ref_meshes_for(nid, &mods);
                        let dm = self.apply_poly_modifiers_cached(
                            nid,
                            &mesh_arc,
                            &mods,
                            &cutters,
                            if sig_fresh { Some(sig.0) } else { None },
                        );
                        self.poly_modifier_evals_this_frame =
                            self.poly_modifier_evals_this_frame.saturating_add(1);
                        self.poly_display_cache.insert(
                            nid,
                            PolyDisplayMeshCache {
                                key: sig,
                                mesh: dm,
                                arc_ptr: None,
                                face_count: None,
                            },
                        );
                    }
                    return;
                }
            }
        }

        // Content (or GPU residency) actually needs work — count + invalidate pick once.
        // Bump edit generation so pick/LOD memos cannot serve a pre-edit (csig, stack_key)
        // after tools that call `poly_sync_mesh_cache` directly (bevel live, inset, etc.).
        // Early-out paths above leave generation alone so redundant syncs stay free.
        self.bump_poly_edit_generation();
        self.poly_sync_calls_this_frame =
            self.poly_sync_calls_this_frame.saturating_add(1);
        self.poly_invalidate_pick_cache();

        // Everything from here to the raster call is the stage the [SyncSplit] line calls
        // `resolve`: the display-cache lookup, and on a miss the whole modifier-stack evaluation.
        // The 2026-08-12 split showed raster+upload accounting for only ~37 of the ~82 ms `mesh`
        // bucket, so the rest lives here and needed naming before anything is changed.
        let _t_res = std::time::Instant::now();
        // Key hot but GPU cold (or missing Arc mesh): re-use cached display mesh without re-eval.
        let cached_display = self
            .poly_display_cache
            .get(&nid)
            .filter(|e| e.key == sig)
            .map(|e| e.mesh.clone());

        let (cache_id, display_mesh) = if let Some(dm) = cached_display {
            let cid = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody {
                    mesh_cache_id, ..
                } => *mesh_cache_id,
                _ => None,
            });
            // Display content changed (key moved) even though the fold was served from
            // cache — the coarse LOD meshes were built from the OLD content and must not
            // keep being drawn (a stale QEM-decimated LOD looks exactly like "the Decimate
            // modifier is still applied" after its removal).
            self.poly_lod_invalidate(nid);
            (cid, dm)
        } else {
            let (cache_id, mesh_arc, mods) = {
                let node = match self.scene.scene_graph.get(nid) {
                    Some(n) => n,
                    None => return,
                };
                match &node.payload {
                    NodePayload::PolygonalBody {
                        mesh,
                        mesh_cache_id,
                        modifiers,
                        ..
                    } => (*mesh_cache_id, mesh.clone(), modifiers.clone()),
                    _ => return,
                }
            };
            let cutters = self.poly_ref_meshes_for(nid, &mods);
            // No display-cache key was built on this path, so there is no signature to reuse.
            let dm = self.apply_poly_modifiers_cached(nid, &mesh_arc, &mods, &cutters, None);
            self.poly_modifier_evals_this_frame =
                self.poly_modifier_evals_this_frame.saturating_add(1);
            let base_arc = std::sync::Arc::as_ptr(&mesh_arc) as usize;
            let base_faces = mesh_arc.face_count();
            self.poly_display_cache.insert(
                nid,
                PolyDisplayMeshCache {
                    key: sig,
                    mesh: dm.clone(),
                    arc_ptr: Some(base_arc),
                    face_count: Some(base_faces),
                },
            );
            self.poly_lod_invalidate(nid);
            (cache_id, dm)
        };
        // Ear-clip triangulation (concave-safe) + material ranges for multi-mat faces.
        // Normals come from the body's authored state (Auto Smooth angle + Mark Sharp).
        //
        // [SyncSplit] — `poly_drain_display_dirty` measured 44–53 ms EVERY frame of a scripted
        // 200-frame edit (2026-08-12), i.e. roughly half the 135 ms edit frame, and the coarse
        // timer names the drain but not WHICH stage inside it. Same threshold discipline as
        // `[SceneOv]`/`[MidOv]`: only slow syncs print, so a healthy session pays nothing.
        let _ms_res = _t_res.elapsed().as_secs_f32() * 1000.0;
        let _t_nrm = std::time::Instant::now();
        let nrm = self.poly_normal_state(nid, &display_mesh);
        let _ms_nrm = _t_nrm.elapsed().as_secs_f32() * 1000.0;
        let _t_ras = std::time::Instant::now();
        let (verts, indices, mat_ranges) =
            core_math::poly_mesh::raster_data_ngon_with_normals(&display_mesh, &nrm.cfg());
        let _ms_ras = _t_ras.elapsed().as_secs_f32() * 1000.0;
        let raster = (verts, indices);
        let tri_count = raster.1.len() as u32 / 3;
        let vert_count = raster.0.len();
        Self::ctrx_diag(&format!(
            "poly_sync nid={nid:?} live_faces={live_faces} raster_verts={vert_count} tris={tri_count} \
             gpu_was_missing={gpu_missing} mat_ranges={}",
            mat_ranges.len()
        ));

        let ranges = if mat_ranges.len() > 1 {
            Some(mat_ranges)
        } else {
            None
        };
        // B.4: shared MeshCacheId from Shift+D — COW only when content diverges (write needed).
        // Matching content already returned above via poly_mesh_gpu_key.
        self.cow_mesh_cache_id_before_write(nid);
        let cache_id = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody {
                mesh_cache_id: Some(c),
                ..
            } => Some(*c),
            _ => None,
        });
        let _t_up = std::time::Instant::now();
        if let Some(cid) = cache_id {
            // NO TOPOLOGY-STABLE FAST PATH HERE — tried 2026-08-12, MEASURED SLOWER, reverted.
            // The idea was sound (a vertex grab changes no connectivity, so the index buffer is
            // identical and `update_chunk_verts_in_place` already implements that contract), but an
            // interleaved A/B in ONE process — alternate frames fast/full, so both populations see
            // the same machine load — came out at upload 21.3 ms fast vs 13.2 ms full over 101/100
            // samples, with `resolve` and `raster` matching to 0.3 ms (which is what validates the
            // harness: those stages are untouched by the switch).
            //
            // Cause: the in-place helper is not the cheaper path at this size. It CLONES the vertex
            // array into `imported_raster`, then mirrors positions and normals into `cpu_mesh` with
            // two serial per-vertex loops. `update_editable_mesh_ex` does the same split under
            // rayon. At 323k tris the serial loops lose by more than the saved index clone wins.
            // A real win here needs the index-reuse to happen WITHOUT the serial mirror — i.e. in
            // `raster_data_ngon_with_normals` (skip re-triangulation), not at the cache boundary.
            //
            // REGRESSION NOTE 2026-08-13: the revert of that experiment deleted this branch's
            // ENTIRE body — including the update call below — leaving only the key stamp. Every
            // sync on an existing body computed the raster, threw it away, and stamped the key as
            // if it had uploaded ("[SyncSplit] upload 0.0" in the owner's console; "mesh no longer
            // updates when modeling"; the stuck Ctrl+Z). The key stamp MUST come with the write.
            self.cache
                .mesh_cache
                .update_editable_mesh_ex(cid, raster, tri_count, ranges);
            self.cache.poly_mesh_gpu_key.insert(cid, sig);
            let _ms_up = _t_up.elapsed().as_secs_f32() * 1000.0;
            if _ms_res + _ms_nrm + _ms_ras + _ms_up > 10.0 || crate::perf_spam_on() {
                eprintln!(
                    "[SyncSplit] {:.1}ms = resolve {:.1} + normals {:.1} + raster {:.1} + upload {:.1}                      | tris={} evals={}",
                    _ms_res + _ms_nrm + _ms_ras + _ms_up,
                    _ms_res, _ms_nrm, _ms_ras, _ms_up, tri_count,
                    self.poly_modifier_evals_this_frame
                );
            }
        } else {
            let cid = self.cache.mesh_cache.alloc_id();
            self.cache
                .mesh_cache
                .update_editable_mesh_ex(cid, raster, tri_count, ranges);
            self.cache.poly_mesh_gpu_key.insert(cid, sig);
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh_cache_id, .. } = &mut node.payload {
                    *mesh_cache_id = Some(cid);
                }
            }
        }
        // Ensure cache entry is present (already inserted on eval path; re-assert key on GPU-only path).
        if self
            .poly_display_cache
            .get(&nid)
            .map(|e| e.key != sig)
            .unwrap_or(true)
        {
            // Base Arc identity for O(1) idle skip (see early-out above).
            let (base_arc, base_faces) = self.scene.scene_graph.get(nid).and_then(|n| {
                match &n.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Some((
                        std::sync::Arc::as_ptr(mesh) as usize,
                        mesh.face_count(),
                    )),
                    _ => None,
                }
            }).unwrap_or((0, 0));
            self.poly_display_cache.insert(
                nid,
                PolyDisplayMeshCache {
                    key: sig,
                    mesh: display_mesh.clone(),
                    arc_ptr: if base_arc != 0 { Some(base_arc) } else { None },
                    face_count: if base_faces > 0 { Some(base_faces) } else { None },
                },
            );
        }
        // Same-frame GPU upload: solid mesh must not stay undrawable after topology edit.
        if let Some(gpu) = self.gpu.as_ref() {
            self.cache
                .mesh_cache
                .upload_pending(&gpu.device, &gpu.queue);
            // Verify solid is drawable; if upload was skipped (budget) or hit the empty-index
            // path, force another drain (upload_pending re-queues from imported_persistent).
            let cid_now = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody {
                    mesh_cache_id: Some(c),
                    ..
                } => Some(*c),
                _ => None,
            });
            let still_missing = cid_now
                .map(|c| {
                    self.cache.mesh_cache.solid_undrawable(c)
                        && live_faces > 0
                        && tri_count > 0
                })
                .unwrap_or(false);
            if still_missing {
                Self::ctrx_diag("poly_sync: GPU still undrawable after upload — second drain");
                self.cache
                    .mesh_cache
                    .upload_pending(&gpu.device, &gpu.queue);
            }
            let gpu_ok = cid_now
                .and_then(|c| self.cache.mesh_cache.get(c))
                .and_then(|c| c.gpu_mesh.as_ref().map(|g| g.index_count))
                .unwrap_or(0);
            Self::ctrx_diag(&format!(
                "poly_sync GPU index_count={gpu_ok} (expect ~{} indices for {tri_count} tris)",
                tri_count * 3
            ));
            if live_faces > 0 && tri_count > 0 && gpu_ok == 0 {
                Self::ctrx_diag(
                    "poly_sync WARN: solid still has 0 indices with live faces — viewport will vanish",
                );
            }
        } else {
            Self::ctrx_diag("poly_sync: no GPU ctx — solid cannot upload");
        }
        // Full retessellation = structural mesh change (cid may have COW'd; tri_count/ranges
        // replaced). MESH rebuilds the structural draw list + busts the instance plane key
        // centrally (dirty.rs) — RENDER alone repaints the stale list (frozen-solid bug).
        // Also keeps painting every frame during bevel/extrude drag.
        self.mark_dirty(crate::DirtyFlags::MESH);
        // LAB poly→SDF live modifier: every retessellation is the geometry-change signal —
        // stamp the debounced refresh clock for linked MeshVolume prims (no-op when unlinked).
        self.lab_notify_poly_mesh_changed(nid);
    }

    /// Order-sensitive hash of every modifier field that changes the DISPLAY mesh (skips the
    /// `expanded` UI-only flag). Half of the display cache key in `poly_sync_mesh_cache`; the other
    /// half is the base mesh's `content_signature`. Floats hash by bit pattern so a re-derived-equal
    /// value keys identically.
    pub(crate) fn poly_modifier_stack_hash(mods: &[core_math::scene_graph::PolyModifier]) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        Self::hash_modifier_stack_into(mods, &mut h);
        h.finish()
    }

    /// Order-independent quantized endpoint key for an edge. Tags are carried by POSITION
    /// because every modifier renumbers ids; positions hold until something moves geometry,
    /// which is the honest lifetime for "this edge was created by an operation".
    pub(crate) fn edge_pos_key(
        mesh: &HalfEdgeMesh,
        he: core_math::half_edge::HalfEdgeId,
    ) -> (i64, i64, i64, i64, i64, i64) {
        const Q: f32 = 1e5;
        let k = |p: glam::Vec3| {
            ((p.x * Q).round() as i64, (p.y * Q).round() as i64, (p.z * Q).round() as i64)
        };
        let a = k(mesh.vert(mesh.he_source(he)).position);
        let b = k(mesh.vert(mesh.he(he).vertex).position);
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        (a.0, a.1, a.2, b.0, b.1, b.2)
    }

    /// Every live half-edge of `nid`'s base mesh that carries AUTHORED sidecar intent, and so
    /// must survive any cleanup pass. ONE definition of "protected" for every cleanup caller.
    ///
    /// Owner 2026-08-05: "mesh clean up we have some mesh clean up operation that break the tag
    /// systeme, usaly all edge that are tag to bevel are important so mesh clean up should never
    /// touch it." Four layers qualify, because each stores intent ON the edge that is silently
    /// destroyed if the edge is dissolved or merged away:
    ///   * `op_created` — Smart Select's do-not-bevel tag (the owner's "tag systeme")
    ///   * `sharp`      — Mark Sharp, drives the split-normal fan
    ///   * `seam`       — authored UV seam
    ///   * `crease`     — subdiv crease > 0
    /// A `None`/empty result means "nothing authored here", and cleanup keeps its old freedom.
    pub(crate) fn poly_protected_edges(
        &self,
        nid: NodeId,
        mesh: &HalfEdgeMesh,
    ) -> std::collections::HashSet<core_math::half_edge::HalfEdgeId> {
        use core_math::half_edge::HalfEdgeId;
        let mut out = std::collections::HashSet::new();
        let Some(cd) = self.cache.poly_custom_data.get(&nid) else {
            return out;
        };
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let creased = {
                let k = core_math::poly_mesh::EdgeKey::new(mesh.he_source(he), mesh.he(he).vertex);
                cd.crease.get(k).is_some_and(|v| *v > 0.0)
            };
            if cd.op_created.get(he) || cd.sharp.get(he) || cd.seam.get(he) || creased {
                out.insert(he);
            }
        }
        out
    }

    /// The same protected set expressed as undirected endpoint POSITION pairs — the identity
    /// that survives a rebuild, which is what `cleanup_mesh_with` needs (it renumbers everything).
    pub(crate) fn poly_protected_edge_positions(
        &self,
        nid: NodeId,
        mesh: &HalfEdgeMesh,
    ) -> Vec<(glam::Vec3, glam::Vec3)> {
        self.poly_protected_edges(nid, mesh)
            .into_iter()
            .map(|he| {
                (
                    mesh.vert(mesh.he_source(he)).position,
                    mesh.vert(mesh.he(he).vertex).position,
                )
            })
            .collect()
    }

    fn hash_modifier_stack_into(mods: &[PolyModifier], h: &mut impl std::hash::Hasher) {
        use std::hash::Hasher;
        for m in mods {
            h.write_u8(m.enabled as u8);
            h.write_u8(m.mod_type as u8);
            h.write_u32(m.subdiv_levels);
            h.write_u8(m.mirror_axis);
            h.write_u8(m.mirror_merge as u8);
            h.write_u32(m.mirror_merge_threshold.to_bits());
            h.write_u64(m.mirror_object_nid);
            h.write_u32(m.solidify_thickness.to_bits());
            h.write_u32(m.solidify_offset.to_bits());
            h.write_u32(m.smooth_iterations);
            h.write_u32(m.smooth_factor.to_bits());
            // Smooth's mask feather. Unhashed = the slider would not move the display-mesh key
            // and the control would look frozen (the same trap the Pipe note below records).
            h.write_u32(m.smooth_falloff);
            h.write_u32(m.array_count);
            h.write_u32(m.array_offset.x.to_bits());
            h.write_u32(m.array_offset.y.to_bits());
            h.write_u32(m.array_offset.z.to_bits());
            h.write_u32(m.bevel_width.to_bits());
            h.write_u32(m.bevel_segments);
            h.write_u32(m.bevel_shape.to_bits());
            h.write_u8(m.bevel_clamp as u8);
            // Variable width. Both MUST be in the hash: they change the output mesh, so leaving
            // them out would leave the display-cache key identical and the toggle would appear to do
            // nothing until some unrelated edit forced a rebuild (the trap documented above the
            // Smart Select tag fold).
            h.write_u8(m.bevel_var_width);
            h.write_u32(m.bevel_width_min.to_bits());
            h.write_u8(m.bevel_auto_merge as u8);
            h.write_u32(m.decimate_angle.to_bits());
            h.write_u8(m.bool_op);
            h.write_u64(m.bool_cutter_nid);
            h.write_u32(m.weighted_keep_sharp_deg.to_bits());
            h.write_u32(m.circ_angle_deg.to_bits());
            h.write_u8(m.circ_axis);
            h.write_u32(m.circ_center.x.to_bits());
            h.write_u32(m.circ_center.y.to_bits());
            h.write_u32(m.circ_center.z.to_bits());
            h.write_u32(m.circ_radius.to_bits());
            // Bend. Same rule as Pipe below — every one of these changes the output mesh, so a field
            // left out here would leave the display-cache key identical and freeze the control.
            h.write_u32(m.bend_angle_deg.to_bits());
            h.write_u8(m.bend_axis);
            h.write_u32(m.bend_dir_deg.to_bits());
            h.write_u32(m.bend_limit_lo.to_bits());
            h.write_u32(m.bend_limit_hi.to_bits());
            h.write_u32(m.bend_center.x.to_bits());
            h.write_u32(m.bend_center.y.to_bits());
            h.write_u32(m.bend_center.z.to_bits());
            // Pipe. A field absent here is a field the user cannot edit: the signature would
            // not change, the cached display mesh would be reused, and the modifier would
            // look frozen (owner 2026-08-04: "the pipe modifier does not update live").
            h.write_u8(m.bevel_smart_select as u8);
            m.geo_channel.hash_into(h);
            h.write_u32(m.inset_thickness.to_bits());
            h.write_u32(m.inset_depth.to_bits());
            h.write_u8(m.inset_individual as u8);
            h.write_u8(m.weighted_mode);
            h.write_u32(m.weighted_weight);
            h.write_u32(m.weighted_threshold.to_bits());
            h.write_u32(m.pipe_radius.to_bits());
            h.write_u32(m.pipe_segments);
            h.write_u8(m.pipe_closed as u8);
            h.write_u8(m.pipe_caps as u8);
            h.write_usize(m.pipe_path.len());
            for p in &m.pipe_path {
                h.write_u32(p.x.to_bits());
                h.write_u32(p.y.to_bits());
                h.write_u32(p.z.to_bits());
            }
            // Scatter. Same rule as Pipe above: an unhashed field is a field the user cannot
            // edit — the key would not move, the cached mesh would be reused, and the slider
            // would do nothing. `every_scatter_field_changes_the_stack_hash` guards this.
            h.write_u64(m.scatter_source_nid);
            h.write_u8(m.scatter_source);
            h.write_u32(m.scatter_count);
            h.write_u32(m.scatter_seed);
            h.write_u8(m.scatter_align_normal as u8);
            h.write_u32(m.scatter_spin_deg.to_bits());
            for v in [m.scatter_rot_jitter_deg, m.scatter_pos_jitter, m.scatter_scale_axis_min, m.scatter_scale_axis_max] {
                h.write_u32(v.x.to_bits());
                h.write_u32(v.y.to_bits());
                h.write_u32(v.z.to_bits());
            }
            h.write_u32(m.scatter_scale_min.to_bits());
            h.write_u32(m.scatter_scale_max.to_bits());
            h.write_u8(m.bevel_consume as u8);
            h.write_u8(m.bevel_limit_angle as u8);
            h.write_u32(m.bevel_angle_deg.to_bits());
            // Vertex Merge / Mesh Cleanup — every editable field must move the stack hash.
            h.write_u32(m.merge_distance.to_bits());
            h.write_u8(m.cleanup_flatten_folds as u8);
            h.write_u8(m.cleanup_merge_coplanar as u8);
            h.write_u32(m.cleanup_dissolve_angle.to_bits());
            // Voxel Remesh — unhashed fields freeze the slider (same rule as Pipe/Scatter).
            h.write_u32(m.remesh_resolution);
            h.write_u8(m.remesh_smooth as u8);
            h.write_u32(m.remesh_padding.to_bits());
            // Reprojection strength is an OUTPUT-changing param, so it must be in the stack
            // hash or the prefix/display cache would serve the previous mesh when it moves.
            h.write_u32(m.remesh_project.to_bits());
            h.write_u8(m.remesh_collapse as u8);
            h.write_u8(m.remesh_skip_relax_drag as u8);
            h.write_u8(m.remesh_skip_seal_drag as u8);
            h.write_u8(m.remesh_drop_res_drag as u8);
            h.write_u8(m.remesh_union_islands as u8);
            h.write_u64(m.sculpt_delta_gen);
            h.write_u64(m.sculpt_deltas.len() as u64);
        }
    }

    /// Resolve every OTHER poly body a modifier stack references (Boolean cutter, Scatter
    /// source), keyed by `NodeId.0`, with each mesh already expressed in **target local space**.
    ///
    /// ONE resolver for both, because both need exactly the same thing and both must fail the
    /// same way. A reference that is 0, points at a deleted node, points at a non-poly node, or
    /// points at the target ITSELF is silently absent from the map — the modifier then finds no
    /// mesh and no-ops. That is what makes a dangling or self-reference structurally incapable
    /// of panicking or recursing, rather than something each call site has to remember.
    pub(crate) fn poly_ref_meshes_for(
        &self,
        target_nid: NodeId,
        modifiers: &[core_math::scene_graph::PolyModifier],
    ) -> std::collections::HashMap<u64, core_math::half_edge::HalfEdgeMesh> {
        use core_math::scene_graph::{NodePayload, PolyModifierType};
        let mut out = std::collections::HashMap::new();
        let target_world = self.scene.scene_graph.world_transform(target_nid);
        let inv_target = target_world.inverse();
        for m in modifiers {
            if !m.enabled {
                continue;
            }
            let ref_nid = match m.mod_type {
                PolyModifierType::Boolean => m.bool_cutter_nid,
                PolyModifierType::Scatter => m.scatter_source_nid,
                _ => continue,
            };
            if ref_nid == 0 || ref_nid == target_nid.0 {
                continue;
            }
            if out.contains_key(&ref_nid) {
                continue;
            }
            let cnid = NodeId(ref_nid);
            let Some(cnode) = self.scene.scene_graph.get(cnid) else {
                continue;
            };
            let NodePayload::PolygonalBody { mesh: cmesh, .. } = &cnode.payload else {
                continue;
            };
            let local: core_math::half_edge::HalfEdgeMesh =
                if m.mod_type == PolyModifierType::Scatter {
                    // A Scatter SOURCE is a prototype, not a placed object: the copies are built
                    // around its OWN origin and the scatter's own transform decides where each one
                    // lands. Baking the source's world placement in would offset every instance by
                    // wherever the prototype happens to be parked in the scene, which is not what
                    // "scatter this object over that mesh" means. Its local geometry is used
                    // verbatim; size is then the user's, via the scale sliders.
                    (**cmesh).clone()
                } else {
                    // Cutter → world → target local: a boolean cuts where the cutter actually IS.
                    let cworld = self.scene.scene_graph.world_transform(cnid);
                    crate::poly::boolean::transformed(cmesh.as_ref(), inv_target * cworld)
                };
            out.insert(ref_nid, local);
        }
        out
    }

    /// Install a REBUILT base mesh on poly body `nid`, carrying its sidecar across in the SAME
    /// step. THE entry point for any destructive op that replaces `PolygonalBody.mesh` with a
    /// mesh whose ids are new.
    ///
    /// `from_polygons` / `merge_meshes` / the boolean kernel renumber every id, and
    /// `cache.poly_custom_data` is keyed BY id — so an install that only writes the mesh does not
    /// merely LOSE the UVs, seams, sharps, creases, polygroups and Smart Select tags, it aims
    /// them at unrelated edges of the new mesh. Carrying at the install is what makes "an
    /// operation must not change what it did not touch" true for attributes as well as geometry
    /// (owner 2026-08-11: "smart select after boolean cut still forgets the tag on geo that was
    /// not touched"). Every site that hand-rolled `*mesh = Arc::new(..)` had to remember this
    /// separately, and the ones that forgot are exactly the bugs — hence one function.
    ///
    /// `created` = half-edges this op BUILT, ids valid on `new_mesh`; stamped `op_created` on top
    /// of the carried sidecar. Pass `&[]` when the op has no provenance to declare — fail closed
    /// (Smart Select bevels an edge it knows nothing about) rather than guessing a tag on.
    ///
    /// **Ops that only ALLOCATE must NOT use this** (extrude/inset keep every original id, and a
    /// position remap would drop the tags on every vertex they moved) — see
    /// [`crate::poly::op_tags::carry_op_tags`], which documents that split.
    pub(crate) fn poly_install_rebuilt_mesh(
        &mut self,
        nid: NodeId,
        new_mesh: HalfEdgeMesh,
        created: &[core_math::half_edge::HalfEdgeId],
    ) {
        use core_math::scene_graph::NodePayload;
        use std::sync::Arc;

        let Some(old) = self.poly_node_mesh(nid) else { return };
        let carried = crate::poly::op_tags::carry_op_tags(
            self.cache.poly_custom_data.get(&nid),
            &old,
            &new_mesh,
            created,
        );
        match carried {
            Some(cd) => {
                self.cache.poly_custom_data.insert(nid, cd);
            }
            // Nothing authored and nothing created: drop the entry rather than leave stale keys.
            None => {
                self.cache.poly_custom_data.remove(&nid);
            }
        }
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = Arc::new(new_mesh);
            }
        }
        self.poly_invalidate_pick_cache();
        self.poly_request_display_sync(nid);
    }

    /// This body's persisted Smart Select tags, keyed by POSITION off `base`.
    ///
    /// Every modifier renumbers, so the ids in the sidecar mean nothing to the evaluated mesh;
    /// positions hold until something moves geometry. ONE builder so the live display path and
    /// the Apply bake cannot disagree about which edges are operation-created — they used to,
    /// and Apply baked geometry the viewport had never shown.
    pub(crate) fn poly_op_tag_keys(
        &self,
        nid: NodeId,
        base: &HalfEdgeMesh,
    ) -> std::collections::HashSet<(i64, i64, i64, i64, i64, i64)> {
        let mut tags = std::collections::HashSet::new();
        if let Some(cd) = self.cache.poly_custom_data.get(&nid) {
            for i in 0..base.half_edges.len() as u32 {
                let he = core_math::half_edge::HalfEdgeId(i);
                if base.is_edge_live(he) && cd.op_created.get(he) {
                    tags.insert(Self::edge_pos_key(base, he));
                }
            }
        }
        tags
    }

    /// Bake the single modifier at stack index `mi` into the base mesh and remove only that entry.
    ///
    /// Evaluation runs through `0..=mi` so the clicked modifier is applied in stack order (same
    /// geometry the viewport shows when later modifiers are ignored). Only the clicked card is
    /// removed — modifiers above and below stay on the stack.
    ///
    /// Entries above `mi` keep their cards but are turned **off**: their effect is already written
    /// into the baked base, and leaving them enabled would re-run them (double apply). Re-enable
    /// only if you intentionally want that. Undo via `transaction`.
    pub(crate) fn apply_poly_modifier_through(&mut self, nid: NodeId, mi: usize) {
        use core_math::scene_graph::NodePayload;

        let (base, mods, label) = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, modifiers, .. } = &node.payload else {
                self.toast("Apply: select a polygonal mesh");
                return;
            };
            if mi >= modifiers.len() {
                return;
            }
            let label = modifiers[mi].label().to_string();
            ((**mesh).clone(), modifiers.clone(), label)
        };

        // Evaluate through the clicked slot so it is applied after the live stack above it.
        let prefix = &mods[..=mi];
        let cutters = self.poly_ref_meshes_for(nid, prefix);
        let centers = self.poly_mirror_centers_for(nid, prefix);
        // BAKE WHAT THE VIEWPORT SHOWED. These two were `Default::default()` — an EMPTY tag set
        // and empty authored channels — while the live display path (`apply_poly_modifiers_cached`)
        // evaluates the same stack WITH them. So a Smart Select bevel, a polygroup-masked or
        // edge-weight-driven modifier previewed one mesh and Apply wrote a different one.
        let tags = self.poly_op_tag_keys(nid, &base);
        let auth = self.poly_authored_channels(nid, &base);
        let baked =
            Self::apply_poly_modifiers_tagged(&base, prefix, &cutters, &tags, &auth, &centers);
        // Only the clicked entry leaves the stack. Entries above stay as cards but are disabled
        // so they do not re-apply effects already baked into the mesh.
        let mut disabled_above = 0usize;
        let remaining: Vec<_> = mods
            .into_iter()
            .enumerate()
            .filter_map(|(i, mut m)| {
                if i == mi {
                    None
                } else {
                    if i < mi && m.enabled {
                        m.enabled = false;
                        disabled_above += 1;
                    }
                    Some(m)
                }
            })
            .collect();

        self.transaction(&format!("Apply {label}"), |s| {
            s.poly_release_shared_display_arcs(nid);
            // Baking is a REBUILD: every id in the sidecar is now meaningless. Carry it across
            // by position (this used to write the mesh alone, which left every UV / seam /
            // crease / polygroup / Smart Select tag pointing at an unrelated edge).
            s.poly_install_rebuilt_mesh(nid, baked, &[]);
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                    *modifiers = remaining;
                }
            }
            // Selection points at a stack index: drop if it was the applied slot, else shift
            // indices that sat below the removed card.
            if let Some((snid, si)) = s.ui.poly_mod_selected {
                if snid == nid {
                    if si == mi {
                        s.ui.poly_mod_selected = None;
                    } else if si > mi {
                        s.ui.poly_mod_selected = Some((nid, si - 1));
                    }
                }
            }
            s.poly_sync_mesh_cache(nid);
            s.overlay_lines_fp = u64::MAX;
            s.mark_dirty(crate::DirtyFlags::SCENE);
            s.mark_dirty(crate::DirtyFlags::RENDER);
            if disabled_above > 0 {
                s.toast(&format!(
                    "Applied {label} ({disabled_above} above disabled — already baked)"
                ));
            } else {
                s.toast(&format!("Applied {label}"));
            }
        });
    }

    /// Apply the modifier stack to a base mesh, returning the display mesh.
    /// Only enabled modifiers are applied. The base mesh is not mutated.
    /// Pure / cache-free path (unit tests, lab). Prefer `apply_poly_modifiers_cached` in the app.
    ///
    /// `cutters`: optional map `bool_cutter_nid → mesh already in target local space` for Boolean.
    pub(crate) fn apply_poly_modifiers(
        base: &core_math::half_edge::HalfEdgeMesh,
        modifiers: &[core_math::scene_graph::PolyModifier],
        cutters: &std::collections::HashMap<u64, core_math::half_edge::HalfEdgeMesh>,
    ) -> core_math::half_edge::HalfEdgeMesh {
        let empty_c = std::collections::HashMap::new();
        Self::apply_poly_modifiers_tagged(
            base,
            modifiers,
            cutters,
            &Default::default(),
            &Default::default(),
            &empty_c,
        )
    }

    /// This body's AUTHORED per-element data (polygroups, edge weights), re-keyed by POSITION so
    /// it survives the stack — the same carry the Smart Select tags use, and for the same reason:
    /// every id in `CustomData` is invalidated by the first modifier that renumbers.
    ///
    /// Built off `base` (the authoring domain). Empty when the body has no sidecar, in which case
    /// an authored channel matches nothing rather than everything.
    pub(crate) fn poly_authored_channels(
        &self,
        nid: NodeId,
        base: &HalfEdgeMesh,
    ) -> core_math::geometry_channel::AuthoredChannels {
        use core_math::geometry_channel::AuthoredChannels;
        use core_math::half_edge::{FaceId, HalfEdgeId, NO_EDGE};
        let mut out = AuthoredChannels::default();
        let Some(cd) = self.cache.poly_custom_data.get(&nid) else {
            return out;
        };
        for i in 0..base.faces.len() as u32 {
            let f = FaceId(i);
            if base.face(f).half_edge == NO_EDGE {
                continue;
            }
            if let Some(&gid) = cd.groups.get(f) {
                if gid != 0 {
                    out.group_by_face.insert(AuthoredChannels::face_key(base, f), gid);
                }
            }
        }
        for i in 0..base.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !base.is_edge_live(he) {
                continue;
            }
            let k = core_math::poly_mesh::EdgeKey::new(base.he_source(he), base.he(he).vertex);
            if let Some(&w) = cd.crease.get(k) {
                if w > 0.0 {
                    let a = base.vert(base.he_source(he)).position;
                    let b = base.vert(base.he(he).vertex).position;
                    out.weight_by_edge.insert(AuthoredChannels::edge_key(a, b), w);
                }
            }
        }
        out
    }

    /// As `apply_poly_modifiers`, plus the PERSISTED "created by an operation" tags read from
    /// the body's sidecar — position-keyed, because ids do not survive the stack. Bevel Smart
    /// Select consumes them: they are what makes it work on a mesh whose bevel/boolean were
    /// applied DESTRUCTIVELY, which is the owner's actual workflow.
    pub(crate) fn apply_poly_modifiers_tagged(
        base: &core_math::half_edge::HalfEdgeMesh,
        modifiers: &[core_math::scene_graph::PolyModifier],
        cutters: &std::collections::HashMap<u64, core_math::half_edge::HalfEdgeMesh>,
        op_created: &std::collections::HashSet<(i64, i64, i64, i64, i64, i64)>,
        auth: &core_math::geometry_channel::AuthoredChannels,
        mirror_centers: &std::collections::HashMap<u64, glam::Vec3>,
    ) -> core_math::half_edge::HalfEdgeMesh {
        let mut result = base.clone();
        // SMART SELECT provenance, threaded within a SINGLE evaluation: a Boolean modifier
        // records where each result face came from, and a Bevel modifier after it can then
        // skip the cutter's own tessellation exactly. Any other modifier clears the tags,
        // because they are indexed by result face and topology changes renumber faces --
        // stale tags would skip arbitrary edges. No persistence needed: the stack
        // re-evaluates on every edit.
        let mut prov = core_math::mesh_boolean::BoolProvenance::default();
        for m in modifiers {
            Self::apply_one_poly_modifier(
                &mut result, m, cutters, &mut prov, op_created, auth, mirror_centers,
            );
        }
        result
    }

    /// Create an Empty (null) at the resolved workplane origin for use as a Mirror center.
    /// Returns `None` if the graph cannot allocate a node.
    pub(crate) fn create_mirror_center_null_at_workplane(&mut self) -> Option<NodeId> {
        let origin = self.viewport.wp_resolved.origin;
        let root = self.scene.scene_graph.root();
        let name = self.next_unique_empty_name("MirrorCenter");
        let nid = self
            .scene
            .scene_graph
            .add_child(root, NodePayload::Empty { name: name.clone() });
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            // Workplane origin is world-space; Empty under Root uses local = world.
            node.local_position = origin;
        }
        self.mark_outliner_dirty();
        self.mark_dirty(crate::DirtyFlags::SCENE);
        self.toast(&format!("{name} at workplane origin"));
        Some(nid)
    }

    /// When the workplane is **Locked**, create a null at its origin for a new Mirror modifier.
    pub(crate) fn maybe_create_mirror_center_from_workplane(&mut self) -> Option<NodeId> {
        if self.viewport.wp_mode != crate::WorkPlaneMode::Locked {
            return None;
        }
        self.create_mirror_center_null_at_workplane()
    }

    fn next_unique_empty_name(&self, base: &str) -> String {
        let mut n = 1u32;
        loop {
            let candidate = if n == 1 {
                base.to_string()
            } else {
                format!("{base}.{n:03}")
            };
            let taken = self.scene.scene_graph.all_nodes().any(|(_, node)| {
                matches!(&node.payload, NodePayload::Empty { name } if name == &candidate)
            });
            if !taken {
                return candidate;
            }
            n += 1;
            if n > 999 {
                return format!("{base}.{}", self.scene.next_prim_id);
            }
        }
    }

    /// Resolve mirror-plane origins for enabled Mirror mods that reference a scene object.
    /// Values are in the **target body local** space (same as the mesh being evaluated).
    pub(crate) fn poly_mirror_centers_for(
        &self,
        target_nid: NodeId,
        modifiers: &[PolyModifier],
    ) -> std::collections::HashMap<u64, glam::Vec3> {
        use core_math::scene_graph::PolyModifierType;
        let mut out = std::collections::HashMap::new();
        let target_world = self.scene.scene_graph.world_transform(target_nid);
        let inv = target_world.inverse();
        if !inv.is_finite() {
            return out;
        }
        for m in modifiers {
            if !m.enabled
                || m.mod_type != PolyModifierType::Mirror
                || m.mirror_object_nid == 0
                || out.contains_key(&m.mirror_object_nid)
            {
                continue;
            }
            let cnid = NodeId(m.mirror_object_nid);
            if self.scene.scene_graph.get(cnid).is_none() {
                continue;
            }
            let world_c = self
                .scene
                .scene_graph
                .world_transform(cnid)
                .transform_point3(glam::Vec3::ZERO);
            let local = inv.transform_point3(world_c);
            if local.is_finite() {
                out.insert(m.mirror_object_nid, local);
            }
        }
        out
    }

    /// Apply a single stack slot (no-op when disabled). Shared by free + cached paths.
    ///
    /// `mirror_centers`: `mirror_object_nid → center in target-local space` (empty = local origin).
    pub(crate) fn apply_one_poly_modifier(
        result: &mut HalfEdgeMesh,
        m: &PolyModifier,
        cutters: &std::collections::HashMap<u64, HalfEdgeMesh>,
        prov: &mut core_math::mesh_boolean::BoolProvenance,
        op_created: &std::collections::HashSet<(i64, i64, i64, i64, i64, i64)>,
        auth: &core_math::geometry_channel::AuthoredChannels,
        mirror_centers: &std::collections::HashMap<u64, glam::Vec3>,
    ) {
        use core_math::mesh_boolean::{mesh_boolean, BoolOp};
        use core_math::scene_graph::PolyModifierType;
        if !m.enabled {
            return;
        }
        // Only the Boolean that produced the tags, and the Bevel that reads them, may see
        // them; anything else renumbers faces and invalidates them.
        if !matches!(m.mod_type, PolyModifierType::Boolean | PolyModifierType::Bevel) {
            *prov = core_math::mesh_boolean::BoolProvenance::default();
        }
        match m.mod_type {
            PolyModifierType::Mirror => {
                let axes = core_math::half_edge::MirrorAxes::from_bits(m.mirror_axis);
                let center = if m.mirror_object_nid != 0 {
                    mirror_centers
                        .get(&m.mirror_object_nid)
                        .copied()
                        .unwrap_or(glam::Vec3::ZERO)
                } else {
                    glam::Vec3::ZERO
                };
                // Local origin + no object → fast axis path (Blender default).
                if center.length_squared() < 1e-16 {
                    *result = result.mirrored(axes, m.mirror_merge, m.mirror_merge_threshold);
                } else {
                    // Plane through object origin: successive axis mirrors about `center`.
                    let thr = m.mirror_merge_threshold;
                    let merge = m.mirror_merge;
                    let mut out = result.clone();
                    if axes.contains_x() {
                        out = out.mirrored_across(center, glam::Vec3::X, merge, thr);
                    }
                    if axes.contains_y() {
                        out = out.mirrored_across(center, glam::Vec3::Y, merge, thr);
                    }
                    if axes.contains_z() {
                        out = out.mirrored_across(center, glam::Vec3::Z, merge, thr);
                    }
                    *result = out;
                }
            }
            PolyModifierType::Subdivide => {
                for _ in 0..m.subdiv_levels.min(4) {
                    result.subdivide_catmull_clark();
                }
            }
            PolyModifierType::Solidify => {
                result.solidify(m.solidify_thickness, m.solidify_offset);
            }
            PolyModifierType::Array => {
                // array_count = TOTAL instances including the original (min 1).
                let count = m.array_count.max(1);
                if count >= 2 {
                    let src = result.clone();
                    for i in 1..count {
                        let off = m.array_offset * i as f32;
                        let placed = crate::poly::boolean::transformed(
                            &src,
                            glam::Mat4::from_translation(off),
                        );
                        *result = crate::poly::boolean::merge_meshes(result, &placed);
                    }
                }
            }
            PolyModifierType::Duplicate => {
                let extras = m.array_count.max(1);
                let src = result.clone();
                for i in 1..=extras {
                    let off = m.array_offset * i as f32;
                    let placed = crate::poly::boolean::transformed(
                        &src,
                        glam::Mat4::from_translation(off),
                    );
                    *result = crate::poly::boolean::merge_meshes(result, &placed);
                }
            }
            PolyModifierType::Pipe => {
                // PROCEDURAL PIPE (owner-ratified 2026-08-04): the polyline is retained on
                // the modifier, so radius / segments stay editable forever. Generates the
                // tube and MERGES it into the stack result -- on an empty base body that is
                // the whole object; on an existing mesh it is a new island. An empty path
                // no-ops, which is also what a pre-Pipe .fl4 deserialises to.
                if m.pipe_path.len() >= 2 {
                    let tube = core_math::half_edge::HalfEdgeMesh::new_pipe(
                        &m.pipe_path,
                        m.pipe_radius.max(1e-4),
                        m.pipe_segments,
                        m.pipe_closed,
                        m.pipe_caps,
                    );
                    if tube.face_count() > 0 {
                        *result = if result.face_count() == 0 {
                            tube
                        } else {
                            crate::poly::boolean::merge_meshes(result, &tube)
                        };
                    }
                }
            }
            PolyModifierType::Scatter => {
                // SCATTER (owner 2026-08-05): "chose a object to scater on a mesh, the point
                // source are the poly ... scale, rotation, position parameter for ramdomeness".
                // The stack result so far IS the target surface; the copies are merged in as
                // separate islands, so the target mesh itself is never modified.
                //
                // NO-OP PATHS, all of which are silent by construction (never a panic, never
                // recursion): source unset (0) — which is also what a pre-Scatter .fl4 loads
                // as; source deleted or not a poly body, or the source IS this body — the
                // resolver drops all three, so the lookup simply misses; empty target mesh —
                // no sites; empty source mesh — nothing to place.
                let Some(src) = cutters.get(&m.scatter_source_nid) else {
                    return;
                };
                if src.face_count() == 0 || result.face_count() == 0 {
                    return;
                }
                let params = core_math::scatter::ScatterParams {
                    source: m.scatter_source,
                    count: m.scatter_count,
                    seed: m.scatter_seed,
                    align_to_normal: m.scatter_align_normal,
                    spin_deg: m.scatter_spin_deg,
                    rot_jitter_deg: m.scatter_rot_jitter_deg,
                    pos_jitter: m.scatter_pos_jitter,
                    scale_min: m.scatter_scale_min,
                    scale_max: m.scatter_scale_max,
                    scale_axis_min: m.scatter_scale_axis_min,
                    scale_axis_max: m.scatter_scale_axis_max,
                };
                let xf = core_math::scatter::scatter_transforms(result, &params);
                if xf.is_empty() {
                    return;
                }
                let placed: Vec<HalfEdgeMesh> = xf
                    .iter()
                    .map(|t| crate::poly::boolean::transformed(src, *t))
                    .collect();
                // One concatenation pass over target + every island: folding pairwise would be
                // O(N²) rebuilds at up to MAX_SCATTER_INSTANCES islands.
                let mut all: Vec<&HalfEdgeMesh> = Vec::with_capacity(placed.len() + 1);
                all.push(result);
                all.extend(placed.iter());
                *result = crate::poly::boolean::merge_mesh_list(&all);
            }
            PolyModifierType::ArrayCircular => {
                let count = m.array_count.max(1);
                if count >= 2 {
                    let src = result.clone();
                    let axis = match m.circ_axis {
                        0 => glam::Vec3::X,
                        1 => glam::Vec3::Y,
                        _ => glam::Vec3::Z,
                    };
                    let center = m.circ_center;
                    let total_rad = m.circ_angle_deg.to_radians();
                    let full_circle = (m.circ_angle_deg - 360.0).abs() < 0.05
                        || m.circ_angle_deg.abs() >= 359.5;
                    let step = if full_circle {
                        total_rad / count as f32
                    } else if count > 1 {
                        total_rad / (count - 1) as f32
                    } else {
                        0.0
                    };
                    let centroid = {
                        let mut acc = glam::Vec3::ZERO;
                        let mut n = 0u32;
                        for v in &src.vertices {
                            acc += v.position;
                            n += 1;
                        }
                        if n > 0 {
                            acc / n as f32
                        } else {
                            center
                        }
                    };
                    let mut radial0 = centroid - center;
                    let along = radial0.dot(axis);
                    radial0 = radial0 - axis * along;
                    if radial0.length_squared() < 1e-10 {
                        radial0 = if axis.dot(glam::Vec3::Y).abs() < 0.9 {
                            axis.cross(glam::Vec3::Y)
                        } else {
                            axis.cross(glam::Vec3::X)
                        };
                    }
                    let radial_dir = radial0.normalize_or_zero();
                    for i in 1..count {
                        let angle = step * i as f32;
                        let rot = glam::Mat4::from_axis_angle(axis, angle);
                        let to_c = glam::Mat4::from_translation(-center);
                        let from_c = glam::Mat4::from_translation(center);
                        let mut xform = from_c * rot * to_c;
                        if m.circ_radius.abs() > 1e-8 {
                            let extra =
                                rot.transform_vector3(radial_dir) * (m.circ_radius * i as f32);
                            xform = glam::Mat4::from_translation(extra) * xform;
                        }
                        let placed = crate::poly::boolean::transformed(&src, xform);
                        *result = crate::poly::boolean::merge_meshes(result, &placed);
                    }
                }
            }
            PolyModifierType::Triangulate => {
                // Geometry channel (Modo-style): which faces become triangles. All = every
                // face (legacy path). A restricted channel leaves non-matching quads/n-gons
                // intact — e.g. "N-gons only" triangulates only 5+ sides.
                if m.geo_channel.is_all() {
                    *result = core_math::poly_mesh::triangulate_mesh(result);
                } else {
                    let allowed = m.geo_channel.matching_faces_auth(result, Some(auth));
                    if allowed.is_empty() {
                        // Channel matches nothing — leave the mesh alone (never silent
                        // "triangulate everything" when the user asked for a filter).
                    } else {
                        *result =
                            core_math::poly_mesh::triangulate_mesh_faces(result, Some(&allowed));
                    }
                }
            }
            PolyModifierType::Smooth => {
                // Geometry channel (Modo-style), same role it plays on Bevel/Inset: WHICH
                // geometry this modifier is allowed to touch. A face/edge channel becomes a
                // feathered per-vertex strength mask — `None` when the channel is All, which
                // keeps the unfiltered stack on the exact pre-existing kernel path.
                let mask = m.geo_channel.vertex_weights_auth(
                    result,
                    m.smooth_falloff,
                    Some(auth),
                );
                result.smooth_laplacian_masked(
                    m.smooth_iterations,
                    m.smooth_factor,
                    mask.as_deref(),
                );
            }
            PolyModifierType::Bevel => {
                if m.bevel_width > 1e-6 {
                    let mut selected = all_live_half_edge_ids(result);
                    // Geometry channel first (Modo-style): only edges matching the channel.
                    if !m.geo_channel.is_all() {
                        let before = selected.len();
                        let as_vec: Vec<u32> = selected.iter().copied().collect();
                        let filtered = m.geo_channel.filter_edge_ids(result, &as_vec);
                        selected = filtered.into_iter().collect();
                        if before != selected.len() {
                            eprintln!(
                                "[BEVEL] geo channel {}: {} → {} edges",
                                core_math::geometry_channel::channel_summary(&m.geo_channel),
                                before,
                                selected.len()
                            );
                        }
                    }
                    // The set this modifier is ALLOWED to touch: after the user's EXPLICIT
                    // Geometry Channel, before any rule Smart Select infers. Captured here, not
                    // lower down, because the near-90 force-keep must still override the inferred
                    // rules (that is its whole job) while never escaping the channel.
                    let in_channel: Vec<u32> = selected.iter().copied().collect();
                    if m.bevel_smart_select {
                        // GEOMETRY ALWAYS, TAGS ADDITIONALLY (owner 2026-08-05: "no, those
                        // work — apply the bevel modifier and you will see they get beveled").
                        // This used to run ONLY when the mesh had no tags at all, so the
                        // moment a bevel wrote one tag the whole classification switched off
                        // and the boolean's flat support edges started beveling again. The
                        // two rules are not alternatives: an edge inside a round or lying
                        // flat is never a corner, whatever the tag record says. Tags ADD
                        // exclusions on top (they catch edges geometry cannot see).
                        //
                        // Owner reference files define the split: TEST_06 20 smooth + 2 flat
                        // vs 49 creases; TEST-08 3 smooth + 0 flat vs 18 creases.
                        // SMOOTH ONLY -- NOT FLAT (owner 2026-08-05: "edge created by knife
                        // cut should be included in the bevel, not removed with smart
                        // select"). A knife cut across a flat face produces a COPLANAR edge,
                        // geometrically identical to a boolean's flat support seam, and the
                        // user wants opposite treatment for the two: bevel the knife edge,
                        // skip the boolean seam. No geometric test can separate them -- only
                        // provenance can. So geometry now handles ONLY the unambiguous case
                        // (an edge inside a ROUND is never a corner, whatever made it), and
                        // flat edges are left entirely to tags, which the boolean writes.
                        let cos_t = SMART_SELECT_TANGENT_DEG.to_radians().cos();
                        let before = selected.len();
                        selected.retain(|&e| {
                            let he = core_math::half_edge::HalfEdgeId(e);
                            match result.edge_dihedral_cos(he) {
                                // Coplanar: ambiguous (knife vs boolean) -> keep, tags decide.
                                Some(c) if c >= 1.0 - 1e-6 => true,
                                // Inside a round -> never a corner -> skip.
                                Some(c) => c <= cos_t,
                                None => true,
                            }
                        });
                        if before != selected.len() {
                            eprintln!(
                                "[BEVEL] smart select: skipped {} of {before} smooth/flat edges",
                                before - selected.len()
                            );
                        }
                    }
                    // NEAR-RIGHT-ANGLE EDGES ALWAYS BEVEL (owner 2026-08-05: "80 to 100
                    // degree edges should always be bevel"). A near-square corner is a real
                    // structural edge whatever produced it, so it OVERRIDES every exclusion -
                    // geometry and tags alike. This is the safety net for the whole rule
                    // stack: no future rule can silently eat a corner the user can see.
                    //
                    // SCOPED TO THE CHANNEL. This iterated the WHOLE mesh, and the re-insert below
                    // is unconditional — so any edge the Geometry Channel had just filtered OUT
                    // came straight back if it happened to sit at 80-100 degrees. Ticking Smart
                    // Select on a polygroup- or weight-masked Bevel made it jump to the entire
                    // object's hard corners, i.e. Smart Select silently cancelled the user's
                    // explicit selection. "Overrides every exclusion" means the rules THIS feature
                    // infers (geometry, tags) — never the channel the user chose by hand.
                    // `in_channel` is the full live set when no channel is set, so the unmasked
                    // case is bit-identical.
                    let mut always: std::collections::HashSet<u32> =
                        std::collections::HashSet::new();
                    if m.bevel_smart_select {
                        for &e in in_channel.iter() {
                            let he = core_math::half_edge::HalfEdgeId(e);
                            if let Some(c) = result.edge_dihedral_cos(he) {
                                // 80..=100 degrees between face normals.
                                let deg = c.clamp(-1.0, 1.0).acos().to_degrees();
                                if (80.0..=100.0).contains(&deg) {
                                    always.insert(e);
                                }
                            }
                        }
                    }
                    if m.bevel_smart_select && !op_created.is_empty() {
                        // PERSISTED TAGS: edges a bevel or a boolean created, recorded when
                        // that operation ran. Covers the owner's real case, where both were
                        // applied destructively so nothing is left in the modifier stack to
                        // infer from.
                        let before = selected.len();
                        selected.retain(|&e| {
                            !op_created.contains(&Self::edge_pos_key(
                                result,
                                core_math::half_edge::HalfEdgeId(e),
                            ))
                        });
                        if before != selected.len() {
                            eprintln!(
                                "[BEVEL] smart select: skipped {} of {before} operation-created edges",
                                before - selected.len()
                            );
                        }
                    }
                    if m.bevel_smart_select && !always.is_empty() {
                        let before = selected.len();
                        for &e in &always {
                            selected.insert(e);
                        }
                        if before != selected.len() {
                            eprintln!(
                                "[BEVEL] smart select: {} near-90-degree edges force-kept",
                                selected.len() - before
                            );
                        }
                    }
                    if m.bevel_smart_select && !prov.is_empty() {
                        // RULE 2 ONLY. Skip edges whose BOTH faces came from the cutter --
                        // the vertical seams of a subtracted cylinder. The rim, with one
                        // face from each operand, is never matched: that is the edge the
                        // user wants beveled. Proven sound (BEVEL.md section 6 table:
                        // valid, manifold, folds=0). Rule 3 is NOT applied here - excluding
                        // coplanar edges produced folds; they need dissolving instead.
                        let before = selected.len();
                        selected.retain(|&e| {
                            !prov.edge_is_cutter_interior(
                                result,
                                core_math::half_edge::HalfEdgeId(e),
                            )
                        });
                        if before != selected.len() {
                            eprintln!(
                                "[BEVEL] smart select: skipped {} of {before} cutter edges",
                                before - selected.len()
                            );
                        }
                    }
                    // LIMIT METHOD: ANGLE (Blender's Bevel modifier). Independent of Smart
                    // Select — Blender's limit method is its own control, and this one works
                    // on a mesh with no provenance at all.
                    //
                    // Deliberately LAST. It is purely subtractive, so running last can never
                    // resurrect an edge an earlier rule removed — but running it EARLIER
                    // would let the near-90-degree force-keep above re-insert edges the
                    // user's angle limit had just excluded (set the limit above 100 and the
                    // force-keep would hand every square corner straight back). Last means
                    // the user's explicit angle is the final word.
                    if m.bevel_limit_angle {
                        let cos_limit = m.bevel_angle_deg.clamp(0.0, 180.0).to_radians().cos();
                        let before = selected.len();
                        selected.retain(|&e| {
                            let he = core_math::half_edge::HalfEdgeId(e);
                            match result.edge_dihedral_cos(he) {
                                // dot(n1,n2): 1.0 = coplanar (0 deg), 0.0 = square (90 deg).
                                // Keep when deviation >= limit, i.e. cos <= cos(limit).
                                Some(c) => c <= cos_limit,
                                // Boundary / non-manifold edge has no dihedral: no angle to
                                // test, so the angle limit cannot justify removing it.
                                None => true,
                            }
                        });
                        if before != selected.len() {
                            eprintln!(
                                "[BEVEL] angle limit {:.0} deg: skipped {} of {before} edges",
                                m.bevel_angle_deg,
                                before - selected.len()
                            );
                        }
                    }
                    // Fail closed: if the rule left nothing, bevel nothing rather than
                    // silently reverting to beveling every edge.
                    if !selected.is_empty() {
                        let params = core_math::bevel_op::BevelParams {
                            width: m.bevel_width,
                            segments: m.bevel_segments.max(1).min(32),
                            shape: m.bevel_shape.clamp(0.0, 1.0),
                            clamp: m.bevel_clamp,
                        };
                        // CONSUME GEOMETRY (bevel_absorb.rs). Preferred: FUSE — dissolve
                        // rings the fillet covers, CUT the stripe boundary at exactly
                        // `width` along each rail, loft the profile between the boundary
                        // loops (Maya / Offset-Edge-Loop-Cut placement; the boundaries ARE
                        // the rims). Fallback chain, fail closed: fuse out of scope →
                        // v2 absorb + offset bevel → plain clamped bevel. Working copy
                        // only — the base mesh is untouched.
                        let mut fused = false;
                        if m.bevel_consume {
                            if let Some(f) = core_math::bevel_absorb::consume_fuse_bevel(
                                result, &selected, &params,
                            ) {
                                eprintln!("[BEVEL] consume: fused stripe at width {}", m.bevel_width);
                                *result = f;
                                fused = true;
                            } else if let Some(cut) = core_math::bevel_absorb::consume_wedge_cut(
                                result,
                                &selected,
                                &params,
                            ) {
                                // Boolean-rim topology: wedge-sweep subtract. The sweep's
                                // cross-section is the consume profile, so Segments/Shape
                                // round this path too (2026-08-09).
                                eprintln!("[BEVEL] consume: wedge-cut strip at width {}", m.bevel_width);
                                *result = cut;
                                fused = true;
                            } else if let Some(absorbed) =
                                core_math::bevel_absorb::absorb_for_bevel(
                                    result,
                                    &selected,
                                    m.bevel_width,
                                )
                            {
                                *result = absorbed;
                                // Kernel ops are ID-preserving, but drop anything that died.
                                selected.retain(|&e| {
                                    (e as usize) < result.half_edges.len()
                                        && result
                                            .is_edge_live(core_math::half_edge::HalfEdgeId(e))
                                });
                                eprintln!(
                                    "[BEVEL] consume: absorbed blocking loops for width {}",
                                    m.bevel_width
                                );
                            }
                        }
                        if !fused && !selected.is_empty() {
                            // VARIABLE WIDTH. Built here rather than inside the kernel because the
                            // AuthoredWeight driver needs the body's sidecar, which only the app
                            // layer has (`auth` is already threaded in for `geo_channel`).
                            //
                            // Deliberately skipped when Consume Geometry took the mesh (`fused`):
                            // absorb decides which rings to dissolve from ONE width, so a per-edge
                            // width would make that decision disagree with the geometry it then
                            // produces. The UI makes the two toggles mutually exclusive; this is the
                            // matching fail-closed guard, not a second policy.
                            let var = bevel_var_width_spec(m, result, &selected, auth);
                            let beveled = core_math::bevel_op::bevel_edges_offset_var(
                                result, &selected, &params, var.as_ref(),
                            );
                            if core_math::bevel_op::bevel_result_installable(result, &beveled) {
                                *result = beveled;
                            }
                        }
                    }
                }
            }
            PolyModifierType::Decimate => {
                // Planar / limited dissolve at `decimate_angle` (Blender Limited Dissolve).
                // Protect none — the modifier owns the whole mesh. Tagged-edge protection is
                // a mesh-cleanup concern (ops path), not this stack entry.
                let _ = result.limited_dissolve(m.decimate_angle, None, None);
            }
            PolyModifierType::Boolean => {
                let Some(cutter) = cutters.get(&m.bool_cutter_nid) else {
                    return;
                };
                let op = match m.bool_op {
                    1 => BoolOp::Union,
                    2 => BoolOp::Intersect,
                    _ => BoolOp::Difference,
                };
                if let Some((out, p)) =
                    core_math::mesh_boolean::mesh_boolean_with_provenance(result, cutter, op)
                {
                    if out.validate().is_ok() {
                        *result = out;
                        *prov = p; // a following Bevel can now skip the cutter exactly
                    }
                }
            }
            PolyModifierType::WeightedNormal => {
                result.recompute_normals_weighted_ex(
                    m.weighted_keep_sharp_deg,
                    m.weighted_mode,
                    m.weighted_weight,
                    m.weighted_threshold,
                );
            }
            PolyModifierType::SmoothNormal => {
                // Settings reuse weighted_* + smooth_factor as Strength (0..1 amount).
                // Legacy unconfigured stacks (shared Default knobs) → full smooth.
                let legacy = (m.weighted_keep_sharp_deg - 30.0).abs() < 1e-3
                    && (m.smooth_factor - 0.5).abs() < 1e-3
                    && m.weighted_weight == 50
                    && m.weighted_mode == 2
                    && (m.weighted_threshold - 0.01).abs() < 1e-4;
                let angle = if legacy { 180.0 } else { m.weighted_keep_sharp_deg };
                let strength = if legacy { 1.0 } else { m.smooth_factor };
                result.recompute_normals_smooth_ex(
                    angle,
                    m.weighted_mode,
                    m.weighted_weight,
                    m.weighted_threshold,
                    strength,
                );
            }
            PolyModifierType::Inset => {
                // Non-destructive inset: faces matching geometry channel (All = every live face).
                //
                // REGION mode needs a boundary. Selecting every face of a closed solid has
                // none (every edge is interior to the set) → silent no-op. That made the
                // default Add→Inset look dead (diag_modifier_audit). Fall back to Individual
                // when Region has zero boundary edges so All-faces always does something.
                if m.inset_thickness.abs() > 1e-8 {
                    use core_math::half_edge::{FaceId, NO_EDGE};
                    use std::collections::HashSet;
                    let mut faces: Vec<u32> = (0..result.faces.len() as u32)
                        .filter(|&i| result.face(FaceId(i)).half_edge != NO_EDGE)
                        .collect();
                    faces = m.geo_channel.filter_face_ids(result, &faces);
                    if !faces.is_empty() {
                        let mut individual = m.inset_individual;
                        if !individual && faces.len() > 1 {
                            let set: HashSet<FaceId> =
                                faces.iter().map(|&f| FaceId(f)).collect();
                            if result.inset_region_offset_dirs(&set).is_empty() {
                                individual = true;
                            }
                        }
                        let _ = crate::poly::inset::apply_inset(
                            result,
                            &faces,
                            m.inset_thickness,
                            m.inset_depth,
                            individual,
                        );
                    }
                }
            }
            PolyModifierType::VertexMerge => {
                // Blender Merge by Distance: weld verts within threshold to their average.
                let thr = m.merge_distance.max(0.0);
                if thr > 0.0 {
                    let _removed = result.merge_by_distance(thr);
                }
            }
            PolyModifierType::MeshCleanup => {
                // Mesh Cleanup kernel (`mesh_boolean::cleanup_mesh_with`): folds, coplanar
                // merge, optional limited dissolve, T-junction repair. No-op when already clean.
                // Protected edges: reverse of `edge_pos_key` quantisation (Q=1e5).
                const Q: f32 = 1e5;
                let deq = |x: i64, y: i64, z: i64| {
                    glam::Vec3::new(x as f32 / Q, y as f32 / Q, z as f32 / Q)
                };
                let opts = core_math::mesh_boolean::CleanupOptions {
                    flatten_folds: m.cleanup_flatten_folds,
                    merge_coplanar: m.cleanup_merge_coplanar,
                    dissolve_angle_deg: m.cleanup_dissolve_angle.max(0.0),
                    protected_edges: op_created
                        .iter()
                        .map(|&(ax, ay, az, bx, by, bz)| {
                            (deq(ax, ay, az), deq(bx, by, bz))
                        })
                        .collect(),
                    // The MODIFIER keeps its original three knobs. Exposing the new Clean Up
                    // passes here would mean new SERIALIZED `PolyModifier` fields, which is a
                    // save-format change (CLAUDE.md) and belongs in its own task — not smuggled
                    // in behind a defaulted struct literal. Stack behaviour is unchanged.
                    ..Default::default()
                };
                if let Some(cleaned) =
                    core_math::mesh_boolean::cleanup_mesh_with(result, &opts)
                {
                    *result = cleaned;
                }
            }
            PolyModifierType::Remesh => {
                // Poly → voxel SDF → Surface Nets (sculpt remesh backend). Fail closed:
                // keep the previous stack mesh if voxelize/extract yields nothing.
                let mut remesh_cache = None;
                if let Some(out) = crate::poly::remesh::voxel_remesh_mesh_incremental_ex(
                    result,
                    m.remesh_resolution,
                    m.remesh_smooth,
                    m.remesh_padding,
                    m.remesh_project,
                    m.remesh_union_islands,
                    m.remesh_collapse,
                    0,
                    0,
                    false,
                    false,
                    false,
                    &mut remesh_cache,
                ) {
                    *result = out;
                }
            }
            PolyModifierType::Sculpt => {
                apply_polysculpt_deltas(result, &m.sculpt_deltas);
            }
            PolyModifierType::Bend => {
                let (along, base_up, base_side) = bend_frame(m.bend_axis);
                // The heading spins the curl plane about the bend axis; at 0 deg it curls toward the
                // next axis in the cyclic order, which is what the UI's per-axis label promises.
                let (s, c) = m.bend_dir_deg.to_radians().sin_cos();
                let up = base_up * c + base_side * s;
                result.bend_deform(
                    along,
                    up,
                    m.bend_center,
                    m.bend_angle_deg.to_radians(),
                    m.bend_limit_lo,
                    m.bend_limit_hi,
                );
            }
        }
    }
}

/// Add stored PolySculpt deltas onto live verts. Missing / free ids are skipped (rebase orphans).
fn apply_polysculpt_deltas(
    mesh: &mut core_math::half_edge::HalfEdgeMesh,
    deltas: &[(u32, glam::Vec3)],
) {
    use core_math::half_edge::VertexId;
    for &(i, d) in deltas {
        let vid = VertexId(i);
        if mesh.is_vert_live(vid) {
            mesh.vert_mut(vid).position += d;
        }
    }
}

/// The bend axis' orthonormal frame: `(along, up_at_0_deg, up_at_90_deg)`, cyclic X→Y→Z so the
/// triple is always right-handed and the heading turns the same way on every axis.
///
/// Shared by the apply arm and the properties panel — the panel labels the heading with the axis
/// names this returns, so a second copy of the cycle would let the label drift off the deform.
pub(crate) fn bend_frame(axis: u8) -> (glam::Vec3, glam::Vec3, glam::Vec3) {
    match axis {
        0 => (glam::Vec3::X, glam::Vec3::Y, glam::Vec3::Z),
        1 => (glam::Vec3::Y, glam::Vec3::Z, glam::Vec3::X),
        _ => (glam::Vec3::Z, glam::Vec3::X, glam::Vec3::Y),
    }
}

/// Every live half-edge id on `mesh` (including twins). `bevel_edges_offset` canonicalizes to
/// undirected primary edges and skips boundary / slit / free slots.
fn all_live_half_edge_ids(mesh: &core_math::half_edge::HalfEdgeMesh) -> std::collections::HashSet<u32> {
    use core_math::half_edge::{HalfEdgeId, NO_FACE};
    let mut out = std::collections::HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let he = mesh.he(HalfEdgeId(i));
        if he.face == NO_FACE {
            continue;
        }
        out.insert(i);
    }
    out
}

/// Translate a Bevel modifier's PER-EDGE WIDTH settings into the kernel's policy. `None` = one width
/// AND one global limit, which is the kernel's untouched original path.
///
/// Two independent toggles feed this:
///  * `bevel_auto_merge` alone → [`core_math::bevel_op::BevelWidthDriver::Uniform`]: keep the single
///    width, but limit each edge by its own capacity so a tight pair pins at its meet point (where
///    the kernel's `WeldMap` merges the two rails) instead of throttling the whole mesh.
///  * `bevel_var_width` (1 = auto by local size, 2 = authored edge weight) → a graded driver. Grading
///    REQUIRES the per-edge ceiling — under one global minimum every edge would be capped to the
///    smallest capacity and the grading would vanish — so Variable Width implies auto-merge, and the
///    UI shows it that way rather than leaving the state invisible.
///
/// An unknown `bevel_var_width` (a file from a newer build) falls back to auto rather than off: the
/// user did enable grading, and auto is the driver that needs no data we might be missing.
pub(crate) fn bevel_var_width_spec(
    m: &PolyModifier,
    mesh: &HalfEdgeMesh,
    selected: &std::collections::HashSet<u32>,
    auth: &core_math::geometry_channel::AuthoredChannels,
) -> Option<core_math::bevel_op::BevelWidthPolicy> {
    use core_math::bevel_op::{BevelWidthDriver, BevelWidthPolicy};
    // Auto-merge ALONE does nothing without Clamp Overlap — its entire effect is "apply the clamp
    // limit per edge", and there is no limit to apply. Return `None` rather than a no-op policy so
    // the result is bit-identical to the untouched path: a `Uniform` policy would still route the
    // widths through the per-edge least-squares, which agrees mathematically but not in the last f32
    // bits (the reason the uniform branch keeps its original expression verbatim).
    // AUTHORED EDGE WEIGHT IS ALWAYS LIVE — it is not a driver and needs no mode.
    //
    // Owner 2026-08-11: "the edge weight feature should only have one weight slider, not part of
    // variable [width]; the weight is a percentage of the width." It used to be reachable only by
    // ticking Variable Width and then picking "Authored weight" from a combo inside it, which is
    // why painting weights appeared to do nothing at all — the default driver is 0, so the column
    // was simply never read. Resolve the weights first, unconditionally, and let their presence
    // alone turn the per-edge path on.
    let mut weights = std::collections::HashMap::new();
    for &e in selected {
        let he = core_math::half_edge::HalfEdgeId(e);
        // `weight_of_edge` returns 1.0 for an edge with nothing authored, which is also the
        // kernel's default — only edges the user actually painted need an entry.
        if let Some(w) = auth.authored_weight_of_edge(mesh, he) {
            weights.insert(e, w);
            let twin = mesh.he(he).twin;
            if twin != core_math::half_edge::NO_TWIN {
                weights.insert(twin.0, w);
            }
        }
    }

    let merge_only = m.bevel_var_width == 0;
    // No authored weights AND no other reason for a per-edge path → return `None` so the result is
    // bit-identical to the untouched kernel path (see the note below on why that matters).
    if weights.is_empty() && merge_only && (!m.bevel_auto_merge || !m.bevel_clamp) {
        return None;
    }
    let driver = match m.bevel_var_width {
        0 => BevelWidthDriver::Uniform, // auto-merge only: one width, per-edge limit
        2 => BevelWidthDriver::AuthoredWeight,
        // 3 = grade by CORNER SPAN, not by the segment's own clamp capacity (owner 2026-08-11: a
        // subdivided panel is not "small detail", but per-segment capacity says it is).
        3 => BevelWidthDriver::CornerSpan,
        _ => BevelWidthDriver::AutoSize,
    };
    Some(BevelWidthPolicy {
        width_min: m.bevel_width_min.max(0.0),
        driver,
        // Only Variable Width / Auto Merge trade the global clamp limit for per-edge ceilings —
        // that swap IS what those options do. A policy that exists purely to apply authored
        // weights must not make it: the weight would then come out WIDER than the unweighted
        // bevel wherever the global limit was binding, and would read as having turned some
        // other option on.
        per_edge_limit: !merge_only || m.bevel_auto_merge,
        weights,
    })
}

/// Validate mesh topology in debug builds.
#[allow(dead_code)] // FUTURE: called ad-hoc during poly debugging
#[cfg(debug_assertions)]
pub(crate) fn debug_validate_mesh(mesh: &core_math::half_edge::HalfEdgeMesh, op_name: &str) {
    if let Err(e) = mesh.validate() {
        eprintln!("[MESH BUG] Topology corrupt after '{}': {}", op_name, e);
    }
}

#[allow(dead_code)]
#[cfg(not(debug_assertions))]
pub(crate) fn debug_validate_mesh(_mesh: &core_math::half_edge::HalfEdgeMesh, _op_name: &str) {}

#[cfg(test)]
mod tests {
    use super::EXTRUDE_TAG_TANGENT_DEG;
    use core_math::half_edge::{HalfEdgeMesh, FaceId, NO_EDGE};
    use core_math::scene_graph::{PolyModifier, PolyModifierType};
    use glam::Vec3;
    use crate::AzraApp;

    fn cube() -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(-1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,1.0),
            Vec3::new(-1.0,-1.0,1.0), Vec3::new(-1.0,1.0,-1.0), Vec3::new(1.0,1.0,-1.0),
            Vec3::new(1.0,1.0,1.0), Vec3::new(-1.0,1.0,1.0),
        ];
        let f: Vec<Vec<usize>> = vec![
            vec![0,1,2,3], vec![7,6,5,4], vec![4,5,1,0],
            vec![6,7,3,2], vec![7,4,0,3], vec![5,6,2,1],
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    fn live_faces(m: &HalfEdgeMesh) -> usize {
        (0..m.faces.len() as u32).map(FaceId).filter(|&f| m.face(f).half_edge != NO_EDGE).count()
    }

    /// Box whose top face is split by one diagonal into two coplanar triangles — the minimum
    /// fixture where Mesh Cleanup has real work to do (`merge_coplanar` wants that diagonal).
    fn box_with_split_top() -> (HalfEdgeMesh, (Vec3, Vec3), (Vec3, Vec3)) {
        let h = 1.0f32;
        let p = vec![
            Vec3::new(-h,-h,-h), Vec3::new(h,-h,-h), Vec3::new(h,h,-h), Vec3::new(-h,h,-h),
            Vec3::new(-h,-h, h), Vec3::new(h,-h, h), Vec3::new(h,h, h), Vec3::new(-h,h, h),
        ];
        let f: Vec<Vec<usize>> = vec![
            vec![0,3,2,1], vec![4,5,6], vec![4,6,7],
            vec![0,1,5,4], vec![1,2,6,5], vec![2,3,7,6], vec![3,0,4,7],
        ];
        // (the mergeable diagonal, a genuine 90° box edge that is never a merge candidate)
        (HalfEdgeMesh::from_polygons(&p, &f), (p[4], p[6]), (p[0], p[1]))
    }

    fn app_with(mesh: HalfEdgeMesh) -> (AzraApp, core_math::scene_graph::NodeId) {
        use core_math::scene_graph::NodePayload;
        let mut app = AzraApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Probe".into(),
                mesh: std::sync::Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                // Hand-built fixture mesh: authored, not derived from a PrimitiveSpec.
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        (app, nid)
    }

    /// Tag the edge between `a` and `b` (either direction) with `op_created`.
    fn tag_edge(app: &mut AzraApp, nid: core_math::scene_graph::NodeId, a: Vec3, b: Vec3) {
        use core_math::half_edge::HalfEdgeId;
        let m = app.poly_node_mesh(nid).expect("poly body");
        let mut cd = app.cache.poly_custom_data.remove(&nid).unwrap_or_default();
        let hit = (0..m.half_edges.len() as u32).map(HalfEdgeId).find(|&h| {
            m.is_edge_live(h) && {
                let (s, d) = (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
                ((s - a).length() < 1e-5 && (d - b).length() < 1e-5)
                    || ((s - b).length() < 1e-5 && (d - a).length() < 1e-5)
            }
        }).expect("edge exists on the probe mesh");
        cd.op_created.set(hit, true);
        app.cache.poly_custom_data.insert(nid, cd);
    }

    /// Is the edge between these two positions still carrying `op_created` on the body?
    /// `None` = the edge no longer exists at all (a different failure from "lost its tag").
    fn edge_tagged(
        app: &AzraApp,
        nid: core_math::scene_graph::NodeId,
        a: Vec3,
        b: Vec3,
    ) -> Option<bool> {
        use core_math::half_edge::HalfEdgeId;
        let m = app.poly_node_mesh(nid)?;
        let cd = app.cache.poly_custom_data.get(&nid);
        let mut found = false;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) {
                continue;
            }
            let (s, d) = (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
            let hit = ((s - a).length() < 1e-5 && (d - b).length() < 1e-5)
                || ((s - b).length() < 1e-5 && (d - a).length() < 1e-5);
            if !hit {
                continue;
            }
            found = true;
            // Either half-edge carrying the bit is enough — Smart Select keys edges undirected.
            if cd.is_some_and(|c| c.op_created.get(h)) {
                return Some(true);
            }
        }
        found.then_some(false)
    }

    /// Does an edge between these two positions still exist on the body?
    fn has_edge(app: &AzraApp, nid: core_math::scene_graph::NodeId, a: Vec3, b: Vec3) -> bool {
        use core_math::half_edge::HalfEdgeId;
        let Some(m) = app.poly_node_mesh(nid) else { return false };
        (0..m.half_edges.len() as u32).map(HalfEdgeId).any(|h| {
            m.is_edge_live(h) && {
                let (s, d) = (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
                ((s - a).length() < 1e-5 && (d - b).length() < 1e-5)
                    || ((s - b).length() < 1e-5 && (d - a).length() < 1e-5)
            }
        })
    }

    fn tag_count(app: &AzraApp, nid: core_math::scene_graph::NodeId) -> usize {
        use core_math::half_edge::HalfEdgeId;
        let Some(cd) = app.cache.poly_custom_data.get(&nid) else { return 0 };
        let Some(m) = app.poly_node_mesh(nid) else { return 0 };
        (0..m.half_edges.len() as u32).map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && cd.op_created.get(h))
            .count()
    }



    /// Exhaustive list of every [`PolyModifierType`]. The `match` has **no** `_` arm — adding a
    /// variant without updating this function fails at compile time (Gate 3).
    fn all_poly_modifier_types() -> Vec<PolyModifierType> {
        use PolyModifierType::*;
        // Force exhaustiveness: if a new variant is added, this match fails to compile.
        fn assert_exhaustive(t: PolyModifierType) {
            match t {
                Subdivide | Mirror | Solidify | Smooth | Triangulate | Decimate | Array | Bevel
                | Boolean | WeightedNormal | SmoothNormal | Duplicate | ArrayCircular | Pipe
                | Scatter | Inset | VertexMerge | MeshCleanup | Remesh | Bend | Sculpt => {}
            }
        }
        let _ = assert_exhaustive as fn(PolyModifierType);
        vec![
            Subdivide,
            Mirror,
            Solidify,
            Smooth,
            Triangulate,
            Decimate,
            Array,
            Bevel,
            Boolean,
            WeightedNormal,
            SmoothNormal,
            Duplicate,
            ArrayCircular,
            Pipe,
            Scatter,
            Inset,
            VertexMerge,
            MeshCleanup,
            Remesh,
            Bend,
            Sculpt,
        ]
    }

    /// Bake display the way the app does for a single-mod stack: post-modifier mesh +
    /// `poly_normal_state`-equivalent corner config (Weighted/Smooth feed `custom_corner`).
    fn bake_with_mod(mesh: &HalfEdgeMesh, m: &PolyModifier) -> (Vec<[f32; 8]>, Vec<u32>) {
        use core_math::half_edge::{smooth_corner_normals_ex, weighted_corner_normals};
        use core_math::poly_mesh::NormalConfig;
        let custom = match m.mod_type {
            PolyModifierType::WeightedNormal if m.enabled => Some(weighted_corner_normals(
                mesh,
                m.weighted_keep_sharp_deg,
                m.weighted_mode,
                m.weighted_weight,
                m.weighted_threshold,
                None,
            )),
            PolyModifierType::SmoothNormal if m.enabled => {
                let legacy = (m.weighted_keep_sharp_deg - 30.0).abs() < 1e-3
                    && (m.smooth_factor - 0.5).abs() < 1e-3
                    && m.weighted_weight == 50
                    && m.weighted_mode == 2
                    && (m.weighted_threshold - 0.01).abs() < 1e-4;
                let angle = if legacy { 180.0 } else { m.weighted_keep_sharp_deg };
                let strength = if legacy { 1.0 } else { m.smooth_factor };
                Some(smooth_corner_normals_ex(
                    mesh,
                    angle,
                    m.weighted_mode,
                    m.weighted_weight,
                    m.weighted_threshold,
                    strength,
                    None,
                ))
            }
            _ => None,
        };
        let cfg = NormalConfig {
            angle_deg: core_math::poly_mesh::AUTO_SMOOTH_ANGLE_DEG,
            sharp: None,
            custom_corner: custom.as_deref(),
        };
        let (v, i, _) = core_math::poly_mesh::raster_data_ngon_with_normals(mesh, &cfg);
        (v, i)
    }

    /// MODIFIER AUDIT (owner 2026-08-05). Every type is applied alone; mesh + bake must change
    /// unless the variant is on the deliberate allow-list (needs external data / unsuitable base).
    #[test]
    fn diag_modifier_audit() {
        use PolyModifierType as T;
        // Cube is the default probe. Decimate is planar dissolve — a cube has no coplanar
        // neighbours, so the Decimate arm is probed on a subdivided plane instead.
        let cube_base = cube();
        let plane_base = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        // Deliberate no-ops at default params (need external data or a dirty mesh). Documented.
        let allow_noop_defaults: &[T] = &[
            T::Boolean,     // needs bool_cutter_nid
            T::Scatter,     // needs scatter_source_nid
            T::Pipe,        // needs non-empty pipe_path
            T::VertexMerge, // default merge_distance (0.001) does not weld a unit cube
            T::MeshCleanup, // clean cube has nothing to fold/dissolve
            T::Sculpt,      // empty stored-delta list is a no-op until a stroke writes one
        ];
        let mut dead: Vec<T> = Vec::new();
        for t in all_poly_modifier_types() {
            let base = if t == T::Decimate {
                &plane_base
            } else {
                &cube_base
            };
            let cfg = core_math::poly_mesh::NormalConfig::default();
            let (bv, bi, _) = core_math::poly_mesh::raster_data_ngon_with_normals(base, &cfg);
            let m = PolyModifier::new(t);
            let out = AzraApp::apply_poly_modifiers(base, &[m.clone()], &Default::default());
            let (ov, oi) = bake_with_mod(&out, &m);
            let mesh_changed = out.vertex_count() != base.vertex_count()
                || out.face_count() != base.face_count();
            // Smooth Laplacian may keep counts but move verts — detect via positions.
            let pos_changed = out.vertices.len() != base.vertices.len()
                || out.vertices.iter().zip(base.vertices.iter()).any(|(a, b)| {
                    (a.position - b.position).length_squared() > 1e-12
                });
            let mesh_or_pos = mesh_changed || pos_changed;
            let pixels_changed = ov != bv || oi != bi;
            eprintln!(
                "{:>14?}: mesh {}->{}v {}->{}f {:>7} | baked {}->{}v {}->{}tri {:>7}",
                t,
                base.vertex_count(),
                out.vertex_count(),
                base.face_count(),
                out.face_count(),
                if mesh_or_pos { "CHANGED" } else { "same" },
                bv.len(),
                ov.len(),
                bi.len() / 3,
                oi.len() / 3,
                if pixels_changed { "CHANGED" } else { "SAME" },
            );
            if !mesh_or_pos && !pixels_changed && !allow_noop_defaults.contains(&t) {
                dead.push(t);
            }
        }
        assert!(
            dead.is_empty(),
            "modifier(s) change neither mesh nor bake (dead): {dead:?}. \
             Either implement them or add to allow_noop_defaults with a comment."
        );
    }

    /// Inset modifier on a closed cube must change topology (default All-faces + Region
    /// would otherwise be a silent no-op — no region boundary).
    #[test]
    fn inset_modifier_on_cube_changes_topology() {
        let base = cube();
        let m = PolyModifier::new(PolyModifierType::Inset);
        assert!(
            (m.inset_thickness).abs() > 1e-8,
            "default thickness must be non-zero"
        );
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &Default::default());
        assert!(
            out.vertex_count() > base.vertex_count() || out.face_count() > base.face_count(),
            "Inset modifier must add inset ring verts/faces on a cube \
             (got {}v/{}f from {}v/{}f)",
            out.vertex_count(),
            out.face_count(),
            base.vertex_count(),
            base.face_count()
        );
        assert!(out.validate().is_ok(), "inset stack result must be manifold-valid");
    }

    /// Gate 0: changing Weighted Normal fields must change the **baked normals**, not just the
    /// stack hash (`every_weighted_normal_field_changes_the_stack_hash` only guards the key).
    #[test]
    fn weighted_normal_params_change_baked_normals() {
        // Curved-ish mesh so mode/weight/keep_sharp actually diverge (a cube is too symmetric).
        let base = HalfEdgeMesh::new_uv_sphere(1.0, 8, 6);
        let mut a = PolyModifier::new(PolyModifierType::WeightedNormal);
        a.weighted_mode = 0;
        a.weighted_weight = 50;
        a.weighted_threshold = 0.0;
        a.weighted_keep_sharp_deg = 180.0;
        let mut b = a.clone();
        b.weighted_mode = 1;
        let mut c = a.clone();
        c.weighted_weight = 100;
        let mut d = a.clone();
        d.weighted_keep_sharp_deg = 30.0;
        // Threshold only affects the keep-sharp *reference* when keep_sharp < 180.
        // Cube corner: three equal faces — threshold blends them (same as normals unit test).
        let corner = {
            let pos = vec![
                glam::Vec3::ZERO,
                glam::Vec3::X,
                glam::Vec3::Y,
                glam::Vec3::Z,
            ];
            let faces = vec![vec![0, 1, 2], vec![0, 2, 3], vec![0, 3, 1]];
            HalfEdgeMesh::from_polygons(&pos, &faces)
        };
        let mut e0 = PolyModifier::new(PolyModifierType::WeightedNormal);
        e0.weighted_mode = 2;
        e0.weighted_weight = 50;
        e0.weighted_keep_sharp_deg = 60.0;
        e0.weighted_threshold = 0.0;
        let mut e1 = e0.clone();
        e1.weighted_threshold = 0.01;

        let bake = |mesh: &HalfEdgeMesh, m: &PolyModifier| {
            let out = AzraApp::apply_poly_modifiers(mesh, std::slice::from_ref(m), &Default::default());
            bake_with_mod(&out, m).0
        };
        let na = bake(&base, &a);
        assert_ne!(na, bake(&base, &b), "weighted_mode must change baked normals");
        assert_ne!(na, bake(&base, &c), "weighted_weight must change baked normals");
        assert_ne!(na, bake(&base, &d), "weighted_keep_sharp_deg must change baked normals");
        assert_ne!(
            bake(&corner, &e0),
            bake(&corner, &e1),
            "weighted_threshold must change baked normals"
        );
    }

    /// Gate 0 regression: body with **no** normal modifier bakes byte-identically to the
    /// pre-custom-corner path (default Auto Smooth only).
    #[test]
    fn no_normal_modifier_bake_is_byte_identical() {
        let base = cube();
        let cfg = core_math::poly_mesh::NormalConfig::default();
        let (v0, i0, r0) = core_math::poly_mesh::raster_data_ngon_with_normals(&base, &cfg);
        // Mirror stack changes topology — but with empty stack / Triangulate still default path.
        let out = AzraApp::apply_poly_modifiers(&base, &[], &Default::default());
        let (v1, i1, r1) = core_math::poly_mesh::raster_data_ngon_with_normals(&out, &cfg);
        assert_eq!(v0, v1);
        assert_eq!(i0, i1);
        assert_eq!(r0, r1);
        // Subdivide then bake without custom corners still uses default NormalConfig.
        let mut sub = PolyModifier::new(PolyModifierType::Subdivide);
        sub.subdiv_levels = 1;
        let sub_out = AzraApp::apply_poly_modifiers(&base, &[sub], &Default::default());
        let (vs, is, _) = core_math::poly_mesh::raster_data_ngon_with_normals(&sub_out, &cfg);
        let (vs2, is2) = bake_with_mod(&sub_out, &PolyModifier::new(PolyModifierType::Array)); // no custom
        // bake_with_mod without Weighted/Smooth uses default-equivalent corners.
        assert_eq!(vs, vs2);
        assert_eq!(is, is2);
    }

    /// EXPORT == DISPLAY (owner 2026-08-05). Both exporters used to call `to_raster_data()` on
    /// the BASE mesh, losing authored normals AND the whole modifier stack. These assert the two
    /// halves of that: a Mark Sharp edge must reach the exporter, and a modifier must too.
    #[test]
    fn export_raster_matches_the_displayed_normals() {
        use core_math::half_edge::HalfEdgeId;
        let (mut app, nid) = app_with(cube());
        // Smooth everything, so any later split can only come from the authored mark.
        app.cache.poly_custom_data.entry(nid).or_default().auto_smooth_deg = Some(180.0);
        // The n-gon emitter always writes one vertex PER FACE CORNER, so sharpness changes the
        // normal VECTORS, never the count — compare the normals, not the vertex total.
        let normals = |a: &mut AzraApp| -> Vec<[f32; 3]> {
            a.poly_export_raster(nid)
                .expect("export raster")
                .0
                .iter()
                .map(|v| [v[3], v[4], v[5]])
                .collect()
        };
        let smooth = normals(&mut app);

        // Ring one WHOLE FACE, not a single edge. A cube vertex's smooth fan is a CLOSED ring
        // of three faces, so marking one of its edges leaves the other two still connecting
        // everything and nothing splits — Blender behaves the same way. Isolating a face is the
        // smallest mark that must visibly change its corner normals.
        let m = app.poly_node_mesh(nid).expect("body");
        let face_edges: Vec<HalfEdgeId> = m.face_half_edges(FaceId(0));
        {
            let cd = app.cache.poly_custom_data.entry(nid).or_default();
            for &h in &face_edges {
                cd.sharp.set(h, true);
                let tw = m.he(h).twin;
                if tw != core_math::half_edge::NO_TWIN {
                    cd.sharp.set(tw, true);
                }
            }
        }
        let marked = normals(&mut app);
        assert_ne!(
            smooth, marked,
            "Mark Sharp did not reach the exporter - it is still emitting base-mesh normals"
        );

        // And it must equal the DISPLAY bake exactly, not merely differ from the smooth case.
        let display = app.poly_cached_display_mesh(nid).expect("display mesh");
        let nrm = app.poly_normal_state(nid, &display);
        let (dv, di, _) = core_math::poly_mesh::raster_data_ngon_with_normals(&display, &nrm.cfg());
        let (ev, ei) = app.poly_export_raster(nid).expect("export raster");
        assert_eq!(ev, dv, "export vertices differ from the displayed ones");
        assert_eq!(ei, di, "export indices differ from the displayed ones");
    }

    /// The modifier stack must reach the exporter too - the half of the bug that was not reported.
    #[test]
    fn export_raster_includes_the_modifier_stack() {
        let (mut app, nid) = app_with(cube());
        let plain = app.poly_export_raster(nid).expect("export raster").1.len();
        if let Some(node) = app.scene.scene_graph.get_mut(nid) {
            if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                &mut node.payload
            {
                let mut m = PolyModifier::new(PolyModifierType::Mirror);
                m.mirror_axis = 1; // BITMASK, not an index: bit0 = X. 0 would mirror nothing.
                modifiers.push(m);
            }
        }
        // Any real stack edit goes through a mutation choke point; this test edits the graph
        // directly, so it must bump the same counter or the memoised display key stays stale.
        app.bump_poly_edit_generation();
        let mirrored = app.poly_export_raster(nid).expect("export raster").1.len();
        assert!(
            mirrored > plain,
            "a Mirror modifier must be in the export: {plain} -> {mirrored} indices (unchanged =              the exporter is still reading the unmodified base mesh)"
        );
    }

    /// THE OWNER BUG (2026-08-11): "mesh clean up does not do a good job cleaning the mesh."
    ///
    /// Verified through the APP path, not the kernel: `poly_mesh_cleanup_now` → gates →
    /// transaction → installed mesh → the report the UI shows. The kernel's closed-result gate
    /// made cleanup a no-op on ANY body with a rim, and the user-visible symptom was the popup
    /// saying "Mesh is already clean — nothing to repair" over a mesh that plainly was not.
    #[test]
    fn mesh_cleanup_repairs_an_open_body_instead_of_calling_it_clean() {
        // Two coplanar quads sharing an edge: OPEN, and one n-gon would do.
        let p = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let mesh = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 4, 5], vec![1, 2, 3, 4]]);
        let (mut app, nid) = app_with(mesh);
        app.poly_mesh_cleanup_now(nid);

        let rep = app.ui.mesh_cleanup_result.clone().expect("cleanup must report a run");
        assert!(rep.applied, "cleanup called a repairable open sheet 'already clean'");
        let m = app.poly_node_mesh(nid).expect("poly body");
        assert_eq!(live_faces(&m), 1, "the two coplanar quads must merge into one n-gon");
    }

    /// The `Mesh > Clean Up > Fill Holes` command, through its real op entry point.
    #[test]
    fn cleanup_fill_holes_command_caps_an_open_box() {
        let mut mesh = cube();
        mesh.delete_face(FaceId(0));
        assert_eq!(live_faces(&mesh), 5);
        let (mut app, nid) = app_with(mesh);
        crate::ops::poly::cleanup_passes::fill_holes(&mut app);
        let m = app.poly_node_mesh(nid).expect("poly body");
        assert_eq!(live_faces(&m), 6, "the 4-sided hole was not capped");
        assert_eq!(
            core_math::mesh_boolean::boundary_half_edge_count(&m),
            0,
            "body still open after Fill Holes"
        );
    }

    /// The `Mesh > Clean Up > Delete Loose` command must not touch a healthy body — a cleanup
    /// command that "succeeds" by eating good geometry is worse than one that refuses.
    #[test]
    fn cleanup_delete_loose_command_leaves_a_healthy_cube_alone() {
        let (mut app, nid) = app_with(cube());
        let before = app.poly_node_mesh(nid).expect("poly body").content_signature();
        crate::ops::poly::cleanup_passes::delete_loose(&mut app);
        let after = app.poly_node_mesh(nid).expect("poly body").content_signature();
        assert_eq!(before, after, "Delete Loose modified a clean cube");
    }

    /// Blender Shift+N through the app path: AZRA's Recalculate Normals only ever recomputed
    /// normal VECTORS from the existing winding, so a reversed face stayed reversed.
    #[test]
    fn recalculate_normals_rewinds_a_reversed_face() {
        let p = vec![
            Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),   Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),  Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),    Vec3::new(-1.0, 1.0, 1.0),
        ];
        let mut f: Vec<Vec<usize>> = vec![
            vec![0, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4],
            vec![1, 2, 6, 5], vec![2, 3, 7, 6], vec![3, 0, 4, 7],
        ];
        f[2].reverse(); // one face wound inward
        let mesh = HalfEdgeMesh::from_polygons(&p, &f);
        assert!(
            core_math::mesh_boolean::boundary_half_edge_count(&mesh) > 0,
            "fixture must actually be mis-wound (a reversed face cannot twin)"
        );
        let (mut app, nid) = app_with(mesh);
        crate::ops::poly::recalculate_normals::run(&mut app);
        let m = app.poly_node_mesh(nid).expect("poly body");
        assert_eq!(
            core_math::mesh_boolean::boundary_half_edge_count(&m),
            0,
            "winding still inconsistent after Recalculate Normals"
        );
    }

    /// THE OWNER BUG (2026-08-05): "mesh clean up we have some mesh clean up operation that
    /// break the tag systeme." Cleanup used to `poly_custom_data.remove(&nid)` — every Smart
    /// Select tag, sharp mark, UV, seam and crease on the body was erased by a repair pass.
    ///
    /// Here the tag sits on a 90° box edge cleanup has no interest in, so this isolates the
    /// CARRY (does the sidecar survive the rebuild?) from the PROTECTION tested below.
    #[test]
    fn mesh_cleanup_carries_tags_through_the_rebuild() {
        let (mesh, diagonal, box_edge) = box_with_split_top();
        let (mut app, nid) = app_with(mesh);
        tag_edge(&mut app, nid, box_edge.0, box_edge.1);
        assert_eq!(tag_count(&app, nid), 1, "fixture starts with exactly one tag");

        app.ui.mesh_cleanup_flatten = false;
        app.ui.mesh_cleanup_merge = true;
        app.ui.mesh_cleanup_dissolve_deg = 0.0;
        app.poly_mesh_cleanup_now(nid);

        let m = app.poly_node_mesh(nid).expect("poly body");
        assert_eq!(live_faces(&m), 6, "cleanup must still do its job: split top merged");
        assert!(!has_edge(&app, nid, diagonal.0, diagonal.1), "the untagged diagonal is merged away");
        assert_eq!(
            tag_count(&app, nid), 1,
            "the tag must survive the rebuild — this is the reported bug"
        );
        assert!(has_edge(&app, nid, box_edge.0, box_edge.1), "the tagged edge itself survives");
    }

    /// Owner 2026-08-05: "usaly all edge that are tag to bevel are important so mesh clean up
    /// should never touch it." Same fixture, tag moved ONTO the mergeable diagonal: cleanup now
    /// has nothing it is allowed to do, so it must leave the mesh alone rather than merge it.
    #[test]
    fn mesh_cleanup_never_removes_a_tagged_edge() {
        let (mesh, diagonal, _) = box_with_split_top();
        let (mut app, nid) = app_with(mesh);
        tag_edge(&mut app, nid, diagonal.0, diagonal.1);

        app.ui.mesh_cleanup_flatten = false;
        app.ui.mesh_cleanup_merge = true;
        app.ui.mesh_cleanup_dissolve_deg = 0.0;
        app.poly_mesh_cleanup_now(nid);

        assert!(
            has_edge(&app, nid, diagonal.0, diagonal.1),
            "a tagged edge must survive cleanup even when it is the only thing worth merging"
        );
        let m = app.poly_node_mesh(nid).expect("poly body");
        assert_eq!(live_faces(&m), 7, "nothing merged: the split top is still two faces");
        assert_eq!(tag_count(&app, nid), 1, "and its tag is still on it");
    }

    /// Limited Dissolve is the other cleanup op the owner flagged. A flat grid is the most
    /// dissolvable geometry there is; one tagged edge in it must still come out the far side.
    #[test]
    fn limited_dissolve_never_removes_a_tagged_edge() {
        let grid = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        // An interior edge of the grid, addressed by position.
        let (a, b) = {
            use core_math::half_edge::{HalfEdgeId, NO_FACE};
            let h = (0..grid.half_edges.len() as u32).map(HalfEdgeId).find(|&h| {
                grid.is_edge_live(h) && {
                    let (f1, f2) = grid.edge_faces(h);
                    f1 != NO_FACE && f2 != NO_FACE && f1 != f2
                }
            }).expect("grid has interior edges");
            (grid.vert(grid.he_source(h)).position, grid.vert(grid.he(h).vertex).position)
        };
        let (mut app, nid) = app_with(grid);
        tag_edge(&mut app, nid, a, b);
        app.poly_work_mode = crate::PolyWorkMode::Edge;
        app.poly_limited_dissolve_angle(5.0);

        assert!(
            has_edge(&app, nid, a, b),
            "Limited Dissolve flattened a tagged edge — the owner's 'cleanup breaks the tags'"
        );
        assert_eq!(tag_count(&app, nid), 1, "and the tag is still attached to it");
    }

    /// Every Weighted Normal setting must change the stack hash, or the modifier looks
    /// frozen: the hash is half the display-cache key, so an unhashed field means the cached
    /// mesh is reused and the slider does nothing. This exact bug shipped twice already.
    #[test]
    fn every_weighted_normal_field_changes_the_stack_hash() {
        let base = PolyModifier::new(PolyModifierType::WeightedNormal);
        let h0 = AzraApp::poly_modifier_stack_hash(std::slice::from_ref(&base));
        let mut m = base.clone();
        m.weighted_mode = 0;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[m]), "mode not hashed");
        let mut w = base.clone();
        w.weighted_weight = 80;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[w]), "weight not hashed");
        let mut t = base.clone();
        t.weighted_threshold += 0.05;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[t]), "threshold not hashed");
    }

    /// Remesh params must all change the stack hash or the resolution slider freezes.
    #[test]
    fn every_remesh_field_changes_the_stack_hash() {
        let base = PolyModifier::new(PolyModifierType::Remesh);
        let h0 = AzraApp::poly_modifier_stack_hash(std::slice::from_ref(&base));
        let mut r = base.clone();
        r.remesh_resolution = 48;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[r]), "resolution not hashed");
        let mut s = base.clone();
        s.remesh_smooth = !s.remesh_smooth;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[s]), "smooth not hashed");
        let mut p = base.clone();
        p.remesh_padding += 0.05;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[p]), "padding not hashed");
        let mut c = base.clone();
        c.remesh_collapse = !c.remesh_collapse;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[c]), "collapse not hashed");
        let mut rl = base.clone();
        rl.remesh_skip_relax_drag = !rl.remesh_skip_relax_drag;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[rl]), "skip-relax-drag not hashed");
        let mut sl = base.clone();
        sl.remesh_skip_seal_drag = !sl.remesh_skip_seal_drag;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[sl]), "skip-seal-drag not hashed");
        let mut dr = base.clone();
        dr.remesh_drop_res_drag = !dr.remesh_drop_res_drag;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[dr]), "drop-res-drag not hashed");
        let mut u = base.clone();
        u.remesh_union_islands = true;
        assert_ne!(
            h0,
            AzraApp::poly_modifier_stack_hash(&[u]),
            "union_islands not hashed"
        );
    }

    /// Voxel remesh on a cube must change topology (not a silent no-op).
    #[test]
    fn remesh_modifier_changes_cube_topology() {
        let base = cube();
        let m = PolyModifier::new(PolyModifierType::Remesh);
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &Default::default());
        assert!(
            out.face_count() != base.face_count() || out.vertex_count() != base.vertex_count(),
            "Remesh must change cube topology (got {}v/{}f, base {}v/{}f)",
            out.vertex_count(),
            out.face_count(),
            base.vertex_count(),
            base.face_count()
        );
        assert!(out.validate().is_ok(), "remeshed mesh must validate");
    }

    /// PERF PROBE — the Remesh "slider scrub" through the PRODUCTION modifier dispatch
    /// (`apply_poly_modifiers` → `apply_one_poly_modifier` → `voxel_remesh_mesh` → band
    /// sampler → parallel Surface Nets → half-edge rebuild), on a poly object dense enough
    /// to be realistic (Subdivide×2 box ≈ 96 quads → couple hundred tris). Each tick uses a
    /// DIFFERENT resolution, exactly like a user dragging the Resolution slider.
    /// Baseline 2026-08-08 pre-fix: seconds/tick. Post-fix: tens of ms/tick.
    /// Run: cargo test -p app --release bench_remesh_slider_scrub -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_remesh_slider_scrub() {
        let mut base = cube();
        let mut subdiv = PolyModifier::new(PolyModifierType::Subdivide);
        subdiv.subdiv_levels = 2;
        base = AzraApp::apply_poly_modifiers(&base, &[subdiv], &Default::default());
        for res in [32u32, 64, 96, 128, 192, 256] {
            let mut m = PolyModifier::new(PolyModifierType::Remesh);
            m.remesh_resolution = res;
            let t = std::time::Instant::now();
            let out =
                AzraApp::apply_poly_modifiers(&base, std::slice::from_ref(&m), &Default::default());
            eprintln!(
                "[REMESH SCRUB] res={res:3}: {:>7.1}ms  ({}v/{}f -> {}v/{}f)",
                t.elapsed().as_secs_f64() * 1e3,
                base.vertex_count(),
                base.face_count(),
                out.vertex_count(),
                out.face_count()
            );
            assert!(out.validate().is_ok(), "scrub res={res} must produce a valid mesh");
        }
    }

    /// Same guard as `every_weighted_normal_field_changes_the_stack_hash`, for Scatter: an
    /// unhashed field means the cached display mesh is reused and the control does nothing.
    #[test]
    fn every_scatter_field_changes_the_stack_hash() {
        let base = PolyModifier::new(PolyModifierType::Scatter);
        let h0 = AzraApp::poly_modifier_stack_hash(std::slice::from_ref(&base));
        // (label, mutation) for every authored Scatter field.
        let mutations: Vec<(&str, Box<dyn Fn(&mut PolyModifier)>)> = vec![
            ("source_nid", Box::new(|m: &mut PolyModifier| m.scatter_source_nid = 12)),
            ("source", Box::new(|m: &mut PolyModifier| m.scatter_source = 2)),
            ("count", Box::new(|m: &mut PolyModifier| m.scatter_count += 7)),
            ("seed", Box::new(|m: &mut PolyModifier| m.scatter_seed += 1)),
            ("align_normal", Box::new(|m: &mut PolyModifier| m.scatter_align_normal ^= true)),
            ("spin_deg", Box::new(|m: &mut PolyModifier| m.scatter_spin_deg += 15.0)),
            ("rot_jitter.x", Box::new(|m: &mut PolyModifier| m.scatter_rot_jitter_deg.x += 5.0)),
            ("rot_jitter.y", Box::new(|m: &mut PolyModifier| m.scatter_rot_jitter_deg.y += 5.0)),
            ("rot_jitter.z", Box::new(|m: &mut PolyModifier| m.scatter_rot_jitter_deg.z += 5.0)),
            ("pos_jitter.x", Box::new(|m: &mut PolyModifier| m.scatter_pos_jitter.x += 0.1)),
            ("pos_jitter.y", Box::new(|m: &mut PolyModifier| m.scatter_pos_jitter.y += 0.1)),
            ("pos_jitter.z", Box::new(|m: &mut PolyModifier| m.scatter_pos_jitter.z += 0.1)),
            ("scale_min", Box::new(|m: &mut PolyModifier| m.scatter_scale_min -= 0.3)),
            ("scale_max", Box::new(|m: &mut PolyModifier| m.scatter_scale_max += 0.3)),
            ("axis_min.x", Box::new(|m: &mut PolyModifier| m.scatter_scale_axis_min.x -= 0.2)),
            ("axis_min.y", Box::new(|m: &mut PolyModifier| m.scatter_scale_axis_min.y -= 0.2)),
            ("axis_min.z", Box::new(|m: &mut PolyModifier| m.scatter_scale_axis_min.z -= 0.2)),
            ("axis_max.x", Box::new(|m: &mut PolyModifier| m.scatter_scale_axis_max.x += 0.2)),
            ("axis_max.y", Box::new(|m: &mut PolyModifier| m.scatter_scale_axis_max.y += 0.2)),
            ("axis_max.z", Box::new(|m: &mut PolyModifier| m.scatter_scale_axis_max.z += 0.2)),
        ];
        for (name, f) in mutations {
            let mut m = base.clone();
            f(&mut m);
            assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[m]), "{name} not hashed");
        }
    }

    /// An UNSET Scatter — which is exactly what a pre-Scatter `.fl4` deserialises to — must
    /// leave the mesh bit-identical. Same for a source that no longer resolves (deleted body,
    /// or the body itself: the resolver drops both, so the map simply misses).
    #[test]
    fn unset_or_dangling_scatter_is_a_noop() {
        let base = core_math::half_edge::HalfEdgeMesh::new_box(Vec3::ONE);
        let empty = std::collections::HashMap::new();

        let unset = PolyModifier::new(PolyModifierType::Scatter);
        assert_eq!(unset.scatter_source_nid, 0, "a fresh/legacy Scatter is unset");
        let out = AzraApp::apply_poly_modifiers(&base, &[unset], &empty);
        assert_eq!(live_faces(&out), live_faces(&base));
        assert_eq!(out.vertices.len(), base.vertices.len());

        // Source set, but the node is gone → absent from the resolved map → no-op, no panic.
        let mut dangling = PolyModifier::new(PolyModifierType::Scatter);
        dangling.scatter_source_nid = 999;
        let out = AzraApp::apply_poly_modifiers(&base, &[dangling], &empty);
        assert_eq!(live_faces(&out), live_faces(&base));
    }

    /// End to end through the REAL app path: one island per site, target mesh untouched.
    #[test]
    fn scatter_merges_one_island_per_site() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        let base = HM::new_box(Vec3::ONE);
        let src = HM::new_box(Vec3::splat(0.1));
        let mut cutters = std::collections::HashMap::new();
        cutters.insert(5u64, src);

        let mut m = PolyModifier::new(PolyModifierType::Scatter);
        m.scatter_source_nid = 5;
        m.scatter_source = core_math::scatter::SCATTER_SRC_FACE_CENTERS;
        let out = AzraApp::apply_poly_modifiers(&base, &[m.clone()], &cutters);
        // 6 target faces + one 6-faced cube per face centre.
        assert_eq!(live_faces(&out), 6 + 6 * 6);
        assert!(out.validate().is_ok(), "scatter result must be a valid mesh");

        // Vertex source = one island per vertex (8 on a box).
        let mut mv = m.clone();
        mv.scatter_source = core_math::scatter::SCATTER_SRC_VERTICES;
        assert_eq!(live_faces(&AzraApp::apply_poly_modifiers(&base, &[mv], &cutters)), 6 + 8 * 6);

        // Random source: exactly `count` islands, area-weighted.
        let mut mr = m;
        mr.scatter_source = core_math::scatter::SCATTER_SRC_RANDOM_FACES;
        mr.scatter_count = 25;
        assert_eq!(live_faces(&AzraApp::apply_poly_modifiers(&base, &[mr], &cutters)), 6 + 25 * 6);
    }

    /// The seed is the whole reproducibility contract: same seed = same mesh, and a new seed
    /// moves the instances (which is also why the seed MUST be in the stack hash).
    #[test]
    fn scatter_seed_is_reproducible_and_changes_the_result() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        let base = HM::new_box(Vec3::ONE);
        let mut cutters = std::collections::HashMap::new();
        cutters.insert(5u64, HM::new_box(Vec3::splat(0.1)));
        let mut m = PolyModifier::new(PolyModifierType::Scatter);
        m.scatter_source_nid = 5;
        m.scatter_source = core_math::scatter::SCATTER_SRC_RANDOM_FACES;
        m.scatter_count = 12;
        m.scatter_seed = 1;
        let a = AzraApp::apply_poly_modifiers(&base, &[m.clone()], &cutters);
        let b = AzraApp::apply_poly_modifiers(&base, &[m.clone()], &cutters);
        let mut m2 = m;
        m2.scatter_seed = 2;
        let c = AzraApp::apply_poly_modifiers(&base, &[m2], &cutters);
        let pos = |mesh: &HM| -> Vec<[u32; 3]> {
            mesh.vertices
                .iter()
                .map(|v| [v.position.x.to_bits(), v.position.y.to_bits(), v.position.z.to_bits()])
                .collect()
        };
        assert_eq!(pos(&a), pos(&b), "same seed must rebuild the same mesh");
        assert_ne!(pos(&a), pos(&c), "a different seed must move the instances");
    }

    /// OWNER 2026-08-11: "smart select after boolean cut still forgets the tag on geo that was
    /// not touched — we need to make sure the tag persists."
    ///
    /// `poly_boolean_execute_cut` installs a `merge_meshes(diff, inter)` REBUILD, so every id is
    /// new — and it wrote the mesh WITHOUT re-keying `cache.poly_custom_data`. That is worse than
    /// dropping the sidecar: the surviving entries stay at their old ids and now name unrelated
    /// edges of the new mesh, so geometry the cut never reached silently loses its do-not-bevel
    /// tag (and strangers silently gain one). Subtract/Union/Intersect have carried the sidecar
    /// since BOOL-STALECD; Cut never did.
    ///
    /// The gate is the OWNER'S sentence, not an implementation detail: an edge the cut did not
    /// touch must come out of the op still tagged.
    #[test]
    fn boolean_cut_keeps_tags_on_geometry_it_did_not_touch() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        use core_math::scene_graph::NodePayload;

        // Target cube at the origin (corners at ±1), cutter box biting the −X−Y−Z corner only.
        let (mut app, target) = app_with(HM::new_box(Vec3::splat(1.0)));
        let root = app.scene.scene_graph.root();
        let cutter = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Cutter".into(),
                mesh: std::sync::Arc::new(HM::new_box(Vec3::splat(0.4))),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        // Park the cutter over one corner so most of the cube is provably untouched.
        if let Some(n) = app.scene.scene_graph.get_mut(cutter) {
            n.local_position = Vec3::splat(-1.0);
        }

        // The tagged edge: the +X+Y edge of the cube, diagonally opposite the cut.
        let (ta, tb) = (Vec3::new(1.0, 1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));
        tag_edge(&mut app, target, ta, tb);
        assert_eq!(edge_tagged(&app, target, ta, tb), Some(true), "fixture must start tagged");

        app.modeling_context = crate::ModelingContext::Polygonal;
        app.poly_work_mode = crate::PolyWorkMode::Object;
        // Selection order matters: ACTIVE = target, the other selected body = cutter.
        app.select_single_unified(crate::SceneItemId::Node(cutter));
        app.toggle_select_unified(crate::SceneItemId::Node(target));
        assert_eq!(
            app.sel.usel.active_node(),
            Some(target),
            "the boolean plan takes the ACTIVE body as the target"
        );

        let before = app.poly_node_mesh(target).expect("target").face_count();
        app.poly_boolean_execute_cut();
        let after = app.poly_node_mesh(target).expect("target").face_count();
        assert_ne!(before, after, "the cut must actually have imprinted (fixture check)");

        assert_eq!(
            edge_tagged(&app, target, ta, tb),
            Some(true),
            "an edge the cut did not touch must keep its Smart Select tag"
        );
    }

    /// Same class, the OTHER install that forgot: Apply bakes the stack into the base mesh, which
    /// renumbers every id. It replaced the mesh alone, leaving the sidecar keyed to the dead ids.
    ///
    /// Also pins the second half of that bug: Apply used to evaluate the stack with an EMPTY tag
    /// set while the viewport evaluated it WITH the tags, so a Smart Select bevel baked geometry
    /// the user had never been shown.
    #[test]
    fn applying_a_modifier_keeps_the_sidecar() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        use core_math::scene_graph::NodePayload;

        let (mut app, nid) = app_with(HM::new_box(Vec3::splat(1.0)));
        let (ta, tb) = (Vec3::new(1.0, 1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));
        tag_edge(&mut app, nid, ta, tb);

        // Triangulate rebuilds the mesh (new ids) but keeps every original edge in place, so a
        // surviving tag is unambiguous.
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                modifiers.push(PolyModifier::new(PolyModifierType::Triangulate));
            }
        }
        app.apply_poly_modifier_through(nid, 0);

        assert!(
            has_edge(&app, nid, ta, tb),
            "Triangulate keeps the original box edges (fixture check)"
        );
        assert_eq!(
            edge_tagged(&app, nid, ta, tb),
            Some(true),
            "Apply must carry the sidecar across the bake, not leave it keyed to dead ids"
        );
    }

    /// Apply on a stack card must bake and remove ONLY that card — not the whole prefix
    /// (owner: "apply button should only apply the modifier where the button is clicked").
    #[test]
    fn apply_removes_only_the_clicked_modifier() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        use core_math::scene_graph::NodePayload;

        let (mut app, nid) = app_with(HM::new_box(Vec3::splat(1.0)));
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                modifiers.push(PolyModifier::new(PolyModifierType::Mirror));
                modifiers.push(PolyModifier::new(PolyModifierType::Triangulate));
                modifiers.push(PolyModifier::new(PolyModifierType::Solidify));
            }
        }

        // Apply the middle card (Triangulate) only.
        app.apply_poly_modifier_through(nid, 1);

        let mods = match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { modifiers, .. } => modifiers.clone(),
            _ => panic!("expected poly body"),
        };
        assert_eq!(mods.len(), 2, "only the clicked modifier should leave the stack");
        assert!(
            matches!(mods[0].mod_type, PolyModifierType::Mirror),
            "modifier above the click must stay"
        );
        assert!(
            !mods[0].enabled,
            "modifier above must disable — its effect is already in the baked base"
        );
        assert!(
            matches!(mods[1].mod_type, PolyModifierType::Solidify),
            "modifier below the click must stay"
        );
        assert!(
            mods[1].enabled,
            "modifier below must stay live"
        );
    }

    /// SMART SELECT end to end (owner: "if we subtract a perfect cylinder, all the segments
    /// that create the round segment should be tagged to ignore the bevel"). Boolean then
    /// Bevel in one stack: ON must bevel FEWER edges than OFF, yet still bevel the rim, and
    /// the result must stay watertight (BEVEL.md section 6: rule 2 is the sound one).
    #[test]
    fn smart_select_skips_cutter_tessellation_and_keeps_the_rim() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        let base = HM::new_box(Vec3::splat(2.0));
        let mut cutters = std::collections::HashMap::new();
        cutters.insert(7u64, HM::new_cylinder(0.5, 6.0, 16));

        let mut boolean = PolyModifier::new(PolyModifierType::Boolean);
        boolean.bool_op = 0;
        boolean.bool_cutter_nid = 7;
        let mut bevel = PolyModifier::new(PolyModifierType::Bevel);
        bevel.bevel_width = 0.02;
        bevel.bevel_segments = 1;

        let cut_only = AzraApp::apply_poly_modifiers(&base, &[boolean.clone()], &cutters);
        let mut off = bevel.clone();
        off.bevel_smart_select = false;
        let all = AzraApp::apply_poly_modifiers(&base, &[boolean.clone(), off], &cutters);
        let mut on = bevel.clone();
        on.bevel_smart_select = true;
        let smart = AzraApp::apply_poly_modifiers(&base, &[boolean, on], &cutters);

        let (c, a, sm) = (live_faces(&cut_only), live_faces(&all), live_faces(&smart));
        assert!(a > c, "bevel-all must add faces ({a} vs {c})");
        assert!(sm < a, "Smart Select must bevel fewer edges ({sm} vs {a})");
        assert!(sm > c, "Smart Select must still bevel the rim ({sm} vs {c})");
        assert!(smart.validate().is_ok(), "smart result must be valid");
        assert!(smart.is_manifold().is_ok(), "smart result must stay watertight");
        assert_eq!(smart.count_folded_coplanar_pairs(), 0, "no folds");
    }

    /// AUTHORED MASK, END TO END through the real stack path — a Smooth whose geometry channel is
    /// a POLYGROUP must relax the marked faces and leave everything else EXACTLY where it was.
    ///
    /// This is the gate that the plumbing actually works: the channel is read inside
    /// `apply_one_poly_modifier`, the authored table is position-keyed (not id-keyed), and the mask
    /// reaches `smooth_laplacian_masked`. A kernel-level test cannot catch a break in any of that.
    #[test]
    fn polygroup_channel_masks_the_smooth_modifier() {
        use core_math::geometry_channel::{AuthoredChannels, GeoChannelKind, GeometryChannel};
        use core_math::half_edge::FaceId;

        // A plane with a bump, so smoothing has something to move.
        let mut base = core_math::half_edge::HalfEdgeMesh::new_plane(6.0, 6.0, 6, 6);
        let bump = core_math::half_edge::VertexId(24);
        base.vert_mut(bump).position.y += 1.0;

        let mut smooth = PolyModifier::new(PolyModifierType::Smooth);
        smooth.smooth_iterations = 4;
        smooth.smooth_factor = 0.5;
        smooth.smooth_falloff = 0; // hard mask, so the boundary is exact and assertable
        smooth.geo_channel = GeometryChannel {
            kind: GeoChannelKind::Polygroup,
            group_id: 1,
            ..Default::default()
        };
        let e = std::collections::HashMap::new();

        // Mark ONE face — the one carrying the bump is face 0's neighbourhood; pick face 0.
        let mut auth = AuthoredChannels::default();
        auth.group_by_face
            .insert(AuthoredChannels::face_key(&base, FaceId(0)), 1);
        let marked: Vec<u32> = base
            .face_vertices(FaceId(0))
            .iter()
            .map(|v| v.0)
            .collect();

        let out = AzraApp::apply_poly_modifiers_tagged(
            &base,
            &[smooth.clone()],
            &e,
            &Default::default(),
            &auth,
            &std::collections::HashMap::new(),
        );
        assert_eq!(out.vertices.len(), base.vertices.len(), "smooth never renumbers");

        let mut moved = 0usize;
        for i in 0..base.vertices.len() {
            let vid = core_math::half_edge::VertexId(i as u32);
            let (a, b) = (base.vert(vid).position, out.vert(vid).position);
            if marked.contains(&(i as u32)) {
                if (a - b).length() > 1e-6 {
                    moved += 1;
                }
            } else {
                assert_eq!(
                    a.to_array(),
                    b.to_array(),
                    "vertex {i} is outside the marked group and must not move at all"
                );
            }
        }
        assert!(moved > 0, "the marked group must actually be smoothed");

        // Control: the SAME stack with no authored table must be a strict no-op — a polygroup
        // channel with nothing authored matches nothing, it does not fall back to the whole mesh.
        let none = AzraApp::apply_poly_modifiers_tagged(
            &base,
            &[smooth],
            &e,
            &Default::default(),
            &Default::default(),
            &std::collections::HashMap::new(),
        );
        for i in 0..base.vertices.len() {
            let vid = core_math::half_edge::VertexId(i as u32);
            assert_eq!(
                base.vert(vid).position.to_array(),
                none.vert(vid).position.to_array(),
                "vertex {i} moved with no authored data — the channel failed OPEN"
            );
        }
    }

    /// NEAR-RIGHT-ANGLE OVERRIDE (owner 2026-08-05: "80 to 100 degree edges should always
    /// be bevel"). A near-square corner is a real structural edge whatever produced it, so
    /// it beats EVERY exclusion - geometry and tags alike. This is the safety net for the
    /// whole rule stack: no future rule can silently eat a corner the user can see.
    ///
    /// (Tag-driven exclusion itself is proven end-to-end by
    /// `boolean_support_edges_are_not_beveled_by_smart_select`, on real boolean output.)
    #[test]
    fn near_right_angle_edges_are_never_excluded() {
        use core_math::half_edge::HalfEdgeId;
        let base = cube(); // every edge is exactly 90 degrees
        let mut bevel = PolyModifier::new(PolyModifierType::Bevel);
        bevel.bevel_width = 0.05;
        bevel.bevel_segments = 1;
        bevel.bevel_smart_select = true;
        let e = std::collections::HashMap::new();

        // Tag EVERY edge - the most hostile case possible.
        let mut tags = std::collections::HashSet::new();
        for i in 0..base.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if base.is_edge_live(he) {
                tags.insert(AzraApp::edge_pos_key(&base, he));
            }
        }
        let tagged = AzraApp::apply_poly_modifiers_tagged(&base, &[bevel.clone()], &e, &tags, &Default::default(), &std::collections::HashMap::new());
        let untagged =
            AzraApp::apply_poly_modifiers_tagged(&base, &[bevel], &e, &Default::default(), &Default::default(), &std::collections::HashMap::new());

        assert_eq!(
            live_faces(&tagged),
            live_faces(&untagged),
            "a 90-degree corner must bevel even when tagged - the override failed"
        );
        assert!(live_faces(&tagged) > live_faces(&base), "the bevel must have happened");
    }

    /// END-TO-END: cut a cylinder, tag exactly as the boolean install does, then run the
    /// Bevel modifier with Smart Select. The support edges the cut created must NOT be
    /// beveled. Owner 2026-08-05: "segment created by boolean are still beveled with the
    /// modifier" — this test is the thing I should have written before claiming it worked.
    #[test]
    fn boolean_support_edges_are_not_beveled_by_smart_select() {
        use core_math::half_edge::{HalfEdgeMesh as HM, HalfEdgeId, NO_TWIN};
        use core_math::mesh_boolean::{mesh_boolean_with_provenance, BoolOp};
        let cube = HM::new_box(Vec3::splat(2.0));
        let cyl = HM::new_cylinder(0.5, 6.0, 16);
        let (res, prov) =
            mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference).expect("boolean");

        // Tag exactly as poly/boolean.rs does at install: cutter interior + flat support.
        let mut tags: std::collections::HashSet<(i64, i64, i64, i64, i64, i64)> =
            std::collections::HashSet::new();
        for i in 0..res.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if res.is_edge_live(he) && prov.edge_is_cutter_interior(&res, he) {
                tags.insert(AzraApp::edge_pos_key(&res, he));
            }
        }
        let support: Vec<u32> = prov.new_flat_edges(&res);
        assert!(!support.is_empty(), "the cut must leave support edges");
        for &e in &support {
            let he = HalfEdgeId(e);
            tags.insert(AzraApp::edge_pos_key(&res, he));
            let tw = res.he(he).twin;
            if tw != NO_TWIN {
                tags.insert(AzraApp::edge_pos_key(&res, tw));
            }
        }

        let mut bevel = PolyModifier::new(PolyModifierType::Bevel);
        bevel.bevel_width = 0.02;
        bevel.bevel_segments = 1;
        bevel.bevel_smart_select = true;
        let bevel_ref = bevel.clone();
        let e = std::collections::HashMap::new();
        let out = AzraApp::apply_poly_modifiers_tagged(&res, &[bevel], &e, &tags, &Default::default(), &std::collections::HashMap::new());

        // Compare against the SAME bevel with no tags. Position keys cannot be used to check
        // survival: beveling a NEIGHBOURING crease moves this edge's endpoints, so its key
        // changes even when it was correctly left alone. What is observable is that tagging
        // removes edges from the selection, so fewer strips are built.
        let untagged = AzraApp::apply_poly_modifiers_tagged(
            &res, &[bevel_ref.clone()], &e, &Default::default(), &Default::default(),
            &std::collections::HashMap::new(),
        );
        let (f_tagged, f_untagged) = (live_faces(&out), live_faces(&untagged));
        eprintln!(
            "[E2E] faces: tagged={f_tagged} untagged={f_untagged} (support edges={})",
            support.len()
        );
        assert!(
            f_tagged < f_untagged,
            "tagging changed nothing - the boolean's edges were beveled anyway              ({f_tagged} vs {f_untagged})"
        );
        assert!(out.validate().is_ok() && out.is_manifold().is_ok(), "result must be sound");
    }

    // ------------------------------------------------------------------------------------
    // INSET + EXTRUDE tagging (owner 2026-08-05: "inset: tag all geo created by inset should
    // be tagged not to bevel" / "extrude: we should use angle based to tag or not, maybe all
    // 20 degree under are tagged out of bevel").
    // ------------------------------------------------------------------------------------

    /// The face of `m` whose normal is closest to `dir`.
    fn face_facing(m: &HalfEdgeMesh, dir: Vec3) -> u32 {
        (0..m.faces.len() as u32)
            .filter(|&f| m.face(FaceId(f)).half_edge != NO_EDGE)
            .max_by(|&a, &b| {
                let da = m.compute_face_normal(FaceId(a)).dot(dir);
                let db = m.compute_face_normal(FaceId(b)).dot(dir);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("mesh has a live face")
    }

    fn keys(
        m: &HalfEdgeMesh,
        edges: &[core_math::half_edge::HalfEdgeId],
    ) -> std::collections::HashSet<(i64, i64, i64, i64, i64, i64)> {
        edges.iter().map(|&h| AzraApp::edge_pos_key(m, h)).collect()
    }

    /// END-TO-END: run the REAL inset, stamp exactly as the install does, then run the Bevel
    /// modifier with Smart Select. The geometry the inset created must NOT be beveled.
    ///
    /// Note this is a case GEOMETRY alone cannot handle: a flat inset's new edges are exactly
    /// coplanar, and the modifier deliberately keeps coplanar edges beveled (a knife cut is
    /// coplanar). Only the tags can exclude them.
    #[test]
    fn inset_created_edges_are_not_beveled_by_smart_select() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        let mut m = HM::new_box(Vec3::splat(2.0));
        let top = face_facing(&m, Vec3::Y);
        let applied = crate::poly::inset::apply_inset(&mut m, &[top], 0.4, 0.0, false);
        assert!(!applied.inner.is_empty(), "the inset must produce an island");
        assert!(!applied.new_verts.is_empty(), "the inset must allocate vertices");

        // EXACTLY the install's rule (poly/inset.rs -> poly_inset_stamp_tags).
        let created = crate::poly::op_tags::edges_incident_to_verts(&m, &applied.new_verts);
        let tags = keys(&m, &created);
        assert!(!tags.is_empty(), "the inset must report created edges");

        let mut bevel = PolyModifier::new(PolyModifierType::Bevel);
        bevel.bevel_width = 0.05;
        bevel.bevel_segments = 1;
        bevel.bevel_smart_select = true;
        let e = std::collections::HashMap::new();
        let tagged = AzraApp::apply_poly_modifiers_tagged(&m, &[bevel.clone()], &e, &tags, &Default::default(), &std::collections::HashMap::new());
        let untagged =
            AzraApp::apply_poly_modifiers_tagged(&m, &[bevel], &e, &Default::default(), &Default::default(), &std::collections::HashMap::new());

        let (ft, fu) = (live_faces(&tagged), live_faces(&untagged));
        eprintln!(
            "[E2E inset] created half-edges={} tag keys={} faces: tagged={ft} untagged={fu}",
            created.len(),
            tags.len()
        );
        assert!(
            ft < fu,
            "tagging changed nothing - the inset's own edges were beveled anyway ({ft} vs {fu})"
        );
        assert!(tagged.validate().is_ok(), "result must be sound");
    }

    /// MEASUREMENT the owner asked for: the inset region's OUTER boundary — where the rim band
    /// meets the untouched surface — is the SAME edge, in the same place, that existed before
    /// the inset. It is a real crease (a 90° cube edge here) and stays beveled. Tagging it would
    /// repeat the bevel's end-arc mistake (BEVEL.md §6).
    #[test]
    fn inset_leaves_the_regions_outer_boundary_beveled() {
        use core_math::half_edge::{HalfEdgeId, HalfEdgeMesh as HM};
        let mut m = HM::new_box(Vec3::splat(2.0));
        let top = face_facing(&m, Vec3::Y);
        // The 4 edges bounding the top face, BEFORE the inset, by position key.
        let outer_before: std::collections::HashSet<_> = m
            .face_half_edges(FaceId(top))
            .iter()
            .map(|&h| AzraApp::edge_pos_key(&m, h))
            .collect();
        assert_eq!(outer_before.len(), 4);

        let applied = crate::poly::inset::apply_inset(&mut m, &[top], 0.4, 0.0, false);
        let created = crate::poly::op_tags::edges_incident_to_verts(&m, &applied.new_verts);
        let tags = keys(&m, &created);

        let hit: Vec<_> = outer_before.intersection(&tags).collect();
        eprintln!(
            "[MEASURE inset] outer boundary edges={} of which tagged={} (created keys={})",
            outer_before.len(),
            hit.len(),
            tags.len()
        );
        assert!(hit.is_empty(), "the pre-existing outer boundary must NOT be tagged");

        // ... and those 4 edges really are creases, not seams geometry would have skipped.
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) || !outer_before.contains(&AzraApp::edge_pos_key(&m, h)) {
                continue;
            }
            let c = m.edge_dihedral_cos(h).expect("interior edge");
            assert!(c.abs() < 0.01, "expected a ~90 degree cube edge, got cos={c}");
        }
    }

    /// END-TO-END: a FLUSH extrude — pushing a cube's whole top face up — leaves the new wall
    /// exactly coplanar with the side face it grew out of. That seam is invisible geometry and
    /// must not be beveled; it is also exactly coplanar, so only provenance can exclude it.
    #[test]
    fn flush_extrude_seams_are_tagged_and_not_beveled() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        let mut m = HM::new_box(Vec3::splat(2.0));
        let pre = m.clone();
        let top = face_facing(&m, Vec3::Y);
        let set: std::collections::HashSet<FaceId> = std::iter::once(FaceId(top)).collect();
        let caps = m.extrude_region(&set, Vec3::new(0.0, 0.5, 0.0));
        assert!(!caps.is_empty(), "extrude must produce a cap");

        // EXACTLY the install's rule (poly/extrude.rs -> poly_extrude_stamp_tags).
        let created_faces = crate::poly::op_tags::faces_created(&pre, &m);
        let edges = crate::poly::op_tags::edges_of_faces(&m, &created_faces);
        let tagged_edges =
            crate::poly::op_tags::retain_near_tangent(&m, &edges, EXTRUDE_TAG_TANGENT_DEG);
        eprintln!(
            "[E2E extrude] created faces={} created half-edges={} tagged={}",
            created_faces.len(),
            edges.len(),
            tagged_edges.len()
        );
        assert!(!tagged_edges.is_empty(), "the flush base seams must be tagged");
        let tags = keys(&m, &tagged_edges);

        let mut bevel = PolyModifier::new(PolyModifierType::Bevel);
        bevel.bevel_width = 0.05;
        bevel.bevel_segments = 1;
        bevel.bevel_smart_select = true;
        let e = std::collections::HashMap::new();
        let with = AzraApp::apply_poly_modifiers_tagged(&m, &[bevel.clone()], &e, &tags, &Default::default(), &std::collections::HashMap::new());
        let without =
            AzraApp::apply_poly_modifiers_tagged(&m, &[bevel], &e, &Default::default(), &Default::default(), &std::collections::HashMap::new());
        let (ft, fu) = (live_faces(&with), live_faces(&without));
        eprintln!("[E2E extrude] faces: tagged={ft} untagged={fu}");
        assert!(
            ft < fu,
            "tagging changed nothing - the flush seam was beveled anyway ({ft} vs {fu})"
        );
        assert!(with.validate().is_ok(), "result must be sound");
    }

    /// The other half of the owner's rule: an ORDINARY extrude — a boss whose walls stand at
    /// ~90° to both the cap and the surrounding surface — must tag NOTHING, so normal modeling
    /// is completely unaffected by this feature.
    #[test]
    fn a_steep_extrude_tags_nothing() {
        use core_math::half_edge::HalfEdgeMesh as HM;
        let mut m = HM::new_box(Vec3::splat(2.0));
        // Inset first so the boss is SMALLER than the face: every created wall then meets a
        // real crease instead of running flush with the cube's side.
        let top = face_facing(&m, Vec3::Y);
        let island = crate::poly::inset::apply_inset(&mut m, &[top], 0.5, 0.0, false);
        assert_eq!(island.inner.len(), 1);

        let pre = m.clone();
        let set: std::collections::HashSet<FaceId> =
            island.inner.iter().map(|&f| FaceId(f)).collect();
        let caps = m.extrude_region(&set, Vec3::new(0.0, 0.6, 0.0));
        assert!(!caps.is_empty());

        let created_faces = crate::poly::op_tags::faces_created(&pre, &m);
        let edges = crate::poly::op_tags::edges_of_faces(&m, &created_faces);
        let tagged =
            crate::poly::op_tags::retain_near_tangent(&m, &edges, EXTRUDE_TAG_TANGENT_DEG);
        let angles: Vec<f32> = edges
            .iter()
            .filter_map(|&h| m.edge_dihedral_cos(h))
            .map(|c| c.acos().to_degrees())
            .collect();
        let min_angle = angles.iter().cloned().fold(f32::MAX, f32::min);
        eprintln!(
            "[E2E extrude steep] created faces={} half-edges={} tagged={} min deviation={min_angle:.1} deg",
            created_faces.len(),
            edges.len(),
            tagged.len()
        );
        assert!(!edges.is_empty(), "the extrude must create edges to judge");
        assert!(
            tagged.is_empty(),
            "an ordinary extrude must tag nothing, tagged {} edges (min deviation {min_angle:.1} deg)",
            tagged.len()
        );
    }

    /// SMART SELECT MUST STAY INSIDE THE GEOMETRY CHANNEL.
    ///
    /// The near-90° "always bevel" safety net was built by scanning the WHOLE mesh and then
    /// re-inserted unconditionally, so every hard corner the user's channel had just excluded came
    /// straight back. Symptom: mask a Bevel to a polygroup (or an edge-weight range, or N-gons),
    /// tick Smart Select, and the bevel jumps to the entire object — Smart Select silently
    /// cancelling an explicit selection. It overrides the rules it INFERS, never the one the user
    /// chose by hand.
    ///
    /// Gate: with a channel set, Smart Select on must not bevel more edges than Smart Select off.
    /// A cube is the sharpest possible case — every one of its edges is exactly 90°, so a
    /// whole-mesh force-keep re-selects all of them.
    #[test]
    fn smart_select_never_escapes_the_geometry_channel() {
        use core_math::geometry_channel::{GeoChannelKind, GeometryChannel};
        use core_math::half_edge::HalfEdgeMesh as HM;

        // Cube + one triangulated face: the channel "N-gons only" then excludes the two triangles,
        // and EVERY remaining cube edge sits at exactly 90° for the force-keep to grab.
        let base = HM::new_box(Vec3::splat(1.0));

        let mut chan = PolyModifier::new(PolyModifierType::Bevel);
        chan.bevel_width = 0.05;
        chan.bevel_segments = 1;
        chan.geo_channel = GeometryChannel {
            kind: GeoChannelKind::EdgeAngle,
            // A band no cube edge falls in: 100..=170 excludes all of its 90° edges.
            angle_min_deg: 100.0,
            angle_max_deg: 170.0,
            ..Default::default()
        };
        let cutters = std::collections::HashMap::new();

        let mut off = chan.clone();
        off.bevel_smart_select = false;
        let masked_off = AzraApp::apply_poly_modifiers(&base, &[off], &cutters);

        let mut on = chan.clone();
        on.bevel_smart_select = true;
        let masked_on = AzraApp::apply_poly_modifiers(&base, &[on], &cutters);

        let f = |m: &HM| live_faces(m);
        eprintln!(
            "[CHANNEL] base={} channel+smart_off={} channel+smart_on={}",
            f(&base), f(&masked_off), f(&masked_on)
        );
        // The channel matches nothing on a cube, so BOTH must be the untouched base mesh.
        assert_eq!(
            f(&masked_off), f(&base),
            "fixture check: this channel must match no cube edge"
        );
        assert_eq!(
            f(&masked_on), f(&masked_off),
            "Smart Select must not bevel edges the Geometry Channel excluded — it overrides the \
             rules it infers, never the user's explicit selection"
        );
    }

    /// Fail-closed: with no boolean there are no tags, so Smart Select must be INERT and
    /// match bevel-everything exactly. Missing provenance never silently skips edges.
    #[test]
    fn smart_select_is_inert_without_a_boolean() {
        let mut off = PolyModifier::new(PolyModifierType::Bevel);
        off.bevel_width = 0.05;
        off.bevel_segments = 1;
        let mut on = off.clone();
        on.bevel_smart_select = true;
        let e = std::collections::HashMap::new();
        assert_eq!(
            live_faces(&AzraApp::apply_poly_modifiers(&cube(), &[off], &e)),
            live_faces(&AzraApp::apply_poly_modifiers(&cube(), &[on], &e)),
        );
    }

    /// EVERY editable modifier field must change the stack hash. The hash is half the display
    /// cache key, so a field missing from it is a field the user cannot edit: the signature
    /// stays equal, the cached mesh is reused, and the modifier looks frozen. This exact bug
    /// shipped twice - elongation missing from the bake hash, then every Pipe field missing
    /// here ("the pipe modifier does not update live"). Add a case whenever you add a field.
    #[test]
    fn every_pipe_field_changes_the_stack_hash() {
        let base = {
            let mut m = PolyModifier::new(PolyModifierType::Pipe);
            m.pipe_path = vec![Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)];
            m
        };
        let h0 = AzraApp::poly_modifier_stack_hash(std::slice::from_ref(&base));

        let mut r = base.clone();
        r.pipe_radius += 0.05;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[r]), "radius not hashed");

        let mut sg = base.clone();
        sg.pipe_segments += 1;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[sg]), "segments not hashed");

        let mut c = base.clone();
        c.pipe_caps = !c.pipe_caps;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[c]), "caps not hashed");

        let mut cl = base.clone();
        cl.pipe_closed = !cl.pipe_closed;
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[cl]), "closed not hashed");

        let mut p = base.clone();
        p.pipe_path.push(Vec3::new(1.0, 1.0, 0.0));
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[p]), "path length not hashed");

        let mut pm = base.clone();
        pm.pipe_path[1] = Vec3::new(0.0, 2.0, 0.0);
        assert_ne!(h0, AzraApp::poly_modifier_stack_hash(&[pm]), "path positions not hashed");
    }

    /// PIPE on an EMPTY base = the "own object" mode: the modifier generates the whole body.
    #[test]
    fn pipe_modifier_generates_the_whole_body_from_an_empty_base() {
        let mut m = PolyModifier::new(PolyModifierType::Pipe);
        m.pipe_path = vec![Vec3::ZERO, Vec3::new(0.0, 2.0, 0.0)];
        m.pipe_radius = 0.2;
        m.pipe_segments = 10;
        m.pipe_caps = true;
        let out = AzraApp::apply_poly_modifiers(
            &HalfEdgeMesh::new(), &[m], &std::collections::HashMap::new(),
        );
        assert_eq!(live_faces(&out), 10 + 2, "10 side quads + 2 caps");
        assert!(out.validate().is_ok());
        assert!(out.is_manifold().is_ok(), "capped pipe is a closed solid");
    }

    /// PIPE on an existing mesh = the "island in the current mesh" mode: the host survives
    /// untouched and the tube is ADDED (owner's second target choice).
    #[test]
    fn pipe_modifier_adds_an_island_without_disturbing_the_host() {
        let base = cube();
        let host_faces = live_faces(&base);
        let mut m = PolyModifier::new(PolyModifierType::Pipe);
        m.pipe_path = vec![Vec3::new(3.0, 0.0, 0.0), Vec3::new(3.0, 2.0, 0.0)];
        m.pipe_radius = 0.2;
        m.pipe_segments = 8;
        m.pipe_caps = true;
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        assert_eq!(
            live_faces(&out), host_faces + 8 + 2,
            "host faces must be untouched and the tube added as a separate island"
        );
        assert!(out.validate().is_ok());
    }

    /// An empty path is a NO-OP — this is exactly what a pre-Pipe .fl4 deserialises to, so it
    /// must never destroy or alter the mesh it loads onto.
    #[test]
    fn pipe_modifier_with_no_path_is_a_noop() {
        let base = cube();
        let before = live_faces(&base);
        let m = PolyModifier::new(PolyModifierType::Pipe); // pipe_path defaults empty
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        assert_eq!(live_faces(&out), before, "empty path must leave the mesh alone");
    }

    /// Radius and segments are re-read from the modifier on every evaluation — that is what
    /// makes them editable forever rather than baked at creation.
    #[test]
    fn pipe_modifier_reevaluates_radius_and_segments() {
        let mut m = PolyModifier::new(PolyModifierType::Pipe);
        m.pipe_path = vec![Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)];
        m.pipe_caps = false;
        m.pipe_segments = 6;
        let empty = std::collections::HashMap::new();
        let a = AzraApp::apply_poly_modifiers(&HalfEdgeMesh::new(), &[m.clone()], &empty);
        assert_eq!(live_faces(&a), 6);
        m.pipe_segments = 20;
        m.pipe_radius = 0.5;
        let b = AzraApp::apply_poly_modifiers(&HalfEdgeMesh::new(), &[m], &empty);
        assert_eq!(live_faces(&b), 20, "segment change must re-generate, not stack");
        let r = b.vert(core_math::half_edge::VertexId(0)).position;
        assert!(((r.x * r.x + r.z * r.z).sqrt() - 0.5).abs() < 1e-4, "radius change must apply");
    }

    /// Subdivide then Array: free apply path must stay deterministic after P2a apply_one split.
    /// Cube 6 quads → Catmull-Clark L1 = 24 quads → Array×2 = 48 faces.
    #[test]
    fn apply_subdiv_then_array_face_count() {
        let mut sub = PolyModifier::new(PolyModifierType::Subdivide);
        sub.subdiv_levels = 1;
        let mut arr = PolyModifier::new(PolyModifierType::Array);
        arr.array_count = 2;
        arr.array_offset = Vec3::new(4.0, 0.0, 0.0);
        let out = AzraApp::apply_poly_modifiers(
            &cube(),
            &[sub, arr],
            &std::collections::HashMap::new(),
        );
        assert_eq!(live_faces(&out), 48, "subdiv L1 cube (24) × array 2 = 48 faces");
    }

    /// FULL APP PATH: drive the REAL `AzraApp::apply_poly_modifiers` (Array logic lives app-side in
    /// merge_meshes/transformed) with the Blender Array-modifier oracle. Blender Array count=3,
    /// constant_offset (2,0,0), merge OFF, on the size-2 cube = 24 verts / 18 faces (no weld,
    /// captured 2026-07-17). AZRA must match that multiset exactly.
    #[test]
    fn apply_array_matches_blender_oracle() {
        let mut m = PolyModifier::new(PolyModifierType::Array);
        m.array_count = 3;
        m.array_offset = Vec3::new(2.0, 0.0, 0.0);
        let out = AzraApp::apply_poly_modifiers(&cube(), &[m], &std::collections::HashMap::new());
        assert_eq!(out.vertices.len(), 24, "3 cube copies, no weld = 24 verts (Blender oracle)");
        assert_eq!(live_faces(&out), 18, "3 x 6 quads = 18 faces (Blender oracle)");
        // Copy origins at x = -1..1, 1..3, 3..5 → x extremes -1 and 5.
        let xs: Vec<f32> = out.vertices.iter().map(|v| v.position.x).collect();
        let (xmin, xmax) = xs.iter().fold((f32::MAX, f32::MIN), |(a,b),&x| (a.min(x), b.max(x)));
        assert!((xmin - -1.0).abs() < 1e-4 && (xmax - 5.0).abs() < 1e-4, "array spans x=[-1,5] ({xmin},{xmax})");
    }

    /// Duplicate: array_count = EXTRA copies. count=1 → original + 1 = 16 verts / 12 faces.
    #[test]
    fn apply_duplicate_one_extra() {
        let mut m = PolyModifier::new(PolyModifierType::Duplicate);
        m.array_count = 1;
        m.array_offset = Vec3::new(3.0, 0.0, 0.0);
        let out = AzraApp::apply_poly_modifiers(&cube(), &[m], &std::collections::HashMap::new());
        assert_eq!(out.vertices.len(), 16, "original + 1 copy = 16 verts");
        assert_eq!(live_faces(&out), 12, "2 x 6 quads = 12 faces");
        let xs: Vec<f32> = out.vertices.iter().map(|v| v.position.x).collect();
        let (xmin, xmax) = xs.iter().fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        // original [-1,1] + copy at +3 → [2,4] → span [-1, 4]
        assert!(
            (xmin - -1.0).abs() < 1e-4 && (xmax - 4.0).abs() < 1e-4,
            "duplicate spans x=[-1,4] ({xmin},{xmax})"
        );
    }

    /// Circular array: count=4 full 360° about Z through origin → 4 disjoint cubes.
    #[test]
    fn apply_array_circular_full_ring() {
        let mut m = PolyModifier::new(PolyModifierType::ArrayCircular);
        m.array_count = 4;
        m.circ_angle_deg = 360.0;
        m.circ_axis = 2; // Z
        m.circ_center = Vec3::ZERO;
        m.circ_radius = 0.0;
        let out = AzraApp::apply_poly_modifiers(&cube(), &[m], &std::collections::HashMap::new());
        assert_eq!(out.vertices.len(), 32, "4 cube copies = 32 verts");
        assert_eq!(live_faces(&out), 24, "4 x 6 quads = 24 faces");
    }

    /// FULL APP PATH: Triangulate arm through the real dispatcher = all-tris, 12 faces.
    #[test]
    fn apply_triangulate_all_tris() {
        let out = AzraApp::apply_poly_modifiers(
            &cube(),
            &[PolyModifier::new(PolyModifierType::Triangulate)],
            &std::collections::HashMap::new(),
        );
        assert_eq!(live_faces(&out), 12, "cube triangulate = 12 tris");
        for i in 0..out.faces.len() as u32 {
            let f = FaceId(i);
            if out.face(f).half_edge != NO_EDGE {
                assert_eq!(out.face_vertices(f).len(), 3, "every face is a triangle");
            }
        }
    }

    /// Geometry channel on Triangulate: "N-gons only" leaves quads alone on a cube (0 n-gons).
    #[test]
    fn apply_triangulate_channel_ngons_only_leaves_cube() {
        use core_math::geometry_channel::{GeoChannelKind, GeometryChannel};
        let mut m = PolyModifier::new(PolyModifierType::Triangulate);
        m.geo_channel = GeometryChannel {
            kind: GeoChannelKind::Ngons,
            ..Default::default()
        };
        let base = cube();
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        assert_eq!(
            live_faces(&out),
            live_faces(&base),
            "no n-gons on a cube → channel matches nothing → mesh unchanged"
        );
        // All faces still quads.
        for i in 0..out.faces.len() as u32 {
            let f = FaceId(i);
            if out.face(f).half_edge != NO_EDGE {
                assert_eq!(out.face_vertices(f).len(), 4);
            }
        }
    }

    /// Geometry channel on Triangulate: "Quads only" splits every cube face into 2 tris.
    #[test]
    fn apply_triangulate_channel_quads_only() {
        use core_math::geometry_channel::{GeoChannelKind, GeometryChannel};
        let mut m = PolyModifier::new(PolyModifierType::Triangulate);
        m.geo_channel = GeometryChannel {
            kind: GeoChannelKind::Quads,
            ..Default::default()
        };
        let out = AzraApp::apply_poly_modifiers(
            &cube(),
            &[m],
            &std::collections::HashMap::new(),
        );
        assert_eq!(live_faces(&out), 12, "6 quads → 12 tris");
        for i in 0..out.faces.len() as u32 {
            let f = FaceId(i);
            if out.face(f).half_edge != NO_EDGE {
                assert_eq!(out.face_vertices(f).len(), 3);
            }
        }
    }

    /// FULL APP PATH: Decimate = limited dissolve. Coplanar grid collapses; cube at 5° is untouched.
    #[test]
    fn apply_decimate_dissolves_coplanar_plane() {
        let plane = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let before_f = plane.face_count();
        assert!(before_f > 1);
        let out = AzraApp::apply_poly_modifiers(
            &plane,
            &[PolyModifier::new(PolyModifierType::Decimate)],
            &std::collections::HashMap::new(),
        );
        assert!(
            out.face_count() < before_f,
            "Decimate must reduce faces on a coplanar grid: {before_f} → {}",
            out.face_count()
        );
        // Cube faces meet at 90° — default 5° threshold must leave them alone.
        let cube_out = AzraApp::apply_poly_modifiers(
            &cube(),
            &[PolyModifier::new(PolyModifierType::Decimate)],
            &std::collections::HashMap::new(),
        );
        assert_eq!(live_faces(&cube_out), 6, "cube Decimate at 5° is a no-op");
    }

    /// B2: Bevel modifier arm calls bevel_op on all edges — same result as a direct all-edge
    /// bevel_edges_offset at the same width/segments. Property: not identity, and bit-identical
    /// to the tool path (same params).
    #[test]
    fn apply_bevel_matches_bevel_op_all_edges() {
        let base = cube();
        let mut m = PolyModifier::new(PolyModifierType::Bevel);
        m.bevel_width = 0.05;
        m.bevel_segments = 1;
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());

        let selected = super::all_live_half_edge_ids(&base);
        let params = core_math::bevel_op::BevelParams {
            width: 0.05,
            segments: 1,
            shape: 0.5,
            clamp: true,
        };
        let direct = core_math::bevel_op::bevel_edges_offset(&base, &selected, &params);
        assert!(
            core_math::bevel_op::bevel_result_installable(&base, &direct),
            "direct all-edge bevel must be installable"
        );
        assert_eq!(
            out.vertices.len(),
            direct.vertices.len(),
            "modifier Bevel must match bevel_op vertex count"
        );
        assert_eq!(
            live_faces(&out),
            live_faces(&direct),
            "modifier Bevel must match bevel_op face count"
        );
        // Not identity: all-edge bevel on a cube grows topology.
        assert!(
            out.vertices.len() > base.vertices.len(),
            "Bevel modifier must change the mesh (got {} verts, base 8)",
            out.vertices.len()
        );
        // Positions: same multiset (sort by quantized coords).
        let mut a: Vec<[i64; 3]> = out.vertices.iter().map(|v| {
            let p = v.position;
            [
                (p.x * 1e5).round() as i64,
                (p.y * 1e5).round() as i64,
                (p.z * 1e5).round() as i64,
            ]
        }).collect();
        let mut b: Vec<[i64; 3]> = direct.vertices.iter().map(|v| {
            let p = v.position;
            [
                (p.x * 1e5).round() as i64,
                (p.y * 1e5).round() as i64,
                (p.z * 1e5).round() as i64,
            ]
        }).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "modifier Bevel positions must match bevel_op (quantized)");
    }

    /// B2 gate: disabled Bevel leaves the base mesh unchanged.
    #[test]
    fn apply_bevel_disabled_is_identity() {
        let base = cube();
        let mut m = PolyModifier::new(PolyModifierType::Bevel);
        m.enabled = false;
        m.bevel_width = 0.05;
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        assert_eq!(out.vertices.len(), 8);
        assert_eq!(live_faces(&out), 6);
    }

    /// P1-1 residual: shape must not be a silent constant — flat (0) vs round (0.5) differ on
    /// multi-seg all-edge bevel. Property gate, not "output changed somehow".
    #[test]
    fn apply_bevel_shape_flat_differs_from_round() {
        let base = cube();
        let mut flat = PolyModifier::new(PolyModifierType::Bevel);
        flat.bevel_width = 0.08;
        flat.bevel_segments = 3;
        flat.bevel_shape = 0.0;
        flat.bevel_clamp = true;
        let mut round = flat.clone();
        round.bevel_shape = 0.5;
        let a = AzraApp::apply_poly_modifiers(&base, &[flat], &std::collections::HashMap::new());
        let b = AzraApp::apply_poly_modifiers(&base, &[round], &std::collections::HashMap::new());
        assert!(a.vertices.len() > base.vertices.len(), "bevel must grow topology");
        assert_eq!(a.vertices.len(), b.vertices.len(), "same segments → same vert count class");
        // Positions: at least one vertex differs (quantized).
        let key = |m: &HalfEdgeMesh| -> Vec<[i64; 3]> {
            let mut v: Vec<[i64; 3]> = m.vertices.iter().map(|vert| {
                let p = vert.position;
                [(p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64]
            }).collect();
            v.sort_unstable();
            v
        };
        assert_ne!(key(&a), key(&b), "shape 0 (flat) must differ from shape 0.5 (round)");
    }

    /// FULL APP PATH: panel bitmask X=1 must mirror X (P0-1). Uses +X-shifted cube so the property
    /// is "negative x appears", not mere vert count (symmetric cube would pass a wrong-axis gate).
    #[test]
    fn apply_mirror_bitmask_x_mirrors_x() {
        let base = {
            let mut m = cube();
            for v in m.vertices.iter_mut() {
                v.position.x += 2.0; // cube is x∈[-1,1] → x∈[1,3]
            }
            m
        };
        let mut m = PolyModifier::new(PolyModifierType::Mirror);
        m.mirror_axis = 1; // panel X checkbox bit
        m.mirror_merge = false;
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        let min_x = out.vertices.iter().map(|v| v.position.x).fold(f32::MAX, f32::min);
        let max_x = out.vertices.iter().map(|v| v.position.x).fold(f32::MIN, f32::max);
        assert!(min_x < -0.9, "panel X bit must produce negative-x (min_x={min_x})");
        assert!(max_x > 0.9, "panel X bit must keep positive-x (max_x={max_x})");
        let min_y = out.vertices.iter().map(|v| v.position.y).fold(f32::MAX, f32::min);
        let max_y = out.vertices.iter().map(|v| v.position.y).fold(f32::MIN, f32::max);
        assert!(min_y > -1.1 && max_y < 1.1, "X mirror must not expand Y");
    }

    #[test]
    fn apply_mirror_bitmask_y_mirrors_y() {
        let base = {
            let mut m = cube();
            for v in m.vertices.iter_mut() {
                v.position.y += 2.0;
            }
            m
        };
        let mut m = PolyModifier::new(PolyModifierType::Mirror);
        m.mirror_axis = 2; // panel Y checkbox bit
        m.mirror_merge = false;
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        let min_y = out.vertices.iter().map(|v| v.position.y).fold(f32::MAX, f32::min);
        assert!(min_y < -0.9, "panel Y bit must produce negative-y (min_y={min_y})");
    }

    #[test]
    fn apply_mirror_default_is_x_bit() {
        let m = PolyModifier::new(PolyModifierType::Mirror);
        assert_eq!(m.mirror_axis, 1, "default Mirror modifier must be X bit (1), not index 0");
    }

    #[test]
    fn apply_vertex_merge_welds_close_verts_to_average() {
        // Two coincident-ish verts on a triangle: merge by 0.01 must drop vertex count.
        let mut base = HalfEdgeMesh::from_polygons(
            &[
                glam::Vec3::new(0.0, 0.0, 0.0),
                glam::Vec3::new(1.0, 0.0, 0.0),
                glam::Vec3::new(0.5, 1.0, 0.0),
                glam::Vec3::new(0.001, 0.0, 0.0), // near v0 — weld
            ],
            &[vec![0, 1, 2], vec![3, 1, 2]],
        );
        let before = base.vertex_count();
        let mut m = PolyModifier::new(PolyModifierType::VertexMerge);
        m.merge_distance = 0.01;
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        assert!(
            out.vertex_count() < before,
            "Vertex Merge must reduce vert count ({before} → {})",
            out.vertex_count()
        );
        assert!(out.validate().is_ok(), "welded mesh must validate");
    }

    #[test]
    fn apply_mesh_cleanup_modifier_runs_without_panic() {
        let base = cube();
        let mut m = PolyModifier::new(PolyModifierType::MeshCleanup);
        m.cleanup_flatten_folds = true;
        m.cleanup_merge_coplanar = true;
        m.cleanup_dissolve_angle = 5.0;
        // Clean cube may no-op (returns original counts) — just must not panic / corrupt.
        let out = AzraApp::apply_poly_modifiers(&base, &[m], &std::collections::HashMap::new());
        assert!(out.validate().is_ok());
        assert!(out.face_count() > 0);
    }

    #[test]
    fn vertex_merge_and_cleanup_change_stack_hash() {
        let a = PolyModifier::new(PolyModifierType::VertexMerge);
        let mut b = a.clone();
        b.merge_distance = 0.05;
        assert_ne!(
            AzraApp::poly_modifier_stack_hash(&[a]),
            AzraApp::poly_modifier_stack_hash(&[b]),
            "merge_distance must move the stack hash"
        );
        let c = PolyModifier::new(PolyModifierType::MeshCleanup);
        let mut d = c.clone();
        d.cleanup_dissolve_angle = 10.0;
        assert_ne!(
            AzraApp::poly_modifier_stack_hash(&[c]),
            AzraApp::poly_modifier_stack_hash(&[d]),
            "cleanup_dissolve_angle must move the stack hash"
        );
    }

    /// END-TO-END through the MODIFIER STACK (not the kernel directly): a Bevel modifier with
    /// Variable Width on must produce different geometry than the same stack with it off, and the
    /// authored-weight driver must reach the kernel through the sidecar.
    ///
    /// This is the path the owner actually uses — `PolyModifier` fields → `bevel_var_width_spec` →
    /// `bevel_edges_offset_var`. The kernel gates in `core_math` prove the math; only this proves the
    /// app wired it up.
    #[test]
    fn bevel_variable_width_reaches_the_kernel_through_the_stack() {
        use core_math::geometry_channel::AuthoredChannels;
        use core_math::half_edge::{HalfEdgeId, NO_FACE, NO_TWIN};

        // A big box and a small one in ONE mesh: one width cannot serve both.
        let unit = core_math::half_edge::HalfEdgeMesh::new_box(glam::Vec3::ONE);
        let soup = |scale: f32, off: glam::Vec3| -> (Vec<glam::Vec3>, Vec<Vec<usize>>) {
            (
                unit.vertices.iter().map(|v| v.position * scale + off).collect(),
                (0..unit.faces.len() as u32)
                    .map(|f| {
                        unit.face_vertices(core_math::half_edge::FaceId(f))
                            .iter()
                            .map(|v| v.0 as usize)
                            .collect()
                    })
                    .collect(),
            )
        };
        let (mut verts, mut faces) = soup(1.0, glam::Vec3::ZERO);
        let (tv, tf) = soup(0.05, glam::Vec3::new(5.0, 0.0, 0.0));
        let base_i = verts.len();
        verts.extend(tv);
        faces.extend(
            tf.into_iter()
                .map(|f| f.into_iter().map(|i| i + base_i).collect::<Vec<usize>>()),
        );
        let base = core_math::half_edge::HalfEdgeMesh::from_polygons(&verts, &faces);

        let mut m = PolyModifier::new(PolyModifierType::Bevel);
        m.bevel_width = 0.3;
        m.bevel_segments = 1;
        m.bevel_clamp = true;
        let e = std::collections::HashMap::new();
        let auth = AuthoredChannels::default();

        // The measurable: distance from the BIG box's corner to the nearest output vertex is w·√2.
        let corner_w = |mesh: &core_math::half_edge::HalfEdgeMesh| -> f32 {
            mesh.vertices
                .iter()
                .map(|v| (v.position - glam::Vec3::ONE).length())
                .fold(f32::INFINITY, f32::min)
                / std::f32::consts::SQRT_2
        };

        let off = AzraApp::apply_poly_modifiers_tagged(&base, &[m.clone()], &e, &Default::default(), &auth, &std::collections::HashMap::new());
        let w_off = corner_w(&off);
        assert!(
            (w_off - 0.05).abs() < 5e-3,
            "with variable width OFF the tiny box's limit must still govern the big one, got {w_off}"
        );

        let mut on = m.clone();
        on.bevel_var_width = 1; // auto by size
        let out = AzraApp::apply_poly_modifiers_tagged(&base, &[on], &e, &Default::default(), &auth, &std::collections::HashMap::new());
        let w_on = corner_w(&out);
        assert!(
            (w_on - 0.3).abs() < 5e-3,
            "the modifier stack must pass the variable-width spec through: expected 0.3, got {w_on}"
        );
        assert!(out.validate().is_ok(), "stack output must be a valid mesh");

        // AUTHORED WEIGHT IS A PERCENTAGE OF WIDTH AND NEEDS NO MODE (owner 2026-08-11: "the edge
        // weight feature should only have one weight slider, not part of variable [width]; the
        // weight is a percentage of the width"). Painting a weight used to require ticking Variable
        // Width and then choosing "Authored weight" from a combo inside it, so the column was never
        // read with the default settings and painting appeared to do nothing whatsoever.
        //
        // Weight the BIG box's edges to 0.5 and leave `bevel_var_width` at its default 0 — the
        // width must halve anyway. This is the regression that matters: no mode was enabled.
        let weight_big = |w: f32| {
            let mut a_w = AuthoredChannels::default();
            for i in 0..base.half_edges.len() as u32 {
                let he = HalfEdgeId(i);
                if base.he(he).face == NO_FACE || base.he(he).twin == NO_TWIN {
                    continue;
                }
                let a = base.vert(base.he_source(he)).position;
                let b = base.vert(base.he(he).vertex).position;
                if a.x < 2.0 && b.x < 2.0 {
                    // big box only (the tiny one sits at x≈5)
                    a_w.weight_by_edge.insert(AuthoredChannels::edge_key(a, b), w);
                }
            }
            a_w
        };

        let mut graded = m.clone();
        graded.bevel_var_width = 1; // auto by size, so the big box would otherwise reach 0.3
        let half = AzraApp::apply_poly_modifiers_tagged(
            &base, &[graded.clone()], &e, &Default::default(), &weight_big(0.5),
            &std::collections::HashMap::new(),
        );
        let w_half = corner_w(&half);
        assert!(
            (w_half - 0.15).abs() < 5e-3,
            "weight 0.5 must halve the width the driver chose (0.3): got {w_half}"
        );

        // With NO variable-width mode at all the weight must still apply — that is the whole point.
        let plain_weighted = AzraApp::apply_poly_modifiers_tagged(
            &base, &[m.clone()], &e, &Default::default(), &weight_big(0.5),
            &std::collections::HashMap::new(),
        );
        assert!(
            corner_w(&plain_weighted) < w_off - 1e-3,
            "a painted weight must narrow the bevel with bevel_var_width = 0, got {} vs {w_off}",
            corner_w(&plain_weighted)
        );

        // UNPAINTED IS FULL WIDTH, never zero: an empty sidecar must reproduce the un-weighted
        // result exactly, so turning the feature on cannot shrink geometry nobody painted.
        let unweighted = AzraApp::apply_poly_modifiers_tagged(
            &base, &[graded], &e, &Default::default(), &auth,
            &std::collections::HashMap::new(),
        );
        let w_unw = corner_w(&unweighted);
        assert!(
            (w_unw - 0.3).abs() < 5e-3,
            "an unpainted edge must keep the FULL width, got {w_unw}"
        );
    }

    /// LIVE DISPLAY PATH: a default Bevel (no Variable Width, channel = All) must still scale
    /// by painted W through `apply_poly_modifiers_cached`. The tagged helper already did; the
    /// prefix ladder used to pass empty auth and the weights looked dead.
    #[test]
    fn bevel_painted_weight_reaches_the_cached_display_path() {
        use core_math::half_edge::{HalfEdgeId, NO_FACE, NO_TWIN};
        use core_math::poly_mesh::EdgeKey;
        use core_math::scene_graph::NodePayload;

        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        let mut bev = PolyModifier::new(PolyModifierType::Bevel);
        bev.bevel_width = 0.2;
        bev.bevel_segments = 1;
        bev.bevel_clamp = true;
        bev.bevel_var_width = 0; // default — weight must still apply
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                modifiers.push(bev.clone());
            }
        }

        assert!(
            AzraApp::poly_stack_needs_tagged_eval(std::slice::from_ref(&bev)),
            "an enabled Bevel must take the tagged walk so painted W is not dropped"
        );

        let corner_w = |mesh: &HalfEdgeMesh| -> f32 {
            mesh.vertices
                .iter()
                .map(|v| (v.position - Vec3::ONE).length())
                .fold(f32::INFINITY, f32::min)
        };

        let empty = std::collections::HashMap::new();
        let unweighted = app.apply_poly_modifiers_cached(nid, &base, &[bev.clone()], &empty, None);
        let w0 = corner_w(&unweighted);
        // Cube corner (1,1,1) meet sits ~width·√2 from the original corner; we only need a
        // real bevel so the 0.5 weight can shrink it.
        assert!(
            w0 > 0.15 && w0 < 0.4,
            "unpainted cube bevel @0.2 should move the corner, got {w0}"
        );

        // Paint every live cube edge at 0.5 — same store W uses (`CustomData.crease`).
        {
            let cd = app.cache.poly_custom_data.entry(nid).or_default();
            for i in 0..base.half_edges.len() as u32 {
                let he = HalfEdgeId(i);
                if !base.is_edge_live(he) || base.he(he).face == NO_FACE || base.he(he).twin == NO_TWIN
                {
                    continue;
                }
                cd.crease.set(EdgeKey::of_half_edge(&base, he), 0.5);
            }
        }

        let weighted = app.apply_poly_modifiers_cached(nid, &base, &[bev], &empty, None);
        let w1 = corner_w(&weighted);
        assert!(
            (w1 - 0.5 * w0).abs() < 2e-2,
            "W=0.5 must be half of Width through the CACHED display path, got {w1} vs 0.5*{w0}"
        );
        assert!(
            w1 < w0 - 1e-3,
            "painted weight must actually shrink the bevel ({w1} vs {w0})"
        );
    }

    /// Bevel VARIABLE WIDTH must move the display-cache key. Both fields change the output mesh, so
    /// without them in the hash the cached display mesh is reused and the toggle looks like it does
    /// nothing until some unrelated edit forces a rebuild — the exact stale-display trap the Smart
    /// Select tags hit on 2026-08-05.
    #[test]
    fn bevel_variable_width_changes_stack_hash() {
        let base = PolyModifier::new(PolyModifierType::Bevel);
        let mut on = base.clone();
        on.bevel_var_width = 1;
        assert_ne!(
            AzraApp::poly_modifier_stack_hash(&[base.clone()]),
            AzraApp::poly_modifier_stack_hash(&[on.clone()]),
            "enabling variable width must move the stack hash"
        );
        let mut authored = on.clone();
        authored.bevel_var_width = 2;
        assert_ne!(
            AzraApp::poly_modifier_stack_hash(&[on.clone()]),
            AzraApp::poly_modifier_stack_hash(&[authored]),
            "switching driver must move the stack hash"
        );
        let mut floored = on.clone();
        floored.bevel_width_min = 0.01;
        assert_ne!(
            AzraApp::poly_modifier_stack_hash(&[on]),
            AzraApp::poly_modifier_stack_hash(&[floored]),
            "Min Width must move the stack hash"
        );
        let mut merged = base.clone();
        merged.bevel_auto_merge = true;
        assert_ne!(
            AzraApp::poly_modifier_stack_hash(&[base]),
            AzraApp::poly_modifier_stack_hash(&[merged]),
            "Auto Merge Vertices must move the stack hash"
        );
    }

    /// AUTO MERGE VERTICES end-to-end through the modifier stack. A cube carrying a narrow loop-cut
    /// band: with one global limit the band throttles the whole cube; with the toggle on the roomy
    /// edges reach the requested width while the band pins and welds at its own limit.
    ///
    /// Also pins the two dependencies the UI expresses, because a user who cannot see the state has
    /// no way to diagnose it: the toggle needs Clamp Overlap, and Variable Width implies it.
    #[test]
    fn bevel_auto_merge_reaches_the_kernel_through_the_stack() {
        use core_math::geometry_channel::AuthoredChannels;
        use core_math::half_edge::{HalfEdgeId, NO_FACE, NO_TWIN};

        let mut base = core_math::half_edge::HalfEdgeMesh::new_box(glam::Vec3::ONE);
        let he = (0..base.half_edges.len() as u32)
            .map(HalfEdgeId)
            .find(|&h| base.he(h).face != NO_FACE && base.he(h).twin != NO_TWIN)
            .expect("cube has a manifold edge");
        base.loop_cut_multi(he, &[0.97], None);

        let mut m = PolyModifier::new(PolyModifierType::Bevel);
        m.bevel_width = 0.2;
        m.bevel_segments = 1;
        m.bevel_clamp = true;
        let e = std::collections::HashMap::new();
        let auth = AuthoredChannels::default();
        // Width reached at the roomy corner, opposite the band: nearest new vert is w*sqrt(2) away.
        let roomy_w = |mesh: &core_math::half_edge::HalfEdgeMesh| -> f32 {
            mesh.vertices
                .iter()
                .map(|v| (v.position - glam::Vec3::new(-1.0, -1.0, -1.0)).length())
                .fold(f32::INFINITY, f32::min)
                / std::f32::consts::SQRT_2
        };

        let off = AzraApp::apply_poly_modifiers_tagged(&base, &[m.clone()], &e, &Default::default(), &auth, &Default::default());
        let w_off = roomy_w(&off);
        assert!(
            (w_off - 0.03).abs() < 5e-3,
            "with the toggle OFF the narrow band must still throttle the whole cube, got {w_off}"
        );

        let mut on = m.clone();
        on.bevel_auto_merge = true;
        let out = AzraApp::apply_poly_modifiers_tagged(&base, &[on.clone()], &e, &Default::default(), &auth, &Default::default());
        let w_on = roomy_w(&out);
        assert!(
            (w_on - 0.2).abs() < 5e-3,
            "the stack must pass the merge policy through: expected 0.2 at the roomy corner, got {w_on}"
        );
        assert!(out.validate().is_ok(), "stack output must be a valid mesh");

        // Needs Clamp Overlap — with no limit there is no meet position, so it must be inert rather
        // than quietly doing something else.
        let mut noclamp = on.clone();
        noclamp.bevel_clamp = false;
        let a = AzraApp::apply_poly_modifiers_tagged(&base, &[noclamp.clone()], &e, &Default::default(), &auth, &Default::default());
        let mut noclamp_off = noclamp.clone();
        noclamp_off.bevel_auto_merge = false;
        let b = AzraApp::apply_poly_modifiers_tagged(&base, &[noclamp_off], &e, &Default::default(), &auth, &Default::default());
        assert_eq!(
            a.vertices.len(),
            b.vertices.len(),
            "with Clamp Overlap off the toggle must be a no-op"
        );

        // Variable Width implies it: the graded driver already limits per edge, so the roomy corner
        // reaches full width even with `bevel_auto_merge` left false.
        let mut graded = m.clone();
        graded.bevel_var_width = 1;
        graded.bevel_auto_merge = false;
        let g = AzraApp::apply_poly_modifiers_tagged(&base, &[graded], &e, &Default::default(), &auth, &Default::default());
        assert!(
            (roomy_w(&g) - 0.2).abs() < 5e-3,
            "Variable Width must already imply the per-edge limit, got {}",
            roomy_w(&g)
        );
    }

    #[test]
    fn polysculpt_edit_arms_brush_mode() {
        let base = cube();
        let (mut app, nid) = app_with(base);
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                &mut n.payload
            {
                *modifiers = vec![PolyModifier::new(PolyModifierType::Sculpt)];
            }
        }
        app.poly_sculpt_enter_edit(nid, 0);
        assert_eq!(app.tool.work_mode, crate::WorkMode::Sculpt);
        assert_eq!(app.sculpt.poly_edit, Some((nid, 0)));
        assert!(app.poly_sculpt_radius() > 0.0);
        app.poly_sculpt_leave_edit();
        assert!(app.sculpt.poly_edit.is_none());
        assert_eq!(app.tool.work_mode, crate::WorkMode::Object);
    }

    #[test]
    fn polysculpt_stored_deltas_move_verts() {
        let base = cube();
        let mut sc = PolyModifier::new(PolyModifierType::Sculpt);
        sc.sculpt_deltas = vec![(0, Vec3::Y * 0.25)];
        sc.sculpt_delta_gen = 1;
        let out = AzraApp::apply_poly_modifiers(&base, &[sc], &Default::default());
        let d = out.vertices[0].position - base.vertices[0].position;
        assert!(
            (d - Vec3::Y * 0.25).length() < 1e-5,
            "stored delta must add onto vert 0, got {d}"
        );
        assert_eq!(out.vertex_count(), base.vertex_count());
    }

    #[test]
    fn polysculpt_slot_is_never_a_prefix_hit() {
        use core_math::scene_graph::NodePayload;
        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        let mut remesh = PolyModifier::new(PolyModifierType::Remesh);
        remesh.remesh_resolution = 16;
        remesh.remesh_smooth = false;
        remesh.remesh_project = 0.0;
        let mut sc = PolyModifier::new(PolyModifierType::Sculpt);
        sc.sculpt_deltas = vec![(0, Vec3::Y * 0.1)];
        sc.sculpt_delta_gen = 1;
        let stack = [remesh, sc.clone()];
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                *modifiers = stack.to_vec();
            }
        }
        crate::poly::remesh::reset_remesh_full_eval_count();
        let empty = std::collections::HashMap::new();
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack, &empty, None);
        let first_evals = crate::poly::remesh::remesh_full_eval_count();
        assert!(first_evals >= 1, "cold remesh must eval once");
        {
            let body = app.poly_prefix_cache.get(&nid).expect("prefix body");
            assert!(
                body.entries.get(1).and_then(|e| e.as_ref()).is_none(),
                "C1: PolySculpt slot must never store a prefix (stale full-hit is impossible)"
            );
            assert!(
                body.entries.get(0).and_then(|e| e.as_ref()).is_some(),
                "Remesh prefix must stay so strokes do not re-pay remesh"
            );
        }
        let mut sc2 = sc;
        sc2.sculpt_deltas = vec![(0, Vec3::Y * 0.3)];
        sc2.sculpt_delta_gen = 2;
        let stack2 = [stack[0].clone(), sc2];
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack2, &empty, None);
        let second_evals = crate::poly::remesh::remesh_full_eval_count();
        assert_eq!(
            second_evals, first_evals,
            "delta edits must not re-run remesh (got {second_evals} after {first_evals})"
        );
    }

    #[test]
    fn polysculpt_delta_gen_changes_the_stack_hash() {
        let mut m = PolyModifier::new(PolyModifierType::Sculpt);
        let h0 = AzraApp::poly_modifier_stack_hash(std::slice::from_ref(&m));
        m.sculpt_delta_gen = 1;
        assert_ne!(
            h0,
            AzraApp::poly_modifier_stack_hash(std::slice::from_ref(&m)),
            "sculpt_delta_gen must be in the stack hash"
        );
    }

    #[test]
    fn polysculpt_working_mesh_leaves_prefix_arc_unmoved() {
        use core_math::scene_graph::NodePayload;
        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        let mut remesh = PolyModifier::new(PolyModifierType::Remesh);
        remesh.remesh_resolution = 16;
        remesh.remesh_smooth = false;
        remesh.remesh_project = 0.0;
        let sc = PolyModifier::new(PolyModifierType::Sculpt);
        let stack = [remesh, sc];
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                *modifiers = stack.to_vec();
            }
        }
        let empty = std::collections::HashMap::new();
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack, &empty, None);
        let prefix_ptr = {
            let body = app.poly_prefix_cache.get(&nid).expect("prefix");
            let e = body.entries[0].as_ref().expect("remesh prefix");
            std::sync::Arc::as_ptr(&e.mesh) as usize
        };
        let working_ptr = app
            .poly_sculpt_working
            .get(&nid)
            .map(|w| w.prefix_ptr)
            .expect("working");
        assert_eq!(prefix_ptr, working_ptr);
        let moved = app.poly_sculpt_stamp(nid, glam::Vec3::ZERO, 2.0, glam::Vec3::Y * 0.05);
        assert!(!moved.is_empty(), "stamp must hit cube remesh verts");
        let prefix_ptr_after = {
            let body = app.poly_prefix_cache.get(&nid).expect("prefix");
            let e = body.entries[0].as_ref().expect("remesh prefix");
            std::sync::Arc::as_ptr(&e.mesh) as usize
        };
        assert_eq!(
            prefix_ptr, prefix_ptr_after,
            "C1: remesh prefix Arc must not move on a stroke"
        );
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        assert!(w.display_verts.iter().any(|v| (v[1] - 0.0).abs() > 1e-4));
    }

    #[test]
    fn polysculpt_stamp_323k_patches_display_and_records_cost() {
        // 402×402 quads = 323_208 tris — the 323k body the plan names.
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, 402, 402);
        let tris = base.face_count() * 2;
        assert!(
            (320_000..=330_000).contains(&tris),
            "fixture must be the 323k body, got {tris} tris"
        );
        let (mut app, nid) = app_with(base.clone());
        let sc = PolyModifier::new(PolyModifierType::Sculpt);
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                &mut n.payload
            {
                *modifiers = vec![sc];
            }
        }
        let empty = std::collections::HashMap::new();
        let stack = [PolyModifier::new(PolyModifierType::Sculpt)];
        let base_ptr = {
            match &app.scene.scene_graph.get(nid).unwrap().payload {
                core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => {
                    std::sync::Arc::as_ptr(mesh) as usize
                }
                _ => panic!("poly"),
            }
        };
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack, &empty, None);
        let before = app
            .poly_sculpt_working
            .get(&nid)
            .map(|w| w.display_verts.clone())
            .expect("working display");
        let moved = app.poly_sculpt_stamp(nid, glam::Vec3::ZERO, 0.08, glam::Vec3::Y * 0.02);
        assert!(!moved.is_empty(), "stamp must hit the plane center");
        let w = app.poly_sculpt_working.get(&nid).expect("working");
        let changed = w
            .display_verts
            .iter()
            .zip(before.iter())
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| (x - y).abs() > 1e-8))
            .count();
        assert!(changed > 0, "C2: display bytes must update");
        let base_ptr_after = {
            match &app.scene.scene_graph.get(nid).unwrap().payload {
                core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => {
                    std::sync::Arc::as_ptr(mesh) as usize
                }
                _ => panic!("poly"),
            }
        };
        assert_eq!(base_ptr, base_ptr_after, "base Arc unmoved");
        println!(
            "S0b 323k stamp: {} verts  stamp={:.3}ms  query_ov={:.3}ms  query_grid={:.3}ms",
            moved.len(),
            w.last_stamp_ms,
            w.last_query_ov_ms,
            w.last_query_grid_ms
        );
        assert!(
            w.last_stamp_ms > 0.0,
            "frame cost must be measured (got {}ms)",
            w.last_stamp_ms
        );
        // Grid must not be slower than a full O(V) scan on this body.
        assert!(
            w.last_query_grid_ms <= w.last_query_ov_ms + 0.5,
            "grid {:.3}ms vs O(V) {:.3}ms — keep the cheaper query",
            w.last_query_grid_ms,
            w.last_query_ov_ms
        );
    }

    #[test]
    fn polysculpt_azra_sculptlayer_draw_moves_along_normal() {
        let _ = std::env::var("AZRA_SCULPTLAYER");
        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                &mut n.payload
            {
                *modifiers = vec![PolyModifier::new(PolyModifierType::Sculpt)];
            }
        }
        let empty = std::collections::HashMap::new();
        let stack = [PolyModifier::new(PolyModifierType::Sculpt)];
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack, &empty, None);
        let hit = glam::Vec3::new(0.0, 1.0, 0.0);
        let sample = core_math::sculpt_layer::StrokeSample::new(hit, glam::Vec3::Y, 1.0);
        let dabs = AzraApp::poly_sculpt_dabs_from_samples(&[sample], 1.5, 1.0, glam::Vec3::ZERO);
        // PR-5 contract: the working mesh is truth mid-stroke; the delta sidecar is
        // written ONCE at commit_episode (the document choke).
        app.begin_poly_sculpt_episode(nid, Some(0));
        let n = app.poly_sculpt_apply_dabs(
            nid,
            &dabs,
            core_math::voxel_sculpt::VoxelBrush::Draw,
            1.0,
        );
        assert!(n > 0, "Draw dab must move verts");
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let lifted = w
            .mesh
            .vertices
            .iter()
            .filter(|v| v.position.y > 1.0 + 1e-4)
            .count();
        assert!(lifted > 0, "Draw along +Y must lift the top");
        app.commit_episode();
        let mods = match &app.scene.scene_graph.get(nid).unwrap().payload {
            core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } => modifiers,
            _ => panic!("poly"),
        };
        assert!(
            mods[0].sculpt_deltas.iter().any(|(_, d)| d.y > 0.0),
            "stored deltas must point along the dab"
        );
    }

    #[test]
    fn polysculpt_producer_uses_same_interpolator() {
        let mut app = AzraApp::new_headless();
        app.sdf_stroke_interpolator.reset();
        app.sdf_stroke_interpolator.spacing = 0.10;
        let a = core_math::sculpt_layer::StrokeSample::new(glam::Vec3::ZERO, glam::Vec3::Y, 0.5);
        let b = core_math::sculpt_layer::StrokeSample::new(
            glam::Vec3::X * 1.0,
            glam::Vec3::Y,
            1.0,
        );
        let first = app.sdf_stroke_interpolator.push(a, 0.2);
        let more = app.sdf_stroke_interpolator.push(b, 0.2);
        assert_eq!(first.len(), 1, "first sample emits the start dab");
        assert!(
            more.len() > 1,
            "spacing 0.10 × r=0.2 over 1.0 units must emit multiple dabs (got {})",
            more.len()
        );
        assert!(
            more.iter().any(|s| (s.pressure - 1.0).abs() < 1e-5)
                && first[0].pressure == 0.5,
            "pressure rides the same StrokeSample as the voxel producer"
        );
        let dabs = AzraApp::poly_sculpt_dabs_from_samples(&more, 0.2, 1.0, glam::Vec3::X);
        assert_eq!(dabs.len(), more.len());
        assert!(dabs.iter().all(|d| (d.radius - 0.2).abs() < 1e-5));
    }

    #[test]
    fn polysculpt_rebase_survives_remesh_res_change() {
        use core_math::scene_graph::NodePayload;
        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        let mut remesh = PolyModifier::new(PolyModifierType::Remesh);
        remesh.remesh_resolution = 16;
        remesh.remesh_smooth = false;
        remesh.remesh_project = 0.0;
        let mut sc = PolyModifier::new(PolyModifierType::Sculpt);
        sc.sculpt_deltas = vec![(0, glam::Vec3::Y * 0.2)];
        sc.sculpt_delta_gen = 1;
        let stack = [remesh.clone(), sc.clone()];
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                *modifiers = stack.to_vec();
            }
        }
        let empty = std::collections::HashMap::new();
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack, &empty, None);
        remesh.remesh_resolution = 24;
        let stack2 = [remesh, sc];
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                *modifiers = stack2.to_vec();
            }
        }
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack2, &empty, None);
        let mods = match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { modifiers, .. } => modifiers,
            _ => panic!("poly"),
        };
        assert!(
            !mods[1].sculpt_deltas.is_empty(),
            "deltas must re-splat onto the new remesh"
        );
    }

    #[test]
    fn polysculpt_rebase_reports_full_orphan() {
        let old = cube();
        let new = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
        let far = vec![(0, glam::Vec3::new(100.0, 100.0, 100.0))];
        let (out, orphans) = crate::poly::sculpt::rebase_deltas(&old, &far, &new);
        assert!(out.is_empty());
        assert!(orphans >= 1, "a 100-unit miss must be an orphan");
    }

    #[test]
    fn polysculpt_draw_matches_surface_kernel() {
        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                &mut n.payload
            {
                *modifiers = vec![PolyModifier::new(PolyModifierType::Sculpt)];
            }
        }
        let empty = std::collections::HashMap::new();
        let stack = [PolyModifier::new(PolyModifierType::Sculpt)];
        let _ = app.apply_poly_modifiers_cached(nid, &base, &stack, &empty, None);
        let hit = glam::Vec3::new(0.0, 1.0, 0.0);
        let sample = core_math::sculpt_layer::StrokeSample::new(hit, glam::Vec3::Y, 1.0);
        let dabs = AzraApp::poly_sculpt_dabs_from_samples(&[sample], 1.5, 1.0, glam::Vec3::ZERO);
        let before: Vec<glam::Vec3> = app
            .poly_sculpt_working
            .get(&nid)
            .unwrap()
            .mesh
            .vertices
            .iter()
            .map(|v| v.position)
            .collect();
        let mut packed: Vec<[f32; 8]> = before
            .iter()
            .map(|p| [p.x, p.y, p.z, 0.0, 1.0, 0.0, 0.0, 0.0])
            .collect();
        let planes = vec![None; dabs.len()];
        crate::voxel_surface_sculpt::displace_verts_for_dabs(
            &mut packed,
            &dabs,
            core_math::voxel_sculpt::VoxelBrush::Draw,
            1.0,
            core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS,
            core_math::voxel_sculpt::FalloffPreset::Smooth,
            &planes,
            None,
            0.1,
            None,
            None,
        );
        let _ = app.poly_sculpt_apply_dabs(
            nid,
            &dabs,
            core_math::voxel_sculpt::VoxelBrush::Draw,
            1.0,
        );
        let after = &app.poly_sculpt_working.get(&nid).unwrap().mesh;
        for i in 0..before.len() as u32 {
            if !after.is_vert_live(core_math::half_edge::VertexId(i)) {
                continue;
            }
            let got = after.vert(core_math::half_edge::VertexId(i)).position;
            let exp = glam::Vec3::new(packed[i as usize][0], packed[i as usize][1], packed[i as usize][2]);
            assert!(
                (got - exp).length() < 1e-4,
                "vert {i}: PolySculpt {got} != surface kernel {exp}"
            );
        }
    }

    #[test]
    fn surface_sculpt_enter_keeps_remesh_surface() {
        use core_math::scene_graph::NodePayload;
        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        let mut remesh = PolyModifier::new(PolyModifierType::Remesh);
        remesh.remesh_resolution = 16;
        remesh.remesh_smooth = false;
        remesh.remesh_project = 0.0;
        remesh.remesh_collapse = false;
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                *modifiers = vec![remesh.clone()];
            }
        }
        let empty = std::collections::HashMap::new();
        let eval = app.apply_poly_modifiers_cached(
            nid,
            &base,
            std::slice::from_ref(&remesh),
            &empty,
            None,
        );
        let remesh_verts = eval.vertex_count();
        let remesh_faces = eval.face_count();
        assert!(
            remesh_verts > 8 && remesh_faces > 6,
            "fixture remesh must densify the cube, got V={remesh_verts} F={remesh_faces}"
        );
        app.begin_surface_sculpt(nid);
        let body = match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, modifiers, .. } => (mesh, modifiers),
            _ => panic!("poly"),
        };
        assert_eq!(
            body.0.vertex_count(),
            remesh_verts,
            "enter Sculpt must keep the remesh surface, not drop back to the cube"
        );
        assert_eq!(body.0.face_count(), remesh_faces);
        assert!(
            body.1
                .iter()
                .filter(|m| m.mod_type == PolyModifierType::Remesh)
                .all(|m| !m.enabled),
            "Remesh must turn off on enter so a later sync cannot rebuild/hide the surface"
        );
        let w = app.poly_sculpt_working.get(&nid).expect("working");
        assert_eq!(w.mesh.vertex_count(), remesh_verts);
    }

    #[test]
    fn surface_sculpt_action_on_remeshed_cube_arms_brushes() {
        use core_math::scene_graph::NodePayload;
        let base = cube();
        let (mut app, nid) = app_with(base.clone());
        let mut remesh = PolyModifier::new(PolyModifierType::Remesh);
        remesh.remesh_resolution = 16;
        remesh.remesh_smooth = false;
        remesh.remesh_project = 0.0;
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                *modifiers = vec![remesh];
            }
        }
        app.begin_surface_sculpt(nid);
        assert_eq!(app.tool.work_mode, crate::WorkMode::Sculpt);
        assert_eq!(app.sculpt.poly_edit.map(|(n, _)| n), Some(nid));
        let mods = match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { modifiers, .. } => modifiers,
            _ => panic!("poly"),
        };
        assert!(
            mods.iter()
                .all(|m| m.mod_type != PolyModifierType::Sculpt),
            "Surface Sculpt must not add a Sculpt modifier"
        );
        assert!(
            mods.iter()
                .filter(|m| m.mod_type == PolyModifierType::Remesh)
                .all(|m| !m.enabled),
            "enter Surface sculpt must bake Remesh so the remesh surface cannot vanish"
        );
        assert!(!app.tool.current.is_gizmo());
        assert!(!app.tool.gizmo_armed);
        let hit = glam::Vec3::new(0.0, 1.0, 0.0);
        let sample = core_math::sculpt_layer::StrokeSample::new(hit, glam::Vec3::Y, 1.0);
        let dabs = AzraApp::poly_sculpt_dabs_from_samples(&[sample], 1.5, 1.0, glam::Vec3::ZERO);
        let n = app.poly_sculpt_apply_dabs(
            nid,
            &dabs,
            core_math::voxel_sculpt::VoxelBrush::Draw,
            1.0,
        );
        assert!(n > 0, "Draw on remeshed cube must move verts, got {n}");
        let body = match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, modifiers, .. } => (mesh, modifiers),
            _ => panic!("poly"),
        };
        assert!(
            body.0.vertices.iter().any(|v| v.position.y > 1.0 + 1e-4),
            "direct Surface Sculpt must write the remeshed surface into the body"
        );
        assert!(
            body.1
                .iter()
                .filter(|m| m.mod_type == PolyModifierType::Remesh)
                .all(|m| !m.enabled),
            "Remesh must turn off so it does not rebuild under the stroke"
        );
    }

    #[test]
    fn surface_sculpt_direct_on_cube_writes_base_no_modifier() {
        let base = cube();
        let (mut app, nid) = app_with(base);
        app.begin_surface_sculpt(nid);
        let mods = match &app.scene.scene_graph.get(nid).unwrap().payload {
            core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } => modifiers,
            _ => panic!("poly"),
        };
        assert!(mods.is_empty(), "plain cube Surface Sculpt adds no modifier");
        let hit = glam::Vec3::new(0.0, 1.0, 0.0);
        let sample = core_math::sculpt_layer::StrokeSample::new(hit, glam::Vec3::Y, 1.0);
        let dabs = AzraApp::poly_sculpt_dabs_from_samples(&[sample], 1.5, 1.0, glam::Vec3::ZERO);
        // PR-5 contract: authored mesh is written ONCE, at commit (never per dab).
        app.begin_poly_sculpt_episode(nid, None);
        let n = app.poly_sculpt_apply_dabs(
            nid,
            &dabs,
            core_math::voxel_sculpt::VoxelBrush::Draw,
            1.0,
        );
        assert!(n > 0, "Draw on cube must move verts, got {n}");
        app.commit_episode();
        let mesh = match &app.scene.scene_graph.get(nid).unwrap().payload {
            core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => panic!("poly"),
        };
        assert!(
            mesh.vertices.iter().any(|v| v.position.y > 1.0 + 1e-4),
            "direct stroke must land on the authored mesh"
        );
    }

    #[test]
    fn enter_sculpt_mode_on_poly_cube_starts_surface_not_voxel() {
        let base = cube();
        let (mut app, nid) = app_with(base);
        app.enter_sculpt_mode();
        assert_eq!(app.tool.work_mode, crate::WorkMode::Sculpt);
        assert_eq!(app.sculpt.poly_edit.map(|(n, _)| n), Some(nid));
        assert!(app.sculpt.voxel.is_none(), "must not spawn a voxel subject");
    }

    #[test]
    fn surface_sculpt_on_selection_uses_active_poly() {
        let base = cube();
        let (mut app, nid) = app_with(base);
        app.begin_surface_sculpt_on_selection();
        assert_eq!(app.sculpt.poly_edit.map(|(n, _)| n), Some(nid));
    }

    #[test]
    fn surface_sculpt_lmb_is_brush_not_paint_select() {
        let base = cube();
        let (mut app, nid) = app_with(base);
        app.modeling_context = crate::ModelingContext::Polygonal;
        app.poly_work_mode = crate::PolyWorkMode::Face;
        app.begin_surface_sculpt(nid);
        assert_eq!(
            app.poly_work_mode,
            crate::PolyWorkMode::Object,
            "leave Face/Edge/Vertex so paint-select is not the default LMB"
        );
        assert!(app.sculpt_brush_owns_lmb());
        assert!(app.tool.current.is_idle());
        // Even if the user flips back to Face while the sculpt room is live, LMB stays the brush.
        app.poly_work_mode = crate::PolyWorkMode::Face;
        let would_arm_paint = app.modeling_context == crate::ModelingContext::Polygonal
            && app.poly_work_mode != crate::PolyWorkMode::Object
            && app.tool.current.is_idle()
            && !app.sculpt_brush_owns_lmb();
        assert!(
            !would_arm_paint,
            "Sculpt-room LMB must not start poly paint-select"
        );
    }

    #[test]
    fn switching_to_object_face_edge_vertex_drops_sculpt() {
        let modes = [
            crate::PolyWorkMode::Object,
            crate::PolyWorkMode::Face,
            crate::PolyWorkMode::Edge,
            crate::PolyWorkMode::Vertex,
        ];
        for mode in modes {
            let base = cube();
            let (mut app, nid) = app_with(base);
            app.modeling_context = crate::ModelingContext::Polygonal;
            app.begin_surface_sculpt(nid);
            assert!(app.sculpt_brush_owns_lmb(), "{mode:?}: sculpt must own LMB");
            app.poly_switch_work_mode(mode);
            assert!(
                app.sculpt.poly_edit.is_none(),
                "{mode:?}: Surface sculpt must drop"
            );
            assert_ne!(
                app.tool.work_mode,
                crate::WorkMode::Sculpt,
                "{mode:?}: must leave the sculpt room"
            );
            assert!(
                !app.sculpt_brush_owns_lmb(),
                "{mode:?}: LMB must be selection, not the brush"
            );
            assert!(
                app.tool.mode.is_select(),
                "{mode:?}: default tool must be Select"
            );
            assert_eq!(app.poly_tool, crate::PolyTool::Idle);
            assert_eq!(app.poly_work_mode, mode);
        }
    }

    #[test]
    fn polysculpt_modifier_undo_reverts_one_stroke() {
        use core_math::scene_graph::NodePayload;
        let base = cube();
        let (mut app, nid) = app_with(base);
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                *modifiers = vec![PolyModifier::new(PolyModifierType::Sculpt)];
            }
        }
        app.poly_sculpt_enter_edit(nid, 0);
        let dab = |p: glam::Vec3, n: glam::Vec3| {
            let s = core_math::sculpt_layer::StrokeSample::new(p, n, 1.0);
            AzraApp::poly_sculpt_dabs_from_samples(&[s], 1.5, 1.0, glam::Vec3::ZERO)
        };
        app.begin_poly_sculpt_episode(nid, Some(0));
        assert!(
            app.poly_sculpt_apply_dabs(
                nid,
                &dab(glam::Vec3::Y, glam::Vec3::Y),
                core_math::voxel_sculpt::VoxelBrush::Draw,
                1.0,
            ) > 0
        );
        app.commit_episode();
        app.begin_poly_sculpt_episode(nid, Some(0));
        assert!(
            app.poly_sculpt_apply_dabs(
                nid,
                &dab(glam::Vec3::X, glam::Vec3::X),
                core_math::voxel_sculpt::VoxelBrush::Draw,
                1.0,
            ) > 0
        );
        app.commit_episode();
        let mods = |app: &AzraApp| match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { modifiers, .. } => modifiers[0].sculpt_deltas.clone(),
            _ => panic!("poly"),
        };
        let two = mods(&app);
        assert!(two.iter().any(|(_, d)| d.y > 0.0), "stroke 1 must lift +Y");
        assert!(two.iter().any(|(_, d)| d.x > 0.0), "stroke 2 must push +X");
        app.tool.work_mode = crate::WorkMode::Sculpt;
        app.undo();
        let one = mods(&app);
        assert!(
            one.iter().any(|(_, d)| d.y > 0.0),
            "Ctrl+Z must keep the first stroke"
        );
        assert!(
            one.iter().all(|(_, d)| d.x <= 1e-5),
            "Ctrl+Z must drop only the last stroke, not the whole layer"
        );
    }

    #[test]
    fn surface_sculpt_direct_undo_reverts_one_stroke() {
        use core_math::scene_graph::NodePayload;
        let base = cube();
        let (mut app, nid) = app_with(base);
        app.begin_surface_sculpt(nid);
        let dab = |p: glam::Vec3, n: glam::Vec3| {
            let s = core_math::sculpt_layer::StrokeSample::new(p, n, 1.0);
            AzraApp::poly_sculpt_dabs_from_samples(&[s], 1.5, 1.0, glam::Vec3::ZERO)
        };
        app.begin_poly_sculpt_episode(nid, None);
        assert!(
            app.poly_sculpt_apply_dabs(
                nid,
                &dab(glam::Vec3::Y, glam::Vec3::Y),
                core_math::voxel_sculpt::VoxelBrush::Draw,
                1.0,
            ) > 0
        );
        app.commit_episode();
        app.begin_poly_sculpt_episode(nid, None);
        assert!(
            app.poly_sculpt_apply_dabs(
                nid,
                &dab(glam::Vec3::X, glam::Vec3::X),
                core_math::voxel_sculpt::VoxelBrush::Draw,
                1.0,
            ) > 0
        );
        app.commit_episode();
        let pos = |app: &AzraApp| match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, .. } => {
                mesh.vertices.iter().map(|v| v.position).collect::<Vec<_>>()
            }
            _ => panic!("poly"),
        };
        let two = pos(&app);
        assert!(two.iter().any(|p| p.y > 1.0 + 1e-4));
        assert!(two.iter().any(|p| p.x > 1.0 + 1e-4));
        app.tool.work_mode = crate::WorkMode::Sculpt;
        app.undo();
        let one = pos(&app);
        assert!(
            one.iter().any(|p| p.y > 1.0 + 1e-4),
            "first stroke must remain on the mesh"
        );
        assert!(
            one.iter().all(|p| p.x <= 1.0 + 1e-4),
            "Ctrl+Z must not wipe every Surface stroke"
        );
    }
}
