// core_engine/app/src/poly/slide.rs
// Vertex slide (Shift+V) + edge slide (Shift+E) — Blender MESH_OT_vert_slide / MESH_OT_edge_slide.
// Both are PURE VERTEX MOVES (no topology change): snapshot the mesh, re-move live from the snapshot as
// the mouse drives a factor, and commit ONE undo on confirm (restore snapshot → transaction re-applies).
// Kernel: vertex slide lerps toward a picked vertex_fan neighbour; edge slide uses HalfEdgeMesh::
// edge_slide_rails. Mirrors the loop-cut-slide interaction pattern (poly/loop_cut.rs).

use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, HalfEdgeId, HalfEdgeMesh};
use core_math::scene_graph::{NodePayload, NodeId};
use crate::{AzraApp, ActiveTool, PolyVertSlideDrag, PolyEdgeSlideDrag, PolyTool, DirtyFlags, world_to_screen};

impl AzraApp {
    // ============================================================
    // Start
    // ============================================================

    /// Begin a VERTEX slide on the hovered (or first selected) vertex. The drag then picks which incident
    /// edge V rides and how far. No-op if there is no vertex to slide.
    pub(crate) fn poly_start_vert_slide(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let vert = match self.poly_sel.hovered_vertex
            .or_else(|| self.poly_sel.selected_vertices.iter().copied().next()) {
            Some(v) => v, None => { self.toast("Vertex Slide: hover or select a vertex first"); return; }
        };
        let wm = self.scene.scene_graph.world_transform(nid);
        let (orig_mesh, neighbors, orig_world) = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh.clone(), _ => return };
            if vert as usize >= mesh.vertices.len() { return; }
            let orig_world = wm.transform_point3(mesh.vert(VertexId(vert)).position);
            // Incident-edge neighbours (candidate rails), deduped, with WORLD positions for screen projection.
            let mut seen = std::collections::HashSet::new();
            let mut neighbors: Vec<(u32, Vec3)> = Vec::new();
            for h in mesh.vertex_fan(VertexId(vert)) {
                let nb = mesh.he(h).vertex; // outgoing → neighbour
                if nb.0 == vert { continue; }
                if seen.insert(nb.0) {
                    neighbors.push((nb.0, wm.transform_point3(mesh.vert(nb).position)));
                }
            }
            (mesh, neighbors, orig_world)
        };
        if neighbors.is_empty() { return; }
        self.start_operation(ActiveTool::PolyVertSlide(PolyVertSlideDrag {
            node_id: nid, original_mesh: orig_mesh, vert, neighbors, orig_world,
            chosen: 0, factor: 0.0, clamp: true, start_mouse: self.viewport.viewport_mouse,
        }));
        self.toast("Vertex Slide: drag toward an edge, LMB/Enter confirm, Esc cancel (C=clamp)");
    }

    /// Begin an EDGE slide on the selected edge loop (or the hovered edge). Every loop vertex rides its two
    /// off-loop rails by one shared factor.
    pub(crate) fn poly_start_edge_slide(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let edges: Vec<u32> = if !self.poly_sel.selected_edges.is_empty() {
            self.poly_sel.selected_edges.iter().copied().collect()
        } else if let Some(e) = self.poly_sel.hovered_edge {
            vec![e]
        } else { self.toast("Edge Slide: select or hover an edge loop first"); return; };

        let wm = self.scene.scene_graph.world_transform(nid);
        let (orig_mesh, loop_verts, rail_a, rail_b) = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh.clone(), _ => return };
            // COHERENT loop rails (kernel walks the selection into one consistently-directed loop so every
            // vertex's +rail is on the same side — fixes "one side slides the wrong way").
            let hedges: Vec<HalfEdgeId> = edges.iter()
                .filter(|&&e| (e as usize) < mesh.half_edges.len())
                .map(|&e| HalfEdgeId(e)).collect();
            let mut loop_verts: Vec<(u32, Vec3, Vec3, Vec3)> = Vec::new();
            for (v, rp, rn) in mesh.edge_slide_rails_loop(&hedges) {
                let o = mesh.vert(v).position;
                let rpp = mesh.vert(rp).position;
                let rnn = mesh.vert(rn).position;
                loop_verts.push((v.0, o, rpp, rnn));
            }
            // Mouse→factor axis: pick the loop vertex whose −rail→+rail span is LONGEST in world
            // space (and non-degenerate). Averaging all rails produces a "weird average" that
            // points nowhere on channels / miters (owner Alt+S / edge-slide report 2026-07-30).
            // Factor is still shared; each vert moves on its OWN rails in regenerate.
            let mut best_len = 0.0f32;
            let mut ra = Vec3::ZERO;
            let mut rb = Vec3::ZERO;
            for &(_, o, rp, rn) in &loop_verts {
                let pos = rp - o;
                let neg = rn - o;
                let span = pos.length() + neg.length();
                if span > best_len {
                    best_len = span;
                    // Place axis through this vert's origin so screen projection has real length.
                    ra = wm.transform_point3(o + neg);
                    rb = wm.transform_point3(o + pos);
                }
            }
            if best_len < 1e-8 {
                // Fallback: centroid of unit rails if all spans ~0.
                let n = loop_verts.len().max(1) as f32;
                let centroid: Vec3 = loop_verts.iter().map(|(_, o, _, _)| *o).sum::<Vec3>() / n;
                let avg_pos: Vec3 = loop_verts.iter().map(|(_, o, rp, _)| (*rp - *o).normalize_or_zero()).sum::<Vec3>() / n;
                let avg_neg: Vec3 = loop_verts.iter().map(|(_, o, _, rn)| (*rn - *o).normalize_or_zero()).sum::<Vec3>() / n;
                ra = wm.transform_point3(centroid + avg_neg);
                rb = wm.transform_point3(centroid + avg_pos);
            }
            (mesh, loop_verts, ra, rb)
        };
        if loop_verts.is_empty() { self.toast("Edge Slide: no slidable rails on this edge"); return; }
        self.start_operation(ActiveTool::PolyEdgeSlide(PolyEdgeSlideDrag {
            node_id: nid, original_mesh: orig_mesh, loop_verts,
            factor: 0.0, clamp: true, start_mouse: self.viewport.viewport_mouse,
            rail_a_world: rail_a, rail_b_world: rail_b,
            mouse_pick_rails: false,
        }));
        self.toast("Edge Slide: drag along the rails, LMB/Enter confirm, Esc cancel (C=clamp)");
    }

    /// Multi-vertex edge-constrained slide (Alt+S in Vertex mode): each selected vertex rides the
    /// incident edge whose screen direction best matches the mouse drag. Shared signed factor from
    /// the best-matching axis so the whole selection moves coherently without normal-averaging.
    pub(crate) fn poly_start_multi_vert_edge_slide(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let mut ids: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        if ids.is_empty() {
            if let Some(v) = self.poly_sel.hovered_vertex { ids.push(v); }
        }
        if ids.is_empty() {
            self.toast("Slide: select vertices first");
            return;
        }
        ids.sort_unstable();
        let wm = self.scene.scene_graph.world_transform(nid);
        let (orig_mesh, loop_verts, rail_a, rail_b) = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh.clone(), _ => return };
            // Encode each vert's best two opposite fan neighbours as ±rails (longest pair when possible).
            let mut loop_verts: Vec<(u32, Vec3, Vec3, Vec3)> = Vec::new();
            for &vid in &ids {
                if (vid as usize) >= mesh.vertices.len() { continue; }
                let o = mesh.vert(VertexId(vid)).position;
                let mut nbs: Vec<Vec3> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for h in mesh.vertex_fan(VertexId(vid)) {
                    let nb = mesh.he(h).vertex;
                    if nb.0 == vid || !seen.insert(nb.0) { continue; }
                    nbs.push(mesh.vert(nb).position);
                }
                if nbs.is_empty() { continue; }
                // Pick the neighbour pair with largest angular separation as ±rails; if only one, use it as + and
                // synthesize − as opposite direction (clamp will keep t≥0).
                let (rp, rn) = if nbs.len() == 1 {
                    let p = nbs[0];
                    let d = p - o;
                    (p, o - d)
                } else {
                    let mut best = (0usize, 1usize, -2.0f32);
                    for i in 0..nbs.len() {
                        for j in (i + 1)..nbs.len() {
                            let di = (nbs[i] - o).normalize_or_zero();
                            let dj = (nbs[j] - o).normalize_or_zero();
                            let sep = -di.dot(dj); // prefer opposite
                            if sep > best.2 { best = (i, j, sep); }
                        }
                    }
                    (nbs[best.0], nbs[best.1])
                };
                loop_verts.push((vid, o, rp, rn));
            }
            // Mouse axis = longest rail span among selected verts (same as edge slide).
            let mut best_len = 0.0f32;
            let mut ra = Vec3::ZERO;
            let mut rb = Vec3::ZERO;
            for &(_, o, rp, rn) in &loop_verts {
                let span = (rp - o).length() + (rn - o).length();
                if span > best_len {
                    best_len = span;
                    ra = wm.transform_point3(rn);
                    rb = wm.transform_point3(rp);
                }
            }
            (mesh, loop_verts, ra, rb)
        };
        if loop_verts.is_empty() {
            self.toast("Slide: no connected edges on the selection");
            return;
        }
        self.start_operation(ActiveTool::PolyEdgeSlide(PolyEdgeSlideDrag {
            node_id: nid,
            original_mesh: orig_mesh,
            loop_verts,
            factor: 0.0,
            clamp: true,
            start_mouse: self.viewport.viewport_mouse,
            rail_a_world: rail_a,
            rail_b_world: rail_b,
            mouse_pick_rails: true,
        }));
        self.toast("Vertex edge-slide: drag along edges, LMB/Enter confirm, Esc cancel (C=clamp)");
    }

    // ============================================================
    // Update (mouse → factor, live re-move)
    // ============================================================

    /// Project the mouse onto a world axis and return the factor (displacement / half-axis-length).
    fn slide_factor_along(&self, a: Vec3, b: Vec3, start: egui::Pos2, mp: egui::Pos2, rect: egui::Rect) -> Option<f32> {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let sa = world_to_screen(a, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg)?;
        let sb = world_to_screen(b, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg)?;
        let (ex, ey) = (sb.x - sa.x, sb.y - sa.y);
        let len = (ex * ex + ey * ey).sqrt();
        if len < 1e-3 { return None; }
        Some(((mp.x - start.x) * (ex / len) + (mp.y - start.y) * (ey / len)) / (len * 0.5))
    }

    pub(crate) fn poly_update_vert_slide(&mut self, mp: egui::Pos2, rect: egui::Rect, alt: bool) {
        let (orig_world, neighbors, start, clamp, prev, prev_chosen) = {
            let d = match self.tool.current.as_poly_vert_slide() { Some(d) => d, None => return };
            (d.orig_world, d.neighbors.clone(), d.start_mouse, d.clamp, d.factor, d.chosen)
        };
        // Re-pick the neighbour whose screen direction best matches the drag (Blender: the edge you drag
        // toward). Below a tiny drag threshold keep the current choice to avoid flicker.
        let drag = egui::vec2(mp.x - start.x, mp.y - start.y);
        let mut chosen = prev_chosen;
        if drag.length() > 4.0 {
            let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let aspect = rect.width() / rect.height().max(1.0);
            let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
            let so = world_to_screen(orig_world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
            if let Some(so) = so {
                let dn = drag.normalized();
                let mut best = f32::NEG_INFINITY;
                for (i, (_, nw)) in neighbors.iter().enumerate() {
                    if let Some(sn) = world_to_screen(*nw, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                        let axis = egui::vec2(sn.x - so.x, sn.y - so.y);
                        if axis.length() > 1e-3 {
                            let dot = axis.normalized().dot(dn);
                            if dot > best { best = dot; chosen = i; }
                        }
                    }
                }
            }
        }
        let (nb_vid, nb_world) = match neighbors.get(chosen) { Some(n) => *n, None => return };
        let _ = nb_vid;
        // Alt held (Blender) OR clamp toggled off (C) lets the slide go BEYOND the neighbour (extrapolate).
        let eff_clamp = clamp && !alt;
        let factor = match self.slide_factor_along(orig_world, nb_world, start, mp, rect) {
            Some(f) => if eff_clamp { f.clamp(0.0, 1.0) } else { f.clamp(-4.0, 4.0) },
            None => prev,
        };
        if (factor - prev).abs() < 1e-4 && chosen == prev_chosen { return; }
        if let Some(d) = self.tool.current.as_poly_vert_slide_mut() { d.factor = factor; d.chosen = chosen; }
        self.poly_vert_slide_regenerate();
    }

    pub(crate) fn poly_update_edge_slide(&mut self, mp: egui::Pos2, rect: egui::Rect, alt: bool) {
        // Multi-vert edge-slide (Alt+S Vertex): re-pick each vert's ± rails from its fan so the
        // mouse direction chooses which connected edge to ride — not a frozen average normal.
        self.poly_edge_slide_repick_rails_from_mouse(mp, rect);

        let (a, b, start, clamp, prev) = {
            let d = match self.tool.current.as_poly_edge_slide() { Some(d) => d, None => return };
            (d.rail_a_world, d.rail_b_world, d.start_mouse, d.clamp, d.factor)
        };
        // rail axis is −rail → +rail, so a positive projection = toward +rail = t>0. Alt (Blender) / C-off
        // unclamps so the loop can slide PAST the rails (beyond the shape).
        let eff_clamp = clamp && !alt;
        let factor = match self.slide_factor_along(a, b, start, mp, rect) {
            Some(f) => if eff_clamp { f.clamp(-1.0, 1.0) } else { f.clamp(-4.0, 4.0) },
            None => prev,
        };
        if (factor - prev).abs() < 1e-4 { return; }
        if let Some(d) = self.tool.current.as_poly_edge_slide_mut() { d.factor = factor; }
        self.poly_edge_slide_regenerate();
    }

    /// Multi-vert Alt+S only (`mouse_pick_rails`): re-orient each vert's +rail to the fan neighbour
    /// that best matches the mouse drag in screen space. True edge-loop slides leave rails alone.
    fn poly_edge_slide_repick_rails_from_mouse(&mut self, mp: egui::Pos2, rect: egui::Rect) {
        let (nid, start, orig, mut loop_verts, pick) = {
            let d = match self.tool.current.as_poly_edge_slide() {
                Some(d) => d,
                None => return,
            };
            if !d.mouse_pick_rails {
                return;
            }
            (
                d.node_id,
                d.start_mouse,
                d.original_mesh.clone(),
                d.loop_verts.clone(),
                true,
            )
        };
        let _ = pick;
        let drag = egui::vec2(mp.x - start.x, mp.y - start.y);
        if drag.length() < 4.0 {
            return;
        }
        let dn = drag.normalized();
        let wm = self.scene.scene_graph.world_transform(nid);
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };

        let mut best_axis: Option<(Vec3, Vec3, f32)> = None;
        for entry in &mut loop_verts {
            let (vid, o, _, _) = *entry;
            let v = VertexId(vid);
            if (vid as usize) >= orig.vertices.len() {
                continue;
            }
            let mut nbs: Vec<(u32, Vec3)> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for h in orig.vertex_fan(v) {
                let nb = orig.he(h).vertex;
                if nb.0 == vid || !seen.insert(nb.0) {
                    continue;
                }
                nbs.push((nb.0, orig.vert(nb).position));
            }
            if nbs.is_empty() {
                continue;
            }
            if nbs.len() == 1 {
                entry.2 = nbs[0].1;
                entry.3 = o - (nbs[0].1 - o);
                continue;
            }
            let ow = wm.transform_point3(o);
            let so = match world_to_screen(
                ow,
                cam_pos,
                self.viewport.camera.orbit,
                rect,
                aspect,
                ortho_hw,
                self.render.fov_deg,
            ) {
                Some(s) => s,
                None => continue,
            };
            let mut best_i = 0usize;
            let mut best_dot = f32::NEG_INFINITY;
            let mut scores: Vec<f32> = Vec::with_capacity(nbs.len());
            for (i, (_, p)) in nbs.iter().enumerate() {
                let pw = wm.transform_point3(*p);
                let sn = world_to_screen(
                    pw,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                );
                let dot = if let Some(sn) = sn {
                    let axis = egui::vec2(sn.x - so.x, sn.y - so.y);
                    if axis.length() > 1e-3 {
                        axis.normalized().dot(dn)
                    } else {
                        -2.0
                    }
                } else {
                    -2.0
                };
                scores.push(dot);
                if dot > best_dot {
                    best_dot = dot;
                    best_i = i;
                }
            }
            let mut worst_i = 0usize;
            let mut worst_dot = f32::INFINITY;
            for (i, &dot) in scores.iter().enumerate() {
                if dot < worst_dot {
                    worst_dot = dot;
                    worst_i = i;
                }
            }
            entry.2 = nbs[best_i].1;
            entry.3 = nbs[worst_i].1;
            let ra = wm.transform_point3(entry.3);
            let rb = wm.transform_point3(entry.2);
            let slen = if let (Some(sa), Some(sb)) = (
                world_to_screen(ra, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                world_to_screen(rb, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            ) {
                ((sb.x - sa.x).powi(2) + (sb.y - sa.y).powi(2)).sqrt()
            } else {
                0.0
            };
            if best_axis.map_or(true, |(_, _, l)| slen > l) {
                best_axis = Some((ra, rb, slen));
            }
        }
        if let Some(d) = self.tool.current.as_poly_edge_slide_mut() {
            d.loop_verts = loop_verts;
            if let Some((ra, rb, _)) = best_axis {
                d.rail_a_world = ra;
                d.rail_b_world = rb;
            }
        }
    }

    // ============================================================
    // Live re-move from the snapshot
    // ============================================================

    fn poly_vert_slide_regenerate(&mut self) {
        let (nid, orig, vert, chosen_vid, factor) = {
            let d = match self.tool.current.as_poly_vert_slide() { Some(d) => d, None => return };
            (d.node_id, d.original_mesh.clone(), d.vert, d.neighbors.get(d.chosen).map(|n| n.0), d.factor)
        };
        let chosen_vid = match chosen_vid { Some(v) => v, None => return };
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mut m = (*orig).clone();
                let v0 = orig.vert(VertexId(vert)).position;
                let n = orig.vert(VertexId(chosen_vid)).position;
                if (vert as usize) < m.vertices.len() {
                    m.vertices[vert as usize].position = v0 + factor * (n - v0);
                }
                m.recompute_normals();
                *mesh = Arc::new(m);
            }
        }
        self.poly_release_shared_display_arcs(nid);
        self.poly_sync_after_position_edit(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    fn poly_edge_slide_regenerate(&mut self) {
        let (nid, orig, loop_verts, factor) = {
            let d = match self.tool.current.as_poly_edge_slide() { Some(d) => d, None => return };
            (d.node_id, d.original_mesh.clone(), d.loop_verts.clone(), d.factor)
        };
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mut m = (*orig).clone();
                for (vid, o, rp, rn) in &loop_verts {
                    let target = if factor >= 0.0 { *o + factor * (*rp - *o) } else { *o + (-factor) * (*rn - *o) };
                    if (*vid as usize) < m.vertices.len() { m.vertices[*vid as usize].position = target; }
                }
                m.recompute_normals();
                *mesh = Arc::new(m);
            }
        }
        self.poly_release_shared_display_arcs(nid);
        self.poly_sync_after_position_edit(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    // ============================================================
    // Confirm / cancel (shared for both slides)
    // ============================================================

    /// (nid, original snapshot) of whichever slide is active.
    fn active_slide_ctx(&self) -> Option<(NodeId, Arc<HalfEdgeMesh>)> {
        if let Some(d) = self.tool.current.as_poly_vert_slide() { return Some((d.node_id, d.original_mesh.clone())); }
        if let Some(d) = self.tool.current.as_poly_edge_slide() { return Some((d.node_id, d.original_mesh.clone())); }
        None
    }

    /// Confirm the slide as ONE undo: capture the slid mesh, restore the snapshot for a clean before-image,
    /// then a transaction swaps in the slid mesh (mirrors poly_confirm_loop_cut_slide).
    ///
    /// Zero-factor confirm (user pressed Enter without sliding) restores the snapshot and exits
    /// with **no** undo entry — same honesty as fill/bridge refuse-before-transaction.
    pub(crate) fn poly_confirm_slide(&mut self) {
        let factor = self.tool.current.as_poly_vert_slide().map(|d| d.factor)
            .or_else(|| self.tool.current.as_poly_edge_slide().map(|d| d.factor))
            .unwrap_or(0.0);
        let (nid, orig) = match self.active_slide_ctx() { Some(c) => c, None => return };
        // Grab the current (already slid) mesh from the node.
        let slid = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return,
        };
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        if factor.abs() < 1e-6 {
            // No movement: restore snapshot (live mesh may still equal orig) and exit without undo.
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload { *mesh = orig; }
            }
            self.poly_request_display_sync(nid);
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload { *mesh = orig.clone(); }
        }
        self.transaction("Slide", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload { *mesh = slid.clone(); }
            }
        });
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_cancel_slide(&mut self) {
        let (nid, orig) = match self.active_slide_ctx() { Some(c) => c, None => return };
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload { *mesh = orig.clone(); }
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_slide_toggle_clamp(&mut self) {
        let mut on = None;
        if let Some(d) = self.tool.current.as_poly_vert_slide_mut() { d.clamp = !d.clamp; on = Some(d.clamp); }
        else if let Some(d) = self.tool.current.as_poly_edge_slide_mut() { d.clamp = !d.clamp; on = Some(d.clamp); }
        if let Some(on) = on { self.toast(if on { "Slide: Clamp ON" } else { "Slide: Clamp OFF" }); }
    }
}
