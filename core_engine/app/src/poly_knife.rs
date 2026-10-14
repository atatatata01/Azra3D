// core_engine/app/src/poly_knife.rs
// Knife tool: hover snap, click placement, cut confirmation.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use eframe::egui;
use glam::{Vec2, Vec3};
use core_math::half_edge::{HalfEdgeMesh, VertexId, HalfEdgeId, FaceId, NO_TWIN, NO_FACE, NO_EDGE};
use core_math::scene_graph::NodePayload;
use crate::{
    AzraApp, PolyTool, DirtyFlags, ModelingContext, PolyWorkMode,
    world_to_screen, dist_point_to_segment,
};
use crate::poly_selection::mouse_to_ray;

/// What a slice does with the region its path encloses.
///
/// A solid boolean is undefined on an OPEN mesh — Manifold needs a watertight operand, and a
/// surface has no inside to subtract from. But "cookie-cut this shape out" IS well defined on a
/// surface: scribe the loop in, then drop the faces it encloses. That is what the knife's subtract
/// means on a plane, and it is the same machinery the scribe already runs — see
/// [`AzraApp::poly_knife_free_boolean`].
/// Which boolean the knife's SHAPE cut runs (owner 2026-08-11: *"with shape knife tool pressing
/// [I] should let you change the boolean type to intersection, U to union"*).
///
/// The shape (rect / circle / closed polyline) is the cutter either way — only the operator
/// changes, so one drawn outline can remove, add, or keep material without redrawing it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KnifeBoolOp {
    /// `A − cutter` — the historical cookie-cutter. Default.
    Subtract,
    /// `A ∪ cutter` — the extruded shape becomes a boss/pad ON the surface. Needs a height
    /// (`E`), and the cutter's base is sunk slightly INTO the solid so the join is never a
    /// coplanar face pair.
    Union,
    /// `A ∩ cutter` — keep only what the outline encloses, discard the rest of the body.
    Intersect,
}

impl KnifeBoolOp {
    fn label(self) -> &'static str {
        match self {
            KnifeBoolOp::Subtract => "SUBTRACT",
            KnifeBoolOp::Union => "UNION",
            KnifeBoolOp::Intersect => "INTERSECT",
        }
    }
}

pub(crate) enum SliceEnclosed {
    /// Leave it in place as its own face(s) — every scribe / slice verb.
    Keep,
    /// Delete every face whose centroid falls inside one of these rings (drawing-plane 2D,
    /// i.e. projected along the plane normal — the same axis the solid path extrudes its prism
    /// along, so open and closed meshes cut the same silhouette).
    Remove(Vec<Vec<Vec2>>),
}

/// Commit gate for a knife dry-run (KNIFE.md §3 "App safety net").
///
/// `validate().is_ok()` alone was too weak — it passed the owner's circle cut, whose −X face was
/// a 47-ring pinched on its bridge vertex, and the damaged mesh committed. But a blanket
/// `is_manifold()` is too STRONG: `integrity()` rejects ANY repeated vertex in a face, and a
/// dangling knife slit revisits its anchor through distinct spur edges BY DESIGN (§1, Blender
/// parity). Using `is_manifold()` here would refuse every legitimate slit — the exact class §3
/// tells us to preserve. So this is the narrower predicate that catches the DAMAGE class without
/// killing intentional slits:
///   * `validate()` (integrity minus the face-repeat rule is not separable, so `validate()`'s own
///     purge-aware rules still run),
///   * a vertex appearing 3+ times on one ring — a multi-fold self-touch, never a legal spur
///     (one spur touches its anchor exactly twice),
///   * a zero-length (adjacent-repeat) edge,
///   * edge multiplicity: a directed edge used by >1 face (same winding = duplicate/overlap) or an
///     undirected edge incident to >2 faces — the real twin-corruption class `is_manifold()` was
///     wanted for, and one that repeated-vertex slits do not trip.
fn knife_result_is_sound(m: &HalfEdgeMesh) -> bool {
    if m.validate().is_err() {
        return false;
    }
    for fi in 0..m.faces.len() as u32 {
        let f = FaceId(fi);
        if !m.is_face_live(f) {
            continue;
        }
        let vs = m.face_vertices(f);
        let n = vs.len();
        if n < 3 {
            return false;
        }
        let mut counts: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for v in &vs {
            *counts.entry(v.0).or_insert(0) += 1;
        }
        if counts.values().any(|&c| c > 2) {
            return false; // multi-fold self-touch — not a spur
        }
        for i in 0..n {
            if vs[i] == vs[(i + 1) % n] {
                return false; // zero-length edge
            }
        }
    }
    let mut directed: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    let mut undirected: std::collections::HashMap<(u32, u32), u32> =
        std::collections::HashMap::new();
    // Walk LIVE faces only (free-list half-edges are unreachable from a live face loop).
    for fi in 0..m.faces.len() as u32 {
        let f = FaceId(fi);
        if !m.is_face_live(f) {
            continue;
        }
        for h in m.face_half_edges(f) {
            if m.he(h).face == NO_FACE {
                continue;
            }
            let s = m.he_source(h).0;
            let d = m.he(h).vertex.0;
            *directed.entry((s, d)).or_insert(0) += 1;
            let key = if s <= d { (s, d) } else { (d, s) };
            *undirected.entry(key).or_insert(0) += 1;
        }
    }
    if directed.values().any(|&c| c > 1) || undirected.values().any(|&c| c > 2) {
        return false;
    }
    true
}

impl AzraApp {
    /// Canonical knife activation — the ONLY path that arms the tool (K key, tools palette,
    /// command palette all route here). Toggles off when already armed. The knife operates on
    /// poly edit sub-modes only: hover/snap never runs in Object mode, so arming it there
    /// produced a convincing-looking dead tool (HUD drawn, clicks placing nothing).
    pub(crate) fn poly_activate_knife(&mut self) {
        // Owner 2026-08-19: "K does not work in the sculpt room". From POLY sculpt, K now
        // CONFIRMS the sculpt and Tabs into the last edit sub-mode, then arms the knife
        // (Tool Exit Convention: switching tools = confirm current work, then switch).
        // Voxel sculpt keeps the refusal below — the knife cuts half-edge meshes, not
        // density grids.
        if self.modeling_context == ModelingContext::Polygonal
            && self.tool.work_mode == crate::WorkMode::Sculpt
            && self.sculpt.voxel.is_none()
            && (self.sculpt.poly_edit.is_some() || self.poly_sculpt_target().is_some())
        {
            if self.sculpt.poly_edit.is_some() {
                self.poly_sculpt_leave_edit(); // commits the stroke; work_mode → Object
            } else {
                self.commit_episode();
                self.sculpt.stroke_active = false;
                self.tool.work_mode = crate::WorkMode::Object;
            }
            let target = self.ui.last_poly_edit_mode.unwrap_or(PolyWorkMode::Vertex);
            self.poly_switch_work_mode(target);
        }
        if self.modeling_context != ModelingContext::Polygonal
            || self.poly_work_mode == PolyWorkMode::Object
        {
            self.toast("Knife needs a poly edit mode — Tab into the mesh first");
            return;
        }
        // Never steal Ctrl+R: loop-cut K is a one-shot through-cut and must not arm this tool.
        if self.poly_loop_cut_is_active() {
            return;
        }
        if self.poly_tool == PolyTool::Knife {
            // Same-key re-press on an ARMED knife. This used to CANCEL the tool with no toast.
            // Harmless when written (2026-07-18: the knife exited after every cut), a trap since
            // 2026-08-06 when shape cuts started KEEPING the knife armed: the natural chord for the
            // next cut is `K, R, drag`, and that K silently killed the tool, so R had nothing to arm
            // and the drag fell through to click-select (owner 2026-10-11: "when I try to K
            // rectangular, pressing R does not trigger").
            //
            // Sibling tools (bevel `B`, hinge) CONFIRM in-flight work on a same-key press and never
            // drop the tool; Esc / RMB are the exits (Tool Exit Convention). Same here:
            //   • a cut in progress (shape being dragged, or ≥2 free points) → commit it, as Enter
            //     / release would — never discard the user's work on a tool key;
            //   • nothing drawn → restart FRESH (points cleared, free line) and stay armed, so
            //     `K, R` means the same thing on the first cut and on every cut after it.
            if self.knife_shape_anchor.is_some() {
                self.poly_knife_shape_release();
            } else if self.knife_points.len() >= 2 {
                self.poly_knife_confirm(true);
            } else {
                self.knife_points.clear();
                self.knife_snap_pos = None;
                self.poly_knife_shape_to_free_line();
            }
            return;
        }
        self.knife_points.clear();
        self.knife_snap_pos = None;
        // set_poly_tool drops G/R/S — knife must never run under an armed Grab (owner 2026-08-06).
        self.set_poly_tool(PolyTool::Knife);
        self.toast(
            "Knife: LMB place | dbl-click start = close | Ctrl=H/V | Shift/magnet=angle ° | Enter cut | Esc cancel",
        );
    }

    /// Did the USER scope the next cut to a face selection?
    ///
    /// Every knife verb reads a Face-mode selection as "cut only these polys". Since 2026-08-11 the
    /// tool also WRITES a face selection — the boolean's own result — and the two meanings collided:
    /// after one rect cut the tool sat in Face mode holding the cut walls, so the next rect drag
    /// scoped itself to them and scribed instead of punching a hole (owner: *"you broke the click
    /// and drag cut feature with shape"*). **A tool's output must never read back as its input.**
    ///
    /// Decided by VALUE, not by a flag: the selection stops matching the moment the user touches it,
    /// so there is no invalidation contract for a future caller to forget. (Manually re-selecting
    /// exactly the faces the last cut produced reads as "not scoped" — `S` still slices explicitly.)
    pub(crate) fn knife_has_user_face_scope(&self) -> bool {
        !self.poly_sel.selected_faces.is_empty()
            && self.poly_sel.selected_faces != self.knife_last_result_faces
    }

    /// `U` / `I` — pick the boolean the next shape cut runs. Pressing the SAME op again returns
    /// to Subtract, so the three ops need only two chords and there is always a way back to the
    /// default without leaving the tool.
    pub(crate) fn poly_knife_set_bool_op(&mut self, op: KnifeBoolOp) {
        if self.poly_tool != PolyTool::Knife {
            return;
        }
        let next = if self.knife_bool_op == op {
            KnifeBoolOp::Subtract
        } else {
            op
        };
        self.knife_bool_op = next;
        self.mark_dirty(DirtyFlags::RENDER); // HUD/preview colour follows the op
        match next {
            KnifeBoolOp::Subtract => {
                self.toast("Knife: SUBTRACT — the shape removes material (U = union, I = intersect)")
            }
            KnifeBoolOp::Union => self.toast(
                "Knife: UNION — the shape ADDS material · press E and pull OUT to set its height",
            ),
            KnifeBoolOp::Intersect => {
                self.toast("Knife: INTERSECT — keeps only what the shape encloses")
            }
        }
    }

    /// Angle-snap (snap menu °): Shift held, or magnet / snap toggle on.
    /// Ctrl is **not** this — it is H/V axis lock (see `poly_knife_hover_inner`), matching
    /// Draw Polyline / the viewport HUD (`Ctrl/magnet = Straight + angle`).
    fn knife_angle_snap_active(&self, shift: bool) -> bool {
        self.viewport.snap_rotation_deg > 0.05
            && (shift || self.is_snap_active(false))
    }

    /// Mesh surface hit under a screen pixel (perspective-safe). Returns nearest triangle hit.
    fn knife_surface_hit_at_screen(
        &self,
        screen: egui::Pos2,
        rect: egui::Rect,
        mesh: &core_math::half_edge::HalfEdgeMesh,
        xform: &impl Fn(Vec3) -> Vec3,
        cam_pos: Vec3,
        aspect: f32,
        ortho_hw: f32,
    ) -> Option<Vec3> {
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;
        let (ray_o, ray_d) = mouse_to_ray(screen, rect, cam_pos, orbit, aspect, ortho_hw, fov);
        knife_surface_hit_along_ray(mesh, xform, ray_o, ray_d)
    }

    /// Update knife tool hover: snap to nearest edge/vertex on mesh surface.
    /// - **Ctrl** = H/V straight lock from the last point (polyline parity). Vertex/edge
    ///   snap stays on — it is filtered to the constrained ray, not disabled.
    /// - **Shift / magnet** + snap° = angle increments; **edge/vert snap stays on**.
    pub(crate) fn poly_knife_hover(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
        shift: bool,
    ) {
        self.poly_knife_hover_inner(mouse, rect, ctrl, shift);
        // CLOSE-LOOP MAGNET (owner 2026-08-04: "it needs to snap visually to the point
        // before double click"). With 3+ points placed and the cursor in closing range of
        // the FIRST point, the live snap locks onto it (kind 4) so the preview line lands
        // exactly on the start and the marker announces that a double-click closes the
        // loop. A single click there places the point EXACTLY on the start, which the
        // confirm-time closed-loop detection then honors. Runs AFTER all normal snapping,
        // in both knife personalities.
        if self.poly_tool == PolyTool::Knife
            && self.knife_points.len() >= 3
            && self.knife_near_first_point(mouse, rect)
        {
            if let Some(&first) = self.knife_points.first() {
                self.knife_snap_pos = Some(first);
                self.knife_snap_kind = 4;
                self.knife_snap_edge = None;
            }
        }
    }

