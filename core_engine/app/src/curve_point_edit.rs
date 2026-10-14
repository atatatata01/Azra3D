//! Interactive PROFILE-POINT editor for the Polyline shape (`SdfShape::CurveExtrude`).
//!
//! Activation rule (no separate mode toggle): the point handles show + are editable whenever
//! EXACTLY ONE `CurveExtrude` SDF prim is selected in any non-sculpt SDF work mode (Object OR the
//! per-prim edit mode). It is NOT Object-only: for SDF, individual prims are selectable only in the
//! edit mode (`WorkMode::Vertex`), and creating a Polyline auto-enters it — an Object-only gate
//! could never coincide with a single selected prim, so the handles never appeared. Resolved by
//! [`AzraApp::curve_edit_prim_index`] (set `AZRA_CURVE_DBG=1` to trace its decision).
//!
//! This mirrors the poly vertex-edit system (`poly_selection.rs` + `poly_drag_tools.rs`) but for the
//! 2D profile of a CurveExtrude prim: selection is a set of profile-vertex indices
//! (`self.curve_pt_sel`, NOT `UnifiedSelection` — those are scene objects), G/R/S run a modal
//! transform in the profile plane, and every mutation is undoable via `transaction()`/episode.
//!
//! Profile geometry: a vertex `pos: Vec2(a, b)` lives at prim-local `(x=a, y=extrude, z=b)`. The
//! extrusion axis is prim-local Y; the profile lives in the prim-local XZ plane. World placement =
//! prim transform (pos/rot/scale) then body transform.

use eframe::egui;
use glam::{Vec2, Vec3};
use core_math::primitives::{SdfShape, CurveVertex};
use core_math::scene_graph::BodyTransform;
use app_types::{TransformKind, AxisConstraint, ActiveTool, CurvePointDrag, CurveBevelDrag};
use crate::*;

/// Screen-space pick radius for a profile handle (px).
const CURVE_PT_PICK_PX: f32 = 10.0;

/// Result of [`AzraApp::curve_press_dispatch`] — how the caller must treat the LMB press.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CurvePressOutcome {
    /// The dispatcher started/handled a point grab (or shift-toggle). Caller does nothing more.
    Grab,
    /// Curve editor active but the press hit no handle. Caller may start a point-editor box-select.
    Empty,
    /// No curve editor / no prim, OR armed-gizmo over empty space — caller runs its normal press
    /// handling (whole-prim gizmo drag / object selection).
    No,
}

/// Pure activation predicate for the profile-point editor (extracted so the gate is unit-testable
/// without an `AzraApp`). The editor is active iff the modeling context is SDF, the work mode is
/// not the voxel Sculpt room, exactly one prim is selected, and that prim is a `CurveExtrude`.
#[inline]
pub(crate) fn curve_point_editor_active(is_sdf: bool, is_sculpt_mode: bool, sel_count: usize, sole_is_curve: bool) -> bool {
    is_sdf && !is_sculpt_mode && sel_count == 1 && sole_is_curve
}

/// Pixel radius within which an LMB press is treated as landing ON a profile edge (segment drag).
const CURVE_EDGE_PICK_PX: f32 = 8.0;
/// Pixel radius for hitting the persistent bevel DIAMOND handle (matches the shared
/// `draw_diamond_handle` hover convention of <16px).
const CURVE_DIAMOND_HIT_PX: f32 = 16.0;
/// Squared-pixel move threshold that promotes a direct press into a real drag (below = click).
const CURVE_DRAG_PX2: f32 = 16.0;

/// Snap-anchor capture for the Ctrl / Ctrl+Shift move constraints, from the drag-start selection.
/// Returns `(slide_dirs, corner_prev)`:
///   * single point → its two incident-edge directions (for Ctrl+Shift edge-slide) + the previous
///     vertex UV (for Ctrl "corner angle to 45°").
///   * a contiguous 2-point segment → the segment's own axis (edge-slide along itself).
///   * anything else → no anchors.
fn curve_snap_anchors(verts: &[CurveVertex], original: &[(usize, Vec2)]) -> (Vec<Vec2>, Option<Vec2>) {
    let n = verts.len();
    if n < 2 { return (Vec::new(), None); }
    if original.len() == 1 {
        let i = original[0].0;
        let prev = verts[(i + n - 1) % n].pos;
        let cur = verts[i].pos;
        let next = verts[(i + 1) % n].pos;
        let mut dirs = Vec::new();
        let d0 = (cur - prev).normalize_or_zero();
        let d1 = (next - cur).normalize_or_zero();
        if d0 != Vec2::ZERO { dirs.push(d0); }
        if d1 != Vec2::ZERO { dirs.push(d1); }
        return (dirs, Some(prev));
    }
    if original.len() == 2 {
        let (a, b) = (original[0].0, original[1].0);
        let adjacent = (a + 1) % n == b || (b + 1) % n == a;
        if adjacent {
            let d = (verts[b].pos - verts[a].pos).normalize_or_zero();
            if d != Vec2::ZERO { return (vec![d], None); }
        }
    }
    (Vec::new(), None)
}

/// Snap a translation delta to the nearest H / V / 45° direction, preserving the moved distance
/// along that axis (projection onto the snapped unit vector).
fn curve_snap_hv45(delta: Vec2) -> Vec2 {
    let len = delta.length();
    if len < 1e-6 { return delta; }
    let step = std::f32::consts::FRAC_PI_4;
    let snapped = (delta.y.atan2(delta.x) / step).round() * step;
    let dir = Vec2::new(snapped.cos(), snapped.sin());
    dir * delta.dot(dir)
}

/// Snap a moved point so the edge from `prev` to it lands on a multiple of 45°, keeping its distance.
fn curve_snap_point_angle45(prev: Vec2, pos: Vec2) -> Vec2 {
    let v = pos - prev;
    let len = v.length();
    if len < 1e-6 { return pos; }
    let step = std::f32::consts::FRAC_PI_4;
    let snapped = (v.y.atan2(v.x) / step).round() * step;
    prev + Vec2::new(snapped.cos(), snapped.sin()) * len
}

/// Advance a corner mode encoded in the sign of `bevel_radius` by one step. Returns the NEXT mode as
/// `0` (sharp) / `1` (round) / `-1` (chamfer). Cycle order Sharp→Round→Chamfer→Sharp; `forward=false`
/// reverses it.
fn curve_next_corner_mode(cur: f32, forward: bool) -> i8 {
    let mode: i8 = if cur.abs() <= 1e-6 { 0 } else if cur > 0.0 { 1 } else { -1 };
    if forward {
        match mode { 0 => 1, 1 => -1, _ => 0 }
    } else {
        match mode { 0 => -1, -1 => 1, _ => 0 }
    }
}

/// Flatten a profile edge to horizontal OR vertical (whichever it's closest to), pivoting on the
/// segment midpoint so BOTH endpoints move symmetrically. Returns the new `(a, b)` positions.
/// Horizontal when the edge is more horizontal than vertical (|dx| >= |dy|): equalize y to the
/// midpoint y (keep each x); else vertical: equalize x to the midpoint x. Midpoint is invariant.
fn curve_flatten_edge(a: Vec2, b: Vec2) -> (Vec2, Vec2) {
    let m = (a + b) * 0.5;
    let d = b - a;
    if d.x.abs() >= d.y.abs() {
        (Vec2::new(a.x, m.y), Vec2::new(b.x, m.y))
    } else {
        (Vec2::new(m.x, a.y), Vec2::new(m.x, b.y))
    }
}

/// Set the selected edge `(cur_a → cur_b)` to a reference segment's slope, keeping the selected
/// edge's own LENGTH and MIDPOINT, and PRESERVING endpoint order — the reference direction is
/// flipped 180° when it points opposite the current edge (`dot < 0`) so the two points never swap
/// sides. Returns the new `(a, b)`. Pure (order-preservation math is unit-tested).
fn curve_copy_angle(cur_a: Vec2, cur_b: Vec2, ref_dir: Vec2) -> (Vec2, Vec2) {
    let cur_dir = cur_b - cur_a;
    let len = cur_dir.length();
    let m = (cur_a + cur_b) * 0.5;
    let mut rd = ref_dir.normalize_or_zero();
    if rd == Vec2::ZERO || len < 1e-6 { return (cur_a, cur_b); }
    if rd.dot(cur_dir.normalize_or_zero()) < 0.0 { rd = -rd; }
    let half = rd * (len * 0.5);
    (m - half, m + half)
}

/// Constrain a translation delta to whichever candidate direction it most follows (edge-slide).
fn curve_snap_slide(delta: Vec2, dirs: &[Vec2]) -> Vec2 {
    let mut best: Option<(f32, Vec2)> = None;
    for &d in dirs {
        let proj = delta.dot(d);
        if best.map_or(true, |(bp, _)| proj.abs() > bp.abs()) { best = Some((proj, d)); }
    }
    match best { Some((proj, d)) => d * proj, None => delta }
}

impl AzraApp {
    // ============================================================== activation + frame math

