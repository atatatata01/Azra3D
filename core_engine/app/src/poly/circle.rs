use std::collections::HashSet;
use std::sync::Arc;
use glam::Vec3;
use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

/// Result of a successful Circle apply — seeds the redo panel + viewport radius handle (S4).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CircleApplyInfo {
    pub fit_radius: f32,
    pub center: Vec3,
    /// Unit radial direction in the fit plane for the on-screen radius handle.
    pub handle_dir: Vec3,
    pub plane_normal: Vec3,
}

/// THE single Circle producer: reshape `faces`' boundary loop(s) onto a circle and report the auto-fit
/// radius of the first loop (so the redo panel can seed its slider from the fit instead of a guess).
///
/// One rule, one path — the menu command, the Alt+C keybind and the redo panel all come through here,
/// straight onto `HalfEdgeMesh::circle_region` (CLAUDE.md: never two code paths for one operation; the
/// inset drag shipped a duplicated copy of the kernel's math in the app layer for three rounds — see
/// POLY_POLISH_PLAN §7/§8).
///
/// Returns `None` when the region has no circle-able boundary (empty selection, or a pinched/non-simple
/// loop the kernel refuses). The mesh is untouched in that case.
pub(crate) fn apply_circle(mesh: &mut HalfEdgeMesh, faces: &[u32], radius: Option<f32>, flatten: bool,
                           regular: bool, relax_passes: u32) -> Option<CircleApplyInfo> {
    if faces.is_empty() { return None; }
    let set: HashSet<FaceId> = faces.iter().map(|&f| FaceId(f)).collect();
    let res = mesh.circle_region(&set, radius, flatten, regular, relax_passes);
    res.loops.first().map(|l| {
        let handle_dir = l.boundary.first().map(|(_, d)| *d).unwrap_or(Vec3::X);
        CircleApplyInfo {
            fit_radius: l.radius,
            center: l.center,
            handle_dir,
            plane_normal: l.normal.normalize_or_zero(),
        }
    })
}

/// THE single Circle producer for the EDGE domain — the exact analogue of [`apply_circle`], reshaping a
/// selected EDGE LOOP onto a circle. It shares the reshape core with the face path inside `core_math`
/// (`circle_edge_loop` -> `circle_reshape_loops`, the same math `circle_region` uses), so there is ONE
/// reshape rule, forked only at the "which vertices are the loop" step (faces' boundary vs the selected
/// edges).
///
/// Returns `None` when `edges` is empty or is not a single closed loop (the kernel refuses a pinch /
/// fork / open chain); the mesh is untouched in that case, so the caller's dry-run / undo contract holds
/// identically to the face path. `relax_passes` is threaded for a uniform redo-panel UX but a pure edge
/// loop encloses no interior, so it is a documented no-op here.
pub(crate) fn apply_circle_edges(mesh: &mut HalfEdgeMesh, edges: &[u32], radius: Option<f32>,
                                 flatten: bool, regular: bool, relax_passes: u32) -> Option<CircleApplyInfo> {
    if edges.is_empty() { return None; }
    let set: HashSet<HalfEdgeId> = edges.iter().map(|&e| HalfEdgeId(e)).collect();
    let res = mesh.circle_edge_loop(&set, radius, flatten, regular, relax_passes);
    res.loops.first().map(|l| {
        let handle_dir = l.boundary.first().map(|(_, d)| *d).unwrap_or(Vec3::X);
        CircleApplyInfo {
            fit_radius: l.radius,
            center: l.center,
            handle_dir,
            plane_normal: l.normal.normalize_or_zero(),
        }
    })
}

impl AzraApp {
    /// Alt+C / menu — reshape the selected face region's boundary onto a circle (LoopTools "Circle").
    ///
    /// The selection is LEFT ON THE REGION afterwards: Circle is a mid-workflow step, and the whole
    /// point is to chain it — `Circle -> I (inset) -> E (extrude)` turns a grid into a round boss. It is
    /// a pure reshape, so the face ids survive and the selection needs no repair.
    pub(crate) fn poly_circle_faces(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Modo implicit full selection: empty selection targets EVERY face (SAFE — a pure reshape). The
        // "select first" toast is gone; if the all-face region has no single boundary loop the dry-run
        // below refuses honestly. `poly_faces_or_all` returns a sorted, deterministic set.
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() {
            return;
        }
        let before = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return,
        };
        // Blender's LoopTools defaults: auto radius, flatten, regular. `relax_passes` is AZRA's own
        // (Blender leaves the interior grid alone); the default relaxes so the grid inside the new
        // circle doesn't shear — the owner's ask. Set it to 0 in the redo panel for Blender parity.
        let relax = self.poly_circle_relax;

