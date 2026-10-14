//! Viewport auto-mode policy — extracted from viewport_render.rs (de-god slice 15).
//!
//! `update_viewport_mode_auto` decides FullSdf vs Hybrid from has_raster /
//! all_sdf_meshed / object_mode. Pure policy: reads the scene graph, mutates
//! `self.render.viewport_mode`, toasts. Deliberately kept out of the render path.

use crate::*;

impl FourthLayerApp {
    /// Auto-detect non-SDF bodies and switch viewport mode accordingly.
    /// Called before dispatch_gpu_render — keeps policy logic out of the render path.
    pub(crate) fn update_viewport_mode_auto(&mut self) {
        // (force_analytical_debug FullSdf override REMOVED 2026-07-14 — analytical march path stripped.)

        // NOTE: Auto-enable of per-body volume cache is deferred until
        // active_body_nid tracking is reliable (body selection vs editing UX).
        // The selective serialization gate in dispatch_gpu_render is ready —
        // it just needs use_body_vol_cache=true + correct active_body_nid.
        // For now, users enable via the "Per-Body Cache" toggle in properties.

        // Object Mode is the ONLY mode (sdf_edit_active removed 2026-07-13): the unified
        // raster path always runs. The raster pass owns the G-buffer CLEAR, floor grid, depth,
        // and GPU picking, and the Display Invariant mandates triangles-only — so FullSdf
        // (full-screen march, raster skipped) is never valid outside force_analytical_debug.
        // Forcing Hybrid eliminates the "delete last object → frozen ghost" class: an empty /
        // placeholder / mid-bake scene keeps clearing the G-buffer (and refreshing raster_depth)
        // every frame instead of flipping to FullSdf.
        if self.render.viewport_mode == ViewportRenderMode::FullSdf {
            self.render.viewport_mode = ViewportRenderMode::Hybrid;
            // AZRA_MASTER_ROADMAP Phase A2: only walk the graph when we need the toast —
            // not every frame. Do not delete: toast still depends on has_raster.
            let has_raster = self.scene.scene_graph.all_nodes().any(|(_, n)| {
                matches!(
                    &n.payload,
                    core_math::scene_graph::NodePayload::PolygonalBody { .. }
                        | core_math::scene_graph::NodePayload::NurbsBody { .. }
                        | core_math::scene_graph::NodePayload::ImportedMesh { .. }
                        | core_math::scene_graph::NodePayload::VoxelBody { .. }
                )
            });
            if has_raster {
                self.toast("Hybrid viewport — poly/NURBS meshes visible");
            }
        }
    }
}
