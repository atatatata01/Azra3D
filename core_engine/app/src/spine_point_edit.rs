//! Interactive 3D spine-point editor for `SdfShape::SpinePipe`.
//!
//! Activation: exactly one SpinePipe prim selected in SDF context (not sculpt).
//! Selection lives in `spine_pt_sel` (indices into `points`), not UnifiedSelection.
//!
//! Gestures (parity with polyline `curve_point_edit`):
//! - LMB empty → **append** a point at the hit and drag it (grow the path)
//! - LMB on point → select + drag (move); re-click sole selected → cycle corner type
//! - **Ctrl+drag** (while moving) → snap to visible SDF/mesh surface
//! - Scroll over point → cycle Sharp → Bezier → Chamfer (stored like CurveVertex.bevel_radius)
//! - **Bezier** yellow handles: LMB free; **Ctrl+drag** = both rotate together (same turn, not opposite)
//! - Shift+edge → insert mid-segment + drag
//! - G grab / Delete (≥1 point remains)
//! - Drag does **not** arm the scene SelectionBox (spine_pt_drag is outside ActiveTool)

use eframe::egui;
use glam::Vec3;
use core_math::primitives::SdfShape;
use core_math::scene_graph::BodyTransform;
use crate::*;

const SPINE_PT_PICK_PX: f32 = 12.0;
/// Bezier handle knobs — slightly larger than control points so yellow tips win the pick.
const SPINE_HANDLE_PICK_PX: f32 = 16.0;
const SPINE_EDGE_PICK_PX: f32 = 10.0;
const SPINE_DRAG_PX2: f32 = 16.0;

/// In-progress spine point drag (not an ActiveTool — kept on AzraApp for a small surface).
#[derive(Clone)]
pub(crate) struct SpinePtDrag {
    pub prim_idx: usize,
    /// (point index, local position at drag start)
    pub originals: Vec<(usize, Vec3)>,
    pub start_mouse: egui::Pos2,
    pub start_hit: Vec3,
    pub plane_o: Vec3,
    pub plane_n: Vec3,
    pub moved: bool,
    /// 0 = move, 1 = created-on-press (cancel removes if unmoved),
    /// 2 = re-click (unused), 3 = scale about centroid, 4 = rotate/twist about view axis,
    /// 5 = Bezier handle drag.
    pub press_kind: u8,
    /// Point under press when press_kind==2 (cycle candidate).
    /// For press_kind==5: control-point index of the handle being dragged.
    pub cycle_pi: Option<usize>,
    /// Local centroid at scale/rotate drag start (press_kind 3/4).
    pub scale_centroid: Vec3,
    /// Mouse distance from centroid-screen at scale start (press_kind 3).
    pub scale_start_dist: f32,
    /// Screen-space angle (atan2) at rotate start (press_kind 4).
    pub rotate_start_angle: f32,
    /// Keyboard modal (G/S/R from key): follows mouse without LMB; confirm on LMB click / Enter.
    /// LMB-initiated move/add: false — update only while LMB held, confirm on release.
    pub modal: bool,
    /// Radius at scale start when scaling tube radius (single-point / radius mode).
    pub start_radius: f32,
    /// True when press_kind==3 scales `SpinePipe.radius` instead of point positions.
    /// For press_kind==5: true = handle_out, false = handle_in.
    pub scale_radius: bool,
}

/// Keep `joins` length in sync with `points` (missing → Sharp = 0).
fn spine_sync_joins(points: &[Vec3], joins: &mut Vec<f32>) {
    joins.resize(points.len(), 0.0);
}

/// Keep per-point radius scales in sync (missing → 1.0).
fn spine_sync_scales(points: &[Vec3], scales: &mut Vec<f32>) {
    let old = scales.len();
    scales.resize(points.len(), 1.0);
    for s in scales.iter_mut().skip(old) {
        *s = 1.0;
    }
}

/// Keep Bezier handles parallel to points (missing → ZERO = auto when Bezier).
fn spine_sync_handles(points: &[Vec3], hin: &mut Vec<Vec3>, hout: &mut Vec<Vec3>) {
    hin.resize(points.len(), Vec3::ZERO);
    hout.resize(points.len(), Vec3::ZERO);
}

/// Sync joins + point_scale + handles after any points mutation.
fn spine_sync_all(
    points: &[Vec3],
    joins: &mut Vec<f32>,
    scales: &mut Vec<f32>,
    hin: &mut Vec<Vec3>,
    hout: &mut Vec<Vec3>,
) {
    spine_sync_joins(points, joins);
    spine_sync_scales(points, scales);
    spine_sync_handles(points, hin, hout);
}

/// Insert a point at `at` with DEFAULT per-point attributes (Sharp join, scale 1.0, zero
/// handles) into EVERY parallel array atomically. Sync runs FIRST (against the pre-insert
/// point count) so legacy prims with short arrays can't shift attribution. Inserting into
/// `points` alone and letting sync_all append at the END shifts every attribute from `at`
/// onward — the "new point inherits the old point's radius scale" bug.
fn spine_insert_point_at(
    points: &mut Vec<Vec3>,
    joins: &mut Vec<f32>,
    scales: &mut Vec<f32>,
    hin: &mut Vec<Vec3>,
    hout: &mut Vec<Vec3>,
    at: usize,
    pos: Vec3,
) -> usize {
    spine_sync_all(points, joins, scales, hin, hout);
    let at = at.min(points.len());
    points.insert(at, pos);
    joins.insert(at, 0.0);
    scales.insert(at, 1.0);
    hin.insert(at, Vec3::ZERO);
    hout.insert(at, Vec3::ZERO);
    at
}

/// Remove the point at `at` from EVERY parallel array atomically (sync first — see
/// `spine_insert_point_at` for why truncate-from-END removal shifts attribution).
fn spine_remove_point_at(
    points: &mut Vec<Vec3>,
    joins: &mut Vec<f32>,
    scales: &mut Vec<f32>,
    hin: &mut Vec<Vec3>,
    hout: &mut Vec<Vec3>,
    at: usize,
) {
    spine_sync_all(points, joins, scales, hin, hout);
    if at < points.len() {
        points.remove(at);
        joins.remove(at);
        scales.remove(at);
        hin.remove(at);
        hout.remove(at);
    }
}

/// Next corner mode: 0 sharp / 1 bezier / −1 chamfer (matches curve_point_edit packing).
fn spine_next_corner_mode(cur: f32, forward: bool) -> i8 {
    let mode: i8 = if cur.abs() <= 1e-6 { 0 } else if cur > 0.0 { 1 } else { -1 };
    if forward {
        match mode { 0 => 1, 1 => -1, _ => 0 }
    } else {
        match mode { 0 => -1, -1 => 1, _ => 0 }
    }
}

fn spine_join_label(j: f32) -> &'static str {
    if j.abs() <= 1e-6 { "Sharp" } else if j > 0.0 { "Bezier" } else { "Chamfer" }
}

impl AzraApp {
    /// Active only while `ActiveTool::SpinePointEdit` owns the tool slot (exclusive modal).
    pub(crate) fn spine_edit_prim_index(&self) -> Option<usize> {
        let uid = match &self.tool.current {
            ActiveTool::SpinePointEdit { prim_uid } => *prim_uid,
            _ => return None,
        };
        let idx = self.prim_index_by_uid(uid)?;
        if idx >= self.scene.primitives.len() { return None; }
        matches!(self.scene.primitives[idx].shape, SdfShape::SpinePipe { .. }).then_some(idx)
    }

