//! Baked mesh-AO poll wiring (AO_BAKE_PLAN.md Phase A dispatch site).
//!
//! Runs from `poll_background_tasks` ONLY (Frame Ordering Contract): it drains
//! finished CPU AO bakes, uploads the resulting per-vertex AO into the per-body
//! GPU side-buffer, and services explicit "Rebake AO" requests (queuing the rayon
//! bake job once the body's CPU triangle mesh is resident — kicking the on-demand
//! SDF-display readback when it is not). Never per-frame, never in a panel callback.

use core_math::scene_graph::{MeshCacheId, NodeId, NodePayload};
use render::ao_bake::{AoBakeParams, AoRequest};

impl super::AzraApp {
    /// Resolve the MeshCacheId + current content stamp for a body node, if it has one.
    pub(crate) fn ao_body_cache_id(&self, nid: NodeId) -> Option<MeshCacheId> {
        let node = self.scene.scene_graph.get(nid)?;
        match &node.payload {
            NodePayload::SdfBody { mesh_cache_id, .. } => *mesh_cache_id,
            NodePayload::ImportedMesh { mesh_cache_id, .. } => Some(*mesh_cache_id),
            NodePayload::VoxelBody { mesh_cache_id, .. } => *mesh_cache_id,
            NodePayload::PolygonalBody { mesh_cache_id, .. } => *mesh_cache_id,
            NodePayload::NurbsBody { mesh_cache_id, .. }
            | NodePayload::NurbsSolid { mesh_cache_id, .. } => *mesh_cache_id,
            _ => None,
        }
    }

    /// Default occlusion radius for a body ≈ half its mesh AABB diagonal
    /// (AO_BAKE_PLAN.md owner knob). Falls back to 1.0 when no bounds are known.
    fn ao_default_radius(&self, cid: MeshCacheId) -> f32 {
        if let Some((mn, mx)) = self.cache.pt_accel.local_aabb(cid) {
            let diag = (mx - mn).length();
            if diag.is_finite() && diag > 1e-4 {
                return diag * 0.5;
            }
        }
        1.0
    }

    /// The params the next bake for this body will use (panel-set radius/quality,
    /// else the body-diagonal default radius + Medium quality).
    pub(crate) fn ao_params_for(&self, cid: MeshCacheId) -> AoBakeParams {
        if let Some(p) = self.cache.ao_bake_params.get(&cid) {
            return *p;
        }
        AoBakeParams { radius: self.ao_default_radius(cid), ..Default::default() }
    }

    /// User pressed "Rebake AO" for a body (panel action). One-shot: records the
    /// request; the poll phase does the actual work (Frame Ordering Contract — the
    /// panel never kicks the readback or the bake itself).
    pub(crate) fn request_ao_bake(&mut self, cid: MeshCacheId) {
        if !self.cache.ao_bake_requests.contains(&cid) {
            self.cache.ao_bake_requests.push(cid);
        }
    }

    /// Whether a body's baked AO is STALE vs its current mesh content (drives the
    /// panel stale badge). True only when a bake exists but no longer matches.
    pub(crate) fn ao_is_stale(&self, cid: MeshCacheId) -> bool {
        let Some(stamp) = self.cache.mesh_cache.pt_stamp(cid) else {
            // No mesh yet → a prior bake (if any) can't be validated; treat as stale.
            return self.cache.ao_bake.has_bake(cid);
        };
        self.cache.ao_bake.has_bake(cid) && !self.cache.ao_bake.is_current(cid, stamp)
    }