    /// Flat-cache index of the single selected `CurveExtrude` prim, if the profile point editor is
    /// active. This is the ONE activation gate — see module docs.
    ///
    /// Activation = a single SDF prim of shape `CurveExtrude` is selected, in ANY non-sculpt SDF
    /// work mode. It is deliberately NOT restricted to `Object` mode: for SDF, individual prims are
    /// selectable only in the per-prim EDIT mode (`WorkMode::Vertex` — see `switch_work_mode`, which
    /// clears prim selection and selects the whole body Node when entering `Object`), and creating a
    /// Polyline auto-enters that edit mode (`add_primitive_immediate`). An Object-only gate could
    /// therefore never fire together with a single selected prim, so the handles never showed. This
    /// mirrors the *effective* reachability of the box/cylinder bevel-handle gate
    /// (`viewport_overlays.rs` ~487: Object + single selected prim), extended to also cover the SDF
    /// edit mode where prim selection actually lives.
    pub(crate) fn curve_edit_prim_index(&self) -> Option<usize> {
        let dbg = std::env::var_os("AZRA_CURVE_DBG").is_some();
        let none = |why: &str| -> Option<usize> {
            if dbg {
                eprintln!("[curve_dbg] gate=None ({}) work_mode={:?} ctx={:?} sel={}",
                    why, self.tool.work_mode, self.modeling_context, self.selected_prim_indices_unified().len());
            }
            None
        };
        // SDF only (CurveExtrude prims live exclusively in SDF bodies), and never in the voxel
        // Sculpt room — but Object AND the SDF edit modes (Vertex/Edge/Face) all qualify.
        let is_sdf = self.modeling_context == ModelingContext::Sdf;
        let is_sculpt = self.tool.work_mode == WorkMode::Sculpt;
        let sel = self.selected_prim_indices_unified();
        let idx = sel.first().copied();
        let sole_is_curve = idx.map_or(false, |i| i < self.scene.primitives.len()
            && matches!(self.scene.primitives[i].shape, SdfShape::CurveExtrude { .. }));
        if !curve_point_editor_active(is_sdf, is_sculpt, sel.len(), sole_is_curve) {
            return none(if !is_sdf { "not-sdf" } else if is_sculpt { "sculpt" }
                else if sel.len() != 1 { "sel!=1" } else { "shape!=CurveExtrude/stale" });
        }
        let idx = idx.unwrap();
        if dbg {
            eprintln!("[curve_dbg] gate=Some(idx={}) work_mode={:?}", idx, self.tool.work_mode);
        }
        Some(idx)
    }

    /// World-space frame of the prim's profile plane:
    /// `(origin, u_world, v_world, normal)` where a UV point `(a,b)` maps to
    /// `origin + u_world*a + v_world*b`. `u_world`/`v_world` already include prim + body scale.
    fn curve_prim_world_frame(&self, idx: usize) -> (Vec3, Vec3, Vec3, Vec3) {
        let prim = &self.scene.primitives[idx];
        let rq = prim.rotation_quat();
        let s = prim.scale;
        let p = prim.position;
        let bt: BodyTransform = self.scene.prim_body_map.get(idx)
            .and_then(|(nid, _)| self.scene.scene_graph.body_world_transform(*nid))
            .unwrap_or_default();
        let wl = |local: Vec3| bt.local_to_world(rq * (local * s) + p);
        let o = wl(Vec3::ZERO);
        let u = wl(Vec3::X) - o;
        let v = wl(Vec3::Z) - o;
        let n = (wl(Vec3::Y) - o).normalize_or_zero();
        (o, u, v, n)
    }

    #[inline]
    fn curve_world_to_uv(o: Vec3, u: Vec3, v: Vec3, w: Vec3) -> Vec2 {
        let d = w - o;
        Vec2::new(d.dot(u) / u.length_squared().max(1e-9), d.dot(v) / v.length_squared().max(1e-9))
    }