        // DRY-RUN on a scratch copy first. `circle_region` REFUSES some regions (no simple boundary
        // loop, degenerate plane) and must then change nothing at all — including pushing no undo
        // entry, since `transaction()` snapshots and pushes UNCONDITIONALLY, which would silently eat
        // the user's next Ctrl+Z. Same "ask before acting" shape as `poly_join_has_candidates()`, and
        // cheap in context: `transaction()` already clones the whole scene graph.
        let mut next = (*before).clone();
        let info = match apply_circle(&mut next, &faces, None, true, true, relax) {
            Some(i) => i,
            None => {
                self.toast("Circle: the selected region has no single boundary loop to round");
                return;
            }
        };
        self.transaction("Circle Region", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = Arc::new(next);
                }
            }
            // Pure reshape: every id survives, so the selection stays valid as-is and STAYS on the
            // region (Circle is a mid-workflow step — Circle -> I -> E). Only the screen-space pick
            // cache and the render mesh need refreshing.
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.ui.poly_circle_adjust = Some(crate::types::PolyCircleAdjust {
            nid, faces, edges: None, before, radius: None, fit_radius: info.fit_radius,
            flatten: true, regular: true, relax_passes: relax, needs_settle: false,
            center: info.center, handle_dir: info.handle_dir, plane_normal: info.plane_normal,
            handle_dragging: false,
        });
    }

    /// Alt+C / menu in EDGE mode — reshape the selected EDGE LOOP's vertices onto a circle (LoopTools
    /// "Circle" run on an edge loop). The EDGE-domain twin of [`poly_circle_faces`](Self::poly_circle_faces):
    /// same dry-run-refusal + transaction + Adjust-Circle redo-panel shape, differing ONLY in reading
    /// `selected_edges` and running the edge producer. The reshape math is shared in `core_math`.
    ///
    /// The selection is LEFT on the loop afterwards (pure reshape preserves every id), so it chains the
    /// same way the face path does.
    pub(crate) fn poly_circle_edges(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Modo implicit full selection: empty selection targets EVERY edge (SAFE — a pure reshape). The
        // "select first" toast is gone; the dry-run below refuses honestly if the all-edge set is not a
        // single closed loop. `poly_edges_or_all` returns a sorted, deterministic representative set.
        let edges: Vec<u32> = self.poly_edges_or_all(nid);
        if edges.is_empty() {
            return;
        }
        let before = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return,
        };
        let relax = self.poly_circle_relax;

        // DRY-RUN on a scratch copy first — same refusal contract as poly_circle_faces: circle_edge_loop
        // REFUSES a selection that is not a single closed loop and must then change nothing (no undo
        // entry, since transaction() pushes unconditionally and would otherwise eat the next Ctrl+Z).
        let mut next = (*before).clone();
        let info = match apply_circle_edges(&mut next, &edges, None, true, true, relax) {
            Some(i) => i,
            None => {
                self.toast("Circle: the selected edges are not a single closed loop");
                return;
            }
        };
        self.transaction("Circle Edge Loop", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = Arc::new(next);
                }
            }
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.ui.poly_circle_adjust = Some(crate::types::PolyCircleAdjust {
            nid, faces: Vec::new(), edges: Some(edges), before, radius: None, fit_radius: info.fit_radius,
            flatten: true, regular: true, relax_passes: relax, needs_settle: false,
            center: info.center, handle_dir: info.handle_dir, plane_normal: info.plane_normal,
            handle_dragging: false,
        });
    }

    /// Mode-aware Circle entry (Alt+C keybind + command palette): Face -> region boundary, Edge -> edge
    /// loop, Vertex/Object -> a toast. Both selection domains fire through the ONE shared reshape core;
    /// this only picks which loop-gathering to use.
    pub(crate) fn poly_circle(&mut self) {
        match self.poly_work_mode {
            PolyWorkMode::Face | PolyWorkMode::Group => self.poly_circle_faces(),
            PolyWorkMode::Edge => self.poly_circle_edges(),
            _ => self.toast("Circle: switch to Face or Edge mode and select a loop"),
        }
    }

    /// Blender's "Adjust Last Operation" (F9) redo panel for Circle — it STAYS after confirm so Radius /
    /// Flatten / Regular / Relax can be tweaked with a LIVE re-run. Every change re-runs from the
    /// PRE-circle snapshot (so tweaking never stacks circles) and settles into ONE undo entry on release.
    pub(crate) fn poly_circle_adjust_active(&self) -> bool {
        let Some(a) = self.ui.poly_circle_adjust.as_ref() else {
            return false;
        };
        self.scene.scene_graph.get(a.nid).is_some() && self.sel.usel.active_node() == Some(a.nid)
    }

    /// Shared last-circle UI + apply (Tool Parameters dock + legacy float).
    pub(crate) fn poly_circle_adjust_panel(&mut self, ui: &mut eframe::egui::Ui) -> bool {
        use eframe::egui;
        let mut a = match self.ui.poly_circle_adjust.clone() {
            Some(a) => a,
            None => return false,
        };
        if self.scene.scene_graph.get(a.nid).is_none() || self.sel.usel.active_node() != Some(a.nid)
        {
            self.ui.poly_circle_adjust = None;
            return false;
        }

        let mut changed = false;
        let mut released = false;
        let mut close = false;
        ui.label(
            egui::RichText::new("Last · Circle")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        let mut custom = a.radius.is_some();
        if ui
            .checkbox(&mut custom, "Custom Radius")
            .on_hover_text("Off = auto (least-squares best fit of the boundary loop).")
            .changed()
        {
            a.radius = if custom { Some(a.fit_radius) } else { None };
            changed = true;
            released = true;
        }
        if let Some(r) = a.radius.as_mut() {
            let hi = (a.fit_radius * 4.0).max(1.0);
            let resp = ui.add(egui::Slider::new(r, 0.001..=hi).text("Radius"));
            changed |= resp.changed();
            released |= resp.drag_stopped() || resp.lost_focus();
        }
        let resp = ui.checkbox(&mut a.regular, "Regular").on_hover_text(
            "On (Blender default) = spread the vertices at equal angles.\nOff = every vertex keeps its own angle.",
        );
        if resp.changed() {
            changed = true;
            released = true;
        }
        let resp = ui.checkbox(&mut a.flatten, "Flatten").on_hover_text(
            "On (Blender default) = land the loop exactly in its best-fit plane.\nOff = keep each vertex's offset along the plane normal.",
        );
        if resp.changed() {
            changed = true;
            released = true;
        }
        let resp = ui
            .add(egui::Slider::new(&mut a.relax_passes, 0..=20).text("Relax Interior"))
            .on_hover_text(
                "AZRA extension — Blender's Circle leaves the interior alone (use 0 for parity).\nRelaxes the region's inner grid so it doesn't shear against the new circular boundary.",
            );
        changed |= resp.changed();
        released |= resp.drag_stopped() || resp.lost_focus();
        ui.separator();
        if ui.button("Done").clicked() {
            close = true;
        }

        if changed {
            a.needs_settle = true;
            let nid = a.nid;
            let mut m = (*a.before).clone();
            let info = match &a.edges {
                Some(edges) => {
                    apply_circle_edges(&mut m, edges, a.radius, a.flatten, a.regular, a.relax_passes)
                }
                None => apply_circle(&mut m, &a.faces, a.radius, a.flatten, a.regular, a.relax_passes),
            };
            if let Some(info) = info {
                a.center = info.center;
                a.handle_dir = info.handle_dir;
                a.plane_normal = info.plane_normal;
                if a.radius.is_none() {
                    a.fit_radius = info.fit_radius;
                }
            }
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = Arc::new(m);
                }
            }
            self.poly_circle_relax = a.relax_passes;
            self.poly_invalidate_pick_cache();
            self.poly_request_display_sync(nid);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        self.ui.poly_circle_adjust = Some(a.clone());
        if released {
            self.poly_request_display_sync(a.nid);
            if a.needs_settle {
                self.transaction_snapshot_only("Adjust Circle");
                if let Some(adj) = self.ui.poly_circle_adjust.as_mut() {
                    adj.needs_settle = false;
                }
            }
        }
        if close {
            self.poly_request_display_sync(a.nid);
            self.ui.poly_circle_adjust = None;
        }
        true
    }

    /// Legacy floating window (old UI only).
    pub(crate) fn draw_poly_circle_adjust(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui;
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            return;
        }
        if !self.poly_circle_adjust_active() {
            return;
        }
        egui::Window::new("Adjust Circle")
            .id(egui::Id::new("poly_circle_adjust"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                let _ = self.poly_circle_adjust_panel(ui);
            });
    }

    /// Current world-space radius for the circle handle (custom or fit).
    pub(crate) fn poly_circle_handle_radius(a: &crate::types::PolyCircleAdjust) -> f32 {
        a.radius.unwrap_or(a.fit_radius).max(0.001)
    }

    /// Draw the circle center + radius handle while Adjust Circle is open (S4).
    pub(crate) fn draw_poly_circle_radius_handle(
        &self,
        painter: &eframe::egui::Painter,
        rect: eframe::egui::Rect,
    ) {
        use eframe::egui;
        use crate::world_to_screen;
        let a = match self.ui.poly_circle_adjust.as_ref() {
            Some(a) => a,
            None => return,
        };
        let r = Self::poly_circle_handle_radius(a);
        let tip = a.center + a.handle_dir * r;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let fov = self.render.fov_deg;
        let s0 = world_to_screen(
            a.center,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            fov,
        );
        let s1 = world_to_screen(
            tip,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            fov,
        );
        let (Some(c), Some(h)) = (s0, s1) else {
            return;
        };
        let cyan = egui::Color32::from_rgb(80, 220, 255);
        let dim = egui::Color32::from_rgba_unmultiplied(80, 220, 255, 140);
        painter.line_segment([c, h], egui::Stroke::new(1.5, dim));
        painter.circle_filled(c, 4.0, cyan);
        let hr = if a.handle_dragging { 8.0 } else { 6.5 };
        painter.circle_filled(h, hr, cyan);
        painter.circle_stroke(h, hr, egui::Stroke::new(1.5, egui::Color32::WHITE));
        painter.text(
            egui::pos2(h.x + 10.0, h.y - 10.0),
            egui::Align2::LEFT_BOTTOM,
            format!("R {:.3}", r),
            egui::FontId::proportional(11.0),
            cyan,
        );
    }

    /// Hit-test / drag the circle radius handle. Returns true if the event was consumed.
    pub(crate) fn poly_circle_handle_input(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        primary_down: bool,
        primary_pressed: bool,
        primary_released: bool,
    ) -> bool {
        let Some(mut a) = self.ui.poly_circle_adjust.clone() else {
            return false;
        };
        let r = Self::poly_circle_handle_radius(&a);
        let tip = a.center + a.handle_dir * r;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let fov = self.render.fov_deg;
        let s1 = crate::world_to_screen(
            tip,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            fov,
        );
        let hit = s1.map(|h| {
            let dx = mouse.x - h.x;
            let dy = mouse.y - h.y;
            (dx * dx + dy * dy).sqrt() < 12.0
        }).unwrap_or(false);

        if primary_pressed && hit {
            a.handle_dragging = true;
            // Arm custom radius at current value so auto-fit doesn't fight the drag.
            a.radius = Some(r);
            self.ui.poly_circle_adjust = Some(a);
            return true;
        }
        if a.handle_dragging && primary_down {
            if let Some(hit_pt) = self.ray_plane_hit(mouse, rect, a.center, a.plane_normal) {
                let to = hit_pt - a.center;
                let planar = to - a.plane_normal * to.dot(a.plane_normal);
                let new_r = planar.length().max(0.001);
                a.radius = Some(new_r);
                a.needs_settle = true;
                // Live re-run from pre-circle snapshot.
                let mut m = (*a.before).clone();
                let info = match &a.edges {
                    Some(edges) => {
                        apply_circle_edges(&mut m, edges, a.radius, a.flatten, a.regular, a.relax_passes)
                    }
                    None => apply_circle(&mut m, &a.faces, a.radius, a.flatten, a.regular, a.relax_passes),
                };
                if let Some(info) = info {
                    a.center = info.center;
                    a.handle_dir = info.handle_dir;
                    a.plane_normal = info.plane_normal;
                }
                let nid = a.nid;
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *mesh = Arc::new(m);
                    }
                }
                self.poly_invalidate_pick_cache();
                self.poly_request_display_sync(nid);
                self.mark_dirty(DirtyFlags::RENDER);
            }
            self.ui.poly_circle_adjust = Some(a);
            return true;
        }
        if a.handle_dragging && primary_released {
            a.handle_dragging = false;
            if a.needs_settle {
                self.poly_request_display_sync(a.nid);
                self.transaction_snapshot_only("Adjust Circle");
                a.needs_settle = false;
            }
            self.ui.poly_circle_adjust = Some(a);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::half_edge::VertexId;
    use glam::Vec3;

    /// THE APP-PATH GATE (`feedback_verify_app_path_not_kernel`; POLY_POLISH_PLAN §3.4 "confirm the app
    /// default matches the oracle settings").
    ///
    /// A green kernel test proves nothing about what the I key / Alt+C actually does — the inset even-
    /// offset bug survived precisely because every `core_math` test passed while the app layer called
    /// the kernel differently. So this drives the APP's producer, `apply_circle`, with the EXACT
    /// arguments `poly_circle_faces` passes (`None, true, true` = Blender's auto radius / flatten /
    /// regular) and diffs Blender's real LoopTools output vertex-for-vertex.
    ///
    /// `relax_passes = 0` here because that is Blender's behaviour and this test's job is PARITY; the
    /// shipping default (`poly_circle_relax = 4`) is AZRA's documented extension and is gated on its own
    /// properties in `core_math` (`circle_region_relax_distributes_the_interior_and_pins_the_boundary`).
    #[test]
    fn app_apply_circle_at_shipping_defaults_matches_blender_oracle() {
        // Same input as circle_ref.py ["asym"]: the new_plane(2,2,4,4) grid with vertex 10 displaced.
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        m.vert_mut(VertexId(10)).position = Vec3::new(-0.6, 0.0, 0.0);
        let faces: Vec<u32> = (0..m.faces.len() as u32).collect();

        // EXACTLY what poly_circle_faces() calls (radius=None, flatten=true, regular=true).
        let fit = apply_circle(&mut m, &faces, None, true, true, 0).expect("app path must circle the grid");
        assert!((fit.fit_radius - 1.138111).abs() < 1e-4, "app path reported radius {}, Blender says 1.138111", fit.fit_radius);

        // circle_ref.json ["asym"]["verts"] — the boundary ring (the 9 interior verts must NOT move).
        let oracle: [[f32; 3]; 25] = [
            [-0.779766, 0.0, -0.804766], [-0.410536, 0.0, -1.051478], [ 0.025000, 0.0, -1.138111],
            [ 0.460536, 0.0, -1.051477], [ 0.829766, 0.0, -0.804766], [-1.026477, 0.0, -0.435536],
            [-0.500000, 0.0, -0.500000], [ 0.000000, 0.0, -0.500000], [ 0.500000, 0.0, -0.500000],
            [ 1.076478, 0.0, -0.435536], [-1.113111, 0.0,  0.000000], [-0.500000, 0.0,  0.000000],
            [ 0.000000, 0.0,  0.000000], [ 0.500000, 0.0,  0.000000], [ 1.163111, 0.0,  0.000000],
            [-1.026478, 0.0,  0.435536], [-0.500000, 0.0,  0.500000], [ 0.000000, 0.0,  0.500000],
            [ 0.500000, 0.0,  0.500000], [ 1.076477, 0.0,  0.435536], [-0.779766, 0.0,  0.804766],
            [-0.410536, 0.0,  1.051477], [ 0.025000, 0.0,  1.138111], [ 0.460536, 0.0,  1.051478],
            [ 0.829766, 0.0,  0.804766],
        ];
        for (i, o) in oracle.iter().enumerate() {
            let want = Vec3::new(o[0], o[1], o[2]);
            let got = m.vert(VertexId(i as u32)).position;
            assert!((got - want).length() < 1e-3,
                "APP PATH vertex {i}: {got:?} != Blender LoopTools {want:?}");
        }
    }

    /// The refusal contract the undo path leans on: `poly_circle_faces` dry-runs `apply_circle` and only
    /// opens a `transaction()` when this returns Some, because `transaction()` pushes an undo entry
    /// unconditionally. If a refusal ever started returning Some, a no-op would eat the user's Ctrl+Z.
    #[test]
    fn app_apply_circle_returns_none_when_refused() {
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let before: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        assert!(apply_circle(&mut m, &[], None, true, true, 4).is_none(), "empty selection must refuse");
        // Two diagonal quads touch only at a corner => the boundary pinches => no simple loop.
        assert!(apply_circle(&mut m, &[5, 10], None, true, true, 4).is_none(), "pinched region must refuse");
        let after: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        assert_eq!(before, after, "a refused circle must leave the mesh untouched");
    }

    /// APP-PATH GATE for the EDGE domain (`feedback_verify_app_path_not_kernel`) — a green kernel test
    /// proves nothing about what Alt+C-in-Edge-mode actually runs. This drives the app's EDGE producer
    /// `apply_circle_edges` with the EXACT args `poly_circle_edges` passes (`None, true, true` = auto /
    /// flatten / regular) on the outer boundary edge loop, and diffs Blender LoopTools' EDGE-mode output
    /// (circle_edge_ref.json ["asym_edge"], which is byte-identical to the face oracle) vertex-for-vertex.
    #[test]
    fn app_apply_circle_edges_at_shipping_defaults_matches_blender_oracle() {
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        m.vert_mut(VertexId(10)).position = Vec3::new(-0.6, 0.0, 0.0); // circle_edge_ref.py ["asym_edge"]
        // The outer boundary edge loop = selected_edges the user would paint (Shift+L). Built from the
        // region boundary of all faces so "the loop" is defined the same way the kernel test builds it.
        let all: HashSet<FaceId> = (0..m.faces.len() as u32).map(FaceId).collect();
        let edges: Vec<u32> = m.region_boundary_half_edges(&all).into_iter().map(|h| h.0).collect();
        assert_eq!(edges.len(), 16, "outer boundary loop is 16 edges");

        let fit = apply_circle_edges(&mut m, &edges, None, true, true, 0)
            .expect("app edge path must circle the loop");
        assert!((fit.fit_radius - 1.138111).abs() < 1e-4, "app edge path radius {}, Blender says 1.138111", fit.fit_radius);

        let oracle: [[f32; 3]; 25] = [
            [-0.779766, 0.0, -0.804766], [-0.410536, 0.0, -1.051478], [ 0.025000, 0.0, -1.138111],
            [ 0.460536, 0.0, -1.051477], [ 0.829766, 0.0, -0.804766], [-1.026477, 0.0, -0.435536],
            [-0.500000, 0.0, -0.500000], [ 0.000000, 0.0, -0.500000], [ 0.500000, 0.0, -0.500000],
            [ 1.076478, 0.0, -0.435536], [-1.113111, 0.0,  0.000000], [-0.500000, 0.0,  0.000000],
            [ 0.000000, 0.0,  0.000000], [ 0.500000, 0.0,  0.000000], [ 1.163111, 0.0,  0.000000],
            [-1.026478, 0.0,  0.435536], [-0.500000, 0.0,  0.500000], [ 0.000000, 0.0,  0.500000],
            [ 0.500000, 0.0,  0.500000], [ 1.076477, 0.0,  0.435536], [-0.779766, 0.0,  0.804766],
            [-0.410536, 0.0,  1.051477], [ 0.025000, 0.0,  1.138111], [ 0.460536, 0.0,  1.051478],
            [ 0.829766, 0.0,  0.804766],
        ];
        for (i, o) in oracle.iter().enumerate() {
            let want = Vec3::new(o[0], o[1], o[2]);
            let got = m.vert(VertexId(i as u32)).position;
            assert!((got - want).length() < 1e-3,
                "APP EDGE PATH vertex {i}: {got:?} != Blender LoopTools {want:?}");
        }
    }

    /// Edge-domain twin of the face refusal contract: `poly_circle_edges` dry-runs `apply_circle_edges`
    /// and only opens a `transaction()` on Some, so a non-loop selection must return None (no undo entry
    /// to eat) and leave the mesh untouched.
    #[test]
    fn app_apply_circle_edges_returns_none_when_refused() {
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let before: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        assert!(apply_circle_edges(&mut m, &[], None, true, true, 4).is_none(), "empty selection refuses");
        // A single edge (both endpoints degree 1) is not a closed loop.
        let one = m.region_boundary_half_edges(&[FaceId(0)].into_iter().collect())[0].0;
        assert!(apply_circle_edges(&mut m, &[one], None, true, true, 4).is_none(), "single edge refuses");
        let after: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        assert_eq!(before, after, "a refused edge circle must leave the mesh untouched");
    }
}