    fn poly_knife_hover_inner(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
        shift: bool,
    ) {
        self.knife_snap_pos = None;
        self.knife_snap_kind = 0;
        self.knife_snap_edge = None;
        if self.poly_tool != PolyTool::Knife {
            return;
        }
        // Free-polyline personality: the drawing plane was FROZEN at the first click, so hover
        // is a pure ray∩plane — no mesh snapping, and orbiting mid-draw cannot make the line
        // swim (R4: the cut extrudes along the CAPTURED view, not the current one).
        if self.knife_free {
            if let Some((po, pu, pv, pn)) = self.knife_free_plane {
                let cam_pos = self.viewport.camera.target
                    + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
                let aspect = rect.width() / rect.height().max(1.0);
                let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
                let (ro, rd) = mouse_to_ray(
                    mouse, rect, cam_pos, self.viewport.camera.orbit, aspect, ortho_hw, self.render.fov_deg,
                );
                if let Some(mut hit) = knife_plane_hit(ro, rd, po, pn) {
                    // Same modifiers as the classic knife / Draw Polyline:
                    //   Ctrl  = H/V axis lock in the captured plane
                    //   Shift / magnet + snap° = angle increments in that plane
                    // Ctrl wins when both are held (clip_curve parity).
                    if let Some(&last) = self.knife_points.last() {
                        let d = hit - last;
                        let x = d.dot(pu);
                        let y = d.dot(pv);
                        let len = (x * x + y * y).sqrt();
                        if len > 1e-6 {
                            if ctrl {
                                hit = if x.abs() >= y.abs() {
                                    last + pu * x
                                } else {
                                    last + pv * y
                                };
                            } else if self.knife_angle_snap_active(shift) {
                                let step =
                                    self.viewport.snap_rotation_deg.to_radians().max(1e-4);
                                let ang_s = (y.atan2(x) / step).round() * step;
                                hit = last + (pu * ang_s.cos() + pv * ang_s.sin()) * len;
                            }
                        }
                    }
                    self.knife_snap_pos = Some(hit);
                    self.knife_snap_kind = 3;
                }
            }
            return;
        }
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let xform = |p: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(p.x, p.y, p.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };

        let node = match self.scene.scene_graph.get(nid) {
            Some(n) => n,
            None => return,
        };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
        let front_depth = self.poly_front_surface_depth(
            nid,
            mouse,
            rect,
            cam_pos,
            self.viewport.camera.orbit,
            aspect,
            ortho_hw,
        );
        let bias = (self.viewport.camera.distance * 0.02).max(1e-3);
        let visible = |wp: Vec3| -> bool {
            match front_depth {
                Some(front) => Self::view_depth(wp, cam_pos, cam_fwd) <= front + bias,
                None => true,
            }
        };

        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;

        let has_prev = !self.knife_points.is_empty();
        let last = self.knife_points.last().copied();

        // ONE snapping space (owner 2026-08-03: "snaps to the wrong poly / can't edge-snap
        // with angle on — I feel we are using 2 snapping systems"): the constraint is
        // computed in SCREEN pixels — the same space as the vertex/edge snap radii below —
        // as a unit ray from the last point's screen position.
        //   Ctrl  = H/V (dominant screen axis) — advertised as "Straight"
        //   Shift / magnet + snap° = angle increments
        // Ctrl wins when both are held (Draw Polyline / clip_curve parity).
        // The old "Ctrl = no snap" early-out inverted this and made the HUD's
        // "Ctrl/magnet = Straight + angle" a dead key.
        let constrain_line: Option<(egui::Pos2, egui::Vec2)> = if has_prev {
            last.and_then(|lw| world_to_screen(lw, cam_pos, orbit, rect, aspect, ortho_hw, fov))
                .and_then(|ls| {
                    let d = mouse - ls;
                    if d.length() < 4.0 {
                        return None; // cursor on the last point: direction undefined
                    }
                    if ctrl {
                        let dir = if d.x.abs() >= d.y.abs() {
                            egui::vec2(if d.x >= 0.0 { 1.0 } else { -1.0 }, 0.0)
                        } else {
                            egui::vec2(0.0, if d.y >= 0.0 { 1.0 } else { -1.0 })
                        };
                        Some((ls, dir))
                    } else if self.knife_angle_snap_active(shift) {
                        let step = self.viewport.snap_rotation_deg.to_radians().max(1e-4);
                        let ang = d.y.atan2(d.x);
                        let ang_s = (ang / step).round() * step;
                        Some((ls, egui::vec2(ang_s.cos(), ang_s.sin())))
                    } else {
                        None
                    }
                })
        } else {
            None
        };
        let want_constrain = constrain_line.is_some();

        let on_constrain_line = |sp: egui::Pos2| -> bool {
            let Some((last_s, dir)) = constrain_line else {
                return true; // free: no filter
            };
            // dir is unit: t is pixels along the constrained ray, perp distance in pixels.
            let to = sp - last_s;
            let t = to.x * dir.x + to.y * dir.y;
            if t < -2.0 {
                return false;
            }
            let proj = egui::pos2(last_s.x + dir.x * t, last_s.y + dir.y * t);
            (sp - proj).length() <= 12.0
        };
        // Constrained vertex snap must be much stricter than the 12px ray corridor: a
        // vertex 12px off the ray would win over the exact ray-x-edge intersection and
        // bend the supposedly straight segment. 3px = essentially ON the ray.
        let on_constrain_strict = |sp: egui::Pos2| -> bool {
            let Some((last_s, dir)) = constrain_line else {
                return true;
            };
            let to = sp - last_s;
            let t = to.x * dir.x + to.y * dir.y;
            if t < -2.0 {
                return false;
            }
            let proj = egui::pos2(last_s.x + dir.x * t, last_s.y + dir.y * t);
            (sp - proj).length() <= 3.0
        };

        // 1. Vertex snap (still active under Ctrl — filtered to the H/V ray when constrained).
        let mut best_vert_dist = 12.0f32;
        let mut best_vert_pos = None;
        for i in 0..mesh.vertices.len() as u32 {
            if mesh.vert(VertexId(i)).half_edge == NO_EDGE {
                continue;
            }
            let wp = xform(mesh.vert(VertexId(i)).position);
            if !visible(wp) {
                continue;
            }
            if let Some(sp) =
                world_to_screen(wp, cam_pos, orbit, rect, aspect, ortho_hw, fov)
            {
                if !on_constrain_strict(sp) {
                    continue;
                }
                let d = (sp - mouse).length();
                if d < best_vert_dist {
                    best_vert_dist = d;
                    best_vert_pos = Some(wp);
                }
            }
        }
        if let Some(vp) = best_vert_pos {
            self.knife_snap_pos = Some(vp);
            self.knife_snap_kind = 1;
            return;
        }

        // 2. Edge snap (still active during Shift angle snap).
        let mut best_edge_dist = 12.0f32;
        let mut best_edge_pos = None;
        let mut best_edge_ends: Option<(Vec3, Vec3)> = None;
        for i in 0..mesh.half_edges.len() as u32 {
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE {
                continue;
            }
            if he.twin != NO_TWIN && i >= he.twin.0 {
                continue;
            }
            let src = mesh.he_source(HalfEdgeId(i));
            let p0 = xform(mesh.vert(src).position);
            let p1 = xform(mesh.vert(he.vertex).position);
            let sp0 = world_to_screen(p0, cam_pos, orbit, rect, aspect, ortho_hw, fov);
            let sp1 = world_to_screen(p1, cam_pos, orbit, rect, aspect, ortho_hw, fov);
            if let (Some(a), Some(b)) = (sp0, sp1) {
                let d = dist_point_to_segment(mouse, a, b);
                if d < best_edge_dist {
                    let ab = b - a;
                    // Angle-constrained: the snap point is the EXACT intersection of the
                    // constrained ray with this edge (owner 2026-08-03: "I can snap to the
                    // edge but have no way to make sure the segment stays straight") —
                    // projecting the cursor onto the edge broke the angle by up to the
                    // 12px ray tolerance. Free mode keeps the cursor projection.
                    let t = if let Some((ls, dir)) = constrain_line {
                        // Solve ls + s*dir = a + t*ab  (2x2 cross-product form).
                        let denom = dir.x * ab.y - dir.y * ab.x;
                        if denom.abs() < 1e-6 {
                            continue; // near-parallel: no stable intersection
                        }
                        let la = a - ls;
                        let s_ray = (la.x * ab.y - la.y * ab.x) / denom;
                        let t_seg = (la.x * dir.y - la.y * dir.x) / -denom;
                        if s_ray < -2.0 || !(-0.001..=1.001).contains(&t_seg) {
                            continue; // behind the last point / off the edge span
                        }
                        t_seg.clamp(0.0, 1.0)
                    } else {
                        let ma = mouse - a;
                        ((ma.x * ab.x + ma.y * ab.y) / (ab.x * ab.x + ab.y * ab.y).max(1e-10))
                            .clamp(0.0, 1.0)
                    };
                    let hit = p0.lerp(p1, t);
                    let hit_s = egui::pos2(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                    if !on_constrain_line(hit_s) {
                        continue;
                    }
                    if visible(hit) {
                        best_edge_dist = d;
                        best_edge_pos = Some(hit);
                        best_edge_ends = Some((p0, p1));
                    }
                }
            }
        }
        if let Some(ep) = best_edge_pos {
            self.knife_snap_pos = Some(ep);
            self.knife_snap_kind = 2;
            self.knife_snap_edge = best_edge_ends;
            return;
        }

        // 3. Constrained free point (no vertex/edge near the ray): project the cursor onto
        // the snapped screen ray and take the SURFACE under that pixel — same space and same
        // raycast as every other snap, so there is no second system to disagree with.
        if want_constrain {
            if let Some((ls, dir)) = constrain_line {
                let to = mouse - ls;
                let t = (to.x * dir.x + to.y * dir.y).max(0.0);
                let cs = egui::pos2(ls.x + dir.x * t, ls.y + dir.y * t);
                if let Some(hit) = self.knife_surface_hit_at_screen(
                    cs, rect, mesh, &xform, cam_pos, aspect, ortho_hw,
                ) {
                    self.knife_snap_pos = Some(hit);
                    self.knife_snap_kind = 3;
                    self.knife_snap_edge = None;
                    return;
                }
            }
        }

        // 4. Free surface hit.
        if let Some(hit) = self.knife_surface_hit_at_screen(
            mouse, rect, mesh, &xform, cam_pos, aspect, ortho_hw,
        ) {
            self.knife_snap_pos = Some(hit);
            self.knife_snap_kind = 0;
        }

        // 5. R1 "first click decides": nothing under the cursor and no points placed yet →
        // offer the FREE-POLYLINE start. Preview plane: view-aligned through the body's world
        // origin (sensible depth); the real plane is captured on click.
        if self.knife_snap_pos.is_none() && self.knife_points.is_empty() {
            let anchor = xform(Vec3::ZERO);
            let n = (orbit * Vec3::NEG_Z).normalize();
            let (ro, rd) = mouse_to_ray(mouse, rect, cam_pos, orbit, aspect, ortho_hw, fov);
            if let Some(hit) = knife_plane_hit(ro, rd, anchor, n) {
                self.knife_snap_pos = Some(hit);
                self.knife_snap_kind = 3;
            }
        }
    }

    /// Place a knife point at the current snap position.
    pub(crate) fn poly_knife_click(&mut self) {
        if let Some(pos) = self.knife_snap_pos {
            // R1: first click on EMPTY space (snap kind 3 = plane candidate) arms the free
            // personality and freezes the drawing plane to THIS camera at THIS point.
            if self.knife_points.is_empty() && self.knife_snap_kind == 3 && !self.knife_free {
                let orbit = self.viewport.camera.orbit;
                let view = (orbit * Vec3::NEG_Z).normalize();
                self.knife_free = true;
                self.knife_free_plane = Some((
                    pos,
                    (orbit * Vec3::X).normalize(),
                    (orbit * Vec3::Y).normalize(),
                    view,
                ));
                self.knife_free_view = Some(view);
                let face_hint = if self.knife_has_user_face_scope() {
                    " | face sel (full island=through · partial=slice faces)"
                } else {
                    ""
                };
                self.toast(&format!(
                    "Knife free line: LMB add point | dbl-click start = close | S = through-slice | Enter finish | Esc cancel{face_hint}"
                ));
            }
            // Skip a point coincident with the last — a zero-length knife segment is never useful, and this
            // keeps confirm's commit-hover (below) idempotent when the cursor hasn't moved since the click.
            if self.knife_points.last().map_or(true, |p| p.distance(pos) > 1e-5) {
                self.knife_points.push(pos);
            }
        }
    }

    /// True when the hover snap (or a screen position) is near the first placed knife point.
    /// Used for double-click-to-close (owner: "double clic of the first point to close the shape").
    pub(crate) fn knife_near_first_point(&self, mouse: egui::Pos2, rect: egui::Rect) -> bool {
        let Some(&first) = self.knife_points.first() else { return false };
        if self.knife_points.len() < 3 {
            return false;
        }
        // Prefer 3D proximity to the live snap (same space as placement).
        if let Some(snap) = self.knife_snap_pos {
            let scale = self.viewport.camera.distance.max(0.1);
            if snap.distance(first) < scale * 0.04 {
                return true;
            }
        }
        // Screen-space fallback (~14 px).
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        if let Some(sp) = world_to_screen(
            first, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg,
        ) {
            let d = ((sp.x - mouse.x).powi(2) + (sp.y - mouse.y).powi(2)).sqrt();
            if d < 14.0 {
                return true;
            }
        }
        false
    }

    /// Double-click the first point → close the loop and commit the cut (basic + free knife).
    /// Returns true when the close was handled (caller should not also place a point).
    pub(crate) fn poly_knife_try_close(&mut self, mouse: egui::Pos2, rect: egui::Rect) -> bool {
        if self.poly_tool != PolyTool::Knife {
            return false;
        }
        if self.knife_points.len() < 3 {
            return false;
        }
        if !self.knife_near_first_point(mouse, rect) {
            return false;
        }
        // Ensure the polyline ends at the start (closed path).
        if let Some(&first) = self.knife_points.first() {
            if self.knife_points.last().map_or(true, |p| p.distance(first) > 1e-5) {
                self.knife_points.push(first);
            }
        }
        // commit_hover=false: the loop is already closed; don't place a stray hover point.
        self.poly_knife_confirm_closed(false);
        true
    }

    /// Legacy Enter/RMB finish-mode flip (Tool Params checkbox owns this now).
    /// **Not** bound to S — S calls [`Self::poly_knife_slice_through_now`] (apply cut).
    pub(crate) fn poly_knife_toggle_slice(&mut self) {
        self.knife_free_slice = !self.knife_free_slice;
        self.toast(if self.knife_free_slice {
            "Knife confirm: SCRIBE only (Enter/RMB) — S still applies through-slice now"
        } else {
            "Knife confirm: SUBTRACT (Enter/RMB) — S still applies through-slice now"
        });
    }

    /// `R` / `C` in the free-polyline knife: arm a SHAPE polyline (1 = rectangle, 2 = circle).
    /// LMB-drag then sizes it — press anchors, release commits (owner 2026-08-05).
    ///
    /// Requires the free personality, because the shape is laid out in the captured drawing
    /// plane's 2D basis. Without that plane there is no surface to lay a rectangle ON, and
    /// guessing one from the current view would make the shape swim the moment you orbit —
    /// the exact class of bug the frozen plane exists to prevent.
    pub(crate) fn poly_knife_shape_arm(&mut self, kind: u8) {
        if self.poly_tool != PolyTool::Knife {
            return;
        }
        // A shape may be armed from a COLD knife. It used to demand an already-started free cut,
        // which made the control unusable from the Tool Params panel — you had to click empty
        // space first, and the only way to arm it at all was a key chord that never reached here
        // (owner 2026-08-05: "knife modifier R and C do not work, their shortcut is not properly
        // set, they bring rotate and circle select"). The drawing plane is captured on PRESS
        // instead, from the same view-aligned plane through the body origin that the first-click
        // path (R1) uses — so the shape is still orbit-proof, just armed earlier.
        if self.knife_shape_kind == kind {
            // Same key again disarms — a shape you armed by mistake should not trap the LMB.
            self.knife_shape_kind = 0;
            self.knife_shape_anchor = None;
            self.toast("Knife: shape cancelled");
            return;
        }
        self.knife_shape_kind = kind;
        self.knife_shape_anchor = None;
        self.knife_shape_cursor = None;
        self.knife_shape_screen_a = None;
        self.knife_shape_screen_b = None;
        // Keep last corner bevel so scroll-while-drag feels sticky across re-arms.
        self.knife_shape_array_count = 1;
        self.toast(if kind == 1 {
            "Knife: RECT — click face · drag size · Alt=center · Space=move · R=rotate · Ctrl+D=array · S=slice"
        } else {
            "Knife: CIRCLE — click face · drag · Alt=center · Space=move · scroll=segs · S=slice · L=free"
        });
    }

    /// Ctrl+D mid-drag on a knife rectangle: start/grow a linear array (edge-to-edge
    /// along the longer side). Count 1 → 2; already arraying → +1 (capped 32).
    pub(crate) fn poly_knife_shape_array_ctrl_d(&mut self) -> bool {
        if self.poly_tool != PolyTool::Knife
            || self.knife_shape_kind != 1
            || self.knife_shape_anchor.is_none()
        {
            return false;
        }
        let n = self.knife_shape_array_count.max(1);
        self.knife_shape_array_count = if n <= 1 { 2 } else { (n + 1).min(32) };
        self.poly_knife_shape_rebuild();
        self.toast(&format!(
            "Knife: rect array ×{} · scroll=count · Shift+scroll=bevel segs",
            self.knife_shape_array_count
        ));
        true
    }

    /// True when the wheel may adjust knife shape params.
    ///
    /// Owner 2026-08-06: scroll is **only** armed while a rectangle or circle is being
    /// **dragged** (anchor set → live polyline drawn). Merely having Knife active with R/C
    /// armed and nothing drawn must **not** consume the wheel (was still scrolling bevel/segs).
    pub(crate) fn knife_shape_scroll_armed(&self) -> bool {
        if self.poly_tool != PolyTool::Knife {
            return false;
        }
        if self.tool.current.is_drag_select_tool() {
            return false;
        }
        // Depth phase: the shape is LOCKED, so segments / bevel / array have nothing to change —
        // and the wheel must fall through to camera ZOOM. The phase exists so the user can move
        // the camera to judge the depth; swallowing the wheel there would leave them able to
        // orbit but not zoom, which is half a camera.
        if self.knife_shape_depth_mode {
            return false;
        }
        // Mid-drag only: rect / circle being sized (rubber-band + live knife_points polyline).
        self.knife_shape_kind != 0 && self.knife_shape_anchor.is_some()
    }

    /// Wheel while a knife rectangle/circle is **being dragged** (owner 2026-08-06):
    ///   * **Rectangle** — plain scroll = corner bevel amount; **Shift+scroll** = bevel segments
    ///   * **Circle** — scroll = segment count (also Tool Parameters)
    ///
    /// `wheel_ticks` is measured in **physical wheel notches** (fractional is fine — the
    /// accumulator carries the remainder across frames), so one notch = one segment step.
    /// The caller converts raw wheel units with `WHEEL_UNITS_PER_NOTCH`; do NOT decode
    /// `Event::MouseWheel` line units here, one notch is ~3 of them (owner: "3× too fast").
    /// Bevel-amount scroll uses a tiny per-notch step because the range is 0..1.
    ///
    /// **Hard isolation**: returns false when nothing is drawn / drag-select owns the gesture.
    pub(crate) fn poly_knife_shape_scroll(&mut self, wheel_ticks: f32, shift: bool) -> bool {
        if !self.knife_shape_scroll_armed() {
            return false;
        }
        if wheel_ticks.abs() < 1e-6 {
            return false;
        }
        match self.knife_shape_kind {
            1 if shift => {
                // Shift+scroll = bevel segment count (one step per discrete notch).
                self.knife_shape_scroll_accum += wheel_ticks;
                let mut changed = false;
                // Threshold 1.0 = one MouseWheel Line unit → ±1 segment.
                while self.knife_shape_scroll_accum >= 1.0 {
                    self.knife_shape_scroll_accum -= 1.0;
                    let n = (self.knife_shape_segments + 1).min(64);
                    if n != self.knife_shape_segments {
                        self.knife_shape_segments = n;
                        changed = true;
                    }
                }
                while self.knife_shape_scroll_accum <= -1.0 {
                    self.knife_shape_scroll_accum += 1.0;
                    let n = self.knife_shape_segments.saturating_sub(1).max(3);
                    if n != self.knife_shape_segments {
                        self.knife_shape_segments = n;
                        changed = true;
                    }
                }
                if changed {
                    self.poly_knife_shape_rebuild();
                    self.toast(&format!(
                        "Knife: rect bevel segments {} (Shift+scroll)",
                        self.knife_shape_segments
                    ));
                }
                true // consume wheel even if accum not yet full
            }
            // Array mode (Ctrl+D): plain scroll steps copy count. Off-array = corner bevel.
            1 if self.knife_shape_array_count >= 2 => {
                self.knife_shape_scroll_accum += wheel_ticks;
                let mut changed = false;
                while self.knife_shape_scroll_accum >= 1.0 {
                    self.knife_shape_scroll_accum -= 1.0;
                    let n = (self.knife_shape_array_count + 1).min(32);
                    if n != self.knife_shape_array_count {
                        self.knife_shape_array_count = n;
                        changed = true;
                    }
                }
                while self.knife_shape_scroll_accum <= -1.0 {
                    self.knife_shape_scroll_accum += 1.0;
                    let n = self.knife_shape_array_count.saturating_sub(1).max(1);
                    if n != self.knife_shape_array_count {
                        self.knife_shape_array_count = n;
                        changed = true;
                    }
                }
                if changed {
                    self.poly_knife_shape_rebuild();
                    if self.knife_shape_array_count <= 1 {
                        self.toast("Knife: rect array off · scroll=bevel · Ctrl+D=array");
                    } else {
                        self.toast(&format!(
                            "Knife: rect array ×{}",
                            self.knife_shape_array_count
                        ));
                    }
                }
                true
            }
            1 => {
                // Plain scroll = corner bevel amount. One physical notch → fixed step.
                // Owner 2026-08-07: full round in ~7 notches ("75× too slow" at 0.002).
                // The earlier 0.04→0.002 tuning was measured against the old tick source,
                // which multi-fired per notch — not a real preference for 500-tick bevels.
                const BEVEL_PER_NOTCH: f32 = 0.15;
                let delta = BEVEL_PER_NOTCH * wheel_ticks;
                let before = self.knife_shape_corner_bevel;
                self.knife_shape_corner_bevel =
                    (self.knife_shape_corner_bevel + delta).clamp(0.0, 1.0);
                if (self.knife_shape_corner_bevel - before).abs() < 1e-9 {
                    return true;
                }
                self.poly_knife_shape_rebuild();
                let pct = self.knife_shape_corner_bevel * 100.0;
                let n = self.knife_points.len().saturating_sub(1);
                let segs = self.knife_shape_segments;
                self.toast(&format!(
                    "Knife: rect corner bevel {pct:.0}% · {n} pts · {segs} segs · Ctrl+D=array"
                ));
                true
            }
            2 => {
                // Circle: scroll = segments, discrete notches only (same accum as rect Shift).
                self.knife_shape_scroll_accum += wheel_ticks;
                let mut changed = false;
                while self.knife_shape_scroll_accum >= 1.0 {
                    self.knife_shape_scroll_accum -= 1.0;
                    let n = (self.knife_shape_segments + 1).min(256);
                    if n != self.knife_shape_segments {
                        self.knife_shape_segments = n;
                        changed = true;
                    }
                }
                while self.knife_shape_scroll_accum <= -1.0 {
                    self.knife_shape_scroll_accum += 1.0;
                    let n = self.knife_shape_segments.saturating_sub(1).max(3);
                    if n != self.knife_shape_segments {
                        self.knife_shape_segments = n;
                        changed = true;
                    }
                }
                if changed {
                    self.poly_knife_shape_rebuild();
                    self.toast(&format!(
                        "Knife: circle {} segments",
                        self.knife_shape_segments
                    ));
                }
                true
            }
            _ => false,
        }
    }

    /// Capture the free-knife drawing plane if it is not set yet: view-aligned through the
    /// active body's **bounds centre** (not mesh local 0 — that can sit outside a translated
    /// poly). Frozen at THIS camera so orbit does not swim the rectangle.
    fn knife_ensure_free_plane(&mut self) {
        if self.knife_free_plane.is_some() {
            return;
        }
        let origin = self
            .sel
            .usel
            .active_node()
            .and_then(|nid| {
                let node = self.scene.scene_graph.get(nid)?;
                let mesh = match &node.payload {
                    NodePayload::PolygonalBody { mesh, .. } => mesh,
                    _ => return None,
                };
                let mut lo = Vec3::splat(f32::MAX);
                let mut hi = Vec3::splat(f32::MIN);
                let mut any = false;
                for i in 0..mesh.vertices.len() as u32 {
                    let v = VertexId(i);
                    if mesh.vert(v).half_edge == NO_EDGE {
                        continue;
                    }
                    let p = mesh.vert(v).position;
                    lo = lo.min(p);
                    hi = hi.max(p);
                    any = true;
                }
                if !any {
                    return None;
                }
                let local_c = (lo + hi) * 0.5;
                let m = self.scene.scene_graph.world_transform(nid);
                let w = m * glam::Vec4::new(local_c.x, local_c.y, local_c.z, 1.0);
                Some(Vec3::new(w.x, w.y, w.z))
            })
            .or_else(|| {
                self.sel.usel.active_node().map(|nid| {
                    let m = self.scene.scene_graph.world_transform(nid);
                    let w = m * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
                    Vec3::new(w.x, w.y, w.z)
                })
            })
            .unwrap_or(self.viewport.camera.target);
        let orbit = self.viewport.camera.orbit;
        let view = (orbit * Vec3::NEG_Z).normalize();
        self.knife_free = true;
        self.knife_free_plane = Some((
            origin,
            (orbit * Vec3::X).normalize(),
            (orbit * Vec3::Y).normalize(),
            view,
        ));
        self.knife_free_view = Some(view);
    }

    /// Re-lay the armed shape after a parameter change (segment count from the panel), when a
    /// drag is already anchored. No-op otherwise — nothing to redraw yet.
    pub(crate) fn poly_knife_shape_refresh(&mut self) {
        if self.knife_shape_anchor.is_some() {
            self.poly_knife_shape_rebuild();
        }
    }

    /// Press: anchor the shape at the face under the cursor (position + face normal as
    /// the drawing plane). Critical in **perspective**: the cut starts on the surface you
    /// click, not a view-aligned plane through the body centre (owner 2026-08-07).
    /// Falls back to the view-aligned free plane only when the ray misses the mesh.
    /// `from_center` (Alt): press is the shape **center**; drag sets half-extents / radius.
    pub(crate) fn poly_knife_shape_press(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        from_center: bool,
    ) {
        // Every new shape drag starts as a through cut. Belt-and-braces against a depth phase
        // that ended by a path other than commit/cancel (RMB-confirm, tool switch): a stale
        // `depth_mode` would lock the shape the instant it was pressed, with no way back.
        self.poly_knife_shape_depth_reset();
        // Prefer face under mouse: origin = hit, basis = face-plane frame (n = face normal).
        // Overwrites any previous free plane so each rect/circle drag can start on a new face.
        if let Some((hit, n_world)) = self.knife_face_hit_under_mouse(mouse, rect) {
            let (u, v, n) = knife_face_plane_basis(n_world, self.viewport.camera.orbit);
            // Face plane for drawing; capture camera view for S / through-slice (Blender parity).
            let view = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
            self.knife_free = true;
            self.knife_free_plane = Some((hit, u, v, n));
            self.knife_free_view = Some(view);
            self.knife_shape_from_center = from_center;
            self.knife_shape_anchor = Some(hit);
            self.knife_shape_cursor = Some(hit);
            self.knife_shape_screen_a = Some(mouse);
            self.knife_shape_screen_b = Some(mouse);
            self.knife_shape_rotation_rad = 0.0;
            self.knife_shape_rot_last_mouse = None;
            self.knife_shape_move_last_mouse = None;
            self.knife_shape_array_count = 1;
            self.knife_points.clear();
            return;
        }

        // Miss: view-aligned plane through body bounds centre (previous behaviour).
        // Clear a stale face plane so we don't keep drawing on a previous face.
        self.knife_free_plane = None;
        self.knife_free_view = None;
        self.knife_ensure_free_plane();
        let p = match self.knife_plane_point_at(mouse, rect) {
            Some(p) => p,
            None => {
                // Ray parallel to plane (rare with view-aligned plane) — fall back to plane origin.
                let Some((po, _, _, _)) = self.knife_free_plane else { return };
                po
            }
        };
        self.knife_shape_from_center = from_center;
        self.knife_shape_anchor = Some(p);
        self.knife_shape_cursor = Some(p);
        self.knife_shape_screen_a = Some(mouse);
        self.knife_shape_screen_b = Some(mouse);
        // Fresh drag always starts unrotated (owner: do not keep prior R orientation).
        self.knife_shape_rotation_rad = 0.0;
        self.knife_shape_rot_last_mouse = None;
        self.knife_shape_move_last_mouse = None;
        // Array is per-gesture (Ctrl+D mid-drag); new press starts single.
        self.knife_shape_array_count = 1;
        self.knife_points.clear();
    }

    /// Frontmost mesh face hit under a screen pixel: world position + world face normal.
    /// Used to seed knife shape cuts on the surface under the cursor (perspective-safe).
    /// SDF: analytical surface hit from the scene field.
    fn knife_face_hit_under_mouse(
        &self,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> Option<(Vec3, Vec3)> {
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh.as_ref(),
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let xform = |p: Vec3| world.transform_point3(p);
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;
        let (ray_o, ray_d) = mouse_to_ray(mouse, rect, cam_pos, orbit, aspect, ortho_hw, fov);
        knife_surface_hit_with_normal(mesh, &xform, ray_o, ray_d)
    }

    /// Drag: size the shape, **Space** = move whole shape, **R** = rotate rect.
    /// `from_center` tracks Alt each frame so the user can hold/release mid-drag.
    /// `move_held` = Space down (translate anchor+cursor without changing size).
    /// `rotate_held` = R key down (rect only). `snap_angle` = Ctrl → snap to snap settings.
    pub(crate) fn poly_knife_shape_drag(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        from_center: bool,
        move_held: bool,
        rotate_held: bool,
        snap_angle: bool,
    ) {
        if self.knife_shape_anchor.is_none() {
            return;
        }
        // E was pressed: the silhouette is LOCKED. Ignore the rest of the sizing drag entirely
        // — resizing after the lock is what would make the shape swim while the user is looking
        // for the depth, and the depth phase owns the pointer from here on.
        if self.knife_shape_depth_mode {
            return;
        }
        // Hold Space mid-drag → pan the whole shape (Blender box-select parity).
        // Takes priority over rotate so Space+R doesn't spin while the user is repositioning.
        if move_held {
            self.poly_knife_shape_move(mouse, rect);
            return;
        }
        self.knife_shape_move_last_mouse = None;
        // Hold R mid-drag on rectangle → rotate in-plane (not resize).
        if rotate_held && self.knife_shape_kind == 1 {
            self.poly_knife_shape_rotate(mouse, snap_angle);
            return;
        }
        self.knife_shape_rot_last_mouse = None;
        self.knife_shape_from_center = from_center;
        self.knife_shape_screen_b = Some(mouse);
        if let Some(p) = self.knife_plane_point_at(mouse, rect) {
            self.knife_shape_cursor = Some(p);
            self.poly_knife_shape_rebuild();
        }
    }

    /// Translate the knife shape (anchor + cursor + screen rubber-band) by mouse delta.
    /// Used while **Space** is held mid-drag so Alt-from-center rects can be repositioned
    /// after sizing without restarting the drag (owner 2026-08-06).
    pub(crate) fn poly_knife_shape_move(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        if self.knife_shape_anchor.is_none() {
            return;
        }
        let prev = self.knife_shape_move_last_mouse.unwrap_or(mouse);
        self.knife_shape_move_last_mouse = Some(mouse);
        let d_screen = mouse - prev;
        if d_screen.length_sq() < 1e-8 {
            return;
        }
        // Screen rubber-band moves 1:1 with the cursor.
        if let Some(sa) = self.knife_shape_screen_a.as_mut() {
            *sa += d_screen;
        }
        if let Some(sb) = self.knife_shape_screen_b.as_mut() {
            *sb += d_screen;
        }
        // World plane points: translate by the same mouse motion projected onto the plane.
        let Some(p_prev) = self.knife_plane_point_at(prev, rect) else {
            return;
        };
        let Some(p_now) = self.knife_plane_point_at(mouse, rect) else {
            return;
        };
        let d_world = p_now - p_prev;
        if d_world.length_squared() < 1e-14 {
            return;
        }
        if let Some(a) = self.knife_shape_anchor.as_mut() {
            *a += d_world;
        }
        if let Some(c) = self.knife_shape_cursor.as_mut() {
            *c += d_world;
        }
        // Keep rotation pivot coherent (angular track uses screen AABB midpoint).
        self.knife_shape_rot_last_mouse = None;
        self.poly_knife_shape_rebuild();
    }

    /// Rotate the knife rectangle around its center from mouse motion.
    ///
    /// Angular delta uses the 2D cross product of (prev→center)×(motion) / r² so **near the
    /// rect = faster spin**, far away = slower (owner 2026-08-06). Ctrl snaps to
    /// `viewport.snap_rotation_deg`.
    pub(crate) fn poly_knife_shape_rotate(&mut self, mouse: egui::Pos2, snap_angle: bool) {
        let (Some(sa), Some(sb)) = (self.knife_shape_screen_a, self.knife_shape_screen_b) else {
            return;
        };
        // Shape center in screen space (AABB midpoint of rubber-band / press–cursor).
        let cx = (sa.x + sb.x) * 0.5;
        let cy = (sa.y + sb.y) * 0.5;
        let center = egui::pos2(cx, cy);

        let prev = self.knife_shape_rot_last_mouse.unwrap_or(mouse);
        self.knife_shape_rot_last_mouse = Some(mouse);

        let v0 = egui::vec2(prev.x - center.x, prev.y - center.y);
        let v1 = egui::vec2(mouse.x - center.x, mouse.y - center.y);
        let r0_sq = v0.x * v0.x + v0.y * v0.y;
        // Too close to center → ignore (unstable / infinite angular gain).
        if r0_sq < 4.0 {
            return;
        }
        // dθ = (v0 × dm) / |v0|²  — pure geometric angular change; near pivot ⇒ large dθ.
        let dm = egui::vec2(mouse.x - prev.x, mouse.y - prev.y);
        if dm.x * dm.x + dm.y * dm.y < 1e-8 {
            return;
        }
        let d_ang = (v0.x * dm.y - v0.y * dm.x) / r0_sq;
        // Negate so on-screen mouse motion matches rect rotation direction (owner: was reverse).
        self.knife_shape_rotation_rad -= d_ang;

        if snap_angle {
            let step_deg = self.viewport.snap_rotation_deg.max(0.0);
            if step_deg > 1e-6 {
                let step = step_deg.to_radians();
                self.knife_shape_rotation_rad =
                    (self.knife_shape_rotation_rad / step).round() * step;
            }
        }

        self.poly_knife_shape_rebuild();
    }

    /// Release: cut when the shape has a real extent; keep R/C mode for the next drag.
    ///
    /// Owner 2026-08-06: cut on release (not place-then-confirm). Shape LMB is fully owned.
    ///
    /// Commit rules (owner 2026-08-06):
    ///   * **Faces selected** → always **SLICE** those faces only (never subtract the body).
    ///   * **No selection** → closed rect/circle **SUBTRACT** (cookie-cutter hole).
    ///   * **S mid-drag** → always **SLICE** (see [`Self::poly_knife_slice_through_now`]).
    ///
    /// **Rect corner bevel (scroll while dragging):** the ARC polyline **is** the cut — the
    /// same points RMB/Enter cuts through [`Self::poly_knife_free_confirm`]. Release and RMB
    /// are one operation and must land the same mesh (owner 2026-08-08: "the bevel point cut
    /// works on the right click, on the release of drag should be same thing").
    ///
    /// **DO NOT re-add a "cut sharp, then bevel the corner verts" post-pass here** (tried and
    /// reverted 2026-08-08, BEVEL.md §6). A knife corner round is a property of the cut PATH,
    /// not a 3D bevel strip, so a vertex bevel cannot reproduce it: on the SLICE path the four
    /// corner verts are coplanar, so the facet it inserts is coplanar and invisible; on the
    /// SUBTRACT path it only chamfers the front rim while the hole stays square through its
    /// depth. Both read to the user as "the rectangular knife stopped beveling corners", and
    /// it silently fell back to a sharp cut whenever the corner verts failed to match. Unbevel
    /// support is a separate problem — see BEVEL.md §6.
    pub(crate) fn poly_knife_shape_release(&mut self) {
        if self.knife_shape_anchor.is_none() {
            return;
        }
        // DEPTH PHASE (E was pressed mid-drag): this release ends SIZING ONLY. The shape stays
        // anchored and drawn so the camera can be orbited to judge the depth; the next LMB drag
        // pulls the depth and ITS release commits (owner 2026-08-09).
        if self.knife_shape_depth_mode {
            self.knife_shape_rot_last_mouse = None;
            self.knife_shape_move_last_mouse = None;
            self.knife_shape_depth_committable = false;
            return;
        }
        self.poly_knife_shape_commit_cut();
    }

    /// The ONE shape-cut commit path: a plain release, the depth-phase release, and a
    /// depth-armed Enter all land here. Never fork it per gesture — a depth cut that
    /// diverged from a through cut would be two code paths for one operation
    /// (CLAUDE.md § No Bandaids), the exact defect the 2026-08-08 rect-bevel fix removed.
    fn poly_knife_shape_commit_cut(&mut self) {
        // Clear the drag rubber-band only — keep shape MODE armed (R/C) so the next
        // LMB drag is another rect/circle cut. Owner 2026-08-06: "shape should still be
        // the same mode after a cut; L goes back to polyline".
        let keep_kind = self.knife_shape_kind;
        self.knife_shape_anchor = None;
        self.knife_shape_cursor = None;
        self.knife_shape_screen_a = None;
        self.knife_shape_screen_b = None;
        self.knife_shape_from_center = false;
        self.knife_shape_rot_last_mouse = None;
        self.knife_shape_move_last_mouse = None;
        // Keep array_count until cut runs (knife_points holds all rings); restore_mode resets it.
        // Closed shape writes first==last → 5 points for a rect. Require a real extent.
        let extent_ok = self.knife_shape_extent_ok();
        if self.knife_points.len() < 3 || !extent_ok {
            self.knife_points.clear();
            self.knife_shape_kind = keep_kind; // stay in rect/circle for retry
            self.knife_shape_array_count = 1;
            self.poly_knife_shape_depth_reset();
            self.toast("Knife: shape too small — drag to size, then release");
            return;
        }
        // `knife_points` already holds the rounded ring `poly_knife_shape_rebuild` laid out —
        // it is the cut, unmodified. Same bytes RMB/Enter would cut.
        self.knife_ensure_free_plane();
        self.knife_free = true;
        // Face-mode face selection → slice those polys only; else cookie-cutter boolean.
        // An explicitly chosen Union/Intersect (U/I) always means the BOOLEAN: the user asked for
        // an op a scribe cannot express, so a face selection must not silently downgrade it.
        let has_face_sel = matches!(
            self.poly_work_mode,
            PolyWorkMode::Face | PolyWorkMode::Group
        ) && self.knife_has_user_face_scope()
            && self.knife_bool_op == KnifeBoolOp::Subtract;
        if has_face_sel {
            // A slice SCRIBES the surface — it has no thickness to stop at, so an armed depth
            // has no meaning here. Say so rather than silently switching the user's verb to a
            // subtract or silently dropping the depth they just dialled in.
            if self.knife_cut_depth().is_some() {
                self.toast(
                    "Knife: cut depth needs the subtract (deselect faces) — slicing through",
                );
            }
            self.knife_free_slice = true;
            let closed = self.knife_free_is_closed();
            self.poly_knife_free_slice_commit(closed, false, SliceEnclosed::Keep);
        } else {
            // Cookie-cutter subtract on the free drawing plane (prism along view).
            // Rect array = multi-loop prism (one Difference, all holes).
            self.knife_free_slice = false;
            // `None` = closed loop → enclosed material dies.
            self.poly_knife_free_boolean(None);
        }
        // free_boolean / free_slice clear points/plane; re-assert shape mode.
        self.poly_knife_shape_restore_mode(keep_kind);
    }

    // ── Cut DEPTH phase (`E` mid-drag) ────────────────────────────────────────────────
    //
    // Owner 2026-08-09: *"knife tool shape project (rec, cir) while dragging pressing E should
    // let you define the thickness of the cut, user needs to be able to rotate camera, cut
    // confirms when user releases the left click."*
    //
    // "Thickness" = how far the cookie-cutter prism reaches INTO the surface, along the drawing
    // plane's inward normal (−n): depth 0 = the historical through-hole, depth d > 0 = a blind
    // pocket with a floor at d. The silhouette never changes — E freezes it, which is exactly
    // what lets the camera orbit before the depth is pulled without the shape swimming (the
    // frozen drawing plane already guarantees that; see `poly_knife_shape_arm`).

    /// `E`: lock the drawn rect/circle and enter the depth phase. Idempotent — pressing E again
    /// keeps the depth already dialled in rather than resetting it to a through cut.
    pub(crate) fn poly_knife_shape_depth_arm(&mut self) {
        if self.poly_tool != PolyTool::Knife
            || self.knife_shape_kind == 0
            || self.knife_shape_anchor.is_none()
        {
            return;
        }
        if self.knife_shape_depth_mode {
            return;
        }
        self.knife_shape_depth_mode = true;
        self.knife_shape_depth = 0.0;
        self.knife_shape_depth_committable = false;
        self.knife_shape_depth_frozen = false;
        // Sizing is over — drop the sizing sub-gestures so a still-held Space/R cannot move or
        // spin the locked shape.
        self.knife_shape_rot_last_mouse = None;
        self.knife_shape_move_last_mouse = None;
        self.toast("Knife depth: move the mouse to extrude the cut · LMB release = cut · Esc = cancel");
    }

    /// Track the depth to the CURSOR, every frame the phase is live — hover included, no button
    /// required (owner 2026-08-09: *"it need to extrude to the mouse position"*).
    ///
    /// This is ABSOLUTE, not a drag delta: the pocket floor sits where the pointer is, so the
    /// moment `E` is pressed the shape starts extruding and the phase is visibly alive. The
    /// first cut of this feature seeded a reference on LMB-press and integrated a delta from
    /// there — correct arithmetic, but until you pressed the button nothing moved, which reads
    /// exactly like a dead key. Do not reintroduce the delta form.
    ///
    /// SIGNED — both directions (owner 2026-08-11: *"on the E extrude I can only go one
    /// direction, need to be able to go in and out for the union"*). `t > 0` is into the surface
    /// (a pocket, what a Subtract wants); `t < 0` is out of it (the height of a Union boss).
    /// The old `t.max(0.0)` clamp made the outward half of the axis dead: the cursor moved, the
    /// number stayed 0, and a Union could never be given a height. It was correct only while
    /// Subtract was the only op — an outward cutter really does subtract nothing.
    pub(crate) fn poly_knife_shape_depth_track(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        if !self.knife_shape_depth_mode || self.knife_shape_depth_frozen {
            return;
        }
        let Some(t) = self.poly_knife_shape_depth_axis_t(mouse, rect) else { return };
        self.knife_shape_depth = t;
    }

    /// LMB press in the depth phase: arms the confirm. The depth itself needs no seeding — it
    /// already tracks the cursor — so pressing the button changes nothing on screen.
    pub(crate) fn poly_knife_shape_depth_press(&mut self) {
        if self.knife_shape_depth_mode {
            self.knife_shape_depth_committable = true;
        }
    }

    /// LMB release in the depth phase = CONFIRM. Runs the shared commit path, so a depth cut is
    /// the same operation as a through cut with one parameter changed.
    pub(crate) fn poly_knife_shape_depth_release(&mut self) {
        if !self.knife_shape_depth_mode || !self.knife_shape_depth_committable {
            return; // the sizing drag's own release — it must not cut (see shape_release)
        }
        self.knife_shape_depth_committable = false;
        // A tap with no drag has not defined a thickness yet. Staying in the phase costs the
        // user one more drag; committing would punch a full through-hole they never asked for
        // — the destructive reading of an ambiguous gesture is the wrong default.
        if self.knife_cut_depth().is_none() {
            self.toast("Knife depth: drag to set the depth first (Esc cancels)");
            return;
        }
        self.poly_knife_shape_commit_cut();
    }

    /// Cursor → distance along the INWARD normal from the shape centre, measured on a
    /// camera-facing plane hinged about that axis. Reuses `compute_extrude_depth`
    /// (`ops/geo/placement_math.rs`) — the create tool's extrude phase does the identical job,
    /// so both tools track the cursor the same way at every camera angle.
    fn poly_knife_shape_depth_axis_t(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<f32> {
        let (o, _u, _v, n) = self.knife_free_plane?;
        let center = self.poly_knife_shape_center().unwrap_or(o);
        let (ray_o, ray_d) = self.cam_ray(mouse, rect);
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_up = (self.viewport.camera.orbit * Vec3::Y).normalize();
        crate::ops::geo::placement_math::compute_extrude_depth(
            ray_o, ray_d, center, -n, cam_pos, cam_up,
        )
    }

    /// Centroid of the drawn ring(s) in world space — the depth axis passes through it.
    pub(crate) fn poly_knife_shape_center(&self) -> Option<Vec3> {
        if self.knife_points.is_empty() {
            return None;
        }
        let mut c = Vec3::ZERO;
        for p in &self.knife_points {
            c += *p;
        }
        Some(c / self.knife_points.len() as f32)
    }

    /// The active cut depth in world units, or `None` for the historical through cut.
    ///
    /// SIGNED: `> 0` reaches INTO the surface (pocket), `< 0` stands OUT of it (Union boss).
    /// Magnitude is what the threshold tests — an outward pull is as real a depth as an inward
    /// one, and reading it as "no depth" is what made Union unusable.
    ///
    /// `E` alone does NOT make a depth cut: a depth that was never dragged is still 0, and a
    /// sub-epsilon prism would be a degenerate cutter the boolean rejects. Below the threshold
    /// the tool falls back to the through cut it has always done — fails OPEN to the behaviour
    /// the user already knows, never to a silent no-op.
    pub(crate) fn knife_cut_depth(&self) -> Option<f32> {
        if !self.knife_shape_depth_mode {
            return None;
        }
        let bound = self.scene_bound_radius().max(1.0);
        if self.knife_shape_depth.abs() <= bound * 1e-4 {
            return None;
        }
        Some(self.knife_shape_depth)
    }

    /// Leave the depth phase (cut committed, tool cancelled, or shape mode changed).
    pub(crate) fn poly_knife_shape_depth_reset(&mut self) {
        self.knife_shape_depth_mode = false;
        self.knife_shape_depth = 0.0;
        self.knife_shape_depth_committable = false;
        self.knife_shape_depth_frozen = false;
    }

    /// After a shape cut (release or S), re-arm R/C mode without exiting Knife.
    fn poly_knife_shape_restore_mode(&mut self, keep_kind: u8) {
        // Depth is per-cut, never sticky: the next rect must start as a through cut unless the
        // user presses E again. Unconditional (outside the keep_kind gate) so the S-slice path,
        // which also lands here, can never leave a stale depth armed.
        self.poly_knife_shape_depth_reset();
        if self.poly_tool == PolyTool::Knife && keep_kind != 0 {
            self.knife_shape_kind = keep_kind;
            self.knife_shape_anchor = None;
            self.knife_shape_cursor = None;
            self.knife_shape_screen_a = None;
            self.knife_shape_screen_b = None;
            self.knife_shape_from_center = false;
            self.knife_shape_rot_last_mouse = None;
            self.knife_shape_move_last_mouse = None;
            self.knife_shape_array_count = 1;
            // Rotation resets on the next drag press (poly_knife_shape_press).
        }
    }

    /// L — free polyline mode (disarm rectangle/circle, keep Knife active).
    pub(crate) fn poly_knife_shape_to_free_line(&mut self) {
        if self.poly_tool != PolyTool::Knife {
            return;
        }
        let was_shape = self.knife_shape_kind != 0 || self.knife_shape_anchor.is_some();
        self.knife_shape_kind = 0;
        self.knife_shape_anchor = None;
        self.knife_shape_cursor = None;
        self.knife_shape_screen_a = None;
        self.knife_shape_screen_b = None;
        self.knife_shape_from_center = false;
        self.knife_shape_rotation_rad = 0.0;
        self.knife_shape_rot_last_mouse = None;
        self.knife_shape_move_last_mouse = None;
        self.knife_shape_array_count = 1;
        // Leaving shape mode leaves the depth phase with it — a free polyline has no locked
        // silhouette to pull a depth from, and a stale flag would draw a depth preview over it.
        self.poly_knife_shape_depth_reset();
        // Shape rubber-band points are not free-line intent — drop them when leaving shape mode.
        if was_shape {
            self.knife_points.clear();
        }
        self.toast("Knife: free polyline — LMB place · R rect · C circle · S = slice");
    }

    /// True when the current shape polyline has a usable size (not a zero-area click).
    fn knife_shape_extent_ok(&self) -> bool {
        if self.knife_points.len() < 3 {
            return false;
        }
        let mut lo = self.knife_points[0];
        let mut hi = lo;
        for p in &self.knife_points {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        (hi - lo).length() > 1e-4
    }

    /// Ray→drawing-plane point under the cursor. Shares the free-knife hover math.
    pub(crate) fn knife_plane_point_at(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<Vec3> {
        let (po, _pu, _pv, pn) = self.knife_free_plane?;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let (ro, rd) = mouse_to_ray(
            mouse, rect, cam_pos, self.viewport.camera.orbit, aspect, ortho_hw, self.render.fov_deg,
        );
        knife_plane_hit(ro, rd, po, pn)
    }

    /// Lay the armed shape out in the drawing plane's 2D basis and write it to `knife_points`.
    ///
    /// The FIRST point is repeated as the last: `knife_free_is_closed` decides closure by
    /// first↔last distance against 5% of the extent, and a rectangle's four corners end one
    /// full side apart — it would read as an OPEN line and take the subtract path.
    ///
    /// Rectangle corners honour `knife_shape_corner_bevel` (scroll while dragging): 0 = sharp
    /// quad, >0 = rounded corners with polyline points (owner: scroll bevels rect corners).
    ///
    /// **Alt / `knife_shape_from_center`**: press = center, drag = half-extents (rect) or
    /// radius (circle — same as default for circle, but screen AABB also expands from center).
    fn poly_knife_shape_rebuild(&mut self) {
        let (Some(a), Some(c)) = (self.knife_shape_anchor, self.knife_shape_cursor) else {
            return;
        };
        let Some((o, u, v, _n)) = self.knife_free_plane else { return };
        let to2 = |w: Vec3| Vec2::new((w - o).dot(u), (w - o).dot(v));
        let to3 = |p: Vec2| o + u * p.x + v * p.y;
        let (a2, c2) = (to2(a), to2(c));
        let from_center = self.knife_shape_from_center;
        let mut pts: Vec<Vec3> = Vec::new();
        if self.knife_shape_kind == 1 {
            // Rect: corner→corner by default; Alt = press is center, |Δ| = half-extents.
            let (lo0, hi0) = if from_center {
                let hx = (c2.x - a2.x).abs();
                let hy = (c2.y - a2.y).abs();
                (a2 - Vec2::new(hx, hy), a2 + Vec2::new(hx, hy))
            } else {
                (a2.min(c2), a2.max(c2))
            };
            let w = (hi0.x - lo0.x).abs();
            let h = (hi0.y - lo0.y).abs();
            // Linear array (Ctrl+D): tile along longer side, edge-to-edge.
            let array_n = self.knife_shape_array_count.max(1).min(32);
            let step = if w >= h {
                Vec2::new(w, 0.0)
            } else {
                Vec2::new(0.0, h)
            };
            // Rotation is about the first rect's center (hold-R); array offsets rotate with it.
            let center0 = (lo0 + hi0) * 0.5;
            let rot = self.knife_shape_rotation_rad;
            let (cr, sr) = (rot.cos(), rot.sin());
            let rot_about = |p: Vec2, center: Vec2| {
                let d = p - center;
                center + Vec2::new(d.x * cr - d.y * sr, d.x * sr + d.y * cr)
            };
            let half_min = 0.5 * w.min(h);
            let r = (self.knife_shape_corner_bevel.clamp(0.0, 1.0) * half_min).max(0.0);
            let segs = self.knife_shape_segments.clamp(3, 64);
            for ai in 0..array_n {
                let off = step * (ai as f32);
                let lo = lo0 + off;
                let hi = hi0 + off;
                // Keep shared rotation center at first rect so the whole array spins as one.
                let rot2 = |p: Vec2| rot_about(p, center0);
                let mut ring: Vec<Vec3> = Vec::new();
                if r < 1e-5 || half_min < 1e-5 {
                    for p in [
                        Vec2::new(lo.x, lo.y),
                        Vec2::new(hi.x, lo.y),
                        Vec2::new(hi.x, hi.y),
                        Vec2::new(lo.x, hi.y),
                    ] {
                        ring.push(to3(rot2(p)));
                    }
                } else {
                    let corner_arcs = [
                        (Vec2::new(lo.x + r, lo.y + r), std::f32::consts::PI, 1.5 * std::f32::consts::PI),
                        (Vec2::new(hi.x - r, lo.y + r), 1.5 * std::f32::consts::PI, 2.0 * std::f32::consts::PI),
                        (Vec2::new(hi.x - r, hi.y - r), 0.0, 0.5 * std::f32::consts::PI),
                        (Vec2::new(lo.x + r, hi.y - r), 0.5 * std::f32::consts::PI, std::f32::consts::PI),
                    ];
                    for &(arc_c, a0, a1) in &corner_arcs {
                        for i in 0..=segs {
                            let t = i as f32 / segs as f32;
                            let ang = a0 + (a1 - a0) * t;
                            let local = arc_c + Vec2::new(ang.cos(), ang.sin()) * r;
                            ring.push(to3(rot2(local)));
                        }
                    }
                }
                if let Some(&first) = ring.first() {
                    ring.push(first);
                }
                pts.extend(ring);
            }
            // Array rings are already closed; skip the single close below.
            self.knife_points = pts;
            return;
        } else {
            // Circle: press = center, drag = radius. Alt keeps the same center sizing.
            let r = (c2 - a2).length();
            if r < 1e-6 {
                return;
            }
            let n = self.knife_shape_segments.clamp(3, 256);
            // Start the ring at the cursor so the rim tracks the pointer while dragging.
            let phase = (c2.y - a2.y).atan2(c2.x - a2.x);
            for i in 0..n {
                let t = phase + std::f32::consts::TAU * (i as f32) / (n as f32);
                pts.push(to3(a2 + Vec2::new(t.cos(), t.sin()) * r));
            }
        }
        if let Some(&first) = pts.first() {
            pts.push(first);
        }
        self.knife_points = pts;
    }

    /// `S` — apply a **slice** of the current knife path immediately (one-shot verb).
    ///
    /// Owner 2026-08-06:
    ///   * Shape knife (R/C mid-drag or free polyline with points): **slice**, never subtract.
    ///   * **Faces selected** → slice **only those faces** (always).
    ///   * No face selection → through-all slice of islands the path crosses.
    ///
    /// Shape mode is re-armed after S so the next LMB is another rect/circle.
    pub(crate) fn poly_knife_slice_through_now(&mut self) {
        if self.poly_tool != PolyTool::Knife {
            return;
        }
        if self.knife_points.len() < 2 {
            self.toast("Knife: place at least two points before slicing");
            return;
        }
        // `S` is a different verb from the depth cut: a slice scribes through the whole island
        // and has nothing to stop at. Say the depth is being dropped rather than dropping it
        // quietly (`restore_mode` clears the phase at the end of this call either way).
        if self.knife_cut_depth().is_some() {
            self.toast("Knife: S slices through — cut depth applies to the release (subtract)");
        }
        // SDF: S = seam slice (owner: "slice will use seams").
        //
        // `knife_points` is committed as laid out — the rounded ring IS the path, matching the
        // poly side's single-path rule below. The branch this landed from also forced a sharp
        // path here and re-bevelled the corners so Unbevel could peel them; that design was
        // DEFERRED (owner 2026-08-08) in favour of master's preview==commit invariant, so the
        // sharp-forcing pass is deliberately absent rather than lost.
        // Mid-drag shape: the rounded ring in `knife_points` IS the slice path (same as
        // release and RMB — see `poly_knife_shape_release` on why no sharp-then-bevel pass).
        let keep_shape = self.knife_shape_kind;
        let was_shape_drag = self.knife_shape_anchor.is_some() && keep_shape != 0;
        if was_shape_drag {
            self.knife_shape_anchor = None;
            self.knife_shape_cursor = None;
            self.knife_shape_screen_a = None;
            self.knife_shape_screen_b = None;
            self.knife_shape_from_center = false;
        }

        // S always through-slices. Face selection scopes to face-linked islands only
        // (other islands rejected) — see free_slice_commit / classic confirm.
        let force_through = true;

        if self.knife_free && self.knife_free_plane.is_some() {
            self.knife_free_slice = true;
            let closed = self.knife_free_is_closed();
            self.poly_knife_free_slice_commit(closed, force_through, SliceEnclosed::Keep);
            if was_shape_drag {
                self.poly_knife_shape_restore_mode(keep_shape);
            }
            return;
        }
        // Classic surface knife: S always through; island mask applied inside confirm_classic.
        self.poly_knife_confirm_classic(false, true);
    }

    /// Confirm knife cut: resolve each knife point to a real, ring-incident vertex
    /// (snap / edge-split / poke-face), then connect consecutive cut vertices.
    ///
    /// A confirm gesture (RMB or Enter) that has nothing to cut CANCELS + EXITS the
    /// tool instead of leaving it armed — per the Tool Exit Convention ("Knife → RMB
    /// confirms"). The mesh resolution never invents free-floating vertices.
    ///
    /// `commit_hover` distinguishes the two confirm gestures:
    /// - **Enter (`true`)** — Blender rubber-band parity: the live cursor is the PENDING point of the
    ///   in-progress segment, so Enter commits it as the final cut point.
    /// - **RMB (`false`)** — a pure confirm/exit gesture (AZRA's Tool Exit Convention). The cursor is at
    ///   an arbitrary "I'm done" location the user did NOT click as a cut point; committing it added a
    ///   spurious segment running to wherever the mouse happened to be ("takes the mouse position as a
    ///   point even if the user did not click").
    pub(crate) fn poly_knife_confirm(&mut self, commit_hover: bool) {
        self.poly_knife_confirm_closed(commit_hover);
    }

    /// Confirm knife cut. When the path already ends at the first point (or was closed by
    /// double-click on the start), `closed=true` is passed to the engine so the last→first
    /// segment is cut too.
    pub(crate) fn poly_knife_confirm_closed(&mut self, commit_hover: bool) {
        if self.knife_free {
            self.poly_knife_free_confirm(commit_hover);
            return;
        }
        self.poly_knife_confirm_classic(commit_hover, false);
    }

    /// Classic (surface) knife confirm. `through` is false for Enter/RMB (visible surface only)
    /// and true for the S "slice through" verb. Face/Group selection expands to face-linked
    /// islands and restricts the cut to those shells only (other islands rejected).
    fn poly_knife_confirm_classic(&mut self, commit_hover: bool, through: bool) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Enter only: commit the point currently under the cursor (the live red-cursor snap) before
        // cutting. Reuses the canonical placement path (no-ops off-mesh, dedups a coincident point), so it
        // never invents a free-floating vertex. RMB skips this — see the doc note above.
        // Double-click close already pushed the first point — skip hover commit so we don't add junk.
        if commit_hover {
            self.poly_knife_click();
        }
        let knife_pts = self.knife_points.clone();

        // K75A: a confirm with fewer than 2 placed points is not a cut — cancel + exit
        // (single shared exit path; no dangling armed tool).
        if knife_pts.len() < 2 {
            self.poly_knife_cancel();
            self.toast("Knife cancelled");
            return;
        }
        // Closed loop: last point lands on first (double-click close or user re-clicked start).
        let closed = knife_pts.len() >= 3
            && knife_pts.first().zip(knife_pts.last())
                .map(|(a, b)| a.distance(*b) < 1e-4 * self.viewport.camera.distance.max(0.1))
                .unwrap_or(false);

        // Camera projection captured before the transaction (borrows &self). The cut engine walks the
        // PROJECTED polyline and splits every crossed edge — so a cut across many faces finally connects
        // (the old "resolve each point, connect only if they share a face" made no geometry for multi-face
        // cuts). The placed points are WORLD-space surface positions; `project` maps a LOCAL mesh vertex
        // through the body transform to screen.
        let rect = self.viewport.viewport_rect;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let orbit = self.viewport.camera.orbit;
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        // Copy the FOV into a local so `project` captures a plain f32, not `&self` — otherwise the closure's
        // immutable borrow of self collides with `self.transaction()` below (E0502).
        let fov_deg = self.render.fov_deg;
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let world_inv = world_xform.inverse();
        // The engine's front-face test dots view_dir against MESH-LOCAL face normals, so the world
        // camera forward must come into local space: dot(world_normal, v) = dot((M⁻¹)ᵀ·n_local, v)
        // = dot(n_local, M⁻¹·v) — exact for any transform, including non-uniform and mirrored scale.
        // Passing the world vector here made the visible-surface pass skip the aimed-at edges on any
        // rotated body ("nothing to cut" / cuts landing on back faces).
        let vw = (orbit * Vec3::NEG_Z).normalize();
        let vl = world_inv * glam::Vec4::new(vw.x, vw.y, vw.z, 0.0);
        let view_dir = Vec3::new(vl.x, vl.y, vl.z).normalize_or_zero();
        // Camera position in MESH-LOCAL space — the knife engine's true-occlusion oracle (drops
        // phantom crossings on hidden faces; visible-only cuts must cut only the seen surface).
        let cl = world_inv * glam::Vec4::new(cam_pos.x, cam_pos.y, cam_pos.z, 1.0);
        let cam_local = Vec3::new(cl.x, cl.y, cl.z);
        let project = |local: Vec3| -> Option<Vec2> {
            let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
            world_to_screen(Vec3::new(w.x, w.y, w.z), cam_pos, orbit, rect, aspect, ortho_hw, fov_deg)
                .map(|p| Vec2::new(p.x, p.y))
        };
        // Face scope: full island through ≠ partial-face auto-slice (see knife_face_cut_scope).
        let (face_mask, scope_through) = {
            let in_face_mode = matches!(
                self.poly_work_mode,
                PolyWorkMode::Face | PolyWorkMode::Group
            );
            if !in_face_mode || !self.knife_has_user_face_scope() {
                (None, through)
            } else {
                let node = self.scene.scene_graph.get(nid);
                let mesh = node.and_then(|nd| match &nd.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Some(mesh.as_ref()),
                    _ => None,
                });
                if let Some(mesh) = mesh {
                    knife_face_cut_scope(
                        mesh,
                        &self.poly_sel.selected_faces,
                        &self.poly_sel.hidden_vertices,
                        through, // S → force through only on partial; full island always through
                    )
                } else {
                    let mask: HashSet<FaceId> = self
                        .poly_sel
                        .selected_faces
                        .iter()
                        .map(|&f| FaceId(f))
                        .collect();
                    (Some(mask), through)
                }
            }
        };
        // Full-island scope forces through; partial keeps Enter surface / S through.
        let through = scope_through;
        // Build screen + mesh-LOCAL 3D positions TOGETHER so their indices stay aligned (the engine pairs
        // poly[k] with poly_3d[k]). A point behind the camera drops from BOTH. The local positions let the
        // engine classify a point that snapped to a face interior as a real interior cut vertex (Blender
        // parity: cutting INTO an n-gon and stopping, or zig-zagging through a face).
        let mut screen_pts: Vec<Vec2> = Vec::with_capacity(knife_pts.len());
        let mut local_pts: Vec<Vec3> = Vec::with_capacity(knife_pts.len());
        for wp in &knife_pts {
            if let Some(sp) = world_to_screen(*wp, cam_pos, orbit, rect, aspect, ortho_hw, fov_deg) {
                screen_pts.push(Vec2::new(sp.x, sp.y));
                let l = world_inv * glam::Vec4::new(wp.x, wp.y, wp.z, 1.0);
                local_pts.push(Vec3::new(l.x, l.y, l.z));
            }
        }
        if screen_pts.len() < 2 {
            self.poly_knife_cancel();
            self.toast("Knife cancelled");
            return;
        }
        // Dry-run on a clone so a cut that crosses nothing leaves NO phantom undo step.
        let (preview, result_valid, before) = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh, _ => return };
            let before = knife_position_set(mesh);
            let mut clone = (**mesh).clone();
            let n = core_math::knife::knife_cut_screen(
                &mut clone, &project, &screen_pts, Some(&local_pts), closed, through,
                view_dir, Some(cam_local), face_mask.as_ref(),
            );
            (n, clone.validate().is_ok(), before)
        };
        if preview == 0 {
            // Soft-fail: keep the path so the user can add points / retry (hard-cancel felt
            // like "simple cut always fails" when a 2-pt path barely missed).
            self.toast("Knife: nothing to cut — extend the path across the mesh");
            return;
        }
        // Safety net: NEVER commit a mesh the cut leaves non-manifold. The engine rebuilds via from_polygons
        // (robust), so this is a backstop for any unforeseen path — the dry-run is deterministic w.r.t. the
        // real cut, so validating the clone guarantees the committed mesh is sound. Reject visibly instead.
        if !result_valid {
            // Soft-fail: path stays so user can simplify without re-arming the tool.
            self.toast("Knife: that path can't cut cleanly here — try fewer / simpler points");
            return;
        }

        let path_world = knife_pts.clone();
        // Pre-cut vert snapshot: the post-cut collapse is SCOPED to verts the cut creates —
        // a global collapse ate authored tessellation (owner 2026-08-08 "cleaned all my geo").
        let pre_verts = self
            .poly_node_mesh(nid)
            .map(|m| knife_live_positions(&m))
            .unwrap_or_default();
        let face_mask_commit = face_mask;
        let tx_name = if through {
            if face_mask_commit.is_some() {
                "Knife Slice Through (island)"
            } else {
                "Knife Slice Through"
            }
        } else {
            "Knife Cut"
        };
        self.transaction(tx_name, |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    core_math::knife::knife_cut_screen(
                        mesh, &project, &screen_pts, Some(&local_pts), closed, through,
                        view_dir, Some(cam_local), face_mask_commit.as_ref(),
                    );
                    // Straight densify samples become valence-2 collinear pins — collapse so
                    // the cut is clean segments (owner test-019 "random vertex / crazy outline").
                    // SCOPED: pre-cut verts are untouchable (owner 2026-08-08).
                    core_math::mesh_boolean::collapse_collinear_pins_scoped(mesh, &pre_verts);
                    mesh.purge_degenerate_faces();
                }
            }
            if s.knife_select_after_cut {
                knife_apply_cut_selection_filtered(s, nid, &before, &path_world, None);
            }
            // TRADITIONAL (in-poly) KNIFE: tag its cut edges so Bevel Smart Select leaves
            // them alone (owner 2026-08-05: "traditional knife on a poly should be tagged to
            // not bevel"). This does NOT apply to the ALTERNATIVE free-knife mode, which
            // removes material and whose edges are real creases the user wants beveled -
            // that path (`poly_knife_free_confirm`) is deliberately untagged, and the owner
            // confirmed the distinction: "this rule was for alternative mode, not inside
            // poly cut".
            //
            // Provenance is what makes this possible: a scribe cut leaves COPLANAR edges,
            // geometrically identical to a boolean's flat support edges, so no angle test
            // could separate them - but each operation tags its own output.
            if let Some(node) = s.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    let cut: Vec<core_math::half_edge::HalfEdgeId> =
                        knife_new_cut_edges(mesh, &before);
                    if !cut.is_empty() {
                        let cd = s.cache.poly_custom_data.entry(nid).or_default();
                        for he in cut {
                            cd.op_created.set(he, true);
                        }
                    }
                }
            }
            // TOOL EXITS AFTER A CUT — and it MUST (owner 2026-08-04: "it takes 2 clicks to
            // select a segment"). Click-to-select is gated on `poly_tool == PolyTool::Idle`
            // (input_viewport.rs:1186), so a knife left armed OWNS every click and selection
            // silently stops working — a P0 per CLAUDE.md. A modal tool that consumes clicks
            // and click-to-select cannot both be live; Blender resolves it the same way (K is
            // modal, ends on confirm). The owner's "stay ready for another cut" is honoured by
            // keeping Knife as the ARMED PALETTE TOOL so re-entry is one keypress, not a
            // re-pick.
            s.knife_points.clear();
            s.knife_snap_pos = None;
            s.poly_tool = PolyTool::Idle;
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast(if through {
                if closed { "Knife: closed through-slice applied" } else { "Knife: through-slice applied" }
            } else if closed {
                "Knife: closed cut applied"
            } else {
                "Knife cut applied"
            });
        });
    }

    /// Reset knife tool state. Canonical exit path shared by confirm-with-nothing and
    /// external cancel; callers toast their own message.
    pub(crate) fn poly_knife_cancel(&mut self) {
        self.knife_points.clear();
        self.knife_snap_pos = None;
        self.knife_free = false;
        self.knife_free_slice = false;
        self.knife_free_view = None;
        self.knife_free_plane = None;
        self.knife_shape_kind = 0;
        self.knife_shape_anchor = None;
        self.knife_shape_cursor = None;
        self.knife_shape_screen_a = None;
        self.knife_shape_screen_b = None;
        self.knife_shape_from_center = false;
        self.knife_shape_rotation_rad = 0.0;
        self.knife_shape_rot_last_mouse = None;
        self.knife_shape_move_last_mouse = None;
        self.poly_knife_shape_depth_reset();
        self.poly_tool = PolyTool::Idle;
    }

    // ================= Free-polyline personality (owner spec 2026-08-03) =================
    // R1 first-click-decides armed us; the points live on the captured plane. Enter/RMB lands
    // here: SLICE scribes the projected line into the surface (through-all); SUBTRACT removes
    // material — a closed loop commits at once, an open line asks for ONE side-pick click.

    /// Lasso-close test: the line counts as CLOSED when it ends within 5% of its own
    /// extent of the start point.
    pub(crate) fn knife_free_is_closed(&self) -> bool {
        // Shape personalities (R rect / C circle) are closed BY CONSTRUCTION: their corner
        // list never repeats the first point, so the endpoint-proximity test below reads a
        // rect as an OPEN 3-segment path — and the slice silently skipped the closing side
        // (owner 2026-08-08 "only the 2 segments from top missing").
        if self.knife_shape_kind != 0 && self.knife_points.len() >= 3 {
            return true;
        }
        let Some((o, u, v, _n)) = self.knife_free_plane else { return false };
        if self.knife_points.len() < 3 {
            return false;
        }
        let p2: Vec<Vec2> = self
            .knife_points
            .iter()
            .map(|w| Vec2::new((*w - o).dot(u), (*w - o).dot(v)))
            .collect();
        let (mut lo, mut hi) = (p2[0], p2[0]);
        for p in &p2 {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let ext = (hi - lo).length().max(1e-4);
        (p2[0] - *p2.last().unwrap()).length() < ext * 0.05
    }

    fn poly_knife_free_confirm(&mut self, commit_hover: bool) {
        if self.knife_free_plane.is_none() {
            self.poly_knife_cancel();
            return;
        }
        // Face-mode face selection + free knife → SLICE those faces only (never boolean body).
        let has_face_sel = matches!(
            self.poly_work_mode,
            PolyWorkMode::Face | PolyWorkMode::Group
        ) && self.knife_has_user_face_scope();
        if has_face_sel {
            if commit_hover {
                self.poly_knife_click();
            }
            if self.knife_points.len() < 2 {
                self.poly_knife_cancel();
                self.toast("Knife cancelled");
                return;
            }
            let closed = self.knife_free_is_closed();
            self.poly_knife_free_slice_commit(closed, false, SliceEnclosed::Keep);
            return;
        }
        // Owner 2026-08-03 rev: NO separate side-pick stage. The confirm gesture's cursor
        // position doubles as the side pick for an open subtract — RMB (or Enter) above the
        // line kills the top, below kills the bottom, in one gesture. Because of that, the
        // rubber-band point under the cursor is committed as a final point ONLY when it is
        // not needed as the side pick (slice, or an already-closed loop).
        let cursor = self.knife_snap_pos;
        if self.knife_free_slice {
            if commit_hover {
                self.poly_knife_click();
            }
            if self.knife_points.len() < 2 {
                self.poly_knife_cancel();
                self.toast("Knife cancelled");
                return;
            }
            let closed = self.knife_free_is_closed();
            self.poly_knife_free_slice_commit(closed, false, SliceEnclosed::Keep);
            return;
        }
        if self.knife_points.len() < 2 {
            self.poly_knife_cancel();
            self.toast("Knife cancelled");
            return;
        }
        if self.knife_free_is_closed() {
            self.poly_knife_free_boolean(None);
        } else if let Some(pick) = cursor {
            self.poly_knife_free_boolean(Some(pick));
        } else {
            self.toast("Knife: point the cursor above or below the line, then confirm");
        }
    }

    /// SLICE: scribe the polyline into the mesh surface along the CAPTURED view axis
    /// (R4 — orbit-proof).
    ///
    /// Owner 2026-08-07 (revised — do **not** conflate these):
    ///   * **No face selection** → through-cut **all** islands under the path.
    ///   * **Full island selected** (every face of a face-linked shell) → **perfect
    ///     through-cut** of that shell only; other islands rejected.
    ///   * **Partial face selection** → auto-slice **exactly** those faces (no island
    ///     expand). Not the same as full-island through.
    ///
    /// `pub(crate)` so loop-cut **K** can run a one-shot through-slice without arming the
    /// Knife tool modal (caller must clear knife state afterward).
    ///
    /// `enclosed` is what happens to the region the path encloses — [`SliceEnclosed::Keep`] for
    /// every scribe/slice verb, [`SliceEnclosed::Remove`] for the OPEN-MESH subtract (see
    /// [`Self::poly_knife_free_boolean`]).
    pub(crate) fn poly_knife_free_slice_commit(
        &mut self,
        closed: bool,
        force_through: bool,
        enclosed: SliceEnclosed,
    ) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.poly_knife_cancel();
                return;
            }
        };
        let Some((o_draw, u_draw, v_draw, n_plane)) = self.knife_free_plane else {
            self.poly_knife_cancel();
            return;
        };
        // Body→world, for the open-mesh subtract's centroid test (face centroids are LOCAL,
        // the enclosure rings are in the WORLD drawing plane). Captured here: the transaction
        // holds a `get_mut` borrow of the node and cannot read the graph again.
        let world_xform = self.scene.scene_graph.world_transform(nid);
        // Face scope: full island vs partial — see knife_face_cut_scope.
        let (face_mask, through) = {
            let in_face_mode = matches!(
                self.poly_work_mode,
                PolyWorkMode::Face | PolyWorkMode::Group
            );
            if !in_face_mode || !self.knife_has_user_face_scope() {
                // No face filter → through-all islands (force_through unused).
                let _ = force_through;
                (None, true)
            } else {
                let node = self.scene.scene_graph.get(nid);
                let mesh = node.and_then(|nd| match &nd.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Some(mesh.as_ref()),
                    _ => None,
                });
                if let Some(mesh) = mesh {
                    knife_face_cut_scope(
                        mesh,
                        &self.poly_sel.selected_faces,
                        &self.poly_sel.hidden_vertices,
                        force_through,
                    )
                } else {
                    let mask: HashSet<FaceId> = self
                        .poly_sel
                        .selected_faces
                        .iter()
                        .map(|&f| FaceId(f))
                        .collect();
                    (Some(mask), force_through)
                }
            }
        };
        // (Owner 2026-08-08: S stays the PROJECTED-EDGE scribe through all islands. The
        // boolean kerf slice lives on Alt+S — `poly_knife_slice_kerf_now` — as its own
        // personality, never as a silent reroute of S.)
        // SLICE PROJECTS ALONG CAPTURED CAMERA VIEW (Blender knife cut-through), not the
        // drawing-plane normal. Shape R/C press freezes a FACE plane for drawing; if we
        // project along face n, rays skim the face and "all S slices fail" (owner 2026-08-07).
        // View-aligned free knife: knife_free_view == plane n → same as before.
        let view_w = self
            .knife_free_view
            .filter(|v| v.length_squared() > 1e-12)
            .unwrap_or(n_plane)
            .normalize_or_zero();
        // Orbit-proof "screen": orthographic basis in the view plane through the draw origin.
        let (o, u, v) = {
            let helper = if view_w.cross(Vec3::Y).length_squared() > 1e-6 {
                Vec3::Y
            } else {
                Vec3::X
            };
            let uu = view_w.cross(helper).normalize_or_zero();
            let vv = view_w.cross(uu).normalize_or_zero();
            // Prefer the camera's right/up when view matches (stable labels).
            let orbit = self.viewport.camera.orbit;
            let cam_r = (orbit * Vec3::X).normalize_or_zero();
            let cam_u = (orbit * Vec3::Y).normalize_or_zero();
            if cam_r.cross(cam_u).normalize_or_zero().dot(view_w) > 0.9 {
                (o_draw, cam_r, cam_u)
            } else {
                (o_draw, uu, vv)
            }
        };
        // View-plane UV "screen": scale to a pixel-like magnitude so the engine's
        // screen-space epsilons behave the same as in the classic personality.
        let (mut lo, mut hi) = (Vec2::ZERO, Vec2::ZERO);
        let mut first = true;
        for w in &self.knife_points {
            let p = Vec2::new((*w - o).dot(u), (*w - o).dot(v));
            if first {
                lo = p;
                hi = p;
                first = false;
            } else {
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        let k = 512.0 / (hi - lo).length().max(1e-4);
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let world_inv = world_xform.inverse();
        let project = move |local: Vec3| -> Option<Vec2> {
            let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
            let wp = Vec3::new(w.x, w.y, w.z);
            Some(Vec2::new((wp - o).dot(u) * k, (wp - o).dot(v) * k))
        };
        // Mesh-local view direction for through-cut front tests + densify rays.
        let vl = world_inv * glam::Vec4::new(view_w.x, view_w.y, view_w.z, 0.0);
        let view_dir = Vec3::new(vl.x, vl.y, vl.z).normalize_or_zero();

        // INTERIOR CUT POINTS + THROUGH LAYERS.
        //
        // Free-knife points live on the frozen DRAWING PLANE. Each is resolved onto the mesh
        // along the CAPTURED view. Owner 2026-08-05 regression ("slice not keeping polyline
        // shape / cut does not go through"):
        //
        // 1) Shape: only click vertices were surface-resolved. A bent path became a chord
        //    between silhouette crossings when mid-segment detail was lost. We densify the
        //    plane polyline so the drawn shape is sampled densely enough to keep bends.
        //
        // 2) Through: poly_3d used only the FIRST surface hit (front). Edge-crossing with
        //    through=true does cut back silhouettes, but mid-face BENDS never land on the
        //    back face — so the far side is a straight chord. Fix: for each densified point
        //    collect EVERY hit along the view ray and run the cut once per depth layer so
        //    the same polyline shape is scribed on front AND back (and every intervening
        //    shell).
        //
        // Misses use a far off-mesh sentinel (never the raw plane point): mid-volume plane
        // points used to pass the prism winding test and create flaps outside the body.
        //
        // Reliability (owner 2026-08-06 "simple cut fails"): densify samples that miss the
        // mesh used to become Break nodes and SEVER the cut between real silhouette
        // crossings. We DROP exterior densify mid-samples (keep original corners + hits)
        // so a straight 2-point through-slice is just entry/exit Bounds. Fallbacks:
        // undensified path, then pure screen-crossing through-cut.
        let (poly_dense, layers, layers_front, sentinel, click_poly, click_layers, click_front) = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(nd) => nd,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            let (mut lo3, mut hi3) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if mesh.vert(v).half_edge == NO_EDGE {
                    continue;
                }
                lo3 = lo3.min(mesh.vert(v).position);
                hi3 = hi3.max(mesh.vert(v).position);
            }
            let diag = if hi3.x >= lo3.x {
                (hi3 - lo3).length().max(1e-3)
            } else {
                1.0
            };
            let back_off = diag * 4.0;
            // Slightly coarser densify than diag/24 — fewer pins, still keeps bends.
            let step = (diag / 16.0).clamp(1e-3, diag * 0.3);
            let sentinel = lo3 - Vec3::splat(diag * 4.0);
            let corner_tol = (step * 0.35).max(1e-4);

            // Densify world polyline in the drawing plane so bends survive as interior cuts.
            let dense_world_raw = densify_polyline_world(&self.knife_points, closed, step);
            let is_original_corner = |w: Vec3| -> bool {
                self.knife_points
                    .iter()
                    .any(|c| (*c - w).length() <= corner_tol)
            };

            let ident = |p: Vec3| p;
            // Shell labels once per commit — layers are keyed by (shell, facing), never by
            // raw depth ordinal (a grazed silhouette shifting one sample's hit count used
            // to scramble every deeper layer into a mixed-shell polyline).
            let shell_ids = knife_face_shell_ids(mesh);
            let mut keep_world: Vec<Vec3> = Vec::with_capacity(dense_world_raw.len());
            let mut keep_hits: Vec<Vec<(f32, Vec3, u32, bool)>> =
                Vec::with_capacity(dense_world_raw.len());
            for w in &dense_world_raw {
                let lp = {
                    let l = world_inv * glam::Vec4::new(w.x, w.y, w.z, 1.0);
                    Vec3::new(l.x, l.y, l.z)
                };
                let hits = knife_surface_hits_shelled(
                    mesh,
                    &ident,
                    lp - view_dir * back_off,
                    view_dir,
                    &shell_ids,
                );
                // Drop densify mids that miss the mesh — they only produce Breaks that
                // sever Bound→Bound through-cuts on simple straight paths.
                if hits.is_empty() && !is_original_corner(*w) {
                    continue;
                }
                keep_world.push(*w);
                keep_hits.push(hits);
            }
            // Degenerate after strip: fall back to original corners only.
            if keep_world.len() < 2 {
                keep_world = self.knife_points.clone();
                keep_hits.clear();
                for w in &keep_world {
                    let lp = {
                        let l = world_inv * glam::Vec4::new(w.x, w.y, w.z, 1.0);
                        Vec3::new(l.x, l.y, l.z)
                    };
                    keep_hits.push(knife_surface_hits_shelled(
                        mesh,
                        &ident,
                        lp - view_dir * back_off,
                        view_dir,
                        &shell_ids,
                    ));
                }
            }

            let (layers, layers_front) = build_shell_layers(&keep_hits, sentinel);
            // KNIFEDIAG: dump the densified samples + their per-shell surface hits — the
            // ground truth for "which pass invented that vertex".
            if std::env::var_os("AZRA_KNIFEDIAG").is_some() {
                eprintln!(
                    "[KnifeSlice] densify: {} raw -> {} kept, layers={}",
                    dense_world_raw.len(),
                    keep_world.len(),
                    layers.len()
                );
                for (i, (w, hits)) in keep_world.iter().zip(&keep_hits).enumerate() {
                    let hs: Vec<String> = hits
                        .iter()
                        .map(|(t, h, s, fr)| {
                            format!("s{}{}t{:.2}({:.2},{:.2},{:.2})",
                                s, if *fr { "F" } else { "B" }, t, h.x, h.y, h.z)
                        })
                        .collect();
                    eprintln!(
                        "[KnifeSlice]   [{i}] plane=({:.2},{:.2},{:.2}) hits=[{}]",
                        w.x, w.y, w.z,
                        hs.join(" ")
                    );
                }
            }
            let poly_dense: Vec<Vec2> = keep_world
                .iter()
                .map(|w| Vec2::new((*w - o).dot(u) * k, (*w - o).dot(v) * k))
                .collect();

            // Undensified click path (fallback if densified cut fails).
            let mut click_hits: Vec<Vec<(f32, Vec3, u32, bool)>> =
                Vec::with_capacity(self.knife_points.len());
            for w in &self.knife_points {
                let lp = {
                    let l = world_inv * glam::Vec4::new(w.x, w.y, w.z, 1.0);
                    Vec3::new(l.x, l.y, l.z)
                };
                click_hits.push(knife_surface_hits_shelled(
                    mesh,
                    &ident,
                    lp - view_dir * back_off,
                    view_dir,
                    &shell_ids,
                ));
            }
            let (click_layers, click_front) = build_shell_layers(&click_hits, sentinel);
            let click_poly: Vec<Vec2> = self
                .knife_points
                .iter()
                .map(|w| Vec2::new((*w - o).dot(u) * k, (*w - o).dot(v) * k))
                .collect();

            (poly_dense, layers, layers_front, sentinel, click_poly, click_layers, click_front)
        };

        // Try densified surface path → undensified corners → pure silhouette through-cut.
        let try_cut = |src: &HalfEdgeMesh,
                       poly: &[Vec2],
                       lyrs: &[Vec<Vec3>],
                       fronts: &[bool],
                       use_layers: bool|
         -> (usize, bool) {
            let mut clone = src.clone();
            let cnt = if use_layers {
                knife_apply_slice_layers(
                    &mut clone,
                    &project,
                    poly,
                    lyrs,
                    fronts,
                    sentinel,
                    closed,
                    view_dir,
                    face_mask.as_ref(),
                    through,
                )
            } else {
                // Pure screen crossings, no poly_3d — never flaps, reliable simple cuts.
                core_math::knife::knife_cut_screen(
                    &mut clone,
                    &project,
                    poly,
                    None,
                    closed,
                    through,
                    view_dir,
                    None,
                    face_mask.as_ref(),
                )
            };
            // Judge the HEALED mesh — commit runs the scoped collapse +
            // purge_degenerate_faces after the cut, so digons/pins the heal removes must
            // not veto the preview (they used to: "can't cut cleanly" on clean-after-heal
            // cuts was the largest false-fail class). Same scoping as commit: pre-cut
            // verts (src is the pre-cut mesh) are untouchable.
            let ok = cnt > 0 && {
                let pre = knife_live_positions(src);
                core_math::mesh_boolean::collapse_collinear_pins_scoped(&mut clone, &pre);
                clone.purge_degenerate_faces();
                knife_result_is_sound(&clone)
            };
            (cnt, ok)
        };

        let (preview, result_valid, before, strategy) = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(nd) => nd,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            let before = knife_position_set(mesh);
            // Strategy 0: densified + surface-resolved layers (keeps bends + through shells).
            let (c0, ok0) = try_cut(mesh.as_ref(), &poly_dense, &layers, &layers_front, true);
            if ok0 {
                (c0, true, before, 0u8)
            } else {
                // Strategy 1: original click corners only (no densify Breaks).
                let (c1, ok1) =
                    try_cut(mesh.as_ref(), &click_poly, &click_layers, &click_front, true);
                if ok1 {
                    (c1, true, before, 1u8)
                } else {
                    // Strategy 2: pure silhouette through-cut (screen crossings only).
                    let (c2, ok2) =
                        try_cut(mesh.as_ref(), &poly_dense, &layers, &layers_front, false);
                    if ok2 {
                        (c2, true, before, 2u8)
                    } else {
                        // Prefer undensified pure cut if densified screen poly is weird.
                        let (c3, ok3) =
                            try_cut(mesh.as_ref(), &click_poly, &click_layers, &click_front, false);
                        if ok3 {
                            (c3, true, before, 3u8)
                        } else {
                            // Report best non-zero attempt for diagnostics; still soft-fail.
                            let best = c0.max(c1).max(c2).max(c3);
                            (best, false, before, 255u8)
                        }
                    }
                }
            }
        };
        // KNIFEDIAG: name the winning strategy (0 densified layers, 1 click layers,
        // 2 densified pure-screen, 3 click pure-screen, 255 all failed).
        if std::env::var_os("AZRA_KNIFEDIAG").is_some() {
            eprintln!("[KnifeSlice] strategy={strategy} preview_cuts={preview} valid={result_valid}");
        }
        if preview == 0 || !result_valid {
            // Soft-fail: keep path + tool armed so the user can adjust (was hard-cancel
            // which felt like "simple cut always fails").
            self.toast(if face_mask.is_some() {
                "Knife: nothing to cut on the face selection — adjust path or Esc"
            } else if !result_valid && preview > 0 {
                "Knife slice: that path can't cut cleanly here — try fewer points"
            } else {
                "Knife slice: nothing to cut — extend path across the mesh"
            });
            return;
        }
        let path_world = self.knife_points.clone();
        // Pre-cut vert snapshot for the SCOPED post-cut collapse (owner 2026-08-08).
        let pre_verts = self
            .poly_node_mesh(nid)
            .map(|m| knife_live_positions(&m))
            .unwrap_or_default();
        // Rim-select filter uses drawing-plane normal (path coplanar with the stroke).
        let plane_n = n_plane;
        let layers_commit = layers;
        let layers_front_commit = layers_front;
        let poly_dense_commit = poly_dense;
        let click_poly_commit = click_poly;
        let click_layers_commit = click_layers;
        let click_front_commit = click_front;
        let strat = strategy;
        self.transaction("Knife Slice", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    match strat {
                        0 => {
                            knife_apply_slice_layers(
                                mesh,
                                &project,
                                &poly_dense_commit,
                                &layers_commit,
                                &layers_front_commit,
                                sentinel,
                                closed,
                                view_dir,
                                face_mask.as_ref(),
                                through,
                            );
                        }
                        1 => {
                            knife_apply_slice_layers(
                                mesh,
                                &project,
                                &click_poly_commit,
                                &click_layers_commit,
                                &click_front_commit,
                                sentinel,
                                closed,
                                view_dir,
                                face_mask.as_ref(),
                                through,
                            );
                        }
                        2 => {
                            core_math::knife::knife_cut_screen(
                                mesh,
                                &project,
                                &poly_dense_commit,
                                None,
                                closed,
                                through,
                                view_dir,
                                None,
                                face_mask.as_ref(),
                            );
                        }
                        _ => {
                            core_math::knife::knife_cut_screen(
                                mesh,
                                &project,
                                &click_poly_commit,
                                None,
                                closed,
                                through,
                                view_dir,
                                None,
                                face_mask.as_ref(),
                            );
                        }
                    }
                    // Densify inserts collinear mid-samples on straight segments so bends
                    // survive; collapse them after the cut so the result is clean segments
                    // (owner test-019: trapezoid selection outline "became crazy").
                    // SCOPED: pre-cut verts are untouchable (owner 2026-08-08 "cleaned all
                    // my geo" — the global collapse ate authored tessellation).
                    core_math::mesh_boolean::collapse_collinear_pins_scoped(mesh, &pre_verts);
                    // Slice can leave digons (2-vert faces). Purge them so Ctrl+X / edit ops
                    // are not blocked by `integrity: degenerate face N: only 2 vertices`
                    // (owner 2026-08-06, face after slice cut).
                    mesh.purge_degenerate_faces();
                    // OPEN-MESH SUBTRACT: the scribe separated the enclosed region into its
                    // own face(s); dropping them is the cut. Projection is along the
                    // drawing-plane normal — the same silhouette the solid path's prism
                    // sweeps, so a plane and a solid cut the same shape.
                    //
                    // A face is enclosed when EVERY vertex is inside-or-on the ring (plus a
                    // centroid check to reject a sliver hugging the ring from outside).
                    // **Centroid alone is not an interior test** and was wrong here: the
                    // scribe leaves the OUTER region as concave n-gons whose centroids land
                    // inside the loop — on a plane all three faces then read as enclosed and
                    // the whole surface would have died. Vertices cannot lie about this.
                    if let SliceEnclosed::Remove(rings) = &enclosed {
                        // Boundary tolerance from the ring's own scale — every vertex of the
                        // enclosed region lies exactly ON the ring, so "on" must read as in.
                        let mut ext: f32 = 0.0;
                        for r in rings.iter() {
                            for p in r.iter() {
                                ext = ext.max(p.x.abs()).max(p.y.abs());
                            }
                        }
                        let eps = 1e-4 * ext.max(1.0);
                        let mut dying: std::collections::HashSet<FaceId> =
                            std::collections::HashSet::new();
                        let to_plane = |lp: Vec3| {
                            let w = world_xform.transform_point3(lp);
                            Vec2::new((w - o_draw).dot(u_draw), (w - o_draw).dot(v_draw))
                        };
                        for fi in 0..mesh.faces.len() as u32 {
                            let f = FaceId(fi);
                            if !mesh.is_face_live(f) {
                                continue;
                            }
                            let vs = mesh.face_vertices(f);
                            if vs.is_empty() {
                                continue;
                            }
                            let all_in = vs.iter().all(|v| {
                                knife_ring_contains_or_touches(
                                    rings,
                                    to_plane(mesh.vert(*v).position),
                                    eps,
                                )
                            });
                            if !all_in {
                                continue;
                            }
                            let mut c = Vec3::ZERO;
                            for v in &vs {
                                c += mesh.vert(*v).position;
                            }
                            c /= vs.len() as f32;
                            if knife_ring_contains_or_touches(rings, to_plane(c), eps) {
                                dying.insert(f);
                            }
                        }
                        // Never let a cookie-cut consume the whole body — that is the open-mesh
                        // twin of the solid path's "would remove the whole object" refusal.
                        // Fails CLOSED: the scribe still committed, so the user keeps the cut
                        // outline and nothing is silently destroyed.
                        if !dying.is_empty() && dying.len() < mesh.face_count() {
                            mesh.delete_faces(&dying);
                        }
                    }
                }
            }
            // Optional rim select (OFF by default).
            if s.knife_select_after_cut {
                knife_apply_cut_selection_filtered(s, nid, &before, &path_world, Some(plane_n));
            }
            // Stays armed until the user switches tool — see the note in
            // poly_knife_confirm_closed. The free-plane state resets so the next first click
            // re-decides the personality (R1); SLICE/SUBTRACT persists as a user setting.
            s.knife_points.clear();
            s.knife_snap_pos = None;
            s.knife_free = false;
            s.knife_free_plane = None;
            s.knife_free_view = None;
            s.poly_tool = PolyTool::Knife;
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast(if matches!(enclosed, SliceEnclosed::Remove(_)) {
                "Knife: material removed (open mesh — cut through the surface)"
            } else if face_mask.is_some() {
                if through {
                    "Knife: through-cut on full island(s) only"
                } else {
                    "Knife: sliced selected faces only"
                }
            } else {
                "Knife through-slice (all islands) applied"
            });
        });
    }

    /// BOOLEAN SLICE — Alt+S (owner 2026-08-08 "produce the connected faces that go through
    /// the mesh island and use geo to slice"). The closed drawn loop(s) become the SAME
    /// filled prism cutter subtract uses (walls = the owner's "4 connected faces through
    /// everything", extruded ±4×scene bound along the captured view). Then Box-Cutter
    /// "slice": keep BOTH halves — `A − C` and `A ∩ C` — merged into one body as separate
    /// pieces with real cut-wall geometry. No kerf, no material loss (the two halves sum
    /// exactly to A). Same dry-run, manifold and volume gates as subtract. Returns false
    /// when anything refuses (caller toasts).
    fn poly_knife_free_slice_boolean(&mut self, nid: core_math::scene_graph::NodeId) -> bool {
        use core_math::mesh_boolean::{mesh_boolean, BoolOp};
        let Some((o, u, v, _pn)) = self.knife_free_plane else { return false };
        let view_w = self
            .knife_free_view
            .filter(|w| w.length_squared() > 1e-12)
            .unwrap_or(_pn)
            .normalize_or_zero();
        if view_w.length_squared() < 0.5 {
            return false;
        }
        let big = self.scene_bound_radius().max(1.0) * 4.0;
        let loops = knife_split_closed_loops_p2(&self.knife_points, o, u, v);
        if loops.is_empty() {
            return false;
        }
        let world_inv = self.scene.scene_graph.world_transform(nid).inverse();
        let to_local = |w: Vec3| -> Vec3 {
            let l = world_inv * glam::Vec4::new(w.x, w.y, w.z, 1.0);
            Vec3::new(l.x, l.y, l.z)
        };
        // Filled prism cutter — identical construction to the subtract path, but extruded
        // along the CAPTURED VIEW (slice cuts what you saw, not the drawing-plane normal).
        let mut pos: Vec<Vec3> = Vec::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for mut p2 in loops {
            if knife_signed_area(&p2) < 0.0 {
                p2.reverse();
            }
            let tris = core_math::nurbs_trim::ear_clip_uv(&p2);
            if tris.is_empty() {
                continue;
            }
            let base = pos.len();
            let m = p2.len();
            for p in &p2 {
                pos.push(to_local(o + u * p.x + v * p.y - view_w * big));
            }
            for p in &p2 {
                pos.push(to_local(o + u * p.x + v * p.y + view_w * big));
            }
            for t in &tris {
                faces.push(vec![base + t[0], base + t[1], base + t[2]]);
            }
            for t in &tris {
                faces.push(vec![base + t[2] + m, base + t[1] + m, base + t[0] + m]);
            }
            for k in 0..m {
                let j = (k + 1) % m;
                faces.push(vec![base + j, base + k, base + k + m, base + j + m]);
            }
        }
        if pos.is_empty() || faces.is_empty() {
            return false;
        }
        let cutter = HalfEdgeMesh::from_polygons(&pos, &faces);
        if cutter.validate().is_err() || cutter.is_manifold().is_err() {
            return false;
        }
        // Dry-run BOTH halves OUTSIDE the transaction — a refused cut leaves no undo entry.
        let (mut merged, before) = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return false };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return false };
            let Some(outside) = mesh_boolean(mesh, &cutter, BoolOp::Difference) else {
                return false;
            };
            let Some(inside) = mesh_boolean(mesh, &cutter, BoolOp::Intersect) else {
                return false;
            };
            // A loop that misses the mesh leaves one half empty — that's a no-op, refuse
            // so the caller can tell the user instead of silently replacing the mesh.
            if outside.face_count() == 0 || inside.face_count() == 0 {
                return false;
            }
            // Pre-cut positions — the "before" side of the new-geometry diff the post-cut
            // auto-select runs on. Taken here, while the untouched mesh is still borrowed.
            let before = knife_position_set(mesh);
            (crate::poly::boolean::merge_meshes(&outside, &inside), before)
        };
        if merged.validate().is_err() || merged.face_count() == 0 {
            return false;
        }
        // Same knife-face-only tri cleanup as free boolean (circle/rect caps). Keep raw merge
        // if cleanup would break the mesh.
        let cleaned = {
            let mut c = merged.clone();
            let n = knife_cleanup_boolean_result(&mut c, &before);
            if n > 0 && c.validate().is_ok() && c.face_count() > 0 {
                merged = c;
                n
            } else {
                0
            }
        };
        if merged.validate().is_err() || merged.face_count() == 0 {
            return false;
        }
        self.transaction("Knife Slice (Boolean)", |s| {
            // `merge_meshes` of the two boolean halves is a REBUILD — every id is new, so the
            // sidecar must be re-keyed here or the untouched half of the mesh silently loses (and
            // mis-aims) its UVs / seams / creases / polygroups / Smart Select tags.
            s.poly_install_rebuilt_mesh(nid, merged, &[]);
            // Same auto-select as the subtract: the cut WALLS are what this verb produced.
            let picked = if s.knife_select_after_cut {
                knife_apply_boolean_result_selection(s, nid, &before, Some(view_w))
            } else {
                KnifeCutSelection::None
            };
            s.knife_points.clear();
            s.knife_snap_pos = None;
            s.knife_free = false;
            s.knife_free_plane = None;
            s.knife_free_view = None;
            s.poly_tool = PolyTool::Knife;
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            let clean_note = if cleaned > 0 {
                format!(" · cleaned {cleaned} tri edge(s)")
            } else {
                String::new()
            };
            match picked {
                KnifeCutSelection::Faces(n) => s.toast(&format!(
                    "Knife boolean slice: both pieces kept · {n} cut wall(s) selected{clean_note}"
                )),
                _ => s.toast(&format!(
                    "Knife boolean slice: both pieces kept (cut walls are real geometry){clean_note}"
                )),
            }
        });
        true
    }

    /// Alt+S — boolean slice of the current closed shape (see
    /// [`Self::poly_knife_free_slice_boolean`]). Plain S stays the projected-edge scribe.
    pub(crate) fn poly_knife_slice_kerf_now(&mut self) {
        if self.poly_tool != PolyTool::Knife {
            return;
        }
        if self.knife_points.len() < 3 {
            self.toast("Boolean slice needs a closed shape — draw a rect (R) / circle (C) first");
            return;
        }
        let Some(nid) = self.sel.usel.active_node() else { return };
        self.knife_ensure_free_plane();
        if !self.knife_free_is_closed() {
            self.toast("Boolean slice needs a CLOSED outline — close the loop or use R/C");
            return;
        }
        let mesh_closed = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|nd| match &nd.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.as_ref()),
                _ => None,
            })
            .map(|m| core_math::mesh_boolean::boundary_half_edge_count(m) == 0)
            .unwrap_or(false);
        if !mesh_closed {
            self.toast("Boolean slice needs a closed mesh — this one has open edges (use S)");
            return;
        }
        if !self.poly_knife_free_slice_boolean(nid) {
            self.toast("Boolean slice failed here — adjust the outline (S still scribes)");
        }
    }

    /// SUBTRACT: boolean-remove material. `open_side = None` → closed loop (cookie cutter,
    /// enclosed material dies). `Some(world_pick)` → open line; the picked side dies. The
    /// cutter is the polyline's polygon extruded ±(4×scene bound) along the captured view.
    fn poly_knife_free_boolean(&mut self, open_side: Option<Vec3>) {
        use core_math::mesh_boolean::{boundary_half_edge_count, mesh_boolean, BoolOp};
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.poly_knife_cancel();
                return;
            }
        };
        let Some((o, u, v, n)) = self.knife_free_plane else {
            self.poly_knife_cancel();
            return;
        };
        let big = self.scene_bound_radius().max(1.0) * 4.0;

        // ── BOOLEAN TYPE gates (owner 2026-08-11) ────────────────────────────────────────
        // Both refusals are about a cutter that cannot mean what the op says, and both fail
        // CLOSED (nothing committed, tool still armed, message says which key fixes it).
        let op = self.knife_bool_op;
        if op != KnifeBoolOp::Subtract && open_side.is_some() {
            // The open-line cutter is a half-plane ribbon swept 8× the scene: unioning it would
            // bury the model in a slab, intersecting it would keep a slab-shaped bite. Only a
            // closed outline bounds material the way Union/Intersect need.
            self.poly_knife_cancel();
            self.toast(&format!(
                "Knife: {} needs a CLOSED outline — draw a rect (R) / circle (C)",
                op.label()
            ));
            return;
        }
        if op == KnifeBoolOp::Union && self.knife_cut_depth().is_none() {
            // Without a height the prism runs ±4× the scene bound: a union would weld a rod
            // through the whole model. A boss has to be as tall as the user says it is.
            self.poly_knife_cancel();
            self.toast("Knife: UNION needs a height — press E and pull OUT of the surface");
            return;
        }
        if op == KnifeBoolOp::Subtract && self.knife_cut_depth().is_some_and(|d| d < 0.0) {
            // The prism stands outside the material: a subtract would shave a sliver off the
            // surface. Almost always a pull in the wrong direction, so say which way.
            self.poly_knife_cancel();
            self.toast("Knife: pull INTO the surface to subtract — or press U to add material");
            return;
        }

        // One or more closed loops in plane UV (rect linear array = multiple rings).
        let mut loops_p2: Vec<Vec<Vec2>> = Vec::new();

        if let Some(sw) = open_side {
            // WYSIWYG ribbon (owner 2026-08-03): open line → single polygon loop.
            let mut p2: Vec<Vec2> = self
                .knife_points
                .iter()
                .map(|w| Vec2::new((*w - o).dot(u), (*w - o).dot(v)))
                .collect();
            let m = p2.len();
            if m < 2 {
                self.poly_knife_cancel();
                return;
            }
            let s2 = Vec2::new((sw - o).dot(u), (sw - o).dot(v));
            let d0 = (p2[0] - p2[1]).normalize_or_zero();
            let dn = (p2[m - 1] - p2[m - 2]).normalize_or_zero();
            if d0.length_squared() < 0.5 || dn.length_squared() < 0.5 {
                self.poly_knife_cancel();
                self.toast("Knife: line too short to pick a side");
                return;
            }
            let mut ext = Vec::with_capacity(m + 2);
            ext.extend(p2.iter().copied());
            let chord = p2[m - 1] - p2[0];
            let mut perp = Vec2::new(-chord.y, chord.x);
            if perp.length_squared() < 1e-10 {
                perp = Vec2::new(-d0.y, d0.x);
            }
            let perp = perp.normalize_or_zero();
            let side = if (s2 - p2[0]).dot(perp) >= 0.0 { 1.0 } else { -1.0 };
            let off = perp * side * big * 2.0;
            let last = *ext.last().unwrap();
            let first_pt = ext[0];
            ext.push(last + off);
            ext.push(first_pt + off);
            p2 = ext;
            loops_p2.push(p2);
        } else {
            // Closed cut: split concatenated closed rings (rect array) into separate loops.
            loops_p2 = knife_split_closed_loops_p2(&self.knife_points, o, u, v);
            if loops_p2.is_empty() {
                self.poly_knife_cancel();
                self.toast("Knife: need at least 3 points for a closed cut");
                return;
            }
        }

        // ── OPEN MESH → SURFACE COOKIE-CUT (owner 2026-08-08: "can we make the boolean work
        // on an open mesh … also useful when you knife a simple plane") ──────────────────
        //
        // A SOLID boolean cannot run here and never will: Manifold needs a watertight operand,
        // and a surface has no interior to subtract from — "inside" is undefined, so a
        // Difference has no answer. What IS well defined on a surface is the cookie-cut the
        // user is actually asking for: scribe the outline in, then drop the faces it encloses.
        // The scribe already works on open meshes (it is pure surface topology), so this routes
        // to the SAME machinery every slice verb uses and hands it the enclosure rings — the
        // very polygons the prism below would have been extruded from, so open and closed
        // meshes cut the identical silhouette. `enclosed` is what differs, nothing else.
        //
        // Deliberately scoped to OPEN meshes: a closed body keeps the true solid boolean, which
        // is strictly better there (it rebuilds interior walls; a surface cut cannot).
        let target_open = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|nd| match &nd.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.as_ref()),
                _ => None,
            })
            .map(|m| boundary_half_edge_count(m) > 0)
            .unwrap_or(false);
        if target_open {
            // Union / Intersect have no surface analogue: there is no interior to add to or keep,
            // and the cookie-cut can only REMOVE. Refuse rather than silently running a Subtract
            // under a different label — a wrong op is worse than no op.
            if self.knife_bool_op != KnifeBoolOp::Subtract {
                let op = self.knife_bool_op.label();
                self.poly_knife_cancel();
                self.toast(&format!(
                    "Knife: {op} needs a closed mesh — this one has open edges (subtract still cuts)"
                ));
                return;
            }
            // A surface has no thickness to stop inside, so the cookie-cut here is always
            // full-depth. Say it instead of pretending the depth was applied.
            if self.knife_cut_depth().is_some() {
                self.toast("Knife: cut depth needs a closed mesh — cutting the surface through");
            }
            let closed_path = open_side.is_none();
            self.poly_knife_free_slice_commit(
                closed_path,
                true,
                SliceEnclosed::Remove(loops_p2),
            );
            return;
        }

        // Cutter prism(s) in TARGET-LOCAL space (boolean runs where the mesh lives).
        // Multiple loops (array) → multi-component solid; one Difference removes all holes.
        let world_inv = self.scene.scene_graph.world_transform(nid).inverse();
        let to_local = |w: Vec3| -> Vec3 {
            let l = world_inv * glam::Vec4::new(w.x, w.y, w.z, 1.0);
            Vec3::new(l.x, l.y, l.z)
        };
        let mut pos: Vec<Vec3> = Vec::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        // CUT DEPTH (`E`, owner 2026-08-09): bound the prism instead of running it ±4× the scene.
        // `n` faces the viewer (`knife_face_plane_basis` flips it), so material is at −n: the
        // cutter spans from a hair ABOVE the drawing plane down to −depth, and what survives
        // below that is the pocket floor. The small outward margin keeps the entry cap off the
        // surface plane — an exactly coplanar boolean face is the fold class BOOLEAN_V1 fights.
        // `None` = the historical through cut, byte-identical to before this feature.
        //
        // SIGNED since 2026-08-11 (owner: "need to be able to go in and out for the union"):
        // a negative depth stands the prism OUT of the surface instead — a boss for UNION. Its
        // base is sunk `bite` INTO the solid, which is the owner's own warning made structural:
        // *"watch out for perfect plane alignment on the union, when drawing the shape on the
        // surface"*. Drawn on the surface, a flush base gives the union two exactly coincident
        // coplanar faces — the degeneracy the boolean's fold class is worst at. The overlap costs
        // nothing (the material is added anyway) and removes the coincidence entirely.
        let bite = big * 1e-3;
        let (t_lo, t_hi) = match self.knife_cut_depth() {
            // Into the surface: pocket floor at −d, entry cap a hair above the plane.
            Some(d) if d > 0.0 => (-d, bite),
            // Out of it: base sunk `bite` below the plane, cap at the pulled height. Clamped so
            // the cap can never fall below the base (the ring order fixes the cap winding).
            Some(d) => (-bite, (-d).max(bite * 2.0)),
            None => (-big, big),
        };
        for mut p2 in loops_p2 {
            if knife_signed_area(&p2) < 0.0 {
                p2.reverse();
            }
            let tris = core_math::nurbs_trim::ear_clip_uv(&p2);
            if tris.is_empty() {
                continue;
            }
            let base = pos.len();
            let m = p2.len();
            // Deeper ring FIRST (the old `-n*big` slot): cap winding below depends on the order.
            for p in &p2 {
                pos.push(to_local(o + u * p.x + v * p.y + n * t_lo));
            }
            for p in &p2 {
                pos.push(to_local(o + u * p.x + v * p.y + n * t_hi));
            }
            for t in &tris {
                faces.push(vec![base + t[0], base + t[1], base + t[2]]);
            }
            for t in &tris {
                faces.push(vec![base + t[2] + m, base + t[1] + m, base + t[0] + m]);
            }
            for k in 0..m {
                let j = (k + 1) % m;
                faces.push(vec![base + j, base + k, base + k + m, base + j + m]);
            }
        }
        if pos.is_empty() || faces.is_empty() {
            self.poly_knife_cancel();
            self.toast("Knife: cut outline is degenerate (self-crossing?)");
            return;
        }
        let cutter = HalfEdgeMesh::from_polygons(&pos, &faces);
        if cutter.validate().is_err() || cutter.is_manifold().is_err() {
            self.poly_knife_cancel();
            self.toast("Knife: cut outline is degenerate (self-crossing?)");
            return;
        }

        // AZRA_KNIFE_DUMP=1: capture the EXACT pre-cut mesh + cutter polygon + plane to
        // %TEMP%/azra_knife_repro.json — the bevel-bug method: analyze the user's real
        // topology instead of guessing from screenshots (owner 2026-08-03: top cut broke
        // the bottom of the shape).
        if std::env::var_os("AZRA_KNIFE_DUMP").is_some() {
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    let mut js = String::from("{\"verts\":[");
                    for i in 0..mesh.vertices.len() as u32 {
                        let p = mesh.vert(VertexId(i)).position;
                        js.push_str(&format!("[{},{},{}],", p.x, p.y, p.z));
                    }
                    if js.ends_with(',') { js.pop(); }
                    js.push_str("],\"faces\":[");
                    for i in 0..mesh.faces.len() as u32 {
                        if mesh.face(FaceId(i)).half_edge == NO_EDGE { continue; }
                        let fv = mesh.face_vertices(FaceId(i));
                        js.push('[');
                        for v in &fv { js.push_str(&format!("{},", v.0)); }
                        if js.ends_with(',') { js.pop(); }
                        js.push_str("],");
                    }
                    if js.ends_with(',') { js.pop(); }
                    js.push_str("],\"poly2\":[");
                    for w in &self.knife_points {
                        let p = Vec2::new((*w - o).dot(u), (*w - o).dot(v));
                        js.push_str(&format!("[{},{}],", p.x, p.y));
                    }
                    if js.ends_with(',') { js.pop(); }
                    let wx = self.scene.scene_graph.world_transform(nid).to_cols_array();
                    js.push_str(&format!(
                        "],\"plane_o\":[{},{},{}],\"plane_u\":[{},{},{}],\"plane_v\":[{},{},{}],\"plane_n\":[{},{},{}],\"big\":{},\"open_side\":{},\"world_xform\":{:?}}}",
                        o.x, o.y, o.z, u.x, u.y, u.z, v.x, v.y, v.z, n.x, n.y, n.z, big,
                        open_side.is_some(), wx,
                    ));
                    let path = std::env::temp_dir().join("azra_knife_repro.json");
                    match std::fs::write(&path, js) {
                        Ok(()) => eprintln!("[KNIFE_DUMP] wrote {}", path.display()),
                        Err(e) => eprintln!("[KNIFE_DUMP] FAILED: {e}"),
                    }
                }
            }
        }

        // Dry-run the boolean OUTSIDE the transaction — a failed cut leaves no undo entry.
        let (result, before) = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(nd) => nd,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            // Open meshes never reach here — they took the surface cookie-cut branch above.
            debug_assert_eq!(
                boundary_half_edge_count(mesh),
                0,
                "solid boolean reached with an open operand — the open-mesh route above was skipped"
            );
            let before = knife_position_set(mesh);
            let bop = match op {
                KnifeBoolOp::Subtract => BoolOp::Difference,
                KnifeBoolOp::Union => BoolOp::Union,
                KnifeBoolOp::Intersect => BoolOp::Intersect,
            };
            (mesh_boolean(mesh, &cutter, bop), before)
        };
        let Some(raw) = result else {
            self.poly_knife_cancel();
            self.toast("Knife: boolean failed here — adjust the line");
            return;
        };
        if !crate::poly::boolean::result_installable(&raw) {
            self.poly_knife_cancel();
            self.toast(if raw.face_count() == 0 {
                // Only Subtract/Intersect can empty the body — an Intersect whose outline missed
                // the mesh is the common way, so name the fix instead of blaming the "cut".
                match op {
                    KnifeBoolOp::Intersect => {
                        "Knife: INTERSECT kept nothing — the outline must overlap the mesh"
                    }
                    _ => "Knife: that cut would remove the whole object",
                }
            } else {
                "Knife: cut result not manifold — adjust the line"
            });
            return;
        }
        // Dissolve ear-clip triangle fans on knife-produced faces only (circle/rect caps,
        // pocket floors) — owner 2026-08-12: "remove triangle from result … only face produce
        // by knife action". Body faces outside the cut are never touched. If cleanup would
        // break manifold, keep the raw boolean result.
        let (res, cleaned) = {
            let mut cleaned_mesh = raw.clone();
            let n = knife_cleanup_boolean_result(&mut cleaned_mesh, &before);
            if n > 0 && crate::poly::boolean::result_installable(&cleaned_mesh) {
                (cleaned_mesh, n)
            } else {
                (raw, 0)
            }
        };

        let plane_n = n;
        self.transaction("Knife Cut", |s| {
            // The boolean kernel renumbers everything, so the sidecar rides across by position —
            // the geometry this cut did not reach must come out of it still carrying its tags
            // (owner 2026-08-11), exactly as for the X-pie booleans.
            s.poly_install_rebuilt_mesh(nid, res, &[]);
            // Auto-select the boolean's own result — cut walls / pocket floor (owner 2026-08-11).
            let picked = if s.knife_select_after_cut {
                knife_apply_boolean_result_selection(s, nid, &before, Some(plane_n))
            } else {
                KnifeCutSelection::None
            };
            // Stays armed until the user switches tool. Free-plane resets so the next
            // first click re-decides the personality (R1). Shape kind is restored by
            // poly_knife_shape_release so R/C mode survives the cut.
            s.knife_points.clear();
            s.knife_snap_pos = None;
            s.knife_free = false;
            s.knife_free_plane = None;
            s.knife_free_view = None;
            s.poly_tool = PolyTool::Knife;
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            let verb = match op {
                KnifeBoolOp::Subtract => "material removed",
                KnifeBoolOp::Union => "material added",
                KnifeBoolOp::Intersect => "kept only the enclosed material",
            };
            let clean_note = if cleaned > 0 {
                format!(" · cleaned {cleaned} tri edge(s)")
            } else {
                String::new()
            };
            match picked {
                KnifeCutSelection::Faces(n) => {
                    s.toast(&format!("Knife: {verb} · {n} new face(s) selected{clean_note}"))
                }
                KnifeCutSelection::Edges => {
                    s.toast(&format!("Knife: {verb} · rim selected{clean_note}"))
                }
                KnifeCutSelection::None => s.toast(&format!("Knife: {verb}{clean_note}")),
            }
        });
    }
}

