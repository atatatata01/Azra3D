// inset_drag.rs — pure relocation from poly_drag_tools.rs
// Inset drag (I). Signed thickness: right = classic inset, left = outer rim segment (outset).

use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, FaceId};
use core_math::scene_graph::NodePayload;
use crate::{
    AzraApp, PolyTool, DirtyFlags,
    ActiveTool, PolyInsetDrag,
};

/// Slide the new ring: `pos = orig + dir * amount + depth_n * depth`.
/// Dirs point inward → amount > 0 shrinks (inset), amount < 0 extends outside (body corners fixed).
fn apply_inset_positions(
    mesh: &mut core_math::half_edge::HalfEdgeMesh,
    ring: &[(u32, Vec3)],
    dirs: &[Vec3],
    amount: f32,
    depth_n: Vec3,
    depth: f32,
) {
    for (i, &(vid, orig)) in ring.iter().enumerate() {
        if i < dirs.len() && (vid as usize) < mesh.vertices.len() {
            mesh.vert_mut(VertexId(vid)).position =
                orig + dirs[i] * amount + depth_n * depth;
        }
    }
}

impl AzraApp {
    /// Start an inset drag: create topology with zero inset, arm for mouse dragging.
    pub(crate) fn poly_start_inset_drag(&mut self, mouse: egui::Pos2) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() { return; }
        let before = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => {
                self.toast("Inset: active object is not a poly mesh");
                return;
            }
        };
        self.begin_episode("Inset Face");
        let (original_positions, offset_dirs, new_face_ids, rim_face_ids) =
            self.poly_inset_build_topology(nid, &faces, self.poly_inset_individual);
        if original_positions.is_empty() { self.cancel_episode(); return; }

        let depth_normal = {
            let mut navg = Vec3::ZERO;
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    for &f in &faces {
                        navg += mesh.compute_face_normal(FaceId(f));
                    }
                }
            }
            navg.normalize_or_zero()
        };

        let orig_sel_faces = self.poly_sel.selected_faces.clone();
        let orig_sel_edges = self.poly_sel.selected_edges.clone();
        let orig_sel_vertices = self.poly_sel.selected_vertices.clone();

        // Start at thickness 0 → island selected (classic). Outward drag reselects the rim.
        self.poly_sel.selected_faces.clear();
        for &f in &new_face_ids { self.poly_sel.selected_faces.insert(f); }
        self.poly_sync_mesh_cache(nid);

        // Smart clamp: max inward thickness before ring verts meet (EDGE CASE INSET).
        let max_inward = core_math::half_edge::HalfEdgeMesh::inset_max_inward_thickness(
            &original_positions,
            &offset_dirs,
        );

        self.start_operation(ActiveTool::PolyInset(PolyInsetDrag {
            node_id: nid, original_positions, offset_dirs, new_face_ids, rim_face_ids,
            max_inward,
            warned_clamp: false,
            rims_flipped: false,
            before, faces,
            start_mouse: mouse, accumulated_amount: 0.0,
            depth_normal, accumulated_depth: 0.0,
            orig_sel_faces, orig_sel_edges, orig_sel_vertices,
        }));
        self.toast(
            "Inset: drag R=in · L=out · Alt=depth · clamps+merges at collapse (not bevel)",
        );
    }

    /// Build inset topology at thickness 0. Returns `(ring_pos, dirs, island_ids, rim_ids)`.
    pub(crate) fn poly_inset_build_topology(
        &mut self, nid: core_math::scene_graph::NodeId, faces: &[u32], individual: bool,
    ) -> (Vec<(u32, Vec3)>, Vec<Vec3>, Vec<u32>, Vec<u32>) {
        let mut original_positions = Vec::new();
        let mut offset_dirs = Vec::new();
        let mut island_ids = Vec::new();
        let mut rim_ids = Vec::new();
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mesh = Arc::make_mut(mesh);
                let sets: Vec<std::collections::HashSet<FaceId>> = if individual || faces.len() == 1 {
                    faces.iter().map(|&f| std::iter::once(FaceId(f)).collect()).collect()
                } else {
                    vec![faces.iter().map(|&f| FaceId(f)).collect()]
                };
                for set in &sets {
                    let res = mesh.inset_region(set, 0.0, 0.0);
                    for f in &res.inner { island_ids.push(f.0); }
                    for f in &res.rim { rim_ids.push(f.0); }
                    for &(v, d) in &res.boundary {
                        original_positions.push((v.0, mesh.vert(v).position));
                        offset_dirs.push(d);
                    }
                }
            }
        }
        (original_positions, offset_dirs, island_ids, rim_ids)
    }

    /// Select island (inward) or rim segment (outward / ready to thicken).
    fn poly_inset_sync_selection(&mut self, amount: f32) {
        let (rim_ids, island_ids) = {
            let d = match self.tool.current.as_poly_inset() { Some(d) => d, None => return };
            (d.rim_face_ids.clone(), d.new_face_ids.clone())
        };
        self.poly_sel.selected_faces.clear();
        let pick = if amount < 0.0 { &rim_ids } else { &island_ids };
        for &f in pick { self.poly_sel.selected_faces.insert(f); }
    }

    /// Switch Region / Individual mid-drag.
    pub(crate) fn poly_inset_set_mode(&mut self, individual: bool) {
        if self.poly_inset_individual == individual { return; }
        self.poly_inset_individual = individual;
        let (nid, before, faces, start_mouse, amount, depth, depth_n) = match self.tool.current.as_poly_inset() {
            Some(d) => (d.node_id, d.before.clone(), d.faces.clone(), d.start_mouse,
                        d.accumulated_amount, d.accumulated_depth, d.depth_normal),
            None => return,
        };
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload { *mesh = before.clone(); }
        }
        let (original_positions, offset_dirs, new_face_ids, rim_face_ids) =
            self.poly_inset_build_topology(nid, &faces, individual);
        if original_positions.is_empty() { return; }

        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mesh = Arc::make_mut(mesh);
                apply_inset_positions(mesh, &original_positions, &offset_dirs, amount, depth_n, depth);
            }
        }
        let max_inward = core_math::half_edge::HalfEdgeMesh::inset_max_inward_thickness(
            &original_positions,
            &offset_dirs,
        );
        if let Some(d) = self.tool.current.as_poly_inset_mut() {
            d.original_positions = original_positions;
            d.offset_dirs = offset_dirs;
            d.max_inward = max_inward;
            d.warned_clamp = false;
            d.new_face_ids = new_face_ids;
            d.rim_face_ids = rim_face_ids;
            d.rims_flipped = false;
            d.start_mouse = start_mouse;
        }
        self.poly_inset_sync_selection(amount);
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(if individual { "Inset: Individual" } else { "Inset: Region (group)" });
    }

    /// Used by tool-params panel when thickness is edited live.
    pub(crate) fn poly_inset_ensure_topology(&mut self, amount: f32) {
        self.poly_inset_sync_selection(amount);
    }

    /// Update inset drag: signed thickness + depth.
    /// Right = inward inset, left = outer rim segment (body fixed), Alt = depth.
    ///
    /// Inward thickness is **clamped** to `max_inward` so complex shapes do not invert. At the
    /// clamp point, verts sit on the collapse locus; confirm then **merges** them (unlike bevel,
    /// which keeps verts so the profile can still change after clamp).
    pub(crate) fn poly_update_inset_drag(&mut self, mouse: egui::Pos2, _rect: egui::Rect, ctrl: bool, alt: bool) {
        let (nid, originals, dirs, start, prev_amount, prev_depth, depth_n, max_in) = {
            let d = match self.tool.current.as_poly_inset() { Some(d) => d, None => return };
            (d.node_id, d.original_positions.clone(), d.offset_dirs.clone(), d.start_mouse,
             d.accumulated_amount, d.accumulated_depth, d.depth_normal, d.max_inward)
        };

        let scale = self.viewport.camera.distance * self.tweak.grab_speed * 0.5;
        let (mut amount, depth) = if alt {
            let dy = start.y - mouse.y;
            let raw_depth = dy * scale;
            let depth = if self.is_snap_active(ctrl) {
                let grid = self.viewport.grid_cell_size.max(0.01);
                (raw_depth / grid).round() * grid
            } else { raw_depth };
            (prev_amount, depth)
        } else {
            let dx = mouse.x - start.x;
            let raw_amount = dx * scale;
            let amount = if self.is_snap_active(ctrl) {
                let grid = self.viewport.grid_cell_size.max(0.01);
                (raw_amount / grid).round() * grid
            } else { raw_amount };
            (amount, prev_depth)
        };

        // Smart clamp inward only (complex shapes). Outward outset stays free.
        let mut hit_clamp = false;
        if amount > 0.0 && max_in.is_finite() && max_in > 0.0 && amount > max_in {
            amount = max_in;
            hit_clamp = true;
        }

        self.poly_with_base_mesh_mut(nid, |mesh| {
            apply_inset_positions(mesh, &originals, &dirs, amount, depth_n, depth);
        });
        self.poly_inset_sync_selection(amount);

        self.poly_sync_after_position_edit(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        if let Some(d) = self.tool.current.as_poly_inset_mut() {
            d.accumulated_amount = amount;
            d.accumulated_depth = depth;
            if hit_clamp && !d.warned_clamp {
                d.warned_clamp = true;
                self.toast(&format!(
                    "Inset clamped at {amount:.3} — release merges verts at clamp (clean topology)"
                ));
            }
        }
    }

    pub(crate) fn poly_cancel_inset_drag(&mut self) {
        let restore = self.tool.current.as_poly_inset()
            .map(|d| (d.orig_sel_faces.clone(), d.orig_sel_edges.clone(), d.orig_sel_vertices.clone()));
        if let Some(entry) = self.pending_episode.take() {
            if let Some(sg) = entry.payload.scene_graph_snapshot() {
                self.scene.scene_graph = sg;
            }
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        if let Some((f, e, v)) = restore {
            self.poly_sel.selected_faces = f;
            self.poly_sel.selected_edges = e;
            self.poly_sel.selected_vertices = v;
        } else {
            self.poly_sel.selected_faces.clear();
            self.poly_sel.selected_edges.clear();
            self.poly_sel.selected_vertices.clear();
        }
        self.poly_sync_all_meshes();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_confirm_inset_drag(&mut self) {
        let (nid, amount, depth, select_faces, ring_vids, max_in, originals, dirs, depth_n) =
            match self.tool.current.as_poly_inset() {
                Some(d) => {
                    let sel = if d.accumulated_amount < 0.0 {
                        d.rim_face_ids.clone()
                    } else {
                        d.new_face_ids.clone()
                    };
                    (
                        d.node_id,
                        d.accumulated_amount,
                        d.accumulated_depth,
                        sel,
                        d.original_positions.iter().map(|&(v, _)| v).collect::<Vec<u32>>(),
                        d.max_inward,
                        d.original_positions.clone(),
                        d.offset_dirs.clone(),
                        d.depth_normal,
                    )
                }
                None => return,
            };
        if amount.abs() < 1e-6 && depth.abs() < 1e-6 {
            self.poly_cancel_inset_drag();
            return;
        }

        // Ensure final positions are applied (numeric panel / clamp edge).
        self.poly_with_base_mesh_mut(nid, |mesh| {
            apply_inset_positions(mesh, &originals, &dirs, amount, depth_n, depth);
        });

        // At clamp (or near-coincident ring verts on complex shapes): MERGE — unlike bevel,
        // which keeps verts so the profile can still change after Clamp Overlap.
        let at_clamp = amount > 0.0
            && max_in.is_finite()
            && max_in > 0.0
            && amount >= max_in * 0.98;
        if at_clamp || amount > 0.0 {
            self.poly_inset_merge_ring_at_clamp(nid, &ring_vids, at_clamp);
        }

        // Leave the right faces selected for the next op.
        self.poly_sel.selected_faces.clear();
        for &f in &select_faces { self.poly_sel.selected_faces.insert(f); }

        let adjust = self.tool.current.as_poly_inset().map(|d| crate::types::PolyInsetAdjust {
            nid: d.node_id,
            faces: d.faces.clone(),
            before: d.before.clone(),
            thickness: d.accumulated_amount,
            depth: d.accumulated_depth,
            individual: self.poly_inset_individual,
            needs_settle: false,
        });
        // SMART SELECT (owner 2026-08-05: "inset: tag all geo created by inset should be tagged
        // not to bevel"). Stamped at CONFIRM, not at drag start: the topology is built at
        // thickness 0, where every new ring vertex is COINCIDENT with the original it was split
        // from, so `remap_by_position` could not tell the two apart. At confirm the positions are
        // final and `d.before` is the numbering the sidecar is still keyed by.
        let tag_input = self.tool.current.as_poly_inset().map(|d| {
            (
                d.before.clone(),
                d.original_positions.iter().map(|&(v, _)| v).collect::<Vec<u32>>(),
            )
        });
        if let Some((before, new_verts)) = tag_input {
            self.poly_inset_stamp_tags(nid, &before, &new_verts);
        }
        self.commit_episode();
        self.poly_flush_mesh_dirty();
        let faces: Vec<core_math::half_edge::FaceId> =
            select_faces.into_iter().map(core_math::half_edge::FaceId).collect();
        let _ = self.poly_auto_group_faces(nid, core_math::poly_mesh::PolyGroupKind::Inset, faces);
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.ui.poly_inset_adjust = adjust;
    }

    /// Weld near-coincident ring verts after a clamped inset (clean topology on complex shapes).
    ///
    /// At clamp, several ring verts land on the same point — leave them as a cluster and they
    /// create zero-area faces / inverted loops. Bevel deliberately keeps those verts so width /
    /// shape can still be adjusted; inset merges them (owner EDGE CASE INSET + MESHmachine Unf*ck
    /// "Merge" spirit).
    fn poly_inset_merge_ring_at_clamp(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        ring_vids: &[u32],
        force: bool,
    ) {
        if ring_vids.len() < 2 {
            return;
        }
        let thr = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            let (mn, mx) = mesh.bounding_box();
            // Tight when forced (at clamp); slightly looser for near-coincident clean-up.
            let base = ((mx - mn).length() * if force { 0.002 } else { 0.0005 }).max(1e-5);
            base
        };
        let mut merged = 0usize;
        // Multiple passes: chains of collapsed corners need iterative pairwise welds.
        for _ in 0..8 {
            let ops: Vec<(Vec<u32>, Vec3)> = {
                let node = match self.scene.scene_graph.get(nid) {
                    Some(n) => n,
                    None => return,
                };
                let mesh = match &node.payload {
                    NodePayload::PolygonalBody { mesh, .. } => mesh,
                    _ => return,
                };
                let live: Vec<(u32, Vec3)> = ring_vids
                    .iter()
                    .copied()
                    .filter(|&v| (v as usize) < mesh.vertices.len() && mesh.is_vert_live(VertexId(v)))
                    .map(|v| (v, mesh.vert(VertexId(v)).position))
                    .collect();
                if live.len() < 2 {
                    return;
                }
                // Union-find clusters within thr.
                let n = live.len();
                let mut parent: Vec<usize> = (0..n).collect();
                fn find(p: &mut [usize], mut x: usize) -> usize {
                    while p[x] != x {
                        p[x] = p[p[x]];
                        x = p[x];
                    }
                    x
                }
                for i in 0..n {
                    for j in (i + 1)..n {
                        if live[i].1.distance(live[j].1) <= thr {
                            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                            if a != b {
                                parent[a] = b;
                            }
                        }
                    }
                }
                let mut groups: std::collections::BTreeMap<usize, Vec<usize>> =
                    std::collections::BTreeMap::new();
                for i in 0..n {
                    let r = find(&mut parent, i);
                    groups.entry(r).or_default().push(i);
                }
                groups
                    .values()
                    .filter(|g| g.len() >= 2)
                    .map(|g| {
                        let mut ids: Vec<u32> = g.iter().map(|&i| live[i].0).collect();
                        ids.sort_unstable();
                        let target =
                            g.iter().map(|&i| live[i].1).fold(Vec3::ZERO, |a, b| a + b) / g.len() as f32;
                        (ids, target)
                    })
                    .collect()
            };
            if ops.is_empty() {
                break;
            }
            self.poly_with_base_mesh_mut(nid, |mesh| {
                for (ids, target) in &ops {
                    let vids: Vec<VertexId> = ids.iter().map(|&v| VertexId(v)).collect();
                    if mesh.merge_vertices_to(&vids, *target).is_some() {
                        merged += vids.len().saturating_sub(1);
                    }
                }
            });
        }
        if merged > 0 {
            self.poly_request_display_sync(nid);
            self.poly_invalidate_pick_cache();
            self.toast(&format!("Inset: merged {merged} ring vert(s) at clamp"));
        }
    }
}
