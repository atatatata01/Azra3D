// shrink_fatten.rs — pure relocation from poly_drag_tools.rs
// Shrink/Fatten drag (Alt+S).

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
    // ============================================================
    // Poly Shrink/Fatten (Blender Alt+S) — move selection along per-vertex normals
    // ============================================================

    /// Start a Shrink/Fatten drag. Snapshots each affected vertex's ORIGINAL position + unit vertex
    /// normal (frozen for the whole drag so the offset stays stable — recomputing the normal after
    /// motion would feed back and wobble). Works from any sub-element mode via the shared affected-vertex
    /// gather. No topology change, no selection change.
    pub(crate) fn poly_start_shrink_fatten(&mut self, mouse: egui::Pos2) {
        let affected = self.poly_gather_affected_vertices();
        if affected.is_empty() { self.toast("Shrink/Fatten: select geometry first"); return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };

        // Deterministic order so the drag snapshot is stable across frames (HashSet iter is random).
        let mut ids: Vec<u32> = affected.into_iter().collect();
        ids.sort_unstable();

        let mut verts: Vec<(u32, Vec3, Vec3)> = Vec::with_capacity(ids.len());
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                for &vid in &ids {
                    let v = VertexId(vid);
                    let n = mesh.compute_vertex_normal(v).normalize_or_zero();
                    verts.push((vid, mesh.vert(v).position, n));
                }
            }
        }
        if verts.is_empty() { return; }

        self.begin_episode("Shrink/Fatten");
        self.start_operation(ActiveTool::PolyShrinkFatten(PolyShrinkFattenDrag {
            node_id: nid, verts, start_mouse: mouse, accumulated_offset: 0.0,
        }));
        self.toast("Shrink/Fatten: drag L/R = offset, LMB/Enter confirm, Esc cancel");
    }

    /// Update Shrink/Fatten: signed offset from horizontal mouse delta (right = fatten, left = shrink),
    /// each vertex = orig + normal * offset (absolute from the snapshot — no drift). Snap rounds to grid.
    pub(crate) fn poly_update_shrink_fatten(&mut self, mouse: egui::Pos2, ctrl: bool) {
        let (nid, verts, start) = {
            let d = match self.tool.current.as_poly_shrink_fatten() { Some(d) => d, None => return };
            (d.node_id, d.verts.clone(), d.start_mouse)
        };
        let dx = mouse.x - start.x;
        let raw = dx * self.viewport.camera.distance * self.tweak.grab_speed * 0.5; // SIGNED (shrink < 0)
        let offset = if self.is_snap_active(ctrl) {
            let grid = self.viewport.grid_cell_size.max(0.01);
            (raw / grid).round() * grid
        } else { raw };

        self.poly_with_base_mesh_mut(nid, |mesh| {
            for &(vid, orig, normal) in &verts {
                mesh.vert_mut(VertexId(vid)).position = orig + normal * offset;
            }
        });
        self.poly_sync_after_position_edit(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        if let Some(d) = self.tool.current.as_poly_shrink_fatten_mut() { d.accumulated_offset = offset; }
    }

    /// Confirm Shrink/Fatten — commit the episode (one undo entry) and return to idle.
    /// Zero offset = cancel (no undo entry).
    pub(crate) fn poly_confirm_shrink_fatten(&mut self) {
        let offset = self.tool.current.as_poly_shrink_fatten().map(|d| d.accumulated_offset).unwrap_or(0.0);
        if offset.abs() < 1e-6 {
            self.poly_cancel_shrink_fatten();
            return;
        }
        self.commit_episode();
        self.poly_flush_mesh_dirty();
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
    }

    /// Cancel Shrink/Fatten — restore the pre-drag mesh from the episode snapshot. Selection is
    /// untouched by this tool, so nothing to restore there.
    pub(crate) fn poly_cancel_shrink_fatten(&mut self) {
        if let Some(entry) = self.pending_episode.take() {
            if let Some(sg) = entry.payload.scene_graph_snapshot() {
                self.scene.scene_graph = sg;
            }
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.poly_sync_all_meshes();
        self.mark_dirty(DirtyFlags::RENDER);
    }
}