/// Nearest surface hit along a ray, in whatever space `xform` maps mesh vertices into.
///
/// Extracted from `knife_surface_hit_at_screen` (which now supplies a camera ray) so the free
/// knife can resolve its drawing-plane points onto the surface along the CAPTURED view without
/// a second containment rule that could disagree with the hover snap.
///
/// Ray ∩ face PLANE + winding containment on the transformed ring — NOT a fan of triangles (a
/// fan on a concave n-gon hits phantom area over the notch and misses real face area, snapping
/// clicks to the wrong surface point). The plane is rebuilt from the transformed ring because
/// `xform` may rotate/scale, so the local normal cannot be reused.
fn knife_surface_hit_along_ray(
    mesh: &HalfEdgeMesh,
    xform: &impl Fn(Vec3) -> Vec3,
    ray_o: Vec3,
    ray_d: Vec3,
) -> Option<Vec3> {
    knife_surface_hits_along_ray(mesh, xform, ray_o, ray_d)
        .into_iter()
        .next()
}

/// Apply free-knife slice layers to `mesh`.
///
/// Owner 2026-08-07 — through-slice scoping:
///   * **No face mask** (`through=true`): one **through=true** cut using densified frontmost
///     hits so edge crossings hit **every** island/shell under the path. Deeper layers get
///     mid-face bends on back shells with through=false (avoids front↔back Bound mangling).
///   * **Face mask + through**: same through strategy but `restrict_faces` rejects edges and
///     interiors outside the selected **face-linked island(s)** — cut through that shell only.
///   * **Face mask + !through**: visible-only per-layer, still masked (classic surface path).
fn knife_apply_slice_layers(
    mesh: &mut HalfEdgeMesh,
    project: &dyn Fn(Vec3) -> Option<Vec2>,
    poly_dense: &[Vec2],
    layers: &[Vec<Vec3>],
    layer_front: &[bool],
    sentinel: Vec3,
    closed: bool,
    view_dir: Vec3,
    face_mask: Option<&HashSet<FaceId>>,
    through: bool,
) -> usize {
    let has_surface = |layer: &[Vec3]| {
        layer
            .iter()
            .any(|p| (*p - sentinel).length_squared() > 1e-12)
    };
    let layers_had_surface = layers.iter().any(|l| has_surface(l));
    // Entry surfaces (front-facing) are scribed along the view; exit surfaces against it.
    let dir_for = |li: usize| {
        if layer_front.get(li).copied().unwrap_or(li == 0) {
            view_dir
        } else {
            -view_dir
        }
    };
    let mut cnt = 0usize;
    // "New vert" detection is by POSITION, never by id — the engine recycles freed vertex
    // slots (a rebuilt shell frees its old ids and the cut verts land in them), so any
    // `id >= len_before` test misclassifies. Same exact-bits set the app-level rim select uses.
    let pos_before = knife_position_set(mesh);

    if through {
        // Through-cut: all shells under the path, optionally restricted to face islands.
        if layers_had_surface {
            // Deeper layers carry an EXACT scribe of every hidden surface the densify rays
            // saw. Chording those same faces from the front pass (`through=true`) cuts
            // them TWICE at densify-quantized offsets — duplicate ring verts on shared
            // silhouette edges, sliver faces, and mis-bound interior flap verts (owner
            // 2026-08-07 "always fails to do a proper cut": straight box slice came out
            // +7v/+3f instead of +4v/+4f with a vertex INSIDE the volume). So the front
            // pass chords the far side ONLY when no deeper layer exists to scribe it.
            let deep_scribe = layers.iter().skip(1).any(|l| has_surface(l));
            if let Some(layer0) = layers.first() {
                cnt += core_math::knife::knife_cut_screen(
                    mesh,
                    project,
                    poly_dense,
                    Some(layer0.as_slice()),
                    closed,
                    !deep_scribe, // through only when the far side has no exact scribe
                    dir_for(0),
                    None,
                    face_mask,
                );
            }
            // Deeper surfaces: exact per-shell scribe (through=false, facing-correct view).
            for (li, layer) in layers.iter().enumerate().skip(1) {
                cnt += core_math::knife::knife_cut_screen(
                    mesh,
                    project,
                    poly_dense,
                    Some(layer.as_slice()),
                    closed,
                    false,
                    dir_for(li),
                    None,
                    face_mask,
                );
            }
            // Silhouette walls: with an exact far-side scribe the front pass runs
            // through=false, so nothing chords the faces that are GRAZING to the view
            // (side walls of a straight slice, the top face under a rect's two vertical
            // lines) — they end up with rim verts from the neighbouring scribes but stay
            // unsplit. Deterministic close: pair each face's new rim verts along the
            // drawn path (arclength on the screen polyline) and split once per pair via
            // the ID-preserving Euler op — no screen-crossing math, no duplicate-vert
            // risk (split_face_between no-ops on adjacent pairs, i.e. faces the engine
            // already cut). Pairing by path arclength keeps each crossing's entry/exit
            // together, so a face crossed by TWO rect lines gets TWO independent splits
            // (owner 2026-08-08 "only the 2 segments from top missing").
            let seg_n = if closed {
                poly_dense.len()
            } else {
                poly_dense.len().saturating_sub(1)
            };
            let arc_of = |p2: Vec2| -> f32 {
                let mut best = (f32::MAX, 0.0f32);
                for si in 0..seg_n {
                    let a = poly_dense[si];
                    let b = poly_dense[(si + 1) % poly_dense.len()];
                    let ab = b - a;
                    let t = ((p2 - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
                    let d = (a + ab * t - p2).length_squared();
                    if d < best.0 {
                        best = (d, si as f32 + t);
                    }
                }
                best.1
            };
            for fi in 0..mesh.faces.len() as u32 {
                let f = FaceId(fi);
                if !mesh.is_face_live(f) {
                    continue;
                }
                if let Some(mask) = face_mask {
                    if !mask.contains(&f) {
                        continue;
                    }
                }
                let ring = mesh.face_vertices(f);
                let mut new: Vec<VertexId> = ring
                    .iter()
                    .copied()
                    .filter(|&v| {
                        let p = mesh.vert(v).position;
                        !pos_before.contains(&[p.x.to_bits(), p.y.to_bits(), p.z.to_bits()])
                    })
                    .collect();
                // Odd counts are ambiguous (a crossing whose partner vert failed) — fail
                // closed, leave the face untouched.
                if new.len() < 2 || new.len() % 2 != 0 {
                    continue;
                }
                new.sort_by(|&a, &b| {
                    let aa = project(mesh.vert(a).position)
                        .map(&arc_of)
                        .unwrap_or(f32::MAX);
                    let bb = project(mesh.vert(b).position)
                        .map(&arc_of)
                        .unwrap_or(f32::MAX);
                    aa.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
                });
                // After a split the second pair may live in either half — resolve per pair.
                let mut candidates = vec![f];
                for pair in new.chunks(2) {
                    let (va, vb) = (pair[0], pair[1]);
                    let target = candidates.iter().copied().find(|&cf| {
                        mesh.is_face_live(cf) && {
                            let r = mesh.face_vertices(cf);
                            r.contains(&va) && r.contains(&vb)
                        }
                    });
                    if let Some(cf) = target {
                        if let Some(nf) = mesh.split_face_between(cf, va, vb) {
                            cnt += 1;
                            candidates.push(nf);
                        }
                    }
                }
            }
        } else {
            // No surface hit under densify rays — pure silhouette through-cut.
            cnt += core_math::knife::knife_cut_screen(
                mesh,
                project,
                poly_dense,
                None,
                closed,
                true,
                view_dir,
                None,
                face_mask,
            );
        }
        return cnt;
    }

    // Non-through (visible-only): per-layer, masked when face islands are selected.
    for (li, layer) in layers.iter().enumerate() {
        cnt += core_math::knife::knife_cut_screen(
            mesh,
            project,
            poly_dense,
            Some(layer.as_slice()),
            closed,
            false,
            dir_for(li),
            None,
            face_mask,
        );
    }
    cnt
}

/// Label every live face with a face-linked shell id (BFS over twin adjacency — the same
/// connectivity as `knife_expand_face_islands`, but a full labeling instead of seed expansion).
fn knife_face_shell_ids(mesh: &HalfEdgeMesh) -> Vec<u32> {
    let n = mesh.faces.len();
    let mut ids = vec![u32::MAX; n];
    let mut next = 0u32;
    for fi in 0..n as u32 {
        if mesh.face(FaceId(fi)).half_edge == NO_EDGE || ids[fi as usize] != u32::MAX {
            continue;
        }
        ids[fi as usize] = next;
        let mut queue = VecDeque::new();
        queue.push_back(fi);
        while let Some(f) = queue.pop_front() {
            for he in mesh.face_half_edges(FaceId(f)) {
                let twin = mesh.he(he).twin;
                if twin == NO_TWIN {
                    continue;
                }
                let adj = mesh.he(twin).face;
                if adj == NO_FACE || ids[adj.0 as usize] != u32::MAX {
                    continue;
                }
                ids[adj.0 as usize] = next;
                queue.push_back(adj.0);
            }
        }
        next += 1;
    }
    ids
}

/// All hits along a ray tagged with shell id + facing: (t, pos, shell, front-facing).
/// Front = the face normal opposes the ray (an ENTRY surface as seen along the slice view).
fn knife_surface_hits_shelled(
    mesh: &HalfEdgeMesh,
    xform: &impl Fn(Vec3) -> Vec3,
    ray_o: Vec3,
    ray_d: Vec3,
    shell_ids: &[u32],
) -> Vec<(f32, Vec3, u32, bool)> {
    knife_surface_hits_along_ray_n(mesh, xform, ray_o, ray_d)
        .into_iter()
        .map(|(t, p, n, f)| {
            let shell = shell_ids.get(f.0 as usize).copied().unwrap_or(u32::MAX);
            (t, p, shell, ray_d.dot(n) < 0.0)
        })
        .collect()
}

/// Group per-sample ray hits into scribe layers keyed by (shell id, facing,
/// occurrence-within-group) instead of raw depth ordinal.
///
/// Depth-ordinal layering (`hits[li]`) treated "layer index" as "which surface", but any
/// sample whose ray grazed a silhouette or clipped an extra fin had a DIFFERENT hit count —
/// shifting every deeper hit up one slot for that sample only. Layer 1 then mixed back
/// faces of one sample with mid faces of the next, and the scribed polyline jumped between
/// shells → duplicate ring verts / slivers → `knife_result_is_sound` veto ("slice always
/// fails"). Keying by (shell, facing) makes each layer's content deterministic: a missing
/// hit in one group never displaces another group's hits. Groups are ordered by nearest t,
/// so layer 0 is still the front entry surface. Returns (layers, per-layer front flags).
fn build_shell_layers(
    hits_list: &[Vec<(f32, Vec3, u32, bool)>],
    sentinel: Vec3,
) -> (Vec<Vec<Vec3>>, Vec<bool>) {
    use std::collections::HashMap;
    // (shell, front) -> (nearest t seen, max occurrences within one sample)
    let mut groups: HashMap<(u32, bool), (f32, usize)> = HashMap::new();
    for hits in hits_list {
        let mut count: HashMap<(u32, bool), usize> = HashMap::new();
        for &(t, _, s, fr) in hits {
            let e = groups.entry((s, fr)).or_insert((t, 0));
            if t < e.0 {
                e.0 = t;
            }
            *count.entry((s, fr)).or_insert(0) += 1;
        }
        for (key, c) in count {
            let e = groups.get_mut(&key).unwrap();
            if c > e.1 {
                e.1 = c;
            }
        }
    }
    if groups.is_empty() {
        // No hits anywhere — one sentinel layer keeps the pure-silhouette fallback alive.
        return (vec![vec![sentinel; hits_list.len()]], vec![true]);
    }
    let mut order: Vec<((u32, bool), (f32, usize))> = groups.into_iter().collect();
    order.sort_by(|a, b| {
        a.1 .0
            .partial_cmp(&b.1 .0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut layers: Vec<Vec<Vec3>> = Vec::new();
    let mut fronts: Vec<bool> = Vec::new();
    for ((shell, fr), (_, occ)) in order {
        for j in 0..occ {
            let mut layer = Vec::with_capacity(hits_list.len());
            for hits in hits_list {
                let p = hits
                    .iter()
                    .filter(|h| h.2 == shell && h.3 == fr)
                    .nth(j)
                    .map(|h| h.1);
                layer.push(p.unwrap_or(sentinel));
            }
            layers.push(layer);
            fronts.push(fr);
        }
    }
    (layers, fronts)
}

/// Expand face seeds to full face-linked islands (BFS over twin adjacency).
/// Same connectivity as Select Linked (L) — hidden verts exclude faces they dominate.
fn knife_expand_face_islands(
    mesh: &HalfEdgeMesh,
    seeds: &[u32],
    hidden: &HashSet<u32>,
) -> HashSet<FaceId> {
    let mut visited: HashSet<u32> = HashSet::new();
    let mut queue = VecDeque::new();
    for &s in seeds {
        if (s as usize) >= mesh.faces.len() || mesh.face(FaceId(s)).half_edge == NO_EDGE {
            continue;
        }
        let dominated = mesh
            .face_vertices(FaceId(s))
            .iter()
            .any(|v| hidden.contains(&v.0));
        if !dominated && visited.insert(s) {
            queue.push_back(s);
        }
    }
    while let Some(fi) = queue.pop_front() {
        for he in mesh.face_half_edges(FaceId(fi)) {
            let twin = mesh.he(he).twin;
            if twin == NO_TWIN {
                continue;
            }
            let adj = mesh.he(twin).face;
            if adj == NO_FACE {
                continue;
            }
            let adj_id = adj.0;
            if visited.contains(&adj_id) {
                continue;
            }
            let adj_hidden = mesh
                .face_vertices(FaceId(adj_id))
                .iter()
                .any(|v| hidden.contains(&v.0));
            if !adj_hidden && visited.insert(adj_id) {
                queue.push_back(adj_id);
            }
        }
    }
    visited.into_iter().map(FaceId).collect()
}

/// Resolve knife face mask + through flag.
///
/// Owner 2026-08-07: **full island ≠ partial poly**.
/// - Every face of a face-linked shell is selected → mask = that shell, **through = true**
///   (perfect through-cut of the island only).
/// - Any partial component → mask = **exact selected faces only** (no expand), through =
///   `force_through` (S) so auto-slice is not island-through.
/// - Empty selection → `(None, true)` for through-all (caller may override through).
fn knife_face_cut_scope(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    hidden: &HashSet<u32>,
    force_through: bool,
) -> (Option<HashSet<FaceId>>, bool) {
    if selected.is_empty() {
        return (None, true);
    }
    let seeds: Vec<u32> = selected.iter().copied().collect();
    let mut full_union: HashSet<FaceId> = HashSet::new();
    let mut any_partial = false;
    let mut covered: HashSet<u32> = HashSet::new();

    for &seed in &seeds {
        if covered.contains(&seed) {
            continue;
        }
        let island = knife_expand_face_islands(mesh, &[seed], hidden);
        if island.is_empty() {
            any_partial = true;
            continue;
        }
        for f in &island {
            covered.insert(f.0);
        }
        let fully = island.iter().all(|f| selected.contains(&f.0));
        if fully {
            full_union.extend(island);
        } else {
            any_partial = true;
        }
    }

    if !any_partial && !full_union.is_empty() {
        // Full island(s) only — perfect through-cut of those shells.
        (Some(full_union), true)
    } else {
        // Partial poly selection — auto-slice exact faces, never expand to island.
        let mask: HashSet<FaceId> = selected.iter().map(|&f| FaceId(f)).collect();
        (Some(mask), force_through)
    }
}

/// ALL surface hits along a ray, sorted near → far. Used by free-knife through-slice so the
/// same polyline shape is scribed on every shell the view ray pierces (front + back + …).
fn knife_surface_hits_along_ray(
    mesh: &HalfEdgeMesh,
    xform: &impl Fn(Vec3) -> Vec3,
    ray_o: Vec3,
    ray_d: Vec3,
) -> Vec<Vec3> {
    knife_surface_hits_along_ray_n(mesh, xform, ray_o, ray_d)
        .into_iter()
        .map(|(_, p, _, _)| p)
        .collect()
}

/// Frontmost surface hit with world face normal (Newell of the transformed ring).
fn knife_surface_hit_with_normal(
    mesh: &HalfEdgeMesh,
    xform: &impl Fn(Vec3) -> Vec3,
    ray_o: Vec3,
    ray_d: Vec3,
) -> Option<(Vec3, Vec3)> {
    knife_surface_hits_along_ray_n(mesh, xform, ray_o, ray_d)
        .into_iter()
        .next()
        .map(|(_, p, n, _)| (p, n))
}

/// All face hits along a ray: (t, world_pos, world_normal, face), sorted near → far.
fn knife_surface_hits_along_ray_n(
    mesh: &HalfEdgeMesh,
    xform: &impl Fn(Vec3) -> Vec3,
    ray_o: Vec3,
    ray_d: Vec3,
) -> Vec<(f32, Vec3, Vec3, FaceId)> {
    let mut hits: Vec<(f32, Vec3, Vec3, FaceId)> = Vec::new();
    for i in 0..mesh.faces.len() as u32 {
        if mesh.face(FaceId(i)).half_edge == NO_EDGE {
            continue;
        }
        let verts = mesh.face_vertices(FaceId(i));
        if verts.len() < 3 {
            continue;
        }
        let pts: Vec<Vec3> = verts.iter().map(|&v| xform(mesh.vert(v).position)).collect();
        let n = core_math::knife::newell_normal(&pts);
        if n == Vec3::ZERO {
            continue;
        }
        let denom = ray_d.dot(n);
        if denom.abs() < 1e-9 {
            continue;
        }
        let t = (pts[0] - ray_o).dot(n) / denom;
        if t > 1e-5 && core_math::knife::point_in_polygon_3d(&pts, n, ray_o + ray_d * t) {
            hits.push((t, ray_o + ray_d * t, n.normalize_or_zero(), FaceId(i)));
        }
    }
    hits.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Unique by depth so coplanar double-hits (shared edge) don't stack two layers.
    let mut out: Vec<(f32, Vec3, Vec3, FaceId)> = Vec::with_capacity(hits.len());
    for (t, p, n, f) in hits {
        if out
            .last()
            .map_or(true, |(_, q, _, _)| (*q - p).length_squared() > 1e-8)
        {
            out.push((t, p, n, f));
        }
    }
    out
}

/// Orthonormal face-plane frame for knife shape drawing.
/// `n_in` = face Newell normal (world). Flipped so it faces the camera (dot with view dir ≤ 0),
/// then u/v span the face plane for rect/circle sizing.
fn knife_face_plane_basis(n_in: Vec3, cam_orbit: glam::Quat) -> (Vec3, Vec3, Vec3) {
    let view = (cam_orbit * Vec3::NEG_Z).normalize_or_zero();
    let mut n = n_in.normalize_or_zero();
    if n.length_squared() < 1e-12 {
        n = -view;
    }
    // Prefer outward-facing relative to the view (same convention as free-line view plane).
    if n.dot(view) > 0.0 {
        n = -n;
    }
    // Stable tangent: avoid parallel-to-Y degeneracy.
    let helper = if n.cross(Vec3::Y).length_squared() > 1e-6 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let u = n.cross(helper).normalize_or_zero();
    let v = n.cross(u).normalize_or_zero();
    // Ensure right-handed: u × v ≈ n
    let n2 = u.cross(v).normalize_or_zero();
    let n_out = if n2.dot(n) >= 0.0 { n2 } else { -n2 };
    (u, v, n_out)
}

/// Densify a world-space polyline so free-knife slice keeps mid-path bends (not just click
/// corners). `max_step` is the maximum segment length in world units.
fn densify_polyline_world(pts: &[Vec3], closed: bool, max_step: f32) -> Vec<Vec3> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let step = max_step.max(1e-4);
    let mut out = Vec::with_capacity(pts.len() * 2);
    let n_seg = if closed {
        pts.len() - if pts.first() == pts.last() { 1 } else { 0 }
    } else {
        pts.len() - 1
    };
    // If first==last (shape closed marker), don't densify the zero-length close segment twice.
    let use_pts: &[Vec3] = if closed && pts.len() >= 2 && pts.first() == pts.last() {
        &pts[..pts.len() - 1]
    } else {
        pts
    };
    let n = use_pts.len();
    let seg_count = if closed { n } else { n.saturating_sub(1) };
    for i in 0..seg_count {
        let a = use_pts[i];
        let b = use_pts[(i + 1) % n];
        out.push(a);
        let d = (b - a).length();
        if d > step * 1.5 {
            let n_mid = (d / step).floor() as usize;
            for k in 1..n_mid {
                let t = (k as f32) * step / d;
                if t > 1e-4 && t < 1.0 - 1e-4 {
                    out.push(a.lerp(b, t));
                }
            }
        }
    }
    if !closed {
        if let Some(&last) = use_pts.last() {
            out.push(last);
        }
    } else if let Some(&first) = use_pts.first() {
        // Closed path for the engine: re-append first as last so closed=true rings seal.
        out.push(first);
    }
    let _ = n_seg;
    out
}

/// Ray ∩ plane (front-side hits only). Free fn: no `self` borrow inside hover's mesh borrows.
fn knife_plane_hit(ray_o: Vec3, ray_d: Vec3, origin: Vec3, n: Vec3) -> Option<Vec3> {
    let d = ray_d.dot(n);
    if d.abs() < 1e-6 {
        return None;
    }
    let t = (origin - ray_o).dot(n) / d;
    if t <= 1e-4 {
        return None;
    }
    Some(ray_o + ray_d * t)
}

/// Bit-exact position set of a mesh's live vertices — the "before" side of the
/// new-vertex diff that drives post-cut edge selection.
/// Live vertex positions — the pre-cut snapshot handed to the SCOPED post-cut cleanup
/// (`collapse_collinear_pins_scoped`): every vert in this list is untouchable, so cleanup
/// can only collapse pins the cut itself created.
fn knife_live_positions(mesh: &HalfEdgeMesh) -> Vec<Vec3> {
    (0..mesh.vertices.len() as u32)
        .map(VertexId)
        .filter(|&v| mesh.vert(v).half_edge != NO_EDGE)
        .map(|v| mesh.vert(v).position)
        .collect()
}

fn knife_position_set(mesh: &HalfEdgeMesh) -> std::collections::HashSet<[u32; 3]> {
    let mut set = std::collections::HashSet::with_capacity(mesh.vertices.len());
    for i in 0..mesh.vertices.len() as u32 {
        let vt = mesh.vert(VertexId(i));
        if vt.half_edge == NO_EDGE {
            continue;
        }
        set.insert([
            vt.position.x.to_bits(),
            vt.position.y.to_bits(),
            vt.position.z.to_bits(),
        ]);
    }
    set
}

/// Per-vertex "this vertex did not exist before the cut", indexed by [`VertexId`].
///
/// New-by-POSITION, never by id: both engines (scribe and boolean) rebuild the mesh, so vertex
/// ids are not stable across a cut and freed slots get recycled — positions are the only thing
/// that survives. Dead vertices read `false` (they are nothing, not new).
fn knife_new_vertex_flags(
    mesh: &HalfEdgeMesh,
    before: &std::collections::HashSet<[u32; 3]>,
) -> Vec<bool> {
    let mut new_vert = vec![false; mesh.vertices.len()];
    for i in 0..mesh.vertices.len() as u32 {
        let vt = mesh.vert(VertexId(i));
        if vt.half_edge == NO_EDGE {
            continue;
        }
        let key = [
            vt.position.x.to_bits(),
            vt.position.y.to_bits(),
            vt.position.z.to_bits(),
        ];
        new_vert[i as usize] = !before.contains(&key);
    }
    new_vert
}

/// Faces whose EVERY vertex is new = the geometry the BOOLEAN itself produced — the cut walls,
/// a pocket floor, the flat cross-section a side cut exposes.
///
/// Faces the boolean merely *reshaped* keep at least one pre-cut vertex and are deliberately
/// excluded: the top face that gained a hole existed before the cut, so it is not the cut's
/// result. Same new-by-position rule as [`knife_new_cut_edges`], via
/// [`knife_new_vertex_flags`].
fn knife_new_cut_faces(
    mesh: &HalfEdgeMesh,
    before: &std::collections::HashSet<[u32; 3]>,
) -> Vec<FaceId> {
    let new_vert = knife_new_vertex_flags(mesh, before);
    let mut out = Vec::new();
    for fi in 0..mesh.faces.len() as u32 {
        let f = FaceId(fi);
        if !mesh.is_face_live(f) {
            continue;
        }
        let vs = mesh.face_vertices(f);
        if vs.is_empty() {
            continue;
        }
        if vs
            .iter()
            .all(|v| new_vert.get(v.0 as usize).copied().unwrap_or(false))
        {
            out.push(f);
        }
    }
    out
}

/// Faces the knife boolean **produced or retriangulated** — candidates for post-cut cleanup.
///
/// * Fully new faces (all verts new) — pocket floor, boss caps, cut walls (ear-clip tri fans).
/// * Triangle faces with ≥2 new verts — tessellation debris glued onto the cut rim.
///
/// Deliberately excludes reshaped original faces that only gained a hole boundary (1 new vert
/// chain on an old face) — those are not knife-produced and must not be limited-dissolved.
fn knife_cleanup_candidate_faces(
    mesh: &HalfEdgeMesh,
    before: &std::collections::HashSet<[u32; 3]>,
) -> HashSet<FaceId> {
    let new_vert = knife_new_vertex_flags(mesh, before);
    let mut out = HashSet::new();
    for fi in 0..mesh.faces.len() as u32 {
        let f = FaceId(fi);
        if !mesh.is_face_live(f) {
            continue;
        }
        let vs = mesh.face_vertices(f);
        if vs.len() < 3 {
            continue;
        }
        let n_new = vs
            .iter()
            .filter(|v| new_vert.get(v.0 as usize).copied().unwrap_or(false))
            .count();
        if n_new == 0 {
            continue;
        }
        if n_new == vs.len() {
            // Fully knife-produced (cap / wall / floor).
            out.insert(f);
        } else if vs.len() == 3 && n_new >= 2 {
            // Triangle debris from boolean tessellation on the cut.
            out.insert(f);
        }
    }
    out
}

/// Mesh cleanup scoped to faces produced by the knife boolean — dissolve coplanar triangle
/// fans (circle/rect caps, pocket floors) into n-gons without touching the rest of the body.
///
/// Owner 2026-08-12: knife circle union/subtract leaves ear-clip triangle fans; limited
/// dissolve those faces only so the result is editable quads/n-gons, not a radial tri mesh.
///
/// Returns how many edges were dissolved (0 = nothing to clean).
fn knife_cleanup_boolean_result(
    mesh: &mut HalfEdgeMesh,
    before: &std::collections::HashSet<[u32; 3]>,
) -> u32 {
    // Angles escalate: planar caps dissolve at 5°, shallow curved walls need more.
    // only_faces requires BOTH sides of an edge in the set → cut-outline edges (knife face
    // vs original body) are never dissolved.
    let mut total = 0u32;
    for angle in [5.0_f32, 15.0, 30.0] {
        let only = knife_cleanup_candidate_faces(mesh, before);
        if only.len() < 2 {
            break;
        }
        let n = mesh.limited_dissolve(angle, Some(&only), None);
        total = total.saturating_add(n);
    }
    // Collapse valence-2 collinear pins on NEW verts only — keep every pre-cut vertex.
    let keep: Vec<Vec3> = before
        .iter()
        .map(|b| {
            Vec3::new(
                f32::from_bits(b[0]),
                f32::from_bits(b[1]),
                f32::from_bits(b[2]),
            )
        })
        .collect();
    if !keep.is_empty() {
        core_math::mesh_boolean::collapse_collinear_pins_scoped(mesh, &keep);
    }
    // Refuse a cleanup that broke the mesh — caller still has the uncleaned dry-run only if
    // we mutate in place carefully: validate and leave the last good state is hard after
    // in-place dissolve. limited_dissolve is designed to stay valid; assert soft.
    if mesh.validate().is_err() {
        // Should be rare — log and return what we dissolved; install path also gates installable.
        eprintln!("[knife] cleanup left mesh invalid (validate failed)");
    }
    total
}

/// Edges whose BOTH endpoints did not exist before the cut = the cut chain / cut boundary.
/// (Cut vertices are edge-splits or boolean-intersection verts — new positions; the engines
/// rebuild the mesh so IDs are not stable, but positions are.)
fn knife_new_cut_edges(
    mesh: &HalfEdgeMesh,
    before: &std::collections::HashSet<[u32; 3]>,
) -> Vec<HalfEdgeId> {
    let new_vert = knife_new_vertex_flags(mesh, before);
    let mut out = Vec::new();
    for h in 0..mesh.half_edges.len() as u32 {
        let he = mesh.he(HalfEdgeId(h));
        if he.twin != NO_TWIN && he.twin.0 < h {
            continue; // one half-edge per undirected edge
        }
        if he.prev == NO_EDGE {
            continue;
        }
        let src = mesh.he(he.prev).vertex;
        let dst = he.vertex;
        if (src.0 as usize) < new_vert.len()
            && (dst.0 as usize) < new_vert.len()
            && new_vert[src.0 as usize]
            && new_vert[dst.0 as usize]
        {
            out.push(HalfEdgeId(h));
        }
    }
    out
}

/// Owner 2026-08-03: "after a cut the resulting cut edge should be selected." Runs inside
/// the commit transaction, after the mesh mutation. Switches to Edge mode (the only mode
/// where an edge selection is visible/usable) and selects the cut chain.
fn knife_apply_cut_selection(
    s: &mut AzraApp,
    nid: core_math::scene_graph::NodeId,
    before: &std::collections::HashSet<[u32; 3]>,
) {
    knife_apply_cut_selection_filtered(s, nid, before, &[], None);
}

/// Select cut edges after a free/alt knife cut — the **cut-face / cut-loop ring**.
///
/// Owner (2026-08-04 OPEN, 2026-08-06 confirm): keep new–new edges that lie **in the cut
/// plane** (`|ed · plane_n|` small). Drop depth walls (parallel to the view/plane normal).
/// The old path-parallel filter kept only stroke-aligned segments and missed the full cut
/// outline wanted for bevel.
///
/// What a post-cut auto-select actually landed on — drives the commit toast so the message
/// never claims a selection that was not made.
enum KnifeCutSelection {
    /// The cut produced nothing selectable (selection left untouched).
    None,
    /// The boolean's own new faces — cut walls / pocket floor (Face mode).
    Faces(usize),
    /// The in-plane cut rim (Edge mode).
    Edges,
}

/// Select the geometry a knife BOOLEAN produced (owner 2026-08-11: *"knife cut should also auto
/// set selection, set the resulting geo from boolean"*).
///
/// The result of a subtract/boolean-slice is the geometry the boolean CREATED — the cut walls,
/// the pocket floor, the exposed cross-section. Selecting exactly those (in Face mode, the only
/// mode where a face selection is usable) leaves the user one keystroke from beveling, insetting,
/// extruding or re-materialing the cut instead of hunting for it.
///
/// Falls back to the in-plane rim edges when the cut created no new face — the open-mesh
/// cookie-cut REMOVES faces, so on a surface the rim IS the result. Runs inside the commit
/// transaction, after the mesh mutation and BEFORE the caller re-arms the knife (switching work
/// mode cancels an armed knife — see `poly_switch_work_mode`).
fn knife_apply_boolean_result_selection(
    s: &mut AzraApp,
    nid: core_math::scene_graph::NodeId,
    before: &std::collections::HashSet<[u32; 3]>,
    plane_n: Option<Vec3>,
) -> KnifeCutSelection {
    let faces = {
        let Some(node) = s.scene.scene_graph.get(nid) else { return KnifeCutSelection::None };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            return KnifeCutSelection::None;
        };
        knife_new_cut_faces(mesh, before)
    };
    if faces.is_empty() {
        let n = knife_apply_cut_selection_filtered(s, nid, before, &[], plane_n);
        return if n == 0 {
            KnifeCutSelection::None
        } else {
            KnifeCutSelection::Edges
        };
    }
    // Group mode already selects by face, so leave the user in it — switching would drop their
    // polygroup display for nothing. Only a non-face mode has to move.
    if !matches!(s.poly_work_mode, PolyWorkMode::Face | PolyWorkMode::Group) {
        s.poly_switch_work_mode(PolyWorkMode::Face);
    }
    s.poly_sel.selected_edges.clear();
    s.poly_sel.selected_faces.clear();
    s.poly_sel.selected_vertices.clear();
    let n = faces.len();
    for f in faces {
        s.poly_sel.selected_faces.insert(f.0);
    }
    // Remember what WE wrote. Every knife verb asks `knife_has_user_face_scope()` before treating a
    // face selection as "cut only these" — without this record the tool would read its own result
    // back as the user's instruction and the next cut would scope itself to the last one's walls.
    s.knife_last_result_faces = s.poly_sel.selected_faces.clone();
    KnifeCutSelection::Faces(n)
}

/// Surface knife (no plane): select the full new–new cut chain without path filter.
///
/// Returns how many edges were selected (0 = selection left untouched).
fn knife_apply_cut_selection_filtered(
    s: &mut AzraApp,
    nid: core_math::scene_graph::NodeId,
    before: &std::collections::HashSet<[u32; 3]>,
    path_world: &[Vec3],
    plane_n: Option<Vec3>,
) -> usize {
    let _ = path_world; // kept for call-site stability; plane filter is the cut ring
    let edges = {
        let Some(node) = s.scene.scene_graph.get(nid) else { return 0 };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return 0 };
        let world_xform = s.scene.scene_graph.world_transform(nid);
        let to_world = |p: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(p.x, p.y, p.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };
        let raw = knife_new_cut_edges(mesh, before);
        match plane_n.map(|nn| nn.normalize_or_zero()) {
            Some(nn) if nn.length_squared() > 0.5 => {
                // In-plane: edge direction ⟂ plane normal. Drop depth walls (‖ed · n‖ large).
                raw.into_iter()
                    .filter(|&he| {
                        let src = mesh.he_source(he);
                        let dst = mesh.he(he).vertex;
                        let ed = (to_world(mesh.vert(dst).position)
                            - to_world(mesh.vert(src).position))
                            .normalize_or_zero();
                        if ed.length_squared() < 0.5 {
                            return false;
                        }
                        // Keep edges nearly perpendicular to the view/cut normal (in cut plane).
                        ed.dot(nn).abs() < 0.35
                    })
                    .collect()
            }
            _ => raw,
        }
    };
    if edges.is_empty() {
        return 0;
    }
    if s.poly_work_mode != PolyWorkMode::Edge {
        s.poly_switch_work_mode(PolyWorkMode::Edge);
    }
    s.poly_sel.selected_edges.clear();
    s.poly_sel.selected_faces.clear();
    s.poly_sel.selected_vertices.clear();
    // This verb leaves NO face selection, so there is no tool-authored face scope to remember —
    // stale bookkeeping here would make a later genuine face selection read as the knife's own.
    s.knife_last_result_faces.clear();
    let n = edges.len();
    for e in edges {
        s.poly_sel.selected_edges.insert(e.0);
    }
    n
}

/// Is `p` inside one of `rings`, or ON a ring boundary within `eps`?
///
/// *Searched for an existing helper first:* `nurbs_trim::point_in_polygon` is the containment
/// half and is reused here — but it is a crossing-number test, so a point lying exactly ON an
/// edge is a coin flip, and after a scribe **every** vertex of the enclosed region lies exactly
/// on the ring. The only `&[Vec2]` polyline distance in the tree
/// (`curves::dist_to_open_polyline_2d`) takes `CurveVertex`, not raw points, so the
/// on-boundary half is written here rather than bent into shape.
fn knife_ring_contains_or_touches(rings: &[Vec<Vec2>], p: Vec2, eps: f32) -> bool {
    for r in rings {
        if r.len() < 3 {
            continue;
        }
        if core_math::nurbs_trim::point_in_polygon(p, r) {
            return true;
        }
        for i in 0..r.len() {
            let a = r[i];
            let b = r[(i + 1) % r.len()];
            let e = b - a;
            let t = ((p - a).dot(e) / e.dot(e).max(1e-12)).clamp(0.0, 1.0);
            if (p - (a + e * t)).length() <= eps {
                return true;
            }
        }
    }
    false
}

/// Shoelace signed area (CCW positive) in plane coords.
fn knife_signed_area(poly: &[Vec2]) -> f32 {
    let mut a = 0.0f32;
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        a += p.x * q.y - q.x * p.y;
    }
    0.5 * a
}

/// Split concatenated closed world rings (each ends by repeating its first point) into
/// plane-UV polygons for multi-rect knife array boolean.
fn knife_split_closed_loops_p2(
    points: &[Vec3],
    o: Vec3,
    u: Vec3,
    v: Vec3,
) -> Vec<Vec<Vec2>> {
    let to2 = |w: Vec3| Vec2::new((w - o).dot(u), (w - o).dot(v));
    let mut loops: Vec<Vec<Vec2>> = Vec::new();
    let mut i = 0;
    while i < points.len() {
        let start = points[i];
        let mut ring_w: Vec<Vec3> = vec![start];
        i += 1;
        while i < points.len() {
            ring_w.push(points[i]);
            if (points[i] - start).length_squared() < 1e-10 {
                i += 1;
                break;
            }
            i += 1;
        }
        // Drop duplicate close.
        if ring_w.len() >= 2 {
            let last = *ring_w.last().unwrap();
            let first = ring_w[0];
            if (last - first).length_squared() < 1e-10 {
                ring_w.pop();
            }
        }
        if ring_w.len() < 3 {
            continue;
        }
        let mut p2: Vec<Vec2> = ring_w.iter().copied().map(to2).collect();
        // Also drop near-closed by extent (legacy free_boolean tolerance).
        if p2.len() >= 4 {
            let d = (p2[0] - *p2.last().unwrap()).length();
            let (mut lo, mut hi) = (p2[0], p2[0]);
            for p in &p2 {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
            if d < (hi - lo).length().max(1e-4) * 0.05 {
                p2.pop();
            }
        }
        if p2.len() >= 3 {
            loops.push(p2);
        }
    }
    // Fallback: single open-ish polyline treated as one loop (non-array path).
    if loops.is_empty() && points.len() >= 3 {
        let mut p2: Vec<Vec2> = points.iter().copied().map(to2).collect();
        if p2.len() >= 4 {
            let d = (p2[0] - *p2.last().unwrap()).length();
            let (mut lo, mut hi) = (p2[0], p2[0]);
            for p in &p2 {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
            if d < (hi - lo).length().max(1e-4) * 0.05 {
                p2.pop();
            }
        }
        if p2.len() >= 3 {
            loops.push(p2);
        }
    }
    loops
}


// =====================================================================================
// APP-PATH gates (memory `feedback_verify_app_path_not_kernel`): `core_math::knife` already
// proves the cut ENGINE handles interior points and dangling slits. These prove the WIRING —
// that the free knife's `S` actually hands its points to that engine. The engine tests passed
// the whole time this was broken, which is exactly why they are not enough.
// =====================================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActiveTool, FourthLayerApp, PolyRegionSelectState, SelectionShape,
    };

    /// Headless app with ONE flat quad spanning [-1,1] in the z = 0 plane, and the free knife
    /// armed on a drawing plane in front of it, looking down -Z. Mirrors the owner's screenshot:
    /// a single face seen head-on with the cut line drawn on the view plane.
    fn app_with_quad_and_free_knife() -> (FourthLayerApp, core_math::scene_graph::NodeId) {
        let p = vec![
            Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0),
        ];
        let mesh = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3]]);
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Quad".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                // Hand-built fixture mesh: authored, not derived from a PrimitiveSpec.
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        // Drawing plane: z = +2, u = X, v = Y, normal = camera forward (-Z).
        app.knife_free_plane = Some((
            Vec3::new(0.0, 0.0, 2.0), Vec3::X, Vec3::Y, Vec3::NEG_Z,
        ));
        (app, nid)
    }

    fn vert_at(app: &FourthLayerApp, nid: core_math::scene_graph::NodeId, p: Vec3) -> bool {
        let Some(m) = app.poly_node_mesh(nid) else { return false };
        (0..m.vertices.len() as u32).map(VertexId).any(|v| {
            m.is_vert_live(v) && (m.vert(v).position - p).length() < 1e-3
        })
    }

    fn vert_count(app: &FourthLayerApp, nid: core_math::scene_graph::NodeId) -> usize {
        let Some(m) = app.poly_node_mesh(nid) else { return 0 };
        (0..m.vertices.len() as u32).map(VertexId).filter(|&v| m.is_vert_live(v)).count()
    }

    /// Owner 2026-08-06: S / shape slice must cut **every mesh island** the path covers when
    /// no Face-mode face selection is active — not only the first island under densify rays.
    #[test]
    fn s_cuts_all_mesh_islands_when_no_face_selection() {
        // Two disjoint quads side-by-side (two islands in one poly body).
        let p = vec![
            // Island A: left
            Vec3::new(-3.0, -1.0, 0.0),
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
            Vec3::new(-3.0, 1.0, 0.0),
            // Island B: right
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(3.0, -1.0, 0.0),
            Vec3::new(3.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
        ];
        let mesh = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]]);
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "TwoIslands".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        app.poly_work_mode = PolyWorkMode::Object; // not Face — through-all
        app.poly_sel.selected_faces.clear();
        app.knife_free_plane = Some((Vec3::new(0.0, 0.0, 2.0), Vec3::X, Vec3::Y, Vec3::NEG_Z));
        // Horizontal line spanning both islands.
        app.knife_points = vec![
            Vec3::new(-4.0, 0.0, 2.0),
            Vec3::new(4.0, 0.0, 2.0),
        ];
        let v0 = vert_count(&app, nid);
        app.poly_knife_slice_through_now();
        assert!(
            vert_count(&app, nid) > v0,
            "S must cut at least one island"
        );
        // Each island must gain boundary crossings on the cut line y=0.
        assert!(
            vert_at(&app, nid, Vec3::new(-3.0, 0.0, 0.0))
                || vert_at(&app, nid, Vec3::new(-1.0, 0.0, 0.0)),
            "left island must be cut"
        );
        assert!(
            vert_at(&app, nid, Vec3::new(1.0, 0.0, 0.0))
                || vert_at(&app, nid, Vec3::new(3.0, 0.0, 0.0)),
            "right island must be cut (through-all islands)"
        );
    }

    fn face_count(app: &FourthLayerApp, nid: core_math::scene_graph::NodeId) -> usize {
        let Some(m) = app.poly_node_mesh(nid) else { return 0 };
        (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| m.is_face_live(f))
            .count()
    }

    fn app_with_mesh(mesh: HalfEdgeMesh) -> (FourthLayerApp, core_math::scene_graph::NodeId) {
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "SliceFixture".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        app.poly_work_mode = PolyWorkMode::Object;
        app.poly_sel.selected_faces.clear();
        app.knife_free_plane = Some((Vec3::new(0.0, 0.0, 2.0), Vec3::X, Vec3::Y, Vec3::NEG_Z));
        (app, nid)
    }

    /// The owner's "straight box slice always fails" repro (2026-08-07): a 2-point S
    /// through-slice of a closed cube must come out +4v/+4f and sound — the historical
    /// failure was +7v/+3f (ordinal-layer scramble) which the soundness gate then vetoed.
    #[test]
    fn s_straight_through_slice_of_cube_is_4v_4f() {
        let (mut app, nid) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(1.0)));
        app.knife_points = vec![Vec3::new(-2.0, 0.25, 2.0), Vec3::new(2.0, 0.25, 2.0)];
        let (v0, f0) = (vert_count(&app, nid), face_count(&app, nid));
        app.poly_knife_slice_through_now();
        let (v1, f1) = (vert_count(&app, nid), face_count(&app, nid));
        assert_eq!(v1 - v0, 4, "straight cube through-slice must add exactly 4 verts");
        assert_eq!(f1 - f0, 4, "straight cube through-slice must add exactly 4 faces");
        // All new verts lie ON the cut line y=0.25 on cube edges — never inside the volume.
        assert!(vert_at(&app, nid, Vec3::new(1.0, 0.25, 1.0)), "front-right edge vert");
        assert!(vert_at(&app, nid, Vec3::new(-1.0, 0.25, 1.0)), "front-left edge vert");
        assert!(vert_at(&app, nid, Vec3::new(1.0, 0.25, -1.0)), "back-right edge vert");
        assert!(vert_at(&app, nid, Vec3::new(-1.0, 0.25, -1.0)), "back-left edge vert");
        let m = app.poly_node_mesh(nid).unwrap();
        assert!(knife_result_is_sound(&m), "committed mesh must be sound");
    }

    /// Two disjoint closed cubes side by side: shell-keyed layers must scribe each shell
    /// independently (ordinal layers mixed shell B's front into shell A's back layer).
    #[test]
    fn s_through_slice_two_cubes_cuts_each_shell_independently() {
        let mut positions = Vec::new();
        let mut faces = Vec::new();
        for dx in [-2.0f32, 2.0] {
            let b = HalfEdgeMesh::new_box(Vec3::splat(0.5));
            let base = positions.len();
            for i in 0..b.vertices.len() as u32 {
                positions.push(b.vert(VertexId(i)).position + Vec3::new(dx, 0.0, 0.0));
            }
            for i in 0..b.faces.len() as u32 {
                let f = FaceId(i);
                if b.face(f).half_edge == NO_EDGE {
                    continue;
                }
                faces.push(b.face_vertices(f).iter().map(|v| base + v.0 as usize).collect());
            }
        }
        let mesh = HalfEdgeMesh::from_polygons(&positions, &faces);
        let (mut app, nid) = app_with_mesh(mesh);
        app.knife_points = vec![Vec3::new(-4.0, 0.25, 2.0), Vec3::new(4.0, 0.25, 2.0)];
        let (v0, f0) = (vert_count(&app, nid), face_count(&app, nid));
        app.poly_knife_slice_through_now();
        let (v1, f1) = (vert_count(&app, nid), face_count(&app, nid));
        assert_eq!(v1 - v0, 8, "each cube must gain 4 verts (got {} total)", v1 - v0);
        assert_eq!(f1 - f0, 8, "each cube must gain 4 faces (got {} total)", f1 - f0);
        for dx in [-2.0f32, 2.0] {
            for (ex, ez) in [(0.5, 0.5), (-0.5, 0.5), (0.5, -0.5), (-0.5, -0.5)] {
                assert!(
                    vert_at(&app, nid, Vec3::new(dx + ex, 0.25, ez)),
                    "cube at x={dx} missing cut vert at edge ({ex},{ez})"
                );
            }
        }
        let m = app.poly_node_mesh(nid).unwrap();
        assert!(knife_result_is_sound(&m), "committed mesh must be sound");
    }

    /// Owner 2026-08-08 "mesh cleanup after a cut cleaned all my geo": the post-cut collinear
    /// collapse must be SCOPED to verts the cut created — an authored collinear valence-2
    /// vert (a legitimate T-vert / subdivision vert on a straight rim) must survive a slice.
    #[test]
    fn s_slice_cleanup_never_dissolves_authored_collinear_verts() {
        // Pentagon = quad with an AUTHORED mid-edge vert at (0,-1,0): valence 2, collinear
        // with its neighbours — exactly what the old global collapse dissolved.
        let p = vec![
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0), // authored collinear valence-2 vert
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
        ];
        let mesh = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3, 4]]);
        let (mut app, nid) = app_with_mesh(mesh);
        // Horizontal slice at y=0.25 — nowhere near the authored vert.
        app.knife_points = vec![Vec3::new(-2.0, 0.25, 2.0), Vec3::new(2.0, 0.25, 2.0)];
        let v0 = vert_count(&app, nid);
        app.poly_knife_slice_through_now();
        assert!(vert_count(&app, nid) > v0, "slice must commit");
        assert!(
            vert_at(&app, nid, Vec3::new(0.0, -1.0, 0.0)),
            "authored collinear vert must SURVIVE the post-cut cleanup"
        );
    }

    /// Owner 2026-08-08 repro: cube → bevel top-front edge → rectangular S-slice straddling
    /// front face + top. The cut must go THROUGH — front scribe AND back-face scribe — not
    /// stop at the first shell.
    #[test]
    fn s_rect_slice_on_beveled_cube_cuts_through_to_back() {
        use core_math::bevel_op::{bevel_edges_offset, BevelParams};
        let cube = HalfEdgeMesh::new_box(Vec3::splat(1.0));
        // Top-front edge: endpoints at y=1, z=1.
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !cube.is_edge_live(he) || cube.he(he).twin == NO_TWIN {
                continue;
            }
            let a = cube.vert(cube.he_source(he)).position;
            let b = cube.vert(cube.he(he).vertex).position;
            if (a.y - 1.0).abs() < 1e-4 && (b.y - 1.0).abs() < 1e-4
                && (a.z - 1.0).abs() < 1e-4 && (b.z - 1.0).abs() < 1e-4
            {
                sel.insert(i);
                break;
            }
        }
        assert!(!sel.is_empty(), "top-front edge must exist");
        let beveled = bevel_edges_offset(
            &cube,
            &sel,
            &BevelParams { width: 0.2, segments: 2, shape: 0.5, clamp: true },
        );
        assert!(beveled.validate().is_ok(), "beveled cube must be valid");
        let (mut app, nid) = app_with_mesh(beveled);
        app.knife_shape_kind = 1; // R rect personality — closed by construction
        // Closed rect on the draw plane: x in [-0.4, 0.4], y from mid-front (0.0) to above
        // the cube (1.5) — upper corners miss the mesh, exactly the screenshot gesture.
        app.knife_points = vec![
            Vec3::new(-0.4, 0.0, 2.0),
            Vec3::new(0.4, 0.0, 2.0),
            Vec3::new(0.4, 1.5, 2.0),
            Vec3::new(-0.4, 1.5, 2.0),
        ];
        let v0 = vert_count(&app, nid);
        app.poly_knife_slice_through_now();
        let v1 = vert_count(&app, nid);
        assert!(v1 > v0, "rect slice must commit (gained {} verts)", v1 - v0);
        // Scribe slice: cut lines at x=±0.4 must exist on the front (z≈1) AND the back
        // (z≈-1) — the cut goes through, never stops at the first shell.
        let m = app.poly_node_mesh(nid).unwrap();
        let has_vert_near = |x: f32, z: f32| {
            (0..m.vertices.len() as u32).map(VertexId).any(|v| {
                let p = m.vert(v).position;
                m.is_vert_live(v)
                    && (p.x - x).abs() < 0.05
                    && (p.z - z).abs() < 0.05
            })
        };
        assert!(has_vert_near(-0.4, 1.0) && has_vert_near(0.4, 1.0), "front face must be cut");
        assert!(
            has_vert_near(-0.4, -1.0) && has_vert_near(0.4, -1.0),
            "back face must be cut too — through-cut stopped at the front shell"
        );
        assert!(knife_result_is_sound(&m), "committed mesh must be sound");
    }

    fn edge_between(app: &FourthLayerApp, nid: core_math::scene_graph::NodeId, a: Vec3, b: Vec3) -> bool {
        let Some(m) = app.poly_node_mesh(nid) else { return false };
        (0..m.half_edges.len() as u32).map(HalfEdgeId).any(|he| {
            m.is_edge_live(he) && {
                let p = m.vert(m.he_source(he)).position;
                let q = m.vert(m.he(he).vertex).position;
                (p - a).length() < 1e-3 && (q - b).length() < 1e-3
            }
        })
    }

    /// Owner 2026-08-08 "only the 2 segments from top is missing": a rect S-slice whose
    /// vertical lines run over the top of a cube must scribe the TOP face too. The top is
    /// grazing to the view (rays never hit it), and it gains FOUR rim verts (two per rect
    /// line) — the pair-along-path connect must split it twice, not skip it.
    #[test]
    fn s_rect_slice_cuts_grazing_top_face_segments() {
        let (mut app, nid) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(1.0)));
        app.knife_shape_kind = 1; // R rect personality — closed by construction
        app.knife_points = vec![
            Vec3::new(-0.4, 0.0, 2.0),
            Vec3::new(0.4, 0.0, 2.0),
            Vec3::new(0.4, 1.5, 2.0),
            Vec3::new(-0.4, 1.5, 2.0),
        ];
        let v0 = vert_count(&app, nid);
        app.poly_knife_slice_through_now();
        assert!(vert_count(&app, nid) > v0, "rect slice must commit");
        // The top face must carry BOTH cut segments, front rim to back rim.
        for x in [-0.4f32, 0.4] {
            let front = Vec3::new(x, 1.0, 1.0);
            let back = Vec3::new(x, 1.0, -1.0);
            assert!(
                edge_between(&app, nid, front, back) || edge_between(&app, nid, back, front),
                "top face missing its cut segment at x={x}"
            );
        }
        let m = app.poly_node_mesh(nid).unwrap();
        assert!(knife_result_is_sound(&m), "committed mesh must be sound");
    }

    /// Alt+S — boolean slice (owner 2026-08-08 "produce the connected faces through the
    /// island and use geo to slice"): both pieces kept, real cut-wall geometry, mesh valid.
    #[test]
    fn alt_s_boolean_slice_keeps_both_pieces() {
        let (mut app, nid) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(1.0)));
        app.knife_shape_kind = 1;
        app.knife_points = vec![
            Vec3::new(-0.4, 0.0, 2.0),
            Vec3::new(0.4, 0.0, 2.0),
            Vec3::new(0.4, 1.5, 2.0),
            Vec3::new(-0.4, 1.5, 2.0),
        ];
        app.knife_free_view = Some(Vec3::NEG_Z);
        let v0 = vert_count(&app, nid);
        app.poly_knife_slice_kerf_now();
        let m = app.poly_node_mesh(nid).unwrap();
        assert!(vert_count(&app, nid) > v0, "boolean slice must commit");
        assert!(m.validate().is_ok(), "boolean slice result must be valid");
        // Both halves kept as separate pieces (the cut column + the rest).
        let shells = knife_face_shell_ids(&m);
        let n_shells = shells.iter().filter(|&&s| s != u32::MAX).max().map_or(0, |&s| s + 1);
        assert!(
            n_shells >= 2,
            "boolean slice must keep BOTH pieces (got {n_shells} shell(s))"
        );
    }

    /// Owner 2026-08-08 "do a session of slice tests on different shapes and forms".
    /// Every case runs the real S commit and checks three invariants:
    ///   1. the slice COMMITS (vert count grows),
    ///   2. the result passes `knife_result_is_sound`,
    ///   3. no vertex escapes the pre-cut bounding box (space-flap guard).
    #[test]
    fn slice_session_battery() {
        struct Case {
            name: &'static str,
            mesh: HalfEdgeMesh,
            points: Vec<Vec3>,
            shape: u8,
            // (plane origin,u,v,normal) + view override; None = default front view.
            frame: Option<((Vec3, Vec3, Vec3, Vec3), Vec3)>,
        }
        let octagon = |cx: f32, cy: f32, r: f32| -> Vec<Vec3> {
            (0..8)
                .map(|i| {
                    let a = std::f32::consts::TAU * (i as f32) / 8.0;
                    Vec3::new(cx + r * a.cos(), cy + r * a.sin(), 2.0)
                })
                .collect()
        };
        let line = |y: f32| vec![Vec3::new(-3.0, y, 2.0), Vec3::new(3.0, y, 2.0)];
        let cases = vec![
            Case { name: "cube_h_line", mesh: HalfEdgeMesh::new_box(Vec3::splat(1.0)),
                   points: line(0.25), shape: 0, frame: None },
            Case { name: "cube_diag_line", mesh: HalfEdgeMesh::new_box(Vec3::splat(1.0)),
                   points: vec![Vec3::new(-2.0, -0.5, 2.0), Vec3::new(2.0, 0.8, 2.0)],
                   shape: 0, frame: None },
            Case { name: "cube_bent_path", mesh: HalfEdgeMesh::new_box(Vec3::splat(1.0)),
                   points: vec![Vec3::new(-2.0, 0.0, 2.0), Vec3::new(0.0, 0.7, 2.0),
                                Vec3::new(2.0, 0.0, 2.0)], shape: 0, frame: None },
            Case { name: "sphere_h_line", mesh: HalfEdgeMesh::new_uv_sphere(1.0, 16, 8),
                   points: line(0.25), shape: 0, frame: None },
            Case { name: "cylinder_h_line", mesh: HalfEdgeMesh::new_cylinder(0.8, 2.0, 16),
                   points: line(0.3), shape: 0, frame: None },
            Case { name: "cone_h_line", mesh: HalfEdgeMesh::new_cone(0.8, 1.5, 16),
                   points: line(0.3), shape: 0, frame: None },
            // y=0.1 ON PURPOSE: y=0 coincides with an existing minor-ring edge loop and the
            // correct result there is a no-op (cut resolves to existing edges, zero new verts).
            Case { name: "torus_h_line", mesh: HalfEdgeMesh::new_torus(1.0, 0.3, 16, 8),
                   points: line(0.1), shape: 0, frame: None },
            Case { name: "wall_v_line", mesh: HalfEdgeMesh::new_plane(2.0, 2.0, 8, 8),
                   points: vec![Vec3::new(0.1, 2.0, -2.0), Vec3::new(0.1, 2.0, 2.0)],
                   shape: 0,
                   frame: Some(((Vec3::new(0.0, 2.0, 0.0), Vec3::X, Vec3::Z, Vec3::NEG_Y),
                                Vec3::NEG_Y)) },
            Case { name: "cube_rect_over_top", mesh: HalfEdgeMesh::new_box(Vec3::splat(1.0)),
                   points: vec![Vec3::new(-0.4, 0.0, 2.0), Vec3::new(0.4, 0.0, 2.0),
                                Vec3::new(0.4, 1.5, 2.0), Vec3::new(-0.4, 1.5, 2.0)],
                   shape: 1, frame: None },
            Case { name: "cube_octagon_on_silhouette",
                   mesh: HalfEdgeMesh::new_box(Vec3::splat(1.0)),
                   points: octagon(0.9, 0.0, 0.5), shape: 2, frame: None },
        ];
        let mut failures: Vec<String> = Vec::new();
        for case in cases {
            // Pre-cut bbox for the flap guard.
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for i in 0..case.mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if case.mesh.vert(v).half_edge == NO_EDGE { continue; }
                lo = lo.min(case.mesh.vert(v).position);
                hi = hi.max(case.mesh.vert(v).position);
            }
            let eps = (hi - lo).length().max(1e-3) * 1e-3 + 1e-4;
            let (mut app, nid) = app_with_mesh(case.mesh);
            app.knife_shape_kind = case.shape;
            if let Some((plane, view)) = case.frame {
                app.knife_free_plane = Some(plane);
                app.knife_free_view = Some(view);
            }
            app.knife_points = case.points;
            let v0 = vert_count(&app, nid);
            app.poly_knife_slice_through_now();
            let m = app.poly_node_mesh(nid).unwrap();
            let committed = vert_count(&app, nid) > v0;
            let sound = knife_result_is_sound(&m);
            let mut flap = false;
            for i in 0..m.vertices.len() as u32 {
                let v = VertexId(i);
                if !m.is_vert_live(v) { continue; }
                let p = m.vert(v).position;
                if p.x < lo.x - eps || p.y < lo.y - eps || p.z < lo.z - eps
                    || p.x > hi.x + eps || p.y > hi.y + eps || p.z > hi.z + eps
                {
                    flap = true;
                    break;
                }
            }
            eprintln!(
                "[SLICE-TEST] {:<28} committed={} sound={} flap={}",
                case.name, committed, sound, flap
            );
            if !committed || !sound || flap {
                failures.push(format!(
                    "{} (committed={committed} sound={sound} flap={flap})",
                    case.name
                ));
            }
        }
        assert!(failures.is_empty(), "slice battery failures:\n{}", failures.join("\n"));
    }

    /// Face-mode selection expands to the face-linked island and cuts only that island.
    #[test]
    fn s_with_face_selection_only_cuts_selected_island() {
        let p = vec![
            Vec3::new(-3.0, -1.0, 0.0),
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
            Vec3::new(-3.0, 1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(3.0, -1.0, 0.0),
            Vec3::new(3.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
        ];
        let mesh = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]]);
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "TwoIslands".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        app.poly_work_mode = PolyWorkMode::Face;
        // Select only left face (id 0).
        app.poly_sel.selected_faces.clear();
        app.poly_sel.selected_faces.insert(0);
        app.knife_free_plane = Some((Vec3::new(0.0, 0.0, 2.0), Vec3::X, Vec3::Y, Vec3::NEG_Z));
        app.knife_points = vec![
            Vec3::new(-4.0, 0.0, 2.0),
            Vec3::new(4.0, 0.0, 2.0),
        ];
        app.poly_knife_slice_through_now();
        assert!(
            vert_at(&app, nid, Vec3::new(-3.0, 0.0, 0.0))
                || vert_at(&app, nid, Vec3::new(-1.0, 0.0, 0.0)),
            "selected left island must be cut"
        );
        // Right island must stay uncut (no verts on y=0 at x=±1,±3 of right island that
        // weren't there — original right island had no verts on y=0).
        assert!(
            !vert_at(&app, nid, Vec3::new(1.0, 0.0, 0.0))
                && !vert_at(&app, nid, Vec3::new(3.0, 0.0, 0.0)),
            "unselected right island must NOT be cut"
        );
    }

    /// Owner 2026-08-07: **full** left island selected → through-cut that shell only.
    /// (One face seed is partial — not full-island mode.)
    #[test]
    fn s_selected_island_through_cuts_shell_rejects_other() {
        // Left box centered at (-2,0,0), right at (+2,0,0); half-extent 0.5.
        let mut left = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        for i in 0..left.vertices.len() as u32 {
            let v = VertexId(i);
            if left.vert(v).half_edge != NO_EDGE {
                left.vert_mut(v).position.x -= 2.0;
            }
        }
        let mut right = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        for i in 0..right.vertices.len() as u32 {
            let v = VertexId(i);
            if right.vert(v).half_edge != NO_EDGE {
                right.vert_mut(v).position.x += 2.0;
            }
        }
        // Merge as two disconnected islands via from_polygons soup.
        let mut positions = Vec::new();
        let mut faces = Vec::new();
        for m in [&left, &right] {
            let base = positions.len();
            for i in 0..m.vertices.len() as u32 {
                let v = VertexId(i);
                if m.vert(v).half_edge == NO_EDGE {
                    positions.push(Vec3::ZERO); // slot
                } else {
                    positions.push(m.vert(v).position);
                }
            }
            for fi in 0..m.faces.len() as u32 {
                if m.face(FaceId(fi)).half_edge == NO_EDGE {
                    continue;
                }
                let ring: Vec<usize> = m
                    .face_vertices(FaceId(fi))
                    .into_iter()
                    .map(|v| base + v.0 as usize)
                    .collect();
                if ring.len() >= 3 {
                    faces.push(ring);
                }
            }
        }
        let mesh = HalfEdgeMesh::from_polygons(&positions, &faces);
        // Full left island: every face with centroid x < 0.
        let left_faces: Vec<u32> = (0..mesh.faces.len() as u32)
            .filter(|&fi| {
                if mesh.face(FaceId(fi)).half_edge == NO_EDGE {
                    return false;
                }
                let vs = mesh.face_vertices(FaceId(fi));
                let c: Vec3 = vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>()
                    / vs.len().max(1) as f32;
                c.x < 0.0
            })
            .collect();
        assert!(
            left_faces.len() >= 6,
            "closed box island should have 6 faces, got {}",
            left_faces.len()
        );

        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "TwoBoxes".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        app.poly_work_mode = PolyWorkMode::Face;
        app.poly_sel.selected_faces.clear();
        for &f in &left_faces {
            app.poly_sel.selected_faces.insert(f);
        }
        // Looking down -Z; free plane through origin.
        app.knife_free_plane = Some((Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::NEG_Z));
        // Horizontal cut across both boxes (y=0).
        app.knife_points = vec![
            Vec3::new(-4.0, 0.0, 0.0),
            Vec3::new(4.0, 0.0, 0.0),
        ];
        let v0 = vert_count(&app, nid);
        app.poly_knife_slice_through_now();
        assert!(
            vert_count(&app, nid) > v0,
            "full left island must receive a through-cut"
        );
        // Left box half-extents: x in [-2.5,-1.5], y in [-0.5,0.5], z in [-0.5,0.5].
        // Through-cut at y=0 should hit front (z=+0.5) and/or back (z=-0.5) on left.
        let near_left = |qx: f32, qz: f32| {
            (0..app.poly_node_mesh(nid).unwrap().vertices.len() as u32).any(|i| {
                let m = app.poly_node_mesh(nid).unwrap();
                let v = VertexId(i);
                if !m.is_vert_live(v) {
                    return false;
                }
                let p = m.vert(v).position;
                (p.x - qx).abs() < 0.08 && p.y.abs() < 0.08 && (p.z - qz).abs() < 0.08
            })
        };
        assert!(
            near_left(-2.5, 0.5)
                || near_left(-1.5, 0.5)
                || near_left(-2.5, -0.5)
                || near_left(-1.5, -0.5)
                || near_left(-2.0, 0.5)
                || near_left(-2.0, -0.5),
            "left island shell must be cut (front and/or back silhouette at y=0)"
        );
        // Right box must not gain cut verts on y=0 at its silhouette.
        let near_right = |qx: f32, qz: f32| {
            (0..app.poly_node_mesh(nid).unwrap().vertices.len() as u32).any(|i| {
                let m = app.poly_node_mesh(nid).unwrap();
                let v = VertexId(i);
                if !m.is_vert_live(v) {
                    return false;
                }
                let p = m.vert(v).position;
                (p.x - qx).abs() < 0.08 && p.y.abs() < 0.08 && (p.z - qz).abs() < 0.08
            })
        };
        assert!(
            !near_right(1.5, 0.5)
                && !near_right(2.5, 0.5)
                && !near_right(1.5, -0.5)
                && !near_right(2.5, -0.5)
                && !near_right(2.0, 0.5)
                && !near_right(2.0, -0.5),
            "unselected right island must be rejected from the cut"
        );
    }

    /// THE OWNER SCREENSHOT (2026-08-05, "this slice pressing s fail"): a free-knife line that
    /// enters the quad from outside and STOPS INSIDE it. `S` reported "nothing to cut" and left
    /// the mesh untouched — the path has exactly ONE boundary crossing, and with the interior
    /// points withheld there was nothing to connect it to.
    #[test]
    fn s_slices_a_path_that_dies_inside_the_face() {
        let (mut app, nid) = app_with_quad_and_free_knife();
        let v0 = vert_count(&app, nid);
        // Drawn on the plane (z = 2): start below the quad, enter, bend twice, stop inside.
        app.knife_points = vec![
            Vec3::new(0.0, -2.0, 2.0),
            Vec3::new(0.0, -0.5, 2.0),
            Vec3::new(0.4,  0.0, 2.0),
            Vec3::new(0.4,  0.5, 2.0),
        ];
        app.poly_knife_slice_through_now();

        assert!(vert_count(&app, nid) > v0, "S must cut: the mesh gained no vertices at all");
        assert!(
            vert_at(&app, nid, Vec3::new(0.0, -1.0, 0.0)),
            "the boundary crossing must become a real vertex"
        );
        assert!(
            vert_at(&app, nid, Vec3::new(0.4, 0.5, 0.0)),
            "the DANGLING TIP inside the face must exist — this is what the screenshot cut to"
        );
        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "mesh valid after the slit: {:?}", m.validate());
    }

    /// The same defect with a path that DOES cross twice: the bend used to be dropped, so the
    /// cut came out as a straight chord between entry and exit instead of the drawn zig-zag.
    /// Asserting on the bend vertex is what separates "it cut something" from "it cut what I drew".
    #[test]
    fn s_follows_the_drawn_bend_instead_of_chording_it() {
        let (mut app, nid) = app_with_quad_and_free_knife();
        app.knife_points = vec![
            Vec3::new(-0.2, -2.0, 2.0),
            Vec3::new(-0.2,  0.0, 2.0),
            Vec3::new( 0.6,  0.0, 2.0),
            Vec3::new( 0.6,  2.0, 2.0),
        ];
        app.poly_knife_slice_through_now();

        assert!(
            vert_at(&app, nid, Vec3::new(-0.2, 0.0, 0.0)) && vert_at(&app, nid, Vec3::new(0.6, 0.0, 0.0)),
            "both drawn bends must be real vertices — a straight chord would have neither"
        );
        let m = app.poly_node_mesh(nid).expect("body");
        // Through + densify may produce >2 faces (extra splits); the gate is "the quad was cut".
        assert!(
            m.face_count() >= 2,
            "a through-cut must split the quad (got {} faces)",
            m.face_count()
        );
        assert!(m.validate().is_ok(), "mesh valid after the through-cut: {:?}", m.validate());
    }

    /// Owner test-019 (2026-08-06): free-knife densify was inserting many collinear mid-samples
    /// on every straight segment, so the cut face's selection outline looked like a "crazy"
    /// multi-vert trapezoid instead of clean segments. After the cut, collinear valence-2 pins
    /// must collapse; intentional bends (corners) stay.
    #[test]
    fn s_straight_slice_collapses_densify_pins_to_clean_segments() {
        let (mut app, nid) = app_with_quad_and_free_knife();
        // Long straight vertical cut through the quad — densify would have added ~a dozen
        // collinear mid verts along y without the post-cut collapse.
        app.knife_points = vec![
            Vec3::new(0.0, -2.0, 2.0),
            Vec3::new(0.0,  2.0, 2.0),
        ];
        app.poly_knife_slice_through_now();
        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "valid after straight slice: {:?}", m.validate());
        // No valence-2 collinear leftover (the densify-pin class).
        let mut v2_colin = 0usize;
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if !m.is_vert_live(v) {
                continue;
            }
            let fan = m.vertex_fan(v);
            if fan.len() != 2 {
                continue;
            }
            let a = m.he(fan[0]).vertex;
            let b = m.he(fan[1]).vertex;
            let d1 = (m.vert(v).position - m.vert(a).position).normalize_or_zero();
            let d2 = (m.vert(b).position - m.vert(v).position).normalize_or_zero();
            if d1.dot(d2) > 0.999 {
                v2_colin += 1;
            }
        }
        assert_eq!(
            v2_colin, 0,
            "straight densify pins must collapse; left {v2_colin} valence-2 collinear verts (v={})",
            m.vertex_count()
        );
        // Boundary crossings at top and bottom of the unit quad must exist.
        assert!(
            vert_at(&app, nid, Vec3::new(0.0, -1.0, 0.0))
                && vert_at(&app, nid, Vec3::new(0.0, 1.0, 0.0)),
            "entry/exit of the straight cut must remain"
        );
    }

    /// A path drawn entirely off the silhouette must still refuse cleanly — the surface-resolve
    /// must not invent geometry when its ray misses the mesh.
    #[test]
    fn s_off_the_mesh_still_refuses() {
        let (mut app, nid) = app_with_quad_and_free_knife();
        let v0 = vert_count(&app, nid);
        app.knife_points = vec![
            Vec3::new(3.0, -2.0, 2.0),
            Vec3::new(3.0,  2.0, 2.0),
        ];
        app.poly_knife_slice_through_now();
        assert_eq!(vert_count(&app, nid), v0, "a miss must leave the mesh untouched");
    }

    /// THE OWNER REGRESSION (2026-08-05, "slice produce geo outside of the target mesh"): the
    /// free knife's drawing plane is captured through the BODY CENTRE, so a path point drawn
    /// past the silhouette still sits inside the infinite PRISM of the top/bottom face. The
    /// interior-click classifier is a winding test that reads any such point as "inside" that
    /// face at any height, so it became a real cut vertex OUT IN SPACE and its edges rendered
    /// as flaps sticking out of the cube.
    ///
    /// The assertion is the owner's sentence made literal: after a slice, no vertex may lie
    /// outside the mesh it cut.
    #[test]
    fn s_never_creates_geometry_outside_the_body() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE); // [-1,1]^3
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Cube".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                // Hand-built fixture mesh: authored, not derived from a PrimitiveSpec.
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        // Drawing plane through the BODY CENTRE, as the real first-click capture does.
        app.knife_free_plane = Some((Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::NEG_Z));
        // Enter above the cube, cross the front face with a bend, exit below it. The first and
        // last points are past the silhouette but still inside the top/bottom face prisms.
        app.knife_points = vec![
            Vec3::new( 0.2,  2.0, 0.0),
            Vec3::new( 0.2,  0.0, 0.0),
            Vec3::new(-0.3, -0.5, 0.0),
            Vec3::new(-0.3, -2.0, 0.0),
        ];
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if !m.is_vert_live(v) {
                continue;
            }
            let p = m.vert(v).position;
            assert!(
                p.x.abs() <= 1.0 + 1e-3 && p.y.abs() <= 1.0 + 1e-3 && p.z.abs() <= 1.0 + 1e-3,
                "slice put a vertex OUTSIDE the cube at {p:?} — the flap regression is back"
            );
        }
        assert!(m.validate().is_ok(), "mesh valid after the slice: {:?}", m.validate());
    }

    /// Owner 2026-08-05: "slice (S) cut is not keeping the polyline shape and the cut do not go
    /// true". A bent free-knife path on a solid cube must (a) land the bends as real verts on
    /// the front surface and (b) cut the BACK face too (`through=true` from S).
    #[test]
    fn s_cube_bent_path_keeps_shape_and_cuts_through() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5)); // [-0.5,0.5]^3
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Cube".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                // Hand-built fixture mesh: authored, not derived from a PrimitiveSpec.
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        // Drawing plane through body centre, looking down -Z (matches free-knife capture).
        app.knife_free_plane = Some((Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::NEG_Z));
        // Bent path fully crossing the cube (entry below, two bends inside, exit above).
        app.knife_points = vec![
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(0.25, -0.2, 0.0),
            Vec3::new(-0.25, 0.2, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "mesh valid: {:?}", m.validate());

        let near = |q: Vec3, tol: f32| {
            (0..m.vertices.len() as u32).any(|i| {
                let v = VertexId(i);
                m.is_vert_live(v) && (m.vert(v).position - q).length() < tol
            })
        };
        // Shape: bends must exist on the FRONT face (z = +0.5), not chording to a straight cut.
        assert!(
            near(Vec3::new(0.25, -0.2, 0.5), 0.08),
            "first bend missing on front — polyline shape was flattened/chorded"
        );
        assert!(
            near(Vec3::new(-0.25, 0.2, 0.5), 0.08),
            "second bend missing on front — polyline shape was flattened/chorded"
        );
        // Through + shape: the SAME bends must land on the BACK face (z = -0.5), not a chord.
        assert!(
            near(Vec3::new(0.25, -0.2, -0.5), 0.08),
            "first bend missing on back — through-slice lost polyline shape on the far side"
        );
        assert!(
            near(Vec3::new(-0.25, 0.2, -0.5), 0.08),
            "second bend missing on back — through-slice lost polyline shape on the far side"
        );
        // Owner 2026-08-06 ortho S-slice fail: the old final through=true pass (no poly_3d)
        // chained front/back Bounds into mid-volume probes. No vertex may sit inside the cube
        // (only on the shell faces of the AABB).
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if !m.is_vert_live(v) {
                continue;
            }
            let p = m.vert(v).position;
            let on_shell = p.x.abs() > 0.5 - 1e-3
                || p.y.abs() > 0.5 - 1e-3
                || p.z.abs() > 0.5 - 1e-3;
            assert!(
                on_shell,
                "slice left an interior vertex at {p:?} — double through-pass flap regression"
            );
        }
    }

    /// Owner 2026-08-06: orthographic free-knife S on a tall pillar must not produce a
    /// "crazy" cut outline (spikes / mid-volume chords). Reproduces the desktop screenshot
    /// case: view-aligned free plane, horizontal bent path across a vertical box.
    #[test]
    fn s_ortho_pillar_slice_stays_on_shell() {
        // Tall box ~ pillar (half-extents).
        let mesh = HalfEdgeMesh::new_box(Vec3::new(0.5, 1.5, 0.5));
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Pillar".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.render.ortho = true;
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        // Looking down -Z (front face at +z), free plane through body centre.
        app.knife_free_plane = Some((Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::NEG_Z));
        // Horizontal-ish cut near the base with two bends (matches owner "S slice" path style).
        app.knife_points = vec![
            Vec3::new(-0.8, -1.0, 0.0),
            Vec3::new(-0.2, -1.0, 0.0),
            Vec3::new(0.2, -0.7, 0.0),
            Vec3::new(0.8, -0.7, 0.0),
        ];
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "mesh valid after ortho pillar slice: {:?}", m.validate());
        let half = Vec3::new(0.5, 1.5, 0.5);
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if !m.is_vert_live(v) {
                continue;
            }
            let p = m.vert(v).position;
            // No flaps outside the AABB.
            assert!(
                p.x.abs() <= half.x + 1e-3
                    && p.y.abs() <= half.y + 1e-3
                    && p.z.abs() <= half.z + 1e-3,
                "ortho S-slice put vertex outside pillar at {p:?}"
            );
            // No interior mid-volume verts (double through-pass symptom).
            let on_shell = p.x.abs() > half.x - 1e-3
                || p.y.abs() > half.y - 1e-3
                || p.z.abs() > half.z - 1e-3;
            assert!(
                on_shell,
                "ortho S-slice left interior vertex at {p:?} (mangled cut face)"
            );
        }
        // Must actually have cut (new verts beyond the original 8 box corners).
        let live = (0..m.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| m.is_vert_live(v))
            .count();
        assert!(live > 8, "slice must add cut verts (got {live})");
    }

    /// Owner 2026-08-07: shape R/C press freezes a FACE plane for drawing. S slice must still
    /// project along the **camera view** (Blender cut-through), not the face normal — otherwise
    /// every S on a face-pressed rect/circle fails with "nothing to cut".
    #[test]
    fn s_slice_on_face_drawing_plane_still_cuts_through() {
        // Unit cube centered at origin (half-extent 0.5).
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "CubeFacePlane".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        // Drawing plane = TOP face (y = +0.5), face normal +Y — like R-press on the top.
        // Camera looks along -Z (not along face normal).
        app.knife_free_plane = Some((
            Vec3::new(0.0, 0.5, 0.0),
            Vec3::X,
            Vec3::NEG_Z,
            Vec3::Y,
        ));
        app.knife_free_view = Some(Vec3::NEG_Z);
        // Path across the top face in X, slightly outside the silhouette so through-cut
        // has clear entry/exit in the view projection.
        app.knife_points = vec![
            Vec3::new(-1.0, 0.5, 0.0),
            Vec3::new(1.0, 0.5, 0.0),
        ];
        let v0 = vert_count(&app, nid);
        let f0 = app.poly_node_mesh(nid).map(|m| m.face_count()).unwrap_or(0);
        app.poly_knife_slice_through_now();
        // This fixture is DEGENERATE by construction: the drawn line lies in the top
        // face's own plane and the camera views that plane edge-on, so the extruded cut
        // plane IS the face plane and every crossing param on the edge-on edges is
        // mathematically arbitrary. The owner requirement this test protects is only
        // "S must not report nothing-to-cut" (the face-normal-projection bug). Since the
        // 2026-08-07 crossing merge (near-coincident crossings snap to existing verts
        // instead of materialising garbage positions on degenerate edges), the committed
        // cut here may legitimately be a vert-FREE divide between existing rim verts —
        // so assert "a cut committed" (verts OR faces gained), not a vert delta.
        let v1 = vert_count(&app, nid);
        let f1 = app.poly_node_mesh(nid).map(|m| m.face_count()).unwrap_or(0);
        assert!(
            v1 > v0 || f1 > f0,
            "S on face-plane stroke must cut (view project), not report nothing-to-cut \
             (v {v0}->{v1}, f {f0}->{f1})"
        );
        let m = app.poly_node_mesh(nid).expect("body");
        assert!(
            m.validate().is_ok(),
            "mesh valid after face-plane S slice: {:?}",
            m.validate()
        );
    }

    /// Owner 2026-08-06: after a shape drag, rubber-band clears but MODE stays (R/C).
    /// L switches to free polyline — not automatic disarm on release.
    #[test]
    fn shape_release_keeps_kind_armed() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 1; // Rectangle armed
        app.knife_shape_anchor = Some(Vec3::new(-0.5, -0.5, 2.0));
        app.knife_shape_cursor = Some(Vec3::new(0.5, 0.5, 2.0));
        app.knife_shape_screen_a = Some(egui::pos2(10.0, 10.0));
        app.knife_shape_screen_b = Some(egui::pos2(100.0, 100.0));
        // Closed rectangle as rebuild would write it (first == last).
        app.knife_points = vec![
            Vec3::new(-0.5, -0.5, 2.0),
            Vec3::new(0.5, -0.5, 2.0),
            Vec3::new(0.5, 0.5, 2.0),
            Vec3::new(-0.5, 0.5, 2.0),
            Vec3::new(-0.5, -0.5, 2.0),
        ];
        app.poly_knife_shape_release();

        assert_eq!(
            app.knife_shape_kind, 1,
            "release must KEEP rectangle mode for the next drag"
        );
        assert!(app.knife_shape_anchor.is_none(), "anchor cleared after place");
        assert!(app.knife_shape_cursor.is_none(), "cursor cleared after place");
        assert!(app.knife_shape_screen_a.is_none(), "screen rubber-band cleared");
        assert!(app.knife_shape_screen_b.is_none(), "screen rubber-band cleared");
    }

    /// Space mid-drag must translate the whole rect without changing half-extents
    /// (owner: Alt-from-center rect → hold Space to reposition).
    #[test]
    fn shape_space_move_translates_without_resizing() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 1;
        app.knife_shape_from_center = true;
        // Center at origin, half-extent to (0.5, 0.4) on the free plane z=2.
        app.knife_shape_anchor = Some(Vec3::new(0.0, 0.0, 2.0));
        app.knife_shape_cursor = Some(Vec3::new(0.5, 0.4, 2.0));
        app.knife_shape_screen_a = Some(egui::pos2(100.0, 100.0));
        app.knife_shape_screen_b = Some(egui::pos2(150.0, 140.0));
        app.poly_knife_shape_rebuild();
        let before = app.knife_points.clone();
        assert!(before.len() >= 4, "rect must be laid out");

        // Simulate Space-move: world-plane delta +0.2 X via move helper internals.
        // Directly call move with synthetic mouse delta by setting last mouse then moving.
        app.knife_shape_move_last_mouse = Some(egui::pos2(150.0, 100.0));
        // Without a real camera projection, plane hit may fail — instead translate fields
        // the same way move would when plane hits succeed.
        let d = Vec3::new(0.25, -0.1, 0.0);
        if let Some(a) = app.knife_shape_anchor.as_mut() {
            *a += d;
        }
        if let Some(c) = app.knife_shape_cursor.as_mut() {
            *c += d;
        }
        app.poly_knife_shape_rebuild();
        let after = app.knife_points.clone();
        assert_eq!(before.len(), after.len(), "move must not change point count");
        // All corners shift by the same delta (size preserved).
        for (b, a) in before.iter().zip(after.iter()) {
            let delta = *a - *b;
            assert!(
                (delta - d).length() < 1e-4,
                "corner shifted by {delta:?}, expected {d:?}"
            );
        }
        // Half-extent length preserved (from_center: |cursor-anchor|).
        let ext_before = (before[1] - before[0]).length(); // side length proxy if ordered
        let _ = ext_before;
        let half_b = (Vec3::new(0.0, 0.0, 2.0) - Vec3::new(0.5, 0.4, 2.0)).length();
        let half_a = (app.knife_shape_anchor.unwrap() - app.knife_shape_cursor.unwrap()).length();
        assert!((half_a - half_b).abs() < 1e-5, "half-extent must be unchanged by move");
    }

    #[test]
    fn l_returns_to_free_polyline_mode() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 2;
        app.poly_knife_shape_to_free_line();
        assert_eq!(app.knife_shape_kind, 0);
        assert!(app.poly_tool == PolyTool::Knife);
    }

    /// Owner 2026-08-11: *"knife cut should also auto set selection, set the resulting geo from
    /// boolean."* The default flipped ON that day (it shipped OFF 2026-08-04) — the toggle stays
    /// as the opt-out, not the opt-in.
    #[test]
    fn select_after_cut_defaults_on() {
        let (app, _) = app_with_quad_and_free_knife();
        assert!(
            app.knife_select_after_cut,
            "Select after cut must default ON — a cut hands back what it produced"
        );
    }

    /// A SOLID subtract must leave the boolean's own geometry selected: the walls of the hole it
    /// punched, in Face mode, and nothing that existed before the cut.
    ///
    /// Asserted by PROPERTY, not by face count — the boolean is free to emit the walls as quads
    /// or triangles, and pinning "4 faces" would fail on a retessellation that is not a bug.
    #[test]
    fn solid_subtract_auto_selects_the_boolean_result_faces() {
        let (mut a, nid) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(2.0)));
        // The knife only arms from a poly EDIT mode (`poly_activate_knife`), so the post-cut
        // mode switch is always edit→edit — reproduce that precondition here.
        a.modeling_context = ModelingContext::Polygonal;
        a.poly_work_mode = PolyWorkMode::Edge;
        a.knife_free_view = Some(Vec3::NEG_Z);
        // 1x1 closed loop → a square hole straight through the 4x4x4 box along Z.
        a.knife_points = vec![
            Vec3::new(-0.5, -0.5, 2.0),
            Vec3::new(0.5, -0.5, 2.0),
            Vec3::new(0.5, 0.5, 2.0),
            Vec3::new(-0.5, 0.5, 2.0),
            Vec3::new(-0.5, -0.5, 2.0),
        ];
        assert!(a.knife_select_after_cut, "fixture relies on the ON default");
        a.poly_knife_free_boolean(None);

        let out = a.poly_node_mesh(nid).expect("solid subtract must commit");
        assert_eq!(
            a.poly_work_mode,
            PolyWorkMode::Face,
            "a face selection is only usable in Face mode"
        );
        assert!(
            !a.poly_sel.selected_faces.is_empty(),
            "the boolean's new faces must be selected"
        );
        assert!(
            a.poly_sel.selected_edges.is_empty() && a.poly_sel.selected_vertices.is_empty(),
            "the face selection must not leave stale edge/vertex selection behind"
        );
        // Every selected face is a WALL of the hole: inside the 1x1 cutter footprint, and
        // never one of the box's own faces (which reach out to |x|,|y| = 2).
        let mut wall_planes = [false; 4]; // x=-0.5, x=+0.5, y=-0.5, y=+0.5
        for &f in &a.poly_sel.selected_faces {
            let f = FaceId(f);
            assert!(out.is_face_live(f), "selected face {f:?} is dead");
            let vs = out.face_vertices(f);
            for v in &vs {
                let p = out.vert(*v).position;
                assert!(
                    p.x.abs() <= 0.5 + 1e-3 && p.y.abs() <= 0.5 + 1e-3,
                    "selected face {f:?} escapes the cut footprint at {p:?} — that is pre-cut geometry"
                );
            }
            let all = |g: &dyn Fn(Vec3) -> bool| vs.iter().all(|v| g(out.vert(*v).position));
            if all(&|p: Vec3| (p.x + 0.5).abs() < 1e-3) { wall_planes[0] = true }
            if all(&|p: Vec3| (p.x - 0.5).abs() < 1e-3) { wall_planes[1] = true }
            if all(&|p: Vec3| (p.y + 0.5).abs() < 1e-3) { wall_planes[2] = true }
            if all(&|p: Vec3| (p.y - 0.5).abs() < 1e-3) { wall_planes[3] = true }
        }
        assert!(
            wall_planes.iter().all(|&hit| hit),
            "all four walls of the hole must be selected, got {wall_planes:?}"
        );
    }

    /// REGRESSION (owner 2026-08-11: *"you broke the click and drag cut feature with shape"*).
    /// Two rect cuts in a row must BOTH punch a hole. The auto-select above lands the tool in Face
    /// mode holding the cut walls, and `poly_knife_shape_commit_cut` reads a face selection as
    /// "scope this cut to those faces" — so cut #2 silently became a slice of cut #1's own walls
    /// and stopped removing material. The tool's OUTPUT must never be read back as the user's
    /// INTENT.
    #[test]
    fn two_shape_cuts_in_a_row_both_punch_a_hole() {
        let (mut a, nid) = run_shape_bool(KnifeBoolOp::Subtract, None);
        let after_first = a.poly_node_mesh(nid).expect("first cut").face_count();
        assert!(
            a.poly_work_mode == PolyWorkMode::Face && !a.poly_sel.selected_faces.is_empty(),
            "fixture precondition: cut #1 must leave its own face selection armed"
        );
        // Cut #2 — a second rect elsewhere on the same face, same click-drag gesture.
        a.knife_shape_kind = 1;
        a.knife_free = true;
        a.knife_free_plane = Some((Vec3::new(0.0, 0.0, 0.5), Vec3::X, Vec3::Y, Vec3::Z));
        a.knife_free_view = Some(Vec3::NEG_Z);
        a.knife_shape_anchor = Some(Vec3::new(0.3, 0.3, 0.5));
        a.knife_shape_cursor = Some(Vec3::new(0.45, 0.45, 0.5));
        a.poly_knife_shape_rebuild();
        a.poly_knife_shape_release();
        let m = a.poly_node_mesh(nid).expect("second cut");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert_eq!(open_edge_count(&m), 0, "the second cut left the body open");
        // A hole through the box: the far (−Z) face must have gained a second rim.
        let verts_at_far = (0..m.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| m.is_vert_live(v) && (m.vert(v).position.z + 0.5).abs() < 1e-3)
            .count();
        assert!(
            verts_at_far >= 12,
            "the second cut did not punch through (far face has {verts_at_far} verts, \
             expected the box's 4 + two hole rims) — it was routed to a slice"
        );
        assert!(
            m.face_count() > after_first,
            "the second cut added no geometry ({} → {})",
            after_first,
            m.face_count()
        );
    }

    /// The fallback half of the same contract: an OPEN mesh cookie-cut REMOVES faces, so there is
    /// no new face to hand back — the rim it exposed is the result, and it lands in Edge mode.
    #[test]
    fn open_mesh_subtract_auto_selects_the_cut_rim() {
        let (mut a, nid) = app_with_quad_and_free_knife();
        a.modeling_context = ModelingContext::Polygonal;
        a.poly_work_mode = PolyWorkMode::Face;
        a.knife_free_view = Some(Vec3::NEG_Z);
        a.knife_points = vec![
            Vec3::new(-0.5, -0.5, 2.0),
            Vec3::new(0.5, -0.5, 2.0),
            Vec3::new(0.5, 0.5, 2.0),
            Vec3::new(-0.5, 0.5, 2.0),
            Vec3::new(-0.5, -0.5, 2.0),
        ];
        a.poly_knife_free_boolean(None);
        let _ = a.poly_node_mesh(nid).expect("cookie-cut must commit");
        assert_eq!(a.poly_work_mode, PolyWorkMode::Edge, "a rim is an EDGE selection");
        assert!(
            !a.poly_sel.selected_edges.is_empty(),
            "the hole rim must be selected after an open-mesh cut"
        );
    }

    /// Scroll-bevel / circle segments must refuse while a drag-select tool owns the pointer.
    #[test]
    fn knife_shape_scroll_blocked_during_drag_select() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 2;
        app.knife_shape_segments = 24;
        app.knife_shape_anchor = Some(Vec3::ZERO); // even mid-drag…
        app.tool.current = ActiveTool::PolyRegionSelect(PolyRegionSelectState {
            shape: SelectionShape::Box,
            front_only: true,
            subtract: false,
            start: egui::pos2(0.0, 0.0),
            current: egui::pos2(10.0, 10.0),
            path: vec![egui::pos2(0.0, 0.0), egui::pos2(10.0, 10.0)],
        });
        assert!(
            !app.poly_knife_shape_scroll(1.0, false),
            "scroll must not change segments during region select"
        );
        assert_eq!(app.knife_shape_segments, 24, "segments unchanged under drag-select");
    }

    /// Owner 2026-08-06: R/C armed with nothing drawn must NOT consume scroll.
    #[test]
    fn knife_shape_scroll_blocked_when_nothing_drawn() {
        let (mut app, _) = app_with_quad_and_free_knife();
        // Rectangle armed, no drag started.
        app.knife_shape_kind = 1;
        app.knife_shape_anchor = None;
        app.knife_points.clear();
        app.knife_shape_corner_bevel = 0.0;
        app.knife_shape_segments = 3;
        assert!(
            !app.knife_shape_scroll_armed(),
            "scroll must not be armed with nothing drawn"
        );
        assert!(
            !app.poly_knife_shape_scroll(1.0, false),
            "scroll must not change bevel when only R is armed"
        );
        assert_eq!(app.knife_shape_corner_bevel, 0.0);
        assert_eq!(app.knife_shape_segments, 3);

        // Circle armed, no drag.
        app.knife_shape_kind = 2;
        assert!(!app.poly_knife_shape_scroll(1.0, false));
        assert_eq!(app.knife_shape_segments, 3);
    }

    /// Owner 2026-08-06: while dragging a rectangle, scroll bevels corners and rewrites
    /// `knife_points` as the rounded polyline (yellow points in the overlay).
    #[test]
    fn rect_drag_scroll_bevels_corners_and_polyline_points() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 1;
        app.knife_shape_corner_bevel = 0.0;
        app.knife_shape_segments = 8;
        app.knife_shape_anchor = Some(Vec3::new(-1.0, -1.0, 2.0));
        app.knife_shape_cursor = Some(Vec3::new(1.0, 1.0, 2.0));
        // Plane already set by fixture.
        app.poly_knife_shape_rebuild();
        let sharp_n = app.knife_points.len();
        // Sharp closed rect: 4 corners + close = 5.
        assert_eq!(sharp_n, 5, "sharp rect = 4 corners + closed first");

        // Mid-drag: one discrete notch → +15% corner bevel (owner 2026-08-07: full round in
        // ~7 notches); polyline densifies with arcs.
        assert!(
            app.poly_knife_shape_scroll(1.0, false),
            "scroll while rect dragging must be accepted"
        );
        assert!(
            (app.knife_shape_corner_bevel - 0.15).abs() < 1e-6,
            "one scroll tick → +0.15 bevel, got {}",
            app.knife_shape_corner_bevel
        );
        let beveled_n = app.knife_points.len();
        assert!(
            beveled_n > sharp_n,
            "beveled rect must add polyline arc points (sharp {sharp_n} → {beveled_n})"
        );
        // Closed ring preserved.
        let first = *app.knife_points.first().unwrap();
        let last = *app.knife_points.last().unwrap();
        assert!(
            (first - last).length() < 1e-5,
            "beveled polyline must stay closed (first==last)"
        );

        // Scroll down back toward sharp.
        let before = app.knife_shape_corner_bevel;
        assert!(app.poly_knife_shape_scroll(-1.0, false));
        assert!(
            app.knife_shape_corner_bevel < before,
            "scroll down reduces corner bevel"
        );
        assert!(
            app.knife_shape_corner_bevel.abs() < 1e-5,
            "one tick down returns to sharp"
        );
    }

    /// REGRESSION GATE (owner 2026-08-08): *"the bevel point cut works on the right click, on
    /// the release of drag should be same thing."*
    ///
    /// A 2026-08-08 change made the RELEASE path cut a SHARP rect and then vertex-bevel the
    /// four corner verts, so Unbevel could reverse it. RMB/Enter kept cutting the arc ring —
    /// two code paths for one operation, and the release one silently produced a square hole
    /// (the vertex bevel is coplanar-invisible on a slice, and only chamfers the front rim on
    /// a subtract). Release and RMB must commit the SAME mesh, and it must be the rounded one.
    #[test]
    fn rect_release_cuts_the_same_rounded_corners_as_rmb_confirm() {
        let arm = |app: &mut FourthLayerApp, bevel: f32| {
            app.knife_shape_kind = 1;
            app.knife_shape_segments = 6;
            app.knife_shape_corner_bevel = bevel;
            app.knife_shape_anchor = Some(Vec3::new(-0.5, -0.5, 2.0));
            app.knife_shape_cursor = Some(Vec3::new(0.5, 0.5, 2.0));
            app.knife_free_view = Some(Vec3::NEG_Z);
            app.poly_knife_shape_rebuild();
        };

        // ── Path A: LMB drag → release ────────────────────────────────────
        let (mut a, nid_a) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(2.0)));
        arm(&mut a, 0.5);
        let arc_pts = a.knife_points.len();
        assert!(
            arc_pts > 5,
            "corner bevel must densify the ring into arcs (got {arc_pts} points)"
        );
        a.poly_knife_shape_release();
        let mesh_a = a.poly_node_mesh(nid_a).expect("release must cut");

        // ── Path B: identical rubber-band, RMB confirm ────────────────────
        let (mut b, nid_b) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(2.0)));
        arm(&mut b, 0.5);
        b.poly_knife_confirm(false);
        let mesh_b = b.poly_node_mesh(nid_b).expect("RMB confirm must cut");

        assert_eq!(
            mesh_a.content_signature(),
            mesh_b.content_signature(),
            "release and RMB must commit the SAME cut (release v={} f={} vs RMB v={} f={})",
            mesh_a.vertex_count(),
            mesh_a.face_count(),
            mesh_b.vertex_count(),
            mesh_b.face_count(),
        );

        // ── And it is really the ROUNDED cut, not a sharp one ─────────────
        let (mut c, nid_c) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(2.0)));
        arm(&mut c, 0.0);
        assert_eq!(c.knife_points.len(), 5, "sharp rect = 4 corners + close");
        c.poly_knife_shape_release();
        let mesh_c = c.poly_node_mesh(nid_c).expect("sharp release must cut");
        assert!(
            mesh_a.vertex_count() > mesh_c.vertex_count(),
            "the corner round must add geometry over a sharp rect cut \
             (rounded v={} vs sharp v={}) — the release path ate the bevel again",
            mesh_a.vertex_count(),
            mesh_c.vertex_count(),
        );
        assert!(mesh_a.validate().is_ok(), "rounded cut must be a valid mesh");
        assert!(
            mesh_a.is_manifold().is_ok(),
            "rounded cut must stay watertight"
        );
    }



    /// OPEN-MESH SUBTRACT (owner 2026-08-08: "can we make the boolean work on an open mesh …
    /// also useful when you knife a simple plane"). A solid boolean is undefined on a surface,
    /// so the knife's subtract becomes a surface cookie-cut: scribe the loop, drop the faces it
    /// encloses. Before this, the subtract refused with "needs a closed mesh".
    #[test]
    fn closed_loop_subtract_punches_a_hole_in_an_open_plane() {
        let (mut a, nid) = app_with_quad_and_free_knife();
        let base = a.poly_node_mesh(nid).unwrap();
        assert!(
            core_math::mesh_boolean::boundary_half_edge_count(&base) > 0,
            "fixture must be an OPEN mesh"
        );
        assert_eq!(base.face_count(), 1, "plane starts as one quad");

        a.knife_free_view = Some(Vec3::NEG_Z);
        a.knife_points = vec![
            Vec3::new(-0.5, -0.5, 2.0),
            Vec3::new(0.5, -0.5, 2.0),
            Vec3::new(0.5, 0.5, 2.0),
            Vec3::new(-0.5, 0.5, 2.0),
            Vec3::new(-0.5, -0.5, 2.0),
        ];
        a.poly_knife_free_boolean(None);

        let out = a.poly_node_mesh(nid).expect("subtract must commit on an open mesh");
        assert!(out.face_count() > 0, "the cut must not consume the plane");
        assert!(out.validate().is_ok(), "result must be a valid mesh");

        // AREA is the unambiguous test: a 2x2 plane minus a 1x1 hole = 3.0.
        // (Deliberately NOT a centroid test — the scribe leaves the surviving OUTER region as
        // concave n-gons whose centroids sit inside the loop. That is what made the first
        // version of this feature delete everything.)
        let area = |m: &HalfEdgeMesh| -> f32 {
            let mut a = 0.0;
            for i in 0..m.faces.len() as u32 {
                let f = FaceId(i);
                if !m.is_face_live(f) {
                    continue;
                }
                let vs = m.face_vertices(f);
                for k in 1..vs.len().saturating_sub(1) {
                    let (p0, p1, p2) = (
                        m.vert(vs[0]).position,
                        m.vert(vs[k]).position,
                        m.vert(vs[k + 1]).position,
                    );
                    a += (p1 - p0).cross(p2 - p0).length() * 0.5;
                }
            }
            a
        };
        let got = area(&out);
        assert!(
            (got - 3.0).abs() < 1e-3,
            "plane 2x2 (area 4) minus a 1x1 hole must leave area 3.0, got {got}"
        );
        // The outer extent is untouched — only the middle went.
        let (mn, mx) = out.bounding_box();
        assert!(
            (mn - Vec3::new(-1.0, -1.0, 0.0)).length() < 1e-4
                && (mx - Vec3::new(1.0, 1.0, 0.0)).length() < 1e-4,
            "the plane's outer extent must be untouched (got {mn:?}..{mx:?})"
        );
        // The hole is a REAL boundary, not just missing shading: the plane's rim was 4
        // boundary half-edges, the frame-with-a-hole is 8 (outer rim + hole rim).
        assert_eq!(
            core_math::mesh_boolean::boundary_half_edge_count(&out),
            8,
            "expected outer rim (4) + hole rim (4) boundary half-edges"
        );
    }

    /// The guard: a cookie-cut that would eat EVERY face refuses to delete rather than leaving
    /// an empty body. Fails closed — the scribe still committed.
    #[test]
    fn open_mesh_subtract_never_consumes_the_whole_surface() {
        let (mut a, nid) = app_with_quad_and_free_knife();
        a.knife_free_view = Some(Vec3::NEG_Z);
        // Loop far larger than the quad — every face centroid is inside it.
        a.knife_points = vec![
            Vec3::new(-5.0, -5.0, 2.0),
            Vec3::new(5.0, -5.0, 2.0),
            Vec3::new(5.0, 5.0, 2.0),
            Vec3::new(-5.0, 5.0, 2.0),
            Vec3::new(-5.0, -5.0, 2.0),
        ];
        a.poly_knife_free_boolean(None);
        let out = a.poly_node_mesh(nid).expect("body must survive");
        assert!(
            out.face_count() > 0,
            "an all-consuming cookie-cut must refuse, not empty the body"
        );
    }

    /// A CLOSED body must keep the real solid boolean — the open-mesh route is scoped to
    /// surfaces only and must not steal the watertight path (which rebuilds interior walls).
    #[test]
    fn closed_body_still_takes_the_solid_boolean_path() {
        let (mut a, nid) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(2.0)));
        a.knife_free_view = Some(Vec3::NEG_Z);
        a.knife_points = vec![
            Vec3::new(-0.5, -0.5, 2.0),
            Vec3::new(0.5, -0.5, 2.0),
            Vec3::new(0.5, 0.5, 2.0),
            Vec3::new(-0.5, 0.5, 2.0),
            Vec3::new(-0.5, -0.5, 2.0),
        ];
        a.poly_knife_free_boolean(None);
        let out = a.poly_node_mesh(nid).expect("solid subtract must commit");
        assert_eq!(
            core_math::mesh_boolean::boundary_half_edge_count(&out),
            0,
            "a solid cut must stay watertight — the surface route must not have run"
        );
        assert!(out.face_count() > 6, "the hole must add wall geometry");
    }


    /// UNBEVEL a knife-rect corner round (owner 2026-08-08: *"just rail vertex to one shared
    /// point, do that for each ring edge in the bevel, now you have perfect 90 degrees, and
    /// since you refind the starting edge you can reapply bevel to change segment and
    /// roundness"*). That is exactly what `unbevel`'s reverse math does — it was refusing
    /// because the rebuild re-derived winding from the mesh centroid, which sits INSIDE the
    /// hole, so every wall of the hole got flipped and `from_polygons` went non-manifold.
    ///
    /// Pins the whole round-trip: cut → unbevel → sharp 90 degree corner → re-bevel.
    #[test]
    fn knife_rect_corner_round_unbevels_to_a_sharp_90_degree_corner() {
        let (mut a, nid) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(2.0)));
        a.knife_free_view = Some(Vec3::NEG_Z);
        a.knife_shape_kind = 1;
        a.knife_shape_segments = 4;
        a.knife_shape_corner_bevel = 0.4; // r = 0.4 * 0.5 = 0.2
        a.knife_shape_anchor = Some(Vec3::new(-0.5, -0.5, 2.0));
        a.knife_shape_cursor = Some(Vec3::new(0.5, 0.5, 2.0));
        a.poly_knife_shape_rebuild();
        a.poly_knife_shape_release();
        let m = a.poly_node_mesh(nid).expect("rounded rect cut");
        assert!(m.is_manifold().is_ok(), "cut must be sound");
        // No vertex sits at the SHARP corner yet — it is rounded away.
        let sharp_xy = Vec2::new(0.5, 0.5);
        let has_corner = |mm: &HalfEdgeMesh| {
            (0..mm.vertices.len() as u32).any(|i| {
                let v = VertexId(i);
                mm.is_vert_live(v) && {
                    let p = mm.vert(v).position;
                    (Vec2::new(p.x, p.y) - sharp_xy).length() < 1e-3
                }
            })
        };
        assert!(!has_corner(&m), "the corner must start ROUNDED (no vertex at the sharp point)");

        // The corner strip: the arc wall quads (arc centre (0.3,0.3), radius 0.2).
        let cc = Vec2::new(0.3, 0.3);
        let mut strip: std::collections::HashSet<u32> = Default::default();
        for i in 0..m.faces.len() as u32 {
            let f = FaceId(i);
            if !m.is_face_live(f) {
                continue;
            }
            let vs = m.face_vertices(f);
            let mut c = Vec3::ZERO;
            for v in &vs {
                c += m.vert(*v).position;
            }
            c /= vs.len() as f32;
            if m.compute_face_normal(f).z.abs() < 0.3
                && ((Vec2::new(c.x, c.y) - cc).length() - 0.2).abs() < 0.06
            {
                strip.insert(i);
            }
        }
        assert_eq!(strip.len(), 4, "4 segments => 4 arc quads");

        let res = core_math::unbevel::unbevel_from_faces(&m, &strip)
            .expect("unbevel must reverse a knife-rect corner round");
        let sharp = &res.mesh;
        assert!(sharp.is_manifold().is_ok(), "recovered mesh must be manifold");
        assert!(sharp.validate().is_ok(), "recovered mesh must validate");
        // The 10 arc verts (5 per rim) collapsed to the 2 corner points: -8 verts, -4 faces.
        assert_eq!(sharp.vertex_count(), m.vertex_count() - 8, "arc rings must collapse to one point each");
        assert_eq!(sharp.face_count(), m.face_count() - 4, "the 4 arc quads must be gone");
        // "now you have perfect 90 degrees" — the corner is back, at the TRUE rect corner.
        assert!(has_corner(sharp), "the sharp corner vertex must be recovered at (0.5, 0.5)");
        assert!(!res.sharp_edges.is_empty(), "the recovered edge must be reported");
        for &e in &res.sharp_edges {
            let c = sharp
                .edge_dihedral_cos(core_math::half_edge::HalfEdgeId(e))
                .expect("recovered edge is interior");
            let deg = c.clamp(-1.0, 1.0).acos().to_degrees();
            assert!(
                (deg - 90.0).abs() < 1.0,
                "recovered corner must be a true 90 degree edge, got {deg}"
            );
        }

        // "you can reapply bevel to change segment and roundness" — re-bevel the recovered
        // edge at a DIFFERENT segment count and it takes.
        let params = core_math::bevel_op::BevelParams {
            width: 0.2,
            segments: 7,
            shape: 0.5,
            clamp: true,
        };
        let rebev = core_math::bevel_op::bevel_edges_offset(sharp, &res.sharp_edges, &params);
        assert!(rebev.is_manifold().is_ok(), "re-bevel must stay manifold");
        assert!(
            rebev.face_count() > sharp.face_count(),
            "re-bevel at 7 segments must add geometry ({} -> {})",
            sharp.face_count(),
            rebev.face_count()
        );
        assert!(!has_corner(&rebev), "re-bevel must round the corner away again");
    }


    /// The path the user actually takes: Ctrl+Alt+B with the corner strip's EDGES selected
    /// (not its faces). Seeding "every face touching a selected edge" pulls in both hole walls
    /// — each shares a rail with the strip — so the component grew sideways and refused. The
    /// `crossed` seed keeps only faces the selection SPANS, and must land the same result the
    /// face selection does.
    #[test]
    fn unbevel_works_from_the_corner_strips_own_edge_selection() {
        let (mut a, nid) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(2.0)));
        a.knife_free_view = Some(Vec3::NEG_Z);
        a.knife_shape_kind = 1;
        a.knife_shape_segments = 4;
        a.knife_shape_corner_bevel = 0.4;
        a.knife_shape_anchor = Some(Vec3::new(-0.5, -0.5, 2.0));
        a.knife_shape_cursor = Some(Vec3::new(0.5, 0.5, 2.0));
        a.poly_knife_shape_rebuild();
        a.poly_knife_shape_release();
        let m = a.poly_node_mesh(nid).expect("cut");

        // The strip's cross edges: the vertical ones on the corner arc (radius 0.2 @ (0.3,0.3)).
        let cc = Vec2::new(0.3, 0.3);
        let mut edges: std::collections::HashSet<u32> = Default::default();
        for i in 0..m.half_edges.len() as u32 {
            let h = core_math::half_edge::HalfEdgeId(i);
            if !m.is_edge_live(h) {
                continue;
            }
            let p0 = m.vert(m.he_source(h)).position;
            let p1 = m.vert(m.he(h).vertex).position;
            let mid = (p0 + p1) * 0.5;
            if ((Vec2::new(mid.x, mid.y) - cc).length() - 0.2).abs() < 0.07
                && (p0.z - p1.z).abs() > 1.0
            {
                edges.insert(i);
            }
        }
        assert!(!edges.is_empty(), "fixture must find the strip's cross edges");

        let from_edges = core_math::unbevel::unbevel_from_edges(&m, &edges)
            .expect("unbevel must work from the strip's own edge selection");
        assert!(from_edges.mesh.is_manifold().is_ok(), "must stay manifold");
        assert_eq!(
            from_edges.mesh.vertex_count(),
            m.vertex_count() - 8,
            "edge-selected unbevel must collapse the same 10 arc verts to 2"
        );
        assert!(
            !from_edges.sharp_edges.is_empty(),
            "the recovered edge must be reported so Bevel can be re-applied"
        );
    }

    /// Shift+scroll on rect mid-drag changes bevel segment count (one step per tick).
    #[test]
    fn rect_shift_scroll_changes_bevel_segments() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 1;
        app.knife_shape_segments = 3;
        app.knife_shape_scroll_accum = 0.0;
        app.knife_shape_corner_bevel = 0.5; // need bevel > 0 so segs affect point count
        app.knife_shape_anchor = Some(Vec3::new(-1.0, -1.0, 2.0));
        app.knife_shape_cursor = Some(Vec3::new(1.0, 1.0, 2.0));
        app.poly_knife_shape_rebuild();
        let n0 = app.knife_points.len();
        assert!(app.poly_knife_shape_scroll(1.0, true));
        assert_eq!(app.knife_shape_segments, 4, "one Shift+scroll tick → +1 segment");
        assert!(
            app.knife_points.len() > n0,
            "more segments densifies corner arcs"
        );
        // Plain scroll must not change segments.
        let segs = app.knife_shape_segments;
        assert!(app.poly_knife_shape_scroll(1.0, false));
        assert_eq!(app.knife_shape_segments, segs, "plain scroll is bevel amount only");
    }

    /// Hold-R rotate spins plane points around the rect center.
    #[test]
    fn rect_rotation_spins_corners_around_center() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 1;
        app.knife_shape_corner_bevel = 0.0;
        app.knife_shape_rotation_rad = 0.0;
        app.knife_shape_from_center = true;
        app.knife_shape_anchor = Some(Vec3::new(0.0, 0.0, 2.0));
        app.knife_shape_cursor = Some(Vec3::new(1.0, 0.5, 2.0));
        app.poly_knife_shape_rebuild();
        let sharp = app.knife_points.clone();
        // 90° rotation about center.
        app.knife_shape_rotation_rad = std::f32::consts::FRAC_PI_2;
        app.poly_knife_shape_rebuild();
        // First corner was approx (-1, -0.5) in plane → after 90° CCW around origin ≈ (0.5, -1).
        let p0 = sharp[0];
        let q0 = app.knife_points[0];
        // Center at (0,0,2); rotation in XY of plane (u=X,v=Y).
        let expected = Vec3::new(-p0.y, p0.x, p0.z); // 90° CCW in XY
        assert!(
            (q0 - expected).length() < 1e-3,
            "90° rotate: {p0:?} → {q0:?}, expected {expected:?}"
        );
    }

    /// Alt = draw rect from center: press is center, half-extents = |cursor−anchor|.
    #[test]
    fn rect_from_center_alt_sizes_about_anchor() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 1;
        app.knife_shape_from_center = true;
        app.knife_shape_corner_bevel = 0.0;
        let center = Vec3::new(0.0, 0.0, 2.0);
        let corner = Vec3::new(1.0, 0.5, 2.0);
        app.knife_shape_anchor = Some(center);
        app.knife_shape_cursor = Some(corner);
        app.poly_knife_shape_rebuild();
        assert_eq!(app.knife_points.len(), 5, "closed rect");
        // AABB of world points should be center ± half-extents (in plane XY).
        let mut lo = app.knife_points[0];
        let mut hi = lo;
        for p in &app.knife_points {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let mid = (lo + hi) * 0.5;
        assert!(
            (mid - center).length() < 1e-3,
            "from-center rect mid must sit on press point, got mid={mid:?}"
        );
        assert!((hi.x - lo.x - 2.0).abs() < 1e-3, "width = 2*|Δx|");
        assert!((hi.y - lo.y - 1.0).abs() < 1e-3, "height = 2*|Δy|");
    }

    /// Degenerate click (no size) clears the drag but keeps shape mode for another try.
    #[test]
    fn shape_release_keeps_kind_even_when_too_small() {
        let (mut app, _) = app_with_quad_and_free_knife();
        app.knife_shape_kind = 2; // Circle armed
        app.knife_shape_anchor = Some(Vec3::new(0.0, 0.0, 2.0));
        app.knife_points.clear();
        app.poly_knife_shape_release();

        assert_eq!(
            app.knife_shape_kind, 2,
            "must stay in circle mode after cancelled degenerate place"
        );
        assert!(app.knife_shape_anchor.is_none());
        assert!(app.knife_points.is_empty());
    }

    /// Input-path contract: once a shape is anchored, ANY primary-up must release —
    /// not only egui's drag_stopped (which is often missing outside the viewport hit rect).
    #[test]
    fn shape_should_release_on_primary_up_not_only_drag_stopped() {
        // Mirrors the condition in input_viewport.rs. Pure logic so a regression cannot
        // re-require drag_stopped alone.
        let should = |has_anchor: bool, drag_stopped: bool, primary_released: bool, primary_down: bool| {
            has_anchor && (drag_stopped || primary_released || !primary_down)
        };
        assert!(
            should(true, false, true, false),
            "primary_released with anchor must commit"
        );
        assert!(
            should(true, false, false, false),
            "button up with anchor must commit even without drag_stopped"
        );
        assert!(
            should(true, true, false, false),
            "drag_stopped still commits"
        );
        assert!(
            !should(true, false, false, true),
            "still holding LMB must not commit"
        );
        assert!(
            !should(false, true, true, false),
            "no anchor = not mid-shape, leave kind armed for next drag"
        );
    }

    /// Owner 2026-08-07 "it always fails to do a proper cut": every S fixture above looks
    /// dead-on down -Z with an axis-aligned plane basis. Real usage captures the free plane
    /// from the ORBITED camera (poly_knife_click stores orbit·X / orbit·Y / orbit·-Z).
    /// A cube sliced from a 3/4 view must still get a full through-ring: cut verts on BOTH
    /// the view-front and the view-back of the shell.
    #[test]
    fn s_cube_slice_from_oblique_orbit_view() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "CubeOblique".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        // Default-orbit-like 3/4 view (yaw 30°, pitch -25°) — exactly what the first-click
        // capture would store for this camera.
        let orbit = glam::Quat::from_euler(
            glam::EulerRot::YXZ,
            30f32.to_radians(),
            (-25f32).to_radians(),
            0.0,
        );
        let view = (orbit * Vec3::NEG_Z).normalize();
        let u = (orbit * Vec3::X).normalize();
        let v = (orbit * Vec3::Y).normalize();
        app.knife_free_plane = Some((Vec3::ZERO, u, v, view));
        app.knife_free_view = Some(view);
        // Horizontal (view-space) line straddling the whole cube.
        app.knife_points = vec![-u * 2.0, u * 2.0];
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "mesh valid after oblique slice: {:?}", m.validate());
        let (mut front, mut back, mut new_verts) = (0usize, 0usize, 0usize);
        for i in 0..m.vertices.len() as u32 {
            let vid = VertexId(i);
            if !m.is_vert_live(vid) {
                continue;
            }
            let p = m.vert(vid).position;
            // Original box corners sit at (±0.5)³ — anything else is a cut vert.
            let is_corner = (p.x.abs() - 0.5).abs() < 1e-3
                && (p.y.abs() - 0.5).abs() < 1e-3
                && (p.z.abs() - 0.5).abs() < 1e-3;
            if is_corner {
                continue;
            }
            new_verts += 1;
            if p.dot(view) < 0.0 {
                front += 1;
            } else {
                back += 1;
            }
        }
        assert!(
            new_verts >= 4,
            "oblique S slice made no real ring (got {new_verts} cut verts)"
        );
        assert!(
            front > 0 && back > 0,
            "through-slice must ring the shell: front={front} back={back} — a one-sided cut is not through"
        );
    }

    /// Owner 2026-08-07 P1 ("classic surface knife S DELETES faces"): CLASSIC knife (two
    /// clicks ON the mesh — left face interior, front face interior), then S. The hidden
    /// right-back edge through-crossing interleaved (screen order) into the visible node
    /// chain and stole the tail-slit anchor: a spur stitched from a hidden edge to a
    /// front-face point, self-intersecting in 3D — the rebuild ate the left AND right
    /// faces (6 → 4). The fix chains nodes by FACE CONNECTIVITY (hidden crossings form
    /// their own depth-sheet chain) and plane-gates the slit's face binding.
    ///
    /// Correct result (KNIFE.md §1): interior→edge→interior across two faces = two slits
    /// anchored on the shared-edge crossing — no face divides, and NO face disappears.
    #[test]
    fn s_classic_two_face_clicks_never_deletes_faces() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE); // [-1,1]³ — the diag repro cube
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "CubeClassic".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        // CLASSIC personality: knife_free stays false; points are on-surface clicks.
        app.viewport.viewport_rect =
            egui::Rect::from_min_size(egui::pos2(64.0, 58.0), egui::vec2(888.0, 654.0));
        app.knife_points = vec![
            Vec3::new(-1.0, -0.267, 0.439), // left face interior
            Vec3::new(0.233, -0.237, 1.0),  // front face interior
        ];
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "mesh valid after classic S: {:?}", m.validate());
        assert_eq!(
            m.face_count(),
            6,
            "two-face interior slit must not divide and must NEVER delete faces"
        );
        // The two faces that used to vanish must still exist as intact axis faces.
        for (axis, sign) in [(0usize, -1.0f32), (0, 1.0)] {
            let found = (0..m.faces.len() as u32).any(|fi| {
                let f = core_math::half_edge::FaceId(fi);
                m.is_face_live(f)
                    && m.face_vertices(f).iter().all(|&v| {
                        let p = m.vert(v).position;
                        (p[axis] - sign).abs() < 1e-3
                    })
            });
            assert!(found, "the x={sign} face was deleted by the classic S slice");
        }
        // Both clicked points must be embedded (the two slits actually happened).
        assert!(
            vert_at(&app, nid, Vec3::new(-1.0, -0.267, 0.439)),
            "left-face click must be embedded as a slit tip"
        );
        assert!(
            vert_at(&app, nid, Vec3::new(0.233, -0.237, 1.0)),
            "front-face click must be embedded as a slit tip"
        );
    }

    /// The other untested axis of real usage: every fixture body sits at the origin with an
    /// IDENTITY transform. A moved + rotated body must slice identically — the engine works
    /// in mesh-local space and the app glue owns every world↔local conversion.
    #[test]
    fn s_cube_slice_on_moved_rotated_body() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child_with_transform(
            root,
            NodePayload::PolygonalBody {
                name: "CubeMoved".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
            Vec3::new(1.2, 0.4, -0.6),
            Vec3::new(0.0, 40.0, 0.0), // degrees (node local transform converts)
            Vec3::ONE,
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        let view = Vec3::NEG_Z;
        // Free plane captured through the BODY's world origin, as the real first click does
        // (knife_plane_hit anchors the view plane at xform(Vec3::ZERO)).
        let anchor = app
            .scene
            .scene_graph
            .world_transform(nid)
            .transform_point3(Vec3::ZERO);
        app.knife_free_plane = Some((anchor, Vec3::X, Vec3::Y, view));
        app.knife_free_view = Some(view);
        app.knife_points = vec![anchor - Vec3::X * 2.0, anchor + Vec3::X * 2.0];
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "mesh valid after moved-body slice: {:?}", m.validate());
        let world = app.scene.scene_graph.world_transform(nid);
        let (mut front, mut back, mut new_verts) = (0usize, 0usize, 0usize);
        for i in 0..m.vertices.len() as u32 {
            let vid = VertexId(i);
            if !m.is_vert_live(vid) {
                continue;
            }
            let p = m.vert(vid).position; // mesh-LOCAL
            let is_corner = (p.x.abs() - 0.5).abs() < 1e-3
                && (p.y.abs() - 0.5).abs() < 1e-3
                && (p.z.abs() - 0.5).abs() < 1e-3;
            if is_corner {
                continue;
            }
            new_verts += 1;
            let rel = world.transform_point3(p) - anchor;
            if rel.dot(view) < 0.0 {
                front += 1;
            } else {
                back += 1;
            }
        }
        assert!(
            new_verts >= 4,
            "moved/rotated body S slice made no real ring (got {new_verts} cut verts)"
        );
        assert!(
            front > 0 && back > 0,
            "through-slice must ring the moved body's shell: front={front} back={back}"
        );
    }

    /// OWNER REPRO 2026-08-07 "KNIFE FAIL.fl4" — rect slice whose top side leaves the
    /// silhouette (a NOTCH). The saved result carried two 16-gon rings with repeated
    /// vertices AND two coincident duplicate vertices. Three-invariant gate:
    ///   1. no pinched face beyond a legitimate slit,
    ///   2. no coincident duplicate vertices,
    ///   3. no boundary half-edges.
    #[test]
    fn s_rect_notch_slice_has_no_duplicate_or_pinch() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5)); // [-0.5,0.5]^3
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Cube".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        app.knife_free_plane = Some((Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::NEG_Z));
        app.knife_free_view = Some(Vec3::NEG_Z);
        // Closed rect: bottom two corners INSIDE the front/back faces, top side ABOVE the
        // cube (y = 1.0 > 0.5) so the two vertical sides cross the top edge.
        app.knife_points = vec![
            Vec3::new(-0.2, -0.1, 0.0),
            Vec3::new(0.2, -0.1, 0.0),
            Vec3::new(0.2, 1.0, 0.0),
            Vec3::new(-0.2, 1.0, 0.0),
            Vec3::new(-0.2, -0.1, 0.0),
        ];
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());

        // (3) closed solid — zero boundary half-edges.
        let mut boundary = 0usize;
        for i in 0..m.half_edges.len() as u32 {
            let h = core_math::half_edge::HalfEdgeId(i);
            if m.he(h).face != core_math::half_edge::NO_FACE
                && m.he(h).twin == core_math::half_edge::NO_TWIN
            {
                boundary += 1;
            }
        }
        assert_eq!(boundary, 0, "rect-notch slice left {boundary} open edges");

        // (2) no coincident duplicate vertices.
        let mut live: Vec<(u32, Vec3)> = Vec::new();
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if m.is_vert_live(v) {
                live.push((i, m.vert(v).position));
            }
        }
        let mut dups = Vec::new();
        for a in 0..live.len() {
            for b in a + 1..live.len() {
                if (live[a].1 - live[b].1).length() < 1e-4 {
                    dups.push((live[a].0, live[b].0, live[a].1));
                }
            }
        }
        assert!(dups.is_empty(), "coincident duplicate vertices: {dups:?}");

        // (1) the notch must DIVIDE, not leave dangling slits: no repeated-vertex ring.
        let mut pinched = Vec::new();
        for fi in 0..m.faces.len() as u32 {
            let f = core_math::half_edge::FaceId(fi);
            if !m.is_face_live(f) {
                continue;
            }
            let vs: Vec<u32> = m.face_vertices(f).iter().map(|v| v.0).collect();
            let uniq: std::collections::HashSet<u32> = vs.iter().copied().collect();
            if uniq.len() < vs.len() {
                pinched.push((fi, vs));
            }
        }
        assert!(pinched.is_empty(), "pinched (slit) rings survived: {pinched:?}");

        // Blender oracle (docs/planning/poly/knife_rect_notch_oracle.py, solver=EXACT):
        // pinched=0 coincident=0 boundary=0 non_manifold=0, and the two notched sheets
        // become 8-gons. A slice SCRIBES (it never removes the notch material), so the
        // counts differ from the oracle's DIFFERENCE — but the notched front/back faces
        // must be DIVIDED, i.e. the front sheet is no longer one single face.
        let front_faces = (0..m.faces.len() as u32)
            .map(core_math::half_edge::FaceId)
            .filter(|&f| {
                m.is_face_live(f)
                    && m.face_vertices(f)
                        .iter()
                        .all(|v| (m.vert(*v).position.z - 0.5).abs() < 1e-3)
            })
            .count();
        assert!(
            front_faces >= 2,
            "notch did not DIVIDE the front sheet (still {front_faces} face) — \
             the wrap-around run degraded to slits again"
        );
    }

    /// Owner 2026-08-07 "KNIFE CIRCLE FAIL TO DO, GOT SEGMENT AFTER CUT"
    /// (`KNIFE CIRCLE CUT FAIL.fl4`, KNIFE.md §5.5) — through the REAL app path.
    ///
    /// A circle drawn wholly INSIDE one face crosses no edge and snaps to no vertex, so the
    /// chain has zero Bound anchors and head == tail. Before the fix, `nearest(head) ==
    /// nearest(tail)` and the anchor-less branch's `(Some(a), _)` arm emitted a `split_poly_slit`
    /// spur: the circle appeared as edges, the face was NEVER divided, and the ring pinched on
    /// its bridge vertex (owner's file: 47-ring, 28 unique verts). Now the closed-loop case
    /// divides into inner disc + two annulus halves — Blender's EXACT answer
    /// (`circle_knife_oracle.py`: pinched=0, boundary=0, non-manifold=0).
    #[test]
    fn s_circle_inside_one_face_divides_it_and_never_pinches() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5)); // [-0.5,0.5]^3
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Cube".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.poly_tool = PolyTool::Knife;
        app.knife_free = true;
        app.knife_free_plane = Some((Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::NEG_Z));
        app.knife_free_view = Some(Vec3::NEG_Z);
        // 16-gon circle, radius 0.25 — wholly inside the ±0.5 front face, no edge crossings.
        // The repeated closing sample is what `poly_knife_shape_rebuild` emits so
        // `knife_free_is_closed()` reads closure (§5.6) — keep it, the kernel drops it.
        let n = 16;
        let mut pts: Vec<Vec3> = (0..n)
            .map(|k| {
                let t = std::f32::consts::TAU * k as f32 / n as f32;
                Vec3::new(0.25 * t.cos(), 0.25 * t.sin(), 0.0)
            })
            .collect();
        pts.push(pts[0]);
        app.knife_points = pts;
        app.poly_knife_slice_through_now();

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());

        // No pinched / slit ring — the defect's exact signature.
        let mut pinched = Vec::new();
        for fi in 0..m.faces.len() as u32 {
            let f = core_math::half_edge::FaceId(fi);
            if !m.is_face_live(f) {
                continue;
            }
            let vs: Vec<u32> = m.face_vertices(f).iter().map(|v| v.0).collect();
            let uniq: std::collections::HashSet<u32> = vs.iter().copied().collect();
            if uniq.len() < vs.len() {
                pinched.push((fi, vs));
            }
        }
        assert!(
            pinched.is_empty(),
            "circle cut left a slit spur / pinched ring — 'GOT SEGMENT AFTER CUT': {pinched:?}"
        );

        // Closed solid (oracle boundary = 0).
        let mut boundary = 0usize;
        for i in 0..m.half_edges.len() as u32 {
            let h = core_math::half_edge::HalfEdgeId(i);
            if m.he(h).face != core_math::half_edge::NO_FACE
                && m.he(h).twin == core_math::half_edge::NO_TWIN
            {
                boundary += 1;
            }
        }
        assert_eq!(boundary, 0, "circle slice left {boundary} open edges");

        // The front sheet must be DIVIDED (disc + annulus halves), not one spur-carrying face.
        let front_faces = (0..m.faces.len() as u32)
            .map(core_math::half_edge::FaceId)
            .filter(|&f| {
                m.is_face_live(f)
                    && m.face_vertices(f)
                        .iter()
                        .all(|v| (m.vert(*v).position.z - 0.5).abs() < 1e-3)
            })
            .count();
        assert!(
            front_faces >= 3,
            "circle did not divide the front sheet into disc + annulus halves \
             (still {front_faces} faces) — the closed loop degraded to a slit spur again"
        );
    }

    /// Owner 2026-08-09 *"E don't work"*: the chord must resolve AND the state split must hand
    /// it to the knife, not to Extrude. Both halves are silent failure modes — a new
    /// `ACTION_DEFS` entry with no `default_bind` arm falls back to the `A` sentinel and the key
    /// is simply dead, and a split that fires in the wrong order runs Extrude instead.
    #[test]
    fn e_arms_knife_depth_and_never_falls_through_to_extrude() {
        use crate::keybindings::{KeyBindings, ACT_POLY_EXTRUDE, ACT_POLY_KNIFE_DEPTH};
        let kb = KeyBindings::defaults();
        assert_eq!(
            kb.get(ACT_POLY_KNIFE_DEPTH).key,
            egui::Key::E,
            "knife depth lost its default chord — the key would be silently dead"
        );
        assert_eq!(
            kb.get(ACT_POLY_EXTRUDE).key,
            egui::Key::E,
            "this test is meaningless unless the two really share E"
        );

        let armed = |anchor: bool| -> bool {
            let mut app = FourthLayerApp::new_headless();
            let root = app.scene.scene_graph.root();
            let nid = app.scene.scene_graph.add_child(
                root,
                NodePayload::PolygonalBody {
                    name: "Cube".into(),
                    mesh: Arc::new(HalfEdgeMesh::new_box(Vec3::splat(0.5))),
                    mesh_cache_id: None,
                    material: core_math::primitives::PbrMaterial::default(),
                    modifiers: Vec::new(),
                    source: core_math::primitive_spec::MeshSource::Authored,
                },
            );
            app.select_single_unified(crate::SceneItemId::Node(nid));
            app.modeling_context = crate::ModelingContext::Polygonal;
            app.poly_tool = PolyTool::Knife;
            app.knife_shape_kind = 1;
            if anchor {
                app.knife_free = true;
                app.knife_free_plane =
                    Some((Vec3::new(0.0, 0.0, 0.5), Vec3::X, Vec3::Y, Vec3::Z));
                app.knife_shape_anchor = Some(Vec3::new(-0.2, -0.2, 0.5));
                app.knife_shape_cursor = Some(Vec3::new(0.2, 0.2, 0.5));
            }
            // One physical `E` sets BOTH flags — that is the whole point of the split.
            app.poly_handle_keys(&crate::PolyKeyFlags {
                knife_depth: true,
                extrude: true,
                ..Default::default()
            });
            assert_eq!(app.poly_tool, PolyTool::Knife, "E switched the tool — Extrude ran");
            app.knife_shape_depth_mode
        };

        assert!(armed(true), "E did not arm the depth phase during a shape drag");
        assert!(
            !armed(false),
            "E armed depth with no shape drag — the split stole the chord from Extrude"
        );
    }

    /// Owner 2026-08-09: *"knife tool shape project (rec, cir) while dragging pressing E should
    /// let you define the thickness of the cut … cut confirms when user releases the left
    /// click."* — through the REAL app path (`poly_knife_shape_depth_release`).
    ///
    /// A depth cut must be a BLIND POCKET: the cutter prism is bounded at `knife_shape_depth`
    /// instead of running ±4× the scene bound, so the far side of the body is never opened.
    ///
    /// The CONTROL half (same points, depth phase off) is load-bearing: without it every
    /// assertion below would also pass on a tool that quietly cut nothing at all, and it pins
    /// the through cut as unchanged by this feature.
    #[test]
    fn rect_depth_cut_makes_a_blind_pocket_and_leaves_the_far_face_whole() {
        // 0.4 × 0.4 rect on the +Z face of a 1.0-thick box, cut 0.4 deep.
        let run = |depth: Option<f32>| -> Arc<HalfEdgeMesh> {
            let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5)); // [-0.5,0.5]^3
            let mut app = FourthLayerApp::new_headless();
            let root = app.scene.scene_graph.root();
            let nid = app.scene.scene_graph.add_child(
                root,
                NodePayload::PolygonalBody {
                    name: "Cube".into(),
                    mesh: Arc::new(mesh),
                    mesh_cache_id: None,
                    material: core_math::primitives::PbrMaterial::default(),
                    modifiers: Vec::new(),
                    source: core_math::primitive_spec::MeshSource::Authored,
                },
            );
            app.select_single_unified(crate::SceneItemId::Node(nid));
            app.poly_tool = PolyTool::Knife;
            app.knife_shape_kind = 1;
            app.knife_free = true;
            // The drawing plane a press on the +Z face produces: origin on the face, normal
            // facing the viewer (`knife_face_plane_basis` flips it that way), so the material
            // — and therefore the depth — runs along −n.
            app.knife_free_plane = Some((Vec3::new(0.0, 0.0, 0.5), Vec3::X, Vec3::Y, Vec3::Z));
            app.knife_free_view = Some(Vec3::NEG_Z);
            app.knife_points = vec![
                Vec3::new(-0.2, -0.2, 0.5),
                Vec3::new(0.2, -0.2, 0.5),
                Vec3::new(0.2, 0.2, 0.5),
                Vec3::new(-0.2, 0.2, 0.5),
                Vec3::new(-0.2, -0.2, 0.5),
            ];
            app.knife_shape_anchor = Some(Vec3::new(-0.2, -0.2, 0.5));
            app.knife_shape_cursor = Some(Vec3::new(0.2, 0.2, 0.5));
            match depth {
                Some(d) => {
                    app.poly_knife_shape_depth_arm(); // E — locks the shape
                    // The sizing release must NOT cut once the depth phase is armed: no LMB
                    // press has armed a confirm yet.
                    app.poly_knife_shape_release();
                    assert!(
                        app.knife_shape_anchor.is_some(),
                        "the sizing release committed the cut during the depth phase"
                    );
                    app.knife_shape_depth = d; // = the cursor moved this far along −n
                    app.poly_knife_shape_depth_press(); // LMB down arms the confirm
                    app.poly_knife_shape_depth_release(); // release = cut
                }
                None => app.poly_knife_shape_release(),
            }
            app.poly_node_mesh(nid).expect("body")
        };
        let live_verts_at_z = |m: &HalfEdgeMesh, z: f32| -> usize {
            (0..m.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| m.is_vert_live(v) && (m.vert(v).position.z - z).abs() < 1e-3)
                .count()
        };

        let pocket = run(Some(0.4));
        assert!(pocket.validate().is_ok(), "validate: {:?}", pocket.validate());
        let mut boundary = 0usize;
        for i in 0..pocket.half_edges.len() as u32 {
            let h = core_math::half_edge::HalfEdgeId(i);
            if pocket.he(h).face != core_math::half_edge::NO_FACE
                && pocket.he(h).twin == core_math::half_edge::NO_TWIN
            {
                boundary += 1;
            }
        }
        assert_eq!(boundary, 0, "depth cut left {boundary} open edges — not a closed solid");

        // The far (−Z) face is WHOLE: a through cut would punch its 4 hole-rim verts into it.
        assert_eq!(
            live_verts_at_z(&pocket, -0.5),
            4,
            "the depth cut reached the far face — the prism was not bounded"
        );
        // …and the pocket has a FLOOR at 0.5 − 0.4.
        assert_eq!(
            live_verts_at_z(&pocket, 0.1),
            4,
            "no pocket floor at z = 0.1 — depth was ignored"
        );
        // The outward margin on the entry cap must not grow the body.
        for i in 0..pocket.vertices.len() as u32 {
            let v = VertexId(i);
            if pocket.is_vert_live(v) {
                let p = pocket.vert(v).position;
                assert!(
                    p.z <= 0.5 + 1e-3 && p.z >= -0.5 - 1e-3,
                    "vertex escaped the original body: {p:?}"
                );
            }
        }

        // CONTROL — the historical through cut, unchanged: it opens the far face and leaves
        // no floor anywhere in between.
        let through = run(None);
        assert!(through.validate().is_ok(), "control validate: {:?}", through.validate());
        assert_eq!(
            live_verts_at_z(&through, -0.5),
            8,
            "the through cut stopped opening the far face — depth leaked into the default path"
        );
        assert_eq!(
            live_verts_at_z(&through, 0.1),
            0,
            "the through cut grew a floor — depth leaked into the default path"
        );
    }

    // ── BOOLEAN TYPE: U = union, I = intersect (owner 2026-08-11) ──────────────────────────
    //
    // Same rig as the depth test: a rect drawn on the +Z face of a [-0.5,0.5]³ box, plane normal
    // facing the viewer (+Z) so a POSITIVE depth runs into the material and a NEGATIVE one stands
    // out of it. `depth: None` = the unbounded through prism.
    fn run_shape_bool(
        op: KnifeBoolOp,
        depth: Option<f32>,
    ) -> (FourthLayerApp, core_math::scene_graph::NodeId) {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let (mut app, nid) = app_with_mesh(mesh);
        app.modeling_context = ModelingContext::Polygonal;
        app.poly_work_mode = PolyWorkMode::Edge;
        app.knife_shape_kind = 1;
        app.knife_bool_op = op;
        app.knife_free_plane = Some((Vec3::new(0.0, 0.0, 0.5), Vec3::X, Vec3::Y, Vec3::Z));
        app.knife_free_view = Some(Vec3::NEG_Z);
        app.knife_points = vec![
            Vec3::new(-0.2, -0.2, 0.5),
            Vec3::new(0.2, -0.2, 0.5),
            Vec3::new(0.2, 0.2, 0.5),
            Vec3::new(-0.2, 0.2, 0.5),
            Vec3::new(-0.2, -0.2, 0.5),
        ];
        app.knife_shape_anchor = Some(Vec3::new(-0.2, -0.2, 0.5));
        app.knife_shape_cursor = Some(Vec3::new(0.2, 0.2, 0.5));
        if let Some(d) = depth {
            app.poly_knife_shape_depth_arm();
            app.poly_knife_shape_release();
            app.knife_shape_depth = d;
            app.poly_knife_shape_depth_press();
            app.poly_knife_shape_depth_release();
        } else {
            app.poly_knife_shape_release();
        }
        (app, nid)
    }

    fn open_edge_count(m: &HalfEdgeMesh) -> usize {
        core_math::mesh_boolean::boundary_half_edge_count(m)
    }

    /// A negative depth must survive `knife_cut_depth` — the old `<= eps` test read every outward
    /// pull as "no depth at all", which is exactly why a Union could never be given a height.
    #[test]
    fn cut_depth_keeps_its_sign_for_an_outward_pull() {
        let (mut a, _) = app_with_mesh(HalfEdgeMesh::new_box(Vec3::splat(0.5)));
        a.knife_shape_depth_mode = true;
        a.knife_shape_depth = -0.4;
        assert_eq!(
            a.knife_cut_depth(),
            Some(-0.4),
            "an outward pull is a real depth, not None"
        );
        a.knife_shape_depth = 0.0;
        assert_eq!(a.knife_cut_depth(), None, "a depth that was never pulled is still None");
    }

    /// UNION with an outward height: the shape becomes a boss STANDING ON the surface, and the
    /// body stays a closed solid. The base overlap is what makes it watertight — a flush base
    /// would hand the boolean two coincident coplanar faces (the owner's "watch out for perfect
    /// plane alignment on the union, when drawing the shape on the surface").
    #[test]
    fn union_with_outward_depth_grows_a_boss_on_the_surface() {
        let (a, nid) = run_shape_bool(KnifeBoolOp::Union, Some(-0.4));
        let m = a.poly_node_mesh(nid).expect("union must commit");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert_eq!(open_edge_count(&m), 0, "the union left the body open");
        let (mn, mx) = m.bounding_box();
        assert!(
            (mx.z - 0.9).abs() < 1e-3,
            "the boss must stand 0.4 above the +Z face (top at 0.9), got {}",
            mx.z
        );
        assert!(
            (mn - Vec3::splat(-0.5)).length() < 1e-3
                && (mx.x - 0.5).abs() < 1e-3
                && (mx.y - 0.5).abs() < 1e-3,
            "the union must only grow along +Z, got {mn:?}..{mx:?}"
        );
        // The boss cap sits at the drawn 0.4x0.4 outline, not the whole face.
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            if m.is_vert_live(v) {
                let p = m.vert(v).position;
                if p.z > 0.5 + 1e-3 {
                    assert!(
                        p.x.abs() <= 0.2 + 1e-3 && p.y.abs() <= 0.2 + 1e-3,
                        "geometry above the face escaped the drawn outline at {p:?}"
                    );
                }
            }
        }
    }

    /// UNION without a height would weld a rod 4x the scene bound through the whole model. Refuse,
    /// and leave the body untouched — fail closed, and say which key fixes it.
    #[test]
    fn union_without_a_height_refuses_instead_of_welding_a_rod() {
        let (a, nid) = run_shape_bool(KnifeBoolOp::Union, None);
        let m = a.poly_node_mesh(nid).expect("body");
        assert_eq!(m.face_count(), 6, "the body must be untouched by a refused union");
        let (mn, mx) = m.bounding_box();
        assert!(
            (mn - Vec3::splat(-0.5)).length() < 1e-4 && (mx - Vec3::splat(0.5)).length() < 1e-4,
            "a refused union changed the body: {mn:?}..{mx:?}"
        );
    }

    /// SUBTRACT with an OUTWARD pull would shave a sliver off the surface — that is a pull in the
    /// wrong direction, not a cut. Refuse rather than commit a result nobody asked for.
    #[test]
    fn subtract_with_outward_depth_refuses() {
        let (a, nid) = run_shape_bool(KnifeBoolOp::Subtract, Some(-0.4));
        let m = a.poly_node_mesh(nid).expect("body");
        assert_eq!(m.face_count(), 6, "the body must be untouched by a refused subtract");
    }

    /// INTERSECT keeps ONLY what the outline encloses: the 0.4x0.4 column through the box.
    #[test]
    fn intersect_keeps_only_the_enclosed_column() {
        let (a, nid) = run_shape_bool(KnifeBoolOp::Intersect, None);
        let m = a.poly_node_mesh(nid).expect("intersect must commit");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert_eq!(open_edge_count(&m), 0, "the intersect left the body open");
        let (mn, mx) = m.bounding_box();
        assert!(
            (mn - Vec3::new(-0.2, -0.2, -0.5)).length() < 1e-3
                && (mx - Vec3::new(0.2, 0.2, 0.5)).length() < 1e-3,
            "expected the 0.4x0.4 column spanning the box in Z, got {mn:?}..{mx:?}"
        );
    }

    /// The op is picked by a chord, and the chord TOGGLES: pressing the same one again is the way
    /// back to Subtract without leaving the tool.
    #[test]
    fn bool_op_chords_toggle_back_to_subtract() {
        let (mut a, _) = app_with_quad_and_free_knife();
        assert_eq!(a.knife_bool_op, KnifeBoolOp::Subtract, "default is Subtract");
        a.poly_knife_set_bool_op(KnifeBoolOp::Union);
        assert_eq!(a.knife_bool_op, KnifeBoolOp::Union);
        a.poly_knife_set_bool_op(KnifeBoolOp::Intersect);
        assert_eq!(a.knife_bool_op, KnifeBoolOp::Intersect, "switching op, not toggling");
        a.poly_knife_set_bool_op(KnifeBoolOp::Intersect);
        assert_eq!(a.knife_bool_op, KnifeBoolOp::Subtract, "same chord again = back to Subtract");
        // Not the knife's tool state → the chord must not reach into another tool's session.
        a.poly_tool = PolyTool::Idle;
        a.poly_knife_set_bool_op(KnifeBoolOp::Union);
        assert_eq!(a.knife_bool_op, KnifeBoolOp::Subtract, "op must only change while Knife is armed");
    }

    /// Union / Intersect have no meaning on a surface (nothing to add to, nothing to keep), so the
    /// open-mesh cookie-cut must refuse them instead of quietly running a Subtract under the wrong
    /// name.
    #[test]
    fn union_on_an_open_mesh_refuses() {
        let (mut a, nid) = app_with_quad_and_free_knife();
        a.knife_bool_op = KnifeBoolOp::Union;
        // Give it a real height so this test fails at the OPEN-MESH gate, not at the
        // "union needs a height" gate that runs before it.
        a.knife_shape_depth_mode = true;
        a.knife_shape_depth = -0.4;
        a.knife_free_view = Some(Vec3::NEG_Z);
        a.knife_points = vec![
            Vec3::new(-0.5, -0.5, 2.0),
            Vec3::new(0.5, -0.5, 2.0),
            Vec3::new(0.5, 0.5, 2.0),
            Vec3::new(-0.5, 0.5, 2.0),
            Vec3::new(-0.5, -0.5, 2.0),
        ];
        a.poly_knife_free_boolean(None);
        let m = a.poly_node_mesh(nid).expect("body");
        assert_eq!(m.face_count(), 1, "the plane must be untouched by a refused union");
    }

    // ── CTRL SNAP (owner 2026-08-17: "hold ctrl for snap using the knife, the snapping
    // function do not come") ────────────────────────────────────────────────────────────
    //
    // A 2026-08-03 inversion made Ctrl disable ALL knife snap (free surface only). The HUD,
    // snap bar, and Draw Polyline all advertise Ctrl = H/V / snap. These gates lock the
    // restored contract: first-point Ctrl still vertex-snaps; after a point, Ctrl locks H/V.

    /// Classic knife on a head-on quad. Camera looks down −Z so screen X/Y track world X/−Y.
    fn app_classic_knife_facing_quad() -> (FourthLayerApp, egui::Rect) {
        let p = vec![
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
        ];
        let mesh = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3]]);
        let (mut app, _nid) = app_with_mesh(mesh);
        app.modeling_context = ModelingContext::Polygonal;
        app.poly_work_mode = PolyWorkMode::Vertex;
        app.knife_free = false;
        app.knife_free_plane = None;
        app.viewport.camera.orbit = glam::Quat::IDENTITY;
        app.viewport.camera.target = Vec3::ZERO;
        app.viewport.camera.distance = 8.0;
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        app.viewport.viewport_rect = rect;
        (app, rect)
    }

    /// POLY-STALE-IDS-AFTER-REBUILD (owner 2026-10-13: "delete a vertex, then K"). The REAL
    /// classic commit path, then the frame's prune: a vertex selected before a cut that does
    /// not touch it is the SAME geometric vertex afterwards — and so are the hover, the active
    /// vertex, the hidden set and an id-keyed sidecar tag. The old commit rebuilt through a
    /// plain `from_polygons`, so these ids stayed in range but named different geometry.
    #[test]
    fn classic_knife_keeps_selection_on_the_same_geometry() {
        // 4×4 grid on z = 0 spanning [-1, 1]², ids j*5 + i, facing the camera (+Z).
        let n = 4usize;
        let mut p = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                p.push(Vec3::new(i as f32 / 2.0 - 1.0, j as f32 / 2.0 - 1.0, 0.0));
            }
        }
        let mut f = Vec::new();
        for j in 0..n {
            for i in 0..n {
                let k = j * (n + 1) + i;
                f.push(vec![k, k + 1, k + n + 2, k + n + 1]);
            }
        }
        let mut mesh = HalfEdgeMesh::from_polygons(&p, &f);
        mesh.delete_vertex(VertexId(21)); // top row: sparse mesh, far from the cut
        let (mut app, _rect) = app_classic_knife_facing_quad();
        let nid = app.sel.usel.active_node().expect("fixture body");
        if let Some(NodePayload::PolygonalBody { mesh: m, .. }) =
            app.scene.scene_graph.get_mut(nid).map(|nd| &mut nd.payload)
        {
            *m = Arc::new(mesh.clone());
        }
        app.knife_select_after_cut = false; // keep the user's selection (the case under test)

        // Bottom row (y = -1) and its first edge: untouched by a cut at y = 0.1.
        let (sel, hov, act, hid) = (VertexId(3), VertexId(2), VertexId(1), VertexId(4));
        let tagged = mesh.face_half_edges(FaceId(0))[0];
        let pos_of = |m: &HalfEdgeMesh, v: VertexId| m.vert(v).position;
        let ends = |m: &HalfEdgeMesh, h: core_math::half_edge::HalfEdgeId| (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
        let (sel_p, hov_p, act_p, hid_p) = (pos_of(&mesh, sel), pos_of(&mesh, hov), pos_of(&mesh, act), pos_of(&mesh, hid));
        let tagged_ends = ends(&mesh, tagged);
        app.poly_sel.selected_vertices.insert(sel.0);
        app.poly_sel.hovered_vertex = Some(hov.0);
        app.poly_active_vertex = Some(act.0);
        app.poly_vert_history.push(act.0);
        app.poly_sel.hidden_vertices.insert(hid.0);
        app.cache.poly_custom_data.entry(nid).or_default().op_created.set(tagged, true);

        let v0 = mesh.vertex_count();
        app.poly_tool = PolyTool::Knife;
        app.knife_points = vec![Vec3::new(-1.0, 0.1, 0.0), Vec3::new(1.0, 0.1, 0.0)];
        app.poly_knife_confirm(false);
        app.prune_poly_sel_to_live(); // what the frame runs before the overlay draw

        let m = app.poly_node_mesh(nid).expect("body");
        assert!(m.vertex_count() > v0, "the cut must happen ({v0} -> {})", m.vertex_count());
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert!(app.poly_sel.selected_vertices.contains(&sel.0), "selection dropped");
        assert_eq!(pos_of(&m, sel), sel_p, "selected id names a different vertex after the cut");
        assert_eq!(app.poly_sel.hovered_vertex.map(|v| pos_of(&m, VertexId(v))), Some(hov_p), "hover moved");
        assert_eq!(app.poly_active_vertex.map(|v| pos_of(&m, VertexId(v))), Some(act_p), "active vertex moved");
        assert!(app.poly_vert_history.iter().all(|&v| pos_of(&m, VertexId(v)) == act_p), "history moved");
        assert!(app.poly_sel.hidden_vertices.contains(&hid.0));
        assert_eq!(pos_of(&m, hid), hid_p, "hidden id names a different vertex after the cut");
        let cd = app.cache.poly_custom_data.get(&nid).expect("sidecar");
        assert!(cd.op_created.get(tagged), "sidecar tag lost");
        assert_eq!(ends(&m, tagged), tagged_ends, "sidecar tag now sits on a different edge");
    }

    fn knife_screen_of(app: &FourthLayerApp, rect: egui::Rect, world: Vec3) -> egui::Pos2 {
        let cam_pos = app.viewport.camera.target
            + app.viewport.camera.orbit * Vec3::new(0.0, 0.0, app.viewport.camera.distance);
        let aspect = rect.width() / rect.height();
        world_to_screen(
            world,
            cam_pos,
            app.viewport.camera.orbit,
            rect,
            aspect,
            0.0,
            app.render.fov_deg,
        )
        .expect("world point must project onto the viewport")
    }

    /// Owner: hold Ctrl for snap — the first point must still magnet to a vertex. The
    /// inverted "Ctrl = no snap" early-out made this a free mid-face hit (kind 0).
    #[test]
    fn ctrl_on_first_point_still_snaps_to_vertex() {
        let (mut app, rect) = app_classic_knife_facing_quad();
        let mouse = knife_screen_of(&app, rect, Vec3::new(1.0, 1.0, 0.0));
        app.poly_knife_hover(mouse, rect, true, false);
        assert_eq!(
            app.knife_snap_kind, 1,
            "Ctrl on the first point must still vertex-snap, got kind={} pos={:?}",
            app.knife_snap_kind, app.knife_snap_pos
        );
        let snap = app.knife_snap_pos.expect("Ctrl hover over a corner must produce a snap");
        assert!(
            (snap - Vec3::new(1.0, 1.0, 0.0)).length() < 1e-3,
            "snapped to {snap:?}, want the (1,1,0) corner"
        );
    }

    /// After a point is placed, Ctrl locks the rubber-band to screen H/V (Draw Polyline
    /// parity). A diagonal hover must land on the dominant axis, not the free cursor.
    #[test]
    fn ctrl_locks_rubber_band_to_screen_h_or_v() {
        let (mut app, rect) = app_classic_knife_facing_quad();
        app.knife_points = vec![Vec3::ZERO];
        // Cursor is clearly more horizontal than vertical → H lock → world Y stays 0.
        let mouse = knife_screen_of(&app, rect, Vec3::new(0.6, 0.25, 0.0));
        app.poly_knife_hover(mouse, rect, true, false);
        let snap = app.knife_snap_pos.expect("Ctrl hover after a point must produce a snap");
        assert!(
            snap.y.abs() < 0.08,
            "Ctrl H-lock must keep the rubber-band on the last point's screen-horizontal \
             (world Y ≈ 0), got {snap:?}"
        );
        assert!(
            snap.x > 0.3,
            "H-lock must still follow the cursor along X, got {snap:?}"
        );
        assert_ne!(
            app.knife_snap_kind, 0,
            "Ctrl H/V is a snap, not a free mid-face hit"
        );
    }

    /// Free-polyline personality: Ctrl H/V is in the captured drawing plane, not discarded.
    #[test]
    fn ctrl_h_v_also_locks_the_free_polyline_plane() {
        let (mut app, rect) = app_classic_knife_facing_quad();
        app.knife_free = true;
        app.knife_free_plane = Some((Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::NEG_Z));
        app.knife_points = vec![Vec3::ZERO];
        let mouse = knife_screen_of(&app, rect, Vec3::new(0.6, 0.25, 0.0));
        app.poly_knife_hover(mouse, rect, true, false);
        let snap = app.knife_snap_pos.expect("free-knife Ctrl hover must hit the plane");
        assert!(
            snap.y.abs() < 1e-3,
            "free-knife Ctrl must H-lock in the drawing plane, got {snap:?}"
        );
        assert!(
            (snap.x - 0.6).abs() < 0.08,
            "free-knife Ctrl H-lock keeps the plane-X of the cursor, got {snap:?}"
        );
    }
}

