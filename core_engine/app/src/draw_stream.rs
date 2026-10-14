//! AZRA_MASTER_ROADMAP Phase B.1 — structural draw stream + draw list.
//!
//! Scene walk → list of drawable NodeIds. Rebuilt on SCENE/MESH dirty only;
//! camera motion reuses the list. Visibility is filtered at use time
//! (outliner eye toggle is RENDER-only).
//!
//! **Structural draw list** (B.1 extension / C.9 prerequisite): `structural_draw_calls`
//! caches transform-derived InstanceData + materials + kinds. Rebuild on SCENE|MESH,
//! selection-outline change, or poly LOD switch. Per-frame work is the cull mask only
//! (frustum + occlusion) — no gather, no sort, no 800 KB content-hash rebuild on orbit.
//! See `dispatch_raster_pass` in `raster_gbuffer.rs`.
//!
//! C.5: also rebuilds `cache.body_index_map` — dense 0-based object IDs written to
//! `gbuf_motion.r` / `selected_body_index` (R32Float; exact ints past f16's 2048).

use core_math::scene_graph::{NodeId, NodePayload};

use crate::AzraApp;

impl AzraApp {
    /// Rebuild the structural candidate list (which nodes *can* draw).
    /// Call only when `draw_stream_stale` or the list is empty.
    pub(crate) fn rebuild_structural_draw_stream(&mut self) {
        let mut out: Vec<NodeId> = Vec::with_capacity(64);
        for (nid, node) in self.scene.scene_graph.all_nodes() {
            match &node.payload {
                NodePayload::SdfBody { .. } => {
                    // Nested bodies are subsumed into the parent bake — not standalone draws.
                    if self.scene.scene_graph.parent_body(nid).is_some() {
                        continue;
                    }
                    if !self.scene.scene_graph.body_has_renderable_content(nid) {
                        continue;
                    }
                    out.push(nid);
                }
                NodePayload::ImportedMesh { .. }
                | NodePayload::VoxelBody { .. }
                | NodePayload::PolygonalBody { .. }
                | NodePayload::NurbsBody { .. } => {
                    out.push(nid);
                }
                _ => {}
            }
        }
        // C.5 dense body_index: SdfBody order matches `sdf_bodies()` / selected_body_index /
        // volume_atlas body_order (0-based). Other drawables get unique IDs after that range so
        // gbuf_motion.r stays unambiguous past the old f16 2048 ceiling.
        self.cache.body_index_map.clear();
        for (idx, (nid, _)) in self.scene.scene_graph.sdf_bodies().enumerate() {
            self.cache.body_index_map.insert(nid, idx);
        }
        let mut next = self.cache.body_index_map.len();
        for &nid in &out {
            self.cache.body_index_map.entry(nid).or_insert_with(|| {
                let i = next;
                next += 1;
                i
            });
        }
        self.draw_stream = out;
        self.draw_stream_stale = false;
        self.draw_stream_rebuilds_last_frame = 1;
    }

    /// Ensure stream is current; sets rebuilds counter to 0 if reused.
    /// Empty stream after a rebuild is valid (no drawables) — do **not** rebuild every
    /// frame when empty (that made scoreboard `streamReb=1` forever on empty scenes).
    pub(crate) fn ensure_structural_draw_stream(&mut self) {
        if self.draw_stream_stale {
            self.rebuild_structural_draw_stream();
        } else {
            self.draw_stream_rebuilds_last_frame = 0;
        }
    }
}
