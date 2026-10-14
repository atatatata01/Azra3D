// paint.rs — pure relocation from poly_selection.rs
// Box / circle / lasso / paint region select + release-time click select.
// OWNER-FINAL visible-only / front_only rules travel intact with poly_screen_test.

use std::collections::{HashSet, VecDeque};
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, FaceId, NO_FACE, NO_EDGE};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{
    AzraApp, ModelingContext, PolyWorkMode, PolyTool, DirtyFlags,
    ActiveTool, SelectionShape,
    world_to_screen, world_segment_to_screen, dist_point_to_segment,
};

use super::point_in_polygon;

impl AzraApp {
    // ============================================================
    // Box Select & Circle Select
    // ============================================================

    /// Select all poly sub-elements whose screen-space projection falls inside the given rect.
    pub(crate) fn poly_box_select_apply(&mut self, screen_rect: egui::Rect, viewport: egui::Rect, additive: bool) {
        if !additive { self.poly_clear_selection(); }
        // B-key box tool keeps its historical THROUGH (x-ray) behavior — front_only=false.
        let hits = self.poly_screen_test(|sp| screen_rect.contains(sp), &[screen_rect.center()], false, viewport);
        for id in hits { self.poly_add_to_selection(id); }
        // Box-select has no single "last" vertex → clear the active target so M merges at CENTROID
        // (Blender: box-selected verts merge at center; shift-selected merge at the last/active).
        self.poly_active_vertex = None;
        self.sync_orbit_center_to_selection();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Select all poly sub-elements within a screen-space circle (additive add — no clear).
    /// `front_only` = occlusion filter: the C-key brush passes `false` (through, historical), the
    /// LMB paint drag passes `self.sel.visible_only` (paint what you see).
    pub(crate) fn poly_circle_select_apply(&mut self, center: egui::Pos2, radius: f32, front_only: bool, viewport: egui::Rect) {
        let r2 = radius * radius;
        let hits = self.poly_screen_test(|sp| (sp - center).length_sq() <= r2, &[center], front_only, viewport);
        for id in hits { self.poly_add_to_selection(id); }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Paint-select brush SWEPT from `a` to `b` (last→current mouse) — a capsule, not a point circle. A
    /// fast drag jumps many pixels between frames; a per-frame point brush leaves gaps in the stroke
    /// ("paint select not reliable"). Testing distance to the SEGMENT catches every element the brush
    /// passed over, regardless of drag speed. Combined with the per-edge sampling in poly_screen_test,
    /// an edge is selected anywhere the stroke crosses it.
    /// `erase` = Shift-held: the brush REMOVES from the selection instead of adding (Blender/ZBrush
    /// paint-select convention). Owner 2026-07-30 — the flag was captured at stroke start and then
    /// never consulted, so Shift+paint silently kept adding.
    pub(crate) fn poly_paint_stroke_apply(&mut self, a: egui::Pos2, b: egui::Pos2, radius: f32, front_only: bool, viewport: egui::Rect, erase: bool) {
        // Probe points along the stroke so a brush sweeping over a face's interior selects it (not just
        // when it grazes a vertex/edge). Sample ~every radius px along a->b, plus both endpoints.
        let mut probes = vec![a, b];
        let len = (b - a).length();
        let n = (len / radius.max(1.0)).ceil() as u32;
        for i in 1..n { let t = i as f32 / n as f32; probes.push(egui::pos2(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)); }
        let hits = self.poly_screen_test(|sp| dist_point_to_segment(sp, a, b) <= radius, &probes, front_only, viewport);
        for id in hits {
            if erase {
                self.poly_remove_from_selection(id);
            } else {
                self.poly_add_to_selection(id);
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Test all elements in the current poly sub-mode against a screen-space predicate.
    ///
    /// `front_only` layers a self-occlusion filter ON TOP of the predicate: when true an element is
    /// kept only if it is not behind the mesh's own front surface at its screen position (front /
    /// visible-only, reusing `poly_front_surface_depth` — the SAME depth logic as the hover-pick
    /// occlusion gate). When false the predicate alone decides (through / x-ray). This is the ONE
    /// primitive all three region shapes (Box/Circle/Lasso) + paint route through — the shape is only
    /// the `pred`, front/through is only this flag.
    /// `probes` are screen points KNOWN to be inside the selection region (the brush stroke samples, a
    /// region's center, the lasso path). A FACE is also selected when any probe lands INSIDE its screen
    /// polygon — so a brush parked over a face's interior (not near any vertex/edge) still paints it
    /// ("mouse on a poly wasn't painting it"). Pass `&[]` when there is no meaningful interior probe.
    fn poly_screen_test(&self, pred: impl Fn(egui::Pos2) -> bool, probes: &[egui::Pos2], front_only: bool, viewport: egui::Rect) -> Vec<u32> {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return vec![] };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return vec![] };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return vec![],
        };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_orbit = self.viewport.camera.orbit;
        let aspect = viewport.width() / viewport.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let hidden = &self.poly_sel.hidden_vertices;
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let xform_pos = |local: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };
        let xform_dir = |d: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(d.x, d.y, d.z, 0.0); // w=0 → direction, no translation
            Vec3::new(w.x, w.y, w.z).normalize_or_zero()
        };
        let fwd = (cam_orbit * Vec3::NEG_Z).normalize();
        // Depth bias identical to the hover-pick gate so silhouette / on-surface elements aren't culled.
        let bias = (self.viewport.camera.distance * 0.02).max(1e-3);
        // `p` = element's world point, `sp` = its screen point. Returns true when it is occluded by the
        // mesh's own nearer surface. Only computed when `front_only` (the ray scan is O(faces) per call).
        let occluded = |p: Vec3, sp: egui::Pos2| -> bool {
            if !front_only { return false; }
            match self.poly_front_surface_depth(nid, sp, viewport, cam_pos, cam_orbit, aspect, ortho_hw) {
                Some(front) => Self::view_depth(p, cam_pos, fwd) > front + bias,
                None => false, // ray missed every face → nothing occludes
            }
        };
        // Back-face cull for front_only selection. The depth ray MISSES near a silhouette (no front face
        // covers that screen pixel), so `occluded` returns false there and back-facing geometry just
        // behind the contour leaked into the selection ("painting an exterior edge selects hidden
        // geometry"). Culling by facing fixes it: a face is visible only if it faces the camera; an edge
        // is visible if AT LEAST ONE of its faces does (so true silhouette/contour edges stay selectable).
        let world_face_normal = |fid: FaceId| -> Vec3 {
            // ZERO for "no face" / dead slot — `faces_camera` reads ZERO as "don't cull", which
            // is the right answer for a wire edge that has no surface to face with. Indexing
            // `face_vertices(NO_FACE)` would panic; this is the guard, not a fallback.
            if fid == NO_FACE || !mesh.is_face_live(fid) { return Vec3::ZERO; }
            let vs = mesh.face_vertices(fid);
            if vs.len() < 3 { return Vec3::ZERO; }
            let a = xform_pos(mesh.vert(vs[0]).position);
            let b = xform_pos(mesh.vert(vs[1]).position);
            let c = xform_pos(mesh.vert(vs[2]).position);
            (b - a).cross(c - a).normalize_or_zero()
        };
        let faces_camera = |p: Vec3, n: Vec3| -> bool {
            if n == Vec3::ZERO { return true; } // degenerate → don't cull
            let to_cam = if ortho_hw > 0.0 { -fwd } else { (cam_pos - p).normalize_or_zero() };
            n.dot(to_cam) > -0.05 // small tolerance so grazing/edge-on faces aren't culled
        };
        // GPU select buffer (Blender model, PAINT_SELECT_AUDIT.md P1): when valid, EVERY visibility
        // question is an O(1) image lookup — the depth buffer already resolved occlusion, so the ray
        // scan + back-face heuristics below are bypassed entirely. They remain ONLY as the fallback
        // for the frame(s) while the buffer is stale/pending (and as the X-ray path, front_only=false).
        let img = if front_only { self.poly_pick_image_valid() } else { None };
        let mut result = Vec::new();

        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                for i in 0..mesh.vertices.len() as u32 {
                    if mesh.vert(VertexId(i)).half_edge == NO_EDGE || hidden.contains(&i) { continue; }
                    let pw = xform_pos(mesh.vert(VertexId(i)).position);
                    // Back-face cull heuristic — fallback path only (the image needs no heuristic).
                    if front_only && img.is_none() && !faces_camera(pw, xform_dir(mesh.vert(VertexId(i)).normal)) { continue; }
                    if let Some(sp) = world_to_screen(pw, cam_pos, cam_orbit, viewport, aspect, ortho_hw, self.render.fov_deg) {
                        if pred(sp) {
                            let vis = if let Some(img) = img {
                                // Per-element buffer: the vert's OWN depth-resolved id — exact,
                                // no incident-face inference (that was the silhouette bug class).
                                img.any_near_r(sp, 2, |id| {
                                    id == crate::poly_select_gpu::pick_id_vert(i)
                                })
                            } else {
                                !occluded(pw, sp)
                            };
                            if vis { result.push(i); }
                        }
                    }
                }
            }
            PolyWorkMode::Edge => {
                // ONE canonical edge walk (`canonical_edges`) — the same one the pick cache, the
                // GPU select buffer and the wire overlay use. The old inline `he.face == NO_FACE
                // → skip` + min-of-pair pair made every rim edge of an OPEN mesh unreachable
                // (both halves failed one of the two tests), so box / lasso / circle / paint
                // could not select geometry the user could see, hover and click.
                for he_id in mesh.canonical_edges() {
                    let he_idx = he_id.0;
                    let he = mesh.he(he_id);
                    let src = mesh.he_source(he_id);
                    let dst = he.vertex;
                    if hidden.contains(&src.0) || hidden.contains(&dst.0) { continue; }
                    let p0 = xform_pos(mesh.vert(src).position);
                    let p1 = xform_pos(mesh.vert(dst).position);
                    // Back-face cull heuristic — fallback path only (the image needs no heuristic).
                    if front_only && img.is_none() {
                        let em = (p0 + p1) * 0.5;
                        let (fa, fb) = mesh.edge_faces(he_id);
                        // A pure WIRE edge has no incident face, so there is no facing to test —
                        // it must never be culled (it is visible from every direction).
                        let mut any_front = fa == NO_FACE && fb == NO_FACE;
                        if !any_front && fa != NO_FACE {
                            any_front = faces_camera(em, world_face_normal(fa));
                        }
                        if !any_front && fb != NO_FACE {
                            any_front = faces_camera(em, world_face_normal(fb));
                        }
                        if !any_front { continue; }
                    }
                    let sp0 = world_to_screen(p0, cam_pos, cam_orbit, viewport, aspect, ortho_hw, self.render.fov_deg);
                    let sp1 = world_to_screen(p1, cam_pos, cam_orbit, viewport, aspect, ortho_hw, self.render.fov_deg);
                    if let (Some(a), Some(b)) = (sp0, sp1) {
                        // Test the WHOLE edge, not just its midpoint: sample along a->b so a paint brush /
                        // box / circle / lasso that touches ANY part of the edge selects it. The old
                        // midpoint-only test was the "drag select didn't pick all visible edges" bug — an
                        // edge the brush crossed off-center was missed. Step ~every 6px of screen length;
                        // occlusion is checked at the SAMPLE that hit (its interpolated world point), so a
                        // partially-visible edge is caught via its visible portion.
                        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                        let steps = ((len / 6.0).ceil() as u32).clamp(1, 48);
                        for s in 0..=steps {
                            let t = s as f32 / steps as f32;
                            let sp = egui::pos2(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                            if pred(sp) {
                                let vis = if let Some(img) = img {
                                    // The edge's OWN id (he_idx is canonical here — the loop
                                    // skips the higher twin). Endpoint vert ids accepted too:
                                    // verts draw with higher priority and can own the pixels
                                    // of a short / endpoint-adjacent sample.
                                    img.any_near_r(sp, 2, |id| {
                                        id == crate::poly_select_gpu::pick_id_edge(he_idx)
                                            || id == crate::poly_select_gpu::pick_id_vert(src.0)
                                            || id == crate::poly_select_gpu::pick_id_vert(dst.0)
                                    })
                                } else {
                                    !occluded(p0 + (p1 - p0) * t, sp)
                                };
                                if vis { result.push(he_idx); break; }
                            }
                        }
                    }
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                for fi in 0..mesh.faces.len() as u32 {
                    if mesh.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
                    let verts = mesh.face_vertices(FaceId(fi));
                    if verts.iter().any(|v| hidden.contains(&v.0)) { continue; }
                    // A face is selected when the region overlaps ANY part of it — "if the ring touches
                    // the face at any point it must select", at any brush size. Two independent ways to
                    // touch: (A) the region touches the face BOUNDARY — sample every boundary edge (~every
                    // 6px, like Edge mode) and test each sample against `pred`; a small brush grazing an
                    // edge from outside is caught here. (B) the region is INSIDE the face — any interior
                    // probe (brush stroke sample / region center / lasso point) is point_in_polygon of the
                    // face's screen polygon; a small brush parked in a big face's interior is caught here.
                    let mut poly: Vec<(Vec3, egui::Pos2)> = Vec::with_capacity(verts.len());
                    let mut cw = Vec3::ZERO;
                    for &v in &verts {
                        let pw = xform_pos(mesh.vert(v).position);
                        if let Some(sp) = world_to_screen(pw, cam_pos, cam_orbit, viewport, aspect, ortho_hw, self.render.fov_deg) {
                            cw += pw; poly.push((pw, sp));
                        }
                    }
                    if poly.is_empty() { continue; }
                    let fcw = cw / poly.len() as f32;
                    // Back-face cull heuristic — fallback path only (the image needs no heuristic).
                    if front_only && img.is_none() && poly.len() >= 3 {
                        let n = (poly[1].0 - poly[0].0).cross(poly[2].0 - poly[0].0).normalize_or_zero();
                        if !faces_camera(fcw, n) { continue; }
                    }
                    let poly_sp: Vec<egui::Pos2> = poly.iter().map(|&(_, sp)| sp).collect();
                    // (A) boundary-edge sampling. Keep the SCREEN point of the hit alongside the world
                    // point: the image visibility test happens at the exact screen sample.
                    let mut hit: Option<(Vec3, egui::Pos2)> = None;
                    'edges: for i in 0..poly.len() {
                        let (w0, a) = poly[i];
                        let (w1, b) = poly[(i + 1) % poly.len()];
                        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                        let steps = ((len / 6.0).ceil() as u32).clamp(1, 48);
                        for s in 0..=steps {
                            let t = s as f32 / steps as f32;
                            let sp = egui::pos2(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                            if pred(sp) { hit = Some((w0 + (w1 - w0) * t, sp)); break 'edges; }
                        }
                    }
                    // (B) region inside the face — the probe itself is the screen point.
                    if hit.is_none() {
                        if let Some(&pr) = probes.iter().find(|&&pr| point_in_polygon(pr, &poly_sp)) {
                            hit = Some((fcw, pr));
                        }
                    }
                    if let Some((hw, hs)) = hit {
                        let vis = if let Some(img) = img {
                            // Accept the face's OWN id or anything on its RIM (boundary edges +
                            // corner verts): case-A hit samples lie ON boundary edges, whose
                            // pixels now carry the edge/vert ids drawn over the face — all of
                            // them prove this face is the front surface at that sample.
                            let mut accept: HashSet<u32> = HashSet::new();
                            accept.insert(crate::poly_select_gpu::pick_id_face(fi));
                            for he in mesh.face_half_edges(FaceId(fi)) {
                                accept.insert(crate::poly_select_gpu::pick_id_edge(
                                    crate::poly_select_gpu::canonical_he(mesh, he.0),
                                ));
                            }
                            for &v in &verts {
                                accept.insert(crate::poly_select_gpu::pick_id_vert(v.0));
                            }
                            img.any_near(hs, |id| accept.contains(&id))
                        } else {
                            !occluded(hw, hs)
                        };
                        if vis { result.push(fi); }
                    }
                }
            }
            PolyWorkMode::Object => {}
        }
        result
    }

    /// Apply the active `PolyRegionSelect` (RMB drag) to the current selection. Reads the
    /// tool state's shape + front/through flag + subtract flag, builds the matching screen
    /// predicate, and routes it through the ONE `poly_screen_test` primitive.
    /// Plain RMB ADDS (never clears — owner 2026-08-07: "RMB should always add, not act like
    /// paint select"). **Shift+RMB REMOVES hits** (deselect).
    pub(crate) fn poly_apply_region_select(&mut self, viewport: egui::Rect) {
        let (shape, front_only, subtract, start, current, path) = match &self.tool.current {
            ActiveTool::PolyRegionSelect(rs) =>
                (rs.shape, rs.front_only, rs.subtract, rs.start, rs.current, rs.path.clone()),
            _ => return,
        };
        let hits = match shape {
            SelectionShape::Box => {
                let r = egui::Rect::from_two_pos(start, current);
                self.poly_screen_test(|sp| r.contains(sp), &[r.center()], front_only, viewport)
            }
            SelectionShape::Circle => {
                let r2 = (current - start).length_sq();
                self.poly_screen_test(|sp| (sp - start).length_sq() <= r2, &[start], front_only, viewport)
            }
            SelectionShape::Lasso => {
                self.poly_screen_test(|sp| point_in_polygon(sp, &path), &path, front_only, viewport)
            }
        };
        if subtract {
            for id in hits {
                self.poly_remove_from_selection(id);
            }
        } else {
            for id in hits {
                self.poly_add_to_selection(id);
            }
        }
        // Region select has no single "last" element → clear the active target (M merges at centroid).
        self.poly_active_vertex = None;
        self.sync_orbit_center_to_selection();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Shared release-time click-select for the poly LMB tools (paint tool click, B-key box tiny-drag).
    /// Handles Alt=edge-loop/ring, manual double-click (loop / linked), and the plain single click —
    /// the ONE path so paint and box don't diverge. `now` = `ctx.input(|i| i.time)`.
    /// Selection-undo wrapper for the poly drag-release select path (mirrors `handle_click_select`).
    /// Captures the pre-selection, runs the real release logic, records a coalesced selection undo if
    /// it changed. Keeps sub-element selection (paint/loop/double-click select) on the undo stack too.
    pub(crate) fn poly_release_click_select(&mut self, release_pos: egui::Pos2, additive: bool, ctrl: bool, alt: bool, now: f64) {
        let pre = self.capture_selection_snapshot();
        self.poly_release_click_select_inner(release_pos, additive, ctrl, alt, now);
        if !self.capture_selection_snapshot().same_as(&pre) {
            self.record_selection_undo(pre);
            // Orbit around the new poly element selection by default.
            self.sync_orbit_center_to_selection();
        }
        // This is the live LMB path (paint-click / tiny box). Without RENDER the GPU
        // edge highlight (`append_poly_selected_edges`) keeps last frame's overlay —
        // owner: "I need to rotate the camera after an edge selection to see the
        // highlight." `handle_click_select` already dirties; this twin forgot to.
        self.mark_dirty(DirtyFlags::RENDER);
    }

    fn poly_release_click_select_inner(&mut self, release_pos: egui::Pos2, additive: bool, ctrl: bool, alt: bool, now: f64) {
        // Empty-click tool-drop escalation (owner: "clicking never drops G/R/S — click should drop the
        // TOOL, click again drop selection"). A plain (non-additive, non-alt) click that hovers nothing
        // first drops any armed tool (G/R/S gizmo, Bevel/Extrude mode, poly tool) and KEEPS the
        // selection; only a subsequent empty click (nothing left to drop) falls through to clear it.
        if !additive && !alt && self.poly_hover_is_empty() && self.drop_active_tool_only() {
            self.poly_last_click_t = -1.0;
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }
        // Generous window/radius: a real double-click jitters more than 6px between taps and can be
        // slower than 0.35s; the tight values missed valid double-clicks (part of the "1 in 3" report).
        let is_double = self.poly_last_click_t >= 0.0
            && (now - self.poly_last_click_t) < 0.45
            && (release_pos - self.poly_last_click_p).length() < 14.0;
        if self.poly_work_mode == PolyWorkMode::Edge && alt {
            // Edge + Alt (via unmoved Alt+LMB / region-select click path):
            //   Alt+click            → edge LOOP (bidirectional LSelect)
            //   Alt+Shift+click      → DIRECTED edge loop from 2 edges (driver + seed)
            //                          Blender/MeshMachine: two selected edges orient the walk
            //                          through poles / hard junctions
            //   Alt+double-click     → edge RING  (owner 2026-08-06)
            //   Ctrl+Alt+click       → edge RING  (Blender parity, single click)
            // SHIFT also keeps/adds. Must NOT clear the double-click timer on the first Alt
            // click — that made Alt+double-click impossible (always two loops).
            //
            // Directed (Alt+Shift): do NOT clear, and keep prior selection as the driver
            // *before* seeding hover — driver resolution reads selected_edges + history.
            let want_ring = ctrl || is_double;
            let want_directed = additive && !want_ring;
            if !additive {
                self.poly_sel.selected_edges.clear();
            }
            // Seed from hover so ring/loop has a start edge under the cursor.
            // For directed: leave prior edges selected; only add the hover seed.
            if let Some(e) = self.poly_sel.hovered_edge {
                self.poly_sel.selected_edges.insert(e);
                // Record hover in history so a following Alt+Shift can use it as driver.
                self.poly_push_edge_history(e);
            }
            if want_ring {
                self.poly_select_edge_ring();
                self.poly_last_click_t = -1.0; // consume double so a 3rd click is not another ring
            } else if want_directed {
                // Two-edge oriented loop (prev selected + clicked seed).
                self.poly_select_edge_loop_directed();
                self.poly_last_click_t = now;
                self.poly_last_click_p = release_pos;
            } else {
                self.poly_select_edge_loop();
                // Arm the double-click window — next Alt-click quickly → ring.
                self.poly_last_click_t = now;
                self.poly_last_click_p = release_pos;
            }
        } else if is_double {
            // Owner 2026-08-06:
            //   plain double-click  → select LINKED ISLAND (all connected geometry)
            //   Shift+double-click  → LOOP, using selection history for direction
            //                         (face loop expects ≥2 faces: select A then B, then Shift+dbl)
            match self.poly_work_mode {
                PolyWorkMode::Edge => {
                    if additive {
                        // Directed edge loop when history/prior selection gives a driver.
                        self.poly_select_edge_loop();
                    } else {
                        // Island: all edges on the connected component under the cursor.
                        self.poly_select_linked_hover(false);
                    }
                }
                PolyWorkMode::Face | PolyWorkMode::Group => {
                    if additive {
                        // Face LOOP — history (last two selected) orients direction.
                        self.poly_select_face_loop();
                    } else {
                        // Island: all faces of the shell under the cursor.
                        self.poly_select_linked_hover(false);
                    }
                }
                PolyWorkMode::Vertex => self.poly_select_linked_hover(additive),
                _ => {}
            }
            self.poly_last_click_t = -1.0; // consume so a 3rd click isn't read as another double
        } else {
            self.poly_click_select(additive);
            self.poly_last_click_t = now;
            self.poly_last_click_p = release_pos;
        }
    }

    /// Add an element ID to the appropriate selection set based on current poly work mode.
    fn poly_add_to_selection(&mut self, id: u32) {
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                if self.poly_sel.selected_vertices.insert(id) {
                    self.poly_push_vert_history(id);
                    self.poly_active_vertex = Some(id);
                }
            }
            PolyWorkMode::Edge => {
                if self.poly_sel.selected_edges.insert(id) {
                    self.poly_push_edge_history(id);
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                if self.poly_sel.selected_faces.insert(id) {
                    self.poly_push_face_history(id);
                }
            }
            PolyWorkMode::Object => {}
        }
    }

    /// Remove one element from the current mode's selection — the exact inverse of
    /// [`poly_add_to_selection`], so the two stay in lockstep as modes are added.
    ///
    /// Owner 2026-07-30: "the paint select do not support shift deselect." `PolyPaintSelectState`
    /// already CAPTURED `shift` at stroke start, but the stroke applier only ever added, so the flag
    /// was dead state. Edge mode removes the twin too, because a selection may hold either half
    /// (`poly_select_edge_loop` inserts raw ids) — otherwise erasing an edge could leave its twin
    /// selected and the element would look un-erasable.
    fn poly_remove_from_selection(&mut self, id: u32) {
        match self.poly_work_mode {
            PolyWorkMode::Vertex => { self.poly_sel.selected_vertices.remove(&id); }
            PolyWorkMode::Edge => {
                self.poly_sel.selected_edges.remove(&id);
                if let Some(nid) = self.sel.usel.active_node() {
                    if let Some(node) = self.scene.scene_graph.get(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                            if (id as usize) < mesh.half_edges.len() {
                                let t = mesh.he(core_math::half_edge::HalfEdgeId(id)).twin;
                                if t != core_math::half_edge::NO_TWIN {
                                    self.poly_sel.selected_edges.remove(&t.0);
                                }
                            }
                        }
                    }
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                self.poly_sel.selected_faces.remove(&id);
            }
            PolyWorkMode::Object => {}
        }
    }
}
