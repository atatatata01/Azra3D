// bevel_preview.rs — pure relocation from poly_drag_tools.rs
// GPU SDF bevel preview upload (secondary system).

use std::collections::HashSet;
use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, HalfEdgeId, FaceId, NO_FACE, NO_EDGE, NO_TWIN};
use core_math::scene_graph::NodePayload;
use crate::{
    AzraApp, ModelingContext, PolyWorkMode, PolyTool, DirtyFlags,
    ActiveTool, PolyExtrudeDrag, PolyInsetDrag, PolyBevelDrag, PolyShrinkFattenDrag,
    PolyTransformState, TransformKind, AxisConstraint, FalloffKind,
    world_to_screen,
};

impl AzraApp {
    // ================================================================
    // Bevel System 2: GPU SDF PREVIEW — Sprint 21 raymarched preview
    // ================================================================
    // SECONDARY system. Raymarches an SDF approximation of bevel on the GPU.
    // NOT connected to the Ctrl+B poly modal above. Intended for FUTURE use:
    // hover pre-highlighting (show bevel ghost on edge hover before committing).
    // Currently only activatable via UI panel checkbox.

    /// Upload selected edges to GPU for bevel preview.
    pub(crate) fn bevel_preview_upload_edges(&mut self) {
        if self.gpu.is_none() { return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        if self.bevel_pv.stored_edges.is_empty() {
            self.bevel_pv.edge_count = 0;
            return;
        }

        let world_transform = self.scene.scene_graph.world_transform(nid);
        let (gpu_edges, bbox_min, bbox_max) = graphics::bevel_preview::extract_bevel_edges(
            mesh, world_transform, &self.bevel_pv.stored_edges,
        );

        if gpu_edges.is_empty() {
            self.bevel_pv.edge_count = 0;
            return;
        }

        let w = self.bevel_pv.width;
        let expanded_min = bbox_min - Vec3::splat(w * 1.5);
        let expanded_max = bbox_max + Vec3::splat(w * 1.5);
        let grid = graphics::bevel_preview::build_spatial_grid(
            &gpu_edges, expanded_min, expanded_max, w, 8,
        );

        let max_edges = 256;
        let max_grid_indices = 2048;
        let gpu_edges = if gpu_edges.len() > max_edges { &gpu_edges[..max_edges] } else { &gpu_edges };
        let edge_count = gpu_edges.len() as u32;

        self.bevel_pv.edge_count = edge_count;
        self.bevel_pv.cached_bbox_min = expanded_min;
        self.bevel_pv.cached_bbox_max = expanded_max;
        let gpu = self.gpu.as_ref().unwrap();
        let queue = &gpu.queue;

        queue.write_buffer(&gpu.buffers.bevel_edge_buffer, 0, bytemuck::cast_slice(gpu_edges));
        queue.write_buffer(&gpu.buffers.bevel_grid_cells_buffer, 0, bytemuck::cast_slice(&grid.cells));
        if !grid.edge_indices.is_empty() {
            let indices = if grid.edge_indices.len() > max_grid_indices {
                &grid.edge_indices[..max_grid_indices]
            } else {
                &grid.edge_indices
            };
            queue.write_buffer(&gpu.buffers.bevel_grid_indices_buffer, 0, bytemuck::cast_slice(indices));
        }

        self.bevel_pv.needs_edge_upload = false;
        self.bevel_pv.needs_param_upload = true;
    }

    /// Upload bevel params to GPU (called when camera/params change while active).
    pub(crate) fn bevel_preview_upload_params(&mut self, rect: egui::Rect) {
        if self.gpu.is_none() { return; }
        if self.bevel_pv.edge_count == 0 { return; }

        let bbox_min = self.bevel_pv.cached_bbox_min;
        let bbox_max = self.bevel_pv.cached_bbox_max;

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
        let cam_up = (self.viewport.camera.orbit * Vec3::Y).normalize();
        let aspect = rect.width() / rect.height().max(1.0);
        let view = glam::Mat4::look_at_rh(cam_pos, cam_pos + cam_fwd, cam_up);
        let proj = if self.render.ortho {
            let h = self.viewport.camera.distance;
            glam::Mat4::orthographic_rh(-h * aspect, h * aspect, -h, h, 0.01, 200.0)
        } else {
            glam::Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, aspect, 0.01, 200.0)
        };
        let view_proj = proj * view;

        let w = rect.width();
        let h = rect.height();

        let bw = self.bevel_pv.width;
        let raw_min = bbox_min + Vec3::splat(bw * 1.5);
        let raw_max = bbox_max - Vec3::splat(bw * 1.5);

        // S21.10: Compute screen-space tile bounds for tiled dispatch
        let (tile_min, tile_max) = graphics::bevel_preview::compute_screen_tiles(
            bbox_min, bbox_max, view_proj, w, h,
        );
        self.bevel_pv.tile_min = tile_min;
        self.bevel_pv.tile_max = tile_max;

        let mut params = graphics::bevel_preview::build_params(
            bw,
            self.bevel_pv.depth,
            self.bevel_pv.shape,
            self.bevel_pv.profile,
            self.bevel_pv.edge_count,
            glam::UVec3::splat(8),
            raw_min, raw_max,
            view_proj, cam_pos,
            w, h,
        );
        // Set tile offset so WGSL maps local gid to correct viewport pixel
        params.tile_offset = [tile_min[0] as f32 * 8.0, tile_min[1] as f32 * 8.0];

        let gpu = self.gpu.as_ref().unwrap();
        let queue = &gpu.queue;
        queue.write_buffer(&gpu.buffers.bevel_params_buffer, 0, bytemuck::bytes_of(&params));

        self.bevel_pv.needs_param_upload = false;
    }
}