    /// (cam_pos, orbit, aspect, ortho_hw) — matches `draw_clip_curve_overlay`.
    fn curve_cam(&self, rect: egui::Rect) -> (Vec3, glam::Quat, f32, f32) {
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance * (self.render.fov_deg * 0.5).to_radians().tan()
        } else { 0.0 };
        (cam_pos, self.viewport.camera.orbit, aspect, ortho_hw)
    }

    /// Read the profile vertices (UV pos + bevel_radius) of the CurveExtrude prim at `idx`.
    fn curve_profile_verts(&self, idx: usize) -> Vec<CurveVertex> {
        if let SdfShape::CurveExtrude { profile, .. } = &self.scene.primitives[idx].shape {
            profile.verts.clone()
        } else { Vec::new() }
    }

    // ============================================================== hover / pick

    /// Per-frame housekeeping: clear the point selection when the owning prim changes / the editor
    /// deactivates, and refresh the hovered handle.
    pub(crate) fn curve_pt_tick(&mut self, mouse: Option<egui::Pos2>, rect: egui::Rect) {
        let idx = match self.curve_edit_prim_index() {
            Some(i) => i,
            None => {
                if !self.curve_pt_sel.is_empty() { self.curve_pt_sel.clear(); }
                self.curve_pt_hover = None;
                self.curve_pt_owner = None;
                return;
            }
        };
        // Owner sync: a different prim → drop stale selection.
        let uid = self.scene.primitives[idx].prim_uid;
        if self.curve_pt_owner != Some(uid) {
            self.curve_pt_sel.clear();
            self.curve_pt_owner = Some(uid);
        }
        // Drop indices that no longer exist (e.g. after delete/undo).
        let n = self.curve_profile_verts(idx).len();
        self.curve_pt_sel.retain(|&i| i < n);
        // Hover.
        self.curve_pt_hover = mouse.and_then(|m| self.curve_pt_pick(idx, m, rect));
    }

    /// Nearest profile handle to the mouse within the pick radius.
    pub(crate) fn curve_pt_pick(&self, idx: usize, m: egui::Pos2, rect: egui::Rect) -> Option<usize> {
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        let verts = self.curve_profile_verts(idx);
        let mut best: Option<(usize, f32)> = None;
        for (i, cv) in verts.iter().enumerate() {
            let world = o + u * cv.pos.x + v * cv.pos.y;
            if let Some(sp) = world_to_screen(world, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                let d = (sp - m).length();
                if d < CURVE_PT_PICK_PX && best.map_or(true, |(_, bd)| d < bd) { best = Some((i, d)); }
            }
        }
        best.map(|(i, _)| i)
    }

    /// Screen-space bounding rect of ALL profile handles, expanded by a pick margin. Used to decide
    /// whether a handle-missing press is "near the profile region" (→ point-editor box-select) or
    /// clearly outside it (→ fall through to normal prim selection so the user can click another prim).
    /// Reuses the SAME projection as `curve_pt_pick` (curve world frame → `world_to_screen`). Returns
    /// `None` if no profile vert projects on-screen.
    pub(crate) fn curve_profile_screen_rect(&self, idx: usize, rect: egui::Rect) -> Option<egui::Rect> {
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        let verts = self.curve_profile_verts(idx);
        let mut bb: Option<egui::Rect> = None;
        for cv in verts.iter() {
            let world = o + u * cv.pos.x + v * cv.pos.y;
            if let Some(sp) = world_to_screen(world, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                bb = Some(match bb {
                    Some(r) => r.union(egui::Rect::from_min_max(sp, sp)),
                    None => egui::Rect::from_min_max(sp, sp),
                });
            }
        }
        // Margin: a handle radius + the edge pick tolerance, so a press just outside a boundary handle
        // (still a legit rubber-band deselect) stays "inside", but a click on open surface elsewhere is out.
        let margin = CURVE_PT_PICK_PX + CURVE_EDGE_PICK_PX;
        bb.map(|r| r.expand(margin))
    }

    /// Nearest profile EDGE to the mouse (for inserting a point): returns `(insert_index, uv)` where
    /// `insert_index` splits the nearest edge (i, i+1) → insert after i, at the cursor's plane UV.
    fn curve_pt_edge_insert(&self, idx: usize, m: egui::Pos2, rect: egui::Rect) -> Option<(usize, Vec2)> {
        let (o, u, v, n) = self.curve_prim_world_frame(idx);
        let world = self.ray_plane_hit(m, rect, o, n)?;
        let uv = Self::curve_world_to_uv(o, u, v, world);
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        let verts = self.curve_profile_verts(idx);
        let cnt = verts.len();
        if cnt < 2 { return Some((cnt, uv)); }
        let screen: Vec<Option<egui::Pos2>> = verts.iter().map(|cv| {
            let w = o + u * cv.pos.x + v * cv.pos.y;
            world_to_screen(w, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg)
        }).collect();
        let mut best: (f32, usize) = (f32::MAX, cnt - 1);
        for i in 0..cnt {
            let j = (i + 1) % cnt;
            if let (Some(a), Some(b)) = (screen[i], screen[j]) {
                let d = dist_point_to_segment(m, a, b);
                if d < best.0 { best = (d, i); }
            }
        }
        // Always split the nearest edge (insert after its first vertex), at the cursor UV.
        Some((best.1 + 1, uv))
    }

    // ============================================================== click / select / add

    /// Handle a profile-point click. Returns `true` if the click was consumed (hit a point, cycled a
    /// corner, added a point, or cleared a non-empty selection); `false` = let normal object
    /// selection proceed (so clicking empty space with no points selected still deselects the prim).
    pub(crate) fn curve_pt_click(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect, shift: bool) -> bool {
        let hit = self.curve_pt_pick(idx, m, rect);
        match hit {
            Some(pi) => {
                if shift {
                    if !self.curve_pt_sel.remove(&pi) { self.curve_pt_sel.insert(pi); }
                } else if self.curve_pt_sel.len() == 1 && self.curve_pt_sel.contains(&pi) {
                    // Re-click on the sole selected point: keep the selection (no-op). Corner-mode
                    // cycling moved to CLICKING the persistent bevel diamond (SDF design language:
                    // the gizmo owns bevel, a point click only selects).
                } else {
                    self.curve_pt_sel.clear();
                    self.curve_pt_sel.insert(pi);
                }
                true
            }
            None => {
                if shift {
                    // Shift+click on an edge/empty = insert a new point (atomic, undoable).
                    self.curve_pt_add_point(idx, m, rect);
                    true
                } else if !self.curve_pt_sel.is_empty() {
                    self.curve_pt_sel.clear();
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Box-select profile points inside the screen rect (additive when `shift`).
    pub(crate) fn curve_pt_box_select(&mut self, idx: usize, a: egui::Pos2, b: egui::Pos2, rect: egui::Rect, shift: bool) {
        let sr = egui::Rect::from_two_pos(a, b);
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        if !shift { self.curve_pt_sel.clear(); }
        let verts = self.curve_profile_verts(idx);
        for (i, cv) in verts.iter().enumerate() {
            let world = o + u * cv.pos.x + v * cv.pos.y;
            if let Some(sp) = world_to_screen(world, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                if sr.contains(sp) { self.curve_pt_sel.insert(i); }
            }
        }
    }

    /// Apply one corner-cycle step to `targets` (Sharp→Round→Chamfer→Sharp; the mode is packed in
    /// the SIGN of `bevel_radius`). `base_cur` is the radius the NEXT mode is derived from (the
    /// hovered/first-selected point's current value). A freshly-beveled corner gets a default radius
    /// from its shorter neighbour edge; an existing magnitude is preserved. ONE transaction + toast.
    /// Shared by scroll-over-point cycling and the diamond-handle CLICK.
    fn curve_apply_corner_cycle(&mut self, idx: usize, targets: Vec<usize>, base_cur: f32, forward: bool) {
        let next_mode = curve_next_corner_mode(base_cur, forward); // 0 sharp, 1 round, -1 chamfer
        let label = match next_mode { 1 => "Round", -1 => "Chamfer", _ => "Sharp" };
        self.transaction("Curve corner", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                    let cnt = profile.verts.len();
                    for &t in &targets {
                        if t >= cnt { continue; }
                        let a = profile.verts[(t + cnt - 1) % cnt].pos;
                        let b = profile.verts[t].pos;
                        let c = profile.verts[(t + 1) % cnt].pos;
                        let default_r = ((a - b).length().min((c - b).length()) * 0.3).max(0.02);
                        let existing = profile.verts[t].bevel_radius.abs();
                        let mag = if existing > 1e-6 { existing } else { default_r };
                        profile.verts[t].bevel_radius = match next_mode {
                            1 => mag, -1 => -mag, _ => 0.0,
                        };
                    }
                }
            });
        });
        self.toast(&format!("Corner: {}", label));
    }

    /// Insert a new profile point on the nearest edge at the cursor position (atomic + undoable),
    /// then select it.
    fn curve_pt_add_point(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect) {
        let (insert_at, uv) = match self.curve_pt_edge_insert(idx, m, rect) {
            Some(x) => x, None => { self.toast("Point off the workplane"); return; }
        };
        let uid = self.scene.primitives[idx].prim_uid;
        self.transaction("Add Curve Point", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                    let at = insert_at.min(profile.verts.len());
                    profile.verts.insert(at, CurveVertex { pos: uv, bevel_radius: 0.0 });
                }
            });
        });
        // Select only the new point.
        self.curve_pt_sel.clear();
        self.curve_pt_sel.insert(insert_at);
        self.curve_pt_owner = Some(uid);
        self.toast("Point added");
    }

    /// Delete the selected profile points. Guards a closed profile at ≥3 verts.
    pub(crate) fn curve_pt_delete(&mut self) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        if self.curve_pt_sel.is_empty() { return; }
        let to_remove: Vec<usize> = self.curve_pt_sel.iter().copied().collect();
        if self.curve_pt_remove(idx, to_remove, "Delete Curve Points") { self.toast("Points deleted"); }
    }

    /// Dissolve the selected points (remove, closing the profile across the gap — for a closed
    /// polyline this is topologically the same removal as delete, just a distinct undo label).
    pub(crate) fn curve_pt_dissolve(&mut self) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        if self.curve_pt_sel.is_empty() { return; }
        let to_remove: Vec<usize> = self.curve_pt_sel.iter().copied().collect();
        if self.curve_pt_remove(idx, to_remove, "Dissolve Curve Points") { self.toast("Points dissolved"); }
    }

    /// Shared vertex remover: dedup + high→low removal, ≥3-vert guard, one transaction. Returns true
    /// if the removal was applied (false = refused by the guard).
    fn curve_pt_remove(&mut self, idx: usize, mut to_remove: Vec<usize>, label: &'static str) -> bool {
        let cur_len = self.curve_profile_verts(idx).len();
        to_remove.retain(|&i| i < cur_len);
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        to_remove.dedup();
        if to_remove.is_empty() { return false; }
        if cur_len < to_remove.len() + 3 {
            self.toast("Polyline needs at least 3 points");
            return false;
        }
        self.transaction(label, |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                    for &r in &to_remove {
                        if r < profile.verts.len() { profile.verts.remove(r); }
                    }
                }
            });
        });
        self.curve_pt_sel.clear();
        true
    }

    /// Select every profile point (A).
    pub(crate) fn curve_pt_select_all(&mut self) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        let n = self.curve_profile_verts(idx).len();
        self.curve_pt_sel = (0..n).collect();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Subdivide each SELECTED segment (an edge whose both endpoints are selected) by inserting its
    /// midpoint. One transaction. Midpoints are computed from the pre-insert geometry; inserts run
    /// high→low so earlier indices stay valid.
    pub(crate) fn curve_pt_subdivide(&mut self) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        let verts = self.curve_profile_verts(idx);
        let n = verts.len();
        if n < 2 { return; }
        let mut inserts: Vec<(usize, Vec2)> = (0..n).filter_map(|i| {
            let j = (i + 1) % n;
            if self.curve_pt_sel.contains(&i) && self.curve_pt_sel.contains(&j) {
                Some((i + 1, (verts[i].pos + verts[j].pos) * 0.5))
            } else { None }
        }).collect();
        if inserts.is_empty() { self.toast("Select a segment (2 adjacent points)"); return; }
        inserts.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        self.transaction("Subdivide Curve", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                    for &(at, mid) in &inserts {
                        let at = at.min(profile.verts.len());
                        profile.verts.insert(at, CurveVertex { pos: mid, bevel_radius: 0.0 });
                    }
                }
            });
        });
        self.toast(&format!("Subdivided {} segment(s)", inserts.len()));
    }

    // ============================================================== direct pointer drag (press/release)

    /// Nearest profile EDGE to the mouse within the pick radius (for a segment drag / shift-add).
    /// Returns the edge's first-vertex index `i` (edge = `(i, (i+1) mod n)`).
    pub(crate) fn curve_pt_edge_pick(&self, idx: usize, m: egui::Pos2, rect: egui::Rect) -> Option<usize> {
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        let verts = self.curve_profile_verts(idx);
        let cnt = verts.len();
        if cnt < 2 { return None; }
        let screen: Vec<Option<egui::Pos2>> = verts.iter().map(|cv| {
            world_to_screen(o + u * cv.pos.x + v * cv.pos.y, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg)
        }).collect();
        let mut best: Option<(f32, usize)> = None;
        for i in 0..cnt {
            let j = (i + 1) % cnt;
            if let (Some(a), Some(b)) = (screen[i], screen[j]) {
                let d = dist_point_to_segment(m, a, b);
                if d < CURVE_EDGE_PICK_PX && best.map_or(true, |(bd, _)| d < bd) { best = Some((d, i)); }
            }
        }
        best.map(|(_, i)| i)
    }

    /// SOLE owner of an LMB PRESS while a Polyline (CurveExtrude) profile-point editor is active.
    /// This one method resolves EVERY curve-prim press state so the press is never split across two
    /// racing arms / two files (the old `is_transform()` arm + the `is_idle && !gizmo_armed` arm each
    /// saw only part of the picture, and the armed-gizmo case — G with the prim selected but no point
    /// selected — fell through BOTH into a whole-prim gizmo drag, which is exactly the "clicking a
    /// point moves the whole prim" bug).
    ///
    /// Preconditions the caller must satisfy: it is the LMB-press frame, not a copy-angle pick, and
    /// the pointer is over the viewport. Handles, atomically:
    ///   * `ActiveTool::Transform` (whole-prim/node modal, e.g. after clicking a gizmo axis) over a
    ///     point/edge → confirm the modal (its own undo entry via `confirm_modal_transform`, the same
    ///     path Enter/click-confirm uses), then begin the point/edge grab (a SEPARATE episode → a
    ///     separate undo entry). Over empty space → leave the modal alone (caller/keyboard confirm it
    ///     on release).
    ///   * armed gizmo (`gizmo_armed`, `tool.current` idle) over a point/edge → disarm the gizmo,
    ///     then begin the point/edge grab. Over empty space → return `Consumed::No` so the caller's
    ///     armed-gizmo path still moves the whole prim (G-with-no-point-selected must stay whole-prim).
    ///   * idle over a point/edge → begin the grab directly. Over empty space → the caller starts a
    ///     point-editor `SelectionBox` (box-select / click-deselect).
    ///
    /// Return value tells the caller what to do with the press it must NOT re-handle:
    ///   * `Consumed::Grab`  — a grab (or shift-toggle) started/handled; caller does nothing more.
    ///   * `Consumed::Empty` — curve editor active but the press missed all handles; caller may start
    ///                         a point-editor box-select (only meaningful when idle).
    ///   * `Consumed::No`    — no curve editor / no prim, OR armed-gizmo over empty space; caller runs
    ///                         its normal (whole-prim gizmo / object) press handling.
    pub(crate) fn curve_press_dispatch(&mut self, m: egui::Pos2, rect: egui::Rect, shift: bool)
        -> CurvePressOutcome {
        let cidx = match self.curve_edit_prim_index() { Some(i) => i, None => return CurvePressOutcome::No };

        // GIZMO-CLASS HANDLES OUTRANK THE POINT EDITOR (the file's precedence rule) — and not
        // just the transform gizmo: the blend-K diamond, falloff handles, per-edge bevel
        // handles, and uniform-bevel ring all start their drags from the caller's drag chain.
        // Consuming the press here (point pick, or worse Empty → point box-select) starves
        // those arms — the "blend K smooth broken on curve prims" bug. Yield the press.
        if self.tool.blend_k_hovered
            || self.tool.falloff_hovered.is_some()
            || self.tool.hovered_bevel_edge.is_some()
            || self.tool.uniform_bevel_hovered
        {
            return CurvePressOutcome::No;
        }

        // (0) Persistent bevel DIAMOND — a gizmo, so it outranks point/edge picks (gizmo > point
        //     edit, the file's precedence rule). `curve_idle_diamond` returns None whenever the
        //     handle isn't drawn (tool not idle / no point selected), so it can never steal a press
        //     while hidden. Starts a DIRECT bevel gesture: drag = radius fine-tune (one episode),
        //     unmoved click = corner-type cycle (resolved on release).
        if self.tool.gizmo_hovered.is_none() {
            if let Some((sp, _, _)) = self.curve_idle_diamond(rect) {
                if (m - sp).length() < CURVE_DIAMOND_HIT_PX {
                    self.set_gizmo_armed(false);
                    if self.curve_bevel_begin(m) { return CurvePressOutcome::Grab; }
                }
            }
        }

        let over_pt = self.curve_pt_pick(cidx, m, rect).is_some()
            || self.curve_pt_edge_pick(cidx, m, rect).is_some();

        // (1) Whole-prim / node modal transform is running (a gizmo axis was clicked, or a keyboard
        //     modal on the node). Only steal the press when it lands on a handle; otherwise the modal
        //     keeps ownership and the release path confirms it normally.
        if self.tool.current.is_transform() {
            if !over_pt { return CurvePressOutcome::No; }
            self.confirm_modal_transform();
            let _ = self.curve_pt_press(cidx, m, rect, shift);
            return CurvePressOutcome::Grab;
        }

        // (2) Gizmo armed on the curve prim (G/R/S pressed, prim selected, NO point selected). A press
        //     ON a handle now grabs THAT point (disarm first so the caller's armed-gizmo drag path is
        //     dead this frame); a press on empty space still means "move the whole prim" → defer.
        if self.tool.gizmo_armed && self.tool.current.is_idle() && self.tool.gizmo_hovered.is_none() {
            if !over_pt { return CurvePressOutcome::No; }
            self.set_gizmo_armed(false);
            let _ = self.curve_pt_press(cidx, m, rect, shift);
            return CurvePressOutcome::Grab;
        }

        // (3) Idle: direct grab on a handle, else decide between a point-editor box-select (press near
        //     the profile region → rubber-band deselect / box-select of points) and a fall-through to
        //     normal prim selection (press clearly OUTSIDE the profile's screen bounds → `No`, so the
        //     caller reaches prim picking and the user can select a DIFFERENT prim by clicking open
        //     surface). Without this split every idle miss returned `Empty` → a tiny box-select that
        //     could never switch prims while a polyline was active.
        if self.tool.current.is_idle() && self.tool.gizmo_hovered.is_none() && !self.tool.gizmo_armed {
            if self.curve_pt_press(cidx, m, rect, shift) {
                return CurvePressOutcome::Grab;
            }
            match self.curve_profile_screen_rect(cidx, rect) {
                Some(r) if r.contains(m) => return CurvePressOutcome::Empty,
                _ => return CurvePressOutcome::No,
            }
        }

        CurvePressOutcome::No
    }

    /// LMB-press dispatch for the point editor (called before the selection-box fallback). Returns
    /// true if it consumed the press (started a direct drag or toggled a point); false → the caller
    /// starts a `SelectionBox` (box-select / click-deselect / far shift-add).
    ///
    /// Precedence (gizmo handle > point edit) is enforced by the caller's `!gizmo_hovered`/`!armed`
    /// guard. Gestures: point → select+drag (shift = toggle, no drag); edge → segment drag
    /// (shift = add point on that edge + drag-follow); empty → fall through to box-select.
    pub(crate) fn curve_pt_press(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect, shift: bool) -> bool {
        if let Some(pi) = self.curve_pt_pick(idx, m, rect) {
            if shift {
                if !self.curve_pt_sel.remove(&pi) { self.curve_pt_sel.insert(pi); }
                self.mark_dirty(DirtyFlags::RENDER);
                return true;
            }
            if !self.curve_pt_sel.contains(&pi) {
                self.curve_pt_sel.clear();
                self.curve_pt_sel.insert(pi);
            }
            return self.curve_pt_begin_direct(idx, m, 1);
        }
        if let Some(ei) = self.curve_pt_edge_pick(idx, m, rect) {
            if shift {
                return self.curve_pt_add_and_drag(idx, m, rect);
            }
            let n = self.curve_profile_verts(idx).len();
            self.curve_pt_sel.clear();
            self.curve_pt_sel.insert(ei);
            self.curve_pt_sel.insert((ei + 1) % n);
            return self.curve_pt_begin_direct(idx, m, 2);
        }
        false
    }

    /// Seed + install a direct grab drag on the CURRENT selection (episode = one undo on release).
    fn curve_pt_begin_direct(&mut self, idx: usize, m: egui::Pos2, press_kind: u8) -> bool {
        let drag = match self.curve_pt_seed_drag(idx, TransformKind::Grab, m, true, press_kind) {
            Some(d) => d, None => return false,
        };
        self.begin_episode("Curve Move Point");
        self.start_operation(ActiveTool::CurvePointTransform(drag));
        true
    }

    /// Shift-add on an edge: insert a point at the cursor UV then drag-follow it, all in ONE episode
    /// (single undo covering add + move).
    fn curve_pt_add_and_drag(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect) -> bool {
        let (insert_at, uv) = match self.curve_pt_edge_insert(idx, m, rect) { Some(x) => x, None => return false };
        let uid = self.scene.primitives[idx].prim_uid;
        self.begin_episode("Add Curve Point");
        self.write_prim(idx, |p| {
            if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                let at = insert_at.min(profile.verts.len());
                profile.verts.insert(at, CurveVertex { pos: uv, bevel_radius: 0.0 });
            }
        });
        self.curve_pt_sel.clear();
        self.curve_pt_sel.insert(insert_at);
        self.curve_pt_owner = Some(uid);
        match self.curve_pt_seed_drag(idx, TransformKind::Grab, m, true, 3) {
            Some(drag) => { self.start_operation(ActiveTool::CurvePointTransform(drag)); }
            None => { self.commit_episode(); }
        }
        true
    }

    /// LMB-release for a direct point/edge/add drag. Commits when it became a real drag OR added a
    /// point; a pure click (never moved past threshold) drops the empty episode (no zero-delta undo)
    /// but keeps the press-time selection. Returns true if it handled the release.
    pub(crate) fn curve_pt_release_drag(&mut self) -> bool {
        let (direct, moved, press_kind) = match self.tool.current.as_curve_point_transform() {
            Some(d) => (d.direct, d.moved, d.press_kind),
            None => return false,
        };
        if !direct { return false; }
        if moved || press_kind == 3 {
            self.curve_pt_confirm_transform();
        } else {
            self.cancel_episode();
            self.tool.current = ActiveTool::Idle;
            self.mark_dirty(DirtyFlags::RENDER);
        }
        true
    }

    /// RMB on a profile point → delete it (≥3 guard). Returns true if a point was hit (so the caller
    /// suppresses the normal RMB cancel/context menu).
    pub(crate) fn curve_pt_rmb_delete(&mut self, m: egui::Pos2, rect: egui::Rect) -> bool {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return false };
        let pi = match self.curve_pt_pick(idx, m, rect) { Some(p) => p, None => return false };
        if self.curve_pt_remove(idx, vec![pi], "Delete Curve Point") { self.toast("Point deleted"); }
        true
    }

    /// Scroll over a point/selection → cycle its corner TYPE (Sharp→Round→Chamfer). Contextual: only
    /// fires when the editor is active and the cursor is over a handle. If the hovered point is part
    /// of a multi-selection, the whole selection cycles together. Returns true if it consumed the
    /// scroll. `forward` = scroll-up.
    pub(crate) fn curve_pt_scroll_cycle(&mut self, idx: usize, m: egui::Pos2, rect: egui::Rect, forward: bool) -> bool {
        let hp = match self.curve_pt_pick(idx, m, rect) { Some(p) => p, None => return false };
        let verts = self.curve_profile_verts(idx);
        let n = verts.len();
        if hp >= n { return false; }
        let mut targets: Vec<usize> = if self.curve_pt_sel.contains(&hp) && self.curve_pt_sel.len() > 1 {
            self.curve_pt_sel.iter().copied().filter(|&i| i < n).collect()
        } else { vec![hp] };
        targets.dedup();
        // Next mode from the hovered point's current mode.
        let cur = verts[hp].bevel_radius;
        self.curve_apply_corner_cycle(idx, targets, cur, forward);
        true
    }

    // ============================================================== modal transform (G/R/S)

    /// Start a modal G/R/S transform on the selected profile points (mirrors `poly_start_transform`,
    /// but in the profile plane). One undo per drag via episode.
    pub(crate) fn curve_pt_start_transform(&mut self, kind: TransformKind, mouse: egui::Pos2) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        if self.curve_pt_sel.is_empty() { return; }
        let drag = match self.curve_pt_seed_drag(idx, kind, mouse, false, 0) { Some(d) => d, None => return };
        self.begin_episode(match kind {
            TransformKind::Grab => "Curve Grab",
            TransformKind::Rotate => "Curve Rotate",
            TransformKind::Scale => "Curve Scale",
            TransformKind::Elongate => "Curve Scale",
        });
        self.start_operation(ActiveTool::CurvePointTransform(drag));
    }

    /// Build the modal-transform state from the CURRENT point selection, WITHOUT opening an undo
    /// episode or installing the tool (callers manage the episode — keyboard G/R/S opens a fresh one,
    /// shift-add reuses the add episode). `direct` = pointer-initiated; `press_kind` per `CurvePointDrag`.
    /// Captures the profile-plane frame + the Ctrl/Ctrl+Shift snap anchors (adjacent-edge slide dirs
    /// and the single-point 45° corner anchor) at drag start, per the drag-start-snapshot snap rule.
    fn curve_pt_seed_drag(&self, idx: usize, kind: TransformKind, mouse: egui::Pos2,
                          direct: bool, press_kind: u8) -> Option<CurvePointDrag> {
        let verts = self.curve_profile_verts(idx);
        let original: Vec<(usize, Vec2)> = self.curve_pt_sel.iter()
            .filter(|&&i| i < verts.len())
            .map(|&i| (i, verts[i].pos)).collect();
        if original.is_empty() { return None; }
        let mut center = Vec2::ZERO;
        for &(_, p) in &original { center += p; }
        center /= original.len() as f32;
        let (o, u, v, n) = self.curve_prim_world_frame(idx);
        let uid = self.scene.primitives[idx].prim_uid;
        // Snap anchors (Grab only). Single point → two adjacent edge dirs + prev-vertex anchor;
        // a contiguous 2-point segment → the segment's own axis.
        let (slide_dirs, corner_prev) = curve_snap_anchors(&verts, &original);
        Some(CurvePointDrag {
            prim_uid: uid, kind, axis: AxisConstraint::None,
            original, center, start_mouse: mouse, num_buf: String::new(),
            plane_origin: o, plane_normal: n, plane_u: u, plane_v: v,
            direct, press_kind, moved: false, slide_dirs, corner_prev,
        })
    }

    /// Apply the modal transform for the current mouse position. `ctrl`/`shift` drive the live snap
    /// modes for Grab (Ctrl = 45°/H-V-45 snap, Ctrl+Shift = edge-slide); grid snap rides the global
    /// `viewport.snap_grid` when smart-snap is on. Snapping always uses the drag-start snapshot
    /// (`original` positions + captured anchors), never live neighbours.
    pub(crate) fn curve_pt_update_transform(&mut self, cur: egui::Pos2, rect: egui::Rect, ctrl: bool, shift: bool) {
        let d = match self.tool.current.as_curve_point_transform() {
            Some(d) => (d.prim_uid, d.kind, d.axis, d.center, d.start_mouse, d.original.clone(),
                        d.num_buf.clone(), d.plane_origin, d.plane_normal, d.plane_u, d.plane_v,
                        d.slide_dirs.clone(), d.corner_prev),
            None => return,
        };
        let (uid, kind, axis, center, start_mouse, original, num_buf,
             p_o, p_n, p_u, p_v, slide_dirs, corner_prev) = d;
        let idx = match self.prim_index_by_uid(uid) { Some(i) => i, None => return };

        // Promote to a real drag once the pointer moves past the click threshold (direct drags).
        let moved_px2 = (cur.x - start_mouse.x).powi(2) + (cur.y - start_mouse.y).powi(2);
        if moved_px2 > CURVE_DRAG_PX2 {
            if let Some(dm) = self.tool.current.as_curve_point_transform_mut() { dm.moved = true; }
        }

        let num_val: Option<f32> = if num_buf.is_empty() { None } else { num_buf.parse().ok() };
        let dx = cur.x - start_mouse.x;
        let settings = self.transform_settings();
        // Global grid-snap (reuse the existing viewport snap toggle + step — no new snapper).
        let grid = if self.viewport.smart_snap_on { self.viewport.snap_grid } else { 0.0 };
        let quantize = |uv: Vec2| -> Vec2 {
            if grid > 1e-6 { Vec2::new((uv.x / grid).round() * grid, (uv.y / grid).round() * grid) } else { uv }
        };

        // Map both mouse positions onto the profile plane → UV (for a 1:1 grab).
        let cur_uv = self.ray_plane_hit(cur, rect, p_o, p_n)
            .map(|w| Self::curve_world_to_uv(p_o, p_u, p_v, w));
        let start_uv = self.ray_plane_hit(start_mouse, rect, p_o, p_n)
            .map(|w| Self::curve_world_to_uv(p_o, p_u, p_v, w));

        let new_positions: Vec<(usize, Vec2)> = match kind {
            TransformKind::Grab => {
                let mut delta = match (cur_uv, start_uv) { (Some(c), Some(s)) => c - s, _ => Vec2::ZERO };
                match axis {
                    AxisConstraint::X => delta.y = 0.0,
                    AxisConstraint::Y => delta.x = 0.0,
                    _ => {}
                }
                // Ctrl+Shift = edge-slide along a captured original edge; Ctrl = H/V/45° (or, for a
                // single point with a prev anchor, snap the resulting corner angle to 45°).
                if ctrl && shift && !slide_dirs.is_empty() {
                    delta = curve_snap_slide(delta, &slide_dirs);
                } else if ctrl {
                    if let (Some(prev), true) = (corner_prev, original.len() == 1) {
                        let snapped = curve_snap_point_angle45(prev, original[0].1 + delta);
                        delta = snapped - original[0].1;
                    } else {
                        delta = curve_snap_hv45(delta);
                    }
                }
                original.iter().map(|&(i, p)| (i, quantize(p + delta))).collect()
            }
            TransformKind::Rotate => {
                let ang = num_val.unwrap_or(dx * settings.rotate_speed).to_radians();
                let (s, c) = (ang.sin(), ang.cos());
                original.iter().map(|&(i, p)| {
                    let l = p - center;
                    (i, center + Vec2::new(l.x * c - l.y * s, l.x * s + l.y * c))
                }).collect()
            }
            TransformKind::Scale | TransformKind::Elongate => {
                let f = num_val.unwrap_or_else(|| (1.0 + dx * settings.scale_speed).max(0.01));
                original.iter().map(|&(i, p)| {
                    let mut l = (p - center) * f;
                    match axis {
                        AxisConstraint::X => l.y = (p - center).y,
                        AxisConstraint::Y => l.x = (p - center).x,
                        _ => {}
                    }
                    (i, center + l)
                }).collect()
            }
        };

        self.write_prim(idx, |p| {
            if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                for &(i, np) in &new_positions {
                    if i < profile.verts.len() { profile.verts[i].pos = np; }
                }
            }
        });
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// True when every dragged profile point still sits at its episode-start UV.
    /// `press_kind == 3` (shift-add on edge) always created geometry — never a no-op.
    fn curve_pt_transform_is_noop(&self) -> bool {
        let Some(d) = self.tool.current.as_curve_point_transform() else { return true };
        // Add-then-drag: insert already mutated the profile before the drag; cancel would
        // wipe the new point via episode restore. Always treat as a real change.
        if d.press_kind == 3 { return false; }
        const EPS2: f32 = 1e-12;
        let Some(idx) = self.prim_index_by_uid(d.prim_uid) else { return true };
        let verts = self.curve_profile_verts(idx);
        d.original.iter().all(|&(i, orig)| {
            verts.get(i).map(|v| (v.pos - orig).length_squared() < EPS2).unwrap_or(true)
        })
    }

    /// Confirm the modal transform — keep positions, one undo entry.
    ///
    /// No-op confirm (G/R/S then Enter without moving, or direct release already gated by
    /// `moved`) cancels the episode so Ctrl+Z is not wasted — same honesty class as poly /
    /// object modal G/R/S.
    pub(crate) fn curve_pt_confirm_transform(&mut self) {
        if self.curve_pt_transform_is_noop() {
            self.curve_pt_cancel_transform();
            return;
        }
        self.commit_episode();
        self.tool.current = ActiveTool::Idle;
        self.rebake_no_undo();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Cancel the modal transform — restore original positions.
    pub(crate) fn curve_pt_cancel_transform(&mut self) {
        let restore = self.tool.current.as_curve_point_transform()
            .map(|d| (d.prim_uid, d.original.clone()));
        if let Some((uid, original)) = restore {
            if let Some(idx) = self.prim_index_by_uid(uid) {
                self.write_prim(idx, |p| {
                    if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                        for &(i, op) in &original {
                            if i < profile.verts.len() { profile.verts[i].pos = op; }
                        }
                    }
                });
            }
        }
        self.cancel_episode();
        self.tool.current = ActiveTool::Idle;
        self.rebake_no_undo();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Numeric-buffer input for the active curve transform.
    pub(crate) fn curve_pt_transform_num_input(&mut self, ch: char) {
        if let Some(d) = self.tool.current.as_curve_point_transform_mut() {
            if ch == '\x08' { d.num_buf.pop(); } else { d.num_buf.push(ch); }
        }
    }

    /// Axis lock for the active curve transform (X = UV.x, Y = UV.y).
    pub(crate) fn curve_pt_transform_set_axis(&mut self, axis: AxisConstraint) {
        if let Some(d) = self.tool.current.as_curve_point_transform_mut() {
            d.axis = axis; d.num_buf.clear();
        }
    }

    // ============================================================== TIER 2 — bevel size handle

    /// Begin a press-drag bevel gesture on the PERSISTENT diamond handle — THE one way the radius
    /// changes (the old keyboard-B auto-size modal is gone; B now just points at the diamond).
    /// Gathers the selected points, opens ONE undo episode, and installs
    /// `ActiveTool::CurveBevelTransform` seeded with the CURRENT radius so the value doesn't jump.
    /// Release commits the drag, or (unmoved click) cycles the corner type, via
    /// `curve_bevel_release_direct`. Returns false (no episode) if nothing valid is selected.
    pub(crate) fn curve_bevel_begin(&mut self, mouse: egui::Pos2) -> bool {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return false };
        if self.curve_pt_sel.is_empty() { return false; }
        let verts = self.curve_profile_verts(idx);
        let targets: Vec<usize> = self.curve_pt_sel.iter().copied().filter(|&i| i < verts.len()).collect();
        if targets.is_empty() { return false; }
        let original: Vec<f32> = targets.iter().map(|&i| verts[i].bevel_radius).collect();
        // Sign: first target's polarity; sharp → default round (+1).
        let sign = { let r = verts[targets[0]].bevel_radius; if r < 0.0 { -1.0 } else { 1.0 } };
        let mut anchor = Vec2::ZERO;
        for &i in &targets { anchor += verts[i].pos; }
        anchor /= targets.len() as f32;
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        let uid = self.scene.primitives[idx].prim_uid;
        let cur_r = verts[targets[0]].bevel_radius.abs();
        self.begin_episode("Curve Bevel");
        self.start_operation(ActiveTool::CurveBevelTransform(CurveBevelDrag {
            prim_uid: uid, targets, original, sign, anchor_uv: anchor,
            plane_origin: o, plane_u: u, plane_v: v,
            radius: cur_r, start_mouse: mouse, start_radius: cur_r,
            moved: false,
        }));
        true
    }

    /// Per-frame drive of the diamond drag: radius = start_radius + horizontal drag delta, scaled
    /// relative to the UV-per-pixel of the plane u axis so the feel is resolution-independent.
    pub(crate) fn curve_bevel_update(&mut self, cur: egui::Pos2) {
        let d = match self.tool.current.as_curve_bevel() {
            Some(d) => (d.plane_u, d.start_mouse, d.start_radius),
            None => return,
        };
        let (p_u, start_mouse, start_radius) = d;
        const FINE: f32 = 0.0025;
        let radius = (start_radius + (cur.x - start_mouse.x) * FINE * p_u.length().max(0.1)).max(0.0);
        // Click/drag promotion: past the shared click threshold it's a radius drag.
        let moved_px2 = (cur.x - start_mouse.x).powi(2) + (cur.y - start_mouse.y).powi(2);
        let was_moved = self.tool.current.as_curve_bevel().map_or(false, |d| d.moved);
        let now_moved = was_moved || moved_px2 > CURVE_DRAG_PX2;
        if let Some(dm) = self.tool.current.as_curve_bevel_mut() {
            dm.radius = radius;
            dm.moved = now_moved;
        }
        // Only write the prim once the press became a real drag — an unmoved press must leave the
        // radii untouched (it resolves as a corner-type CLICK on release).
        if now_moved { self.curve_bevel_apply(); }
    }

    /// Write the current radius (× sign) into every target vertex's `bevel_radius`.
    fn curve_bevel_apply(&mut self) {
        let (uid, targets, sign, radius) = match self.tool.current.as_curve_bevel() {
            Some(d) => (d.prim_uid, d.targets.clone(), d.sign, d.radius),
            None => return,
        };
        let idx = match self.prim_index_by_uid(uid) { Some(i) => i, None => return };
        self.write_prim(idx, |p| {
            if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                for &t in &targets {
                    if t < profile.verts.len() { profile.verts[t].bevel_radius = radius * sign; }
                }
            }
        });
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// True when every target still has its pre-modal bevel radius (B then confirm at same size).
    fn curve_bevel_is_noop(&self) -> bool {
        let Some(d) = self.tool.current.as_curve_bevel() else { return true };
        const EPS: f32 = 1e-6;
        let applied = d.radius * d.sign;
        d.original.iter().all(|&orig| (applied - orig).abs() < EPS)
    }

    /// Confirm the bevel gesture (LMB) — keep the radii, one undo entry.
    ///
    /// No-op confirm (B then confirm without changing radius from originals) cancels the episode
    /// so Ctrl+Z is not wasted — same honesty class as curve G/R/S / poly zero-offset.
    pub(crate) fn curve_bevel_confirm(&mut self) {
        if self.curve_bevel_is_noop() {
            self.curve_bevel_cancel();
            return;
        }
        let r = self.tool.current.as_curve_bevel().map(|d| d.radius).unwrap_or(0.0);
        self.commit_episode();
        self.tool.current = ActiveTool::Idle;
        self.rebake_no_undo();
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!("Bevel {:.2}", r));
    }

    /// Cancel the bevel gesture (RMB / Esc) — restore the pre-B radii.
    pub(crate) fn curve_bevel_cancel(&mut self) {
        let restore = self.tool.current.as_curve_bevel()
            .map(|d| (d.prim_uid, d.targets.clone(), d.original.clone()));
        if let Some((uid, targets, original)) = restore {
            if let Some(idx) = self.prim_index_by_uid(uid) {
                self.write_prim(idx, |p| {
                    if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                        for (t, r) in targets.iter().zip(original.iter()) {
                            if *t < profile.verts.len() { profile.verts[*t].bevel_radius = *r; }
                        }
                    }
                });
            }
        }
        self.cancel_episode();
        self.tool.current = ActiveTool::Idle;
        self.rebake_no_undo();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Shared diamond geometry: screen position + label + colour for a bevel diamond at the given
    /// profile anchor/radius/sign. Handle sits at the anchor offset along camera-right, scaled by
    /// the radius — the ONE placement rule for both the live modal handle and the persistent one.
    fn curve_diamond_screen(&self, rect: egui::Rect, plane_origin: Vec3, plane_u: Vec3, plane_v: Vec3,
                            anchor_uv: Vec2, radius: f32, sign: f32)
        -> Option<(egui::Pos2, String, (u8, u8, u8))> {
        let world = plane_origin + plane_u * anchor_uv.x + plane_v * anchor_uv.y;
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        let cam_right = (orbit * Vec3::X).normalize_or_zero();
        let handle = world + cam_right * (radius * plane_u.length().max(0.1) + 0.15);
        let sp = world_to_screen(handle, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg)?;
        let (label, rgb) = if sign < 0.0 {
            (format!("Chamfer {:.2}", radius), (255u8, 150u8, 60u8))
        } else if radius <= 1e-6 {
            ("Sharp (click: type, drag: size)".to_string(), (0u8, 200u8, 255u8))
        } else {
            (format!("Bevel {:.2}", radius), (80u8, 220u8, 120u8))
        };
        Some((sp, label, rgb))
    }

    /// Screen position + label + colour for the bevel diamond handle (drawn during the modal).
    fn curve_bevel_handle(&self, rect: egui::Rect) -> Option<(egui::Pos2, String, (u8, u8, u8))> {
        let d = self.tool.current.as_curve_bevel()?;
        self.curve_diamond_screen(rect, d.plane_origin, d.plane_u, d.plane_v,
                                  d.anchor_uv, d.radius, d.sign)
    }

    /// PERSISTENT bevel diamond, shown whenever the point editor is active, the tool is idle, and
    /// at least one profile point is selected (the SDF design language: the gizmo owns bevel).
    /// Position/radius/sign come from the CURRENT selection (anchor = selection centroid, value =
    /// first selected point's `bevel_radius`). `None` = not shown → must not hit-test.
    pub(crate) fn curve_idle_diamond(&self, rect: egui::Rect) -> Option<(egui::Pos2, String, (u8, u8, u8))> {
        if !self.tool.current.is_idle() { return None; }
        let idx = self.curve_edit_prim_index()?;
        let verts = self.curve_profile_verts(idx);
        let sel: Vec<usize> = self.curve_pt_sel.iter().copied().filter(|&i| i < verts.len()).collect();
        if sel.is_empty() { return None; }
        let mut anchor = Vec2::ZERO;
        for &i in &sel { anchor += verts[i].pos; }
        anchor /= sel.len() as f32;
        let r0 = verts[sel[0]].bevel_radius;
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        self.curve_diamond_screen(rect, o, u, v, anchor, r0.abs(), if r0 < 0.0 { -1.0 } else { 1.0 })
    }

    /// LMB-release for the (press-held) diamond gesture. Moved past the click threshold →
    /// commit the radius drag (one undo, "Bevel r" toast). Unmoved CLICK → drop the empty episode
    /// and cycle the corner TYPE of every target (Sharp→Round→Chamfer, its own transaction+toast).
    pub(crate) fn curve_bevel_release_direct(&mut self) -> bool {
        let (moved, uid, targets) = match self.tool.current.as_curve_bevel() {
            Some(d) => (d.moved, d.prim_uid, d.targets.clone()),
            None => return false,
        };
        if moved {
            self.curve_bevel_confirm();
        } else {
            // Pure click: restore (radius untouched anyway), drop the episode, then type-cycle.
            self.curve_bevel_cancel();
            if let Some(idx) = self.prim_index_by_uid(uid) {
                let verts = self.curve_profile_verts(idx);
                let base_cur = targets.first().and_then(|&t| verts.get(t)).map_or(0.0, |v| v.bevel_radius);
                self.curve_apply_corner_cycle(idx, targets, base_cur, true);
            }
        }
        true
    }

    // ============================================================== TIER 3 — edge tools

    /// If exactly two ADJACENT points are selected (including the closed wrap i=0 / last), return
    /// them as the ordered edge `(i, j)` with `j = (i+1) mod n`. Else `None`.
    pub(crate) fn curve_selected_edge(&self, idx: usize) -> Option<(usize, usize)> {
        if self.curve_pt_sel.len() != 2 { return None; }
        let n = self.curve_profile_verts(idx).len();
        if n < 3 { return None; }
        let mut it = self.curve_pt_sel.iter().copied();
        let (a, b) = (it.next()?, it.next()?);
        // Stale-index guard: the selection can briefly outlive a topology change (delete/undo in
        // the same frame, before curve_pt_tick retains). An out-of-range index must never reach
        // the callers' `verts[i]` (toolbar draw / flatten) — that is a panic, not an edge.
        if a >= n || b >= n { return None; }
        if (a + 1) % n == b { Some((a, b)) }
        else if (b + 1) % n == a { Some((b, a)) }
        else { None }
    }

    /// Flatten the selected edge to H or V (whichever it's closest to), symmetric about the midpoint.
    /// Already-axis-aligned edges leave positions unchanged → no undo entry.
    pub(crate) fn curve_flatten_selected(&mut self) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        let (i, j) = match self.curve_selected_edge(idx) { Some(e) => e, None => return };
        let verts = self.curve_profile_verts(idx);
        let (na, nb) = curve_flatten_edge(verts[i].pos, verts[j].pos);
        const EPS2: f32 = 1e-12;
        if (na - verts[i].pos).length_squared() < EPS2 && (nb - verts[j].pos).length_squared() < EPS2 {
            self.toast("Flatten: edge already axis-aligned");
            return;
        }
        self.transaction("Flatten Edge", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                    if i < profile.verts.len() { profile.verts[i].pos = na; }
                    if j < profile.verts.len() { profile.verts[j].pos = nb; }
                }
            });
        });
        self.toast("Edge flattened");
    }

    /// Arm the "Copy angle" pick: the next LMB on another segment sets the selected edge's slope.
    pub(crate) fn curve_copy_angle_arm(&mut self) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        if let Some(edge) = self.curve_selected_edge(idx) {
            self.curve_copy_angle_arm = Some(edge);
            self.toast("Copy angle: click a reference segment (Esc to cancel)");
        }
    }

    /// Handle an LMB during Copy-angle pick mode: read the segment under the cursor and set the
    /// armed edge's slope to it (order-preserving). Returns true if the pick was consumed.
    pub(crate) fn curve_copy_angle_pick(&mut self, m: egui::Pos2, rect: egui::Rect) -> bool {
        let (i, j) = match self.curve_copy_angle_arm { Some(e) => e, None => return false };
        let idx = match self.curve_edit_prim_index() { Some(x) => x, None => { self.curve_copy_angle_arm = None; return false; } };
        let ref_edge = match self.curve_pt_edge_pick(idx, m, rect) { Some(e) => e, None => {
            self.toast("No segment under cursor"); return true; // consume (stay armed would confuse) → cancel
        }};
        let verts = self.curve_profile_verts(idx);
        let n = verts.len();
        // Stale-arm guard: the armed edge was captured on an earlier frame — topology may have
        // changed (add/delete/undo) since. Cancel instead of indexing out of range.
        if i >= n || j >= n || ref_edge >= n {
            self.curve_copy_angle_arm = None;
            self.toast("Copy angle cancelled (profile changed)");
            return true;
        }
        // Ignore picking the edge itself.
        let rj = (ref_edge + 1) % n;
        let ref_dir = verts[rj].pos - verts[ref_edge].pos;
        let (na, nb) = curve_copy_angle(verts[i].pos, verts[j].pos, ref_dir);
        // Same slope as the reference (or degenerate ref) → positions unchanged → no empty undo.
        const EPS2: f32 = 1e-12;
        if (na - verts[i].pos).length_squared() < EPS2 && (nb - verts[j].pos).length_squared() < EPS2 {
            self.curve_copy_angle_arm = None;
            self.toast("Copy angle: already matched (or zero-length ref)");
            return true;
        }
        self.transaction("Copy Angle", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::CurveExtrude { profile, .. } = &mut p.shape {
                    if i < profile.verts.len() { profile.verts[i].pos = na; }
                    if j < profile.verts.len() { profile.verts[j].pos = nb; }
                }
            });
        });
        self.curve_copy_angle_arm = None;
        self.toast("Angle copied");
        true
    }

    /// Draw the Tier-3 floating edge toolbar (Flatten / Copy angle) at the selected segment's
    /// midpoint. Small egui buttons positioned above the segment; they interact via `ui.interact`
    /// so they don't eat viewport clicks meant for points. No-op unless exactly 2 adjacent points
    /// are selected.
    pub(crate) fn draw_curve_edge_toolbar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        if self.tool.current.is_curve_bevel() { return; }
        let (i, j) = match self.curve_selected_edge(idx) { Some(e) => e, None => return };
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        let verts = self.curve_profile_verts(idx);
        let mid_uv = (verts[i].pos + verts[j].pos) * 0.5;
        let mid_w = o + u * mid_uv.x + v * mid_uv.y;
        let sp = match world_to_screen(mid_w, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            Some(p) => p, None => return,
        };
        let armed = self.curve_copy_angle_arm.is_some();
        // Two small buttons, centered above the segment midpoint.
        let bw = 62.0f32; let bh = 20.0f32; let gap = 4.0f32;
        let total = bw * 2.0 + gap;
        let top = sp.y - bh - 14.0;
        let x0 = sp.x - total * 0.5;
        let mut do_flatten = false; let mut do_copy = false;
        let btn = |ui: &mut egui::Ui, r: egui::Rect, id: &str, text: &str, hot: bool| -> bool {
            let resp = ui.interact(r, ui.id().with(id), egui::Sense::click());
            let bg = if resp.hovered() { egui::Color32::from_rgb(90, 74, 180) }
                     else if hot { egui::Color32::from_rgb(123, 97, 255) }
                     else { egui::Color32::from_rgba_unmultiplied(40, 40, 55, 220) };
            ui.painter().rect_filled(r, 4.0, bg);
            ui.painter().rect_stroke(r, 4.0, egui::Stroke::new(1.0, egui::Color32::from_gray(30)), egui::StrokeKind::Middle);
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text,
                egui::FontId::proportional(11.0), egui::Color32::WHITE);
            resp.clicked()
        };
        let r_flat = egui::Rect::from_min_size(egui::pos2(x0, top), egui::vec2(bw, bh));
        let r_copy = egui::Rect::from_min_size(egui::pos2(x0 + bw + gap, top), egui::vec2(bw, bh));
        if btn(ui, r_flat, "curve_flatten_btn", "Flatten", false) { do_flatten = true; }
        if btn(ui, r_copy, "curve_copyang_btn", "Copy angle", armed) { do_copy = true; }
        if do_flatten { self.curve_flatten_selected(); }
        if do_copy {
            if armed { self.curve_copy_angle_arm = None; self.toast("Copy angle cancelled"); }
            else { self.curve_copy_angle_arm(); }
        }
    }

    // ============================================================== overlay

    /// Draw the profile-point handles + polygon outline for the active CurveExtrude prim.
    pub(crate) fn draw_curve_point_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let idx = match self.curve_edit_prim_index() { Some(i) => i, None => return };
        let (o, u, v, _n) = self.curve_prim_world_frame(idx);
        let (cam_pos, orbit, aspect, ortho_hw) = self.curve_cam(rect);
        let verts = self.curve_profile_verts(idx);
        if verts.is_empty() { return; }
        let to_screen = |uv: Vec2| world_to_screen(o + u * uv.x + v * uv.y, cam_pos, orbit, rect, aspect, ortho_hw, self.render.fov_deg);

        // Outline (closed polygon).
        let edge_color = egui::Color32::from_rgba_unmultiplied(120, 200, 255, 150);
        let cnt = verts.len();
        for i in 0..cnt {
            let j = (i + 1) % cnt;
            if let (Some(a), Some(b)) = (to_screen(verts[i].pos), to_screen(verts[j].pos)) {
                painter.line_segment([a, b], egui::Stroke::new(1.5, edge_color));
            }
        }
        // Handles, colored by corner mode; selected = bigger + white ring; hovered = yellow.
        for (i, cv) in verts.iter().enumerate() {
            let sp = match to_screen(cv.pos) { Some(p) => p, None => continue };
            let base = if cv.is_round() { egui::Color32::from_rgb(80, 220, 120) }
                       else if cv.is_chamfer() { egui::Color32::from_rgb(255, 150, 60) }
                       else { egui::Color32::from_rgb(0, 200, 255) };
            let selected = self.curve_pt_sel.contains(&i);
            let hovered = self.curve_pt_hover == Some(i);
            if selected {
                painter.circle_filled(sp, 5.0, base);
                painter.circle_stroke(sp, 6.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
            } else if hovered {
                painter.circle_filled(sp, 4.5, egui::Color32::from_rgb(255, 220, 100));
            } else {
                painter.circle_filled(sp, 4.0, base);
            }
        }
        // Live bevel modal: draw the reused diamond handle at the anchor (Tier 2).
        if self.tool.current.is_curve_bevel() {
            if let Some((dp, label, rgb)) = self.curve_bevel_handle(rect) {
                let dragging = self.tool.current.as_curve_bevel().map_or(false, |d| d.moved);
                self.draw_diamond_handle(painter, dp, rgb, &label, !dragging, dragging);
            }
        }
        // Persistent diamond (idle + selection): the gizmo that OWNS bevel — drag = radius,
        // click = corner-type cycle. Same visual primitive as the modal handle.
        else if let Some((dp, label, rgb)) = self.curve_idle_diamond(rect) {
            let hovered = (self.viewport.viewport_mouse - dp).length() < 16.0;
            self.draw_diamond_handle(painter, dp, rgb, &label, hovered, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use glam::Vec2;

    // Mirror of `curve_pt_edge_insert`'s index rule: splitting nearest edge (i,i+1) inserts at i+1.
    fn insert_index_for_edge(nearest_edge_first: usize) -> usize { nearest_edge_first + 1 }

    #[test]
    fn edge_split_insert_index() {
        // 4-vert square: nearest edge (1,2) → new vertex inserted at index 2 (between old 1 and 2).
        assert_eq!(insert_index_for_edge(1), 2);
        // nearest edge (3,0) [the closing edge] → insert at 4 (append to end, still between 3 and 0).
        assert_eq!(insert_index_for_edge(3), 4);
    }

    #[test]
    fn delete_min_three_guard() {
        // Guard: cur_len - remove_ct must stay >= 3.
        let ok = |len: usize, rm: usize| len - rm >= 3;
        assert!(!ok(4, 2), "4 verts, delete 2 → 2 < 3, refuse");
        assert!(ok(6, 3), "6 verts, delete 3 → 3, allowed");
        assert!(!ok(3, 1), "3 verts, delete 1 → 2, refuse");
    }

    #[test]
    fn corner_cycle_sign() {
        // Sharp(0) → Round(+r) → Chamfer(-r) → Sharp(0).
        let next = |cur: f32, r: f32| -> f32 {
            let mag = if cur.abs() > 1e-6 { cur.abs() } else { r };
            if cur.abs() <= 1e-6 { mag } else if cur > 0.0 { -mag } else { 0.0 }
        };
        let r = 0.1;
        let a = next(0.0, r);   assert!(a > 0.0, "sharp→round");
        let b = next(a, r);     assert!(b < 0.0, "round→chamfer");
        let c = next(b, r);     assert_eq!(c, 0.0, "chamfer→sharp");
    }

    #[test]
    fn editor_activation_gate() {
        use super::curve_point_editor_active;
        // Active: SDF, not sculpt, exactly one CurveExtrude prim selected — in ANY non-sculpt mode.
        assert!(curve_point_editor_active(true, false, 1, true), "single curve prim in SDF → active");
        // Wrong shape → inactive (must not hijack box/cylinder/etc.).
        assert!(!curve_point_editor_active(true, false, 1, false), "single non-curve prim → inactive");
        // Two selected → inactive.
        assert!(!curve_point_editor_active(true, false, 2, true), "2 selected → inactive");
        // Zero selected → inactive.
        assert!(!curve_point_editor_active(true, false, 0, true), "nothing selected → inactive");
        // Sculpt room → inactive even for a curve prim.
        assert!(!curve_point_editor_active(true, true, 1, true), "sculpt mode → inactive");
        // Non-SDF context (poly/voxel) → inactive.
        assert!(!curve_point_editor_active(false, false, 1, true), "non-SDF → inactive");
    }

    #[test]
    fn corner_mode_cycle_forward_and_back() {
        use super::curve_next_corner_mode;
        // Forward: sharp(0) → round(+1) → chamfer(-1) → sharp(0).
        assert_eq!(curve_next_corner_mode(0.0, true), 1);
        assert_eq!(curve_next_corner_mode(0.5, true), -1);
        assert_eq!(curve_next_corner_mode(-0.5, true), 0);
        // Backward reverses: sharp → chamfer → round → sharp.
        assert_eq!(curve_next_corner_mode(0.0, false), -1);
        assert_eq!(curve_next_corner_mode(-0.5, false), 1);
        assert_eq!(curve_next_corner_mode(0.5, false), 0);
    }

    #[test]
    fn snap_hv45_locks_to_octants() {
        use super::curve_snap_hv45;
        // A mostly-horizontal delta snaps to pure horizontal, preserving x magnitude.
        let d = curve_snap_hv45(Vec2::new(1.0, 0.1));
        assert!(d.y.abs() < 1e-4 && (d.x - 1.0).abs() < 0.02, "≈horizontal → H, got {:?}", d);
        // A ~45° delta snaps onto the diagonal (equal components).
        let d = curve_snap_hv45(Vec2::new(1.0, 0.9));
        assert!((d.x - d.y).abs() < 1e-3, "≈45° → equal components, got {:?}", d);
        // Zero stays zero.
        assert_eq!(curve_snap_hv45(Vec2::ZERO), Vec2::ZERO);
    }

    #[test]
    fn snap_point_angle45_from_prev() {
        use super::curve_snap_point_angle45;
        let prev = Vec2::new(0.0, 0.0);
        // A point almost due-east of prev snaps onto the 0° ray, keeping its distance.
        let out = curve_snap_point_angle45(prev, Vec2::new(2.0, 0.2));
        assert!(out.y.abs() < 1e-3, "snaps to horizontal ray, got {:?}", out);
        assert!((out.length() - Vec2::new(2.0, 0.2).length()).abs() < 1e-4, "distance preserved");
    }

    #[test]
    fn snap_slide_projects_onto_best_dir() {
        use super::curve_snap_slide;
        let dirs = [Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)];
        // Delta mostly along +x → constrained to the x axis (y dropped).
        let out = super::curve_snap_slide(Vec2::new(3.0, 0.4), &dirs);
        assert!((out - Vec2::new(3.0, 0.0)).length() < 1e-4, "slides along x, got {:?}", out);
        // Empty candidate list → delta passes through unchanged.
        assert_eq!(curve_snap_slide(Vec2::new(1.0, 2.0), &[]), Vec2::new(1.0, 2.0));
    }

    #[test]
    fn snap_anchors_single_point_and_segment() {
        use super::curve_snap_anchors;
        use core_math::primitives::CurveVertex;
        // Unit square (CCW): 0(-1,-1) 1(1,-1) 2(1,1) 3(-1,1).
        let sq: Vec<CurveVertex> = [(-1.0,-1.0),(1.0,-1.0),(1.0,1.0),(-1.0,1.0)]
            .iter().map(|&(x,y)| CurveVertex { pos: Vec2::new(x,y), bevel_radius: 0.0 }).collect();
        // Single point 1 → two incident-edge dirs + prev vertex = vert 0.
        let (dirs, prev) = curve_snap_anchors(&sq, &[(1, sq[1].pos)]);
        assert_eq!(dirs.len(), 2);
        assert_eq!(prev, Some(sq[0].pos));
        // Adjacent pair (1,2) → the segment's own axis (vertical), no corner anchor.
        let (dirs, prev) = curve_snap_anchors(&sq, &[(1, sq[1].pos), (2, sq[2].pos)]);
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].x.abs() < 1e-4 && dirs[0].y.abs() > 0.9, "segment axis vertical, got {:?}", dirs[0]);
        assert_eq!(prev, None);
        // Non-adjacent pair (0,2) → no slide anchor.
        let (dirs, _) = curve_snap_anchors(&sq, &[(0, sq[0].pos), (2, sq[2].pos)]);
        assert!(dirs.is_empty());
    }

    #[test]
    fn subdivide_selected_edge_midpoints() {
        // Pure model of curve_pt_subdivide's insert plan: edges with BOTH endpoints selected get a
        // midpoint inserted at i+1; inserts run high→low. 4-square, select all → 4 new midpoints.
        let n = 4usize;
        let sel: std::collections::BTreeSet<usize> = (0..n).collect();
        let mut inserts: Vec<usize> = (0..n).filter(|&i| {
            let j = (i + 1) % n; sel.contains(&i) && sel.contains(&j)
        }).map(|i| i + 1).collect();
        inserts.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(inserts, vec![4, 3, 2, 1], "closing edge appends at n, others between");
    }

    #[test]
    fn flatten_edge_horizontal_choice_symmetric() {
        use super::curve_flatten_edge;
        // A mostly-horizontal edge (dx > dy) → equalize y to the midpoint, symmetric about mid.
        let a = Vec2::new(0.0, 0.2);
        let b = Vec2::new(2.0, -0.4);
        let (na, nb) = curve_flatten_edge(a, b);
        let mid_y = (a.y + b.y) * 0.5;
        assert!((na.y - mid_y).abs() < 1e-6 && (nb.y - mid_y).abs() < 1e-6, "both y snap to midpoint");
        assert_eq!(na.x, a.x); assert_eq!(nb.x, b.x); // x preserved
        // Midpoint invariant (symmetric pivot).
        let m0 = (a + b) * 0.5; let m1 = (na + nb) * 0.5;
        assert!((m0 - m1).length() < 1e-6, "midpoint preserved");
    }

    #[test]
    fn flatten_edge_vertical_choice() {
        use super::curve_flatten_edge;
        // A mostly-vertical edge (dy > dx) → equalize x to the midpoint.
        let a = Vec2::new(0.1, 0.0);
        let b = Vec2::new(-0.3, 3.0);
        let (na, nb) = curve_flatten_edge(a, b);
        let mid_x = (a.x + b.x) * 0.5;
        assert!((na.x - mid_x).abs() < 1e-6 && (nb.x - mid_x).abs() < 1e-6, "both x snap to midpoint");
        assert_eq!(na.y, a.y); assert_eq!(nb.y, b.y);
    }

    #[test]
    fn copy_angle_preserves_length_midpoint_and_order() {
        use super::curve_copy_angle;
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(2.0, 0.0); // horizontal, length 2, mid (1,0)
        // Reference pointing straight UP → selected edge becomes vertical, same length + midpoint.
        let (na, nb) = curve_copy_angle(a, b, Vec2::new(0.0, 5.0));
        assert!(((nb - na).length() - 2.0).abs() < 1e-5, "length preserved");
        let m = (na + nb) * 0.5;
        assert!((m - Vec2::new(1.0, 0.0)).length() < 1e-5, "midpoint preserved");
        // Order preserved: na is the image of a (was the -x/lower end); after aligning to +y ref,
        // a→b direction should still point the SAME rough way as the (flipped-to-match) ref.
        let new_dir = (nb - na).normalize_or_zero();
        // Reference direction pointed +y; endpoint order preserved means na→nb has POSITIVE dot with
        // the (order-corrected) reference. Original a→b was +x; ref +y; dot(+x,+y)=0 (not <0) so no
        // flip → na→nb points +y.
        assert!(new_dir.y > 0.9, "a→b points along +y (no unwanted swap), got {:?}", new_dir);
    }

    #[test]
    fn copy_angle_flips_reference_to_keep_order() {
        use super::curve_copy_angle;
        // Selected edge points +x; reference points -x (opposite). Order preservation must flip the
        // reference 180° so na→nb still points +x (the two points don't swap sides).
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(2.0, 0.0);
        let (na, nb) = curve_copy_angle(a, b, Vec2::new(-3.0, 0.0));
        let new_dir = (nb - na).normalize_or_zero();
        assert!(new_dir.x > 0.9, "a→b still points +x after 180° flip, got {:?}", new_dir);
    }

    #[test]
    fn centroid_of_points() {
        let pts = [Vec2::new(0.0, 0.0), Vec2::new(2.0, 0.0), Vec2::new(2.0, 2.0), Vec2::new(0.0, 2.0)];
        let mut c = Vec2::ZERO;
        for &p in &pts { c += p; }
        c /= pts.len() as f32;
        assert!((c - Vec2::new(1.0, 1.0)).length() < 1e-6);
    }
}