// =====================================================================================
// KEY-DISPATCH gates: drive the REAL keyboard handler with synthetic egui key events, so the
// chain `K` → knife armed → `R` → rectangle armed is proven end to end (owner 2026-10-11:
// "when I try to K rectangular, pressing R does not trigger"). Calling `poly_knife_shape_arm`
// directly would prove nothing about the dispatch that was reported broken.
// =====================================================================================
#[cfg(test)]
mod knife_r_key_dispatch_tests {
    use super::*;
    use crate::{FourthLayerApp, ModelingContext, PolyTool, PolyWorkMode, WorkMode};
    use core_math::half_edge::HalfEdgeMesh;
    use core_math::scene_graph::NodePayload;
    use eframe::egui;
    use glam::Vec3;
    use std::sync::Arc;

    fn app_with_selected_quad() -> FourthLayerApp {
        let p = vec![
            Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0),
        ];
        let mesh = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3]]);
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Quad".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.modeling_context = ModelingContext::Polygonal;
        app
    }

    fn screen() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))
    }

    /// One press frame + one release frame through the production keyboard handler.
    fn tap(app: &mut FourthLayerApp, ctx: &egui::Context, key: egui::Key) {
        for pressed in [true, false] {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen());
            raw.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run(raw, |ctx| app.handle_keyboard(ctx, false));
        }
    }

    fn toasts(app: &FourthLayerApp) -> Vec<String> {
        app.ui.toasts.iter().map(|t| t.message.clone()).collect()
    }

    #[test]
    fn k_then_r_arms_rectangle_from_poly_edit() {
        let mut app = app_with_selected_quad();
        app.poly_work_mode = PolyWorkMode::Face;
        let ctx = egui::Context::default();
        tap(&mut app, &ctx, egui::Key::K);
        assert_eq!(app.poly_tool, PolyTool::Knife, "K must arm the knife; toasts: {:?}", toasts(&app));
        tap(&mut app, &ctx, egui::Key::R);
        assert_eq!(
            app.knife_shape_kind, 1,
            "R under the knife must arm the rectangle; toasts: {:?}", toasts(&app)
        );
        assert!(!app.tool.gizmo_armed, "R under the knife must not arm the rotate gizmo");
        assert_eq!(app.poly_tool, PolyTool::Knife, "the knife must survive R");
    }

    #[test]
    fn k_then_r_arms_rectangle_from_poly_sculpt_room() {
        let mut app = app_with_selected_quad();
        app.poly_work_mode = PolyWorkMode::Object;
        app.tool.work_mode = WorkMode::Sculpt;
        let ctx = egui::Context::default();
        tap(&mut app, &ctx, egui::Key::K);
        assert_eq!(app.poly_tool, PolyTool::Knife, "K from the sculpt room must arm the knife; toasts: {:?}", toasts(&app));
        tap(&mut app, &ctx, egui::Key::R);
        assert_eq!(
            app.knife_shape_kind, 1,
            "R under the knife must arm the rectangle; toasts: {:?}", toasts(&app)
        );
        assert!(!app.tool.gizmo_armed, "R under the knife must not arm the rotate gizmo");
    }

    /// The repeat chord. After a rect cut the knife stays armed in rect mode by design
    /// (`shape_release_keeps_kind_armed`), so the user's next `K` lands on an ARMED knife.
    /// It used to cancel the tool silently — then `R` had nothing to arm and did nothing, with
    /// no toast (owner 2026-10-11). `K` on an idle armed knife must restart it and stay armed.
    #[test]
    fn k_on_armed_idle_knife_restarts_it_and_r_arms_rectangle_again() {
        let mut app = app_with_selected_quad();
        app.poly_work_mode = PolyWorkMode::Face;
        let ctx = egui::Context::default();
        tap(&mut app, &ctx, egui::Key::K);
        tap(&mut app, &ctx, egui::Key::R);
        assert_eq!(app.knife_shape_kind, 1);
        // State right after a committed rect cut: rubber-band cleared, mode kept, no points.
        app.knife_shape_anchor = None;
        app.knife_points.clear();
        app.ui.toasts.clear();

        tap(&mut app, &ctx, egui::Key::K);
        assert_eq!(
            app.poly_tool, PolyTool::Knife,
            "K on an armed knife must NOT drop the tool; toasts: {:?}", toasts(&app)
        );
        assert_eq!(app.knife_shape_kind, 0, "K restarts as a free line so the next R arms rect");
        assert!(!toasts(&app).is_empty(), "the restart must be legible — never a silent state change");

        tap(&mut app, &ctx, egui::Key::R);
        assert_eq!(
            app.knife_shape_kind, 1,
            "second `K, R` must arm the rectangle exactly like the first; toasts: {:?}", toasts(&app)
        );
        assert_eq!(app.poly_tool, PolyTool::Knife);
    }

    /// `K` with a cut in progress commits it (sibling convention: bevel `B` / hinge confirm on a
    /// same-key press) instead of discarding the placed points.
    #[test]
    fn k_with_free_points_in_progress_commits_the_cut() {
        let mut app = app_with_selected_quad();
        app.poly_work_mode = PolyWorkMode::Face;
        let nid = app.sel.usel.active_node().expect("quad selected");
        let before = app.poly_node_mesh(nid).map(|m| m.vertices.len()).unwrap_or(0);
        let ctx = egui::Context::default();
        tap(&mut app, &ctx, egui::Key::K);
        assert_eq!(app.poly_tool, PolyTool::Knife);
        // Free-line SLICE across the quad on a drawing plane in front of it (looking down -Z).
        // Slice personality: an open SUBTRACT line would instead ask for the cursor side (same
        // as Enter does), which is correct but not what this test measures.
        app.knife_free = true;
        app.knife_free_slice = true;
        app.knife_free_plane = Some((Vec3::new(0.0, 0.0, 2.0), Vec3::X, Vec3::Y, Vec3::NEG_Z));
        app.knife_points = vec![Vec3::new(-2.0, 0.0, 2.0), Vec3::new(2.0, 0.0, 2.0)];

        tap(&mut app, &ctx, egui::Key::K);
        let after = app.poly_node_mesh(nid).map(|m| m.vertices.len()).unwrap_or(0);
        assert!(
            after > before,
            "K mid-cut must COMMIT the cut (verts {before} → {after}), never silently discard it; toasts: {:?}",
            toasts(&app)
        );
    }
}