    /// Enter exclusive spine point-edit tool (create path + re-enter from Object mode).
    pub(crate) fn spine_enter_edit(&mut self, idx: usize) {
        if idx >= self.scene.primitives.len() { return; }
        if !matches!(self.scene.primitives[idx].shape, SdfShape::SpinePipe { .. }) { return; }
        let uid = self.scene.primitives[idx].prim_uid;
        if self.tool.work_mode != WorkMode::Vertex {
            self.switch_work_mode(WorkMode::Vertex);
        }
        self.select_single(idx);
        self.set_gizmo_armed(false);
        self.spine_pt_sel.clear();
        self.spine_pt_sel.insert(0);
        self.spine_pt_owner = Some(uid);
        // Modal tool state — other shortcuts must not fire (is_busy + keyboard early-out).
        if self.tool.current.is_spine_point_edit() {
            // already in tool — refresh uid only
            self.tool.current = ActiveTool::SpinePointEdit { prim_uid: uid };
        } else {
            self.start_operation(ActiveTool::SpinePointEdit { prim_uid: uid });
        }
        self.toast("Spine: select point · Ctrl+click before/after · Shift multi · RMB delete");
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Exit exclusive spine tool → Object mode, prim stays selected for G/R/S.
    pub(crate) fn spine_exit_tool(&mut self) {
        if self.spine_pt_drag.is_some() {
            self.spine_pt_confirm_drag();
        }
        self.spine_pt_sel.clear();
        self.spine_pt_hover = None;
        self.set_gizmo_armed(false);
        self.ui.spine_corner_pie.open = false;
        if self.tool.current.is_spine_point_edit() {
            self.tool.current = ActiveTool::Idle;
        }
        if self.tool.work_mode != WorkMode::Object {
            self.switch_work_mode(WorkMode::Object);
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    fn spine_points(&self, idx: usize) -> Vec<Vec3> {
        match &self.scene.primitives[idx].shape {
            SdfShape::SpinePipe { points, .. } => points.clone(),
            _ => Vec::new(),
        }
    }

    fn spine_joins(&self, idx: usize) -> Vec<f32> {
        match &self.scene.primitives[idx].shape {
            SdfShape::SpinePipe { joins, points, .. } => {
                let mut j = joins.clone();
                spine_sync_joins(points, &mut j);
                j
            }
            _ => Vec::new(),
        }
    }

    /// Resolved (in, out) handle offsets for drawing/picking — auto when Bezier + zero storage.
    fn spine_resolved_handles(&self, idx: usize) -> Vec<(Vec3, Vec3)> {
        match &self.scene.primitives[idx].shape {
            SdfShape::SpinePipe {
                points, joins, handles_in, handles_out, closed, ..
            } => {
                let n = points.len();
                (0..n)
                    .map(|i| {
                        let hin = core_math::curves::spine_resolve_handle_in(
                            points, joins, handles_in, i, *closed,
                        );
                        let hout = core_math::curves::spine_resolve_handle_out(
                            points, joins, handles_out, i, *closed,
                        );
                        (hin, hout)
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Pick a Bezier handle under the cursor. Returns (point_index, is_out).
    fn spine_pick_handle(&self, idx: usize, mouse: egui::Pos2, rect: egui::Rect) -> Option<(usize, bool)> {
        let pts = self.spine_points(idx);
        let joins = self.spine_joins(idx);
        let handles = self.spine_resolved_handles(idx);
        let mut best: Option<(f32, usize, bool)> = None;
        for (i, local) in pts.iter().enumerate() {
            // Only Bezier points expose handles.
            if joins.get(i).copied().unwrap_or(0.0) <= 1e-6 {
                continue;
            }
            let (hin, hout) = handles.get(i).copied().unwrap_or((Vec3::ZERO, Vec3::ZERO));
            for (is_out, hoff) in [(false, hin), (true, hout)] {
                if hoff.length_squared() < 1e-12 {
                    continue;
                }
                let hw = self.spine_local_to_world(idx, *local + hoff);
                let Some(sp) = self.spine_screen(hw, rect) else { continue };
                let d = (sp - mouse).length();
                if d <= SPINE_HANDLE_PICK_PX && best.map_or(true, |(bd, _, _)| d < bd) {
                    best = Some((d, i, is_out));
                }
            }
        }
        best.map(|(_, i, o)| (i, o))
    }

    fn spine_pt_start_handle_drag(
        &mut self,
        idx: usize,
        m: egui::Pos2,
        rect: egui::Rect,
        pi: usize,
        is_out: bool,
    ) {
        let pts = self.spine_points(idx);
        if pi >= pts.len() { return; }
        let closed = matches!(
            self.scene.primitives[idx].shape,
            SdfShape::SpinePipe { closed: true, .. }
        );
        // Materialize current resolved handle into originals (local offset).
        let (hin0, hout0) = match &self.scene.primitives[idx].shape {
            SdfShape::SpinePipe { points, joins, handles_in, handles_out, .. } => {
                let hin = core_math::curves::spine_resolve_handle_in(
                    points, joins, handles_in, pi, closed,
                );
                let hout = core_math::curves::spine_resolve_handle_out(
                    points, joins, handles_out, pi, closed,
                );
                (hin, hout)
            }
            _ => return,
        };
        let mut orig_h = if is_out { hout0 } else { hin0 };
        let mut sib = if is_out { hin0 } else { hout0 };
        let anchor = pts[pi];
        // Plane through **this selected control point**, workplane normal (not global WP origin).
        let (plane_o, plane_n) = self.spine_plane_through_point(idx, pi);
        // Snap existing handle offsets onto that plane so yellow knobs start coplanar.
        let tip0 = self.spine_project_onto_point_plane(
            idx,
            pi,
            self.spine_local_to_world(idx, anchor + orig_h),
        );
        orig_h = self.spine_world_to_local(idx, tip0) - anchor;
        let sib0 = self.spine_project_onto_point_plane(
            idx,
            pi,
            self.spine_local_to_world(idx, anchor + sib),
        );
        sib = self.spine_world_to_local(idx, sib0) - anchor;

        let Some(start_hit) = self.spine_place_handle_tip(idx, pi, m, rect) else {
            self.toast("Aim at the workplane to move the handle");
            return;
        };
        if !self.has_active_episode() {
            self.begin_episode("Spine Bezier Handle");
        }
        self.spine_pt_drag = Some(SpinePtDrag {
            prim_idx: idx,
            originals: vec![(pi, orig_h), (pi, sib)],
            start_mouse: m,
            start_hit,
            plane_o,
            plane_n,
            moved: false,
            press_kind: 5,
            cycle_pi: Some(pi),
            scale_centroid: anchor,
            scale_start_dist: 1.0,
            rotate_start_angle: 0.0,
            modal: false,
            start_radius: 0.0,
            scale_radius: is_out, // true = out
        });
        self.spine_pt_update_drag(m, rect, false, false, false);
    }

    /// Prim-local → world (includes body transform).
    fn spine_local_to_world(&self, idx: usize, local: Vec3) -> Vec3 {
        let prim = &self.scene.primitives[idx];
        let rq = prim.rotation_quat();
        let s = prim.scale;
        let p = prim.position;
        let bt: BodyTransform = self.scene.prim_body_map.get(idx)
            .and_then(|(nid, _)| self.scene.scene_graph.body_world_transform(*nid))
            .unwrap_or_default();
        bt.local_to_world(rq * (local * s) + p)
    }

    /// World → prim-local.
    fn spine_world_to_local(&self, idx: usize, world: Vec3) -> Vec3 {
        let prim = &self.scene.primitives[idx];
        let rq = prim.rotation_quat();
        let s = prim.scale;
        let p = prim.position;
        let bt: BodyTransform = self.scene.prim_body_map.get(idx)
            .and_then(|(nid, _)| self.scene.scene_graph.body_world_transform(*nid))
            .unwrap_or_default();
        let body_local = bt.world_to_local(world);
        let unrot = rq.inverse() * (body_local - p);
        Vec3::new(
            unrot.x / s.x.abs().max(1e-8) * if s.x < 0.0 { -1.0 } else { 1.0 },
            unrot.y / s.y.abs().max(1e-8) * if s.y < 0.0 { -1.0 } else { 1.0 },
            unrot.z / s.z.abs().max(1e-8) * if s.z < 0.0 { -1.0 } else { 1.0 },
        )
    }

    fn spine_cam(&self, rect: egui::Rect) -> (Vec3, glam::Quat, f32, f32) {
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance * (self.render.fov_deg * 0.5).to_radians().tan()
        } else { 0.0 };
        (cam_pos, self.viewport.camera.orbit, aspect, ortho_hw)
    }

    fn spine_screen(&self, world: Vec3, rect: egui::Rect) -> Option<egui::Pos2> {
        let (cam, orbit, aspect, ortho) = self.spine_cam(rect);
        world_to_screen(world, cam, orbit, rect, aspect, ortho, self.render.fov_deg)
    }

    /// Point that anchors the creation plane origin:
    /// **last selected** (max index if multi), else hover, else last path point.
    fn spine_place_anchor_pi(&self, idx: usize) -> Option<usize> {
        let n = self.spine_points(idx).len();
        if n == 0 {
            return None;
        }
        if let Some(pi) = self.spine_pt_sel.iter().copied().filter(|&i| i < n).max() {
            return Some(pi);
        }
        if let Some(h) = self.spine_pt_hover.filter(|&i| i < n) {
            return Some(h);
        }
        Some(n - 1)
    }

    /// Workplane normal from `wp_resolved` (same source as workplane.rs).
    fn spine_wp_normal(&self) -> Vec3 {
        let mut n = self.viewport.wp_resolved.normal;
        if n.length_squared() < 0.25 {
            Vec3::Y
        } else {
            n.normalize()
        }
    }

    /// Plane through a specific control point, workplane normal.
    /// Used for Bezier handles on the selected vertex (same rule as create).
    fn spine_plane_through_point(&self, idx: usize, pi: usize) -> (Vec3, Vec3) {
        let n = self.spine_wp_normal();
        let o = self
            .spine_points(idx)
            .get(pi)
            .map(|local| self.spine_local_to_world(idx, *local))
            .unwrap_or(self.viewport.wp_resolved.origin);
        (o, n)
    }

    /// Creation plane: **workplane normal** + origin at the **last selected** point.
    fn spine_creation_plane(&self, idx: usize) -> (Vec3, Vec3) {
        let n = self.spine_wp_normal();
        let o = self
            .spine_place_anchor_pi(idx)
            .and_then(|pi| self.spine_points(idx).get(pi).copied())
            .map(|local| self.spine_local_to_world(idx, local))
            .unwrap_or(self.viewport.wp_resolved.origin);
        (o, n)
    }

    /// Project a world point onto the plane through control point `pi` (WP normal).
    fn spine_project_onto_point_plane(&self, idx: usize, pi: usize, world: Vec3) -> Vec3 {
        let (o, n) = self.spine_plane_through_point(idx, pi);
        world - n * (world - o).dot(n)
    }

    /// World hit for **new** points: plane through last selected point, WP normal.
    fn spine_place_world(
        &self,
        idx: usize,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> Option<Vec3> {
        let (o, n) = self.spine_creation_plane(idx);
        self.ray_plane_hit(mouse, rect, o, n)
    }

    /// Place a spine control point: creation-plane hit → prim-local.
    fn spine_place_local(
        &self,
        idx: usize,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> Option<Vec3> {
        let world = self.spine_place_world(idx, mouse, rect)?;
        Some(self.spine_world_to_local(idx, world))
    }

    /// Bezier handle tip under mouse: plane through **that** control point, WP normal.
    fn spine_place_handle_tip(
        &self,
        idx: usize,
        pi: usize,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> Option<Vec3> {
        let (o, n) = self.spine_plane_through_point(idx, pi);
        self.ray_plane_hit(mouse, rect, o, n)
    }

    // ---- tick / pick ----

    pub(crate) fn spine_pt_tick(&mut self, mouse: Option<egui::Pos2>, rect: egui::Rect) {
        let idx = match self.spine_edit_prim_index() {
            Some(i) => i,
            None => {
                if !self.spine_pt_sel.is_empty() { self.spine_pt_sel.clear(); }
                self.spine_pt_hover = None;
                self.spine_pt_owner = None;
                if self.spine_pt_drag.is_some() { self.spine_pt_cancel_drag(); }
                return;
            }
        };
        let uid = self.scene.primitives[idx].prim_uid;
        if self.spine_pt_owner != Some(uid) {
            self.spine_pt_sel.clear();
            self.spine_pt_owner = Some(uid);
        }
        let n = self.spine_points(idx).len();
        self.spine_pt_sel.retain(|&i| i < n);
        self.spine_pt_hover = mouse.and_then(|m| self.spine_pick_point(idx, m, rect));
    }

    fn spine_pick_point(&self, idx: usize, mouse: egui::Pos2, rect: egui::Rect) -> Option<usize> {
        let pts = self.spine_points(idx);
        let mut best: Option<(usize, f32)> = None;
        for (i, local) in pts.iter().enumerate() {
            let w = self.spine_local_to_world(idx, *local);
            let Some(sp) = self.spine_screen(w, rect) else { continue };
            let d2 = (sp - mouse).length_sq();
            if d2 <= SPINE_PT_PICK_PX * SPINE_PT_PICK_PX {
                if best.map_or(true, |(_, bd)| d2 < bd) {
                    best = Some((i, d2));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    /// Nearest spine segment under cursor; returns insert index (after edge start) and local hit.
    fn spine_pick_edge(&self, idx: usize, mouse: egui::Pos2, rect: egui::Rect)
        -> Option<(usize, Vec3)>
    {
        let pts = self.spine_points(idx);
        let n = pts.len();
        if n < 2 { return None; }
        let closed = matches!(
            self.scene.primitives[idx].shape,
            SdfShape::SpinePipe { closed: true, .. }
        );
        let mut best: Option<(usize, f32, Vec3)> = None;
        let segs = if closed { n } else { n - 1 };
        for i in 0..segs {
            let j = (i + 1) % n;
            let wa = self.spine_local_to_world(idx, pts[i]);
            let wb = self.spine_local_to_world(idx, pts[j]);
            let Some(sa) = self.spine_screen(wa, rect) else { continue };
            let Some(sb) = self.spine_screen(wb, rect) else { continue };
            let ab = sb - sa;
            let ab2 = ab.length_sq().max(1e-6);
            let t = ((mouse - sa).dot(ab) / ab2).clamp(0.0, 1.0);
            let closest = sa + ab * t;
            let d = (mouse - closest).length();
            if d <= SPINE_EDGE_PICK_PX {
                let local = pts[i].lerp(pts[j], t);
                if best.map_or(true, |(_, bd, _)| d < bd) {
                    best = Some((j, d, local)); // insert *before* j (= after i)
                }
            }
        }
        best.map(|(ins, _, local)| (ins, local))
    }

    // ---- corner type (polyline parity) ----

    fn spine_pt_cycle_corner(&mut self, idx: usize, pi: usize, forward: bool) {
        let joins = self.spine_joins(idx);
        if pi >= joins.len() { return; }
        let cur = joins[pi];
        let next_mode = spine_next_corner_mode(cur, forward);
        let pts = self.spine_points(idx);
        let default_r = if pts.len() >= 2 {
            let prev = pts[pi.saturating_sub(1).min(pts.len() - 1)];
            let next = pts[(pi + 1).min(pts.len() - 1)];
            let b = pts[pi];
            ((prev - b).length().min((next - b).length()) * 0.3).max(0.02)
        } else {
            0.05
        };
        let mag = if cur.abs() > 1e-6 { cur.abs() } else { default_r };
        let next = match next_mode {
            1 => mag,
            -1 => -mag,
            _ => 0.0,
        };
        let label = spine_join_label(next);
        self.transaction("Spine corner", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                    spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                    if pi < joins.len() {
                        joins[pi] = next;
                    }
                }
            });
        });
        self.toast(&format!("Spine corner: {}", label));
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Scroll over a point/selection → cycle corner type. Returns true if consumed.
    pub(crate) fn spine_pt_scroll_cycle(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect, forward: bool) -> bool {
        let hp = match self.spine_pick_point(idx, m, rect) { Some(p) => p, None => return false };
        let n = self.spine_points(idx).len();
        if hp >= n { return false; }
        let targets: Vec<usize> = if self.spine_pt_sel.contains(&hp) && self.spine_pt_sel.len() > 1 {
            self.spine_pt_sel.iter().copied().filter(|&i| i < n).collect()
        } else {
            vec![hp]
        };
        for &t in &targets {
            self.spine_pt_cycle_corner(idx, t, forward);
        }
        true
    }

    // ---- selection / press ----

    /// LMB press. Returns true if consumed.
    ///
    /// - click/drag on point → select + move
    /// - **Shift+click point** → multi-select toggle (add/remove from selection)
    /// - **Ctrl+click** → **add** point at mouse:
    ///   - with a point selected → insert **before or after** that point (from mouse side)
    ///   - no selection + on edge → split edge
    ///   - no selection + empty → append
    /// - plain empty → clear point sel
    pub(crate) fn spine_pt_press(
        &mut self,
        idx: usize,
        m: egui::Pos2,
        rect: egui::Rect,
        shift: bool,
        ctrl: bool,
    ) -> bool {
        // ── Ctrl+click = add point ─────────────────────────────────────────
        if ctrl && !shift {
            return self.spine_pt_ctrl_add(idx, m, rect);
        }

        // ── Bezier handle hit (before control points) ──────────────────────
        if !shift {
            if let Some((pi, is_out)) = self.spine_pick_handle(idx, m, rect) {
                self.spine_pt_sel.clear();
                self.spine_pt_sel.insert(pi);
                self.spine_pt_start_handle_drag(idx, m, rect, pi, is_out);
                return true;
            }
        }

        // ── Point hit ──────────────────────────────────────────────────────
        if let Some(pi) = self.spine_pick_point(idx, m, rect) {
            if shift {
                // Multi-select toggle — no drag
                if !self.spine_pt_sel.remove(&pi) {
                    self.spine_pt_sel.insert(pi);
                }
                self.mark_dirty(DirtyFlags::RENDER);
                return true;
            }
            // Plain click: if already multi-selected and this is in sel, keep multi for drag
            if !self.spine_pt_sel.contains(&pi) {
                self.spine_pt_sel.clear();
                self.spine_pt_sel.insert(pi);
            }
            self.spine_pt_start_drag_ex(idx, m, rect, 0, None);
            return true;
        }

        // ── Edge hit (no ctrl) ─────────────────────────────────────────────
        if let Some((ins, _local)) = self.spine_pick_edge(idx, m, rect) {
            let n = self.spine_points(idx).len();
            let a = if ins == 0 { n - 1 } else { ins - 1 };
            if shift {
                self.spine_pt_sel.insert(a);
                self.spine_pt_sel.insert(ins % n);
            } else {
                self.spine_pt_sel.clear();
                self.spine_pt_sel.insert(a);
                self.spine_pt_sel.insert(ins % n);
                self.spine_pt_start_drag_ex(idx, m, rect, 0, None);
            }
            self.mark_dirty(DirtyFlags::RENDER);
            return true;
        }

        // Empty: plain clears sel; shift keeps multi-sel (so you can keep building selection)
        if !shift {
            self.spine_pt_sel.clear();
            self.mark_dirty(DirtyFlags::RENDER);
        }
        true
    }

    /// Ctrl+click add: plane through **last selected** point, workplane normal.
    fn spine_pt_ctrl_add(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect) -> bool {
        if self.spine_points(idx).len() >= core_math::curves::MAX_SPINE_POINTS {
            self.toast(&format!(
                "Spine max {} points (GPU bake budget)",
                core_math::curves::MAX_SPINE_POINTS
            ));
            return true;
        }
        let Some(local) = self.spine_place_local(idx, m, rect) else {
            self.toast("Aim at the workplane to place a point");
            return true;
        };
        let pts = self.spine_points(idx);
        let n = pts.len();
        let closed = matches!(
            self.scene.primitives[idx].shape,
            SdfShape::SpinePipe { closed: true, .. }
        );

        // Prefer insert before/after the active selected point (or sole selection).
        let anchor = if self.spine_pt_sel.len() == 1 {
            self.spine_pt_sel.iter().next().copied()
        } else if self.spine_pt_sel.len() > 1 {
            self.spine_pt_sel.iter().copied().max()
        } else {
            self.spine_pt_hover
        };

        if let Some(si) = anchor.filter(|&i| i < n) {
            let insert_at = self.spine_insert_index_around(si, local, &pts, closed);
            return self.spine_pt_insert_and_drag(idx, m, rect, insert_at, local);
        }

        // No selection: if near an edge in screen space, insert on that edge index but
        // still use the **workplane** hit as the point position (not the 3D edge lerp).
        if let Some((ins, _edge_local)) = self.spine_pick_edge(idx, m, rect) {
            return self.spine_pt_insert_and_drag(idx, m, rect, ins, local);
        }
        self.spine_pt_insert_and_drag(idx, m, rect, n, local)
    }

    /// Decide insert index relative to selected point `si` from mouse local pos.
    /// Returns index into `points` for `insert` (0..=n).
    ///
    /// Open path: **first or last** selection always inserts **after** that point
    /// (grow the chain outward — never before the start or before the end tip).
    fn spine_insert_index_around(
        &self,
        si: usize,
        local: Vec3,
        pts: &[Vec3],
        closed: bool,
    ) -> usize {
        let n = pts.len();
        if n == 0 { return 0; }
        if n == 1 {
            // Only seed: always become second point (after).
            return 1;
        }

        // Open ends: always after the selected first/last point.
        if !closed {
            if si == 0 {
                return 1; // after first
            }
            if si + 1 == n {
                return n; // append after last
            }
        }

        let p = pts[si];
        let to_m = local - p;
        let has_prev = closed || si > 0;
        let has_next = closed || si + 1 < n;
        let prev_i = if has_prev { Some(if si == 0 { n - 1 } else { si - 1 }) } else { None };
        let next_i = if has_next { Some((si + 1) % n) } else { None };

        let score = |j: usize| -> f32 {
            let dir = pts[j] - p;
            let len = dir.length();
            if len < 1e-8 {
                return -to_m.length();
            }
            to_m.dot(dir / len)
        };

        // Interior (or closed loop): mouse side chooses before vs after.
        match (prev_i, next_i) {
            (Some(pi), Some(ni)) => {
                if score(ni) >= score(pi) {
                    (si + 1).min(n)
                } else {
                    si
                }
            }
            (None, Some(_)) => si + 1,
            (Some(_), None) => n,
            (None, None) => n,
        }
    }

    /// RMB on a point (or on selection) → delete. Returns true if consumed (suppress context menu).
    pub(crate) fn spine_pt_rmb_delete(&mut self, m: egui::Pos2, rect: egui::Rect) -> bool {
        let Some(idx) = self.spine_edit_prim_index() else { return false };
        let Some(pi) = self.spine_pick_point(idx, m, rect) else { return false };
        // If hit is part of multi-sel, delete whole selection; else just the hit point.
        if self.spine_pt_sel.contains(&pi) && self.spine_pt_sel.len() > 1 {
            // keep sel as-is for delete
        } else {
            self.spine_pt_sel.clear();
            self.spine_pt_sel.insert(pi);
        }
        self.spine_pt_delete();
        true
    }

    fn spine_pt_insert_and_drag(
        &mut self,
        idx: usize,
        m: egui::Pos2,
        rect: egui::Rect,
        at: usize,
        local: Vec3,
    ) -> bool {
        if self.spine_points(idx).len() >= core_math::curves::MAX_SPINE_POINTS {
            self.toast(&format!(
                "Spine max {} points (GPU bake budget)",
                core_math::curves::MAX_SPINE_POINTS
            ));
            return true;
        }
        // Single episode for add + follow drag (start_drag_ex reuses it).
        if !self.has_active_episode() {
            self.begin_episode("Add Spine Point");
        }
        let insert_at = {
            let mut out = at;
            self.write_prim(idx, |p| {
                if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                    out = spine_insert_point_at(points, joins, point_scale, handles_in, handles_out, at, local);
                }
            });
            out
        };
        self.spine_pt_sel.clear();
        self.spine_pt_sel.insert(insert_at);
        // Force a drag even if plane hit is flaky — fall back to workplane place.
        if !self.spine_pt_try_start_drag(idx, m, rect, 1) {
            // Point already inserted; commit without follow-drag.
            self.rebake_no_undo();
            self.commit_episode();
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast("Spine point added");
        }
        true
    }

    /// Like start_drag_ex but returns false if the workplane ray failed.
    fn spine_pt_try_start_drag(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect, press_kind: u8) -> bool {
        let pts = self.spine_points(idx);
        let mut originals: Vec<(usize, Vec3)> = self.spine_pt_sel.iter()
            .filter_map(|&i| pts.get(i).map(|p| (i, *p)))
            .collect();
        if originals.is_empty() { return false; }
        originals.sort_by_key(|(i, _)| *i);

        // Create-drag uses plane through last selected / last path point.
        // Move-drag freezes the resolved workplane (wp origin + normal).
        let (plane_o, plane_n, start_hit) = if press_kind == 1 {
            let (o, n) = self.spine_creation_plane(idx);
            let Some(hit) = self.ray_plane_hit(m, rect, o, n) else { return false };
            (o, n, hit)
        } else {
            let o = self.viewport.wp_resolved.origin;
            let n = self.viewport.wp_resolved.normal;
            let Some(hit) = self.ray_workplane_hit(m, rect) else { return false };
            (o, n, hit)
        };

        if !self.has_active_episode() {
            let label = if press_kind == 1 { "Add Spine Point" } else { "Move Spine Point" };
            self.begin_episode(label);
        }
        self.spine_pt_drag = Some(SpinePtDrag {
            prim_idx: idx,
            originals,
            start_mouse: m,
            start_hit,
            plane_o,
            plane_n,
            moved: press_kind == 1,
            press_kind,
            cycle_pi: None,
            scale_centroid: Vec3::ZERO,
            scale_start_dist: 1.0,
            rotate_start_angle: 0.0,
            modal: false,
            start_radius: 0.0,
            scale_radius: false,
        });
        if press_kind == 1 {
            self.spine_pt_update_drag(m, rect, false, false, false);
        }
        true
    }

    /// Exit point-edit → Object mode with the prim still selected (G/R/S on whole object).
    pub(crate) fn spine_finish_edit(&mut self) {
        self.spine_exit_tool();
        self.toast("Spine object mode — G/R/S · Space deselect");
    }

    /// Apply join type from B-pie (0=Sharp, 1=Round, 2=Chamfer).
    /// No selection required: uses selection if any, else hover, else **all** points.
    pub(crate) fn spine_apply_join_type(&mut self, mode: usize) {
        let Some(idx) = self.spine_edit_prim_index() else { return };
        let n = self.spine_points(idx).len();
        if n == 0 { return; }
        let mut targets: Vec<usize> = self.spine_pt_sel.iter().copied().filter(|&i| i < n).collect();
        if targets.is_empty() {
            if let Some(h) = self.spine_pt_hover {
                if h < n { targets.push(h); }
            }
        }
        if targets.is_empty() {
            targets = (0..n).collect(); // whole spine
        }
        let pts = self.spine_points(idx);
        let default_r = 0.05f32;
        let mag_for = |pi: usize| -> f32 {
            if pts.len() >= 2 {
                let b = pts[pi.min(pts.len() - 1)];
                let prev = pts[pi.saturating_sub(1)];
                let next = pts[(pi + 1).min(pts.len() - 1)];
                ((prev - b).length().min((next - b).length()) * 0.3).max(0.02)
            } else {
                default_r
            }
        };
        let closed = matches!(
            self.scene.primitives[idx].shape,
            SdfShape::SpinePipe { closed: true, .. }
        );
        let label = match mode {
            1 => "Bezier",
            2 => "Chamfer",
            _ => "Sharp",
        };
        // Auto handles: project tips onto plane through **that** control point (WP normal).
        let mut planned: Vec<(usize, f32, Vec3, Vec3)> = Vec::new();
        for &t in &targets {
            let jv = match mode {
                1 => mag_for(t),
                2 => -mag_for(t),
                _ => 0.0,
            };
            if mode == 1 {
                let (mut hin, mut hout) =
                    core_math::curves::spine_auto_handles(&pts, t, closed);
                let anchor = pts.get(t).copied().unwrap_or(Vec3::ZERO);
                for h in [&mut hin, &mut hout] {
                    let tip_w = self.spine_local_to_world(idx, anchor + *h);
                    let tip_on = self.spine_project_onto_point_plane(idx, t, tip_w);
                    *h = self.spine_world_to_local(idx, tip_on) - anchor;
                }
                planned.push((t, jv, hin, hout));
            } else {
                planned.push((t, jv, Vec3::ZERO, Vec3::ZERO));
            }
        }
        self.transaction("Spine corner", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::SpinePipe {
                    points, joins, point_scale, handles_in, handles_out, ..
                } = &mut p.shape
                {
                    spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                    for &(t, jv, hin, hout) in &planned {
                        if t >= joins.len() { continue; }
                        joins[t] = jv;
                        if t < handles_in.len() { handles_in[t] = hin; }
                        if t < handles_out.len() { handles_out[t] = hout; }
                    }
                }
            });
        });
        self.toast(&format!("Spine corner: {} ({} pts)", label, targets.len()));
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Quick-tap B: cycle join type on selection / hover / all points.
    pub(crate) fn spine_cycle_join_type(&mut self) {
        let Some(idx) = self.spine_edit_prim_index() else { return };
        let joins = self.spine_joins(idx);
        let n = joins.len();
        if n == 0 { return; }
        let sample = self.spine_pt_sel.iter().copied().find(|&i| i < n)
            .or(self.spine_pt_hover.filter(|&i| i < n))
            .unwrap_or(0);
        let cur = joins.get(sample).copied().unwrap_or(0.0);
        let next_mode = spine_next_corner_mode(cur, true);
        let mode = match next_mode { 1 => 1usize, -1 => 2, _ => 0 };
        self.spine_apply_join_type(mode);
    }

    /// Ensure we have a point selection for S/R (hover or sole point auto-picks).
    fn spine_ensure_sel_for_transform(&mut self, idx: usize) -> bool {
        if !self.spine_pt_sel.is_empty() { return true; }
        if let Some(h) = self.spine_pt_hover {
            self.spine_pt_sel.insert(h);
            return true;
        }
        let n = self.spine_points(idx).len();
        if n == 1 {
            self.spine_pt_sel.insert(0);
            return true;
        }
        self.toast("Select spine points first");
        false
    }

    /// S: modal **per-point radius scale** (point parameter), not spatial curve scale.
    /// Edits `point_scale[i]` for the selection (effective radius = global radius × scale).
    pub(crate) fn spine_pt_scale_key(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let Some(idx) = self.spine_edit_prim_index() else { return };
        if !self.spine_ensure_sel_for_transform(idx) { return; }
        // Ensure scales exist
        self.write_prim(idx, |p| {
            if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                spine_sync_all(points, joins, point_scale, handles_in, handles_out);
            }
        });
        let pts = self.spine_points(idx);
        let scales: Vec<f32> = match &self.scene.primitives[idx].shape {
            SdfShape::SpinePipe { point_scale, .. } => {
                let mut s = point_scale.clone();
                spine_sync_scales(&pts, &mut s);
                s
            }
            _ => return,
        };
        // Pack old scale into Vec3.x so restore/update can use originals.
        let originals: Vec<(usize, Vec3)> = self.spine_pt_sel.iter()
            .filter_map(|&i| {
                if i < pts.len() {
                    Some((i, Vec3::splat(scales.get(i).copied().unwrap_or(1.0))))
                } else { None }
            })
            .collect();
        if originals.is_empty() { return; }
        let centroid = originals.iter()
            .filter_map(|(i, _)| pts.get(*i).copied())
            .sum::<Vec3>() / originals.len().max(1) as f32;
        let c_w = self.spine_local_to_world(idx, centroid);
        let Some(c_s) = self.spine_screen(c_w, rect) else {
            self.toast("Cannot scale (point off-screen)");
            return;
        };
        let dist = (mouse - c_s).length().max(40.0);
        if !self.has_active_episode() {
            self.begin_episode("Scale Spine Point Radius");
        }
        self.spine_pt_drag = Some(SpinePtDrag {
            prim_idx: idx,
            originals,
            start_mouse: mouse,
            start_hit: c_w,
            plane_o: c_w,
            plane_n: (self.viewport.camera.target
                + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance)
                - c_w).normalize_or_zero(),
            moved: false,
            press_kind: 3,
            cycle_pi: None,
            scale_centroid: centroid,
            scale_start_dist: dist,
            rotate_start_angle: 0.0,
            modal: true,
            start_radius: 0.0,
            scale_radius: true, // means "edit point_scale param"
        });
        self.toast("Point radius scale — move mouse · LMB/Enter confirm");
    }

    /// R: modal rotate/twist about view axis — follows mouse without LMB.
    pub(crate) fn spine_pt_rotate_key(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let Some(idx) = self.spine_edit_prim_index() else { return };
        if !self.spine_ensure_sel_for_transform(idx) { return; }
        let pts = self.spine_points(idx);
        let originals: Vec<(usize, Vec3)> = self.spine_pt_sel.iter()
            .filter_map(|&i| pts.get(i).map(|p| (i, *p)))
            .collect();
        if originals.is_empty() { return; }
        if originals.len() < 2 {
            self.toast("Select 2+ points to rotate / twist");
            return;
        }
        let centroid: Vec3 = originals.iter().map(|(_, p)| *p).sum::<Vec3>()
            / originals.len() as f32;
        let c_w = self.spine_local_to_world(idx, centroid);
        let Some(c_s) = self.spine_screen(c_w, rect) else {
            self.toast("Cannot rotate (points off-screen)");
            return;
        };
        let ang = (mouse.y - c_s.y).atan2(mouse.x - c_s.x);
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let axis = (cam_pos - c_w).normalize_or_zero();
        let axis = if axis.length_squared() < 0.25 { Vec3::Y } else { axis };
        if !self.has_active_episode() {
            self.begin_episode("Rotate Spine Points");
        }
        self.spine_pt_drag = Some(SpinePtDrag {
            prim_idx: idx,
            originals,
            start_mouse: mouse,
            start_hit: c_w,
            plane_o: c_w,
            plane_n: axis,
            moved: false,
            press_kind: 4,
            cycle_pi: None,
            scale_centroid: centroid,
            scale_start_dist: 1.0,
            rotate_start_angle: ang,
            modal: true,
            start_radius: 0.0,
            scale_radius: false,
        });
        self.toast("Rotate / twist — move mouse · LMB/Enter confirm");
    }

    pub(crate) fn spine_pt_start_drag(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect) {
        self.spine_pt_start_drag_ex(idx, m, rect, 0, None);
    }

    fn spine_pt_start_drag_ex(
        &mut self,
        idx: usize,
        m: egui::Pos2,
        rect: egui::Rect,
        press_kind: u8,
        cycle_pi: Option<usize>,
    ) {
        let pts = self.spine_points(idx);
        let mut originals: Vec<(usize, Vec3)> = self.spine_pt_sel.iter()
            .filter_map(|&i| pts.get(i).map(|p| (i, *p)))
            .collect();
        if originals.is_empty() { return; }
        originals.sort_by_key(|(i, _)| *i);

        // Drag plane = resolved workplane (workplane.rs), same as clip/create.
        let wp = &self.viewport.wp_resolved;
        let plane_o = wp.origin;
        let plane_n = wp.normal;
        let Some(start_hit) = self.ray_workplane_hit(m, rect) else { return };

        if !self.has_active_episode() {
            let label = if press_kind == 1 { "Add Spine Point" } else { "Move Spine Point" };
            self.begin_episode(label);
        }
        let pk = if cycle_pi.is_some() { 2 } else { press_kind };
        self.spine_pt_drag = Some(SpinePtDrag {
            prim_idx: idx,
            originals,
            start_mouse: m,
            start_hit,
            plane_o,
            plane_n,
            moved: false,
            press_kind: pk,
            cycle_pi,
            scale_centroid: Vec3::ZERO,
            scale_start_dist: 1.0,
            rotate_start_angle: 0.0,
            modal: false,
            start_radius: 0.0,
            scale_radius: false,
        });
    }

    /// Keyboard G: modal free-move (mouse follow, no LMB required until confirm).
    pub(crate) fn spine_pt_grab_key(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let Some(idx) = self.spine_edit_prim_index() else { return };
        if !self.spine_ensure_sel_for_transform(idx) { return; }
        if !self.spine_pt_try_start_drag(idx, mouse, rect, 0) { return; }
        if let Some(d) = self.spine_pt_drag.as_mut() {
            d.modal = true;
        }
        self.toast("Move — Shift angle-snap · Alt workplane · LMB/Enter confirm");
    }

    /// Snap a world-space move delta to angle increments in the **workplane** frame.
    /// Horizontal = workplane tangent_u, vertical = tangent_v, diagonals = 45° (or
    /// `snap_rotation_deg` from the snap tool). Result lies on the workplane.
    fn spine_snap_delta_workplane(&self, delta: Vec3) -> Vec3 {
        let wp = &self.viewport.wp_resolved;
        let mut u = wp.tangent_u;
        let mut v = wp.tangent_v;
        let mut n = wp.normal;
        if u.length_squared() < 0.25 || v.length_squared() < 0.25 || n.length_squared() < 0.25 {
            // Degenerate resolved plane → world XZ as workplane
            u = Vec3::X;
            v = Vec3::Z;
            n = Vec3::Y;
        } else {
            u = u.normalize();
            v = v.normalize();
            n = n.normalize();
            // Re-orthonormalize V against U (stable if frame is slightly off)
            v = (v - u * v.dot(u)).normalize_or_zero();
            if v.length_squared() < 0.25 {
                v = n.cross(u).normalize_or_zero();
            }
        }
        // Project free delta onto workplane first
        let d = delta - n * delta.dot(n);
        let len = d.length();
        if len < 1e-8 {
            return Vec3::ZERO;
        }
        let x = d.dot(u);
        let y = d.dot(v);
        let mut step_deg = self.viewport.snap_rotation_deg;
        if step_deg < 1e-3 {
            step_deg = 45.0; // H / V / 45° relative to workplane
        }
        let step = step_deg.to_radians().max(1e-4);
        let ang = (y.atan2(x) / step).round() * step;
        (u * ang.cos() + v * ang.sin()) * len
    }

    /// Quantize a workplane delta to the snap grid along workplane U/V.
    fn spine_snap_delta_grid_workplane(&self, delta: Vec3) -> Vec3 {
        let g = self.viewport.snap_grid;
        if g < 1e-6 {
            return delta;
        }
        let wp = &self.viewport.wp_resolved;
        let mut u = wp.tangent_u.normalize_or_zero();
        let mut v = wp.tangent_v.normalize_or_zero();
        let n = wp.normal.normalize_or_zero();
        if u.length_squared() < 0.25 || v.length_squared() < 0.25 {
            u = Vec3::X;
            v = Vec3::Z;
        } else {
            v = (v - u * v.dot(u)).normalize_or_zero();
            if v.length_squared() < 0.25 {
                v = n.cross(u).normalize_or_zero();
            }
        }
        let d = if n.length_squared() > 0.25 {
            delta - n * delta.dot(n)
        } else {
            delta
        };
        let x = (d.dot(u) / g).round() * g;
        let y = (d.dot(v) / g).round() * g;
        u * x + v * y
    }

    pub(crate) fn spine_pt_update_drag(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        shift: bool,
        alt: bool,
        ctrl: bool,
    ) {
        let Some(drag) = self.spine_pt_drag.clone() else { return };
        let d2 = (mouse - drag.start_mouse).length_sq();
        if d2 > SPINE_DRAG_PX2 {
            if let Some(d) = self.spine_pt_drag.as_mut() { d.moved = true; }
        }
        let idx = drag.prim_idx;
        if idx >= self.scene.primitives.len() { return; }

        if drag.press_kind == 3 {
            // Per-point radius scale parameter (not spatial).
            let c_w = self.spine_local_to_world(idx, drag.scale_centroid);
            let Some(c_s) = self.spine_screen(c_w, rect) else { return };
            let dist = (mouse - c_s).length().max(1.0);
            let s = (dist / drag.scale_start_dist.max(1.0)).clamp(0.05, 20.0);
            self.write_prim(idx, |p| {
                if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                    spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                    for &(i, orig) in &drag.originals {
                        if i < point_scale.len() {
                            point_scale[i] = (orig.x * s).clamp(0.05, 50.0);
                        }
                    }
                }
            });
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        if drag.press_kind == 4 {
            // Rotate about view axis through centroid (local-space Rodriguez via Quat).
            let c_w = self.spine_local_to_world(idx, drag.scale_centroid);
            let Some(c_s) = self.spine_screen(c_w, rect) else { return };
            let ang = (mouse.y - c_s.y).atan2(mouse.x - c_s.x);
            let delta = ang - drag.rotate_start_angle;
            // View axis in world → approx local: use prim inverse rot on plane_n (camera axis).
            let axis_w = drag.plane_n.normalize_or_zero();
            let prim = &self.scene.primitives[idx];
            let axis_l = (prim.rotation_quat().inverse() * axis_w).normalize_or_zero();
            let axis_l = if axis_l.length_squared() < 0.25 { Vec3::Y } else { axis_l };
            let q = glam::Quat::from_axis_angle(axis_l, delta);
            let mut new_pts = self.spine_points(idx);
            let c = drag.scale_centroid;
            for &(i, orig_local) in &drag.originals {
                if i >= new_pts.len() { continue; }
                new_pts[i] = c + q * (orig_local - c);
            }
            self.write_prim(idx, |p| {
                if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                    *points = new_pts;
                    spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                }
            });
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        if drag.press_kind == 5 {
            // Yellow Bezier handle: plane through control point, WP normal.
            // **Ctrl** = both tips **rotate together** (same turn around the control point —
            //   NOT opposite / mirror). Free without Ctrl = only the grabbed tip moves.
            let Some(pi) = drag.cycle_pi else { return };
            let is_out = drag.scale_radius;
            let anchor_local = drag.scale_centroid;
            let (plane_o, plane_n) = self.spine_plane_through_point(idx, pi);
            let Some(mut tip_w) = self.ray_plane_hit(mouse, rect, plane_o, plane_n) else {
                return;
            };
            if shift {
                let anchor_w = self.spine_local_to_world(idx, anchor_local);
                let n = plane_n.normalize_or_zero();
                let anchor_on = anchor_w - n * (anchor_w - plane_o).dot(n);
                let mut delta = tip_w - anchor_on;
                delta = delta - n * delta.dot(n);
                delta = self.spine_snap_delta_workplane(delta);
                if self.viewport.snap_enabled && self.viewport.snap_grid > 1e-6 {
                    delta = self.spine_snap_delta_grid_workplane(delta);
                }
                tip_w = anchor_on + delta;
            }
            tip_w = tip_w - plane_n * (tip_w - plane_o).dot(plane_n);
            let new_h = self.spine_world_to_local(idx, tip_w) - anchor_local;

            // originals[0] = dragged handle at press; originals[1] = sibling at press.
            let orig_drag = drag.originals.first().map(|(_, h)| *h).unwrap_or(Vec3::ZERO);
            let orig_sib = drag.originals.get(1).map(|(_, h)| *h).unwrap_or(Vec3::ZERO);
            let together = ctrl && !alt; // Ctrl = rotate both the same way
            let sib_h = if together {
                let a = orig_drag.normalize_or_zero();
                let b = new_h.normalize_or_zero();
                if a.length_squared() < 0.25 || b.length_squared() < 0.25 {
                    // No stable turn — keep sibling as at press (still "together" as frozen pair).
                    orig_sib
                } else {
                    // Same rotation that takes orig_drag → new_h direction, applied to sibling.
                    // Both tips turn together (NOT -b * length which is opposite).
                    let q = glam::Quat::from_rotation_arc(a, b);
                    let mut s = q * orig_sib;
                    // Stay on the control-point plane.
                    let sib_w = self.spine_local_to_world(idx, anchor_local + s);
                    let sib_on = self.spine_project_onto_point_plane(idx, pi, sib_w);
                    s = self.spine_world_to_local(idx, sib_on) - anchor_local;
                    // Keep original sibling length.
                    let sl = orig_sib.length();
                    if sl > 1e-8 && s.length_squared() > 1e-12 {
                        s = s.normalize() * sl;
                    }
                    s
                }
            } else {
                Vec3::ZERO // unused when free
            };
            self.write_prim(idx, |p| {
                if let SdfShape::SpinePipe {
                    points, joins, point_scale, handles_in, handles_out, ..
                } = &mut p.shape
                {
                    spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                    if pi >= points.len() { return; }
                    if pi < joins.len() && joins[pi] <= 1e-6 {
                        joins[pi] = joins[pi].abs().max(0.05);
                    }
                    if is_out {
                        if pi < handles_out.len() {
                            handles_out[pi] = new_h;
                        }
                        if together && pi < handles_in.len() {
                            handles_in[pi] = sib_h;
                        }
                    } else {
                        if pi < handles_in.len() {
                            handles_in[pi] = new_h;
                        }
                        if together && pi < handles_out.len() {
                            handles_out[pi] = sib_h;
                        }
                    }
                }
            });
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        // Create follow-drag: stay on plane through the **anchor** at drag start
        // (last selected before insert). Frozen plane_o/plane_n from try_start_drag.
        if drag.press_kind == 1 && drag.originals.len() == 1 {
            let Some(hit) = self.ray_plane_hit(mouse, rect, drag.plane_o, drag.plane_n) else {
                return;
            };
            let local = self.spine_world_to_local(idx, hit);
            let pi = drag.originals[0].0;
            let mut new_pts = self.spine_points(idx);
            if pi < new_pts.len() {
                new_pts[pi] = local;
            }
            self.write_prim(idx, |p| {
                if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                    *points = new_pts;
                    spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                }
            });
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        // Ctrl held while moving a point → snap primary to visible SDF/mesh surface.
        if ctrl && !drag.originals.is_empty() {
            let (origin, dir) = self.cam_ray(mouse, rect);
            if let Some((hit, _n)) = self.raycast_scene_surface(origin, dir) {
                let (_pi0, orig0) = drag.originals[0];
                let ow0 = self.spine_local_to_world(idx, orig0);
                let delta_w = hit - ow0;
                let mut new_pts = self.spine_points(idx);
                for &(i, orig_local) in &drag.originals {
                    if i >= new_pts.len() { continue; }
                    let ow = self.spine_local_to_world(idx, orig_local);
                    new_pts[i] = self.spine_world_to_local(idx, ow + delta_w);
                }
                self.write_prim(idx, |p| {
                    if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                        *points = new_pts;
                        spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                    }
                });
                self.mark_dirty(DirtyFlags::RENDER);
                return;
            }
        }

        // Default move: delta on resolved workplane via ray_plane_hit (same as workplane.rs).
        // Alt → free camera-facing plane.
        let wp = &self.viewport.wp_resolved;
        let use_wp = !alt;
        let (plane_o, plane_n) = if use_wp {
            (wp.origin, wp.normal)
        } else {
            let cam_pos = self.viewport.camera.target
                + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let mut n = (cam_pos - drag.start_hit).normalize_or_zero();
            if n.length_squared() < 0.25 {
                n = drag.plane_n;
            }
            (drag.start_hit, n)
        };
        let Some(hit) = self.ray_plane_hit(mouse, rect, plane_o, plane_n) else { return };
        let start_on = if use_wp {
            // Project drag-start onto live workplane (origin can match Auto snap).
            let n = plane_n.normalize_or_zero();
            drag.start_hit - n * (drag.start_hit - plane_o).dot(n)
        } else {
            drag.start_hit
        };
        let mut delta_w = hit - start_on;
        if use_wp {
            let n = plane_n.normalize_or_zero();
            delta_w = delta_w - n * delta_w.dot(n);
        }
        if shift {
            delta_w = self.spine_snap_delta_workplane(delta_w);
        }
        if use_wp && self.viewport.snap_enabled && self.viewport.snap_grid > 1e-6 {
            delta_w = self.spine_snap_delta_grid_workplane(delta_w);
        }
        let mut new_pts = self.spine_points(idx);
        for &(i, orig_local) in &drag.originals {
            if i >= new_pts.len() { continue; }
            let ow = self.spine_local_to_world(idx, orig_local);
            new_pts[i] = self.spine_world_to_local(idx, ow + delta_w);
        }
        self.write_prim(idx, |p| {
            if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                *points = new_pts;
                spine_sync_all(points, joins, point_scale, handles_in, handles_out);
            }
        });
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn spine_pt_confirm_drag(&mut self) {
        let Some(drag) = self.spine_pt_drag.take() else { return };
        if !drag.moved {
            let idx = drag.prim_idx;
            if drag.press_kind == 1 {
                // Unmoved add → remove the new point (no empty undo).
                if idx < self.scene.primitives.len() {
                    if let Some(&(pi, _)) = drag.originals.first() {
                        self.write_prim(idx, |p| {
                            if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                                spine_remove_point_at(points, joins, point_scale, handles_in, handles_out, pi);
                            }
                        });
                    }
                }
                self.cancel_episode();
            } else if drag.press_kind == 2 {
                // Re-click sole selected point → cycle corner (polyline parity).
                if let Some(pi) = drag.cycle_pi {
                    // Restore is no-op (never moved). Drop empty move episode then cycle.
                    self.cancel_episode();
                    if idx < self.scene.primitives.len() {
                        self.spine_pt_cycle_corner(idx, pi, true);
                    }
                } else {
                    self.cancel_episode();
                }
            } else {
                // Unmoved modal / click — restore points + radius.
                if idx < self.scene.primitives.len() {
                    self.spine_restore_drag(&drag);
                }
                self.cancel_episode();
            }
        } else {
            self.rebake_no_undo();
            self.commit_episode();
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    fn spine_restore_drag(&mut self, drag: &SpinePtDrag) {
        let idx = drag.prim_idx;
        if idx >= self.scene.primitives.len() { return; }
        self.write_prim(idx, |p| {
            if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                if drag.press_kind == 5 {
                    // Bezier handle: originals[0]=dragged offset, originals[1]=sibling.
                    let Some(pi) = drag.cycle_pi else { return };
                    let is_out = drag.scale_radius;
                    let h0 = drag.originals.first().map(|(_, h)| *h).unwrap_or(Vec3::ZERO);
                    let h1 = drag.originals.get(1).map(|(_, h)| *h).unwrap_or(Vec3::ZERO);
                    if is_out {
                        if pi < handles_out.len() { handles_out[pi] = h0; }
                        if pi < handles_in.len() { handles_in[pi] = h1; }
                    } else {
                        if pi < handles_in.len() { handles_in[pi] = h0; }
                        if pi < handles_out.len() { handles_out[pi] = h1; }
                    }
                } else if drag.press_kind == 3 {
                    // originals: (i, Vec3::splat(old_scale)) for point_scale param
                    for &(i, orig) in &drag.originals {
                        if i < point_scale.len() {
                            point_scale[i] = orig.x.max(0.001);
                        }
                    }
                } else {
                    for &(i, orig) in &drag.originals {
                        if i < points.len() { points[i] = orig; }
                    }
                }
            }
        });
    }

    pub(crate) fn spine_pt_cancel_drag(&mut self) {
        let Some(drag) = self.spine_pt_drag.take() else { return };
        let idx = drag.prim_idx;
        if idx < self.scene.primitives.len() {
            if drag.press_kind == 1 {
                // Remove points created this press.
                self.write_prim(idx, |p| {
                    if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut p.shape {
                        for &(i, _) in drag.originals.iter().rev() {
                            spine_remove_point_at(points, joins, point_scale, handles_in, handles_out, i);
                        }
                    }
                });
            } else {
                self.spine_restore_drag(&drag);
            }
        }
        self.cancel_episode();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn spine_pt_delete(&mut self) {
        let Some(idx) = self.spine_edit_prim_index() else { return };
        if self.spine_pt_sel.is_empty() { return; }
        let n = self.spine_points(idx).len();
        let remove: Vec<usize> = self.spine_pt_sel.iter().copied().collect();
        // Keep at least 1 seed point (authoring start).
        if n.saturating_sub(remove.len()) < 1 {
            self.toast("Spine needs at least 1 point");
            return;
        }
        self.transaction("Delete Spine Point", |s| {
            if let SdfShape::SpinePipe { points, joins, point_scale, handles_in, handles_out, .. } = &mut s.scene.primitives[idx].shape {
                let mut keep = vec![true; points.len()];
                for &i in &remove {
                    if i < keep.len() { keep[i] = false; }
                }
                spine_sync_all(points, joins, point_scale, handles_in, handles_out);
                let mut new_pts = Vec::new();
                let mut new_joins = Vec::new();
                let mut new_scales = Vec::new();
                let mut new_hin = Vec::new();
                let mut new_hout = Vec::new();
                for (i, p) in points.iter().enumerate() {
                    if keep[i] {
                        new_pts.push(*p);
                        new_joins.push(joins.get(i).copied().unwrap_or(0.0));
                        new_scales.push(point_scale.get(i).copied().unwrap_or(1.0));
                        new_hin.push(handles_in.get(i).copied().unwrap_or(Vec3::ZERO));
                        new_hout.push(handles_out.get(i).copied().unwrap_or(Vec3::ZERO));
                    }
                }
                *points = new_pts;
                *joins = new_joins;
                *point_scale = new_scales;
                *handles_in = new_hin;
                *handles_out = new_hout;
            }
            s.mirror_prim_to_graph(idx);
            s.rebake_no_undo();
        });
        self.spine_pt_sel.clear();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Box-select spine points inside the screen rect (additive when `shift`).
    pub(crate) fn spine_pt_box_select(&mut self, idx: usize, a: egui::Pos2, b: egui::Pos2, rect: egui::Rect, shift: bool) {
        let sr = egui::Rect::from_two_pos(a, b);
        if !shift { self.spine_pt_sel.clear(); }
        let pts = self.spine_points(idx);
        for (i, local) in pts.iter().enumerate() {
            let w = self.spine_local_to_world(idx, *local);
            if let Some(sp) = self.spine_screen(w, rect) {
                if sr.contains(sp) { self.spine_pt_sel.insert(i); }
            }
        }
    }

    // ---- overlay ----

    pub(crate) fn draw_spine_point_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(idx) = self.spine_edit_prim_index() else { return };
        let pts = self.spine_points(idx);
        if pts.is_empty() { return; }
        let joins = self.spine_joins(idx);
        let closed = matches!(
            self.scene.primitives[idx].shape,
            SdfShape::SpinePipe { closed: true, .. }
        );
        let col_edge = egui::Color32::from_rgb(80, 200, 255);
        let col_sharp = egui::Color32::from_rgb(100, 220, 255);
        let col_bezier = egui::Color32::from_rgb(120, 220, 120);
        let col_chamfer = egui::Color32::from_rgb(240, 180, 80);
        let col_handle = egui::Color32::from_rgb(255, 180, 60);
        let col_handle_line = egui::Color32::from_rgb(200, 160, 80);
        let col_sel = egui::Color32::from_rgb(255, 255, 255);
        let col_hov = egui::Color32::from_rgb(255, 220, 80);
        let handles = self.spine_resolved_handles(idx);

        // Segments — for Bezier joins, draw a cheap tessellated preview of the curve.
        if pts.len() >= 2 {
            let segs = if closed { pts.len() } else { pts.len() - 1 };
            for i in 0..segs {
                let j = (i + 1) % pts.len();
                let wa = self.spine_local_to_world(idx, pts[i]);
                let wb = self.spine_local_to_world(idx, pts[j]);
                let (hin_j, _) = handles.get(j).copied().unwrap_or((Vec3::ZERO, Vec3::ZERO));
                let (_, hout_i) = handles.get(i).copied().unwrap_or((Vec3::ZERO, Vec3::ZERO));
                if hout_i.length_squared() > 1e-12 || hin_j.length_squared() > 1e-12 {
                    // Cubic preview (8 segments).
                    let p0 = pts[i];
                    let p3 = pts[j];
                    let p1 = p0 + hout_i;
                    let p2 = p3 + hin_j;
                    let mut prev_s = self.spine_screen(self.spine_local_to_world(idx, p0), rect);
                    for k in 1..=8 {
                        let t = k as f32 / 8.0;
                        let pt = core_math::curves::cubic_bezier_3d(p0, p1, p2, p3, t);
                        let ws = self.spine_local_to_world(idx, pt);
                        let cur_s = self.spine_screen(ws, rect);
                        if let (Some(a), Some(b)) = (prev_s, cur_s) {
                            painter.line_segment(
                                [a, b],
                                egui::Stroke::new(2.0, col_bezier),
                            );
                        }
                        prev_s = cur_s;
                    }
                } else if let (Some(sa), Some(sb)) =
                    (self.spine_screen(wa, rect), self.spine_screen(wb, rect))
                {
                    painter.line_segment([sa, sb], egui::Stroke::new(1.5, col_edge));
                }
            }
        }

        // Points — color encodes join type (drawn under handles so yellow knobs stay on top).
        for (i, local) in pts.iter().enumerate() {
            let w = self.spine_local_to_world(idx, *local);
            let Some(sp) = self.spine_screen(w, rect) else { continue };
            let selected = self.spine_pt_sel.contains(&i);
            let hovered = self.spine_pt_hover == Some(i);
            let j = joins.get(i).copied().unwrap_or(0.0);
            let type_col = if j.abs() <= 1e-6 {
                col_sharp
            } else if j > 0.0 {
                col_bezier
            } else {
                col_chamfer
            };
            let r = if selected { 6.0 } else if hovered { 5.5 } else { 4.5 };
            let fill = if selected { col_sel } else if hovered { col_hov } else { type_col };
            painter.circle_filled(sp, r, fill);
            if selected {
                painter.circle_stroke(sp, r + 2.0, egui::Stroke::new(1.5, type_col));
            }
        }

        // Bezier handles (yellow) — always on workplane when dragged; draw last for visibility.
        for (i, local) in pts.iter().enumerate() {
            let jv = joins.get(i).copied().unwrap_or(0.0);
            if jv <= 1e-6 { continue; }
            let show = self.spine_pt_sel.contains(&i)
                || self.spine_pt_sel.is_empty()
                || self.spine_pt_hover == Some(i);
            if !show { continue; }
            let (hin, hout) = handles.get(i).copied().unwrap_or((Vec3::ZERO, Vec3::ZERO));
            let anchor_s = self.spine_screen(self.spine_local_to_world(idx, *local), rect);
            for hoff in [hin, hout] {
                if hoff.length_squared() < 1e-12 { continue; }
                let hw = self.spine_local_to_world(idx, *local + hoff);
                let Some(hs) = self.spine_screen(hw, rect) else { continue };
                if let Some(as_) = anchor_s {
                    painter.line_segment([as_, hs], egui::Stroke::new(1.5, col_handle_line));
                }
                painter.circle_filled(hs, 5.5, col_handle);
                painter.circle_stroke(hs, 5.5, egui::Stroke::new(1.5, col_sel));
            }
        }
        // Hint when only the seed exists
        if pts.len() < 2 {
            if let Some(sp) = self.spine_screen(self.spine_local_to_world(idx, pts[0]), rect) {
                painter.text(
                    sp + egui::vec2(10.0, -12.0),
                    egui::Align2::LEFT_CENTER,
                    "Ctrl+click add · Shift multi-select · RMB delete · S/R · B · Space",
                    egui::FontId::proportional(11.0),
                    egui::Color32::from_rgb(180, 220, 255),
                );
            }
        }
    }
}

#[cfg(test)]
mod spine_parallel_array_tests {
    use super::*;

    /// The "new point inherits the old point's radius scale" bug: insert/remove must keep
    /// every per-point attribute attached to ITS point, at any slot, including legacy prims
    /// whose parallel arrays are shorter than `points`.
    #[test]
    fn insert_and_remove_keep_attributes_attached() {
        let mut points = vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0];
        let mut joins = vec![0.0, 1.0, -1.0];
        let mut scales = vec![1.0, 3.0, 0.5];
        let mut hin = vec![Vec3::ZERO, Vec3::Y, Vec3::Z];
        let mut hout = vec![Vec3::ZERO, -Vec3::Y, -Vec3::Z];

        // Insert mid-array: new point gets DEFAULTS; neighbors keep their own attributes.
        let at = spine_insert_point_at(&mut points, &mut joins, &mut scales, &mut hin, &mut hout, 1, Vec3::X * 0.5);
        assert_eq!(at, 1);
        assert_eq!(scales, vec![1.0, 1.0, 3.0, 0.5], "scale must not shift onto the new point");
        assert_eq!(joins, vec![0.0, 0.0, 1.0, -1.0]);
        assert_eq!(hin[2], Vec3::Y, "handles follow their point");

        // Remove that point: original attribution restored exactly.
        spine_remove_point_at(&mut points, &mut joins, &mut scales, &mut hin, &mut hout, 1);
        assert_eq!(scales, vec![1.0, 3.0, 0.5]);
        assert_eq!(joins, vec![0.0, 1.0, -1.0]);

        // Legacy prim (short parallels): sync-first prevents shifted attribution.
        let mut short_scales = vec![2.0];
        let mut short_joins = vec![];
        let mut short_hin = vec![];
        let mut short_hout = vec![];
        spine_insert_point_at(&mut points, &mut short_joins, &mut short_scales, &mut short_hin, &mut short_hout, 1, Vec3::NEG_X);
        assert_eq!(short_scales, vec![2.0, 1.0, 1.0, 1.0], "legacy pad THEN insert defaults");
    }
}