    /// Poll-phase AO bake service: drain finished bakes (upload side-buffer) + kick
    /// pending explicit requests. Called from `poll_background_tasks`.
    pub(crate) fn poll_ao_bake(&mut self) {
        // 1) Drain finished CPU bakes. The per-vertex AO now lives in the cache; a
        //    RENDER dirty re-runs the deferred shade so the (Phase-B) GPU consumer
        //    picks it up. The GPU upload of the per-body AO side-buffer is the Phase-B
        //    raster-consumption slice (see AO_BAKE handoff note) — the CPU cache and
        //    its staleness signal (consumed by the panel) are complete here.
        if self.cache.ao_bake.poll() {
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }

        // 2) Bound cache growth: drop AO for meshes the mesh_cache no longer owns.
        let mesh_cache = &self.cache.mesh_cache;
        self.cache.ao_bake.retain_live(|id| mesh_cache.get(id).is_some());

        // 3) Service explicit requests (one per poll — the bake is background but the
        //    source clone / readback kick is frame-thread work; Frame Budget Rule).
        if self.cache.ao_bake_requests.is_empty() {
            return;
        }
        let cid = self.cache.ao_bake_requests[0];
        // If the body's CPU triangle mesh is resident, queue the bake now.
        let stamp = self.cache.mesh_cache.pt_stamp(cid);
        if let (Some(stamp), Some((verts, indices))) =
            (stamp, self.cache.mesh_cache.pt_source_data(cid))
        {
            let params = self.ao_params_for(cid);
            match self.cache.ao_bake.request(cid, stamp, verts, indices, params) {
                AoRequest::Queued => {
                    self.cache.ao_bake_requests.remove(0);
                }
                AoRequest::Noop => {
                    // Already current/in-flight — request satisfied.
                    self.cache.ao_bake_requests.remove(0);
                }
                AoRequest::RejectedTooDense => {
                    self.toast(
                        "AO bake: mesh too dense (>1.2M tris) — lower Grid (≤1024) to bake AO",
                    );
                    self.cache.ao_bake_requests.remove(0);
                }
                AoRequest::Empty => {
                    self.cache.ao_bake_requests.remove(0);
                }
            }
            return;
        }

        // No CPU mesh yet: kick the on-demand SDF-display readback for the owning
        // body (the same path the PT pump uses) and LEAVE the request queued so the
        // next poll (after the readback lands into mesh_cache) queues the bake. The
        // readback machinery is PathTrace/RT-gated today; invalidating the snapshot
        // makes the pump re-read whenever it runs. For raster-only users whose body
        // has no CPU mesh at all, the request waits until one exists (editable/
        // imported/voxel bodies always do; brick-resident SDF needs the snapshot).
        if let Some(nid) = self.body_nid_for_cache_id(cid) {
            self.invalidate_pt_sdf_snapshot(nid);
        }
        // Avoid an unbounded spin if no CPU mesh will ever arrive in raster-only mode:
        // drop the request after it has waited without a mesh, so it does not pin the
        // queue forever. The user can press Rebake again; a toast tells them why.
        // (One retry window: the request stays for this poll's readback kick, then is
        // dropped next poll if still no stamp.)
        if stamp.is_none() && !self.render_readback_gate_open() {
            self.toast(
                "AO bake: body mesh not CPU-resident — enable Path Trace/RT once to snapshot it",
            );
            self.cache.ao_bake_requests.remove(0);
        }
    }

    /// True when the SDF-display readback path is actually dispatched (PathTrace or
    /// RT shadows). Mirrors `pt_scene.rs`'s `update_pt_accel` residency gate — used
    /// to decide whether a queued AO request can expect a CPU mesh to arrive.
    fn render_readback_gate_open(&self) -> bool {
        self.render.shading_mode == crate::ShadingMode::PathTrace || self.rt_shadows_active()
    }

    /// Reverse lookup: the body NodeId owning a MeshCacheId (for the readback kick).
    fn body_nid_for_cache_id(&self, cid: MeshCacheId) -> Option<NodeId> {
        for (nid, node) in self.scene.scene_graph.all_nodes() {
            let owns = match &node.payload {
                NodePayload::SdfBody { mesh_cache_id, .. } => *mesh_cache_id == Some(cid),
                NodePayload::ImportedMesh { mesh_cache_id, .. } => *mesh_cache_id == cid,
                NodePayload::VoxelBody { mesh_cache_id, .. } => *mesh_cache_id == Some(cid),
                NodePayload::PolygonalBody { mesh_cache_id, .. } => *mesh_cache_id == Some(cid),
                NodePayload::NurbsBody { mesh_cache_id, .. }
                | NodePayload::NurbsSolid { mesh_cache_id, .. } => *mesh_cache_id == Some(cid),
                _ => false,
            };
            if owns {
                return Some(nid);
            }
        }
        None
    }
}
